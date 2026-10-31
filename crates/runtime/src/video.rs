//! A video container's picture, decoded once into bounded thumbnails.
//!
//! A video sample's sound is its audio track ([`crate::samples`]); this
//! module is its picture. The first video track's packets are demuxed with
//! Symphonia and decoded - Motion JPEG through `image`, VP8 through
//! OxiMedia, both pure Rust. The frames are thinned to a fixed byte
//! budget, so a painter can follow the clip where the sound is without
//! holding its full frames. H.264 and its family are named and refused:
//! no native decoder ships yet, and the message says what to re-encode
//! the clip as.

use std::sync::Arc;

use symphonia::core::formats::FormatOptions;
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;

/// The longest edge a thumbnail keeps. The inline painter is small, and a
/// larger picture would only cost memory.
const THUMB_EDGE: u32 = 160;

/// The longest frame edge this module will decode, whatever the container
/// says. A frame's own header sizes the decoder's allocations before any
/// pixel is checked, so a lying edge is refused here, not by the
/// allocator: an allocation failure aborts, and no panic boundary catches
/// it.
const MAX_PICTURE_EDGE: u32 = 4096;

/// RGBA bytes one clip's thumbnails may hold between them. A clip with
/// more frames than that thins to fewer, evenly spaced ones.
const CLIP_BUDGET_BYTES: usize = 12 << 20;

/// Container packets one clip may carry before the decode stops early,
/// sound packets counted with the picture's. The kept strip covers the
/// decoded prefix; an hour-long video is a film, not a sample.
const MAX_PICTURE_PACKETS: u64 = 65_536;

/// The fastest thumbnail rate kept. Frames closer together than this are
/// decoded - a later frame needs them - but not stored.
const MAX_KEPT_FPS: f32 = 24.0;

/// One clip's picture as a filmstrip of RGBA thumbnails.
#[derive(Clone, Debug)]
pub struct VideoThumbnails {
    width: u32,
    height: u32,
    /// Presentation time of each kept frame, in seconds from the picture's
    /// start, ascending.
    times: Box<[f32]>,
    /// `width * height * 4` RGBA bytes per kept frame, in `times` order.
    frames: Box<[u8]>,
}

impl VideoThumbnails {
    /// Build a strip from kept frames: presentation times in ascending
    /// order, each frame `width * height * 4` RGBA bytes. The library
    /// builds these by decoding a container; a host that decodes its own
    /// pictures can build one the same way.
    pub fn from_frames(
        width: u32,
        height: u32,
        frames: Vec<(f32, Vec<u8>)>,
    ) -> Result<Self, String> {
        let stride = width as usize * height as usize * 4;
        if width == 0 || height == 0 {
            return Err("a strip is at least one pixel".to_owned());
        }
        let mut times = Vec::with_capacity(frames.len());
        let mut packed = Vec::with_capacity(stride * frames.len());
        let mut last = f32::MIN;
        for (time, frame) in frames {
            if !time.is_finite() || time <= last {
                return Err("frame times are finite and ascending".to_owned());
            }
            if frame.len() != stride {
                return Err("a frame is width * height * 4 bytes".to_owned());
            }
            times.push(time);
            packed.extend_from_slice(&frame);
            last = time;
        }
        if times.is_empty() {
            return Err("a strip has at least one frame".to_owned());
        }
        Ok(Self {
            width,
            height,
            times: times.into(),
            frames: packed.into(),
        })
    }

    pub fn width(&self) -> u32 {
        self.width
    }

    pub fn height(&self) -> u32 {
        self.height
    }

    pub fn frame_count(&self) -> usize {
        self.times.len()
    }

    /// The picture's last kept frame time, or 0 for an empty strip.
    pub fn duration(&self) -> f32 {
        self.times.last().copied().unwrap_or(0.0)
    }

