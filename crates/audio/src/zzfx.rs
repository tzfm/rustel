//! ZzFX, streamed.
//!
//! A ZzFX voice plays with no outer envelope and no stop: it runs to its own
//! end whatever the event's duration. ZzFX's `buildSamples` pre-renders a
//! mono buffer; this voice computes the same loop one sample at a time
//! instead, because the only backward reference in the whole algorithm is
//! the `zdelay` tap - everything else is a function of the running state -
//! and a streaming voice needs no allocation on the audio thread.
//!
//! The compact ZzFX source has several details that a direct translation
//! gets wrong; a comment marks each one at its line. Two rules apply
//! throughout: ZzFX arithmetic is defined on doubles, so all state is f64 and
//! the f32 conversion happens once at the output; and the arithmetic keeps
//! the source's form, because a simplification changes the sound.

/// The twenty ZzFX parameters, in `buildSamples` order, plus the one random
/// draw made at build time. POD: this rides inside the onset event.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ZzfxParams {
    pub volume: f64,
    pub randomness: f64,
    pub frequency: f64,
    pub attack: f64,
    pub sustain: f64,
    pub release: f64,
    /// 0 sine, 1 triangle, 2 saw, 3 tan, 4 noise. `z_square` and bare `zzfx`
    /// carry -1: it is nonzero but not `> 1`, so it takes the triangle
    /// branch. `z_square` puts that triangle through `shapeCurve` 0,
    /// `sign(tri) * |tri|^0`, which makes it square.
    pub shape: f64,
    pub shape_curve: f64,
    pub slide: f64,
    pub delta_slide: f64,
    pub pitch_jump: f64,
    pub pitch_jump_time: f64,
    pub repeat_time: f64,
    pub noise: f64,
    pub modulation: f64,
    pub bit_crush: f64,
    pub delay: f64,
    pub sustain_volume: f64,
    pub decay: f64,
    pub tremolo: f64,
    /// The one random draw: `frequency *= 1 + randomness·2·draw −
    /// randomness`. Seeded from the onset like every other stochastic
    /// source, so `zrand(0)` is bit-comparable and `zrand > 0` is judged on
    /// level.
    pub random_draw: f64,
}

impl Default for ZzfxParams {
    /// ZzFX's stock defaults. Named controls override most of them, but a
    /// raw `zzfx([...])` array shorter than twenty falls back to these.
    fn default() -> Self {
        Self {
            volume: 1.0,
            randomness: 0.05,
            frequency: 220.0,
            attack: 0.0,
            sustain: 0.0,
            release: 0.1,
            shape: 0.0,
            shape_curve: 1.0,
            slide: 0.0,
            delta_slide: 0.0,
            pitch_jump: 0.0,
            pitch_jump_time: 0.0,
            repeat_time: 0.0,
            noise: 0.0,
            modulation: 0.0,
            bit_crush: 0.0,
            delay: 0.0,
            sustain_volume: 1.0,
            decay: 0.0,
            tremolo: 0.0,
            random_draw: 0.5,
        }
    }
}

impl ZzfxParams {
    /// Overwrite field `index` (0..20 in `buildSamples` order) from a raw
    /// `zzfx([...])` array entry.
    pub fn set_raw(&mut self, index: usize, value: f64) {
        match index {
            0 => self.volume = value,
            1 => self.randomness = value,
            2 => self.frequency = value,
            3 => self.attack = value,
            4 => self.sustain = value,
            5 => self.release = value,
            6 => self.shape = value,
            7 => self.shape_curve = value,
            8 => self.slide = value,
            9 => self.delta_slide = value,
            10 => self.pitch_jump = value,
            11 => self.pitch_jump_time = value,
            12 => self.repeat_time = value,
            13 => self.noise = value,
            14 => self.modulation = value,
            15 => self.bit_crush = value,
            16 => self.delay = value,
            17 => self.sustain_volume = value,
            18 => self.decay = value,
            19 => self.tremolo = value,
            _ => {}
        }
    }

    /// Field `index` in the same order, for the flat wire encoding.
    #[must_use]
    pub fn raw(&self, index: usize) -> f64 {
        match index {
            0 => self.volume,
            1 => self.randomness,
            2 => self.frequency,
            3 => self.attack,
            4 => self.sustain,
            5 => self.release,
            6 => self.shape,
            7 => self.shape_curve,
            8 => self.slide,
            9 => self.delta_slide,
            10 => self.pitch_jump,
            11 => self.pitch_jump_time,
            12 => self.repeat_time,
            13 => self.noise,
            14 => self.modulation,
            15 => self.bit_crush,
            16 => self.delay,
            17 => self.sustain_volume,
            18 => self.decay,
            19 => self.tremolo,
            _ => 0.0,
        }
    }
}

