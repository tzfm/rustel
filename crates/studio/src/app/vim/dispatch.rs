//! Vim key dispatch outside the editor.
//!
//! The editor owns the text. This module owns every decision the studio
//! makes before the editor sees a Vim key: who else has the keyboard, the
//! mapping queue, and the built-in key. The generic editor glue only asks
//! this module and never reads a Vim mode itself.

use super::super::*;

/// A reason a key must not reach the Vim engine. Add a rule to the stage
/// where it belongs instead of adding an if to [`dispatch`].
type Bypass = fn(&App, &KeyEvent) -> bool;

/// Keys another surface owns before mappings are considered.
const BEFORE_MAPPINGS: &[Bypass] = &[remote_control_key, remote_sheet_key];

/// Keys that fall through after mappings had their chance.
const AFTER_MAPPINGS: &[Bypass] = &[learned_layer_shortcut, panel_escape];

/// Route one key through the Vim engine. `false` sends it to the ordinary
/// studio routing.
pub(crate) fn dispatch(app: &mut App, key: KeyEvent) -> Result<bool, RuntimeError> {
    if BEFORE_MAPPINGS.iter().any(|rule| rule(app, &key)) {
        return Ok(false);
    }
    let moment = app.moment();
    if app
        .scenes
        .current_mut()
        .editor
        .vim_queue_mapped_key(key, moment)
    {
        drain_mappings(app, false)?;
        app.dirty_frame = true;
        return Ok(true);
    }
    if AFTER_MAPPINGS.iter().any(|rule| rule(app, &key)) {
        return Ok(false);
    }
    dispatch_builtin(app, key)
}

/// A machine sends normal keys, not Vim modes.
fn remote_control_key(app: &App, _key: &KeyEvent) -> bool {
    app.remote_key_event
}

/// The remote sheet has the keyboard while it is open: its address and
/// token are typed into, and its rows answer to j/k. Vim waits.
fn remote_sheet_key(app: &App, _key: &KeyEvent) -> bool {
    #[cfg(feature = "remote-control")]
    {
        app.remote_panel_open
    }
    #[cfg(not(feature = "remote-control"))]
    {
        let _ = app;
        false
    }
}

/// A learnt studio chord on a key the Vim layer would take wins, as the
/// player asked. A pending Vim command keeps its key.
fn learned_layer_shortcut(app: &App, key: &KeyEvent) -> bool {
    key.kind == KeyEventKind::Press
        && !app.editor().vim_has_pending_command()
        && app.keybinds.override_action_for(key).is_some()
        && app.keybinds.takes_vim_layer_key(key)
}

/// Vim Normal does not eat the Esc that puts an open panel away. A sheet
/// left behind the editor is otherwise impossible to close.
fn panel_escape(app: &App, key: &KeyEvent) -> bool {
    key.code == KeyCode::Esc
        && key.modifiers.is_empty()
        && app.focus == Focus::Editor
        && app.front_panel().is_some()
        && app.editor().vim_is_normal()
        && !app.editor().vim_has_pending_command()
}

pub(crate) fn flush_timeout(app: &mut App) -> Result<(), RuntimeError> {
    if app.editor().vim_mapping_timed_out(app.moment()) {
        flush_pending(app)?;
    }
    Ok(())
}

pub(crate) fn flush_pending(app: &mut App) -> Result<(), RuntimeError> {
    drain_mappings(app, true)
}

/// Leave Vim before an action that did not come from the keyboard. A held
/// mapping prefix or Insert must not change what a pad or knob does.
pub(crate) fn interrupt(app: &mut App) -> Result<(), RuntimeError> {
    flush_pending(app)?;
    for scene in app.scenes.scenes_mut() {
        scene.editor.interrupt_key_mode();
    }
    Ok(())
}

fn drain_mappings(app: &mut App, force: bool) -> Result<(), RuntimeError> {
    while let Some((mapped, remaining)) =
        app.scenes.current_mut().editor.vim_take_mapped_keys(force)
    {
        for step in mapped {
            match step {
                crate::vim_config::VimMappedKey::Key(key) => {
                    if app.editor_accepts_raw_paste() && dispatch_builtin(app, key)? {
                        if app.editor().vim_motion_blocked() {
                            break;
                        }
                    } else {
                        app.route_terminal_event(Event::Key(key), false)?;
                    }
                }
                crate::vim_config::VimMappedKey::Action(action) => {
                    // A Studio action is not a Vim motion. A pending
                    // operator must not wait for the next key.
                    app.editor_mut().vim_cancel_pending();
                    app.dispatch_bind_action(action, false)?;
                }
            }
        }
        for key in remaining {
            app.handle_terminal_event(Event::Key(key))?;
        }
    }
    app.refresh_editor_shortcut_hints();
    Ok(())
}

