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

    fn focus_editor_for_locator(&mut self) {
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