    /// The frame showing `seconds` into the clip: the newest kept frame at
    /// or before it. Between two kept frames the earlier one shows, the way
    /// a filmstrip does, and a time past the last frame holds it.
    pub fn frame_at(&self, seconds: f32) -> Option<&[u8]> {
        if self.times.is_empty() || !seconds.is_finite() || seconds < 0.0 {
            return None;
        }
        let index = self.times.partition_point(|&time| time <= seconds).max(1) - 1;
        let stride = self.width as usize * self.height as usize * 4;
        self.frames.get(index * stride..(index + 1) * stride)
    }
}

/// Where a clip's picture stands, as the library answers a painter.
#[derive(Clone, Debug)]
pub enum VideoPicture {
    /// A decode is running on a library thread; ask again on a later poll.
    Decoding,
    Ready(Arc<VideoThumbnails>),
    /// Why there is no strip. Names the codec when the build cannot decode it.
    Failed(String),
}

/// What a first packet says the picture track holds. Container headers
/// name codecs only as far as Symphonia's audio tables go, so the packet
/// itself identifies the codec: each family starts with unmistakable bytes.
enum PictureCodec {
    MotionJpeg,
    Vp8,
    Vp9,
    Av1,
    H264,
    Hevc,
    Unknown,
}

fn sniff_picture_codec(packet: &[u8]) -> PictureCodec {
    // A JPEG image begins with its SOI marker and another marker.
    if packet.starts_with(&[0xFF, 0xD8, 0xFF]) {
        return PictureCodec::MotionJpeg;
    }
    // Matroska carries H.264 and HEVC behind Annex B start codes, with the
    // parameter sets in the track header: a stream can open on a slice,
    // an AUD or an SEI, not only on an SPS.
    if packet.starts_with(&[0, 0, 1]) || packet.starts_with(&[0, 0, 0, 1]) {
        let start = if packet[2] == 1 { 3 } else { 4 };
        return match packet.get(start) {
            // HEVC heads with a VPS or SPS (32, 33) or an IRAP slice
            // (19 through 21).
            Some(&nal) if matches!((nal >> 1) & 0x3F, 19..=21 | 32..=34) => PictureCodec::Hevc,
            // H.264: forbidden bit clear, then any VCL NAL, SPS, or AUD.
            Some(&nal) if nal & 0x80 == 0 && matches!(nal & 0x1F, 1..=9) => PictureCodec::H264,
            _ => PictureCodec::Unknown,
        };
    }
    // A VP8 chunk carries its start code at byte 3.
    if packet.len() >= 6 && packet[3..6] == [0x9D, 0x01, 0x2A] {
        return PictureCodec::Vp8;
    }
    // MP4 stores H.264 and HEVC as length-prefixed NALs, with the parameter
    // sets in the track header rather than the stream: a sample begins with
    // a slice. The length must stay inside the packet, or the bytes are not
    // a prefix at all.
    if let (Some(length), Some(&nal)) = (
        packet
            .get(..4)
            .and_then(|bytes| bytes.try_into().ok())
            .map(u32::from_be_bytes),
        packet.get(4),
    ) && usize::try_from(length).is_ok_and(|length| length < packet.len())
    {
        // H.264: forbidden bit clear, then a slice or a parameter set.
        if nal & 0x80 == 0 && matches!(nal & 0x1F, 1..=9) {
            return PictureCodec::H264;
        }
        // HEVC: the type is the first six bits after the forbidden one.
        if nal & 0x80 == 0 && matches!((nal >> 1) & 0x3F, 16..=23 | 32..=35) {
            return PictureCodec::Hevc;
        }
    }
    // VP9's two-bit frame marker is 0b10.
    if packet.first().is_some_and(|&byte| byte >> 6 == 0b10) {
        return PictureCodec::Vp9;
    }
    // AV1 packs OBUs; a stream starts with a temporal delimiter (type 2)
    // or a sequence header (type 1).
    if packet
        .first()
        .is_some_and(|&byte| byte & 0x80 == 0 && matches!((byte >> 3) & 0x0F, 1 | 2))
    {
        return PictureCodec::Av1;
    }
    PictureCodec::Unknown
}

