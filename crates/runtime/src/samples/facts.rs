//! What one sample file says about itself: its length on disk and what its
//! header declares. The browser shows these as the details of the selected
//! sound. Nothing here decodes, so a long file costs what its header costs.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use super::codecs::{audio_track, codec_for, codec_name, is_audio_track};
use super::{Codec, SampleLibrary, ScoreCacheKind, cache_path};

/// The facts of one sample file.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SampleFacts {
    /// The file's length, when the file is on this machine.
    pub bytes: Option<u64>,
    /// What the header declares about the sound, when it could be read.
    pub sound: Option<SoundFacts>,
    /// The container holds a track that no sound decoder reads: a picture.
    pub picture: bool,
    /// A remote file that no cache holds yet, so there is nothing to read.
    pub remote: bool,
}

/// What a header declares about the sound in it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SoundFacts {
    /// The codec, where the container does not name it: Vorbis or Opus in
    /// an Ogg, AAC in an MP4.
    pub codec: Option<&'static str>,
    pub rate: Option<u32>,
    pub channels: Option<u16>,
    pub bits: Option<u32>,
    pub seconds: Option<f64>,
}

impl SampleLibrary {
    /// The facts of the file at `url`: a local file where it lies, a remote
    /// one from the cache a load filled. Reads the length and the header
    /// only, never the whole file. Blocking: keep it off the interface
    /// thread, because a folder can be on a slow disk or a network.
    pub fn sample_facts(&self, url: &str) -> SampleFacts {
        if let Some(path) = url.strip_prefix("file://") {
            // Only a regular file is read: a fifo or a device named like a
            // sample would hold the open, or never end.
            return local_file(url, path)
                .filter(|path| std::fs::metadata(path).is_ok_and(|metadata| metadata.is_file()))
                .map_or_else(SampleFacts::default, |path| facts_of(&path, url));
        }
        match self.cached_file(url) {
            Some(path) => facts_of(&path, url),
            None => SampleFacts {
                remote: true,
                ..SampleFacts::default()
            },
        }
    }

    /// The cache entry that holds a remote `url`'s bytes, if a load kept it.
    fn cached_file(&self, url: &str) -> Option<PathBuf> {
        [
            cache_path(&self.shared.host_cache, url),
            self.shared.score_cache.path(url, ScoreCacheKind::Audio),
        ]
        .into_iter()
        .find(|path| {
            std::fs::symlink_metadata(path)
                .is_ok_and(|metadata| metadata.is_file() && metadata.len() > 0)
        })
    }
}

/// A local file's path: as written, the way a folder the player imported
/// names its files, or decoded, the way a score's own folder names them.
fn local_file(url: &str, written: &str) -> Option<PathBuf> {
    let written = PathBuf::from(written);
    if written.exists() {
        return Some(written);
    }
    url::Url::parse(url).ok()?.to_file_path().ok()
}

/// The most of a file one read of its facts takes. A header is at the
/// start of a file, or found by seeks; a file with no mark a demuxer knows
/// is searched no further than this.
const HEADER_BUDGET: u64 = 16 * 1024 * 1024;

/// A file read no further than [`HEADER_BUDGET`]: past it, a read finds
/// the end. Seeks are free, because a container finds its header by them.
struct Budgeted {
    file: File,
    left: u64,
    length: Option<u64>,
}

impl Read for Budgeted {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        let room = usize::try_from(self.left)
            .unwrap_or(usize::MAX)
            .min(buffer.len());
        let read = self.file.read(&mut buffer[..room])?;
        self.left -= read as u64;
        Ok(read)
    }
}

impl Seek for Budgeted {
    fn seek(&mut self, position: SeekFrom) -> std::io::Result<u64> {
        self.file.seek(position)
    }
}

impl symphonia::core::io::MediaSource for Budgeted {
    fn is_seekable(&self) -> bool {
        true
    }

    fn byte_len(&self) -> Option<u64> {
        self.length
    }
}

