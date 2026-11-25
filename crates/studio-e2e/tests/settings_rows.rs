//! Check settings changes, limits, scrolling, About paging and persistence.
//! Prebakes, sources, keybinds and editor effects have separate suites.

// The engine's `silent` output refuses to open unless this process's
// allocator is the audio-callback tripwire - the callback must be provably
// allocation-free, and a global allocator can only come from a binary. See
// `crates/studio/tests/studio_live_engine.rs` for the same contract.
#[global_allocator]
static AUDIO_CALLBACK_ALLOCATOR: rustel_audio::tripwire::TripwireAlloc =
    rustel_audio::tripwire::TripwireAlloc;

use crossterm::event::{KeyCode, KeyModifiers};
use rustel_studio_e2e::{
    Hermetic, PanelKind, hermetic, hermetic_sized, hermetic_with_own_sample_cache, reopen_over,
    wav_bytes,
};
use std::time::Duration;

/// Walk the sheet's rows by label until the given one is selected - by
/// what is drawn, not by row arithmetic. Up/Down move the selection on
/// every list page (only Right/Left/Space change values), so the walk
/// itself touches nothing.
fn select_row(studio: &mut Hermetic, marker: &str) {
    for _ in 0..40 {
        if studio.rows().iter().any(|row| row.contains(marker)) {
            return;
        }
        studio.press(KeyCode::Down, KeyModifiers::NONE);
        studio.pump();
    }
    panic!("no row reads {marker:?}:\n{}", studio.rows().join("\n"));
}

/// The drawn row that carries this label - the row itself, not the
/// explanation the sheet prints underneath the list.
fn row_with(studio: &mut Hermetic, label: &str) -> String {
    studio
        .rows()
        .into_iter()
        .find(|row| row.contains(label))
        .unwrap_or_else(|| panic!("no row reads {label:?}:\n{}", studio.rows().join("\n")))
}

/// Pump until the studio has heard back about a refused sample source: the
/// refusal lands on its own turn, and a busy machine can take longer than
/// a settle, so the permissions page would not list it yet.
fn wait_for_refused_source(studio: &mut Hermetic) {
    studio.pump_until(Duration::from_secs(10), "the source is refused", |studio| {
        let status = studio.status();
        status.contains("is not allowed for samples")
            || status.contains("not an allowed sample source")
    });
}

/// Open the sheet and walk to the permissions page, wherever the sheet
/// left it - the sheet keeps its page across closes, so a second visit
/// does not start on the first one. The orchestra switch is on that page
/// from the studio's first breath, so the page always draws it.
fn open_permissions(studio: &mut Hermetic) {
    studio.chord("ctrl+o");
    studio.settle();
    for _ in 0..10 {
        if studio
            .rows()
            .iter()
            .any(|row| row.contains("native orchestras"))
        {
            return;
        }
        studio.press(KeyCode::BackTab, KeyModifiers::SHIFT);
        studio.pump();
    }
    panic!(
        "no page reads native orchestras:\n{}",
        studio.rows().join("\n")
    );
}

/// The selected row, by its marker - inside the sheet, not the header's
/// scene chips, which wear the same `▸`.
fn selected_row(studio: &mut Hermetic) -> Option<String> {
    studio
        .rows()
        .into_iter()
        .find(|row| row.contains("▸") && row.contains("│"))
}

/// The row's VALUE, as its own segment: a row with room prints its
/// explanation inline after the value, and explanations quote other
/// rungs (`header counters · advanced adds…`), so `contains` on the
/// whole row reads the prose as the setting. Segments are what the
/// sheet's own two-space gaps divide the row into.
fn value_of(studio: &mut Hermetic, label: &str) -> String {
    let row = row_with(studio, label);
    let segments: Vec<&str> = row
        .split("  ")
        .map(str::trim)
        .filter(|segment| !segment.is_empty())
        .collect();
    let at = segments
        .iter()
        .position(|segment| segment.contains(label))
        .unwrap_or_else(|| panic!("no segment carries {label:?}: {row}"));
    segments
        .get(at + 1)
        .map(|segment| segment.trim_matches('│').trim().to_owned())
        .unwrap_or_else(|| panic!("the row has no value segment: {row}"))
}


/// Open the sheet on its first page.
fn open_sheet(studio: &mut Hermetic) {
    studio.chord("ctrl+o");
    studio.settle();
    assert!(
        row_with(studio, "launch on").contains("▸"),
        "the sheet opens with its first row selected:\n{}",
        studio.rows().join("\n")
    );
}

