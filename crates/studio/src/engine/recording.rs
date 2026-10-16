use super::*;

impl StudioEngine {
    // ---- takes ---------------------------------------------------------

    /// Start writing the final mix to `path`. With nothing playing the
    /// device is opened on a silent score first, so the take starts now.
    pub fn start_recording(&mut self, path: std::path::PathBuf) -> Result<(), RuntimeError> {
        if self.closing_recording.is_some() {
            return Err(RuntimeError::Message(
                "the previous take is still closing".into(),
            ));
        }
        if self.recording.is_some() {
            return Err(RuntimeError::Message("already recording".into()));
        }
        if self.sample_recording.is_some() {
            return Err(RuntimeError::Message(
                "a sample is recording - finish the sample before recording a take".into(),
            ));
        }
        if self.live.is_none() {
            self.start_silence()?;
        }
        let live = self.live.as_mut().expect("device opened");
        // A preview may have opened this otherwise-idle transport. Recording
        // takes ownership now, so the preview horizon must not retire it.
        live.audition_owned = false;
        let sample_rate = live.device.sample_rate();
        let writer = TakeWriter::start(path, sample_rate)
            .map_err(|error| RuntimeError::Message(format!("cannot record: {error}")))?;
        let capture = live
            .device
            .start_recording_capture()
            .map_err(|error| RuntimeError::Message(format!("cannot record: {error}")))?;
        self.recording = Some(Recording {
            writer,
            input: Some(RecordingInput {
                capture,
                sample_rate,
            }),
            sample_rate,
            started: Instant::now(),
            frames: 0,
            dropped: 0,
            tap_dropped_seen: 0,
            scratch: Vec::with_capacity(1 << 16),
        });
        Ok(())
    }

    /// Close and join the take. On success, everything queued reaches disk
    /// and the header carries the final sizes; inspect the returned status
    /// for writer errors.
    ///
    /// The wait is bounded. The audio callback retires the tap, and a
    /// device that never calls again (a wedged driver, an unplugged
    /// interface) must not block this thread: a blocked studio cannot read
    /// a key or handle a signal. Frames the tap never hands over are not
    /// on disk; the writer is still joined and its status is still
    /// reported.
    pub fn stop_recording(&mut self) -> Option<TakeStatus> {
        self.stop_recording_with(RecordCapture::is_closed)
    }

    /// `stop_recording` with the tap's retirement test injected, so a
    /// driver that never calls again can be modelled without one.
    fn stop_recording_with(
        &mut self,
        retired: impl Fn(&RecordCapture) -> bool,
    ) -> Option<TakeStatus> {
        const TAP_CLOSE_WAIT: Duration = Duration::from_millis(500);
        // Ask first: a take nobody has asked to close yet (the engine
        // dropping on quit with the tape still rolling) has no closure to
        // advance, and returning None there left the writer unjoined and
        // the header without its sizes.
        self.request_recording_close();
        let deadline = Instant::now() + TAP_CLOSE_WAIT;
        loop {
            self.advance_recording_close_with(|capture| retired(capture));
            match self.closing_recording.take()? {
                ClosingRecording::Writer(writer) => return Some(writer.finish()),
                pending @ ClosingRecording::Tap { .. } => {
                    if Instant::now() >= deadline {
                        // Return the tap so `advance_recording_close` can
                        // keep trying on later turns; the caller hears the
                        // same "nothing closed" a fresh take would say.
                        self.closing_recording = Some(pending);
                        return None;
                    }
                    self.closing_recording = Some(pending);
                    std::thread::sleep(Duration::from_millis(1));
                }
            }
        }
    }

    /// Close the tap without waiting for its writer. An existing closure
    /// remains owned here until it is collected, including after audio Stop.
    pub(crate) fn request_recording_close(&mut self) -> bool {
        if !self.close_recording_for_rate_change() {
            self.close_recording(true);
        }
        self.closing_recording.is_some()
    }

    fn close_recording_for_rate_change(&mut self) -> bool {
        let (Some(recording), Some(device)) = (self.recording.as_ref(), self.audio_device()) else {
            return false;
        };
        if device.sample_rate() == recording.sample_rate {
            return false;
        }
        let path = recording.writer.status().path;
        self.close_recording(true);
        self.pending_diagnostics
            .push_back(StudioDiagnostic::message(
                "record",
                format!(
                    "take ended at {}: the output changed sample rate",
                    path.display()
                ),
            ));
        true
    }

