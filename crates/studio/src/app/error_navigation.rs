//! Global navigation to the current score's first blocking diagnostic.
use super::*;

impl App {
    pub(super) fn jump_to_first_error(&mut self) {
        self.close_piano_mode();
        let scene = self.scenes.current();
        let target = (scene.id, scene.editor.revision());
        if !self.lint_pending
            && let Some(result) = self
                .lint
                .get(&scene.id)
                .filter(|result| {
                    result.revision == target.1
                        && result.input_channels == self.lint_input_channels()
                })
                .cloned()
        {
            self.error_jump = None;
            self.reveal_first_error(&result);
            return;
        }
        // Use the existing background checker even when automatic checking
        // is disabled. Do not evaluate, save, or block the UI on a fresh check.
        let context = self.lint_context_for(scene);
        self.error_jump = Some(target);
        self.linter.submit(LintRequest {
            scene: target.0,
            revision: target.1,
            source: Arc::from(scene.editor.source()),
            mini: self.options.mini && scene.is_score(),
            library: self.worker.library(),
            context,
        });
        self.status = "checking for errors…".into();
        self.dirty_frame = true;
    }

    pub(super) fn finish_error_jump(&mut self, result: &LintResult) {
        if self.error_jump != Some((result.scene, result.revision)) {
            return;
        }
        self.error_jump = None;
        // A late answer must never navigate another scene or outdated text.
        if result.scene != self.scenes.current().id || result.revision != self.editor().revision() {
            return;
        }
        if result.input_channels != self.lint_input_channels() {
            self.jump_to_first_error();
            return;
        }
        self.reveal_first_error(result);
    }

    fn reveal_first_error(&mut self, result: &LintResult) {
        let Some(error) = result
            .diagnostics
            .iter()
            .filter(|error| error.level != LintLevel::Note)
            .min_by_key(|error| error.from)
        else {
            self.lint_status = None;
            if !self.reveal_evaluation_error() {
                self.focus_editor_for_locator();
                self.editor_mut().reveal_selection();
                self.invalidate_maps();
                let (line, column) = self.caret_position().unwrap_or((1, 1));
                self.status = format!("no errors · cursor: {line}:{column}");
                self.arm_focus_rotation_flash(RotationStop::Editor);
            }
            return;
        };
        self.reveal_error_at(error.from, &error.message, true);
    }

    /// Static lint cannot see every JavaScript failure. A current engine
    /// refusal still owns navigation even when the static check is clean.
    fn reveal_evaluation_error(&mut self) -> bool {
        let current = self.scenes.current();
        // Setup has its own error owner and uses source hashes. Its error
        // slot may describe the other scope, so require both identities.
        let setup_error = current.prebake().and_then(|scope| {
            let hash = source_revision(&current.editor.source());
            (self.prebake_rejected[scope.index()].as_deref() == Some(hash.as_str()))
                .then(|| self.errors.get(ErrorOwner::Setup))
                .flatten()
                .filter(|message| message.starts_with(&format!("{}:", scope.tab_name())))
                .map(str::to_owned)
        });
        let (scene, revision, message) = if let Some(message) = setup_error {
            (current.id, current.editor.revision(), message)
        } else {
            let Some((scene, revision)) = self.update_failure else {
                return false;
            };
            let Some(message) = self.errors.get(ErrorOwner::Evaluation).map(str::to_owned) else {
                return false;
            };
            (scene, revision, message)
        };
        // Do not follow positions in text that has since been edited or closed.
        if !self
            .scenes
            .get(scene)
            .is_some_and(|scene| scene.editor.revision() == revision)
        {
            return false;
        }
        if scene != self.scenes.current().id {
            let Some(index) = self.scenes.index_of(scene) else {
                return false;
            };
            if !self.select_scene(index) {
                return false;
            }
        }
        let document = self.editor().document();
        let at = reported_position(&message).and_then(|(line, column)| {
            let line = line.checked_sub(1)?;
            if line >= document.line_count() {
                return None;
            }
            let text = document.line_content(line);
            let column = column.unwrap_or(1).checked_sub(1)?;
            // QuickJS reports byte columns, not terminal cell columns. The
            // shared reveal helper then snaps to a complete UTF-8 grapheme.
            let byte = column.min(text.len());
            Some(document.line_start(line).0 + byte)
        });
        // Some engine failures carry no source position. Open the affected
        // score at its start and keep the failure visible; never say "no errors".
        self.reveal_error_at(at.unwrap_or(0), &message, at.is_some());
        true
    }