/// Open the sheet on the appearance page, the second tab.
fn open_appearance(studio: &mut Hermetic) {
    open_sheet(studio);
    studio.press(KeyCode::Tab, KeyModifiers::NONE);
    studio.settle();
}

/// Open the sheet on the About page, the last tab.
fn open_about(studio: &mut Hermetic) {
    open_sheet(studio);
    studio.press(KeyCode::BackTab, KeyModifiers::SHIFT);
    studio.settle();
}


/// The row's percentage, as the opacity rows print it.
fn percent(row: &str) -> u32 {
    row.split_whitespace()
        .find_map(|word| {
            word.strip_suffix('%')
                .and_then(|digits| digits.parse().ok())
        })
        .unwrap_or_else(|| panic!("no percentage on row: {row}"))
}

/// A ladder steps forward, wraps to its start, and Left steps back into
/// the wrap from the other side - the metric detail's four rungs are the
/// ladder every other ladder is.
#[test]
fn ladders_step_wrap_both_ways() {
    let mut studio = hermetic();
    open_appearance(&mut studio);
    select_row(&mut studio, "▸ metric detail");

    const RUNGS: [&str; 4] = ["none", "basic", "advanced", "full"];
    let rung = |studio: &mut Hermetic| {
        let value = value_of(studio, "metric detail");
        RUNGS
            .iter()
            .find(|name| value.starts_with(**name))
            .copied()
            .unwrap_or_else(|| panic!("the row reads no rung of the ladder: {value}"))
    };
    let start = rung(&mut studio);

    let mut walked = Vec::new();
    for _ in 0..4 {
        studio.press(KeyCode::Right, KeyModifiers::NONE);
        studio.settle();
        walked.push(rung(&mut studio));
    }
    assert_eq!(
        walked
            .iter()
            .collect::<std::collections::HashSet<_>>()
            .len(),
        4,
        "four steps visited four different rungs: {walked:?}"
    );
    assert_eq!(
        *walked.last().unwrap(),
        start,
        "and the fourth wrapped back to the start"
    );

    studio.press(KeyCode::Left, KeyModifiers::NONE);
    studio.settle();
    assert_ne!(
        rung(&mut studio),
        start,
        "Left from the start steps back into the wrap"
    );
}

/// The syntax check's three modes each say what they still do - marks
/// going away could otherwise read as the score having been fixed.
#[test]
fn syntax_check_modes_each_say_what_they_still_do() {
    let mut studio = hermetic();
    open_sheet(&mut studio);
    select_row(&mut studio, "▸ syntax check");

    let expected = |value: &str| match value {
        "on update" => "syntax check on update - errors are marked where an update is refused",
        "full" => "syntax check full - errors are marked a pause after typing stops",
        _ => {
            "syntax check off - nothing is marked; the footer and the log still say why an update is refused"
        }
    };

    let start = value_of(&mut studio, "syntax check");
    for _ in 0..3 {
        studio.press(KeyCode::Right, KeyModifiers::NONE);
        studio.settle();
        let value = value_of(&mut studio, "syntax check");
        assert_eq!(
            studio.status(),
            expected(&value),
            "the mode explains itself"
        );
    }
    assert_eq!(
        value_of(&mut studio, "syntax check"),
        start,
        "three rungs wrapped the ladder back to its start"
    );
}

