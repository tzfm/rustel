//! Read-only inventory of implementations this runtime can actually use.
//!
//! Hardware facts and product projections extend this model without teaching
//! the UI or CLI to make their own selection decisions.

use std::fmt;
use std::str::FromStr;
use std::sync::OnceLock;

pub use rustel_audio::{
    AudioHost, AudioInputFacts, AudioOutputFacts, AudioSampleFormat, AudioStreamFacts, DspDispatch,
};

/// Stable identity of one implemented engine capability.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
#[non_exhaustive]
pub enum CapabilityId {
    PortableScalarDsp,
    Avx2Convolution,
    Avx2Supersaw,
    Avx2Wavetable,
}

impl CapabilityId {
    /// Stable machine-readable identity for reports and overrides.
    pub const fn code(self) -> &'static str {
        match self {
            Self::PortableScalarDsp => "portable_scalar_dsp",
            Self::Avx2Convolution => "avx2_convolution",
            Self::Avx2Supersaw => "avx2_supersaw",
            Self::Avx2Wavetable => "avx2_wavetable",
        }
    }
}

/// Typed explanation for a capability's selection state.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum CapabilityReason {
    /// This is the only implementation currently available for its work.
    OnlyAvailableImplementation,
    /// This implementation remains active as the oracle and universal fallback.
    ReferenceAndFallback,
    /// An explicit portable override selected this implementation.
    ForcedByPreference,
    /// Runtime policy selected this implementation after checking the CPU.
    SelectedByRuntime,
    /// The binary contains this implementation, but the CPU cannot run it.
    CpuFeatureUnavailable,
    /// A portable override left this otherwise available implementation idle.
    DisabledByPreference,
}

impl CapabilityReason {
    /// Stable machine-readable identity for product projections.
    pub const fn code(self) -> &'static str {
        match self {
            Self::OnlyAvailableImplementation => "only_available_implementation",
            Self::ReferenceAndFallback => "reference_and_fallback",
            Self::ForcedByPreference => "forced_by_preference",
            Self::SelectedByRuntime => "selected_by_runtime",
            Self::CpuFeatureUnavailable => "cpu_feature_unavailable",
            Self::DisabledByPreference => "disabled_by_preference",
        }
    }
}

/// Four independent facts about one implemented capability.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CapabilityStatus {
    id: CapabilityId,
    compiled: bool,
    detected: bool,
    selected: bool,
    reason: CapabilityReason,
}

impl CapabilityStatus {
    const fn new(
        id: CapabilityId,
        compiled: bool,
        detected: bool,
        selected: bool,
        reason: CapabilityReason,
    ) -> Self {
        Self {
            id,
            compiled,
            detected,
            selected,
            reason,
        }
    }

    const fn selected_capability(id: CapabilityId, reason: CapabilityReason) -> Self {
        Self::new(id, true, true, true, reason)
    }

    /// The stable identity of this capability.
    pub const fn id(self) -> CapabilityId {
        self.id
    }

    /// Whether this binary contains the implementation.
    pub const fn compiled(self) -> bool {
        self.compiled
    }

    /// Whether the running machine satisfies its prerequisites.
    pub const fn detected(self) -> bool {
        self.detected
    }

    /// Whether runtime policy chose this implementation.
    pub const fn selected(self) -> bool {
        self.selected
    }

    /// Typed explanation for the selection state.
    pub const fn reason(self) -> CapabilityReason {
        self.reason
    }
}

/// Acceleration policy for engine-owned DSP kernels.
///
/// Named optimized implementations are added only when their kernels exist.
/// This does not control RustFFT's planner or other dependency dispatch.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[non_exhaustive]
pub enum AccelerationPreference {
    #[default]
    Auto,
    Portable,
}

impl AccelerationPreference {
    /// Resolve this preference into choices retained by an audio backend.
    pub fn dispatch(self) -> DspDispatch {
        match self {
            Self::Auto => DspDispatch::automatic(),
            Self::Portable => DspDispatch::portable(),
        }
    }

    /// Stable machine-readable identity for configuration and reports.
    pub const fn code(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Portable => "portable",
        }
    }
}

/// A requested acceleration implementation this runtime does not contain.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AccelerationPreferenceParseError {
    requested: Box<str>,
}

