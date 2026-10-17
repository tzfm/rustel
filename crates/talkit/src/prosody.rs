//! The pitch contour of an utterance.
//!
//! The plan owns pitch over time and nothing else. It anchors one target to
//! each syllable nucleus, then answers `f0_at(ms)` for the synthesizer. All
//! variation is derived deterministically from the nucleus index, so the same
//! text and settings always render the same wave.
//!
//! The base pitch is the pitch of the first syllable in every quality.
//! Monotone holds it, natural drifts down from it and sung walks a chord up
//! from it, so a one-syllable word sounds exactly at the pitch a note asks
//! for.

use crate::params::Quality;
use crate::utterance::Terminal;

/// The semitone steps sung mode walks through, relative to the base pitch.
const SUNG_DEGREES: [f32; 4] = [0.0, 4.0, 7.0, 4.0];
/// Declination in natural mode: from the first syllable the voice drifts
/// down to this share of the base pitch by the end of the utterance.
const DECLINE_TO: f32 = 0.83;
/// How far a terminal glide moves the pitch by the end of the last syllable.
const FALL_TO: f32 = 0.82;
const RISE_TO: f32 = 1.28;
/// Sung mode slides between degrees over this many ms.
const PORTAMENTO_MS: f32 = 70.0;
const VIBRATO_HZ: f32 = 5.5;
const VIBRATO_DEPTH: f32 = 0.007;

/// Pitch targets for one utterance.
pub struct Plan {
    base_hz: f32,
    quality: Quality,
    /// Start and end in ms of each syllable nucleus.
    nuclei: Vec<(f32, f32)>,
    /// The Hz each nucleus aims at, after style, declination and jitter.
    targets: Vec<f32>,
    terminal: Terminal,
}

impl Plan {
    /// Build the contour for an utterance with syllable nuclei `(start, end)`
    /// in ms.
    pub fn new(
        base_hz: f32,
        quality: Quality,
        nuclei: &[(f32, f32)],
        total_ms: f32,
        terminal: Terminal,
    ) -> Self {
        let targets = match quality {
            Quality::Natural => {
                let first = nuclei.first().map_or(0.0, |&(start, _)| start);
                let span = (total_ms - first).max(1.0);
                nuclei
                    .iter()
                    .enumerate()
                    .map(|(i, &(start, _))| {
                        let decl = 1.0 + (DECLINE_TO - 1.0) * ((start - first) / span);
                        base_hz * decl * jitter(i)
                    })
                    .collect()
            }
            Quality::Monotone => nuclei.iter().map(|_| base_hz).collect(),
            Quality::Sung => nuclei
                .iter()
                .enumerate()
                .map(|(i, _)| base_hz * semitones(SUNG_DEGREES[i % SUNG_DEGREES.len()]))
                .collect(),
        };
        Self {
            base_hz,
            quality,
            nuclei: nuclei.to_vec(),
            targets,
            terminal,
        }
    }

    /// The fundamental frequency at `ms` into the utterance.
    pub fn f0_at(&self, ms: f32) -> f32 {
        // The last nucleus to start by `ms`: the one `ms` falls in, or the
        // one whose pitch holds through the consonants after it. Before the
        // first nucleus the voice is on the base pitch.
        let k = match self.nuclei.partition_point(|&(start, _)| start <= ms) {
            0 => return self.base_hz,
            after => after - 1,
        };
        let (start, end) = self.nuclei[k];
        match self.quality {
            Quality::Monotone => self.targets[k],
            Quality::Natural => {
                let mut hz = self.targets[k];
                if k == self.nuclei.len() - 1 {
                    hz *= self.terminal_factor(ms, start, end);
                }
                hz
            }
            Quality::Sung => {
                let prev = if k == 0 {
                    self.targets[0]
                } else {
                    self.targets[k - 1]
                };
                let into = (ms - start).min(PORTAMENTO_MS) / PORTAMENTO_MS;
                let mut hz = prev + (self.targets[k] - prev) * into;
                hz *= 1.0 + (VIBRATO_DEPTH * (TAU * VIBRATO_HZ * ms / 1000.0).sin());
                hz
            }
        }
    }

    /// The factor a terminal glide holds at `ms` across the last syllable.
    fn terminal_factor(&self, ms: f32, start: f32, end: f32) -> f32 {
        let span = (end - start).max(1.0);
        let t = ((ms - start) / span).clamp(0.0, 1.0);
        match self.terminal {
            Terminal::Fall => 1.0 + (FALL_TO - 1.0) * t,
            Terminal::Rise => 1.0 + (RISE_TO - 1.0) * t,
            Terminal::None => 1.0,
        }
    }
}

const TAU: f32 = std::f32::consts::TAU;

fn semitones(semitones: f32) -> f32 {
    (2.0f32).powf(semitones / 12.0)
}

/// Deterministic per-syllable drift of up to 2%, so natural mode never
/// repeats a syllable at exactly the same pitch. The first syllable keeps
/// the base pitch.
fn jitter(i: usize) -> f32 {
    if i == 0 {
        return 1.0;
    }
    let h = (i as u32).wrapping_mul(2654435761);
    let unit = (h >> 24) as f32 / 16777216.0;
    1.0 + (unit - 0.5) * 0.04
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plan(quality: Quality, terminal: Terminal) -> Plan {
        let nuclei = vec![(0.0, 120.0), (200.0, 320.0), (400.0, 520.0)];
        Plan::new(100.0, quality, &nuclei, 600.0, terminal)
    }

    #[test]
    fn monotone_stays_exactly_on_the_base_pitch() {
        let p = plan(Quality::Monotone, Terminal::Fall);
        for ms in (0..600).step_by(7) {
            assert!((p.f0_at(ms as f32) - 100.0).abs() < 1e-4, "at {ms} ms");
        }
    }

    #[test]
    fn natural_starts_on_the_base_declines_and_glides_at_the_terminal() {
        let p = plan(Quality::Natural, Terminal::Fall);
        assert!((p.f0_at(60.0) - 100.0).abs() < 1e-4, "{}", p.f0_at(60.0));
        assert!(p.f0_at(60.0) > p.f0_at(260.0), "expected a decline");
        let falling = plan(Quality::Natural, Terminal::Fall);
        let flat = plan(Quality::Natural, Terminal::None);
        assert!(falling.f0_at(500.0) < flat.f0_at(500.0));
        let rising = plan(Quality::Natural, Terminal::Rise);
        assert!(rising.f0_at(500.0) > flat.f0_at(500.0));
    }

    #[test]
    fn sung_steps_through_scale_degrees_with_vibrato() {
        let p = plan(Quality::Sung, Terminal::None);
        // At the start of the first nucleus the slide begins at the first
        // degree, which is the base itself.
        assert!((p.f0_at(0.0) - 100.0).abs() < 1.0);
        // Well into the second nucleus the pitch sits near the second degree
        // (four semitones up), vibrato aside.
        let expected = 100.0 * semitones(4.0);
        let got = p.f0_at(300.0);
        assert!((got - expected).abs() / expected < 0.02, "got {got}");
    }
}
