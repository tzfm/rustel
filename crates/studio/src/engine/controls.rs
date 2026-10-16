use super::*;

impl StudioEngine {
    /// Move one of the running score's sliders.
    ///
    /// The cell the pattern reads is updated in place and the active graph
    /// is re-queried under a fresh generation, so events already sent to
    /// the device before the takeover keep sounding while everything after
    /// it carries the new value - the same path a MIDI-mapped control takes
    /// in the CLI. Nothing here evaluates source.
    pub fn set_slider(&mut self, id: &str, value: f64) -> Result<(), RuntimeError> {
        self.set_slider_with_transition(id, value, false)
    }

    /// Commit the exact cell value and ramp eligible sustained gain, cutoff
    /// and resonance.
    pub fn set_slider_smoothed(&mut self, id: &str, value: f64) -> Result<(), RuntimeError> {
        self.set_slider_with_transition(id, value, true)
    }

    pub(crate) fn set_slider_with_transition(
        &mut self,
        id: &str,
        value: f64,
        smooth: bool,
    ) -> Result<(), RuntimeError> {
        if !value.is_finite() {
            return Err(RuntimeError::Message(format!(
                "slider {id} was given a non-finite value"
            )));
        }
        let Some(slider) = self.ui.layout.slider(id) else {
            return Err(RuntimeError::Message(format!(
                "slider {id} is not part of the running score"
            )));
        };
        if value < slider.min || value > slider.max {
            return Err(RuntimeError::Message(format!(
                "slider {id} value {value} is outside {}..{}",
                slider.min, slider.max
            )));
        }
        self.session.consume_audio_confirmations();
        let device_time = self.device_time();
        let result = self
            .session
            .with_panic_recovery(device_time, |session| session.set_slider_value(id, value));
        self.sync_recovered_session();
        match result? {
            true => self.ui.layout.set_slider_value(id, value),
            false => {
                return Err(RuntimeError::Message(format!(
                    "the running score refused slider {id}"
                )));
            }
        }
        if self.is_stopping() || self.session.transport().is_stopped() {
            return Ok(());
        }
        if let Some(binding) = self.session.slider_binding(id)
            && (value as f32).is_finite()
        {
            let update = rustel_audio::live_control::LiveControlUpdate {
                binding,
                value: value as f32,
                smooth,
            };
            if let Some(pending) = self
                .pending_live_controls
                .iter_mut()
                .find(|pending| pending.binding == binding)
            {
                *pending = update;
            } else {
                self.pending_live_controls.push(update);
            }
            self.flush_live_controls();
        }
        let now = Instant::now();
        let due = self
            .slider_requery_at
            .is_none_or(|at| now.duration_since(at) >= SLIDER_REQUERY_INTERVAL);
        if due {
            self.requery_for_sliders(now)?;
        } else {
            self.slider_requery_pending = true;
        }
        Ok(())
    }

    /// Bring the sliders' current values forward into the queried horizon:
    /// one generation flip, one takeover seam.
    pub(super) fn requery_for_sliders(&mut self, now: Instant) -> Result<(), RuntimeError> {
        self.sync_input_channels();
        self.slider_requery_pending = false;
        self.slider_requery_at = Some(now);
        let Some(live) = self.live.as_mut() else {
            return Ok(());
        };
        // A start's first query has not run: it reads the slider as it
        // stands, and a requery now would move cycle zero off the start.
        if live.initial_start_generation.is_some() {
            return Ok(());
        }
        if let Some((generation_before, generation_after)) = self
            .session
            .requery_active_at(live.device.clock_seconds())?
        {
            queue_diagnostic(
                &mut self.pending_diagnostics,
                // A knob moving requeries about eight times a second, so
                // this is the machinery talking to itself: `Debug`, where
                // it is there for the reader who goes looking and nowhere
                // near the reader who does not.
                StudioDiagnostic::trace(
                    "sliders",
                    format!("slider requery {generation_before}→{generation_after}"),
                ),
            );
            live.producer
                .arm_control_requery(generation_before, generation_after);
        }
        Ok(())
    }

