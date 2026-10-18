//! Sending the score to the engine and handling what comes back: refusing
//! text the check does not pass, queueing and flushing evaluations, draining
//! engine control events (layouts, diagnostics, snapshots, stops), applying an
//! evaluation's outcome, and resetting state when the engine worker
//! disconnects. It also keeps the cache of evaluated revisions, places the
//! inline visual rows a layout asks for under their calls, and turns nested
//! engine refusals into the short error line shown in the footer.

use super::*;

pub(super) const MAX_EVALUATION_REVISIONS: usize = 16;

/// What a refused update says on the footer, as opposed to in the log.
///
/// The engine nests its reasons: the rollback wraps the conversion's
/// refusal, which wraps the voice's own complaint. Written out in full
/// that is
///
/// > the new score could not be played (scalar audio refused every onset
/// > of the replacement's first window (unison 100 exceeds the native
/// > supersaw's 32-voice ceiling)); the last audible score keeps playing
///
/// which is a fine thing to find in `studio.log` afterwards and no use at
/// all on one line under a set. Only two parts of it are the player's: the
/// innermost clause, which is the thing they can go and change, and the
/// tail, which is what happened to the sound. The rest names the parts of
/// the engine that passed the complaint along.
///
/// Anything that is not shaped like one of these is left exactly as it is.
pub(super) fn player_facing_error(message: &str) -> String {
    const OPENING: &str = "the new score could not be played (";
    let Some(rest) = message.strip_prefix(OPENING) else {
        return concise_evaluation_error(message);
    };
    // The deepest balanced group is the original complaint. Depth starts at
    // one because the prefix took the first bracket with it.
    let (mut depth, mut deepest, mut start, mut inner) = (1usize, 0usize, 0usize, None);
    let mut closed = None;
    for (at, character) in rest.char_indices() {
        match character {
            '(' => {
                depth += 1;
                if depth > deepest {
                    deepest = depth;
                    start = at + character.len_utf8();
                }
            }
            ')' => {
                if depth == deepest && inner.is_none() {
                    inner = Some(&rest[start..at]);
                }
                depth -= 1;
                if depth == 0 {
                    closed = Some(at + character.len_utf8());
                    break;
                }
            }
            _ => {}
        }
    }
    let reason = match inner {
        // No nesting at all: the whole bracket is the reason.
        None => match closed {
            Some(end) => &rest[..end - 1],
            None => return message.to_owned(),
        },
        Some(inner) => inner,
    };
    let tail = closed
        .map(|end| rest[end..].trim_start_matches([';', '.', ' ']).trim())
        .filter(|tail| !tail.is_empty());
    concise_evaluation_error(&match tail {
        Some(tail) => format!("{reason} - {tail}"),
        None => reason.to_owned(),
    })
}

/// Remove engine routing from an evaluation error while retaining the cause
/// and its source location. The full message is still written to Studio Log.
fn concise_evaluation_error(message: &str) -> String {
    let mut reason = message.trim();
    // These are typed elsewhere; on the footer they merely repeat that the
    // red line is an evaluation error.
    loop {
        let stripped = ["evaluation: ", "javascript: "]
            .into_iter()
            .find_map(|prefix| reason.strip_prefix(prefix));
        let Some(stripped) = stripped else { break };
        reason = stripped;
    }

    const REPLACEMENT: &str = "replacement query failed; last-good score kept (";
    if let Some(inner) = reason
        .strip_prefix(REPLACEMENT)
        .and_then(|rest| rest.strip_suffix(')'))
    {
        reason = inner;
    }

    // Callback IDs help correlate engine internals in the log, but the score
    // location after the cause is what lets a musician fix the line.
    if let Some(rest) = reason.strip_prefix("pattern callback ")
        && let Some((id, cause)) = rest.split_once(" failed: ")
        && !id.is_empty()
        && id.chars().all(|character| character.is_ascii_digit())
    {
        reason = cause;
    }

    for class in [
        "TypeError: ",
        "ReferenceError: ",
        "SyntaxError: ",
        "RangeError: ",
        "EvalError: ",
        "Error: ",
    ] {
        if let Some(cause) = reason.strip_prefix(class) {
            reason = cause;
            break;
        }
    }
    reason.to_owned()
}

#[derive(Clone)]
pub(super) struct PendingEvaluation {
    pub(super) request_id: u64,
    /// The scene whose text went out, at `editor_revision`.
    pub(super) scene: SceneId,
    pub(super) editor_revision: Revision,
    pub(super) source: Arc<str>,
    pub(super) launch: Launch,
    /// Whether the score starts from its own cycle zero when it lands.
    pub(super) rewind: bool,
    /// Whether this install belongs on the tape.
    ///
    /// Every evaluate somebody asked for does. The one that does not is the
    /// re-install a prebake triggers: the artist changed no score, and a
    /// tape saying they did would replay a gesture that never happened.
    pub(super) record: bool,
}

/// A block's identity survives switching editors, including between tapes.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct ReplayBlock {
    pub(super) path: PathBuf,
    pub(super) index: usize,
}

impl App {
    /// Keep the evaluated revision mappable in the audible editor however
    /// many edits follow - a held slider key is one per repeat, and the
    /// widgets and marks are all expressed in that revision's coordinates.
    pub(super) fn pin_visual_revision(&mut self) {
        if let Some((scene, revision)) = self.visual_pin.take()
            && let Some(previous) = self.scenes.get_mut(scene)
        {
            previous.editor.unpin_revision(revision);
        }
        if let (Some(scene), Some(revision)) = (self.audible_scene, self.visual_revision)
            && let Some(current) = self.scenes.get_mut(scene)
        {
            current.editor.pin_revision(revision);
            self.visual_pin = Some((scene, revision));
        }
    }

    /// Place the layout's inline widgets in the audible scene, following
    /// each anchor through any edits made since the score was evaluated.
    /// Rows whose call has since been deleted or commented out are simply
    /// not placed.
    pub(super) fn install_virtual_rows(&mut self, revision: Revision) {
        let Some(audible) = self.audible_scene else {
            return;
        };
        let Some(layout) = self.visual.layout() else {
            return;
        };
        let Some(editor) = self.audible_editor() else {
            return;
        };
        let current = editor.revision();
        let evaluated = self.evaluated_source.as_deref();
        let rows = if self.ui_settings.animation {
            match virtual_rows_for(
                layout,
                |offset| editor.map_offset_since(revision, offset),
                |visual| visual_is_alive(editor, revision, evaluated, visual),
            ) {
                Ok(rows) => rows,
                Err(error) => {
                    self.set_error(ErrorOwner::Interface, error);
                    return;
                }
            }
        } else {
            // Visualizers are off: their rows collapse and the code takes
            // the space back, until the switch flips again.
            Vec::new()
        };
        if rows == editor.virtual_rows() {
            return;
        }
        let Some(scene) = self.scenes.get_mut(audible) else {
            return;
        };
        match scene.editor.set_virtual_rows(current, rows) {
            Ok(()) => {
                self.clear_error(ErrorOwner::Interface);
                self.invalidate_maps();
                self.dirty_frame = true;
            }
            Err(error) => self.set_error(ErrorOwner::Interface, error.to_string()),
        }
    }

    /// Check the tab on screen and refuse it if it does not check out,
    /// leaving the findings where the underline and the footer read them.
    ///
    /// True when the text was refused. Shared by the score path and the
    /// prebake path, which differ only in what the checker is told the text
    /// is and in what the message says was not taken.
    pub(super) fn refuse_unclean(&mut self, revision: Revision, source: &Arc<str>) -> bool {
        let scene = self.scenes.current();
        let (id, prebake) = (scene.id, scene.prebake());
        // An import whose failure has rested a few seconds is asked for
        // again. This update is still refused for it; the next one is not,
        // once it is in.
        self.retry_failed_imports(id, false);
        let mini = self.options.mini && scene.is_score();
        let context = self.lint_context_for(scene);
        let library = self.worker.library();
        let diagnostics = lint_with(source, mini, library.as_deref(), &context);
        let Some(reason) = rejection_of(source, &diagnostics) else {
            return false;
        };
        // Where it was refused is marked in the editor unless the syntax
        // check is off. The log and the footer say why whatever the mode: an
        // error that keeps a score from playing is never kept quiet.
        if self.ui_settings.syntax_check != SyntaxCheck::Off {
            self.lint.insert(
                id,
                LintResult {
                    scene: id,
                    revision,
                    input_channels: context.input_channels,
                    diagnostics,
                },
            );
        }
        let refused = match prebake {
            Some(scope) => format!("{} refused - {reason}", scope.tab_name()),
            None => format!("refused - {reason}"),
        };
        self.log
            .push_alert(LogLevel::Warn, "check", refused, lint_alert(id));
        self.status = match prebake {
            Some(_) => format!("refused - {reason} · the setup was not applied"),
            None => format!("refused - {reason} · the last good score keeps playing"),
        };
        self.update_failure = Some((id, revision));
        true
    }

    pub(super) fn queue_evaluation(&mut self, revision: Revision, source: Arc<str>) {
        // Playing the score is the end of a preview under it: what goes out
        // now is the score alone, so the snippet is no longer sounding and
        // nothing is owed a restore.
        #[cfg(feature = "hydra")]
        {
            self.snippet_preview = None;
        }
        let scene = self.scenes.current().id;
        self.queue_evaluation_for(scene, revision, source, true);
    }

