//! Sample format selection, decoding, and decoder resource guards.

use rustel_audio::{DecodedSample, decode_wav};

use super::Codec;

/// Pick the decoder from the URL's file extension.
///
/// Wav is the fallback rather than an error because that is the format a bank
/// omits an extension for, and a wrong guess surfaces as a decode failure
/// naming the file - better than refusing a sample that could have played.
pub(super) fn codec_for(url: &str) -> Codec {
    // Query and fragment are normal on a fetched URL and are not part of the
    // filename; `kick.wav?v=2` is still a wav.
    let path = url.split(['?', '#']).next().unwrap_or(url);
    match path
        .rsplit('.')
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase()
        .as_str()
    {
        "mp3" => Codec::Mp3,
        // `.oga` is the Xiph-recommended extension for audio-only Ogg and
        // shows up in exported folders.
        "ogg" | "oga" => Codec::Ogg,
        // Video containers are sample containers too: their audio track is
        // the sound the score asks for. `.mov` is QuickTime, which shares the
        // ISO base media layout.
        "mp4" | "m4v" | "mov" => Codec::Mp4,
        // WebM is Matroska's web profile, and Symphonia reads its audio
        // tracks through the same demuxer.
        "mkv" | "webm" => Codec::Matroska,
        _ => Codec::Wav,
    }
}

/// Name a symphonia codec so a decode failure says what the file holds.
///
/// An `.ogg` is a container, not a codec: the same extension carries Vorbis
/// and Opus, and a phone or messaging app writes Opus. Reporting the codec is
/// the difference between "your file is broken" and "convert this one".
fn codec_name(codec: symphonia::core::codecs::CodecType) -> &'static str {
    use symphonia::core::codecs;
    match codec {
        codecs::CODEC_TYPE_VORBIS => "Vorbis",
        codecs::CODEC_TYPE_OPUS => "Opus",
        codecs::CODEC_TYPE_MP3 => "MP3",
        codecs::CODEC_TYPE_FLAC => "FLAC",
        codecs::CODEC_TYPE_AAC => "AAC",
        codecs::CODEC_TYPE_ALAC => "ALAC",
        codecs::CODEC_TYPE_NULL => "unknown codec",
        _ => "unrecognised codec",
    }
}

/// Decode MP3 with gapless trimming so encoder priming and padding do not
/// add silence to one-shot samples.
pub(super) fn decode_mp3(bytes: &[u8]) -> Result<DecodedSample, String> {
    decode_compressed(bytes, "mp3")
}

/// Decode Ogg Vorbis through symphonia. Ogg Opus uses the separate Opus
/// decoder when the `opus` feature is enabled; builds without it refuse Opus.
///
/// Vorbis setup headers are checked in [`decode_compressed_with`] after
/// codec detection and before decoder construction, regardless of the file
/// extension or container used to reach this entry point.
pub(super) fn decode_ogg(bytes: &[u8]) -> Result<DecodedSample, String> {
    decode_compressed(bytes, "ogg")
}

/// Decode one sample and contain a decoder panic.
///
/// This runs on the loader thread, which is the only thread that loads
/// samples. A panic there ends the thread, and every remaining sound then
/// stays at "loading". Decoders parse untrusted input, so a panic becomes the
/// same refusal as any other decode failure, with a message the caller can
/// report.
pub(super) fn decode_guarded(
    url: &str,
    bytes: &[u8],
    context_rate: Option<u32>,
) -> Result<DecodedSample, String> {
    let codec = codec_for(url);
    let decode = || {
        #[cfg(test)]
        panic_hook::panic_if_requested();
        let decoded = match codec {
            Codec::Mp3 => decode_mp3(bytes),
            Codec::Ogg => decode_ogg(bytes),
            Codec::Mp4 => decode_compressed(bytes, "mp4"),
            Codec::Matroska => decode_compressed(bytes, "mkv"),
            Codec::Wav => decode_wav(bytes),
        }?;
        // Convert to the context rate before playback, which interpolates
        // for `speed` alone. Wavetables keep the file's rate because they
        // have no playback context at decode time.
        match context_rate {
            Some(rate) => decoded.resampled_to(rate),
            None => Ok(decoded),
        }
    };
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(decode)) {
        Ok(result) => result,
        Err(payload) => {
            let detail = payload
                .downcast_ref::<&str>()
                .map(|text| (*text).to_owned())
                .or_else(|| payload.downcast_ref::<String>().cloned())
                .unwrap_or_else(|| "panic".to_owned());
            Err(format!("{codec:?} decoder panicked: {detail}"))
        }
    }
}

/// Per-codebook resource limit: 2^24 values, or 64 MiB for an f32 table.
/// Check declared sizes before decoder construction because allocation
/// failure cannot be caught by the decoder's panic boundary.
///
/// Charge `max(entries, entries * dimensions)` so zero-dimensional books
/// still count their per-entry state. This bounds the f32 lookup table;
/// other codebook allocations are only approximated by the entry charge.
const VORBIS_CODEBOOK_VALUES_MAX: u64 = 1 << 24;

/// Shared resource limit across a setup header's codebooks: 2^26 values,
/// or 256 MiB of f32 lookup tables. The decoder retains all of these tables,
/// so individually admitted books must also fit a combined budget.
///
/// As with [`VORBIS_CODEBOOK_VALUES_MAX`], the entry charge is a coarse
/// allowance for other codebook state, not a bound on total decoder memory.
const VORBIS_SETUP_VALUES_TOTAL: u64 = 1 << 26;

/// Check Vorbis codebook resource limits before symphonia allocates them.
///
/// `extra_data` is assembled by symphonia's demuxer: a 30-byte identification
/// header followed by the setup packet. Reading this same buffer avoids
/// duplicating container parsing and checks the sizes the decoder will use.
///
/// Walk every codebook to enforce per-book and shared value limits, check
/// that lookup multiplicands fit the packet, and reject truncated metadata.
/// The resource limits can also reject otherwise valid large codebooks.
/// Unrecognized or missing setup signatures are left to the decoder.
///
/// This is not full header validation: floors, residues, mappings and modes
/// are not inspected. The comment header is parsed during probing, before
/// this guard runs.
pub(super) fn guard_vorbis_setup(extra_data: &[u8]) -> Result<(), String> {
    // `read_ident_header` consumes a fixed 30 bytes; the setup packet is
    // whatever follows. Anything shorter, or a setup packet that does not
    // open with the `0x05 'vorbis'` signature the decoder demands, is not a
    // header this guard judges - the decoder reports its own error.
    const IDENT_HEADER_LEN: usize = 30;
    let Some(setup) = extra_data.get(IDENT_HEADER_LEN..) else {
        return Ok(());
    };
    if setup.len() < 7 || setup[0] != 0x05 || &setup[1..7] != b"vorbis" {
        return Ok(());
    }
    let mut values_left = VORBIS_SETUP_VALUES_TOTAL;
    vorbis_setup_codebooks_within_bounds(setup, &mut values_left)
}