    /// A slider move that arrived inside the interval gets its re-query
    /// once the interval has passed, so the last value of a drag always
    /// lands in the horizon.
    pub(super) fn flush_slider_requery(&mut self) {
        if !self.slider_requery_pending
            || self.is_stopping()
            || self.session.transport().is_stopped()
            || self.live.is_none()
        {
            return;
        }
        let now = Instant::now();
        if self
            .slider_requery_at
            .is_none_or(|at| now.duration_since(at) >= SLIDER_REQUERY_INTERVAL)
            && let Err(error) = self.requery_for_sliders(now)
        {
            self.pending_diagnostics
                .push_back(StudioDiagnostic::message("slider", error.to_string()));
        }
    }

    pub(super) fn flush_live_controls(&mut self) {
        if self.is_stopping() || self.session.transport().is_stopped() {
            self.pending_live_controls.clear();
            return;
        }
        let Some(live) = self.live.as_mut() else {
            self.pending_live_controls.clear();
            return;
        };
        self.pending_live_controls
            .retain(|update| !live.device.try_set_live_control(*update));
    }

    // ---- clocks ----------------------------------------------------------

    /// Send MIDI clock to a port, or to none.
    pub fn set_clock_out(&mut self, port: Option<&str>) -> Result<(), String> {
        if let Some(mut previous) = self.clock_out.take() {
            previous.advance(false, 0.0, 1.0, Instant::now());
        }
        if let Some(port) = port {
            self.clock_out = Some(ClockOut::open(port)?);
        }
        Ok(())
    }

    /// Say which ports may be opened from here on.
    ///
    /// Changing this does not close what is already open: a port that was
    /// allowed when the set started keeps its sender until the set ends,
    /// because pulling a live output mid-phrase leaves whatever it was
    /// playing stuck on. It decides what may be opened NEXT, and the
    /// refusals already said are forgotten so a port turned back on can
    /// report its own trouble again.
    pub fn set_midi_enablement(&mut self, enabled: crate::devices::MidiEnablement) {
        if self.midi_enabled != enabled {
            self.midi_disabled_reported.clear();
        }
        self.midi_enabled = enabled;
    }

    /// Follow MIDI clock from a port, or from none.
    pub fn set_clock_in(&mut self, port: Option<&str>) -> Result<(), String> {
        self.clock_in = None;
        self.clock_locked = false;
        self.clock_external_cps = None;
        if let Some(port) = port {
            self.clock_in = Some(ClockIn::open(port)?);
        }
        Ok(())
    }

    /// The MIDI clock out this tick: transport news, then the ticks due
    /// ahead on the scheduler's grid.
    pub(super) fn advance_clock_out(&mut self) {
        let Some(out) = self.clock_out.as_mut() else {
            return;
        };
        let playing = self
            .live
            .as_ref()
            .is_some_and(|live| live.draining.is_none())
            && !self.session.transport().is_stopped();
        let (cycle_now, cps) = match self.live.as_ref() {
            Some(live) => {
                let now = live.device.clock_seconds();
                (self.session.cycle_at_time(now), self.session.cps())
            }
            None => (0.0, self.session.cps()),
        };
        out.advance(playing, cycle_now, cps, Instant::now());
    }

    /// The MIDI clock in, unless Link is being followed instead.
    pub(super) fn follow_clock_in(&mut self) {
        let Some(clock) = self.clock_in.as_ref() else {
            return;
        };
        let now_instant = Instant::now();
        let Some(estimate) = clock.estimate(now_instant) else {
            self.clock_locked = false;
            self.clock_external_cps = None;
            return;
        };
        self.clock_external_cps = Some(estimate.cps);
        self.steer_to(estimate, now_instant);
    }

