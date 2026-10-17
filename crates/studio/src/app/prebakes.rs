//! Prebakes: the global and per-set setup code that runs before any score.
//! Opening and closing a prebake tab from the Settings sheet, saving its text,
//! checking it, queueing it for the engine one request at a time (global first,
//! then local), handling what the engine sends back, and re-evaluating the
//! playing score once a new setup has run.

use super::*;

/// One setup evaluation the interface asked the engine for.
#[derive(Clone, Debug)]
pub(super) struct PrebakeRequest {
    pub(super) request_id: u64,
    pub(super) scope: PrebakeScope,
    pub(super) source: Arc<str>,
    /// The last of a chain. When it applies and a score is sounding, that
    /// score is installed again so the music picks the new setup up.
    pub(super) reinstall_after: bool,
}

impl App {
    /// A prebake's text as stored, whether or not a tab is showing it.
    pub(super) fn stored_prebake(&self, scope: PrebakeScope) -> &str {
        match scope {
            PrebakeScope::Global => &self.global_prebake,
            PrebakeScope::Local => self.scenes.local_prebake(),
        }
    }

    /// Which prebake the tab on screen is, when it is one.
    pub(super) fn current_prebake(&self) -> Option<PrebakeScope> {
        self.scenes.current().prebake()
    }

    /// The two rows the settings sheet draws.
    pub(super) fn prebake_rows(&self) -> [PrebakeRow; 2] {
        PrebakeScope::ALL.map(|scope| {
            let stored = self.stored_prebake(scope);
            let hash = source_revision(stored);
            let index = scope.index();
            let open = self.scenes.prebake_index(scope);
            PrebakeRow {
                lines: if stored.is_empty() {
                    0
                } else {
                    stored.lines().count()
                },
                blank: prebake::is_blank(stored),
                // Of what is STORED: a text edited since it ran has not run.
                verdict: if self.prebake_applied[index].as_deref() == Some(hash.as_str()) {
                    PrebakeVerdict::Applied
                } else if self.prebake_rejected[index].as_deref() == Some(hash.as_str()) {
                    PrebakeVerdict::Rejected
                } else {
                    PrebakeVerdict::Unchecked
                },
                open: open.is_some(),
                dirty: open
                    .and_then(|index| self.scenes.scenes().get(index))
                    .is_some_and(|scene| scene.dirty),
            }
        })
    }

    /// Keep a prebake's text where its scope says it lives. The tab goes
    /// clean only when the store really took it.
    pub(super) fn persist_prebake(&mut self, scope: PrebakeScope, text: &str) -> bool {
        let kept = match scope {
            PrebakeScope::Global => {
                self.global_prebake = if prebake::is_blank(text) {
                    String::new()
                } else {
                    text.to_owned()
                };
                match prebake::save_global_to(self.global_prebake_path.as_deref(), text) {
                    Ok(()) => {
                        self.clear_error(ErrorOwner::Save);
                        true
                    }
                    Err(error) => {
                        self.set_error(
                            ErrorOwner::Save,
                            format!("{} not kept: {error}", scope.tab_name()),
                        );
                        false
                    }
                }
            }
            PrebakeScope::Local => {
                self.scenes.set_local_prebake(text);
                self.persist_manifest();
                self.errors.get(ErrorOwner::Save).is_none()
            }
        };
        if kept && let Some(index) = self.scenes.prebake_index(scope) {
            let revision = source_revision(text);
            if let Some(scene) = self.scenes.scenes_mut().get_mut(index) {
                scene.saved_source_revision = revision;
                scene.latest_save = None;
                scene.refresh_dirty();
            }
        }
        kept
    }

    /// Run the setups a studio opens on, global then local.
    ///
    /// One that does not check out is reported and skipped rather than run:
    /// the scores still open, and the footer says what is wrong with the
    /// setup they were written against.
    pub(super) fn queue_startup_prebakes(&mut self) {
        let mut chain = Vec::new();
        for scope in PrebakeScope::ALL {
            if prebake::is_blank(self.stored_prebake(scope)) {
                continue;
            }
            if self.check_stored_prebake(scope) {
                chain.push(scope);
            }
        }
        self.queue_prebakes(&chain, false);
    }