/// The running loop, initialised the way `buildSamples`' preamble scales its
/// parameters.
#[derive(Clone, Copy, Debug)]
pub struct ZzfxVoice {
    // Scaled per-sample quantities.
    frequency: f64,
    start_frequency: f64,
    slide: f64,
    start_slide: f64,
    delta_slide: f64,
    modulation: f64,
    pitch_jump: f64,
    pitch_jump_time: f64,
    /// `(repeatTime * sampleRate) | 0` - an integer, and 0 disables both the
    /// repeat and the tremolo.
    repeat_time: i64,
    /// Envelope breakpoints, in frames (f64: the comparisons stay in floats).
    attack: f64,
    decay: f64,
    sustain: f64,
    release: f64,
    delay: f64,
    /// `(bitCrush * 100) | 0`. ZERO ON THE DEFAULT PATH: ZzFX defines
    /// `c % 0` as NaN, which makes the recompute branch run every sample;
    /// an integer `%` by zero panics, so the modulus is only taken when this
    /// is non-zero, and zero means "always recompute".
    bit_crush_mod: i64,
    shape: f64,
    shape_curve: f64,
    volume: f64,
    noise: f64,
    sustain_volume: f64,
    tremolo: f64,
    /// `(attack + decay + sustain + release + delay) | 0`.
    length: f64,
    // Loop state, named as in the source.
    t: f64,
    tm: f64,
    i: f64,
    j: f64,
    r: f64,
    c: f64,
    s: f64,
}

/// Round half toward +infinity, where `f64::round` rounds half away from
/// zero. The triangle shape feeds it negative values, where they differ.
fn js_round(value: f64) -> f64 {
    (value + 0.5).floor()
}

impl ZzfxVoice {
    #[must_use]
    pub fn new(params: &ZzfxParams, sample_rate: f64) -> Self {
        let pi2 = std::f64::consts::TAU;
        // `slide *= (500 * PI2) / sampleRate / sampleRate`. The start value
        // is the scaled one, and the repeat resets to it.
        let slide = params.slide * (500.0 * pi2) / sample_rate / sample_rate;
        // `frequency *= (1 + randomness·2·random − randomness) · PI2 / rate`.
        let frequency = params.frequency
            * ((1.0 + params.randomness * 2.0 * params.random_draw - params.randomness) * pi2
                / sample_rate);
        // `attack * sampleRate + 9` - the +9 is a deliberate minimum that
        // prevents a click. Keep it.
        let attack = params.attack * sample_rate + 9.0;
        let decay = params.decay * sample_rate;
        let sustain = params.sustain * sample_rate;
        let release = params.release * sample_rate;
        let delay = params.delay * sample_rate;
        // `deltaSlide *= (500 * PI2) / sampleRate ** 3`: the divisor is the
        // cube of the rate.
        let delta_slide = params.delta_slide * (500.0 * pi2) / sample_rate.powi(3);
        let modulation = params.modulation * pi2 / sample_rate;
        let pitch_jump = params.pitch_jump * pi2 / sample_rate;
        let pitch_jump_time = params.pitch_jump_time * sample_rate;
        let repeat_time = (params.repeat_time * sample_rate).trunc() as i64;
        let length = (attack + decay + sustain + release + delay).trunc();
        Self {
            frequency,
            start_frequency: frequency,
            slide,
            start_slide: slide,
            delta_slide,
            modulation,
            pitch_jump,
            pitch_jump_time,
            repeat_time,
            attack,
            decay,
            sustain,
            release,
            delay,
            bit_crush_mod: (params.bit_crush * 100.0).trunc() as i64,
            shape: params.shape,
            shape_curve: params.shape_curve,
            volume: params.volume,
            noise: params.noise,
            sustain_volume: params.sustain_volume,
            tremolo: params.tremolo,
            length,
            t: 0.0,
            tm: 0.0,
            i: 0.0,
            j: 1.0,
            r: 0.0,
            c: 0.0,
            s: 0.0,
        }
    }

    /// The buffer's total length in frames, known before a sample is made.
    #[must_use]
    pub fn frames(&self) -> u64 {
        if self.length.is_finite() && self.length > 0.0 {
            self.length as u64
        } else {
            0
        }
    }

