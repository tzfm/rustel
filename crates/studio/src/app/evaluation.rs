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
                self.dirty_frame |= if self.stop_requested {
                    self.audio_advisory.reset()
                } else {
                    self.audio_advisory.observe(&snapshot, Instant::now())
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
                    self.worker.discard_visual_updates();
                    self.visual.stop();
                    self.snapshot = None;
                }
                self.set_error(ErrorOwner::Engine, Self::failure_line(&failure));
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
                    self.errors.set(
                        ErrorOwner::Evaluation,
                        player_facing_error(&failure.message),
                    );
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
