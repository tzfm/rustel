//! Check documented shortcuts and platform labels under both keyboard
//! capability sets. Assert each action's effect and read docs/studio.md directly.

// The engine's `silent` output refuses to open unless this process's
// allocator is the audio-callback tripwire - the callback must be provably
// allocation-free, and a global allocator can only come from a binary. See
// `crates/studio/tests/studio_live_engine.rs` for the same contract.
#[global_allocator]
static AUDIO_CALLBACK_ALLOCATOR: rustel_audio::tripwire::TripwireAlloc =
    rustel_audio::tripwire::TripwireAlloc;

use crossterm::event::{KeyCode, KeyModifiers};
use rustel_studio::editor::KeyboardCapabilities;
use rustel_studio_e2e::hermetic_with;
use std::path::PathBuf;

/// A documented chord and what the app should visibly do about it.
struct Documented {
    chord: &'static str,
    /// The docs table row this chord is promised under. Checked for
    /// presence so a renamed or deleted row breaks this test loudly.
    row: &'static str,
    /// Only meaningful under the enhanced capability set: a legacy terminal
    /// cannot report these apart from other keys.
    enhanced_only: bool,
    /// State the chord needs in place before it is pressed - a playing
    /// transport for Stop, history for Undo, a second scene for Next.
    setup: Option<fn(&mut rustel_studio_e2e::Hermetic)>,
    check: Check,
}

/// How a test tells the chord was honoured, without asserting on UI the
/// chord was not meant to change.
enum Check {
    /// The menu bar opened with nothing dropped down - F1 where the bar has
    /// a row to live in.
    Menu,
    /// The named panel opened (focus moved into it).
    Panel,
    /// The master fader moved.
    Master,
    /// The transport state changed.
    Transport(fn(&rustel_studio_e2e::Hermetic) -> bool),
    /// The frame changed shape (zen, split).
    Frame,
    /// ^J on a number wrote the fader: the score holds a `slider(...)`
    /// where the number stood, and the popup is closed again.
    SmartAction,
    /// The hop moved the caret to the other pane. ^⇧E (and F10) hop where
    /// the terminal delivers the shifted chord; the check opens a split
    /// first and holds the open frame against the hop.
    PaneHop,
    /// The slider literal under the caret moved. This chord means nothing
    /// without a live control, so it carries its own setup: a score with a
    /// `slider(...)` in it, evaluated so the engine registers the control.
    Slider,
    /// An editor effect: the setup made the history, the chord acts on it.
    Edit {
        expect: fn(&rustel_studio_e2e::Hermetic) -> bool,
    },
    /// The focused scene moved to this index - previous/next wraps included.
    SceneAt(usize),
    /// The scene strip took the keyboard: what is typed next reaches the
    /// strip, not the score, and Esc gives it back.
    Strip,
    /// The app answered on its status line - the receipt of a chord whose
    /// honest hermetic outcome is a message: there is no MIDI controller to
    /// learn from, no pad to forget, but the chord was recognised and
    /// answered, which is the part the docs promise.
    Says(&'static str),
    /// The same, where the honest answer depends on the host: a machine
    /// with a MIDI controller (a virtual one counts) enters learning, one
    /// without says there is nothing to learn from. Either receipt is the
    /// chord being honoured.
    SaysAny(&'static [&'static str]),
    /// A synchronous observable the chord owns, checked as a closure - the
    /// scene count a chord added, and anything else with no dedicated shape.
    Effect(fn(&rustel_studio_e2e::Hermetic) -> bool),
}

/// Setups the table names by intent.
///
/// `play_first` gives Stop a transport to cut; `type_an_edit` gives Undo
/// something to undo; `undo_first` gives the two redo spellings something to
/// redo; `three_scenes` gives the scene steppers room to move in both
/// directions.
fn play_first(studio: &mut rustel_studio_e2e::Hermetic) {
    studio.chord("ctrl+s");
    studio.settle();
    assert!(
        studio.is_playing(),
        "setup: the transport plays before Stop is pressed"
    );
}

