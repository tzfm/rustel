//! The wait of a live edit for its plugin copies.
//!
//! At the commit of a live edit the session plans the chains of the edit
//! from its notes near the cycle in play, and the plugin host builds each
//! copy the notes need: see [`crate::vst::plan`]. An edit with a copy that is
//! not ready waits a short time at the commit seam. Then the score in play is
//! in play again, and the session holds the text of the edit. When the copies
//! are ready, the caller asks again with the same text: the text evaluates a
//! second time and goes in play.

use super::*;

/// An edit waits for its plugin copies while one became ready in this
/// time. After it, the edit plays, and a note with no copy plays with no
/// plugin until its copy is ready. A caller that holds a start for its
/// plugin copies waits by the same rule: see [`Session::plugins_pending`].
pub const PLUGIN_WAIT: Duration = Duration::from_secs(20);
/// The longest wait at the commit seam. A light plugin builds one more copy
/// in this time, so the edit goes in play with no second evaluation.
const SEAM_WAIT: Duration = Duration::from_millis(150);
/// The time between 2 looks of the wait at the commit seam.
const SEAM_LOOK_EVERY: Duration = Duration::from_millis(2);
/// The cycles of a candidate the commit seam reads first: the cycle in play
/// and the next.
const SEAM_CYCLES: i64 = 2;
/// The cycles of a candidate the session reads for a launch, and at the
/// commit seam when a copy is not ready. The plan then has the lanes that
/// play later too.
const PLAN_CYCLES: i64 = 16;

/// An edit that waits for its plugin copies.
pub(super) struct PluginHold {
    /// The text of the edit.
    source: Arc<str>,
    /// The copies the notes of the edit need, the copies of the first notes
    /// first.
    wants: Vec<crate::vst::Want>,
    /// The count of the copies not ready, at the last look.
    pending: usize,
    /// The last time a copy became ready, or the start of the wait.
    progress: Instant,
    /// The last look at the copies.
    looked: Instant,
}

impl Session {
    /// [`Self::reload_with_clock_cancellable`] for a caller that follows the
    /// wait for the plugin copies. An edit with a copy that is not ready
    /// waits while the wait is on: see [`Self::set_plugin_wait`]. The answer
    /// is then [`LiveReload::Waiting`]: the score in play plays on, and the
    /// session holds the text. The caller asks again with the same text when
    /// [`Self::waits_for_plugins`] is false. The text then evaluates a second
    /// time and goes in play. A new text takes the place of the text that
    /// waits.
    pub fn reload_live<F>(
        &mut self,
        source: &str,
        mini: bool,
        cancellation: &AtomicBool,
        mut clock: F,
    ) -> Result<LiveReload, RuntimeError>
    where
        F: FnMut() -> f64,
    {
        if self.waits_for_plugins(source) {
            return Ok(LiveReload::Waiting);
        }
        let mut first_clock = Some(clock());
        let now = first_clock.unwrap_or_default();
        self.with_panic_recovery(now, |session| {
            session.reload_with_clock_cancellable_guarded(
                source,
                mini,
                cancellation,
                true,
                || first_clock.take().unwrap_or_else(&mut clock),
            )?;
            Ok(if session.holds_edit(source) {
                LiveReload::Waiting
            } else {
                LiveReload::Installed(session.generation())
            })
        })
    }

    /// Prepares the plugin copies of a text before it goes in play, for a
    /// launch on a cycle line. The text evaluates, the session plans its
    /// chains from its notes from `from_cycle`, and the plugin host builds
    /// each copy the notes need. Then the score in play is in play again:
    /// nothing installs. A launch from the start of the text gives cycle 0.
    /// A text with no pattern ends the plan of the text before.
    /// True when a copy is not ready and the wait is on. The session then
    /// holds the text as [`Self::reload_live`] does, and the launch waits
    /// while [`Self::waits_for_plugins`] is true. A wait of the same text
    /// that ended stays. The commit of the text ends it with one line.
    pub fn prepare_reload<F>(
        &mut self,
        source: &str,
        mini: bool,
        from_cycle: f64,
        cancellation: &AtomicBool,
        mut clock: F,
    ) -> Result<bool, RuntimeError>
    where
        F: FnMut() -> f64,
    {
        if self.waits_for_plugins(source) {
            return Ok(true);
        }
        // A mini score has no plugin, and a session with no live output
        // plans no chain.
        if mini || !self.plugins_live() {
            return Ok(false);
        }
        let now = clock();
        self.with_panic_recovery(now, |session| {
            session.prepare_reload_guarded(source, from_cycle, cancellation, now, &mut clock)
        })
    }

