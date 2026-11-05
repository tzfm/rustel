//! The picture codecs this build decodes: how a track names its codec, and
//! the decoders that turn its packets into pictures.
//!
//! Every decoder is pure Rust. H.264 decodes through `rust_h264`, HEVC
//! through `rusty_h265`, AV1 through rav1d, VP8 and VP9 through OxiMedia,
//! and Motion JPEG through `image`. A parameter set or frame header that
//! names a picture past the decode limits is refused before the decoder
//! allocates for it: an allocation failure aborts, and no panic boundary
//! catches it.

use oximedia_codec::traits::{DecoderConfig, VideoDecoder as _};
use rust_h264::nal::{NalUnit, NalUnitType};

use super::av1;
use super::picture::{Colour, Picture, Plane, Samples, Yuv};

/// The longest picture edge a decoder here allocates for.
const MAX_PICTURE_EDGE: u32 = 8192;

/// The most pixels one picture may hold: 8K UHD at its widest.
const MAX_PICTURE_PIXELS: u32 = 8192 * 4320;

/// Threads one clip's decode may use. The decode runs beside the audio
/// engine and the editor, so it takes a few cores, not all of them.
const MAX_DECODE_THREADS: usize = 4;

pub(super) fn decode_threads() -> usize {
    std::thread::available_parallelism()
        .map_or(1, std::num::NonZero::get)
        .min(MAX_DECODE_THREADS)
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum Codec {
    MotionJpeg,
    Vp8,
    Vp9,
    Av1,
    H264,
    Hevc,
}

impl Codec {
    /// Whether the decoders hand pictures out in another order than their
    /// packets, as H.264 and HEVC do around B-frames. The others show a
    /// packet's picture before the next packet goes in, or none for a
    /// frame that is never shown.
    pub(super) fn reorders(self) -> bool {
        matches!(self, Self::H264 | Self::Hevc)
    }

    pub(super) fn name(self) -> &'static str {
        match self {
            Self::MotionJpeg => "Motion JPEG",
            Self::Vp8 => "VP8",
            Self::Vp9 => "VP9",
            Self::Av1 => "AV1",
            Self::H264 => "H.264",
            Self::Hevc => "HEVC",
        }
    }
}

/// What a container's track header says the track holds.
#[derive(Debug, PartialEq)]
pub(super) enum Named {
    Picture(Codec),
    /// A picture codec this build has no decoder for, by its common name.
    Unsupported(String),
    /// Text, timecode, sound or other data.
    NotPicture,
}

/// A track as its container's header describes it.
#[derive(Debug, PartialEq)]
pub(super) struct Entry {
    pub(super) named: Named,
    /// The `avcC`, `hvcC` or `av1C` record the header carries.
    pub(super) config: Option<Box<[u8]>>,
}

/// Why a track gives no picture.
#[derive(Debug, PartialEq)]
pub(super) enum Refusal {
    /// The track holds text, timecode or other data, not a picture.
    NotPicture,
    /// A picture this build cannot decode, and why.
    Codec(String),
}

fn cannot_decode(codec: &str) -> Refusal {
    Refusal::Codec(format!(
        "the picture is {codec}, which this build cannot decode. \
         Re-encode the clip as H.264, HEVC, AV1 or VP9."
    ))
}

/// How a stream frames its NAL units.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Framing {
    /// Start codes, as an elementary stream carries them.
    AnnexB,
    /// A big-endian length of this many bytes before each unit, as MP4 and
    /// Matroska carry them.
    Length(usize),
}

/// A picture track: its codec and the setup each of its decoders starts
/// from.
pub(super) struct Track {
    pub(super) codec: Codec,
    framing: Framing,
    /// The `avcC` or `hvcC` record with the stream's parameter sets.
    config: Option<Box<[u8]>>,
}

impl Track {
    /// Name a track's codec from its container's header, or failing that
    /// from its first packet.
    pub(super) fn identify(entry: Option<&Entry>, packet: &[u8]) -> Result<Self, Refusal> {
        let track = match entry {
            Some(Entry {
                named: Named::Picture(codec),
                config,
            }) => Self::configured(*codec, config.as_deref())?,
            // Some writers file a codec this build reads under a code it
            // does not map, such as Motion JPEG as `mp4v`. A packet that
            // says plainly what it is wins over the code.
            Some(Entry {
                named: Named::Unsupported(name),
                ..
            }) => Self::recognise(packet).ok_or_else(|| cannot_decode(name))?,
            Some(Entry {
                named: Named::NotPicture,
                ..
            }) => return Err(Refusal::NotPicture),
            None => Self::sniff(packet)?,
        };
        if track.codec == Codec::Vp9
            && let Some(profile) = vp9_profile(packet)
            && profile > 0
        {
            // OxiMedia decodes 8-bit 4:2:0, which is profile 0.
            return Err(cannot_decode(&format!("VP9 profile {profile}")));
        }
        Ok(track)
    }

