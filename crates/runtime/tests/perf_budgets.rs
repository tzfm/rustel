//! Deterministic allocation and retained-memory regression budgets for the
//! native score tier across the tracked example songs.
//!
//! All checks run sequentially in one test so the process-wide counting
//! allocator observes only the work under test with zero cross-test noise.

use rustel_fraction::Fraction;
use rustel_runtime::{Session, SessionConfig};
use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicIsize, AtomicU64, Ordering};

struct Counting;

static LIVE: AtomicIsize = AtomicIsize::new(0);
static ALLOCATIONS: AtomicU64 = AtomicU64::new(0);

// SAFETY: every method delegates to `System` with the caller's exact arguments
// and updates only relaxed atomic counters.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // SAFETY: forwarded to `System::alloc` under the caller's invariants.
        let ptr = unsafe { System.alloc(layout) };
        if !ptr.is_null() {
            ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
            LIVE.fetch_add(layout.size() as isize, Ordering::Relaxed);
        }
        ptr
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        // SAFETY: forwarded to `System::alloc_zeroed` under the caller's invariants.
        let ptr = unsafe { System.alloc_zeroed(layout) };
        if !ptr.is_null() {
            ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
            LIVE.fetch_add(layout.size() as isize, Ordering::Relaxed);
        }
        ptr
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: forwarded to `System::dealloc` under the caller's invariants.
        unsafe { System.dealloc(ptr, layout) };
        LIVE.fetch_sub(layout.size() as isize, Ordering::Relaxed);
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        // SAFETY: forwarded to `System::realloc` under the caller's invariants.
        let moved = unsafe { System.realloc(ptr, layout, new_size) };
        if !moved.is_null() {
            ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
            LIVE.fetch_add(
                new_size as isize - layout.size() as isize,
                Ordering::Relaxed,
            );
        }
        moved
    }
}

#[global_allocator]
static ALLOCATOR: Counting = Counting;

struct Measured {
    allocations: u64,
    held_bytes: i64,
}

fn measure<T>(work: impl FnOnce() -> T) -> (T, Measured) {
    let live_before = LIVE.load(Ordering::Relaxed);
    let allocs_before = ALLOCATIONS.load(Ordering::Relaxed);
    let out = work();
    let allocs_after = ALLOCATIONS.load(Ordering::Relaxed);
    let live_after = LIVE.load(Ordering::Relaxed);
    (
        out,
        Measured {
            allocations: allocs_after - allocs_before,
            held_bytes: (live_after - live_before) as i64,
        },
    )
}

fn edited(source: &str) -> String {
    let mut text = source.to_owned();
    match text.rfind(|c: char| c.is_ascii_digit()) {
        Some(at) => {
            let digit = text.as_bytes()[at] - b'0';
            let next = char::from(b'0' + (digit + 1) % 10);
            text.replace_range(at..=at, next.encode_utf8(&mut [0; 4]));
        }
        None => text.push_str("\n// edit\n"),
    }
    text
}

struct SongBudget {
    file: &'static str,
    max_cold_allocs: u64,
    max_reeval_allocs: u64,
    max_edit_allocs: u64,
    max_steady_allocs: u64,
    max_window_allocs: u64,
    max_worst_window_allocs: u64,
    max_held_bytes: i64,
}

const BUDGETS: &[SongBudget] = &[
    SongBudget {
        file: "01-lanes.strudel",
        max_cold_allocs: 60,
        max_reeval_allocs: 4,
        max_edit_allocs: 40,
        max_steady_allocs: 8,
        max_window_allocs: 2,
        max_worst_window_allocs: 2,
        max_held_bytes: 2_900,
    },
    SongBudget {
        file: "02-arps.strudel",
        max_cold_allocs: 110,
        max_reeval_allocs: 4,
        max_edit_allocs: 110,
        max_steady_allocs: 9,
        max_window_allocs: 2,
        max_worst_window_allocs: 2,
        max_held_bytes: 3_800,
    },
    SongBudget {
        file: "03-percussion.strudel",
        max_cold_allocs: 120,
        max_reeval_allocs: 4,
        max_edit_allocs: 70,
        max_steady_allocs: 9,
        max_window_allocs: 2,
        max_worst_window_allocs: 2,
        max_held_bytes: 4_000,
    },
    SongBudget {
        file: "04-chips.strudel",
        max_cold_allocs: 125,
        max_reeval_allocs: 4,
        max_edit_allocs: 125,
        max_steady_allocs: 9,
        max_window_allocs: 2,
        max_worst_window_allocs: 2,
        max_held_bytes: 4_000,
    },
    SongBudget {
        file: "05-sections.strudel",
        max_cold_allocs: 520,
        max_reeval_allocs: 4,
        max_edit_allocs: 110,
        max_steady_allocs: 38,
        max_window_allocs: 2,
        max_worst_window_allocs: 7,
        max_held_bytes: 21_000,
    },
    SongBudget {
        file: "06-distortion.strudel",
        max_cold_allocs: 160,
        max_reeval_allocs: 4,
        max_edit_allocs: 160,
        max_steady_allocs: 9,
        max_window_allocs: 2,
        max_worst_window_allocs: 2,
        max_held_bytes: 4_900,
    },
];

