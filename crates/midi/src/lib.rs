/*
lib.rs - MIDI output for live patterns
Message planning adapted from Strudel packages/midi/midi.mjs.
Copyright (C) 2022 Strudel contributors

Rust implementation and additions:
Copyright (C) 2026 Rustel contributors

This program is free software: you can redistribute it and/or modify it under
the terms of the GNU Affero General Public License as published by the Free
Software Foundation, either version 3 of the License, or (at your option) any
later version.
*/

//! Timed MIDI output.
//!
//! Upstream uses Web MIDI, which takes a timestamp per message and does the
//! scheduling itself. That includes the note-off, which it queues at note-on
//! time. `midir` has no such facility: it sends each message immediately. So
//! [`MidiSender`] owns the scheduling: a time-ordered queue drained by one
//! thread that sleeps until the next message is due.
//!
//! Nothing here may end a set. A vanished port, a value out of range and a
//! full queue are each reported and dropped. Silence from one MIDI device is
//! less harmful than a stopped process.

pub mod input;

use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap, HashSet};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use midir::{MidiOutput, MidiOutputConnection};

/// What to say when there is no sequencer to ask.
#[cfg(target_os = "linux")]
pub(crate) const NO_SEQUENCER: &str =
    "no ALSA sequencer: /dev/snd/seq is absent, so this machine has no MIDI ports";

/// What to say when the sequencer is there but refuses this user.
#[cfg(target_os = "linux")]
pub(crate) const SEQUENCER_REFUSED: &str = "no ALSA sequencer access: this user may not open \
     /dev/snd/seq; join the audio group (sudo usermod -aG audio $USER), then log in again";

/// `Ok` where a `midir` client may be built, or why it may not.
///
/// `midir` reaches MIDI through ALSA here, and ALSA's sequencer is the
/// `/dev/snd/seq` character device. Where it is absent (no `snd-seq` module: a
/// container, a CI runner, WSL) or refuses this user (outside the `audio`
/// group), every client fails, `midir` says only that MIDI support could not
/// be initialized, and libasound writes its own account to stderr on every
/// attempt. Asking the filesystem first names the cause once, in words,
/// without calling into ALSA.
#[cfg(target_os = "linux")]
pub(crate) fn sequencer_ready() -> Result<(), &'static str> {
    const SEQUENCER: &std::ffi::CStr = c"/dev/snd/seq";
    // SAFETY: `SEQUENCER` is a static NUL-terminated string, and `access`
    // only reads it.
    let access = unsafe { libc::access(SEQUENCER.as_ptr(), libc::R_OK | libc::W_OK) };
    sequencer_verdict(if access == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    })
}

/// Every other platform reaches MIDI through its own API, which has no such
/// device.
#[cfg(not(target_os = "linux"))]
pub(crate) fn sequencer_ready() -> Result<(), &'static str> {
    Ok(())
}

/// [`sequencer_ready`]'s answer, given what `access(2)` said about opening
/// `/dev/snd/seq` for reading and writing. Any failure other than absence or
/// refusal is left for ALSA's own open to report.
#[cfg(target_os = "linux")]
fn sequencer_verdict(access: std::io::Result<()>) -> Result<(), &'static str> {
    match access.map_err(|error| error.kind()) {
        Err(std::io::ErrorKind::NotFound) => Err(NO_SEQUENCER),
        Err(std::io::ErrorKind::PermissionDenied) => Err(SEQUENCER_REFUSED),
        _ => Ok(()),
    }
}

/// A `midir` output client, or why there is not one.
pub(crate) fn open_output(name: &str) -> Result<MidiOutput, String> {
    sequencer_ready()?;
    MidiOutput::new(name).map_err(|error| error.to_string())
}

/// A `midir` input client, or why there is not one.
pub(crate) fn open_input(name: &str) -> Result<midir::MidiInput, String> {
    sequencer_ready()?;
    midir::MidiInput::new(name).map_err(|error| error.to_string())
}

/// Longest a queued message may sit before the sender gives up on it.
///
/// A message whose target has long passed is not worth sending: a note-on from
/// ten seconds ago arrives as a stuck note nobody asked for. Dropping it is the
/// same choice the audio path makes for stale events.
pub const MAX_QUEUE_LATENESS: Duration = Duration::from_millis(500);

/// Queue ceiling. Bounded so a runaway score cannot grow it without limit;
/// pushes past this are refused and counted, never blocked on.
pub const MAX_QUEUED_MESSAGES: usize = 8192;

/// Hard process-wide reservation for active and retiring sender workers.
///
/// A platform `send` is allowed to wedge forever. Sender destruction is
/// deliberately nonblocking, so a wedged worker can outlive its score; this
/// cap prevents device churn from translating that failure into unbounded
/// detached threads. It covers the runtime's 32 retained score ports plus 16
/// published ports with room for workers completing asynchronous retirement.
pub const MAX_MIDI_SENDER_WORKERS: usize = 64;

struct MidiWorkerBudget {
    live: AtomicUsize,
    limit: usize,
}

impl MidiWorkerBudget {
    const fn new(limit: usize) -> Self {
        Self {
            live: AtomicUsize::new(0),
            limit,
        }
    }

    fn reserve(self: &Arc<Self>) -> Option<MidiWorkerPermit> {
        let mut current = self.live.load(Ordering::Acquire);
        loop {
            if current >= self.limit {
                return None;
            }
            match self.live.compare_exchange_weak(
                current,
                current + 1,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => {
                    return Some(MidiWorkerPermit {
                        budget: Arc::clone(self),
                    });
                }
                Err(observed) => current = observed,
            }
        }
    }
}

struct MidiWorkerPermit {
    budget: Arc<MidiWorkerBudget>,
}

impl Drop for MidiWorkerPermit {
    fn drop(&mut self) {
        self.budget.live.fetch_sub(1, Ordering::AcqRel);
    }
}

static MIDI_WORKER_BUDGET: std::sync::LazyLock<Arc<MidiWorkerBudget>> =
    std::sync::LazyLock::new(|| Arc::new(MidiWorkerBudget::new(MAX_MIDI_SENDER_WORKERS)));

/// One MIDI message, already encoded.
///
/// Three bytes covers every channel-voice and realtime message we send. Sysex
/// is deliberately absent for now - it is variable length and would force an
/// allocation onto this path.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MidiMessage {
    bytes: [u8; 3],
    len: u8,
}

impl MidiMessage {
    pub fn as_slice(&self) -> &[u8] {
        &self.bytes[..usize::from(self.len)]
    }

    fn two(status: u8, a: u8) -> Self {
        Self {
            bytes: [status, a, 0],
            len: 2,
        }
    }

    fn three(status: u8, a: u8, b: u8) -> Self {
        Self {
            bytes: [status, a, b],
            len: 3,
        }
    }

    fn one(status: u8) -> Self {
        Self {
            bytes: [status, 0, 0],
            len: 1,
        }
    }

    /// `midichan` is 1-16 in the score, as upstream documents; the wire
    /// wants 0-15.
    fn status(kind: u8, channel: u8) -> u8 {
        kind | (channel.clamp(1, 16) - 1)
    }

    pub fn note_on(channel: u8, note: u8, velocity: u8) -> Self {
        Self::three(Self::status(0x90, channel), note & 0x7f, velocity & 0x7f)
    }

    /// Note-off as an explicit 0x80, not a zero-velocity note-on: some
    /// hardware distinguishes them, and strudel.cc sends a real
    /// note-off.
    pub fn note_off(channel: u8, note: u8) -> Self {
        Self::three(Self::status(0x80, channel), note & 0x7f, 0)
    }

    pub fn control_change(channel: u8, controller: u8, value: u8) -> Self {
        Self::three(Self::status(0xb0, channel), controller & 0x7f, value & 0x7f)
    }

    pub fn program_change(channel: u8, program: u8) -> Self {
        Self::two(Self::status(0xc0, channel), program & 0x7f)
    }

    pub fn channel_aftertouch(channel: u8, pressure: u8) -> Self {
        Self::two(Self::status(0xd0, channel), pressure & 0x7f)
    }

    /// 14-bit, centre 8192, LSB first - the wire order, which is the reverse
    /// of how the value reads.
    pub fn pitch_bend(channel: u8, value14: u16) -> Self {
        let clamped = value14.min(16383);
        Self::three(
            Self::status(0xe0, channel),
            (clamped & 0x7f) as u8,
            ((clamped >> 7) & 0x7f) as u8,
        )
    }

    /// Whether this message stops sound rather than starting it.
    ///
    /// The lateness bound exists so a note-on from ten seconds ago does not
    /// arrive as a note nobody asked for. Applied to a note-off it does the
    /// opposite of its purpose: the note-on was already sent, so dropping its
    /// note-off leaves the note sounding until the end of the set. A late
    /// note-off is a slightly long note; a dropped one is a drone.
    ///
    /// All-notes-off (123) and all-sound-off (120) are here for the same
    /// reason - they are the last thing that can rescue a stuck note.
    pub fn ends_sound(&self) -> bool {
        match self.bytes[0] & 0xf0 {
            0x80 => true,
            0xb0 => matches!(self.bytes[1], 120 | 123),
            _ => false,
        }
    }

    fn starts_sound(&self) -> bool {
        self.bytes[0] & 0xf0 == 0x90 && self.bytes[2] != 0
    }

    fn is_note_off(&self) -> bool {
        self.bytes[0] & 0xf0 == 0x80
    }

    pub fn clock() -> Self {
        Self::one(0xf8)
    }
    pub fn start() -> Self {
        Self::one(0xfa)
    }
    pub fn cont() -> Self {
        Self::one(0xfb)
    }
    pub fn stop() -> Self {
        Self::one(0xfc)
    }
}

/// `gain * velocity` in 0..1 to a 1-127 velocity.
///
/// Upstream multiplies gain into velocity before sending
/// (`velocity = gain * velocity`), with defaults gain 1 and velocity 0.9.
/// Zero maps to 1, not 0: a zero-velocity note-on IS a note-off on the wire,
/// which would swallow the note instead of playing it quietly.
pub fn velocity_byte(value: f64) -> u8 {
    if !value.is_finite() {
        return 1;
    }
    ((value.clamp(0.0, 1.0) * 127.0).round() as u8).max(1)
}

/// A 0..1 control value to 0-127: `round(ccv * 127)`.
pub fn unit_byte(value: f64) -> u8 {
    if !value.is_finite() {
        return 0;
    }
    (value.clamp(0.0, 1.0) * 127.0).round() as u8
}

/// A -1..1 bend to 14-bit, centre 8192.
pub fn bend_value(value: f64) -> u16 {
    if !value.is_finite() {
        return 8192;
    }
    let normalised = (value.clamp(-1.0, 1.0) + 1.0) / 2.0;
    (normalised * 16383.0).round() as u16
}

/// A note number, which the score may give as a float.
pub fn note_byte(value: f64) -> Option<u8> {
    if !value.is_finite() || !(0.0..=127.0).contains(&value.round()) {
        return None;
    }
    Some(value.round() as u8)
}

/// Upstream's defaults, from the `midiConfig` literal.
pub const DEFAULT_VELOCITY: f64 = 0.9;
pub const DEFAULT_GAIN: f64 = 1.0;
pub const DEFAULT_CHANNEL: u8 = 1;

