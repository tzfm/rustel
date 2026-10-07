/*
limiter.rs - a brickwall peak limiter for an output bus
Copyright (C) 2026 Rustel contributors

This program is free software: you can redistribute it and/or modify it under
the terms of the GNU Affero General Public License as published by the Free Software
Foundation, either version 3 of the License, or (at your option) any later version.
*/

//! A lookahead brickwall peak limiter, for a whole bus rather than a voice.
//!
//! The per-voice compressor is a `DynamicsCompressorNode` equivalent and has
//! no lookahead, so it cannot promise a ceiling: by the time it has seen a
//! transient the transient is already out. This one delays the signal by its
//! lookahead and spends that runway bringing the gain down, so the sample
//! that needed the reduction is the first one to receive it.
//!
//! # The ceiling, and why it holds
//!
//! Per frame, `required` is the gain that would put this frame exactly on the
//! ceiling - 1 when the frame is already under it. Three stages turn that into
//! the gain actually applied:
//!
//! 1. a sliding MINIMUM of `required` over `[n - L, n]` - the runway, plus
//!    the frame at the far end of it - so a peak is seen `lookahead` frames
//!    before it arrives;
//! 2. a RELEASE, which may only raise the gain, and only gradually;
//! 3. a boxcar MEAN over the lookahead, which rounds the corners the first two
//!    stages leave and is what keeps the result free of the zipper the naive
//!    design produces.
//!
//! The mean is the step that makes the ceiling provable rather than merely
//! likely. Writing `r` for the required gains, `m` for the sliding minimum and
//! `h` for the released signal, the output gain is
//!
//! ```text
//! g[n] = (1 / La) * sum over j in 0..La of h[n - j]
//! ```
//!
//! Release never lifts `h` above `m`, so `h[n - j] <= m[n - j]`, and `m[n - j]`
//! is the least `r` over `[n - j - L, n - j]`. With `La <= L + 1` the index
//! `n - L` lies inside every one of those windows, so every term of the mean is
//! at most `r[n - L]`, and therefore so is their average. `r[n - L]` is by
//! construction the gain that puts frame `n - L` on the ceiling, and frame
//! `n - L` is exactly the frame this output carries. The ceiling holds for
//! every sample, with no overshoot to trim.
//!
//! The proof needs both bounds. The term `j = 0` needs the minimum's window
//! to reach back to `n - L`. With the window one frame shorter, `(n - L, n]`,
//! `r[n - L]` does not bound the newest term of the mean, and one spike in
//! silence leaves the cascade at hundreds of times the ceiling for the clamp
//! to catch. The mean runs at `La = L`, one term inside its bound. It is then
//! the same length as the runway, so the ring it reads and the ring it
//! delays through are the same size.
//!
//! In `f32` rather than in the reals, the mean's running sum rounds, and the
//! result lands within about six parts in a million of the ceiling on either
//! side. That residue, and nothing larger, is what the clamp behind the
//! cascade is there for.
//!
//! The obvious alternative - hold the sliding minimum and ramp linearly toward
//! it over the remaining runway - arrives at the right gain at the right
//! instant and still overshoots enormously, because the ramp is above the
//! target for the whole of its descent. A clamp behind it would then be doing
//! the limiting, audibly.
//!
//! # What it refuses to do
//!
//! Nothing here allocates, locks, or panics: it is built to run inside the
//! device callback under [`crate::tripwire`]. A non-finite input frame is
//! answered with silence for that frame, the same answer
//! [`crate::distortion`] gives and for the same reason - one NaN reaching a
//! feedback path poisons it for the rest of the session.

use crate::meter::db_to_linear;

/// The ceiling on the lookahead, in frames, as a power of two so the rings
/// mask instead of dividing. 1024 frames is 21 ms at 48 kHz, which is more
/// than any character asks for; at 192 kHz it is 5.3 ms, which is less than
/// Warm's 8, so at the top rates the runway is the ring rather than the
/// character. [`Character::lookahead_frames`] is where the two meet, and
/// what anything reporting the delay has to ask.
const RING_FRAMES: usize = 1024;

/// The ring an in-line limiter needs, far smaller than the master's.
///
/// Its runway is one frame, so the minimum's window is two and the delay
/// is two; four is the smallest power of two that holds them and still
/// lets the deque tell full from empty. That is about a hundred bytes a
/// voice against the master ring's twenty-five kilobytes, so the engine
/// can lease one to every voice that asks.
const INLINE_RING_FRAMES: usize = 4;

/// A limiter sized for one voice. See [`Brickwall::in_line`].
pub type InlineLimiter = Brickwall<INLINE_RING_FRAMES>;

/// Stereo, interleaved, as the whole output stage is.
const CHANNELS: usize = 2;

/// Rates up to `u16::MAX` convert exactly; higher rates are scaled relative
/// to that bound so the conversion does not saturate there.
fn rate_hz(sample_rate: u32) -> f32 {
    f32::from(u16::try_from(sample_rate.min(u32::from(u16::MAX))).unwrap_or(1)).max(1.0)
        * if sample_rate > u32::from(u16::MAX) {
            sample_rate as f32 / f32::from(u16::MAX)
        } else {
            1.0
        }
}

