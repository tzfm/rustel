//! Master-bus metering: sample peak and ITU-R BS.1770 momentary loudness.
//!
//! The measurement runs inside the audio callback, so it is deliberately
//! built from fixed-size state: two biquads per channel and one sliding
//! window of energy slots. There is no allocation, no lock and no syscall on
//! this path. Results are published through plain atomics that a UI thread
//! samples whenever it draws.

use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU32, AtomicU64, Ordering};

/// Momentary loudness window, as specified by BS.1770.
const WINDOW_SECONDS: f64 = 0.4;
/// Sub-blocks the window is divided into. 10 ms slots keep the meter's
/// response smooth without storing the samples themselves.
const WINDOW_SLOTS: usize = 40;
/// The value reported for digital silence. BS.1770 loudness of true silence is
/// negative infinity; a floor keeps the transported value finite.
pub const SILENCE_LUFS: f32 = -70.0;

/// A published master-bus reading.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MasterLevels {
    /// Highest absolute sample since the previous read, post-fader and
    /// post-limiter. 1.0 is digital full scale.
    pub peak: f32,
    /// Momentary (400 ms) loudness in LUFS, post-fader.
    pub lufs: f32,
    /// Callback blocks that contained a sample at or beyond full scale.
    pub clipped_blocks: u64,
    /// The worst gain reduction the limiter applied over the interval, as a
    /// linear factor in `(0, 1]`. 1 is a limiter that did nothing, and is
    /// also what a path with no limiter reports.
    pub reduction: f32,
}

impl Default for MasterLevels {
    fn default() -> Self {
        Self {
            peak: 0.0,
            lufs: SILENCE_LUFS,
            clipped_blocks: 0,
            // No limiter, or one that did nothing: the same reading.
            reduction: 1.0,
        }
    }
}

/// Lock-free publication slots shared by the callback and the UI.
#[derive(Debug)]
pub struct MasterMeterShared {
    /// Post-fader sample peak, accumulated with `fetch_max` and reset by the
    /// reader. Non-negative floats order identically to their bit patterns,
    /// which is what makes the integer maximum correct here.
    peak_bits: AtomicU32,
    lufs_bits: AtomicU32,
    clipped_blocks: AtomicU64,
    /// Linear master gain applied to the final mix.
    gain_bits: AtomicU32,
    /// The limiter's ceiling in dBFS, or a non-finite value for "off".
    limiter_threshold_bits: AtomicU32,
    /// Index into [`crate::limiter::Character::ALL`].
    limiter_character: AtomicU8,
    /// Whether the limiter's ceiling is brought back up to full scale.
    limiter_makeup: AtomicBool,
    /// The worst gain reduction of the last block, linear.
    reduction_bits: AtomicU32,
}

impl Default for MasterMeterShared {
    fn default() -> Self {
        Self::new()
    }
}

/// What the callback needs to know to run the limiter.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LimiterSettings {
    pub threshold_db: f32,
    pub character: crate::limiter::Character,
}

impl MasterMeterShared {
    pub fn new() -> Self {
        Self {
            peak_bits: AtomicU32::new(0),
            lufs_bits: AtomicU32::new(SILENCE_LUFS.to_bits()),
            clipped_blocks: AtomicU64::new(0),
            gain_bits: AtomicU32::new(1.0f32.to_bits()),
            limiter_threshold_bits: AtomicU32::new(f32::NAN.to_bits()),
            limiter_character: AtomicU8::new(0),
            limiter_makeup: AtomicBool::new(false),
            reduction_bits: AtomicU32::new(1.0f32.to_bits()),
        }
    }

    /// Turn the master limiter on at a ceiling, or off with `None`.
    ///
    /// Off still protects the device: the callback clamps every sample to
    /// full scale. Off means that the output reaches the ceiling by clipping
    /// and not by lookahead gain reduction. That choice changes the sound,
    /// not the safety.
    pub fn set_limiter(&self, settings: Option<LimiterSettings>) {
        match settings {
            // A non-finite ceiling is the off sentinel, so one arriving as
            // a value would read back as off. It is refused here rather
            // than left to mean two things.
            Some(settings) if settings.threshold_db.is_finite() => {
                self.limiter_character
                    .store(settings.character as u8, Ordering::Relaxed);
                self.limiter_threshold_bits
                    .store(settings.threshold_db.to_bits(), Ordering::Release);
            }
            Some(_) => self
                .limiter_threshold_bits
                .store(f32::NAN.to_bits(), Ordering::Release),
            None => self
                .limiter_threshold_bits
                .store(f32::NAN.to_bits(), Ordering::Release),
        }
    }