    fn new(codec: Codec, framing: Framing, config: Option<Box<[u8]>>) -> Self {
        Self {
            codec,
            framing,
            config,
        }
    }

    /// A codec the header names, with the setup record it carries. H.264
    /// and HEVC frame their units by the length the record gives, or by
    /// four bytes with the parameter sets in the stream.
    fn configured(codec: Codec, config: Option<&[u8]>) -> Result<Self, Refusal> {
        let unreadable = || {
            Refusal::Codec(format!(
                "the {} track's setup record does not parse",
                codec.name()
            ))
        };
        let length = match (codec, config) {
            (Codec::H264, Some(config)) => avcc_length(config).ok_or_else(unreadable)?,
            (Codec::Hevc, Some(config)) => hvcc(config).ok_or_else(unreadable)?.0,
            (Codec::H264 | Codec::Hevc, None) => 4,
            _ => return Ok(Self::new(codec, Framing::AnnexB, None)),
        };
        Ok(Self::new(
            codec,
            Framing::Length(length),
            config.map(Into::into),
        ))
    }

    /// What a first packet says it holds, when the container names no
    /// codec. Each family starts with unmistakable bytes.
    /// The codec a packet names beyond doubt: a JPEG image, an elementary
    /// stream with its parameter sets, or a VP8 keyframe.
    fn recognise(packet: &[u8]) -> Option<Self> {
        let codec = |codec| Some(Self::new(codec, Framing::AnnexB, None));
        // A JPEG image begins with its SOI marker and another marker.
        if packet.starts_with(&[0xFF, 0xD8, 0xFF]) {
            return codec(Codec::MotionJpeg);
        }
        // An elementary stream opens with its parameter sets.
        if packet.starts_with(&[0, 0, 1]) || packet.starts_with(&[0, 0, 0, 1]) {
            // No HEVC unit of an opening access unit reads as an H.264
            // sequence parameter set, so H.264 is asked first.
            let nals = split_nals(packet, Framing::AnnexB);
            if nals.iter().any(|nal| h264_type(nal) == Some(7)) {
                return codec(Codec::H264);
            }
            if nals
                .iter()
                .any(|nal| matches!(hevc_type(nal), Some(32 | 33)))
            {
                return codec(Codec::Hevc);
            }
        }
        // A VP8 keyframe carries its start code at byte 3.
        if packet.len() >= 6 && packet[3..6] == [0x9D, 0x01, 0x2A] {
            return codec(Codec::Vp8);
        }
        None
    }

    fn sniff(packet: &[u8]) -> Result<Self, Refusal> {
        if let Some(track) = Self::recognise(packet) {
            return Ok(track);
        }
        let codec = |codec| Ok(Self::new(codec, Framing::AnnexB, None));
        if packet.starts_with(&[0, 0, 1]) || packet.starts_with(&[0, 0, 0, 1]) {
            return Err(cannot_decode("an elementary stream with no parameter sets"));
        }
        // VP9's two-bit frame marker is 0b10.
        if packet.first().is_some_and(|&byte| byte >> 6 == 0b10) {
            return codec(Codec::Vp9);
        }
        // An AV1 temporal unit opens with a temporal delimiter (type 2) or
        // a sequence header (type 1).
        if packet
            .first()
            .is_some_and(|&byte| byte & 0x80 == 0 && matches!((byte >> 3) & 0x0F, 1 | 2))
        {
            return codec(Codec::Av1);
        }
        Err(cannot_decode("an unrecognised video codec"))
    }

    /// Whether `packet` starts a picture that decodes without any before
    /// it, so the packets before it can be dropped undecoded.
    pub(super) fn is_keyframe(&self, packet: &[u8]) -> bool {
        match self.codec {
            Codec::MotionJpeg => true,
            // The frame tag's lowest bit is clear on a keyframe.
            Codec::Vp8 => packet.first().is_some_and(|&byte| byte & 1 == 0),
            Codec::Vp9 => vp9_keyframe(packet),
            Codec::Av1 => av1_keyframe(packet),
            Codec::H264 => split_nals(packet, self.framing)
                .iter()
                .any(|nal| h264_type(nal) == Some(5)),
            // IDR, CRA and BLA pictures. A fresh decoder drops the leading
            // pictures a CRA's group opens with, as the standard says.
            Codec::Hevc => split_nals(packet, self.framing)
                .iter()
                .any(|nal| matches!(hevc_type(nal), Some(16..=21))),
        }
    }

    /// Whether `packet` is an HEVC CRA picture. A fresh decoder that
    /// starts at one drops the RASL pictures of its group.
    pub(super) fn is_cra(&self, packet: &[u8]) -> bool {
        self.hevc_has(packet, |kind| kind == 21)
    }