fn type_an_edit(studio: &mut rustel_studio_e2e::Hermetic) {
    studio.type_text("$: s(\"hh\")\n");
    assert!(
        studio.source().contains("hh"),
        "setup: the edit is in the score:\n{}",
        studio.source()
    );
}

fn undo_first(studio: &mut rustel_studio_e2e::Hermetic) {
    type_an_edit(studio);
    studio.chord("ctrl+z");
    assert!(
        !studio.source().contains("hh"),
        "setup: the edit is undone:\n{}",
        studio.source()
    );
}

fn three_scenes(studio: &mut rustel_studio_e2e::Hermetic) {
    studio.chord("ctrl+n");
    studio.chord("ctrl+n");
    studio.settle();
    assert_eq!(
        studio.scene_names().len(),
        3,
        "setup: three scenes to step between"
    );
    assert_eq!(
        studio.current_scene_index(),
        2,
        "setup: the last is focused"
    );
}

/// The caret on a bare number - `800` in an `lpf` - the thing ^J turns
/// into a fader. The offset is found in the source rather than counted:
/// the count is where drift hides.
fn caret_on_a_number(studio: &mut rustel_studio_e2e::Hermetic) {
    studio.set_score("$: s(\"bd\").lpf(800)");
    let source = studio.source();
    let at = source
        .find("800")
        .expect("setup: the number is in the score");
    studio.set_caret(at + 2);
}

