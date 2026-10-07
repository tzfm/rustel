//! Keep coverage.md rows in one-to-one correspondence with suite files.

use std::collections::BTreeSet;
use std::path::PathBuf;

fn crate_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// The suite files on disk. A file here with no table row fails the run.
fn suite_files() -> BTreeSet<String> {
    let tests = crate_root().join("tests");
    let mut names = BTreeSet::new();
    for entry in std::fs::read_dir(&tests).expect("tests/ directory is readable") {
        let entry = entry.expect("tests/ entry is readable");
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.ends_with(".rs") {
            names.insert(name.into_owned());
        }
    }
    assert!(
        !names.is_empty(),
        "no suite files found under tests/ - the directory moved?"
    );
    names
}

/// The suite file a table row names, or `None` for a line that is not such a
/// row. Rows read ``| `file.rs` | surface | key cases | status |``. The
/// backticks are optional, so a row written without them is still checked.
/// The header and separator name no `.rs` file, so they return `None`.
fn row_file(line: &str) -> Option<String> {
    let line = line.trim_start();
    if !line.starts_with('|') {
        return None;
    }
    let cell = line.trim_start_matches('|').split('|').next()?.trim();
    let file = cell.trim_matches('`').trim();
    file.ends_with(".rs").then(|| file.to_owned())
}

/// The `.rs` files the table's first column names.
fn table_files() -> BTreeSet<String> {
    let coverage =
        std::fs::read_to_string(crate_root().join("coverage.md")).expect("coverage.md is readable");
    let names: BTreeSet<String> = coverage.lines().filter_map(row_file).collect();
    assert!(
        !names.is_empty(),
        "coverage.md's table parsed to no rows - the table's shape moved? \
         rows read `| `file.rs` | surface | key cases | status |`"
    );
    names
}

/// Every suite file has its row - the table is the whole inventory of what
/// is tested, by construction.
#[test]
fn every_suite_file_has_a_row_in_the_coverage_table() {
    let on_disk = suite_files();
    let in_table = table_files();
    let missing: Vec<_> = on_disk.difference(&in_table).collect();
    assert!(
        missing.is_empty(),
        "suite files with no row in coverage.md: {missing:?}; \
         a PR that adds a suite adds its row in the same change"
    );
}

/// Every row names a suite file that exists - the table never promises a
/// surface the suite no longer holds.
#[test]
fn every_coverage_row_names_a_suite_file() {
    let on_disk = suite_files();
    let in_table = table_files();
    let stale: Vec<_> = in_table.difference(&on_disk).collect();
    assert!(
        stale.is_empty(),
        "coverage.md rows with no suite file behind them: {stale:?}; \
         remove the row or restore the file"
    );
}

/// Every suite that opens a studio installs the audio-callback tripwire.
///
/// The engine's `silent` output refuses to open without it. The app turns
/// that `Err` into a status line, not a panic, so a suite without the
/// allocator still passes its editor, panel and desk assertions with no
/// engine behind them.
///
/// The suites that open no studio in-process do not install it: this guard,
/// `docs_claims.rs`, and the two pty suites, which drive a separate binary
/// that installs its own.
#[test]
fn every_suite_that_opens_a_studio_installs_the_audio_tripwire() {
    let mut missing = Vec::new();
    for name in suite_files() {
        let path = crate_root().join("tests").join(&name);
        let source = std::fs::read_to_string(&path)
            .unwrap_or_else(|error| panic!("{} is readable: {error}", path.display()));
        if source.contains("hermetic") && !source.contains("#[global_allocator]") {
            missing.push(name);
        }
    }
    assert!(
        missing.is_empty(),
        "these suites open a studio without the audio-callback tripwire, so the \
         engine refuses to open and every assertion runs against a studio with \
         no engine behind it: {missing:?}"
    );
}

/// A row is either `covered` or the PR is not done: an honest gap lives in
/// the *not yet covered* section, which a PR must never grow - the table's
/// own rule, held here so a row cannot quietly sit at `partial` forever.
#[test]
fn every_row_is_either_covered_or_the_pr_is_not_done() {
    let coverage =
        std::fs::read_to_string(crate_root().join("coverage.md")).expect("coverage.md is readable");
    for line in coverage.lines() {
        let Some(file) = row_file(line) else {
            continue;
        };
        let cells: Vec<&str> = line.trim_start().split('|').collect();
        let status = cells
            .iter()
            .rev()
            .find(|cell| !cell.trim().is_empty())
            .map(|cell| cell.trim())
            .unwrap_or("");
        assert_eq!(
            status, "covered",
            "coverage.md's row for {file} reads `{status}`; an uncovered \
             surface belongs in *not yet covered*, and a PR must never grow \
             that list"
        );
    }
}

/// The *not yet covered* section as it stands: the standing `None.`
/// declaration, word for word.
const NOTHING_UNCOVERED: &str = "None. Every surface the studio ships has a suite row \
     above; a PR that adds a surface adds its row here in the same change.";

/// The *not yet covered* section must equal the standing `None.`
/// declaration, compared word for word. A list entry or a gap written as
/// prose fails this test. A gap that ships must change this assertion in
/// the same change, so a reviewer sees it.
#[test]
fn the_not_yet_covered_ledger_has_not_grown() {
    let coverage =
        std::fs::read_to_string(crate_root().join("coverage.md")).expect("coverage.md is readable");
    let section = coverage
        .split("## Not yet covered")
        .nth(1)
        .expect("coverage.md has a *not yet covered* section")
        .split("## ")
        .next()
        .expect("the section ends at the next heading");
    let words = |text: &str| text.split_whitespace().collect::<Vec<_>>().join(" ");
    assert_eq!(
        words(section),
        words(NOTHING_UNCOVERED),
        "the *not yet covered* ledger has grown:\n{}\n\
         a PR must never grow this list - cover the surface, or make the \
         gap visible by changing this assertion in review",
        section.trim()
    );
}
