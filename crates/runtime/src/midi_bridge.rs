/*
midi_bridge.rs - Route scheduled onsets to a MIDI port
Copyright (C) 2026 Rustel contributors

This program is free software: you can redistribute it and/or modify it under
the terms of the GNU Affero General Public License as published by the Free
Software Foundation, either version 3 of the License, or (at your option) any
later version.
*/

//! Turning scheduled onsets into MIDI sends.
//!
//! A hap goes to MIDI when it carries a `midiport`, which is what `.midi(port)`
//! sets. That keeps the decision in the score's own vocabulary rather than in a
//! side channel, so `midiport` can also be patterned per-hap to move a phrase
//! between devices.
//!
//! Audio is unaffected: a hap can drive a synth AND a MIDI device, the same way
//! `.midi()` in the browser layers on top of whatever the pattern already did.

use std::{
    borrow::Cow,
    collections::{BTreeSet, HashMap, VecDeque},
    sync::atomic::{AtomicU64, Ordering},
    sync::{Arc, Condvar, Mutex},
    time::{Duration, Instant},
};

use rustel_midi::{MidiControls, MidiSender};

use crate::hap_json::{OnsetEventJson, ValueJson};

/// The port a hap names, plus the controls it carries.
pub struct MidiOnset {
    pub onset_id: u64,
    pub generation: u64,
    pub port: String,
    pub controls: MidiControls,
    /// Session-clock seconds, the same scale as `OnsetEventJson::target_time`.
    pub target_time: f64,
    pub duration_secs: f64,
}

fn object(value: &ValueJson) -> Option<&serde_json::Map<String, serde_json::Value>> {
    match value {
        ValueJson::Raw(serde_json::Value::Object(map)) => Some(map),
        _ => None,
    }
}

fn number(map: &serde_json::Map<String, serde_json::Value>, key: &str) -> Option<f64> {
    map.get(key)?.as_f64()
}

/// A pitch control. The score can write it as a number or as a note name.
///
/// `note("c3")` arrives here as the string `"c3"`. `rustel-voice` resolves
/// names where the audio path needs a frequency, so the scheduler leaves the
/// value as the score wrote it. This function resolves the name for MIDI. A
/// hap with no usable note plans no messages.
///
/// A bare name uses octave 3, the default of `note_to_hz`, so a name gives
/// the same pitch on both paths.
fn pitch(map: &serde_json::Map<String, serde_json::Value>, key: &str) -> Option<f64> {
    match map.get(key)? {
        serde_json::Value::Number(number) => number.as_f64(),
        serde_json::Value::String(text) => {
            let text = text.trim();
            match text.parse::<f64>() {
                Ok(number) => Some(number),
                // A name that does not parse is not a note; `plan` refuses the
                // NaN `note_to_midi` returns for a half-written one.
                Err(_) => rustel_core::util::note_to_midi(text, 3).ok(),
            }
        }
        _ => None,
    }
}

/// Retain only transport commands the wire planner understands.
///
/// Returning fresh fixed strings is deliberate: an unknown `midicmd` may be
/// arbitrarily large score input and must not be cloned into the live queue
/// merely to be discarded later by `rustel_midi::plan`.
fn midi_command(map: &serde_json::Map<String, serde_json::Value>) -> Option<String> {
    match map.get("midicmd")?.as_str()? {
        "clock" => Some("clock".into()),
        "midiClock" => Some("midiClock".into()),
        "start" => Some("start".into()),
        "stop" => Some("stop".into()),
        "continue" => Some("continue".into()),
        _ => None,
    }
}

/// Borrow the route named by one onset without cloning an attacker-controlled
/// score string. Numeric selectors are formatted into a small owned value.
pub fn midi_route(onset: &OnsetEventJson) -> Option<Cow<'_, str>> {
    let map = object(&onset.value)?;
    match map.get("midiport")? {
        serde_json::Value::String(port) => Some(Cow::Borrowed(port)),
        serde_json::Value::Number(port) => Some(Cow::Owned(port.to_string())),
        _ => None,
    }
}

/// Extract the MIDI intent of one onset after its route has been bounded and
/// copied by the caller.
pub fn midi_onset_with_port(onset: &OnsetEventJson, port: String) -> Option<MidiOnset> {
    let map = object(&onset.value)?;
    let options = map.get("midiopts").and_then(serde_json::Value::as_object);
    let option_number = |key: &str| {
        options
            .and_then(|options| options.get(key))
            .and_then(serde_json::Value::as_f64)
            .filter(|value| value.is_finite())
    };
    Some(MidiOnset {
        onset_id: onset.onset_id,
        generation: onset.generation,
        port,
        target_time: onset.target_time,
        duration_secs: onset.duration_secs,
        controls: MidiControls {
            // Read `note` only. `n` is the sample-index control, and the
            // audio path does not read it as a pitch either. For example,
            // `s("bd:3 sd:7").midi(port)` must not send note-ons at the
            // sample index.
            note: pitch(map, "note"),
            gain: number(map, "gain").or_else(|| option_number("gain")),
            velocity: number(map, "velocity").or_else(|| option_number("velocity")),
            midichan: number(map, "midichan").or_else(|| option_number("midichannel")),
            ccn: number(map, "ccn").or_else(|| number(map, "ctlNum")),
            ccv: number(map, "ccv"),
            prog_num: number(map, "progNum"),
            midibend: number(map, "midibend"),
            miditouch: number(map, "miditouch"),
            midicmd: midi_command(map),
            is_controller: options
                .and_then(|options| options.get("isController"))
                .is_some_and(|value| match value {
                    serde_json::Value::Bool(flag) => *flag,
                    serde_json::Value::Number(number) => {
                        number.as_f64().is_some_and(|value| value != 0.0)
                    }
                    _ => false,
                }),
            note_offset_ms: option_number("noteOffsetMs"),
            // A midimap turns named controls into CC messages: for every entry
            // of the selected map whose control the hap carries, one CC with
            // the value normalised into the entry's range. The hap's `midimap`
            // control names the map; absent, the map registered as "default"
            // applies, and a hap naming a map nobody registered sends no
            // mapped CCs. This reads the bound settings. The caller
            // (session.rs) binds the runtime's settings around the collection
            // pass, because an unbound read falls back to the process-global
            // state and sees nothing a Session registered.
            mapped_ccs: {
                let name = match map.get("midimap") {
                    None => match options.and_then(|options| options.get("midimap")) {
                        None => Some("default"),
                        Some(serde_json::Value::String(name)) => Some(name.as_str()),
                        Some(_) => None,
                    },
                    Some(serde_json::Value::String(name)) => Some(name.as_str()),
                    // Map lookup is strict: null and numeric values do not
                    // select the string-named default/map.
                    Some(_) => None,
                };
                match name.and_then(rustel_core::midimap::midi_map) {
                    // Never scan the score-controlled hap object: an object
                    // pattern may carry arbitrarily many unrelated keys, and
                    // this conversion runs after the bounded query turn. Walk
                    // the pinned alias table plus the registered map instead;
                    // both have hard native ceilings (currently 499 aliases
                    // and at most 256 map entries), then perform O(1) hap
                    // lookups. Alias canonicalisation survives without giving
                    // one onset unbounded post-query work.
                    Some(entries) => {
                        let controls = rustel_core::controls::default_control_registry();
                        let mut mapped = Vec::new();
                        for (alias, canonical) in controls.alias_entries() {
                            let Some(entry) = entries.get(canonical) else {
                                continue;
                            };
                            let Some(value) = map.get(alias).and_then(serde_json::Value::as_f64)
                            else {
                                continue;
                            };
                            if value.is_finite() {
                                mapped.push((entry.ccn, entry.normalise(value)));
                            }
                        }
                        // User-defined controls have no pinned aliases. The
                        // retained map is a HashMap, so sort its bounded (at
                        // most 256) custom subset before emitting wire work;
                        // otherwise two identical runs can reorder their CCs.
                        let mut custom_entries = entries
                            .iter()
                            .filter(|(control, _)| controls.canonical_name(control).is_none())
                            .collect::<Vec<_>>();
                        custom_entries.sort_unstable_by_key(|(name, _)| *name);
                        for (control, entry) in custom_entries {
                            let Some(value) = map
                                .get(control)
                                .and_then(serde_json::Value::as_f64)
                                .filter(|value| value.is_finite())
                            else {
                                continue;
                            };
                            mapped.push((entry.ccn, entry.normalise(value)));
                        }
                        mapped
                    }
                    None => Vec::new(),
                }
            },
        },
    })
}

/// Extract the MIDI intent of one onset, or `None` if it names no port.
pub fn midi_onset(onset: &OnsetEventJson) -> Option<MidiOnset> {
    let port = midi_route(onset)?.into_owned();
    midi_onset_with_port(onset, port)
}

/// A gap this large between the two clocks is not drift, it is a different
/// timeline - a recycled device, a restarted transport. Only that is worth
/// breaking the mapping for.
const CLOCK_DISCONTINUITY: Duration = Duration::from_secs(1);

/// How fast the anchor may be nudged back into agreement, as a fraction of the
/// wall time since the last call.
///
/// Ordinary crystal error between an audio device and `Instant` is ~100ppm;
/// correcting at 1000ppm outruns it tenfold while moving the anchor only
/// microseconds per pass. That bound is what keeps the mapping monotone: two
/// messages a millisecond apart in the score cannot swap, because the anchor
/// cannot move a millisecond in the time between the passes that queued them.
///
/// Drift accumulated over a note's whole gate is handled by [`NoteOrdering`].
const MAX_SLEW_RATE: f64 = 0.001;

/// Ceiling on the wall time one slew may be computed from.
///
/// The rate alone is not enough. Passes are normally 2ms apart, but the loop
/// tolerates multi-second stalls (`DEVICE_PROGRESS_DEADLINE` is 3s). After a
/// one-second gap between two passes, the rate allows 1ms of slew. That is
/// equal to the note-off margin, so a hap pair across that pass could
/// invert. Capping the denominator limits one correction to 250 microseconds
/// for any gap. That stays below the margin, and the slew still converges
/// ten times faster than the drift it corrects.
const MAX_SLEW_WINDOW: Duration = Duration::from_millis(250);

/// Maximum offset in seconds for MIDI `Duration` conversions.
///
/// Shared with clock output, where very small positive tempos can produce
/// offsets too large for `Duration::from_secs_f64`. Such ticks remain beyond
/// the scheduling horizon after clamping.
pub(crate) const MAX_OFFSET_SECS: f64 = 3600.0;

fn offset(base: Instant, secs: f64) -> Instant {
    if !secs.is_finite() {
        return base;
    }
    if secs >= 0.0 {
        let forward = Duration::from_secs_f64(secs.min(MAX_OFFSET_SECS));
        base.checked_add(forward).unwrap_or(base)
    } else {
        let back = Duration::from_secs_f64((-secs).min(MAX_OFFSET_SECS));
        base.checked_sub(back).unwrap_or(base)
    }
}

/// The wall instant and the session time it maps.
struct Anchor {
    wall: Instant,
    now: f64,
    /// When `instant_for` last ran, which is the slew budget's denominator.
    last_wall: Instant,
}

/// Session-clock seconds to the wall-clock instant a message is due.
///
/// The clock keeps one mapping for the whole set, not one per scheduling
/// pass. A mapping derived in each pass, `Instant::now() + (target - now)`,
/// gives each message the jitter of the pass that schedules it, and
/// consecutive haps are usually scheduled in different passes. A note-off is
/// only [`rustel_midi::NOTE_OFFSET_SECS`] (1ms) before the next note-on. A
/// pass that starts 1ms early then puts the note-on first: the synth
/// receives on-on-off-off and cuts the note it just started. Repeated notes
/// show this fault.
///
/// The two clocks still drift apart: `device.clock_seconds()` counts frames
/// at a nominal rate, and nothing aligns it with `Instant`. The clock
/// corrects this with a slew of a few microseconds per pass, not a step. A
/// step large enough to matter (100ms) can move a queued note-off behind the
/// next note-on. Only a real discontinuity causes a step. A note-off is
/// stamped once, at its onset, so slew that accumulates over a whole gate
/// can still invert a pair; [`NoteOrdering`] corrects that at stamp time.
///
/// One anchor also keeps every port on the same timeline, so a phrase split
/// across two devices stays in phase.
#[derive(Default)]
pub struct MidiClock {
    anchor: Option<Anchor>,
}

impl MidiClock {
    pub fn new() -> Self {
        Self::default()
    }

    /// When `target` is due, given that the session clock now reads `now`.
    pub fn instant_for(&mut self, now: f64, target: f64) -> Instant {
        let wall = Instant::now();
        match self.anchor.as_mut() {
            None => {
                self.anchor = Some(Anchor {
                    wall,
                    now,
                    last_wall: wall,
                });
            }
            Some(anchor) => {
                // Rebase onto `now` FIRST. The mapping is unchanged; this
                // only keeps the `Instant` offset down to one lookahead, so
                // an hours-long set never walks out to `MAX_OFFSET_SECS` and
                // saturates there.
                let mapped_now = offset(anchor.wall, now - anchor.now);
                let ahead = mapped_now > wall;
                let apart = if ahead {
                    mapped_now - wall
                } else {
                    wall - mapped_now
                };
                if !now.is_finite() || apart >= CLOCK_DISCONTINUITY {
                    *anchor = Anchor {
                        wall,
                        now,
                        last_wall: wall,
                    };
                } else {
                    let elapsed = wall
                        .saturating_duration_since(anchor.last_wall)
                        .min(MAX_SLEW_WINDOW)
                        .as_secs_f64();
                    let step = apart.min(Duration::from_secs_f64(elapsed * MAX_SLEW_RATE));
                    anchor.wall = if ahead {
                        mapped_now.checked_sub(step).unwrap_or(mapped_now)
                    } else {
                        mapped_now + step
                    };
                    anchor.now = now;
                    anchor.last_wall = wall;
                }
            }
        }
        let anchor = self.anchor.as_ref().expect("anchor set above");
        offset(anchor.wall, target - anchor.now)
    }
}

/// Track each key's stamped note-offs to preserve score order across clock slew.
///
/// An off is stamped at its onset, potentially a whole gate before the next
/// note-on. [`MidiClock`]'s per-pass slew bound cannot prevent drift accumulated
/// over that gate from moving the later on before the earlier off. Raise an
/// inverted on to the off's instant; the sender's arrival-order tie break then
/// sends the earlier-queued off first. Intentional retriggers before a previous
/// note's end keep their timing relative to that off.
///
/// Messages accompanying the note-on move with it. Its own off keeps the clock's
/// stamp unless that would precede the moved on. This shortens the affected gate
/// instead of carrying its delay into every later note.
///
/// With overlapping notes, the on must follow every off at or before it in score
/// time. Keep each off unless another is both earlier in the score and later on
/// the wire. Records expire past the sender's lateness bound and have a global cap.
#[derive(Default)]
pub struct NoteOrdering {
    offs: HashMap<(String, u8, u8), Vec<StampedOff>>,
    /// Entries across every key, held under [`MAX_TRACKED_NOTE_OFFS`].
    tracked: usize,
}

/// One recorded note-off: the session time it sounds, on `target_time`'s
/// scale, and the wall instant it was queued for.
#[derive(Clone, Copy)]
struct StampedOff {
    target: f64,
    due: Instant,
}

impl StampedOff {
    /// Whether `self` makes `other` redundant: every note-on the score placed
    /// at or after `other` is at or after `self` too, and following `self` on
    /// the wire already follows `other`.
    fn covers(&self, other: &Self) -> bool {
        self.target <= other.target && self.due >= other.due
    }

    /// Whether a note-on could still be sent before this off. Once the off is
    /// past due by the sender's lateness bound, a note-on stamped earlier than
    /// it would be dropped as late rather than sent.
    fn can_be_overtaken(&self, now: Instant) -> bool {
        self.due + rustel_midi::MAX_QUEUE_LATENESS > now
    }
}

/// Ceiling on the remembered note-offs, so a score that walks every key of
/// every channel of every port cannot grow the record without limit.
const MAX_TRACKED_NOTE_OFFS: usize = 8192;

/// Tolerance in seconds when comparing an on's score time with a recorded off.
///
/// With zero note-off lead, equivalent times can round differently: the off's
/// onset-plus-gate may be `0.1 + 0.2`, while the next onset is `0.3`.
const SCORE_ORDER_SLACK_SECS: f64 = 1e-6;

/// A note message's `(channel, note, starts)`, or `None` for anything that is
/// not a note. Read off the encoded wire bytes, the same way the sender's own
/// `starts_sound`/`ends_sound` read them; a zero-velocity note-on is a
/// note-off on the wire and counts as one here.
fn note_key(message: &rustel_midi::MidiMessage) -> Option<(u8, u8, bool)> {
    let bytes = message.as_slice();
    if bytes.len() < 3 {
        return None;
    }
    match bytes[0] >> 4 {
        0x9 => Some((bytes[0] & 0xf, bytes[1] & 0x7f, bytes[2] != 0)),
        0x8 => Some((bytes[0] & 0xf, bytes[1] & 0x7f, false)),
        _ => None,
    }
}

impl NoteOrdering {
    pub fn new() -> Self {
        Self::default()
    }

    /// Stamp one planned onset and remember its note-off instants.
    ///
    /// `base` maps `onset_target` to wall time, including any caller-supplied
    /// runway. Planned offsets are relative to it. If a note-on would overtake
    /// a score-prior off, use that off's instant as a floor for the whole batch.
    /// This keeps accompanying controllers and other messages with the on.
    /// The onset's own off moves only if it falls below that floor. Preserve the
    /// natural send order when raising timestamps creates ties. Negative and
    /// NaN offsets use the base instant; later offsets have a one-hour ceiling.
    pub fn stamp_batch(
        &mut self,
        port: &str,
        onset_target: f64,
        base: Instant,
        planned: &[(f64, rustel_midi::MidiMessage)],
    ) -> Vec<(Instant, rustel_midi::MidiMessage)> {
        #[allow(clippy::manual_clamp, reason = "max/min maps NaN offsets to zero")]
        let natural = planned
            .iter()
            .map(|(offset, _)| base + Duration::from_secs_f64(offset.max(0.0).min(MAX_OFFSET_SECS)))
            .collect::<Vec<_>>();
        // The latest queued off that one of this onset's note-ons must follow
        // but was stamped before.
        let mut floor = None;
        if self.tracked > 0 {
            for ((offset, message), natural_on) in planned.iter().zip(&natural) {
                let Some((channel, note, true)) = note_key(message) else {
                    continue;
                };
                let Some(offs) = self.offs.get(&(port.to_owned(), channel, note)) else {
                    continue;
                };
                // Compare score times with tolerance, but wall instants exactly:
                // even a nanosecond's inversion changes the sender's heap order.
                let on_target = onset_target + offset;
                for off in offs {
                    if off.target <= on_target + SCORE_ORDER_SLACK_SECS && off.due > *natural_on {
                        floor = floor.max(Some(off.due));
                    }
                }
            }
        }
        let mut batch = planned
            .iter()
            .zip(&natural)
            .map(|((_, message), due)| (floor.map_or(*due, |floor| (*due).max(floor)), *message))
            .collect::<Vec<_>>();
        let mut now = None;
        for ((offset, message), (due, _)) in planned.iter().zip(&batch) {
            let Some((channel, note, false)) = note_key(message) else {
                continue;
            };
            let now = *now.get_or_insert_with(Instant::now);
            self.record(
                (port.to_owned(), channel, note),
                StampedOff {
                    target: onset_target + offset,
                    due: *due,
                },
                now,
            );
        }
        if floor.is_some() {
            // Raising onto the floor is monotone, so it can only turn an order
            // into a tie, and the sender breaks a tie by arrival: arrive in the
            // order the natural stamps would have gone out. A stable sort on
            // them is exactly that order.
            let mut order = (0..batch.len()).collect::<Vec<_>>();
            order.sort_by_key(|&index| natural[index]);
            batch = order.into_iter().map(|index| batch[index]).collect();
        }
        batch
    }