    fn reveal_error_at(&mut self, offset: usize, message: &str, located: bool) {
        let document = self.editor().document();
        let mut at = ByteOffset(offset.min(document.len_bytes()));
        // Diagnostics name byte spans; a caret also has to respect an entire
        // grapheme and CRLF. Land before that cluster when its interior is named.
        while at.0 > 0 && document.validate_caret_offset(at).is_err() {
            at.0 -= 1;
        }
        self.focus_editor_for_locator();
        if let Err(error) = self
            .editor_mut()
            .set_selection(super::super::editor::Selection::caret(at))
        {
            self.set_error(ErrorOwner::Editor, error.to_string());
            return;
        }
        self.editor_mut().reveal_selection();
        self.invalidate_maps();
        let (line, column) = self.caret_position().unwrap_or((1, 1));
        let status = if located {
            format!("{line}:{column} · {message}")
        } else {
            format!("{message} · source location unavailable")
        };
        self.status.clone_from(&status);
        self.lint_status = Some((self.scenes.current().id, self.editor().revision(), status));
        self.arm_focus_rotation_flash(RotationStop::Editor);
    }

    pub(super) fn focus_editor_for_locator(&mut self) {
        self.help = None;
        self.menu = None;
        self.dismiss_dialogs(None);
        // A sticky log on a small terminal falls back to an overlay.
        if self.log_panel.is_some() && !self.log_is_docked() {
            self.close_panel(PanelKind::Log);
        }
        self.strip_mode = SceneStripMode::Idle;
        self.renaming_bank = None;
        self.renaming_bank_source = None;
        self.armed_slider = None;
        self.stop_precision_drag();
        self.stop_replay_drag();
        self.pointer = None;
        self.drop_pane_selection();
        self.focus = Focus::Editor;
        self.marks_muted = None;
    }
}

/// The JS host's source-mapped, one-based position, not a stack frame or a
/// number guessed from arbitrary error text. A missing column means line start.
fn reported_position(message: &str) -> Option<(usize, Option<usize>)> {
    let (_, rest) = message.rsplit_once(" - line ")?;
    let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
    let line = rest[..digits].parse().ok()?;
    let column = rest[digits..].strip_prefix(", column ").and_then(|rest| {
        let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
        rest[..digits].parse().ok()
    });
    Some((line, column))
}

#[cfg(test)]
mod tests {
    mod error_navigation {
        //! Tests for error navigation.

        use crate::app::*;
        use crate::editor::{GridRect, Selection};
        use crate::keybinds::{BindAction, KeyCombo, KeybindLearn};
        use crossterm::event::{KeyEvent, KeyModifiers};
        use rustel_runtime::lint::Diagnostic;

        #[test]
        fn a_late_callback_failure_keeps_the_installed_revision() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = crate::app::tests::support::app_over(directory.path());
            app.editor_mut()
                .clone_from(&Editor::new("// old score\nthrow 42;").unwrap());
            let scene = app.scenes.current().id;
            let installed = app.editor().revision();
            app.audible_scene = Some(scene);
            app.visual_revision = Some(installed);

            app.dispatch_editor(Command::InsertText("// inserted line\n".into()))
                .unwrap();
            let current = app.editor().revision();
            assert!(current > installed);
            let caret = app.editor().primary_selection();
            app.handle_control(StudioControlEvent::Diagnostic(
                crate::engine::StudioDiagnostic {
                    kind: "evaluation".into(),
                    message: "callback failed - line 2".into(),
                    recoverable: true,
                    level: StudioDiagnosticLevel::Error,
                    alert: None,
                },
            ));
            assert_eq!(app.update_failure, Some((scene, installed)));
            app.lint.insert(
                scene,
                crate::lint::LintResult {
                    scene,
                    revision: current,
                    input_channels: app.lint_input_channels(),
                    diagnostics: Vec::new(),
                },
            );
            app.lint_pending = false;
            app.jump_to_first_error();
            assert_eq!(
                app.editor().primary_selection(),
                caret,
                "a position from the installed score must not navigate newer text"
            );
        }

        fn app_at(directory: &std::path::Path, source: &str) -> App {
            let mut app = crate::app::tests::support::app_over(directory);
            app.editor_mut().clone_from(&Editor::new(source).unwrap());
            app.frame = Rect::new(0, 0, 120, 40);
            app.worker
                .set_library(Arc::new(rustel_runtime::samples::SampleLibrary::empty()));
            app
        }