    /// Show a prebake in the focused pane, from the settings sheet.
    pub(super) fn open_prebake(&mut self, scope: PrebakeScope) {
        // Setup belongs to the set view; a tape gives the studio back first.
        if self.replay_view.is_some() {
            self.close_replay();
        }
        self.close_settings_sheet();
        self.focus = Focus::Editor;
        #[cfg(feature = "hydra")]
        self.sync_settings_webcam_preview();
        // A slider armed in another tab must not catch arrows meant here.
        self.armed_slider = None;
        let stored = self.stored_prebake(scope).to_owned();
        let text = if stored.is_empty() {
            prebake::PREBAKE_STARTER.to_owned()
        } else {
            stored
        };
        match self.scenes.open_prebake(scope, &text) {
            Ok(id) => {
                // Already showing in the other pane: the caret goes there,
                // exactly as selecting a scene does.
                if let Some(other) = self.panes.iter().position(|pane| pane.scene == id)
                    && other != self.focused
                {
                    self.focus_pane(other);
                } else {
                    self.panes[self.focused].scene = id;
                    self.reconcile_panes();
                }
                self.strip_mode = SceneStripMode::Idle;
                self.invalidate_maps();
                self.pointer = None;
                self.rebuild_slider_spans();
                self.lint_pending = true;
                self.status = format!(
                    "{} - setup before the scores; {} applies it, {} closes the tab",
                    scope.tab_name(),
                    self.keybinds.hint(BindAction::Evaluate),
                    self.keybinds.hint(BindAction::CloseScene)
                );
            }
            Err(error) => self.set_error(ErrorOwner::Interface, error.to_string()),
        }
        self.dirty_frame = true;
    }

    /// Close a prebake tab. Its text is kept, as a scene's is, and checked
    /// on the way out: one that does not check out is still kept, and says
    /// so, rather than waiting until the next studio to break.
    pub(super) fn close_prebake(&mut self, scope: PrebakeScope) {
        let scene = self.scenes.current();
        let (dirty, text) = (scene.dirty, scene.editor.source());
        if dirty {
            self.persist_prebake(scope, &text);
            self.check_stored_prebake(scope);
        }
        let id = self.scenes.current().id;
        match self.scenes.close_current() {
            Ok(_) => {
                self.reconcile_panes();
                self.strip_mode = SceneStripMode::Idle;
                self.rebuild_slider_spans();
                self.lint.remove(&id);
                self.live_sliders.remove(&id);
                self.readiness.remove(&id);
                self.invalidate_maps();
                self.status = format!(
                    "closed {} - the settings sheet opens it again",
                    scope.tab_name()
                );
            }
            Err(error) => self.set_error(ErrorOwner::Interface, error.to_string()),
        }
        self.dirty_frame = true;
    }

    /// Check what is stored without running it, and report a text that does
    /// not check out. Used where a prebake is kept but not applied: closing
    /// a tab, quitting, and opening a studio.
    pub(super) fn check_stored_prebake(&mut self, scope: PrebakeScope) -> bool {
        let stored = self.stored_prebake(scope).to_owned();
        if prebake::is_blank(&stored) {
            self.prebake_rejected[scope.index()] = None;
            self.clear_prebake_error(scope);
            return true;
        }
        let context = match scope {
            PrebakeScope::Global => LintContext::from_setup_sources([stored.as_str()]).as_setup(),
            PrebakeScope::Local => {
                LintContext::from_setup_sources([self.global_prebake.as_str(), stored.as_str()])
                    .as_setup()
            }
        };
        let library = self.worker.library();
        let diagnostics = lint_with(&stored, false, library.as_deref(), &context);
        match rejection_of(&stored, &diagnostics) {
            Some(reason) => {
                self.prebake_rejected[scope.index()] = Some(source_revision(&stored));
                self.set_error(
                    ErrorOwner::Setup,
                    format!("{} refused - {reason}", scope.tab_name()),
                );
                false
            }
            None => {
                self.prebake_rejected[scope.index()] = None;
                self.clear_prebake_error(scope);
                true
            }
        }
    }

