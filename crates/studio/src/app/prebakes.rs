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

/// A score update waiting for changed setups to finish applying first.
#[derive(Clone, Debug)]
pub(super) struct DeferredScoreUpdate {
    pub(super) scene: SceneId,
    pub(super) revision: Revision,
    pub(super) source: Arc<str>,
    pub(super) request: LaunchRequest,
    pub(super) explicit: bool,
    pub(super) is_replay: bool,
    pub(super) write_scene: bool,
    pub(super) source_kind: &'static str,
}

/// Result of checking whether changed prebakes must run before a score update.
pub(super) enum PrebakePreparation {
    /// Every setup is already applied; proceed with the score immediately.
    Ready(DeferredScoreUpdate),
    /// Changed setups are queued or running; the score will run once they land.
    Queued,
    /// A changed setup failed linting; the score does not run.
    Refused,
}

impl App {
    /// A prebake's text as stored, whether or not a tab is showing it.
    pub(super) fn stored_prebake(&self, scope: PrebakeScope) -> &str {
        match scope {
            PrebakeScope::Global => &self.global_prebake,
            PrebakeScope::Local => self.scenes.local_prebake(),
        }
    }

    /// A prebake's live text: its open tab buffer when dirty, otherwise its
    /// stored text so opening an empty starter tab to inspect it never counts
    /// as a pending edit.
    fn live_prebake_source(&self, scope: PrebakeScope) -> std::borrow::Cow<'_, str> {
        if let Some(index) = self.scenes.prebake_index(scope)
            && let Some(scene) = self.scenes.scenes().get(index)
            && scene.dirty
        {
            return std::borrow::Cow::Owned(scene.editor.source());
        }
        std::borrow::Cow::Borrowed(self.stored_prebake(scope))
    }

    /// Whether `source` matches what the engine has already applied for `scope`.
    fn prebake_is_applied(&self, scope: PrebakeScope, source: &str) -> bool {
        if prebake::is_blank(source) {
            return self.prebake_applied[scope.index()].is_none();
        }
        self.prebake_applied[scope.index()].as_deref() == Some(source_revision(source).as_str())
    }

    /// Whether `source` has already been settled for `scope` (applied, or
    /// rejected with the exact same text). A prebake whose text has not
    /// changed since it failed stays in the setup error slot instead of
    /// re-running on every score update and blocking playback.
    fn prebake_is_settled(&self, scope: PrebakeScope, source: &str) -> bool {
        if self.prebake_is_applied(scope, source) {
            return true;
        }
        if prebake::is_blank(source) {
            return false;
        }
        self.prebake_rejected[scope.index()].as_deref() == Some(source_revision(source).as_str())
    }

    /// The newest source already queued or in flight for `scope`, if any.
    fn prebake_latest_queued_source(&self, scope: PrebakeScope) -> Option<&str> {
        self.prebake_queue
            .iter()
            .rev()
            .find(|request| request.scope == scope)
            .or_else(|| {
                self.prebake_inflight
                    .as_ref()
                    .filter(|request| request.scope == scope)
            })
            .map(|request| request.source.as_ref())
    }

    /// Whether either prebake has text that differs from what is settled.
    pub(super) fn has_unapplied_prebake(&self) -> bool {
        PrebakeScope::ALL.into_iter().any(|scope| {
            let source = self.live_prebake_source(scope);
            !self.prebake_is_settled(scope, &source)
        })
    }

    fn clear_applied_prebake(&mut self, scope: PrebakeScope) {
        let index = scope.index();
        self.prebake_applied[index] = None;
        self.prebake_rejected[index] = None;
        self.applied_setup[index] = Arc::default();
        self.lint_pending = true;
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

    /// Run the setup of a set just taken up, as a studio opening on that
    /// set would.
    ///
    /// What the last set's setup defined stays in the engine, but the
    /// checker no longer accepts it: an export of this set, or the next
    /// studio on it, runs this set's setup alone, and a score that leans on
    /// the old one would fail there.
    pub(super) fn take_up_set_prebake(&mut self) {
        let index = PrebakeScope::Local.index();
        self.set_first_request = self.next_request_id;
        self.applied_setup[index] = Arc::default();
        self.prebake_applied[index] = None;
        self.lint_pending = true;
        self.opening_prebake = if self.check_stored_prebake(PrebakeScope::Local) {
            // Quietly, as the set's levels come back: the status line is
            // the set being opened. A refusal still says so.
            let opened = std::mem::take(&mut self.status);
            let request = self.queue_prebakes(&[PrebakeScope::Local], false);
            self.status = opened;
            request
        } else {
            None
        };
    }

    /// Show a prebake in the focused pane, from the settings sheet, the
    /// Options menu, or a learnt chord.
    pub(super) fn open_prebake(&mut self, scope: PrebakeScope) {
        // The theme editor keeps an unsaved draft. Say so instead of
        // closing it and putting the theme back.
        if self.theme_editor.is_some() {
            self.status = "the theme editor is open - s saves, Esc goes back to the themes".into();
            self.dirty_frame = true;
            return;
        }
        // Setup belongs to the set view; a tape gives the studio back first.
        if self.replay_view.is_some() {
            self.close_replay();
        }
        // Sheets and prompts over the old surface would keep the keyboard
        // while the prebake is drawn under them. The log is reading
        // furniture and stays.
        self.dismiss_dialogs_except(&[PanelKind::Log]);
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
                self.end_pointer_gesture();
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
                self.live_numbers.remove(&id);
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

    /// What a local setup may take for granted from the global setup.
    pub(super) fn global_setup_lint_context(&self, global_override: Option<&str>) -> LintContext {
        if global_override.is_none()
            && self.prebake_is_applied(PrebakeScope::Global, &self.global_prebake)
        {
            let mut context = LintContext::default();
            context.add_setup_definitions(&self.applied_setup[PrebakeScope::Global.index()]);
            context
        } else {
            LintContext::from_setup_sources([
                global_override.unwrap_or(self.global_prebake.as_str())
            ])
        }
    }

    /// Check `source` for `scope` without running it, returning the first
    /// rejection reason if it does not check out.
    fn check_prebake_text(
        &mut self,
        scope: PrebakeScope,
        source: &str,
        global_override: Option<&str>,
    ) -> Result<(), String> {
        if prebake::is_blank(source) {
            self.prebake_rejected[scope.index()] = None;
            self.clear_prebake_error(scope);
            return Ok(());
        }
        let context = match scope {
            PrebakeScope::Global => LintContext::default().as_setup(),
            PrebakeScope::Local => self.global_setup_lint_context(global_override).as_setup(),
        };
        let library = self.worker.library();
        let diagnostics = lint_with(source, false, library.as_deref(), &context);
        match rejection_of(source, &diagnostics) {
            Some(reason) => {
                self.prebake_rejected[scope.index()] = Some(source_revision(source));
                self.set_error(
                    ErrorOwner::Setup,
                    format!("{} refused - {reason}", scope.tab_name()),
                );
                Err(reason)
            }
            None => {
                self.prebake_rejected[scope.index()] = None;
                self.clear_prebake_error(scope);
                Ok(())
            }
        }
    }

    /// Check what is stored without running it, and report a text that does
    /// not check out. Used where a prebake is kept but not applied: closing
    /// a tab, quitting, and opening a studio.
    pub(super) fn check_stored_prebake(&mut self, scope: PrebakeScope) -> bool {
        let stored = self.stored_prebake(scope).to_owned();
        self.check_prebake_text(scope, &stored, None).is_ok()
    }

    /// Apply any prebake whose text changed before running a score update,
    /// global then local. Returns `Ready` when nothing changed so the score
    /// evaluates right away.
    pub(super) fn prepare_prebakes_for_score(
        &mut self,
        update: DeferredScoreUpdate,
    ) -> PrebakePreparation {
        let global_source = self.live_prebake_source(PrebakeScope::Global).into_owned();
        let local_source = self.live_prebake_source(PrebakeScope::Local).into_owned();
        for (scope, text) in [
            (PrebakeScope::Global, global_source.as_str()),
            (PrebakeScope::Local, local_source.as_str()),
        ] {
            if prebake::is_blank(text)
                && (!self.prebake_is_applied(scope, text) || self.stored_prebake(scope) != text)
            {
                self.persist_prebake(scope, text);
                self.clear_applied_prebake(scope);
                self.clear_prebake_error(scope);
            }
        }
        let global_changed = !prebake::is_blank(&global_source)
            && !self.prebake_is_settled(PrebakeScope::Global, &global_source);
        let local_present = !prebake::is_blank(&local_source);
        let local_changed =
            local_present && !self.prebake_is_settled(PrebakeScope::Local, &local_source);
        if !global_changed && !local_changed {
            return PrebakePreparation::Ready(update);
        }

        let mut to_queue = Vec::new();
        if global_changed {
            if let Err(reason) = self.check_prebake_text(PrebakeScope::Global, &global_source, None)
            {
                self.status = format!("refused - {reason} · the setup was not applied");
                if update.explicit {
                    self.finish_evaluation_feedback(false);
                }
                self.dirty_frame = true;
                return PrebakePreparation::Refused;
            }
            self.persist_prebake(PrebakeScope::Global, &global_source);
            if self.prebake_latest_queued_source(PrebakeScope::Global)
                != Some(global_source.as_str())
            {
                to_queue.push(PrebakeScope::Global);
            }
            if local_present {
                if let Err(reason) = self.check_prebake_text(
                    PrebakeScope::Local,
                    &local_source,
                    Some(&global_source),
                ) {
                    self.status = format!("refused - {reason} · the setup was not applied");
                    if update.explicit {
                        self.finish_evaluation_feedback(false);
                    }
                    self.dirty_frame = true;
                    return PrebakePreparation::Refused;
                }
                self.persist_prebake(PrebakeScope::Local, &local_source);
                if !to_queue.is_empty()
                    || self.prebake_latest_queued_source(PrebakeScope::Local)
                        != Some(local_source.as_str())
                {
                    to_queue.push(PrebakeScope::Local);
                }
            }
        } else if local_changed {
            if let Err(reason) = self.check_prebake_text(PrebakeScope::Local, &local_source, None) {
                self.status = format!("refused - {reason} · the setup was not applied");
                if update.explicit {
                    self.finish_evaluation_feedback(false);
                }
                self.dirty_frame = true;
                return PrebakePreparation::Refused;
            }
            self.persist_prebake(PrebakeScope::Local, &local_source);
            if self.prebake_latest_queued_source(PrebakeScope::Local) != Some(local_source.as_str())
            {
                to_queue.push(PrebakeScope::Local);
            }
        }

        if !to_queue.is_empty() {
            self.queue_prebakes(&to_queue, false);
            self.deferred_score_update = Some(update);
            self.dirty_frame = true;
            return PrebakePreparation::Queued;
        }
        let inflight_defines_names = self.prebake_inflight.as_ref().is_some_and(|request| {
            let definitions = rustel_transpiler::setup_definitions(&request.source);
            !definitions.names.is_empty() || definitions.has_dynamic_names
        });
        if !self.prebake_queue.is_empty() || inflight_defines_names {
            self.deferred_score_update = Some(update);
            self.dirty_frame = true;
            return PrebakePreparation::Queued;
        }
        PrebakePreparation::Ready(update)
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
        let global_for_local = if scope == PrebakeScope::Local {
            let global_source = self.live_prebake_source(PrebakeScope::Global).into_owned();
            if !self.prebake_is_settled(PrebakeScope::Global, &global_source) {
                if prebake::is_blank(&global_source) {
                    self.persist_prebake(PrebakeScope::Global, &global_source);
                    self.clear_applied_prebake(PrebakeScope::Global);
                    self.clear_prebake_error(PrebakeScope::Global);
                    None
                } else {
                    if let Err(reason) =
                        self.check_prebake_text(PrebakeScope::Global, &global_source, None)
                    {
                        self.status = format!("refused - {reason} · the setup was not applied");
                        self.finish_evaluation_feedback(false);
                        self.dirty_frame = true;
                        return;
                    }
                    self.persist_prebake(PrebakeScope::Global, &global_source);
                    Some(global_source)
                }
            } else {
                None
            }
        } else {
            None
        };

        if self.refuse_unclean(revision, &source) {
            self.finish_evaluation_feedback(false);
            // Refused text is not kept: the tab still holds it, and closing
            // or quitting will, exactly as a refused score stays in its
            // buffer and reaches disk on the way out.
            self.prebake_rejected[scope.index()] = Some(source_revision(&source));
            self.dirty_frame = true;
            return;
        }
        let changed = !self.prebake_is_applied(scope, &source);
        self.persist_prebake(scope, &source);
        self.clear_prebake_error(scope);
        if prebake::is_blank(&source) {
            self.clear_applied_prebake(scope);
            if let Some(global_source) = global_for_local
                && !prebake::is_blank(&global_source)
            {
                self.queue_prebakes(&[PrebakeScope::Global], true);
                self.dirty_frame = true;
                return;
            }
            self.finish_evaluation_feedback(true);
            self.status = format!("{} is empty - nothing to run", scope.tab_name());
            self.dirty_frame = true;
            return;
        }

        // Order is the point: a fresh global setup is followed by the set's
        // own, so one heap always sees them global-then-local however they
        // were edited. A setup whose text has not changed since it applied
        // does not run again.
        let mut chain = Vec::new();
        match scope {
            PrebakeScope::Global => {
                if !changed {
                    self.finish_evaluation_feedback(true);
                    self.status = format!("{} unchanged - already applied", scope.tab_name());
                    self.dirty_frame = true;
                    return;
                }
                chain.push(PrebakeScope::Global);
                let local_source = self.live_prebake_source(PrebakeScope::Local).into_owned();
                if !prebake::is_blank(&local_source) {
                    if self.stored_prebake(PrebakeScope::Local) != local_source {
                        if let Err(reason) =
                            self.check_prebake_text(PrebakeScope::Local, &local_source, None)
                        {
                            self.status = format!("refused - {reason} · the setup was not applied");
                            self.finish_evaluation_feedback(false);
                            self.dirty_frame = true;
                            return;
                        }
                        self.persist_prebake(PrebakeScope::Local, &local_source);
                    }
                    chain.push(PrebakeScope::Local);
                }
            }
            PrebakeScope::Local => {
                let global_changed = global_for_local.is_some();
                if !global_changed && !changed {
                    self.finish_evaluation_feedback(true);
                    self.status = format!("{} unchanged - already applied", scope.tab_name());
                    self.dirty_frame = true;
                    return;
                }
                if global_changed {
                    chain.push(PrebakeScope::Global);
                }
                chain.push(PrebakeScope::Local);
            }
        }
        self.queue_prebakes(&chain, true);
        self.dirty_frame = true;
    }

    /// Queue setups in the order they must run, skipping the empty ones.
    /// Returns the last request queued, when there is one.
    pub(super) fn queue_prebakes(
        &mut self,
        chain: &[PrebakeScope],
        reinstall_after: bool,
    ) -> Option<u64> {
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
        let last = queued.last_mut()?;
        let last_request = last.request_id;
        if let Some(feedback) = &mut self.evaluation_feedback
            && feedback.last_request == first_request
        {
            feedback.last_request = last_request;
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
        Some(last_request)
    }

    /// Hand the next setup to the engine. One at a time, so the order the
    /// queue holds is the order the heap sees.
    pub(super) fn flush_pending_prebake(&mut self) {
        if !self.engine_connected {
            self.finish_evaluation_feedback(false);
            self.prebake_queue.clear();
            self.prebake_inflight = None;
            self.deferred_score_update = None;
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
        let stale_local =
            sent.scope == PrebakeScope::Local && sent.request_id < self.set_first_request;
        let waiting_for_score = !stale_local
            && success == Some(true)
            && self.prebake_queue.is_empty()
            && self.deferred_score_update.is_some();
        if !waiting_for_score {
            self.finish_evaluation_request(outcome.request_id, success);
        }
        // The last set's own setup, still out when another set was taken
        // up, says nothing about the set open now or the chain queued for it.
        if stale_local {
            self.flush_pending_prebake();
            self.dirty_frame = true;
            return;
        }
        let opening = self
            .opening_prebake
            .take_if(|request| *request == sent.request_id)
            .is_some();
        if success != Some(true) {
            if let Some(deferred) = self.deferred_score_update.take()
                && deferred.explicit
            {
                match success {
                    Some(false) => self.finish_evaluation_feedback(false),
                    None => self.evaluation_feedback = None,
                    Some(true) => {}
                }
            }
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
                // scores that follow it. Once the engine has evaluated the
                // setup, the runtime surface diff replaces static dynamic-name
                // uncertainty.
                let mut definitions = rustel_transpiler::setup_definitions(&sent.source);
                if let Some(runtime_names) = outcome.defined_names {
                    let mut seen: std::collections::HashSet<String> =
                        definitions.names.iter().cloned().collect();
                    for name in runtime_names {
                        if seen.insert(name.clone()) {
                            definitions.names.push(name);
                        }
                    }
                    definitions.has_dynamic_names = false;
                    definitions.has_dynamic_globals = false;
                    definitions.has_dynamic_methods = false;
                }
                self.applied_setup[index] = Arc::new(definitions);
                self.lint_pending = true;
                self.log.push(
                    LogLevel::Info,
                    "setup",
                    format!("{} applied", sent.scope.tab_name()),
                );
                if !opening {
                    self.status = format!("{} applied", sent.scope.tab_name());
                }
                if self.prebake_queue.is_empty() {
                    if let Some(deferred) = self.deferred_score_update.take() {
                        self.finish_score_update(deferred, true);
                    } else if sent.reinstall_after {
                        self.reinstall_audible_score();
                    }
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
                // The chain is over, so the line that said it was being
                // applied is too.
                if self.status.starts_with("applying the ") {
                    self.status = format!("{} refused", sent.scope.tab_name());
                }
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
        if self.launch_arming.is_armed() {
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
        // happened. A snippet preview stays, and so does the score that
        // closing the column puts back.
        self.queue_evaluation_for(scene, revision, source, false);
    }
}

#[cfg(test)]
mod tests {
    mod prebakes {
        //! Tests for `app/prebakes.rs`.

        use crate::app::tests::support::*;
        use crate::app::*;
        use crate::menu::MenuAction;

        /// The shortcut path opens the same tab and closes the sheet.
        #[test]
        fn the_prebake_actions_open_their_tabs_and_close_settings() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = app_over(directory.path());

            app.toggle_settings_sheet();
            assert!(
                app.dispatch_bind_action(BindAction::OpenGlobalPrebake, false)
                    .expect("global prebake action")
            );
            assert!(app.settings_sheet.is_none(), "the sheet handed over");
            assert_eq!(app.current_prebake(), Some(PrebakeScope::Global));
            assert_eq!(app.focus, Focus::Editor);

            app.toggle_settings_sheet();
            assert!(
                app.dispatch_bind_action(BindAction::OpenLocalPrebake, false)
                    .expect("local prebake action")
            );
            assert_eq!(app.current_prebake(), Some(PrebakeScope::Local));

            app.toggle_theme_picker();
            assert!(app.theme_picker.is_some(), "the picker is open");
            app.dispatch_menu_action(MenuAction::GlobalPrebake)
                .expect("global prebake menu row");
            assert_eq!(app.current_prebake(), Some(PrebakeScope::Global));
            assert!(
                app.theme_picker.is_none(),
                "the prebake dismissed the sheet over it"
            );
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
            assert!(app.launch_arming.armed_scene().is_none());
            assert_eq!(app.next_evaluation.launch, Launch::Now);

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

        /// A name from a script another scene imports stays available to every
        /// scene, so the checker does not refuse it in a scene with no import
        /// of its own.
        #[test]
        fn a_score_may_call_a_name_from_another_scenes_import() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = app_over(directory.path());
            let score = Arc::<str>::from("$: shimmer(s(\"hh\"))");
            let revision = app.editor().revision();
            assert!(
                app.refuse_unclean(revision, &score),
                "with no script anywhere this really is unknown"
            );

            app.scenes.current_mut().editor = Editor::new(
                "await import('http://127.0.0.1:8123/scripts.js')\n$: shimmer(s(\"bd\"))",
            )
            .unwrap();
            app.new_scene(false);
            let revision = app.editor().revision();
            assert!(
                !app.refuse_unclean(revision, &score),
                "the other scene's script may define it"
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
                defined_names: None,
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
                defined_names: None,
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
                defined_names: None,
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

        const FIRST_SETUP: &str = "globalThis.first = () => note('c')";
        const SECOND_SETUP: &str = "globalThis.second = () => note('e')";

        /// A studio over `directory` whose set has a setup of its own, sent
        /// to the engine as a studio opening on the set sends it.
        fn app_with_setup(directory: &std::path::Path) -> App {
            let mut app = app_over(directory);
            app.scenes.set_local_prebake(FIRST_SETUP);
            app.queue_startup_prebakes();
            app
        }

        /// The set at `directory`, with `setup` as its own.
        fn set_with_setup(directory: &std::path::Path, setup: &str) -> SceneSet {
            std::fs::write(directory.join("second.strudel"), "$: s(\"hh\")\n").expect("scene");
            let mut set = SceneSet::open(directory, "// starter").expect("set");
            set.set_local_prebake(setup);
            set
        }

        /// Whether the check lets a score call `helper`.
        fn accepts(app: &mut App, helper: &str) -> bool {
            let revision = app.editor().revision();
            !app.refuse_unclean(revision, &Arc::<str>::from(format!("$: {helper}()")))
        }

        /// A set taken up runs its own setup, as the next studio on it and
        /// its export do, and the last set's helpers are refused there. It
        /// runs quietly: the status line stays with the set that opened.
        #[test]
        fn a_set_taken_up_runs_its_own_setup_in_place_of_the_last_one() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = app_with_setup(directory.path());
            settle(&mut app);
            assert!(accepts(&mut app, "first"));

            let elsewhere = tempfile::tempdir().expect("second set");
            app.switch_set(set_with_setup(elsewhere.path(), SECOND_SETUP), "opened set");
            settle(&mut app);

            assert!(
                !app.status.contains("prebake"),
                "the setup took the status line: {}",
                app.status
            );
            assert_eq!(
                app.prebake_rows()[PrebakeScope::Local.index()].verdict,
                PrebakeVerdict::Applied
            );
            assert!(
                accepts(&mut app, "second"),
                "the new set's helper was refused"
            );
            assert!(
                !accepts(&mut app, "first"),
                "the last set's helper was still accepted"
            );
        }

        /// A set with no setup of its own refuses the last set's helpers
        /// too: its export and the next studio on it run no local setup.
        #[test]
        fn a_set_without_a_setup_refuses_the_last_sets_helpers() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = app_with_setup(directory.path());
            settle(&mut app);
            assert!(accepts(&mut app, "first"));

            let elsewhere = tempfile::tempdir().expect("second set");
            app.switch_set(set_with_setup(elsewhere.path(), ""), "opened set");

            assert!(app.prebake_inflight.is_none() && app.prebake_queue.is_empty());
            assert!(
                !accepts(&mut app, "first"),
                "the last set's helper was still accepted"
            );
        }

        /// The last set's setup, still running when another set is taken
        /// up, ends without a word about the new set: its helpers are not
        /// accepted there, its failure is not reported there, and the new
        /// set's own setup still goes out behind it.
        #[test]
        fn the_last_sets_setup_ending_late_leaves_the_new_set_alone() {
            for fails in [false, true] {
                let directory = tempfile::tempdir().expect("temp dir");
                let mut app = app_with_setup(directory.path());
                let late = app
                    .prebake_inflight
                    .as_ref()
                    .expect("the first set's setup went out")
                    .request_id;

                let elsewhere = tempfile::tempdir().expect("second set");
                app.switch_set(set_with_setup(elsewhere.path(), SECOND_SETUP), "opened set");
                app.handle_prebake(PrebakeOutcome {
                    request_id: late,
                    scope: PrebakeScope::Local,
                    result: if fails {
                        Err(crate::worker::EngineFailure {
                            kind: "evaluation".into(),
                            message: "boom".into(),
                            recoverable: true,
                            playback_stopped: false,
                        })
                    } else {
                        Ok(())
                    },
                    defined_names: None,
                });

                assert!(
                    app.errors.get(ErrorOwner::Setup).is_none(),
                    "the last set's setup was reported against this one"
                );
                assert!(
                    !accepts(&mut app, "first"),
                    "the last set's helper was accepted (fails: {fails})"
                );
                assert_eq!(
                    app.prebake_inflight
                        .as_ref()
                        .map(|request| &*request.source),
                    Some(SECOND_SETUP),
                    "the new set's setup did not go out (fails: {fails})"
                );
            }
        }

        /// Updating a score applies changed setups first, global then local,
        /// so a score sees edits without a separate Ctrl+S on each setup tab.
        #[test]
        fn a_score_update_applies_changed_prebakes_global_then_local_first() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = app_over(directory.path());

            app.open_prebake(PrebakeScope::Global);
            type_text(&mut app, "globalThis.fromGlobal = () => note('c3')");
            press(&mut app, KeyCode::Char('w'), KeyModifiers::CONTROL);

            app.open_prebake(PrebakeScope::Local);
            type_text(&mut app, "globalThis.fromLocal = () => fromGlobal()");
            press(&mut app, KeyCode::Char('w'), KeyModifiers::CONTROL);

            app.scenes.current_mut().editor = Editor::new("$: fromLocal()").unwrap();
            press(&mut app, KeyCode::F(5), KeyModifiers::NONE);

            let queued = app
                .prebake_inflight
                .iter()
                .map(|request| request.scope)
                .chain(app.prebake_queue.iter().map(|request| request.scope))
                .collect::<Vec<_>>();
            assert_eq!(queued, [PrebakeScope::Global, PrebakeScope::Local]);
            assert!(app.deferred_score_update.is_some());

            settle(&mut app);
            assert_eq!(
                app.prebake_rows()[PrebakeScope::Global.index()].verdict,
                PrebakeVerdict::Applied
            );
            assert_eq!(
                app.prebake_rows()[PrebakeScope::Local.index()].verdict,
                PrebakeVerdict::Applied
            );
            assert_eq!(app.errors.get(ErrorOwner::Setup), None);
            assert!(
                app.evaluation_revisions
                    .get(&source_revision("$: fromLocal()"))
                    .is_some(),
                "the deferred score was not dispatched after the prebakes applied"
            );
            assert!(
                app.errors
                    .get(ErrorOwner::Evaluation)
                    .is_none_or(
                        |error| !error.contains("fromLocal") && !error.contains("fromGlobal")
                    ),
                "evaluation error: {:?}",
                app.errors.get(ErrorOwner::Evaluation)
            );
        }

        /// Updating the local prebake applies a changed global prebake first.
        #[test]
        fn a_local_prebake_update_applies_a_changed_global_prebake_first() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = app_over(directory.path());

            app.open_prebake(PrebakeScope::Global);
            type_text(&mut app, "globalThis.fromGlobal = () => note('c3')");
            press(&mut app, KeyCode::Char('w'), KeyModifiers::CONTROL);

            app.open_prebake(PrebakeScope::Local);
            type_text(&mut app, "globalThis.fromLocal = () => fromGlobal()");
            press(&mut app, KeyCode::F(5), KeyModifiers::NONE);

            let queued = app
                .prebake_inflight
                .iter()
                .map(|request| request.scope)
                .chain(app.prebake_queue.iter().map(|request| request.scope))
                .collect::<Vec<_>>();
            assert_eq!(queued, [PrebakeScope::Global, PrebakeScope::Local]);

            settle(&mut app);
            assert_eq!(
                app.prebake_rows()[PrebakeScope::Global.index()].verdict,
                PrebakeVerdict::Applied
            );
            assert_eq!(
                app.prebake_rows()[PrebakeScope::Local.index()].verdict,
                PrebakeVerdict::Applied
            );
        }

        /// A prebake whose text has not changed since it applied does not run
        /// a second time.
        #[test]
        fn an_unchanged_applied_prebake_does_not_run_again() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = app_over(directory.path());

            app.open_prebake(PrebakeScope::Global);
            type_text(&mut app, "globalThis.g = 1");
            press(&mut app, KeyCode::F(5), KeyModifiers::NONE);
            settle(&mut app);

            let before = app.next_request_id;
            press(&mut app, KeyCode::F(5), KeyModifiers::NONE);
            assert_eq!(app.next_request_id, before);
            assert!(app.prebake_inflight.is_none() && app.prebake_queue.is_empty());

            app.open_prebake(PrebakeScope::Local);
            type_text(&mut app, "globalThis.l = 2");
            press(&mut app, KeyCode::F(5), KeyModifiers::NONE);
            let queued = app
                .prebake_inflight
                .iter()
                .map(|request| request.scope)
                .chain(app.prebake_queue.iter().map(|request| request.scope))
                .collect::<Vec<_>>();
            assert_eq!(queued, [PrebakeScope::Local]);
            settle(&mut app);

            let before_local_repeat = app.next_request_id;
            press(&mut app, KeyCode::F(5), KeyModifiers::NONE);
            assert_eq!(app.next_request_id, before_local_repeat);
            assert!(app.prebake_inflight.is_none() && app.prebake_queue.is_empty());
        }

        /// A prebake with computed writes still catches unknown functions at
        /// check time, and once applied exposes only its runtime-defined names
        /// so typos in later scores stay caught.
        #[test]
        fn a_dynamic_prebake_catches_unknown_calls_and_exposes_runtime_names() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = app_over(directory.path());

            app.open_prebake(PrebakeScope::Global);
            app.scenes.current_mut().editor = Editor::new(
                "function add(name) { window[name + 's'] = () => s(name); }\nadd('kick');\nmissingHelper();",
            )
            .unwrap();
            press(&mut app, KeyCode::F(5), KeyModifiers::NONE);
            assert!(
                app.prebake_inflight.is_none(),
                "a broken dynamic prebake should be refused before running"
            );
            assert!(
                app.status.contains("missingHelper")
                    || app
                        .errors
                        .get(ErrorOwner::Evaluation)
                        .is_some_and(|error| error.contains("missingHelper")),
                "status={:?}, eval_err={:?}",
                app.status,
                app.errors.get(ErrorOwner::Evaluation)
            );

            app.scenes.current_mut().editor = Editor::new(
                "function add(name) { window[name + 's'] = () => s(name); }\nadd('kick');",
            )
            .unwrap();
            press(&mut app, KeyCode::F(5), KeyModifiers::NONE);
            settle(&mut app);

            assert_eq!(app.errors.get(ErrorOwner::Setup), None);
            assert_eq!(
                app.prebake_rows()[PrebakeScope::Global.index()].verdict,
                PrebakeVerdict::Applied
            );
            press(&mut app, KeyCode::Char('w'), KeyModifiers::CONTROL);
            assert!(
                accepts(&mut app, "kicks"),
                "runtime-defined dynamic name was refused"
            );
            assert!(
                !accepts(&mut app, "kickss"),
                "typo was accepted after dynamic prebake applied"
            );
        }
    }
}
