use super::*;

impl Session {
    /// Update one slider registered by the active score. Callers are
    /// responsible for revision/id/range policy. This boundary performs the
    /// finite host-cell mutation and reports an unknown id as `false`.
    ///
    /// A slider that is the value of a plugin key also moves the parameter
    /// value at once, on a live output that plays: the next turn of the
    /// output gives the value to the copy, and to each note in its queue of
    /// this generation or older. The move makes no new generation. While the
    /// transport is stopped, no parameter value moves: the next start reads
    /// the slider. After a score change, a slider moves no parameter value
    /// until a note of the new score links it.
    pub fn set_slider_value(&self, id: &str, value: f64) -> Result<bool, RuntimeError> {
        let moved = self
            .js
            .set_slider_value(id, value)
            .map_err(RuntimeError::Js)?;
        // A move of a score with no link takes no lock of the chains.
        #[cfg(feature = "vst")]
        if moved
            && self.slider_links.load(std::sync::atomic::Ordering::Relaxed)
            && self.plugins_live()
            && !self.transport.is_stopped()
        {
            crate::vst::slider_moved(&self.chains, id, value, self.generation());
        }
        Ok(moved)
    }

    /// Current value of one active query-time slider cell.
    pub fn slider_value(&self, id: &str) -> Result<Option<f64>, RuntimeError> {
        self.js.slider_value(id).map_err(RuntimeError::Js)
    }

    /// Opaque direct-control token for the currently committed slider cell.
    pub fn slider_binding(&self, id: &str) -> Option<u64> {
        self.js.slider_binding(id)
    }

    /// Invalidate the safely replaceable future after a query-time control
    /// mutation, preserving the active graph and musical phase.
    ///
    /// The returned generations must be handed to the live producer before
    /// its next step. That producer prefills the new generation and publishes
    /// it to the device together with [`Self::take_requery_takeover_time`], so
    /// already-audible old events survive while stale prefetched events do not.
    pub fn requery_active_at(&mut self, now: f64) -> Result<Option<(u64, u64)>, RuntimeError> {
        if !now.is_finite() || now < 0.0 {
            return Err(RuntimeError::Message(format!(
                "control requery time must be finite and non-negative, got {now}"
            )));
        }
        if self.transport.is_stopped() {
            return Ok(None);
        }
        let margin = self.continuity_margin.unwrap_or(self.schedule_lead);
        let takeover_time = now + margin.max(0.0);
        let generation_before = self.generation();
        // A rewind that is installed but not yet published owns the
        // takeover: its line (or its edit instant) and its cut. A slider,
        // key or clock-steer requery in that window re-queries the same
        // graph, which the from-zero install already clamps to cycle zero,
        // so it publishes at the rewind's takeover. With only the time
        // replaced, the `AtTakeover` cut would apply at `now + margin`,
        // before the rewind's line.
        let pending_rewind = (self.requery_takeover_cut != TakeoverCut::None)
            .then_some(self.requery_takeover_time)
            .flatten();
        let takeover_time = pending_rewind.unwrap_or(takeover_time);
        // A live device drops the outgoing onsets from the takeover frame,
        // so the cursor starts on that frame's edge. A pending rewind keeps
        // its own takeover: its cut ends the outgoing score, and its first
        // cycle begins the new one.
        let takeover_edge = match pending_rewind {
            None => self.takeover_frame_edge(takeover_time),
            Some(_) => None,
        };
        let requeried = match takeover_edge {
            Some(edge) => self.scheduler.requery_active_from(now, edge),
            None => self.scheduler.requery_active(now, takeover_time),
        };
        let Some(generation_after) = requeried else {
            return Ok(None);
        };
        // The device swap retires a key press that the outgoing generation
        // pinned at or after the takeover frame, so that pin names a schedule
        // that never plays. Forget it so the re-query's own pass re-claims
        // the press at the takeover frame, the earliest moment the device
        // can sound it. Otherwise the player waits for a frontier that a
        // heavy sibling pattern can push out of reach. A pin before the
        // takeover stays: those frames are already in the device's lookahead
        // and keep sounding across the takeover, so re-claiming one would
        // sound the press twice.
        self.js.midi_input_bus().forget_key_placements_from(
            self.scheduler
                .cycle_at_time(takeover_edge.unwrap_or(takeover_time)),
        );
        // A slider/control re-query keeps the same graph and therefore the
        // same MIDI-input handles, but the listener's audible/session union
        // follows scheduler generations. Republish the retained input set at
        // the exact generation bump so it cannot be retired as stale.
        self.js
            .midi_input_bus()
            .republish_current_generation(generation_after);
        self.requery_takeover_time = Some(takeover_time);
        if pending_rewind.is_none() {
            // An ordinary control requery rings out: no rewind shape may
            // ride along with its time.
            self.requery_takeover_cut = TakeoverCut::None;
            self.requery_anchor_time = None;
        }
        // A pending rewind is a new score that the device does not play yet.
        self.requeried_generation = pending_rewind.is_none().then_some(generation_after);
        Ok(Some((generation_before, generation_after)))
    }

    /// Refill a live device after its output stream discarded the complete
    /// event ring. Unlike an ordinary control re-query, no old audio horizon
    /// remains to cover a continuity takeover: resume at the replacement
    /// stream's safe scheduling lead and publish that exact cursor with the
    /// recovery generation.
    #[cfg(feature = "device-audio")]
    pub fn requery_after_output_recycle_at(
        &mut self,
        now: f64,
    ) -> Result<Option<(u64, u64)>, RuntimeError> {
        if !now.is_finite() || now < 0.0 {
            return Err(RuntimeError::Message(format!(
                "output recycle requery time must be finite and non-negative, got {now}"
            )));
        }
        if self.transport.is_stopped() {
            return Ok(None);
        }
        let cursor_time = now + self.schedule_lead.max(0.0);
        let generation_before = self.generation();
        let Some(generation_after) = self.scheduler.requery_active(now, cursor_time) else {
            return Ok(None);
        };
        // Same contract as the control re-query: a pin at or after the cursor
        // names audio the discarded ring never carried, so the replacement
        // pass must re-claim the press at the cursor rather than leave it
        // parked ahead.
        self.js
            .midi_input_bus()
            .forget_key_placements_from(self.scheduler.cycle_at_time(cursor_time));
        self.js
            .midi_input_bus()
            .republish_current_generation(generation_after);
        self.requery_takeover_time = Some(cursor_time);
        // The recycled stream starts empty: there is no outgoing rendition
        // to cut, and a rewind shape left standing would be published on
        // this recovery generation's cursor instead of its own line.
        self.requery_takeover_cut = TakeoverCut::None;
        self.requery_anchor_time = None;
        self.requeried_generation = Some(generation_after);
        // The new output holds no plugin copy yet, so the looks at the
        // cycles ahead start again at the cycle in play.
        #[cfg(feature = "vst")]
        self.plugin_ahead.restart();
        // Nor does it hold a latency-compensated event from before the
        // cursor: the recovery generation sounds every onset it queries.
        self.compensated_onsets.clear();
        // The OSC and serial messages that are out were timed against the
        // stream that failed. The recovery generation sends its own, in
        // time with the audio it plays.
        self.forget_handed_out();
        // These intents came from the discarded audio horizon. Letting them
        // cross after the same-graph generation bump would produce external
        // notes or packets with no corresponding recovered audio onset.
        #[cfg(feature = "midi")]
        self.pending_midi.clear();
        #[cfg(feature = "osc")]
        self.pending_osc.clear();
        #[cfg(feature = "serial")]
        self.pending_serial.clear();
        Ok(Some((generation_before, generation_after)))
    }

    pub(super) fn ensure_bindings(&mut self) -> Result<(), RuntimeError> {
        if self.bindings_ready {
            return Ok(());
        }
        self.js
            .install_semantic_bindings()
            .map_err(RuntimeError::Js)?;
        // `s` is not an alias of `m`. The mini pattern is the argument to the
        // `s` control, so `s("bd sd")` produces `{s: "bd"}` control objects,
        // not bare `"bd"` values.
        self.bindings_ready = true;
        Ok(())
    }

    /// Install an explicit Rust `Pattern` as the active graph (no JS).
    pub fn set_pattern(&mut self, pattern: Pattern) -> Result<(), RuntimeError> {
        self.with_panic_recovery(0.0, |session| session.set_pattern_at(pattern, 0.0, true))
    }

    /// Select the random-number behavior for this Session only.
    pub fn set_rng_mode(&mut self, mode: rustel_core::rng::RngMode) {
        self.with_runtime_settings(|| rustel_core::rng::use_rng(mode));
    }

    /// Select the default join alignment for graph construction and deferred
    /// composers in this Session.
    pub fn set_default_join(&mut self, alignment: rustel_core::compose::Alignment) {
        self.with_runtime_settings(|| rustel_core::compose::set_default_alignment(alignment));
    }

    /// Select or reset the default voicing dictionary for this Session.
    pub fn set_default_voicings(&mut self, name: Option<&str>) {
        self.with_runtime_settings(|| match name {
            Some(name) => rustel_core::voicings::set_default_voicings(name),
            None => rustel_core::voicings::reset_default_voicings(),
        });
    }

    /// Run `f` with this Session's module settings selected.
    ///
    /// Calls to the native RNG, join, and voicing setters affect this Session
    /// and no other. A returned graph acquires Session ownership when it is
    /// installed with [`Session::set_pattern`].
    pub fn with_runtime_settings<R>(&mut self, f: impl FnOnce() -> R) -> R {
        self.claim_core_settings();
        self.js.with_runtime_settings(f)
    }

    pub(super) fn claim_core_settings(&mut self) {
        if !self.core_settings_owned {
            if !self.core_settings_initialized {
                self.js.inherit_current_runtime_settings();
                self.core_settings_initialized = true;
            }
            self.core_settings_owned = true;
        }
    }

    pub(super) fn set_pattern_at(
        &mut self,
        pattern: Pattern,
        now: f64,
        restart_transport: bool,
    ) -> Result<(), RuntimeError> {
        let settings =
            (!self.core_settings_owned).then(|| self.js.snapshot_ambient_runtime_settings());
        // A direct Pattern names no takeover: the device refuses the
        // outgoing generation's unplayed events from its flip, which the
        // edit instant stands for.
        let key_takeover_cycle = self.scheduler.cycle_at_time(now);
        let result = self.install_pattern_at(
            pattern,
            now,
            restart_transport,
            None,
            None,
            key_takeover_cycle,
        );
        if result.is_ok()
            && let Some(settings) = settings
        {
            self.js.adopt_runtime_settings(&settings);
            self.core_settings_initialized = true;
        }
        // A direct pattern has no text, so it has no lanes: each lane of the
        // score before leaves.
        #[cfg(feature = "vst")]
        if result.is_ok() {
            self.lanes.install_pattern();
            let live = self.plugins_live();
            crate::vst::score_changed(&self.chains, &self.lanes, None, live);
            crate::vst::forget_lanes_away(&self.chains, &mut self.lanes);
            #[cfg(feature = "device-audio")]
            self.plugin_ahead.restart();
        }
        result
    }

