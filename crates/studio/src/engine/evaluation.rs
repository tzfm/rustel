use super::*;

impl StudioEngine {
    /// Evaluate the editor buffer. If playback is stopped this also opens a
    /// fresh device and starts at cycle zero; otherwise it stages a continuous
    /// replacement and lets the producer publish the cutover transactionally.
    pub fn evaluate(&mut self, source: &str, mini: bool) -> Result<StudioInstall, RuntimeError> {
        self.evaluate_guarded(source, mini, || false)
    }

    /// Evaluate only while `cancelled` remains false. On a stopped engine the
    /// transport is armed before the guard is rechecked and is never cleared
    /// again inside startup, so a newer UI Stop cannot be lost to a restart
    /// race while the device is opening.
    pub fn evaluate_guarded(
        &mut self,
        source: &str,
        mini: bool,
        cancelled: impl Fn() -> bool,
    ) -> Result<StudioInstall, RuntimeError> {
        // A rewind sets its flag on the session before this call, and the
        // install consumes it. A refused score, a throw or a cancellation
        // installs nothing. A flag that stays set would rewind the next
        // unrelated save, so a failed evaluation clears it below.
        self.sync_polyphony();
        let result = self.evaluate_guarded_inner(source, mini, cancelled);
        self.sync_recovered_session();
        self.sync_polyphony();
        if result.is_ok() {
            // A fresh evaluation may have settled a tempo of its own;
            // the session hears it once, and only if it moved.
            self.publish_link_tempo();
        }
        if result.is_err() {
            self.session.clear_next_from_zero();
        }
        // Taken by the install; one that never ran leaves it for nobody.
        // The document stays for the next install.
        self.next_install.preview = false;
        result
    }

    fn evaluate_guarded_inner(
        &mut self,
        source: &str,
        mini: bool,
        cancelled: impl Fn() -> bool,
    ) -> Result<StudioInstall, RuntimeError> {
        // Evaluating while a tail is still ringing out means "play this
        // now": finish the stop at once and start fresh from cycle zero.
        if self.is_stopping() {
            let _ = self.stop(self.config.stop_timeout);
        }
        if self.live.is_none() {
            self.session.transport().start();
            if cancelled() {
                self.session.transport().stop();
                return Err(RuntimeError::Cancelled);
            }
            return self.evaluate_and_start_armed(source, mini);
        }

        if cancelled() {
            return Err(RuntimeError::Cancelled);
        }
        // The edit comes before the copies a close sent home: the start goes
        // on, and the edit plays as in the async mode.
        #[cfg(feature = "vst")]
        self.end_start_returning();
        if self.start_is_held() {
            return self.replace_held_start(source, mini);
        }
        // An edit played now supersedes a launch still waiting, and one
        // fired but not yet heard.
        self.supersede_launch();
        match self.reload_live(source, mini)? {
            Some(install) => Ok(install),
            None => Ok(self.hold_for_plugins(source, mini)),
        }
    }

    /// Run one setup file on the session heap.
    ///
    /// Setup is not a score and this is not [`Self::evaluate_guarded`]: no
    /// transport is started, no device opened, no graph replaced, no
    /// generation cut, no launch cancelled. What it leaves behind is what
    /// the next score can call.
    ///
    /// `shutdown` cancels it, not the transport's stopped flag. The stopped
    /// flag is set from the moment a stop is requested until the next
    /// evaluate starts the transport again. If the evaluator read it, every
    /// setup applied from a stopped studio would be cancelled, and most
    /// setups are written there. The interface thread sets it as soon as
    /// the key is pressed, so it could also abort a setup mid-turn and
    /// leave the heap partly changed. A Stop concerns the transport, and a
    /// setup does not touch the transport. Shutdown still interrupts a
    /// runaway setup through QuickJS's interrupt handler, and the
    /// evaluation budget bounds it in both cases.
    pub fn evaluate_prebake_guarded(
        &mut self,
        source: &str,
        shutdown: &std::sync::atomic::AtomicBool,
    ) -> Result<(), RuntimeError> {
        self.session.consume_audio_confirmations();
        let now = self
            .live
            .as_ref()
            .map_or(0.0, |live| live.device.clock_seconds());
        let result = self
            .session
            .evaluate_prebake_cancellable_at(source, now, shutdown);
        self.sync_recovered_session();
        result?;
        // A setup is where `samples(...)` and `preload(...)` belong, so
        // start to load what it names now and do not wait for the first
        // score to ask. The setup is not the score: it does not take the
        // score's place here, so the sweep keeps the playing score's
        // sounds. The setup is already applied, so a failed warm-up is
        // reported and does not fail the setup.
        if let Err(error) = self.warm_names(source, SETUP_SAMPLE_WARM_BUDGET) {
            self.sync_recovered_session();
            queue_diagnostic(
                &mut self.pending_diagnostics,
                StudioDiagnostic::runtime(&error, true),
            );
        }
        Ok(())
    }

