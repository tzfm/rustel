//! Vim outside the editor.
//!
//! One package owns every Vim decision the studio makes outside the
//! score's text engine: key dispatch, panel text surfaces, insert
//! mappings, and list navigation. The generic routing calls this package,
//! never a Vim module directly.
//!
//! [`enabled`] is the one switch. It off, no alias, modal key, hint,
//! paste, or focus reset applies anywhere.

use super::*;

mod config;
mod dispatch;
mod keys;
mod mappings;

pub(super) use dispatch::{dispatch, flush_pending, flush_timeout, sync};
pub(super) use keys::{
    VimTextKey, enter_insert, focus_changed, handle_text_key, panel_takes_list_navigation,
    surface_typing,
};

/// Vim list keys are on, and the editor uses Vim keys.
pub(super) fn enabled(app: &App) -> bool {
    app.ui_settings.vim_list_navigation
        && app.ui_settings.editor_keybindings == crate::editor::EditorKeybindings::Vim
}