    /// Steer the scheduler onto an outside clock: bend the tempo a little
    /// towards it while the phase is close, jump when it is far, and
    /// re-query so the device hears the change. The arithmetic itself is
    /// shared with the live command, so both follow a clock identically.
    pub(super) fn steer_to(&mut self, estimate: ClockEstimate, now_instant: Instant) {
        if !estimate.running
            || now_instant.duration_since(self.clock_followed_at) < midi_clock::FOLLOW_INTERVAL
        {
            return;
        }
        self.clock_followed_at = now_instant;
        if self.is_stopping() || self.session.transport().is_stopped() {
            return;
        }
        let Some(live) = self.live.as_mut() else {
            return;
        };
        let now = live.device.clock_seconds();
        let (steer, locked) = midi_clock::steer(
            estimate,
            self.session.cycle_at_time(now),
            self.session.cps(),
        );
        self.clock_locked = locked;
        let midi_clock::Steer::To { cps, cycle } = steer else {
            return;
        };
        self.session.retime(now, cps, cycle);
        if let Ok(Some((before, after))) = self.session.requery_active_at(now) {
            queue_diagnostic(
                &mut self.pending_diagnostics,
                // The same, once per steer while an outside clock leads.
                StudioDiagnostic::trace("clock", format!("clock steer requery {before}→{after}")),
            );
            live.producer.arm_control_requery(before, after);
        }
    }

    // ---- orbits ----------------------------------------------------------

    /// The orbits heard in the last few seconds, with their held peaks.
    pub(super) fn orbit_levels(&self) -> Vec<OrbitLevel> {
        let now = Instant::now();
        self.orbit_last_heard
            .iter()
            .enumerate()
            .filter(|(_, heard)| heard.is_some_and(|at| now.duration_since(at) < ORBIT_LINGER))
            .map(|(orbit, _)| OrbitLevel {
                orbit: orbit as u8,
                peak: self.orbit_peak[orbit],
                pair: self.orbit_routing[orbit],
            })
            .collect()
    }

    /// Send an orbit to an output pair; remembered across devices.
    pub fn route_orbit(&mut self, orbit: usize, pair: u8) {
        if let Some(slot) = self.orbit_routing.get_mut(orbit) {
            *slot = pair;
        }
        if let Some(live) = self.live.as_ref() {
            live.device.set_orbit_output(orbit, pair);
        }
    }

    /// Read the orbit meters off the device and keep the routing applied
    /// to whichever device is open.
    pub(super) fn observe_orbits(&mut self) {
        let Some(live) = self.live.as_ref() else {
            return;
        };
        if live.device.orbit_outputs() != self.orbit_routing {
            for (orbit, pair) in self.orbit_routing.iter().enumerate() {
                live.device.set_orbit_output(orbit, *pair);
            }
        }
        let levels = live.device.take_orbit_levels();
        let now = Instant::now();
        for (orbit, level) in levels.iter().enumerate() {
            if *level > 0.0005 {
                self.orbit_last_heard[orbit] = Some(now);
            }
            self.orbit_peak[orbit] = level.max(self.orbit_peak[orbit] * ORBIT_PEAK_DECAY);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::tests::*;
    mod link {
        use super::*;

        /// Joining founds the session where the score stands, a tempo the
        /// session already runs is not published again, and one a fresh
        /// evaluation settled on is.
        #[test]
        fn joining_founds_the_session_on_the_scores_tempo_and_publishes_only_a_change() {
            // The assertions here are bookkeeping, so the session stays
            // off the wire: a plain test run must not put a socket the
            // OS would ask its consent to.
            rustel_runtime::link::keep_the_session_off_the_wire();
            let mut engine = StudioEngine::new(StudioConfig {
                output: Some("silent".into()),
                ..StudioConfig::default()
            })
            .expect("studio");
            assert!(engine.link.is_none(), "out of the session at start");

            engine.set_link(true).expect("join");
            let status = || engine.link.as_ref().expect("joined").status();
            let founded = midi_clock::bpm(engine.session.cps());
            assert!(
                (status().bpm - founded).abs() <= 0.05,
                "founded where the score stands: {}",
                status().bpm
            );

            // The tempo the session runs is ours already: publishing must
            // not move it, and must not say anything that would.
            engine.publish_link_tempo();
            assert!(
                (status().bpm - founded).abs() <= 0.05,
                "an agreeing tempo is not sent: {}",
                status().bpm
            );

            // A fresh evaluation settles on a different tempo, and the
            // session is brought to it.
            engine
                .session
                .reload_at("setcpm(150/4); $: s(\"bd\")", false, 0.0)
                .expect("tempo change");
            let moved = midi_clock::bpm(engine.session.cps());
            assert!(moved > founded + 1.0, "the score really moved: {moved}");
            engine.publish_link_tempo();
            let deadline = Instant::now() + Duration::from_secs(5);
            while (status().bpm - moved).abs() > 0.05 && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(10));
            }
            assert!(
                (status().bpm - moved).abs() <= 0.05,
                "the session follows the new tempo: {}",
                status().bpm
            );

            // Leaving gives the session up, and joining an open session
            // again changes nothing.
            engine.set_link(false).expect("leave");
            assert!(engine.link.is_none(), "the session is given up");
        }
    }

