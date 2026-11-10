//! A video container's picture, decoded once into bounded thumbnails.
//!
//! A video sample's sound is its audio track ([`crate::samples`]). This
//! module is its picture. Symphonia demuxes the first video track, and a
//! pure-Rust decoder turns its packets into pictures: H.264, HEVC, AV1,
//! VP9, VP8 or Motion JPEG. The kept frames fit a fixed byte budget, so a
//! painter can follow the clip where the sound is without holding its full
//! frames. A long clip spreads that budget over its length and decodes only
//! the keyframe groups its kept frames fall in.

mod av1;
mod codec;
mod matroska;
mod mp4;
mod picture;

use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use symphonia::core::formats::FormatOptions;
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;
use symphonia::core::units::TimeBase;

use codec::{Decoder, Refusal, Track};
use picture::Picture;

/// The longest edge a thumbnail keeps. The inline painter is small, and a
/// larger picture would only cost memory.
const THUMB_EDGE: u32 = 160;

/// RGBA bytes one clip's thumbnails may hold between them. A clip with
/// more frames than that thins to fewer, evenly spaced ones.
const CLIP_BUDGET_BYTES: usize = 12 << 20;

/// Picture packets one clip may carry before the decode stops early: 72
/// minutes at 60 frames a second. The decode time bounds the work, and
/// this bounds a container of endless tiny packets. The strip covers the
/// decoded prefix.
const MAX_PICTURE_PACKETS: u64 = 1 << 18;

/// The fastest thumbnail rate kept. Frames closer together than this are
/// decoded, because a later frame needs them, but not stored.
const MAX_KEPT_FPS: f32 = 24.0;

/// Bytes one keyframe group may buffer before it decodes as it arrives. A
/// stream whose keyframes go unrecognised still decodes, frame by frame.
const MAX_GROUP_BYTES: usize = 32 << 20;

/// The time a clip's picture aims to take. A keyframe group decodes whole
/// only while the decode keeps that pace, so a long clip falls back to
/// one picture a group rather than running out of time.
const TARGET_DECODE_TIME: Duration = Duration::from_secs(15);

/// The time one clip's picture may take. Past it, the strip keeps the
/// frames decoded so far.
const MAX_DECODE_TIME: Duration = Duration::from_secs(60);

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
    /// The clip's length in seconds, at least the last kept frame's time.
    length: f32,
}

impl VideoThumbnails {
    /// Build a strip from kept frames: presentation times in ascending
    /// order, each frame `width * height * 4` RGBA bytes. The library
    /// builds these by decoding a container. A host that decodes its own
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
            length: last,
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

    /// The clip's length in seconds. A decoded clip runs a frame past its
    /// last picture, however few frames the strip kept, or as long as its
    /// container says when the decode stopped early. A strip built from
    /// frames runs to its last.
    pub fn duration(&self) -> f32 {
        self.length
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
    /// A decode is running on a library thread. Ask again on a later poll.
    Decoding,
    Ready(Arc<VideoThumbnails>),
    /// Why there is no strip. Names the codec when the build cannot decode it.
    Failed(String),
    /// The name is a sound with no picture: a sound file, a synth, or a
    /// container with no video track.
    NotVideo,
    /// The name is a bank the note picks its file from, with these keys
    /// as MIDI numbers. Each key is a clip of its own: ask for
    /// `name note:key`, the key the note picks by
    /// [`crate::samples::closest_note_key`].
    ByNote(Vec<f64>),
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
    /// Seconds between kept frames. The clip's length sets it, and it
    /// doubles every time the strip thins.
    min_gap: f32,
    last_time: f32,
    /// The clip's length, when the container gives it.
    duration: Option<f32>,
    decoded: usize,
    failures: usize,
}

impl Strip {
    fn new(duration: Option<f32>) -> Self {
        let mut strip = Self {
            width: 0,
            height: 0,
            times: Vec::new(),
            frames: Vec::new(),
            min_gap: 1.0 / MAX_KEPT_FPS,
            last_time: f32::MIN,
            duration: duration.filter(|duration| duration.is_finite() && *duration > 0.0),
            decoded: 0,
            failures: 0,
        };
        // A 16:9 thumbnail until the first frame gives the real shape.
        let (width, height) = thumb_size(1920, 1080);
        strip.plan(width as usize * height as usize * 4);
        strip
    }

