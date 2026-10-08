/*
rustel-jsruntime - a budget-aware QuickJS allocator
Allocation layout and pointer handling adapted from rquickjs 0.9.0,
core/src/allocator/rust.rs:
Copyright (c) 2020 Mees Delzenne
See crates/jsruntime/LICENSE-rquickjs for the original MIT terms.

Heap budgeting, refusal tracking and other Rustel additions:
Copyright (C) 2026 Rustel contributors

This program is free software: you can redistribute it and/or modify it under
the terms of the GNU Affero General Public License as published by the Free
Software Foundation, either version 3 of the License, or (at your option) any
later version.
*/

//! A QuickJS allocator that enforces a heap budget and records WHY it refused.
//!
//! # Why not `Runtime::set_memory_limit`
//!
//! QuickJS enforces its own limit *before* calling the allocator, so an
//! over-limit allocation never reaches this code and there is nothing to
//! observe. All the host sees afterwards is a JavaScript exception - and the
//! text is not dependable: `JS_ThrowOutOfMemory` normally builds
//! `InternalError: out of memory`, but if constructing that error also exhausts
//! the heap it throws null instead. Matching on the message is not safe: the
//! substring it needs also matches an ordinary user exception that mentions
//! "null pointer".
//!
//! So the budget lives here, at the allocation site, and denial sets a
//! STRUCTURAL flag that no message can imitate.
//!
//! # The flag's lifetime
//!
//! Set only when THIS allocator refuses - never when the system allocator
//! fails, which is a different fact about a different resource. It is sticky
//! across nested callbacks (a callback that queries another pattern must not
//! clear its caller's refusal) and cleared once at an outer execution boundary:
//! either a direct query or one scheduler tick. At that boundary,
//! [`take_heap_exhausted`] both reads and resets it.

use std::alloc::{self, Layout};
use std::cell::Cell;
use std::mem;

use rquickjs::allocator::Allocator;

/// QuickJS allocates `u64`s, so every block is aligned for one.
const ALLOC_ALIGN: usize = mem::align_of::<u64>();

const fn max(a: usize, b: usize) -> usize {
    if a < b { b } else { a }
}

/// A header carrying each block's size, so `dealloc`/`realloc` can un-account
/// exactly what was charged rather than trusting a caller-supplied length.
const HEADER_SIZE: usize = max(mem::size_of::<Header>(), ALLOC_ALIGN);

#[derive(Copy, Clone)]
#[repr(transparent)]
struct Header {
    size: usize,
}

#[inline]
fn round_size(size: usize) -> Option<usize> {
    // `div_ceil(...)*ALIGN` can overflow at the multiplication even though the
    // caller later checks the header addition. This function is reached from a
    // C allocator callback, so a panic would cross FFI rather than become a
    // handled QuickJS allocation failure.
    size.checked_add(ALLOC_ALIGN - 1)
        .map(|rounded| rounded & !(ALLOC_ALIGN - 1))
}

thread_local! {
    /// Set when the BUDGET denied an allocation.
    ///
    /// Thread-local because a QuickJS runtime belongs to exactly one thread -
    /// the same reason the callback host does.
    static HEAP_EXHAUSTED: Cell<bool> = const { Cell::new(false) };
}

/// Whether this thread's budget has denied an allocation, clearing the flag.
///
/// Called once at the outer direct-query or scheduler-tick boundary. Nested
/// callbacks deliberately do not clear it: a refusal inside a callback is a
/// refusal of the whole operation, and clearing on the way out would lose it
/// exactly when it matters.
pub(super) fn take_heap_exhausted() -> bool {
    HEAP_EXHAUSTED.with(|flag| flag.replace(false))
}

/// Whether a refusal is pending, without clearing it.
pub(super) fn heap_exhausted() -> bool {
    HEAP_EXHAUSTED.with(Cell::get)
}

fn note_exhausted() {
    HEAP_EXHAUSTED.with(|flag| flag.set(true));
}

#[cfg(test)]
thread_local! {
    /// Test hook: make the UNDERLYING system allocator fail.
    ///
    /// A system failure and a budget denial both return null, and the whole
    /// point of the flag is that only one of them is a heap-limit refusal.
    /// Distinguishing them cannot be tested by exhausting real memory - that
    /// is the memory bomb this suite exists to avoid - so the failure is
    /// injected instead.
    static FAIL_SYSTEM_ALLOC: Cell<bool> = const { Cell::new(false) };
}

