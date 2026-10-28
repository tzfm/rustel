//! Glue between the studio and the text editor. Keys and mouse events are
//! passed to the focused score's editor here, and so is the bookkeeping after
//! an edit: marking the scene dirty, scheduling lint, refreshing virtual rows
//! and moving the caret past rendered sliders. The editor's Update and Stop
//! effects are also turned into saves, evaluations and transport stops here.

use super::*;

impl App {
    pub(super) fn sync_editor_keybindings(&mut self) {
        self.keybinds
            .set_editor_keybindings(self.ui_settings.editor_keybindings);
        let mappings = super::vim::mappings(self);
        for scene in self.scenes.scenes_mut() {
            scene
                .editor
                .set_keybindings(self.ui_settings.editor_keybindings);
            scene
                .editor
                .set_vim_mappings(std::sync::Arc::clone(&mappings));
        }
        self.refresh_editor_shortcut_hints();
    }

    pub(super) fn editor_cursor_shape(&self) -> super::super::terminal::CaretShape {
        use super::super::terminal::CaretShape;
        // Vim shapes follow the mode: a block in Normal and Visual, a bar
        // in Insert. The setting and its blink do not apply, so typing
        // cannot restart a blink phase on every key.
        if self.editor().uses_modal_keys() {
            return if self.editor().accepts_typed_text() {
                CaretShape::SteadyBar
            } else {
                CaretShape::SteadyBlock
            };
        }
        if self.editor_accepts_raw_paste() && !self.editor().accepts_typed_text() {
            CaretShape::SteadyBlock
        } else {
            self.theme.caret_shape(self.ui_settings.caret_shape)
        }
    }

    /// Typed text has a place in the editor when the editor accepts it, or
    /// when a machine sent it. A remote key is literal text in every mode.
    pub(super) fn editor_takes_typed_text(&self) -> bool {
        self.remote_key_event || self.editor().accepts_typed_text()
    }

    /// Leave any pending key mode before an action from outside the
    /// keyboard. The Vim package owns how a mode is left.
    pub(super) fn interrupt_key_input(&mut self) -> Result<(), RuntimeError> {
        super::vim::interrupt(self)
    }

    pub(super) fn forward_mouse_to_editor(
        &mut self,
        mouse: MouseEvent,
    ) -> Result<(), RuntimeError> {
        let Some(map_area) = self.focused_map().map(|map| map.area()) else {
            return Ok(());
        };
        let before = self.editor().revision();
        let viewport_before = self.editor().viewport();
        let previous_caret = self.editor().primary_selection().head.0;
        let moment = self.moment();
        let outcome = {
            let map = self.panes[self.focused]
                .last_map
                .as_ref()
                .expect("checked above");
            self.scenes
                .current_mut()
                .editor
                .mouse_event(mouse, map, moment)
        };
        let outcome = outcome.and_then(|changed| {
            self.scenes.current_mut().editor.normalize_after_input()?;
            Ok(changed)
        });
        match outcome {
            Ok(changed) => {
                self.clear_error(ErrorOwner::Editor);
                self.dirty_frame |= changed;
            }
            Err(error) => self.set_error(ErrorOwner::Editor, error.to_string()),
        }
        self.after_document_change(before, false);
        self.warp_caret_over_sliders(previous_caret);
        if self.editor().viewport() != viewport_before {
            match self.editor().screen_map(map_area) {
                Ok(map) => self.panes[self.focused].last_map = Some(map),
                Err(error) => {
                    self.invalidate_maps();
                    self.set_error(ErrorOwner::Interface, error.to_string());
                }
            }
        }
        Ok(())
    }

