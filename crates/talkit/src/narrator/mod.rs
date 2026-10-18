//! Phonetic strings to sound: the original engine's design, rebuilt from
//! measurements of its output.
//!
//! ```text
//! phonetic items ─> timing ─> segments ─┬─> tracks ─┐
//!                                       └─> pitch ──┴─> synth ─> samples
//! ```

mod allophone;
mod glottis;
mod pitch;
mod synth;
mod table;
mod timing;
mod track;

use crate::RATE;
use crate::params::{Effort, Params, Quality};
use crate::phonetic::Item;

/// Silence kept after the last sound, ms, so the voice can ring out.
const TAIL_MS: f32 = 60.0;
/// One unit of the voice record's level offsets, in dB.
const BIAS_DB: f32 = 0.95;
/// Breathy speech: louder voicing with breath of about a third of it.
const BREATHY_VOICE: f32 = 1.33;
const BREATHY_BREATH: f32 = 0.28;
/// Whispered speech: breath in place of the voice, near its level.
const WHISPER: f32 = 0.48;

fn bias(db: i8) -> f32 {
    // Far down, a source is off.
    if db <= -40 {
        return 0.0;
    }
    10f32.powf(f32::from(db) * BIAS_DB / 20.0)
}

/// Speak `items` with `params`. Returns samples from -1 to 1 at
/// [`crate::RATE`], in the original engine's level and 8-bit steps.
pub(crate) fn speak(items: &[Item], params: &Params) -> Vec<f32> {
    let timbre = params.timbre;
    let mut items = items.to_vec();
    allophone::apply(&mut items);
    let segs = timing::build(&items, params.speed, timbre.vowels);
    let Some(last) = segs.last() else {
        return Vec::new();
    };
    let total_ms = last.t1 + TAIL_MS;
    let mut tracks = track::Tracks::build(&segs, params.speed, &timbre);
    let mut pitch = pitch::Pitch::plan(&segs, params.quality, params.pitch, params.speed, &timbre);
    let upper = timbre.upper.map(|(hz, bw)| (f32::from(hz), f32::from(bw)));
    let tract = table::tract(timbre.tract);
    let mut synth = synth::Synth::new(timbre.glottis, upper, timbre.tilt, tract.hiss);
    let (av, ah) = (bias(timbre.av), bias(timbre.ah));
    let af = bias(timbre.af) * 10f32.powf(tract.af / 20.0);
    let frames = (total_ms * RATE as f32 / 1000.0) as usize;
    let mut out = Vec::with_capacity(frames);
    for n in 0..frames {
        let t = n as f32 * 1000.0 / RATE as f32;
        let mut fr = tracks.at(t);
        fr.f0 = pitch.at(t);
        fr.av *= av;
        fr.ah *= ah;
        fr.af *= af;
        match params.effort {
            Effort::Normal => {}
            Effort::Breathy => {
                fr.ah += fr.av * BREATHY_BREATH;
                fr.av *= BREATHY_VOICE;
            }
            Effort::Whispered => {
                fr.ah += fr.av * WHISPER;
                fr.av = 0.0;
            }
        }
        fr.av *= tract.voice_gain(fr.f0);
        if params.quality != Quality::Monotone {
            fr.av *= pitch::loudness(fr.f0, params.pitch);
        }
        let silent = fr.av == 0.0 && fr.ah == 0.0 && fr.af == 0.0;
        out.push(synth::to_output(synth.next(&fr), silent));
    }
    out
}
