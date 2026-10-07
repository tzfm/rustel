//! Latency-compensated voices, and OSC and serial messages, across a
//! generation takeover.
//!
//! A voice whose effect has a latency starts before its onset: the voice
//! resolver aims a stretched voice a vocoder's latency early, so the shifted
//! sound lands on the onset. Such an audio event has two frames, the frame
//! of its onset and the earlier target frame the device starts it on. For
//! every other voice the two are one.
//!
//! A takeover reads both. The session gives an onset to the outgoing or the
//! incoming generation by the frame of the onset (see
//! `Session::takeover_frame_edge`). The device keeps or drops an outgoing
//! event by its target frame, and has no other choice: a voice that has
//! started is sounding. So with the onset on the takeover frame or after it
//! and the target frame before it, the device keeps the outgoing copy and
//! the incoming generation plays the onset again. Two copies sound, and the
//! new one is late wherever its target frame is already rendered.
//!
//! The copy the device holds is the one that plays. This ledger follows the
//! compensated events converted for the device and what each takeover leaves
//! of them, and names the incoming generation's events to leave out: one for
//! each outgoing copy the device keeps at the same cycle position.
//!
//! The incoming event can be a plain one: a save removed the effect. A
//! plain event of another lane can be at the same position, and the device
//! dropped that lane's outgoing copy. So a copy stands for a plain event
//! only when its value is the copy's without the effect. When the save
//! also changed the rest of the value, both sound. The ledger cannot tell
//! that event from another lane's event of the same value: when a save
//! removes the stretched one of two such lanes, the copy stands for the
//! event of the lane that stays.
//!
//! A limit: a copy stands only for an event at its own cycle position
//! whose onset is nearer to its own than its compensation is long. A save
//! that moves the onset further, with a large tempo change or `.late()`,
//! leaves the copy and the incoming event to sound both: the ledger cannot
//! tell a moved onset from another onset.
//!
//! Only the audio event is left out for a copy the device holds. MIDI hands
//! over on the onset's own frame: its bridge prunes the outgoing copy from
//! there, and the incoming generation's copy is the one sent.
//!
//! OSC and serial hand over later, on a frontier. A host sends an OSC bundle
//! at once, with the time of its onset, and queues a serial write for that
//! time. A takeover cannot recall either, and the outgoing generation has
//! handed its onsets out one schedule cover ahead, about half a second. So
//! the message that is out is the one that plays. [`HandedOut`] follows the
//! latest onset a host handed out, and the session leaves out the incoming
//! intent of each onset on that frame or before it.
//!
//! The frontier moves only when a host tells the session of a message it
//! handed out. A window that is staged and then rolled back does not move
//! it, nor does one that a reload shield holds and a takeover cuts.
//!
//! A key press is the exception. A press of `midikeys` has no onset until a
//! window places it, on the begin of that window, and the requery that a
//! live host makes for the press begins before the frontier. No message is
//! out for that press, so the intents of its onset go out
//! ([`PlacedPresses`]). They do not move the frontier: the later windows of
//! that generation can still have onsets before it.
//!
//! Four limits. A steady window can place a press before the requery runs,
//! one schedule cover ahead. Its message is then out for that time, and
//! stays there when the requery moves the sound to the takeover. When the
//! requery places that press again beside a new one, the note of an onset
//! shows which press it plays. A score that changes the notes of the keys,
//! or a new press with the same note as the old one, leaves the new press
//! to the frontier. Only the onset on the cycle
//! position of a press is known as the press's: a copy that the score moves
//! in time follows the frontier. And a window counts a press as placed also
//! when the host then hands out none of its messages (a save that is rolled
//! back, a failed step): that press follows the frontier from then on.
//!
//! An edit, a moved slider or a clock steer therefore reaches OSC and serial
//! from the frontier on, up to one schedule cover after it reaches the
//! audio. An onset that the edit adds before the frontier is not sent, and
//! one that it removes there is out already.
//!
//! The frontier is a time, not a cycle position. A clock steer or a changed
//! tempo moves the onsets in time: the receiver has the old timing up to the
//! frontier and the new timing after it. So an onset that moves across the
//! frontier is sent twice when it moves later, and not at all when it moves
//! earlier.
//!
//! A takeover with a cut, a transport start, the first window of a device
//! and the refill of an output that failed each begin a new timeline. All
//! their messages go out, and the frontier starts again. The messages of the
//! outgoing score that are already sent still play: OSC and serial have no
//! cut. After an output failure they play beside the refill's own.
//!
//! The ledger assumes that a window it records reaches the device once its
//! generation is published. The scheduler assumes the same of an onset it
//! emitted (see `Scheduler::replace_pattern_continued`).

#[cfg(any(feature = "osc", feature = "serial"))]
use std::sync::{Arc, Weak};

use rustel_audio::TakeoverCut;
#[cfg(any(feature = "osc", feature = "serial"))]
use rustel_core::midi_in::{InputPort, PlacedKey};
#[cfg(any(feature = "osc", feature = "serial"))]
use rustel_fraction::Fraction;

use super::LiveQueryBudgetMode;
#[cfg(any(feature = "osc", feature = "serial"))]
use crate::OnsetEventJson;
use crate::ValueJson;
#[cfg(any(feature = "osc", feature = "serial"))]
use crate::render::onset_frame_at;

/// The control whose latency the voice resolver compensates.
const COMPENSATED_CONTROL: &str = "stretch";

/// How long an onset stays in the ledger once the clock has passed it, in
/// seconds. A control requery can publish a moment after its takeover, and
/// the onsets between the two are still the device's.
const RETAINED_PAST_SECS: u64 = 1;

/// `value` without the compensated control: the value of the same onset
/// once a save removes the effect.
fn without_compensated_control(value: &ValueJson) -> ValueJson {
    let mut value = value.clone();
    if let ValueJson::Raw(serde_json::Value::Object(controls)) = &mut value {
        controls.remove(COMPENSATED_CONTROL);
    }
    value
}

/// Remove the elements of `items` at `indices`, which ascend.
pub(super) fn remove_ascending<T>(items: &mut Vec<T>, indices: &[usize]) {
    let mut removed = indices.iter().copied().peekable();
    let mut index = 0;
    items.retain(|_| {
        let remove = removed.next_if_eq(&index).is_some();
        index += 1;
        !remove
    });
}

/// One audio event whose target frame is before the frame of its onset.
struct CompensatedOnset {
    generation: u64,
    /// The onset's cycle position, as the scheduler spells it. It names the
    /// same onset in every generation, whatever its time there.
    whole_begin: String,
    /// The value as text. It shows whether the next generation changed the
    /// onset or left it as it was.
    value: String,
    /// The value without the compensated control. A plain event with this
    /// value is the same onset with the effect removed.
    plain_value: ValueJson,
    /// The frame the device starts the voice on.
    target_frame: u64,
    /// The frame of the onset itself.
    onset_frame: u64,
}

/// An event of the window being converted: a compensated one, or a plain
/// one, whose target frame is the frame of its onset.
pub(super) struct ConvertedOnset<'a> {
    /// Its place among the window's converted events.
    pub(super) event: usize,
    pub(super) whole_begin: &'a str,
    pub(super) value: &'a str,
    /// The value as the voice resolver reads it.
    pub(super) controls: &'a ValueJson,
    pub(super) target_frame: u64,
    pub(super) onset_frame: u64,
}

struct HeldOnset {
    onset: CompensatedOnset,
    /// The generation that left its own event for this onset out. Each copy
    /// stands for one event of a generation, and a later generation meets it
    /// afresh.
    stands_for: Option<u64>,
}

impl HeldOnset {
    /// Whether the device keeps this copy across the takeover of
    /// `generation` on `takeover_frame`, and it is free to stand for
    /// `event`: an event of that generation at its cycle position, with the
    /// onset nearer to its own than its compensation is long.
    fn can_stand_for(
        &self,
        generation: u64,
        takeover_frame: u64,
        event: &ConvertedOnset<'_>,
    ) -> bool {
        let copy = &self.onset;
        copy.generation < generation
            && self.stands_for != Some(generation)
            && copy.target_frame < takeover_frame
            && takeover_frame <= copy.onset_frame
            && copy.whole_begin == event.whole_begin
            && copy.onset_frame.abs_diff(event.onset_frame) < copy.onset_frame - copy.target_frame
    }
}

/// The compensated audio events a live device can hold.
#[derive(Default)]
pub(super) struct CompensatedOnsets {
    /// Events converted for the generations the device has played.
    held: Vec<HeldOnset>,
    /// Events of the first window of a generation that is not published yet.
    /// Its batch reaches the device only with the publication.
    unpublished: Vec<CompensatedOnset>,
    /// The generation the device plays and the frame it took over on, when
    /// the outgoing generations rang out there. `None` after a cut.
    takeover: Option<(u64, u64)>,
    /// The takeover frame the unpublished generation's window was read
    /// against, for the publication to check.
    unpublished_takeover: Option<(u64, Option<u64>)>,
}

impl CompensatedOnsets {
    /// Forget everything: the device holds nothing a later takeover can
    /// meet. Used at a transport start and when an output discards its ring.
    pub(super) fn clear(&mut self) {
        *self = Self::default();
    }

    /// Open the conversion of one window of `generation` at `now_frame`.
    /// Return the takeover frame for its compensated events: the device
    /// keeps the outgoing generations' events before that frame. Return
    /// `None` when the device keeps none of them.
    ///
    /// `pending_takeover` is that frame for a generation still to publish.
    /// It is `None` when the takeover cuts the outgoing audio, or names no
    /// time and so drops all of it.
    pub(super) fn begin_window(
        &mut self,
        generation: u64,
        mode: LiveQueryBudgetMode,
        now_frame: u64,
        sample_rate: u32,
        pending_takeover: Option<u64>,
    ) -> Option<u64> {
        // A window converted for an earlier unpublished generation, or an
        // earlier try of this one, never reached the device.
        self.unpublished.clear();
        self.unpublished_takeover = None;
        let retained = RETAINED_PAST_SECS * u64::from(sample_rate);
        self.held
            .retain(|held| held.onset.onset_frame.saturating_add(retained) >= now_frame);
        match mode {
            // The device plays its first generation: nothing is older.
            LiveQueryBudgetMode::InitialPrefill => {
                self.clear();
                None
            }
            LiveQueryBudgetMode::ReplacementPrefill => {
                self.unpublished_takeover = Some((generation, pending_takeover));
                pending_takeover
            }
            LiveQueryBudgetMode::Steady => self
                .takeover
                .filter(|(playing, _)| *playing == generation)
                .map(|(_, frame)| frame),
        }
    }