/// The ceiling a limiter is built with unless something says otherwise.
pub const DEFAULT_THRESHOLD_DB: f32 = -3.0;

/// How the limiter sounds while it works.
///
/// One processor, four sets of numbers. The safety property is a property of
/// the cascade, so proving it once proves it for every character; a character
/// that needed its own code path would need its own proof.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum Character {
    /// Long lookahead, slow release: it takes the peaks off and leaves the
    /// programme where it was.
    #[default]
    Transparent,
    /// Short lookahead and a fast release, so the level comes back between
    /// hits and the transients stay pointed.
    Punchy,
    /// A slow release over a long window: the reduction rides the phrase
    /// rather than the hit.
    Warm,
    /// The shortest runway and the fastest recovery. It is doing something and
    /// means to be heard doing it.
    Hard,
}

impl Character {
    pub const ALL: [Self; 4] = [Self::Transparent, Self::Punchy, Self::Warm, Self::Hard];

    /// The name a flag, a setting and a score all use.
    pub fn key(self) -> &'static str {
        match self {
            Self::Transparent => "transparent",
            Self::Punchy => "punchy",
            Self::Warm => "warm",
            Self::Hard => "hard",
        }
    }

    /// The name a narrow strip has room for. Written out rather than
    /// truncated: `punchy` and a future `punchier` would truncate to the
    /// same five letters, and a mode you cannot tell from its neighbour is
    /// worse than one with an awkward abbreviation.
    pub fn short(self) -> &'static str {
        match self {
            Self::Transparent => "clean",
            Self::Punchy => "punch",
            Self::Warm => "warm",
            Self::Hard => "hard",
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|c| c.key() == text)
    }

    /// Lookahead in milliseconds. This is also the length of the mean, so it
    /// is the time the limiter takes to reach a gain.
    ///
    /// This is the requested value. The value in use at a given rate is
    /// [`Self::lookahead_frames`], which the ring can shorten. Use that one
    /// for any value shown to a user.
    pub fn lookahead_millis(self) -> f32 {
        match self {
            Self::Transparent => 5.0,
            Self::Punchy => 2.0,
            Self::Warm => 8.0,
            Self::Hard => 1.0,
        }
    }

    /// The runway actually taken at a rate: the character's milliseconds in
    /// frames, floored at one and capped by the ring.
    ///
    /// The cap is not hypothetical - Warm at 192 kHz wants 1536 frames and
    /// gets 1022 - so this, and not the millisecond figure, is the delay the
    /// signal really carries.
    pub fn lookahead_frames(self, sample_rate: u32) -> usize {
        let frames = (self.lookahead_millis() * rate_hz(sample_rate) / 1000.0).round();
        // At least one frame of runway, and never more than the ring has
        // room for: the minimum's window is one frame longer than the
        // runway, and a deque as long as the ring could not tell full from
        // empty.
        (frames.max(1.0) as usize).min(RING_FRAMES - 2)
    }

    /// The same runway as a duration, for a readout. Zero rate, zero delay:
    /// there is no device to be late for.
    pub fn lookahead_millis_at(self, sample_rate: u32) -> f32 {
        if sample_rate == 0 {
            return 0.0;
        }
        let frames = u16::try_from(self.lookahead_frames(sample_rate)).unwrap_or(u16::MAX);
        f32::from(frames) * 1000.0 / rate_hz(sample_rate)
    }

    /// How long the gain takes to come most of the way back, in milliseconds.
    fn release_millis(self) -> f32 {
        match self {
            Self::Transparent => 200.0,
            Self::Punchy => 60.0,
            Self::Warm => 400.0,
            Self::Hard => 30.0,
        }
    }
}

/// A brickwall peak limiter over an interleaved stereo bus.
///
/// The path of one frame, with a runway of `L` frames. The mean is the gain
/// that multiplies the delayed frame:
///
/// ```text
/// in --+--> required --> min over --> release --> mean --+
///      |    gain         [n-L, n]     (slow rise) over L |
///      |                                                 v
///      +--> delay ring, L frames ----------------------> x --> clamp --> out
/// ```
///
/// Both channels take the same gain: a limiter that moved them independently
/// would pull the image toward whichever side was quieter on every peak.
/// `RING` is how many frames of history the cascade keeps, and must be a
/// power of two so the rings mask instead of dividing. It is a parameter
/// because the master's limiter and a voice's need very different sizes:
/// the master looks milliseconds ahead and a voice looks one frame ahead.
/// A ring sized for the master is twenty-five kilobytes, which is too much
/// to lease to every voice.
#[derive(Debug)]
pub struct Brickwall<const RING: usize> {
    /// The signal, waiting out its lookahead.
    delay: [[f32; CHANNELS]; RING],
    /// Required gains, for the sliding minimum to look back over.
    required: [f32; RING],
    /// Released gains, for the mean to average.
    released: [f32; RING],
    /// A monotonic deque over `required`, holding the indices of the frames
    /// that could still be the minimum of a future window. Values increase
    /// from front to back, so the front is always the window's minimum.
    monotonic: [usize; RING],
    front: usize,
    back: usize,
    /// Running sum of the last `lookahead` released gains, so the mean costs
    /// an add and a subtract rather than a loop. `f64` because it accumulates:
    /// an `f32` sum drifts over a set.
    released_sum: f64,
    write: usize,
    /// Frames written since the last reset, so the first `lookahead` frames
    /// know they are still filling the runway.
    filled: usize,
    lookahead: usize,
    release_coefficient: f32,
    threshold: f32,
    threshold_db: f32,
    character: Character,
    sample_rate: u32,
    /// The most gain reduction applied since it was last taken, as a linear
    /// factor in `(0, 1]`. 1 means the limiter did nothing.
    reduction: f32,
}

