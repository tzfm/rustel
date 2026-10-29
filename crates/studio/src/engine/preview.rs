use super::*;

impl StudioEngine {
    /// Play one sound once, outside the score: `bd:2` from the browser.
    ///
    /// The sound is resolved through the same voice path the scheduler
    /// uses and pushed straight into the device ring on the device's
    /// current generation, so the playing score is untouched. A sample
    /// that is still loading is retried for a few seconds. With nothing
    /// playing, a silent score is started first so there is a device to
    /// preview through.
    pub fn audition(&mut self, sound: &str, gain: f32) -> Result<(), RuntimeError> {
        self.audition_request(AuditionRequest {
            sound: sound.to_owned(),
            notes: Vec::new(),
            gain,
            step_secs: 0.0,
            slot: 0,
            at: None,
        })
    }

    /// Play notes one after another, `step_secs` apart, outside the score:
    /// a scale as it is heard, not as a cluster. At most
    /// [`MAX_AUDITION_RUN_NOTES`] notes sound; the rest are dropped.
    pub fn audition_run(
        &mut self,
        notes: &[f32],
        sound: &str,
        gain: f32,
        step_secs: f64,
    ) -> Result<(), RuntimeError> {
        let notes = notes
            .iter()
            .copied()
            .filter(|note| note.is_finite())
            .take(MAX_AUDITION_RUN_NOTES)
            .collect::<Vec<_>>();
        if notes.is_empty() {
            return Ok(());
        }
        if self.is_stopping() {
            let _ = self.stop(self.config.stop_timeout);
        }
        let opened_for_audition = self.live.is_none();
        if opened_for_audition {
            self.start_silence()?;
            if let Some(live) = self.live.as_mut() {
                live.audition_owned = true;
            }
        }
        // The first note now, the rest as their moments come: a run that
        // handed the device all of its notes at once could not be stopped
        // partway, because a queued event cannot be unqueued.
        let request = AuditionRequest {
            sound: sound.to_owned(),
            notes,
            gain,
            step_secs: step_secs.clamp(0.0, 1.0),
            slot: 0,
            at: None,
        };
        self.stop_audition();
        self.audition_horizon_frame = 0;
        self.note_preview();
        // When the first note sounds, not when it is handed over. Each
        // later note is one whole step after this time, so the run keeps
        // its tempo. A run timed from `now` plays its first note at
        // `now + lead` and its second one step after `now`, so the first
        // gap is one lead too short.
        let due = self
            .live
            .as_ref()
            .map(|live| live.device.clock_seconds() + AUDITION_LEAD_SECS)
            .unwrap_or_default();
        if let Some(live) = self.live.as_mut() {
            live.run = Some(RunningAudition {
                request,
                next: 0,
                due,
            });
        }
        self.advance_run();
        Ok(())
    }

    /// Hand the device every note of the run whose moment has come.
    fn advance_run(&mut self) {
        let Some(mut run) = self.live.as_ref().and_then(|live| live.run.clone()) else {
            return;
        };
        let now = match self.live.as_ref() {
            Some(live) => live.device.clock_seconds(),
            None => return,
        };
        while let Some(at) = run.next_onset(now) {
            let note = run.request.notes[run.next];
            let one = AuditionRequest {
                sound: run.request.sound.clone(),
                notes: vec![note],
                gain: run.request.gain,
                // One note at a time: the run's step decides its length,
                // not a chord's.
                step_secs: run.request.step_secs,
                // Its own slot, so it rings over the note before it the
                // way a hand does not lift off a piano between notes.
                slot: run.next % MAX_AUDITION_RUN_NOTES,
                // The moment this note belongs to, not the moment the tick
                // loop happened to hand it over.
                at: Some(at),
            };
            match self.push_audition(&one) {
                Ok(AuditionPush::Played) => {}
                Ok(AuditionPush::Loading) => break,
                Err(error) => {
                    queue_diagnostic(
                        &mut self.pending_diagnostics,
                        StudioDiagnostic::message("preview", error.to_string()),
                    );
                    if let Some(live) = self.live.as_mut() {
                        live.run = None;
                    }
                    return;
                }
            }
            run.sounded();
        }
        if let Some(live) = self.live.as_mut() {
            live.run = if run.next >= run.request.notes.len() {
                None
            } else {
                Some(run)
            };
        }
    }

    /// Play several notes at once, outside the score: a chord from the
    /// browser, `[60, 64, 67]` on `piano`.
    ///
    /// Each note is resolved as its own voice on `sound`: a bank picks the
    /// sample and the rate for that pitch, and a synth name plays the pitch
    /// directly. All notes are pushed at one target time, so they sound
    /// together. Each note takes its own choke group from the preview's
    /// block, so the notes ring together and the next preview cuts only the
    /// note it replaces. [`Self::stop_audition`] silences all of them. At
    /// most [`MAX_AUDITION_NOTES`] notes sound; the rest are dropped. With
    /// no notes this is [`Self::audition`].
    pub fn audition_notes(
        &mut self,
        notes: &[f32],
        sound: &str,
        gain: f32,
    ) -> Result<(), RuntimeError> {
        let notes = notes
            .iter()
            .copied()
            .filter(|note| note.is_finite())
            .take(MAX_AUDITION_NOTES)
            .collect::<Vec<_>>();
        self.audition_request(AuditionRequest {
            sound: sound.to_owned(),
            notes,
            gain,
            step_secs: 0.0,
            slot: 0,
            at: None,
        })
    }

    pub fn set_piano_settings(&mut self, sound: crate::PianoSound, volume: u16) {
        self.piano_sound = sound;
        self.piano_volume = volume.min(crate::piano::MAX_PIANO_VOLUME);
    }

    /// Open keyboard audio when F12 opens, without loading samples or touching transport.
    pub fn prepare_piano(&mut self) -> Result<(), RuntimeError> {
        #[cfg(feature = "vst")]
        {
            self.piano_mode = true;
        }
        self.open_piano_output()?;
        self.piano_close_when_idle = false;
        Ok(())
    }

    /// Piano mode closes. The plugin sweep decides again, and closes a
    /// parked output it left open for Piano mode. The app sends the piano
    /// settings on each settings change, with Piano mode open or closed. A
    /// call with Piano mode closed asks for no sweep.
    pub fn close_piano_mode(&mut self) {
        #[cfg(feature = "vst")]
        {
            if self.piano_mode {
                self.plugins_swept = false;
            }
            self.piano_mode = false;
        }
    }

