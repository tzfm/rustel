//! Bounded decoding for repository-owned PCM sample assets.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

static NEXT_SAMPLE_IDENTITY: AtomicU64 = AtomicU64::new(2);

fn next_sample_identity() -> Result<u64, String> {
    NEXT_SAMPLE_IDENTITY
        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |identity| {
            identity.checked_add(1)
        })
        .map_err(|_| "decoded sample identity exhausted".to_owned())
}

/// The most one sound may hold, whatever the player asks for.
///
/// A runaway guard, and the only part of this that is not the player's:
/// it is what stops a folder holding one enormous file, or `/dev/zero`,
/// or a URL that never ends, from taking the process with it. Nothing
/// above may raise the ceiling past this.
pub const MAX_SAMPLE_PCM_BYTES: usize = 1024 * 1024 * 1024;

/// The most one sound may hold before anybody has said otherwise.
///
/// 48 kHz float stereo is 384 kB a second, so this is about eleven
/// minutes of it, or twenty-three of sixteen-bit stereo. It was 64 MiB,
/// which refused an ordinary four-minute take the browser plays.
pub const DEFAULT_SAMPLE_PCM_BYTES: usize = 256 * 1024 * 1024;

/// The floor the ceiling may be set to: below this the bundled sounds
/// themselves would start being refused.
pub const MIN_SAMPLE_PCM_BYTES: usize = 4 * 1024 * 1024;

/// The ceiling in force. Process-wide because it is a policy about files
/// rather than about any one graph, and because the decoders and the
/// fetcher - which run on loader threads with no engine in reach - are
/// where it has to hold.
static SAMPLE_PCM_CEILING: AtomicUsize = AtomicUsize::new(DEFAULT_SAMPLE_PCM_BYTES);

/// The most one sound may hold right now.
pub fn sample_pcm_ceiling() -> usize {
    SAMPLE_PCM_CEILING.load(Ordering::Relaxed)
}

/// Ask for a new ceiling. Returns what was actually set, which is the ask
/// clamped between [`MIN_SAMPLE_PCM_BYTES`] and [`MAX_SAMPLE_PCM_BYTES`] -
/// a player may raise the ceiling but not remove the guard.
pub fn set_sample_pcm_ceiling(bytes: usize) -> usize {
    let wanted = bytes.clamp(MIN_SAMPLE_PCM_BYTES, MAX_SAMPLE_PCM_BYTES);
    SAMPLE_PCM_CEILING.store(wanted, Ordering::Relaxed);
    wanted
}

/// A byte count as a reader thinks of a file.
pub fn format_sample_bytes(bytes: usize) -> String {
    let mib = bytes as f64 / (1024.0 * 1024.0);
    if mib >= 10.0 {
        format!("{mib:.0} MB")
    } else {
        format!("{mib:.1} MB")
    }
}

/// A sample name whose bytes ship with the native runtime.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum BundledSample {
    Bd,
}

/// Index into a [`SampleBank`]. Id 0 is always the bundled `bd`; the
/// fetch/library layer assigns the rest at load time. The id crosses the
/// live ring (`Copy`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SampleId(pub u32);

pub const BUNDLED_BD_SAMPLE_ID: SampleId = SampleId(0);
/// Every backend reconstructs this same immutable repository-owned body.
pub const BUNDLED_BD_SAMPLE_IDENTITY: u64 = 1;

/// Fixed slot count decided at init: installing never allocates on the
/// audio thread.
pub const SAMPLE_BANK_CAPACITY: usize = 2048;

/// Decoded PCM the render loop reads. Slots are fixed at construction;
/// installing writes a pointer and HANDS BACK whatever it displaced so the
/// producer side can free it - the audio thread never allocates or frees.
pub struct SampleBank {
    slots: Vec<Option<Box<DecodedSample>>>,
}

impl SampleBank {
    pub fn empty() -> Self {
        let mut slots: Vec<Option<Box<DecodedSample>>> = Vec::with_capacity(SAMPLE_BANK_CAPACITY);
        slots.resize_with(SAMPLE_BANK_CAPACITY, || None);
        Self { slots }
    }

    pub fn with_bundled_bd() -> Result<Self, String> {
        let mut slots: Vec<Option<Box<DecodedSample>>> = Vec::with_capacity(SAMPLE_BANK_CAPACITY);
        slots.resize_with(SAMPLE_BANK_CAPACITY, || None);
        slots[BUNDLED_BD_SAMPLE_ID.0 as usize] = Some(Box::new(bundled_sample(BundledSample::Bd)?));
        Ok(Self { slots })
    }

    #[inline]
    pub fn get(&self, id: SampleId) -> Option<&DecodedSample> {
        self.slots.get(id.0 as usize)?.as_deref()
    }

    /// Install (or replace) a slot, returning the displaced sample so the
    /// CALLER frees it. Out-of-range ids return the box untouched.
    pub fn install(
        &mut self,
        id: SampleId,
        sample: Box<DecodedSample>,
    ) -> Result<Option<Box<DecodedSample>>, Box<DecodedSample>> {
        match self.slots.get_mut(id.0 as usize) {
            Some(slot) => Ok(slot.replace(sample)),
            None => Err(sample),
        }
    }

    /// Empty a slot, returning whatever it held. The bundled `bd` cannot
    /// be cleared: every backend guarantees that id.
    pub fn clear(&mut self, id: SampleId) -> Option<Box<DecodedSample>> {
        if id == BUNDLED_BD_SAMPLE_ID {
            return None;
        }
        self.slots.get_mut(id.0 as usize)?.take()
    }
}

impl BundledSample {
    pub const fn duration_secs(self) -> f32 {
        match self {
            Self::Bd => 0.25,
        }
    }
}

