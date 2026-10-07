// Native key mapping and layout fallback adapted from Crossterm 0.29.0,
// src/event/sys/windows/parse.rs, with separate UTF-16 press/release state.
//
// MIT License
//
// Copyright (c) 2019 Timon
//
// Permission is hereby granted, free of charge, to any person obtaining a copy
// of this software and associated documentation files (the "Software"), to deal
// in the Software without restriction, including without limitation the rights
// to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
// copies of the Software, and to permit persons to whom the Software is
// furnished to do so, subject to the following conditions:
//
// The above copyright notice and this permission notice shall be included in all
// copies or substantial portions of the Software.
//
// THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
// IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
// FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
// AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
// LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
// OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
// SOFTWARE.

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};
use windows_sys::Win32::{
    System::Console::{
        CAPSLOCK_ON, KEY_EVENT_RECORD, LEFT_ALT_PRESSED, LEFT_CTRL_PRESSED, NUMLOCK_ON,
        RIGHT_ALT_PRESSED, RIGHT_CTRL_PRESSED, SHIFT_PRESSED,
    },
    UI::{
        Input::KeyboardAndMouse::{
            GetKeyboardLayout, ToUnicodeEx, VK_BACK, VK_CONTROL, VK_DELETE, VK_DOWN, VK_END,
            VK_ESCAPE, VK_F1, VK_F24, VK_HOME, VK_INSERT, VK_LCONTROL, VK_LEFT, VK_LMENU,
            VK_LSHIFT, VK_MENU, VK_NEXT, VK_NUMPAD0, VK_NUMPAD9, VK_PRIOR, VK_RCONTROL, VK_RETURN,
            VK_RIGHT, VK_RMENU, VK_RSHIFT, VK_SHIFT, VK_TAB, VK_UP,
        },
        WindowsAndMessaging::{GetForegroundWindow, GetWindowThreadProcessId},
    },
};

#[derive(Default)]
pub(super) struct NativeKeys {
    // Console input may interleave high-down, high-up, low-down, low-up.
    // Keeping each direction separate prevents duplicated or lost characters.
    high_surrogates: [Option<KEY_EVENT_RECORD>; 2],
    // Paste is text, so its raw UTF-16 must bypass shortcut interpretation and
    // must never complete a surrogate that came from an ordinary key event.
    paste_high_surrogate: Option<KEY_EVENT_RECORD>,
}

impl NativeKeys {
    /// Paste boundaries cannot carry half a character into another gesture.
    pub(super) fn reset_text(&mut self) {
        self.paste_high_surrogate = None;
        self.high_surrogates = [None; 2];
    }

    /// Decodes text committed during bracketed paste. In particular, a literal
    /// LF synthesized as Ctrl+J stays LF, and Alt-code releases still commit.
    pub(super) fn decode_text(&mut self, record: KEY_EVENT_RECORD) -> Option<(char, u16)> {
        let utf16 = unsafe { record.uChar.UnicodeChar };
        let alt_code = matches!(record.wVirtualKeyCode, VK_MENU | VK_LMENU | VK_RMENU)
            && record.bKeyDown == 0
            && utf16 != 0;
        if !alt_code {
            if record.bKeyDown == 0
                || matches!(
                    record.wVirtualKeyCode,
                    VK_SHIFT
                        | VK_LSHIFT
                        | VK_RSHIFT
                        | VK_CONTROL
                        | VK_LCONTROL
                        | VK_RCONTROL
                        | VK_MENU
                        | VK_LMENU
                        | VK_RMENU
                )
            {
                return None;
            }
            let modifiers = modifiers(record.dwControlKeyState);
            if modifiers.contains(KeyModifiers::ALT)
                && !modifiers.intersects(KeyModifiers::SHIFT | KeyModifiers::CONTROL)
                && (VK_NUMPAD0..=VK_NUMPAD9).contains(&record.wVirtualKeyCode)
            {
                return None;
            }
        }
        let character = match utf16 {
            0 => {
                // A native key with no translated character is not paste text.
                self.paste_high_surrogate = None;
                return None;
            }
            0xd800..=0xdbff => {
                self.paste_high_surrogate = Some(record);
                return None;
            }
            0xdc00..=0xdfff => {
                let high = self.paste_high_surrogate.take()?;
                if (high.bKeyDown != 0) != (record.bKeyDown != 0)
                    || !matching_surrogate_metadata(&high, &record)
                {
                    return None;
                }
                combine_surrogates(unsafe { high.uChar.UnicodeChar }, utf16)?
            }
            _ => {
                self.paste_high_surrogate = None;
                char::from_u32(u32::from(utf16))?
            }
        };
        Some((character, record.wRepeatCount.max(1)))
    }

