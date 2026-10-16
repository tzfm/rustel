use super::*;

impl StudioEngine {
    /// Pad notices. Polling is a log note, so it does not toast.
    /// `watch_pads: false` leaves the process-global queue alone.
    pub(super) fn collect_gamepad_notices(&mut self) {
        if !self.config.watch_pads {
            return;
        }
        for notice in rustel_core::gamepad::take_notices() {
            let diagnostic = match notice {
                rustel_core::gamepad::Notice::Polling => {
                    StudioDiagnostic::note("gamepad", "gamepads: watching for pads")
                }
                rustel_core::gamepad::Notice::Connected { pad, name } => {
                    // A pad that went while a button was held leaves this
                    // slot's last-seen state stuck at that value; a fresh
                    // connect really does start at rest (`set_connected`
                    // clears it on the poller side), so the baseline is
                    // reset here too, or the first real read afterwards
                    // would misread the snap back to rest as a release.
                    #[cfg(feature = "gamepad")]
                    {
                        if let Some(seen) = self.gamepad_buttons_seen.get_mut(pad) {
                            *seen = [0.0; rustel_core::gamepad::BUTTONS];
                        }
                        if let Some(logged) = self.gamepad_axes_logged.get_mut(pad) {
                            *logged = [None; rustel_core::gamepad::AXES];
                        }
                    }
                    StudioDiagnostic::info("gamepad", format!("gamepad({pad}) connected: {name}"))
                }
                rustel_core::gamepad::Notice::Disconnected { pad, name } => StudioDiagnostic::info(
                    "gamepad",
                    format!("gamepad({pad}) disconnected: {name}"),
                ),
                rustel_core::gamepad::Notice::Problem(problem) => {
                    StudioDiagnostic::message("gamepad", problem)
                }
            };
            self.pending_diagnostics.push_back(diagnostic);
        }
        #[cfg(feature = "gamepad")]
        self.collect_gamepad_activity();
    }

    /// A pad's buttons and sticks, from turn to turn. A press or a release
    /// is a discrete event, like a MIDI key, and reaches the log at a level
    /// a reader sees. A stick position is not discrete: a sweep across its
    /// throw changes it on nearly every engine turn. So a stick reaches the
    /// log as [`StudioDiagnostic::trace`], and only after it moves past
    /// [`GAMEPAD_AXIS_LOG_THRESHOLD`] from the last value logged for it.
    /// `studio.log` records every line, `Debug` lines included, so the
    /// threshold keeps a held stick from filling the file. Both kinds also
    /// reach the mixer's feed beside MIDI.
    #[cfg(feature = "gamepad")]
    fn collect_gamepad_activity(&mut self) {
        for (slot, name) in rustel_core::gamepad::connected_pads() {
            let Some(pad) = rustel_core::gamepad::pad(slot) else {
                continue;
            };
            if let Some(seen) = self.gamepad_buttons_seen.get_mut(slot) {
                for (index, was) in seen.iter_mut().enumerate() {
                    let now = pad.button(index) as f32;
                    let verb = if *was < 0.5 && now >= 0.5 {
                        Some("pressed")
                    } else if *was >= 0.5 && now < 0.5 {
                        Some("released")
                    } else {
                        None
                    };
                    if let Some(verb) = verb {
                        let line = format!(
                            "gamepad({slot}) {name}: {} {verb}",
                            rustel_runtime::gamepad::slot_name(index)
                        );
                        rustel_core::gamepad::note_activity(line.clone());
                        queue_diagnostic(
                            &mut self.pending_diagnostics,
                            StudioDiagnostic::note("gamepad-activity", line),
                        );
                    }
                    *was = now;
                }
            }
            if let Some(logged) = self.gamepad_axes_logged.get_mut(slot) {
                for (axis, last) in logged.iter_mut().enumerate() {
                    let now = pad.axis(axis) as f32;
                    let moved_enough = match *last {
                        Some(previous) => (now - previous).abs() >= GAMEPAD_AXIS_LOG_THRESHOLD,
                        // Centre is not itself news: a stick at rest reads
                        // 0.0 and must not write a line the moment it is
                        // first seen.
                        None => now.abs() >= GAMEPAD_AXIS_LOG_THRESHOLD,
                    };
                    if moved_enough {
                        let line = format!(
                            "gamepad({slot}) {name}: {} {now:+.2}",
                            rustel_runtime::gamepad::AXIS_NAMES[axis]
                        );
                        rustel_core::gamepad::note_activity(line.clone());
                        queue_diagnostic(
                            &mut self.pending_diagnostics,
                            StudioDiagnostic::trace("gamepad-activity", line),
                        );
                        *last = Some(now);
                    }
                }
            }
        }
    }