    fn prepare_reload_guarded<F>(
        &mut self,
        source: &str,
        from_cycle: f64,
        cancellation: &AtomicBool,
        now: f64,
        clock: &mut F,
    ) -> Result<bool, RuntimeError>
    where
        F: FnMut() -> f64,
    {
        Self::validate_live_score_clock("budget", now)?;
        let held = self
            .plugin_hold
            .take()
            .filter(|held| *held.source == *source);
        let evaluation_started = Instant::now();
        let evaluated = self.evaluate_score_cancellable(
            source,
            SCORE_CPU_BUDGET,
            cancellation,
            false,
            NoPatternPolicy::InstallSilence,
        );
        self.record_producer_phase(ProducerPhase::Evaluation, evaluation_started.elapsed());
        let evaluated = evaluated?;
        let wants = match &evaluated {
            EvaluatedScore::JavaScript {
                pattern, settings, ..
            } => {
                let cps = Self::evaluated_realtime_cps(&evaluated, self.scheduler.cps());
                // The evaluation took time from the cover of the score in
                // play. The read gets only the rest, as at the commit seam.
                let spare = self.seam_time(clock(), LIVE_REPLACEMENT_MIN_HEADROOM, None);
                let window = Self::live_replacement_probe_budget(cps).min(spare);
                let from = from_cycle.floor() as i64;
                let deadline = Instant::now() + window;
                let (haps, _) = self.candidate_haps(pattern, settings, from, PLAN_CYCLES, deadline);
                crate::vst::plan(&self.chains, source, &haps)
            }
            // A score with no pattern plays no plugin. It has no plan of its
            // own, so the plan of the text before ends, as at the commit seam.
            EvaluatedScore::MiniFallback(_) => {
                crate::vst::forget_plan(&self.chains);
                Vec::new()
            }
        };
        self.restore_score_in_play()?;
        let rate = self.plugin_rate.load(std::sync::atomic::Ordering::Relaxed);
        let pending = crate::vst::ensure(&wants, rate);
        let Some(first) = pending.first().map(|want| want.name().to_owned()) else {
            return Ok(false);
        };
        let pending = pending.len();
        // A wait of this text that ended stays as it is. The commit of the
        // text ends it with its one line, and the text plays.
        let ended = held
            .as_ref()
            .is_some_and(|held| held.pending > 0 && held.progress.elapsed() >= PLUGIN_WAIT);
        if ended {
            self.plugin_hold = held;
            return Ok(false);
        }
        Ok(self.plugin_wait && self.hold_edit(source, wants, pending, &first, held))
    }

    /// True while the edit with the text `source` waits for its plugin
    /// copies: see [`Self::reload_live`]. The edit waits while a copy is not
    /// ready and one became ready in the last [`PLUGIN_WAIT`]. A call asks
    /// the plugin host again when [`PLUGIN_LOOK_EVERY`] passed since the
    /// last look. False with the wait off or a stopped transport.
    pub fn waits_for_plugins(&mut self, source: &str) -> bool {
        if !self.plugin_wait || self.transport.is_stopped() {
            return false;
        }
        let rate = self.plugin_rate.load(std::sync::atomic::Ordering::Relaxed);
        let Some(hold) = self
            .plugin_hold
            .as_mut()
            .filter(|hold| *hold.source == *source)
        else {
            return false;
        };
        let now = Instant::now();
        if now.duration_since(hold.looked) >= PLUGIN_LOOK_EVERY {
            let pending = crate::vst::ensure(&hold.wants, rate).len();
            if pending < hold.pending {
                hold.progress = now;
            }
            (hold.pending, hold.looked) = (pending, now);
        }
        hold.pending > 0 && now.duration_since(hold.progress) < PLUGIN_WAIT
    }

