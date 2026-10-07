//! What a reload shield staged for the external outputs and has not sent.

use rustel_runtime::midi_bridge::MidiOnset;
use rustel_runtime::midi_bridge::onset_frame_at;

/// A reload shield's `.osc()` bundles and `.serial()` writes, and its
/// `.midi()` onsets at or past an armed line. OSC and serial cannot be
/// recalled once sent, and MIDI past an armed line could sound before any
/// prune runs; each waits here until it falls within the steady cover, as a
/// steady drain would send it. What is held goes with the audio it belongs
/// to: where the audio retires or discards the outgoing score, what is held
/// for it from there goes too. One whose onset has passed is dropped rather
/// than sent late.
#[derive(Default)]
pub(super) struct ShieldHold {
    midi: HeldIntents<MidiOnset>,
    #[cfg(feature = "osc")]
    osc: HeldIntents<(f64, rustel_runtime::osc_bridge::OscOnset)>,
    #[cfg(feature = "serial")]
    serial: HeldIntents<(f64, rustel_runtime::serial_bridge::SerialOnset)>,
}

/// What one drain takes out of the [`ShieldHold`], by family.
pub(super) struct Released {
    pub(super) midi: Vec<MidiOnset>,
    #[cfg(feature = "osc")]
    pub(super) osc: Vec<(f64, rustel_runtime::osc_bridge::OscOnset)>,
    #[cfg(feature = "serial")]
    pub(super) serial: Vec<(f64, rustel_runtime::serial_bridge::SerialOnset)>,
}

impl ShieldHold {
    /// Hold the OSC and serial a shield's pass staged in the session.
    #[cfg(any(feature = "osc", feature = "serial"))]
    pub(super) fn take_staged(&mut self, session: &mut rustel_runtime::Session) {
        #[cfg(feature = "osc")]
        self.osc.0.extend(session.take_pending_osc());
        #[cfg(feature = "serial")]
        self.serial.0.extend(session.take_pending_serial());
    }

    /// Hold the shield's MIDI that lies at or past `armed_line`, the frame a
    /// pre-armed line cut retires the outgoing onsets from, and leave the
    /// rest in `midi` for the bridge.
    pub(super) fn hold_midi_past(
        &mut self,
        midi: &mut Vec<MidiOnset>,
        armed_line: Option<u64>,
        sample_rate: u32,
    ) {
        if let Some(line) = armed_line {
            self.midi.0.extend(midi.extract_if(.., |intent| {
                at_or_past(intent.target_time, line, sample_rate)
            }));
        }
    }

    /// Take out what a drain at `device_now` sends with `cover` seconds of
    /// steady cover. An `armed_line` not yet passed keeps what lies at or
    /// past it waiting.
    pub(super) fn release(
        &mut self,
        device_now: f64,
        cover: f64,
        armed_line: Option<u64>,
        sample_rate: u32,
    ) -> Released {
        let drain = Drain {
            device_now,
            cover,
            armed_line,
            sample_rate,
        };
        Released {
            midi: self.midi.release(&drain),
            #[cfg(feature = "osc")]
            osc: self.osc.release(&drain),
            #[cfg(feature = "serial")]
            serial: self.serial.release(&drain),
        }
    }

    /// Drop what is due at or after `frame`, where the audio retires the
    /// outgoing onsets. Everything held belongs to a generation older than
    /// any that publishes after it was held.
    pub(super) fn cut_from(&mut self, frame: u64, sample_rate: u32) {
        self.midi.cut_from(frame, sample_rate);
        #[cfg(feature = "osc")]
        self.osc.cut_from(frame, sample_rate);
        #[cfg(feature = "serial")]
        self.serial.cut_from(frame, sample_rate);
    }

    pub(super) fn clear(&mut self) {
        *self = Self::default();
    }

    /// When everything held is due, family by family.
    #[cfg(all(test, feature = "osc"))]
    pub(super) fn held_targets_for_test(&self) -> Vec<f64> {
        let mut targets: Vec<f64> = self.osc.0.iter().map(ExternalIntent::target_time).collect();
        targets.extend(self.midi.0.iter().map(ExternalIntent::target_time));
        #[cfg(feature = "serial")]
        targets.extend(self.serial.0.iter().map(ExternalIntent::target_time));
        targets
    }

    /// Hold an `.osc()` bundle due at each of `targets`.
    #[cfg(all(test, feature = "osc"))]
    pub(super) fn hold_osc_for_test(&mut self, targets: &[f64]) {
        self.osc.0.extend(targets.iter().map(|target_time| {
            let bundle = rustel_runtime::osc_bridge::OscOnset {
                onset_id: 0,
                generation: 0,
                host: String::new(),
                port: 0,
                destination: None,
                args: Vec::new(),
                encoded_bytes: 0,
                target_time: *target_time,
            };
            (0.0, bundle)
        }));
    }
}

/// Whether an onset due at `target_time` falls at or past `frame`, on the
/// frame the audio and the MIDI bridge give it ([`onset_frame_at`]).
fn at_or_past(target_time: f64, frame: u64, sample_rate: u32) -> bool {
    onset_frame_at(target_time, sample_rate) >= frame
}

/// One family's held intents, in the shape the session stages them in.
struct HeldIntents<I>(Vec<I>);

impl<I> Default for HeldIntents<I> {
    fn default() -> Self {
        Self(Vec::new())
    }
}

impl<I: ExternalIntent> HeldIntents<I> {
    /// Take out, in order, what `drain` sends, and drop what it drops.
    fn release(&mut self, drain: &Drain) -> Vec<I> {
        let mut sent = Vec::new();
        for held in std::mem::take(&mut self.0) {
            match drain.release(held.target_time()) {
                HeldRelease::Wait => self.0.push(held),
                HeldRelease::Send => sent.push(held),
                HeldRelease::Drop => {}
            }
        }
        sent
    }