    pub(super) fn dispatch_editor(&mut self, command: Command) -> Result<(), RuntimeError> {
        let may_restore_savepoint = matches!(command, Command::Undo | Command::Redo);
        let before = self.editor().revision();
        let previous_caret = self.editor().primary_selection().head.0;
        let moment = self.moment();
        let outcome =
            self.scenes
                .current_mut()
                .editor
                .dispatch(command, moment, &mut *self.clipboard);
        let outcome = outcome.and_then(|effects| {
            self.scenes.current_mut().editor.normalize_after_input()?;
            Ok(effects)
        });
        match outcome {
            Ok(effects) => {
                self.clear_error(ErrorOwner::Editor);
                for effect in effects {
                    self.handle_editor_effect(effect)?;
                }
            }
            Err(error) => self.set_error(ErrorOwner::Editor, error.to_string()),
        }
        self.after_document_change(before, may_restore_savepoint);
        self.warp_caret_over_sliders(previous_caret);
        self.dirty_frame = true;
        Ok(())
    }

    pub(super) fn after_document_change(&mut self, before: Revision, may_restore_savepoint: bool) {
        let scene = self.scenes.current().id;
        self.after_edit(scene, before, may_restore_savepoint);
    }

    /// A rendered control is one block: a caret that lands strictly inside
    /// its call is warped across it, in the direction it was travelling -
    /// Right from before the widget lands after it, Left the reverse, and a
    /// click takes the nearer edge. What cannot rest inside cannot silently
    /// un-render or edit the call's hidden text.
    pub(super) fn warp_caret_over_sliders(&mut self, previous: usize) {
        let scene_id = self.scenes.current().id;
        let selection = self.editor().primary_selection();
        if !selection.is_empty() {
            return;
        }
        let caret = selection.head.0;
        let Some(chip) = self
            .live_chips(scene_id)
            .into_iter()
            .find(|chip| caret > chip.call.start && caret < chip.span.from)
        else {
            return;
        };
        let target = if previous <= chip.call.start {
            chip.span.from
        } else if previous >= chip.span.from || caret - chip.call.start <= chip.span.from - caret {
            chip.call.start
        } else {
            chip.span.from
        };
        let _ = self
            .scenes
            .current_mut()
            .editor
            .set_selection(super::super::editor::Selection::caret(ByteOffset(target)));
        self.dirty_frame = true;
    }

    /// Bookkeeping after `scene`'s text may have changed.
    pub(super) fn after_edit(
        &mut self,
        scene_id: SceneId,
        before: Revision,
        may_restore_savepoint: bool,
    ) {
        let Some(scene) = self.scenes.get_mut(scene_id) else {
            return;
        };
        if scene.editor.revision() == before {
            return;
        }
        // `visual_revision` deliberately survives: it names the revision
        // the installed layout was evaluated at, and every decoration is
        // followed forward from there rather than discarded.
        if may_restore_savepoint {
            scene.refresh_dirty();
        } else {
            scene.dirty = true;
        }
        self.last_edit_at = Instant::now();
        self.lint_pending = true;
        self.auto_evaluate_due = true;
        self.readiness_due = Some((scene_id, self.last_edit_at + READINESS_SETTLE));
        self.marks_muted = Some(scene_id);
        self.invalidate_maps();
        // A widget whose call was just deleted or commented out gives
        // its rows back at once, and an undo that brings the call back
        // gets them back too.
        if Some(scene_id) == self.audible_scene
            && let Some(revision) = self.visual_revision
        {
            self.install_virtual_rows(revision);
        }
    }

