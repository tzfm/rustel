use super::*;

impl Session {
    /// Lower the QuickJS heap ceiling for this session.
    ///
    /// Exposed so callers can exercise the session-level heap bound at a small
    /// size. Monotonic and refused below live usage, as at the host layer.
    pub fn set_js_memory_limit(&self, bytes: usize) -> Result<(), RuntimeError> {
        self.js
            .set_memory_limit(bytes)
            .map_err(RuntimeError::Message)
    }

    /// Bytes currently live on the QuickJS heap.
    ///
    /// A loaded session already holds tens of megabytes for the control
    /// surface, and that figure moves as patterns are evaluated - so a test
    /// wanting a ceiling "just above current usage" has to ask rather than
    /// guess.
    pub fn js_heap_live(&self) -> usize {
        self.js.heap_live()
    }

    /// Run QuickJS garbage collection.
    ///
    /// Two passes, matching the rest of this crate's tests: the first
    /// collects, the second reclaims cycles the first uncovered. A host
    /// uses this to tell live (rooted) heap from garbage a collection
    /// would free. The studio does not call it; QuickJS collects on its
    /// own schedule.
    pub fn run_js_gc(&self) {
        self.js.run_gc();
        self.js.run_gc();
    }

    /// Whether the active graph needs the QuickJS callback host to be queried.
    ///
    /// The scheduler queries on its own clock, so `play`/`render` have to know
    /// whether to install the host scope. Exposed because it is also the
    /// executable form of the purity claim: a source that reaches no JavaScript
    /// must schedule with nothing installed.
    pub fn active_needs_host(&self) -> bool {
        self.js.active_needs_host()
    }

    /// Query the active graph over `[begin, end)` in cycle time.
    pub fn query(&self, begin: Fraction, end: Fraction) -> Result<Vec<Hap>, RuntimeError> {
        self.query_with_js_budget(begin, end, QUERY_JS_CPU_BUDGET)
    }

    /// How far the scheduler has queried the active graph, in cycles.
    pub fn scheduled_to_cycle(&self) -> f64 {
        self.scheduler.scheduled_to_cycle()
    }

    /// What is coming: the onsets of the active graph in `[from_cycle,
    /// to_cycle)`, as traces a painter can place ahead of the audio. Read
    /// with the non-advancing query and without `_cps`, so nothing the
    /// scheduler owns moves and no MIDI key is placed. Sorted by onset, at
    /// most one UI batch of them; their onset ids carry
    /// [`PREVIEW_ONSET_ID_FLAG`]. Empty when nothing is playing.
    pub fn preview_traces(
        &self,
        from_cycle: f64,
        to_cycle: f64,
        generation: u64,
    ) -> Result<Vec<rustel_scheduler::ScheduleTraceEvent>, RuntimeError> {
        if !self.js.has_active_pattern()
            || !from_cycle.is_finite()
            || !to_cycle.is_finite()
            || to_cycle <= from_cycle
        {
            return Ok(Vec::new());
        }
        let (Some(begin), Some(end)) =
            (Fraction::from_f64(from_cycle), Fraction::from_f64(to_cycle))
        else {
            return Ok(Vec::new());
        };
        let haps = self.query_with_js_budget(begin, end, PREVIEW_QUERY_JS_BUDGET)?;
        let mut onsets = haps
            .into_iter()
            .filter(|hap| hap.has_onset())
            .filter_map(|hap| {
                let whole = hap.whole?;
                (whole.begin >= begin).then(|| (whole.begin, hap.value.show(), hap))
            })
            .collect::<Vec<_>>();
        onsets.sort_by(|left, right| left.0.cmp(&right.0).then_with(|| left.1.cmp(&right.1)));
        onsets.truncate(crate::ui_events::MAX_UI_EVENTS_PER_BATCH);
        Ok(onsets
            .into_iter()
            .enumerate()
            .filter_map(|(index, (begin, _, hap))| {
                rustel_scheduler::ScheduleTraceEvent::from_hap(
                    &hap,
                    generation,
                    PREVIEW_ONSET_ID_FLAG | index as u64,
                    self.time_at_cycle(begin),
                )
            })
            .collect())
    }

    /// Query with an explicit synchronous-JavaScript budget.
    ///
    /// Kept private because the product contract is the fixed ceiling above;
    /// focused tests inject a tiny duration so the real interrupt path is
    /// mutation-sensitive without making the suite wait two seconds.
    pub(super) fn query_with_js_budget(
        &self,
        begin: Fraction,
        end: Fraction,
        budget: Duration,
    ) -> Result<Vec<Hap>, RuntimeError> {
        // A panic in this query is a query panic of the score in play.
        self.panic_is_query.set(true);
        let haps = self.query_bounded(|cancellation| {
            self.js.query_cancellable_with_hap_budget(
                Slot::Active,
                0,
                begin,
                end,
                budget,
                self.scheduler.query_hap_budget(),
                cancellation,
            )
        });
        self.panic_is_query.set(false);
        haps
    }

    /// Runs `run`, one query of the active score, under the stop flag of the
    /// transport and in the panic boundary of a query. The call does not
    /// mark a panic in `run` as a query panic of the score in play, so a
    /// panic in a read of a candidate never parks the score in play: see
    /// [`Self::with_panic_recovery`].
    pub(super) fn query_bounded(
        &self,
        run: impl FnOnce(&AtomicBool) -> Result<Vec<Hap>, rustel_jsruntime::QueryError>,
    ) -> Result<Vec<Hap>, RuntimeError> {
        if self.panic_poisoned {
            return Err(RuntimeError::Panic(
                "native score work panicked; Session recovery is unavailable".into(),
            ));
        }
        // Cancellable: a dense query is long enough for a user to change their
        // mind about, and a JavaScript callback can otherwise prevent the
        // recursive Rust checks from ever regaining control. One caller-owned
        // flag therefore drives both core cancellation and QuickJS's interrupt
        // handler under the same outer query boundary.
        let cancellation = self.transport.stopped_flag();
        let query = || {
            #[cfg(any(test, feature = "test-support"))]
            self.panic_if_injected(SessionPanicPoint::Query);
            let result = rustel_core::with_cancellation(cancellation, || {
                run(cancellation).map_err(RuntimeError::from)
            });
            self.js.raise_caught_native_panic();
            result
        };
        // Inside a recovery boundary a panic rebuilds the Session; a query
        // made outside one, such as `rustel query`, reports it as an error.
        if recovery::panic_is_contained() {
            query()
        } else {
            recovery::catch_score_panic(query).unwrap_or_else(|message| {
                Err(RuntimeError::Panic(format!(
                    "native score work panicked ({message})"
                )))
            })
        }
    }

    pub fn query_report(
        &self,
        begin: Fraction,
        end: Fraction,
    ) -> Result<QueryReport, RuntimeError> {
        let haps = self.query(begin, end)?;
        // A stack contains a lane's throw and keeps the other lanes' haps.
        // The report still says that the score threw.
        let contained = rustel_core::take_query_contained_throw();
        Ok(QueryReport {
            begin: fraction_label(begin),
            end: fraction_label(end),
            source: self.last_source.as_deref().unwrap_or_default().to_owned(),
            haps: haps_to_json(&haps),
            // Taken after the query so the report owns the message; a
            // following query starts clean.
            query_threw: self.js.take_query_throw().or(contained),
        })
    }

    /// Microbench query throughput for the active pattern.
    pub fn bench(
        &self,
        begin: Fraction,
        end: Fraction,
        iterations: u64,
    ) -> Result<BenchMetrics, RuntimeError> {
        if iterations == 0 {
            return Err(RuntimeError::Message(
                "bench iterations must be greater than 0".into(),
            ));
        }
        // Warm once so first-eval noise is outside the timed loop.
        let _ = self.query(begin, end)?;
        let start = Instant::now();
        let mut total_haps = 0u64;
        for _ in 0..iterations {
            // Checked between iterations as well as inside each query: a bench
            // of a cheap pattern is a tight loop where no single query is long
            // enough to notice a stop.
            if self.transport.is_stopped() {
                return Err(RuntimeError::Cancelled);
            }
            total_haps += self.query(begin, end)?.len() as u64;
        }
        let elapsed = start.elapsed().as_secs_f64().max(f64::EPSILON);
        Ok(BenchMetrics {
            source: self.last_source.as_deref().unwrap_or_default().to_owned(),
            begin: fraction_label(begin),
            end: fraction_label(end),
            iterations,
            total_haps,
            haps_per_iteration: total_haps as f64 / iterations as f64,
            elapsed_secs: elapsed,
            queries_per_sec: iterations as f64 / elapsed,
            haps_per_sec: total_haps as f64 / elapsed,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::tests::*;

    fn assert_nested_shrink_refusal(session: &mut Session) {
        let over = rustel_core::MAX_STEPWISE_ENTRIES + 1;
        let error = session
            .schedule_at(0.0)
            .expect_err("MAX+1 shrink must refuse");
        let RuntimeError::ResourceLimit(message) = error else {
            panic!("shrink lost its resource type: {error:?}");
        };
        assert!(
            message.contains("shrink/grow")
                && message.contains(&over.to_string())
                && message.contains(&rustel_core::MAX_STEPWISE_ENTRIES.to_string()),
            "wrong shrink refusal: {message}"
        );
        assert!(
            matches!(
                session.scheduler.refusal(),
                Some(rustel_core::QueryLimit::StepwiseExpansion {
                    operation,
                    minimum_entries,
                    limit,
                }) if *operation == "shrink/grow"
                    && *minimum_entries == over
                    && *limit == rustel_core::MAX_STEPWISE_ENTRIES
            ),
            "wrong refusal attribution: {:?}",
            session.scheduler.refusal()
        );
    }

    fn assert_native_query_and_play(
        session: &mut Session,
        steps: Option<Fraction>,
        haps: &[&str],
        onsets: &[(&str, &str)],
    ) {
        assert!(!session.active_needs_host());
        assert_eq!(
            session.js.active_pattern().expect("active pattern").steps,
            steps
        );
        assert_eq!(
            session
                .query(Fraction::ZERO, Fraction::ONE)
                .expect("query")
                .into_iter()
                .map(|hap| hap.show())
                .collect::<Vec<_>>(),
            haps,
        );
        assert_eq!(
            session
                .play(2.0)
                .expect("schedule")
                .onsets
                .iter()
                .map(|onset| (onset.whole_begin.as_str(), onset.value_show.as_str()))
                .collect::<Vec<_>>(),
            onsets,
        );
    }

    #[cfg(test)]
    mod stack_lane_error_tests {
        //! A query error in one `$:` lane. Every `$:` score is a stack, even with
        //! one lane, and a stack contains a child's throw as strudel.cc does: the
        //! lane goes silent and the other lanes play. The offline report still says
        //! the score threw. The replacement probe is covered in
        //! `lane_throw_probe_tests`.

        use super::*;
        use crate::session::test_support::{quiet_session, warnings};

        const FAILING_LANE: &str = r#"$: note("0 4".add("x")).s("sine")"#;

        fn threw_parse_numeral(query_threw: Option<&str>) -> bool {
            query_threw.is_some_and(|message| message.contains("cannot parse as numeral"))
        }

        #[test]
        fn a_score_whose_only_lane_throws_plays_silence_and_reports_query_threw() {
            let mut session = quiet_session(FAILING_LANE);
            let report = session.play(2.0).expect("play");
            assert!(report.onsets.is_empty(), "{:?}", report.onsets);
            assert!(
                threw_parse_numeral(report.query_threw.as_deref()),
                "a silent bounce must report its throw: {:?}",
                report.query_threw
            );
            let warnings = warnings(&mut session);
            assert_eq!(warnings.len(), 1, "{warnings:?}");
        }

        #[test]
        fn a_throwing_lane_beside_a_healthy_one_plays_it_and_reports_query_threw() {
            let mut session = quiet_session(&format!("$: s(\"bd*4\")\n{FAILING_LANE}"));
            let report = session.play(2.0).expect("play");
            assert!(!report.onsets.is_empty(), "the failing lane silenced bd");
            assert!(
                report.onsets.iter().all(|onset| onset.value_show == "s:bd"),
                "{:?}",
                report.onsets
            );
            assert!(
                threw_parse_numeral(report.query_threw.as_deref()),
                "{:?}",
                report.query_threw
            );
            let warnings = warnings(&mut session);
            assert_eq!(warnings.len(), 1, "{warnings:?}");
            assert!(
                warnings[0].contains("cannot parse as numeral"),
                "{warnings:?}"
            );
        }

        #[test]
        fn a_bounce_whose_only_lane_throws_fails_its_report() {
            let directory =
                tempfile::tempdir_in(env!("CARGO_MANIFEST_DIR")).expect("render directory");
            let mut session = quiet_session(FAILING_LANE);
            let report = session
                .render(
                    1.0,
                    &directory.path().join("onsets.json"),
                    RenderFormat::OnsetJson,
                )
                .expect("render");
            assert_eq!(report.onset_count, 0);
            assert!(
                report
                    .failure()
                    .is_some_and(|failure| failure.contains("cannot parse as numeral")),
                "the exit status must not call a silent bounce a success: {report:?}"
            );
        }

        #[test]
        fn a_fail_open_filter_in_a_lane_is_not_a_throw() {
            let mut session = quiet_session(r#"$: s("bd*4").filterValues(x => log(x))"#);
            let report = session.play(2.0).expect("play");
            assert!(!report.onsets.is_empty());
            assert_eq!(report.query_threw, None);
        }

        #[test]
        fn a_query_report_keeps_the_healthy_lane_and_reports_query_threw() {
            let session = quiet_session(&format!("$: s(\"bd*4\")\n{FAILING_LANE}"));
            let report = session
                .query_report(Fraction::ZERO, Fraction::ONE)
                .expect("query");
            assert_eq!(report.haps.len(), 4, "{:?}", report.haps);
            assert!(
                threw_parse_numeral(report.query_threw.as_deref()),
                "{:?}",
                report.query_threw
            );
        }
    }

    #[test]
    fn direct_query_deadline_and_stop_are_bounded_and_same_session_recovers() {
        assert_eq!(
            QUERY_JS_CPU_BUDGET,
            Duration::from_secs(2),
            "the Session query-time JavaScript ceiling changed"
        );
        let mut session = Session::new().expect("session");
        session
            .evaluate(QUERY_SPIN_SOURCE)
            .expect("evaluate query-time runaway");
        assert!(
            session.active_needs_host(),
            "a user-authored Pattern query must take the impure host route"
        );

        let tiny_budget = Duration::from_millis(40);
        let started = Instant::now();
        let deadline = session
            .query_with_js_budget(Fraction::ZERO, Fraction::ONE, tiny_budget)
            .expect_err("runaway query callback must hit its QuickJS deadline");
        assert!(
            matches!(deadline, RuntimeError::ResourceLimit(_)),
            "query deadline lost the resource-limit channel: {deadline:?}"
        );
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "40ms query deadline returned too late: {:?}",
            started.elapsed()
        );
        assert_eq!(session.js.query_depth(), 0, "deadline leaked query stack");
        assert_eq!(
            rustel_jsruntime::bridge_frame_depth(),
            0,
            "deadline leaked bridge frame"
        );

        set_query_spin(&session, false);
        let recovered = session
            .query_with_js_budget(Fraction::ZERO, Fraction::ONE, tiny_budget)
            .expect("same-session query after deadline");
        assert_eq!(recovered.len(), 1);
        assert_eq!(recovered[0].value.show(), "recovered");

        set_query_spin(&session, true);
        let transport = session.transport();
        let stopper = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(25));
            transport.stop();
        });
        let started = Instant::now();
        let cancelled = session
            .query_with_js_budget(Fraction::ZERO, Fraction::ONE, Duration::from_secs(2))
            .expect_err("Stop must interrupt a running JavaScript query");
        stopper.join().expect("stopper thread");
        assert!(
            matches!(cancelled, RuntimeError::Cancelled),
            "query Stop lost cancellation identity: {cancelled:?}"
        );
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "query Stop waited for the two-second deadline: {:?}",
            started.elapsed()
        );

