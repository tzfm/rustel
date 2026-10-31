//! Checking the score while it is typed and following its sounds as they
//! load: lint requests sent after a typing pause, lint results folded in
//! (underline ranges, the first finding shown with its line:column, lint marks
//! kept off the line being typed), and what the checker knows (setup names,
//! input channels, MIDI ports). Also here are the sound readiness estimate, the
//! half-second library poll, the header's go state and the footer's status line
//! with its hints.

use super::*;

/// What the checker can judge beyond a score's own text. It grows after
/// launch, and a check made before it grew is asked again when it does.
#[derive(Clone, Debug, Default, PartialEq)]
pub(super) struct LintKnowledge {
    /// Sound and bank names can be judged: the library has its banks and
    /// no manifest is still on its way.
    sound_names: bool,
    /// Why the focused scene's imports failed, those that did: a finding
    /// names the reason, and goes when the import is answered.
    import_failures: Vec<String>,
    midi_inputs: Option<Vec<String>>,
    midi_outputs: Option<Vec<String>>,
}

/// How long typing has to pause before the text is checked. Short enough
/// that the finding is there by the time a hand reaches for update,
/// long enough that a burst of keys costs one check.
pub(super) const LINT_DEBOUNCE: Duration = Duration::from_millis(350);

/// How often a scene's sounds are re-checked while some are still loading.
pub(super) const READINESS_POLL: Duration = Duration::from_millis(500);

/// How long after a keystroke the sounds it named are asked for. Long
/// enough to coalesce a paste, far shorter than the lint: the download is
/// the slow part, and it starts on the bet that a name typed is a name
/// meant.
pub(super) const READINESS_SETTLE: Duration = Duration::from_millis(80);

/// How long after a keystroke the line under the caret keeps its marks to
/// itself. A string being typed is unterminated and a name half typed is
/// unknown; painting either red under the fingers is noise. The header's
/// count never waits.
pub(super) const MARK_SETTLE: Duration = Duration::from_millis(1_000);

/// A source-based estimate of where literal sound references stand. The
/// scanner cannot establish readiness for every runtime-selected voice:
/// separate `n` controls, pitch-dependent zones and dynamic names can
/// select assets other than the literal's default index and MIDI 36.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(super) struct Readiness {
    pub(super) ready: usize,
    pub(super) loading: usize,
    pub(super) failed: usize,
    /// Names no bank answers to. The linter has already said so if it
    /// could; a score that registers its own samples is left alone.
    pub(super) unknown: usize,
}

pub(super) fn lint_has_problems(result: &LintResult) -> bool {
    result
        .diagnostics
        .iter()
        .any(|diagnostic| diagnostic.level != LintLevel::Note)
}

pub(super) fn lint_alert(scene: SceneId) -> String {
    format!("lint:{scene:?}")
}

impl Readiness {
    pub(super) fn total(&self) -> usize {
        self.ready + self.loading + self.failed + self.unknown
    }

    pub(super) fn go_state(&self) -> GoState {
        if self.failed > 0 {
            GoState::Failed {
                failed: self.failed,
            }
        } else if self.loading > 0 {
            GoState::Loading {
                ready: self.ready,
                total: self.total(),
            }
        } else {
            GoState::Ready
        }
    }

    #[cfg(feature = "hydra")]
    fn preview_go_state(&self) -> GoState {
        match self.go_state() {
            // The editor also has lint to explain unknown names. A preview
            // bypasses that editor verdict, so unknown is not a green check.
            GoState::Ready if self.unknown > 0 => GoState::Unchecked,
            state => state,
        }
    }

    /// Look every sound of `source` up, which starts fetching the ones the
    /// library has not got. Bank-prefixed and bare names are both tried;
    /// this scan cannot associate a bank or a separate `n`/pitch control
    /// with each voice, so its counts remain estimates.
    pub(super) fn of(source: &str, library: &rustel_runtime::samples::SampleLibrary) -> Self {
        let banks = rustel_runtime::sounds::banks_in_score(source);
        let mut readiness = Self::default();
        for sound in rustel_runtime::sounds::in_score(source) {
            let (name, index) = match sound.split_once(':') {
                Some((name, index)) => (name, index.parse::<f64>().unwrap_or(0.0)),
                None => (sound.as_str(), 0.0),
            };
            let mut candidates = banks
                .iter()
                .map(|bank| format!("{bank}_{name}"))
                .collect::<Vec<_>>();
            candidates.push(name.to_owned());
            let states = candidates
                .iter()
                .map(|candidate| library.readiness(candidate, index))
                .collect::<Vec<_>>();
            let best = if states.contains(&SoundReadiness::Ready) {
                SoundReadiness::Ready
            } else if states.contains(&SoundReadiness::Loading) {
                SoundReadiness::Loading
            } else if states.contains(&SoundReadiness::Failed) {
                SoundReadiness::Failed
            } else {
                SoundReadiness::Unknown
            };
            match best {
                SoundReadiness::Ready => readiness.ready += 1,
                SoundReadiness::Loading => readiness.loading += 1,
                SoundReadiness::Failed => readiness.failed += 1,
                SoundReadiness::Unknown => readiness.unknown += 1,
            }
        }
        // The score's imports count like its sounds: one on its way keeps
        // the header at loading, one refused says so. One the library has
        // not been asked for yet - the checker asks when it next runs - is
        // not counted either way.
        for spec in rustel_runtime::sounds::samples_specs(source)
            .into_iter()
            .flatten()
        {
            match library.samples_source_state(&spec) {
                Some(rustel_runtime::samples::SourceState::Ready) => readiness.ready += 1,
                Some(rustel_runtime::samples::SourceState::Loading) => readiness.loading += 1,
                Some(rustel_runtime::samples::SourceState::Failed(_)) => readiness.failed += 1,
                None => {}
            }
        }
        readiness
    }
}

impl App {
    /// Hand the focused scene's text to the linter, once typing has paused.
    ///
    /// Unless the syntax check is full the text is not checked here, but the
    /// pause still owes the rest of what it always did: the mixer's strips,
    /// the imports and the sounds follow the text as before.
    pub(super) fn submit_lint(&mut self) {
        self.lint_pending = false;
        let scene = self.scenes.current();
        // The mixer's strips follow the text on the same debounce as the
        // lint: an orbit typed is a strip, an orbit deleted is not.
        let orbits = orbits_in_source(&scene.editor.source());
        if orbits != self.orbits_in_code {
            self.orbits_in_code = orbits;
            self.dirty_frame = true;
        }
        if self.ui_settings.syntax_check != SyntaxCheck::Full {
            self.settle_unchecked_text();
            return;
        }
        let scene = self.scenes.current();
        let context = self.lint_context_for(scene);
        self.linter.submit(LintRequest {
            scene: scene.id,
            revision: scene.editor.revision(),
            source: Arc::from(scene.editor.source()),
            mini: self.options.mini && scene.is_score(),
            library: self.worker.library(),
            context,
        });
    }

    pub(super) fn drain_lint(&mut self) {
        while let Some(result) = self.linter.try_recv() {
            self.finish_error_jump(&result);
            // A check already under way when the mode left full still
            // answers; what it found is not wanted any more.
            if self.ui_settings.syntax_check == SyntaxCheck::Full {
                self.absorb_lint(result);
            }
        }
    }

    /// What a paused text owes the studio besides a check, for when the
    /// syntax check is not full: the imports it names are looked up and its
    /// sounds asked for, as a check's result landing would have done.
    ///
    /// A verdict an update's refusal left on the scene goes too. The refusal
    /// marks where the update was refused, which is worth seeing, but once
    /// the text has been edited nothing will check it again to say whether
    /// it is still true.
    fn settle_unchecked_text(&mut self) {
        let scene = self.scenes.current().id;
        if self.lint.remove(&scene).is_some() {
            self.dirty_frame = true;
        }
        self.look_up_imports(scene);
        self.refresh_readiness(scene);
    }

    /// The syntax check's mode, changed.
    ///
    /// Leaving full takes every finding on screen down at once - the
    /// underlines, the header's count, the footer's line and the tabs' marks -
    /// rather than leaving a last verdict over text nothing checks any
    /// more. Full checks the focused scene at the next turn instead of
    /// waiting for a keystroke; the other scenes are checked as they are
    /// visited, as at launch.
    pub(super) fn apply_syntax_check(&mut self) {
        if self.ui_settings.syntax_check == SyntaxCheck::Full {
            self.lint_pending = true;
            let now = Instant::now();
            self.last_edit_at = now.checked_sub(LINT_DEBOUNCE).unwrap_or(now);
        } else {
            self.lint.clear();
        }
        self.dirty_frame = true;
    }