const DOCUMENTED: &[Documented] = &[
    Documented {
        chord: "f1",
        row: "Menu bar",
        enhanced_only: false,
        setup: None,
        check: Check::Menu,
    },
    Documented {
        chord: "ctrl+s",
        row: "Update (write the file and play it)",
        enhanced_only: false,
        setup: None,
        check: Check::Transport(|s| s.is_playing()),
    },
    Documented {
        chord: "ctrl+enter",
        row: "Update (write the file and play it)",
        enhanced_only: true,
        setup: None,
        check: Check::Transport(|s| s.is_playing()),
    },
    Documented {
        chord: "ctrl+.",
        row: "Stop",
        enhanced_only: true,
        setup: Some(play_first),
        check: Check::Transport(|s| s.is_stopping() || !s.is_playing()),
    },
    Documented {
        chord: "f5",
        row: "Update (write the file and play it)",
        enhanced_only: false,
        setup: None,
        check: Check::Transport(|s| s.is_playing()),
    },
    Documented {
        chord: "ctrl+g",
        row: "Stop",
        enhanced_only: false,
        setup: Some(play_first),
        check: Check::Transport(|s| s.is_stopping() || !s.is_playing()),
    },
    Documented {
        chord: "f8",
        row: "Stop",
        enhanced_only: false,
        setup: Some(play_first),
        check: Check::Transport(|s| s.is_stopping() || !s.is_playing()),
    },
    Documented {
        chord: "ctrl+z",
        row: "Undo / redo",
        enhanced_only: false,
        setup: Some(type_an_edit),
        check: Check::Edit {
            expect: |s| !s.source().contains("hh"),
        },
    },
    Documented {
        chord: "ctrl+shift+z",
        row: "Undo / redo",
        enhanced_only: false,
        setup: Some(undo_first),
        check: Check::Edit {
            expect: |s| s.source().contains("hh"),
        },
    },
    Documented {
        chord: "ctrl+y",
        row: "Undo / redo",
        enhanced_only: false,
        setup: Some(undo_first),
        check: Check::Edit {
            expect: |s| s.source().contains("hh"),
        },
    },
    Documented {
        chord: "ctrl+/",
        row: "Comment / uncomment",
        enhanced_only: false,
        setup: None,
        check: Check::Edit {
            expect: |s| s.source().starts_with("//"),
        },
    },
    Documented {
        chord: "ctrl+p",
        row: "Devices",
        enhanced_only: false,
        setup: None,
        check: Check::Panel,
    },
    Documented {
        chord: "ctrl+shift+up",
        row: "Master volume",
        enhanced_only: true,
        setup: None,
        check: Check::Master,
    },
    Documented {
        chord: "ctrl+shift+down",
        row: "Master volume",
        enhanced_only: true,
        setup: None,
        check: Check::Master,
    },
    Documented {
        chord: "alt+up",
        row: "Slider under the caret",
        enhanced_only: false,
        setup: None,
        check: Check::Slider,
    },
    Documented {
        chord: "alt+down",
        row: "Slider under the caret",
        enhanced_only: false,
        setup: None,
        check: Check::Slider,
    },
    Documented {
        chord: "ctrl+j",
        row: "Smart action at the caret",
        enhanced_only: false,
        setup: Some(caret_on_a_number),
        check: Check::SmartAction,
    },
    Documented {
        chord: "f11",
        row: "Zen mode",
        enhanced_only: false,
        setup: None,
        check: Check::Frame,
    },
    Documented {
        chord: "f6",
        row: "Previous / next scene",
        enhanced_only: false,
        setup: Some(three_scenes),
        check: Check::SceneAt(1),
    },
    Documented {
        chord: "f7",
        row: "Previous / next scene",
        enhanced_only: false,
        setup: Some(three_scenes),
        check: Check::SceneAt(0),
    },
    Documented {
        chord: "ctrl+n",
        row: "New scene / duplicate",
        enhanced_only: false,
        setup: None,
        check: Check::Effect(|s| s.scene_names().len() == 2),
    },
    Documented {
        chord: "ctrl+shift+n",
        row: "New scene / duplicate",
        enhanced_only: true,
        setup: None,
        check: Check::Effect(|s| s.scene_names().len() == 2),
    },
    Documented {
        chord: "ctrl+r",
        row: "Rename / close scene",
        enhanced_only: false,
        setup: None,
        check: Check::Strip,
    },
    Documented {
        chord: "ctrl+shift+r",
        row: "Record a take",
        enhanced_only: true,
        setup: None,
        check: Check::Says("recording"),
    },
    Documented {
        chord: "f9",
        row: "Log",
        enhanced_only: false,
        setup: None,
        check: Check::Panel,
    },
    Documented {
        chord: "ctrl+shift+x",
        row: "Export",
        enhanced_only: true,
        setup: None,
        check: Check::Panel,
    },
    Documented {
        chord: "ctrl+t",
        row: "Theme",
        enhanced_only: false,
        setup: None,
        check: Check::Panel,
    },
    Documented {
        chord: "ctrl+b",
        row: "Settings",
        enhanced_only: false,
        setup: None,
        check: Check::Panel,
    },
    Documented {
        chord: "ctrl+shift+p",
        row: "Settings",
        enhanced_only: true,
        setup: None,
        check: Check::Panel,
    },
    Documented {
        chord: "ctrl+l",
        row: "Learn a pad / forget it",
        enhanced_only: false,
        setup: None,
        check: Check::SaysAny(&["learning -", "no MIDI input"]),
    },
    Documented {
        chord: "ctrl+shift+l",
        row: "Learn a pad / forget it",
        enhanced_only: true,
        setup: None,
        check: Check::Says("has no pad"),
    },
    Documented {
        chord: "ctrl+d",
        row: "Docs for the function at the caret",
        enhanced_only: false,
        setup: None,
        check: Check::Panel,
    },
    Documented {
        chord: "ctrl+f",
        row: "Values for the argument at the caret; the reference",
        enhanced_only: false,
        setup: None,
        check: Check::Panel,
    },
    Documented {
        chord: "ctrl+space",
        row: "Values for the argument at the caret; the reference",
        enhanced_only: false,
        setup: None,
        check: Check::Panel,
    },
    Documented {
        chord: "ctrl+e",
        row: "Split / close the split",
        enhanced_only: false,
        setup: None,
        check: Check::Frame,
    },
    Documented {
        chord: "ctrl+shift+e",
        row: "Split / close the split",
        enhanced_only: true,
        setup: None,
        check: Check::PaneHop,
    },
    Documented {
        chord: "ctrl+[",
        row: "Previous / next scene",
        enhanced_only: true,
        setup: Some(three_scenes),
        check: Check::SceneAt(1),
    },
    Documented {
        chord: "ctrl+]",
        row: "Previous / next scene",
        enhanced_only: true,
        setup: Some(three_scenes),
        check: Check::SceneAt(0),
    },
];

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
}

