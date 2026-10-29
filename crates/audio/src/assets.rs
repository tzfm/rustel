//! Cross-thread sample delivery that keeps the audio callback allocation-
//! and free-free.
//!
//! The producer decodes samples, boxes them, and pushes raw pointers through
//! a fixed SPSC install ring. The audio callback pops between blocks, writes
//! the pointer into its [`crate::sample::SampleBank`] slot, and pushes any
//! displaced pointer through the return ring, where the producer reclaims
//! and frees it. Ownership is a strict baton pass: exactly one side owns a
//! pointer at any time, and only the producer ever allocates or frees.
//! An FX install can displace several rooms. Its drain reserves return slots
//! before it takes each install, and defers work while the return ring is full.

use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

use crate::sample::{DecodedSample, SampleId};

/// One pending install. `Copy` raw payload so ring slots stay POD.
#[derive(Clone, Copy)]
pub(crate) struct SampleInstall {
    pub id: SampleId,
    pub sample: *mut DecodedSample,
}

/// A displaced sample travelling back for the producer to free.
#[derive(Clone, Copy)]
pub(crate) struct ReturnedSample(pub *mut DecodedSample);

// SAFETY: the pointers are batons - exactly one side owns one at any time
// (producer boxes → callback installs/displaces → producer frees), enforced
// by the SPSC handshake below. `DecodedSample` itself is Send.
unsafe impl Send for SampleInstall {}
unsafe impl Send for ReturnedSample {}

#[cfg(any(feature = "device-audio", test))]
const ASSET_RING_CAPACITY: usize = 64;

/// Minimal SPSC ring of `Copy` payloads. Same discipline as the event ring:
/// the producer owns `tail`, the consumer owns `head`, slots are plain data.
pub(crate) struct AssetRing<T: Copy> {
    slots: Vec<std::cell::UnsafeCell<Option<T>>>,
    head: AtomicUsize,
    tail: AtomicUsize,
}

// SAFETY: single-producer/single-consumer by construction (the device holds
// the producer side, the callback the consumer side); each slot is touched
// by exactly one side at a time via the head/tail handshake.
unsafe impl<T: Copy + Send> Send for AssetRing<T> {}
unsafe impl<T: Copy + Send> Sync for AssetRing<T> {}

impl<T: Copy> AssetRing<T> {
    #[cfg(any(feature = "device-audio", test))]
    pub fn new() -> Self {
        Self::with_capacity(ASSET_RING_CAPACITY)
    }

    #[cfg(any(feature = "device-audio", test))]
    fn with_capacity(capacity: usize) -> Self {
        let mut slots = Vec::with_capacity(capacity);
        slots.resize_with(capacity, || std::cell::UnsafeCell::new(None));
        Self {
            slots,
            head: AtomicUsize::new(0),
            tail: AtomicUsize::new(0),
        }
    }

    pub fn push(&self, value: T) -> Result<(), T> {
        let tail = self.tail.load(Ordering::Relaxed);
        let head = self.head.load(Ordering::Acquire);
        if tail.wrapping_sub(head) >= self.slots.len() {
            return Err(value);
        }
        let slot = &self.slots[tail % self.slots.len()];
        // SAFETY: `tail` is unpublished, so the consumer cannot read this
        // slot until the Release store below.
        unsafe { *slot.get() = Some(value) };
        self.tail.store(tail.wrapping_add(1), Ordering::Release);
        Ok(())
    }

    pub fn pop(&self) -> Option<T> {
        let head = self.head.load(Ordering::Relaxed);
        let tail = self.tail.load(Ordering::Acquire);
        if head == tail {
            return None;
        }
        let slot = &self.slots[head % self.slots.len()];
        // SAFETY: `head` is published only after the take, and the producer
        // cannot reuse the slot until then.
        let value = unsafe { (*slot.get()).take() };
        self.head.store(head.wrapping_add(1), Ordering::Release);
        value
    }

