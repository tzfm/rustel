//! A dropped `xen` construction with a bad scale returns its error message,
//! so editing through many bad scale names does not retain one allocation
//! per attempt.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};

struct Counting;

static LIVE_BYTES: AtomicUsize = AtomicUsize::new(0);

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let pointer = unsafe { System.alloc(layout) };
        if !pointer.is_null() {
            LIVE_BYTES.fetch_add(layout.size(), Ordering::Relaxed);
        }
        pointer
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        LIVE_BYTES.fetch_sub(layout.size(), Ordering::Relaxed);
        unsafe { System.dealloc(pointer, layout) }
    }

    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        let moved = unsafe { System.realloc(pointer, layout, new_size) };
        if !moved.is_null() {
            LIVE_BYTES.fetch_add(new_size.saturating_sub(layout.size()), Ordering::Relaxed);
        }
        moved
    }
}

#[global_allocator]
static COUNTING: Counting = Counting;

#[test]
fn a_dropped_xen_error_returns_its_memory() {
    // Warm every path the construction touches once, then measure a run of
    // constructions that are all dropped.
    for _ in 0..16 {
        drop(rustel_core::xen::xen(
            &rustel_core::pure(rustel_core::Value::Str("c".into())),
            rustel_core::Value::Str("no-such-scale".into()),
        ));
    }
    let baseline = LIVE_BYTES.load(Ordering::Relaxed);
    for _ in 0..1_000 {
        drop(rustel_core::xen::xen(
            &rustel_core::pure(rustel_core::Value::Str("c".into())),
            rustel_core::Value::Str("no-such-scale".into()),
        ));
    }
    let grown = LIVE_BYTES.load(Ordering::Relaxed).saturating_sub(baseline);
    assert!(
        grown < 8_192,
        "dropping every pattern still retained {grown} bytes; each bad scale name leaks its message"
    );
}