    /// Ends the wait of the edit that waits for its plugin copies: its text
    /// does not go in play. The plugin host ends the copies only that text
    /// needed. A new text needs no cancel: it takes the place of the text
    /// that waits, and its plan starts from the plan of that text. A text
    /// with no plan ends that plan.
    pub fn cancel_plugin_hold(&mut self) {
        self.plugin_hold = None;
        crate::vst::forget_plan(&self.chains);
    }

    /// Whether an edit waits for its plugin copies. On at the start. With the
    /// wait off, an edit goes in play at once, and a note plays with no
    /// plugin until its copy is ready. An edit that waits goes in play at
    /// the next call of its door.
    pub fn set_plugin_wait(&mut self, on: bool) {
        self.plugin_wait = on;
    }

    /// True when the session holds the edit with the text `source`.
    pub(super) fn holds_edit(&self, source: &str) -> bool {
        let hold = self.plugin_hold.as_ref();
        hold.is_some_and(|hold| *hold.source == *source)
    }

    /// The plugin copies the edit with the text `source` waits for: the
    /// count not ready at the last look, and the count in all. `None` when
    /// the session holds no edit with this text. The call asks the plugin
    /// host nothing.
    pub fn plugins_waited_for(&self, source: &str) -> Option<(usize, usize)> {
        let hold = self.plugin_hold.as_ref();
        let hold = hold.filter(|hold| *hold.source == *source)?;
        Some((hold.pending, hold.wants.len()))
    }

