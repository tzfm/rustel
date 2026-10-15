//! The Link wire itself: two participants on one machine, kept on
//! loopback so the test joins no network the OS may ask permission
//! for. The arithmetic of the merge is covered by the unit tests.

#![cfg(feature = "test-support")]

use std::thread;
use std::time::{Duration, Instant};

use rustel_runtime::link::{Link, speak_only_on_loopback};

/// Two participants on this machine, each with a grid of their own, end
/// up standing in one session on one grid. Skipped where the
/// announcements never come back through the looped group.
#[test]
fn two_participants_meet_on_one_grid() {
    speak_only_on_loopback();
    let Ok(first) = Link::start(120.0, 0.0) else {
        eprintln!("no Link socket here; skipping");
        return;
    };
    let Ok(second) = Link::start(90.0, 7.0) else {
        eprintln!("only one Link socket here; skipping");
        return;
    };
    let discovery_deadline = Instant::now() + Duration::from_secs(6);
    loop {
        if first.status().peers >= 1 && second.status().peers >= 1 {
            break;
        }
        if Instant::now() > discovery_deadline {
            eprintln!("no discovery here; skipping");
            return;
        }
        thread::sleep(Duration::from_millis(100));
    }
    // Both measure, one session yields, and from then on both run the
    // same tempo and the same phase, whatever each started with.
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let (a, b) = (first.status(), second.status());
        let same_tempo = (a.bpm - b.bpm).abs() < 0.5;
        let same_phase = (a.cycle - b.cycle).abs() < 0.01;
        if same_tempo && same_phase {
            break;
        }
        assert!(Instant::now() < deadline, "no shared grid within 20 s");
        thread::sleep(Duration::from_millis(100));
    }
}
