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
    /// them again; and with Hydra, on every poll, it follows a sounding
    /// preview's sounds.
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
