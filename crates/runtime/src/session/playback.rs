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
                crate::render::scalar_events(
                    &report.onsets,
                    device.sample_rate(),
                    self.config.cps,
                    self.sample_lookup(),
                )
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
        match device.play_with_samples_dispatch_and_polyphony(
            &events,
            duration_secs,
            self.transport.stopped_flag(),
            installs,
            self.config.dsp_dispatch,
            self.max_polyphony(),
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

        // The mp3 door is the same render; only the container differs, so a
        // tape bounced either way is the same audio.
        self.render_under_diagnostic_policy(|session| {
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
            )
            .map_err(RuntimeError::Io)
        })
    }

    /// Run an offline render under this Session's diagnostic policy: with
    /// direct logging on, voice notices and refusals print as they happen;
    /// otherwise both are queued for [`Self::take_diagnostics`].
    fn render_under_diagnostic_policy<T>(
        &mut self,
        render: impl FnOnce(&Self) -> Result<(T, Vec<String>), RuntimeError>,
    ) -> Result<T, RuntimeError> {
        let direct = self.direct_diagnostic_logging;
        let (rendered, notices) = rustel_voice::with_diagnostic_policy(direct, || render(self));
        self.report_voice_notices(notices);
        let (rendered, refused) = rendered?;
        // With direct logging the render already printed each refusal.
        if !direct {
            for message in refused {
                self.report_diagnostic(
                    "voice-refused",
                    message.clone(),
                    serde_json::json!({ "voice_refused": { "message": &message } }),
                );
            }
        }
        Ok(rendered)
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
        let written_bytes = self.render_under_diagnostic_policy(|session| match format {
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
                )
                .map(|(bytes, refused)| (Some(bytes), refused))
                .map_err(render_error)
            }
            RenderFormat::OnsetJson => {
                write_onset_dump(out_path, &play.onsets)?;
                Ok((None, Vec::new()))
            }
        })?;
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
        })
    }
}
