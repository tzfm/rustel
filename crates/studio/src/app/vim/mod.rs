//! Vim outside the editor.
//!
//! One package owns every Vim decision the studio makes outside the
//! score's text engine: key dispatch, panel text surfaces, and list
//! navigation. The generic routing calls this package, never a Vim
//! module directly.
//!
//! [`enabled`] is the settings switch. [`applies`] adds the event rule:
//! a machine's key is a normal key. With either off, no Vim alias, modal
//! key, paste, or focus reset applies.

use super::*;

mod config;
mod dispatch;
mod keys;

pub(super) use dispatch::{
    dispatch, flush_pending, flush_timeout, interrupt, learned_layer_shortcut, mappings,
};
pub(super) use keys::{
    VimTextKey, enter_insert, focus_changed, handle_text_key, panel_takes_list_navigation,
    surface_typing,
};

/// Vim list keys are on, and the editor uses Vim keys.
pub(super) fn enabled(app: &App) -> bool {
    app.ui_settings.vim_list_navigation
        && app.ui_settings.editor_keybindings == crate::editor::EditorKeybindings::Vim
}

/// Vim handles this event. A remote key is a machine's key, so it edits
/// normally even when the local user left a Vim mode behind.
pub(super) fn applies(app: &App) -> bool {
    enabled(app) && !app.remote_key_event
}
