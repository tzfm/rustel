//! Presentation-neutral facts about an opened audio stream.

use crate::AudioBufferPreference;

/// The host responsible for an opened output stream.
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum AudioHost {
    /// A native host selected through CPAL.
    Cpal { id: Box<str> },
    /// The engine's wall-clock output used when no hardware stream is open.
    Silent,
}

impl AudioHost {
    /// Record a native host identifier reported by CPAL.
    pub fn cpal(id: impl Into<Box<str>>) -> Self {
        Self::Cpal { id: id.into() }
    }

    /// Stable host category used by product projections.
    pub const fn kind(&self) -> &'static str {
        match self {
            Self::Cpal { .. } => "cpal",
            Self::Silent => "silent",
        }
    }

    /// CPAL's platform-neutral backend identifier, when a native host exists.
    pub fn id(&self) -> Option<&str> {
        match self {
            Self::Cpal { id } => Some(id),
            Self::Silent => None,
        }
    }
}

/// PCM sample representation accepted by the opened stream.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum AudioSampleFormat {
    I8,
    I16,
    I32,
    I64,
    U8,
    U16,
    U32,
    U64,
    F32,
    F64,
}

impl AudioSampleFormat {
    /// Stable machine-readable identity for reports.
    pub const fn code(self) -> &'static str {
        match self {
            Self::I8 => "i8",
            Self::I16 => "i16",
            Self::I32 => "i32",
            Self::I64 => "i64",
            Self::U8 => "u8",
            Self::U16 => "u16",
            Self::U32 => "u32",
            Self::U64 => "u64",
            Self::F32 => "f32",
            Self::F64 => "f64",
        }
    }
}

/// Stable facts about the optional input stream beside an output.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AudioInputFacts {
    device_id: Box<str>,
    sample_rate_hz: u32,
    channels: u16,
    sample_format: AudioSampleFormat,
}

impl AudioInputFacts {
    /// Construct facts captured from one opened input stream.
    pub fn new(
        device_id: impl Into<Box<str>>,
        sample_rate_hz: u32,
        channels: u16,
        sample_format: AudioSampleFormat,
    ) -> Self {
        Self {
            device_id: device_id.into(),
            sample_rate_hz,
            channels,
            sample_format,
        }
    }

    /// CPAL's stable device identifier.
    pub fn device_id(&self) -> &str {
        &self.device_id
    }

    /// The input stream's actual sample rate.
    pub const fn sample_rate_hz(&self) -> u32 {
        self.sample_rate_hz
    }

    /// The input stream's channel count.
    pub const fn channels(&self) -> u16 {
        self.channels
    }

    /// The input stream's PCM representation.
    pub const fn sample_format(&self) -> AudioSampleFormat {
        self.sample_format
    }
}

/// Stable facts about the opened output stream.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AudioOutputFacts {
    host: AudioHost,
    device_id: Box<str>,
    sample_rate_hz: u32,
    channels: u16,
    sample_format: Option<AudioSampleFormat>,
    buffer_preference: AudioBufferPreference,
    requested_buffer_frames: u32,
    reported_buffer_frames: Option<u32>,
    device_period_frames: Option<u32>,
    host_timestamps_available: bool,
}

impl AudioOutputFacts {
    /// Construct facts captured from one opened output stream.
    pub fn new(
        host: AudioHost,
        device_id: impl Into<Box<str>>,
        sample_rate_hz: u32,
        channels: u16,
        sample_format: Option<AudioSampleFormat>,
        requested_buffer_frames: u32,
        reported_buffer_frames: Option<u32>,
    ) -> Self {
        let host_timestamps_available = matches!(host, AudioHost::Cpal { .. });
        Self {
            host,
            device_id: device_id.into(),
            sample_rate_hz,
            channels,
            sample_format,
            buffer_preference: AudioBufferPreference::Auto,
            requested_buffer_frames,
            reported_buffer_frames,
            device_period_frames: None,
            host_timestamps_available,
        }
    }

