//! Incoming stereo mix chunks, written to a WAV file by a dedicated thread.
//!
//! Chunks are enqueued without waiting for disk writes. Opening a file and
//! blocking finish or Drop remain synchronous; requested closure can instead
//! be polled without joining an unfinished writer. The writer patches the
//! header every second so a set that ends in a crash still leaves a playable
//! file. Samples are clipped and quantized to 24-bit PCM; the final raw summary
//! precedes that conversion and includes producer-declared synthetic silence.
//! Silence uses fixed writer storage, not a zero-filled producer allocation;
//! a large silence run or stalled disk can still make shutdown arbitrarily slow.

use std::fs::File;
use std::io::{self, BufWriter, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, sync_channel};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

const CHANNELS: u16 = 2;
const BYTES_PER_SAMPLE: u32 = 3;
const HEADER_BYTES: u64 = 44;
/// How often the sizes in the header are brought up to date.
const HEADER_REFRESH: Duration = Duration::from_secs(1);
/// Chunks waiting for the disk before the engine counts them as dropped.
const QUEUE_CHUNKS: usize = 256;

/// Raw samples received by the writer, before PCM24 conversion.
///
/// Includes declared synthetic silence and chunks drained after an I/O failure.
/// This does not establish
/// successful writing, recording-tap coverage, or physical audio output.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct RawSignalSummary {
    pub sample_count: u64,
    pub nonfinite_count: u64,
    /// Largest absolute finite sample, without clipping; zero when none exist.
    pub finite_peak: f32,
}

impl RawSignalSummary {
    fn observe(&mut self, samples: &[f32], silence_frames: u64) -> Result<(), &'static str> {
        let count = sample_count(samples.len(), silence_frames)?;
        let sample_count = self
            .sample_count
            .checked_add(count)
            .ok_or("raw sample count overflow")?;
        let mut nonfinite = 0u64;
        let mut finite_peak = self.finite_peak;
        for sample in samples {
            if sample.is_finite() {
                finite_peak = finite_peak.max(sample.abs());
            } else {
                // Bounded by the checked, representable slice length.
                nonfinite += 1;
            }
        }
        let nonfinite_count = self
            .nonfinite_count
            .checked_add(nonfinite)
            .ok_or("raw non-finite sample count overflow")?;
        *self = Self {
            sample_count,
            nonfinite_count,
            finite_peak,
        };
        Ok(())
    }
}

/// Where a take stands, as the engine reports it.
#[derive(Clone, Debug, PartialEq)]
pub struct TakeStatus {
    pub path: PathBuf,
    pub sample_rate: u32,
    /// Frames on disk (or queued for it).
    pub frames: u64,
    /// Bytes written so far, header included.
    pub bytes: u64,
    /// An I/O, writer-thread, or raw-summary failure.
    pub error: Option<String>,
    /// Exact received-input summary after joining the writer. None while
    /// open, or if the writer panicked or a summary count overflowed.
    /// A present summary does not imply that `error` is None.
    pub final_signal: Option<RawSignalSummary>,
}

impl TakeStatus {
    pub fn seconds(&self) -> f64 {
        self.frames as f64 / f64::from(self.sample_rate.max(1))
    }

    /// Whether the completed writer received only digital silence.
    pub fn is_silent(&self) -> bool {
        self.final_signal
            .is_some_and(|signal| signal.nonfinite_count == 0 && signal.finite_peak == 0.0)
    }
}

struct Shared {
    frames: AtomicU64,
    bytes: AtomicU64,
    error: Mutex<Option<String>>,
}

impl Shared {
    fn record_error(&self, error: &str) {
        let mut previous = self.error.lock().unwrap_or_else(|e| e.into_inner());
        match previous.as_mut() {
            Some(message)
                if message.as_str() == error || message.split("; ").any(|cause| cause == error) => {
            }
            Some(message) => {
                message.push_str("; ");
                message.push_str(error);
            }
            None => *previous = Some(error.to_owned()),
        }
    }
}

#[derive(Debug)]
struct TakeChunk {
    samples: Vec<f32>,
    silence_frames: u64,
}

fn sample_count(samples: usize, silence_frames: u64) -> Result<u64, &'static str> {
    u64::try_from(samples)
        .ok()
        .and_then(|samples| silence_frames.checked_mul(2)?.checked_add(samples))
        .ok_or("raw sample count overflow")
}

fn file_bytes(samples: u64) -> Result<u64, &'static str> {
    samples
        .checked_mul(u64::from(BYTES_PER_SAMPLE))
        .and_then(|bytes| bytes.checked_add(HEADER_BYTES))
        .ok_or("recording byte count overflow")
}

pub struct TakeWriter {
    sender: Option<SyncSender<TakeChunk>>,
    join: Option<JoinHandle<Result<RawSignalSummary, &'static str>>>,
    shared: Arc<Shared>,
    path: PathBuf,
    sample_rate: u32,
    queued_frames: u64,
    // Unlike per-item floor frame counts, this includes odd public Vec tails.
    queued_samples: u64,
    final_signal: Option<RawSignalSummary>,
}

impl TakeWriter {
    /// Create the file, write its header, and start the thread.
    pub fn start(path: PathBuf, sample_rate: u32) -> io::Result<Self> {
        let mut file = File::create(&path)?;
        write_header(&mut file, sample_rate, 0)?;
        file.flush()?;
        let shared = Arc::new(Shared {
            frames: AtomicU64::new(0),
            bytes: AtomicU64::new(HEADER_BYTES),
            error: Mutex::new(None),
        });
        let (sender, receiver) = sync_channel(QUEUE_CHUNKS);
        let worker = Arc::clone(&shared);
        let join = thread::Builder::new()
            .name("studio-record".into())
            .spawn(move || run(file, sample_rate, receiver, &worker))?;
        Ok(Self {
            sender: Some(sender),
            join: Some(join),
            shared,
            path,
            sample_rate,
            queued_frames: 0,
            queued_samples: 0,
            final_signal: None,
        })
    }

    /// Hand over interleaved stereo frames. Returns `false` when the queue is
    /// full, closed or disconnected, or checked count arithmetic overflows.
    /// Overflow records an error and closes input; rejected chunks are dropped.
    pub fn push(&mut self, chunk: Vec<f32>) -> bool {
        match self.push_padded(chunk, 0) {
            Ok(accepted) => accepted,
            Err(error) => {
                self.fail(error);
                self.request_close();
                false
            }
        }
    }

    /// One admission for PCM followed by synthetic silence; Full still drops
    /// the whole item. Public odd-length PCM keeps its per-item floor frames.
    pub(super) fn push_padded(
        &mut self,
        samples: Vec<f32>,
        silence_frames: u64,
    ) -> Result<bool, &'static str> {
        if self.sender.is_none() {
            return Ok(false);
        }
        let count = sample_count(samples.len(), silence_frames)?;
        let frames = self
            .queued_frames
            .checked_add(count / 2)
            .ok_or("recording frame count overflow")?;
        let queued_samples = self
            .queued_samples
            .checked_add(count)
            .ok_or("raw sample count overflow")?;
        file_bytes(queued_samples)?;
        let chunk = TakeChunk {
            samples,
            silence_frames,
        };
        match self.sender.as_ref() {
            Some(sender) if sender.try_send(chunk).is_ok() => {
                self.queued_frames = frames;
                self.queued_samples = queued_samples;
                Ok(true)
            }
            _ => Ok(false),
        }
    }

    pub(super) fn fail(&self, error: &str) {
        self.shared.record_error(error);
    }

    pub fn status(&self) -> TakeStatus {
        TakeStatus {
            path: self.path.clone(),
            sample_rate: self.sample_rate,
            frames: self
                .shared
                .frames
                .load(Ordering::Relaxed)
                .max(self.queued_frames),
            bytes: self.shared.bytes.load(Ordering::Relaxed),
            error: self
                .shared
                .error
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone(),
            final_signal: self.final_signal,
        }
    }

    /// Stop accepting chunks and let the writer drain its existing queue.
    /// Repeated requests are harmless. Retain this owner until collection:
    /// dropping it still waits for the writer.
    pub(super) fn request_close(&mut self) {
        drop(self.sender.take());
    }

    /// Collect the joined result once, after closure has been requested.
    /// Returns None while open, unfinished, or already collected.
    /// A finished handle can still briefly wait for thread teardown when
    /// joined; this is neither wait-free nor suitable for an audio callback.
    pub(super) fn try_finish(&mut self) -> Option<TakeStatus> {
        if self.sender.is_some() || !self.join.as_ref()?.is_finished() {
            return None;
        }
        self.join_writer();
        Some(self.status())
    }

    /// Drain and join the writer, blocking until completion and returning
    /// its final summary and any error.
    /// On success, everything queued reaches the disk and the header carries
    /// the final sizes.
    pub fn finish(mut self) -> TakeStatus {
        self.request_close();
        if let Some(status) = self.try_finish() {
            return status;
        }
        self.join_writer();
        self.status()
    }

    fn close(&mut self) {
        self.request_close();
        self.join_writer();
    }

    fn join_writer(&mut self) {
        if let Some(join) = self.join.take() {
            let error = match join.join() {
                Ok(Ok(summary)) => {
                    self.final_signal = Some(summary);
                    None
                }
                Ok(Err(error)) => Some(error),
                Err(_) => Some("recording writer thread panicked"),
            };
            if let Some(error) = error {
                self.shared.record_error(error);
            }
        }
    }
}

