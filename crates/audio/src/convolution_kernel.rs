//! Prepared frequency-domain convolution kernels.
//!
//! Selection happens while a convolver is built. The audio callback invokes
//! the stored function directly and never performs CPU-feature detection.

use std::sync::OnceLock;

use rustfft::num_complex::Complex;

#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
mod x86;

type MultiplyAccumulate = unsafe fn(&mut [Complex<f32>], &[Complex<f32>], &[Complex<f32>]);

/// Frequency-domain convolution implementation available to this binary.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum ConvolutionKernelKind {
    /// Scalar reference implementation available on every supported target.
    Portable,
    /// Four-bin complex multiply-accumulate using x86 AVX2 instructions.
    Avx2,
}

impl ConvolutionKernelKind {
    /// Whether this binary contains the implementation.
    pub const fn compiled(self) -> bool {
        match self {
            Self::Portable => true,
            Self::Avx2 => cfg!(any(target_arch = "x86", target_arch = "x86_64")),
        }
    }
}

/// Implementation selected once for newly prepared convolution reverbs.
pub fn selected_convolution_kernel_kind() -> ConvolutionKernelKind {
    *automatic_kind()
}

/// One prepared implementation of the convolution inner loop.
#[derive(Clone, Copy)]
pub(crate) struct ConvolutionKernel {
    multiply_accumulate: MultiplyAccumulate,
}

impl ConvolutionKernel {
    /// Portable reference implementation available on every target.
    pub(crate) const PORTABLE: Self = Self {
        multiply_accumulate: portable_multiply_accumulate,
    };

    /// Best implementation supported by this binary and running CPU.
    ///
    /// Detection is cached for the process. Convolvers copy only the selected
    /// function pointer into their prepared state.
    pub(crate) fn automatic() -> Self {
        match selected_convolution_kernel_kind() {
            ConvolutionKernelKind::Portable => Self::PORTABLE,
            #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
            ConvolutionKernelKind::Avx2 => x86::AVX2,
            #[cfg(not(any(target_arch = "x86", target_arch = "x86_64")))]
            ConvolutionKernelKind::Avx2 => unreachable!("AVX2 is not selectable on this target"),
        }
    }

    #[cfg(test)]
    pub(crate) fn same_implementation(self, other: Self) -> bool {
        std::ptr::fn_addr_eq(self.multiply_accumulate, other.multiply_accumulate)
    }

    #[inline]
    pub(crate) fn multiply_accumulate(
        self,
        accumulator: &mut [Complex<f32>],
        spectrum: &[Complex<f32>],
        partition: &[Complex<f32>],
    ) {
        debug_assert_eq!(accumulator.len(), spectrum.len());
        debug_assert_eq!(accumulator.len(), partition.len());
        // SAFETY: kernels stored in this type are either portable or selected
        // only after their target features have been detected. Implementations
        // retain the portable zip semantics for unequal slices.
        unsafe { (self.multiply_accumulate)(accumulator, spectrum, partition) };
    }
}

fn automatic_kind() -> &'static ConvolutionKernelKind {
    static SELECTION: OnceLock<ConvolutionKernelKind> = OnceLock::new();
    SELECTION.get_or_init(detect_best_kernel)
}

fn detect_best_kernel() -> ConvolutionKernelKind {
    #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
    if std::arch::is_x86_feature_detected!("avx2") {
        return ConvolutionKernelKind::Avx2;
    }

    ConvolutionKernelKind::Portable
}