fn facts_of(path: &Path, url: &str) -> SampleFacts {
    let Ok(file) = File::open(path) else {
        return SampleFacts::default();
    };
    let bytes = file.metadata().ok().map(|metadata| metadata.len());
    let codec = codec_for(url);
    let budgeted = || Budgeted {
        file,
        left: HEADER_BUDGET,
        length: bytes,
    };
    // A header is untrusted input, as a decode's is: a reader that
    // panics on it leaves the file with no facts.
    let read = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| match codec {
        Codec::Wav => (wav_facts(budgeted(), bytes), false),
        Codec::Mp3 => (probed(budgeted(), "mp3", false), false),
        Codec::Ogg => (probed(budgeted(), "ogg", true), false),
        Codec::Mp4 | Codec::Matroska => {
            let extension = if codec == Codec::Mp4 { "mp4" } else { "mkv" };
            probed_container(budgeted(), extension)
        }
    }));
    let (sound, picture) = read.unwrap_or((None, false));
    SampleFacts {
        bytes,
        sound,
        picture,
        remote: false,
    }
}

/// A compressed sound's header, through the demuxer the loader decodes it
/// with.
fn probed(file: Budgeted, extension: &str, name_codec: bool) -> Option<SoundFacts> {
    let format = demuxed(file, extension)?;
    let track = audio_track(format.as_ref(), extension).ok()?;
    Some(sound_of(track, name_codec))
}

/// A video container: its sound's header, and whether it holds a picture.
fn probed_container(file: Budgeted, extension: &str) -> (Option<SoundFacts>, bool) {
    let Some(format) = demuxed(file, extension) else {
        return (None, false);
    };
    let picture = format.tracks().iter().any(|track| !is_audio_track(track));
    let sound = audio_track(format.as_ref(), extension)
        .ok()
        .map(|track| sound_of(track, true));
    (sound, picture)
}

fn demuxed(
    file: Budgeted,
    extension: &str,
) -> Option<Box<dyn symphonia::core::formats::FormatReader>> {
    use symphonia::core::formats::FormatOptions;
    use symphonia::core::io::MediaSourceStream;
    use symphonia::core::meta::MetadataOptions;
    use symphonia::core::probe::Hint;

    let stream = MediaSourceStream::new(Box::new(file), Default::default());
    let mut hint = Hint::new();
    hint.with_extension(extension);
    symphonia::default::get_probe()
        .format(
            &hint,
            stream,
            &FormatOptions::default(),
            &MetadataOptions::default(),
        )
        .ok()
        .map(|probed| probed.format)
}

fn sound_of(track: &symphonia::core::formats::Track, name_codec: bool) -> SoundFacts {
    let params = &track.codec_params;
    let seconds = params.n_frames.and_then(|frames| match params.time_base {
        Some(base) => {
            let time = base.calc_time(frames);
            Some(time.seconds as f64 + time.frac)
        }
        None => params
            .sample_rate
            .filter(|rate| *rate > 0)
            .map(|rate| frames as f64 / f64::from(rate)),
    });
    SoundFacts {
        codec: name_codec.then(|| codec_name(params.codec)),
        rate: params.sample_rate,
        channels: params
            .channels
            .and_then(|channels| u16::try_from(channels.count()).ok()),
        // A compressed track's depth is the decoder's, not the file's.
        bits: None,
        seconds,
    }
}