impl Drop for TakeWriter {
    fn drop(&mut self) {
        self.close();
    }
}

fn run(
    file: File,
    sample_rate: u32,
    receiver: Receiver<TakeChunk>,
    shared: &Shared,
) -> Result<RawSignalSummary, &'static str> {
    let mut out = BufWriter::with_capacity(1 << 16, file);
    let mut frames = 0u64;
    let mut bytes = [0u8; 3];
    let mut last_header = Instant::now();
    let mut failed = false;
    let mut signal = Ok(RawSignalSummary::default());
    for chunk in receiver {
        if let Ok(summary) = &mut signal
            && let Err(error) = summary.observe(&chunk.samples, chunk.silence_frames)
        {
            signal = Err(error);
        }
        if failed {
            // Keep draining so the engine never blocks on a dead take.
            continue;
        }
        let counts = (|| {
            let samples = sample_count(chunk.samples.len(), chunk.silence_frames)?;
            let next_frames = frames
                .checked_add(samples / 2)
                .ok_or("recording frame count overflow")?;
            // The raw total includes odd tails that the legacy frame count
            // deliberately floors separately for each public chunk.
            file_bytes(signal.as_ref().map_err(|error| *error)?.sample_count)?;
            let reported_bytes = file_bytes(
                next_frames
                    .checked_mul(2)
                    .ok_or("recording frame count overflow")?,
            )?;
            Ok::<_, &'static str>((next_frames, reported_bytes))
        })();
        let (next_frames, reported_bytes) = match counts {
            Ok(counts) => counts,
            Err(error) => {
                failed = true;
                // A summary failure is returned and recorded by join_writer.
                if signal.is_ok() {
                    shared.record_error(error);
                }
                continue;
            }
        };
        let mut written = || -> io::Result<()> {
            for sample in &chunk.samples {
                let value = (sample.clamp(-1.0, 1.0) * 8_388_607.0).round() as i32;
                bytes[0] = value as u8;
                bytes[1] = (value >> 8) as u8;
                bytes[2] = (value >> 16) as u8;
                out.write_all(&bytes)?;
            }
            // Fixed storage even for a very large declared silence run.
            // Complete-item accounting and header refresh stay after it.
            let zeros = [0u8; 6 * 1024];
            let mut remaining = chunk.silence_frames;
            while remaining > 0 {
                let count = remaining.min(1024) as usize;
                out.write_all(&zeros[..count * 6])?;
                remaining -= count as u64;
            }
            frames = next_frames;
            if last_header.elapsed() >= HEADER_REFRESH {
                last_header = Instant::now();
                refresh_header(&mut out, sample_rate, frames)?;
            }
            Ok(())
        };
        if let Err(error) = written() {
            failed = true;
            shared.record_error(&error.to_string());
            continue;
        }
        shared.frames.store(frames, Ordering::Relaxed);
        shared.bytes.store(reported_bytes, Ordering::Relaxed);
    }
    if !failed {
        let mut finish = || -> io::Result<()> {
            refresh_header(&mut out, sample_rate, frames)?;
            out.flush()?;
            out.get_ref().sync_all()
        };
        if let Err(error) = finish() {
            shared.record_error(&error.to_string());
        }
    }
    signal
}

fn refresh_header(out: &mut BufWriter<File>, sample_rate: u32, frames: u64) -> io::Result<()> {
    out.flush()?;
    let file = out.get_mut();
    let end = file.stream_position()?;
    file.seek(SeekFrom::Start(0))?;
    write_header(file, sample_rate, frames)?;
    file.seek(SeekFrom::Start(end))?;
    Ok(())
}

fn write_header(file: &mut File, sample_rate: u32, frames: u64) -> io::Result<()> {
    let samples = frames
        .checked_mul(u64::from(CHANNELS))
        .ok_or_else(|| io::Error::other("recording frame count overflow"))?;
    let data_bytes = (file_bytes(samples).map_err(io::Error::other)? - HEADER_BYTES)
        .min(u64::from(u32::MAX - 36));
    let data_bytes = data_bytes as u32;
    write_wave_header(
        file,
        1, // PCM
        CHANNELS,
        sample_rate,
        BYTES_PER_SAMPLE as u16 * 8,
        data_bytes,
    )
}

/// A plain 44-byte canonical RIFF/WAVE header: `fmt ` always written as the
/// basic 16-byte PCM/IEEE-float form (never the WAVE_FORMAT_EXTENSIBLE one),
/// immediately followed by `data`. Every caller - the recorder's own take
/// writer and the general trim rewriter alike - describes a format that
/// fits in this basic form, so there is never a reason to reach for the
/// extensible one on write, even when the source file the trim read from
/// used it.
fn write_wave_header(
    file: &mut File,
    format_tag: u16,
    channels: u16,
    sample_rate: u32,
    bits_per_sample: u16,
    data_bytes: u32,
) -> io::Result<()> {
    let block_align = channels * (bits_per_sample / 8);
    let mut header = Vec::with_capacity(HEADER_BYTES as usize);
    header.extend_from_slice(b"RIFF");
    header.extend_from_slice(&(36 + data_bytes).to_le_bytes());
    header.extend_from_slice(b"WAVE");
    header.extend_from_slice(b"fmt ");
    header.extend_from_slice(&16u32.to_le_bytes());
    header.extend_from_slice(&format_tag.to_le_bytes());
    header.extend_from_slice(&channels.to_le_bytes());
    header.extend_from_slice(&sample_rate.to_le_bytes());
    header.extend_from_slice(&(sample_rate * u32::from(block_align)).to_le_bytes());
    header.extend_from_slice(&block_align.to_le_bytes());
    header.extend_from_slice(&bits_per_sample.to_le_bytes());
    header.extend_from_slice(b"data");
    header.extend_from_slice(&data_bytes.to_le_bytes());
    file.write_all(&header)
}

/// The name a take gets: the set's, the moment it started, `.wav`.
pub fn take_filename(unix_seconds: i64, stem: Option<&Path>) -> String {
    let tape = rustel_runtime::session_log::default_session_filename(unix_seconds, stem);
    let base = tape
        .strip_suffix(rustel_runtime::product::SESSION_FILE_SUFFIX)
        .unwrap_or(&tape);
    format!("{base}.wav")
}

/// How far under full scale a frame has to sit to count as the silence
/// around a take rather than the take itself: -60 dBFS, well under an
/// ordinary room's own noise floor, so a quiet tail is trimmed but a soft
/// passage never is.
const QUIET_THRESHOLD_DBFS: f64 = -60.0;

/// The pad kept before the first loud frame and after the last one, so a
/// transient's own attack or release is never clipped and the cut never
/// clicks. `trim_silence` clamps this to what the take actually has either
/// side of the loud span, for a take shorter than the pad itself.
const TRIM_PAD_MS: u64 = 10;

/// Full scale for the signed 24-bit samples this module writes - see
/// `run`'s `sample.clamp(-1.0, 1.0) * 8_388_607.0`.
const FULL_SCALE_24_BIT: f64 = 8_388_607.0;

fn exceeds_quiet_threshold(peak_abs: f64, full_scale: f64) -> bool {
    peak_abs > full_scale * 10f64.powf(QUIET_THRESHOLD_DBFS / 20.0)
}

/// Sign-extend one little-endian 24-bit sample. The same trick the audio
/// crate's `decode_wav` uses: land the three bytes at the top of a 32-bit
/// word and let an arithmetic right shift carry the sign back down.
fn read_i24(chunk: &[u8]) -> i32 {
    i32::from_le_bytes([0, chunk[0], chunk[1], chunk[2]]) >> 8
}

/// The sample encodings trim understands, resolved from a `fmt ` chunk's
/// format tag and bit depth (including the real tag hiding inside a
/// WAVE_FORMAT_EXTENSIBLE sub-format). Every variant is little-endian.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SampleFormat {
    /// Unsigned, midpoint 128.
    Int8,
    Int16,
    Int24,
    Int32,
    Float32,
}

impl SampleFormat {
    fn from_tag_and_bits(format_tag: u16, bits_per_sample: u16) -> Option<Self> {
        match (format_tag, bits_per_sample) {
            (1, 8) => Some(Self::Int8),
            (1, 16) => Some(Self::Int16),
            (1, 24) => Some(Self::Int24),
            (1, 32) => Some(Self::Int32),
            (3, 32) => Some(Self::Float32),
            _ => None,
        }
    }

    fn format_tag(self) -> u16 {
        match self {
            Self::Float32 => 3,
            _ => 1,
        }
    }

    fn bits_per_sample(self) -> u16 {
        match self {
            Self::Int8 => 8,
            Self::Int16 => 16,
            Self::Int24 => 24,
            Self::Int32 | Self::Float32 => 32,
        }
    }

    fn bytes_per_sample(self) -> usize {
        self.bits_per_sample() as usize / 8
    }

    /// The magnitude that reads as "full scale" for [`exceeds_quiet_threshold`]:
    /// the same signed-integer convention `FULL_SCALE_24_BIT` already used
    /// (`2^(bits-1) - 1`), or unity for float samples.
    fn full_scale(self) -> f64 {
        match self {
            Self::Float32 => 1.0,
            Self::Int24 => FULL_SCALE_24_BIT,
            _ => ((1i64 << (self.bits_per_sample() - 1)) - 1) as f64,
        }
    }

