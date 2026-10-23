//! Band-limited oscillator tables following Firefox's WebAudio
//! `PeriodicWave` construction and range selection.

use std::sync::{Arc, OnceLock};

use crate::Waveform;

const OCTAVE_BANDS: f32 = 3.0;
const CENTS_PER_RANGE: f32 = 1200.0 / OCTAVE_BANDS;

/// Firefox builds every builtin waveform on a 4096-sample table, at any
/// sample rate.
const BUILTIN_TABLE_SIZE: usize = 4096;

/// A custom wave doubles its table when its coefficients, DC included,
/// outgrow the builtin size.
const MAX_TABLE_SIZE: usize = 8192;

/// The pitch ranges of the largest table.
const MAX_RANGES: usize = 38;

#[derive(Clone, Copy, Default)]
struct Complex {
    re: f64,
    im: f64,
}

impl Complex {
    fn add(self, other: Self) -> Self {
        Self {
            re: self.re + other.re,
            im: self.im + other.im,
        }
    }

    fn sub(self, other: Self) -> Self {
        Self {
            re: self.re - other.re,
            im: self.im - other.im,
        }
    }

    fn mul(self, other: Self) -> Self {
        Self {
            re: self.re * other.re - self.im * other.im,
            im: self.re * other.im + self.im * other.re,
        }
    }
}

struct ShapeTables {
    samples: Box<[f32]>,
    table_size: usize,
}

impl ShapeTables {
    fn new(waveform: Waveform, table_size: usize, range_count: usize) -> Self {
        let mut samples = vec![0.0; range_count * table_size].into_boxed_slice();
        let mut normalization = 1.0f32;
        for range in 0..range_count {
            let partials = number_of_partials(table_size, range);
            let table = inverse_table(waveform, table_size, partials);
            if range == 0 {
                let peak = table.iter().copied().map(f32::abs).fold(0.0, f32::max);
                if peak > 0.0 {
                    normalization = peak.recip();
                }
            }
            let destination = &mut samples[range * table_size..(range + 1) * table_size];
            for (destination, sample) in destination.iter_mut().zip(table) {
                *destination = sample * normalization;
            }
        }
        Self {
            samples,
            table_size,
        }
    }

    fn range(&self, range: usize) -> &[f32] {
        &self.samples[range * self.table_size..(range + 1) * self.table_size]
    }
}

pub(crate) struct PeriodicWaveTables {
    sample_rate: f32,
    table_size: usize,
    range_count: usize,
    square: ShapeTables,
    sawtooth: ShapeTables,
    triangle: ShapeTables,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct TableSelection {
    higher_range: usize,
    lower_range: usize,
    range_fraction: f32,
}

/// The rate whose tables every backend shares, built once for the process;
/// a backend at any other rate builds its own.
pub(crate) const SHARED_TABLES_RATE: u32 = 48_000;

pub(crate) fn prepared_tables(sample_rate: u32) -> Arc<PeriodicWaveTables> {
    static TABLES_48K: OnceLock<Arc<PeriodicWaveTables>> = OnceLock::new();
    if sample_rate == SHARED_TABLES_RATE {
        TABLES_48K
            .get_or_init(|| Arc::new(PeriodicWaveTables::new(sample_rate)))
            .clone()
    } else {
        Arc::new(PeriodicWaveTables::new(sample_rate))
    }
}

impl PeriodicWaveTables {
    pub(crate) fn new(sample_rate: u32) -> Self {
        let table_size = BUILTIN_TABLE_SIZE;
        let range_count = range_count(table_size);
        Self {
            sample_rate: sample_rate as f32,
            table_size,
            range_count,
            square: ShapeTables::new(Waveform::Square, table_size, range_count),
            sawtooth: ShapeTables::new(Waveform::Sawtooth, table_size, range_count),
            triangle: ShapeTables::new(Waveform::Triangle, table_size, range_count),
        }
    }

    /// The three shapes' tables, every band of each.
    pub(crate) fn bytes(&self) -> usize {
        [&self.square, &self.sawtooth, &self.triangle]
            .into_iter()
            .map(|shape| std::mem::size_of_val(&*shape.samples))
            .sum()
    }

    pub(crate) fn selection(&self, frequency: f32) -> TableSelection {
        select(
            frequency,
            self.sample_rate,
            self.table_size,
            self.range_count,
        )
    }