    fn open_piano_output(&mut self) -> Result<(), RuntimeError> {
        #[cfg(target_os = "linux")]
        self.refresh_idle_default_output();
        if self.audio_device().is_none() {
            let mut device = self.open_output().map_err(runtime_device_error)?;
            device
                .arm_callback_tripwire_checked()
                .map_err(|error| RuntimeError::Audio(error.to_string()))?;
            device.set_master_gain(self.master.gain());
            device.set_limiter(self.master.limiter());
            device.set_limiter_makeup(self.master.limiter_makeup());
            device.set_max_polyphony(self.session.max_polyphony());
            self.apply_mix_gains(&device);
            if let Some(library) = self.session.sample_library() {
                requeue_retained_samples(library, &self.retained_samples);
            }
            self.piano_output = Some(device);
        }
        self.audio_device().expect("piano output").resume();
        Ok(())
    }

    /// Press one physical computer-piano key. Repeats do not retrigger it;
    /// the slot, rather than pitch, owns release across octave changes.
    pub fn piano_note_on(&mut self, key: u8, note: u8, velocity: u8) -> Result<(), RuntimeError> {
        let key = usize::from(key);
        if key >= PIANO_KEYS || note > 127 {
            return Ok(());
        }
        if velocity == 0 {
            self.piano_note_off(key as u8);
            return Ok(());
        }
        if self.piano[key].is_some_and(|voice| !voice.releasing) {
            return Ok(());
        }
        self.open_piano_output()?;
        let event = self.piano_event(key, note, velocity)?;
        if !self
            .audio_device()
            .expect("piano output")
            .push_immediate(event)
        {
            return Err(RuntimeError::Message(
                "audio busy; try the piano key again".into(),
            ));
        }
        self.piano_close_when_idle = false;
        self.piano[key] = Some(PianoVoice {
            releasing: false,
            release_at: None,
        });
        self.note_preview();
        Ok(())
    }

    fn piano_event(&self, key: usize, note: u8, velocity: u8) -> Result<AudioEvent, RuntimeError> {
        let device = self.audio_device().expect("piano output started");
        // Resolution needs a time base, but callback admission chooses the
        // actual onset. No tempo quantisation or score lookahead is involved.
        // Bypass the session library so even a bank named "triangle" cannot
        // replace the oscillator or delay a keypress with sample loading.
        let mut onset = rustel_voice::resolve_voice(
            &serde_json::json!({"s":self.piano_sound.key(),"note":note}),
            u64::MAX,
            1.0,
            0.0,
            device.sample_rate(),
            self.session.cps(),
        )
        .map_err(RuntimeError::Message)?;
        // This simple oscillator has no modulators or time-dependent FX.
        // Its envelope stays in sustain until its key's choke arrives;
        // this is a held gate, not a periodically retriggered preview.
        onset.duration_secs = f32::INFINITY;
        onset.controls.lfo_end_secs = f32::INFINITY;
        onset.controls.filter_lfo_end_secs = f32::INFINITY;
        onset.controls.envelope = rustel_audio::Envelope {
            attack_secs: 0.005,
            decay_secs: 0.1,
            sustain: 0.7,
            release_secs: 0.02,
        };
        onset.controls.piano = true;
        Ok(AudioEvent {
            onset_id: u64::MAX,
            generation: device.generation(),
            target_frame: 0,
            onset_lead: 0.0,
            freq_hz: onset.freq_hz,
            gain: onset.gain
                * (f32::from(velocity.min(127)) / 127.0)
                * 0.3
                * f32::from(self.piano_volume)
                / 100.0,
            duration_secs: onset.duration_secs,
            // It must not enter the browser waveform or generator ownership.
            ui_visuals: 0,
            controls: onset.controls,
            sample: onset.sample,
            wavetable: onset.wavetable,
            synth: onset.synth,
            cut: Some(piano_cut_group(key)),
        })
    }

    /// The sole output, whether a score is playing or the keyboard is idle.
    pub(super) fn audio_device(&self) -> Option<&LiveScalarDevice> {
        self.live
            .as_ref()
            .map(|live| &live.device)
            .or(self.piano_output.as_ref())
    }

    pub fn piano_note_off(&mut self, key: u8) {
        if let Some(voice) = self
            .piano
            .get_mut(usize::from(key))
            .and_then(Option::as_mut)
        {
            voice.releasing = true;
        }
        self.advance_piano_releases();
    }

    pub fn stop_piano(&mut self) {
        self.piano_close_when_idle = true;
        for voice in self.piano.iter_mut().flatten() {
            voice.releasing = true;
        }
        self.advance_piano_releases();
    }

    /// Retry callback queue backpressure; score generations cannot cancel a
    /// direct note or its release.
    pub(super) fn advance_piano_releases(&mut self) {
        let Some(device) = self
            .live
            .as_ref()
            .map(|live| &live.device)
            .or(self.piano_output.as_ref())
        else {
            self.piano = [None; PIANO_KEYS];
            return;
        };
        let now = device.clock_frames();
        let rate = f64::from(device.sample_rate());
        for (key, slot) in self.piano.iter_mut().enumerate() {
            let Some(voice) = slot.as_mut().filter(|voice| voice.releasing) else {
                continue;
            };
            if let Some((_, frame)) = voice.release_at {
                if now > frame.saturating_add((PIANO_RELEASE_SECS * rate).ceil() as u64) {
                    *slot = None;
                }
                continue;
            }
            // At most 64 commands are admitted, one frame apart; include that
            // bounded queue time before considering the release complete.
            let frame = (device.render_frontier_seconds() * rate).ceil() as u64 + 64;
            if device.push_immediate(AudioEvent {
                onset_id: u64::MAX,
                generation: device.generation(),
                target_frame: 0,
                onset_lead: 0.0,
                freq_hz: 440.0,
                gain: 0.0,
                duration_secs: 0.01,
                ui_visuals: 0,
                controls: rustel_audio::OscillatorControls {
                    choke_only: true,
                    piano: true,
                    ..Default::default()
                },
                sample: None,
                wavetable: None,
                synth: None,
                cut: Some(piano_cut_group(key)),
            }) {
                voice.release_at = Some((0, frame));
            }
        }
    }