/// The limiter on a whole bus: milliseconds of lookahead, and the ring to
/// hold them. `Brickwall` is the same design over a smaller history, and
/// [`InlineLimiter`] is the size a single voice needs.
pub type Limiter = Brickwall<RING_FRAMES>;

impl<const RING: usize> Brickwall<RING> {
    /// The ring masks instead of dividing, so it has to be a power of two,
    /// and the deque has to be able to hold `lookahead + 1` entries and
    /// still tell full from empty.
    const MASK: usize = RING - 1;
    const _RING_IS_A_POWER_OF_TWO: () = assert!(RING.is_power_of_two() && RING >= 4);

    /// A limiter for a single voice, with one frame of runway.
    ///
    /// The safety limiter looks ahead because it must never let a peak
    /// through un-smoothed, and its few milliseconds cost nothing when
    /// everything on the output is delayed by the same amount. In line on
    /// one voice they would not: a limited kick would sit up to eight
    /// milliseconds behind an unlimited hat, and the offset would change
    /// with the character - one millisecond on `hard`, eight on `warm` -
    /// so switching modes would move the voice in time. A creative insert
    /// must not do that.
    ///
    /// So the runway is one frame - 21 microseconds at 48 kHz, the shortest
    /// the cascade can be built with - and the same one frame whatever the
    /// character. What is left is a very fast infinite-ratio compressor
    /// with a hard ceiling behind it, which is the sound reached for on a
    /// drum bus, and it stays in time.
    ///
    /// It is still the gain that limits, not the clamp. One frame is enough
    /// for that: the sliding minimum spans `[n - 1, n]`, so the gain a
    /// frame leaves under is already the lesser of what it needed and what
    /// the frame after it needs, and the release only ever falls
    /// immediately and rises slowly. A lone spike is turned down, not
    /// chopped - which is the property `in_line_mechanism_tests` measures
    /// rather than assumes.
    pub fn in_line(sample_rate: u32, threshold_db: f32, character: Character) -> Self {
        let mut limiter = Self::new(sample_rate, threshold_db, character);
        limiter.lookahead = 1;
        limiter.reset();
        limiter
    }

    pub fn new(sample_rate: u32, threshold_db: f32, character: Character) -> Self {
        let mut limiter = Self {
            delay: [[0.0; CHANNELS]; RING],
            required: [1.0; RING],
            released: [1.0; RING],
            monotonic: [0; RING],
            front: 0,
            back: 0,
            released_sum: 0.0,
            write: 0,
            filled: 0,
            lookahead: 1,
            release_coefficient: 0.0,
            threshold: 1.0,
            threshold_db: DEFAULT_THRESHOLD_DB,
            character,
            sample_rate: sample_rate.max(1),
            reduction: 1.0,
        };
        limiter.set_threshold_db(threshold_db);
        limiter.configure(character);
        limiter.reset();
        limiter
    }

    /// Point an existing in-line limiter at a new ceiling and character, in
    /// place. Its runway stays one frame, as [`Self::in_line`] sets it.
    ///
    /// A voice leases its limiter from a pool. Assigning a fresh
    /// `InlineLimiter::in_line` over it would build one on the stack inside
    /// the audio callback and copy a few hundred bytes for each note. This
    /// method writes the fields and clears the rings in place.
    pub fn reconfigure(&mut self, sample_rate: u32, threshold_db: f32, character: Character) {
        self.sample_rate = sample_rate.max(1);
        self.set_threshold_db(threshold_db);
        self.configure(character);
        self.lookahead = 1;
        self.reset();
    }

    /// The ceiling, in dBFS. A threshold at or above 0 dBFS still clamps at
    /// full scale, so "a very high ceiling" never means "no ceiling".
    pub fn set_threshold_db(&mut self, threshold_db: f32) {
        let threshold_db = if threshold_db.is_finite() {
            threshold_db.clamp(-60.0, 0.0)
        } else {
            DEFAULT_THRESHOLD_DB
        };
        self.threshold_db = threshold_db;
        self.threshold = db_to_linear(threshold_db).clamp(f32::MIN_POSITIVE, 1.0);
    }

    pub fn threshold_db(&self) -> f32 {
        self.threshold_db
    }

    pub fn character(&self) -> Character {
        self.character
    }

    /// Change character. The runway length changes with it, so the rings are
    /// cleared: a mean taken partly over the old window would not be a mean of
    /// anything. A caller changing character mid-play should expect the
    /// lookahead's worth of signal to be re-filled.
    pub fn set_character(&mut self, character: Character) {
        if character == self.character {
            return;
        }
        self.configure(character);
        self.reset();
    }