    /// Remember one queued off, dropping the ones on its key it makes
    /// redundant and the ones nothing can overtake any more.
    fn record(&mut self, key: (String, u8, u8), off: StampedOff, now: Instant) {
        if self.tracked >= MAX_TRACKED_NOTE_OFFS {
            self.evict(now);
        }
        let offs = self.offs.entry(key).or_default();
        let before = offs.len();
        offs.retain(|kept| !off.covers(kept) && kept.can_be_overtaken(now));
        if !offs.iter().any(|kept| kept.covers(&off)) {
            offs.push(off);
        }
        self.tracked = self.tracked - before + offs.len();
    }

    /// Make room below [`MAX_TRACKED_NOTE_OFFS`].
    ///
    /// Drop expired records first. If all entries remain live, discard the
    /// earliest-due off, giving up its ordering protection to keep memory bounded.
    fn evict(&mut self, now: Instant) {
        self.offs.retain(|_, offs| {
            offs.retain(|off| off.can_be_overtaken(now));
            !offs.is_empty()
        });
        self.tracked = self.offs.values().map(Vec::len).sum();
        if self.tracked < MAX_TRACKED_NOTE_OFFS {
            return;
        }
        let oldest = self
            .offs
            .iter()
            .flat_map(|(key, offs)| {
                offs.iter()
                    .enumerate()
                    .map(move |(index, off)| (off.due, key, index))
            })
            .min_by_key(|(due, _, _)| *due)
            .map(|(_, key, index)| (key.clone(), index));
        if let Some((key, index)) = oldest
            && let Some(offs) = self.offs.get_mut(&key)
        {
            offs.swap_remove(index);
            if offs.is_empty() {
                self.offs.remove(&key);
            }
            self.tracked -= 1;
        }
    }
}

/// MIDI ports retained for the audible/candidate generation union.
///
/// Opening is attempted once per distinct port name in that union. A failure is
/// remembered so a missing device reports once per score rather than once per
/// onset, then retired after the score can no longer be audible.
const MAX_MIDI_OUTPUT_PORTS: usize = 16;
const MAX_MIDI_OUTPUTS_ACROSS_CUTOVERS: usize = MAX_MIDI_OUTPUT_PORTS * 2;
const MIDI_OUTPUT_RETRY_BACKOFF: Duration = Duration::from_secs(2);
const MAX_MIDI_OPEN_REQUESTS: usize = MAX_MIDI_OUTPUTS_ACROSS_CUTOVERS;
const MAX_MIDI_OPEN_RESULTS: usize = MAX_MIDI_OUTPUTS_ACROSS_CUTOVERS;
const MAX_PENDING_OPEN_MESSAGES_PER_PORT: usize = crate::MAX_PENDING_OPEN_WRITES_PER_PORT;
const MAX_PENDING_OPEN_MESSAGES_GLOBAL: usize = rustel_midi::MAX_QUEUED_MESSAGES;
const MAX_PENDING_MIDI_ERRORS: usize = MAX_MIDI_OUTPUTS_ACROSS_CUTOVERS;
const MAX_MIDI_ERROR_BYTES: usize = 2048;
pub(crate) const MAX_MIDI_PORT_NAME_BYTES: usize = 1024;

/// How a port is opened by name: the platform's stack, or a stand-in.
pub type MidiOpenFn = dyn Fn(&str) -> Result<MidiSender, String> + Send + Sync + 'static;

struct MidiOpenRequest {
    id: u64,
    name: String,
}

struct MidiOpenResult {
    id: u64,
    name: String,
    completed_at: Instant,
    result: Result<MidiSender, String>,
    error_truncated: bool,
}

struct MidiOpenQueue {
    pending: VecDeque<MidiOpenRequest>,
    desired: HashMap<String, u64>,
    stopping: bool,
}

/// Sole owner of synchronous platform output discovery/open.
///
/// The live producer only publishes bounded requests and drains bounded
/// results. The worker never holds either mutex while invoking the platform,
/// and shutdown detaches because a driver open may itself be wedged.
struct MidiOpenManager {
    requests: Arc<(Mutex<MidiOpenQueue>, Condvar)>,
    results: Arc<Mutex<VecDeque<MidiOpenResult>>>,
    dropped_results: Arc<AtomicU64>,
    thread: Option<std::thread::JoinHandle<()>>,
    startup_error: Option<String>,
}

fn bound_midi_error(mut message: String) -> (String, bool) {
    if message.len() <= MAX_MIDI_ERROR_BYTES {
        return (message, false);
    }
    const SUFFIX: &str = "…";
    let mut end = MAX_MIDI_ERROR_BYTES.saturating_sub(SUFFIX.len());
    while end != 0 && !message.is_char_boundary(end) {
        end -= 1;
    }
    message.truncate(end);
    message.push_str(SUFFIX);
    (message, true)
}

impl MidiOpenManager {
    fn new(opener: Arc<MidiOpenFn>) -> Self {
        let requests = Arc::new((
            Mutex::new(MidiOpenQueue {
                pending: VecDeque::new(),
                desired: HashMap::new(),
                stopping: false,
            }),
            Condvar::new(),
        ));
        let results = Arc::new(Mutex::new(VecDeque::new()));
        let dropped_results = Arc::new(AtomicU64::new(0));
        let spawned = {
            let requests = Arc::clone(&requests);
            let results = Arc::clone(&results);
            let dropped_results = Arc::clone(&dropped_results);
            std::thread::Builder::new()
                .name("rustel-midi-open".into())
                .spawn(move || {
                    let (lock, ready) = &*requests;
                    let mut queue = lock.lock().unwrap_or_else(|error| error.into_inner());
                    // A sender's Drop only requests shutdown. Its worker owns
                    // the platform connection until the completion bit sets,
                    // including when an in-flight open was canceled. Never
                    // ask WinMM to open that same port in the meantime.
                    let mut connections: Vec<(String, rustel_midi::MidiSenderCompletion)> =
                        Vec::new();
                    loop {
                        if queue.stopping {
                            return;
                        }
                        connections.retain(|(_, completion)| !completion.is_finished());
                        let available = queue.pending.iter().position(|request| {
                            !connections.iter().any(|(name, _)| name == &request.name)
                        });
                        let Some(index) = available else {
                            queue = if queue.pending.is_empty() {
                                ready.wait(queue).unwrap_or_else(|error| error.into_inner())
                            } else {
                                ready
                                    .wait_timeout(queue, Duration::from_millis(5))
                                    .unwrap_or_else(|error| error.into_inner())
                                    .0
                            };
                            continue;
                        };
                        let Some(request) = queue.pending.remove(index) else {
                            continue;
                        };
                        if queue.desired.get(&request.name) != Some(&request.id) {
                            continue;
                        }
                        drop(queue);
                        let (result, error_truncated) = match opener(&request.name) {
                            Ok(sender) => (Ok(sender), false),
                            Err(message) => {
                                let (message, truncated) = bound_midi_error(message);
                                (Err(message), truncated)
                            }
                        };
                        if let Ok(sender) = &result {
                            connections.push((request.name.clone(), sender.completion()));
                        }
                        queue = lock.lock().unwrap_or_else(|error| error.into_inner());
                        let still_desired = queue.desired.get(&request.name) == Some(&request.id);
                        if still_desired {
                            queue.desired.remove(&request.name);
                        }
                        drop(queue);
                        if !still_desired {
                            // Cancellation wins before result publication. A
                            // successful stale sender never received a score
                            // batch, and its nonblocking Drop stays entirely
                            // on this management path.
                            drop(result);
                            queue = lock.lock().unwrap_or_else(|error| error.into_inner());
                            continue;
                        }
                        let completed = MidiOpenResult {
                            id: request.id,
                            name: request.name.clone(),
                            completed_at: Instant::now(),
                            result,
                            error_truncated,
                        };
                        let mut output = results.lock().unwrap_or_else(|error| error.into_inner());
                        if output.len() == MAX_MIDI_OPEN_RESULTS {
                            // This is unreachable while the runtime respects
                            // its 32-entry cross-cutover cap, but retaining the
                            // newest results makes the memory bound explicit
                            // even under a future caller bug.
                            output.pop_front();
                            dropped_results.fetch_add(1, Ordering::Relaxed);
                        }
                        output.push_back(completed);
                        drop(output);
                        queue = lock.lock().unwrap_or_else(|error| error.into_inner());
                    }
                })
        };
        let (thread, startup_error) = match spawned {
            Ok(thread) => (Some(thread), None),
            Err(error) => {
                let (message, _) = bound_midi_error(format!(
                    "cannot start the MIDI output opener worker: {error}"
                ));
                (None, Some(message))
            }
        };
        Self {
            requests,
            results,
            dropped_results,
            thread,
            startup_error,
        }
    }

    fn enqueue(&self, request: MidiOpenRequest) -> Result<(), String> {
        if self.thread.is_none() {
            return Err(self
                .startup_error
                .clone()
                .unwrap_or_else(|| "the MIDI output opener is unavailable".into()));
        }
        let (lock, ready) = &*self.requests;
        let mut queue = lock.lock().unwrap_or_else(|error| error.into_inner());
        if queue.stopping {
            return Err("the MIDI output opener is stopping".into());
        }
        let desired_name = request.name.clone();
        let desired_id = request.id;
        if let Some(existing) = queue
            .pending
            .iter_mut()
            .find(|existing| existing.name == request.name)
        {
            *existing = request;
        } else {
            if queue.pending.len() >= MAX_MIDI_OPEN_REQUESTS {
                return Err("the bounded MIDI output opener queue is full".into());
            }
            queue.pending.push_back(request);
        }
        queue.desired.insert(desired_name, desired_id);
        drop(queue);
        ready.notify_one();
        Ok(())
    }

    fn take_results(&self) -> VecDeque<MidiOpenResult> {
        let mut results = self
            .results
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        std::mem::take(&mut *results)
    }

    fn cancel(&self, name: &str, id: u64) {
        let (lock, _) = &*self.requests;
        let mut queue = lock.lock().unwrap_or_else(|error| error.into_inner());
        if queue.desired.get(name) == Some(&id) {
            queue.desired.remove(name);
        }
        queue
            .pending
            .retain(|request| request.id != id || request.name != name);
    }

    fn take_dropped_results(&self) -> u64 {
        self.dropped_results.swap(0, Ordering::AcqRel)
    }
}

impl Drop for MidiOpenManager {
    fn drop(&mut self) {
        let (lock, ready) = &*self.requests;
        let mut queue = lock.lock().unwrap_or_else(|error| error.into_inner());
        queue.pending.clear();
        queue.desired.clear();
        queue.stopping = true;
        drop(queue);
        ready.notify_all();
        // Detach: the opener may be inside a platform call with no deadline.
        let _ = self.thread.take();
    }
}

struct PendingMidiBatch {
    generation: u64,
    onset_frame: u64,
    expires_at_frame: Option<u64>,
    messages: Vec<(Instant, rustel_midi::MidiMessage)>,
}

// The frame rule is in `render`, beside the audio event conversion. A
// takeover uses it in a build without MIDI too.
pub use crate::render::onset_frame_at;

struct MidiOutputEntry {
    name: String,
    sender: Option<MidiSender>,
    /// Score generations which can still schedule through this entry. A host-
    /// published virtual port is permanent, but still tracks score membership
    /// so a reload can prune that generation's queued future onsets.
    generations: std::collections::BTreeSet<u64>,
    published: bool,
    /// Old-only ports remain alive until the audio consumer crosses the
    /// continuity takeover. One deadline covers arbitrarily many rapid slider
    /// generations without retaining an unbounded generation set.
    retain_until_frame: u64,
    retry_at: Instant,
    failure_reported: bool,
    attempts: u64,
}

struct RetiringMidiSender {
    name: String,
    completion: rustel_midi::MidiSenderCompletion,
}

/// A note of the outgoing generation that sounds at its port at a takeover,
/// from the takeover frame on. The incoming generation plays that onset
/// again, and this note stands in for its copy.
struct KeptNote {
    /// The name of the port's entry.
    port: String,
    /// The generation whose copy the note stands in for.
    incoming: u64,
    onset_frame: u64,
    channel: u8,
    note: u8,
}

/// A platform output open failed, or a later open cleared that failure.
#[derive(Debug, PartialEq, Eq)]
pub enum MidiOutputNotice {
    OpenFailed { port: String, message: String },
    Opened { port: String },
}

pub struct MidiOutputs {
    open: Vec<MidiOutputEntry>,
    limit_reported: BTreeSet<u64>,
    invalid_name_reported: BTreeSet<u64>,
    retained_limit_reported: bool,
    retired_report: rustel_midi::MidiReport,
    retiring: Vec<RetiringMidiSender>,
    pending_report: rustel_midi::MidiReport,
    manager: MidiOpenManager,
    opening: HashMap<String, u64>,
    pending: HashMap<String, VecDeque<PendingMidiBatch>>,
    pending_by_port: HashMap<String, usize>,
    pending_messages: usize,
    next_pending_expiry_frame: Option<u64>,
    /// The notes of the last takeover that still stand in for a copy. Each
    /// takeover replaces the list, and the sender queues bound its length.
    kept_notes: Vec<KeptNote>,
    pending_notices: VecDeque<MidiOutputNotice>,
    warned_ports: BTreeSet<String>,
    next_request_id: u64,
    retry_backoff: Duration,
    pending_per_port_limit: usize,
    pending_global_limit: usize,
}

impl Default for MidiOutputs {
    fn default() -> Self {
        Self::with_opener(Arc::new(|name| MidiSender::open(Some(name))))
    }
}

impl MidiOutputs {
    pub fn new() -> Self {
        Self::default()
    }

    /// Outputs that open ports through `opener` instead of the platform:
    /// a capture in a test, the real thing everywhere else.
    pub fn with_opener(opener: Arc<MidiOpenFn>) -> Self {
        Self {
            open: Vec::new(),
            limit_reported: BTreeSet::new(),
            invalid_name_reported: BTreeSet::new(),
            retained_limit_reported: false,
            retired_report: rustel_midi::MidiReport::default(),
            retiring: Vec::new(),
            pending_report: rustel_midi::MidiReport::default(),
            manager: MidiOpenManager::new(opener),
            opening: HashMap::new(),
            pending: HashMap::new(),
            pending_by_port: HashMap::new(),
            pending_messages: 0,
            next_pending_expiry_frame: None,
            kept_notes: Vec::new(),
            pending_notices: VecDeque::new(),
            warned_ports: BTreeSet::new(),
            next_request_id: 1,
            retry_backoff: MIDI_OUTPUT_RETRY_BACKOFF,
            pending_per_port_limit: MAX_PENDING_OPEN_MESSAGES_PER_PORT,
            pending_global_limit: MAX_PENDING_OPEN_MESSAGES_GLOBAL,
        }
    }

    fn ensure_entry(&mut self, generation: u64, port: &str) -> Result<Option<usize>, String> {
        if port.len() > MAX_MIDI_PORT_NAME_BYTES {
            if !self.invalid_name_reported.insert(generation) {
                return Ok(None);
            }
            return Err(format!(
                "a MIDI output selector may contain at most {MAX_MIDI_PORT_NAME_BYTES} UTF-8 bytes"
            ));
        }
        let generation_ports = self
            .open
            .iter()
            .filter(|entry| entry.generations.contains(&generation))
            .count();
        if let Some(index) = self.open.iter().position(|entry| entry.name == port) {
            if !self.open[index].generations.contains(&generation) {
                if generation_ports >= MAX_MIDI_OUTPUT_PORTS {
                    if !self.limit_reported.insert(generation) {
                        return Ok(None);
                    }
                    return Err(format!(
                        "a score may name at most {MAX_MIDI_OUTPUT_PORTS} MIDI output ports at once; \"{port}\" was not opened"
                    ));
                }
                self.open[index].generations.insert(generation);
            }
            if port == rustel_core::UNREADABLE_MIDI_PORT {
                if self.open[index].failure_reported {
                    return Ok(None);
                }
                self.open[index].failure_reported = true;
                return Err(unreadable_midi_port_message());
            }
            return Ok(Some(index));
        }
        if generation_ports >= MAX_MIDI_OUTPUT_PORTS {
            if !self.limit_reported.insert(generation) {
                return Ok(None);
            }
            return Err(format!(
                "a score may name at most {MAX_MIDI_OUTPUT_PORTS} MIDI output ports at once; \"{port}\" was not opened"
            ));
        }
        if self.open.iter().filter(|entry| !entry.published).count()
            >= MAX_MIDI_OUTPUTS_ACROSS_CUTOVERS
        {
            if self.retained_limit_reported {
                return Ok(None);
            }
            self.retained_limit_reported = true;
            return Err(format!(
                "at most {MAX_MIDI_OUTPUTS_ACROSS_CUTOVERS} MIDI output ports may be retained across a live cutover; \"{port}\" was not opened"
            ));
        }
        let now = Instant::now();
        let generations = std::collections::BTreeSet::from([generation]);
        // The score named a port that did not reach `.midi()` as a literal.
        // Reported in the score's own terms - the sentinel itself would mean
        // nothing to a musician reading the line they just typed.
        if port == rustel_core::UNREADABLE_MIDI_PORT {
            self.open.push(MidiOutputEntry {
                name: port.to_string(),
                sender: None,
                generations,
                published: false,
                retain_until_frame: 0,
                retry_at: now + MIDI_OUTPUT_RETRY_BACKOFF,
                failure_reported: true,
                attempts: 0,
            });
            return Err(unreadable_midi_port_message());
        }
        self.open.push(MidiOutputEntry {
            name: port.to_string(),
            sender: None,
            generations,
            published: false,
            retain_until_frame: 0,
            retry_at: now,
            failure_reported: false,
            attempts: 0,
        });
        Ok(Some(self.open.len() - 1))
    }

    fn start_open(&mut self, index: usize) -> Result<bool, String> {
        let name = self.open[index].name.clone();
        if self.open[index].sender.is_some() || self.open[index].published {
            return Ok(true);
        }
        if self.opening.contains_key(&name) {
            return Ok(true);
        }
        // WinMM can refuse a second connection until the first sender's
        // worker has dropped its platform port. Its completion bit is set
        // after that destructor, so retain the new onset until then.
        if self.retiring.iter().any(|sender| sender.name == name) {
            return Ok(true);
        }
        if Instant::now() < self.open[index].retry_at {
            return Ok(false);
        }
        let request_id = self.next_request_id;
        self.next_request_id = self.next_request_id.wrapping_add(1).max(1);
        if let Err(message) = self.manager.enqueue(MidiOpenRequest {
            id: request_id,
            name: name.clone(),
        }) {
            self.open[index].retry_at = Instant::now() + self.retry_backoff;
            if !self.open[index].failure_reported {
                self.open[index].failure_reported = true;
                return Err(message);
            }
            return Ok(false);
        }
        self.open[index].attempts = self.open[index].attempts.saturating_add(1);
        self.opening.insert(name, request_id);
        Ok(true)
    }

    fn queue_pending_batch(&mut self, name: &str, batch: PendingMidiBatch) -> bool {
        let messages = batch.messages.len();
        if messages == 0 {
            return false;
        }
        let port_messages = self.pending_by_port.get(name).copied().unwrap_or(0);
        if messages > self.pending_per_port_limit.saturating_sub(port_messages)
            || messages
                > self
                    .pending_global_limit
                    .saturating_sub(self.pending_messages)
        {
            self.pending_report.refused_full = self
                .pending_report
                .refused_full
                .saturating_add(messages as u64);
            return false;
        }
        self.pending
            .entry(name.to_string())
            .or_default()
            .push_back(batch);
        self.pending_by_port
            .insert(name.to_string(), port_messages + messages);
        self.pending_messages += messages;
        true
    }

    fn take_pending(&mut self, name: &str) -> VecDeque<PendingMidiBatch> {
        let batches = self.pending.remove(name).unwrap_or_default();
        let messages = batches
            .iter()
            .map(|batch| batch.messages.len())
            .sum::<usize>();
        self.pending_messages = self.pending_messages.saturating_sub(messages);
        self.pending_by_port.remove(name);
        batches
    }