    /// Fold one lint result in - split out so a test can hand one over
    /// directly, the way the drain does.
    pub(super) fn absorb_lint(&mut self, result: super::super::lint::LintResult) {
        if self.scenes.get(result.scene).is_none() {
            return;
        }
        if result.input_channels != self.lint_input_channels() {
            // An input change can overtake a check without editing the score.
            // Never install a verdict for the previous device's channel count.
            self.lint_pending |= result.scene == self.scenes.current().id;
            return;
        }
        let previous = self.lint.get(&result.scene);
        let input_finding_resolved = previous.is_some_and(|previous| {
            previous.revision == result.revision
                && previous.input_channels != result.input_channels
                && lint_has_problems(previous)
                && !lint_has_problems(&result)
                && self.scenes.current().id == result.scene
                && self.editor().revision() == result.revision
                && rejection_of(&self.editor().source(), &previous.diagnostics).is_some_and(
                    |reason| {
                        self.status
                            == format!("refused - {reason} · the last good score keeps playing")
                    },
                )
        });
        let changed = previous != Some(&result);
        let scene = result.scene;
        let clean_at = (!lint_has_problems(&result)).then_some(result.revision);
        let clean_current = clean_at.is_some_and(|revision| {
            self.scenes
                .get(scene)
                .is_some_and(|scene| scene.editor.revision() == revision)
        });
        let status_only_refusal = clean_at.is_some_and(|revision| {
            self.update_failure
                .is_some_and(|(about, refused)| about == scene && revision > refused)
                && self.errors.get(ErrorOwner::Evaluation).is_none()
        });
        let lint_status_retired = self
            .lint_status
            .as_ref()
            .is_some_and(|(about, revision, _)| *about == scene && result.revision > *revision);
        if lint_status_retired {
            if self
                .lint_status
                .as_ref()
                .is_some_and(|(_, _, message)| self.status == message.as_str())
            {
                self.status = "ready".into();
            }
            self.lint_status = None;
        }
        // The lint reads a slider's numbers to judge them; it does not
        // hand the score a control. A control appears when the score is
        // evaluated (see `queue_evaluation`), because until then there is
        // nothing playing for it to move: a chip drawn over a call the
        // engine has never seen would look live and do nothing.
        self.lint.insert(scene, result);
        // The imports the score names are fetched as the checker sees
        // them - the fetch an evaluation would make - so their banks are
        // in the browser to look at, and their names judged, before the
        // score is evaluated. One that cannot be read is marked where it
        // is written, once the checker looks again.
        self.look_up_imports(scene);
        // The footer's update message is about text the engine refused.
        // That text edited and found fine, the message is stale: it would
        // stay up, under a header saying ready, until the next update
        // said nothing of the kind. The same text checked again keeps it.
        if let Some(revision) = clean_at
            && clean_current
            && self
                .update_failure
                .is_some_and(|(about, refused)| about == scene && revision > refused)
        {
            self.update_failure = None;
            self.clear_error(ErrorOwner::Evaluation);
            if status_only_refusal {
                self.status = "ready".into();
            }
        }
        if let Some(revision) = clean_at
            && clean_current
            && self
                .evaluation_alerts
                .get(&scene)
                .is_some_and(|refused| revision > *refused)
        {
            self.resolve_evaluation_alert(scene, revision);
        }
        // Input lint can become valid without an edit. Retire only the
        // exact refusal it produced, leaving later/runtime messages alone.
        if input_finding_resolved {
            self.status = "input channels checked".into();
        }
        let resolved_alerts = if clean_current {
            self.log.resolve_alert(&lint_alert(scene))
        } else {
            0
        };
        // The sounds were asked for as they were typed; a fresh look here
        // keeps the header's word current with the checker's, clean or not
        // - a score with a problem still wants its samples in.
        self.refresh_readiness(scene);
        self.dirty_frame |= changed || resolved_alerts > 0;
    }

    /// Ask the engine for every `samples("…")` a scene names that the
    /// library has not been asked for yet.
    pub(super) fn look_up_imports(&self, scene_id: SceneId) {
        // Without a library handle yet, the engine's own library keeps the
        // ask from being repeated.
        self.ask_for_imports(scene_id, |library, spec| {
            library.is_none_or(|library| library.samples_source_state(spec).is_none())
        });
    }

    /// Ask the engine again for every `samples("…")` a scene names whose
    /// failure has rested, so a failure the network caused recovers by
    /// itself. Typing never asks: an update does, and the poll does in the
    /// `background`, where a failure in a row rests longer.
    pub(super) fn retry_failed_imports(&self, scene_id: SceneId, background: bool) {
        self.ask_for_imports(scene_id, |library, spec| {
            library.is_some_and(|library| library.samples_source_rested(spec, background))
        });
    }

    fn ask_for_imports(
        &self,
        scene_id: SceneId,
        due: impl Fn(Option<&rustel_runtime::samples::SampleLibrary>, &str) -> bool,
    ) {
        let Some(scene) = self.scenes.get(scene_id) else {
            return;
        };
        let library = self.worker.library();
        for spec in rustel_runtime::sounds::samples_specs(&scene.editor.source())
            .into_iter()
            .flatten()
        {
            if due(library.as_deref(), &spec) {
                self.worker.try_look_up_samples(&spec);
            }
        }
    }

    /// Ask the library where every sound a scene names stands - which also
    /// starts fetching the ones it has not got.
    pub(super) fn refresh_readiness(&mut self, scene_id: SceneId) {
        let Some(scene) = self.scenes.get(scene_id) else {
            return;
        };
        let Some(library) = self.worker.library() else {
            return;
        };
        let source = scene.editor.source();
        let readiness = Readiness::of(&source, &library);
        if self.ui_settings.precache_sources {
            self.cache_score_imports(&source, &library);
        }
        let changed = self.readiness.get(&scene_id) != Some(&readiness);
        self.readiness.insert(scene_id, readiness);
        self.dirty_frame |= changed;
    }

    /// The line being typed on gets its marks back once the typing pauses.
    pub(super) fn settle_marks(&mut self, now: Instant) {
        if self.marks_muted.is_some() && now.duration_since(self.last_edit_at) >= MARK_SETTLE {
            self.marks_muted = None;
            self.dirty_frame = true;
        }
    }

    /// The keystroke's bet: a name typed inside `s("…")` - the string still
    /// open, the score not yet checked - is a name meant, so the library is
    /// asked for it now and the download is under way while the line is
    /// still being finished. Wrong bets cost a small file.
    pub(super) fn settle_readiness(&mut self, now: Instant) {
        if let Some((scene, due)) = self.readiness_due
            && now >= due
        {
            self.readiness_due = None;
            self.refresh_readiness(scene);
        }
    }

    /// The half-second poll: everything that lands off this thread or on
    /// the loader's schedule is looked at here, in this order. Trims that
    /// finished; fader changes the worker has not taken yet, and an output
    /// latency still to apply; the sample
    /// sources' reports, the pre-cache they owe and the bank names the
    /// library gave an import; the cache walk and clear; a first-time
    /// preview's shape; whether manifests are still on the way; the focused
    /// scene's imports whose failure has rested, asked again; what the
    /// checker knows; the files still caching; and the Sources rows. Last,
    /// while the focused scene's sounds are still arriving it asks after
    /// them again; with video, every clip a painter names; and with Hydra,
    /// on every poll, it follows a sounding preview's sounds.
    pub(super) fn poll_readiness(&mut self) {
        self.readiness_polled_at = Instant::now();
        self.poll_trim_jobs();
        self.send_mixer_gains();
        self.poll_source_reports_precache_and_aliases();
        self.poll_cache_results();
        self.poll_preview_shape();
        let loading = self.poll_library_loading();
        self.retry_failed_imports(self.scenes.current().id, true);
        self.poll_lint_knowledge();
        self.poll_caching_samples();
        self.poll_source_rows(loading);
        let scene = self.scenes.current().id;
        if self
            .readiness
            .get(&scene)
            .is_some_and(|readiness| readiness.loading > 0)
        {
            self.refresh_readiness(scene);
        }
        #[cfg(feature = "video")]
        self.poll_video_thumbnails();
        #[cfg(feature = "hydra")]
        self.refresh_preview_readiness();
    }