    /// Sample a builtin oscillator at its nominal frequency.
    ///
    /// Sine is evaluated directly and never consults the band-limited table
    /// ranges, so calculating their logarithmic selection would be dead work.
    #[inline]
    pub(crate) fn sample_for_frequency(
        &self,
        waveform: Waveform,
        phase: f64,
        frequency: f32,
    ) -> f32 {
        if waveform == Waveform::Sine {
            (phase * std::f64::consts::TAU).sin() as f32
        } else {
            self.sample(waveform, phase, self.selection(frequency))
        }
    }

    pub(crate) fn sample(&self, waveform: Waveform, phase: f64, selection: TableSelection) -> f32 {
        if waveform == Waveform::Sine {
            return (phase * std::f64::consts::TAU).sin() as f32;
        }
        let shape = match waveform {
            Waveform::Square => &self.square,
            Waveform::Sawtooth => &self.sawtooth,
            Waveform::Triangle => &self.triangle,
            Waveform::Sine => unreachable!(),
        };
        read(
            shape.range(selection.higher_range),
            shape.range(selection.lower_range),
            phase,
            selection.range_fraction,
        )
    }
}

/// The two pitch ranges that play `frequency` and the crossfade between them.
fn select(
    frequency: f32,
    sample_rate: f32,
    table_size: usize,
    range_count: usize,
) -> TableSelection {
    let lowest_frequency = (sample_rate * 0.5) / (table_size / 2) as f32;
    let frequency = frequency.abs();
    let ratio = if frequency > 0.0 {
        frequency / lowest_frequency
    } else {
        0.5
    };
    let pitch_range =
        (1.0 + ratio.log2() * 1200.0 / CENTS_PER_RANGE).clamp(0.0, (range_count - 1) as f32);
    let higher_range = pitch_range as usize;
    let lower_range = (higher_range + 1).min(range_count - 1);
    let range_fraction = pitch_range - higher_range as f32;
    TableSelection {
        higher_range,
        lower_range,
        range_fraction,
    }
}

/// Interpolate each table at `phase`, then crossfade from `higher` (more
/// partials) toward `lower` by `range_fraction`. Both tables hold one
/// period of the same power-of-two length.
#[inline]
fn read(higher: &[f32], lower: &[f32], phase: f64, range_fraction: f32) -> f32 {
    let table_size = higher.len();
    let virtual_index = phase * table_size as f64;
    let whole = virtual_index.floor();
    // The table size is a power of two, so masking the signed index
    // wraps any phase, a negative one included, into one period.
    let index = whole as i64 as usize & (table_size - 1);
    let next = (index + 1) & (table_size - 1);
    let fraction = (virtual_index - whole) as f32;
    let interpolate = |table: &[f32]| table[index] + fraction * (table[next] - table[index]);
    let higher = interpolate(higher);
    let lower = interpolate(lower);
    higher + range_fraction * (lower - higher)
}

/// Firefox's range count, `3·log2(size)` computed in `f32` and truncated:
/// 36 for 4096 samples, and 38, not 39, for 8192.
fn range_count(table_size: usize) -> usize {
    if table_size > BUILTIN_TABLE_SIZE {
        MAX_RANGES
    } else {
        36
    }
}

fn number_of_partials(table_size: usize, range: usize) -> usize {
    (table_size as f32 * 0.5 * 2.0f32.powf(-(range as f32 * CENTS_PER_RANGE) / 1200.0)) as usize
}

fn coefficient(waveform: Waveform, harmonic: usize) -> f64 {
    let n = harmonic as f64;
    match waveform {
        Waveform::Sine => {
            if harmonic == 1 {
                1.0
            } else {
                0.0
            }
        }
        Waveform::Square => {
            if harmonic & 1 == 1 {
                4.0 / (n * std::f64::consts::PI)
            } else {
                0.0
            }
        }
        Waveform::Sawtooth => {
            let sign = if harmonic & 1 == 1 { 1.0 } else { -1.0 };
            sign * 2.0 / (n * std::f64::consts::PI)
        }
        Waveform::Triangle => {
            if harmonic & 1 == 1 {
                let sign = if ((harmonic - 1) / 2) & 1 == 1 {
                    -1.0
                } else {
                    1.0
                };
                sign * 8.0 / (std::f64::consts::PI * n).powi(2)
            } else {
                0.0
            }
        }
    }
}

fn inverse_table(waveform: Waveform, table_size: usize, partials: usize) -> Vec<f32> {
    let mut spectrum = vec![Complex::default(); table_size];
    for harmonic in 1..=partials.min(table_size / 2 - 1) {
        let coefficient = coefficient(waveform, harmonic);
        let magnitude = coefficient * table_size as f64 * 0.5;
        spectrum[harmonic].im = -magnitude;
        spectrum[table_size - harmonic].im = magnitude;
    }
    inverse_fft(&mut spectrum);
    spectrum
        .into_iter()
        .map(|sample| sample.re as f32)
        .collect()
}

fn inverse_fft(values: &mut [Complex]) {
    let len = values.len();
    let mut reversed = 0usize;
    for index in 1..len {
        let mut bit = len >> 1;
        while reversed & bit != 0 {
            reversed ^= bit;
            bit >>= 1;
        }
        reversed ^= bit;
        if index < reversed {
            values.swap(index, reversed);
        }
    }

    let mut width = 2;
    while width <= len {
        let angle = std::f64::consts::TAU / width as f64;
        let root = Complex {
            re: angle.cos(),
            im: angle.sin(),
        };
        for start in (0..len).step_by(width) {
            let mut factor = Complex { re: 1.0, im: 0.0 };
            for offset in 0..width / 2 {
                let even = values[start + offset];
                let odd = values[start + offset + width / 2].mul(factor);
                values[start + offset] = even.add(odd);
                values[start + offset + width / 2] = even.sub(odd);
                factor = factor.mul(root);
            }
        }
        width *= 2;
    }
    let scale = 1.0 / len as f64;
    for value in values {
        value.re *= scale;
        value.im *= scale;
    }
}

/// A periodic wave built from custom partials, as Firefox builds one for
/// `createPeriodicWave`: one band-limited table per pitch range, read like a
/// builtin waveform, and the peak of each partial count for normalization.
///
/// A note normalizes by the peak of its first `n` partials, where `n` is
/// the most partials below Nyquist at any pitch the note has played so far
/// (see [`CustomWave::partial_limit`]). A falling pitch raises `n` and can
/// change the level mid-note, as in Firefox.
pub(crate) struct CustomWave {
    table_size: usize,
    range_count: usize,
    /// The partials the lowest pitches play.
    partials: usize,
    /// One unnormalized table per distinct partial count, `table_size`
    /// samples each.
    tables: Box<[f32]>,
    /// The table of each pitch range.
    range_tables: [u8; MAX_RANGES],
    /// `norms[n - 1]` is the scale while the note plays at most `n` partials.
    norms: Box<[f32]>,
}

impl CustomWave {
    /// The table for a list of `requested` partials. Firefox counts the DC
    /// term as a coefficient.
    pub(crate) fn table_size(requested: usize) -> usize {
        if requested < BUILTIN_TABLE_SIZE {
            BUILTIN_TABLE_SIZE
        } else {
            MAX_TABLE_SIZE
        }
    }