    fn queue_notice(&mut self, notice: MidiOutputNotice) {
        let port = match &notice {
            MidiOutputNotice::OpenFailed { port, .. } | MidiOutputNotice::Opened { port } => port,
        };
        match &notice {
            MidiOutputNotice::OpenFailed { .. } => {
                // Reserve one recovery slot per warning until the consumer takes it.
                // At capacity, omit new warning keys instead of losing a recovery.
                if !self.warned_ports.contains(port) {
                    if self.warned_ports.len() >= MAX_PENDING_MIDI_ERRORS {
                        self.pending_report.errors_dropped =
                            self.pending_report.errors_dropped.saturating_add(1);
                        return;
                    }
                    self.warned_ports.insert(port.clone());
                }
            }
            MidiOutputNotice::Opened { .. } if !self.warned_ports.contains(port) => return,
            MidiOutputNotice::Opened { .. } => {}
        }
        self.pending_notices.retain(|queued| match queued {
            MidiOutputNotice::OpenFailed { port: queued, .. }
            | MidiOutputNotice::Opened { port: queued } => queued != port,
        });
        self.pending_notices.push_back(notice);
    }

    fn take_notices(&mut self) -> Vec<MidiOutputNotice> {
        let notices = self.pending_notices.drain(..).collect::<Vec<_>>();
        for notice in &notices {
            if let MidiOutputNotice::Opened { port } = notice {
                self.warned_ports.remove(port);
            }
        }
        notices
    }

    fn queue_error(&mut self, port: String, message: String) {
        let message = if message.contains("MMSYSERR_ALLOCATED") {
            format!("{message}; the MIDI port may still be closing or be in use by another app")
        } else {
            message
        };
        let (message, truncated) = bound_midi_error(message);
        if truncated {
            self.pending_report.errors_dropped =
                self.pending_report.errors_dropped.saturating_add(1);
        }
        self.queue_notice(MidiOutputNotice::OpenFailed { port, message });
    }

    fn reap_retiring(&mut self) {
        let mut still_retiring = Vec::with_capacity(self.retiring.len());
        for sender in self.retiring.drain(..) {
            if sender.completion.is_finished() {
                add_midi_report(&mut self.retired_report, sender.completion.report());
            } else {
                still_retiring.push(sender);
            }
        }
        self.retiring = still_retiring;
    }

    fn retire_sender(&mut self, name: String, sender: MidiSender) {
        self.reap_retiring();
        let completion = sender.completion();
        drop(sender);
        self.retiring.push(RetiringMidiSender { name, completion });
    }

    fn flush_open_pending(&mut self) {
        let ready_names = self
            .open
            .iter()
            .filter(|entry| entry.sender.is_some() && self.pending.contains_key(&entry.name))
            .map(|entry| entry.name.clone())
            .collect::<Vec<_>>();
        for name in ready_names {
            let pending = self.take_pending(&name);
            let Some(sender) = self
                .open
                .iter()
                .find(|entry| entry.name == name)
                .and_then(|entry| entry.sender.as_ref())
            else {
                continue;
            };
            for batch in pending {
                sender.send_batch_for_generation_with_expiry_at(
                    batch.generation,
                    batch.onset_frame,
                    batch.expires_at_frame,
                    &batch.messages,
                );
            }
        }
    }

    fn apply_open_results(&mut self, flush_pending: bool) {
        self.reap_retiring();
        self.pending_report.errors_dropped = self
            .pending_report
            .errors_dropped
            .saturating_add(self.manager.take_dropped_results());
        for completed in self.manager.take_results() {
            if self.opening.get(&completed.name) != Some(&completed.id) {
                // The port was retired, reset, promoted, or superseded while
                // its platform call was in flight. Dropping a successful stale
                // sender is nonblocking and asks its own worker for silence.
                if let Ok(sender) = completed.result {
                    self.retire_sender(completed.name, sender);
                }
                continue;
            }
            self.opening.remove(&completed.name);
            let Some(index) = self
                .open
                .iter()
                .position(|entry| entry.name == completed.name)
            else {
                self.take_pending(&completed.name);
                continue;
            };
            match completed.result {
                Ok(sender) => {
                    if self.open[index].published || self.open[index].sender.is_some() {
                        self.take_pending(&completed.name);
                        self.retire_sender(completed.name, sender);
                        continue;
                    }
                    self.open[index].sender = Some(sender);
                    self.open[index].failure_reported = false;
                    self.open[index].retry_at = completed.completed_at;
                    self.queue_notice(MidiOutputNotice::Opened {
                        port: completed.name,
                    });
                }
                Err(message) => {
                    if completed.error_truncated {
                        self.pending_report.errors_dropped =
                            self.pending_report.errors_dropped.saturating_add(1);
                    }
                    self.take_pending(&completed.name);
                    self.open[index].retry_at = completed.completed_at + self.retry_backoff;
                    if !self.open[index].failure_reported {
                        self.open[index].failure_reported = true;
                        self.queue_error(completed.name, message);
                    }
                }
            }
        }
        // A new score may have queued onsets while an older sender with the
        // same selector was retiring. Open it once the old connection closes.
        let waiting = self
            .open
            .iter()
            .enumerate()
            .filter(|(_, entry)| self.pending.contains_key(&entry.name))
            .map(|(index, _)| index)
            .collect::<Vec<_>>();
        for index in waiting {
            if let Err(message) = self.start_open(index) {
                let name = self.open[index].name.clone();
                self.take_pending(&name);
                self.queue_error(name, message);
            }
        }
        if flush_pending {
            self.flush_open_pending();
        }
    }

    /// Reconcile the current audio frame before installing completed opens,
    /// then return each newly observed failure once.
    ///
    /// Ordering is part of the safety contract: a pre-takeover old batch may
    /// wait for an opening device only until that exact frame. Pruning it
    /// before a ready sender is flushed prevents a late platform open from
    /// sounding an old score after the new generation is audible.
    pub fn poll_at(
        &mut self,
        audible_generation: u64,
        session_generation: u64,
        current_frame: u64,
    ) -> Vec<String> {
        self.poll_notices_at(audible_generation, session_generation, current_frame)
            .into_iter()
            .filter_map(|notice| match notice {
                MidiOutputNotice::OpenFailed { message, .. } => Some(message),
                MidiOutputNotice::Opened { .. } => None,
            })
            .collect()
    }

    /// Reconcile the current audio frame and return named output open changes.
    /// Studio uses the port identity to resolve a warning after recovery.
    pub fn poll_notices_at(
        &mut self,
        audible_generation: u64,
        session_generation: u64,
        current_frame: u64,
    ) -> Vec<MidiOutputNotice> {
        self.sync_generations_at(audible_generation, session_generation, current_frame);
        self.take_notices()
    }

    #[cfg(test)]
    fn poll(&mut self) -> Vec<String> {
        self.apply_open_results(true);
        self.take_notices()
            .into_iter()
            .filter_map(|notice| match notice {
                MidiOutputNotice::OpenFailed { message, .. } => Some(message),
                MidiOutputNotice::Opened { .. } => None,
            })
            .collect()
    }

    /// Admit one complete score onset without ever opening a platform device
    /// on the live producer.
    ///
    /// While a name is opening, the earliest complete batches are retained in
    /// order under per-port and process-wide hard caps. `true` means the whole
    /// batch crossed this producer-side boundary (either into an open sender
    /// or the bounded pre-open queue), preserving UI acceptance semantics.
    ///
    /// A batch whose note already sounds at the port is left out and counts
    /// as accepted: see [`Self::take_over_generation_from`].
    pub fn submit_batch_for_generation_at(
        &mut self,
        generation: u64,
        port: &str,
        onset_frame: u64,
        messages: Vec<(Instant, rustel_midi::MidiMessage)>,
    ) -> Result<bool, String> {
        self.reap_retiring();
        if messages.is_empty() {
            return Ok(false);
        }
        let Some(index) = self.ensure_entry(generation, port)? else {
            return Ok(false);
        };
        if self.kept_note_stands_in(generation, port, onset_frame, &messages) {
            return Ok(true);
        }
        if let Some(sender) = self.open[index].sender.as_ref() {
            return Ok(sender.send_batch_for_generation_at(generation, onset_frame, &messages));
        }
        if !self.start_open(index)? {
            return Ok(false);
        }
        let name = self.open[index].name.clone();
        Ok(self.queue_pending_batch(
            &name,
            PendingMidiBatch {
                generation,
                onset_frame,
                expires_at_frame: None,
                messages,
            },
        ))
    }

    /// Whether a note that the last takeover kept stands in for this batch:
    /// the same note on the same port at the same onset frame. The note
    /// then stands in for no other batch.
    ///
    /// The planner gives an onset one note at most. A batch with more
    /// note-ons is not a planned onset and is never left out.
    fn kept_note_stands_in(
        &mut self,
        generation: u64,
        port: &str,
        onset_frame: u64,
        messages: &[(Instant, rustel_midi::MidiMessage)],
    ) -> bool {
        if self.kept_notes.is_empty() {
            return false;
        }
        let mut note_ons = messages
            .iter()
            .filter_map(|(_, message)| match note_key(message) {
                Some((channel, note, true)) => Some((channel, note)),
                _ => None,
            });
        let (Some((channel, note)), None) = (note_ons.next(), note_ons.next()) else {
            return false;
        };
        let kept = self.kept_notes.iter().position(|kept| {
            kept.incoming == generation
                && kept.onset_frame == onset_frame
                && kept.channel == channel
                && kept.note == note
                && kept.port == port
        });
        match kept {
            Some(index) => {
                self.kept_notes.swap_remove(index);
                true
            }
            None => false,
        }
    }

    /// Reserve a score generation's use of a port without opening it.
    ///
    /// The live producer calls this for a successful step's plannable routes
    /// before reconciling the exact takeover frame. That lets a replacement
    /// generation reuse the already-open same-name sender instead of retiring
    /// it at the boundary and racing its emergency silence against a reopen.
    /// Platform discovery remains exclusively on the asynchronous opener.
    pub fn reserve_generation_port(&mut self, generation: u64, port: &str) -> Result<bool, String> {
        self.reap_retiring();
        Ok(self.ensure_entry(generation, port)?.is_some())
    }

    /// Nonblocking compatibility probe. New live code should submit a whole
    /// batch through [`Self::submit_batch_for_generation_at`] and call
    /// [`Self::poll_at`] with the current audio frame. This legacy API starts
    /// an asynchronous open and may return `None` until that open is installed
    /// by `poll_at`; it never performs a platform open itself.
    fn sender_for(&mut self, generation: u64, port: &str) -> Result<Option<&MidiSender>, String> {
        // Install a completed sender for backwards compatibility, but never
        // flush score batches without a current audio-frame reconciliation.
        self.apply_open_results(false);
        let Some(index) = self.ensure_entry(generation, port)? else {
            return Ok(None);
        };
        let _ = self.start_open(index)?;
        Ok(self.open[index].sender.as_ref())
    }

    /// Backwards-compatible nonblocking sender lookup.
    ///
    /// Returns `None` while an asynchronous platform open is pending. Callers
    /// that need score-safe generation pruning should prefer the batch API.
    pub fn sender(&mut self, port: &str) -> Result<Option<&MidiSender>, String> {
        self.sender_for(0, port)
    }

    fn recount_pending(&mut self) {
        self.pending_by_port.clear();
        self.pending_messages = 0;
        for (name, batches) in &self.pending {
            let messages = batches
                .iter()
                .map(|batch| batch.messages.len())
                .sum::<usize>();
            self.pending_by_port.insert(name.clone(), messages);
            self.pending_messages = self.pending_messages.saturating_add(messages);
        }
    }

    fn prune_pending_generation(&mut self, generation: u64, takeover_frame: u64) {
        let mut marked_expiry = false;
        self.pending.retain(|_, batches| {
            batches.retain_mut(|batch| {
                if batch.generation != generation {
                    return true;
                }
                if batch.onset_frame >= takeover_frame {
                    return false;
                }
                batch.expires_at_frame = Some(
                    batch
                        .expires_at_frame
                        .map_or(takeover_frame, |deadline| deadline.min(takeover_frame)),
                );
                marked_expiry = true;
                true
            });
            !batches.is_empty()
        });
        self.recount_pending();
        if marked_expiry {
            self.next_pending_expiry_frame = Some(
                self.next_pending_expiry_frame
                    .map_or(takeover_frame, |known| known.min(takeover_frame)),
            );
        }
    }

    fn prune_expired_pending(&mut self, current_frame: u64) {
        if self
            .next_pending_expiry_frame
            .is_none_or(|deadline| deadline > current_frame)
        {
            return;
        }
        let mut changed = false;
        self.pending.retain(|_, batches| {
            let before = batches.len();
            batches.retain(|batch| {
                !batch
                    .expires_at_frame
                    .is_some_and(|deadline| deadline <= current_frame)
            });
            changed |= batches.len() != before;
            !batches.is_empty()
        });
        if changed {
            self.recount_pending();
        }
        self.next_pending_expiry_frame = self
            .pending
            .values()
            .flat_map(|batches| batches.iter())
            .filter_map(|batch| batch.expires_at_frame)
            .filter(|deadline| *deadline > current_frame)
            .min();
    }

    fn cancel_name(&mut self, name: &str) {
        if let Some(request_id) = self.opening.remove(name) {
            self.manager.cancel(name, request_id);
        }
        self.take_pending(name);
    }

    /// Drop `generation`'s batches that have not started and fall at or
    /// after `frame`, retained or queued, and let the rest expire there.
    pub fn prune_generation_from(&mut self, generation: u64, frame: u64) {
        self.prune_generation_reporting_started(generation, frame);
    }

    /// [`Self::prune_generation_from`], with each note the senders keep
    /// because its batch has started, under the name of its port.
    fn prune_generation_reporting_started(
        &mut self,
        generation: u64,
        frame: u64,
    ) -> Vec<(String, rustel_midi::StartedNote)> {
        // Prune producer-retained batches before installing any just-completed
        // sender result, so an async open can never briefly flush a future
        // onset that the simultaneous audio cutover has already superseded.
        self.prune_pending_generation(generation, frame);
        let mut started = Vec::new();
        for entry in &self.open {
            // Queue tags are authoritative. Prune every sender, including a
            // permanent published port; host-originated untagged messages are
            // preserved and a sender unused by this generation is a no-op.
            if let Some(sender) = entry.sender.as_ref() {
                let (_, notes) = sender.prune_generation_reporting_started(generation, frame);
                started.extend(notes.into_iter().map(|note| (entry.name.clone(), note)));
            }
        }
        started
    }

    /// Mark the generation being replaced as audible through `takeover_frame`.
    ///
    /// `device.generation()` flips immediately, while the callback deliberately
    /// keeps old events before the takeover. Removing the old membership here
    /// and aggregating it into one deadline avoids both premature all-notes-off
    /// and an unbounded generation set during a rapid slider drag.
    pub fn begin_generation_transition(
        &mut self,
        previous_generation: u64,
        next_generation: u64,
        takeover_frame: u64,
    ) {
        self.begin_generation_transition_from(
            previous_generation,
            next_generation,
            takeover_frame,
            takeover_frame,
        );
    }

    /// [`Self::begin_generation_transition`], pruning the replaced
    /// generation from `retired_from`: its takeover, or an earlier frame its
    /// audio was already retired from.
    pub fn begin_generation_transition_from(
        &mut self,
        previous_generation: u64,
        next_generation: u64,
        takeover_frame: u64,
        retired_from: u64,
    ) {
        self.transition(
            previous_generation,
            next_generation,
            takeover_frame,
            retired_from,
            false,
        );
    }

    /// [`Self::begin_generation_transition_from`] for a flip that carries
    /// `cut`, as the device's `set_generation` does.
    ///
    /// Without a cut the outgoing notes ring out, and the incoming
    /// generation plays the onsets from `takeover_frame` again. A control
    /// requery can publish after that frame. The port then has the outgoing
    /// note of an onset between the two, and the incoming copy would sound
    /// it again. So an outgoing note from the takeover frame on stands in
    /// for its copy if its note-on has started and its note-off is still
    /// queued when this call runs: the next batch of `next_generation` with
    /// that note on that port at that onset frame is left out. The note
    /// keeps its own note-off. A note that has ended by then has no
    /// stand-in, and its copy goes out.
    ///
    /// A cut starts the score again, and a flip without a takeover frame
    /// starts a new lifetime. Their onsets are not copies: all are sent.
    pub fn take_over_generation_from(
        &mut self,
        previous_generation: u64,
        next_generation: u64,
        takeover_frame: u64,
        retired_from: u64,
        cut: rustel_audio::TakeoverCut,
    ) {
        let rings_out = cut == rustel_audio::TakeoverCut::None && takeover_frame != 0;
        self.transition(
            previous_generation,
            next_generation,
            takeover_frame,
            retired_from,
            rings_out,
        );
    }

    fn transition(
        &mut self,
        previous_generation: u64,
        next_generation: u64,
        takeover_frame: u64,
        retired_from: u64,
        keep_started: bool,
    ) {
        if previous_generation == next_generation {
            return;
        }
        let started = self.prune_generation_reporting_started(previous_generation, retired_from);
        // The copies of an earlier takeover go out in the tick of that takeover.
        self.kept_notes.clear();
        if keep_started {
            self.kept_notes.extend(
                started
                    .into_iter()
                    .filter(|(_, started)| started.onset_frame >= takeover_frame)
                    .filter_map(|(port, started)| {
                        let (channel, note, _) = note_key(&started.note_off)?;
                        Some(KeptNote {
                            port,
                            incoming: next_generation,
                            onset_frame: started.onset_frame,
                            channel,
                            note,
                        })
                    }),
            );
        }
        for entry in &mut self.open {
            let was_previous = entry.generations.remove(&previous_generation);
            if was_previous && !entry.published {
                entry.retain_until_frame = entry.retain_until_frame.max(takeover_frame);
            }
        }
        // Do not install an asynchronous result here: this callback runs
        // inside the producer step and has no exact current audio frame. The
        // result may contain a pre-previous batch whose own deadline passed
        // since the last poll. `poll_at` immediately after the step first
        // reconciles every exact expiry, then installs and flushes safely.
    }

    /// Retain only ports needed by the audible/candidate generation union,
    /// plus old-only ports whose continuity takeover has not passed yet.
    pub fn sync_generations(&mut self, audible_generation: u64, session_generation: u64) {
        self.sync_generations_at(audible_generation, session_generation, u64::MAX);
    }

    pub fn sync_generations_at(
        &mut self,
        audible_generation: u64,
        session_generation: u64,
        current_frame: u64,
    ) {
        self.prune_expired_pending(current_frame);
        let wants =
            |generation| generation == audible_generation || generation == session_generation;
        let mut retained = Vec::with_capacity(self.open.len());
        let mut retired_names = Vec::new();
        let mut retired_senders = Vec::new();
        let mut expired_memberships = Vec::new();
        for mut entry in self.open.drain(..) {
            if let Some(sender) = entry.sender.as_ref() {
                sender.prune_expired_at(current_frame);
            }
            entry.generations.retain(|generation| wants(*generation));
            // The takeover is the hard retirement boundary. Keeping an old
            // sender merely because it has queued work lets a score pin each
            // port for the full one-hour scheduling clamp and eventually
            // exhaust the cross-cutover allowance. Dropping the sender here
            // atomically discards that stale future work and sends all-notes-
            // off/all-sound-off on every channel, so sounding old-generation
            // notes still end safely.
            let retirement_pending =
                entry.retain_until_frame != 0 && entry.retain_until_frame > current_frame;
            if entry.retain_until_frame != 0 && !retirement_pending {
                entry.retain_until_frame = 0;
                expired_memberships.push((entry.name.clone(), entry.generations.clone()));
            }
            if entry.published || !entry.generations.is_empty() || retirement_pending {
                retained.push(entry);
            } else {
                retired_names.push(entry.name.clone());
                if let Some(sender) = entry.sender.take() {
                    retired_senders.push((entry.name.clone(), sender));
                }
            }
        }
        self.open = retained;
        for (name, sender) in retired_senders {
            self.retire_sender(name, sender);
        }
        let mut pruned_expired = false;
        for (name, generations) in expired_memberships {
            if let Some(batches) = self.pending.get_mut(&name) {
                batches.retain(|batch| generations.contains(&batch.generation));
                pruned_expired = true;
            }
        }
        if pruned_expired {
            self.pending.retain(|_, batches| !batches.is_empty());
            self.recount_pending();
        }
        for name in retired_names {
            self.cancel_name(&name);
        }
        // A result already completed for a retired request is now stale and
        // discarded; retained current results may safely flush their pruned
        // pending batches.
        self.apply_open_results(true);
        self.limit_reported.retain(|generation| wants(*generation));
        self.invalid_name_reported
            .retain(|generation| wants(*generation));
        if self.open.iter().filter(|entry| !entry.published).count()
            < MAX_MIDI_OUTPUTS_ACROSS_CUTOVERS
        {
            self.retained_limit_reported = false;
        }
    }