    /// The limiter reaches the device on an ordinary turn, with the gain,
    /// and not only on the rebuild after an output change.
    #[test]
    fn the_limiter_reaches_the_device_on_an_ordinary_turn() {
        let mut engine = silent_engine_with_pattern(rustel_core::silence());
        let settings = rustel_audio::LimiterSettings {
            threshold_db: -9.8,
            character: rustel_audio::LimiterCharacter::Transparent,
        };
        engine.master.set_limiter(Some(settings));
        engine.tick(|_| Ok(())).expect("a turn");
        let live = engine.live.as_ref().expect("the silent device is live");
        assert_eq!(
            live.device.limiter(),
            Some(settings),
            "the ceiling never left the bus"
        );

        // And taking it off travels the same way.
        engine.master.set_limiter(None);
        engine.tick(|_| Ok(())).expect("another turn");
        let live = engine.live.as_ref().expect("still live");
        assert_eq!(live.device.limiter(), None);
    }

    #[test]
    fn max_polyphony_default_and_score_override_reach_the_live_device() {
        let mut engine = silent_engine_with_pattern(rustel_core::silence());
        engine.master.set_max_polyphony(192);
        engine.tick(|_| Ok(())).expect("host default update");
        assert_eq!(engine.session.max_polyphony(), 192);
        assert_eq!(engine.master.effective_max_polyphony(), 192);
        assert_eq!(engine.master.max_polyphony_override(), None);
        assert_eq!(engine.live.as_ref().unwrap().device.max_polyphony(), 192);
        engine
            .session
            .evaluate("setMaxPolyphony(4); s('sine')")
            .expect("score override");
        engine.tick(|_| Ok(())).expect("override update");
        assert_eq!(engine.live.as_ref().unwrap().device.max_polyphony(), 4);
        assert_eq!(engine.master.effective_max_polyphony(), 4);
        assert_eq!(engine.master.max_polyphony_override(), Some(4));
        engine.master.set_max_polyphony(256);
        assert!(
            engine
                .session
                .evaluate("setMaxPolyphony(32); throw new Error('no')")
                .is_err()
        );
        engine.tick(|_| Ok(())).expect("rejected override");
        assert_eq!(engine.live.as_ref().unwrap().device.max_polyphony(), 4);
        assert_eq!(engine.session.config().max_polyphony, 256);
        let replacement =
            LiveScalarDevice::start_silent(48_000, engine.generation()).expect("replacement");
        engine.live.as_mut().unwrap().device = replacement;
        engine.rearm_recycled_output().expect("rearm output");
        assert_eq!(engine.live.as_ref().unwrap().device.max_polyphony(), 4);
    }

