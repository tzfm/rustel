//! The audio input, as a source a voice can read.
//!
//! The input callback writes frames into a ring; the output callback's
//! voices read them back a little behind, mono, one channel each. The two
//! callbacks run on their own clocks, so the reader keeps a fixed distance
//! behind the writer and re-syncs when it drifts out of the ring - a live
//! source with no length, gated by the event like a synth.

use std::sync::atomic::{AtomicU32, AtomicU64, AtomicUsize, Ordering};

/// Frames kept: about a second at 48 kHz, per channel.
pub const INPUT_RING_FRAMES: usize = 1 << 16;
/// The most input channels read.
pub const MAX_INPUT_CHANNELS: usize = 16;
/// How far behind the writer the reader sits, in frames: enough for one
/// callback of jitter on each side.
const READ_LAG_FRAMES: u64 = 512;

/// The most the input fader lifts: +48 dB, as a linear factor.
pub const MAX_GAIN: f32 = 256.0;

/// `len` atomics reading zero, from zeroed memory rather than a loop that
/// stores each one: the pages of a large zeroed allocation stay untouched,
/// and so out of the resident set, until something writes them. Every
/// output carries an input ring sized for sixteen channels and a record tap
/// whether or not an input is open or a take is running.
pub(crate) fn zeroed_atomics(len: usize) -> Box<[AtomicU32]> {
    // SAFETY: an all-zero bit pattern is a valid `AtomicU32` holding 0.
    unsafe { Box::<[AtomicU32]>::new_zeroed_slice(len).assume_init() }
}

/// How many of the writer's largest deliveries the reader stays behind. Some
/// drivers deliver frames in bursts: a phone microphone over a wireless link
/// delivers thousands at a time. With this lag the reader does not reach the
/// write head between two bursts, which would read as silence and crackle.
const READ_LAG_CHUNKS: u64 = 2;

pub struct InputRing {
    /// `INPUT_RING_FRAMES` frames of `MAX_INPUT_CHANNELS`, interleaved,
    /// as f32 bits.
    slots: Box<[AtomicU32]>,
    channels: AtomicUsize,
    /// Frames written so far.
    written: AtomicU64,
    /// The loudest absolute sample since the last take, as f32 bits - a
    /// non-negative float's bits order like its value, so a `fetch_max`
    /// is the whole meter.
    peak: AtomicU32,
    /// The input's own fader, as f32 bits: applied as the frames land, so
    /// every voice on `in` and the meter hear the same level.
    gain: AtomicU32,
    /// The rate the frames land at, in Hz; 0 until the stream says. A
    /// reader at another rate walks the ring at the ratio.
    sample_rate: AtomicU32,
    /// The largest single delivery so far, in frames: the reader's lag.
    largest_write: AtomicU64,
}

impl Default for InputRing {
    fn default() -> Self {
        Self::new()
    }
}

impl InputRing {
    pub fn new() -> Self {
        Self {
            slots: zeroed_atomics(INPUT_RING_FRAMES * MAX_INPUT_CHANNELS),
            channels: AtomicUsize::new(0),
            written: AtomicU64::new(0),
            peak: AtomicU32::new(0),
            gain: AtomicU32::new(1.0f32.to_bits()),
            sample_rate: AtomicU32::new(0),
            largest_write: AtomicU64::new(0),
        }
    }

    /// The rate the frames land at; 0 while unknown.
    pub fn sample_rate(&self) -> u32 {
        self.sample_rate.load(Ordering::Relaxed)
    }

    pub fn set_sample_rate(&self, rate: u32) {
        self.sample_rate.store(rate, Ordering::Relaxed);
    }

    /// The driver handed over this many frames at once: the reader's lag
    /// follows the largest delivery. Told before the delivery is written,
    /// in whatever pieces, so the lag measures the delivery and not the
    /// pieces.
    pub fn note_delivery(&self, frames: u64) {
        self.largest_write
            .fetch_max(frames.min(INPUT_RING_FRAMES as u64 / 4), Ordering::Relaxed);
    }

