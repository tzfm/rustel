//! Rust-owned audio backend facade for the headless rustel runtime.

#![forbid(unsafe_op_in_unsafe_fn)]

/// Fixed post-mix history retained when live visual analysis is enabled.
pub const LIVE_ANALYSIS_WINDOW_SAMPLES: usize = 4096;
/// Frames of left and right the master tap keeps for a stereo picture:
/// about 21 ms at 48 kHz, a vectorscope's worth.
pub const LIVE_ANALYSIS_SIDES_SAMPLES: usize = 1024;

// Constructed only by the device layer; the types stay compiled (and
// warning-free) so feature-less builds keep type-checking the live consumer.
pub use live::TakeoverCut;
pub mod assets;
pub mod backend;
mod biquad;
mod buffer;
pub mod bytebeat;
pub mod confirmation;
mod convolution_kernel;
#[cfg(feature = "device-audio")]
pub mod device;
mod device_facts;
mod dispatch;
pub mod distortion;
pub mod input;
pub mod limiter;
pub mod live;
pub mod live_control;
pub mod meter;
#[cfg(windows)]
pub mod mmcss;
mod output_options;
mod periodic_wave;
pub mod pressure;
#[cfg(feature = "device-audio")]
mod realtime;
pub mod render;
pub mod resample;
pub mod reverb;
pub mod ring;
mod sample;
pub mod scalar;
#[cfg_attr(not(feature = "device-audio"), allow(dead_code))]
pub mod stretch;
mod supersaw_kernel;
pub mod tripwire;
pub mod warp;
mod wavetable_kernel;
pub mod zzfx;

pub use backend::{
    AudioBackend, BYTEBEAT_EXPRESSIONS, BusMod, CompressorControls, DelayControls, DuckControls,
    DuckTarget, EnvMod, Envelope, EnvelopeParam, FilterControls, FilterEnvelope, FilterLfoKind,
    FilterStages, FmControls, FmOperator, FmRoute, FmWave, FxStage, LfoMod, MAX_BUSES,
    MAX_DUCK_TARGETS, MAX_FM_OPERATORS, MAX_FM_ROUTES, MAX_FX_STAGES, MAX_PARTIALS, MAX_VOICE_MODS,
    ModTarget, ModulatorParam, OnsetEvent, OscillatorControls, PartialsControls, PhaserControls,
    PitchEnvControls, PulseWidthLfoControls, ReverbControls, ShapeControls, StaticBiquad,
    SynthSource, TransientControls, TremoloControls, VibratoControls, VowelControls, Waveform,
    WavetableControls, fm_operator_slot,
};
pub use buffer::{AudioBufferError, AudioBufferPreference, AudioBufferRange};
pub use convolution_kernel::{ConvolutionKernelKind, selected_convolution_kernel_kind};
#[cfg(feature = "device-audio")]
pub use device::{
    AssetQueuePressureSnapshot, AudioDeviceInfo, AudioInput, CallbackTripwireArmError,
    DevicePlaybackError, LiveAnalysisSnapshot, LiveDeviceMemory, LiveDeviceReport, LiveReverbBatch,
    LiveScalarDevice, RecordCapture, RecordCaptureError, SILENT_OUTPUT_NAME, ScalarDevice,
    audio_device_inventory, audio_devices, input_retry_delay,
};
pub use device_facts::{
    AudioHost, AudioInputFacts, AudioOutputFacts, AudioSampleFormat, AudioStreamFacts,
};
pub use dispatch::DspDispatch;
pub use distortion::{DISTORTION_ALGORITHMS, DistortControls};
pub use limiter::{Character as LimiterCharacter, DEFAULT_THRESHOLD_DB, Limiter};
pub use live::{
    LINE_ARM_WITHDRAWN, LiveBlockReport, LiveFlipAtomics, LiveScalarBackend, MAX_LIVE_VOICES,
};
pub use meter::{
    LimiterSettings, MasterLevels, MasterMeter, MasterMeterShared, SILENCE_LUFS, db_to_linear,
    linear_to_db,
};
pub use output_options::LiveOutputOptions;
pub use pressure::{RealtimePool, RealtimePressureSnapshot};
#[cfg(feature = "device-audio")]
pub use realtime::RealtimeLoadSnapshot;
pub use render::{
    RENDER_CANCELLED, RenderControl, RenderLimiter, RenderTick, SilenceStop, WavSampleFormat,
    render_pcm, write_pcm16_wav, write_pcm16_wav_controlled, write_pcm16_wav_reporting,
    write_wav_controlled, write_wav_reporting,
};
pub use ring::{AudioEvent, QueuedAudioEvent, Ring};
pub use sample::{
    BUNDLED_BD_SAMPLE_ID, BUNDLED_BD_SAMPLE_IDENTITY, BundledSample, DEFAULT_SAMPLE_PCM_BYTES,
    DecodedSample, MAX_SAMPLE_PCM_BYTES, MIN_SAMPLE_PCM_BYTES, SAMPLE_BANK_CAPACITY, SampleBank,
    SampleControls, SampleHold, SampleId, SampleResamplingMode, decode_pcm16_mono_wav, decode_wav,
    format_sample_bytes, sample_pcm_ceiling, set_sample_pcm_ceiling,
};
pub use scalar::{
    MAX_ACTIVE_VOICES, MAX_CONFIGURABLE_POLYPHONY, MAX_ORBITS, MAX_PENDING_EVENTS, MAX_POLYPHONY,
    ScalarBackend,
};
pub use supersaw_kernel::{SupersawKernelKind, selected_supersaw_kernel_kind};
pub use warp::{WarpMode, warp_phase};
pub use wavetable_kernel::{WavetableKernelKind, selected_wavetable_kernel_kind};