/// Plain sample playback data that may cross the live event ring.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SampleControls {
    pub sample: SampleId,
    pub playback_rate: f32,
    pub begin: f32,
    pub end: f32,
    /// Whether the amplitude gate follows the selected slice or the source hap.
    pub hold: SampleHold,
    /// `speed(0)` is a per-hap no-op, not an error.
    pub muted: bool,
    /// Soundfont zones loop between these positions (SECONDS on the decoded
    /// buffer's own timeline) for as long as the voice lives; the gain
    /// envelope, not the loop, ends the note.
    pub loop_secs: Option<(f32, f32)>,
    /// Envelope peak: 1.0 for plain samples; soundfonts render at the
    /// synth family's 0.3 peak.
    pub envelope_peak: f32,
    /// `speed < 0` - begin/end/loop offsets apply in REVERSED coordinates.
    /// The render backend flips only the read index, which is exact.
    pub reversed: bool,
    /// `nudge` - delays the SOURCE start by this many seconds while the
    /// amplitude envelope still runs from the hap onset.
    pub nudge_secs: f32,
    /// `cut` - choke group: this trigger fades the group's previous voice
    /// to silence over 10 ms starting at this trigger's (nudged) start.
    pub cut: Option<f32>,
}

impl SampleControls {
    /// Envelope gate and optional buffer end, in seconds from the onset.
    /// Keep this shared by the renderer and producer-side sample retention.
    pub(crate) fn duration_and_natural_stop(
        &self,
        decoded: &DecodedSample,
        hap_duration_secs: f32,
    ) -> (f32, Option<f32>) {
        let source_frames = decoded.frames() as f64;
        let playback_rate = f64::from(self.playback_rate);
        let natural_duration = source_frames / f64::from(decoded.sample_rate());
        let slice_duration_secs =
            ((f64::from(self.end - self.begin) * natural_duration) / playback_rate) as f32;
        let duration_secs = match self.hold {
            SampleHold::Slice => slice_duration_secs,
            SampleHold::Hap => hap_duration_secs,
        };
        // The source starts `nudge` seconds late, so its natural end shifts
        // by the same amount. Reverse playback uses the same coordinates.
        let natural_stop = ((((1.0 - f64::from(self.begin)) * natural_duration) / playback_rate)
            + f64::from(self.nudge_secs)) as f32;
        // A looping zone plays for as long as the envelope holds it; the
        // buffer end is not a stop.
        let natural_stop = if self.loop_secs.is_some() {
            None
        } else {
            Some(natural_stop)
        };
        (duration_secs, natural_stop)
    }
}

/// Filters and vowels keep rendering after their source stops: 10 ms plus
/// two quanta cover the block the stop lands inside. Add the ring after the
/// minimum of envelope and buffer end, so short samples keep their tails.
pub(crate) fn stop_secs_for(source_stop_secs: f32, rings: bool, sample_rate: f32) -> f32 {
    source_stop_secs
        + if rings {
            0.01 + 256.0 / sample_rate
        } else {
            0.0
        }
}

/// Sample envelope gate selection.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum SampleHold {
    /// With no `clip`, `loop`, or explicit `release`, gate the voice for the
    /// selected sample slice.
    Slice,
    /// An explicit `clip` or `release` keeps the source hap's effective gate.
    Hap,
}

/// How a sample voice reads between decoded PCM frames.
///
/// Linear interpolation is the default.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SampleResamplingMode {
    /// Interpolate between adjacent frames.
    #[default]
    Linear,
    /// Discard the fractional frame position and read the preceding frame.
    Raw,
}

/// Decoded PCM owned outside the real-time callback. Preserves the source's
/// mono or interleaved stereo channels.
#[derive(Clone)]
pub struct DecodedSample {
    identity: u64,
    sample_rate: u32,
    channels: u16,
    // Live device recovery keeps a producer-side reference so decoded assets
    // can be reinstalled after CPAL reopens. Shared immutable PCM makes that
    // reference cheap instead of duplicating multi-megabyte sample bodies.
    pcm: Arc<[f32]>,
}

// The identity is ephemeral ownership evidence, not musical content, a
// diagnostic identifier, or part of any existing content comparison.
impl PartialEq for DecodedSample {
    fn eq(&self, other: &Self) -> bool {
        self.sample_rate == other.sample_rate
            && self.channels == other.channels
            && self.pcm == other.pcm
    }
}

impl std::fmt::Debug for DecodedSample {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("DecodedSample")
            .field("sample_rate", &self.sample_rate)
            .field("channels", &self.channels)
            .field("pcm", &self.pcm)
            .finish()
    }
}

impl DecodedSample {
    /// Assemble from an external decoder (the runtime's mp3 path).
    /// `pcm` is interleaved when stereo; frames-per-channel must divide.
    pub fn from_parts(sample_rate: u32, channels: u16, pcm: Vec<f32>) -> Result<Self, String> {
        if !(1..=2).contains(&channels) {
            return Err(format!(
                "decoded sample must be mono or stereo, got {channels}"
            ));
        }
        if sample_rate == 0 || sample_rate > 384_000 {
            return Err(format!(
                "decoded sample rate {sample_rate}Hz is unsupported"
            ));
        }
        if pcm.is_empty() || !pcm.len().is_multiple_of(usize::from(channels)) {
            return Err("decoded sample must contain complete non-empty frames".to_owned());
        }
        if pcm.len() * 4 > sample_pcm_ceiling() {
            return Err("decoded sample exceeds the size limit".to_owned());
        }
        Ok(Self {
            identity: next_sample_identity()?,
            sample_rate,
            channels,
            pcm: pcm.into(),
        })
    }

    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    /// Immutable native body identity. Clones retain it; a newly decoded or
    /// resampled body gets a new checked identity. The bundled kick is 1.
    pub fn identity(&self) -> u64 {
        self.identity
    }

