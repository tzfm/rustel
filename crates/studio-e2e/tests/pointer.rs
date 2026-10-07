//! Drive clicks, drags and wheels through Event::Mouse and real dispatch.
//! Locate targets in rendered frames; count characters rather than UTF-8 bytes.

// The launch test plays the set, and the engine's `silent` output refuses
// to open unless this process's allocator is the audio-callback tripwire.
#[global_allocator]
static AUDIO_CALLBACK_ALLOCATOR: rustel_audio::tripwire::TripwireAlloc =
    rustel_audio::tripwire::TripwireAlloc;

use crossterm::event::{KeyCode, KeyModifiers, MouseButton, MouseEventKind};
use rustel_studio_e2e::{
    PanelKind, glyph_column, hermetic, hermetic_with_live_slider, hermetic_with_local_bank,
    row_containing,
};

/// (column, row) of a needle in the rendered frame - the needle's first
/// character, counted in columns.
fn locate(rows: &[String], needle: &str) -> (u16, u16) {
    let y = row_containing(rows, needle);
    let byte = rows[y].find(needle).expect("needle is in the row named");
    let x = rows[y][..byte].chars().count() as u16;
    (x, y as u16)
}

/// Where the master dock's bar is, read from the frame. The dock's `MASTER`
/// title starts at the dock's left edge. The bar is the row above it and
/// ends where the gain readout begins. The scale follows from those two
/// columns: the fader's knee sits at a known fraction of the travel. The
/// handle is a translucent cap with no glyph of its own to find.
struct DockBar {
    loudest: u16,
    loud: u16,
    quiet: u16,
}

fn dock_bar(rows: &[String]) -> DockBar {
    // The title row: the last row carrying `MASTER` (the footer's is
    // alone in that).
    let title_y = rows
        .iter()
        .rposition(|row| row.contains("MASTER"))
        .expect("the master dock is drawn");
    let title = &rows[title_y];
    let bar_x = title.find("MASTER").expect("the title is in the row named") as u16;
    // The bar is the row above. The gain readout is a right-aligned
    // 8-column field ending in `dB`, drawn just past the bar - its `d`
    // sits on the bar's own last column plus six, so the bar's right
    // edge is six columns back from the `d`.
    let bar_row = &rows[title_y - 1];
    let byte = bar_row
        .rfind("dB")
        .unwrap_or_else(|| panic!("no gain readout on the bar row:\n{}", rows.join("\n")));
    let bar_right = bar_row[..byte].chars().count() as u16;
    let bar_right = bar_right.saturating_sub(6);
    let travel = bar_right.saturating_sub(bar_x);
    // The fader's scale: 0 dB sits at `KNEE_POSITION + (20/26)*0.6` of
    // the travel, the loud working range just above it, and the quiet
    // end at the bar's left edge. Each column here is inside the bar.
    let at = |fraction: f32| bar_x + (fraction * travel as f32).round() as u16;
    DockBar {
        loudest: bar_right,
        loud: at(0.9),
        quiet: bar_x + 1,
    }
}

/// The row the fader's bar rides on: the `MASTER` title's row above.
fn bar_y(rows: &[String]) -> u16 {
    rows.iter()
        .rposition(|row| row.contains("MASTER"))
        .expect("the master dock is drawn")
        .checked_sub(1)
        .expect("the bar is not the screen's first row") as u16
}

fn ctrl_click(studio: &mut rustel_studio_e2e::Hermetic, x: u16, y: u16) {
    studio.mouse(
        MouseEventKind::Down(MouseButton::Left),
        x,
        y,
        KeyModifiers::CONTROL,
    );
    studio.mouse(
        MouseEventKind::Up(MouseButton::Left),
        x,
        y,
        KeyModifiers::CONTROL,
    );
}

fn right_click(studio: &mut rustel_studio_e2e::Hermetic, x: u16, y: u16) {
    studio.mouse(
        MouseEventKind::Down(MouseButton::Right),
        x,
        y,
        KeyModifiers::NONE,
    );
    studio.mouse(
        MouseEventKind::Up(MouseButton::Right),
        x,
        y,
        KeyModifiers::NONE,
    );
}