/// Scalar fidelity oracle for every architecture-specific implementation.
unsafe fn portable_multiply_accumulate(
    accumulator: &mut [Complex<f32>],
    spectrum: &[Complex<f32>],
    partition: &[Complex<f32>],
) {
    for ((accumulator, spectrum), partition) in accumulator.iter_mut().zip(spectrum).zip(partition)
    {
        *accumulator += spectrum * partition;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn value(index: usize, salt: usize) -> Complex<f32> {
        Complex {
            re: ((index * 17 + salt * 11) as f32 * 0.03125).sin(),
            im: ((index * 13 + salt * 7) as f32 * 0.0625).cos(),
        }
    }

    #[test]
    fn portable_kernel_matches_the_scalar_expression_for_varied_lengths() {
        assert_kernel_matches_scalar(ConvolutionKernel::PORTABLE);
    }

    fn assert_kernel_matches_scalar(kernel: ConvolutionKernel) {
        for len in [0, 1, 2, 3, 4, 7, 8, 15, 16, 255, 256, 4096] {
            for offset in 0..4 {
                let accumulator = (0..len + offset)
                    .map(|index| value(index, 1))
                    .collect::<Vec<_>>();
                let spectrum = (0..len + offset)
                    .map(|index| value(index, 2))
                    .collect::<Vec<_>>();
                let partition = (0..len + offset)
                    .map(|index| value(index, 3))
                    .collect::<Vec<_>>();
                let mut expected = accumulator[offset..].to_vec();
                for ((slot, left), right) in expected
                    .iter_mut()
                    .zip(&spectrum[offset..])
                    .zip(&partition[offset..])
                {
                    *slot += left * right;
                }

                let mut actual = accumulator;
                kernel.multiply_accumulate(
                    &mut actual[offset..],
                    &spectrum[offset..],
                    &partition[offset..],
                );

                for (index, (actual, expected)) in
                    actual[offset..].iter().zip(&expected).enumerate()
                {
                    assert_eq!(
                        actual.re.to_bits(),
                        expected.re.to_bits(),
                        "real {len}:{offset}:{index}"
                    );
                    assert_eq!(
                        actual.im.to_bits(),
                        expected.im.to_bits(),
                        "imag {len}:{offset}:{index}"
                    );
                }
            }
        }
    }

    #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
    #[test]
    fn avx2_kernel_matches_the_scalar_expression_bit_for_bit() {
        if !std::arch::is_x86_feature_detected!("avx2") {
            return;
        }
        assert_kernel_matches_scalar(x86::AVX2);
    }

    #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
    #[test]
    fn avx2_kernel_matches_scalar_special_float_behavior() {
        if !std::arch::is_x86_feature_detected!("avx2") {
            return;
        }

        let tiny = f32::from_bits(1);
        let mut actual = vec![
            Complex::new(0.0, -0.0),
            Complex::new(tiny, -tiny),
            Complex::new(f32::MIN_POSITIVE, -f32::MIN_POSITIVE),
            Complex::new(f32::MAX, -f32::MAX),
            Complex::new(f32::INFINITY, f32::NEG_INFINITY),
            Complex::new(f32::NAN, 1.0),
            Complex::new(-1.0, f32::NAN),
            Complex::new(0.5, -0.25),
        ];
        let spectrum = [
            Complex::new(-0.0, 0.0),
            Complex::new(-tiny, tiny),
            Complex::new(0.5, -0.25),
            Complex::new(2.0, -2.0),
            Complex::new(0.0, 1.0),
            Complex::new(2.0, 3.0),
            Complex::new(4.0, 5.0),
            Complex::new(f32::INFINITY, 0.0),
        ];
        let partition = [
            Complex::new(1.0, -1.0),
            Complex::new(tiny, tiny),
            Complex::new(-0.0, 0.0),
            Complex::new(0.5, 0.5),
            Complex::new(f32::INFINITY, 0.0),
            Complex::new(7.0, 11.0),
            Complex::new(13.0, 17.0),
            Complex::new(0.0, 1.0),
        ];
        let mut expected = actual.clone();
        for ((slot, left), right) in expected.iter_mut().zip(&spectrum).zip(&partition) {
            *slot += left * right;
        }

        x86::AVX2.multiply_accumulate(&mut actual, &spectrum, &partition);
        for (index, (actual, expected)) in actual.iter().zip(&expected).enumerate() {
            for (part, actual, expected) in [
                ("real", actual.re, expected.re),
                ("imaginary", actual.im, expected.im),
            ] {
                if expected.is_nan() {
                    assert!(actual.is_nan(), "{part} component {index}");
                } else {
                    assert_eq!(
                        actual.to_bits(),
                        expected.to_bits(),
                        "{part} component {index}"
                    );
                }
            }
        }
    }

    #[test]
    fn automatic_selection_matches_detected_hardware() {
        let kind = selected_convolution_kernel_kind();
        #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
        assert_eq!(
            kind,
            if std::arch::is_x86_feature_detected!("avx2") {
                ConvolutionKernelKind::Avx2
            } else {
                ConvolutionKernelKind::Portable
            }
        );
        #[cfg(not(any(target_arch = "x86", target_arch = "x86_64")))]
        assert_eq!(kind, ConvolutionKernelKind::Portable);

        assert!(ConvolutionKernelKind::Portable.compiled());
        assert_eq!(
            ConvolutionKernelKind::Avx2.compiled(),
            cfg!(any(target_arch = "x86", target_arch = "x86_64"))
        );
    }
}
