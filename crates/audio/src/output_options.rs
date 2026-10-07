use crate::{AudioBufferPreference, DspDispatch, SampleResamplingMode};

/// Requests used when preparing live output, separate from the actual stream
/// facts reported by the host. Defaults retain automatic engine selection
/// and buffer policy, with linear sample interpolation.
#[derive(Clone, Copy, Debug, Default)]
pub struct LiveOutputOptions {
    dispatch: DspDispatch,
    buffer_preference: AudioBufferPreference,
    sample_resampling_mode: SampleResamplingMode,
}

impl LiveOutputOptions {
    /// Use the same prepared engine-kernel selection for voices and assets.
    pub const fn with_dispatch(mut self, dispatch: DspDispatch) -> Self {
        self.dispatch = dispatch;
        self
    }

    /// The immutable selection supplied to the live backend and producer.
    pub const fn dispatch(self) -> DspDispatch {
        self.dispatch
    }

    /// Request a callback period without changing the engine-kernel selection.
    /// The device validates and resolves this preference before preparation.
    pub const fn with_buffer_preference(mut self, preference: AudioBufferPreference) -> Self {
        self.buffer_preference = preference;
        self
    }

    /// Original buffer preference, before adjustment to a device's range.
    pub const fn buffer_preference(self) -> AudioBufferPreference {
        self.buffer_preference
    }

    /// Select sample interpolation before output starts. Output replacement
    /// keeps this selection. The default is [`SampleResamplingMode::Linear`].
    pub const fn with_sample_resampling_mode(mut self, mode: SampleResamplingMode) -> Self {
        self.sample_resampling_mode = mode;
        self
    }

    /// Sample interpolation used to prepare the live backend.
    pub const fn sample_resampling_mode(self) -> SampleResamplingMode {
        self.sample_resampling_mode
    }
}