/// Walk codebooks in Vorbis I section 3.2.1 order, charging declared sizes
/// against `values_left` before traversing codeword lengths or lookup data.
/// Both variable-length sections must be skipped exactly to find the next
/// codebook's declaration.
fn vorbis_setup_codebooks_within_bounds(setup: &[u8], values_left: &mut u64) -> Result<(), String> {
    let truncated = || "vorbis setup header ends mid-codebook".to_owned();
    // The caller matched `0x05 'vorbis'`; the codebooks start after it.
    let mut bits = VorbisBits::new(&setup[7..]);
    let codebook_count = bits.read(8).ok_or_else(truncated)? as usize + 1;
    for index in 0..codebook_count {
        if bits.read(24).ok_or_else(truncated)? != 0x564342 {
            // Not "BCV": this is where symphonia's own parse of the header
            // stops too, so the name is the only difference.
            return Err(format!(
                "vorbis setup codebook {index} lost its 0x564342 sync"
            ));
        }
        let dimensions = bits.read(16).ok_or_else(truncated)?;
        let entries = bits.read(24).ok_or_else(truncated)?;
        // Lookup storage grows with entries * dimensions; codeword state
        // also grows with entries when dimensions is zero.
        let table = u64::from(entries) * u64::from(dimensions);
        let charged = table.max(u64::from(entries));
        if charged > VORBIS_CODEBOOK_VALUES_MAX {
            return Err(format!(
                "vorbis setup codebook {index} declares a {charged}-value lookup table, \
                 over the {VORBIS_CODEBOOK_VALUES_MAX}-value cap"
            ));
        }
        if charged > *values_left {
            return Err(format!(
                "vorbis setup codebook {index} brings its header to {charged} lookup \
                 values, over the {VORBIS_SETUP_VALUES_TOTAL}-value budget"
            ));
        }
        *values_left -= charged;

        // Ordered lengths use an initial 5-bit length and runs whose bit
        // width is ilog(entries remaining). Unordered lengths use 5 bits
        // per present entry, with optional presence bits for a sparse list.
        if bits.read(1).ok_or_else(truncated)? == 1 {
            bits.read(5).ok_or_else(truncated)?;
            let mut covered = 0u32;
            while covered < entries {
                let left = entries - covered;
                let run_bits = 32 - left.leading_zeros();
                covered += bits.read(run_bits).ok_or_else(truncated)?;
            }
            if covered > entries {
                return Err(format!(
                    "vorbis setup codebook {index} has length runs past its entry count"
                ));
            }
        } else {
            let sparse = bits.read(1).ok_or_else(truncated)? == 1;
            for _ in 0..entries {
                if !sparse || bits.read(1).ok_or_else(truncated)? == 1 {
                    bits.read(5).ok_or_else(truncated)?;
                }
            }
        }

        match bits.read(4).ok_or_else(truncated)? {
            0 => {}
            lookup @ (1 | 2) => {
                // Minimum value, delta value, value bits, sequence flag:
                // read past them to keep the walk aligned.
                bits.read(32).ok_or_else(truncated)?;
                bits.read(32).ok_or_else(truncated)?;
                let value_bits = u64::from(bits.read(4).ok_or_else(truncated)? + 1);
                bits.read(1).ok_or_else(truncated)?;
                // Type 2 stores entries * dimensions multiplicands; type 1
                // stores floor(entries^(1/dimensions)). Check their packet
                // space separately from the expanded lookup-table budget.
                let count = if lookup == 2 {
                    table
                } else {
                    vorbis_lookup1_values(entries, dimensions as u16)
                };
                let needed = count.saturating_mul(value_bits);
                if needed > bits.remaining_bits() {
                    return Err(format!(
                        "vorbis setup codebook {index} promises {needed} multiplicand \
                         bits its packet does not carry"
                    ));
                }
                bits.skip(needed).ok_or_else(truncated)?;
            }
            _ => {
                return Err(format!(
                    "vorbis setup codebook {index} names a lookup type past 2"
                ));
            }
        }
    }
    Ok(())
}

/// Type-1 multiplicand count: the greatest integer `v` for which
/// `v ^ dimensions <= entries` (Vorbis I section 3.2.1).
/// Binary search avoids floating-point rounding at integer boundaries.
/// Zero dimensions returns a sentinel that cannot fit in a setup packet.
pub(super) fn vorbis_lookup1_values(entries: u32, dimensions: u16) -> u64 {
    let (entries, dimensions) = (u64::from(entries), u64::from(dimensions));
    if dimensions == 0 {
        return u64::MAX;
    }
    if entries == 0 {
        return 0;
    }
    // base^dimensions <= entries, without overflowing on the way.
    let pow_within = |base: u64| -> bool {
        let mut product = 1u64;
        for _ in 0..dimensions {
            product = match product.checked_mul(base) {
                Some(product) if product <= entries => product,
                _ => return false,
            };
        }
        true
    };
    // entries^1 <= entries, so the root never exceeds entries.
    let mut low = 0u64;
    let mut high = entries;
    while low < high {
        let mid = (low + high).div_ceil(2);
        if pow_within(mid) {
            low = mid;
        } else {
            high = mid - 1;
        }
    }
    low
}

/// A least-significant-bit-first bit reader over the tail of one Vorbis
/// header packet.
///
/// Vorbis packs every header integer with its least significant bit first
/// (spec section 3.1), which is also the direction symphonia's `BitReaderRtl`
/// reads - the scan has to consume bits identically or every later field
/// lands on the wrong boundary. Reads past the end of the packet come back
/// `None`, which the codebook walk turns into the same refusal symphonia's
/// reader turns its end-of-stream into.
struct VorbisBits<'a> {
    packet: &'a [u8],
    byte: usize,
    bit: u32,
}

impl<'a> VorbisBits<'a> {
    fn new(packet: &'a [u8]) -> Self {
        VorbisBits {
            packet,
            byte: 0,
            bit: 0,
        }
    }

    /// Read up to 32 bits packed least-significant bit first.
    fn read(&mut self, bits: u32) -> Option<u32> {
        let mut value = 0u32;
        for index in 0..bits {
            let byte = *self.packet.get(self.byte)?;
            value |= u32::from((byte >> self.bit) & 1) << index;
            self.bit += 1;
            if self.bit == 8 {
                self.bit = 0;
                self.byte += 1;
            }
        }
        Some(value)
    }

    /// Bits from the current position to the end of the packet.
    fn remaining_bits(&self) -> u64 {
        (self.packet.len() as u64 - self.byte as u64) * 8 - u64::from(self.bit)
    }

    /// Step over `bits` bits without reading them, failing past the end.
    fn skip(&mut self, bits: u64) -> Option<()> {
        if bits > self.remaining_bits() {
            return None;
        }
        let crossed = u64::from(self.bit) + bits;
        self.byte += (crossed / 8) as usize;
        self.bit = (crossed % 8) as u32;
        Some(())
    }
}

/// Opus decodes at 48 kHz whatever the source rate was; `OpusHead` records the
/// original only so a player can report it.
#[cfg(feature = "opus")]
const OPUS_RATE: u32 = 48_000;

/// Frames per channel in the longest packet Opus allows: 120 ms at 48 kHz.
#[cfg(feature = "opus")]
const OPUS_MAX_FRAME: usize = 5_760;

/// Refuse Opus in a build that left the decoder out.
///
/// Only the web build does, because there `decodeAudioData` decodes the
/// container before the bytes ever reach this crate. If a file gets here
/// anyway the host skipped that step, and saying so is more useful than a
/// codec complaint.
#[cfg(not(feature = "opus"))]
fn decode_opus_packets(
    _format: &mut Box<dyn symphonia::core::formats::FormatReader>,
    _track_id: u32,
    _params: &symphonia::core::codecs::CodecParameters,
) -> Result<DecodedSample, String> {
    Err("this build decodes Opus in the host, not here".into())
}