impl AccelerationPreferenceParseError {
    /// The unsupported value supplied by the caller.
    pub fn requested(&self) -> &str {
        &self.requested
    }
}

impl fmt::Display for AccelerationPreferenceParseError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "unknown acceleration implementation {:?}; expected auto or portable",
            self.requested
        )
    }
}

impl std::error::Error for AccelerationPreferenceParseError {}

impl FromStr for AccelerationPreference {
    type Err = AccelerationPreferenceParseError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "auto" => Ok(Self::Auto),
            "portable" => Ok(Self::Portable),
            _ => Err(AccelerationPreferenceParseError {
                requested: value.into(),
            }),
        }
    }
}

/// CPU architecture relevant to runtime feature detection.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum CpuArchitecture {
    X86,
    X86_64,
    Aarch64,
    Other,
}

impl CpuArchitecture {
    /// Stable machine-readable identity for reports.
    pub const fn code(self) -> &'static str {
        match self {
            Self::X86 => "x86",
            Self::X86_64 => "x86_64",
            Self::Aarch64 => "aarch64",
            Self::Other => "other",
        }
    }
}

/// One CPU feature the runtime knows how to detect safely.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
#[non_exhaustive]
pub enum CpuFeatureId {
    Avx2,
    Fma,
    Avx512f,
    Neon,
    Sve,
}

impl CpuFeatureId {
    /// Stable machine-readable identity for reports.
    pub const fn code(self) -> &'static str {
        match self {
            Self::Avx2 => "avx2",
            Self::Fma => "fma",
            Self::Avx512f => "avx512f",
            Self::Neon => "neon",
            Self::Sve => "sve",
        }
    }
}

/// Detection result for one feature relevant to the current architecture.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CpuFeatureStatus {
    id: CpuFeatureId,
    detected: bool,
}

impl CpuFeatureStatus {
    const fn new(id: CpuFeatureId, detected: bool) -> Self {
        Self { id, detected }
    }

    /// The stable identity of this feature.
    pub const fn id(self) -> CpuFeatureId {
        self.id
    }

    /// Whether the running CPU and operating system expose this feature.
    pub const fn detected(self) -> bool {
        self.detected
    }
}

/// CPU facts captured once for the process.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CpuFeatureSet {
    architecture: CpuArchitecture,
    statuses: [Option<CpuFeatureStatus>; 5],
}

impl CpuFeatureSet {
    fn detect() -> Self {
        detect_cpu_features()
    }

    /// The architecture on which these facts were detected.
    pub const fn architecture(self) -> CpuArchitecture {
        self.architecture
    }

    /// Features relevant to this architecture, including unavailable ones.
    pub fn statuses(&self) -> impl Iterator<Item = CpuFeatureStatus> + '_ {
        self.statuses.iter().flatten().copied()
    }

    /// `None` means the feature does not belong to this architecture.
    pub fn status(&self, id: CpuFeatureId) -> Option<CpuFeatureStatus> {
        self.statuses().find(|status| status.id == id)
    }
}

#[cfg(target_arch = "x86")]
fn detect_cpu_features() -> CpuFeatureSet {
    CpuFeatureSet {
        architecture: CpuArchitecture::X86,
        statuses: [
            Some(CpuFeatureStatus::new(
                CpuFeatureId::Avx2,
                std::arch::is_x86_feature_detected!("avx2"),
            )),
            Some(CpuFeatureStatus::new(
                CpuFeatureId::Fma,
                std::arch::is_x86_feature_detected!("fma"),
            )),
            Some(CpuFeatureStatus::new(
                CpuFeatureId::Avx512f,
                std::arch::is_x86_feature_detected!("avx512f"),
            )),
            None,
            None,
        ],
    }
}

#[cfg(target_arch = "x86_64")]
fn detect_cpu_features() -> CpuFeatureSet {
    CpuFeatureSet {
        architecture: CpuArchitecture::X86_64,
        statuses: [
            Some(CpuFeatureStatus::new(
                CpuFeatureId::Avx2,
                std::arch::is_x86_feature_detected!("avx2"),
            )),
            Some(CpuFeatureStatus::new(
                CpuFeatureId::Fma,
                std::arch::is_x86_feature_detected!("fma"),
            )),
            Some(CpuFeatureStatus::new(
                CpuFeatureId::Avx512f,
                std::arch::is_x86_feature_detected!("avx512f"),
            )),
            None,
            None,
        ],
    }
}

