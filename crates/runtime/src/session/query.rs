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
        // Taken whether or not it is reported, so a following query starts
        // clean.
        let callback_failure = rustel_core::take_query_callback_failure();
        Ok(QueryReport {
            begin: fraction_label(begin),
            end: fraction_label(end),
            source: self.last_source.as_deref().unwrap_or_default().to_owned(),
            haps: haps_to_json(&haps),
            // Taken after the query so the report owns the message; a
            // following query starts clean.
            //
            // A query whose every hap a contained failure dropped is empty
            // because of the error: it says so, as a bounce does.
            query_threw: self
                .js
                .take_query_throw()
                .or(contained)
                .or_else(|| callback_failure.filter(|_| haps.is_empty())),
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
            session
                .js
                .active_pattern()
                .expect("active contract")
                .steps(),
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
}