    fn frame_bytes(&self) -> usize {
        self.width as usize * self.height as usize * 4
    }

    /// Space the kept frames so the clip's length fits in nine tenths of
    /// half the budget, the most a strip holds before it thins. The margin
    /// absorbs frame times that fall late of the plan.
    fn plan(&mut self, frame_bytes: usize) {
        if let Some(duration) = self.duration {
            let frames = (CLIP_BUDGET_BYTES / 2 / frame_bytes.max(1) * 9 / 10).max(1);
            self.min_gap = self.min_gap.max(duration / frames as f32);
        }
    }

    /// Whether a frame shown at `time` is due.
    fn wants(&self, time: f32) -> bool {
        self.times.is_empty() || (time > self.last_time && time >= self.next_due())
    }

    /// When the next kept frame is due: at the start of the next slot of
    /// `min_gap` after the last kept frame's. A slot, not a gap from the
    /// last frame, so a 30 fps clip keeps 24 frames a second, not 15. Each
    /// slot starts a tenth early, so a frame time rounded to the
    /// millisecond still lands in its own slot.
    fn next_due(&self) -> f32 {
        ((self.last_time / self.min_gap + 0.1).floor() + 0.9) * self.min_gap
    }

    /// The kept frames due in a keyframe group shown from `start` until
    /// `end`.
    fn due_between(&self, start: f32, end: f32) -> usize {
        let first = if self.times.is_empty() {
            start
        } else {
            self.next_due().max(start)
        };
        if first >= end {
            0
        } else {
            ((end - first) / self.min_gap).ceil() as usize
        }
    }

    /// A decoded picture shown at `time`. Only a due one is scaled and
    /// kept. `force` keeps a picture newer than the last kept one even
    /// when it is early: a keyframe that stands for its whole group.
    fn offer(&mut self, time: f32, picture: &Picture<'_>, force: bool) -> Result<(), String> {
        self.decoded += 1;
        if self.width == 0 {
            let (width, height) = picture.size();
            (self.width, self.height) = thumb_size(width, height);
            self.plan(self.frame_bytes());
        }
        if !(self.wants(time) || (force && time > self.last_time)) {
            return Ok(());
        }
        let thumbnail = picture.thumbnail(self.width, self.height)?;
        self.keep(time, &thumbnail);
        Ok(())
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

    /// The strip of a clip `length` seconds long.
    fn finish(self, length: f32) -> VideoThumbnails {
        let last = self.times.last().copied().unwrap_or(0.0);
        VideoThumbnails {
            width: self.width,
            height: self.height,
            times: self.times.into(),
            frames: self.frames.into(),
            length: length.max(last),
        }
    }
}

/// An open decoder and the times of the packets it was given. Pictures
/// leave a decoder in display order, and a packet's time is when its own
/// picture shows, so each picture takes the earliest time still waiting.
/// That holds for Matroska, which times packets as shown, and for MP4,
/// whose decode times Symphonia gives in decode order.
struct Session {
    decoder: Decoder,
    pending: BinaryHeap<Reverse<u64>>,
    /// The session began at an HEVC CRA, so its decoder drops the RASL
    /// pictures until the next keyframe.
    drops_rasl: bool,
    /// No packet has gone in yet: the next is the keyframe it opened at.
    fresh: bool,
}

/// The picture track's packets on their way to the strip, one keyframe
/// group at a time. A group is buffered until the next keyframe bounds it.
/// Then it is skipped when no kept frame falls in it, reduced to its
/// keyframe when one does, and decoded whole when several do and the
/// decode keeps pace.
struct Clip {
    track: Track,
    time_base: TimeBase,
    started: Instant,
    session: Option<Session>,
    group: Vec<(u64, Box<[u8]>)>,
    group_bytes: usize,
    /// Set when the group outgrew its buffer: the rest of it decodes as
    /// it arrives.
    streaming: bool,
    strip: Strip,
    /// The earliest and latest packet times, and how many packets came.
    packet_times: (u64, u64, u64),
    /// Why the first failed packet failed, in case none ever decodes.
    first_failure: Option<String>,
}

impl Clip {
    fn new(track: Track, time_base: TimeBase, duration: Option<f32>) -> Self {
        Self {
            track,
            time_base,
            started: Instant::now(),
            session: None,
            group: Vec::new(),
            group_bytes: 0,
            streaming: false,
            strip: Strip::new(duration),
            packet_times: (u64::MAX, 0, 0),
            first_failure: None,
        }
    }