    /// The mixer's orbit fader: stored, and on the live device at once.
    pub fn set_orbit_gain(&mut self, orbit: usize, gain: f32) {
        if let Some(slot) = self.orbit_gains.get_mut(orbit) {
            *slot = gain;
        }
        if let Some(live) = self.live.as_ref() {
            live.device.set_orbit_gain(orbit, gain);
        }
    }

    /// The audio input's fader: stored, and on the live device at once.
    pub fn set_input_gain(&mut self, gain: f32) {
        self.input_gain = gain;
        if let Some(live) = self.live.as_ref() {
            live.device.set_input_gain(gain);
        }
        if let Some(monitor) = self.input_monitor.as_ref() {
            monitor.set_gain(gain);
        }
    }

    /// Every fader onto a device: a new or recycled one opens at unity
    /// and has to be told what the mixer says.
    pub(super) fn apply_mix_gains(&self, device: &LiveScalarDevice) {
        for (orbit, gain) in self.orbit_gains.iter().enumerate() {
            if device.orbit_gain(orbit) != *gain {
                device.set_orbit_gain(orbit, *gain);
            }
        }
        if device.input_gain() != self.input_gain {
            device.set_input_gain(self.input_gain);
        }
    }

    /// Choose the audio input `s("in")` plays, or none. Opened on the live
    /// device at once when there is one, and again on every device that
    /// follows.
    /// Which notes the set has bound to scene launch. Forwarded to every
    /// live MIDI-input port, where the driver thread reads the set on each
    /// NoteOn: a launch pad's press never enters the musical ring, so the
    /// press-count watcher sees nothing and arms no requery - the launch
    /// lands alone, the way ctrl+enter does.
    pub fn set_launch_pads(&mut self, pads: Vec<(u8, u8)>) {
        // The bus keeps the set and stamps it onto every port it creates or
        // publishes later, so a keyboard a score names after this send is
        // born knowing its transport buttons.
        self.session.midi_input_bus().set_launch_pads(&pads);
        self.launch_pads = pads;
    }

    /// Choose the audio input that `s("in")` plays, or none. The next turn
    /// opens it, and the choice is kept across device switches.
    pub fn set_input_device(&mut self, name: Option<&str>) {
        let changed = self.preferred_input.as_deref() != name;
        self.preferred_input = name.map(str::to_owned);
        self.input_refused = None;
        self.input_attempts = 0;
        self.input_retry_at = Instant::now();
        // A different choice lets the old input go, wherever it was open:
        // the next turn opens the new one. (It used to stay open until the
        // device was rebuilt, so choosing another microphone did nothing.)
        if changed {
            self.close_input();
        }
    }

    /// Whether opening the input for `wanted` is news: the first time for
    /// this choice, yes; the handoff between the monitor and the live
    /// device, no - the microphone was open and metering all along.
    fn announce_input(&mut self, wanted: &str) -> bool {
        if self.input_announced.as_deref() == Some(wanted) {
            return false;
        }
        self.input_announced = Some(wanted.to_owned());
        true
    }

    /// Let go of the input wherever it is open.
    pub(super) fn close_input(&mut self) {
        if let Some(device) = self
            .live
            .as_mut()
            .map(|live| &mut live.device)
            .or(self.piano_output.as_mut())
        {
            device.close_input();
        }
        self.input_monitor = None;
        self.input_opened_for = None;
        self.sync_input_channels();
    }