/// Upstream shortens every note by `noteOffsetMs` (1ms; 10 on Firefox) so the
/// note-off lands before the next note-on. Without it a clock drift of a
/// fraction of a millisecond lets the next note-on overtake the previous
/// note-off, and the new note is cut off instead of the old one.
pub const NOTE_OFFSET_SECS: f64 = 0.001;

/// The MIDI-relevant controls of one hap, already pulled out of the value.
///
/// A plain struct so this crate needs nothing from the runtime's JSON types
/// and [`plan`] stays a pure function the tests can drive directly.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct MidiControls {
    pub note: Option<f64>,
    pub gain: Option<f64>,
    pub velocity: Option<f64>,
    pub midichan: Option<f64>,
    pub ccn: Option<f64>,
    pub ccv: Option<f64>,
    pub prog_num: Option<f64>,
    pub midibend: Option<f64>,
    pub miditouch: Option<f64>,
    pub midicmd: Option<String>,
    /// Suppress note-on/off while retaining controller/program/bend output.
    pub is_controller: bool,
    /// Per-connection note-off lead in milliseconds. Invalid values fall back
    /// to [`NOTE_OFFSET_SECS`].
    pub note_offset_ms: Option<f64>,
    /// CC messages a midimap derived from named controls: `(ccn, ccv in 0..1)`.
    ///
    /// Produced by the bridge from `midimaps({...})` registrations - this crate
    /// only encodes them, exactly as it encodes an explicit `ccn`/`ccv` pair.
    pub mapped_ccs: Vec<(u8, f64)>,
}

/// Visit the messages one hap should emit, in wire order.
///
/// Both [`plan`] and [`has_output`] use this path so the replacement cutover
/// check cannot drift from the messages the sender will actually receive.
fn for_each_planned_message(
    controls: &MidiControls,
    duration_secs: f64,
    mut emit: impl FnMut(f64, MidiMessage),
) -> bool {
    let emitted = std::cell::Cell::new(false);
    let mut emit = |offset_secs, message| {
        emitted.set(true);
        emit(offset_secs, message);
    };
    let channel = controls
        .midichan
        .filter(|value| value.is_finite())
        .map(|value| value.round().clamp(1.0, 16.0) as u8)
        .unwrap_or(DEFAULT_CHANNEL);

    // `trigger()` calls `forEach(mapCC(...))` before its ordinary note,
    // program and explicit-CC sends. Preserve that order: if a map and an
    // explicit `ccn` target the same controller, the explicit value must be
    // the final value observed by the device.
    for (ccn, ccv) in &controls.mapped_ccs {
        emit(
            0.0,
            MidiMessage::control_change(channel, *ccn & 0x7f, unit_byte(*ccv)),
        );
    }

    if let Some(note) = controls.note.and_then(note_byte)
        && !controls.is_controller
    {
        let gain = controls.gain.unwrap_or(DEFAULT_GAIN);
        let velocity = controls.velocity.unwrap_or(DEFAULT_VELOCITY);
        // `velocity = gain * velocity`, matching strudel.cc exactly.
        let byte = velocity_byte(gain * velocity);
        // `offset = Math.min(noteOffsetMs, hapDuration / 2)`: a very short
        // note keeps half its length rather than going negative.
        let gate = duration_secs.max(0.0);
        let configured_offset = controls
            .note_offset_ms
            .filter(|value| value.is_finite() && *value >= 0.0)
            .map(|milliseconds| milliseconds / 1_000.0)
            .unwrap_or(NOTE_OFFSET_SECS);
        let offset = configured_offset.min(gate / 2.0);
        emit(0.0, MidiMessage::note_on(channel, note, byte));
        emit(gate - offset, MidiMessage::note_off(channel, note));
    }

    if let Some(program) = controls
        .prog_num
        .filter(|value| value.is_finite() && (0.0..=127.0).contains(value))
    {
        emit(
            0.0,
            MidiMessage::program_change(channel, program.round() as u8),
        );
    }

    // Upstream requires BOTH before sending anything.
    if let (Some(ccn), Some(ccv)) = (controls.ccn, controls.ccv)
        && ccn.is_finite()
        && ccn.fract() == 0.0
        && (0.0..=127.0).contains(&ccn)
    {
        emit(
            0.0,
            MidiMessage::control_change(channel, ccn as u8, unit_byte(ccv)),
        );
    }

    if let Some(bend) = controls.midibend.filter(|value| value.is_finite()) {
        emit(0.0, MidiMessage::pitch_bend(channel, bend_value(bend)));
    }

    if let Some(touch) = controls.miditouch.filter(|value| value.is_finite()) {
        emit(
            0.0,
            MidiMessage::channel_aftertouch(channel, unit_byte(touch)),
        );
    }

    if let Some(command) = controls.midicmd.as_deref() {
        let message = match command {
            "clock" | "midiClock" => Some(MidiMessage::clock()),
            "start" => Some(MidiMessage::start()),
            "stop" => Some(MidiMessage::stop()),
            "continue" => Some(MidiMessage::cont()),
            _ => None,
        };
        if let Some(message) = message {
            emit(0.0, message);
        }
    }

    emitted.get()
}

/// Whether this hap will produce at least one valid MIDI message.
///
/// This is the allocation-free counterpart to [`plan`]. In particular, a
/// `midiport` with no MIDI controls, a half-specified CC, an invalid note, or
/// an unknown transport command is not output merely because it names a port.
pub fn has_output(controls: &MidiControls) -> bool {
    for_each_planned_message(controls, 0.0, |_, _| {})
}

/// Translate one hap into the messages it should emit, as offsets in seconds
/// from the onset.
///
/// Order matches strudel.cc: mapped controls, note, program
/// change, explicit control change, pitch bend, aftertouch, then transport.
/// Anything absent from the
/// hap emits nothing, so `.midi()` on a pattern that only sets `ccv`/`ccn`
/// sends control changes and no notes - which is how a score drives a synth's
/// knobs without playing it.
pub fn plan(controls: &MidiControls, duration_secs: f64) -> Vec<(f64, MidiMessage)> {
    let mut out = Vec::new();
    for_each_planned_message(controls, duration_secs, |offset, message| {
        out.push((offset, message));
    });
    out
}

#[derive(Debug, Default, Clone, Copy)]
pub struct MidiReport {
    pub sent: u64,
    /// Messages dropped because their target time had already passed by more
    /// than [`MAX_QUEUE_LATENESS`].
    pub dropped_late: u64,
    /// Messages refused because the queue was at [`MAX_QUEUED_MESSAGES`].
    pub refused_full: u64,
    /// Writes the port rejected - usually a device that went away.
    pub send_errors: u64,
    /// Bounded lifecycle diagnostics discarded or truncated before delivery.
    /// Sender-only reports leave this at zero; the runtime bridge aggregates
    /// asynchronous opener observability into the same monotonic snapshot.
    pub errors_dropped: u64,
}

/// A note that a takeover keeps: the worker has taken its note-on to send,
/// and its note-off is still queued.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StartedNote {
    /// The frame of the onset, as its batch was tagged.
    pub onset_frame: u64,
    /// The queued note-off. It names the channel and the note.
    pub note_off: MidiMessage,
}

/// Where encoded bytes actually go.
///
/// Behind a trait so tests can check the timing and encoding path with no
/// MIDI hardware and no ALSA sequencer.
pub trait MidiPort: Send {
    fn send(&mut self, bytes: &[u8]) -> Result<(), String>;

    /// Send now, with the queue deadline available to capture ports.
    fn send_scheduled(&mut self, _due: Instant, bytes: &[u8]) -> Result<(), String> {
        self.send(bytes)
    }
}

impl MidiPort for MidiOutputConnection {
    fn send(&mut self, bytes: &[u8]) -> Result<(), String> {
        MidiOutputConnection::send(self, bytes).map_err(|error| error.to_string())
    }
}

/// One captured send: when it went out, and the exact bytes.
pub type CapturedMessage = (Instant, Vec<u8>);

/// Records what was sent and when, for tests.
#[derive(Clone, Default)]
pub struct CapturePort {
    pub log: Arc<Mutex<Vec<CapturedMessage>>>,
}

impl CapturePort {
    pub fn new() -> Self {
        Self::default()
    }

    /// Every message sent so far, in send order.
    pub fn messages(&self) -> Vec<CapturedMessage> {
        self.log.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }
}

impl MidiPort for CapturePort {
    fn send(&mut self, bytes: &[u8]) -> Result<(), String> {
        self.log
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push((Instant::now(), bytes.to_vec()));
        Ok(())
    }
}

struct Queue {
    heap: BinaryHeap<Reverse<Queued>>,
    /// Batches whose first message has crossed the queue/port boundary.
    /// Cutover must retain their queued tail (especially note-offs), because
    /// the in-flight message can no longer be cancelled safely.
    started_batches: HashSet<u64>,
    /// Messages still in the heap for each tagged score batch.
    remaining_by_batch: HashMap<u64, usize>,
    /// Earliest per-batch hard expiry, for an O(1) no-work producer poll.
    next_expiry_frame: Option<u64>,
    stopping: bool,
}

impl Queue {
    /// Remove the queued tail of a batch whose note-on is known not to have
    /// reached the device. This is deliberately narrower than a generic send
    /// failure: a port error is ambiguous, so its fail-safe note-off remains.
    fn purge_unsounded_batch(&mut self, batch_id: u64) -> usize {
        let before = self.heap.len();
        self.heap
            .retain(|queued| queued.0.batch_id != Some(batch_id));
        self.remaining_by_batch.remove(&batch_id);
        self.started_batches.remove(&batch_id);
        before - self.heap.len()
    }
}

#[derive(PartialEq, Eq)]
struct Queued {
    due: Instant,
    /// Ties broken by arrival so a note-on never sorts after the note-off
    /// queued alongside it at the same instant.
    seq: u64,
    /// Score generation and onset frame are attached to the WHOLE batch.
    /// Every message produced by one onset carries the same pair, including a
    /// note-off due after a live-code takeover. This lets a cutover discard a
    /// future old onset atomically without either keeping its later note-off
    /// to cut a replacement note or dropping the off for an onset that already
    /// sounded. Host-originated messages leave `generation` empty.
    generation: Option<u64>,
    onset_frame: u64,
    /// Hard audio-frame boundary after which an untouched pre-takeover batch
    /// is stale even if its original onset was before that takeover.
    expires_at_frame: Option<u64>,
    /// Unique within this sender. Present only for score-owned batches that a
    /// generation cutover may prune.
    batch_id: Option<u64>,
    message: MidiMessage,
}

impl Ord for Queued {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.due
            .cmp(&other.due)
            .then_with(|| self.seq.cmp(&other.seq))
    }
}