impl PictureCodec {
    fn name(self) -> &'static str {
        match self {
            Self::MotionJpeg => "Motion JPEG",
            Self::Vp8 => "VP8",
            Self::Vp9 => "VP9",
            Self::Av1 => "AV1",
            Self::H264 => "H.264",
            Self::Hevc => "H.265",
            Self::Unknown => "an unrecognised video codec",
        }
    }

    /// The decoders this build carries. A codec with none is refused before
    /// any packet is wasted on it. `packet` is the sniffed one, whose
    /// headers size the decoder's allocations.
    fn decoder(self, packet: &[u8]) -> Result<PictureDecoder, String> {
        match self {
            Self::MotionJpeg => Ok(PictureDecoder::MotionJpeg),
            Self::Vp8 => {
                // The frame tag sizes the decoder's planes before any
                // pixel is decoded; a lying edge is refused here rather
                // than by the allocator, whose failure aborts.
                if let Some((width, height)) = vp8_frame_size(packet)
                    && width.max(height) > MAX_PICTURE_EDGE
                {
                    return Err(format!(
                        "the clip says its picture is {width}x{height}, past the \
                         {MAX_PICTURE_EDGE} pixel decode limit"
                    ));
                }
                Ok(PictureDecoder::Vp8(Box::new(
                    oximedia_codec::Vp8Decoder::new(oximedia_codec::DecoderConfig {
                        codec: oximedia_core::CodecId::Vp8,
                        ..Default::default()
                    })
                    .map_err(|error| format!("VP8 decoder: {error}"))?,
                )))
            }
            codec => Err(format!(
                "the picture is {}, which this build cannot decode yet; \
                 re-encode the clip as VP8 WebM or Motion JPEG",
                codec.name(),
            )),
        }
    }
}

/// The picture decoders this build carries.
enum PictureDecoder {
    /// Each packet is a whole JPEG image.
    MotionJpeg,
    /// OxiMedia's push-pull VP8 decoder, boxed to keep the enum small.
    Vp8(Box<oximedia_codec::Vp8Decoder>),
}

impl PictureDecoder {
    /// One packet to RGBA. A failed frame is skipped by the caller, so a
    /// torn packet costs one frame and not the clip.
    fn decode(&mut self, packet: &[u8], pts: i64) -> Result<(u32, u32, Vec<u8>), String> {
        match self {
            Self::MotionJpeg => decode_jpeg_frame(packet),
            Self::Vp8(decoder) => {
                use oximedia_codec::VideoDecoder as _;
                decoder
                    .send_packet(packet, pts)
                    .map_err(|error| format!("VP8 packet: {error}"))?;
                let frame = decoder
                    .receive_frame()
                    .map_err(|error| format!("VP8 frame: {error}"))?
                    .ok_or("VP8 packet decoded to no frame")?;
                yuv420_to_rgba(&frame)
            }
        }
    }
}

