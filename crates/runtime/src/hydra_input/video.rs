//! Looping video for Hydra inputs.
//!
//! A video plays in a loop, without sound, on its input's own thread, as
//! Hydra's own videos do. WebM and Matroska files with VP8 or VP9 video decode
//! through OxiMedia. Animated GIF, PNG and WebP files decode through `image`.
//! Every decoder is pure Rust. A frame header that names a picture over the
//! input limits is refused before a decoder allocates for it.
//!
//! MP4 is refused. OxiMedia's MP4 demuxer sizes its sample tables from header
//! counts that the file size does not bound, so a few hundred bytes could
//! ask for gigabytes. Its Matroska demuxer reads only the bytes the file has.

use super::*;

use bytes::Bytes;
use image::codecs::gif::GifDecoder;
use image::codecs::png::PngDecoder;
use image::codecs::webp::WebPDecoder;
use image::{AnimationDecoder, ImageDecoder, ImageFormat};
use oximedia_codec::traits::{DecoderConfig, VideoDecoder};
use oximedia_container::demux::{Demuxer, MatroskaDemuxer};
use oximedia_container::{ContainerFormat, StreamInfo};
use oximedia_core::{CodecId, MediaType};
use oximedia_io::MemorySource;

/// The media types a video request accepts, as an `Accept` header lists them.
pub(crate) const VIDEO_MEDIA_TYPES: &[&str] = &[
    "video/webm",
    "video/x-matroska",
    "image/gif",
    "image/apng",
    "image/png",
    "image/webp",
];

/// What a refused file should become, as a command a musician can run.
const CONVERT_HINT: &str =
    "convert it with: ffmpeg -i input.mp4 -c:v libvpx-vp9 -pix_fmt yuv420p -an output.webm";

/// Browsers show an animation frame with a delay of 10 ms or less for 100 ms.
const MIN_FRAME_DELAY: Duration = Duration::from_millis(10);
const SHORT_FRAME_DELAY: Duration = Duration::from_millis(100);
/// The frame time a container gives no timestamps for: 30 frames a second.
const DEFAULT_FRAME_STEP: Duration = Duration::from_micros(33_333);
/// The longest gap between two frames. A later timestamp is refused rather
/// than freezing the picture without a word.
const MAX_FRAME_GAP: Duration = Duration::from_secs(10);
/// How long a wait sleeps before it looks at the cancel flag again.
const CANCEL_POLL: Duration = Duration::from_millis(20);

/// Play `bytes` in a loop until `cancel` is set or `publish` reports that a
/// newer request owns the input. `publish` receives each frame as RGBA.
pub(super) fn play(
    bytes: Vec<u8>,
    cancel: &AtomicBool,
    publish: &mut dyn FnMut(u32, u32, Vec<u8>) -> Result<bool, String>,
) -> Result<(), String> {
    let bytes = Bytes::from(bytes);
    if let Ok(format) = image::guess_format(&bytes)
        && matches!(
            format,
            ImageFormat::Gif | ImageFormat::Png | ImageFormat::WebP
        )
    {
        return play_animation(&bytes, format, cancel, publish);
    }
    play_container(&bytes, cancel, publish)
}

/// The outcome of one pass through a video.
enum Pass {
    /// Every frame was shown. The next pass starts after `length`.
    Done { frames: usize, length: Duration },
    /// The cancel flag was set, or a newer request owns the input.
    Stopped,
}

fn play_container(
    bytes: &Bytes,
    cancel: &AtomicBool,
    publish: &mut dyn FnMut(u32, u32, Vec<u8>) -> Result<bool, String>,
) -> Result<(), String> {
    loop {
        let started = Instant::now();
        match container_pass(bytes, started, cancel, publish)? {
            Pass::Stopped => return Ok(()),
            Pass::Done { frames: 0, .. } => {
                return Err("Hydra video has no frames that decode".into());
            }
            Pass::Done { frames: 1, .. } => return hold(cancel),
            Pass::Done { length, .. } => {
                if !wait_until(started, length, cancel) {
                    return Ok(());
                }
            }
        }
    }
}