/// The docs table exists and is the one this test reads - if the file
/// moves or the table is renamed, this test says so rather than silently
/// passing over an empty parse. Every chord this file asserts is still
/// promised under its row; a row removed from the docs must remove its
/// entry here in the same PR.
#[test]
fn the_documented_shortcut_table_is_present() {
    let docs = std::fs::read_to_string(repo_root().join("docs").join("studio.md"))
        .expect("docs/studio.md is readable");
    for Documented { chord, row, .. } in DOCUMENTED {
        assert!(
            docs.contains(row),
            "the docs row {row:?} (promising `{chord}`) is gone or renamed; \
             update the table and this list together"
        );
    }
}

/// Every documented chord does what the docs promise, under the capability
/// sets that can deliver it. Each chord gets a fresh studio: several of
/// them change global mode (the menu swallows input, a split changes the
/// frame), and one test's leftovers must never be the next test's setup.
#[test]
fn every_documented_chord_is_honoured() {
    for Documented {
        chord,
        enhanced_only,
        setup,
        check,
        ..
    } in DOCUMENTED
    {
        for capabilities in capability_sets(*enhanced_only) {
            let mut studio = hermetic_with(capabilities);
            if let Some(setup) = setup {
                setup(&mut studio);
            }
            let frame_before = match check {
                Check::Frame => Some(studio.screen()),
                _ => None,
            };
            let master_before = match check {
                Check::Master => studio.master_gain_db(),
                _ => 0.0,
            };
            if matches!(check, Check::Slider) {
                // The chord nudges a live control, so the setup gives it
                // one: a score with a slider at a known value, evaluated
                // so the engine has registered it. The caret lands inside
                // the `slider(...)` call - the range the chord reads.
                studio.chord("ctrl+a");
                studio.type_text("$: s(\"bd\").gain(slider(0.8,0,1,0.1))");
                studio.chord("ctrl+s");
                studio.settle();
                assert!(
                    studio.source().contains("slider(0.8"),
                    "setup: the slider score is in place:\n{}",
                    studio.source()
                );
                for _ in 0..3 {
                    studio.press(KeyCode::Left, KeyModifiers::NONE);
                }
            }
            studio.chord(chord);
            match check {
                Check::Menu => assert!(
                    studio.menu_open(),
                    "the documented chord `{chord}` ({capabilities:?}) did not open \
                     the menu bar; the docs and the keymap have drifted"
                ),
                Check::Panel => assert!(
                    studio.focus().is_some() || studio.panel().is_some(),
                    "the documented chord `{chord}` ({capabilities:?}) opened nothing; \
                     the docs and the keymap have drifted"
                ),
                Check::Master => assert!(
                    (studio.master_gain_db() - master_before).abs() > f32::EPSILON,
                    "the documented chord `{chord}` ({capabilities:?}) did not move the \
                     master fader"
                ),
                Check::Transport(played) => {
                    // Playing is asynchronous: the chord hands the score to
                    // the worker, and the transport state lands after a turn.
                    studio.settle();
                    assert!(
                        played(&studio),
                        "the documented chord `{chord}` ({capabilities:?}) did not reach the \
                         transport"
                    );
                }
                Check::Frame => {
                    studio.settle();
                    assert_ne!(
                        frame_before,
                        Some(studio.screen()),
                        "the documented chord `{chord}` ({capabilities:?}) changed no frame"
                    );
                }
                Check::PaneHop => {
                    studio.chord("ctrl+e");
                    studio.settle();
                    let open_frame = studio.screen();
                    studio.chord(chord);
                    studio.settle();
                    assert_ne!(
                        Some(open_frame),
                        Some(studio.screen()),
                        "the documented chord `{chord}` ({capabilities:?}) hopped to no other \
                         pane"
                    );
                }
                Check::Slider => {
                    // Up from 0.8 lands on 0.9; down lands on 0.7. The
                    // step is the slider literal's own 0.1.
                    let expected = if chord.ends_with("up") { "0.9" } else { "0.7" };
                    assert!(
                        studio.source().contains(expected),
                        "the documented chord `{chord}` ({capabilities:?}) did not move the \
                         slider literal to {expected}:\n{}",
                        studio.source()
                    );
                    // The move names the slider by the call it sits in.
                    // Alt+Enter armed the literal, so the hint shows the
                    // step and not the plain reading.
                    assert!(
                        studio.status().contains("gain ="),
                        "the move is named for the call the slider sits in: {}",
                        studio.status()
                    );
                }
                Check::SmartAction => {
                    // ^J opens the menu. Enter chooses "make this number a
                    // fader", and Enter again writes the call without
                    // evaluating. The form arrives filled from the number.
                    assert!(
                        studio.status().starts_with("smart action: Enter chooses"),
                        "the documented chord `{chord}` ({capabilities:?}) did not open the \
                         smart action: {}",
                        studio.status()
                    );
                    studio.press(KeyCode::Enter, KeyModifiers::NONE);
                    assert!(
                        studio.status().starts_with("Tab between the fields"),
                        "Enter on the menu opened the form: {}",
                        studio.status()
                    );
                    studio.press(KeyCode::Enter, KeyModifiers::NONE);
                    assert!(
                        studio.source().contains("slider("),
                        "the documented chord `{chord}` ({capabilities:?}) made no fader of \
                         the number:\n{}",
                        studio.source()
                    );
                    assert!(
                        !studio.status().starts_with("smart action:"),
                        "the documented chord `{chord}` ({capabilities:?}) left the smart \
                         action open: {}",
                        studio.status()
                    );
                }
                Check::Edit { expect } => assert!(
                    expect(&studio),
                    "the documented chord `{chord}` ({capabilities:?}) did not do its edit:\n{}",
                    studio.source()
                ),
                Check::SceneAt(expected) => assert_eq!(
                    studio.current_scene_index(),
                    *expected,
                    "the documented chord `{chord}` ({capabilities:?}) moved the scene to the \
                     wrong place"
                ),
                Check::Strip => {
                    // A rename owns the keyboard: the letters it is given
                    // name the scene, they never reach the score, and Esc
                    // gives the keyboard back with nothing renamed.
                    studio.type_text("zz");
                    assert!(
                        !studio.source().contains("zz"),
                        "the documented chord `{chord}` ({capabilities:?}) let typing reach the \
                         score while the strip held the keyboard:\n{}",
                        studio.source()
                    );
                    assert_eq!(
                        studio.scene_names(),
                        vec!["first"],
                        "the documented chord `{chord}` ({capabilities:?}) renamed before Enter"
                    );
                    studio.press(KeyCode::Esc, KeyModifiers::NONE);
                    assert_eq!(
                        studio.scene_names(),
                        vec!["first"],
                        "the documented chord `{chord}` ({capabilities:?}) did not let Esc cancel"
                    );
                }
                Check::Says(receipt) => assert!(
                    studio.status().contains(receipt),
                    "the documented chord `{chord}` ({capabilities:?}) answered \
                     `{}` instead of saying it {receipt:?}",
                    studio.status()
                ),
                Check::SaysAny(one_of) => assert!(
                    one_of
                        .iter()
                        .any(|receipt| studio.status().contains(receipt)),
                    "the documented chord `{chord}` ({capabilities:?}) answered `{}` instead \
                     of one of {one_of:?}",
                    studio.status()
                ),
                Check::Effect(effect) => assert!(
                    effect(&studio),
                    "the documented chord `{chord}` ({capabilities:?}) had no effect \
                     (scenes: {:?})",
                    studio.scene_names()
                ),
            }
        }
    }
}