    /// Only a healthy, open stream proves which channels are available. A
    /// disconnected or unopened input keeps the ordinary silent-source policy.
    pub(super) fn input_channels(&self) -> usize {
        if let Some(device) = self.audio_device()
            && device.input_name().is_some()
            && !device.input_failed()
        {
            return device.input_channels();
        }
        self.input_monitor
            .as_ref()
            .filter(|input| !input.failed())
            .map_or(0, |input| input.channels())
    }

    pub(super) fn sync_input_channels(&mut self) {
        let channels = self.input_channels();
        self.session
            .set_audio_input_channels((channels > 0).then_some(channels));
    }

    /// Keep the chosen input open. Delay retries after opening or stream failures.
    pub(super) fn ensure_input(&mut self) {
        self.ensure_input_with(
            Instant::now(),
            |device, monitor| match device {
                Some(device) => (device.input_name().is_some(), device.input_failed()),
                None => (
                    monitor.is_some(),
                    monitor.is_some_and(rustel_audio::AudioInput::failed),
                ),
            },
            Self::open_selected_input,
        );
    }

    fn ensure_input_with(
        &mut self,
        now: Instant,
        status: impl FnOnce(
            Option<&LiveScalarDevice>,
            Option<&rustel_audio::AudioInput>,
        ) -> (bool, bool),
        open_input: impl FnOnce(&mut Self, &str) -> Result<(String, usize), DevicePlaybackError>,
    ) {
        self.sync_input_channels();
        let Some(wanted) = self.preferred_input.clone() else {
            return;
        };
        let input_device = self.live.as_ref().map(|live| &live.device).or(self
            .piano_output
            .as_ref()
            .filter(|_| self.piano_has_score_tail));
        let (open, failed) = status(input_device, self.input_monitor.as_ref());
        if open && !failed && self.input_opened_for.as_deref() == Some(wanted.as_str()) {
            self.input_refused = None;
            self.input_attempts = 0;
            return;
        }
        let refused_before = self.input_refused.as_deref() == Some(&wanted);
        if open {
            self.close_input();
            if failed {
                self.defer_input_retry(wanted, now);
                if !refused_before {
                    self.pending_diagnostics
                        .push_back(StudioDiagnostic::message(
                            "input",
                            "audio in: the input stopped - will try again",
                        ));
                }
                return;
            }
        }
        // A new choice opens at once. A failed input waits for its retry.
        if refused_before && now < self.input_retry_at {
            return;
        }
        match open_input(self, &wanted) {
            Ok((name, channels)) => {
                // A later healthy observation clears the failure episode.
                if self.announce_input(&wanted) {
                    self.pending_diagnostics.push_back(StudioDiagnostic::info(
                        "input",
                        format!(
                            "audio in: {name} - {channels} channel(s); s(\"in\") plays channel 0, in:1 the next"
                        ),
                    ));
                }
                self.input_opened_for = Some(wanted);
            }
            Err(error) => {
                self.defer_input_retry(wanted, now);
                if !refused_before {
                    self.pending_diagnostics
                        .push_back(StudioDiagnostic::message(
                            "input",
                            format!("audio in: {error} - will keep trying"),
                        ));
                }
            }
        }
        self.sync_input_channels();
    }

    fn defer_input_retry(&mut self, wanted: String, now: Instant) {
        self.input_attempts = self.input_attempts.saturating_add(1);
        self.input_retry_at = now
            .checked_add(rustel_audio::input_retry_delay(self.input_attempts))
            .unwrap_or(now);
        self.input_refused = Some(wanted);
        self.input_announced = None;
    }