    /// This buffer at `target_rate`, converted the way `decodeAudioData`
    /// converts a file to the context's rate. An equal rate is a cheap clone:
    /// the PCM is shared, not copied. A conversion whose body would pass the
    /// sample ceiling is refused rather than allocated: the rate a file
    /// declares decides the size, and a failed allocation aborts the process.
    pub fn resampled_to(&self, target_rate: u32) -> Result<Self, String> {
        if target_rate == 0 || target_rate == self.sample_rate {
            return Ok(self.clone());
        }
        let pcm =
            crate::resample::interleaved(&self.pcm, self.channels, self.sample_rate, target_rate)?;
        Self::from_parts(target_rate, self.channels, pcm)
    }

    pub fn channels(&self) -> u16 {
        self.channels
    }

    /// Frames PER CHANNEL.
    pub fn frames(&self) -> usize {
        self.pcm.len() / usize::from(self.channels.max(1))
    }

    /// (left, right) at fractional `position` frames with linear
    /// interpolation toward the next frame; mono duplicates. Past the end:
    /// `None` (the voice renders silence).
    #[inline]
    pub fn stereo_at(&self, position: f64) -> Option<(f32, f32)> {
        let frame = position as usize;
        let t = (position - frame as f64) as f32;
        if self.channels == 2 {
            let frames = self.pcm.len() / 2;
            if frame >= frames {
                return None;
            }
            let i = frame * 2;
            // SAFETY: every DecodedSample contains complete interleaved
            // frames. The frame check proves `i` and `i + 1` are present;
            // the next-frame check proves `i + 2` and `i + 3` are present.
            // Keeping one check per frame avoids four bounds checks in this
            // per-voice, per-sample callback path.
            let (l0, r0) = unsafe { (*self.pcm.get_unchecked(i), *self.pcm.get_unchecked(i + 1)) };
            let (l1, r1) = if frame + 1 < frames {
                // SAFETY: established by the next-frame check above.
                unsafe {
                    (
                        *self.pcm.get_unchecked(i + 2),
                        *self.pcm.get_unchecked(i + 3),
                    )
                }
            } else {
                (l0, r0)
            };
            Some((l0 + (l1 - l0) * t, r0 + (r1 - r0) * t))
        } else {
            if frame >= self.pcm.len() {
                return None;
            }
            // SAFETY: established by the frame check above.
            let a = unsafe { *self.pcm.get_unchecked(frame) };
            let b = if frame + 1 < self.pcm.len() {
                // SAFETY: established by the next-frame check above.
                unsafe { *self.pcm.get_unchecked(frame + 1) }
            } else {
                a
            };
            let value = a + (b - a) * t;
            Some((value, value))
        }
    }

    /// Read a voice frame with the selected interpolation mode.
    #[inline]
    pub fn stereo_at_with_mode(
        &self,
        position: f64,
        mode: SampleResamplingMode,
    ) -> Option<(f32, f32)> {
        match mode {
            SampleResamplingMode::Linear => self.stereo_at(position),
            SampleResamplingMode::Raw => {
                let frame = position as usize;
                if self.channels == 2 {
                    let index = frame.checked_mul(2)?;
                    Some((*self.pcm.get(index)?, *self.pcm.get(index + 1)?))
                } else {
                    let value = *self.pcm.get(frame)?;
                    Some((value, value))
                }
            }
        }
    }

    /// Reversed playback, including the deliberate one-sample lag on every
    /// channel after the first.
    ///
    /// On a reversed STEREO buffer, channel 1 lands one sample late, loses
    /// its final sample, and opens on silence - comb-filtering the mono sum.
    /// Mono buffers are unaffected. Chromium-verified: the lagged right
    /// channel correlates 1.000000 (0.511105 unlagged). Forward playback is
    /// untouched.
    pub fn stereo_at_reversed(&self, read: f64) -> Option<(f32, f32)> {
        self.stereo_at_reversed_with_mode(read, SampleResamplingMode::Linear)
    }

    /// Reversed playback uses the same read mode on both channels and retains
    /// the one-frame delay on the right channel.
    pub fn stereo_at_reversed_with_mode(
        &self,
        read: f64,
        mode: SampleResamplingMode,
    ) -> Option<(f32, f32)> {
        let (left, right) = self.stereo_at_with_mode(read, mode)?;
        if self.channels != 2 {
            return Some((left, right));
        }
        let last = (self.frames().max(1) - 1) as f64;
        let position = last - read;
        let shifted = if position >= 1.0 {
            self.stereo_at_with_mode(read + 1.0, mode)
                .map_or(0.0, |(_, r)| r)
        } else {
            // Ramping out of the opening silence toward the buffer's own last
            // sample, which is where the reversed channel begins. Raw mode
            // drops this fractional ramp as it drops fractional frame reads.
            match mode {
                SampleResamplingMode::Linear => {
                    let first = self.stereo_at(last).map_or(0.0, |(_, r)| r);
                    first * position.max(0.0) as f32
                }
                SampleResamplingMode::Raw => 0.0,
            }
        };
        Some((left, shifted))
    }

    pub fn pcm(&self) -> &[f32] {
        &self.pcm
    }

    /// Decoded body size in bytes. Shared PCM is counted once per
    /// `DecodedSample` handle; the live bank and the producer retain the
    /// same `Arc`.
    pub fn pcm_bytes(&self) -> usize {
        self.pcm.len().saturating_mul(4)
    }
}

