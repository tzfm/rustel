//! Exercise the actual Unix keyboard decoder on every CI host, including Windows.
//! The terminal_decoder integration test supplies Cargo's resolved source.
//! Only the event types and raw-mode query are adapted; no parser code is copied.

#![cfg(test)]

#[allow(dead_code)]
mod event {
    pub use crossterm::event::*;

    #[derive(Debug, Clone, PartialEq)]
    pub enum InternalEvent {
        Event(Event),
        CursorPosition(u16, u16),
        KeyboardEnhancementFlags(KeyboardEnhancementFlags),
        PrimaryDeviceAttributes,
    }

    pub mod internal {
        pub use super::InternalEvent;
    }

    pub mod sys {
        pub mod unix {
            // Upstream code has its own feature gates and lint conventions.
            #[allow(unexpected_cfgs, clippy::all)]
            pub mod parse {
                include!(env!("RUSTEL_CROSSTERM_PARSER"));
            }
        }
    }
}

mod terminal {
    pub mod sys {
        pub fn is_raw_mode_enabled() -> bool {
            false
        }
    }
}

use event::sys::unix::parse::parse_event;
use event::{Event, InternalEvent, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, ModifierKeyCode};

fn decoded_key(bytes: &[u8]) -> KeyEvent {
    let parsed = parse_event(bytes, false).unwrap_or_else(|error| panic!("{bytes:?}: {error}"));
    let Some(InternalEvent::Event(Event::Key(event))) = parsed else {
        panic!("{bytes:?}: expected a key, got {parsed:?}");
    };
    event
}

fn assert_key(bytes: &[u8], code: KeyCode, modifiers: KeyModifiers, kind: KeyEventKind) {
    let event = decoded_key(bytes);
    // KeyEvent::PartialEq normalizes ASCII case. Compare the fields directly so
    // a lowercase code plus SHIFT cannot hide broken uppercase text insertion.
    assert_eq!(event.code, code, "{bytes:?}");
    assert_eq!(event.modifiers, modifiers, "{bytes:?}");
    assert_eq!(event.kind, kind, "{bytes:?}");
}

#[test]
fn ghostty_modifier_reports_preserve_standalone_press_and_release_events() {
    // Ghostty reports these with the negotiated report-all + report-events
    // flags. In particular, releasing Shift must reach Studio even while the
    // pointer stays still, so it can restore the application pointer shape.
    for (number, code, press_mask, modifier) in [
        (57441, ModifierKeyCode::LeftShift, 2, KeyModifiers::SHIFT),
        (57447, ModifierKeyCode::RightShift, 2, KeyModifiers::SHIFT),
        (
            57442,
            ModifierKeyCode::LeftControl,
            5,
            KeyModifiers::CONTROL,
        ),
        (
            57448,
            ModifierKeyCode::RightControl,
            5,
            KeyModifiers::CONTROL,
        ),
        (57443, ModifierKeyCode::LeftAlt, 3, KeyModifiers::ALT),
        (57449, ModifierKeyCode::RightAlt, 3, KeyModifiers::ALT),
        (57444, ModifierKeyCode::LeftSuper, 9, KeyModifiers::SUPER),
        (57450, ModifierKeyCode::RightSuper, 9, KeyModifiers::SUPER),
    ] {
        let press = format!("\x1b[{number};{press_mask}u");
        assert_key(
            press.as_bytes(),
            KeyCode::Modifier(code),
            modifier,
            KeyEventKind::Press,
        );
        let release = format!("\x1b[{number};1:3u");
        assert_key(
            release.as_bytes(),
            KeyCode::Modifier(code),
            // Crossterm includes the key's own modifier even on release.
            // Consumers must use the event kind, not an empty modifier mask.
            modifier,
            KeyEventKind::Release,
        );
    }
}

#[test]
fn iterm_function_and_navigation_reports_preserve_keys_modifiers_and_event_types() {
    // iTerm2 3.6.11: csiUNumber identifies the key, while F1..F12 and
    // Insert/Delete use zero for their shifted and base-layout alternatives.
    let mut cases: Vec<(String, KeyCode, bool)> = [11, 12, 13, 14, 15, 17, 18, 19, 20, 21, 23, 24]
        .into_iter()
        .enumerate()
        .map(|(index, number)| (format!("{number}~"), KeyCode::F(index as u8 + 1), true))
        .collect();
    cases.extend([
        ("2~".into(), KeyCode::Insert, true),
        ("3~".into(), KeyCode::Delete, true),
        ("H".into(), KeyCode::Home, false),
        ("F".into(), KeyCode::End, false),
        ("A".into(), KeyCode::Up, false),
        ("B".into(), KeyCode::Down, false),
        ("C".into(), KeyCode::Right, false),
        ("D".into(), KeyCode::Left, false),
        ("5~".into(), KeyCode::PageUp, false),
        ("6~".into(), KeyCode::PageDown, false),
    ]);
    for (definition, code, zero_alternates) in cases {
        for (mask, modifiers) in [
            (2, KeyModifiers::SHIFT),
            (5, KeyModifiers::CONTROL),
            (6, KeyModifiers::CONTROL | KeyModifiers::SHIFT),
        ] {
            for (event_suffix, kind) in [
                ("", KeyEventKind::Press),
                (":2", KeyEventKind::Repeat),
                (":3", KeyEventKind::Release),
            ] {
                let sequence = if let Some(number) = definition.strip_suffix('~') {
                    let alternate = if zero_alternates && modifiers.contains(KeyModifiers::SHIFT) {
                        ":0:0"
                    } else {
                        ""
                    };
                    format!("\x1b[{number}{alternate};{mask}{event_suffix}~")
                } else {
                    format!("\x1b[1;{mask}{event_suffix}{definition}")
                };
                assert_key(sequence.as_bytes(), code, modifiers, kind);
            }
        }
    }
}

