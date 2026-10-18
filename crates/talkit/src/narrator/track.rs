//! Parameter tracks: each sound's targets, joined by smooth transitions.
//!
//! A track is its first target plus one smooth step per change of target.
//! A step starts before the boundary between two sounds and ends after it,
//! so steps overlap when a sound is short, and a short sound then falls
//! short of its own target, as speech does.
//!
//! ```text
//! target  ─────┐            ┌──────
//!              └────────────┘
//! track   ────╮              ╭────
//!              ╰──╮      ╭──╯
//!                  ╰────╯
//! ```
//!
//! A stop is a silent closure, then a burst of noise at its end. After a
//! voiceless stop the next sound starts with breath before the voice comes
//! in. An `h` is breath through the shape of the sound after it.

use super::synth::Frame;
use super::table::{Class, Hiss, Target, target, tract};
use super::timing::Seg;
use crate::personality::Timbre;
use crate::phonetic::Ph;

/// The channels a frame is built from.
const F1: usize = 0;
const B1: usize = 3;
const AV: usize = 6;
const AH: usize = 7;
const AF: usize = 8;
const CHANNELS: usize = 9;

/// The burst of a stop, ms at speed 150.
const BURST_MS: f32 = 25.0;
/// The voice bar of a voiced stop's closure, against its voicing.
const VOICE_BAR: f32 = 0.15;
/// The breath under every voiced sound.
const BREATH: f32 = 0.93;
/// The breath of an `h` and of a voiceless stop's release.
const ASPIRATION: f32 = 1.86;
/// The sources fade out over this long before the last sound ends, ms at
/// speed 150.
const END_FADE_MS: f32 = 40.0;

/// How long the breath after a voiceless stop lasts, ms at speed 150.
fn aspiration_ms(ph: Ph) -> f32 {
    match ph {
        Ph::P => 25.0,
        Ph::T => 57.0,
        Ph::K => 52.0,
        _ => 0.0,
    }
}

/// One smooth step of a channel: `delta` spread from `at - before` to
/// `at + after`, ms.
#[derive(Clone, Copy, Debug)]
struct Step {
    at: f32,
    before: f32,
    after: f32,
    delta: f32,
}

impl Step {
    /// The share of the step done at `t`: 0 before, 1 after, two half
    /// parabolas in between that meet at the boundary.
    fn done(&self, t: f32) -> f32 {
        let x = t - self.at;
        if x <= -self.before {
            0.0
        } else if x >= self.after {
            1.0
        } else if x < 0.0 {
            let u = (x + self.before) / self.before;
            0.5 * u * u
        } else {
            let u = (self.after - x) / self.after;
            1.0 - 0.5 * u * u
        }
    }
}

/// The tracks of one utterance.
pub(super) struct Tracks {
    /// The value of each channel with every finished step added.
    settled: [f32; CHANNELS],
    /// Each channel's steps by start time.
    steps: [Vec<Step>; CHANNELS],
    /// The first step of each channel not yet started.
    next: [usize; CHANNELS],
    /// The steps under way.
    active: [Vec<Step>; CHANNELS],
    /// The noise colors by time: from this ms on, this hiss.
    hisses: Vec<(f32, Hiss)>,
    next_hiss: usize,
}

/// One sound's values on every channel, through the voice's tract and
/// its formant and bandwidth factors.
fn values(tg: &Target, to_end: bool, timbre: &Timbre) -> [f32; CHANNELS] {
    let tract = tract(timbre.tract);
    let f = if to_end { tg.to } else { tg.f };
    let mut v = [0.0; CHANNELS];
    for k in 0..3 {
        v[F1 + k] = f[k] * tract.f[k] * f32::from(timbre.formants[k]) / 100.0;
        let scale = (f32::from(timbre.bandwidths[k]) / 100.0).min(4.0);
        v[B1 + k] = (tg.b[k] * tract.b[k] * scale).min(512.0);
    }
    v[AV] = tg.av;
    v[AH] = tg.ah + if tg.av > 0.0 { BREATH } else { 0.0 };
    v[AF] = tg.af;
    v
}