    /// The runway in frames a given character asks for at a given rate.
    ///
    /// `configure` reads this same arithmetic through
    /// [`Character::lookahead_frames`], so a caller that has to keep another
    /// path in step with the limiter - the MIDI schedule, which is timed from
    /// a device clock the limiter sits behind - reads the latency here rather
    /// than owning its own copy of it. One copy also means one cap: the ring
    /// holds `RING_FRAMES - 2` at most, which is what the sliding minimum's
    /// proof needs, and a second arithmetic would be a second place to get
    /// that wrong.
    pub fn latency_frames_at(character: Character, sample_rate: u32) -> usize {
        character.lookahead_frames(sample_rate)
    }

    /// Coefficients, computed once here rather than per sample.
    fn configure(&mut self, character: Character) {
        self.character = character;
        let rate = rate_hz(self.sample_rate);
        // `lookahead_frames` caps by `RING_FRAMES`, because that is the ring
        // the readouts and the latency contract are about. This instance may
        // be a smaller one - `InlineLimiter` is four frames - and a runway
        // longer than its own ring would make `write + RING - lookahead - 1`
        // wrap. `in_line` and `reconfigure` both put the runway back to one
        // frame afterwards, so nothing reaches that today; capping here means
        // a caller that uses `new` or `set_character` on a small ring gets a
        // short runway rather than an underflow. `RING - 2` is the same rule
        // the master uses, and a no-op at `RING_FRAMES`.
        self.lookahead = character
            .lookahead_frames(self.sample_rate)
            .min(RING.saturating_sub(2).max(1));
        // One time constant per release, so the gain covers about 63% of the
        // distance back in the stated time and the rest asymptotically.
        let release_frames = (character.release_millis() * rate / 1000.0).max(1.0);
        self.release_coefficient = 1.0 - (-1.0 / release_frames).exp();
    }

    /// Zero the state and keep the coefficients.
    pub fn reset(&mut self) {
        self.delay = [[0.0; CHANNELS]; RING];
        self.required = [1.0; RING];
        self.released = [1.0; RING];
        self.front = 0;
        self.back = 0;
        self.released_sum = 0.0;
        self.write = 0;
        self.filled = 0;
        self.reduction = 1.0;
    }

    /// How many frames the signal is held back. A caller that has to keep two
    /// paths in step needs this; one that does not can ignore it.
    pub fn latency_frames(&self) -> usize {
        self.lookahead
    }

    /// The most reduction applied since this was last called, as a linear
    /// factor in `(0, 1]`; 1 means the limiter passed the signal through.
    /// Taking it resets it, so a meter reads a peak-hold per interval.
    pub fn take_reduction(&mut self) -> f32 {
        let reduction = self.reduction;
        self.reduction = 1.0;
        reduction
    }

    /// Limit one interleaved stereo block in place.
    ///
    /// The block may be any length; nothing here depends on it. Frames come
    /// out `latency_frames()` behind the ones going in, which is the whole
    /// mechanism and not an implementation detail - a caller keeping another
    /// path in step has to account for it.
    pub fn process_stereo(&mut self, block: &mut [f32]) {
        let frames = block.len() / CHANNELS;
        for frame in 0..frames {
            let (left, right) =
                self.process_frame(block[frame * CHANNELS], block[frame * CHANNELS + 1]);
            block[frame * CHANNELS] = left;
            block[frame * CHANNELS + 1] = right;
        }
    }

    /// One stereo frame. The block form is this in a loop, so the ceiling
    /// is guaranteed by one piece of code however the caller feeds it.
    pub fn process_frame(&mut self, left: f32, right: f32) -> (f32, f32) {
        let (left, right) = if left.is_finite() && right.is_finite() {
            (left, right)
        } else {
            // One NaN in a feedback path is the rest of the session. The
            // frame is dropped rather than propagated.
            (0.0, 0.0)
        };

        let peak = left.abs().max(right.abs());
        let required = if peak > self.threshold {
            (self.threshold / peak).clamp(0.0, 1.0)
        } else {
            1.0
        };

        let (out_left, out_right) = self.step(left, right, required);
        // The clamp behind the cascade. It does not do the limiting: the
        // cascade alone lands within a few ULPs of the ceiling, as the tests
        // on `step` show. The clamp removes that residue, which comes from
        // the rounding of the mean's running sum, so the ceiling holds
        // exactly.
        (
            out_left.clamp(-self.threshold, self.threshold),
            out_right.clamp(-self.threshold, self.threshold),
        )
    }