    /// Replace the sounding score continuously: the reload is staged, the
    /// producer publishes the cutover transactionally. `None` when the
    /// session holds the edit for its plugin copies: see
    /// [`rustel_runtime::Session::reload_live`]. Nothing installs then, and
    /// the install mark stays for the next install.
    pub(super) fn reload_live(
        &mut self,
        source: &str,
        mini: bool,
    ) -> Result<Option<StudioInstall>, RuntimeError> {
        let mark = self.next_install.take();
        self.ensure_input();
        // A reload evaluates the score's own tempo; when an outside clock is
        // followed the next tick puts it straight back, not a fifth of a
        // second later.
        if self.clock_in.is_some() {
            self.clock_followed_at = Instant::now() - midi_clock::FOLLOW_INTERVAL;
        }
        self.shield_evaluation()?;
        #[cfg(feature = "vst")]
        let waits = self.waits_for_sounds();
        let (generation_before, generation_after) = {
            let live = self.live.as_ref().expect("checked live playback");
            self.session
                .set_schedule_lead(live.device.schedule_lead_seconds());
            self.session
                .set_continuity_margin(live.device.continuity_margin_seconds());
            let generation_before = self.session.generation();
            let transport = self.session.transport();
            let clock = || live.device.clock_seconds();
            // In the async mode an edit plays at once, and a note plays
            // with no plugin until its copy is ready.
            #[cfg(feature = "vst")]
            let generation_after = {
                self.session.set_plugin_wait(waits);
                self.session.set_plugin_document(mark.document);
                let reloaded =
                    self.session
                        .reload_live(source, mini, transport.stopped_flag(), clock)?;
                match reloaded {
                    rustel_runtime::LiveReload::Installed(generation) => generation,
                    rustel_runtime::LiveReload::Waiting => {
                        self.next_install = mark;
                        return Ok(None);
                    }
                }
            };
            #[cfg(not(feature = "vst"))]
            let generation_after = self.session.reload_with_clock_cancellable(
                source,
                mini,
                transport.stopped_flag(),
                clock,
            )?;
            (generation_before, generation_after)
        };
        let live = self
            .live
            .as_mut()
            .expect("live playback survived evaluation");
        live.producer
            .arm_replacement(generation_before, generation_after);
        // A real score now owns this transport. It may have reused the
        // silent device an audition opened, but the score's cycle must keep
        // running after the audition finishes.
        live.audition_owned = false;
        self.warm_source(source, LIVE_RELOAD_SAMPLE_WARM_BUDGET)?;
        self.note_install_for_played(generation_after, mark.preview, source);
        self.follow_edit(source, mini);
        // The room has been replaced by `source`, whatever the launch or edit
        // that asked for it: a fired rewind remembered for the grace no
        // longer speaks for what sounds.
        self.recent_rewind = None;
        Ok(Some(StudioInstall {
            generation: generation_after,
            source_revision: source_revision(source),
            pending_cutover: true,
            answered_repeat: false,
            held: false,
        }))
    }

    /// Prepares the plugin copies of a text that plays later: see
    /// [`rustel_runtime::Session::prepare_reload`]. The text plays from
    /// `from_cycle`, and is of `document`: see
    /// [`Self::mark_next_install_document`]. In the wait mode the session
    /// holds the text while a copy is not ready. In the async mode the
    /// builds start, and nothing waits. The shield keeps the score in play
    /// sounding through the evaluation. A text that fails here fails again
    /// when it plays, and says so then.
    ///
    /// The call prepares nothing for a session with no plugins on its
    /// output, or for a text with no `vst` in a session where no evaluation
    /// ran a plugin call: a set with no plugin costs no second evaluation.
    /// Such a session builds the copies of a chain from a setup helper when
    /// the text plays. `Some(true)` when the prepare ran. `None` when a new
    /// score waits for its first window: the call prepares nothing then. A
    /// launch asks again at a later turn of its countdown. A held edit
    /// builds its copies at its landing.
    #[cfg(feature = "vst")]
    pub(super) fn prepare_live(
        &mut self,
        source: &str,
        mini: bool,
        from_cycle: f64,
        document: u64,
    ) -> Option<bool> {
        if mini
            || !self.session.plugins_live()
            || !(source.contains("vst") || self.session.ran_plugin_calls())
        {
            return Some(false);
        }
        // A new score that waits for its first window publishes it before
        // its takeover. An evaluation here holds this thread past it, and
        // the producer rolls that score back.
        let live = self.live.as_ref().expect("checked live playback");
        if live.device.generation() != self.session.generation() {
            return None;
        }
        if self.shield_evaluation().is_err() {
            return Some(false);
        }
        self.session.set_plugin_wait(self.waits_for_sounds());
        self.session.set_plugin_document(document);
        let live = self.live.as_ref().expect("checked live playback");
        let transport = self.session.transport();
        let _ =
            self.session
                .prepare_reload(source, mini, from_cycle, transport.stopped_flag(), || {
                    live.device.clock_seconds()
                });
        Some(true)
    }