    /// Sample whether the library still has manifests on the way, and keep
    /// it for the frames between polls to read; a change repaints. Returns
    /// it as sampled, for the source rows later in the poll.
    fn poll_library_loading(&mut self) -> bool {
        let loading = self
            .worker
            .library()
            .is_some_and(|library| library.manifests_pending() > 0);
        if loading != self.library_loading {
            self.library_loading = loading;
            self.dirty_frame = true;
        }
        loading
    }

    /// The check at launch runs before the checker knows enough: the
    /// sample library's manifests and the MIDI ports arrive after it, and
    /// until then it judges no sound, bank or port name. Nothing in the
    /// text changes when they arrive, so without this a misspelt sound in
    /// the score the studio opened on stayed unmarked until a keystroke.
    fn poll_lint_knowledge(&mut self) {
        let knowledge = self.lint_knowledge();
        if knowledge != self.lint_knowledge {
            self.lint_knowledge = knowledge;
            self.lint_pending = true;
        }
    }

    /// Files the sample loader still has in its line, sampled on the poll
    /// for the frame to read.
    fn poll_caching_samples(&mut self) {
        let caching = self
            .worker
            .library()
            .map_or(0, |library| library.pending_loads());
        if caching != self.caching_samples {
            self.caching_samples = caching;
            self.dirty_frame = true;
        }
    }

    /// Ask the library for every clip a video painter names and no strip
    /// is held for. The first ask starts a decode on a library thread;
    /// this poll collects it when it lands. A clip whose answer never
    /// comes is named once and then left alone.
    #[cfg(feature = "video")]
    fn poll_video_thumbnails(&mut self) {
        let Some(library) = self.worker.library() else {
            return;
        };
        let mut changed = false;
        for name in self.visual.video_names_to_ask() {
            use rustel_runtime::video::VideoPicture;
            match library.video_thumbnails(&name) {
                Some(VideoPicture::Ready(strip)) => {
                    changed |= self
                        .visual
                        .install_video(&name, super::super::visuals::VideoEntry::Ready(strip));
                }
                Some(VideoPicture::Failed(message)) => {
                    changed |= self
                        .visual
                        .install_video(&name, super::super::visuals::VideoEntry::Failed(message));
                }
                Some(VideoPicture::Decoding) | None => {
                    changed |= self.visual.note_video_ask(&name);
                }
            }
        }
        if changed {
            self.dirty_frame = true;
        }
    }

    /// The header follows an installed preview while it owns playback;
    /// otherwise it describes the focused scene's next update.
    pub(super) fn go_state(&self) -> GoState {
        #[cfg(feature = "hydra")]
        if let Some(preview) = self.snippet_preview.as_ref()
            && self.installed_revision.as_deref() == Some(preview.revision.as_str())
            && self.is_playing()
            && !self.is_stopping()
            && !self.stop_requested
        {
            return preview
                .readiness
                .as_ref()
                .map_or(GoState::Unchecked, Readiness::preview_go_state);
        }
        let scene = self.scenes.current().id;
        if let Some(result) = self.lint.get(&scene) {
            if result.input_channels != self.lint_input_channels() {
                return GoState::Unchecked;
            }
            let count = result
                .diagnostics
                .iter()
                .filter(|diagnostic| diagnostic.level != LintLevel::Note)
                .count();
            if count > 0 {
                return GoState::Problems { count };
            }
        }
        self.readiness
            .get(&scene)
            .map_or(GoState::Unchecked, Readiness::go_state)
    }

    /// The linter's findings for a scene, followed into its current text.
    /// A finding whose text was deleted is gone; one whose revision has aged
    /// out of the trail is dropped rather than drawn in the wrong place.
    pub(super) fn lint_ranges(&self, scene_id: SceneId) -> Vec<(usize, usize)> {
        let (Some(result), Some(scene)) = (self.lint.get(&scene_id), self.scenes.get(scene_id))
        else {
            return Vec::new();
        };
        if result.input_channels != self.lint_input_channels() {
            return Vec::new();
        }
        // Quiet while typing: the line under the caret keeps its marks to
        // itself until the typing pauses or the caret leaves it. The
        // header's count and message never wait.
        let muted = (self.marks_muted == Some(scene_id)
            && self.focus == Focus::Editor
            && scene_id == self.scenes.current().id)
            .then(|| {
                let document = scene.editor.document();
                let line = document
                    .line_of(scene.editor.primary_selection().head)
                    .ok()?;
                let range = document.line_content_range(line);
                Some((range.start.0, range.end.0))
            })
            .flatten();
        result
            .diagnostics
            .iter()
            .filter(|diagnostic| diagnostic.level != LintLevel::Note)
            .filter_map(|diagnostic| {
                scene
                    .editor
                    .map_range_since(result.revision, diagnostic.from..diagnostic.to)
                    .map(|range| (range.start, range.end))
            })
            // Ranges are half-open: one ending where the line starts is
            // the line above's.
            .filter(|(from, to)| muted.is_none_or(|(start, end)| *to <= start || *from > end))
            .collect()
    }

    /// The first finding in the focused scene, with its current location
    /// first so a narrow footer cannot ellipsize the useful part away.
    pub(super) fn lint_message(&self) -> Option<String> {
        let scene = self.scenes.current();
        let result = self.lint.get(&scene.id)?;
        if result.input_channels != self.lint_input_channels() {
            return None;
        }
        let diagnostic = result
            .diagnostics
            .iter()
            .find(|diagnostic| diagnostic.level != LintLevel::Note)?;
        let location = scene
            .editor
            .map_range_since(result.revision, diagnostic.from..diagnostic.to)
            .and_then(|range| {
                let document = scene.editor.document();
                let at = ByteOffset(range.start);
                let line = document.line_of(at).ok()?;
                let column = document
                    .slice(document.line_start(line)..at)
                    .map(|text| text.chars().count())
                    .unwrap_or(0);
                Some((line + 1, column + 1))
            })
            .map(|(line, column)| format!("{line}:{column} · "))
            .unwrap_or_default();
        Some(format!("{location}{}", diagnostic.message))
    }

    /// Notes describe the current source once, without becoming errors or
    /// per-onset log messages. They yield to the existing status and errors.
    pub(super) fn footer_status(&self) -> String {
        let mut status = self.status.clone();
        // A live slider is text until the caret touches its control. Say what
        // that extra state unlocks while it is true, and put the useful part
        // first so a narrow terminal clips old status rather than the key.
        if self.focus == Focus::Editor && self.slider_at_caret().is_some() {
            let hint = super::super::keybinds::shortcut_label("Alt+Up/Down changes slider");
            status = if status.is_empty() {
                hint.into_owned()
            } else {
                format!("{hint} · {status}")
            };
        }
        let scene = self.scenes.current();
        let hint = self
            .lint
            .get(&scene.id)
            .filter(|result| {
                result.revision == scene.editor.revision()
                    && result.input_channels == self.lint_input_channels()
                    && !lint_has_problems(result)
            })
            .and_then(|result| {
                result
                    .diagnostics
                    .iter()
                    .find(|diagnostic| diagnostic.level == LintLevel::Note)
            });
        if self.errors.visible().is_none()
            && self.focus == Focus::Editor
            && let Some(hint) = hint
        {
            if status.is_empty() {
                format!("hint: {}", hint.message)
            } else {
                format!("{status} · hint: {}", hint.message)
            }
        } else {
            status
        }
    }

    /// Zero means no verified input; keep the documented silent-input
    /// behavior while still checking the native channel ceiling in lint.
    pub(super) fn lint_input_channels(&self) -> Option<usize> {
        let channels = self
            .input_channels()
            .min(rustel_audio::input::MAX_INPUT_CHANNELS);
        (channels > 0).then_some(channels)
    }

    /// The ports the checker may judge a name against, or `None` when the
    /// probe has not run or found nothing.
    fn lint_midi_ports(&self, direction: MidiDirection) -> Option<Vec<String>> {
        let ports = self.midi_port_names(direction);
        (!ports.is_empty()).then_some(ports)
    }