    /// Apply the focused score without a keypress, once typing has paused.
    ///
    /// The launch row names the line the edit lands on, the same line an
    /// update waits for. Only while the set plays: starting the transport
    /// is a decision the studio does not make on its own. The
    /// text owes everything an update owes - the check, the write - and a
    /// refusal leaves the last good score playing, said in the footer the
    /// same way. What it never owes is the flash: that confirms a press,
    /// and nobody pressed anything.
    pub(super) fn auto_evaluate(&mut self) {
        // The snapshot trails a Stop press by up to 100 ms, so the local
        // flag holds the apply until it catches up.
        if !self.ui_settings.auto_evaluate
            || !self.is_playing()
            || self.is_stopping()
            || self.stop_requested
            || self.current_prebake().is_some()
            || self.current_replay().is_some()
        {
            return;
        }
        let scene = self.scenes.current();
        if !scene.is_score() {
            return;
        }
        let (id, revision) = (scene.id, scene.editor.revision());
        let source: Arc<str> = Arc::from(scene.editor.source());
        // What is playing is already this text, byte for byte. An auto
        // apply has nothing to say about it, unlike a press, which is owed
        // an answer for the gesture alone.
        if self.install_would_be_redundant(id, &source) {
            self.rebind_unchanged_score(id, revision, Arc::clone(&source));
            return;
        }
        // Already armed for its line: pressing again would only cancel and
        // re-arm it on the same line, and applying again is the same
        // non-press twice over.
        if self.armed_revision.as_deref() == Some(source_revision(&source).as_str()) {
            return;
        }
        // Audio dropped into the open set plays from this update, so the
        // set is read again before the score's sound names are judged.
        self.refresh_set_samples();
        if self.refuse_unclean(revision, &source) {
            // A refused launch is not a launch: nothing inherits a line or
            // a rewind from an update that never happened.
            self.next_launch = Launch::Now;
            self.next_rewind = false;
            return;
        }
        self.log.push(
            LogLevel::Debug,
            "transport",
            format!(
                "auto update - {} ({} bytes)",
                self.scenes.current().name(),
                source.len()
            ),
        );
        self.silence_preview();
        self.playing_pane = Some(self.focused);
        // The file is what you hear, whoever asked for the update.
        self.write_scene(id, revision, Arc::clone(&source));
        self.queue_evaluation(revision, source);
    }

    /// Run the update a permission just unblocked, without a keypress: the
    /// score or setup that was refused runs again by itself, so permitting
    /// is the whole walk, not half of one.
    ///
    /// Like the auto apply, it owes no flash - nobody pressed anything. And
    /// it runs only what is still the refused text, on screen: an edit since
    /// the refusal owns the next run, and so does a tab the player has
    /// moved to.
    pub(super) fn retry_refused_update(&mut self) {
        if self.current_replay().is_some() {
            return;
        }
        // A refused setup re-runs the way its tab presses Ctrl+S - its own
        // check, keep, and chain - when the refused text is still the one
        // in front of the player.
        if let Some(scope) = self.current_prebake() {
            let scene = self.scenes.current();
            let source: Arc<str> = Arc::from(scene.editor.source());
            let refused = self.prebake_rejected[scope.index()].as_deref()
                == Some(source_revision(&source).as_str());
            let revision = scene.editor.revision();
            if refused {
                self.update_prebake(scope, revision, source);
            }
            return;
        }
        let Some((scene, revision)) = self.update_failure else {
            return;
        };
        // The refused score is the buffer, unchanged since, and still in
        // front of the player. Anything else already has an owner: the next
        // edit, the next press, or the tab the player moved to.
        let current = self.scenes.current();
        let refused =
            current.id == scene && current.is_score() && current.editor.revision() == revision;
        if !refused {
            return;
        }
        let source: Arc<str> = Arc::from(current.editor.source());
        self.refresh_set_samples();
        if self.refuse_unclean(revision, &source) {
            return;
        }
        self.log.push(
            LogLevel::Debug,
            "transport",
            format!(
                "re-running the refused update - {} ({} bytes)",
                self.scenes.current().name(),
                source.len()
            ),
        );
        self.silence_preview();
        self.playing_pane = Some(self.focused);
        // The file is what you hear, whoever asked for the update.
        self.write_scene(scene, revision, Arc::clone(&source));
        self.queue_evaluation_for(scene, revision, source, true);
    }

    /// Queue an evaluation against a named scene rather than the focused
    /// one, and say whether it belongs on the tape. Both matter for the
    /// re-install a prebake triggers: it lands on whatever is SOUNDING,
    /// which may not be on screen, and nobody typed it.
    pub(super) fn queue_evaluation_for(
        &mut self,
        scene: SceneId,
        revision: Revision,
        source: Arc<str>,
        record: bool,
    ) {
        // A tape plays on its own clock. The launch setting is about the
        // set - a line to land a change on - and a re-enactment has no use
        // for it: a block waiting on a cycle line leaves the set's music
        // playing over the tape, and the next block, due a second later,
        // cancels the wait before it lands. So a replay takes the transport
        // at once, whatever the setting says.
        let replaying = self.replays.contains_key(&scene);
        if replaying {
            self.forget_arming();
            self.next_launch = Launch::Now;
        }
        // The launch setting is the default for every way of playing, not a
        // modifier-key privilege: an ordinary Ctrl+S while the set plays
        // waits for the same cycle line a pad does, with the same countdown.
        // Stopped, there is no line to wait for - updates play now.
        // A preview is the exception, for the same reason a replay is: it
        // takes the transport at once. Waiting for a cycle line would
        // leave the snippet it replaces playing over the one that was
        // asked for.
        #[cfg(feature = "hydra")]
        let auditioning = self.snippet_preview.is_some();
        #[cfg(not(feature = "hydra"))]
        let auditioning = false;
        if !replaying
            && !auditioning
            && matches!(self.next_launch, Launch::Now)
            && self.is_playing()
            && !self.is_stopping()
            && let Some(unit_cycles) = self.ui_settings.quantise.unit_cycles()
        {
            self.next_launch = Launch::Quantised { unit_cycles };
        }
        // Something is being installed, so the set is not on its way out
        // any more, whatever the last snapshot still says.
        self.stop_requested = false;
        // Nothing played from the replay view goes on a tape: a run is
        // the tape being heard again, and a save in the tab is an edit
        // of a block, not a new one - so a tape never grows for being
        // replayed.
        let record = record && !self.replays.contains_key(&scene);
        let request_id = self.next_request_id;
        self.next_request_id = self.next_request_id.saturating_add(1);
        let revision_hash = source_revision(&source);
        let replay_block = self.replays.get(&scene).and_then(|tab| {
            tab.playing().map(|index| ReplayBlock {
                path: tab.path.clone(),
                index,
            })
        });
        self.evaluation_revisions.remember(
            revision_hash.clone(),
            revision,
            Arc::clone(&source),
            scene,
            replay_block,
        );
        self.bind_live_sliders(scene, revision, &source);
        // From zero, or joining the cycle already running.
        //
        // A replay is on its own clock and is not rewound here. What is
        // left is the scene's own answer - the ⟲ on its chip - and ^⇧S,
        // which asks for one rewind whatever the scene says.
        //
        // The scene carries it rather than the gesture because it is a
        // property of the score: a one-cycle loop does not care where it
        // joins, and an `<a b>` is heard from its second half when it
        // joins wherever the clock happened to stand. One pad mapped to
        // one scene therefore does the right thing without a second pad
        // for the rewinding kind.
        let flagged = self
            .scenes
            .get(scene)
            .is_some_and(|scene| scene.rewind && scene.is_score());
        // A preview that wants its beginning plants the flag itself before
        // calling; the gate must not swallow it - a restart sent ahead of
        // the install cannot say which install it is for, so the flag on
        // the install is the only way a snippet reliably starts at its
        // start. The scene's ⟲ stays a property of the set: a preview
        // under a score joins the set's cycle and must not yank its clock.
        let rewind = !replaying
            && if auditioning {
                std::mem::take(&mut self.next_rewind)
            } else {
                std::mem::take(&mut self.next_rewind) || flagged
            };
        let launch = std::mem::replace(&mut self.next_launch, Launch::Now);
        // Armed only when there is a line to wait for: stopped, the engine
        // plays a quantised request at once, and an arming here would be a
        // countdown to nothing.
        if let Launch::Quantised { unit_cycles } = launch
            && self.is_playing()
            && !self.is_stopping()
        {
            self.armed_scene = Some(scene);
            self.armed_request = Some(request_id);
            self.armed_revision = Some(revision_hash.clone());
            self.landing_name = self.scenes.get(scene).map(|scene| scene.name().to_owned());
            self.log.push(
                LogLevel::Info,
                "launch",
                format!("armed for the next line of {unit_cycles} cycles"),
            );
        }
        if let Some(pending) = &self.pending_evaluation {
            self.finish_evaluation_request(pending.request_id, None);
        }
        self.pending_evaluation = Some(PendingEvaluation {
            request_id,
            scene,
            editor_revision: revision,
            source,
            launch,
            rewind,
            record,
        });
        self.status = "evaluating on the native engine…".into();
        self.flush_pending_evaluation();
    }