    /// Bring the limiter's ceiling back up to full scale, or leave it.
    ///
    /// The makeup gain is exactly `1 / threshold`. The limiter guarantees
    /// that nothing leaves above its ceiling, so that gain puts the ceiling
    /// at full scale and cannot take a sample past it. The amount is fixed,
    /// so the control is a switch.
    pub fn set_limiter_makeup(&self, on: bool) {
        self.limiter_makeup.store(on, Ordering::Relaxed);
    }

    /// The gain to apply after limiting, 1 when there is nothing to make
    /// up. Reads the ceiling it is compensating, so the two cannot drift.
    pub fn limiter_makeup_gain(&self) -> f32 {
        if !self.limiter_makeup.load(Ordering::Relaxed) {
            return 1.0;
        }
        match self.limiter_settings() {
            // `threshold` is the linear ceiling the limiter clamps to, and
            // is never zero, so this never divides by nothing.
            Some(settings) => {
                1.0 / db_to_linear(settings.threshold_db).clamp(f32::MIN_POSITIVE, 1.0)
            }
            None => 1.0,
        }
    }

    /// The callback's view of the setting; `None` while the limiter is off.
    /// A non-finite threshold is the off sentinel, which is why the setter
    /// refuses to store one as a value.
    pub fn limiter_settings(&self) -> Option<LimiterSettings> {
        let threshold_db = f32::from_bits(self.limiter_threshold_bits.load(Ordering::Acquire));
        if !threshold_db.is_finite() {
            return None;
        }
        let character = crate::limiter::Character::ALL
            .get(usize::from(self.limiter_character.load(Ordering::Relaxed)))
            .copied()
            .unwrap_or_default();
        Some(LimiterSettings {
            threshold_db,
            character,
        })
    }

    /// Callback-side publication of the block's worst gain reduction.
    pub fn publish_reduction(&self, reduction: f32) {
        let reduction = if reduction.is_finite() {
            reduction.clamp(0.0, 1.0)
        } else {
            1.0
        };
        // A minimum, not a store: a producer turn faster than the frame rate
        // must not hide the block that did the most work. Non-negative floats
        // order as their bits do, so this is a `fetch_min` on the bits.
        self.reduction_bits
            .fetch_min(reduction.to_bits(), Ordering::Relaxed);
    }

    /// The worst gain reduction the limiter last applied, as a linear factor
    /// in `(0, 1]`; 1 means it passed the signal through.
    pub fn reduction(&self) -> f32 {
        f32::from_bits(self.reduction_bits.load(Ordering::Relaxed))
    }

