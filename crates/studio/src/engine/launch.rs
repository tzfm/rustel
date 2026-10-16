use super::*;

impl StudioEngine {
    // ---- quantised launch --------------------------------------------

    pub(super) fn launch_info(&self) -> Option<LaunchInfo> {
        let now = self.live.as_ref()?.device.clock_seconds();
        self.launch_info_at(now)
    }

    /// The countdown at device time `now`: a launch still armed, or one
    /// that fired within its head-room while its line is still ahead. Past
    /// the line there is nothing to wait for and no countdown to show -
    /// the grace memory answers a repeat press, but it is not a launch.
    fn launch_info_at(&self, now: f64) -> Option<LaunchInfo> {
        let (boundary_cycle, boundary_time) = match (&self.pending_launch, self.landing.as_ref()) {
            (Some(pending), _) => (pending.boundary_cycle, self.pending_line_time(pending, now)),
            (None, Some(landing)) if now < landing.boundary_time => {
                (landing.boundary_cycle, landing.boundary_time)
            }
            _ => return None,
        };
        let cycle_now = self.session.cycle_at_time(now);
        Some(LaunchInfo {
            boundary_cycle,
            cycles_left: (boundary_cycle - cycle_now).max(0.0),
            seconds_left: (boundary_time - now).max(0.0),
        })
    }

    /// When an armed launch's line falls under the mapping in force NOW.
    /// The line is a cycle; an outside clock that retimes the transport
    /// during the countdown moves the time of that cycle, and the takeover,
    /// cycle zero and the device cut must all land on the moved bar line,
    /// not where it stood when the pad was pressed.
    pub(super) fn pending_line_time(&self, pending: &PendingLaunch, now: f64) -> f64 {
        let cps = self.session.cps();
        if !(cps.is_finite() && cps > 0.0) {
            return pending.boundary_time;
        }
        let time = now + (pending.boundary_cycle - self.session.cycle_at_time(now)) / cps;
        if time.is_finite() {
            time
        } else {
            pending.boundary_time
        }
    }

    pub(super) fn launch_headroom(&self) -> f64 {
        self.live
            .as_ref()
            .map(|live| {
                live.device.schedule_lead_seconds()
                    + live.device.continuity_margin_seconds()
                    + LAUNCH_HEADROOM_SECS
            })
            .unwrap_or(LAUNCH_HEADROOM_SECS)
    }

    /// Arm `source` to take over on the next line of `unit_cycles`. With
    /// nothing playing there is no line to wait for: `Ok(None)` says
    /// "evaluate now". A launch already armed is replaced.
    pub fn arm_launch(
        &mut self,
        source: &str,
        mini: bool,
        unit_cycles: f64,
        rewind: bool,
    ) -> Result<Option<LaunchInfo>, RuntimeError> {
        if self.live.is_none() || self.is_stopping() {
            self.pending_launch = None;
            self.recent_rewind = None;
            return Ok(None);
        }
        // A start still holding its cycle zero has no line yet: the launch
        // replaces it now, from its own cycle zero.
        if self.start_is_held() {
            return Ok(None);
        }
        let now = self.live.as_ref().expect("checked").device.clock_seconds();
        self.arm_launch_at(now, source, mini, unit_cycles, rewind)
    }

    /// [`Self::arm_launch`] decided at one device time, `now`: every guard
    /// and every answer reads it, never the clock again, so an answer cannot
    /// disagree with the guard that chose it.
    fn arm_launch_at(
        &mut self,
        now: f64,
        source: &str,
        mini: bool,
        unit_cycles: f64,
        rewind: bool,
    ) -> Result<Option<LaunchInfo>, RuntimeError> {
        // Settle first. A press can arrive after the line has passed and
        // before the next tick runs, and the landing is then still set.
        // Settling here turns it into the memory that the guard below
        // reads.
        self.settle_landing_at(now);
        // A launch that has fired but not yet landed already owns the line:
        // re-arming the same score would install it a second time, one line
        // later, and the ear hears the first beat twice. The same source
        // pressed again is the same request - answer it with the launch in
        // flight. A different source is an edit and must supersede, so it
        // replaces the landing's countdown below.
        if let Some(landing) = &self.landing
            && now < landing.boundary_time
            && landing.install.source_revision == source_revision(source)
            && landing.rewind == rewind
        {
            // The worker arms this request, and the next outcome poll
            // answers it with the landing's own install: same success,
            // no second install. The answer is built from the `now` that
            // passed the guard: reading the clock again could cross the
            // line in between and turn it into "evaluate now" - a second
            // restart on the downbeat.
            let answer = LaunchInfo {
                boundary_cycle: landing.boundary_cycle,
                cycles_left: (landing.boundary_cycle - self.session.cycle_at_time(now)).max(0.0),
                seconds_left: (landing.boundary_time - now).max(0.0),
            };
            self.launch_outcome = Some(Ok(StudioInstall {
                answered_repeat: true,
                ..landing.install.clone()
            }));
            self.log_launch("repeat press before the line - answered with the launch in flight");
            return Ok(Some(answer));
        }
        // The line has passed and the landing is settled. The second press
        // of a fast double press can still arrive here: the first press
        // fires one head-room early, so the second finds no pending launch
        // and no landing. Arming the same score again installs it from zero
        // one line later, and the first beat sounds twice. So the fired
        // rewind is remembered briefly past its line, and the same score
        // pressed inside the grace is answered with it. Past the grace the
        // memory expires and the press restarts the score. The grace does
        // not apply while a launch is armed or landing.
        if self.pending_launch.is_none()
            && self.landing.is_none()
            && rewind
            && let Some(recent) = &self.recent_rewind
            && now < recent.landed_at + recent.grace_secs
            && recent.install.source_revision == source_revision(source)
        {
            let answer = LaunchInfo {
                boundary_cycle: recent.boundary_cycle,
                cycles_left: 0.0,
                seconds_left: 0.0,
            };
            self.launch_outcome = Some(Ok(StudioInstall {
                answered_repeat: true,
                ..recent.install.clone()
            }));
            // Some, not None: None sends the worker into an immediate
            // evaluate - a third restart, right now. The info says the
            // answered press waits for nothing.
            self.log_launch("repeat press just after the line - answered with the fired rewind");
            return Ok(Some(answer));
        }
        let cps = self.session.cps().max(1e-6);
        let cycle_now = self.session.cycle_at_time(now);
        let headroom_cycles = self.launch_headroom() * cps;
        let boundary_cycle = next_boundary(cycle_now, unit_cycles, headroom_cycles);
        let boundary_time = now + (boundary_cycle - cycle_now) / cps;
        // The countdown gives the decoder a head start: the incoming
        // score's sounds start to load at arm time, not at fire time. One
        // beat is usually enough for the replacement's first window to
        // render complete, so the line needs no loading hold.
        // Warming does not install. Until the launch lands, the playing
        // score stays the current score and the sample sweep keeps what
        // it names.
        self.warm_incoming(source);
        self.look_up_imports_of(source);
        // In the wait mode, the plugin copies of the launch build during the
        // countdown too, for the cycles it plays first.
        #[cfg(feature = "vst")]
        self.prepare_live(source, mini, if rewind { 0.0 } else { boundary_cycle });
        self.pending_launch = Some(PendingLaunch {
            source: source.to_owned(),
            mini,
            boundary_cycle,
            boundary_time,
            rewind,
            unit_cycles,
            preview: std::mem::take(&mut self.next_install_preview),
            followed: self.new_in_text(source, mini),
            waited: false,
        });
        self.ask_for_waited();
        // The launch now promises a boundary on this timeline.
        self.live
            .as_mut()
            .expect("checked")
            .initial_start_generation = None;
        self.launch_outcome = None;
        let info = self.launch_info_at(now);
        debug_assert!(info.is_some(), "a freshly armed launch has a countdown");
        Ok(info)
    }