    /// The partials a list of `requested` plays: the table keeps half its
    /// size in coefficients, DC included, so 2047 or 4095.
    pub(crate) fn played_partials(requested: usize) -> usize {
        requested.min(Self::table_size(requested) / 2 - 1)
    }

    /// `real` and `imag` hold the cosine and sine terms of partials 1, 2, ...
    /// Each table holds `Σ real·cos + imag·sin` over its partials; the
    /// normalization is applied when the note reads it.
    pub(crate) fn new(table_size: usize, real: &[f32], imag: &[f32]) -> Self {
        let partials = real.len().min(imag.len()).min(table_size / 2 - 1);
        let range_count = range_count(table_size);
        let mut counts = [0usize; MAX_RANGES];
        for (range, count) in counts.iter_mut().take(range_count).enumerate() {
            *count = number_of_partials(table_size, range).min(partials);
        }
        // Higher ranges cull more partials, so the counts never rise.
        let mut distinct = counts[..range_count].to_vec();
        distinct.dedup();
        let mut range_tables = [0u8; MAX_RANGES];
        for (table, count) in range_tables.iter_mut().zip(&counts[..range_count]) {
            *table = distinct
                .iter()
                .position(|candidate| candidate == count)
                .expect("every count is among the distinct counts") as u8;
        }

        // Sum the partials in order. After partial `n` the sum is the table
        // of every range that plays `n` partials, and its peak normalizes a
        // note that plays at most `n`.
        let circle = unit_circle();
        let stride = MAX_TABLE_SIZE / table_size;
        let mask = table_size - 1;
        let mut tables = vec![0.0f32; distinct.len() * table_size].into_boxed_slice();
        let mut norms = Vec::with_capacity(partials);
        let mut sum = vec![0.0f64; table_size];
        let mut peak = 0.0f64;
        // A range with no partial keeps its silent table.
        let mut next = distinct.iter().rposition(|&count| count > 0).map(|i| i + 1);
        for partial in 1..=partials {
            let (re, im) = (f64::from(real[partial - 1]), f64::from(imag[partial - 1]));
            if re != 0.0 || im != 0.0 {
                peak = 0.0;
                let mut index = 0usize;
                for value in &mut sum {
                    let (cos, sin) = circle[index * stride];
                    *value += re * cos + im * sin;
                    peak = peak.max(value.abs());
                    index = (index + partial) & mask;
                }
            }
            // A silent table keeps Firefox's initial scale of 1.
            norms.push(if peak > 0.0 { (1.0 / peak) as f32 } else { 1.0 });
            if let Some(slot) = next
                && distinct[slot - 1] == partial
            {
                let table = &mut tables[(slot - 1) * table_size..slot * table_size];
                for (sample, value) in table.iter_mut().zip(&sum) {
                    *sample = *value as f32;
                }
                next = (slot > 1).then_some(slot - 1);
            }
        }
        Self {
            table_size,
            range_count,
            partials,
            tables,
            range_tables,
            norms: norms.into_boxed_slice(),
        }
    }