impl PartialOrd for Queued {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

/// Resolve a score's port selector. This is the one resolver that both MIDI
/// openers and the studio's device authorization call.
///
/// A selector is a numeric index from `rustel devices`, or a case-insensitive
/// name fragment such as `.midi("Maschine")`. The first matching name wins.
/// Numeric indices are returned as written; the opener checks their bounds.
///
/// An explicit empty selector is refused: matching it as a substring would
/// silently select the first port. Callers handle an absent selector's default.
pub fn port_for_selector(selector: &str, names: &[String]) -> Option<usize> {
    let text = selector.trim();
    if text.is_empty() {
        return None;
    }
    if let Ok(index) = text.parse::<usize>() {
        return Some(index);
    }
    let needle = text.to_ascii_lowercase();
    names
        .iter()
        .position(|name| name.to_ascii_lowercase().contains(&needle))
}

/// Resolve and bounds-check a selector for either MIDI direction.
///
/// `None` selects the first port. `kind` identifies the direction in error
/// messages ("MIDI port" or "MIDI input").
pub(crate) fn resolve_port(
    selector: Option<&str>,
    names: &[String],
    kind: &str,
) -> Result<usize, String> {
    let index = match selector {
        None => 0,
        Some(text) => port_for_selector(text, names).ok_or_else(|| {
            format!(
                "{kind} \"{text}\" not found; available: {}",
                names.join(", ")
            )
        })?,
    };
    if index >= names.len() {
        return Err(format!(
            "{kind} index {index} is out of range; {} port(s) available: {}",
            names.len(),
            names.join(", ")
        ));
    }
    Ok(index)
}

/// Owns the port and the thread that drains the queue.
pub struct MidiSender {
    queue: Arc<(Mutex<Queue>, Condvar)>,
    seq: AtomicU64,
    batch_seq: AtomicU64,
    sent: Arc<AtomicU64>,
    dropped_late: Arc<AtomicU64>,
    refused_full: Arc<AtomicU64>,
    send_errors: Arc<AtomicU64>,
    port_name: String,
    thread: Option<std::thread::JoinHandle<()>>,
    completion: MidiSenderCompletion,
}

/// Cloneable observation that a detached sender worker has actually exited.
///
/// Dropping [`MidiSender`] only requests emergency silence and detaches. A
/// lifecycle manager can retain this tiny token to account for retirement
/// without waiting for a platform write or connection destructor.
#[derive(Clone)]
pub struct MidiSenderCompletion {
    finished: Arc<AtomicBool>,
    sent: Arc<AtomicU64>,
    dropped_late: Arc<AtomicU64>,
    refused_full: Arc<AtomicU64>,
    send_errors: Arc<AtomicU64>,
}

impl MidiSenderCompletion {
    pub fn is_finished(&self) -> bool {
        self.finished.load(Ordering::Acquire)
    }

    pub fn report(&self) -> MidiReport {
        MidiReport {
            sent: self.sent.load(Ordering::Relaxed),
            dropped_late: self.dropped_late.load(Ordering::Relaxed),
            refused_full: self.refused_full.load(Ordering::Relaxed),
            send_errors: self.send_errors.load(Ordering::Relaxed),
            errors_dropped: 0,
        }
    }
}

struct MidiWorkerExit {
    finished: Arc<AtomicBool>,
}

impl Drop for MidiWorkerExit {
    fn drop(&mut self) {
        self.finished.store(true, Ordering::Release);
    }
}

impl MidiSender {
    /// List the output ports, in the order a numeric `midiport` indexes them.
    pub fn ports() -> Result<Vec<String>, String> {
        let output = open_output("rustel")?;
        Ok(output
            .ports()
            .iter()
            .map(|port| output.port_name(port).unwrap_or_else(|_| "?".into()))
            .collect())
    }

    /// List the INPUT ports, in the order a numeric selector indexes them.
    ///
    /// Separate from [`Self::ports`] because CoreMIDI and ALSA keep sources and
    /// destinations in different namespaces: a controller is an input and a
    /// synth is an output, and the same device can appear in both with
    /// different indices. Listing only outputs left a musician guessing at what
    /// their controller is called.
    pub fn input_ports() -> Result<Vec<String>, String> {
        let input = open_input("rustel")?;
        Ok(input
            .ports()
            .iter()
            .map(|port| input.port_name(port).unwrap_or_else(|_| "?".into()))
            .collect())
    }

    /// Open a port by name or index.
    ///
    /// Upstream's `getDevice` accepts a name or an index and falls back to the
    /// first port, so a bare `.midi()` works with one device plugged in. The
    /// name match is case-insensitive and by substring, because the full names
    /// carry platform noise a musician should not have to type
    /// ("Midi Through:Midi Through Port-0 14:0").
    pub fn open(selector: Option<&str>) -> Result<Self, String> {
        let output = open_output("rustel")?;
        let ports = output.ports();
        if ports.is_empty() {
            return Err("no MIDI output ports available".into());
        }
        let names: Vec<String> = ports
            .iter()
            .map(|port| output.port_name(port).unwrap_or_else(|_| "?".into()))
            .collect();

        let index = resolve_port(selector, &names, "MIDI port")?;
        let port = &ports[index];
        let port_name = names[index].clone();
        let connection = output
            .connect(port, "rustel-out")
            .map_err(|error| format!("could not open MIDI port \"{port_name}\": {error}"))?;

        Self::try_with_port(Box::new(connection), port_name)
    }

    /// Create our own port that other software connects to, rather than
    /// opening one that already exists.
    ///
    /// On macOS and Linux this needs no setup: a program such as Ableton
    /// sees "rustel" in its MIDI inputs. The browser needs an IAC bus
    /// enabled in Audio MIDI Setup first.
    ///
    /// Windows has no equivalent - WinMM cannot create ports - so there the
    /// caller must install a loopback driver (loopMIDI) and [`Self::open`] it
    /// by name.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub fn create_virtual(name: &str) -> Result<Self, String> {
        use midir::os::unix::VirtualOutput as _;
        let output = open_output("rustel")?;
        let connection = output
            .create_virtual(name)
            .map_err(|error| format!("could not create virtual MIDI port \"{name}\": {error}"))?;
        Self::try_with_port(Box::new(connection), name.to_string())
    }

    /// Build a sender over any sink. Public so tests can drive the real
    /// timing path through a [`CapturePort`].
    pub fn with_port(port: Box<dyn MidiPort>, port_name: String) -> Self {
        let fallback_name = port_name.clone();
        Self::try_with_port(port, port_name).unwrap_or_else(|_| Self::closed(fallback_name))
    }

    /// Fallible form of [`Self::with_port`] used by platform openers.
    ///
    /// The reservation includes workers whose owner has already been dropped
    /// but whose platform `send` or connection destructor has not returned.
    pub fn try_with_port(port: Box<dyn MidiPort>, port_name: String) -> Result<Self, String> {
        Self::try_with_port_and_budget(port, port_name, Arc::clone(&MIDI_WORKER_BUDGET))
    }

    fn try_with_port_and_budget(
        port: Box<dyn MidiPort>,
        port_name: String,
        worker_budget: Arc<MidiWorkerBudget>,
    ) -> Result<Self, String> {
        let permit = worker_budget.reserve().ok_or_else(|| {
            format!(
                "at most {MAX_MIDI_SENDER_WORKERS} MIDI sender workers may be active or retiring"
            )
        })?;
        let queue = Arc::new((
            Mutex::new(Queue {
                heap: BinaryHeap::new(),
                started_batches: HashSet::new(),
                remaining_by_batch: HashMap::new(),
                next_expiry_frame: None,
                stopping: false,
            }),
            Condvar::new(),
        ));
        let sent = Arc::new(AtomicU64::new(0));
        let dropped_late = Arc::new(AtomicU64::new(0));
        let send_errors = Arc::new(AtomicU64::new(0));
        let completion = MidiSenderCompletion {
            finished: Arc::new(AtomicBool::new(false)),
            sent: Arc::clone(&sent),
            dropped_late: Arc::clone(&dropped_late),
            refused_full: Arc::new(AtomicU64::new(0)),
            send_errors: Arc::clone(&send_errors),
        };

        let thread = {
            let queue = Arc::clone(&queue);
            let sent = Arc::clone(&sent);
            let dropped_late = Arc::clone(&dropped_late);
            let send_errors = Arc::clone(&send_errors);
            let finished = Arc::clone(&completion.finished);
            std::thread::Builder::new()
                .name("rustel-midi".into())
                .spawn(move || {
                    // Declaration order is intentional: locals drop in
                    // reverse, so the port (and its platform connection)
                    // disappears first, then the global reservation, and the
                    // completion bit is published last even during unwind.
                    let _exit = MidiWorkerExit { finished };
                    let _permit = permit;
                    let mut owned_port = port;
                    let (lock, condvar) = &*queue;
                    let mut guard = lock.lock().unwrap_or_else(|e| e.into_inner());
                    loop {
                        if guard.stopping {
                            // Send only what is already due, which is the
                            // all-notes-off that `Drop` queues, and discard
                            // the rest. Waiting for the queue to empty would
                            // delay exit on Ctrl-C until the last note-off
                            // comes due, which can be many seconds.
                            let now = Instant::now();
                            let mut due_now = Vec::new();
                            while let Some(Reverse(next)) = guard.heap.peek() {
                                if next.due > now {
                                    break;
                                }
                                if let Some(Reverse(ready)) = guard.heap.pop() {
                                    due_now.push((ready.due, ready.message));
                                }
                            }
                            drop(guard);
                            for (due, message) in due_now {
                                if owned_port.send_scheduled(due, message.as_slice()).is_err() {
                                    send_errors.fetch_add(1, Ordering::Relaxed);
                                } else {
                                    sent.fetch_add(1, Ordering::Relaxed);
                                }
                            }
                            return;
                        }
                        let Some(Reverse(next)) = guard.heap.peek() else {
                            // Nothing queued: park until something arrives.
                            guard = condvar.wait(guard).unwrap_or_else(|e| e.into_inner());
                            continue;
                        };
                        let now = Instant::now();
                        if next.due > now {
                            let wait = next.due - now;
                            let (next_guard, _) = condvar
                                .wait_timeout(guard, wait)
                                .unwrap_or_else(|e| e.into_inner());
                            guard = next_guard;
                            continue;
                        }
                        let Some(Reverse(due)) = guard.heap.pop() else {
                            continue;
                        };
                        let batch_id = due.batch_id;
                        if let Some(batch_id) = batch_id {
                            // Only crossing the sound-start boundary commits
                            // the batch. `plan` deliberately places mapped
                            // controller changes before a note-on; a blocked
                            // CC must not pin that still-cancellable note and
                            // let it escape after a generation cutover.
                            // Conversely, publishing commitment while holding
                            // the queue lock before a note-on send means a
                            // simultaneous cutover can never split that onset
                            // from its fail-safe note-off.
                            if due.message.starts_sound() {
                                guard.started_batches.insert(batch_id);
                            }
                            let last_queued = guard
                                .remaining_by_batch
                                .get_mut(&batch_id)
                                .is_some_and(|remaining| {
                                    *remaining = remaining.saturating_sub(1);
                                    *remaining == 0
                                });
                            if last_queued {
                                guard.remaining_by_batch.remove(&batch_id);
                            }
                        }
                        // Send with the lock released: a slow or wedged port
                        // must not block the scheduler pushing new messages.
                        drop(guard);
                        let is_late_drop = now.duration_since(due.due) > MAX_QUEUE_LATENESS
                            && !due.message.ends_sound();
                        let purge_unsounded_batch = is_late_drop && due.message.starts_sound();
                        if is_late_drop {
                            dropped_late.fetch_add(1, Ordering::Relaxed);
                        } else if owned_port
                            .send_scheduled(due.due, due.message.as_slice())
                            .is_err()
                        {
                            send_errors.fetch_add(1, Ordering::Relaxed);
                        } else {
                            sent.fetch_add(1, Ordering::Relaxed);
                        }
                        guard = lock.lock().unwrap_or_else(|e| e.into_inner());
                        if let Some(batch_id) = batch_id {
                            if purge_unsounded_batch {
                                guard.purge_unsounded_batch(batch_id);
                            } else if !guard.remaining_by_batch.contains_key(&batch_id) {
                                guard.started_batches.remove(&batch_id);
                            }
                        }
                    }
                })
                .map_err(|error| format!("cannot start MIDI sender worker: {error}"))?
        };

        Ok(Self {
            queue,
            seq: AtomicU64::new(0),
            batch_seq: AtomicU64::new(0),
            sent,
            dropped_late,
            refused_full: Arc::clone(&completion.refused_full),
            send_errors,
            port_name,
            thread: Some(thread),
            completion,
        })
    }