    /// One iteration of the generate loop: the sample `b[i]` receives, or
    /// `None` past the end.
    ///
    /// `ring` is the `zdelay` history: `ring[i % len]` holds the sample
    /// written at index `i`, so a ring at least `delay` frames long
    /// reproduces the `delay`-frames-back tap exactly. With `zdelay` 0 it is
    /// never touched, and an EMPTY ring simply mutes the tap (the voice
    /// plays undelayed rather than not at all, the pool-exhaustion
    /// convention).
    pub fn step(&mut self, ring: &mut [f32]) -> Option<f32> {
        let pi2 = std::f64::consts::TAU;
        // `i < length`, NaN-rejecting: a non-finite length (a hostile
        // parameter) compares false and the voice produces nothing.
        if self.i.partial_cmp(&self.length) != Some(std::cmp::Ordering::Less) {
            return None;
        }

        // See `bit_crush_mod`: modulus zero means the branch ALWAYS runs,
        // and on skipped iterations `s` deliberately keeps its previous
        // value. That staleness IS the crush; do not hoist the recompute.
        self.c += 1.0;
        let recompute = if self.bit_crush_mod == 0 {
            true
        } else {
            (self.c as i64) % self.bit_crush_mod == 0
        };
        if recompute {
            let t = self.t;
            let shape = self.shape;
            let mut s = if shape != 0.0 {
                if shape > 1.0 {
                    if shape > 2.0 {
                        if shape > 3.0 {
                            // 4: noise
                            ((t % pi2).powi(3)).sin()
                        } else {
                            // 3: tan, clamped
                            t.tan().clamp(-1.0, 1.0)
                        }
                    } else {
                        // 2: saw. The `+2) % 2` normalises a negative
                        // remainder; keep the double modulus as written.
                        1.0 - (((2.0 * t / pi2) % 2.0 + 2.0) % 2.0)
                    }
                } else {
                    // 1: triangle - and -1 (z_square / bare zzfx) lands here
                    // too. `js_round`, not `f64::round`: t goes negative.
                    1.0 - 4.0 * (js_round(t / pi2) - t / pi2).abs()
                }
            } else {
                t.sin()
            };

            // `sign(v) = v > 0 ? 1 : -1` - zero is negative here.
            let sign = if s > 0.0 { 1.0 } else { -1.0 };
            let i = self.i;
            let tremolo_gain = if self.repeat_time != 0 {
                1.0 - self.tremolo + self.tremolo * ((pi2 * i / self.repeat_time as f64).sin())
            } else {
                1.0
            };
            let envelope = if i < self.attack {
                i / self.attack
            } else if i < self.attack + self.decay {
                1.0 - ((i - self.attack) / self.decay) * (1.0 - self.sustain_volume)
            } else if i < self.attack + self.decay + self.sustain {
                self.sustain_volume
            } else if i < self.length - self.delay {
                ((self.length - i - self.delay) / self.release) * self.sustain_volume
            } else {
                0.0
            };
            s = tremolo_gain * sign * s.abs().powf(self.shape_curve) * self.volume * envelope;

            s = if self.delay != 0.0 {
                let tap = if self.delay > i {
                    0.0
                } else {
                    let fade = if i < self.length - self.delay {
                        1.0
                    } else {
                        (self.length - i) / self.delay
                    };
                    let read = (i - self.delay).trunc();
                    let held = if ring.is_empty() {
                        0.0
                    } else {
                        f64::from(ring[(read as u64 % ring.len() as u64) as usize])
                    };
                    fade * held
                };
                s / 2.0 + tap / 2.0
            } else {
                s
            };
            self.s = s;
        }

        // `f = (frequency += slide += deltaSlide) * cos(modulation * tm++)`.
        self.slide += self.delta_slide;
        self.frequency += self.slide;
        let f = self.frequency * (self.modulation * self.tm).cos();
        self.tm += 1.0;
        // The pseudo-noise is `sin` of the sample index pushed through `1e9`
        // and `% 2` - deterministic, not a RNG, and not simplifiable.
        self.t += f - f * self.noise * (1.0 - (((self.i.sin() + 1.0) * 1e9) % 2.0));

        if self.j != 0.0 {
            self.j += 1.0;
            if self.j > self.pitch_jump_time {
                self.frequency += self.pitch_jump;
                self.start_frequency += self.pitch_jump;
                self.j = 0.0;
            }
        }

        if self.repeat_time != 0 {
            self.r += 1.0;
            if (self.r as i64) % self.repeat_time == 0 {
                self.frequency = self.start_frequency;
                self.slide = self.start_slide;
                if self.j == 0.0 {
                    self.j = 1.0;
                }
            }
        }

        // `b[i++] = s` - the write is the loop update, after the body.
        let out = self.s as f32;
        if !ring.is_empty() {
            let write = (self.i as u64 % ring.len() as u64) as usize;
            ring[write] = out;
        }
        self.i += 1.0;
        Some(if out.is_finite() { out } else { 0.0 })
    }
}

#[cfg(test)]
mod tests {
    use super::{ZzfxParams, ZzfxVoice, js_round};