/// The opacity rows clamp at 100%, and touching one claims it: the
/// theme stops overwriting it, so what the sheet shows is what a reopen
/// shows - together with a plain ladder row that rides the same prefs
/// file.
#[test]
fn opacity_rows_clamp_and_are_claimed_across_a_reopen() {
    let mut studio = hermetic();
    open_appearance(&mut studio);
    select_row(&mut studio, "▸ ui opacity");
    let opacity = |studio: &mut Hermetic| percent(&value_of(studio, "ui opacity"));

    for _ in 0..40 {
        if opacity(&mut studio) == 100 {
            break;
        }
        studio.press(KeyCode::Right, KeyModifiers::NONE);
        studio.settle();
    }
    assert_eq!(
        opacity(&mut studio),
        100,
        "the ladder reaches full: {}",
        row_with(&mut studio, "ui opacity")
    );
    studio.press(KeyCode::Right, KeyModifiers::NONE);
    studio.settle();
    assert_eq!(
        opacity(&mut studio),
        100,
        "and stops there: {}",
        row_with(&mut studio, "ui opacity")
    );
    studio.press(KeyCode::Left, KeyModifiers::NONE);
    studio.settle();
    let left_with = opacity(&mut studio);
    assert!(
        left_with < 100,
        "Left stepped back off the ceiling: {left_with}%"
    );

    select_row(&mut studio, "▸ caret shape");
    studio.press(KeyCode::Right, KeyModifiers::NONE);
    studio.press(KeyCode::Right, KeyModifiers::NONE);
    studio.settle();
    assert!(
        row_with(&mut studio, "caret shape").contains("steady block"),
        "setup: a second row was changed"
    );

    studio.press(KeyCode::Esc, KeyModifiers::NONE);
    studio.settle();
    // Prefs are written on a debounce: the wait is what makes the quit
    // honest.
    std::thread::sleep(std::time::Duration::from_millis(700));
    studio.pump();
    studio.quit();
    let mut studio = reopen_over(studio);

    open_appearance(&mut studio);
    select_row(&mut studio, "▸ ui opacity");
    assert_eq!(
        percent(&value_of(&mut studio, "ui opacity")),
        left_with,
        "the claimed opacity survived the reopen: {}",
        row_with(&mut studio, "ui opacity")
    );
    select_row(&mut studio, "▸ caret shape");
    assert!(
        row_with(&mut studio, "caret shape").contains("steady block"),
        "and so did the caret: {}",
        row_with(&mut studio, "caret shape")
    );
}

/// A beat-line-only change is the reader's claim on its own: it survives
/// a reopen without an opacity having to change beside it.
#[test]
fn a_beat_line_only_change_survives_a_reopen() {
    let mut studio = hermetic();
    open_appearance(&mut studio);
    select_row(&mut studio, "▸ beat line");
    let before = row_with(&mut studio, "beat line");
    for _ in 0..10 {
        studio.press(KeyCode::Right, KeyModifiers::NONE);
        studio.settle();
        if row_with(&mut studio, "beat line") != before {
            break;
        }
    }
    let changed = row_with(&mut studio, "beat line");
    assert_ne!(changed, before, "setup: the beat line moved from {before}");

    studio.press(KeyCode::Esc, KeyModifiers::NONE);
    studio.settle();
    // Prefs are written on a debounce: the wait is what makes the quit
    // honest.
    std::thread::sleep(std::time::Duration::from_millis(700));
    studio.pump();
    studio.quit();
    let mut studio = reopen_over(studio);

    open_appearance(&mut studio);
    select_row(&mut studio, "▸ beat line");
    assert_eq!(
        row_with(&mut studio, "beat line"),
        changed,
        "the beat-line-only change survived the reopen"
    );
}

/// The folder rows are doors, not ladders: Enter replaces the sheet with
/// the picker, and the picker's Esc comes back to the sheet - a settings
/// prompt remembers where it was opened from.
#[test]
fn folder_rows_open_their_picker_and_esc_returns_to_the_sheet() {
    let mut studio = hermetic();
    open_sheet(&mut studio);

    select_row(&mut studio, "▸ sets folder");
    studio.press(KeyCode::Enter, KeyModifiers::NONE);
    studio.settle();
    assert_eq!(
        studio.status(),
        "sets folder - type a path, or Tab to browse and Enter on . · Esc back"
    );
    assert!(
        studio
            .rows()
            .iter()
            .any(|row| row.contains("keeps new sets there")),
        "the picker is up with its own promise:\n{}",
        studio.rows().join("\n")
    );

    studio.press(KeyCode::Esc, KeyModifiers::NONE);
    studio.settle();
    assert_eq!(studio.status(), "set prompt closed");
    assert_eq!(
        studio.focus(),
        Some(PanelKind::Settings),
        "the Esc came back to the sheet that opened the picker"
    );
    // The sheet keeps its scroll where the picker left it, and the first
    // page now scrolls, so the row still standing is the one selected, not
    // the sheet's first row.
    assert!(
        selected_row(&mut studio).is_some_and(|row| row.contains("sets folder")),
        "and the sheet is still standing:\n{}",
        studio.rows().join("\n")
    );

    select_row(&mut studio, "▸ recordings folder");
    studio.press(KeyCode::Enter, KeyModifiers::NONE);
    studio.settle();
    assert_eq!(
        studio.status(),
        "recordings folder - type a path, or Tab to browse and Enter on . · Esc back"
    );
    studio.press(KeyCode::Esc, KeyModifiers::NONE);
    studio.settle();
    assert_eq!(studio.status(), "set prompt closed");
}

