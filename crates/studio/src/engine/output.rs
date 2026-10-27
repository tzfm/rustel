use super::*;

impl StudioEngine {
    /// Name of the output currently in use, or the pending preference when
    /// playback has not started yet.
    pub fn output_name(&self) -> Option<String> {
        self.audio_device()
            .map(|device| device.name().to_owned())
            .or_else(|| self.preferred_output.clone())
    }

    /// Move playback onto a named output.
    ///
    /// While stopped this only records the choice; the next evaluation opens
    /// it. While playing the stream is recycled onto the new device and the
    /// scheduling horizon is re-armed exactly as it is after a stall.
    /// If opening fails after teardown, the previous output is tried once.
    /// The requested error is still returned; reopening only arms score refill
    /// and does not promise uninterrupted audio or preserve old effect tails.
    pub fn set_output_device(&mut self, name: &str) -> Result<(), RuntimeError> {
        let transport = self.session.transport();
        self.set_output_device_with(name, |device, selector, samples| {
            device.recycle_output_to_with_samples(selector, samples, || transport.is_stopped())
        })
    }

    pub(super) fn set_output_device_with(
        &mut self,
        name: &str,
        mut recycle: impl FnMut(
            &mut LiveScalarDevice,
            Option<&str>,
            &[(SampleId, DecodedSample)],
        ) -> Result<(), DevicePlaybackError>,
    ) -> Result<(), RuntimeError> {
        if self.live.is_none() || self.session.transport().is_stopped() {
            if self.live.is_some() {
                let _ = self.stop_output(self.config.stop_timeout, false);
            }
            self.close_piano_output();
            self.preferred_output = Some(name.to_owned());
            return Ok(());
        }
        self.session.consume_audio_confirmations();
        let samples = self.collect_recycle_samples();
        let live = self.live.as_mut().expect("checked playback");
        let previous = live.device.name().to_owned();
        let requested = recycle(&mut live.device, Some(name), &samples);
        self.session.consume_audio_confirmations();
        let requested_error = match requested {
            Ok(()) => None,
            Err(error) if live.device.has_output() => {
                self.requeue_recycle_samples(samples);
                return Err(runtime_device_error(error));
            }
            Err(error) => Some(error),
        };
        if self.session.transport().is_stopped() {
            let _ = self.stop_output(self.config.stop_timeout, false);
            return Err(output_recovery_cancelled(requested_error.as_ref()));
        }
        if let Some(error) = &requested_error {
            // Discovery rejection returned above without touching playback.
            // Teardown lost all voices and queued audio: reopen the exact old
            // output once, then refill through the ordinary recovery path.
            let recovered = recycle(&mut live.device, Some(&previous), &samples).and_then(|()| {
                if live.device.name() != previous {
                    Err(DevicePlaybackError::Unavailable(format!(
                        "reopened output {:?} instead of {previous:?}",
                        live.device.name()
                    )))
                } else {
                    Ok(())
                }
            });
            self.session.consume_audio_confirmations();
            if let Err(recovery) = recovered {
                let message =
                    format!("{error}; could not reopen previous output {previous:?}: {recovery}");
                let _ = self.stop_output(self.config.stop_timeout, false);
                return Err(RuntimeError::Audio(message));
            }
        }
        if self.session.transport().is_stopped() {
            let _ = self.stop_output(self.config.stop_timeout, false);
            return Err(output_recovery_cancelled(requested_error.as_ref()));
        }
        if let Err(error) = self.rearm_recycled_output() {
            let message = match requested_error {
                Some(requested) => {
                    format!("{requested}; previous output could not refill: {error}")
                }
                None => format!("replacement output could not refill: {error}"),
            };
            let _ = self.stop_output(self.config.stop_timeout, false);
            return Err(RuntimeError::Audio(message));
        }
        if self.session.transport().is_stopped() {
            let _ = self.stop_output(self.config.stop_timeout, false);
            return Err(output_recovery_cancelled(requested_error.as_ref()));
        }
        if let Some(error) = requested_error {
            return Err(RuntimeError::Audio(format!(
                "{error}; previous output {previous:?} reopened; score refill pending"
            )));
        }
        self.preferred_output = Some(name.to_owned());
        Ok(())
    }

    /// Test hook: see the field of the same name.
    #[cfg(test)]
    pub(crate) fn set_fail_output_recycles_for_test(&mut self, fail: bool) {
        self.fail_output_recycles_for_test = fail;
    }

    /// Adopt an output buffer size, in frames. `None` returns to the
    /// automatic policy. A playing set recycles its output so the choice is
    /// heard now, exactly as a device switch does; stopped or silent, it is
    /// remembered for the next open.
    pub fn set_output_buffer_frames(&mut self, frames: Option<u32>) -> Result<(), RuntimeError> {
        let preference = frames.map_or(rustel_audio::AudioBufferPreference::Auto, |frames| {
            rustel_audio::AudioBufferPreference::Frames(frames)
        });
        preference
            .validate()
            .map_err(|error| RuntimeError::Audio(error.to_string()))?;
        self.config.output_buffer_frames = frames;
        if self.live.is_some() && self.session.transport().is_stopped() {
            let _ = self.stop_output(self.config.stop_timeout, false);
        }
        self.close_piano_output();
        if self.live.is_none() || self.session.transport().is_stopped() {
            return Ok(());
        }
        #[cfg(test)]
        if self.fail_output_recycles_for_test {
            return self.set_output_device_with_frames(preference, |device, _, _| {
                device.fail_output_replacement_for_test()
            });
        }
        let transport = self.session.transport();
        self.set_output_device_with_frames(preference, |device, selector, samples| {
            device.recycle_output_to_with_samples(selector, samples, || transport.is_stopped())
        })
    }

    /// The device-switch recycle with one difference: the replacement keeps
    /// the current output and adopts the just-validated preference first, so
    /// every open this recycle makes - the requested one and any recovery
    /// reopen of the previous device - carries the new size.
    fn set_output_device_with_frames(
        &mut self,
        preference: rustel_audio::AudioBufferPreference,
        recycle: impl FnMut(
            &mut LiveScalarDevice,
            Option<&str>,
            &[(SampleId, DecodedSample)],
        ) -> Result<(), DevicePlaybackError>,
    ) -> Result<(), RuntimeError> {
        let Some(name) = self.live.as_ref().map(|live| live.device.name().to_owned()) else {
            return Ok(());
        };
        self.live
            .as_mut()
            .expect("checked playback")
            .device
            .set_buffer_preference(preference);
        // Recycling the output by its name is not choosing it: a set that
        // follows the host default keeps following it, so the preference
        // the device-switch path records is put back afterwards.
        let preferred = self.preferred_output.clone();
        let recycled = self.set_output_device_with(&name, recycle);
        if recycled.is_ok() {
            self.preferred_output = preferred;
        }
        recycled
    }