fn container_pass(
    bytes: &Bytes,
    started: Instant,
    cancel: &AtomicBool,
    publish: &mut dyn FnMut(u32, u32, Vec<u8>) -> Result<bool, String>,
) -> Result<Pass, String> {
    let mut demuxer = Demux::open(bytes)?;
    let stream = demuxer.video_stream()?;
    let mut decoder = decoder_for(stream.codec)?;
    let timebase = (stream.timebase.num, stream.timebase.den);
    let undecodable = |error: oximedia_codec::CodecError| {
        format!("Hydra video could not be decoded ({error}); {CONVERT_HINT}")
    };
    let mut first_pts = None;
    let mut frames = 0;
    let mut last = Duration::ZERO;
    let mut step = DEFAULT_FRAME_STEP;
    while let Some(packet) = demuxer.next_packet()? {
        if cancel.load(Ordering::Acquire) {
            return Ok(Pass::Stopped);
        }
        if packet.stream_index != stream.index {
            continue;
        }
        check_frame_sizes(stream.codec, &packet.data)?;
        let pts = packet.timestamp.pts;
        decoder
            .send_packet(&packet.data, pts)
            .map_err(undecodable)?;
        while let Some(frame) = decoder.receive_frame().map_err(undecodable)? {
            let first = *first_pts.get_or_insert(pts);
            let at = pts_offset(pts.saturating_sub(first), timebase).unwrap_or(last + step);
            if at > last + MAX_FRAME_GAP {
                return Err(format!(
                    "Hydra video has a gap of more than {} seconds between two frames",
                    MAX_FRAME_GAP.as_secs()
                ));
            }
            if at > last {
                step = at - last;
                last = at;
            }
            if !wait_until(started, at, cancel) {
                return Ok(Pass::Stopped);
            }
            let (width, height, rgba) = raw_frame_rgba(&frame).map_err(|_| {
                format!(
                    "Hydra video frames are {:?}; only 8-bit 4:2:0 plays, {CONVERT_HINT}",
                    frame.format
                )
            })?;
            if !publish(width, height, rgba)? {
                return Ok(Pass::Stopped);
            }
            frames += 1;
        }
    }
    Ok(Pass::Done {
        frames,
        length: last + step,
    })
}

/// A Matroska demuxer over the whole file in memory. Its API is async; the
/// input worker has no other work, so it blocks on each call.
struct Demux(MatroskaDemuxer<MemorySource>);

impl Demux {
    fn open(bytes: &Bytes) -> Result<Self, String> {
        let refused = || format!("Hydra video is not WebM or Matroska; {CONVERT_HINT}");
        let format = oximedia_container::probe_format(bytes)
            .map_err(|_| refused())?
            .format;
        if !matches!(format, ContainerFormat::Matroska | ContainerFormat::WebM) {
            return Err(refused());
        }
        let mut demuxer = MatroskaDemuxer::new(MemorySource::new(bytes.clone()));
        pollster::block_on(demuxer.probe()).map_err(|error| {
            format!(
                "Hydra video could not be read ({error}); only VP8 and VP9 play, {CONVERT_HINT}"
            )
        })?;
        Ok(Self(demuxer))
    }

    fn streams(&self) -> &[StreamInfo] {
        self.0.streams()
    }

    /// The first video stream, checked against the input limits.
    fn video_stream(&self) -> Result<StreamInfo, String> {
        let Some(stream) = self
            .streams()
            .iter()
            .find(|stream| stream.media_type == MediaType::Video)
        else {
            return Err("Hydra video has no video stream".into());
        };
        if let (Some(width), Some(height)) = (stream.codec_params.width, stream.codec_params.height)
        {
            check_size(width, height)?;
        }
        Ok(stream.clone())
    }

    fn next_packet(&mut self) -> Result<Option<oximedia_container::Packet>, String> {
        match pollster::block_on(self.0.read_packet()) {
            Ok(packet) => Ok(Some(packet)),
            Err(error) if error.is_eof() => Ok(None),
            Err(error) => Err(format!("Hydra video could not be read: {error}")),
        }
    }
}

fn decoder_for(codec: CodecId) -> Result<Box<dyn VideoDecoder>, String> {
    let config = DecoderConfig {
        codec,
        ..DecoderConfig::default()
    };
    let decoder: Box<dyn VideoDecoder> = match codec {
        CodecId::Vp8 => {
            Box::new(oximedia_codec::vp8::Vp8Decoder::new(config).map_err(decoder_error)?)
        }
        CodecId::Vp9 => {
            Box::new(oximedia_codec::vp9::Vp9Decoder::new(config).map_err(decoder_error)?)
        }
        other => {
            return Err(format!(
                "Hydra video uses {other}; only VP8 and VP9 play, {CONVERT_HINT}"
            ));
        }
    };
    Ok(decoder)
}

