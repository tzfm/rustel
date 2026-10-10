//! Layout: splits, zen, resize, and the empty studio's shape.
//!
//! The static shape of the studio is what goldens are for; resize is a
//! state assertion (the app must survive and lay out again), not a golden
//! one, because every machine's minimum widths differ by one glyph here
//! and there.

// The engine's `silent` output refuses to open unless this process's
// allocator is the audio-callback tripwire - the callback must be provably
// allocation-free, and a global allocator can only come from a binary. See
// `crates/studio/tests/studio_live_engine.rs` for the same contract.
#[global_allocator]
static AUDIO_CALLBACK_ALLOCATOR: rustel_audio::tripwire::TripwireAlloc =
    rustel_audio::tripwire::TripwireAlloc;

use crossterm::event::{KeyCode, KeyModifiers};
use rustel_studio_e2e::{assert_golden, hermetic, hermetic_sized};

/// The empty studio's shape at the standard size, as a golden. Live values
/// (meter, playhead, stats) make rows differ by platform-neutral
/// determinism only; if this golden ever contains one, the test is wrong,
/// not the studio.
#[test]
fn the_empty_studio_at_150x40() {
    let mut studio = hermetic();
    assert_golden(&mut studio, "layout/empty_150x40");
}

/// The same at 80×24 - the narrow case the footer and the strip must
/// survive. Resize is real: the app is told first, then painted at the
/// new size.
#[test]
fn the_empty_studio_at_80x24() {
    let mut studio = hermetic_sized(80, 24);
    assert_golden(&mut studio, "layout/empty_80x24");
}

/// Ctrl+E opens a split, hops the keyboard between panes on further
/// presses, and Ctrl+Shift+E closes it - leaving one pane holding the
/// whole score.
#[test]
fn a_split_opens_hops_and_closes() {
    let mut studio = hermetic();

    studio.chord("ctrl+e");
    studio.settle();
    let split = studio.screen();
    assert!(
        split.lines().count() == 40,
        "a split does not change the grid's height:\n{split}"
    );

    // Hop: with two panes, Ctrl+E moves the keyboard to the other pane.
    studio.chord("ctrl+e");
    studio.type_text("1");
    let panes = studio.source();
    assert!(
        panes.contains('1'),
        "the second pane took the key:\n{panes}"
    );

    studio.chord("ctrl+shift+e");
    studio.settle();
    let closed = studio.screen();
    assert_ne!(split, closed, "closing the split changes the frame");
}

/// F11 toggles zen: the chrome folds away, and F11 brings it back. The
/// restored frame is the old frame except the status line, which zen's
/// exit legitimately rewrites ("stage restored") - so the comparison
/// holds every row but that one.
#[test]
fn zen_folds_the_chrome_away_and_back() {
    let mut studio = hermetic();

    let before = studio.screen();
    studio.press(KeyCode::F(11), KeyModifiers::NONE);
    studio.settle();
    let zen = studio.screen();
    assert_ne!(before, zen, "zen changed the frame");

    studio.press(KeyCode::F(11), KeyModifiers::NONE);
    studio.settle();
    let restored = studio.screen();
    let before_rows: Vec<&str> = before.lines().collect();
    let restored_rows: Vec<&str> = restored.lines().collect();
    assert_eq!(
        before_rows.len(),
        restored_rows.len(),
        "the restored frame has the same shape"
    );
    for (index, (was, now)) in before_rows.iter().zip(restored_rows.iter()).enumerate() {
        if index == before_rows.len() - 2 {
            continue; // The status line: zen's exit legitimately rewrites it.
        }
        assert_eq!(
            was, now,
            "row {index} differs after zen folded away and back"
        );
    }
}

/// A resize down to 80×24 and back to 150×40 lays out both times and keeps
/// the score's text intact - the editor's viewport shrinks, its document
/// does not.
#[test]
fn resizing_down_and_back_keeps_the_score() {
    let mut studio = hermetic();
    let before = studio.source();

    studio.resize(80, 24);
    studio.settle();
    let narrow = studio.rows();
    assert_eq!(narrow.len(), 24, "the frame is 24 rows");
    assert!(
        narrow.iter().all(|row| row.chars().count() == 80),
        "every row is 80 cells wide"
    );

    studio.resize(150, 40);
    studio.settle();
    assert_eq!(studio.source(), before, "the score survived the round trip");
    let wide = studio.rows();
    assert_eq!(wide.len(), 40, "the frame is 40 rows again");
}