#[test]
fn native_tracked_songs_stay_within_allocation_and_memory_budgets() {
    let songs_dir = concat!(env!("CARGO_MANIFEST_DIR"), "/examples/songs");

    // Warm one-time process-wide registries and thread-local pools so cold-eval
    // budgets measure per-score work rather than first-use static tables.
    for budget in BUDGETS {
        let source = std::fs::read_to_string(format!("{songs_dir}/{}", budget.file))
            .expect("tracked song exists");
        let mut warm = Session::with_config(SessionConfig::default()).expect("warm session");
        warm.evaluate(&source).expect("warm evaluate");
        let _ = warm.query(Fraction::ZERO, Fraction::new(16, 1));
        drop(warm);
    }
    rustel_runtime::forget_mini_literals();

    let whole_end = Fraction::new(16, 1);
    let eighth = Fraction::new(1, 8);

    for budget in BUDGETS {
        let source = std::fs::read_to_string(format!("{songs_dir}/{}", budget.file))
            .expect("tracked song exists");
        rustel_runtime::forget_mini_literals();

        let (session, boot) = measure(|| Session::with_config(SessionConfig::default()));
        let mut session = session.expect("fresh session");
        assert!(
            boot.allocations <= 8,
            "{}: boot allocations {} exceeded budget 8",
            budget.file,
            boot.allocations
        );
        assert!(
            boot.held_bytes <= 512,
            "{}: boot held {} B exceeded budget 512 B",
            budget.file,
            boot.held_bytes
        );

        let (eval_res, cold) = measure(|| session.evaluate(&source));
        eval_res.expect("cold evaluate");
        assert!(
            session.evaluated_natively(),
            "{}: expected native tier evaluation",
            budget.file
        );
        assert!(
            !session.script_engine_started(),
            "{}: native song must not start the script engine",
            budget.file
        );
        assert!(
            cold.allocations <= budget.max_cold_allocs,
            "{}: cold eval allocations {} exceeded budget {}",
            budget.file,
            cold.allocations,
            budget.max_cold_allocs
        );

        // Warm one 16-cycle query, then check steady query allocations.
        let _ = session.query(Fraction::ZERO, whole_end);
        let (_, steady) = measure(|| session.query(Fraction::ZERO, whole_end));
        assert!(
            steady.allocations <= budget.max_steady_allocs,
            "{}: steady query allocations {} exceeded budget {}",
            budget.file,
            steady.allocations,
            budget.max_steady_allocs
        );

        // Measure 128 1/8-cycle playback windows (16 cycles).
        let mut total_win_allocs = 0u64;
        let mut worst_win_allocs = 0u64;
        let mut begin = Fraction::ZERO;
        for _ in 0..128 {
            let end = begin.checked_add(eighth).expect("window end");
            let (_, win) = measure(|| session.query(begin, end));
            total_win_allocs += win.allocations;
            worst_win_allocs = worst_win_allocs.max(win.allocations);
            begin = end;
        }
        let avg_win_allocs = total_win_allocs / 128;
        assert!(
            avg_win_allocs <= budget.max_window_allocs,
            "{}: avg window allocations {} exceeded budget {}",
            budget.file,
            avg_win_allocs,
            budget.max_window_allocs
        );
        assert!(
            worst_win_allocs <= budget.max_worst_window_allocs,
            "{}: worst window allocations {} exceeded budget {}",
            budget.file,
            worst_win_allocs,
            budget.max_worst_window_allocs
        );

        let (reeval_res, reeval) = measure(|| session.evaluate(&source));
        reeval_res.expect("re-evaluate");
        assert!(
            reeval.allocations <= budget.max_reeval_allocs,
            "{}: re-eval allocations {} exceeded budget {}",
            budget.file,
            reeval.allocations,
            budget.max_reeval_allocs
        );

        let edited_source = edited(&source);
        let (edit_res, edit) = measure(|| session.evaluate(&edited_source));
        edit_res.expect("edit evaluate");
        assert!(
            edit.allocations <= budget.max_edit_allocs,
            "{}: edit allocations {} exceeded budget {}",
            budget.file,
            edit.allocations,
            budget.max_edit_allocs
        );

        let (_, dropped) = measure(|| {
            drop(session);
            rustel_runtime::forget_mini_literals();
        });
        let held = -dropped.held_bytes;
        assert!(
            held <= budget.max_held_bytes,
            "{}: session held {} B exceeded budget {} B",
            budget.file,
            held,
            budget.max_held_bytes
        );
    }
}
