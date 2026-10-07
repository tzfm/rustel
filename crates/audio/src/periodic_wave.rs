//! Band-limited builtin oscillator tables following Chromium's WebAudio
//! `PeriodicWave` construction and range selection.

use std::sync::{Arc, OnceLock};

use crate::Waveform;

const OCTAVE_BANDS: f32 = 3.0;
const CENTS_PER_RANGE: f32 = 1200.0 / OCTAVE_BANDS;

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
        let table_size = periodic_wave_size(sample_rate);
        let range_count = (0.5 + OCTAVE_BANDS * (table_size as f32).log2()) as usize;
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
        let lowest_frequency = (self.sample_rate * 0.5) / (self.table_size / 2) as f32;
        let frequency = frequency.abs();
        let ratio = if frequency > 0.0 {
            frequency / lowest_frequency
        } else {
            0.5
        };
        let pitch_range = (1.0 + ratio.log2() * 1200.0 / CENTS_PER_RANGE)
            .clamp(0.0, (self.range_count - 1) as f32);
        let higher_range = pitch_range as usize;
        let lower_range = (higher_range + 1).min(self.range_count - 1);
        let range_fraction = pitch_range - higher_range as f32;
        TableSelection {
            higher_range,
            lower_range,
            range_fraction,
        }
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

        let virtual_index = phase * self.table_size as f64;
        let whole = virtual_index.floor();
        // The table size is a power of two, so masking the signed index
        // wraps any phase, a negative one included, into one period.
        let index = whole as i64 as usize & (self.table_size - 1);
        let next = (index + 1) & (self.table_size - 1);
        let fraction = (virtual_index - whole) as f32;
        let interpolate = |table: &[f32]| table[index] + fraction * (table[next] - table[index]);
        let higher = interpolate(shape.range(selection.higher_range));
        let lower = interpolate(shape.range(selection.lower_range));
        higher + selection.range_fraction * (lower - higher)
    }
}

fn periodic_wave_size(sample_rate: u32) -> usize {
    if sample_rate <= 24_000 {
        2048
    } else if sample_rate <= 88_200 {
        4096
    } else {
        16_384
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
    fn table_size_tracks_chromiums_sample_rate_breakpoints() {
        assert_eq!(periodic_wave_size(24_000), 2048);
        assert_eq!(periodic_wave_size(24_001), 4096);
        assert_eq!(periodic_wave_size(88_200), 4096);
        assert_eq!(periodic_wave_size(88_201), 16_384);
        for (sample_rate, expected_size) in [(24_000, 2048), (96_000, 16_384)] {
            let tables = PeriodicWaveTables::new(sample_rate);
            assert_eq!(tables.table_size, expected_size);
            let selection = tables.selection(440.0);
            assert!(tables.sample(Waveform::Square, 0.25, selection).is_finite());
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
