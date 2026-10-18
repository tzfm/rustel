//! Finite playback and offline rendering of scores and recorded sessions.

use super::*;

impl Session {
    /// Schedule via `VirtualClock` for `duration_secs` of wall time.
    ///
    /// This base method returns an onset timeline and does not open a device;
    /// the opt-in finite CPAL adapter consumes the same report separately.
    pub fn play(&mut self, duration_secs: f64) -> Result<PlayReport, RuntimeError> {
        self.play_with_js_budget(duration_secs, QUERY_JS_CPU_BUDGET)
    }

    /// [`Session::play`] with an injected per-tick QuickJS budget for focused
    /// boundary tests.
    pub(super) fn play_with_js_budget(
        &mut self,
        duration_secs: f64,
        query_js_budget: Duration,
    ) -> Result<PlayReport, RuntimeError> {
        self.play_window(0.0, duration_secs, query_js_budget, true)
    }

    /// Schedule a window starting at `start_secs`, optionally re-anchoring the
    /// pattern at cycle zero.
    ///
    /// Recorded sets preserve the cycle/time mapping between windows so each
    /// saved edit takes effect without restarting the bar. `play` instead
    /// re-anchors each independent run.
    pub(crate) fn play_window(
        &mut self,
        start_secs: f64,
        duration_secs: f64,
        query_js_budget: Duration,
        reanchor: bool,
    ) -> Result<PlayReport, RuntimeError> {
        self.with_panic_recovery(start_secs, |session| {
            session.panic_is_query.set(true);
            #[cfg(any(test, feature = "test-support"))]
            session.panic_if_injected(SessionPanicPoint::Query);
            session.play_window_guarded(start_secs, duration_secs, query_js_budget, reanchor)
        })
    }

    fn play_window_guarded(
        &mut self,
        start_secs: f64,
        duration_secs: f64,
        query_js_budget: Duration,
        reanchor: bool,
    ) -> Result<PlayReport, RuntimeError> {
        // Validate here as well as in the CLI: every caller needs a finite,
        // non-negative window so timeline collection terminates. Zero is valid.
        if !duration_secs.is_finite() {
            return Err(RuntimeError::Message(format!(
                "duration must be a finite number, got {duration_secs}"
            )));
        }
        if duration_secs < 0.0 {
            return Err(RuntimeError::Message(format!(
                "duration must not be negative, got {duration_secs}"
            )));
        }
        if duration_secs > MAX_DURATION_SECS {
            return Err(RuntimeError::ResourceLimit(format!(
                "duration must be at most {MAX_DURATION_SECS} seconds, got {duration_secs}"
            )));
        }
        let pattern = self.js.active_pattern().ok_or(RuntimeError::NoPattern)?;
        // Handle zero before the collector's inclusive end bound can admit an
        // onset at the window's start.
        if duration_secs == 0.0 {
            return Ok(PlayReport {
                query_threw: None,
                duration_secs,
                cps: self.config.cps,
                generation: self.transport.generation(),
                audio_backend: "none".into(),
                device_audio: "not-requested".into(),
                note: "Zero-length window: nothing is scheduled.".into(),
                onsets: Vec::new(),
            });
        }
        let needs_host = self.js.active_needs_host();
        let clock = VirtualClock::new(start_secs);
        if reanchor {
            // `play()` is a new transport lifetime. A key placed before a
            // caller stopped the exposed Transport handle must not replay in
            // the new offline/render pass.
            self.js.midi_input_bus().clear_keys();
        }
        self.transport.start();
        let generation = if reanchor {
            let generation = self.scheduler.set_pattern(pattern, clock.now());
            // Replacement retains the old query cursor; a new play starts at zero.
            self.scheduler.rebase_start_anchor(clock.now());
            self.js
                .midi_input_bus()
                .republish_current_generation(generation);
            generation
        } else {
            // Already installed by the caller, mapping intact.
            self.transport.generation()
        };

        let js = &self.js;
        let scheduler = &mut self.scheduler;
        let cancellation = self.transport.stopped_flag();
        let horizon = self.config.horizon;
        let step = scheduler.refill_floor_seconds();

        // The scheduler queries the graph on its own clock, outside
        // `JsRuntime::query`, so callback-bearing graphs need an explicit host
        // scope for each tick.
        //
        // Keep the scope one tick wide. Nested callbacks allocate cells that
        // `call_bind` adds to the live `BridgeFrame`, which is a GC root. A
        // frame around the whole loop would retain every tick's cells until
        // playback ends.
        //
        // `drain_due` stays outside: it only filters the event queue,
        // never queries a pattern and never enters JavaScript, so it needs no
        // host, no query stack and no frame.
        let tick = |scheduler: &mut Scheduler,
                    clock: &VirtualClock|
         -> Result<rustel_scheduler::TickStatus, RuntimeError> {
            js.with_runtime_settings(|| {
                rustel_core::with_cancellation(cancellation, || {
                    if needs_host {
                        js.with_active_scope_cancellable(query_js_budget, cancellation, || {
                            scheduler.tick(clock)
                        })
                        .map_err(RuntimeError::from)
                    } else {
                        // Pure graphs need no JavaScript host, but long native
                        // queries still inherit the cancellation scope.
                        Ok(scheduler.tick(clock))
                    }
                })
            })
        };

        let mut onsets = Vec::new();
        let mut overflowed = false;
        // Captured before the closure borrows the session's parts.
        let direct_diagnostic_logging = self.direct_diagnostic_logging;
        let collect = |onsets: &mut Vec<OnsetEventJson>,
                       overflowed: &mut bool,
                       events: Vec<rustel_scheduler::Event>| {
            for event in events {
                // The whole timeline stays in memory until this call returns.
                // Bound onset count as well as duration: a short, dense pattern
                // can still produce an excessive allocation.
                if onsets.len() >= MAX_ONSETS {
                    *overflowed = true;
                    return;
                }
                if event.target_time <= start_secs + duration_secs + f64::EPSILON {
                    onsets.push(OnsetEventJson {
                        onset_id: event.onset_id,
                        generation: event.generation,
                        whole_begin: event.whole_begin.show(),
                        duration_secs: event.duration.to_f64() / self.config.cps,
                        target_time: event.target_time,
                        live_controls: event.live_controls,
                        ui_visuals: event.ui_visuals,
                        value: ValueJson::from_value(&event.value),
                        value_show: event.value.show(),
                        log_line: event.log_line.as_deref().map(str::to_owned),
                    });
                    // Print logs for accepted onsets so re-querying a span
                    // does not print twice. With direct logging off they are
                    // queued once the window is collected.
                    if let Some(line) = event.log_line.as_deref()
                        && direct_diagnostic_logging
                    {
                        eprintln!("{}", serde_json::json!({ "log": { "message": line } }));
                    }
                }
            }
        };

        let mut t = start_secs;
        let mut stopped = false;
        let mut queue_full = false;
        let mut refused: Option<rustel_core::QueryLimit> = None;
        let mut thrown: Option<String> = None;
        let mut callback_failure: Option<String> = None;
        let mut contained_throw: Option<String> = None;
        let mut iterations = 0u64;
        while t <= start_secs + duration_secs + horizon {
            iterations += 1;
            // The same report the live route makes: a query that threw is
            // silent by design, but never unexplained.
            if let Some(message) = scheduler.take_thrown() {
                thrown.get_or_insert(message);
            }
            if let Some(message) = scheduler.take_callback_failure() {
                callback_failure.get_or_insert(message);
            }
            if let Some(message) = scheduler.take_contained_throw() {
                contained_throw.get_or_insert(message);
            }
            let Some(status) = play_tick_or_stopped(tick(scheduler, &clock))? else {
                stopped = true;
                break;
            };
            match status {
                // Stop ends the loop, not merely the output. The saving is
                // modest: `tick` already returns early once stopped, but
                // breaking avoids empty iterations through the remaining
                // window.
                rustel_scheduler::TickStatus::Stopped => {
                    stopped = true;
                    break;
                }
                // A dense pattern refills the queue instantly, so ticking again
                // walks straight back into the same wall.
                rustel_scheduler::TickStatus::QueueFull => {
                    queue_full = true;
                    break;
                }
                // Stop can arrive after `Scheduler::tick`'s entry check while
                // a native query is running. Core then reports the same Stop as
                // a typed cancellation refusal. Preserve play's established
                // partial-success contract instead of reclassifying that race
                // as a resource failure.
                rustel_scheduler::TickStatus::Refused
                    if matches!(
                        scheduler.refusal(),
                        Some(rustel_core::QueryLimit::Cancelled)
                    ) =>
                {
                    stopped = true;
                    break;
                }
                // A refused query is not silence: report it rather than
                // scheduling nothing and calling that success.
                rustel_scheduler::TickStatus::Refused => {
                    refused = scheduler.refusal().cloned();
                    break;
                }
                _ => {}
            }
            collect(&mut onsets, &mut overflowed, scheduler.drain_due(&clock));
            if overflowed {
                break;
            }
            clock.advance(step);
            t += step;
        }
        if !stopped && !queue_full && !overflowed && refused.is_none() {
            // Final drain at the end of the window.
            clock.set(start_secs + duration_secs);
            if let Some(status) = play_tick_or_stopped(tick(scheduler, &clock))? {
                match status {
                    rustel_scheduler::TickStatus::Stopped => {}
                    rustel_scheduler::TickStatus::Refused
                        if matches!(
                            scheduler.refusal(),
                            Some(rustel_core::QueryLimit::Cancelled)
                        ) => {}
                    rustel_scheduler::TickStatus::Refused => {
                        refused = scheduler.refusal().cloned();
                    }
                    _ => collect(&mut onsets, &mut overflowed, scheduler.drain_due(&clock)),
                }
            }
        }

        // A throw raised by the last tick (the loop's final iteration or the
        // final drain) has no next-iteration take to pick it up, and a break
        // on stop, queue-full, overflow or refusal skips that take too. Take
        // every channel here or the report and diagnostics below lose them.
        if let Some(message) = scheduler.take_thrown() {
            thrown.get_or_insert(message);
        }
        if let Some(message) = scheduler.take_callback_failure() {
            callback_failure.get_or_insert(message);
        }
        if let Some(message) = scheduler.take_contained_throw() {
            contained_throw.get_or_insert(message);
        }
        if !direct_diagnostic_logging {
            for line in onsets.iter().filter_map(|onset| onset.log_line.as_deref()) {
                self.report_diagnostic(
                    "log",
                    line,
                    serde_json::json!({ "log": { "message": line } }),
                );
            }
        }

        self.last_play_iterations = iterations;
        // Typed all the way through: the limit decides the outcome, not the
        // wording of its message.
        if let Some(refusal) = refused {
            return Err(refusal.into());
        }
        if overflowed || queue_full {
            return Err(RuntimeError::ResourceLimit(format!(
                "scheduling exceeded its allocation bound (onsets cap \
                 {MAX_ONSETS}, queue cap {}). The whole timeline is held in \
                 memory before it is emitted, so this is refused rather than \
                 grown into: shorten the duration or thin the pattern.",
                rustel_scheduler::MAX_QUEUED_EVENTS
            )));
        }
        onsets.sort_by(|a, b| {
            a.target_time
                .partial_cmp(&b.target_time)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.onset_id.cmp(&b.onset_id))
        });
        onsets.dedup_by_key(|e| e.onset_id);