    /// Put the setup error away only when neither prebake is refused: one
    /// scope's good news is not the other's.
    fn clear_prebake_error(&mut self, scope: PrebakeScope) {
        let other = match scope {
            PrebakeScope::Global => PrebakeScope::Local,
            PrebakeScope::Local => PrebakeScope::Global,
        };
        let other_stored = source_revision(self.stored_prebake(other));
        if self.prebake_rejected[other.index()].as_deref() != Some(other_stored.as_str()) {
            self.clear_error(ErrorOwner::Setup);
        }
    }

    /// Ctrl+S on a prebake tab: check it, keep it, run it.
    pub(super) fn update_prebake(
        &mut self,
        scope: PrebakeScope,
        revision: Revision,
        source: Arc<str>,
    ) {
        if self.refuse_unclean(revision, &source) {
            self.finish_evaluation_feedback(false);
            // Refused text is not kept: the tab still holds it, and closing
            // or quitting will, exactly as a refused score stays in its
            // buffer and reaches disk on the way out.
            self.prebake_rejected[scope.index()] = Some(source_revision(&source));
            self.dirty_frame = true;
            return;
        }
        self.persist_prebake(scope, &source);
        self.clear_prebake_error(scope);
        if prebake::is_blank(&source) {
            self.finish_evaluation_feedback(true);
            self.prebake_applied[scope.index()] = None;
            self.prebake_rejected[scope.index()] = None;
            self.status = format!("{} is empty - nothing to run", scope.tab_name());
            self.dirty_frame = true;
            return;
        }
        // Order is the point: a fresh global setup is followed by the set's
        // own, so one heap always sees them global-then-local however they
        // were edited.
        let chain: &[PrebakeScope] = match scope {
            PrebakeScope::Global => &PrebakeScope::ALL,
            PrebakeScope::Local => &[PrebakeScope::Local],
        };
        self.queue_prebakes(chain, true);
        self.dirty_frame = true;
    }

    /// Queue setups in the order they must run, skipping the empty ones.
    pub(super) fn queue_prebakes(&mut self, chain: &[PrebakeScope], reinstall_after: bool) {
        let first_request = self.next_request_id;
        let mut queued = Vec::new();
        for &scope in chain {
            let source = Arc::<str>::from(self.stored_prebake(scope));
            if prebake::is_blank(&source) {
                continue;
            }
            let request_id = self.next_request_id;
            self.next_request_id = self.next_request_id.saturating_add(1);
            queued.push(PrebakeRequest {
                request_id,
                scope,
                source,
                reinstall_after: false,
            });
        }
        let Some(last) = queued.last_mut() else {
            return;
        };
        if let Some(feedback) = &mut self.evaluation_feedback
            && feedback.last_request == first_request
        {
            feedback.last_request = last.request_id;
        }
        // Only the end of a chain re-installs, and only once.
        last.reinstall_after = reinstall_after;
        let names = queued
            .iter()
            .map(|request| request.scope.label())
            .collect::<Vec<_>>()
            .join(", then ");
        self.prebake_queue.extend(queued);
        self.status = format!("applying the {names} prebake…");
        self.flush_pending_prebake();
    }