/// The silence between phrases: no source, a neutral tract.
fn silence() -> Target {
    let mut tg = target(Ph::Q);
    tg.av = 0.0;
    tg
}

/// Before and after a boundary: the formant spread, then the source spread.
type Spread = ((f32, f32), (f32, f32));

/// How far a change spreads before and after a boundary, ms at speed 150,
/// for the formant channels and for the source levels, by the classes on
/// each side. Fitted to the original engine's plain male voice.
#[rustfmt::skip]
const SPREADS: &[((Class, Class), Spread)] = {
    use Class::*;
    &[
        ((Aspirate, Diphthong), ((16.0, 20.0), (20.0, 23.0))),
        ((Aspirate, Vowel), ((13.0, 17.0), (58.0, 12.0))),
        ((Diphthong, Fricative), ((87.0, 23.0), (8.0, 30.0))),
        ((Diphthong, Sonorant), ((36.0, 2.0), (245.0, 64.0))),
        ((Diphthong, Stop), ((86.0, 36.0), (1.0, 14.0))),
        ((Diphthong, VoicedFricative), ((56.0, 29.0), (13.0, 28.0))),
        ((Diphthong, Vowel), ((95.0, 151.0), (16.0, 94.0))),
        ((Fricative, Diphthong), ((15.0, 123.0), (14.0, 23.0))),
        ((Fricative, Fricative), ((6.0, 27.0), (7.0, 38.0))),
        ((Fricative, Sonorant), ((23.0, 40.0), (17.0, 3.0))),
        ((Fricative, Stop), ((27.0, 21.0), (23.0, 1.0))),
        ((Fricative, VoicedStop), ((60.0, 11.0), (2.0, 7.0))),
        ((Fricative, Vowel), ((26.0, 97.0), (14.0, 10.0))),
        ((Sonorant, Aspirate), ((20.0, 20.0), (8.0, 8.0))),
        ((Sonorant, Diphthong), ((20.0, 65.0), (19.0, 45.0))),
        ((Sonorant, Fricative), ((7.0, 24.0), (17.0, 35.0))),
        ((Sonorant, Sonorant), ((28.0, 78.0), (25.0, 64.0))),
        ((Sonorant, Stop), ((14.0, 33.0), (12.0, 17.0))),
        ((Sonorant, VoicedFricative), ((17.0, 31.0), (1.0, 25.0))),
        ((Sonorant, VoicedStop), ((14.0, 29.0), (25.0, 16.0))),
        ((Sonorant, Vowel), ((29.0, 55.0), (12.0, 82.0))),
        ((Stop, Diphthong), ((10.0, 150.0), (5.0, 9.0))),
        ((Stop, Fricative), ((5.0, 60.0), (10.0, 10.0))),
        ((Stop, Sonorant), ((18.0, 66.0), (1.0, 3.0))),
        ((Stop, Stop), ((24.0, 20.0), (4.0, 17.0))),
        ((Stop, VoicedFricative), ((16.0, 22.0), (17.0, 23.0))),
        ((Stop, VoicedStop), ((40.0, 21.0), (7.0, 11.0))),
        ((Stop, Vowel), ((13.0, 149.0), (15.0, 20.0))),
        ((VoicedFricative, Fricative), ((27.0, 32.0), (51.0, 60.0))),
        ((VoicedFricative, Sonorant), ((25.0, 16.0), (20.0, 26.0))),
        ((VoicedFricative, Stop), ((6.0, 9.0), (69.0, 9.0))),
        ((VoicedFricative, VoicedFricative), ((81.0, 1.0), (2.0, 49.0))),
        ((VoicedFricative, VoicedStop), ((18.0, 20.0), (8.0, 23.0))),
        ((VoicedFricative, Vowel), ((14.0, 80.0), (34.0, 2.0))),
        ((VoicedStop, Diphthong), ((36.0, 56.0), (43.0, 1.0))),
        ((VoicedStop, Sonorant), ((8.0, 82.0), (76.0, 36.0))),
        ((VoicedStop, Stop), ((32.0, 27.0), (13.0, 4.0))),
        ((VoicedStop, VoicedFricative), ((2.0, 12.0), (30.0, 57.0))),
        ((VoicedStop, Vowel), ((1.0, 96.0), (34.0, 10.0))),
        ((Vowel, Aspirate), ((89.0, 67.0), (101.0, 11.0))),
        ((Vowel, Diphthong), ((7.0, 144.0), (102.0, 5.0))),
        ((Vowel, Fricative), ((74.0, 30.0), (17.0, 9.0))),
        ((Vowel, Sonorant), ((60.0, 22.0), (56.0, 28.0))),
        ((Vowel, Stop), ((44.0, 42.0), (1.0, 15.0))),
        ((Vowel, VoicedFricative), ((51.0, 25.0), (19.0, 16.0))),
        ((Vowel, VoicedStop), ((47.0, 23.0), (10.0, 18.0))),
        ((Vowel, Vowel), ((92.0, 89.0), (35.0, 50.0))),
    ]
};