        // A window that threw produced no haps, which is right; reporting
        // nothing about it is not. Raised here rather than inside the loop so
        // one broken lane reports once, not once per tick.
        if let Some(message) = thrown.as_ref() {
            self.report_diagnostic(
                "query-threw",
                message.clone(),
                serde_json::json!({ "query_threw": { "message": message } }),
            );
        }
        // A contained callback failure is the same news, reported once for
        // the whole offline pass.
        if let Some(message) = callback_failure.as_ref() {
            self.report_diagnostic(
                "query-threw",
                message.clone(),
                serde_json::json!({ "query_threw": { "message": message } }),
            );
        }
        // `logger(...)` and `console.log(...)` are written while a score
        // evaluates, not when a note sounds, so the offline path drains them
        // here, at the first opportunity after. The per-onset `.log()` lines
        // are reported in collection order, above.
        for line in self.js.take_logs() {
            self.report_diagnostic(
                "log",
                line.clone(),
                serde_json::json!({ "log": { "message": line } }),
            );
        }

        Ok(PlayReport {
            // Only a throw empties a window. A contained callback failure
            // (a `filterValues`/`filterHaps` predicate that threw) fails
            // open: the hap it judged keeps playing and the bounce is
            // complete, so the diagnostic above reports it and the exit
            // status does not fail. A lane whose throw a stack contained
            // is missing from the bounce, so it does fail.
            query_threw: thrown.or(contained_throw),
            duration_secs,
            cps: self.config.cps,
            generation,
            audio_backend: "none".into(),
            device_audio: "not-requested".into(),
            note: "Emitted an onset timeline without opening an audio device.".into(),
            onsets,
        })
    }

    /// Play finite scalar output through CPAL's default device.
    #[cfg(feature = "device-audio")]
    pub fn play_on_device(&mut self, duration_secs: f64) -> Result<PlayReport, RuntimeError> {
        // The finite path gets the same sample world as live/render: boot the
        // default library (lazy, idempotent), kick every referenced load,
        // wait for the decodes, then hand the decoded PCM to the device.
        // Without this, events resolve against BundledOnly and any non-bd
        // sound is refused as an unknown wavetable.
        if let Err(error) = self.enable_default_samples() {
            let message = error.to_string();
            self.report_diagnostic(
                "sample-library-unavailable",
                message.clone(),
                serde_json::json!({
                    "sample_library": { "status": "unavailable", "message": message }
                }),
            );
        }
        let mut report = self.play(duration_secs)?;
        let device = rustel_audio::ScalarDevice::open_default()
            .map_err(|error| RuntimeError::Audio(error.to_string()))?;
        let csound = self.csound.for_render();
        let (events, notices) =
            rustel_voice::with_diagnostic_policy(self.direct_diagnostic_logging, || {
                if let Some(library) = &self.samples {
                    // The device, not the config, decides what rate this run
                    // decodes to.
                    library.set_render_rate(device.sample_rate());
                    for onset in &report.onsets {
                        let _ = crate::render::scalar_event(
                            onset,
                            device.sample_rate(),
                            self.config.cps,
                            library.as_ref(),
                        );
                    }
                    library.wait_until_idle(std::time::Duration::from_secs(120));
                }
                let lookup = crate::csound::Lookup {
                    samples: self.sample_lookup(),
                    state: &csound,
                };
                let events = crate::render::scalar_events(
                    &report.onsets,
                    device.sample_rate(),
                    self.config.cps,
                    &lookup,
                )?;
                Ok::<_, String>(events)
            });
        self.report_voice_notices(notices);
        let events =
            events.map_err(|error| RuntimeError::Message(format!("scalar audio: {error}")))?;
        let installs = match &self.samples {
            Some(library) => library
                .take_ready()
                .into_iter()
                .map(|(id, decoded)| (id, Box::new(decoded)))
                .collect(),
            None => Vec::new(),
        };
        match device.play_with_samples_dispatch_polyphony_and_inserts(
            &events,
            duration_secs,
            self.transport.stopped_flag(),
            installs,
            self.config.dsp_dispatch,
            self.max_polyphony(),
            Some(csound.provider(true)),
        ) {
            Ok(()) => {}
            Err(rustel_audio::DevicePlaybackError::Cancelled) => {
                return Err(RuntimeError::Cancelled);
            }
            Err(rustel_audio::DevicePlaybackError::ResourceLimit(message)) => {
                return Err(RuntimeError::ResourceLimit(message));
            }
            Err(error) => return Err(RuntimeError::Audio(error.to_string())),
        }
        if let Some(error) = csound.take_errors().into_iter().next() {
            return Err(RuntimeError::Audio(error));
        }
        report.audio_backend = "scalar-rust/cpal".into();
        report.device_audio = "played".into();
        report.note = format!(
            "Played finite scalar PCM on CPAL device {:?} at {}Hz/{}ch.",
            device.name(),
            device.sample_rate(),
            device.channels()
        );
        Ok(report)
    }

    /// Seconds of audio [`Session::render_session`] writes for `saves`: a
    /// positive `until_secs`, otherwise the last save's offset plus
    /// `tail_secs`, or `tail_secs` for an empty timeline.
    pub fn render_session_secs(
        saves: &[(f64, impl AsRef<str>)],
        tail_secs: f64,
        until_secs: Option<f64>,
    ) -> f64 {
        match until_secs {
            Some(until) if until > 0.0 => until,
            _ => saves
                .last()
                .map(|(at, _)| at + tail_secs.max(0.0))
                .unwrap_or(tail_secs),
        }
    }

    /// Render a recorded set offline: install each save at the moment it was
    /// made, collect the whole timeline, then render it in one pass.
    ///
    /// This is the difference between a bounce that takes as long as the set
    /// and one that takes as long as the machine needs - a thirty-minute
    /// performance in seconds. It is possible only because the renderer takes
    /// a flat onset list: voices, tails and takeovers cross save boundaries
    /// because nothing is spliced, and the cycle/time mapping is never
    /// re-anchored, so every score lands on the beat it landed on live.
    ///
    /// An onset on a save's own instant belongs to the score that save
    /// installs, so it bounces once; an onset on the set's closing instant is
    /// kept, even when a save lands on or past the end.
    ///
    /// A save that fails to evaluate is skipped and the previous score keeps
    /// playing through its window, so the rest of the set still bounces. The
    /// caller passes the saves the tape marks as installed, though, so such a
    /// save is not what happened at the time: the report carries the first
    /// one in [`RenderReport::failed_save`] and the caller fails its exit
    /// status on it, the file written either way.
    ///
    /// An empty timeline renders silence for the length
    /// [`Session::render_session_secs`] gives.
    ///
    /// Polyphony is currently resolved once, after collecting the saves. A
    /// fixed host budget is honored throughout; if saves change the explicit
    /// `setMaxPolyphony` module setting, the final accepted value applies to
    /// the entire bounce. The flat event stream does not yet carry a voice
    /// budget automation timeline.
    ///
    /// The bounce also resolves the tempo once. The cps in force after the
    /// last save resolves each onset of the bounce. In a tape with a tempo
    /// change, an onset before the last change takes its tempo-synced
    /// controls from the final cps. Its tremolo, the LFO of each filter and
    /// each `lfo()` modulator with no `retrig` start at the phase the final
    /// cps gives. The clock time and the gate length of each onset keep the
    /// cps of their own window.
    pub fn render_session(
        &mut self,
        saves: &[(f64, impl AsRef<str>)],
        tail_secs: f64,
        until_secs: Option<f64>,
        out_path: &Path,
        mp3: bool,
    ) -> Result<RenderReport, RuntimeError> {
        if mp3 {
            crate::render::ensure_mp3_available()?;
        }
        let mut onsets: Vec<OnsetEventJson> = Vec::new();
        let mut csound_saves = Vec::new();
        // The first window whose pattern threw: its span bounced silent, and
        // the report must say so exactly as a single-score render does - a
        // tape is handed to someone else, and "wrote the file" is not the
        // same claim as "the set is in it".
        let mut query_threw: Option<String> = None;
        // The first save that installed live but fails to evaluate now: the
        // previous save plays through its window instead, which is a bounce
        // of a different set.
        let mut failed_save: Option<String> = None;
        let mut installed_any = false;
        // The last played window's onsets, held until the saves at its end
        // decide who owns its closing instant.
        let mut held: Vec<OnsetEventJson> = Vec::new();
        let mut failed_on_replay = 0usize;
        let print_progress = self.direct_diagnostic_logging;
        let mut last_report = Instant::now();
        let started = Instant::now();
        let end_secs = Self::render_session_secs(saves, tail_secs, until_secs);

        for (index, (at, source)) in saves.iter().enumerate() {
            // Ctrl-C during a long bounce. `play_window` starts the transport
            // on every window, so a stop set between windows is cleared by the
            // next one unless the loop itself gives up.
            if self.transport.is_stopped() && installed_any {
                return Err(RuntimeError::Cancelled);
            }
            // A six-minute set is otherwise a minute of nothing. Report where
            // it is, on the set's clock, which is the number the artist knows.
            if print_progress && last_report.elapsed() >= Duration::from_secs(2) {
                last_report = Instant::now();
                crate::render::report_progress(
                    serde_json::json!({
                        "render_progress": {
                            "save": index + 1,
                            "of": saves.len(),
                            "set_secs": at.round(),
                            "onsets": onsets.len() + held.len(),
                            "elapsed_secs": started.elapsed().as_secs(),
                        }
                    }),
                    || {
                        format!(
                            "replaying: save {} of {}, {} s into the set, {} onsets so far ({} s elapsed)",
                            index + 1,
                            saves.len(),
                            at.round(),
                            onsets.len() + held.len(),
                            started.elapsed().as_secs()
                        )
                    },
                );
            }
            let first = !installed_any;
            let generation_before = self.generation();
            match self.evaluate_at_cancellable(
                source.as_ref(),
                *at,
                first,
                SCORE_CPU_BUDGET,
                &NEVER_CANCELLED,
                NoPatternPolicy::InstallSilence,
                first,
            ) {
                Ok(()) => installed_any = true,
                Err(error) => {
                    failed_on_replay += 1;
                    // Nothing has ever installed: there is no previous score to
                    // keep, so this is fatal rather than survivable.
                    if first {
                        return Err(error);
                    }
                    failed_save.get_or_insert_with(|| format!("at {at:.1} s: {error}"));
                }
            }
            // A save that installs before the end is queried from its own
            // instant, so the outgoing score's onsets from that instant on
            // are its replacement's, as the live swap retires them. A save
            // that fails, or one that never plays, leaves them.
            if *at < end_secs && self.generation() != generation_before {
                held.retain(|onset| onset.target_time < *at);
                let snapshot = self.csound.snapshot();
                if csound_saves.last().is_some_and(|(last, _)| last == at) {
                    csound_saves.pop();
                }
                if csound_saves
                    .last()
                    .is_none_or(|(_, previous)| previous != &snapshot)
                {
                    csound_saves.push((*at, snapshot));
                }
            }
            let next = saves
                .get(index + 1)
                .map(|(next_at, _)| *next_at)
                .unwrap_or(end_secs)
                .min(end_secs);
            let window = (next - at).max(0.0);
            if window <= 0.0 {
                continue;
            }
            let report = self.play_window(*at, window, QUERY_JS_CPU_BUDGET, false)?;
            query_threw = query_threw.or(report.query_threw);
            onsets.append(&mut held);
            held = report.onsets;
        }
        // The set's end is not a save: the last window keeps its closing
        // instant.
        onsets.append(&mut held);

        onsets.sort_by(|a, b| {
            a.target_time
                .partial_cmp(&b.target_time)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.onset_id.cmp(&b.onset_id))
        });
        onsets.dedup_by_key(|onset| onset.onset_id);
        let csound = self.csound.with_render_timeline(csound_saves);

        // The mp3 door is the same render; only the container differs, so a
        // tape bounced either way is the same audio.
        let (_, voices_refused) = self.render_under_diagnostic_policy(|session| {
            if mp3 {
                crate::render::write_scalar_mp3_with_dispatch(
                    out_path,
                    session.config.sample_rate,
                    end_secs,
                    &onsets,
                    session.config.cps,
                    session.samples.as_deref(),
                    session.config.dsp_dispatch,
                    session.max_polyphony(),
                    Some(&csound),
                )
            } else {
                crate::render::write_scalar_wav_controlled_with_dispatch(
                    out_path,
                    session.config.sample_rate,
                    end_secs,
                    &onsets,
                    session.config.cps,
                    session.samples.as_deref(),
                    print_progress,
                    rustel_audio::RenderControl {
                        observer: None,
                        cancelled: Some(session.transport.stopped_flag()),
                        finish: None,
                        stop_when_silent: None,
                        limiter: None,
                    },
                    rustel_audio::WavSampleFormat::Pcm16,
                    session.config.dsp_dispatch,
                    session.max_polyphony(),
                    Some(&csound),
                )
            }
            .map_err(render_error)
        })?;
        self.log_sample_failures();

        Ok(RenderReport {
            query_threw,
            failed_save,
            duration_secs: end_secs,
            cps: self.config.cps,
            sample_rate: self.config.sample_rate,
            channels: self.config.channels,
            format: if mp3 {
                "mp3-320-scalar".into()
            } else {
                "wav-scalar-pcm".into()
            },
            path: out_path.display().to_string(),
            audio_backend: "scalar-rust".into(),
            device_audio: "not-requested".into(),
            note: format!(
                "Offline session bounce: {} saves installed on their recorded clock, \
                 {failed_on_replay} failed to evaluate on replay (the previous save played in \
                 their place). Deterministic scalar render.",
                saves.len() - failed_on_replay
            ),
            onset_count: onsets.len(),
            voices_refused,
        })
    }

    /// End a bounce when the music stops rather than when the clock does.
    ///
    /// `floor` is linear amplitude and `hold` is how long the output has to
    /// stay under it. They are separate because a score may write a bar of
    /// silence on purpose: the threshold decides what counts as quiet, the
    /// hold decides how much quiet means finished.
    /// Render this much past the scheduled length: room for the last
    /// event's tail, which a silence stop then ends.
    pub fn set_render_tail(&mut self, secs: f64) {
        self.render_tail_secs = secs.max(0.0);
    }

    /// Apply a master limiter to WAV/MP3 file exports. `None` bypasses it;
    /// score-level `.limit()` effects are independent of this setting.
    pub fn set_export_limiter(&mut self, limiter: Option<rustel_audio::RenderLimiter>) {
        self.export_limiter = limiter;
    }

    pub fn stop_export_when_silent(&mut self, floor: f32, hold: std::time::Duration) {
        self.stop_when_silent = Some(rustel_audio::SilenceStop {
            floor,
            hold_frames: (hold.as_secs_f64() * f64::from(self.config.sample_rate)).round() as usize,
            after_frames: 0,
        });
    }

    /// Render `duration_secs` of the evaluated score to `out_path`.
    ///
    /// Progress is printed to stderr only when direct diagnostic logging is
    /// on (see [`Self::set_direct_diagnostic_logging`]). Samples other than
    /// the bundled `bd` need [`Self::enable_default_samples`] first unless
    /// the score loads them itself.
    pub fn render(
        &mut self,
        duration_secs: f64,
        out_path: &Path,
        format: RenderFormat,
    ) -> Result<RenderReport, RuntimeError> {
        let report = self.direct_diagnostic_logging;
        self.render_controlled(duration_secs, out_path, format, report, None, None)
    }

    /// Render `duration_secs` of the evaluated score as interleaved stereo
    /// f32 PCM, without writing a file.
    ///
    /// The same scalar path the offline writers use, stopping one step short
    /// of a container. A caller that wants to measure or hash a render - the
    /// corpus suite does exactly that, 1400 times - should not have to spool
    /// megabytes through the filesystem and parse them back.
    ///
    /// Samples other than the bundled `bd` need
    /// [`Self::enable_default_samples`] first unless the score loads them
    /// itself.
    pub fn render_pcm(&mut self, duration_secs: f64) -> Result<Vec<f32>, RuntimeError> {
        let play = self.play(duration_secs)?;
        self.render_under_diagnostic_policy(|session| {
            crate::render::render_scalar_pcm_with_refusals(
                session.config.sample_rate,
                duration_secs,
                &play.onsets,
                session.config.cps,
                session.samples.as_deref(),
                session.config.dsp_dispatch,
                session.max_polyphony(),
                Some(&session.csound),
            )
            .map_err(RuntimeError::Io)
        })
        .map(|(pcm, _)| pcm)
    }

    /// Run an offline render under this Session's diagnostic policy: with
    /// direct logging on, voice notices and refusals print as they happen;
    /// otherwise both are queued for [`Self::take_diagnostics`]. Returns the
    /// distinct refusals either way.
    fn render_under_diagnostic_policy<T>(
        &mut self,
        render: impl FnOnce(&Self) -> Result<(T, Vec<String>), RuntimeError>,
    ) -> Result<(T, Vec<String>), RuntimeError> {
        let direct = self.direct_diagnostic_logging;
        let (rendered, notices) = rustel_voice::with_diagnostic_policy(direct, || render(self));
        self.report_voice_notices(notices);
        let (rendered, refused) = rendered?;
        // With direct logging the render already printed each refusal.
        if !direct {
            for message in &refused {
                self.report_diagnostic(
                    "voice-refused",
                    message.clone(),
                    serde_json::json!({ "voice_refused": { "message": message } }),
                );
            }
        }
        Ok((rendered, refused))
    }

    /// [`Self::render`] watched and steerable: `observer` sees every block
    /// written (a progress bar, a scope), `finish` ends the bounce early
    /// with a short fade and keeps the file, and `report` decides whether
    /// progress is also printed to stderr as the CLI does. An mp3 is
    /// rendered the same way, to a WAV beside the target, then encoded.
    pub fn render_controlled<'o>(
        &mut self,
        duration_secs: f64,
        out_path: &Path,
        format: RenderFormat,
        report: bool,
        observer: Option<&'o mut (dyn FnMut(rustel_audio::RenderTick<'_>) + 'o)>,
        finish: Option<&std::sync::atomic::AtomicBool>,
    ) -> Result<RenderReport, RuntimeError> {
        if format == RenderFormat::ScalarMp3 {
            crate::render::ensure_mp3_available()?;
        }
        let play = self.play(duration_secs)?;
        // `--until-silence` ends the file early, so the requested duration is
        // a ceiling rather than a fact. Report what is actually on disk. The
        // scheduler stopped at the length; the audio may run on into the
        // tail, until the silence stop - when one is set - ends it, and a
        // rest inside the length is never mistaken for the end.
        let audio_secs = duration_secs + self.render_tail_secs;
        let stop_when_silent = self.stop_when_silent.map(|stop| rustel_audio::SilenceStop {
            after_frames: if self.render_tail_secs > 0.0 {
                (duration_secs * f64::from(self.config.sample_rate)).round() as usize
            } else {
                stop.after_frames
            },
            ..stop
        });
        let rendered = self.render_under_diagnostic_policy(|session| match format {
            RenderFormat::Wav => {
                // A silent skeleton has no tail to ring out.
                write_silent_wav(
                    out_path,
                    session.config.sample_rate,
                    session.config.channels,
                    duration_secs,
                )?;
                Ok((None, Vec::new()))
            }
            // The unwatched mp3 shortcut renders the whole length blind; a
            // silence stop or a tail needs the controlled path.
            RenderFormat::ScalarMp3
                if observer.is_none()
                    && finish.is_none()
                    && stop_when_silent.is_none()
                    && session.export_limiter.is_none()
                    && session.render_tail_secs == 0.0 =>
            {
                let (_, refused) = crate::render::write_scalar_mp3_with_dispatch(
                    out_path,
                    session.config.sample_rate,
                    audio_secs,
                    &play.onsets,
                    session.config.cps,
                    session.samples.as_deref(),
                    session.config.dsp_dispatch,
                    session.max_polyphony(),
                    Some(&session.csound),
                )
                .map_err(|error| match error.kind() {
                    std::io::ErrorKind::InvalidInput => {
                        RuntimeError::Message(format!("scalar audio: {error}"))
                    }
                    _ => RuntimeError::Io(error),
                })?;
                Ok((None, refused))
            }
            RenderFormat::ScalarMp3 => {
                // Watched or steerable: render as WAV beside the target so
                // the same loop drives the scope and the early finish, then
                // encode what was kept. The stage is 16-bit on purpose - it
                // exists to feed the mp3 encoder, not to be kept.
                let staged = out_path.with_extension("rendering.wav");
                let rendered = crate::render::write_scalar_wav_controlled_with_dispatch(
                    &staged,
                    session.config.sample_rate,
                    audio_secs,
                    &play.onsets,
                    session.config.cps,
                    session.samples.as_deref(),
                    report,
                    rustel_audio::RenderControl {
                        observer,
                        cancelled: Some(session.transport.stopped_flag()),
                        finish,
                        stop_when_silent,
                        limiter: session.export_limiter,
                    },
                    rustel_audio::WavSampleFormat::Pcm16,
                    session.config.dsp_dispatch,
                    session.max_polyphony(),
                    Some(&session.csound),
                )
                .map_err(render_error);
                let encoded = rendered.and_then(|(bytes, refused)| {
                    let pcm =
                        crate::render::read_pcm16_stereo_wav(&staged).map_err(RuntimeError::Io)?;
                    crate::render::encode_mp3(out_path, session.config.sample_rate, &pcm)
                        .map_err(RuntimeError::Io)?;
                    Ok((Some(bytes), refused))
                });
                let _ = std::fs::remove_file(&staged);
                encoded
            }
            RenderFormat::ScalarWav | RenderFormat::ScalarF32Wav => {
                crate::render::write_scalar_wav_controlled_with_dispatch(
                    out_path,
                    session.config.sample_rate,
                    audio_secs,
                    &play.onsets,
                    session.config.cps,
                    session.samples.as_deref(),
                    // Any bounce can be the long one: it is the score's
                    // density and duration that decide, not which command
                    // asked for it. Ten minutes of silence reads as a hang.
                    report,
                    rustel_audio::RenderControl {
                        observer,
                        cancelled: Some(session.transport.stopped_flag()),
                        finish,
                        stop_when_silent,
                        limiter: session.export_limiter,
                    },
                    match format {
                        RenderFormat::ScalarF32Wav => rustel_audio::WavSampleFormat::Float32,
                        _ => rustel_audio::WavSampleFormat::Pcm16,
                    },
                    session.config.dsp_dispatch,
                    session.max_polyphony(),
                    Some(&session.csound),
                )
                .map(|(bytes, refused)| (Some(bytes), refused))
                .map_err(render_error)
            }
            RenderFormat::OnsetJson => {
                write_onset_dump(out_path, &play.onsets)?;
                Ok((None, Vec::new()))
            }
        })?;
        let (written_bytes, voices_refused) = rendered;
        self.log_sample_failures();
        Ok(RenderReport {
            query_threw: play.query_threw.clone(),
            failed_save: None,
            duration_secs: match written_bytes {
                Some(bytes) => {
                    // Only the float WAV stores 4-byte samples. The mp3 stage
                    // is a 16-bit WAV.
                    let sample_bytes = if format == RenderFormat::ScalarF32Wav {
                        4
                    } else {
                        2
                    };
                    let frames = bytes / (usize::from(self.config.channels.max(1)) * sample_bytes);
                    frames as f64 / f64::from(self.config.sample_rate)
                }
                None => duration_secs,
            },
            cps: self.config.cps,
            sample_rate: self.config.sample_rate,
            channels: self.config.channels,
            format: match format {
                RenderFormat::Wav => "wav-silent-pcm".into(),
                RenderFormat::ScalarWav => "wav-scalar-pcm".into(),
                RenderFormat::ScalarF32Wav => "wav-scalar-f32".into(),
                RenderFormat::OnsetJson => "onset-json".into(),
                RenderFormat::ScalarMp3 => "mp3-scalar-320k".into(),
            },
            path: out_path.display().to_string(),
            audio_backend: match format {
                RenderFormat::ScalarWav | RenderFormat::ScalarF32Wav | RenderFormat::ScalarMp3 => {
                    "scalar-rust".into()
                }
                _ => "none".into(),
            },
            device_audio: "not-requested".into(),
            note: match format {
                RenderFormat::ScalarWav => "Deterministic scalar PCM render.".into(),
                RenderFormat::ScalarF32Wav => {
                    "Deterministic scalar render, 32-bit float and unclamped.".into()
                }
                _ => "Offline render without device playback.".into(),
            },
            onset_count: play.onsets.len(),
            voices_refused,
        })
    }
}