        fn cache(app: &mut App, diagnostics: Vec<Diagnostic>) -> LintResult {
            let result = LintResult {
                scene: app.scenes.current().id,
                revision: app.editor().revision(),
                input_channels: app.lint_input_channels(),
                diagnostics,
            };
            app.lint.insert(result.scene, result.clone());
            app.lint_pending = false;
            result
        }

        fn problem(level: LintLevel, at: usize) -> Diagnostic {
            Diagnostic {
                level,
                from: at,
                to: at + 1,
                message: "test problem".into(),
            }
        }

        fn press(app: &mut App, code: KeyCode, modifiers: KeyModifiers) {
            app.handle_terminal_event(Event::Key(KeyEvent::new(code, modifiers)))
                .unwrap();
        }

        fn jump(app: &mut App) {
            press(app, KeyCode::F(3), KeyModifiers::SHIFT);
        }

        #[test]
        fn first_error_escapes_panels_menus_prompts_and_help_without_evaluating() {
            let directory = tempfile::tempdir().unwrap();
            let source = "// note\n$: s(\"missing\")\n$: s(\"bd\").lpf(";
            let at = source.find("missing").unwrap();
            let mut app = app_at(directory.path(), source);
            let revision = app.editor().revision();
            let next_request = app.next_request_id;
            let stored = std::fs::read_to_string(&app.scenes.current().path).unwrap();
            for surface in 0..8 {
                app.editor_mut()
                    .set_selection(Selection::caret(ByteOffset(0)))
                    .unwrap();
                // Deliberately unsorted: "first" is source order, ignoring notes.
                cache(
                    &mut app,
                    vec![
                        problem(LintLevel::Syntax, source.len() - 1),
                        problem(LintLevel::Note, 0),
                        problem(LintLevel::Value, at),
                    ],
                );
                match surface {
                    0 => {
                        app.open_set_panel(false);
                        app.focus_panel(PanelKind::Set);
                    }
                    1 => {
                        app.toggle_reference_browse();
                    }
                    2 => {
                        app.settings_sheet = Some(SettingsSheet::default());
                        app.focus_panel(PanelKind::Settings);
                    }
                    3 => {
                        app.menu = Some(crate::menu::MenuState::opened());
                    }
                    4 => {
                        app.help = Some(HelpState::default());
                    }
                    5 => {
                        app.open_set_prompt(SetPrompt::OpenSet);
                    }
                    6 => {
                        app.begin_rename();
                    }
                    _ => {
                        app.toggle_log_panel();
                    }
                }
                jump(&mut app);
                assert_eq!(app.focus, Focus::Editor, "surface {surface}");
                assert_eq!(
                    app.editor().primary_selection(),
                    Selection::caret(ByteOffset(at))
                );
                assert!(app.help.is_none() && app.menu.is_none() && app.settings_sheet.is_none());
                assert!(app.set_prompt.is_none());
                assert!(matches!(app.strip_mode, SceneStripMode::Idle));
                assert_eq!(app.editor().source(), source);
                assert_eq!(app.editor().revision(), revision);
                assert_eq!(app.next_request_id, next_request, "jump must not evaluate");
                assert!(matches!(
                    app.focus_rotation_flash,
                    Some((RotationStop::Editor, _))
                ));
                assert!(app.status.starts_with("2:7 · "), "{}", app.status);
                assert_eq!(
                    std::fs::read_to_string(&app.scenes.current().path).unwrap(),
                    stored
                );
            }
        }

        #[test]
        fn first_error_is_visible_with_wrapping_and_both_scroll_axes() {
            let directory = tempfile::tempdir().unwrap();
            let source = format!(
                "{}{}bad\n{}",
                "// filler\n".repeat(70),
                " ".repeat(170),
                "// tail\n".repeat(70)
            );
            let at = source.find("bad").unwrap();
            let mut app = app_at(directory.path(), &source);
            for wrap in [false, true] {
                app.editor_mut().set_wrap(wrap);
                app.editor_mut().set_view_size(35, 12);
                let mut viewport = app.editor().viewport();
                viewport.top_row = 120;
                viewport.left_column = 0;
                app.editor_mut().set_viewport(viewport);
                cache(&mut app, vec![problem(LintLevel::Syntax, at)]);
                jump(&mut app);
                let map = app
                    .editor()
                    .screen_map(GridRect::new(0, 0, 35, 12))
                    .unwrap();
                assert!(
                    map.cell_for_offset(ByteOffset(at)).is_some(),
                    "caret hidden with wrap={wrap}"
                );
                assert_eq!(app.editor().viewport().left_column > 0, !wrap);
            }
        }