/// A pair the fit did not see takes a diphthong's spread as a vowel's, and
/// a short default after that.
fn spread(left: Class, right: Class) -> Spread {
    let plain = |c: Class| {
        if c == Class::Diphthong {
            Class::Vowel
        } else {
            c
        }
    };
    [(left, right), (plain(left), plain(right))]
        .iter()
        .find_map(|pair| SPREADS.iter().find(|(p, _)| p == pair))
        .map_or(((20.0, 20.0), (8.0, 8.0)), |(_, s)| *s)
}

/// The steps under construction, with each channel's current value.
struct Builder {
    now: [f32; CHANNELS],
    steps: [Vec<Step>; CHANNELS],
}

impl Builder {
    /// Move channel `ch` to `value` around `at`.
    fn set(&mut self, ch: usize, at: f32, value: f32, before: f32, after: f32) {
        let delta = value - self.now[ch];
        if delta != 0.0 {
            self.steps[ch].push(Step {
                at,
                before: before.max(1.0),
                after: after.max(1.0),
                delta,
            });
            self.now[ch] = value;
        }
    }
}

/// Each sound's target. An `h` takes the shape of the sound after it.
fn targets(segs: &[Seg]) -> Vec<Target> {
    (0..segs.len())
        .map(|i| {
            let mut tg = segs[i].ph.map_or_else(silence, target);
            if tg.class == Class::Aspirate {
                let shape = segs[i + 1..]
                    .iter()
                    .find_map(|s| s.ph.map(target).filter(|t| t.class != Class::Aspirate))
                    .unwrap_or(tg);
                tg.f = shape.f;
                tg.b = shape.b;
                tg.to = shape.f;
                tg.ah = ASPIRATION;
            }
            tg
        })
        .collect()
}