fn decoder_error(error: impl std::fmt::Display) -> String {
    format!("Hydra video decoder could not start: {error}")
}

/// `ticks` of a `num / den` second timebase, as a duration.
fn pts_offset(ticks: i64, (num, den): (i64, i64)) -> Option<Duration> {
    if ticks < 0 || num <= 0 || den <= 0 {
        return None;
    }
    let nanos = i128::from(ticks)
        .checked_mul(i128::from(num))?
        .checked_mul(1_000_000_000)?
        / i128::from(den);
    u64::try_from(nanos).ok().map(Duration::from_nanos)
}

fn play_animation(
    bytes: &Bytes,
    format: ImageFormat,
    cancel: &AtomicBool,
    publish: &mut dyn FnMut(u32, u32, Vec<u8>) -> Result<bool, String>,
) -> Result<(), String> {
    loop {
        let started = Instant::now();
        let mut at = Duration::ZERO;
        let mut frames = 0;
        for frame in animation_frames(bytes, format)? {
            let frame = frame
                .map_err(|error| format!("Hydra video frame could not be decoded: {error}"))?;
            if !wait_until(started, at, cancel) {
                return Ok(());
            }
            let (numerator, denominator) = frame.delay().numer_denom_ms();
            let delay =
                Duration::from_micros(u64::from(numerator) * 1_000 / u64::from(denominator.max(1)));
            at += if delay <= MIN_FRAME_DELAY {
                SHORT_FRAME_DELAY
            } else {
                delay
            };
            let buffer = frame.into_buffer();
            let (width, height) = buffer.dimensions();
            if !publish(width, height, buffer.into_raw())? {
                return Ok(());
            }
            frames += 1;
        }
        match frames {
            0 => return Err("Hydra video has no frames that decode".into()),
            1 => return hold(cancel),
            _ => {
                if !wait_until(started, at, cancel) {
                    return Ok(());
                }
            }
        }
    }
}

/// The frames of an animated GIF, PNG or WebP. A file that is not animated
/// is one frame.
fn animation_frames(bytes: &[u8], format: ImageFormat) -> Result<image::Frames<'_>, String> {
    let decode = |error: image::ImageError| format!("Hydra video could not be decoded: {error}");
    let reader = Cursor::new(bytes);
    let frames = match format {
        ImageFormat::Gif => {
            let mut decoder = GifDecoder::new(reader).map_err(decode)?;
            limit_animation(&mut decoder)?;
            decoder.into_frames()
        }
        ImageFormat::Png => {
            let mut decoder = PngDecoder::new(reader).map_err(decode)?;
            limit_animation(&mut decoder)?;
            if !decoder.is_apng().map_err(decode)? {
                return single_frame(bytes);
            }
            decoder.apng().map_err(decode)?.into_frames()
        }
        ImageFormat::WebP => {
            let mut decoder = WebPDecoder::new(reader).map_err(decode)?;
            limit_animation(&mut decoder)?;
            if !decoder.has_animation() {
                return single_frame(bytes);
            }
            decoder.into_frames()
        }
        _ => {
            return Err(format!(
                "Hydra video is not WebM or Matroska; {CONVERT_HINT}"
            ));
        }
    };
    Ok(frames)
}

/// A still image as an animation of one frame.
fn single_frame(bytes: &[u8]) -> Result<image::Frames<'static>, String> {
    let (width, height, rgba) = decode_image(bytes)?;
    let buffer = image::RgbaImage::from_raw(width, height, rgba)
        .ok_or_else(|| "Hydra image could not be decoded".to_owned())?;
    Ok(image::Frames::new(Box::new(std::iter::once(Ok(
        image::Frame::new(buffer),
    )))))
}

/// The input limits, applied before the first frame is decoded.
fn limit_animation(decoder: &mut impl ImageDecoder) -> Result<(), String> {
    let (width, height) = decoder.dimensions();
    check_size(width, height)?;
    let mut limits = Limits::default();
    limits.max_image_width = Some(MAX_IMAGE_EDGE);
    limits.max_image_height = Some(MAX_IMAGE_EDGE);
    limits.max_alloc = Some(MAX_DECODE_ALLOC);
    decoder
        .set_limits(limits)
        .map_err(|error| format!("Hydra video is over the input limits: {error}"))
}