    /// Whether `packet` is an HEVC RASL picture, which leans on pictures
    /// before the CRA it follows.
    pub(super) fn is_rasl(&self, packet: &[u8]) -> bool {
        self.hevc_has(packet, |kind| matches!(kind, 8 | 9))
    }

    fn hevc_has(&self, packet: &[u8], kind: impl Fn(u8) -> bool) -> bool {
        self.codec == Codec::Hevc
            && split_nals(packet, self.framing)
                .iter()
                .any(|nal| hevc_type(nal).is_some_and(&kind))
    }

    /// A fresh decoder of up to `threads` threads, primed with the track's
    /// parameter sets.
    pub(super) fn open(&self, threads: usize) -> Result<Decoder, String> {
        let config = self.config.as_deref();
        Ok(match self.codec {
            Codec::MotionJpeg => Decoder::MotionJpeg,
            Codec::Vp8 => Decoder::Vp8(Box::new(
                oximedia_codec::Vp8Decoder::new(oximedia_config(oximedia_core::CodecId::Vp8))
                    .map_err(|error| format!("VP8 decoder: {error}"))?,
            )),
            Codec::Vp9 => Decoder::Vp9(Box::new(Vp9 {
                decoder: oximedia_codec::vp9::Vp9Decoder::new(oximedia_config(
                    oximedia_core::CodecId::Vp9,
                ))
                .map_err(|error| format!("VP9 decoder: {error}"))?,
                colour: Colour::BT601,
            })),
            Codec::Av1 => Decoder::Av1(av1::Decoder::new(MAX_PICTURE_PIXELS, threads)?),
            Codec::H264 => Decoder::H264(Box::new(H264::new(self.framing, config, threads)?)),
            Codec::Hevc => Decoder::Hevc(Box::new(Hevc::new(self.framing, config)?)),
        })
    }
}

fn oximedia_config(codec: oximedia_core::CodecId) -> DecoderConfig {
    DecoderConfig {
        codec,
        ..DecoderConfig::default()
    }
}

/// The picture decoders this build carries.
pub(super) enum Decoder {
    /// Each packet is a whole JPEG image.
    MotionJpeg,
    Vp8(Box<oximedia_codec::Vp8Decoder>),
    Vp9(Box<Vp9>),
    Av1(av1::Decoder),
    H264(Box<H264>),
    Hevc(Box<Hevc>),
}