/// One Motion JPEG packet: a whole JPEG image.
fn decode_jpeg_frame(packet: &[u8]) -> Result<(u32, u32, Vec<u8>), String> {
    let mut reader = image::ImageReader::new(std::io::Cursor::new(packet))
        .with_guessed_format()
        .map_err(|error| format!("MJPEG frame: {error}"))?;
    // A frame far larger than its thumbnail can only cost time and memory,
    // and its own header sizes the buffers before any pixel is seen.
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
/// tag: 14 bits for each side, one less than stored. Read before the
/// decoder is built, because the tag sizes its planes.
fn vp8_frame_size(packet: &[u8]) -> Option<(u32, u32)> {
    if packet.len() < 10 {
        return None;
    }
    let edge = |low: u8, high: u8| u32::from((u16::from(high) & 0x3F) << 8 | u16::from(low)) + 1;
    Some((edge(packet[6], packet[7]), edge(packet[8], packet[9])))
}

/// A decoded VP8 frame to RGBA. VP8 pictures are 4:2:0 BT.601 with
/// limited range, and the planes carry their own strides.
fn yuv420_to_rgba(frame: &oximedia_codec::VideoFrame) -> Result<(u32, u32, Vec<u8>), String> {
    if frame.format != oximedia_core::PixelFormat::Yuv420p {
        return Err(format!("VP8 gave {:?} pixels", frame.format).to_string());
    }
    let (width, height) = (frame.width as usize, frame.height as usize);
    if frame.planes.len() < 3 || width == 0 || height == 0 {
        return Err("VP8 gave no planes".to_owned());
    }
    let plane = |index: usize| -> Result<(&[u8], usize), String> {
        let plane = frame
            .planes
            .get(index)
            .ok_or_else(|| "VP8 gave no planes".to_owned())?;
        Ok((&plane.data, plane.stride.max(plane.width as usize).max(1)))
    };
    let (y, y_stride) = plane(0)?;
    let (u, u_stride) = plane(1)?;
    let (v, v_stride) = plane(2)?;
    let mut rgba = Vec::with_capacity(width * height * 4);
    let mut convert = |luma: u8, chroma_u: u8, chroma_v: u8| {
        // BT.601 limited range, the coefficients every YUV conversion in
        // this engine shares.
        let luma = f32::from(luma) - 16.0;
        let chroma_u = f32::from(chroma_u) - 128.0;
        let chroma_v = f32::from(chroma_v) - 128.0;
        let red = 1.164_38 * luma + 1.596_03 * chroma_v;
        let green = 1.164_38 * luma - 0.391_76 * chroma_u - 0.812_97 * chroma_v;
        let blue = 1.164_38 * luma + 2.017_23 * chroma_u;
        for channel in [red, green, blue] {
            rgba.push(channel.round().clamp(0.0, 255.0) as u8);
        }
        rgba.push(255);
    };
    for row in 0..height {
        let y_row = row * y_stride;
        let chroma_row = (row / 2) * u_stride;
        let v_row = (row / 2) * v_stride;
        for column in 0..width {
            let luma = *y.get(y_row + column).ok_or("VP8 luma plane is short")?;
            let at = column / 2;
            let chroma_u = *u.get(chroma_row + at).ok_or("VP8 chroma plane is short")?;
            let chroma_v = *v.get(v_row + at).ok_or("VP8 chroma plane is short")?;
            convert(luma, chroma_u, chroma_v);
        }
    }
    Ok((width as u32, height as u32, rgba))
}

/// Average the source pixels under each destination pixel. Nearest sampling
/// would shimmer as the clip plays; a box keeps still areas still.
fn scale_rgba(
    source: &[u8],
    source_width: u32,
    source_height: u32,
    width: u32,
    height: u32,
) -> Vec<u8> {
    let (sw, sh, dw, dh) = (
        source_width as usize,
        source_height as usize,
        width as usize,
        height as usize,
    );
    if sw == 0 || sh == 0 || dw == 0 || dh == 0 {
        return Vec::new();
    }
    let mut out = Vec::with_capacity(dw * dh * 4);
    for dy in 0..dh {
        // Source rows this destination row covers.
        let y0 = dy * sh / dh;
        let y1 = ((dy + 1) * sh / dh).max(y0 + 1);
        for dx in 0..dw {
            let x0 = dx * sw / dw;
            let x1 = ((dx + 1) * sw / dw).max(x0 + 1);
            let (mut red, mut green, mut blue) = (0u64, 0u64, 0u64);
            let mut pixels = 0u64;
            for y in y0..y1.min(sh) {
                let row = y * sw;
                for x in x0..x1.min(sw) {
                    let at = (row + x) * 4;
                    red += u64::from(source[at]);
                    green += u64::from(source[at + 1]);
                    blue += u64::from(source[at + 2]);
                    pixels += 1;
                }
            }
            if pixels == 0 {
                out.extend_from_slice(&[0, 0, 0, 255]);
            } else {
                let mean = |sum: u64| u8::try_from(sum / pixels).unwrap_or(u8::MAX);
                out.push(mean(red));
                out.push(mean(green));
                out.push(mean(blue));
                out.push(255);
            }
        }
    }
    out
}

/// Thumbnail dimensions for a frame `width` by `height`: the largest fit
/// inside [`THUMB_EDGE`] that keeps the frame's shape. A frame already
/// small enough keeps its own size.
fn thumb_size(width: u32, height: u32) -> (u32, u32) {
    if width.max(height) <= THUMB_EDGE {
        return (width.max(1), height.max(1));
    }
    if width >= height {
        let scaled = (u64::from(height) * u64::from(THUMB_EDGE) / u64::from(width)).max(1);
        (THUMB_EDGE, u32::try_from(scaled).unwrap_or(THUMB_EDGE))
    } else {
        let scaled = (u64::from(width) * u64::from(THUMB_EDGE) / u64::from(height)).max(1);
        (u32::try_from(scaled).unwrap_or(THUMB_EDGE), THUMB_EDGE)
    }
}

/// The strip under construction: frames thinned as the budget fills, so
/// the working set never exceeds it whatever the clip's length.
struct Strip {
    width: u32,
    height: u32,
    times: Vec<f32>,
    frames: Vec<u8>,
    /// Seconds between kept frames, which doubles every time the strip
    /// thins.
    min_gap: f32,
    last_time: f32,
    decoded: usize,
    failures: usize,
}

impl Strip {
    fn frame_bytes(&self) -> usize {
        self.width as usize * self.height as usize * 4
    }

    fn keep(&mut self, time: f32, frame: &[u8]) {
        self.times.push(time);
        self.frames.extend_from_slice(frame);
        self.last_time = time;
        if self.frames.len() > CLIP_BUDGET_BYTES / 2 {
            self.thin();
        }
    }

    /// Drop every second frame and double the gap between kept ones. Each
    /// thinning halves the strip and halves its frame rate, so a long clip
    /// spreads the same budget over more of its time.
    fn thin(&mut self) {
        let stride = self.frame_bytes();
        let mut frames = Vec::with_capacity(self.frames.len() / 2);
        let mut times = Vec::with_capacity(self.times.len() / 2);
        for (index, frame) in self.frames.chunks_exact(stride).enumerate() {
            if index % 2 == 0 {
                frames.extend_from_slice(frame);
                times.push(self.times[index]);
            }
        }
        self.frames = frames;
        self.times = times;
        self.min_gap *= 2.0;
    }

    fn finish(self) -> VideoThumbnails {
        VideoThumbnails {
            width: self.width,
            height: self.height,
            times: self.times.into(),
            frames: self.frames.into(),
        }
    }
}

/// Decode a video container's picture track into thumbnails.
///
/// `extension` names the container family, `mp4` or `mkv`, the way the
/// sample loader names it for the audio track; Symphonia probes by it.
/// A container with no video track, or one this build cannot decode, is
/// refused by name.
pub fn decode_thumbnails(bytes: &[u8], extension: &str) -> Result<VideoThumbnails, String> {
    // Decoders parse untrusted input on a library thread, so a panic is
    // contained the same refusal as any other decode failure; see
    // `samples::codecs::decode_guarded`.
    let decode = || decode_thumbnails_inner(bytes, extension);
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(decode)) {
        Ok(result) => result,
        Err(payload) => {
            let detail = payload
                .downcast_ref::<&str>()
                .map(|text| (*text).to_owned())
                .or_else(|| payload.downcast_ref::<String>().cloned())
                .unwrap_or_else(|| "panic".to_owned());
            Err(format!("video decoder panicked: {detail}"))
        }
    }
}

