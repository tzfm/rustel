//! How big one sample file may be, by what the file is.

use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

use super::Codec;
use super::codecs::codec_for;

/// The most one video file may hold before anybody has said otherwise.
pub const DEFAULT_VIDEO_FILE_BYTES: usize = 256 * 1024 * 1024;

/// The most a player may raise it to. The whole file stays in memory
/// while its sound and its picture decode, so this guards the host.
pub const MAX_VIDEO_FILE_BYTES: usize = 2 * 1024 * 1024 * 1024;

/// The video file ceiling in force. Process-wide for the reason the sound
/// ceiling is: the loader threads that enforce it have no engine in reach.
static VIDEO_FILE_CEILING: AtomicUsize = AtomicUsize::new(DEFAULT_VIDEO_FILE_BYTES);

/// How many times the video file ceiling moved, so a picture it refused
/// can be asked for again under the new one.
static VIDEO_CEILING_MOVES: AtomicU64 = AtomicU64::new(0);

/// The video file ceiling in force.
pub fn video_file_ceiling() -> usize {
    VIDEO_FILE_CEILING.load(Ordering::Relaxed)
}

/// Ask for a new video file ceiling. Returns what was set: the ask
/// clamped between the sound ceiling's floor and [`MAX_VIDEO_FILE_BYTES`].
pub fn set_video_file_ceiling(bytes: usize) -> usize {
    let wanted = bytes.clamp(rustel_audio::MIN_SAMPLE_PCM_BYTES, MAX_VIDEO_FILE_BYTES);
    if VIDEO_FILE_CEILING.swap(wanted, Ordering::Relaxed) != wanted {
        VIDEO_CEILING_MOVES.fetch_add(1, Ordering::Relaxed);
    }
    wanted
}

/// How many times the video file ceiling has moved.
#[cfg(feature = "video")]
pub(crate) fn video_ceiling_moves() -> u64 {
    VIDEO_CEILING_MOVES.load(Ordering::Relaxed)
}

/// The most one file may hold, and how a refusal says so.
///
/// A video container's picture is most of its bytes, so the file has a
/// ceiling of its own. Its sound still decodes under the sound ceiling.
#[derive(Clone, Copy, Debug)]
pub(crate) struct FileLimit {
    pub(crate) bytes: usize,
    video: bool,
}

impl FileLimit {
    pub(crate) fn of(url: &str) -> Self {
        match codec_for(url) {
            Codec::Mp4 | Codec::Matroska => Self {
                bytes: video_file_ceiling(),
                video: true,
            },
            Codec::Wav | Codec::Mp3 | Codec::Ogg => Self {
                bytes: rustel_audio::sample_pcm_ceiling(),
                video: false,
            },
        }
    }

    /// What the file is, for a refusal that names its kind.
    pub(crate) fn kind(self) -> &'static str {
        if self.video { "video" } else { "sample" }
    }

    /// Why `name` is refused. `size` is the file's own length, or `None`
    /// when the read stopped at the limit and nothing measured it.
    pub(crate) fn refusal(self, name: &str, size: Option<u64>) -> String {
        let limit = rustel_audio::format_sample_bytes(self.bytes);
        let holder = if self.video { "one video" } else { "one sound" };
        let refusal = match size {
            Some(size) => format!(
                "{name} is {}, past the {limit} {holder} can hold",
                rustel_audio::format_sample_bytes(usize::try_from(size).unwrap_or(usize::MAX))
            ),
            None => format!("{name} is over {limit}, the most {holder} can hold"),
        };
        if self.video {
            format!("{refusal}. Raise biggest video in Settings to load it")
        } else {
            refusal
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_video_file_names_the_setting_that_raises_its_limit() {
        let video = FileLimit::of("file:///clips/take.MOV");
        assert_eq!(video.kind(), "video");
        let refusal = video.refusal("take.MOV", Some(3 << 30));
        assert!(refusal.contains("one video can hold"), "{refusal}");
        assert!(
            refusal.ends_with("Raise biggest video in Settings to load it"),
            "{refusal}"
        );
        assert!(
            FileLimit::of("https://host/clip.webm?v=2")
                .refusal("clip.webm", None)
                .contains("the most one video can hold")
        );
    }

    #[test]
    fn a_sound_keeps_the_sound_ceiling_and_its_own_words() {
        let sound = FileLimit::of("file:///kits/kick.wav");
        assert_eq!(sound.kind(), "sample");
        assert_eq!(sound.bytes, rustel_audio::sample_pcm_ceiling());
        let refusal = sound.refusal("kick.wav", None);
        assert!(refusal.contains("the most one sound can hold"), "{refusal}");
        assert!(!refusal.contains("Settings"), "{refusal}");
    }

    /// The only test that moves the process-wide ceiling, so no other
    /// test reads a video file against a ceiling this one set.
    #[test]
    fn a_video_past_its_ceiling_loads_once_the_ceiling_is_raised() {
        let directory = tempfile::tempdir().expect("temp dir");
        let clip = directory.path().join("long take.mp4");
        std::fs::write(&clip, vec![0u8; 5 << 20]).expect("fixture");
        let url = format!("file://{}", clip.display());

        assert_eq!(
            set_video_file_ceiling(1),
            rustel_audio::MIN_SAMPLE_PCM_BYTES
        );
        let refusal = crate::samples::fetch_located(&url).expect_err("past the ceiling");
        assert!(
            refusal.contains("5.0 MB, past the 4.0 MB one video can hold"),
            "{refusal}"
        );
        assert!(
            refusal.contains("Raise biggest video in Settings"),
            "{refusal}"
        );

        set_video_file_ceiling(8 << 20);
        let bytes = crate::samples::fetch_located(&url).expect("inside the raised ceiling");
        assert_eq!(bytes.len(), 5 << 20);

        assert_eq!(set_video_file_ceiling(usize::MAX), MAX_VIDEO_FILE_BYTES);
        set_video_file_ceiling(DEFAULT_VIDEO_FILE_BYTES);
    }
}