    fn push(&mut self, ts: u64, data: Box<[u8]>) {
        let (first, latest, count) = self.packet_times;
        self.packet_times = (first.min(ts), latest.max(ts), count + 1);
        if self.track.is_keyframe(&data) && (self.streaming || !self.group.is_empty()) {
            self.close_group(seconds(ts, self.time_base));
        }
        if self.streaming {
            self.decode(ts, &data, false);
            return;
        }
        self.group_bytes += data.len();
        self.group.push((ts, data));
        if self.group_bytes > MAX_GROUP_BYTES {
            self.streaming = true;
            self.group_bytes = 0;
            for (ts, data) in std::mem::take(&mut self.group) {
                self.decode(ts, &data, false);
            }
        }
    }

    /// Decode what the buffered group is worth, now that `end` bounds it.
    fn close_group(&mut self, end: f32) {
        let group = std::mem::take(&mut self.group);
        self.group_bytes = 0;
        if std::mem::take(&mut self.streaming) {
            return;
        }
        let Some((first, _)) = group.first() else {
            return;
        };
        let start = seconds(*first, self.time_base);
        let due = self.strip.due_between(start, end);
        let whole = due > 1 && on_pace(self.started.elapsed(), start, self.strip.duration);
        match (due, whole) {
            (0, _) => self.end_session(false),
            (_, false) => {
                // The keyframe's picture stands for the group. A fresh
                // decoder starts from it, and the rest is never decoded.
                // One picture needs no thread pool made and joined for it.
                self.end_session(false);
                let (ts, data) = &group[0];
                self.open_session(data, 1);
                self.decode(*ts, data, true);
                self.end_session(true);
            }
            (_, true) => {
                for (ts, data) in &group {
                    self.decode(*ts, data, false);
                }
            }
        }
    }

    /// Open a session of up to `threads` decode threads, starting at the
    /// keyframe `data`.
    fn open_session(&mut self, data: &[u8], threads: usize) {
        match self.track.open(threads) {
            Ok(decoder) => {
                self.session = Some(Session {
                    decoder,
                    pending: BinaryHeap::new(),
                    drops_rasl: self.track.is_cra(data),
                    fresh: true,
                });
            }
            Err(message) => {
                self.strip.failures += 1;
                self.first_failure.get_or_insert(message);
            }
        }
    }

    fn decode(&mut self, ts: u64, data: &[u8], force: bool) {
        if self.session.is_none() {
            self.open_session(data, codec::decode_threads());
        }
        let Some(Session {
            decoder,
            pending,
            drops_rasl,
            fresh,
        }) = self.session.as_mut()
        else {
            return;
        };
        // A later keyframe's leading pictures have what they lean on.
        if !std::mem::take(fresh) && self.track.is_keyframe(data) {
            *drops_rasl = false;
        }
        // A picture the decoder drops would leave its time waiting, and
        // every later picture would take the time before its own.
        if *drops_rasl && self.track.is_rasl(data) {
            return;
        }
        pending.push(Reverse(ts));
        let (strip, failure, time_base) =
            (&mut self.strip, &mut self.first_failure, self.time_base);
        let mut shown = 0_usize;
        let result = decoder.decode(data, &mut |picture: Picture<'_>| {
            shown += 1;
            offer(strip, failure, pending, time_base, &picture, force);
        });
        // A packet that failed, or that a decoder in packet order showed
        // nothing for, such as a hidden VP8 frame, gives its time back.
        if result.is_err() || (shown == 0 && !self.track.codec.reorders()) {
            pending.retain(|&Reverse(waiting)| waiting != ts);
        }
        if let Err(message) = result {
            self.strip.failures += 1;
            self.first_failure.get_or_insert(message);
        }
    }