    /// Bind an evaluated text's sliders to its editor revision.
    fn bind_live_sliders(&mut self, scene: SceneId, revision: Revision, source: &str) {
        // The sliders of the score being evaluated become its controls:
        // from here on the engine knows them, so a drag has something to
        // move. Their spans follow the text from this revision forward.
        // Not while a snippet is playing under the score: the sliders of
        // the two texts together would put spans where the editor has no
        // text for them.
        #[cfg(feature = "hydra")]
        let plain = self.snippet_preview.is_none();
        #[cfg(not(feature = "hydra"))]
        let plain = true;
        if !self.options.mini && plain {
            self.live_sliders.insert(
                scene,
                LiveSliderSet {
                    revision,
                    sliders: rustel_runtime::ui_events::literal_sliders(source),
                },
            );
        }
    }

    /// Identical text can have a new revision after a replacement. Keep the
    /// installed evaluation, but bind its controls to that text again.
    pub(super) fn rebind_unchanged_score(
        &mut self,
        scene: SceneId,
        revision: Revision,
        source: Arc<str>,
    ) {
        let hash = source_revision(&source);
        // An identical-source launch can already be queued for another
        // scene or replay. Its next layout must retain that ownership.
        if self
            .evaluation_revisions
            .get(&hash)
            .is_none_or(|entry| entry.scene == scene && entry.replay_block.is_none())
        {
            self.evaluation_revisions
                .remember(hash, revision, Arc::clone(&source), scene, None);
        }
        self.bind_live_sliders(scene, revision, &source);
        if self.audible_scene == Some(scene)
            && self.visual_replay_block.is_none()
            && self.evaluated_source.as_deref() == Some(source.as_ref())
        {
            self.visual_revision = Some(revision);
            self.pin_visual_revision();
            self.install_virtual_rows(revision);
            self.rebuild_slider_spans();
            self.refresh_decorations();
        }
        self.invalidate_maps();
    }

    pub(super) fn flush_pending_evaluation(&mut self) {
        if !self.engine_connected {
            self.finish_evaluation_feedback(false);
            self.pending_evaluation = None;
            return;
        }
        // A score never overtakes a setup it may depend on. The engine
        // takes commands in order, so only what is still waiting here can
        // get ahead: hold it one turn.
        if !self.prebake_queue.is_empty() {
            return;
        }
        let Some(pending) = self.pending_evaluation.take() else {
            return;
        };
        // A snippet previewed under the set is removed when the score is
        // restored. Its install must be marked as a preview in the engine.
        #[cfg(feature = "hydra")]
        let preview = self
            .snippet_preview
            .as_ref()
            .is_some_and(|preview| preview.source == pending.source);
        #[cfg(not(feature = "hydra"))]
        let preview = false;
        let send = if preview {
            StudioWorker::try_evaluate_snippet_preview
        } else {
            StudioWorker::try_evaluate
        };
        match send(
            &self.worker,
            pending.request_id,
            pending.editor_revision.0,
            Arc::clone(&pending.source),
            self.options.mini,
            pending.launch,
            pending.rewind,
        ) {
            Ok(()) => {
                self.inflight.insert(pending.request_id, pending);
            }
            Err(EvaluationSendError::Full(source)) => {
                self.pending_evaluation = Some(PendingEvaluation { source, ..pending });
            }
            Err(EvaluationSendError::Disconnected(_)) => {
                self.mark_engine_disconnected();
            }
        }
    }

