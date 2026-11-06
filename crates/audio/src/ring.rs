//! Wait-free SPSC ring carrying `Copy` POD events into the audio callback.
//!
//! Payloads must not run `Drop` on the audio thread: a deallocation there can
//! block for as long as the allocator needs, and the callback has a deadline.
//! Hence `Copy` POD only.

use std::cell::UnsafeCell;
use std::mem::MaybeUninit;
use std::sync::atomic::{AtomicU32, AtomicU64, AtomicUsize, Ordering, fence};
use std::sync::{Mutex, PoisonError};

use crate::backend::{CompactKabelControls, KabelControls};
use crate::partials::WaveLeases;
use crate::{DecodedSample, OscillatorControls, SampleControls, SampleId};

/// Scheduling metadata travels beside, rather than inside, the musical event.
/// Ordinary/offline callers continue to submit untracked `AudioEvent`s.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct QueuedAudioEvent<K = KabelControls> {
    pub event: AudioEvent<K>,
    pub confirmation: Option<crate::confirmation::ConfirmationOnset>,
    /// Expected immutable body for an actual sample onset. Unknown bodies
    /// may still play, but cannot certify a tracked recovery window.
    pub expected_sample_identity: Option<u64>,
}

impl<K> From<AudioEvent<K>> for QueuedAudioEvent<K> {
    fn from(event: AudioEvent<K>) -> Self {
        Self {
            event,
            confirmation: None,
            expected_sample_identity: None,
        }
    }
}

impl<K> std::ops::Deref for QueuedAudioEvent<K> {
    type Target = AudioEvent<K>;

    fn deref(&self) -> &Self::Target {
        &self.event
    }
}

/// A scheduled voice. The audio ring uses compact graph handles so its
/// payload stays `Copy` and has no owned pointers.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AudioEvent<K = KabelControls> {
    pub onset_id: u64,
    pub generation: u64,
    pub ui_visuals: u64,
    pub target_frame: u64,
    /// How far the source has advanced at `target_frame`, in frames.
    pub onset_lead: f32,
    pub freq_hz: f32,
    pub gain: f32,
    pub duration_secs: f32,
    pub controls: OscillatorControls<K>,
    pub sample: Option<SampleControls>,
    pub wavetable: Option<crate::backend::WavetableControls>,
    pub synth: Option<crate::backend::SynthSource>,
    /// Event-level choke group (see [`crate::OnsetEvent::cut`]): a new
    /// onset in the group cuts the previous one, whatever its source kind.
    pub cut: Option<f32>,
}

impl<K> AudioEvent<K> {
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
    buf: UnsafeCell<Box<[MaybeUninit<QueuedAudioEvent<CompactKabelControls>>]>>,
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
    /// The chunk the consumer renders, `rendering_from..rendering_until`
    /// frames at `rendering_rate`; see [`Ring::publish_frontier`]. Zero until
    /// the consumer publishes one.
    rendering_from: AtomicU64,
    rendering_until: AtomicU64,
    rendering_rate: AtomicU32,
    /// The restart lead of the newest cutover, in frames. A restart moves a
    /// note of its first window up to this far after its target: see
    /// [`Ring::push`].
    restart_lead: AtomicU32,
    /// The custom waves of pushed notes. Only the producer locks it.
    waves: Mutex<WaveLeases>,
}

// SAFETY: single producer / single consumer with atomic index publication,
// upheld by the callers documented above and enforced by the role claims in
// `push`/`pop`, which refuse a second thread in release as well as debug.
unsafe impl Sync for Ring {}
unsafe impl Send for Ring {}