    /// Reset every score-owned MIDI timeline after an audio output recycle.
    ///
    /// MIDI queue tags and retention deadlines are expressed in the audio
    /// stream's absolute frame domain. Reusing either after a sample-rate
    /// change would compare coordinates from different clocks; even a
    /// same-rate recycle discarded the audio horizon those messages belonged
    /// to. Published virtual ports remain available to the host, but their
    /// score membership is empty and their queue is replaced with emergency
    /// silence. Ordinary score ports are retired outright so their worker
    /// performs the same hard-silence shutdown before a later generation can
    /// reopen them.
    pub fn reset_after_audio_recycle(&mut self) {
        // Invalidate in-flight request identities and old-frame pending work
        // before consuming any completion that raced with the recycle.
        for (name, request_id) in self.opening.drain() {
            self.manager.cancel(&name, request_id);
        }
        self.pending.clear();
        self.pending_by_port.clear();
        self.pending_messages = 0;
        self.next_pending_expiry_frame = None;
        self.kept_notes.clear();
        let mut retained = Vec::new();
        let mut retired_senders = Vec::new();
        for mut entry in std::mem::take(&mut self.open) {
            entry.generations.clear();
            entry.retain_until_frame = 0;
            if entry.published {
                if let Some(sender) = entry.sender.as_ref() {
                    sender.silence_all();
                }
                retained.push(entry);
            } else if let Some(sender) = entry.sender.take() {
                retired_senders.push((entry.name.clone(), sender));
            }
        }
        self.open = retained;
        for (name, sender) in retired_senders {
            self.retire_sender(name, sender);
        }
        self.limit_reported.clear();
        self.invalid_name_reported.clear();
        self.retained_limit_reported = false;
        self.apply_open_results(true);
    }