/// Decode the Opus packets symphonia has demuxed out of an Ogg stream.
///
/// symphonia parses `OpusHead` and hands over whole packets but ships no Opus
/// decoder, so this is the one codec that needs its own. `pre_skip` from the
/// header is the encoder's priming, discarded here the same way Chrome's
/// decodeAudioData discards it - otherwise every one-shot starts a few
/// milliseconds late, which on a drum is audible.
#[cfg(feature = "opus")]
fn decode_opus_packets(
    format: &mut Box<dyn symphonia::core::formats::FormatReader>,
    track_id: u32,
    params: &symphonia::core::codecs::CodecParameters,
) -> Result<DecodedSample, String> {
    let channels = params
        .channels
        .map(|channels| channels.count())
        .unwrap_or(1)
        .max(1);
    if channels > 2 {
        // Beyond stereo the header uses a channel-mapping family that needs a
        // multistream decoder. A surround file in a sample folder is a
        // mistake; say so rather than emitting the wrong two channels.
        return Err(format!("Opus with {channels} channels is not supported"));
    }
    let mut decoder = opuscule::Decoder::new(
        opuscule::SampleRate::Hz48000,
        if channels == 2 {
            opuscule::Channels::Stereo
        } else {
            opuscule::Channels::Mono
        },
    );
    // 120 ms per channel: the longest an Opus packet can carry. The decoder
    // takes the output capacity from this slice, so it has to be the maximum
    // rather than the frame size of the packet in hand.
    let mut frame = vec![0f32; OPUS_MAX_FRAME * channels];

    let mut pcm: Vec<f32> = Vec::new();
    let mut packets = 0usize;
    let mut refused = 0usize;
    // Keep the FIRST packet error. Skipping a bad packet is right, but a file
    // where every packet is refused must not report "no audio" - that reads as
    // an empty file and hides the decoder's actual complaint.
    let mut first_error: Option<String> = None;
    loop {
        let packet = match format.next_packet() {
            Ok(packet) => packet,
            Err(symphonia::core::errors::Error::IoError(error))
                if error.kind() == std::io::ErrorKind::UnexpectedEof =>
            {
                break;
            }
            Err(symphonia::core::errors::Error::ResetRequired) => break,
            Err(error) => return Err(format!("opus read: {error}")),
        };
        if packet.track_id() != track_id {
            continue;
        }
        packets += 1;
        let frames = match decoder.decode(Some(packet.buf()), &mut frame, false) {
            Ok(frames) => frames,
            // A corrupt packet is skipped, like Chrome's decoder. Concealment
            // would invent audio; for a one-shot sample, dropping is honest.
            Err(error) => {
                refused += 1;
                first_error.get_or_insert_with(|| format!("{error:?}"));
                continue;
            }
        };
        pcm.extend_from_slice(&frame[..frames * channels]);
        if pcm.len() * 4 > rustel_audio::sample_pcm_ceiling() {
            return Err("opus exceeds the sample size limit".into());
        }
    }

    if pcm.is_empty() {
        return Err(match first_error {
            Some(error) => format!("opus decode failed on all {packets} packet(s), first: {error}"),
            None => format!("opus decoded to no audio from {packets} packet(s)"),
        });
    }
    // A file that mostly failed is worse than one that failed outright: it
    // plays, with holes. Name it rather than let it sound like a bad edit.
    if refused > packets / 4 {
        return Err(format!(
            "opus decode failed on {refused} of {packets} packets, first: {}",
            first_error.unwrap_or_else(|| "unknown".into())
        ));
    }

    // `delay` is OpusHead's pre_skip, in 48 kHz samples per channel.
    let skip = params.delay.unwrap_or(0) as usize * channels;
    if skip < pcm.len() {
        pcm.drain(..skip);
    } else if skip > 0 {
        // Priming longer than the whole file means there is nothing to play.
        pcm.clear();
    }
    if pcm.is_empty() {
        return Err(format!(
            "opus is shorter than its own {skip}-sample pre-skip"
        ));
    }
    DecodedSample::from_parts(OPUS_RATE, channels as u16, pcm)
}

/// Decode a container symphonia can probe, named by its file extension.
///
/// `enable_gapless` matters for mp3 and is inert elsewhere: Vorbis carries its
/// own trimming in the final page's granule position, which the demuxer
/// applies regardless.
///
/// Decode with symphonia's gapless trimming first. It matches Chrome's
/// `decodeAudioData` alignment, which soundfont loop points rely on. Fall
/// back to a manual trim when the demuxer cannot take the file: a Xing
/// header whose frame count is smaller than its own encoder delay makes
/// symphonia's gapless arithmetic underflow and panic. The fallback reads
/// the same delay and padding and trims them itself, clamped to what the
/// file has.
fn decode_compressed(bytes: &[u8], extension: &str) -> Result<DecodedSample, String> {
    let without_gapless = || {
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            decode_compressed_with(bytes, extension, false)
        }))
        .unwrap_or_else(|_| Err(format!("{extension} decoder panicked").into()))
        .map_err(CompressedFailure::into_message)
    };
    let gapless = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        decode_compressed_with(bytes, extension, true)
    }));
    match gapless {
        Ok(Ok(decoded)) => Ok(decoded),
        // The guard's verdict does not depend on gapless trimming: probing the
        // file a second time would only report the same refusal twice.
        Ok(Err(CompressedFailure::Refused(refusal))) => Err(refusal),
        Ok(Err(CompressedFailure::Failed(gapless_error))) => without_gapless()
            .map_err(|error| format!("{gapless_error}; without gapless trimming: {error}")),
        Err(_) => without_gapless(),
    }
}

/// Why [`decode_compressed_with`] gave up. A header [`guard_vorbis_setup`]
/// refused is refused whatever the trimming, so it skips the non-gapless
/// retry; any other failure may be the gapless arithmetic and earns one.
enum CompressedFailure {
    Refused(String),
    Failed(String),
}

impl CompressedFailure {
    fn into_message(self) -> String {
        match self {
            CompressedFailure::Refused(message) | CompressedFailure::Failed(message) => message,
        }
    }
}

impl From<String> for CompressedFailure {
    fn from(message: String) -> Self {
        CompressedFailure::Failed(message)
    }
}

/// Whether a container track carries audio. Symphonia has no single
/// "audio" flag, but every audio codec it can describe gives the track a
/// rate or a channel layout; a video track has neither.
fn is_audio_track(track: &symphonia::core::formats::Track) -> bool {
    track.codec_params.sample_rate.is_some() || track.codec_params.channels.is_some()
}

/// The track a sample asks for: the container's default when that is
/// audio, else its first audio track.
///
/// A video container's default track is the picture. Refusing it with
/// "unsupported codec: H.264" would be technically true and useless - the
/// sound is in the next track.
fn audio_track<'a>(
    format: &'a dyn symphonia::core::formats::FormatReader,
    extension: &str,
) -> Result<&'a symphonia::core::formats::Track, CompressedFailure> {
    format
        .default_track()
        .filter(|track| is_audio_track(track))
        .or_else(|| format.tracks().iter().find(|track| is_audio_track(track)))
        .ok_or_else(|| format!("{extension} has no audio track").into())
}

