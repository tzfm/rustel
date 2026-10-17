//! Open the Lanes panel from the View menu, read its rows, move it with `e`
//! and hand the keyboard back with Esc.

// The engine's `silent` output refuses to open unless this process's
// allocator is the audio-callback tripwire - the callback must be provably
// allocation-free, and a global allocator can only come from a binary. See
// `crates/studio/tests/studio_live_engine.rs` for the same contract.
#[global_allocator]
static AUDIO_CALLBACK_ALLOCATOR: rustel_audio::tripwire::TripwireAlloc =
    rustel_audio::tripwire::TripwireAlloc;

use crossterm::event::{KeyCode, KeyModifiers};
use rustel_studio_e2e::{PanelKind, hermetic};

/// The panel has no chord: F1, `v` drops View, and `e` picks Lanes. A
/// toggle row runs its change and closes the bar.
fn toggle_lanes(studio: &mut rustel_studio_e2e::Hermetic) {
    studio.press(KeyCode::F(1), KeyModifiers::NONE);
    studio.press(KeyCode::Char('v'), KeyModifiers::NONE);
    studio.press(KeyCode::Char('e'), KeyModifiers::NONE);
}

/// The row of the panel's rule.
fn rule_row(rows: &[String]) -> Option<usize> {
    rows.iter().position(|row| row.contains("─ lanes ─"))
}

#[test]
fn the_panel_names_each_lane_and_moves_between_the_edges() {
    let mut studio = hermetic();
    studio.set_score("kick: s(\"sbd*4\")\nhats: s(\"white*8\").gain(.4)\n");
    studio.chord("ctrl+s");
    studio.settle();

    toggle_lanes(&mut studio);
    assert_eq!(studio.focus(), Some(PanelKind::Lanes));
    assert!(
        studio.status().starts_with("lanes docked at the bottom"),
        "{}",
        studio.status()
    );
    let rows = studio.rows();
    let rule = rule_row(&rows).expect("the panel's rule is on screen");
    let band = &rows[rule..];
    for label in ["kick", "hats"] {
        assert!(
            band.iter().any(|row| row.trim_start().starts_with(label)),
            "a row named {label}:\n{}",
            rows.join("\n")
        );
    }
    assert!(band.iter().any(|row| row.contains('┊')), "the playhead");

    studio.press(KeyCode::Char('e'), KeyModifiers::NONE);
    assert_eq!(studio.status(), "lanes along the top");
    let top = rule_row(&studio.rows()).expect("the rule after the move");
    assert!(top < rule, "the band moved up: rule {rule} -> {top}");

    studio.press(KeyCode::Esc, KeyModifiers::NONE);
    assert_eq!(studio.focus(), None);
    assert!(rule_row(&studio.rows()).is_some(), "Esc leaves it standing");

    toggle_lanes(&mut studio);
    assert_eq!(studio.status(), "lanes hidden");
    assert!(rule_row(&studio.rows()).is_none());
}