#[test]
fn shifted_text_preserves_layout_characters_and_piano_releases() {
    for (primary, shifted, expected) in [
        (97, 65, 'A'),
        (49, 33, '!'),
        (50, 64, '@'),
        (59, 58, ':'),
        (233, 201, 'É'),
        (1078, 1046, 'Ж'),
    ] {
        for (suffix, kind) in [
            ("", KeyEventKind::Press),
            (":2", KeyEventKind::Repeat),
            (":3", KeyEventKind::Release),
        ] {
            let sequence = format!("\x1b[{primary}:{shifted};2{suffix}u");
            assert_key(
                sequence.as_bytes(),
                KeyCode::Char(expected),
                KeyModifiers::NONE,
                kind,
            );
        }
    }
    for (text, modifiers) in [
        ("é", KeyModifiers::NONE),
        ("Ж", KeyModifiers::SHIFT),
        ("中", KeyModifiers::NONE),
    ] {
        assert_key(
            text.as_bytes(),
            KeyCode::Char(text.chars().next().unwrap()),
            modifiers,
            KeyEventKind::Press,
        );
    }
    for (suffix, kind) in [
        ("", KeyEventKind::Press),
        (":2", KeyEventKind::Repeat),
        (":3", KeyEventKind::Release),
    ] {
        let sequence = format!("\x1b[97;1{suffix}u");
        assert_key(
            sequence.as_bytes(),
            KeyCode::Char('a'),
            KeyModifiers::NONE,
            kind,
        );
    }
}

#[test]
fn disabling_alternate_reports_would_lose_shifted_text() {
    // Guards against replacing the parser fix with a flag change. These are
    // base characters, not the text the user typed on a shifted keyboard.
    assert_eq!(decoded_key(b"\x1b[97;2u").code, KeyCode::Char('a'));
    assert_eq!(decoded_key(b"\x1b[49;2u").code, KeyCode::Char('1'));
}

#[test]
fn legacy_modified_f3_remains_ambiguous_with_cursor_position_replies() {
    for (modifier, column) in [(2, 1), (5, 4), (6, 5)] {
        let sequence = format!("\x1b[1;{modifier}R");
        assert_eq!(
            parse_event(sequence.as_bytes(), false).unwrap(),
            Some(InternalEvent::CursorPosition(column, 0))
        );
    }
}

#[test]
fn apple_terminal_default_sequences_decode_to_the_keys_studio_normalizes() {
    assert_key(
        b"\x1c",
        KeyCode::Char('4'),
        KeyModifiers::CONTROL,
        KeyEventKind::Press,
    );
    // Apple Terminal's default Shift+F5 through Shift+F12 report F13 through F20.
    for (number, function) in [
        (25, 13),
        (26, 14),
        (28, 15),
        (29, 16),
        (31, 17),
        (32, 18),
        (33, 19),
        (34, 20),
    ] {
        assert_key(
            format!("\x1b[{number}~").as_bytes(),
            KeyCode::F(function),
            KeyModifiers::NONE,
            KeyEventKind::Press,
        );
    }
}

#[test]
fn high_function_keys_preserve_alternate_codes_modifiers_and_kinds() {
    // Exercise both parser fixes together, including zero-valued alternatives.
    for (number, function) in [
        (25, 13),
        (26, 14),
        (28, 15),
        (29, 16),
        (31, 17),
        (32, 18),
        (33, 19),
        (34, 20),
    ] {
        for alternate in [":0", ":0:0", ":65:97"] {
            for (mask, modifiers) in [
                (1, KeyModifiers::NONE),
                (2, KeyModifiers::SHIFT),
                (3, KeyModifiers::ALT),
                (4, KeyModifiers::SHIFT | KeyModifiers::ALT),
                (5, KeyModifiers::CONTROL),
                (6, KeyModifiers::SHIFT | KeyModifiers::CONTROL),
                (7, KeyModifiers::ALT | KeyModifiers::CONTROL),
                (
                    8,
                    KeyModifiers::SHIFT | KeyModifiers::ALT | KeyModifiers::CONTROL,
                ),
            ] {
                for (suffix, kind) in [
                    ("", KeyEventKind::Press),
                    (":1", KeyEventKind::Press),
                    (":2", KeyEventKind::Repeat),
                    (":3", KeyEventKind::Release),
                ] {
                    let sequence = format!("\x1b[{number}{alternate};{mask}{suffix}~");
                    assert_key(sequence.as_bytes(), KeyCode::F(function), modifiers, kind);
                }
            }
        }
    }
}
