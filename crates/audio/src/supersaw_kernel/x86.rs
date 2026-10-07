//! x86 kernels for static supersaw lane stacks.
//!
//! Eight independent lane oscillators are evaluated together. Stereo sums
//! remain scalar and in lane order, preserving the existing floating-point
//! accumulation order. FMA is deliberately not used.

#[cfg(target_arch = "x86")]
use std::arch::x86::{
    _CMP_GE_OQ, _CMP_GT_OQ, _CMP_LT_OQ, _mm256_add_ps, _mm256_blendv_ps, _mm256_cmp_ps,
    _mm256_div_ps, _mm256_loadu_ps, _mm256_mul_ps, _mm256_set1_ps, _mm256_setzero_ps,
    _mm256_storeu_ps, _mm256_sub_ps,
};
#[cfg(target_arch = "x86_64")]
use std::arch::x86_64::{
    _CMP_GE_OQ, _CMP_GT_OQ, _CMP_LT_OQ, _mm256_add_ps, _mm256_blendv_ps, _mm256_cmp_ps,
    _mm256_div_ps, _mm256_loadu_ps, _mm256_mul_ps, _mm256_set1_ps, _mm256_setzero_ps,
    _mm256_storeu_ps, _mm256_sub_ps,
};

use super::StaticSupersawKernel;
use crate::scalar::saw_blep;

const LANES_PER_VECTOR: usize = 8;

pub(super) const AVX2: StaticSupersawKernel = StaticSupersawKernel {
    render: avx2_render_static_lanes,
    minimum_lanes: LANES_PER_VECTOR,
};

/// # Safety
///
/// The caller must establish AVX2 support, valid slice bounds, and ascending
/// finite, nondecreasing detune ratios prepared by `PreparedSupersaw`.
#[target_feature(enable = "avx2")]
unsafe fn avx2_render_static_lanes(
    phases: &mut [f32],
    detune_ratios: &[f32],
    count: usize,
    frequency_hz: f32,
    sample_rate: f32,
    (mut gain_l, mut gain_r): (f32, f32),
) -> (f32, f32) {
    let maximum_frequency = frequency_hz * detune_ratios[count - 1];
    let minimum_frequency = frequency_hz * detune_ratios[0];
    if !(sample_rate.is_finite()
        && sample_rate > 0.0
        && minimum_frequency.is_finite()
        && minimum_frequency > 0.0
        && maximum_frequency.is_finite()
        && maximum_frequency < sample_rate)
    {
        return scalar_render_static_lanes(
            phases,
            detune_ratios,
            count,
            frequency_hz,
            sample_rate,
            (gain_l, gain_r),
        );
    }

    let mut left = 0.0f32;
    let mut right = 0.0f32;
    let vectorized = count - count % LANES_PER_VECTOR;
    let mut lane = 0;

    // SAFETY: this function's target feature supplies every intrinsic below.
    // Each vector starts below `vectorized`, so all eight loaded/stored lanes
    // are within both prepared slices.
    unsafe {
        let zero = _mm256_setzero_ps();
        let half = _mm256_set1_ps(0.5);
        let one = _mm256_set1_ps(1.0);
        let two = _mm256_set1_ps(2.0);
        let frequency = _mm256_set1_ps(frequency_hz);
        let rate = _mm256_set1_ps(sample_rate);

        while lane < vectorized {
            let phase = _mm256_loadu_ps(phases.as_ptr().add(lane));
            let ratios = _mm256_loadu_ps(detune_ratios.as_ptr().add(lane));
            let voice_frequency = _mm256_mul_ps(frequency, ratios);
            let quotient = _mm256_div_ps(voice_frequency, rate);

            // The validated quotient is in (0, 1], where rem_euclid(1) is
            // either the quotient or zero at the rounded upper endpoint.
            let wrapped = _mm256_cmp_ps::<_CMP_GE_OQ>(quotient, one);
            let dt = _mm256_blendv_ps(quotient, zero, wrapped);
            let blep_dt = _mm256_blendv_ps(
                dt,
                _mm256_sub_ps(one, dt),
                _mm256_cmp_ps::<_CMP_GT_OQ>(dt, half),
            );
            // The scalar returns before dividing when the step is zero.
            // Substitute one in those lanes so masked-off calculations do
            // not introduce a floating-point divide-by-zero exception.
            let positive_dt = _mm256_cmp_ps::<_CMP_GT_OQ>(blep_dt, zero);
            let inverse = _mm256_div_ps(one, _mm256_blendv_ps(one, blep_dt, positive_dt));

            let value = _mm256_sub_ps(_mm256_mul_ps(two, phase), one);
            let low_position = _mm256_mul_ps(phase, inverse);
            let low_blep = _mm256_sub_ps(
                _mm256_sub_ps(
                    _mm256_mul_ps(two, low_position),
                    _mm256_mul_ps(low_position, low_position),
                ),
                one,
            );
            let high_position = _mm256_mul_ps(_mm256_sub_ps(phase, one), inverse);
            let high_blep = _mm256_add_ps(
                _mm256_add_ps(
                    _mm256_mul_ps(high_position, high_position),
                    _mm256_mul_ps(two, high_position),
                ),
                one,
            );
            let low_mask = _mm256_cmp_ps::<_CMP_LT_OQ>(phase, blep_dt);
            let high_mask = _mm256_cmp_ps::<_CMP_GT_OQ>(phase, _mm256_sub_ps(one, blep_dt));
            let blep = _mm256_blendv_ps(
                _mm256_blendv_ps(zero, high_blep, high_mask),
                low_blep,
                low_mask,
            );
            let samples = _mm256_sub_ps(value, blep);

            let advanced = _mm256_add_ps(phase, dt);
            let next = _mm256_blendv_ps(
                advanced,
                _mm256_sub_ps(advanced, one),
                _mm256_cmp_ps::<_CMP_GE_OQ>(advanced, one),
            );
            _mm256_storeu_ps(phases.as_mut_ptr().add(lane), next);

            let mut values = [0.0f32; LANES_PER_VECTOR];
            _mm256_storeu_ps(values.as_mut_ptr(), samples);
            for value in values {
                left += value * gain_l;
                right += value * gain_r;
                std::mem::swap(&mut gain_l, &mut gain_r);
            }
            lane += LANES_PER_VECTOR;
        }
    }

    for (phase, ratio) in phases[lane..count]
        .iter_mut()
        .zip(&detune_ratios[lane..count])
    {
        let dt = (frequency_hz * ratio / sample_rate).rem_euclid(1.0);
        let value = saw_blep(*phase, dt);
        left += value * gain_l;
        right += value * gain_r;
        let mut next = *phase + dt;
        if next >= 1.0 {
            next -= 1.0;
        }
        *phase = next;
        std::mem::swap(&mut gain_l, &mut gain_r);
    }
    (left, right)
}