    pub(crate) fn bytes(&self) -> usize {
        std::mem::size_of::<Self>()
            + std::mem::size_of_val(&*self.tables)
            + std::mem::size_of_val(&*self.norms)
    }

    pub(crate) fn selection(&self, frequency: f32, sample_rate: f32) -> TableSelection {
        select(frequency, sample_rate, self.table_size, self.range_count)
    }

    /// The unnormalized wave at `phase`.
    #[inline]
    pub(crate) fn sample(&self, phase: f64, selection: TableSelection) -> f32 {
        read(
            self.table(selection.higher_range),
            self.table(selection.lower_range),
            phase,
            selection.range_fraction,
        )
    }

    fn table(&self, range: usize) -> &[f32] {
        let index = usize::from(self.range_tables[range]);
        &self.tables[index * self.table_size..(index + 1) * self.table_size]
    }

    /// Partials below Nyquist at `frequency`, up to half the table. A note
    /// keeps the largest value it has met: its normalization never falls
    /// back to fewer partials.
    #[inline]
    pub(crate) fn partial_limit(&self, frequency: f32, sample_rate: f32) -> usize {
        let half = self.table_size / 2;
        let frequency = frequency.abs();
        if frequency == 0.0 {
            half
        } else {
            half.min((sample_rate * 0.5 / frequency) as usize)
        }
    }

    /// The scale for a note whose [`Self::partial_limit`] reached `limit`.
    #[inline]
    pub(crate) fn norm(&self, limit: usize) -> f32 {
        match limit.min(self.partials) {
            0 => 1.0,
            count => self.norms[count - 1],
        }
    }
}

/// `cos` and `sin` of `2π·j/8192`. A 4096-sample table reads every second
/// entry.
fn unit_circle() -> &'static [(f64, f64)] {
    static CIRCLE: OnceLock<Box<[(f64, f64)]>> = OnceLock::new();
    CIRCLE.get_or_init(|| {
        (0..MAX_TABLE_SIZE)
            .map(|step| {
                let angle = std::f64::consts::TAU * step as f64 / MAX_TABLE_SIZE as f64;
                (angle.cos(), angle.sin())
            })
            .collect()
    })
}

#[cfg(test)]
mod phase_wrap_tests {
    use super::*;