    /// Log one launch decision with the transport cycle, so a reader can
    /// match the line to the beat. It is a note: it reaches the studio log
    /// and the log file on disk, and does not take the status line.
    pub(super) fn log_launch(&mut self, message: &str) {
        let cycle = self.session.cycle_at_time(self.device_time());
        queue_diagnostic(
            &mut self.pending_diagnostics,
            StudioDiagnostic::note("launch", format!("{message} (cycle {cycle:.2})")),
        );
    }

    /// A launch outcome the player has to be told about. The press was
    /// already answered - "updated, swaps in on the next cycle" - and then
    /// the room did not do what that promised: the score was refused, or
    /// the room fell silent at the line and could not be refilled. As a
    /// log note nobody would know why nothing changed, so it takes the
    /// status line as a warning.
    pub(super) fn warn_launch(&mut self, message: &str) {
        queue_diagnostic(
            &mut self.pending_diagnostics,
            StudioDiagnostic::message("launch", message),
        );
    }

    pub(super) fn say_launch(&mut self, line: LaunchLine) {
        match line {
            LaunchLine::Note(message) => self.log_launch(&message),
            LaunchLine::Warning(message) => self.warn_launch(&message),
        }
    }

    /// Forget a launch still waiting for its fire, and an edit held for its
    /// sounds; the outcome says it was cancelled. A launch that has already
    /// fired is untouched: its generation is built, its line is armed, and
    /// a newer launch armed over it (a pad race) lands after it rather than
    /// unmaking it - the in-flight launch still answers a repeat of itself.
    pub fn cancel_pending_launch(&mut self) {
        let launch = self.pending_launch.take().is_some();
        let held = self.load.held_edit.take().is_some();
        if launch || held {
            self.launch_outcome = Some(Err(RuntimeError::Cancelled));
        }
    }

    /// End every launch in flight: an edit played now, or a stop. The
    /// pending launch is cancelled, and a fired launch that has not been
    /// heard gives back its line cut. A cut that stays armed silences what
    /// plays at the line: the edit, or the tail that a stop lets ring out.
    pub fn supersede_launch(&mut self) {
        self.cancel_pending_launch();
        self.abandon_landing(false, false);
    }

    /// Let go of a fired launch before, or instead of, its landing.
    ///
    /// A landing whose line has passed and whose generation reached the
    /// device was a rewind the ear has heard: it stays answerable through
    /// the grace. Anything else never sounded, and its pre-armed line cut
    /// is withdrawn so the rendition still playing keeps playing. With
    /// `refill`, a withdrawal that came too late (a block across the line
    /// had already cut the outgoing onsets) asks the still-active score
    /// again from now. `refused` is a replacement the producer rolled back:
    /// even if the rollback's own flip has since reached the device, the
    /// refused install never sounded and must not answer a repeat.
    pub(super) fn abandon_landing(&mut self, refill: bool, refused: bool) {
        let Some(landing) = self.landing.take() else {
            return;
        };
        let Some(live) = self.live.as_ref() else {
            return;
        };
        let heard = !refused
            && live.device.clock_seconds() >= landing.boundary_time
            && live.device.generation() >= landing.install.generation;
        if heard {
            if landing.rewind {
                self.recent_rewind = Some(RecentRewind {
                    grace_secs: rewind_grace_secs(landing.unit_cycles, self.session.cps()),
                    install: landing.install,
                    boundary_cycle: landing.boundary_cycle,
                    landed_at: landing.boundary_time,
                });
            }
            return;
        }
        if landing.rewind {
            self.withdraw_line_cut(refill);
        }
    }

    /// Withdraw the line cut a fired rewind armed, because no flip of its
    /// is coming. The consumer lets a fired arm go on the withdrawal, but
    /// the outgoing onsets it retired at the line are gone. So when the arm
    /// was still set and the render frontier, read after the withdrawal, has
    /// passed its line, what the external outputs owed from the line goes
    /// too. With `refill` the still-active score is then asked again from
    /// now, so the room refills at the continuity margin. A line a published
    /// flip already cleared is left alone: what sounds from it is the
    /// launched score.
    pub(super) fn withdraw_line_cut(&mut self, refill: bool) {
        let Some(live) = self.live.as_mut() else {
            return;
        };
        let armed = live.device.armed_line_frame();
        live.device.clear_line_arm();
        self.line_cut_withdrawn = false;
        let Some(line_frame) = armed else {
            return;
        };
        if !self.retire_past_fired_line(line_frame) || !refill {
            return;
        }
        let Some(live) = self.live.as_mut() else {
            return;
        };
        let now = live.device.clock_seconds();
        let requery = self.session.requery_active_at(now);
        if let Ok(Some((before, after))) = requery {
            live.producer.arm_control_requery(before, after);
        }
        if let Some(line) = refill_line(requery) {
            self.say_launch(line);
        }
    }

    /// A fired launch whose replacement the producer refused (its first
    /// window could not sound, or it ran out of room) will never flip in,
    /// whether the producer rolled back, is still rolling back, or latched.
    /// The producer names that generation; the session cannot, because the
    /// commonest pad rewind restarts the score already playing, and a
    /// rollback then puts back the very text the launch installed. Its line
    /// cut is withdrawn at once - the rollback's own flip carries the
    /// handoff and its re-query refills the room - and it is forgotten
    /// rather than promoted to the grace memory, since the ear never heard
    /// it. A landing the line already settled into that memory on this
    /// turn is forgotten there too.
    pub(super) fn abandon_rolled_back_landing(&mut self) {
        let Some(abandoned) = self
            .live
            .as_mut()
            .and_then(|live| live.producer.take_abandoned_generation())
        else {
            return;
        };
        if self
            .recent_rewind
            .as_ref()
            .is_some_and(|recent| recent.install.generation == abandoned)
        {
            self.recent_rewind = None;
        }
        if self
            .landing
            .as_ref()
            .is_none_or(|landing| landing.install.generation != abandoned)
        {
            return;
        }
        self.warn_launch("the launch's score was refused - the playing score keeps the line");
        self.abandon_landing(false, true);
    }