    fn open_selected_input(
        &mut self,
        wanted: &str,
    ) -> Result<(String, usize), DevicePlaybackError> {
        let input_device = self.live.as_mut().map(|live| &mut live.device).or(self
            .piano_output
            .as_mut()
            .filter(|_| self.piano_has_score_tail));
        match input_device {
            Some(device) => device
                .open_input(Some(wanted))
                .map(|name| (name, device.input_channels())),
            None => rustel_audio::AudioInput::open(
                Some(wanted),
                std::sync::Arc::new(rustel_audio::input::InputRing::new()),
                None,
            )
            .map(|input| {
                input.set_gain(self.input_gain);
                let facts = (input.name().to_owned(), input.channels());
                self.input_monitor = Some(input);
                facts
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::tests::*;
    mod input_retry {
        use super::*;

        #[derive(Clone, Copy)]
        enum InputStatus {
            Closed,
            Healthy,
            Failed,
        }

        struct InputHarness {
            engine: StudioEngine,
            live: bool,
            opens: Vec<String>,
        }

        impl InputHarness {
            fn new(live: bool) -> Self {
                let mut engine = if live {
                    silent_engine_for_output_selection()
                } else {
                    StudioEngine::new(StudioConfig::default()).expect("studio")
                };
                engine.set_input_device(Some("microphone"));
                engine.pending_diagnostics.clear();
                Self {
                    engine,
                    live,
                    opens: Vec::new(),
                }
            }

            fn turn(&mut self, now: Instant, status: InputStatus) {
                self.turn_with_result(now, status, Ok(()));
            }

            fn turn_with_result(
                &mut self,
                now: Instant,
                status: InputStatus,
                result: Result<(), DevicePlaybackError>,
            ) {
                let live = self.live;
                let opens = &mut self.opens;
                self.engine.ensure_input_with(
                    now,
                    |device, monitor| {
                        assert_eq!(device.is_some(), live, "live or idle input route");
                        assert!(monitor.is_none(), "the fixture opens no physical input");
                        match status {
                            InputStatus::Closed => (false, false),
                            InputStatus::Healthy => (true, false),
                            InputStatus::Failed => (true, true),
                        }
                    },
                    |_, wanted| {
                        opens.push(wanted.to_owned());
                        result.map(|()| (wanted.to_owned(), 2))
                    },
                );
            }

            fn refuse(&mut self, now: Instant) {
                self.turn_with_result(
                    now,
                    InputStatus::Closed,
                    Err(DevicePlaybackError::Unavailable("input busy".into())),
                );
            }
        }

        #[test]
        fn failed_live_and_idle_inputs_wait_before_reopening() {
            for live in [false, true] {
                let mut input = InputHarness::new(live);
                let now = Instant::now();
                input.turn(now, InputStatus::Closed);
                assert_eq!(input.opens, ["microphone"]);
                assert_eq!(input.engine.input_opened_for.as_deref(), Some("microphone"));

                input.engine.session.set_direct_diagnostic_logging(false);
                input.engine.session.set_audio_input_channels(Some(2));
                input
                    .engine
                    .session
                    .evaluate("s('in').n(3).fast(8)")
                    .expect("input score");
                input.turn(now, InputStatus::Failed);
                let deadline = now + Duration::from_millis(500);
                assert_eq!(input.opens.len(), 1, "failure must not reopen in this turn");
                assert_eq!(input.engine.input_opened_for, None, "failed input closed");
                assert_eq!(input.engine.input_announced, None);
                assert_eq!(input.engine.input_refused.as_deref(), Some("microphone"));
                assert_eq!(input.engine.input_attempts, 1);
                assert_eq!(input.engine.input_retry_at, deadline);
                assert_eq!(input.engine.snapshot().input_channels, 0);
                input.engine.session.transport().start();
                let events = input.engine.session.schedule_audio_at(0.0, 48_000).unwrap();
                assert!(!events.is_empty());
                assert!(events.iter().all(|event| matches!(
                    event.synth,
                    Some(rustel_audio::SynthSource::Input { channel: 3 })
                )));
                let diagnostics = input.engine.pending_diagnostics.len();
                assert_eq!(diagnostics, 2, "one opening and one failure message");
                assert!(
                    input.engine.pending_diagnostics[1]
                        .message
                        .contains("will try again")
                );

                for millis in [0, 2, 100, 499] {
                    input.turn(now + Duration::from_millis(millis), InputStatus::Closed);
                    assert_eq!(input.opens.len(), 1);
                    assert_eq!(input.engine.input_attempts, 1);
                    assert_eq!(input.engine.input_retry_at, deadline);
                    assert_eq!(input.engine.pending_diagnostics.len(), diagnostics);
                }
                input.turn(deadline, InputStatus::Closed);
                assert_eq!(input.opens, ["microphone", "microphone"]);
                assert_eq!(
                    input.engine.input_attempts, 1,
                    "opening is not a health check"
                );
                assert_eq!(input.engine.input_refused.as_deref(), Some("microphone"));
            }
        }

        #[test]
        fn repeated_stream_failures_increase_and_cap_the_delay() {
            for live in [false, true] {
                let mut input = InputHarness::new(live);
                let mut now = Instant::now();
                input.turn(now, InputStatus::Closed);
                for (index, millis) in [500, 1000, 2000, 4000, 8000, 16_000, 16_000, 16_000]
                    .into_iter()
                    .enumerate()
                {
                    now += Duration::from_millis(1);
                    input.turn(now, InputStatus::Failed);
                    let deadline = now + Duration::from_millis(millis);
                    assert_eq!(input.engine.input_attempts, (index + 1) as u64);
                    assert_eq!(input.engine.input_retry_at, deadline);
                    assert_eq!(input.opens.len(), index + 1);
                    let diagnostics = input.engine.pending_diagnostics.len();
                    assert_eq!(
                        input
                            .engine
                            .pending_diagnostics
                            .iter()
                            .filter(|diagnostic| diagnostic.message.contains("the input stopped"))
                            .count(),
                        1,
                        "report the first stream failure in the episode"
                    );

                    input.turn(deadline - Duration::from_nanos(1), InputStatus::Closed);
                    assert_eq!(input.opens.len(), index + 1);
                    assert_eq!(input.engine.pending_diagnostics.len(), diagnostics);
                    input.turn(deadline, InputStatus::Closed);
                    assert_eq!(input.opens.len(), index + 2);
                    assert_eq!(input.engine.input_attempts, (index + 1) as u64);
                    now = deadline;
                }
            }
        }

        #[test]
        fn opening_errors_and_stream_failures_share_the_failure_episode() {
            for live in [false, true] {
                let mut input = InputHarness::new(live);
                let now = Instant::now();
                input.refuse(now);
                assert_eq!(input.engine.input_attempts, 1);
                assert_eq!(input.engine.pending_diagnostics.len(), 1);
                assert!(
                    input.engine.pending_diagnostics[0]
                        .message
                        .contains("input busy")
                );
                let retry = input.engine.input_retry_at;
                input.refuse(retry - Duration::from_nanos(1));
                assert_eq!(input.opens.len(), 1);
                assert_eq!(input.engine.input_retry_at, retry);
                input.refuse(retry);
                assert_eq!(input.opens.len(), 2);
                assert_eq!(input.engine.input_attempts, 2);
                assert_eq!(input.engine.input_retry_at, retry + Duration::from_secs(1));
                assert_eq!(
                    input.engine.pending_diagnostics.len(),
                    1,
                    "no repeated refusal"
                );

                let retry = input.engine.input_retry_at;
                input.turn(retry, InputStatus::Closed);
                assert_eq!(input.engine.input_attempts, 2);
                input.turn(retry, InputStatus::Failed);
                assert_eq!(input.opens.len(), 3);
                assert_eq!(input.engine.input_attempts, 3);
                assert_eq!(input.engine.input_retry_at, retry + Duration::from_secs(2));
                assert_eq!(
                    input.engine.pending_diagnostics.len(),
                    2,
                    "only the first refusal and the successful opening are reported"
                );
            }
        }

        #[test]
        fn a_healthy_observation_resets_the_failure_episode() {
            for live in [false, true] {
                let mut input = InputHarness::new(live);
                let now = Instant::now();
                input.refuse(now);
                input.refuse(input.engine.input_retry_at);
                let retry = input.engine.input_retry_at;
                input.turn(retry, InputStatus::Closed);
                assert_eq!(input.engine.input_attempts, 2);
                let diagnostics = input.engine.pending_diagnostics.len();
                input.turn(retry, InputStatus::Healthy);
                assert_eq!(input.opens.len(), 3, "healthy input is not reopened");
                assert_eq!(input.engine.input_refused, None);
                assert_eq!(input.engine.input_attempts, 0);
                assert_eq!(input.engine.pending_diagnostics.len(), diagnostics);

                input.turn(retry, InputStatus::Failed);
                assert_eq!(input.engine.input_attempts, 1);
                assert_eq!(
                    input.engine.input_retry_at,
                    retry + Duration::from_millis(500)
                );
                assert_eq!(input.engine.pending_diagnostics.len(), diagnostics + 1);
                assert!(
                    input.engine.pending_diagnostics[diagnostics]
                        .message
                        .contains("the input stopped"),
                    "a healthy observation starts a new failure episode"
                );
            }
        }

        #[test]
        fn an_explicit_input_choice_can_retry_at_once() {
            for live in [false, true] {
                for wanted in ["microphone", "other microphone"] {
                    let mut input = InputHarness::new(live);
                    let now = Instant::now();
                    input.refuse(now);
                    let deadline = input.engine.input_retry_at;
                    input.engine.set_input_device(Some(wanted));
                    input.turn(now, InputStatus::Closed);
                    assert_eq!(input.opens, ["microphone", wanted]);
                    assert_eq!(input.engine.input_attempts, 0);
                    assert_eq!(input.engine.input_refused, None);
                    assert_eq!(input.engine.input_opened_for.as_deref(), Some(wanted));
                    assert!(now < deadline);

                    input.engine.set_input_device(None);
                    input.turn(now, InputStatus::Closed);
                    assert_eq!(input.opens.len(), 2, "no input means no opening attempt");
                    assert_eq!(input.engine.input_opened_for, None);
                }
            }
        }

        #[test]
        fn a_healthy_input_handoff_keeps_the_first_attempt_immediate() {
            for audition_tail in [false, true] {
                let mut input = InputHarness::new(false);
                let now = Instant::now();
                input.turn(now, InputStatus::Closed);
                input.turn(now, InputStatus::Healthy);
                let diagnostics = input.engine.pending_diagnostics.len();

                input.engine.close_input();
                play_silently(&mut input.engine);
                input.live = true;
                input.turn(now, InputStatus::Closed);
                assert_eq!(input.opens.len(), 2);
                assert_eq!(input.engine.input_attempts, 0);
                assert_eq!(input.engine.pending_diagnostics.len(), diagnostics);

                input.engine.prepare_piano().unwrap();
                let stream = input.engine.device_report().unwrap().stream_id;
                if audition_tail {
                    input.engine.detach_stopped_output_for_piano();
                    input.engine.service_idle_piano();
                    assert_eq!(input.engine.input_opened_for.as_deref(), Some("microphone"));
                    input.engine.piano_tail_silent_since =
                        Some(Instant::now() - GRACEFUL_STOP_SILENCE_HOLD);
                    input.engine.service_idle_piano();
                } else {
                    input.engine.request_stop();
                    input.engine.prepare_piano().unwrap();
                    assert!(matches!(
                        input.engine.advance_graceful_stop(Duration::ZERO),
                        StudioTick::Stopping
                    ));
                    assert!(matches!(
                        input
                            .engine
                            .advance_graceful_stop(GRACEFUL_STOP_SILENCE_HOLD),
                        StudioTick::Idle
                    ));
                }
                assert!(input.engine.live.is_none());
                assert!(input.engine.input_opened_for.is_none());
                assert!(!input.engine.piano_has_score_tail);
                assert_eq!(input.engine.device_report().unwrap().stream_id, stream);
                input.live = false;
                input.turn(now, InputStatus::Closed);
                input.turn(now, InputStatus::Healthy);
                assert_eq!(input.opens.len(), 3);
                assert_eq!(input.engine.input_attempts, 0);
                assert_eq!(input.engine.pending_diagnostics.len(), diagnostics);
            }
        }

        #[test]
        fn a_handoff_preserves_a_pending_input_retry() {
            let mut input = InputHarness::new(false);
            let now = Instant::now();
            input.turn(now, InputStatus::Closed);
            input.turn(now, InputStatus::Failed);
            let deadline = input.engine.input_retry_at;

            play_silently(&mut input.engine);
            input.live = true;
            input.turn(now, InputStatus::Closed);
            assert_eq!(input.opens.len(), 1);
            assert_eq!(input.engine.input_attempts, 1);
            assert_eq!(input.engine.input_retry_at, deadline);
            input.turn(deadline, InputStatus::Closed);
            assert_eq!(input.opens.len(), 2);
            assert_eq!(input.engine.input_attempts, 1);
        }
    }

    /// The input is announced once for a choice: the handoff between its
    /// monitor and the live device is the same microphone, and not news;
    /// another choice is, and so is the first coming back.
    #[test]
    fn the_input_is_announced_once_per_choice() {
        let mut engine = StudioEngine::new(StudioConfig::default()).expect("studio");
        assert!(engine.announce_input("iPhone"), "the first open is news");
        assert!(
            !engine.announce_input("iPhone"),
            "the same input on the live device is not"
        );
        assert!(engine.announce_input("Scarlett"), "another choice is");
        assert!(engine.announce_input("iPhone"), "and the first, back again");
        engine.input_announced = None;
        assert!(
            engine.announce_input("iPhone"),
            "an input that died and came back is"
        );
    }

    #[test]
    fn no_open_input_clears_obsolete_channel_validation() {
        for close in [false, true] {
            let mut engine = silent_engine_for_output_selection();
            engine.session.set_direct_diagnostic_logging(false);
            engine.session.set_audio_input_channels(Some(2));
            engine
                .session
                .evaluate("s('in').n(3).fast(8)")
                .expect("input score");
            if close {
                engine.close_input();
            } else {
                engine.ensure_input();
            }
            assert_eq!(engine.snapshot().input_channels, 0);
            let events = engine.session.schedule_audio_at(0.0, 48_000).unwrap();
            assert!(!events.is_empty());
            assert!(events.iter().all(|event| matches!(
                event.synth,
                Some(rustel_audio::SynthSource::Input { channel: 3 })
            )));
        }
    }

    /// A pad coming or going reaches the studio as a diagnostic: the log
    /// and the status line say so, the way they do for an audio input.
    #[test]
    fn a_pad_arriving_or_going_is_a_diagnostic() {
        let mut engine = silent_engine_for_output_selection();
        let _ = rustel_core::gamepad::take_notices();
        engine.pending_diagnostics.clear();
        rustel_core::gamepad::notice(rustel_core::gamepad::Notice::Connected {
            pad: 1,
            name: "Test Pad".to_owned(),
        });
        rustel_core::gamepad::notice(rustel_core::gamepad::Notice::Disconnected {
            pad: 1,
            name: "Test Pad".to_owned(),
        });
        engine.collect_gamepad_notices();
        let lines: Vec<(String, StudioDiagnosticLevel)> = engine
            .pending_diagnostics
            .iter()
            .filter(|diagnostic| diagnostic.kind == "gamepad")
            .map(|diagnostic| (diagnostic.message.clone(), diagnostic.level))
            .collect();
        assert_eq!(
            lines,
            vec![
                (
                    "gamepad(1) connected: Test Pad".to_owned(),
                    StudioDiagnosticLevel::Info
                ),
                (
                    "gamepad(1) disconnected: Test Pad".to_owned(),
                    StudioDiagnosticLevel::Info
                ),
            ]
        );
        assert!(
            rustel_core::gamepad::take_notices().is_empty(),
            "taken, not left for the next turn"
        );
    }

    /// A button crossing half is discrete news, the way a MIDI key landing
    /// is: it reaches the log at a level a reader sees, in the CLI
    /// monitor's own wording, and lands in the mixer's feed beside MIDI's.
    #[cfg(feature = "gamepad")]
    #[test]
    fn a_pads_button_reaches_the_log_the_way_a_midi_note_does() {
        let _guard = GAMEPAD_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let pad = reset_test_pad_0();
        rustel_core::gamepad::set_name(0, Some("Test Pad Buttons".to_owned()));
        pad.set_connected(true);
        let mut engine = silent_engine_for_output_selection();
        engine.pending_diagnostics.clear();

        pad.set_button(0, 1.0);
        engine.collect_gamepad_notices();
        let pressed: Vec<(String, StudioDiagnosticLevel)> = engine
            .pending_diagnostics
            .iter()
            .filter(|diagnostic| diagnostic.kind == "gamepad-activity")
            .map(|diagnostic| (diagnostic.message.clone(), diagnostic.level))
            .collect();
        assert_eq!(
            pressed,
            vec![(
                "gamepad(0) Test Pad Buttons: a pressed".to_owned(),
                StudioDiagnosticLevel::Note
            )]
        );
        assert!(
            rustel_core::gamepad::recent_activity()
                .contains(&"gamepad(0) Test Pad Buttons: a pressed".to_owned()),
            "the mixer's own feed sees the press too"
        );

        engine.pending_diagnostics.clear();
        pad.set_button(0, 0.0);
        engine.collect_gamepad_notices();
        let released: Vec<(String, StudioDiagnosticLevel)> = engine
            .pending_diagnostics
            .iter()
            .filter(|diagnostic| diagnostic.kind == "gamepad-activity")
            .map(|diagnostic| (diagnostic.message.clone(), diagnostic.level))
            .collect();
        assert_eq!(
            released,
            vec![(
                "gamepad(0) Test Pad Buttons: a released".to_owned(),
                StudioDiagnosticLevel::Note
            )]
        );
        pad.set_connected(false);
    }

    /// `GAMEPAD_AXIS_LOG_THRESHOLD` coalesces a full stick sweep to a few
    /// lines in `studio.log`. Each line is at `Trace`, which the log panel
    /// does not show by default.
    #[cfg(feature = "gamepad")]
    #[test]
    fn a_sticks_continuous_move_does_not_flood_the_log() {
        let _guard = GAMEPAD_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let pad = reset_test_pad_0();
        rustel_core::gamepad::set_name(0, Some("Test Pad Stick".to_owned()));
        pad.set_connected(true);
        let mut engine = silent_engine_for_output_selection();
        engine.pending_diagnostics.clear();

        // A slow, steady sweep across the whole throw: the shape a musician
        // leaning on a stick actually sends, sampled once a turn.
        const SAMPLES: usize = 200;
        let mut seen = Vec::new();
        for step in 0..=SAMPLES {
            let value = -1.0 + 2.0 * (step as f32) / (SAMPLES as f32);
            pad.set_axis(0, value);
            engine.collect_gamepad_notices();
            emit_pending_diagnostics(&mut engine.pending_diagnostics, &mut |update| {
                if let StudioUpdate::Diagnostic(diagnostic) = update {
                    seen.push(diagnostic);
                }
                Ok(())
            });
        }
        let axis_lines: Vec<&StudioDiagnostic> = seen
            .iter()
            .filter(|diagnostic| diagnostic.kind == "gamepad-activity")
            .collect();
        assert!(
            !axis_lines.is_empty(),
            "the sweep crossed the threshold at least once"
        );
        assert!(
            axis_lines.len() < SAMPLES / 4,
            "a full sweep coalesced to a handful of lines, not one a sample: {}",
            axis_lines.len()
        );
        assert!(
            axis_lines
                .iter()
                .all(|diagnostic| diagnostic.level == StudioDiagnosticLevel::Trace),
            "a stick's motion never reaches a level the log panel shows by default: {axis_lines:?}"
        );
        pad.set_connected(false);
    }

    /// A pad that is connected but nobody is touching must cost the log
    /// nothing: the studio watches every pad from launch, and a controller
    /// left switched on all evening must not write to `studio.log` for
    /// doing nothing.
    #[cfg(feature = "gamepad")]
    #[test]
    fn an_idle_connected_pad_writes_no_activity() {
        let _guard = GAMEPAD_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let pad = reset_test_pad_0();
        rustel_core::gamepad::set_name(0, Some("Test Pad Idle".to_owned()));
        pad.set_connected(true);
        let mut engine = silent_engine_for_output_selection();
        engine.pending_diagnostics.clear();

        for _ in 0..20 {
            engine.collect_gamepad_notices();
        }
        assert!(
            engine
                .pending_diagnostics
                .iter()
                .all(|diagnostic| diagnostic.kind != "gamepad-activity"),
            "a pad at rest wrote nothing: {:?}",
            engine.pending_diagnostics
        );
        pad.set_connected(false);
    }
}
