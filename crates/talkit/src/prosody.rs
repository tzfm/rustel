//! The pitch contour of an utterance.
//!
//! The plan owns pitch over time and nothing else. It anchors one target to
//! each syllable nucleus, then answers `f0_at(ms)` for the synthesizer. All
//! variation is derived deterministically from the nucleus index, so the same
//! text and settings always render the same wave.
//!
//! The base pitch is the pitch of the first syllable in every quality.
//! Monotone holds it and sung walks a chord up from it. Natural speaks the
//! way the original engine does, phrase by phrase: a stressed syllable rises
//! above its neighbors, the voice sinks a little across each phrase and
//! starts again after a pause, a question sits higher and rises at its end,
//! and a statement falls at its end. The whole curve is scaled so the first
//! syllable still sits exactly on the base pitch a note asks for.

use crate::params::Quality;
use crate::utterance::Terminal;

/// One syllable nucleus as the plan sees it.
#[derive(Clone, Copy, Debug)]
pub struct Nucleus {
    /// Start and end in ms from the utterance start.
    pub start: f32,
    pub end: f32,
    /// True when the language rules stressed the syllable.
    pub accent: bool,
    /// The phrase the syllable belongs to, an index into the utterance's
    /// phrase endings.
    pub phrase: usize,
}

/// The semitone steps sung mode walks through, relative to the base pitch.
const SUNG_DEGREES: [f32; 4] = [0.0, 4.0, 7.0, 4.0];
/// A stressed syllable sits this much above an unstressed one.
const ACCENT: f32 = 1.2;
/// Across one phrase the voice sinks to this share of where it started.
const PHRASE_DECLINE: f32 = 0.92;
/// A question is spoken this much higher.
const QUESTION_LIFT: f32 = 1.15;
/// How far the last syllable of a phrase glides by its end: down at a full
/// stop, up at a question mark, a little up at a comma.
const FALL_TO: f32 = 0.75;
const RISE_TO: f32 = 1.3;
const CONTINUE_TO: f32 = 1.05;
/// Natural and sung modes slide from one syllable's pitch to the next over
/// this many ms.
const GLIDE_MS: f32 = 70.0;
const VIBRATO_HZ: f32 = 5.5;
const VIBRATO_DEPTH: f32 = 0.007;

/// Pitch targets for one utterance.
pub struct Plan {
    base_hz: f32,
    quality: Quality,
    nuclei: Vec<Nucleus>,
    /// The Hz each nucleus aims at.
    targets: Vec<f32>,
    /// How each phrase ends.
    phrases: Vec<Terminal>,
}

impl Plan {
    /// Build the contour for an utterance's syllable nuclei, in time order,
    /// and the endings of its phrases.
    pub fn new(base_hz: f32, quality: Quality, nuclei: &[Nucleus], phrases: &[Terminal]) -> Self {
        let targets = match quality {
            Quality::Natural => natural_targets(base_hz, nuclei, phrases),
            Quality::Monotone => vec![base_hz; nuclei.len()],
            Quality::Sung => (0..nuclei.len())
                .map(|i| base_hz * semitones(SUNG_DEGREES[i % SUNG_DEGREES.len()]))
                .collect(),
        };
        Self {
            base_hz,
            quality,
            nuclei: nuclei.to_vec(),
            targets,
            phrases: phrases.to_vec(),
        }
    }

    /// The fundamental frequency at `ms` into the utterance.
    pub fn f0_at(&self, ms: f32) -> f32 {
        // The last nucleus to start by `ms`: the one `ms` falls in, or the
        // one whose pitch holds through the consonants after it. Before the
        // first nucleus the voice is on the base pitch.
        let k = match self.nuclei.partition_point(|n| n.start <= ms) {
            0 => return self.base_hz,
            after => after - 1,
        };
        if self.quality == Quality::Monotone {
            return self.targets[k];
        }
        let nucleus = self.nuclei[k];
        let prev = self.targets[k.saturating_sub(1)];
        let into = (ms - nucleus.start).min(GLIDE_MS) / GLIDE_MS;
        let mut hz = prev + (self.targets[k] - prev) * into;
        match self.quality {
            Quality::Sung => {
                hz *= 1.0 + (VIBRATO_DEPTH * (TAU * VIBRATO_HZ * ms / 1000.0).sin());
            }
            _ => {
                let ends_phrase = self
                    .nuclei
                    .get(k + 1)
                    .is_none_or(|next| next.phrase != nucleus.phrase);
                if ends_phrase {
                    hz *= self.terminal_factor(ms, nucleus);
                }
            }
        }
        hz
    }