    pub(super) fn drain_engine(&mut self) {
        for _ in 0..MAX_EVENTS_PER_TURN {
            match self.worker.try_recv_control() {
                Ok(event) => self.handle_control(event),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    self.mark_engine_disconnected();
                    break;
                }
            }
        }
        for _ in 0..MAX_EVENTS_PER_TURN {
            match self.worker.try_recv_trace() {
                Ok(request) => match self.visual.install_trace_request(request) {
                    Ok(installed) => self.dirty_frame |= installed,
                    Err(error) => self.set_error(ErrorOwner::Interface, error.to_string()),
                },
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => break,
            }
        }
        if let Some((metadata, analysis)) = self.worker.take_latest_audio() {
            self.dirty_frame |= self.visual.install_audio(metadata, analysis);
        }
    }

    pub(super) fn handle_control(&mut self, event: StudioControlEvent) {
        let input_channels_before = self.lint_input_channels();
        match event {
            StudioControlEvent::Evaluation(outcome) => self.handle_evaluation(outcome),
            StudioControlEvent::Prebake(outcome) => self.handle_prebake(outcome),
            StudioControlEvent::Recording(outcome) => self.handle_recording(outcome),
            StudioControlEvent::SampleRecording(outcome) => self.handle_sample_recording(outcome),
            StudioControlEvent::Layout(layout) => {
                let hash = layout.ui_layout.source_revision.clone();
                // What the engine has taken as the running score: a layout
                // arrives once the source is evaluated and observed, which can
                // run ahead of the device cutover by a prefill. Everything
                // below follows the layout, not the sound - the tab marked as
                // playing and the inline widget rows move to the evaluated
                // scene while the previous one is still audible. The engine
                // also installs `silence` for a preview or a take with no
                // evaluation at all, which is why this is not recorded at
                // evaluation time.
                self.installed_revision = Some(hash.clone());
                let evaluated = self.evaluation_revisions.get(&hash);
                self.visual.install_layout(layout);
                match evaluated {
                    Some(entry) => {
                        // Widgets leave a scene that stopped being the
                        // audible one, so switching back to it does not show
                        // rows for music that is no longer playing.
                        if self.audible_scene != Some(entry.scene)
                            && let Some(previous) =
                                self.audible_scene.and_then(|id| self.scenes.get_mut(id))
                        {
                            previous.editor.clear_virtual_rows();
                        }
                        self.visual_revision = entry.revision;
                        self.visual_replay_block = entry.replay_block;
                        self.evaluated_source = Some(entry.source);
                        self.audible_scene = Some(entry.scene);
                        self.pin_visual_revision();
                        if let Some(revision) = entry.revision {
                            self.install_virtual_rows(revision);
                        }
                    }
                    None => {
                        self.visual_revision = None;
                        self.visual_replay_block = None;
                        self.evaluated_source = None;
                        self.audible_scene = None;
                    }
                }
                self.rebuild_slider_spans();
                self.invalidate_maps();
                self.dirty_frame = true;
            }
            StudioControlEvent::Diagnostic(mut diagnostic) => {
                let alert = match diagnostic.alert.take() {
                    Some(DiagnosticAlert::Resolve(key)) => {
                        self.dirty_frame |= self.log.resolve_alert(&key) > 0;
                        return;
                    }
                    Some(DiagnosticAlert::Raise(key)) => Some(key),
                    None => None,
                };
                match (diagnostic.level, diagnostic.recoverable) {
                    // The sounds a load skipped, said once when it is over:
                    // the log keeps the line and the status line stays on
                    // the music. The header's loading line said the rest.
                    (StudioDiagnosticLevel::Warning, _)
                        if diagnostic.kind == rustel_runtime::SAMPLE_LOADING_DIAGNOSTIC =>
                    {
                        self.log.push_with(
                            LogLevel::Warn,
                            &diagnostic.kind,
                            diagnostic.message,
                            alert,
                        );
                    }
                    (StudioDiagnosticLevel::Error, true) => {
                        let source_error =
                            matches!(diagnostic.kind.as_str(), "evaluation" | "mini");
                        let owner = if source_error {
                            self.audible_text().or_else(|| {
                                // A stopped score has no installed identity. An
                                // unknown audible revision must not borrow editor text.
                                self.audible_scene.is_none().then(|| {
                                    let scene = self.scenes.current();
                                    (scene.id, scene.editor.revision())
                                })
                            })
                        } else {
                            None
                        };
                        let alert = alert.or_else(|| {
                            owner.and_then(|(scene, revision)| {
                                self.evaluation_alert(scene, revision)
                            })
                        });
                        // The log takes the message whole; the footer takes
                        // the part of it a player can act on.
                        self.log.push_with(
                            LogLevel::Error,
                            &diagnostic.kind,
                            diagnostic.message.clone(),
                            alert,
                        );
                        self.errors.set(
                            ErrorOwner::Evaluation,
                            player_facing_error(&diagnostic.message),
                        );
                        self.update_failure = owner;
                    }
                    (StudioDiagnosticLevel::Error, false) => {
                        // Not `set_error`: that logs the same text it shows,
                        // and here those are deliberately different - the
                        // whole message under the part that spoke, the
                        // player's half on the footer.
                        self.log.push_with(
                            LogLevel::Error,
                            &diagnostic.kind,
                            diagnostic.message.clone(),
                            alert,
                        );
                        self.errors
                            .set(ErrorOwner::Engine, player_facing_error(&diagnostic.message));
                    }
                    (StudioDiagnosticLevel::Warning, _) => {
                        self.log.push_with(
                            LogLevel::Warn,
                            &diagnostic.kind,
                            diagnostic.message.clone(),
                            alert,
                        );
                        if diagnostic.kind == "gamepad" {
                            self.toast(diagnostic.message.clone());
                        }
                        self.status = diagnostic.message;
                    }
                    (StudioDiagnosticLevel::Info, _) => {
                        self.log.push_with(
                            LogLevel::Info,
                            &diagnostic.kind,
                            diagnostic.message.clone(),
                            alert,
                        );
                        // A `.log()` line is one per sounding note: news for
                        // the log panel it lands in, but a footer that
                        // echoed each one would flicker through every hap.
                        if diagnostic.kind != "log" {
                            // A device coming or going is news wherever the
                            // eyes are, not only on the status line under
                            // everything.
                            if diagnostic.kind == "gamepad" || diagnostic.kind == "input" {
                                self.toast(diagnostic.message.clone());
                            }
                            self.status = diagnostic.message;
                        }
                    }
                    // The log keeps it; the footer stays on the music. The
                    // stream facts arrive beside an install, and a fact that
                    // took the status line replaced "playing from the top".
                    (StudioDiagnosticLevel::Note, _) => {
                        self.log.push_with(
                            LogLevel::Info,
                            &diagnostic.kind,
                            diagnostic.message,
                            alert,
                        );
                    }
                    (StudioDiagnosticLevel::Trace, _) => {
                        self.log.push_with(
                            LogLevel::Debug,
                            &diagnostic.kind,
                            diagnostic.message,
                            alert,
                        );
                    }
                }
                self.dirty_frame = true;
            }
            StudioControlEvent::Snapshot(snapshot) => {
                let now = Instant::now();
                self.dirty_frame |= if self.stop_requested {
                    self.audio_advisory.reset() | self.power_advisory.reset()
                } else {
                    self.audio_advisory.observe(&snapshot, now)
                        | self.power_advisory.observe(&snapshot, now)
                };
                // A score can finish draining while the keyboard keeps the
                // device open. There is no device-stop acknowledgement in
                // that case; the snapshot confirms the score has finished.
                if self.stop_requested && !snapshot.playing {
                    self.settle_stopped_transport();
                    self.status = "score stopped".into();
                }
                // Snapshots also arrive while the engine is stopped. Clear
                // an engine failure only after playback has resumed.
                if snapshot.playing && !snapshot.stopping {
                    self.clear_error(ErrorOwner::Engine);
                }
                self.input_meter.observe(
                    rustel_audio::MasterLevels {
                        peak: snapshot.input_peak,
                        lufs: f32::NEG_INFINITY,
                        clipped_blocks: 0,
                        // The input's own meter: the limiter is on the
                        // output, so nothing here is reduced.
                        reduction: 1.0,
                    },
                    Instant::now(),
                );
                self.dirty_frame |= self.visual.install_snapshot_clock(&snapshot);
                // A closed tap can still have an unfinished writer. Only its
                // structural final reply releases the capture identity.
                if self.snapshot.as_ref() != Some(&*snapshot) {
                    // The light is held from here, not read off the
                    // current peak at paint time: a live microphone
                    // crosses the threshold several times a second, and a
                    // chip lit from the instantaneous value strobed at the
                    // frame rate for as long as anyone sat near it.
                    if snapshot.input_peak > INPUT_LIGHT_PEAK {
                        self.input_lit_at = Some(Instant::now());
                    }
                    if let Some(device) = snapshot.device.as_ref() {
                        self.resting_output = Some(device.name().to_owned());
                    }
                    self.snapshot = Some(*snapshot);
                    self.dirty_frame = true;
                }
            }
            StudioControlEvent::Stopped(stop) => {
                self.settle_stopped_transport();
                self.status = if stop.acknowledged {
                    "audio stopped cleanly".into()
                } else {
                    "audio stopped; the device did not acknowledge before the deadline".into()
                };
                self.snapshot = None;
            }
            StudioControlEvent::EngineFailure(failure) => {
                if failure.playback_stopped {
                    self.audio_advisory.reset();
                    self.power_advisory.reset();
                    self.worker.discard_visual_updates();
                    self.visual.stop();
                    self.snapshot = None;
                }
                self.set_error(ErrorOwner::Engine, Self::failure_line(&failure));
            }
            StudioControlEvent::LinkRefused => {
                self.link_enabled = false;
                self.link_seen_playing = None;
                self.status = "link session not joined".into();
                self.dirty_frame = true;
            }
        }
        self.refresh_input_lint(input_channels_before);
    }

    pub(super) fn handle_evaluation(&mut self, outcome: EvaluationOutcome) {
        let sent = self.inflight.remove(&outcome.request_id);
        if sent.is_some() {
            self.finish_evaluation_request(outcome.request_id, outcome_success(&outcome.result));
        }
        // Only the armed request's own outcome ends the arming. A newer
        // launch makes the worker answer the OLD request with a cancel
        // after the new one is already armed here; that answer is about
        // the old one.
        if self.armed_request == Some(outcome.request_id) {
            self.forget_arming();
        }
        match outcome.result {
            Ok(install) if install.answered_repeat => {
                // A repeat launch press answered with the launch already
                // playing (or about to): nothing new was installed. The
                // save is still recorded - the request carried it - but the
                // log gets no second "installed" line, and the visuals keep
                // the look-ahead a restart would throw away.
                if let Some(sent) = &sent.as_ref().filter(|sent| sent.record) {
                    self.record_save(SaveStatus::Installed, &sent.source, None, None);
                }
                self.status = "already playing from the top".to_owned();
                if let Some(sent) = &sent {
                    self.resolve_evaluation_alert(sent.scene, sent.editor_revision);
                    self.retire_other_scene_failure(sent.scene);
                }
                // Successful installs clear unowned runtime footers. Their
                // log alerts retain their separate lifetime.
                if self.update_failure.is_none() {
                    self.clear_error(ErrorOwner::Evaluation);
                }
            }
            Ok(install) => {
                self.visual.start();
                if let Some(sent) = &sent.as_ref().filter(|sent| sent.record) {
                    self.record_save(SaveStatus::Installed, &sent.source, None, None);
                }
                // The status line is read mid-set, over the music, by someone
                // whose hands are busy. It says what happened, not how it was
                // arranged: "prefetched" and "cuts over atomically" describe a
                // guarantee the engine owes itself, and the log below keeps
                // that wording for anyone debugging.
                let stage = if install.pending_cutover {
                    "updated - swaps in on the next cycle"
                } else {
                    "playing from the top"
                };
                self.status = stage.to_owned();
                if let Some(clock) = self.snapshot.as_ref().map(|snapshot| &snapshot.clock) {
                    let source = clock.in_port.clone();
                    if let Some(source) = source {
                        self.status = format!("updated - following {source} for tempo");
                    }
                }
                self.log.push(
                    LogLevel::Info,
                    "update",
                    format!(
                        "generation {} installed - {}",
                        install.generation,
                        if install.pending_cutover {
                            "replacement prefetched; audio cuts over atomically"
                        } else {
                            "playback started at cycle zero"
                        }
                    ),
                );
                if let Some(sent) = &sent {
                    self.resolve_evaluation_alert(sent.scene, sent.editor_revision);
                    self.retire_other_scene_failure(sent.scene);
                }
                if self.update_failure.is_none() {
                    self.clear_error(ErrorOwner::Evaluation);
                }
            }
            Err(failure) => {
                if failure.kind == "cancelled" {
                    self.clear_error(ErrorOwner::Evaluation);
                    self.status = failure.message;
                } else {
                    let full = Self::failure_line(&failure);
                    if let Some(sent) = &sent.as_ref().filter(|sent| sent.record) {
                        self.record_save(SaveStatus::Rejected, &sent.source, Some(&full), None);
                    }
                    // The log keeps the complete route through replacement
                    // probing and callback dispatch. The footer keeps the
                    // actionable cause and source location.
                    let alert = sent
                        .as_ref()
                        .and_then(|sent| self.evaluation_alert(sent.scene, sent.editor_revision));
                    self.log.push_with(LogLevel::Error, "update", full, alert);
                    let footer = self
                        .note_untrusted_import(&failure)
                        .or_else(|| self.note_csound_refusal(&failure))
                        .unwrap_or_else(|| player_facing_error(&failure.message));
                    self.errors.set(ErrorOwner::Evaluation, footer);
                    self.update_failure =
                        sent.as_ref().map(|sent| (sent.scene, sent.editor_revision));
                }
            }
        }
        self.dirty_frame = true;
    }

    fn evaluation_alert(&mut self, scene: SceneId, revision: Revision) -> Option<String> {
        // Closed scenes cannot add entries. Prune their old entries to keep
        // ownership bounded by the open scenes, even across repeated closes.
        self.evaluation_alerts
            .retain(|scene, _| self.scenes.get(*scene).is_some());
        self.scenes.get(scene)?;
        self.evaluation_alerts
            .entry(scene)
            .and_modify(|refused| *refused = (*refused).max(revision))
            .or_insert(revision);
        Some(format!("evaluation:{scene:?}"))
    }

    pub(super) fn resolve_evaluation_alert(&mut self, scene: SceneId, revision: Revision) {
        if self
            .evaluation_alerts
            .get(&scene)
            .is_some_and(|refused| revision >= *refused)
        {
            self.evaluation_alerts.remove(&scene);
            self.dirty_frame |= self.log.resolve_alert(&format!("evaluation:{scene:?}")) > 0;
        }
        // A failure whose scene was closed has no later update to resolve it.
        if self.update_failure.is_some_and(|(about, refused)| {
            self.scenes.get(about).is_none() || (about == scene && revision >= refused)
        }) {
            self.update_failure = None;
            self.clear_error(ErrorOwner::Evaluation);
        }
    }

    /// An install from `scene` retires the footer failure of any other
    /// scene: the footer reports the latest update, and that one worked. The
    /// other scene's log alert stays open until its own text is accepted.
    fn retire_other_scene_failure(&mut self, scene: SceneId) {
        if self.update_failure.is_some_and(|(about, _)| about != scene) {
            self.update_failure = None;
            self.clear_error(ErrorOwner::Evaluation);
        }
    }

    /// The audible scene and the revision installed in the engine. The
    /// editor can already contain newer text when a callback failure arrives.
    pub(super) fn audible_text(&self) -> Option<(SceneId, Revision)> {
        let scene = self.audible_scene?;
        self.scenes.get(scene)?;
        Some((scene, self.visual_revision?))
    }

    pub(super) fn mark_engine_disconnected(&mut self) {
        self.finish_evaluation_feedback(false);
        let input_channels_before = self.lint_input_channels();
        if self.engine_connected {
            self.engine_connected = false;
            self.pending_evaluation = None;
            self.pending_slider = None;
            self.inflight.clear();
            self.prebake_queue.clear();
            self.prebake_inflight = None;
            self.snapshot = None;
            self.audio_advisory.reset();
            self.power_advisory.reset();
            self.visual.stop();
            self.set_error(
                ErrorOwner::Engine,
                "studio engine worker disconnected".into(),
            );
        }
        self.refresh_input_lint(input_channels_before);
    }

    /// An engine failure as one line: its kind, then what it says.
    ///
    /// Without the guard the line said `audio: audio: audio output ...
    /// failed`, because the error's own Display already opens with the
    /// kind the failure carries beside it.
    fn failure_line(failure: &super::super::EngineFailure) -> String {
        if failure
            .message
            .strip_prefix(&failure.kind)
            .is_some_and(|rest| rest.starts_with(": "))
        {
            return failure.message.clone();
        }
        format!("{}: {}", failure.kind, failure.message)
    }
}