    /// The peak the signal reached BEFORE the limiter, so CLIP keeps meaning
    /// "this score asked for more than full scale" rather than going dark the
    /// moment a ceiling below full scale is in force.
    pub fn observe_unlimited(&self, stereo: &[f32]) {
        let mut peak = 0.0f32;
        let mut wild = false;
        for sample in stereo {
            // `f32::max` returns the non-NaN operand, so a NaN never becomes
            // the peak and has to be looked for on its own - otherwise a
            // score handing the device NaN would be reported as silence.
            if sample.is_finite() {
                peak = peak.max(sample.abs());
            } else {
                wild = true;
            }
        }
        if wild || peak >= 1.0 {
            self.clipped_blocks.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// Set the linear master gain. Values are clamped into a sane range so a
    /// UI bug cannot hand the callback a NaN or an eardrum-threatening factor.
    pub fn set_gain(&self, gain: f32) {
        let clamped = if gain.is_finite() {
            gain.clamp(0.0, 4.0)
        } else {
            1.0
        };
        self.gain_bits.store(clamped.to_bits(), Ordering::Relaxed);
    }

    pub fn gain(&self) -> f32 {
        f32::from_bits(self.gain_bits.load(Ordering::Relaxed))
    }

    /// Read and reset the peak, leaving loudness and the clip counter intact.
    ///
    /// Resetting on read is what makes this a true peak-per-interval meter: a
    /// caller that polls at frame rate sees the loudest sample within each
    /// frame rather than the loudest sample ever played.
    pub fn take(&self) -> MasterLevels {
        MasterLevels {
            peak: f32::from_bits(self.peak_bits.swap(0, Ordering::AcqRel)),
            lufs: f32::from_bits(self.lufs_bits.load(Ordering::Relaxed)),
            clipped_blocks: self.clipped_blocks.load(Ordering::Relaxed),
            // Reset on read, like `peak`: this is the worst reduction WITHIN
            // the interval, not the worst ever.
            reduction: f32::from_bits(self.reduction_bits.swap(1.0f32.to_bits(), Ordering::AcqRel)),
        }
    }

    /// Callback-side publication of one block's measurements.
    fn publish(&self, peak: f32, lufs: f32, clipped: bool) {
        self.peak_bits.fetch_max(peak.to_bits(), Ordering::Relaxed);
        self.lufs_bits.store(lufs.to_bits(), Ordering::Relaxed);
        if clipped {
            self.clipped_blocks.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// Forget history across a device change so the replacement stream does
    /// not inherit the previous device's peak hold.
    pub fn reset_levels(&self) {
        self.peak_bits.store(0, Ordering::Relaxed);
        self.lufs_bits
            .store(SILENCE_LUFS.to_bits(), Ordering::Relaxed);
    }
}

/// A direct-form-II transposed biquad in `f64`.
///
/// Loudness integrates over 400 ms of a 38 Hz high-pass, where `f32` state
/// accumulates audible error; the cost of `f64` here is a few nanoseconds per
/// frame.
#[derive(Clone, Copy, Debug, Default)]
struct Biquad {
    b0: f64,
    b1: f64,
    b2: f64,
    a1: f64,
    a2: f64,
    s1: f64,
    s2: f64,
}

impl Biquad {
    fn process(&mut self, input: f64) -> f64 {
        let output = self.b0 * input + self.s1;
        self.s1 = self.b1 * input - self.a1 * output + self.s2;
        self.s2 = self.b2 * input - self.a2 * output;
        output
    }

    fn reset(&mut self) {
        self.s1 = 0.0;
        self.s2 = 0.0;
    }

    /// BS.1770 stage 1: a +4 dB high shelf standing in for the head's
    /// acoustic response.
    fn k_weight_shelf(sample_rate: f64) -> Self {
        let f0 = 1681.974450955533;
        let gain_db = 3.999_843_853_973_347;
        let q = 0.707_175_236_955_419_6;
        let k = (std::f64::consts::PI * f0 / sample_rate).tan();
        let vh = 10.0f64.powf(gain_db / 20.0);
        let vb = vh.powf(0.499_666_774_154_541_6);
        let a0 = 1.0 + k / q + k * k;
        Self {
            b0: (vh + vb * k / q + k * k) / a0,
            b1: 2.0 * (k * k - vh) / a0,
            b2: (vh - vb * k / q + k * k) / a0,
            a1: 2.0 * (k * k - 1.0) / a0,
            a2: (1.0 - k / q + k * k) / a0,
            s1: 0.0,
            s2: 0.0,
        }
    }

    /// BS.1770 stage 2: an RLB high-pass at roughly 38 Hz.
    fn k_weight_highpass(sample_rate: f64) -> Self {
        let f0 = 38.135_470_876_024_44;
        let q = 0.500_327_037_323_877_3;
        let k = (std::f64::consts::PI * f0 / sample_rate).tan();
        let denominator = 1.0 + k / q + k * k;
        Self {
            b0: 1.0,
            b1: -2.0,
            b2: 1.0,
            a1: 2.0 * (k * k - 1.0) / denominator,
            a2: (1.0 - k / q + k * k) / denominator,
            s1: 0.0,
            s2: 0.0,
        }
    }
}

#[derive(Clone, Copy, Debug, Default)]
struct ChannelWeighting {
    shelf: Biquad,
    highpass: Biquad,
}

impl ChannelWeighting {
    fn new(sample_rate: f64) -> Self {
        Self {
            shelf: Biquad::k_weight_shelf(sample_rate),
            highpass: Biquad::k_weight_highpass(sample_rate),
        }
    }

    fn process(&mut self, sample: f64) -> f64 {
        self.highpass.process(self.shelf.process(sample))
    }

    fn reset(&mut self) {
        self.shelf.reset();
        self.highpass.reset();
    }
}

/// Callback-owned meter state: K-weighting filters plus a sliding energy
/// window. One of these belongs to each live output stream.
#[derive(Debug)]
pub struct MasterMeter {
    left: ChannelWeighting,
    right: ChannelWeighting,
    slots: [f64; WINDOW_SLOTS],
    slot: usize,
    slot_frames: u32,
    slot_capacity: u32,
    filled_slots: usize,
}

impl MasterMeter {
    pub fn new(sample_rate: u32) -> Self {
        let rate = f64::from(sample_rate.max(1));
        let slot_capacity = ((rate * WINDOW_SECONDS / WINDOW_SLOTS as f64).round() as u32).max(1);
        Self {
            left: ChannelWeighting::new(rate),
            right: ChannelWeighting::new(rate),
            slots: [0.0; WINDOW_SLOTS],
            slot: 0,
            slot_frames: 0,
            slot_capacity,
            filled_slots: 0,
        }
    }

    /// Measure one interleaved stereo block and publish the result.
    ///
    /// Callback-only. Every operation here is arithmetic over fixed-size
    /// state; the block length does not change what is allocated.
    pub fn observe_stereo(&mut self, stereo: &[f32], shared: &MasterMeterShared) {
        let frames = stereo.len() / 2;
        if frames == 0 {
            return;
        }
        // One pass for the peak and for finiteness. `f32::max` returns the
        // non-NaN operand, so a NaN has to be detected explicitly rather than
        // inferred from the peak.
        let mut peak = 0.0f32;
        let mut finite = true;
        for &sample in &stereo[..frames * 2] {
            let magnitude = sample.abs();
            if magnitude > peak {
                peak = magnitude;
            }
            finite &= sample.is_finite();
        }
        // A non-finite sample would poison the filter state and the window
        // forever; drop the block and restart rather than latching a NaN.
        if !finite {
            self.reset();
            shared.publish(0.0, SILENCE_LUFS, true);
            return;
        }
        for frame in 0..frames {
            let weighted_left = self.left.process(f64::from(stereo[frame * 2]));
            let weighted_right = self.right.process(f64::from(stereo[frame * 2 + 1]));
            self.slots[self.slot] +=
                weighted_left * weighted_left + weighted_right * weighted_right;
            self.slot_frames += 1;
            if self.slot_frames >= self.slot_capacity {
                self.slot = (self.slot + 1) % WINDOW_SLOTS;
                self.slots[self.slot] = 0.0;
                self.slot_frames = 0;
                self.filled_slots = self.filled_slots.saturating_add(1);
            }
        }
        // Clipping is judged before the limiter, by `observe_unlimited`: a
        // post-limiter buffer cannot reach full scale when a ceiling is in
        // force, so asking this buffer would answer "never".
        shared.publish(peak, self.momentary_lufs(), false);
    }

    fn momentary_lufs(&self) -> f32 {
        // The ring always holds the partly filled current slot plus at most
        // `WINDOW_SLOTS - 1` completed ones; counting a full ring of complete
        // slots would divide the same energy by an extra ten milliseconds.
        let complete = self.filled_slots.min(WINDOW_SLOTS - 1);
        let frames = complete as f64 * f64::from(self.slot_capacity) + f64::from(self.slot_frames);
        if frames <= 0.0 {
            return SILENCE_LUFS;
        }
        // A NaN would have been caught by the finiteness check, but summing
        // an empty window is still a legitimate zero.
        let mean_square = self.slots.iter().sum::<f64>() / frames;
        if mean_square <= 0.0 || !mean_square.is_finite() {
            return SILENCE_LUFS;
        }
        let lufs = -0.691 + 10.0 * mean_square.log10();
        (lufs as f32).max(SILENCE_LUFS)
    }

    fn reset(&mut self) {
        self.left.reset();
        self.right.reset();
        self.slots = [0.0; WINDOW_SLOTS];
        self.slot = 0;
        self.slot_frames = 0;
        self.filled_slots = 0;
    }
}

/// Convert a linear gain to decibels, with a floor for silence.
pub fn linear_to_db(linear: f32) -> f32 {
    if linear <= 1e-6 {
        return -120.0;
    }
    20.0 * linear.log10()
}

/// Convert decibels to a linear gain.
pub fn db_to_linear(db: f32) -> f32 {
    if db <= -119.0 {
        return 0.0;
    }
    10.0f32.powf(db / 20.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn measure(
        sample_rate: u32,
        seconds: f64,
        mut generate: impl FnMut(usize) -> (f32, f32),
    ) -> MasterLevels {
        let shared = MasterMeterShared::new();
        let mut meter = MasterMeter::new(sample_rate);
        let frames = (f64::from(sample_rate) * seconds) as usize;
        let mut block = [0.0f32; 256];
        let mut index = 0;
        while index < frames {
            let count = (frames - index).min(128);
            for frame in 0..count {
                let (left, right) = generate(index + frame);
                block[frame * 2] = left;
                block[frame * 2 + 1] = right;
            }
            meter.observe_stereo(&block[..count * 2], &shared);
            index += count;
        }
        shared.take()
    }

    /// Power gain of a biquad at `frequency`, evaluated in the frequency
    /// domain. Independent of the time-domain recursion under test.
    fn power_gain(filter: &Biquad, frequency: f64, sample_rate: f64) -> f64 {
        let omega = std::f64::consts::TAU * frequency / sample_rate;
        let (numerator_real, numerator_imaginary) = (
            filter.b0 + filter.b1 * omega.cos() + filter.b2 * (2.0 * omega).cos(),
            -(filter.b1 * omega.sin() + filter.b2 * (2.0 * omega).sin()),
        );
        let (denominator_real, denominator_imaginary) = (
            1.0 + filter.a1 * omega.cos() + filter.a2 * (2.0 * omega).cos(),
            -(filter.a1 * omega.sin() + filter.a2 * (2.0 * omega).sin()),
        );
        (numerator_real * numerator_real + numerator_imaginary * numerator_imaginary)
            / (denominator_real * denominator_real + denominator_imaginary * denominator_imaginary)
    }

    #[test]
    fn k_weighting_matches_the_coefficients_published_for_forty_eight_kilohertz() {
        // BS.1770-4, tables 1 and 2. Deriving them from the analog prototype
        // is what lets the meter follow a device that is not running at 48 kHz.
        let shelf = Biquad::k_weight_shelf(48_000.0);
        for (actual, expected) in [
            (shelf.b0, 1.535_124_859_586_97),
            (shelf.b1, -2.691_696_189_406_38),
            (shelf.b2, 1.198_392_810_852_85),
            (shelf.a1, -1.690_659_293_182_41),
            (shelf.a2, 0.732_480_774_215_85),
        ] {
            assert!((actual - expected).abs() < 1e-9, "{actual} != {expected}");
        }

        let highpass = Biquad::k_weight_highpass(48_000.0);
        for (actual, expected) in [
            (highpass.b0, 1.0),
            (highpass.b1, -2.0),
            (highpass.b2, 1.0),
            (highpass.a1, -1.990_047_454_833_98),
            (highpass.a2, 0.990_072_250_366_21),
        ] {
            assert!((actual - expected).abs() < 1e-8, "{actual} != {expected}");
        }
    }

    #[test]
    fn a_thousand_hertz_tone_reads_the_loudness_its_weighted_power_implies() {
        let sample_rate = 48_000.0;
        let root_mean_square = 10.0f64.powf(-20.0 / 20.0);
        let amplitude = (root_mean_square * std::f64::consts::SQRT_2) as f32;
        let levels = measure(48_000, 2.0, |index| {
            let phase = std::f32::consts::TAU * 1_000.0 * index as f32 / 48_000.0;
            let value = amplitude * phase.sin();
            (value, value)
        });

        let weighting = power_gain(&Biquad::k_weight_shelf(sample_rate), 1_000.0, sample_rate)
            * power_gain(
                &Biquad::k_weight_highpass(sample_rate),
                1_000.0,
                sample_rate,
            );
        // Two channels at unit weight, so the summed energy is doubled.
        let expected =
            -0.691 + 10.0 * (2.0 * root_mean_square * root_mean_square * weighting).log10();
        assert!(
            (f64::from(levels.lufs) - expected).abs() < 0.1,
            "expected about {expected:.2} LUFS, measured {}",
            levels.lufs
        );
    }

    #[test]
    fn silence_reads_the_floor_and_no_peak() {
        let levels = measure(48_000, 0.5, |_| (0.0, 0.0));
        assert_eq!(levels.peak, 0.0);
        assert_eq!(levels.lufs, SILENCE_LUFS);
        assert_eq!(levels.clipped_blocks, 0);
    }

    #[test]
    fn full_scale_samples_are_counted_as_clipping_and_peak_resets_on_read() {
        let shared = MasterMeterShared::new();
        let mut meter = MasterMeter::new(48_000);
        let mut block = [0.0f32; 256];
        block[0] = 1.0;
        block[1] = -0.5;
        // Clipping is asked of the signal BEFORE the limiter, so the answer
        // stays "this score asked for more than full scale" whatever ceiling
        // is in force; the peak and the loudness are of what is played.
        shared.observe_unlimited(&block);
        meter.observe_stereo(&block, &shared);

        let first = shared.take();
        assert_eq!(first.peak, 1.0);
        assert_eq!(first.clipped_blocks, 1);

        let second = shared.take();
        assert_eq!(second.peak, 0.0, "peak is per reading interval");
        assert_eq!(second.clipped_blocks, 1, "clip history is cumulative");
    }

    /// The indicator survives a ceiling. A limited buffer cannot reach full
    /// scale, so judging clipping after the limiter would answer "never" and
    /// the footer's CLIP would go dark exactly when it matters most.
    #[test]
    fn clipping_is_judged_before_the_limiter_so_a_ceiling_cannot_hide_it() {
        let shared = MasterMeterShared::new();
        let mut meter = MasterMeter::new(48_000);
        let mut limiter = crate::limiter::Limiter::new(
            48_000,
            crate::limiter::DEFAULT_THRESHOLD_DB,
            crate::limiter::Character::Transparent,
        );

        // A score asking for four times full scale.
        let mut block = [4.0f32; 512];
        shared.observe_unlimited(&block);
        limiter.process_stereo(&mut block);
        meter.observe_stereo(&block, &shared);

        let levels = shared.take();
        assert_eq!(
            levels.clipped_blocks, 1,
            "CLIP went dark behind the ceiling"
        );
        assert!(
            levels.peak <= db_to_linear(crate::limiter::DEFAULT_THRESHOLD_DB),
            "the meter read {} , past the ceiling",
            levels.peak
        );

        // A NaN is a clip too: it is a score handing the device something it
        // cannot play, and it must not be reported as quiet.
        let nan = [f32::NAN; 8];
        shared.observe_unlimited(&nan);
        assert_eq!(shared.take().clipped_blocks, 2);
    }

    /// Makeup gain is `1 / ceiling`: it puts the ceiling on full scale and
    /// cannot pass it.
    #[test]
    fn limiter_makeup_lands_the_ceiling_on_full_scale() {
        let shared = MasterMeterShared::new();
        assert_eq!(shared.limiter_makeup_gain(), 1.0, "off, nothing to make up");

        shared.set_limiter_makeup(true);
        assert_eq!(
            shared.limiter_makeup_gain(),
            1.0,
            "and nothing to make up with no limiter running"
        );

        for threshold_db in [-0.1f32, -1.0, -6.0, -12.0, -24.0, -60.0] {
            shared.set_limiter(Some(LimiterSettings {
                threshold_db,
                character: crate::limiter::Character::Transparent,
            }));
            let makeup = shared.limiter_makeup_gain();
            let landed = db_to_linear(threshold_db) * makeup;
            assert!(
                (landed - 1.0).abs() < 1e-5,
                "{threshold_db} dB: the ceiling landed at {landed}, not full scale"
            );
        }

        // And switching it off leaves the signal alone again.
        shared.set_limiter_makeup(false);
        assert_eq!(shared.limiter_makeup_gain(), 1.0);
    }

    /// The off sentinel round-trips, and a character survives it.
    #[test]
    fn the_limiter_setting_crosses_to_the_callback_and_off_is_off() {
        let shared = MasterMeterShared::new();
        assert_eq!(shared.limiter_settings(), None, "it starts off");

        shared.set_limiter(Some(LimiterSettings {
            threshold_db: -6.0,
            character: crate::limiter::Character::Punchy,
        }));
        let settings = shared.limiter_settings().expect("on");
        assert_eq!(settings.threshold_db, -6.0);
        assert_eq!(settings.character, crate::limiter::Character::Punchy);

        shared.set_limiter(None);
        assert_eq!(shared.limiter_settings(), None);

        // Reduction is a linear factor held at its worst across an interval
        // and emptied by the reading, so a fast producer turn cannot hide the
        // block that did the most work.
        assert_eq!(shared.reduction(), 1.0);
        shared.publish_reduction(0.5);
        shared.publish_reduction(0.9);
        assert_eq!(shared.reduction(), 0.5, "the hold kept the lesser");
        assert_eq!(shared.take().reduction, 0.5);
        assert_eq!(shared.reduction(), 1.0, "the reading emptied the hold");
        // Nonsense is "did nothing" rather than a hold at zero.
        shared.publish_reduction(f32::NAN);
        assert_eq!(shared.reduction(), 1.0);
    }

    /// One 128-frame block of a 1 kHz stereo tone starting at `frame`.
    fn tone_block(frame: usize, amplitude: f32) -> [f32; 256] {
        let mut block = [0.0f32; 256];
        for index in 0..128 {
            let phase = std::f32::consts::TAU * 1_000.0 * (frame + index) as f32 / 48_000.0;
            let value = amplitude * phase.sin();
            block[index * 2] = value;
            block[index * 2 + 1] = value;
        }
        block
    }

    #[test]
    fn a_non_finite_sample_cannot_latch_the_meter() {
        let shared = MasterMeterShared::new();
        let mut meter = MasterMeter::new(48_000);
        meter.observe_stereo(&[f32::NAN; 256], &shared);
        assert!(shared.take().lufs.is_finite());

        for block in 0..200 {
            meter.observe_stereo(&tone_block(block * 128, 0.25), &shared);
        }
        let levels = shared.take();
        assert!(
            levels.lufs.is_finite() && levels.lufs > SILENCE_LUFS,
            "the meter stayed at the floor after the poisoned block: {}",
            levels.lufs
        );
        assert!(
            (levels.peak - 0.25).abs() < 0.01,
            "peak was {}",
            levels.peak
        );
    }

    #[test]
    fn gain_is_clamped_into_a_survivable_range() {
        let shared = MasterMeterShared::new();
        shared.set_gain(f32::NAN);
        assert_eq!(shared.gain(), 1.0);
        shared.set_gain(-3.0);
        assert_eq!(shared.gain(), 0.0);
        shared.set_gain(100.0);
        assert_eq!(shared.gain(), 4.0);
        shared.set_gain(0.5);
        assert_eq!(shared.gain(), 0.5);
    }

    #[test]
    fn decibel_conversions_round_trip_over_the_fader_range() {
        for db in [-60.0, -18.0, -6.0, 0.0, 6.0] {
            let round_trip = linear_to_db(db_to_linear(db));
            assert!((round_trip - db).abs() < 0.001, "{db} became {round_trip}");
        }
        assert_eq!(db_to_linear(-120.0), 0.0);
        assert_eq!(linear_to_db(0.0), -120.0);
    }

    #[test]
    fn loudness_tracks_a_level_change_within_the_window() {
        let shared = MasterMeterShared::new();
        let mut meter = MasterMeter::new(48_000);
        for block in 0..400 {
            meter.observe_stereo(&tone_block(block * 128, 0.25), &shared);
        }
        let loud = shared.take().lufs;
        for block in 400..800 {
            meter.observe_stereo(&tone_block(block * 128, 0.025), &shared);
        }
        let quiet = shared.take().lufs;
        assert!(
            (loud - quiet - 20.0).abs() < 0.5,
            "a twenty decibel drop measured {}",
            loud - quiet
        );
    }

    #[test]
    fn direct_current_carries_no_loudness() {
        // The 38 Hz stage exists exactly so an offset cannot register as
        // programme level; a meter that reported it would be measuring a
        // silent signal as loud.
        let levels = measure(48_000, 1.0, |_| (0.5, 0.5));
        assert_eq!(levels.peak, 0.5);
        assert_eq!(levels.lufs, SILENCE_LUFS);
    }
}