    /// The bit-crush modulus is zero on the default path. ZzFX defines
    /// `c % 0` as NaN, so the recompute branch always runs. A bare integer
    /// modulus panics, and skipping the branch gives silence.
    #[test]
    fn a_default_note_neither_panics_nor_plays_silence() {
        let params = ZzfxParams {
            volume: 0.25,
            randomness: 0.0,
            frequency: 220.0,
            sustain: 0.2,
            ..ZzfxParams::default()
        };
        let mut voice = ZzfxVoice::new(&params, 48_000.0);
        assert!(voice.frames() > 0, "a note has frames");
        let mut power = 0.0f64;
        let mut count = 0u64;
        while let Some(sample) = voice.step(&mut []) {
            assert!(sample.is_finite(), "sample {count} is not finite");
            power += f64::from(sample) * f64::from(sample);
            count += 1;
        }
        assert_eq!(count, voice.frames(), "the loop must run to its length");
        assert!(
            (power / count as f64).sqrt() > 1e-3,
            "the default bit-crush path must still make sound"
        );
    }

    /// Trap 2: `js_round` rounds halves toward +infinity; `f64::round`
    /// rounds them away from zero. The triangle shape feeds negative `t`
    /// through it.
    #[test]
    fn rounding_follows_javascript_on_negative_halves() {
        assert_eq!(js_round(-0.5), 0.0, "Math.round(-0.5) is -0");
        assert_eq!(js_round(-1.5), -1.0);
        assert_eq!(js_round(0.5), 1.0);
        assert_eq!(js_round(2.5), 3.0);
        // Rust's own round disagrees on exactly these, which is the trap.
        assert_eq!((-0.5f64).round(), -1.0);

        // And a triangle whose slide drives `t` negative must stay finite and
        // audible the whole way down.
        let params = ZzfxParams {
            volume: 0.25,
            randomness: 0.0,
            frequency: 110.0,
            sustain: 0.3,
            shape: 1.0,
            slide: -8.0,
            ..ZzfxParams::default()
        };
        let mut voice = ZzfxVoice::new(&params, 48_000.0);
        let mut power = 0.0f64;
        let mut count = 0u64;
        while let Some(sample) = voice.step(&mut []) {
            assert!(sample.is_finite());
            power += f64::from(sample) * f64::from(sample);
            count += 1;
        }
        assert!((power / count as f64).sqrt() > 1e-3);
    }

    /// Trap 7: `attack * sampleRate + 9` - the +9 is a deliberate minimum
    /// that prevents a pop; the buffer's length carries it.
    #[test]
    fn the_nine_frame_minimum_attack_reaches_the_length() {
        let params = ZzfxParams {
            sustain: 0.0,
            release: 0.0,
            attack: 0.0,
            decay: 0.0,
            ..ZzfxParams::default()
        };
        let voice = ZzfxVoice::new(&params, 48_000.0);
        assert_eq!(
            voice.frames(),
            9,
            "length = (0·rate + 9)|0 with all else zero"
        );
    }

    /// The raw-array accessors are the wire encoding; they must invert.
    #[test]
    fn params_round_trip_through_their_raw_indices() {
        let mut params = ZzfxParams::default();
        for index in 0..20 {
            params.set_raw(index, index as f64 * 0.37 - 1.0);
        }
        let mut back = ZzfxParams::default();
        for index in 0..20 {
            back.set_raw(index, params.raw(index));
        }
        back.random_draw = params.random_draw;
        assert_eq!(params, back);
    }

    /// The zdelay tap is `b[(i - delay) | 0]` against the voice's own output
    /// history; a ring shorter than the note still reproduces it as long as
    /// it covers the delay distance, and an EMPTY ring mutes the tap rather
    /// than the voice.
    #[test]
    fn the_delay_tap_reads_the_voices_own_history() {
        let params = ZzfxParams {
            volume: 0.25,
            randomness: 0.0,
            frequency: 220.0,
            sustain: 0.1,
            delay: 0.05,
            ..ZzfxParams::default()
        };
        let sample_rate = 48_000.0;
        let mut with_ring = ZzfxVoice::new(&params, sample_rate);
        let mut no_ring = ZzfxVoice::new(&params, sample_rate);
        let mut ring = vec![0.0f32; 48_000 * 2];
        let delay_frames = (0.05 * sample_rate) as u64;
        let mut differed = false;
        let mut frame = 0u64;
        while let (Some(a), Some(b)) = (with_ring.step(&mut ring), no_ring.step(&mut [])) {
            if frame < delay_frames {
                assert_eq!(a, b, "before the delay distance the tap reads zeros");
            } else if a != b {
                differed = true;
            }
            assert!(a.is_finite() && b.is_finite());
            frame += 1;
        }
        assert!(differed, "past the delay distance the tap must be audible");
    }
}