/// One evaluation the studio sent: where its text came from.
#[derive(Clone)]
pub(super) struct EvaluatedRevision {
    /// None while a replay editor displays a different block.
    pub(super) revision: Option<Revision>,
    pub(super) source: Arc<str>,
    pub(super) scene: SceneId,
    pub(super) replay_block: Option<ReplayBlock>,
}

/// The editor revisions (and their text and scene) that were sent for
/// evaluation, keyed by source hash, so a layout coming back from the engine
/// can be placed in the coordinates it was written for.
#[derive(Default)]
pub(super) struct RevisionCache {
    pub(super) entries: HashMap<String, EvaluatedRevision>,
    pub(super) order: VecDeque<String>,
}

impl RevisionCache {
    pub(super) fn remember(
        &mut self,
        hash: String,
        revision: Revision,
        source: Arc<str>,
        scene: SceneId,
        replay_block: Option<ReplayBlock>,
    ) {
        if self.entries.contains_key(&hash) {
            self.order.retain(|candidate| candidate != &hash);
        }
        self.entries.insert(
            hash.clone(),
            EvaluatedRevision {
                revision: Some(revision),
                source,
                scene,
                replay_block,
            },
        );
        self.order.push_back(hash);
        while self.order.len() > MAX_EVALUATION_REVISIONS {
            if let Some(oldest) = self.order.pop_front() {
                self.entries.remove(&oldest);
            }
        }
    }

    pub(super) fn get(&self, hash: &str) -> Option<EvaluatedRevision> {
        self.entries.get(hash).cloned()
    }
}

/// Whether a visual's call is still in the text: its range survived the
/// edits since evaluation, still names the painter, and its line has not
/// been commented out. Anything else releases the rows it was given.
///
/// A range that was deleted outright cannot be followed back through an
/// undo - both of its ends collapsed onto one offset - so the evaluated
/// text is consulted as well: if the call is sitting on the line its start
/// maps to, it is back.
pub(super) fn visual_is_alive(
    editor: &Editor,
    revision: Revision,
    evaluated_source: Option<&str>,
    visual: &UiVisual,
) -> bool {
    let document = editor.document();
    let names_painter = |range: &std::ops::Range<usize>| {
        document
            .slice(ByteOffset(range.start)..ByteOffset(range.end))
            .is_ok_and(|text| text.contains(visual.kind.as_str()))
    };
    let followed = editor
        .map_range_since(revision, visual.from..visual.to)
        .filter(names_painter);
    let range = match followed {
        Some(range) => range,
        None => {
            // The trail cannot grow a range back after a deletion, so look
            // for the evaluated call text on the line its start maps to.
            let Some(call) = evaluated_source.and_then(|source| source.get(visual.from..visual.to))
            else {
                return false;
            };
            let Some(start) = editor.map_offset_since(revision, visual.from) else {
                return false;
            };
            let Ok(line) = document.line_of(ByteOffset(start)) else {
                return false;
            };
            let content = document.line_content_range(line);
            let Ok(text) = document.slice(content.clone()) else {
                return false;
            };
            let Some(found) = text.find(call) else {
                return false;
            };
            content.start.0 + found..content.start.0 + found + call.len()
        }
    };
    let Ok(line) = document.line_of(ByteOffset(range.start)) else {
        return false;
    };
    let line_start = document.line_start(line).0;
    let Ok(prefix) = document.slice(ByteOffset(line_start)..ByteOffset(range.start)) else {
        return false;
    };
    !prefix.trim_start().starts_with("//")
}