/// The About page reports on the current terminal: its name (`rustel-e2e`
/// in the hermetic fixture), the drawing tier, the shortcut profile, the
/// feature checklist and the capability report. It prints the paging keys
/// where the page can scroll.
#[test]
fn about_page_reads_the_terminal_it_is_running_in() {
    let mut studio = hermetic();
    open_about(&mut studio);

    let rows = studio.rows();
    let screen = rows.join("\n");
    for expected in [
        "Terminal: rustel-e2e",
        "Rendering: Automatic (Cells)",
        "Shortcut profile: automatic (rustel-e2e)",
        "Terminal features",
        "Full colour",
        "PgUp/PgDn scroll",
        "build",
    ] {
        assert!(
            screen.contains(expected),
            "the About page reads {expected:?}:\n{screen}"
        );
    }

    // Tab is the sheet's own key on every page: from the last tab it
    // wraps to the first.
    studio.press(KeyCode::Tab, KeyModifiers::NONE);
    studio.settle();
    assert!(
        row_with(&mut studio, "launch on").contains("▸"),
        "Tab wrapped the strip back to the settings page:\n{}",
        studio.rows().join("\n")
    );

    studio.press(KeyCode::Esc, KeyModifiers::NONE);
    studio.settle();
    assert_eq!(studio.focus(), None, "and Esc closed the sheet");
}

#[test]
fn settings_and_appearance_keep_their_place_until_studio_restarts() {
    let mut studio = hermetic_sized(120, 24);
    open_sheet(&mut studio);
    select_row(&mut studio, "▸ syntax check");
    let selection = |studio: &mut Hermetic| {
        studio
            .rows()
            .into_iter()
            .enumerate()
            .find(|(_, row)| row.contains('▸') && row.contains('│'))
            .expect("the selected settings row is visible")
    };
    let settings_position = selection(&mut studio);
    studio.press(KeyCode::Esc, KeyModifiers::NONE);
    studio.chord("ctrl+o");
    studio.settle();
    assert_eq!(selection(&mut studio), settings_position);

    studio.press(KeyCode::Tab, KeyModifiers::NONE);
    select_row(&mut studio, "▸ frame rate");
    let appearance_position = selection(&mut studio);
    studio.chord("ctrl+o");
    studio.chord("ctrl+o");
    studio.settle();
    assert_eq!(selection(&mut studio), appearance_position);

    studio.press(KeyCode::BackTab, KeyModifiers::SHIFT);
    studio.settle();
    assert_eq!(selection(&mut studio), settings_position);
    studio.press(KeyCode::Tab, KeyModifiers::NONE);
    studio.settle();
    assert_eq!(selection(&mut studio), appearance_position);

    let mut studio = reopen_over(studio);
    open_sheet(&mut studio);
}