#[cfg(target_arch = "aarch64")]
fn detect_cpu_features() -> CpuFeatureSet {
    CpuFeatureSet {
        architecture: CpuArchitecture::Aarch64,
        statuses: [
            Some(CpuFeatureStatus::new(
                CpuFeatureId::Neon,
                std::arch::is_aarch64_feature_detected!("neon"),
            )),
            Some(CpuFeatureStatus::new(
                CpuFeatureId::Sve,
                std::arch::is_aarch64_feature_detected!("sve"),
            )),
            None,
            None,
            None,
        ],
    }
}

#[cfg(not(any(target_arch = "x86", target_arch = "x86_64", target_arch = "aarch64")))]
fn detect_cpu_features() -> CpuFeatureSet {
    CpuFeatureSet {
        architecture: CpuArchitecture::Other,
        statuses: [None; 5],
    }
}

/// One immutable selection snapshot shared by every product surface.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CapabilityRegistry {
    statuses: Box<[CapabilityStatus]>,
    cpu_features: CpuFeatureSet,
    preference: AccelerationPreference,
    audio: Option<AudioStreamFacts>,
}

impl CapabilityRegistry {
    fn new(dispatch: DspDispatch, cpu_features: CpuFeatureSet) -> Self {
        let preference = if dispatch.is_forced_portable() {
            AccelerationPreference::Portable
        } else {
            AccelerationPreference::Auto
        };
        let avx2_feature_detected = cpu_features
            .status(CpuFeatureId::Avx2)
            .is_some_and(|status| status.detected());
        let convolution_compiled = rustel_audio::ConvolutionKernelKind::Avx2.compiled();
        let convolution_detected = convolution_compiled && avx2_feature_detected;
        let convolution_selected =
            dispatch.convolution_kernel_kind() == rustel_audio::ConvolutionKernelKind::Avx2;
        let supersaw_compiled = rustel_audio::SupersawKernelKind::Avx2.compiled();
        let supersaw_detected = supersaw_compiled && avx2_feature_detected;
        let supersaw_selected =
            dispatch.supersaw_kernel_kind() == rustel_audio::SupersawKernelKind::Avx2;
        let wavetable_compiled = rustel_audio::WavetableKernelKind::Avx2.compiled();
        let wavetable_detected = wavetable_compiled && avx2_feature_detected;
        let wavetable_selected =
            dispatch.wavetable_kernel_kind() == rustel_audio::WavetableKernelKind::Avx2;
        let accelerated = convolution_selected || supersaw_selected || wavetable_selected;

        let mut statuses = Vec::with_capacity(
            1 + usize::from(convolution_compiled)
                + usize::from(supersaw_compiled)
                + usize::from(wavetable_compiled),
        );
        statuses.push(CapabilityStatus::selected_capability(
            CapabilityId::PortableScalarDsp,
            match preference {
                AccelerationPreference::Auto if accelerated => {
                    CapabilityReason::ReferenceAndFallback
                }
                AccelerationPreference::Auto => CapabilityReason::OnlyAvailableImplementation,
                AccelerationPreference::Portable => CapabilityReason::ForcedByPreference,
            },
        ));

        if convolution_compiled {
            let reason = match preference {
                AccelerationPreference::Portable => CapabilityReason::DisabledByPreference,
                AccelerationPreference::Auto if convolution_detected => {
                    CapabilityReason::SelectedByRuntime
                }
                AccelerationPreference::Auto => CapabilityReason::CpuFeatureUnavailable,
            };
            statuses.push(CapabilityStatus::new(
                CapabilityId::Avx2Convolution,
                true,
                convolution_detected,
                convolution_selected,
                reason,
            ));
        }
        if supersaw_compiled {
            let reason = match preference {
                AccelerationPreference::Portable => CapabilityReason::DisabledByPreference,
                AccelerationPreference::Auto if supersaw_detected => {
                    CapabilityReason::SelectedByRuntime
                }
                AccelerationPreference::Auto => CapabilityReason::CpuFeatureUnavailable,
            };
            statuses.push(CapabilityStatus::new(
                CapabilityId::Avx2Supersaw,
                true,
                supersaw_detected,
                supersaw_selected,
                reason,
            ));
        }
        if wavetable_compiled {
            let reason = match preference {
                AccelerationPreference::Portable => CapabilityReason::DisabledByPreference,
                AccelerationPreference::Auto if wavetable_detected => {
                    CapabilityReason::SelectedByRuntime
                }
                AccelerationPreference::Auto => CapabilityReason::CpuFeatureUnavailable,
            };
            statuses.push(CapabilityStatus::new(
                CapabilityId::Avx2Wavetable,
                true,
                wavetable_detected,
                wavetable_selected,
                reason,
            ));
        }
        Self {
            statuses: statuses.into_boxed_slice(),
            cpu_features,
            preference,
            audio: None,
        }
    }