fn u16_at(bytes: &[u8], offset: usize) -> Result<u16, String> {
    let value = bytes
        .get(offset..offset + 2)
        .ok_or_else(|| "truncated WAV integer".to_owned())?;
    Ok(u16::from_le_bytes([value[0], value[1]]))
}

fn u32_at(bytes: &[u8], offset: usize) -> Result<u32, String> {
    let value = bytes
        .get(offset..offset + 4)
        .ok_or_else(|| "truncated WAV integer".to_owned())?;
    Ok(u32::from_le_bytes([value[0], value[1], value[2], value[3]]))
}

/// Decode a bounded mono PCM16 RIFF/WAVE file.
pub fn decode_pcm16_mono_wav(bytes: &[u8]) -> Result<DecodedSample, String> {
    decode_pcm16_mono_wav_with_identity(bytes, None)
}

fn decode_pcm16_mono_wav_with_identity(
    bytes: &[u8],
    identity: Option<u64>,
) -> Result<DecodedSample, String> {
    if bytes.len() < 12 || &bytes[..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return Err("sample is not a RIFF/WAVE file".to_owned());
    }
    let declared = usize::try_from(u32_at(bytes, 4)?)
        .map_err(|_| "WAV RIFF size is not addressable".to_owned())?
        .checked_add(8)
        .ok_or_else(|| "WAV RIFF size overflowed".to_owned())?;
    if declared > bytes.len() {
        return Err("WAV RIFF body is truncated".to_owned());
    }

    let mut format = None;
    let mut data = None;
    let mut offset = 12usize;
    while offset < declared {
        let header_end = offset
            .checked_add(8)
            .ok_or_else(|| "WAV chunk offset overflowed".to_owned())?;
        if header_end > declared {
            return Err("WAV chunk header is truncated".to_owned());
        }
        let size = usize::try_from(u32_at(bytes, offset + 4)?)
            .map_err(|_| "WAV chunk is not addressable".to_owned())?;
        let body_start = header_end;
        let body_end = body_start
            .checked_add(size)
            .ok_or_else(|| "WAV chunk size overflowed".to_owned())?;
        if body_end > declared {
            return Err("WAV chunk body is truncated".to_owned());
        }
        match &bytes[offset..offset + 4] {
            b"fmt " if format.is_none() => format = Some(&bytes[body_start..body_end]),
            b"data" if data.is_none() => data = Some(&bytes[body_start..body_end]),
            _ => {}
        }
        offset = body_end
            .checked_add(size & 1)
            .ok_or_else(|| "WAV padding offset overflowed".to_owned())?;
    }

    let format = format.ok_or_else(|| "WAV has no fmt chunk".to_owned())?;
    if format.len() < 16 {
        return Err("WAV fmt chunk is truncated".to_owned());
    }
    let encoding = u16_at(format, 0)?;
    let channels = u16_at(format, 2)?;
    let sample_rate = u32_at(format, 4)?;
    let byte_rate = u32_at(format, 8)?;
    let block_align = u16_at(format, 12)?;
    let bits_per_sample = u16_at(format, 14)?;
    if encoding != 1 || channels != 1 || bits_per_sample != 16 {
        return Err(format!(
            "sample WAV must be mono PCM16, got encoding {encoding}, {channels} channels, {bits_per_sample} bits"
        ));
    }
    if sample_rate == 0 || sample_rate > 384_000 {
        return Err(format!("sample WAV rate {sample_rate}Hz is unsupported"));
    }
    let expected_byte_rate = sample_rate
        .checked_mul(2)
        .ok_or_else(|| "sample WAV byte rate overflowed".to_owned())?;
    if block_align != 2 || byte_rate != expected_byte_rate {
        return Err("sample WAV block alignment or byte rate is inconsistent".to_owned());
    }

    let data = data.ok_or_else(|| "WAV has no data chunk".to_owned())?;
    if data.len() > sample_pcm_ceiling() {
        return Err(format!(
            "this sample is {}, past the {} one sound can hold",
            format_sample_bytes(data.len()),
            format_sample_bytes(sample_pcm_ceiling())
        ));
    }
    if data.is_empty() || data.len() % 2 != 0 {
        return Err("sample WAV PCM body must contain complete non-empty i16 frames".to_owned());
    }
    let mut pcm = Vec::with_capacity(data.len() / 2);
    for bytes in data.as_chunks::<2>().0 {
        pcm.push(f32::from(i16::from_le_bytes([bytes[0], bytes[1]])) / 32768.0);
    }
    Ok(DecodedSample {
        identity: identity.map_or_else(next_sample_identity, Ok)?,
        sample_rate,
        channels: 1,
        pcm: pcm.into(),
    })
}