    /// Inert cap-exhaustion value for the infallible test/sink constructor.
    /// Platform openers always use the fallible constructor and surface the
    /// reservation error instead of producing this sentinel.
    fn closed(port_name: String) -> Self {
        Self {
            queue: Arc::new((
                Mutex::new(Queue {
                    heap: BinaryHeap::new(),
                    started_batches: HashSet::new(),
                    remaining_by_batch: HashMap::new(),
                    next_expiry_frame: None,
                    stopping: true,
                }),
                Condvar::new(),
            )),
            seq: AtomicU64::new(0),
            batch_seq: AtomicU64::new(0),
            sent: Arc::new(AtomicU64::new(0)),
            dropped_late: Arc::new(AtomicU64::new(0)),
            refused_full: Arc::new(AtomicU64::new(0)),
            send_errors: Arc::new(AtomicU64::new(0)),
            port_name,
            thread: None,
            completion: MidiSenderCompletion {
                finished: Arc::new(AtomicBool::new(true)),
                sent: Arc::new(AtomicU64::new(0)),
                dropped_late: Arc::new(AtomicU64::new(0)),
                refused_full: Arc::new(AtomicU64::new(0)),
                send_errors: Arc::new(AtomicU64::new(0)),
            },
        }
    }

    pub fn port_name(&self) -> &str {
        &self.port_name
    }

    pub fn completion(&self) -> MidiSenderCompletion {
        self.completion.clone()
    }

    /// Queue a message for `due`. Never blocks. Returns whether the queue
    /// accepted the message; a stopped/full queue is counted where applicable
    /// and reported as `false` so producer-side correlation cannot claim a
    /// dropped message crossed the output boundary.
    pub fn send_at(&self, due: Instant, message: MidiMessage) -> bool {
        self.send_batch_at(&[(due, message)])
    }

    /// Queue one onset's messages as a single admission decision.
    ///
    /// A note-on and its already-planned note-off are one safety unit: if the
    /// bounded queue has room for only the note-on, accepting it would leave a
    /// sounding note with no scheduled way to stop. The producer therefore
    /// hands the complete onset to this method, which either inserts every
    /// message while holding the queue lock or inserts none of them.
    ///
    /// Returns `false` for an empty batch, a stopped queue, or a batch that
    /// does not fit. A capacity refusal counts every refused message in
    /// [`MidiReport::refused_full`].
    pub fn send_batch_at(&self, messages: &[(Instant, MidiMessage)]) -> bool {
        self.send_batch_tagged(None, 0, None, messages)
    }

    /// Queue one score onset with the generation/frame used by audio cutover.
    ///
    /// The frame describes the batch's ONSET, not each message's due time: a
    /// pre-cutover note keeps its paired note-off even when that off is later
    /// than the takeover. Conversely, every message of an onset at or after
    /// the takeover is stale and can be removed as one unit.
    pub fn send_batch_for_generation_at(
        &self,
        generation: u64,
        onset_frame: u64,
        messages: &[(Instant, MidiMessage)],
    ) -> bool {
        self.send_batch_for_generation_with_expiry_at(generation, onset_frame, None, messages)
    }

    /// Queue a score onset that was retained while its port opened.
    ///
    /// `expires_at_frame` preserves the original generation takeover boundary
    /// across that asynchronous open; dropping it here would let a delayed
    /// result flush a stale pre-takeover onset.
    pub fn send_batch_for_generation_with_expiry_at(
        &self,
        generation: u64,
        onset_frame: u64,
        expires_at_frame: Option<u64>,
        messages: &[(Instant, MidiMessage)],
    ) -> bool {
        self.send_batch_tagged(Some(generation), onset_frame, expires_at_frame, messages)
    }

    fn send_batch_tagged(
        &self,
        generation: Option<u64>,
        onset_frame: u64,
        expires_at_frame: Option<u64>,
        messages: &[(Instant, MidiMessage)],
    ) -> bool {
        if messages.is_empty() {
            return false;
        }

        let (lock, condvar) = &*self.queue;
        let mut guard = lock.lock().unwrap_or_else(|e| e.into_inner());
        if guard.stopping {
            return false;
        }
        if messages.len() > MAX_QUEUED_MESSAGES.saturating_sub(guard.heap.len()) {
            self.refused_full
                .fetch_add(messages.len() as u64, Ordering::Relaxed);
            return false;
        }
        let batch_id = generation.map(|_| self.batch_seq.fetch_add(1, Ordering::Relaxed));
        if let Some(batch_id) = batch_id {
            guard.remaining_by_batch.insert(batch_id, messages.len());
        }
        if let Some(deadline) = expires_at_frame {
            guard.next_expiry_frame = Some(
                guard
                    .next_expiry_frame
                    .map_or(deadline, |known| known.min(deadline)),
            );
        }
        for &(due, message) in messages {
            guard.heap.push(Reverse(Queued {
                due,
                seq: self.seq.fetch_add(1, Ordering::Relaxed),
                generation,
                onset_frame,
                expires_at_frame,
                batch_id,
                message,
            }));
        }
        drop(guard);
        condvar.notify_one();
        true
    }

    /// Discard future onsets belonging to a superseded score generation.
    ///
    /// This shares the admission mutex, so a producer cannot interleave a
    /// partial batch with the cutover. The predicate is batch metadata rather
    /// than message due-time: every untouched note-on/off pair is removed
    /// together. Once the worker has popped a sound-starting message, the batch
    /// is committed and its queued tail is retained; the in-flight note can no
    /// longer be cancelled, and pruning its note-off could strand it.
    /// Untagged host messages are never score-cutover state.
    pub fn prune_generation_from(&self, generation: u64, takeover_frame: u64) -> usize {
        self.prune_generation_reporting_started(generation, takeover_frame)
            .0
    }

    /// [`Self::prune_generation_from`], with the notes it keeps because
    /// their batch has started: the notes of `generation` from
    /// `takeover_frame` on whose note-on the worker has taken and whose
    /// note-off is still queued.
    ///
    /// The generation that takes over plays those onsets again. The caller
    /// can leave its copy of each reported note out, so that the note does
    /// not sound twice. The report and the removal share one lock: the
    /// worker cannot start a batch between the two.
    pub fn prune_generation_reporting_started(
        &self,
        generation: u64,
        takeover_frame: u64,
    ) -> (usize, Vec<StartedNote>) {
        let (lock, condvar) = &*self.queue;
        let mut guard = lock.lock().unwrap_or_else(|e| e.into_inner());
        let Queue {
            heap,
            started_batches,
            remaining_by_batch,
            next_expiry_frame,
            ..
        } = &mut *guard;
        let before = heap.len();
        let mut removed_by_batch = HashMap::<u64, usize>::new();
        let mut queued_messages = std::mem::take(heap).into_vec();
        let mut marked_expiry = false;
        let mut started_notes = Vec::new();
        queued_messages.retain_mut(|item| {
            let queued = &mut item.0;
            let started = queued
                .batch_id
                .is_some_and(|batch_id| started_batches.contains(&batch_id));
            let superseded =
                queued.generation == Some(generation) && queued.onset_frame >= takeover_frame;
            let remove = superseded && !started;
            if superseded && started && queued.message.is_note_off() {
                started_notes.push(StartedNote {
                    onset_frame: queued.onset_frame,
                    note_off: queued.message,
                });
            }
            if remove && let Some(batch_id) = queued.batch_id {
                *removed_by_batch.entry(batch_id).or_default() += 1;
            }
            if !remove && !started && queued.generation == Some(generation) {
                marked_expiry = true;
                queued.expires_at_frame = Some(
                    queued
                        .expires_at_frame
                        .map_or(takeover_frame, |deadline| deadline.min(takeover_frame)),
                );
            }
            !remove
        });
        *heap = BinaryHeap::from(queued_messages);
        if marked_expiry {
            *next_expiry_frame = Some(
                (*next_expiry_frame).map_or(takeover_frame, |known| known.min(takeover_frame)),
            );
        }
        for (batch_id, count) in removed_by_batch {
            let empty = remaining_by_batch
                .get_mut(&batch_id)
                .is_some_and(|remaining| {
                    *remaining = remaining.saturating_sub(count);
                    *remaining == 0
                });
            if empty {
                remaining_by_batch.remove(&batch_id);
                started_batches.remove(&batch_id);
            }
        }
        let removed = before - heap.len();
        drop(guard);
        if removed != 0 {
            // The worker may be sleeping on a removed head; make it recompute
            // its deadline instead of waiting for stale work to come due.
            condvar.notify_one();
        }
        (removed, started_notes)
    }

    /// Discard untouched pre-takeover batches at their exact hard boundary.
    ///
    /// Each queued message carries the deadline assigned when its generation
    /// was replaced, so rapid A→B→C transitions prune A at A's boundary rather
    /// than extending it to C's later retirement deadline. Sound-started
    /// batches retain their fail-safe tail; untagged host work is unaffected.
    pub fn prune_expired_at(&self, current_frame: u64) -> usize {
        let (lock, condvar) = &*self.queue;
        let mut guard = lock.lock().unwrap_or_else(|e| e.into_inner());
        if guard
            .next_expiry_frame
            .is_none_or(|deadline| deadline > current_frame)
        {
            return 0;
        }
        let Queue {
            heap,
            started_batches,
            remaining_by_batch,
            next_expiry_frame,
            ..
        } = &mut *guard;
        let before = heap.len();
        let mut removed_by_batch = HashMap::<u64, usize>::new();
        heap.retain(|queued| {
            let queued = &queued.0;
            let started = queued
                .batch_id
                .is_some_and(|batch_id| started_batches.contains(&batch_id));
            let remove = queued
                .expires_at_frame
                .is_some_and(|deadline| deadline <= current_frame)
                && !started;
            if remove && let Some(batch_id) = queued.batch_id {
                *removed_by_batch.entry(batch_id).or_default() += 1;
            }
            !remove
        });
        for (batch_id, count) in removed_by_batch {
            let empty = remaining_by_batch
                .get_mut(&batch_id)
                .is_some_and(|remaining| {
                    *remaining = remaining.saturating_sub(count);
                    *remaining == 0
                });
            if empty {
                remaining_by_batch.remove(&batch_id);
                started_batches.remove(&batch_id);
            }
        }
        *next_expiry_frame = heap
            .iter()
            .filter_map(|queued| queued.0.expires_at_frame)
            .filter(|deadline| *deadline > current_frame)
            .min();
        let removed = before - heap.len();
        drop(guard);
        if removed != 0 {
            condvar.notify_one();
        }
        removed
    }

