//! Check the layout of large static surfaces with settled, hermetic frames.
//! Pin host readings and set-name length; assert_golden scrubs the set path.
//! Settings and log goldens are excluded because their truncated paths vary
//! with the host's temp-root length and cannot be scrubbed reliably.

// The engine's `silent` output refuses to open unless this process's
// allocator is the audio-callback tripwire - the callback must be provably
// allocation-free, and a global allocator can only come from a binary. See
// `crates/studio/tests/studio_live_engine.rs` for the same contract.
#[global_allocator]
static AUDIO_CALLBACK_ALLOCATOR: rustel_audio::tripwire::TripwireAlloc =
    rustel_audio::tripwire::TripwireAlloc;

use crossterm::event::{KeyCode, KeyModifiers};
use rustel_studio_e2e::{assert_golden, hermetic};

/// The mixer's desk: a strip each for the input, the orbits the score names,
/// and the master. Its column positions are the contract - a substring check
/// cannot distinguish a rail that moved one cell from one that did not move.
#[test]
fn the_mixer_desk() {
    let mut studio = hermetic();
    studio.press(KeyCode::F(4), KeyModifiers::NONE);
    studio.settle();
    assert_golden(&mut studio, "panels/mixer_desk");
}

/// The reference browser, which is documentation rendered as UI: a long,
/// densely laid-out surface where a change to the column widths or the
/// entry list is exactly what a substring assertion would miss.
#[test]
fn the_reference_browser() {
    let mut studio = hermetic();
    studio.chord("ctrl+f");
    studio.settle();
    assert_golden(&mut studio, "panels/reference");
}

/// The menu bar, which F1 opens outside zen mode. Seven titles in a fixed
/// order with their mnemonics - a menu that moves or loses a title is a
/// navigation change, and this is the screen that says so.
#[test]
fn the_menu_bar() {
    let mut studio = hermetic();
    studio.press(KeyCode::F(1), KeyModifiers::NONE);
    studio.settle();
    assert_golden(&mut studio, "panels/menu_bar");
}
