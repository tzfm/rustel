//! x86 wavetable interpolation kernels.
//!
//! Table reads are gathered for eight independent lanes. Stereo summation,
//! detuning, phase advancement, and phase warping remain scalar and retain
//! their existing order. FMA is deliberately not used.

#[cfg(target_arch = "x86")]
use std::arch::x86::{
    _mm256_add_epi32, _mm256_add_ps, _mm256_blendv_epi8, _mm256_cmpgt_epi32, _mm256_cvtepi32_ps,
    _mm256_cvttps_epi32, _mm256_i32gather_ps, _mm256_loadu_ps, _mm256_mul_ps, _mm256_set1_epi32,
    _mm256_set1_ps, _mm256_setzero_si256, _mm256_storeu_ps, _mm256_sub_ps,
};
#[cfg(target_arch = "x86_64")]
use std::arch::x86_64::{
    _mm256_add_epi32, _mm256_add_ps, _mm256_blendv_epi8, _mm256_cmpgt_epi32, _mm256_cvtepi32_ps,
    _mm256_cvttps_epi32, _mm256_i32gather_ps, _mm256_loadu_ps, _mm256_mul_ps, _mm256_set1_epi32,
    _mm256_set1_ps, _mm256_setzero_si256, _mm256_storeu_ps, _mm256_sub_ps,
};

const LANES_PER_VECTOR: usize = 8;

/// # Safety
///
/// The caller must establish AVX2 support, valid slice bounds, two complete
/// frames whose absolute indices fit in `i32`, and finite phases in `0..=1`.
#[allow(clippy::too_many_arguments)]
#[target_feature(enable = "avx2")]
pub(super) unsafe fn avx2_interpolate_lanes(
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
        let frame_len_f32 = _mm256_set1_ps(frame_len as f32);
        let last_index = _mm256_set1_epi32(frame_len as i32 - 1);
        let one_index = _mm256_set1_epi32(1);
        let zero_index = _mm256_setzero_si256();
        let frame_a_base = _mm256_set1_epi32((frame_a * frame_len) as i32);
        let frame_b_base = _mm256_set1_epi32((frame_b * frame_len) as i32);
        let frame_interp = _mm256_set1_ps(interp);

        while lane < vectorized {
            let phase = _mm256_loadu_ps(phases.as_ptr().add(lane));
            let position = _mm256_mul_ps(phase, frame_len_f32);
            let raw_index = _mm256_cvttps_epi32(position);
            let wrapped = _mm256_cmpgt_epi32(raw_index, last_index);
            let index = _mm256_blendv_epi8(raw_index, zero_index, wrapped);
            let fraction = _mm256_sub_ps(position, _mm256_cvtepi32_ps(raw_index));

            let raw_next = _mm256_add_epi32(index, one_index);
            let next_wrapped = _mm256_cmpgt_epi32(raw_next, last_index);
            let next = _mm256_blendv_epi8(raw_next, zero_index, next_wrapped);

            let a0 = _mm256_i32gather_ps::<4>(pcm.as_ptr(), _mm256_add_epi32(frame_a_base, index));
            let a1 = _mm256_i32gather_ps::<4>(pcm.as_ptr(), _mm256_add_epi32(frame_a_base, next));
            let b0 = _mm256_i32gather_ps::<4>(pcm.as_ptr(), _mm256_add_epi32(frame_b_base, index));
            let b1 = _mm256_i32gather_ps::<4>(pcm.as_ptr(), _mm256_add_epi32(frame_b_base, next));

            let sample_a = _mm256_add_ps(a0, _mm256_mul_ps(_mm256_sub_ps(a1, a0), fraction));
            let sample_b = _mm256_add_ps(b0, _mm256_mul_ps(_mm256_sub_ps(b1, b0), fraction));
            let sample = _mm256_add_ps(
                sample_a,
                _mm256_mul_ps(_mm256_sub_ps(sample_b, sample_a), frame_interp),
            );
            _mm256_storeu_ps(values.as_mut_ptr().add(lane), sample);
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
    fn avx2_matches_scalar_bit_for_bit_for_full_vectors_and_tails() {
        if !std::arch::is_x86_feature_detected!("avx2") {
            return;
        }
        for frame_len in [17, 2_048] {
            let pcm = table(frame_len, 4);
            let phases: [f32; 32] = std::array::from_fn(|lane| match lane {
                0 => 0.0,
                1 => 1.0,
                _ => ((lane as f32 * 0.137) + 0.000_03).rem_euclid(1.0),
            });
            for count in [8, 9, 15, 16, 31, 32] {
                for interp in [0.0, 0.317, 1.0] {
                    let mut expected = [0.0; 32];
                    for lane in 0..count {
                        expected[lane] =
                            scalar_interpolate(&pcm, frame_len, 1, 3, interp, phases[lane]);
                    }
                    let mut actual = [0.0; 32];
                    // SAFETY: the test checked AVX2 and provides valid arrays
                    // and complete frames.
                    unsafe {
                        avx2_interpolate_lanes(
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
    fn avx2_wavetable_cycle_end_reads_first_sample_in_vectors_and_tail() {
        if !std::arch::is_x86_feature_detected!("avx2") {
            return;
        }
        for frame_len in [1, 17, 2048] {
            let pcm = table(frame_len, 4);
            for interp in [0.0, 0.317, 1.0] {
                let phases = [1.0; 9];
                let mut actual = [0.0; 9];
                // SAFETY: AVX2 checked above; both complete frames and all
                // nine lanes are in bounds, including one scalar tail lane.
                unsafe {
                    avx2_interpolate_lanes(&pcm, frame_len, 1, 3, interp, &phases, &mut actual, 9);
                }
                let expected = pcm[frame_len] + (pcm[3 * frame_len] - pcm[frame_len]) * interp;
                for value in actual {
                    assert_eq!(value.to_bits(), expected.to_bits());
                }
            }
        }
    }
}