impl Decoder {
    /// Decode one packet, handing each picture it completes to `each` in
    /// display order. A failed packet costs its own pictures, not the
    /// clip's.
    pub(super) fn decode(
        &mut self,
        packet: &[u8],
        each: &mut dyn FnMut(Picture<'_>),
    ) -> Result<(), String> {
        match self {
            Self::MotionJpeg => {
                let (width, height, pixels) = decode_jpeg_frame(packet)?;
                each(Picture::Rgba {
                    width,
                    height,
                    pixels: &pixels,
                });
                Ok(())
            }
            Self::Vp8(decoder) => {
                // Every keyframe sizes the planes anew, so each is checked.
                if packet.first().is_some_and(|&byte| byte & 1 == 0)
                    && let Some((width, height)) = vp8_frame_size(packet)
                {
                    check_size(width, height)?;
                }
                decoder
                    .send_packet(packet, 0)
                    .map_err(|error| format!("VP8 packet: {error}"))?;
                while let Some(frame) = decoder
                    .receive_frame()
                    .map_err(|error| format!("VP8 frame: {error}"))?
                {
                    each(oximedia_picture(&frame, Colour::BT601)?);
                }
                Ok(())
            }
            Self::Vp9(vp9) => vp9.decode(packet, each),
            Self::Av1(decoder) => decoder.decode(packet, each),
            Self::H264(decoder) => decoder.decode(packet, each),
            Self::Hevc(decoder) => decoder.decode(packet, each),
        }
    }

    /// The pictures still held for reordering once the packets end.
    pub(super) fn finish(&mut self, each: &mut dyn FnMut(Picture<'_>)) {
        match self {
            Self::MotionJpeg | Self::Vp8(_) | Self::Vp9(_) => {}
            Self::Av1(decoder) => decoder.finish(each),
            Self::H264(decoder) => {
                let colour = decoder.colour;
                for frame in decoder.decoder.flush() {
                    each(h264_picture(&frame, colour));
                }
            }
            Self::Hevc(decoder) => {
                decoder.decoder.flush();
                decoder.drain(each);
            }
        }
    }
}

/// Refuse a picture past the decode limits.
fn check_size(width: u32, height: u32) -> Result<(), String> {
    if width.max(height) > MAX_PICTURE_EDGE
        || u64::from(width) * u64::from(height) > u64::from(MAX_PICTURE_PIXELS)
    {
        return Err(format!(
            "the clip says its picture is {width}x{height}, larger than the \
             8192x4320 this build decodes"
        ));
    }
    Ok(())
}

/// One Motion JPEG packet: a whole JPEG image.
fn decode_jpeg_frame(packet: &[u8]) -> Result<(u32, u32, Vec<u8>), String> {
    let mut reader = image::ImageReader::new(std::io::Cursor::new(packet))
        .with_guessed_format()
        .map_err(|error| format!("MJPEG frame: {error}"))?;
    // The header sizes the buffers before any pixel is seen.
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(MAX_PICTURE_EDGE);
    limits.max_image_height = Some(MAX_PICTURE_EDGE);
    limits.max_alloc = Some(256 << 20);
    reader.limits(limits);
    let decoded = reader
        .decode()
        .map_err(|error| format!("MJPEG frame: {error}"))?;
    let rgba = decoded.to_rgba8();
    Ok((rgba.width(), rgba.height(), rgba.into_raw()))
}

/// A VP8 keyframe's declared picture size, from its uncompressed frame
/// tag: 14 bits for each side, one less than stored.
fn vp8_frame_size(packet: &[u8]) -> Option<(u32, u32)> {
    if packet.len() < 10 {
        return None;
    }
    let edge = |low: u8, high: u8| u32::from((u16::from(high) & 0x3F) << 8 | u16::from(low)) + 1;
    Some((edge(packet[6], packet[7]), edge(packet[8], packet[9])))
}

/// An OxiMedia frame's planes. VP8 and VP9 profile 0 are 8-bit 4:2:0.
fn oximedia_picture(
    frame: &oximedia_codec::VideoFrame,
    colour: Colour,
) -> Result<Picture<'_>, String> {
    if frame.format != oximedia_core::PixelFormat::Yuv420p {
        return Err(format!("the decoder gave {:?} pixels", frame.format));
    }
    let (width, height) = (frame.width as usize, frame.height as usize);
    let plane = |index: usize, width: usize, height: usize| -> Result<Plane<'_>, String> {
        let plane = frame
            .planes
            .get(index)
            .ok_or_else(|| "the decoder gave no planes".to_owned())?;
        Ok(Plane {
            samples: Samples::Narrow(&plane.data),
            stride: plane.stride.max(width),
            width,
            height,
        })
    };
    let (chroma_width, chroma_height) = (width.div_ceil(2), height.div_ceil(2));
    Ok(Picture::Yuv(Yuv {
        width: frame.width,
        height: frame.height,
        luma: plane(0, width, height)?,
        chroma: Some([
            plane(1, chroma_width, chroma_height)?,
            plane(2, chroma_width, chroma_height)?,
        ]),
        depth: 8,
        colour,
    }))
}

pub(super) struct Vp9 {
    decoder: oximedia_codec::vp9::Vp9Decoder,
    /// The colour space the last keyframe signalled.
    colour: Colour,
}