    /// Native host or synthetic silent-output identity.
    pub const fn host(&self) -> &AudioHost {
        &self.host
    }

    /// Record the original preference independently of the resolved request.
    pub fn with_buffer_preference(mut self, preference: AudioBufferPreference) -> Self {
        self.buffer_preference = preference;
        self
    }

    /// Original selection before adjustment to the device's supported range.
    pub const fn buffer_preference(&self) -> AudioBufferPreference {
        self.buffer_preference
    }

    /// CPAL's stable output identifier, or `silent` for synthetic output.
    pub fn device_id(&self) -> &str {
        &self.device_id
    }

    /// The output stream's actual sample rate.
    pub const fn sample_rate_hz(&self) -> u32 {
        self.sample_rate_hz
    }

    /// The output stream's channel count.
    pub const fn channels(&self) -> u16 {
        self.channels
    }

    /// PCM representation accepted by the native output stream.
    ///
    /// Synthetic silent output has no host sample representation.
    pub const fn sample_format(&self) -> Option<AudioSampleFormat> {
        self.sample_format
    }

    /// Buffer size requested from the host when the stream was built.
    pub const fn requested_buffer_frames(&self) -> u32 {
        self.requested_buffer_frames
    }

    /// Host-reported callback buffer size, when the backend exposes it.
    pub const fn reported_buffer_frames(&self) -> Option<u32> {
        self.reported_buffer_frames
    }

    /// Record the fixed callback period a host advertised for the device.
    pub fn with_device_period_frames(mut self, period: Option<u32>) -> Self {
        self.device_period_frames = period;
        self
    }

    /// The host's fixed callback period, when it advertises one instead of
    /// a buffer range - WASAPI's shared-mode period, 480 frames at 48 kHz.
    /// A buffer asked for above it is kept queued ahead of the period rather
    /// than shrinking or growing the callback cadence.
    pub const fn device_period_frames(&self) -> Option<u32> {
        self.device_period_frames
    }

    /// The frames of output the stream holds. This is normally what the host
    /// reports per callback. On a host whose fixed period is below the
    /// requested buffer (WASAPI in shared mode), it is the requested buffer:
    /// the engine keeps that many frames queued ahead of the device, and a
    /// listener hears the output that much later. Falls back to the request
    /// when the host reports nothing.
    pub fn latency_frames(&self) -> u32 {
        match self.device_period_frames {
            Some(period) if period < self.requested_buffer_frames => self.requested_buffer_frames,
            _ => self
                .reported_buffer_frames
                .unwrap_or(self.requested_buffer_frames),
        }
    }

    /// Whether callback timing comes from native host timestamps.
    pub const fn host_timestamps_available(&self) -> bool {
        self.host_timestamps_available
    }

    /// One line that names the host, the device, the sample rate and the
    /// buffer.
    ///
    /// The frames shown are what the host reports per callback, or the
    /// requested size when the host reports nothing. The milliseconds use
    /// the actual sample rate. A host with a fixed period and a larger buffer
    /// queued ahead of it shows both, and the milliseconds are the buffer's
    /// (see [`Self::latency_frames`]).
    pub fn describe_one_line(&self) -> String {
        let host = self
            .host
            .id()
            .map_or_else(|| self.host.kind().to_owned(), str::to_owned);
        let frames = self.latency_frames();
        let size = match self.device_period_frames {
            Some(period) if period != frames => {
                format!("period {period} · buffer {frames} frames")
            }
            _ => format!("{frames} frames"),
        };
        let millis = if self.sample_rate_hz == 0 {
            String::new()
        } else {
            format!(
                " · {:.1} ms",
                f64::from(frames) * 1000.0 / f64::from(self.sample_rate_hz)
            )
        };
        format!(
            "{} · {} · {} Hz · {}{}",
            host, self.device_id, self.sample_rate_hz, size, millis
        )
    }
}