    /// Drain only the prefix observed here; concurrent arrivals belong to a
    /// later drain so a refilling producer cannot prolong an audio callback.
    pub fn drain_available(&self) -> impl Iterator<Item = T> + '_ {
        std::iter::from_fn(|| self.pop()).take(self.len())
    }

    pub fn len(&self) -> usize {
        let head = self.head.load(Ordering::Acquire);
        self.tail
            .load(Ordering::Acquire)
            .wrapping_sub(head)
            .min(self.slots.len())
    }

    pub fn capacity(&self) -> usize {
        self.slots.len()
    }
}

/// One pending orbit-reverb install (same baton discipline as samples).
#[derive(Clone, Copy)]
pub(crate) struct ReverbInstall {
    pub orbit: u8,
    pub reverb: *mut crate::reverb::OrbitReverb,
}

#[derive(Clone, Copy)]
pub(crate) struct ReturnedReverb(pub *mut crate::reverb::OrbitReverb);

/// One pending `.FX()` stage reverb. Unlike orbit installs there is no orbit
/// index: the callback drops the box into [`crate::scalar::ScalarBackend`]'s
/// stage pool.
#[derive(Clone, Copy)]
pub(crate) struct FxReverbInstall {
    pub reverb: *mut crate::reverb::OrbitReverb,
}

#[derive(Clone, Copy)]
pub(crate) struct ReturnedFxReverb(pub *mut crate::reverb::OrbitReverb);

/// A producer-prepared graph and its return baton. Graph storage is never
/// allocated or destroyed by the callback.
#[derive(Clone, Copy)]
pub(crate) struct KabelInstall {
    pub graph: *mut rustel_kabel::Graph,
}
#[derive(Clone, Copy)]
pub(crate) struct ReturnedKabel(pub *mut rustel_kabel::Graph);

pub(crate) const MAX_KABEL_PROGRAMS: usize = 4096;

/// Immutable graph code retained until every queued use has finished.
pub(crate) struct PreparedKabelProgram {
    pub program: rustel_kabel::Program,
    pub until_frame: u64,
}

#[derive(Clone, Copy)]
pub(crate) struct ProgramInstall {
    pub program: *mut PreparedKabelProgram,
}

#[derive(Clone, Copy)]
pub(crate) struct ReturnedProgram(pub *mut PreparedKabelProgram);

// SAFETY: the SPSC baton pass transfers unique ownership of each allocation.
unsafe impl Send for ProgramInstall {}
unsafe impl Send for ReturnedProgram {}

// SAFETY: unique graph ownership moves through the same SPSC baton pass.
unsafe impl Send for KabelInstall {}
unsafe impl Send for ReturnedKabel {}

// SAFETY: same SPSC baton pass as the sample pointers below.
unsafe impl Send for ReverbInstall {}
unsafe impl Send for ReturnedReverb {}
unsafe impl Send for FxReverbInstall {}
unsafe impl Send for ReturnedFxReverb {}

/// One pending orbit insert, with the same baton discipline as a reverb.
#[derive(Clone, Copy)]
pub(crate) struct InsertInstall {
    pub slot: crate::insert::InsertSlot,
    /// The rate the insert was prepared for. An output at a different rate
    /// does not take the insert.
    pub sample_rate: u32,
    pub insert: *mut dyn crate::insert::OrbitInsert,
}

#[derive(Clone, Copy)]
pub(crate) struct ReturnedInsert(pub *mut dyn crate::insert::OrbitInsert);

// SAFETY: same SPSC baton pass. `OrbitInsert` is `Send`.
unsafe impl Send for InsertInstall {}
unsafe impl Send for ReturnedInsert {}

/// A copy the producer asks the callback to give back, by its slot and its
/// key. A release holds no box.
#[derive(Clone, Copy)]
pub(crate) struct InsertRelease {
    pub slot: crate::insert::InsertSlot,
    pub key: crate::insert::InsertKey,
}

/// The plugin workers of an output on their way to the callback: one count
/// of their `Arc`, from `Arc::into_raw`. The device keeps its own count, so
/// the callback never drops the last one.
#[derive(Clone, Copy)]
pub(crate) struct PoolInstall(pub *const crate::pool::PoolShared);

// SAFETY: the count crosses the SPSC ring once, and one side owns it at a
// time. The pool is `Send` and `Sync`.
unsafe impl Send for PoolInstall {}

