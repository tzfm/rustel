//! Wait-free SPSC ring carrying `Copy` POD events into the audio callback.
//!
//! Payloads must not run `Drop` on the audio thread: a deallocation there can
//! block for as long as the allocator needs, and the callback has a deadline.
//! Hence `Copy` POD only.

use std::cell::UnsafeCell;
use std::mem::MaybeUninit;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

use crate::{DecodedSample, OscillatorControls, SampleControls, SampleId};

/// Scheduling metadata travels beside, rather than inside, the musical event.
/// Ordinary/offline callers continue to submit untracked `AudioEvent`s.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct QueuedAudioEvent {
    pub event: AudioEvent,
    pub confirmation: Option<crate::confirmation::ConfirmationOnset>,
    /// Expected immutable body for an actual sample onset. Unknown bodies
    /// may still play, but cannot certify a tracked recovery window.
    pub expected_sample_identity: Option<u64>,
}

impl From<AudioEvent> for QueuedAudioEvent {
    fn from(event: AudioEvent) -> Self {
        Self {
            event,
            confirmation: None,
            expected_sample_identity: None,
        }
    }
}

impl std::ops::Deref for QueuedAudioEvent {
    type Target = AudioEvent;

    fn deref(&self) -> &Self::Target {
        &self.event
    }
}

/// What crosses into the audio callback. Plain data, `Copy`, no pointers.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AudioEvent {
    pub onset_id: u64,
    pub generation: u64,
    pub ui_visuals: u64,
    pub target_frame: u64,
    /// How far the source has advanced at `target_frame`, in frames.
    pub onset_lead: f32,
    pub freq_hz: f32,
    pub gain: f32,
    pub duration_secs: f32,
    pub controls: OscillatorControls,
    pub sample: Option<SampleControls>,
    pub wavetable: Option<crate::backend::WavetableControls>,
    pub synth: Option<crate::backend::SynthSource>,
    /// Event-level choke group (see [`crate::OnsetEvent::cut`]): a new
    /// onset in the group cuts the previous one, whatever its source kind.
    pub cut: Option<f32>,
}

impl AudioEvent {
    /// A conservative frame after which this onset cannot read its bank
    /// sample. `decoded` must be the body selected by this event. Queued
    /// events must retain that body until this frame even after a score edit.
    /// The caller must bound `target_frame` by the latest possible onset,
    /// including any restart rebasing performed by the live consumer.
    pub fn sample_end_frame(
        &self,
        decoded: &DecodedSample,
        output_sample_rate: u32,
    ) -> Option<(SampleId, u64)> {
        if self.synth.is_some() {
            return None;
        }
        let (id, duration_secs, natural_stop_secs) = if let Some(wavetable) = self.wavetable {
            (wavetable.table, self.duration_secs, None)
        } else {
            let sample = self.sample?;
            let (duration_secs, natural_stop_secs) =
                sample.duration_and_natural_stop(decoded, self.duration_secs);
            (sample.sample, duration_secs, natural_stop_secs)
        };
        let mut source_stop_secs = duration_secs + self.controls.envelope.release_secs;
        if let Some(natural_stop_secs) = natural_stop_secs {
            source_stop_secs = source_stop_secs.min(natural_stop_secs);
        }
        // Always allow the filter ring, including future chain changes.
        // Ignoring onset_lead can only keep the body a little longer.
        let stop_secs =
            crate::sample::stop_secs_for(source_stop_secs, true, output_sample_rate as f32);
        if output_sample_rate == 0 || !stop_secs.is_finite() {
            return Some((id, u64::MAX));
        }
        // The renderer's age clock is f32. Round past its boundary before
        // converting to frames, including for unusually long/slow samples
        // whose float spacing exceeds a render quantum.
        let frames = (stop_secs.max(0.0).next_up() * output_sample_rate as f32).next_up();
        let end_frame = self
            .target_frame
            .saturating_add(f64::from(frames).ceil() as u64)
            .saturating_add(128);
        Some((id, end_frame))
    }
}