    /// One frame through the cascade: minimum, release, mean.
    fn step(&mut self, left: f32, right: f32, required: f32) -> (f32, f32) {
        let write = self.write;

        // The frame that will come out once its runway is spent.
        self.delay[write] = [left, right];
        self.required[write] = required;

        // Sliding minimum: drop everything at the back that this frame is at
        // least as small as - none of them can be a future window's minimum -
        // then drop the front if it has aged out of the window.
        while self.back != self.front {
            let last = (self.back + Self::MASK) & Self::MASK;
            if self.required[self.monotonic[last]] >= required {
                self.back = last;
            } else {
                break;
            }
        }
        self.monotonic[self.back] = write;
        self.back = (self.back + 1) & Self::MASK;
        // The window is `[n - L, n]`: L + 1 frames, one more than the
        // runway. The ceiling holds because of that extra frame. The output
        // leaving now is frame `n - L`, and the mean below has a term for
        // `n`. A window that had already dropped `n - L` leaves that term
        // unbounded by `r[n - L]`. An isolated spike then escapes the
        // cascade by the release coefficient over the runway, and the clamp
        // does the limiting, audibly, on the transients this design must
        // catch.
        let oldest = (write + RING - self.lookahead - 1) & Self::MASK;
        if self.monotonic[self.front] == oldest && self.filled > self.lookahead {
            self.front = (self.front + 1) & Self::MASK;
        }
        let minimum = self.required[self.monotonic[self.front]];

        // Release: down is immediate, up is gradual. `minimum` is already the
        // least required gain in the window, so falling to it is what buys the
        // runway; rising from it is the only part with a time constant.
        let read = (write + RING - self.lookahead) & Self::MASK;
        let previous = if self.filled == 0 {
            1.0
        } else {
            self.released[(write + Self::MASK) & Self::MASK]
        };
        let released = if minimum < previous {
            minimum
        } else {
            previous + (minimum - previous) * self.release_coefficient
        };

        // Boxcar mean over the runway, kept as a running sum.
        self.released_sum += f64::from(released);
        let leaving = self.released[read];
        if self.filled >= self.lookahead {
            self.released_sum -= f64::from(leaving);
        }
        self.released[write] = released;

        let span = if self.filled >= self.lookahead {
            self.lookahead
        } else {
            self.filled + 1
        };
        let gain = (self.released_sum / span as f64) as f32;
        let gain = if gain.is_finite() {
            gain.clamp(0.0, 1.0)
        } else {
            0.0
        };
        if gain < self.reduction {
            self.reduction = gain;
        }

        // The frame leaving the delay is the one this gain was computed for.
        let (out_left, out_right) = if self.filled >= self.lookahead {
            (self.delay[read][0], self.delay[read][1])
        } else {
            // Still filling the runway: nothing has come far enough to leave.
            (0.0, 0.0)
        };

        self.write = (write + 1) & Self::MASK;
        self.filled = self.filled.saturating_add(1);
        (out_left * gain, out_right * gain)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn limiter(character: Character) -> Limiter {
        limiter_at(48_000, character)
    }

    fn limiter_at(sample_rate: u32, character: Character) -> Limiter {
        Limiter::new(sample_rate, DEFAULT_THRESHOLD_DB, character)
    }

    /// For every character and every pathological input, no sample leaves
    /// above the ceiling. The tests below check the cascade without the clamp.
    #[test]
    fn no_input_can_push_a_sample_past_the_ceiling() {
        let hostile: Vec<f32> = vec![
            0.0,
            1.0,
            -1.0,
            4.0,
            -4.0,
            16.796, // the loudest peak in the committed corpus goldens
            1e9,
            -1e9,
            f32::MAX,
            f32::MIN,
            f32::MIN_POSITIVE,
            -f32::MIN_POSITIVE,
            f32::NAN,
            f32::INFINITY,
            f32::NEG_INFINITY,
        ];
        for character in Character::ALL {
            let mut limiter = limiter(character);
            let ceiling = limiter.threshold;
            // Every hostile value against every other, so a transient into a
            // NaN into a DC step is covered as well as each alone.
            let mut block = Vec::new();
            for left in &hostile {
                for right in &hostile {
                    block.push(*left);
                    block.push(*right);
                }
            }
            // Long enough to spend the runway several times over.
            for _ in 0..8 {
                let mut pass = block.clone();
                limiter.process_stereo(&mut pass);
                for (index, sample) in pass.iter().enumerate() {
                    assert!(
                        sample.is_finite(),
                        "{}: sample {index} is {sample}",
                        character.key()
                    );
                    assert!(
                        sample.abs() <= ceiling,
                        "{}: sample {index} is {sample}, ceiling {ceiling}",
                        character.key()
                    );
                }
            }
        }
    }

    /// The cascade alone holds the ceiling. This test reads [`Limiter::step`],
    /// before the clamp, because the clamp would hide a wrong cascade.
    #[test]
    fn the_cascade_holds_the_ceiling_without_the_clamp_behind_it() {
        for character in Character::ALL {
            let mut limiter = limiter(character);
            let ceiling = limiter.threshold;
            let runway = limiter.latency_frames();
            let mut worst = 0.0f32;
            let mut settled = 0.0f32;
            // Silence, then a DC step far above the ceiling: the hardest case
            // for a lookahead design, because the gain has one runway to fall
            // the whole distance.
            let frames = runway * 12;
            for frame in 0..frames {
                let sample = if frame < runway * 4 { 0.0 } else { 1e9 };
                // Exactly what `process_frame` hands the cascade, so this is
                // the same signal it sees - only without the clamp behind it.
                let required = if sample > ceiling {
                    (ceiling / sample).clamp(0.0, 1.0)
                } else {
                    1.0
                };
                let (left, right) = limiter.step(sample, sample, required);
                let peak = left.abs().max(right.abs());
                worst = worst.max(peak);
                if frame + 1 == frames {
                    settled = peak;
                }
            }
            // Not `<= ceiling`: the running sum of the boxcar mean rounds.
            // The measured worst case is 6.3e-6 of the ceiling (0.00005 dB).
            // The bound is thirteen orders of magnitude under the result of
            // a cascade that does nothing.
            let over = (worst - ceiling) / ceiling;
            assert!(
                over < 1e-4,
                "{}: the cascade let {worst} through a {ceiling} ceiling, over by {over:e}",
                character.key()
            );
            // And it is limiting rather than muting: a cascade that answered
            // everything with silence would pass the line above.
            assert!(
                settled > ceiling * 0.99,
                "{}: settled at {settled}, nowhere near the {ceiling} it should ride",
                character.key()
            );
        }
    }

    /// One spike in silence stays under the ceiling before the clamp. This
    /// catches a sliding-minimum window of `(n - L, n]` instead of
    /// `[n - L, n]`, which lets the spike through the cascade.
    #[test]
    fn one_spike_in_silence_is_held_by_the_cascade_too() {
        for character in Character::ALL {
            let mut limiter = limiter(character);
            let ceiling = limiter.threshold;
            let runway = limiter.latency_frames();
            let spike_at = runway * 4;
            let mut worst = 0.0f32;
            for frame in 0..runway * 12 {
                let sample: f32 = if frame == spike_at { 1e9 } else { 0.0 };
                let required = if sample > ceiling {
                    (ceiling / sample).clamp(0.0, 1.0)
                } else {
                    1.0
                };
                let (left, right) = limiter.step(sample, sample, required);
                worst = worst.max(left.abs()).max(right.abs());
            }
            // The same float residue the DC step leaves, and nothing more:
            // a billion is nine orders of magnitude of headroom for an
            // off-by-one to show up in.
            let over = (worst - ceiling) / ceiling;
            assert!(
                over < 1e-4,
                "{}: a spike escaped the cascade at {worst} against a {ceiling} ceiling, over by {over:e}",
                character.key()
            );
            // And it was turned down to the ceiling rather than muted: the
            // spike is still there, as loud as it is allowed to be.
            assert!(
                worst > ceiling * 0.99,
                "{}: the spike came out at {worst}, not the {ceiling} it should ride",
                character.key()
            );
        }
    }

    /// The runway table a caller reads off the characters: one to eight
    /// milliseconds, rounded to frames at the rate asked. `latency_frames`
    /// must agree with it, since `configure` is this arithmetic and the
    /// caller keeping another path in step cannot own a limiter.
    #[test]
    fn the_runway_table_is_what_latency_frames_reports() {
        for (character, millis) in [
            (Character::Hard, 1.0),
            (Character::Punchy, 2.0),
            (Character::Transparent, 5.0),
            (Character::Warm, 8.0),
        ] {
            for rate in [44_100, 48_000, 96_000] {
                let frames = Limiter::latency_frames_at(character, rate);
                assert_eq!(frames, (millis * f64::from(rate) / 1000.0).round() as usize);
                let limiter = Limiter::new(rate, -1.0, character);
                assert_eq!(
                    limiter.latency_frames(),
                    frames,
                    "{} at {rate}",
                    character.key()
                );
            }
        }
    }

    /// A signal already under the ceiling comes back unchanged, delayed by the
    /// runway. A limiter that touched quiet programme would be a compressor.
    #[test]
    fn a_signal_under_the_ceiling_is_returned_untouched_after_its_runway() {
        let mut limiter = limiter(Character::Transparent);
        let runway = limiter.latency_frames();
        let frames = runway * 6;
        let source: Vec<f32> = (0..frames)
            .map(|frame| {
                let phase = frame as f32 / 64.0 * std::f32::consts::TAU;
                phase.sin() * 0.25
            })
            .collect();
        let mut block: Vec<f32> = source.iter().flat_map(|s| [*s, *s]).collect();
        limiter.process_stereo(&mut block);
        for frame in runway..frames {
            let got = block[frame * CHANNELS];
            let want = source[frame - runway];
            assert!(
                (got - want).abs() < 1e-6,
                "frame {frame}: {got} is not the {want} written {runway} frames ago"
            );
        }
        assert_eq!(limiter.take_reduction(), 1.0, "quiet programme was reduced");
    }

    /// The reduction a meter reads is a peak hold that empties when taken.
    #[test]
    fn the_reduction_is_a_peak_hold_that_empties_when_read() {
        let mut limiter = limiter(Character::Transparent);
        let runway = limiter.latency_frames();
        let mut loud = vec![1.0f32; runway * 8 * CHANNELS];
        limiter.process_stereo(&mut loud);
        let reduction = limiter.take_reduction();
        assert!(
            reduction < 1.0,
            "a full-scale signal against a -3 dBFS ceiling was not reduced"
        );
        assert!(
            reduction > 0.0,
            "reduction collapsed to silence: {reduction}"
        );
        assert_eq!(limiter.take_reduction(), 1.0, "the hold did not empty");
    }

    /// The characters differ in runway, and every one of them is at least a
    /// frame and inside the ring the proof needs.
    #[test]
    fn every_character_has_a_runway_inside_the_ring() {
        for rate in [8_000, 44_100, 48_000, 96_000, 192_000] {
            for character in Character::ALL {
                let limiter = Limiter::new(rate, DEFAULT_THRESHOLD_DB, character);
                let runway = limiter.latency_frames();
                assert!(runway >= 1, "{} at {rate} has no runway", character.key());
                assert!(
                    runway < RING_FRAMES,
                    "{} at {rate} wants {runway} frames, ring is {RING_FRAMES}",
                    character.key()
                );
            }
        }
        assert!(
            Character::Hard.lookahead_millis() < Character::Transparent.lookahead_millis(),
            "hard is supposed to be the shortest runway"
        );
    }

    /// A character's name survives the preferences and a flag; an unknown one
    /// is refused rather than quietly becoming the default, because a typo in
    /// a limiter setting should not be silence about the ceiling.
    #[test]
    fn a_character_keeps_its_name_and_an_unknown_one_is_refused() {
        for character in Character::ALL {
            assert_eq!(Character::parse(character.key()), Some(character));
        }
        assert_eq!(Character::parse("brickwall"), None);
        assert_eq!(Character::default(), Character::Transparent);
    }

    /// The runway a character gets is not always the one it asks for, and a
    /// readout is owed the one it gets.
    ///
    /// The ring is the ceiling, and it is not a formality: Warm's 8 ms is
    /// 1536 frames at 192 kHz and the ring holds 1023, so at the top rates
    /// the character is cut short. A report built on `lookahead_millis`
    /// would have told a player 8 ms of delay they were not paying, and the
    /// limiter itself would have disagreed by the same 2.7.
    #[test]
    fn the_runway_a_character_gets_is_what_the_rate_and_the_ring_allow() {
        for character in Character::ALL {
            for rate in [8_000, 44_100, 48_000, 96_000, 192_000] {
                let frames = character.lookahead_frames(rate);
                // The one thing a readout must not do is disagree with the
                // limiter it is describing.
                assert_eq!(
                    frames,
                    limiter_at(rate, character).latency_frames(),
                    "{} at {rate}: the readout and the limiter agree",
                    character.key()
                );
                // Never longer than asked for, give or take the frame the
                // rounding is allowed.
                let millis = character.lookahead_millis_at(rate);
                let frame = 1000.0 / rate as f32;
                assert!(
                    millis <= character.lookahead_millis() + frame,
                    "{} at {rate}: {millis} ms for a {} ms character",
                    character.key(),
                    character.lookahead_millis()
                );
            }
        }
        // The cap, named: Warm wants 1536 frames at 192 kHz and gets the
        // ring, which is 5.3 ms of the 8 it asked for.
        assert_eq!(Character::Warm.lookahead_frames(192_000), RING_FRAMES - 2);
        let cut = Character::Warm.lookahead_millis_at(192_000);
        assert!(
            (cut - 5.323).abs() < 0.01,
            "the runway it really has: {cut} ms"
        );
        // And at the rates a device usually opens at, it is not cut at all.
        assert!((Character::Warm.lookahead_millis_at(48_000) - 8.0).abs() < 0.02);
        // No rate at all is no delay: there is no device to be late for.
        assert_eq!(Character::Warm.lookahead_millis_at(0), 0.0);
    }

    /// A threshold is a ceiling, so it is never above full scale however it is
    /// asked for, and a nonsense one falls back rather than opening the gate.
    #[test]
    fn a_threshold_is_never_above_full_scale() {
        let mut limiter = limiter(Character::Transparent);
        for db in [0.0, 12.0, f32::INFINITY, f32::NAN, -1000.0, -0.0] {
            limiter.set_threshold_db(db);
            assert!(
                limiter.threshold > 0.0 && limiter.threshold <= 1.0,
                "{db} dB gave a {} ceiling",
                limiter.threshold
            );
        }
        limiter.set_threshold_db(-6.0);
        assert!((limiter.threshold_db() + 6.0).abs() < 1e-6);
    }

    /// Reset returns it to a fresh limiter's behaviour, coefficients kept.
    #[test]
    fn reset_clears_the_runway_and_keeps_the_character() {
        let mut limiter = limiter(Character::Punchy);
        let runway = limiter.latency_frames();
        let mut loud = vec![4.0f32; runway * 4 * CHANNELS];
        limiter.process_stereo(&mut loud);
        limiter.reset();
        assert_eq!(limiter.character(), Character::Punchy);
        assert_eq!(limiter.latency_frames(), runway);
        assert_eq!(limiter.take_reduction(), 1.0);

        let mut fresh = Limiter::new(48_000, DEFAULT_THRESHOLD_DB, Character::Punchy);
        let mut a = vec![0.5f32; runway * 4 * CHANNELS];
        let mut b = a.clone();
        limiter.process_stereo(&mut a);
        fresh.process_stereo(&mut b);
        assert_eq!(a, b, "a reset limiter did not behave like a fresh one");
    }

    /// Any block length, including one frame at a time, gives the same samples
    /// as one long block: nothing in the cascade may depend on the host's
    /// buffer size.
    #[test]
    fn the_result_does_not_depend_on_how_the_block_is_cut() {
        let frames = 3000;
        let source: Vec<f32> = (0..frames)
            .flat_map(|frame| {
                let phase = frame as f32 / 30.0 * std::f32::consts::TAU;
                let value = phase.sin() * if frame % 700 < 40 { 6.0 } else { 0.4 };
                [value, value * 0.5]
            })
            .collect();

        let mut whole = source.clone();
        limiter(Character::Transparent).process_stereo(&mut whole);

        for chunk_frames in [1usize, 7, 128, 333] {
            let mut piecewise = source.clone();
            let mut limiter = limiter(Character::Transparent);
            for chunk in piecewise.chunks_mut(chunk_frames * CHANNELS) {
                limiter.process_stereo(chunk);
            }
            assert_eq!(
                piecewise, whole,
                "cutting the block into {chunk_frames}-frame pieces changed the result"
            );
        }
    }
}

#[cfg(test)]
mod in_line_tests {
    use super::*;

