//! Open settings with Ctrl-O or Ctrl-Shift-P. Check persisted settings,
//! prebake validation and saved set setup.

// The engine's `silent` output refuses to open unless this process's
// allocator is the audio-callback tripwire - the callback must be provably
// allocation-free, and a global allocator can only come from a binary. See
// `crates/studio/tests/studio_live_engine.rs` for the same contract.
#[global_allocator]
static AUDIO_CALLBACK_ALLOCATOR: rustel_audio::tripwire::TripwireAlloc =
    rustel_audio::tripwire::TripwireAlloc;

use crossterm::event::{KeyCode, KeyModifiers};
use rustel_studio_e2e::{PanelKind, hermetic, reopen_over, row_containing};

/// The biggest-sound ceiling is a setting now, not a constant a rebuild
/// changes: the general page's row names the rung and roughly how long
/// a take it holds, and ←/→ walk the ladder. What breaks if these fail:
/// a musician with a long take has no way to raise the ceiling, or the
/// row promises a size the player then refuses to honour.
#[test]
fn the_biggest_sound_is_a_ladder_a_setting_walks() {
    let mut studio = hermetic();
    studio.chord("ctrl+o");
    row_containing(&studio.rows(), "Tab pages");

    // The memory rows live at the end of the general page. The walk reads
    // the sheet's own selection marker: Right acts on the row the sheet
    // says is selected, not on any row that happens to be visible.
    for _ in 0..30 {
        if studio
            .rows()
            .iter()
            .any(|row| row.contains("▸ biggest sound"))
        {
            break;
        }
        studio.press(KeyCode::Down, KeyModifiers::NONE);
    }
    assert!(
        studio
            .rows()
            .iter()
            .any(|row| row.contains("▸ biggest sound")),
        "the walk selected the biggest-sound row:\n{}",
        studio.rows().join("\n")
    );

    // The row says the rung and its minutes - a musician counts in
    // seconds of take, not in mebibytes.
    let read = |studio: &mut rustel_studio_e2e::Hermetic| {
        studio
            .rows()
            .iter()
            .find(|row| row.contains("biggest sound"))
            .expect("the row is on the general page")
            .clone()
    };
    let row = read(&mut studio);
    assert!(
        row.contains("256 MiB"),
        "the default rung is Roomy: {row:?}"
    );
    assert!(row.contains("min"), "the rung says what it holds: {row:?}");

    // → climbs the ladder, ↓ comes back down: the value follows the keys.
    studio.press(KeyCode::Right, KeyModifiers::NONE);
    assert!(
        read(&mut studio).contains("512 MiB"),
        "→ raised the ceiling"
    );
    studio.press(KeyCode::Right, KeyModifiers::NONE);
    assert!(
        read(&mut studio).contains("1 GiB"),
        "the ladder tops out at the player's own hard maximum"
    );
    studio.press(KeyCode::Left, KeyModifiers::NONE);
    assert!(
        read(&mut studio).contains("512 MiB"),
        "← came back down a rung"
    );

    studio.press(KeyCode::Esc, KeyModifiers::NONE);
}

/// Walk the sheet's rows by label until the given one is selected -
/// by what is drawn, not by row arithmetic, so the suite holds under
/// every feature set's row list.
fn select_row(studio: &mut rustel_studio_e2e::Hermetic, marker: &str) {
    for _ in 0..30 {
        if studio.rows().iter().any(|row| row.contains(marker)) {
            return;
        }
        studio.press(KeyCode::Down, KeyModifiers::NONE);
    }
    panic!("no row reads {marker:?}:\n{}", studio.rows().join("\n"));
}

/// The clock rows are on the devices panel's MIDI tab, below the ports they
/// choose between. On a host with no MIDI ports they read "none", and a
/// step says to tick a port's box. The test names no real port, so it holds
/// on any host: it asserts the rows' place, their order and the no-ports
/// arithmetic.
#[test]
fn the_clock_rows_say_what_they_do_and_honour_no_ports() {
    let mut studio = hermetic();

    // Not on the settings sheet any more.
    studio.chord("ctrl+o");
    assert_eq!(studio.focus(), Some(PanelKind::Settings));
    assert!(
        !studio.rows().iter().any(|row| row.contains("clock out")),
        "the clock rows have left the sheet"
    );
    studio.press(KeyCode::Esc, KeyModifiers::NONE);

    // On the devices panel's MIDI tab.
    studio.chord("ctrl+p");
    studio.press(KeyCode::Tab, KeyModifiers::NONE);
    studio.press(KeyCode::Tab, KeyModifiers::NONE);
    let rows = studio.rows();
    let input = row_containing(&rows, "clock in");
    let out = row_containing(&rows, "clock out");
    assert!(input < out, "clock in, then clock out");
    assert!(rows[input].contains("< none >"), "{}", rows[input]);
    assert!(rows[out].contains("< none >"), "{}", rows[out]);

    // A hermetic host lists no ports, and says so where the column names go.
    row_containing(&rows, "no MIDI ports found");

    // Stepping with nothing ticked keeps the choice at none, says where a
    // port would come from, and leaves the panel open.
    studio.press(KeyCode::Right, KeyModifiers::NONE);
    let rows = studio.rows();
    assert!(
        rows.iter()
            .any(|row| row.contains("clock in") && row.contains("< none >")),
        "no ports, no choice: the row stays none\n{}",
        rows.join("\n")
    );
    assert!(studio.status().contains("tick"), "{}", studio.status());
    assert_eq!(studio.focus(), Some(PanelKind::Devices));
}