/// Force the underlying allocator to fail for the duration of `f`.
///
/// Compiled only for tests, so the product cannot reach it and no `allow` is
/// needed to say so.
#[cfg(test)]
fn with_failing_system_allocator<R>(f: impl FnOnce() -> R) -> R {
    struct Guard(bool);
    impl Drop for Guard {
        fn drop(&mut self) {
            FAIL_SYSTEM_ALLOC.with(|c| c.set(self.0));
        }
    }
    let previous = FAIL_SYSTEM_ALLOC.with(|c| c.replace(true));
    let _guard = Guard(previous);
    f()
}

#[cfg(test)]
fn system_alloc_should_fail() -> bool {
    FAIL_SYSTEM_ALLOC.with(Cell::get)
}

/// Always false outside tests: the hook does not exist there.
#[cfg(not(test))]
fn system_alloc_should_fail() -> bool {
    false
}

/// Live bytes and the ceiling, shared with the runtime that owns them.
#[derive(Debug)]
pub(super) struct HeapBudget {
    live: Cell<usize>,
    limit: Cell<usize>,
}

impl HeapBudget {
    pub(super) fn new(limit: usize) -> Self {
        Self {
            live: Cell::new(0),
            limit: Cell::new(limit),
        }
    }

    pub(super) fn live(&self) -> usize {
        self.live.get()
    }

    pub(super) fn limit(&self) -> usize {
        self.limit.get()
    }

    /// Lower the ceiling. Refuses to go below what is already live.
    ///
    /// Monotonic lowering alone can poison a runtime: drop the ceiling under
    /// current usage and every subsequent allocation is denied, with no way
    /// back because raising is forbidden. A limit that cannot be honoured is
    /// not a tighter bound, it is a broken runtime.
    pub(super) fn lower_to(&self, bytes: usize) -> Result<(), String> {
        if bytes == 0 {
            return Err("a heap ceiling of zero means unlimited".to_string());
        }
        let current = self.limit.get();
        if bytes > current {
            return Err(format!(
                "the QuickJS heap ceiling is monotonic: it is {current} bytes \
                 and cannot be raised to {bytes}"
            ));
        }
        let live = self.live.get();
        if bytes < live {
            return Err(format!(
                "refusing to lower the heap ceiling to {bytes} bytes while \
                 {live} are live: every later allocation would be denied and \
                 the ceiling cannot be raised again"
            ));
        }
        self.limit.set(bytes);
        Ok(())
    }

    /// Reserve `bytes`, or refuse and record why.
    fn reserve(&self, bytes: usize) -> bool {
        let Some(next) = self.live.get().checked_add(bytes) else {
            note_exhausted();
            return false;
        };
        if next > self.limit.get() {
            note_exhausted();
            return false;
        }
        self.live.set(next);
        true
    }

    /// Give `bytes` back. Saturating: an underflow would be a bug here, and
    /// wrapping to a huge live count would deny everything afterwards.
    fn release(&self, bytes: usize) {
        self.live.set(self.live.get().saturating_sub(bytes));
    }
}

/// The allocator handed to `Runtime::new_with_alloc`.
///
/// Holds an `Rc` to the same budget the runtime reports on, so lowering the
/// ceiling and reading live usage act on one shared state.
pub(super) struct BudgetedAllocator {
    budget: std::rc::Rc<HeapBudget>,
}

impl BudgetedAllocator {
    pub(super) fn new(budget: std::rc::Rc<HeapBudget>) -> Self {
        Self { budget }
    }
}