    /// What the checker can judge beyond the score's own text, which grows
    /// after launch: sound and bank names once the sample library has its
    /// banks and every manifest is in (the rule `rustel_runtime::lint` judges them
    /// by), why the focused scene's imports failed, and MIDI port names once
    /// the ports have been probed.
    fn lint_knowledge(&self) -> LintKnowledge {
        let library = self.worker.library();
        let import_failures = library.as_ref().map_or_else(Vec::new, |library| {
            rustel_runtime::sounds::samples_specs(&self.scenes.current().editor.source())
                .into_iter()
                .flatten()
                .filter_map(|spec| match library.samples_source_state(&spec) {
                    Some(rustel_runtime::samples::SourceState::Failed(reason)) => Some(reason),
                    _ => None,
                })
                .collect()
        });
        LintKnowledge {
            sound_names: library
                .is_some_and(|library| library.manifests_pending() == 0 && library.has_banks()),
            import_failures,
            midi_inputs: self.lint_midi_ports(MidiDirection::In),
            midi_outputs: self.lint_midi_ports(MidiDirection::Out),
        }
    }

    pub(super) fn refresh_input_lint(&mut self, previous: Option<usize>) {
        if previous != self.lint_input_channels() {
            self.lint_pending = true;
            self.dirty_frame = true;
        }
    }

    /// What the checker may assume while reading this tab.
    ///
    /// A score sees both setups; a set's own setup sees the global one; the
    /// global one sees only itself. Each also sees what it defines itself,
    /// so calling a helper three lines above is not an unknown function.
    pub(super) fn lint_context_for(&self, scene: &super::super::scenes::Scene) -> Arc<LintContext> {
        let own = scene.editor.source();
        let mut context = match scene.prebake() {
            None => {
                let mut context = LintContext::default();
                for definitions in &self.applied_setup {
                    context
                        .setup_names
                        .extend(definitions.names.iter().cloned());
                    context.setup_has_dynamic_names |= definitions.has_dynamic_names;
                }
                context
            }
            Some(PrebakeScope::Global) => {
                LintContext::from_setup_sources([own.as_str()]).as_setup()
            }
            Some(PrebakeScope::Local) => {
                LintContext::from_setup_sources([self.global_prebake.as_str(), own.as_str()])
                    .as_setup()
            }
        };
        if scene.is_score() {
            // Its own registrations are already read from the score itself.
            context.mode = LintMode::Score;
        }
        context.input_channels = self.lint_input_channels();
        // What is plugged in right now, so a score naming a port that is
        // not there says so before it is evaluated in front of anyone.
        // Empty means unprobed, and an unprobed machine judges nothing.
        context.midi_inputs = self.lint_midi_ports(MidiDirection::In);
        context.midi_outputs = self.lint_midi_ports(MidiDirection::Out);
        Arc::new(context)
    }
}

#[cfg(test)]
mod tests {
    mod input_hint {
        //! Tests for input hint.

        use crate::app::tests::support::app_over;
        use crate::app::*;

        const SOURCE: &str = "$: s(\"in\").speed(2)";

        const HINT: &str = "speed does not change live input";

        fn prepare(app: &mut App) -> LintResult {
            app.worker
                .set_library(Arc::new(rustel_runtime::samples::SampleLibrary::empty()));
            app.scenes.current_mut().editor = Editor::new(SOURCE).unwrap();
            app.focus = Focus::Editor;
            app.status = "updated".into();
            LintResult {
                scene: app.scenes.current().id,
                revision: app.editor().revision(),
                input_channels: app.lint_input_channels(),
                diagnostics: vec![rustel_runtime::lint::Diagnostic {
                    level: LintLevel::Note,
                    message: HINT.into(),
                    from: SOURCE.find("speed").unwrap(),
                    to: SOURCE.find("speed").unwrap() + "speed".len(),
                }],
            }
        }

        #[test]
        fn input_notes_keep_ready_without_error_marks_or_refusing_updates() {
            let directory = tempfile::tempdir().unwrap();
            let mut app = app_over(directory.path());
            let result = prepare(&mut app);
            let scene = result.scene;
            app.absorb_lint(result);
            assert_eq!(app.go_state(), GoState::Ready);
            assert!(app.lint_ranges(scene).is_empty());
            assert!(app.lint_message().is_none());
            assert!(app.scene_chips().iter().all(|chip| !chip.errors));
            assert!(!app.refuse_unclean(app.editor().revision(), &Arc::from(SOURCE)));
            assert_eq!(app.footer_status(), format!("updated · hint: {HINT}"));
        }

        #[test]
        fn an_invalid_input_outranks_a_note_even_when_the_note_comes_first() {
            let directory = tempfile::tempdir().unwrap();
            let mut app = app_over(directory.path());
            let mut result = prepare(&mut app);
            let source = "$: s(\"in:2323\").speed(2)";
            app.scenes.current_mut().editor = Editor::new(source).unwrap();
            result.revision = app.editor().revision();
            let context = app.lint_context_for(app.scenes.current());
            let library = app.worker.library();
            result.diagnostics.extend(
                lint_with(source, false, library.as_deref(), &context)
                    .into_iter()
                    .filter(|diagnostic| diagnostic.level != LintLevel::Note),
            );
            app.absorb_lint(result);
            assert_eq!(app.go_state(), GoState::Problems { count: 1 });
            let start = source.find("in:2323").unwrap();
            assert_eq!(
                app.lint_ranges(app.scenes.current().id),
                [(start, start + 7)]
            );
            assert!(app.lint_message().unwrap().contains("2323"));
            assert_eq!(app.footer_status(), "updated");
            assert!(app.refuse_unclean(app.editor().revision(), &Arc::from(source)));
        }

        #[test]
        fn contextual_notes_do_not_repeat_toasts_or_logs_or_replace_owned_status() {
            let directory = tempfile::tempdir().unwrap();
            let mut app = app_over(directory.path());
            let result = prepare(&mut app);
            let lines = app.log.lines().len();
            let toast = app.toast.clone();
            for _ in 0..3 {
                app.absorb_lint(result.clone());
                assert_eq!(app.footer_status(), format!("updated · hint: {HINT}"));
            }
            assert_eq!(app.status, "updated");
            assert_eq!(app.log.lines().len(), lines);
            assert_eq!(app.toast, toast);

            app.status = "master -6 dB".into();
            assert!(app.footer_status().starts_with("master -6 dB · "));
            app.set_error(ErrorOwner::Engine, "audio device disconnected".into());
            assert_eq!(app.footer_status(), app.status);
            assert_eq!(app.errors.visible(), Some("audio device disconnected"));
            app.clear_error(ErrorOwner::Engine);

            app.dispatch_editor(Command::InsertText(" ".into()))
                .unwrap();
            assert_ne!(app.editor().revision(), result.revision);
            assert!(
                !app.footer_status().contains(HINT),
                "old source hints disappear"
            );
        }
    }

    mod input_lint {
        //! Tests for input lint.

        use crate::app::tests::support::app_over;
        use crate::app::*;

        fn input_snapshot(app: &mut App, channels: usize) {
            app.handle_control(StudioControlEvent::Snapshot(Box::new(StudioSnapshot {
                playing: true,
                input_device: (channels > 0).then(|| "test input".into()),
                input_channels: channels,
                ..Default::default()
            })));
        }

        fn checked(app: &App) -> LintResult {
            let scene = app.scenes.current();
            let context = app.lint_context_for(scene);
            let library = app.worker.library();
            LintResult {
                scene: scene.id,
                revision: scene.editor.revision(),
                input_channels: context.input_channels,
                diagnostics: lint_with(&scene.editor.source(), false, library.as_deref(), &context),
            }
        }