/// Decode a bounded RIFF/WAVE file for the sample library: integer PCM
/// (16/24/32-bit) or float32. Preserves mono or interleaved stereo channels
/// and the file's sample rate.
pub fn decode_wav(bytes: &[u8]) -> Result<DecodedSample, String> {
    if bytes.len() < 12 || &bytes[..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return Err("sample is not a RIFF/WAVE file".to_owned());
    }
    let declared = usize::try_from(u32_at(bytes, 4)?)
        .map_err(|_| "WAV RIFF size is not addressable".to_owned())?
        .checked_add(8)
        .ok_or_else(|| "WAV RIFF size overflowed".to_owned())?
        .min(bytes.len());

    let mut format = None;
    let mut data = None;
    let mut data_clamped = false;
    let mut offset = 12usize;
    while offset + 8 <= declared {
        let size = usize::try_from(u32_at(bytes, offset + 4)?)
            .map_err(|_| "WAV chunk is not addressable".to_owned())?;
        let body_start = offset + 8;
        let mut body_end = body_start
            .checked_add(size)
            .ok_or_else(|| "WAV chunk size overflowed".to_owned())?;
        let id = &bytes[offset..offset + 4];
        if body_end > declared {
            // A data chunk that overruns the container still plays if some
            // complete frames remain - an incomplete download, or a writer
            // that set the size past EOF. Trailing metadata that overruns
            // after fmt+data is junk: Samplit2 (VCSL) writes an odd-sized
            // 24-bit data chunk and then INFO with no pad byte. A mandatory
            // pad would read INFO as a truncated chunk and refuse a file
            // that browsers play.
            if id == b"data" && data.is_none() {
                body_end = declared;
                data_clamped = true;
            } else if format.is_some() && data.is_some() {
                break;
            } else {
                return Err("WAV chunk body is truncated".to_owned());
            }
        }
        match id {
            b"fmt " if format.is_none() => format = Some(&bytes[body_start..body_end]),
            b"data" if data.is_none() => data = Some(&bytes[body_start..body_end]),
            _ => {}
        }
        offset = body_end;
        // Spec pad is one byte after an odd body, usually 0. Writers that
        // skip it leave the next id at body_end - 'I' of INFO, not 0 - so
        // only consume a zero pad.
        if size & 1 != 0 && offset < declared && bytes[offset] == 0 {
            offset += 1;
        }
    }

    let format = format.ok_or_else(|| "WAV has no fmt chunk".to_owned())?;
    if format.len() < 16 {
        return Err("WAV fmt chunk is truncated".to_owned());
    }
    let mut encoding = u16_at(format, 0)?;
    let channels = u16_at(format, 2)?;
    let sample_rate = u32_at(format, 4)?;
    let bits = u16_at(format, 14)?;
    // WAVE_FORMAT_EXTENSIBLE wraps the real encoding in a GUID whose first
    // two bytes are the plain format tag.
    if encoding == 0xFFFE {
        if format.len() < 26 {
            return Err("WAV extensible fmt chunk is truncated".to_owned());
        }
        encoding = u16_at(format, 24)?;
    }
    if !(1..=2).contains(&channels) {
        return Err(format!(
            "sample WAV must be mono or stereo, got {channels} channels"
        ));
    }
    if sample_rate == 0 || sample_rate > 384_000 {
        return Err(format!("sample WAV rate {sample_rate}Hz is unsupported"));
    }
    let bytes_per = match (encoding, bits) {
        (1, 16) => 2usize,
        (1, 24) => 3,
        (1, 32) => 4,
        (3, 32) => 4,
        _ => {
            return Err(format!(
                "sample WAV must be PCM 16/24/32-bit or float32, got encoding {encoding} at {bits} bits"
            ));
        }
    };
    let mut data = data.ok_or_else(|| "WAV has no data chunk".to_owned())?;
    if data.len() > sample_pcm_ceiling() {
        return Err(format!(
            "this sample is {}, past the {} one sound can hold",
            format_sample_bytes(data.len()),
            format_sample_bytes(sample_pcm_ceiling())
        ));
    }
    let frame_bytes = bytes_per * channels as usize;
    if data_clamped && frame_bytes != 0 {
        let usable = (data.len() / frame_bytes).saturating_mul(frame_bytes);
        data = &data[..usable];
    }
    if data.is_empty() || data.len() % frame_bytes != 0 {
        return Err("sample WAV PCM body must contain complete non-empty frames".to_owned());
    }
    let frames = data.len() / frame_bytes;
    // Reserve samples rather than frames: stereo stores two samples per
    // frame, and the full capacity avoids reallocating during decoding.
    let mut pcm = Vec::with_capacity(frames * channels as usize);
    let read_one = |chunk: &[u8]| -> f32 {
        match (encoding, bits) {
            (1, 16) => f32::from(i16::from_le_bytes([chunk[0], chunk[1]])) / 32768.0,
            (1, 24) => {
                let value = i32::from_le_bytes([0, chunk[0], chunk[1], chunk[2]]) >> 8;
                value as f32 / 8_388_608.0
            }
            (1, 32) => {
                i32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]) as f32
                    / 2_147_483_648.0
            }
            (3, 32) => f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]),
            _ => unreachable!("format matrix checked above"),
        }
    };
    for frame in data.chunks_exact(frame_bytes) {
        if channels == 1 {
            pcm.push(read_one(frame));
        } else {
            pcm.push(read_one(&frame[..bytes_per]));
            pcm.push(read_one(&frame[bytes_per..]));
        }
    }
    Ok(DecodedSample {
        identity: next_sample_identity()?,
        sample_rate,
        channels,
        pcm: pcm.into(),
    })
}

pub(crate) fn bundled_sample(sample: BundledSample) -> Result<DecodedSample, String> {
    match sample {
        BundledSample::Bd => {
            let decoded = decode_pcm16_mono_wav_with_identity(
                include_bytes!("../assets/bd.wav"),
                Some(BUNDLED_BD_SAMPLE_IDENTITY),
            )?;
            if decoded.sample_rate() != 48_000 || decoded.frames() != 12_000 {
                return Err("bundled bd.wav metadata does not match its pinned contract".to_owned());
            }
            Ok(decoded)
        }
    }
}

#[cfg(test)]
mod interpolation_tests {
    use super::{DecodedSample, SampleResamplingMode};
    use std::hint::black_box;
    use std::time::Instant;