fn check_size(width: u32, height: u32) -> Result<(), String> {
    let pixels = u64::from(width) * u64::from(height);
    if width == 0
        || height == 0
        || width > MAX_IMAGE_EDGE
        || height > MAX_IMAGE_EDGE
        || pixels > MAX_IMAGE_PIXELS
    {
        return Err(format!(
            "Hydra video frames are {width}x{height}; inputs hold at most {MAX_IMAGE_EDGE} pixels on an edge and {MAX_IMAGE_PIXELS} pixels"
        ));
    }
    Ok(())
}

/// Refuse a packet whose frame header names a picture over the input limits.
/// OxiMedia's decoders allocate whatever size a header names, so each header
/// is read with the parser that the decoder itself uses.
fn check_frame_sizes(codec: CodecId, packet: &[u8]) -> Result<(), String> {
    let unreadable =
        |error: oximedia_codec::CodecError| format!("Hydra video could not be decoded: {error}");
    match codec {
        CodecId::Vp8 => {
            let header = oximedia_codec::vp8::FrameHeader::parse(packet).map_err(unreadable)?;
            if header.frame_type == oximedia_codec::vp8::FrameType::Key {
                check_size(u32::from(header.width), u32::from(header.height))?;
            }
        }
        CodecId::Vp9 => {
            let superframe = oximedia_codec::vp9::Superframe::parse(packet).map_err(unreadable)?;
            for frame in &superframe.frames {
                // Without the decoder's reference sizes, a frame that takes
                // its size from a reference does not parse here. That size
                // was checked with its own frame. A header that does not
                // parse for another reason fails in the decoder too, before
                // it allocates.
                if let Ok(header) = oximedia_codec::vp9::UncompressedHeader::parse(frame)
                    && !header.show_existing_frame
                {
                    check_size(header.width, header.height)?;
                }
            }
        }
        _ => {}
    }
    Ok(())
}

/// Keep the one frame on screen until the input is released.
fn hold(cancel: &AtomicBool) -> Result<(), String> {
    while !cancel.load(Ordering::Acquire) {
        std::thread::sleep(CANCEL_POLL);
    }
    Ok(())
}