    pub(super) fn install_pattern_at(
        &mut self,
        pattern: Pattern,
        now: f64,
        restart_transport: bool,
        overlap_cycle: Option<(f64, f64)>,
        takeover_edge: Option<f64>,
        key_takeover_cycle: f64,
    ) -> Result<(), RuntimeError> {
        // A new score: its missing channels are its own news.
        self.input_warnings_reported.clear();
        self.refusals_reported.clear();
        self.plugin_lines_reported.clear();
        self.ensure_bindings()?;
        let builder = self.js.builder();
        self.js
            .set_active(&builder, pattern.clone())
            .map_err(RuntimeError::Js)?;
        let expected_generation = self.generation().checked_add(1).ok_or_else(|| {
            RuntimeError::ResourceLimit("scheduler generation counter exhausted".into())
        })?;
        // Native/mini graphs cannot name `midin()` ports. Publish their empty
        // set only after the fallible JS wrapper installation succeeds and
        // immediately before the infallible scheduler generation commit.
        self.js
            .commit_empty_midi_input_generation(expected_generation)
            .map_err(RuntimeError::Js)?;
        let generation = match overlap_cycle {
            Some(overlap) => {
                self.replace_pattern_continued(pattern, now, None, overlap, takeover_edge)
            }
            None => self.scheduler.set_pattern(pattern, now),
        };
        debug_assert_eq!(generation, expected_generation);
        if restart_transport {
            self.js.midi_input_bus().clear_keys();
            self.transport.start();
        } else if !self.transport.is_stopped() {
            // Same boundary as the JavaScript path: a pattern installed over
            // a running transport must not inherit the outgoing graph's
            // pinned key backlog, and must still hear the presses it never
            // placed or whose pins the takeover drops. See the note in
            // `commit_evaluated_score`.
            self.js
                .midi_input_bus()
                .retire_key_backlog(key_takeover_cycle);
        }
        // A direct Rust Pattern has no textual score the live watchdog can
        // reconstruct. Keep the previous audible snapshot until this
        // candidate reaches the device, but do not let a later successful
        // cutover mislabel the Pattern as the last JavaScript/mini source.
        self.last_source = None;
        self.last_path = EvaluateSource::Pattern;
        self.js.snapshot_active_as_last_good();
        Ok(())
    }

    /// Replace the pattern as a continued replacement over `overlap`: the
    /// cycle at the edit and the seconds from it to the takeover (see
    /// `commit_evaluated_score`).
    ///
    /// A live device drops the outgoing copies from a takeover frame. With
    /// a `takeover_edge` ([`Self::takeover_frame_edge`]) the pre-marked
    /// window ends on that frame's edge. Without one no device applies a
    /// takeover frame, and the window ends at the takeover time.
    pub(super) fn replace_pattern_continued(
        &mut self,
        pattern: Pattern,
        now: f64,
        cps: Option<f64>,
        (cycle_now, takeover): (f64, f64),
        takeover_edge: Option<f64>,
    ) -> u64 {
        // The window ends where the outgoing rate reaches the takeover: the
        // device holds the outgoing copies on the times that rate gave
        // them. A window on the new rate sounds onsets twice for a slower
        // score and skips onsets for a faster score.
        //
        // Exception: a score more than twice as slow. With the window on
        // the outgoing rate, its replacement has nothing to play until
        // `margin * old / new` seconds after the edit: 2.5 s for a 0.25 s
        // margin at a tenth of the rate. So the window ends where the new
        // rate is one margin past the takeover. The replacement resumes
        // there and plays the later outgoing copies again.
        let outgoing = self.scheduler.cps();
        let rate = outgoing.min(2.0 * cps.unwrap_or(outgoing));
        let takeover_cycle = cycle_now + takeover * rate;
        match takeover_edge {
            Some(edge) => self.scheduler.replace_pattern_continued_until(
                pattern,
                now,
                cps,
                cycle_now,
                edge,
                // Only the exception ends the window before the edge.
                (rate < outgoing).then_some(takeover_cycle),
            ),
            None => self.scheduler.replace_pattern_continued(
                pattern,
                now,
                cps,
                cycle_now,
                takeover_cycle,
            ),
        }
    }