    /// Drop what is due at or after `frame`.
    fn cut_from(&mut self, frame: u64, sample_rate: u32) {
        self.0
            .retain(|intent| !at_or_past(intent.target_time(), frame, sample_rate));
    }
}

/// An external intent's onset, in device-clock seconds.
trait ExternalIntent {
    fn target_time(&self) -> f64;
}

/// The `(lead, intent)` pairs the session stages OSC and serial in.
impl<I: ExternalIntent> ExternalIntent for (f64, I) {
    fn target_time(&self) -> f64 {
        self.1.target_time()
    }
}

impl ExternalIntent for MidiOnset {
    fn target_time(&self) -> f64 {
        self.target_time
    }
}

#[cfg(feature = "osc")]
impl ExternalIntent for rustel_runtime::osc_bridge::OscOnset {
    fn target_time(&self) -> f64 {
        self.target_time
    }
}

#[cfg(feature = "serial")]
impl ExternalIntent for rustel_runtime::serial_bridge::SerialOnset {
    fn target_time(&self) -> f64 {
        self.target_time
    }
}

/// One drain of the hold: when it runs, how far ahead it sends, and the
/// line cut armed but not yet passed, if one is.
struct Drain {
    device_now: f64,
    cover: f64,
    armed_line: Option<u64>,
    sample_rate: u32,
}

impl Drain {
    /// What this drain does with an intent due at `target_time`.
    fn release(&self, target_time: f64) -> HeldRelease {
        match HeldRelease::at(target_time, self.device_now, self.cover) {
            HeldRelease::Send
                if self
                    .armed_line
                    .is_some_and(|line| at_or_past(target_time, line, self.sample_rate)) =>
            {
                HeldRelease::Wait
            }
            release => release,
        }
    }
}

/// What a drain does with a held intent.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum HeldRelease {
    /// Due past the steady cover, or at or past a line not yet passed: a
    /// later drain decides.
    Wait,
    /// Due within the steady cover: this drain sends it, on time.
    Send,
    /// Its onset has passed: sent now it would sound late, bunched with
    /// whatever else fell due while no drain ran.
    Drop,
}

impl HeldRelease {
    /// For an intent due at `target_time`, drained at `device_now` with
    /// `cover` seconds of steady cover.
    fn at(target_time: f64, device_now: f64, cover: f64) -> Self {
        if target_time < device_now {
            Self::Drop
        } else if target_time < device_now + cover {
            Self::Send
        } else {
            Self::Wait
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A bare onset time stands in for a held intent.
    impl ExternalIntent for f64 {
        fn target_time(&self) -> f64 {
            *self
        }
    }

    fn held_at(targets: &[f64]) -> HeldIntents<f64> {
        HeldIntents(targets.to_vec())
    }

    const RATE: u32 = 48_000;

    /// A drain at `device_now` with `cover` seconds of steady cover.
    fn drain_at(device_now: f64, cover: f64, armed_line: Option<u64>) -> Drain {
        Drain {
            device_now,
            cover,
            armed_line,
            sample_rate: RATE,
        }
    }

    /// A held intent goes out once it is due within the steady cover, and is
    /// dropped once its onset has passed.
    #[test]
    fn a_held_intent_goes_out_within_the_steady_cover() {
        let (now, cover) = (10.0, 0.5);
        assert_eq!(HeldRelease::at(now + cover, now, cover), HeldRelease::Wait);
        assert_eq!(
            HeldRelease::at(now + cover - 0.01, now, cover),
            HeldRelease::Send
        );
        assert_eq!(HeldRelease::at(now, now, cover), HeldRelease::Send);
        assert_eq!(HeldRelease::at(now - 0.001, now, cover), HeldRelease::Drop);

        let mut hold = held_at(&[9.0, 9.999, 10.2, 10.4, 10.6, 11.0]);
        assert_eq!(hold.release(&drain_at(now, cover, None)), [10.2, 10.4]);
        assert_eq!(hold.0, [10.6, 11.0]);
    }

    /// A line armed and not yet passed keeps what lies at or past its frame
    /// waiting, on the frame the audio gives the onset: one just inside the
    /// line's frame, or within float dust of the line, is at it.
    #[test]
    fn a_line_not_yet_passed_keeps_what_lies_at_it_waiting() {
        let line: u64 = 96_001;
        let line_seconds = line as f64 / f64::from(RATE);
        let before = (line as f64 - 1.0) / f64::from(RATE);
        let just_inside = (line as f64 - 0.4) / f64::from(RATE);
        let mut hold = held_at(&[before, just_inside, line_seconds - 1e-12, line_seconds]);
        assert_eq!(
            hold.release(&drain_at(line_seconds - 0.2, 0.5, Some(line))),
            [before]
        );
        assert_eq!(hold.0, [just_inside, line_seconds - 1e-12, line_seconds]);
    }

    /// A takeover, a fired line cut or a hush drops what is held from its frame
    /// on, on the frame the audio gives the onset: one within float dust of the
    /// frame is at it.
    #[test]
    fn a_cut_drops_what_is_held_from_its_frame_on() {
        let takeover = 2.0;
        let frame = 96_000;
        let step = 1.0 / f64::from(RATE);
        let mut hold = held_at(&[takeover - step, takeover - 1e-12, takeover, takeover + step]);
        hold.cut_from(frame, RATE);
        assert_eq!(hold.0, [takeover - step]);
    }
}