/// A RIFF/WAVE header: the `fmt ` chunk, then the length of the `data`
/// chunk, read chunk by chunk with seeks over the rest. The data length is
/// the declared one, or what the file holds when that is shorter. A writer
/// that streamed the file declares 0 or the largest size, and the data
/// then runs to the end of the file. Only plain samples - integer PCM and
/// float - have a length by their frame size, and a depth.
fn wav_facts(mut file: impl Read + Seek, length: Option<u64>) -> Option<SoundFacts> {
    let mut riff = [0u8; 12];
    file.read_exact(&mut riff).ok()?;
    if &riff[0..4] != b"RIFF" || &riff[8..12] != b"WAVE" {
        return None;
    }
    let mut format: Option<(u16, u16, u32, u16)> = None;
    let mut at = 12u64;
    // A sample's header holds a few chunks before its data. Stop looking
    // after many more, rather than walk a file that is not a sample.
    for _ in 0..64 {
        let mut chunk = [0u8; 8];
        if file.read_exact(&mut chunk).is_err() {
            break;
        }
        let size = u64::from(u32::from_le_bytes([chunk[4], chunk[5], chunk[6], chunk[7]]));
        let start = at + 8;
        match &chunk[0..4] {
            b"fmt " if size >= 16 => {
                let mut fmt = [0u8; 16];
                file.read_exact(&mut fmt).ok()?;
                format = Some((
                    u16::from_le_bytes([fmt[0], fmt[1]]),
                    u16::from_le_bytes([fmt[2], fmt[3]]),
                    u32::from_le_bytes([fmt[4], fmt[5], fmt[6], fmt[7]]),
                    u16::from_le_bytes([fmt[14], fmt[15]]),
                ));
            }
            b"data" => {
                let (tag, channels, rate, bits) = format?;
                // PCM, IEEE float, or the extensible form of either.
                let plain = matches!(tag, 1 | 3 | 0xFFFE);
                let streamed = size == 0 || size == u64::from(u32::MAX);
                let on_disk = length.map(|length| length.saturating_sub(start));
                let held = match on_disk {
                    Some(on_disk) if streamed => on_disk,
                    Some(on_disk) => on_disk.min(size),
                    None => size,
                };
                let frame = u64::from(channels) * u64::from(bits).div_ceil(8);
                return Some(SoundFacts {
                    codec: None,
                    rate: Some(rate),
                    channels: Some(channels),
                    bits: plain.then_some(u32::from(bits)),
                    seconds: (plain && frame > 0 && rate > 0)
                        .then(|| (held / frame) as f64 / f64::from(rate)),
                });
            }
            _ => {}
        }
        at = start + size + size % 2;
        if file.seek(SeekFrom::Start(at)).is_err() {
            break;
        }
    }
    format.map(|(tag, channels, rate, bits)| SoundFacts {
        codec: None,
        rate: Some(rate),
        channels: Some(channels),
        bits: matches!(tag, 1 | 3 | 0xFFFE).then_some(u32::from(bits)),
        seconds: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wav(rate: u32, channels: u16, bits: u16, frames: u32, list_chunk: bool) -> Vec<u8> {
        let frame = u32::from(channels) * u32::from(bits / 8);
        let data = frames * frame;
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"RIFF");
        bytes.extend_from_slice(&0u32.to_le_bytes());
        bytes.extend_from_slice(b"WAVE");
        bytes.extend_from_slice(b"fmt ");
        bytes.extend_from_slice(&16u32.to_le_bytes());
        bytes.extend_from_slice(&1u16.to_le_bytes());
        bytes.extend_from_slice(&channels.to_le_bytes());
        bytes.extend_from_slice(&rate.to_le_bytes());
        bytes.extend_from_slice(&(rate * frame).to_le_bytes());
        bytes.extend_from_slice(&(frame as u16).to_le_bytes());
        bytes.extend_from_slice(&bits.to_le_bytes());
        if list_chunk {
            // An odd-sized chunk before the data, with its pad byte.
            bytes.extend_from_slice(b"LIST");
            bytes.extend_from_slice(&3u32.to_le_bytes());
            bytes.extend_from_slice(b"abc\0");
        }
        bytes.extend_from_slice(b"data");
        bytes.extend_from_slice(&data.to_le_bytes());
        bytes.resize(bytes.len() + data as usize, 0);
        bytes
    }

    fn written(directory: &Path, name: &str, bytes: &[u8]) -> String {
        let path = directory.join(name);
        std::fs::write(&path, bytes).unwrap();
        format!("file://{}", path.display())
    }

    #[test]
    fn a_wav_header_gives_its_rate_channels_depth_and_length() {
        let directory = tempfile::tempdir().unwrap();
        let bytes = wav(44_100, 2, 24, 22_050, true);
        let url = written(directory.path(), "pad.wav", &bytes);

        let facts = facts_of(Path::new(url.strip_prefix("file://").unwrap()), &url);
        assert_eq!(facts.bytes, Some(bytes.len() as u64));
        assert!(!facts.picture && !facts.remote);
        let sound = facts.sound.expect("the header is read");
        assert_eq!(
            (sound.rate, sound.channels, sound.bits, sound.codec),
            (Some(44_100), Some(2), Some(24), None)
        );
        assert_eq!(sound.seconds, Some(0.5));
    }

    #[test]
    fn a_wav_cut_short_counts_only_the_frames_it_holds() {
        let directory = tempfile::tempdir().unwrap();
        let mut bytes = wav(8_000, 1, 16, 8_000, false);
        bytes.truncate(bytes.len() - 8_000);
        let url = written(directory.path(), "cut.wav", &bytes);

        let facts = facts_of(Path::new(url.strip_prefix("file://").unwrap()), &url);
        assert_eq!(facts.sound.and_then(|sound| sound.seconds), Some(0.5));
    }

    #[test]
    fn a_video_container_names_its_sound_codec_and_its_picture() {
        let directory = tempfile::tempdir().unwrap();
        let clip = include_bytes!("../video/testdata/tiny_video_vp8.webm");
        let url = written(directory.path(), "clip.webm", clip);
        let facts = facts_of(Path::new(url.strip_prefix("file://").unwrap()), &url);
        assert!(facts.picture, "{facts:?}");
        let sound = facts.sound.expect("the sound track is read");
        assert_eq!(
            (sound.codec, sound.rate, sound.bits),
            (Some("Vorbis"), Some(8_000), None)
        );
        assert!(
            sound.seconds.is_some_and(|seconds| seconds > 0.5),
            "{sound:?}"
        );

        let silent = include_bytes!("../video/testdata/tiny_audio_only.webm");
        let url = written(directory.path(), "tone.webm", silent);
        let facts = facts_of(Path::new(url.strip_prefix("file://").unwrap()), &url);
        assert!(!facts.picture, "a sound-only WebM holds no picture");
        assert!(facts.sound.is_some());
    }

    #[test]
    fn a_remote_sound_no_cache_holds_has_nothing_to_read() {
        let library = SampleLibrary::empty_without_loading();
        let facts = library.sample_facts("https://example.invalid/kits/kick.wav");
        assert!(facts.remote);
        assert_eq!((facts.bytes, facts.sound), (None, None));
    }

    #[test]
    fn a_streamed_wav_runs_to_the_end_of_its_file() {
        let directory = tempfile::tempdir().unwrap();
        for declared in [0u32, u32::MAX] {
            let mut bytes = wav(8_000, 1, 16, 4_000, false);
            let size_at = bytes.len() - 8_000 - 4;
            bytes[size_at..size_at + 4].copy_from_slice(&declared.to_le_bytes());
            let url = written(directory.path(), "streamed.wav", &bytes);
            let facts = facts_of(Path::new(url.strip_prefix("file://").unwrap()), &url);
            assert_eq!(
                facts.sound.and_then(|sound| sound.seconds),
                Some(0.5),
                "declared {declared:#x}"
            );
        }
    }

    #[test]
    fn a_compressed_wav_has_no_length_by_its_frame_size() {
        let directory = tempfile::tempdir().unwrap();
        let mut bytes = wav(22_050, 1, 4, 1_000, false);
        // Format tag 2: Microsoft ADPCM, whose blocks hold many frames.
        bytes[20..22].copy_from_slice(&2u16.to_le_bytes());
        let url = written(directory.path(), "adpcm.wav", &bytes);
        let sound = facts_of(Path::new(url.strip_prefix("file://").unwrap()), &url)
            .sound
            .expect("the header is read");
        assert_eq!(
            (sound.rate, sound.channels, sound.bits, sound.seconds),
            (Some(22_050), Some(1), None, None)
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_fifo_named_like_a_sample_is_not_opened() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("pipe.wav");
        let name = std::ffi::CString::new(path.display().to_string()).unwrap();
        // SAFETY: a valid NUL-terminated path and a plain mode.
        assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);

        // Opening a fifo for reading waits for a writer that never comes.
        let facts = SampleLibrary::empty_without_loading()
            .sample_facts(&format!("file://{}", path.display()));
        assert_eq!(facts, SampleFacts::default());
    }

    #[test]
    fn a_file_that_is_no_sample_has_a_length_and_no_sound() {
        let directory = tempfile::tempdir().unwrap();
        let url = written(directory.path(), "notes.wav", b"not a riff file at all");

        let facts = facts_of(Path::new(url.strip_prefix("file://").unwrap()), &url);
        assert_eq!(facts.bytes, Some(22));
        assert_eq!(facts.sound, None);
    }
}