        #[test]
        fn first_error_reveals_the_rendered_caret_in_the_focused_split_pane() {
            let directory = tempfile::tempdir().unwrap();
            let mut app = app_at(directory.path(), "// left pane stays here");
            let left = app.scenes.current().id;
            let left_selection = app.editor().primary_selection();
            app.toggle_split();
            assert_eq!(app.focused, 1);
            let source = format!("{}{}bad", "// filler\n".repeat(60), " ".repeat(170));
            let at = source.find("bad").unwrap();
            app.scenes.current_mut().editor = Editor::new(&source).unwrap();
            let mut terminal =
                ratatui::Terminal::new(ratatui::backend::TestBackend::new(120, 30)).unwrap();
            for wrap in [false, true] {
                app.editor_mut().set_wrap(wrap);
                app.editor_mut()
                    .set_selection(Selection::caret(ByteOffset(0)))
                    .unwrap();
                app.settings_sheet = Some(SettingsSheet::default());
                app.focus_panel(PanelKind::Settings);
                terminal
                    .draw(|frame| app.paint_into(frame, Instant::now()).unwrap())
                    .unwrap();
                cache(&mut app, vec![problem(LintLevel::Value, at)]);
                jump(&mut app);
                terminal
                    .draw(|frame| app.paint_into(frame, Instant::now()).unwrap())
                    .unwrap();
                let cell = app
                    .focused_map()
                    .unwrap()
                    .cell_for_offset(ByteOffset(at))
                    .expect("rendered caret is visible");
                assert_eq!(
                    terminal
                        .backend()
                        .buffer()
                        .cell((cell.x, cell.y))
                        .unwrap()
                        .symbol(),
                    "b"
                );
                let painted = terminal.backend().buffer().cell((cell.x, cell.y)).unwrap();
                let fill = app
                    .theme
                    .caret_line_fill()
                    .expect("theme has a locator fill");
                assert_eq!(painted.bg, fill);
                assert!(
                    !painted
                        .modifier
                        .contains(ratatui::style::Modifier::REVERSED),
                    "the locator must preserve the caret and syntax foregrounds"
                );
                assert!(
                    terminal
                        .backend()
                        .buffer()
                        .cell((cell.x.saturating_sub(1), cell.y))
                        .is_some_and(|cell| cell.bg == fill),
                    "the locator should identify the line, not only recolour the caret"
                );
                assert!(app.settings_sheet.is_none());
                assert_eq!(app.focus, Focus::Editor);
                assert_eq!(app.focused, 1);
                assert_eq!(
                    app.scenes.get(left).unwrap().editor.primary_selection(),
                    left_selection
                );
                assert_eq!(
                    app.scenes.get(left).unwrap().editor.source(),
                    "// left pane stays here"
                );
            }
        }

        #[test]
        fn fresh_error_checks_work_in_every_syntax_mode_and_skip_stale_positions() {
            let directory = tempfile::tempdir().unwrap();
            let mut app = app_at(directory.path(), "$: s(\"bd\").lpf(");
            for mode in [SyntaxCheck::Full, SyntaxCheck::OnUpdate, SyntaxCheck::Off] {
                app.ui_settings.syntax_check = mode;
                app.lint.clear();
                app.lint_pending = true;
                app.editor_mut()
                    .set_selection(Selection::caret(ByteOffset(0)))
                    .unwrap();
                jump(&mut app);
                assert!(app.error_jump.is_some());
                let deadline = Instant::now() + Duration::from_secs(5);
                while app.error_jump.is_some() {
                    app.drain_lint();
                    assert!(Instant::now() < deadline, "no lint answer");
                    std::thread::sleep(Duration::from_millis(5));
                }
                let diagnostics = lint_with(
                    &app.editor().source(),
                    false,
                    None,
                    &app.lint_context_for(app.scenes.current()),
                );
                let first = diagnostics
                    .iter()
                    .filter(|d| d.level != LintLevel::Note)
                    .min_by_key(|d| d.from)
                    .unwrap();
                assert_eq!(
                    app.editor().primary_selection().head,
                    ByteOffset(first.from)
                );
                assert!(
                    app.status.starts_with("1:") && app.status.contains(" · "),
                    "{}",
                    app.status
                );
            }
            let stale = cache(&mut app, vec![problem(LintLevel::Value, 3)]);
            app.error_jump = Some((stale.scene, stale.revision));
            app.dispatch_editor(Command::InsertText(" ".into()))
                .unwrap();
            let caret = app.editor().primary_selection();
            app.finish_error_jump(&stale);
            assert_eq!(
                app.editor().primary_selection(),
                caret,
                "old revision must not move caret"
            );
            assert!(app.error_jump.is_none());
            app.error_jump = Some((stale.scene, stale.revision));
            app.new_scene(false);
            let current = app.scenes.current().id;
            app.finish_error_jump(&stale);
            assert_eq!(
                app.scenes.current().id,
                current,
                "old scene must not steal focus"
            );
        }