/// Stable facts about the opened output stream and its optional input.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AudioStreamFacts {
    output: AudioOutputFacts,
    input: Option<AudioInputFacts>,
}

impl AudioStreamFacts {
    /// Combine an output with the optional input opened beside it.
    pub fn new(output: AudioOutputFacts, input: Option<AudioInputFacts>) -> Self {
        Self { output, input }
    }

    /// Facts about the active output stream.
    pub const fn output(&self) -> &AudioOutputFacts {
        &self.output
    }

    /// Facts about the optional input stream.
    pub const fn input(&self) -> Option<&AudioInputFacts> {
        self.input.as_ref()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stable_codes_do_not_depend_on_debug_output() {
        assert_eq!(AudioHost::cpal("wasapi").kind(), "cpal");
        assert_eq!(AudioHost::Silent.kind(), "silent");
        assert_eq!(AudioSampleFormat::I16.code(), "i16");
        assert_eq!(AudioSampleFormat::F32.code(), "f32");
    }

    #[test]
    fn one_line_describes_host_device_rate_and_cost() {
        let native = AudioOutputFacts::new(
            AudioHost::cpal("WASAPI"),
            "Speakers (Realtek Audio)",
            48_000,
            2,
            Some(AudioSampleFormat::F32),
            128,
            Some(128),
        );
        assert_eq!(
            native.describe_one_line(),
            "WASAPI · Speakers (Realtek Audio) · 48000 Hz · 128 frames · 2.7 ms"
        );
        // A host that will not report falls back to the request; the guess
        // is the row's, not a silent substitution.
        let unreported = AudioOutputFacts::new(
            AudioHost::cpal("alsa"),
            "default",
            44_100,
            2,
            Some(AudioSampleFormat::I16),
            256,
            None,
        );
        assert_eq!(
            unreported.describe_one_line(),
            "alsa · default · 44100 Hz · 256 frames · 5.8 ms"
        );
        let silent = AudioOutputFacts::new(AudioHost::Silent, "silent", 48_000, 2, None, 256, None);
        assert_eq!(
            silent.describe_one_line(),
            "silent · silent · 48000 Hz · 256 frames · 5.3 ms"
        );
    }

    /// WASAPI's shared mode: the period is the callback cadence and a larger
    /// buffer asked for sits queued ahead of it. The line says both, and the
    /// milliseconds are the buffer's; a request that got the period itself
    /// reads as before.
    #[test]
    fn one_line_says_period_and_buffer_when_they_differ() {
        let wasapi = |requested| {
            AudioOutputFacts::new(
                AudioHost::cpal("WASAPI"),
                "Speakers",
                48_000,
                2,
                Some(AudioSampleFormat::F32),
                requested,
                Some(480),
            )
            .with_device_period_frames(Some(480))
        };
        let buffered = wasapi(2_048);
        assert_eq!(buffered.latency_frames(), 2_048);
        assert_eq!(buffered.device_period_frames(), Some(480));
        assert_eq!(
            buffered.describe_one_line(),
            "WASAPI · Speakers · 48000 Hz · period 480 · buffer 2048 frames · 42.7 ms"
        );
        let at_period = wasapi(480);
        assert_eq!(at_period.latency_frames(), 480);
        assert_eq!(
            at_period.describe_one_line(),
            "WASAPI · Speakers · 48000 Hz · 480 frames · 10.0 ms"
        );
    }

    #[test]
    fn buffer_facts_keep_preference_request_and_report_separate() {
        let output = AudioOutputFacts::new(
            AudioHost::cpal("test-host"),
            "test-output",
            48_000,
            2,
            Some(AudioSampleFormat::F32),
            256,
            Some(512),
        )
        .with_buffer_preference(AudioBufferPreference::Frames(128));
        assert_eq!(
            output.buffer_preference(),
            AudioBufferPreference::Frames(128)
        );
        assert_eq!(output.requested_buffer_frames(), 256);
        assert_eq!(output.reported_buffer_frames(), Some(512));
    }
}