    /// Hand over the pictures the open decoder still holds, then close it.
    fn end_session(&mut self, force: bool) {
        let Some(Session {
            mut decoder,
            mut pending,
            ..
        }) = self.session.take()
        else {
            return;
        };
        let (strip, failure, time_base) =
            (&mut self.strip, &mut self.first_failure, self.time_base);
        decoder.finish(&mut |picture: Picture<'_>| {
            offer(strip, failure, &mut pending, time_base, &picture, force);
        });
    }

    /// The strip, once the packets stop. A clip whose container `ended`
    /// lasts a frame past its latest packet, one average step, the
    /// timeline its frames are on. A decode cut short lasts as long as the
    /// container says.
    fn finish(mut self, ended: bool) -> Result<VideoThumbnails, String> {
        // The last group ends a frame after its last packet.
        if let Some(&(last, _)) = self.group.last() {
            self.close_group(seconds(last, self.time_base) + 1.0 / MAX_KEPT_FPS);
        }
        self.end_session(false);
        if self.strip.times.is_empty() {
            let codec = self.track.codec.name();
            let failures = self.strip.failures;
            return Err(self.first_failure.unwrap_or_else(|| {
                format!("the {codec} picture decoded no frames ({failures} packets failed)")
            }));
        }
        let (first, latest, count) = self.packet_times;
        let step = latest.saturating_sub(first) / count.saturating_sub(1).max(1);
        let shown = seconds(latest.saturating_add(step), self.time_base);
        let length = match self.strip.duration {
            Some(container) if !ended => container.max(shown),
            _ => shown,
        };
        Ok(self.strip.finish(length))
    }
}

/// Whether a decode `elapsed` into its work, at a group shown from
/// `start`, is on pace to finish a clip `duration` long within
/// [`TARGET_DECODE_TIME`]. A clip of unknown length keeps any pace.
fn on_pace(elapsed: Duration, start: f32, duration: Option<f32>) -> bool {
    let Some(duration) = duration else {
        return true;
    };
    // A tenth of the target is spent at the start, before any pace shows.
    let share = (start / duration).clamp(0.0, 1.0) + 0.1;
    elapsed <= TARGET_DECODE_TIME.mul_f32(share)
}

/// A picture leaving a decoder takes the earliest waiting packet time.
fn offer(
    strip: &mut Strip,
    failure: &mut Option<String>,
    pending: &mut BinaryHeap<Reverse<u64>>,
    time_base: TimeBase,
    picture: &Picture<'_>,
    force: bool,
) {
    let Some(Reverse(ts)) = pending.pop() else {
        return;
    };
    if let Err(message) = strip.offer(seconds(ts, time_base), picture, force) {
        strip.failures += 1;
        failure.get_or_insert(message);
    }
}

fn seconds(ts: u64, time_base: TimeBase) -> f32 {
    (ts as f64 * f64::from(time_base.numer) / f64::from(time_base.denom)) as f32
}