// SAFETY: every pointer returned is either null or a block of at least the
// requested size, aligned to `ALLOC_ALIGN`, carrying a header the deallocation
// path reads back. `usable_size` reports the rounded size actually available.
unsafe impl Allocator for BudgetedAllocator {
    fn alloc(&mut self, size: usize) -> *mut u8 {
        let Some(size) = round_size(size) else {
            note_exhausted();
            return std::ptr::null_mut();
        };
        let Some(total) = size.checked_add(HEADER_SIZE) else {
            note_exhausted();
            return std::ptr::null_mut();
        };
        if !self.budget.reserve(total) {
            return std::ptr::null_mut();
        }
        let Ok(layout) = Layout::from_size_align(total, ALLOC_ALIGN) else {
            self.budget.release(total);
            return std::ptr::null_mut();
        };
        // SAFETY: `layout` has a non-zero size (HEADER_SIZE is non-zero).
        let ptr = if system_alloc_should_fail() {
            std::ptr::null_mut()
        } else {
            unsafe { alloc::alloc(layout) }
        };
        if ptr.is_null() {
            // The SYSTEM refused, not the budget. Accounting is rolled back and
            // the flag is deliberately NOT set: reporting this as a heap-limit
            // refusal would blame a bound that had room to spare.
            self.budget.release(total);
            return std::ptr::null_mut();
        }
        // SAFETY: `ptr` is a fresh block of at least `HEADER_SIZE + size`.
        unsafe {
            ptr.cast::<Header>().write(Header { size });
            ptr.add(HEADER_SIZE)
        }
    }

    fn calloc(&mut self, count: usize, size: usize) -> *mut u8 {
        if count == 0 || size == 0 {
            return std::ptr::null_mut();
        }
        // Checked, not `expect`: the product is attacker-controlled through
        // `new Array(n)`, and panicking across the QuickJS FFI boundary is
        // undefined behaviour rather than a diagnostic.
        let Some(total_size) = count.checked_mul(size) else {
            note_exhausted();
            return std::ptr::null_mut();
        };
        let Some(total_size) = round_size(total_size) else {
            note_exhausted();
            return std::ptr::null_mut();
        };
        let Some(total) = total_size.checked_add(HEADER_SIZE) else {
            note_exhausted();
            return std::ptr::null_mut();
        };
        if !self.budget.reserve(total) {
            return std::ptr::null_mut();
        }
        let Ok(layout) = Layout::from_size_align(total, ALLOC_ALIGN) else {
            self.budget.release(total);
            return std::ptr::null_mut();
        };
        // SAFETY: non-zero layout, as above.
        let ptr = if system_alloc_should_fail() {
            std::ptr::null_mut()
        } else {
            unsafe { alloc::alloc_zeroed(layout) }
        };
        if ptr.is_null() {
            self.budget.release(total);
            return std::ptr::null_mut();
        }
        // SAFETY: fresh block of at least `HEADER_SIZE + total_size`.
        unsafe {
            ptr.cast::<Header>().write(Header { size: total_size });
            ptr.add(HEADER_SIZE)
        }
    }

    /// # Safety
    /// `ptr` must have come from this allocator.
    unsafe fn dealloc(&mut self, ptr: *mut u8) {
        // QuickJS never passes a null `ptr` here. A null `ptr` has no header,
        // so there is nothing to release.
        if ptr.is_null() {
            return;
        }
        // SAFETY: the caller guarantees `ptr` is ours, so the header is intact.
        unsafe {
            let base = ptr.sub(HEADER_SIZE);
            let size = base.cast::<Header>().read().size;
            let total = size + HEADER_SIZE;
            self.budget.release(total);
            alloc::dealloc(base, Layout::from_size_align_unchecked(total, ALLOC_ALIGN));
        }
    }

    /// # Safety
    /// `ptr` must have come from this allocator.
    unsafe fn realloc(&mut self, ptr: *mut u8, new_size: usize) -> *mut u8 {
        // QuickJS never passes a null `ptr` here. A null `ptr` has no header,
        // so the call fails with no charge and no refusal flag.
        if ptr.is_null() {
            return std::ptr::null_mut();
        }
        let Some(new_size) = round_size(new_size) else {
            note_exhausted();
            return std::ptr::null_mut();
        };
        // SAFETY: the caller guarantees `ptr` is ours.
        unsafe {
            let base = ptr.sub(HEADER_SIZE);
            let old_size = base.cast::<Header>().read().size;
            let old_total = old_size + HEADER_SIZE;
            let Some(new_total) = new_size.checked_add(HEADER_SIZE) else {
                note_exhausted();
                return std::ptr::null_mut();
            };

            // Only the GROWTH is charged, and only after it is known to fit.
            // A failed realloc must leave the old allocation and its accounting
            // exactly as they were - charging first and refunding on failure
            // would deny an allocation that fits whenever the delta briefly
            // pushed past the ceiling.
            if new_total > old_total {
                let growth = new_total - old_total;
                if !self.budget.reserve(growth) {
                    return std::ptr::null_mut();
                }
            }

            let layout = Layout::from_size_align_unchecked(old_total, ALLOC_ALIGN);
            let out = if system_alloc_should_fail() {
                std::ptr::null_mut()
            } else {
                alloc::realloc(base, layout, new_total)
            };
            if out.is_null() {
                // System failure. Undo only what this call reserved; the
                // original block is still live and still accounted for.
                if new_total > old_total {
                    self.budget.release(new_total - old_total);
                }
                return std::ptr::null_mut();
            }
            if new_total < old_total {
                self.budget.release(old_total - new_total);
            }
            out.cast::<Header>().write(Header { size: new_size });
            out.add(HEADER_SIZE)
        }
    }