    pub(super) fn sync_polyphony(&mut self) {
        self.session
            .set_default_max_polyphony(self.master.max_polyphony());
        let override_voices = self.session.max_polyphony_override();
        self.master
            .max_polyphony_override
            .store(override_voices.unwrap_or(0), Ordering::Relaxed);
        let effective = override_voices.unwrap_or(self.session.config().max_polyphony);
        self.master
            .effective_max_polyphony
            .store(effective, Ordering::Relaxed);
        if let Some(device) = self.audio_device() {
            device.set_max_polyphony(effective);
        }
    }

    pub(super) fn rearm_recycled_output(&mut self) -> Result<(), RuntimeError> {
        self.piano = [None; PIANO_KEYS];
        self.sync_polyphony();
        // The frame domain the ports' timelines lived in went with the old
        // stream: silence them before the session's generation moves on.
        self.external_outputs().forget();
        let waits = self.waits_for_sounds();
        let live = self.live.as_mut().expect("recycled playback");
        live.device.set_master_gain(self.master.gain());
        live.device.set_limiter(self.master.limiter());
        live.device.set_limiter_makeup(self.master.limiter_makeup());
        for (orbit, gain) in self.orbit_gains.iter().enumerate() {
            live.device.set_orbit_gain(orbit, *gain);
        }
        live.device.set_input_gain(self.input_gain);
        live.progress_clock = live.device.clock_nanos();
        live.progress_at = live.started_at.elapsed();
        live.last_recycle_at = Some(live.progress_at);
        self.session
            .set_schedule_lead(live.device.schedule_lead_seconds());
        self.session
            .set_continuity_margin(live.device.continuity_margin_seconds());
        arm_output_recovery_after_recycle(
            &mut self.session,
            &mut live.producer,
            live.device.clock_seconds(),
            live.device.sample_rate(),
            waits,
        )?;
        // Say what opened, as the first open does: a buffer or device
        // change is exactly when the player wants to read what the host
        // granted. A log note, never the status line.
        let facts = live.device.audio_facts().output().describe_one_line();
        queue_diagnostic(
            &mut self.pending_diagnostics,
            StudioDiagnostic::note("audio", facts),
        );
        // The replacement was seeded before callbacks activated. The old
        // bank and queued events are gone; forgotten ids were excluded from
        // that snapshot, so every retiring id can now return to the library.
        self.release_every_retiring_sample();
        Ok(())
    }

    /// Open the output again after its stream reported an error, keeping
    /// the music: the same reopen a stalled clock gets, asked for by a
    /// different symptom.
    ///
    /// Returns `false` when it is too soon to try again. A device that
    /// fails, is reopened and fails again within the cooldown will not
    /// recover, and a reopen on every turn would load the machine and
    /// hide the failure.
    pub(super) fn reopen_failed_device(
        &mut self,
        observed_at: Duration,
    ) -> Result<bool, RuntimeError> {
        let live = self.live.as_ref().expect("checked playback");
        if live
            .last_recycle_at
            .is_some_and(|last| observed_at.saturating_sub(last) < DEVICE_RECYCLE_COOLDOWN)
        {
            return Ok(false);
        }
        let name = live.device.name().to_owned();
        self.session.consume_audio_confirmations();
        let samples = self.collect_recycle_samples();
        let transport = self.session.transport();
        let live = self.live.as_mut().expect("checked playback");
        let recycled = live
            .device
            .recycle_output_to_with_samples(None, &samples, || transport.is_stopped());
        self.session.consume_audio_confirmations();
        if let Err(error) = recycled {
            self.requeue_recycle_samples(samples);
            return Err(runtime_device_error(error));
        }
        if transport.is_stopped() {
            return Err(RuntimeError::Cancelled);
        }
        self.external_outputs().forget();
        self.piano = [None; PIANO_KEYS];
        let waits = self.waits_for_sounds();
        let live = self.live.as_mut().expect("checked playback");
        live.last_recycle_at = Some(observed_at);
        live.progress_at = observed_at;
        live.progress_clock = live.device.clock_nanos();
        self.session
            .set_schedule_lead(live.device.schedule_lead_seconds());
        self.session
            .set_continuity_margin(live.device.continuity_margin_seconds());
        arm_output_recovery_after_recycle(
            &mut self.session,
            &mut live.producer,
            live.device.clock_seconds(),
            live.device.sample_rate(),
            waits,
        )?;
        queue_diagnostic(
            &mut self.pending_diagnostics,
            StudioDiagnostic::info(
                "audio-recycled",
                format!("audio output {name} stopped and was opened again"),
            ),
        );
        self.release_every_retiring_sample();
        Ok(true)
    }