    pub(super) fn handle_editor_effect(
        &mut self,
        effect: EditorEffect,
    ) -> Result<(), RuntimeError> {
        match effect {
            EditorEffect::Evaluate { revision, source } => {
                self.begin_evaluation_feedback();
                #[cfg(feature = "hydra")]
                {
                    self.generator_preview_pending = false;
                }
                let source = Arc::<str>::from(source.to_string());
                // Update on a prebake tab keeps the setup and runs it. It
                // installs no score, so none of what follows applies.
                if let Some(scope) = self.current_prebake() {
                    self.update_prebake(scope, revision, source);
                    return Ok(());
                }
                // Audio dropped into the open set plays from this update, so
                // the set is read again before the score's sound names are
                // judged.
                self.refresh_set_samples();
                // A score that names something that does not exist is
                // refused before it plays, like a syntax error: the last
                // good score keeps sounding. There is no pressing through
                // it - a set on stage must never take a score the engine
                // will refuse, and the engine's own refusal is a rollback
                // anyway.
                if self.refuse_unclean(revision, &source) {
                    self.finish_evaluation_feedback(false);
                    // A refused launch is not a launch: the next plain
                    // update must not inherit its cycle line, nor its
                    // rewind.
                    self.next_launch = Launch::Now;
                    self.next_rewind = false;
                    self.dirty_frame = true;
                    return Ok(());
                }
                self.log.push(
                    LogLevel::Debug,
                    "transport",
                    format!(
                        "update - {} ({} bytes)",
                        self.scenes.current().name(),
                        source.len()
                    ),
                );
                self.silence_preview();
                // Update is the save: the file is what you hear. The write
                // and the evaluate leave together; a rejected score is still
                // the score on disk, exactly as a watched set behaves. A
                // file that already holds the text is left alone, so playing
                // a set you only opened does not touch it.
                self.playing_pane = Some(self.focused);
                let scene = self.scenes.current().id;
                // A replay tab's text is one block. Updates save normal
                // tapes; live recorder and debug tapes keep edits in the tab.
                // On a replay ^S also starts the chosen block - when its
                // recorded time is up the next block loads and plays - and
                // the run starts whether or not the engine has to change.
                if let Some(id) = self.current_replay() {
                    if let Some(tab) = self.replays.get_mut(&id) {
                        let chosen = tab.selected;
                        let refused = tab.set_source(chosen, &source).err();
                        tab.start(chosen, Instant::now());
                        if let Some(error) = refused {
                            self.set_error(ErrorOwner::Interface, error);
                        }
                    }
                    self.save_replay_tab(id);
                } else {
                    // The file this write lands on may be about to take its
                    // name from the score's `@title`, which is why the name
                    // is settled first: the rename must never leave a save
                    // in flight to the path the file just left.
                    self.sync_name_from_title();
                    self.write_scene(scene, revision, Arc::clone(&source));
                }
                // Nothing to install: the sounding score is already this one,
                // byte for byte. Said rather than silently dropped, because
                // pressing update is a deliberate gesture - and it is still a
                // save, which the write above has already done.
                //
                // Only while playing. Stopped, update is how you start, and
                // the text being unchanged is beside the point.
                // A tape is played from a block, and pressing play means
                // play it: the text matching what already sounds is no
                // reason to leave the set's music where it is.
                //
                // A rewind is the exception: restarting the score is the
                // gesture, so the same bytes are exactly what it is for.
                // Refusing it would mean "stop first to restart it" - the
                // thing a rewind update exists to save you from.
                let rewinding = self.next_rewind
                    || self
                        .scenes
                        .get(scene)
                        .is_some_and(|scene| scene.rewind && scene.is_score());
                if self.current_replay().is_none()
                    && !rewinding
                    && self.install_would_be_redundant(scene, &source)
                {
                    self.rebind_unchanged_score(scene, revision, Arc::clone(&source));
                    self.finish_evaluation_feedback(true);
                    self.status = "unchanged - already playing (stop first to restart it)".into();
                    self.dirty_frame = true;
                    return Ok(());
                }
                // The text that is already armed is waiting for its line;
                // pressing again would only cancel and re-arm it on the
                // same line. Say so instead.
                if !rewinding
                    && self.armed_revision.as_deref() == Some(source_revision(&source).as_str())
                {
                    if self.current_replay().is_none() && self.armed_scene == Some(scene) {
                        self.rebind_unchanged_score(scene, revision, Arc::clone(&source));
                    }
                    self.next_launch = Launch::Now;
                    self.finish_evaluation_feedback(true);
                    self.status = "already armed - waiting for its cycle line".into();
                    self.dirty_frame = true;
                    return Ok(());
                }
                self.queue_evaluation(revision, source);
            }
            EditorEffect::Stop => {
                self.evaluation_feedback = None;
                if self.piano.open {
                    self.silence_piano();
                }
                #[cfg(feature = "hydra")]
                {
                    self.generator_preview_pending = false;
                }
                self.pending_evaluation = None;
                self.stop_replays();
                // A stop is a stop: the engine cancels the armed launch, and
                // nothing here should keep counting down to it.
                self.forget_arming();
                self.landing_name = None;
                self.next_launch = Launch::Now;
                // A stop for a transport already stopped has nothing to
                // stop: the engine word would only come back as a second
                // settle, which resets the advisories and discards the
                // visual update the held frame is showing - a flicker on a
                // picture at rest. An update already sent is on its way to
                // playing, and only the engine's stop cancels it.
                let mut said: Option<String> = None;
                if self.is_playing()
                    || self.is_stopping()
                    || self.stop_requested
                    || !self.inflight.is_empty()
                {
                    self.log
                        .push(LogLevel::Debug, "transport", "stop - the set goes quiet");
                    // The snapshot can trail the key by a frame. The local flag
                    // makes even two rapid presses distinct: first drain, second
                    // cut every remaining voice and effect tail now.
                    let cutting = self.stop_requested || self.is_stopping();
                    // The worker stops on an atomic now; the snapshot that says so
                    // arrives up to 100ms later. Forgetting here means a Stop
                    // followed immediately by Update restarts, instead of the
                    // update being read as redundant against a score that is
                    // already on its way out.
                    self.installed_revision = None;
                    self.stop_requested = true;
                    // Scheduling stops on the first press even while existing
                    // voices and effects drain. Freeze the transport picture at
                    // that instant so the cycle counter and beat light do not
                    // imply that the score is still advancing through its tail.
                    self.visual.stop();
                    // Is what is stopping the set at all? A stop of a tape
                    // being heard again, or of a preview sounding under the
                    // score, is not an event of the set the recorder is
                    // writing, so it must not grow that tape.
                    let stopping_the_set = self.replay_view.is_none();
                    #[cfg(feature = "hydra")]
                    let stopping_the_set = stopping_the_set && self.snippet_preview.is_none();
                    if cutting {
                        self.worker.force_stop();
                    } else {
                        self.worker.request_stop();
                    }
                    // What a stop installs is silence; the tape says so where
                    // it happened, so a replay goes quiet there too.
                    if !cutting && stopping_the_set && self.recorder.is_some() {
                        let at = self.recorder_started.elapsed().as_secs_f64();
                        if let Some(recorder) = self.recorder.as_mut() {
                            recorder.record_save_via(
                                at,
                                SaveStatus::Installed,
                                rustel_runtime::session_log::STOP_SOURCE,
                                None,
                                Some("stop"),
                            );
                        }
                    }
                    said = Some(if cutting {
                        "stopping now".to_owned()
                    } else {
                        format!(
                            "stopping - letting the tail ring out ({} again cuts it)",
                            self.keybinds.hint(BindAction::Stop)
                        )
                    });
                } else if self.preview_armed.is_some() {
                    // The browser's audition sounds under its own voice, not
                    // the transport: its stop still says the word, though the
                    // engine's transport has nothing to stop. The silence
                    // below is the whole of the work, and a second press
                    // finds nothing left to cut.
                    said = Some(format!(
                        "stopping - letting the tail ring out ({} again cuts it)",
                        self.keybinds.hint(BindAction::Stop)
                    ));
                }
                // The same window: a preview was playing under a score
                // that is on its way out, so there is nothing to put back
                // and nothing to hang the next one on.
                #[cfg(feature = "hydra")]
                {
                    self.snippet_preview = None;
                    self.snippet_heard = false;
                }
                // A stop is a stop, and that includes the sample sounding
                // under the browser. The audition has no transport of its
                // own, so the stop silences it here. The browser draws its
                // progress from the clock that started the audition, and
                // that clock would otherwise keep counting through the
                // silence.
                self.silence_preview();
                if let Some(said) = said {
                    self.status = said;
                }
            }
        }
        Ok(())
    }

