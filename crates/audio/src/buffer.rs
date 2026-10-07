//! Buffer requests resolved before opening or replacing an audio stream.
//!
//! A resolved request is distinct from the host's reported buffer size and
//! the frame count actually delivered to an audio callback.

use std::fmt;

/// The preferred buffer size, retained separately from the resolved request.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum AudioBufferPreference {
    /// Use the automatic frame count supplied by the device policy.
    #[default]
    Auto,
    /// Request 128 frames before applying the device range.
    LowLatency,
    /// Request 256 frames before applying the device range.
    Balanced,
    /// Request 2,048 frames before applying the device range.
    Safe,
    /// Request this frame count, clamped to the supported device range.
    Frames(u32),
}

/// The buffer range advertised by an audio device.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum AudioBufferRange {
    /// The host does not advertise a range; engine bounds still apply.
    #[default]
    Unknown,
    /// Inclusive minimum and maximum frame counts.
    Range { min: u32, max: u32 },
}

/// Why a buffer request cannot be resolved.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AudioBufferError {
    /// An explicit or automatic request falls outside the engine bounds.
    InvalidFrames(u32),
    /// The device's advertised minimum exceeds its maximum.
    InvalidRange { min: u32, max: u32 },
    /// The advertised range does not intersect the engine bounds.
    UnsupportedRange { min: u32, max: u32 },
}

impl AudioBufferPreference {
    /// Smallest frame count accepted by the engine's request policy.
    pub const MIN_FRAMES: u32 = 32;
    /// Largest frame count accepted by the engine's request policy.
    pub const MAX_FRAMES: u32 = 16_384;

    /// Reject an invalid explicit request before device probing or teardown.
    ///
    /// Automatic defaults are supplied and checked later by [`Self::resolve`].
    pub fn validate(self) -> Result<(), AudioBufferError> {
        match self {
            Self::Frames(frames) => validate_frames(frames),
            _ => Ok(()),
        }
    }

    /// Resolve a request within both the engine and advertised device bounds.
    ///
    /// `auto_frames` is used and validated only for [`Self::Auto`]. An invalid
    /// request is rejected before clamping; resolution never changes the
    /// stored preference or claims the host will deliver the requested size.
    pub fn resolve(
        self,
        auto_frames: u32,
        range: AudioBufferRange,
    ) -> Result<u32, AudioBufferError> {
        let frames = match self {
            Self::Auto => auto_frames,
            Self::LowLatency => 128,
            Self::Balanced => 256,
            Self::Safe => 2_048,
            Self::Frames(frames) => frames,
        };
        validate_frames(frames)?;

        let (min, max) = match range {
            AudioBufferRange::Unknown => (Self::MIN_FRAMES, Self::MAX_FRAMES),
            AudioBufferRange::Range { min, max } => {
                if min > max {
                    return Err(AudioBufferError::InvalidRange { min, max });
                }
                let lower = min.max(Self::MIN_FRAMES);
                let upper = max.min(Self::MAX_FRAMES);
                if lower > upper {
                    return Err(AudioBufferError::UnsupportedRange { min, max });
                }
                (lower, upper)
            }
        };
        Ok(frames.clamp(min, max))
    }
}

fn validate_frames(frames: u32) -> Result<(), AudioBufferError> {
    if (AudioBufferPreference::MIN_FRAMES..=AudioBufferPreference::MAX_FRAMES).contains(&frames) {
        Ok(())
    } else {
        Err(AudioBufferError::InvalidFrames(frames))
    }
}

impl fmt::Display for AudioBufferError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidFrames(frames) => write!(
                f,
                "audio buffer request {frames} is outside {}..={} frames",
                AudioBufferPreference::MIN_FRAMES,
                AudioBufferPreference::MAX_FRAMES,
            ),
            Self::InvalidRange { min, max } => {
                write!(f, "audio device buffer range {min}..={max} is reversed")
            }
            Self::UnsupportedRange { min, max } => write!(
                f,
                "audio device buffer range {min}..={max} does not overlap {}..={} frames",
                AudioBufferPreference::MIN_FRAMES,
                AudioBufferPreference::MAX_FRAMES,
            ),
        }
    }
}

impl std::error::Error for AudioBufferError {}

#[cfg(test)]
mod tests {
    use super::{AudioBufferError, AudioBufferPreference, AudioBufferRange};
    use AudioBufferPreference::{Auto, Balanced, Frames, LowLatency, Safe};
    use AudioBufferRange::{Range, Unknown};

    #[test]
    fn explicit_requests_validate_before_device_resolution() {
        for frames in [0, 31, 16_385, u32::MAX] {
            assert_eq!(
                Frames(frames).validate(),
                Err(AudioBufferError::InvalidFrames(frames))
            );
            assert_eq!(
                Frames(frames).resolve(256, Unknown),
                Err(AudioBufferError::InvalidFrames(frames))
            );
        }
        for frames in [32, 128, 513, 16_384] {
            assert_eq!(Frames(frames).validate(), Ok(()));
            assert_eq!(Frames(frames).resolve(256, Unknown), Ok(frames));
        }
    }

    #[test]
    fn presets_ignore_unselected_automatic_defaults() {
        assert_eq!(AudioBufferPreference::default(), Auto);
        assert_eq!(Auto.validate(), Ok(()));
        for (preference, frames) in [
            (LowLatency, 128),
            (Balanced, 256),
            (Safe, 2_048),
            (Frames(513), 513),
        ] {
            assert_eq!(preference.validate(), Ok(()));
            for unused_default in [0, u32::MAX] {
                assert_eq!(preference.resolve(unused_default, Unknown), Ok(frames));
            }
        }
    }

    #[test]
    fn automatic_defaults_obey_engine_bounds() {
        for frames in [32, 256, 2_048, 16_384] {
            assert_eq!(Auto.resolve(frames, Unknown), Ok(frames));
        }
        for frames in [0, 31, 16_385, u32::MAX] {
            assert_eq!(
                Auto.resolve(frames, Range { min: 128, max: 512 }),
                Err(AudioBufferError::InvalidFrames(frames))
            );
        }
    }

    #[test]
    fn requests_clamp_to_the_device_and_engine_intersection() {
        for (preference, min, max, expected) in [
            (LowLatency, 512, 1_024, 512),
            (Balanced, 64, 128, 128),
            (Safe, 64, 1_024, 1_024),
            (Frames(128), 256, 512, 256),
            (Frames(1_024), 64, 128, 128),
            (Frames(32), 0, 64, 32),
            (Frames(16_384), 8_192, u32::MAX, 16_384),
            (Balanced, 0, u32::MAX, 256),
            (Safe, 256, 256, 256),
            (Auto, 64, 256, 256),
        ] {
            assert_eq!(preference.resolve(2_048, Range { min, max }), Ok(expected));
        }
    }

    #[test]
    fn device_ranges_without_a_valid_engine_request_are_rejected() {
        for (min, max) in [(0, 0), (0, 31), (16_385, u32::MAX)] {
            assert_eq!(
                Balanced.resolve(256, Range { min, max }),
                Err(AudioBufferError::UnsupportedRange { min, max })
            );
        }
    }

    #[test]
    fn reversed_device_ranges_are_rejected_before_clamping() {
        for (min, max) in [(512, 128), (u32::MAX, 0)] {
            assert_eq!(
                Balanced.resolve(256, Range { min, max }),
                Err(AudioBufferError::InvalidRange { min, max })
            );
        }
    }
}