/// Send lines on their way to the callback, with the same baton discipline
/// as a reverb: see [`crate::scalar::SendLines`].
#[derive(Clone, Copy)]
pub(crate) struct SendLineInstall(pub *mut crate::scalar::SendLines);

/// The send lines the callback replaced, on their way back to be freed.
#[derive(Clone, Copy)]
pub(crate) struct ReturnedSendLines(pub *mut crate::scalar::SendLines);

// SAFETY: same SPSC baton pass: the producer boxes the lines, the callback
// takes them and returns the lines they replace, and the producer frees
// those. One side owns each box at a time. `SendLines` is `Send`.
unsafe impl Send for SendLineInstall {}
unsafe impl Send for ReturnedSendLines {}

const KABEL_RETIRING: usize = 1 << (usize::BITS - 1);

/// Keeps prepared graph programs alive until a scheduling window has been
/// published or discarded. Hold this on the producer across queue backpressure.
#[derive(Default)]
#[must_use = "Keep this reservation until prepared events are published or discarded"]
pub struct KabelPublication {
    channel: Option<std::sync::Arc<SampleChannel>>,
}

impl std::fmt::Debug for KabelPublication {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("KabelPublication")
            .field("active", &self.channel.is_some())
            .finish()
    }
}

impl KabelPublication {
    #[cfg(feature = "device-audio")]
    pub(crate) fn is_active(&self) -> bool {
        self.channel.is_some()
    }

    #[cfg(any(feature = "device-audio", test))]
    pub(crate) fn new(channel: std::sync::Arc<SampleChannel>) -> Self {
        let state = &channel.active_kabel_publications;
        let mut current = state.load(Ordering::Acquire);
        loop {
            if current == KABEL_RETIRING {
                // Only the producer waits. Callback retirement is bounded and
                // releases its reservation during unwinding as well.
                std::thread::yield_now();
                current = state.load(Ordering::Acquire);
                continue;
            }
            assert!(
                current < KABEL_RETIRING - 1,
                "too many KabelSalat publications"
            );
            match state.compare_exchange_weak(
                current,
                current + 1,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => break,
                Err(observed) => current = observed,
            }
        }
        Self {
            channel: Some(channel),
        }
    }
}

impl Drop for KabelPublication {
    fn drop(&mut self) {
        if let Some(channel) = &self.channel {
            // Every event publication precedes this release. The callback reads
            // it before scanning event rings, so an ended reservation cannot
            // hide an event that still references its program.
            channel
                .active_kabel_publications
                .fetch_sub(1, Ordering::Release);
        }
    }
}

pub(crate) struct KabelRetirement<'a>(&'a AtomicUsize);

impl Drop for KabelRetirement<'_> {
    fn drop(&mut self) {
        // Returns precede the next producer's acquisition and cache reclamation.
        self.0.store(0, Ordering::Release);
    }
}

/// Both directions plus the leak counter, shared by the device (producer
/// side) and the callback (consumer side).
pub(crate) struct SampleChannel {
    pub installs: AssetRing<SampleInstall>,
    pub returns: AssetRing<ReturnedSample>,
    pub reverb_installs: AssetRing<ReverbInstall>,
    pub reverb_returns: AssetRing<ReturnedReverb>,
    pub fx_reverb_installs: AssetRing<FxReverbInstall>,
    pub fx_reverb_returns: AssetRing<ReturnedFxReverb>,
    pub insert_installs: AssetRing<InsertInstall>,
    pub insert_returns: AssetRing<ReturnedInsert>,
    pub insert_releases: AssetRing<InsertRelease>,
    /// One place: the device ships its plugin workers to each backend once.
    pub pool_installs: AssetRing<PoolInstall>,
    /// One place each: the send lines on their way to the callback, and the
    /// lines they replaced on their way back.
    pub send_line_installs: AssetRing<SendLineInstall>,
    pub send_line_returns: AssetRing<ReturnedSendLines>,
    /// Stage reverbs refused by the install queue or callback byte budget.
    pub fx_reverb_refusals: AtomicU64,
    pub insert_refusals: AtomicU64,
    pub kabel_installs: AssetRing<KabelInstall>,
    pub kabel_returns: AssetRing<ReturnedKabel>,
    pub program_installs: AssetRing<ProgramInstall>,
    pub program_returns: AssetRing<ReturnedProgram>,
    pub program_resident_bytes: AtomicUsize,
    pub active_kabel_publications: AtomicUsize,
    pub kabel_refusals: AtomicU64,
    pub kabel_resident_bytes: AtomicUsize,
    /// Stage reverb bytes currently admitted by the callback, including
    /// boxes leased to active voices.
    pub fx_reverb_resident_bytes: AtomicUsize,
    /// Displaced samples the callback could not return because the return
    /// ring was momentarily full. They are intentionally leaked rather than
    /// freed on the audio thread; a non-zero count is a producer-contract
    /// violation (reclaim before pushing) worth surfacing in telemetry.
    pub leaked: AtomicU64,
}