    pub(super) fn moment(&self) -> HistoryMoment {
        HistoryMoment(self.started.elapsed().as_millis() as u64)
    }
}

/// Whether a command changes the document - the ones that, arriving while a
/// panel holds the keyboard, mean the score has been typed into and focus
/// belongs there again.
pub(super) fn edits_document(command: &Command) -> bool {
    matches!(
        command,
        Command::InsertText(_)
            | Command::ReplaceRange { .. }
            | Command::PasteText(_)
            | Command::Newline
            | Command::Indent
            | Command::Outdent
            | Command::DeleteBackward
            | Command::DeleteForward
            | Command::DeleteWordBackward
            | Command::DeleteWordForward
            | Command::ToggleComment
            | Command::Cut
            | Command::Paste
            | Command::Undo
            | Command::Redo
    )
}

#[cfg(test)]
mod tests {
    mod editing {
        //! Tests for `app/editing.rs`.

        use crate::app::*;
        use crate::editor::MemoryClipboard;

        /// A rendered control is one block: the caret warps over it instead of
        /// resting inside, where it used to silently un-render the widget and
        /// let arrows edit invisible text.
        #[test]
        fn the_caret_warps_over_a_control_instead_of_entering_it() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = crate::app::tests::support::app_over(directory.path());
            let scene = app.scenes.current().id;
            let source = "$: s(\"bd\").velocity(slider(0.5, 0.1, 1))\n";
            app.editor_mut()
                .dispatch(
                    Command::PasteText(source.into()),
                    HistoryMoment(0),
                    &mut MemoryClipboard::default(),
                )
                .expect("seed the score");
            let revision = app.scenes.current().editor.revision();
            app.live_sliders.insert(
                scene,
                LiveSliderSet {
                    revision,
                    sliders: rustel_runtime::ui_events::literal_sliders(
                        &app.scenes.current().editor.source(),
                    ),
                },
            );
            let call = app
                .scenes
                .current()
                .editor
                .source()
                .find("slider(")
                .unwrap();
            // The pill covers `slider(` alone; the numbers stay text.
            let end = call + "slider(".len();

