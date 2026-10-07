//! Check adding, toggling, refetching, aliasing, editing and removing sources,
//! plus confirmation before clearing caches. settings_prebake covers shipped
//! packs; drop_import covers pasted paths.

// The engine's `silent` output refuses to open unless this process's
// allocator is the audio-callback tripwire - the callback must be provably
// allocation-free, and a global allocator can only come from a binary. See
// `crates/studio/tests/studio_live_engine.rs` for the same contract.
#[global_allocator]
static AUDIO_CALLBACK_ALLOCATOR: rustel_audio::tripwire::TripwireAlloc =
    rustel_audio::tripwire::TripwireAlloc;

use std::path::{Path, PathBuf};

use crossterm::event::{KeyCode, KeyModifiers};
use rustel_studio_e2e::{
    Hermetic, hermetic, hermetic_with_own_sample_cache, reopen_over, row_containing, wav_bytes,
};

/// A folder of audio the way an import wants it: one bank folder with
/// one real WAV in it, unique per tag, outside every fixture directory.
fn audio_folder(tag: &str) -> PathBuf {
    audio_folder_with_bank(tag, "pack")
}

/// [`audio_folder`], with the bank folder - the bank's name once
/// imported - named.
fn audio_folder_with_bank(tag: &str, bank: &str) -> PathBuf {
    let at = std::env::temp_dir().join(format!("rustel-sources-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&at);
    let bank = at.join(bank);
    std::fs::create_dir_all(&bank).expect("bank folder");
    std::fs::write(bank.join("hit.wav"), wav_bytes()).expect("sample");
    at
}

fn cleanup(at: &Path) {
    let _ = std::fs::remove_dir_all(at);
}

/// Walk the page's rows until the given one is selected.
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

/// Open the sheet on its Sources page - the fifth of six tabs.
fn open_sources(studio: &mut Hermetic) {
    studio.chord("ctrl+o");
    for _ in 0..4 {
        studio.press(KeyCode::Tab, KeyModifiers::NONE);
    }
    studio.settle();
    assert!(
        studio
            .rows()
            .iter()
            .any(|row| row.contains("fetch imports")),
        "the Sources page opens on its cache controls:\n{}",
        studio.rows().join("\n")
    );
}

/// How long a local import may take to walk its folder.
const IMPORT_BUDGET: std::time::Duration = std::time::Duration::from_secs(5);

/// A local import lands as `fetching its list of sounds` and settles, on
/// a later frame, to the receipt - bounded like every asynchronous wait
/// in the suite.
fn wait_for_import(studio: &mut Hermetic) {
    studio.pump_until(IMPORT_BUDGET, "the import settled", |studio| {
        studio.status().contains("sound(s) imported")
    });
}

/// Import a folder by dropping it on the studio - the sheet lands on
/// Sources with the row already listed - and leave the selection on the
/// imported row.
fn import_folder(studio: &mut Hermetic, folder: &Path) {
    studio.paste(&folder.display().to_string());
    studio.settle();
    wait_for_import(studio);
    let name = folder.file_name().unwrap().to_string_lossy().into_owned();
    assert_eq!(
        studio.focus(),
        Some(rustel_studio_e2e::PanelKind::Settings),
        "setup: the import landed on the sheet"
    );
    select_row(studio, &format!("▸ {name}"));
}

/// `a` opens the import picker over the page, a pasted folder path imports
/// through it, and `d` removes the row from the list.
#[test]
fn a_source_added_from_the_sheet_lists_and_can_be_removed() {
    let mut studio = hermetic();
    let folder = audio_folder("add");
    open_sources(&mut studio);

    studio.press(KeyCode::Char('a'), KeyModifiers::NONE);
    studio.settle();
    assert_eq!(
        studio.status(),
        "import samples - a folder, a URL, or github:user/repo · Enter imports · Esc back"
    );

    studio.paste(&folder.display().to_string());
    studio.press(KeyCode::Enter, KeyModifiers::NONE);
    studio.settle();
    wait_for_import(&mut studio);
    let rows = studio.rows();
    let name = folder.file_name().unwrap().to_string_lossy();
    assert!(
        rows.iter()
            .any(|row| row.contains(name.as_ref()) && row.contains("(local)")),
        "the added folder is listed as a local source:\n{}",
        rows.join("\n")
    );

    select_row(&mut studio, &format!("▸ {name}"));
    studio.press(KeyCode::Char('d'), KeyModifiers::NONE);
    studio.settle();
    assert_eq!(studio.status(), format!("{name} removed"));
    // The status line itself names what was removed - the row that must
    // be gone is the list's own: name and local-ness together.
    let rows = studio.rows();
    assert!(
        !rows
            .iter()
            .any(|row| row.contains(name.as_ref()) && row.contains("(local)")),
        "and the row is off the list:\n{}",
        rows.join("\n")
    );

    studio.press(KeyCode::Esc, KeyModifiers::NONE);
    studio.settle();
    cleanup(&folder);
}

/// A source whose folder is gone when the studio starts - what a renamed
/// set leaves behind, or a drive that is unplugged - stays on the page and
/// reads missing. It raises no warning badge, and the log says so as
/// information, with where to remove it.
#[test]
fn a_source_whose_folder_is_gone_at_start_reads_missing_without_a_warning() {
    let mut studio = hermetic();
    let folder = audio_folder("gone-at-start");
    let name = folder.file_name().unwrap().to_string_lossy().into_owned();
    import_folder(&mut studio, &folder);
    // The source is a preference, written on a debounce.
    std::thread::sleep(std::time::Duration::from_millis(700));
    studio.pump();
    studio.quit();
    cleanup(&folder);

    let mut studio = reopen_over(studio);
    open_sources(&mut studio);
    studio.pump_until(IMPORT_BUDGET, "the row reads missing", |studio| {
        studio
            .rows()
            .iter()
            .any(|row| row.contains(&name) && row.contains("missing"))
    });
    studio.press(KeyCode::Esc, KeyModifiers::NONE);
    studio.settle();
    assert!(
        !studio.rows().iter().any(|row| row.contains('⚠')),
        "a missing folder raises no warning badge:\n{}",
        studio.rows().join("\n")
    );

    studio.press(KeyCode::F(9), KeyModifiers::NONE);
    let rows = studio.rows();
    let at = row_containing(&rows, "info  samples");
    let entry = rows[at..]
        .iter()
        .filter_map(|row| row.split('│').nth(1))
        .map(str::trim)
        .collect::<String>();
    // The panel wraps the entry and drops the space at each break, and the
    // break position depends on the path length. Compare without whitespace.
    let unwrapped = entry.split_whitespace().collect::<String>();
    assert!(
        unwrapped.contains(&name) && unwrapped.contains("missing-Settings→Samples"),
        "said as information: {entry}"
    );
}

/// Space is the source's own switch - off says off, on says on - and
/// Enter on a source that is off refuses to fetch it rather than
/// fetching something the player just silenced.
#[test]
fn space_turns_a_source_off_and_enter_refuses_to_fetch_it() {
    let mut studio = hermetic();
    let folder = audio_folder("toggle");
    let name = folder.file_name().unwrap().to_string_lossy().into_owned();

    import_folder(&mut studio, &folder);

    studio.press(KeyCode::Char(' '), KeyModifiers::NONE);
    studio.settle();
    assert_eq!(studio.status(), format!("{name} off"));

    studio.press(KeyCode::Enter, KeyModifiers::NONE);
    studio.settle();
    assert_eq!(
        studio.status(),
        format!("{name} is off - Space turns it on"),
        "a silenced source is not refetched"
    );

    studio.press(KeyCode::Char(' '), KeyModifiers::NONE);
    studio.settle();
    // Enabling walks the folder again. Its receipt can replace the
    // transient `on` message before the next frame.
    let status = studio.status();
    assert!(
        status == format!("{name} on")
            || (status.starts_with(&format!("{name} -")) && status.contains("sound(s) imported")),
        "the source is enabled again: {status}"
    );

    // The refetch of a local folder settles faster than a frame: the
    // transient `fetching again` may already be the receipt, and the
    // receipt is the proof the scan ran.
    studio.press(KeyCode::Enter, KeyModifiers::NONE);
    studio.settle();
    wait_for_import(&mut studio);
    let status = studio.status().to_owned();
    assert!(
        status.starts_with(&format!("{name} -")) && status.contains("sound(s) imported"),
        "and an enabled one refetches on Enter, settling to its receipt: {status}"
    );

    studio.press(KeyCode::Esc, KeyModifiers::NONE);
    studio.settle();
    cleanup(&folder);
}

/// `r` aliases the bank a source imported: a source with one bank opens
/// the naming prompt straight away, and Esc calls the alias off with the
/// bank's own name still standing.
#[test]
fn r_aliases_the_bank_a_source_imported() {
    let mut studio = hermetic();
    let folder = audio_folder("alias");

    import_folder(&mut studio, &folder);

    studio.press(KeyCode::Char('r'), KeyModifiers::NONE);
    studio.settle();
    assert_eq!(
        studio.status(),
        "bank alias - type its playable name · original name resets · Esc back"
    );

    studio.press(KeyCode::Esc, KeyModifiers::NONE);
    studio.settle();
    assert_eq!(studio.status(), "alias cancelled");
    assert_eq!(
        studio.focus(),
        Some(rustel_studio_e2e::PanelKind::Settings),
        "the cancelled alias comes back to the sheet:\n{}",
        studio.rows().join("\n")
    );

    studio.press(KeyCode::Esc, KeyModifiers::NONE);
    studio.settle();
    cleanup(&folder);
}

/// Alt+R in the samples browser is the same alias door, reached from the
/// bank under the cursor rather than the source's row: a bank a source
/// imported opens the naming prompt, and Esc calls it off with the bank's
/// own name still standing. (A bank the studio ships with, or the score's
/// own, is not the player's to alias; the in-tree
/// `alt_r_from_the_browser_opens_the_rename_prompt_or_refuses` holds that
/// half.)
#[test]
fn alt_r_in_the_samples_browser_aliases_an_imported_bank() {
    const BANK: &str = "e2ealiasbank";
    let mut studio = hermetic();
    let folder = audio_folder_with_bank("browser-alias", BANK);
    import_folder(&mut studio, &folder);
    studio.press(KeyCode::Esc, KeyModifiers::NONE);
    studio.settle();
    assert_eq!(studio.focus(), None, "setup: the sheet is closed");

    // The samples tab, searched down to the imported bank: a search lists
    // exactly what it matched, the best match first and selected.
    studio.press(KeyCode::Home, KeyModifiers::NONE);
    studio.chord("ctrl+d");
    studio.press(KeyCode::Tab, KeyModifiers::NONE);
    studio.wait_for_catalogue();
    assert_eq!(studio.reference_tab(), Some("samples"));
    studio.type_text(BANK);
    studio.settle();
    assert!(
        studio.rows().iter().any(|row| row.contains(BANK)),
        "the imported bank is listed:\n{}",
        studio.rows().join("\n")
    );

    studio.press(KeyCode::Char('r'), KeyModifiers::ALT);
    studio.settle();
    assert_eq!(
        studio.status(),
        "bank alias - type its playable name · original name resets · Esc back"
    );

    studio.press(KeyCode::Esc, KeyModifiers::NONE);
    studio.settle();
    assert_eq!(studio.status(), "alias cancelled");
    assert!(
        studio
            .samples_catalogue_names()
            .iter()
            .any(|name| name == BANK),
        "the bank keeps its own name"
    );

    cleanup(&folder);
}

/// `e` opens the source's spec for editing - folder or URL, what is
/// typed is read as a source - and Esc keeps the source exactly as it
/// was.
#[test]
fn e_opens_the_source_for_editing_and_esc_keeps_it() {
    let mut studio = hermetic();
    let folder = audio_folder("edit");
    let name = folder.file_name().unwrap().to_string_lossy().into_owned();

    import_folder(&mut studio, &folder);

    studio.press(KeyCode::Char('e'), KeyModifiers::NONE);
    studio.settle();
    assert_eq!(
        studio.status(),
        "edit sample source - change the folder or URL · Enter keeps · Esc back"
    );

    studio.press(KeyCode::Esc, KeyModifiers::NONE);
    studio.settle();
    assert_eq!(studio.status(), "import cancelled");
    let rows = studio.rows();
    assert!(
        rows.iter()
            .any(|row| row.contains(&name) && row.contains("(local)")),
        "the kept source is still listed:\n{}",
        rows.join("\n")
    );

    studio.press(KeyCode::Esc, KeyModifiers::NONE);
    studio.settle();
    cleanup(&folder);
}

/// Emptying the sample cache is a two-step: the ask, then Esc cancels
/// without touching a file - and only the second Enter clears, with the
/// receipt arriving when the walk is done.
#[test]
fn clearing_the_sample_cache_asks_and_cancels() {
    let mut studio = hermetic_with_own_sample_cache();
    open_sources(&mut studio);
    select_row(&mut studio, "▸ clear cache");

    studio.press(KeyCode::Enter, KeyModifiers::NONE);
    studio.settle();
    assert_eq!(
        studio.status(),
        "Enter again clears the cache · Esc cancels"
    );

    studio.press(KeyCode::Esc, KeyModifiers::NONE);
    studio.settle();
    assert_eq!(studio.status(), "empty cancelled");
    assert!(
        studio.rows().iter().any(|row| row.contains("clear cache")),
        "the sheet is still standing on its Sources page:\n{}",
        studio.rows().join("\n")
    );

    select_row(&mut studio, "▸ clear cache");
    studio.press(KeyCode::Enter, KeyModifiers::NONE);
    studio.settle();
    studio.press(KeyCode::Enter, KeyModifiers::NONE);
    studio.settle();
    for _ in 0..500 {
        if studio.status().contains("cache cleared") {
            break;
        }
        studio.pump();
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert_eq!(
        studio.status(),
        "cache cleared - files download again when needed",
        "the second Enter cleared it and the receipt arrived"
    );

    studio.press(KeyCode::Esc, KeyModifiers::NONE);
    studio.settle();
}

/// The `fetch imports` switch says what "on" means: imported sounds cache
/// as they resolve. Off is a plain kept setting again. The row's own
/// explain says "not on first play", so the test reads the switch by its
/// bullet and not by the word "on".
#[test]
fn fetch_imports_says_what_it_turns_on() {
    let mut studio = hermetic();
    open_sources(&mut studio);

    let row = |studio: &mut Hermetic| {
        studio
            .rows()
            .into_iter()
            .find(|row| row.contains("fetch imports"))
            .expect("the switch is the page's first row")
    };
    assert!(
        row(&mut studio).contains("○ off"),
        "the switch ships off: {}",
        row(&mut studio)
    );

    studio.press(KeyCode::Char(' '), KeyModifiers::NONE);
    studio.settle();
    assert_eq!(
        studio.status(),
        "fetch imports on - the score's imported sounds cache as they resolve",
        "on explains itself"
    );
    assert!(
        row(&mut studio).contains("● on"),
        "and the row reads on: {}",
        row(&mut studio)
    );

    // Off is the plain kept setting - and leaves the studio the way it
    // was found.
    studio.press(KeyCode::Char(' '), KeyModifiers::NONE);
    studio.settle();
    assert_eq!(
        studio.status(),
        "settings kept - drawing with cells",
        "off needs no explanation of its own"
    );
    assert!(
        row(&mut studio).contains("○ off"),
        "and the row reads off again: {}",
        row(&mut studio)
    );

    studio.press(KeyCode::Esc, KeyModifiers::NONE);
    studio.settle();
}