impl Tracks {
    pub(super) fn build(segs: &[Seg], speed: u32, timbre: &Timbre) -> Self {
        let scale = 150.0 / (speed & !1).max(2) as f32;
        let targets = targets(segs);
        let first = targets.first().copied().unwrap_or_else(silence);
        let start = values(&first, false, timbre);
        let mut b = Builder {
            now: start,
            steps: Default::default(),
        };
        let mut prev_class = first.class;
        // Breath after a voiceless stop: where it ends.
        let mut breath_until: Option<f32> = None;
        for (i, seg) in segs.iter().enumerate() {
            let tg = targets[i];
            let here = values(&tg, false, timbre);
            let dur = seg.t1 - seg.t0;
            if i > 0 {
                let ((fb, fa), (ab, aa)) = spread(prev_class, tg.class);
                // A change cannot spread past the middle of a sound.
                let left = (segs[i - 1].t1 - segs[i - 1].t0) * 0.9;
                let right = dur * 0.9;
                for (ch, &value) in here.iter().enumerate() {
                    let source = (AV..=AF).contains(&ch);
                    let (before, after) = if source { (ab, aa) } else { (fb, fa) };
                    let (before, after) = ((before * scale).min(left), (after * scale).min(right));
                    match (ch, breath_until) {
                        // The voice waits for the breath to end.
                        (AV, Some(end)) => b.set(ch, end, value, 3.0 * scale, aa * scale),
                        (AH, Some(end)) => {
                            b.set(ch, seg.t0, ASPIRATION, 2.0, 2.0);
                            b.set(ch, end, value, 3.0 * scale, 10.0 * scale);
                        }
                        _ => b.set(ch, seg.t0, value, before, after),
                    }
                }
            }
            breath_until = None;
            match tg.class {
                Class::Diphthong => {
                    let end = values(&tg, true, timbre);
                    for (ch, &v) in end.iter().enumerate().take(B1) {
                        b.set(ch, seg.t0 + dur * 0.5, v, dur * 0.35, dur * 0.35);
                    }
                }
                Class::Stop | Class::VoicedStop => {
                    let voiced = tg.class == Class::VoicedStop;
                    let bar = if voiced {
                        b.now[AV].max(tg.av) * VOICE_BAR
                    } else {
                        0.0
                    };
                    b.set(AV, seg.t0, bar, 1.0, 3.0 * scale);
                    b.set(AH, seg.t0, 0.0, 1.0, 3.0 * scale);
                    b.set(AF, seg.t0, 0.0, 1.0, 1.0);
                    let burst = (seg.t1 - BURST_MS * scale).max(seg.t0 + dur * 0.3);
                    b.set(AF, burst, tg.af, 1.0, 2.0);
                    b.set(AF, seg.t1, 0.0, 2.0, 8.0 * scale);
                    let breath = seg.ph.map_or(0.0, aspiration_ms) * scale;
                    if breath > 0.0 {
                        breath_until = Some(seg.t1 + breath);
                    }
                }
                _ => {}
            }
            prev_class = tg.class;
        }
        // Voicing fades out at the end, and every channel goes quiet.
        let end = segs.last().map_or(0.0, |s| s.t1);
        for ch in [AV, AH, AF] {
            b.set(ch, end, 0.0, END_FADE_MS * scale, 1.0);
        }
        let mut steps = b.steps;
        for list in &mut steps {
            list.sort_by(|a, b| (a.at - a.before).total_cmp(&(b.at - b.before)));
        }
        // Each noisy sound colors the noise from halfway into the sound
        // before it, so its own ramps carry its color.
        let mut hisses = Vec::new();
        for (i, seg) in segs.iter().enumerate() {
            if targets[i].af > 0.0 {
                let from = i
                    .checked_sub(1)
                    .map_or(seg.t0, |p| (segs[p].t0 + segs[p].t1) / 2.0);
                hisses.push((from, targets[i].hiss));
            }
        }
        Self {
            settled: start,
            steps,
            next: [0; CHANNELS],
            active: Default::default(),
            hisses,
            next_hiss: 0,
        }
    }

    /// The frame at `t` ms, without the pitch. Times must not go back.
    pub(super) fn at(&mut self, t: f32) -> Frame {
        let mut v = self.settled;
        for (ch, value) in v.iter_mut().enumerate() {
            let list = &self.steps[ch];
            while let Some(step) = list.get(self.next[ch])
                && step.at - step.before <= t
            {
                self.active[ch].push(*step);
                self.next[ch] += 1;
            }
            let settled = &mut self.settled[ch];
            self.active[ch].retain(|step| {
                let done = step.at + step.after <= t;
                if done {
                    *settled += step.delta;
                }
                !done
            });
            *value = self.settled[ch]
                + self.active[ch]
                    .iter()
                    .map(|step| step.delta * step.done(t))
                    .sum::<f32>();
        }
        while self.next_hiss + 1 < self.hisses.len() && self.hisses[self.next_hiss + 1].0 <= t {
            self.next_hiss += 1;
        }
        Frame {
            f0: 0.0,
            av: v[AV].max(0.0),
            ah: v[AH].max(0.0),
            af: v[AF].max(0.0),
            f: [v[F1], v[F1 + 1], v[F1 + 2]],
            b: [v[B1], v[B1 + 1], v[B1 + 2]],
            hiss: self.hisses.get(self.next_hiss).map_or(Hiss::NONE, |h| h.1),
        }
    }
}