        #[test]
        fn first_error_keeps_clean_scores_in_place_and_handles_unicode_and_eof() {
            let directory = tempfile::tempdir().unwrap();
            let source = "// e\u{301} 😀\r\n$: s(\"bd\")";
            let mut app = app_at(directory.path(), source);
            app.focus = Focus::Editor;
            cache(&mut app, vec![problem(LintLevel::Note, 0)]);
            let before = app.editor().primary_selection();
            jump(&mut app);
            assert_eq!(app.editor().primary_selection(), before);
            assert_eq!(app.status, "no errors · cursor: 1:1");
            assert!(matches!(
                app.focus_rotation_flash,
                Some((RotationStop::Editor, _))
            ));
            app.settings_sheet = Some(SettingsSheet::default());
            app.focus_panel(PanelKind::Settings);
            cache(&mut app, vec![problem(LintLevel::Note, 0)]);
            jump(&mut app);
            assert_eq!(app.focus, Focus::Editor);
            assert!(app.settings_sheet.is_none());
            assert_eq!(app.editor().primary_selection(), before);
            assert_eq!(app.status, "no errors · cursor: 1:1");
            assert!(matches!(
                app.focus_rotation_flash,
                Some((RotationStop::Editor, _))
            ));
            for (offset, expected) in [(4, 3), (source.len(), source.len())] {
                cache(&mut app, vec![problem(LintLevel::Syntax, offset)]);
                jump(&mut app);
                assert_eq!(app.editor().primary_selection().head, ByteOffset(expected));
            }
        }

        #[test]
        fn clean_lint_cannot_hide_a_current_evaluation_error() {
            let directory = tempfile::tempdir().unwrap();
            let source = format!("{}const é = 1; broken();", "// filler\n".repeat(60));
            let at = source.find("broken").unwrap();
            let mut app = app_at(directory.path(), &source);
            let scene = app.scenes.current().id;
            let revision = app.editor().revision();
            app.inflight.insert(
                17,
                PendingEvaluation {
                    request_id: 17,
                    scene,
                    editor_revision: revision,
                    source: Arc::from(source.as_str()),
                    launch: Launch::Now,
                    rewind: false,
                    record: false,
                },
            );
            app.handle_evaluation(EvaluationOutcome {
                request_id: 17,
                editor_revision: revision.0,
                result: Err(crate::EngineFailure {
                    kind: "evaluation".into(),
                    message: "javascript: TypeError: not a function - line 61, column 15".into(),
                    recoverable: true,
                    playback_stopped: false,
                }),
            });
            let next_request = app.next_request_id;
            for pending in [false, true] {
                app.editor_mut()
                    .set_selection(Selection::caret(ByteOffset(0)))
                    .unwrap();
                app.editor_mut().set_view_size(35, 12);
                app.settings_sheet = Some(SettingsSheet::default());
                app.focus_panel(PanelKind::Settings);
                let clean = cache(&mut app, Vec::new());
                app.lint_pending = pending;
                jump(&mut app);
                if pending {
                    assert!(app.error_jump.is_some());
                    app.finish_error_jump(&clean);
                }
                assert_eq!(app.focus, Focus::Editor);
                assert!(app.settings_sheet.is_none());
                assert_eq!(app.editor().primary_selection().head, ByteOffset(at));
                assert!(
                    app.editor()
                        .screen_map(GridRect::new(0, 0, 35, 12))
                        .unwrap()
                        .cell_for_offset(ByteOffset(at))
                        .is_some()
                );
                assert!(app.status.contains("not a function"), "{}", app.status);
                assert!(!app.status.contains("no errors"));
                assert_eq!(
                    app.next_request_id, next_request,
                    "navigation must not evaluate"
                );
                assert_eq!(app.editor().source(), source);
            }
        }