fn decode_thumbnails_inner(bytes: &[u8], extension: &str) -> Result<VideoThumbnails, String> {
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
            &FormatOptions::default(),
            &MetadataOptions::default(),
        )
        .map_err(|error| format!("{extension} probe: {error}"))?;
    let mut format = probed.format;
    // Tracks that could carry the picture, with the time base each counts
    // in. A subtitle or chapter track sits among them: its packets sniff
    // as nothing known and are passed over, so a text track listed before
    // the video does not refuse the clip.
    let mut time_bases: std::collections::HashMap<u32, symphonia::core::units::TimeBase> =
        std::collections::HashMap::new();
    for track in format.tracks() {
        let base = track
            .codec_params
            .time_base
            .unwrap_or(symphonia::core::units::TimeBase::new(1, 1_000));
        time_bases.insert(track.id, base);
    }
    let candidates = crate::samples::container_video_track_ids(format.as_ref());
    if candidates.is_empty() {
        return Err(format!("{extension} has no video track"));
    }
    let mut skipped: std::collections::HashSet<u32> = std::collections::HashSet::new();
    let mut strip: Option<Strip> = None;
    let mut decoder: Option<PictureDecoder> = None;
    let mut selected: Option<u32> = None;
    let mut refused: Option<String> = None;
    let mut packets = 0u64;
    while packets < MAX_PICTURE_PACKETS {
        let packet = match format.next_packet() {
            Ok(packet) => packet,
            // The end of the container ends the strip, whatever it holds.
            Err(_) => break,
        };
        packets += 1;
        let track_id = packet.track_id();
        if !candidates.contains(&track_id)
            || skipped.contains(&track_id)
            || selected.is_some_and(|selected| selected != track_id)
        {
            continue;
        }
        let data = &packet.data[..];
        if data.is_empty() {
            continue;
        }
        if decoder.is_none() {
            match sniff_picture_codec(data).decoder(data) {
                Ok(picture) => {
                    selected = Some(track_id);
                    decoder = Some(picture);
                }
                Err(message) => {
                    // A codec this build cannot decode is named once, but a
                    // later track can still be the picture: Matroska files
                    // open with text tracks before video.
                    refused.get_or_insert(message);
                    skipped.insert(track_id);
                    continue;
                }
            }
        }
        // The demuxer counts time in the track's time base; without one a
        // Matroska tick is a millisecond.
        let time_base = time_bases[&track_id];
        let seconds = packet.ts() as f32 * time_base.numer as f32 / time_base.denom as f32;
        let strip = strip.get_or_insert_with(|| Strip {
            width: 0,
            height: 0,
            times: Vec::new(),
            frames: Vec::new(),
            min_gap: 1.0 / MAX_KEPT_FPS,
            last_time: f32::MIN,
            decoded: 0,
            failures: 0,
        });
        // The strip names its size once; a frame that does not match is
        // scaled to it, so a mid-clip size change costs nothing.
        let frame = decoder
            .as_mut()
            .expect("the decoder is chosen before the first frame")
            .decode(data, packet.ts() as i64);
        match frame {
            Ok((frame_width, frame_height, rgba)) => {
                strip.decoded += 1;
                if strip.width == 0 {
                    let (width, height) = thumb_size(frame_width, frame_height);
                    strip.width = width;
                    strip.height = height;
                }
                let scaled =
                    scale_rgba(&rgba, frame_width, frame_height, strip.width, strip.height);
                // A frame out of order, or on the heels of the last kept
                // one, is decoded but not stored.
                if seconds > strip.last_time && seconds - strip.last_time >= strip.min_gap {
                    strip.keep(seconds, &scaled);
                }
            }
            Err(message) => {
                strip.failures += 1;
                if strip.decoded == 0 && strip.times.is_empty() {
                    // Nothing has decoded yet: hold the reason in case
                    // nothing ever does.
                    refused = refused.or(Some(message));
                }
            }
        }
    }
    if let Some(message) = refused
        && strip.as_ref().is_none_or(|strip| strip.times.is_empty())
    {
        return Err(message);
    }
    let strip = strip.ok_or_else(|| format!("{extension} has no video track"))?;
    if strip.times.is_empty() {
        return Err(format!(
            "{extension} decoded no video frames ({} packets failed)",
            strip.failures
        ));
    }
    Ok(strip.finish())
}