    /// Mark the events of `window`, the compensated events of one window of
    /// `generation` in conversion order, to leave out: the device keeps an
    /// outgoing copy of their onset across the takeover on `takeover_frame`.
    ///
    /// A copy is kept when its target frame is before the takeover frame,
    /// and the incoming generation plays its onset again when the onset is
    /// on that frame or later. It stands for one event of the window at its cycle
    /// position. An event with the same value is paired first, so that an
    /// onset the generation left as it was is the one left out. A copy that
    /// remains is paired with an event of another value at the position: a
    /// control requery changed the value, and the voice the device holds
    /// plays the onset as it was.
    ///
    /// A cycle position can come again at another time, once an outside
    /// clock moves the cycle back. So a copy stands only for an event whose
    /// onset is nearer to its own than its compensation is long: the same
    /// onset, moved at most by a re-anchored mapping or a changed tempo.
    pub(super) fn claim_outgoing_copies(
        &mut self,
        generation: u64,
        takeover_frame: u64,
        window: &[ConvertedOnset<'_>],
    ) -> Vec<bool> {
        let mut left_out = vec![false; window.len()];
        for same_value in [true, false] {
            for (event, left_out) in window.iter().zip(&mut left_out) {
                if *left_out {
                    continue;
                }
                let copy = self.held.iter_mut().find(|held| {
                    held.can_stand_for(generation, takeover_frame, event)
                        && (!same_value || held.onset.value == event.value)
                });
                if let Some(copy) = copy {
                    copy.stands_for = Some(generation);
                    *left_out = true;
                }
            }
        }
        left_out
    }

    /// Mark the events of `window`, the plain events of one window of
    /// `generation`, to leave out. [`Self::claim_outgoing_copies`] runs
    /// first for the same window: a compensated event has the first claim
    /// on a copy.
    ///
    /// A copy that remains stands for one plain event at its cycle position
    /// whose value is its own without the compensated control: a save
    /// removed the effect. A plain event of another value is another
    /// lane's. The device dropped that lane's outgoing copy, so it plays.
    pub(super) fn claim_outgoing_copies_for_plain(
        &mut self,
        generation: u64,
        takeover_frame: u64,
        window: &[ConvertedOnset<'_>],
    ) -> Vec<bool> {
        let mut left_out = vec![false; window.len()];
        for (event, left_out) in window.iter().zip(&mut left_out) {
            let copy = self.held.iter_mut().find(|held| {
                held.can_stand_for(generation, takeover_frame, event)
                    && held.onset.plain_value == *event.controls
            });
            if let Some(copy) = copy {
                copy.stands_for = Some(generation);
                *left_out = true;
            }
        }
        left_out
    }

    /// Record `window`: the compensated events that the window just
    /// converted for `generation` sends to the producer.
    pub(super) fn record(
        &mut self,
        generation: u64,
        mode: LiveQueryBudgetMode,
        window: &[ConvertedOnset<'_>],
    ) {
        let onsets = window.iter().map(|event| CompensatedOnset {
            generation,
            whole_begin: event.whole_begin.to_owned(),
            value: event.value.to_owned(),
            plain_value: without_compensated_control(event.controls),
            target_frame: event.target_frame,
            onset_frame: event.onset_frame,
        });
        match mode {
            LiveQueryBudgetMode::ReplacementPrefill => self.unpublished.extend(onsets),
            LiveQueryBudgetMode::InitialPrefill | LiveQueryBudgetMode::Steady => {
                self.held.extend(onsets.map(|onset| HeldOnset {
                    onset,
                    stands_for: None,
                }));
            }
        }
    }

    /// The producer published `generation`: do to the ledger what the flip
    /// does to the device. The device keeps the outgoing generations' events
    /// aimed before `takeover_frame`, or none when the takeover cuts them.
    /// The first window of `generation` now goes to the device.
    pub(super) fn published(&mut self, generation: u64, takeover_frame: u64, cut: TakeoverCut) {
        let rings_out = cut == TakeoverCut::None;
        // The first window was converted one step before this publication,
        // against the takeover the session held for it then.
        debug_assert!(
            self.unpublished_takeover
                .is_none_or(|(unpublished, frame)| unpublished != generation
                    || frame.unwrap_or(0) == if rings_out { takeover_frame } else { 0 }),
            "generation {generation} published takeover frame {takeover_frame} ({cut:?}), \
             and its first window was read against {:?}",
            self.unpublished_takeover
        );
        self.held
            .retain(|held| rings_out && held.onset.target_frame < takeover_frame);
        self.held.extend(
            self.unpublished
                .drain(..)
                .filter(|onset| onset.generation == generation)
                .map(|onset| HeldOnset {
                    onset,
                    stands_for: None,
                }),
        );
        self.unpublished_takeover = None;
        self.takeover = rings_out.then_some((generation, takeover_frame));
    }
}

/// How far a host has handed out the messages of one kind, OSC or serial.
#[cfg(any(feature = "osc", feature = "serial"))]
#[derive(Default)]
pub(super) struct HandedOut {
    /// The newest generation that handed out past the messages of the
    /// generations before it, and the latest onset with a message handed
    /// out, in seconds on the session clock.
    frontier: Option<(u64, f64)>,
}

#[cfg(any(feature = "osc", feature = "serial"))]
impl HandedOut {
    /// Start again: no message is out on a new timeline.
    pub(super) fn clear(&mut self) {
        self.frontier = None;
    }

    /// A host handed out a message of `generation` for the onset at
    /// `target_time`, in seconds on the session clock.
    ///
    /// A message on the frontier or before it does not move the frontier:
    /// it is a late message of an earlier generation, or the message of a
    /// key press (see [`PlacedPresses`]). The later windows of its
    /// generation can still have onsets before the frontier, and the
    /// messages that are out stand for those.
    pub(super) fn note(&mut self, generation: u64, target_time: f64) {
        if !target_time.is_finite() {
            return;
        }
        self.frontier = Some(match self.frontier {
            Some((newest, latest)) if target_time <= latest => (newest, latest),
            Some((newest, _)) => (newest.max(generation), target_time),
            None => (generation, target_time),
        });
    }

    /// Whether a message that is out stands for the intent of `generation`
    /// for the onset at `target_time`: an earlier generation handed out the
    /// onsets up to the frontier, and this onset is on the frontier's frame
    /// or before it.
    ///
    /// The intents of the newest generation that handed out past the
    /// frontier always go out. Its windows follow one another in time, and
    /// two of its onsets can share a frame across two windows.
    pub(super) fn stands_for(&self, generation: u64, target_time: f64, sample_rate: u32) -> bool {
        self.frontier.is_some_and(|(newest, latest)| {
            newest < generation
                && sample_rate > 0
                && onset_frame_at(target_time, sample_rate) <= onset_frame_at(latest, sample_rate)
        })
    }
}

/// The key presses that the live windows have placed.
///
/// A press of `midikeys` has no onset until a window places it, on the
/// begin of that window. So no message is out for a press that a window is
/// the first to place, wherever the frontier is, and the intents of its
/// onset always go out.
#[cfg(any(feature = "osc", feature = "serial"))]
#[derive(Default)]
pub(super) struct PlacedPresses {
    /// For each keyboard, the latest press that a window with a message to
    /// hand out had placed (see [`PlacedKey::press`]). A later press is one
    /// that no such window placed (see `KeyRing::placed`).
    latest: Vec<(Weak<InputPort>, u64)>,
}

#[cfg(any(feature = "osc", feature = "serial"))]
impl PlacedPresses {
    /// Return the ids of the onsets of `onsets`, one window, that play a
    /// press of `placed` that this window is the first to place. Then count
    /// every press of `placed` as placed.
    ///
    /// The onset of a press is on the cycle position of the press. A press
    /// that an earlier window placed can be there too, when a requery
    /// forgot its place and this window placed it again. A message can be
    /// out for that press, so the note of an onset then shows which press
    /// it plays ([`onsets_of_new_presses`]).
    pub(super) fn claim_new(
        &mut self,
        placed: &[(Arc<InputPort>, Vec<PlacedKey>)],
        onsets: &[OnsetEventJson],
    ) -> Vec<u64> {
        self.latest.retain(|(port, _)| port.strong_count() > 0);
        // Each press, and whether this window is the first to place it.
        let mut presses: Vec<(&PlacedKey, bool)> = Vec::new();
        for (port, keys) in placed {
            let known = self
                .latest
                .iter()
                .position(|(known, _)| std::ptr::eq(known.as_ptr(), Arc::as_ptr(port)));
            let before = known.map_or(0, |known| self.latest[known].1);
            presses.extend(keys.iter().map(|key| (key, key.press > before)));
            let latest = keys.iter().map(|key| key.press).fold(before, u64::max);
            match known {
                Some(known) => self.latest[known].1 = latest,
                None => self.latest.push((Arc::downgrade(port), latest)),
            }
        }
        let position =
            |key: &PlacedKey| Fraction::checked_new(i128::from(key.num), i128::from(key.den));
        let mut new_onsets = Vec::new();
        let mut seen: Vec<Fraction> = Vec::new();
        for (key, _) in presses.iter().filter(|(_, new)| *new) {
            let Some(here) = position(key).filter(|here| !seen.contains(here)) else {
                continue;
            };
            seen.push(here);
            // The notes of the presses on this position: new, or not.
            let notes = |new: bool| -> Vec<u8> {
                let pressed = presses
                    .iter()
                    .filter(|(key, key_new)| *key_new == new && position(key) == Some(here));
                pressed.map(|(key, _)| key.note).collect()
            };
            let at = here.show();
            let onsets_here: Vec<&OnsetEventJson> = onsets
                .iter()
                .filter(|onset| onset.whole_begin == at)
                .collect();
            new_onsets.extend(onsets_of_new_presses(
                &notes(true),
                &notes(false),
                &onsets_here,
            ));
        }
        new_onsets
    }
}

/// Return the ids of the onsets of `onsets`, all on one cycle position, that
/// play a new press. `new` and `placed_before` are the notes of the presses
/// on that position that the window is the first to place, and of the
/// others.
///
/// With no press placed before, every onset is a new press's. With one, an
/// onset is a new press's when it plays the note of a new press and of no
/// other. When the notes of the onsets are not the notes of the presses, the
/// score changes the notes, and no onset is returned.
#[cfg(any(feature = "osc", feature = "serial"))]
fn onsets_of_new_presses(new: &[u8], placed_before: &[u8], onsets: &[&OnsetEventJson]) -> Vec<u64> {
    if placed_before.is_empty() {
        return onsets.iter().map(|onset| onset.onset_id).collect();
    }
    let played: Option<Vec<u8>> = onsets.iter().map(|onset| key_note(onset)).collect();
    let Some(mut played) = played else {
        return Vec::new();
    };
    let mut pressed = [new, placed_before].concat();
    played.sort_unstable();
    pressed.sort_unstable();
    // Each press plays one onset in each lane of the score that plays the
    // keyboard.
    let lanes = played.len() / pressed.len();
    let same_notes = lanes > 0
        && played.len() == lanes * pressed.len()
        && played
            .iter()
            .enumerate()
            .all(|(index, note)| *note == pressed[index / lanes]);
    if !same_notes {
        return Vec::new();
    }
    let new_onsets = onsets.iter().filter(|onset| {
        key_note(onset).is_some_and(|note| new.contains(&note) && !placed_before.contains(&note))
    });
    new_onsets.map(|onset| onset.onset_id).collect()
}

/// The MIDI note number in the value of `onset`, as `midikeys` writes it.
#[cfg(any(feature = "osc", feature = "serial"))]
fn key_note(onset: &OnsetEventJson) -> Option<u8> {
    let ValueJson::Raw(serde_json::Value::Object(controls)) = &onset.value else {
        return None;
    };
    let note = controls.get("note")?.as_f64()?;
    (note.fract() == 0.0 && (0.0..=127.0).contains(&note)).then_some(note as u8)
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: u32 = 48_000;
    /// The frame the takeovers below flip on.
    const TAKEOVER: u64 = 49_280;
    /// How early a stretched voice starts at 48 kHz: 40 ms and 127 frames.
    const COMPENSATION: u64 = 2_047;

    fn event<'a>(whole_begin: &'a str, value: &'a str, onset_frame: u64) -> ConvertedOnset<'a> {
        ConvertedOnset {
            event: 0,
            whole_begin,
            value,
            controls: &ValueJson::Null,
            target_frame: onset_frame - COMPENSATION,
            onset_frame,
        }
    }

    /// An event with `controls` as its value, aimed early when `stretched`.
    fn voice<'a>(
        whole_begin: &'a str,
        controls: &'a ValueJson,
        stretched: bool,
        onset_frame: u64,
    ) -> ConvertedOnset<'a> {
        ConvertedOnset {
            event: 0,
            whole_begin,
            value: "",
            controls,
            target_frame: onset_frame - if stretched { COMPENSATION } else { 0 },
            onset_frame,
        }
    }

    fn controls(value: serde_json::Value) -> ValueJson {
        ValueJson::Raw(value)
    }

    /// A ledger whose device plays generation 1 and holds `window` of it.
    fn playing(window: &[ConvertedOnset<'_>]) -> CompensatedOnsets {
        let mut ledger = CompensatedOnsets::default();
        assert_eq!(
            ledger.begin_window(1, LiveQueryBudgetMode::InitialPrefill, 0, RATE, None),
            None
        );
        ledger.record(1, LiveQueryBudgetMode::InitialPrefill, window);
        ledger
    }

    /// Open the first window of `generation`, to take over on `takeover`.
    fn taking_over(ledger: &mut CompensatedOnsets, generation: u64, now: u64, takeover: u64) {
        assert_eq!(
            ledger.begin_window(
                generation,
                LiveQueryBudgetMode::ReplacementPrefill,
                now,
                RATE,
                Some(takeover),
            ),
            Some(takeover)
        );
    }

    #[test]
    fn remove_ascending_keeps_the_other_elements_in_order() {
        let mut items = vec!['a', 'b', 'c', 'd', 'e'];
        remove_ascending(&mut items, &[0, 3]);
        assert_eq!(items, ['b', 'c', 'e']);
        remove_ascending(&mut items, &[]);
        assert_eq!(items, ['b', 'c', 'e']);
    }

    /// An incoming event is left out only when the outgoing copy is aimed
    /// before the takeover frame and its onset is on that frame or later.
    #[test]
    fn an_incoming_event_is_left_out_for_a_copy_that_straddles_the_takeover() {
        let window = [
            event("7/2", "pad", TAKEOVER - 1),
            event("4", "pad", TAKEOVER),
            event("17/4", "pad", TAKEOVER + COMPENSATION - 1),
            event("9/2", "pad", TAKEOVER + COMPENSATION),
        ];
        let mut ledger = playing(&window);
        taking_over(&mut ledger, 2, 45_440, TAKEOVER);
        assert_eq!(
            ledger.claim_outgoing_copies(2, TAKEOVER, &window),
            [false, true, true, false]
        );
    }

    /// One copy stands for one event of a generation. A chord has a copy
    /// for each note, and a note the incoming generation adds at the
    /// position plays: the event with the same value is paired first.
    #[test]
    fn a_copy_stands_for_one_event_and_pairs_with_its_own_value_first() {
        let mut ledger = playing(&[event("4", "e1", TAKEOVER), event("4", "g2", TAKEOVER)]);
        taking_over(&mut ledger, 2, 45_440, TAKEOVER);
        assert_eq!(
            ledger.claim_outgoing_copies(
                2,
                TAKEOVER,
                &[
                    event("4", "c3", TAKEOVER),
                    event("4", "g2", TAKEOVER),
                    event("4", "e1", TAKEOVER),
                    event("4", "e1", TAKEOVER),
                ]
            ),
            [false, true, true, false],
            "the added c3 and the second e1 have no copy on the device"
        );
        assert_eq!(
            ledger.claim_outgoing_copies(2, TAKEOVER, &[event("4", "e1", TAKEOVER)]),
            [false],
            "a copy stands for one event of a generation"
        );
    }

    /// A control requery can change the value of an onset the device
    /// already holds. The copy still stands for it: the voice plays as it
    /// was, once.
    #[test]
    fn a_copy_stands_for_its_onset_under_a_changed_value() {
        let mut ledger = playing(&[event("4", "pad lpf:800", TAKEOVER)]);
        taking_over(&mut ledger, 2, 49_024, TAKEOVER);
        assert_eq!(
            ledger.claim_outgoing_copies(
                2,
                TAKEOVER,
                &[
                    event("4", "pad lpf:900", TAKEOVER),
                    event("33/8", "pad lpf:900", TAKEOVER + 3_000),
                ]
            ),
            [true, false]
        );
    }

    /// A copy stands for an onset less than its compensation away: a
    /// re-anchored mapping or a changed tempo moved it. An onset further
    /// away is another onset: an outside clock moved the cycle back.
    #[test]
    fn a_copy_stands_only_for_an_onset_near_its_own() {
        let copy = TAKEOVER + 100;
        for (onset_frame, same_onset) in [
            (copy, true),
            (copy + 30, true),
            (copy - 30, true),
            (copy + COMPENSATION - 1, true),
            (copy - COMPENSATION + 1, true),
            (copy + COMPENSATION, false),
            (copy - COMPENSATION, false),
            (copy + 24_000, false),
        ] {
            let mut ledger = playing(&[event("4", "pad", copy)]);
            taking_over(&mut ledger, 2, 49_024, TAKEOVER);
            assert_eq!(
                ledger.claim_outgoing_copies(2, TAKEOVER, &[event("4", "pad", onset_frame)]),
                [same_onset],
                "an onset on frame {onset_frame}, with the copy's on {copy}"
            );
        }
    }

    /// A save removes the effect: the copy stands for the plain event of its
    /// value without the control, in any key order. It stands for one such
    /// event, and not for a plain event of another lane at the position.
    #[test]
    fn a_copy_stands_for_a_plain_event_of_its_value_without_the_effect() {
        let stretched = controls(serde_json::json!({"s": "saw", "lpf": 800, "stretch": 1}));
        let plain = controls(serde_json::json!({"lpf": 800, "s": "saw"}));
        let other_lane = controls(serde_json::json!({"s": "bd"}));
        let changed = controls(serde_json::json!({"s": "saw", "lpf": 900}));
        let mut ledger = playing(&[voice("4", &stretched, true, TAKEOVER)]);
        taking_over(&mut ledger, 2, 45_440, TAKEOVER);
        assert_eq!(
            ledger.claim_outgoing_copies_for_plain(
                2,
                TAKEOVER,
                &[
                    voice("4", &other_lane, false, TAKEOVER),
                    voice("4", &changed, false, TAKEOVER),
                    voice("17/4", &plain, false, TAKEOVER),
                    voice("4", &plain, false, TAKEOVER + COMPENSATION),
                    voice("4", &plain, false, TAKEOVER),
                    voice("4", &plain, false, TAKEOVER),
                ]
            ),
            [false, false, false, false, true, false]
        );
    }

    /// A compensated event has the first claim on a copy. The plain event
    /// at the position then has no copy on the device, and plays.
    #[test]
    fn a_plain_event_does_not_take_the_copy_of_a_compensated_event() {
        let stretched = controls(serde_json::json!({"s": "saw", "stretch": 1}));
        let plain = controls(serde_json::json!({"s": "saw"}));
        let mut ledger = playing(&[voice("4", &stretched, true, TAKEOVER)]);
        taking_over(&mut ledger, 2, 45_440, TAKEOVER);
        assert_eq!(
            ledger.claim_outgoing_copies(2, TAKEOVER, &[voice("4", &stretched, true, TAKEOVER)]),
            [true]
        );
        assert_eq!(
            ledger.claim_outgoing_copies_for_plain(
                2,
                TAKEOVER,
                &[voice("4", &plain, false, TAKEOVER)]
            ),
            [false]
        );
    }

    /// A takeover that cuts the outgoing audio, or names no frame and so
    /// drops all of it, leaves no copy to stand for the takeover after it.
    /// One that lets the outgoing audio ring leaves the copy aimed before
    /// its frame.
    #[test]
    fn a_takeover_that_keeps_no_outgoing_event_leaves_no_copy() {
        let first = TAKEOVER - 1_000;
        for (frame, cut, kept) in [
            (first, TakeoverCut::None, true),
            (first, TakeoverCut::AtFlip, false),
            (first, TakeoverCut::AtTakeover, false),
            (0, TakeoverCut::None, false),
        ] {
            let mut ledger = playing(&[event("4", "pad", TAKEOVER)]);
            let pending = (cut == TakeoverCut::None && frame != 0).then_some(frame);
            assert_eq!(
                ledger.begin_window(
                    2,
                    LiveQueryBudgetMode::ReplacementPrefill,
                    45_440,
                    RATE,
                    pending
                ),
                pending
            );
            ledger.published(2, frame, cut);
            taking_over(&mut ledger, 3, 49_024, TAKEOVER);
            assert_eq!(
                ledger.claim_outgoing_copies(3, TAKEOVER, &[event("4", "pad", TAKEOVER)]),
                [kept],
                "after a takeover on frame {frame} with {cut:?}"
            );
        }
    }

    /// The first window of a generation reaches the device with its
    /// publication. One that is never published was never there, so the
    /// generation after it does not meet its events.
    #[test]
    fn an_unpublished_window_is_not_on_the_device() {
        let later = [event("5", "pad", TAKEOVER + 6_000)];
        let mut ledger = playing(&[]);
        taking_over(&mut ledger, 2, 45_440, TAKEOVER);
        ledger.record(2, LiveQueryBudgetMode::ReplacementPrefill, &later);
        // Generation 2 is refused. Generation 3 takes over between the
        // target frame and the onset of that event.
        let third = TAKEOVER + 5_900;
        taking_over(&mut ledger, 3, 50_000, third);
        assert_eq!(ledger.claim_outgoing_copies(3, third, &later), [false]);
        ledger.record(3, LiveQueryBudgetMode::ReplacementPrefill, &later);
        ledger.published(3, third, TakeoverCut::None);
        // Generation 3 is published: generation 4 meets its event.
        let fourth = third + 50;
        taking_over(&mut ledger, 4, 50_100, fourth);
        assert_eq!(ledger.claim_outgoing_copies(4, fourth, &later), [true]);
    }

    /// The ledger drops what the flip drops: the outgoing events aimed at or
    /// after the takeover frame. A copy that stays stands again for the
    /// next generation, when that one also takes over before its onset.
    #[test]
    fn a_publication_leaves_the_ledger_what_the_flip_leaves_the_device() {
        let at_takeover = [event("4", "pad", TAKEOVER)];
        let later = [event("9/2", "pad", TAKEOVER + 6_000)];
        let mut ledger = playing(&[
            event("4", "pad", TAKEOVER),
            event("9/2", "pad", TAKEOVER + 6_000),
        ]);
        // A requery two blocks before the first onset.
        let first = TAKEOVER - 256;
        taking_over(&mut ledger, 2, 48_768, first);
        assert_eq!(ledger.claim_outgoing_copies(2, first, &at_takeover), [true]);
        assert_eq!(ledger.claim_outgoing_copies(2, first, &later), [false]);
        // Generation 2 plays the later onset itself.
        ledger.record(2, LiveQueryBudgetMode::ReplacementPrefill, &later);
        ledger.published(2, first, TakeoverCut::None);
        // A second requery, one block later: the copy generation 1 left
        // still sounds, and stands for generation 3's event too.
        let second = first + 128;
        taking_over(&mut ledger, 3, 48_896, second);
        assert_eq!(
            ledger.claim_outgoing_copies(3, second, &at_takeover),
            [true]
        );
        assert_eq!(
            ledger.claim_outgoing_copies(3, second, &later),
            [false],
            "the device holds no event for 9/2 before this takeover: \
             generation 1's was dropped, and generation 2's is aimed after it"
        );
        ledger.published(3, second, TakeoverCut::None);
        // A third takeover between the target frame and the onset of 9/2.
        let third = TAKEOVER + 4_000;
        taking_over(&mut ledger, 4, 49_280, third);
        assert_eq!(
            ledger.claim_outgoing_copies(4, third, &later),
            [false],
            "generation 2's event for 9/2 was aimed after generation 3's \
             takeover, so the device dropped it there"
        );
        assert_eq!(
            ledger.claim_outgoing_copies(4, third, &at_takeover),
            [false],
            "the onset at 4 is before this takeover"
        );
    }

    /// A window the published generation converts later reads the same
    /// takeover: its first window need not reach every onset the device
    /// holds a copy of.
    #[test]
    fn a_later_window_of_the_published_generation_reads_the_same_takeover() {
        let window = [event("4", "pad", TAKEOVER)];
        let mut ledger = playing(&window);
        taking_over(&mut ledger, 2, 49_024, TAKEOVER);
        ledger.published(2, TAKEOVER, TakeoverCut::None);
        assert_eq!(
            ledger.begin_window(2, LiveQueryBudgetMode::Steady, 49_152, RATE, None),
            Some(TAKEOVER)
        );
        assert_eq!(ledger.claim_outgoing_copies(2, TAKEOVER, &window), [true]);
        assert_eq!(
            ledger.begin_window(1, LiveQueryBudgetMode::Steady, 49_152, RATE, None),
            None,
            "another generation has no takeover to read"
        );
    }

    /// An onset leaves the ledger a second after the clock passes it, and a
    /// device that plays its first generation holds nothing older.
    #[test]
    fn past_onsets_and_an_earlier_device_are_forgotten() {
        let window = [event("4", "pad", TAKEOVER)];
        let second_later = TAKEOVER + u64::from(RATE);
        let mut ledger = playing(&window);
        taking_over(&mut ledger, 2, second_later, TAKEOVER);
        assert_eq!(
            ledger.claim_outgoing_copies(2, TAKEOVER, &window),
            [true],
            "a window converted after its takeover still meets the copy"
        );
        taking_over(&mut ledger, 3, second_later + 1, TAKEOVER);
        assert_eq!(ledger.claim_outgoing_copies(3, TAKEOVER, &window), [false]);

        let mut ledger = playing(&window);
        ledger.begin_window(5, LiveQueryBudgetMode::InitialPrefill, 0, RATE, None);
        taking_over(&mut ledger, 6, 45_440, TAKEOVER);
        assert_eq!(ledger.claim_outgoing_copies(6, TAKEOVER, &window), [false]);
    }

    /// Time in seconds of `frame`.
    #[cfg(any(feature = "osc", feature = "serial"))]
    fn seconds(frame: u64) -> f64 {
        frame as f64 / f64::from(RATE)
    }

    /// A message that is out stands for a newer generation's intent on the
    /// frontier's frame or before it. It never stands for an intent of the
    /// generation that handed out last.
    #[cfg(any(feature = "osc", feature = "serial"))]
    #[test]
    fn a_message_that_is_out_stands_for_a_newer_intent_up_to_the_frontier() {
        let mut handed_out = HandedOut::default();
        assert!(!handed_out.stands_for(2, seconds(TAKEOVER), RATE));
        handed_out.note(1, seconds(TAKEOVER));
        handed_out.note(1, seconds(TAKEOVER + 6_000));
        handed_out.note(1, seconds(TAKEOVER));
        let frontier = TAKEOVER + 6_000;
        assert!(handed_out.stands_for(2, seconds(TAKEOVER), RATE));
        assert!(handed_out.stands_for(2, seconds(frontier), RATE));
        assert!(
            handed_out.stands_for(2, (frontier as f64 - 0.3) / f64::from(RATE), RATE),
            "an onset in the last frame before the frontier is on its frame"
        );
        assert!(!handed_out.stands_for(2, seconds(frontier + 1), RATE));
        assert!(
            !handed_out.stands_for(1, seconds(TAKEOVER), RATE),
            "the generation that handed out sends all its intents"
        );

        // The newer generation hands out past the frontier and becomes the
        // one that handed out last. A late message of the older generation
        // changes nothing.
        handed_out.note(2, seconds(frontier + 6_000));
        handed_out.note(1, seconds(TAKEOVER + 3_000));
        assert!(!handed_out.stands_for(2, seconds(TAKEOVER), RATE));
        assert!(handed_out.stands_for(3, seconds(frontier + 6_000), RATE));
        assert!(!handed_out.stands_for(3, seconds(frontier + 6_001), RATE));

        handed_out.clear();
        assert!(!handed_out.stands_for(3, 0.0, RATE));
    }

    /// A time that is not a number moves no frontier and is behind none,
    /// and a rate of zero has no frames to compare.
    #[cfg(any(feature = "osc", feature = "serial"))]
    #[test]
    fn a_time_that_no_frame_holds_is_never_behind_the_frontier() {
        let mut handed_out = HandedOut::default();
        handed_out.note(1, f64::NAN);
        handed_out.note(1, f64::INFINITY);
        assert!(!handed_out.stands_for(2, 0.0, RATE));
        handed_out.note(1, 1.0);
        assert!(handed_out.stands_for(2, 0.5, RATE));
        assert!(!handed_out.stands_for(2, f64::NAN, RATE));
        assert!(!handed_out.stands_for(2, f64::INFINITY, RATE));
        assert!(!handed_out.stands_for(2, 0.5, 0));
    }

    /// A message on the frontier or before it is a key's, or a late one of
    /// an earlier generation. It moves nothing: the messages that are out
    /// still stand for the later intents of its generation up to the frontier.
    #[cfg(any(feature = "osc", feature = "serial"))]
    #[test]
    fn a_message_before_the_frontier_does_not_move_it() {
        let frontier = TAKEOVER + 6_000;
        let mut handed_out = HandedOut::default();
        handed_out.note(1, seconds(frontier));
        handed_out.note(2, seconds(TAKEOVER));
        handed_out.note(2, seconds(frontier));
        assert!(handed_out.stands_for(2, seconds(TAKEOVER + 3_000), RATE));
        assert!(handed_out.stands_for(2, seconds(frontier), RATE));
        assert!(!handed_out.stands_for(2, seconds(frontier + 1), RATE));

        // Generation 2 hands out past the frontier: its intents all go out.
        handed_out.note(2, seconds(frontier + 6_000));
        assert!(!handed_out.stands_for(2, seconds(frontier + 3_000), RATE));
        assert!(handed_out.stands_for(3, seconds(frontier + 6_000), RATE));
    }

    /// The onsets of one window and the presses behind them.
    #[cfg(any(feature = "osc", feature = "serial"))]
    mod placed_presses {
        use std::sync::Arc;

        use rustel_core::midi_in::{InputPort, PlacedKey};

        use super::super::PlacedPresses;
        use crate::{OnsetEventJson, ValueJson};

        fn keyboard() -> Arc<InputPort> {
            Arc::new(InputPort::new("keyboard".into()))
        }

        /// The press numbered `press` of `note`, placed on cycle `num`/`den`.
        fn key(press: u64, note: u8, num: i64, den: i64) -> PlacedKey {
            PlacedKey {
                press,
                note,
                num,
                den,
            }
        }

        /// An onset on `whole_begin` that plays `note`.
        fn onset(onset_id: u64, whole_begin: &str, note: u8) -> OnsetEventJson {
            OnsetEventJson {
                live_controls: [0; 2],
                onset_id,
                generation: 2,
                whole_begin: whole_begin.into(),
                duration_secs: 0.1,
                target_time: 0.5,
                value: ValueJson::Raw(serde_json::json!({"note": note, "s": "tri"})),
                value_show: String::new(),
                ui_visuals: 0,
                log_line: None,
            }
        }

        /// A window is the first to place a press: every onset on its cycle
        /// position is the press's, whatever note the score gives it. The
        /// next window finds the press placed.
        #[test]
        fn the_onsets_on_a_new_press_are_the_press() {
            let mut presses = PlacedPresses::default();
            let placed = [(keyboard(), vec![key(1, 60, 1, 2), key(2, 64, 2, 4)])];
            let window = [
                onset(7, "1/2", 72),
                onset(8, "5/8", 60),
                onset(9, "1/2", 76),
            ];
            assert_eq!(presses.claim_new(&placed, &window), [7, 9]);
            assert_eq!(presses.claim_new(&placed, &window), [0u64; 0]);
        }

        /// A requery forgot the place of press 1, and the window places it
        /// again beside the new press 2. The note shows which onsets are
        /// the new press's, in each lane of the score that plays the keyboard.
        #[test]
        fn the_note_shows_which_onsets_play_a_new_press() {
            let port = keyboard();
            let mut presses = PlacedPresses::default();
            let before = [(Arc::clone(&port), vec![key(1, 60, 3, 4)])];
            assert_eq!(presses.claim_new(&before, &[onset(1, "3/4", 60)]), [1]);
            let placed = [(port, vec![key(1, 60, 1, 2), key(2, 64, 1, 2)])];
            let window = [
                onset(2, "1/2", 60),
                onset(3, "1/2", 64),
                onset(4, "1/2", 64),
                onset(5, "1/2", 60),
            ];
            assert_eq!(presses.claim_new(&placed, &window), [3, 4]);
        }

        /// Beside a press placed before, the note does not show a new press
        /// when the score changes the notes, when both play one note, or
        /// when the onsets are not one for each press. No onset is claimed.
        #[test]
        fn a_new_press_that_the_note_does_not_show_is_not_claimed() {
            for (new_note, window) in [
                (64, vec![onset(2, "1/2", 72), onset(3, "1/2", 76)]),
                (64, vec![onset(2, "1/2", 64), onset(3, "1/2", 64)]),
                (60, vec![onset(2, "1/2", 60), onset(3, "1/2", 60)]),
                (
                    64,
                    vec![
                        onset(2, "1/2", 60),
                        onset(3, "1/2", 64),
                        onset(4, "1/2", 64),
                    ],
                ),
            ] {
                let port = keyboard();
                let mut presses = PlacedPresses::default();
                let before = [(Arc::clone(&port), vec![key(1, 60, 3, 4)])];
                presses.claim_new(&before, &[]);
                let placed = [(port, vec![key(1, 60, 1, 2), key(2, new_note, 1, 2)])];
                assert_eq!(
                    presses.claim_new(&placed, &window),
                    [0u64; 0],
                    "a new press of note {new_note} with {} onsets",
                    window.len()
                );
            }
        }

        /// Each keyboard counts its own presses. A press that another
        /// keyboard placed before, on the same cycle position, is a press
        /// placed before.
        #[test]
        fn each_keyboard_counts_its_own_presses() {
            let (first, second) = (keyboard(), keyboard());
            let mut presses = PlacedPresses::default();
            let before = [(Arc::clone(&first), vec![key(5, 60, 3, 4)])];
            presses.claim_new(&before, &[]);
            let placed = [
                (first, vec![key(5, 60, 1, 2)]),
                (second, vec![key(1, 64, 1, 2)]),
            ];
            let window = [onset(2, "1/2", 60), onset(3, "1/2", 64)];
            assert_eq!(presses.claim_new(&placed, &window), [3]);
        }
    }

    /// The ledger at work in the session's live conversion.
    mod conversion {
        use std::time::Duration;

        use super::super::super::{
            LiveAudioBatch, LiveAudioScheduleError, Session, SessionConfig, SessionPanicPoint,
        };
        use super::{COMPENSATION, LiveQueryBudgetMode, RATE};
        use crate::render::takeover_frame_at;

        /// The producer's reserve for its own work after a query. A score
        /// with a slider needs the host, and the host needs a reserve.
        const RESERVE: Duration = Duration::from_millis(2);

        /// A session at one cycle a second whose handovers take over a
        /// quarter second ahead, playing `score` from time zero.
        fn playing(score: &str) -> Session {
            let mut session = Session::with_config(SessionConfig {
                cps: 1.0,
                ..SessionConfig::default()
            })
            .expect("session");
            session.set_direct_diagnostic_logging(false);
            session.set_schedule_lead(0.0);
            session.set_continuity_margin(0.25);
            session.evaluate(score).expect("score");
            session.restart_transport_at(0.0);
            session
        }

        fn window(session: &mut Session, now: f64, mode: LiveQueryBudgetMode) -> LiveAudioBatch {
            match session.schedule_audio_live_at(now, RATE, RESERVE, mode) {
                Ok(batch) => batch,
                Err(
                    LiveAudioScheduleError::Retryable(error)
                    | LiveAudioScheduleError::AwaitingSamples(error)
                    | LiveAudioScheduleError::CandidateCommitted(error)
                    | LiveAudioScheduleError::CandidateRefusedScore(error),
                ) => panic!("the window at {now} s did not schedule: {error}"),
            }
        }

        fn target_frames(batch: &LiveAudioBatch) -> Vec<u64> {
            batch
                .events
                .iter()
                .map(|event| event.target_frame)
                .collect()
        }

        /// A requery takes over on an onset with a plain and a stretched
        /// voice. The device keeps only the stretched copy, so the incoming
        /// window leaves out only that event and does not count it.
        #[test]
        fn a_requery_leaves_out_only_the_event_the_device_holds_a_copy_of() {
            let mut session = playing(r#"stack(s("sawtooth*4").stretch(1), s("square*4"))"#);
            let first = window(&mut session, 0.0, LiveQueryBudgetMode::InitialPrefill);
            assert_eq!(
                target_frames(&first)[..6],
                [
                    0,
                    0,
                    12_000 - COMPENSATION,
                    12_000,
                    24_000 - COMPENSATION,
                    24_000
                ],
                "test premise: the device holds both voices of the onset at 0.5 s"
            );

            session
                .requery_active_at(0.25)
                .expect("requery")
                .expect("a running transport requeries");
            let second = window(&mut session, 0.25, LiveQueryBudgetMode::ReplacementPrefill);
            assert_eq!(
                target_frames(&second)[..3],
                [24_000, 36_000 - COMPENSATION, 36_000],
                "the stretched voice of the onset at 0.5 s is the device's copy"
            );
            let counts = second.dispositions;
            assert_eq!(counts.converted as usize, second.events.len());
            assert_eq!(
                (
                    counts.intended,
                    counts.refused,
                    counts.skipped_loading,
                    counts.external
                ),
                (counts.converted, 0, 0, 0),
                "the window's counts still add up"
            );
            assert_eq!(second.sample_identities.len(), second.events.len());
            assert_eq!(session.take_requery_takeover_time(), Some(0.5));
        }

        /// MIDI hands over on the frame of the onset, where the outgoing
        /// generation's copy is pruned. So the incoming MIDI intent for the
        /// onset at 0.5 s stays although its audio event is left out.
        #[cfg(feature = "midi")]
        #[test]
        fn the_onset_left_out_of_the_audio_keeps_its_midi_intent() {
            let mut session = playing(r#"note("c3*4").s("sawtooth").stretch(1).midi("out")"#);
            window(&mut session, 0.0, LiveQueryBudgetMode::InitialPrefill);
            drop(session.take_pending_midi());
            session
                .requery_active_at(0.25)
                .expect("requery")
                .expect("a running transport requeries");
            let second = window(&mut session, 0.25, LiveQueryBudgetMode::ReplacementPrefill);
            assert_eq!(
                target_frames(&second).first(),
                Some(&(36_000 - COMPENSATION)),
                "test premise: the audio event of the onset at 0.5 s is left out"
            );
            let generation = session.generation();
            let midi: Vec<(u64, f64)> = session
                .take_pending_midi()
                .iter()
                .map(|intent| (intent.generation, intent.target_time))
                .collect();
            assert_eq!(midi.first(), Some(&(generation, 0.5)), "{midi:?}");
        }

        /// A slider changes the value of every onset a requery queries. The
        /// device's copy plays the onset at 0.5 s with the old value, and
        /// the new event is left out. Later onsets carry the new value.
        #[test]
        fn a_slider_requery_leaves_out_the_event_under_its_new_value() {
            let cutoffs = |batch: &LiveAudioBatch| -> Vec<(u64, f32)> {
                batch
                    .events
                    .iter()
                    .map(|event| {
                        let lowpass = event.controls.filters.lowpass.expect("a lowpass");
                        (event.target_frame, lowpass.frequency_hz)
                    })
                    .collect()
            };
            let mut session = playing(r#"s("sawtooth*4").stretch(1).lpf(slider(800, 100, 5000))"#);
            let first = window(&mut session, 0.0, LiveQueryBudgetMode::InitialPrefill);
            assert_eq!(
                cutoffs(&first)[..3],
                [
                    (0, 800.0),
                    (12_000 - COMPENSATION, 800.0),
                    (24_000 - COMPENSATION, 800.0)
                ],
                "test premise: the device holds the onset at 0.5 s"
            );

            let (slider, _) = session
                .js
                .slider_values()
                .expect("slider cells")
                .first()
                .expect("one slider")
                .clone();
            assert!(
                session
                    .set_slider_value(&slider, 2_400.0)
                    .expect("slider write")
            );
            session
                .requery_active_at(0.25)
                .expect("requery")
                .expect("a running transport requeries");
            let second = window(&mut session, 0.25, LiveQueryBudgetMode::ReplacementPrefill);
            assert_eq!(
                cutoffs(&second).first(),
                Some(&(36_000 - COMPENSATION, 2_400.0)),
                "the onset at 0.5 s stays the device's copy: {:?}",
                cutoffs(&second)
            );
        }

        /// A save removes `.stretch(1)`. The device keeps the stretched copy
        /// of the onset at 0.5 s, so the plain event of that onset is left
        /// out. The square is another lane's: the device dropped its copy.
        #[test]
        fn a_save_that_removes_the_effect_leaves_out_the_plain_event_of_the_copy() {
            let mut session = playing(r#"stack(s("sawtooth*4").stretch(1), s("square*4"))"#);
            let first = window(&mut session, 0.0, LiveQueryBudgetMode::InitialPrefill);
            assert_eq!(
                target_frames(&first)[4..6],
                [24_000 - COMPENSATION, 24_000],
                "test premise: the device holds both voices of the onset at 0.5 s"
            );

            session
                .reload_at(r#"stack(s("sawtooth*4"), s("square*4"))"#, false, 0.25)
                .expect("save");
            let second = window(&mut session, 0.25, LiveQueryBudgetMode::ReplacementPrefill);
            assert_eq!(
                target_frames(&second)[..3],
                [24_000, 36_000, 36_000],
                "only the square of the onset at 0.5 s is this window's to play"
            );
            let synths = |batch: &LiveAudioBatch| -> Vec<_> {
                batch.events.iter().map(|event| event.synth).collect()
            };
            assert_eq!(synths(&second)[0], synths(&first)[5], "the square stays");
            let counts = second.dispositions;
            assert_eq!(counts.converted as usize, second.events.len());
            assert_eq!(counts.intended, counts.converted);
            assert_eq!(second.sample_identities.len(), second.events.len());
            assert_eq!(session.take_requery_takeover_time(), Some(0.5));
        }

        /// A save removes the stretched lane. Its copy on the device stands
        /// for no event of the new score, so the plain lane plays the onset.
        #[test]
        fn a_save_that_removes_a_stretched_lane_keeps_the_plain_lane() {
            let mut session = playing(r#"stack(s("sawtooth*4").stretch(1), s("square*4"))"#);
            window(&mut session, 0.0, LiveQueryBudgetMode::InitialPrefill);
            session
                .reload_at(r#"s("square*4")"#, false, 0.25)
                .expect("save");
            let second = window(&mut session, 0.25, LiveQueryBudgetMode::ReplacementPrefill);
            assert_eq!(target_frames(&second)[..2], [24_000, 36_000]);
        }

        /// A save adds `.stretch(1)`. The device dropped the plain copy of
        /// the onset at 0.5 s, so the stretched event of that onset plays.
        #[test]
        fn a_save_that_adds_the_effect_plays_the_stretched_event() {
            let mut session = playing(r#"s("sawtooth*4")"#);
            window(&mut session, 0.0, LiveQueryBudgetMode::InitialPrefill);
            session
                .reload_at(r#"s("sawtooth*4").stretch(1)"#, false, 0.25)
                .expect("save");
            let second = window(&mut session, 0.25, LiveQueryBudgetMode::ReplacementPrefill);
            assert_eq!(
                target_frames(&second)[..2],
                [24_000 - COMPENSATION, 36_000 - COMPENSATION]
            );
        }

        /// A lane and its stretched twin play the same value. The stretched
        /// event has the first claim on the copy, so the plain event of the
        /// onset at 0.5 s plays: the device dropped its outgoing copy.
        #[test]
        fn a_handover_of_a_lane_and_its_stretched_twin_plays_the_plain_event() {
            let score = r#"stack(s("sawtooth*4").stretch(1), s("sawtooth*4"))"#;
            for save in [false, true] {
                let mut session = playing(score);
                let first = window(&mut session, 0.0, LiveQueryBudgetMode::InitialPrefill);
                assert_eq!(
                    target_frames(&first)[4..6],
                    [24_000 - COMPENSATION, 24_000],
                    "test premise: the device holds both voices of the onset at 0.5 s"
                );
                if save {
                    let again = format!("{score}\n// again\n");
                    session.reload_at(&again, false, 0.25).expect("save");
                } else {
                    session
                        .requery_active_at(0.25)
                        .expect("requery")
                        .expect("a running transport requeries");
                }
                let second = window(&mut session, 0.25, LiveQueryBudgetMode::ReplacementPrefill);
                assert_eq!(
                    target_frames(&second)[..3],
                    [24_000, 36_000 - COMPENSATION, 36_000],
                    "save: {save}"
                );
            }
        }

        /// The plain event left out for a stretched copy keeps its MIDI
        /// intent: MIDI hands over on the frame of the onset.
        #[cfg(feature = "midi")]
        #[test]
        fn the_plain_event_left_out_of_the_audio_keeps_its_midi_intent() {
            let mut session = playing(r#"note("c3*4").s("sawtooth").stretch(1).midi("out")"#);
            window(&mut session, 0.0, LiveQueryBudgetMode::InitialPrefill);
            drop(session.take_pending_midi());
            session
                .reload_at(r#"note("c3*4").s("sawtooth").midi("out")"#, false, 0.25)
                .expect("save");
            let second = window(&mut session, 0.25, LiveQueryBudgetMode::ReplacementPrefill);
            assert_eq!(
                target_frames(&second).first(),
                Some(&36_000),
                "test premise: the audio event of the onset at 0.5 s is left out"
            );
            let generation = session.generation();
            let midi: Vec<(u64, f64)> = session
                .take_pending_midi()
                .iter()
                .map(|intent| (intent.generation, intent.target_time))
                .collect();
            assert_eq!(midi.first(), Some(&(generation, 0.5)), "{midi:?}");
        }

        /// A session rebuilt after a panic keeps the ledger: the device still
        /// holds what it was sent. With a stretched onset every 750 frames,
        /// some copies are aimed before any takeover. None sounds again.
        #[test]
        fn a_rebuilt_session_still_meets_the_copies_the_device_holds() {
            let mut session = playing(r#"s("sawtooth*64").stretch(1)"#);
            window(&mut session, 0.0, LiveQueryBudgetMode::InitialPrefill);
            window(&mut session, 0.2, LiveQueryBudgetMode::Steady);
            session.inject_panic_for_test(SessionPanicPoint::Query);
            assert!(
                session
                    .schedule_audio_live_at(0.25, RATE, RESERVE, LiveQueryBudgetMode::Steady)
                    .is_err(),
                "test premise: the query panics and the session is rebuilt"
            );
            assert_eq!(
                session.live_sample_rate,
                Some(RATE),
                "the replay takes over on a frame, as before the panic"
            );

            let replayed = window(&mut session, 0.25, LiveQueryBudgetMode::ReplacementPrefill);
            let takeover = session
                .take_requery_takeover_time()
                .expect("the replayed score takes over from the device's audio");
            let takeover_frame = takeover_frame_at(takeover, RATE);
            let before_takeover: Vec<u64> = target_frames(&replayed)
                .into_iter()
                .filter(|frame| *frame < takeover_frame)
                .collect();
            assert!(
                !replayed.events.is_empty(),
                "test premise: the replay sounds"
            );
            assert_eq!(
                before_takeover, [0u64; 0],
                "events aimed before the takeover on frame {takeover_frame}: \
                 the device holds a copy of each"
            );
        }

        /// The frontier of the OSC bundles at work in the same conversion.
        /// Serial takes the same path, and the played tests cover both.
        #[cfg(feature = "osc")]
        mod osc_frontier {
            use rustel_audio::TakeoverCut;

            use super::{
                LiveQueryBudgetMode, RATE, RESERVE, Session, SessionPanicPoint, playing, window,
            };
            use crate::render::onset_frame_at;

            /// Eight onsets a cycle with OSC output: one every 6000 frames.
            const SCORE: &str = r#"note("c3*8").s("sawtooth").osc(57120)"#;
            const STEP: u64 = 6_000;
            /// The latest onset the first window stages. Its cover is the
            /// half second of horizon and the quarter second of margin, and
            /// ends before the onset at 0.75 s.
            const FRONTIER: u64 = 30_000;

            /// The frame of each staged bundle.
            fn staged(session: &mut Session) -> Vec<u64> {
                let staged = session.take_pending_osc();
                let frame = |(_, intent): &(f64, crate::osc_bridge::OscOnset)| {
                    onset_frame_at(intent.target_time, RATE)
                };
                staged.iter().map(frame).collect()
            }

            /// Hand the staged bundles out, as a host does.
            fn hand_out(session: &mut Session) -> Vec<u64> {
                let staged = session.take_pending_osc();
                for (_, intent) in &staged {
                    session.note_osc_handed_out(intent.generation, intent.target_time);
                }
                let frame = |(_, intent): &(f64, crate::osc_bridge::OscOnset)| {
                    onset_frame_at(intent.target_time, RATE)
                };
                staged.iter().map(frame).collect()
            }

            /// `SCORE` playing, with its first window handed out.
            fn handed_out_to_the_frontier() -> Session {
                let mut session = playing(SCORE);
                window(&mut session, 0.0, LiveQueryBudgetMode::InitialPrefill);
                let first = hand_out(&mut session);
                assert_eq!(first.last(), Some(&FRONTIER), "test premise: {first:?}");
                session
            }

            fn requery(session: &mut Session, now: f64) {
                session
                    .requery_active_at(now)
                    .expect("requery")
                    .expect("a running transport requeries");
            }

            /// A requery at 0.25 s takes over at 0.5 s. Its audio starts
            /// there. Its bundles start after the frontier: the bundles of
            /// the onsets up to it are out.
            #[test]
            fn a_requery_stages_the_bundles_after_the_frontier() {
                let mut session = handed_out_to_the_frontier();
                requery(&mut session, 0.25);
                let second = window(&mut session, 0.25, LiveQueryBudgetMode::ReplacementPrefill);
                assert_eq!(
                    second.events.first().map(|event| event.target_frame),
                    Some(24_000),
                    "test premise: the audio hands over on the takeover frame"
                );
                assert_eq!(
                    hand_out(&mut session),
                    [FRONTIER + STEP, FRONTIER + 2 * STEP]
                );
                // The generation that handed out last sends each window whole.
                window(&mut session, 0.5, LiveQueryBudgetMode::Steady);
                assert_eq!(
                    staged(&mut session),
                    [FRONTIER + 3 * STEP, FRONTIER + 4 * STEP]
                );
            }

            /// A window whose bundles never reach a host does not move the
            /// frontier: a rolled-back replacement, or what a reload shield
            /// stages and a takeover cuts. The next generation sends them.
            #[test]
            fn a_window_that_is_not_handed_out_does_not_move_the_frontier() {
                let mut session = handed_out_to_the_frontier();
                requery(&mut session, 0.25);
                window(&mut session, 0.25, LiveQueryBudgetMode::ReplacementPrefill);
                assert_eq!(
                    staged(&mut session),
                    [FRONTIER + STEP, FRONTIER + 2 * STEP],
                    "test premise: the window staged two bundles, and no host took them"
                );
                requery(&mut session, 0.25);
                window(&mut session, 0.25, LiveQueryBudgetMode::ReplacementPrefill);
                assert_eq!(staged(&mut session), [FRONTIER + STEP, FRONTIER + 2 * STEP]);
            }

            /// A score that only an OSC receiver can play has no audio event.
            /// Its replacement is still valid when the bundles of its whole
            /// first window are out, and it counts those onsets as external.
            #[test]
            fn a_window_whose_bundles_are_all_out_is_still_an_external_window() {
                let mut session = playing(r#"s("not-a-native-sample*8").osc(57120)"#);
                window(&mut session, 0.0, LiveQueryBudgetMode::InitialPrefill);
                assert_eq!(hand_out(&mut session).last(), Some(&FRONTIER));
                requery(&mut session, 0.0);
                let second = window(&mut session, 0.0, LiveQueryBudgetMode::ReplacementPrefill);
                assert!(second.events.is_empty(), "test premise: no audio event");
                assert_eq!(staged(&mut session), [0u64; 0], "every bundle is out");
                assert!(second.prefill_progress);
                assert_eq!(
                    (second.dispositions.external, second.dispositions.refused),
                    (second.dispositions.intended, 0),
                    "the onsets of the window are external, as before the takeover"
                );
            }

            /// A transport start begins a new timeline. The generation after
            /// it sends its bundles from its own takeover, before the old
            /// frontier too.
            #[test]
            fn a_transport_start_sends_the_bundles_before_the_old_frontier() {
                for start in [
                    Session::restart_transport_at as fn(&mut Session, f64),
                    Session::finish_transport_start_at,
                ] {
                    let mut session = handed_out_to_the_frontier();
                    start(&mut session, 0.0);
                    requery(&mut session, 0.0);
                    window(&mut session, 0.0, LiveQueryBudgetMode::ReplacementPrefill);
                    assert_eq!(
                        staged(&mut session),
                        [2 * STEP, 3 * STEP, 4 * STEP, FRONTIER]
                    );
                }
            }

            /// A producer that plays its first generation begins a timeline
            /// too, with no transport start: a host opened another device.
            #[test]
            fn a_first_generation_sends_the_bundles_before_the_old_frontier() {
                let mut session = handed_out_to_the_frontier();
                session.reload_at(SCORE, false, 0.0).expect("save");
                window(&mut session, 0.0, LiveQueryBudgetMode::InitialPrefill);
                let first = staged(&mut session);
                assert!(
                    first.iter().any(|frame| *frame <= FRONTIER),
                    "the old frontier left out bundles of the new device: {first:?}"
                );
            }

            /// An output that failed discards its ring, and the generation
            /// that refills it plays those onsets on the new stream. It
            /// sends their bundles too, in time with that audio.
            #[test]
            fn the_refill_of_a_recycled_output_sends_its_own_bundles() {
                let mut session = handed_out_to_the_frontier();
                session
                    .requery_after_output_recycle_at(0.25)
                    .expect("requery")
                    .expect("a running transport requeries");
                window(&mut session, 0.25, LiveQueryBudgetMode::ReplacementPrefill);
                assert_eq!(staged(&mut session).first(), Some(&(2 * STEP)));
            }

            /// A session rebuilt after a panic keeps the frontier: the
            /// bundles that are out stay out. The replayed score takes over
            /// at 0.5 s and sends the onsets after the frontier.
            #[test]
            fn a_rebuilt_session_still_meets_the_bundles_that_are_out() {
                let mut session = handed_out_to_the_frontier();
                session.inject_panic_for_test(SessionPanicPoint::Query);
                assert!(
                    session
                        .schedule_audio_live_at(0.25, RATE, RESERVE, LiveQueryBudgetMode::Steady)
                        .is_err(),
                    "test premise: the query panics and the session is rebuilt"
                );
                let replayed = window(&mut session, 0.25, LiveQueryBudgetMode::ReplacementPrefill);
                assert!(
                    replayed
                        .events
                        .iter()
                        .any(|event| event.target_frame <= FRONTIER),
                    "test premise: the replay has onsets before the frontier"
                );
                let bundles = staged(&mut session);
                assert_eq!(bundles.first(), Some(&(FRONTIER + STEP)), "{bundles:?}");
            }

            /// A session rebuilt after a panic keeps the presses it placed.
            /// The replay places the press again before its old pin: the
            /// bundle of the press is out already, so no second one goes out.
            #[test]
            fn a_rebuilt_session_still_meets_the_presses_it_placed() {
                let mut session = keys_handed_out_to_the_frontier();
                press(&session, 60);
                requery(&mut session, 0.25);
                window(&mut session, 0.25, LiveQueryBudgetMode::ReplacementPrefill);
                assert_eq!(hand_out_keys(&mut session).1, [(24_000, 60)]);
                session.set_continuity_margin(0.05);
                session.inject_panic_for_test(SessionPanicPoint::Query);
                assert!(
                    session
                        .schedule_audio_live_at(0.25, RATE, RESERVE, LiveQueryBudgetMode::Steady)
                        .is_err(),
                    "test premise: the query panics and the session is rebuilt"
                );
                let replayed = window(&mut session, 0.25, LiveQueryBudgetMode::ReplacementPrefill);
                assert!(
                    replayed
                        .events
                        .iter()
                        .any(|event| event.target_frame < 18_000),
                    "test premise: the replay places the press before its old pin"
                );
                assert_eq!(hand_out_keys(&mut session).1, []);
            }

            /// A save that plays its score from the beginning takes over
            /// with a cut. Its first window sends every bundle, before the
            /// old frontier too, and the published cut forgets the frontier.
            #[test]
            fn a_takeover_with_a_cut_sends_its_whole_first_window() {
                let mut session = handed_out_to_the_frontier();
                session.start_next_from_zero();
                let generation = session.reload_at(SCORE, false, 0.25).expect("save");
                window(&mut session, 0.25, LiveQueryBudgetMode::ReplacementPrefill);
                let (takeover, cut) = session.take_requery_takeover().expect("a takeover");
                assert_ne!(cut, TakeoverCut::None, "test premise: the save cuts");
                let takeover_frame = crate::render::takeover_frame_at(takeover, RATE);
                let first = staged(&mut session);
                assert_eq!(
                    first.first(),
                    Some(&takeover_frame),
                    "cycle zero is on the takeover"
                );
                assert!(
                    first.iter().filter(|frame| **frame <= FRONTIER).count() > 1,
                    "test premise: the window has onsets before the old frontier: {first:?}"
                );
                let spaced = first.windows(2).all(|pair| pair[1] - pair[0] == STEP);
                assert!(spaced, "a bundle of the first window is missing: {first:?}");

                // No host took those bundles. The cut still ends the old
                // timeline: the generation after it meets no frontier.
                session.audio_takeover_published(generation, takeover_frame, cut);
                requery(&mut session, 0.25);
                window(&mut session, 0.25, LiveQueryBudgetMode::ReplacementPrefill);
                let after_the_cut = staged(&mut session);
                assert!(
                    after_the_cut.iter().any(|frame| *frame <= FRONTIER),
                    "the old frontier left out bundles of the new timeline: {after_the_cut:?}"
                );
            }

            /// An edit without a cut keeps the frontier across its published
            /// takeover. When that edit sent nothing, a second edit still
            /// meets the bundles that are out.
            #[test]
            fn a_takeover_without_a_cut_keeps_the_frontier() {
                let mut session = handed_out_to_the_frontier();
                let generation = session.reload_at(SCORE, false, 0.0).expect("save");
                window(&mut session, 0.0, LiveQueryBudgetMode::ReplacementPrefill);
                let (takeover, cut) = session.take_requery_takeover().expect("a takeover");
                assert_eq!(cut, TakeoverCut::None, "test premise: an edit");
                assert_eq!(staged(&mut session), [0u64; 0], "every bundle is out");
                let takeover_frame = crate::render::takeover_frame_at(takeover, RATE);
                session.audio_takeover_published(generation, takeover_frame, cut);
                requery(&mut session, 0.25);
                window(&mut session, 0.25, LiveQueryBudgetMode::ReplacementPrefill);
                assert_eq!(staged(&mut session), [FRONTIER + STEP, FRONTIER + 2 * STEP]);
            }

            /// `SCORE` with a keyboard beside it. A key plays a triangle.
            const KEYS: &str = r#"const kb = await midikeys('keyboard')
stack(note("c3*8").s("sawtooth"), kb(0.05).s("tri")).osc(57120)"#;

            /// Press `note` on the keyboard of `KEYS`.
            fn press(session: &Session, note: u8) {
                session
                    .midi_input_bus()
                    .find("keyboard")
                    .expect("keyboard port")
                    .observe_note_on(rustel_core::midi_in::now_nanos(), 1, note, 100);
            }

            /// Hand the staged bundles out, as a host does. Return the
            /// frames of the loop's bundles, then the frame and the note of
            /// each key's.
            fn hand_out_keys(session: &mut Session) -> (Vec<u64>, Vec<(u64, u8)>) {
                let staged = session.take_pending_osc();
                let (mut looped, mut keys) = (Vec::new(), Vec::new());
                for (_, intent) in &staged {
                    session.note_osc_handed_out(intent.generation, intent.target_time);
                    let frame = onset_frame_at(intent.target_time, RATE);
                    let triangle = rustel_osc::OscValue::Str("tri".into());
                    if intent.args.contains(&triangle) {
                        let note = (36..96).find(|note| {
                            intent
                                .args
                                .contains(&rustel_osc::OscValue::Float(f32::from(*note)))
                        });
                        keys.push((frame, note.expect("a key has a note")));
                    } else {
                        looped.push(frame);
                    }
                }
                (looped, keys)
            }

            /// `KEYS` playing, with its first window handed out.
            fn keys_handed_out_to_the_frontier() -> Session {
                let mut session = playing(KEYS);
                window(&mut session, 0.0, LiveQueryBudgetMode::InitialPrefill);
                let (first, keys) = hand_out_keys(&mut session);
                assert_eq!(first.last(), Some(&FRONTIER), "test premise: {first:?}");
                assert_eq!(keys, [], "test premise: no key is pressed");
                session
            }

            /// A key pressed over a running loop takes over at 0.5 s, before
            /// the frontier. No bundle is out for it, so its bundle goes
            /// out. The loop's bundles up to the frontier stay out.
            #[test]
            fn a_key_pressed_over_a_running_loop_sends_its_bundle() {
                let mut session = keys_handed_out_to_the_frontier();
                press(&session, 60);
                requery(&mut session, 0.25);
                window(&mut session, 0.25, LiveQueryBudgetMode::ReplacementPrefill);
                assert_eq!(
                    hand_out_keys(&mut session),
                    (
                        vec![FRONTIER + STEP, FRONTIER + 2 * STEP],
                        vec![(24_000, 60)]
                    )
                );
            }

            /// The first window of a key requery can end before the frontier:
            /// here the margin is shorter than at the start. The key's bundle
            /// goes out, and the later windows send no bundle of the loop again.
            #[test]
            fn the_windows_after_a_key_press_send_no_bundle_of_the_loop_again() {
                let mut session = keys_handed_out_to_the_frontier();
                session.set_continuity_margin(0.05);
                press(&session, 60);
                requery(&mut session, 0.0);
                window(&mut session, 0.0, LiveQueryBudgetMode::ReplacementPrefill);
                assert_eq!(hand_out_keys(&mut session), (vec![], vec![(2_400, 60)]));
                let mut later = Vec::new();
                for now in [0.05, 0.1, 0.15, 0.2, 0.3] {
                    window(&mut session, now, LiveQueryBudgetMode::Steady);
                    let (looped, keys) = hand_out_keys(&mut session);
                    assert_eq!(keys, [], "the key plays once");
                    later.extend(looped);
                }
                assert_eq!(later, [FRONTIER + STEP], "the frontier stays at {FRONTIER}");
            }

            /// Three presses, each with a requery from the same device clock.
            /// Each requery places every press so far on its takeover. Only
            /// the new press has no bundle out, so only its bundle goes out.
            #[test]
            fn presses_on_one_takeover_each_send_one_bundle() {
                let mut session = keys_handed_out_to_the_frontier();
                let mut sent = (Vec::new(), Vec::new());
                for note in [60, 64, 67] {
                    press(&session, note);
                    requery(&mut session, 0.25);
                    window(&mut session, 0.25, LiveQueryBudgetMode::ReplacementPrefill);
                    let (looped, keys) = hand_out_keys(&mut session);
                    sent.0.extend(looped);
                    sent.1.extend(keys);
                }
                assert_eq!(
                    sent,
                    (
                        vec![FRONTIER + STEP, FRONTIER + 2 * STEP],
                        vec![(24_000, 60), (24_000, 64), (24_000, 67)]
                    )
                );
            }

            /// A steady window places a press before its requery runs, and
            /// the bundle goes out for that time. The requery places it
            /// again beside a new press: only the new one has no bundle out.
            #[test]
            fn a_press_that_an_earlier_window_placed_sends_no_second_bundle() {
                let mut session = keys_handed_out_to_the_frontier();
                press(&session, 60);
                window(&mut session, 0.1, LiveQueryBudgetMode::Steady);
                assert_eq!(
                    hand_out_keys(&mut session),
                    (vec![FRONTIER + STEP], vec![(FRONTIER + STEP, 60)]),
                    "test premise: the steady window placed the press"
                );
                press(&session, 64);
                requery(&mut session, 0.25);
                let second = window(&mut session, 0.25, LiveQueryBudgetMode::ReplacementPrefill);
                let on_the_takeover = second
                    .events
                    .iter()
                    .filter(|event| event.target_frame == 24_000);
                assert_eq!(
                    on_the_takeover.count(),
                    3,
                    "test premise: the loop and both keys sound on the takeover"
                );
                assert_eq!(
                    hand_out_keys(&mut session),
                    (vec![FRONTIER + 2 * STEP], vec![(24_000, 64)])
                );
            }

            /// A slider that moves makes a requery every 120 ms. Two keys
            /// are pressed between them, 3 ms apart. Every bundle of the
            /// loop goes out once, and each key's once.
            #[test]
            fn keys_pressed_while_a_slider_moves_send_each_bundle_once() {
                let mut session = keys_handed_out_to_the_frontier();
                let mut sent = (Vec::new(), Vec::new());
                for (now, key) in [
                    (0.13, None),
                    (0.25, None),
                    (0.3, Some(60)),
                    (0.303, Some(64)),
                    (0.37, None),
                    (0.49, None),
                ] {
                    if let Some(key) = key {
                        press(&session, key);
                    }
                    requery(&mut session, now);
                    for (now, mode) in [
                        (now, LiveQueryBudgetMode::ReplacementPrefill),
                        (now + 0.01, LiveQueryBudgetMode::Steady),
                    ] {
                        window(&mut session, now, mode);
                        let (looped, keys) = hand_out_keys(&mut session);
                        sent.0.extend(looped);
                        sent.1.extend(keys);
                    }
                }
                let looped: Vec<u64> = (1..=4).map(|step| FRONTIER + step * STEP).collect();
                // A press takes over a quarter second after its requery.
                assert_eq!(sent, (looped, vec![(26_400, 60), (26_544, 64)]));
            }

            /// A key pressed before a save is the saved score's to play. Its
            /// first window places the press on its begin, the time of the
            /// save, and sounds it there. No bundle is out for the press.
            #[test]
            fn a_key_pressed_before_a_save_sends_its_bundle() {
                let mut session = keys_handed_out_to_the_frontier();
                press(&session, 60);
                let again = format!("{KEYS}\n// again\n");
                session.reload_at(&again, false, 0.25).expect("save");
                let second = window(&mut session, 0.25, LiveQueryBudgetMode::ReplacementPrefill);
                let (_, cut) = session.take_requery_takeover().expect("a takeover");
                assert_eq!(cut, TakeoverCut::None, "test premise: an edit");
                assert_eq!(
                    second.events.first().map(|event| event.target_frame),
                    Some(12_000),
                    "test premise: the key sounds at the time of the save"
                );
                let (looped, keys) = hand_out_keys(&mut session);
                assert_eq!(keys, [(12_000, 60)]);
                assert_eq!(looped.first(), Some(&(FRONTIER + STEP)), "{looped:?}");
            }

            /// A clock steer moves each onset in time, and the frontier is a
            /// time. The bundles go out from the first onset after it. Here
            /// the steer moves each onset a sixteenth of a second.
            #[test]
            fn a_clock_steer_reaches_osc_from_the_frontier_on() {
                for moved_cycles in [0.0625, -0.0625] {
                    let mut session = handed_out_to_the_frontier();
                    session.retime(0.25, 1.0, 0.25 + moved_cycles);
                    requery(&mut session, 0.25);
                    window(&mut session, 0.25, LiveQueryBudgetMode::ReplacementPrefill);
                    assert_eq!(
                        staged(&mut session),
                        [
                            FRONTIER + STEP / 2,
                            FRONTIER + 3 * STEP / 2,
                            FRONTIER + 5 * STEP / 2
                        ],
                        "a steer of {moved_cycles} cycle"
                    );
                }
            }
        }
    }
}