impl Session {
    /// Produce fixed-layout audio-ring events off the real-time thread.
    ///
    /// This works without `device-audio`: a host can send the returned events
    /// through [`rustel_audio::Ring`] to its own audio callback. `now` is in
    /// seconds on the session clock; `sample_rate` must match the host's DSP
    /// rate so each event's absolute `target_frame` uses the same clock.
    pub fn schedule_audio_at(
        &mut self,
        now: f64,
        sample_rate: u32,
    ) -> Result<Vec<rustel_audio::AudioEvent>, RuntimeError> {
        self.schedule_audio_through(now, now + self.config.horizon, sample_rate)
    }

    /// Produce fixed-layout audio-ring events through `through`, entirely
    /// off the real-time thread, without requiring an audio device backend.
    ///
    /// `now` and `through` are seconds on the session clock. The deadline is
    /// limited to the configured lookahead horizon, as in
    /// [`Self::schedule_through`]. Scheduling advances the shared cursor, so
    /// callers should use one scheduling method per window.
    pub fn schedule_audio_through(
        &mut self,
        now: f64,
        through: f64,
        sample_rate: u32,
    ) -> Result<Vec<rustel_audio::AudioEvent>, RuntimeError> {
        self.with_panic_recovery(now, |session| {
            session.schedule_audio_through_guarded(now, through, sample_rate)
        })
    }