    /// A new stream starts from the fixed lag and learns its own bursts:
    /// a wired interface after a phone's microphone need not carry the
    /// phone's 170 ms.
    pub fn forget_deliveries(&self) {
        self.largest_write.store(0, Ordering::Relaxed);
    }

    /// How far behind the writer a reader is placed: a fixed few
    /// milliseconds, or twice the largest delivery so far when the driver
    /// writes in bursts, so a burst that has not landed yet is never read.
    pub fn read_lag(&self) -> u64 {
        let largest = self.largest_write.load(Ordering::Relaxed);
        // The fixed minimum must not reduce the two-delivery margin.
        (largest * READ_LAG_CHUNKS).max(READ_LAG_FRAMES)
    }

    /// The input fader, linear; 1 is unity. The ceiling is +48 dB, far
    /// enough to bring a phone's microphone up to the music.
    pub fn set_gain(&self, gain: f32) {
        let gain = if gain.is_finite() {
            gain.clamp(0.0, MAX_GAIN)
        } else {
            1.0
        };
        self.gain.store(gain.to_bits(), Ordering::Relaxed);
    }

    pub fn gain(&self) -> f32 {
        f32::from_bits(self.gain.load(Ordering::Relaxed))
    }

    /// The loudest absolute sample written since the last take; 0 with
    /// nothing written since.
    pub fn take_peak(&self) -> f32 {
        f32::from_bits(self.peak.swap(0, Ordering::Relaxed))
    }

    /// Channels the writer delivers; zero while no input is open.
    pub fn channels(&self) -> usize {
        self.channels.load(Ordering::Acquire)
    }

    pub fn set_channels(&self, channels: usize) {
        self.channels
            .store(channels.min(MAX_INPUT_CHANNELS), Ordering::Release);
    }

    /// The input callback: interleaved frames of `channels`.
    pub fn write(&self, interleaved: &[f32], channels: usize) {
        if channels == 0 {
            return;
        }
        let kept = channels.min(MAX_INPUT_CHANNELS);
        let written = self.written.load(Ordering::Relaxed);
        let frames = interleaved.len() / channels;
        let gain = self.gain();
        let mut peak = 0.0f32;
        for frame in 0..frames {
            let slot = ((written + frame as u64) % INPUT_RING_FRAMES as u64) as usize;
            for channel in 0..kept {
                let sample = interleaved[frame * channels + channel] * gain;
                peak = peak.max(sample.abs());
                self.slots[slot * MAX_INPUT_CHANNELS + channel]
                    .store(sample.to_bits(), Ordering::Relaxed);
            }
        }
        self.peak.fetch_max(peak.to_bits(), Ordering::Relaxed);
        self.written
            .store(written + frames as u64, Ordering::Release);
    }

    pub fn written(&self) -> u64 {
        self.written.load(Ordering::Acquire)
    }

    /// Bytes of the ring the writer has reached. The slots are zeroed
    /// memory, resident only once written, and every frame is laid out for
    /// all sixteen channels, so a mono input reaches as much as a
    /// sixteen-channel one: the whole ring after a second or so.
    pub fn touched_bytes(&self) -> usize {
        let frames = usize::try_from(self.written()).unwrap_or(usize::MAX);
        frames.min(INPUT_RING_FRAMES) * MAX_INPUT_CHANNELS * std::mem::size_of::<AtomicU32>()
    }

    /// One sample of `channel` at ring frame `frame`; silence beyond what
    /// has been written or for a channel the input does not have.
    pub fn sample(&self, frame: u64, channel: usize) -> f32 {
        // Acquire pairs with the writer's Release on `written`, so a slot
        // read after this cannot see stale bits for a published frame.
        if channel >= self.channels() || frame >= self.written.load(Ordering::Acquire) {
            return 0.0;
        }
        let slot = (frame % INPUT_RING_FRAMES as u64) as usize;
        f32::from_bits(self.slots[slot * MAX_INPUT_CHANNELS + channel].load(Ordering::Relaxed))
    }

