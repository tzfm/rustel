//! Prepared kernels for static supersaw lanes.
//!
//! CPU selection happens while the backend is initialized. Portable machines
//! retain the original inline scalar loop; only a proven accelerated
//! implementation crosses this boundary in the audio callback.

use std::sync::OnceLock;

#[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
mod x86;

type RenderStaticLanes = unsafe fn(&mut [f32], &[f32], usize, f32, f32, (f32, f32)) -> (f32, f32);

/// Static supersaw-lane implementation available to this binary.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum SupersawKernelKind {
    /// Inline scalar implementation available on every supported target.
    Portable,
    /// Eight-lane supersaw evaluation using x86 AVX2 instructions.
    Avx2,
}

impl SupersawKernelKind {
    /// Whether this binary contains the implementation.
    pub const fn compiled(self) -> bool {
        match self {
            Self::Portable => true,
            Self::Avx2 => cfg!(any(target_arch = "x86", target_arch = "x86_64")),
        }
    }
}

/// Implementation selected once for newly initialized scalar backends.
pub fn selected_supersaw_kernel_kind() -> SupersawKernelKind {
    *automatic_kind()
}

/// A target-feature implementation selected before the sample loop.
#[derive(Clone, Copy, Debug)]
pub(crate) struct StaticSupersawKernel {
    render: RenderStaticLanes,
    minimum_lanes: usize,
}

impl StaticSupersawKernel {
    /// Choose the best implementation while the backend is initialized,
    /// before its audio callback can run.
    pub(crate) fn automatic() -> Option<Self> {
        match selected_supersaw_kernel_kind() {
            SupersawKernelKind::Portable => None,
            #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
            SupersawKernelKind::Avx2 => Some(x86::AVX2),
            #[cfg(not(any(target_arch = "x86", target_arch = "x86_64")))]
            SupersawKernelKind::Avx2 => unreachable!("AVX2 is not selectable on this target"),
        }
    }

    /// The current kernels become worthwhile at one full vector. Smaller
    /// stacks keep the original scalar source loop.
    pub(crate) const fn supports(self, lanes: usize) -> bool {
        lanes >= self.minimum_lanes
    }

    /// # Safety
    ///
    /// This kernel may be called only after construction by [`Self::automatic`].
    /// `count` must be within both slices, and the ratios must be the finite,
    /// nondecreasing values prepared for a supersaw voice.
    #[inline]
    pub(crate) unsafe fn render(
        self,
        phases: &mut [f32],
        detune_ratios: &[f32],
        count: usize,
        frequency_hz: f32,
        sample_rate: f32,
        pan_gains: (f32, f32),
    ) -> (f32, f32) {
        debug_assert!(count >= 8 && count <= phases.len() && count <= detune_ratios.len());
        // SAFETY: construction established the target feature and the caller
        // supplies the prepared supersaw arrays described above.
        unsafe {
            (self.render)(
                phases,
                detune_ratios,
                count,
                frequency_hz,
                sample_rate,
                pan_gains,
            )
        }
    }
}

fn automatic_kind() -> &'static SupersawKernelKind {
    static SELECTION: OnceLock<SupersawKernelKind> = OnceLock::new();
    SELECTION.get_or_init(detect_best_kernel)
}

fn detect_best_kernel() -> SupersawKernelKind {
    #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
    if std::arch::is_x86_feature_detected!("avx2") {
        return SupersawKernelKind::Avx2;
    }

    SupersawKernelKind::Portable
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn automatic_selection_is_cached_and_respects_the_vector_width() {
        let first = StaticSupersawKernel::automatic();
        let second = StaticSupersawKernel::automatic();
        assert_eq!(first.is_some(), second.is_some());

        #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
        assert_eq!(
            selected_supersaw_kernel_kind(),
            if std::arch::is_x86_feature_detected!("avx2") {
                SupersawKernelKind::Avx2
            } else {
                SupersawKernelKind::Portable
            }
        );
        #[cfg(not(any(target_arch = "x86", target_arch = "x86_64")))]
        assert_eq!(
            selected_supersaw_kernel_kind(),
            SupersawKernelKind::Portable
        );

        if let Some(kernel) = first {
            assert!(!kernel.supports(7));
            assert!(kernel.supports(8));
            assert!(kernel.supports(32));
        }

        assert!(SupersawKernelKind::Portable.compiled());
        assert_eq!(
            SupersawKernelKind::Avx2.compiled(),
            cfg!(any(target_arch = "x86", target_arch = "x86_64"))
        );
    }
}