impl SampleChannel {
    /// Excludes new producer reservations while the callback returns idle state.
    /// The callback makes one attempt and never waits for a producer.
    pub(crate) fn try_kabel_retirement(&self) -> Option<KabelRetirement<'_>> {
        self.active_kabel_publications
            .compare_exchange(0, KABEL_RETIRING, Ordering::AcqRel, Ordering::Relaxed)
            .ok()
            .map(|_| KabelRetirement(&self.active_kabel_publications))
    }

    #[cfg(any(feature = "device-audio", test))]
    pub fn new() -> Self {
        Self {
            installs: AssetRing::new(),
            returns: AssetRing::new(),
            reverb_installs: AssetRing::new(),
            reverb_returns: AssetRing::new(),
            fx_reverb_installs: AssetRing::new(),
            fx_reverb_returns: AssetRing::new(),
            // A place for each slot: one turn ships each insert of a start.
            insert_installs: AssetRing::with_capacity(crate::insert::INSERT_SLOTS),
            insert_returns: AssetRing::with_capacity(crate::insert::INSERT_SLOTS),
            insert_releases: AssetRing::with_capacity(crate::insert::INSERT_SLOTS),
            pool_installs: AssetRing::with_capacity(1),
            send_line_installs: AssetRing::with_capacity(1),
            send_line_returns: AssetRing::with_capacity(1),
            fx_reverb_refusals: AtomicU64::new(0),
            insert_refusals: AtomicU64::new(0),
            kabel_installs: AssetRing::new(),
            kabel_returns: AssetRing::new(),
            program_installs: AssetRing::with_capacity(512),
            program_returns: AssetRing::with_capacity(512),
            program_resident_bytes: AtomicUsize::new(0),
            active_kabel_publications: AtomicUsize::new(0),
            kabel_refusals: AtomicU64::new(0),
            kabel_resident_bytes: AtomicUsize::new(0),
            fx_reverb_resident_bytes: AtomicUsize::new(0),
            leaked: AtomicU64::new(0),
        }
    }

    /// Producer side: free every returned sample. Call only from the single
    /// producer thread (before pushing new installs). The return rings are
    /// SPSC - a second reclaiming thread races the consumer head and can
    /// double-free.
    pub fn reclaim(&self) {
        self.reclaim_with_fx(|_| {});
    }

    /// Like [`Self::reclaim`], and `returned_fx` sees each returned stage
    /// reverb before it is freed. The producer uses it to drop the cached
    /// fingerprint of a box that the callback evicted or refused.
    pub fn reclaim_with_fx(&self, returned_fx: impl FnMut(*mut crate::reverb::OrbitReverb)) {
        self.reclaim_with_processors(returned_fx, |_| {});
    }

    pub fn reclaim_with_processors(
        &self,
        returned_fx: impl FnMut(*mut crate::reverb::OrbitReverb),
        returned_kabel: impl FnMut(*mut rustel_kabel::Graph),
    ) {
        self.reclaim_with_programs(returned_fx, returned_kabel, |_| {});
    }

    pub fn reclaim_with_programs(
        &self,
        returned_fx: impl FnMut(*mut crate::reverb::OrbitReverb),
        returned_kabel: impl FnMut(*mut rustel_kabel::Graph),
        returned_program: impl FnMut(*mut PreparedKabelProgram),
    ) {
        self.reclaim_with_inserts(returned_fx, returned_kabel, returned_program, drop);
    }

    /// Like [`Self::reclaim_with_programs`], and `returned_insert` takes each
    /// insert the callback gave back. The caller frees it.
    pub fn reclaim_with_inserts(
        &self,
        mut returned_fx: impl FnMut(*mut crate::reverb::OrbitReverb),
        mut returned_kabel: impl FnMut(*mut rustel_kabel::Graph),
        mut returned_program: impl FnMut(*mut PreparedKabelProgram),
        mut returned_insert: impl FnMut(Box<dyn crate::insert::OrbitInsert>),
    ) {
        while let Some(returned) = self.program_returns.pop() {
            returned_program(returned.0);
            // SAFETY: the callback returned the unique program baton.
            drop(unsafe { Box::from_raw(returned.0) });
        }
        while let Some(returned) = self.returns.pop() {
            // SAFETY: the pointer completed the full baton pass - boxed by
            // the producer, installed and displaced by the callback - and
            // this is its unique owner now.
            drop(unsafe { Box::from_raw(returned.0) });
        }
        while let Some(returned) = self.reverb_returns.pop() {
            // SAFETY: same pass for reverbs.
            drop(unsafe { Box::from_raw(returned.0) });
        }
        while let Some(returned) = self.kabel_returns.pop() {
            returned_kabel(returned.0);
            // SAFETY: the callback returned unique ownership to the producer.
            drop(unsafe { Box::from_raw(returned.0) });
        }
        while let Some(returned) = self.fx_reverb_returns.pop() {
            returned_fx(returned.0);
            // SAFETY: same pass for stage reverbs retired by the callback.
            drop(unsafe { Box::from_raw(returned.0) });
        }
        while let Some(returned) = self.insert_returns.pop() {
            // SAFETY: same pass for inserts displaced by the callback.
            returned_insert(unsafe { Box::from_raw(returned.0) });
        }
        while let Some(returned) = self.send_line_returns.pop() {
            // SAFETY: same pass for send lines the callback replaced.
            drop(unsafe { Box::from_raw(returned.0) });
        }
    }
}