impl Vp9 {
    fn decode(&mut self, packet: &[u8], each: &mut dyn FnMut(Picture<'_>)) -> Result<(), String> {
        let unreadable = |error: oximedia_codec::CodecError| format!("VP9 packet: {error}");
        let superframe = oximedia_codec::vp9::Superframe::parse(packet).map_err(unreadable)?;
        for frame in &superframe.frames {
            // A frame that takes its size from a reference does not parse
            // without the decoder's state. That size was checked with its
            // own frame.
            if let Ok(header) = oximedia_codec::vp9::UncompressedHeader::parse(frame)
                && !header.show_existing_frame
            {
                check_size(header.width, header.height)?;
                if header.frame_type == oximedia_codec::vp9::HeaderFrameType::Key {
                    self.colour = vp9_colour(&header);
                }
            }
        }
        self.decoder.send_packet(packet, 0).map_err(unreadable)?;
        while let Some(frame) = self.decoder.receive_frame().map_err(unreadable)? {
            each(oximedia_picture(&frame, self.colour)?);
        }
        Ok(())
    }
}

fn vp9_colour(header: &oximedia_codec::vp9::UncompressedHeader) -> Colour {
    use oximedia_codec::vp9::ColorSpace;
    let matrix = match header.color_space {
        ColorSpace::Bt601 | ColorSpace::Smpte170 => 6,
        ColorSpace::Bt709 => 1,
        ColorSpace::Bt2020 => 9,
        _ => 2,
    };
    Colour::signalled(matrix, header.color_range, header.height)
}

/// The profile in a VP9 frame header's first byte.
fn vp9_profile(packet: &[u8]) -> Option<u8> {
    let &byte = packet.first()?;
    (byte >> 6 == 0b10).then_some(((byte >> 5) & 1) | (((byte >> 4) & 1) << 1))
}

/// A VP9 packet whose first frame is a keyframe: frame marker, profile,
/// then a clear `show_existing_frame` and a clear `frame_type`.
fn vp9_keyframe(packet: &[u8]) -> bool {
    let (Some(profile), Some(&byte)) = (vp9_profile(packet), packet.first()) else {
        return false;
    };
    // Profile 3 spends one more bit before the flags.
    let show_existing = if profile == 3 { 2 } else { 3 };
    (byte >> show_existing) & 1 == 0 && (byte >> (show_existing - 1)) & 1 == 0
}

/// An AV1 temporal unit that opens a coded video sequence: a sequence
/// header and a frame header with `show_existing_frame` clear and
/// `frame_type` KEY_FRAME. A fresh decoder starts from the sequence
/// header, so a keyframe without one is not a starting point.
fn av1_keyframe(unit: &[u8]) -> bool {
    let mut sequence_header = false;
    let mut rest = unit;
    while let Some((&header, after)) = rest.split_first() {
        let kind = (header >> 3) & 0x0F;
        let extension = usize::from(header & 0x04 != 0);
        let Some(after) = after.get(extension..) else {
            return false;
        };
        let (size, after) = if header & 0x02 != 0 {
            match leb128(after) {
                Some(found) => found,
                None => return false,
            }
        } else {
            (after.len(), after)
        };
        let Some(payload) = after.get(..size) else {
            return false;
        };
        match kind {
            1 => sequence_header = true,
            // A frame header, alone or with its tile groups.
            3 | 6 => {
                return sequence_header
                    && payload
                        .first()
                        .is_some_and(|&byte| byte >> 7 == 0 && (byte >> 5) & 0x03 == 0);
            }
            _ => {}
        }
        rest = &after[size..];
    }
    false
}

/// An unsigned LEB128 value and the bytes after it.
fn leb128(data: &[u8]) -> Option<(usize, &[u8])> {
    let mut value = 0usize;
    for (index, &byte) in data.iter().enumerate().take(8) {
        value |= usize::from(byte & 0x7F) << (7 * index);
        if byte & 0x80 == 0 {
            return Some((value, &data[index + 1..]));
        }
    }
    None
}

/// The NAL units of one packet, without their framing.
fn split_nals(packet: &[u8], framing: Framing) -> Vec<&[u8]> {
    let mut nals = Vec::new();
    match framing {
        Framing::Length(size) => {
            let mut rest = packet;
            while let Some(prefix) = rest.get(..size) {
                let length = prefix
                    .iter()
                    .fold(0usize, |length, &byte| length << 8 | usize::from(byte));
                let Some(nal) = rest.get(size..size + length) else {
                    break;
                };
                nals.push(nal);
                rest = &rest[size + length..];
            }
        }
        Framing::AnnexB => {
            let mut starts = Vec::new();
            let mut at = 0;
            while at + 3 <= packet.len() {
                if packet[at..at + 3] == [0, 0, 1] {
                    starts.push(at + 3);
                    at += 3;
                } else {
                    at += 1;
                }
            }
            for (index, &start) in starts.iter().enumerate() {
                let end = starts.get(index + 1).map_or(packet.len(), |next| next - 3);
                // A four-byte start code leaves its zero on the unit before.
                let mut nal = &packet[start..end.max(start)];
                while let Some((&0, head)) = nal.split_last() {
                    nal = head;
                }
                if !nal.is_empty() {
                    nals.push(nal);
                }
            }
        }
    }
    nals
}

/// An H.264 NAL unit's type, when its forbidden bit is clear.
fn h264_type(nal: &[u8]) -> Option<u8> {
    nal.first()
        .filter(|&&byte| byte & 0x80 == 0)
        .map(|byte| byte & 0x1F)
}

/// A base-layer HEVC NAL unit's type, when its header is well formed: the
/// forbidden bit clear, layer 0 and a temporal id.
fn hevc_type(nal: &[u8]) -> Option<u8> {
    match nal {
        [first, second, ..] if first & 0x81 == 0 && second >> 3 == 0 && second & 0x07 != 0 => {
            Some(first >> 1)
        }
        _ => None,
    }
}

/// An `avcC` record's NAL length size, when it parses.
fn avcc_length(config: &[u8]) -> Option<usize> {
    rust_h264::nal::parse_avcc_config(config)
        .ok()
        .map(|parsed| parsed.length_size)
}

/// An `hvcC` record's NAL length size and its parameter sets, when it
/// parses.
fn hvcc(config: &[u8]) -> Option<(usize, Vec<&[u8]>)> {
    if config.len() < 23 || config[0] != 1 {
        return None;
    }
    let length = usize::from(config[21] & 0x03) + 1;
    if length == 3 {
        return None;
    }
    let mut rest = &config[23..];
    let mut nals = Vec::new();
    for _ in 0..config[22] {
        let count = u16::from_be_bytes(rest.get(1..3)?.try_into().ok()?);
        rest = &rest[3..];
        for _ in 0..count {
            let size = usize::from(u16::from_be_bytes(rest.get(..2)?.try_into().ok()?));
            nals.push(rest.get(2..2 + size)?);
            rest = &rest[2 + size..];
        }
    }
    Some((length, nals))
}

pub(super) struct H264 {
    /// Pictures decode on worker threads and leave in display order.
    decoder: rust_h264::threading::ThreadedDecoder,
    framing: Framing,
    /// The colour the last sequence parameter set signalled.
    colour: Colour,
}

impl H264 {
    fn new(framing: Framing, config: Option<&[u8]>, threads: usize) -> Result<Self, String> {
        let mut decoder = Self {
            decoder: rust_h264::threading::ThreadedDecoder::new(threads),
            framing,
            colour: Colour::BT601,
        };
        if let Some(config) = config {
            let parsed = rust_h264::nal::parse_avcc_config(config)
                .map_err(|error| format!("H.264 setup: {error}"))?;
            for nal in parsed.sps_nals.iter().chain(&parsed.pps_nals) {
                decoder.nal(nal, &mut |_| {})?;
            }
        }
        Ok(decoder)
    }