    /// The in-line limiter that scores reach through `.limit()` holds its
    /// ceiling for any input, including NaN and infinity.
    #[test]
    fn nothing_above_the_ceiling_ever_leaves_it() {
        for character in Character::ALL {
            for threshold_db in [-24.0, -12.0, -6.0, -1.0, 0.0] {
                let mut limiter = InlineLimiter::in_line(48_000, threshold_db, character);
                let ceiling = db_to_linear(threshold_db);
                for frame in 0..4_800 {
                    let wild = match frame % 7 {
                        0 => f32::NAN,
                        1 => f32::INFINITY,
                        2 => f32::NEG_INFINITY,
                        3 => 40.0,
                        4 => -40.0,
                        5 => (frame as f32 * 0.01).sin() * 12.0,
                        _ => 0.5,
                    };
                    let (left, right) = limiter.process_frame(wild, -wild);
                    assert!(
                        left.is_finite() && right.is_finite(),
                        "{character:?} at {threshold_db}: a non-finite sample left the limiter"
                    );
                    assert!(
                        left.abs() <= ceiling + 1e-6 && right.abs() <= ceiling + 1e-6,
                        "{character:?} at {threshold_db}: {left} / {right} passed a ceiling of {ceiling}"
                    );
                }
            }
        }
    }