/// # Safety contract
///
/// Exactly one thread may call [`Ring::push`] and exactly one may call
/// [`Ring::pop`] at a time. Both take `&self` so the ring can live behind an
/// `Arc` shared with the audio callback, which means the compiler does not
/// enforce that rule - two concurrent pushes would race through the
/// `UnsafeCell` and be undefined behaviour.
///
/// The first thread to push becomes the producer and the first to pop
/// becomes the consumer. Each keeps its role until the unsafe
/// [`Ring::release_producer`] or [`Ring::release_consumer`] hands it to the
/// next thread that uses it. A host whose audio callback can move to a new
/// thread (a device reopen, a route change, an audio-server restart) calls
/// `release_consumer` once the old callback has provably stopped; without it
/// every pop from the new thread is refused, which is silence in release
/// builds and a panic in debug builds. Replacing the producer thread needs
/// `release_producer` the same way.
///
/// The rule is upheld here by construction: the producer thread is the only
/// pusher, the audio callback is the only popper, and `recycle_output` drains
/// only after dropping the CPAL stream, which joins the callback thread first.
/// The ownership checks below run in release as well as debug: a second
/// producer or consumer is refused (and counted in `role_conflicts`) instead
/// of racing through the `UnsafeCell`. Refusing rather than panicking keeps
/// the audio path alive; debug builds also assert, so a test reports the
/// broken contract instead of quietly dropping an event.
///
/// The role check therefore guards the race unless a release is misused,
/// which is why both releases are `unsafe`. It is a runtime guard, not a
/// type: a design that hands the consumer to the callback and takes it back
/// when the stream is dropped would make a second consumer unrepresentable.
/// That design changes the realtime ownership model and needs device
/// testing first.
pub struct Ring {
    /// Slots are written before they are read, never the other way round:
    /// the consumer only reads between `head` and `tail`, and every slot in
    /// that span was written by the push that published it. So the buffer
    /// starts uninitialised, and each page of it becomes resident when the
    /// producer first reaches it, not when a device opens. Opening an output
    /// to audition one sound used to write every slot of a ring sized for
    /// the densest score, tens of megabytes, only to free it on the stop.
    buf: UnsafeCell<Box<[MaybeUninit<QueuedAudioEvent>]>>,
    mask: usize,
    head: AtomicUsize,
    tail: AtomicUsize,
    pub refused: AtomicUsize,
    pub peak_depth: AtomicUsize,
    /// Operations refused because a second thread tried to take a role. Zero
    /// on every healthy run; non-zero is a contract bug worth reporting, not
    /// a capacity problem, so it is counted apart from `refused`.
    pub role_conflicts: AtomicUsize,
    /// Thread identities of the single producer and single consumer, recorded
    /// on first use. Zero means "not yet claimed".
    producer: AtomicU64,
    consumer: AtomicU64,
}

// SAFETY: single producer / single consumer with atomic index publication,
// upheld by the callers documented above and enforced by the role claims in
// `push`/`pop`, which refuse a second thread in release as well as debug.
unsafe impl Sync for Ring {}
unsafe impl Send for Ring {}

/// Hash of the current thread's id - `ThreadId` has no stable integer form.
fn this_thread() -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    std::thread::current().id().hash(&mut hasher);
    // Never zero, so the slot's "unclaimed" value stays distinguishable.
    hasher.finish() | 1
}

/// Claim a role for this thread. `false` means another thread already holds
/// it and the caller must refuse the operation.
///
/// The check runs in release builds too. A debug-only check would catch a
/// broken contract in tests and leave the same race through the `UnsafeCell`
/// unreported in release builds. One uncontended compare-exchange per event
/// costs far less than the event itself, and it is lock-free, so it stays
/// realtime-safe in the callback.
///
/// It refuses instead of panicking because the caller is the audio path, and
/// a panic there stops the audio. A refused push is counted like a full ring;
/// debug builds still assert, so a test reports the bug instead of silently
/// dropping an event.
#[inline]
#[must_use]
fn claim(slot: &AtomicU64, role: &str) -> bool {
    let me = this_thread();
    match slot.compare_exchange(0, me, Ordering::AcqRel, Ordering::Acquire) {
        Ok(_) => true,
        Err(other) if other == me => true,
        Err(_) => {
            let _ = role;
            false
        }
    }
}