    /// Hand the next setup to the engine. One at a time, so the order the
    /// queue holds is the order the heap sees.
    pub(super) fn flush_pending_prebake(&mut self) {
        if !self.engine_connected {
            self.finish_evaluation_feedback(false);
            self.prebake_queue.clear();
            self.prebake_inflight = None;
            return;
        }
        if self.prebake_inflight.is_some() {
            return;
        }
        let Some(request) = self.prebake_queue.pop_front() else {
            return;
        };
        match self.worker.try_evaluate_prebake(
            request.request_id,
            request.scope,
            Arc::clone(&request.source),
        ) {
            Ok(()) => {
                // A setup that throws later may still have defined its helper.
                self.sent_setup_selected_variants |=
                    rustel_runtime::sounds::variant_selection(&request.source).is_some();
                self.prebake_inflight = Some(request);
            }
            // The queue is full: keep it at the front and try next turn.
            Err(EvaluationSendError::Full(_)) => self.prebake_queue.push_front(request),
            Err(EvaluationSendError::Disconnected(_)) => self.mark_engine_disconnected(),
        }
    }

    /// What became of one setup.
    pub(super) fn handle_prebake(&mut self, outcome: PrebakeOutcome) {
        let Some(sent) = self
            .prebake_inflight
            .take_if(|pending| pending.request_id == outcome.request_id)
        else {
            return;
        };
        let success = outcome_success(&outcome.result);
        self.finish_evaluation_request(outcome.request_id, success);
        if success != Some(true) {
            // A setup that does not apply clears the queue behind it, so a
            // gesture waiting there settles the same way.
            let discarded = self
                .prebake_queue
                .iter()
                .map(|request| request.request_id)
                .collect::<Vec<_>>();
            for request_id in discarded {
                self.finish_evaluation_request(request_id, success);
            }
        }
        let index = sent.scope.index();
        let hash = source_revision(&sent.source);
        match outcome.result {
            Ok(()) => {
                self.prebake_applied[index] = Some(hash);
                self.prebake_rejected[index] = None;
                self.clear_prebake_error(sent.scope);
                // What it defined is what the checker may now accept in the
                // scores that follow it.
                self.applied_setup[index] =
                    Arc::new(rustel_transpiler::setup_definitions(&sent.source));
                self.lint_pending = true;
                self.log.push(
                    LogLevel::Info,
                    "setup",
                    format!("{} applied", sent.scope.tab_name()),
                );
                self.status = format!("{} applied", sent.scope.tab_name());
                if sent.reinstall_after && self.prebake_queue.is_empty() {
                    self.reinstall_audible_score();
                }
            }
            Err(failure) if failure.kind == "cancelled" => {
                self.prebake_queue.clear();
                self.status = format!("{} - {}", sent.scope.tab_name(), failure.message);
            }
            Err(failure) => {
                self.prebake_rejected[index] = Some(hash);
                // A refused setup ends its chain: the set's own setup is
                // written against a global one that did not run, and
                // nothing sounding should be rebuilt on half a heap.
                self.prebake_queue.clear();
                let message = self
                    .note_untrusted_import(&failure)
                    .or_else(|| self.note_csound_refusal(&failure))
                    .unwrap_or_else(|| format!("{}: {}", failure.kind, failure.message));
                self.set_error(
                    ErrorOwner::Setup,
                    format!("{}: {message}", sent.scope.tab_name()),
                );
            }
        }
        self.flush_pending_prebake();
        self.dirty_frame = true;
    }

    /// Install the sounding score again, from the text that is sounding, so
    /// the music picks up what the setup just defined.
    ///
    /// The ordinary evaluate path, so continuity, the cutover and the launch
    /// setting all behave as they always do. Stopped, nothing happens at
    /// all: a setup never starts the transport.
    fn reinstall_audible_score(&mut self) {
        if !self.is_playing() || self.is_stopping() {
            return;
        }
        // A launch already waiting carries its own text and evaluates it on
        // its line, so it takes the new setup by itself - and installing
        // now would cancel the launch the artist is waiting for.
        if self.armed_request.is_some() {
            self.status = "setup applied - the armed launch takes it on its line".into();
            return;
        }
        let (Some(scene), Some(revision), Some(source)) = (
            self.audible_scene,
            self.visual_revision,
            self.evaluated_source.clone(),
        ) else {
            return;
        };
        if self.scenes.get(scene).is_none() {
            return;
        }
        self.log.push(
            LogLevel::Info,
            "setup",
            "re-installing the sounding score with the new setup",
        );
        // Not on the tape: nobody changed a score, and a replay that
        // installed one here would be replaying a gesture that never
        // happened.
        self.queue_evaluation_for(scene, revision, source, false);
    }
}