    fn close_recording(&mut self, mut flush: bool) {
        let Some(mut recording) = self.recording.take() else {
            return;
        };
        debug_assert!(self.closing_recording.is_none());
        if let Some(input) = recording.input.as_ref()
            && let Err(error) = input.capture.request_close()
        {
            recording.writer.fail(&error.to_string());
            flush = false;
            recording.input = None;
        }
        let target = recording.target_frames();
        self.closing_recording = Some(ClosingRecording::Tap {
            recording,
            target,
            has_output: self.audio_device().is_some(),
            flush,
        });
    }

    pub(super) fn advance_recording_close(&mut self) {
        self.advance_recording_close_with(RecordCapture::is_closed);
    }

    fn advance_recording_close_with(&mut self, retired: impl FnOnce(&RecordCapture) -> bool) {
        let Some(closing) = self.closing_recording.take() else {
            return;
        };
        let ClosingRecording::Tap {
            mut recording,
            target,
            has_output,
            flush,
        } = closing
        else {
            self.closing_recording = Some(closing);
            return;
        };
        if recording
            .input
            .as_ref()
            .is_some_and(|input| !retired(&input.capture))
        {
            self.closing_recording = Some(ClosingRecording::Tap {
                recording,
                target,
                has_output,
                flush,
            });
            return;
        }
        if flush {
            recording.scratch.clear();
            let result = recording.drain_input(None).and_then(|loss| {
                recording
                    .submit(loss, target, has_output)
                    .map_err(str::to_owned)
            });
            if let Err(error) = result {
                recording.writer.fail(&error);
            }
        }
        let mut writer = recording.writer;
        writer.request_close();
        self.closing_recording = Some(ClosingRecording::Writer(writer));
    }

    /// Collect once, without joining an unfinished writer. The caller must
    /// reserve space for the final reply before removing its only owner.
    pub(crate) fn try_finish_recording(&mut self) -> Option<TakeStatus> {
        self.advance_recording_close();
        let ClosingRecording::Writer(writer) = self.closing_recording.as_mut()? else {
            return None;
        };
        let status = writer.try_finish()?;
        drop(self.closing_recording.take());
        Some(status)
    }

    pub fn is_recording(&self) -> bool {
        self.recording.is_some()
    }

    /// The ring the chosen input writes into right now: the live device's
    /// while one plays, the monitor's while nothing does.
    fn chosen_input_ring(&self) -> Option<Arc<rustel_audio::input::InputRing>> {
        match self.live.as_ref() {
            Some(live) => live.device.input_ring(),
            None => self
                .input_monitor
                .as_ref()
                .map(|input| Arc::clone(input.ring())),
        }
    }

    /// Start recording a sample from the audio input into `path`.
    ///
    /// The chosen input when there is one - the one `s("in")` plays,
    /// through its fader - and otherwise the host's default input, opened
    /// for this recording alone. Refused while a take records, and refused
    /// as a machine with no input refuses it when the config opens no
    /// default input ([`StudioConfig::open_default_input`], a harness's
    /// setting).
    pub fn start_sample_recording(&mut self, path: std::path::PathBuf) -> Result<(), RuntimeError> {
        if self.recording.is_some() || self.closing_recording.is_some() {
            return Err(RuntimeError::Message(
                "a take is recording - finish the take before recording a sample".into(),
            ));
        }
        if self.sample_recording.is_some() {
            return Err(RuntimeError::Message("already recording a sample".into()));
        }
        let mut own_input = None;
        let ring = if self.preferred_input.is_some() {
            self.ensure_input();
            self.chosen_input_ring().ok_or_else(|| {
                RuntimeError::Message(
                    "the chosen audio input is not open - check it under Devices".into(),
                )
            })?
        } else {
            let no_input = |why: &dyn std::fmt::Display| {
                RuntimeError::Message(format!("no audio input to record from: {why}"))
            };
            if !self.config.open_default_input {
                return Err(no_input(&"this studio opens no default input"));
            }
            let ring = Arc::new(rustel_audio::input::InputRing::new());
            let input = rustel_audio::AudioInput::open(None, Arc::clone(&ring), None)
                .map_err(|error| no_input(&error))?;
            own_input = Some(input);
            ring
        };
        let sample_rate = ring.sample_rate();
        if sample_rate == 0 {
            return Err(RuntimeError::Message(
                "the audio input has not started yet - press again in a moment".into(),
            ));
        }
        let writer = TakeWriter::start(path, sample_rate)
            .map_err(|error| RuntimeError::Message(format!("cannot record: {error}")))?;
        self.sample_recording = Some(SampleRecording {
            writer,
            cursor: ring.written(),
            ring,
            own_input,
            dropped: 0,
        });
        Ok(())
    }

