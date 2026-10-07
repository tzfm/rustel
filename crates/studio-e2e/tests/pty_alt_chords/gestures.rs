//! Editor gestures decoded from terminal bytes, with saved files as evidence
//! of which selection and pane received the keys.

use std::path::Path;
use std::time::{Duration, Instant};

use super::{Screen, Terminal, rustel_binary};

const COLS: u16 = 100;
const ROWS: u16 = 30;
const REPAINT_BUDGET: Duration = Duration::from_secs(10);

fn open_scores(left: &str, right: Option<&str>) -> (tempfile::TempDir, Terminal) {
    let binary = rustel_binary().expect("build the binary first");
    let home = tempfile::tempdir().expect("temporary set");
    std::fs::write(home.path().join("live.strudel"), left).expect("left score");
    if let Some(right) = right {
        std::fs::write(home.path().join("other.strudel"), right).expect("right score");
        let manifest = serde_json::json!({
            "scenes": [{"file": "live.strudel"}, {"file": "other.strudel"}],
            "current": "live.strudel"
        });
        std::fs::write(home.path().join("rustel-set.json"), manifest.to_string())
            .expect("two-scene set");
    }
    // Sine scores need no samples; a local source keeps startup independent
    // of remote manifests and the runner's sample catalogue.
    let samples = home.path().join("samples");
    std::fs::create_dir(&samples).expect("local sample folder");
    let prefs = serde_json::json!({"sample_sources": [{"spec": samples}]});
    std::fs::write(home.path().join("studio.json"), prefs.to_string())
        .expect("local sample preferences");
    let terminal = Terminal::open(&binary, ROWS, COLS, home.path());
    wait_for_screen(
        &terminal,
        Duration::from_secs(30),
        "ready studio",
        |screen| screen.contains(" ready"),
    );
    (home, terminal)
}