/// Sleep until `offset` after `started`. False when the input was released
/// first. An offset past the clock's range waits for the release.
fn wait_until(started: Instant, offset: Duration, cancel: &AtomicBool) -> bool {
    let Some(deadline) = started.checked_add(offset) else {
        let _ = hold(cancel);
        return false;
    };
    loop {
        if cancel.load(Ordering::Acquire) {
            return false;
        }
        let now = Instant::now();
        if now >= deadline {
            return true;
        }
        std::thread::sleep((deadline - now).min(CANCEL_POLL));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The first packet of a clip and its codec, past the container's own
    /// size check.
    fn first_packet(clip: &[u8]) -> (CodecId, Vec<u8>) {
        let mut demuxer = Demux::open(&Bytes::copy_from_slice(clip)).expect("a clip");
        let codec = demuxer.streams()[0].codec;
        let packet = demuxer.next_packet().expect("readable").expect("a packet");
        (codec, packet.data.to_vec())
    }

    /// Two clips 4096 pixels wide, VP9 and VP8, are refused from their key
    /// frame header, before a decoder sees the packet.
    #[test]
    fn frame_headers_over_the_limits_are_refused_before_decoding() {
        let wide: [&[u8]; 2] = [
            include_bytes!("testdata/wide_vp9.webm"),
            include_bytes!("testdata/wide_vp8.webm"),
        ];
        for clip in wide {
            let (codec, packet) = first_packet(clip);
            let error = check_frame_sizes(codec, &packet).expect_err("4096 pixels wide");
            assert!(error.contains("4096x16"), "{codec:?}: {error}");
        }
        let fitting: [&[u8]; 2] = [
            include_bytes!("testdata/red_vp9.webm"),
            include_bytes!("testdata/blue_vp8.webm"),
        ];
        for clip in fitting {
            let (codec, packet) = first_packet(clip);
            assert_eq!(check_frame_sizes(codec, &packet), Ok(()), "{codec:?}");
        }
        // A key frame cut short, and a superframe whose sizes do not add up
        // to its payload, are refused rather than passed on.
        assert!(check_frame_sizes(CodecId::Vp8, &[0, 0, 0, 0x9d, 0x01, 0x2a]).is_err());
        let marker = 0b1100_0001;
        let packet = [1, 2, 3, 4, 5, 6, marker, 3, 2, marker];
        assert!(check_frame_sizes(CodecId::Vp9, &packet).is_err());
    }

    #[test]
    fn container_timestamps_become_offsets() {
        assert_eq!(
            pts_offset(1500, (1, 1000)),
            Some(Duration::from_millis(1500))
        );
        assert_eq!(pts_offset(-1, (1, 1000)), None);
        assert_eq!(pts_offset(1, (1, 0)), None);
        assert_eq!(pts_offset(i64::MAX, (i64::MAX, 1)), None, "no overflow");
    }

    /// A two-frame GIF loops: the first frame comes back after the second.
    #[test]
    fn an_animated_gif_loops_until_it_is_released() {
        use image::codecs::gif::{GifEncoder, Repeat};
        let mut bytes = Vec::new();
        {
            let mut encoder = GifEncoder::new(&mut bytes);
            encoder.set_repeat(Repeat::Infinite).expect("repeat");
            for shade in [0u8, 255] {
                let buffer = image::RgbaImage::from_pixel(4, 2, image::Rgba([shade, 0, 0, 255]));
                let frame = image::Frame::from_parts(
                    buffer,
                    0,
                    0,
                    image::Delay::from_numer_denom_ms(20, 1),
                );
                encoder.encode_frame(frame).expect("frame");
            }
        }
        let cancel = AtomicBool::new(false);
        let mut shown = Vec::new();
        let result = play(bytes, &cancel, &mut |width, height, rgba| {
            assert_eq!((width, height), (4, 2));
            shown.push(rgba[0]);
            if shown.len() == 3 {
                cancel.store(true, Ordering::Release);
            }
            Ok(true)
        });
        assert_eq!(result, Ok(()));
        assert_eq!(shown, [0, 255, 0]);
    }

    /// Two WebM clips, 32x16, three frames at 10 a second: VP9 red and VP8
    /// blue. Each plays in its colour and starts again after its last frame.
    #[test]
    fn webm_clips_play_their_frames_and_loop() {
        let clips: [(&[u8], [u8; 3]); 2] = [
            (include_bytes!("testdata/red_vp9.webm"), [255, 0, 0]),
            (include_bytes!("testdata/blue_vp8.webm"), [0, 0, 255]),
        ];
        for (clip, colour) in clips {
            let cancel = AtomicBool::new(false);
            let mut frames = 0;
            let result = play(clip.to_vec(), &cancel, &mut |width, height, rgba| {
                assert_eq!((width, height), (32, 16));
                for (channel, expected) in rgba[..3].iter().zip(colour) {
                    assert!(channel.abs_diff(expected) < 40, "{:?}", &rgba[..4]);
                }
                frames += 1;
                if frames == 4 {
                    cancel.store(true, Ordering::Release);
                }
                Ok(true)
            });
            assert_eq!(result, Ok(()));
            assert_eq!(frames, 4, "three frames, then the first again");
        }
    }

    /// MP4 is refused before its demuxer reads the sample tables, even with
    /// VP9 inside: those tables can ask for any amount of memory.
    #[test]
    fn an_mp4_is_refused_before_it_is_demuxed() {
        let cancel = AtomicBool::new(false);
        let clip = include_bytes!("testdata/red_vp9.mp4").to_vec();
        let error = play(clip, &cancel, &mut |_, _, _| Ok(true)).expect_err("refused");
        assert!(error.contains("not WebM or Matroska"), "{error}");
        assert!(error.contains("ffmpeg"), "{error}");
    }

    #[test]
    fn a_file_that_is_not_a_video_says_how_to_convert_it() {
        let cancel = AtomicBool::new(false);
        let error = play(b"not a video at all".to_vec(), &cancel, &mut |_, _, _| {
            Ok(true)
        })
        .expect_err("refused");
        assert!(error.contains("ffmpeg"), "{error}");
    }
}