impl Ring {
    pub fn new(cap: usize) -> Self {
        let cap = cap.next_power_of_two();
        Self {
            buf: UnsafeCell::new(Box::new_uninit_slice(cap)),
            mask: cap - 1,
            head: AtomicUsize::new(0),
            tail: AtomicUsize::new(0),
            refused: AtomicUsize::new(0),
            peak_depth: AtomicUsize::new(0),
            role_conflicts: AtomicUsize::new(0),
            producer: AtomicU64::new(0),
            consumer: AtomicU64::new(0),
        }
    }

    pub fn capacity(&self) -> usize {
        self.mask + 1
    }

    /// Events accepted since the ring was made; a refused push is not one,
    /// and taking an event does not undo it. The buffer starts uninitialised
    /// and a slot becomes resident when a push first reaches it, so this
    /// against the capacity is how much of the ring the process holds. Only
    /// the producer moves it, so the producer reads it exactly.
    pub fn pushed(&self) -> usize {
        self.tail.load(Ordering::Relaxed)
    }

    pub fn len(&self) -> usize {
        let head = self.head.load(Ordering::Acquire);
        self.tail
            .load(Ordering::Acquire)
            .wrapping_sub(head)
            .min(self.capacity())
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Limit this drain to the current queue length, so producer refills cannot
    /// extend it. Each consumed entry uses the ordinary SPSC check.
    pub fn drain_available(&self) -> impl Iterator<Item = AudioEvent> + '_ {
        std::iter::from_fn(|| self.pop()).take(self.len())
    }