    /// Retire only transport ownership. The callback, microphone and capture
    /// handle stay intact so already-sounding audio finishes unchanged.
    pub(super) fn detach_stopped_output_for_piano(&mut self) {
        let Some(live) = self.live.take() else { return };
        // As for a stop: what its last text sounded is kept for its tab.
        self.end_played();
        self.stopped_device_time
            .get_or_insert_with(|| live.device.clock_seconds());
        // As for a stop: the score is no longer kept for being the score.
        self.ask_idle_sweep();
        self.session.transport().stop();
        // As for a stop: the score of a tab that closed while it played
        // ends its chains.
        #[cfg(feature = "vst")]
        {
            self.closed_plugin_copies |= self.session.end_closed_plugin_document();
        }
        self.session.consume_audio_confirmations();
        self.supersede_launch();
        self.recent_rewind = None;
        if let Some(out) = self.clock_out.as_mut() {
            out.advance(false, 0.0, 1.0, Instant::now());
        }
        self.external_outputs().forget();
        hush(&live.device);
        self.piano_output = Some(live.device);
        self.piano_has_score_tail = true;
        self.piano_tail_silent_since = None;
    }

    /// Reopen an idle Linux default output to resolve its current route.
    #[cfg(any(target_os = "linux", test))]
    pub(super) fn refresh_idle_default_output(&mut self) {
        if self.preferred_output.is_none()
            && self.live.is_none()
            && self.piano_close_when_idle
            && self.piano.iter().all(Option::is_none)
            && !self.piano_has_score_tail
        {
            self.close_piano_output();
        }
    }

    /// Close the parked output on failure, a route or buffer change, shutdown,
    /// an idle sweep outside Piano mode that unloads a plugin the output
    /// holds a copy of, or a closed tab whose copies it holds.
    pub(super) fn close_piano_output(&mut self) {
        let Some(device) = self.piano_output.take() else {
            return;
        };
        // Each copy the output holds goes home.
        #[cfg(feature = "vst")]
        {
            self.closed_plugin_copies = false;
        }
        device.stop_and_wait(self.config.stop_timeout);
        drop(device);
        self.piano = [None; PIANO_KEYS];
        self.piano_has_score_tail = false;
        self.piano_tail_silent_since = None;
        if let Some(library) = self.session.sample_library() {
            requeue_retained_samples(library, &self.retained_samples);
        }
        self.release_every_retiring_sample();
    }

    pub(super) fn service_idle_piano(&mut self) {
        self.install_ready_samples();
        self.advance_piano_releases();
        if let Some(device) = self.piano_output.as_ref() {
            device.set_master_gain(self.master.gain());
            device.set_limiter(self.master.limiter());
            device.set_limiter_makeup(self.master.limiter_makeup());
            self.apply_mix_gains(device);
            let levels = device.take_levels();
            self.master.publish(levels);
            let tail_done = if self.piano_has_score_tail {
                let (sources_active, peak) = device.take_score_activity();
                if !sources_active && peak <= GRACEFUL_STOP_SILENCE_PEAK {
                    self.piano_tail_silent_since
                        .get_or_insert_with(Instant::now)
                        .elapsed()
                        >= GRACEFUL_STOP_SILENCE_HOLD
                } else {
                    self.piano_tail_silent_since = None;
                    false
                }
            } else {
                false
            };
            // The parked output stays open for the next start: closing a
            // stream and opening one again make their own click on some
            // hosts, even from silence. Only a failed stream is retired.
            if device.output_failed() {
                self.close_piano_output();
            } else if tail_done {
                self.close_input();
                self.piano_has_score_tail = false;
                self.piano_tail_silent_since = None;
            }
        }
    }

    /// The one path both previews take: start something to preview
    /// through if nothing is playing, push, and retry a sound that is
    /// still downloading for a few seconds.
    fn audition_request(&mut self, request: AuditionRequest) -> Result<(), RuntimeError> {
        if self.is_stopping() {
            let _ = self.stop(self.config.stop_timeout);
        }
        let opened_for_audition = self.live.is_none();
        if opened_for_audition {
            self.start_silence()?;
            if let Some(live) = self.live.as_mut() {
                live.audition_owned = true;
            }
        }
        // A new preview chokes the last one. Its own end, rather than the
        // old sound's longer tail, decides when an audition-only transport
        // can be retired.
        self.audition_horizon_frame = 0;
        self.note_preview();
        match self.push_audition(&request)? {
            AuditionPush::Played => {
                if let Some(live) = self.live.as_mut() {
                    live.audition = None;
                }
                Ok(())
            }
            AuditionPush::Loading => {
                if let Some(live) = self.live.as_mut() {
                    live.audition = Some(PendingAudition {
                        request,
                        deadline: Instant::now() + AUDITION_LOAD_TIMEOUT,
                    });
                }
                Ok(())
            }
        }
    }

    /// Silence whatever the browser is previewing, leaving the score alone:
    /// a zero-gain member of the audition choke group, which chokes the
    /// sounding preview exactly as the next preview would and is itself
    /// inaudible. Nothing to do when nothing is live.
    pub fn stop_audition(&mut self) {
        if let Some(live) = self.live.as_mut() {
            live.audition = None;
            // A run's remaining notes are the engine's, not the device's:
            // dropping it is what makes a scale stoppable partway.
            live.run = None;
        }
        let Some(live) = self.live.as_ref() else {
            return;
        };
        let target_time = live.device.clock_seconds() + AUDITION_LEAD_SECS;
        let target_frame = frame_at(target_time, live.device.sample_rate());
        // An explicit stop makes the choke itself the preview horizon. The
        // next engine turn may then retire a transport that exists only for
        // this audition instead of waiting out the sound's original length.
        self.audition_horizon_frame = target_frame;
        // Every note of the widest preview has its own group, so silence
        // takes one choke each: a chord must not keep ringing because
        // only its first note was told to stop.
        for slot in 0..=MAX_AUDITION_RUN_NOTES {
            let _ = live.device.push(AudioEvent {
                onset_id: u64::MAX,
                generation: live.device.generation(),
                target_frame,
                onset_lead: 0.0,
                freq_hz: 440.0,
                gain: 0.0,
                duration_secs: 0.01,
                ui_visuals: AUDITION_VISUALS,
                controls: Default::default(),
                sample: None,
                wavetable: None,
                synth: None,
                cut: Some(audition_cut_group(slot)),
            });
        }
    }