    /// A phase outside one period reads the point its wrapped value reads, a
    /// negative phase included: a frequency swung far enough moves the phase more
    /// than one period in a sample.
    #[test]
    fn any_phase_reads_the_point_of_its_wrapped_phase() {
        let tables = PeriodicWaveTables::new(48_000);
        let selection = tables.selection(440.0);
        for waveform in [Waveform::Square, Waveform::Sawtooth, Waveform::Triangle] {
            for step in 0..64 {
                // Binary fractions, so shifting by whole periods is exact.
                let phase = f64::from(step) / 64.0 + 1.0 / 8192.0;
                let wrapped = tables.sample(waveform, phase, selection);
                for periods in [-3.0, -1.0, 1.0, 2.0] {
                    assert_eq!(
                        tables.sample(waveform, phase + periods, selection),
                        wrapped,
                        "{waveform:?} at {phase} + {periods} periods"
                    );
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn webaudio_tables_are_normalized_and_band_limited() {
        let tables = PeriodicWaveTables::new(48_000);
        for waveform in [Waveform::Square, Waveform::Sawtooth, Waveform::Triangle] {
            let shape = match waveform {
                Waveform::Square => &tables.square,
                Waveform::Sawtooth => &tables.sawtooth,
                Waveform::Triangle => &tables.triangle,
                Waveform::Sine => unreachable!(),
            };
            let low_peak = shape
                .range(0)
                .iter()
                .copied()
                .map(f32::abs)
                .fold(0.0, f32::max);
            assert!((low_peak - 1.0).abs() < 1e-5, "{waveform:?}: {low_peak}");
            assert_eq!(tables.sample(waveform, 0.0, tables.selection(440.0)), 0.0);
        }
    }

    #[test]
    fn builtin_tables_keep_firefoxs_size_at_every_sample_rate() {
        for sample_rate in [22_050, 48_000, 96_000] {
            let tables = PeriodicWaveTables::new(sample_rate);
            assert_eq!((tables.table_size, tables.range_count), (4096, 36));
            let selection = tables.selection(440.0);
            assert!(tables.sample(Waveform::Square, 0.25, selection).is_finite());
        }
    }

    /// A custom wave with the sawtooth's own terms plays the builtin
    /// sawtooth: the same tables per pitch range, the same normalization.
    #[test]
    fn a_custom_sawtooth_plays_the_builtin_sawtooth() {
        let tables = PeriodicWaveTables::new(48_000);
        let partials = CustomWave::played_partials(2047);
        let real = vec![0.0; partials];
        let imag: Vec<f32> = (1..=partials)
            .map(|harmonic| coefficient(Waveform::Sawtooth, harmonic) as f32)
            .collect();
        let custom = CustomWave::new(CustomWave::table_size(partials), &real, &imag);
        for frequency in [5.0f32, 55.0, 440.0, 3_520.0, 15_000.0] {
            let selection = custom.selection(frequency, 48_000.0);
            for phase in [0.0, 0.1, 0.37, 0.5, 0.93] {
                let played = custom.sample(phase, selection) * custom.norm(usize::MAX);
                let builtin = tables.sample(Waveform::Sawtooth, phase, selection);
                assert!(
                    (played - builtin).abs() < 1e-5,
                    "{frequency} Hz at phase {phase}: {played} against {builtin}"
                );
            }
        }
    }

    /// Firefox keeps half its table in coefficients, DC included, and
    /// doubles the table once a list outgrows it.
    #[test]
    fn a_long_list_keeps_what_firefoxs_table_holds() {
        for (requested, table_size, played) in [
            (128, 4096, 128),
            (2047, 4096, 2047),
            (3000, 4096, 2047),
            (4095, 4096, 2047),
            (4096, 8192, 4095),
            (100_000, 8192, 4095),
        ] {
            assert_eq!(CustomWave::table_size(requested), table_size, "{requested}");
            assert_eq!(
                CustomWave::played_partials(requested),
                played,
                "{requested}"
            );
        }
    }

    #[test]
    fn table_selection_uses_frequency_magnitude() {
        let tables = PeriodicWaveTables::new(48_000);
        let positive = tables.sample(Waveform::Square, 0.125, tables.selection(440.0));
        let negative = tables.sample(Waveform::Square, 0.125, tables.selection(-440.0));
        assert_eq!(positive, negative);
    }

    #[test]
    fn frequency_sampling_matches_explicit_selection() {
        let tables = PeriodicWaveTables::new(48_000);
        for waveform in [
            Waveform::Sine,
            Waveform::Square,
            Waveform::Sawtooth,
            Waveform::Triangle,
        ] {
            for phase in [0.0, 0.125, 0.333, 0.999] {
                for frequency in [-880.0, 0.0, 55.0, 440.0, 20_000.0] {
                    assert_eq!(
                        tables.sample_for_frequency(waveform, phase, frequency),
                        tables.sample(waveform, phase, tables.selection(frequency))
                    );
                }
            }
        }
    }
}