    #[test]
    fn max_polyphony_host_default_stays_current_while_stopped() {
        let mut engine = StudioEngine::new(StudioConfig {
            session: SessionConfig::default().with_max_polyphony(192),
            default_samples: false,
            watch_pads: false,
            ..Default::default()
        })
        .expect("engine");
        assert_eq!(engine.master.max_polyphony(), 192);
        assert_eq!(engine.master.effective_max_polyphony(), 192);
        engine.master.set_max_polyphony(256);
        engine.idle_turn(|_| Ok(()));
        assert_eq!(engine.session.max_polyphony(), 256);
        assert_eq!(engine.master.effective_max_polyphony(), 256);
        assert_eq!(engine.master.max_polyphony_override(), None);
        engine
            .session
            .evaluate("setMaxPolyphony(256); s('sine')")
            .expect("score override matching default");
        engine.idle_turn(|_| Ok(()));
        assert_eq!(
            engine.master.max_polyphony_override(),
            Some(256),
            "explicitness is retained even when equal to the default"
        );
        engine.master.set_max_polyphony(64);
        engine.idle_turn(|_| Ok(()));
        assert_eq!(engine.master.effective_max_polyphony(), 256);
        assert_eq!(engine.master.max_polyphony_override(), Some(256));
    }

    /// The MIDI schedule reads the limiter's runway off the master bus: the
    /// studio opens with the limiter off and so with nothing to wait out, a
    /// character change lands on the next read, and off holds nothing back.
    #[test]
    fn the_master_bus_reports_the_limiters_runway() {
        let bus = crate::engine::StudioMasterBus::default();
        // The limiter is a setting now, and the studio opens with it off:
        // there is no runway to report until something asks for one.
        assert_eq!(bus.limiter_latency_frames(48_000), 0);
        bus.set_limiter(Some(rustel_audio::LimiterSettings {
            threshold_db: -1.0,
            character: rustel_audio::LimiterCharacter::Transparent,
        }));
        // Transparent holds five milliseconds at 48 kHz.
        assert_eq!(bus.limiter_latency_frames(48_000), 240);
        bus.set_limiter(Some(rustel_audio::LimiterSettings {
            threshold_db: -6.0,
            character: rustel_audio::LimiterCharacter::Hard,
        }));
        assert_eq!(bus.limiter_latency_frames(48_000), 48);
        // Off is not "whatever the last character was": no offset at all.
        bus.set_limiter(None);
        assert_eq!(bus.limiter_latency_frames(48_000), 0);
    }

    /// A `.midi()` note is timed `latency_frames()` later than the device
    /// clock it was placed on, so the hardware fires with the audio the
    /// limiter held back - never ahead of it. Off adds nothing, and a
    /// character change only moves notes scheduled after it.
    #[test]
    fn the_midi_schedule_waits_out_the_limiters_runway() {
        let mut engine = silent_engine_with_pattern(rustel_core::silence());
        let anchor = engine.midi_clock.instant_for(0.0, 0.0);
        // The mapping slews towards the wall clock by a thousandth of the
        // elapsed time per call; between two back-to-back calls that is
        // microseconds, so the runway dominates the comparison.
        let runway = |frames: u64| Duration::from_secs_f64(frames as f64 / 48_000.0);
        // The studio opens with the limiter off, so there is nothing to
        // wait out yet.
        assert!(
            engine
                .midi_instant_for(0.0, 0.0, 48_000)
                .duration_since(anchor)
                < Duration::from_millis(1),
            "off holds nothing back"
        );
        engine
            .master
            .set_limiter(Some(rustel_audio::LimiterSettings {
                threshold_db: -1.0,
                character: rustel_audio::LimiterCharacter::Transparent,
            }));
        let due = engine.midi_instant_for(0.0, 0.0, 48_000);
        let wait = due.duration_since(anchor);
        assert!(
            wait >= runway(240) && wait < runway(240) + Duration::from_millis(1),
            "transparent holds five milliseconds, not {wait:?}"
        );
        engine
            .master
            .set_limiter(Some(rustel_audio::LimiterSettings {
                threshold_db: -6.0,
                character: rustel_audio::LimiterCharacter::Hard,
            }));
        let due = engine.midi_instant_for(0.0, 0.0, 48_000);
        let wait = due.duration_since(anchor);
        assert!(
            wait >= runway(48) && wait < runway(48) + Duration::from_millis(1),
            "a change lands on the next scheduling turn: hard holds one \
             millisecond, not {wait:?}"
        );
        engine.master.set_limiter(None);
        let due = engine.midi_instant_for(0.0, 0.0, 48_000);
        assert!(
            due.duration_since(anchor) < Duration::from_millis(1),
            "off holds nothing back"
        );
    }