        #[test]
        fn evaluation_error_owns_navigation_until_its_revision_is_edited() {
            let directory = tempfile::tempdir().unwrap();
            let mut app = app_at(directory.path(), "// first\nbroken();");
            let scene = app.scenes.current().id;
            let revision = app.editor().revision();
            app.update_failure = Some((scene, revision));
            app.errors.set(
                ErrorOwner::Evaluation,
                "broken is not defined - line 2".into(),
            );
            app.toggle_split();
            let other = app.scenes.current().id;
            let other_selection = app.editor().primary_selection();
            cache(&mut app, Vec::new());
            jump(&mut app);
            assert_eq!(app.scenes.current().id, scene);
            assert_eq!(app.focused, 0);
            assert_eq!(app.editor().primary_selection().head, ByteOffset(9));
            assert_eq!(
                app.scenes.get(other).unwrap().editor.primary_selection(),
                other_selection
            );

            app.dispatch_editor(Command::InsertText(" ".into()))
                .unwrap();
            let caret = app.editor().primary_selection();
            cache(&mut app, Vec::new());
            jump(&mut app);
            assert_eq!(
                app.editor().primary_selection(),
                caret,
                "stale error must not move the caret"
            );
            assert!(app.status.starts_with("no errors · cursor:"));
        }

        #[test]
        fn unlocated_evaluation_errors_never_claim_the_score_is_clean() {
            let directory = tempfile::tempdir().unwrap();
            let mut app = app_at(directory.path(), "// first\nthrow 42;");
            let scene = app.scenes.current().id;
            app.update_failure = Some((scene, app.editor().revision()));
            app.errors.set(ErrorOwner::Evaluation, "42".into());
            app.editor_mut()
                .set_selection(Selection::caret(ByteOffset(9)))
                .unwrap();
            app.toggle_reference_browse();
            cache(&mut app, Vec::new());
            jump(&mut app);
            assert_eq!(app.focus, Focus::Editor);
            assert_eq!(app.editor().primary_selection().head, ByteOffset(0));
            assert_eq!(app.status, "42 · source location unavailable");
            assert_eq!(app.errors.get(ErrorOwner::Evaluation), Some("42"));
        }

        #[test]
        fn a_refused_update_of_a_closed_scene_clears_on_the_next_install() {
            let directory = tempfile::tempdir().unwrap();
            let mut app = app_at(directory.path(), "s(\"sine\")");
            let closed = SceneId(app.scenes.next_id());
            app.update_failure = Some((closed, Revision(1)));
            app.errors.set(ErrorOwner::Evaluation, "refused".into());
            let scene = app.scenes.current().id;
            app.resolve_evaluation_alert(scene, app.editor().revision());
            assert_eq!(app.update_failure, None);
            assert_eq!(app.errors.get(ErrorOwner::Evaluation), None);
        }

        #[test]
        fn current_setup_failures_outrank_a_clean_static_check() {
            let directory = tempfile::tempdir().unwrap();
            let mut app = app_at(directory.path(), "s(\"sine\")");
            let scope = PrebakeScope::Local;
            app.open_prebake(scope);
            let source = "// setup\nthrow new Error('broken');";
            app.editor_mut().clone_from(&Editor::new(source).unwrap());
            app.prebake_rejected[scope.index()] = Some(source_revision(source));
            app.errors.set(
                ErrorOwner::Setup,
                format!("{}: javascript: broken - line 2", scope.tab_name()),
            );
            app.settings_sheet = Some(SettingsSheet::default());
            app.focus_panel(PanelKind::Settings);
            cache(&mut app, Vec::new());
            jump(&mut app);
            assert_eq!(app.current_prebake(), Some(scope));
            assert_eq!(app.focus, Focus::Editor);
            assert_eq!(app.editor().primary_selection().head, ByteOffset(9));
            assert!(app.status.contains("broken"));
            assert!(!app.status.contains("no errors"));

            app.dispatch_editor(Command::InsertText(" ".into()))
                .unwrap();
            let caret = app.editor().primary_selection();
            cache(&mut app, Vec::new());
            jump(&mut app);
            assert_eq!(app.editor().primary_selection(), caret);
            assert!(app.status.starts_with("no errors · cursor:"));
        }