    /// Every capability implemented by this binary, in stable report order.
    pub const fn statuses(&self) -> &[CapabilityStatus] {
        &self.statuses
    }

    /// Look up one capability without allocating or re-running detection.
    pub fn status(&self, id: CapabilityId) -> Option<CapabilityStatus> {
        self.statuses.iter().copied().find(|status| status.id == id)
    }

    /// CPU facts captured when this registry was constructed.
    pub const fn cpu_features(&self) -> CpuFeatureSet {
        self.cpu_features
    }

    /// The policy used to choose implemented capabilities.
    pub const fn preference(&self) -> AccelerationPreference {
        self.preference
    }

    /// Facts captured from the opened audio stream, when one was supplied.
    pub const fn audio(&self) -> Option<&AudioStreamFacts> {
        self.audio.as_ref()
    }

    /// Attach an opened stream's immutable facts to this registry snapshot.
    pub fn with_audio_facts(mut self, audio: AudioStreamFacts) -> Self {
        self.audio = Some(audio);
        self
    }
}

/// Process-wide automatic capability selection.
///
/// Detection runs once on first access. The stored result is immutable, and
/// hot loops never repeat feature checks.
pub fn capability_registry() -> &'static CapabilityRegistry {
    static REGISTRY: OnceLock<CapabilityRegistry> = OnceLock::new();
    REGISTRY
        .get_or_init(|| CapabilityRegistry::new(DspDispatch::automatic(), CpuFeatureSet::detect()))
}

/// Report the engine-owned kernel choices retained by an audio backend.
///
/// Selection comes from `dispatch`, not a fresh policy decision. CPU facts
/// remain the process-wide detected snapshot. RustFFT and other dependency
/// dispatch are outside this registry's kernel-selection scope.
pub fn capability_registry_for_dispatch(dispatch: DspDispatch) -> CapabilityRegistry {
    CapabilityRegistry::new(dispatch, capability_registry().cpu_features())
}

/// Construct an explicit report/test selection using already-detected CPU facts.
///
/// This exercises policy and report projections without mutating process-wide
/// audio dispatch. Audio objects retain the implementation chosen when they
/// were constructed; use [`capability_registry_for_dispatch`] to report one
/// such object's actual choices.
pub fn capability_registry_for(preference: AccelerationPreference) -> CapabilityRegistry {
    capability_registry_for_dispatch(preference.dispatch())
}

/// Automatic selection combined with facts from one opened audio stream.
///
/// For an explicitly configured backend, attach its stream facts to
/// [`capability_registry_for_dispatch`] instead.
pub fn capability_registry_with_audio(audio: AudioStreamFacts) -> CapabilityRegistry {
    capability_registry_for(AccelerationPreference::Auto).with_audio_facts(audio)
}