    /// Prepares the plugin copies of the armed launch, in each mode: see
    /// [`Self::prepare_live`]. The arm calls it, and each turn of the
    /// countdown before the fire time, a head-room before the line: a launch
    /// armed while a new score waits for its first window prepares at a
    /// later turn. Past the fire time the launch does not prepare: the fire
    /// evaluates the text. A prepare that ends past the fire time lets the
    /// line pass, as a launch that waits for its sounds does, and the launch
    /// takes the next line. A launch with nothing to prepare keeps its line.
    #[cfg(feature = "vst")]
    pub(super) fn prepare_launch(&mut self) {
        let Some(pending) = self.pending_launch.as_ref() else {
            return;
        };
        if pending.prepared {
            return;
        }
        let (source, mini) = (pending.source.clone(), pending.mini);
        let from_cycle = if pending.rewind {
            0.0
        } else {
            pending.boundary_cycle
        };
        let document = pending.mark.document;
        let Some(ran) = self.prepare_live(&source, mini, from_cycle, document) else {
            return;
        };
        if let Some(pending) = self.pending_launch.as_mut() {
            pending.prepared = true;
        }
        if !ran {
            return;
        }
        let now = self.device_time();
        let Some(pending) = self.pending_launch.as_ref() else {
            return;
        };
        if now >= self.pending_line_time(pending, now) - self.launch_headroom() {
            self.launch_takes_next_line(
                now,
                "plugin prepare ran past the head-room - the launch takes the next line",
            );
        }
    }

    /// Keeps the score in play sounding through an evaluation on this
    /// thread: the producer queues more audio ahead, and the staged messages
    /// of the score in play go out now.
    fn shield_evaluation(&mut self) -> Result<(), RuntimeError> {
        self.session.consume_audio_confirmations();
        self.sync_played_score();
        let mut shield_audio = Vec::new();
        {
            let live = self.live.as_mut().expect("checked live playback");
            let device = &live.device;
            live.producer
                .reap_completed_output(device.completed_intake_frames(), device.sample_rate());
            let mut played = PlayedSink {
                by_score: &mut self.played_by_score,
                by_text: &mut self.played_by_text,
                samples: &self.retained_samples,
                library: self.session.sample_library().cloned(),
                use_until: &mut self.sample_use_until,
                sample_rate: device.sample_rate(),
                from: self.played_from_generation,
                text_from: self.text_from_generation,
            };
            let mut reverbs = LiveReverbBatch::default();
            let shield = live.producer.shield_reload_with_clock_and_preparation(
                &mut self.session,
                || device.clock_seconds(),
                device.sample_rate(),
                |events| {
                    device
                        .prepare_kabel_events(events)
                        .map_err(RuntimeError::ResourceLimit)
                },
                |events| LiveReverbBatch::install_inserts(device, events),
                |event| {
                    reverbs.observe_prepared(device, &event);
                    let pushed = device.push(event.clone());
                    if pushed {
                        played.note(&event, device.render_frontier_frames());
                        shield_audio.push(UiAcceptedOnset {
                            generation: event.generation,
                            onset_id: event.onset_id,
                            frequency_hz: Some(event.freq_hz),
                            gain: Some(event.gain),
                        });
                    }
                    pushed
                },
            );
            let preparation_started = Instant::now();
            reverbs.flush(device);
            self.session
                .record_live_asset_preparation(preparation_started.elapsed());
            shield?;
        }
        self.pending_accepted_audio.extend(shield_audio);
        self.session.consume_audio_confirmations();
        // The replacement's collect would replace what the shield's pass
        // staged before the tick drains: the [`ShieldHold`] takes what must
        // wait, and MIDI short of an armed line goes to the bridge now.
        #[cfg(any(feature = "osc", feature = "serial"))]
        self.shield_hold.take_staged(&mut self.session);
        {
            let (device_now, armed_line, sample_rate) = {
                let device = &self.live.as_ref().expect("checked live playback").device;
                (
                    device.clock_seconds(),
                    device.armed_line_frame(),
                    device.sample_rate(),
                )
            };
            let mut midi = self.session.take_pending_midi();
            self.shield_hold
                .hold_midi_past(&mut midi, armed_line, sample_rate);
            self.dispatch_midi(midi, device_now);
        }
        Ok(())
    }

    /// Open a fresh device and evaluate once. The first producer turn finishes
    /// anchoring cycle zero after input and sample preparation.
    pub fn evaluate_and_start(
        &mut self,
        source: &str,
        mini: bool,
    ) -> Result<StudioInstall, RuntimeError> {
        if self.live.is_some() {
            return self.evaluate(source, mini);
        }

        self.session.transport().start();
        self.evaluate_and_start_armed(source, mini)
    }