        #[test]
        fn clean_score_locator_reveals_the_caret_behind_a_panel() {
            let directory = tempfile::tempdir().unwrap();
            let source = format!("{}s(\"sine\")", "// filler\n".repeat(60));
            let at = source.find("s(").unwrap();
            let mut app = app_at(directory.path(), &source);
            app.editor_mut().set_view_size(35, 12);
            app.editor_mut()
                .set_selection(Selection::caret(ByteOffset(at)))
                .unwrap();
            let mut viewport = app.editor().viewport();
            viewport.top_row = 0;
            app.editor_mut().set_viewport(viewport);
            app.settings_sheet = Some(SettingsSheet::default());
            app.focus_panel(PanelKind::Settings);
            cache(&mut app, vec![problem(LintLevel::Note, 0)]);
            jump(&mut app);
            assert_eq!(app.focus, Focus::Editor);
            assert!(app.settings_sheet.is_none());
            assert_eq!(app.editor().primary_selection().head, ByteOffset(at));
            assert!(
                app.editor()
                    .screen_map(GridRect::new(0, 0, 35, 12))
                    .unwrap()
                    .cell_for_offset(ByteOffset(at))
                    .is_some()
            );
            assert_eq!(app.status, "no errors · cursor: 61:1");
        }

        #[test]
        fn lint_footer_leads_with_the_current_line_and_column() {
            let directory = tempfile::tempdir().unwrap();
            let source = "first\nsecond";
            let mut app = app_at(directory.path(), source);
            cache(
                &mut app,
                vec![problem(
                    LintLevel::Syntax,
                    source.find("second").unwrap() + 2,
                )],
            );
            assert_eq!(app.lint_message().as_deref(), Some("2:3 · test problem"));
        }

        #[test]
        fn a_navigated_diagnostic_does_not_survive_its_edited_revision() {
            let directory = tempfile::tempdir().unwrap();
            let mut app = app_at(directory.path(), "bad");
            cache(&mut app, vec![problem(LintLevel::Syntax, 0)]);
            jump(&mut app);
            let copied = app.status.clone();
            assert!(app.lint_status.is_some());

            app.dispatch_editor(Command::InsertText(" ".into()))
                .unwrap();
            let clean = LintResult {
                scene: app.scenes.current().id,
                revision: app.editor().revision(),
                input_channels: app.lint_input_channels(),
                diagnostics: Vec::new(),
            };
            app.absorb_lint(clean);

            assert_ne!(app.status, copied);
            assert_eq!(app.status, "ready");
            assert!(app.lint_status.is_none());
            assert!(app.lint_message().is_none());
        }

        #[test]
        fn first_error_rebinds_in_menus_and_respects_key_learning_and_stop() {
            let directory = tempfile::tempdir().unwrap();
            let mut app = app_at(directory.path(), "// error");
            cache(&mut app, vec![problem(LintLevel::Syntax, 3)]);
            assert_eq!(
                app.keybinds
                    .action_for(&KeyEvent::new(KeyCode::F(8), KeyModifiers::NONE)),
                Some(BindAction::Stop)
            );
            app.settings_sheet = Some(SettingsSheet::default());
            app.keybind_learn = Some(KeybindLearn::arm(BindAction::Undo));
            jump(&mut app);
            assert!(matches!(
                app.keybind_learn,
                Some(KeybindLearn::Confirm {
                    held_by: BindAction::FirstError,
                    ..
                })
            ));
            assert!(app.settings_sheet.is_some());
            assert_eq!(app.editor().primary_selection().head, ByteOffset(0));
            app.keybind_learn = None;
            let rebound = KeyCombo::parse("ctrl+f3").unwrap();
            app.keybinds.learn(BindAction::FirstError, Some(rebound));
            jump(&mut app);
            assert_eq!(
                app.editor().primary_selection().head,
                ByteOffset(0),
                "retired shortcut"
            );
            let row = app
                .menus()
                .into_iter()
                .flat_map(|menu| menu.items)
                .find(|row| row.label == "First error / locate cursor")
                .unwrap();
            assert_eq!(row.accel, rebound.hint());
            press(&mut app, KeyCode::F(3), KeyModifiers::CONTROL);
            assert_eq!(app.editor().primary_selection().head, ByteOffset(3));
            assert_eq!(app.focus, Focus::Editor);
        }
    }
}
