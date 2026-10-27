//! aarch64 wavetable interpolation kernels.
//!
//! Table reads are gathered for four independent lanes. NEON has no gather
//! instruction, so the four table reads are scalar loads; every arithmetic
//! step around them runs in vectors. Stereo summation, detuning, phase
//! advancement, and phase warping remain scalar and retain their existing
//! order. FMA is deliberately not used.

use std::arch::aarch64::{
    vaddq_f32, vaddq_s32, vbslq_s32, vcgtq_s32, vcvtq_f32_s32, vcvtq_s32_f32, vdupq_n_f32,
    vdupq_n_s32, vgetq_lane_s32, vld1q_f32, vmulq_f32, vst1q_f32, vsubq_f32,
};

const LANES_PER_VECTOR: usize = 4;

/// # Safety
///
/// The caller must establish NEON support, valid slice bounds, two complete
/// frames whose absolute indices fit in `i32`, and finite phases in `0..=1`.
#[allow(clippy::too_many_arguments)]
pub(super) unsafe fn neon_interpolate_lanes(
    pcm: &[f32],
    frame_len: usize,
    frame_a: usize,
    frame_b: usize,
    interp: f32,
    phases: &[f32],
    values: &mut [f32],
    count: usize,
) {
    let vectorized = count - count % LANES_PER_VECTOR;
    let mut lane = 0;

    // SAFETY: this function's target feature supplies every intrinsic below.
    // The caller validates both complete frames, absolute i32 gather indices,
    // phases, and all slice lengths. Each vector starts below `vectorized`.
    unsafe {
        let frame_len_f32 = vdupq_n_f32(frame_len as f32);
        let last_index = vdupq_n_s32(frame_len as i32 - 1);
        let one_index = vdupq_n_s32(1);
        let frame_a_base = vdupq_n_s32((frame_a * frame_len) as i32);
        let frame_b_base = vdupq_n_s32((frame_b * frame_len) as i32);
        let frame_interp = vdupq_n_f32(interp);

        while lane < vectorized {
            let phase = vld1q_f32(phases.as_ptr().add(lane));
            let position = vmulq_f32(phase, frame_len_f32);
            let raw_index = vcvtq_s32_f32(position);
            let wrapped = vcgtq_s32(raw_index, last_index);
            let index = vbslq_s32(wrapped, vdupq_n_s32(0), raw_index);
            let fraction = vsubq_f32(position, vcvtq_f32_s32(raw_index));

            let raw_next = vaddq_s32(index, one_index);
            let next_wrapped = vcgtq_s32(raw_next, last_index);
            let next = vbslq_s32(next_wrapped, vdupq_n_s32(0), raw_next);

            let a_base = vaddq_s32(frame_a_base, index);
            let b_base = vaddq_s32(frame_b_base, index);
            let a_next = vaddq_s32(frame_a_base, next);
            let b_next = vaddq_s32(frame_b_base, next);

            // NEON gathers by hand: four reads a frame pair.
            let a0 = gather(pcm, a_base);
            let a1 = gather(pcm, a_next);
            let b0 = gather(pcm, b_base);
            let b1 = gather(pcm, b_next);

            let sample_a = vaddq_f32(a0, vmulq_f32(vsubq_f32(a1, a0), fraction));
            let sample_b = vaddq_f32(b0, vmulq_f32(vsubq_f32(b1, b0), fraction));
            let sample = vaddq_f32(
                sample_a,
                vmulq_f32(vsubq_f32(sample_b, sample_a), frame_interp),
            );
            vst1q_f32(values.as_mut_ptr().add(lane), sample);
            lane += LANES_PER_VECTOR;
        }
    }

    for (phase, value) in phases[lane..count]
        .iter()
        .copied()
        .zip(&mut values[lane..count])
    {
        *value = scalar_interpolate(pcm, frame_len, frame_a, frame_b, interp, phase);
    }
}