    fn push_audition(&mut self, request: &AuditionRequest) -> Result<AuditionPush, RuntimeError> {
        let Some(library) = self.session.sample_library().cloned() else {
            return Err(RuntimeError::Message(
                "no sample library to preview from".into(),
            ));
        };
        let Some(live) = self.live.as_ref() else {
            return Err(RuntimeError::Message("nothing is playing".into()));
        };
        let (name, index) = match request.sound.split_once(':') {
            Some((name, index)) => (name, index.parse::<f64>().unwrap_or(0.0)),
            None => (request.sound.as_str(), 0.0),
        };
        // A chord is one value per note on the same sound; a sample
        // preview is the same thing with no note to carry.
        let values: Vec<serde_json::Value> = if request.notes.is_empty() {
            vec![serde_json::json!({ "s": name, "n": index })]
        } else {
            request
                .notes
                .iter()
                .map(|note| serde_json::json!({ "s": name, "n": index, "note": note }))
                .collect()
        };
        let gate = if request.step_secs > 0.0 {
            // A note of a run rings well past the next one: a scale on a
            // piano is a hand rolling through it, not a row of clicks.
            (request.step_secs * 5.0).clamp(0.45, 0.9)
        } else if request.notes.is_empty() {
            AUDITION_GATE_SECS
        } else {
            AUDITION_CHORD_GATE_SECS
        };
        // Eight voices at full gain is eight times the level of one. Each
        // note takes its share of the room the way a mix does, so a chord
        // sits beside a one-shot rather than clipping over it. One voice
        // divides by one and is untouched. A run sounds one note at a
        // time, so it keeps its full level.
        let spread = if request.step_secs > 0.0 {
            1.0
        } else {
            1.0 / (values.len() as f32).sqrt()
        };
        // One target time for every voice makes a chord; a step between
        // them makes a run. A request that names its own moment lands on
        // it, never earlier than the device can take it.
        let earliest = live.device.clock_seconds() + AUDITION_LEAD_SECS;
        let first_time = request.at.map_or(earliest, |at| at.max(earliest));
        let mut events = Vec::with_capacity(values.len());
        for (step, value) in values.iter().enumerate() {
            let target_time = first_time + step as f64 * request.step_secs;
            let onset = match rustel_voice::resolve_voice_with_samples(
                value,
                u64::MAX,
                gate,
                target_time,
                live.device.sample_rate(),
                self.session.cps(),
                library.as_ref(),
            ) {
                Ok(onset) => onset,
                Err(message) if message.contains("still loading") => {
                    return Ok(AuditionPush::Loading);
                }
                Err(message) => return Err(RuntimeError::Message(message)),
            };
            // The device only plays samples it has been handed; one that just
            // finished downloading arrives through `install_ready_samples` on
            // a later tick. A chord waits for all of its notes rather than
            // sounding half of itself.
            if let Some(sample) = &onset.sample
                && !self.retained_samples.contains_key(&sample.sample)
            {
                return Ok(AuditionPush::Loading);
            }
            if let Some(sample) = &onset.sample {
                self.sample_last_used.insert(sample.sample, Instant::now());
            }
            events.push(AudioEvent {
                onset_id: u64::MAX,
                generation: live.device.generation(),
                target_frame: onset.onset_frame,
                onset_lead: onset.onset_lead,
                freq_hz: onset.freq_hz,
                // The preview's own volume, over the onset's: a browser next to
                // a playing set needs headroom past unity to be heard.
                gain: onset.gain * request.gain * spread,
                duration_secs: onset.duration_secs,
                ui_visuals: AUDITION_VISUALS,
                controls: onset.controls,
                sample: onset.sample,
                wavetable: onset.wavetable,
                synth: onset.synth,
                // One preview at a time: a new audition cuts the preview that
                // still rings, so auditions down a bank do not pile up. Each
                // note takes a choke group of its own, so the notes of a chord
                // ring together and the note of the next preview cuts the note
                // in the same position.
                cut: Some(audition_cut_group(request.slot + step)),
            });
        }
        let sample_rate = u64::from(live.device.sample_rate());
        self.audition_horizon_frame = self.audition_horizon_frame.max(
            events
                .iter()
                .map(|event| {
                    let duration_frames =
                        (event.duration_secs.max(0.0) * sample_rate as f32).ceil() as u64;
                    event.target_frame.saturating_add(duration_frames)
                })
                .max()
                .unwrap_or(0),
        );
        let mut reverbs = LiveReverbBatch::default();
        for event in &events {
            reverbs
                .observe(&live.device, event)
                .map_err(RuntimeError::ResourceLimit)?;
        }
        reverbs.flush(&live.device);
        // A three-note chord after a seven-note one leaves four groups
        // holding voices nobody cut: hush the tail of the block before the
        // new preview lands. A run's notes arrive one push at a time and
        // must not hush each other, so only a chord clears the tail.
        let tail = if request.step_secs > 0.0 {
            MAX_AUDITION_RUN_NOTES + 1
        } else {
            values.len()
        };
        for slot in tail..=MAX_AUDITION_RUN_NOTES {
            let _ = live.device.push(AudioEvent {
                onset_id: u64::MAX,
                generation: live.device.generation(),
                target_frame: frame_at(first_time, live.device.sample_rate()),
                onset_lead: 0.0,
                freq_hz: 440.0,
                gain: 0.0,
                duration_secs: 0.01,
                ui_visuals: AUDITION_VISUALS,
                controls: Default::default(),
                sample: None,
                wavetable: None,
                synth: None,
                cut: Some(audition_cut_group(slot)),
            });
        }
        for event in events {
            if !live.device.push(event.clone()) {
                return Err(RuntimeError::Message(
                    "the device is busy; try the preview again".into(),
                ));
            }
            note_sample_use(
                &event,
                &self.retained_samples,
                self.session.sample_library().map(Arc::as_ref),
                &mut self.sample_use_until,
                live.device.sample_rate(),
                live.device.render_frontier_frames(),
                live.device.restart_lead(),
            );
        }
        Ok(AuditionPush::Played)
    }

    /// Each turn's preview upkeep: keyboard releases, a preview whose
    /// sample was loading, and the notes of a run whose moments have come.
    pub(super) fn advance_previews(&mut self) {
        self.advance_piano_releases();
        self.advance_audition();
        self.advance_run();
    }