    fn decode(&mut self, packet: &[u8], each: &mut dyn FnMut(Picture<'_>)) -> Result<(), String> {
        let nals = match self.framing {
            Framing::Length(size) => rust_h264::nal::parse_avcc(packet, size),
            Framing::AnnexB => rust_h264::nal::parse_annex_b(packet),
        };
        let mut failure = None;
        for nal in &nals {
            if let Err(message) = self.nal(nal, each) {
                failure.get_or_insert(message);
            }
        }
        failure.map_or(Ok(()), Err)
    }

    fn nal(&mut self, nal: &NalUnit<'_>, each: &mut dyn FnMut(Picture<'_>)) -> Result<(), String> {
        if matches!(nal.nal_unit_type, NalUnitType::Sps)
            && let Ok(info) = rust_h264::nal::parse_sps_info(nal)
        {
            check_size(info.coded_width, info.coded_height)?;
            self.colour = Colour::signalled(info.matrix, info.full_range, info.height);
        }
        let frames = self
            .decoder
            .decode_nal(nal)
            .map_err(|error| format!("H.264: {error}"))?;
        for frame in &frames {
            each(h264_picture(frame, self.colour));
        }
        Ok(())
    }
}

fn h264_picture(frame: &rust_h264::decoder::Frame, colour: Colour) -> Picture<'_> {
    let samples = |data| {
        if frame.bit_depth > 8 {
            Samples::WideBytes(data)
        } else {
            Samples::Narrow(data)
        }
    };
    let (width, height) = (frame.width as usize, frame.height as usize);
    let (chroma_width, chroma_height) = (
        frame.chroma_width() as usize,
        frame.chroma_height() as usize,
    );
    let chroma = |data| Plane {
        samples: samples(data),
        stride: chroma_width,
        width: chroma_width,
        height: chroma_height,
    };
    Picture::Yuv(Yuv {
        width: frame.width,
        height: frame.height,
        luma: Plane {
            samples: samples(&frame.y),
            stride: width,
            width,
            height,
        },
        chroma: Some([chroma(&frame.u), chroma(&frame.v)]),
        depth: frame.bit_depth,
        colour,
    })
}

pub(super) struct Hevc {
    decoder: rusty_h265::Decoder,
    framing: Framing,
    /// The colour the last sequence parameter set signalled.
    colour: Colour,
}

impl Hevc {
    fn new(framing: Framing, config: Option<&[u8]>) -> Result<Self, String> {
        let mut decoder = Self {
            decoder: rusty_h265::Decoder::new(),
            framing,
            colour: Colour::BT601,
        };
        if let Some(config) = config {
            let (_, nals) =
                hvcc(config).ok_or_else(|| "the HEVC setup record does not parse".to_owned())?;
            for nal in nals {
                decoder.nal(nal)?;
            }
        }
        Ok(decoder)
    }