    fn schedule_audio_through_guarded(
        &mut self,
        now: f64,
        through: f64,
        sample_rate: u32,
    ) -> Result<Vec<rustel_audio::AudioEvent>, RuntimeError> {
        // Per-voice refusals (unknown sound, unsupported control) skip THAT
        // voice, exactly as a dropped event leaves the rest playing. One bad
        // onset must never silence the whole batch. Each distinct refusal is
        // logged once.
        self.log_sample_failures();
        let library = self.samples.clone();
        let bundled = rustel_voice::BundledOnly;
        let lookup: &dyn rustel_voice::SampleLookup = match &library {
            Some(library) => library.as_ref(),
            None => &bundled,
        };
        let mut refusals = Vec::new();
        let cps = self.config.cps;
        let onsets = self.schedule_through(now, through)?;
        // Emit `.log()` text for scheduled onsets. Formatting happens during
        // the pattern query, but emission waits until an onset is accepted
        // so re-querying a span cannot print the same onset twice.
        //
        // `logger(...)` and `console.log(...)` are drained in the same pass.
        // Those are written while a score EVALUATES rather than when a note
        // sounds, so they come out at the first opportunity after.
        let mut logged: Vec<String> = self.js.take_logs();
        logged.extend(onsets.iter().filter_map(|onset| onset.log_line.clone()));
        for line in logged {
            self.report_diagnostic(
                "log",
                line.clone(),
                serde_json::json!({ "log": { "message": line } }),
            );
        }

        #[cfg(feature = "serial")]
        {
            self.collect_pending_serial(&onsets, now);
        }
        // Collected in the SAME pass: `schedule_through` advances the
        // scheduler, so asking again for this window would return the next
        // onsets, not these.
        #[cfg(feature = "midi")]
        {
            self.collect_pending_midi(&onsets);
        }
        #[cfg(feature = "osc")]
        self.collect_pending_osc(&onsets, now, cps);

        let mut input_warnings: Vec<String> = Vec::new();
        let (events, notices) =
            rustel_voice::with_diagnostic_policy(self.direct_diagnostic_logging, || {
                onsets
                    .iter()
                    .filter_map(|onset| {
                        match self.live_audio_event(onset, sample_rate, cps, lookup) {
                            Ok(event) => {
                                if let Some(warning) = self.input_channel_warning(&event)
                                    && !input_warnings.contains(&warning)
                                {
                                    input_warnings.push(warning);
                                }
                                Some(event)
                            }
                            Err(error) => {
                                let message = error.to_string();
                                if refusals.last() != Some(&message) {
                                    refusals.push(message);
                                }
                                None
                            }
                        }
                    })
                    .collect()
            });
        self.report_voice_notices(notices);
        // Said once per score text, like a plugin line: the set plays on,
        // and a refusal that stays true is not news each window.
        for message in refusals {
            if self
                .refusals_reported
                .insert(("voice-refused", message.clone()))
            {
                self.report_diagnostic(
                    "voice-refused",
                    message.clone(),
                    serde_json::json!({ "voice_refused": { "message": message } }),
                );
            }
        }
        for message in input_warnings {
            if self.input_warnings_reported.insert(message.clone()) {
                self.report_diagnostic(
                    "audio-input",
                    message.clone(),
                    serde_json::json!({ "audio_input": { "message": message } }),
                );
            }
        }
        Ok(events)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(test)]
    mod offline_window_tests {
        //! The offline window every bounce door uses (`play_window`): what it
        //! reports about a pattern that threw, and which score owns the instant two
        //! of a session bounce's windows share.