fn dispatch_builtin(app: &mut App, key: KeyEvent) -> Result<bool, RuntimeError> {
    let kind = key.kind;
    let mut key = crate::vim_config::canonical_key(key);
    key.kind = kind;
    let pending = app.editor().vim_has_pending_command();
    // The Vim layer gives an action a key of its own, such as K for Docs.
    // Mappings and learnt studio chords have already had their chance.
    if !pending
        && key.kind == KeyEventKind::Press
        && let Some(action) = app.keybinds.vim_layer_action_for(&key)
        && !app.editor().vim_is_insert()
    {
        app.dispatch_bind_action(action, false)?;
        app.dirty_frame = true;
        return Ok(true);
    }
    // Vim p puts what was copied last. The clipboard answers the key, and
    // a yank or delete writes the register there, so a copied score and a
    // yanked line take the same road.
    if !pending
        && key.kind == KeyEventKind::Press
        && key.modifiers.is_empty()
        && matches!(key.code, KeyCode::Char('p' | 'P'))
        && app.editor().vim_accepts_put()
    {
        let register = app.editor().vim_register().map(str::to_owned);
        let clipboard = app
            .clipboard
            .get_text()
            .ok()
            .filter(|text| !text.is_empty());
        // A copy from another surface is newer than the register. When the
        // two agree, Vim's own put keeps its linewise and visual rules.
        if clipboard
            .as_deref()
            .is_some_and(|text| register.as_deref() != Some(text))
        {
            app.dispatch_editor(Command::Paste)?;
            app.focus = Focus::Editor;
            app.dirty_frame = true;
            return Ok(true);
        }
    }
    let register = app.editor().vim_register().map(str::to_owned);
    let before = app.editor().revision();
    let previous_caret = app.editor().primary_selection().head.0;
    let moment = app.moment();
    let atomic_delete_range = if !app.editor().vim_is_insert()
        && matches!(key.code, KeyCode::Char('x') | KeyCode::Delete)
    {
        app.slider_delete_range(&Command::DeleteForward)
    } else {
        None
    };
    let outcome = app
        .scenes
        .current_mut()
        .editor
        .vim_key(key, moment, atomic_delete_range);
    let mut handled = match outcome {
        Ok(handled) => {
            if handled {
                app.clear_error(ErrorOwner::Editor);
            }
            handled
        }
        Err(error) => {
            app.set_error(ErrorOwner::Editor, error.to_string());
            true
        }
    };
    if !handled && pending {
        app.scenes.current_mut().editor.vim_cancel_pending();
        handled = true;
    }
    if !handled && let Some(command) = app.editor().vim_insert_command(key) {
        app.dispatch_editor(command)?;
        return Ok(true);
    }
    if handled {
        // A yank or delete fills the register. Keep the clipboard in step,
        // so p pastes the newest copy from any surface.
        let register_now = app.editor().vim_register().map(str::to_owned);
        if register_now.as_deref() != register.as_deref()
            && let Some(text) = register_now
        {
            let _ = app.clipboard.set_text(text);
        }
        if app.editor().revision() != before || app.editor().vim_needs_escape() {
            app.focus = Focus::Editor;
        }
        if app.editor().revision() != before {
            #[cfg(feature = "hydra")]
            app.sync_settings_webcam_preview();
            app.armed_slider = None;
            app.drop_pane_selection();
        }
        app.after_document_change(before, true);
        app.warp_caret_over_sliders(previous_caret);
        app.dirty_frame = true;
    }
    Ok(handled)
}

/// The parsed mappings every editor follows. The settings fan-out stays
/// generic; the Vim package owns only the mapping source.
pub(crate) fn mappings(app: &App) -> std::sync::Arc<crate::vim_config::VimMappings> {
    app.vim_config.mappings()
}