    fn evaluate_and_start_armed(
        &mut self,
        source: &str,
        mini: bool,
    ) -> Result<StudioInstall, RuntimeError> {
        debug_assert!(self.live.is_none());
        self.sync_polyphony();
        #[cfg(target_os = "linux")]
        self.refresh_idle_default_output();
        // The live device carries the input from here: the monitor's stream
        // goes before the output opens, so the two never hold one microphone.
        self.input_monitor = None;
        if self
            .piano_output
            .as_ref()
            .is_none_or(|device| device.input_name().is_none())
        {
            self.input_opened_for = None;
        }
        self.sync_input_channels();

        let was_piano_output = self.piano_output.is_some();
        // A parked output stayed open through the last stop, so this set
        // adopts its stream instead of opening one.
        let parked = self.piano_output.take();
        let mut device = match parked.map_or_else(|| self.open_output(), Ok) {
            Ok(device) => device,
            Err(error) => {
                self.session.transport().stop();
                return Err(runtime_device_error(error));
            }
        };
        if let Err(error) = self
            .session
            .bind_audio_confirmations(device.confirmations())
        {
            self.session.transport().stop();
            self.restore_failed_start_device(device, was_piano_output);
            return Err(error);
        }
        if let Err(error) = device.arm_callback_tripwire_checked() {
            self.session.transport().stop();
            // A piano output may already carry a real callback violation.
            // Retire it so the next launch opens a fresh reporting window.
            device.stop();
            drop(device);
            return Err(RuntimeError::Audio(error.to_string()));
        }

        let lead_deadline = Instant::now() + DEVICE_LEAD_WAIT;
        while device.report().playback_latency_nanos == 0 && Instant::now() < lead_deadline {
            std::thread::sleep(Duration::from_millis(5));
        }
        self.start_on(device, source, mini, was_piano_output)
    }

    /// Evaluate `source` onto a freshly opened `device` and make it the
    /// live playback. Cycle zero is anchored by the first producer turn,
    /// once the start's first window has loaded in wait mode.
    pub(super) fn start_on(
        &mut self,
        device: LiveScalarDevice,
        source: &str,
        mini: bool,
        was_piano_output: bool,
    ) -> Result<StudioInstall, RuntimeError> {
        let mark = self.next_install.take();
        // Log what opened, once: the host, the device and the buffer
        // size. It is a log note, so it does not take the status line
        // from the install that follows ("playing from the top").
        queue_diagnostic(
            &mut self.pending_diagnostics,
            StudioDiagnostic::note("audio", device.audio_facts().output().describe_one_line()),
        );
        self.session
            .set_schedule_lead(device.schedule_lead_seconds());
        self.session
            .set_continuity_margin(device.continuity_margin_seconds());

        let transport = self.session.transport();
        #[cfg(feature = "vst")]
        self.session.set_plugin_document(mark.document);
        let evaluation = self.session.reload_at_cancellable(
            source,
            mini,
            device.clock_seconds(),
            transport.stopped_flag(),
        );
        let generation = match evaluation {
            Ok(generation) => generation,
            Err(error) => {
                transport.stop();
                self.restore_failed_start_device(device, was_piano_output);
                return Err(error);
            }
        };
        if transport.is_stopped() {
            self.restore_failed_start_device(device, was_piano_output);
            return Err(RuntimeError::Cancelled);
        }

        if let Err(error) = self
            .warm_source(source, STARTUP_SAMPLE_WARM_BUDGET)
            .and_then(|_| self.follow_start(source))
        {
            transport.stop();
            self.restore_failed_start_device(device, was_piano_output);
            return Err(error);
        }
        let producer = match LiveFileProducer::unwatched(self.config.poll_interval) {
            Ok(mut producer) => {
                producer.adopt_recovery_epoch(&self.session);
                producer
            }
            Err(error) => {
                transport.stop();
                self.restore_failed_start_device(device, was_piano_output);
                return Err(error);
            }
        };
        // Provisional UI mapping. Input opening, sample transfer and the
        // worker's first Hydra turn still happen before audio can be queued;
        // the first producer turn refreshes this anchor after that work.
        let anchor = device.clock_seconds()
            + device.schedule_lead_seconds()
            + self.config.start_preroll.as_secs_f64();
        self.session.restart_transport_at(anchor);
        // A restart has no outgoing audible generation. Discard any continuity
        // cursor created while replacing the retained stopped graph.
        let _ = self.session.take_requery_takeover();
        device.set_generation(generation, 0, TakeoverCut::None);
        device.set_analysis_enabled(false);
        device.set_visual_analysis_mask(0);
        self.sync_polyphony();
        device.set_max_polyphony(self.session.max_polyphony());
        device.set_master_gain(self.master.gain());
        // Set the limiter here as well as on every turn: a set or a setting
        // can ask for a limiter before the first tick reaches the device.
        device.set_limiter(self.master.limiter());
        device.set_limiter_makeup(self.master.limiter_makeup());

        // A failed install leaves a parked output silent. Resume only once
        // its new generation and producer are ready.
        device.resume();
        self.ui.reset();
        // Each set gets its MIDI troubles told afresh.
        {
            self.midi_trouble_reported = [false; 4];
        }
        // And its serial ones. A port that was not there when the studio
        // opened is remembered as absent for good otherwise, so an adapter
        // plugged in later never gets its frames and never gets a word
        // about why. The outputs themselves stay: an open still in flight
        // from the last set cannot be called back, so this set adopts it
        // rather than asking the driver for the same port a second time.
        #[cfg(feature = "serial")]
        {
            self.serial_outputs.begin_set();
            self.serial_reported = std::collections::BTreeSet::new();
        }
        self.piano_has_score_tail = false;
        self.piano_tail_silent_since = None;
        self.stopped_device_time = None;
        // The output plays now, and gives back each copy its chains do not
        // name.
        #[cfg(feature = "vst")]
        {
            self.closed_plugin_copies = false;
        }
        // The install below starts the performer's score, unless it is a
        // preview's.
        self.awaiting_score = true;
        self.live = Some(StudioPlayback {
            registry: Arc::new(rustel_runtime::capability_registry_for_dispatch(
                device.dispatch(),
            )),
            device,
            producer,
            initial_start_generation: Some(generation),
            pressure_monitor: EnginePressureMonitor::default(),
            started_at: Instant::now(),
            progress_at: Duration::ZERO,
            progress_clock: 0,
            last_recycle_at: None,
            last_step_error: None,
            last_fx_reverb_refusals: 0,
            draining: None,
            audition: None,
            run: None,
            audition_owned: false,
        });

        self.note_install_for_played(generation, mark.preview, source);

        Ok(StudioInstall {
            generation,
            source_revision: source_revision(source),
            pending_cutover: false,
            answered_repeat: false,
            held: false,
        })
    }