    /// The commit seam of a live candidate, between its probe and its
    /// commit. The session plans the chains of the candidate from its notes
    /// near the cycle in play, and the plugin host builds each copy the notes
    /// need. True when the candidate goes on to its commit.
    ///
    /// The seam reads and waits only while the cover of the score in play
    /// lasts, and before the line of a launch, so a launch keeps its line. A
    /// launch with no time before its line misses the line in any case, and
    /// its first read takes the cover. With `hold` and the wait on, a
    /// candidate with a copy that is not ready waits: the plan reads more
    /// cycles, and the seam waits a short time. The 2 reads get the budget
    /// of a probe together. So the reads and the short wait hold the caller
    /// for that budget and [`SEAM_WAIT`] at most. Then the score in play is
    /// in play again, and the session holds the text. Nothing waits for a
    /// first score, a score with no pattern, a stopped transport or a
    /// session with no live output. Such a candidate makes no plan, and the
    /// plan of the text before ends.
    pub(super) fn plugin_seam<F>(
        &mut self,
        evaluated: &EvaluatedScore,
        source: &str,
        hold: bool,
        realtime_cps: Option<f64>,
        probe_elapsed: Duration,
        clock: &mut F,
    ) -> Result<bool, RuntimeError>
    where
        F: FnMut() -> f64,
    {
        // A new text takes the place of the text that waits.
        let held = self
            .plugin_hold
            .take()
            .filter(|held| *held.source == *source);
        let EvaluatedScore::JavaScript {
            pattern, settings, ..
        } = evaluated
        else {
            // A text with no plan of its own ends the plan of the text
            // before.
            crate::vst::forget_plan(&self.chains);
            return Ok(true);
        };
        if !self.plugins_live() || self.generation() == 0 || self.transport.is_stopped() {
            crate::vst::forget_plan(&self.chains);
            return Ok(true);
        }
        let started = Instant::now();
        let seam_now = clock();
        let headroom = Self::live_replacement_takeover_headroom(realtime_cps, probe_elapsed);
        // A launch takes over on its line, and a rewind plays from the
        // first cycle of its score.
        let line = self.takeover_override.filter(|line| line.is_finite());
        // The seam ends before the takeover of the commit leaves the cover
        // of the score in play, and before the line of a launch.
        let spare = self.seam_time(seam_now, headroom, line);
        // A launch with no time before its line misses the line in any
        // case. Its first read then takes the cover, so a copy that is not
        // ready holds the launch.
        let first_spare = if spare.is_zero() {
            self.seam_time(seam_now, headroom, None)
        } else {
            spare
        };
        let from = if self.next_from_zero {
            0
        } else {
            self.scheduler
                .cycle_at_time(line.unwrap_or(seam_now))
                .floor() as i64
        };
        // The first cycles get about the time of 2 probes. The read stops at
        // the compute share of the seam time, so the plan and the look end
        // before the line too.
        let budget = Self::live_replacement_probe_budget(realtime_cps);
        let window = (probe_elapsed * 2 + Duration::from_millis(5))
            .max(Duration::from_millis(15))
            .min(budget)
            .min(first_spare.mul_f64(LIVE_REPLACEMENT_QUERY_COMPUTE_SHARE));
        let deadline = started + window;
        let (mut haps, next) = self.candidate_haps(pattern, settings, from, SEAM_CYCLES, deadline);
        let mut wants = crate::vst::plan(&self.chains, source, &haps);
        let rate = self.plugin_rate.load(std::sync::atomic::Ordering::Relaxed);
        // The count of the copies not ready, and the plugin of the first.
        let look = |wants: &[crate::vst::Want]| {
            let pending = crate::vst::ensure(wants, rate);
            let first = pending.first();
            first.map(|want| (pending.len(), want.name().to_owned()))
        };
        let mut pending = look(&wants);
        if pending.is_none() || !(hold && self.plugin_wait) {
            return Ok(true);
        }
        // A launch with no time before its line holds at once and takes a
        // later line.
        let until = seam_now + spare.as_secs_f64() - SEAM_LOOK_EVERY.as_secs_f64();
        if clock() < until {
            // The plan reads more cycles, so it has the lanes that play
            // later too. It starts from the plan before, and each chain
            // keeps its number. The read goes on at the cycle where the
            // first read stopped. The 2 reads get the budget of a probe, as
            // the read of a launch does: a shield makes the seam time long,
            // and the caller makes no other turn in the seam. The read also
            // stops at the compute share of the seam time. The plan, the
            // look and the restore take the rest.
            let more = from + PLAN_CYCLES - next;
            let share = spare.mul_f64(LIVE_REPLACEMENT_QUERY_COMPUTE_SHARE);
            let deadline = started + budget.min(share);
            let (later, _) = self.candidate_haps(pattern, settings, next, more, deadline);
            haps.extend(later);
            wants = crate::vst::plan(&self.chains, source, &haps);
            pending = look(&wants);
        }
        // A light plugin builds a copy in some ms.
        let waited = Instant::now();
        while pending.is_some()
            && waited.elapsed() < SEAM_WAIT
            && clock() < until
            && !self.transport.is_stopped()
        {
            std::thread::sleep(SEAM_LOOK_EVERY);
            pending = look(&wants);
        }
        let Some((pending, first)) = pending else {
            return Ok(true);
        };
        if !self.hold_edit(source, wants, pending, &first, held) {
            return Ok(true);
        }
        self.restore_score_in_play()?;
        Ok(false)
    }