    pub fn is_recording_sample(&self) -> bool {
        self.sample_recording.is_some()
    }

    /// One turn of the sample recorder. The input can move between the
    /// monitor and the live device as playback starts and stops; the
    /// recording follows it, and what fell between the two is not in it.
    pub(super) fn pump_sample_recording(&mut self) {
        let chosen = self.chosen_input_ring();
        let Some(recording) = self.sample_recording.as_mut() else {
            return;
        };
        if recording.own_input.is_none()
            && let Some(ring) = chosen
            && !Arc::ptr_eq(&ring, &recording.ring)
        {
            recording.cursor = ring.written();
            recording.ring = ring;
        }
        recording.drain();
    }

    /// Finish the sample: what the input wrote since the last turn, then
    /// the writer joined. No tap has to retire, so this does not wait on
    /// the audio callback. `None` when no sample is recording.
    pub fn stop_sample_recording(&mut self) -> Option<TakeStatus> {
        self.pump_sample_recording();
        let mut recording = self.sample_recording.take()?;
        recording.drain();
        // The default input opened for this recording goes with it.
        drop(recording.own_input.take());
        if recording.dropped > 0 {
            self.pending_diagnostics
                .push_back(StudioDiagnostic::message(
                    "record",
                    format!(
                        "the sample lost {:.2}s the input wrote faster than it was read",
                        recording.dropped as f64 / f64::from(recording.ring.sample_rate().max(1))
                    ),
                ));
        }
        Some(recording.writer.finish())
    }

    pub(super) fn recording_info(&self) -> Option<RecordingInfo> {
        let recording = self.recording.as_ref()?;
        let status = recording.writer.status();
        Some(RecordingInfo {
            path: status.path,
            seconds: recording.frames as f64 / f64::from(recording.sample_rate.max(1)),
            bytes: status.bytes,
            dropped_seconds: recording.dropped as f64 / f64::from(recording.sample_rate.max(1)),
            error: status.error,
        })
    }