/// Walk the sheet's rows by label until the given one is selected - by
/// what is drawn, not by row arithmetic. Up/Down move the selection on
/// every list page (only Right/Left/Space change values), so the walk
/// itself touches nothing.
fn select_row(studio: &mut rustel_studio_e2e::Hermetic, marker: &str) {
    for _ in 0..40 {
        if studio.rows().iter().any(|row| row.contains(marker)) {
            return;
        }
        studio.press(KeyCode::Down, KeyModifiers::NONE);
        studio.pump();
    }
    panic!("no row reads {marker:?}:\n{}", studio.rows().join("\n"));
}

/// Check the map's cells, excluding the header spinner and reference column.
/// Use its enabled geometry even in a test with no map.
fn has_minimap_braille(rows: &[String], reference_open: bool) -> bool {
    let width = rows[0].chars().count() as u16;
    let area = ((0, 0).into(), (width, rows.len() as u16).into()).into();
    let layout = rustel_studio::view::regions(area, false, 1, reference_open, true, None, None);
    let map = layout.panes[0].minimap;
    rows[usize::from(map.y)..usize::from(map.bottom())]
        .iter()
        .any(|row| {
            row.chars()
                .skip(usize::from(map.x))
                .take(usize::from(map.width))
                .any(|c| ('\u{2800}'..='\u{28ff}').contains(&c))
        })
}

/// With the minimap switch on, a tall pane draws the braille map at its
/// right edge. With it off (the default), a score taller than a page keeps
/// a scrollbar there. The map survives a resize down and back.
#[test]
fn the_minimap_and_the_reference_column_survive_a_resize() {
    let mut studio = hermetic();

    // The minimap and the scrollbar both need a score taller than a page.
    studio.type_text("$: s(\"bd\")\n");
    for line in 0..39 {
        studio.type_text(&format!("// line {line} s(\"hh\")\n"));
    }
    studio.settle();

    // Switch off (the default): the scrollbar fallback holds the edge.
    let fallback = studio.rows();
    assert!(
        fallback
            .iter()
            .any(|row| row.trim_end().ends_with('┃') || row.trim_end().ends_with('│')),
        "a page-overflowing score without the minimap keeps a scrollbar at the right edge:\n{}",
        fallback.join("\n")
    );
    assert!(
        !has_minimap_braille(&fallback, false),
        "no minimap draws while the switch is off:\n{}",
        fallback.join("\n")
    );

    // Switch on through the sheet, as a player does: the map takes the
    // edge and the scrollbar goes away.
    studio.chord("ctrl+o");
    studio.settle();
    select_row(&mut studio, "▸ minimap");
    studio.press(KeyCode::Right, KeyModifiers::NONE);
    studio.settle();
    studio.press(KeyCode::Esc, KeyModifiers::NONE);
    studio.settle();

    let mapped = studio.rows();
    assert!(
        has_minimap_braille(&mapped, false),
        "the minimap draws at the pane's right edge once the switch is on:\n{}",
        mapped.join("\n")
    );
    assert!(
        !mapped
            .iter()
            .any(|row| row.trim_end().ends_with('┃') || row.trim_end().ends_with('│')),
        "the scrollbar gives the edge up to the minimap:\n{}",
        mapped.join("\n")
    );

    // The reference column opens, and the minimap survives a resize down
    // and back.
    studio.chord("ctrl+space");
    studio.settle();
    let with_reference = studio.rows();
    let header_row = with_reference
        .iter()
        .position(|row| row.contains("first"))
        .expect("the strip names the starter scene");

    studio.resize(80, 24);
    studio.settle();
    studio.resize(150, 40);
    studio.settle();

    let after = studio.rows();
    assert!(
        after.len() > header_row,
        "the strip is still on screen after the round trip"
    );
    assert!(
        has_minimap_braille(&after, true),
        "the minimap is still drawn after the resize round trip:\n{}",
        after.join("\n")
    );
}