    /// Holds the text of an edit with `pending` copies not ready, the copy
    /// of the plugin `first` among them. `held` is the wait of the same text
    /// before, as the look of its door saw it: see
    /// [`Self::waits_for_plugins`]. A wait with a copy not ready keeps the
    /// last time a copy became ready. A wait with each copy ready is over,
    /// so a copy not ready now starts a new wait. False when the wait ends:
    /// no copy became ready in [`PLUGIN_WAIT`]. The edit then plays, with
    /// one line. A text gives one line at the start of its wait.
    fn hold_edit(
        &mut self,
        source: &str,
        wants: Vec<crate::vst::Want>,
        pending: usize,
        first: &str,
        held: Option<PluginHold>,
    ) -> bool {
        let now = Instant::now();
        let progress = match &held {
            Some(held) if held.pending > 0 => held.progress,
            _ => now,
        };
        if now.duration_since(progress) >= PLUGIN_WAIT {
            let message = format!(
                "plugin \"{first}\" is still loading after {} s: the update plays without it until it is ready",
                PLUGIN_WAIT.as_secs()
            );
            let record = serde_json::json!({ "vst_skipped": { "message": &message } });
            self.report_plugin_line(message, record);
            return false;
        }
        if held.is_none() {
            let message = format!("plugin \"{first}\" is still loading");
            let record = serde_json::json!({ "vst_loading": { "message": &message } });
            self.report_diagnostic(SAMPLE_AWAITED_DIAGNOSTIC, message, record);
        }
        self.plugin_hold = Some(PluginHold {
            source: Arc::from(source),
            wants,
            pending,
            progress,
            looked: now,
        });
        true
    }

    /// The longest time at the commit seam from `now`. The takeover of the
    /// commit comes the margin of the output or `headroom` after the commit,
    /// and stays inside the cover of the score in play. With `line`, the
    /// line of a launch, the takeover comes at the line or before it.
    fn seam_time(&self, now: f64, headroom: Duration, line: Option<f64>) -> Duration {
        let margin = self.continuity_margin.unwrap_or(self.schedule_lead);
        let margin = margin.max(headroom.as_secs_f64());
        let cover = self.scheduler.horizon_remaining(now);
        let cover = line.map_or(cover, |line| cover.min(line - now));
        let spare = cover - margin;
        Duration::try_from_secs_f64(spare.max(0.0)).unwrap_or(Duration::ZERO)
    }

    /// The notes of a live candidate from cycle `from`, one cycle at a time
    /// and `cycles` at most, as its probe queries them. The query stops at
    /// the first cycle that throws or does not end before `deadline`. Each
    /// cycle converts before the next query, so the deadline holds the
    /// conversion too. Gives the notes and the next cycle to read. These
    /// cycles do not play now, so their console lines go.
    fn candidate_haps(
        &self,
        pattern: &Pattern,
        settings: &RuntimeSettings,
        from: i64,
        cycles: i64,
        deadline: Instant,
    ) -> (Vec<crate::HapJson>, i64) {
        let budget = self.scheduler.query_hap_budget();
        let mut haps = Vec::new();
        let mut next = from;
        self.js.with_logs_dropped(|| {
            for cycle in from..from + cycles {
                let begin = Fraction::int(i128::from(cycle));
                let state = State::new(TimeSpan::new(begin, begin.add(Fraction::ONE)));
                let query = || {
                    rustel_core::with_query_deadline(deadline, || {
                        pattern.query_arc_outcome_with_budget(&state, budget)
                    })
                };
                let queried = if pattern.is_pure() {
                    settings.with(query).ok()
                } else {
                    let left = deadline.saturating_duration_since(Instant::now());
                    let cancellation = self.transport.stopped_flag();
                    let scoped = || {
                        self.js.with_contained_query_errors_unlogged(|| {
                            self.js
                                .with_active_scope_cancellable(left, cancellation, query)
                        })
                    };
                    settings.with(scoped).ok().and_then(Result::ok)
                };
                // A stack keeps the other lanes of a lane that throws.
                let _ = rustel_core::take_query_contained_throw();
                let Some(QueryArcOutcome::Haps(found)) = queried else {
                    break;
                };
                haps.extend(haps_to_json(&found));
                next = cycle + 1;
            }
        });
        (haps, next)
    }