/// The container's first video track: one no audio decoder could read. A
/// video track has neither a rate nor a channel layout, which every audio
/// track has. The `video` painters decode this track's packets.
#[cfg(feature = "video")]
pub(crate) fn container_video_track(
    format: &dyn symphonia::core::formats::FormatReader,
) -> Option<&symphonia::core::formats::Track> {
    format.tracks().iter().find(|track| !is_audio_track(track))
}

fn decode_compressed_with(
    bytes: &[u8],
    extension: &str,
    gapless: bool,
) -> Result<DecodedSample, CompressedFailure> {
    use symphonia::core::codecs::DecoderOptions;
    use symphonia::core::formats::FormatOptions;
    use symphonia::core::io::MediaSourceStream;
    use symphonia::core::meta::MetadataOptions;
    use symphonia::core::probe::Hint;

    let stream = MediaSourceStream::new(
        Box::new(std::io::Cursor::new(bytes.to_vec())),
        Default::default(),
    );
    let mut hint = Hint::new();
    hint.with_extension(extension);
    let probed = symphonia::default::get_probe()
        .format(
            &hint,
            stream,
            &FormatOptions {
                enable_gapless: gapless,
                ..Default::default()
            },
            &MetadataOptions::default(),
        )
        .map_err(|error| format!("{extension} probe: {error}"))?;
    let mut format = probed.format;
    let track = audio_track(format.as_ref(), extension)?;
    let track_id = track.id;
    let params = track.codec_params.clone();
    let codec = params.codec;
    // This is the one boundary every container reaches, whatever its
    // extension, before a decoder is built. A Vorbis setup header can size a
    // codebook lookup table so large that `VorbisDecoder::try_new` aborts the
    // process allocating it; guard the exact bytes the decoder will read -
    // the demuxed identification and setup headers - before `make` sees them.
    if codec == symphonia::core::codecs::CODEC_TYPE_VORBIS {
        guard_vorbis_setup(params.extra_data.as_deref().unwrap_or_default())
            .map_err(CompressedFailure::Refused)?;
    }
    // Encoder delay and padding, for the manual trim of the fallback path;
    // symphonia trims them itself when gapless is on.
    let (delay, padding) = if gapless {
        (0usize, 0usize)
    } else {
        (
            params.delay.unwrap_or(0) as usize,
            params.padding.unwrap_or(0) as usize,
        )
    };
    if codec == symphonia::core::codecs::CODEC_TYPE_OPUS {
        return decode_opus_packets(&mut format, track_id, &params).map_err(Into::into);
    }
    let mut decoder = symphonia::default::get_codecs()
        .make(&params, &DecoderOptions::default())
        .map_err(|error| {
            // "unsupported codec" alone sends someone to check their file
            // name. Naming what is actually inside the container tells them
            // what to convert from, and tells us what to add next.
            format!("{extension} decoder: {error} ({})", codec_name(codec))
        })?;

    let mut sample_rate = 0u32;
    let mut channels = 0u16;
    let mut pcm: Vec<f32> = Vec::new();
    loop {
        let packet = match format.next_packet() {
            Ok(packet) => packet,
            Err(symphonia::core::errors::Error::IoError(error))
                if error.kind() == std::io::ErrorKind::UnexpectedEof =>
            {
                break;
            }
            Err(symphonia::core::errors::Error::ResetRequired) => break,
            Err(error) => return Err(format!("{extension} read: {error}").into()),
        };
        if packet.track_id() != track_id {
            continue;
        }
        let decoded = match decoder.decode(&packet) {
            Ok(decoded) => decoded,
            // A corrupt frame is skipped, like Chrome's decoder.
            Err(symphonia::core::errors::Error::DecodeError(_)) => continue,
            Err(error) => return Err(format!("{extension} decode: {error}").into()),
        };
        let spec = *decoded.spec();
        sample_rate = spec.rate;
        channels = spec.channels.count().min(2) as u16;
        let mut buffer =
            symphonia::core::audio::SampleBuffer::<f32>::new(decoded.capacity() as u64, spec);
        buffer.copy_interleaved_ref(decoded);
        let interleaved = buffer.samples();
        let src_channels = spec.channels.count();
        if src_channels <= 2 {
            pcm.extend_from_slice(interleaved);
        } else {
            // Vorbis carries up to 8 channels. Keep the first two rather than
            // downmixing: a surround file in a sample folder is a mistake, and
            // the front pair is the part anyone meant to hear.
            for frame in interleaved.chunks_exact(src_channels) {
                pcm.push(frame[0]);
                pcm.push(frame[1]);
            }
        }
        if pcm.len() * 4 > rustel_audio::sample_pcm_ceiling() {
            return Err(format!("{extension} exceeds the sample size limit").into());
        }
    }
    if pcm.is_empty() {
        return Err(format!("{extension} decoded to no audio").into());
    }
    if delay > 0 || padding > 0 {
        let channels = usize::from(channels.max(1));
        let frames = pcm.len() / channels;
        // Never trim the file away: keep at least one frame, however the
        // header overstates its padding.
        let start = delay.min(frames.saturating_sub(1));
        let end = frames.saturating_sub(padding).max(start + 1);
        pcm = pcm[start * channels..end * channels].to_vec();
    }
    DecodedSample::from_parts(sample_rate, channels.max(1), pcm).map_err(Into::into)
}

#[cfg(test)]
pub(super) mod panic_hook {
    //! A scoped, one-shot decoder failure on the calling test thread only.

    use std::cell::Cell;

    thread_local! {
        static PANIC_NEXT_DECODE: Cell<bool> = const { Cell::new(false) };
    }

    pub(crate) fn with_decoder_panic<T>(run: impl FnOnce() -> T) -> T {
        struct Reset;
        impl Drop for Reset {
            fn drop(&mut self) {
                PANIC_NEXT_DECODE.set(false);
            }
        }

        assert!(!PANIC_NEXT_DECODE.replace(true), "nested decoder failure");
        let _reset = Reset;
        run()
    }