#[cfg(test)]
mod tests {
    mod prebakes {
        //! Tests for `app/prebakes.rs`.

        use crate::app::tests::support::*;
        use crate::app::*;

        /// Enter on a prebake row opens it as a tab and hands over the keyboard.
        #[test]
        fn the_settings_sheet_opens_a_prebake_as_a_tab() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = app_over(directory.path());

            app.toggle_settings_sheet();
            assert_eq!(app.focus, Focus::Panel(PanelKind::Settings));
            // Up from the top wraps to the last row (full paths), then trim
            // recordings, then the local prebake.
            press(&mut app, KeyCode::Up, KeyModifiers::NONE);
            press(&mut app, KeyCode::Up, KeyModifiers::NONE);
            press(&mut app, KeyCode::Up, KeyModifiers::NONE);
            press(&mut app, KeyCode::Enter, KeyModifiers::NONE);

            assert!(app.settings_sheet.is_none(), "the sheet handed over");
            assert_eq!(app.focus, Focus::Editor);
            assert_eq!(app.current_prebake(), Some(PrebakeScope::Local));
            assert_eq!(app.editor().source(), prebake::PREBAKE_STARTER);
            assert!(!app.scenes.current().dirty, "a tab opens on what is stored");
            assert_eq!(app.scenes.score_count(), 1);
            assert_eq!(app.scenes.len(), 2);
            assert!(app.status.contains("prebake (local)"), "{}", app.status);
        }

        /// A prebake tab is never a file: its text goes where its scope keeps
        /// it, and the folder gains no score.
        #[test]
        fn a_prebake_tab_is_kept_but_never_written_as_a_score() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = app_over(directory.path());

            app.open_prebake(PrebakeScope::Global);
            type_text(&mut app, "globalThis.riff = 1");
            assert!(app.scenes.current().dirty);
            press(&mut app, KeyCode::Char('w'), KeyModifiers::CONTROL);

