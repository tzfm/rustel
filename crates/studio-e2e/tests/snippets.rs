//! End-to-end navigation and copying for the separate Generator and Examples tabs.

#[global_allocator]
static AUDIO_CALLBACK_ALLOCATOR: rustel_audio::tripwire::TripwireAlloc =
    rustel_audio::tripwire::TripwireAlloc;

use crossterm::event::{KeyCode, KeyModifiers};
use rustel_studio_e2e::{Hermetic, hermetic, row_containing};

fn open_tab(steps: usize) -> Hermetic {
    let mut studio = hermetic();
    studio.chord("ctrl+f");
    for _ in 0..steps {
        studio.press(KeyCode::Tab, KeyModifiers::NONE);
    }
    studio
}

fn first_example() -> Hermetic {
    let mut studio = open_tab(5);
    assert_eq!(studio.reference_tab(), Some("examples"));
    studio.press(KeyCode::Down, KeyModifiers::NONE); // First groove shelf
    studio.press(KeyCode::Right, KeyModifiers::NONE);
    studio.press(KeyCode::Down, KeyModifiers::NONE); // Four on the floor
    studio
}

#[test]
fn examples_is_last_and_the_browser_wraps() {
    let mut studio = open_tab(4);
    assert_eq!(studio.reference_tab(), Some("generator"));
    row_containing(&studio.rows(), "Glassy pulse");
    studio.press(KeyCode::Tab, KeyModifiers::NONE);
    assert_eq!(studio.reference_tab(), Some("examples"));
    row_containing(&studio.rows(), "PARTS");
    studio.press(KeyCode::Tab, KeyModifiers::NONE);
    assert_eq!(studio.reference_tab(), Some("reference"));
}

#[test]
fn arrows_open_and_fold_the_example_shelves() {
    let mut studio = open_tab(5);
    studio.press(KeyCode::Down, KeyModifiers::NONE);
    studio.press(KeyCode::Right, KeyModifiers::NONE);
    row_containing(&studio.rows(), "Four on the floor");
    studio.press(KeyCode::Left, KeyModifiers::NONE);
    assert!(
        !studio
            .rows()
            .iter()
            .any(|row| row.contains("Four on the floor"))
    );
    studio.press(KeyCode::Right, KeyModifiers::NONE);
    row_containing(&studio.rows(), "Four on the floor");
}

#[test]
fn enter_copies_a_whole_example_and_leaves_the_tree_open() {
    let mut studio = first_example();
    let before = studio.score();
    studio.press(KeyCode::Enter, KeyModifiers::NONE);
    let copied = studio.clipboard_text().expect("the example was copied");
    assert_eq!(
        copied.trim(),
        "$: s(\"bd*4, [~ cp]*2, hh*8\").bank(\"RolandTR909\")"
    );
    assert_eq!(studio.score(), before);
    assert_eq!(studio.reference_tab(), Some("examples"));
    row_containing(&studio.rows(), "Four on the floor");
}

#[test]
fn the_generator_composes_and_recalls_exact_code() {
    let mut studio = open_tab(4);
    studio.press(KeyCode::Down, KeyModifiers::NONE); // Generate
    studio.type_text("c");
    let first = studio.clipboard_text().expect("initial idea");
    assert!(first.contains("$:"));
    studio.press(KeyCode::Right, KeyModifiers::NONE);
    studio.type_text("c");
    let second = studio.clipboard_text().expect("new idea");
    assert_ne!(first, second);
    studio.press(KeyCode::Left, KeyModifiers::NONE);
    studio.type_text("c");
    assert_eq!(studio.clipboard_text().unwrap(), first);
    studio.press(KeyCode::Right, KeyModifiers::NONE);
    studio.type_text("c");
    assert_eq!(studio.clipboard_text().unwrap(), second);
}

#[test]
fn a_music_example_can_be_copied_as_a_stack() {
    let mut studio = first_example();
    row_containing(&studio.rows(), "bd*4");
    studio.type_text("s");
    studio.press(KeyCode::Enter, KeyModifiers::NONE);
    let copied = studio.clipboard_text().expect("stack copied");
    assert!(copied.contains("stack("), "{copied}");
    assert!(copied.contains("bd*4"), "{copied}");
    assert!(!copied.contains(".out()"));
}