/// A score that imports a script from an origin nobody trusted is refused,
/// and the footer names the permissions page. The page offers the origin as
/// a switch: Enter trusts it and the studio re-runs the refused update by
/// itself, Space turns the trust off where the row stands, and the next
/// studio still trusts it.
#[test]
fn a_refused_import_origin_is_trusted_with_one_key_and_kept() {
    let mut studio = hermetic();
    studio.set_score("await import('https://scripts.invalid/helpers.js')\n$: s(\"bd\")");
    studio.chord("ctrl+s");
    studio.settle();
    assert_eq!(
        studio.errors().as_deref(),
        Some(
            "https://scripts.invalid is not trusted - Settings → Permissions lets its scripts run"
        ),
    );

    open_permissions(&mut studio);
    select_row(&mut studio, "▸ https://scripts.invalid");
    let row = row_with(&mut studio, "https://scripts.invalid");
    assert!(row.contains("scripts  off"), "{row}");
    studio.press(KeyCode::Enter, KeyModifiers::NONE);
    studio.settle();
    let row = row_with(&mut studio, "https://scripts.invalid");
    assert!(row.contains("scripts  on"), "{row}");
    // The trust re-ran the refused update by itself, so the footer can only
    // hold the fetch of the newly trusted origin - never the refusal.
    if let Some(error) = studio.errors() {
        assert!(
            !error.contains("is not trusted"),
            "the refusal left with the trust: {error}"
        );
    }
    // Space takes the trust back, and the row stays to give it again.
    studio.press(KeyCode::Char(' '), KeyModifiers::NONE);
    studio.settle();
    assert!(row_with(&mut studio, "https://scripts.invalid").contains("scripts  off"));
    studio.press(KeyCode::Char(' '), KeyModifiers::NONE);
    studio.settle();
    assert!(row_with(&mut studio, "https://scripts.invalid").contains("scripts  on"));

    studio.press(KeyCode::Esc, KeyModifiers::NONE);
    studio.settle();
    // Prefs are written on a debounce.
    std::thread::sleep(std::time::Duration::from_millis(700));
    studio.pump();
    studio.quit();
    let mut studio = reopen_over(studio);
    open_permissions(&mut studio);
    select_row(&mut studio, "▸ https://scripts.invalid");
    // `d` takes the origin off the page; the orchestra switch stays. The
    // status line still names the origin - it reports the removal - so the
    // check reads the row shape: a row is its marker, then the origin.
    studio.press(KeyCode::Char('d'), KeyModifiers::NONE);
    studio.settle();
    assert!(
        studio
            .rows()
            .into_iter()
            .all(|row| !row.contains("▸ https://scripts.invalid")
                && !row.contains("  https://scripts.invalid")),
        "no row carries the origin as its label:\n{}",
        studio.rows().join("\n")
    );
    row_with(&mut studio, "native orchestras");
}

/// A score whose `samples()` names a server that no source allows plays on
/// without the bank, and the folder of the refused file lands on the
/// permissions page. Enter allows it, and the next studio still allows it.
/// Before that, the page lists no source: the built-in ones have no row.
#[test]
fn a_refused_sample_folder_is_allowed_with_one_key_and_kept() {
    let mut studio = hermetic();
    open_permissions(&mut studio);
    assert!(
        studio
            .rows()
            .iter()
            .all(|row| !row.contains("samples  ") && !row.contains("githubusercontent")),
        "no source row yet:\n{}",
        studio.rows().join("\n")
    );
    studio.press(KeyCode::Esc, KeyModifiers::NONE);
    studio.settle();

    studio.set_score("samples('http://kit.invalid/kit.json')\n$: s(\"bd\")");
    studio.chord("ctrl+s");
    wait_for_refused_source(&mut studio);
    open_permissions(&mut studio);
    select_row(&mut studio, "▸ http://kit.invalid");
    let refused = row_with(&mut studio, "http://kit.invalid");
    assert!(refused.contains("samples  off"), "{refused}");
    studio.press(KeyCode::Enter, KeyModifiers::NONE);
    // Read before the re-run it starts: the fetch of a host that does not
    // exist then says so on the same line.
    assert!(
        studio.status().contains("http://kit.invalid allowed"),
        "{}",
        studio.status()
    );
    studio.settle();
    let allowed = row_with(&mut studio, "http://kit.invalid");
    assert!(allowed.contains("samples  on"), "{allowed}");

    studio.press(KeyCode::Esc, KeyModifiers::NONE);
    studio.settle();
    // Prefs are written on a debounce.
    std::thread::sleep(std::time::Duration::from_millis(700));
    studio.pump();
    studio.quit();
    let mut studio = reopen_over(studio);
    open_permissions(&mut studio);
    assert!(row_with(&mut studio, "http://kit.invalid").contains("samples  on"));
}

/// A sample map on this machine, served until the test ends: `/map.json`
/// names `sn`, and any other path is a short silent file.
fn serve_a_map() -> String {
    use std::io::{BufRead, BufReader, Write};
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind server");
    let origin = format!("http://{}", listener.local_addr().expect("address"));
    let map = format!(r#"{{"_base":"{origin}/","sn":"sn.wav"}}"#);
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let mut reader = BufReader::new(stream);
            let mut line = String::new();
            let _ = reader.read_line(&mut line);
            let mut header = String::new();
            while reader.read_line(&mut header).is_ok_and(|read| read > 2) {
                header.clear();
            }
            let body = if line.contains("/map.json") {
                map.clone().into_bytes()
            } else {
                wav_bytes()
            };
            let mut stream = reader.into_inner();
            let _ = write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            let _ = stream.write_all(&body);
        }
    });
    origin
}

