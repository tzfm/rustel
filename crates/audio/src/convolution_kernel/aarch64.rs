//! aarch64 frequency-domain kernels.
//!
//! `Complex<f32>` is `repr(C)` with two adjacent `f32` fields. The NEON
//! implementation handles two complex bins in one four-float vector. It
//! deliberately avoids FMA so every multiply, add, and subtract rounds at
//! the same boundary as the scalar oracle.

use std::arch::aarch64::{vaddq_f32, vld1q_f32, vmulq_f32, vrev64q_f32, vst1q_f32};
use std::arch::aarch64::{
    veorq_u32, vreinterpretq_f32_u32, vreinterpretq_u32_f32, vtrn1q_f32, vtrn2q_f32,
};

use rustfft::num_complex::Complex;

use super::ConvolutionKernel;

const COMPLEX_PER_VECTOR: usize = 2;

pub(super) const NEON: ConvolutionKernel = ConvolutionKernel {
    multiply_accumulate: neon_multiply_accumulate,
};

/// # Safety
///
/// The caller must establish NEON support before invoking this function.
#[target_feature(enable = "neon")]
unsafe fn neon_multiply_accumulate(
    accumulator: &mut [Complex<f32>],
    spectrum: &[Complex<f32>],
    partition: &[Complex<f32>],
) {
    const { assert!(std::mem::size_of::<Complex<f32>>() == 2 * std::mem::size_of::<f32>()) };

    let len = accumulator.len().min(spectrum.len()).min(partition.len());
    let vectorized_len = len - len % COMPLEX_PER_VECTOR;
    let mut index = 0;

    while index < vectorized_len {
        let float_index = index * 2;
        // SAFETY: `index..index + 2` is within all three slices. A
        // `Complex<f32>` is two adjacent floats, and NEON loads impose no
        // alignment requirement.
        unsafe {
            // Lane layout per vector: [l0.re, l0.im, l1.re, l1.im].
            let left = vld1q_f32(spectrum.as_ptr().cast::<f32>().add(float_index));
            let right = vld1q_f32(partition.as_ptr().cast::<f32>().add(float_index));
            let current = vld1q_f32(accumulator.as_ptr().cast::<f32>().add(float_index));

            // [l0.re, l0.re, l1.re, l1.re]
            let left_real = vtrn1q_f32(left, left);
            // [l0.im, l0.im, l1.im, l1.im]
            let left_imag = vtrn2q_f32(left, left);
            // [r0.im, r0.re, r1.im, r1.re]
            let right_swapped = vrev64q_f32(right);
            // Even lanes hold each bin's real product, odd lanes its
            // imaginary product, exactly as the scalar oracle writes them.
            let real_products = vmulq_f32(left_real, right);
            let imag_products = vmulq_f32(left_imag, right_swapped);
            // x86's addsub subtracts on the EVEN lanes, which hold each
            // bin's real product, and adds on the odd ones. Flip the sign of
            // the even lanes with an exclusive or against negative zero,
            // then a plain add matches the oracle bit for bit.
            let flip = vld1q_f32([-0.0f32, 0.0, -0.0, 0.0].as_ptr());
            let flipped = vreinterpretq_f32_u32(veorq_u32(
                vreinterpretq_u32_f32(imag_products),
                vreinterpretq_u32_f32(flip),
            ));
            let product = vaddq_f32(real_products, flipped);
            let result = vaddq_f32(current, product);

            vst1q_f32(
                accumulator.as_mut_ptr().cast::<f32>().add(float_index),
                result,
            );
        }
        index += COMPLEX_PER_VECTOR;
    }

    for ((slot, left), right) in accumulator[index..len]
        .iter_mut()
        .zip(&spectrum[index..len])
        .zip(&partition[index..len])
    {
        *slot += left * right;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bins(count: usize, seed: f32) -> Vec<Complex<f32>> {
        (0..count)
            .map(|index| {
                let x = seed + index as f32 * 0.37;
                Complex {
                    re: (x * 0.013).sin() * 0.9,
                    im: (x * 0.029).cos() * 0.8,
                }
            })
            .collect()
    }

    /// The kernel matches the scalar oracle bit for bit over the sizes a
    /// convolver actually uses, including the scalar tail.
    #[test]
    fn neon_matches_scalar_bit_for_bit() {
        if !std::arch::is_aarch64_feature_detected!("neon") {
            return;
        }
        for count in [2, 3, 7, 128, 256, 4096] {
            let spectrum = bins(count, 0.1);
            let partition = bins(count, 5.0);
            let mut expected = bins(count, 9.0);
            let mut actual = expected.clone();
            for ((slot, left), right) in expected.iter_mut().zip(&spectrum).zip(&partition) {
                *slot += *left * *right;
            }
            // SAFETY: the test checked NEON and provides equal-length
            // slices.
            unsafe {
                NEON.multiply_accumulate(&mut actual, &spectrum, &partition);
            }
            for (bin, (actual, expected)) in actual.iter().zip(&expected).enumerate() {
                assert_eq!(
                    actual.re.to_bits(),
                    expected.re.to_bits(),
                    "re {bin} of {count}"
                );
                assert_eq!(
                    actual.im.to_bits(),
                    expected.im.to_bits(),
                    "im {bin} of {count}"
                );
            }
        }
    }
}