    pub(crate) fn panic_if_requested() {
        assert!(!PANIC_NEXT_DECODE.replace(false), "test decoder failure");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::samples::*;
    #[cfg(test)]
    mod vorbis_guard_tests {
        //! The Vorbis setup-header guard, [`guard_vorbis_setup`]: its codebook
        //! arithmetic checked directly on the buffer symphonia's demuxer hands the
        //! codec, and the production decode paths driven end to end with hand-built
        //! and real Ogg files.

        use super::*;

        /// A least-significant-bit-first bit writer, the packing direction of
        /// Vorbis header fields (spec section 3.1), for building header packets
        /// bit by bit in the fixtures below.
        struct TestBits {
            bytes: Vec<u8>,
            bit: u32,
        }

        impl TestBits {
            fn new() -> Self {
                TestBits {
                    bytes: Vec::new(),
                    bit: 0,
                }
            }

            fn push(&mut self, value: u32, bits: u32) {
                for index in 0..bits {
                    if self.bit == 0 {
                        self.bytes.push(0);
                    }
                    if (value >> index) & 1 == 1 {
                        *self.bytes.last_mut().expect("a byte to set") |= 1 << self.bit;
                    }
                    self.bit = (self.bit + 1) % 8;
                }
            }

            fn into_bytes(self) -> Vec<u8> {
                self.bytes
            }
        }

        /// The Ogg CRC-32 (polynomial 0x04c11db7, init 0, no reflection, no
        /// final xor) of a page whose checksum field is zeroed, written back into
        /// it. symphonia's page reader refuses a page whose CRC does not match,
        /// so a fixture meant to reach the decoder has to carry a real one.
        fn ogg_crc_page(page: &mut [u8]) {
            page[22..26].copy_from_slice(&[0u8; 4]);
            let mut crc = 0u32;
            for &byte in page.iter() {
                crc ^= u32::from(byte) << 24;
                for _ in 0..8 {
                    crc = if crc & 0x8000_0000 != 0 {
                        (crc << 1) ^ 0x04c1_1db7
                    } else {
                        crc << 1
                    };
                }
            }
            page[22..26].copy_from_slice(&crc.to_le_bytes());
        }

        /// The Ogg lacing values for one packet that terminates on its page:
        /// runs of 255 for each full segment, then a final value under 255 (a
        /// trailing 0 when the length is an exact multiple of 255).
        fn lacing_for(len: usize) -> Vec<u8> {
            let mut segments = vec![255u8; len / 255];
            segments.push((len % 255) as u8);
            segments
        }

        /// Build one Ogg page from an explicit segment table and payload, with a
        /// correct CRC. This is the low-level builder the fixtures below layer on:
        /// a segment table lets a page end mid-packet (a trailing 255) so a packet
        /// can be split across pages the way a real muxer splits a large one.
        fn ogg_page_raw(
            payload: &[u8],
            segments: &[u8],
            header_type: u8,
            sequence: u32,
            serial: u32,
            absgp: u64,
        ) -> Vec<u8> {
            assert!(segments.len() <= 255, "a page holds at most 255 segments");
            let mut page = Vec::with_capacity(27 + segments.len() + payload.len());
            page.extend_from_slice(b"OggS");
            page.push(0);
            page.push(header_type);
            page.extend_from_slice(&absgp.to_le_bytes());
            page.extend_from_slice(&serial.to_le_bytes());
            page.extend_from_slice(&sequence.to_le_bytes());
            page.extend_from_slice(&0u32.to_le_bytes());
            page.push(segments.len() as u8);
            page.extend_from_slice(segments);
            page.extend_from_slice(payload);
            ogg_crc_page(&mut page);
            page
        }

        /// One Ogg page carrying whole packets, each terminated with its own
        /// lacing - the ordinary case where no packet spans a page boundary.
        fn ogg_page_packets(
            packets: &[&[u8]],
            header_type: u8,
            sequence: u32,
            serial: u32,
            absgp: u64,
        ) -> Vec<u8> {
            let mut segments = Vec::new();
            let mut payload = Vec::new();
            for packet in packets {
                segments.extend(lacing_for(packet.len()));
                payload.extend_from_slice(packet);
            }
            ogg_page_raw(&payload, &segments, header_type, sequence, serial, absgp)
        }

        /// Split one packet across two pages: the first page ends on a 255 lacing
        /// (the packet continues), the second is flagged a continuation (0x01)
        /// and carries the rest. This is the layout a real muxer produces when a
        /// setup header does not fit on one page, and the case the guard used to
        /// mis-handle by splicing the second page's header into the packet.
        fn split_packet_pages(packet: &[u8], first_sequence: u32, serial: u32) -> Vec<u8> {
            assert!(
                packet.len() > 255,
                "a split needs a packet over one segment"
            );
            let mut pages = ogg_page_raw(&packet[..255], &[255], 0x00, first_sequence, serial, 0);
            let rest = &packet[255..];
            pages.extend(ogg_page_raw(
                rest,
                &lacing_for(rest.len()),
                0x01,
                first_sequence + 1,
                serial,
                0,
            ));
            pages
        }

        /// A well-formed 30-byte Vorbis identification header: mono 44.1 kHz.
        fn vorbis_id_packet() -> Vec<u8> {
            let mut id = vec![0x01];
            id.extend_from_slice(b"vorbis");
            id.extend_from_slice(&0u32.to_le_bytes());
            id.push(1);
            id.extend_from_slice(&44_100u32.to_le_bytes());
            id.extend_from_slice(&0u32.to_le_bytes());
            id.extend_from_slice(&0u32.to_le_bytes());
            id.extend_from_slice(&0u32.to_le_bytes());
            id.push((11 << 4) | 11);
            id.push(1);
            assert_eq!(id.len(), 30, "the mapper requires exactly 30 bytes");
            id
        }

        /// An empty but well-formed Vorbis comment header.
        fn vorbis_comment_packet() -> Vec<u8> {
            let mut comment = vec![0x03];
            comment.extend_from_slice(b"vorbis");
            comment.extend_from_slice(&0u32.to_le_bytes());
            comment.extend_from_slice(&0u32.to_le_bytes());
            comment.push(1);
            comment
        }

        /// Wrap a setup header in the pages of one logical stream, closed by an
        /// end-of-stream page carrying one audio packet. The closing page matters
        /// for a hostile fixture reaching the decoder: a stream that ends right
        /// after its setup header is refused by the demuxer's probe before the
        /// codec parses it, so the attack carries one audio packet to get the
        /// probe one packet further. `split` puts the setup across two pages,
        /// the layout a real muxer uses for a header too big for one page.
        fn vorbis_header_ogg_inner(setup: &[u8], split: bool) -> Vec<u8> {
            let serial = 0x564F_u32;
            let mut ogg = ogg_page_packets(&[&vorbis_id_packet()], 0x02, 0, serial, 0);
            ogg.extend(ogg_page_packets(
                &[&vorbis_comment_packet()],
                0x00,
                1,
                serial,
                0,
            ));
            if split {
                ogg.extend(split_packet_pages(setup, 2, serial));
                ogg.extend(ogg_page_packets(&[&[0x00]], 0x04, 4, serial, 0));
            } else {
                ogg.extend(ogg_page_packets(&[setup], 0x00, 2, serial, 0));
                ogg.extend(ogg_page_packets(&[&[0x00]], 0x04, 3, serial, 0));
            }
            ogg
        }

        fn vorbis_header_ogg(setup: &[u8]) -> Vec<u8> {
            vorbis_header_ogg_inner(setup, false)
        }

        /// The buffer symphonia's Ogg Vorbis mapper hands the codec: the 30-byte
        /// identification header, then the setup packet whole. This is exactly
        /// what [`guard_vorbis_setup`] receives, so the direct-guard tests below
        /// can feed a setup packet without paginating anything.
        fn extra_data(setup: &[u8]) -> Vec<u8> {
            let mut buf = vorbis_id_packet();
            buf.extend_from_slice(setup);
            buf
        }

        /// A setup header packet (`0x05 'vorbis'` then the raw codebook bits).
        fn setup_packet(bits: TestBits) -> Vec<u8> {
            let mut setup = vec![0x05];
            setup.extend_from_slice(b"vorbis");
            setup.extend_from_slice(&bits.into_bytes());
            setup
        }

        /// One codebook whose ordered codeword lengths cover every entry in a
        /// single run - the cheapest way to write a book with a huge entry count,
        /// and what a size-lie fixture uses. `lookup` is the VQ lookup type; when
        /// it is 1 or 2 the table header follows but no multiplicands, so the book
        /// is only ever valid to *allocate* toward.
        fn oversized_codebook(bits: &mut TestBits, dimensions: u32, entries: u32, lookup: u32) {
            bits.push(0x564342, 24); // codebook sync "BCV"
            bits.push(dimensions, 16);
            bits.push(entries, 24);
            bits.push(1, 1); // codeword lengths are ordered
            bits.push(0, 5); // initial length minus one
            let run_bits = 32 - entries.leading_zeros();
            bits.push(entries, run_bits); // one run covers every entry
            bits.push(lookup, 4);
            if lookup != 0 {
                bits.push(0, 32); // minimum value
                bits.push(0, 32); // delta value
                bits.push(0, 4); // value bits minus one
                bits.push(0, 1); // sequence_p
            }
        }

        /// A minimal second codebook to follow the book under test: one dimension,
        /// two entries of length 1, no lookup. If the walk over the book before it
        /// skipped a single bit too many or too few, this book's sync word would not
        /// be where the guard looks for it.
        fn small_codebook(bits: &mut TestBits) {
            bits.push(0x564342, 24); // codebook sync "BCV"
            bits.push(1, 16); // dimensions
            bits.push(2, 24); // entries
            bits.push(0, 1); // unordered
            bits.push(0, 1); // not sparse
            bits.push(0, 5); // length 1
            bits.push(0, 5); // length 1
            bits.push(0, 4); // lookup type 0
        }

        /// Regression: 2^23 entries * 2^15 dimensions wraps u32 in symphonia 0.5.5,
        /// and the decoder then aborts on a huge allocation. The guard refuses the
        /// u64 product. The test calls it directly: the test profile panics first.
        #[test]
        fn the_wrapping_size_lie_is_refused_by_the_setup_guard() {
            let mut bits = TestBits::new();
            bits.push(0, 8); // one codebook
            oversized_codebook(&mut bits, 32_768, 8_388_608, 2); // 2^15 x 2^23
            let error = guard_vorbis_setup(&extra_data(&setup_packet(bits)))
                .expect_err("the wrapping size lie must be refused");
            assert!(error.contains("cap"), "names the value cap: {error}");
        }

        /// A type-2 lookup that promises entries × dimensions multiplicands the
        /// packet does not carry. The decoder reads exactly that many before it
        /// can build the book, so a header promising more table than the packet
        /// holds is a stream no decoder finishes, only allocates toward.
        #[test]
        fn a_type2_multiplicand_promise_the_packet_cannot_back_is_refused() {
            let mut bits = TestBits::new();
            bits.push(0, 8); // one codebook
            bits.push(0x564342, 24); // codebook sync "BCV"
            bits.push(2, 16); // dimensions
            bits.push(4, 24); // entries: eight multiplicands promised
            bits.push(0, 1); // codeword lengths unordered
            bits.push(0, 1); // ...and not sparse
            for _ in 0..4 {
                bits.push(0, 5); // one codeword length per entry
            }
            bits.push(2, 4); // lookup type 2
            bits.push(0, 32); // minimum value
            bits.push(0, 32); // delta value
            bits.push(0, 4); // one bit per multiplicand
            bits.push(0, 1); // sequence_p
            // The packet ends here: not one promised multiplicand is carried.
            let error = guard_vorbis_setup(&extra_data(&setup_packet(bits)))
                .expect_err("a table its packet cannot back must be refused");
            assert!(
                error.contains("multiplicand"),
                "names the missing multiplicands: {error}"
            );
        }

        /// The value cap also covers lookup type 1. The packet carries only
        /// `floor(entries^(1/dims))` multiplicands, but the decoder allocates the
        /// full 2^26-value table.
        #[test]
        fn a_type1_lookup_table_over_the_cap_is_refused() {
            let mut bits = TestBits::new();
            bits.push(0, 8); // one codebook
            oversized_codebook(&mut bits, 8, 8_388_608, 1); // 8 x 2^23 = 2^26 table
            let error = guard_vorbis_setup(&extra_data(&setup_packet(bits)))
                .expect_err("a type-1 table over the cap must be refused");
            assert!(error.contains("cap"), "names the value cap: {error}");
        }

        /// An honest type-1 book whose `floor(entries^(1/dims))` multiplicands are
        /// all present passes: the guard walks the type-1 path, computes the count
        /// by integer root, and lets a real book through.
        #[test]
        fn a_type1_lookup_codebook_with_its_multiplicands_passes() {
            let mut bits = TestBits::new();
            bits.push(0, 8); // one codebook
            bits.push(0x564342, 24); // codebook sync "BCV"
            bits.push(3, 16); // dimensions
            bits.push(8, 24); // entries: floor(8^(1/3)) = 2 multiplicands
            bits.push(0, 1); // unordered
            bits.push(0, 1); // not sparse
            for _ in 0..8 {
                bits.push(0, 5); // one codeword length per entry
            }
            bits.push(1, 4); // lookup type 1
            bits.push(0, 32); // minimum value
            bits.push(0, 32); // delta value
            bits.push(0, 4); // one bit per multiplicand
            bits.push(0, 1); // sequence_p
            bits.push(0, 1); // the two promised multiplicands...
            bits.push(1, 1);
            guard_vorbis_setup(&extra_data(&setup_packet(bits)))
                .expect("an honest type-1 book is not the guard's business");
        }

        /// Books with zero dimensions and 2^24-1 entries each. Their lookup tables
        /// are empty, but symphonia builds entry-sized state for such a book. The
        /// guard charges the entry count against the header budget and refuses the
        /// fifth book. The test checks the guard's charge, not symphonia's.
        #[test]
        fn a_dimension_zero_codebook_flood_is_refused() {
            let mut bits = TestBits::new();
            bits.push(4, 8); // five codebooks
            for _ in 0..5 {
                oversized_codebook(&mut bits, 0, 16_777_215, 0); // dims 0, entries 2^24-1
            }
            let error = guard_vorbis_setup(&extra_data(&setup_packet(bits)))
                .expect_err("a dimension-zero flood must be refused");
            assert!(error.contains("budget"), "names the stream budget: {error}");
        }

        /// A small, honestly-declared type-2 book passes. The guard refuses
        /// impossible tables, not real ones: a two-dimension, four-entry book
        /// whose eight one-bit multiplicands are all present is what a real
        /// encoder writes, and the same walk must let it through.
        #[test]
        fn an_honest_codebook_passes_the_setup_guard() {
            let mut bits = TestBits::new();
            bits.push(0, 8); // one codebook
            bits.push(0x564342, 24); // codebook sync "BCV"
            bits.push(2, 16); // dimensions
            bits.push(4, 24); // entries
            bits.push(0, 1); // unordered
            bits.push(0, 1); // not sparse
            for _ in 0..4 {
                bits.push(0, 5); // one codeword length per entry
            }
            bits.push(2, 4); // lookup type 2: eight multiplicands
            bits.push(0, 32); // minimum value
            bits.push(0, 32); // delta value
            bits.push(0, 4); // one bit per multiplicand
            bits.push(0, 1); // sequence_p
            for value in 0..8 {
                bits.push(u32::from(value % 2 == 0), 1); // the eight promised values
            }
            guard_vorbis_setup(&extra_data(&setup_packet(bits)))
                .expect("an honest codebook is not the guard's business");
        }

        /// A sparsely packed codeword list passes. Most real books mark unused
        /// entries with a single 0 bit, so the guard must walk the sparse branch
        /// (one flag bit per entry, five length bits only when used) to land the
        /// next field on its true boundary.
        #[test]
        fn a_sparse_codeword_list_is_walked() {
            let mut bits = TestBits::new();
            bits.push(0, 8); // one codebook
            bits.push(0x564342, 24); // codebook sync "BCV"
            bits.push(1, 16); // dimensions
            bits.push(4, 24); // entries
            bits.push(0, 1); // unordered
            bits.push(1, 1); // sparse
            for entry in 0..4 {
                let used = entry % 2 == 0;
                bits.push(u32::from(used), 1); // used flag
                if used {
                    bits.push(0, 5); // length only for used entries
                }
            }
            bits.push(0, 4); // lookup type 0: no table
            guard_vorbis_setup(&extra_data(&setup_packet(bits)))
                .expect("a sparse book is not the guard's business");
        }

        /// A complete type-2 book followed by a second book. The guard must skip
        /// exactly entries * dimensions (4 * 2) three-bit multiplicands to find
        /// the sync word of the next book.
        #[test]
        fn a_type2_book_is_stepped_over_to_the_next_codebook() {
            let mut bits = TestBits::new();
            bits.push(1, 8); // two codebooks
            bits.push(0x564342, 24); // codebook sync "BCV"
            bits.push(2, 16); // dimensions
            bits.push(4, 24); // entries
            bits.push(0, 1); // unordered
            bits.push(0, 1); // not sparse
            for _ in 0..4 {
                bits.push(1, 5); // four codewords of length 2: a complete code
            }
            bits.push(2, 4); // lookup type 2
            bits.push(0, 32); // minimum value
            bits.push(0, 32); // delta value
            bits.push(2, 4); // three bits per multiplicand
            bits.push(0, 1); // sequence_p
            for value in 0..8 {
                bits.push(value, 3); // all eight multiplicands
            }
            small_codebook(&mut bits);
            guard_vorbis_setup(&extra_data(&setup_packet(bits)))
                .expect("a type-2 book and the book after it are not the guard's business");
        }

        /// A length-ordered book with several runs, followed by a second book.
        /// Each run count uses `ilog(entries left)` bits (3, 2, 2 here), so the
        /// guard must size each run by the entries that remain.
        #[test]
        fn a_multi_run_ordered_book_is_stepped_over_to_the_next_codebook() {
            let mut bits = TestBits::new();
            bits.push(1, 8); // two codebooks
            bits.push(0x564342, 24); // codebook sync "BCV"
            bits.push(1, 16); // dimensions
            bits.push(4, 24); // entries
            bits.push(1, 1); // codeword lengths are ordered
            bits.push(0, 5); // lengths start at 1
            bits.push(1, 3); // one entry of length 1, in ilog(4) = 3 bits
            bits.push(1, 2); // one of length 2, in ilog(3) = 2 bits
            bits.push(2, 2); // two of length 3, in ilog(2) = 2 bits: 1, 2, 3, 3 is complete
            bits.push(0, 4); // lookup type 0
            small_codebook(&mut bits);
            guard_vorbis_setup(&extra_data(&setup_packet(bits)))
                .expect("an ordered book and the book after it are not the guard's business");
        }

        /// A type-1 book that symphonia would take to the table allocation: 2^22
        /// entries and 8 dimensions give a 2^25-value (128 MiB) f32 table, with
        /// all six multiplicands present. The guard refuses it first. The sizes
        /// keep symphonia's f32 `lookup1_values` exact, so the probe passes it on.
        #[test]
        fn a_type1_table_symphonia_would_allocate_is_refused_before_the_decoder() {
            let mut bits = TestBits::new();
            bits.push(0, 8); // one codebook
            oversized_codebook(&mut bits, 8, 4_194_304, 1); // 8 x 2^22 = a 2^25-value table
            for value in 0..6 {
                bits.push(value % 2, 1); // the six one-bit multiplicands, all present
            }
            let ogg = vorbis_header_ogg(&setup_packet(bits));
            let error = decode_guarded("https://example.test/table.ogg", &ogg, Some(48_000))
                .expect_err("a table symphonia would allocate must be refused first");
            assert!(error.contains("cap"), "names the value cap: {error}");
        }

        /// The type-1 multiplicand count, `floor(entries^(1/dims))`, computed by
        /// integer binary search. Exact-power and off-by-one edges are where an
        /// f32 root would drift, and dimension/entry zero are the saturating
        /// corners symphonia's release build hits.
        #[test]
        fn vorbis_lookup1_values_matches_the_spec() {
            assert_eq!(vorbis_lookup1_values(8, 3), 2); // 2^3 = 8
            assert_eq!(vorbis_lookup1_values(7, 3), 1); // 2^3 > 7
            assert_eq!(vorbis_lookup1_values(27, 3), 3); // 3^3 = 27
            assert_eq!(vorbis_lookup1_values(26, 3), 2); // 3^3 > 26
            assert_eq!(vorbis_lookup1_values(1, 1), 1);
            assert_eq!(vorbis_lookup1_values(16_777_215, 1), 16_777_215); // v^1 <= entries
            assert_eq!(vorbis_lookup1_values(0, 4), 0);
            assert_eq!(vorbis_lookup1_values(100, 0), u64::MAX); // saturating corner
        }

        /// The rename bypass, closed: a hostile Vorbis stream served under any
        /// extension reaches the same decoder, so the guard runs at the decoder
        /// boundary, not by file name. A book declaring a 2^26-value table (which
        /// fits u32, so symphonia's probe demuxes it without panicking) is refused
        /// whether the URL says `.ogg` or `.mp3`, and whether it arrives as a
        /// bank sample or a soundfont zone (which decodes through `decode_mp3`).
        /// symphonia would stop this particular book itself, short of the table,
        /// for want of its multiplicands, so this test pins *where* the guard runs;
        /// the type-1 test below pins what it prevents.
        #[test]
        fn a_hostile_vorbis_setup_is_refused_at_every_entry_point() {
            let mut bits = TestBits::new();
            bits.push(0, 8); // one codebook
            oversized_codebook(&mut bits, 8, 8_388_608, 2); // 2^26-value table, fits u32
            let ogg = vorbis_header_ogg(&setup_packet(bits));

            for url in [
                "https://example.test/attack.ogg",
                "https://example.test/attack.mp3",
            ] {
                let error = decode_guarded(url, &ogg, Some(48_000))
                    .expect_err("the hostile setup must be refused");
                assert!(error.contains("cap"), "{url} names the value cap: {error}");
                // A refusal does not depend on gapless trimming, so it is reported
                // once rather than again from a pointless non-gapless retry.
                assert!(
                    !error.contains("without gapless trimming"),
                    "{url} reports the refusal once: {error}"
                );
            }
            // The soundfont zone path decodes every non-RIFF zone through
            // `decode_mp3`; the guard fires there too.
            let error = decode_mp3(&ogg).expect_err("the zone path must refuse it");
            assert!(
                error.contains("cap"),
                "the zone path names the cap: {error}"
            );
        }

        /// A setup header split across two pages, hostile. symphonia's demuxer
        /// reassembles the packet from both pages before the codec sees it, so the
        /// guard inspects the whole header: a valid padding book fills the first
        /// page and the oversized book begins only on the continuation page. The
        /// old page-splicing pre-scan spliced the second page's 27-byte header into
        /// the packet and lost the walk; guarding the demuxed header cannot.
        #[test]
        fn a_hostile_setup_split_across_two_pages_is_refused() {
            let mut bits = TestBits::new();
            bits.push(1, 8); // two codebooks
            // A valid padding book: 512 entries of length 9 are a complete code
            // (512 x 2^-9 = 1), and their 320 bytes of dense lengths push the second
            // book past the first page's 255 bytes onto the continuation page.
            bits.push(0x564342, 24);
            bits.push(1, 16); // dimensions
            bits.push(512, 24); // entries
            bits.push(0, 1); // unordered
            bits.push(0, 1); // not sparse
            for _ in 0..512 {
                bits.push(8, 5); // length 9
            }
            bits.push(0, 4); // lookup type 0
            // The oversized book, now past the page boundary.
            oversized_codebook(&mut bits, 8, 8_388_608, 2);

            let setup = setup_packet(bits);
            assert!(setup.len() > 300, "the setup must span two pages");
            let ogg = vorbis_header_ogg_inner(&setup, true);
            let error = decode_guarded("https://example.test/split.ogg", &ogg, Some(48_000))
                .expect_err("the split hostile setup must be refused");
            assert!(error.contains("cap"), "names the value cap: {error}");
        }

        /// The committed real file: 30 ms of a 330 Hz tone, 8 kHz mono, encoded
        /// off-line by libvorbis into 2.7 KB.
        const TINY_VORBIS: &[u8] = include_bytes!("testdata/tiny_vorbis.ogg");

        /// A decode that ran but produced silence passes every other check and is
        /// wrong only at the speaker, so a real file is held to audible output at
        /// its own rate (decoded with no context rate, so nothing resamples it).
        fn assert_audible(decoded: &DecodedSample) {
            assert!(decoded.frames() > 0, "decoded to no frames");
            assert_eq!(decoded.sample_rate(), 8_000, "the fixture's own rate");
            let peak = decoded
                .pcm()
                .iter()
                .fold(0.0f32, |peak, sample| peak.max(sample.abs()));
            assert!(peak > 0.05, "decoded to near silence: peak {peak}");
        }

        /// A real libvorbis file decodes to audible audio through the production
        /// entry point: the guard walks its 19 codebooks and refuses none. It has
        /// no length-ordered books and no type-2 lookups; other tests cover those.
        #[test]
        fn a_real_vorbis_file_decodes_through_the_guarded_path() {
            let decoded = decode_guarded("kick.ogg", TINY_VORBIS, None)
                .expect("a real vorbis file must decode");
            assert_audible(&decoded);
        }

        /// The same file with its setup header split across two pages still
        /// decodes: the guard reads the reassembled header, not the raw page bytes.
        #[test]
        fn a_real_vorbis_setup_spanning_pages_still_decodes() {
            let repaginated = repaginate_setup_across_pages(TINY_VORBIS);
            let decoded = decode_guarded("kick.ogg", &repaginated, None)
                .expect("a real file with a page-spanning setup must decode");
            assert_audible(&decoded);
        }

        /// Reassemble the packets of a single-stream Ogg file in order, returning
        /// the serial, the final page's granule, and the packet payloads.
        fn ogg_packets(ogg: &[u8]) -> (u32, u64, Vec<Vec<u8>>) {
            let mut packets = Vec::new();
            let mut current = Vec::new();
            let (mut serial, mut final_gp) = (0u32, 0u64);
            let mut cursor = 0usize;
            while cursor + 27 <= ogg.len() && &ogg[cursor..cursor + 4] == b"OggS" {
                serial = u32::from_le_bytes(ogg[cursor + 14..cursor + 18].try_into().unwrap());
                let absgp = u64::from_le_bytes(ogg[cursor + 6..cursor + 14].try_into().unwrap());
                if absgp != u64::MAX {
                    final_gp = final_gp.max(absgp);
                }
                let segments = usize::from(ogg[cursor + 26]);
                let table = &ogg[cursor + 27..cursor + 27 + segments];
                let mut offset = cursor + 27 + segments;
                for &lacing in table {
                    current.extend_from_slice(&ogg[offset..offset + usize::from(lacing)]);
                    offset += usize::from(lacing);
                    if lacing < 255 {
                        packets.push(std::mem::take(&mut current));
                    }
                }
                cursor = offset;
            }
            (serial, final_gp, packets)
        }

        /// Re-lay a single-stream Ogg file so the setup packet spans two pages,
        /// keeping the identification, comment, and audio packets intact.
        fn repaginate_setup_across_pages(ogg: &[u8]) -> Vec<u8> {
            let (serial, final_gp, packets) = ogg_packets(ogg);
            assert!(packets.len() >= 4, "need ident, comment, setup, and audio");
            let mut out = ogg_page_packets(&[&packets[0]], 0x02, 0, serial, 0); // ident (BOS)
            out.extend(ogg_page_packets(&[&packets[1]], 0x00, 1, serial, 0)); // comment
            out.extend(split_packet_pages(&packets[2], 2, serial)); // setup (seq 2, 3)
            let audio: Vec<&[u8]> = packets[3..].iter().map(Vec::as_slice).collect();
            out.extend(ogg_page_packets(&audio, 0x04, 4, serial, final_gp)); // audio (EOS)
            out
        }
    }

    mod video_container_tests {

        //! Video containers as sample sources: their audio track is decoded,
        //! and a container with no audio track is refused by name.

        use super::*;

        /// 0.6 s of 440 Hz at 8 kHz, beside a 64x36 H.264 picture. The
        /// picture is the default track, so reaching the tone proves the
        /// audio track is chosen rather than the default one.
        const TINY_VIDEO: &[u8] = include_bytes!("testdata/tiny_video.mp4");
        /// The same encoder's picture alone, with no audio track.
        const SILENT_VIDEO: &[u8] = include_bytes!("testdata/tiny_video_silent.mp4");

        #[test]
        fn video_extensions_choose_a_video_container_decoder() {
            for url in [
                "clip.mp4",
                "clip.M4V",
                "clip.mov?cache=1",
                "https://example.test/clip.webm#t=1",
            ] {
                assert!(
                    matches!(codec_for(url), Codec::Mp4 | Codec::Matroska),
                    "{url} must reach a container decoder"
                );
            }
            assert_eq!(
                codec_for("file://C:/clips/deep bass.mp4"),
                Codec::Mp4,
                "a space in the path is part of the name"
            );
        }

        #[test]
        fn a_video_file_plays_its_audio_track() {
            let decoded =
                decode_guarded("clip.mp4", TINY_VIDEO, None).expect("the audio track must decode");
            assert_eq!(decoded.sample_rate(), 8_000);
            assert_eq!(decoded.channels(), 1);
            assert!(decoded.frames() > 2_000, "about half a second of tone");
            let peak = decoded
                .pcm()
                .iter()
                .fold(0.0f32, |peak, sample| peak.max(sample.abs()));
            assert!(peak > 0.05, "decoded to near silence: peak {peak}");
        }

        #[test]
        fn a_video_file_without_an_audio_track_says_so() {
            let error = decode_guarded("clip.mp4", SILENT_VIDEO, None)
                .expect_err("a silent picture is not a sample");
            assert!(
                error.contains("no audio track"),
                "names what is missing: {error}"
            );
        }
    }
}