/// A token for the current thread, taken from a counter on first use.
/// `ThreadId` has no stable integer form.
fn this_thread() -> u64 {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    thread_local! {
        // Never zero, so the slot's "unclaimed" value stays distinguishable.
        static TOKEN: u64 = NEXT.fetch_add(1, Ordering::Relaxed);
    }
    TOKEN.with(|token| *token)
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
            rendering_from: AtomicU64::new(0),
            rendering_until: AtomicU64::new(0),
            rendering_rate: AtomicU32::new(0),
            restart_lead: AtomicU32::new(0),
            waves: Mutex::default(),
        }
    }

    /// Producer side: the restart lead of the newest cutover, in frames.
    #[cfg(any(test, feature = "device-audio"))]
    pub(crate) fn set_restart_lead(&self, frames: u32) {
        self.restart_lead.store(frames, Ordering::Relaxed);
    }

    /// The restart lead of [`Self::set_restart_lead`].
    #[cfg(feature = "device-audio")]
    pub(crate) fn restart_lead(&self) -> u32 {
        self.restart_lead.load(Ordering::Relaxed)
    }

    /// Consumer side: publish the chunk about to render, frames `from..until`
    /// at `sample_rate`, before taking its events. Every frame before `from`
    /// is rendered, and an event taken now starts before `until`. The
    /// producer holds a pushed note's custom wave until a published `from`
    /// passes the note's last frame.
    pub(crate) fn publish_frontier(&self, from: u64, until: u64, sample_rate: u32) {
        self.rendering_rate.store(sample_rate, Ordering::Relaxed);
        self.rendering_from.store(from, Ordering::Relaxed);
        self.rendering_until.store(until, Ordering::Relaxed);
        // Pairs with the fence after a push: either this chunk's drain sees
        // the push, or the producer reads this chunk.
        fence(Ordering::SeqCst);
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

    #[cfg(any(test, feature = "device-audio"))]
    pub(crate) fn published_end(&self) -> usize {
        self.tail.load(Ordering::Acquire)
    }

    /// Consumer-side length through a captured producer cursor. A cursor
    /// behind the head has already been consumed, including across wrap.
    #[cfg(any(test, feature = "device-audio"))]
    pub(crate) fn len_before(&self, end: usize) -> usize {
        let len = end.wrapping_sub(self.head.load(Ordering::Relaxed));
        if len <= self.capacity() { len } else { 0 }
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
    pub fn drain_available(&self) -> impl Iterator<Item = AudioEvent<CompactKabelControls>> + '_ {
        std::iter::from_fn(|| self.pop()).take(self.len())
    }

    pub(crate) fn drain_queued_available(
        &self,
    ) -> impl Iterator<Item = QueuedAudioEvent<CompactKabelControls>> + '_ {
        std::iter::from_fn(|| self.pop_queued()).take(self.len())
    }

    #[cfg(any(test, feature = "device-audio"))]
    pub(crate) fn drain_queued_before(
        &self,
        end: usize,
    ) -> impl Iterator<Item = QueuedAudioEvent<CompactKabelControls>> + '_ {
        std::iter::from_fn(|| self.pop_queued()).take(self.len_before(end))
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
        if tail.wrapping_sub(head) == self.capacity() {
            self.refused.fetch_add(1, Ordering::Relaxed);
            return false;
        }
        let mut e = e.map_kabel(CompactKabelControls::from);
        let leased = e
            .event
            .controls
            .partials
            .as_mut()
            .and_then(crate::partials::lease)
            .map(|wave| {
                let event = &e.event;
                let hold = crate::partials::hold_frames(
                    event.duration_secs,
                    event.controls.envelope.release_secs,
                    event.target_frame,
                    crate::scalar::kabel_controls(&event.controls)
                        .map(|kabel| kabel.end_secs)
                        .reduce(f64::max),
                    self.rendering_rate.load(Ordering::Relaxed),
                );
                // The source of a note of a plugin chain starts up to the
                // latency of the chain after its target, and a restart moves
                // the target up to its restart lead later.
                let later = u64::from(event.controls.chain_latency())
                    + u64::from(self.restart_lead.load(Ordering::Relaxed));
                (wave, event.target_frame, hold.saturating_add(later))
            });
        // SAFETY: sole producer; slot unpublished until Release store.
        unsafe {
            (*self.buf.get())[tail & self.mask].write(e);
        }
        self.tail.store(tail.wrapping_add(1), Ordering::Release);
        let d = tail.wrapping_add(1).wrapping_sub(head);
        self.peak_depth.fetch_max(d, Ordering::Relaxed);
        let mut waves = self.waves.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some((wave, start, hold)) = leased {
            // Pairs with the fence in `publish_frontier`. A late note starts
            // when the consumer takes it, before the chunk end read here.
            fence(Ordering::SeqCst);
            let until = self.rendering_until.load(Ordering::Relaxed);
            waves.hold(wave, start.max(until).saturating_add(hold));
        }
        waves.release_before(self.rendering_from.load(Ordering::Relaxed));
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

    /// Protect code referenced by the consumer snapshot without releasing slots.
    /// This is a consumer operation under the same single-thread rule as `pop`.
    /// The fixed-purpose visitor cannot reenter this ring and release its head.
    pub(crate) fn protect_kabel_programs(&self, scalar: &mut crate::ScalarBackend) {
        if !claim(&self.consumer, "consumer") {
            self.role_conflicts.fetch_add(1, Ordering::Relaxed);
            debug_assert!(false, "the audio ring allows exactly one consumer");
            return;
        }
        let head = self.head.load(Ordering::Relaxed);
        let tail = self.tail.load(Ordering::Acquire);
        for offset in 0..tail.wrapping_sub(head) {
            let index = head.wrapping_add(offset);
            // SAFETY: the sole consumer retains the head throughout this visit.
            // Acquire observes initialized slots up to tail. The producer cannot
            // overwrite any of those slots until the consumer releases the head.
            let event = unsafe { (*self.buf.get())[index & self.mask].assume_init_ref() };
            for controls in crate::scalar::kabel_controls(&event.controls) {
                scalar.protect_kabel_program(controls.program);
            }
        }
    }

    pub fn pop(&self) -> Option<AudioEvent<CompactKabelControls>> {
        self.pop_queued().map(|queued| queued.event)
    }

    fn pop_queued(&self) -> Option<QueuedAudioEvent<CompactKabelControls>> {
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
        self.head.store(head.wrapping_add(1), Ordering::Release);
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

    /// A pushed note holds its custom wave until the consumer starts a chunk
    /// past the note's last frame, also after the registry lets the wave go.
    #[test]
    fn a_pushed_note_holds_its_wave_until_the_consumer_passes_it() {
        // A term no other test registers, so the wave is this test's alone.
        let mut partials = crate::register_partials(1, &[0.0], &[0.123_456_7]);
        let ring = Ring::new(4);
        ring.publish_frontier(0, 128, 48_000);
        let mut note = ev(0);
        note.controls.partials = Some(partials);
        assert!(ring.push(note));
        crate::partials::forget(&partials);
        let wave = ring
            .pop()
            .and_then(|note| note.controls.partials)
            .and_then(|partials| partials.wave)
            .expect("a leased wave");
        let wave = wave.wave();
        assert!(wave.sample(0.25, wave.selection(440.0, 48_000.0)) > 0.1);
        // The note lasts 0.11 s. Once the consumer is past it, the next push
        // lets the wave go.
        ring.publish_frontier(48_000, 48_128, 48_000);
        assert!(ring.push(ev(1)));
        assert_eq!(ring.waves.lock().unwrap().len(), 0);
        assert!(crate::partials::lease(&mut partials).is_none());

        // The source of a note of a plugin chain with 480 frames of latency
        // starts up to that latency after its target, and a restart moves
        // the target up to its restart lead later, 240 frames here. The wave
        // is held 720 frames longer.
        let mut partials = crate::register_partials(1, &[0.0], &[0.234_567_8]);
        let ring = Ring::new(4);
        ring.publish_frontier(0, 128, 48_000);
        ring.set_restart_lead(240);
        let mut note = ev(0);
        note.controls.partials = Some(partials);
        let stage = crate::InsertControls::new(crate::InsertKey::default());
        note.controls.effects[0] = Some(stage.with_latency(480));
        let release = note.controls.envelope.release_secs;
        assert!(ring.push(note));
        crate::partials::forget(&partials);
        assert!(ring.pop().is_some());
        let plain = 128 + crate::partials::hold_frames(0.1, release, 0, None, 48_000);
        let until = plain + 480 + 240;
        for (from, held) in [(until - 1, 1), (until, 0)] {
            ring.publish_frontier(from, from + 128, 48_000);
            assert!(ring.push(ev(1)));
            assert!(ring.pop().is_some());
            assert_eq!(ring.waves.lock().unwrap().len(), held, "from {from}");
        }
        assert!(crate::partials::lease(&mut partials).is_none());
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

        for cursor in [0, usize::MAX - 1] {
            let ring = Ring::new(4);
            ring.head.store(cursor, Ordering::Relaxed);
            ring.tail.store(cursor, Ordering::Relaxed);
            for id in 0..3 {
                assert!(ring.push(ev(id)));
            }
            assert_eq!(ring.len(), 3);
            let end = ring.published_end();
            assert_eq!(ring.pop().unwrap().onset_id, 0);
            assert!(ring.push(ev(3)));
            assert_eq!(
                ring.drain_queued_before(end)
                    .map(|queued| queued.event.onset_id)
                    .collect::<Vec<_>>(),
                [1, 2]
            );
            assert_eq!(ring.drain_queued_before(end).count(), 0);
            assert_eq!(ring.pop().unwrap().onset_id, 3);
            assert_eq!(ring.drain_queued_before(end).count(), 0);
        }
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
        assert!(!std::mem::needs_drop::<AudioEvent<CompactKabelControls>>());
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

impl<K> QueuedAudioEvent<K> {
    pub(crate) fn map_kabel<L>(self, mut map: impl FnMut(K) -> L) -> QueuedAudioEvent<L> {
        QueuedAudioEvent {
            event: self.event.map_kabel(&mut map),
            confirmation: self.confirmation,
            expected_sample_identity: self.expected_sample_identity,
        }
    }
}

impl<K> AudioEvent<K> {
    pub(crate) fn map_kabel<L>(self, mut map: impl FnMut(K) -> L) -> AudioEvent<L> {
        AudioEvent {
            onset_id: self.onset_id,
            generation: self.generation,
            ui_visuals: self.ui_visuals,
            target_frame: self.target_frame,
            onset_lead: self.onset_lead,
            freq_hz: self.freq_hz,
            gain: self.gain,
            duration_secs: self.duration_secs,
            controls: self.controls.map_kabel(&mut map),
            sample: self.sample,
            wavetable: self.wavetable,
            synth: self.synth,
            cut: self.cut,
        }
    }
}

#[cfg(test)]
mod kabel_snapshot_tests {
    use super::*;
    use crate::{FilterControls, FxStage, ScalarBackend};
    use rustel_kabel::{Program, ProgramId};

    fn program(value: usize) -> Program {
        Program::from_json(&format!(r#"{{"nodes":[{{"type":"n","value":{value}}}]}}"#)).unwrap()
    }

    fn controls(program: Program) -> KabelControls {
        KabelControls {
            program: program.into(),
            start_secs: 0.0,
            gate_end_secs: 1.0,
            end_secs: 1.0,
        }
    }

    fn event(id: u64, program: Program) -> AudioEvent {
        let mut event = super::tests::ev(id);
        event.controls.kabel = Some(controls(program));
        event
    }

    fn stage(program: Program) -> FxStage {
        FxStage {
            kabel: Some(controls(program)),
            stretch: None,
            transient: None,
            gain: 1.0,
            filters: FilterControls::default(),
            vowel: None,
            coarse: None,
            crush: None,
            shape: None,
            distort: None,
            tremolo: None,
            compressor: None,
            pan_x: None,
            phaser: None,
            delay: None,
            dry: 1.0,
            room: None,
        }
    }

    fn bank(programs: &[Program]) -> ScalarBackend {
        let mut backend = ScalarBackend::with_kabel_program_bank_for_test(programs.len());
        for program in programs {
            let entry = Box::new(crate::assets::PreparedKabelProgram {
                program: *program,
                until_frame: 0,
            });
            assert!(backend.install_kabel_program(entry, drop).is_none());
        }
        backend
    }

    fn retire(backend: &mut ScalarBackend) -> Vec<ProgramId> {
        let mut retired = Vec::new();
        backend.retire_kabel_programs(16, |entry| retired.push(entry.program.id()));
        retired.sort();
        retired
    }

    #[test]
    fn empty_snapshot_never_reads_uninitialized_slots() {
        let code = program(1);
        let mut backend = bank(&[code]);
        Ring::new(4).protect_kabel_programs(&mut backend);
        assert_eq!(retire(&mut backend), [code.id()]);
    }

    #[test]
    fn wrapped_snapshot_protects_main_and_every_fx_without_consuming() {
        let codes: Vec<_> = (0..6).map(program).collect();
        let mut backend = bank(&codes);
        let ring = Ring::new(4);
        for id in 0..3 {
            assert!(ring.push(super::tests::ev(id)));
            assert_eq!(ring.pop().unwrap().onset_id, id);
        }
        let mut first = event(3, codes[0]);
        for (index, slot) in first.controls.fx_stages.iter_mut().enumerate() {
            *slot = Some(stage(codes[index + 1]));
        }
        assert!(ring.push(first));
        assert!(ring.push(event(4, codes[4])));
        assert!(ring.push(super::tests::ev(5)));
        assert!(ring.push(event(6, codes[0])));
        backend.begin_kabel_program_retirement();
        ring.protect_kabel_programs(&mut backend);
        assert_eq!(retire(&mut backend), [codes[5].id()]);
        assert_eq!(
            ring.drain_available()
                .map(|event| event.onset_id)
                .collect::<Vec<_>>(),
            [3, 4, 5, 6]
        );
        backend.begin_kabel_program_retirement();
        ring.protect_kabel_programs(&mut backend);
        assert_eq!(
            retire(&mut backend),
            codes[..5].iter().map(Program::id).collect::<Vec<_>>()
        );
    }

    #[test]
    fn snapshot_protects_programs_when_counters_wrap() {
        let codes: Vec<_> = (0..4).map(program).collect();
        let mut backend = bank(&codes);
        let ring = Ring::new(4);
        ring.head.store(usize::MAX - 1, Ordering::Relaxed);
        ring.tail.store(usize::MAX - 1, Ordering::Relaxed);
        for (index, code) in codes[..3].iter().enumerate() {
            assert!(ring.push(event(index as u64, *code)));
        }

        backend.begin_kabel_program_retirement();
        ring.protect_kabel_programs(&mut backend);
        assert_eq!(retire(&mut backend), [codes[3].id()]);
        assert_eq!(
            ring.drain_available()
                .map(|event| event.onset_id)
                .collect::<Vec<_>>(),
            [0, 1, 2]
        );
        backend.begin_kabel_program_retirement();
        ring.protect_kabel_programs(&mut backend);
        assert_eq!(
            retire(&mut backend),
            codes[..3].iter().map(Program::id).collect::<Vec<_>>()
        );
    }

    #[test]
    fn snapshot_refuses_a_second_consumer_thread() {
        let ring = std::sync::Arc::new(Ring::new(2));
        assert!(ring.pop().is_none());
        let other = std::sync::Arc::clone(&ring);
        let outcome = std::thread::spawn(move || {
            let mut backend = bank(&[]);
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                other.protect_kabel_programs(&mut backend);
            }))
        })
        .join()
        .unwrap();
        assert_eq!(outcome.is_err(), cfg!(debug_assertions));
        assert_eq!(ring.role_conflicts.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn snapshots_share_published_slots_safely_with_the_producer() {
        const ITEMS: u64 = if cfg!(miri) { 8 } else { 512 };
        let code = program(1);
        let ring = std::sync::Arc::new(Ring::new(4));
        let producer_ring = std::sync::Arc::clone(&ring);
        let producer = std::thread::spawn(move || {
            for id in 0..ITEMS {
                while !producer_ring.push(event(id, code)) {
                    std::thread::yield_now();
                }
            }
        });
        let mut backend = bank(&[code]);
        for expected in 0..ITEMS {
            let event = loop {
                ring.protect_kabel_programs(&mut backend);
                if let Some(event) = ring.pop() {
                    break event;
                }
                std::thread::yield_now();
            };
            assert_eq!(event.onset_id, expected);
            assert_eq!(event.controls.kabel.unwrap().program, code.id());
        }
        producer.join().unwrap();
        assert_eq!(ring.role_conflicts.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn callback_payload_contains_handles_instead_of_inline_programs() {
        use std::mem::{needs_drop, size_of};
        let compact = size_of::<QueuedAudioEvent<CompactKabelControls>>();
        let producer = size_of::<QueuedAudioEvent>();
        assert_eq!(size_of::<CompactKabelControls>(), 32);
        // A note also carries its insert chain, its effects and its
        // instrument, with their parameter values.
        let inserts = size_of::<Option<u64>>()
            + (crate::insert::EFFECT_CHAIN + 1)
                * size_of::<Option<crate::insert::InsertControls>>();
        assert!(
            compact <= 4352 + inserts,
            "callback event grew to {compact} bytes"
        );
        // The lead of a note finds no room in the padding of its controls.
        // So each event grows by 8 bytes from 4944, and the live ring of
        // 8192 events by 64 KiB.
        assert_eq!(compact, 4952);
        assert!(compact <= size_of::<QueuedAudioEvent<()>>() + 4 * 32);
        assert_eq!(producer, compact, "producer events must share graph code");
        assert!(!needs_drop::<QueuedAudioEvent<CompactKabelControls>>());
        assert!(!needs_drop::<OscillatorControls<CompactKabelControls>>());
    }
}