impl Drop for SampleChannel {
    fn drop(&mut self) {
        // Whoever drops the channel owns both queues exclusively.
        self.reclaim();
        while let Some(install) = self.installs.pop() {
            if install.sample.is_null() {
                continue;
            }
            // SAFETY: never reached the consumer; unique ownership here.
            drop(unsafe { Box::from_raw(install.sample) });
        }
        while let Some(install) = self.reverb_installs.pop() {
            // SAFETY: never reached the consumer; unique ownership here.
            drop(unsafe { Box::from_raw(install.reverb) });
        }
        while let Some(install) = self.program_installs.pop() {
            // SAFETY: this program never left the producer queue.
            drop(unsafe { Box::from_raw(install.program) });
        }
        while let Some(install) = self.kabel_installs.pop() {
            // SAFETY: the install never left the producer queue.
            drop(unsafe { Box::from_raw(install.graph) });
        }
        while let Some(install) = self.fx_reverb_installs.pop() {
            // SAFETY: never reached the consumer; unique ownership here.
            drop(unsafe { Box::from_raw(install.reverb) });
        }
        while let Some(install) = self.insert_installs.pop() {
            // SAFETY: never reached the consumer. Unique ownership here.
            drop(unsafe { Box::from_raw(install.insert) });
        }
        while let Some(install) = self.pool_installs.pop() {
            // SAFETY: the count never reached the consumer, and this side
            // owns it.
            drop(unsafe { std::sync::Arc::from_raw(install.0) });
        }
        while let Some(install) = self.send_line_installs.pop() {
            // SAFETY: never reached the consumer. Unique ownership here.
            drop(unsafe { Box::from_raw(install.0) });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ring_round_trips_and_refuses_when_full() {
        let ring: AssetRing<u32> = AssetRing::new();
        for value in 0..ASSET_RING_CAPACITY as u32 {
            assert!(ring.push(value).is_ok());
        }
        assert_eq!(ring.len(), ASSET_RING_CAPACITY);
        assert_eq!(ring.capacity(), ASSET_RING_CAPACITY);
        assert_eq!(ring.push(999), Err(999), "full ring must refuse");
        for value in 0..ASSET_RING_CAPACITY as u32 {
            assert_eq!(ring.pop(), Some(value));
        }
        assert_eq!(ring.pop(), None);
        assert_eq!(ring.len(), 0);
    }

    #[test]
    fn draining_a_prefix_defers_concurrent_refills() {
        let ring = AssetRing::new();
        for value in 0..3 {
            ring.push(value).unwrap();
        }
        let mut prefix = ring.drain_available();
        // Each consumer step releases a slot which the producer immediately
        // refills. Neither queue capacity nor waiting for empty bounds this.
        for value in 0..3 {
            assert_eq!(prefix.next(), Some(value));
            ring.push(value + 3).unwrap();
        }
        assert_eq!(prefix.next(), None, "the captured prefix is complete");
        assert_eq!(ring.drain_available().collect::<Vec<_>>(), [3, 4, 5]);

        let mut empty = ring.drain_available();
        ring.push(6).unwrap();
        assert_eq!(empty.next(), None, "an empty prefix stays empty");
        assert_eq!(ring.pop(), Some(6));
    }

    #[test]
    fn draining_a_full_wrapped_prefix_preserves_refill_order() {
        let ring = AssetRing::new();
        for value in 0..ASSET_RING_CAPACITY - 3 {
            ring.push(value).unwrap();
            assert_eq!(ring.pop(), Some(value));
        }
        for value in 0..ASSET_RING_CAPACITY {
            ring.push(value).unwrap();
        }
        let mut prefix = ring.drain_available();
        for value in 0..ASSET_RING_CAPACITY {
            assert_eq!(prefix.next(), Some(value));
            ring.push(value + ASSET_RING_CAPACITY).unwrap();
        }
        assert_eq!(prefix.next(), None);
        assert_eq!(
            ring.drain_available().collect::<Vec<_>>(),
            (ASSET_RING_CAPACITY..2 * ASSET_RING_CAPACITY).collect::<Vec<_>>()
        );
    }

    #[test]
    fn observed_depth_stays_bounded_while_both_sides_advance() {
        // Miri runs each step of the two threads, so the count is small there.
        const ITEMS: u32 = if cfg!(miri) { 300 } else { 100_000 };
        let ring = std::sync::Arc::new(AssetRing::new());
        let producer_ring = std::sync::Arc::clone(&ring);
        let producer = std::thread::spawn(move || {
            for value in 0..ITEMS {
                let mut pending = value;
                loop {
                    match producer_ring.push(pending) {
                        Ok(()) => break,
                        Err(value) => {
                            pending = value;
                            std::thread::yield_now();
                        }
                    }
                }
            }
        });
        let consumer_ring = std::sync::Arc::clone(&ring);
        let consumer = std::thread::spawn(move || {
            for expected in 0..ITEMS {
                loop {
                    if let Some(value) = consumer_ring.pop() {
                        assert_eq!(value, expected);
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

    fn boxed_sample() -> *mut DecodedSample {
        Box::into_raw(Box::new(
            crate::sample::decode_wav(include_bytes!("../assets/bd.wav")).expect("bundled bd"),
        ))
    }

    /// An 8 frame room, so the build stays short.
    fn boxed_room() -> *mut crate::reverb::OrbitReverb {
        Box::into_raw(Box::new(crate::reverb::OrbitReverb::generate(
            8_000,
            crate::reverb::ReverbParams {
                size_secs: 0.001,
                fade_secs: 0.0,
                lp_start_hz: 15_000.0,
                lp_end_hz: 1_000.0,
                ir: None,
            },
        )))
    }

    fn boxed_graph() -> *mut rustel_kabel::Graph {
        let program = rustel_kabel::Program::from_json(
            r#"{"nodes":[{"type":"n","value":0.25},{"type":"output","inputs":[0]}]}"#,
        )
        .expect("graph program");
        Box::into_raw(Box::new(
            rustel_kabel::Graph::new(&program, 8_000.0).expect("graph"),
        ))
    }

    #[test]
    fn graph_baton_crosses_threads_and_returns_unique_ownership() {
        let channel = std::sync::Arc::new(SampleChannel::new());
        let pointer = boxed_graph();
        channel
            .kabel_installs
            .push(KabelInstall { graph: pointer })
            .ok()
            .expect("install");
        let consumer_channel = std::sync::Arc::clone(&channel);
        std::thread::spawn(move || {
            let install = consumer_channel
                .kabel_installs
                .pop()
                .expect("published graph");
            // SAFETY: this consumer uniquely owns the popped install baton.
            let mut graph = unsafe { Box::from_raw(install.graph) };
            assert_eq!(graph.process([0.0; 2], 1.0, 0.0), [0.25, 0.0]);
            let returned = ReturnedKabel(Box::into_raw(graph));
            consumer_channel
                .kabel_returns
                .push(returned)
                .ok()
                .expect("return");
        })
        .join()
        .expect("consumer");
        let mut returned = Vec::new();
        channel.reclaim_with_processors(|_| {}, |graph| returned.push(graph));
        assert_eq!(returned, [pointer]);
        channel.reclaim_with_processors(|_| {}, |_| panic!("graph returned twice"));
    }

    fn boxed_program() -> *mut PreparedKabelProgram {
        Box::into_raw(Box::new(PreparedKabelProgram {
            program: rustel_kabel::Program::from_json(
                r#"{"nodes":[{"type":"n","value":0.25},{"type":"output","inputs":[0]}]}"#,
            )
            .expect("program"),
            until_frame: 128,
        }))
    }

    #[test]
    fn program_baton_crosses_threads_and_returns_unique_ownership() {
        let channel = std::sync::Arc::new(SampleChannel::new());
        let pointer = boxed_program();
        channel
            .program_installs
            .push(ProgramInstall { program: pointer })
            .ok()
            .expect("install");
        let consumer = std::sync::Arc::clone(&channel);
        std::thread::spawn(move || {
            let install = consumer.program_installs.pop().expect("program install");
            // SAFETY: popping transferred the unique baton to this consumer.
            let program = unsafe { Box::from_raw(install.program) };
            assert_eq!(program.until_frame, 128);
            consumer
                .program_returns
                .push(ReturnedProgram(Box::into_raw(program)))
                .ok()
                .expect("return");
        })
        .join()
        .expect("consumer");
        let mut returned = Vec::new();
        channel.reclaim_with_programs(|_| {}, |_| {}, |program| returned.push(program));
        assert_eq!(returned, [pointer]);
        channel.reclaim_with_programs(|_| {}, |_| {}, |_| panic!("program returned twice"));
    }

    #[test]
    fn retirement_excludes_publication_and_orders_returned_storage() {
        use std::sync::{Arc, atomic::AtomicBool};
        let channel = Arc::new(SampleChannel::new());
        let retirement = channel
            .try_kabel_retirement()
            .expect("idle publication counter");
        assert!(channel.try_kabel_retirement().is_none());
        let started = Arc::new(AtomicBool::new(false));
        let acquired = Arc::new(AtomicBool::new(false));
        let producer_channel = Arc::clone(&channel);
        let producer_started = Arc::clone(&started);
        let producer_acquired = Arc::clone(&acquired);
        let producer = std::thread::spawn(move || {
            producer_started.store(true, Ordering::Release);
            let _publication = KabelPublication::new(Arc::clone(&producer_channel));
            producer_acquired.store(true, Ordering::Release);
            let returned = producer_channel
                .kabel_returns
                .pop()
                .expect("retired before admission");
            // SAFETY: the callback transferred unique ownership through the ring.
            drop(unsafe { Box::from_raw(returned.0) });
            assert!(producer_channel.try_kabel_retirement().is_none());
        });
        while !started.load(Ordering::Acquire) {
            std::thread::yield_now();
        }
        assert!(!acquired.load(Ordering::Acquire));
        channel
            .kabel_returns
            .push(ReturnedKabel(boxed_graph()))
            .ok()
            .expect("return");
        drop(retirement);
        producer.join().expect("producer");
        assert!(acquired.load(Ordering::Acquire));
        assert!(channel.try_kabel_retirement().is_some());
    }

    #[test]
    fn retirement_unwind_releases_producer_admission() {
        let channel = std::sync::Arc::new(SampleChannel::new());
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _retirement = channel.try_kabel_retirement().unwrap();
            panic!("retirement interrupted");
        }));
        assert!(result.is_err());
        let first = KabelPublication::new(std::sync::Arc::clone(&channel));
        let second = KabelPublication::new(std::sync::Arc::clone(&channel));
        assert!(channel.try_kabel_retirement().is_none());
        drop(first);
        assert!(channel.try_kabel_retirement().is_none());
        drop(second);
        assert!(channel.try_kabel_retirement().is_some());
    }

    #[test]
    fn channel_drop_frees_each_queued_baton() {
        let channel = SampleChannel::new();
        for sample in [boxed_sample(), std::ptr::null_mut()] {
            let id = SampleId(7);
            let install = SampleInstall { id, sample };
            channel.installs.push(install).ok().expect("push");
        }
        let orbit = ReverbInstall {
            orbit: 1,
            reverb: boxed_room(),
        };
        channel.reverb_installs.push(orbit).ok().expect("push");
        let stage = FxReverbInstall {
            reverb: boxed_room(),
        };
        channel.fx_reverb_installs.push(stage).ok().expect("push");

        // An insert goes through its 2 rings as a reverb does.
        struct Quiet;
        impl crate::insert::OrbitInsert for Quiet {
            fn key(&self) -> crate::insert::InsertKey {
                crate::insert::InsertKey::default()
            }
            fn set_param(&mut self, _param: crate::insert::InsertParam, _frames: u32) {}
            fn process(&mut self, _left: &mut [f32], _right: &mut [f32]) {}
        }
        let boxed_insert = || Box::into_raw(Box::new(Quiet) as Box<dyn crate::insert::OrbitInsert>);
        let insert = InsertInstall {
            slot: crate::insert::effect_slot(2, 0),
            sample_rate: 48_000,
            insert: boxed_insert(),
        };
        channel.insert_installs.push(insert).ok().expect("push");

        let sample = ReturnedSample(boxed_sample());
        channel.returns.push(sample).ok().expect("push");
        let orbit = ReturnedReverb(boxed_room());
        channel.reverb_returns.push(orbit).ok().expect("push");
        let stage = ReturnedFxReverb(boxed_room());
        channel.fx_reverb_returns.push(stage).ok().expect("push");
        let insert = ReturnedInsert(boxed_insert());
        channel.insert_returns.push(insert).ok().expect("push");
        // The plugin workers of the output wait in their one place.
        let pool = std::sync::Arc::new(crate::pool::PoolShared::new(1, true).expect("a pool"));
        let install = PoolInstall(std::sync::Arc::into_raw(std::sync::Arc::clone(&pool)));
        channel.pool_installs.push(install).ok().expect("push");
        // Send lines go through their 2 rings as a reverb does.
        let lines = || Box::into_raw(Box::new(crate::scalar::SendLines::new(0, 8_000)));
        let install = SendLineInstall(lines());
        channel.send_line_installs.push(install).ok().expect("push");
        let returned = ReturnedSendLines(lines());
        channel.send_line_returns.push(returned).ok().expect("push");
        channel
            .kabel_installs
            .push(KabelInstall {
                graph: boxed_graph(),
            })
            .ok()
            .expect("push");
        channel
            .kabel_returns
            .push(ReturnedKabel(boxed_graph()))
            .ok()
            .expect("push");
        channel
            .program_installs
            .push(ProgramInstall {
                program: boxed_program(),
            })
            .ok()
            .expect("push");
        channel
            .program_returns
            .push(ReturnedProgram(boxed_program()))
            .ok()
            .expect("push");
        drop(channel); // must not leak (miri/asan would flag)
        assert_eq!(std::sync::Arc::strong_count(&pool), 1);
    }
}