    #[inline]
    fn checked_reference(decoded: &DecodedSample, position: f64) -> Option<(f32, f32)> {
        let frame = position as usize;
        let t = (position - frame as f64) as f32;
        if decoded.channels == 2 {
            let i = frame.checked_mul(2)?;
            let l0 = *decoded.pcm.get(i)?;
            let r0 = decoded.pcm.get(i + 1).copied().unwrap_or(l0);
            let l1 = decoded.pcm.get(i + 2).copied().unwrap_or(l0);
            let r1 = decoded.pcm.get(i + 3).copied().unwrap_or(r0);
            Some((l0 + (l1 - l0) * t, r0 + (r1 - r0) * t))
        } else {
            let a = *decoded.pcm.get(frame)?;
            let b = decoded.pcm.get(frame + 1).copied().unwrap_or(a);
            let value = a + (b - a) * t;
            Some((value, value))
        }
    }

    fn assert_same_bits(actual: Option<(f32, f32)>, expected: Option<(f32, f32)>) {
        assert_eq!(
            actual.map(|(left, right)| (left.to_bits(), right.to_bits())),
            expected.map(|(left, right)| (left.to_bits(), right.to_bits()))
        );
    }

    #[test]
    fn interpolation_fast_path_is_bit_exact_to_checked_indexing() {
        let mono = DecodedSample::from_parts(48_000, 1, vec![-0.75, 0.25, 1.0, -0.5])
            .expect("mono buffer");
        let stereo = DecodedSample::from_parts(
            48_000,
            2,
            vec![-0.75, 0.5, 0.25, -1.0, 1.0, 0.125, -0.5, 0.75],
        )
        .expect("stereo buffer");
        let positions = [
            -0.5,
            0.0,
            0.125,
            0.5,
            1.0,
            1.75,
            2.5,
            3.0,
            3.75,
            4.0,
            f64::NAN,
            f64::INFINITY,
        ];

        for decoded in [&mono, &stereo] {
            for position in positions {
                assert_same_bits(
                    decoded.stereo_at(position),
                    checked_reference(decoded, position),
                );
            }
        }
    }

    #[test]
    fn raw_mode_holds_the_preceding_mono_or_stereo_frame() {
        let mono =
            DecodedSample::from_parts(48_000, 1, vec![-0.75, 0.25, 1.0]).expect("mono buffer");
        let stereo = DecodedSample::from_parts(48_000, 2, vec![-0.75, 0.5, 0.25, -1.0, 1.0, 0.125])
            .expect("stereo buffer");

        assert_eq!(
            mono.stereo_at_with_mode(0.99, SampleResamplingMode::Raw),
            Some((-0.75, -0.75))
        );
        assert_eq!(
            stereo.stereo_at_with_mode(1.99, SampleResamplingMode::Raw),
            Some((0.25, -1.0))
        );
        assert_eq!(
            stereo.stereo_at_with_mode(2.99, SampleResamplingMode::Raw),
            Some((1.0, 0.125)),
            "the last frame holds until the buffer ends"
        );
        assert_eq!(
            stereo.stereo_at_with_mode(3.0, SampleResamplingMode::Raw),
            None
        );
        assert_eq!(
            stereo.stereo_at_with_mode(0.5, SampleResamplingMode::Linear),
            stereo.stereo_at(0.5),
            "the linear selection preserves the old reader"
        );
    }

    #[test]
    fn raw_reverse_keeps_the_stereo_delay_without_interpolating_its_silence() {
        let stereo = DecodedSample::from_parts(48_000, 2, vec![1.0, 11.0, 2.0, 12.0, 3.0, 13.0])
            .expect("stereo buffer");

        assert_eq!(
            stereo.stereo_at_reversed_with_mode(1.5, SampleResamplingMode::Raw),
            Some((2.0, 0.0))
        );
        assert_eq!(
            stereo.stereo_at_reversed_with_mode(1.0, SampleResamplingMode::Raw),
            Some((2.0, 13.0))
        );
    }

    #[inline(never)]
    fn time_reads(
        positions: &[f64],
        passes: usize,
        mut read: impl FnMut(f64) -> Option<(f32, f32)>,
    ) -> u128 {
        let started = Instant::now();
        for _ in 0..passes {
            for &position in positions {
                black_box(read(black_box(position)));
            }
        }
        started.elapsed().as_nanos()
    }

    #[test]
    #[ignore = "manual release benchmark"]
    fn interpolation_bounds_check_overhead_report() {
        const FRAMES: usize = 8_192;
        const POSITIONS: usize = 4_096;
        const PASSES: usize = 1_024;
        let mut pcm = Vec::with_capacity(FRAMES * 2);
        for frame in 0..FRAMES {
            let sample = (frame as f32 * 0.000_173).fract() * 2.0 - 1.0;
            pcm.extend_from_slice(&[sample, sample * -0.625]);
        }
        let decoded = DecodedSample::from_parts(48_000, 2, pcm).expect("stereo buffer");
        let positions = (0..POSITIONS)
            .map(|index| ((index * 2_053) % (FRAMES - 1)) as f64 + 0.375)
            .collect::<Vec<_>>();

        // Warm both instruction paths and the same PCM working set before
        // reporting paired A/B/A samples.
        black_box(time_reads(&positions, 64, |position| {
            checked_reference(&decoded, position)
        }));
        black_box(time_reads(&positions, 64, |position| {
            decoded.stereo_at(position)
        }));

        for repetition in 0..15 {
            let checked_a = time_reads(&positions, PASSES, |position| {
                checked_reference(&decoded, position)
            });
            let fast = time_reads(&positions, PASSES, |position| decoded.stereo_at(position));
            let checked_b = time_reads(&positions, PASSES, |position| {
                checked_reference(&decoded, position)
            });
            eprintln!(
                "{{\"benchmark\":\"sample-interpolation-bounds\",\"repetition\":{repetition},\"reads\":{},\"checked_a_nanos\":{checked_a},\"fast_nanos\":{fast},\"checked_b_nanos\":{checked_b}}}",
                POSITIONS * PASSES
            );
        }
    }
}
#[cfg(test)]
mod reversed_tests {
    use super::DecodedSample;