fn wheel(studio: &mut rustel_studio_e2e::Hermetic, down: bool, x: u16, y: u16) {
    let kind = if down {
        MouseEventKind::ScrollDown
    } else {
        MouseEventKind::ScrollUp
    };
    studio.mouse(kind, x, y, KeyModifiers::NONE);
}

fn drag(studio: &mut rustel_studio_e2e::Hermetic, from: (u16, u16), to: (u16, u16)) {
    studio.mouse(
        MouseEventKind::Down(MouseButton::Left),
        from.0,
        from.1,
        KeyModifiers::NONE,
    );
    studio.mouse(
        MouseEventKind::Drag(MouseButton::Left),
        to.0,
        to.1,
        KeyModifiers::NONE,
    );
    studio.mouse(
        MouseEventKind::Up(MouseButton::Left),
        to.0,
        to.1,
        KeyModifiers::NONE,
    );
}

/// A plain click on a scene chip selects that scene, and plays nothing.
#[test]
fn a_click_on_a_scene_chip_selects_it() {
    let mut studio = hermetic();
    studio.chord("ctrl+n");
    studio.settle();
    assert_eq!(studio.scene_names().len(), 2, "setup: two scenes");

    // The starter scene is named `first`, so the new one takes the first
    // free numbered name - `scene 1`, not `scene 2`.
    let rows = studio.rows();
    let (x, y) = locate(&rows, "2 scene 1");
    studio.click(x + 2, y);

    assert_eq!(
        studio.current_scene_index(),
        1,
        "the click selected the second scene"
    );
    assert!(!studio.is_playing(), "selecting a scene is not playing it");
}

/// No click plays a scene: no modifier, no right button. macOS turns
/// Ctrl+click into a right click, so the test checks both. A click selects
/// the scene; ^S or a pad plays it.
#[test]
fn no_click_on_a_scene_chip_plays_it() {
    let mut studio = hermetic();
    studio.chord("ctrl+n");
    studio.settle();
    assert_eq!(studio.scene_names().len(), 2, "setup: two scenes");

    // The starter scene is named `first`, so the new one takes the first
    // free numbered name - `scene 1`, not `scene 2`.
    let rows = studio.rows();
    let (x, y) = locate(&rows, "2 scene 1");
    ctrl_click(&mut studio, x + 2, y);
    studio.settle();
    assert!(
        !studio.is_playing(),
        "the Ctrl+click played nothing: {}",
        studio.status()
    );
    assert_eq!(
        studio.current_scene_index(),
        1,
        "the click still chose the scene it landed on"
    );

    // The right button - what macOS actually sends for Ctrl+click.
    right_click(&mut studio, x + 2, y);
    studio.settle();
    assert!(!studio.is_playing(), "the right click played nothing");

    // A plain click chooses; ^S plays the one you are looking at.
    let rows = studio.rows();
    let (first_x, first_y) = locate(&rows, "1 first");
    studio.click(first_x + 2, first_y);
    assert_eq!(
        studio.current_scene_index(),
        0,
        "the plain click chose the first scene"
    );
    assert!(!studio.is_playing(), "choosing is not playing");
    studio.chord("ctrl+s");
    studio.settle();
    assert!(
        studio.is_playing(),
        "^S played the focused scene: {}",
        studio.status()
    );
    let rows = studio.rows();
    let chip_row = &rows[locate(&rows, "1 first").1 as usize];
    assert!(
        chip_row.contains('▶'),
        "the playing scene's chip says so: {chip_row}"
    );
}

/// The header's readiness chip is a button: pressing it opens the log at
/// what it is counting - the click the docs hang on the badge.
#[test]
fn a_click_on_the_readiness_chip_opens_the_log() {
    let mut studio = hermetic();

    let rows = studio.rows();
    let (x, y) = locate(&rows, "✓ ready");
    studio.click(x + 2, y);

    assert_eq!(
        studio.focus(),
        Some(PanelKind::Log),
        "the readiness chip opened the log"
    );
}