    /// Hand one drained PCM batch and its trailing padding to the writer.
    /// Queue refusal still drops the whole batch; gap positions are unchanged.
    pub(super) fn pump_recording(&mut self) {
        self.advance_recording_close();
        if self.close_recording_for_rate_change() {
            return;
        }
        let device = self
            .live
            .as_ref()
            .map(|live| &live.device)
            .or(self.piano_output.as_ref());
        let Some(recording) = self.recording.as_mut() else {
            return;
        };
        recording.scratch.clear();
        let result = recording.drain_input(device).and_then(|loss| {
            recording
                .submit(loss, recording.target_frames(), device.is_some())
                .map_err(str::to_owned)
        });
        if let Err(error) = result {
            recording.writer.fail(&error);
            self.close_recording(false);
            return;
        }
        if recording.input.is_none()
            && let Some(device) = device
        {
            match device.start_recording_capture() {
                Ok(capture) => {
                    recording.input = Some(RecordingInput {
                        capture,
                        sample_rate: device.sample_rate(),
                    })
                }
                Err(error) => {
                    recording.writer.fail(&error.to_string());
                    self.close_recording(false);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::tests::*;

    #[test]
    fn recording_padding_keeps_tap_loss_distinct_from_wall_clock_padding() {
        assert_eq!(recording_padding(100, 10, 20, 135, 100, true), Ok(20));
        assert_eq!(recording_padding(100, 10, 20, 190, 100, true), Ok(80));
        assert_eq!(recording_padding(100, 0, 0, 350, 100, false), Ok(100));
        assert_eq!(
            recording_padding(0, 1, 1 << 40, 0, 48_000, true),
            Ok(1 << 40)
        );
        assert_eq!(
            recording_padding(u64::MAX, 1, 0, 0, 48_000, true),
            Err("recording frame count overflow")
        );
        assert_eq!(
            recording_padding(0, 1, u64::MAX, 0, 48_000, true),
            Err("recording frame count overflow")
        );
    }

    #[test]
    fn recording_submission_preserves_attempted_frames_and_whole_item_drops() {
        let directory = tempfile::tempdir_in(env!("CARGO_MANIFEST_DIR")).unwrap();
        let mut recording = Recording {
            writer: TakeWriter::start(directory.path().join("counts.wav"), 48_000).unwrap(),
            input: None,
            sample_rate: 48_000,
            started: Instant::now(),
            frames: 0,
            dropped: 0,
            tap_dropped_seen: 0,
            scratch: vec![0.5, -0.5],
        };
        recording.submit(2, 0, true).unwrap();
        assert_eq!((recording.frames, recording.dropped), (3, 2));
        recording.writer.request_close();
        recording.scratch.extend([0.25, -0.25]);
        recording.submit(0, 0, true).unwrap();
        assert_eq!((recording.frames, recording.dropped), (4, 3));
        let status = recording.writer.finish();
        assert_eq!(status.error, None);
        assert_eq!(status.frames, 3, "the rejected frame was not retried");
        assert_eq!(status.final_signal.unwrap().sample_count, 6);
    }

    #[test]
    fn recording_submission_bounds_large_rejected_padding_without_materializing_it() {
        let directory = tempfile::tempdir_in(env!("CARGO_MANIFEST_DIR")).unwrap();
        let mut recording = Recording {
            writer: TakeWriter::start(directory.path().join("rejected.wav"), 48_000).unwrap(),
            input: None,
            sample_rate: 48_000,
            started: Instant::now(),
            frames: 0,
            dropped: 0,
            tap_dropped_seen: 0,
            scratch: vec![0.5, -0.5],
        };
        recording.writer.request_close();
        let loss = 1 << 40;
        recording.submit(loss, 0, true).unwrap();
        assert!(recording.scratch.is_empty());
        assert_eq!(recording.scratch.capacity(), 1 << 16);
        // Preserve the old attempted/drop totals, not a unique-gap metric.
        assert_eq!(
            (recording.frames, recording.dropped),
            (loss + 1, 2 * loss + 1)
        );
        let status = recording.writer.finish();
        assert_eq!(status.frames, 0);
        assert_eq!(status.final_signal.unwrap().sample_count, 0);
    }

    #[test]
    fn dropping_the_engine_flushes_its_pending_recording_tail() {
        let directory = tempfile::tempdir_in(env!("CARGO_MANIFEST_DIR")).unwrap();
        let path = directory.path().join("drop-tail.wav");
        let mut held = HeldRecording::new(path.clone(), 48_000, false, false);
        held.wait_for_capture_callback();
        assert_eq!(
            held.engine
                .recording
                .as_ref()
                .unwrap()
                .writer
                .status()
                .frames,
            0
        );
        assert!(held.engine.request_recording_close());
        let Some(ClosingRecording::Tap { target, .. }) = held.engine.closing_recording.as_mut()
        else {
            panic!("closure discarded the pending tap");
        };
        *target = 0;
        // HeldRecording releases the writer gate before dropping the engine.
        // This covers destruction during pending close. With no wall padding,
        // only this callback's retained samples can make the file nonempty.
        drop(held);
        let bytes = std::fs::read(path).unwrap();
        let data_bytes = u32::from_le_bytes(bytes[40..44].try_into().unwrap()) as usize;
        assert!(
            data_bytes > 0,
            "engine destruction lost the pending tap tail"
        );
        assert_eq!(data_bytes % 6, 0);
        assert_eq!(bytes.len(), 44 + data_bytes);
    }

    #[test]
    fn pending_tap_close_retains_ownership_until_the_final_handoff() {
        let directory = tempfile::tempdir_in(env!("CARGO_MANIFEST_DIR")).unwrap();
        let path = directory.path().join("pending-tap.wav");
        let mut held = HeldRecording::new(path.clone(), 48_000, false, false);
        held.wait_for_capture_callback();
        assert!(held.engine.request_recording_close());
        let Some(ClosingRecording::Tap { target, .. }) = held.engine.closing_recording.as_mut()
        else {
            panic!("closure discarded the pending tap");
        };
        *target = 0;
        for _ in 0..3 {
            // The audio tests hold an actual write guard. Here the predicate
            // isolates Studio's ownership while that retirement is pending.
            held.engine.advance_recording_close_with(|_| false);
            let Some(ClosingRecording::Tap {
                recording,
                target,
                has_output,
                flush,
            }) = held.engine.closing_recording.as_ref()
            else {
                panic!("pending retirement reached the writer");
            };
            assert_eq!(*target, 0);
            assert!(*has_output && *flush);
            assert_eq!(
                (
                    recording.frames,
                    recording.dropped,
                    recording.tap_dropped_seen
                ),
                (0, 0, 0)
            );
            assert_eq!(recording.writer.status().frames, 0);
            assert!(
                held.engine
                    .live
                    .as_ref()
                    .unwrap()
                    .device
                    .owns_record_capture(&recording.input.as_ref().unwrap().capture)
            );
        }
        let refused = directory.path().join("refused.wav");
        assert!(held.engine.start_recording(refused.clone()).is_err());
        assert!(!refused.exists());

        held.engine.session.transport().stop();
        drop(held.engine.live.take());
        held.engine.advance_recording_close();
        let Some(ClosingRecording::Writer(writer)) = held.engine.closing_recording.as_ref() else {
            panic!("joined output did not retire its tap");
        };
        let frames = writer.status().frames;
        assert!(frames > 0, "the retained tap tail was lost");
        held.wait_until_held();
        for _ in 0..3 {
            assert_eq!(held.engine.try_finish_recording(), None);
            let Some(ClosingRecording::Writer(writer)) = held.engine.closing_recording.as_ref()
            else {
                panic!("writer ownership disappeared");
            };
            assert_eq!(writer.status().frames, frames);
        }
        held.release_and_wait();
        let status = held.engine.try_finish_recording().unwrap();
        assert_eq!(status.path, path);
        assert_eq!(status.error, None);
        assert_eq!(status.frames, frames);
        assert_eq!(status.final_signal.unwrap().sample_count, frames * 2);
        assert_eq!(held.engine.try_finish_recording(), None);
    }

    #[test]
    fn recording_rebinds_only_after_the_previous_output_tap_is_drained() {
        let directory = tempfile::tempdir_in(env!("CARGO_MANIFEST_DIR")).unwrap();
        let mut held =
            HeldRecording::new(directory.path().join("rebind.wav"), 48_000, false, false);
        held.wait_for_capture_callback();
        let mut live = held.engine.live.take().unwrap();
        drop(live.device);
        live.device = LiveScalarDevice::start_silent(48_000, held.engine.generation()).unwrap();
        held.engine
            .session
            .bind_audio_confirmations(live.device.confirmations())
            .expect("bind replacement output confirmations");
        held.engine.live = Some(live);
        assert!(
            !held
                .engine
                .live
                .as_ref()
                .unwrap()
                .device
                .owns_record_capture(
                    &held
                        .engine
                        .recording
                        .as_ref()
                        .unwrap()
                        .input
                        .as_ref()
                        .unwrap()
                        .capture
                )
        );
        // A future test-owned start keeps the wall-padding target at zero.
        // Check it after the pump too, so elapsed time cannot satisfy the
        // retained-PCM assertion if the old tap was not drained.
        held.engine.recording.as_mut().unwrap().started = Instant::now() + Duration::from_secs(60);
        held.engine.pump_recording();
        let recording = held.engine.recording.as_ref().unwrap();
        assert_eq!(recording.target_frames(), 0);
        assert!(recording.frames > 0, "old output tail was not submitted");
        assert_eq!(recording.dropped, 0);
        assert_eq!(recording.tap_dropped_seen, 0);
        assert!(
            held.engine
                .live
                .as_ref()
                .unwrap()
                .device
                .owns_record_capture(&recording.input.as_ref().unwrap().capture)
        );
        assert!(held.engine.request_recording_close());
        held.wait_until_held();
        held.release_and_wait();
        assert_eq!(held.engine.try_finish_recording().unwrap().error, None);
    }

    #[test]
    fn changed_output_rate_flushes_only_the_previous_capture() {
        let directory = tempfile::tempdir_in(env!("CARGO_MANIFEST_DIR")).unwrap();
        let path = directory.path().join("changed-rate-tail.wav");
        let mut held = HeldRecording::new(path.clone(), 48_000, false, false);
        held.wait_for_capture_callback();
        let mut live = held.engine.live.take().unwrap();
        drop(live.device);
        live.device = LiveScalarDevice::start_silent(44_100, held.engine.generation()).unwrap();
        held.engine
            .session
            .bind_audio_confirmations(live.device.confirmations())
            .expect("bind replacement output confirmations");
        held.engine.live = Some(live);
        assert!(held.engine.request_recording_close());
        let Some(ClosingRecording::Tap {
            recording, target, ..
        }) = held.engine.closing_recording.as_mut()
        else {
            panic!("rate change discarded the pending tap");
        };
        assert_eq!(recording.sample_rate, 48_000);
        assert_eq!(recording.input.as_ref().unwrap().sample_rate, 48_000);
        assert_eq!(
            recording.input.as_ref().unwrap().capture.dropped_frames(),
            Ok(0)
        );
        assert_eq!(recording.writer.status().frames, 0);
        *target = 0;
        assert!(
            !held
                .engine
                .live
                .as_ref()
                .unwrap()
                .device
                .owns_record_capture(&recording.input.as_ref().unwrap().capture)
        );
        assert!(held.engine.request_recording_close());
        held.engine.pump_recording();
        assert!(!held.engine.is_recording());
        let device = &held.engine.live.as_ref().unwrap().device;
        assert_eq!(device.sample_rate(), 44_100);
        assert!(!device.recording_enabled());
        assert_eq!(
            held.engine
                .pending_diagnostics
                .iter()
                .filter(|diagnostic| diagnostic.kind == "record")
                .count(),
            1
        );
        let Some(ClosingRecording::Writer(writer)) = held.engine.closing_recording.as_ref() else {
            panic!("old-rate tap did not reach its writer");
        };
        let frames = writer.status().frames;
        assert!(frames > 0, "old-rate tail was not submitted");
        held.wait_until_held();
        held.release_and_wait();
        let status = held.engine.try_finish_recording().unwrap();
        assert_eq!(status.path, path);
        assert_eq!(status.sample_rate, 48_000);
        assert_eq!(status.error, None);
        assert_eq!(status.frames, frames);
        assert_eq!(status.final_signal.unwrap().sample_count, frames * 2);
        assert_eq!(held.engine.try_finish_recording(), None);
        assert!(
            !held
                .engine
                .live
                .as_ref()
                .unwrap()
                .device
                .recording_enabled()
        );
    }

    #[test]
    fn closing_take_does_not_block_ticks_or_graceful_stop() {
        let directory = tempfile::tempdir_in(env!("CARGO_MANIFEST_DIR")).unwrap();
        let path = directory.path().join("closing.wav");
        let mut held = HeldRecording::new(path.clone(), 48_000, false, false);
        assert!(held.engine.request_recording_close());
        held.wait_until_held();
        assert!(!held.engine.is_recording());
        for _ in 0..2 {
            assert_eq!(held.engine.try_finish_recording(), None);
            assert!(!held.closing_finished());
            assert!(matches!(
                held.engine.tick_at(Duration::ZERO, accepted).unwrap(),
                StudioTick::Running { step: Some(_), .. }
            ));
        }
        let refused = directory.path().join("refused.wav");
        let generation = held.engine.generation();
        let stream = held.engine.live.as_ref().unwrap().device.stream_id();
        assert!(held.engine.start_recording(refused.clone()).is_err());
        assert!(!refused.exists());
        assert_eq!(held.engine.generation(), generation);
        assert_eq!(
            held.engine.live.as_ref().unwrap().device.stream_id(),
            stream
        );

        held.engine.request_stop();
        assert!(matches!(
            held.engine.tick_at(Duration::ZERO, accepted).unwrap(),
            StudioTick::Stopping
        ));
        assert!(matches!(
            held.engine
                .tick_at(
                    GRACEFUL_STOP_SILENCE_HOLD + Duration::from_millis(1),
                    accepted
                )
                .unwrap(),
            StudioTick::Stopped(_)
        ));
        assert!(!held.engine.is_playing());
        assert_eq!(held.engine.try_finish_recording(), None);
        assert!(!held.closing_finished());

        held.release_and_wait();
        let status = held.engine.try_finish_recording().unwrap();
        assert_eq!(status.path, path);
        assert_eq!(status.error, None);
        assert_eq!(status.final_signal.unwrap().nonfinite_count, 0);
        assert_eq!(held.engine.try_finish_recording(), None);
        assert_eq!(held.engine.stop_recording(), None);
    }

    /// `stop_recording` is the whole close (ask, wait, join), because the
    /// engine's Drop calls only it. A take still rolling at quit is asked
    /// to close, and its writer finishes.
    #[test]
    fn stop_recording_closes_a_take_nobody_asked_to_close() {
        let directory = tempfile::tempdir_in(env!("CARGO_MANIFEST_DIR")).unwrap();
        let mut held =
            HeldRecording::new(directory.path().join("rolling.wav"), 48_000, false, false);
        assert!(held.engine.is_recording());
        assert!(held.engine.closing_recording.is_none());
        held.gate.release();
        let status = held
            .engine
            .stop_recording()
            .expect("the take is asked to close, then joined");
        assert_eq!(status.error, None);
        assert!(status.final_signal.is_some());
        assert!(!held.engine.is_recording());
        assert_eq!(held.engine.stop_recording(), None);
    }

    /// A driver that never calls again does not block the close. The tap
    /// gets half a second, then the close returns `None` and keeps the
    /// closure. The close completes when the driver is back.
    #[test]
    fn stop_recording_gives_a_wedged_tap_half_a_second_then_returns() {
        let directory = tempfile::tempdir_in(env!("CARGO_MANIFEST_DIR")).unwrap();
        let mut held =
            HeldRecording::new(directory.path().join("wedged.wav"), 48_000, false, false);
        let started = Instant::now();
        assert_eq!(
            held.engine.stop_recording_with(|_| false),
            None,
            "a tap that never retires is not waited on forever"
        );
        let waited = started.elapsed();
        assert!(
            waited >= Duration::from_millis(450) && waited < Duration::from_secs(3),
            "the wait is bounded at about half a second: {waited:?}"
        );
        assert!(
            matches!(
                held.engine.closing_recording,
                Some(ClosingRecording::Tap { .. })
            ),
            "the closure is kept for later turns"
        );
        // The driver comes back: the same close now completes and the take
        // is on disk with its sizes.
        held.gate.release();
        let status = held
            .engine
            .stop_recording()
            .expect("once the tap retires the take is joined");
        assert_eq!(status.error, None);
        assert!(status.final_signal.is_some());
    }

    #[test]
    fn audio_stop_keeps_an_open_take_until_explicit_close() {
        let directory = tempfile::tempdir_in(env!("CARGO_MANIFEST_DIR")).unwrap();
        let mut held = HeldRecording::new(directory.path().join("open.wav"), 48_000, false, false);
        assert!(held.engine.stop(Duration::from_millis(200)).is_some());
        assert!(!held.engine.is_playing());
        assert!(held.engine.is_recording());
        assert!(held.engine.closing_recording.is_none());
        assert!(held.engine.request_recording_close());
        held.wait_until_held();
        held.gate.release();
        let status = held.engine.stop_recording().unwrap();
        assert_eq!(status.error, None);
        assert!(status.final_signal.is_some());
        assert_eq!(held.engine.stop_recording(), None);
    }

    #[test]
    fn rate_change_closes_once_without_waiting_for_the_writer() {
        let directory = tempfile::tempdir_in(env!("CARGO_MANIFEST_DIR")).unwrap();
        let mut held =
            HeldRecording::new(directory.path().join("old-rate.wav"), 44_100, false, false);
        let mut notices = 0;
        for _ in 0..2 {
            held.engine.tick_at(Duration::ZERO, |update| {
                    if matches!(&update, StudioUpdate::Diagnostic(diagnostic) if diagnostic.kind == "record") {
                        notices += 1;
                    }
                    Ok(())
                }).unwrap();
        }
        held.wait_until_held();
        assert_eq!(notices, 1);
        assert!(!held.engine.is_recording());
        assert!(
            !held
                .engine
                .live
                .as_ref()
                .unwrap()
                .device
                .recording_enabled()
        );
        assert_eq!(held.engine.try_finish_recording(), None);
        held.release_and_wait();
        let status = held.engine.try_finish_recording().unwrap();
        assert_eq!(status.sample_rate, 44_100);
        assert_eq!(status.error, None);
        assert!(status.final_signal.is_some());
    }
    #[cfg(test)]
    mod sample_recording_tests {
        use crate::engine::*;
        use rustel_audio::input::{INPUT_RING_FRAMES, InputRing};

        fn recording(ring: &Arc<InputRing>, path: std::path::PathBuf) -> SampleRecording {
            SampleRecording {
                writer: TakeWriter::start(path, 48_000).expect("writer"),
                cursor: ring.written(),
                ring: Arc::clone(ring),
                own_input: None,
                dropped: 0,
            }
        }

        /// A mono microphone lands on both sides of the stereo file, frame for
        /// frame, starting where the recording started and not before.
        #[test]
        fn a_mono_input_is_recorded_on_both_sides_from_the_press() {
            let directory = tempfile::tempdir().expect("temp dir");
            let ring = Arc::new(InputRing::new());
            ring.set_channels(1);
            ring.set_sample_rate(48_000);
            // Before the press: not in the sample.
            ring.write(&[0.9; 256], 1);
            let mut sample = recording(&ring, directory.path().join("sample.wav"));
            ring.write(&[0.5; 1_000], 1);
            sample.drain();
            ring.write(&[0.25; 500], 1);
            sample.drain();
            let status = sample.writer.finish();
            assert_eq!(status.error, None);
            assert_eq!(status.frames, 1_500);
            let signal = status.final_signal.expect("summary");
            assert_eq!(signal.sample_count, 3_000, "two channels a frame");
            assert!(
                (signal.finite_peak - 0.5).abs() < 1e-6,
                "the 0.9 before the press is not in it"
            );
            assert_eq!(sample.dropped, 0);
        }

        /// Recorded with the transport stopped, as a sample often is: the
        /// stopped turn drains the input like the playing one, so the file holds
        /// the whole take and not the last second the ring still had.
        #[test]
        fn a_sample_recorded_while_stopped_keeps_its_beginning() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut engine = StudioEngine::new(StudioConfig {
                output: Some("silent".into()),
                ..StudioConfig::default()
            })
            .expect("engine");
            let ring = Arc::new(InputRing::new());
            ring.set_channels(1);
            ring.set_sample_rate(48_000);
            engine.sample_recording = Some(recording(&ring, directory.path().join("sample.wav")));
            // Five seconds of input, a turn every tenth of one: four times what
            // the ring holds, so only draining on every stopped turn keeps it.
            for tenth in 0..50 {
                let level = if tenth == 0 { 0.75 } else { 0.25 };
                ring.write(&vec![level; 4_800], 1);
                engine.idle_turn(|_| Ok(()));
            }
            let status = engine
                .stop_sample_recording()
                .expect("a sample was recording");
            assert_eq!(status.error, None);
            assert_eq!(status.frames, 50 * 4_800, "every frame, from the first");
            let signal = status.final_signal.expect("summary");
            assert!(
                (signal.finite_peak - 0.75).abs() < 1e-6,
                "the loud first tenth is in the file"
            );
        }

        /// A pump that finds more than one chunk waiting takes all of it.
        #[test]
        fn one_drain_takes_everything_the_ring_holds() {
            let directory = tempfile::tempdir().expect("temp dir");
            let ring = Arc::new(InputRing::new());
            ring.set_channels(1);
            ring.set_sample_rate(48_000);
            let mut sample = recording(&ring, directory.path().join("sample.wav"));
            ring.write(&vec![0.5; 40_000], 1);
            sample.drain();
            assert_eq!(sample.cursor, ring.written());
            assert_eq!(sample.dropped, 0);
            assert_eq!(sample.writer.finish().frames, 40_000);
        }

        /// A reader that fell a whole ring behind counts what the writer came
        /// round over, rather than recording frames that are no longer those.
        #[test]
        fn frames_the_ring_wrote_over_are_counted_not_recorded() {
            let directory = tempfile::tempdir().expect("temp dir");
            let ring = Arc::new(InputRing::new());
            ring.set_channels(2);
            ring.set_sample_rate(48_000);
            let mut sample = recording(&ring, directory.path().join("sample.wav"));
            let burst = vec![0.1f32; (INPUT_RING_FRAMES + 4_096) * 2];
            ring.write(&burst, 2);
            while sample.cursor < ring.written() {
                sample.drain();
            }
            assert!(
                sample.dropped >= 4_096 + SAMPLE_RING_GUARD_FRAMES,
                "{}",
                sample.dropped
            );
            let status = sample.writer.finish();
            assert_eq!(
                status.frames + sample.dropped,
                (INPUT_RING_FRAMES + 4_096) as u64,
                "every frame is either on disk or counted lost"
            );
        }
    }
}