    pub fn report(&self) -> MidiReport {
        MidiReport {
            sent: self.sent.load(Ordering::Relaxed),
            dropped_late: self.dropped_late.load(Ordering::Relaxed),
            refused_full: self.refused_full.load(Ordering::Relaxed),
            send_errors: self.send_errors.load(Ordering::Relaxed),
            errors_dropped: 0,
        }
    }

    /// All-notes-off on every channel.
    ///
    /// A set that ends without this leaves whatever was sounding stuck on,
    /// because the note-offs were still queued. This is a priority operation:
    /// discard scheduled work, then enqueue the 32 emergency messages even if
    /// the ordinary bounded queue was full.
    pub fn silence_all(&self) {
        let now = Instant::now();
        let (lock, condvar) = &*self.queue;
        let mut guard = lock.lock().unwrap_or_else(|e| e.into_inner());
        if guard.stopping {
            return;
        }
        self.replace_queue_with_silence(&mut guard, now);
        drop(guard);
        condvar.notify_one();
    }

    /// Replace all scheduled work with the emergency messages while the
    /// caller owns the queue lock.
    fn replace_queue_with_silence(&self, guard: &mut Queue, now: Instant) {
        guard.heap.clear();
        guard.started_batches.clear();
        guard.remaining_by_batch.clear();
        guard.next_expiry_frame = None;
        for channel in 1..=16u8 {
            // CC 123 all-notes-off, then CC 120 all-sound-off for devices that
            // ignore the first.
            for controller in [123, 120] {
                guard.heap.push(Reverse(Queued {
                    due: now,
                    seq: self.seq.fetch_add(1, Ordering::Relaxed),
                    generation: None,
                    onset_frame: 0,
                    expires_at_frame: None,
                    batch_id: None,
                    message: MidiMessage::control_change(channel, controller, 0),
                }));
            }
        }
    }
}

impl Drop for MidiSender {
    fn drop(&mut self) {
        {
            let (lock, condvar) = &*self.queue;
            let mut guard = lock.lock().unwrap_or_else(|e| e.into_inner());
            // Clearing future work, installing the emergency messages, and
            // publishing `stopping` are one state transition. The worker can
            // therefore never observe a shutdown with the old full queue, nor
            // exit before all 32 emergency messages are visible.
            self.replace_queue_with_silence(&mut guard, Instant::now());
            guard.stopping = true;
            condvar.notify_all();
        }
        // Dropping a JoinHandle detaches. A platform send can wedge after a
        // cable disappears; joining here placed that unbounded wait directly
        // on the live producer. The worker owns the connection, its global
        // reservation, and the completion token until emergency silence and
        // platform destruction actually finish.
        let _ = self.thread.take();
    }
}

#[cfg(test)]
mod sequencer_tests {
    use super::*;