    pub(super) fn recycle_stalled_device(
        &mut self,
        observed_at: Duration,
    ) -> Result<(), RuntimeError> {
        let live = self.live.as_mut().expect("checked playback");
        let clock = live.device.clock_nanos();
        if clock != live.progress_clock {
            live.progress_clock = clock;
            live.progress_at = observed_at;
            return Ok(());
        }
        if observed_at.saturating_sub(live.progress_at) < DEVICE_PROGRESS_DEADLINE
            || live
                .last_recycle_at
                .is_some_and(|last| observed_at.saturating_sub(last) < DEVICE_RECYCLE_COOLDOWN)
        {
            return Ok(());
        }

        self.session.consume_audio_confirmations();
        let samples = self.collect_recycle_samples();
        let transport = self.session.transport();
        let live = self.live.as_mut().expect("checked playback");
        let recycled = live
            .device
            .recycle_output_to_with_samples(None, &samples, || transport.is_stopped());
        self.session.consume_audio_confirmations();
        if let Err(error) = recycled {
            self.requeue_recycle_samples(samples);
            return Err(runtime_device_error(error));
        }
        if transport.is_stopped() {
            return Err(RuntimeError::Cancelled);
        }
        self.external_outputs().forget();
        let waits = self.waits_for_sounds();
        let live = self.live.as_mut().expect("checked playback");
        live.last_recycle_at = Some(observed_at);
        live.progress_at = observed_at;
        live.progress_clock = live.device.clock_nanos();
        self.session
            .set_schedule_lead(live.device.schedule_lead_seconds());
        self.session
            .set_continuity_margin(live.device.continuity_margin_seconds());
        arm_output_recovery_after_recycle(
            &mut self.session,
            &mut live.producer,
            live.device.clock_seconds(),
            live.device.sample_rate(),
            waits,
        )?;
        queue_diagnostic(
            &mut self.pending_diagnostics,
            StudioDiagnostic::info(
                "audio-recycled",
                format!(
                    "audio device {} was reopened after its clock stopped advancing",
                    live.device.name()
                ),
            ),
        );
        // As on an explicit output change, the seeded replacement and its
        // new producer backlog cannot refer to the old retiring ids.
        self.release_every_retiring_sample();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::tests::*;

    #[test]
    fn opening_output_updates_sample_decode_rate_before_evaluation() {
        let engine = StudioEngine::new(StudioConfig {
            session: rustel_runtime::SessionConfig::default().with_sample_rate(44_100),
            output: Some(rustel_audio::SILENT_OUTPUT_NAME.to_owned()),
            ..StudioConfig::default()
        })
        .expect("studio");
        let library = engine.sample_library().expect("sample library");
        assert_eq!(library.render_rate_for_test(), 44_100);

        // Every startup route opens the output before evaluating its source.
        let device = engine.open_output().expect("silent output");
        assert_eq!(device.sample_rate(), 48_000);
        assert_eq!(library.render_rate_for_test(), device.sample_rate());
        assert_eq!(engine.session.config().sample_rate, 44_100);
    }

    #[test]
    fn configured_output_and_cached_metadata_retain_dispatch() {
        for dispatch in [
            rustel_audio::DspDispatch::automatic(),
            rustel_audio::DspDispatch::portable(),
        ] {
            let mut options = crate::StudioOptions::new("unused.strudel");
            options.session.dsp_dispatch = dispatch;
            let engine = StudioEngine::new(StudioConfig {
                session: options.clone().session,
                output: Some(rustel_audio::SILENT_OUTPUT_NAME.to_owned()),
                ..StudioConfig::default()
            })
            .expect("configured engine");
            let expected = rustel_runtime::capability_registry_for_dispatch(dispatch);
            assert_eq!(
                rustel_runtime::capability_registry_for_dispatch(
                    engine.session.config().dsp_dispatch
                ),
                expected
            );
            // Fresh starts use the same opener as ordinary scores, replay
            // blocks and auditions. No score or physical device is needed.
            for _ in 0..2 {
                let mut device = engine.open_output().expect("configured silent output");
                let registry = Arc::new(rustel_runtime::capability_registry_for_dispatch(
                    device.dispatch(),
                ));
                assert_eq!(registry.as_ref(), &expected);
                let before = StudioDeviceInfo::from_device(&device, Arc::clone(&registry));
                device.recycle_output().expect("recycle selected output");
                let after = StudioDeviceInfo::from_device(&device, Arc::clone(&registry));
                assert_eq!(
                    rustel_runtime::capability_registry_for_dispatch(device.dispatch()),
                    expected
                );
                assert!(Arc::ptr_eq(&before.registry, &after.registry));
                assert_eq!(after.registry.as_ref(), &expected);
                assert_eq!(
                    after.audio.output().host(),
                    &rustel_runtime::AudioHost::Silent
                );
            }
        }
    }

    #[test]
    fn device_recycle_requeues_retained_samples_before_new_publications() {
        let library = rustel_runtime::samples::SampleLibrary::empty();
        let old = DecodedSample::from_parts(48_000, 1, vec![0.25, 0.5]).expect("old sample");
        let new = DecodedSample::from_parts(48_000, 1, vec![0.75, 1.0]).expect("new sample");
        let retained_only_id = SampleId(8);
        let replaced_id = SampleId(9);
        library.requeue_ready(replaced_id, new.clone());
        let retained = HashMap::from([(retained_only_id, old.clone()), (replaced_id, old.clone())]);

        requeue_retained_samples(&library, &retained);

        let ready = library.take_ready();
        assert_eq!(ready.len(), 2);
        assert_eq!(ready[0], (retained_only_id, old));
        assert_eq!(ready[1], (replaced_id, new));
    }

    #[test]
    fn output_recovery_snapshot_selects_latest_valid_ready_body() {
        let mut engine = StudioEngine::new(StudioConfig::default()).expect("studio");
        let library = Arc::clone(engine.session.sample_library().unwrap());
        let old = DecodedSample::from_parts(48_000, 1, vec![0.25]).unwrap();
        let newer = DecodedSample::from_parts(48_000, 1, vec![0.5]).unwrap();
        let replaced = SampleId(8);
        let retained_only = SampleId(9);
        let forgotten = SampleId(10);
        for id in [replaced, retained_only, forgotten] {
            engine.retain_sample_for_test(id, old.clone());
        }
        engine.retain_sample_for_test(SampleId(SAMPLE_BANK_CAPACITY as u32), old.clone());
        library.requeue_ready(replaced, old.clone());
        library.requeue_ready(replaced, newer.clone());
        library.remember_ready_for_test("forgotten-recovery-body", forgotten);
        library.requeue_ready(forgotten, old.clone());
        library.forget_decoded(&std::collections::HashSet::from([forgotten]));

        let samples = engine.collect_recycle_samples();

        assert_eq!(
            samples,
            vec![(replaced, newer.clone()), (retained_only, old)]
        );
        assert!(library.take_ready().is_empty());
        assert_eq!(
            engine.retained_samples[&replaced].identity(),
            newer.identity()
        );
        assert!(engine.sample_last_used.contains_key(&replaced));
    }

    #[test]
    fn delayed_first_preparation_keeps_the_downbeat_and_next_kick_in_time() {
        use rustel_audio::device::ManualLiveOutput;

        const RATE: u32 = 48_000;
        let mut session = Session::with_config(
            SessionConfig::default()
                .with_horizon(1.5)
                .with_sample_rate(RATE),
        )
        .expect("session");
        session.evaluate(r#"s("bd*4")"#).expect("four kicks");
        session.transport().start();
        let generation = session.generation();
        let mut output = ManualLiveOutput::new(RATE, generation).expect("manual output");
        session
            .bind_audio_confirmations(output.device().confirmations())
            .expect("confirmations");
        let preroll = DEFAULT_STUDIO_START_PREROLL;
        let provisional = output.device().schedule_lead_seconds() + preroll.as_secs_f64();
        session.restart_transport_at(provisional);
        let mut pending = Some(generation);
        let mut producer =
            LiveFileProducer::unwatched(DEFAULT_STUDIO_POLL_INTERVAL).expect("producer");

        // Model input opening / first Hydra work consuming the original lead.
        // Advance the actual output clock deterministically, without sleeping
        // or depending on a machine's input-opening latency.
        output.render(&mut vec![0.0; RATE as usize * 2]);
        let now = output.device().clock_seconds();
        assert!(now > provisional, "the old downbeat is already past");
        anchor_initial_start(&mut session, &mut pending, output.device(), preroll);
        assert_eq!(pending, None);
        let mut queued = Vec::new();
        producer
            .step_unwatched_with_clock(
                &mut session,
                || output.device().clock_seconds(),
                RATE,
                |event| {
                    assert!(output.device().push(event.clone()));
                    queued.push(event);
                    true
                },
            )
            .expect("initial prefill");
        assert!(queued.len() >= 2, "both opening kicks are scheduled");
        let first = queued[0].target_frame;
        let second = queued[1].target_frame;
        assert!(
            first > output.device().clock_frames(),
            "downbeat is not late"
        );
        assert_eq!(second - first, u64::from(RATE / 2));
        let first_time = first as f64 / f64::from(RATE);
        assert!(session.cycle_at_time(first_time).abs() < 1e-8);

        let start = output.device().clock_frames();
        let frames = (second + 512 - start) as usize;
        let mut pcm = vec![0.0; frames * 2];
        output.render(&mut pcm);
        let at = ((first - start) * 2) as usize;
        let next = ((second - start) * 2) as usize;
        let attack = &pcm[at..at + 1024];
        assert!(attack.iter().any(|sample| sample.abs() > 1e-4));
        assert_eq!(
            attack,
            &pcm[next..next + 1024],
            "the full first attack survives"
        );
        assert_eq!(output.device().report().late_events, 0);

        // A subsequent tick cannot move already-submitted onsets or restart
        // the bar, even though the output has now passed both opening hits.
        anchor_initial_start(&mut session, &mut pending, output.device(), preroll);
        assert!(session.cycle_at_time(first_time).abs() < 1e-8);
    }

    #[test]
    fn initial_phase_rebase_preserves_the_start_floor_for_slider_requery() {
        use rustel_audio::device::ManualLiveOutput;

        const RATE: u32 = 48_000;
        let mut session =
            Session::with_config(SessionConfig::default().with_horizon(1.0)).expect("session");
        session.evaluate(r#"s("bd*16")"#).expect("score");
        let generation = session.generation();
        let mut output = ManualLiveOutput::new(RATE, generation).expect("manual output");
        session.restart_transport_at(0.2);
        let mut pending = Some(generation);
        output.render(&mut vec![0.0; RATE as usize * 2]);
        let now = output.device().clock_seconds();
        let anchor = now
            + output.device().schedule_lead_seconds()
            + DEFAULT_STUDIO_START_PREROLL.as_secs_f64();
        anchor_initial_start(
            &mut session,
            &mut pending,
            output.device(),
            DEFAULT_STUDIO_START_PREROLL,
        );

        session.set_continuity_margin(0.0);
        session.requery_active_at(now).expect("slider requery");
        let events = session.schedule_audio_at(now, RATE).expect("schedule");
        let first = events.first().expect("downbeat").target_frame;
        assert_eq!(first, (anchor * f64::from(RATE)).round() as u64);
        assert!(events.iter().all(|event| event.target_frame >= first));
    }

    #[test]
    fn initial_anchor_preserves_midi_keys_received_after_the_explicit_start() {
        use rustel_audio::device::ManualLiveOutput;

        let mut session = Session::new().expect("session");
        session
            .evaluate(r#"const kb = await midikeys('keyboard'); kb(0.25).s('tri')"#)
            .expect("keyboard score");
        session.transport().start();
        let generation = session.generation();
        let mut output = ManualLiveOutput::new(48_000, generation).expect("manual output");
        session.restart_transport_at(0.2);
        let mut pending = Some(generation);
        let port = session.midi_input_bus().find("keyboard").expect("keyboard");
        port.observe_note_on(rustel_core::midi_in::now_nanos(), 1, 60, 100);

        output.render(&mut vec![0.0; 48_000 * 2]);
        anchor_initial_start(
            &mut session,
            &mut pending,
            output.device(),
            DEFAULT_STUDIO_START_PREROLL,
        );
        let mut hits = Vec::new();
        port.keys.select(0.0, 1.0, 0, Some((0, 1)), &mut hits);
        assert_eq!(hits.len(), 1, "finishing startup discarded its NoteOn");
        assert_eq!(
            (hits[0].note, hits[0].velocity, hits[0].channel),
            (60, 100, 1)
        );
    }

    #[test]
    fn initial_start_does_not_rebase_a_replacement_or_cancelled_transport() {
        use rustel_audio::device::ManualLiveOutput;

        for cancelled in [false, true] {
            let mut session = Session::new().expect("session");
            session.evaluate(r#"s("bd*4")"#).expect("score");
            session.transport().start();
            let generation = session.generation();
            let output = ManualLiveOutput::new(48_000, generation).expect("manual output");
            let mut pending = Some(generation);
            if cancelled {
                session.transport().stop();
            } else {
                session.reload_at(r#"s("bd*8")"#, false, 2.0).expect("edit");
            }
            let cycle = session.cycle_at_time(2.0);
            anchor_initial_start(
                &mut session,
                &mut pending,
                output.device(),
                DEFAULT_STUDIO_START_PREROLL,
            );
            assert_eq!(pending, None);
            assert_eq!(session.cycle_at_time(2.0), cycle);
            assert_eq!(session.transport().is_stopped(), cancelled);
        }
    }

    #[test]
    fn recovery_preparation_seeds_large_ready_bank_before_first_prefill_copy() {
        use rustel_audio::confirmation::WindowOutcome;
        use rustel_audio::device::ManualLiveOutput;
        use rustel_core::{Value, pure};

        const RATE: u32 = 48_000;
        const BACKLOG: usize = 1536;
        const SOUND: &str = "recovery-backlog-sample";
        for frames in [128_usize, 1024] {
            let mut engine = StudioEngine::new(StudioConfig {
                session: SessionConfig::default()
                    .with_cps(1.0)
                    .with_horizon(1.0)
                    .with_sample_rate(RATE),
                ..StudioConfig::default()
            })
            .expect("studio");
            engine
                .session
                .enable_default_samples()
                .expect("sample library");
            let library = Arc::clone(engine.session.sample_library().unwrap());
            let decoded = DecodedSample::from_parts(RATE, 1, vec![0.25; RATE as usize])
                .expect("one-second retained body");
            let identity = decoded.identity();
            engine.retained_samples = (1..=BACKLOG as u32)
                .map(|slot| (SampleId(slot), decoded.clone()))
                .collect();
            // The required body is last in the retained bank's transfer order.
            // All ids fit the bank and share one immutable decoded body.
            let id = *engine.retained_samples.keys().last().unwrap();
            let samples = engine.collect_recycle_samples();
            assert_eq!(samples.len(), BACKLOG);
            assert_eq!(engine.sample_last_used.len(), BACKLOG);
            assert!(library.take_ready().is_empty());
            library.remember_ready_for_test(SOUND, id);
            assert_eq!(
                library.readiness(SOUND, 0.0),
                rustel_runtime::samples::SoundReadiness::Ready
            );
            assert_eq!(library.decoded_identity(id), Some(identity));
            let session = &mut engine.session;
            session
                .set_pattern(
                    pure(Value::object(vec![
                        ("s".into(), Value::Str(SOUND.into())),
                        ("gain".into(), Value::F64(0.5)),
                    ]))
                    .fast(8.into()),
                )
                .expect("native sample controls");
            session.set_schedule_lead(0.35);
            session.set_continuity_margin(0.35);
            session.transport().start();
            // This is Studio's actual collection path and the same prepared
            // backend used by a seeded output restart, not ring publication.
            let mut output =
                ManualLiveOutput::new_with_samples(RATE, session.generation(), &samples)
                    .expect("seeded recovery bank");
            let confirmations = output.device().confirmations();
            session
                .bind_audio_confirmations(confirmations.clone())
                .expect("bind real receipts");
            let mut producer =
                LiveFileProducer::unwatched(Duration::from_millis(2)).expect("producer");
            arm_output_recovery_after_recycle(session, &mut producer, 0.0, RATE, true)
                .expect("recovery requery");
            assert_eq!(output.device().report().asset_queues.sample_installs, 0);

            let mut published = None;
            let mut queued = Vec::new();
            let device = output.device();
            producer
                .step_unwatched_with_clock_and_cutover(
                    session,
                    || device.clock_seconds(),
                    RATE,
                    |generation, takeover, cut| {
                        published = Some((generation, takeover, cut));
                        device.set_generation(generation, takeover, cut);
                    },
                    |event| {
                        assert!(device.push(event.clone()));
                        queued.push(event);
                        true
                    },
                )
                .expect("Ready recovery prefill");
            let (generation, takeover, _cut) = published.expect("generation publication");
            assert_eq!(takeover, 16_800, "350 ms recovery lead");
            assert!((2..=8).contains(&queued.len()));
            assert!(queued.iter().all(|event| {
                event.sample.is_some_and(|sample| sample.sample == id)
                    && event.expected_sample_identity == Some(identity)
                    && event.confirmation.is_some()
                    && event.generation == generation
                    && event.target_frame >= takeover
            }));
            let first_target = queued[0].target_frame;
            let last_target = queued.last().unwrap().target_frame;
            assert_eq!(first_target, 18_000);
            assert!(queued[1].target_frame > first_target + 256 + frames as u64);
            let callbacks = (last_target + frames as u64).div_ceil(frames as u64);
            assert!(callbacks <= 512);

            let mut pcm = vec![0.0_f32; frames * 2];
            let mut first_peak = 0.0_f32;
            let mut later_peak = 0.0_f32;
            let mut first_copy_end = None;
            for _ in 0..callbacks {
                output.render(&mut pcm);
                let report = output.device().report();
                assert!(pcm.iter().all(|sample| sample.is_finite()));
                assert_eq!(report.asset_queues.sample_installs, 0);
                let peak = pcm
                    .iter()
                    .fold(0.0_f32, |peak, sample| peak.max(sample.abs()));
                if first_copy_end.is_none() {
                    first_peak = first_peak.max(peak);
                    if report.submitted_frames >= first_target + 256 {
                        first_copy_end = Some(report.submitted_frames);
                    }
                } else {
                    later_peak = later_peak.max(peak);
                }
            }
            assert!(
                later_peak > 0.0,
                "later onsets must play the installed body"
            );
            let terminal = confirmations.pop_terminal().expect("real copy terminal");
            assert_eq!(terminal.generation, generation);
            assert_eq!(terminal.takeover_frame, takeover);
            assert!(terminal.copied_end_frame > first_target);
            assert!(confirmations.pop_terminal().is_none());
            let report = output.device().report();
            assert_eq!(report.callback_errors, 0);
            assert_eq!(report.callback_scope_misses, 0);
            assert_eq!(report.callback_allocations, 0);
            assert_eq!(report.callback_frees, 0);
            assert_eq!(report.ring_role_conflicts, 0);
            assert_eq!(report.late_events, 0);
            assert_eq!(report.asset_queues.leaked, 0);
            assert_eq!(
                terminal.outcome,
                WindowOutcome::Confirmed,
                "{frames} host frames"
            );
            assert!(
                first_peak > 0.0,
                "{frames} host frames: first ready onset lost"
            );
        }
    }

    #[test]
    fn output_preparation_failure_restores_score_through_studio_ticks() {
        use rustel_core::{Value, pure};
        let pattern = pure(Value::object(vec![
            ("s".into(), Value::Str("sine".into())),
            ("note".into(), Value::F64(60.0)),
            ("gain".into(), Value::F64(0.25)),
        ]))
        .fast(16.into());
        let mut engine = silent_engine_with_pattern(pattern);
        wait_for_output_notes(&mut engine, 0);
        let before = engine.live.as_ref().unwrap().device.report();
        let generation = engine.generation();
        let preference = engine.preferred_output.clone();
        let mut attempts = 0;
        let error = engine
            .set_output_device_with("replacement", |device, selector, samples| {
                attempts += 1;
                if attempts == 1 {
                    assert_eq!(selector, Some("replacement"));
                    device.fail_output_replacement_for_test()
                } else {
                    assert_eq!(attempts, 2, "only one recovery attempt is allowed");
                    assert_eq!(selector, Some(rustel_audio::SILENT_OUTPUT_NAME));
                    device.recycle_output_to_with_samples(selector, samples, || false)
                }
            })
            .expect_err("requested output was not installed");
        assert!(error.to_string().contains("output preparation failed"));
        // Drive the ordinary health check, refill and callback, not a manual
        // producer push. A healthy empty replacement is not recovered music.
        wait_for_output_notes(&mut engine, before.accepted_events);
        assert_eq!(attempts, 2);
        assert_eq!(engine.generation(), generation + 1);
        assert_eq!(engine.preferred_output, preference);
        assert_eq!(
            engine.output_name().as_deref(),
            Some(rustel_audio::SILENT_OUTPUT_NAME)
        );
        let after = engine.live.as_ref().unwrap().device.report();
        assert!(after.callbacks > before.callbacks);
        assert!(after.playhead_nanos > before.playhead_nanos);
    }

    /// A stream that reports an error is reopened, and the set plays on.
    ///
    /// Taking a Bluetooth headset's microphone drops it out of A2DP and
    /// the output stream with it, which is a device going away under the
    /// music rather than a machine that cannot keep up: the studio said
    /// "failed while playing live", stopped, and left `no output` behind.
    /// It reopens instead, the way it does for a clock that stops
    /// advancing, and says so in the log.
    #[test]
    fn an_output_stream_that_fails_is_opened_again_rather_than_ending_the_set() {
        use rustel_core::{Value, pure};
        let pattern = pure(Value::object(vec![
            ("s".into(), Value::Str("sine".into())),
            ("note".into(), Value::F64(60.0)),
            ("gain".into(), Value::F64(0.25)),
        ]))
        .fast(16.into());
        let mut engine = silent_engine_with_pattern(pattern);
        wait_for_output_notes(&mut engine, 0);
        let sounded = engine
            .live
            .as_ref()
            .expect("live output")
            .device
            .report()
            .accepted_events;

        engine
            .live
            .as_ref()
            .expect("live output")
            .device
            .fail_output_stream_for_test();
        let mut said: Vec<StudioDiagnostic> = Vec::new();
        let tick = engine
            .tick(|update| {
                if let StudioUpdate::Diagnostic(diagnostic) = &update {
                    said.push(diagnostic.clone());
                }
                Ok(())
            })
            .expect("the set plays on");
        assert!(
            matches!(tick, StudioTick::Running { .. }),
            "a failed stream is reopened, not the end of the set: {tick:?}"
        );
        assert!(engine.is_playing());
        assert!(
            !engine
                .live
                .as_ref()
                .expect("live output")
                .device
                .output_failed(),
            "and the stream that replaced it is a healthy one"
        );
        assert!(
            said.iter().any(|diagnostic| {
                diagnostic.kind == "audio-recycled"
                    && diagnostic.level == StudioDiagnosticLevel::Info
                    && diagnostic.message.contains("opened again")
            }),
            "the log says what happened: {said:?}"
        );
        // And the music is really on the new stream: events accepted
        // past what the old one had taken, which is what `no output` cost
        // and what a reopen is for.
        wait_for_output_notes(&mut engine, sounded);

        // Failing again inside the cooldown is a device that is not coming
        // back: the studio says so rather than reopening it every turn.
        engine
            .live
            .as_ref()
            .expect("live output")
            .device
            .fail_output_stream_for_test();
        let error = engine
            .tick(|_| Ok(()))
            .expect_err("a device that keeps failing ends the set");
        assert!(
            error.to_string().contains("failed while playing live"),
            "{error}"
        );
        assert!(engine.session.transport().is_stopped());
        assert!(engine.piano_output.is_none());
    }

    #[test]
    fn output_recovery_failure_stops_without_repeated_attempts() {
        let mut engine = silent_engine_for_output_selection();
        engine.config.stop_timeout = Duration::from_millis(10);
        let library = Arc::clone(engine.session.sample_library().unwrap());
        let id = SampleId(512);
        let decoded = DecodedSample::from_parts(48_000, 1, vec![0.25]).unwrap();
        library.requeue_ready(id, decoded.clone());
        let preference = engine.preferred_output.clone();
        let mut attempts = 0;
        let error = engine
            .set_output_device_with("replacement", |device, _, samples| {
                attempts += 1;
                assert_eq!(samples, &[(id, decoded.clone())]);
                device.fail_output_replacement_for_test()
            })
            .expect_err("both outputs failed");
        assert!(error.to_string().contains("output preparation failed"));
        assert!(
            error
                .to_string()
                .contains("could not reopen previous output")
        );
        assert_eq!(attempts, 2);
        assert!(!engine.is_playing());
        assert!(engine.session.transport().is_stopped());
        assert!(engine.piano_output.is_none());
        assert_eq!(engine.preferred_output, preference);
        assert_eq!(engine.retained_samples.get(&id), Some(&decoded));
        assert_eq!(library.take_ready(), vec![(id, decoded)]);
        for _ in 0..3 {
            assert!(matches!(engine.tick(|_| Ok(())).unwrap(), StudioTick::Idle));
        }
    }

    #[test]
    fn stop_during_output_change_prevents_recovery_prefill() {
        for stop_on_attempt in [1, 2] {
            let mut engine = silent_engine_for_output_selection();
            engine.config.stop_timeout = Duration::from_millis(10);
            let library = Arc::clone(engine.session.sample_library().unwrap());
            let id = SampleId(512);
            let decoded = DecodedSample::from_parts(48_000, 1, vec![0.25]).unwrap();
            library.requeue_ready(id, decoded.clone());
            let stop = engine.stop_handle();
            let generation = engine.generation();
            let mut attempts = 0;
            let error = engine
                .set_output_device_with("replacement", |device, selector, samples| {
                    attempts += 1;
                    let result = if attempts == 1 {
                        device.fail_output_replacement_for_test()
                    } else {
                        device.recycle_output_to_with_samples(selector, samples, || false)
                    };
                    if attempts == stop_on_attempt {
                        stop.request_stop();
                    }
                    result
                })
                .expect_err("Stop cancels recovery");
            assert!(error.to_string().contains("output preparation failed"));
            assert!(error.to_string().contains("cancelled"));
            assert_eq!(attempts, stop_on_attempt);
            assert_eq!(
                engine.generation(),
                generation,
                "no recovery requery after Stop"
            );
            assert!(!engine.is_playing());
            assert!(stop.is_stopped());
            assert!(engine.piano_output.is_none());
            assert_eq!(engine.retained_samples.get(&id), Some(&decoded));
            assert_eq!(library.take_ready(), vec![(id, decoded)]);
        }
    }

    #[test]
    fn output_selection_during_stop_applies_on_the_next_start() {
        let mut engine = silent_engine_for_output_selection();
        engine.preferred_output = None;
        let stream = engine.live.as_ref().unwrap().device.stream_id();
        let generation = engine.generation();
        engine.request_stop();
        assert!(matches!(
            engine.advance_graceful_stop(Duration::ZERO),
            StudioTick::Stopping
        ));
        engine
            .set_output_device_with(rustel_audio::SILENT_OUTPUT_NAME, |_, _, _| {
                panic!("reopened after Stop")
            })
            .expect("record next preference");
        assert!(!engine.is_playing());
        assert!(engine.session.transport().is_stopped());
        assert!(engine.piano_output.is_none());
        assert_eq!(engine.generation(), generation);
        assert_eq!(
            engine.preferred_output.as_deref(),
            Some(rustel_audio::SILENT_OUTPUT_NAME)
        );
        start_selected_output(&mut engine);
        let device = &engine.live.as_ref().unwrap().device;
        assert_ne!(device.stream_id(), stream);
        assert_eq!(device.name(), rustel_audio::SILENT_OUTPUT_NAME);
    }

    #[test]
    fn an_error_stop_releases_the_output_without_a_stream_failure() {
        let mut engine = silent_engine_for_output_selection();
        let stream = engine.live.as_ref().unwrap().device.stream_id();
        assert!(!engine.live.as_ref().unwrap().device.output_failed());
        let stop = engine
            .stop_output(Duration::from_millis(500), false)
            .expect("stop output");
        assert!(stop.acknowledged);
        assert!(engine.piano_output.is_none());
        start_selected_output(&mut engine);
        assert_ne!(engine.live.as_ref().unwrap().device.stream_id(), stream);
    }

    #[test]
    fn an_unacknowledged_stop_releases_the_output_after_its_timeout() {
        let mut engine = silent_engine_for_output_selection();
        let stream = engine.live.as_ref().unwrap().device.stream_id();
        hold_clock(&engine, true);
        let stop = engine.stop(Duration::from_millis(10)).expect("stop output");
        assert!(!stop.acknowledged);
        assert!(engine.piano_output.is_none());
        start_selected_output(&mut engine);
        let device = &engine.live.as_ref().unwrap().device;
        assert_ne!(device.stream_id(), stream);
        settle_callbacks(device, device.callbacks(), 2);
    }

    #[test]
    fn a_failed_start_does_not_restore_an_unhealthy_parked_output() {
        for failed in [false, true] {
            let mut engine = silent_engine_for_output_selection();
            engine.stop(Duration::from_millis(500));
            let device = engine.piano_output.take().expect("parked output");
            if failed {
                device.fail_output_stream_for_test();
            }
            engine.restore_failed_start_device(device, true);
            assert_eq!(engine.piano_output.is_some(), !failed);
        }
    }

    #[test]
    fn refreshing_the_default_output_preserves_piano_and_tail_ownership() {
        let mut engine = silent_engine_for_output_selection();
        engine.stop(Duration::from_millis(500));
        let stream = engine.piano_output.as_ref().unwrap().stream_id();
        engine.refresh_idle_default_output();
        assert_eq!(
            engine.piano_output.as_ref().unwrap().stream_id(),
            stream,
            "an explicit output stays open"
        );

        engine.preferred_output = None;
        engine.piano_close_when_idle = false;
        engine.refresh_idle_default_output();
        assert!(engine.piano_output.is_some(), "the piano owns its output");

        engine.piano_close_when_idle = true;
        engine.piano[0] = Some(PianoVoice {
            releasing: true,
            release_at: None,
        });
        engine.refresh_idle_default_output();
        assert!(engine.piano_output.is_some(), "a key is still releasing");

        engine.piano[0] = None;
        engine.piano_has_score_tail = true;
        engine.refresh_idle_default_output();
        assert!(engine.piano_output.is_some(), "the score tail still sounds");

        engine.piano_has_score_tail = false;
        engine.refresh_idle_default_output();
        assert!(
            engine.piano_output.is_none(),
            "an idle default is refreshed"
        );
    }

    #[test]
    fn output_recovery_seeds_retained_samples_and_accepts_valid_silence() {
        let mut engine = silent_engine_for_output_selection();
        engine
            .session
            .enable_default_samples()
            .expect("sample library");
        let library = Arc::clone(engine.session.sample_library().unwrap());
        let id = SampleId(512);
        let retiring = SampleId(513);
        let decoded = DecodedSample::from_parts(44_100, 1, vec![0.25; 64]).unwrap();
        engine.retain_sample_for_test(id, decoded.clone());
        engine.retain_sample_for_test(retiring, decoded.clone());
        library.forget_id_for_test(retiring);
        engine.retire_sample(retiring, u64::MAX);
        engine.release_retired_samples();
        assert_eq!(engine.retiring_count(), 1, "old horizon still owns the id");
        assert_eq!(library.free_id_count(), 0);
        let generation = engine.generation();
        engine.live.as_mut().unwrap().progress_clock = u64::MAX;
        let mut attempts = 0;
        engine
            .set_output_device_with("replacement", |device, selector, samples| {
                attempts += 1;
                assert_eq!(samples, &[(id, decoded.clone())]);
                if attempts == 1 {
                    device.fail_output_replacement_for_test()
                } else {
                    device.recycle_output_to_with_samples(selector, samples, || false)
                }
            })
            .expect_err("requested output refused");
        assert_eq!(attempts, 2);
        assert!(
            library.take_ready().is_empty(),
            "successful fallback already seeded the bank"
        );
        assert_eq!(engine.retained_samples.get(&id), Some(&decoded));
        engine.retry_pending_uninstalls();
        assert_eq!(engine.retiring_count(), 0);
        assert_eq!(library.free_id_count(), 1, "recovery released the old id");
        assert!(!engine.retained_samples.contains_key(&retiring));
        let live = engine.live.as_ref().unwrap();
        assert_eq!(live.device.report().asset_queues.sample_installs, 0);
        assert_eq!(library.render_rate_for_test(), live.device.sample_rate());
        assert_ne!(live.progress_clock, u64::MAX);
        assert_eq!(live.last_recycle_at, Some(live.progress_at));
        assert_eq!(
            live.device.generation(),
            generation,
            "refill not yet published"
        );
        assert!(matches!(
            engine.tick(|_| Ok(())).unwrap(),
            StudioTick::Running { step: Some(_), .. }
        ));
        assert_eq!(engine.generation(), generation + 1);
        assert_eq!(
            engine.live.as_ref().unwrap().device.generation(),
            generation + 1
        );
        assert!(!engine.session.transport().is_stopped());
    }

    /// The output-latency row recycles the output by its current name so
    /// the new size is heard now. That is not choosing the device: a set
    /// following the host default must still follow it afterwards, or the
    /// next headphone plug-in goes unheard.
    #[test]
    fn changing_the_output_buffer_keeps_following_the_host_default() {
        let mut engine = silent_engine_for_output_selection();
        engine.preferred_output = None;
        engine
            .set_output_buffer_frames(Some(256))
            .expect("a playing set recycles its output");
        assert_eq!(engine.config.output_buffer_frames, Some(256));
        assert_eq!(
            engine.preferred_output, None,
            "a buffer change is not a device choice"
        );
        assert!(engine.live.is_some(), "the set is still playing");
    }

    #[test]
    fn an_output_buffer_change_during_stop_applies_on_the_next_start() {
        let mut engine = silent_engine_for_output_selection();
        let stream = engine.live.as_ref().unwrap().device.stream_id();
        engine.request_stop();
        assert!(matches!(
            engine.advance_graceful_stop(Duration::ZERO),
            StudioTick::Stopping
        ));
        engine
            .set_output_buffer_frames(Some(512))
            .expect("record next buffer size");
        assert!(!engine.is_playing());
        assert!(engine.piano_output.is_none());
        start_selected_output(&mut engine);
        let device = &engine.live.as_ref().unwrap().device;
        assert_ne!(device.stream_id(), stream);
        assert_eq!(
            device.buffer_preference(),
            rustel_audio::AudioBufferPreference::Frames(512)
        );
    }

    /// A live latency change opens a new stream, and what the host granted
    /// is logged the way the first open logs it.
    #[test]
    fn an_output_latency_change_logs_the_facts_of_the_stream_it_opened() {
        let mut engine = silent_engine_for_output_selection();
        engine.pending_diagnostics.clear();
        engine
            .set_output_buffer_frames(Some(512))
            .expect("a playing set recycles its output");
        let facts = engine
            .pending_diagnostics
            .iter()
            .find(|diagnostic| diagnostic.kind == "audio")
            .expect("the recycled stream is described");
        assert_eq!(facts.level, StudioDiagnosticLevel::Note);
        assert!(facts.message.contains("512"), "{}", facts.message);
    }

    #[test]
    fn output_selection_failure_preserves_preference_and_live_state() {
        let mut engine = silent_engine_for_output_selection();
        let library = Arc::clone(engine.session.sample_library().unwrap());
        let id = SampleId(512);
        let replaced = SampleId(513);
        let decoded = DecodedSample::from_parts(48_000, 1, vec![0.25]).unwrap();
        let newer = DecodedSample::from_parts(48_000, 1, vec![0.5]).unwrap();
        library.requeue_ready(id, decoded.clone());
        library.requeue_ready(replaced, decoded.clone());
        engine
            .session
            .evaluate_mini("~")
            .expect("replayable mini source");
        engine
            .session
            .schedule_at(0.0)
            .expect("fill silent horizon");
        let generation = engine.generation();
        let horizon = engine.session.scheduled_to_cycle();
        let source = engine.session.active_source().map(str::to_owned);
        assert_eq!(source.as_deref(), Some("~"));
        let preference = engine.preferred_output.clone();
        let live = engine.live.as_mut().unwrap();
        live.device.set_generation(generation, 0, TakeoverCut::None);
        live.progress_clock = 123;
        let stream = live.device.stream_id();
        let output = live.device.audio_facts();
        let mut calls = 0;

        let error = engine
            .set_output_device_with("refused-output", |device, selector, samples| {
                calls += 1;
                assert_eq!(selector, Some("refused-output"));
                assert_eq!(device.stream_id(), stream);
                assert_eq!(
                    samples,
                    &[(id, decoded.clone()), (replaced, decoded.clone())]
                );
                library.requeue_ready(replaced, newer.clone());
                Err(DevicePlaybackError::Unavailable("discovery refused".into()))
            })
            .expect_err("refused recycle");

        assert!(matches!(error, RuntimeError::Audio(message) if message == "discovery refused"));
        assert_eq!(calls, 1);
        assert!(engine.is_playing());
        assert!(!engine.session.transport().is_stopped());
        assert_eq!(engine.generation(), generation);
        assert_eq!(engine.session.scheduled_to_cycle(), horizon);
        assert_eq!(engine.session.active_source(), source.as_deref());
        let live = engine.live.as_ref().unwrap();
        assert_eq!(live.device.stream_id(), stream);
        assert_eq!(live.device.audio_facts(), output);
        assert_eq!(live.device.generation(), generation);
        assert_eq!(live.progress_clock, 123);
        assert!(live.draining.is_none());
        assert_eq!(engine.preferred_output, preference);
        assert_eq!(library.take_ready(), vec![(id, decoded), (replaced, newer)]);
    }

    #[test]
    fn stopped_output_selection_records_preference_without_recycling() {
        let mut engine = StudioEngine::new(StudioConfig::default()).expect("studio");
        engine.session.transport().stop();
        let generation = engine.generation();
        engine
            .set_output_device_with("next-output", |_, _, _| {
                panic!("stopped selection recycled")
            })
            .expect("record preference");
        assert!(!engine.is_playing());
        assert!(engine.session.transport().is_stopped());
        assert_eq!(engine.generation(), generation);
        assert_eq!(engine.preferred_output.as_deref(), Some("next-output"));
        assert_eq!(engine.output_name().as_deref(), Some("next-output"));
    }

    #[test]
    fn successful_output_selection_commits_preference_and_requeries_once() {
        let mut engine = silent_engine_for_output_selection();
        engine.preferred_output = None;
        let generation = engine.generation();
        let mut calls = 0;
        engine
            .set_output_device_with(
                rustel_audio::SILENT_OUTPUT_NAME,
                |device, selector, samples| {
                    calls += 1;
                    assert_eq!(selector, Some(rustel_audio::SILENT_OUTPUT_NAME));
                    device.recycle_output_to_with_samples(selector, samples, || false)
                },
            )
            .expect("recycle silent output");
        assert_eq!(calls, 1);
        assert!(engine.is_playing());
        assert!(!engine.session.transport().is_stopped());
        assert_eq!(engine.generation(), generation + 1);
        assert_eq!(
            engine.preferred_output.as_deref(),
            Some(rustel_audio::SILENT_OUTPUT_NAME)
        );
        assert_eq!(
            engine.output_name().as_deref(),
            Some(rustel_audio::SILENT_OUTPUT_NAME)
        );
        // Recovery has re-armed scheduling, not published a consumer result.
        assert_eq!(
            engine.live.as_ref().unwrap().device.generation(),
            generation
        );
    }

    #[test]
    fn recycled_output_updates_decode_rate_without_rewriting_ready_samples() {
        let mut session =
            Session::with_config(rustel_runtime::SessionConfig::default().with_sample_rate(44_100))
                .expect("session");
        session.enable_default_samples().expect("sample library");
        session
            .set_pattern(rustel_core::silence())
            .expect("silent pattern");
        let library = Arc::clone(session.sample_library().unwrap());
        let decoded = DecodedSample::from_parts(44_100, 1, vec![0.25, 0.5]).unwrap();
        library.requeue_ready(SampleId(8), decoded.clone());
        let mut producer = LiveFileProducer::unwatched(Duration::from_millis(2)).unwrap();

        // Both manual switching and stalled-output recovery use this seam.
        // Rebinding the same rate must also leave already decoded PCM alone.
        for rate in [48_000, 96_000, 96_000, 44_100] {
            let generation = session.generation();
            arm_output_recovery_after_recycle(&mut session, &mut producer, 0.25, rate, true)
                .expect("arm recovery");
            assert_eq!(library.render_rate_for_test(), rate);
            assert_eq!(session.config().sample_rate, 44_100);
            assert_eq!(session.generation(), generation + 1);
        }
        // An invalid requery clock must not restore the discarded output's
        // rate: the replacement stream has already opened successfully.
        let generation = session.generation();
        assert!(
            arm_output_recovery_after_recycle(&mut session, &mut producer, f64::NAN, 88_200, true)
                .is_err()
        );
        assert_eq!(library.render_rate_for_test(), 88_200);
        assert_eq!(session.generation(), generation);
        assert_eq!(library.take_ready(), vec![(SampleId(8), decoded)]);
    }

    #[test]
    fn device_recycle_requeries_and_refills_the_drained_horizon() {
        let source = "note('c4').fast(16)";
        let mut session = Session::new().expect("session");
        session.evaluate(source).expect("score");
        session.set_schedule_lead(0.1);
        session.set_continuity_margin(0.1);
        let mut producer = LiveFileProducer::unwatched(Duration::from_millis(2)).expect("producer");
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
        let recovery_now = 0.25;
        arm_output_recovery_after_recycle(&mut session, &mut producer, recovery_now, 44_100, true)
            .expect("arm recovery");
        let after = session.generation();
        assert_eq!(after, before + 1);

        let sample_rate = 44_100;
        let expected_takeover = ((recovery_now + 0.1) * f64::from(sample_rate)).round() as u64;
        let mut publication = None;
        let mut recovered = Vec::new();
        producer
            .step_unwatched_with_clock_and_cutover(
                &mut session,
                || recovery_now,
                sample_rate,
                |generation, frame, _cut| publication = Some((generation, frame)),
                |event| {
                    recovered.push(event);
                    true
                },
            )
            .expect("recovery prefill");

        assert_eq!(publication, Some((after, expected_takeover)));
        assert!(!recovered.is_empty(), "drained horizon was not refilled");
        assert!(
            recovered.iter().all(|event| {
                event.generation == after && event.target_frame >= expected_takeover
            })
        );
    }
}