/// A prebake the checker refuses is not applied, but is still kept - the
/// text stays in the tab, and the store is told only on the way out.
#[test]
fn a_refused_prebake_is_not_applied_but_still_kept() {
    let mut studio = hermetic();

    studio.chord("ctrl+o");
    select_row(&mut studio, "▸ local prebake");
    studio.press(KeyCode::Enter, KeyModifiers::NONE);
    // Setup has rules: setcps belongs to a score, not to setup.
    studio.set_score("setcps(3)");
    studio.chord("ctrl+s");

    assert!(
        studio.status().starts_with("refused - "),
        "the refusal says why: {}",
        studio.status()
    );
    assert!(
        studio.status().contains("the setup was not applied"),
        "{}",
        studio.status()
    );
    assert!(studio.is_dirty(), "refused text is not kept as stored");

    // And the tab survives with the refused text in it.
    assert_eq!(
        studio.scene_names().last(),
        Some(&"prebake (local)".to_owned())
    );
    assert!(studio.score().contains("setcps(3)"));
}

/// A helper that only wraps a refused call is setup the checker takes:
/// community prebakes hand scores a `bpm` helper that wraps `setcpm`, and
/// defining it runs nothing - the call runs when a score calls the helper.
/// Applying it used to refuse the whole prebake over the wrapper, and with
/// it every helper the set needed.
#[test]
fn a_prebake_helper_that_wraps_a_refused_call_applies() {
    let mut studio = hermetic();

    studio.chord("ctrl+o");
    select_row(&mut studio, "▸ local prebake");
    studio.press(KeyCode::Enter, KeyModifiers::NONE);
    studio.set_score("window.bpm = b => setcpm(b/4)");
    studio.chord("ctrl+s");
    studio.settle();

    assert_eq!(studio.status(), "prebake (local) applied");
}

/// Writing over a name the engine owns is a hint in the footer, never a
/// refusal: the paste is kept, runs, and the footer says which native name
/// it replaced.
#[test]
fn replacing_an_engine_name_says_so_without_refusing() {
    let mut studio = hermetic();

    studio.chord("ctrl+o");
    select_row(&mut studio, "▸ local prebake");
    studio.press(KeyCode::Enter, KeyModifiers::NONE);
    studio.set_score("window.note = (p) => p");
    studio.chord("ctrl+s");
    studio.settle();

    assert_eq!(studio.status(), "prebake (local) applied");
    assert!(
        studio
            .rows()
            .iter()
            .any(|row| row.contains("hint: ") && row.contains("`note`")),
        "the footer names what the paste replaced:\n{}",
        studio.rows().join("\n")
    );
}

/// A blank prebake applies as nothing: the store is emptied, and the
/// status says there was nothing to run.
#[test]
fn an_emptied_prebake_applies_as_nothing() {
    let mut studio = hermetic();

    studio.chord("ctrl+o");
    select_row(&mut studio, "▸ local prebake");
    studio.press(KeyCode::Enter, KeyModifiers::NONE);
    studio.set_score("");
    studio.chord("ctrl+s");
    studio.settle();

    assert_eq!(studio.status(), "prebake (local) is empty - nothing to run");
}

/// The local prebake is kept with the set: a studio reopened over the same
/// folder still has it, and the settings sheet's row says how many lines.
#[test]
fn the_local_prebake_survives_a_reopen() {
    let mut studio = hermetic();

    studio.chord("ctrl+o");
    select_row(&mut studio, "▸ local prebake");
    studio.press(KeyCode::Enter, KeyModifiers::NONE);
    studio.set_score("globalThis.riff = () => note(\"c e g\")");
    studio.chord("ctrl+s");
    studio.settle();
    assert_eq!(studio.status(), "prebake (local) applied");

    let mut reopened = reopen_over(studio);
    assert_eq!(
        reopened.status(),
        "prebake (local) applied",
        "a stored prebake runs again over the same set"
    );
    reopened.chord("ctrl+o");
    select_row(&mut reopened, "▸ local prebake");
    let row = row_containing(&reopened.rows(), "▸ local prebake");
    assert!(
        reopened.rows()[row].contains("1 line"),
        "the sheet counts the stored lines: {}",
        reopened.rows()[row]
    );
}