/// The footer's ♪ chip opens the device picker over itself, keyboard and
/// all - and Esc hands everything back.
#[test]
fn a_click_on_the_device_chip_opens_the_picker() {
    let mut studio = hermetic();

    let rows = studio.rows();
    let (x, y) = locate(&rows, "♪ no output");
    studio.click(x + 1, y);

    assert_eq!(
        studio.focus(),
        Some(PanelKind::Devices),
        "the ♪ chip opened the picker"
    );
    assert!(studio.panel().is_some(), "the picker is standing");

    studio.press(KeyCode::Esc, KeyModifiers::NONE);
    assert_eq!(studio.focus(), None, "Esc closed the picker");
    assert!(studio.panel().is_none());
}

/// A menu row works by mouse, end to end: File ▸ Quit quits outright -
/// a menu choice is not a slip of the hand, so there is no are-you-sure.
#[test]
fn a_menu_row_works_by_mouse() {
    let mut studio = hermetic();

    studio.press(KeyCode::F(1), KeyModifiers::NONE);
    assert!(studio.menu_open(), "setup: the bar is up");
    let rows = studio.rows();
    let (x, y) = locate(&rows, "File");
    studio.click(x + 1, y);
    assert!(studio.menu_open(), "the click dropped the File menu");

    let rows = studio.rows();
    let (x, y) = locate(&rows, "Quit");
    studio.click(x + 1, y);
    assert!(
        studio.wants_quit(),
        "clicking Quit quit without asking: {}",
        studio.status()
    );
}

/// A press outside an open dropdown dismisses it and is consumed - the
/// click must not fall through onto the score, moving the caret or typing.
#[test]
fn a_click_outside_an_open_menu_dismisses_it_without_touching_the_score() {
    let mut studio = hermetic();
    let before = studio.score();

    studio.press(KeyCode::F(1), KeyModifiers::NONE);
    let rows = studio.rows();
    let (x, y) = locate(&rows, "File");
    studio.click(x + 1, y);
    assert!(studio.menu_open(), "setup: the File menu is down");

    // Mid-editor, nowhere near the dropdown.
    studio.click(60, 12);
    assert!(!studio.menu_open(), "the outside click dismissed the menu");
    assert_eq!(
        studio.score(),
        before,
        "the dismissed click never reached the score"
    );
}

/// The master dock's bar is a fader: a press near the loud end raises the
/// gain, a press low on the bar lowers it, and the readout follows. The
/// handle is a translucent cap, so the test aims from the bar: the
/// readout's column marks the bar's end and the scale's knee sits at a
/// known fraction of the travel.
#[test]
fn dragging_the_master_fader_moves_the_gain_both_ways() {
    let mut studio = hermetic();
    assert_eq!(studio.master_gain_db(), 0.0, "setup: unity");

    let rows = studio.rows();
    let bar = dock_bar(&rows);
    drag(
        &mut studio,
        (bar.loud, bar_y(&rows)),
        (bar.loudest, bar_y(&rows)),
    );
    let up = studio.master_gain_db();
    assert!(
        up > 4.0,
        "the drag to the bar's loud end raised the fader, got {up} dB: {}",
        studio.status()
    );
    let rows = studio.rows();
    let readout = &rows[locate(&rows, "+6.0dB").1 as usize];
    assert!(
        readout.contains("+6.0dB"),
        "the readout says where the fader is: {readout}"
    );

    // Down the bar from wherever the fader now stands: lower than it was.
    let rows = studio.rows();
    let bar = dock_bar(&rows);
    drag(
        &mut studio,
        (bar.loud, bar_y(&rows)),
        (bar.quiet, bar_y(&rows)),
    );
    let down = studio.master_gain_db();
    assert!(
        down < up - 1.0,
        "the second drag brought the fader down: {up} dB became {down} dB"
    );
}

/// The wheel over the master meter steps the fader half a decibel at a
/// time - down lowers, up raises - wherever the fader happens to stand.
#[test]
fn the_wheel_over_the_master_meter_steps_the_fader() {
    let mut studio = hermetic();

    let rows = studio.rows();
    let bar = dock_bar(&rows);
    let y = bar_y(&rows);
    wheel(&mut studio, true, bar.loud, y);
    let lowered = studio.master_gain_db();
    assert!(
        (lowered + 0.5).abs() < 1e-3,
        "the wheel lowered the fader half a decibel, got {lowered} dB"
    );

    wheel(&mut studio, false, bar.loud, y);
    let raised = studio.master_gain_db();
    assert!(
        raised.abs() < 1e-3,
        "the wheel back up returned to unity, got {raised} dB"
    );
}