            assert_eq!(app.scenes.len(), 1, "the tab closed");
            assert!(app.global_prebake.contains("globalThis.riff = 1"));
            let file = directory.path().join("config").join("prebake.strudel");
            assert!(file.exists(), "the global prebake was not kept");
            assert!(
                std::fs::read_to_string(&file)
                    .expect("read")
                    .contains("globalThis.riff = 1")
            );
            let scores = std::fs::read_dir(directory.path())
                .expect("read dir")
                .filter_map(Result::ok)
                .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "strudel"))
                .map(|entry| entry.file_name().to_string_lossy().into_owned())
                .collect::<Vec<_>>();
            assert_eq!(
                scores,
                ["first.strudel"],
                "a prebake tab was written as a score, or the global one as a scene"
            );
            assert!(
                !directory.path().join("rustel-set.json").exists(),
                "a global prebake earned the set a project file"
            );
        }

        /// Closing a dirty local prebake keeps it in the set's project file.
        #[test]
        fn closing_a_local_prebake_keeps_it_in_the_project_file() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = app_over(directory.path());

            app.open_prebake(PrebakeScope::Local);
            type_text(&mut app, "globalThis.k = 1");
            press(&mut app, KeyCode::Char('w'), KeyModifiers::CONTROL);

            let manifest = std::fs::read_to_string(directory.path().join("rustel-set.json"))
                .expect("project file");
            assert!(manifest.contains("globalThis.k = 1"), "{manifest}");
            assert!(manifest.contains("first.strudel"));
            assert!(
                !manifest.contains("prebake (local)"),
                "the tab was listed as a scene: {manifest}"
            );
        }

        /// A prebake that does not check out is still kept - nothing is lost by
        /// closing - but it says so, and keeps saying so.
        #[test]
        fn closing_a_prebake_that_does_not_check_out_keeps_it_and_says_so() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = app_over(directory.path());

            app.open_prebake(PrebakeScope::Global);
            type_text(&mut app, "globalThis.a = (");
            press(&mut app, KeyCode::Char('w'), KeyModifiers::CONTROL);

            assert!(
                app.global_prebake.contains("globalThis.a = ("),
                "the text was thrown away"
            );
            assert_eq!(
                app.prebake_rows()[PrebakeScope::Global.index()].verdict,
                PrebakeVerdict::Rejected
            );
            let error = app.errors.get(ErrorOwner::Setup).expect("a setup error");
            assert!(error.contains("prebake (global)"), "{error}");

            // A later score refusal does not take the setup's message away: the
            // setup is still broken after the newer message.
            app.set_error(ErrorOwner::Evaluation, "something else".into());
            assert!(app.errors.get(ErrorOwner::Setup).is_some());
        }

        /// Update on a prebake tab keeps it, runs it, and never pretends to be
        /// an evaluation.
        #[test]
        fn update_on_a_prebake_tab_keeps_it_and_queues_it_without_touching_the_score() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = app_over(directory.path());

            app.open_prebake(PrebakeScope::Local);
            type_text(&mut app, "globalThis.riff = () => note('c')");
            press(&mut app, KeyCode::F(5), KeyModifiers::NONE);

            assert!(
                app.scenes.local_prebake().contains("globalThis.riff"),
                "update did not keep the setup"
            );
            assert!(!app.scenes.current().dirty, "the tab stayed dirty");
            assert!(app.inflight.is_empty(), "a setup went out as an evaluation");
            assert!(app.pending_evaluation.is_none());
            assert!(app.armed_scene.is_none());
            assert_eq!(app.next_launch, Launch::Now);

            settle(&mut app);
            assert_eq!(
                app.prebake_rows()[PrebakeScope::Local.index()].verdict,
                PrebakeVerdict::Applied,
                "status: {}",
                app.status
            );
            assert!(
                app.applied_setup[PrebakeScope::Local.index()]
                    .names
                    .iter()
                    .any(|name| name == "riff"),
                "the checker was not told what the setup defined"
            );
        }

        /// A global update runs global first and the set's own behind it, so
        /// one heap sees them in the documented order.
        #[test]
        fn a_global_update_reapplies_the_local_one_behind_it() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = app_over(directory.path());
            app.scenes.set_local_prebake("globalThis.l = 1");

            app.open_prebake(PrebakeScope::Global);
            type_text(&mut app, "globalThis.g = 1");
            press(&mut app, KeyCode::F(5), KeyModifiers::NONE);

            let queued = std::iter::once(
                app.prebake_inflight
                    .as_ref()
                    .map(|request| request.scope)
                    .expect("the global one went first"),
            )
            .chain(app.prebake_queue.iter().map(|request| request.scope))
            .collect::<Vec<_>>();
            assert_eq!(queued, [PrebakeScope::Global, PrebakeScope::Local]);
            assert!(
                !app.prebake_inflight
                    .as_ref()
                    .expect("in flight")
                    .reinstall_after,
                "only the end of a chain re-installs"
            );
        }

        /// An empty prebake is kept as nothing and never run.
        #[test]
        fn an_empty_prebake_is_kept_as_nothing_and_never_run() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = app_over(directory.path());

            app.open_prebake(PrebakeScope::Global);
            // The starter is comments, which are text; empty it entirely.
            app.scenes.current_mut().editor = crate::editor::Editor::new("   ").expect("editor");
            press(&mut app, KeyCode::F(5), KeyModifiers::NONE);

            assert!(app.prebake_inflight.is_none());
            assert!(app.prebake_queue.is_empty());
            assert!(app.global_prebake.is_empty());
            assert!(
                !directory
                    .path()
                    .join("config")
                    .join("prebake.strudel")
                    .exists(),
                "an empty prebake left a file behind"
            );
            assert!(app.status.contains("empty"), "{}", app.status);
        }

        /// A prebake the checker refuses is neither kept nor run.
        #[test]
        fn a_prebake_the_checker_refuses_is_not_applied() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = app_over(directory.path());

            app.open_prebake(PrebakeScope::Global);
            type_text(&mut app, "setcps(1)");
            press(&mut app, KeyCode::F(5), KeyModifiers::NONE);

            assert!(app.prebake_inflight.is_none(), "a refused setup was run");
            assert!(app.global_prebake.is_empty(), "a refused setup was kept");
            assert!(app.status.contains("refused"), "{}", app.status);
            assert!(
                app.status.contains("setup was not applied"),
                "{}",
                app.status
            );
        }

        /// A score may call what an applied setup defined; without it, the same
        /// score is an unknown function.
        #[test]
        fn a_score_calling_a_setup_helper_is_not_refused() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = app_over(directory.path());
            let score = Arc::<str>::from("$: riff()");
            let revision = app.editor().revision();

            assert!(
                app.refuse_unclean(revision, &score),
                "without a setup this really is unknown"
            );

            app.applied_setup[PrebakeScope::Global.index()] = Arc::new(
                rustel_transpiler::setup_definitions("globalThis.riff = () => note('c')"),
            );
            assert!(
                !app.refuse_unclean(revision, &score),
                "the setup's names did not reach the checker"
            );
        }

        /// A setup applying while a score sounds installs that score again, off
        /// the tape, so the music takes the new definitions.
        #[test]
        fn a_setup_reinstalls_the_sounding_score_but_never_records_it() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = app_over(directory.path());
            let scene = app.scenes.current().id;
            let source = Arc::<str>::from("$: s(\"bd\")");

            app.snapshot = Some(sounding(true, false));
            app.installed_revision = Some(source_revision(&source));
            app.audible_scene = Some(scene);
            app.visual_revision = Some(app.editor().revision());
            app.evaluated_source = Some(Arc::clone(&source));
            app.ui_settings.quantise = crate::settings::Quantise::Cycle;

            app.prebake_inflight = Some(PrebakeRequest {
                request_id: 4_242,
                scope: PrebakeScope::Global,
                source: Arc::from("globalThis.g = 1"),
                reinstall_after: true,
            });
            app.handle_prebake(PrebakeOutcome {
                request_id: 4_242,
                scope: PrebakeScope::Global,
                result: Ok(()),
            });

            let sent = app
                .pending_evaluation
                .as_ref()
                .or_else(|| app.inflight.values().next())
                .expect("the sounding score was not installed again");
            assert_eq!(&*sent.source, &*source);
            assert!(!sent.record, "the re-install reached the tape");
            assert_eq!(sent.launch, Launch::Quantised { unit_cycles: 1.0 });
            assert!(
                app.log
                    .lines()
                    .iter()
                    .any(|line| line.text.contains("re-installing")),
                "the log did not say why the score was installed again"
            );
        }

        /// Stopped, a setup changes the heap and nothing else: it never starts
        /// the transport, so there is nothing to re-install.
        #[test]
        fn nothing_is_reinstalled_while_the_studio_is_stopped() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = app_over(directory.path());
            app.snapshot = None;
            app.audible_scene = Some(app.scenes.current().id);
            app.visual_revision = Some(app.editor().revision());
            app.evaluated_source = Some(Arc::from("$: s(\"bd\")"));

            app.prebake_inflight = Some(PrebakeRequest {
                request_id: 7,
                scope: PrebakeScope::Global,
                source: Arc::from("globalThis.g = 1"),
                reinstall_after: true,
            });
            app.handle_prebake(PrebakeOutcome {
                request_id: 7,
                scope: PrebakeScope::Global,
                result: Ok(()),
            });

            assert!(app.pending_evaluation.is_none());
            assert!(app.inflight.is_empty());
        }

        /// A refused setup ends its chain and reports under `setup`, and the
        /// sounding score is left alone.
        #[test]
        fn a_refused_setup_ends_the_chain_and_never_reinstalls() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = app_over(directory.path());
            app.snapshot = Some(sounding(true, false));
            app.audible_scene = Some(app.scenes.current().id);
            app.visual_revision = Some(app.editor().revision());
            app.evaluated_source = Some(Arc::from("$: s(\"bd\")"));

            app.prebake_inflight = Some(PrebakeRequest {
                request_id: 9,
                scope: PrebakeScope::Global,
                source: Arc::from("globalThis.g = 1"),
                reinstall_after: false,
            });
            app.prebake_queue.push_back(PrebakeRequest {
                request_id: 10,
                scope: PrebakeScope::Local,
                source: Arc::from("globalThis.l = 1"),
                reinstall_after: true,
            });
            app.handle_prebake(PrebakeOutcome {
                request_id: 9,
                scope: PrebakeScope::Global,
                result: Err(crate::worker::EngineFailure {
                    kind: "evaluation".into(),
                    message: "boom".into(),
                    recoverable: true,
                    playback_stopped: false,
                }),
            });

            assert!(
                app.prebake_queue.is_empty(),
                "the set's setup ran on half a heap"
            );
            assert!(app.pending_evaluation.is_none() && app.inflight.is_empty());
            let error = app.errors.get(ErrorOwner::Setup).expect("a setup error");
            assert!(error.contains("prebake (global)"), "{error}");
            assert!(
                app.log
                    .lines()
                    .iter()
                    .any(|line| line.kind == "setup" && line.level == LogLevel::Error),
                "the log did not carry it under setup"
            );
        }

        /// A Stop is about the transport. It does not throw a setup away.
        #[test]
        fn a_stop_does_not_lose_a_queued_setup() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = app_over(directory.path());
            app.prebake_queue.push_back(PrebakeRequest {
                request_id: 3,
                scope: PrebakeScope::Local,
                source: Arc::from("globalThis.l = 1"),
                reinstall_after: false,
            });

            app.dispatch_editor(Command::Stop).expect("stop");
            assert_eq!(app.prebake_queue.len(), 1);
        }

        /// A prebake tab is not launched, renamed, padded or exported.
        #[test]
        fn a_prebake_tab_refuses_what_only_a_scene_answers_to() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = app_over(directory.path());
            app.open_prebake(PrebakeScope::Local);
            let index = app.scenes.current_index();

            app.launch_scene(index).expect("launch");
            assert!(app.inflight.is_empty() && app.pending_evaluation.is_none());
            assert!(app.status.contains("setup"), "{}", app.status);

            app.begin_rename();
            assert_eq!(app.strip_mode, SceneStripMode::Idle);
            app.begin_learn();
            assert_eq!(app.strip_mode, SceneStripMode::Idle);
            app.toggle_export_sheet();
            assert!(app.export_sheet.is_none());
            app.new_scene(true);
            assert_eq!(
                app.scenes.score_count(),
                1,
                "a setup was duplicated as a score"
            );
        }

        /// A setup stored broken is reported at startup and never run, and the
        /// scores open regardless.
        #[test]
        fn a_broken_stored_setup_is_reported_at_startup_and_the_scores_still_open() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = app_over(directory.path());
            app.global_prebake = "globalThis.a = (".to_owned();
            app.scenes.set_local_prebake("globalThis.l = 1");

            app.queue_startup_prebakes();

            let queued = app
                .prebake_inflight
                .iter()
                .map(|request| request.scope)
                .chain(app.prebake_queue.iter().map(|request| request.scope))
                .collect::<Vec<_>>();
            assert_eq!(queued, [PrebakeScope::Local], "a broken setup was run");
            assert!(app.errors.get(ErrorOwner::Setup).is_some());
            assert_eq!(app.scenes.score_count(), 1);
            assert_eq!(app.editor().source(), "$: s(\"bd\")\n");
        }
    }
}