        #[test]
        fn invalid_input_update_is_refused_before_saving_or_replacing_the_performance() {
            for (channels, source) in [(0, "$: s(\"in:2323\")"), (2, "$: s(\"in:3\")")] {
                let directory = tempfile::tempdir().unwrap();
                let mut app = app_over(directory.path());
                app.worker
                    .set_library(Arc::new(rustel_runtime::samples::SampleLibrary::empty()));
                input_snapshot(&mut app, channels);
                let previous: Arc<str> = Arc::from("$: s(\"sine\")");
                let previous_hash = source_revision(&previous);
                app.installed_revision = Some(previous_hash.clone());
                app.evaluated_source = Some(Arc::clone(&previous));
                app.audible_scene = Some(app.scenes.current().id);
                app.scenes.current_mut().editor = Editor::new(source).unwrap();
                let path = app.scenes.current().path.clone();
                let stored = std::fs::read_to_string(&path).unwrap();
                let request_id = app.next_request_id;
                app.handle_editor_effect(EditorEffect::Evaluate {
                    revision: app.editor().revision(),
                    source: ropey::Rope::from_str(source),
                })
                .unwrap();
                assert!(app.status.contains("refused"), "{}", app.status);
                assert!(app.status.contains("input"), "{}", app.status);
                assert_eq!(app.go_state(), GoState::Problems { count: 1 });
                assert_eq!(
                    app.installed_revision.as_deref(),
                    Some(previous_hash.as_str())
                );
                assert_eq!(app.evaluated_source.as_deref(), Some(previous.as_ref()));
                assert!(app.is_playing());
                assert_eq!(app.next_request_id, request_id, "no evaluation was queued");
                assert_eq!(
                    std::fs::read_to_string(path).unwrap(),
                    stored,
                    "no bad save"
                );
            }
        }

        #[test]
        fn input_device_changes_invalidate_old_lint_without_an_editor_change() {
            let directory = tempfile::tempdir().unwrap();
            let mut app = app_over(directory.path());
            app.worker
                .set_library(Arc::new(rustel_runtime::samples::SampleLibrary::empty()));
            app.scenes.current_mut().editor = Editor::new("$: s(\"in:3\")").unwrap();
            let scene = app.scenes.current().id;
            let revision = app.editor().revision();
            input_snapshot(&mut app, 2);
            let stereo_result = checked(&app);
            assert_eq!(stereo_result.diagnostics.len(), 1);
            app.absorb_lint(stereo_result.clone());
            assert_eq!(app.go_state(), GoState::Problems { count: 1 });
            assert!(!app.lint_ranges(scene).is_empty());
            app.handle_editor_effect(EditorEffect::Evaluate {
                revision,
                source: ropey::Rope::from_str("$: s(\"in:3\")"),
            })
            .unwrap();
            assert!(app.status.contains("refused"), "{}", app.status);

            app.lint_pending = false;
            input_snapshot(&mut app, 4);
            assert!(app.lint_pending);
            assert_eq!(app.editor().revision(), revision);
            assert_eq!(app.go_state(), GoState::Unchecked);
            assert!(app.lint_ranges(scene).is_empty());
            assert!(app.lint_message().is_none());
            app.absorb_lint(stereo_result);
            assert_eq!(app.go_state(), GoState::Unchecked, "old result stays stale");
            let four_channel_result = checked(&app);
            assert!(four_channel_result.diagnostics.is_empty());
            app.absorb_lint(four_channel_result.clone());
            assert_eq!(app.go_state(), GoState::Ready);
            assert!(!app.status.contains("refused"), "{}", app.status);

            input_snapshot(&mut app, 2);
            app.absorb_lint(four_channel_result);
            assert_eq!(app.go_state(), GoState::Unchecked, "old ready is stale too");
            app.absorb_lint(checked(&app));
            assert_eq!(app.go_state(), GoState::Problems { count: 1 });

            // Losing the input restores the established silence-without-a-device
            // behavior; it cannot retain a count-specific error from the old device.
            input_snapshot(&mut app, 0);
            assert_eq!(app.lint_input_channels(), None);
            app.absorb_lint(checked(&app));
            assert_eq!(app.go_state(), GoState::Ready);
        }

        #[test]
        fn background_lint_receives_the_selected_inputs_channel_count() {
            let directory = tempfile::tempdir().unwrap();
            let mut app = app_over(directory.path());
            app.worker
                .set_library(Arc::new(rustel_runtime::samples::SampleLibrary::empty()));
            app.scenes.current_mut().editor = Editor::new("$: s(\"in:2\")").unwrap();
            input_snapshot(&mut app, 2);
            app.submit_lint();
            let deadline = Instant::now() + Duration::from_secs(5);
            let result = loop {
                if let Some(result) = app.linter.try_recv() {
                    break result;
                }
                assert!(Instant::now() < deadline, "lint worker did not answer");
                std::thread::sleep(Duration::from_millis(5));
            };
            assert_eq!(result.input_channels, Some(2));
            assert_eq!(result.diagnostics.len(), 1);
            assert_eq!(
                &app.editor().source()[result.diagnostics[0].from..result.diagnostics[0].to],
                "in:2"
            );
            app.absorb_lint(result);
            assert_eq!(app.go_state(), GoState::Problems { count: 1 });
        }

        #[test]
        fn background_lint_marks_stray_names_before_any_evaluation() {
            let directory = tempfile::tempdir().unwrap();
            let mut app = app_over(directory.path());
            app.worker
                .set_library(Arc::new(rustel_runtime::samples::SampleLibrary::empty()));
            app.scenes.current_mut().editor = Editor::new("$: s('sine')\n\n kaskaskjas").unwrap();
            let request_id = app.next_request_id;
            app.submit_lint();
            let deadline = Instant::now() + Duration::from_secs(5);
            let result = loop {
                if let Some(result) = app.linter.try_recv() {
                    break result;
                }
                assert!(Instant::now() < deadline, "lint worker did not answer");
                std::thread::sleep(Duration::from_millis(5));
            };
            assert_eq!(result.diagnostics.len(), 1, "{:?}", result.diagnostics);
            let finding = &result.diagnostics[0];
            assert_eq!(
                &app.editor().source()[finding.from..finding.to],
                "kaskaskjas"
            );
            app.absorb_lint(result);
            assert_eq!(app.go_state(), GoState::Problems { count: 1 });
            assert!(!app.lint_ranges(app.scenes.current().id).is_empty());
            assert_eq!(app.next_request_id, request_id);
            assert!(app.inflight.is_empty());
        }
    }

    mod readiness {
        //! Tests for `app/readiness.rs`.

        use crate::app::readiness::*;
        use crate::app::tests::support::*;