    /// Retry a preview whose sample was loading, once per tick.
    fn advance_audition(&mut self) {
        let Some(pending) = self.live.as_ref().and_then(|live| live.audition.clone()) else {
            return;
        };
        let outcome = self.push_audition(&pending.request);
        let Some(live) = self.live.as_mut() else {
            return;
        };
        match outcome {
            Ok(AuditionPush::Loading) if Instant::now() < pending.deadline => {}
            Ok(AuditionPush::Loading) => {
                live.audition = None;
                queue_diagnostic(
                    &mut self.pending_diagnostics,
                    StudioDiagnostic::message(
                        "preview",
                        format!("{} did not load in time to preview", pending.request.sound),
                    ),
                );
            }
            Ok(AuditionPush::Played) => live.audition = None,
            Err(error) => {
                live.audition = None;
                queue_diagnostic(
                    &mut self.pending_diagnostics,
                    StudioDiagnostic::message("preview", error.to_string()),
                );
            }
        }
    }

    /// Stop the silent score opened solely to host a browser audition once
    /// no pending note remains and the last scheduled voice has ended. A
    /// real score never carries `audition_owned`, so previewing over a set
    /// cannot stop the performance underneath it.
    pub(super) fn retire_finished_audition_transport(&mut self) {
        if self.recording.is_some() || self.closing_recording.is_some() {
            return;
        }
        // A score waiting to land takes this output over when it does.
        if self.load.held_edit.is_some() || self.pending_launch.is_some() {
            return;
        }
        let finished = self.live.as_ref().is_some_and(|live| {
            if !live.audition_owned || live.audition.is_some() || live.run.is_some() {
                return false;
            }
            let now = (live.device.clock_seconds().max(0.0) * f64::from(live.device.sample_rate()))
                .floor() as u64;
            now >= self.audition_horizon_frame
        });
        if finished {
            if self.piano.iter().any(Option::is_some) {
                self.detach_stopped_output_for_piano();
            } else {
                self.request_stop();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::tests::*;

    /// Every note of a run lands exactly one step after the one before it,
    /// whichever tick hands it over.
    #[test]
    fn every_note_of_a_run_lands_one_step_after_the_last() {
        let step = 0.15;
        let notes = vec![60.0, 62.0, 64.0, 65.0, 67.0];
        let count = notes.len();
        let clock = 8.0;
        let mut run = RunningAudition {
            request: AuditionRequest {
                sound: "triangle".into(),
                notes,
                gain: 1.0,
                step_secs: step,
                slot: 0,
                at: None,
            },
            next: 0,
            due: clock + AUDITION_LEAD_SECS,
        };
        // The tick loop, walking the device clock forward two milliseconds
        // at a time and handing over whatever has come due.
        let mut onsets = Vec::new();
        let mut now = clock;
        while onsets.len() < count && now < clock + 5.0 {
            while let Some(at) = run.next_onset(now) {
                onsets.push(at);
                run.sounded();
            }
            now += 0.002;
        }
        assert_eq!(onsets.len(), count, "every note was handed over");
        for (index, onset) in onsets.iter().enumerate() {
            let wanted = clock + AUDITION_LEAD_SECS + index as f64 * step;
            assert!(
                (onset - wanted).abs() < 1e-9,
                "note {index} landed at {onset}, not {wanted}: {onsets:?}"
            );
        }

        // A run that fell behind - its sound was still loading, or a tick
        // ran long - starts again from here rather than firing every note
        // it missed at once.
        let mut run = RunningAudition {
            request: AuditionRequest {
                sound: "triangle".into(),
                notes: vec![60.0, 62.0, 64.0],
                gain: 1.0,
                step_secs: step,
                slot: 0,
                at: None,
            },
            next: 0,
            due: clock + AUDITION_LEAD_SECS,
        };
        let late = clock + 2.0;
        let first = run.next_onset(late).expect("its moment is long past");
        assert!(
            first >= late + AUDITION_LEAD_SECS,
            "an overdue note is still placed where the device can take it: {first}"
        );
        run.sounded();
        assert_eq!(
            run.next_onset(late),
            None,
            "and the note after it waits its step out rather than piling on"
        );
        assert!((run.due - (first + step)).abs() < 1e-9);
    }

    #[test]
    fn piano_keys_overlap_repeat_and_release_independently() {
        let mut engine = silent_engine_with_pattern(rustel_core::silence());
        engine.session.set_sample_library_for_test(Arc::new(
            rustel_runtime::samples::SampleLibrary::empty_without_loading(),
        ));
        engine.set_piano_settings(crate::PianoSound::Triangle, 130);
        let source = engine.active_source().map(str::to_owned);
        engine.piano_note_on(0, 60, 100).unwrap();
        engine.piano_note_on(4, 64, 100).unwrap();
        engine.piano_note_on(0, 72, 127).unwrap(); // held key/octave change cannot retrigger
        wait_piano_audio(&mut engine, 2);
        assert_eq!(engine.piano.iter().flatten().count(), 2);
        engine.piano_note_off(0);
        wait_piano_audio(&mut engine, 1);
        assert!(engine.piano[0].is_none());
        assert!(engine.piano[4].is_some());
        assert_eq!(engine.active_source(), source.as_deref());
        assert!(!engine.session.transport().is_stopped());
        engine.stop_piano();
        wait_piano_audio(&mut engine, 0);
        assert!(engine.piano.iter().all(Option::is_none));
        engine.retire_finished_audition_transport();
        assert!(
            !engine.session.transport().is_stopped(),
            "score keeps its transport"
        );
    }

    #[test]
    fn piano_is_audible_over_a_sampled_score_without_replacing_its_voice() {
        use rustel_core::Value;

        let mut engine = silent_engine_with_pattern(rustel_core::pure(Value::object([
            ("s".into(), Value::Str("piano".into())),
            ("note".into(), Value::F64(36.0)),
            ("gain".into(), Value::F64(0.1)),
        ])));
        let library =
            Arc::new(rustel_runtime::samples::SampleLibrary::with_loading_sample_for_test("piano"));
        library.finish_loading_sample_frames_for_test(480_000);
        engine.session.set_sample_library_for_test(library);
        engine.tick(|_| Ok(())).expect("schedule the score");
        wait_piano_audio(&mut engine, 1);
        let device = engine.audio_device().unwrap();
        settle_callbacks(device, device.callbacks(), 16);
        let score_peak = device.take_levels().peak;
        assert!(score_peak > 0.001, "the sampled score is audible");
        assert!(engine.piano_event(0, 36, 100).unwrap().sample.is_none());

        engine.piano_note_on(0, 36, 100).unwrap();
        wait_piano_audio(&mut engine, 2);
        let device = engine.audio_device().unwrap();
        settle_callbacks(device, device.callbacks(), 16);
        let combined_peak = device.take_levels().peak;
        assert!(
            combined_peak > score_peak * 1.5,
            "keyboard oscillator is audible: {combined_peak} over {score_peak}"
        );

        engine.piano_note_off(0);
        wait_piano_audio(&mut engine, 1);
        let device = engine.audio_device().unwrap();
        device.take_levels();
        settle_callbacks(device, device.callbacks(), 16);
        let remaining_peak = device.take_levels().peak;
        assert!(
            (remaining_peak - score_peak).abs() < 0.0001,
            "key-up preserves the score: {remaining_peak} vs {score_peak}"
        );
        assert!(engine.is_playing());
    }

    #[test]
    fn piano_idle_output_is_audible_without_starting_or_advancing_a_score() {
        let mut engine = silent_engine_with_pattern(rustel_core::silence());
        engine.session.set_sample_library_for_test(Arc::new(
            rustel_runtime::samples::SampleLibrary::empty_without_loading(),
        ));
        engine.set_piano_settings(crate::PianoSound::Triangle, 130);
        engine.stop(Duration::from_millis(500));
        let before = engine.snapshot();
        let source = engine.active_source().map(str::to_owned);
        let stream = engine.device_report().unwrap().stream_id;
        engine.piano_note_on(0, 60, 100).unwrap();
        engine.piano_note_on(4, 64, 100).unwrap();
        wait_piano_audio(&mut engine, 2);
        let device = engine.piano_output.as_ref().expect("output only");
        assert_eq!(device.report().stream_id, stream);
        settle_callbacks(device, device.callbacks(), 16);
        assert!(device.take_levels().peak > 0.0001, "direct PCM is audible");
        engine.idle_turn(|_| Ok(()));
        let during = engine.snapshot();
        assert!(!during.playing);
        assert!(!engine.is_playing());
        assert!(engine.live.is_none(), "no score producer created");
        assert!(engine.session.transport().is_stopped());
        assert_eq!(during.cycle, before.cycle);
        assert_eq!(during.cps, before.cps);
        assert_eq!(during.session_generation, before.session_generation);
        assert_eq!(engine.active_source(), source.as_deref());
        engine.piano_note_off(0);
        wait_piano_audio(&mut engine, 1);
        engine.stop_piano();
        wait_piano_audio(&mut engine, 0);
        engine.idle_turn(|_| Ok(()));
        let parked = engine
            .piano_output
            .as_ref()
            .expect("the parked output stays for the next start");
        settle_callbacks(parked, parked.callbacks(), 16);
        assert_eq!(
            parked.take_levels().peak,
            0.0,
            "the parked output is silent"
        );
        assert_eq!(engine.snapshot().cycle, before.cycle);
    }

    #[test]
    fn piano_outlives_browser_preview_without_keeping_its_transport_running() {
        let mut engine = silent_engine_with_pattern(rustel_core::silence());
        engine.session.set_sample_library_for_test(Arc::new(
            rustel_runtime::samples::SampleLibrary::empty_without_loading(),
        ));
        engine.set_piano_settings(crate::PianoSound::Triangle, 130);
        engine.live.as_mut().unwrap().audition_owned = true;
        engine.piano_note_on(0, 60, 100).unwrap();
        wait_piano_audio(&mut engine, 1);
        engine.retire_finished_audition_transport();
        assert!(!engine.is_playing());
        assert!(engine.session.transport().is_stopped());
        assert!(engine.piano_output.is_some());
        wait_piano_audio(&mut engine, 1);
        engine.stop_piano();
        wait_piano_audio(&mut engine, 0);
    }

    #[test]
    fn piano_during_stop_preserves_tail_recording_and_hard_stop() {
        let directory = tempfile::tempdir().unwrap();
        let mut engine = silent_engine_with_pattern(rustel_core::silence());
        engine.session.set_sample_library_for_test(Arc::new(
            rustel_runtime::samples::SampleLibrary::empty_without_loading(),
        ));
        engine.set_piano_settings(crate::PianoSound::Triangle, 130);
        engine
            .start_recording(directory.path().join("piano-tail.wav"))
            .unwrap();
        let mut score = engine.piano_event(0, 48, 100).unwrap();
        score.controls.piano = false;
        score.duration_secs = 30.0;
        score.cut = None;
        let device = engine.audio_device().unwrap();
        score.target_frame = device.clock_frames();
        assert!(device.push(score));
        wait_piano_audio(&mut engine, 1);
        engine.pump_recording();
        let stream = engine.device_report().unwrap().stream_id;
        engine.request_stop();
        let frozen = engine.snapshot().cycle;
        // Exercise Stop -> keypress in one command batch, before any drain tick.
        engine.piano_note_on(4, 64, 100).unwrap();
        wait_piano_audio(&mut engine, 2);
        assert!(
            engine.is_playing(),
            "a keypress cannot retire the score tail"
        );
        assert!(engine.piano_output.is_none());
        assert!(matches!(
            engine.tick(|_| Ok(())).unwrap(),
            StudioTick::Stopping
        ));
        assert!(engine.is_stopping());
        assert_eq!(engine.device_report().unwrap().stream_id, stream);
        assert_eq!(engine.snapshot().cycle, frozen);
        let before = engine.recording.as_ref().unwrap().frames;
        let device = engine.audio_device().unwrap();
        settle_callbacks(device, device.callbacks(), 16);
        engine.tick(|_| Ok(())).unwrap();
        assert!(
            engine.recording.as_ref().unwrap().frames > before,
            "the original recording tap continues through score and keyboard audio"
        );
        engine.stop_piano();
        wait_piano_audio(&mut engine, 1);
        engine.tick(|_| Ok(())).unwrap();
        assert!(
            engine.live.is_some(),
            "helper exit must not cut the old tail"
        );
        assert_eq!(engine.snapshot().cycle, frozen);
        assert!(engine.master_bus().take_levels().peak > 0.0001);
        let stop = engine
            .stop(Duration::from_millis(500))
            .expect("shared output still needs a stop acknowledgement");
        assert!(stop.acknowledged);
        assert!(stop.report.stop_acknowledged);
        assert_eq!(stop.report.stream_id, stream);
        assert!(
            engine.piano_output.is_some()
                && engine.session.transport().is_stopped()
                && engine.piano.iter().all(Option::is_none),
            "hard stop closes every sound and parks the output"
        );
        let take = engine.stop_recording().expect("joined recording");
        assert!(take.frames > 0);
        assert!(take.error.is_none());
        assert!(take.final_signal.unwrap().sample_count > 0);
    }

    #[test]
    fn piano_cut_sounding_cancels_queued_keys_without_forgetting_their_releases() {
        let mut engine = silent_engine_with_pattern(rustel_core::silence());
        engine.session.set_sample_library_for_test(Arc::new(
            rustel_runtime::samples::SampleLibrary::empty_without_loading(),
        ));
        engine.set_piano_settings(crate::PianoSound::Triangle, 130);
        assert!(
            engine
                .live
                .as_ref()
                .unwrap()
                .device
                .hold_silent_clock_for_test(true)
        );
        engine.piano_note_on(0, 60, 100).unwrap();
        engine.piano_note_on(4, 64, 100).unwrap();
        engine.cut_sounding();
        for key in [0, 4] {
            assert!(
                engine.piano[key].unwrap().releasing,
                "global cut must retain queued key release ownership"
            );
        }
        assert!(
            engine
                .live
                .as_ref()
                .unwrap()
                .device
                .hold_silent_clock_for_test(false)
        );
        wait_piano_audio(&mut engine, 0);
        assert!(engine.piano.iter().all(Option::is_none));
        // A future press is a fresh voice, and still has an effective key-up.
        engine.piano_note_on(0, 72, 100).unwrap();
        wait_piano_audio(&mut engine, 1);
        engine.piano_note_off(0);
        wait_piano_audio(&mut engine, 0);
    }

    #[test]
    fn piano_queued_press_survives_a_score_generation_change() {
        let mut engine = silent_engine_with_pattern(rustel_core::silence());
        engine.session.set_sample_library_for_test(Arc::new(
            rustel_runtime::samples::SampleLibrary::empty_without_loading(),
        ));
        engine.set_piano_settings(crate::PianoSound::Triangle, 130);
        let device = &engine.live.as_ref().unwrap().device;
        assert!(device.hold_silent_clock_for_test(true));
        engine.piano_note_on(0, 60, 100).unwrap();
        let device = &engine.live.as_ref().unwrap().device;
        // The onset remains in the ring when an unrelated score update wins.
        device.set_generation(
            device.generation() + 1,
            device.clock_frames(),
            TakeoverCut::None,
        );
        assert!(device.hold_silent_clock_for_test(false));
        wait_piano_audio(&mut engine, 1);
        engine.stop_piano();
        wait_piano_audio(&mut engine, 0);
    }

    #[test]
    fn piano_fast_repress_lands_after_queued_release_even_across_generation_changes() {
        for replace_generation in [false, true] {
            let mut engine = silent_engine_with_pattern(rustel_core::silence());
            engine.session.set_sample_library_for_test(Arc::new(
                rustel_runtime::samples::SampleLibrary::empty_without_loading(),
            ));
            engine.set_piano_settings(crate::PianoSound::Triangle, 130);
            engine.piano_note_on(0, 60, 100).unwrap();
            wait_piano_audio(&mut engine, 1);
            assert!(
                engine
                    .live
                    .as_ref()
                    .unwrap()
                    .device
                    .hold_silent_clock_for_test(true)
            );
            engine.piano_note_off(0);
            if replace_generation {
                hush(&engine.live.as_ref().unwrap().device);
            }
            engine.piano_note_on(0, 72, 100).unwrap();
            assert!(
                engine
                    .live
                    .as_ref()
                    .unwrap()
                    .device
                    .hold_silent_clock_for_test(false)
            );
            wait_piano_audio(&mut engine, 1);
            // Let the old release and its complete fade pass. The re-pressed
            // voice still sounds and has not inherited a release request.
            let device = &engine.live.as_ref().unwrap().device;
            settle_callbacks(device, device.callbacks(), 16);
            assert_eq!(device.report().realtime_pressure.active_voices, 1);
            assert!(!engine.piano[0].unwrap().releasing);
            engine.stop_piano();
            wait_piano_audio(&mut engine, 0);
        }
    }

    #[test]
    fn piano_sounds_ignore_loading_and_ready_sample_banks() {
        use crate::PianoSound;

        let mut engine = silent_engine_with_pattern(rustel_core::silence());
        for sound in [PianoSound::Sine, PianoSound::Triangle, PianoSound::Square] {
            let library = Arc::new(
                rustel_runtime::samples::SampleLibrary::with_loading_sample_for_test(sound.key()),
            );
            engine
                .session
                .set_sample_library_for_test(Arc::clone(&library));
            engine.set_piano_settings(sound, 130);
            engine.prepare_piano().unwrap();
            let oscillator = engine.piano_event(0, 60, 100).unwrap();
            assert!(oscillator.sample.is_none());
            assert!(oscillator.wavetable.is_none());
            engine.piano_note_on(0, 60, 100).unwrap();
            wait_piano_audio(&mut engine, 1);
            engine.piano_note_off(0);
            wait_piano_audio(&mut engine, 0);

            library.finish_loading_sample_for_test();
            engine.piano_note_on(0, 60, 100).unwrap();
            wait_piano_audio(&mut engine, 1);
            assert!(engine.piano_event(0, 60, 100).unwrap().sample.is_none());
            assert!(
                engine.retained_samples.is_empty(),
                "keys never install sample PCM"
            );
            engine.stop_piano();
            wait_piano_audio(&mut engine, 0);
        }
    }

    #[test]
    fn piano_first_key_sounds_without_a_sample_library_while_the_score_stays_idle() {
        let mut engine = StudioEngine::new(StudioConfig {
            output: Some(rustel_audio::SILENT_OUTPUT_NAME.to_owned()),
            default_samples: false,
            ..StudioConfig::default()
        })
        .expect("studio");
        engine.session.transport().stop();
        assert!(engine.session.sample_library().is_none());
        let before = engine.snapshot();
        // Unit tests do not install the binary's allocation tripwire. Supply
        // the silent output as in the other idle keyboard fixture.
        engine.piano_output = Some(engine.open_output().unwrap());
        engine.prepare_piano().unwrap();
        engine.piano_note_on(0, 60, 100).unwrap();
        wait_piano_audio(&mut engine, 1);
        let device = engine.piano_output.as_ref().unwrap();
        settle_callbacks(device, device.callbacks(), 16);
        assert!(device.take_levels().peak > 0.0001, "first key is audible");
        engine.idle_turn(|_| Ok(()));
        let during = engine.snapshot();
        assert!(!during.playing);
        assert_eq!(during.cycle, before.cycle);
        assert_eq!(during.session_generation, before.session_generation);
        assert!(engine.session.transport().is_stopped());
        assert!(
            engine.session.sample_library().is_none(),
            "no sample loader was started"
        );
        assert!(
            engine.retained_samples.is_empty(),
            "no sample PCM is retained"
        );
        engine.stop_piano();
        wait_piano_audio(&mut engine, 0);
        assert!(
            engine
                .stop(Duration::from_millis(500))
                .unwrap()
                .acknowledged
        );
    }

    #[test]
    fn a_score_tail_finishes_naturally_while_a_keyboard_note_stays_held() {
        let mut engine = silent_engine_with_pattern(rustel_core::silence());
        engine.session.set_sample_library_for_test(Arc::new(
            rustel_runtime::samples::SampleLibrary::empty_without_loading(),
        ));
        engine.set_piano_settings(crate::PianoSound::Triangle, 130);
        let mut score = engine.piano_event(0, 48, 100).unwrap();
        score.controls.piano = false;
        score.duration_secs = 0.6;
        score.cut = None;
        let device = engine.audio_device().unwrap();
        score.target_frame = device.clock_frames();
        assert!(device.push(score));
        wait_piano_audio(&mut engine, 1);
        let stream = engine.device_report().unwrap().stream_id;
        engine.request_stop();
        let frozen = engine.snapshot().cycle;
        engine.tick(|_| Ok(())).unwrap();
        let generation = engine.audio_device().unwrap().generation();
        engine.prepare_piano().unwrap();
        engine.piano_note_on(4, 64, 100).unwrap();
        wait_piano_audio(&mut engine, 2);
        assert!(
            engine.is_stopping(),
            "F12 and keypress do not retire the tail"
        );
        assert_eq!(engine.audio_device().unwrap().generation(), generation);

        let deadline = Instant::now() + Duration::from_secs(3);
        while engine.is_playing() {
            engine.tick(|_| Ok(())).unwrap();
            assert!(
                Instant::now() < deadline,
                "held keyboard note prolonged score stop"
            );
            std::thread::sleep(Duration::from_millis(3));
        }
        assert!(!engine.snapshot().stopping);
        assert_eq!(engine.snapshot().cycle, frozen);
        assert_eq!(engine.device_report().unwrap().stream_id, stream);
        wait_piano_audio(&mut engine, 1);
        let device = engine.audio_device().unwrap();
        device.take_levels();
        settle_callbacks(device, device.callbacks(), 8);
        assert!(
            device.take_levels().peak > 0.001,
            "held keyboard remains audible"
        );
        assert!(!engine.piano[4].unwrap().releasing);
        engine.piano_note_off(4);
        wait_piano_audio(&mut engine, 0);
        assert!(
            engine
                .stop(Duration::from_millis(500))
                .unwrap()
                .acknowledged
        );
    }

    #[test]
    fn piano_selected_synth_is_immediate_held_and_separate_from_preview_groups() {
        let mut engine = silent_engine_with_pattern(rustel_core::silence());
        engine.session.set_sample_library_for_test(Arc::new(
            rustel_runtime::samples::SampleLibrary::empty_without_loading(),
        ));
        engine.set_piano_settings(crate::PianoSound::Triangle, 130);
        let event = engine.piano_event(0, 69, 127).unwrap();
        assert_eq!(event.freq_hz, 440.0);
        assert_eq!(event.duration_secs, f32::INFINITY);
        assert_eq!(event.controls.preview_epoch, 0);
        assert_eq!(event.ui_visuals, 0);
        assert!(event.sample.is_none());
        engine.set_piano_settings(crate::PianoSound::Triangle, 100);
        let original = engine.piano_event(0, 69, 127).unwrap();
        assert!((event.gain - original.gain * 1.3).abs() < 0.00001);
        engine.set_piano_settings(crate::PianoSound::Square, 0);
        assert_eq!(engine.piano_event(0, 69, 127).unwrap().gain, 0.0);
        for slot in 0..=MAX_AUDITION_RUN_NOTES {
            for key in 0..PIANO_KEYS {
                assert_ne!(audition_cut_group(slot), piano_cut_group(key));
            }
        }
    }

    #[test]
    fn a_finished_audition_stops_only_the_transport_it_opened() {
        let mut audition = silent_engine_with_pattern(rustel_core::silence());
        audition.live.as_mut().unwrap().audition_owned = true;
        audition.audition_horizon_frame = 0;
        audition.retire_finished_audition_transport();
        assert!(
            audition.session.transport().is_stopped(),
            "the silent score opened for the preview retires with it"
        );

        let mut set = silent_engine_with_pattern(rustel_core::silence());
        set.audition_horizon_frame = 0;
        set.retire_finished_audition_transport();
        assert!(
            !set.session.transport().is_stopped(),
            "a real set keeps its clock after a preview"
        );
    }

    #[test]
    fn recording_takes_ownership_of_a_preview_transport() {
        let directory = tempfile::tempdir_in(env!("CARGO_MANIFEST_DIR")).unwrap();
        let mut engine = silent_engine_with_pattern(rustel_core::silence());
        engine.live.as_mut().unwrap().audition_owned = true;
        engine.audition_horizon_frame = 0;

        engine
            .start_recording(directory.path().join("preview-take.wav"))
            .expect("start take on preview transport");
        assert!(!engine.live.as_ref().unwrap().audition_owned);
        engine.retire_finished_audition_transport();
        assert!(
            !engine.session.transport().is_stopped(),
            "the expired preview must not stop the active take"
        );
        engine.stop_recording().expect("finished take");
    }
}
