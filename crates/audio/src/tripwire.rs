//! Runtime tripwires for the audio-callback contract.
//!
//! Integration tests may install [`TripwireAlloc`] as the global allocator.
//! Library code uses [`audio_scope`] plus the counters below so a simulated
//! callback can prove it performed no allocation or free.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::sync::atomic::{AtomicBool, AtomicIsize, AtomicU64, AtomicUsize, Ordering::Relaxed};

thread_local! {
    static IN_AUDIO: Cell<bool> = const { Cell::new(false) };
}

pub static AUDIO_ALLOCS: AtomicU64 = AtomicU64::new(0);
pub static AUDIO_FREES: AtomicU64 = AtomicU64::new(0);
pub static AUDIO_SCOPE_ENTRIES: AtomicU64 = AtomicU64::new(0);

static MEASURE_ACTIVE: AtomicBool = AtomicBool::new(false);
static MEASURE_DEPTH: AtomicUsize = AtomicUsize::new(0);
static MEASURE_ALLOCS: AtomicU64 = AtomicU64::new(0);
static MEASURE_ALLOCATED: AtomicU64 = AtomicU64::new(0);
static MEASURE_LIVE: AtomicIsize = AtomicIsize::new(0);
static MEASURE_PEAK: AtomicIsize = AtomicIsize::new(0);

#[inline]
fn record_alloc(size: usize) {
    MEASURE_ALLOCS.fetch_add(1, Relaxed);
    MEASURE_ALLOCATED.fetch_add(size as u64, Relaxed);
    let live = MEASURE_LIVE.fetch_add(size as isize, Relaxed) + size as isize;
    MEASURE_PEAK.fetch_max(live, Relaxed);
}

#[inline]
fn record_realloc(old_size: usize, new_size: usize) {
    MEASURE_ALLOCS.fetch_add(1, Relaxed);
    MEASURE_ALLOCATED.fetch_add(new_size as u64, Relaxed);
    let delta = new_size as isize - old_size as isize;
    let live = MEASURE_LIVE.fetch_add(delta, Relaxed) + delta;
    MEASURE_PEAK.fetch_max(live, Relaxed);
}

pub struct TripwireAlloc;

pub fn in_audio_scope() -> bool {
    IN_AUDIO.try_with(|f| f.get()).unwrap_or(false)
}

unsafe impl GlobalAlloc for TripwireAlloc {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        if in_audio_scope() {
            AUDIO_ALLOCS.fetch_add(1, Relaxed);
        }
        let ptr = unsafe { System.alloc(l) };
        if MEASURE_ACTIVE.load(Relaxed) && !ptr.is_null() {
            record_alloc(l.size());
        }
        ptr
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        if in_audio_scope() {
            AUDIO_FREES.fetch_add(1, Relaxed);
        }
        unsafe { System.dealloc(p, l) };
        if MEASURE_ACTIVE.load(Relaxed) {
            MEASURE_LIVE.fetch_sub(l.size() as isize, Relaxed);
        }
    }
    unsafe fn realloc(&self, p: *mut u8, l: Layout, n: usize) -> *mut u8 {
        if in_audio_scope() {
            AUDIO_ALLOCS.fetch_add(1, Relaxed);
        }
        let ptr = unsafe { System.realloc(p, l, n) };
        if MEASURE_ACTIVE.load(Relaxed) && !ptr.is_null() {
            record_realloc(l.size(), n);
        }
        ptr
    }
    unsafe fn alloc_zeroed(&self, l: Layout) -> *mut u8 {
        if in_audio_scope() {
            AUDIO_ALLOCS.fetch_add(1, Relaxed);
        }
        let ptr = unsafe { System.alloc_zeroed(l) };
        if MEASURE_ACTIVE.load(Relaxed) && !ptr.is_null() {
            record_alloc(l.size());
        }
        ptr
    }
}

/// Process-wide allocation and heap-memory deltas recorded by
/// [`measure_allocations`] when [`TripwireAlloc`] is installed as the global
/// allocator.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AllocMetrics {
    pub allocations: u64,
    pub allocated_bytes: u64,
    /// Net bytes still held when the measured block ended, less bytes freed.
    /// Negative when the block freed more heap memory than it kept.
    pub held_bytes: i64,
    /// Peak bytes held above the starting live level during the block.
    pub peak_bytes: u64,
}

/// Run `work` while counting allocations and live/peak bytes through
/// [`TripwireAlloc`]. Outside this scope the global allocator performs only a
/// single relaxed flag check.
pub fn measure_allocations<T>(work: impl FnOnce() -> T) -> (T, AllocMetrics) {
    struct Guard;
    impl Drop for Guard {
        fn drop(&mut self) {
            if MEASURE_DEPTH.fetch_sub(1, Relaxed) == 1 {
                MEASURE_ACTIVE.store(false, Relaxed);
            }
        }
    }

    if MEASURE_DEPTH.fetch_add(1, Relaxed) == 0 {
        MEASURE_ACTIVE.store(true, Relaxed);
    }
    let _guard = Guard;

    let live_before = MEASURE_LIVE.load(Relaxed);
    MEASURE_PEAK.store(live_before, Relaxed);
    let allocs_before = MEASURE_ALLOCS.load(Relaxed);
    let bytes_before = MEASURE_ALLOCATED.load(Relaxed);

    let out = work();

    let live_after = MEASURE_LIVE.load(Relaxed);
    let peak = MEASURE_PEAK.load(Relaxed);
    let metrics = AllocMetrics {
        allocations: MEASURE_ALLOCS.load(Relaxed).wrapping_sub(allocs_before),
        allocated_bytes: MEASURE_ALLOCATED.load(Relaxed).wrapping_sub(bytes_before),
        held_bytes: (live_after - live_before) as i64,
        peak_bytes: (peak - live_before).max(0) as u64,
    };
    (out, metrics)
}

pub fn audio_scope<R>(f: impl FnOnce() -> R) -> R {
    struct Restore(bool);

    impl Drop for Restore {
        fn drop(&mut self) {
            IN_AUDIO.with(|flag| flag.set(self.0));
        }
    }

    AUDIO_SCOPE_ENTRIES.fetch_add(1, Relaxed);
    let previous = IN_AUDIO.with(|flag| flag.replace(true));
    let restore = Restore(previous);
    let result = f();
    drop(restore);
    result
}

pub fn scope_entries() -> u64 {
    AUDIO_SCOPE_ENTRIES.load(Relaxed)
}

/// Prove that the executable installed [`TripwireAlloc`] before trusting a
/// zero callback-allocation report.
///
/// This runs before a live stream is opened. The allocation and its release
/// are deliberate positive controls; a binary using the system allocator
/// leaves both counters unchanged and therefore fails closed.
pub fn allocator_is_armed() -> bool {
    let before = Violations::capture();
    audio_scope(|| {
        let mut allocation = Vec::with_capacity(8);
        allocation.push(0_u8);
        std::hint::black_box(allocation);
    });
    let delta = Violations::capture().since(before);
    delta.allocs > 0 && delta.frees > 0
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Violations {
    pub allocs: u64,
    pub frees: u64,
}

impl Violations {
    pub fn capture() -> Self {
        Self {
            allocs: AUDIO_ALLOCS.load(Relaxed),
            frees: AUDIO_FREES.load(Relaxed),
        }
    }

    pub fn since(self, base: Self) -> Self {
        Self {
            allocs: self.allocs.wrapping_sub(base.allocs),
            frees: self.frees.wrapping_sub(base.frees),
        }
    }

    pub fn clean(self) -> bool {
        self == Self::default()
    }
}