    /// Returns one decoded key and its original repeat count. The input owner
    /// expands repeats lazily and tracks held physical keys across records.
    pub(super) fn decode(&mut self, record: KEY_EVENT_RECORD) -> Option<(KeyEvent, u16)> {
        let utf16 = unsafe { record.uChar.UnicodeChar };
        let down = record.bKeyDown != 0;
        let alt_key = matches!(record.wVirtualKeyCode, VK_MENU | VK_LMENU | VK_RMENU);
        let alt_code = alt_key && !down && utf16 != 0;
        let mut modifiers = modifiers(record.dwControlKeyState);

        if !alt_code {
            if matches!(
                record.wVirtualKeyCode,
                VK_SHIFT
                    | VK_LSHIFT
                    | VK_RSHIFT
                    | VK_CONTROL
                    | VK_LCONTROL
                    | VK_RCONTROL
                    | VK_MENU
                    | VK_LMENU
                    | VK_RMENU
            ) {
                return None;
            }
            if modifiers.contains(KeyModifiers::ALT)
                && !modifiers.intersects(KeyModifiers::SHIFT | KeyModifiers::CONTROL)
                && (VK_NUMPAD0..=VK_NUMPAD9).contains(&record.wVirtualKeyCode)
            {
                // Alt+numpad commits its character on the Alt release.
                return None;
            }
        }

        // Check virtual keys before Unicode: Ctrl+Enter carries LF, but remains
        // a modified Enter, and Ctrl+[ need not be confused with Escape.
        let special = if alt_code {
            None
        } else {
            special_key(record.wVirtualKeyCode, modifiers)
        };
        let code = if let Some(code) = special {
            self.high_surrogates[usize::from(down)] = None;
            code
        } else {
            KeyCode::Char(self.character(record, utf16)?)
        };

        let kind = if down || alt_code {
            KeyEventKind::Press
        } else {
            KeyEventKind::Release
        };
        if alt_code {
            // The Alt chord has finished; this record commits text even though
            // the physical event is key-up. A release would be discarded.
            modifiers.remove(KeyModifiers::ALT);
        }
        let mut state = KeyEventState::empty();
        if record.dwControlKeyState & CAPSLOCK_ON != 0 {
            state |= KeyEventState::CAPS_LOCK;
        }
        if record.dwControlKeyState & NUMLOCK_ON != 0 {
            state |= KeyEventState::NUM_LOCK;
        }
        Some((
            KeyEvent::new_with_kind_and_state(code, modifiers, kind, state),
            record.wRepeatCount.max(1),
        ))
    }

    fn character(&mut self, record: KEY_EVENT_RECORD, utf16: u16) -> Option<char> {
        let pending = &mut self.high_surrogates[usize::from(record.bKeyDown != 0)];
        match utf16 {
            0xd800..=0xdbff => {
                *pending = Some(record);
                None
            }
            0xdc00..=0xdfff => {
                let high = pending.take()?;
                if !matching_surrogate_metadata(&high, &record) {
                    return None;
                }
                let first = unsafe { high.uChar.UnicodeChar };
                combine_surrogates(first, utf16)
            }
            0..=0x1f => {
                *pending = None;
                layout_character(&record)
            }
            _ => {
                *pending = None;
                char::from_u32(u32::from(utf16))
            }
        }
    }
}

fn matching_surrogate_metadata(high: &KEY_EVENT_RECORD, low: &KEY_EVENT_RECORD) -> bool {
    high.wVirtualKeyCode == low.wVirtualKeyCode
        && high.wVirtualScanCode == low.wVirtualScanCode
        && high.dwControlKeyState == low.dwControlKeyState
        && high.wRepeatCount.max(1) == low.wRepeatCount.max(1)
}