    /// # Safety
    /// `ptr` must have come from this allocator.
    unsafe fn usable_size(ptr: *mut u8) -> usize {
        // SAFETY: the caller guarantees `ptr` is ours.
        unsafe { ptr.sub(HEADER_SIZE).cast::<Header>().read().size }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::rc::Rc;

    /// A block big enough to matter and small enough to be harmless: 64 KiB
    /// against a 1 MiB budget. Nothing here allocates at a scale that could
    /// hurt the machine even with every guard removed.
    const BLOCK: usize = 64 * 1024;

    fn allocator(limit: usize) -> (BudgetedAllocator, Rc<HeapBudget>) {
        let budget = Rc::new(HeapBudget::new(limit));
        (BudgetedAllocator::new(budget.clone()), budget)
    }

    #[test]
    fn a_system_failure_is_not_reported_as_heap_exhaustion() {
        // Both return null; only one is a budget refusal. Reporting a system
        // failure as `HostMemory` would blame a ceiling that had room to spare,
        // and the caller would shorten a pattern that was never the problem.
        let (mut alloc, budget) = allocator(1024 * 1024);
        let _ = take_heap_exhausted();

        let ptr = with_failing_system_allocator(|| alloc.alloc(BLOCK));
        assert!(ptr.is_null(), "the injected failure must refuse");
        assert!(
            !heap_exhausted(),
            "a SYSTEM allocation failure must not set the heap-exhaustion flag"
        );
        assert_eq!(
            budget.live(),
            0,
            "a failed allocation must not stay accounted for"
        );

        // ...and the budget still works afterwards.
        let ptr = alloc.alloc(BLOCK);
        assert!(!ptr.is_null());
        assert!(budget.live() >= BLOCK);
        // SAFETY: from this allocator.
        unsafe { alloc.dealloc(ptr) };
        assert_eq!(budget.live(), 0);
    }

    #[test]
    fn a_budget_denial_does_set_the_flag() {
        // The complement, so the test above cannot pass because the flag is
        // never set at all.
        let (mut alloc, _budget) = allocator(BLOCK);
        let _ = take_heap_exhausted();
        let ptr = alloc.alloc(BLOCK * 4);
        assert!(ptr.is_null());
        assert!(
            take_heap_exhausted(),
            "a BUDGET denial must set the flag, or nothing distinguishes it"
        );
    }

    #[test]
    fn alignment_overflow_is_a_refusal_not_an_ffi_panic() {
        let (mut alloc, budget) = allocator(1024 * 1024);

        let ptr = alloc.alloc(usize::MAX);
        assert!(ptr.is_null(), "an unrepresentable allocation must fail");
        assert!(
            take_heap_exhausted(),
            "an unrepresentable allocation must set the structural refusal flag"
        );
        assert_eq!(budget.live(), 0, "a refused allocation was accounted");

        let ptr = alloc.calloc(usize::MAX, 1);
        assert!(ptr.is_null(), "an unrepresentable calloc must fail");
        assert!(
            take_heap_exhausted(),
            "an unrepresentable calloc must set the structural refusal flag"
        );
        assert_eq!(budget.live(), 0, "a refused calloc was accounted");

        let ptr = alloc.alloc(BLOCK);
        assert!(!ptr.is_null());
        let before = budget.live();
        // SAFETY: `ptr` came from this allocator; refusal must retain it.
        let out = unsafe { alloc.realloc(ptr, usize::MAX) };
        assert!(out.is_null(), "an unrepresentable realloc must fail");
        assert!(take_heap_exhausted());
        assert_eq!(
            budget.live(),
            before,
            "a refused realloc changed the original allocation's accounting"
        );
        // SAFETY: failed realloc leaves the original allocation valid.
        unsafe { alloc.dealloc(ptr) };
        assert_eq!(budget.live(), 0);
    }

    #[test]
    fn calloc_zeroes_the_block_and_usable_size_covers_the_request() {
        let (mut alloc, budget) = allocator(1024 * 1024);
        let ptr = alloc.calloc(3, 5);
        assert!(!ptr.is_null());
        // SAFETY: from this allocator.
        let usable = unsafe { BudgetedAllocator::usable_size(ptr) };
        assert!(usable >= 15);
        // SAFETY: the allocator reports `usable` bytes at `ptr`.
        let bytes = unsafe { std::slice::from_raw_parts_mut(ptr, usable) };
        assert!(bytes.iter().all(|byte| *byte == 0));
        bytes.fill(0xA5);
        // SAFETY: from this allocator.
        unsafe { alloc.dealloc(ptr) };
        assert_eq!(budget.live(), 0);
    }

    #[test]
    fn realloc_accounting_survives_shrink_and_regrow() {
        let (mut alloc, budget) = allocator(1024 * 1024);
        let ptr = alloc.alloc(BLOCK);
        assert!(!ptr.is_null());
        let after_alloc = budget.live();
        assert!(after_alloc >= BLOCK);

        // SHRINK: the released bytes must come back, or live usage drifts up
        // over a session until the ceiling denies work that fits.
        // SAFETY: from this allocator.
        let ptr = unsafe { alloc.realloc(ptr, BLOCK / 4) };
        assert!(!ptr.is_null());
        let after_shrink = budget.live();
        assert!(
            after_shrink < after_alloc,
            "shrinking from {BLOCK} to {} did not release: {after_alloc} -> \
             {after_shrink}",
            BLOCK / 4
        );

        // REGROW to the original size: accounting must return to where it was,
        // not to shrink-size plus the full new size.
        // SAFETY: from this allocator.
        let ptr = unsafe { alloc.realloc(ptr, BLOCK) };
        assert!(!ptr.is_null());
        assert_eq!(
            budget.live(),
            after_alloc,
            "regrowing to the original size must restore the original \
             accounting exactly"
        );

        // SAFETY: from this allocator.
        unsafe { alloc.dealloc(ptr) };
        assert_eq!(budget.live(), 0, "dealloc must release everything");
    }

    #[test]
    fn a_failed_realloc_retains_the_original_allocation_and_accounting() {
        let (mut alloc, budget) = allocator(1024 * 1024);
        let ptr = alloc.alloc(BLOCK);
        assert!(!ptr.is_null());
        let before = budget.live();
        let _ = take_heap_exhausted();

        // SAFETY: from this allocator.
        let out = with_failing_system_allocator(|| unsafe { alloc.realloc(ptr, BLOCK * 2) });
        assert!(out.is_null(), "the injected failure must refuse");
        assert_eq!(
            budget.live(),
            before,
            "a failed realloc must leave the ORIGINAL block accounted for \
             exactly as it was - neither charged for the growth it did not get \
             nor credited for the block it still holds"
        );
        assert!(
            !heap_exhausted(),
            "a system realloc failure is not a budget refusal"
        );

        // The original pointer is still valid and still ours.
        // SAFETY: the failed realloc left it untouched.
        unsafe { alloc.dealloc(ptr) };
        assert_eq!(budget.live(), 0);
    }

    #[test]
    fn a_realloc_denied_by_the_budget_keeps_the_original_too() {
        // The budget-side twin: the growth does not fit, so the call is
        // refused, and the block it already holds must be unaffected.
        let (mut alloc, budget) = allocator(BLOCK * 2);
        let ptr = alloc.alloc(BLOCK);
        assert!(!ptr.is_null());
        let before = budget.live();
        let _ = take_heap_exhausted();

        // SAFETY: from this allocator.
        let out = unsafe { alloc.realloc(ptr, BLOCK * 8) };
        assert!(out.is_null(), "growth past the ceiling must be refused");
        assert!(take_heap_exhausted(), "a budget denial must set the flag");
        assert_eq!(
            budget.live(),
            before,
            "a denied realloc must not disturb the existing accounting"
        );

        // SAFETY: untouched by the denied realloc.
        unsafe { alloc.dealloc(ptr) };
        assert_eq!(budget.live(), 0);
    }
}