#[cfg(test)]
mod tests {
    //! Video containers as picture sources: their first video track's
    //! packets decode to a thinned filmstrip, and a codec this build
    //! cannot decode is named.

    use super::*;

    /// 0.6 s of a moving test pattern at 10 fps, VP8 beside a Vorbis tone.
    const TINY_VP8: &[u8] = include_bytes!("video/testdata/tiny_video_vp8.webm");
    /// The same pattern as Motion JPEG in a QuickTime container.
    const TINY_MJPEG: &[u8] = include_bytes!("video/testdata/tiny_video_mjpeg.mov");
    /// An H.264 picture beside a tone: the audio plays, the picture cannot.
    const TINY_H264: &[u8] = include_bytes!("samples/testdata/tiny_video.mp4");
    /// A Matroska with no video track at all.
    const AUDIO_ONLY: &[u8] = include_bytes!("video/testdata/tiny_audio_only.webm");
    /// A subtitle track listed before the VP8 picture.
    const TINY_SUBS: &[u8] = include_bytes!("video/testdata/tiny_video_subs.mkv");

    #[test]
    fn a_vp8_clip_decodes_a_filmstrip_that_moves() {
        let strip = decode_thumbnails(TINY_VP8, "mkv").expect("the picture must decode");
        assert_eq!((strip.width(), strip.height()), (64, 36));
        assert_eq!(strip.frame_count(), 6, "every frame fits the budget");
        assert!((strip.duration() - 0.5).abs() < 0.05, "ten frames a second");
        let first = strip.frame_at(0.0).expect("the first frame");
        let last = strip.frame_at(0.6).expect("the last frame holds");
        assert_ne!(first, last, "the test pattern moves");
        // Between two kept frames the earlier one shows; before the first,
        // the first does.
        assert_eq!(strip.frame_at(0.14), strip.frame_at(0.1));
        assert_eq!(strip.frame_at(0.0), strip.frame_at(0.04));
        assert!(
            strip.frame_at(-1.0).is_none(),
            "nothing shows before the clip"
        );
    }