    /// The absolute value of one sample at `chunk`, on the same scale as
    /// [`Self::full_scale`]. `chunk` must be exactly `bytes_per_sample()`
    /// long.
    fn sample_abs(self, chunk: &[u8]) -> f64 {
        match self {
            Self::Int8 => (i32::from(chunk[0]) - 128).unsigned_abs() as f64,
            Self::Int16 => {
                i16::from_le_bytes(chunk.try_into().expect("2 bytes")).unsigned_abs() as f64
            }
            Self::Int24 => read_i24(chunk).unsigned_abs() as f64,
            Self::Int32 => {
                i32::from_le_bytes(chunk.try_into().expect("4 bytes")).unsigned_abs() as f64
            }
            Self::Float32 => {
                f64::from(f32::from_le_bytes(chunk.try_into().expect("4 bytes")).abs())
            }
        }
    }
}

/// One chunk found while walking a RIFF list: its id, and the byte range of
/// its payload (after the 8-byte id+size header, before the pad byte an odd
/// size gets).
struct RiffChunk {
    id: [u8; 4],
    start: usize,
    end: usize,
}

/// Walk every chunk in a RIFF/WAVE file's body, honouring each chunk's own
/// declared size and the pad byte an odd size carries - so a chunk that
/// isn't `fmt ` or `data` (a `LIST`, a `fact`, anything else) is skipped
/// correctly rather than assumed absent or misread as a fixed offset would.
fn riff_chunks(bytes: &[u8]) -> Result<Vec<RiffChunk>, String> {
    if bytes.len() < 12 || &bytes[0..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return Err("not a recognizable wav: missing the RIFF/WAVE header".to_owned());
    }
    let mut chunks = Vec::new();
    let mut pos = 12usize;
    while pos + 8 <= bytes.len() {
        let id: [u8; 4] = bytes[pos..pos + 4].try_into().expect("4 bytes");
        let size =
            u32::from_le_bytes(bytes[pos + 4..pos + 8].try_into().expect("4 bytes")) as usize;
        let start = pos + 8;
        let end = start.saturating_add(size).min(bytes.len());
        chunks.push(RiffChunk { id, start, end });
        // Chunks are padded to an even size; the pad byte itself carries no
        // data and sits outside the chunk's own declared size.
        pos = start + size + (size % 2);
        if pos <= start {
            break;
        }
    }
    Ok(chunks)
}

/// Where a wav file's samples stand, once its header has been read.
struct TakeHeader {
    sample_rate: u32,
    channels: u16,
    format: SampleFormat,
    /// Byte offset of the first PCM sample.
    data_start: usize,
    /// Complete frames only: a header a crash left short of its last
    /// declared byte, or overstating what is actually on disk, is trusted
    /// only as far as whichever of the two agrees with fewer bytes.
    frames: usize,
}

/// A general RIFF/WAVE reader: any chunk order (a `LIST` or `fact` chunk
/// between `fmt ` and `data` is skipped, not assumed absent), a `fmt ` of
/// 16, 18 or 40 (WAVE_FORMAT_EXTENSIBLE) bytes, and PCM integer 8/16/24/32-bit
/// or 32-bit IEEE float samples, mono or more channels. Anything else -
/// compressed formats, unsupported bit depths - is refused by name rather
/// than silently misread. This intentionally drops any other metadata
/// chunk on the file trim then rewrites; preserving it is not required and
/// dropping it keeps the writer simple.
fn read_take_header(bytes: &[u8]) -> Result<TakeHeader, String> {
    let chunks = riff_chunks(bytes)?;
    let fmt_chunk = chunks
        .iter()
        .find(|chunk| &chunk.id == b"fmt ")
        .ok_or_else(|| "this wav has no fmt chunk".to_owned())?;
    let fmt = &bytes[fmt_chunk.start..fmt_chunk.end];
    if fmt.len() != 16 && fmt.len() != 18 && fmt.len() != 40 {
        return Err(format!(
            "this wav's fmt chunk is {} bytes, not 16, 18 or 40",
            fmt.len()
        ));
    }
    let mut format_tag = u16::from_le_bytes(fmt[0..2].try_into().expect("2 bytes"));
    let channels = u16::from_le_bytes(fmt[2..4].try_into().expect("2 bytes"));
    let sample_rate = u32::from_le_bytes(fmt[4..8].try_into().expect("4 bytes"));
    let bits_per_sample = u16::from_le_bytes(fmt[14..16].try_into().expect("2 bytes"));
    if format_tag == 0xFFFE {
        if fmt.len() < 40 {
            return Err(
                "this wav declares WAVE_FORMAT_EXTENSIBLE but its fmt chunk is too short"
                    .to_owned(),
            );
        }
        // The sub-format GUID's first two bytes carry the real format tag
        // (the rest of the GUID is the fixed KSDATAFORMAT_SUBTYPE suffix).
        format_tag = u16::from_le_bytes(fmt[24..26].try_into().expect("2 bytes"));
    }
    if channels == 0 {
        return Err("this wav declares zero channels".to_owned());
    }
    let format = SampleFormat::from_tag_and_bits(format_tag, bits_per_sample).ok_or_else(|| {
        format!(
            "this wav is format tag {format_tag} at {bits_per_sample} bits per sample - trim only reads PCM 8/16/24/32-bit or 32-bit float"
        )
    })?;
    let data_chunk = chunks
        .iter()
        .find(|chunk| &chunk.id == b"data")
        .ok_or_else(|| "this wav is missing its data chunk".to_owned())?;
    let data_start = data_chunk.start;
    let declared = data_chunk.end - data_chunk.start;
    let available = bytes.len().saturating_sub(data_start);
    let bytes_per_frame = channels as usize * format.bytes_per_sample();
    let frames = declared.min(available) / bytes_per_frame;
    Ok(TakeHeader {
        sample_rate,
        channels,
        format,
        data_start,
        frames,
    })
}

/// What a silence trim removed, in frames: what was cut from each end,
/// not what is left.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Trimmed {
    pub frames_before: u64,
    pub frames_after: u64,
}

/// Cut a finished take's leading and trailing silence, keeping a short pad
/// on each side so a transient is never clipped and the cut never clicks.
///
/// Reads the whole file, finds the first and last frame louder than -60
/// dBFS on either channel, and - when that span is not already the whole
/// take - rewrites the file to hold only the padded span. The write lands
/// in a temporary file beside `path` and is renamed over it, so a crash or
/// a full disk mid-write leaves the original take exactly as it was rather
/// than a half-written one.
///
/// `Ok(None)` means nothing to remove: the take is already tight, and
/// `path` is left completely untouched, byte for byte. An entirely silent
/// take is refused with `Err` rather than written out empty - an empty
/// take is not a shorter recording, it is a recording that did not happen.
pub fn trim_silence(path: &Path) -> Result<Option<Trimmed>, String> {
    let bytes =
        std::fs::read(path).map_err(|error| format!("cannot read {}: {error}", path.display()))?;
    let header = read_take_header(&bytes)?;
    if header.frames == 0 {
        return Ok(None);
    }
    let bytes_per_sample = header.format.bytes_per_sample();
    let bytes_per_frame = header.channels as usize * bytes_per_sample;
    let full_scale = header.format.full_scale();
    let data = &bytes[header.data_start..header.data_start + header.frames * bytes_per_frame];

    let mut first_loud = None;
    let mut last_loud = None;
    for (index, frame) in data.chunks_exact(bytes_per_frame).enumerate() {
        let peak = frame
            .chunks_exact(bytes_per_sample)
            .map(|sample| header.format.sample_abs(sample))
            .fold(0.0_f64, f64::max);
        if exceeds_quiet_threshold(peak, full_scale) {
            first_loud.get_or_insert(index);
            last_loud = Some(index);
        }
    }
    let (Some(first_loud), Some(last_loud)) = (first_loud, last_loud) else {
        return Err(format!(
            "this take is silent  - nothing on it rises above {QUIET_THRESHOLD_DBFS} dBFS"
        ));
    };

    let pad_frames = (u64::from(header.sample_rate) * TRIM_PAD_MS / 1000) as usize;
    let start = first_loud.saturating_sub(pad_frames);
    let end = (last_loud + pad_frames).min(header.frames - 1);
    let frames_before = start as u64;
    let frames_after = (header.frames - 1 - end) as u64;
    if frames_before == 0 && frames_after == 0 {
        return Ok(None);
    }

    let kept = &data[start * bytes_per_frame..(end + 1) * bytes_per_frame];
    let kept_bytes = u32::try_from(kept.len())
        .map_err(|_| "the trimmed take is too large for a wav data chunk".to_owned())?;

    let mut tmp_name = path
        .file_name()
        .ok_or_else(|| format!("{} has no file name", path.display()))?
        .to_os_string();
    tmp_name.push(".trim-tmp");
    let tmp_path = path.with_file_name(tmp_name);
    // Byte-preserving: the kept PCM is copied straight through untouched
    // (never decoded to float and re-encoded, which would requantize the
    // player's audio), with a fresh header describing the same format,
    // channel count, rate and bit depth the source had. Any other chunk
    // the source carried (a LIST, a fact chunk, ...) is not carried over -
    // preserving it is not required, and dropping it keeps this writer to
    // one plain, canonical header shape.
    let write = || -> io::Result<()> {
        let mut file = File::create(&tmp_path)?;
        write_wave_header(
            &mut file,
            header.format.format_tag(),
            header.channels,
            header.sample_rate,
            header.format.bits_per_sample(),
            kept_bytes,
        )?;
        file.write_all(kept)?;
        file.flush()?;
        file.sync_all()
    };
    if let Err(error) = write() {
        let _ = std::fs::remove_file(&tmp_path);
        return Err(format!("cannot write the trimmed take: {error}"));
    }
    if let Err(error) = replace_file(&tmp_path, path) {
        let _ = std::fs::remove_file(&tmp_path);
        return Err(format!(
            "cannot replace {} with its trimmed copy: {error}",
            path.display()
        ));
    }
    Ok(Some(Trimmed {
        frames_before,
        frames_after,
    }))
}