fn scalar_render_static_lanes(
    phases: &mut [f32],
    detune_ratios: &[f32],
    count: usize,
    frequency_hz: f32,
    sample_rate: f32,
    (mut gain_l, mut gain_r): (f32, f32),
) -> (f32, f32) {
    let mut left = 0.0f32;
    let mut right = 0.0f32;
    for (phase, ratio) in phases[..count].iter_mut().zip(&detune_ratios[..count]) {
        let dt = (frequency_hz * ratio / sample_rate).rem_euclid(1.0);
        let value = saw_blep(*phase, dt);
        left += value * gain_l;
        right += value * gain_r;
        let mut next = *phase + dt;
        if next >= 1.0 {
            next -= 1.0;
        }
        *phase = next;
        std::mem::swap(&mut gain_l, &mut gain_r);
    }
    (left, right)
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEST_LANES: usize = 32;

    fn ratios() -> [f32; TEST_LANES] {
        std::array::from_fn(|lane| 2.0f32.powf(((lane as f32 * 0.17) - 2.4) / 12.0))
    }

    fn phases() -> [f32; TEST_LANES] {
        std::array::from_fn(|lane| {
            ((lane as f32 * 0.137) + if lane % 3 == 0 { 0.999_91 } else { 0.000_03 })
                .rem_euclid(1.0)
        })
    }

    #[test]
    fn avx2_matches_scalar_bit_for_bit_over_stateful_renders() {
        if !std::arch::is_x86_feature_detected!("avx2") {
            return;
        }
        let ratios = ratios();
        for count in [8, 9, 15, 16, 31, 32] {
            for (frequency_hz, sample_rate, pan_gains) in [
                (55.0, 44_100.0, (0.2, 0.9)),
                (440.0, 48_000.0, (0.7, 0.3)),
                (18_000.0, 96_000.0, (1.0, 0.0)),
            ] {
                let mut expected_phases = phases();
                let mut actual_phases = expected_phases;
                for frame in 0..4096 {
                    let expected = scalar_render_static_lanes(
                        &mut expected_phases,
                        &ratios,
                        count,
                        frequency_hz,
                        sample_rate,
                        pan_gains,
                    );
                    // SAFETY: the test checked AVX2 and provides valid arrays.
                    let actual = unsafe {
                        avx2_render_static_lanes(
                            &mut actual_phases,
                            &ratios,
                            count,
                            frequency_hz,
                            sample_rate,
                            pan_gains,
                        )
                    };
                    assert_eq!(
                        actual.0.to_bits(),
                        expected.0.to_bits(),
                        "left {count}:{frame}"
                    );
                    assert_eq!(
                        actual.1.to_bits(),
                        expected.1.to_bits(),
                        "right {count}:{frame}"
                    );
                    for (lane, (actual, expected)) in actual_phases
                        .iter()
                        .zip(&expected_phases)
                        .take(count)
                        .enumerate()
                    {
                        assert_eq!(
                            actual.to_bits(),
                            expected.to_bits(),
                            "phase {count}:{frame}:{lane}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn avx2_falls_back_exactly_for_non_vectorizable_frequencies() {
        if !std::arch::is_x86_feature_detected!("avx2") {
            return;
        }
        let ratios = ratios();
        for frequency_hz in [0.0, -220.0, 100_000.0, f32::INFINITY, f32::NAN] {
            let mut expected_phases = phases();
            let mut actual_phases = expected_phases;
            let expected = scalar_render_static_lanes(
                &mut expected_phases,
                &ratios,
                TEST_LANES,
                frequency_hz,
                48_000.0,
                (0.4, 0.6),
            );
            // SAFETY: the test checked AVX2 and provides valid arrays.
            let actual = unsafe {
                avx2_render_static_lanes(
                    &mut actual_phases,
                    &ratios,
                    TEST_LANES,
                    frequency_hz,
                    48_000.0,
                    (0.4, 0.6),
                )
            };
            for (actual, expected) in [actual.0, actual.1]
                .into_iter()
                .zip([expected.0, expected.1])
            {
                if expected.is_nan() {
                    assert!(actual.is_nan());
                } else {
                    assert_eq!(actual.to_bits(), expected.to_bits());
                }
            }
            for (actual, expected) in actual_phases.iter().zip(&expected_phases) {
                if expected.is_nan() {
                    assert!(actual.is_nan());
                } else {
                    assert_eq!(actual.to_bits(), expected.to_bits());
                }
            }
        }
    }
}