    /// Request emergency silence from every connected output and wait only up
    /// to `timeout` for detached sender workers to finish their platform sends
    /// and connection destructors.
    ///
    /// This is an end-of-set boundary, never a producer tick. A wedged driver
    /// cannot extend the deadline; unfinished completion tokens remain in the
    /// report so counters do not disappear while retirement continues. An
    /// opener still inside a platform call is deliberately not part of this
    /// deadline: its retained batches are cleared before cancellation, so it
    /// has never emitted score data and poses no stuck-note risk.
    pub fn shutdown(&mut self, timeout: Duration) -> bool {
        for (name, request_id) in self.opening.drain() {
            self.manager.cancel(&name, request_id);
        }
        self.pending.clear();
        self.pending_by_port.clear();
        self.pending_messages = 0;
        self.next_pending_expiry_frame = None;

        let mut senders = Vec::new();
        for mut entry in std::mem::take(&mut self.open) {
            if let Some(sender) = entry.sender.take() {
                senders.push((entry.name.clone(), sender));
            }
        }
        for (name, sender) in senders {
            self.retire_sender(name, sender);
        }
        self.apply_open_results(true);

        let deadline = Instant::now() + timeout;
        loop {
            self.apply_open_results(true);
            if self.retiring.is_empty() {
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    pub fn is_empty(&self) -> bool {
        self.open.is_empty()
    }

    /// Publish a virtual port under `name`, so other software can connect
    /// to it without any loopback set up first.
    ///
    /// Driven by the host (`--midi-virtual`), never by the score: which ports
    /// exist is a fact about this machine, and a score that said it would stop
    /// running on strudel.cc, where no such thing is possible.
    ///
    /// Registered under `name` so an ordinary `.midi('name')` finds it exactly
    /// as it would a hardware port.
    /// This is intentionally a startup-only API. Virtual-port creation is a
    /// synchronous platform operation and the CLI calls it before entering the
    /// producer loop. Score-driven output must use the asynchronous batch
    /// submission path; retirement of this sender remains nonblocking.
    pub fn publish_at_startup(&mut self, name: &str) -> Result<(), String> {
        if let Some(index) = self.open.iter().position(|entry| entry.name == name) {
            if self.open[index].published {
                return Ok(());
            }
            if self.open.iter().filter(|entry| entry.published).count() >= MAX_MIDI_OUTPUT_PORTS {
                return Err(format!(
                    "at most {MAX_MIDI_OUTPUT_PORTS} MIDI output ports may be open or published"
                ));
            }
            let sender = Self::create_virtual(name)?;
            self.cancel_name(name);
            if let Some(previous) = self.open[index].sender.replace(sender) {
                self.retire_sender(name.to_string(), previous);
            }
            self.open[index].published = true;
            return Ok(());
        }
        if name.len() > MAX_MIDI_PORT_NAME_BYTES {
            return Err(format!(
                "a MIDI output name may contain at most {MAX_MIDI_PORT_NAME_BYTES} UTF-8 bytes"
            ));
        }
        if self.open.iter().filter(|entry| entry.published).count() >= MAX_MIDI_OUTPUT_PORTS {
            return Err(format!(
                "at most {MAX_MIDI_OUTPUT_PORTS} MIDI output ports may be open or published"
            ));
        }
        let sender = Self::create_virtual(name)?;
        self.open.push(MidiOutputEntry {
            name: name.to_string(),
            sender: Some(sender),
            generations: std::collections::BTreeSet::new(),
            published: true,
            retain_until_frame: 0,
            retry_at: Instant::now(),
            failure_reported: false,
            attempts: 1,
        });
        Ok(())
    }

    /// Backwards-compatible name for the startup-only virtual publication
    /// operation. Product code uses [`Self::publish_at_startup`] so the
    /// synchronous boundary is explicit at the call site.
    pub fn publish(&mut self, name: &str) -> Result<(), String> {
        self.publish_at_startup(name)
    }

    /// Create a virtual port where the platform allows it.
    ///
    /// CoreMIDI and ALSA can publish a port other software connects to;
    /// WinMM cannot, and says so rather than failing obscurely.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    fn create_virtual(name: &str) -> Result<MidiSender, String> {
        MidiSender::create_virtual(name)
    }

    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    fn create_virtual(name: &str) -> Result<MidiSender, String> {
        Err(format!(
            "this platform cannot create the virtual MIDI port \"{name}\": Windows has no such \
             facility. Install a loopback driver such as loopMIDI and pass its port name instead"
        ))
    }

    /// Combined counters across every open port.
    pub fn report(&self) -> rustel_midi::MidiReport {
        let mut total = self.retired_report;
        add_midi_report(&mut total, self.pending_report);
        for entry in &self.open {
            if let Some(sender) = &entry.sender {
                add_midi_report(&mut total, sender.report());
            }
        }
        for sender in &self.retiring {
            add_midi_report(&mut total, sender.completion.report());
        }
        total
    }
}

fn add_midi_report(total: &mut rustel_midi::MidiReport, report: rustel_midi::MidiReport) {
    total.sent = total.sent.saturating_add(report.sent);
    total.dropped_late = total.dropped_late.saturating_add(report.dropped_late);
    total.refused_full = total.refused_full.saturating_add(report.refused_full);
    total.send_errors = total.send_errors.saturating_add(report.send_errors);
    total.errors_dropped = total.errors_dropped.saturating_add(report.errors_dropped);
}

fn unreadable_midi_port_message() -> String {
    concat!(
        "the MIDI port did not reach .midi() as a literal name. Use a ",
        "single-quoted name - .midi('IAC Driver Bus 1') - or the index ",
        "that `rustel midi-list` prints, as .midi(0)"
    )
    .to_string()
}

#[cfg(test)]
mod offset_tests {
    use super::*;
    use rustel_midi::MidiMessage;

    #[test]
    fn planned_offsets_stay_within_the_scheduling_bounds() {
        let base = Instant::now();
        let message = MidiMessage::note_on(0, 60, 100);
        for (offset, expected) in [
            (f64::NAN, 0.0),
            (f64::NEG_INFINITY, 0.0),
            (-0.5, 0.0),
            (0.0, 0.0),
            (0.5, 0.5),
            (MAX_OFFSET_SECS + 1.0, MAX_OFFSET_SECS),
            (f64::INFINITY, MAX_OFFSET_SECS),
        ] {
            let batch = NoteOrdering::new().stamp_batch("p", 0.0, base, &[(offset, message)]);
            assert_eq!(batch.len(), 1);
            assert_eq!(batch[0].0, base + Duration::from_secs_f64(expected));
            assert_eq!(batch[0].1.as_slice(), message.as_slice());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wire_batch(
        due: Instant,
        message: rustel_midi::MidiMessage,
    ) -> Vec<(Instant, rustel_midi::MidiMessage)> {
        vec![(due, message)]
    }

    fn poll_until(
        outputs: &mut MidiOutputs,
        condition: impl Fn(&MidiOutputs) -> bool,
    ) -> Vec<String> {
        let deadline = Instant::now() + Duration::from_secs(2);
        let mut errors = Vec::new();
        loop {
            errors.extend(outputs.poll());
            if condition(outputs) {
                return errors;
            }
            assert!(Instant::now() < deadline, "async MIDI output timed out");
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    fn wait_for_capture(capture: &rustel_midi::CapturePort, count: usize) {
        let deadline = Instant::now() + Duration::from_secs(2);
        while capture.messages().len() < count {
            assert!(Instant::now() < deadline, "MIDI capture timed out");
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    struct BlockingFirstMidiPort {
        capture: rustel_midi::CapturePort,
        entered: Option<std::sync::mpsc::SyncSender<()>>,
        release: std::sync::mpsc::Receiver<()>,
    }

    impl rustel_midi::MidiPort for BlockingFirstMidiPort {
        fn send(&mut self, bytes: &[u8]) -> Result<(), String> {
            if let Some(entered) = self.entered.take() {
                entered
                    .send(())
                    .map_err(|_| "blocking MIDI observer disappeared".to_string())?;
                self.release
                    .recv_timeout(Duration::from_secs(2))
                    .map_err(|_| "blocking MIDI send was not released".to_string())?;
            }
            self.capture.send(bytes)
        }
    }

    struct BlockingDropMidiPort {
        entered: std::sync::mpsc::SyncSender<()>,
        release: std::sync::mpsc::Receiver<()>,
        closed: Arc<std::sync::atomic::AtomicBool>,
    }

    impl rustel_midi::MidiPort for BlockingDropMidiPort {
        fn send(&mut self, _bytes: &[u8]) -> Result<(), String> {
            Ok(())
        }
    }

    impl Drop for BlockingDropMidiPort {
        fn drop(&mut self) {
            let _ = self.entered.send(());
            let _ = self.release.recv_timeout(Duration::from_secs(2));
            self.closed
                .store(true, std::sync::atomic::Ordering::Release);
        }
    }

    /// A midimap turns a named control into a CC message. The whole chain:
    /// registration (settings-scoped), the hap's `midimap` selector, the range
    /// normalisation, and the planner emitting the wire bytes.
    #[test]
    fn a_midimap_derives_ccs_from_named_controls() {
        let settings = rustel_core::settings::RuntimeSettings::default();
        let _bind = settings.bind();
        rustel_core::midimap::register_midi_map_json(
            "mymap",
            r#"{"lpf": {"ccn": 74, "min": 0, "max": 4000}}"#,
        )
        .expect("registers");
        rustel_core::midimap::register_midi_map_json("default", r#"{"gain": 7}"#)
            .expect("registers");

        // The named map scales lpf into 0..1.
        let got = midi_onset(&onset(serde_json::json!({
            "midiport": "x", "midimap": "mymap", "cutoff": 1000.0,
        })))
        .expect("midi hap");
        assert_eq!(got.controls.mapped_ccs, vec![(74, 0.25)]);
        // And the planner sends it as a real CC: 0.25 * 127 rounds to 32.
        let planned = rustel_midi::plan(&got.controls, 0.5);
        assert!(
            planned
                .iter()
                .any(|(_, message)| message.as_slice() == [0xb0, 74, 32]),
            "no CC 74 on the wire: {planned:?}"
        );

        // No `midimap` control means the map registered as "default".
        let got = midi_onset(&onset(serde_json::json!({
            "midiport": "x", "gain": 0.5,
        })))
        .expect("midi hap");
        assert_eq!(got.controls.mapped_ccs, vec![(7, 0.5)]);

        // A map nobody registered derives nothing - and must not panic.
        let got = midi_onset(&onset(serde_json::json!({
            "midiport": "x", "midimap": "nope", "cutoff": 1000.0,
        })))
        .expect("midi hap");
        assert!(got.controls.mapped_ccs.is_empty());
    }

    #[test]
    fn mapped_aliases_and_custom_controls_keep_a_deterministic_order() {
        let settings = rustel_core::settings::RuntimeSettings::default();
        let _bind = settings.bind();
        // Registration canonicalises `cutoff` to `lpf`; the hap deliberately
        // uses the canonical spelling to cover both directions of the alias
        // surface. Custom entries remain literal and are emitted by name.
        rustel_core::midimap::register_midi_map_json(
            "mixed",
            r#"{
            "z_custom": 12,
            "cutoff": {"ccn": 74, "min": 0, "max": 4000},
            "a_custom": 11
        }"#,
        )
        .expect("register mixed map");

        let got = midi_onset(&onset(serde_json::json!({
            "midiport": "x",
            "midimap": "mixed",
            "lpf": 1000.0,
            "z_custom": 0.8,
            "a_custom": 0.2,
        })))
        .expect("midi hap");
        assert_eq!(
            got.controls.mapped_ccs,
            vec![(74, 0.25), (11, 0.2), (12, 0.8)],
            "known aliases must be preserved and custom controls sorted"
        );
    }

    #[test]
    fn a_huge_unrelated_hap_object_does_not_change_the_bounded_map_surface() {
        let settings = rustel_core::settings::RuntimeSettings::default();
        let _bind = settings.bind();
        rustel_core::midimap::register_midi_map_json("bounded", r#"{"mapped_custom": 23}"#)
            .expect("register custom map");

        // The post-query MIDI conversion must walk the capped registered map,
        // not this score-controlled object. Keep this far above every native
        // map/alias ceiling so a future reintroduction of hap scanning is
        // conspicuous in both profiling and review.
        let mut value = serde_json::Map::new();
        for index in 0..20_000 {
            value.insert(format!("unrelated_{index:05}"), serde_json::json!(index));
        }
        value.insert("midiport".into(), serde_json::json!("x"));
        value.insert("midimap".into(), serde_json::json!("bounded"));
        value.insert("mapped_custom".into(), serde_json::json!(0.5));

        let got = midi_onset(&onset(serde_json::Value::Object(value))).expect("midi hap");
        assert_eq!(got.controls.mapped_ccs, vec![(23, 0.5)]);
    }

    fn onset(value: serde_json::Value) -> OnsetEventJson {
        OnsetEventJson {
            onset_id: 1,
            generation: 1,
            whole_begin: "0/1".into(),
            duration_secs: 0.5,
            target_time: 12.5,
            live_controls: [0; 3],
            ui_visuals: 0,
            value: ValueJson::Raw(value),
            value_show: String::new(),
            log_line: None,
        }
    }

    #[test]
    fn a_hap_without_a_port_is_not_a_midi_hap() {
        assert!(midi_onset(&onset(serde_json::json!({ "note": 60, "s": "bd" }))).is_none());
    }

    #[test]
    fn the_port_may_be_a_name_or_an_index() {
        let named = midi_onset(&onset(serde_json::json!({ "midiport": "IAC Bus 1" }))).unwrap();
        assert_eq!(named.port, "IAC Bus 1");
        // Mini-notation reduces a bare 0 to a number, not a string.
        let indexed = midi_onset(&onset(serde_json::json!({ "midiport": 0 }))).unwrap();
        assert_eq!(indexed.port, "0");
    }

    #[test]
    fn controls_and_timing_come_through_intact() {
        let got = midi_onset(&onset(serde_json::json!({
            "midiport": "x",
            "note": 64,
            "gain": 0.5,
            "midichan": 3,
            "ccn": 74,
            "ccv": 0.25,
            "midicmd": "start",
        })))
        .unwrap();
        assert_eq!(got.onset_id, 1);
        assert_eq!(got.generation, 1);
        assert_eq!(got.target_time, 12.5);
        assert_eq!(got.duration_secs, 0.5);
        assert_eq!(got.controls.note, Some(64.0));
        assert_eq!(got.controls.gain, Some(0.5));
        assert_eq!(got.controls.midichan, Some(3.0));
        assert_eq!(got.controls.ccn, Some(74.0));
        assert_eq!(got.controls.ccv, Some(0.25));
        assert_eq!(got.controls.midicmd.as_deref(), Some("start"));
    }

    #[test]
    fn midi_options_are_defaults_that_the_scores_own_controls_outrank() {
        let got = midi_onset(&onset(serde_json::json!({
            "midiport": "x",
            "note": 64,
            "gain": 0.25,
            "velocity": 0.4,
            "midichan": 3,
            "midiopts": {
                "gain": 0.9,
                "velocity": 0.8,
                "midichannel": 9
            }
        })))
        .expect("MIDI intent");
        assert_eq!(got.controls.gain, Some(0.25));
        assert_eq!(got.controls.velocity, Some(0.4));
        assert_eq!(got.controls.midichan, Some(3.0));

        let fallback = midi_onset(&onset(serde_json::json!({
            "midiport": "x",
            "note": 64,
            "midiopts": {
                "gain": 0.7,
                "velocity": 0.6,
                "midichannel": 11
            }
        })))
        .expect("MIDI intent");
        assert_eq!(fallback.controls.gain, Some(0.7));
        assert_eq!(fallback.controls.velocity, Some(0.6));
        assert_eq!(fallback.controls.midichan, Some(11.0));
    }

    #[test]
    fn is_controller_and_note_offset_cross_the_boundary() {
        let got = midi_onset(&onset(serde_json::json!({
            "midiport": "x",
            "note": 64,
            "midiopts": { "isController": true, "noteOffsetMs": 10 }
        })))
        .expect("MIDI intent");
        assert!(got.controls.is_controller);
        assert_eq!(got.controls.note_offset_ms, Some(10.0));

        let plain = midi_onset(&onset(serde_json::json!({
            "midiport": "x",
            "note": 64
        })))
        .expect("MIDI intent");
        assert!(!plain.controls.is_controller);
        assert_eq!(plain.controls.note_offset_ms, None);
    }

    #[test]
    fn unknown_midi_commands_are_not_cloned_into_live_intents() {
        let oversized = "not-a-command".repeat(100_000);
        let got = midi_onset(&onset(serde_json::json!({
            "midiport": "x",
            "midicmd": oversized,
        })))
        .unwrap();
        assert!(got.controls.midicmd.is_none());

        for command in ["clock", "midiClock", "start", "stop", "continue"] {
            let got = midi_onset(&onset(serde_json::json!({
                "midiport": "x",
                "midicmd": command,
            })))
            .unwrap();
            assert_eq!(got.controls.midicmd.as_deref(), Some(command));
        }
    }

    /// A note written as a name must send MIDI. Names arrive here as strings,
    /// because `rustel-voice` resolves them where the audio path needs a
    /// frequency.
    #[test]
    fn a_note_name_is_a_pitch_the_same_way_a_number_is() {
        let got = midi_onset(&onset(serde_json::json!({ "midiport": "x", "note": "c3" }))).unwrap();
        // Octave 3 is the default the audio path uses, so both paths agree.
        assert_eq!(got.controls.note, Some(48.0));

        for (name, expected) in [("e3", 52.0), ("g3", 55.0), ("a4", 69.0), ("cs3", 49.0)] {
            let got =
                midi_onset(&onset(serde_json::json!({ "midiport": "x", "note": name }))).unwrap();
            assert_eq!(got.controls.note, Some(expected), "note name {name}");
        }
    }

    /// A number that arrived as text is still a number, and a word that is not
    /// a note is not one - it must not become a pitch by accident.
    #[test]
    fn a_numeric_string_is_a_number_and_a_non_note_is_neither() {
        let numeric =
            midi_onset(&onset(serde_json::json!({ "midiport": "x", "note": "60" }))).unwrap();
        assert_eq!(numeric.controls.note, Some(60.0));

        let nonsense =
            midi_onset(&onset(serde_json::json!({ "midiport": "x", "note": "bd" }))).unwrap();
        assert_eq!(
            nonsense.controls.note, None,
            "a sound name is not a pitch and must plan no note"
        );
    }

    /// `n` is the sample index, not a pitch. `s("bd:3").midi(port)` sets `n`
    /// through the sample-variant syntax without the score ever naming it, so
    /// reading it as a note turned every drum line into note-ons at the sample
    /// index.
    #[test]
    fn n_is_a_sample_index_and_never_a_note() {
        let variant = midi_onset(&onset(
            serde_json::json!({ "midiport": "x", "s": "bd", "n": 3 }),
        ))
        .unwrap();
        assert_eq!(
            variant.controls.note, None,
            "a sample variant became a note number"
        );
        // A real note still wins, alongside an `n` that selects the sample.
        let both = midi_onset(&onset(
            serde_json::json!({ "midiport": "x", "n": 3, "note": 60 }),
        ))
        .unwrap();
        assert_eq!(both.controls.note, Some(60.0));
    }

    /// The whole reason [`MidiClock`] exists: consecutive haps are scheduled in
    /// DIFFERENT passes, and a note-off sits only a millisecond before the next
    /// note-on. Re-deriving the wall-clock mapping each pass pinned every
    /// message to that pass's jitter, so a pass that ran a millisecond early
    /// put the note-on FIRST and the synth cut the note it had just started.
    #[test]
    fn one_anchor_keeps_session_order_across_passes() {
        let mut clock = MidiClock::new();
        // Two haps a beat apart, each scheduled in its own pass, with the
        // note-off of the first a millisecond before the note-on of the second.
        let first_off = clock.instant_for(0.0, 0.499);
        std::thread::sleep(Duration::from_millis(3));
        let second_on = clock.instant_for(0.05, 0.5);
        assert!(
            first_off < second_on,
            "the note-off must still precede the next note-on"
        );
    }

    /// Order has to survive many passes, not just two, and a gap the score
    /// wrote has to stay that gap on the wire however unevenly the passes fall.
    #[test]
    fn spacing_survives_a_jittery_run_of_passes() {
        let mut clock = MidiClock::new();
        let started = Instant::now();
        let mut previous: Option<Instant> = None;
        for step in 0..40 {
            // Uneven work between passes, which is what a producer step is.
            std::thread::sleep(Duration::from_micros(if step % 3 == 0 {
                2200
            } else {
                200
            }));
            // The session clock tracks wall time, as a device clock does.
            let now = started.elapsed().as_secs_f64();
            // Each pass hands over a hap 50ms further into the score.
            let due = clock.instant_for(now, f64::from(step) * 0.05);
            if let Some(previous) = previous {
                let gap = due.duration_since(previous);
                assert!(
                    (gap.as_secs_f64() - 0.05).abs() < 0.0005,
                    "pass {step} moved a 50ms gap to {:.4}s",
                    gap.as_secs_f64()
                );
            }
            previous = Some(due);
        }
    }

    /// The correction must not move the anchor further than the note-off
    /// margin between the passes that queue a pair. A step would send a
    /// later-planned note-on before a note-off that is already queued.
    #[test]
    fn correcting_a_drifting_clock_never_reorders_a_note_pair() {
        let mut clock = MidiClock::new();
        let started = Instant::now();
        let mut previous_on: Option<Instant> = None;
        // A session clock running 2% fast - far past any real crystal - so the
        // mapping is pulled hard for the whole run.
        for step in 0..300 {
            std::thread::sleep(Duration::from_micros(200));
            let now = started.elapsed().as_secs_f64() * 1.02;
            // One hap per pass, 50ms apart, as a sequence actually lands: the
            // note-off of this hap a millisecond before its own note-on, and
            // every hap strictly after the last.
            let onset = 0.5 + f64::from(step) * 0.05;
            let off = clock.instant_for(now, onset - 0.001);
            let on = clock.instant_for(now, onset);
            assert!(
                off < on,
                "pass {step}: the note-off was not before its note-on"
            );
            if let Some(previous_on) = previous_on {
                assert!(
                    previous_on < off,
                    "pass {step}: the previous note-on overtook this note-off"
                );
            }
            previous_on = Some(on);
        }
    }

    /// A note-off is stamped once, at its onset. Slew that accumulates over
    /// the whole note can put the next note-on before that off.
    /// [`NoteOrdering`] must keep the off first.
    #[test]
    fn slew_absorbed_over_a_whole_note_never_reorders_the_next_note_on() {
        let mut clock = MidiClock::new();
        let mut ordering = NoteOrdering::new();
        let started = Instant::now();
        // A session clock running 2% fast - far past any real crystal, and
        // past the slew's own 1000ppm rate, so the anchor is pulled back at
        // the full rate for the whole note.
        let session = |started: &Instant| started.elapsed().as_secs_f64() * 1.02;
        let controls = rustel_midi::MidiControls {
            note: Some(60.0),
            ..Default::default()
        };
        let gate = 2.2;

        // The long note, stamped whole on its own pass at the very start.
        let base = clock.instant_for(session(&started), 0.0);
        let first =
            ordering.stamp_batch("slew-order", 0.0, base, &rustel_midi::plan(&controls, gate));
        let off_due = first
            .iter()
            .find(|(_, message)| message.ends_sound())
            .map(|(due, _)| *due)
            .expect("a planned note has a note-off");

        // The producer keeps stamping other passes while the note sounds.
        while session(&started) < gate {
            std::thread::sleep(Duration::from_millis(2));
            let now = session(&started);
            clock.instant_for(now, now + 0.05);
        }

        // The same key's next onset, stamped whole on a later pass. Without
        // the ordering guard this lands ~1.2ms BEFORE the queued off: 2%
        // drift over 2.2s buys ~2.2ms of backward slew, more than the 1ms
        // lead the off was given.
        let base = clock.instant_for(session(&started), gate);
        let second =
            ordering.stamp_batch("slew-order", gate, base, &rustel_midi::plan(&controls, 0.2));
        let on_due = second[0].0;
        assert!(
            off_due <= on_due,
            "the queued note-off ({off_due:?}) sorts AFTER the next note-on \
         ({on_due:?}) by {:?}: accumulated slew moved the new stamp \
         earlier than an off queued {} seconds ago",
            off_due - on_due,
            gate,
        );

        // And the heap agrees: queued in stamp order, the wire sees the
        // note-off first.
        let capture = rustel_midi::CapturePort::new();
        let sender = MidiSender::with_port(Box::new(capture.clone()), "slew-order".into());
        assert!(sender.send_batch_at(&first));
        assert!(sender.send_batch_at(&second));
        // Three, not four: the long note's own note-on is two seconds past
        // due by the time this queues it, and the sender drops a note-on
        // that late rather than sound a note nobody asked for.
        wait_for_capture(&capture, 3);
        let payloads = capture
            .messages()
            .into_iter()
            .map(|(_, payload)| payload)
            .collect::<Vec<_>>();
        let wire_off = payloads
            .iter()
            .position(|payload| payload.as_slice() == [0x80, 60, 0])
            .expect("the note-off reached the wire");
        let wire_on = payloads
            .iter()
            .position(|payload| payload[0] == 0x90 && payload[1] == 60)
            .expect("the note-on reached the wire");
        assert!(
            wire_off < wire_on,
            "the sender emitted the note-on before the queued note-off: {payloads:?}"
        );
    }

    /// A planned note of `gate` seconds on `key`, and nothing else.
    fn note(key: f64, gate: f64) -> Vec<(f64, rustel_midi::MidiMessage)> {
        rustel_midi::plan(
            &MidiControls {
                note: Some(key),
                ..Default::default()
            },
            gate,
        )
    }

    /// How an onset was stamped before [`NoteOrdering`] existed: its base
    /// plus each planned offset.
    fn natural_stamps(
        base: Instant,
        planned: &[(f64, rustel_midi::MidiMessage)],
    ) -> Vec<(Instant, rustel_midi::MidiMessage)> {
        planned
            .iter()
            .map(|(offset, message)| {
                (
                    base + Duration::from_secs_f64(offset.clamp(0.0, 3600.0)),
                    *message,
                )
            })
            .collect()
    }

    /// The instant a stamped batch queues its note-on for.
    fn on_due(batch: &[(Instant, rustel_midi::MidiMessage)]) -> Instant {
        batch
            .iter()
            .find(|(_, message)| matches!(note_key(message), Some((_, _, true))))
            .map(|(due, _)| *due)
            .expect("a planned note has a note-on")
    }

    /// The instant a stamped batch queues its note-off for.
    fn off_due(batch: &[(Instant, rustel_midi::MidiMessage)]) -> Instant {
        batch
            .iter()
            .find(|(_, message)| message.ends_sound())
            .map(|(due, _)| *due)
            .expect("a planned note has a note-off")
    }

    /// [`NoteOrdering`] moves only a note-on that the score placed at or
    /// after a queued off but the slew stamped before it. The note-on moves
    /// onto the latest such off, with the messages sent with it, and its own
    /// note-off keeps the clock's stamp. A retrigger inside a long gate,
    /// another key, and another port keep their natural instants.
    #[test]
    fn the_note_ordering_moves_only_what_the_slew_inverted() {
        let mut ordering = NoteOrdering::new();
        let base = Instant::now() + Duration::from_secs(30);
        // The mapping as the slew has left it, `pulled_ms` behind the score.
        let at = |secs: f64, pulled_ms: u64| {
            base + Duration::from_secs_f64(secs) - Duration::from_millis(pulled_ms)
        };

        // A long note, with nothing recorded yet: exactly its natural stamps.
        let long_plan = note(60.0, 4.0);
        let long = ordering.stamp_batch("port", 0.0, base, &long_plan);
        assert_eq!(long, natural_stamps(base, &long_plan));
        let long_off = off_due(&long);

        // A retrigger the score placed BEFORE that off keeps its natural
        // instant: the long note's off cutting it is the wire behaviour the
        // score asked for. It queues a second off on the key, due before the
        // long note's.
        let retrigger_plan = note(60.0, 0.5);
        let retrigger = ordering.stamp_batch("port", 3.0, at(3.0, 1), &retrigger_plan);
        assert_eq!(retrigger, natural_stamps(at(3.0, 1), &retrigger_plan));
        assert!(off_due(&retrigger) < long_off);

        // The next onset is stamped 3ms early, 2ms past the off's lead. Its
        // note-on moves onto the later-due off (the long note's), together
        // with its program change and bend. Its own off keeps its stamp.
        let next_plan = rustel_midi::plan(
            &MidiControls {
                note: Some(60.0),
                prog_num: Some(5.0),
                midibend: Some(0.25),
                ..Default::default()
            },
            0.5,
        );
        let next = ordering.stamp_batch("port", 4.0, at(4.0, 3), &next_plan);
        let unmoved = natural_stamps(at(4.0, 3), &next_plan);
        assert_eq!(
            on_due(&next),
            long_off,
            "an inverted note-on was not moved onto the latest queued note-off"
        );
        for (due, message) in &next {
            if !message.ends_sound() {
                assert_eq!(*due, long_off, "{message:?} did not move with its note-on");
            }
        }
        assert_eq!(
            off_due(&next),
            off_due(&unmoved),
            "the moved onset's note-off left the clock's timeline"
        );
        let mut wire_order = unmoved.clone();
        wire_order.sort_by_key(|(due, _)| *due);
        assert_eq!(
            next.iter().map(|(_, message)| *message).collect::<Vec<_>>(),
            wire_order
                .iter()
                .map(|(_, message)| *message)
                .collect::<Vec<_>>(),
            "the moved onset no longer goes out in its own order"
        );

        // Moved further than its own gate - a whole second, past any real
        // slew - a note's off joins its note-on rather than precede it, and
        // still goes out after it.
        let squeezed = ordering.stamp_batch("port", 4.5, at(3.5, 0), &note(60.0, 0.2));
        assert_eq!(on_due(&squeezed), off_due(&next));
        assert_eq!(off_due(&squeezed), off_due(&next));
        assert!(matches!(note_key(&squeezed[0].1), Some((_, 60, true))));
        assert!(squeezed[1].1.ends_sound());

        // Another key, and the same key on another port, are never moved by
        // this port's records, even at the very instant key 60 was.
        let other_key = ordering.stamp_batch("port", 4.0, at(4.0, 3), &note(62.0, 0.5));
        assert_eq!(other_key, natural_stamps(at(4.0, 3), &note(62.0, 0.5)));
        let other_port = ordering.stamp_batch("port-2", 4.0, at(4.0, 3), &note(60.0, 0.5));
        assert_eq!(other_port, natural_stamps(at(4.0, 3), &note(60.0, 0.5)));

        // A controller-only batch records nothing and moves nothing.
        let cc = rustel_midi::plan(
            &MidiControls {
                ccn: Some(7.0),
                ccv: Some(0.5),
                ..Default::default()
            },
            0.5,
        );
        let tracked = ordering.tracked;
        let cc_batch = ordering.stamp_batch("port", 10.0, at(4.0, 3), &cc);
        assert_eq!(cc_batch, natural_stamps(at(4.0, 3), &cc));
        assert_eq!(ordering.tracked, tracked);
    }

    /// Under steady drift every repeat on one key inverts by the same
    /// amount. Each note-on moves by its own inversion at most, and each
    /// note-off keeps the clock's stamp, so the moves do not accumulate.
    #[test]
    fn a_legato_line_under_steady_drift_never_accumulates_its_moves() {
        let mut ordering = NoteOrdering::new();
        // 10s notes at 200ppm: each successor is stamped 2ms early, a
        // millisecond past the note-off's lead.
        let gate = 10.0;
        let pulled_per_note = Duration::from_millis(2);
        let start = Instant::now() + Duration::from_secs(30);
        let planned = note(60.0, gate);
        let mut previous: Option<(Instant, Instant)> = None;
        for step in 0..6u32 {
            let target = f64::from(step) * gate;
            let base = start + Duration::from_secs_f64(target) - pulled_per_note * step;
            let batch = ordering.stamp_batch("legato", target, base, &planned);
            let (on, off) = (on_due(&batch), off_due(&batch));
            let natural_off = off_due(&natural_stamps(base, &planned));
            if let Some((previous_natural_off, previous_off)) = previous {
                let inversion = previous_natural_off.saturating_duration_since(base);
                assert!(
                    inversion > Duration::ZERO,
                    "step {step}: the drift did not invert the pair"
                );
                assert!(
                    on - base <= inversion,
                    "step {step}: the note-on moved {:?} for its own {inversion:?} inversion",
                    on - base
                );
                assert!(
                    on >= previous_off,
                    "step {step}: the note-on precedes the previous note-off by {:?}",
                    previous_off - on
                );
            }
            assert_eq!(
                off, natural_off,
                "step {step}: the note-off left the clock's timeline"
            );
            previous = Some((natural_off, off));
        }
    }

    /// A note-on must follow each off that the score placed before it. An
    /// overlapping longer note has a later off, which the note-on need not
    /// follow.
    #[test]
    fn a_note_on_follows_every_off_the_score_placed_before_it() {
        let mut ordering = NoteOrdering::new();
        let start = Instant::now() + Duration::from_secs(30);
        let at = |secs: f64| start + Duration::from_secs_f64(secs);

        // A 2s note, and a longer one on the same key sounding across its end.
        let ending = ordering.stamp_batch("overlap", 0.0, at(0.0), &note(60.0, 2.0));
        let ending_off = off_due(&ending);
        let overlapping = ordering.stamp_batch("overlap", 1.5, at(1.5), &note(60.0, 8.5));
        assert!(off_due(&overlapping) > ending_off);

        // The next note straight after the 2s one, stamped 2ms early, as a
        // full-rate slew leaves it after those 2s.
        let next_plan = note(60.0, 0.5);
        let base = at(2.0) - Duration::from_millis(2);
        let next = ordering.stamp_batch("overlap", 2.0, base, &next_plan);
        assert_eq!(
            on_due(&next),
            ending_off,
            "the note-on was not moved onto the off of the note it follows"
        );
        assert_eq!(off_due(&next), off_due(&natural_stamps(base, &next_plan)));
    }

    /// With a zero note-off lead, the off and the next note-on are one score
    /// instant, but `0.1 + 0.2` rounds one ulp past `0.3`. The note-on must
    /// still follow the off.
    #[test]
    fn a_zero_lead_legato_pair_is_ordered_across_float_rounding() {
        let mut ordering = NoteOrdering::new();
        let controls = MidiControls {
            note: Some(60.0),
            note_offset_ms: Some(0.0),
            ..Default::default()
        };
        let start = Instant::now() + Duration::from_secs(30);
        let first_plan = rustel_midi::plan(&controls, 0.2);
        assert!(
            0.1 + first_plan[1].0 > 0.3,
            "the premise: the off's session time rounds past the next onset's"
        );
        let first = ordering.stamp_batch(
            "zero-lead",
            0.1,
            start + Duration::from_millis(100),
            &first_plan,
        );

        // The next onset, stamped a hair early by the slew: with no lead at
        // all, any pull inverts the pair.
        let base = start + Duration::from_millis(300) - Duration::from_micros(50);
        let next = ordering.stamp_batch("zero-lead", 0.3, base, &rustel_midi::plan(&controls, 0.2));
        assert_eq!(on_due(&next), off_due(&first));
    }

    /// A score the slew never inverts keeps every natural stamp in the same
    /// order: repeats, a chord, a retrigger inside a gate, controllers, and a
    /// mapping pulled back by less than the lead or pushed later.
    #[test]
    fn a_score_the_slew_never_inverted_keeps_every_natural_stamp() {
        let mut ordering = NoteOrdering::new();
        let start = Instant::now() + Duration::from_secs(30);
        let with_controls = |key: f64| MidiControls {
            note: Some(key),
            prog_num: Some(3.0),
            ccn: Some(7.0),
            ccv: Some(0.5),
            midibend: Some(-0.5),
            mapped_ccs: vec![(74, 0.25)],
            ..Default::default()
        };
        // (target, gate, key, microseconds the mapping is pulled back; a
        // negative pull pushes it later)
        let onsets: [(f64, f64, f64, i64); 9] = [
            (0.0, 0.5, 60.0, 0),
            (0.5, 0.5, 60.0, 0),
            (0.5, 1.0, 64.0, 0),
            (1.0, 0.25, 60.0, 500),
            (1.1, 0.1, 60.0, 500),
            (1.25, 2.0, 60.0, 900),
            (1.5, 0.5, 64.0, -300),
            (3.25, 0.5, 60.0, -1_000),
            (3.75, 0.5, 60.0, -1_000),
        ];
        for (target, gate, key, pulled_us) in onsets {
            let natural = start + Duration::from_secs_f64(target);
            let base = if pulled_us >= 0 {
                natural - Duration::from_micros(pulled_us.unsigned_abs())
            } else {
                natural + Duration::from_micros(pulled_us.unsigned_abs())
            };
            let planned = rustel_midi::plan(&with_controls(key), gate);
            assert_eq!(
                ordering.stamp_batch("ordinary", target, base, &planned),
                natural_stamps(base, &planned),
                "the onset at {target} did not keep its natural stamps"
            );
        }
    }

    /// The anchor is rebased onto `now` on every call, so the `Instant`
    /// offset stays at one scheduling lookahead and does not saturate at
    /// `MAX_OFFSET_SECS`. The test places the clock deep into a set: a
    /// fast-forward without matching wall time is a discontinuity and would
    /// re-anchor.
    #[test]
    fn a_long_set_does_not_saturate_the_offset_clamp() {
        let mut clock = MidiClock::new();
        // Four hours in, well past MAX_OFFSET_SECS.
        let now = 4.0 * 3600.0;
        let before = Instant::now();
        let first = clock.instant_for(now, now + 0.10);
        let second = clock.instant_for(now, now + 0.35);

        let apart = second.duration_since(first).as_secs_f64();
        assert!(
            (apart - 0.25).abs() < 0.001,
            "a 250ms gap four hours into a set became {apart:.6}s"
        );
        // And it is due a tenth of a second out, not an hour.
        let lead = first.duration_since(before).as_secs_f64();
        assert!(
            lead < 1.0,
            "a target 100ms out mapped {lead:.1}s out - the clamp saturated"
        );
    }

    /// A clock that jumps (a recycled device, a restarted transport) gets a
    /// new anchor. Without it, every later message is due at a wrong time.
    #[test]
    fn a_jumped_clock_is_re_anchored_rather_than_followed_off_a_cliff() {
        let mut clock = MidiClock::new();
        let before = clock.instant_for(0.0, 0.0);
        // The session clock leaps a minute with no wall time passing.
        let after = clock.instant_for(60.0, 60.0);
        let apart = after.max(before) - after.min(before);
        assert!(
            apart < Duration::from_secs(1),
            "a jumped clock was followed instead of re-anchored: {apart:?}"
        );
    }

    /// A `target_time` behind the anchor is normal - an onset can be due by the
    /// time its pass hands it over - and must produce an instant in the past,
    /// which `send_at` sends at once, not a panic on `Instant` underflow.
    #[test]
    fn a_target_already_past_yields_an_earlier_instant_not_a_panic() {
        let mut clock = MidiClock::new();
        let anchor = clock.instant_for(10.0, 10.0);
        let past = clock.instant_for(10.0, 9.5);
        assert!(past < anchor);
    }

    /// The half of the unreadable-port fix that had no test: it must report,
    /// name the score's own vocabulary, and never reach a device.
    #[test]
    fn an_unreadable_port_name_reports_once_and_is_never_opened() {
        let mut outputs = MidiOutputs::new();
        let Err(first) = outputs.sender(rustel_core::UNREADABLE_MIDI_PORT) else {
            panic!("an unreadable port name must report");
        };
        assert!(
            first.contains(".midi("),
            "the report must be in the score's terms, got: {first}"
        );
        assert!(
            matches!(outputs.sender(rustel_core::UNREADABLE_MIDI_PORT), Ok(None)),
            "the second onset must not report again"
        );
    }

    #[test]
    fn a_port_that_cannot_be_opened_is_remembered_rather_than_retried() {
        let attempts = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let opener_attempts = Arc::clone(&attempts);
        let mut outputs = MidiOutputs::with_opener(Arc::new(move |_| {
            opener_attempts.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            Err("missing test port".into())
        }));
        let due = Instant::now() + Duration::from_secs(1);
        assert!(
            outputs
                .submit_batch_for_generation_at(
                    1,
                    "missing",
                    0,
                    wire_batch(due, rustel_midi::MidiMessage::clock()),
                )
                .expect("bounded async admission")
        );
        let errors = poll_until(&mut outputs, |outputs| {
            !outputs.opening.contains_key("missing")
        });
        assert_eq!(errors, ["missing test port"]);
        assert!(outputs.poll().is_empty(), "open error repeated");
        assert_eq!(attempts.load(std::sync::atomic::Ordering::Relaxed), 1);

        assert!(
            !outputs
                .submit_batch_for_generation_at(
                    1,
                    "missing",
                    0,
                    wire_batch(due, rustel_midi::MidiMessage::clock()),
                )
                .expect("backoff refusal")
        );
        assert_eq!(attempts.load(std::sync::atomic::Ordering::Relaxed), 1);
    }

    #[test]
    fn retained_old_names_cannot_bypass_the_new_generations_port_cap() {
        let mut outputs = MidiOutputs::new();
        outputs.open = (0..MAX_MIDI_OUTPUT_PORTS)
            .map(|index| {
                let name = format!("port-{index}");
                MidiOutputEntry {
                    name: name.clone(),
                    sender: Some(MidiSender::with_port(
                        Box::new(rustel_midi::CapturePort::new()),
                        name,
                    )),
                    generations: std::collections::BTreeSet::from([1]),
                    published: false,
                    retain_until_frame: 0,
                    retry_at: Instant::now(),
                    failure_reported: false,
                    attempts: 1,
                }
            })
            .collect();
        outputs.sync_generations(1, 2);
        for index in 0..MAX_MIDI_OUTPUT_PORTS {
            assert!(
                outputs
                    .sender_for(2, &format!("port-{index}"))
                    .unwrap()
                    .is_some()
            );
        }
        let Err(message) = outputs.sender_for(2, "generation-two-overflow") else {
            panic!("the seventeenth generation-two name bypassed the cap");
        };
        assert!(message.contains("at most"), "{message}");
        assert_eq!(outputs.open.len(), MAX_MIDI_OUTPUT_PORTS);
        assert_eq!(
            outputs
                .open
                .iter()
                .filter(|entry| entry.generations.contains(&2))
                .count(),
            MAX_MIDI_OUTPUT_PORTS
        );

        outputs.sync_generations(2, 3);
        for index in 0..MAX_MIDI_OUTPUT_PORTS {
            let _ = outputs.sender_for(3, &format!("port-{index}"));
        }
        assert!(outputs.sender_for(3, "generation-three-overflow").is_err());
        assert_eq!(outputs.open.len(), MAX_MIDI_OUTPUT_PORTS);
    }

    #[test]
    fn cross_cutover_output_retention_has_a_global_cap() {
        let mut outputs = MidiOutputs::new();
        outputs.open = (0..MAX_MIDI_OUTPUTS_ACROSS_CUTOVERS)
            .map(|index| MidiOutputEntry {
                name: format!("retained-{index}"),
                sender: None,
                generations: std::collections::BTreeSet::from([index as u64 + 10]),
                published: false,
                retain_until_frame: 100,
                retry_at: Instant::now(),
                failure_reported: true,
                attempts: 1,
            })
            .collect();

        let Err(message) = outputs.sender_for(1, "overflow") else {
            panic!("the cross-cutover cap was bypassed");
        };
        assert!(
            message.contains("retained across a live cutover"),
            "{message}"
        );
        assert!(outputs.sender_for(1, "overflow-again").unwrap().is_none());

        outputs.sync_generations_at(1, 1, 100);
        assert!(outputs.open.is_empty());
        assert!(outputs.sender_for(1, "recovered").unwrap().is_none());
        assert_eq!(outputs.open.len(), 1);
    }

    #[test]
    fn output_sender_survives_until_takeover_and_is_reused_by_the_new_generation() {
        let sender =
            MidiSender::with_port(Box::new(rustel_midi::CapturePort::new()), "shared".into());
        let mut outputs = MidiOutputs::new();
        outputs.open.push(MidiOutputEntry {
            name: "shared".into(),
            sender: Some(sender),
            generations: std::collections::BTreeSet::from([1]),
            published: false,
            retain_until_frame: 0,
            retry_at: Instant::now(),
            failure_reported: false,
            attempts: 1,
        });
        let original = outputs.open[0].sender.as_ref().unwrap() as *const MidiSender;
        outputs.begin_generation_transition(1, 2, 100);
        assert_eq!(outputs.open[0].retain_until_frame, 100);
        assert!(outputs.sender_for(2, "shared").unwrap().is_some());
        let reused = outputs.open[0].sender.as_ref().unwrap() as *const MidiSender;
        assert_eq!(reused, original, "the same port was closed and reopened");

        outputs.sync_generations_at(2, 2, 99);
        assert_eq!(outputs.open.len(), 1);
        outputs.sync_generations_at(2, 2, 100);
        assert_eq!(outputs.open.len(), 1, "new generation still uses the port");
    }

    #[test]
    fn pre_poll_route_reservation_reuses_same_sender_at_the_exact_boundary() {
        let capture = rustel_midi::CapturePort::new();
        let sender = MidiSender::with_port(Box::new(capture.clone()), "reserved-shared".into());
        let mut outputs = MidiOutputs::new();
        outputs.open.push(MidiOutputEntry {
            name: "reserved-shared".into(),
            sender: Some(sender),
            generations: [1].into(),
            published: false,
            retain_until_frame: 0,
            retry_at: Instant::now(),
            failure_reported: false,
            attempts: 1,
        });

        outputs.begin_generation_transition(1, 2, 100);
        assert!(
            outputs
                .reserve_generation_port(2, "reserved-shared")
                .expect("route reservation")
        );
        assert!(outputs.poll_at(2, 2, 100).is_empty());
        assert!(outputs.open[0].sender.is_some());
        assert_eq!(outputs.open[0].attempts, 1, "same-name sender was reopened");
        assert!(capture.messages().is_empty(), "reuse emitted shutdown CCs");

        assert!(
            outputs
                .submit_batch_for_generation_at(
                    2,
                    "reserved-shared",
                    100,
                    wire_batch(
                        Instant::now() + Duration::from_millis(10),
                        rustel_midi::MidiMessage::cont(),
                    ),
                )
                .expect("replacement batch")
        );
        wait_for_capture(&capture, 1);
        assert_eq!(capture.messages()[0].1, vec![0xfb]);
    }

    #[test]
    fn reused_output_prunes_the_old_future_batch_before_admitting_the_new_one() {
        let capture = rustel_midi::CapturePort::new();
        let sender = MidiSender::with_port(Box::new(capture.clone()), "shared-prune".into());
        let due = Instant::now() + Duration::from_millis(30);
        assert!(sender.send_batch_for_generation_at(
            1,
            100,
            &[(due, rustel_midi::MidiMessage::start())],
        ));
        let mut outputs = MidiOutputs::new();
        outputs.open.push(MidiOutputEntry {
            name: "shared-prune".into(),
            sender: Some(sender),
            generations: std::collections::BTreeSet::from([1]),
            published: false,
            retain_until_frame: 0,
            retry_at: Instant::now(),
            failure_reported: false,
            attempts: 1,
        });

        outputs.begin_generation_transition(1, 2, 100);
        let sender = outputs.sender_for(2, "shared-prune").unwrap().unwrap();
        assert!(sender.send_batch_for_generation_at(
            2,
            100,
            &[(due, rustel_midi::MidiMessage::cont())],
        ));
        // Wait for the expected message, not for a fixed time. The sleep
        // after it is the grace period in which a second message would show
        // that the old batch was not pruned.
        wait_for_capture(&capture, 1);
        std::thread::sleep(Duration::from_millis(100));

        let payloads = capture
            .messages()
            .into_iter()
            .map(|(_, payload)| payload)
            .collect::<Vec<_>>();
        assert_eq!(payloads, vec![vec![0xfb]]);
    }

    /// Outputs whose port `name` is open over `capture` for generation 1.
    fn open_over(name: &str, capture: &rustel_midi::CapturePort) -> MidiOutputs {
        let mut outputs = MidiOutputs::new();
        open_port(&mut outputs, name, capture);
        outputs
    }

    /// Open the port `name` of `outputs` over `capture` for generation 1.
    fn open_port(outputs: &mut MidiOutputs, name: &str, capture: &rustel_midi::CapturePort) {
        outputs.open.push(MidiOutputEntry {
            name: name.into(),
            sender: Some(MidiSender::with_port(
                Box::new(capture.clone()),
                name.into(),
            )),
            generations: [1].into(),
            published: false,
            retain_until_frame: 0,
            retry_at: Instant::now(),
            failure_reported: false,
            attempts: 1,
        });
    }

    /// One note on `channel`: its note-on due at `on` and its note-off at `off`.
    fn note_batch(
        channel: u8,
        note: u8,
        on: Instant,
        off: Instant,
    ) -> Vec<(Instant, rustel_midi::MidiMessage)> {
        vec![
            (on, rustel_midi::MidiMessage::note_on(channel, note, 100)),
            (off, rustel_midi::MidiMessage::note_off(channel, note)),
        ]
    }

    fn payloads(capture: &rustel_midi::CapturePort) -> Vec<Vec<u8>> {
        capture
            .messages()
            .into_iter()
            .map(|(_, payload)| payload)
            .collect()
    }

    /// A control requery can publish after its takeover frame. The port has
    /// then sent the outgoing note of an onset between the two. The incoming
    /// copy is left out, and the note keeps its one note-off.
    #[test]
    fn a_late_takeover_leaves_out_the_copy_of_a_note_that_sounds() {
        let capture = rustel_midi::CapturePort::new();
        let mut outputs = open_over("late-requery", &capture);
        let now = Instant::now();
        let note = note_batch(1, 60, now, now + Duration::from_millis(250));
        assert!(
            outputs
                .submit_batch_for_generation_at(1, "late-requery", 100, note.clone())
                .expect("outgoing note")
        );
        wait_for_capture(&capture, 1);

        outputs.take_over_generation_from(1, 2, 100, 100, rustel_audio::TakeoverCut::None);
        assert!(
            outputs
                .submit_batch_for_generation_at(2, "late-requery", 100, note)
                .expect("incoming copy"),
            "the onset sounds, so it counts as accepted"
        );
        // Wait for the note-off, then for a second note-on or note-off.
        wait_for_capture(&capture, 2);
        std::thread::sleep(Duration::from_millis(100));
        assert_eq!(payloads(&capture), [vec![0x90, 60, 100], vec![0x80, 60, 0]]);
    }

    /// A kept note stands in for one batch: the same note on the same port
    /// and channel at the same onset frame. Every other batch of the
    /// incoming generation goes out, a second copy included.
    #[test]
    fn a_kept_note_stands_in_for_one_copy_of_its_own_onset() {
        let capture = rustel_midi::CapturePort::new();
        let other = rustel_midi::CapturePort::new();
        let mut outputs = open_over("kept-note", &capture);
        open_port(&mut outputs, "other-port", &other);
        let now = Instant::now();
        let later = now + Duration::from_secs(30);
        let submit = |outputs: &mut MidiOutputs, generation, port, frame, channel, note| {
            assert!(
                outputs
                    .submit_batch_for_generation_at(
                        generation,
                        port,
                        frame,
                        note_batch(channel, note, now, later),
                    )
                    .expect("batch")
            );
        };
        submit(&mut outputs, 1, "kept-note", 100, 1, 60);
        wait_for_capture(&capture, 1);
        outputs.take_over_generation_from(1, 2, 100, 100, rustel_audio::TakeoverCut::None);

        submit(&mut outputs, 2, "other-port", 100, 1, 60);
        submit(&mut outputs, 2, "kept-note", 100, 1, 62);
        submit(&mut outputs, 2, "kept-note", 101, 1, 60);
        submit(&mut outputs, 2, "kept-note", 100, 2, 60);
        submit(&mut outputs, 2, "kept-note", 100, 1, 60);
        submit(&mut outputs, 2, "kept-note", 100, 1, 60);
        wait_for_capture(&capture, 5);
        wait_for_capture(&other, 1);
        std::thread::sleep(Duration::from_millis(100));
        assert_eq!(
            payloads(&capture),
            [
                vec![0x90, 60, 100],
                vec![0x90, 62, 100],
                vec![0x90, 60, 100],
                vec![0x91, 60, 100],
                vec![0x90, 60, 100],
            ],
            "only the first copy of the kept note is left out"
        );
        assert_eq!(payloads(&other), [vec![0x90, 60, 100]]);
    }

    /// A restart is not a copy of the score it cuts, and a flip without a
    /// takeover frame starts a new lifetime. Each sends all its onsets, as
    /// does a takeover that names no cut. A recycle forgets the kept notes.
    #[test]
    fn a_cut_or_a_new_lifetime_sends_every_onset() {
        use rustel_audio::TakeoverCut;
        type TakeOver = fn(&mut MidiOutputs);
        let takeovers: [(&str, TakeOver); 4] = [
            ("a cut at the flip", |outputs| {
                outputs.take_over_generation_from(1, 2, 100, 100, TakeoverCut::AtFlip)
            }),
            ("a cut at the takeover", |outputs| {
                outputs.take_over_generation_from(1, 2, 100, 100, TakeoverCut::AtTakeover)
            }),
            ("a flip without a takeover frame", |outputs| {
                outputs.take_over_generation_from(1, 2, 0, 0, TakeoverCut::None)
            }),
            ("a transition without a cut word", |outputs| {
                outputs.begin_generation_transition(1, 2, 100)
            }),
        ];
        for (name, take_over) in takeovers {
            let capture = rustel_midi::CapturePort::new();
            let mut outputs = open_over("restart", &capture);
            let now = Instant::now();
            let note = note_batch(1, 60, now, now + Duration::from_secs(30));
            assert!(
                outputs
                    .submit_batch_for_generation_at(1, "restart", 100, note.clone())
                    .expect("outgoing note")
            );
            wait_for_capture(&capture, 1);
            take_over(&mut outputs);
            assert!(
                outputs
                    .submit_batch_for_generation_at(2, "restart", 100, note)
                    .expect("incoming note")
            );
            wait_for_capture(&capture, 2);
            assert_eq!(
                payloads(&capture),
                [vec![0x90, 60, 100], vec![0x90, 60, 100]],
                "{name}"
            );
        }

        let capture = rustel_midi::CapturePort::new();
        let mut outputs = open_over("recycle", &capture);
        let now = Instant::now();
        let note = note_batch(1, 60, now, now + Duration::from_secs(30));
        assert!(
            outputs
                .submit_batch_for_generation_at(1, "recycle", 100, note)
                .expect("outgoing note")
        );
        wait_for_capture(&capture, 1);
        outputs.take_over_generation_from(1, 2, 100, 100, TakeoverCut::None);
        assert_eq!(outputs.kept_notes.len(), 1, "test premise");
        outputs.reset_after_audio_recycle();
        assert!(outputs.kept_notes.is_empty());
    }

    #[test]
    fn published_output_tracks_and_prunes_score_generations() {
        let capture = rustel_midi::CapturePort::new();
        let sender = MidiSender::with_port(Box::new(capture.clone()), "published-prune".into());
        let due = Instant::now() + Duration::from_millis(30);
        assert!(sender.send_batch_for_generation_at(
            1,
            100,
            &[(due, rustel_midi::MidiMessage::start())],
        ));
        let mut outputs = MidiOutputs::new();
        outputs.open.push(MidiOutputEntry {
            name: "published-prune".into(),
            sender: Some(sender),
            generations: std::collections::BTreeSet::from([1]),
            published: true,
            retain_until_frame: 0,
            retry_at: Instant::now(),
            failure_reported: false,
            attempts: 1,
        });

        outputs.begin_generation_transition(1, 2, 100);
        assert!(outputs.open[0].generations.is_empty());
        outputs.sync_generations_at(2, 2, 100);
        assert_eq!(outputs.open.len(), 1, "published port was retired");
        let sender = outputs.sender_for(2, "published-prune").unwrap().unwrap();
        assert!(sender.send_batch_for_generation_at(
            2,
            100,
            &[(due, rustel_midi::MidiMessage::cont())],
        ));
        assert_eq!(outputs.open[0].generations, [2].into());
        // As above: wait for the one message that is meant to arrive, then
        // keep the grace period in which a second one would be the failure.
        wait_for_capture(&capture, 1);
        std::thread::sleep(Duration::from_millis(100));

        let payloads = capture
            .messages()
            .into_iter()
            .map(|(_, payload)| payload)
            .collect::<Vec<_>>();
        assert_eq!(payloads, vec![vec![0xfb]]);
    }

    #[test]
    fn audio_recycle_hard_resets_score_queues_and_preserves_only_published_ports() {
        let published_capture = rustel_midi::CapturePort::new();
        let score_capture = rustel_midi::CapturePort::new();
        let published = MidiSender::with_port(
            Box::new(published_capture.clone()),
            "published-recycle".into(),
        );
        let score = MidiSender::with_port(Box::new(score_capture.clone()), "score-recycle".into());
        let old_due = Instant::now() + Duration::from_secs(60);
        assert!(published.send_batch_for_generation_at(
            7,
            48_000,
            &[(old_due, rustel_midi::MidiMessage::start())],
        ));
        assert!(score.send_batch_for_generation_at(
            7,
            48_000,
            &[(old_due, rustel_midi::MidiMessage::start())],
        ));

        let mut outputs = MidiOutputs::new();
        outputs.open.push(MidiOutputEntry {
            name: "published-recycle".into(),
            sender: Some(published),
            generations: std::collections::BTreeSet::from([7]),
            published: true,
            retain_until_frame: 96_000,
            retry_at: Instant::now(),
            failure_reported: false,
            attempts: 1,
        });
        outputs.open.push(MidiOutputEntry {
            name: "score-recycle".into(),
            sender: Some(score),
            generations: std::collections::BTreeSet::from([7]),
            published: false,
            retain_until_frame: 96_000,
            retry_at: Instant::now(),
            failure_reported: false,
            attempts: 1,
        });
        outputs.limit_reported.insert(7);
        outputs.invalid_name_reported.insert(7);
        outputs.retained_limit_reported = true;

        outputs.reset_after_audio_recycle();

        assert_eq!(outputs.open.len(), 1);
        assert!(outputs.open[0].published);
        assert!(outputs.open[0].generations.is_empty());
        assert_eq!(outputs.open[0].retain_until_frame, 0);
        assert!(outputs.limit_reported.is_empty());
        assert!(outputs.invalid_name_reported.is_empty());
        assert!(!outputs.retained_limit_reported);

        let sender = outputs
            .sender_for(8, "published-recycle")
            .expect("published sender")
            .expect("open published sender");
        assert!(sender.send_batch_for_generation_at(
            8,
            44_100,
            &[(
                Instant::now() + Duration::from_millis(10),
                rustel_midi::MidiMessage::cont(),
            )],
        ));
        assert_eq!(outputs.open[0].generations, [8].into());

        let deadline = Instant::now() + Duration::from_secs(1);
        while published_capture.messages().len() < 33 && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(1));
        }
        let published_payloads = published_capture
            .messages()
            .into_iter()
            .map(|(_, payload)| payload)
            .collect::<Vec<_>>();
        assert!(
            published_payloads.iter().any(|payload| payload == &[0xfb]),
            "new-generation work did not reach the retained published port"
        );
        assert!(
            !published_payloads.iter().any(|payload| payload == &[0xfa]),
            "old-frame work survived the recycle reset"
        );

        let score_payloads = score_capture
            .messages()
            .into_iter()
            .map(|(_, payload)| payload)
            .collect::<Vec<_>>();
        assert_eq!(score_payloads.len(), 32, "retired port was not silenced");
        assert!(
            !score_payloads.iter().any(|payload| payload == &[0xfa]),
            "retired score port sent stale work"
        );
    }

    #[test]
    fn old_only_sender_retires_at_not_before_the_takeover() {
        let sender =
            MidiSender::with_port(Box::new(rustel_midi::CapturePort::new()), "old-only".into());
        let mut outputs = MidiOutputs::new();
        outputs.open.push(MidiOutputEntry {
            name: "old-only".into(),
            sender: Some(sender),
            generations: std::collections::BTreeSet::from([1]),
            published: false,
            retain_until_frame: 0,
            retry_at: Instant::now(),
            failure_reported: false,
            attempts: 1,
        });

        outputs.begin_generation_transition(1, 2, 100);
        outputs.sync_generations_at(2, 2, 99);
        assert_eq!(outputs.open.len(), 1);
        outputs.sync_generations_at(2, 2, 100);
        assert!(outputs.open.is_empty());
    }

    #[test]
    fn queued_old_messages_are_silenced_and_retired_at_takeover() {
        let capture = rustel_midi::CapturePort::new();
        let sender = MidiSender::with_port(Box::new(capture.clone()), "old-queued".into());
        let due = Instant::now() + Duration::from_secs(60);
        assert!(sender.send_batch_at(&[(due, rustel_midi::MidiMessage::clock())]));
        let mut outputs = MidiOutputs::new();
        outputs.open.push(MidiOutputEntry {
            name: "old-queued".into(),
            sender: Some(sender),
            generations: std::collections::BTreeSet::from([1]),
            published: false,
            retain_until_frame: 0,
            retry_at: Instant::now(),
            failure_reported: false,
            attempts: 1,
        });

        outputs.begin_generation_transition(1, 2, 100);
        outputs.sync_generations_at(2, 2, 99);
        assert_eq!(outputs.open.len(), 1);
        outputs.sync_generations_at(2, 2, 100);
        assert!(outputs.open.is_empty());

        wait_for_capture(&capture, 32);
        let sent = capture.messages();
        assert_eq!(
            sent.len(),
            32,
            "retirement did not emit exact emergency silence"
        );
        assert!(sent.iter().all(|(_, bytes)| {
            bytes.len() == 3
                && bytes[0] & 0xf0 == 0xb0
                && matches!(bytes[1], 120 | 123)
                && bytes[2] == 0
        }));
        assert!(
            sent.iter().all(|(_, bytes)| bytes.as_slice() != [0xf8]),
            "the retired generation's future clock escaped: {sent:?}"
        );
    }

    #[test]
    fn midi_onset_frames_match_audio_dust_and_ceil_rules() {
        assert_eq!(onset_frame_at(0.8, 48_000), 38_400);
        assert_eq!(onset_frame_at(38_400.0000000005 / 48_000.0, 48_000), 38_400);
        assert_eq!(onset_frame_at(38_400.25 / 48_000.0, 48_000), 38_401);
        assert_eq!(onset_frame_at(f64::NAN, 48_000), u64::MAX);
        assert_eq!(onset_frame_at(-1.0, 48_000), u64::MAX);
    }

    #[test]
    fn rapid_transitions_extend_one_deadline_without_growing_membership() {
        let sender =
            MidiSender::with_port(Box::new(rustel_midi::CapturePort::new()), "shared".into());
        let mut outputs = MidiOutputs::new();
        outputs.open.push(MidiOutputEntry {
            name: "shared".into(),
            sender: Some(sender),
            generations: std::collections::BTreeSet::from([1]),
            published: false,
            retain_until_frame: 0,
            retry_at: Instant::now(),
            failure_reported: false,
            attempts: 1,
        });

        for generation in 2..=128u64 {
            outputs.begin_generation_transition(generation - 1, generation, 100 + generation);
            assert!(outputs.sender_for(generation, "shared").unwrap().is_some());
            assert_eq!(outputs.open.len(), 1);
            assert_eq!(outputs.open[0].generations.len(), 1);
        }
        assert_eq!(outputs.open[0].retain_until_frame, 228);
    }

    #[test]
    fn disjoint_old_ports_keep_their_own_first_takeover_deadline() {
        let entry = |name: &str, generation: u64| MidiOutputEntry {
            name: name.into(),
            sender: Some(MidiSender::with_port(
                Box::new(rustel_midi::CapturePort::new()),
                name.into(),
            )),
            generations: std::collections::BTreeSet::from([generation]),
            published: false,
            retain_until_frame: 0,
            retry_at: Instant::now(),
            failure_reported: false,
            attempts: 1,
        };
        let mut outputs = MidiOutputs::new();
        outputs.open.push(entry("a", 1));
        outputs.begin_generation_transition(1, 2, 100);
        outputs.open.push(entry("b", 2));
        outputs.begin_generation_transition(2, 3, 200);
        outputs.open.push(entry("c", 3));

        assert_eq!(outputs.open[0].retain_until_frame, 100);
        assert_eq!(outputs.open[1].retain_until_frame, 200);
        outputs.sync_generations_at(3, 3, 100);
        assert_eq!(
            outputs
                .open
                .iter()
                .map(|entry| entry.name.as_str())
                .collect::<Vec<_>>(),
            ["b", "c"]
        );
    }

    #[test]
    fn retired_sender_reports_remain_monotonic() {
        let saturated = |name: &str| {
            let sender =
                MidiSender::with_port(Box::new(rustel_midi::CapturePort::new()), name.to_string());
            let due = Instant::now() + Duration::from_secs(60);
            let batch =
                vec![(due, rustel_midi::MidiMessage::clock()); rustel_midi::MAX_QUEUED_MESSAGES];
            assert!(sender.send_batch_at(&batch));
            assert!(!sender.send_batch_at(&[(due, rustel_midi::MidiMessage::clock())]));
            sender
        };
        let mut outputs = MidiOutputs::new();
        outputs.open.push(MidiOutputEntry {
            name: "old".into(),
            sender: Some(saturated("old")),
            generations: std::collections::BTreeSet::from([1]),
            published: false,
            retain_until_frame: 0,
            retry_at: Instant::now(),
            failure_reported: false,
            attempts: 1,
        });
        assert_eq!(outputs.report().refused_full, 1);

        outputs.begin_generation_transition(1, 2, 100);
        outputs.sync_generations_at(2, 2, 100);
        assert_eq!(outputs.report().refused_full, 1);

        outputs.open.push(MidiOutputEntry {
            name: "new".into(),
            sender: Some(saturated("new")),
            generations: std::collections::BTreeSet::from([2]),
            published: false,
            retain_until_frame: 0,
            retry_at: Instant::now(),
            failure_reported: false,
            attempts: 1,
        });
        assert_eq!(outputs.report().refused_full, 2);
    }

    #[test]
    fn promoting_an_existing_name_cannot_exceed_the_published_port_cap() {
        let mut outputs = MidiOutputs::new();
        outputs.open = (0..MAX_MIDI_OUTPUT_PORTS)
            .map(|index| MidiOutputEntry {
                name: format!("published-{index}"),
                sender: None,
                generations: std::collections::BTreeSet::new(),
                published: true,
                retain_until_frame: 0,
                retry_at: Instant::now(),
                failure_reported: false,
                attempts: 0,
            })
            .collect();
        outputs.open.push(MidiOutputEntry {
            name: "score-port".into(),
            sender: None,
            generations: std::collections::BTreeSet::from([1]),
            published: false,
            retain_until_frame: 0,
            retry_at: Instant::now() + MIDI_OUTPUT_RETRY_BACKOFF,
            failure_reported: true,
            attempts: 1,
        });

        let Err(message) = outputs.publish_at_startup("score-port") else {
            panic!("a seventeenth published port was accepted");
        };
        assert!(message.contains("at most"), "{message}");
        assert!(!outputs.open.last().unwrap().published);
        assert_eq!(
            outputs.open.iter().filter(|entry| entry.published).count(),
            MAX_MIDI_OUTPUT_PORTS
        );

        assert!(outputs.publish_at_startup("published-0").is_ok());
    }

    #[test]
    fn oversized_output_selectors_are_not_retained_or_repeatedly_reported() {
        let mut outputs = MidiOutputs::new();
        let oversized = "x".repeat(MAX_MIDI_PORT_NAME_BYTES + 1);
        let Err(message) = outputs.sender(&oversized) else {
            panic!("oversized selector was accepted");
        };
        assert!(message.contains("at most"), "{message}");
        assert!(outputs.sender(&oversized).unwrap().is_none());
        assert!(outputs.open.is_empty());
    }

    #[test]
    fn asynchronous_open_never_blocks_admission_and_flushes_first_batches_in_order() {
        let capture = rustel_midi::CapturePort::new();
        let opened_capture = capture.clone();
        let (entered_tx, entered_rx) = std::sync::mpsc::sync_channel(1);
        let (release_tx, release_rx) = std::sync::mpsc::sync_channel(0);
        let release_rx = Arc::new(Mutex::new(release_rx));
        let opener_release = Arc::clone(&release_rx);
        let mut outputs = MidiOutputs::with_opener(Arc::new(move |name| {
            entered_tx
                .send(name.to_string())
                .map_err(|_| "open observer disappeared".to_string())?;
            opener_release
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .recv_timeout(Duration::from_secs(2))
                .map_err(|_| "test opener was not released".to_string())?;
            Ok(MidiSender::with_port(
                Box::new(opened_capture.clone()),
                name.to_string(),
            ))
        }));
        let due = Instant::now() + Duration::from_millis(40);
        let started = Instant::now();
        assert!(
            outputs
                .submit_batch_for_generation_at(
                    1,
                    "slow-open",
                    0,
                    wire_batch(due, rustel_midi::MidiMessage::start()),
                )
                .expect("first admission")
        );
        assert!(
            started.elapsed() < Duration::from_millis(50),
            "producer waited for a platform open"
        );
        assert!(
            outputs
                .submit_batch_for_generation_at(
                    1,
                    "slow-open",
                    1,
                    wire_batch(due, rustel_midi::MidiMessage::cont()),
                )
                .expect("second admission")
        );
        assert_eq!(
            entered_rx
                .recv_timeout(Duration::from_secs(2))
                .expect("opener never started"),
            "slow-open"
        );
        release_tx.send(()).expect("opener abandoned its request");
        poll_until(&mut outputs, |outputs| {
            outputs
                .open
                .iter()
                .any(|entry| entry.name == "slow-open" && entry.sender.is_some())
        });
        wait_for_capture(&capture, 2);
        let payloads = capture
            .messages()
            .into_iter()
            .map(|(_, payload)| payload)
            .collect::<Vec<_>>();
        assert_eq!(payloads, vec![vec![0xfa], vec![0xfb]]);
    }

    #[test]
    fn delayed_reused_open_prunes_old_pending_at_the_exact_takeover_before_flush() {
        let capture = rustel_midi::CapturePort::new();
        let opened_capture = capture.clone();
        let (entered_tx, entered_rx) = std::sync::mpsc::sync_channel(1);
        let (release_tx, release_rx) = std::sync::mpsc::sync_channel(0);
        let release_rx = Arc::new(Mutex::new(release_rx));
        let opener_release = Arc::clone(&release_rx);
        let mut outputs = MidiOutputs::with_opener(Arc::new(move |name| {
            entered_tx
                .send(())
                .map_err(|_| "open observer disappeared".to_string())?;
            opener_release
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .recv_timeout(Duration::from_secs(2))
                .map_err(|_| "test opener was not released".to_string())?;
            Ok(MidiSender::with_port(
                Box::new(opened_capture.clone()),
                name.to_string(),
            ))
        }));
        let due = Instant::now() + Duration::from_millis(80);
        assert!(
            outputs
                .submit_batch_for_generation_at(
                    1,
                    "shared-open",
                    99,
                    wire_batch(due, rustel_midi::MidiMessage::start()),
                )
                .expect("old admission")
        );
        entered_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("opener never blocked");
        outputs.begin_generation_transition(1, 2, 100);
        assert!(
            outputs
                .submit_batch_for_generation_at(
                    2,
                    "shared-open",
                    100,
                    wire_batch(due, rustel_midi::MidiMessage::cont()),
                )
                .expect("replacement admission")
        );
        assert!(outputs.poll_at(2, 2, 99).is_empty());
        release_tx.send(()).expect("opener abandoned its request");

        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            assert!(outputs.poll_at(2, 2, 100).is_empty());
            if outputs
                .open
                .iter()
                .any(|entry| entry.name == "shared-open" && entry.sender.is_some())
            {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "completed open was not installed"
            );
            std::thread::sleep(Duration::from_millis(1));
        }
        wait_for_capture(&capture, 1);
        std::thread::sleep(Duration::from_millis(20));
        let payloads = capture
            .messages()
            .into_iter()
            .map(|(_, payload)| payload)
            .collect::<Vec<_>>();
        assert_eq!(payloads, vec![vec![0xfb]]);
    }

    #[test]
    fn a_later_transition_cannot_flush_an_earlier_expired_open_result() {
        let capture = rustel_midi::CapturePort::new();
        let opened_capture = capture.clone();
        let (entered_tx, entered_rx) = std::sync::mpsc::sync_channel(1);
        let (release_tx, release_rx) = std::sync::mpsc::sync_channel(0);
        let release_rx = Arc::new(Mutex::new(release_rx));
        let opener_release = Arc::clone(&release_rx);
        let mut outputs = MidiOutputs::with_opener(Arc::new(move |name| {
            entered_tx
                .send(())
                .map_err(|_| "open observer disappeared".to_string())?;
            opener_release
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .recv_timeout(Duration::from_secs(2))
                .map_err(|_| "test opener was not released".to_string())?;
            Ok(MidiSender::with_port(
                Box::new(opened_capture.clone()),
                name.to_string(),
            ))
        }));
        let due = Instant::now() + Duration::from_millis(80);
        assert!(
            outputs
                .submit_batch_for_generation_at(
                    1,
                    "transition-race",
                    99,
                    wire_batch(due, rustel_midi::MidiMessage::start()),
                )
                .expect("generation A admission")
        );
        entered_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("opener never blocked");
        outputs.begin_generation_transition(1, 2, 100);
        assert!(
            outputs
                .submit_batch_for_generation_at(
                    2,
                    "transition-race",
                    150,
                    wire_batch(due, rustel_midi::MidiMessage::cont()),
                )
                .expect("generation B admission")
        );
        assert!(outputs.poll_at(2, 2, 99).is_empty());
        release_tx.send(()).expect("opener abandoned its request");
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            if !outputs
                .manager
                .results
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .is_empty()
            {
                break;
            }
            assert!(Instant::now() < deadline, "open result never completed");
            std::thread::sleep(Duration::from_millis(1));
        }

        // The producer can cross from frame 99 to 101 while executing this
        // next transition. It has no current-frame argument and therefore must
        // leave result installation to the immediately-following poll.
        outputs.begin_generation_transition(2, 3, 200);
        assert!(outputs.open[0].sender.is_none());
        assert!(capture.messages().is_empty());
        assert!(outputs.poll_at(3, 3, 101).is_empty());
        wait_for_capture(&capture, 1);
        let payloads = capture
            .messages()
            .into_iter()
            .map(|(_, payload)| payload)
            .collect::<Vec<_>>();
        assert_eq!(payloads, vec![vec![0xfb]]);
    }

    #[test]
    fn canceled_queued_opens_do_not_delay_or_saturate_the_latest_score() {
        let calls = Arc::new(Mutex::new(Vec::<String>::new()));
        let opener_calls = Arc::clone(&calls);
        let (entered_tx, entered_rx) = std::sync::mpsc::sync_channel(1);
        let (release_tx, release_rx) = std::sync::mpsc::sync_channel(0);
        let release_rx = Arc::new(Mutex::new(release_rx));
        let opener_release = Arc::clone(&release_rx);
        let mut outputs = MidiOutputs::with_opener(Arc::new(move |name| {
            opener_calls
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .push(name.to_string());
            if name == "blocked" {
                entered_tx
                    .send(())
                    .map_err(|_| "open observer disappeared".to_string())?;
                opener_release
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .recv_timeout(Duration::from_secs(2))
                    .map_err(|_| "test opener was not released".to_string())?;
            }
            Ok(MidiSender::with_port(
                Box::new(rustel_midi::CapturePort::new()),
                name.to_string(),
            ))
        }));
        let due = Instant::now() + Duration::from_secs(1);
        assert!(
            outputs
                .submit_batch_for_generation_at(
                    1,
                    "blocked",
                    100,
                    wire_batch(due, rustel_midi::MidiMessage::clock()),
                )
                .expect("blocked admission")
        );
        entered_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("first open never blocked");
        for index in 0..(MAX_MIDI_OUTPUT_PORTS - 1) {
            assert!(
                outputs
                    .submit_batch_for_generation_at(
                        1,
                        &format!("stale-{index}"),
                        100,
                        wire_batch(due, rustel_midi::MidiMessage::clock()),
                    )
                    .expect("stale admission")
            );
        }
        outputs.begin_generation_transition(1, 2, 100);
        outputs.sync_generations_at(2, 2, 100);
        assert!(
            outputs
                .submit_batch_for_generation_at(
                    2,
                    "latest",
                    100,
                    wire_batch(due, rustel_midi::MidiMessage::clock()),
                )
                .expect("latest admission was falsely saturated")
        );
        release_tx.send(()).expect("blocked opener disappeared");
        poll_until(&mut outputs, |outputs| {
            outputs
                .open
                .iter()
                .any(|entry| entry.name == "latest" && entry.sender.is_some())
        });
        assert_eq!(
            *calls.lock().unwrap_or_else(|error| error.into_inner()),
            vec!["blocked".to_string(), "latest".to_string()]
        );
    }

    #[test]
    fn pending_open_caps_refuse_whole_batches_in_constant_accounting() {
        let (entered_tx, entered_rx) = std::sync::mpsc::sync_channel(1);
        let (release_tx, release_rx) = std::sync::mpsc::sync_channel(0);
        let release_rx = Arc::new(Mutex::new(release_rx));
        let opener_release = Arc::clone(&release_rx);
        let first = Arc::new(std::sync::atomic::AtomicBool::new(true));
        let opener_first = Arc::clone(&first);
        let mut outputs = MidiOutputs::with_opener(Arc::new(move |name| {
            if opener_first.swap(false, std::sync::atomic::Ordering::AcqRel) {
                entered_tx
                    .send(())
                    .map_err(|_| "open observer disappeared".to_string())?;
                opener_release
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .recv_timeout(Duration::from_secs(2))
                    .map_err(|_| "test opener was not released".to_string())?;
            }
            Ok(MidiSender::with_port(
                Box::new(rustel_midi::CapturePort::new()),
                name.to_string(),
            ))
        }));
        outputs.pending_per_port_limit = 2;
        outputs.pending_global_limit = 3;
        let due = Instant::now() + Duration::from_secs(1);
        assert!(
            outputs
                .submit_batch_for_generation_at(
                    1,
                    "a",
                    0,
                    vec![
                        (due, rustel_midi::MidiMessage::start()),
                        (due, rustel_midi::MidiMessage::cont()),
                    ],
                )
                .expect("first complete batch")
        );
        entered_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("opener never blocked");
        assert!(
            !outputs
                .submit_batch_for_generation_at(
                    1,
                    "a",
                    1,
                    wire_batch(due, rustel_midi::MidiMessage::clock()),
                )
                .expect("per-port refusal")
        );
        assert!(
            outputs
                .submit_batch_for_generation_at(
                    1,
                    "b",
                    2,
                    wire_batch(due, rustel_midi::MidiMessage::clock()),
                )
                .expect("global final slot")
        );
        assert!(
            !outputs
                .submit_batch_for_generation_at(
                    1,
                    "c",
                    3,
                    wire_batch(due, rustel_midi::MidiMessage::clock()),
                )
                .expect("global refusal")
        );
        assert_eq!(outputs.pending_messages, 3);
        assert_eq!(outputs.pending_by_port.get("a"), Some(&2));
        assert_eq!(outputs.report().refused_full, 2);
        release_tx.send(()).expect("opener disappeared");
    }

    #[test]
    fn opener_errors_are_utf8_bounded_counted_and_reported_once() {
        let mut outputs = MidiOutputs::with_opener(Arc::new(|_| Err("é".repeat(2_000))));
        assert!(
            outputs
                .submit_batch_for_generation_at(
                    1,
                    "bad",
                    0,
                    wire_batch(
                        Instant::now() + Duration::from_secs(1),
                        rustel_midi::MidiMessage::clock(),
                    ),
                )
                .expect("bounded admission")
        );
        let errors = poll_until(&mut outputs, |outputs| !outputs.opening.contains_key("bad"));
        assert_eq!(errors.len(), 1);
        assert!(errors[0].len() <= MAX_MIDI_ERROR_BYTES);
        assert!(errors[0].is_char_boundary(errors[0].len()));
        assert_eq!(outputs.report().errors_dropped, 1);
        assert!(outputs.poll().is_empty(), "bounded error repeated");
    }

    #[test]
    fn shutdown_waits_for_fast_emergency_silence_and_keeps_reports_monotonic() {
        let capture = rustel_midi::CapturePort::new();
        let sender = MidiSender::with_port(Box::new(capture.clone()), "shutdown-fast".into());
        assert!(sender.send_batch_for_generation_at(
            1,
            0,
            &[(
                Instant::now() + Duration::from_secs(60),
                rustel_midi::MidiMessage::note_on(1, 60, 100),
            )],
        ));
        let mut outputs = MidiOutputs::new();
        outputs.open.push(MidiOutputEntry {
            name: "shutdown-fast".into(),
            sender: Some(sender),
            generations: [1].into(),
            published: false,
            retain_until_frame: 0,
            retry_at: Instant::now(),
            failure_reported: false,
            attempts: 1,
        });
        assert!(outputs.shutdown(Duration::from_secs(1)));
        wait_for_capture(&capture, 32);
        assert_eq!(capture.messages().len(), 32);
        assert_eq!(outputs.report().sent, 32);
    }

    #[test]
    fn shutdown_deadline_does_not_join_a_wedged_sender_and_late_stats_survive() {
        let capture = rustel_midi::CapturePort::new();
        let (entered_tx, entered_rx) = std::sync::mpsc::sync_channel(0);
        let (release_tx, release_rx) = std::sync::mpsc::sync_channel(0);
        let sender = MidiSender::with_port(
            Box::new(BlockingFirstMidiPort {
                capture: capture.clone(),
                entered: Some(entered_tx),
                release: release_rx,
            }),
            "shutdown-blocked".into(),
        );
        assert!(sender.send_at(Instant::now(), rustel_midi::MidiMessage::clock()));
        entered_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("sender never wedged");
        let mut outputs = MidiOutputs::new();
        outputs.open.push(MidiOutputEntry {
            name: "shutdown-blocked".into(),
            sender: Some(sender),
            generations: [1].into(),
            published: false,
            retain_until_frame: 0,
            retry_at: Instant::now(),
            failure_reported: false,
            attempts: 1,
        });
        let before = outputs.report();
        let started = Instant::now();
        assert!(!outputs.shutdown(Duration::from_millis(20)));
        assert!(started.elapsed() < Duration::from_millis(100));
        let during = outputs.report();
        assert!(during.sent >= before.sent);
        release_tx.send(()).expect("blocked sender disappeared");
        wait_for_capture(&capture, 33);
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            outputs.reap_retiring();
            if outputs.retiring.is_empty() {
                break;
            }
            assert!(Instant::now() < deadline, "retiring sender never completed");
            std::thread::sleep(Duration::from_millis(1));
        }
        let after = outputs.report();
        assert!(after.sent >= during.sent);
        assert_eq!(after.sent, 33);
        let emergency = capture
            .messages()
            .into_iter()
            .filter(|(_, bytes)| {
                bytes.len() == 3 && bytes[0] & 0xf0 == 0xb0 && matches!(bytes[1], 120 | 123)
            })
            .count();
        assert_eq!(emergency, 32);
    }

    #[test]
    fn shutdown_does_not_wait_for_an_open_that_never_received_score_data() {
        let (entered_tx, entered_rx) = std::sync::mpsc::sync_channel(1);
        let (release_tx, release_rx) = std::sync::mpsc::sync_channel(0);
        let release_rx = Arc::new(Mutex::new(release_rx));
        let opener_release = Arc::clone(&release_rx);
        let mut outputs = MidiOutputs::with_opener(Arc::new(move |name| {
            entered_tx
                .send(())
                .map_err(|_| "open observer disappeared".to_string())?;
            opener_release
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .recv_timeout(Duration::from_secs(2))
                .map_err(|_| "test opener was not released".to_string())?;
            Ok(MidiSender::with_port(
                Box::new(rustel_midi::CapturePort::new()),
                name.to_string(),
            ))
        }));
        assert!(
            outputs
                .submit_batch_for_generation_at(
                    1,
                    "blocked-open-shutdown",
                    0,
                    wire_batch(
                        Instant::now() + Duration::from_secs(1),
                        rustel_midi::MidiMessage::clock(),
                    ),
                )
                .expect("pending open admission")
        );
        entered_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("opener never blocked");
        let started = Instant::now();
        assert!(outputs.shutdown(Duration::from_millis(20)));
        assert!(started.elapsed() < Duration::from_millis(100));
        release_tx.send(()).expect("opener disappeared");
    }

    #[test]
    fn legacy_sender_lookup_eventually_observes_the_async_result() {
        let mut outputs = MidiOutputs::with_opener(Arc::new(|name| {
            Ok(MidiSender::with_port(
                Box::new(rustel_midi::CapturePort::new()),
                name.to_string(),
            ))
        }));
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            if outputs.sender("legacy").expect("legacy lookup").is_some() {
                break;
            }
            assert!(Instant::now() < deadline, "legacy lookup stayed None");
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    #[test]
    fn a_missing_port_retries_with_backoff_not_on_every_generation() {
        let attempts = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let opener_attempts = Arc::clone(&attempts);
        let capture = rustel_midi::CapturePort::new();
        let opened_capture = capture.clone();
        let mut outputs = MidiOutputs::with_opener(Arc::new(move |name| {
            let attempt = opener_attempts.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            if attempt == 0 {
                Err("temporarily missing".into())
            } else {
                Ok(MidiSender::with_port(
                    Box::new(opened_capture.clone()),
                    name.to_string(),
                ))
            }
        }));
        let first_due = Instant::now() + Duration::from_secs(1);
        assert!(
            outputs
                .submit_batch_for_generation_at(
                    1,
                    "hotplug",
                    0,
                    wire_batch(first_due, rustel_midi::MidiMessage::start()),
                )
                .expect("first admission")
        );
        let errors = poll_until(&mut outputs, |outputs| {
            !outputs.opening.contains_key("hotplug")
        });
        assert_eq!(errors, ["temporarily missing"]);
        for generation in 1..=32 {
            assert!(
                outputs
                    .submit_batch_for_generation_at(
                        generation,
                        "hotplug",
                        0,
                        wire_batch(first_due, rustel_midi::MidiMessage::clock()),
                    )
                    .is_ok()
            );
        }
        assert_eq!(outputs.open[0].attempts, 1);

        outputs.retry_backoff = Duration::ZERO;
        outputs.open[0].retry_at = Instant::now() - Duration::from_millis(1);
        let due = Instant::now() + Duration::from_millis(20);
        assert!(
            outputs
                .submit_batch_for_generation_at(
                    33,
                    "hotplug",
                    0,
                    wire_batch(due, rustel_midi::MidiMessage::cont()),
                )
                .expect("retry admission")
        );
        let errors = poll_until(&mut outputs, |outputs| {
            outputs
                .open
                .iter()
                .any(|entry| entry.name == "hotplug" && entry.sender.is_some())
        });
        assert!(errors.is_empty());
        assert_eq!(outputs.open[0].attempts, 2);
        wait_for_capture(&capture, 1);
        assert_eq!(capture.messages()[0].1, vec![0xfb]);
    }

    #[test]
    fn restart_waits_for_same_port_connection_to_finish_closing() {
        use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

        let (entered_tx, entered_rx) = std::sync::mpsc::sync_channel(1);
        let (release_tx, release_rx) = std::sync::mpsc::sync_channel(1);
        let closed = Arc::new(AtomicBool::new(false));
        let first = MidiSender::with_port(
            Box::new(BlockingDropMidiPort {
                entered: entered_tx,
                release: release_rx,
                closed: Arc::clone(&closed),
            }),
            "Wavetable".into(),
        );
        let attempts = Arc::new(AtomicUsize::new(0));
        let capture = rustel_midi::CapturePort::new();
        let opener_attempts = Arc::clone(&attempts);
        let opener_closed = Arc::clone(&closed);
        let opener_capture = capture.clone();
        let mut outputs = MidiOutputs::with_opener(Arc::new(move |name| {
            opener_attempts.fetch_add(1, Ordering::Relaxed);
            if !opener_closed.load(Ordering::Acquire) {
                return Err("WinMM port still allocated".into());
            }
            Ok(MidiSender::with_port(
                Box::new(opener_capture.clone()),
                name.to_string(),
            ))
        }));
        outputs.open.push(MidiOutputEntry {
            name: "Wavetable".into(),
            sender: Some(first),
            generations: [1].into(),
            published: false,
            retain_until_frame: 0,
            retry_at: Instant::now(),
            failure_reported: false,
            attempts: 1,
        });

        outputs.reset_after_audio_recycle();
        entered_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("old MIDI connection did not start closing");
        assert!(
            outputs
                .submit_batch_for_generation_at(
                    2,
                    "Wavetable",
                    0,
                    wire_batch(
                        Instant::now() + Duration::from_secs(1),
                        rustel_midi::MidiMessage::clock(),
                    ),
                )
                .expect("new onset queues while port closes")
        );
        for _ in 0..5 {
            assert!(outputs.poll().is_empty());
        }
        assert_eq!(attempts.load(Ordering::Relaxed), 0);
        assert_eq!(outputs.pending_messages, 1);

        release_tx.send(()).expect("old connection disappeared");
        let errors = poll_until(&mut outputs, |outputs| {
            outputs
                .open
                .iter()
                .any(|entry| entry.name == "Wavetable" && entry.sender.is_some())
        });
        assert!(
            errors.is_empty(),
            "a restart must not report an allocated port"
        );
        assert_eq!(attempts.load(Ordering::Relaxed), 1);
        wait_for_capture(&capture, 1);
        assert_eq!(capture.messages()[0].1, vec![0xf8]);
    }

    #[test]
    fn canceled_in_flight_open_still_closes_before_reopening_same_port() {
        use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

        let (open_started_tx, open_started_rx) = std::sync::mpsc::sync_channel(1);
        let (open_release_tx, open_release_rx) = std::sync::mpsc::sync_channel(1);
        let (close_started_tx, close_started_rx) = std::sync::mpsc::sync_channel(1);
        let (close_release_tx, close_release_rx) = std::sync::mpsc::sync_channel(1);
        let open_release_rx = Mutex::new(open_release_rx);
        let close_release_rx = Mutex::new(Some(close_release_rx));
        let closed = Arc::new(AtomicBool::new(false));
        let attempts = Arc::new(AtomicUsize::new(0));
        let capture = rustel_midi::CapturePort::new();
        let opener_attempts = Arc::clone(&attempts);
        let opener_closed = Arc::clone(&closed);
        let opener_capture = capture.clone();
        let mut outputs = MidiOutputs::with_opener(Arc::new(move |name| {
            let attempt = opener_attempts.fetch_add(1, Ordering::Relaxed);
            if attempt == 0 {
                open_started_tx
                    .send(())
                    .expect("first opener lost its test");
                open_release_rx
                    .lock()
                    .unwrap_or_else(|error| error.into_inner())
                    .recv_timeout(Duration::from_secs(2))
                    .expect("first opener was not released");
                Ok(MidiSender::with_port(
                    Box::new(BlockingDropMidiPort {
                        entered: close_started_tx.clone(),
                        release: close_release_rx
                            .lock()
                            .unwrap_or_else(|error| error.into_inner())
                            .take()
                            .expect("first sender owns the close gate"),
                        closed: Arc::clone(&opener_closed),
                    }),
                    name.to_string(),
                ))
            } else if !opener_closed.load(Ordering::Acquire) {
                Err("WinMM port still allocated".into())
            } else {
                Ok(MidiSender::with_port(
                    Box::new(opener_capture.clone()),
                    name.to_string(),
                ))
            }
        }));
        assert!(
            outputs
                .submit_batch_for_generation_at(
                    1,
                    "Wavetable",
                    0,
                    wire_batch(
                        Instant::now() + Duration::from_secs(1),
                        rustel_midi::MidiMessage::clock(),
                    ),
                )
                .expect("first onset")
        );
        open_started_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("first opener did not start");
        outputs.reset_after_audio_recycle();
        assert!(
            outputs
                .submit_batch_for_generation_at(
                    2,
                    "Wavetable",
                    0,
                    wire_batch(
                        Instant::now() + Duration::from_secs(1),
                        rustel_midi::MidiMessage::clock(),
                    ),
                )
                .expect("restart onset")
        );
        open_release_tx.send(()).expect("first opener disappeared");
        close_started_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("canceled sender did not start closing");
        for _ in 0..5 {
            assert!(outputs.poll().is_empty());
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(attempts.load(Ordering::Relaxed), 1);

        close_release_tx
            .send(())
            .expect("old connection disappeared");
        let errors = poll_until(&mut outputs, |outputs| {
            outputs
                .open
                .iter()
                .any(|entry| entry.name == "Wavetable" && entry.sender.is_some())
        });
        assert!(errors.is_empty(), "stale open must retire before the retry");
        assert_eq!(attempts.load(Ordering::Relaxed), 2);
        wait_for_capture(&capture, 1);
        assert_eq!(capture.messages()[0].1, vec![0xf8]);
    }

    #[test]
    fn recovery_notice_survives_full_failure_queue() {
        let mut outputs = MidiOutputs::with_opener(Arc::new(|name| {
            Ok(MidiSender::with_port(
                Box::new(rustel_midi::CapturePort::new()),
                name.to_string(),
            ))
        }));
        outputs.queue_error("Wavetable".into(), "unavailable".into());
        assert!(matches!(
            outputs.poll_notices_at(0, 0, 0).as_slice(),
            [MidiOutputNotice::OpenFailed { port, .. }] if port == "Wavetable"
        ));
        for index in 0..MAX_PENDING_MIDI_ERRORS {
            outputs.queue_error(format!("port-{index}"), "unavailable".into());
        }
        outputs.queue_notice(MidiOutputNotice::Opened {
            port: "Wavetable".into(),
        });
        assert!(
            outputs
                .poll_notices_at(0, 0, 0)
                .contains(&MidiOutputNotice::Opened {
                    port: "Wavetable".into(),
                })
        );
        assert_eq!(outputs.report().errors_dropped, 1);
    }

    #[test]
    fn allocated_windows_port_error_explains_likely_causes() {
        let mut outputs = MidiOutputs::with_opener(Arc::new(|_| {
            panic!("this test only inspects queued errors")
        }));
        outputs.queue_error(
            "Wavetable".into(),
            "could not create Windows MM MIDI output port (MMSYSERR_ALLOCATED)".into(),
        );
        let notices = outputs.poll_notices_at(0, 0, 0);
        let MidiOutputNotice::OpenFailed { message, .. } = &notices[0] else {
            panic!("expected an open failure")
        };
        assert!(message.contains("may still be closing or be in use by another app"));
    }

    #[test]
    fn successful_opens_without_warnings_do_not_accumulate_notices() {
        let mut outputs = MidiOutputs::with_opener(Arc::new(|_| panic!("no platform open")));
        for index in 0..(MAX_PENDING_MIDI_ERRORS * 4) {
            outputs.queue_notice(MidiOutputNotice::Opened {
                port: format!("port-{index}"),
            });
            assert!(outputs.pending_notices.is_empty());
        }
        assert!(outputs.poll_notices_at(0, 0, 0).is_empty());
    }

    #[test]
    fn pending_recoveries_bound_notice_admission_until_polled() {
        let mut outputs = MidiOutputs::with_opener(Arc::new(|_| panic!("no platform open")));
        for index in 0..MAX_PENDING_MIDI_ERRORS {
            outputs.queue_error(format!("port-{index}"), "unavailable".into());
        }
        assert_eq!(
            outputs.poll_notices_at(0, 0, 0).len(),
            MAX_PENDING_MIDI_ERRORS
        );
        for index in 0..MAX_PENDING_MIDI_ERRORS {
            outputs.queue_notice(MidiOutputNotice::Opened {
                port: format!("port-{index}"),
            });
        }
        for index in MAX_PENDING_MIDI_ERRORS..(MAX_PENDING_MIDI_ERRORS * 4) {
            let port = format!("port-{index}");
            outputs.queue_error(port.clone(), "unavailable".into());
            outputs.queue_notice(MidiOutputNotice::Opened { port });
            assert_eq!(outputs.pending_notices.len(), MAX_PENDING_MIDI_ERRORS);
        }
        assert_eq!(
            outputs.report().errors_dropped,
            (MAX_PENDING_MIDI_ERRORS * 3) as u64
        );
        let notices = outputs.poll_notices_at(0, 0, 0);
        assert_eq!(notices.len(), MAX_PENDING_MIDI_ERRORS);
        for index in 0..MAX_PENDING_MIDI_ERRORS {
            assert!(notices.contains(&MidiOutputNotice::Opened {
                port: format!("port-{index}"),
            }));
        }
        outputs.queue_error("later-port".into(), "unavailable".into());
        assert!(matches!(
            outputs.poll_notices_at(0, 0, 0).as_slice(),
            [MidiOutputNotice::OpenFailed { port, .. }] if port == "later-port"
        ));
    }

    #[test]
    fn renewed_failure_supersedes_recovery_until_next_poll() {
        let mut outputs = MidiOutputs::with_opener(Arc::new(|_| panic!("no platform open")));
        outputs.queue_error("Wavetable".into(), "first failure".into());
        assert_eq!(outputs.poll_notices_at(0, 0, 0).len(), 1);
        outputs.queue_notice(MidiOutputNotice::Opened {
            port: "Wavetable".into(),
        });
        outputs.queue_error("Wavetable".into(), "second failure".into());
        assert_eq!(
            outputs.poll_notices_at(0, 0, 0),
            [MidiOutputNotice::OpenFailed {
                port: "Wavetable".into(),
                message: "second failure".into(),
            }]
        );
        outputs.queue_notice(MidiOutputNotice::Opened {
            port: "Wavetable".into(),
        });
        assert_eq!(
            outputs.poll_notices_at(0, 0, 0),
            [MidiOutputNotice::Opened {
                port: "Wavetable".into(),
            }]
        );
        outputs.queue_notice(MidiOutputNotice::Opened {
            port: "Wavetable".into(),
        });
        assert!(outputs.poll_notices_at(0, 0, 0).is_empty());
    }
}