    /// A frame between two, by linear interpolation: how a reader at
    /// another rate walks the ring without a step at every frame.
    pub fn sample_at(&self, position: f64, channel: usize) -> f32 {
        if !position.is_finite() || position < 0.0 {
            return 0.0;
        }
        let frame = position.floor();
        let fraction = (position - frame) as f32;
        let frame = frame as u64;
        let first = self.sample(frame, channel);
        if fraction <= 0.0 {
            return first;
        }
        let written = self.written.load(Ordering::Acquire);
        if frame + 1 >= written {
            return first;
        }
        first + (self.sample(frame + 1, channel) - first) * fraction
    }

    /// Keep a cursor with room for the next block, or place a new one if it
    /// is ahead of the writer, at least half a ring behind, or too close.
    ///
    /// `wanted` is the next block's length in input-ring frames. A new cursor
    /// sits `max(read_lag(), wanted)` frames behind the writer, clamped to zero
    /// at startup. This reserves room for the whole block even when the input
    /// callback delivers smaller chunks. The lag also leaves a margin for
    /// drift between the input and output clocks.
    ///
    /// A startup cursor can track the write head without overtaking it, so
    /// checking for overtake alone would keep reading unpublished frames as
    /// silence. Reposition whenever there is less than a block available.
    /// Even for a zero-length request, a cursor at the write head needs
    /// repositioning once input has arrived.
    pub fn reader_frame(&self, cursor: Option<u64>, wanted: u64) -> u64 {
        let written = self.written.load(Ordering::Acquire);
        let fresh = written.saturating_sub(self.read_lag().max(wanted));
        let Some(cursor) = cursor else {
            return fresh;
        };
        if cursor > written || written - cursor >= INPUT_RING_FRAMES as u64 / 2 {
            return fresh;
        }
        if written - cursor < wanted.max(1) {
            return fresh;
        }
        cursor
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The ring keeps the loudest sample since the last take: a meter
    /// reads it and resets it in one move.
    /// The fader is applied as the frames land: what `in` plays and what
    /// the meter reads are both post-fader.
    #[test]
    fn the_input_fader_scales_what_lands_and_what_the_meter_reads() {
        let ring = InputRing::new();
        ring.set_channels(1);
        ring.set_gain(0.5);
        ring.write(&[0.8, -0.4], 1);
        assert_eq!(ring.take_peak(), 0.4);
        assert_eq!(ring.sample(0, 0), 0.4);
        ring.set_gain(f32::NAN);
        assert_eq!(ring.gain(), 1.0, "nonsense is unity");
        ring.set_gain(1000.0);
        assert_eq!(ring.gain(), MAX_GAIN, "clamped at +48 dB");
    }

    /// A frame takes its place for every channel whatever the input has,
    /// and the ring never holds more than itself once it wraps.
    #[test]
    fn an_input_ring_is_resident_as_far_as_frames_have_landed() {
        let ring = InputRing::new();
        assert_eq!(ring.touched_bytes(), 0);
        ring.set_channels(1);
        ring.write(&[0.0; 3], 1);
        assert_eq!(ring.touched_bytes(), 3 * MAX_INPUT_CHANNELS * 4);
        ring.write(&vec![0.0; INPUT_RING_FRAMES], 1);
        assert_eq!(
            ring.touched_bytes(),
            INPUT_RING_FRAMES * MAX_INPUT_CHANNELS * 4
        );
    }

    #[test]
    fn the_input_peak_is_the_loudest_sample_since_the_last_take() {
        let ring = InputRing::new();
        ring.set_channels(2);
        assert_eq!(ring.take_peak(), 0.0);
        ring.write(&[0.1, -0.6, 0.3, 0.2], 2);
        ring.write(&[0.05, 0.05], 2);
        assert_eq!(ring.take_peak(), 0.6, "the loudest, sign aside");
        assert_eq!(ring.take_peak(), 0.0, "taken");
    }

    #[test]
    fn frames_written_are_read_back_per_channel_and_the_cursor_resyncs() {
        let ring = InputRing::new();
        assert_eq!(ring.sample(0, 0), 0.0, "nothing written");
        ring.set_channels(2);
        ring.write(&[0.5, -0.5, 0.25, -0.25], 2);
        assert_eq!(ring.written(), 2);
        assert_eq!(ring.sample(0, 0), 0.5);
        assert_eq!(ring.sample(0, 1), -0.5);
        assert_eq!(ring.sample(1, 0), 0.25);
        assert_eq!(ring.sample(1, 2), 0.0, "no third channel");
        assert_eq!(ring.sample(7, 0), 0.0, "not yet written");
        // A fresh cursor sits behind the writer; a sane one runs on; a
        // stale one is re-placed. (Written in callback-sized pieces, so the
        // lag stays the fixed one a steady driver gets.)
        let silence = vec![0.0f32; 2 * 256];
        for _ in 0..8 {
            ring.write(&silence, 2);
        }
        let first = ring.reader_frame(None, 256);
        assert_eq!(first, ring.written() - READ_LAG_FRAMES);
        assert_eq!(ring.reader_frame(Some(first + 10), 256), first + 10);
        assert_eq!(
            ring.reader_frame(Some(0), 256),
            0,
            "still inside the ring, and a block of room: kept"
        );
        assert_eq!(
            ring.reader_frame(Some(ring.written() + 10), 256),
            ring.written() - READ_LAG_FRAMES,
            "ahead of the writer: re-placed"
        );
    }

    /// A reader placed on an empty ring must not stay at the write head. The
    /// cursor starts at zero lag and advances at the writer's rate, so the
    /// overtake check alone never moves it back.
    #[test]
    fn a_reader_placed_on_a_cold_ring_backs_off_once_the_writer_starts() {
        // A 512-frame output callback, which is what asks the ring for
        // frames and what decides whether a cursor is safe to keep.
        const BLOCK: u64 = 512;
        let ring = InputRing::new();
        ring.set_channels(1);
        // Placed before a single frame has landed: the only honest answer
        // is zero, and it is a cursor with no room at all.
        let mut cursor = ring.reader_frame(None, BLOCK);
        assert_eq!(cursor, 0);
        // Now the driver starts, and the reader keeps pace with it exactly
        // - which is what an audio callback does.
        for block in 0..16 {
            ring.note_delivery(BLOCK);
            ring.write(&[0.0; BLOCK as usize], 1);
            cursor = ring.reader_frame(Some(cursor), BLOCK);
            let room = ring.written() - cursor;
            assert!(
                room >= BLOCK,
                "block {block}: the reader has {room} frames of room for a \
                 {BLOCK}-frame block, so the tail of it is unwritten"
            );
            cursor += BLOCK;
        }
        // And once it has room it is left alone: a cursor sitting at the
        // full lag is not re-placed every block, or the fractional
        // interpolation would jump on every callback.
        let steady = ring.written() - ring.read_lag();
        assert_eq!(ring.reader_frame(Some(steady), BLOCK), steady);

        // The property, stated as the code states it: a cursor is kept
        // exactly when it holds the block about to be read.
        let lag = ring.read_lag();
        let written = ring.written();
        for gap in 0..=(lag + BLOCK) {
            let cursor = written - gap.min(written);
            let kept = ring.reader_frame(Some(cursor), BLOCK) == cursor;
            assert_eq!(
                kept,
                gap >= BLOCK,
                "a cursor {gap} frames back, reading a {BLOCK}-frame block: \
                 kept={kept}"
            );
        }

        // And the block is the caller's, not the delivery's or the lag's.
        // This is the case a margin reckoned from either gets wrong: a small
        // interface against a large callback. Sixty-four-frame deliveries
        // leave the reader at the fixed 512-frame lag, and that still must
        // not hand back a cursor that runs off the end of a 2048-frame read.
        const LARGE_BLOCK: u64 = 2048;
        let small = InputRing::new();
        small.set_channels(1);
        small.note_delivery(64);
        for _ in 0..128 {
            small.write(&[0.0; 64], 1);
        }
        assert_eq!(
            small.read_lag(),
            READ_LAG_FRAMES,
            "the fixed lag, shorter than the block"
        );
        let placed = small.reader_frame(None, LARGE_BLOCK);
        assert_eq!(
            placed,
            small.written() - LARGE_BLOCK,
            "placed a block back, not a lag back"
        );
        let kept = small.reader_frame(Some(placed), LARGE_BLOCK);
        assert!(
            small.written() - kept >= LARGE_BLOCK,
            "a {}-frame lag was handed to a {LARGE_BLOCK}-frame read with only \
             {} frames of room",
            small.read_lag(),
            small.written() - kept
        );

        // Overtaking the writer still re-places it.
        assert_eq!(
            ring.reader_frame(Some(ring.written() + 1), BLOCK),
            ring.written() - ring.read_lag()
        );
    }

    #[test]
    fn bursty_writers_widen_the_lag_and_readers_interpolate() {
        let ring = InputRing::new();
        ring.set_channels(1);
        assert_eq!(
            ring.read_lag(),
            READ_LAG_FRAMES,
            "the fixed lag until a burst"
        );
        ring.note_delivery(100);
        ring.write(&[0.0; 100], 1);
        assert_eq!(
            ring.read_lag(),
            READ_LAG_FRAMES,
            "a small delivery changes nothing"
        );
        // The driver's delivery is measured whole, however the callback
        // then writes it: an eight-channel interface handing over 1024
        // frames as two pieces of 512 is still a 1024-frame delivery.
        // The boundary case: a delivery of exactly the fixed lag is still
        // two deliveries behind, not one. The old `>` took the fixed lag
        // here and left the reader a single block of room.
        ring.note_delivery(READ_LAG_FRAMES);
        assert_eq!(
            ring.read_lag(),
            READ_LAG_FRAMES * READ_LAG_CHUNKS,
            "a delivery of exactly the fixed lag is two deliveries behind"
        );
        ring.note_delivery(1024);
        for _ in 0..2 {
            ring.write(&[0.0; 512], 1);
        }
        assert_eq!(ring.read_lag(), 2048, "two deliveries behind");
        ring.note_delivery(4096);
        ring.write(&vec![0.0; 4096], 1);
        assert_eq!(ring.read_lag(), 8192, "two bursts behind a bursty driver");
        ring.forget_deliveries();
        assert_eq!(ring.read_lag(), READ_LAG_FRAMES, "a new stream starts over");
        ring.note_delivery(4096);
        assert_eq!(ring.read_lag(), 8192);
        assert_eq!(
            ring.reader_frame(None, 256),
            ring.written().saturating_sub(ring.read_lag()),
            "placed at the floor"
        );
        let ring = InputRing::new();
        ring.set_channels(1);
        ring.write(&[0.0, 1.0, 0.5], 1);
        assert_eq!(ring.sample_at(0.5, 0), 0.5);
        assert_eq!(ring.sample_at(1.25, 0), 0.875);
        assert_eq!(ring.sample_at(2.5, 0), 0.5, "past the last frame holds it");
        assert_eq!(ring.sample_at(-1.0, 0), 0.0);
        assert_eq!(ring.sample_at(7.0, 0), 0.0, "unwritten is silence");
        assert_eq!(ring.sample_rate(), 0, "unknown until the stream says");
        ring.set_sample_rate(48_000);
        assert_eq!(ring.sample_rate(), 48_000);
    }
}