/// Rows each widget of a layout occupies, anchored after the line of its
/// call. Every painter is inline in the studio: `.pianoroll()` and
/// `._pianoroll()` alike draw beneath the line that asked for them.
pub(super) fn virtual_rows_for(
    layout: &UiLayout,
    anchor: impl Fn(usize) -> Option<usize>,
    alive: impl Fn(&UiVisual) -> bool,
) -> Result<Vec<VirtualRowSpec>, String> {
    layout
        .visuals
        .iter()
        // A plain `pianoroll()` is global: it paints on the stage behind
        // the score (see `paint_stage`), and takes no rows. Its `_`
        // twin is the inline one.
        .filter(|visual| visual.kind != "markcss" && visual.inline)
        .filter_map(|visual| {
            // A round widget needs room to be round; a time-axis widget does
            // not, and stealing twelve rows for a scope would push the score
            // off a laptop screen.
            let height = match visual.kind.as_str() {
                "pianoroll" | "punchcard" => 8,
                // On its side the roll needs the height it gave to time.
                "wordfall" => 12,
                "scope" | "tscope" | "spectrum" => 6,
                "spiral" => 12,
                "pitchwheel" => 10,
                other => return Some(Err(format!("unknown inline visual kind {other:?}"))),
            };
            if !alive(visual) {
                return None;
            }
            let at = anchor(visual.to)?;
            Some(Ok(VirtualRowSpec::new(
                Arc::<str>::from(visual.id.as_str()),
                ByteOffset(at),
                height,
            )))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    mod auto_evaluate {
        //! Tests for the auto evaluate path in `app/evaluation.rs`.

        use crate::app::tests::support::*;
        use crate::app::*;

        /// A studio playing one scene, with auto evaluate on or off. The
        /// scene keeps the score `app_over` opened, "$: s(\"bd\")".
        fn playing_app(directory: &std::path::Path, auto: bool) -> (App, SceneId) {
            let mut app = app_over(directory);
            app.ui_settings.auto_evaluate = auto;
            app.snapshot = Some(sounding(true, false));
            let scene = app.scenes.current().id;
            (app, scene)
        }

        /// The evaluations one auto apply queued, pending or in flight.
        fn queued_by(app: &App, scene: SceneId) -> Vec<PendingEvaluation> {
            app.pending_evaluation
                .iter()
                .chain(app.inflight.values())
                .filter(|pending| pending.scene == scene)
                .cloned()
                .collect()
        }

        fn clear_queued(app: &mut App) {
            app.pending_evaluation = None;
            app.inflight.clear();
        }

        /// Off is the default for a reason: nothing applies itself, and the
        /// press stays the only way an edit becomes sound.
        #[test]
        fn off_leaves_the_updates_to_the_press() {
            let directory = tempfile::tempdir().expect("temp dir");
            let (mut app, scene) = playing_app(directory.path(), false);
            app.auto_evaluate();
            assert!(
                queued_by(&app, scene).is_empty(),
                "no evaluation left without a press"
            );
        }

        /// A stopped set is not started by typing. Update is how playback
        /// starts, and the studio does not make that decision alone.
        #[test]
        fn a_paused_edit_does_not_start_a_stopped_set() {
            let directory = tempfile::tempdir().expect("temp dir");
            let (mut app, scene) = plain_app(directory.path(), "$: s(\"bd\")\n");
            app.ui_settings.auto_evaluate = true;
            app.snapshot = None;
            app.auto_evaluate();
            assert!(
                queued_by(&app, scene).is_empty(),
                "a stopped set stays put until update starts it"
            );
        }

        /// A pause that ends just after Stop must not start the set again.
        #[test]
        fn a_paused_edit_does_not_restart_a_set_being_stopped() {
            let directory = tempfile::tempdir().expect("temp dir");
            let (mut app, scene) = playing_app(directory.path(), true);
            app.stop_requested = true;
            app.auto_evaluate();
            assert!(
                queued_by(&app, scene).is_empty(),
                "the stop holds while the snapshot still says playing"
            );
        }

        /// The apply itself: queued while the set plays, and no flash, because
        /// a flash confirms a press and nobody pressed anything. The launch
        /// row is off by default, so the edit lands at once.
        #[test]
        fn a_paused_edit_applies_itself_without_a_flash() {
            let directory = tempfile::tempdir().expect("temp dir");
            let (mut app, scene) = playing_app(directory.path(), true);
            app.auto_evaluate();
            let queued = queued_by(&app, scene);
            assert_eq!(queued.len(), 1, "the paused edit was queued");
            assert_eq!(&*queued[0].source, "$: s(\"bd\")\n");
            assert_eq!(queued[0].launch, Launch::Now);
            assert!(
                app.evaluation_feedback.is_none(),
                "an auto apply begins no flash"
            );
            clear_queued(&mut app);
        }

        /// The launch row owns the line an applied edit lands on, the same
        /// line an update waits for: off plays it at once, beat and cycle
        /// hold it for the line the row names.
        #[test]
        fn the_launch_row_names_the_line_its_edits_land_on() {
            for (quantise, launch) in [
                (settings::Quantise::Off, Launch::Now),
                (
                    settings::Quantise::Beat,
                    Launch::Quantised { unit_cycles: 0.25 },
                ),
                (
                    settings::Quantise::Cycle,
                    Launch::Quantised { unit_cycles: 1.0 },
                ),
            ] {
                let directory = tempfile::tempdir().expect("temp dir");
                let (mut app, scene) = playing_app(directory.path(), true);
                app.ui_settings.quantise = quantise;
                app.auto_evaluate();
                let queued = queued_by(&app, scene);
                assert_eq!(queued.len(), 1, "{quantise:?} queued its edit");
                assert_eq!(
                    queued[0].launch, launch,
                    "{quantise:?} lands where the launch row says"
                );
                clear_queued(&mut app);
            }
        }

        /// Text the checker refuses is not applied: the last good score keeps
        /// playing, and the footer says why, as a refused update does.
        #[test]
        fn refused_text_keeps_the_last_good_score() {
            let directory = tempfile::tempdir().expect("temp dir");
            let (mut app, scene) = playing_app(directory.path(), true);
            app.editor_mut()
                .dispatch(
                    Command::PasteText("$: s(\"bd\"".into()),
                    HistoryMoment(1),
                    &mut MemoryClipboard::default(),
                )
                .expect("break the score");
            app.auto_evaluate();
            assert!(
                queued_by(&app, scene).is_empty(),
                "an unclean score is not sent to the engine"
            );
            assert!(
                app.status.contains("refused"),
                "the refusal is said: {}",
                app.status
            );
        }

        /// The typing pause gates the apply: a pump inside the pause holds it,
        /// and the pump after it applies. The flag is spent either way, so a
        /// pause that elapsed while the set was stopped never fires when
        /// playback later starts.
        #[test]
        fn the_typing_pause_gates_the_apply() {
            let directory = tempfile::tempdir().expect("temp dir");
            let (mut app, scene) = playing_app(directory.path(), true);
            app.dispatch_editor(Command::PasteText("$: s(\"hh\")\n".into()))
                .expect("edit the score");
            assert!(app.auto_evaluate_due, "the edit armed the apply");
            app.pump_turn();
            assert!(
                queued_by(&app, scene).is_empty(),
                "inside the pause nothing applies"
            );
            assert!(
                app.auto_evaluate_due,
                "the apply waits inside the pause, it is not lost"
            );
            let elapsed =
                app.ui_settings.typing_pause.duration() + std::time::Duration::from_millis(50);
            app.last_edit_at = std::time::Instant::now()
                .checked_sub(elapsed)
                .expect("backdate the edit");
            // An idle worker still says stopped every 100 ms, and the drain
            // at a pump's head is read before the apply at its end: a word
            // landed between the two spends the pause on a set that reads
            // stopped. Put the playing picture back, owe the pause again,
            // and pump at once - nothing new arrives between two pumps run
            // back to back, so the next one runs clean through.
            let mut applied = false;
            for _ in 0..50 {
                app.snapshot = Some(sounding(true, false));
                app.pump_turn();
                if !app.auto_evaluate_due && !queued_by(&app, scene).is_empty() {
                    applied = true;
                    break;
                }
                app.auto_evaluate_due = true;
            }
            assert!(applied, "past the pause the edit applies");
            clear_queued(&mut app);

            // A pause that elapsed while stopped buys nothing later.
            app.auto_evaluate_due = true;
            app.snapshot = None;
            app.last_edit_at = std::time::Instant::now()
                .checked_sub(elapsed)
                .expect("backdate the edit");
            app.pump_turn();
            assert!(
                !app.auto_evaluate_due,
                "the elapsed pause is spent stopped, not saved up"
            );
        }
    }

    mod evaluation {
        //! Tests for `app/evaluation.rs`.

        use crate::app::evaluation::*;
        use crate::app::tests::support::*;
        use crate::editor::{MemoryClipboard, Selection};

        #[test]
        fn only_the_underscored_painters_take_rows_the_plain_ones_are_the_stage() {
            let layout =
                rustel_runtime::ui_events::visual_layout("$: note(60)._pianoroll().scope()", 4)
                    .unwrap()
                    .ui_layout;
            let rows = virtual_rows_for(&layout, Some, |_| true).unwrap();
            assert_eq!(
                rows.len(),
                1,
                "the plain scope() paints on the stage, not in rows"
            );
            assert_eq!(rows[0].height, 8);
            let layout =
                rustel_runtime::ui_events::visual_layout("$: note(60)._pianoroll()._scope()", 4)
                    .unwrap()
                    .ui_layout;
            let rows = virtual_rows_for(&layout, Some, |_| true).unwrap();
            assert_eq!(rows.len(), 2);
            assert_eq!(rows[1].height, 6);
        }

        #[test]
        fn a_deleted_or_commented_call_releases_its_rows_and_undo_restores_them() {
            let source = "$: s(\"bd\")._pianoroll()\n$: s(\"hh\")._scope()\n";
            let mut editor = Editor::new(source).unwrap();
            let revision = editor.revision();
            let layout = rustel_runtime::ui_events::visual_layout(source, 1)
                .unwrap()
                .ui_layout;
            let alive = |editor: &Editor| {
                layout
                    .visuals
                    .iter()
                    .filter(|visual| visual_is_alive(editor, revision, Some(source), visual))
                    .map(|visual| visual.kind.clone())
                    .collect::<Vec<_>>()
            };
            assert_eq!(alive(&editor), ["pianoroll", "scope"]);

            // Delete `._pianoroll()` from the first line.
            let call = source.find("._pianoroll()").unwrap();
            editor
                .set_selection(Selection::range(
                    ByteOffset(call),
                    ByteOffset(call + "._pianoroll()".len()),
                ))
                .unwrap();
            editor
                .dispatch(
                    Command::DeleteForward,
                    HistoryMoment(0),
                    &mut MemoryClipboard::default(),
                )
                .unwrap();
            assert_eq!(alive(&editor), ["scope"]);

            // Comment the second line out.
            let second = editor.source().find("$: s(\"hh\")").unwrap();
            editor
                .set_selection(Selection::caret(ByteOffset(second)))
                .unwrap();
            editor
                .dispatch(
                    Command::ToggleComment,
                    HistoryMoment(1_000),
                    &mut MemoryClipboard::default(),
                )
                .unwrap();
            assert!(alive(&editor).is_empty());

            editor.undo().unwrap();
            editor.undo().unwrap();
            assert_eq!(alive(&editor), ["pianoroll", "scope"]);
        }

        #[test]
        fn revision_cache_evicts_by_age_not_hash_sort_order() {
            let mut cache = RevisionCache::default();
            for index in 0..MAX_EVALUATION_REVISIONS {
                cache.remember(
                    format!("z{index:02}"),
                    Revision(index as u64),
                    Arc::from(""),
                    SceneId(1),
                    None,
                );
            }
            cache.remember(
                "a-newest".into(),
                Revision(99),
                Arc::from("newest"),
                SceneId(2),
                None,
            );

            let newest = cache.get("a-newest").unwrap();
            assert_eq!(newest.revision, Some(Revision(99)));
            assert_eq!(newest.scene, SceneId(2));
            assert!(cache.get("z00").is_none());
            assert_eq!(cache.order.len(), MAX_EVALUATION_REVISIONS);
        }

        /// The footer shows only the innermost clause and the tail of a nested
        /// engine refusal. The log keeps the whole message.
        #[test]
        fn a_refused_update_says_on_the_footer_only_what_a_player_can_act_on() {
            let engine = "the new score could not be played (scalar audio refused every onset of the \
             replacement's first window (unison 100 exceeds the native supersaw's 32-voice \
             ceiling)); the last audible score keeps playing";
            assert_eq!(
                player_facing_error(engine),
                "unison 100 exceeds the native supersaw's 32-voice ceiling - the last audible score \
             keeps playing"
            );

            // The orbit spelling of the same fault.
            let orbit = "the new score could not be played (scalar audio refused every onset of the \
             replacement's first window (orbit 19999999 is outside the supported 0..16 range)); \
             putting the last audible score back";
            assert_eq!(
                player_facing_error(orbit),
                "orbit 19999999 is outside the supported 0..16 range - putting the last audible \
             score back"
            );

            // One level of nesting: the bracket is itself the reason.
            assert_eq!(
                player_facing_error("the new score could not be played (the engine went away)"),
                "the engine went away"
            );

            // Everything else is left exactly as it is - a syntax error with
            // brackets of its own must not be taken apart.
            for other in [
                "line 2: unexpected token )",
                "sound \"abcde\" is not in the library",
                "",
                "the new score could not be played",
            ] {
                assert_eq!(player_facing_error(other), other);
            }

            let callback = "javascript: replacement query failed; last-good score kept (pattern \
            callback 14 failed: TypeError: not a function - line 28, column 90)";
            assert_eq!(
                player_facing_error(callback),
                "not a function - line 28, column 90"
            );
        }

        /// A repeat launch press answered with the launch already playing is
        /// not a second install: one "installed" line in the log, a status that
        /// says the score is already playing, and the visuals left as they are.
        #[test]
        fn an_answered_repeat_press_is_not_announced_as_a_second_install() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = app_over(directory.path());
            let scene = app.scenes.current().id;
            for request_id in [1, 2] {
                app.inflight.insert(
                    request_id,
                    PendingEvaluation {
                        request_id,
                        scene,
                        editor_revision: app.scenes.current().editor.revision(),
                        source: Arc::from("$: s(\"bd\")"),
                        launch: Launch::Quantised { unit_cycles: 1.0 },
                        rewind: true,
                        record: true,
                    },
                );
            }
            let install = |answered_repeat| crate::StudioInstall {
                generation: 7,
                source_revision: "r".into(),
                pending_cutover: true,
                answered_repeat,
                held: false,
            };
            let installed_lines = |app: &App| {
                app.log
                    .lines()
                    .iter()
                    .filter(|line| line.text.contains("generation 7 installed"))
                    .count()
            };

            app.handle_evaluation(EvaluationOutcome {
                request_id: 1,
                editor_revision: 0,
                result: Ok(install(false)),
            });
            assert_eq!(installed_lines(&app), 1);
            assert!(app.visual.running());
            app.visual.stop();

            app.handle_evaluation(EvaluationOutcome {
                request_id: 2,
                editor_revision: 0,
                result: Ok(install(true)),
            });
            assert_eq!(installed_lines(&app), 1, "no second install line");
            assert_eq!(app.status, "already playing from the top");
            assert!(
                !app.visual.running(),
                "the answer does not restart the visuals"
            );
            assert!(!app.is_evaluating(), "both requests were answered");
        }

        /// A live replacement can report several layers of engine routing around
        /// one JavaScript mistake. The footer names the mistake and its location;
        /// Studio Log retains the complete diagnostic for debugging.
        #[test]
        fn a_callback_failure_keeps_engine_routing_out_of_the_footer() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = app_over(directory.path());
            let message = "javascript: replacement query failed; last-good score kept (pattern \
            callback 14 failed: TypeError: not a function - line 28, column 90)";

            app.handle_evaluation(EvaluationOutcome {
                request_id: 17,
                editor_revision: 0,
                result: Err(crate::EngineFailure {
                    kind: "evaluation".into(),
                    message: message.into(),
                    recoverable: true,
                    playback_stopped: false,
                }),
            });

            assert_eq!(
                app.errors.visible(),
                Some("not a function - line 28, column 90")
            );
            assert_eq!(
                app.log.lines().back().map(|line| line.text.as_str()),
                Some(
                    "evaluation: javascript: replacement query failed; last-good score kept (pattern \
                 callback 14 failed: TypeError: not a function - line 28, column 90)"
                )
            );
        }

        /// A refusal arrives late for an origin on the saved list. The Trust
        /// page keeps one row for this origin.
        #[test]
        fn a_stale_import_refusal_adds_no_second_row_for_a_saved_origin() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = app_over(directory.path());
            let origin = "https://scripts.invalid";
            app.prefs.import_origins.push(origin.to_owned());

            app.handle_evaluation(EvaluationOutcome {
                request_id: 17,
                editor_revision: 0,
                result: Err(crate::EngineFailure {
                    kind: "untrusted-import".into(),
                    message: format!("{origin} is not trusted for import()"),
                    recoverable: true,
                    playback_stopped: false,
                }),
            });
            assert!(app.blocked_imports.is_empty());

            // Enter on a refused row of the same origin adds nothing.
            app.blocked_imports.push(origin.to_owned());
            app.permit_row(1);
            assert_eq!(app.prefs.import_origins, [origin]);
            assert!(app.blocked_imports.is_empty());
        }

        #[test]
        fn an_engine_failure_alert_clears_when_playback_resumes() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = app_over(directory.path());
            let unseen_before = app.log.unseen();
            let failure = "audio output failed while playing live";

            app.handle_control(StudioControlEvent::EngineFailure(crate::EngineFailure {
                kind: "audio".into(),
                message: failure.into(),
                recoverable: true,
                playback_stopped: true,
            }));
            assert!(app.errors.get(ErrorOwner::Engine).is_some());
            assert_eq!(app.log.unseen(), unseen_before + 1);

            app.handle_control(StudioControlEvent::Snapshot(Box::new(sounding(
                false, false,
            ))));
            assert!(app.errors.get(ErrorOwner::Engine).is_some());
            assert_eq!(app.log.unseen(), unseen_before + 1);

            app.handle_control(StudioControlEvent::Snapshot(Box::new(sounding(
                true, false,
            ))));
            assert!(app.errors.get(ErrorOwner::Engine).is_none());
            assert_eq!(app.log.unseen(), unseen_before, "the header alert clears");
            assert!(
                app.log
                    .lines()
                    .iter()
                    .any(|line| line.text.contains(failure) && line.resolved),
                "the failure remains in the log as resolved history"
            );

            app.handle_control(StudioControlEvent::EngineFailure(crate::EngineFailure {
                kind: "audio".into(),
                message: failure.into(),
                recoverable: true,
                playback_stopped: true,
            }));
            assert!(app.errors.get(ErrorOwner::Engine).is_some());
            assert_eq!(app.log.unseen(), unseen_before + 1);
            app.log.push(LogLevel::Warn, "other", "unrelated warning");
            app.handle_control(StudioControlEvent::Snapshot(Box::new(sounding(
                true, false,
            ))));
            assert_eq!(
                app.log.unseen(),
                unseen_before + 1,
                "an unrelated warning keeps its header alert"
            );
        }

        #[test]
        fn unowned_engine_failures_do_not_borrow_an_editors_position() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = app_over(directory.path());
            app.audible_scene = Some(app.scenes.current().id);
            app.visual_revision = None;
            app.handle_control(StudioControlEvent::Diagnostic(
                crate::engine::StudioDiagnostic {
                    kind: "evaluation".into(),
                    message: "callback failed - line 2".into(),
                    recoverable: true,
                    level: StudioDiagnosticLevel::Error,
                    alert: None,
                },
            ));
            assert_eq!(app.update_failure, None);
            assert!(app.errors.get(ErrorOwner::Evaluation).is_some());

            app.visual_revision = Some(app.editor().revision());
            app.handle_evaluation(EvaluationOutcome {
                request_id: u64::MAX,
                editor_revision: app.editor().revision().0,
                result: Err(crate::EngineFailure {
                    kind: "evaluation".into(),
                    message: "refused replacement - line 3".into(),
                    recoverable: true,
                    playback_stopped: false,
                }),
            });
            assert_eq!(
                app.update_failure, None,
                "an unknown request does not own the audible score's source position"
            );
            assert!(app.errors.get(ErrorOwner::Evaluation).is_some());
        }

        /// A score waits behind a setup it may depend on.
        #[test]
        fn a_score_never_overtakes_a_queued_setup() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = app_over(directory.path());
            app.prebake_inflight = Some(PrebakeRequest {
                request_id: 1,
                scope: PrebakeScope::Global,
                source: Arc::from("globalThis.g = 1"),
                reinstall_after: false,
            });
            app.prebake_queue.push_back(PrebakeRequest {
                request_id: 2,
                scope: PrebakeScope::Local,
                source: Arc::from("globalThis.l = 1"),
                reinstall_after: true,
            });

            app.queue_evaluation(app.editor().revision(), Arc::from("$: s(\"bd\")"));
            assert!(
                app.pending_evaluation.is_some() && app.inflight.is_empty(),
                "a score was sent ahead of a setup"
            );
        }

        /// A `.log()` line is a note that sounded: the log panel takes it at
        /// Info - where the quiet floor keeps it - and the footer's status is
        /// left alone, one hap per line.
        #[test]
        fn a_log_line_lands_in_the_panel_without_touching_the_status() {
            let directory = tempfile::tempdir().expect("tempdir");
            let mut app = app_over(directory.path());
            app.status = "ready".to_owned();
            for message in [
                "[hap] 0/1 → 1/4: note:c s:sine",
                "[hap] 1/4 → 1/2: note:c s:sine",
            ] {
                app.handle_control(StudioControlEvent::Diagnostic(
                    crate::engine::StudioDiagnostic {
                        kind: "log".into(),
                        message: message.into(),
                        recoverable: true,
                        level: StudioDiagnosticLevel::Info,
                        alert: None,
                    },
                ));
            }
            let logged = app
                .log
                .lines()
                .iter()
                .filter(|line| line.kind.as_str() == "log")
                .collect::<Vec<_>>();
            assert_eq!(logged.len(), 2, "both lines are in the panel");
            assert!(
                logged
                    .iter()
                    .all(|line| line.level == LogLevel::Info && line.text.contains("[hap] "))
            );
            assert_eq!(app.status, "ready", "the status line keeps its own words");
        }

        #[test]
        fn a_recoverable_runtime_refusal_is_still_a_red_footer_error() {
            let directory = tempfile::tempdir().expect("tempdir");
            let mut app = app_over(directory.path());
            app.handle_control(StudioControlEvent::Diagnostic(
                crate::engine::StudioDiagnostic {
                    kind: "resource-limit".into(),
                    message: "query deadline exceeded".into(),
                    recoverable: true,
                    level: StudioDiagnosticLevel::Error,
                    alert: None,
                },
            ));

            assert_eq!(app.errors.visible(), Some("query deadline exceeded"));
            assert_eq!(
                app.log.lines().back().map(|line| line.level),
                Some(LogLevel::Error)
            );
        }

        /// The stream facts ("coreaudio · … · 128 frames · 2.7 ms") are queued
        /// beside an install and used to arrive as a warning, which took the
        /// status line: two studio-e2e suites saw "silent · silent · 48000 Hz
        /// · 256 frames · 5.3 ms" where "playing from the top" belonged. A
        /// note is for the log alone.
        #[test]
        fn a_note_diagnostic_is_logged_and_leaves_the_status_line_alone() {
            let directory = tempfile::tempdir().expect("tempdir");
            let mut app = app_over(directory.path());
            app.status = "playing from the top".into();
            app.handle_control(StudioControlEvent::Diagnostic(
                crate::engine::StudioDiagnostic {
                    kind: "audio".into(),
                    message: "silent · silent · 48000 Hz · 256 frames · 5.3 ms".into(),
                    recoverable: true,
                    level: StudioDiagnosticLevel::Note,
                    alert: None,
                },
            ));

            assert_eq!(app.status, "playing from the top");
            assert_eq!(app.errors.visible(), None);
            let last = app.log.lines().back().expect("the note is logged");
            assert_eq!(last.level, LogLevel::Info);
            assert!(
                last.text.contains("256 frames"),
                "the log keeps the facts: {}",
                last.text
            );
        }

        fn pending_alert_update(app: &mut App, request_id: u64) -> (SceneId, Revision) {
            let scene = app.scenes.current();
            let owner = (scene.id, scene.editor.revision());
            app.inflight.insert(
                request_id,
                PendingEvaluation {
                    request_id,
                    scene: owner.0,
                    editor_revision: owner.1,
                    source: Arc::from(scene.editor.source()),
                    launch: Launch::Now,
                    rewind: false,
                    record: false,
                },
            );
            owner
        }

        fn refuse_alert_update(app: &mut App, request_id: u64) -> (SceneId, Revision) {
            let owner = pending_alert_update(app, request_id);
            app.handle_evaluation(EvaluationOutcome {
                request_id,
                editor_revision: owner.1.0,
                result: Err(crate::EngineFailure {
                    kind: "evaluation".into(),
                    message: format!("refused request {request_id}"),
                    recoverable: true,
                    playback_stopped: false,
                }),
            });
            owner
        }

        fn alert_install(answered_repeat: bool) -> crate::StudioInstall {
            crate::StudioInstall {
                generation: 7,
                source_revision: "r".into(),
                pending_cutover: true,
                answered_repeat,
                held: false,
            }
        }

        #[test]
        fn clean_lint_resolves_its_scene_even_when_another_scene_owns_the_footer() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = app_over(directory.path());
            let unseen_before = app.log.unseen();
            let first = refuse_alert_update(&mut app, 1);
            app.new_scene(false);
            let second = refuse_alert_update(&mut app, 2);
            assert_ne!(first.0, second.0);
            assert_eq!(app.log.unseen(), unseen_before + 2);

            assert!(app.select_scene(0));
            type_text(&mut app, " ");
            app.absorb_lint(crate::lint::LintResult {
                scene: first.0,
                revision: app.editor().revision(),
                input_channels: app.lint_input_channels(),
                diagnostics: Vec::new(),
            });
            assert_eq!(app.log.unseen(), unseen_before + 1);
            assert_eq!(app.update_failure, Some(second));
            assert_eq!(
                app.errors.get(ErrorOwner::Evaluation),
                Some("refused request 2")
            );
            assert!(
                app.log
                    .lines()
                    .iter()
                    .any(|line| line.text.contains("request 1") && line.resolved)
            );
            assert!(
                app.log
                    .lines()
                    .iter()
                    .any(|line| line.text.contains("request 2") && !line.resolved)
            );
        }

        #[test]
        fn an_install_clears_another_scenes_footer_and_preserves_unrelated_alerts() {
            for answered_repeat in [false, true] {
                let directory = tempfile::tempdir().expect("temp dir");
                let mut app = app_over(directory.path());
                app.log.push(LogLevel::Warn, "device", "unrelated warning");
                let unseen_before = app.log.unseen();
                let first = refuse_alert_update(&mut app, 1);
                pending_alert_update(&mut app, 3);
                app.new_scene(false);
                refuse_alert_update(&mut app, 2);
                assert_eq!(app.log.unseen(), unseen_before + 2);

                app.handle_evaluation(EvaluationOutcome {
                    request_id: 3,
                    editor_revision: first.1.0,
                    result: Ok(alert_install(answered_repeat)),
                });
                assert_eq!(app.log.unseen(), unseen_before + 1);
                // The footer follows the latest install. The other scene's
                // log alert stays open.
                assert_eq!(app.update_failure, None);
                assert_eq!(app.errors.get(ErrorOwner::Evaluation), None);
                assert!(
                    app.log
                        .lines()
                        .iter()
                        .any(|line| line.text.contains("request 1") && line.resolved)
                );
                assert!(
                    app.log
                        .lines()
                        .iter()
                        .any(|line| line.text.contains("request 2") && !line.resolved)
                );
                assert!(
                    app.log
                        .lines()
                        .iter()
                        .any(|line| line.kind == "device" && !line.resolved)
                );
            }
        }

        #[test]
        fn cancelled_unowned_and_older_updates_do_not_resolve_newer_alerts() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = app_over(directory.path());
            let unseen_before = app.log.unseen();
            let old = pending_alert_update(&mut app, 1);
            type_text(&mut app, " ");
            let refused = refuse_alert_update(&mut app, 2);
            assert!(refused.1 > old.1);
            pending_alert_update(&mut app, 3);

            app.handle_evaluation(EvaluationOutcome {
                request_id: 3,
                editor_revision: refused.1.0,
                result: Err(crate::EngineFailure {
                    kind: "cancelled".into(),
                    message: "cancelled".into(),
                    recoverable: true,
                    playback_stopped: false,
                }),
            });
            app.handle_evaluation(EvaluationOutcome {
                request_id: u64::MAX,
                editor_revision: refused.1.0,
                result: Ok(alert_install(false)),
            });
            app.handle_evaluation(EvaluationOutcome {
                request_id: 1,
                editor_revision: old.1.0,
                result: Ok(alert_install(false)),
            });
            assert_eq!(app.log.unseen(), unseen_before + 1);
            assert_eq!(app.update_failure, Some(refused));
            assert!(
                app.log
                    .lines()
                    .iter()
                    .any(|line| line.text.contains("request 2") && !line.resolved)
            );
        }

        #[test]
        fn an_install_clears_an_unowned_runtime_footer_without_retiring_its_alert() {
            for answered_repeat in [false, true] {
                let directory = tempfile::tempdir().expect("temp dir");
                let mut app = app_over(directory.path());
                let unseen_before = app.log.unseen();
                let refused = refuse_alert_update(&mut app, 1);
                app.handle_control(StudioControlEvent::Diagnostic(crate::StudioDiagnostic {
                    kind: "audio".into(),
                    message: "reverb capacity reached".into(),
                    recoverable: true,
                    level: StudioDiagnosticLevel::Error,
                    alert: None,
                }));
                assert_eq!(app.log.unseen(), unseen_before + 2);

                type_text(&mut app, " ");
                app.absorb_lint(crate::lint::LintResult {
                    scene: refused.0,
                    revision: app.editor().revision(),
                    input_channels: app.lint_input_channels(),
                    diagnostics: Vec::new(),
                });
                assert_eq!(app.log.unseen(), unseen_before + 1);
                assert_eq!(
                    app.errors.get(ErrorOwner::Evaluation),
                    Some("reverb capacity reached")
                );

                let accepted = pending_alert_update(&mut app, 2);
                app.handle_evaluation(EvaluationOutcome {
                    request_id: 2,
                    editor_revision: accepted.1.0,
                    result: Ok(alert_install(answered_repeat)),
                });
                assert!(app.errors.get(ErrorOwner::Evaluation).is_none());
                assert_eq!(app.log.unseen(), unseen_before + 1);
                assert!(app.log.lines().iter().any(|line| {
                    line.kind == "audio" && line.text == "reverb capacity reached" && !line.resolved
                }));
            }
        }
    }
}
