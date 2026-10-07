//! Runtime tripwires for the audio-callback contract.
//!
//! Integration tests may install [`TripwireAlloc`] as the global allocator.
//! Library code uses [`audio_scope`] plus the counters below so a simulated
//! callback can prove it performed no allocation or free.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};

thread_local! {
    static IN_AUDIO: Cell<bool> = const { Cell::new(false) };
}

pub static AUDIO_ALLOCS: AtomicU64 = AtomicU64::new(0);
pub static AUDIO_FREES: AtomicU64 = AtomicU64::new(0);
pub static AUDIO_SCOPE_ENTRIES: AtomicU64 = AtomicU64::new(0);

pub struct TripwireAlloc;

pub fn in_audio_scope() -> bool {
    IN_AUDIO.try_with(|f| f.get()).unwrap_or(false)
}

unsafe impl GlobalAlloc for TripwireAlloc {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        if in_audio_scope() {
            AUDIO_ALLOCS.fetch_add(1, Relaxed);
        }
        unsafe { System.alloc(l) }
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        if in_audio_scope() {
            AUDIO_FREES.fetch_add(1, Relaxed);
        }
        unsafe { System.dealloc(p, l) }
    }
    unsafe fn realloc(&self, p: *mut u8, l: Layout, n: usize) -> *mut u8 {
        if in_audio_scope() {
            AUDIO_ALLOCS.fetch_add(1, Relaxed);
        }
        unsafe { System.realloc(p, l, n) }
    }
    unsafe fn alloc_zeroed(&self, l: Layout) -> *mut u8 {
        if in_audio_scope() {
            AUDIO_ALLOCS.fetch_add(1, Relaxed);
        }
        unsafe { System.alloc_zeroed(l) }
    }
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