    /// Reversed stereo deliberately lands channel 1 one sample late
    /// (see `stereo_at_reversed`).
    #[test]
    fn a_reversed_stereo_buffer_carries_strudels_one_sample_channel_offset() {
        // Four frames, interleaved: L = 1..4, R = 11..14.
        let decoded =
            DecodedSample::from_parts(48_000, 2, vec![1.0, 11.0, 2.0, 12.0, 3.0, 13.0, 4.0, 14.0])
                .expect("stereo buffer");
        let last = (decoded.frames() - 1) as f64;

        // Left is the plain reversal: 4, 3, 2, 1.
        for (step, expected) in [(0.0, 4.0), (1.0, 3.0), (2.0, 2.0), (3.0, 1.0)] {
            let (left, _) = decoded.stereo_at_reversed(last - step).expect("in range");
            assert_eq!(left, expected, "left at step {step}");
        }

        // Right is that reversal pushed one sample later, so it opens on
        // silence and never reaches the buffer's first frame at all.
        let right_at = |step: f64| decoded.stereo_at_reversed(last - step).expect("in range").1;
        assert_eq!(right_at(0.0), 0.0, "dest[0] is the silence, not a sample");
        for (step, expected) in [(1.0, 14.0), (2.0, 13.0), (3.0, 12.0)] {
            assert_eq!(right_at(step), expected, "right at step {step}");
        }

        // Halfway into that opening step it ramps out of the silence.
        assert_eq!(
            right_at(0.5),
            7.0,
            "expected half of the reversed first sample"
        );

        // A MONO buffer has only channel 0, so no offset applies and both
        // sides stay aligned.
        let mono =
            DecodedSample::from_parts(48_000, 1, vec![1.0, 2.0, 3.0, 4.0]).expect("mono buffer");
        let (left, right) = mono.stereo_at_reversed(3.0).expect("in range");
        assert_eq!((left, right), (4.0, 4.0), "a mono reversal must not shift");
    }
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sample_identity_is_clone_stable_but_not_part_of_content_equality() {
        let first = DecodedSample::from_parts(48_000, 1, vec![0.5; 256]).expect("first body");
        let equal = DecodedSample::from_parts(48_000, 1, vec![0.5; 256]).expect("equal body");
        assert!(first.identity() >= 2);
        assert_ne!(first.identity(), equal.identity());
        assert_eq!(first, equal);
        assert_eq!(first.clone().identity(), first.identity());
        assert_ne!(
            first
                .resampled_to(24_000)
                .expect("resampled body")
                .identity(),
            first.identity()
        );
        assert!(!format!("{first:?}").contains("identity"));
        let bundled = bundled_sample(BundledSample::Bd).expect("bundled body");
        assert_eq!(bundled.identity(), BUNDLED_BD_SAMPLE_IDENTITY);
        assert_eq!(
            bundled_sample(BundledSample::Bd)
                .expect("independent backend")
                .identity(),
            bundled.identity()
        );
    }

    #[test]
    fn bundled_asset_is_exactly_the_repository_generator_output() {
        const SAMPLE_RATE: u32 = 48_000;
        const FRAMES: u32 = 12_000;
        const PCM_BYTES: u32 = FRAMES * 2;

        let mut expected = Vec::with_capacity(44 + PCM_BYTES as usize);
        expected.extend_from_slice(b"RIFF");
        expected.extend_from_slice(&(36 + PCM_BYTES).to_le_bytes());
        expected.extend_from_slice(b"WAVEfmt ");
        expected.extend_from_slice(&16u32.to_le_bytes());
        expected.extend_from_slice(&1u16.to_le_bytes());
        expected.extend_from_slice(&1u16.to_le_bytes());
        expected.extend_from_slice(&SAMPLE_RATE.to_le_bytes());
        expected.extend_from_slice(&(SAMPLE_RATE * 2).to_le_bytes());
        expected.extend_from_slice(&2u16.to_le_bytes());
        expected.extend_from_slice(&16u16.to_le_bytes());
        expected.extend_from_slice(b"data");
        expected.extend_from_slice(&PCM_BYTES.to_le_bytes());

        let mut phase = 0u32;
        for frame in 0..FRAMES {
            let frequency = 150 - (105 * frame) / FRAMES;
            let increment = ((f64::from(frequency) * 4_294_967_296.0 / f64::from(SAMPLE_RATE))
                + 0.5)
                .floor() as u32;
            phase = phase.wrapping_add(increment);
            let cycle = f64::from(phase) / 4_294_967_296.0;
            let triangle = 1.0 - 4.0 * (cycle - 0.5).abs();
            let attack = (f64::from(frame) / 96.0).min(1.0);
            let decay = f64::from(FRAMES - frame) / f64::from(FRAMES);
            // Round half-up - floor(x + 0.5), negative halves included - the
            // asset generator's rounding. Recompute rather than reassert
            // metadata from the same possibly stale asset.
            let sample = (triangle * attack * decay * decay * 0.82 * 32767.0 + 0.5).floor() as i16;
            expected.extend_from_slice(&sample.to_le_bytes());
        }

        assert_eq!(include_bytes!("../assets/bd.wav").as_slice(), expected);
    }