        use super::*;
        use crate::session::test_support::{quiet_session, warnings};

        /// A throw raised only by the window's LAST tick has no next iteration to
        /// take it, so only the take after the loop reports it. Without that take
        /// the bounce is silent where the score threw and exits zero.
        #[test]
        fn a_throw_raised_only_by_the_last_offline_tick_is_still_reported() {
            const WINDOW_SECS: f64 = 1.0;
            // The furthest this window's ticks query: only the last query reaches it.
            let mut probe = quiet_session(
                "globalThis.__furthest = 0;
         new Pattern(state => {
           globalThis.__furthest = Math.max(globalThis.__furthest, Number(state.span.end));
           return [];
         })",
            );
            let report = probe.play(WINDOW_SECS).expect("probe window");
            assert_eq!(report.query_threw, None);
            let furthest = probe.js.get_number("__furthest").expect("probe ran");
            assert!(furthest > 0.0, "the probe was never queried");

            let mut session = quiet_session(&format!(
                "new Pattern(state => {{
           if (Number(state.span.end) >= {furthest}) throw new Error('last tick');
           return [];
         }})"
            ));
            let report = session.play(WINDOW_SECS).expect("window");
            assert!(
                report
                    .query_threw
                    .as_deref()
                    .is_some_and(|message| message.contains("last tick")),
                "the last tick's throw was lost: {:?}",
                report.query_threw
            );
            let warnings = warnings(&mut session);
            assert_eq!(warnings.len(), 1, "{warnings:?}");
            assert!(warnings[0].contains("last tick"), "{warnings:?}");
        }

        /// Bounces `saves` to `until` seconds at cps 0.5 anchored at zero, where
        /// every score below holds one hap per cycle: on 0 s, 2 s and 4 s.
        fn bounce(saves: &[(f64, &str)], until: f64) -> RenderReport {
            let directory =
                tempfile::tempdir_in(env!("CARGO_MANIFEST_DIR")).expect("render directory");
            let mut session = Session::new().expect("session");
            session.set_direct_diagnostic_logging(false);
            let saves: Vec<(f64, String)> = saves
                .iter()
                .map(|(at, source)| (*at, (*source).to_owned()))
                .collect();
            let report = session
                .render_session(
                    &saves,
                    0.0,
                    Some(until),
                    &directory.path().join("bounce.wav"),
                    false,
                )
                .expect("session bounce");
            assert!(report.query_threw.is_none(), "{report:?}");
            report
        }

        const LOW: &str = r#"$: note("c3").s("sawtooth").gain(0.25)"#;
        const HIGH: &str = r#"$: note("c5").s("sawtooth").gain(0.25)"#;
        const BROKEN: &str = "note(";

        /// An onset on the instant of a save that installs bounces once, whether the
        /// save keeps or changes the score, and whichever order a failed save at the
        /// same instant comes in.
        #[test]
        fn an_onset_on_an_installing_saves_instant_bounces_once() {
            for (label, saves) in [
                ("unchanged", vec![(0.0, LOW), (2.0, LOW)]),
                ("changed", vec![(0.0, LOW), (2.0, HIGH)]),
                (
                    "failed then installed",
                    vec![(0.0, LOW), (2.0, BROKEN), (2.0, HIGH)],
                ),
                (
                    "installed then failed",
                    vec![(0.0, LOW), (2.0, HIGH), (2.0, BROKEN)],
                ),
            ] {
                let report = bounce(&saves, 4.0);
                assert_eq!(report.onset_count, 3, "{label}: {report:?}");
            }
        }

        /// A save that fails to evaluate leaves the outgoing score's onset on its
        /// instant in the bounce.
        #[test]
        fn a_failed_save_keeps_the_outgoing_onset_on_its_instant() {
            let report = bounce(&[(0.0, LOW), (2.0, BROKEN)], 4.0);
            assert!(report.failed_save.is_some(), "{report:?}");
            assert_eq!(report.onset_count, 3, "{report:?}");
        }

        /// A save on or past the set's end leaves the onset on the closing instant in
        /// the bounce.
        #[test]
        fn a_save_on_or_past_the_end_keeps_the_closing_onset() {
            for at in [4.0, 5.0] {
                let report = bounce(&[(0.0, LOW), (at, HIGH)], 4.0);
                assert_eq!(report.onset_count, 3, "save at {at}: {report:?}");
            }
        }

        /// A filter predicate that throws fails open: the hap it tested keeps
        /// playing, so the bounce has every hit. The failure is one diagnostic and
        /// does not set `query_threw`, which fails the exit status of a bounce
        /// that is silent where the score is not.
        #[test]
        fn an_offline_bounce_through_a_throwing_filter_is_complete_not_threw() {
            let mut session = quiet_session(r#"$: s("bd*4").filterValues(x => log(x))"#);
            let seconds = 2.0 / session.cps();
            let report = session
                .play_window(0.0, seconds, QUERY_JS_CPU_BUDGET, false)
                .expect("offline window");
            assert_eq!(
                report.query_threw, None,
                "a filter that failed open was reported as a silent bounce"
            );
            assert!(
                report.onsets.len() >= 8,
                "the throwing filter silenced the bounce: {} onsets",
                report.onsets.len()
            );
            let warnings = warnings(&mut session);
            assert_eq!(warnings.len(), 1, "{warnings:?}");
            assert!(warnings[0].contains("log is not defined"), "{warnings:?}");
        }
    }

    #[cfg(test)]
    mod playback_reanchor_tests {
        use super::*;
        use rustel_core::{Value, fastcat, pure};

        fn native_session() -> Session {
            let mut session = Session::with_config(SessionConfig {
                cps: 1.0,
                horizon: 0.25,
                sample_rate: 48_000,
                channels: 2,
                ..Default::default()
            })
            .expect("session");
            let notes = [60.0, 72.0].into_iter().map(|note| {
                pure(Value::object(vec![
                    ("s".into(), Value::Str("sine".into())),
                    ("note".into(), Value::F64(note)),
                    ("gain".into(), Value::F64(0.25)),
                ]))
            });
            session
                .set_pattern(fastcat(notes.collect()))
                .expect("native pattern");
            assert!(!session.active_needs_host());
            session
        }

        #[test]
        fn repeated_play_restarts_at_zero_without_reusing_onset_ids() {
            let mut session = native_session();
            let generation = session.generation();
            let fresh = native_session().play(0.625).expect("fresh play");
            let first = session.play(0.625).expect("first play");
            assert!(
                session.scheduler.scheduled_to_cycle().fract() > 0.0,
                "the first pass must leave a fractional query cursor"
            );
            let repeated = session.play(0.625).expect("repeated play");
            let semantics = |report: &PlayReport| {
                report
                    .onsets
                    .iter()
                    .map(|onset| {
                        (
                            onset.whole_begin.clone(),
                            onset.target_time,
                            onset.duration_secs,
                            onset.value.clone(),
                            onset.ui_visuals,
                        )
                    })
                    .collect::<Vec<_>>()
            };
            assert_eq!(fresh.onsets.len(), 2);
            assert_eq!(fresh.onsets[0].target_time, 0.0);
            assert_eq!(fresh.onsets[1].target_time, 0.5);
            assert_eq!(semantics(&first), semantics(&fresh));
            assert_eq!(semantics(&repeated), semantics(&fresh));
            assert_eq!(first.generation, generation + 1);
            assert_eq!(repeated.generation, first.generation + 1);
            assert!(repeated.onsets[0].onset_id > first.onsets.last().unwrap().onset_id);
            assert!(
                repeated
                    .onsets
                    .iter()
                    .all(|onset| onset.generation == repeated.generation)
            );
            assert_eq!(session.time_at_cycle(Fraction::ZERO), 0.0);
        }

        #[test]
        fn repeated_pcm_render_restarts_after_an_intervening_play() {
            let expected = native_session().render_pcm(0.125).expect("fresh PCM");
            assert_eq!(expected.len(), 12_000);
            assert!(expected.iter().all(|sample| sample.is_finite()));
            assert!(expected.iter().any(|sample| sample.abs() > 1e-6));
            let mut session = native_session();
            let first = session.render_pcm(0.125).expect("first PCM");
            session.play(0.625).expect("intervening play");
            let repeated = session.render_pcm(0.125).expect("repeated PCM");
            for actual in [&first, &repeated] {
                assert_eq!(actual.len(), expected.len());
                for (index, (actual, expected)) in actual.iter().zip(&expected).enumerate() {
                    assert!(actual.is_finite());
                    assert_eq!(actual.to_bits(), expected.to_bits(), "sample {index}");
                }
            }
        }

        #[test]
        fn non_reanchored_play_window_preserves_the_installed_timeline() {
            let mut session = native_session();
            session.scheduler.rebase_anchor(10.0, 0.25);
            let generation = session.generation();
            let report = session
                .play_window(10.0, 0.3, QUERY_JS_CPU_BUDGET, false)
                .expect("continuous window");
            assert_eq!(report.generation, generation);
            assert_eq!(session.generation(), generation);
            assert_eq!(session.time_at_cycle(Fraction::ZERO), 9.75);
            assert_eq!(report.onsets.len(), 1);
            let onset = &report.onsets[0];
            assert_eq!(onset.whole_begin, Fraction::new(1, 2).show());
            assert_eq!(onset.target_time, 10.25);
            assert_eq!(onset.duration_secs, 0.5);
            assert_eq!(
                onset.value,
                ValueJson::Raw(serde_json::json!({
                    "s": "sine", "note": 72, "gain": 0.25,
                }))
            );
        }
    }

    /// `setGainCurve` shapes every gain, the default 0.8 included, as
    /// superdough does.
    #[test]
    fn the_gain_curve_shapes_what_plays() {
        let peak_of = |score: &str| {
            let mut session = Session::with_config(SessionConfig::default()).expect("session");
            session.evaluate(score).expect("evaluate");
            let pcm = session.render_pcm(1.0).expect("render");
            pcm.iter().fold(0f32, |peak, sample| peak.max(sample.abs()))
        };
        let straight = peak_of("$: s(\"sine\").gain(.5)");
        let squared = peak_of("setGainCurve(x => x * x)\n$: s(\"sine\").gain(.5)");
        assert!(
            (squared / straight - 0.5).abs() < 0.05,
            "gain .5 through x² is half as loud: {straight} -> {squared}"
        );
        let default_straight = peak_of("$: s(\"sine\")");
        let default_squared = peak_of("setGainCurve(x => x * x)\n$: s(\"sine\")");
        assert!(
            (default_squared / default_straight - 0.8).abs() < 0.05,
            "the default 0.8 goes through the curve too: {default_straight} -> {default_squared}"
        );
        let identity = peak_of("setGainCurve(x => x)\n$: s(\"sine\").gain(.5)");
        assert!(
            (identity - straight).abs() < 1e-3,
            "an identity curve changes nothing"
        );
    }

    #[test]
    fn play_restarts_the_transport_so_a_prior_stop_is_not_cancellation() {
        // `play` calls `transport.start()` before scheduling, so a Stop
        // issued beforehand is cleared. A run is cancelled only from the
        // handle, concurrently, while it is in flight.
        let mut session = Session::new().expect("session");
        session.evaluate(r#"s("bd sd")"#).expect("evaluate");
        session.transport().stop();
        assert!(
            !session.play(2.0).expect("play").onsets.is_empty(),
            "`play` no longer restarts the transport; if that is deliberate, \
             stop-before-play is now cancellation and this test should say so"
        );
    }

    #[test]
    fn play_carries_the_legato_alias_as_effective_hap_duration() {
        let mut session = Session::new().expect("session");
        session
            .evaluate(r#"note("c4").legato(0.25)"#)
            .expect("evaluate the public alias");
        let report = session.play(0.1).expect("play");
        let onset = report.onsets.first().expect("one scheduled onset");
        let crate::ValueJson::Raw(serde_json::Value::Object(value)) = &onset.value else {
            panic!("control pattern did not produce an object value");
        };
        assert_eq!(value.get("clip"), Some(&serde_json::json!(0.25)));
        assert!(
            (onset.duration_secs - 0.5).abs() < f64::EPSILON,
            "legato writes clip=0.25, so one cycle at 0.5 cps must gate for 0.5s; got {}",
            onset.duration_secs
        );
    }

    #[test]
    fn stopping_the_transport_mid_run_cancels_it() {
        // A `play` that ignores Stop also returns, so termination proves
        // nothing. Stop discards queued events, so a cancelled run yields
        // fewer onsets. The full run takes seconds; the stop lands at 100 ms.
        let mut session = Session::new().expect("session");
        session.evaluate(r#"s("bd*8")"#).expect("evaluate");
        let full = session.play(86_400.0).expect("play").onsets.len();
        assert!(
            full > 1000,
            "the uncancelled run scheduled only {full} onsets"
        );

        let mut session = Session::new().expect("session");
        session.evaluate(r#"s("bd*8")"#).expect("evaluate");
        let handle = session.transport();
        let stopper = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(100));
            handle.stop();
        });
        let started = std::time::Instant::now();
        let cancelled = session.play(86_400.0).expect("play").onsets.len();
        let elapsed = started.elapsed();
        stopper.join().expect("stopper thread");

        assert!(
            cancelled < full,
            "a mid-run Stop changed nothing: {cancelled} onsets vs {full} \
             uncancelled. Stop must discard queued events, not drain them."
        );
        assert!(
            elapsed < std::time::Duration::from_secs(60),
            "the cancelled run took {elapsed:?}; Stop must end it promptly"
        );
    }

    #[test]
    fn stop_ends_the_computation_not_just_the_output() {
        // Stop must end the work, not only the output. The scheduler
        // suppresses events after Stop either way, so the test compares the
        // loop's iteration count and elapsed time, not only the onset count.
        let mut session = Session::new().expect("session");
        session.evaluate(r#"s("bd*8")"#).expect("evaluate");
        let started = std::time::Instant::now();
        let full = session.play(86_400.0).expect("play").onsets.len();
        let full_elapsed = started.elapsed();
        let full_iterations = session.last_play_iterations;
        assert!(
            full > 1000,
            "the uncancelled run scheduled only {full} onsets"
        );
        assert!(
            full_iterations > 1_000_000,
            "the uncancelled run only iterated {full_iterations} times, so the \
             comparison below has no headroom"
        );

        let mut session = Session::new().expect("session");
        session.evaluate(r#"s("bd*8")"#).expect("evaluate");
        session.transport().stop();
        // `play` restarts the transport, so stop from a handle DURING the run.
        let handle = session.transport();
        let stopper = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(50));
            handle.stop();
        });
        let started = std::time::Instant::now();
        let cancelled = session.play(86_400.0).expect("play").onsets.len();
        let cancelled_elapsed = started.elapsed();
        stopper.join().expect("stopper");

        assert!(
            cancelled < full,
            "Stop delivered as many onsets as an uncancelled run: {cancelled} \
             vs {full}"
        );
        // A loose bound: a run that keeps querying after Stop takes seconds
        // and fails it.
        assert!(
            cancelled_elapsed * 10 < full_elapsed,
            "a stopped run took {cancelled_elapsed:?} against {full_elapsed:?} \
             uncancelled - Stop is not ending the work"
        );
        // The loop must stop iterating, not just stop producing. Timing alone
        // is too sensitive to machine load, so the loop counts itself.
        let cancelled_iterations = session.last_play_iterations;
        assert!(
            cancelled_iterations < full_iterations / 10,
            "a stopped run performed {cancelled_iterations} of the \
             {full_iterations} iterations an uncancelled run does: `play` is not \
             breaking on `TickStatus::Stopped`, it is spinning through the rest \
             of the window doing nothing"
        );
    }

    #[test]
    fn the_public_play_api_validates_its_own_duration() {
        // `play` is public, so an embedder or a test can call it without the
        // CLI's validation. `Session::play(f64::INFINITY)` must be refused.
        let mut session = Session::new().expect("session");
        session.evaluate(r#"s("bd")"#).expect("evaluate");
        for bad in [f64::INFINITY, f64::NAN, -1.0, 1e300, 1e12] {
            let result = session.play(bad);
            assert!(
                result.is_err(),
                "Session::play({bad}) was accepted; the bound must live at the \
                 library boundary, not only in the argument parser"
            );
        }
        // ...and the ordinary case still works.
        assert!(!session.play(2.0).expect("play").onsets.is_empty());
    }

    #[test]
    fn max_polyphony_changes_the_voices_in_offline_pcm() {
        fn render(voices: usize, script_override: bool) -> Vec<f32> {
            let mut session = Session::with_config(SessionConfig {
                max_polyphony: if script_override { 128 } else { voices },
                ..Default::default()
            })
            .expect("session");
            let setter = if script_override {
                format!("setMaxPolyphony({voices});")
            } else {
                String::new()
            };
            session.evaluate(&format!("{setter} stack(note('c3'), note('e3'), note('g3'), note('c4')).s('sine').gain(0.05).attack(0).decay(0).sustain(1).release(0.1)")).expect("score");
            session.render_pcm(0.05).expect("PCM")
        }
        let limited = render(1, false);
        let all = render(4, false);
        assert_eq!(
            all,
            render(4, true),
            "score and host budgets reach the same backend"
        );
        assert!(
            limited
                .iter()
                .zip(&all)
                .any(|(a, b)| (a - b).abs() > 0.0001),
            "limiting polyphony must change actual audio"
        );
    }
}