fn wait_for_screen(
    terminal: &Terminal,
    budget: Duration,
    description: &str,
    matches: impl Fn(&Screen) -> bool,
) -> Screen {
    let deadline = Instant::now() + budget;
    loop {
        // Search the reconstructed screen, never the raw history: a row
        // scrolled out of view must stop satisfying the assertion.
        let mut screen = Screen::new(usize::from(COLS), usize::from(ROWS));
        screen.feed(&terminal.text());
        if matches(&screen) {
            return screen;
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for {description}:\n{}",
            (0..screen.rows)
                .map(|row| screen.row_text(row))
                .collect::<Vec<_>>()
                .join("\n")
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Coordinates are one-based, as SGR mouse reports require.
fn position(screen: &Screen, needle: &str) -> (usize, usize) {
    (0..screen.rows)
        .find_map(|row| {
            let line = screen.row_text(row);
            line.find(needle)
                .map(|offset| (line[..offset].chars().count() + 1, row + 1))
        })
        .unwrap_or_else(|| panic!("{needle:?} is absent from the screen"))
}

fn mouse(terminal: &mut Terminal, button: u8, x: usize, y: usize, release: bool) {
    let suffix = if release { 'm' } else { 'M' };
    terminal.write(format!("\x1b[<{button};{x};{y}{suffix}").as_bytes());
}

fn wait_for_saved(terminal: &Terminal, path: &Path, expected: &str) {
    let deadline = Instant::now() + REPAINT_BUDGET;
    loop {
        let actual = std::fs::read_to_string(path).expect("saved score readable");
        if actual == expected {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "saved score differs: expected {expected:?}, got {actual:?}\n{}",
            terminal.diagnostics()
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn split(terminal: &mut Terminal) -> Screen {
    terminal.write(b"\x05"); // Ctrl+E opens the next scene on the right.
    wait_for_screen(terminal, REPAINT_BUDGET, "both editor panes", |screen| {
        screen.contains("LEFT_TOKEN") && screen.contains("RIGHT_TOKEN")
    })
}

#[test]
fn sgr_drag_replaces_only_the_selected_source() {
    let source = "// before DRAG_TOKEN after\n$: note(\"c4\").s(\"sine\")\n";
    let (home, mut terminal) = open_scores(source, None);
    let screen = wait_for_screen(&terminal, REPAINT_BUDGET, "drag target", |screen| {
        screen.contains("DRAG_TOKEN")
    });
    let (x, y) = position(&screen, "DRAG_TOKEN");
    mouse(&mut terminal, 0, x, y, false);
    mouse(&mut terminal, 32, x + "DRAG_TOKEN".len(), y, false);
    mouse(&mut terminal, 0, x + "DRAG_TOKEN".len(), y, true);
    terminal.write(b"replacement\x13"); // Ctrl+S saves the resulting selection edit.
    wait_for_saved(
        &terminal,
        &home.path().join("live.strudel"),
        "// before replacement after\n$: note(\"c4\").s(\"sine\")\n",
    );
    wait_for_screen(&terminal, REPAINT_BUDGET, "replaced selection", |screen| {
        screen.contains("before replacement after") && !screen.contains("DRAG_TOKEN")
    });
}

#[test]
fn sgr_drag_across_a_split_edge_stays_in_its_originating_editor() {
    let left = "// LEFT_TOKEN\n$: note(\"c4\").s(\"sine\")\n";
    let right = "// RIGHT_TOKEN\n$: note(\"d4\").s(\"sine\")\n";
    let (home, mut terminal) = open_scores(left, Some(right));
    let screen = split(&mut terminal);
    let (left_x, y) = position(&screen, "LEFT_TOKEN");
    let (right_x, right_y) = position(&screen, "RIGHT_TOKEN");
    assert_eq!(y, right_y, "the scores begin on the same editor row");
    assert!(left_x < right_x, "the second score is in the right pane");

    // The split initially focuses the right pane. A press in the left
    // source starts its selection; dragging over the other pane clamps
    // to the originating line instead of handing the gesture across.
    mouse(&mut terminal, 0, left_x, y, false);
    mouse(&mut terminal, 32, right_x, y, false);
    mouse(&mut terminal, 0, right_x, y, true);
    terminal.write(b"LEFT_REPLACED\x13");
    wait_for_saved(
        &terminal,
        &home.path().join("live.strudel"),
        "// LEFT_REPLACED\n$: note(\"c4\").s(\"sine\")\n",
    );
    assert_eq!(
        std::fs::read_to_string(home.path().join("other.strudel")).expect("right score"),
        right,
        "a drag crossing the divider never edits the other scene"
    );
    wait_for_screen(
        &terminal,
        REPAINT_BUDGET,
        "right pane preserved",
        |screen| screen.contains("RIGHT_TOKEN"),
    );
}

#[test]
fn sgr_wheel_preserves_focus_and_f10_moves_keys_between_split_panes() {
    let mut left = "// LEFT_TOKEN\n".to_owned();
    for row in 1..60 {
        left.push_str(&format!("// LEFT_ROW_{row:02}\n"));
    }
    left.push_str("$: note(\"c4\").s(\"sine\")\n");
    let right = "// RIGHT_TOKEN\n$: note(\"d4\").s(\"sine\")\n";
    let (home, mut terminal) = open_scores(&left, Some(right));
    let screen = split(&mut terminal);
    let (x, y) = position(&screen, "LEFT_TOKEN");

    mouse(&mut terminal, 65, x, y, false); // Wheel down over the unfocused pane.
    wait_for_screen(
        &terminal,
        REPAINT_BUDGET,
        "left pane scrolled down",
        |screen| {
            !screen.contains("LEFT_TOKEN")
                && screen.contains("LEFT_ROW_03")
                && screen.contains("RIGHT_TOKEN")
        },
    );
    terminal.write(b"// RIGHT_EDIT\r\x13");
    let right_edited = format!("// RIGHT_EDIT\n{right}");
    wait_for_saved(&terminal, &home.path().join("other.strudel"), &right_edited);
    assert_eq!(
        std::fs::read_to_string(home.path().join("live.strudel")).expect("left score"),
        left,
        "the wheel scrolls under the pointer without taking the keys"
    );

    mouse(&mut terminal, 64, x, y, false); // Wheel up restores the first source row.
    wait_for_screen(
        &terminal,
        REPAINT_BUDGET,
        "left pane scrolled up",
        |screen| screen.contains("LEFT_TOKEN") && screen.contains("RIGHT_EDIT"),
    );
    terminal.write(b"\x1b[21~// LEFT_EDIT\r\x13"); // F10 hops to the left pane.
    wait_for_saved(
        &terminal,
        &home.path().join("live.strudel"),
        &format!("// LEFT_EDIT\n{left}"),
    );
    terminal.write(b"\x1b[21~// RIGHT_AGAIN\r\x13"); // F10 returns to the right caret.
    wait_for_saved(
        &terminal,
        &home.path().join("other.strudel"),
        &format!("// RIGHT_EDIT\n// RIGHT_AGAIN\n{right}"),
    );
}
