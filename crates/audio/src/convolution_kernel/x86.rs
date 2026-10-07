//! x86 frequency-domain kernels.
//!
//! `Complex<f32>` is `repr(C)` with two adjacent `f32` fields. The AVX2
//! implementation handles four complex bins in one unaligned eight-float
//! vector. It deliberately avoids FMA so every multiply, add, and subtract
//! rounds at the same boundary as the scalar oracle.

#[cfg(target_arch = "x86")]
use std::arch::x86::{
    _mm256_add_ps, _mm256_addsub_ps, _mm256_loadu_ps, _mm256_movehdup_ps, _mm256_moveldup_ps,
    _mm256_mul_ps, _mm256_permute_ps, _mm256_storeu_ps,
};
#[cfg(target_arch = "x86_64")]
use std::arch::x86_64::{
    _mm256_add_ps, _mm256_addsub_ps, _mm256_loadu_ps, _mm256_movehdup_ps, _mm256_moveldup_ps,
    _mm256_mul_ps, _mm256_permute_ps, _mm256_storeu_ps,
};

use rustfft::num_complex::Complex;

use super::ConvolutionKernel;

const COMPLEX_PER_VECTOR: usize = 4;

pub(super) const AVX2: ConvolutionKernel = ConvolutionKernel {
    multiply_accumulate: avx2_multiply_accumulate,
};

/// # Safety
///
/// The caller must establish AVX2 support before invoking this function.
#[target_feature(enable = "avx2")]
unsafe fn avx2_multiply_accumulate(
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
        // SAFETY: `index..index + 4` is within all three slices. A
        // `Complex<f32>` is two adjacent floats, and unaligned loads and
        // stores impose no stronger alignment requirement.
        unsafe {
            let left = _mm256_loadu_ps(spectrum.as_ptr().cast::<f32>().add(float_index));
            let right = _mm256_loadu_ps(partition.as_ptr().cast::<f32>().add(float_index));
            let current = _mm256_loadu_ps(accumulator.as_ptr().cast::<f32>().add(float_index));

            let left_real = _mm256_moveldup_ps(left);
            let left_imag = _mm256_movehdup_ps(left);
            let right_swapped = _mm256_permute_ps::<0b1011_0001>(right);
            let real_products = _mm256_mul_ps(left_real, right);
            let imag_products = _mm256_mul_ps(left_imag, right_swapped);
            let product = _mm256_addsub_ps(real_products, imag_products);
            let result = _mm256_add_ps(current, product);

            _mm256_storeu_ps(
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