    #[test]
    fn a_motion_jpeg_clip_decodes_the_same_strip() {
        let strip = decode_thumbnails(TINY_MJPEG, "mp4").expect("the picture must decode");
        assert_eq!((strip.width(), strip.height()), (64, 36));
        assert_eq!(strip.frame_count(), 6);
        assert_ne!(
            strip.frame_at(0.0).expect("the first frame"),
            strip.frame_at(0.5).expect("the last frame"),
            "the test pattern moves"
        );
    }

    #[test]
    fn an_h264_picture_is_refused_by_name() {
        let error = decode_thumbnails(TINY_H264, "mp4").expect_err("no native H.264 decoder ships");
        assert!(
            error.contains("H.264") && error.contains("re-encode"),
            "names what to do: {error}"
        );
    }

    /// A text track listed before the video is passed over, not mistaken
    /// for the picture: its packets sniff as nothing this build knows.
    #[test]
    fn a_subtitle_track_before_the_video_is_not_the_picture() {
        let strip = decode_thumbnails(TINY_SUBS, "mkv").expect("the picture must decode");
        assert_eq!((strip.width(), strip.height()), (64, 36));
        assert_eq!(strip.frame_count(), 4, "0.4 s at ten frames a second");
        assert_ne!(
            strip.frame_at(0.0).expect("the first frame"),
            strip.frame_at(0.3).expect("the last frame"),
            "the test pattern moves"
        );
    }