    /// What an install means for what the score has sounded. A snippet
    /// previewed under the set counts from the sets as they stood, and the
    /// score put back - any install that is not a preview - returns to
    /// them: see [`Self::played_before_preview`]. A different text starts
    /// [`Self::played_by_text`] afresh, and a transport starts on it.
    pub(super) fn note_install_for_played(&mut self, generation: u64, preview: bool, source: &str) {
        if preview {
            if self.played_before_preview.is_none() {
                self.played_before_preview =
                    Some((self.played_by_score.clone(), self.played_by_text.clone()));
            }
            return;
        }
        if let Some((by_score, by_text)) = self.played_before_preview.take() {
            self.played_by_score = by_score;
            self.played_by_text = by_text;
            self.played_from_generation = generation;
            self.protection_generation = self.protection_generation.wrapping_add(1);
            self.idle_swept = false;
        }
        if self.played_text.as_deref() != Some(source) {
            self.played_by_text.clear();
            self.text_from_generation = generation;
            self.played_text = Some(source.to_owned());
            self.played_reset_pending = true;
        }
        if std::mem::take(&mut self.awaiting_score) {
            self.played_by_score.clone_from(&self.played_by_text);
        }
    }

    /// A published cutover can still be awaiting callback confirmation.
    /// Keep its rollback material until that confirmation, and restore the
    /// conservative union if the producer has put the previous text back.
    pub(super) fn sync_played_score(&mut self) {
        if !self.played_reset_pending || self.played_before_preview.is_some() {
            return;
        }
        let Some(source) = self.session.active_source() else {
            return;
        };
        let generation = self.session.generation();
        if self.current_score != source {
            let source = source.to_owned();
            self.restore_played_score(&source);
            if let Err(error) = self.warm_source(&source, LIVE_RELOAD_SAMPLE_WARM_BUDGET) {
                self.sync_recovered_session();
                queue_diagnostic(
                    &mut self.pending_diagnostics,
                    StudioDiagnostic::runtime(&error, true),
                );
                return;
            }
        }
        if self.session.confirmed_audio_generation() == Some(generation) {
            self.played_by_score.clone_from(&self.played_by_text);
            self.played_from_generation =
                self.played_from_generation.max(self.text_from_generation);
            self.played_reset_pending = false;
            self.protection_generation = self.protection_generation.wrapping_add(1);
            self.idle_swept = false;
        }
    }

    pub(super) fn restore_played_score(&mut self, source: &str) {
        self.played_by_text.clone_from(&self.played_by_score);
        if self.played_text.is_some() {
            self.played_text = Some(source.to_owned());
        }
        self.text_from_generation = self.session.generation();
    }

    /// The transport has ended: what its last text sounded stays while a
    /// tab holds that text, and the rest goes, a snippet previewed under
    /// it included.
    pub(super) fn end_played(&mut self) {
        if let Some((by_score, by_text)) = self.played_before_preview.take() {
            self.played_by_score = by_score;
            self.played_by_text = by_text;
        }
        self.played_from_generation = 0;
        self.text_from_generation = 0;
        self.played_reset_pending = false;
        if self.played_text.is_none() {
            self.played_by_text.clear();
        }
        self.played_by_score.clone_from(&self.played_by_text);
    }

    /// Open the output on silence, for a preview or a take while nothing
    /// plays. Silence is no score of the performer's, so what the last one
    /// sounded stays kept for it. It is of no scene either: it installs in
    /// [`rustel_runtime::PREVIEW_DOCUMENT`], and the document mark stays.
    pub(super) fn start_silence(&mut self) -> Result<(), RuntimeError> {
        let document = self.next_install.document;
        self.next_install = InstallMark {
            preview: true,
            document: rustel_runtime::PREVIEW_DOCUMENT,
        };
        let started = self.evaluate_and_start("silence", false).map(drop);
        self.next_install.document = document;
        started
    }

    /// Say whether the score handed over next is a snippet previewed under
    /// the set rather than the performer's own: see
    /// [`Self::played_before_preview`]. The worker says it with every
    /// evaluation, the way it plants a rewind.
    pub fn mark_next_install_preview(&mut self, preview: bool) {
        self.next_install.preview = preview;
    }

