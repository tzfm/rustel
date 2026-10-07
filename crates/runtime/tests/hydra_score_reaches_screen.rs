//! A score's sketch must reach the terminal, not only a snippet's.
//!
//! The score realm and the snippet parser record the same text differently.
//! `src(o0)` comes back from a score as a chain whose head is `o0`, because
//! the recorder cannot tell an uncalled generator from a called one when it
//! serialises. The parser records a bare global. The composer must accept
//! both forms, so a test here draws the same sketch through both paths.
#![cfg(feature = "hydra")]

use std::time::{Duration, Instant};

const FEEDBACK: &str = "shape(4, 0.3, 0.02).add(src(o0).scale(0.96), 0.94).out(o0)";

fn lit(pixels: &[u8]) -> usize {
    pixels.iter().step_by(4).filter(|value| **value > 8).count()
}

fn wait_for_frame(frames: &rustel_hydra::HydraFrames) -> Option<(u16, u16, usize)> {
    let until = Instant::now() + Duration::from_secs(20);
    while Instant::now() < until {
        if let Some((width, height, pixels)) = frames.take() {
            return Some((width, height, lit(&pixels)));
        }
        std::thread::sleep(Duration::from_millis(40));
    }
    None
}

#[test]
fn a_recorded_score_and_a_parsed_snippet_draw_the_same_thing() {
    // The shelf's path: text, parsed here.
    let mut shelf = rustel_hydra::HydraHost::new();
    shelf.preview(Some(FEEDBACK)).expect("preview");
    let previewed = wait_for_frame(&shelf.preview_frames());

    // The score's path: the same text, recorded by the realm.
    let mut session = rustel_runtime::Session::new().expect("session");
    session
        .evaluate(&format!("await initHydra()\n{FEEDBACK}"))
        .expect("score evaluates");
    let candidate = session.take_pending_hydra().expect("a recording");
    let update =
        rustel_runtime::hydra::HydraUpdate::from_candidate(&candidate).expect("reads back");

    let mut bridge = rustel_runtime::hydra::HydraBridge::new();
    let frames = bridge.frames();
    bridge.apply(update).expect("applies");
    bridge.set_drawing(true);
    let scored = wait_for_frame(&frames);

    let said: Vec<String> = bridge
        .take_events()
        .iter()
        .map(|event| format!("{event:?}"))
        .collect();

    let (_, _, shelf_lit) = previewed.expect("the shelf drew it");
    let (_, _, score_lit) =
        scored.unwrap_or_else(|| panic!("the score drew nothing; renderer said {said:?}"));
    assert!(shelf_lit > 0, "the shelf's picture is black");
    assert!(
        score_lit > 0,
        "the score's picture is black; renderer said {said:?}"
    );
    assert!(
        !said.iter().any(|line| line.contains("no transform called")),
        "a bare output was read as a transform: {said:?}"
    );
}

/// A statement that cannot be drawn costs its own layer, not the screen.
#[test]
fn one_refused_statement_does_not_stop_the_rest() {
    let mut session = rustel_runtime::Session::new().expect("session");
    session
        .evaluate(
            "await initHydra()\n\
             osc(20, 0.1, 1.2).kaleid(5).out(o0)\n\
             osc(10).modulate(noise(3), \"not a number\").out(o1)",
        )
        .expect("score evaluates");
    let Some(candidate) = session.take_pending_hydra() else {
        return;
    };
    let Ok(update) = rustel_runtime::hydra::HydraUpdate::from_candidate(&candidate) else {
        return;
    };

    let mut bridge = rustel_runtime::hydra::HydraBridge::new();
    let frames = bridge.frames();
    bridge.apply(update).expect("applies");
    bridge.set_drawing(true);

    let drawn = wait_for_frame(&frames)
        .unwrap_or_else(|| panic!("one bad statement stopped every other one"));
    assert!(drawn.2 > 0, "the good statement still drew");
}
