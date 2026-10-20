//! Insert mappings for Vim text surfaces.
//!
//! The editor owns the score's Vim engine. A panel search is a text field
//! of its own, so it reads the same mapping file without the editor state:
//! plain character mappings buffer here, a match inserts its replacement
//! as text, and a mapping to Esc leaves Insert. No key is named in this
//! file; the vimrc is the whole table.

use super::*;
use crate::vim_config::{VimMappedKey, VimMappingMode};

struct TextMapping {
    keys: String,
    replacement: Vec<VimMappedKey>,
}

/// The mapping as plain characters, or `None` when it needs a key this
/// text field cannot read.
fn text_mapping(mapping: &crate::vim_config::VimMapping) -> Option<TextMapping> {
    let mut keys = String::new();
    for key in &mapping.keys {
        match key.code {
            KeyCode::Char(character)
                if !character.is_control()
                    && !key.modifiers.intersects(
                        KeyModifiers::CONTROL
                            | KeyModifiers::SUPER
                            | KeyModifiers::ALT
                            | KeyModifiers::HYPER
                            | KeyModifiers::META,
                    ) =>
            {
                keys.push(character);
            }
            _ => return None,
        }
    }
    (!keys.is_empty()).then_some(TextMapping {
        keys,
        replacement: mapping.replacement.clone(),
    })
}

/// Feed one Insert key. Answers true when the key was consumed. A key that
/// starts no mapping is typed at once; a prefix waits for the rest.
pub(super) fn insert_key(app: &mut App, key: &KeyEvent) -> bool {
    let character = match key.code {
        KeyCode::Char(character)
            if !character.is_control()
                && !key.modifiers.intersects(
                    KeyModifiers::CONTROL
                        | KeyModifiers::SUPER
                        | KeyModifiers::ALT
                        | KeyModifiers::HYPER
                        | KeyModifiers::META,
                ) =>
        {
            character
        }
        _ => {
            flush(app);
            return false;
        }
    };
    app.vim_insert_pending.push(character);
    let mappings = app.vim_config.mappings();
    let mut longer = false;
    let mut exact = None;
    for mapping in mappings.for_mode(VimMappingMode::Insert) {
        let Some(mapping) = text_mapping(mapping) else {
            continue;
        };
        if !mapping.keys.starts_with(&app.vim_insert_pending) {
            continue;
        }
        if mapping.keys.len() > app.vim_insert_pending.len() {
            longer = true;
        } else if exact.is_none() {
            exact = Some(mapping);
        }
    }
    if longer {
        return true;
    }
    if let Some(mapping) = exact {
        app.vim_insert_pending.clear();
        apply(app, &mapping.replacement);
    } else {
        flush(app);
    }
    true
}

/// Put the keys waiting for a mapping into the focused text field.
pub(super) fn flush(app: &mut App) {
    if app.vim_insert_pending.is_empty() {
        return;
    }
    let text = std::mem::take(&mut app.vim_insert_pending);
    let _ = app.paste_to_prompt(&text);
    app.dirty_frame = true;
}

pub(super) fn reset(app: &mut App) {
    app.vim_insert_pending.clear();
}

fn apply(app: &mut App, replacement: &[VimMappedKey]) {
    let mut text = String::new();
    for mapped in replacement {
        match mapped {
            VimMappedKey::Key(key) => match key.code {
                KeyCode::Char(character) if !character.is_control() => text.push(character),
                KeyCode::Esc => {
                    if !text.is_empty() {
                        let _ = app.paste_to_prompt(&text);
                        text.clear();
                    }
                    super::vim_keys::leave_insert(app);
                    return;
                }
                _ => {}
            },
            VimMappedKey::Action(_) => {}
        }
    }
    if !text.is_empty() {
        let _ = app.paste_to_prompt(&text);
    }
    app.dirty_frame = true;
}