/// A drag on a live slider's pill rewrites the literal - the knob goes
/// under the pointer, the score follows - and the status names the
/// control the way an armed slider does.
#[test]
fn dragging_a_slider_pill_rewrites_the_literal() {
    let mut studio = hermetic_with_live_slider();

    let rows = studio.rows();
    let (knob, y) = glyph_column(&rows, '█');
    drag(&mut studio, (knob, y), (knob + 4, y));

    let source = studio.source();
    assert!(
        source.contains("slider(1,0,1,0.1)"),
        "the drag pushed the knob to the top of its travel and rewrote the \
         literal:\n{source}"
    );
    assert!(
        studio.status().contains("Enter expands"),
        "the press armed the slider and said so: {}",
        studio.status()
    );
}

/// A click on the pill leaves the slider armed: ←/→ then step the control
/// without touching the caret, and Esc lets it go.
#[test]
fn a_click_on_the_pill_arms_the_arrows() {
    let mut studio = hermetic_with_live_slider();

    let rows = studio.rows();
    let (knob, y) = glyph_column(&rows, '█');
    studio.click(knob, y);
    studio.press(KeyCode::Right, KeyModifiers::NONE);

    assert!(
        studio.source().contains("slider(0.9,0,1,0.1)"),
        "the arrow stepped the armed slider one notch:\n{}",
        studio.source()
    );
    studio.press(KeyCode::Esc, KeyModifiers::NONE);
    assert!(
        studio.status().contains("slider let go"),
        "Esc disarms: {}",
        studio.status()
    );
}

/// The wheel over a pill steps the slider - up a notch, down a notch -
/// the same notch the arrows step.
#[test]
fn the_wheel_over_a_slider_pill_steps_it() {
    let mut studio = hermetic_with_live_slider();

    let rows = studio.rows();
    let (knob, y) = glyph_column(&rows, '█');
    wheel(&mut studio, false, knob, y);
    assert!(
        studio.source().contains("slider(0.9,0,1,0.1)"),
        "the wheel stepped the slider up:\n{}",
        studio.source()
    );
    // The rewrite moved the knob one cell right; the next wheel is aimed
    // at where the knob is NOW, as a hand would aim.
    let rows = studio.rows();
    let (knob, y) = glyph_column(&rows, '█');
    wheel(&mut studio, true, knob, y);
    assert!(
        studio.source().contains("slider(0.8,0,1,0.1)"),
        "and back down:\n{}",
        studio.source()
    );
}

/// Over the samples tab's pulse row the wheel sets the preview volume: the
/// meter row is the fader, and its label shows the decibels.
#[test]
fn the_wheel_over_the_samples_pulse_row_is_the_preview_volume() {
    let mut studio = hermetic_with_local_bank();
    studio.press(KeyCode::Home, KeyModifiers::NONE);
    studio.chord("ctrl+d");
    studio.press(KeyCode::Tab, KeyModifiers::NONE);
    studio.wait_for_catalogue();
    studio.type_text("testkick");
    studio.settle();

    // The pulse block sits at the panel's foot: the last row carrying the
    // gain label is its meter row. The reference is a right-docked column,
    // so the wheel's column comes from the label's own position - a column
    // on the left of the screen is not over the panel at all.
    let rows = studio.rows();
    let y = rows
        .iter()
        .rposition(|row| row.contains('▸') && row.contains("0.0dB"))
        .unwrap_or_else(|| panic!("no pulse meter row:\n{}", rows.join("\n")));
    let byte = rows[y].find('▸').expect("the marker is in the row");
    let x = rows[y][..byte].chars().count() as u16;
    wheel(&mut studio, true, x, y as u16);
    assert_eq!(
        studio.status(),
        "preview -1.5dB",
        "the wheel on the pulse row turned the preview down"
    );
    wheel(&mut studio, false, x, y as u16);
    assert_eq!(studio.status(), "preview 0.0dB", "and back up to unity");
}