    /// A VP8 keyframe's tag sizes the decoder's planes before any pixel
    /// is decoded, so an edge past the decode limit is refused by name and
    /// never reaches the allocator.
    #[test]
    fn a_lying_vp8_keyframe_edge_is_refused() {
        let mut keyframe = [0u8; 10];
        // Keyframe flag clear, the VP8 start code, then 16383x16383 in the
        // tag's 14-bit sides-less-one.
        keyframe[3..6].copy_from_slice(&[0x9D, 0x01, 0x2A]);
        keyframe[6..8].copy_from_slice(&0x3FFE_u16.to_le_bytes());
        keyframe[8..10].copy_from_slice(&0x3FFE_u16.to_le_bytes());
        assert_eq!(vp8_frame_size(&keyframe), Some((16_383, 16_383)));
        let refused = sniff_picture_codec(&keyframe).decoder(&keyframe);
        assert!(
            refused
                .err()
                .is_some_and(|message| message.contains("4096")),
            "the edge is refused by name"
        );
        assert_eq!(vp8_frame_size(&[0; 9]), None, "a short tag is no tag");
    }

    #[test]
    fn a_built_strip_refuses_its_malformed_frames() {
        let red = [255, 0, 0, 255].repeat(4);
        assert!(
            VideoThumbnails::from_frames(0, 2, vec![(0.0, red.clone())]).is_err(),
            "no edge is no picture"
        );
        assert!(
            VideoThumbnails::from_frames(2, 2, Vec::new()).is_err(),
            "no frames is no strip"
        );
        assert!(
            VideoThumbnails::from_frames(2, 2, vec![(0.0, red.clone()), (0.0, red.clone())])
                .is_err(),
            "times ascend"
        );
        assert!(
            VideoThumbnails::from_frames(2, 2, vec![(0.0, vec![0; 15])]).is_err(),
            "a frame is width * height * 4 bytes"
        );
        assert!(
            VideoThumbnails::from_frames(2, 2, vec![(f32::NAN, red)]).is_err(),
            "times are finite"
        );
    }

    #[test]
    fn a_container_without_a_video_track_says_so() {
        let error = decode_thumbnails(AUDIO_ONLY, "mkv").expect_err("there is no picture");
        assert!(
            error.contains("no video track"),
            "says what is missing: {error}"
        );
    }

    #[test]
    fn a_large_frame_scales_into_the_thumb_edge() {
        assert_eq!(thumb_size(1920, 1080), (160, 90));
        assert_eq!(thumb_size(1080, 1920), (90, 160));
        assert_eq!(thumb_size(64, 36), (64, 36), "a small frame keeps its size");
        assert_eq!(thumb_size(160, 160), (160, 160));
    }

    #[test]
    fn a_huge_frame_is_averaged_not_sampled() {
        // 8 red pixels and 8 blue ones under one destination pixel must
        // average, not pick a side.
        let source = [255, 0, 0, 255, 0, 0, 255, 255];
        let scaled = scale_rgba(&source, 2, 1, 1, 1);
        assert_eq!(scaled, vec![127, 0, 127, 255]);
    }

    #[test]
    fn thinning_keeps_every_second_frame_and_its_times() {
        let strip = decode_thumbnails(TINY_VP8, "mkv").expect("the picture must decode");
        let mut thin = Strip {
            width: strip.width,
            height: strip.height,
            times: strip.times.to_vec(),
            frames: strip.frames.to_vec(),
            min_gap: 1.0 / MAX_KEPT_FPS,
            last_time: strip.duration(),
            decoded: strip.frame_count(),
            failures: 0,
        };
        thin.thin();
        assert_eq!(thin.times.len(), 3, "half of six");
        assert_eq!(thin.times[0], strip.times[0]);
        assert_eq!(thin.times[1], strip.times[2]);
        assert_eq!(thin.times[2], strip.times[4]);
        assert_eq!(thin.frames.len(), thin.frame_bytes() * 3);
        assert!((thin.min_gap - 2.0 / MAX_KEPT_FPS).abs() < f32::EPSILON);
    }
}