fn capability_sets(enhanced_only: bool) -> Vec<KeyboardCapabilities> {
    if enhanced_only {
        vec![KeyboardCapabilities::enhanced()]
    } else {
        vec![
            KeyboardCapabilities::legacy(),
            KeyboardCapabilities::enhanced(),
        ]
    }
}

/// The platform label: how footers and hints spell the modifier. Every
/// platform uses caret notation and none uses ⌘, because Mac terminals
/// already claim the chords worth advertising. Checked once per capability
/// set.
#[test]
fn the_platform_label_matches_the_capability_set() {
    assert_eq!(
        KeyboardCapabilities::legacy().primary_label(),
        "^",
        "the legacy spelling is caret notation"
    );
    assert_eq!(
        KeyboardCapabilities::enhanced().primary_label(),
        "^",
        "the enhanced set spells it the same way - no ⌘, on any platform"
    );
    // And the hints are built from it, so the footer reads as one language.
    assert_eq!(KeyboardCapabilities::legacy().evaluate_hint(), "^S");
    assert_eq!(KeyboardCapabilities::legacy().stop_hint(), "^G");
}

/// A chord the docs do not document must do nothing. Ctrl+I is bound to
/// nothing in the studio: no menu, no panel, no character in the score. The
/// test presses it under the enhanced set, where a terminal reports it as
/// itself; a legacy terminal sends it as Tab's byte.
#[test]
fn an_undocumented_chord_claims_nothing() {
    let mut studio = hermetic_with(KeyboardCapabilities::enhanced());
    let before = studio.source();
    let status = studio.status().to_owned();
    studio.press(KeyCode::Char('i'), KeyModifiers::CONTROL);
    assert_eq!(
        studio.source(),
        before,
        "Ctrl+I typed into the score, so it is a binding the docs do not know about"
    );
    assert!(
        !studio.menu_open(),
        "Ctrl+I opened the menu, so it is a binding the docs do not know about"
    );
    assert_eq!(
        studio.focus(),
        None,
        "Ctrl+I opened a panel, so it is a binding the docs do not know about"
    );
    assert_eq!(
        studio.status(),
        status,
        "Ctrl+I answered on the status line, so it is a binding the docs do not know about"
    );
}

/// Documented but deliberately NOT pressed by the suite:
///
/// - `Ctrl+Shift+O` (show the last file): it hands the file to the host's
///   desktop file manager. On a CI runner that is at best a silent no-op
///   and at worst an open window; the app-side tests cover it.
/// - `Ctrl+Shift+D` (log): a legacy terminal folds it onto `Ctrl+D`, the
///   reference chord, so its behaviour is capability-dependent by design;
///   `F9` covers the log under both sets.
/// - `Ctrl+B, Enter on its row` (open a prebake): covered by the in-tree
///   settings tests; the chord's first half is asserted above.
///
/// `Ctrl+Q` is not in this table's loop - a quit would end the studio the
/// loop goes on to press the next chord into - but it is pressed: its
/// two-press arming is `quit_arming.rs`, and `piano_mode.rs` holds it
/// closing the piano first. `Ctrl+H` (record a sample) is likewise held by
/// `record_sample.rs`, against a studio that opens no input.
const _NOT_PRESSED_BY_THE_SUITE: &[&str] = &["ctrl+shift+o", "ctrl+shift+d"];