fn combine_surrogates(high: u16, low: u16) -> Option<char> {
    char::from_u32(0x10000 + ((u32::from(high) - 0xd800) << 10) + u32::from(low) - 0xdc00)
}

fn modifiers(state: u32) -> KeyModifiers {
    let mut modifiers = KeyModifiers::empty();
    if state & SHIFT_PRESSED != 0 {
        modifiers |= KeyModifiers::SHIFT;
    }
    if state & (LEFT_CTRL_PRESSED | RIGHT_CTRL_PRESSED) != 0 {
        modifiers |= KeyModifiers::CONTROL;
    }
    if state & (LEFT_ALT_PRESSED | RIGHT_ALT_PRESSED) != 0 {
        modifiers |= KeyModifiers::ALT;
    }
    modifiers
}

fn special_key(virtual_key: u16, modifiers: KeyModifiers) -> Option<KeyCode> {
    Some(match virtual_key {
        VK_BACK => KeyCode::Backspace,
        VK_ESCAPE => KeyCode::Esc,
        VK_RETURN => KeyCode::Enter,
        VK_F1..=VK_F24 => KeyCode::F((virtual_key - VK_F1 + 1) as u8),
        VK_LEFT => KeyCode::Left,
        VK_UP => KeyCode::Up,
        VK_RIGHT => KeyCode::Right,
        VK_DOWN => KeyCode::Down,
        VK_PRIOR => KeyCode::PageUp,
        VK_NEXT => KeyCode::PageDown,
        VK_HOME => KeyCode::Home,
        VK_END => KeyCode::End,
        VK_DELETE => KeyCode::Delete,
        VK_INSERT => KeyCode::Insert,
        VK_TAB if modifiers.contains(KeyModifiers::SHIFT) => KeyCode::BackTab,
        VK_TAB => KeyCode::Tab,
        _ => return None,
    })
}