    #[cfg(target_os = "linux")]
    fn verdict_for(errno: i32) -> Result<(), &'static str> {
        sequencer_verdict(Err(std::io::Error::from_raw_os_error(errno)))
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn an_absent_sequencer_is_no_sequencer() {
        assert_eq!(verdict_for(libc::ENOENT), Err(NO_SEQUENCER));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn a_sequencer_this_user_may_open_is_left_to_alsa() {
        assert_eq!(sequencer_verdict(Ok(())), Ok(()));
    }

    /// Root passes `access(2)` whatever the device's mode, so this is the only
    /// place a refusal can be tested on a machine that runs its tests as root.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_sequencer_that_refuses_this_user_says_how_to_be_let_in() {
        for errno in [libc::EACCES, libc::EPERM] {
            assert_eq!(verdict_for(errno), Err(SEQUENCER_REFUSED), "errno {errno}");
        }
        for words in ["/dev/snd/seq", "audio group", "log in again"] {
            assert!(
                SEQUENCER_REFUSED.contains(words),
                "{words:?} is missing from {SEQUENCER_REFUSED:?}"
            );
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn any_other_access_failure_is_left_to_alsa_to_report() {
        assert_eq!(verdict_for(libc::EIO), Ok(()));
    }

    /// An absent or inaccessible sequencer reports its reason once without
    /// requesting a port from ALSA. The host determines which case runs.
    #[test]
    fn a_sequencer_absent_or_refused_is_named_rather_than_probed() {
        let ready = sequencer_ready();
        #[cfg(target_os = "linux")]
        if !std::path::Path::new("/dev/snd/seq").exists() {
            assert_eq!(ready, Err(NO_SEQUENCER));
        }
        #[cfg(not(target_os = "linux"))]
        assert_eq!(ready, Ok(()), "only ALSA has this device");

        let Err(reason) = ready else {
            return;
        };
        assert!(
            reason.contains("/dev/snd/seq"),
            "the reason names the device: {reason}"
        );
        for opened in [open_output("rustel").err(), open_input("rustel").err()] {
            assert_eq!(opened.as_deref(), Some(reason));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wait_for_completion(completion: &MidiSenderCompletion) {
        let deadline = Instant::now() + Duration::from_secs(2);
        while !completion.is_finished() {
            assert!(
                Instant::now() < deadline,
                "MIDI sender worker did not complete"
            );
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    #[test]
    fn channel_is_one_based_on_the_way_in_and_zero_based_on_the_wire() {
        assert_eq!(
            MidiMessage::note_on(1, 60, 100).as_slice(),
            &[0x90, 60, 100]
        );
        assert_eq!(
            MidiMessage::note_on(16, 60, 100).as_slice(),
            &[0x9f, 60, 100]
        );
        // Out-of-range channels clamp rather than wrapping into another
        // channel's messages.
        assert_eq!(
            MidiMessage::note_on(0, 60, 100).as_slice(),
            &[0x90, 60, 100]
        );
        assert_eq!(
            MidiMessage::note_on(99, 60, 100).as_slice(),
            &[0x9f, 60, 100]
        );
    }

    #[test]
    fn note_off_is_a_real_note_off_not_a_zero_velocity_note_on() {
        assert_eq!(MidiMessage::note_off(1, 60).as_slice(), &[0x80, 60, 0]);
    }

    #[test]
    fn velocity_never_reaches_zero_because_that_would_be_a_note_off() {
        assert_eq!(velocity_byte(0.0), 1);
        assert_eq!(velocity_byte(-5.0), 1);
        assert_eq!(velocity_byte(f64::NAN), 1);
        assert_eq!(velocity_byte(1.0), 127);
        assert_eq!(velocity_byte(2.0), 127);
        // Upstream default: gain 1 * velocity 0.9.
        assert_eq!(velocity_byte(0.9), 114);
    }

    #[test]
    fn unit_values_scale_the_way_strudel_rounds_them() {
        // `Math.round(ccv * 127)`
        assert_eq!(unit_byte(0.0), 0);
        assert_eq!(unit_byte(1.0), 127);
        assert_eq!(unit_byte(0.5), 64);
        assert_eq!(unit_byte(f64::INFINITY), 0);
    }

    #[test]
    fn pitch_bend_centres_and_splits_lsb_first() {
        assert_eq!(bend_value(0.0), 8192);
        assert_eq!(bend_value(-1.0), 0);
        assert_eq!(bend_value(1.0), 16383);
        assert_eq!(bend_value(f64::NAN), 8192);
        // 8192 = 0b10_0000_0000_0000: LSB 0, MSB 64.
        assert_eq!(
            MidiMessage::pitch_bend(1, 8192).as_slice(),
            &[0xe0, 0x00, 0x40]
        );
    }

    #[test]
    fn out_of_range_notes_are_refused_rather_than_wrapped() {
        assert_eq!(note_byte(60.0), Some(60));
        assert_eq!(note_byte(60.4), Some(60));
        assert_eq!(note_byte(-1.0), None);
        assert_eq!(note_byte(128.0), None);
        assert_eq!(note_byte(f64::NAN), None);
    }

    #[test]
    fn realtime_messages_are_single_status_bytes() {
        assert_eq!(MidiMessage::clock().as_slice(), &[0xf8]);
        assert_eq!(MidiMessage::start().as_slice(), &[0xfa]);
        assert_eq!(MidiMessage::cont().as_slice(), &[0xfb]);
        assert_eq!(MidiMessage::stop().as_slice(), &[0xfc]);
    }

    /// An empty selector is refused: `str::contains("")` is true of every
    /// name, so `.midi("")` would silently pick the first port.
    #[test]
    fn an_empty_selector_is_refused_rather_than_matching_the_first_port() {
        let names = vec!["Alpha One".to_string(), "Beta Two".to_string()];
        assert_eq!(port_for_selector("", &names), None);
        assert_eq!(port_for_selector("   ", &names), None);
        // The loose rule that is the point of this function still works.
        assert_eq!(port_for_selector("beta", &names), Some(1));
        assert_eq!(port_for_selector("BETA TWO", &names), Some(1));
        // A numeric string is still an index, trimmed like any selector.
        assert_eq!(port_for_selector("1", &names), Some(1));
        assert_eq!(port_for_selector(" 0 ", &names), Some(0));
        // A name that matches nothing is still refused, not fallen back from.
        assert_eq!(port_for_selector("Gamma", &names), None);
    }

    /// Both openers resolve through `resolve_port`, so this is what
    /// `.midi(...)` and `midin(...)` open, tested without a device: no
    /// selector is the first port, an explicit empty one is refused like a
    /// name that matches nothing, and an index past the ports is refused.
    #[test]
    fn the_openers_resolve_a_selector_to_a_port_or_a_named_refusal() {
        let names = vec!["Controller Keys".to_string(), "Controller Pads".to_string()];
        assert_eq!(resolve_port(None, &names, "MIDI input"), Ok(0));
        assert_eq!(resolve_port(Some("pads"), &names, "MIDI input"), Ok(1));
        assert_eq!(resolve_port(Some("1"), &names, "MIDI port"), Ok(1));
        for empty in ["", "   "] {
            let refusal = resolve_port(Some(empty), &names, "MIDI input").unwrap_err();
            assert!(
                refusal.starts_with("MIDI input") && refusal.contains("not found; available: "),
                "{refusal}"
            );
        }
        let refusal = resolve_port(Some("missing"), &names, "MIDI port").unwrap_err();
        assert!(refusal.contains("\"missing\" not found"), "{refusal}");
        let refusal = resolve_port(Some("2"), &names, "MIDI port").unwrap_err();
        assert!(refusal.contains("index 2 is out of range"), "{refusal}");
    }

    #[test]
    fn the_queue_orders_by_time_then_arrival() {
        let now = Instant::now();
        let mut heap = BinaryHeap::new();
        // Pushed out of order, and two share an instant.
        heap.push(Reverse(Queued {
            due: now + Duration::from_millis(10),
            seq: 2,
            generation: None,
            onset_frame: 0,
            expires_at_frame: None,
            batch_id: None,
            message: MidiMessage::note_off(1, 60),
        }));
        heap.push(Reverse(Queued {
            due: now,
            seq: 1,
            generation: None,
            onset_frame: 0,
            expires_at_frame: None,
            batch_id: None,
            message: MidiMessage::note_on(1, 62, 100),
        }));
        heap.push(Reverse(Queued {
            due: now,
            seq: 0,
            generation: None,
            onset_frame: 0,
            expires_at_frame: None,
            batch_id: None,
            message: MidiMessage::note_on(1, 60, 100),
        }));
        let order: Vec<u64> = std::iter::from_fn(|| heap.pop().map(|Reverse(q)| q.seq)).collect();
        assert_eq!(
            order,
            vec![0, 1, 2],
            "a note-on must never sort after a note-off queued with it"
        );
    }

    /// The whole point of the sender thread: a note-off queued at note-on time
    /// must actually arrive later, in order, near its target. Upstream gets
    /// this from Web MIDI's `time:` argument; we have to do it ourselves.
    #[test]
    fn queued_messages_arrive_in_time_order_near_their_targets() {
        let capture = CapturePort::new();
        let sender = MidiSender::with_port(Box::new(capture.clone()), "capture".into());

        let start = Instant::now();
        // Queued out of order on purpose.
        sender.send_at(
            start + Duration::from_millis(120),
            MidiMessage::note_off(1, 60),
        );
        sender.send_at(start, MidiMessage::note_on(1, 60, 100));
        sender.send_at(
            start + Duration::from_millis(60),
            MidiMessage::control_change(1, 74, 64),
        );

        std::thread::sleep(Duration::from_millis(320));
        let sent = capture.messages();
        assert_eq!(sent.len(), 3, "expected all three, got {sent:?}");

        let payloads: Vec<Vec<u8>> = sent.iter().map(|(_, bytes)| bytes.clone()).collect();
        assert_eq!(
            payloads,
            vec![vec![0x90, 60, 100], vec![0xb0, 74, 64], vec![0x80, 60, 0],],
            "time order was not restored"
        );

        // Generous bounds: this asserts scheduling happened at all, not that
        // the OS scheduler is precise.
        let offset = |i: usize| sent[i].0.duration_since(start).as_millis() as i64;
        assert!(offset(1) >= 50, "CC arrived too early: {}ms", offset(1));
        assert!(
            offset(2) >= 110,
            "note-off arrived too early: {}ms",
            offset(2)
        );
        assert!(
            offset(2) < 300,
            "note-off arrived too late: {}ms",
            offset(2)
        );
    }

    /// A late note-off or all-notes-off is still sent. The lateness bound
    /// drops a late note-on. A dropped note-off leaves the note sounding.
    #[test]
    fn a_late_note_off_is_still_sent_because_a_dropped_one_is_a_drone() {
        let port = CapturePort::new();
        let sender = MidiSender::with_port(Box::new(port.clone()), "capture".into());
        let long_past = Instant::now() - (MAX_QUEUE_LATENESS + Duration::from_millis(250));
        sender.send_at(long_past, MidiMessage::note_on(1, 60, 100));
        sender.send_at(long_past, MidiMessage::note_off(1, 60));
        sender.send_at(long_past, MidiMessage::control_change(1, 123, 0));
        std::thread::sleep(Duration::from_millis(120));

        let sent: Vec<Vec<u8>> = port.messages().into_iter().map(|(_, b)| b).collect();
        assert!(
            sent.iter().any(|b| b[0] == 0x80 && b[1] == 60),
            "the stale note-off was dropped, stranding the note: {sent:?}"
        );
        assert!(
            sent.iter().any(|b| b[0] == 0xb0 && b[1] == 123),
            "all-notes-off was dropped for being late: {sent:?}"
        );
        assert!(
            !sent.iter().any(|b| b[0] == 0x90),
            "the stale note-ON should still be dropped: {sent:?}"
        );
        let report = sender.report();
        assert_eq!(report.dropped_late, 1, "only the note-on should be dropped");
    }

    #[test]
    fn messages_past_their_moment_are_dropped_not_sent_late() {
        let capture = CapturePort::new();
        let sender = MidiSender::with_port(Box::new(capture.clone()), "capture".into());

        sender.send_at(
            Instant::now() - MAX_QUEUE_LATENESS - Duration::from_millis(200),
            MidiMessage::note_on(1, 60, 100),
        );
        sender.send_at(Instant::now(), MidiMessage::note_on(1, 62, 100));
        std::thread::sleep(Duration::from_millis(150));

        let sent = capture.messages();
        assert_eq!(sent.len(), 1, "the stale note-on should not have been sent");
        assert_eq!(sent[0].1, vec![0x90, 62, 100]);
        assert_eq!(sender.report().dropped_late, 1);
    }

    /// A runaway score must not grow the queue without bound.
    #[test]
    fn a_full_queue_refuses_rather_than_growing_or_blocking() {
        let capture = CapturePort::new();
        // Far enough out that the drain thread cannot empty it underneath us.
        let sender = MidiSender::with_port(Box::new(capture), "capture".into());
        let due = Instant::now() + Duration::from_secs(30);
        let mut accepted = 0;
        for _ in 0..(MAX_QUEUED_MESSAGES + 64) {
            accepted += usize::from(sender.send_at(due, MidiMessage::note_on(1, 60, 100)));
        }
        assert_eq!(accepted, MAX_QUEUED_MESSAGES);
        assert!(
            sender.report().refused_full >= 64,
            "queue did not refuse past its ceiling: {:?}",
            sender.report()
        );
    }

    /// Queue admission is per onset, not per wire message. In particular, the
    /// last free slot may not accept a note-on while refusing its paired off.
    #[test]
    fn a_note_on_and_its_off_are_refused_together_at_capacity() {
        let capture = CapturePort::new();
        let sender = MidiSender::with_port(Box::new(capture), "capture".into());
        let due = Instant::now() + Duration::from_secs(30);
        for _ in 0..(MAX_QUEUED_MESSAGES - 1) {
            assert!(sender.send_at(due, MidiMessage::control_change(1, 74, 64)));
        }

        let note_on = MidiMessage::note_on(1, 61, 100);
        let note_off = MidiMessage::note_off(1, 61);
        assert!(
            !sender.send_batch_at(&[(due, note_on), (due + Duration::from_secs(1), note_off)]),
            "the queue admitted only part of an onset"
        );

        let (lock, _) = &*sender.queue;
        let guard = lock.lock().unwrap_or_else(|e| e.into_inner());
        assert_eq!(guard.heap.len(), MAX_QUEUED_MESSAGES - 1);
        assert!(
            !guard
                .heap
                .iter()
                .any(|queued| queued.0.message == note_on || queued.0.message == note_off),
            "a partial note pair reached the queue"
        );
        drop(guard);
        assert_eq!(sender.report().refused_full, 2);
    }

    #[test]
    fn generation_cutover_prunes_whole_future_batches_at_the_exact_frame() {
        let sender =
            MidiSender::with_port(Box::new(CapturePort::new()), "generation-cutover".into());
        let due = Instant::now() + Duration::from_secs(30);
        let before = [
            (due, MidiMessage::note_on(1, 60, 100)),
            (due + Duration::from_secs(10), MidiMessage::note_off(1, 60)),
        ];
        let exact = [
            (due, MidiMessage::note_on(1, 61, 100)),
            (due + Duration::from_secs(10), MidiMessage::note_off(1, 61)),
        ];
        let after = [
            (due, MidiMessage::note_on(1, 62, 100)),
            (due + Duration::from_secs(10), MidiMessage::note_off(1, 62)),
        ];
        assert!(sender.send_batch_for_generation_at(7, 99, &before));
        assert!(sender.send_batch_for_generation_at(7, 100, &exact));
        assert!(sender.send_batch_for_generation_at(7, 101, &after));

        assert_eq!(sender.prune_generation_from(7, 100), 4);
        let (lock, _) = &*sender.queue;
        let guard = lock.lock().unwrap_or_else(|e| e.into_inner());
        let retained = guard
            .heap
            .iter()
            .map(|queued| (queued.0.onset_frame, queued.0.message))
            .collect::<Vec<_>>();
        assert_eq!(retained.len(), 2);
        assert!(retained.iter().all(|(frame, _)| *frame == 99));
        assert!(
            retained
                .iter()
                .any(|(_, message)| *message == MidiMessage::note_on(1, 60, 100))
        );
        assert!(
            retained
                .iter()
                .any(|(_, message)| *message == MidiMessage::note_off(1, 60))
        );
    }

    #[test]
    fn cutover_keeps_other_generations_and_untagged_host_messages() {
        let sender = MidiSender::with_port(Box::new(CapturePort::new()), "mixed-cutover".into());
        let due = Instant::now() + Duration::from_secs(30);
        assert!(sender.send_batch_for_generation_at(1, 100, &[(due, MidiMessage::start())]));
        assert!(sender.send_batch_for_generation_at(2, 100, &[(due, MidiMessage::cont())]));
        assert!(sender.send_at(due, MidiMessage::clock()));

        assert_eq!(sender.prune_generation_from(1, 100), 1);
        let (lock, _) = &*sender.queue;
        let guard = lock.lock().unwrap_or_else(|e| e.into_inner());
        let messages = guard
            .heap
            .iter()
            .map(|queued| queued.0.message)
            .collect::<Vec<_>>();
        assert_eq!(messages.len(), 2);
        assert!(messages.contains(&MidiMessage::cont()));
        assert!(messages.contains(&MidiMessage::clock()));
    }

    #[test]
    fn late_tagged_note_on_purges_its_future_off_before_cutover() {
        let capture = CapturePort::new();
        let sender = MidiSender::with_port(Box::new(capture.clone()), "late-tagged".into());
        let now = Instant::now();
        assert!(sender.send_batch_for_generation_at(
            7,
            100,
            &[
                (
                    now - MAX_QUEUE_LATENESS - Duration::from_millis(100),
                    MidiMessage::note_on(1, 60, 100),
                ),
                (now + Duration::from_secs(30), MidiMessage::note_off(1, 60)),
            ],
        ));

        let deadline = Instant::now() + Duration::from_secs(2);
        while sender.report().dropped_late == 0 {
            assert!(
                Instant::now() < deadline,
                "worker never classified the stale note-on"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(capture.messages().is_empty(), "the stale note-on was sent");
        assert_eq!(
            sender.prune_generation_from(7, 100),
            0,
            "cutover found an orphaned tail after the unsent note-on"
        );
        loop {
            let (lock, _) = &*sender.queue;
            let clean = {
                let guard = lock.lock().unwrap_or_else(|error| error.into_inner());
                guard.heap.is_empty()
                    && guard.remaining_by_batch.is_empty()
                    && guard.started_batches.is_empty()
            };
            if clean {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "the paired note-off or its batch metadata was stranded"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    struct RejectNoteOnPort {
        attempted: std::sync::mpsc::SyncSender<()>,
    }

    impl MidiPort for RejectNoteOnPort {
        fn send(&mut self, bytes: &[u8]) -> Result<(), String> {
            if bytes.first().is_some_and(|status| status & 0xf0 == 0x90) {
                let _ = self.attempted.send(());
                return Err("ambiguous device failure".into());
            }
            Ok(())
        }
    }

    #[test]
    fn failed_note_on_keeps_its_fail_safe_off_across_cutover() {
        let (attempted_tx, attempted_rx) = std::sync::mpsc::sync_channel(0);
        let sender = MidiSender::with_port(
            Box::new(RejectNoteOnPort {
                attempted: attempted_tx,
            }),
            "reject-note-on".into(),
        );
        let now = Instant::now();
        assert!(sender.send_batch_for_generation_at(
            7,
            100,
            &[
                (now, MidiMessage::note_on(1, 60, 100)),
                (now + Duration::from_secs(30), MidiMessage::note_off(1, 60)),
            ],
        ));
        attempted_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("worker never attempted the note-on");

        assert_eq!(sender.prune_generation_from(7, 100), 0);
        let (lock, _) = &*sender.queue;
        {
            let guard = lock.lock().unwrap_or_else(|error| error.into_inner());
            assert!(
                guard
                    .heap
                    .iter()
                    .any(|queued| queued.0.message == MidiMessage::note_off(1, 60)),
                "ambiguous send failure discarded the fail-safe note-off"
            );
        }
        // The port announced the attempt from inside `send`, before the worker
        // saw the `Err` and counted it, so a reader woken by that announcement
        // can outrun the count. Wait for it the way the lateness test waits
        // for `dropped_late`.
        let deadline = Instant::now() + Duration::from_secs(2);
        while sender.report().send_errors == 0 {
            assert!(
                Instant::now() < deadline,
                "worker never recorded the ambiguous note-on failure"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(sender.report().send_errors, 1);
    }

    /// Deterministically stop the port after the worker has popped a note-on
    /// but before its send returns. This is the cutover race that used to let
    /// pruning remove the paired note-off and leave the device sounding.
    struct NoteOnBarrierPort {
        log: Arc<Mutex<Vec<CapturedMessage>>>,
        entered: Option<std::sync::mpsc::SyncSender<()>>,
        release: std::sync::mpsc::Receiver<()>,
    }

    impl MidiPort for NoteOnBarrierPort {
        fn send(&mut self, bytes: &[u8]) -> Result<(), String> {
            if bytes.first().is_some_and(|status| status & 0xf0 == 0x90)
                && let Some(entered) = self.entered.take()
            {
                entered
                    .send(())
                    .map_err(|_| "cutover test did not observe the note-on".to_string())?;
                self.release
                    .recv_timeout(Duration::from_secs(2))
                    .map_err(|_| "cutover test did not release the note-on".to_string())?;
            }
            self.log
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .push((Instant::now(), bytes.to_vec()));
            Ok(())
        }
    }

    #[test]
    fn cutover_cannot_split_an_in_flight_note_on_from_its_note_off() {
        let log = Arc::new(Mutex::new(Vec::new()));
        let (entered_tx, entered_rx) = std::sync::mpsc::sync_channel(0);
        let (release_tx, release_rx) = std::sync::mpsc::sync_channel(0);
        let port = NoteOnBarrierPort {
            log: Arc::clone(&log),
            entered: Some(entered_tx),
            release: release_rx,
        };
        let sender = MidiSender::with_port(Box::new(port), "cutover-barrier".into());
        let now = Instant::now();
        assert!(sender.send_batch_for_generation_at(
            7,
            100,
            &[
                (now, MidiMessage::note_on(1, 60, 100)),
                (
                    now + Duration::from_millis(60),
                    MidiMessage::note_off(1, 60)
                ),
            ],
        ));

        entered_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("worker never reached the barrier port");
        let removed = sender.prune_generation_from(7, 100);
        // Always release before asserting so a failed regression cannot strand
        // the sender's Drop waiting for this deliberately blocked test port.
        release_tx.send(()).expect("worker abandoned the barrier");
        assert_eq!(removed, 0, "cutover split a batch already being sent");

        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            let payloads = log
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .iter()
                .map(|(_, bytes)| bytes.clone())
                .collect::<Vec<_>>();
            if payloads.len() >= 2 {
                assert_eq!(
                    payloads,
                    vec![vec![0x90, 60, 100], vec![0x80, 60, 0]],
                    "the committed batch did not finish intact"
                );
                break;
            }
            assert!(
                Instant::now() < deadline,
                "paired note-off never reached the port: {payloads:?}"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    /// A cutover reports a note of its generation from the takeover frame on
    /// whose note-on has gone out. It keeps the note-off of that note, and
    /// reports no note of an earlier onset or of another generation.
    #[test]
    fn cutover_reports_the_started_notes_it_keeps_from_the_takeover_frame() {
        let capture = CapturePort::new();
        let sender = MidiSender::with_port(Box::new(capture.clone()), "started-notes".into());
        let now = Instant::now();
        let later = now + Duration::from_secs(30);
        let note = |key, on_due| {
            [
                (on_due, MidiMessage::note_on(1, key, 100)),
                (later, MidiMessage::note_off(1, key)),
            ]
        };
        assert!(sender.send_batch_for_generation_at(7, 99, &note(60, now)));
        assert!(sender.send_batch_for_generation_at(7, 100, &note(61, now)));
        assert!(sender.send_batch_for_generation_at(8, 100, &note(62, now)));
        assert!(sender.send_batch_for_generation_at(7, 101, &note(63, later)));
        let deadline = Instant::now() + Duration::from_secs(2);
        while capture.messages().len() < 3 {
            assert!(Instant::now() < deadline, "the note-ons never went out");
            std::thread::sleep(Duration::from_millis(1));
        }

        let (removed, started) = sender.prune_generation_reporting_started(7, 100);
        assert_eq!(removed, 2, "the batch that has not started goes whole");
        assert_eq!(
            started,
            [StartedNote {
                onset_frame: 100,
                note_off: MidiMessage::note_off(1, 61),
            }]
        );
        let (lock, _) = &*sender.queue;
        let guard = lock.lock().unwrap_or_else(|error| error.into_inner());
        let mut queued = guard
            .heap
            .iter()
            .map(|queued| queued.0.message)
            .collect::<Vec<_>>();
        queued.sort_by_key(|message| message.as_slice().to_vec());
        assert_eq!(
            queued,
            [60, 61, 62].map(|key| MidiMessage::note_off(1, key)),
            "each note that sounds keeps its note-off"
        );
    }

    #[test]
    fn cutover_frees_queue_capacity_without_rewriting_refusal_history() {
        let sender = MidiSender::with_port(Box::new(CapturePort::new()), "full-cutover".into());
        let due = Instant::now() + Duration::from_secs(30);
        let stale = vec![(due, MidiMessage::clock()); MAX_QUEUED_MESSAGES];
        assert!(sender.send_batch_for_generation_at(1, 100, &stale));
        assert!(!sender.send_batch_for_generation_at(2, 100, &[(due, MidiMessage::cont())]));
        assert_eq!(sender.report().refused_full, 1);

        assert_eq!(sender.prune_generation_from(1, 100), MAX_QUEUED_MESSAGES);
        assert!(sender.send_batch_for_generation_at(2, 100, &[(due, MidiMessage::cont())]));
        assert_eq!(sender.report().refused_full, 1);
    }

    #[test]
    fn a_full_queue_cannot_refuse_shutdown_silence() {
        let capture = CapturePort::new();
        let sender = MidiSender::with_port(Box::new(capture.clone()), "capture".into());
        let due = Instant::now() + Duration::from_secs(30);
        for _ in 0..MAX_QUEUED_MESSAGES {
            assert!(sender.send_at(due, MidiMessage::note_on(1, 60, 100)));
        }

        let completion = sender.completion();
        drop(sender);
        wait_for_completion(&completion);
        let payloads: Vec<Vec<u8>> = capture
            .messages()
            .into_iter()
            .map(|(_, payload)| payload)
            .collect();
        assert_eq!(
            payloads.len(),
            32,
            "shutdown did not replace the full queue"
        );
        for channel in 0..16u8 {
            for controller in [123, 120] {
                assert!(
                    payloads.contains(&vec![0xb0 | channel, controller, 0]),
                    "channel {} missed shutdown CC {controller}: {payloads:?}",
                    channel + 1
                );
            }
        }
    }

    /// Shutting down must not wait for queued notes.
    ///
    /// It used to: the drain thread exited only once the queue was EMPTY, so
    /// dropping the sender blocked until the furthest-out message came due.
    /// With a long release that is many seconds of a terminal ignoring Ctrl-C.
    #[test]
    fn shutdown_does_not_wait_for_queued_messages() {
        let capture = CapturePort::new();
        let sender = MidiSender::with_port(Box::new(capture.clone()), "capture".into());
        sender.send_at(
            Instant::now() + Duration::from_secs(30),
            MidiMessage::note_off(1, 60),
        );

        let completion = sender.completion();
        let start = Instant::now();
        drop(sender);
        let took = start.elapsed();
        assert!(
            took < Duration::from_millis(100),
            "drop waited {took:?} for a message due in 30s"
        );
        wait_for_completion(&completion);
        // The all-notes-off still goes out; the future note-off does not.
        let sent = capture.messages();
        assert!(
            sent.iter().any(|(_, bytes)| bytes[1] == 123),
            "all-notes-off was not sent on shutdown: {sent:?}"
        );
        assert!(
            !sent.iter().any(|(_, bytes)| bytes[0] == 0x80),
            "a note-off due in 30s should have been discarded: {sent:?}"
        );
    }

    struct BlockingFirstSendPort {
        log: Arc<Mutex<Vec<CapturedMessage>>>,
        entered: Option<std::sync::mpsc::SyncSender<()>>,
        release: std::sync::mpsc::Receiver<()>,
    }

    impl MidiPort for BlockingFirstSendPort {
        fn send(&mut self, bytes: &[u8]) -> Result<(), String> {
            if let Some(entered) = self.entered.take() {
                entered
                    .send(())
                    .map_err(|_| "blocking test observer disappeared".to_string())?;
                self.release
                    .recv_timeout(Duration::from_secs(2))
                    .map_err(|_| "blocking test release disappeared".to_string())?;
            }
            self.log
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .push((Instant::now(), bytes.to_vec()));
            Ok(())
        }
    }

    #[test]
    fn controller_progress_does_not_commit_a_future_note_across_cutover() {
        let log = Arc::new(Mutex::new(Vec::new()));
        let (entered_tx, entered_rx) = std::sync::mpsc::sync_channel(0);
        let (release_tx, release_rx) = std::sync::mpsc::sync_channel(0);
        let sender = MidiSender::with_port(
            Box::new(BlockingFirstSendPort {
                log: Arc::clone(&log),
                entered: Some(entered_tx),
                release: release_rx,
            }),
            "controller-cutover".into(),
        );
        let now = Instant::now();
        assert!(sender.send_batch_for_generation_at(
            7,
            100,
            &[
                (now, MidiMessage::control_change(1, 74, 64)),
                (now, MidiMessage::note_on(1, 60, 100)),
                (
                    now + Duration::from_millis(40),
                    MidiMessage::note_off(1, 60),
                ),
            ],
        ));

        entered_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("worker never reached the controller barrier");
        let removed = sender.prune_generation_from(7, 100);
        release_tx.send(()).expect("worker abandoned the barrier");
        assert_eq!(
            removed, 2,
            "a controller message committed a still-cancellable note batch"
        );

        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            let payloads = log
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .iter()
                .map(|(_, bytes)| bytes.clone())
                .collect::<Vec<_>>();
            if !payloads.is_empty() {
                assert_eq!(payloads, vec![vec![0xb0, 74, 64]]);
                break;
            }
            assert!(
                Instant::now() < deadline,
                "controller send never left the barrier"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
        std::thread::sleep(Duration::from_millis(80));
        let payloads = log
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .iter()
            .map(|(_, bytes)| bytes.clone())
            .collect::<Vec<_>>();
        assert_eq!(payloads, vec![vec![0xb0, 74, 64]]);
    }

    #[test]
    fn rapid_transitions_expire_each_blocked_generation_at_its_own_frame() {
        let log = Arc::new(Mutex::new(Vec::new()));
        let (entered_tx, entered_rx) = std::sync::mpsc::sync_channel(0);
        let (release_tx, release_rx) = std::sync::mpsc::sync_channel(0);
        let sender = MidiSender::with_port(
            Box::new(BlockingFirstSendPort {
                log: Arc::clone(&log),
                entered: Some(entered_tx),
                release: release_rx,
            }),
            "rapid-expiry".into(),
        );
        let now = Instant::now();
        assert!(sender.send_at(now, MidiMessage::clock()));
        entered_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("worker never reached the blocking host message");
        assert!(sender.send_batch_for_generation_at(
            1,
            99,
            &[
                (now, MidiMessage::note_on(1, 60, 100)),
                (now, MidiMessage::note_off(1, 60)),
            ],
        ));
        assert!(sender.send_batch_for_generation_at(
            2,
            199,
            &[
                (now, MidiMessage::note_on(1, 61, 100)),
                (now, MidiMessage::note_off(1, 61)),
            ],
        ));

        assert_eq!(sender.prune_generation_from(1, 100), 0);
        assert_eq!(sender.prune_generation_from(2, 200), 0);
        assert_eq!(sender.prune_expired_at(99), 0);
        assert_eq!(
            sender.prune_expired_at(100),
            2,
            "generation one inherited generation two's later deadline"
        );
        release_tx.send(()).expect("worker abandoned the barrier");

        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            let payloads = log
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .iter()
                .map(|(_, bytes)| bytes.clone())
                .collect::<Vec<_>>();
            if payloads.len() >= 3 {
                assert_eq!(
                    payloads,
                    vec![vec![0xf8], vec![0x90, 61, 100], vec![0x80, 61, 0],]
                );
                break;
            }
            assert!(
                Instant::now() < deadline,
                "surviving generation did not flush: {payloads:?}"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn blocked_drop_is_nonblocking_eventually_silences_and_holds_its_worker_reservation() {
        let budget = Arc::new(MidiWorkerBudget::new(1));
        let log = Arc::new(Mutex::new(Vec::new()));
        let (entered_tx, entered_rx) = std::sync::mpsc::sync_channel(0);
        let (release_tx, release_rx) = std::sync::mpsc::sync_channel(0);
        let sender = MidiSender::try_with_port_and_budget(
            Box::new(BlockingFirstSendPort {
                log: Arc::clone(&log),
                entered: Some(entered_tx),
                release: release_rx,
            }),
            "blocked".into(),
            Arc::clone(&budget),
        )
        .expect("first reservation");
        assert!(sender.send_at(Instant::now(), MidiMessage::clock()));
        entered_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("sender never entered blocking port");

        let completion = sender.completion();
        let started = Instant::now();
        drop(sender);
        assert!(
            started.elapsed() < Duration::from_millis(100),
            "Drop waited for a blocked platform send"
        );
        assert!(!completion.is_finished());
        assert!(
            MidiSender::try_with_port_and_budget(
                Box::new(CapturePort::new()),
                "over-cap".into(),
                Arc::clone(&budget),
            )
            .is_err(),
            "a retiring blocked worker released its reservation early"
        );

        release_tx.send(()).expect("release blocked send");
        wait_for_completion(&completion);
        let payloads = log
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .iter()
            .map(|(_, payload)| payload.clone())
            .collect::<Vec<_>>();
        assert_eq!(payloads.first(), Some(&vec![0xf8]));
        assert_eq!(payloads.len(), 33, "emergency silence was incomplete");
        assert!(payloads[1..].iter().all(|payload| {
            payload.len() == 3
                && payload[0] & 0xf0 == 0xb0
                && matches!(payload[1], 120 | 123)
                && payload[2] == 0
        }));

        let replacement = MidiSender::try_with_port_and_budget(
            Box::new(CapturePort::new()),
            "replacement".into(),
            budget,
        )
        .expect("completed retirement must release its reservation");
        let replacement_completion = replacement.completion();
        drop(replacement);
        wait_for_completion(&replacement_completion);
    }

    fn note(n: f64) -> MidiControls {
        MidiControls {
            note: Some(n),
            ..Default::default()
        }
    }

    #[test]
    fn a_note_becomes_an_on_now_and_an_off_just_before_the_gate_ends() {
        let plan = plan(&note(60.0), 1.0);
        assert_eq!(plan.len(), 2, "{plan:?}");
        assert_eq!(plan[0].0, 0.0);
        assert_eq!(plan[0].1, MidiMessage::note_on(1, 60, velocity_byte(0.9)));
        // Shortened by noteOffsetMs so the off precedes the next on.
        assert!((plan[1].0 - 0.999).abs() < 1e-9, "{:?}", plan[1].0);
        assert_eq!(plan[1].1, MidiMessage::note_off(1, 60));
    }

    /// `offset = Math.min(noteOffsetMs, hapDuration / 2)` - a very short note
    /// keeps half its length instead of going negative.
    #[test]
    fn a_very_short_note_keeps_half_its_gate_rather_than_going_negative() {
        let plan = plan(&note(60.0), 0.001);
        assert!(plan[1].0 > 0.0, "note-off went backwards: {:?}", plan[1].0);
        assert!((plan[1].0 - 0.0005).abs() < 1e-9, "{:?}", plan[1].0);
    }

    #[test]
    fn controller_mode_suppresses_notes_but_keeps_control_messages() {
        let controls = MidiControls {
            note: Some(60.0),
            ccn: Some(74.0),
            ccv: Some(0.5),
            is_controller: true,
            ..Default::default()
        };
        let messages = plan(&controls, 1.0);
        assert!(
            messages
                .iter()
                .all(|(_, message)| !matches!(message.as_slice()[0] & 0xf0, 0x80 | 0x90))
        );
        assert!(
            messages
                .iter()
                .any(|(_, message)| message.as_slice() == [0xb0, 74, 64])
        );
    }

    #[test]
    fn configured_note_offset_is_bounded_by_half_the_gate() {
        let configured = MidiControls {
            note: Some(60.0),
            note_offset_ms: Some(10.0),
            ..Default::default()
        };
        assert!((plan(&configured, 1.0)[1].0 - 0.99).abs() < 1e-9);

        let huge = MidiControls {
            note: Some(60.0),
            note_offset_ms: Some(1_000.0),
            ..Default::default()
        };
        assert!((plan(&huge, 0.02)[1].0 - 0.01).abs() < 1e-9);
    }

    #[test]
    fn gain_multiplies_velocity_the_way_strudel_does() {
        let controls = MidiControls {
            note: Some(60.0),
            gain: Some(0.5),
            velocity: Some(0.8),
            ..Default::default()
        };
        // velocity = gain * velocity = 0.4 -> round(0.4 * 127) = 51
        assert_eq!(plan(&controls, 1.0)[0].1, MidiMessage::note_on(1, 60, 51));
    }

    #[test]
    fn a_hap_with_no_note_sends_no_note_which_is_how_cc_only_patterns_work() {
        let controls = MidiControls {
            ccn: Some(74.0),
            ccv: Some(0.5),
            midichan: Some(3.0),
            ..Default::default()
        };
        let plan = plan(&controls, 1.0);
        assert_eq!(plan.len(), 1, "{plan:?}");
        assert_eq!(plan[0].1, MidiMessage::control_change(3, 74, 64));
    }

    #[test]
    fn an_explicit_cc_overrides_a_map_targeting_the_same_controller() {
        let controls = MidiControls {
            ccn: Some(74.0),
            ccv: Some(0.8),
            mapped_ccs: vec![(74, 0.2)],
            ..Default::default()
        };
        let messages: Vec<MidiMessage> = plan(&controls, 1.0)
            .into_iter()
            .map(|(_, message)| message)
            .collect();
        assert_eq!(
            messages,
            vec![
                MidiMessage::control_change(1, 74, unit_byte(0.2)),
                MidiMessage::control_change(1, 74, unit_byte(0.8)),
            ]
        );
    }

    /// Upstream sends a CC only when BOTH ccn and ccv are present.
    #[test]
    fn a_half_specified_control_change_sends_nothing() {
        for controls in [
            MidiControls {
                ccn: Some(74.0),
                ..Default::default()
            },
            MidiControls {
                ccv: Some(0.5),
                ..Default::default()
            },
        ] {
            assert!(plan(&controls, 1.0).is_empty(), "{controls:?}");
        }
    }

    #[test]
    fn a_fractional_controller_number_is_not_rounded_to_other_hardware() {
        let controls = MidiControls {
            ccn: Some(74.9),
            ccv: Some(0.5),
            ..Default::default()
        };
        assert!(plan(&controls, 1.0).is_empty());
    }

    #[test]
    fn output_probe_rejects_empty_and_invalid_intents() {
        for controls in [
            MidiControls::default(),
            MidiControls {
                note: Some(999.0),
                ..Default::default()
            },
            MidiControls {
                ccn: Some(74.0),
                ..Default::default()
            },
            MidiControls {
                midicmd: Some("not-a-command".into()),
                ..Default::default()
            },
        ] {
            assert!(!has_output(&controls), "{controls:?}");
            assert!(plan(&controls, 1.0).is_empty(), "{controls:?}");
        }
    }

    #[test]
    fn output_probe_accepts_a_note_producing_intent() {
        let controls = note(60.0);
        assert!(has_output(&controls));
        assert_eq!(plan(&controls, 1.0).len(), 2);
    }

    #[test]
    fn transport_commands_map_to_their_realtime_bytes() {
        for (command, expected) in [
            ("clock", MidiMessage::clock()),
            ("midiClock", MidiMessage::clock()),
            ("start", MidiMessage::start()),
            ("stop", MidiMessage::stop()),
            ("continue", MidiMessage::cont()),
        ] {
            let controls = MidiControls {
                midicmd: Some(command.into()),
                ..Default::default()
            };
            assert_eq!(plan(&controls, 1.0), vec![(0.0, expected)], "{command}");
        }
        // An unknown command is ignored, not an error: a typo in a live set
        // must not stop the music.
        let unknown = MidiControls {
            midicmd: Some("wat".into()),
            ..Default::default()
        };
        assert!(plan(&unknown, 1.0).is_empty());
    }

    /// Nothing in a hap may produce a message that cannot be encoded, however
    /// strange the score gets.
    #[test]
    fn hostile_values_produce_no_messages_rather_than_bad_bytes() {
        for value in [f64::NAN, f64::INFINITY, -1.0, 1e308, 128.0] {
            let plan = plan(&note(value), 1.0);
            assert!(plan.is_empty(), "note({value}) emitted {plan:?}");
        }
        // Out-of-range channel clamps instead of leaking into another
        // channel's status byte.
        let controls = MidiControls {
            note: Some(60.0),
            midichan: Some(9999.0),
            ..Default::default()
        };
        assert_eq!(plan(&controls, 1.0)[0].1.as_slice()[0], 0x9f);
        // Every emitted byte after the status byte is a legal 7-bit value.
        let wild = MidiControls {
            note: Some(60.0),
            gain: Some(f64::INFINITY),
            ccn: Some(74.0),
            ccv: Some(f64::NAN),
            midibend: Some(1e308),
            miditouch: Some(-5.0),
            prog_num: Some(1e9),
            ..Default::default()
        };
        for (_, message) in plan(&wild, 1.0) {
            for byte in &message.as_slice()[1..] {
                assert!(*byte < 128, "non-7-bit data byte {byte} in {message:?}");
            }
        }
    }
}