#[cfg(not(windows))]
fn replace_file(replacement: &Path, destination: &Path) -> io::Result<()> {
    std::fs::rename(replacement, destination)
}

#[cfg(windows)]
fn replace_file(replacement: &Path, destination: &Path) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::ReplaceFileW;

    // ReplaceFileW documents 1175 (ERROR_UNABLE_TO_REMOVE_REPLACED) as a
    // failure that leaves both files under their original names. Virus
    // scanners and indexers can briefly provoke it immediately after the
    // replacement is flushed, so retry only that state. Other replacement
    // errors can describe a partially moved file and must be returned as-is.
    const ERROR_UNABLE_TO_REMOVE_REPLACED: i32 = 1175;
    const RETRY_DELAYS_MS: [u64; 5] = [10, 20, 40, 80, 160];

    let destination: Vec<u16> = destination
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let replacement: Vec<u16> = replacement
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    // ReplaceFileW preserves the destination as one atomic replacement and,
    // unlike std::fs::rename on Windows, accepts an existing destination.
    for retry_delay_ms in RETRY_DELAYS_MS.into_iter().map(Some).chain([None]) {
        let replaced = unsafe {
            ReplaceFileW(
                destination.as_ptr(),
                replacement.as_ptr(),
                std::ptr::null(),
                0,
                std::ptr::null(),
                std::ptr::null(),
            )
        };
        if replaced != 0 {
            return Ok(());
        }
        let error = io::Error::last_os_error();
        let Some(delay_ms) = retry_delay_ms else {
            return Err(error);
        };
        if error.raw_os_error() != Some(ERROR_UNABLE_TO_REMOVE_REPLACED) {
            return Err(error);
        }
        thread::sleep(Duration::from_millis(delay_ms));
    }
    unreachable!("the final replacement attempt returns")
}

#[cfg(test)]
pub(super) mod test_support {
    use super::*;
    use std::sync::mpsc::RecvTimeoutError;

    pub(crate) struct WriterGate {
        reached: Receiver<()>,
        release: Option<SyncSender<()>>,
    }

    impl WriterGate {
        pub(crate) fn wait_until_held(&self) {
            self.reached
                .recv_timeout(Duration::from_secs(5))
                .expect("writer did not reach its return gate");
        }

        pub(crate) fn release(&mut self) {
            drop(self.release.take());
        }
    }

    impl Drop for WriterGate {
        fn drop(&mut self) {
            self.release();
        }
    }

    pub(crate) fn is_finished(writer: &TakeWriter) -> bool {
        writer.join.as_ref().is_some_and(JoinHandle::is_finished)
    }