fn layout_character(record: &KEY_EVENT_RECORD) -> Option<char> {
    // UnicodeChar may be zero or a control code for Ctrl+punctuation. Ask for
    // the unmodified key in the terminal's layout, then restore letter case.
    // This matches native Crossterm shortcuts without assuming a US keyboard.
    let keyboard_state = [0_u8; 256];
    let mut utf16 = [0_u16; 16];
    let count = unsafe {
        let window = GetForegroundWindow();
        let thread = GetWindowThreadProcessId(window, std::ptr::null_mut());
        let layout = GetKeyboardLayout(thread);
        ToUnicodeEx(
            u32::from(record.wVirtualKeyCode),
            u32::from(record.wVirtualScanCode),
            keyboard_state.as_ptr(),
            utf16.as_mut_ptr(),
            utf16.len() as i32,
            0x4, // Do not mutate the kernel's dead-key / composition state.
            layout,
        )
    };
    // Negative results represent dead keys. Their spacing accent is not text
    // to insert; the console will deliver the final composed character later.
    if count <= 0 || count as usize > utf16.len() {
        return None;
    }
    let mut decoded = char::decode_utf16(utf16[..count as usize].iter().copied());
    let character = decoded.next()?.ok()?;
    if decoded.next().is_some() {
        return None;
    }
    let uppercase = (record.dwControlKeyState & SHIFT_PRESSED != 0)
        ^ (record.dwControlKeyState & CAPSLOCK_ON != 0);
    let replacement = if uppercase {
        let mut case = character.to_uppercase();
        let first = case.next()?;
        case.next().is_none().then_some(first)
    } else {
        let mut case = character.to_lowercase();
        let first = case.next()?;
        case.next().is_none().then_some(first)
    };
    Some(replacement.unwrap_or(character))
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows_sys::Win32::{
        System::Console::KEY_EVENT_RECORD_0,
        UI::Input::KeyboardAndMouse::{VK_A, VK_J, VK_PACKET},
    };

    fn record(key: u16, unit: u16, down: bool, state: u32, repeat: u16) -> KEY_EVENT_RECORD {
        KEY_EVENT_RECORD {
            bKeyDown: i32::from(down),
            wRepeatCount: repeat,
            wVirtualKeyCode: key,
            wVirtualScanCode: 0,
            uChar: KEY_EVENT_RECORD_0 { UnicodeChar: unit },
            dwControlKeyState: state,
        }
    }

    #[test]
    fn modified_enter_keeps_virtual_key_modifiers_and_release() {
        let mut keys = NativeKeys::default();
        for (state, expected) in [
            (LEFT_CTRL_PRESSED, KeyModifiers::CONTROL),
            (RIGHT_CTRL_PRESSED, KeyModifiers::CONTROL),
            (SHIFT_PRESSED, KeyModifiers::SHIFT),
        ] {
            for (down, kind) in [(true, KeyEventKind::Press), (false, KeyEventKind::Release)] {
                let (key, count) = keys.decode(record(VK_RETURN, 10, down, state, 1)).unwrap();
                assert_eq!(key, KeyEvent::new_with_kind(KeyCode::Enter, expected, kind));
                assert_eq!(count, 1);
            }
        }
    }

    #[test]
    fn unicode_press_and_release_surrogates_can_interleave() {
        let mut keys = NativeKeys::default();
        let state = SHIFT_PRESSED | RIGHT_ALT_PRESSED;
        assert!(
            keys.decode(record(VK_PACKET, 0xd83d, true, state, 3))
                .is_none()
        );
        assert!(
            keys.decode(record(VK_PACKET, 0xd83d, false, 0, 1))
                .is_none()
        );
        let (press, repeats) = keys
            .decode(record(VK_PACKET, 0xde80, true, state, 3))
            .unwrap();
        let (release, _) = keys.decode(record(VK_PACKET, 0xde80, false, 0, 1)).unwrap();
        assert_eq!(press.code, KeyCode::Char('🚀'));
        assert_eq!(press.kind, KeyEventKind::Press);
        assert_eq!(press.modifiers, KeyModifiers::SHIFT | KeyModifiers::ALT);
        assert_eq!(repeats, 3);
        assert_eq!(release.code, KeyCode::Char('🚀'));
        assert_eq!(release.kind, KeyEventKind::Release);
        assert_eq!(release.modifiers, KeyModifiers::empty());
    }

    #[test]
    fn surrogate_pair_requires_matching_native_metadata() {
        let mut keys = NativeKeys::default();
        assert!(keys.decode(record(VK_PACKET, 0xd83d, true, 0, 1)).is_none());
        assert!(
            keys.decode(record(VK_PACKET, 0xde80, true, SHIFT_PRESSED, 1))
                .is_none()
        );
        assert!(keys.decode(record(VK_PACKET, 0xde80, true, 0, 1)).is_none());
        assert!(keys.decode(record(VK_PACKET, 0xd83d, true, 0, 1)).is_none());
        assert!(keys.decode(record(VK_PACKET, 0xde80, true, 0, 2)).is_none());
    }

    #[test]
    fn repeat_count_is_preserved_for_lazy_expansion() {
        let mut keys = NativeKeys::default();
        let (key, repeats) = keys
            .decode(record(VK_A, u16::from(b'a'), true, 0, 7))
            .unwrap();
        assert_eq!(key.code, KeyCode::Char('a'));
        assert_eq!(key.kind, KeyEventKind::Press);
        assert_eq!(repeats, 7);
        assert_eq!(
            keys.decode(record(VK_A, u16::from(b'a'), true, 0, 0))
                .unwrap()
                .1,
            1
        );
    }

    #[test]
    fn alt_code_release_commits_text_and_suppresses_numpad_digits() {
        let mut keys = NativeKeys::default();
        assert!(
            keys.decode(record(
                VK_NUMPAD0,
                u16::from(b'0'),
                true,
                LEFT_ALT_PRESSED,
                1
            ))
            .is_none()
        );
        let (key, _) = keys
            .decode(record(VK_MENU, 0xe9, false, LEFT_ALT_PRESSED, 1))
            .unwrap();
        assert_eq!(
            key,
            KeyEvent::new(KeyCode::Char('é'), KeyModifiers::empty())
        );
        // Ctrl+Alt is not Alt-only numeric composition.
        assert!(
            keys.decode(record(
                VK_NUMPAD0,
                u16::from(b'0'),
                true,
                LEFT_ALT_PRESSED | LEFT_CTRL_PRESSED,
                1
            ))
            .is_some()
        );
    }

    #[test]
    fn alt_code_surrogate_release_commits_one_character() {
        let mut keys = NativeKeys::default();
        assert!(keys.decode(record(VK_MENU, 0xd83d, false, 0, 1)).is_none());
        let (key, _) = keys.decode(record(VK_MENU, 0xde80, false, 0, 1)).unwrap();
        assert_eq!(
            key,
            KeyEvent::new(KeyCode::Char('🚀'), KeyModifiers::empty())
        );
    }

    #[test]
    fn shift_tab_and_modifier_only_records_keep_native_semantics() {
        let mut keys = NativeKeys::default();
        assert!(
            keys.decode(record(VK_SHIFT, 0, true, SHIFT_PRESSED, 1))
                .is_none()
        );
        let (key, _) = keys
            .decode(record(VK_TAB, 9, true, SHIFT_PRESSED, 1))
            .unwrap();
        assert_eq!(key, KeyEvent::new(KeyCode::BackTab, KeyModifiers::SHIFT));
    }

    #[test]
    fn pasted_native_text_keeps_linefeeds_and_alt_code_surrogates() {
        let mut keys = NativeKeys::default();
        let records = [
            record(VK_A, u16::from(b'a'), true, 0, 1),
            record(VK_A, u16::from(b'a'), false, 0, 1),
            record(VK_J, 10, true, LEFT_CTRL_PRESSED, 1),
            record(VK_J, 10, false, LEFT_CTRL_PRESSED, 1),
            record(VK_MENU, 0xd83d, false, 0, 1),
            record(VK_MENU, 0, true, LEFT_ALT_PRESSED, 1),
            record(VK_NUMPAD0, u16::from(b'0'), true, LEFT_ALT_PRESSED, 1),
            record(VK_NUMPAD0, u16::from(b'0'), false, LEFT_ALT_PRESSED, 1),
            record(VK_MENU, 0xde80, false, 0, 1),
            record(VK_SHIFT, 0, true, SHIFT_PRESSED, 1),
            record(VK_SHIFT, 0, false, 0, 1),
            record(VK_TAB, 9, true, 0, 2),
        ];
        let mut text = String::new();
        for record in records {
            if let Some((character, count)) = keys.decode_text(record) {
                text.extend(std::iter::repeat_n(character, usize::from(count)));
            }
        }
        assert_eq!(text, "a\n🚀\t\t");
    }

    #[test]
    fn pasted_surrogates_ignore_duplicate_native_releases() {
        let mut keys = NativeKeys::default();
        assert!(
            keys.decode_text(record(VK_PACKET, 0xd83d, true, 0, 2))
                .is_none()
        );
        assert!(
            keys.decode_text(record(VK_PACKET, 0xd83d, false, 0, 1))
                .is_none()
        );
        assert_eq!(
            keys.decode_text(record(VK_PACKET, 0xde80, true, 0, 2)),
            Some(('🚀', 2))
        );
        assert!(
            keys.decode_text(record(VK_PACKET, 0xde80, false, 0, 1))
                .is_none()
        );
    }

    #[test]
    fn pasted_surrogates_do_not_cross_text_or_paste_boundaries() {
        let mut keys = NativeKeys::default();
        assert!(keys.decode(record(VK_PACKET, 0xd83d, true, 0, 1)).is_none());
        assert!(
            keys.decode_text(record(VK_PACKET, 0xde80, true, 0, 1))
                .is_none()
        );
        assert!(
            keys.decode_text(record(VK_PACKET, 0xd83d, true, 0, 1))
                .is_none()
        );
        assert_eq!(
            keys.decode_text(record(VK_A, u16::from(b'a'), true, 0, 1)),
            Some(('a', 1))
        );
        assert!(
            keys.decode_text(record(VK_PACKET, 0xde80, true, 0, 1))
                .is_none()
        );
        assert!(
            keys.decode_text(record(VK_PACKET, 0xd83d, true, 0, 1))
                .is_none()
        );
        keys.reset_text();
        assert!(
            keys.decode_text(record(VK_PACKET, 0xde80, true, 0, 1))
                .is_none()
        );
    }
}