    /// The in-line limiter does not move its voice in time, for any
    /// character. The bus limiter looks one to eight milliseconds ahead,
    /// which would delay one voice against the others.
    #[test]
    fn a_signal_under_the_ceiling_arrives_when_it_was_played() {
        for character in Character::ALL {
            let mut limiter = InlineLimiter::in_line(48_000, -6.0, character);
            // Silence, then a lone spike well under the ceiling.
            let mut heard_at = None;
            for frame in 0..64 {
                let sample = if frame == 8 { 0.25 } else { 0.0 };
                let (left, _) = limiter.process_frame(sample, sample);
                if left.abs() > 1e-6 && heard_at.is_none() {
                    heard_at = Some(frame);
                }
            }
            let heard_at = heard_at.expect("the spike came out");
            assert!(
                heard_at <= 9,
                "{character:?}: the spike played at 8 and arrived at {heard_at} - \
                 an in-line limiter must not delay its voice"
            );
        }
        // And the bus form does delay, which is the difference being drawn.
        let mut bus = Limiter::new(48_000, -6.0, Character::Warm);
        assert!(
            bus.latency_frames() > 1,
            "the bus limiter looks ahead; that is the whole point of it"
        );
        let mut heard_at = None;
        for frame in 0..2048 {
            let sample = if frame == 8 { 0.25 } else { 0.0 };
            let (left, _) = bus.process_frame(sample, sample);
            if left.abs() > 1e-6 && heard_at.is_none() {
                heard_at = Some(frame);
            }
        }
        assert!(
            heard_at.expect("it came out eventually") > 9,
            "the bus limiter delays by its runway"
        );
    }
}

#[cfg(test)]
mod in_line_mechanism_tests {
    use super::*;