    pub(crate) fn writer_at_return_gate(
        path: PathBuf,
        sample_rate: u32,
        read_only: bool,
        panic_on_release: bool,
    ) -> (TakeWriter, WriterGate) {
        let mut file = File::create(&path).unwrap();
        write_header(&mut file, sample_rate, 0).unwrap();
        file.flush().unwrap();
        let file = if read_only {
            drop(file);
            File::open(&path).unwrap()
        } else {
            file
        };
        let shared = Arc::new(Shared {
            frames: AtomicU64::new(0),
            bytes: AtomicU64::new(HEADER_BYTES),
            error: Mutex::new(None),
        });
        let worker = Arc::clone(&shared);
        let (sender, receiver) = sync_channel(QUEUE_CHUNKS);
        let (reached_tx, reached) = sync_channel(1);
        let (release, resume) = sync_channel(0);
        let join = thread::spawn(move || {
            let result = run(file, sample_rate, receiver, &worker);
            let _ = reached_tx.send(());
            match resume.recv_timeout(Duration::from_secs(5)) {
                Err(RecvTimeoutError::Disconnected) => {}
                Err(RecvTimeoutError::Timeout) => return Err("writer return gate timed out"),
                Ok(()) => return Err("writer return gate unexpectedly signalled"),
            }
            assert!(!panic_on_release, "injected writer failure");
            result
        });
        (
            TakeWriter {
                sender: Some(sender),
                join: Some(join),
                shared,
                path,
                sample_rate,
                queued_frames: 0,
                queued_samples: 0,
                final_signal: None,
            },
            WriterGate {
                reached,
                release: Some(release),
            },
        )
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::{WriterGate, writer_at_return_gate};
    use super::*;

    fn queued_writer(capacity: usize) -> (TakeWriter, Receiver<TakeChunk>) {
        let (sender, receiver) = sync_channel(capacity);
        (
            TakeWriter {
                sender: Some(sender),
                join: None,
                shared: Arc::new(Shared {
                    frames: AtomicU64::new(0),
                    bytes: AtomicU64::new(HEADER_BYTES),
                    error: Mutex::new(None),
                }),
                path: PathBuf::from("queued.wav"),
                sample_rate: 48_000,
                queued_frames: 0,
                queued_samples: 0,
                final_signal: None,
            },
            receiver,
        )
    }

    #[test]
    fn padded_items_keep_one_bounded_pcm_allocation_and_whole_item_admission() {
        let (mut writer, receiver) = queued_writer(1);
        let samples = vec![0.25, -0.25];
        let allocation = samples.as_ptr();
        let capacity = samples.capacity();
        let silence = 1u64 << 40;
        assert_eq!(writer.push_padded(samples, silence), Ok(true));
        assert_eq!(writer.queued_frames, silence + 1);
        assert_eq!(writer.queued_samples, 2 * (silence + 1));
        assert_eq!(writer.push_padded(vec![1.0, -1.0], 7), Ok(false));
        assert_eq!(writer.queued_frames, silence + 1);
        let chunk = receiver.try_recv().unwrap();
        assert_eq!(chunk.samples, [0.25, -0.25]);
        assert_eq!(chunk.samples.as_ptr(), allocation);
        assert_eq!(chunk.samples.capacity(), capacity);
        assert_eq!(chunk.silence_frames, silence);
        assert!(
            receiver.try_recv().is_err(),
            "the rejected item is not retried"
        );
        assert_eq!(writer.push_padded(Vec::new(), 3), Ok(true));
        assert_eq!(receiver.try_recv().unwrap().silence_frames, 3);
        drop(receiver);
        assert_eq!(writer.push_padded(vec![0.5, -0.5], 2), Ok(false));
        assert_eq!(writer.queued_frames, silence + 4);
    }

    #[test]
    fn padded_counts_reject_overflow_without_admitting_or_recounting_input() {
        let (mut writer, receiver) = queued_writer(1);
        assert_eq!(
            writer.push_padded(Vec::new(), u64::MAX),
            Err("raw sample count overflow")
        );
        // Samples fit u64, but their PCM24 bytes plus header do not.
        assert_eq!(
            writer.push_padded(Vec::new(), u64::MAX / 6 + 1),
            Err("recording byte count overflow")
        );
        assert_eq!(writer.queued_samples, 0);
        assert_eq!(writer.queued_frames, 0);
        assert!(receiver.try_recv().is_err());
        writer.queued_samples = (u64::MAX - HEADER_BYTES) / 3;
        assert!(!writer.push(vec![0.0]));
        let status = writer.status();
        assert_eq!(
            status.error.as_deref(),
            Some("recording byte count overflow")
        );
        assert!(!writer.push(vec![0.0]));
        assert_eq!(
            writer.status(),
            status,
            "a closed refusal adds no duplicate error"
        );
        writer.shared.record_error("later I/O failure");
        writer.fail("recording byte count overflow");
        assert_eq!(
            writer.status().error.as_deref(),
            Some("recording byte count overflow; later I/O failure")
        );
        *writer.shared.error.lock().unwrap() = None;
        writer.shared.record_error("I/O failure; additional detail");
        writer.shared.record_error("I/O failure; additional detail");
        assert_eq!(
            writer.status().error.as_deref(),
            Some("I/O failure; additional detail")
        );

        let mut summary = RawSignalSummary {
            sample_count: u64::MAX - 1,
            ..RawSignalSummary::default()
        };
        let before = summary;
        assert_eq!(summary.observe(&[], 1), Err("raw sample count overflow"));
        assert_eq!(summary, before);
    }

    #[test]
    fn padded_output_matches_materialized_order_bytes_and_raw_summary() {
        let dir = tempfile::tempdir_in(env!("CARGO_MANIFEST_DIR")).unwrap();
        let chunks = [
            (vec![0.5, -2.0, f32::NAN, f32::INFINITY], 3),
            (Vec::new(), 2),
            (vec![-0.5, 0.0], 0),
        ];
        let mut results = Vec::new();
        for padded in [false, true] {
            let path = dir.path().join(format!("padded-{padded}.wav"));
            let mut writer = TakeWriter::start(path.clone(), 48_000).unwrap();
            for (samples, silence) in &chunks {
                if padded {
                    assert_eq!(writer.push_padded(samples.clone(), *silence), Ok(true));
                } else {
                    let mut materialized = samples.clone();
                    materialized.extend(std::iter::repeat_n(0.0, *silence as usize * 2));
                    assert!(writer.push(materialized));
                }
            }
            let status = writer.finish();
            assert_eq!(status.error, None);
            assert_eq!(status.frames, 8);
            assert_eq!(status.bytes, HEADER_BYTES + 48);
            assert_eq!(
                status.final_signal,
                Some(RawSignalSummary {
                    sample_count: 16,
                    nonfinite_count: 2,
                    finite_peak: 2.0,
                })
            );
            results.push((std::fs::read(path).unwrap(), status.final_signal));
        }
        assert_eq!(results[0], results[1]);
    }

    #[test]
    fn odd_public_chunks_preserve_samples_and_per_item_floor_frames() {
        let dir = tempfile::tempdir_in(env!("CARGO_MANIFEST_DIR")).unwrap();
        let path = dir.path().join("odd.wav");
        let mut writer = TakeWriter::start(path.clone(), 48_000).unwrap();
        assert!(writer.push(vec![0.5, -0.5, 1.0]));
        assert!(writer.push(vec![0.0]));
        let status = writer.finish();
        assert_eq!(status.error, None);
        assert_eq!(
            status.frames, 1,
            "odd tails are not carried across public items"
        );
        assert_eq!(status.bytes, HEADER_BYTES + 6);
        assert_eq!(status.final_signal.unwrap().sample_count, 4);
        let bytes = std::fs::read(path).unwrap();
        assert_eq!(u32::from_le_bytes(bytes[40..44].try_into().unwrap()), 6);
        assert_eq!(
            &bytes[44..],
            &[0, 0, 0x40, 0, 0, 0xc0, 0xff, 0xff, 0x7f, 0, 0, 0]
        );
    }

    #[test]
    fn odd_padded_pcm_matches_materialized_samples_and_floor_frames() {
        let dir = tempfile::tempdir_in(env!("CARGO_MANIFEST_DIR")).unwrap();
        let mut outputs = Vec::new();
        for padded in [false, true] {
            let path = dir.path().join(format!("odd-padded-{padded}.wav"));
            let mut writer = TakeWriter::start(path.clone(), 48_000).unwrap();
            let mut samples = vec![0.5, -0.5, 1.0];
            if padded {
                assert_eq!(writer.push_padded(samples, 2), Ok(true));
            } else {
                samples.extend([0.0; 4]);
                assert!(writer.push(samples));
            }
            let status = writer.finish();
            assert_eq!(status.error, None);
            assert_eq!(status.frames, 3, "floor((3 + 2 * 2) / 2)");
            assert_eq!(status.bytes, HEADER_BYTES + 18);
            assert_eq!(
                status.final_signal,
                Some(RawSignalSummary {
                    sample_count: 7,
                    nonfinite_count: 0,
                    finite_peak: 1.0,
                })
            );
            let bytes = std::fs::read(path).unwrap();
            assert_eq!(bytes.len(), 44 + 21, "the odd tail is still written");
            assert_eq!(u32::from_le_bytes(bytes[40..44].try_into().unwrap()), 18);
            outputs.push(bytes);
        }
        assert_eq!(outputs[0], outputs[1]);
    }

    #[test]
    fn silence_suffix_failure_keeps_whole_received_summary_not_partial_frames() {
        let dir = tempfile::tempdir_in(env!("CARGO_MANIFEST_DIR")).unwrap();
        let path = dir.path().join("silence-error.wav");
        File::create(&path).unwrap();
        let (writer, receiver) = queued_writer(2);
        let first_silence = (1 << 16) / 6 + 1;
        let later_silence = 1u64 << 40;
        writer
            .sender
            .as_ref()
            .unwrap()
            .send(TakeChunk {
                samples: vec![0.25, -0.25],
                silence_frames: first_silence,
            })
            .unwrap();
        writer
            .sender
            .as_ref()
            .unwrap()
            .send(TakeChunk {
                samples: vec![f32::NAN, f32::INFINITY, -2.0, 0.0],
                silence_frames: later_silence,
            })
            .unwrap();
        let shared = Arc::clone(&writer.shared);
        drop(writer);
        // An earlier producer refusal must survive the subsequent real I/O error.
        shared.record_error("recording byte count overflow");
        let summary = run(File::open(&path).unwrap(), 48_000, receiver, &shared).unwrap();
        assert_eq!(
            summary,
            RawSignalSummary {
                sample_count: 6 + 2 * (first_silence + later_silence),
                nonfinite_count: 2,
                finite_peak: 2.0,
            }
        );
        let error = shared.error.lock().unwrap().clone().unwrap();
        assert!(error.starts_with("recording byte count overflow; "));
        assert_eq!(error.matches("recording byte count overflow").count(), 1);
        assert_eq!(shared.frames.load(Ordering::Relaxed), 0);
        assert_eq!(shared.bytes.load(Ordering::Relaxed), HEADER_BYTES);
        assert_eq!(std::fs::metadata(path).unwrap().len(), 0);
    }

    #[test]
    fn checked_header_counts_preserve_the_existing_riff_cap() {
        let dir = tempfile::tempdir_in(env!("CARGO_MANIFEST_DIR")).unwrap();
        let path = dir.path().join("header.wav");
        let mut file = File::create(&path).unwrap();
        write_header(&mut file, 48_000, u64::from(u32::MAX)).unwrap();
        let bytes = std::fs::read(&path).unwrap();
        assert_eq!(bytes.len(), HEADER_BYTES as usize);
        assert_eq!(&bytes[..4], b"RIFF");
        assert_eq!(
            u32::from_le_bytes(bytes[40..44].try_into().unwrap()),
            u32::MAX - 36
        );
        assert!(write_header(&mut file, 48_000, u64::MAX).is_err());
        assert_eq!(std::fs::read(path).unwrap(), bytes);
    }

    struct GatedWriter {
        writer: TakeWriter,
        gate: WriterGate,
    }

    impl GatedWriter {
        fn wait_until_held(&self) {
            self.gate.wait_until_held();
        }

        fn collect(&mut self) -> TakeStatus {
            self.gate.release();
            let deadline = Instant::now() + Duration::from_secs(5);
            loop {
                if let Some(status) = self.writer.try_finish() {
                    return status;
                }
                assert!(Instant::now() < deadline, "writer did not terminate");
                thread::sleep(Duration::from_millis(1));
            }
        }
    }

    impl Drop for GatedWriter {
        fn drop(&mut self) {
            // Release before the writer field's blocking Drop, including
            // unwinding from an assertion made while the gate is held.
            self.gate.release();
        }
    }

    fn gated_writer(path: PathBuf, read_only: bool, panic_on_release: bool) -> GatedWriter {
        let (writer, gate) = writer_at_return_gate(path, 48_000, read_only, panic_on_release);
        GatedWriter { writer, gate }
    }

    #[test]
    fn requested_close_retains_an_unfinished_writer_until_collection() {
        let dir = tempfile::tempdir_in(env!("CARGO_MANIFEST_DIR")).unwrap();
        let path = dir.path().join("polled.wav");
        let mut held = gated_writer(path.clone(), false, false);
        assert_eq!(
            held.writer.try_finish(),
            None,
            "open writers are not collected"
        );
        assert!(held.writer.push(vec![0.0, 0.0, 0.5, -0.5]));
        held.writer.request_close();
        held.writer.request_close();
        held.wait_until_held();
        assert!(!held.writer.push(vec![1.0, -1.0]));
        for _ in 0..2 {
            assert_eq!(held.writer.try_finish(), None);
            assert_eq!(held.writer.status().final_signal, None);
            assert!(!held.writer.join.as_ref().unwrap().is_finished());
        }
        let status = held.collect();
        assert_eq!(status.path, path);
        assert_eq!(status.sample_rate, 48_000);
        assert_eq!(status.frames, 2);
        assert_eq!(status.bytes, HEADER_BYTES + 12);
        assert_eq!(status.error, None);
        assert_eq!(
            status.final_signal,
            Some(RawSignalSummary {
                sample_count: 4,
                nonfinite_count: 0,
                finite_peak: 0.5,
            })
        );
        let bytes = std::fs::read(path).unwrap();
        assert_eq!(bytes.len() as u64, status.bytes);
        assert_eq!(u32::from_le_bytes(bytes[40..44].try_into().unwrap()), 12);
        assert_eq!(held.writer.try_finish(), None, "collection is one-shot");
        held.writer.request_close();
        held.writer.close();
        assert_eq!(held.writer.status(), status);
    }

    #[test]
    fn requested_close_drains_declared_silence_before_one_shot_collection() {
        let dir = tempfile::tempdir_in(env!("CARGO_MANIFEST_DIR")).unwrap();
        let path = dir.path().join("padded-close.wav");
        let mut held = gated_writer(path.clone(), false, false);
        assert_eq!(held.writer.push_padded(Vec::new(), 3), Ok(true));
        held.writer.request_close();
        held.wait_until_held();
        assert_eq!(held.writer.try_finish(), None);
        assert_eq!(held.writer.status().final_signal, None);
        let status = held.collect();
        assert_eq!(status.error, None);
        assert_eq!(status.frames, 3);
        assert_eq!(status.bytes, HEADER_BYTES + 18);
        assert_eq!(
            status.final_signal,
            Some(RawSignalSummary {
                sample_count: 6,
                ..RawSignalSummary::default()
            })
        );
        assert_eq!(&std::fs::read(path).unwrap()[44..], &[0; 18]);
        assert_eq!(held.writer.try_finish(), None);
    }

    #[test]
    fn polled_io_failure_keeps_the_received_signal() {
        let dir = tempfile::tempdir_in(env!("CARGO_MANIFEST_DIR")).unwrap();
        let mut held = gated_writer(dir.path().join("polled-error.wav"), true, false);
        // Force a write beyond the buffer before the second chunk is drained.
        let first_samples = (1 << 16) / 3 + 1;
        assert!(held.writer.push(vec![0.25; first_samples]));
        assert!(held.writer.push(vec![f32::NAN, f32::INFINITY, -2.0, 0.0]));
        held.writer.request_close();
        held.wait_until_held();
        let error = held.writer.status().error.expect("read-only write failed");
        assert_eq!(held.writer.try_finish(), None);
        let status = held.collect();
        assert_eq!(status.error.as_deref(), Some(error.as_str()));
        assert_eq!(
            status.final_signal,
            Some(RawSignalSummary {
                sample_count: first_samples as u64 + 4,
                nonfinite_count: 2,
                finite_peak: 2.0,
            })
        );
        assert_eq!(held.writer.try_finish(), None);
        held.writer.close();
        assert_eq!(held.writer.status(), status);
    }

    #[test]
    fn polled_panic_preserves_an_existing_io_error_once() {
        let dir = tempfile::tempdir_in(env!("CARGO_MANIFEST_DIR")).unwrap();
        for read_only in [false, true] {
            let mut held = gated_writer(
                dir.path().join(format!("polled-panic-{read_only}.wav")),
                read_only,
                true,
            );
            assert!(held.writer.push(vec![0.5, -0.5]));
            held.writer.request_close();
            held.wait_until_held();
            let previous = held.writer.status().error;
            assert_eq!(previous.is_some(), read_only);
            assert_eq!(held.writer.try_finish(), None);
            let status = held.collect();
            let expected = match previous {
                Some(error) => format!("{error}; recording writer thread panicked"),
                None => "recording writer thread panicked".to_owned(),
            };
            assert_eq!(status.error.as_deref(), Some(expected.as_str()));
            assert_eq!(status.final_signal, None);
            assert_eq!(held.writer.try_finish(), None);
            held.writer.close();
            assert_eq!(held.writer.status(), status);
        }
    }

    #[test]
    fn a_return_gate_releases_the_writer_during_unwinding() {
        let dir = tempfile::tempdir_in(env!("CARGO_MANIFEST_DIR")).unwrap();
        let path = dir.path().join("unwound.wav");
        let mut held = gated_writer(path, false, false);
        let shared = Arc::clone(&held.writer.shared);
        let unwind = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
            held.writer.request_close();
            held.wait_until_held();
            panic!("unwind while the writer is held");
        }));
        let panic = unwind.expect_err("the test closure must unwind");
        assert_eq!(
            panic.downcast_ref::<&str>(),
            Some(&"unwind while the writer is held")
        );
        assert_eq!(*shared.error.lock().unwrap(), None);
    }

    #[test]
    fn a_take_is_a_playable_24_bit_wav_with_its_sizes_in_the_header() {
        let dir = tempfile::tempdir_in(env!("CARGO_MANIFEST_DIR")).unwrap();
        let path = dir.path().join("take.wav");
        let mut writer = TakeWriter::start(path.clone(), 48_000).unwrap();
        assert_eq!(writer.status().final_signal, None);
        assert!(writer.push(vec![0.0, 0.0, 0.5, -0.5]));
        assert!(writer.push(vec![1.0, -1.0]));
        assert_eq!(writer.status().final_signal, None);
        let status = writer.finish();
        assert_eq!(status.frames, 3);
        assert_eq!(status.error, None);
        assert_eq!(status.bytes, 44 + 3 * 6);
        assert_eq!(
            status.final_signal,
            Some(RawSignalSummary {
                sample_count: 6,
                nonfinite_count: 0,
                finite_peak: 1.0,
            })
        );
        let bytes = std::fs::read(&path).unwrap();
        assert_eq!(bytes.len() as u64, status.bytes);
        assert_eq!(&bytes[0..4], b"RIFF");
        assert_eq!(u32::from_le_bytes(bytes[4..8].try_into().unwrap()), 36 + 18);
        assert_eq!(&bytes[8..12], b"WAVE");
        assert_eq!(u16::from_le_bytes(bytes[22..24].try_into().unwrap()), 2);
        assert_eq!(
            u32::from_le_bytes(bytes[24..28].try_into().unwrap()),
            48_000
        );
        assert_eq!(u16::from_le_bytes(bytes[34..36].try_into().unwrap()), 24);
        assert_eq!(&bytes[36..40], b"data");
        assert_eq!(u32::from_le_bytes(bytes[40..44].try_into().unwrap()), 18);
        // Third sample is 0.5: 0x3FFFFF little-endian, then -0.5.
        let third = &bytes[44 + 6..44 + 9];
        let value = i32::from_le_bytes([third[0], third[1], third[2], 0]) << 8 >> 8;
        assert_eq!(value, 4_194_304, "0.5 rounds half away from zero");
        let fourth = &bytes[44 + 9..44 + 12];
        let value = i32::from_le_bytes([fourth[0], fourth[1], fourth[2], 0]) << 8 >> 8;
        assert_eq!(value, -4_194_304, "{value}");
    }

    #[test]
    fn empty_and_silent_takes_have_final_zero_peaks() {
        let dir = tempfile::tempdir_in(env!("CARGO_MANIFEST_DIR")).unwrap();
        for samples in [0, 8] {
            let mut writer =
                TakeWriter::start(dir.path().join(format!("{samples}.wav")), 48_000).unwrap();
            if samples != 0 {
                assert!(writer.push(vec![0.0; samples]));
            }
            assert_eq!(writer.status().final_signal, None);
            let status = writer.finish();
            assert_eq!(status.error, None);
            assert_eq!(
                status.final_signal,
                Some(RawSignalSummary {
                    sample_count: samples as u64,
                    ..RawSignalSummary::default()
                })
            );
        }
    }

    #[test]
    fn raw_summary_keeps_unclipped_peaks_and_nonfinite_samples() {
        let mut nonfinite = RawSignalSummary::default();
        nonfinite
            .observe(&[f32::NAN, f32::INFINITY, f32::NEG_INFINITY], 0)
            .unwrap();
        assert_eq!(
            nonfinite,
            RawSignalSummary {
                sample_count: 3,
                nonfinite_count: 3,
                finite_peak: 0.0,
            }
        );
        let mut single_sample = RawSignalSummary::default();
        single_sample.observe(&[0.25], 0).unwrap();
        assert_eq!(single_sample.sample_count, 1);

        let dir = tempfile::tempdir_in(env!("CARGO_MANIFEST_DIR")).unwrap();
        let path = dir.path().join("raw.wav");
        let mut writer = TakeWriter::start(path.clone(), 48_000).unwrap();
        assert!(writer.push(vec![0.5, -2.0, f32::NAN, f32::INFINITY]));
        assert!(writer.push(vec![f32::NEG_INFINITY, f32::MAX, -0.5, 0.0]));
        let status = writer.finish();
        assert_eq!(status.error, None);
        assert_eq!(
            status.final_signal,
            Some(RawSignalSummary {
                sample_count: 8,
                nonfinite_count: 3,
                finite_peak: f32::MAX,
            })
        );
        let bytes = std::fs::read(path).unwrap();
        // The existing clamp/round/cast maps NaN to zero and infinities to
        // the clipped endpoints. The raw summary must not alter these bytes.
        assert_eq!(
            &bytes[HEADER_BYTES as usize..],
            &[
                0x00, 0x00, 0x40, // 0.5
                0x01, 0x00, 0x80, // -2.0
                0x00, 0x00, 0x00, // NaN
                0xff, 0xff, 0x7f, // +infinity
                0x01, 0x00, 0x80, // -infinity
                0xff, 0xff, 0x7f, // maximum finite f32
                0x00, 0x00, 0xc0, // -0.5
                0x00, 0x00, 0x00, // zero
            ]
        );
    }

    #[test]
    fn raw_summary_rejects_overflow_without_partial_counts() {
        let mut summary = RawSignalSummary {
            sample_count: u64::MAX,
            ..RawSignalSummary::default()
        };
        let before = summary;
        assert_eq!(summary.observe(&[1.0], 0), Err("raw sample count overflow"));
        assert_eq!(summary, before);

        let mut summary = RawSignalSummary {
            nonfinite_count: u64::MAX,
            ..RawSignalSummary::default()
        };
        let before = summary;
        assert_eq!(
            summary.observe(&[f32::NAN], 0),
            Err("raw non-finite sample count overflow")
        );
        assert_eq!(summary, before);
    }

    #[test]
    fn writer_panic_is_reported_without_hiding_an_earlier_error() {
        let dir = tempfile::tempdir_in(env!("CARGO_MANIFEST_DIR")).unwrap();
        for (index, previous) in [None, Some("earlier I/O failure".to_owned())]
            .into_iter()
            .enumerate()
        {
            let mut writer =
                TakeWriter::start(dir.path().join(format!("panic-{index}.wav")), 48_000).unwrap();
            writer.close();
            writer.final_signal = None;
            *writer.shared.error.lock().unwrap() = previous.clone();
            writer.join = Some(thread::spawn(|| panic!("injected writer failure")));
            let status = writer.finish();
            assert_eq!(status.final_signal, None);
            let expected = match previous {
                Some(error) => format!("{error}; recording writer thread panicked"),
                None => "recording writer thread panicked".to_owned(),
            };
            assert_eq!(status.error.as_deref(), Some(expected.as_str()));
        }
    }

    #[test]
    fn summary_overflow_is_reported_without_a_final_signal() {
        let dir = tempfile::tempdir_in(env!("CARGO_MANIFEST_DIR")).unwrap();
        let mut writer = TakeWriter::start(dir.path().join("overflow.wav"), 48_000).unwrap();
        writer.close();
        writer.final_signal = None;
        writer.join = Some(thread::spawn(|| Err("raw sample count overflow")));
        let status = writer.finish();
        assert_eq!(status.final_signal, None);
        assert_eq!(status.error.as_deref(), Some("raw sample count overflow"));
    }

    #[test]
    fn raw_summary_includes_chunks_drained_after_an_io_failure() {
        let dir = tempfile::tempdir_in(env!("CARGO_MANIFEST_DIR")).unwrap();
        let path = dir.path().join("read-only.wav");
        File::create(&path).unwrap();
        let file = File::open(&path).unwrap();
        let shared = Shared {
            frames: AtomicU64::new(0),
            bytes: AtomicU64::new(HEADER_BYTES),
            error: Mutex::new(None),
        };
        let (sender, receiver) = sync_channel(2);
        // Two bytes beyond the writer buffer: the read-only file fails while
        // writing this chunk, before receiving the next one.
        let first_samples = (1 << 16) / 3 + 1;
        sender
            .send(TakeChunk {
                samples: vec![0.25; first_samples],
                silence_frames: 0,
            })
            .unwrap();
        sender
            .send(TakeChunk {
                samples: vec![f32::NAN, f32::INFINITY, -2.0, 0.0],
                silence_frames: 0,
            })
            .unwrap();
        drop(sender);
        let summary = run(file, 48_000, receiver, &shared).unwrap();
        assert_eq!(
            summary,
            RawSignalSummary {
                sample_count: first_samples as u64 + 4,
                nonfinite_count: 2,
                finite_peak: 2.0,
            }
        );
        assert!(shared.error.lock().unwrap().is_some());
        assert_eq!(shared.frames.load(Ordering::Relaxed), 0);
        assert_eq!(std::fs::metadata(path).unwrap().len(), 0);
    }

    #[test]
    fn a_take_is_named_like_its_tape() {
        let name = take_filename(0, Some(Path::new("/sets/live.strudel")));
        assert_eq!(name, "live-1970-01-01T00-00-00.wav");
    }

    #[test]
    fn quiet_threshold_sits_at_minus_sixty_dbfs_and_i24_sign_extends() {
        assert!(
            !exceeds_quiet_threshold(8_388.0, FULL_SCALE_24_BIT),
            "just under -60 dBFS"
        );
        assert!(
            exceeds_quiet_threshold(8_389.0, FULL_SCALE_24_BIT),
            "just over -60 dBFS"
        );
        assert_eq!(read_i24(&[0x00, 0x00, 0x00]), 0);
        assert_eq!(read_i24(&[0x00, 0x00, 0x40]), 4_194_304, "0.5 full scale");
        assert_eq!(read_i24(&[0x00, 0x00, 0xC0]), -4_194_304, "-0.5 full scale");
    }

    #[test]
    fn trim_silence_removes_padded_lead_and_tail_but_keeps_the_loud_span_exact() {
        let dir = tempfile::tempdir_in(env!("CARGO_MANIFEST_DIR")).unwrap();
        let path = dir.path().join("padded.wav");
        let mut writer = TakeWriter::start(path.clone(), 48_000).unwrap();
        assert_eq!(writer.push_padded(Vec::new(), 2_000), Ok(true));
        let loud: Vec<f32> = std::iter::repeat_n([0.6_f32, -0.6_f32], 1_000)
            .flatten()
            .collect();
        assert!(writer.push(loud));
        assert_eq!(writer.push_padded(Vec::new(), 2_000), Ok(true));
        let status = writer.finish();
        assert_eq!(status.error, None);
        assert_eq!(status.frames, 5_000);

        let original = std::fs::read(&path).unwrap();
        let loud_start = HEADER_BYTES as usize + 2_000 * 6;
        let loud_bytes = original[loud_start..loud_start + 1_000 * 6].to_vec();

        let trimmed = trim_silence(&path)
            .unwrap()
            .expect("2000 frames of silence either side to remove");
        // The 480-frame (10 ms at 48 kHz) pad is kept either side of the
        // loud span, so only 2000 - 480 frames of the silence are cut.
        assert_eq!(trimmed.frames_before, 1_520);
        assert_eq!(trimmed.frames_after, 1_520);

        let after = std::fs::read(&path).unwrap();
        let kept_frames = 5_000 - 1_520 - 1_520;
        assert_eq!(after.len(), HEADER_BYTES as usize + kept_frames * 6);
        let header = read_take_header(&after).unwrap();
        assert_eq!(header.sample_rate, 48_000);
        assert_eq!(header.frames, kept_frames);
        assert_eq!(
            u32::from_le_bytes(after[4..8].try_into().unwrap()),
            36 + (kept_frames * 6) as u32,
            "RIFF size follows the new, shorter body"
        );
        assert_eq!(
            u32::from_le_bytes(after[40..44].try_into().unwrap()),
            (kept_frames * 6) as u32,
            "data chunk size follows it too"
        );
        // The loud span itself moved; its bytes did not.
        let kept_loud_start = HEADER_BYTES as usize + 480 * 6;
        assert_eq!(
            &after[kept_loud_start..kept_loud_start + 1_000 * 6],
            &loud_bytes[..],
            "the kept span is a slice of the original bytes, not a re-encoding"
        );
    }

    #[test]
    fn trim_silence_leaves_an_already_tight_take_byte_identical() {
        let dir = tempfile::tempdir_in(env!("CARGO_MANIFEST_DIR")).unwrap();
        let path = dir.path().join("tight.wav");
        let mut writer = TakeWriter::start(path.clone(), 48_000).unwrap();
        let loud: Vec<f32> = std::iter::repeat_n([0.5_f32, -0.5_f32], 200)
            .flatten()
            .collect();
        assert!(writer.push(loud));
        let status = writer.finish();
        assert_eq!(status.error, None);

        let before = std::fs::read(&path).unwrap();
        assert_eq!(
            trim_silence(&path).unwrap(),
            None,
            "loud from the first frame to the last: nothing to remove"
        );
        let after = std::fs::read(&path).unwrap();
        assert_eq!(before, after, "an untouched take is not even rewritten");
    }

    #[test]
    fn trim_silence_refuses_a_silent_take_and_leaves_it_untouched() {
        let dir = tempfile::tempdir_in(env!("CARGO_MANIFEST_DIR")).unwrap();
        let path = dir.path().join("silent.wav");
        let mut writer = TakeWriter::start(path.clone(), 48_000).unwrap();
        assert_eq!(writer.push_padded(Vec::new(), 500), Ok(true));
        let status = writer.finish();
        assert_eq!(status.error, None);

        let before = std::fs::read(&path).unwrap();
        let error = trim_silence(&path).unwrap_err();
        assert!(error.contains("silent"), "{error}");
        let after = std::fs::read(&path).unwrap();
        assert_eq!(
            before, after,
            "a take refused as silent is never written out empty"
        );
    }

    /// One sample at `amplitude` (a fraction of the format's full scale, or
    /// of unity for float) encoded the way a real wav file would hold it.
    fn encode_sample(format: SampleFormat, amplitude: f64) -> Vec<u8> {
        if format == SampleFormat::Float32 {
            return (amplitude as f32).to_le_bytes().to_vec();
        }
        let value = (amplitude * format.full_scale()).round() as i64;
        match format {
            SampleFormat::Int8 => vec![(128 + value) as u8],
            SampleFormat::Int16 => (value as i16).to_le_bytes().to_vec(),
            SampleFormat::Int24 => {
                let bytes = (value as i32).to_le_bytes();
                vec![bytes[0], bytes[1], bytes[2]]
            }
            SampleFormat::Int32 => (value as i32).to_le_bytes().to_vec(),
            SampleFormat::Float32 => unreachable!("handled above"),
        }
    }

    /// A minimal, ordinary wav file: a plain 16-byte `fmt ` chunk straight
    /// followed by `data`, the same shape [`write_wave_header`] emits.
    fn write_test_wav(
        dir: &Path,
        name: &str,
        format: SampleFormat,
        channels: u16,
        sample_rate: u32,
        data: &[u8],
    ) -> PathBuf {
        let path = dir.join(name);
        let mut file = File::create(&path).unwrap();
        write_wave_header(
            &mut file,
            format.format_tag(),
            channels,
            sample_rate,
            format.bits_per_sample(),
            data.len() as u32,
        )
        .unwrap();
        file.write_all(data).unwrap();
        path
    }

    /// A `fmt ` chunk payload of the requested total length: the common
    /// 16-byte prefix, plus whatever extension bytes make up the rest.
    fn fmt_payload(
        format_tag: u16,
        channels: u16,
        sample_rate: u32,
        bits_per_sample: u16,
        extension: &[u8],
    ) -> Vec<u8> {
        let block_align = channels * (bits_per_sample / 8);
        let mut payload = Vec::new();
        payload.extend_from_slice(&format_tag.to_le_bytes());
        payload.extend_from_slice(&channels.to_le_bytes());
        payload.extend_from_slice(&sample_rate.to_le_bytes());
        payload.extend_from_slice(&(sample_rate * u32::from(block_align)).to_le_bytes());
        payload.extend_from_slice(&block_align.to_le_bytes());
        payload.extend_from_slice(&bits_per_sample.to_le_bytes());
        payload.extend_from_slice(extension);
        payload
    }

    /// The 24 extension bytes of a WAVE_FORMAT_EXTENSIBLE `fmt ` chunk:
    /// cbSize, valid bits, channel mask, then a sub-format GUID whose first
    /// two bytes are the real format tag.
    fn extensible_extension(bits_per_sample: u16, real_format_tag: u16) -> Vec<u8> {
        let mut extension = Vec::new();
        extension.extend_from_slice(&22u16.to_le_bytes()); // cbSize
        extension.extend_from_slice(&bits_per_sample.to_le_bytes());
        extension.extend_from_slice(&0u32.to_le_bytes()); // channel mask
        extension.extend_from_slice(&real_format_tag.to_le_bytes());
        extension.extend_from_slice(&[0u8; 14]); // rest of the fixed GUID suffix
        extension
    }

    /// Hand-assembled RIFF/WAVE bytes: whatever `fmt ` payload and extra
    /// chunks the test wants between `fmt ` and `data`, then the given PCM.
    /// Exercises the general chunk walker directly, independent of
    /// [`write_wave_header`], which never emits anything but the plain
    /// 16-byte shape.
    fn build_riff(fmt_payload: &[u8], extra_chunks: &[(&[u8; 4], &[u8])], data: &[u8]) -> Vec<u8> {
        fn push_chunk(body: &mut Vec<u8>, id: &[u8; 4], payload: &[u8]) {
            body.extend_from_slice(id);
            body.extend_from_slice(&(payload.len() as u32).to_le_bytes());
            body.extend_from_slice(payload);
            if payload.len() % 2 == 1 {
                body.push(0);
            }
        }
        let mut body = Vec::new();
        push_chunk(&mut body, b"fmt ", fmt_payload);
        for (id, payload) in extra_chunks {
            push_chunk(&mut body, id, payload);
        }
        push_chunk(&mut body, b"data", data);
        let mut file = Vec::new();
        file.extend_from_slice(b"RIFF");
        file.extend_from_slice(&(4 + body.len() as u32).to_le_bytes());
        file.extend_from_slice(b"WAVE");
        file.extend_from_slice(&body);
        file
    }

    #[test]
    fn trim_silence_round_trips_every_supported_format_and_channel_count() {
        let dir = tempfile::tempdir_in(env!("CARGO_MANIFEST_DIR")).unwrap();
        let sample_rate = 48_000u32;
        let pad_frames = (u64::from(sample_rate) * TRIM_PAD_MS / 1000) as usize;
        let lead_silence = 1_000usize;
        let trail_silence = 1_000usize;
        let loud_frames = 200usize;

        for format in [
            SampleFormat::Int8,
            SampleFormat::Int16,
            SampleFormat::Int24,
            SampleFormat::Int32,
            SampleFormat::Float32,
        ] {
            for channels in [1u16, 2u16] {
                let mut amplitudes = Vec::new();
                amplitudes.extend(std::iter::repeat_n(0.0, lead_silence));
                amplitudes.extend(std::iter::repeat_n(0.6, loud_frames));
                amplitudes.extend(std::iter::repeat_n(0.0, trail_silence));
                let bytes_per_frame = format.bytes_per_sample() * channels as usize;
                let mut data = Vec::with_capacity(amplitudes.len() * bytes_per_frame);
                for &amplitude in &amplitudes {
                    for _ in 0..channels {
                        data.extend(encode_sample(format, amplitude));
                    }
                }
                let name = format!("roundtrip-{format:?}-{channels}ch.wav");
                let path = write_test_wav(dir.path(), &name, format, channels, sample_rate, &data);
                let original = std::fs::read(&path).unwrap();
                let loud_byte_start = HEADER_BYTES as usize + lead_silence * bytes_per_frame;
                let loud_bytes = original
                    [loud_byte_start..loud_byte_start + loud_frames * bytes_per_frame]
                    .to_vec();

                let trimmed = trim_silence(&path)
                    .unwrap()
                    .unwrap_or_else(|| panic!("{format:?} {channels}ch: silence to trim"));
                assert_eq!(trimmed.frames_before, (lead_silence - pad_frames) as u64);
                assert_eq!(trimmed.frames_after, (trail_silence - pad_frames) as u64);

                let after = std::fs::read(&path).unwrap();
                let header = read_take_header(&after).unwrap();
                assert_eq!(header.sample_rate, sample_rate);
                assert_eq!(header.channels, channels);
                assert_eq!(header.format, format);
                let kept_frames = loud_frames + 2 * pad_frames;
                assert_eq!(header.frames, kept_frames);
                let kept_loud_start = header.data_start + pad_frames * bytes_per_frame;
                assert_eq!(
                    &after[kept_loud_start..kept_loud_start + loud_frames * bytes_per_frame],
                    &loud_bytes[..],
                    "{format:?} {channels}ch: kept span is a slice of the original bytes"
                );
            }
        }
    }

    #[test]
    fn an_18_byte_and_a_40_byte_extensible_fmt_chunk_are_both_accepted() {
        let dir = tempfile::tempdir_in(env!("CARGO_MANIFEST_DIR")).unwrap();
        let data = vec![0u8; 16 * 2 * 4]; // 16 silent stereo 16-bit frames
        let fmt18 = fmt_payload(1, 2, 48_000, 16, &0u16.to_le_bytes());
        let path18 = dir.path().join("fmt18.wav");
        std::fs::write(&path18, build_riff(&fmt18, &[], &data)).unwrap();
        let header18 = read_take_header(&std::fs::read(&path18).unwrap()).unwrap();
        assert_eq!(header18.format, SampleFormat::Int16);
        assert_eq!(header18.channels, 2);

        let fmt40 = fmt_payload(0xFFFE, 2, 48_000, 16, &extensible_extension(16, 1));
        let path40 = dir.path().join("fmt40.wav");
        std::fs::write(&path40, build_riff(&fmt40, &[], &data)).unwrap();
        let header40 = read_take_header(&std::fs::read(&path40).unwrap()).unwrap();
        assert_eq!(header40.format, SampleFormat::Int16);
        assert_eq!(header40.channels, 2);
    }

    #[test]
    fn a_list_chunk_between_fmt_and_data_is_skipped_and_trim_still_works() {
        let dir = tempfile::tempdir_in(env!("CARGO_MANIFEST_DIR")).unwrap();
        let sample_rate = 48_000u32;
        let pad_frames = (u64::from(sample_rate) * TRIM_PAD_MS / 1000) as usize;
        let lead_silence = 1_000usize;
        let loud_frames = 200usize;
        let mut data = Vec::new();
        for _ in 0..lead_silence {
            data.extend([0i16, 0i16].iter().flat_map(|s| s.to_le_bytes()));
        }
        for _ in 0..loud_frames {
            data.extend([20_000i16, -20_000i16].iter().flat_map(|s| s.to_le_bytes()));
        }
        for _ in 0..lead_silence {
            data.extend([0i16, 0i16].iter().flat_map(|s| s.to_le_bytes()));
        }
        let fmt16 = fmt_payload(1, 2, sample_rate, 16, &[]);
        let path = dir.path().join("with-list.wav");
        std::fs::write(
            &path,
            build_riff(&fmt16, &[(b"LIST", b"INFOsome metadata")], &data),
        )
        .unwrap();
        let header = read_take_header(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(header.format, SampleFormat::Int16);
        assert_eq!(header.frames, lead_silence * 2 + loud_frames);

        let trimmed = trim_silence(&path)
            .unwrap()
            .expect("silence either side of the LIST-carrying file to trim");
        assert_eq!(trimmed.frames_before, (lead_silence - pad_frames) as u64);
    }

    #[test]
    fn an_odd_sized_chunk_before_data_parses_past_its_pad_byte() {
        let dir = tempfile::tempdir_in(env!("CARGO_MANIFEST_DIR")).unwrap();
        let fmt16 = fmt_payload(1, 1, 48_000, 8, &[]);
        // 3-byte odd chunk: the pad byte after it must not be mistaken for
        // the start of `data`.
        let odd_chunk: &[u8] = &[1, 2, 3];
        let data = vec![128u8; 10]; // silent 8-bit mono frames
        let path = dir.path().join("odd-chunk.wav");
        std::fs::write(&path, build_riff(&fmt16, &[(b"JUNK", odd_chunk)], &data)).unwrap();
        let header = read_take_header(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(header.format, SampleFormat::Int8);
        assert_eq!(header.frames, 10);
        assert_eq!(header.channels, 1);
    }

    #[test]
    fn a_compressed_tag_is_refused_by_name_and_the_file_is_untouched() {
        let dir = tempfile::tempdir_in(env!("CARGO_MANIFEST_DIR")).unwrap();
        // Microsoft ADPCM: a real compressed tag, not a bit depth this
        // module happens not to special-case.
        let fmt_adpcm = fmt_payload(2, 1, 48_000, 4, &0u16.to_le_bytes());
        let path = dir.path().join("adpcm.wav");
        let before = build_riff(&fmt_adpcm, &[], &[0u8; 32]);
        std::fs::write(&path, &before).unwrap();
        let error = trim_silence(&path).unwrap_err();
        assert!(error.contains("format tag 2"), "{error}");
        let after = std::fs::read(&path).unwrap();
        assert_eq!(before, after, "a refused file is left untouched");
    }
}