    /// Puts the score in play in play again after a candidate that does not
    /// go in play. The visuals of the candidate go too.
    fn restore_score_in_play(&mut self) -> Result<(), RuntimeError> {
        #[cfg(feature = "hydra")]
        {
            self.staged_hydra = None;
        }
        self.js
            .restore_last_good_active()
            .map_err(|error| RuntimeError::Js(error.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::fixture_plugins;
    use super::*;
    use crate::test_support::past_the_cover;

    /// A wait ends when no copy became ready in 20 s, and the edit plays
    /// with one line. A prepare of the same text keeps the wait that ended.
    /// A wait with each copy ready is over, so a copy the landing needs that
    /// is not ready starts a new wait.
    #[test]
    fn a_wait_ends_after_20_s_with_no_new_copy() {
        let _plugins = fixture_plugins();
        let host = crate::vst::host();
        // The load of the fixture takes 600 ms, so the copy of the edit is
        // still not ready at the landing.
        host.wait_idle();
        // SAFETY: one fixture test runs at a time, and the plugin thread has
        // no work. So no other thread reads the variable now.
        unsafe { std::env::set_var(rustel_vst3_fixture::SLOW_ENV, "600") };
        let mut session = Session::new().expect("session");
        session.set_direct_diagnostic_logging(false);
        let playing = r#"$: s("sine").gain(0)"#;
        session.evaluate(playing).expect("score in play");
        let relaxed = std::sync::atomic::Ordering::Relaxed;
        session.plugins_live.store(true, relaxed);
        session.plugin_rate.store(48_000, relaxed);
        let stopped = AtomicBool::new(false);
        let edit = format!("{playing}\n$: note(\"c3\").s(\"sine\").vst(\"rustel fixture\")");
        session.schedule_through(0.1, 0.1).expect("cover");
        let generation = session.generation();
        let reload = session.reload_live(&edit, false, &stopped, past_the_cover(0.15));
        assert_eq!(reload.expect("edit"), LiveReload::Waiting);
        let hold = session.plugin_hold.as_mut().expect("the wait");
        hold.progress = hold.progress.checked_sub(PLUGIN_WAIT).expect("uptime");
        hold.looked = Instant::now();
        assert!(!session.waits_for_plugins(&edit));
        session.schedule_through(0.2, 0.2).expect("cover");
        let plugin_lines = |session: &mut Session| {
            let lines = session.take_diagnostics().into_iter();
            let lines = lines.filter(|line| line.kind == PLUGIN_DIAGNOSTIC);
            lines.map(|line| line.message).collect::<Vec<String>>()
        };
        let prepared = session.prepare_reload(&edit, false, 0.0, &stopped, || 0.2);
        assert!(!prepared.expect("prepare after the end"));
        assert!(plugin_lines(&mut session).is_empty());
        let landed = session.reload_live(&edit, false, &stopped, || 0.25);
        assert_eq!(
            landed.expect("landing"),
            LiveReload::Installed(generation + 1)
        );
        let late = "plugin \"rustel fixture\" is still loading after 20 s: \
                    the update plays without it until it is ready";
        assert_eq!(plugin_lines(&mut session), [late]);
        host.wait_idle();
        // SAFETY: as above.
        unsafe { std::env::remove_var(rustel_vst3_fixture::SLOW_ENV) };

        // A wait with each copy ready is over. A copy the landing needs that
        // is not ready starts a new wait, also 20 s after the last copy.
        let part =
            |notes: &str| format!("\n$: note(\"{notes}\").s(\"sine\").vst(\"rustel fixture\")");
        let sections = format!("{edit}{}{}", part("<c4!16 ~!16>"), part("<~!16 c5!16>"));
        session.schedule_through(0.3, 0.3).expect("cover");
        let prepared = session.prepare_reload(&sections, false, 16.0, &stopped, || 0.3);
        assert!(prepared.expect("prepare"));
        host.wait_idle();
        let hold = session.plugin_hold.as_mut().expect("the wait");
        hold.pending = 0;
        hold.progress = hold.progress.checked_sub(PLUGIN_WAIT).expect("uptime");
        hold.looked = Instant::now();
        let landing = session.reload_live(&sections, false, &stopped, past_the_cover(0.35));
        assert_eq!(landing.expect("landing"), LiveReload::Waiting);
        session.cancel_plugin_hold();
    }
}