    fn decode(&mut self, packet: &[u8], each: &mut dyn FnMut(Picture<'_>)) -> Result<(), String> {
        let mut failure = None;
        for nal in split_nals(packet, self.framing) {
            if let Err(message) = self.nal(nal) {
                failure.get_or_insert(message);
            }
        }
        self.drain(each);
        failure.map_or(Ok(()), Err)
    }

    fn nal(&mut self, nal: &[u8]) -> Result<(), String> {
        if hevc_type(nal) == Some(33) {
            let rbsp = rusty_h265::nal::unescape(&nal[2..]);
            if let Ok(sps) = rusty_h265::ps::parse_sps(&rbsp.data) {
                check_size(sps.width, sps.height)?;
                let (matrix, full_range) = sps.vui.as_ref().map_or((2, false), |vui| {
                    (vui.matrix_coeffs, vui.video_full_range_flag)
                });
                self.colour = Colour::signalled(matrix, full_range, sps.height);
            }
        }
        self.decoder
            .push_nal(nal, None)
            .map_err(|error| format!("HEVC: {error}"))
    }

    fn drain(&mut self, each: &mut dyn FnMut(Picture<'_>)) {
        while let Ok(frame) = self.decoder.next_frame() {
            if let Some(picture) = hevc_picture(&frame, self.colour) {
                each(picture);
            }
        }
    }
}

/// An HEVC frame's cropped planes. Every bit depth is stored as `u16`.
fn hevc_picture(frame: &rusty_h265::Frame, colour: Colour) -> Option<Picture<'_>> {
    let picture = &frame.picture;
    let (left, top, width, height) = picture.crop;
    let (shift_x, shift_y) = match picture.chroma_format_idc {
        1 => (1, 1),
        2 => (1, 0),
        _ => (0, 0),
    };
    let plane = |index: usize, left: usize, top: usize, width: usize, height: usize| {
        let plane = picture.planes.get(index)?;
        Some(Plane {
            samples: Samples::Wide(plane.data.get(top * plane.stride + left..)?),
            stride: plane.stride,
            width,
            height,
        })
    };
    let luma = plane(0, left, top, width, height)?;
    let chroma = if picture.chroma_format_idc == 0 {
        None
    } else {
        let at = |index| {
            plane(
                index,
                left >> shift_x,
                top >> shift_y,
                (width + shift_x) >> shift_x,
                (height + shift_y) >> shift_y,
            )
        };
        Some([at(1)?, at(2)?])
    };
    Some(Picture::Yuv(Yuv {
        width: u32::try_from(width).ok()?,
        height: u32::try_from(height).ok()?,
        luma,
        chroma,
        depth: picture.bit_depth_luma,
        colour,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A VP8 keyframe's tag sizes the decoder's planes before any pixel
    /// is decoded, so an edge past the decode limit is refused by name and
    /// never reaches the allocator. Every keyframe is checked, not only
    /// the first.
    #[test]
    fn a_lying_vp8_keyframe_edge_is_refused() {
        let mut keyframe = [0u8; 10];
        // Keyframe flag clear, the VP8 start code, then 16383x16383 in the
        // tag's 14-bit sides-less-one.
        keyframe[3..6].copy_from_slice(&[0x9D, 0x01, 0x2A]);
        keyframe[6..8].copy_from_slice(&0x3FFE_u16.to_le_bytes());
        keyframe[8..10].copy_from_slice(&0x3FFE_u16.to_le_bytes());
        assert_eq!(vp8_frame_size(&keyframe), Some((16_383, 16_383)));
        let track = Track::identify(None, &keyframe).expect("VP8");
        let mut decoder = track.open(1).expect("a decoder");
        let refused = decoder.decode(&keyframe, &mut |_| panic!("no picture"));
        assert!(
            refused.is_err_and(|message| message.contains("8192x4320")),
            "the edge is refused by name"
        );
        assert_eq!(vp8_frame_size(&[0; 9]), None, "a short tag is no tag");
    }

    #[test]
    fn a_header_entry_names_the_codec_and_its_framing() {
        let entry = |named, config: Option<&[u8]>| Entry {
            named,
            config: config.map(Into::into),
        };
        // An avcC record with two-byte NAL lengths and no parameter sets.
        let avcc = [1, 100, 0, 31, 0xFD, 0xE0, 0];
        let h264 = Track::identify(Some(&entry(Named::Picture(Codec::H264), Some(&avcc))), &[])
            .expect("H.264");
        assert_eq!(
            (h264.codec, h264.framing),
            (Codec::H264, Framing::Length(2))
        );
        let hevc =
            Track::identify(Some(&entry(Named::Picture(Codec::Hevc), None)), &[]).expect("HEVC");
        assert_eq!(
            hevc.framing,
            Framing::Length(4),
            "parameter sets in the stream"
        );
        assert!(matches!(
            Track::identify(Some(&entry(Named::Picture(Codec::H264), Some(&[2, 0]))), &[]),
            Err(Refusal::Codec(message)) if message.contains("H.264 track's setup")
        ));
        assert!(matches!(
            Track::identify(Some(&entry(Named::Unsupported("ProRes".to_owned()), None)), &[]),
            Err(Refusal::Codec(message)) if message.contains("ProRes")
        ));
        // Motion JPEG filed as `mp4v` is still Motion JPEG.
        let jpeg = [0xFF, 0xD8, 0xFF, 0xE0];
        let filed = entry(Named::Unsupported("MPEG-4 Part 2".to_owned()), None);
        assert_eq!(
            Track::identify(Some(&filed), &jpeg).map(|track| track.codec),
            Ok(Codec::MotionJpeg)
        );
        // A packet that says nothing plainly keeps the header's name.
        assert!(matches!(
            Track::identify(Some(&filed), &[0b1000_0000, 0, 0]),
            Err(Refusal::Codec(message)) if message.contains("MPEG-4 Part 2")
        ));
        assert_eq!(
            Track::identify(Some(&entry(Named::NotPicture, None)), &[]).err(),
            Some(Refusal::NotPicture)
        );
    }

    #[test]
    fn a_vp9_profile_past_zero_is_refused_by_name() {
        // Frame marker, profile 2, then a keyframe's clear flags.
        let packet = [0b1001_0000, 0, 0];
        assert_eq!(vp9_profile(&packet), Some(2));
        assert!(matches!(
            Track::identify(None, &packet),
            Err(Refusal::Codec(message)) if message.contains("VP9 profile 2")
        ));
    }

    #[test]
    fn keyframes_are_told_from_the_frames_that_need_them() {
        assert!(vp9_keyframe(&[0b1000_0000]), "profile 0 keyframe");
        assert!(!vp9_keyframe(&[0b1000_0100]), "profile 0 inter frame");
        assert!(!vp9_keyframe(&[0b1000_1000]), "a shown existing frame");
        assert!(
            vp9_keyframe(&[0b1011_0000]),
            "profile 3 skips its reserved bit"
        );
        // A temporal delimiter, a sequence header, then a key frame header.
        let key = [0x12, 0x00, 0x0A, 0x01, 0x00, 0x32, 0x01, 0x10];
        assert!(av1_keyframe(&key));
        // The same unit without its sequence header is no starting point.
        assert!(!av1_keyframe(&[0x12, 0x00, 0x32, 0x01, 0x10]));
        // An inter frame header: frame_type 1.
        assert!(!av1_keyframe(&[
            0x12, 0x00, 0x0A, 0x01, 0x00, 0x32, 0x01, 0x30
        ]));
        let track = |codec, framing| Track::new(codec, framing, None);
        let h264 = track(Codec::H264, Framing::Length(4));
        assert!(h264.is_keyframe(&[0, 0, 0, 2, 0x65, 0x88]), "an IDR slice");
        assert!(!h264.is_keyframe(&[0, 0, 0, 2, 0x41, 0x9A]), "a P slice");
        let hevc = track(Codec::Hevc, Framing::AnnexB);
        assert!(
            hevc.is_keyframe(&[0, 0, 0, 1, 0x26, 0x01, 0xAF]),
            "an IDR picture"
        );
        assert!(
            !hevc.is_keyframe(&[0, 0, 0, 1, 0x02, 0x01, 0xD0]),
            "a trailing picture"
        );
    }

    #[test]
    fn nal_units_split_from_either_framing() {
        let annex_b = [
            0, 0, 0, 1, 0x67, 0x42, 0, 0, 1, 0x68, 0xCE, 0, 0, 0, 1, 0x65,
        ];
        assert_eq!(
            split_nals(&annex_b, Framing::AnnexB),
            vec![&[0x67, 0x42][..], &[0x68, 0xCE][..], &[0x65][..]]
        );
        let lengths = [0, 0, 0, 2, 0x67, 0x42, 0, 0, 0, 9, 0x65];
        assert_eq!(
            split_nals(&lengths, Framing::Length(4)),
            vec![&[0x67, 0x42][..]],
            "a length past the packet ends the split"
        );
    }

    /// An elementary stream names its codec by its parameter sets, which
    /// tells HEVC's two-byte headers from H.264's one-byte ones.
    #[test]
    fn an_elementary_stream_is_named_by_its_parameter_sets() {
        let hevc = [0, 0, 0, 1, 0x40, 0x01, 0x0C, 0, 0, 0, 1, 0x26, 0x01];
        let h264 = [0, 0, 0, 1, 0x67, 0x64, 0x00, 0x1F, 0, 0, 0, 1, 0x65];
        let codec = |packet: &[u8]| Track::identify(None, packet).map(|track| track.codec);
        assert_eq!(codec(&hevc), Ok(Codec::Hevc));
        assert_eq!(codec(&h264), Ok(Codec::H264));
        assert!(codec(&[0, 0, 1, 0x26, 0x01]).is_err(), "no parameter sets");
    }

    #[test]
    fn a_picture_past_the_limits_is_refused() {
        assert!(check_size(7680, 4320).is_ok(), "8K UHD decodes");
        assert!(check_size(8193, 16).is_err(), "too wide");
        assert!(check_size(8192, 8192).is_err(), "too many pixels");
    }
}
