//! Immutable selection for the engine-owned DSP kernels.

use crate::convolution_kernel::{ConvolutionKernel, ConvolutionKernelKind};
use crate::supersaw_kernel::{StaticSupersawKernel, SupersawKernelKind};
use crate::wavetable_kernel::{WavetableKernel, WavetableKernelKind};

/// Kernels prepared before rendering or asset construction.
///
/// The portable selection uses the scalar references for convolution
/// accumulation, supersaw lanes and wavetable interpolation. RustFFT retains
/// its own automatic planning, and compiler-generated SIMD is unaffected.
#[derive(Clone, Copy)]
pub struct DspDispatch {
    forced_portable: bool,
    convolution: ConvolutionKernel,
    convolution_kind: ConvolutionKernelKind,
    supersaw: Option<StaticSupersawKernel>,
    wavetable: Option<WavetableKernel>,
}

impl DspDispatch {
    /// Select implementations using cached runtime CPU detection.
    pub fn automatic() -> Self {
        Self {
            forced_portable: false,
            convolution: ConvolutionKernel::automatic(),
            convolution_kind: crate::selected_convolution_kernel_kind(),
            supersaw: StaticSupersawKernel::automatic(),
            wavetable: WavetableKernel::automatic(),
        }
    }

    /// Select the scalar references for every engine-owned kernel.
    pub const fn portable() -> Self {
        Self {
            forced_portable: true,
            convolution: ConvolutionKernel::PORTABLE,
            convolution_kind: ConvolutionKernelKind::Portable,
            supersaw: None,
            wavetable: None,
        }
    }

    /// Whether the caller explicitly requested the scalar kernel references.
    pub const fn is_forced_portable(self) -> bool {
        self.forced_portable
    }

    pub const fn convolution_kernel_kind(self) -> ConvolutionKernelKind {
        self.convolution_kind
    }

    pub const fn supersaw_kernel_kind(self) -> SupersawKernelKind {
        if self.supersaw.is_some() {
            SupersawKernelKind::Avx2
        } else {
            SupersawKernelKind::Portable
        }
    }

    pub const fn wavetable_kernel_kind(self) -> WavetableKernelKind {
        if self.wavetable.is_some() {
            WavetableKernelKind::Avx2
        } else {
            WavetableKernelKind::Portable
        }
    }

    pub(crate) const fn convolution(self) -> ConvolutionKernel {
        self.convolution
    }

    pub(crate) const fn supersaw(self) -> Option<StaticSupersawKernel> {
        self.supersaw
    }

    pub(crate) const fn wavetable(self) -> Option<WavetableKernel> {
        self.wavetable
    }
}

impl Default for DspDispatch {
    fn default() -> Self {
        Self::automatic()
    }
}

impl std::fmt::Debug for DspDispatch {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("DspDispatch")
            .field("forced_portable", &self.is_forced_portable())
            .field("convolution", &self.convolution_kernel_kind())
            .field("supersaw", &self.supersaw_kernel_kind())
            .field("wavetable", &self.wavetable_kernel_kind())
            .finish()
    }
}