/// Decode a video container's picture track into thumbnails.
///
/// `extension` names the container family, `mp4` or `mkv`, the way the
/// sample loader names it for the audio track. Symphonia probes by it.
/// A container with no video track has no picture: `None`. One this build
/// cannot decode is refused by name. The decode stops early once
/// `cancelled` is set.
pub fn decode_thumbnails(
    bytes: Vec<u8>,
    extension: &str,
    cancelled: &AtomicBool,
) -> Result<Option<VideoThumbnails>, String> {
    // Decoders parse untrusted input on a library thread, so a panic is
    // contained the same refusal as any other decode failure. See
    // `samples::codecs::decode_guarded`.
    let decode = || decode_thumbnails_inner(bytes, extension, cancelled);
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

/// What Symphonia says about a track that could be the picture.
struct Candidate {
    time_base: TimeBase,
    duration: Option<f32>,
}

fn decode_thumbnails_inner(
    bytes: Vec<u8>,
    extension: &str,
    cancelled: &AtomicBool,
) -> Result<Option<VideoThumbnails>, String> {
    // Symphonia names a codec only for sound, so the container's own
    // header names the picture's.
    let entries = match extension {
        "mp4" => mp4::entries(&bytes),
        "mkv" => matroska::entries(&bytes),
        _ => HashMap::new(),
    };
    let stream = MediaSourceStream::new(Box::new(std::io::Cursor::new(bytes)), Default::default());
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
    // Tracks that could carry the picture. A subtitle or chapter track sits
    // among them: its header names it, or its packets sniff as nothing
    // known, and it is passed over. A text track listed before the video
    // does not refuse the clip.
    let ids = crate::samples::container_video_track_ids(format.as_ref());
    if ids.is_empty() {
        return Ok(None);
    }
    let candidates: HashMap<u32, Candidate> = format
        .tracks()
        .iter()
        .filter(|track| ids.contains(&track.id))
        .map(|track| {
            let params = &track.codec_params;
            // Without a time base a Matroska tick is a millisecond.
            let time_base = params.time_base.unwrap_or(TimeBase::new(1, 1_000));
            let candidate = Candidate {
                time_base,
                duration: params.n_frames.map(|ticks| seconds(ticks, time_base)),
            };
            (track.id, candidate)
        })
        .collect();
    let started = Instant::now();
    let mut clip: Option<(u32, Clip)> = None;
    let mut skipped: HashSet<u32> = HashSet::new();
    let mut refused: Option<String> = None;
    let mut packets = 0u64;
    let mut ended = false;
    while packets < MAX_PICTURE_PACKETS && started.elapsed() < MAX_DECODE_TIME {
        if cancelled.load(Ordering::Acquire) {
            return Err("the picture decode was cancelled".to_owned());
        }
        // The end of the container ends the strip, whatever it holds.
        let Ok(packet) = format.next_packet() else {
            ended = true;
            break;
        };
        let id = packet.track_id();
        let Some(candidate) = candidates.get(&id) else {
            continue;
        };
        if packet.data.is_empty()
            || skipped.contains(&id)
            || clip.as_ref().is_some_and(|(selected, _)| *selected != id)
        {
            continue;
        }
        if clip.is_none() {
            match Track::identify(entries.get(&id), &packet.data) {
                Ok(track) => {
                    clip = Some((
                        id,
                        Clip::new(track, candidate.time_base, candidate.duration),
                    ));
                }
                Err(refusal) => {
                    // A track this build cannot decode is named once, but
                    // a later track can still be the picture: Matroska
                    // files open with text tracks before video.
                    if let Refusal::Codec(message) = refusal {
                        refused.get_or_insert(message);
                    }
                    skipped.insert(id);
                    continue;
                }
            }
        }
        if let Some((_, clip)) = clip.as_mut() {
            packets += 1;
            clip.push(packet.ts(), packet.data);
        }
    }
    match clip {
        Some((_, clip)) => clip.finish(ended).map(Some),
        None => refused.map_or(Ok(None), Err),
    }
}

#[cfg(test)]
mod tests {
    //! Video containers as picture sources: their first video track's
    //! packets decode to a thinned filmstrip, whatever the codec.

    use super::*;

    /// 0.6 s of a moving test pattern at 10 fps, VP8 beside a Vorbis tone.
    const TINY_VP8: &[u8] = include_bytes!("video/testdata/tiny_video_vp8.webm");
    /// The same pattern as Motion JPEG in a QuickTime container.
    const TINY_MJPEG: &[u8] = include_bytes!("video/testdata/tiny_video_mjpeg.mov");
    /// A Matroska with no video track at all.
    const AUDIO_ONLY: &[u8] = include_bytes!("video/testdata/tiny_audio_only.webm");
    /// A subtitle track listed before the VP8 picture.
    const TINY_SUBS: &[u8] = include_bytes!("video/testdata/tiny_video_subs.mkv");
    /// 0.6 s of grey at 10 fps, one step brighter each frame: limited-range
    /// luma 16 + 40 a frame, or the same at 10 bits. The H.264 and HEVC
    /// clips carry B-frames, so their decode order is not their display
    /// order.
    const RAMPS: [(&str, &[u8], &str); 6] = [
        (
            "H.264 with B-frames",
            include_bytes!("video/testdata/tiny_ramp_h264.mp4"),
            "mp4",
        ),
        (
            "10-bit H.264 in Matroska",
            include_bytes!("video/testdata/tiny_ramp_h264_10bit.mkv"),
            "mkv",
        ),
        (
            "HEVC with B-frames",
            include_bytes!("video/testdata/tiny_ramp_hevc.mp4"),
            "mp4",
        ),
        (
            "AV1",
            include_bytes!("video/testdata/tiny_ramp_av1.webm"),
            "mkv",
        ),
        (
            "10-bit AV1",
            include_bytes!("video/testdata/tiny_ramp_av1_10bit.webm"),
            "mkv",
        ),
        (
            "VP9",
            include_bytes!("video/testdata/tiny_ramp_vp9.webm"),
            "mkv",
        ),
    ];

    fn decode(clip: &[u8], extension: &str) -> Result<VideoThumbnails, String> {
        decode_thumbnails(clip.to_vec(), extension, &AtomicBool::new(false))?
            .ok_or_else(|| "no picture".to_owned())
    }

    #[test]
    fn a_vp8_clip_decodes_a_filmstrip_that_moves() {
        let strip = decode(TINY_VP8, "mkv").expect("the picture must decode");
        assert_eq!((strip.width(), strip.height()), (64, 36));
        assert_eq!(strip.frame_count(), 6, "every frame fits the budget");
        assert!(
            (strip.duration() - 0.6).abs() < 0.01,
            "six frames of a tenth of a second each: {}",
            strip.duration()
        );
        let first = strip.frame_at(0.0).expect("the first frame");
        let last = strip.frame_at(0.6).expect("the last frame holds");
        assert_ne!(first, last, "the test pattern moves");
        // Between two kept frames the earlier one shows. Before the first,
        // the first does.
        assert_eq!(strip.frame_at(0.14), strip.frame_at(0.1));
        assert_eq!(strip.frame_at(0.0), strip.frame_at(0.04));
        assert!(
            strip.frame_at(-1.0).is_none(),
            "nothing shows before the clip"
        );
    }

    /// Each codec decodes the ramp into six frames whose grey follows the
    /// clock, so a frame out of display order, or in the wrong range, fails.
    #[test]
    fn every_codec_shows_its_frames_in_display_order() {
        for (name, clip, extension) in RAMPS {
            let strip = decode(clip, extension).unwrap_or_else(|error| panic!("{name}: {error}"));
            assert_eq!((strip.width(), strip.height()), (64, 36), "{name}");
            assert_eq!(strip.frame_count(), 6, "{name}");
            assert!(
                (strip.duration() - 0.6).abs() < 0.01,
                "{name} lasts 0.6 s, not {}",
                strip.duration()
            );
            for step in 0..6u32 {
                let frame = strip.frame_at(step as f32 / 10.0 + 0.01).expect("a frame");
                let grey = frame
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .map(|pixel| u32::from(pixel[1]))
                    .sum::<u32>()
                    / (64 * 36);
                let expected = step * 40 * 255 / 219;
                assert!(
                    grey.abs_diff(expected) <= 4,
                    "{name} frame {step}: grey {grey}, not {expected}"
                );
            }
        }
    }

    #[test]
    fn a_motion_jpeg_clip_decodes_the_same_strip() {
        let strip = decode(TINY_MJPEG, "mp4").expect("the picture must decode");
        assert_eq!((strip.width(), strip.height()), (64, 36));
        assert_eq!(strip.frame_count(), 6);
        assert_ne!(
            strip.frame_at(0.0).expect("the first frame"),
            strip.frame_at(0.5).expect("the last frame"),
            "the test pattern moves"
        );
    }

    /// A text track listed before the video is passed over, not mistaken
    /// for the picture: its packets sniff as nothing this build knows.
    #[test]
    fn a_subtitle_track_before_the_video_is_not_the_picture() {
        let strip = decode(TINY_SUBS, "mkv").expect("the picture must decode");
        assert_eq!((strip.width(), strip.height()), (64, 36));
        assert_eq!(strip.frame_count(), 4, "0.4 s at ten frames a second");
        assert_ne!(
            strip.frame_at(0.0).expect("the first frame"),
            strip.frame_at(0.3).expect("the last frame"),
            "the test pattern moves"
        );
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
    fn a_container_without_a_video_track_has_no_picture() {
        let picture = decode_thumbnails(AUDIO_ONLY.to_vec(), "mkv", &AtomicBool::new(false));
        assert!(
            matches!(picture, Ok(None)),
            "a sound, not a refusal: {picture:?}"
        );
    }

    #[test]
    fn a_cancelled_decode_stops() {
        let error = decode_thumbnails(TINY_VP8.to_vec(), "mkv", &AtomicBool::new(true))
            .expect_err("cancelled before the first packet");
        assert!(error.contains("cancelled"), "{error}");
    }

    #[test]
    fn a_large_frame_scales_into_the_thumb_edge() {
        assert_eq!(thumb_size(1920, 1080), (160, 90));
        assert_eq!(thumb_size(1080, 1920), (90, 160));
        assert_eq!(thumb_size(64, 36), (64, 36), "a small frame keeps its size");
        assert_eq!(thumb_size(160, 160), (160, 160));
    }

    #[test]
    fn thinning_keeps_every_second_frame_and_its_times() {
        let strip = decode(TINY_VP8, "mkv").expect("the picture must decode");
        let mut thin = Strip::new(None);
        thin.width = strip.width;
        thin.height = strip.height;
        thin.times = strip.times.to_vec();
        thin.frames = strip.frames.to_vec();
        thin.thin();
        assert_eq!(thin.times.len(), 3, "half of six");
        assert_eq!(thin.times[0], strip.times[0]);
        assert_eq!(thin.times[1], strip.times[2]);
        assert_eq!(thin.times[2], strip.times[4]);
        assert_eq!(thin.frames.len(), thin.frame_bytes() * 3);
        assert!((thin.min_gap - 2.0 / MAX_KEPT_FPS).abs() < f32::EPSILON);
    }

    /// A long clip spreads its budget over its length from the start, so
    /// it never decodes more frames than it keeps.
    #[test]
    fn a_long_clip_spaces_its_frames_over_its_length() {
        let short = Strip::new(Some(2.0));
        assert!((short.min_gap - 1.0 / MAX_KEPT_FPS).abs() < f32::EPSILON);
        let film = Strip::new(Some(600.0));
        let frames = CLIP_BUDGET_BYTES / 2 / (160 * 90 * 4) * 9 / 10;
        assert!((film.min_gap - 600.0 / frames as f32).abs() < 0.01);
        assert!(Strip::new(Some(f32::NAN)).duration.is_none());
    }

    /// A keyframe group with no kept frame due in it is skipped, one with
    /// one due is reduced to its keyframe, and one with several decodes
    /// whole.
    #[test]
    fn a_keyframe_group_decodes_as_far_as_its_kept_frames_need() {
        let mut strip = Strip::new(None);
        strip.min_gap = 4.0;
        assert_eq!(strip.due_between(0.0, 2.0), 1, "the first frame is due");
        strip.times.push(0.0);
        strip.last_time = 0.0;
        assert_eq!(strip.due_between(2.0, 3.5), 0, "nothing due before 3.6 s");
        assert_eq!(strip.due_between(4.0, 6.0), 1, "one due at 4 s");
        assert_eq!(strip.due_between(4.0, 12.5), 3, "4, 8 and 12 s");
        assert_eq!(strip.due_between(6.0, 4.0), 0, "an empty group");
    }

    /// A clip faster than the kept rate keeps 24 frames a second, also with
    /// frame times rounded to the millisecond.
    #[test]
    fn common_frame_rates_keep_24_frames_a_second() {
        for fps in [24.0f32, 25.0, 30.0, 60.0] {
            let mut strip = Strip::new(None);
            for frame in 0..fps as usize {
                let time = (frame as f32 / fps * 1000.0).round() / 1000.0;
                if strip.wants(time) {
                    strip.times.push(time);
                    strip.last_time = time;
                }
            }
            assert_eq!(strip.times.len(), 24, "{fps} fps");
        }
    }

    /// A decode behind the pace that finishes the clip in time reduces its
    /// next groups to their keyframes, so a long clip is covered to its end.
    #[test]
    fn a_decode_behind_its_pace_reduces_groups_to_keyframes() {
        let clip = Some(300.0);
        assert!(
            on_pace(Duration::ZERO, 0.0, clip),
            "the first group decodes whole"
        );
        assert!(
            on_pace(Duration::from_secs(4), 150.0, clip),
            "ahead at half way"
        );
        assert!(
            !on_pace(Duration::from_secs(14), 150.0, clip),
            "behind at half way"
        );
        assert!(
            on_pace(Duration::from_secs(40), 150.0, None),
            "no length, no pace"
        );
    }
}