        #[test]
        fn readiness_reads_banks_and_counts_each_sound_once() {
            assert_eq!(
                rustel_runtime::sounds::banks_in_score(
                    "s(\"bd\").bank(\"RolandTR909\")\n$: s(\"hh\").bank('<tr808 tr909>')"
                ),
                ["RolandTR909", "tr808", "tr909"]
            );
            let library = rustel_runtime::samples::SampleLibrary::empty();
            library
                .register_trusted_custom(r#"{"tr909_bd":["http://127.0.0.1:9/a.wav"]}"#, None)
                .expect("bank");
            let readiness = Readiness::of("s(\"bd sine nope\").bank(\"tr909\")", &library);
            // `sine` is a synth and ready; `bd` resolves through the bank and is
            // on its way; `nope` is nobody's.
            assert_eq!(readiness.ready, 1, "{readiness:?}");
            assert_eq!(readiness.loading, 1, "{readiness:?}");
            assert_eq!(readiness.unknown, 1, "{readiness:?}");
            assert_eq!(readiness.total(), 3);
        }

        /// The samples start downloading as the names are typed - the string
        /// still open, no lint yet, the bank named on the same keystrokes.
        #[test]
        fn typing_a_sound_name_asks_the_library_before_any_lint() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = app_over(directory.path());
            let library = std::sync::Arc::new(rustel_runtime::samples::SampleLibrary::empty());
            library
                .register_trusted_custom(r#"{"tr909_bd":["http://127.0.0.1:9/a.wav"]}"#, None)
                .expect("bank");
            app.worker.set_library(std::sync::Arc::clone(&library));
            let scene = app.scenes.current().id;
            assert!(!app.readiness.contains_key(&scene));

            type_text(&mut app, "$: s(\"bd\").bank(\"tr909");
            let (due_scene, due) = app.readiness_due.expect("a keystroke schedules the ask");
            assert_eq!(due_scene, scene);
            assert!(!app.lint.contains_key(&scene), "no check has run");

            app.settle_readiness(due - Duration::from_millis(1));
            assert!(!app.readiness.contains_key(&scene), "not before the settle");
            app.settle_readiness(due);
            let readiness = app.readiness.get(&scene).expect("asked");
            assert_eq!(
                readiness.loading, 1,
                "tr909_bd is on its way: {readiness:?}"
            );
            assert!(app.readiness_due.is_none(), "spent");
            assert!(
                matches!(app.go_state(), GoState::Loading { total: 1, .. }),
                "the header says so without waiting for the checker"
            );
        }

        /// An update the engine refused leaves its message in the footer while
        /// the refused text stands, even checked clean again; once the text has
        /// changed and the checker finds it fine, the message comes down.
        #[test]
        fn a_refused_update_message_comes_down_once_the_edited_text_checks_out() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = app_over(directory.path());
            let scene = app.scenes.current().id;
            app.log.push(LogLevel::Warn, "device", "unrelated warning");
            let unseen_before = app.log.unseen();
            type_text(&mut app, "$: s(\"bd\"");
            let refused = app.scenes.current().editor.revision();
            app.inflight.insert(
                3,
                PendingEvaluation {
                    request_id: 3,
                    scene,
                    editor_revision: refused,
                    source: Arc::from("$: s(\"bd\""),
                    launch: Launch::Now,
                    rewind: false,
                    record: false,
                },
            );
            app.handle_evaluation(EvaluationOutcome {
                request_id: 3,
                editor_revision: refused.0,
                result: Err(crate::EngineFailure {
                    kind: "compile".into(),
                    message: "Expected `)` but found `EOF`".into(),
                    recoverable: true,
                    playback_stopped: false,
                }),
            });
            assert_eq!(app.update_failure, Some((scene, refused)));
            assert_eq!(app.log.unseen(), unseen_before + 1);
            let message = app.errors.visible().expect("the footer says so").to_owned();
            assert!(message.contains("Expected `)`"), "{message}");

            let clean = |revision| crate::lint::LintResult {
                scene,
                revision,
                input_channels: None,
                diagnostics: Vec::new(),
            };
            app.absorb_lint(clean(refused));
            assert_eq!(
                app.errors.visible(),
                Some(message.as_str()),
                "the same text checked clean keeps the message"
            );

            assert_eq!(app.log.unseen(), unseen_before + 1);
            type_text(&mut app, ")");
            let stale = app.scenes.current().editor.revision();
            type_text(&mut app, " ");
            let fixed = app.scenes.current().editor.revision();
            assert!(fixed > stale && stale > refused);
            app.absorb_lint(clean(stale));
            assert_eq!(app.errors.visible(), Some(message.as_str()));
            assert_eq!(
                app.log.unseen(),
                unseen_before + 1,
                "a stale check resolves nothing"
            );
            app.absorb_lint(clean(fixed));
            assert_eq!(app.errors.visible(), None, "fixed and found fine: gone");
            assert_eq!(app.update_failure, None);
            assert_eq!(
                app.log.unseen(),
                unseen_before,
                "the unrelated warning remains"
            );
            assert!(
                app.log
                    .lines()
                    .iter()
                    .any(|line| line.kind == "update" && line.resolved)
            );
        }

        #[test]
        fn a_stopped_scores_recoverable_error_clears_after_the_edit_checks_out() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = app_over(directory.path());
            app.audible_scene = None;
            assert!(app.audible_text().is_none(), "the test starts stopped");

            let scene = app.scenes.current().id;
            let refused = app.editor().revision();
            app.handle_control(StudioControlEvent::Diagnostic(
                crate::engine::StudioDiagnostic {
                    kind: "audio".into(),
                    message: "reverb capacity reached".into(),
                    recoverable: true,
                    level: StudioDiagnosticLevel::Error,
                    alert: None,
                },
            ));
            let unseen_before = app.log.unseen();
            app.handle_control(StudioControlEvent::Diagnostic(
                crate::engine::StudioDiagnostic {
                    kind: "evaluation".into(),
                    message: "[mini] parse error at line 1".into(),
                    recoverable: true,
                    level: StudioDiagnosticLevel::Error,
                    alert: None,
                },
            ));
            assert_eq!(app.update_failure, Some((scene, refused)));
            assert!(app.errors.get(ErrorOwner::Evaluation).is_some());
            assert_eq!(app.log.unseen(), unseen_before + 1);

            type_text(&mut app, " ");
            let fixed = app.editor().revision();
            app.absorb_lint(crate::lint::LintResult {
                scene,
                revision: fixed,
                input_channels: app.lint_input_channels(),
                diagnostics: Vec::new(),
            });

            assert_eq!(app.errors.get(ErrorOwner::Evaluation), None);
            assert_eq!(app.update_failure, None);
            assert_eq!(app.log.unseen(), unseen_before, "the audio warning remains");
            assert!(
                app.log
                    .lines()
                    .iter()
                    .any(|line| line.kind == "evaluation" && line.resolved)
            );
            assert!(
                app.log
                    .lines()
                    .iter()
                    .any(|line| line.kind == "audio" && !line.resolved)
            );
        }

        #[test]
        fn a_clean_live_lint_resolves_its_warning_badge_without_an_update() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = app_over(directory.path());
            app.log
                .push(LogLevel::Warn, "device", "an unrelated warning");

            type_text(&mut app, "$: s(\"bd\"");
            let scene = app.scenes.current().id;
            let refused = app.editor().revision();
            let source = Arc::<str>::from(app.editor().source());
            assert!(app.refuse_unclean(refused, &source));
            assert_eq!(app.update_failure, Some((scene, refused)));
            assert!(app.status.starts_with("refused -"), "{}", app.status);
            assert_eq!(app.log.unseen(), 2);

            type_text(&mut app, ")");
            let fixed = app.editor().revision();
            app.absorb_lint(crate::lint::LintResult {
                scene,
                revision: fixed,
                input_channels: app.lint_input_channels(),
                diagnostics: Vec::new(),
            });