    /// The clamp behind the cascade never has anything to do.
    ///
    /// With no runway the gain for a frame is reckoned from the frame
    /// after it, which sounds like it should let a lone spike through to
    /// be hard-clipped. It does not, and the reason is the release's
    /// asymmetry: the gain falls to what is required immediately and
    /// rises only at the release rate, so by the time a frame is written
    /// out the gain cannot have recovered above what that frame needed.
    /// The limiting is done by gain, and the clamp stays a backstop.
    #[test]
    fn a_lone_spike_is_turned_down_rather_than_clipped() {
        for character in Character::ALL {
            let mut limiter = InlineLimiter::in_line(48_000, -6.0, character);
            let ceiling = db_to_linear(-6.0);
            let mut loudest: f32 = 0.0;
            for frame in 0..64 {
                // Silence with one full-scale sample in it: the case where
                // the next frame asks for no reduction at all.
                let input = if frame == 4 { 1.0 } else { 0.0 };
                let (out, _) = limiter.process_frame(input, input);
                loudest = loudest.max(out.abs());
            }
            assert!(
                (loudest - ceiling).abs() < 1e-4,
                "{character:?}: the spike came out at {loudest}, not the ceiling"
            );
            // The gain did the limiting. A reduction of 1 would mean the
            // cascade passed the spike and the clamp cut it: the same peak
            // value, a different sound.
            let reduction = limiter.take_reduction();
            assert!(
                reduction < 0.99,
                "{character:?}: the clamp did the work, not the limiter ({reduction})"
            );
        }
    }
}
