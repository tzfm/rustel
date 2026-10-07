//! Key handling that several panels share for lists and file pickers.
//! `picker_key` gives every file picker (the set panel, the visuals panel) the
//! same keys: arrows walk the list, typing edits the path field, Tab opens the
//! folder browser, Enter chooses and Esc backs out. It returns a `PickerKey`
//! outcome. `page_selection` moves a list selection by a page with
//! PageUp/PageDown and stops at either end instead of wrapping.

use super::*;

/// What a key did to a picker.
pub(super) enum PickerKey {
    /// Not the picker's key.
    Ignored,
    /// Moved, typed, or otherwise changed the picker.
    Handled,
    /// Esc from the top: the picker is done.
    Back,
    /// Enter: the reader chose this path.
    Chose(std::path::PathBuf),
}

/// The keys every picker answers the same way: arrows walk the list, a
/// printable character starts or continues the path, Backspace edits it,
/// Tab opens the folder browser (at the typed path when there is one) or
/// closes it, and in the browser Enter goes into a folder, Backspace goes
/// up. Enter chooses; Esc goes back - out of the browser or the path field
/// first, then out of the picker.
pub(super) fn picker_key(
    picker: &mut FilePicker,
    code: KeyCode,
    primary: bool,
    shift: bool,
    frame: Rect,
) -> PickerKey {
    picker.cancel_click();
    let before = picker.scroll_signature();
    let page = picker
        .geometry(frame)
        .map_or(1, |(_, list)| usize::from(list.height).max(1));
    let outcome = picker_key_inner(picker, code, primary, shift, page);
    // A key that moved the selection, or the folder or the list it is in,
    // walks the list the keyboard's way; typing in the path field leaves a
    // clicked row where the pointer left it.
    if picker.scroll_signature() != before {
        picker.hold_scroll = false;
    }
    outcome
}

/// Paging stops at an endpoint instead of wrapping to the other end.
pub(super) fn page_selection(selected: usize, count: usize, rows: usize, down: bool) -> usize {
    let last = count.saturating_sub(1);
    let selected = selected.min(last);
    if down {
        selected.saturating_add(rows.max(1)).min(last)
    } else {
        selected.saturating_sub(rows.max(1))
    }
}

fn picker_key_inner(
    picker: &mut FilePicker,
    code: KeyCode,
    primary: bool,
    shift: bool,
    page: usize,
) -> PickerKey {
    if picker.browsing() {
        match code {
            KeyCode::Esc | KeyCode::Tab => {
                picker.toggle_browsing();
                return PickerKey::Handled;
            }
            KeyCode::Enter if !primary => {
                let browser = picker.browser.as_mut().expect("browsing");
                return match browser.enter() {
                    Some(path) => PickerKey::Chose(path),
                    None => PickerKey::Handled,
                };
            }
            KeyCode::Up => {
                picker.browser.as_mut().expect("browsing").move_by(-1);
                return PickerKey::Handled;
            }
            KeyCode::Down => {
                picker.browser.as_mut().expect("browsing").move_by(1);
                return PickerKey::Handled;
            }
            KeyCode::PageUp | KeyCode::PageDown => {
                let browser = picker.browser.as_mut().expect("browsing");
                browser.selected = page_selection(
                    browser.selected,
                    browser.entries.len(),
                    page,
                    code == KeyCode::PageDown,
                );
                return PickerKey::Handled;
            }
            KeyCode::Backspace => {
                picker.browser.as_mut().expect("browsing").ascend();
                return PickerKey::Handled;
            }
            KeyCode::Home => {
                picker.browser.as_mut().expect("browsing").selected = 0;
                return PickerKey::Handled;
            }
            KeyCode::End => {
                let browser = picker.browser.as_mut().expect("browsing");
                browser.selected = browser.entries.len().saturating_sub(1);
                return PickerKey::Handled;
            }
            _ => {}
        }
    }
    match code {
        KeyCode::Esc => {
            if picker.typing && picker.stop_typing() {
                PickerKey::Handled
            } else {
                PickerKey::Back
            }
        }
        KeyCode::Tab if picker.browsable => {
            picker.toggle_browsing();
            PickerKey::Handled
        }
        KeyCode::Enter if !primary => match picker.choice() {
            Some(PickerChoice::Candidate(path) | PickerChoice::Typed(path)) => {
                PickerKey::Chose(path)
            }
            None => PickerKey::Handled,
        },
        KeyCode::Up if !picker.typing => {
            picker.move_by(-1);
            PickerKey::Handled
        }
        KeyCode::Down if !picker.typing => {
            picker.move_by(1);
            PickerKey::Handled
        }
        KeyCode::PageUp | KeyCode::PageDown => {
            if !picker.typing {
                picker.selected = page_selection(
                    picker.selected,
                    picker.candidates.len(),
                    page,
                    code == KeyCode::PageDown,
                );
            }
            PickerKey::Handled
        }
        // The field's own caret: arrows walk it, Shift with them selects,
        // Home and End go to the ends, Ctrl+A takes it all.
        KeyCode::Left if picker.typing => {
            picker.move_caret(-1, shift);
            PickerKey::Handled
        }
        KeyCode::Right if picker.typing => {
            picker.move_caret(1, shift);
            PickerKey::Handled
        }
        KeyCode::Home if picker.typing => {
            picker.home(shift);
            PickerKey::Handled
        }
        KeyCode::End if picker.typing => {
            picker.end(shift);
            PickerKey::Handled
        }
        KeyCode::Char('a' | 'A') if primary && picker.typing => {
            picker.select_all();
            PickerKey::Handled
        }
        KeyCode::Backspace if picker.typing => {
            picker.pop();
            PickerKey::Handled
        }
        KeyCode::Delete if picker.typing => {
            picker.delete_forward();
            PickerKey::Handled
        }
        KeyCode::Char(character) if !primary && !character.is_control() => {
            // A letter while browsing is a path being typed: the browser
            // gives way to the field.
            if picker.browsing() {
                picker.toggle_browsing();
            }
            if !picker.typing {
                picker.start_typing();
            }
            picker.push(character);
            PickerKey::Handled
        }
        _ => PickerKey::Ignored,
    }
}