            // Stand just before the call and step Right: the caret lands after
            // the whole block, not one byte into it.
            app.editor_mut()
                .set_selection(crate::editor::Selection::caret(ByteOffset(call)))
                .expect("place caret");
            app.dispatch_editor(Command::Move {
                motion: crate::editor::Motion::Right,
                extend: false,
            })
            .expect("move");
            assert_eq!(
                app.editor().primary_selection().head.0,
                end,
                "Right warps to the far edge"
            );

            // And Left from there warps straight back.
            app.dispatch_editor(Command::Move {
                motion: crate::editor::Motion::Left,
                extend: false,
            })
            .expect("move");
            assert_eq!(
                app.editor().primary_selection().head.0,
                call,
                "Left warps back to the near edge"
            );

            // At either edge the control still answers the keyboard nudge.
            assert!(app.nudge_slider_at_caret(1.0), "nudge from the near edge");
            app.editor_mut()
                .set_selection(crate::editor::Selection::caret(ByteOffset(end)))
                .expect("place caret");
            assert!(app.nudge_slider_at_caret(-1.0), "nudge from the far edge");
        }

        /// The footer's caret reading is the editor's own: a move changes the
        /// column, a newline the line. The gutter can be turned off and the
        /// column is shown nowhere else, so this is the one place both live.
        #[test]
        fn the_caret_reading_follows_the_editor() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = crate::app::tests::support::app_over(directory.path());
            let key = |app: &mut App, code: KeyCode| {
                app.handle_terminal_event(Event::Key(KeyEvent::new(code, KeyModifiers::NONE)))
                    .expect("the key is handled");
            };

            key(&mut app, KeyCode::Home);
            let (line, column) = app.caret_position().expect("a caret");
            assert_eq!((line, column), (line, 1), "Home is column 1");

            key(&mut app, KeyCode::Right);
            assert_eq!(
                app.caret_position(),
                Some((line, 2)),
                "one right is column 2 on the same line"
            );

            key(&mut app, KeyCode::Enter);
            assert_eq!(
                app.caret_position(),
                Some((line + 1, 1)),
                "a newline is the next line, column 1"
            );
        }
    }
}