    /// Say the document of the score handed over next: the scene id of its
    /// tab, or [`rustel_runtime::PREVIEW_DOCUMENT`] for a snippet heard
    /// alone. The session keeps the plugin copies of each open scene that
    /// does not play: see [`rustel_runtime::Session::set_plugin_document`].
    /// The worker says it with every evaluation. The mark stays until the
    /// next mark, so an install with no mark of its own is of the document
    /// before.
    pub fn mark_next_install_document(&mut self, document: u64) {
        self.next_install.document = document;
    }

    pub(super) fn restore_failed_start_device(
        &mut self,
        device: LiveScalarDevice,
        was_piano_output: bool,
    ) {
        if was_piano_output && device.check_health().is_ok() {
            self.piano_output = Some(device);
        } else {
            device.stop();
        }
    }

    pub(super) fn open_output(
        &self,
    ) -> Result<LiveScalarDevice, rustel_audio::DevicePlaybackError> {
        let mut options = rustel_audio::LiveOutputOptions::default()
            .with_dispatch(self.session.config().dsp_dispatch);
        if let Some(frames) = self.config.output_buffer_frames {
            options =
                options.with_buffer_preference(rustel_audio::AudioBufferPreference::Frames(frames));
        }
        let device = LiveScalarDevice::start_output_with_options(
            self.preferred_output.as_deref(),
            self.session.generation(),
            options,
        )?;
        if let Some(library) = self.session.sample_library() {
            library.set_render_rate(device.sample_rate());
        }
        self.session.attach_instruments(&device);
        Ok(device)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::tests::*;
    mod panic_recovery {
        use super::*;
        use rustel_runtime::SessionPanicPoint;

        const GOOD: &str = "note('c3').s('sine').fast(8)";
        const CANDIDATE: &str = "note('g4').s('triangle').fast(8)";

        fn playing_score() -> StudioEngine {
            let mut engine = StudioEngine::new(StudioConfig {
                output: Some(rustel_audio::SILENT_OUTPUT_NAME.to_owned()),
                ..StudioConfig::default()
            })
            .expect("studio");
            engine.session.evaluate(GOOD).expect("good score");
            play_silently(&mut engine);
            wait_for_audio(&mut engine);
            engine
        }

        fn wait_for_audio(engine: &mut StudioEngine) {
            let deadline = Instant::now() + Duration::from_secs(5);
            loop {
                engine.tick(accepted).expect("engine stays open");
                if engine.session.confirmed_audio_generation() == Some(engine.generation()) {
                    return;
                }
                assert!(
                    Instant::now() < deadline,
                    "the restored score did not sound"
                );
                std::thread::sleep(Duration::from_millis(5));
            }
        }

        #[test]
        fn panicking_candidate_keeps_the_device_and_last_good_score() {
            for point in [
                SessionPanicPoint::Evaluation,
                SessionPanicPoint::ReplacementProbe,
                SessionPanicPoint::Install,
            ] {
                let mut engine = playing_score();
                let stream = engine.device_report().unwrap().stream_id;
                engine.session.inject_panic_for_test(point);
                let error = engine
                    .evaluate(CANDIDATE, false)
                    .expect_err("refused score");
                assert!(matches!(error, RuntimeError::Panic(_)), "{error}");
                assert!(engine.is_playing(), "panic closed the output");
                assert_eq!(engine.active_source(), Some(GOOD));
                assert_eq!(engine.device_report().unwrap().stream_id, stream);
                wait_for_audio(&mut engine);
                assert_eq!(engine.active_source(), Some(GOOD));
                engine.evaluate(CANDIDATE, false).expect("next edit works");
                wait_for_audio(&mut engine);
                assert_eq!(engine.active_source(), Some(CANDIDATE));
            }
        }

        #[test]
        fn scheduler_panic_is_reported_and_playback_recovers_on_the_same_device() {
            let mut engine = playing_score();
            let stream = engine.device_report().unwrap().stream_id;
            engine
                .session
                .inject_panic_for_test(SessionPanicPoint::Query);
            let deadline = Instant::now() + Duration::from_secs(5);
            let mut panic = None;
            while panic.is_none() {
                engine
                    .tick(|update| {
                        if let StudioUpdate::Diagnostic(diagnostic) = update
                            && diagnostic.kind == "panic"
                        {
                            panic = Some(diagnostic);
                        }
                        Ok(())
                    })
                    .expect("query panic is contained");
                assert!(Instant::now() < deadline, "panic was not reported");
                std::thread::sleep(Duration::from_millis(5));
            }
            let panic = panic.unwrap();
            assert_eq!(panic.level, StudioDiagnosticLevel::Error);
            assert!(panic.recoverable);
            assert!(panic.message.contains("panic"));
            assert!(engine.is_playing());
            assert_eq!(engine.active_source(), Some(GOOD));
            assert_eq!(engine.device_report().unwrap().stream_id, stream);
            wait_for_audio(&mut engine);
        }

        #[test]
        fn visual_preview_panic_is_reported_and_the_next_producer_turn_recovers() {
            let mut engine = playing_score();
            engine
                .session
                .inject_panic_for_test(SessionPanicPoint::Query);
            let live = engine.live.as_ref().unwrap();
            engine.ui.after_step(
                &mut engine.session,
                &live.device,
                Duration::from_secs(10),
                true,
                false,
                Vec::new(),
                &mut engine.pending_diagnostics,
                &mut accepted,
            );
            assert!(engine.pending_diagnostics.iter().any(|d| d.kind == "panic"));
            assert_eq!(engine.active_source(), Some(GOOD));
            wait_for_audio(&mut engine);
        }

        #[test]
        fn launch_pad_filter_survives_a_rebuilt_session() {
            let mut engine = playing_score();
            engine.set_launch_pads(vec![(60, 1)]);
            engine
                .session
                .inject_panic_for_test(SessionPanicPoint::Evaluation);
            engine
                .evaluate(CANDIDATE, false)
                .expect_err("injected panic");
            let (_, port) = engine.session.midi_input_bus().intern("keyboard").unwrap();
            port.observe_note_on(rustel_core::midi_in::now_nanos(), 1, 60, 100);
            let mut hits = Vec::new();
            port.keys.select(
                0.0,
                4.0,
                rustel_core::midi_in::now_nanos(),
                Some((1, 8)),
                &mut hits,
            );
            assert!(
                hits.is_empty(),
                "scene launch pads must not become notes after recovery"
            );
        }

        #[test]
        fn loading_window_query_panic_is_a_refused_score() {
            let mut engine = playing_score();
            engine
                .session
                .inject_panic_for_test(SessionPanicPoint::Query);
            let error = engine.follow_start(GOOD).expect_err("loading query panic");
            assert!(matches!(error, RuntimeError::Panic(_)));
            assert_eq!(engine.active_source(), Some(GOOD));
            wait_for_audio(&mut engine);
        }

        #[test]
        fn a_start_whose_first_query_panics_stops_its_transport() {
            let mut engine = StudioEngine::new(StudioConfig {
                output: Some(rustel_audio::SILENT_OUTPUT_NAME.to_owned()),
                ..StudioConfig::default()
            })
            .expect("studio");
            engine.session.transport().start();
            let device =
                LiveScalarDevice::start_silent(48_000, engine.generation()).expect("silent output");
            engine
                .session
                .inject_panic_for_test(SessionPanicPoint::Query);
            let error = engine
                .start_on(device, GOOD, false, false)
                .expect_err("start query panicked");
            assert!(matches!(error, RuntimeError::Panic(_)), "{error}");
            assert!(!engine.is_playing());
            assert!(engine.session.transport().is_stopped());
        }

        #[test]
        fn a_setup_stays_applied_when_the_playing_scores_warm_up_panics() {
            let mut engine = playing_score();
            engine
                .session
                .inject_panic_for_test(SessionPanicPoint::Query);
            engine
                .evaluate_prebake_guarded(
                    "globalThis.setupNote = 48;",
                    &std::sync::atomic::AtomicBool::new(false),
                )
                .expect("the setup applied");
            assert_eq!(engine.session.active_source(), Some(GOOD));
        }
    }

    /// The whole point of a studio prebake: a score calls what the setup
    /// defined, on the same heap, having never been able to before.
    ///
    /// Driven through `StudioEngine` rather than `Session` because the flag
    /// it hands the evaluator is the thing at issue: the transport's
    /// stopped flag is TRUE on an idle studio, and passing that would
    /// cancel every setup a studio ever runs at rest.
    #[test]
    fn a_setup_makes_its_helper_callable_by_the_next_score() {
        let never = std::sync::atomic::AtomicBool::new(false);
        let mut engine = StudioEngine::new(StudioConfig {
            output: Some("silent".to_owned()),
            ..StudioConfig::default()
        })
        .expect("engine");

        // After a stop the transport's stopped flag is true. Passing that
        // flag to the evaluator would cancel every setup that runs on a
        // stopped studio.
        engine.session.transport().stop();
        assert!(engine.session.transport().is_stopped());
        engine
            .evaluate_prebake_guarded("globalThis.riff = () => note(\"c4 e4\")", &never)
            .expect("setup on a stopped studio");
        assert!(
            engine.session.transport().is_stopped(),
            "setup started the transport"
        );

        let generation = engine.session.generation();
        engine.session.transport().start();
        engine.session.evaluate("riff()").expect("score using it");
        assert_ne!(engine.session.generation(), generation);
        let haps = engine
            .session
            .query(
                rustel_fraction::Fraction::ZERO,
                rustel_fraction::Fraction::ONE,
            )
            .expect("query");
        assert!(!haps.is_empty());
        assert!(
            haps.iter()
                .all(|hap| hap.value.show().contains("c4") || hap.value.show().contains("e4")),
            "the score did not get the setup's notes: {haps:?}"
        );

        // A setup that throws leaves the score that is loaded alone.
        let error = engine
            .evaluate_prebake_guarded("throw new Error('bad setup')", &never)
            .expect_err("a throwing setup");
        assert_eq!(error.kind(), "evaluation");
        assert_eq!(engine.session.active_source(), Some("riff()"));

        // And shutdown really does reach a runaway one.
        let closing = std::sync::atomic::AtomicBool::new(true);
        let error = engine
            .evaluate_prebake_guarded("while (true) {}", &closing)
            .expect_err("a setup nobody is waiting for");
        assert_eq!(error.kind(), "cancelled");
    }

    #[test]
    fn rejected_live_evaluation_keeps_the_audible_source_and_generation() {
        let old = "note(\"c4\")";
        let mut session = Session::new().expect("session");
        session.evaluate(old).expect("old score");
        let generation = session.generation();
        let transport = session.transport();

        let error = session
            .reload_at_cancellable("note(", false, 0.1, transport.stopped_flag())
            .expect_err("invalid replacement");
        assert!(!matches!(error, RuntimeError::Cancelled));
        assert_eq!(session.generation(), generation);
        assert_eq!(session.active_source(), Some(old));
    }

    /// `.log()` writes one line for each note that sounds. Each line
    /// arrives at `Info`, the default floor of the log panel, and no
    /// surface paints it as a failure.
    #[test]
    fn log_lines_arrive_as_info_one_per_sounding_note() {
        let mut engine = silent_engine_for_output_selection();
        let mut logs = Vec::new();
        engine
            .evaluate("$: note(\"c\").s(\"sine\").fast(4).log()", false)
            .expect("logged score");
        // The notes sound on the device's own clock, so these lines arrive
        // as wall time passes rather than as turns are taken. A fixed
        // number of turns counts whatever happened to have sounded by the
        // last one, which on a busy machine is sometimes nothing at all.
        // Wait for the two lines the assertions below are about instead,
        // and give up only at a deadline no healthy run comes near.
        let deadline = Instant::now() + Duration::from_secs(10);
        let sounded = |logs: &[(String, StudioDiagnosticLevel, String)]| {
            logs.iter().filter(|(kind, _, _)| kind == "log").count()
        };
        while sounded(&logs) < 2 {
            engine
                .tick_at(Duration::from_millis(12), |update| {
                    if let StudioUpdate::Diagnostic(diagnostic) = update {
                        logs.push((
                            diagnostic.kind.clone(),
                            diagnostic.level,
                            diagnostic.message,
                        ));
                    }
                    Ok(())
                })
                .expect("tick");
            assert!(
                Instant::now() < deadline,
                "the notes never sounded: {logs:?}"
            );
            std::thread::sleep(Duration::from_millis(2));
        }
        let hap_lines = logs
            .iter()
            .filter(|(kind, _, _)| kind == "log")
            .collect::<Vec<_>>();
        assert!(hap_lines.len() >= 2, "the notes sounded: {:?}", logs);
        assert!(
            hap_lines
                .iter()
                .all(|(_, level, _)| *level == StudioDiagnosticLevel::Info)
        );
        assert!(
            hap_lines
                .iter()
                .all(|(_, _, message)| message.starts_with("[hap] "))
        );
    }

    #[test]
    fn a_syntax_error_does_not_count_as_engine_pressure_refusal() {
        let mut engine = silent_engine_for_output_selection();
        engine
            .evaluate("note(\"c4\")", false)
            .expect("audible score");
        engine.tick_at(Duration::ZERO, accepted).expect("prefill");
        let _ = engine.snapshot();

        let error = engine.evaluate("note(", false).expect_err("invalid syntax");
        assert_eq!(error.kind(), "evaluation");

        engine
            .tick_at(Duration::from_millis(2), accepted)
            .expect("continuation");
        let pressure = engine.snapshot().pressure.expect("still playing");
        assert_eq!(
            pressure.producer_refusals, 0,
            "a typo is not the producer refusing work"
        );
        assert_ne!(pressure.cause, rustel_runtime::EnginePressureCause::Refusal);
    }

    #[test]
    fn replacement_cutover_precedes_the_first_new_generation_push() {
        let mut session = Session::new().expect("session");
        session
            .evaluate("note(\"c4\").fast(4)")
            .expect("initial score");
        session.set_schedule_lead(0.05);
        session.set_continuity_margin(0.05);
        let mut producer =
            LiveFileProducer::unwatched(Duration::from_millis(2)).expect("unwatched producer");
        producer
            .step_unwatched_with_clock_and_cutover(
                &mut session,
                || 0.0,
                48_000,
                |_, _, _| panic!("initial generation must not cut over"),
                |_| true,
            )
            .expect("initial prefill");

        let before = session.generation();
        let transport = session.transport();
        let after = session
            .reload_at_cancellable("note(\"e4\").fast(4)", false, 0.1, transport.stopped_flag())
            .expect("replacement");
        producer.arm_replacement(before, after);

        let actions = std::cell::RefCell::new(Vec::<(&'static str, u64)>::new());
        producer
            .step_unwatched_with_clock_and_cutover(
                &mut session,
                || 0.1,
                48_000,
                |generation, _, _| actions.borrow_mut().push(("cutover", generation)),
                |event| {
                    actions.borrow_mut().push(("push", event.generation));
                    true
                },
            )
            .expect("replacement prefill");
        let actions = actions.into_inner();
        let cutover = actions
            .iter()
            .position(|action| *action == ("cutover", after))
            .expect("device cutover");
        let first_new_push = actions
            .iter()
            .position(|action| *action == ("push", after))
            .expect("new generation push");
        assert!(cutover < first_new_push, "actions: {actions:?}");
    }
}
