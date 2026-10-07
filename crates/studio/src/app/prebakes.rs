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
                self.set_error(
                    ErrorOwner::Setup,
                    format!(
                        "{}: {}: {}",
                        sent.scope.tab_name(),
                        failure.kind,
                        failure.message
                    ),
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