    #[test]
    fn malformed_inputs_are_refused_before_pcm_allocation() {
        let mut truncated = include_bytes!("../assets/bd.wav").to_vec();
        truncated.truncate(43);
        assert!(
            decode_pcm16_mono_wav(&truncated)
                .unwrap_err()
                .contains("truncated")
        );

        let mut wrong_format = include_bytes!("../assets/bd.wav").to_vec();
        wrong_format[22..24].copy_from_slice(&2u16.to_le_bytes());
        assert!(
            decode_pcm16_mono_wav(&wrong_format)
                .unwrap_err()
                .contains("mono PCM16")
        );
    }

    fn le_u16(value: u16) -> [u8; 2] {
        value.to_le_bytes()
    }

    fn le_u32(value: u32) -> [u8; 4] {
        value.to_le_bytes()
    }

    fn wav_with_chunks(sample_rate: u32, bits: u16, data: &[u8], trailing: &[u8]) -> Vec<u8> {
        let block = bits / 8;
        let mut fmt = Vec::new();
        fmt.extend_from_slice(&le_u16(1));
        fmt.extend_from_slice(&le_u16(1));
        fmt.extend_from_slice(&le_u32(sample_rate));
        fmt.extend_from_slice(&le_u32(sample_rate * u32::from(block)));
        fmt.extend_from_slice(&le_u16(block));
        fmt.extend_from_slice(&le_u16(bits));
        let mut body = Vec::from(&b"WAVE"[..]);
        body.extend_from_slice(b"fmt ");
        body.extend_from_slice(&le_u32(fmt.len() as u32));
        body.extend_from_slice(&fmt);
        body.extend_from_slice(b"data");
        body.extend_from_slice(&le_u32(data.len() as u32));
        body.extend_from_slice(data);
        body.extend_from_slice(trailing);
        let mut out = Vec::from(&b"RIFF"[..]);
        out.extend_from_slice(&le_u32((body.len()) as u32));
        out.extend(body);
        out
    }

    #[test]
    fn an_odd_24bit_body_without_a_pad_still_plays_when_info_follows() {
        // VCSL / Samplit2: 24-bit mono, odd data size, INFO immediately
        // after the last sample rather than after a zero pad byte.
        let trailing = {
            let mut info = Vec::from(&b"INFO"[..]);
            info.extend_from_slice(&le_u32(4));
            info.extend_from_slice(b"test");
            info
        };
        let bytes = wav_with_chunks(8_000, 24, &[0x00, 0x00, 0x40], &trailing);
        let decoded = decode_wav(&bytes).expect("odd 24-bit with unpadded INFO");
        assert_eq!(decoded.sample_rate(), 8_000);
        assert_eq!(decoded.channels(), 1);
        assert_eq!(decoded.frames(), 1);
        assert!(decoded.pcm()[0] > 0.0);
    }

    #[test]
    fn a_truncated_trailing_chunk_does_not_refuse_a_complete_data_chunk() {
        let mut bytes = wav_with_chunks(8_000, 16, &[0x00, 0x40], b"");
        bytes.extend_from_slice(b"JUNK");
        bytes.extend_from_slice(&le_u32(1_000_000));
        let riff_size = (bytes.len() - 8) as u32;
        bytes[4..8].copy_from_slice(&le_u32(riff_size));
        let decoded = decode_wav(&bytes).expect("trailing junk after PCM");
        assert_eq!(decoded.frames(), 1);
    }

    #[test]
    fn a_data_chunk_that_overruns_eof_plays_the_complete_frames_it_has() {
        let mut bytes = wav_with_chunks(8_000, 16, &[0x00, 0x40, 0x00, 0x20, 0xFF], b"");
        // Claim two extra bytes that were never written.
        let data_size_at = 12 + 8 + 16 + 4;
        bytes[data_size_at..data_size_at + 4].copy_from_slice(&le_u32(8));
        let riff_size = (bytes.len() - 8) as u32;
        bytes[4..8].copy_from_slice(&le_u32(riff_size));
        let decoded = decode_wav(&bytes).expect("clamped truncated data");
        assert_eq!(decoded.frames(), 2);
    }

    /// A small file that declares 1 Hz passes the decode bound, but 100k
    /// frames at 1 Hz are 4.8 billion frames at 48 kHz. A failed allocation
    /// aborts, so the conversion must return an ordinary refusal.
    #[test]
    fn resampling_a_tiny_file_at_an_absurd_rate_is_refused_not_allocated() {
        let bytes = wav_with_chunks(1, 16, &vec![0u8; 200_000], b"");
        let decoded = decode_wav(&bytes).expect("a 1 Hz file decodes: its own body is tiny");
        assert_eq!(decoded.sample_rate(), 1);
        assert_eq!(decoded.frames(), 100_000);
        let error = decoded.resampled_to(48_000).unwrap_err();
        assert!(error.contains("one sound can hold"), "{error}");
    }

    /// The other half of the guard: an ordinary file at an ordinary rate decodes
    /// and converts as it always has, and the equal-rate path stays a clone.
    #[test]
    fn an_ordinary_file_still_decodes_and_resamples() {
        // A quarter second at 44.1 kHz, well inside every rung of the ceiling
        // before and after the conversion.
        let data: Vec<u8> = (0..11_025)
            .flat_map(|frame| ((frame % 512) as i16).to_le_bytes())
            .collect();
        let bytes = wav_with_chunks(44_100, 16, &data, b"");
        let decoded = decode_wav(&bytes).expect("an ordinary file decodes");
        assert_eq!(decoded.frames(), 11_025);
        let converted = decoded
            .resampled_to(48_000)
            .expect("an ordinary file converts");
        assert_eq!(converted.sample_rate(), 48_000);
        assert_eq!(converted.frames(), 12_000);
        let same = decoded.resampled_to(44_100).expect("the same rate clones");
        assert_eq!(same, decoded);
    }
}