    /// The first instant whose onset lands on the takeover frame `F` of a
    /// takeover at `takeover_time`. An onset takes the first frame at or
    /// after its time, so the edge is just after frame `F - 1`:
    ///
    /// ```text
    /// frame        F - 1                F
    /// time     ------|------------------|------>
    ///                 ^ edge            ^ takeover_time, rounded
    /// outgoing   kept | dropped
    /// ```
    ///
    /// The device drops the outgoing onsets from the edge, so the incoming
    /// generation must play them. A split at the takeover time gives the
    /// onsets between the two marks to neither generation. So the window of
    /// a continued replacement, the cursor of a requery and the key pins
    /// all split the two generations on this edge.
    ///
    /// `None` until a live producer has scheduled, and for a takeover time
    /// that is not finite.
    pub(super) fn takeover_frame_edge(&self, takeover_time: f64) -> Option<f64> {
        let sample_rate = self.live_sample_rate?;
        takeover_time.is_finite().then(|| {
            crate::render::onset_frame_edge(
                crate::render::takeover_frame_at(takeover_time, sample_rate),
                sample_rate,
            )
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::tests::*;
    use rustel_core::Value;
    #[cfg(test)]
    mod lane_throw_probe_tests {
        //! The replacement probe and a `$:` lane that throws. Playback contains a
        //! lane's throw to that lane, but the probe still refuses a candidate whose
        //! probed cycle throws in any lane: the last-good score keeps playing. A lane
        //! that first throws after the probed cycle installs and only it goes silent.

        use super::*;
        use crate::session::test_support::{quiet_session, warnings};

        const PLAYING: &str = r#"$: s("bd*4")"#;
        const BROKEN_LANE: &str = r#"$: note("0 4".add("x")).s("sine")"#;
        /// Adds 0 on cycle 0, the probed cycle, and throws on cycle 1.
        const LATE_BROKEN_LANE: &str = r#"$: note("0 4".add("<0 x>")).s("sine")"#;
        const REFUSED: &str = "replacement query failed; last-good score kept";

        fn two_lanes(second: &str) -> String {
            format!("$: s(\"bd*2 sd\")\n{second}")
        }

        /// Onsets of `sound` that start in `cycle`, read from their `n/d` labels.
        fn onsets_in(report: &PlayReport, cycle: i64, sound: &str) -> usize {
            let starts_in = |onset: &&OnsetEventJson| {
                let (n, d) = onset.whole_begin.split_once('/').expect("n/d label");
                let n: i64 = n.parse().expect("numerator");
                n.div_euclid(d.parse().expect("denominator")) == cycle
            };
            let sound = format!("s:{sound}");
            report
                .onsets
                .iter()
                .filter(starts_in)
                .filter(|onset| onset.value_show.contains(&sound))
                .count()
        }

        fn assert_refused_and_old_score_plays(
            session: &mut Session,
            error: &RuntimeError,
            generation: u64,
        ) {
            assert_eq!(error.kind(), "evaluation");
            let message = error.to_string();
            assert!(message.contains(REFUSED), "{message}");
            assert!(message.contains("cannot parse as numeral"), "{message}");
            assert_eq!(session.generation(), generation);
            assert_eq!(session.active_source(), Some(PLAYING));
            let report = session.play(2.0).expect("play");
            assert_eq!(onsets_in(&report, 0, "bd"), 4, "{:?}", report.onsets);
            assert!(
                report.onsets.iter().all(|onset| onset.value_show == "s:bd"),
                "the refused candidate was published: {:?}",
                report.onsets
            );
            assert_eq!(report.query_threw, None);
            // The refusal is the whole report: no warning and no console line.
            let diagnostics = session.take_diagnostics();
            assert!(
                diagnostics
                    .iter()
                    .all(|diagnostic| !matches!(diagnostic.kind.as_str(), "query-threw" | "log")),
                "{diagnostics:?}"
            );
        }

        #[test]
        fn an_edit_whose_second_lane_throws_is_refused_and_the_old_score_plays() {
            let mut session = quiet_session(PLAYING);
            let generation = session.generation();
            let error = session
                .reload_at(&two_lanes(BROKEN_LANE), false, 0.25)
                .expect_err("a lane that throws on the probed cycle refuses the edit");
            assert_refused_and_old_score_plays(&mut session, &error, generation);
        }

        #[test]
        fn an_edit_whose_only_lane_throws_is_refused_with_the_uncontained_error() {
            let mut session = quiet_session(PLAYING);
            let generation = session.generation();
            let error = session
                .reload_at(BROKEN_LANE, false, 0.25)
                .expect_err("a lane that throws on the probed cycle refuses the edit");
            assert_refused_and_old_score_plays(&mut session, &error, generation);

            // Without `$:` no stack contains the throw; the report is the same.
            let mut uncontained = quiet_session(PLAYING);
            let expected = uncontained
                .reload_at(BROKEN_LANE.trim_start_matches("$: "), false, 0.25)
                .expect_err("an uncontained throw refuses the edit");
            assert_eq!(error.to_string(), expected.to_string());
        }

        #[test]
        fn a_callback_bearing_edit_whose_lane_throws_is_refused_the_same_way() {
            let mut session = quiet_session(PLAYING);
            let generation = session.generation();
            let error = session
                .reload_at(
                    &format!("$: s(\"bd*2 sd\").fmap(x => x)\n{BROKEN_LANE}"),
                    false,
                    0.25,
                )
                .expect_err("a lane that throws on the probed cycle refuses the edit");
            assert_refused_and_old_score_plays(&mut session, &error, generation);
        }

        #[test]
        fn a_lane_that_first_throws_after_the_probed_cycle_installs_and_only_it_goes_silent() {
            let mut session = quiet_session(PLAYING);
            let generation = session.generation();
            let source = two_lanes(LATE_BROKEN_LANE);
            session
                .reload_at(&source, false, 0.25)
                .expect("the probed cycle does not throw");
            assert_ne!(session.generation(), generation);
            assert_eq!(session.active_source(), Some(source.as_str()));

            let report = session.play(4.0).expect("play two cycles");
            let lanes_in =
                |cycle| ["bd", "sd", "sine"].map(|sound| onsets_in(&report, cycle, sound));
            assert_eq!(lanes_in(0), [2, 1, 2], "{:?}", report.onsets);
            assert_eq!(lanes_in(1), [2, 1, 0], "{:?}", report.onsets);
            let warnings = warnings(&mut session);
            assert_eq!(warnings.len(), 1, "{warnings:?}");
            assert!(
                warnings[0].contains("cannot parse as numeral"),
                "{warnings:?}"
            );
        }

        #[test]
        fn a_valid_edit_installs_as_before() {
            let mut session = quiet_session(PLAYING);
            let generation = session.generation();
            let source = two_lanes(r#"$: note("0 4").s("sine")"#);
            session.reload_at(&source, false, 0.25).expect("valid edit");
            assert_ne!(session.generation(), generation);
            assert_eq!(session.active_source(), Some(source.as_str()));
            let report = session.play(2.0).expect("play");
            let lanes = ["bd", "sd", "sine"].map(|sound| onsets_in(&report, 0, sound));
            assert_eq!(lanes, [2, 1, 2], "{:?}", report.onsets);
            assert_eq!(report.query_threw, None);
            assert!(warnings(&mut session).is_empty());
        }

        #[test]
        fn a_first_score_with_a_throwing_lane_installs_when_its_first_query_is_not_probed() {
            let mut session = Session::new().expect("session");
            session.set_direct_diagnostic_logging(false);
            session
                .reload_at(&two_lanes(BROKEN_LANE), false, 0.0)
                .expect("a first score is not probed under the default hap budget");
            assert_eq!(session.generation(), 1);
            let report = session.play(2.0).expect("play");
            let lanes = ["bd", "sd", "sine"].map(|sound| onsets_in(&report, 0, sound));
            assert_eq!(lanes, [2, 1, 0], "{:?}", report.onsets);
            assert!(
                report
                    .query_threw
                    .as_deref()
                    .is_some_and(|message| message.contains("cannot parse as numeral")),
                "{:?}",
                report.query_threw
            );
        }

        #[test]
        fn a_first_score_whose_pure_query_is_probed_is_refused_when_a_lane_throws() {
            let mut session = Session::new().expect("session");
            session.set_query_hap_budget(1_000).expect("hap budget");
            let error = session
                .reload_at(&two_lanes(BROKEN_LANE), false, 0.0)
                .expect_err("a probed first score refuses a throwing lane");
            let message = error.to_string();
            assert!(
                message.contains("score query failed; score was not installed"),
                "{message}"
            );
            assert!(message.contains("cannot parse as numeral"), "{message}");
            assert_eq!(session.generation(), 0);
        }
    }

    /// The `$:` form registers into a stack rather than returning a pattern,
    /// so commenting the only `$:` line out is a different path from
    /// commenting out a bare expression.
    #[test]
    fn commenting_out_the_only_dollar_line_goes_silent() {
        let mut session = Session::with_config(SessionConfig::default()).expect("session");
        session.evaluate("$: s('bd*4')").expect("initial evaluate");
        let playing = session
            .query(Fraction::new(0, 1), Fraction::new(1, 1))
            .expect("query");
        assert!(!playing.is_empty(), "the set never started");

        session
            .evaluate("// $: s('bd*4')")
            .expect("a commented-out score is accepted");
        let muted = session
            .query(Fraction::new(0, 1), Fraction::new(1, 1))
            .expect("query");
        assert!(
            muted.is_empty(),
            "commenting the only $: line left {} events playing",
            muted.len()
        );
    }

    /// An evaluate over a running transport retires the key backlog that the
    /// outgoing graph placed. Without the flush, the incoming generation
    /// re-renders up to two seconds of pinned note-ons.
    #[test]
    fn an_evaluate_over_a_running_transport_flushes_the_pinned_key_backlog() {
        let mut session = Session::new().expect("session");
        session
            .evaluate(r#"const kb = await midikeys('keyboard'); kb(0.5).s('piano')"#)
            .expect("first score");
        session.transport().start();

        // Spam the pads: a burst of note-ons the first score's queries pin
        // at future cycle positions.
        let port = session
            .midi_input_bus()
            .find("keyboard")
            .expect("keyboard port");
        for _ in 0..32 {
            port.observe_note_on(rustel_core::midi_in::now_nanos(), 1, 60, 100);
        }
        let mut placed = Vec::new();
        port.keys.select(
            0.0,
            1.0,
            rustel_core::midi_in::now_nanos(),
            Some((0, 1)),
            &mut placed,
        );
        assert_eq!(
            placed.len(),
            32,
            "the burst must be placeable before the evaluate"
        );

        // A live editor save: a different score over the running transport,
        // no restart - the path the studio actually sends.
        let stopped = std::sync::atomic::AtomicBool::new(false);
        session
            .reload_at_cancellable(
                r#"const kb = await midikeys('keyboard'); kb(0.5).s('sd:4')"#,
                false,
                0.5,
                &stopped,
            )
            .expect("live replacement");

        // The presses belonged to the score that was sounding when they
        // landed. The new generation starts with an empty key memory: what
        // the old graph already scheduled keeps sounding, but the unrendered
        // backlog must not re-render through the new voices.
        let port = session
            .midi_input_bus()
            .find("keyboard")
            .expect("keyboard port survives the reload");
        let mut after = Vec::new();
        port.keys.select(
            0.0,
            1.0,
            rustel_core::midi_in::now_nanos(),
            Some((0, 1)),
            &mut after,
        );
        assert!(
            after.is_empty(),
            "the evaluate flushed the key memory, not carried the backlog across: {} hits",
            after.len()
        );
    }

    /// A press that lands after the outgoing graph's last query, while an
    /// evaluate is in flight, is not placed. It belongs to the incoming
    /// score and survives the backlog flush.
    #[test]
    fn an_evaluate_over_a_running_transport_keeps_the_press_nobody_placed() {
        let mut session = Session::new().expect("session");
        session
            .evaluate(r#"const kb = await midikeys('keyboard'); kb(0.5).s('piano')"#)
            .expect("first score");
        session.transport().start();
        let port = session
            .midi_input_bus()
            .find("keyboard")
            .expect("keyboard port");
        for _ in 0..32 {
            port.observe_note_on(rustel_core::midi_in::now_nanos(), 1, 60, 100);
        }
        let mut placed = Vec::new();
        port.keys.select(
            0.0,
            1.0,
            rustel_core::midi_in::now_nanos(),
            Some((0, 1)),
            &mut placed,
        );
        assert_eq!(placed.len(), 32, "the outgoing score placed its backlog");
        // Played during the evaluate: no query has seen it.
        port.observe_note_on(rustel_core::midi_in::now_nanos(), 1, 67, 90);

        let stopped = std::sync::atomic::AtomicBool::new(false);
        session
            .reload_at_cancellable(
                r#"const kb = await midikeys('keyboard'); kb(0.5).s('sd:4')"#,
                false,
                0.5,
                &stopped,
            )
            .expect("live replacement");

        let port = session
            .midi_input_bus()
            .find("keyboard")
            .expect("keyboard port survives the reload");
        let mut after = Vec::new();
        port.keys.select(
            0.0,
            1.0,
            rustel_core::midi_in::now_nanos(),
            Some((1, 2)),
            &mut after,
        );
        assert_eq!(
            after
                .iter()
                .map(|hit| (hit.note, hit.velocity))
                .collect::<Vec<_>>(),
            [(67, 90)],
            "exactly the unplaced press is heard by the new score"
        );
    }

    /// The mini install door (`install_pattern_at`, which a mini reload and
    /// `set_pattern` share) keeps the same boundary as the JavaScript one:
    /// the pinned backlog is retired and the press nobody placed survives
    /// for the score taking over.
    #[test]
    fn a_mini_reload_over_a_running_transport_keeps_the_press_nobody_placed() {
        let mut session = Session::new().expect("session");
        session
            .evaluate(r#"const kb = await midikeys('keyboard'); kb(0.5).s('piano')"#)
            .expect("first score");
        session.transport().start();
        // The MIDI listener reports the sounding generation, as it does
        // while a device plays: a mini score names no ports, and only the
        // audible generation keeps the keyboard's port on the bus through
        // the install.
        let generation = session.generation();
        session
            .midi_input_bus()
            .snapshot_for(generation, generation);
        let port = session
            .midi_input_bus()
            .find("keyboard")
            .expect("keyboard port");
        for _ in 0..32 {
            port.observe_note_on(rustel_core::midi_in::now_nanos(), 1, 60, 100);
        }
        let mut placed = Vec::new();
        port.keys.select(
            0.0,
            1.0,
            rustel_core::midi_in::now_nanos(),
            Some((0, 1)),
            &mut placed,
        );
        assert_eq!(placed.len(), 32, "the outgoing score placed its backlog");
        // Played during the evaluate: no query has seen it.
        port.observe_note_on(rustel_core::midi_in::now_nanos(), 1, 67, 90);

        let stopped = std::sync::atomic::AtomicBool::new(false);
        session
            .reload_at_cancellable("sd:4", true, 0.5, &stopped)
            .expect("live mini replacement");
        assert_eq!(
            session.generation(),
            generation + 1,
            "the mini score took over"
        );

        let mut after = Vec::new();
        port.keys.select(
            0.0,
            1.0,
            rustel_core::midi_in::now_nanos(),
            Some((1, 2)),
            &mut after,
        );
        assert_eq!(
            after
                .iter()
                .map(|hit| (hit.note, hit.velocity))
                .collect::<Vec<_>>(),
            [(67, 90)],
            "exactly the unplaced press survives the mini install"
        );
    }

    /// The incoming score claims again a press pinned at or past a reload's
    /// takeover, where the device swap drops the outgoing audio. A pin before
    /// the takeover, even after the edit instant, is not claimed twice.
    #[test]
    fn a_press_pinned_past_a_reloads_takeover_is_heard_by_the_new_score_once() {
        let mut session = Session::with_config(SessionConfig {
            cps: 1.0,
            horizon: 1.0,
            ..Default::default()
        })
        .expect("session");
        session.set_schedule_lead(0.0);
        session.set_continuity_margin(0.2);
        session
            .evaluate(r#"const kb = await midikeys('keyboard'); kb(0.05).s('piano')"#)
            .expect("first score");
        session.transport().start();
        let port = session
            .midi_input_bus()
            .find("keyboard")
            .expect("keyboard port");

        // Three presses before the save. The first window pins one at its
        // begin, cycle 0. The requery that the first press arms pins one at
        // that requery's takeover, cycle 1/5: after the save's edit instant
        // and before the save's takeover. The refill pins one at its begin,
        // cycle 6/5: the frontier, which is the horizon and the continuity
        // margin ahead.
        port.observe_note_on(rustel_core::midi_in::now_nanos(), 1, 60, 100);
        session.schedule_at(0.0).expect("first window");
        port.observe_note_on(rustel_core::midi_in::now_nanos(), 1, 64, 80);
        session
            .requery_active_at(0.0)
            .expect("the press requery")
            .expect("a running transport requeries");
        session
            .take_requery_takeover_time()
            .expect("the requery names its takeover");
        session.schedule_at(0.0).expect("the requery window");
        port.observe_note_on(rustel_core::midi_in::now_nanos(), 1, 67, 90);
        session.schedule_at(0.1).expect("the refill");
        let mut pinned = Vec::new();
        port.keys.select(
            0.0,
            2.0,
            rustel_core::midi_in::now_nanos(),
            None,
            &mut pinned,
        );
        assert_eq!(
            pinned
                .iter()
                .map(|hit| (hit.note, hit.num, hit.den))
                .collect::<Vec<_>>(),
            [(60, 0, 1), (64, 1, 5), (67, 6, 5)],
            "the outgoing score pinned all three presses"
        );

        let stopped = std::sync::atomic::AtomicBool::new(false);
        let replacement = session
            .reload_at_cancellable(
                r#"const kb = await midikeys('keyboard'); kb(0.05).s('sd:4')"#,
                false,
                0.1,
                &stopped,
            )
            .expect("live replacement");
        let takeover = session
            .take_requery_takeover_time()
            .expect("a continuous replacement names its takeover");
        assert!(
            0.2 < takeover && takeover < 1.2,
            "the requery's pin lies between the edit instant and the \
             takeover, the refill's past it, with the takeover at {takeover}"
        );

        let mut heard = Vec::new();
        for turn in 0..=10 {
            heard.extend(
                session
                    .schedule_at(0.1 + f64::from(turn) * 0.1)
                    .expect("replacement window")
                    .into_iter()
                    .filter(|event| event.generation == replacement),
            );
        }
        let played = |note: &str| {
            heard
                .iter()
                .filter(|event| event.value_show.starts_with(note))
                .count()
        };
        assert_eq!(
            played("note:67 "),
            1,
            "the press whose pin the takeover drops is heard once: {:?}",
            heard
                .iter()
                .map(|event| (&event.value_show, event.target_time))
                .collect::<Vec<_>>()
        );
        let claimed = heard
            .iter()
            .find(|event| event.value_show.starts_with("note:67 "))
            .expect("the claimed press");
        assert!(
            claimed.target_time >= 0.1,
            "the claimed press lands no earlier than the edit instant, where \
             the incoming score's first span begins: {}",
            claimed.target_time
        );
        assert_eq!(
            played("note:60 "),
            0,
            "the press that sounded before the takeover is not played again"
        );
        // Split at the edit instant instead, this pin would be forgotten and
        // claimed again while the outgoing score still plays it before the
        // takeover: the same press twice.
        assert_eq!(
            played("note:64 "),
            0,
            "the press pinned between the edit instant and the takeover sounds \
             under the outgoing score only"
        );
    }

    /// The device drops the outgoing audio from the takeover frame. A key pinned
    /// in the last frame before it has its onset there, so the new score claims
    /// the press again. A pin one whole frame before stays with the outgoing score.
    #[cfg(feature = "device-audio")]
    #[test]
    fn a_press_pinned_in_the_last_frame_before_a_takeover_is_claimed_again() {
        const RATE: i64 = 48_000;
        // A requery or a reload at 0.5 s takes over 0.25 s later, on this
        // frame. At one cycle a second a frame is 1/48000 cycle.
        const TAKEOVER_FRAME: i64 = 36_000;
        // 0.3 frame before the takeover frame, and one whole frame before.
        let on_the_takeover_frame = (TAKEOVER_FRAME * 10 - 3, RATE * 10);
        let before_the_takeover_frame = (TAKEOVER_FRAME - 1, RATE);
        let pinned_session = || {
            let mut session = Session::with_config(SessionConfig {
                cps: 1.0,
                ..Default::default()
            })
            .expect("session");
            session.set_schedule_lead(0.0);
            session.set_continuity_margin(0.25);
            session
                .evaluate(r#"const kb = await midikeys('keyboard'); kb(0.05).s('tri')"#)
                .expect("score");
            session.restart_transport_at(0.0);
            // A live producer has scheduled at this rate.
            assert!(
                session
                    .schedule_audio_live_at(
                        0.0,
                        RATE as u32,
                        Duration::ZERO,
                        LiveQueryBudgetMode::InitialPrefill,
                    )
                    .is_ok(),
                "the first live window"
            );
            let port = session
                .midi_input_bus()
                .find("keyboard")
                .expect("keyboard port");
            for (note, place) in [(60, on_the_takeover_frame), (64, before_the_takeover_frame)] {
                port.observe_note_on(rustel_core::midi_in::now_nanos(), 1, note, 100);
                let mut placed = Vec::new();
                port.keys.select(
                    0.0,
                    2.0,
                    rustel_core::midi_in::now_nanos(),
                    Some(place),
                    &mut placed,
                );
            }
            (session, port)
        };
        // What a query from cycle 4/5 finds: each press with its position.
        let claimed = |port: &rustel_core::midi_in::InputPort| {
            let mut hits = Vec::new();
            port.keys.select(
                0.0,
                2.0,
                rustel_core::midi_in::now_nanos(),
                Some((4, 5)),
                &mut hits,
            );
            hits.iter()
                .map(|hit| (hit.note, hit.num, hit.den))
                .collect::<Vec<_>>()
        };

        let (mut session, port) = pinned_session();
        session
            .requery_active_at(0.5)
            .expect("requery")
            .expect("a running transport requeries");
        assert_eq!(session.take_requery_takeover_time(), Some(0.75));
        assert_eq!(
            claimed(&port),
            [
                (64, before_the_takeover_frame.0, before_the_takeover_frame.1),
                (60, 4, 5)
            ],
            "the requery keeps the pin before the takeover frame, and claims \
             the press on it again"
        );

        let (mut session, port) = pinned_session();
        session
            .reload_at(
                r#"const kb = await midikeys('keyboard'); kb(0.05).s('sine')"#,
                false,
                0.5,
            )
            .expect("live replacement");
        assert_eq!(session.take_requery_takeover_time(), Some(0.75));
        assert_eq!(
            claimed(&port),
            [(60, 4, 5)],
            "the score taking over claims the press on the takeover frame, \
             and the pin before it stays with the outgoing score"
        );
    }

    #[test]
    fn a_definition_only_register_update_goes_silent_and_publishes_the_method() {
        for definition in [
            "register('double', (amount, pat) => pat.add(amount))",
            "const rg = register; rg('double', (amount, pat) => pat.add(amount))",
        ] {
            let mut session = Session::with_config(SessionConfig::default()).expect("session");
            let stopped = std::sync::atomic::AtomicBool::new(false);
            session
                .reload_at_cancellable("$: s('bd')", false, 0.0, &stopped)
                .expect("initial live score");

            session
                .reload_at_cancellable(definition, false, 0.1, &stopped)
                .expect("definition-only register update");
            assert!(
                session
                    .query(Fraction::ZERO, Fraction::ONE)
                    .expect("silent definition query")
                    .is_empty(),
                "a definition-only update should install silence"
            );

            session
                .reload_at_cancellable("$: pure(1).double(2)", false, 0.2, &stopped)
                .expect("registered method in a later update");
            let haps = session
                .query(Fraction::ZERO, Fraction::ONE)
                .expect("registered method query");
            assert_eq!(haps.len(), 1, "{haps:?}");
            assert_eq!(haps[0].value, rustel_core::Value::F64(3.0));
        }

        let mut session = Session::with_config(SessionConfig::default()).expect("session");
        session
            .reload_at(
                "const rg = register; rg('startFrom', (c, p) => p.filterWhen(x => x.gte(c))); pure(7).startFrom(1)",
                false,
                0.0,
            )
            .expect("aliased registration and method call");
        assert!(
            session
                .query(Fraction::ZERO, Fraction::ONE)
                .unwrap()
                .is_empty()
        );
        let haps = session.query(Fraction::ONE, Fraction::from(2)).unwrap();
        assert_eq!(haps.len(), 1);
        assert_eq!(haps[0].value, rustel_core::Value::F64(7.0));

        let function_error = "Error converting from js 'function' into type 'NativePatternWrapper'";
        assert!(
            Session::score_holds_no_pattern_message("() => 42", function_error).is_none(),
            "an unrelated function-valued score was mistaken for register setup"
        );
    }

    #[test]
    fn a_trailing_register_keeps_collected_lanes_playing() {
        for prefix in ["", "await Promise.resolve()\n"] {
            let mut session = Session::with_config(SessionConfig::default()).expect("session");
            session
                    .evaluate(&format!(
                        "{prefix}$: pure('first')\n$: pure('second')\nregister('identityTail', (pat) => pat)"
                    ))
                    .expect("score with trailing register");
            let values = session
                .query(Fraction::ZERO, Fraction::ONE)
                .expect("lane query")
                .into_iter()
                .map(|hap| hap.value)
                .collect::<Vec<_>>();
            assert_eq!(
                values,
                [
                    rustel_core::Value::Str("first".into()),
                    rustel_core::Value::Str("second".into()),
                ],
                "prefix {prefix:?}"
            );
        }
    }

    #[test]
    #[cfg(feature = "device-audio")]
    fn direct_live_reload_anchors_at_the_post_probe_device_clock() {
        let mut session = Session::new().expect("session");
        session
            .evaluate("setcps(1); note('c4')")
            .expect("initial score");
        let stopped = AtomicBool::new(false);
        let mut clocks = [0.1, 0.9].into_iter();

        session
            .reload_with_clock_cancellable("setcps(2); note('d4')", false, &stopped, || {
                clocks.next().expect("two reload clock samples")
            })
            .expect("live replacement");

        assert!(clocks.next().is_none(), "reload sampled an extra clock");
        assert!(
            (session.scheduler.cycle_at_time(0.9) - 0.9).abs() < 1e-9,
            "replacement pivoted at the pre-evaluation clock"
        );
        assert_eq!(session.take_requery_takeover_time(), Some(0.98));
    }

    /// A score installed from zero is heard from its own beginning: its
    /// takeover becomes cycle zero and it is queried from its first cycle.
    /// An ordinary reload joins the cycle already running.
    #[test]
    #[cfg(feature = "device-audio")]
    fn a_score_installed_from_zero_starts_at_its_own_beginning() {
        let landed = |from_zero: bool| {
            let mut session = Session::new().expect("session");
            session
                .evaluate("setcps(1); $: pure('old')")
                .expect("a score to replace");
            let stopped = AtomicBool::new(false);
            let mut clocks = [5.3, 5.3].into_iter();
            if from_zero {
                session.start_next_from_zero();
            }
            session
                .reload_with_clock_cancellable("setcps(1); $: pure('new')", false, &stopped, || {
                    clocks.next().expect("two reload clock samples")
                })
                .expect("live replacement");
            let takeover = session
                .take_requery_takeover_time()
                .expect("a continuous replacement names its takeover");
            (
                session.scheduler.cycle_at_time(takeover),
                session.scheduled_to_cycle(),
            )
        };

        // Joining: the takeover lands where the count already stood, a
        // little past cycle 5. The re-query cursor sits at the edit instant:
        // the replacement queries the overlap up to the takeover, with the
        // outgoing generation's onsets pre-marked. The device flips at the
        // takeover cycle.
        let (joined, joined_cursor) = landed(false);
        assert!((joined - 5.38).abs() < 1e-9, "{joined}");
        assert!((joined_cursor - 5.3).abs() < 1e-9, "{joined_cursor}");

        // From zero: the same instant IS cycle zero, and the score is
        // queried from its first cycle rather than from cycle five.
        let (zeroed, zeroed_cursor) = landed(true);
        assert!(zeroed.abs() < 1e-9, "the takeover is cycle zero: {zeroed}");
        assert!(
            zeroed_cursor.abs() < 1e-9,
            "and the query starts there, not in the past: {zeroed_cursor}"
        );
    }

    /// A from-zero reload's takeover carries a cut, so the restarted loop is
    /// heard alone. An ordinary reload's takeover carries none: its voices
    /// ring out.
    #[test]
    #[cfg(feature = "device-audio")]
    fn a_from_zero_reload_names_a_takeover_cut_an_ordinary_one_does_not() {
        let takeover = |from_zero: bool| {
            let mut session = Session::new().expect("session");
            session
                .evaluate("setcps(1); $: pure('old')")
                .expect("a score to replace");
            let stopped = AtomicBool::new(false);
            if from_zero {
                session.start_next_from_zero();
            }
            session
                .reload_with_clock_cancellable("setcps(1); $: pure('new')", false, &stopped, || 5.3)
                .expect("live replacement");
            session
                .take_requery_takeover()
                .expect("a continuous replacement names its takeover")
        };

        let (_, ordinary_cut) = takeover(false);
        assert_eq!(
            ordinary_cut,
            TakeoverCut::None,
            "an edit lets its voices ring out"
        );

        let (rewind_takeover, rewind_cut) = takeover(true);
        assert_eq!(
            rewind_cut,
            TakeoverCut::AtFlip,
            "an immediate rewind silences what sounds under it at the flip"
        );
        assert!(
            (rewind_takeover - 5.3).abs() < 1e-9,
            "the takeover is the edit instant itself: cycle zero lands at the cut, not a margin later (the hole a margin left was the restart's lag): {rewind_takeover}"
        );
    }

    /// An immediate rewind whose evaluation fails installs nothing and
    /// clears its from-zero mark, so the next ordinary save is not a rewind.
    #[test]
    #[cfg(feature = "device-audio")]
    fn a_failed_rewind_does_not_turn_the_next_save_into_a_restart() {
        let mut session = Session::new().expect("session");
        session
            .evaluate("setcps(1); $: pure('old')")
            .expect("a score to replace");
        let stopped = AtomicBool::new(false);
        session.start_next_from_zero();
        session
            .reload_with_clock_cancellable("setcps(1); $: pure('new'", false, &stopped, || 2.0)
            .expect_err("a broken score is refused");
        session
            .reload_with_clock_cancellable("setcps(1); $: pure('edit')", false, &stopped, || 2.5)
            .expect("an ordinary save installs");
        let (_, cut) = session
            .take_requery_takeover()
            .expect("the edit names its takeover");
        assert_eq!(
            cut,
            TakeoverCut::None,
            "the failed rewind's from-zero mark must not leak into the edit"
        );
    }

    /// A quantised scene launch that does NOT rewind rings out like an edit,
    /// so its takeover keeps the margin floor: an evaluation that ran past
    /// the line publishes a little late instead of into rendered frames,
    /// where the producer refuses it and rolls the launch back.
    #[test]
    #[cfg(feature = "device-audio")]
    fn a_plain_quantised_launch_keeps_the_margin_floor() {
        for install_clock in [3.05, 2.99] {
            let mut session = Session::new().expect("session");
            session
                .evaluate("setcps(1); $: pure('old')")
                .expect("a score to replace");
            session.set_continuity_margin(0.02);
            let stopped = AtomicBool::new(false);
            session.set_next_takeover_time(3.0);
            session
                .reload_with_clock_cancellable("setcps(1); $: pure('new')", false, &stopped, || {
                    install_clock
                })
                .expect("launched replacement");
            let (takeover, cut) = session
                .take_requery_takeover()
                .expect("the launch names its takeover");
            assert_eq!(cut, TakeoverCut::None, "a plain launch rings out");
            assert!(
                takeover >= install_clock + 0.02 - 1e-9,
                "installed at {install_clock}, the takeover keeps the floor: {takeover}"
            );
        }
    }

    /// A slider, key or clock-steer requery that lands between a quantised
    /// rewind's install and its publication re-queries the same graph. It
    /// must publish at the rewind's line with the rewind's cut: taking only
    /// the requery's own `now + margin` time kept the AtTakeover cut on it,
    /// and the countdown was choked a head-room before its line.
    #[test]
    #[cfg(feature = "device-audio")]
    fn a_control_requery_keeps_a_pending_rewinds_line() {
        let mut session = Session::new().expect("session");
        session
            .evaluate("setcps(1); $: pure('old')")
            .expect("a score to replace");
        let stopped = AtomicBool::new(false);
        session.set_next_takeover_time(3.0);
        session.start_next_from_zero();
        session
            .reload_with_clock_cancellable("setcps(1); $: pure('new')", false, &stopped, || 2.9)
            .expect("launched replacement");
        assert!(
            session
                .requery_active_at(2.92)
                .expect("a knob moves during the countdown")
                .is_some(),
            "the requery bumps the generation"
        );
        let (takeover, cut) = session
            .take_requery_takeover()
            .expect("the rewind's takeover survives the requery");
        assert_eq!(cut, TakeoverCut::AtTakeover, "still the rewind's cut");
        assert!(
            (takeover - 3.0).abs() < 1e-9,
            "published at the line, not at the requery's own time: {takeover}"
        );

        // Once the rewind has published, a requery is an ordinary one: its
        // own time and no cut at all.
        session.requery_active_at(4.0).expect("a later knob move");
        let (_, cut) = session
            .take_requery_takeover()
            .expect("the requery names its takeover");
        assert_eq!(cut, TakeoverCut::None, "an ordinary requery rings out");
    }

    /// A rewind whose commit fails after its cut would have been decided
    /// installs nothing and must leave no cut behind: latched before the
    /// fallible install, a stale AtTakeover rode the next slider requery and
    /// choked every sounding voice on a knob move.
    #[test]
    #[cfg(feature = "device-audio")]
    fn a_rewind_whose_commit_fails_leaves_no_cut_for_the_next_requery() {
        let mut session = Session::new().expect("session");
        session
            .evaluate("setcps(1); $: pure('old')")
            .expect("a score to replace");
        session.set_next_takeover_time(3.0);
        session.start_next_from_zero();
        // A missing callback cell makes wrapper installation fail regardless
        // of the engine's allocator or garbage-collection behavior. Enter the
        // commit directly so the failure occurs after the cut is decided.
        let pattern = rustel_core::pure(rustel_core::Value::F64(1.0)).fmap_js(usize::MAX);
        let failed = session.commit_evaluated_score(
            EvaluatedScore::MiniFallback(pattern),
            "",
            2.9,
            false,
            None,
            Duration::ZERO,
        );
        let error = failed.expect_err("the install is refused");
        assert!(
            error.to_string().contains("no cell was provided"),
            "{error}"
        );
        session
            .requery_active_at(3.5)
            .expect("a knob moves")
            .expect("the old score remains available to requery");
        let (_, cut) = session
            .take_requery_takeover()
            .expect("the requery names its takeover");
        assert_eq!(
            cut,
            TakeoverCut::None,
            "a failed rewind's cut must not ride the next requery"
        );
    }

    /// A quantised rewinding launch, one that waits for a cycle line, cuts at
    /// the line and not at the flip: the countdown plays until the boundary.
    /// The named takeover publishes the `AtTakeover` shape.
    #[test]
    #[cfg(feature = "device-audio")]
    fn a_quantised_launch_names_a_cut_at_the_line() {
        let mut session = Session::new().expect("session");
        session
            .evaluate("setcps(1); $: pure('old')")
            .expect("a score to replace");
        let stopped = AtomicBool::new(false);
        session.set_next_takeover_time(3.0);
        session.start_next_from_zero();
        session
            .reload_with_clock_cancellable("setcps(1); $: pure('new')", false, &stopped, || 2.9)
            .expect("launched replacement");
        let (takeover, cut) = session
            .take_requery_takeover()
            .expect("the launch names its takeover");
        assert_eq!(
            cut,
            TakeoverCut::AtTakeover,
            "the countdown plays to the line"
        );
        assert!(
            (takeover - 3.0).abs() < 1e-9,
            "the line is the takeover: {takeover}"
        );
    }

    /// The cut is consumed with the takeover it belongs to.
    ///
    /// A second, ordinary reload must not inherit the previous rewind's
    /// cut: the flag is taken once, alongside its own takeover, and a
    /// reload with no takeover behind it (a stopped one) clears both.
    #[test]
    #[cfg(feature = "device-audio")]
    fn a_takeover_cut_is_consumed_alongside_its_own_reload() {
        let mut session = Session::new().expect("session");
        session
            .evaluate("setcps(1); $: pure('a')")
            .expect("first score");
        let stopped = AtomicBool::new(false);
        session.start_next_from_zero();
        session
            .reload_with_clock_cancellable("setcps(1); $: pure('b')", false, &stopped, || 1.0)
            .expect("rewinding replacement");
        let (first_takeover, first_cut) = session
            .take_requery_takeover()
            .expect("the rewind names its takeover");
        assert_eq!(first_cut, TakeoverCut::AtFlip, "the rewind's flip cuts");
        assert!(
            (first_takeover - 1.0).abs() < 1e-9,
            "the takeover is the edit instant, cycle zero with it: {first_takeover}"
        );

        // The next reload, an ordinary one, must publish NO cut.
        session
            .reload_with_clock_cancellable("setcps(1); $: pure('c')", false, &stopped, || 2.0)
            .expect("ordinary replacement");
        let (_, second_cut) = session
            .take_requery_takeover()
            .expect("the ordinary replacement names its takeover");
        assert_eq!(
            second_cut,
            TakeoverCut::None,
            "the rewind's cut did not leak to the next reload"
        );

        // And with nothing pending, taking again is clean: no stale cut.
        assert_eq!(session.take_requery_takeover(), None);
    }

    /// A typo is not the producer turning work away. The studio header
    /// paints DSP/sched red for a capacity refusal, and that flash is
    /// wrong after a score the engine will not even parse.
    #[test]
    #[cfg(feature = "device-audio")]
    fn a_syntax_error_on_live_reload_is_not_a_producer_capacity_refusal() {
        let mut session = Session::new().expect("session");
        session.evaluate("note('c4')").expect("audible score");
        let generation = session.generation();
        let stopped = AtomicBool::new(false);
        let error = session
            .reload_with_clock_cancellable("note(", false, &stopped, || 0.0)
            .expect_err("invalid syntax");
        assert_eq!(error.kind(), "evaluation");
        assert_eq!(session.generation(), generation);
        let turn = session.take_pending_producer_turn();
        assert_eq!(turn.atomic_refusal_count, 0);
        assert_eq!(turn.committed_refusal_count, 0);
    }

    /// The live reload path does not allow the mini fallback, so it reaches a
    /// different arm than `evaluate` does. Commenting the only line out was
    /// refused there and the previous part kept playing, which is what a live
    /// coder actually hits when they mute by commenting.
    #[test]
    fn a_live_reload_that_names_no_pattern_goes_silent_rather_than_being_refused() {
        let mut session = Session::with_config(SessionConfig::default()).expect("session");
        let stopped = std::sync::atomic::AtomicBool::new(false);
        session
            .reload_at_cancellable("$: s('bd*4')", false, 0.0, &stopped)
            .expect("initial reload");
        assert!(
            !session
                .query(Fraction::new(0, 1), Fraction::new(1, 1))
                .expect("query")
                .is_empty(),
            "the set never started"
        );

        session
            .reload_at_cancellable("// $: s('bd*4')", false, 0.0, &stopped)
            .expect("a commented-out live reload is accepted, not refused");
        assert!(
            session
                .query(Fraction::new(0, 1), Fraction::new(1, 1))
                .expect("query")
                .is_empty(),
            "the muted reload kept playing"
        );

        session
            .reload_at_cancellable("$: s('bd*4')", false, 0.0, &stopped)
            .expect("uncommenting resumes");
        assert!(
            !session
                .query(Fraction::new(0, 1), Fraction::new(1, 1))
                .expect("query")
                .is_empty(),
            "uncommenting did not resume"
        );
    }

    /// Commenting a part out is how a live coder mutes it. Upstream goes quiet
    /// and resumes when the comment comes off; refusing the save instead kept
    /// the old part playing, so nothing could be muted that way.
    #[test]
    fn a_score_that_names_no_pattern_installs_silence_instead_of_being_refused() {
        let mut session = Session::with_config(SessionConfig::default()).expect("session");
        session.evaluate("s('bd*16')").expect("initial evaluate");

        // Everything commented out: evaluates cleanly, names no pattern.
        session
            .evaluate("setcps(0.5)\n// $: s('bd*16')")
            .expect("a commented-out score is accepted");
        assert!(
            session
                .query(Fraction::new(0, 1), Fraction::new(1, 1))
                .expect("query")
                .is_empty(),
            "a muted score still produced events"
        );

        // And it comes back.
        session
            .evaluate("s('bd*16')")
            .expect("uncommenting resumes");
        assert!(
            !session
                .query(Fraction::new(0, 1), Fraction::new(1, 1))
                .expect("query")
                .is_empty(),
            "uncommenting did not resume"
        );
    }

    #[cfg(feature = "device-audio")]
    #[test]
    fn negative_cycle_timelines_remain_valid_without_an_explicit_start_floor() {
        for previously_started in [false, true] {
            let mut session = Session::new().expect("session");
            session.evaluate(r#"s("bd*16")"#).expect("score");
            session.set_continuity_margin(0.02);
            if previously_started {
                session.restart_transport_at(10.0);
                // An external clock takes ownership of the cycle domain.
                session.retime(9.5, 0.5, -0.25);
            } else {
                // Generic scheduling did not promise a cycle-zero start.
                session.scheduler.rebase_anchor(9.5, -0.25);
            }
            session.requery_active_at(9.5).expect("control requery");
            let events = session
                .schedule_through(9.5, 10.0)
                .expect("negative cycle schedule");
            let first = events.first().expect("negative cycle has music");
            assert_eq!(first.whole_begin, "-3/16");
            assert_eq!(first.target_time, 9.625);

            // Inspecting negative pattern arcs is always legitimate, even
            // when an explicitly restarted playback starts from cycle zero.
            session.restart_transport_at(10.0);
            assert_eq!(
                session
                    .query(Fraction::int(-1), Fraction::ZERO)
                    .expect("negative query")
                    .len(),
                16,
            );
            session
                .requery_active_at(9.7)
                .expect("restarted control requery");
            let restarted = session
                .schedule_through(9.7, 10.2)
                .expect("restarted schedule");
            let first = restarted.first().expect("restarted downbeat");
            assert_eq!(first.whole_begin, "0/1");
            assert_eq!(first.target_time, 10.0);
        }
    }

    #[cfg(feature = "device-audio")]
    #[test]
    fn preroll_requeries_never_schedule_the_previous_cycle() {
        const RATE: u32 = 48_000;
        const START: f64 = 10.0;
        for operation in ["control", "recycle", "reload", "invalid_retime"] {
            for prefilled in [false, true] {
                let mut session = Session::with_config(SessionConfig {
                    horizon: 1.0,
                    sample_rate: RATE,
                    ..SessionConfig::default()
                })
                .expect("session");
                session.evaluate(r#"s("bd*16")"#).expect("score");
                session.restart_transport_at(START);
                session.set_schedule_lead(0.35);
                session.set_continuity_margin(0.02);
                if prefilled {
                    let Ok(batch) = session.schedule_audio_live_at(
                        9.5,
                        RATE,
                        Duration::ZERO,
                        LiveQueryBudgetMode::InitialPrefill,
                    ) else {
                        panic!("initial prefill failed");
                    };
                    assert!(!batch.events.is_empty(), "initial downbeat was queued");
                    assert!(
                        batch
                            .events
                            .iter()
                            .all(|event| event.target_frame >= 480_000)
                    );
                }
                let now = if operation == "recycle" { 9.5 } else { 9.7 };
                match operation {
                    "control" => {
                        session.requery_active_at(now).expect("control requery");
                    }
                    "recycle" => {
                        session
                            .requery_after_output_recycle_at(now)
                            .expect("recycle requery");
                    }
                    "reload" => {
                        session
                            .reload_at(r#"s("bd*16").gain(0.5)"#, false, now)
                            .expect("reload");
                    }
                    "invalid_retime" => {
                        // A rejected external-clock update must not release
                        // the current transport's promised beginning.
                        session.retime(f64::NAN, 0.5, -0.25);
                        session.requery_active_at(now).expect("control requery");
                    }
                    _ => unreachable!(),
                }
                let Ok(batch) = session.schedule_audio_live_at(
                    now,
                    RATE,
                    Duration::ZERO,
                    LiveQueryBudgetMode::ReplacementPrefill,
                ) else {
                    panic!("replacement prefill failed");
                };
                let targets: Vec<_> = batch
                    .events
                    .iter()
                    .map(|event| event.target_frame)
                    .collect();
                assert_eq!(
                    targets.first().copied(),
                    Some(480_000),
                    "{operation}, prefilled={prefilled}: previous-cycle events precede cycle zero: {targets:?}"
                );
                assert!(targets.windows(2).all(|pair| pair[1] - pair[0] == 6_000));
            }
        }
    }

    /// The audio thread drops the queued events of the old generation from the
    /// takeover frame. A live reload re-queries from the continuity margin (the
    /// device's consumption frontier), not from the start lead.
    #[cfg(feature = "device-audio")]
    #[test]
    fn live_reload_requeries_from_the_continuity_margin_not_the_start_lead() {
        let mut session = Session::with_config(SessionConfig::default()).expect("session");
        session.evaluate(r#"s("bd*16")"#).expect("initial evaluate");
        session.restart_transport_at(10.0);
        session.set_schedule_lead(0.35);
        session.set_continuity_margin(0.02);
        // Prefill the horizon, as the live producer would before the save.
        let Ok(_) = session.schedule_audio_live_at(
            10.0,
            44_100,
            std::time::Duration::ZERO,
            LiveQueryBudgetMode::InitialPrefill,
        ) else {
            panic!("prefill schedule failed");
        };
        // The save lands at 11.3. Every queued event is about to be dropped
        // by the consumer, so the new generation must resume at the margin.
        session
            .reload_at(r#"s("bd*16")"#, false, 11.3)
            .expect("reload");
        let Ok(batch) = session.schedule_audio_live_at(
            11.3,
            44_100,
            std::time::Duration::ZERO,
            LiveQueryBudgetMode::ReplacementPrefill,
        ) else {
            panic!("requery schedule failed");
        };
        let first_seconds = batch
            .events
            .iter()
            .map(|event| event.target_frame as f64 / 44_100.0)
            .fold(f64::INFINITY, f64::min);
        // s("bd*16") at the default tempo onsets every 1/(16·cps) seconds;
        // the first re-scheduled onset must sit inside margin + one step of
        // the pattern, and decisively before the floored start lead.
        let step = 1.0 / (16.0 * session.cps());
        assert!(
            first_seconds <= 11.3 + 0.02 + step + 1e-9,
            "first re-scheduled onset at {first_seconds:.3}s leaves a reload hole"
        );
        assert!(
            first_seconds < 11.3 + 0.35,
            "reload cursor still sits at the floored start lead"
        );
    }

    /// Dropping from a very fast tempo to a normal one used to resume the new
    /// generation hundreds of old-tempo cycles ahead: the re-query cursor was
    /// snapshotted as a cycle under the old mapping and then interpreted
    /// under the new one. Only a second evaluate could unstick that silence.
    #[cfg(feature = "device-audio")]
    #[test]
    fn a_tempo_drop_resumes_at_the_continuity_margin_not_old_cycles_later() {
        let mut session = Session::with_config(SessionConfig::default()).expect("session");
        session
            .evaluate("setcps(400)\n$: s(\"bd*4\")")
            .expect("initial evaluate");
        session.restart_transport_at(10.0);
        session.set_schedule_lead(0.35);
        session.set_continuity_margin(0.05);
        assert!((session.cps() - 400.0).abs() < 1e-9);

        session
            .reload_at("setcps(0.6)\n$: s(\"bd*4\")", false, 11.0)
            .expect("reload");
        assert!((session.cps() - 0.6).abs() < 1e-9);
        let Ok(batch) = session.schedule_audio_live_at(
            11.0,
            44_100,
            std::time::Duration::ZERO,
            LiveQueryBudgetMode::ReplacementPrefill,
        ) else {
            panic!("requery schedule failed");
        };
        let first_seconds = batch
            .events
            .iter()
            .map(|event| event.target_frame as f64 / 44_100.0)
            .fold(f64::INFINITY, f64::min);
        let step = 1.0 / (4.0 * session.cps());
        assert!(
            first_seconds <= 11.0 + 0.05 + step + 1e-6,
            "first onset after the tempo drop lands at {first_seconds:.3}s"
        );
        assert_eq!(session.take_requery_takeover_time(), Some(11.05));
    }

    /// A host whose playback latency exceeds the default 0.5 s query horizon
    /// (WSLg RDP sinks have reported >1 s) must still schedule through the
    /// continuity margin. Otherwise takeover sits in unscheduled time and the
    /// live tee records a silent second on the save.
    #[cfg(feature = "device-audio")]
    #[test]
    fn live_schedule_covers_a_continuity_margin_past_the_default_horizon() {
        let mut session = Session::with_config(SessionConfig::default()).expect("session");
        session.evaluate(r#"s("bd*16")"#).expect("initial evaluate");
        session.restart_transport_at(10.0);
        session.set_schedule_lead(0.35);
        session.set_continuity_margin(1.0);
        let batch = match session.schedule_audio_live_at(
            10.0,
            44_100,
            std::time::Duration::ZERO,
            LiveQueryBudgetMode::InitialPrefill,
        ) {
            Ok(batch) => batch,
            Err(super::LiveAudioScheduleError::Retryable(error))
            | Err(super::LiveAudioScheduleError::AwaitingSamples(error))
            | Err(
                super::LiveAudioScheduleError::CandidateCommitted(error)
                | super::LiveAudioScheduleError::CandidateRefusedScore(error),
            ) => {
                panic!("prefill schedule failed: {error}");
            }
        };
        let last_seconds = batch
            .events
            .iter()
            .map(|event| event.target_frame as f64 / 44_100.0)
            .fold(0.0_f64, f64::max);
        assert!(
            last_seconds >= 10.0 + 1.0 - 1e-6,
            "prefill only reached {last_seconds:.3}s; takeover at 11.0s would be a hole"
        );
        session
            .reload_at(r#"s("bd*16")"#, false, 10.0)
            .expect("reload");
        let Ok(batch) = session.schedule_audio_live_at(
            10.0,
            44_100,
            std::time::Duration::from_millis(25),
            LiveQueryBudgetMode::ReplacementPrefill,
        ) else {
            panic!("requery schedule failed");
        };
        let first_seconds = batch
            .events
            .iter()
            .map(|event| event.target_frame as f64 / 44_100.0)
            .fold(f64::INFINITY, f64::min);
        let step = 1.0 / (16.0 * session.cps());
        assert!(
            first_seconds <= 10.0 + 1.0 + step + 1e-9,
            "first replacement onset at {first_seconds:.3}s leaves a reload hole"
        );
    }

    /// A live reload never moves the
    /// cycle↔time mapping (you time your ctrl-enter and the change lands on
    /// beat), while transport start rebases cycle zero. The plain
    /// `replace_pattern` re-anchor jumps the phase by the un-played horizon;
    /// `install_evaluated_score` restores continuity around it.
    #[test]
    fn live_reload_keeps_the_timeline_and_restart_rebases_to_zero() {
        let mut session = Session::with_config(SessionConfig::default()).expect("session");
        session.evaluate("note('c4')").expect("initial evaluate");
        session.restart_transport_at(10.0);
        let origin_before = session.time_at_cycle(rustel_fraction::Fraction::ZERO);
        assert!((origin_before - 10.0).abs() < 1e-12);

        // Live reload mid-flight: mapping must not move at all.
        session.set_schedule_lead(0.25);
        session
            .reload_at("note('c5')", false, 11.3)
            .expect("live reload");
        let origin_after = session.time_at_cycle(rustel_fraction::Fraction::ZERO);
        assert!(
            (origin_after - origin_before).abs() < 1e-9,
            "live reload moved the timeline origin: {origin_before} -> {origin_after}"
        );
        // The re-query cursor starts at the edit instant. The replacement
        // queries the overlap `(edit, takeover)`, and the old generation's
        // emitted onsets are pre-marked so the device's copies do not double.
        let cursor_time = session.time_at_cycle(
            rustel_fraction::Fraction::from_f64(session.cycle_at_time(11.3)).expect("cursor cycle"),
        );
        assert!(
            (cursor_time - 11.3).abs() < 1e-9,
            "cursor did not land on the edit instant: {cursor_time}"
        );
        // The takeover time itself is unchanged: the device still flips
        // generations one schedule lead after the edit.
        assert_eq!(
            session.take_requery_takeover_time(),
            Some(11.3 + 0.25),
            "the edit-instant overlap must not move the takeover"
        );

        // A tempo change pivots at `now`: the cycle position there is
        // preserved while the origin moves with the new slope.
        let cycle_at_pivot = session.cycle_at_time(12.0);
        session
            .reload_at("setcps(1); note('c4')", false, 12.0)
            .expect("tempo reload");
        assert!(
            (session.cycle_at_time(12.0) - cycle_at_pivot).abs() < 1e-9,
            "tempo change moved the phase at the pivot"
        );
        assert!((session.cps() - 1.0).abs() < 1e-12);

        // Restart semantics: play begins at cycle zero again.
        session.restart_transport_at(20.0);
        assert!((session.time_at_cycle(rustel_fraction::Fraction::ZERO) - 20.0).abs() < 1e-12);
    }

    #[test]
    fn live_mini_reload_keeps_the_timeline() {
        let mut session = Session::with_config(SessionConfig::default()).expect("session");
        session.evaluate_mini("bd sd").expect("initial mini");
        session.restart_transport_at(10.0);
        let origin_before = session.time_at_cycle(rustel_fraction::Fraction::ZERO);
        session.set_schedule_lead(0.25);
        session
            .reload_at("bd hh", true, 11.3)
            .expect("mini live reload");
        let origin_after = session.time_at_cycle(rustel_fraction::Fraction::ZERO);
        assert!(
            (origin_after - origin_before).abs() < 1e-9,
            "mini live reload moved the timeline origin: {origin_before} -> {origin_after}"
        );
    }

    /// An edit lands 0.1 s before a cycle boundary, so the hap on that
    /// boundary sits inside the 0.25 s continuity margin. The replacement is
    /// queried from the edit instant and emits that hap.
    #[test]
    fn a_live_reload_keeps_a_first_hap_inside_the_continuity_margin() {
        let mut session = Session::with_config(SessionConfig::default()).expect("session");
        // cps 1: one cycle per second, boundaries every second. The muted
        // first beat is the "muted synth lane" of the hand-timed save.
        // Restarting at 0.0 puts cycle zero at time zero, so cycle N is time N.
        // Double quotes: a single-quoted string is plain text, one hap to the
        // cycle, and the edit would change the hap on the boundary rather
        // than un-mute it.
        session
            .evaluate(r#"setcps(1); n("~ 1 2 3").s("piano")"#)
            .expect("initial score");
        session.restart_transport_at(0.0);
        session.set_schedule_lead(0.25);
        session.set_continuity_margin(0.25);

        // The producer at clock 11.9 has drained its cover, past the boundary
        // at 12.0, with `schedule_through(now, now + horizon + margin)`. One
        // tick covers at most MAX_TICK_SPAN_CYCLES, so advance the cursor in
        // steps first.
        let cover = session.config.horizon + 0.25;
        for _ in 0..10 {
            if session.scheduled_to_cycle() > 12.25 {
                break;
            }
            session
                .schedule_through(11.9, 11.9 + cover)
                .expect("producer prefetch past the boundary");
        }
        assert!(
            session.scheduled_to_cycle() > 12.25,
            "test premise: prefetched past the boundary, got {}",
            session.scheduled_to_cycle()
        );

        // The edit lands at 11.9 - 0.1 s before the boundary at 12.0 - and
        // un-mutes the first beat. The takeover is one margin later, at 12.15:
        // the hap at 12.0 lands inside the takeover window.
        session
            .reload_at(r#"setcps(1); n("1 2 3 4").s("piano")"#, false, 11.9)
            .expect("the un-muting edit");
        assert_eq!(
            session.take_requery_takeover_time(),
            Some(12.15),
            "the takeover stays one margin after the edit"
        );

        // The replacement's first window covers from the edit instant: the
        // 12.0 hap is emitted (late), not silently skipped.
        let events = session
            .schedule_through(11.9, 11.9 + cover)
            .expect("replacement first window");
        let parse_label = |label: &str| -> f64 {
            let mut parts = label.splitn(2, '/');
            let n: f64 = parts.next().expect("numerator").parse().expect("numerator");
            let d: f64 = parts
                .next()
                .map_or(1.0, |d| d.parse().expect("denominator"));
            n / d
        };
        let overlap: Vec<f64> = events
            .iter()
            .map(|event| parse_label(&event.whole_begin))
            .filter(|begin| (11.9..12.15).contains(begin))
            .collect();
        assert_eq!(
            overlap,
            vec![12.0],
            "the un-muted hap ON the boundary must be emitted from the edit \
             instant: {overlap:?}"
        );
    }

    /// An edit changes the hap on the boundary. The device still plays the
    /// outgoing copy, so the replacement does not emit a second copy. The
    /// change is heard from the first onset past the takeover.
    #[test]
    fn a_live_reload_leaves_a_changed_hap_inside_the_continuity_margin_to_the_device() {
        let mut session = Session::with_config(SessionConfig::default()).expect("session");
        session
            .evaluate(r#"setcps(1); n("1 2 3 4").s("piano")"#)
            .expect("initial score");
        session.restart_transport_at(0.0);
        session.set_schedule_lead(0.25);
        session.set_continuity_margin(0.25);

        // As above: the producer at clock 11.9 has drained the horizon past
        // the boundary at 12.0 into the device ring.
        let cover = session.config.horizon + 0.25;
        for _ in 0..10 {
            if session.scheduled_to_cycle() > 12.25 {
                break;
            }
            session
                .schedule_through(11.9, 11.9 + cover)
                .expect("producer prefetch past the boundary");
        }
        assert!(
            session.scheduled_to_cycle() > 12.25,
            "test premise: prefetched past the boundary, got {}",
            session.scheduled_to_cycle()
        );

        // Every hap gains a control. The hap at 12.0 lands inside the
        // takeover window (11.9, 12.15), where the device has its old copy.
        session
            .reload_at(r#"setcps(1); n("1 2 3 4").s("piano").gain(1)"#, false, 11.9)
            .expect("the edit that changes every hap");
        let events = session
            .schedule_through(11.9, 11.9 + cover)
            .expect("replacement first window");
        let begin = |label: &str| -> f64 {
            let mut parts = label.splitn(2, '/');
            let n: f64 = parts.next().expect("numerator").parse().expect("numerator");
            let d: f64 = parts
                .next()
                .map_or(1.0, |d| d.parse().expect("denominator"));
            n / d
        };
        let begins: Vec<f64> = events
            .iter()
            .map(|event| begin(&event.whole_begin))
            .collect();
        assert_eq!(
            begins.first(),
            Some(&12.25),
            "the changed hap ON the boundary is the device's own copy to \
             play; the replacement starts past the takeover: {begins:?}"
        );
    }

    /// A save that changes the tempo sounds every onset once. The outgoing
    /// rate decides which onsets the replacement leaves to the device, for
    /// a slower score and for a faster score.
    #[test]
    fn a_live_reload_at_a_new_tempo_sounds_every_onset_once() {
        for (rate, live_rate) in [
            ("0.8", None),
            ("1.2", None),
            ("0.8", Some(48_000)),
            ("1.2", Some(48_000)),
        ] {
            let sounding = sounding_across_a_save(
                r#"setcps(1); s("bd*32")"#,
                &format!(r#"setcps({rate}); s("bd*32")"#),
                live_rate,
            );
            // Every onset from the save to cycle 12.4, on the 1/32 grid.
            for step in 381..397 {
                let begin = f64::from(step) / 32.0;
                let times: Vec<f64> = sounding
                    .iter()
                    .filter(|(_, sounded)| *sounded == begin)
                    .map(|(time, _)| *time)
                    .collect();
                assert_eq!(
                    times.len(),
                    1,
                    "at {rate} cycles a second the onset at {begin} sounded at {times:?} \
                     (live rate {live_rate:?})"
                );
            }
        }
    }

    /// A save to a tenth of the tempo resumes about one margin after the
    /// takeover. Without the exception in `replace_pattern_continued`, the
    /// replacement has nothing to play until 2.5 s after the edit.
    #[test]
    fn a_live_reload_after_a_tempo_drop_resumes_within_a_margin_of_the_takeover() {
        for live_rate in [None, Some(48_000)] {
            let sounding = sounding_across_a_save(
                r#"setcps(1); s("bd*32")"#,
                r#"setcps(0.1); s("bd*32")"#,
                live_rate,
            );
            // One step of the pattern at the new tempo.
            let step = 1.0 / (32.0 * 0.1);
            // The outgoing copies all sound before the takeover at 12.15.
            let resumed = sounding
                .iter()
                .map(|(time, _)| *time)
                .find(|time| *time >= 12.15);
            assert!(
                resumed.is_some_and(|time| time <= 12.15 + 0.25 + step),
                "the replacement resumed at {resumed:?} (live rate {live_rate:?})"
            );
        }
    }

    /// A save that moves every hap 0.02 s later or earlier leaves the haps
    /// inside the continuity margin to the device. Each has an outgoing
    /// copy 0.02 s away, so only the outgoing copies sound there.
    #[test]
    fn a_live_reload_leaves_a_moved_hap_inside_the_continuity_margin_to_the_device() {
        for (moved, live_rate) in [
            ("late", None),
            ("early", None),
            ("late", Some(48_000)),
            ("early", Some(48_000)),
        ] {
            let old = r#"setcps(1); s("bd*8")"#;
            let sounding = sounding_across_a_save(old, &format!("{old}.{moved}(0.02)"), live_rate);
            let before_takeover: Vec<f64> = sounding
                .iter()
                .map(|(time, _)| *time)
                .filter(|time| *time < 12.15)
                .collect();
            assert_eq!(
                before_takeover,
                vec![12.0, 12.125],
                "{moved}: only the outgoing copies sound before the takeover \
                 (live rate {live_rate:?})"
            );
        }
    }

    /// A replacement that evaluates but throws at query time keeps the
    /// last-good graph and generation. `"a b".add("x")` throws when it is
    /// queried, not when it is built.
    #[test]
    fn live_reload_query_failure_restores_the_exact_last_good_callback_wrapper() {
        let mut session = Session::new().expect("session");
        session
                .evaluate(
                    "setDefaultJoin('out'); (() => { let calls = 0; return pure('last-good').fmap(value => `${value}:${++calls}`); })()",
                )
                .expect("callback-bearing initial score");
        let generation = session.generation();
        let before = shown_note(&session);
        assert_eq!(before, "last-good:1");

        let error = session
            .reload_at(
                r#"setDefaultJoin("mix"); pure("foo").add("bar")"#,
                false,
                0.25,
            )
            .expect_err("query-throwing replacement must not install");
        assert_eq!(error.kind(), "evaluation");
        assert!(
            error.to_string().contains("last-good score kept"),
            "rejection must name the never-die contract: {error}"
        );
        assert_eq!(session.generation(), generation);
        assert_eq!(
            session
                .js
                .with_runtime_settings(rustel_core::compose::default_alignment),
            rustel_core::compose::Alignment::Out,
            "a rejected query probe published its candidate module settings"
        );
        session.js.run_gc();
        session.js.run_gc();
        assert_eq!(shown_note(&session), "last-good:2");
        let after = session
            .schedule_at(0.25)
            .expect("last-good still schedules");
        assert!(
            after
                .iter()
                .any(|event| event.value_show.contains("last-good:3")),
            "query-throwing reload silenced the sounding score: {after:?}"
        );
    }

    #[test]
    fn accepted_live_reload_publishes_candidate_settings_after_its_probe() {
        let mut session = Session::new().expect("session");
        session
            .evaluate("setDefaultJoin('out'); pure('last-good')")
            .expect("initial score");
        assert_eq!(
            session
                .js
                .with_runtime_settings(rustel_core::compose::default_alignment),
            rustel_core::compose::Alignment::Out
        );

        session
            .reload_at("setDefaultJoin('mix'); pure('candidate')", false, 0.25)
            .expect("accepted candidate");
        assert_eq!(
            session
                .js
                .with_runtime_settings(rustel_core::compose::default_alignment),
            rustel_core::compose::Alignment::Mix,
            "the accepted query probe did not publish its candidate module settings"
        );
        assert_eq!(shown_note(&session), "candidate");
    }

    #[test]
    fn rust_patterns_snapshot_ambient_settings_when_a_session_adopts_them() {
        let ambient = rustel_core::settings::RuntimeSettings::default();
        ambient.with(|| {
            rustel_core::voicings::set_default_voicings("guidetones");
            let mut session = Session::new().expect("session");
            session
                .set_pattern(default_voicing_size_pattern())
                .expect("install Rust pattern");

            rustel_core::voicings::set_default_voicings("lefthand");
            let haps = session
                .query(Fraction::ZERO, Fraction::ONE)
                .expect("query adopted pattern");
            assert!(matches!(&haps[0].value, Value::List(notes) if notes.len() == 2));
        });
    }

    #[test]
    fn adopted_rust_patterns_export_metadata_with_the_session_settings() {
        let ambient = rustel_core::settings::RuntimeSettings::default();
        ambient.with(|| {
            rustel_core::voicings::set_default_voicings("guidetones");
            let function = rustel_core::value::FunctionRef::native(
                Some("defaultVoicing"),
                Arc::new(|pattern| pattern.fmap(|_| default_voicing_value())),
                Arc::new(|pattern| pattern.fmap(|_| default_voicing_value())),
            );
            let carrier = rustel_core::pure(Value::Function(function));
            let mut session = Session::new().expect("session");
            session.set_pattern(carrier).expect("install Rust pattern");

            rustel_core::voicings::set_default_voicings("lefthand");
            let haps = session
                .query(Fraction::ZERO, Fraction::ONE)
                .expect("query adopted carrier");
            let Value::Function(function) = &haps[0].value else {
                panic!("carrier did not emit its Function");
            };
            let output = function.apply(rustel_core::pure(Value::Null));
            let output_haps = output.query_arc(Fraction::ZERO, Fraction::ONE);
            assert!(
                matches!(&output_haps[0].value, Value::List(notes) if notes.len() == 2),
                "executable metadata escaped into the ambient runtime"
            );
        });
    }

    #[test]
    fn rust_hosts_can_configure_each_session_explicitly() {
        let mut first = Session::new().expect("first session");
        let mut second = Session::new().expect("second session");
        first.set_default_voicings(Some("guidetones"));
        second.set_default_voicings(Some("lefthand"));
        first
            .set_pattern(default_voicing_size_pattern())
            .expect("first Rust pattern");
        second
            .set_pattern(default_voicing_size_pattern())
            .expect("second Rust pattern");

        let first_haps = first
            .query(Fraction::ZERO, Fraction::ONE)
            .expect("query first Rust pattern");
        let second_haps = second
            .query(Fraction::ZERO, Fraction::ONE)
            .expect("query second Rust pattern");
        assert!(matches!(&first_haps[0].value, Value::List(notes) if notes.len() == 2));
        assert!(matches!(&second_haps[0].value, Value::List(notes) if notes.len() == 4));
    }

    #[test]
    fn rejected_javascript_transition_keeps_the_native_graphs_settings() {
        let ambient = rustel_core::settings::RuntimeSettings::default();
        ambient.with(|| {
            rustel_core::rng::use_rng(rustel_core::rng::RngMode::Precise);
            rustel_core::compose::set_default_alignment(rustel_core::compose::Alignment::Out);
            rustel_core::voicings::set_default_voicings("guidetones");
            let mut session = Session::new().expect("session");
            session
                .set_pattern(default_voicing_size_pattern())
                .expect("install native graph");

            rustel_core::rng::use_rng(rustel_core::rng::RngMode::Legacy);
            rustel_core::compose::set_default_alignment(rustel_core::compose::Alignment::Mix);
            rustel_core::voicings::set_default_voicings("lefthand");
            session
                .evaluate("setDefaultJoin('mix'); throw new Error('reject transition')")
                .expect_err("reject score");

            assert_eq!(
                session_core_settings(&session),
                (
                    rustel_core::rng::RngMode::Precise,
                    rustel_core::compose::Alignment::Out,
                    2,
                )
            );
        });
    }

    #[test]
    fn each_explicit_setter_preserves_the_other_adopted_fields() {
        let ambient = rustel_core::settings::RuntimeSettings::default();
        ambient.with(|| {
            rustel_core::rng::use_rng(rustel_core::rng::RngMode::Precise);
            rustel_core::compose::set_default_alignment(rustel_core::compose::Alignment::Out);
            rustel_core::voicings::set_default_voicings("guidetones");
            let mut rng_session = Session::new().expect("rng session");
            let mut join_session = Session::new().expect("join session");
            let mut voicing_session = Session::new().expect("voicing session");
            for session in [&mut rng_session, &mut join_session, &mut voicing_session] {
                session
                    .set_pattern(default_voicing_size_pattern())
                    .expect("adopt native settings");
            }

            rustel_core::rng::use_rng(rustel_core::rng::RngMode::Legacy);
            rustel_core::compose::set_default_alignment(rustel_core::compose::Alignment::Mix);
            rustel_core::voicings::set_default_voicings("lefthand");

            rng_session.set_rng_mode(rustel_core::rng::RngMode::Legacy);
            join_session.set_default_join(rustel_core::compose::Alignment::Restart);
            voicing_session.set_default_voicings(Some("lefthand"));

            assert_eq!(
                session_core_settings(&rng_session),
                (
                    rustel_core::rng::RngMode::Legacy,
                    rustel_core::compose::Alignment::Out,
                    2,
                )
            );
            assert_eq!(
                session_core_settings(&join_session),
                (
                    rustel_core::rng::RngMode::Precise,
                    rustel_core::compose::Alignment::Restart,
                    2,
                )
            );
            assert_eq!(
                session_core_settings(&voicing_session),
                (
                    rustel_core::rng::RngMode::Precise,
                    rustel_core::compose::Alignment::Out,
                    4,
                )
            );
        });
    }

    #[test]
    fn mini_adoption_is_transactional_across_parse_and_live_probe() {
        let ambient = rustel_core::settings::RuntimeSettings::default();
        ambient.with(|| {
            rustel_core::rng::use_rng(rustel_core::rng::RngMode::Precise);
            rustel_core::compose::set_default_alignment(rustel_core::compose::Alignment::Out);
            rustel_core::voicings::set_default_voicings("guidetones");
            let mut session = Session::new().expect("session");
            session.evaluate_mini("bd").expect("initial mini graph");

            rustel_core::rng::use_rng(rustel_core::rng::RngMode::Legacy);
            rustel_core::compose::set_default_alignment(rustel_core::compose::Alignment::Mix);
            rustel_core::voicings::set_default_voicings("lefthand");
            session
                .evaluate_live_score_cancellable(
                    "[",
                    true,
                    Duration::ZERO,
                    &NEVER_CANCELLED,
                    || 0.0,
                )
                .expect_err("reject malformed mini graph");
            assert_eq!(
                session_core_settings(&session),
                (
                    rustel_core::rng::RngMode::Precise,
                    rustel_core::compose::Alignment::Out,
                    2,
                )
            );

            session
                .evaluate_live_score_cancellable(
                    "sd",
                    true,
                    Duration::ZERO,
                    &NEVER_CANCELLED,
                    || 0.0,
                )
                .expect("probe and install live mini graph");
            assert_eq!(
                session_core_settings(&session),
                (
                    rustel_core::rng::RngMode::Legacy,
                    rustel_core::compose::Alignment::Mix,
                    4,
                )
            );
        });
    }

    #[test]
    fn mini_fallback_constructs_with_the_adopted_session_settings() {
        let ambient = rustel_core::settings::RuntimeSettings::default();
        ambient.with(|| {
            rustel_core::rng::use_rng(rustel_core::rng::RngMode::Precise);
            rustel_core::compose::set_default_alignment(rustel_core::compose::Alignment::In);
            rustel_core::voicings::set_default_voicings("guidetones");
            let mut session = Session::new().expect("session");
            session
                .set_pattern(default_voicing_size_pattern())
                .expect("install native graph");

            rustel_core::rng::use_rng(rustel_core::rng::RngMode::Legacy);
            rustel_core::compose::set_default_alignment(rustel_core::compose::Alignment::Mix);
            rustel_core::voicings::set_default_voicings("lefthand");
            session
                .evaluate("bd sd")
                .expect("mini compatibility fallback");

            assert_eq!(session.last_evaluate_source(), EvaluateSource::MiniRust);
            assert_eq!(
                session_core_settings(&session),
                (
                    rustel_core::rng::RngMode::Precise,
                    rustel_core::compose::Alignment::In,
                    2,
                )
            );
        });
    }
}