/// A source turned off and on again brings its names back: the update
/// refused for a name of its map while it was off runs again once it is
/// on.
#[test]
fn a_source_turned_off_and_on_again_reruns_the_refused_update() {
    let origin = serve_a_map();
    let mut studio = hermetic_with_own_sample_cache();
    studio.set_score(&format!(
        "samples('{origin}/map.json')\n$: s(\"sn\").gain(0.03)"
    ));
    studio.chord("ctrl+s");
    studio.settle();
    open_permissions(&mut studio);
    // The row comes once the engine has refused the map.
    studio.pump_until(Duration::from_secs(10), "the source is refused", |studio| {
        studio.rows().iter().any(|row| row.contains(&origin))
    });
    select_row(&mut studio, &format!("▸ {origin}"));
    studio.press(KeyCode::Enter, KeyModifiers::NONE);
    studio.settle();
    studio.press(KeyCode::Esc, KeyModifiers::NONE);
    studio.settle();
    // The map is fetched once the source is allowed. An update while it
    // is on its way may be refused, and the next one plays. A start still
    // waiting for its sounds is replaced and plays from the top.
    studio.set_score(&format!(
        "samples('{origin}/map.json')\n$: s(\"sn\").gain(0.02)"
    ));
    studio.chord("ctrl+s");
    // One retry a second at most: a press each poll stacks updates.
    let mut pressed = std::time::Instant::now();
    studio.pump_until(Duration::from_secs(20), "the score plays sn", |studio| {
        if studio.status().contains("refused") && pressed.elapsed() > Duration::from_secs(1) {
            studio.chord("ctrl+s");
            pressed = std::time::Instant::now();
        }
        let status = studio.status();
        status.starts_with("updated") || status.starts_with("playing")
    });
    open_permissions(&mut studio);
    select_row(&mut studio, &format!("▸ {origin}"));

    studio.press(KeyCode::Char(' '), KeyModifiers::NONE);
    // The row turns off once the turn that takes the grant back has run.
    studio.pump_until(Duration::from_secs(5), "the source turns off", |studio| {
        studio
            .rows()
            .iter()
            .any(|row| row.contains(&origin) && row.contains("samples  off"))
    });
    studio.press(KeyCode::Esc, KeyModifiers::NONE);
    studio.settle();
    studio.set_score(&format!(
        "samples('{origin}/map.json')\n$: s(\"sn sn\").gain(0.03)"
    ));
    // Only the check's refusal is remembered for a permission to re-run. An
    // update sent before the engine has taken the grant back installs, then
    // rolls back with nothing to re-run, so press again until the check
    // refuses it: one press a second at most, as above.
    let refusals = |studio: &rustel_studio_e2e::Hermetic| {
        studio
            .log_texts()
            .iter()
            .filter(|line| line.contains("refused - ") && line.contains("unknown sound \"sn\""))
            .count()
    };
    let before = refusals(&studio);
    studio.chord("ctrl+s");
    let mut pressed = std::time::Instant::now();
    studio.pump_until(
        Duration::from_secs(15),
        "the check refuses the update",
        |studio| {
            if refusals(studio) > before {
                return true;
            }
            if pressed.elapsed() > Duration::from_secs(1) {
                studio.chord("ctrl+s");
                pressed = std::time::Instant::now();
            }
            false
        },
    );

    open_permissions(&mut studio);
    select_row(&mut studio, &format!("▸ {origin}"));
    studio.press(KeyCode::Enter, KeyModifiers::NONE);
    studio.settle();
    studio.press(KeyCode::Esc, KeyModifiers::NONE);
    // The refused update runs, and the check reads the names the map
    // brought again: no finding is left standing about the sound.
    studio.pump_until(
        Duration::from_secs(20),
        "the refused update runs",
        |studio| {
            let status = studio.status();
            !status.contains("refused")
                && !status.contains("updating the score")
                && studio
                    .rows()
                    .iter()
                    .all(|row| !row.contains("unknown sound"))
        },
    );
}

