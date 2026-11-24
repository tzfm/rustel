//! What one piece of work costs: wall time, process CPU time, and the
//! memory the allocator hands out and keeps.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

static LIVE: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);
static ALLOCATIONS: AtomicU64 = AtomicU64::new(0);
static ALLOCATED: AtomicU64 = AtomicU64::new(0);

/// The system allocator, counting what it hands out and gets back.
pub struct Counting;

// SAFETY: every method forwards to `System` with the caller's arguments and
// only adds counters, so `System`'s guarantees hold unchanged.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // SAFETY: the caller upholds `GlobalAlloc::alloc`'s contract.
        let pointer = unsafe { System.alloc(layout) };
        if !pointer.is_null() {
            handed_out(layout.size());
        }
        pointer
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        // SAFETY: the caller upholds `GlobalAlloc::alloc_zeroed`'s contract.
        let pointer = unsafe { System.alloc_zeroed(layout) };
        if !pointer.is_null() {
            handed_out(layout.size());
        }
        pointer
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        // SAFETY: the caller upholds `GlobalAlloc::dealloc`'s contract.
        unsafe { System.dealloc(pointer, layout) };
        LIVE.fetch_sub(layout.size(), Ordering::Relaxed);
    }

    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        // SAFETY: the caller upholds `GlobalAlloc::realloc`'s contract.
        let moved = unsafe { System.realloc(pointer, layout, new_size) };
        if !moved.is_null() {
            LIVE.fetch_sub(layout.size(), Ordering::Relaxed);
            handed_out(new_size);
        }
        moved
    }
}

fn handed_out(size: usize) {
    ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
    ALLOCATED.fetch_add(size as u64, Ordering::Relaxed);
    let live = LIVE.fetch_add(size, Ordering::Relaxed) + size;
    PEAK.fetch_max(live, Ordering::Relaxed);
}

/// The cost of one piece of work.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Cost {
    pub wall_ns: u64,
    pub cpu_ns: u64,
    /// Allocations made.
    pub allocations: u64,
    /// Bytes those allocations asked for.
    pub allocated: u64,
    /// Bytes still held when the work ended, less those freed: negative
    /// when the work freed more than it kept.
    pub held: i64,
    /// The most bytes held at once during the work, above where it began.
    pub peak: u64,
}

impl Cost {
    /// This cost shared out over `count` equal pieces of work.
    pub fn per(self, count: usize) -> Self {
        let count = count.max(1) as u64;
        Self {
            wall_ns: self.wall_ns / count,
            cpu_ns: self.cpu_ns / count,
            allocations: self.allocations / count,
            allocated: self.allocated / count,
            held: self.held,
            peak: self.peak,
        }
    }

    /// The median of each field over `costs`, field by field.
    pub fn median(costs: &[Self]) -> Self {
        fn middle<T: Copy + Ord>(mut values: Vec<T>) -> T {
            values.sort_unstable();
            values[values.len() / 2]
        }
        Self {
            wall_ns: middle(costs.iter().map(|cost| cost.wall_ns).collect()),
            cpu_ns: middle(costs.iter().map(|cost| cost.cpu_ns).collect()),
            allocations: middle(costs.iter().map(|cost| cost.allocations).collect()),
            allocated: middle(costs.iter().map(|cost| cost.allocated).collect()),
            held: middle(costs.iter().map(|cost| cost.held).collect()),
            peak: middle(costs.iter().map(|cost| cost.peak).collect()),
        }
    }
}

/// Run `work` once and measure it.
pub fn measure<T>(work: impl FnOnce() -> T) -> (T, Cost) {
    let live = LIVE.load(Ordering::Relaxed);
    PEAK.store(live, Ordering::Relaxed);
    let allocations = ALLOCATIONS.load(Ordering::Relaxed);
    let allocated = ALLOCATED.load(Ordering::Relaxed);
    let cpu = cpu_time();
    let started = Instant::now();
    let out = work();
    let wall = started.elapsed();
    let cpu = cpu_time().saturating_sub(cpu);
    let now = LIVE.load(Ordering::Relaxed);
    let cost = Cost {
        wall_ns: nanos(wall),
        cpu_ns: nanos(cpu),
        allocations: ALLOCATIONS.load(Ordering::Relaxed) - allocations,
        allocated: ALLOCATED.load(Ordering::Relaxed) - allocated,
        held: now as i64 - live as i64,
        peak: (PEAK.load(Ordering::Relaxed).saturating_sub(live)) as u64,
    };
    (out, cost)
}

fn nanos(time: Duration) -> u64 {
    u64::try_from(time.as_nanos()).unwrap_or(u64::MAX)
}

/// The user and system CPU time of every thread of the process. A seam
/// that hands work to another thread, as a parse does, is charged for it.
#[cfg(unix)]
fn cpu_time() -> Duration {
    let mut usage = std::mem::MaybeUninit::<libc::rusage>::zeroed();
    // SAFETY: `getrusage` fills the `rusage` it is given for this process,
    // and the zeroed value is a valid one should it not.
    let usage = unsafe {
        libc::getrusage(libc::RUSAGE_SELF, usage.as_mut_ptr());
        usage.assume_init()
    };
    let time = |value: libc::timeval| {
        Duration::new(
            u64::try_from(value.tv_sec).unwrap_or(0),
            u32::try_from(value.tv_usec)
                .unwrap_or(0)
                .saturating_mul(1000),
        )
    };
    time(usage.ru_utime) + time(usage.ru_stime)
}

#[cfg(not(unix))]
fn cpu_time() -> Duration {
    Duration::ZERO
}