            assert_eq!(
                app.log.unseen(),
                1,
                "the fixed lint alert clears without hiding the device warning"
            );
            assert_eq!(app.update_failure, None);
            assert_eq!(app.status, "ready");
            assert!(
                app.log.lines().iter().any(|line| line.kind == "check"),
                "the resolved refusal remains in the log"
            );
        }

        /// The score's imports count in the header like its sounds: one on its
        /// way keeps it at loading, one refused says failed, and one the
        /// library has not been asked for yet is not counted at all.
        #[test]
        fn imports_count_in_the_headers_readiness() {
            let library = rustel_runtime::samples::SampleLibrary::empty();
            library.note_samples_source_state_for_tests(
                "github:me/slow",
                rustel_runtime::samples::SourceState::Loading,
            );
            library.note_samples_source_state_for_tests(
                "github:me/gone",
                rustel_runtime::samples::SourceState::Failed("refused".into()),
            );
            let of = |source: &str| Readiness::of(source, &library);
            assert_eq!(of("samples('github:me/slow')").loading, 1);
            assert_eq!(of("samples('github:me/gone')").failed, 1);
            assert_eq!(of("samples('github:me/unasked')").total(), 0);
            assert_eq!(
                of("// samples('github:me/gone')").total(),
                0,
                "a commented-out import is no import"
            );
        }

        /// A hyphenated name is one sound, looked up whole at the variant it
        /// names: `mlkr-grsl:3` is on its way, not two names nobody has.
        #[test]
        fn a_hyphenated_name_is_one_sound() {
            let library = rustel_runtime::samples::SampleLibrary::empty();
            library
                .register_trusted_custom(
                    r#"{"_base":"http://127.0.0.1:9/","mlkr-grsl":["a.wav","b.wav","c.wav","d.wav"]}"#,
                    None,
                )
                .expect("bank");
            let readiness = Readiness::of("s(\"mlkr-grsl:3\")", &library);
            assert_eq!(readiness.loading, 1, "{readiness:?}");
            assert_eq!(readiness.total(), 1, "{readiness:?}");
        }

        /// The line under the caret keeps its marks to itself while it is being
        /// typed on; a pause, or leaving the line, paints them. The header says
        /// so all along.
        #[test]
        fn marks_stay_off_the_line_being_typed_until_the_typing_pauses() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = app_over(directory.path());
            let scene = app.scenes.current().id;
            type_text(&mut app, "$: s(\n$: s(\"hh\")");
            press(&mut app, KeyCode::Up, KeyModifiers::NONE);
            let source = app.scenes.current().editor.source().to_string();
            let broken = source.find("$: s(\n").expect("the broken line") + 5;
            let revision = app.scenes.current().editor.revision();
            app.absorb_lint(crate::lint::LintResult {
                scene,
                revision,
                input_channels: None,
                diagnostics: vec![rustel_runtime::lint::Diagnostic {
                    level: rustel_runtime::lint::Level::Syntax,
                    message: "Expected `)` but found `EOF`".into(),
                    from: broken - 2,
                    to: broken,
                }],
            });
            assert!(app.lint_ranges(scene).is_empty(), "quiet under the fingers");
            assert!(app.lint_message().is_some(), "the header still says so");

            press(&mut app, KeyCode::Down, KeyModifiers::NONE);
            assert_eq!(
                app.lint_ranges(scene),
                [(broken - 2, broken)],
                "leaving the line paints it"
            );
            press(&mut app, KeyCode::Up, KeyModifiers::NONE);
            assert!(app.lint_ranges(scene).is_empty(), "back on it, quiet again");

            app.last_edit_at = Instant::now() - MARK_SETTLE;
            app.settle_marks(Instant::now());
            assert!(app.marks_muted.is_none() && app.dirty_frame);
            assert_eq!(
                app.lint_ranges(scene),
                [(broken - 2, broken)],
                "a pause paints it"
            );
        }

        /// The sounds a load skipped arrive once, as one line under an alert: the
        /// log keeps it and the header counts it, the status line is left to the
        /// music, and the engine resolving the alert stops the count.
        #[test]
        fn the_late_sounds_line_is_logged_quietly_and_resolves() {
            let directory = tempfile::tempdir().expect("tempdir");
            let mut app = app_over(directory.path());
            let message = "notes skipped while loading: bd:0, hh:2";
            app.status = "quiet".into();
            let unseen = app.log.unseen();
            app.handle_control(StudioControlEvent::Diagnostic(crate::StudioDiagnostic {
                kind: rustel_runtime::SAMPLE_LOADING_DIAGNOSTIC.into(),
                message: message.into(),
                recoverable: true,
                level: StudioDiagnosticLevel::Warning,
                alert: Some(DiagnosticAlert::Raise("samples:late:1".into())),
            }));
            assert_eq!(app.status, "quiet", "the status line stays on the music");
            assert!(app.log.lines().iter().any(|line| line.text == message));
            assert_eq!(app.log.unseen(), unseen + 1);

            app.handle_control(StudioControlEvent::Diagnostic(crate::StudioDiagnostic {
                kind: "alert".into(),
                message: String::new(),
                recoverable: true,
                level: StudioDiagnosticLevel::Trace,
                alert: Some(DiagnosticAlert::Resolve("samples:late:1".into())),
            }));
            assert_eq!(app.log.unseen(), unseen, "the header counts it down");
            let line = app
                .log
                .lines()
                .iter()
                .find(|line| line.text == message)
                .expect("the line stays as history");
            assert!(line.resolved);
            assert!(
                !app.log.lines().iter().any(|line| line.kind == "alert"),
                "a resolution is not a line of its own"
            );
        }

        const GONE: &str = "github:me/gone";

        /// An app whose score imports [`GONE`], its library's pinned manifests in so
        /// an ask about the import waits behind nothing.
        fn app_importing_gone(
            directory: &std::path::Path,
        ) -> (App, Arc<rustel_runtime::samples::SampleLibrary>) {
            let mut app = app_over(directory);
            let library = app.worker.library().expect("the engine's library");
            library.wait_until_idle(Duration::from_secs(60));
            app.scenes.current_mut().editor =
                Editor::new(&format!("samples('{GONE}')\n$: s(\"bd\")\n")).expect("editor");
            (app, library)
        }

        /// Where [`GONE`] stands once the engine has answered with anything but
        /// `was`, asking through `ask` until it has. The engine's command line is
        /// short, so an ask can be dropped; the studio asks again on its next turn,
        /// and so does this.
        fn answered(
            library: &rustel_runtime::samples::SampleLibrary,
            was: &rustel_runtime::samples::SourceState,
            mut ask: impl FnMut(),
        ) -> Option<rustel_runtime::samples::SourceState> {
            let deadline = Instant::now() + Duration::from_secs(20);
            loop {
                ask();
                let state = library.samples_source_state(GONE);
                if state.as_ref() != Some(was) || Instant::now() >= deadline {
                    return state;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
        }

        fn update(app: &mut App) {
            let revision = app.editor().revision();
            let source = ropey::Rope::from_str(&app.editor().source());
            app.handle_editor_effect(EditorEffect::Evaluate { revision, source })
                .expect("update");
        }

        /// An import that failed is asked again once it has rested, by the
        /// half-second poll and by an update, which does not wait out the poll's
        /// longer rest, and the failure refuses the update until an answer replaces
        /// it.
        #[test]
        fn a_rested_import_failure_is_asked_again_by_the_poll_and_by_an_update() {
            use rustel_runtime::samples::SourceState;
            let directory = tempfile::tempdir().expect("temp dir");
            let (mut app, library) = app_importing_gone(directory.path());
            let offline = SourceState::Failed("offline".into());
            library.note_samples_source_state_for_tests(GONE, offline.clone());

            update(&mut app);
            assert!(
                app.status.starts_with("refused - ") && app.status.contains("samples: offline"),
                "{}",
                app.status
            );

            // Rested, the poll asks again. This studio grants scores no network, so
            // the answer is a refusal with a reason of its own.
            library.rest_samples_source_for_tests(GONE);
            let answer = answered(&library, &offline, || app.poll_readiness());
            assert!(
                matches!(&answer, Some(SourceState::Failed(reason)) if reason != "offline"),
                "{answer:?}"
            );

            // So does an update once the failure has rested a few seconds, however
            // often it failed in a row: after these, the poll's rest outlasts the
            // wait below. The update is still refused while the answer is on its
            // way.
            for _ in 0..4 {
                library.note_samples_source_state_for_tests(GONE, offline.clone());
            }
            library.rest_samples_source_for_tests(GONE);
            let answer = answered(&library, &offline, || {
                update(&mut app);
                assert!(app.status.starts_with("refused - "), "{}", app.status);
            });
            assert!(
                matches!(&answer, Some(SourceState::Failed(reason)) if reason != "offline"),
                "{answer:?}"
            );
        }

        /// An import that stops failing is checked again without an edit, so its
        /// finding does not outlast the failure.
        #[test]
        fn an_import_that_stops_failing_is_checked_again() {
            use rustel_runtime::samples::SourceState;
            let directory = tempfile::tempdir().expect("temp dir");
            let (mut app, library) = app_importing_gone(directory.path());
            library
                .note_samples_source_state_for_tests(GONE, SourceState::Failed("offline".into()));
            app.poll_readiness();
            app.lint_pending = false;

            library.note_samples_source_state_for_tests(GONE, SourceState::Ready);
            app.poll_readiness();
            assert!(app.lint_pending, "the checker looks again");
        }
    }

    mod syntax_check {
        //! The syntax check's modes. Unless it is full a paused text is not checked
        //! and nothing is marked, while everything else a pause does still happens;
        //! an update refuses a broken score the same way in every mode, and says why
        //! in the footer and the log, marking where only when the mode is not off.

        use crate::app::tests::support::app_over;
        use crate::app::*;

        /// A score that does not parse, and names an orbit so the mixer's strips
        /// have something to follow.
        const BROKEN: &str = "$: s(\"bd\").orbit(3) (((";

        fn app_with_library(directory: &std::path::Path) -> App {
            let mut app = app_over(directory);
            app.worker
                .set_library(Arc::new(rustel_runtime::samples::SampleLibrary::empty()));
            app
        }

        /// Wait for the lint thread's answer to a submitted request.
        fn lint_answer(app: &App) -> LintResult {
            let deadline = Instant::now() + Duration::from_secs(5);
            loop {
                if let Some(result) = app.linter.try_recv() {
                    return result;
                }
                assert!(Instant::now() < deadline, "lint worker did not answer");
                std::thread::sleep(Duration::from_millis(5));
            }
        }

        #[test]
        fn with_the_switch_off_a_pause_checks_nothing_but_still_settles_the_text() {
            let directory = tempfile::tempdir().unwrap();
            let mut app = app_with_library(directory.path());
            app.ui_settings.syntax_check = SyntaxCheck::OnUpdate;
            app.scenes.current_mut().editor = Editor::new(BROKEN).unwrap();
            let scene = app.scenes.current().id;

            app.lint_pending = true;
            app.submit_lint();

            assert!(
                !app.lint_pending,
                "the pause is spent, as it is with checking on"
            );
            assert!(!app.lint.contains_key(&scene), "no verdict is recorded");
            assert!(app.lint_ranges(scene).is_empty(), "nothing is underlined");
            assert!(app.lint_message().is_none(), "the footer says nothing");
            assert!(!matches!(app.go_state(), GoState::Problems { .. }));
            // What a pause does besides checking still happens.
            assert_eq!(app.orbits_in_code, vec![1, 3], "the mixer follows the text");
            assert!(
                app.readiness.contains_key(&scene),
                "the sounds are asked for as a check's result would have asked"
            );
            // And no request reached the lint thread: give it the time it takes
            // to answer one, and nothing arrives.
            std::thread::sleep(Duration::from_millis(250));
            assert!(
                app.linter.try_recv().is_none(),
                "the linter was never asked"
            );
        }

        #[test]
        fn with_the_switch_off_an_update_still_refuses_and_marks_it_until_the_text_moves_on() {
            let directory = tempfile::tempdir().unwrap();
            let mut app = app_with_library(directory.path());
            app.ui_settings.syntax_check = SyntaxCheck::OnUpdate;
            app.scenes.current_mut().editor = Editor::new(BROKEN).unwrap();
            let scene = app.scenes.current().id;
            let request_id = app.next_request_id;

            app.handle_editor_effect(EditorEffect::Evaluate {
                revision: app.editor().revision(),
                source: ropey::Rope::from_str(BROKEN),
            })
            .unwrap();

            assert!(app.status.starts_with("refused - "), "{}", app.status);
            assert_eq!(app.next_request_id, request_id, "no evaluation was queued");
            assert!(
                matches!(app.go_state(), GoState::Problems { .. }),
                "the refusal shows where it was refused"
            );

            // The text moves on and typing pauses: nothing checks it again, so the
            // refusal's marks do not outstay the text they were about.
            app.lint_pending = true;
            app.submit_lint();
            assert!(!app.lint.contains_key(&scene));
            assert!(app.lint_ranges(scene).is_empty());
            assert!(!matches!(app.go_state(), GoState::Problems { .. }));
        }

        #[test]
        fn switching_off_takes_the_marks_down_and_switching_on_checks_at_once() {
            let directory = tempfile::tempdir().unwrap();
            let mut app = app_with_library(directory.path());
            app.scenes.current_mut().editor = Editor::new(BROKEN).unwrap();
            let scene = app.scenes.current().id;

            // On, as it ships: the pause checks and the verdict is marked.
            assert_eq!(app.ui_settings.syntax_check, SyntaxCheck::Full);
            app.submit_lint();
            let result = lint_answer(&app);
            app.absorb_lint(result);
            assert!(matches!(app.go_state(), GoState::Problems { .. }));
            assert!(app.lint_message().is_some());

            // Off: every mark comes down now, not at the next keystroke.
            app.ui_settings.syntax_check = SyntaxCheck::OnUpdate;
            app.apply_syntax_check();
            assert!(app.lint.is_empty());
            assert!(app.lint_ranges(scene).is_empty());
            assert!(app.lint_message().is_none());
            assert!(!matches!(app.go_state(), GoState::Problems { .. }));

            // On again: the focused scene is due a check at the very next turn.
            app.lint_pending = false;
            app.ui_settings.syntax_check = SyntaxCheck::Full;
            app.apply_syntax_check();
            assert!(app.lint_pending);
            assert!(
                app.last_edit_at.elapsed() >= LINT_DEBOUNCE,
                "the debounce has already run out"
            );
            app.submit_lint();
            let result = lint_answer(&app);
            app.absorb_lint(result);
            assert!(matches!(app.go_state(), GoState::Problems { .. }));
        }

        #[test]
        fn a_check_already_under_way_when_the_switch_goes_off_is_not_shown() {
            let directory = tempfile::tempdir().unwrap();
            let mut app = app_with_library(directory.path());
            app.scenes.current_mut().editor = Editor::new(BROKEN).unwrap();
            let scene = app.scenes.current().id;

            app.submit_lint();
            app.ui_settings.syntax_check = SyntaxCheck::OnUpdate;
            app.apply_syntax_check();
            // Drain until the in-flight answer has certainly arrived and been
            // passed over.
            let deadline = Instant::now() + Duration::from_millis(1500);
            while Instant::now() < deadline {
                app.drain_lint();
                std::thread::sleep(Duration::from_millis(10));
            }
            assert!(
                !app.lint.contains_key(&scene),
                "the late answer is not kept"
            );
            assert!(!matches!(app.go_state(), GoState::Problems { .. }));
        }

        /// A minimal WAV: a header and two silent frames, enough for a folder scan
        /// to take it for a sample.
        fn tiny_wav() -> Vec<u8> {
            let data: [u8; 4] = [0; 4];
            let mut bytes = Vec::new();
            bytes.extend_from_slice(b"RIFF");
            bytes.extend_from_slice(&(36 + data.len() as u32).to_le_bytes());
            bytes.extend_from_slice(b"WAVEfmt ");
            bytes.extend_from_slice(&16u32.to_le_bytes());
            bytes.extend_from_slice(&1u16.to_le_bytes());
            bytes.extend_from_slice(&1u16.to_le_bytes());
            bytes.extend_from_slice(&8000u32.to_le_bytes());
            bytes.extend_from_slice(&16000u32.to_le_bytes());
            bytes.extend_from_slice(&2u16.to_le_bytes());
            bytes.extend_from_slice(&16u16.to_le_bytes());
            bytes.extend_from_slice(b"data");
            bytes.extend_from_slice(&(data.len() as u32).to_le_bytes());
            bytes.extend_from_slice(&data);
            bytes
        }

        /// The check at launch runs before the sample library has its banks, so it
        /// cannot judge a sound name, and nothing in the text changes when they
        /// arrive. The focused scene is checked again then, without a keystroke -
        /// once, not on every poll after.
        #[test]
        fn the_scene_is_checked_again_once_sound_names_can_be_judged() {
            let directory = tempfile::tempdir().unwrap();
            let mut app = app_over(directory.path());
            app.worker
                .set_library(Arc::new(rustel_runtime::samples::SampleLibrary::empty()));
            app.scenes.current_mut().editor = Editor::new("$: s(\"no-such-sound\")").unwrap();

            // Launch: the first check has been made, with nothing to judge by.
            app.poll_readiness();
            app.lint_pending = false;
            app.poll_readiness();
            assert!(!app.lint_pending, "nothing new to judge by, nothing to ask");

            // The library's banks arrive.
            let samples = directory.path().join("kit");
            let kicks = samples.join("kicks");
            std::fs::create_dir_all(&kicks).unwrap();
            std::fs::write(kicks.join("a.wav"), tiny_wav()).unwrap();
            let library = rustel_runtime::samples::SampleLibrary::empty();
            library.adopt_set_folder(&samples).expect("adopt the kit");
            app.worker.set_library(Arc::new(library));
            app.poll_readiness();
            assert!(app.lint_pending, "checked again once names can be judged");

            app.lint_pending = false;
            app.poll_readiness();
            assert!(!app.lint_pending, "and only the once");
        }

        /// Off, a refused update marks nothing in the editor - no underline, no
        /// count in the header - but the footer still says why and the log still
        /// has it: an error that keeps a score from playing is never kept quiet.
        #[test]
        fn off_marks_nothing_but_the_footer_and_the_log_still_say_why() {
            let directory = tempfile::tempdir().unwrap();
            let mut app = app_with_library(directory.path());
            app.ui_settings.syntax_check = SyntaxCheck::Off;
            app.scenes.current_mut().editor = Editor::new(BROKEN).unwrap();
            let scene = app.scenes.current().id;
            let request_id = app.next_request_id;

            app.handle_editor_effect(EditorEffect::Evaluate {
                revision: app.editor().revision(),
                source: ropey::Rope::from_str(BROKEN),
            })
            .unwrap();

            assert_eq!(app.next_request_id, request_id, "no evaluation was queued");
            assert!(!app.lint.contains_key(&scene), "nothing is marked");
            assert!(app.lint_ranges(scene).is_empty());
            assert!(!matches!(app.go_state(), GoState::Problems { .. }));
            assert!(
                app.status.starts_with("refused - "),
                "the footer says why: {}",
                app.status
            );
            assert!(
                app.log
                    .lines()
                    .iter()
                    .any(|line| line.kind == "check" && line.text.starts_with("refused - ")),
                "and the log has it"
            );
        }
    }
}
