//! aarch64 kernels for static supersaw lane stacks.
//!
//! Four independent lane oscillators are evaluated together. Stereo sums
//! remain scalar and in lane order, preserving the existing floating-point
//! accumulation order. FMA is deliberately not used.

use std::arch::aarch64::{
    vaddq_f32, vbslq_f32, vcgeq_f32, vcgtq_f32, vcltq_f32, vdivq_f32, vdupq_n_f32, vld1q_f32,
    vmulq_f32, vst1q_f32, vsubq_f32,
};

use super::{StaticSupersawKernel, SupersawKernelKind};
use crate::scalar::{saw_blep, saw_step};

const LANES_PER_VECTOR: usize = 4;

pub(super) const NEON: StaticSupersawKernel = StaticSupersawKernel {
    kind: SupersawKernelKind::Neon,
    render: neon_render_static_lanes,
    minimum_lanes: LANES_PER_VECTOR,
};

/// # Safety
///
/// The caller must establish NEON support, valid slice bounds, and ascending
/// finite, nondecreasing detune ratios prepared by `PreparedSupersaw`.
#[target_feature(enable = "neon")]
unsafe fn neon_render_static_lanes(
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
    // Each vector starts below `vectorized`, so all four loaded/stored lanes
    // are within both prepared slices.
    unsafe {
        let zero = vdupq_n_f32(0.0);
        let half = vdupq_n_f32(0.5);
        let one = vdupq_n_f32(1.0);
        let two = vdupq_n_f32(2.0);
        let frequency = vdupq_n_f32(frequency_hz);
        let rate = vdupq_n_f32(sample_rate);

        while lane < vectorized {
            let phase = vld1q_f32(phases.as_ptr().add(lane));
            let ratios = vld1q_f32(detune_ratios.as_ptr().add(lane));
            let voice_frequency = vmulq_f32(frequency, ratios);
            let quotient = vdivq_f32(voice_frequency, rate);

            // The validated quotient is in (0, 1], where rem_euclid(1) is
            // either the quotient or zero at the rounded upper endpoint.
            let wrapped = vcgeq_f32(quotient, one);
            let dt = vbslq_f32(wrapped, zero, quotient);
            let blep_dt = vbslq_f32(vcgtq_f32(dt, half), vsubq_f32(one, dt), dt);
            // The scalar returns before dividing when the step is zero.
            // Substitute one in those lanes so masked-off calculations do
            // not introduce a floating-point divide-by-zero exception.
            let positive_dt = vcgtq_f32(blep_dt, zero);
            let inverse = vdivq_f32(one, vbslq_f32(positive_dt, blep_dt, one));

            let value = vsubq_f32(vmulq_f32(two, phase), one);
            let low_position = vmulq_f32(phase, inverse);
            let low_blep = vsubq_f32(
                vsubq_f32(
                    vmulq_f32(two, low_position),
                    vmulq_f32(low_position, low_position),
                ),
                one,
            );
            let high_position = vmulq_f32(vsubq_f32(phase, one), inverse);
            let high_blep = vaddq_f32(
                vaddq_f32(
                    vmulq_f32(high_position, high_position),
                    vmulq_f32(two, high_position),
                ),
                one,
            );
            let low_mask = vcltq_f32(phase, blep_dt);
            let high_mask = vcgtq_f32(phase, vsubq_f32(one, blep_dt));
            let blep = vbslq_f32(low_mask, low_blep, vbslq_f32(high_mask, high_blep, zero));
            let samples = vsubq_f32(value, blep);

            let advanced = vaddq_f32(phase, dt);
            let next = vbslq_f32(vcgeq_f32(advanced, one), vsubq_f32(advanced, one), advanced);
            vst1q_f32(phases.as_mut_ptr().add(lane), next);

            let mut values = [0.0f32; LANES_PER_VECTOR];
            vst1q_f32(values.as_mut_ptr(), samples);
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
        let dt = saw_step(frequency_hz * ratio / sample_rate);
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
        let dt = saw_step(frequency_hz * ratio / sample_rate);
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
    fn neon_matches_scalar_bit_for_bit_over_stateful_renders() {
        if !std::arch::is_aarch64_feature_detected!("neon") {
            return;
        }
        let ratios = ratios();
        // Miri interprets each frame, so the count is small there.
        let frames = if cfg!(miri) { 8 } else { 4096 };
        for count in [4, 5, 7, 8, 15, 16, 31, 32] {
            for (frequency_hz, sample_rate, pan_gains) in [
                (55.0, 44_100.0, (0.2, 0.9)),
                (440.0, 48_000.0, (0.7, 0.3)),
                (18_000.0, 96_000.0, (1.0, 0.0)),
            ] {
                let mut expected_phases = phases();
                let mut actual_phases = expected_phases;
                for frame in 0..frames {
                    let expected = scalar_render_static_lanes(
                        &mut expected_phases,
                        &ratios,
                        count,
                        frequency_hz,
                        sample_rate,
                        pan_gains,
                    );
                    // SAFETY: the test checked NEON and provides valid arrays.
                    let actual = unsafe {
                        neon_render_static_lanes(
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
    fn neon_falls_back_exactly_for_non_vectorizable_frequencies() {
        if !std::arch::is_aarch64_feature_detected!("neon") {
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
            // SAFETY: the test checked NEON and provides valid arrays.
            let actual = unsafe {
                neon_render_static_lanes(
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
