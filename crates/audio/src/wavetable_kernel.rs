//! Prepared kernels for wavetable interpolation.
//!
//! CPU selection happens while the backend is initialized. Portable machines
//! retain the original scalar lane loop; accelerated implementations only
//! replace the bounded table-interpolation arithmetic.

use std::sync::OnceLock;

#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
mod x86;

/// Wavetable interpolation implementation available to this binary.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum WavetableKernelKind {
    /// Inline scalar implementation available on every supported target.
    Portable,
    /// Eight-lane wavetable interpolation using x86 AVX2 instructions.
    Avx2,
}

impl WavetableKernelKind {
    /// Whether this binary contains the implementation.
    pub const fn compiled(self) -> bool {
        match self {
            Self::Portable => true,
            Self::Avx2 => cfg!(any(target_arch = "x86", target_arch = "x86_64")),
        }
    }
}

/// Implementation selected once for newly initialized scalar backends.
pub fn selected_wavetable_kernel_kind() -> WavetableKernelKind {
    *automatic_kind()
}

/// A target-feature implementation selected before the sample loop.
#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
#[derive(Clone, Copy, Debug)]
pub(crate) enum WavetableKernel {
    Avx2,
}

/// No architecture-specific wavetable kernel is compiled for this target.
#[cfg(not(any(target_arch = "x86", target_arch = "x86_64")))]
#[derive(Clone, Copy, Debug)]
pub(crate) struct WavetableKernel;

impl WavetableKernel {
    /// Choose the best implementation while the backend is initialized,
    /// before its audio callback can run.
    pub(crate) fn automatic() -> Option<Self> {
        match selected_wavetable_kernel_kind() {
            WavetableKernelKind::Portable => None,
            #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
            WavetableKernelKind::Avx2 => Some(Self::Avx2),
            #[cfg(not(any(target_arch = "x86", target_arch = "x86_64")))]
            WavetableKernelKind::Avx2 => unreachable!("AVX2 is not selectable on this target"),
        }
    }

    /// Interpolate independent lane phases when all gather indices can be
    /// represented by the selected implementation.
    ///
    /// Returns `false` without modifying `values` when the scalar path is
    /// required for an unusual or incomplete table.
    #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn try_interpolate(
        self,
        pcm: &[f32],
        frame_len: usize,
        frame_a: usize,
        frame_b: usize,
        interp: f32,
        phases: &[f32],
        values: &mut [f32],
        count: usize,
    ) -> bool {
        if count < 8
            || count > phases.len()
            || count > values.len()
            || frame_len == 0
            || pcm.len() > i32::MAX as usize
            || !complete_frame(pcm.len(), frame_len, frame_a)
            || !complete_frame(pcm.len(), frame_len, frame_b)
            || !phases[..count]
                .iter()
                .all(|phase| phase.is_finite() && (0.0..=1.0).contains(phase))
        {
            return false;
        }

        // SAFETY: construction established the target feature. The checks
        // above bound every slice and gather index used by the kernel.
        match self {
            #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
            Self::Avx2 => unsafe {
                x86::avx2_interpolate_lanes(
                    pcm, frame_len, frame_a, frame_b, interp, phases, values, count,
                )
            },
        }
        true
    }

    /// Non-x86 targets retain the scalar path until a target-specific kernel
    /// has independently passed the same fidelity and performance gates.
    #[cfg(not(any(target_arch = "x86", target_arch = "x86_64")))]
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn try_interpolate(
        self,
        _pcm: &[f32],
        _frame_len: usize,
        _frame_a: usize,
        _frame_b: usize,
        _interp: f32,
        _phases: &[f32],
        _values: &mut [f32],
        _count: usize,
    ) -> bool {
        false
    }
}

#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
fn complete_frame(pcm_len: usize, frame_len: usize, frame: usize) -> bool {
    frame
        .checked_mul(frame_len)
        .and_then(|base| base.checked_add(frame_len))
        .is_some_and(|end| end <= pcm_len)
}

fn automatic_kind() -> &'static WavetableKernelKind {
    static SELECTION: OnceLock<WavetableKernelKind> = OnceLock::new();
    SELECTION.get_or_init(detect_best_kernel)
}

fn detect_best_kernel() -> WavetableKernelKind {
    #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
    if std::arch::is_x86_feature_detected!("avx2") {
        return WavetableKernelKind::Avx2;
    }

    WavetableKernelKind::Portable
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn automatic_selection_is_cached() {
        let first = WavetableKernel::automatic();
        let second = WavetableKernel::automatic();
        assert_eq!(first.is_some(), second.is_some());

        #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
        assert_eq!(
            selected_wavetable_kernel_kind(),
            if std::arch::is_x86_feature_detected!("avx2") {
                WavetableKernelKind::Avx2
            } else {
                WavetableKernelKind::Portable
            }
        );
        #[cfg(not(any(target_arch = "x86", target_arch = "x86_64")))]
        assert_eq!(
            selected_wavetable_kernel_kind(),
            WavetableKernelKind::Portable
        );

        assert!(WavetableKernelKind::Portable.compiled());
        assert_eq!(
            WavetableKernelKind::Avx2.compiled(),
            cfg!(any(target_arch = "x86", target_arch = "x86_64"))
        );
    }

    #[test]
    fn incomplete_tables_and_unusual_phases_keep_the_scalar_path() {
        let Some(kernel) = WavetableKernel::automatic() else {
            return;
        };
        let pcm = [0.0; 31];
        let mut values = [7.0; 8];
        assert!(!kernel.try_interpolate(&pcm, 16, 0, 1, 0.5, &[0.0; 8], &mut values, 8,));
        assert_eq!(values, [7.0; 8]);

        let pcm = [0.0; 32];
        for phase in [f32::NAN, f32::INFINITY, -0.01, 1.01] {
            let phases = [phase; 8];
            assert!(!kernel.try_interpolate(&pcm, 16, 0, 1, 0.5, &phases, &mut values, 8,));
            assert_eq!(values, [7.0; 8]);
        }
    }
}