    /// The factor a phrase's ending holds at `ms` across its last syllable.
    fn terminal_factor(&self, ms: f32, nucleus: Nucleus) -> f32 {
        let span = (nucleus.end - nucleus.start).max(1.0);
        let t = ((ms - nucleus.start) / span).clamp(0.0, 1.0);
        let to = match self.phrases.get(nucleus.phrase) {
            Some(Terminal::Fall) => FALL_TO,
            Some(Terminal::Rise) => RISE_TO,
            Some(Terminal::Continue) => CONTINUE_TO,
            Some(Terminal::None) | None => 1.0,
        };
        1.0 + (to - 1.0) * t
    }
}

/// The natural targets: each phrase sinks from its start, stressed
/// syllables rise, questions sit higher, and the first syllable lands on
/// the base pitch.
fn natural_targets(base_hz: f32, nuclei: &[Nucleus], phrases: &[Terminal]) -> Vec<f32> {
    let mut targets = Vec::with_capacity(nuclei.len());
    let mut phrase_start = 0;
    for (i, nucleus) in nuclei.iter().enumerate() {
        if i > 0 && nuclei[i - 1].phrase != nucleus.phrase {
            phrase_start = i;
        }
        let count = nuclei[phrase_start..]
            .iter()
            .take_while(|n| n.phrase == nucleus.phrase)
            .count();
        let progress = if count > 1 {
            (i - phrase_start) as f32 / (count - 1) as f32
        } else {
            0.0
        };
        let mut level = 1.0 + (PHRASE_DECLINE - 1.0) * progress;
        if nucleus.accent {
            level *= ACCENT;
        }
        if phrases.get(nucleus.phrase) == Some(&Terminal::Rise) {
            level *= QUESTION_LIFT;
        }
        targets.push(level * jitter(i));
    }
    let scale = base_hz / targets.first().copied().unwrap_or(1.0);
    targets.iter().map(|t| t * scale).collect()
}

const TAU: f32 = std::f32::consts::TAU;

fn semitones(semitones: f32) -> f32 {
    (2.0f32).powf(semitones / 12.0)
}

/// Deterministic per-syllable drift of up to 2%, so natural mode never
/// repeats a syllable at exactly the same pitch.
fn jitter(i: usize) -> f32 {
    let h = (i as u32).wrapping_mul(2654435761);
    let unit = (h >> 24) as f32 / 16777216.0;
    1.0 + (unit - 0.5) * 0.04
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Three syllables of one phrase: the middle one stressed.
    fn plan(quality: Quality, ending: Terminal) -> Plan {
        let nucleus = |start: f32, accent: bool| Nucleus {
            start,
            end: start + 120.0,
            accent,
            phrase: 0,
        };
        let nuclei = [
            nucleus(0.0, false),
            nucleus(200.0, true),
            nucleus(400.0, false),
        ];
        Plan::new(100.0, quality, &nuclei, &[ending])
    }

    #[test]
    fn monotone_stays_exactly_on_the_base_pitch() {
        let p = plan(Quality::Monotone, Terminal::Fall);
        for ms in (0..600).step_by(7) {
            assert!((p.f0_at(ms as f32) - 100.0).abs() < 1e-4, "at {ms} ms");
        }
    }

    #[test]
    fn natural_starts_on_the_base_rises_on_stress_and_glides_at_the_end() {
        let p = plan(Quality::Natural, Terminal::Fall);
        assert!((p.f0_at(60.0) - 100.0).abs() < 1e-3, "{}", p.f0_at(60.0));
        assert!(p.f0_at(300.0) > 110.0, "the stressed syllable rises");
        let flat = plan(Quality::Natural, Terminal::None);
        assert!(p.f0_at(500.0) < flat.f0_at(500.0), "a full stop falls");
        let rising = plan(Quality::Natural, Terminal::Rise);
        assert!(rising.f0_at(500.0) > flat.f0_at(500.0), "a question rises");
    }

    #[test]
    fn natural_starts_each_phrase_again() {
        let nucleus = |start: f32, phrase: usize| Nucleus {
            start,
            end: start + 100.0,
            accent: false,
            phrase,
        };
        let nuclei: Vec<Nucleus> = (0..8).map(|k| nucleus(k as f32 * 150.0, k / 4)).collect();
        let p = Plan::new(
            100.0,
            Quality::Natural,
            &nuclei,
            &[Terminal::Continue, Terminal::None],
        );
        // The second phrase opens near the first one's opening, not where
        // the first phrase sank to.
        let sunk = p.targets[3];
        let again = p.targets[4];
        assert!(again > sunk * 1.05, "{again} after {sunk}");
    }

    #[test]
    fn sung_steps_through_scale_degrees_with_vibrato() {
        let p = plan(Quality::Sung, Terminal::None);
        assert!((p.f0_at(0.0) - 100.0).abs() < 1.0);
        let expected = 100.0 * semitones(4.0);
        let got = p.f0_at(300.0);
        assert!((got - expected).abs() / expected < 0.02, "got {got}");
    }
}