    /// The generation a repeat launch press was just answered with, while
    /// that answer waits for the worker to collect it: nothing is armed and
    /// nothing will install, so there is no line to announce.
    pub fn answered_repeat_generation(&self) -> Option<u64> {
        match &self.launch_outcome {
            Some(Ok(install)) if install.answered_repeat => Some(install.generation),
            _ => None,
        }
    }

    /// What became of the armed launch, once it fired or was dropped.
    pub fn take_launch_outcome(&mut self) -> Option<Result<StudioInstall, RuntimeError>> {
        self.launch_outcome.take()
    }

    /// Withdraw the pre-armed line cut while a replacement waits for its
    /// samples.
    ///
    /// `advance_launch` arms the cut at the line before the replacement's
    /// evaluation runs. If the replacement's first window then defers for
    /// loading (the producer's rewind loading hold), the flip is held. A
    /// cut that fires with no replacement ready fades the outgoing score
    /// into silence for as long as the decode takes, and the flip's own
    /// choke ramp then lands on silence, which clicks. So the arm is
    /// withdrawn while the hold is on, and the outgoing score keeps
    /// playing. When loading finishes the flip lands with its own
    /// AtTakeover intent, which is the cut at the line; the arm is not put
    /// back. The producer never sees the withdrawal: the arm is its cue to
    /// cut, not a prerequisite for holding.
    pub(super) fn sync_line_cut_with_loading_holds(&mut self) {
        let Some(live) = self.live.as_ref() else {
            return;
        };
        let deferred = live.producer.cut_takeover_deferred_for_loading();
        if deferred && !self.line_cut_withdrawn {
            let line = live.device.armed_line_frame();
            live.device.clear_line_arm();
            self.line_cut_withdrawn = true;
            // An arm that fired first has retired the outgoing onsets
            // already, and the withdrawal brings none of them back.
            if let Some(line) = line {
                self.retire_past_fired_line(line);
            }
            self.log_launch("line cut held - replacement samples still loading");
        } else if !deferred && self.line_cut_withdrawn {
            // The hold is over because the flip landed (or the replacement
            // rolled back). Nothing is re-armed: the flip carries its own
            // AtTakeover intent, which fades the countdown at the line and
            // drops the old ring events from it. The consumer's arm does
            // not check the generation, so a second arm on a landed flip
            // would retire the new generation's onsets at the line, and
            // the restarted loop would lose its first beat. After a
            // rollback it would cut the score still playing into silence.
            self.line_cut_withdrawn = false;
        }
    }

    /// Fire the armed launch if its head-room has opened, as the next
    /// tick would. A launch that fires here owns its line: a command
    /// handled before that tick finds it in flight, not waiting. Does
    /// nothing while stopped, when no tick runs.
    pub fn fire_due_launch(&mut self) {
        if !self.is_playing() {
            return;
        }
        self.sync_line_cut_with_loading_holds();
        self.advance_launch();
    }

    /// Fire the armed launch once its line is within the head-room: the
    /// reload is told the takeover time, so the old generation sounds
    /// until the line and the new one is queried from it.
    pub(super) fn advance_launch(&mut self) {
        let Some(pending) = self.pending_launch.as_ref() else {
            return;
        };
        let Some(live) = self.live.as_ref() else {
            self.pending_launch = None;
            self.launch_outcome = Some(Err(RuntimeError::Cancelled));
            return;
        };
        let now = live.device.clock_seconds();
        let boundary_time = self.pending_line_time(pending, now);
        if now < boundary_time - self.launch_headroom() {
            return;
        }
        if self.launch_waits_for_sounds(now) {
            return;
        }
        let pending = self.pending_launch.take().expect("checked");
        if pending.rewind {
            self.log_launch("rewind fired - from zero at the line");
        }
        self.session.set_next_takeover_time(boundary_time);
        // A rewinding launch puts cycle zero on the line it lands on, so
        // the score starts from its beginning exactly where the bar does.
        if pending.rewind {
            self.session.start_next_from_zero();
        }
        // Pre-arm the line before the evaluation, for a rewinding launch
        // only. A rewind replaces the room at the line. However long the
        // evaluation takes, the outgoing rendition falls silent there and
        // its later onsets are dropped, so the old score cannot play
        // through the countdown and sound its first beat twice. A launch
        // that does not rewind arms nothing: it hands over with the
        // ring-out contract. A failed evaluation withdraws the arm below,
        // and the room keeps playing.
        if pending.rewind {
            let device_cut = frame_at(
                boundary_time,
                self.live.as_ref().expect("checked").device.sample_rate(),
            );
            self.live
                .as_ref()
                .expect("checked")
                .device
                .arm_line_cut(device_cut, true);
            // A fresh arm owns the line: any earlier withdrawal described a
            // previous launch's held flip, not this one.
            self.line_cut_withdrawn = false;
        }
        self.next_install_preview = pending.preview;
        let outcome = self.reload_live(&pending.source, pending.mini);
        if let Ok(Some(install)) = &outcome {
            self.landing = Some(Landing {
                boundary_cycle: pending.boundary_cycle,
                boundary_time,
                install: install.clone(),
                rewind: pending.rewind,
                unit_cycles: pending.unit_cycles,
            });
        } else {
            // A takeover time nobody consumed would silently delay the next
            // ordinary update to a line that has since passed.
            self.session.clear_next_takeover_time();
            self.session.clear_next_from_zero();
            // An evaluation that failed or waits installed nothing, so the
            // old score is still the active one: withdraw its cut, and
            // refill it if the evaluation outlasted the line and the cut
            // already fired.
            if pending.rewind {
                self.withdraw_line_cut(true);
            }
        }
        match outcome {
            Ok(Some(install)) => self.launch_outcome = Some(Ok(install)),
            // The session holds the launch for its plugin copies: the launch
            // takes the first line after them.
            Ok(None) => {
                let preview = std::mem::take(&mut self.next_install_preview);
                self.pending_launch = Some(PendingLaunch { preview, ..pending });
                self.launch_takes_next_line(self.device_time(), "plugins");
            }
            Err(error) => {
                // The launch does not go in play: the session ends its wait.
                #[cfg(feature = "vst")]
                self.session.cancel_plugin_hold();
                self.launch_outcome = Some(Err(error));
            }
        }
    }

    /// A fired launch is landed once its line has passed; the countdown
    /// ends there, not a head-room early.
    pub(super) fn settle_landing(&mut self) {
        if let Some(live) = self.live.as_ref() {
            self.settle_landing_at(live.device.clock_seconds());
        }
    }

