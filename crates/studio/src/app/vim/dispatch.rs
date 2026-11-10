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
pub(crate) fn learned_layer_shortcut(app: &App, key: &KeyEvent) -> bool {
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
    leave_key_modes(app);
    Ok(())
}

/// Leave Visual and drop a pending command in every score, as a studio
/// chord does before its action.
fn leave_key_modes(app: &mut App) {
    for scene in app.scenes.scenes_mut() {
        scene.editor.interrupt_key_mode();
    }
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
                    // A Studio action is not a Vim motion. It leaves Vim
                    // as its chord does, so a pending operator does not
                    // wait for the next key and Visual does not outlive
                    // an edit.
                    leave_key_modes(app);
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
        // A copy from another surface is newer than the register, so it
        // becomes the register. Vim's own put then keeps its linewise and
        // visual rules for it.
        if let Some(text) = clipboard.filter(|text| register.as_deref() != Some(text.as_str())) {
            app.editor_mut().set_vim_register(text);
        }
    }
    let register_writes = app.editor().vim_register_writes();
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
        // Typing lets a clicked slider go, as it does outside Vim.
        if edits_document(&command) {
            app.armed_slider = None;
        }
        app.dispatch_editor(command)?;
        return Ok(true);
    }
    if handled {
        // A yank or delete fills the register. Keep the clipboard in step,
        // so p pastes the newest copy from any surface, even when the same
        // text was yanked before.
        if app.editor().vim_register_writes() != register_writes
            && let Some(text) = app.editor().vim_register().map(str::to_owned)
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::tests::support::*;
    use crate::editor::{Editor, Selection};
    use crate::vim_config::VimMappings;

    /// A score in Vim Normal with the caret at its start, beneath the
    /// shipped mappings and `vimrc`.
    fn vim_app(directory: &std::path::Path, source: &str, vimrc: &str) -> App {
        let mut app = app_over(directory);
        app.scenes.current_mut().editor = Editor::new(source).expect("editor");
        app.ui_settings.editor_keybindings = crate::editor::EditorKeybindings::Vim;
        app.vim_config
            .set_mappings_for_test(VimMappings::parse_with_defaults(vimrc).expect("mappings"));
        app.sync_editor_keybindings();
        app.editor_mut()
            .set_selection(Selection::caret(ByteOffset(0)))
            .expect("caret");
        app.focus = Focus::Editor;
        app
    }

    fn typed(app: &mut App, keys: &str) {
        for character in keys.chars() {
            let modifiers = if character.is_ascii_uppercase() {
                KeyModifiers::SHIFT
            } else {
                KeyModifiers::NONE
            };
            press(app, KeyCode::Char(character), modifiers);
        }
    }

    /// A mapped panel action runs as its chord does where the keyboard
    /// is, learnt chord or not, and says so when it has no use here.
    #[test]
    fn a_mapped_panel_action_runs_as_its_chord_does() {
        let directory = tempfile::tempdir().expect("temp dir");
        let mut app = vim_app(
            directory.path(),
            "$: s(\"sine\")\n",
            "nnoremap <Space>T <Action:focus-timeline>\n",
        );
        let tape = write_tape(
            directory.path(),
            "2026-09-08T12-00-00",
            &[(0.0, "$: s(\"sine\")"), (2.0, "$: s(\"sine\") // two")],
        );
        app.open_replay_tab(&tape);
        app.sync_editor_keybindings();
        app.frame = Rect::new(0, 0, 120, 30);
        let mut regions = view::regions(app.frame, false, 1, false, false, None, None);
        app.split_replay_timelines(&mut regions);
        app.regions = regions;
        assert!(app.timeline_visible(), "the tape shows its timeline");

        app.focus = Focus::Editor;
        typed(&mut app, " T");
        assert_eq!(app.focus, Focus::Timeline, "Space T focuses the timeline");

        let chord = crate::keybinds::KeyCombo::parse("ctrl+shift+f8").expect("chord");
        app.learn_keybind(BindAction::FocusTimeline, Some(chord));
        app.focus = Focus::Editor;
        typed(&mut app, " T");
        assert_eq!(
            app.focus,
            Focus::Timeline,
            "the mapping follows the learnt chord"
        );

        app.focus = Focus::Editor;
        app.regions.panes[0].timeline = Rect::default();
        typed(&mut app, " T");
        assert_eq!(app.focus, Focus::Editor);
        assert!(
            app.status.contains(BindAction::FocusTimeline.label()),
            "{}",
            app.status
        );
    }

    /// A mapped action leaves Visual as its chord does. `Vjgc` and
    /// `Vj` with Ctrl+/ end in the same text, mode and caret, and so do
    /// the keys after them.
    #[test]
    fn a_mapped_comment_leaves_visual_as_its_chord_does() {
        let directory = tempfile::tempdir().expect("temp dir");
        let run = |source: &str, mapped: bool, after: &[(KeyCode, KeyModifiers)]| {
            let mut app = vim_app(directory.path(), source, "");
            typed(&mut app, "Vj");
            if mapped {
                typed(&mut app, "gc");
            } else {
                press(&mut app, KeyCode::Char('/'), KeyModifiers::CONTROL);
            }
            let mut seen = vec![(
                app.editor().source(),
                app.editor().vim_label(),
                app.editor().primary_selection(),
            )];
            for (code, modifiers) in after {
                press(&mut app, *code, *modifiers);
                seen.push((
                    app.editor().source(),
                    app.editor().vim_label(),
                    app.editor().primary_selection(),
                ));
            }
            assert!(
                app.errors.get(ErrorOwner::Editor).is_none(),
                "{:?}",
                app.errors.get(ErrorOwner::Editor)
            );
            seen
        };
        let keys = [
            (KeyCode::Char('j'), KeyModifiers::NONE),
            (KeyCode::Esc, KeyModifiers::NONE),
            (KeyCode::Char('x'), KeyModifiers::NONE),
        ];
        let chord = run("a\nb\nc\n", false, &keys);
        assert_eq!(
            chord[0].0, "// a\n// b\nc\n",
            "the chord comments two lines"
        );
        assert_eq!(run("a\nb\nc\n", true, &keys), chord);

        let keys = [
            (KeyCode::Char('d'), KeyModifiers::CONTROL),
            (KeyCode::Char('x'), KeyModifiers::NONE),
        ];
        let chord = run("// a\n// b\n", false, &keys);
        assert_eq!(chord[0].0, "a\nb\n", "the chord uncomments two lines");
        assert_eq!(run("// a\n// b\n", true, &keys), chord);
    }

    /// A yank is the newest copy even when the register already holds the
    /// same text: a copy made elsewhere in between must not win the put.
    #[test]
    fn the_same_yank_again_is_the_newest_copy() {
        let directory = tempfile::tempdir().expect("temp dir");
        let mut app = vim_app(
            directory.path(),
            "x
y
",
            "",
        );
        typed(&mut app, "yy");
        assert_eq!(app.clipboard.get_text().expect("yank"), "x\n");
        app.clipboard
            .set_text("foo".into())
            .expect("a copy elsewhere");
        typed(&mut app, "yy");
        assert_eq!(
            app.clipboard.get_text().expect("yank again"),
            "x\n",
            "the second yank did not reach the clipboard"
        );
        typed(&mut app, "p");
        assert_eq!(app.editor().source(), "x\nx\ny\n");
    }
}