    /// The ids the layout hands the studio are the ids the running score's
    /// cells answer to, which is what lets a dragged chip reach the pattern
    /// without an evaluate.
    #[test]
    fn layout_slider_ids_address_the_running_scores_cells() {
        let source = "$: s(\"bd\").gain(slider(.25, 0, 1, .05))";
        let mut session = Session::new().expect("session");
        session.evaluate(source).expect("score");
        let layout = visual_layout(source, session.generation()).expect("layout");
        let slider = &layout.ui_layout.sliders[0];
        assert_eq!(session.slider_value(&slider.id).expect("read"), Some(0.25));
        assert!(session.set_slider_value(&slider.id, 0.6).expect("write"));
        assert_eq!(session.slider_value(&slider.id).expect("read"), Some(0.6));
        assert!(
            !session
                .set_slider_value("not-a-slider", 0.6)
                .expect("write")
        );
    }

    /// A `midikeys` press re-queries the pattern from close to now. It does
    /// not wait for the scheduler frontier. See `hear_midi_keys_now`.
    #[test]
    fn a_key_press_asks_the_pattern_again_from_close_to_now() {
        let mut engine = silent_engine_with_pattern(rustel_core::silence());
        engine
            .session
            .evaluate(
                "const kb = await midikeys('keyboard')
$: kb(0.25).s(\"tri\")
",
            )
            .expect("a score that plays a keyboard");
        // The port the score asked for, and a turn to settle on it.
        engine.tick(|_| Ok(())).expect("tick");
        let port = engine
            .session
            .midi_input_bus()
            .find("keyboard")
            .expect("the score's keyboard");

        let quiet = engine.generation();
        engine.tick(|_| Ok(())).expect("tick");
        assert_eq!(
            engine.generation(),
            quiet,
            "a turn with no key pressed asks the pattern nothing"
        );

        // Proof the assertion above can fail: without the press, the
        // generation is still `quiet` after any number of turns.
        for _ in 0..3 {
            engine.tick(|_| Ok(())).expect("tick");
        }
        assert_eq!(engine.generation(), quiet);

        port.observe_note_on(rustel_core::midi_in::now_nanos(), 1, 60, 100);
        engine.tick(|_| Ok(())).expect("tick");
        assert!(
            engine.generation() > quiet,
            "the press asks the pattern again rather than waiting for the frontier"
        );

        // And once asked, it is not asked again for the same press.
        let asked = engine.generation();
        engine.tick(|_| Ok(())).expect("tick");
        assert_eq!(engine.generation(), asked, "one press, one re-query");
    }

    /// A racing horizon fill must not steal a fresh press's placement.
    ///
    /// The first trigger query that selects a press claims it. A producer
    /// step deep in its horizon can win that race and pin the press well
    /// past the takeover frame, as happens when a heavy pattern shares the
    /// stack. The press must sound at the takeover frame, the earliest frame
    /// the device can voice it, and not at the pin of the fill.
    ///
    /// The test stages the race: it publishes the press, runs one query that
    /// begins well past the takeover of the re-query (the fill), then runs
    /// the engine turn that answers the press.
    #[test]
    fn a_horizon_fill_cannot_steal_a_fresh_key_press() {
        let mut engine = silent_engine_with_pattern(rustel_core::silence());
        // A fixed takeover offset keeps the race deterministic whatever the
        // host's own pipeline latency would make the margin (WSLg drifts).
        engine.session.set_continuity_margin(0.1);
        engine
            .session
            .evaluate("const kb = await midikeys('keyboard')\n$: kb(0.25).s(\"tri\")\n")
            .expect("a score that plays a keyboard");
        engine.tick(|_| Ok(())).expect("tick");
        let port = engine
            .session
            .midi_input_bus()
            .find("keyboard")
            .expect("the score's keyboard");

        let now = engine.live.as_ref().expect("live").device.clock_seconds();
        // The press lands, and before the engine can answer it a producer
        // step whose query begins past the re-query's takeover selects it.
        // Staging the fill after the press is what a producer step deep in
        // a heavy stack looks like from the ring.
        port.observe_note_on(rustel_core::midi_in::now_nanos(), 1, 60, 100);
        let fill_events = engine
            .session
            .schedule_audio_through(now + 0.25, now + 0.75, 48_000)
            .expect("the racing horizon fill");
        // The pattern is silent without a press, so the single fill event is
        // the press. The fill pins it at its own begin, now + 0.25 s.
        // `target_frame` counts frames at 48 kHz.
        assert_eq!(
            fill_events.len(),
            1,
            "the fill claims the press at its own begin"
        );
        let fill_pin = fill_events[0].target_frame as f64 / 48_000.0;
        // Well past the 0.1 takeover: the fill's claim is the steal itself.
        // (Window quantization shifts the pin a few ms; nobody hears that.)
        assert!(
            fill_pin >= now + 0.2,
            "the fill pinned the press past the takeover (pin {fill_pin})"
        );

        // The turn that answers the press re-queries from close to now. Its
        // takeover frame (now + 0.1) is inside its own query span, and the
        // fill's pin is not. If the stale pin of the outgoing generation
        // survived, the re-query would sound nothing.
        engine.tick(|_| Ok(())).expect("tick");

        // The press must now be placed at the takeover frame - the earliest
        // moment the device could voice it - not parked at the fill's begin.
        // `select` with no placement offer only reads; it cannot move a pin.
        let takeover_cycle = engine.session.cycle_at_time(now + 0.1);
        let stale_cycle = engine.session.cycle_at_time(now + 0.25);
        let mut hits = Vec::new();
        port.keys.select(
            0.0,
            stale_cycle + 1.0,
            rustel_core::midi_in::now_nanos(),
            None,
            &mut hits,
        );
        assert_eq!(hits.len(), 1, "one press, one placement");
        let placed = hits[0].num as f64 / hits[0].den as f64;
        assert!(
            placed < stale_cycle - 0.01,
            "the press must not stay where the racing fill pinned it \
             ({placed} >= {stale_cycle})"
        );
        assert!(
            placed <= takeover_cycle + 0.06,
            "the press must be re-claimed at the re-query's takeover \
             (~{takeover_cycle}), not parked ahead at {placed}"
        );
    }

    /// A pad bound to scene launch is a transport button, not a key. Its
    /// press arms no requery, so the session generation does not move. A
    /// key with no scene binding still requeries.
    #[test]
    fn a_launch_pad_press_never_arms_the_keys_requery() {
        let mut engine = silent_engine_with_pattern(rustel_core::silence());
        engine
            .session
            .evaluate("const kb = await midikeys('keyboard')\n$: kb(0.25).s(\"tri\")\n")
            .expect("a score that plays a keyboard");
        engine.set_launch_pads(vec![(60, 1)]);
        engine.tick(|_| Ok(())).expect("baseline tick");
        let port = engine
            .session
            .midi_input_bus()
            .find("keyboard")
            .expect("the score's keyboard");

        // The launch pad's press: the ring stays empty and the watcher -
        // which counts musical presses only - sees no change, so the
        // session generation cannot move.
        let before = engine.session.generation();
        port.observe_note_on(rustel_core::midi_in::now_nanos(), 1, 60, 100);
        engine
            .tick(|_| Ok(()))
            .expect("tick over the transport press");
        assert_eq!(
            engine.session.generation(),
            before,
            "a transport press must arm no requery"
        );
        let mut hits = Vec::new();
        port.keys.select(
            0.0,
            4.0,
            rustel_core::midi_in::now_nanos(),
            Some((1, 8)),
            &mut hits,
        );
        assert!(hits.is_empty(), "a transport press is never musical");

        // The control: an unbound note on the same port changes the press
        // count and arms the at-once requery.
        port.observe_note_on(rustel_core::midi_in::now_nanos(), 1, 61, 100);
        engine
            .tick(|_| Ok(()))
            .expect("tick over the musical press");
        assert!(
            engine.session.generation() > before,
            "a musical press must still requery at once"
        );
    }
}
