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
        let mut slots = Vec::with_capacity(ASSET_RING_CAPACITY);
        slots.resize_with(ASSET_RING_CAPACITY, || std::cell::UnsafeCell::new(None));
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

// SAFETY: same SPSC baton pass as the sample pointers below.
unsafe impl Send for ReverbInstall {}
unsafe impl Send for ReturnedReverb {}
unsafe impl Send for FxReverbInstall {}
unsafe impl Send for ReturnedFxReverb {}

/// Both directions plus the leak counter, shared by the device (producer
/// side) and the callback (consumer side).
pub(crate) struct SampleChannel {
    pub installs: AssetRing<SampleInstall>,
    pub returns: AssetRing<ReturnedSample>,
    pub reverb_installs: AssetRing<ReverbInstall>,
    pub reverb_returns: AssetRing<ReturnedReverb>,
    pub fx_reverb_installs: AssetRing<FxReverbInstall>,
    pub fx_reverb_returns: AssetRing<ReturnedFxReverb>,
    /// Stage reverbs refused by the install queue or callback byte budget.
    pub fx_reverb_refusals: AtomicU64,
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
    #[cfg(any(feature = "device-audio", test))]
    pub fn new() -> Self {
        Self {
            installs: AssetRing::new(),
            returns: AssetRing::new(),
            reverb_installs: AssetRing::new(),
            reverb_returns: AssetRing::new(),
            fx_reverb_installs: AssetRing::new(),
            fx_reverb_returns: AssetRing::new(),
            fx_reverb_refusals: AtomicU64::new(0),
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
    pub fn reclaim_with_fx(&self, mut returned_fx: impl FnMut(*mut crate::reverb::OrbitReverb)) {
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
        while let Some(returned) = self.fx_reverb_returns.pop() {
            returned_fx(returned.0);
            // SAFETY: same pass for stage reverbs retired by the callback.
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
        while let Some(install) = self.fx_reverb_installs.pop() {
            // SAFETY: never reached the consumer; unique ownership here.
            drop(unsafe { Box::from_raw(install.reverb) });
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
        const ITEMS: u32 = 100_000;
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

    #[test]
    fn channel_drop_frees_undelivered_installs() {
        let channel = SampleChannel::new();
        let sample = Box::into_raw(Box::new(
            crate::sample::decode_wav(include_bytes!("../assets/bd.wav")).expect("bundled bd"),
        ));
        channel
            .installs
            .push(SampleInstall {
                id: SampleId(7),
                sample,
            })
            .ok()
            .expect("push");
        drop(channel); // must not leak (miri/asan would flag)
    }
}