    pub(crate) fn drain_queued_available(&self) -> impl Iterator<Item = QueuedAudioEvent> + '_ {
        std::iter::from_fn(|| self.pop_queued()).take(self.len())
    }

    pub fn push(&self, e: AudioEvent) -> bool {
        self.push_queued(e.into())
    }

    pub(crate) fn push_queued(&self, e: QueuedAudioEvent) -> bool {
        if !claim(&self.producer, "producer") {
            // Another thread owns the producer role. Writing anyway is the
            // data race this guard exists for, so refuse and count it. The
            // count comes BEFORE the debug assert so a debug run still
            // records the conflict on its way out.
            self.refused.fetch_add(1, Ordering::Relaxed);
            self.role_conflicts.fetch_add(1, Ordering::Relaxed);
            debug_assert!(
                false,
                "the audio ring allows exactly one producer; a second thread would race through the UnsafeCell"
            );
            return false;
        }
        let tail = self.tail.load(Ordering::Relaxed);
        let head = self.head.load(Ordering::Acquire);
        if tail - head == self.capacity() {
            self.refused.fetch_add(1, Ordering::Relaxed);
            return false;
        }
        // SAFETY: sole producer; slot unpublished until Release store.
        unsafe {
            (*self.buf.get())[tail & self.mask].write(e);
        }
        self.tail.store(tail + 1, Ordering::Release);
        let d = tail + 1 - head;
        self.peak_depth.fetch_max(d, Ordering::Relaxed);
        true
    }

    /// Hand the consumer role to whichever thread pops next.
    ///
    /// One thread holds the role at a time, and the holder can change: the
    /// audio callback pops while a stream lives, and `recycle_output` drains
    /// the leftovers after it drops that stream. The drop joins the callback
    /// thread, so the two never overlap. This call marks that handoff, so the
    /// ownership check accepts the new thread and still refuses any other.
    ///
    /// # Safety
    ///
    /// The thread that held the consumer role must have stopped popping and
    /// must not pop again, for example because its thread was joined or its
    /// stream was dropped. Otherwise both consumers can take the same slot
    /// while the producer rewrites it, which is a data race.
    pub unsafe fn release_consumer(&self) {
        self.consumer.store(0, Ordering::Release);
    }

    /// Hand the producer role to whichever thread pushes next.
    ///
    /// For a host that replaces the thread scheduling into the ring.
    ///
    /// # Safety
    ///
    /// The thread that held the producer role must have stopped pushing and
    /// must not push again, for example because its thread was joined.
    /// Otherwise both producers write the same slot, which is a data race.
    pub unsafe fn release_producer(&self) {
        self.producer.store(0, Ordering::Release);
    }

    pub fn pop(&self) -> Option<AudioEvent> {
        self.pop_queued().map(|queued| queued.event)
    }

    fn pop_queued(&self) -> Option<QueuedAudioEvent> {
        if !claim(&self.consumer, "consumer") {
            self.role_conflicts.fetch_add(1, Ordering::Relaxed);
            debug_assert!(
                false,
                "the audio ring allows exactly one consumer; a second thread would race through the UnsafeCell"
            );
            return None;
        }
        let head = self.head.load(Ordering::Relaxed);
        if head == self.tail.load(Ordering::Acquire) {
            return None;
        }
        // SAFETY: sole consumer; slot published by producer Release store,
        // so the push that published it initialised it first. The payload is
        // `Copy`: reading it leaves the slot as it was.
        let e = unsafe { (*self.buf.get())[head & self.mask].assume_init() };
        self.head.store(head + 1, Ordering::Release);
        Some(e)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    pub(super) fn ev(id: u64) -> AudioEvent {
        AudioEvent {
            onset_id: id,
            generation: 1,
            target_frame: id * 10,
            onset_lead: 0.0,
            freq_hz: 440.0,
            gain: 0.5,
            duration_secs: 0.1,
            ui_visuals: 0,
            controls: OscillatorControls::default(),
            sample: None,
            wavetable: None,
            synth: None,
            cut: None,
        }
    }

    #[test]
    fn fifo_order_and_bound() {
        let r = Ring::new(4);
        for i in 0..4 {
            assert!(r.push(ev(i)));
        }
        assert!(!r.push(ev(99)));
        for i in 0..4 {
            assert_eq!(r.pop().unwrap().onset_id, i);
        }
        assert!(r.pop().is_none());
    }

    #[test]
    fn draining_a_wrapped_prefix_does_not_consume_refills() {
        let ring = Ring::new(4);
        for id in 0..3 {
            assert!(ring.push(ev(id)));
            assert_eq!(ring.pop().unwrap().onset_id, id);
        }
        for id in 0..4 {
            assert!(ring.push(ev(id)));
        }
        let mut prefix = ring.drain_available();
        for id in 0..4 {
            assert_eq!(prefix.next().map(|event| event.onset_id), Some(id));
            assert!(ring.push(ev(id + 4)));
        }
        assert_eq!(prefix.next().map(|event| event.onset_id), None);
        assert_eq!(
            ring.drain_available()
                .map(|event| event.onset_id)
                .collect::<Vec<_>>(),
            [4, 5, 6, 7]
        );
        let mut empty = ring.drain_available();
        assert!(ring.push(ev(8)));
        assert_eq!(empty.next().map(|event| event.onset_id), None);
        assert_eq!(ring.pop().unwrap().onset_id, 8);
    }

    /// What the ring holds in memory follows the pushes: a slot taken
    /// stays resident, a refused push reached nothing, and the count runs
    /// on past the capacity once the ring wraps.
    #[test]
    fn a_ring_counts_accepted_pushes_not_pops_or_refusals() {
        let ring = Ring::new(2);
        assert_eq!(ring.pushed(), 0);
        assert!(ring.push(ev(1)));
        assert!(ring.push(ev(2)));
        assert!(!ring.push(ev(3)), "full");
        assert_eq!(ring.pushed(), 2);
        assert_eq!(ring.pop().map(|event| event.onset_id), Some(1));
        assert_eq!(ring.pop().map(|event| event.onset_id), Some(2));
        assert_eq!(ring.pushed(), 2, "taking an event leaves its slot touched");
        assert!(ring.push(ev(4)));
        assert_eq!(ring.pushed(), 3);
    }

    #[test]
    fn payload_has_no_drop_glue() {
        assert!(!std::mem::needs_drop::<AudioEvent>());
    }

    #[test]
    fn observed_depth_stays_bounded_while_both_sides_advance() {
        // Miri runs each step of the two threads, so the count is small there.
        const ITEMS: u64 = if cfg!(miri) { 300 } else { 100_000 };
        let ring = std::sync::Arc::new(Ring::new(8));
        let producer_ring = std::sync::Arc::clone(&ring);
        let producer = std::thread::spawn(move || {
            for id in 0..ITEMS {
                while !producer_ring.push(ev(id)) {
                    std::thread::yield_now();
                }
            }
        });
        let consumer_ring = std::sync::Arc::clone(&ring);
        let consumer = std::thread::spawn(move || {
            for expected in 0..ITEMS {
                loop {
                    if let Some(event) = consumer_ring.pop() {
                        assert_eq!(event.onset_id, expected);
                        break;
                    }
                    std::thread::yield_now();
                }
            }
        });
        while !producer.is_finished() || !consumer.is_finished() {
            assert!(ring.len() <= ring.capacity());
            std::hint::spin_loop();
        }
        producer.join().expect("producer");
        consumer.join().expect("consumer");
        assert_eq!(ring.len(), 0);
    }
}