        session.transport.start();
        set_query_spin(&session, false);
        let recovered_again = session
            .query_with_js_budget(Fraction::ZERO, Fraction::ONE, tiny_budget)
            .expect("same-session query after cancellation");
        assert_eq!(recovered_again.len(), 1);
        assert_eq!(recovered_again[0].value.show(), "recovered");
    }

    #[test]
    fn play_uses_the_same_query_time_js_boundary_and_recovers() {
        let mut session = Session::new().expect("session");
        session
            .evaluate(QUERY_SPIN_SOURCE)
            .expect("evaluate query-time runaway");
        let tiny_budget = Duration::from_millis(40);
        let refused = session
            .play_with_js_budget(1.0, tiny_budget)
            .expect_err("play must bound its impure scheduler tick");
        assert!(
            matches!(refused, RuntimeError::ResourceLimit(_)),
            "play query deadline lost the resource-limit channel: {refused:?}"
        );
        assert_eq!(session.js.query_depth(), 0, "play leaked query stack");
        assert_eq!(
            rustel_jsruntime::bridge_frame_depth(),
            0,
            "play leaked bridge frame"
        );

        set_query_spin(&session, false);
        let report = session
            .play_with_js_budget(1.0, tiny_budget)
            .expect("same-session play after query refusal");
        assert!(
            report
                .onsets
                .iter()
                .any(|onset| onset.value_show == "recovered"),
            "recovered play scheduled no onset: {:?}",
            report.onsets
        );
    }

    #[test]
    fn stop_inside_a_javascript_play_tick_keeps_partial_success_semantics() {
        let mut session = Session::new().expect("session");
        session
            .evaluate(QUERY_SPIN_SOURCE)
            .expect("evaluate query-time runaway");
        let transport = session.transport();
        let stopper = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(30));
            transport.stop();
        });
        let started = Instant::now();
        let report = session
            .play_with_js_budget(1.0, Duration::from_secs(2))
            .expect("Stop during play keeps the already-produced partial report");
        stopper.join().expect("stopper thread");
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "play waited for the query deadline instead of Stop: {:?}",
            started.elapsed()
        );
        assert!(
            report.onsets.is_empty(),
            "the interrupted first tick published partial events: {:?}",
            report.onsets
        );
        assert_eq!(session.js.query_depth(), 0, "Stop leaked query stack");
    }

    #[test]
    fn stop_visible_at_an_impure_tick_preflight_keeps_partial_success_semantics() {
        let outcome = play_tick_or_stopped(Err(RuntimeError::Cancelled))
            .expect("a preflight Stop is not an evaluation failure");
        assert!(
            outcome.is_none(),
            "a preflight Stop must end play without another scheduler tick"
        );

        let resource = play_tick_or_stopped(Err(RuntimeError::ResourceLimit("deadline".into())))
            .expect_err("a real query refusal must not be converted to Stop");
        assert!(matches!(resource, RuntimeError::ResourceLimit(_)));
    }

    #[test]
    fn query_deadline_is_not_an_ordinary_throw_and_pure_ticks_open_no_js_budget() {
        let tiny_budget = Duration::from_millis(40);
        let mut throwing = Session::new().expect("throwing session");
        throwing
            .evaluate("new Pattern(() => { throw new Error('ordinary query throw'); })")
            .expect("evaluate throwing query callback");
        let haps = throwing
            .query_with_js_budget(Fraction::ZERO, Fraction::ONE, tiny_budget)
            .expect("an ordinary strudel.cc query throw becomes silence");
        assert!(haps.is_empty(), "ordinary query throw produced haps");

        let mut pure = Session::new().expect("pure session");
        pure.evaluate("pure('native')")
            .expect("evaluate pure graph");
        assert!(
            !pure.active_needs_host(),
            "pure negative control unexpectedly needs QuickJS"
        );
        let events = pure
            .schedule_through_with_js_budget(0.0, 0.0, Duration::ZERO)
            .expect("pure scheduler tick must not enter the JS deadline boundary");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].value_show, "native");
        assert_eq!(pure.js.query_depth(), 0, "pure tick opened query stack");
        assert_eq!(
            rustel_jsruntime::bridge_frame_depth(),
            0,
            "pure tick opened bridge frame"
        );
    }

    #[test]
    fn stepwise_patterns_keep_exact_query_windows_steps_and_scheduler_onsets() {
        let mut session = Session::new().expect("session");
        session
            .evaluate("sequence(0, 1).replicate(slowcat(1, 2))")
            .expect("evaluate patterned replicate");
        assert!(
            !session.active_needs_host(),
            "a native patterned factor was pushed onto the JavaScript scheduler path"
        );

        fn shows(session: &Session, begin: Fraction, end: Fraction) -> Vec<String> {
            session
                .query(begin, end)
                .expect("query patterned replicate")
                .into_iter()
                .map(|hap| hap.show())
                .collect::<Vec<_>>()
        }
        assert_eq!(
            shows(&session, Fraction::ZERO, Fraction::int(2)),
            [
                "[ 0/1 → 1/2 | 0 ]",
                "[ 1/2 → 1/1 | 1 ]",
                "[ 1/1 → 3/2 | 0 ]",
                "[ 3/2 → 2/1 | 1 ]",
            ],
            "a joined query must select the StepJoin carrier from its own begin"
        );
        assert_eq!(
            shows(&session, Fraction::ONE, Fraction::int(2)),
            [
                "[ 1/1 → 5/4 | 0 ]",
                "[ 5/4 → 3/2 | 1 ]",
                "[ 3/2 → 7/4 | 0 ]",
                "[ 7/4 → 2/1 | 1 ]",
            ],
            "a separately queried second cycle must select factor two"
        );

        // The scheduler queries incrementally, rather than joining the full
        // render span into one carrier selection. At 0.5 CPS, cycle zero's
        // factor-one halves last one second and cycle one's factor-two quarters
        // last half a second. The final event is the documented inclusive
        // drain at the four-second boundary.
        let play = session.play(4.0).expect("schedule patterned replicate");
        let onsets = play
            .onsets
            .iter()
            .map(|onset| {
                (
                    onset.whole_begin.as_str(),
                    onset.value_show.as_str(),
                    onset.target_time,
                    onset.duration_secs,
                )
            })
            .collect::<Vec<_>>();
        assert_eq!(
            onsets,
            [
                ("0/1", "0", 0.0, 1.0),
                ("1/2", "1", 1.0, 1.0),
                ("1/1", "0", 2.0, 0.5),
                ("5/4", "1", 2.5, 0.5),
                ("3/2", "0", 3.0, 0.5),
                ("7/4", "1", 3.5, 0.5),
                ("2/1", "0", 4.0, 0.5),
            ],
            "scheduler windowing changed patterned replicate onsets"
        );

        session
            .evaluate("stepcat(sequence(0, 1, 2, 3).contract(2), 9)")
            .expect("evaluate contract step consumer");
        assert_eq!(
            session.js.active_pattern().expect("active contract").steps,
            Some(Fraction::int(3)),
            "contract metadata did not reach a real step consumer"
        );
        assert_eq!(
            shows(&session, Fraction::ZERO, Fraction::ONE),
            [
                "[ 0/1 → 1/6 | 0 ]",
                "[ 1/6 → 1/3 | 1 ]",
                "[ 1/3 → 1/2 | 2 ]",
                "[ 1/2 → 2/3 | 3 ]",
                "[ 2/3 → 1/1 | 9 ]",
            ],
            "Session lost contract's divided step metadata"
        );
    }

    #[test]
    fn take_drop_keep_exact_step_join_windows_and_scheduler_onsets() {
        fn shows(session: &Session, begin: Fraction, end: Fraction) -> Vec<String> {
            session
                .query(begin, end)
                .expect("query take/drop pattern")
                .into_iter()
                .map(|hap| hap.show())
                .collect()
        }

        let mut session = Session::new().expect("session");
        session
            .evaluate("new Pattern(state => pure('x').query(state)).take(1)")
            .expect("evaluate no-step take");
        assert!(
            !session.active_needs_host(),
            "take did not prune an unreachable no-step receiver callback"
        );
        assert!(
            shows(&session, Fraction::ZERO, Fraction::ONE).is_empty(),
            "a no-step receiver must become nothing"
        );

        session
            .evaluate("sequence(0, 1, 2).take(slowcat(1, 2))")
            .expect("evaluate patterned take");
        assert!(!session.active_needs_host());
        assert_eq!(
            session.js.active_pattern().expect("active take").steps,
            Some(Fraction::ONE),
            "take metadata must come from StepJoin's cycle-zero slice"
        );
        assert_eq!(
            shows(&session, Fraction::ZERO, Fraction::int(2)),
            ["[ 0/1 → 1/1 | 0 ]", "[ 1/1 → 2/1 | 0 ]"],
            "joined take query stopped selecting from its begin cycle"
        );
        assert_eq!(
            shows(&session, Fraction::ONE, Fraction::int(2)),
            ["[ 1/1 → 3/2 | 0 ]", "[ 3/2 → 2/1 | 1 ]"],
            "separate take query lost cycle one's amount"
        );
        let take_play = session.play(4.0).expect("schedule patterned take");
        let take_onsets = take_play
            .onsets
            .iter()
            .map(|onset| {
                (
                    onset.whole_begin.as_str(),
                    onset.value_show.as_str(),
                    onset.target_time,
                    onset.duration_secs,
                )
            })
            .collect::<Vec<_>>();
        assert_eq!(
            take_onsets,
            [
                ("0/1", "0", 0.0, 2.0),
                ("1/1", "0", 2.0, 1.0),
                ("3/2", "1", 3.0, 1.0),
                // The inclusive boundary onset was materialised by the
                // preceding lookahead window, whose StepJoin carrier is 2.
                ("2/1", "0", 4.0, 1.0),
            ],
            "scheduler stopped preserving patterned take's query windows"
        );

        // Use a fresh transport anchor for the second scheduler assertion;
        // live replacement after a completed play intentionally keeps the
        // existing clock rather than rewinding to cycle zero.
        let mut session = Session::new().expect("drop session");
        session
            .evaluate("sequence(0, 1, 2).drop(slowcat(1, 2))")
            .expect("evaluate patterned drop");
        assert!(!session.active_needs_host());
        assert_eq!(
            session.js.active_pattern().expect("active drop").steps,
            Some(Fraction::int(2)),
            "drop metadata must come from StepJoin's cycle-zero slice"
        );
        assert_eq!(
            shows(&session, Fraction::ZERO, Fraction::int(2)),
            [
                "[ 0/1 → 1/2 | 1 ]",
                "[ 1/2 → 1/1 | 2 ]",
                "[ 1/1 → 3/2 | 1 ]",
                "[ 3/2 → 2/1 | 2 ]",
            ],
            "joined drop query stopped selecting from its begin cycle"
        );
        assert_eq!(
            shows(&session, Fraction::ONE, Fraction::int(2)),
            ["[ 1/1 → 2/1 | 2 ]"],
            "separate drop query lost cycle one's amount"
        );
        let drop_play = session.play(4.0).expect("schedule patterned drop");
        let drop_onsets = drop_play
            .onsets
            .iter()
            .map(|onset| {
                (
                    onset.whole_begin.as_str(),
                    onset.value_show.as_str(),
                    onset.target_time,
                    onset.duration_secs,
                )
            })
            .collect::<Vec<_>>();
        assert_eq!(
            drop_onsets,
            [
                ("0/1", "1", 0.0, 1.0),
                ("1/2", "2", 1.0, 1.0),
                ("1/1", "2", 2.0, 2.0),
                // As above, the preceding factor-2 lookahead owns the exact
                // boundary onset.
                ("2/1", "2", 4.0, 2.0),
            ],
            "scheduler stopped preserving patterned drop's query windows"
        );
    }

    #[test]
    fn raw_take_drop_reach_session_query_play_without_stepwise_expansion_charge() {
        const TAKE_HAPS: &[&str] = &["[ 0/1 → 1/2 | d ]", "[ 1/2 → 1/1 | e ]"];
        const DROP_HAPS: &[&str] = &[
            "[ 0/1 → 1/3 | c ]",
            "[ 1/3 → 2/3 | d ]",
            "[ 2/3 → 1/1 | e ]",
        ];

        for (source, expected_haps, expected_steps, expected_onsets) in [
            (
                r#"(() => {
                  if (Object.hasOwn(globalThis, '_take')
                      || Object.hasOwn(rustelScope, '_take')
                      || typeof globalThis._take !== 'undefined'
                      || typeof rustelScope._take !== 'undefined') {
                    throw new Error('raw take leaked outside Pattern.prototype');
                  }
                  return sequence('a','b','c','d','e')._take(-2);
                })()"#,
                TAKE_HAPS,
                2,
                vec![("0/1", "d"), ("1/2", "e"), ("1/1", "d")],
            ),
            (
                r#"(() => {
                  if (Object.hasOwn(globalThis, '_drop')
                      || Object.hasOwn(rustelScope, '_drop')
                      || typeof globalThis._drop !== 'undefined'
                      || typeof rustelScope._drop !== 'undefined') {
                    throw new Error('raw drop leaked outside Pattern.prototype');
                  }
                  return sequence('a','b','c','d','e')._drop(2);
                })()"#,
                DROP_HAPS,
                3,
                vec![("0/1", "c"), ("1/3", "d"), ("2/3", "e"), ("1/1", "c")],
            ),
        ] {
            let mut session = Session::new().expect("raw take/drop session");
            session
                .evaluate(source)
                .unwrap_or_else(|error| panic!("evaluate raw take/drop {source}: {error}"));
            assert_native_query_and_play(
                &mut session,
                Some(Fraction::int(expected_steps)),
                expected_haps,
                &expected_onsets,
            );
        }

        // Raw take/drop only build zoom/take graphs; unlike shrink/grow they
        // do not materialise one entry per declared step.  Crossing the shared
        // expansion threshold is therefore a successful O(1) query, not a
        // `StepwiseExpansion` refusal or a new resource operation.
        let over = rustel_core::MAX_STEPWISE_ENTRIES + 1;
        for (name, expected_steps) in [("_take", 1), ("_drop", over - 1)] {
            let mut session = Session::new().expect("raw take/drop resource session");
            session
                .evaluate(&format!("pure('x').setSteps({over}).{name}(1)"))
                .unwrap_or_else(|error| panic!("construct pool-neutral {name}: {error}"));
            assert!(!session.active_needs_host());
            assert_eq!(
                session
                    .js
                    .active_pattern()
                    .expect("active pool-neutral raw graph")
                    .steps,
                Some(Fraction::int(i128::from(expected_steps)))
            );
            assert_eq!(
                session
                    .query(Fraction::ZERO, Fraction::ONE)
                    .unwrap_or_else(|error| panic!("query pool-neutral {name}: {error}"))
                    .len(),
                1
            );
            assert!(
                session.scheduler.refusal().is_none(),
                "{name}: raw zoom/take graph invented a stepwise refusal"
            );
        }
    }

    #[test]
    fn raw_extend_replicate_reach_session_query_play_without_stepwise_expansion_charge() {
        const EXTEND_HAPS: &[&str] = &[
            "[ 0/1 → 1/4 | a ]",
            "[ 1/4 → 1/2 | b ]",
            "[ 1/2 → 3/4 | c ]",
            "[ 3/4 → 1/1 | d ]",
        ];
        const REPLICATE_HAPS: &[&str] = &[
            "[ 0/1 → 1/4 | a ]",
            "[ 1/4 → 1/2 | b ]",
            "[ 1/2 → 3/4 | a ]",
            "[ 3/4 → 1/1 | b ]",
        ];

        for (name, expected_haps, expected_onsets) in [
            (
                "_extend",
                EXTEND_HAPS,
                vec![
                    ("0/1", "a"),
                    ("1/4", "b"),
                    ("1/2", "c"),
                    ("3/4", "d"),
                    ("1/1", "a"),
                ],
            ),
            (
                "_replicate",
                REPLICATE_HAPS,
                vec![
                    ("0/1", "a"),
                    ("1/4", "b"),
                    ("1/2", "a"),
                    ("3/4", "b"),
                    ("1/1", "c"),
                ],
            ),
        ] {
            let source = format!(
                r#"(() => {{
                  if (Object.hasOwn(globalThis, '{name}')
                      || Object.hasOwn(rustelScope, '{name}')
                      || typeof globalThis.{name} !== 'undefined'
                      || typeof rustelScope.{name} !== 'undefined') {{
                    throw new Error('raw stepwise chain leaked outside Pattern.prototype');
                  }}
                  return slowcat(sequence('a','b'),sequence('c','d')).{name}(2);
                }})()"#
            );
            let mut session = Session::new().expect("raw extend/replicate session");
            session
                .evaluate(&source)
                .unwrap_or_else(|error| panic!("evaluate {name}: {error}"));
            assert_native_query_and_play(
                &mut session,
                Some(Fraction::int(4)),
                expected_haps,
                &expected_onsets,
            );
        }

        // These bodies compose O(1) graph transforms. Large declared metadata
        // must not borrow a materialising stepwise operation or invent a new
        // QueryLimit variant merely because the raw slot is stepwise-adjacent.
        let over = rustel_core::MAX_STEPWISE_ENTRIES + 1;
        for name in ["_extend", "_replicate"] {
            let mut session = Session::new().expect("raw chain resource session");
            session
                .evaluate(&format!("pure('x').setSteps({over}).{name}(1)"))
                .unwrap_or_else(|error| panic!("construct pool-neutral {name}: {error}"));
            assert!(!session.active_needs_host());
            assert_eq!(
                session
                    .js
                    .active_pattern()
                    .expect("active pool-neutral raw chain")
                    .steps,
                Some(Fraction::int(i128::from(over)))
            );
            assert_eq!(
                session
                    .query(Fraction::ZERO, Fraction::ONE)
                    .unwrap_or_else(|error| panic!("query pool-neutral {name}: {error}"))
                    .len(),
                1
            );
            assert!(
                session.scheduler.refusal().is_none(),
                "{name}: raw chain invented a stepwise refusal"
            );
        }
    }

    #[test]
    fn raw_expand_contract_and_with_steps_reach_session_query_play_without_charge() {
        const EXPANDED_HAPS: &[&str] = &[
            "[ 0/1 → 2/5 | a ]",
            "[ 2/5 → 4/5 | b ]",
            "[ 4/5 → 1/1 | z ]",
        ];
        const CONTRACTED_HAPS: &[&str] = &[
            "[ 0/1 → 1/4 | a ]",
            "[ 1/4 → 1/2 | b ]",
            "[ 1/2 → 1/1 | z ]",
        ];

        for (name, transform, expected_steps, expected_haps, expected_onsets) in [
            (
                "withSteps",
                "sequence('a','b').withSteps(steps => steps.mul(2))",
                Fraction::int(5),
                EXPANDED_HAPS,
                vec![("0/1", "a"), ("2/5", "b"), ("4/5", "z"), ("1/1", "a")],
            ),
            (
                "_expand",
                "sequence('a','b')._expand(2)",
                Fraction::int(5),
                EXPANDED_HAPS,
                vec![("0/1", "a"), ("2/5", "b"), ("4/5", "z"), ("1/1", "a")],
            ),
            (
                "_contract",
                "sequence('a','b')._contract(2)",
                Fraction::int(2),
                CONTRACTED_HAPS,
                vec![("0/1", "a"), ("1/4", "b"), ("1/2", "z"), ("1/1", "a")],
            ),
        ] {
            let source = format!(
                r#"(() => {{
                  if (Object.hasOwn(globalThis, '{name}')
                      || Object.hasOwn(rustelScope, '{name}')
                      || typeof globalThis.{name} !== 'undefined'
                      || typeof rustelScope.{name} !== 'undefined') {{
                    throw new Error('prototype-only metadata helper leaked');
                  }}
                  return stepcat({transform}, 'z');
                }})()"#
            );
            let mut session = Session::new().expect("raw metadata session");
            session
                .evaluate(&source)
                .unwrap_or_else(|error| panic!("evaluate {name}: {error}"));
            assert_native_query_and_play(
                &mut session,
                Some(expected_steps),
                expected_haps,
                &expected_onsets,
            );
        }

        // These operations only replace declared metadata and do not
        // materialise one entry per step. Crossing the shared expansion limit
        // must therefore stay a successful one-hap query with no refusal.
        let over = rustel_core::MAX_STEPWISE_ENTRIES + 1;
        for (name, transform, expected_steps) in [
            (
                "withSteps",
                format!("pure('x').setSteps({over}).withSteps(s => s.mul(2))"),
                Fraction::int(i128::from(over) * 2),
            ),
            (
                "_expand",
                format!("pure('x').setSteps({over})._expand(2)"),
                Fraction::int(i128::from(over) * 2),
            ),
            (
                "_contract",
                format!("pure('x').setSteps({over})._contract(2)"),
                Fraction::new(i128::from(over), 2),
            ),
        ] {
            let mut session = Session::new().expect("pool-neutral metadata session");
            session
                .evaluate(&transform)
                .unwrap_or_else(|error| panic!("construct pool-neutral {name}: {error}"));
            assert!(!session.active_needs_host());
            assert_eq!(
                session
                    .js
                    .active_pattern()
                    .expect("active pool-neutral metadata graph")
                    .steps,
                Some(expected_steps)
            );
            assert_eq!(
                session
                    .query(Fraction::ZERO, Fraction::ONE)
                    .unwrap_or_else(|error| panic!("query pool-neutral {name}: {error}"))
                    .len(),
                1
            );
            assert!(
                session.scheduler.refusal().is_none(),
                "{name}: metadata-only transform invented a resource refusal"
            );
        }
    }

    #[test]
    fn raw_range_pair_reaches_session_query_play_without_resource_charge() {
        const HAPS: &[&str] = &[
            "[ 0/1 → 1/4 | 10 ]",
            "[ 1/4 → 1/2 | 15 ]",
            "[ 1/2 → 3/4 | 20 ]",
            "[ 3/4 → 1/1 | z ]",
        ];
        const ONSETS: &[(&str, &str)] = &[
            ("0/1", "10"),
            ("1/4", "15"),
            ("1/2", "20"),
            ("3/4", "z"),
            ("1/1", "10"),
        ];

        for (name, transform) in [
            ("_range", "sequence(0,.5,1)._range(10,20)"),
            ("_range2", "sequence(-1,0,1)._range2(10,20)"),
        ] {
            let source = format!(
                r#"(() => {{
                  if (Object.hasOwn(globalThis, '{name}')
                      || Object.hasOwn(rustelScope, '{name}')
                      || typeof globalThis.{name} !== 'undefined'
                      || typeof rustelScope.{name} !== 'undefined'
                      || Object.hasOwn(Pattern.prototype, '_rangex')
                      || typeof Pattern.prototype._rangex !== 'undefined') {{
                    throw new Error('bounded raw range surface leaked');
                  }}
                  return stepcat({transform}, 'z');
                }})()"#
            );
            let mut session = Session::new().expect("raw range session");
            session
                .evaluate(&source)
                .unwrap_or_else(|error| panic!("evaluate {name}: {error}"));
            assert_native_query_and_play(&mut session, Some(Fraction::int(4)), HAPS, ONSETS);
        }

        // Range is an O(1) composer chain. Large declared metadata must stay a
        // successful one-hap graph and must not borrow a stepwise operation or
        // introduce a new QueryLimit/refusal name.
        let over = rustel_core::MAX_STEPWISE_ENTRIES + 1;
        for (name, source) in [
            ("_range", format!("pure(.5).setSteps({over})._range(10,20)")),
            (
                "_range2",
                format!("pure(0).setSteps({over})._range2(10,20)"),
            ),
        ] {
            let mut session = Session::new().expect("pool-neutral raw range session");
            session
                .evaluate(&source)
                .unwrap_or_else(|error| panic!("construct pool-neutral {name}: {error}"));
            assert!(!session.active_needs_host());
            assert_eq!(
                session
                    .js
                    .active_pattern()
                    .expect("active pool-neutral raw range")
                    .steps,
                Some(Fraction::int(i128::from(over)))
            );
            assert_eq!(
                session
                    .query(Fraction::ZERO, Fraction::ONE)
                    .unwrap_or_else(|error| panic!("query pool-neutral {name}: {error}"))
                    .len(),
                1
            );
            assert!(
                session.scheduler.refusal().is_none(),
                "{name}: range invented a resource refusal"
            );
        }
    }

    #[test]
    fn raw_apply_reaches_session_query_play_and_preserves_nested_resource_limits() {
        const HAPS: &[&str] = &[
            "[ 0/1 → 1/3 | a ]",
            "[ 1/3 → 2/3 | b ]",
            "[ 2/3 → 1/1 | z ]",
        ];
        const ONSETS: &[(&str, &str)] = &[("0/1", "a"), ("1/3", "b"), ("2/3", "z"), ("1/1", "a")];
        let source = r#"(() => {
          if (Object.hasOwn(globalThis, '_apply')
              || Object.hasOwn(rustelScope, '_apply')
              || typeof globalThis._apply !== 'undefined'
              || typeof rustelScope._apply !== 'undefined'
              || Object.hasOwn(Pattern.prototype, '_swingBy')
              || typeof Pattern.prototype._swingBy !== 'undefined') {
            throw new Error('bounded raw apply surface leaked');
          }
          return stepcat(
            pure('ignored')._apply(value => value, sequence('a', 'b')),
            'z'
          );
        })()"#;
        let mut session = Session::new().expect("raw apply session");
        session
            .evaluate(source)
            .expect("evaluate raw apply product score");
        assert_native_query_and_play(&mut session, Some(Fraction::int(3)), HAPS, ONSETS);

        // `_apply` itself only calls and returns: large metadata on the exact
        // selected terminal must not create a new resource operation.
        let over = rustel_core::MAX_STEPWISE_ENTRIES + 1;
        let mut neutral = Session::new().expect("pool-neutral raw apply session");
        neutral
            .evaluate(&format!(
                "pure('x').setSteps({over})._apply(value => value)"
            ))
            .expect("construct pool-neutral raw apply");
        assert!(!neutral.active_needs_host());
        assert_eq!(
            neutral
                .js
                .active_pattern()
                .expect("active pool-neutral raw apply")
                .steps,
            Some(Fraction::int(i128::from(over)))
        );
        assert_eq!(
            neutral
                .query(Fraction::ZERO, Fraction::ONE)
                .expect("query pool-neutral raw apply")
                .len(),
            1
        );
        assert!(
            neutral.scheduler.refusal().is_none(),
            "raw apply invented a resource refusal"
        );

        // Work selected by the eager callback keeps the selected operation's
        // existing bound and attribution; `_apply` is not a laundering layer.
        let mut nested = Session::new().expect("nested raw apply refusal session");
        nested
            .evaluate(&format!("gap({over})._apply(value => value.shrink(0))"))
            .expect("construct callback-selected shrink refusal");
        assert!(!nested.active_needs_host());
        assert_nested_shrink_refusal(&mut nested);
    }

    #[test]
    fn raw_when_reaches_session_branches_and_preserves_nested_resource_limits() {
        const HAPS: &[&str] = &[
            "[ 0/1 → 1/3 | a ]",
            "[ 1/3 → 2/3 | b ]",
            "[ 2/3 → 1/1 | z ]",
        ];
        const ONSETS: &[(&str, &str)] = &[("0/1", "a"), ("1/3", "b"), ("2/3", "z"), ("1/1", "a")];
        let source = r#"(() => {
          if (Object.hasOwn(globalThis, '_when')
              || Object.hasOwn(rustelScope, '_when')
              || typeof globalThis._when !== 'undefined'
              || typeof rustelScope._when !== 'undefined'
              || typeof Pattern.prototype._when !== 'function'
              || Object.hasOwn(Pattern.prototype, '_swingBy')
              || typeof Pattern.prototype._swingBy !== 'undefined') {
            throw new Error('bounded raw when surface leaked');
          }
          globalThis.rawWhenSessionFalseHits = 0;
          globalThis.rawWhenSessionTrueHits = 0;
          const falseSelected = pure('outer')._when(
            0n,
            () => {
              rawWhenSessionFalseHits++;
              throw new Error('false callback ran');
            },
            sequence('a', 'b')
          );
          const guardedTruthy = new Proxy({}, {
            get() { throw new Error('ToBoolean read its object'); }
          });
          const trueSelected = pure('outer')._when(
            guardedTruthy,
            value => {
              rawWhenSessionTrueHits++;
              return value;
            },
            pure('z')
          );
          if (rawWhenSessionFalseHits !== 0 || rawWhenSessionTrueHits !== 1) {
            throw new Error('raw when callback phase changed');
          }
          return stepcat(falseSelected, trueSelected);
        })()"#;
        let mut session = Session::new().expect("raw when session");
        session
            .evaluate(source)
            .expect("evaluate raw when product score");
        assert_eq!(session.js.get_number("rawWhenSessionFalseHits"), Some(0.0));
        assert_eq!(session.js.get_number("rawWhenSessionTrueHits"), Some(1.0));
        assert_native_query_and_play(&mut session, Some(Fraction::int(3)), HAPS, ONSETS);

        // The false branch suppresses the callback body, and `_when` itself
        // adds no resource operation. Large metadata on the selected terminal
        // therefore remains a successful one-hap graph.
        let over = rustel_core::MAX_STEPWISE_ENTRIES + 1;
        let mut neutral = Session::new().expect("pool-neutral raw when session");
        neutral
            .evaluate(&format!(
                r#"(() => {{
                  globalThis.rawWhenSuppressedHits = 0;
                  const result = gap({over})._when(
                    false,
                    value => {{
                      rawWhenSuppressedHits++;
                      return value.shrink(0);
                    }},
                    pure('safe').setSteps({over})
                  );
                  if (rawWhenSuppressedHits !== 0) {{
                    throw new Error('false branch executed nested work');
                  }}
                  return result;
                }})()"#
            ))
            .expect("construct pool-neutral false raw when");
        assert_eq!(neutral.js.get_number("rawWhenSuppressedHits"), Some(0.0));
        assert!(!neutral.active_needs_host());
        assert_eq!(
            neutral
                .js
                .active_pattern()
                .expect("active pool-neutral raw when")
                .steps,
            Some(Fraction::int(i128::from(over)))
        );
        assert_eq!(
            neutral
                .query(Fraction::ZERO, Fraction::ONE)
                .expect("query pool-neutral raw when")
                .len(),
            1
        );
        assert!(
            neutral.scheduler.refusal().is_none(),
            "raw when invented a resource refusal"
        );

        // Work actually selected by the true branch keeps the chosen
        // operation's existing type, limit, and attribution. This does not
        // make arbitrary callbacks resource-free or generally bounded.
        let mut nested = Session::new().expect("nested raw when refusal session");
        nested
            .evaluate(&format!(
                "gap({over})._when(true, value => value.shrink(0))"
            ))
            .expect("construct true-branch shrink refusal");
        assert!(!nested.active_needs_host());
        assert_nested_shrink_refusal(&mut nested);
    }

    #[test]
    fn raw_never_always_reach_session_and_preserve_selected_resource_limits() {
        const HAPS: &[&str] = &[
            "[ 0/1 → 1/3 | a ]",
            "[ 1/3 → 2/3 | b ]",
            "[ 2/3 → 1/1 | z ]",
        ];
        const ONSETS: &[(&str, &str)] = &[("0/1", "a"), ("1/3", "b"), ("2/3", "z"), ("1/1", "a")];
        let source = r#"(() => {
          const names = ['never', '_never', 'always', '_always'];
          const order = Object.getOwnPropertyNames(Pattern.prototype)
            .filter(name => names.includes(name));
          if (order.join(',') !== names.join(',')
              || Object.hasOwn(globalThis, '_never')
              || Object.hasOwn(globalThis, '_always')
              || Object.hasOwn(rustelScope, '_never')
              || Object.hasOwn(rustelScope, '_always')
              || typeof globalThis._never !== 'undefined'
              || typeof globalThis._always !== 'undefined'
              || typeof rustelScope._never !== 'undefined'
              || typeof rustelScope._always !== 'undefined') {
            throw new Error('bounded raw never/always surface changed');
          }
          let ignoredTouches = 0;
          const ignored = new Proxy(function () {}, {
            apply() { ignoredTouches++; throw new Error('never callback ran'); },
            get() { ignoredTouches++; throw new Error('never callback read'); },
          });
          let alwaysHits = 0;
          const neverSelected = pure('outer')._never(
            ignored, sequence('a', 'b'), { marker: 'ignored extra' }
          );
          const alwaysSelected = pure('outer')._always(
            value => { alwaysHits++; return value; },
            pure('z'),
            { marker: 'ignored extra' }
          );
          if (ignoredTouches !== 0 || alwaysHits !== 1) {
            throw new Error('raw never/always callback phase changed');
          }
          return stepcat(neverSelected, alwaysSelected);
        })()"#;
        let mut session = Session::new().expect("raw never/always session");
        session
            .evaluate(source)
            .expect("evaluate raw never/always product score");
        assert_native_query_and_play(&mut session, Some(Fraction::int(3)), HAPS, ONSETS);

        // The pair adds no resource operation: `_never` merely selects an
        // already-built target and `_always` merely calls and returns. Large
        // metadata on an otherwise bounded selected terminal stays valid.
        let over = rustel_core::MAX_STEPWISE_ENTRIES + 1;
        for (name, source) in [
            (
                "_never",
                format!("pure('outer')._never(null, pure('safe').setSteps({over}))"),
            ),
            (
                "_always",
                format!("pure('safe').setSteps({over})._always(value => value)"),
            ),
        ] {
            let mut neutral = Session::new().expect("pool-neutral raw never/always session");
            neutral
                .evaluate(&source)
                .unwrap_or_else(|error| panic!("construct pool-neutral {name}: {error}"));
            assert!(
                !neutral.active_needs_host(),
                "{name}: selected graph needs host"
            );
            assert_eq!(
                neutral
                    .js
                    .active_pattern()
                    .expect("active pool-neutral raw never/always")
                    .steps,
                Some(Fraction::int(i128::from(over)))
            );
            assert_eq!(
                neutral
                    .query(Fraction::ZERO, Fraction::ONE)
                    .unwrap_or_else(|error| panic!("query pool-neutral {name}: {error}"))
                    .len(),
                1
            );
            assert!(
                neutral.scheduler.refusal().is_none(),
                "{name} invented a resource refusal"
            );
        }

        // Caller-evaluated work selected by `_never`, and callback-selected
        // work returned by `_always`, retain the chosen operation's existing
        // type, limit, and attribution. This is not an arbitrary-callback
        // resource-free or general-bound claim.
        for (name, source) in [
            (
                "_never",
                format!("pure('outer')._never(null, gap({over}).shrink(0))"),
            ),
            (
                "_always",
                format!("gap({over})._always(value => value.shrink(0))"),
            ),
        ] {
            let mut nested = Session::new().expect("nested raw never/always refusal session");
            nested
                .evaluate(&source)
                .unwrap_or_else(|error| panic!("construct nested {name}: {error}"));
            assert!(
                !nested.active_needs_host(),
                "{name}: selected graph needs host"
            );
            assert_nested_shrink_refusal(&mut nested);
        }
    }

    #[test]
    fn raw_swing_reaches_session_with_scalar_steps_and_existing_resource_attribution() {
        const HAPS: &[&str] = &[
            "[ (0/1 → 1/8) ⇝ 1/4 | a ]",
            "[ 1/24 ⇜ (1/8 → 1/4) ⇝ 7/24 | a ]",
            "[ (1/4 → 3/8) ⇝ 1/2 | b ]",
            "[ 7/24 ⇜ (3/8 → 1/2) ⇝ 13/24 | b ]",
            "[ (1/2 → 5/8) ⇝ 3/4 | c ]",
            "[ 13/24 ⇜ (5/8 → 3/4) ⇝ 19/24 | c ]",
            "[ (3/4 → 7/8) ⇝ 1/1 | d ]",
            "[ 19/24 ⇜ (7/8 → 1/1) ⇝ 25/24 | d ]",
        ];
        const ONSETS: &[(&str, &str)] = &[
            ("0/1", "a"),
            ("1/4", "b"),
            ("1/2", "c"),
            ("3/4", "d"),
            ("1/1", "a"),
        ];
        let source = r#"(() => {
          const names = ['swing', '_swing'];
          const order = Object.getOwnPropertyNames(Pattern.prototype)
            .filter(name => names.includes(name));
          if (order.join(',') !== names.join(',')
              || Object.hasOwn(globalThis, '_swing')
              || Object.hasOwn(rustelScope, '_swing')
              || typeof globalThis._swing !== 'undefined'
              || typeof rustelScope._swing !== 'undefined'
              || Object.hasOwn(Pattern.prototype, '_swingBy')
              || typeof Pattern.prototype._swingBy !== 'undefined') {
            throw new Error('bounded raw swing surface changed');
          }
          return sequence('a','b','c','d').setSteps(7)._swing(4);
        })()"#;
        let mut session = Session::new().expect("raw swing session");
        session
            .evaluate(source)
            .expect("evaluate raw swing product score");
        assert_native_query_and_play(&mut session, Some(Fraction::int(7)), HAPS, ONSETS);

        let mut zero = Session::new().expect("zero raw swing session");
        zero.evaluate("sequence('a','b').setSteps(7)._swing(0)")
            .expect("evaluate zero raw swing");
        assert!(!zero.active_needs_host());
        assert_eq!(
            zero.js
                .active_pattern()
                .expect("active zero raw swing")
                .steps,
            Some(Fraction::ONE),
            "zero raw swing replaced constructed silence metadata"
        );
        assert!(
            zero.query(Fraction::ZERO, Fraction::ONE)
                .expect("query zero raw swing")
                .is_empty(),
            "zero raw swing produced haps"
        );

        // `_swing` adds only one dynamic handoff to the existing public
        // swing graph. Oversized declared metadata alone must not invent a
        // raw-specific resource operation or refusal.
        let over = rustel_core::MAX_STEPWISE_ENTRIES + 1;
        let mut neutral = Session::new().expect("pool-neutral raw swing session");
        neutral
            .evaluate(&format!("pure('safe').setSteps({over})._swing(4)"))
            .expect("construct pool-neutral raw swing");
        assert!(!neutral.active_needs_host());
        assert_eq!(
            neutral
                .js
                .active_pattern()
                .expect("active pool-neutral raw swing")
                .steps,
            Some(Fraction::int(i128::from(over)))
        );
        assert_eq!(
            neutral
                .query(Fraction::ZERO, Fraction::ONE)
                .expect("query pool-neutral raw swing")
                .len(),
            8,
            "raw swing's fixed scalar graph changed"
        );
        assert!(
            neutral.scheduler.refusal().is_none(),
            "raw swing invented a resource refusal"
        );

        // Existing nested work keeps its own operation name/type/limit after
        // the swing graph wraps it. This is not a general density bound.
        let mut nested = Session::new().expect("nested raw swing refusal session");
        nested
            .evaluate(&format!("gap({over}).shrink(0)._swing(4)"))
            .expect("construct nested raw swing refusal");
        assert!(!nested.active_needs_host());
        assert_nested_shrink_refusal(&mut nested);
    }

    #[test]
    fn raw_signal_quartet_reaches_session_query_play_and_preserves_nested_attribution() {
        const SURFACE: &str = r#"
          const projected = [
            'often', '_often', 'rarely', '_rarely',
            'almostNever', '_almostNever',
            'almostAlways', '_almostAlways'
          ];
          const order = Object.getOwnPropertyNames(Pattern.prototype)
            .filter(name => projected.includes(name));
          if (order.join(',') !== projected.join(',')) {
            throw new Error('raw signal quartet order changed');
          }
          for (const raw of projected.filter(name => name.startsWith('_'))) {
            if (Object.hasOwn(globalThis, raw)
                || Object.hasOwn(rustelScope, raw)
                || typeof globalThis[raw] !== 'undefined'
                || typeof rustelScope[raw] !== 'undefined') {
              throw new Error(`raw signal destination leaked: ${raw}`);
            }
          }
        "#;
        for (raw_name, query_values, play_values) in [
            (
                "_often",
                ["_often!", "_often!"],
                ["_often!", "_often!", "_often"],
            ),
            (
                "_rarely",
                ["_rarely!", "_rarely"],
                ["_rarely!", "_rarely", "_rarely"],
            ),
            (
                "_almostNever",
                ["_almostNever!", "_almostNever"],
                ["_almostNever!", "_almostNever", "_almostNever"],
            ),
            (
                "_almostAlways",
                ["_almostAlways!", "_almostAlways!"],
                ["_almostAlways!", "_almostAlways!", "_almostAlways"],
            ),
        ] {
            let source = format!(
                r#"(() => {{
                     {SURFACE}
                     globalThis.rawSignalSessionCalls = 0;
                     return pure('{raw_name}').setSteps(7).{raw_name}(
                       function (pat) {{
                         'use strict';
                         rawSignalSessionCalls++;
                         return pat.fmap(value => `${{value}}!`);
                       }}
                     );
                   }})()"#,
            );
            let mut session = Session::new().expect("raw signal query session");
            session
                .evaluate(&source)
                .unwrap_or_else(|error| panic!("evaluate Session {raw_name}: {error}"));
            assert_eq!(session.js.get_number("rawSignalSessionCalls"), Some(0.0));
            assert!(
                session.active_needs_host(),
                "{raw_name}: dynamic scalar join was granted host-free purity"
            );
            assert_eq!(
                session
                    .js
                    .active_pattern()
                    .expect("active raw signal")
                    .steps,
                None,
                "{raw_name}: outer result retained source steps"
            );
            assert_eq!(
                session
                    .query(Fraction::ZERO, Fraction::int(2))
                    .unwrap_or_else(|error| panic!("query Session {raw_name}: {error}"))
                    .into_iter()
                    .map(|hap| hap.value.show())
                    .collect::<Vec<_>>(),
                query_values,
                "{raw_name}: Session scalar RNG values changed"
            );
            assert_eq!(
                session.js.get_number("rawSignalSessionCalls"),
                Some(2.0),
                "{raw_name}: callback was not lazy once per carrier cycle"
            );

            let mut play = Session::new().expect("raw signal play session");
            play.evaluate(&source)
                .unwrap_or_else(|error| panic!("evaluate play {raw_name}: {error}"));
            assert_eq!(
                play.play(4.0)
                    .unwrap_or_else(|error| panic!("play Session {raw_name}: {error}"))
                    .onsets
                    .iter()
                    .map(|onset| (onset.whole_begin.as_str(), onset.value_show.as_str(),))
                    .collect::<Vec<_>>(),
                [
                    ("0/1", play_values[0]),
                    ("1/1", play_values[1]),
                    ("2/1", play_values[2]),
                ],
                "{raw_name}: Session scheduler RNG onsets changed"
            );
        }

        let over = rustel_core::MAX_STEPWISE_ENTRIES + 1;
        for raw_name in ["_often", "_rarely", "_almostNever", "_almostAlways"] {
            // The shorthand adds no raw-specific shared-pool operation.  The
            // Canonical tagged `rev` takes the pure native dispatch, so this
            // is also the host-free proof. Oversized source metadata alone
            // remains successful and pool-neutral.
            let mut neutral = Session::new().expect("pool-neutral raw signal session");
            neutral
                .evaluate(&format!("pure('safe').setSteps({over}).{raw_name}(rev)"))
                .unwrap_or_else(|error| panic!("construct neutral {raw_name}: {error}"));
            assert!(
                !neutral.active_needs_host(),
                "{raw_name}: tagged native route unexpectedly needs QuickJS"
            );
            assert_eq!(
                neutral
                    .query(Fraction::ZERO, Fraction::ONE)
                    .unwrap_or_else(|error| panic!("query neutral {raw_name}: {error}"))
                    .len(),
                1,
                "{raw_name}: neutral shorthand changed density"
            );
            assert!(
                neutral.scheduler.refusal().is_none(),
                "{raw_name}: invented a raw-specific resource refusal"
            );

            // The callback runs for every queried carrier before random
            // selection. Existing nested work therefore keeps its original
            // operation/type/limit attribution for every threshold.
            let mut nested = Session::new().expect("nested raw signal refusal session");
            nested
                .evaluate(&format!("gap({over}).{raw_name}(value => value.shrink(0))"))
                .unwrap_or_else(|error| panic!("construct nested {raw_name}: {error}"));
            assert_nested_shrink_refusal(&mut nested);
        }
    }

    #[test]
    fn raw_set_reaches_session_query_play_and_preserves_source_resource_attribution() {
        const SOURCE: &str = r#"
          (() => {
            const order = Object.getOwnPropertyNames(Pattern.prototype)
              .filter(name => name === '_set' || name === 'set');
            const keys = Object.keys(Pattern.prototype)
              .filter(name => name === '_set' || name === 'set');
            const raw = Object.getOwnPropertyDescriptor(Pattern.prototype, '_set');
            const publicSet = Object.getOwnPropertyDescriptor(Pattern.prototype, 'set');
            if (order.join(',') !== '_set,set'
                || keys.join(',') !== '_set'
                || raw.writable !== true
                || raw.enumerable !== true
                || raw.configurable !== true
                || raw.value.name !== ''
                || raw.value.length !== 1
                || typeof publicSet.get !== 'function'
                || publicSet.enumerable !== false
                || publicSet.configurable !== true
                || Object.hasOwn(globalThis, '_set')
                || Object.hasOwn(rustelScope, '_set')
                || typeof globalThis._set !== 'undefined'
                || typeof rustelScope._set !== 'undefined') {
              throw new Error('raw set Session surface changed');
            }
            return pure(1).setSteps(7)._set(9);
          })()
        "#;
        let mut session = Session::new().expect("raw set query session");
        session.evaluate(SOURCE).expect("evaluate Session raw set");
        assert!(
            session.active_needs_host(),
            "raw set's lexical fmap callback was granted host-free purity"
        );
        assert_eq!(
            session.js.active_pattern().expect("active raw set").steps,
            Some(Fraction::int(7)),
            "raw set lost source steps"
        );
        assert_eq!(
            session
                .query(Fraction::ZERO, Fraction::int(2))
                .expect("query Session raw set")
                .into_iter()
                .map(|hap| hap.show())
                .collect::<Vec<_>>(),
            ["[ 0/1 → 1/1 | 9 ]", "[ 1/1 → 2/1 | 9 ]"],
            "Session raw set query changed"
        );

        let mut play = Session::new().expect("raw set play session");
        play.evaluate(SOURCE).expect("evaluate play raw set");
        assert_eq!(
            play.play(4.0)
                .expect("play Session raw set")
                .onsets
                .iter()
                .map(|onset| (onset.whole_begin.as_str(), onset.value_show.as_str()))
                .collect::<Vec<_>>(),
            [("0/1", "9"), ("1/1", "9"), ("2/1", "9")],
            "Session raw set scheduler onsets changed"
        );

        let over = rustel_core::MAX_STEPWISE_ENTRIES + 1;
        // `_set` adds no shared-pool operation or charge. Oversized declared
        // metadata alone stays successful, but the JS mapper remains host
        // backed; this is not an arbitrary-custom-fmap resource claim.
        let mut neutral = Session::new().expect("pool-neutral raw set session");
        neutral
            .evaluate(&format!("pure('safe').setSteps({over})._set('changed')"))
            .expect("construct pool-neutral raw set");
        assert!(neutral.active_needs_host());
        assert_eq!(
            neutral
                .query(Fraction::ZERO, Fraction::ONE)
                .expect("query pool-neutral raw set")
                .len(),
            1,
            "raw set changed canonical fmap density"
        );
        assert!(
            neutral.scheduler.refusal().is_none(),
            "raw set invented a raw-specific resource refusal"
        );

        // Existing work in the source graph is evaluated before the map and
        // retains its original typed operation/limit attribution.
        let mut nested = Session::new().expect("nested raw set refusal session");
        nested
            .evaluate(&format!("gap({over}).shrink(0)._set('changed')"))
            .expect("construct nested raw set refusal");
        assert_nested_shrink_refusal(&mut nested);
    }

    #[test]
    fn raw_keep_reaches_session_query_play_without_executing_ignored_graphs() {
        const SOURCE: &str = r#"
          (() => {
            const names = ['_set', 'set', '_keep', 'keep'];
            const order = Object.getOwnPropertyNames(Pattern.prototype)
              .filter(name => names.includes(name));
            const keys = Object.keys(Pattern.prototype)
              .filter(name => names.includes(name));
            const raw = Object.getOwnPropertyDescriptor(Pattern.prototype, '_keep');
            const publicKeep = Object.getOwnPropertyDescriptor(Pattern.prototype, 'keep');
            if (order.join(',') !== '_set,set,_keep,keep'
                || keys.join(',') !== '_set,_keep'
                || raw.writable !== true
                || raw.enumerable !== true
                || raw.configurable !== true
                || raw.value.name !== ''
                || raw.value.length !== 1
                || typeof publicKeep.get !== 'function'
                || publicKeep.enumerable !== false
                || publicKeep.configurable !== true
                || Object.hasOwn(globalThis, '_keep')
                || Object.hasOwn(rustelScope, '_keep')
                || typeof globalThis._keep !== 'undefined'
                || typeof rustelScope._keep !== 'undefined') {
              throw new Error('raw keep Session surface changed');
            }
            globalThis.rawKeepIgnoredQueries = 0;
            const ignored = new Pattern(() => {
              rawKeepIgnoredQueries++;
              throw new Error('raw keep queried its ignored graph');
            });
            return pure(1).setSteps(7)._keep(ignored);
          })()
        "#;
        let mut session = Session::new().expect("raw keep query session");
        session.evaluate(SOURCE).expect("evaluate Session raw keep");
        assert!(
            session.active_needs_host(),
            "raw keep's lexical fmap callback was granted host-free purity"
        );
        assert_eq!(
            session.js.active_pattern().expect("active raw keep").steps,
            Some(Fraction::int(7)),
            "raw keep lost source steps"
        );
        assert_eq!(
            session
                .query(Fraction::ZERO, Fraction::int(2))
                .expect("query Session raw keep")
                .into_iter()
                .map(|hap| hap.show())
                .collect::<Vec<_>>(),
            ["[ 0/1 → 1/1 | 1 ]", "[ 1/1 → 2/1 | 1 ]"],
            "Session raw keep query changed"
        );
        assert_eq!(
            session.js.get_number("rawKeepIgnoredQueries"),
            Some(0.0),
            "Session raw keep executed the captured ignored graph"
        );

        let mut play = Session::new().expect("raw keep play session");
        play.evaluate(SOURCE).expect("evaluate play raw keep");
        assert_eq!(
            play.play(4.0)
                .expect("play Session raw keep")
                .onsets
                .iter()
                .map(|onset| (onset.whole_begin.as_str(), onset.value_show.as_str()))
                .collect::<Vec<_>>(),
            [("0/1", "1"), ("1/1", "1"), ("2/1", "1")],
            "Session raw keep scheduler onsets changed"
        );
        assert_eq!(
            play.js.get_number("rawKeepIgnoredQueries"),
            Some(0.0),
            "play raw keep executed the captured ignored graph"
        );

        let over = rustel_core::MAX_STEPWISE_ENTRIES + 1;
        // `_keep` adds no shared-pool operation or charge. The ignored first
        // argument remains captured by the mapper, but its graph is not
        // queried and arbitrary custom-fmap/resource behavior is not claimed.
        let mut neutral = Session::new().expect("pool-neutral raw keep session");
        neutral
                .evaluate(&format!(
                    "(() => {{ globalThis.rawKeepNeutralIgnored = 0; const ignored = new Pattern(() => {{ rawKeepNeutralIgnored++; throw new Error('ignored'); }}); return pure('safe').setSteps({over})._keep(ignored); }})()"
                ))
                .expect("construct pool-neutral raw keep");
        assert!(neutral.active_needs_host());
        assert_eq!(
            neutral
                .query(Fraction::ZERO, Fraction::ONE)
                .expect("query pool-neutral raw keep")
                .into_iter()
                .map(|hap| hap.value.show())
                .collect::<Vec<_>>(),
            ["safe"],
            "raw keep changed canonical identity-map density/value"
        );
        assert_eq!(
            neutral.js.get_number("rawKeepNeutralIgnored"),
            Some(0.0),
            "pool-neutral raw keep queried its ignored graph"
        );
        assert!(
            neutral.scheduler.refusal().is_none(),
            "raw keep invented a raw-specific resource refusal"
        );

        // Existing work in the source graph is evaluated before the identity
        // map and retains its original typed operation/limit attribution.
        let mut nested = Session::new().expect("nested raw keep refusal session");
        nested
            .evaluate(&format!("gap({over}).shrink(0)._keep('ignored')"))
            .expect("construct nested raw keep refusal");
        assert_nested_shrink_refusal(&mut nested);
    }

    #[test]
    fn raw_keepif_reaches_session_query_play_and_preserves_source_attribution() {
        const FALSE_SOURCE: &str = r#"
          (() => {
            const names = ['_set', 'set', '_keep', 'keep', '_keepif', 'keepif'];
            const order = Object.getOwnPropertyNames(Pattern.prototype)
              .filter(name => names.includes(name));
            const keys = Object.keys(Pattern.prototype)
              .filter(name => names.includes(name));
            const raw = Object.getOwnPropertyDescriptor(Pattern.prototype, '_keepif');
            const publicKeepif = Object.getOwnPropertyDescriptor(
              Pattern.prototype, 'keepif'
            );
            if (order.join(',') !== '_set,set,_keep,keep,_keepif,keepif'
                || keys.join(',') !== '_set,_keep,_keepif'
                || raw.writable !== true
                || raw.enumerable !== true
                || raw.configurable !== true
                || raw.value.name !== ''
                || raw.value.length !== 1
                || typeof publicKeepif.get !== 'function'
                || publicKeepif.enumerable !== false
                || publicKeepif.configurable !== true
                || Object.hasOwn(globalThis, '_keepif')
                || Object.hasOwn(rustelScope, '_keepif')
                || typeof globalThis._keepif !== 'undefined'
                || typeof rustelScope._keepif !== 'undefined') {
              throw new Error('raw keepif Session surface changed');
            }
            return sequence(1, 2).setSteps(7)._keepif(false);
          })()
        "#;
        let mut session = Session::new().expect("raw keepif query session");
        session
            .evaluate(FALSE_SOURCE)
            .expect("evaluate Session raw keepif");
        assert!(
            session.active_needs_host(),
            "raw keepif's lexical fmap callback was granted host-free purity"
        );
        assert_eq!(
            session
                .js
                .active_pattern()
                .expect("active raw keepif")
                .steps,
            Some(Fraction::int(7)),
            "raw keepif lost source steps"
        );
        assert_eq!(
            session
                .query(Fraction::ZERO, Fraction::int(2))
                .expect("query Session raw keepif")
                .into_iter()
                .map(|hap| hap.show())
                .collect::<Vec<_>>(),
            [
                "[ 0/1 → 1/2 | undefined ]",
                "[ 1/2 → 1/1 | undefined ]",
                "[ 1/1 → 3/2 | undefined ]",
                "[ 3/2 → 2/1 | undefined ]",
            ],
            "Session raw keepif false route removed or retimed haps"
        );

        let mut play = Session::new().expect("raw keepif play session");
        play.evaluate(FALSE_SOURCE)
            .expect("evaluate play raw keepif");
        assert_eq!(
            play.play(4.0)
                .expect("play Session raw keepif")
                .onsets
                .iter()
                .map(|onset| (onset.whole_begin.as_str(), onset.value_show.as_str()))
                .collect::<Vec<_>>(),
            [
                ("0/1", "undefined"),
                ("1/2", "undefined"),
                ("1/1", "undefined"),
                ("3/2", "undefined"),
                ("2/1", "undefined"),
            ],
            "Session raw keepif scheduler false onsets changed"
        );

        // A Pattern-valued condition is truthy but never queried. This is
        // deliberately distinct from the open Pattern-valued source route.
        let mut control = Session::new().expect("raw keepif Pattern-control session");
        control
            .evaluate(
                r#"(() => {
                     globalThis.rawKeepifControlQueries = 0;
                     const condition = new Pattern(() => {
                       rawKeepifControlQueries++;
                       throw new Error('raw keepif queried its condition');
                     });
                     return sequence(1, 2)._keepif(condition);
                   })()"#,
            )
            .expect("construct Pattern-control raw keepif");
        assert_eq!(
            control
                .query(Fraction::ZERO, Fraction::ONE)
                .expect("query Pattern-control raw keepif")
                .into_iter()
                .map(|hap| hap.value.show())
                .collect::<Vec<_>>(),
            ["1", "2"],
            "truthy Pattern control changed raw keepif source values"
        );
        assert_eq!(
            control.js.get_number("rawKeepifControlQueries"),
            Some(0.0),
            "Session raw keepif queried its Pattern control"
        );

        let over = rustel_core::MAX_STEPWISE_ENTRIES + 1;
        // `_keepif` adds no shared-pool operation or charge. Oversized source
        // metadata alone stays successful, while the generated mapper remains
        // host-backed; this is not an arbitrary custom-fmap resource claim.
        let mut neutral = Session::new().expect("pool-neutral raw keepif session");
        neutral
            .evaluate(&format!("pure('safe').setSteps({over})._keepif(false)"))
            .expect("construct pool-neutral raw keepif");
        assert!(neutral.active_needs_host());
        assert_eq!(
            neutral
                .query(Fraction::ZERO, Fraction::ONE)
                .expect("query pool-neutral raw keepif")
                .into_iter()
                .map(|hap| hap.value.show())
                .collect::<Vec<_>>(),
            ["undefined"],
            "raw keepif false route changed canonical density"
        );
        assert!(
            neutral.scheduler.refusal().is_none(),
            "raw keepif invented a raw-specific resource refusal"
        );

        // Existing source work is evaluated before the mapper and retains its
        // typed operation/limit attribution even on the truthy route.
        let mut nested = Session::new().expect("nested raw keepif refusal session");
        nested
            .evaluate(&format!("gap({over}).shrink(0)._keepif(true)"))
            .expect("construct nested raw keepif refusal");
        assert_nested_shrink_refusal(&mut nested);
    }

    #[test]
    fn raw_eqt_reaches_session_query_play_and_preserves_source_attribution() {
        const SOURCE: &str = r#"
          (() => {
            const names = ['eq', '_eqt', 'eqt', 'ne', 'net'];
            const order = Object.getOwnPropertyNames(Pattern.prototype)
              .filter(name => names.includes(name));
            const keys = Object.keys(Pattern.prototype)
              .filter(name => names.includes(name));
            const raw = Object.getOwnPropertyDescriptor(Pattern.prototype, '_eqt');
            const publicEqt = Object.getOwnPropertyDescriptor(Pattern.prototype, 'eqt');
            if (order.join(',') !== 'eq,_eqt,eqt,ne,net'
                || keys.join(',') !== '_eqt'
                || raw.writable !== true
                || raw.enumerable !== true
                || raw.configurable !== true
                || raw.value.name !== ''
                || raw.value.length !== 1
                || typeof publicEqt.get !== 'function'
                || publicEqt.enumerable !== false
                || publicEqt.configurable !== true
                || Object.hasOwn(globalThis, '_eqt')
                || Object.hasOwn(rustelScope, '_eqt')
                || typeof globalThis._eqt !== 'undefined'
                || typeof rustelScope._eqt !== 'undefined') {
              throw new Error('raw eqt Session surface changed');
            }
            return sequence(1, 2).setSteps(7)._eqt(1);
          })()
        "#;
        let mut session = Session::new().expect("raw eqt query session");
        session.evaluate(SOURCE).expect("evaluate Session raw eqt");
        assert!(
            session.active_needs_host(),
            "raw eqt's lexical fmap callback was granted host-free purity"
        );
        assert_eq!(
            session.js.active_pattern().expect("active raw eqt").steps,
            Some(Fraction::int(7)),
            "raw eqt lost source steps"
        );
        assert_eq!(
            session
                .query(Fraction::ZERO, Fraction::int(2))
                .expect("query Session raw eqt")
                .into_iter()
                .map(|hap| hap.show())
                .collect::<Vec<_>>(),
            [
                "[ 0/1 → 1/2 | true ]",
                "[ 1/2 → 1/1 | false ]",
                "[ 1/1 → 3/2 | true ]",
                "[ 3/2 → 2/1 | false ]",
            ],
            "Session raw eqt boolean rhythm changed"
        );

        let mut play = Session::new().expect("raw eqt play session");
        play.evaluate(SOURCE).expect("evaluate play raw eqt");
        assert_eq!(
            play.play(4.0)
                .expect("play Session raw eqt")
                .onsets
                .iter()
                .map(|onset| (onset.whole_begin.as_str(), onset.value_show.as_str()))
                .collect::<Vec<_>>(),
            [
                ("0/1", "true"),
                ("1/2", "false"),
                ("1/1", "true"),
                ("3/2", "false"),
                ("2/1", "true"),
            ],
            "Session raw eqt scheduler onsets changed"
        );

        let over = rustel_core::MAX_STEPWISE_ENTRIES + 1;
        // `_eqt` adds no shared-pool operation or charge. Oversized declared
        // metadata alone remains successful, while the generated mapper is
        // still host-backed.
        let mut neutral = Session::new().expect("pool-neutral raw eqt session");
        neutral
            .evaluate(&format!("sequence(1, 2).setSteps({over})._eqt(1)"))
            .expect("construct pool-neutral raw eqt");
        assert!(neutral.active_needs_host());
        assert_eq!(
            neutral
                .query(Fraction::ZERO, Fraction::ONE)
                .expect("query pool-neutral raw eqt")
                .into_iter()
                .map(|hap| hap.value.show())
                .collect::<Vec<_>>(),
            ["true", "false"],
            "raw eqt changed canonical fmap density/value"
        );
        assert!(
            neutral.scheduler.refusal().is_none(),
            "raw eqt invented a raw-specific resource refusal"
        );

        // Existing bounded work in the source graph is evaluated before the
        // equality mapper and retains its original typed operation and limit.
        let mut nested = Session::new().expect("nested raw eqt refusal session");
        nested
            .evaluate(&format!("gap({over}).shrink(0)._eqt(0)"))
            .expect("construct nested raw eqt refusal");
        assert_nested_shrink_refusal(&mut nested);
    }

    #[test]
    fn raw_net_reaches_session_query_play_and_preserves_source_attribution() {
        const SOURCE: &str = r#"
          (() => {
            const names = ['ne', '_net', 'net', 'and'];
            const order = Object.getOwnPropertyNames(Pattern.prototype)
              .filter(name => names.includes(name));
            const keys = Object.keys(Pattern.prototype)
              .filter(name => names.includes(name));
            const raw = Object.getOwnPropertyDescriptor(Pattern.prototype, '_net');
            const publicNet = Object.getOwnPropertyDescriptor(Pattern.prototype, 'net');
            if (order.join(',') !== 'ne,_net,net,and'
                || keys.join(',') !== '_net'
                || raw.writable !== true
                || raw.enumerable !== true
                || raw.configurable !== true
                || raw.value.name !== ''
                || raw.value.length !== 1
                || typeof publicNet.get !== 'function'
                || publicNet.enumerable !== false
                || publicNet.configurable !== true
                || Object.hasOwn(globalThis, '_net')
                || Object.hasOwn(rustelScope, '_net')
                || Object.hasOwn(globalThis, 'net')
                || Object.hasOwn(rustelScope, 'net')
                || Object.hasOwn(Pattern.prototype, '_eq')
                || Object.hasOwn(Pattern.prototype, '_ne')) {
              throw new Error('raw net Session surface changed');
            }
            return sequence(1, 2).setSteps(7)._net(1);
          })()
        "#;
        let mut session = Session::new().expect("raw net query session");
        session.evaluate(SOURCE).expect("evaluate Session raw net");
        assert!(
            session.active_needs_host(),
            "raw net's lexical fmap callback was granted host-free purity"
        );
        assert_eq!(
            session.js.active_pattern().expect("active raw net").steps,
            Some(Fraction::int(7)),
            "raw net lost source steps"
        );
        assert_eq!(
            session
                .query(Fraction::ZERO, Fraction::int(2))
                .expect("query Session raw net")
                .into_iter()
                .map(|hap| hap.show())
                .collect::<Vec<_>>(),
            [
                "[ 0/1 → 1/2 | false ]",
                "[ 1/2 → 1/1 | true ]",
                "[ 1/1 → 3/2 | false ]",
                "[ 3/2 → 2/1 | true ]",
            ],
            "Session raw net boolean rhythm changed"
        );

        let mut play = Session::new().expect("raw net play session");
        play.evaluate(SOURCE).expect("evaluate play raw net");
        assert_eq!(
            play.play(4.0)
                .expect("play Session raw net")
                .onsets
                .iter()
                .map(|onset| (onset.whole_begin.as_str(), onset.value_show.as_str()))
                .collect::<Vec<_>>(),
            [
                ("0/1", "false"),
                ("1/2", "true"),
                ("1/1", "false"),
                ("3/2", "true"),
                ("2/1", "false"),
            ],
            "Session raw net scheduler onsets changed"
        );

        let over = rustel_core::MAX_STEPWISE_ENTRIES + 1;
        let mut neutral = Session::new().expect("pool-neutral raw net session");
        neutral
            .evaluate(&format!("sequence(1, 2).setSteps({over})._net(1)"))
            .expect("construct pool-neutral raw net");
        assert!(neutral.active_needs_host());
        assert_eq!(
            neutral
                .query(Fraction::ZERO, Fraction::ONE)
                .expect("query pool-neutral raw net")
                .into_iter()
                .map(|hap| hap.value.show())
                .collect::<Vec<_>>(),
            ["false", "true"],
            "raw net changed canonical fmap density/value"
        );
        assert!(
            neutral.scheduler.refusal().is_none(),
            "raw net invented a raw-specific resource refusal"
        );

        let mut nested = Session::new().expect("nested raw net refusal session");
        nested
            .evaluate(&format!("gap({over}).shrink(0)._net(0)"))
            .expect("construct nested raw net refusal");
        assert_nested_shrink_refusal(&mut nested);

        let mut mapper_throw = Session::new().expect("raw net mapper-throw session");
        mapper_throw
            .evaluate(
                r#"(() => {
                     globalThis.rawNetSessionMapperCalls = 0;
                     const originalFmap = Pattern.prototype.fmap;
                     Pattern.prototype.fmap = function (mapper) {
                       return Reflect.apply(originalFmap, this, [new Proxy(mapper, {
                         apply() {
                           rawNetSessionMapperCalls++;
                           throw new Error('raw net Session mapper throw');
                         },
                       })]);
                     };
                     const result = fastcat(pure(1), pure(1))._net(1);
                     Pattern.prototype.fmap = originalFmap;
                     return result;
                   })()"#,
            )
            .expect("construct Session mapper-throw residual");
        assert!(
            mapper_throw
                .query(Fraction::ZERO, Fraction::ONE)
                .expect("query Session mapper-throw residual")
                .is_empty(),
            "Session outer query retained undefined mapper-throw haps"
        );
        assert_eq!(
            mapper_throw.js.get_number("rawNetSessionMapperCalls"),
            Some(2.0),
            "Session outer query did not complete both native mapper calls"
        );
    }

    #[test]
    fn raw_and_reaches_session_query_play_and_preserves_source_attribution() {
        const SOURCE: &str = r#"
          (() => {
            const names = ['net', '_and', 'and', '_or', 'or'];
            const order = Object.getOwnPropertyNames(Pattern.prototype)
              .filter(name => names.includes(name));
            const keys = Object.keys(Pattern.prototype)
              .filter(name => names.includes(name));
            const raw = Object.getOwnPropertyDescriptor(Pattern.prototype, '_and');
            const publicAnd = Object.getOwnPropertyDescriptor(Pattern.prototype, 'and');
            if (order.join(',') !== 'net,_and,and,_or,or'
                || keys.join(',') !== '_and,_or'
                || raw.writable !== true
                || raw.enumerable !== true
                || raw.configurable !== true
                || raw.value.name !== ''
                || raw.value.length !== 1
                || typeof publicAnd.get !== 'function'
                || publicAnd.enumerable !== false
                || publicAnd.configurable !== true
                || Object.hasOwn(globalThis, '_and')
                || Object.hasOwn(rustelScope, '_and')
                || Object.hasOwn(globalThis, 'and')
                || Object.hasOwn(rustelScope, 'and')
                || !Object.hasOwn(Pattern.prototype, '_or')) {
              throw new Error('raw and Session surface changed');
            }
            return sequence(0, 1).setSteps(7)._and('right');
          })()
        "#;
        let mut session = Session::new().expect("raw and query session");
        session.evaluate(SOURCE).expect("evaluate Session raw and");
        assert!(
            session.active_needs_host(),
            "raw and's lexical fmap callback was granted host-free purity"
        );
        assert_eq!(
            session.js.active_pattern().expect("active raw and").steps,
            Some(Fraction::int(7)),
            "raw and lost source steps"
        );
        assert_eq!(
            session
                .query(Fraction::ZERO, Fraction::int(2))
                .expect("query Session raw and")
                .into_iter()
                .map(|hap| hap.show())
                .collect::<Vec<_>>(),
            [
                "[ 0/1 → 1/2 | 0 ]",
                "[ 1/2 → 1/1 | right ]",
                "[ 1/1 → 3/2 | 0 ]",
                "[ 3/2 → 2/1 | right ]",
            ],
            "Session raw and operand-selection rhythm changed"
        );

        let mut play = Session::new().expect("raw and play session");
        play.evaluate(SOURCE).expect("evaluate play raw and");
        assert_eq!(
            play.play(4.0)
                .expect("play Session raw and")
                .onsets
                .iter()
                .map(|onset| (onset.whole_begin.as_str(), onset.value_show.as_str()))
                .collect::<Vec<_>>(),
            [
                ("0/1", "0"),
                ("1/2", "right"),
                ("1/1", "0"),
                ("3/2", "right"),
                ("2/1", "0"),
            ],
            "Session raw and scheduler onsets changed"
        );

        let over = rustel_core::MAX_STEPWISE_ENTRIES + 1;
        let mut neutral = Session::new().expect("pool-neutral raw and session");
        neutral
            .evaluate(&format!("sequence(0, 1).setSteps({over})._and('right')"))
            .expect("construct pool-neutral raw and");
        assert!(neutral.active_needs_host());
        assert_eq!(
            neutral
                .query(Fraction::ZERO, Fraction::ONE)
                .expect("query pool-neutral raw and")
                .into_iter()
                .map(|hap| hap.value.show())
                .collect::<Vec<_>>(),
            ["0", "right"],
            "raw and changed canonical fmap density/operand value"
        );
        assert!(
            neutral.scheduler.refusal().is_none(),
            "raw and invented a raw-specific resource refusal"
        );

        let mut nested = Session::new().expect("nested raw and refusal session");
        nested
            .evaluate(&format!("gap({over}).shrink(0)._and('right')"))
            .expect("construct nested raw and refusal");
        assert_nested_shrink_refusal(&mut nested);

        let mut mapper_throw = Session::new().expect("raw and mapper-throw session");
        mapper_throw
            .evaluate(
                r#"(() => {
                     globalThis.rawAndSessionMapperCalls = 0;
                     const originalFmap = Pattern.prototype.fmap;
                     Pattern.prototype.fmap = function (mapper) {
                       return Reflect.apply(originalFmap, this, [new Proxy(mapper, {
                         apply() {
                           rawAndSessionMapperCalls++;
                           throw new Error('raw and Session mapper throw');
                         },
                       })]);
                     };
                     const result = fastcat(pure(1), pure(1))._and('right');
                     Pattern.prototype.fmap = originalFmap;
                     return result;
                   })()"#,
            )
            .expect("construct Session mapper-throw residual");
        assert!(
            mapper_throw
                .query(Fraction::ZERO, Fraction::ONE)
                .expect("query Session mapper-throw residual")
                .is_empty(),
            "Session outer query retained undefined mapper-throw haps"
        );
        assert_eq!(
            mapper_throw.js.get_number("rawAndSessionMapperCalls"),
            Some(2.0),
            "Session outer query did not complete both native mapper calls"
        );
    }

    #[test]
    fn raw_or_reaches_session_query_play_and_preserves_source_attribution() {
        const SOURCE: &str = r#"
          (() => {
            const names = ['and', '_or', 'or', '_func', 'func'];
            const order = Object.getOwnPropertyNames(Pattern.prototype)
              .filter(name => names.includes(name));
            const keys = Object.keys(Pattern.prototype)
              .filter(name => names.includes(name));
            const raw = Object.getOwnPropertyDescriptor(Pattern.prototype, '_or');
            const publicOr = Object.getOwnPropertyDescriptor(Pattern.prototype, 'or');
            if (order.join(',') !== 'and,_or,or'
                || keys.join(',') !== '_or'
                || raw.writable !== true
                || raw.enumerable !== true
                || raw.configurable !== true
                || raw.value.name !== ''
                || raw.value.length !== 1
                || typeof publicOr.get !== 'function'
                || publicOr.enumerable !== false
                || publicOr.configurable !== true
                || Object.hasOwn(globalThis, '_or')
                || Object.hasOwn(rustelScope, '_or')
                || Object.hasOwn(globalThis, 'or')
                || Object.hasOwn(rustelScope, 'or')
                || Object.hasOwn(Pattern.prototype, '_func')
                || Object.hasOwn(Pattern.prototype, 'func')) {
              throw new Error('raw or Session surface changed');
            }
            return sequence(0, 1).setSteps(7)._or('right');
          })()
        "#;
        let mut session = Session::new().expect("raw or query session");
        session.evaluate(SOURCE).expect("evaluate Session raw or");
        assert!(
            session.active_needs_host(),
            "raw or's lexical fmap callback was granted host-free purity"
        );
        assert_eq!(
            session.js.active_pattern().expect("active raw or").steps,
            Some(Fraction::int(7)),
            "raw or lost source steps"
        );
        assert_eq!(
            session
                .query(Fraction::ZERO, Fraction::int(2))
                .expect("query Session raw or")
                .into_iter()
                .map(|hap| hap.show())
                .collect::<Vec<_>>(),
            [
                "[ 0/1 → 1/2 | right ]",
                "[ 1/2 → 1/1 | 1 ]",
                "[ 1/1 → 3/2 | right ]",
                "[ 3/2 → 2/1 | 1 ]",
            ],
            "Session raw or operand-selection rhythm changed"
        );

        let mut play = Session::new().expect("raw or play session");
        play.evaluate(SOURCE).expect("evaluate play raw or");
        assert_eq!(
            play.play(4.0)
                .expect("play Session raw or")
                .onsets
                .iter()
                .map(|onset| (onset.whole_begin.as_str(), onset.value_show.as_str()))
                .collect::<Vec<_>>(),
            [
                ("0/1", "right"),
                ("1/2", "1"),
                ("1/1", "right"),
                ("3/2", "1"),
                ("2/1", "right"),
            ],
            "Session raw or scheduler onsets changed"
        );

        let over = rustel_core::MAX_STEPWISE_ENTRIES + 1;
        let mut neutral = Session::new().expect("pool-neutral raw or session");
        neutral
            .evaluate(&format!("sequence(0, 1).setSteps({over})._or('right')"))
            .expect("construct pool-neutral raw or");
        assert!(neutral.active_needs_host());
        assert_eq!(
            neutral
                .query(Fraction::ZERO, Fraction::ONE)
                .expect("query pool-neutral raw or")
                .into_iter()
                .map(|hap| hap.value.show())
                .collect::<Vec<_>>(),
            ["right", "1"],
            "raw or changed canonical fmap density/operand value"
        );
        assert!(
            neutral.scheduler.refusal().is_none(),
            "raw or invented a raw-specific resource refusal"
        );

        let mut nested = Session::new().expect("nested raw or refusal session");
        nested
            .evaluate(&format!("gap({over}).shrink(0)._or('right')"))
            .expect("construct nested raw or refusal");
        assert_nested_shrink_refusal(&mut nested);

        let mut mapper_throw = Session::new().expect("raw or mapper-throw session");
        mapper_throw
            .evaluate(
                r#"(() => {
                     globalThis.rawOrSessionMapperCalls = 0;
                     const originalFmap = Pattern.prototype.fmap;
                     Pattern.prototype.fmap = function (mapper) {
                       return Reflect.apply(originalFmap, this, [new Proxy(mapper, {
                         apply() {
                           rawOrSessionMapperCalls++;
                           throw new Error('raw or Session mapper throw');
                         },
                       })]);
                     };
                     const result = fastcat(pure(0), pure(0))._or('right');
                     Pattern.prototype.fmap = originalFmap;
                     return result;
                   })()"#,
            )
            .expect("construct Session mapper-throw residual");
        assert!(
            mapper_throw
                .query(Fraction::ZERO, Fraction::ONE)
                .expect("query Session mapper-throw residual")
                .is_empty(),
            "Session outer query retained undefined mapper-throw haps"
        );
        assert_eq!(
            mapper_throw.js.get_number("rawOrSessionMapperCalls"),
            Some(2.0),
            "Session outer query did not complete both native mapper calls"
        );
    }

    #[test]
    fn shrink_grow_keep_exact_step_join_windows_and_scheduler_onsets() {
        const SHRINK_JOINED: &[&str] = &[
            "[ 0/1 → 1/10 | 0 ]",
            "[ 1/10 → 1/5 | 1 ]",
            "[ 1/5 → 3/10 | 2 ]",
            "[ 3/10 → 2/5 | 3 ]",
            "[ 2/5 → 1/2 | 1 ]",
            "[ 1/2 → 3/5 | 2 ]",
            "[ 3/5 → 7/10 | 3 ]",
            "[ 7/10 → 4/5 | 2 ]",
            "[ 4/5 → 9/10 | 3 ]",
            "[ 9/10 → 1/1 | 3 ]",
            "[ 1/1 → 11/10 | 0 ]",
            "[ 11/10 → 6/5 | 1 ]",
            "[ 6/5 → 13/10 | 2 ]",
            "[ 13/10 → 7/5 | 3 ]",
            "[ 7/5 → 3/2 | 1 ]",
            "[ 3/2 → 8/5 | 2 ]",
            "[ 8/5 → 17/10 | 3 ]",
            "[ 17/10 → 9/5 | 2 ]",
            "[ 9/5 → 19/10 | 3 ]",
            "[ 19/10 → 2/1 | 3 ]",
        ];
        const SHRINK_SECOND: &[&str] = &[
            "[ 1/1 → 7/6 | 0 ]",
            "[ 7/6 → 4/3 | 1 ]",
            "[ 4/3 → 3/2 | 2 ]",
            "[ 3/2 → 5/3 | 3 ]",
            "[ 5/3 → 11/6 | 2 ]",
            "[ 11/6 → 2/1 | 3 ]",
        ];
        const GROW_JOINED: &[&str] = &[
            "[ 0/1 → 1/10 | 0 ]",
            "[ 1/10 → 1/5 | 0 ]",
            "[ 1/5 → 3/10 | 1 ]",
            "[ 3/10 → 2/5 | 0 ]",
            "[ 2/5 → 1/2 | 1 ]",
            "[ 1/2 → 3/5 | 2 ]",
            "[ 3/5 → 7/10 | 0 ]",
            "[ 7/10 → 4/5 | 1 ]",
            "[ 4/5 → 9/10 | 2 ]",
            "[ 9/10 → 1/1 | 3 ]",
            "[ 1/1 → 11/10 | 0 ]",
            "[ 11/10 → 6/5 | 0 ]",
            "[ 6/5 → 13/10 | 1 ]",
            "[ 13/10 → 7/5 | 0 ]",
            "[ 7/5 → 3/2 | 1 ]",
            "[ 3/2 → 8/5 | 2 ]",
            "[ 8/5 → 17/10 | 0 ]",
            "[ 17/10 → 9/5 | 1 ]",
            "[ 9/5 → 19/10 | 2 ]",
            "[ 19/10 → 2/1 | 3 ]",
        ];
        const GROW_SECOND: &[&str] = &[
            "[ 1/1 → 7/6 | 0 ]",
            "[ 7/6 → 4/3 | 1 ]",
            "[ 4/3 → 3/2 | 0 ]",
            "[ 3/2 → 5/3 | 1 ]",
            "[ 5/3 → 11/6 | 2 ]",
            "[ 11/6 → 2/1 | 3 ]",
        ];
        const SHRINK_ONSETS: &[(&str, &str)] = &[
            ("0/1", "0"),
            ("1/10", "1"),
            ("1/5", "2"),
            ("3/10", "3"),
            ("2/5", "1"),
            ("1/2", "2"),
            ("3/5", "3"),
            ("7/10", "2"),
            ("4/5", "3"),
            ("9/10", "3"),
            ("1/1", "0"),
            ("7/6", "1"),
            ("4/3", "2"),
            ("3/2", "3"),
            ("5/3", "2"),
            ("11/6", "3"),
            ("2/1", "0"),
        ];
        const GROW_ONSETS: &[(&str, &str)] = &[
            ("0/1", "0"),
            ("1/10", "0"),
            ("1/5", "1"),
            ("3/10", "0"),
            ("2/5", "1"),
            ("1/2", "2"),
            ("3/5", "0"),
            ("7/10", "1"),
            ("4/5", "2"),
            ("9/10", "3"),
            ("1/1", "0"),
            ("7/6", "1"),
            ("4/3", "0"),
            ("3/2", "1"),
            ("5/3", "2"),
            ("11/6", "3"),
            ("2/1", "0"),
        ];

        fn exercise(
            source: &str,
            joined: &[&str],
            second: &[&str],
            expected_onsets: &[(&str, &str)],
        ) {
            let mut session = Session::new().expect("shrink/grow session");
            session.evaluate(source).expect("evaluate shrink/grow");
            assert!(
                session.active_needs_host(),
                "patterned amount lost mutable query-time shrinklist dispatch"
            );
            assert_eq!(
                session
                    .js
                    .active_pattern()
                    .expect("active shrink/grow")
                    .steps,
                Some(Fraction::int(10)),
                "StepJoin metadata must come from cycle zero"
            );
            let shows = |begin, end| {
                session
                    .query(begin, end)
                    .expect("query shrink/grow")
                    .into_iter()
                    .map(|hap| hap.show())
                    .collect::<Vec<_>>()
            };
            assert_eq!(shows(Fraction::ZERO, Fraction::int(2)), joined);
            assert_eq!(shows(Fraction::ONE, Fraction::int(2)), second);
            assert_eq!(
                shows(Fraction::ONE, Fraction::int(2)),
                second,
                "repeated separate-window query changed"
            );

            let play = session.play(4.0).expect("schedule shrink/grow");
            assert_eq!(play.onsets.len(), expected_onsets.len());
            for (index, onset) in play.onsets.iter().enumerate() {
                let expected_target = if index < 10 {
                    index as f64 / 5.0
                } else {
                    (index - 4) as f64 / 3.0
                };
                let expected_duration = if index < 10 { 1.0 / 5.0 } else { 1.0 / 3.0 };
                assert_eq!(
                    onset.target_time, expected_target,
                    "scheduler changed shrink/grow onset {index}'s target time"
                );
                assert_eq!(
                    onset.duration_secs, expected_duration,
                    "scheduler changed shrink/grow onset {index}'s duration"
                );
            }
            let onsets = play
                .onsets
                .iter()
                .map(|onset| (onset.whole_begin.as_str(), onset.value_show.as_str()))
                .collect::<Vec<_>>();
            assert_eq!(
                onsets, expected_onsets,
                "scheduler changed per-window shrink/grow factors"
            );
            assert_eq!(session.js.query_depth(), 0, "query roots leaked");
        }

        let mut no_steps = Session::new().expect("no-step shrink session");
        no_steps
            .evaluate("new Pattern(state => pure('x').query(state)).shrink(1)")
            .expect("evaluate no-step shrink");
        assert!(!no_steps.active_needs_host());
        assert!(
            no_steps
                .query(Fraction::ZERO, Fraction::ONE)
                .expect("query no-step shrink")
                .is_empty()
        );

        exercise(
            "sequence(0, 1, 2, 3).shrink(slowcat(1, 2))",
            SHRINK_JOINED,
            SHRINK_SECOND,
            SHRINK_ONSETS,
        );
        exercise(
            "sequence(0, 1, 2, 3).grow(slowcat(1, 2))",
            GROW_JOINED,
            GROW_SECOND,
            GROW_ONSETS,
        );
    }

    #[test]
    fn canonical_pair_forms_reach_session_query_and_play_as_host_free_graphs() {
        const SHRINK_HAPS: &[&str] = &[
            "[ 0/1 → 1/7 | a ]",
            "[ 1/7 → 2/7 | b ]",
            "[ 2/7 → 3/7 | c ]",
            "[ 3/7 → 4/7 | d ]",
            "[ 4/7 → 5/7 | b ]",
            "[ 5/7 → 6/7 | c ]",
            "[ 6/7 → 1/1 | d ]",
        ];
        const GROW_HAPS: &[&str] = &[
            "[ 0/1 → 1/13 | a ]",
            "[ 1/13 → 2/13 | b ]",
            "[ (2/13 → 5/26) ⇝ 3/13 | c ]",
            "[ 5/26 → 7/26 | a ]",
            "[ 7/26 → 9/26 | b ]",
            "[ 9/26 → 11/26 | c ]",
            "[ 11/26 → 1/2 | a ]",
            "[ 1/2 → 15/26 | b ]",
            "[ 15/26 → 17/26 | c ]",
            "[ (17/26 → 9/13) ⇝ 19/26 | d ]",
            "[ 9/13 → 10/13 | a ]",
            "[ 10/13 → 11/13 | b ]",
            "[ 11/13 → 12/13 | c ]",
            "[ 12/13 → 1/1 | d ]",
        ];
        const SHRINK_ONSETS: &[(&str, &str)] = &[
            ("0/1", "a"),
            ("1/7", "b"),
            ("2/7", "c"),
            ("3/7", "d"),
            ("4/7", "b"),
            ("5/7", "c"),
            ("6/7", "d"),
            ("1/1", "a"),
        ];
        const GROW_ONSETS: &[(&str, &str)] = &[
            ("0/1", "a"),
            ("1/13", "b"),
            ("2/13", "c"),
            ("5/26", "a"),
            ("7/26", "b"),
            ("9/26", "c"),
            ("11/26", "a"),
            ("1/2", "b"),
            ("15/26", "c"),
            ("17/26", "d"),
            ("9/13", "a"),
            ("10/13", "b"),
            ("11/13", "c"),
            ("12/13", "d"),
            ("1/1", "a"),
        ];

        fn exercise(
            source: &str,
            expected: &[&str],
            steps: i128,
            expected_onsets: &[(&str, &str)],
        ) {
            let mut session = Session::new().expect("canonical pair session");
            session
                .evaluate(source)
                .unwrap_or_else(|error| panic!("evaluate {source}: {error}"));
            assert!(
                !session.active_needs_host(),
                "{source}: eager pair result unexpectedly retained the JS host"
            );
            assert_eq!(
                session
                    .js
                    .active_pattern()
                    .expect("active canonical pair")
                    .steps,
                Some(Fraction::int(steps)),
                "{source}: wrong pair step metadata"
            );
            assert_eq!(
                session
                    .query(Fraction::ZERO, Fraction::ONE)
                    .expect("query canonical pair")
                    .into_iter()
                    .map(|hap| hap.show())
                    .collect::<Vec<_>>(),
                expected,
                "{source}: wrong pair query timing"
            );

            let onsets = session
                .play(2.0)
                .expect("schedule canonical pair")
                .onsets
                .into_iter()
                .map(|onset| (onset.whole_begin, onset.value_show))
                .collect::<Vec<_>>();
            assert_eq!(
                onsets
                    .iter()
                    .map(|(whole, value)| (whole.as_str(), value.as_str()))
                    .collect::<Vec<_>>(),
                expected_onsets,
                "{source}: scheduler changed pair onsets"
            );
        }

        for source in [
            "sequence('a','b','c','d').shrink([1,2])",
            "shrink([1,2],sequence('a','b','c','d'))",
            "shrink([1,2])(sequence('a','b','c','d'))",
            "sequence('a','b','c','d').s_taper([1,2])",
            "s_taper([1,2],sequence('a','b','c','d'))",
            "s_taper([1,2])(sequence('a','b','c','d'))",
        ] {
            exercise(source, SHRINK_HAPS, 7, SHRINK_ONSETS);
        }
        for source in [
            "sequence('a','b','c','d').grow([1,2])",
            "grow([1,2],sequence('a','b','c','d'))",
            "grow([1,2])(sequence('a','b','c','d'))",
        ] {
            exercise(source, GROW_HAPS, 13, GROW_ONSETS);
        }
    }

    #[test]
    fn raw_shrink_grow_reach_session_query_play_and_atomic_resource_refusal() {
        const SHRINK_HAPS: &[&str] = &[
            "[ 0/1 → 1/7 | a ]",
            "[ 1/7 → 2/7 | b ]",
            "[ 2/7 → 3/7 | c ]",
            "[ 3/7 → 4/7 | d ]",
            "[ 4/7 → 5/7 | b ]",
            "[ 5/7 → 6/7 | c ]",
            "[ 6/7 → 1/1 | d ]",
        ];
        const GROW_HAPS: &[&str] = &[
            "[ 0/1 → 1/13 | a ]",
            "[ 1/13 → 2/13 | b ]",
            "[ (2/13 → 5/26) ⇝ 3/13 | c ]",
            "[ 5/26 → 7/26 | a ]",
            "[ 7/26 → 9/26 | b ]",
            "[ 9/26 → 11/26 | c ]",
            "[ 11/26 → 1/2 | a ]",
            "[ 1/2 → 15/26 | b ]",
            "[ 15/26 → 17/26 | c ]",
            "[ (17/26 → 9/13) ⇝ 19/26 | d ]",
            "[ 9/13 → 10/13 | a ]",
            "[ 10/13 → 11/13 | b ]",
            "[ 11/13 → 12/13 | c ]",
            "[ 12/13 → 1/1 | d ]",
        ];

        for (source, expected, steps, expected_onsets) in [
            (
                "sequence('a','b','c','d')._shrink([1,2])",
                SHRINK_HAPS,
                7,
                vec![
                    ("0/1", "a"),
                    ("1/7", "b"),
                    ("2/7", "c"),
                    ("3/7", "d"),
                    ("4/7", "b"),
                    ("5/7", "c"),
                    ("6/7", "d"),
                    ("1/1", "a"),
                ],
            ),
            (
                "sequence('a','b','c','d')._grow([1,2])",
                GROW_HAPS,
                13,
                vec![
                    ("0/1", "a"),
                    ("1/13", "b"),
                    ("2/13", "c"),
                    ("5/26", "a"),
                    ("7/26", "b"),
                    ("9/26", "c"),
                    ("11/26", "a"),
                    ("1/2", "b"),
                    ("15/26", "c"),
                    ("17/26", "d"),
                    ("9/13", "a"),
                    ("10/13", "b"),
                    ("11/13", "c"),
                    ("12/13", "d"),
                    ("1/1", "a"),
                ],
            ),
        ] {
            let mut session = Session::new().expect("raw pair session");
            session
                .evaluate(source)
                .unwrap_or_else(|error| panic!("evaluate {source}: {error}"));
            assert!(
                !session.active_needs_host(),
                "{source}: eager raw pair retained the JS host"
            );
            assert_eq!(
                session.js.active_pattern().expect("active raw pair").steps,
                Some(Fraction::int(steps))
            );
            assert_eq!(
                session
                    .query(Fraction::ZERO, Fraction::ONE)
                    .expect("query raw pair")
                    .into_iter()
                    .map(|hap| hap.show())
                    .collect::<Vec<_>>(),
                expected,
                "{source}: raw pair timing changed"
            );
            assert_eq!(
                session
                    .play(2.0)
                    .expect("schedule raw pair")
                    .onsets
                    .iter()
                    .map(|onset| (onset.whole_begin.as_str(), onset.value_show.as_str()))
                    .collect::<Vec<_>>(),
                expected_onsets,
                "{source}: raw scheduler onsets changed"
            );
        }

        let limit = rustel_core::MAX_STEPWISE_ENTRIES;
        let over = limit + 1;
        for name in ["_shrink", "_grow"] {
            let mut session = Session::new().expect("raw resource session");
            session
                .evaluate(&format!("gap({over}).{name}(0)"))
                .unwrap_or_else(|error| panic!("construct refused {name}: {error}"));
            assert!(
                !session.active_needs_host(),
                "{name}: resource graph retained the JS host"
            );
            let generation = session.generation();
            let queued = session.scheduler.queued();
            let horizon = session.scheduler.horizon_remaining(0.0);
            let error = session
                .schedule_at(0.0)
                .expect_err("raw MAX+1 expansion must refuse scheduling");
            let RuntimeError::ResourceLimit(message) = error else {
                panic!("{name}: raw refusal lost its type: {error:?}");
            };
            assert!(
                message.contains("shrink/grow")
                    && message.contains(&over.to_string())
                    && message.contains(&limit.to_string()),
                "{name}: wrong raw refusal: {message}"
            );
            assert!(
                matches!(
                    session.scheduler.refusal(),
                    Some(rustel_core::QueryLimit::StepwiseExpansion {
                        operation,
                        minimum_entries,
                        limit: refusal_limit,
                    }) if *operation == "shrink/grow"
                        && *minimum_entries == over
                        && *refusal_limit == limit
                ),
                "{name}: scheduler lost the structured raw refusal"
            );
            assert_eq!(session.scheduler.queued(), queued);
            assert_eq!(session.scheduler.horizon_remaining(0.0), horizon);
            assert_eq!(session.generation(), generation);

            session
                .evaluate("pure('raw-recovered')")
                .expect("install raw recovery score");
            let recovered = session
                .schedule_at(0.0)
                .expect("same-now recovery after raw refusal");
            assert_eq!(recovered.len(), 1);
            assert_eq!(recovered[0].onset_id, 0, "raw refusal consumed an onset id");
            assert_eq!(recovered[0].whole_begin, "0/1");
            assert_eq!(recovered[0].value_show, "raw-recovered");
        }
    }

    #[test]
    fn patterned_canonicals_keep_mutable_shrinklist_dispatch_in_session() {
        for name in ["shrink", "grow", "s_taper"] {
            let mut session = Session::new().expect("mutable canonical session");
            session
                .evaluate(&format!(
                    r#"
                      (() => {{
                        globalThis.sessionCanonicalFirstCalls = 0;
                        globalThis.sessionCanonicalSecondCalls = 0;
                        globalThis.sessionCanonicalReceiver =
                          sequence('a', 'b', 'c', 'd');
                        sessionCanonicalReceiver.shrinklist = function () {{
                          sessionCanonicalFirstCalls++;
                          return [this];
                        }};
                        globalThis.sessionCanonicalPattern =
                          sessionCanonicalReceiver.{name}(sequence(1, 2));
                        return sessionCanonicalPattern;
                      }})()
                    "#
                ))
                .unwrap_or_else(|error| panic!("construct mutable {name}: {error}"));
            assert!(
                session.active_needs_host(),
                "patterned {name} lost its mutable host dependency"
            );
            assert_eq!(
                session.js.get_number("sessionCanonicalFirstCalls"),
                Some(2.0),
                "patterned {name} changed its eager dispatch count"
            );
            assert_eq!(
                session
                    .js
                    .active_pattern()
                    .expect("active patterned canonical")
                    .steps,
                Some(Fraction::int(8)),
                "patterned {name} changed its eager cycle-zero metadata"
            );

            session
                .evaluate_prebake(
                    r#"
                      sessionCanonicalReceiver.shrinklist = function () {
                        sessionCanonicalSecondCalls++;
                        return [this];
                      };
                    "#,
                )
                .expect("replace canonical shrinklist after construction");
            session.js.run_gc();
            session.js.run_gc();
            let values = session
                .query(Fraction::ZERO, Fraction::ONE)
                .unwrap_or_else(|error| panic!("query mutable {name}: {error}"))
                .into_iter()
                .map(|hap| hap.value.show())
                .collect::<Vec<_>>();
            assert_eq!(values, ["a", "b", "c", "d", "a", "b", "c", "d"]);
            assert_eq!(
                session.js.get_number("sessionCanonicalSecondCalls"),
                Some(2.0),
                "patterned {name} did not dispatch once per factor hap"
            );
            assert_eq!(
                session
                    .js
                    .active_pattern()
                    .expect("active patterned canonical after mutation")
                    .steps,
                Some(Fraction::int(8)),
                "patterned {name} refreshed stale eager metadata"
            );

            session
                .evaluate_prebake("sessionCanonicalSecondCalls = 0;")
                .expect("reset mutable dispatch counter");
            let play = session
                .play(2.0)
                .unwrap_or_else(|error| panic!("schedule mutable {name}: {error}"));
            assert_eq!(
                play.onsets
                    .iter()
                    .map(|onset| onset.value_show.as_str())
                    .collect::<Vec<_>>(),
                ["a", "b", "c", "d", "a", "b", "c", "d", "a"],
                "Session play stopped using replacement shrinklist for {name}"
            );
            assert!(
                session
                    .js
                    .get_number("sessionCanonicalSecondCalls")
                    .is_some_and(|calls| calls > 0.0),
                "Session play did not enter replacement shrinklist for {name}"
            );
        }
    }

    #[test]
    fn shrink_grow_expansion_refusal_keeps_the_scheduler_atomic() {
        let limit = rustel_core::MAX_STEPWISE_SEGMENTS;
        let over = limit + 1;

        for operation in ["shrink", "grow"] {
            let mut session = Session::new().expect("stepwise expansion session");
            session
                .evaluate(&format!("gap({over}).{operation}(0)"))
                .unwrap_or_else(|error| panic!("construct refused {operation} graph: {error}"));
            assert!(
                !session.active_needs_host(),
                "resource-limit graph unexpectedly retained the JavaScript host"
            );

            let generation = session.generation();
            let queued = session.scheduler.queued();
            let horizon = session.scheduler.horizon_remaining(0.0);
            let error = session
                .schedule_at(0.0)
                .expect_err("MAX+1 expansion must refuse the scheduler tick");
            let RuntimeError::ResourceLimit(message) = error else {
                panic!("{operation} MAX+1 lost its typed refusal: {error:?}");
            };
            assert!(
                message.contains("shrink/grow")
                    && message.contains(&over.to_string())
                    && message.contains(&limit.to_string()),
                "wrong {operation} expansion refusal: {message}"
            );
            assert!(
                matches!(
                    session.scheduler.refusal(),
                    Some(rustel_core::QueryLimit::StepwiseExpansion {
                        operation: refusal_operation,
                        minimum_entries,
                        limit: refusal_limit,
                    }) if *refusal_operation == "shrink/grow"
                        && *minimum_entries == over
                        && *refusal_limit == limit
                ),
                "scheduler did not retain the structured {operation} refusal: {:?}",
                session.scheduler.refusal()
            );
            assert_eq!(
                session.scheduler.queued(),
                queued,
                "refused {operation} tick changed the event queue"
            );
            assert_eq!(
                session.scheduler.horizon_remaining(0.0),
                horizon,
                "refused {operation} tick advanced the query cursor"
            );
            assert_eq!(
                session.generation(),
                generation,
                "refused {operation} tick changed the score generation"
            );

            session
                .evaluate("pure('recovered')")
                .expect("install recovery score");
            let recovered = session
                .schedule_at(0.0)
                .expect("same-now recovery after expansion refusal");
            assert_eq!(recovered.len(), 1);
            assert_eq!(recovered[0].onset_id, 0, "refusal consumed an onset id");
            assert_eq!(recovered[0].whole_begin, "0/1");
            assert_eq!(recovered[0].value_show, "recovered");
        }
    }

    #[test]
    fn canonical_pair_and_override_expansion_boundaries_are_typed_and_atomic() {
        let limit = rustel_core::MAX_STEPWISE_SEGMENTS;
        assert_eq!(limit, rustel_core::MAX_STEPWISE_ENTRIES);
        assert_eq!(limit, 16_384);
        let over = limit + 1;

        fn assert_atomic_refusal(
            source: &str,
            operation: &str,
            limit: u64,
            over: u64,
            inspect_custom_phases: bool,
        ) {
            let mut session = Session::new().expect("canonical boundary session");
            session
                .evaluate(source)
                .unwrap_or_else(|error| panic!("construct refused {operation}: {error}"));
            assert!(
                !session.active_needs_host(),
                "{operation}: refusal graph retained the JavaScript host"
            );
            if inspect_custom_phases {
                assert_eq!(
                    session.js.get_number("canonicalBoundaryReverseCalls"),
                    Some(0.0)
                );
                assert_eq!(
                    session.js.get_number("canonicalBoundaryIteratorCalls"),
                    Some(0.0)
                );
                assert_eq!(
                    session.js.get_number("canonicalBoundaryReduceCalls"),
                    Some(0.0)
                );
            }

            let generation = session.generation();
            let queued = session.scheduler.queued();
            let horizon = session.scheduler.horizon_remaining(0.0);
            let error = session
                .schedule_at(0.0)
                .expect_err("MAX+1 canonical expansion must refuse scheduling");
            let RuntimeError::ResourceLimit(message) = error else {
                panic!("{operation}: canonical refusal lost its type: {error:?}");
            };
            assert!(
                message.contains("shrink/grow")
                    && message.contains(&over.to_string())
                    && message.contains(&limit.to_string()),
                "{operation}: wrong canonical refusal: {message}"
            );
            assert!(
                matches!(
                    session.scheduler.refusal(),
                    Some(rustel_core::QueryLimit::StepwiseExpansion {
                        operation: refusal_operation,
                        minimum_entries,
                        limit: refusal_limit,
                    }) if *refusal_operation == "shrink/grow"
                        && *minimum_entries == over
                        && *refusal_limit == limit
                ),
                "{operation}: scheduler lost structured canonical refusal: {:?}",
                session.scheduler.refusal()
            );
            assert_eq!(session.scheduler.queued(), queued);
            assert_eq!(session.scheduler.horizon_remaining(0.0), horizon);
            assert_eq!(session.generation(), generation);
            if inspect_custom_phases {
                assert_eq!(
                    session.js.get_number("canonicalBoundaryReverseCalls"),
                    Some(0.0)
                );
                assert_eq!(
                    session.js.get_number("canonicalBoundaryIteratorCalls"),
                    Some(0.0)
                );
                assert_eq!(
                    session.js.get_number("canonicalBoundaryReduceCalls"),
                    Some(0.0)
                );
            }

            session
                .evaluate("pure('canonical-recovered')")
                .expect("install canonical boundary recovery score");
            let recovered = session
                .schedule_at(0.0)
                .expect("same-now recovery after canonical refusal");
            assert_eq!(recovered.len(), 1);
            assert_eq!(recovered[0].onset_id, 0, "refusal consumed an onset id");
            assert_eq!(recovered[0].whole_begin, "0/1");
            assert_eq!(recovered[0].value_show, "canonical-recovered");
        }

        for operation in ["shrink", "grow"] {
            // A modest accepted control reaches the default helper handoff in
            // the Session product without repeating the exact-limit boundary
            // case under this route's fixed construction deadline.
            let mut accepted = Session::new().expect("accepted canonical pair session");
            accepted
                .evaluate(&format!("gap(128).{operation}([0, 128])"))
                .unwrap_or_else(|error| panic!("construct accepted pair {operation}: {error}"));
            assert!(!accepted.active_needs_host());
            assert!(
                accepted
                    .query(Fraction::ZERO, Fraction::ONE)
                    .unwrap_or_else(|error| panic!("query accepted pair {operation}: {error}"))
                    .is_empty(),
                "accepted pair {operation} emitted from a silent receiver"
            );

            let default_over = format!("gap({over}).{operation}([0, {over}])");
            assert_atomic_refusal(&default_over, operation, limit, over, false);

            // A matching accepted override proves the canonical body charges
            // an ordinary returned Array itself; MAX+1 below pins the product
            // boundary and atomic recovery.
            let mut custom_accepted = Session::new().expect("accepted override session");
            custom_accepted
                .evaluate(&format!(
                    r#"
                      (() => {{
                        globalThis.canonicalAcceptedReverseCalls = 0;
                        const receiver = gap(1);
                        receiver.shrinklist = function () {{
                          const list = Array(128).fill(this);
                          list.reverse = function () {{
                            canonicalAcceptedReverseCalls++;
                            return Array.prototype.reverse.call(this);
                          }};
                          return list;
                        }};
                        return receiver.{operation}(1);
                      }})()
                    "#
                ))
                .unwrap_or_else(|error| panic!("construct accepted override {operation}: {error}"));
            assert!(!custom_accepted.active_needs_host());
            assert_eq!(
                custom_accepted
                    .js
                    .get_number("canonicalAcceptedReverseCalls"),
                Some(if operation == "grow" { 1.0 } else { 0.0 }),
                "{operation}: accepted Array reverse phase changed"
            );
            assert!(
                custom_accepted
                    .query(Fraction::ZERO, Fraction::ONE)
                    .unwrap_or_else(|error| {
                        panic!("query accepted override {operation}: {error}")
                    })
                    .is_empty()
            );

            // Native safety intentionally refuses an oversized ordinary Array
            // before grow's reverse and before either canonical spreads or
            // reduces/reifies it. Proxy/accessor observation order is a
            // separate residual and is deliberately not exercised here.
            let custom_over = format!(
                r#"
                  (() => {{
                    globalThis.canonicalBoundaryReverseCalls = 0;
                    globalThis.canonicalBoundaryIteratorCalls = 0;
                    globalThis.canonicalBoundaryReduceCalls = 0;
                    const receiver = gap(1);
                    receiver.shrinklist = function () {{
                      const list = Array({over}).fill(this);
                      list.reverse = function () {{
                        canonicalBoundaryReverseCalls++;
                        return Array.prototype.reverse.call(this);
                      }};
                      list[Symbol.iterator] = function () {{
                        canonicalBoundaryIteratorCalls++;
                        return Array.prototype[Symbol.iterator].call(this);
                      }};
                      list.reduce = function (...args) {{
                        canonicalBoundaryReduceCalls++;
                        return Array.prototype.reduce.apply(this, args);
                      }};
                      return list;
                    }};
                    return receiver.{operation}(1);
                  }})()
                "#
            );
            assert_atomic_refusal(&custom_over, operation, limit, over, true);
        }
    }

    #[test]
    fn tour_keeps_exact_order_steps_and_scheduler_onsets() {
        let mut session = Session::new().expect("tour session");
        session
            .evaluate("pure('x').tour(pure('a'), pure('b'))")
            .expect("evaluate tour");
        assert!(
            !session.active_needs_host(),
            "a native tour graph was pushed onto the JavaScript scheduler path"
        );
        assert_eq!(
            session.js.active_pattern().expect("active tour").steps,
            Some(Fraction::int(9))
        );
        assert_eq!(
            session
                .query(Fraction::ZERO, Fraction::ONE)
                .expect("query tour")
                .into_iter()
                .map(|hap| hap.show())
                .collect::<Vec<_>>(),
            [
                "[ 0/1 → 1/9 | a ]",
                "[ 1/9 → 2/9 | b ]",
                "[ 2/9 → 1/3 | x ]",
                "[ 1/3 → 4/9 | a ]",
                "[ 4/9 → 5/9 | x ]",
                "[ 5/9 → 2/3 | b ]",
                "[ 2/3 → 7/9 | x ]",
                "[ 7/9 → 8/9 | a ]",
                "[ 8/9 → 1/1 | b ]",
            ]
        );

        let onsets = session
            .play(2.0)
            .expect("schedule tour")
            .onsets
            .into_iter()
            .map(|onset| (onset.whole_begin, onset.value_show))
            .collect::<Vec<_>>();
        assert_eq!(
            onsets,
            [
                ("0/1".to_owned(), "a".to_owned()),
                ("1/9".to_owned(), "b".to_owned()),
                ("2/9".to_owned(), "x".to_owned()),
                ("1/3".to_owned(), "a".to_owned()),
                ("4/9".to_owned(), "x".to_owned()),
                ("5/9".to_owned(), "b".to_owned()),
                ("2/3".to_owned(), "x".to_owned()),
                ("7/9".to_owned(), "a".to_owned()),
                ("8/9".to_owned(), "b".to_owned()),
                // The scheduler intentionally drains an onset exactly on the
                // requested duration boundary.
                ("1/1".to_owned(), "a".to_owned()),
            ]
        );
    }

    #[test]
    fn stepalt_and_s_alt_keep_lcm_order_steps_and_scheduler_onsets() {
        const CANONICAL: &str = "stepalt(\
          [pure('a'), pure('b')], [pure('c'), pure('d'), pure('e')])";
        const ALIAS: &str = "s_alt(\
          [pure('a'), pure('b')], [pure('c'), pure('d'), pure('e')])";
        const HAPS: &[&str] = &[
            "[ 0/1 → 1/12 | a ]",
            "[ 1/12 → 1/6 | c ]",
            "[ 1/6 → 1/4 | b ]",
            "[ 1/4 → 1/3 | d ]",
            "[ 1/3 → 5/12 | a ]",
            "[ 5/12 → 1/2 | e ]",
            "[ 1/2 → 7/12 | b ]",
            "[ 7/12 → 2/3 | c ]",
            "[ 2/3 → 3/4 | a ]",
            "[ 3/4 → 5/6 | d ]",
            "[ 5/6 → 11/12 | b ]",
            "[ 11/12 → 1/1 | e ]",
        ];

        let mut session = Session::new().expect("stepalt session");
        session
            .evaluate(CANONICAL)
            .expect("evaluate canonical stepalt");
        assert!(
            !session.active_needs_host(),
            "a pure stepalt graph was pushed onto the JavaScript scheduler path"
        );
        assert_eq!(
            session.js.active_pattern().expect("active stepalt").steps,
            Some(Fraction::int(12))
        );
        assert_eq!(
            session
                .query(Fraction::ZERO, Fraction::ONE)
                .expect("query canonical stepalt")
                .into_iter()
                .map(|hap| hap.show())
                .collect::<Vec<_>>(),
            HAPS
        );

        let onsets = session
            .play(2.0)
            .expect("schedule canonical stepalt")
            .onsets
            .into_iter()
            .map(|onset| (onset.whole_begin, onset.value_show))
            .collect::<Vec<_>>();
        assert_eq!(
            onsets,
            [
                ("0/1".to_owned(), "a".to_owned()),
                ("1/12".to_owned(), "c".to_owned()),
                ("1/6".to_owned(), "b".to_owned()),
                ("1/4".to_owned(), "d".to_owned()),
                ("1/3".to_owned(), "a".to_owned()),
                ("5/12".to_owned(), "e".to_owned()),
                ("1/2".to_owned(), "b".to_owned()),
                ("7/12".to_owned(), "c".to_owned()),
                ("2/3".to_owned(), "a".to_owned()),
                ("3/4".to_owned(), "d".to_owned()),
                ("5/6".to_owned(), "b".to_owned()),
                ("11/12".to_owned(), "e".to_owned()),
                // The scheduler drains an onset exactly on the duration edge.
                ("1/1".to_owned(), "a".to_owned()),
            ]
        );

        session.evaluate(ALIAS).expect("evaluate copied s_alt");
        assert_eq!(
            session
                .query(Fraction::ZERO, Fraction::ONE)
                .expect("query copied s_alt")
                .into_iter()
                .map(|hap| hap.show())
                .collect::<Vec<_>>(),
            HAPS,
            "s_alt changed canonical stepalt product timing"
        );
    }

    #[test]
    fn polymeter_aliases_keep_modern_legacy_steps_and_scheduler_onsets() {
        const MODERN: &str = "polymeter(sequence('a', 'b'), sequence('x', 'y', 'z'))";
        const MODERN_HAPS: &[&str] = &[
            "[ 0/1 → 1/6 | a ]",
            "[ 0/1 → 1/6 | x ]",
            "[ 1/6 → 1/3 | b ]",
            "[ 1/6 → 1/3 | y ]",
            "[ 1/3 → 1/2 | a ]",
            "[ 1/3 → 1/2 | z ]",
            "[ 1/2 → 2/3 | b ]",
            "[ 1/2 → 2/3 | x ]",
            "[ 2/3 → 5/6 | a ]",
            "[ 2/3 → 5/6 | y ]",
            "[ 5/6 → 1/1 | b ]",
            "[ 5/6 → 1/1 | z ]",
        ];

        let mut session = Session::new().expect("polymeter session");
        session
            .evaluate(MODERN)
            .expect("evaluate canonical modern polymeter");
        assert!(
            !session.active_needs_host(),
            "a pure polymeter graph entered the JavaScript scheduler path"
        );
        assert_eq!(
            session
                .js
                .active_pattern()
                .expect("active modern polymeter")
                .steps,
            Some(Fraction::int(6))
        );
        assert_eq!(
            session
                .query(Fraction::ZERO, Fraction::ONE)
                .expect("query canonical modern polymeter")
                .into_iter()
                .map(|hap| hap.show())
                .collect::<Vec<_>>(),
            MODERN_HAPS
        );
        assert_eq!(
            session
                .play(2.0)
                .expect("schedule canonical modern polymeter")
                .onsets
                .into_iter()
                .map(|onset| (onset.whole_begin, onset.value_show))
                .collect::<Vec<_>>(),
            [
                ("0/1".to_owned(), "a".to_owned()),
                ("0/1".to_owned(), "x".to_owned()),
                ("1/6".to_owned(), "b".to_owned()),
                ("1/6".to_owned(), "y".to_owned()),
                ("1/3".to_owned(), "a".to_owned()),
                ("1/3".to_owned(), "z".to_owned()),
                ("1/2".to_owned(), "b".to_owned()),
                ("1/2".to_owned(), "x".to_owned()),
                ("2/3".to_owned(), "a".to_owned()),
                ("2/3".to_owned(), "y".to_owned()),
                ("5/6".to_owned(), "b".to_owned()),
                ("5/6".to_owned(), "z".to_owned()),
                ("1/1".to_owned(), "a".to_owned()),
                ("1/1".to_owned(), "x".to_owned()),
            ]
        );

        session
            .evaluate("pm(sequence('a', 'b'), sequence('x', 'y', 'z'))")
            .expect("evaluate pm modern alias");
        assert_eq!(
            session
                .query(Fraction::ZERO, Fraction::ONE)
                .expect("query pm modern alias")
                .into_iter()
                .map(|hap| hap.show())
                .collect::<Vec<_>>(),
            MODERN_HAPS,
            "pm changed modern polymeter timing"
        );

        session
            .evaluate("s_polymeter(['a', 'b'], ['x', 'y', 'z'])")
            .expect("evaluate legacy polymeter alias");
        assert_eq!(
            session
                .js
                .active_pattern()
                .expect("active legacy polymeter")
                .steps,
            Some(Fraction::int(6)),
            "legacy metadata must remain the source-lane LCM"
        );
        assert_eq!(
            session
                .query(Fraction::ZERO, Fraction::ONE)
                .expect("query legacy polymeter alias")
                .into_iter()
                .map(|hap| hap.show())
                .collect::<Vec<_>>(),
            [
                "[ 0/1 → 1/2 | a ]",
                "[ 0/1 → 1/2 | x ]",
                "[ 1/2 → 1/1 | b ]",
                "[ 1/2 → 1/1 | y ]",
            ],
            "the legacy branch used modern LCM pacing"
        );
        assert_eq!(session.js.query_depth(), 0, "polymeter query roots leaked");
    }

    #[test]
    fn stepalt_saved_alias_survives_gc_and_destination_poisoning() {
        let mut session = Session::new().expect("stepalt snapshot session");
        session.js.install_gc_binding().expect("gc binding");
        session
            .evaluate_prebake(
                r#"
                globalThis.savedStepalt = Object.freeze({
                  canonical: rustelScope.stepalt,
                  alias: rustelScope.s_alt
                });
                savedStepalt.canonical.__sharedStepaltMarker = 61;
                if (savedStepalt.canonical !== savedStepalt.alias
                    || savedStepalt.alias.__sharedStepaltMarker !== 61) {
                  throw new Error('initial stepalt alias identity');
                }
                globalThis.stepaltCanonicalGlobalDecoy =
                  function stepaltCanonicalGlobalDecoy() {};
                globalThis.stepaltCanonicalScopeDecoy =
                  function stepaltCanonicalScopeDecoy() {};
                globalThis.stepaltAliasGlobalDecoy =
                  function stepaltAliasGlobalDecoy() {};
                globalThis.stepaltAliasScopeDecoy =
                  function stepaltAliasScopeDecoy() {};
                globalThis.stepaltMethodDecoy = function stepaltMethodDecoy() {};
                globalThis.stepalt = stepaltCanonicalGlobalDecoy;
                rustelScope.stepalt = stepaltCanonicalScopeDecoy;
                globalThis.s_alt = stepaltAliasGlobalDecoy;
                rustelScope.s_alt = stepaltAliasScopeDecoy;
                Pattern.prototype.stepalt = stepaltMethodDecoy;
                Pattern.prototype.s_alt = stepaltMethodDecoy;
                globalThis.assertStepaltPoison = () => {
                  if (globalThis.stepalt !== stepaltCanonicalGlobalDecoy
                      || rustelScope.stepalt !== stepaltCanonicalScopeDecoy
                      || globalThis.s_alt !== stepaltAliasGlobalDecoy
                      || rustelScope.s_alt !== stepaltAliasScopeDecoy
                      || Pattern.prototype.stepalt !== stepaltMethodDecoy
                      || Pattern.prototype.s_alt !== stepaltMethodDecoy) {
                    throw new Error('stepalt destinations were reinjected');
                  }
                };
                "#,
            )
            .expect("save and poison stepalt destinations");
        session.js.run_gc();
        session.js.run_gc();
        session
            .evaluate("assertStepaltPoison(); pure('successful-turn')")
            .expect("successful turn after stepalt poisoning");
        let failure = session
            .evaluate(
                "assertStepaltPoison(); globalThis.stepaltFailedTurn = 1; \
                 setcps(0); pure('failed-turn')",
            )
            .expect_err("invalid tempo must fail after the poison assertion");
        assert_eq!(failure.kind(), "evaluation");
        assert_eq!(session.js.get_number("stepaltFailedTurn"), Some(1.0));

        session
            .evaluate(
                r#"
                (() => {
                  assertStepaltPoison();
                  return savedStepalt.alias(
                    [pure('a'), pure('b')],
                    [pure('c'), pure('d'), pure('e')]
                  );
                })()
                "#,
            )
            .expect("call saved s_alt after GC and later turns");
        assert_eq!(
            session
                .query(Fraction::ZERO, Fraction::ONE)
                .expect("query saved s_alt")
                .into_iter()
                .map(|hap| hap.value.show())
                .collect::<Vec<_>>(),
            ["a", "c", "b", "d", "a", "e", "b", "c", "a", "d", "b", "e"]
        );
        assert_eq!(session.js.query_depth(), 0, "stepalt product roots leaked");
    }

    #[test]
    fn zip_and_s_zip_keep_lcm_order_steps_and_scheduler_onsets() {
        const CANONICAL: &str = "zip(sequence('a0', 'a1'), sequence('b0', 'b1', 'b2'))";
        const ALIAS: &str = "s_zip(sequence('a0', 'a1'), sequence('b0', 'b1', 'b2'))";
        const HAPS: &[&str] = &[
            "[ 0/1 → 1/6 | a0 ]",
            "[ 1/6 → 1/3 | b0 ]",
            "[ 1/3 → 1/2 | a1 ]",
            "[ 1/2 → 2/3 | b1 ]",
            "[ 2/3 → 5/6 | a0 ]",
            "[ 5/6 → 1/1 | b2 ]",
        ];

        let mut session = Session::new().expect("zip session");
        session.evaluate(CANONICAL).expect("evaluate canonical zip");
        assert!(
            !session.active_needs_host(),
            "a native zip graph was pushed onto the JavaScript scheduler path"
        );
        assert_eq!(
            session.js.active_pattern().expect("active zip").steps,
            Some(Fraction::int(6))
        );
        assert_eq!(
            session
                .query(Fraction::ZERO, Fraction::ONE)
                .expect("query canonical zip")
                .into_iter()
                .map(|hap| hap.show())
                .collect::<Vec<_>>(),
            HAPS
        );

        let onsets = session
            .play(2.0)
            .expect("schedule canonical zip")
            .onsets
            .into_iter()
            .map(|onset| (onset.whole_begin, onset.value_show))
            .collect::<Vec<_>>();
        assert_eq!(
            onsets,
            [
                ("0/1".to_owned(), "a0".to_owned()),
                ("1/6".to_owned(), "b0".to_owned()),
                ("1/3".to_owned(), "a1".to_owned()),
                ("1/2".to_owned(), "b1".to_owned()),
                ("2/3".to_owned(), "a0".to_owned()),
                ("5/6".to_owned(), "b2".to_owned()),
                // The scheduler intentionally drains an onset exactly on the
                // requested duration boundary.
                ("1/1".to_owned(), "a1".to_owned()),
            ]
        );

        session.evaluate(ALIAS).expect("evaluate copied s_zip");
        assert!(
            !session.active_needs_host(),
            "the copied free zip alias changed graph purity"
        );
        assert_eq!(
            session
                .query(Fraction::ZERO, Fraction::ONE)
                .expect("query copied s_zip")
                .into_iter()
                .map(|hap| hap.show())
                .collect::<Vec<_>>(),
            HAPS,
            "s_zip changed the canonical free function's product timing"
        );
    }

    #[test]
    fn shrinklist_growlist_and_s_taperlist_reach_session_query_play_after_gc() {
        const SHRINK_HAPS: &[&str] = &[
            "[ 0/1 → 1/9 | a ]",
            "[ 1/9 → 2/9 | b ]",
            "[ 2/9 → 1/3 | c ]",
            "[ 1/3 → 4/9 | d ]",
            "[ 4/9 → 5/9 | b ]",
            "[ 5/9 → 2/3 | c ]",
            "[ 2/3 → 7/9 | d ]",
            "[ 7/9 → 8/9 | c ]",
            "[ 8/9 → 1/1 | d ]",
        ];
        const GROW_VALUES: &[&str] = &["c", "d", "b", "c", "d", "a", "b", "c", "d"];

        let mut session = Session::new().expect("list-helper session");
        session.js.install_gc_binding().expect("gc binding");
        session
            .evaluate_prebake(
                r#"
                if (s_taperlist !== shrinklist
                    || Pattern.prototype.s_taperlist
                        !== Pattern.prototype.shrinklist) {
                  throw new Error('s_taperlist identity');
                }
                const savedShrinklist = shrinklist;
                const savedGrowlist = growlist;
                const savedTaperlist = s_taperlist;
                const dispatch = [];
                const receiver = {
                  shrinklist(value) { dispatch.push(`shrink:${value}`); return ['S']; },
                  growlist(value) { dispatch.push(`grow:${value}`); return ['G']; }
                };
                if (savedShrinklist(2, receiver)[0] !== 'S'
                    || savedTaperlist(3, receiver)[0] !== 'S'
                    || savedGrowlist(4, receiver)[0] !== 'G') {
                  throw new Error('free helper dispatch');
                }
                const reversed = ['left', 'right'];
                let reverseReceiver;
                reversed.reverse = function () {
                  reverseReceiver = this;
                  return Array.prototype.reverse.call(this);
                };
                const protoGrow = Pattern.prototype.growlist.call({
                  shrinklist(value) {
                    dispatch.push(`proto:${value}`);
                    return reversed;
                  }
                }, 5);
                if (dispatch.join(',') !== 'shrink:2,shrink:3,grow:4,proto:5'
                    || protoGrow !== reversed || reverseReceiver !== reversed
                    || protoGrow.join(',') !== 'right,left') {
                  throw new Error('dynamic helper dispatch');
                }

                globalThis.productListHelpers = Object.freeze({
                  shrink: savedShrinklist,
                  grow: savedGrowlist,
                  alias: savedTaperlist,
                  aliasMethod: Pattern.prototype.s_taperlist
                });
                globalThis.productListBase = sequence('a', 'b', 'c', 'd');
                globalThis.productListGlobalDecoy = function poisonedListGlobal() {};
                globalThis.productListScopeDecoy = function poisonedListScope() {};
                for (const name of ['shrinklist', 'growlist', 's_taperlist']) {
                  globalThis[name] = productListGlobalDecoy;
                  rustelScope[name] = productListScopeDecoy;
                }
                globalThis.assertProductListPoison = () => {
                  for (const name of ['shrinklist', 'growlist', 's_taperlist']) {
                    if (globalThis[name] !== productListGlobalDecoy
                        || rustelScope[name] !== productListScopeDecoy) {
                      throw new Error(`list helper destination reinjected: ${name}`);
                    }
                  }
                };
                "#,
            )
            .expect("retain and poison list helpers");
        session.js.run_gc();
        session.js.run_gc();

        let query_values = |session: &Session| {
            session
                .query(Fraction::ZERO, Fraction::ONE)
                .expect("query retained list helper")
                .into_iter()
                .map(|hap| hap.value.show())
                .collect::<Vec<_>>()
        };
        for source in [
            "stepcat(...productListHelpers.shrink([1, 3], productListBase))",
            "stepcat(...productListHelpers.alias([1, 3], productListBase))",
            "stepcat(...productListHelpers.aliasMethod.call(productListBase, [1, 3]))",
        ] {
            session
                .evaluate(&format!("assertProductListPoison(); {source}"))
                .unwrap_or_else(|error| panic!("evaluate {source}: {error}"));
            assert!(
                !session.active_needs_host(),
                "{source}: list graph became impure"
            );
            assert_eq!(
                session
                    .js
                    .active_pattern()
                    .expect("active list graph")
                    .steps,
                Some(Fraction::int(9)),
                "{source}: stepcat lost list step metadata"
            );
            assert_eq!(
                session
                    .query(Fraction::ZERO, Fraction::ONE)
                    .expect("query retained shrink helper")
                    .into_iter()
                    .map(|hap| hap.show())
                    .collect::<Vec<_>>(),
                SHRINK_HAPS,
                "{source}: shrinklist timing changed"
            );
        }

        let onsets = session
            .play(2.0)
            .expect("schedule retained shrinklist")
            .onsets
            .into_iter()
            .map(|onset| (onset.whole_begin, onset.value_show))
            .collect::<Vec<_>>();
        assert_eq!(
            onsets,
            [
                ("0/1".to_owned(), "a".to_owned()),
                ("1/9".to_owned(), "b".to_owned()),
                ("2/9".to_owned(), "c".to_owned()),
                ("1/3".to_owned(), "d".to_owned()),
                ("4/9".to_owned(), "b".to_owned()),
                ("5/9".to_owned(), "c".to_owned()),
                ("2/3".to_owned(), "d".to_owned()),
                ("7/9".to_owned(), "c".to_owned()),
                ("8/9".to_owned(), "d".to_owned()),
                ("1/1".to_owned(), "a".to_owned()),
            ],
            "Session scheduler changed shrinklist inclusive-boundary onsets"
        );

        session
            .evaluate(
                "assertProductListPoison(); \
                 stepcat(...productListHelpers.grow([1, 3], productListBase))",
            )
            .expect("evaluate retained growlist");
        assert_eq!(
            query_values(&session),
            GROW_VALUES,
            "growlist did not reverse the same planned list"
        );
        assert_eq!(
            session.js.query_depth(),
            0,
            "list-helper query roots leaked"
        );
    }

    #[test]
    fn shrinklist_helper_refusal_is_typed_atomic_and_same_now_recoverable() {
        let limit = rustel_core::MAX_STEPWISE_ENTRIES;
        assert_eq!(limit, 16_384);
        let over = limit + 1;

        for helper in ["s_taperlist", "growlist"] {
            let mut session = Session::new().expect("list-helper refusal session");
            session
                .evaluate(&format!("stepcat(...{helper}([0, {over}], gap({over})))"))
                .unwrap_or_else(|error| panic!("construct refused {helper} graph: {error}"));
            assert!(
                !session.active_needs_host(),
                "{helper}: resource refusal retained the JavaScript host"
            );

            let generation = session.generation();
            let queued = session.scheduler.queued();
            let horizon = session.scheduler.horizon_remaining(0.0);
            let error = session
                .schedule_at(0.0)
                .expect_err("MAX+1 list must refuse scheduling");
            let RuntimeError::ResourceLimit(message) = error else {
                panic!("{helper}: list refusal lost its runtime type: {error:?}");
            };
            assert!(
                message.contains("shrinklist")
                    && message.contains(&over.to_string())
                    && message.contains(&limit.to_string()),
                "{helper}: wrong list expansion refusal: {message}"
            );
            assert!(
                matches!(
                    session.scheduler.refusal(),
                    Some(rustel_core::QueryLimit::StepwiseExpansion {
                        operation,
                        minimum_entries,
                        limit: refusal_limit,
                    }) if *operation == "shrinklist"
                        && *minimum_entries == over
                        && *refusal_limit == limit
                ),
                "{helper}: scheduler lost structured shrinklist refusal: {:?}",
                session.scheduler.refusal()
            );
            assert_eq!(session.scheduler.queued(), queued);
            assert_eq!(session.scheduler.horizon_remaining(0.0), horizon);
            assert_eq!(session.generation(), generation);

            session
                .evaluate("pure('list-recovered')")
                .expect("install list-helper recovery score");
            let recovered = session
                .schedule_at(0.0)
                .expect("same-now recovery after list-helper refusal");
            assert_eq!(recovered.len(), 1);
            assert_eq!(recovered[0].onset_id, 0, "refusal consumed an onset id");
            assert_eq!(recovered[0].whole_begin, "0/1");
            assert_eq!(recovered[0].value_show, "list-recovered");
        }
    }

    #[test]
    fn tour_expansion_refusal_keeps_the_scheduler_atomic_and_recovers() {
        let limit = rustel_core::MAX_STEPWISE_ENTRIES;
        assert_eq!(limit, 16_384);

        let mut exact = Session::new().expect("exact tour session");
        exact
            .evaluate("silence.tour(...Array.from({ length: 127 }, () => silence))")
            .expect("construct exact-bound tour");
        assert_eq!(
            exact.js.active_pattern().expect("exact-bound tour").steps,
            Some(Fraction::int(16_384))
        );
        assert!(
            exact
                .schedule_at(0.0)
                .expect("schedule exact-bound tour")
                .is_empty(),
            "the all-silent exact-bound control emitted an onset"
        );

        let mut session = Session::new().expect("refused tour session");
        session
            .evaluate("silence.tour(...Array.from({ length: 128 }, () => silence))")
            .expect("construct refused tour graph");
        assert!(!session.active_needs_host());
        let generation = session.generation();
        let queued = session.scheduler.queued();
        let horizon = session.scheduler.horizon_remaining(0.0);
        let error = session
            .schedule_at(0.0)
            .expect_err("16641-entry tour must refuse the scheduler tick");
        let RuntimeError::ResourceLimit(message) = error else {
            panic!("tour expansion lost its typed refusal: {error:?}");
        };
        assert!(
            message.contains("tour")
                && message.contains("16641")
                && message.contains(&limit.to_string()),
            "wrong tour expansion refusal: {message}"
        );
        assert!(
            matches!(
                session.scheduler.refusal(),
                Some(rustel_core::QueryLimit::StepwiseExpansion {
                    operation,
                    minimum_entries,
                    limit: refusal_limit,
                }) if *operation == "tour"
                    && *minimum_entries == 16_641
                    && *refusal_limit == limit
            ),
            "scheduler did not retain the structured tour refusal: {:?}",
            session.scheduler.refusal()
        );
        assert_eq!(session.scheduler.queued(), queued);
        assert_eq!(session.scheduler.horizon_remaining(0.0), horizon);
        assert_eq!(session.generation(), generation);

        session
            .evaluate("pure('tour-recovered')")
            .expect("install recovery score");
        let recovered = session
            .schedule_at(0.0)
            .expect("same-now recovery after tour refusal");
        assert_eq!(recovered.len(), 1);
        assert_eq!(recovered[0].onset_id, 0, "refusal consumed an onset id");
        assert_eq!(recovered[0].whole_begin, "0/1");
        assert_eq!(recovered[0].value_show, "tour-recovered");
    }

    #[test]
    fn stepalt_source_and_lcm_boundaries_are_typed_atomic_and_recoverable() {
        let limit = rustel_core::MAX_STEPWISE_ENTRIES;
        assert_eq!(limit, 16_384);

        // LCM 8192 across two groups creates exactly 16,384 cycle-major
        // candidate references. All are zero-step and filtered afterwards,
        // so scheduling proves the boundary without producing 16k haps.
        let mut exact = Session::new().expect("exact stepalt session");
        exact
            .evaluate("stepalt(Array(8192).fill(nothing), [nothing])")
            .expect("construct exact-bound stepalt");
        assert_eq!(
            exact
                .js
                .active_pattern()
                .expect("exact-bound stepalt")
                .steps,
            Some(Fraction::ZERO)
        );
        assert!(
            exact
                .schedule_at(0.0)
                .expect("schedule exact-bound stepalt")
                .is_empty(),
            "the all-filtered exact-bound control emitted an onset"
        );

        let mut session = Session::new().expect("refused stepalt session");
        session
            .evaluate("stepalt(Array(8193).fill(nothing), [nothing])")
            .expect("construct refused LCM-amplified stepalt graph");
        assert!(!session.active_needs_host());
        let generation = session.generation();
        let queued = session.scheduler.queued();
        let horizon = session.scheduler.horizon_remaining(0.0);
        let error = session
            .schedule_at(0.0)
            .expect_err("16386-entry stepalt must refuse the scheduler tick");
        let RuntimeError::ResourceLimit(message) = error else {
            panic!("stepalt expansion lost its typed refusal: {error:?}");
        };
        assert!(
            message.contains("stepalt")
                && message.contains("16386")
                && message.contains(&limit.to_string()),
            "wrong stepalt expansion refusal: {message}"
        );
        assert!(
            matches!(
                session.scheduler.refusal(),
                Some(rustel_core::QueryLimit::StepwiseExpansion {
                    operation,
                    minimum_entries,
                    limit: refusal_limit,
                }) if *operation == "stepalt"
                    && *minimum_entries == 16_386
                    && *refusal_limit == limit
            ),
            "scheduler did not retain the structured stepalt refusal: {:?}",
            session.scheduler.refusal()
        );
        assert_eq!(session.scheduler.queued(), queued);
        assert_eq!(session.scheduler.horizon_remaining(0.0), horizon);
        assert_eq!(session.generation(), generation);

        session
            .evaluate("pure('stepalt-recovered')")
            .expect("install recovery score");
        let recovered = session
            .schedule_at(0.0)
            .expect("same-now recovery after stepalt refusal");
        assert_eq!(recovered.len(), 1);
        assert_eq!(recovered[0].onset_id, 0, "refusal consumed an onset id");
        assert_eq!(recovered[0].whole_begin, "0/1");
        assert_eq!(recovered[0].value_show, "stepalt-recovered");

        // An empty group makes cyclic expansion zero, but strudel.cc first maps
        // every other source group. The native cap also bounds that source
        // stage and deliberately refuses before invoking the mutable parser.
        let mut source = Session::new().expect("stepalt source-bound session");
        source
            .evaluate_prebake(
                "globalThis.stepaltRefusedParserCalls = 0; \
                 setStringParser(value => { \
                   stepaltRefusedParserCalls++; return pure(value); \
                 });",
            )
            .expect("install stepalt parser counter");
        source
            .evaluate("stepalt([], Array(16385).fill('x'))")
            .expect("construct source-bound stepalt refusal");
        assert_eq!(
            source.js.get_number("stepaltRefusedParserCalls"),
            Some(0.0),
            "oversized stepalt reified input before its resource preflight"
        );
        let error = source
            .schedule_at(0.0)
            .expect_err("oversized zero-expansion source must still refuse");
        let RuntimeError::ResourceLimit(message) = error else {
            panic!("stepalt source bound lost its typed refusal: {error:?}");
        };
        assert!(
            message.contains("stepalt")
                && message.contains("16385")
                && message.contains(&limit.to_string()),
            "wrong stepalt source-stage refusal: {message}"
        );
    }

    #[test]
    fn polymeter_boundaries_are_typed_atomic_and_recoverable() {
        let limit = rustel_core::MAX_STEPWISE_ENTRIES;
        assert_eq!(limit, 16_384);

        for (source, label) in [
            ("polymeter(gap(1), gap(16383))", "modern"),
            ("polymeter(Array(8192).fill(silence), [silence])", "legacy"),
        ] {
            let mut exact = Session::new().expect("exact polymeter session");
            exact
                .evaluate(source)
                .unwrap_or_else(|error| panic!("construct exact {label} polymeter: {error}"));
            assert!(
                exact
                    .schedule_at(0.0)
                    .unwrap_or_else(|error| panic!("schedule exact {label} polymeter: {error}"))
                    .is_empty(),
                "all-silent exact {label} polymeter emitted an onset"
            );
        }

        for (source, minimum_entries, label) in [
            ("polymeter(gap(1), gap(16384))", 16_385, "modern"),
            (
                "polymeter(Array(8193).fill(silence), [silence])",
                16_386,
                "legacy",
            ),
        ] {
            let mut session = Session::new().expect("refused polymeter session");
            session
                .evaluate(source)
                .unwrap_or_else(|error| panic!("construct refused {label} polymeter: {error}"));
            assert!(
                !session.active_needs_host(),
                "refused {label} polymeter unexpectedly retained the host"
            );
            let generation = session.generation();
            let queued = session.scheduler.queued();
            let horizon = session.scheduler.horizon_remaining(0.0);
            let error = match session.schedule_at(0.0) {
                Ok(events) => {
                    panic!("{label} polymeter did not refuse the scheduler tick: {events:?}")
                }
                Err(error) => error,
            };
            let RuntimeError::ResourceLimit(message) = error else {
                panic!("{label} polymeter lost its typed refusal: {error:?}");
            };
            assert!(
                message.contains("polymeter")
                    && message.contains(&minimum_entries.to_string())
                    && message.contains(&limit.to_string()),
                "wrong {label} polymeter refusal: {message}"
            );
            assert!(
                matches!(
                    session.scheduler.refusal(),
                    Some(rustel_core::QueryLimit::StepwiseExpansion {
                        operation,
                        minimum_entries: observed,
                        limit: refusal_limit,
                    }) if *operation == "polymeter"
                        && *observed == minimum_entries
                        && *refusal_limit == limit
                ),
                "scheduler lost the structured {label} polymeter refusal: {:?}",
                session.scheduler.refusal()
            );
            assert_eq!(session.scheduler.queued(), queued);
            assert_eq!(session.scheduler.horizon_remaining(0.0), horizon);
            assert_eq!(session.generation(), generation);

            session
                .evaluate(&format!("pure('{label}-polymeter-recovered')"))
                .unwrap_or_else(|error| panic!("install {label} recovery score: {error}"));
            let recovered = session
                .schedule_at(0.0)
                .unwrap_or_else(|error| panic!("same-now {label} recovery: {error}"));
            assert_eq!(recovered.len(), 1);
            assert_eq!(recovered[0].onset_id, 0, "refusal consumed an onset id");
            assert_eq!(recovered[0].whole_begin, "0/1");
            assert_eq!(
                recovered[0].value_show,
                format!("{label}-polymeter-recovered")
            );
        }
    }

    #[test]
    fn indexed_echo_owns_callback_arguments_and_results_across_setup_gc() {
        let mut session = Session::new().expect("session");
        session.js.install_gc_binding().expect("gc binding");
        session
            .evaluate(
                r#"
                (() => {
                  globalThis.__echoIndexedCalls = [];
                  globalThis.__echoRetainedArgs = [];
                  const source = pure('x').fmap(value => value);
                  const result = source.echoWith(3, 1/4, function (pattern, index) {
                    __echoIndexedCalls.push(
                      `${arguments.length}:${typeof index}:${index}`
                    );
                    __echoRetainedArgs.push(pattern);
                    return pattern.fmap(value => `${value}:${index}`);
                  });
                  globalThis.__echoIndexedShape = JSON.stringify(__echoIndexedCalls);
                  return result;
                })()
                "#,
            )
            .expect("install indexed echo score");
        assert_eq!(
            session.js.get_string("__echoIndexedShape").as_deref(),
            Some(r#"["2:number:0","2:number:1","2:number:2"]"#),
            "scalar echoWith did not invoke its callback eagerly, once per copy, with the exact index argument"
        );
        assert!(
            session.active_needs_host(),
            "returned fmap callbacks were incorrectly classified as host-free"
        );
        let active = session.js.active_pattern().expect("active indexed graph");
        assert_eq!(
            active.reachable_callbacks().len(),
            5,
            "the final graph must own one source callback, three returned callbacks, and the native event finalizer, but not the construction-only indexed callback"
        );

        let snapshot = |session: &Session| {
            let mut haps = session
                .query(Fraction::ZERO, Fraction::ONE)
                .expect("query indexed echo graph")
                .into_iter()
                .map(|hap| {
                    let whole = hap.whole.expect("echo haps are discrete");
                    (
                        hap.value.show(),
                        whole.begin,
                        whole.end,
                        hap.part.begin,
                        hap.part.end,
                    )
                })
                .collect::<Vec<_>>();
            haps.sort();
            haps
        };
        let expected = vec![
            (
                "x:0".to_owned(),
                Fraction::ZERO,
                Fraction::ONE,
                Fraction::ZERO,
                Fraction::ONE,
            ),
            (
                "x:1".to_owned(),
                Fraction::new(-3, 4),
                Fraction::new(1, 4),
                Fraction::ZERO,
                Fraction::new(1, 4),
            ),
            (
                "x:1".to_owned(),
                Fraction::new(1, 4),
                Fraction::new(5, 4),
                Fraction::new(1, 4),
                Fraction::ONE,
            ),
            (
                "x:2".to_owned(),
                Fraction::new(-1, 2),
                Fraction::new(1, 2),
                Fraction::ZERO,
                Fraction::new(1, 2),
            ),
            (
                "x:2".to_owned(),
                Fraction::new(1, 2),
                Fraction::new(3, 2),
                Fraction::new(1, 2),
                Fraction::ONE,
            ),
        ];
        assert_eq!(snapshot(&session), expected, "wrong indexed echo timing");

        session.js.run_gc();
        session.js.run_gc();
        session
            .evaluate_prebake(
                r#"
                (() => {
                  const state = {
                    span: { begin: Fraction(0), end: Fraction(1) },
                    controls: {}
                  };
                  globalThis.__echoRetainedProof = JSON.stringify(
                    __echoRetainedArgs.map(pattern => {
                      const haps = pattern.query(state);
                      return haps.length > 0
                        && haps.every(hap => hap.value === 'x');
                    })
                  );
                })()
                "#,
            )
            .expect("query retained callback arguments after GC");
        assert_eq!(
            session.js.get_string("__echoRetainedProof").as_deref(),
            Some("[true,true,true]"),
            "callback argument wrappers lost their source sidecars after GC"
        );
        assert_eq!(
            snapshot(&session),
            expected,
            "returned patterns lost callback ownership after GC and another setup turn"
        );
        assert_eq!(session.js.query_depth(), 0, "indexed query scope leaked");
        assert_eq!(
            rustel_jsruntime::bridge_frame_depth(),
            0,
            "indexed query leaked a bridge frame"
        );
        assert_eq!(
            rustel_jsruntime::bridge_scratch_len(),
            0,
            "indexed query leaked harvested callback roots"
        );
    }

    #[test]
    fn projected_list_surface_survives_setup_turn_gc_and_global_decoys() {
        let mut session = Session::new().expect("session");
        session.js.install_gc_binding().expect("gc binding");
        session
            .evaluate_prebake(
                r#"
                const expected = [
                  'pace', 'take', 'drop', 'extend', 'replicate', 'expand',
                  'contract', 'shrink', 'grow',
                  'Fraction', 'Pattern', 'cat', 'fastcat', 'gap', 'growlist', 'noteToMidi',
                  'nothing', 'pm', 'polymeter', 'polyrhythm', 'pr', 'pure',
                  'register', 'reify',
                  's_add', 's_alt', 's_cat', 's_contract', 's_expand', 's_extend',
                  's_polymeter', 's_sub', 's_taper', 's_taperlist', 's_tour', 's_zip',
                  'seq', 'sequence',
                  'setStringParser', 'shrinklist', 'silence',
                  'slowcat', 'stack', 'stepalt', 'stepcat', 'steps', 'rustelScope',
                  'timeCat', 'timecat', 'tour', 'zip',
                  'setCps', 'setcps', 'setCpm', 'setcpm', 'cps',
                  'midimaps', 'defaultmidimap',
                  'voicings', 'rootNotes', 'voicing',
                  'setDefaultVoicings', 'resetVoicings'
                ];
                const keys = Object.keys(rustelScope);
                if (JSON.stringify(keys) !== JSON.stringify(expected)) {
                  throw new Error(`list scope order: ${keys.join(',')}`);
                }
                for (const name of expected) {
                  if (rustelScope[name] !== globalThis[name]) {
                    throw new Error(`list scope identity: ${name}`);
                  }
                }
                if (rustelScope.voicing !== globalThis.voicing) {
                  throw new Error('native voicing projection lost');
                }
                if (rustelScope.polyrhythm !== rustelScope.pr
                    || rustelScope.pr !== rustelScope.stack
                    || rustelScope.timecat !== rustelScope.stepcat) {
                  throw new Error('list alias identity');
                }
                if (rustelScope.cat === rustelScope.slowcat
                    || rustelScope.fastcat === rustelScope.seq
                    || rustelScope.fastcat === rustelScope.sequence
                    || rustelScope.seq === rustelScope.sequence) {
                  throw new Error('distinct list declarations collapsed');
                }
                const singletonNames = ['cat', 'fastcat', 'seq', 'sequence', 'slowcat'];
                for (const name of [...singletonNames, 'stack']) {
                  const descriptor = Object.getOwnPropertyDescriptor(Pattern.prototype, name);
                  if (!descriptor || descriptor.value.name !== name
                      || descriptor.value.length !== 0 || !descriptor.writable
                      || descriptor.enumerable || !descriptor.configurable) {
                    throw new Error(`prototype list descriptor: ${name}`);
                  }
                }
                const owned = suffix => `owned:${suffix}`;
                const source = reify(owned);
                for (const name of singletonNames) {
                  if (rustelScope[name](source) !== source
                      || rustelScope[name]([[[source]]]) !== source
                      || source[name]() !== source) {
                    throw new Error(`singleton wrapper identity: ${name}`);
                  }
                }
                if (rustelScope.stack(source) === source || source.stack() === source) {
                  throw new Error('stack incorrectly reused a singleton wrapper');
                }
                globalThis.productListSingleton = rustelScope.seq([[[source]]]);
                for (const name of [
                  'cat', 'fastcat', 'polyrhythm', 'pr', 'seq', 'sequence',
                  'slowcat', 'stack', 'stepcat', 'timecat'
                ]) {
                  globalThis[name] = function poisonedListGlobal() {
                    throw new Error(`mutable global consulted: ${name}`);
                  };
                }
                globalThis.productWeighted = rustelScope.timecat(
                  [1, productListSingleton], [3, pure('tail')]
                );
                globalThis.productFast = pure('left').fastcat(pure('right'));
                globalThis.productStack = productListSingleton.stack();
                "#,
            )
            .expect("install projected list surface product state");
        session
            .evaluate_prebake("__gc(); __gc();")
            .expect("collect between setup and score turns");
        session
            .evaluate(
                r#"
                rustelScope.stack(
                  productWeighted.fmap(value =>
                    typeof value === 'function' ? value('weighted') : value
                  ),
                  productFast,
                  productStack.fmap(value => value('stack'))
                )
                "#,
            )
            .expect("score through retained list surface");

        let mut got = session
            .query(Fraction::ZERO, Fraction::ONE)
            .expect("query retained list score")
            .into_iter()
            .map(|hap| (hap.value.show(), hap.part.begin, hap.part.end))
            .collect::<Vec<_>>();
        got.sort_by(|left, right| left.0.cmp(&right.0));
        assert_eq!(
            got,
            [
                ("left".to_owned(), Fraction::ZERO, Fraction::new(1, 2),),
                ("owned:stack".to_owned(), Fraction::ZERO, Fraction::ONE,),
                (
                    "owned:weighted".to_owned(),
                    Fraction::ZERO,
                    Fraction::new(1, 4),
                ),
                ("right".to_owned(), Fraction::new(1, 2), Fraction::ONE,),
                ("tail".to_owned(), Fraction::new(1, 4), Fraction::ONE,),
            ],
            "projected list route lost identity, receiver order, or timing"
        );
        assert_eq!(
            session.js.query_depth(),
            0,
            "list product leaked query roots"
        );
    }

    #[test]
    fn projected_stepwise_aliases_survive_gc_and_independent_destination_poisoning() {
        let mut session = Session::new().expect("session");
        session.js.install_gc_binding().expect("gc binding");
        session
            .evaluate_prebake(
                r#"
                const methodAliases = [
                  ['s_taper', 'shrink'],
                  ['s_taperlist', 'shrinklist'],
                  ['s_add', 'take'],
                  ['s_sub', 'drop'],
                  ['s_expand', 'expand'],
                  ['s_extend', 'extend'],
                  ['s_contract', 'contract'],
                  ['steps', 'pace']
                ];
                const freeAliases = [
                  ...methodAliases,
                  ['s_cat', 'stepcat'],
                  ['timeCat', 'stepcat'],
                  ['timecat', 'stepcat']
                ];
                const assertOrdinaryDataProperty = (object, name, value, label) => {
                  const descriptor = Object.getOwnPropertyDescriptor(object, name);
                  if (!descriptor || !Object.hasOwn(descriptor, 'value')
                      || descriptor.value !== value || !descriptor.writable
                      || !descriptor.enumerable || !descriptor.configurable) {
                    throw new Error(`${label} descriptor: ${name}`);
                  }
                };

                for (const [alias, canonical] of freeAliases) {
                  if (globalThis[alias] !== globalThis[canonical]
                      || rustelScope[alias] !== globalThis[canonical]) {
                    throw new Error(`free alias identity: ${alias}`);
                  }
                  assertOrdinaryDataProperty(
                    globalThis, alias, globalThis[canonical], 'global alias'
                  );
                  assertOrdinaryDataProperty(
                    rustelScope, alias, globalThis[canonical], 'scope alias'
                  );
                }
                for (const [alias, canonical] of methodAliases) {
                  if (Pattern.prototype[alias] !== Pattern.prototype[canonical]) {
                    throw new Error(`method alias identity: ${alias}`);
                  }
                  assertOrdinaryDataProperty(
                    Pattern.prototype,
                    alias,
                    Pattern.prototype[canonical],
                    'prototype alias'
                  );
                }
                for (const name of ['timecat', 'timeCat', 's_cat']) {
                  if (Object.hasOwn(Pattern.prototype, name)) {
                    throw new Error(`non-method alias installed on Pattern: ${name}`);
                  }
                }
                const forbiddenRawAliases = [
                  '_s_add', '_s_sub', '_s_taper', '_s_taperlist', '_s_expand', '_s_extend',
                  '_s_contract', '_s_cat', '_timeCat', '_timecat'
                ];
                for (const [label, object] of [
                  ['global', globalThis],
                  ['scope', rustelScope],
                  ['prototype', Pattern.prototype]
                ]) {
                  for (const name of forbiddenRawAliases) {
                    if (Object.hasOwn(object, name)) {
                      throw new Error(`${label} raw alias: ${name}`);
                    }
                  }
                }

                // Each saved function comes from a different public slot. The
                // later score invokes all of them only after two forced GCs.
                globalThis.productStepwiseAliases = Object.freeze({
                  taper: rustelScope.s_taper,
                  taperMethod: Pattern.prototype.s_taper,
                  take: globalThis.s_add,
                  drop: rustelScope.s_sub,
                  expandMethod: Pattern.prototype.s_expand,
                  paceMethod: Pattern.prototype.steps,
                  weighted: rustelScope.timeCat
                });

                globalThis.productAliasGlobalDecoy =
                  function poisonedAliasGlobal() {
                    throw new Error('mutable alias global consulted');
                  };
                globalThis.productAliasScopeDecoy =
                  function poisonedAliasScope() {
                    throw new Error('mutable alias scope consulted');
                  };
                globalThis.productAliasPrototypeDecoy =
                  function poisonedAliasPrototype() {
                    throw new Error('mutable alias prototype consulted');
                  };
                globalThis.productTaperCanonicalDecoys = Object.freeze({
                  global: function poisonedTaperGlobal() {
                    throw new Error('mutable shrink global consulted');
                  },
                  scope: function poisonedTaperScope() {
                    throw new Error('mutable shrink scope consulted');
                  },
                  method: function poisonedTaperMethod() {
                    throw new Error('mutable shrink prototype consulted');
                  }
                });
                globalThis.s_add = productAliasGlobalDecoy;
                rustelScope.s_sub = productAliasScopeDecoy;
                Pattern.prototype.s_expand = productAliasPrototypeDecoy;
                globalThis.s_taper = productAliasGlobalDecoy;
                rustelScope.s_taper = productAliasScopeDecoy;
                Pattern.prototype.s_taper = productAliasPrototypeDecoy;
                globalThis.shrink = productTaperCanonicalDecoys.global;
                rustelScope.shrink = productTaperCanonicalDecoys.scope;
                Pattern.prototype.shrink = productTaperCanonicalDecoys.method;
                delete globalThis.steps;
                delete rustelScope.timeCat;
                delete Pattern.prototype.s_contract;

                globalThis.assertStepwiseAliasPoison = () => {
                  if (globalThis.s_add !== productAliasGlobalDecoy
                      || rustelScope.s_sub !== productAliasScopeDecoy
                      || Pattern.prototype.s_expand
                          !== productAliasPrototypeDecoy
                      || globalThis.s_taper !== productAliasGlobalDecoy
                      || rustelScope.s_taper !== productAliasScopeDecoy
                      || Pattern.prototype.s_taper
                          !== productAliasPrototypeDecoy
                      || globalThis.shrink
                          !== productTaperCanonicalDecoys.global
                      || rustelScope.shrink
                          !== productTaperCanonicalDecoys.scope
                      || Pattern.prototype.shrink
                          !== productTaperCanonicalDecoys.method
                      || Object.hasOwn(globalThis, 'steps')
                      || Object.hasOwn(rustelScope, 'timeCat')
                      || Object.hasOwn(Pattern.prototype, 's_contract')) {
                    throw new Error('static stepwise aliases were reinjected');
                  }
                };
                "#,
            )
            .expect("install and poison projected stepwise aliases");

        session
            .evaluate(
                r#"
                (() => {
                  assertStepwiseAliasPoison();
                  return productStepwiseAliases.weighted(
                    [1, pure(70)], [1, pure(71)]
                  );
                })()
                "#,
            )
            .expect("successful score through a saved alias");
        session.js.run_gc();
        session.js.run_gc();

        let failure = session
            .evaluate(
                r#"
                (() => {
                  assertStepwiseAliasPoison();
                  globalThis.productAliasFailedTurn = 1;
                  setcps(0);
                  return pure(0);
                })()
                "#,
            )
            .expect_err("invalid-tempo score must fail after checking alias poison");
        assert_eq!(failure.kind(), "evaluation");
        assert_eq!(
            session.js.get_number("productAliasFailedTurn"),
            Some(1.0),
            "the failed score never reached its reinjection assertion"
        );

        session
            .evaluate(
                r#"
                (() => {
                  assertStepwiseAliasPoison();
                  const taperBase = () => sequence(60, 61, 62, 63);
                  const tapered = productStepwiseAliases.taper(
                    1, taperBase()
                  );
                  const taperedCurried = productStepwiseAliases.taper(1)(
                    taperBase()
                  );
                  const taperedMethod = productStepwiseAliases.taperMethod.call(
                    taperBase(), 1
                  );
                  const taperState = {
                    span: { begin: 0, end: 1 }, controls: {}
                  };
                  const taperView = pattern => `${pattern._steps.show()}|`
                    + pattern.query(taperState).map(hap =>
                      `${hap.value}:${hap.part.begin.show()}`
                        + `>${hap.part.end.show()}`
                    ).join(',');
                  const expectedTaper = '10/1|60:0/1>1/10,61:1/10>1/5,'
                    + '62:1/5>3/10,63:3/10>2/5,61:2/5>1/2,'
                    + '62:1/2>3/5,63:3/5>7/10,62:7/10>4/5,'
                    + '63:4/5>9/10,63:9/10>1/1';
                  if ([tapered, taperedCurried, taperedMethod].some(
                    pattern => taperView(pattern) !== expectedTaper
                  )) {
                    throw new Error('saved s_taper semantics mismatch');
                  }
                  globalThis.productTaperProof = 1;
                  const taken = productStepwiseAliases.take(
                    2, sequence(10, 11, 12)
                  );
                  const dropped = productStepwiseAliases.drop(
                    1, sequence(20, 21, 22)
                  );
                  const expanded = productStepwiseAliases.expandMethod.call(
                    sequence(40, 41), 2
                  );
                  const paced = productStepwiseAliases.paceMethod.call(
                    sequence(50, 51), 4
                  );
                  const weighted = productStepwiseAliases.weighted(
                    [1, pure(30)], [1, pure(31)]
                  );
                  return stack(
                    taken,
                    dropped,
                    weighted,
                    stepcat(expanded, 99),
                    paced
                  );
                })()
                "#,
            )
            .expect("score through saved aliases after GC and failed evaluation");
        assert_eq!(
            session.js.get_number("productTaperProof"),
            Some(1.0),
            "saved s_taper was not exercised after GC and destination poisoning"
        );

        let got = session
            .query(Fraction::ZERO, Fraction::ONE)
            .expect("query retained stepwise aliases")
            .into_iter()
            .map(|hap| hap.show())
            .collect::<Vec<_>>();
        assert_eq!(
            got,
            [
                "[ 0/1 → 1/4 | 50 ]",
                "[ 0/1 → 2/5 | 40 ]",
                "[ 0/1 → 1/2 | 10 ]",
                "[ 0/1 → 1/2 | 21 ]",
                "[ 0/1 → 1/2 | 30 ]",
                "[ 1/4 → 1/2 | 51 ]",
                "[ 2/5 → 4/5 | 41 ]",
                "[ 1/2 → 3/4 | 50 ]",
                "[ 1/2 → 1/1 | 11 ]",
                "[ 1/2 → 1/1 | 22 ]",
                "[ 1/2 → 1/1 | 31 ]",
                "[ 3/4 → 1/1 | 51 ]",
                "[ 4/5 → 1/1 | 99 ]",
            ],
            "saved stepwise aliases lost callability or representative semantics"
        );
        assert_eq!(
            session.js.query_depth(),
            0,
            "alias product leaked query roots"
        );
    }

    #[test]
    fn projected_elementals_survive_setup_gc_and_mutable_global_decoys() {
        let mut session = Session::new().expect("session");
        session.js.install_gc_binding().expect("gc binding");
        session
            .evaluate_prebake(
                r#"
                const expected = [
                  'pace', 'take', 'drop', 'extend', 'replicate', 'expand',
                  'contract', 'shrink', 'grow',
                  'Fraction', 'Pattern', 'cat', 'fastcat', 'gap', 'growlist', 'noteToMidi',
                  'nothing', 'pm', 'polymeter', 'polyrhythm', 'pr', 'pure',
                  'register', 'reify',
                  's_add', 's_alt', 's_cat', 's_contract', 's_expand', 's_extend',
                  's_polymeter', 's_sub', 's_taper', 's_taperlist', 's_tour', 's_zip',
                  'seq', 'sequence',
                  'setStringParser', 'shrinklist', 'silence',
                  'slowcat', 'stack', 'stepalt', 'stepcat', 'steps', 'rustelScope',
                  'timeCat', 'timecat', 'tour', 'zip',
                  'setCps', 'setcps', 'setCpm', 'setcpm', 'cps',
                  'midimaps', 'defaultmidimap',
                  'voicings', 'rootNotes', 'voicing',
                  'setDefaultVoicings', 'resetVoicings'
                ];
                const keys = Object.keys(rustelScope);
                if (JSON.stringify(keys) !== JSON.stringify(expected)) {
                  throw new Error(`elemental scope order: ${keys.join(',')}`);
                }
                for (const name of expected) {
                  const descriptor = Object.getOwnPropertyDescriptor(
                    rustelScope, name
                  );
                  const globalDescriptor = Object.getOwnPropertyDescriptor(
                    globalThis, name
                  );
                  if (rustelScope[name] !== globalThis[name]
                      || !descriptor?.writable || !descriptor.enumerable
                      || !descriptor.configurable || !globalDescriptor?.writable
                      || !globalDescriptor.enumerable
                      || !globalDescriptor.configurable) {
                    throw new Error(`elemental scope identity: ${name}`);
                  }
                }
                if (rustelScope.voicing !== globalThis.voicing) {
                  throw new Error('native voicing projection lost');
                }
                if (Object.getPrototypeOf(rustelScope) !== Object.prototype
                    || rustelScope.rustelScope !== rustelScope) {
                  throw new Error('elemental scope shape');
                }

                const constructible = value => {
                  try { Reflect.construct(value, []); return true; }
                  catch (_) { return false; }
                };
                if (Fraction.name !== 'fraction' || Fraction.length !== 1
                    || Object.hasOwn(Fraction, 'prototype')
                    || constructible(Fraction)
                    || JSON.stringify(Object.keys(Fraction)) !== '["_original"]'
                    || Fraction._original.name !== 'Fraction') {
                  throw new Error('Fraction reflection');
                }
                const originalDescriptor = Object.getOwnPropertyDescriptor(
                  Fraction, '_original'
                );
                if (!originalDescriptor.writable || !originalDescriptor.enumerable
                    || !originalDescriptor.configurable) {
                  throw new Error('Fraction._original descriptor');
                }
                if (gap.name !== 'gap' || gap.length !== 1
                    || Object.hasOwn(gap, 'prototype') || constructible(gap)) {
                  throw new Error('gap reflection');
                }
                if (pure.name !== 'pure' || pure.length !== 1
                    || !Object.hasOwn(pure, 'prototype') || !constructible(pure)) {
                  throw new Error('pure reflection');
                }

                const normalized = Fraction('2/6');
                if (!(normalized instanceof Fraction._original)
                    || Object.getPrototypeOf(normalized) !== Fraction._original.prototype
                    || normalized.show() !== '1/3' || Number(normalized) !== 1 / 3
                    || Object.getPrototypeOf(gap('2/6')._steps)
                        !== Fraction._original.prototype) {
                  throw new Error('Fraction normalization');
                }
                if (silence === nothing || silence._steps.show() !== '1/1'
                    || nothing._steps.show() !== '0/1'
                    || gap(1) === silence || gap(0) === nothing) {
                  throw new Error('canonical empty singleton shape');
                }
                if (Object.hasOwn(silence, '__pure')
                    || Object.hasOwn(nothing, '__pure')) {
                  throw new Error('empty singletons are not pure values');
                }

                globalThis.elementalCanonicals = {
                  Fraction: rustelScope.Fraction,
                  gap: rustelScope.gap,
                  nothing: rustelScope.nothing,
                  pure: rustelScope.pure,
                  silence: rustelScope.silence,
                  stack: rustelScope.stack,
                  stepcat: rustelScope.stepcat
                };
                globalThis.elementalSilence = elementalCanonicals.silence;
                globalThis.elementalNothing = elementalCanonicals.nothing;
                globalThis.elementalObject = { marker: 7 };
                globalThis.elementalFunction = suffix => `owned:${suffix}`;
                globalThis.elementalPureObject = elementalCanonicals.pure(elementalObject);
                globalThis.elementalPureFunction = elementalCanonicals.pure(elementalFunction);
                if (elementalPureObject.__pure !== elementalObject
                    || elementalPureFunction.__pure !== elementalFunction
                    || elementalCanonicals.pure(elementalObject) === elementalPureObject
                    || new (elementalCanonicals.pure)(elementalObject).__pure
                        !== elementalObject) {
                  throw new Error('pure construction identity');
                }

                // Both evalScope destinations are ordinary mutable snapshots
                // on strudel.cc. Save the canonical objects outside those slots,
                // then make assignment and deletion independently observable.
                globalThis.elementalGlobalDecoys = {};
                globalThis.elementalScopeDecoys = {};
                for (const name of ['Fraction', 'gap', 'pure']) {
                  const globalDecoy = function poisonedElementalGlobal() {
                    throw new Error(`mutable global consulted: ${name}`);
                  };
                  const scopeDecoy = function poisonedElementalScope() {
                    throw new Error(`mutable scope consulted: ${name}`);
                  };
                  elementalGlobalDecoys[name] = globalDecoy;
                  elementalScopeDecoys[name] = scopeDecoy;
                  globalThis[name] = globalDecoy;
                  rustelScope[name] = scopeDecoy;
                }
                for (const name of ['nothing', 'silence']) {
                  delete globalThis[name];
                  delete rustelScope[name];
                }
                for (const name of ['Pattern', 'Hap']) {
                  const decoy = function poisonedElementalDependency() {
                    throw new Error(`mutable dependency consulted: ${name}`);
                  };
                  elementalGlobalDecoys[name] = decoy;
                  globalThis[name] = decoy;
                }
                globalThis.elementalGap = elementalCanonicals.gap(
                  elementalCanonicals.Fraction('2/6')
                );
                if (elementalGap._steps.show() !== '1/3') {
                  throw new Error('gap Fraction coercion');
                }
                "#,
            )
            .expect("install projected elemental product state");
        session
            .evaluate_prebake("__gc(); __gc();")
            .expect("collect between elemental setup and score turns");
        session
            .evaluate(
                r#"
                (() => {
                  const state = {
                    span: {
                      begin: elementalCanonicals.Fraction(0),
                      end: elementalCanonicals.Fraction(1)
                    },
                    controls: {}
                  };
                  const objectHaps = elementalPureObject.query(state);
                  const functionHaps = elementalPureFunction.query(state);
                  if (objectHaps.length !== 1
                      || objectHaps[0].value !== elementalObject
                      || functionHaps.length !== 1
                      || functionHaps[0].value !== elementalFunction) {
                    throw new Error('pure JS identity lost across setup GC');
                  }
                  if (elementalCanonicals.silence !== elementalSilence
                      || elementalCanonicals.nothing !== elementalNothing
                      || elementalCanonicals.silence === elementalCanonicals.nothing) {
                    throw new Error('empty singleton identity lost across turns');
                  }
                  for (const name of ['Fraction', 'gap', 'pure']) {
                    const globalDecoy = elementalGlobalDecoys[name];
                    const scopeDecoy = elementalScopeDecoys[name];
                    if (globalThis[name] !== globalDecoy
                        || globalDecoy.name !== 'poisonedElementalGlobal'
                        || rustelScope[name] !== scopeDecoy
                        || scopeDecoy.name !== 'poisonedElementalScope') {
                      throw new Error(`static destination was reinjected: ${name}`);
                    }
                  }
                  for (const name of ['nothing', 'silence']) {
                    if (Object.hasOwn(globalThis, name)
                        || Object.hasOwn(rustelScope, name)) {
                      throw new Error(`deleted static was reinjected: ${name}`);
                    }
                  }
                  for (const name of ['Pattern', 'Hap']) {
                    const decoy = elementalGlobalDecoys[name];
                    if (globalThis[name] !== decoy
                        || decoy.name !== 'poisonedElementalDependency') {
                      throw new Error(`lexical dependency was reinjected: ${name}`);
                    }
                  }
                  return elementalCanonicals.stack(
                    elementalCanonicals.stepcat(
                      elementalGap, elementalCanonicals.pure('afterGap')
                    ),
                    elementalCanonicals.stepcat(
                      elementalNothing, elementalCanonicals.pure('fromNothing')
                    ),
                    elementalCanonicals.stepcat(
                      elementalSilence, elementalCanonicals.pure('fromSilence')
                    ),
                    elementalPureObject.fmap(value => `object:${value.marker}`),
                    elementalPureFunction.fmap(value => value('pure'))
                  );
                })()
                "#,
            )
            .expect("score through retained elemental surface");

        let mut got = session
            .query(Fraction::ZERO, Fraction::ONE)
            .expect("query retained elemental score")
            .into_iter()
            .map(|hap| (hap.value.show(), hap.part.begin, hap.part.end))
            .collect::<Vec<_>>();
        got.sort_by(|left, right| left.0.cmp(&right.0));
        assert_eq!(
            got,
            [
                ("afterGap".to_owned(), Fraction::new(1, 4), Fraction::ONE,),
                ("fromNothing".to_owned(), Fraction::ZERO, Fraction::ONE,),
                ("fromSilence".to_owned(), Fraction::new(1, 2), Fraction::ONE,),
                ("object:7".to_owned(), Fraction::ZERO, Fraction::ONE,),
                ("owned:pure".to_owned(), Fraction::ZERO, Fraction::ONE,),
            ],
            "elemental route lost Fraction/gap timing, singleton steps, or JS identity"
        );
        assert_eq!(
            session.js.query_depth(),
            0,
            "elemental product leaked query roots"
        );
    }
}
