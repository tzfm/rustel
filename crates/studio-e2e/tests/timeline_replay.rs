//! Check replay navigation and focus with recorder-format tapes. Each block
//! names its index and is one hour apart, so playback cannot advance a block
//! while the test inspects the editor.

// The engine's `silent` output refuses to open unless this process's
// allocator is the audio-callback tripwire - the callback must be provably
// allocation-free, and a global allocator can only come from a binary. See
// `crates/studio/tests/studio_live_engine.rs` for the same contract.
#[global_allocator]
static AUDIO_CALLBACK_ALLOCATOR: rustel_audio::tripwire::TripwireAlloc =
    rustel_audio::tripwire::TripwireAlloc;

use crossterm::event::{KeyCode, KeyModifiers};
use rustel_studio_e2e::{Hermetic, hermetic};

/// How many blocks each test's tape holds: more than any timeline shows
/// across at the suite's frame size, so a page has somewhere to go.
const BLOCKS: usize = 20;

/// What every block's code starts with; the block's index follows it.
const BLOCK_PREFIX: &str = "$: s(\"bd\") // block ";

/// The recorded gap between two blocks: an hour, so a run started by Enter
/// is still on its first block however long a loaded machine takes to
/// settle - the next block is due by the tape's clock, not the test's.
const GAP_SECONDS: f64 = 3600.0;

/// Which block the editor is showing, read back off its text.
fn shown(studio: &Hermetic) -> usize {
    let score = studio.score();
    score
        .trim_end()
        .strip_prefix(BLOCK_PREFIX)
        .and_then(|index| index.parse().ok())
        .unwrap_or_else(|| panic!("the editor is not showing a block: {score:?}"))
}

/// Write the tape - every block's code naming its own index, so no two
/// read alike - open it as the replay view, and give the timeline the
/// keyboard with Alt+→, which also steps onto block 1.
fn open_replay(studio: &mut Hermetic) {
    studio.write_tape(
        "2026-09-05T12-09-48",
        (0..BLOCKS).map(|index| (index as f64 * GAP_SECONDS, format!("{BLOCK_PREFIX}{index}"))),
    );
    studio.open_newest_tape();
    studio.press(KeyCode::Right, KeyModifiers::ALT);
    assert!(studio.timeline_focused(), "the timeline took the keyboard");
    // PgUp/PgDn page by the width the timeline was last laid out at; render
    // once so the pane's width is read from a real frame, not a stale zero.
    studio.render();
}

/// PgDn pages forward by the blocks the timeline shows across, not one
/// block, and starts nothing; Home and End are the ends of the tape; PgUp
/// pages back by the same screenful. Every page leaves the transport
/// stopped - choosing a block is not playing it. The screenful is read from
/// the timeline as laid out, so the expected block holds at any width.
#[test]
fn paging_moves_by_the_screenful_without_playing() {
    let last = BLOCKS - 1;
    let mut studio = hermetic();
    open_replay(&mut studio);
    let page = studio
        .timeline_blocks_across()
        .expect("the timeline is laid out");
    // The premise: a screenful is more than the one block an arrow steps,
    // or paging and stepping could not be told apart.
    assert!(page >= 2, "the timeline shows {page} block(s) across");

    // Alt+→ stepped onto block 1, and it is not playing.
    assert_eq!(shown(&studio), 1);
    assert!(!studio.is_playing(), "walking a tape is never playing it");

    // PgDn from block 1 jumps exactly a screenful, and still starts nothing.
    studio.press(KeyCode::PageDown, KeyModifiers::NONE);
    assert_eq!(
        shown(&studio),
        (1 + page).min(last),
        "PgDn paged by the timeline's width"
    );
    assert!(!studio.is_playing(), "a page is not a play");

    // Home is the tape's first block, End its last.
    studio.press(KeyCode::Home, KeyModifiers::NONE);
    assert_eq!(shown(&studio), 0, "Home is the first block");
    studio.press(KeyCode::End, KeyModifiers::NONE);
    assert_eq!(shown(&studio), last, "End is the last block");

    // PgUp pages back off the last block by the same screenful, without
    // playing; PgDn from the last block has nowhere further to go.
    studio.press(KeyCode::PageDown, KeyModifiers::NONE);
    assert_eq!(shown(&studio), last, "PgDn stops at the end of the tape");
    studio.press(KeyCode::PageUp, KeyModifiers::NONE);
    assert_eq!(
        shown(&studio),
        last.saturating_sub(page),
        "PgUp paged back by the same width"
    );
    assert!(!studio.is_playing(), "a page back is not a play either");
}

/// Enter plays from the selected block and the timeline keeps the keyboard.
/// The next block is an hour later on the tape's clock, so the block on
/// screen after the settle is the one Enter chose.
#[test]
fn enter_plays_from_the_block_and_keeps_the_timeline() {
    let mut studio = hermetic();
    open_replay(&mut studio);

    studio.press(KeyCode::PageDown, KeyModifiers::NONE);
    let chosen = shown(&studio);
    assert!(chosen > 1, "PgDn chose a block further on ({chosen})");
    studio.press(KeyCode::Enter, KeyModifiers::NONE);
    studio.settle();

    assert!(
        studio.timeline_focused(),
        "Enter kept the timeline's keys: {}",
        studio.status()
    );
    assert_eq!(
        shown(&studio),
        chosen,
        "Enter played the block the timeline chose"
    );
    assert!(studio.is_playing(), "Enter started the run");
    studio.chord("ctrl+.");
}