#[cfg(test)]
mod ownership_tests {
    use super::tests::ev;
    use super::*;

    /// A second producer is refused in every build configuration. Only the
    /// extra debug panic depends on the configuration.
    #[test]
    fn a_second_producer_thread_is_refused_in_every_build() {
        let ring = std::sync::Arc::new(Ring::new(8));
        assert!(ring.push(ev(1)), "the first producer must be served");

        let other = std::sync::Arc::clone(&ring);
        let (accepted, panicked) = std::thread::spawn(move || {
            let previous = std::panic::take_hook();
            std::panic::set_hook(Box::new(|_| {}));
            let outcome =
                std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| other.push(ev(2))));
            std::panic::set_hook(previous);
            match outcome {
                Ok(accepted) => (accepted, false),
                Err(_) => (false, true),
            }
        })
        .join()
        .expect("thread");

        assert!(
            !accepted,
            "a second producer was served: that write raced through the UnsafeCell"
        );
        assert_eq!(
            panicked,
            cfg!(debug_assertions),
            "debug builds should also report the broken contract as a panic"
        );
        assert_eq!(
            ring.role_conflicts.load(Ordering::Relaxed),
            1,
            "the refused push must be counted as a role conflict, not capacity"
        );
        // The ring is intact: the legitimate producer's event is still there
        // and the intruder's never landed.
        assert_eq!(ring.pop().map(|e| e.onset_id), Some(1));
        assert_eq!(ring.pop().map(|e| e.onset_id), None);
    }

    /// A second CONSUMER is refused the same way, so a stray drain cannot
    /// steal events from the audio callback mid-stream.
    #[test]
    fn a_second_consumer_thread_is_refused_in_every_build() {
        let ring = std::sync::Arc::new(Ring::new(8));
        assert!(ring.push(ev(1)));
        assert!(ring.push(ev(2)));
        assert_eq!(ring.pop().map(|e| e.onset_id), Some(1));

        let other = std::sync::Arc::clone(&ring);
        let got = std::thread::spawn(move || {
            let previous = std::panic::take_hook();
            std::panic::set_hook(Box::new(|_| {}));
            let popped = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| other.pop()))
                .unwrap_or(None)
                .map(|e| e.onset_id);
            std::panic::set_hook(previous);
            popped
        })
        .join()
        .expect("thread");

        assert_eq!(got, None, "a second consumer must not be served an event");
        // and the real consumer still gets it
        assert_eq!(ring.pop().map(|e| e.onset_id), Some(2));
    }

    #[test]
    fn a_prefix_drain_cannot_take_another_threads_consumer_role() {
        let ring = std::sync::Arc::new(Ring::new(2));
        assert!(ring.push(ev(1)));
        assert!(ring.push(ev(2)));
        assert_eq!(ring.pop().map(|event| event.onset_id), Some(1));

        let other = std::sync::Arc::clone(&ring);
        let got = std::thread::spawn(move || {
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                other.drain_available().next().map(|event| event.onset_id)
            }))
            .unwrap_or(None)
        })
        .join()
        .expect("consumer thread");
        assert_eq!(got, None);
        assert_eq!(ring.role_conflicts.load(Ordering::Relaxed), 1);
        assert_eq!(ring.pop().map(|event| event.onset_id), Some(2));
    }

    /// A released consumer role passes to the next thread: the control thread
    /// drains what is left after the stream and its callback thread are gone.
    #[test]
    fn releasing_the_consumer_lets_another_thread_drain() {
        let ring = std::sync::Arc::new(Ring::new(8));
        ring.push(ev(1));
        ring.push(ev(2));

        let callback = std::sync::Arc::clone(&ring);
        std::thread::spawn(move || {
            callback.pop();
        })
        .join()
        .expect("callback thread");

        // SAFETY: the callback thread was joined above; it pops no more.
        unsafe { ring.release_consumer() };
        assert_eq!(
            ring.pop().map(|e| e.onset_id),
            Some(2),
            "drain must proceed"
        );
    }
}