/// CPU facts stored by the process-wide selection.
pub fn detected_cpu_features() -> &'static CpuFeatureSet {
    &capability_registry().cpu_features
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn automatic_selection_reports_the_implementations_in_this_binary() {
        let registry = capability_registry();
        assert_eq!(registry.preference(), AccelerationPreference::Auto);
        assert_eq!(registry.audio(), None);
        assert_eq!(
            registry.statuses().len(),
            1 + usize::from(rustel_audio::ConvolutionKernelKind::Avx2.compiled())
                + usize::from(rustel_audio::SupersawKernelKind::Avx2.compiled())
                + usize::from(rustel_audio::WavetableKernelKind::Avx2.compiled())
        );

        let scalar = registry
            .status(CapabilityId::PortableScalarDsp)
            .expect("portable scalar DSP");
        assert!(scalar.compiled());
        assert!(scalar.detected());
        assert!(scalar.selected());
        assert_eq!(
            scalar.reason(),
            if rustel_audio::selected_convolution_kernel_kind()
                == rustel_audio::ConvolutionKernelKind::Avx2
                || rustel_audio::selected_supersaw_kernel_kind()
                    == rustel_audio::SupersawKernelKind::Avx2
                || rustel_audio::selected_wavetable_kernel_kind()
                    == rustel_audio::WavetableKernelKind::Avx2
            {
                CapabilityReason::ReferenceAndFallback
            } else {
                CapabilityReason::OnlyAvailableImplementation
            }
        );

        if rustel_audio::ConvolutionKernelKind::Avx2.compiled() {
            let avx2 = registry
                .status(CapabilityId::Avx2Convolution)
                .expect("AVX2 convolution");
            let detected = registry
                .cpu_features()
                .status(CpuFeatureId::Avx2)
                .is_some_and(|status| status.detected());
            assert!(avx2.compiled());
            assert_eq!(avx2.detected(), detected);
            assert_eq!(avx2.selected(), detected);
            assert_eq!(
                avx2.reason(),
                if detected {
                    CapabilityReason::SelectedByRuntime
                } else {
                    CapabilityReason::CpuFeatureUnavailable
                }
            );
        }

        if rustel_audio::SupersawKernelKind::Avx2.compiled() {
            let avx2 = registry
                .status(CapabilityId::Avx2Supersaw)
                .expect("AVX2 supersaw");
            let detected = registry
                .cpu_features()
                .status(CpuFeatureId::Avx2)
                .is_some_and(|status| status.detected());
            assert!(avx2.compiled());
            assert_eq!(avx2.detected(), detected);
            assert_eq!(avx2.selected(), detected);
            assert_eq!(
                avx2.reason(),
                if detected {
                    CapabilityReason::SelectedByRuntime
                } else {
                    CapabilityReason::CpuFeatureUnavailable
                }
            );
        }

        if rustel_audio::WavetableKernelKind::Avx2.compiled() {
            let avx2 = registry
                .status(CapabilityId::Avx2Wavetable)
                .expect("AVX2 wavetable");
            let detected = registry
                .cpu_features()
                .status(CpuFeatureId::Avx2)
                .is_some_and(|status| status.detected());
            assert!(avx2.compiled());
            assert_eq!(avx2.detected(), detected);
            assert_eq!(avx2.selected(), detected);
            assert_eq!(
                avx2.reason(),
                if detected {
                    CapabilityReason::SelectedByRuntime
                } else {
                    CapabilityReason::CpuFeatureUnavailable
                }
            );
        }
    }

    #[test]
    fn portable_override_changes_policy_not_hardware_facts() {
        let automatic = capability_registry();
        let portable = capability_registry_for(AccelerationPreference::Portable);

        assert_eq!(portable.preference(), AccelerationPreference::Portable);
        assert_eq!(portable.cpu_features(), automatic.cpu_features());
        assert_eq!(
            portable
                .status(CapabilityId::PortableScalarDsp)
                .expect("portable scalar DSP")
                .reason(),
            CapabilityReason::ForcedByPreference
        );
        if rustel_audio::ConvolutionKernelKind::Avx2.compiled() {
            let avx2 = portable
                .status(CapabilityId::Avx2Convolution)
                .expect("AVX2 convolution");
            assert!(!avx2.selected());
            assert_eq!(avx2.reason(), CapabilityReason::DisabledByPreference);
        }
        if rustel_audio::SupersawKernelKind::Avx2.compiled() {
            let avx2 = portable
                .status(CapabilityId::Avx2Supersaw)
                .expect("AVX2 supersaw");
            assert!(!avx2.selected());
            assert_eq!(avx2.reason(), CapabilityReason::DisabledByPreference);
        }
        if rustel_audio::WavetableKernelKind::Avx2.compiled() {
            let avx2 = portable
                .status(CapabilityId::Avx2Wavetable)
                .expect("AVX2 wavetable");
            assert!(!avx2.selected());
            assert_eq!(avx2.reason(), CapabilityReason::DisabledByPreference);
        }
    }

    #[test]
    fn actual_dispatch_reports_each_retained_kernel_selection() {
        for dispatch in [DspDispatch::automatic(), DspDispatch::portable()] {
            let registry = capability_registry_for_dispatch(dispatch);
            assert_eq!(
                registry.preference(),
                if dispatch.is_forced_portable() {
                    AccelerationPreference::Portable
                } else {
                    AccelerationPreference::Auto
                }
            );
            assert_eq!(registry.cpu_features(), *detected_cpu_features());
            assert_eq!(registry.audio(), None);

            for (id, compiled, selected) in [
                (
                    CapabilityId::Avx2Convolution,
                    rustel_audio::ConvolutionKernelKind::Avx2.compiled(),
                    dispatch.convolution_kernel_kind() == rustel_audio::ConvolutionKernelKind::Avx2,
                ),
                (
                    CapabilityId::Avx2Supersaw,
                    rustel_audio::SupersawKernelKind::Avx2.compiled(),
                    dispatch.supersaw_kernel_kind() == rustel_audio::SupersawKernelKind::Avx2,
                ),
                (
                    CapabilityId::Avx2Wavetable,
                    rustel_audio::WavetableKernelKind::Avx2.compiled(),
                    dispatch.wavetable_kernel_kind() == rustel_audio::WavetableKernelKind::Avx2,
                ),
            ] {
                let status = registry.status(id);
                assert_eq!(status.is_some(), compiled);
                assert_eq!(status.is_some_and(|status| status.selected()), selected);
                if let Some(status) = status {
                    assert!(!status.selected() || status.detected());
                    assert_eq!(
                        status.reason(),
                        if dispatch.is_forced_portable() {
                            CapabilityReason::DisabledByPreference
                        } else if selected {
                            CapabilityReason::SelectedByRuntime
                        } else {
                            CapabilityReason::CpuFeatureUnavailable
                        }
                    );
                }
            }
        }
    }

    #[test]
    fn portable_and_automatic_dispatch_reports_are_independent() {
        let automatic_dispatch = AccelerationPreference::Auto.dispatch();
        let portable_dispatch = AccelerationPreference::Portable.dispatch();
        assert!(!automatic_dispatch.is_forced_portable());
        assert!(portable_dispatch.is_forced_portable());

        let before = capability_registry_for_dispatch(automatic_dispatch);
        let portable = capability_registry_for_dispatch(portable_dispatch);
        let after = capability_registry_for_dispatch(automatic_dispatch);
        assert_eq!(before.statuses(), after.statuses());
        assert_eq!(before.statuses(), capability_registry().statuses());
        assert_eq!(before.cpu_features(), portable.cpu_features());
        assert_eq!(
            portable.statuses(),
            capability_registry_for(AccelerationPreference::Portable).statuses()
        );
        assert_eq!(
            portable.statuses(),
            capability_registry_for_dispatch(portable_dispatch).statuses()
        );
        assert_eq!(
            portable_dispatch.convolution_kernel_kind(),
            rustel_audio::ConvolutionKernelKind::Portable
        );
        assert_eq!(
            portable_dispatch.supersaw_kernel_kind(),
            rustel_audio::SupersawKernelKind::Portable
        );
        assert_eq!(
            portable_dispatch.wavetable_kernel_kind(),
            rustel_audio::WavetableKernelKind::Portable
        );
    }

    #[test]
    fn only_compiled_selection_names_are_accepted() {
        assert_eq!("auto".parse(), Ok(AccelerationPreference::Auto));
        assert_eq!("portable".parse(), Ok(AccelerationPreference::Portable));

        let error = "avx2"
            .parse::<AccelerationPreference>()
            .expect_err("named implementation selection is not exposed");
        assert_eq!(error.requested(), "avx2");
        assert_eq!(
            error.to_string(),
            "unknown acceleration implementation \"avx2\"; expected auto or portable"
        );
    }

    #[test]
    fn selected_capabilities_are_always_compiled_and_detected() {
        for preference in [
            AccelerationPreference::Auto,
            AccelerationPreference::Portable,
        ] {
            for status in capability_registry_for(preference).statuses() {
                assert!(!status.selected() || (status.compiled() && status.detected()));
            }
        }
    }

    #[test]
    fn report_identities_are_stable_and_typed() {
        assert_eq!(AccelerationPreference::Auto.code(), "auto");
        assert_eq!(AccelerationPreference::Portable.code(), "portable");
        assert_eq!(
            CapabilityId::PortableScalarDsp.code(),
            "portable_scalar_dsp"
        );
        assert_eq!(CapabilityId::Avx2Convolution.code(), "avx2_convolution");
        assert_eq!(CapabilityId::Avx2Supersaw.code(), "avx2_supersaw");
        assert_eq!(CapabilityId::Avx2Wavetable.code(), "avx2_wavetable");
        assert_eq!(
            CapabilityReason::OnlyAvailableImplementation.code(),
            "only_available_implementation"
        );
        assert_eq!(
            CapabilityReason::ReferenceAndFallback.code(),
            "reference_and_fallback"
        );
        assert_eq!(
            CapabilityReason::ForcedByPreference.code(),
            "forced_by_preference"
        );
        assert_eq!(
            CapabilityReason::SelectedByRuntime.code(),
            "selected_by_runtime"
        );
        assert_eq!(
            CapabilityReason::CpuFeatureUnavailable.code(),
            "cpu_feature_unavailable"
        );
        assert_eq!(
            CapabilityReason::DisabledByPreference.code(),
            "disabled_by_preference"
        );
        assert_eq!(CpuFeatureId::Avx2.code(), "avx2");
        assert_eq!(CpuFeatureId::Fma.code(), "fma");
        assert_eq!(CpuFeatureId::Avx512f.code(), "avx512f");
        assert_eq!(CpuFeatureId::Neon.code(), "neon");
        assert_eq!(CpuFeatureId::Sve.code(), "sve");
    }

    #[test]
    fn every_reader_observes_the_same_detected_snapshot() {
        assert!(std::ptr::eq(capability_registry(), capability_registry()));
        assert!(std::ptr::eq(
            detected_cpu_features(),
            detected_cpu_features()
        ));
    }

    #[cfg(feature = "device-audio")]
    #[test]
    fn an_opened_stream_is_attached_without_changing_selection() {
        let device =
            rustel_audio::LiveScalarDevice::start_silent(48_000, 1).expect("silent output");
        let registry = capability_registry_with_audio(device.audio_facts());

        assert_eq!(registry.preference(), AccelerationPreference::Auto);
        assert_eq!(registry.statuses(), capability_registry().statuses());
        let audio = registry.audio().expect("opened audio facts");
        assert_eq!(audio.output().host(), &AudioHost::Silent);
        assert_eq!(audio.output().device_id(), rustel_audio::SILENT_OUTPUT_NAME);
    }

    #[cfg(any(target_arch = "x86", target_arch = "x86_64"))]
    #[test]
    fn x86_detection_matches_the_standard_library_and_lists_no_arm_features() {
        let features = detected_cpu_features();
        assert_eq!(features.statuses().count(), 3);
        assert_eq!(
            features
                .status(CpuFeatureId::Avx2)
                .map(|status| status.detected()),
            Some(std::arch::is_x86_feature_detected!("avx2"))
        );
        assert_eq!(
            features
                .status(CpuFeatureId::Fma)
                .map(|status| status.detected()),
            Some(std::arch::is_x86_feature_detected!("fma"))
        );
        assert_eq!(
            features
                .status(CpuFeatureId::Avx512f)
                .map(|status| status.detected()),
            Some(std::arch::is_x86_feature_detected!("avx512f"))
        );
        assert_eq!(features.status(CpuFeatureId::Neon), None);
        assert_eq!(features.status(CpuFeatureId::Sve), None);
    }

    #[cfg(target_arch = "aarch64")]
    #[test]
    fn arm_detection_matches_the_standard_library_and_lists_no_x86_features() {
        let features = detected_cpu_features();
        assert_eq!(features.statuses().count(), 2);
        assert_eq!(
            features
                .status(CpuFeatureId::Neon)
                .map(|status| status.detected()),
            Some(std::arch::is_aarch64_feature_detected!("neon"))
        );
        assert_eq!(
            features
                .status(CpuFeatureId::Sve)
                .map(|status| status.detected()),
            Some(std::arch::is_aarch64_feature_detected!("sve"))
        );
        assert_eq!(features.status(CpuFeatureId::Avx2), None);
        assert_eq!(features.status(CpuFeatureId::Fma), None);
        assert_eq!(features.status(CpuFeatureId::Avx512f), None);
    }
}
