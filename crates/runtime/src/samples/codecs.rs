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
    let track = format
        .default_track()
        .ok_or_else(|| format!("{extension} has no track"))?;
    let track_id = track.id;
    let codec = track.codec_params.codec;
    // This is the one boundary every container reaches, whatever its
    // extension, before a decoder is built. A Vorbis setup header can size a
    // codebook lookup table so large that `VorbisDecoder::try_new` aborts the
    // process allocating it; guard the exact bytes the decoder will read -
    // the demuxed identification and setup headers - before `make` sees them.
    if codec == symphonia::core::codecs::CODEC_TYPE_VORBIS {
        guard_vorbis_setup(track.codec_params.extra_data.as_deref().unwrap_or_default())
            .map_err(CompressedFailure::Refused)?;
    }
    // Encoder delay and padding, for the manual trim of the fallback path;
    // symphonia trims them itself when gapless is on.
    let (delay, padding) = if gapless {
        (0usize, 0usize)
    } else {
        (
            track.codec_params.delay.unwrap_or(0) as usize,
            track.codec_params.padding.unwrap_or(0) as usize,
        )
    };
    if codec == symphonia::core::codecs::CODEC_TYPE_OPUS {
        let params = track.codec_params.clone();
        return decode_opus_packets(&mut format, track_id, &params).map_err(Into::into);
    }
    let mut decoder = symphonia::default::get_codecs()
        .make(&track.codec_params, &DecoderOptions::default())
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