/// One four-lane table read. The caller validated every index this computes.
///
/// # Safety
///
/// Every lane of `offsets` indexes `pcm` within bounds.
#[inline]
unsafe fn gather(
    pcm: &[f32],
    offsets: std::arch::aarch64::int32x4_t,
) -> std::arch::aarch64::float32x4_t {
    let mut lanes = [0.0f32; LANES_PER_VECTOR];
    for (slot, lane) in lanes.iter_mut().enumerate() {
        // SAFETY: the caller validated the complete frames these offsets
        // address, and this kernel stays inside `count` lanes.
        let offset = vgetq_lane_s32(offsets, lane);
        *slot = *pcm.get_unchecked(offset as usize);
    }
    std::arch::aarch64::vld1q_f32(lanes.as_ptr())
}

fn scalar_interpolate(
    pcm: &[f32],
    frame_len: usize,
    frame_a: usize,
    frame_b: usize,
    interp: f32,
    phase: f32,
) -> f32 {
    let sample_frame = |frame: usize| {
        let base = frame * frame_len;
        let position = phase * frame_len as f32;
        let mut index = position as usize;
        let fraction = position - index as f32;
        if index >= frame_len {
            index = 0;
        }
        let a = pcm[base + index];
        let mut next = index + 1;
        if next >= frame_len {
            next = 0;
        }
        let b = pcm[base + next];
        a + (b - a) * fraction
    };
    let a = sample_frame(frame_a);
    let b = sample_frame(frame_b);
    a + (b - a) * interp
}

#[cfg(test)]
mod tests {
    use super::*;

    fn table(frame_len: usize, frames: usize) -> Vec<f32> {
        (0..frame_len * frames)
            .map(|index| {
                let x = index as f32 * 0.013_579;
                x.sin() * 0.7 + x.cos() * 0.2
            })
            .collect()
    }

    #[test]
    fn neon_matches_scalar_bit_for_bit_for_full_vectors_and_tails() {
        if !std::arch::is_aarch64_feature_detected!("neon") {
            return;
        }
        for frame_len in [17, 2_048] {
            let pcm = table(frame_len, 4);
            let phases: [f32; 32] = std::array::from_fn(|lane| match lane {
                0 => 0.0,
                1 => 1.0,
                _ => ((lane as f32 * 0.137) + 0.000_03).rem_euclid(1.0),
            });
            for count in [4, 5, 7, 8, 15, 16, 31, 32] {
                for interp in [0.0, 0.317, 1.0] {
                    let mut expected = [0.0; 32];
                    for lane in 0..count {
                        expected[lane] =
                            scalar_interpolate(&pcm, frame_len, 1, 3, interp, phases[lane]);
                    }
                    let mut actual = [0.0; 32];
                    // SAFETY: the test checked NEON and provides valid arrays
                    // and complete frames.
                    unsafe {
                        neon_interpolate_lanes(
                            &pcm,
                            frame_len,
                            1,
                            3,
                            interp,
                            &phases,
                            &mut actual,
                            count,
                        )
                    };
                    for lane in 0..count {
                        assert_eq!(
                            actual[lane].to_bits(),
                            expected[lane].to_bits(),
                            "frame_len={frame_len} count={count} interp={interp} lane={lane}",
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn neon_wavetable_cycle_end_reads_first_sample_in_vectors_and_tail() {
        if !std::arch::is_aarch64_feature_detected!("neon") {
            return;
        }
        for frame_len in [1, 17, 2048] {
            let pcm = table(frame_len, 4);
            for interp in [0.0, 0.317, 1.0] {
                let phases = [1.0f32; 9];
                let mut actual = [0.0f32; 9];
                // SAFETY: NEON checked above; both complete frames and all
                // nine lanes are in bounds, including one scalar tail lane.
                unsafe {
                    neon_interpolate_lanes(&pcm, frame_len, 1, 3, interp, &phases, &mut actual, 9);
                }
                let expected = pcm[frame_len] + (pcm[3 * frame_len] - pcm[frame_len]) * interp;
                for value in actual {
                    assert_eq!(value.to_bits(), expected.to_bits());
                }
            }
        }
    }
}