    fn settle_landing_at(&mut self, now: f64) {
        let boundary_time = self.landing.as_ref().map(|landing| landing.boundary_time);
        if let Some(boundary_time) = boundary_time
            && now >= boundary_time
            && let Some(landing) = self.landing.take()
            && landing.rewind
        {
            // The line has passed and the fired rewind is now the room:
            // keep it briefly past the line so the second press of an
            // eager double - which lands after it - is answered with it
            // rather than re-installed from zero one line later.
            self.recent_rewind = Some(RecentRewind {
                grace_secs: rewind_grace_secs(landing.unit_cycles, self.session.cps()),
                install: landing.install,
                boundary_cycle: landing.boundary_cycle,
                landed_at: landing.boundary_time,
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::tests::*;

    /// The countdown warms the INCOMING score's sounds, including the
    /// names a `.bank()` builds, and does not spend its budget querying the
    /// score going out. The text scan alone missed `tr909_bd`, and the line
    /// fired into the loading hold the warm was there to avoid.
    #[test]
    fn arming_a_launch_warms_the_incoming_banked_sounds() {
        let mut engine = silent_engine_with_pattern(rustel_core::silence());
        let library = Arc::new(rustel_runtime::samples::SampleLibrary::empty_without_loading());
        library
            .register_trusted_custom(
                r#"{"_base":"https://samples.invalid/","tr909_bd":"bd.wav","hh":"hh.wav"}"#,
                None,
            )
            .expect("bank map");
        engine
            .session
            .set_sample_library_for_test(Arc::clone(&library));
        engine.current_score = "s(\"hh\")".to_owned();
        let before = library.pending_loads();
        engine
            .arm_launch(r#"s("bd").bank("tr909")"#, false, 1.0, true)
            .expect("arm")
            .expect("armed");
        assert!(
            library.pending_loads() > before,
            "the banked name is on its way before the line"
        );
        assert_eq!(
            engine.current_score, "s(\"hh\")",
            "warming is not installing"
        );
        engine.supersede_launch();
    }

    #[test]
    fn the_next_line_leaves_head_room() {
        assert_eq!(next_boundary(2.3, 1.0, 0.1), 3.0);
        assert_eq!(
            next_boundary(2.95, 1.0, 0.1),
            4.0,
            "too close: the one after"
        );
        assert_eq!(next_boundary(2.3, 0.25, 0.05), 2.5);
        assert_eq!(next_boundary(5.0, 4.0, 0.1), 8.0);
        assert_eq!(next_boundary(7.9, 4.0, 0.5), 12.0);
        assert_eq!(next_boundary(1.0, 0.0, 0.0), 2.0, "a bad unit is a cycle");
    }

    /// A quantised rewind whose incoming score still loads a sample must
    /// not cut the outgoing score at the line. The cut would fade a room
    /// the replacement cannot yet sound, and the flip's choke ramp would
    /// then land on silence, which clicks. The engine withdraws the arm
    /// while the producer holds the flip for loading. When the sample
    /// decodes and the flip lands, the flip owns the cut and the line arm
    /// is not put back.
    #[test]
    fn a_launch_whose_replacement_still_loads_withdraws_the_line_cut_until_the_flip_lands() {
        let mut engine = silent_engine_with_pattern(rustel_core::silence());
        let library =
            Arc::new(rustel_runtime::samples::SampleLibrary::with_loading_sample_for_test("held"));
        engine
            .session
            .set_sample_library_for_test(Arc::clone(&library));
        // Async fires on the line while the sample loads; wait would
        // take the first line after it.
        engine.master.set_load_mode(LoadMode::Async);

        assert!(
            engine
                .arm_launch("s(\"held\")", false, 0.25, true)
                .expect("arm")
                .is_some(),
            "a playing engine arms a quantised launch"
        );
        // Drive the countdown to the fire. The clock stays held from the
        // firing tick until the sample decodes, so the line cannot pass
        // while the test watches the hold.
        let install = match tick_until_fired(&mut engine) {
            Ok(install) => install,
            Err(error) => panic!("the launch failed: {error:?}"),
        };
        assert!(
            engine.landing.is_some(),
            "the fired rewind waits for its line"
        );
        assert_ne!(
            engine
                .live
                .as_ref()
                .expect("playing")
                .device
                .armed_line_word(),
            0,
            "a rewind arms its line cut at fire time"
        );

        // The session installed the score at fire. While the sample loads,
        // the producer holds the device flip: the line cut is withdrawn and
        // the device generation does not move. Poll for the hold.
        let device_before = engine.live.as_ref().expect("playing").device.generation();
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            engine.tick(accepted).expect("tick while loading");
            // Break when the producer reports the hold. The arm must be
            // withdrawn by then: an arm that survives the hold fires into
            // silence at the line.
            if engine
                .live
                .as_ref()
                .expect("playing")
                .producer
                .cut_takeover_deferred_for_loading()
            {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "the producer never held the flip for loading"
            );
            std::thread::sleep(Duration::from_millis(2));
        }
        assert!(
            !engine
                .live
                .as_ref()
                .expect("playing")
                .device
                .line_cut_armed(),
            "the pre-armed line cut is withdrawn while the replacement loads"
        );
        assert_eq!(
            engine.live.as_ref().expect("playing").device.generation(),
            device_before,
            "the held flip must not install the loading score"
        );

        // The sample decodes: the next producer turn publishes the flip to
        // the device and the landing settles.
        hold_clock(&engine, false);
        library.finish_loading_sample_for_test();
        let deadline = Instant::now() + Duration::from_secs(5);
        while engine.live.as_ref().expect("playing").device.generation() != install.generation
            && Instant::now() < deadline
        {
            engine.tick(accepted).expect("tick after decode");
            std::thread::sleep(Duration::from_millis(2));
        }
        assert_eq!(
            engine.live.as_ref().expect("playing").device.generation(),
            install.generation,
            "the flip lands once the replacement can render"
        );
        // The landed flip owns the cut at the line (AtTakeover). A second
        // arm on top of it would cut the new generation's first window.
        // One more tick lets the engine notice the hold ended.
        engine.tick(accepted).expect("tick after the flip");
        assert_eq!(
            engine
                .live
                .as_ref()
                .expect("playing")
                .device
                .armed_line_word(),
            0,
            "no line arm is put back once the replacement's flip has landed"
        );
        assert!(
            !engine.line_cut_withdrawn,
            "the withdrawal is over with the hold"
        );
        assert!(
            !engine
                .live
                .as_ref()
                .expect("playing")
                .producer
                .cut_takeover_deferred_for_loading(),
            "the landed flip ends the producer's loading hold"
        );
    }

    /// A repeat press after the launch fired and before its line is the
    /// same request. It is answered with the launch in flight, not armed
    /// again as a second from-zero install one line later.
    #[test]
    fn a_repeat_press_while_a_fired_launch_is_in_flight_answers_with_that_launch() {
        let mut engine = silent_engine_with_pattern(rustel_core::silence());
        let source = "s(\"tri\")";
        let install = arm_and_fire_launch(&mut engine, source);

        // The line is still ahead: the landing window is open.
        let landing = engine
            .landing
            .as_ref()
            .expect("a fired launch waits for its line");
        let boundary_cycle = landing.boundary_cycle;
        assert!(
            engine
                .live
                .as_ref()
                .expect("playing")
                .device
                .clock_seconds()
                < landing.boundary_time,
            "the line must still be ahead for the landing window to bite"
        );

        // The SAME source pressed again inside the window.
        let repeat = engine
            .arm_launch(source, false, 0.25, true)
            .expect("repeat arm")
            .expect("the in-flight launch answers the repeat");
        assert_eq!(
            repeat.boundary_cycle, boundary_cycle,
            "the repeat is answered with the launch already in flight, not the next line"
        );
        assert!(
            engine.pending_launch.is_none(),
            "the repeat must not arm a second launch"
        );
        // Its outcome is the landing's own install: answered once, no
        // second install.
        match engine.take_launch_outcome() {
            Some(Ok(again)) => assert_eq!(
                again.generation, install.generation,
                "the repeat answers with the fired generation"
            ),
            other => panic!(
                "the repeat press must be answered with the fired launch's success, got {other:?}"
            ),
        }

        // Past the line, nothing fires again: the generation the launch
        // installed is the one that plays on.
        hold_clock(&engine, false);
        let generation = engine.generation();
        let until = engine
            .live
            .as_ref()
            .expect("playing")
            .device
            .clock_seconds()
            + 1.0;
        while engine
            .live
            .as_ref()
            .expect("playing")
            .device
            .clock_seconds()
            < until
        {
            engine.tick(accepted).expect("tick past the line");
            std::thread::sleep(Duration::from_millis(2));
        }
        assert_eq!(
            engine.generation(),
            generation,
            "a repeat press never re-installs the score"
        );
    }

    /// An EDIT during the same window is a different gesture: it supersedes
    /// the in-flight launch and arms the next line afresh.
    #[test]
    fn an_edit_while_a_fired_launch_is_in_flight_supersedes_it() {
        let mut engine = silent_engine_with_pattern(rustel_core::silence());
        arm_and_fire_launch(&mut engine, "s(\"tri\")");
        assert!(
            engine.landing.is_some(),
            "a fired launch waits for its line"
        );

        let _supersede = engine
            .arm_launch("s(\"saw\")", false, 0.25, true)
            .expect("edit arm")
            .expect("an edit arms a launch");
        assert!(
            engine.pending_launch.is_some(),
            "an edit supersedes the in-flight launch with a pending one"
        );
        assert!(
            engine.take_launch_outcome().is_none(),
            "an edit waits for its own fire; it is not answered with the old install"
        );
        engine.supersede_launch();
        let _ = engine.take_launch_outcome();
    }

    /// Only a rewind arms the device's line cut: a quantised launch that
    /// joins the running cycle hands over with the ordinary ring-out.
    #[test]
    fn a_launch_that_does_not_rewind_never_arms_the_line() {
        let mut engine = silent_engine_with_pattern(rustel_core::silence());
        assert!(
            engine
                .arm_launch("s(\"tri\")", false, 0.25, false)
                .expect("arm")
                .is_some()
        );
        tick_until_fired(&mut engine).expect("the launch fires");
        assert_eq!(device_of(&engine).armed_line_word(), 0);
    }

    /// A rewind whose evaluation fails after its line has passed: the
    /// consumer already cut the outgoing score at the line and retired its
    /// onsets. Withdrawing the arm lets the cut go, but only a re-query of
    /// the score still active puts sound back before its next window.
    #[test]
    fn a_rewind_that_fails_after_its_line_withdraws_the_cut_and_refills_the_room() {
        let mut engine = silent_engine_with_pattern(rustel_core::silence());
        arm_past_headroom(&mut engine, "s(\"tri\"", -0.05);
        let generation_before = engine.generation();

        engine.tick(accepted).expect("the firing tick");
        assert!(
            matches!(engine.take_launch_outcome(), Some(Err(_))),
            "the syntax error fails the launch"
        );
        assert!(
            !device_of(&engine).line_cut_armed(),
            "the failed launch withdraws its line cut"
        );
        assert!(engine.landing.is_none(), "a failed launch never lands");
        assert!(engine.recent_rewind.is_none());
        assert_eq!(
            engine.generation(),
            generation_before + 1,
            "the old score is asked again once, so the room refills"
        );
        // The same turn's producer step publishes the refill.
        assert_eq!(device_of(&engine).generation(), generation_before + 1);
        hold_clock(&engine, false);
    }

    /// The same failure before the line: nothing was cut, so the arm is
    /// withdrawn and the old score plays on untouched, with no re-query.
    #[test]
    fn a_rewind_that_fails_before_its_line_withdraws_the_cut_without_a_requery() {
        let mut engine = silent_engine_with_pattern(rustel_core::silence());
        arm_past_headroom(&mut engine, "s(\"tri\"", 0.05);
        let generation_before = engine.generation();

        engine.tick(accepted).expect("the firing tick");
        assert!(matches!(engine.take_launch_outcome(), Some(Err(_))));
        assert_eq!(
            device_of(&engine).armed_line_word(),
            rustel_audio::LINE_ARM_WITHDRAWN
        );
        assert_eq!(engine.generation(), generation_before, "nothing to refill");
        hold_clock(&engine, false);
    }

    /// A fired rewind that the producer refuses (its first window cannot
    /// sound) rolls back to the playing score. Its line cut is withdrawn,
    /// and the refused install does not answer a repeat press.
    #[test]
    fn a_fired_rewind_the_producer_refuses_withdraws_its_line_cut() {
        let mut engine = silent_engine_with_pattern(rustel_core::silence());
        let playing = "note('c4').fast(8)";
        engine.evaluate(playing, false).expect("the playing score");
        let deadline = Instant::now() + Duration::from_secs(10);
        while device_of(&engine).generation() != engine.generation() {
            assert!(
                Instant::now() < deadline,
                "the playing score never published"
            );
            engine.tick(accepted).expect("tick");
            std::thread::sleep(Duration::from_millis(2));
        }
        let audible = engine.generation();
        engine
            .session
            .mark_audible_generation(audible)
            .expect("the playing score is the rollback target");

        assert!(
            engine
                .arm_launch("s(\"nosuchsound*4\")", false, 0.25, true)
                .expect("arm")
                .is_some()
        );
        let line = engine.pending_launch.as_ref().expect("armed").boundary_time;
        tick_until_fired(&mut engine).expect("the evaluation itself succeeds");
        let deadline = Instant::now() + Duration::from_secs(10);
        while engine.session.active_source() != Some(playing) {
            assert!(
                Instant::now() < deadline,
                "the producer never rolled the refused score back"
            );
            engine.tick(accepted).expect("tick");
            std::thread::sleep(Duration::from_millis(2));
        }
        assert!(
            device_of(&engine).clock_seconds() < line,
            "the clock is held before the line"
        );
        assert!(
            !device_of(&engine).line_cut_armed(),
            "the refused launch's line cut is withdrawn"
        );
        assert!(engine.landing.is_none(), "the refused launch never lands");
        assert!(
            engine.recent_rewind.is_none(),
            "a refused install is not remembered as heard"
        );
        hold_clock(&engine, false);
    }

    /// The commonest pad rewind restarts the score that is already
    /// playing. When the producer refuses that launch, its rollback puts
    /// back the very same text, so nothing in the session tells the refused
    /// install from the restored one: the producer names the generation it
    /// gave up on, and the fired line cut goes with it.
    #[test]
    fn a_refused_rewind_of_the_playing_score_withdraws_its_line_cut() {
        let mut engine = silent_engine_with_pattern(rustel_core::silence());
        // One text, two scores: the second evaluation of it - the launch -
        // names a sound nobody has, and every other evaluation plays.
        let playing = "globalThis.plays = (globalThis.plays ?? 0) + 1\n\
                       s(globalThis.plays === 2 ? \"nosuchsound*4\" : \"tri*8\")";
        engine.evaluate(playing, false).expect("the playing score");
        let deadline = Instant::now() + Duration::from_secs(10);
        while device_of(&engine).generation() != engine.generation() {
            assert!(
                Instant::now() < deadline,
                "the playing score never published"
            );
            engine.tick(accepted).expect("tick");
            std::thread::sleep(Duration::from_millis(2));
        }
        let audible = engine.generation();
        engine
            .session
            .mark_audible_generation(audible)
            .expect("the playing score is the rollback target");

        assert!(
            engine
                .arm_launch(playing, false, 0.25, true)
                .expect("arm")
                .is_some()
        );
        let line = engine.pending_launch.as_ref().expect("armed").boundary_time;
        let mut said = Vec::new();
        let mut collect = |update: StudioUpdate| {
            if let StudioUpdate::Diagnostic(diagnostic) = update {
                said.push(diagnostic);
            }
            Ok(())
        };
        let deadline = Instant::now() + Duration::from_secs(10);
        let launched = loop {
            hold_clock(&engine, true);
            engine.tick(&mut collect).expect("tick");
            if let Some(outcome) = engine.take_launch_outcome() {
                break outcome.expect("the evaluation itself succeeds").generation;
            }
            hold_clock(&engine, false);
            assert!(Instant::now() < deadline, "the launch never fired");
            std::thread::sleep(Duration::from_millis(2));
        };
        let deadline = Instant::now() + Duration::from_secs(10);
        while engine.session.generation() == launched {
            assert!(
                Instant::now() < deadline,
                "the producer never rolled the refused score back"
            );
            engine.tick(&mut collect).expect("tick");
            std::thread::sleep(Duration::from_millis(2));
        }
        // Read on the turn that saw the rollback: once the rollback's own
        // flip reaches the device it clears the arm too, and a later turn
        // could no longer tell a withdrawn cut from a superseded one.
        let armed_at_rollback = device_of(&engine).line_cut_armed();
        engine
            .tick(&mut collect)
            .expect("the turn after the rollback");
        assert_eq!(
            engine.session.active_source(),
            Some(playing),
            "the rollback put back the same text"
        );
        assert!(
            device_of(&engine).clock_seconds() < line,
            "the clock is held before the line"
        );
        assert!(
            !armed_at_rollback,
            "the refused launch's line cut is withdrawn"
        );
        assert!(
            !device_of(&engine).line_cut_armed(),
            "and it stays withdrawn"
        );
        assert!(engine.landing.is_none(), "the refused launch never lands");
        assert!(
            engine.recent_rewind.is_none(),
            "a refused install is not remembered as heard"
        );
        // Asked last, so a regression of the withdrawal above reports the
        // withdrawal rather than the missing warning it also causes.
        let refused = said
            .iter()
            .find(|diagnostic| {
                diagnostic.kind == "launch" && diagnostic.message.contains("was refused")
            })
            .expect("the player is told why the launch did nothing");
        assert_eq!(refused.level, StudioDiagnosticLevel::Warning);
        hold_clock(&engine, false);
    }

    /// An edit played now over a fired rewind whose flip is held for
    /// loading: nothing of the rewind has sounded, so its line cut must go
    /// at once. Left armed, it cut the edit at the line into dead air.
    #[test]
    fn an_edit_over_a_held_rewind_withdraws_its_line_cut_at_once() {
        let mut engine = silent_engine_with_pattern(rustel_core::silence());
        let library =
            Arc::new(rustel_runtime::samples::SampleLibrary::with_loading_sample_for_test("held"));
        engine
            .session
            .set_sample_library_for_test(Arc::clone(&library));
        // Async, so the rewind fires while its sample loads.
        engine.master.set_load_mode(LoadMode::Async);
        assert!(
            engine
                .arm_launch("s(\"held\")", false, 0.25, true)
                .expect("arm")
                .is_some()
        );
        tick_until_fired(&mut engine).expect("the launch fires");
        assert!(device_of(&engine).line_cut_armed(), "armed at fire");

        engine
            .evaluate_guarded("s(\"tri\")", false, || false)
            .expect("the edit");
        assert!(
            !device_of(&engine).line_cut_armed(),
            "the edit withdraws the held rewind's line cut"
        );
        assert!(engine.landing.is_none(), "the edit supersedes the landing");
        engine.tick(accepted).expect("tick after the edit");
        assert!(
            !device_of(&engine).line_cut_armed(),
            "and it stays withdrawn"
        );
        hold_clock(&engine, false);
        library.finish_loading_sample_for_test();
    }

    /// A pad race: A fires, B is armed over it, A is pressed again. When B
    /// is cancelled, A is still in flight and answers the repeat.
    #[test]
    fn cancelling_a_pending_launch_keeps_the_fired_one_in_flight() {
        let mut engine = silent_engine_with_pattern(rustel_core::silence());
        let install = arm_and_fire_launch(&mut engine, "s(\"tri\")");
        assert!(
            engine
                .arm_launch("s(\"saw\")", false, 0.25, true)
                .expect("arm B")
                .is_some()
        );
        engine.cancel_pending_launch();
        assert!(matches!(
            engine.take_launch_outcome(),
            Some(Err(RuntimeError::Cancelled))
        ));
        assert!(engine.landing.is_some(), "A is still in flight");

        engine
            .arm_launch("s(\"tri\")", false, 0.25, true)
            .expect("A again")
            .expect("answered");
        assert!(engine.pending_launch.is_none(), "nothing armed again");
        match engine.take_launch_outcome() {
            Some(Ok(again)) => assert_eq!(again.generation, install.generation),
            other => panic!("A's repeat must be answered with A, got {other:?}"),
        }
        hold_clock(&engine, false);
    }

    /// The second press of a fast double press usually arrives after the
    /// line. Inside the grace it is answered with the fired install:
    /// nothing is armed or installed again, and the countdown reads zero.
    #[test]
    fn a_repeat_press_just_after_the_line_is_answered_by_the_grace_memory() {
        let mut engine = silent_engine_with_pattern(rustel_core::silence());
        let source = "s(\"tri\")";
        let install = settle_rewind_into_grace(&mut engine, source);
        let (landed_line_cycle, landed_at) = {
            let recent = engine.recent_rewind.as_ref().expect("checked");
            (recent.boundary_cycle, recent.landed_at)
        };
        assert!(
            engine
                .live
                .as_ref()
                .expect("playing")
                .device
                .clock_seconds()
                >= landed_at,
            "the line has passed: this is the window the shipped guard does not cover"
        );

        assert!(
            engine.snapshot().launch.is_none(),
            "past the line there is no countdown to show, grace or not"
        );

        let repeat = engine
            .arm_launch(source, false, 0.25, true)
            .expect("repeat arm")
            .expect("the grace answers the repeat");
        assert_eq!(
            repeat.boundary_cycle, landed_line_cycle,
            "the answer names the line already landed, not a new one"
        );
        assert_eq!(
            repeat.seconds_left, 0.0,
            "the countdown reads zero: nothing is waited for"
        );
        assert!(
            engine.pending_launch.is_none(),
            "the repeat must not arm a second from-zero install"
        );
        assert!(
            engine.snapshot().launch.is_none(),
            "an answered repeat shows no countdown"
        );
        match engine.take_launch_outcome() {
            Some(Ok(again)) => {
                assert_eq!(
                    again.generation, install.generation,
                    "the repeat answers with the generation already sounding"
                );
                assert!(again.answered_repeat, "the answer says nothing installed");
            }
            other => {
                panic!("the repeat press must be answered with the fired install, got {other:?}")
            }
        }

        // Nothing fired again: the generation the launch installed plays on.
        let generation = engine.generation();
        let until = engine
            .live
            .as_ref()
            .expect("playing")
            .device
            .clock_seconds()
            + 0.5;
        while engine
            .live
            .as_ref()
            .expect("playing")
            .device
            .clock_seconds()
            < until
        {
            engine.tick(accepted).expect("tick");
            std::thread::sleep(Duration::from_millis(2));
        }
        assert_eq!(engine.generation(), generation, "no second install");
    }

    /// A repeat press is answered on the time its guard read. A second
    /// clock read could cross the line, or the end of the grace, and turn
    /// the press into "evaluate now".
    #[test]
    fn a_repeat_press_is_answered_on_the_time_its_guard_read() {
        let mut engine = silent_engine_with_pattern(rustel_core::silence());
        let source = "s(\"tri\")";
        arm_and_fire_launch(&mut engine, source);
        // The landing edge: the clock (held) is before the line, so move
        // the line a hair behind it and decide the press just before it.
        let now = clock_now(&engine);
        engine.landing.as_mut().expect("in flight").boundary_time = now - 0.001;
        let answer = engine
            .arm_launch_at(now - 0.002, source, false, 0.25, true)
            .expect("repeat")
            .expect("answered before the line, as the guard saw it");
        assert!(answer.seconds_left > 0.0);
        assert!(engine.pending_launch.is_none());
        assert!(matches!(
            engine.take_launch_outcome(),
            Some(Ok(StudioInstall {
                answered_repeat: true,
                ..
            }))
        ));

        // The grace edge: settle the landing, then age the memory so the
        // held clock is past the grace while the press was just inside it.
        engine.settle_landing();
        let recent = engine.recent_rewind.as_mut().expect("settled");
        let grace = recent.grace_secs;
        recent.landed_at = now - grace - 0.001;
        let landed_at = recent.landed_at;
        let answer = engine
            .arm_launch_at(landed_at + grace - 1e-6, source, false, 0.25, true)
            .expect("repeat")
            .expect("answered inside the grace, as the guard saw it");
        assert_eq!(answer.seconds_left, 0.0);
        assert!(engine.pending_launch.is_none());
        let _ = engine.take_launch_outcome();
        hold_clock(&engine, false);
    }

    /// An outside clock that retimes the transport during a countdown
    /// moves the time of the line's cycle. The fire, the takeover, cycle
    /// zero and the device cut must land on the moved bar line, not where
    /// it stood when the pad was pressed.
    #[test]
    fn a_launch_lands_on_its_cycle_line_after_the_transport_is_retimed() {
        let mut engine = silent_engine_with_pattern(rustel_core::silence());
        run_clock_past(&engine, 0.05);
        hold_clock(&engine, true);
        let info = engine
            .arm_launch("s(\"tri\")", false, 1.0, true)
            .expect("arm")
            .expect("armed");
        let now = clock_now(&engine);
        let cps = engine.session.cps();
        engine
            .session
            .retime(now, cps * 1.02, engine.session.cycle_at_time(now));
        let retimed = engine.launch_info().expect("still armed");
        assert!(
            (retimed.seconds_left - info.seconds_left / 1.02).abs() < 1e-6,
            "the countdown follows the new tempo: {} vs {}",
            retimed.seconds_left,
            info.seconds_left
        );
        hold_clock(&engine, false);

        // Where the line's cycle falls under the new mapping. (The rewind
        // puts cycle zero there once it fires, so read it before.)
        let moved_line = now + retimed.seconds_left;
        tick_until_fired(&mut engine).expect("the launch fires");
        let landing = engine.landing.as_ref().expect("in flight");
        assert!(
            (landing.boundary_time - moved_line).abs() * 48_000.0 < 1.0,
            "the landing waits for the moved line: {} vs {moved_line}",
            landing.boundary_time
        );
        // The same tick's producer step usually publishes the flip, which
        // carries the cut itself and clears the arm; one still armed must
        // name the moved line.
        let word = device_of(&engine).armed_line_word();
        if word & 0b01 != 0 {
            assert_eq!(
                word >> 2,
                (landing.boundary_time * 48_000.0).round() as u64,
                "the device cut is armed at the moved line"
            );
        }
        hold_clock(&engine, false);
    }

    /// The grace is short on purpose: past it, the same score pressed again
    /// is a genuine restart and arms a fresh launch, as a rewind should.
    #[test]
    fn a_repeat_press_past_the_grace_restarts_for_real() {
        let mut engine = silent_engine_with_pattern(rustel_core::silence());
        let source = "s(\"tri\")";
        settle_rewind_into_grace(&mut engine, source);
        let recent = engine
            .recent_rewind
            .as_mut()
            .expect("the landing settled into the grace memory");
        recent.landed_at -= recent.grace_secs + 0.1;

        let restart = engine
            .arm_launch(source, false, 0.25, true)
            .expect("restart arm")
            .expect("an expired memory is no answer");
        assert!(restart.seconds_left > 0.0, "a fresh line is waited for");
        assert!(
            engine.pending_launch.is_some(),
            "the expired press arms a fresh launch"
        );
        assert!(
            engine.take_launch_outcome().is_none(),
            "it waits for its own fire, not answered with the old install"
        );
        engine.supersede_launch();
        let _ = engine.take_launch_outcome();
    }

    /// The grace is half the launch's unit at its tempo, with no cap. At
    /// 0.5 cps that is 0.25 s for a beat, 1 s for a cycle and 4 s for a
    /// four-cycle phrase.
    #[test]
    fn a_repeat_press_belongs_to_the_nearest_line_for_every_quantise_size() {
        assert_eq!(rewind_grace_secs(0.25, 0.5), 0.25);
        assert_eq!(rewind_grace_secs(1.0, 0.5), 1.0);
        assert_eq!(rewind_grace_secs(4.0, 0.5), 4.0);
        assert_eq!(rewind_grace_secs(8.0, 0.5), 8.0);
        assert_eq!(rewind_grace_secs(0.25, 2.0), 0.0625);
        for (unit, cps) in [
            (0.0, 0.5),
            (0.25, 0.0),
            (f64::NAN, 0.5),
            (0.25, f64::INFINITY),
        ] {
            assert_eq!(
                rewind_grace_secs(unit, cps),
                0.0,
                "no line to be near, no grace: {unit} at {cps}"
            );
        }
    }

    /// With beat quantise at `setCpm(120/4)`, a rewind pressed again and
    /// again restarts the score on each beat line. A press just after the
    /// line is still the launch that landed. A press half a beat later
    /// arms the next beat.
    #[test]
    fn a_rewind_pressed_again_on_the_next_beat_restarts_on_that_beat() {
        let mut engine = silent_engine_with_pattern(rustel_core::silence());
        assert_eq!(engine.session.cps(), 0.5, "120 BPM with four beats a cycle");
        let source = "s(\"tri\")";
        settle_rewind_into_grace(&mut engine, source);
        hold_clock(&engine, true);
        let now = clock_now(&engine);
        let recent = engine.recent_rewind.as_mut().expect("settled");
        assert_eq!(
            recent.grace_secs, 0.25,
            "a beat launch's grace is half a beat"
        );

        // A double landing just after the line: answered, nothing armed.
        recent.landed_at = now - 0.05;
        engine
            .arm_launch_at(now, source, false, 0.25, true)
            .expect("repeat")
            .expect("answered");
        assert!(engine.pending_launch.is_none(), "a double is not a restart");
        assert!(matches!(
            engine.take_launch_outcome(),
            Some(Ok(StudioInstall {
                answered_repeat: true,
                ..
            }))
        ));

        // The press on the next beat (0.3 s after the line: past half a
        // beat, before the next line's head-room): a fresh rewind on it.
        engine.recent_rewind.as_mut().expect("still held").landed_at = now - 0.3;
        let next = engine
            .arm_launch_at(now, source, false, 0.25, true)
            .expect("restart arm")
            .expect("armed for the next beat");
        assert!(
            engine.pending_launch.is_some(),
            "the next beat's press arms a restart instead of being answered"
        );
        assert!(
            engine.take_launch_outcome().is_none(),
            "it waits for its own line"
        );
        // The next beat line past the launch head-room: at most one beat
        // plus the head-room away, never a second beat further.
        assert!(
            next.seconds_left > 0.0 && next.seconds_left <= 0.5 + engine.launch_headroom(),
            "the restart lands on the next beat line: {}",
            next.seconds_left
        );
        engine.supersede_launch();
        let _ = engine.take_launch_outcome();
        hold_clock(&engine, false);
    }

    /// A different score is an edit even inside the grace: it arms a fresh
    /// launch and is not answered with the install it would replace.
    #[test]
    fn an_edit_inside_the_grace_restarts_for_real() {
        let mut engine = silent_engine_with_pattern(rustel_core::silence());
        settle_rewind_into_grace(&mut engine, "s(\"tri\")");

        let edit = engine
            .arm_launch("s(\"saw\")", false, 0.25, true)
            .expect("edit arm")
            .expect("a different score is not answered by the grace");
        assert!(edit.seconds_left > 0.0, "a fresh line is waited for");
        assert!(
            engine.pending_launch.is_some(),
            "the edit arms a fresh launch"
        );
        assert!(
            engine.take_launch_outcome().is_none(),
            "it waits for its own fire, not answered with the old install"
        );
        engine.supersede_launch();
        let _ = engine.take_launch_outcome();
    }

    /// The sliver the first fix left open: press 2 arrives after the line
    /// has passed but BEFORE any turn's tick has settled the landing, so
    /// the landing still dangles. Settling inside arm_launch turns it into
    /// the answerable memory; nothing is armed, nothing re-installs.
    #[test]
    fn a_press_between_the_line_and_the_next_tick_is_still_answered() {
        let mut engine = silent_engine_with_pattern(rustel_core::silence());
        let source = "s(\"tri\")";
        // Arm first. The fire happens inside the head-room, before the
        // line, and the same tick's settle cannot touch a landing whose
        // line is still ahead: that is the natural dangling state.
        let armed = engine
            .arm_launch(source, false, 0.25, true)
            .expect("arm")
            .expect("a playing engine arms a quantised launch");
        let boundary_cycle = armed.boundary_cycle;

        // Tick until the fire. The landing is left waiting for its line.
        let install = match tick_until_fired(&mut engine) {
            Ok(install) => install,
            Err(error) => panic!("the launch failed: {error:?}"),
        };
        assert!(engine.landing.is_some(), "the fired landing waits");
        assert!(
            engine
                .live
                .as_ref()
                .expect("playing")
                .device
                .clock_seconds()
                < engine.landing.as_ref().expect("checked").boundary_time,
            "the fire lands inside the head-room, before the line"
        );
        hold_clock(&engine, false);

        // Now cross the line without a tick: the worker reaches the next
        // tick only after it handles a command that arrives here.
        let until = engine
            .landing
            .as_ref()
            .expect("the landing is still there")
            .boundary_time
            + 0.02;
        while engine
            .live
            .as_ref()
            .expect("playing")
            .device
            .clock_seconds()
            < until
        {
            std::thread::sleep(Duration::from_millis(2));
        }
        // The dangling state this test is about: fired, line passed, no
        // turn's settle has run since.
        assert!(
            engine.landing.is_some(),
            "the landing dangles past its line"
        );
        assert!(engine.recent_rewind.is_none(), "not settled yet");

        // Press 2 lands in the sliver.
        let repeat = engine
            .arm_launch(source, false, 0.25, true)
            .expect("repeat arm")
            .expect("the settled memory answers the repeat");
        assert_eq!(
            repeat.boundary_cycle, boundary_cycle,
            "answered with the line already landed"
        );
        assert!(engine.pending_launch.is_none(), "nothing was armed again");
        match engine.take_launch_outcome() {
            Some(Ok(again)) => assert_eq!(
                again.generation, install.generation,
                "the answer is the generation already sounding"
            ),
            other => panic!("the sliver press must be answered, got {other:?}"),
        }
    }
}