/// `e` opens a refused folder in a prompt with its text. Cut down to the
/// domain, the row allows all of it.
#[test]
fn a_refused_folder_is_cut_down_to_its_domain_with_e() {
    let mut studio = hermetic();
    studio.set_score("samples('https://kit.invalid/kits/808/kit.json')\n$: s(\"bd\")");
    studio.chord("ctrl+s");
    wait_for_refused_source(&mut studio);
    open_permissions(&mut studio);
    select_row(&mut studio, "▸ kit.invalid/kits/808/");
    studio.press(KeyCode::Char('e'), KeyModifiers::NONE);
    studio.settle();
    assert!(
        studio.status().starts_with("edit a source"),
        "{}",
        studio.status()
    );
    for _ in "/kits/808/".chars() {
        studio.press(KeyCode::Backspace, KeyModifiers::NONE);
    }
    studio.press(KeyCode::Enter, KeyModifiers::NONE);
    studio.settle();
    let domain = row_with(&mut studio, "kit.invalid");
    assert!(
        domain.contains("▸ kit.invalid ") && domain.contains("samples  on"),
        "the domain takes the row's place, under the cursor: {domain}"
    );
    assert!(
        studio
            .rows()
            .iter()
            .all(|row| !row.contains("kit.invalid/kits/808/")),
        "{}",
        studio.rows().join("\n")
    );
}

/// An orchestra score plays with no flag and no keypress: an installed
/// library is the studio's opt-in. Space takes the switch off for the run,
/// the next save of an edited score stops with the refusal that names the
/// page, Enter puts the switch back and the studio re-runs the refused
/// update by itself, and the next studio starts with it on again.
#[test]
fn the_orchestra_switch_starts_on_and_space_takes_it_back_for_a_run() {
    let mut studio = hermetic();
    studio.set_score("loadCSound`instr voice\nendin`\n$: note(\"c3\").csound(\"voice\")");
    studio.chord("ctrl+s");
    studio.settle();
    // No permission refusal. A machine without the native library reports
    // the library's own absence, never the permission's.
    match studio.errors() {
        None => {}
        Some(error) => assert!(error.contains("Csound 6 is not available"), "{error}"),
    }

    open_permissions(&mut studio);
    assert!(row_with(&mut studio, "native orchestras").contains("  on"));

    select_row(&mut studio, "▸ native orchestras");
    studio.press(KeyCode::Char(' '), KeyModifiers::NONE);
    studio.settle();
    assert!(row_with(&mut studio, "native orchestras").contains("  off"));

    studio.press(KeyCode::Esc, KeyModifiers::NONE);
    studio.settle();
    // An edit makes the save a new generation - the first one is still
    // sounding, whatever the machine's library said about it - so the press
    // must not land as a repeat of it.
    studio.set_score("loadCSound`instr voice\nendin`\n$: note(\"d3\").csound(\"voice\")");
    studio.chord("ctrl+s");
    studio.settle();
    assert_eq!(
        studio.errors().as_deref(),
        Some("Csound is not permitted - Settings → Permissions lets native orchestras run"),
    );

    open_permissions(&mut studio);
    select_row(&mut studio, "▸ native orchestras");
    studio.press(KeyCode::Enter, KeyModifiers::NONE);
    studio.settle();
    assert!(row_with(&mut studio, "native orchestras").contains("  on"));
    // The permission re-ran the refused update by itself. A machine with
    // the native library installs it and says so; a machine without one
    // reports the library's own absence. The refusal itself is gone either
    // way.
    match studio.errors() {
        None => assert!(
            studio.status().contains("playing from the top")
                || studio.status().starts_with("updated"),
            "the re-run installed: {}",
            studio.status()
        ),
        Some(error) => assert!(error.contains("Csound 6 is not available"), "{error}"),
    }

    studio.quit();
    let mut studio = reopen_over(studio);
    open_permissions(&mut studio);
    assert!(row_with(&mut studio, "native orchestras").contains("  on"));
}

/// Tab walks the strip's pages in order and wraps - each page
/// recognised by a row only it draws.
#[test]
fn tab_cycles_the_pages_and_wraps() {
    let mut studio = hermetic();
    open_sheet(&mut studio);

    for expected in [
        "zen mode",             // appearance
        "not assigned",         // mapping: the slot boxes
        "automatic",            // keybinds: the terminal-profile row
        "fetch imports",        // sources
        "add folder",           // vst
        "OSC / SuperDirt",      // reference
        "native orchestras",    // trust: the switch is on from the start
        "Terminal: rustel-e2e", // about
        "launch on",            // and back to general
    ] {
        studio.press(KeyCode::Tab, KeyModifiers::NONE);
        studio.settle();
        let rows = studio.rows().join("\n");
        assert!(
            rows.contains(expected),
            "the next tab reads {expected:?}:\n{rows}"
        );
    }
}
