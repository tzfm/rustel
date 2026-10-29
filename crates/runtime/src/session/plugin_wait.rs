//! The wait of a live edit for its plugin copies.
//!
//! At the commit of a live edit the session plans the chains of the edit
//! from its notes near the cycle in play, and the plugin host builds each
//! copy the notes need: see [`crate::vst::plan`]. An edit with a copy that is
//! not ready waits a short time at the commit seam. Then the score in play is
//! in play again, and the session holds the text of the edit. When the copies
//! are ready, the caller asks again with the same text: the text evaluates a
//! second time and goes in play.
//!
//! The lanes that do not play, the muted lanes and the lanes a solo drops,
//! load their copies too, and nothing waits for them. While the score plays,
//! the session looks at the cycles ahead, so a copy is ready before the first
//! note of its lane: see [`Session::look_ahead_for_plugins`].

use super::*;

/// An edit waits for its plugin copies while one became ready in this
/// time. After it, the edit plays, and a note with no copy plays with no
/// plugin until its copy is ready. A caller that holds a start for its
/// plugin copies waits by the same rule: see [`Session::plugins_pending`].
pub const PLUGIN_WAIT: Duration = Duration::from_secs(20);
/// The longest wait at the commit seam. A light plugin builds one more copy
/// in this time, so the edit goes in play with no second evaluation. With
/// the wait off, a caller holds the first window of a new output, or a
/// start, this long at most for the copies on their way home: see
/// [`Session::plugins_returning`].
pub const SEAM_WAIT: Duration = Duration::from_millis(150);
/// The time between 2 looks of the wait at the commit seam.
const SEAM_LOOK_EVERY: Duration = Duration::from_millis(2);
/// The cycles of a candidate the commit seam reads first: the cycle in play
/// and the next.
const SEAM_CYCLES: i64 = 2;
/// The cycles of a candidate the session reads for a launch, and at the
/// commit seam when a copy is not ready. The plan then has the lanes that
/// play later too. The looks at the score in play go this far ahead of the
/// cycle in play.
const PLAN_CYCLES: i64 = 16;
/// The longest look at the cycles ahead. A look runs on the producer thread
/// in a steady turn with its cover full, so it gets the floor of a live
/// query. The cover then stays for the next turn, and a launch keeps its
/// line.
const LOOK_MOST: Duration = MIN_LIVE_QUERY_JS_BUDGET;
/// A look reads one cycle, or a half, a quarter and on, down to 1/1024 of a
/// cycle.
const LOOK_HALVINGS: u32 = 10;

/// An edit that waits for its plugin copies, or a launch that its arm
/// prepared: see [`Session::prepare_reload`].
pub(super) struct PluginHold {
    /// The text of the edit or of the launch.
    source: Arc<str>,
    /// The copies the notes of the text need, the copies of the first notes
    /// first, and the copies of the lanes that do not play last.
    wants: Vec<crate::vst::Want>,
    /// The count of the copies not ready, at the last look.
    pending: usize,
    /// The last time a copy became ready, the start of the wait, or the last
    /// press of the text in another document: see
    /// [`Session::adopt_plugin_hold`].
    progress: Instant,
    /// The last look at the copies.
    looked: Instant,
}

impl PluginHold {
    /// The lead of the plugin chains of the lanes of the text that play, in
    /// seconds, at the rate `rate`: see [`crate::vst::lead`]. Each call asks
    /// the plugin host again, so a copy that became ready after the hold
    /// gives its latency. See [`Session::takeover_headroom`].
    pub(super) fn lead(&self, rate: u32) -> f64 {
        let playing = self.wants.iter().take_while(|want| want.sounds()).count();
        crate::vst::lead(&self.wants[..playing], rate)
    }
}

/// The looks at the cycles ahead of the score in play: see
/// [`Session::look_ahead_for_plugins`].
#[derive(Default)]
pub(super) struct PluginAhead {
    /// The start of the part the next look at the lanes that play reads, in
    /// cycles. `None`: the next look reads from the cycle in play.
    next: Option<Fraction>,
    /// The start of the part the next look at the lanes that do not play
    /// reads, as `next`. These looks come when `next` is [`PLAN_CYCLES`]
    /// cycles ahead of the cycle in play.
    shadow: Option<Fraction>,
    /// The cycle in play at the last look.
    play: i64,
    /// The rate of the output at the last look.
    rate: u32,
    /// The query time of one cycle, as the last turn of the producer with a
    /// query measured it.
    turn: Duration,
    /// The query time of one cycle of the lanes that play, as the last look
    /// at them measured it on its part. A look that did not end gives the
    /// time it had: the cycle needs more.
    look: Duration,
    /// The query time of one cycle of the lanes that do not play, as
    /// `look`.
    shadow_look: Duration,
    /// A look at the lanes that play reads one cycle, halved this many
    /// times. A look at [`LOOK_MOST`] that does not end halves the part of
    /// the next look at the same lanes. A look that ends in a quarter of its
    /// time doubles it. The count stays for a new score, as the query times
    /// do.
    halvings: u32,
    /// The halvings of a look at the lanes that do not play, as
    /// `halvings`.
    shadow_halvings: u32,
    /// The next look at the lanes that play comes at this time or later.
    due: Option<Instant>,
    /// The time of the next look at the lanes that do not play, as `due`.
    shadow_due: Option<Instant>,
    /// The time of the look in the last turn of the producer, or 0, until
    /// the producer takes it: see [`Session::take_look_ahead_time`].
    spent: Duration,
}

impl PluginAhead {
    /// The next looks read from the cycle in play: the score or the output
    /// is new.
    pub(super) fn restart(&mut self) {
        (self.next, self.shadow) = (None, None);
    }

    /// The part of a cycle the next look reads, from its start to its end,
    /// with `play` the cycle in play and `rate` the rate of the output, and
    /// true for a look at the lanes that do not play. The lanes that play
    /// come first. `None` when the looks are [`PLAN_CYCLES`] cycles ahead,
    /// or when the next look waits.
    fn part(&mut self, play: i64, rate: u32) -> Option<(Fraction, Fraction, bool)> {
        let start = Fraction::int(i128::from(play));
        // A new rate needs new copies, and a cycle in play that went back
        // brings cycles no look read.
        let goes_on = self.rate == rate && play >= self.play;
        let from = |next: Option<Fraction>| match next {
            Some(next) if goes_on => next.max(start),
            _ => start,
        };
        let (next, shadow_next) = (from(self.next), from(self.shadow));
        (self.next, self.shadow) = (Some(next), Some(shadow_next));
        (self.play, self.rate) = (play, rate);
        let ahead = Fraction::int(i128::from(play + PLAN_CYCLES));
        let (begin, halvings, shadow, due) = if next < ahead {
            (next, self.halvings, false, self.due)
        } else {
            (shadow_next, self.shadow_halvings, true, self.shadow_due)
        };
        let waits = due.is_some_and(|due| Instant::now() < due);
        let end = begin.add(Fraction::new(1, 1 << halvings));
        (begin < ahead && !waits).then_some((begin, end, shadow))
    }

    /// The time of a look: twice the query time of its part, as the last
    /// turn or the last look at the same lanes measured it, and
    /// [`LOOK_MOST`] at most. `shadow`: a look at the lanes that do not
    /// play.
    fn budget(&self, shadow: bool) -> Duration {
        let (look, halvings) = if shadow {
            (self.shadow_look, self.shadow_halvings)
        } else {
            (self.look, self.halvings)
        };
        (self.turn.max(look) * 2 / (1_u32 << halvings)).min(LOOK_MOST)
    }

    /// A look up to `end` had `budget`, took `took`, and its read ended or
    /// not. `shadow`: the look read the lanes that do not play. A look that
    /// does not end leaves its part to the next look at the same lanes,
    /// with twice the time. At [`LOOK_MOST`] that next look reads half the
    /// part, and waits as long as this one took for each part of a cycle.
    /// After a look that ends, the next look at the same lanes waits as long
    /// as this one took.
    fn looked(
        &mut self,
        end: Fraction,
        shadow: bool,
        budget: Duration,
        took: Duration,
        ended: bool,
    ) {
        let (next, look, halvings, due) = if shadow {
            (
                &mut self.shadow,
                &mut self.shadow_look,
                &mut self.shadow_halvings,
                &mut self.shadow_due,
            )
        } else {
            (
                &mut self.next,
                &mut self.look,
                &mut self.halvings,
                &mut self.due,
            )
        };
        let parts = 1_u32 << *halvings;
        let mut wait = took;
        if ended {
            (*next, *look) = (Some(end), took * parts);
            if took < budget / 4 {
                *halvings = halvings.saturating_sub(1);
            }
        } else {
            *look = (*look).max(budget * parts);
            if budget >= LOOK_MOST {
                *halvings = (*halvings + 1).min(LOOK_HALVINGS);
                wait = took * (1_u32 << *halvings);
            }
        }
        *due = Some(Instant::now() + wait);
    }
}

impl Session {
    /// [`Self::reload_with_clock_cancellable`] for a caller that follows the
    /// wait for the plugin copies. An edit with a copy that is not ready
    /// waits while the wait is on: see [`Self::set_plugin_wait`]. The answer
    /// is then [`LiveReload::Waiting`]: the score in play plays on, and the
    /// session holds the text. The caller asks again with the same text when
    /// [`Self::waits_for_plugins`] is false. The text then evaluates a second
    /// time and goes in play. A new text takes the place of the text that
    /// waits. The same text in another document takes its place too, and
    /// evaluates in that document with the read and the plan of the hold:
    /// see [`Self::adopt_plugin_hold`].
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
        let adopted = self.adopt_plugin_hold(source);
        if !adopted && self.waits_for_plugins(source) {
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
    /// each copy the notes need, the copies of the lanes that do not play
    /// last. Each copy the host cannot make gives its reason as a plugin
    /// line. Then the score in play is in play again: nothing installs. A
    /// launch from the start of the text gives cycle 0. A text with no
    /// pattern ends the plan of the text before.
    /// The session keeps the plan and the copies of the text until the
    /// text goes in play, also with each copy ready. The commit seam of the
    /// launch looks at them when no cycle fits before its line: see
    /// [`Self::plugin_seam`]. A new text, a cancel or the commit ends them.
    /// The launch fires the lead of the plan earlier: see
    /// [`Self::takeover_headroom`].
    /// The same text in another document keeps them, and evaluates in that
    /// document: see [`Self::adopt_plugin_hold`].
    /// True when a copy of a lane that plays is not ready and the wait is
    /// on. The session then holds the text as [`Self::reload_live`] does,
    /// and the launch waits while [`Self::waits_for_plugins`] is true. A
    /// wait of the same text in the same document that ended stays. The
    /// commit of the text ends it with one line. In another document the
    /// wait counts again from the arm: see [`Self::adopt_plugin_hold`].
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
        let adopted = self.adopt_plugin_hold(source);
        if !adopted && self.waits_for_plugins(source) {
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
        let rate = self.plugin_rate.load(std::sync::atomic::Ordering::Relaxed);
        let held_lead = held.as_ref().map_or(0.0, |held| held.lead(rate));
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
                let spare = self.seam_time(clock(), LIVE_REPLACEMENT_MIN_HEADROOM, None, held_lead);
                let window = Self::live_replacement_probe_budget(cps).min(spare);
                let from = from_cycle.floor() as i64;
                let deadline = Instant::now() + window;
                let (haps, next) =
                    self.candidate_haps(pattern, settings, from, PLAN_CYCLES, deadline);
                // The lanes that do not play get the rest of the time.
                let shadow = self.candidate_shadow(settings, from, next, deadline);
                crate::vst::plan(
                    &self.chains,
                    &self.lanes,
                    &self.csound,
                    source,
                    &haps,
                    &shadow,
                )
            }
            // A score with no pattern plays no plugin. It has no plan of its
            // own, so the plan of the text before ends, as at the commit seam.
            EvaluatedScore::MiniFallback(_) => {
                crate::vst::forget_plan(&self.chains);
                Vec::new()
            }
        };
        self.restore_score_in_play()?;
        let pending = self.ensure_wants(&wants);
        let first = pending.first().map(|want| want.name().to_owned());
        let pending = pending.len();
        // A wait of this text that ended stays as it is. The commit of the
        // text ends it with its one line, and the text plays.
        let ended = held
            .as_ref()
            .is_some_and(|held| held.pending > 0 && held.progress.elapsed() >= PLUGIN_WAIT);
        match first {
            Some(_) if ended => {
                self.lanes.hold(Some(source));
                self.plugin_hold = held;
                Ok(false)
            }
            Some(first) if self.plugin_wait => {
                Ok(self.hold_edit(source, wants, pending, &first, held))
            }
            // Nothing waits. The fire of the launch looks at the copies, and
            // comes the lead of the plan earlier.
            _ => {
                let now = Instant::now();
                self.lanes.hold(Some(source));
                self.plugin_hold = Some(PluginHold {
                    source: Arc::from(source),
                    wants,
                    pending,
                    progress: now,
                    looked: now,
                });
                Ok(false)
            }
        }
    }

    /// True while the edit with the text `source` waits for its plugin
    /// copies: see [`Self::reload_live`]. The edit waits while a copy is not
    /// ready and one became ready in the last [`PLUGIN_WAIT`]. A copy not
    /// ready after a look with each copy ready starts a new wait, with one
    /// line. A call asks the plugin host again when [`PLUGIN_LOOK_EVERY`]
    /// passed since the last look. The latency of the copies a look finds
    /// raises the lead the cover keeps room for: see [`Self::plugin_lead`].
    /// False with the wait off or a stopped transport.
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
        let mut first = None;
        let mut lead = 0.0;
        if now.duration_since(hold.looked) >= PLUGIN_LOOK_EVERY {
            let (pending, _) = crate::vst::ensure(&hold.wants, rate);
            if hold.pending == 0 {
                first = pending.first().map(|want| want.name().to_owned());
            }
            if pending.len() < hold.pending || hold.pending == 0 {
                hold.progress = now;
            }
            (hold.pending, hold.looked) = (pending.len(), now);
            lead = hold.lead(rate);
        }
        let waits = hold.pending > 0 && now.duration_since(hold.progress) < PLUGIN_WAIT;
        if let Some(first) = first {
            self.report_plugin_wait(&first);
        }
        // A copy that became ready gives its latency. The cover keeps room
        // for the lead of the held text, as at the hold.
        self.raise_cover_lead(lead);
        waits
    }

    /// Ends the wait of the edit that waits for its plugin copies, and the
    /// plan of a launch that its arm prepared: the text does not go in play.
    /// The plugin host ends the copies only that text needed. A new text
    /// needs no cancel: it takes the place of the text that waits, and its
    /// plan starts from the plan of that text. A text with no plan ends that
    /// plan.
    pub fn cancel_plugin_hold(&mut self) {
        self.plugin_hold = None;
        self.lanes.hold(None);
        crate::vst::forget_plan(&self.chains);
    }

    /// Whether an edit waits for its plugin copies. On at the start. With the
    /// wait off, an edit goes in play at once, and a note plays with no
    /// plugin until its copy is ready. An edit that waits goes in play at
    /// the next call of its door. With the wait on, the first window of a
    /// new output waits for the copies its close sent home, until no copy
    /// became ready in [`PLUGIN_WAIT`]. With the wait off, it waits
    /// [`SEAM_WAIT`] at most. The first install of a producer that never
    /// published waits for its copies only with the wait on.
    pub fn set_plugin_wait(&mut self, on: bool) {
        self.plugin_wait = on;
    }

    /// True when the wait is on: see [`Self::set_plugin_wait`].
    pub(crate) fn plugin_wait(&self) -> bool {
        self.plugin_wait
    }

    /// True when the session holds the edit with the text `source`.
    pub(super) fn holds_edit(&self, source: &str) -> bool {
        let hold = self.plugin_hold.as_ref();
        hold.is_some_and(|hold| *hold.source == *source)
    }

    /// The same text in another document takes the place of the text that
    /// waits, with its read and its plan: see [`crate::vst::adopt_plan`].
    /// True when the read of the text was of another document. The door
    /// then evaluates the text in that document. The lanes of that document
    /// come first, then the lanes of the read of the hold, and the plan
    /// starts from the plan of the hold. So the text waits only for a copy
    /// it plays, and no copy builds again. The wait counts from the press,
    /// with no second line. So a copy of a lane of that document gets the
    /// whole [`PLUGIN_WAIT`], also a copy the hold did not wait for. The
    /// plan moves only with its read, and the read of the hold moves with
    /// them. A read of another text, such as a text that failed, leaves the
    /// read of the hold: see [`crate::vst::LaneTable::hold`]. So an
    /// evaluation that fails after the move leaves the read and the plan in
    /// the document of the move, and the next door of the text takes both
    /// from there. The caller sets the document of the text before each
    /// door: see [`Self::set_plugin_document`].
    fn adopt_plugin_hold(&mut self, source: &str) -> bool {
        let adopted = self.holds_edit(source) && self.lanes.adopt_read(source);
        if adopted {
            crate::vst::adopt_plan(&self.chains, &self.lanes, source);
            self.lanes.hold(Some(source));
            if let Some(hold) = self.plugin_hold.as_mut() {
                hold.progress = Instant::now();
            }
        }
        adopted
    }

    /// The plugin copies the edit or the launch with the text `source` waits
    /// for: the count not ready at the last look, and the count in all. A
    /// copy of a lane that does not play does not count. `None` when the
    /// session holds no edit and keeps no launch with this text: see
    /// [`Self::prepare_reload`]. The call asks the plugin host nothing.
    pub fn plugins_waited_for(&self, source: &str) -> Option<(usize, usize)> {
        let hold = self.plugin_hold.as_ref();
        let hold = hold.filter(|hold| *hold.source == *source)?;
        let sounding = hold.wants.iter().filter(|want| want.sounds());
        Some((hold.pending, sounding.count()))
    }

    /// The commit seam of a live candidate, between its probe and its
    /// commit. The session plans the chains of the candidate from its notes
    /// near the cycle in play, and the plugin host builds each copy the notes
    /// need. The notes of the lanes that do not play come after the others,
    /// also after the cycles the seam reads more: their copies load, and
    /// nothing waits for them. Gives the lines of the copies the host cannot
    /// make when the candidate goes on to its commit: the commit forgets each
    /// plugin line said before, so the caller says these after the commit.
    /// `None` when the session holds the text: the lines go out at once.
    ///
    /// The seam reads and waits only while the cover of the score in play
    /// lasts, and before the line of a launch, so a launch keeps its line. A
    /// launch with no time before its line misses the line in any case.
    /// When no cycle fits before the line, the copies the session keeps for
    /// the text serve, with their plan: the copies of the arm of the launch,
    /// or of an earlier hold of the text. See [`Self::prepare_reload`]. The
    /// seam then reads no more. With no kept copies, the first read of a
    /// launch with no time before its line takes the cover. With `hold` and
    /// the wait on, a candidate with a copy that is not ready waits: the
    /// plan reads more cycles, and the seam waits a short time. The 2 reads
    /// get the budget of a probe together. So the reads and the short wait
    /// hold the caller for that budget and [`SEAM_WAIT`] at most. Then the
    /// score in play is in play again, and the session holds the text.
    /// Nothing waits for a first score, a score with no pattern, a stopped
    /// transport or a session with no live output. Such a candidate makes no
    /// plan, and the plan of the text before ends.
    ///
    /// The commit of the candidate takes the lead of the plugin chains of
    /// its plan, and a candidate with no plan has no lead: see
    /// [`Self::plugin_lead`]. The reads and the wait keep room for the lead
    /// in the cover of the score in play.
    pub(super) fn plugin_seam<F>(
        &mut self,
        evaluated: &EvaluatedScore,
        source: &str,
        hold: bool,
        realtime_cps: Option<f64>,
        probe_elapsed: Duration,
        clock: &mut F,
    ) -> Result<Option<Vec<String>>, RuntimeError>
    where
        F: FnMut() -> f64,
    {
        // A new text takes the place of the text that waits.
        let mut held = self
            .plugin_hold
            .take()
            .filter(|held| *held.source == *source);
        let EvaluatedScore::JavaScript {
            pattern, settings, ..
        } = evaluated
        else {
            // A text with no plan of its own ends the plan of the text
            // before, and has no lead.
            crate::vst::forget_plan(&self.chains);
            self.plan_lead = Some(0.0);
            return Ok(Some(Vec::new()));
        };
        if !self.plugins_live() || self.generation() == 0 || self.transport.is_stopped() {
            crate::vst::forget_plan(&self.chains);
            self.plan_lead = Some(0.0);
            return Ok(Some(Vec::new()));
        }
        let started = Instant::now();
        let seam_now = clock();
        let headroom = Self::live_replacement_takeover_headroom(realtime_cps, probe_elapsed);
        // A launch takes over on its line, and a rewind plays from the
        // first cycle of its score.
        let line = self.takeover_override.filter(|line| line.is_finite());
        // The session keeps copies of this text: from the arm of the
        // launch, or from an earlier hold of the text.
        let arm = line.is_some() && held.is_some();
        // The seam ends before the takeover of the commit leaves the cover
        // of the score in play, and before the line of a launch. The lead
        // of the kept copies counts.
        let rate = self.plugin_rate.load(std::sync::atomic::Ordering::Relaxed);
        let held_lead = held.as_ref().map_or(0.0, |held| held.lead(rate));
        let spare = self.seam_time(seam_now, headroom, line, held_lead);
        // A launch with no time before its line misses the line in any
        // case. With no kept copies, its first read then takes the cover,
        // so a copy that is not ready holds the launch.
        let first_spare = if spare.is_zero() {
            self.seam_time(seam_now, headroom, None, held_lead)
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
        // A launch with kept copies reads nothing past its line.
        // A query looks at its deadline only now and then, so with no time
        // before the line the read does not start.
        let (mut haps, next) = if arm && spare.is_zero() {
            (Vec::new(), from)
        } else {
            self.candidate_haps(pattern, settings, from, SEAM_CYCLES, deadline)
        };
        // The lanes that do not play get the rest of the time.
        let mut shadow = self.candidate_shadow(settings, from, next, deadline);
        // No cycle fits before the line: the kept copies serve, and their
        // plan stays for the commit.
        let armed = arm && next == from;
        let mut wants = match held.as_mut() {
            Some(held) if armed => std::mem::take(&mut held.wants),
            _ => crate::vst::plan(
                &self.chains,
                &self.lanes,
                &self.csound,
                source,
                &haps,
                &shadow,
            ),
        };
        // The count of the copies not ready and the plugin of the first, and
        // the lines of the copies the host cannot make.
        let look = |wants: &[crate::vst::Want]| {
            let (pending, lines) = crate::vst::ensure(wants, rate);
            let first = pending.first();
            (
                first.map(|want| (pending.len(), want.name().to_owned())),
                lines,
            )
        };
        // The copies of the lanes that play go to the host first. Their
        // wants come first in the list.
        let playing = wants.iter().take_while(|want| want.sounds()).count();
        let (mut pending, mut lines) = look(&wants[..playing]);
        // A launch with no time before its line holds at once and takes a
        // later line. The kept copies decide, so the seam reads no more.
        // They have the cycles that the arm or an earlier seam read.
        let until = seam_now + spare.as_secs_f64() - SEAM_LOOK_EVERY.as_secs_f64();
        if pending.is_some() && hold && self.plugin_wait && !armed && clock() < until {
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
            let (later, last) = self.candidate_haps(pattern, settings, next, more, deadline);
            haps.extend(later);
            shadow.extend(self.candidate_shadow(settings, next, last, deadline));
            wants = crate::vst::plan(
                &self.chains,
                &self.lanes,
                &self.csound,
                source,
                &haps,
                &shadow,
            );
            (pending, lines) = look(&wants);
        } else {
            // The copies of the lanes that do not play go after the others.
            lines.extend(look(&wants[playing..]).1);
        }
        // The plugins the candidate ran load after its copies.
        self.load_ran_plugins();
        // The commit takes the lead of the plan.
        self.plan_lead = Some(self.leads_of(&wants).0);
        if pending.is_none() || !(hold && self.plugin_wait) {
            return Ok(Some(lines));
        }
        // A light plugin builds a copy in some ms.
        let waited = Instant::now();
        while pending.is_some()
            && waited.elapsed() < SEAM_WAIT
            && clock() < until
            && !self.transport.is_stopped()
        {
            std::thread::sleep(SEAM_LOOK_EVERY);
            (pending, lines) = look(&wants);
        }
        // A copy that became ready gives its latency.
        self.plan_lead = Some(self.leads_of(&wants).0);
        let Some((pending, first)) = pending else {
            return Ok(Some(lines));
        };
        if !self.hold_edit(source, wants, pending, &first, held) {
            return Ok(Some(lines));
        }
        self.restore_score_in_play()?;
        self.report_plugin_lines(lines);
        Ok(None)
    }

    /// Holds the text of an edit with `pending` copies not ready, the copy
    /// of the plugin `first` among them. `held` is the wait of the same text
    /// before, or the copies its arm prepared, as the look of its door saw
    /// it, or as a press in another document left it: see
    /// [`Self::waits_for_plugins`] and [`Self::adopt_plugin_hold`]. A wait
    /// with a copy not ready keeps the last time a copy became ready. A wait
    /// with each copy ready is over, so a copy not ready now starts a new
    /// wait. False when the wait ends: no copy became ready in
    /// [`PLUGIN_WAIT`]. The edit then plays, with one line. A text gives one
    /// line at the start of each wait. The cover of the score in play keeps
    /// room for the lead of the plan of a held text: see
    /// [`Self::plugin_lead`] and [`Self::waits_for_plugins`].
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
        if held.as_ref().is_none_or(|held| held.pending == 0) {
            self.report_plugin_wait(first);
        }
        // The text does not go in play now, so no commit takes the lead of
        // its plan. The cover keeps room for that lead.
        let (lead, _) = self.leads_of(&wants);
        self.plan_lead = None;
        self.raise_cover_lead(lead);
        // A read of another text leaves the read of the hold.
        self.lanes.hold(Some(source));
        self.plugin_hold = Some(PluginHold {
            source: Arc::from(source),
            wants,
            pending,
            progress,
            looked: now,
        });
        true
    }

    /// Says that the text waits for the copy of the plugin `first`.
    fn report_plugin_wait(&mut self, first: &str) {
        let message = format!("plugin \"{first}\" is still loading");
        let record = serde_json::json!({ "vst_loading": { "message": &message } });
        self.report_diagnostic(SAMPLE_AWAITED_DIAGNOSTIC, message, record);
    }

    /// The longest time at the commit seam from `now`. The takeover of the
    /// commit comes the margin of the output or `headroom` after the commit,
    /// and the lead of the plugin chains after that: the lead of the score
    /// in play, or `lead`, the lead of a plan of the text, when that is
    /// larger. The takeover stays inside the cover of the score in play.
    /// With `line`, the line of a launch, the takeover comes at the line or
    /// before it.
    fn seam_time(&self, now: f64, headroom: Duration, line: Option<f64>, lead: f64) -> Duration {
        let margin = self.continuity_margin.unwrap_or(self.schedule_lead);
        let margin = margin.max(headroom.as_secs_f64()) + self.plugin_lead().max(lead);
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

    /// The haps of the lanes of a live candidate that do not play, from
    /// cycle `from` to cycle `to`, one cycle at a time: see
    /// [`Self::shadow_haps`]. `settings` are the settings of the candidate.
    /// The read stops at the first cycle that does not end before
    /// `deadline`.
    fn candidate_shadow(
        &self,
        settings: &RuntimeSettings,
        from: i64,
        to: i64,
        deadline: Instant,
    ) -> Vec<crate::HapJson> {
        let mut haps = Vec::new();
        for cycle in from..to {
            let begin = Fraction::int(i128::from(cycle));
            let end = begin.add(Fraction::ONE);
            let Some(found) = settings.with(|| self.shadow_haps(begin, end, deadline)) else {
                break;
            };
            haps.extend(found);
        }
        haps
    }

    /// The plugins in a turn of the producer of the live output with
    /// `mode`, after its query. A shield before an evaluation does not come
    /// here: the evaluation waits for the shield. A first fill of an output
    /// starts the looks at the cycles ahead again: the output has no copy
    /// yet. A turn with a query measures the query time of one cycle. A
    /// steady turn with its cover full looks at one cycle ahead, or at a part
    /// of it: see [`Self::look_ahead_for_plugins`]. The look counts as asset
    /// preparation in `record`. Gives the time of the look, or 0. The look
    /// caps itself by the cover, so no reserve of the producer holds its
    /// time: see [`Self::take_look_ahead_time`]. The turn counts on the
    /// clock of the plugin chains: see [`crate::vst::tick`]. A session with
    /// no plugin call in an evaluation, with no plugins on its output, or
    /// with a score that a panic recovery restored, looks at nothing.
    pub(super) fn plugin_turn(
        &mut self,
        now: f64,
        continuation_reserve: Duration,
        mode: LiveQueryBudgetMode,
        profile: &rustel_scheduler::SchedulerTickProfile,
        record: &mut ProducerTurnRecord,
    ) -> Duration {
        // A plugin call of a setup text, of an import or of a query
        // callback gives an evaluation no plugin call, and its chain still
        // counts the time it plays.
        if self.plugins_live() {
            crate::vst::tick(&self.chains, now);
        }
        // A score that a panic recovery restored looks at nothing ahead
        // until the next commit. A look of a score that panicked panics
        // again. So a look runs only with no restored source, and a panic in
        // a look never parks the score in play.
        if !self.ran_plugin_calls || !self.plugins_live() || self.recovered_source.is_some() {
            return Duration::ZERO;
        }
        if mode == LiveQueryBudgetMode::InitialPrefill {
            self.plugin_ahead.restart();
        }
        let cycles = profile.query_span_millicycles as f64 / 1000.0;
        if cycles > 0.0 {
            let took = Duration::from_nanos(profile.query_nanos);
            self.plugin_ahead.turn = took.div_f64(cycles);
        }
        let idle = record.outcome == crate::ProducerTurnOutcome::Idle;
        self.plugin_ahead.spent = Duration::ZERO;
        if mode == LiveQueryBudgetMode::Steady && idle {
            let started = Instant::now();
            self.look_ahead_for_plugins(now, continuation_reserve);
            self.plugin_ahead.spent = started.elapsed();
            record.add_phase(ProducerPhase::AssetPreparation, self.plugin_ahead.spent);
        }
        self.plugin_ahead.spent
    }

    /// The time of the look at the cycles ahead in the last turn of the
    /// producer, then 0: see [`Self::plugin_turn`]. The tail of the turn
    /// starts after the look, and the watched producer keeps this time out
    /// of the continuation of its step.
    pub(crate) fn take_look_ahead_time(&mut self) -> Duration {
        std::mem::take(&mut self.plugin_ahead.spent)
    }

    /// One look at a cycle ahead of the score in play, or at a part of it.
    /// The plugin host builds each copy the notes of the part need. A copy
    /// is then ready before the first note of its lane: a lane that starts
    /// later in the score, and a lane at the end of its mute.
    ///
    /// Each look reads one cycle, or a part of it, from the cycle in play up
    /// to [`PLAN_CYCLES`] cycles ahead of it. The looks read the lanes that
    /// play first, so their copies go to the host first. Then the looks read
    /// the same cycles of the lanes that do not play. A read of these lanes
    /// that does not end holds no look at the lanes that play. The looks
    /// start again at the cycle in play with a new score, a first fill of an
    /// output, a new rate of the output, and a cycle in play that goes back.
    /// A look gets twice the query time of its part, inside the cover left,
    /// and [`LOOK_MOST`] at most: see [`PluginAhead::budget`]. A look that
    /// does not end leaves its part to the next look at the same lanes, with
    /// twice the time. At [`LOOK_MOST`] the next look reads half of the
    /// part, so the looks skip no part of a cycle. Each copy the host cannot
    /// make gives its reason as a plugin line. The console lines of a look
    /// go. The latency of the copies raises the lead the cover keeps room
    /// for, so the first notes of such a lane find room for their lead: see
    /// [`Self::plugin_lead`].
    fn look_ahead_for_plugins(&mut self, now: f64, continuation_reserve: Duration) {
        let play = self.scheduler.cycle_at_time(now).floor() as i64;
        let rate = self.plugin_rate.load(std::sync::atomic::Ordering::Relaxed);
        let Some((begin, end, shadow)) = self.plugin_ahead.part(play, rate) else {
            return;
        };
        let Some(cover) = self
            .scheduler
            .affordable_deadline(now, continuation_reserve)
        else {
            return;
        };
        let budget = self.plugin_ahead.budget(shadow).min(cover);
        let started = Instant::now();
        let deadline = started + budget;
        let read = if shadow {
            self.shadow_haps(begin, end, deadline)
        } else {
            self.js.with_logs_dropped(|| {
                let haps = rustel_core::with_query_deadline(deadline, || {
                    self.query_with_js_budget(begin, end, budget)
                });
                // A stack keeps the other lanes of a lane that throws.
                let _ = rustel_core::take_query_contained_throw();
                haps.ok().map(|haps| haps_to_json(&haps))
            })
        };
        let took = started.elapsed();
        self.plugin_ahead
            .looked(end, shadow, budget, took, read.is_some());
        let Some(haps) = read else {
            return;
        };
        let wants = if shadow {
            crate::vst::wants(&self.chains, &self.lanes, &self.csound, &[], &haps)
        } else {
            crate::vst::wants(&self.chains, &self.lanes, &self.csound, &haps, &[])
        };
        self.ensure_wants(&wants);
        // These notes play later: the cover keeps room for their lead.
        let (lead, shadow_lead) = self.leads_of(&wants);
        self.raise_cover_lead(lead.max(shadow_lead));
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
    use super::super::tests::{fixture_plugins, plugin_chain, plugin_note};
    use super::*;
    use crate::test_support::past_the_cover;

    /// A wait ends when no copy became ready in 20 s, and the edit plays
    /// with one line. A prepare of the same text keeps the wait that ended.
    /// A wait with each copy ready is over, so a copy the landing needs that
    /// is not ready starts a new wait, and so does a look that finds a copy
    /// not ready, with one line.
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
        // A look with a copy not ready after a look with each copy ready
        // starts a new wait, with one line. At a new rate no copy is ready.
        let hold = session.plugin_hold.as_mut().expect("the wait");
        hold.pending = 0;
        hold.progress = hold.progress.checked_sub(PLUGIN_WAIT).expect("uptime");
        hold.looked = hold.looked.checked_sub(PLUGIN_LOOK_EVERY).expect("uptime");
        session.plugin_rate.store(44_100, relaxed);
        session.take_diagnostics();
        assert!(session.waits_for_plugins(&sections));
        let lines = session.take_diagnostics().into_iter();
        let waiting = lines.filter(|line| line.kind == SAMPLE_AWAITED_DIAGNOSTIC);
        assert_eq!(waiting.count(), 1, "a new wait");
        session.cancel_plugin_hold();
    }

    /// The looks read one cycle each, up to 16 cycles ahead of the cycle in
    /// play. The looks at the lanes that do not play come after, and one
    /// that does not end holds no look at the lanes that play. A look that
    /// does not end reads its part again, with twice the time. At the
    /// longest time, the next look reads half the part. A look that ends in
    /// a quarter of its time reads twice the part. A new rate, and a cycle
    /// in play that goes back, start the looks again at the cycle in play. A
    /// look waits as long as the look before at the same lanes took.
    #[test]
    fn a_look_reads_the_next_cycle_and_skips_none() {
        let (rate, budget) = (48_000, Duration::from_millis(4));
        let at = |cycle: i64| Fraction::int(i128::from(cycle));
        let whole = |cycle: i64, shadow: bool| Some((at(cycle), at(cycle + 1), shadow));
        let mut ahead = PluginAhead::default();
        assert_eq!(ahead.part(3, rate), whole(3, false));
        ahead.looked(at(4), false, budget, Duration::ZERO, false);
        assert_eq!(ahead.budget(false), budget * 2);
        assert_eq!(
            ahead.part(3, rate),
            whole(3, false),
            "a part that did not end"
        );
        ahead.looked(at(4), false, budget * 2, Duration::ZERO, true);
        assert_eq!(ahead.part(3, rate), whole(4, false));
        // The cycle in play passes the looks: they go on from it.
        for cycle in 9..9 + PLAN_CYCLES {
            assert_eq!(ahead.part(9, rate), whole(cycle, false));
            ahead.looked(at(cycle + 1), false, budget, Duration::ZERO, true);
        }
        // Then the looks read the lanes that do not play.
        assert_eq!(ahead.part(9, rate), whole(9, true));
        ahead.looked(at(10), true, budget, Duration::from_secs(60), false);
        assert_eq!(ahead.budget(true), budget * 2);
        assert_eq!(
            ahead.part(10, rate),
            whole(25, false),
            "the lanes that play"
        );
        ahead.looked(at(26), false, budget, Duration::ZERO, true);
        assert_eq!(
            ahead.part(10, rate),
            None,
            "the wait of the lanes that do not play"
        );
        ahead.shadow_due = None;
        for cycle in 10..10 + PLAN_CYCLES {
            assert_eq!(ahead.part(10, rate), whole(cycle, true));
            ahead.looked(at(cycle + 1), true, budget, Duration::ZERO, true);
        }
        assert_eq!(ahead.part(10, rate), None, "16 cycles ahead");
        assert_eq!(ahead.part(11, rate), whole(26, false));
        assert_eq!(ahead.part(11, 44_100), whole(11, false), "a new rate");
        ahead.looked(at(12), false, budget, Duration::ZERO, true);
        assert_eq!(
            ahead.part(2, 44_100),
            whole(2, false),
            "a cycle in play back"
        );
        ahead.looked(at(3), false, budget, Duration::from_secs(60), true);
        assert_eq!(ahead.part(2, 44_100), None);

        // A cycle that takes 1 s gets the longest time. A look at it that
        // does not end leaves half of its part to the next look.
        (ahead.due, ahead.turn) = (None, Duration::from_secs(1));
        assert_eq!(ahead.part(3, 44_100), whole(3, false));
        assert_eq!(ahead.budget(false), LOOK_MOST);
        ahead.looked(at(4), false, LOOK_MOST, LOOK_MOST, false);
        ahead.due = None;
        let half = Fraction::new(7, 2);
        assert_eq!(ahead.part(3, 44_100), Some((at(3), half, false)));
        ahead.looked(half, false, LOOK_MOST, LOOK_MOST / 2, true);
        ahead.due = None;
        assert_eq!(ahead.part(3, 44_100), Some((half, at(4), false)));
        // A look that ends in a quarter of its time reads a whole cycle
        // again.
        ahead.looked(at(4), false, LOOK_MOST, Duration::ZERO, true);
        ahead.due = None;
        assert_eq!(ahead.part(3, 44_100), whole(4, false));
    }

    /// The copies of the plugins of the bundles in `folder`, after the work
    /// of the plugin thread.
    fn copies_in(folder: &std::path::Path) -> usize {
        let host = crate::vst::host();
        host.wait_idle();
        let plugins = host.plugins().into_iter();
        let plugins = plugins.filter(|plugin| plugin.bundle.starts_with(folder));
        plugins.map(|plugin| plugin.running).sum()
    }

    /// One turn of the live output at `now`. Its tail starts after its look.
    fn turn(session: &mut Session, now: f64, mode: LiveQueryBudgetMode) {
        let reserve = Duration::from_millis(25);
        let started = Instant::now();
        let turn = session.schedule_audio_live_at(now, 48_000, reserve, mode);
        let batch = turn.unwrap_or_else(|_| panic!("a turn at {now}"));
        assert!(batch.tail_started >= started + session.take_look_ahead_time());
    }

    /// The turns of the live output at `now` until the next look at the
    /// lanes that play reads from `cycle` or later. With `shadow`, until the
    /// next look at the lanes that do not play does.
    fn look_until(session: &mut Session, now: f64, cycle: i64, shadow: bool) {
        let started = Instant::now();
        let cycle = Some(Fraction::from(cycle));
        let next = |ahead: &PluginAhead| if shadow { ahead.shadow } else { ahead.next };
        while next(&session.plugin_ahead) < cycle {
            assert!(started.elapsed() < Duration::from_secs(10), "no look");
            turn(session, now, LiveQueryBudgetMode::Steady);
        }
    }

    /// The plugin lines of the session since the last call.
    fn plugin_lines(session: &mut Session) -> Vec<String> {
        let lines = session.take_diagnostics().into_iter();
        let lines = lines.filter(|line| line.kind == PLUGIN_DIAGNOSTIC);
        lines.map(|line| line.message).collect()
    }

    /// A turn of the live output with its cover full looks at one cycle
    /// ahead. The looks build the copy of a lane with its first note at
    /// cycle 6, and the copy of a muted lane. The looks at the muted lanes
    /// come after the looks at the lanes that play. A muted lane with a
    /// preset the host does not have gives one line. The console lines of a
    /// look go. A session looks at nothing with no plugin call in an
    /// evaluation, with no plugins on its output, or with no cover left. A
    /// turn that fills the cover looks at nothing. A first fill of an output
    /// starts the looks again at the cycle in play. A shield looks at
    /// nothing, also with its cover full. A look that does not end in its
    /// time reads its cycle again. A score that a panic recovery restored
    /// looks at nothing until the next commit. A new output starts the looks
    /// again at the cycle in play.
    #[test]
    fn the_looks_ahead_load_the_copies_of_later_notes() {
        let (_one, folder) = fixture_plugins();
        let mut session = Session::new().expect("session");
        session.set_direct_diagnostic_logging(false);
        let relaxed = std::sync::atomic::Ordering::Relaxed;
        session.plugins_live.store(true, relaxed);
        session
            .evaluate(r#"$: s("sine").gain(0)"#)
            .expect("no plugin");
        for _ in 0..4 {
            turn(&mut session, 0.0, LiveQueryBudgetMode::Steady);
        }
        assert_eq!(session.plugin_ahead.next, None, "no plugin call");
        let score = concat!(
            "$: s(\"sine\").gain(0).fmap(v => { console.log(\"ahead\"); return v })\n",
            "$: note(\"c3\").s(\"sine\").vst(\"rustel fixture\").mask(\"<0!6 1>\")\n",
            "_$: note(\"e3\").s(\"sine\").vst(\"rustel fixture\")\n",
            "_$: note(\"g3\").s(\"sine\").vst(\"rustel fixture\", { preset: \"missing\" })",
        );
        session.plugins_live.store(false, relaxed);
        session.evaluate(score).expect("plugin score");
        for _ in 0..4 {
            turn(&mut session, 0.0, LiveQueryBudgetMode::Steady);
        }
        assert_eq!(session.plugin_ahead.next, None, "no plugins on the output");
        // A turn that fills the cover measures the query time of a cycle.
        session.plugins_live.store(true, relaxed);
        // A turn takes the console lines before its look. The check comes
        // after a look at the lanes that play.
        look_until(&mut session, 0.25, PLAN_CYCLES, false);
        assert!(session.js.take_logs().is_empty(), "the lines of a look");
        look_until(&mut session, 0.25, PLAN_CYCLES, true);
        assert_eq!(copies_in(folder.path()), 2, "cycle 6 and the muted lane");
        turn(&mut session, 2.0, LiveQueryBudgetMode::Steady);
        assert_eq!(
            session.plugin_ahead.next,
            Some(Fraction::from(PLAN_CYCLES)),
            "a fill"
        );
        // The host has read the preset folder now. The line of the muted
        // lane comes one time.
        look_until(&mut session, 2.0, PLAN_CYCLES + 1, false);
        look_until(&mut session, 4.0, PLAN_CYCLES + 2, true);
        let missing = "vst skipped: Rustel Fixture has no preset with the name 'missing'";
        assert_eq!(plugin_lines(&mut session), [missing]);
        assert_eq!(copies_in(folder.path()), 2);

        // At 100 s the output has no cover left.
        session.plugin_ahead.due = None;
        session.look_ahead_for_plugins(100.0, Duration::from_millis(25));
        assert_eq!(
            session.plugin_ahead.next,
            Some(Fraction::int(50)),
            "no cover left"
        );
        turn(&mut session, 4.0, LiveQueryBudgetMode::InitialPrefill);
        assert_eq!(session.plugin_ahead.next, None, "a first fill");
        // The evaluation after a shield waits for it. The second shield
        // finds its cover full.
        for _ in 0..2 {
            let shield =
                session.schedule_audio_reload_shield_at(4.0, 48_000, Duration::from_millis(25));
            shield.unwrap_or_else(|_| panic!("a shield at 4.0"));
        }
        assert_eq!(session.plugin_ahead.next, None, "a shield");
        look_until(&mut session, 4.0, 3, false);
        // A look with 2 ns does not end: its cycle stays for the next look,
        // with twice the time.
        let ahead = &mut session.plugin_ahead;
        (ahead.due, ahead.turn, ahead.look) = (None, Duration::ZERO, Duration::from_nanos(1));
        session.look_ahead_for_plugins(4.0, Duration::from_millis(25));
        assert_eq!(
            session.plugin_ahead.next,
            Some(Fraction::int(3)),
            "a look that did not end"
        );
        assert_eq!(session.plugin_ahead.look, Duration::from_nanos(2));
        look_until(&mut session, 4.0, 4, false);

        // A panic recovery restores the score in play. The restored score
        // looks at nothing until the next commit.
        session.inject_panic_for_test(SessionPanicPoint::Query);
        let reserve = Duration::from_millis(25);
        let mode = LiveQueryBudgetMode::Steady;
        let panicked = session.schedule_audio_live_at(5.0, 48_000, reserve, mode);
        assert!(panicked.is_err(), "a turn that panics");
        for _ in 0..4 {
            turn(&mut session, 5.0, mode);
        }
        assert_eq!(session.plugin_ahead.next, None, "a restored score");
        session.reload_at(score, false, 5.0).expect("a commit");
        look_until(&mut session, 5.0, 3, false);
        // A new output starts the looks again at the cycle in play.
        let requery = session.requery_after_output_recycle_at(5.0);
        assert!(requery.expect("a new output").is_some());
        assert_eq!(session.plugin_ahead.next, None, "a new output");
    }

    /// The plan of a start, of an edit and of a launch has the notes of the
    /// lanes that do not play. Their copies load, and nothing waits for
    /// them. The seam reads them in its time, also in the cycles it reads
    /// more for a copy not ready. The copies of the lanes that play go to the
    /// host first, also the copies of these cycles. Each copy the host cannot
    /// make gives one line: the line of an edit comes after its commit, so a
    /// look gives no second one, and the line of a held edit comes at its
    /// hold. A copy that does not start gives one line in each evaluation:
    /// the host and the plan give the same text. A new score and a direct
    /// pattern start the looks again. The plugins an evaluation ran load at
    /// the next look, not at the evaluation.
    #[test]
    fn the_plans_load_the_copies_of_the_lanes_that_do_not_play() {
        let (_one, folder) = fixture_plugins();
        let presets = folder.path().join("presets");
        let presets = presets.join(rustel_vst3_fixture::NAME);
        std::fs::create_dir_all(&presets).expect("preset folder");
        let half = rustel_vst3_fixture::preset(0.5, 0.0);
        std::fs::write(presets.join("Half.vstpreset"), &half).expect("preset file");
        // A build with the preset of a different plugin fails.
        let mut other = half;
        other[8] = b'0';
        std::fs::write(presets.join("Other.vstpreset"), other).expect("preset file");
        let mut session = Session::new().expect("session");
        session.set_direct_diagnostic_logging(false);
        let relaxed = std::sync::atomic::Ordering::Relaxed;
        session.plugins_live.store(true, relaxed);
        session.plugin_rate.store(48_000, relaxed);
        let score = concat!(
            "$: s(\"sine\").gain(0)\n",
            "_$: note(\"e3\").s(\"sine\").vst(\"rustel fixture\")\n",
            "_$: note(\"g3\").s(\"sine\").vst(\"rustel fixture\", { preset: \"missing\" })\n",
            "_$: note(\"b3\").s(\"sine\").vst(\"rustel fixture\", { preset: \"other\" })",
        );
        session.evaluate(score).expect("score");
        // The evaluation loads nothing: the next look loads the plugin, after
        // the copies of the lanes that play.
        let host = crate::vst::host();
        host.wait_idle();
        assert!(host.loaded(rustel_vst3_fixture::NAME).is_none());
        let ceiling = Duration::from_secs(1);
        assert_eq!(session.plugins_pending(0.0, 1.0, ceiling), 0);
        assert_eq!(copies_in(folder.path()), 1, "the copy of the muted lane");
        // The cover of the score. The copy that does not start gives one
        // line, from the host. A look at its lane gives no second one.
        turn(&mut session, 0.0, LiveQueryBudgetMode::Steady);
        let failed =
            "vst skipped: Rustel Fixture did not start: the preset is for a different plugin";
        assert_eq!(plugin_lines(&mut session), [failed]);
        look_until(&mut session, 0.0, 1, true);
        let missing = "vst skipped: Rustel Fixture has no preset with the name 'missing'";
        assert_eq!(plugin_lines(&mut session), [missing]);

        let tone = format!("{score}\n_$: note(\"a3\").s(\"sine\").vsti(\"rustel fixture tone\")");
        let stopped = AtomicBool::new(false);
        let edit = session.reload_live(&tone, false, &stopped, || 0.0);
        assert!(matches!(edit, Ok(LiveReload::Installed(_))));
        assert_eq!(copies_in(folder.path()), 2, "the copy of the muted lane");
        assert_eq!(plugin_lines(&mut session), [missing, failed]);
        look_until(&mut session, 0.0, 1, true);
        assert!(plugin_lines(&mut session).is_empty(), "a second line");

        // A held edit counts the copies of the lanes that play.
        let double =
            "\n$: note(\"c4\").s(\"sine\").vst(\"rustel fixture\").vst(\"rustel fixture\")";
        let absent =
            "\n_$: note(\"d3\").s(\"sine\").vst(\"rustel fixture\", { preset: \"absent\" })";
        let held = format!("{tone}{double}{absent}");
        let edit = session.reload_live(&held, false, &stopped, past_the_cover(0.0));
        assert_eq!(edit.expect("a held edit"), LiveReload::Waiting);
        let absent = "vst skipped: Rustel Fixture has no preset with the name 'absent'";
        assert_eq!(plugin_lines(&mut session), [absent]);
        assert_eq!(session.plugins_waited_for(&held), Some((2, 2)));
        session.cancel_plugin_hold();
        let gone = "\n_$: note(\"f3\").s(\"sine\").vst(\"rustel fixture\", { preset: \"gone\" })";
        let launch = format!("{tone}{gone}");
        let prepared = session.prepare_reload(&launch, false, 0.0, &stopped, || 0.0);
        assert!(!prepared.expect("prepare"));
        let gone = "vst skipped: Rustel Fixture has no preset with the name 'gone'";
        assert_eq!(plugin_lines(&mut session), [gone]);

        // A muted lane with its first note at cycle 8 gets its copy in the
        // cycles the seam reads for a copy not ready.
        let later =
            "\n_$: note(\"b3\").s(\"sine\").vsti(\"rustel fixture tone\").mask(\"<0!8 1!8>\")";
        let more = format!("{tone}{double}{later}");
        assert_eq!(session.plugin_ahead.next, Some(Fraction::from(PLAN_CYCLES)));
        let edit = session.reload_live(&more, false, &stopped, || 0.0);
        assert!(matches!(edit, Ok(LiveReload::Installed(_))));
        assert_eq!(session.plugin_ahead.next, None, "a new score");
        assert_eq!(copies_in(folder.path()), 5, "a copy of cycle 8");

        // The copies of the lanes that play go to the host before the load
        // of a plugin of a muted lane, also a new copy of the cycles the
        // seam reads more. The load takes 600 ms, and the edit plays at once.
        turn(&mut session, 0.0, LiveQueryBudgetMode::Steady);
        let bundle = rustel_vst3_fixture::install(&folder.path().join("slow"));
        let slow = bundle.with_file_name("rustel later.vst3");
        std::fs::rename(&bundle, slow).expect("bundle name");
        host.scan(&crate::vst::folders());
        host.wait_idle();
        // SAFETY: one fixture test runs at a time, and the plugin thread has
        // no work. So no other thread reads the variable now.
        unsafe { std::env::set_var(rustel_vst3_fixture::SLOW_ENV, "600") };
        let lanes = concat!(
            "\n$: note(\"d4\").s(\"sine\").vst(\"rustel fixture\", { preset: \"half\" })",
            "\n$: note(\"<~ ~ e4>\").s(\"sine\").vst(\"rustel fixture\", { preset: \"half\" })",
            "\n_$: note(\"c3\").s(\"sine\").vst(\"rustel later\")",
        );
        let first = format!("{more}{lanes}");
        let edit = session.reload_live(&first, false, &stopped, || 0.0);
        assert!(matches!(edit, Ok(LiveReload::Installed(_))));
        host.wait_idle();
        // SAFETY: as above.
        unsafe { std::env::remove_var(rustel_vst3_fixture::SLOW_ENV) };
        look_until(&mut session, 0.0, 1, false);
        session
            .set_pattern(rustel_core::silence())
            .expect("a direct pattern");
        assert_eq!(session.plugin_ahead.next, None, "a direct pattern");
    }

    /// A lane plays a pattern that a setup text built, so no evaluation ran
    /// a plugin call. The turns of the producer still count for its chain.
    /// The window of a turn tells the plugin host the copy the chain wants:
    /// a copy with another preset gets no build for the slot. An unload of
    /// the plugins keeps the plugin of the chain. A chain in its grace keeps
    /// no plugin. The lane leaves the text and comes back before its time:
    /// it keeps its number and its chain. The lane leaves again: its chain
    /// ends after 5 s of turns, and not before. The host hears that the
    /// chain ended, so its slot gets no build. The host also hears of a
    /// chain that a Csound orbit ends to make room. A turn of an output with
    /// no plugins counts nothing. A session with no live output keeps no
    /// plugin. A live output gives back a copy the chain no longer wants, 5 s
    /// after the chain lost its key. The score rests while a score of another
    /// document plays. After 5 s of play the output names its chain no more,
    /// and the chain keeps the copy of its preset. Its slot takes no copy of
    /// another preset. The score plays again, and its tab closes while it
    /// plays. When a score of another document plays, its chain is a closed
    /// chain in its grace, and it keeps its copy. A session that ends ends
    /// its chains at the host, a closed chain too. Its copy ends, and the
    /// slot of its chain takes no copy then.
    #[test]
    fn a_chain_of_a_setup_pattern_leaves_after_its_time() {
        let (_one, folder) = fixture_plugins();
        let presets = folder.path().join("presets");
        let presets = presets.join(rustel_vst3_fixture::NAME);
        std::fs::create_dir_all(&presets).expect("preset folder");
        let half = rustel_vst3_fixture::preset(0.5, 0.0);
        std::fs::write(presets.join("Half.vstpreset"), half).expect("preset file");
        let mut session = Session::new().expect("session");
        session.set_direct_diagnostic_logging(false);
        let relaxed = std::sync::atomic::Ordering::Relaxed;
        session.plugins_live.store(true, relaxed);
        session.plugin_rate.store(48_000, relaxed);
        let setup = "globalThis.fixture = note('c3').s('sine').vst('rustel fixture')";
        session.evaluate_prebake(setup).expect("setup");
        let kick = r#"$: s("sine").gain(0)"#;
        let score = format!("{kick}\n$: fixture");
        session.evaluate(&score).expect("score");
        let steady = LiveQueryBudgetMode::Steady;
        let reserve = Duration::from_millis(25);
        let window = session.schedule_audio_live_at(0.0, 48_000, reserve, steady);
        let events = window.unwrap_or_else(|_| panic!("a turn at 0")).events;
        let mut events = events.into_iter();
        let chain = events.find_map(|event| event.controls.insert_chain);
        let chain = chain.expect("the chain of the lane");
        let (host, fixture) = (crate::vst::host(), "rustel fixture");
        let loaded = host.resolve(fixture, true);
        assert!(matches!(loaded, crate::vst::Resolved::Ready(..)));
        assert!(
            host.key(fixture, None, false)
                .is_ok_and(|key| key.is_some())
        );
        let slot = rustel_audio::effect_slot(chain, 0);
        host.prepare(fixture, Some("half"), false, 48_000, slot);
        assert_eq!(
            copies_in(folder.path()),
            0,
            "a copy the chain does not want"
        );
        assert_eq!(plugin_chain(&session), chain);
        let insert_chains = |session: &Session| {
            let turn = rustel_audio::InsertTurn::default();
            crate::vst::named_chains(&session.chains, turn)
        };
        assert_eq!(insert_chains(&session).named, [chain]);
        assert_eq!(session.plugin_names(), [fixture]);

        session.evaluate(kick).expect("no plugin lane");
        assert!(session.plugin_names().is_empty(), "a chain in its grace");
        for now in 1..5 {
            turn(&mut session, f64::from(now), steady);
        }
        session.evaluate(&score).expect("the lane back");
        assert_eq!(plugin_note(&session)["__rustel_lane"], "#1");
        assert_eq!(plugin_chain(&session), chain);
        assert_eq!(session.plugin_names(), [fixture]);

        session.evaluate(kick).expect("no plugin lane");
        session.plugins_live.store(false, relaxed);
        for now in 5..20 {
            turn(&mut session, f64::from(now), steady);
        }
        // The first turn after the pause counts 1 s.
        session.plugins_live.store(true, relaxed);
        for now in 20..25 {
            assert_eq!(insert_chains(&session).named, [chain], "at {now} s");
            turn(&mut session, f64::from(now), steady);
        }
        assert!(insert_chains(&session).named.is_empty());
        host.prepare(fixture, None, false, 48_000, slot);
        assert_eq!(copies_in(folder.path()), 0, "a slot of a chain that ended");
        assert!(!session.ran_plugin_calls());

        // The lane comes back, and 63 run-time lanes fill the room. The
        // lane leaves, and a Csound orbit ends its chain to make room.
        session.evaluate(&score).expect("the lane back");
        let spare = rustel_audio::effect_slot(plugin_chain(&session), 0);
        let run_time = |lane: usize| {
            crate::ValueJson::Raw(serde_json::json!({
                "__rustel_lane": format!("${lane}"),
                "vst": [{"name": fixture, "__rustel_binding": format!("p{lane}")}],
            }))
        };
        let names: Vec<crate::ValueJson> =
            (1..rustel_audio::MAX_INSERT_BUSES).map(run_time).collect();
        crate::vst::assign(
            &session.chains,
            &session.lanes,
            &session.csound,
            &names,
            true,
        );
        session.evaluate(kick).expect("no plugin lane");
        let csound = serde_json::json!({"s": "sine", "csound": "probe", "orbit": 2});
        let window = [run_time(1), crate::ValueJson::Raw(csound)];
        crate::vst::assign(
            &session.chains,
            &session.lanes,
            &session.csound,
            &window,
            true,
        );
        host.prepare(fixture, None, false, 48_000, spare);
        assert_eq!(
            copies_in(folder.path()),
            0,
            "a slot of a chain that ended to make room"
        );

        session.evaluate(&score).expect("the lane back");
        let slot = rustel_audio::effect_slot(plugin_chain(&session), 0);
        session.plugins_live.store(false, relaxed);
        assert!(session.plugin_names().is_empty(), "no live output");
        session.plugins_live.store(true, relaxed);
        host.prepare(fixture, Some("half"), false, 48_000, slot);
        assert_eq!(
            copies_in(folder.path()),
            0,
            "a copy the chain does not want"
        );

        // A live output holds the copy of the slot, and the lane takes a new
        // preset. The names source of the output gives the copy as gone 5 s
        // of turns after the take, and not before. The output then gives the
        // copy back.
        let mut output = rustel_audio::device::ManualLiveOutput::new(48_000, session.generation())
            .expect("output");
        session.attach_plugins(output.device());
        host.prepare(fixture, None, false, 48_000, slot);
        host.wait_idle();
        let (key, _) = host
            .key(fixture, None, false)
            .expect("a key")
            .expect("the key");
        output.device().ensure_insert(slot, key);
        output.render(&mut [0.0; 256]);
        let setup =
            "globalThis.fixture = note('c3').s('sine').vst('rustel fixture', {preset: 'half'})";
        session.evaluate_prebake(setup).expect("a new preset");
        session.evaluate(&score).expect("score");
        assert_eq!(plugin_chain(&session), slot.chain);
        for now in 30..34 {
            turn(&mut session, f64::from(now), steady);
            assert!(insert_chains(&session).gone.is_empty(), "at {now} s");
        }
        turn(&mut session, 34.0, steady);
        assert_eq!(insert_chains(&session).gone, [(slot, key)]);
        assert!(output.device().holds_insert(slot, key));
        rustel_audio::LiveReverbBatch::install_inserts(output.device(), &[]);
        output.render(&mut [0.0; 256]);
        rustel_audio::LiveReverbBatch::install_inserts(output.device(), &[]);
        assert!(!output.device().holds_insert(slot, key));
        drop(output);
        // The score rests while a score of another document plays. After
        // 5 s of play the output names its chain no more, and the chain
        // keeps the copy of its preset. Its slot takes no copy of another
        // preset.
        host.prepare(fixture, Some("half"), false, 48_000, slot);
        session.set_plugin_document(1);
        session.evaluate(kick).expect("a score of another document");
        for now in 35..41 {
            turn(&mut session, f64::from(now), steady);
        }
        assert!(insert_chains(&session).named.is_empty(), "a rest chain");
        assert_eq!(copies_in(folder.path()), 1, "the copy of a rest chain");
        host.prepare(fixture, None, false, 48_000, slot);
        assert_eq!(
            copies_in(folder.path()),
            1,
            "a copy the rest chain does not want"
        );
        // The score plays again, and its tab closes while the transport
        // runs, so the score plays on. A score of another document plays:
        // the chain is a closed chain in its grace. No call of the host
        // names it, and the host keeps its copy.
        session.set_plugin_document(0);
        session.evaluate(&score).expect("the score again");
        assert_eq!(plugin_chain(&session), slot.chain);
        assert!(
            !session.forget_plugin_document(0),
            "the closed score plays on"
        );
        session.set_plugin_document(1);
        session.evaluate(kick).expect("a score of another document");
        assert_eq!(copies_in(folder.path()), 1, "the copy of a closed chain");
        drop(session);
        host.prepare(fixture, Some("half"), false, 48_000, slot);
        assert_eq!(
            copies_in(folder.path()),
            0,
            "a slot of a session that ended"
        );
    }

    /// 2 documents with one text share one copy. The text waits in
    /// document 2, and the session holds it while its copy builds. The same
    /// text in document 3 takes its place, with its read and its plan. A
    /// text that throws in document 4 before that press leaves the read of
    /// the hold. The copy of the hold serves that press: no second wait
    /// starts, and the text lands in document 3 with one copy. A launch of a
    /// held text in another document takes its place too: its arm builds
    /// nothing, and nothing waits. A text that throws before the landing
    /// leaves the plan in its document. A cancel ends the hold. The same
    /// text in another document then takes no lane of the cancelled read,
    /// and its new lane gets a new number. A document with a lane of its own
    /// reads that lane first. A lane of its own with a new body comes first
    /// too. The text waits in another document for a new copy, and the same
    /// text in the document with the lane lands at once on its own copy. A
    /// launch there arms from that lane too, and its arm waits for no copy.
    /// A launch of a held text in another document needs a copy the hold
    /// did not wait for. Its wait counts from its arm, with no second line,
    /// also after the time of the wait before is over. A press in another
    /// document whose evaluation fails after it took the read and the plan
    /// leaves both there. The text pressed again in its first document takes
    /// them back, and lands on the copy of the hold.
    #[test]
    fn the_held_text_pressed_in_another_document_lands_on_the_same_copy() {
        let (_one, folder) = fixture_plugins();
        let (host, fixture) = (crate::vst::host(), rustel_vst3_fixture::NAME);
        // The plugin loads first. So each look puts the build of its copy in
        // the line at once, and a wait for the plugin thread waits for it.
        let loaded = host.resolve(fixture, true);
        assert!(matches!(loaded, crate::vst::Resolved::Ready(..)));
        assert!(
            host.key(fixture, None, false)
                .is_ok_and(|key| key.is_some())
        );
        host.wait_idle();
        // Each new copy takes 300 ms in its build, so the copy of a text is
        // not ready at its commit seam.
        // SAFETY: one fixture test runs at a time, and the plugin thread has
        // no work. So no other thread reads the variable now.
        unsafe { std::env::set_var(rustel_vst3_fixture::SLOW_ACTIVATE_ENV, "300") };
        let mut session = Session::new().expect("session");
        session.set_direct_diagnostic_logging(false);
        session.set_plugin_document(1);
        let kick = r#"kick: s("sine").gain(0)"#;
        session.evaluate(kick).expect("score in play");
        let relaxed = std::sync::atomic::Ordering::Relaxed;
        session.plugins_live.store(true, relaxed);
        session.plugin_rate.store(48_000, relaxed);
        let stopped = AtomicBool::new(false);
        session.schedule_through(0.1, 0.1).expect("cover");
        let generation = session.generation();
        let text = format!("{kick}\nlead: note(\"c3\").s(\"sine\").vst(\"rustel fixture\")");
        let waits = |session: &mut Session| {
            let mut lines = session.take_diagnostics().into_iter();
            lines.any(|line| line.kind == SAMPLE_AWAITED_DIAGNOSTIC)
        };
        session.set_plugin_document(2);
        let reload = session.reload_live(&text, false, &stopped, past_the_cover(0.15));
        assert_eq!(reload.expect("the held text"), LiveReload::Waiting);
        assert!(waits(&mut session), "the wait of document 2");
        // A text that throws in document 4 takes the last read. The read of
        // the hold stays.
        session.set_plugin_document(4);
        let typo = format!("{text}\nthrow new Error(\"typo\")");
        let failed = session.reload_live(&typo, false, &stopped, || 0.15);
        assert!(failed.is_err(), "the text that throws");
        // The copy is ready, and the next look sees it.
        let ready = |session: &mut Session| {
            host.wait_idle();
            let hold = session.plugin_hold.as_mut().expect("the wait");
            hold.looked = hold.looked.checked_sub(PLUGIN_LOOK_EVERY).expect("uptime");
            session.take_diagnostics();
        };
        ready(&mut session);
        // The same text in document 3 evaluates there with the read and the
        // plan of the hold. It lands at once on the copy of the hold.
        session.set_plugin_document(3);
        session.schedule_through(0.2, 0.2).expect("cover");
        let landed = session.reload_live(&text, false, &stopped, || 0.25);
        assert_eq!(
            landed.expect("landing"),
            LiveReload::Installed(generation + 1)
        );
        assert!(!waits(&mut session), "a second wait");
        assert_eq!(session.lanes.document(), 3);
        assert_eq!(copies_in(folder.path()), 1);

        // A text with a new lane waits in document 4. A launch arms it in
        // document 5 when its copy is ready, and keeps that copy.
        let more = format!("{text}\npad: note(\"e3\").s(\"sine\").vst(\"rustel fixture\")");
        session.set_plugin_document(4);
        session.schedule_through(0.3, 0.3).expect("cover");
        let reload = session.reload_live(&more, false, &stopped, past_the_cover(0.35));
        assert_eq!(reload.expect("the held text"), LiveReload::Waiting);
        ready(&mut session);
        session.set_plugin_document(5);
        session.schedule_through(0.4, 0.4).expect("cover");
        let prepared = session.prepare_reload(&more, false, 0.0, &stopped, || 0.4);
        assert!(!prepared.expect("prepare"), "the arm waits");
        assert!(!waits(&mut session), "a wait of the arm");
        // A text that throws in document 5 takes the last read. The plan of
        // the held text stays in 5, so its landing builds no copy.
        let broken = format!("{more}\nthrow new Error(\"typo\")");
        let failed = session.reload_live(&broken, false, &stopped, || 0.42);
        assert!(failed.is_err(), "the text that throws");
        let landed = session.reload_live(&more, false, &stopped, || 0.45);
        assert_eq!(
            landed.expect("landing"),
            LiveReload::Installed(generation + 2)
        );
        assert!(!waits(&mut session), "a wait after the throw");
        assert_eq!(session.lanes.document(), 5);
        assert_eq!(copies_in(folder.path()), 2);

        // A cancel ends the hold of a text with a new lane in document 6.
        // The same text in document 7 then reads its own lanes and the lanes
        // in play: its new lane gets a new number, not the number of the
        // cancelled read.
        let last = format!("{more}\nbass: note(\"g2\").s(\"sine\").vst(\"rustel fixture\")");
        session.set_plugin_document(6);
        session.schedule_through(0.5, 0.5).expect("cover");
        let reload = session.reload_live(&last, false, &stopped, past_the_cover(0.55));
        assert_eq!(reload.expect("the held text"), LiveReload::Waiting);
        assert!(waits(&mut session), "the wait of document 6");
        let cancelled = session.lanes.read(&last);
        session.cancel_plugin_hold();
        session.set_plugin_document(7);
        session.schedule_through(0.6, 0.6).expect("cover");
        let reload = session.reload_live(&last, false, &stopped, past_the_cover(0.65));
        assert_eq!(reload.expect("the held text"), LiveReload::Waiting);
        assert!(waits(&mut session), "the wait of document 7");
        let read = session.lanes.read(&last);
        assert_eq!(read[..3], cancelled[..3], "the lanes in play");
        assert_ne!(read[3], cancelled[3], "a lane of the cancelled read");
        session.cancel_plugin_hold();
        assert_eq!(copies_in(folder.path()), 2);

        // Document 3 rests with its lane and its copy, and document 1 plays
        // and is heard. The text waits in document 2, with no lane of its
        // own, for a new copy. The same text in document 3 reads its own lane
        // first: it lands at once on its copy, with no wait.
        session.set_plugin_document(1);
        session.schedule_through(0.7, 0.7).expect("cover");
        let played = session.reload_live(kick, false, &stopped, || 0.75);
        assert_eq!(played.expect("kick"), LiveReload::Installed(generation + 3));
        session
            .mark_audible_generation(generation + 3)
            .expect("heard");
        session.set_plugin_document(2);
        session.schedule_through(0.8, 0.8).expect("cover");
        let reload = session.reload_live(&text, false, &stopped, past_the_cover(0.85));
        assert_eq!(reload.expect("the held text"), LiveReload::Waiting);
        assert!(waits(&mut session), "the wait of document 2");
        session.set_plugin_document(3);
        session.schedule_through(0.9, 0.9).expect("cover");
        let landed = session.reload_live(&text, false, &stopped, || 0.95);
        assert_eq!(
            landed.expect("landing"),
            LiveReload::Installed(generation + 4)
        );
        assert!(!waits(&mut session), "a wait of document 3");
        assert_eq!(session.plugins_waited_for(&text), None);
        assert_eq!(session.lanes.document(), 3);
        // The copy of the lead, the copy of the pad and the new copy of
        // document 2: document 3 builds none.
        assert_eq!(copies_in(folder.path()), 3);

        // A text with a new lane waits in document 4. Document 5 rests with
        // the lanes of that text, and a launch arms it there from those
        // lanes: the arm waits for no copy, and the text lands.
        session.set_plugin_document(4);
        session.schedule_through(1.0, 1.0).expect("cover");
        let reload = session.reload_live(&more, false, &stopped, past_the_cover(1.05));
        assert_eq!(reload.expect("the held text"), LiveReload::Waiting);
        assert!(waits(&mut session), "the wait of document 4");
        session.set_plugin_document(5);
        session.schedule_through(1.1, 1.1).expect("cover");
        let prepared = session.prepare_reload(&more, false, 0.0, &stopped, || 1.1);
        assert!(!prepared.expect("prepare"), "the arm of document 5 waits");
        assert!(!waits(&mut session), "a wait of the arm of document 5");
        let landed = session.reload_live(&more, false, &stopped, || 1.15);
        assert_eq!(
            landed.expect("landing"),
            LiveReload::Installed(generation + 5)
        );
        assert_eq!(session.lanes.document(), 5);
        // The new copy of document 4 joins the 3: document 5 builds none.
        assert_eq!(copies_in(folder.path()), 4);

        // Document 3 rests with the lead on c3 and its copy, and document 1
        // plays and is heard. The lead on e3 waits in document 2 for a new
        // copy. In document 3 the lead on e3 is its own lane with a new
        // body: it keeps the number of that lane, and lands at once on its
        // copy.
        let other = text.replace("c3", "e3");
        session.set_plugin_document(3);
        let own = session.lanes.read(&other);
        session.set_plugin_document(1);
        session.schedule_through(1.2, 1.2).expect("cover");
        let played = session.reload_live(kick, false, &stopped, || 1.25);
        assert_eq!(played.expect("kick"), LiveReload::Installed(generation + 6));
        session
            .mark_audible_generation(generation + 6)
            .expect("heard");
        session.set_plugin_document(2);
        session.schedule_through(1.3, 1.3).expect("cover");
        let reload = session.reload_live(&other, false, &stopped, past_the_cover(1.35));
        assert_eq!(reload.expect("the held text"), LiveReload::Waiting);
        assert!(waits(&mut session), "the wait of document 2");
        session.set_plugin_document(3);
        session.schedule_through(1.4, 1.4).expect("cover");
        let landed = session.reload_live(&other, false, &stopped, || 1.45);
        assert_eq!(
            landed.expect("landing"),
            LiveReload::Installed(generation + 7)
        );
        assert!(!waits(&mut session), "a wait of document 3");
        assert_eq!(session.lanes.read(&other), own, "the lead of document 3");
        // The new copy of document 2 joins the 4: document 3 builds none.
        assert_eq!(copies_in(folder.path()), 5);

        // A text with a new hat and a lane that plays from cycle 16 waits in
        // document 8 for the copy of the hat, and the wait is at its end. A
        // launch arms the text in document 9 from cycle 16. Its arm needs the
        // copy of the late lane, which the hold did not wait for, and waits
        // from the arm with no second line.
        let part = |label: &str, notes: &str| {
            format!("\n{label}: note(\"{notes}\").s(\"sine\").vst(\"rustel fixture\")")
        };
        let sections = format!(
            "{other}{}{}",
            part("hat", "g3"),
            part("late", "<~!16 c5!16>")
        );
        session.set_plugin_document(8);
        session.schedule_through(1.5, 1.5).expect("cover");
        let reload = session.reload_live(&sections, false, &stopped, past_the_cover(1.55));
        assert_eq!(reload.expect("the held text"), LiveReload::Waiting);
        assert!(waits(&mut session), "the wait of document 8");
        let hold = session.plugin_hold.as_mut().expect("the wait");
        hold.progress = hold.progress.checked_sub(PLUGIN_WAIT).expect("uptime");
        session.set_plugin_document(9);
        session.schedule_through(1.6, 1.6).expect("cover");
        let prepared = session.prepare_reload(&sections, false, 16.0, &stopped, || 1.6);
        assert!(prepared.expect("prepare"), "the arm of document 9 waits");
        assert!(!waits(&mut session), "a second wait");
        assert!(
            session.waits_for_plugins(&sections),
            "the wait of document 9"
        );
        // The cancel ends the copies of the hat and of the late lane.
        session.cancel_plugin_hold();
        assert_eq!(copies_in(folder.path()), 5);

        // A text with a new tom waits in document 10. The same text in
        // document 11 takes the read and the plan of the hold, and its
        // evaluation fails on its clock. A text that throws in document 12
        // takes the last read. The same text in document 10 again takes the
        // read and the plan back from document 11, and lands on the copy of
        // the hold.
        let tom = format!("{text}{}", part("tom", "a3"));
        session.set_plugin_document(10);
        session.schedule_through(1.7, 1.7).expect("cover");
        let reload = session.reload_live(&tom, false, &stopped, past_the_cover(1.75));
        assert_eq!(reload.expect("the held text"), LiveReload::Waiting);
        assert!(waits(&mut session), "the wait of document 10");
        session.set_plugin_document(11);
        let failed = session.reload_live(&tom, false, &stopped, || f64::NAN);
        assert!(failed.is_err(), "a clock that fails");
        session.set_plugin_document(12);
        let typo = format!("{tom}\nthrow new Error(\"typo\")");
        let failed = session.reload_live(&typo, false, &stopped, || 1.8);
        assert!(failed.is_err(), "the text that throws");
        ready(&mut session);
        session.set_plugin_document(10);
        session.schedule_through(1.9, 1.9).expect("cover");
        let landed = session.reload_live(&tom, false, &stopped, || 1.95);
        assert_eq!(
            landed.expect("landing"),
            LiveReload::Installed(generation + 8)
        );
        assert!(!waits(&mut session), "a second wait of document 10");
        assert_eq!(session.lanes.document(), 10);
        // The copy of the tom joins the 5: the landing builds none.
        assert_eq!(copies_in(folder.path()), 6);
        // SAFETY: as above.
        unsafe { std::env::remove_var(rustel_vst3_fixture::SLOW_ACTIVATE_ENV) };
    }

    /// The lead of the plugin chains follows their copies. The Delay preset
    /// of the fixture has 480 frames of latency, 10 ms at 48 kHz, and a
    /// chain of 2 such stages has 20 ms. A look at the copies of the score
    /// raises the lead by the lanes that play, and the lead the cover keeps
    /// room for by the lanes that do not play. An edit takes over with the
    /// lead of its plan when that is larger, and its commit sets the lead.
    /// The commit seam keeps room in the cover for the lead in play, or for
    /// the lead of the kept copies when that is larger. An edit with no plan
    /// takes over with the lead of the score in play, and ends it. At a rate
    /// with no copy of the plugin, an edit waits at the seam while its copy
    /// builds, and takes over with the latency of the copy. A look at the
    /// cycles ahead raises the lead the cover keeps room for. A held edit
    /// keeps the lead of its plan for the cover, and a launch its arm
    /// prepared keeps it for its head-room. Both read the copies again: a
    /// copy that becomes ready after the arm gives its latency to the
    /// head-room, and a look of the wait gives it to the cover.
    #[test]
    fn the_lead_of_the_chains_follows_their_copies() {
        let (_one, folder) = fixture_plugins();
        let presets = folder.path().join("presets");
        let presets = presets.join(rustel_vst3_fixture::NAME);
        std::fs::create_dir_all(&presets).expect("preset folder");
        let delay = rustel_vst3_fixture::preset_with_delay(1.0, 0.0, 0.1171875);
        for name in ["Delay", "Late", "Arm"] {
            let file = presets.join(format!("{name}.vstpreset"));
            std::fs::write(file, &delay).expect("preset file");
        }
        let long = rustel_vst3_fixture::preset_with_delay(1.0, 0.0, 1.0);
        std::fs::write(presets.join("Long.vstpreset"), long).expect("preset file");
        let host = crate::vst::host();
        let mut session = Session::new().expect("session");
        session.set_direct_diagnostic_logging(false);
        let relaxed = std::sync::atomic::Ordering::Relaxed;
        session.plugins_live.store(true, relaxed);
        session.plugin_rate.store(48_000, relaxed);
        session.set_continuity_margin(0.01);
        // A lane of `stages` Delay stages that plays `notes`.
        let lane = |notes: &str, stages: usize| {
            let stage = r#".vst("rustel fixture", { preset: "delay" })"#;
            format!("$: note(\"{notes}\").s(\"sine\"){}", stage.repeat(stages))
        };
        let near = |left: f64, right: f64| (left - right).abs() < 1e-9;
        // The cover of the producer: the horizon, the margin and a lead.
        let cover = |session: &Session, lead: f64| {
            near(session.live_producer_schedule_cover(), 0.5 + 0.01 + lead)
        };

        // The lane of 1 stage plays, and the lane of 2 is muted.
        let score = format!("{}\n_{}", lane("c3", 1), lane("e3", 2));
        session.evaluate(&score).expect("score");
        let ceiling = Duration::from_secs(1);
        let started = Instant::now();
        while session.plugins_pending(0.0, 1.0, ceiling) > 0 || session.plugin_lead() == 0.0 {
            assert!(started.elapsed() < Duration::from_secs(20), "no copy");
            host.wait_idle();
            std::thread::sleep(PLUGIN_LOOK_EVERY);
        }
        assert_eq!(copies_in(folder.path()), 3);
        assert!(near(session.plugin_lead(), 0.01));
        assert!(cover(&session, 0.02), "the muted lane");

        // The edit plays the lane of 2 stages: its plan has 20 ms of lead.
        // The takeover waits it in the cover, and the commit sets it. A
        // smaller lead keeps it.
        let stopped = AtomicBool::new(false);
        let floor = LIVE_REPLACEMENT_MIN_HEADROOM.as_secs_f64();
        let edit = |session: &mut Session, text: &str, mini: bool| {
            session.schedule_through(0.0, 0.0).expect("cover");
            let edit = session.reload_live(text, mini, &stopped, || 0.0);
            assert!(matches!(edit, Ok(LiveReload::Installed(_))), "{text}");
            session.take_requery_takeover_time().expect("takeover")
        };
        let both = format!("{}\n{}", lane("c3", 1), lane("e3", 2));
        let takeover = edit(&mut session, &both, false);
        assert!(takeover >= floor + 0.02 - 1e-9, "{takeover}");
        assert!(near(session.plugin_lead(), 0.02));
        assert!(cover(&session, 0.02));
        session.raise_plugin_lead(0.01);
        assert!(near(session.plugin_lead(), 0.02), "a smaller lead");
        // The seam keeps room in the cover for the lead in play, or for the
        // lead of the kept copies when that is larger. So a seam that waits
        // leaves its commit room for the whole lead.
        session.schedule_through(0.0, 0.0).expect("cover");
        let room = session.scheduler.horizon_remaining(0.0) - floor;
        let spare = |session: &Session, lead: f64| {
            let spare = session.seam_time(0.0, LIVE_REPLACEMENT_MIN_HEADROOM, None, lead);
            spare.as_secs_f64()
        };
        assert!(near(spare(&session, 0.0), room - 0.02), "the lead in play");
        let kept = spare(&session, 0.03);
        assert!(near(kept, room - 0.03), "the lead of the kept copies");
        // A mini edit has no plan: it takes over with the lead in play, and
        // its commit ends the lead.
        let takeover = edit(&mut session, "sine", true);
        assert!(takeover >= floor + 0.02 - 1e-9, "{takeover}");
        assert_eq!(session.plugin_lead(), 0.0);
        assert!(cover(&session, 0.0));
        // At 44.1 kHz the plugin has no copy yet, and the host knows no
        // latency of it at that rate. The seam waits while the copy of the
        // edit builds, and the edit takes over with the latency of the copy:
        // 480 frames. The Late preset is the Delay preset under a new name,
        // so its copy is a new build. The build takes 50 ms.
        host.wait_idle();
        // SAFETY: one fixture test runs at a time, and the plugin thread has
        // no work. So no other thread reads the variable now.
        unsafe { std::env::set_var(rustel_vst3_fixture::SLOW_ACTIVATE_ENV, "50") };
        session.plugin_rate.store(44_100, relaxed);
        let first = 480.0 / 44_100.0;
        let late = r#"$: note("c3").s("sine").vst("rustel fixture", { preset: "late" })"#;
        let takeover = edit(&mut session, late, false);
        assert!(takeover >= floor + first - 1e-9, "{takeover}");
        assert!(near(session.plugin_lead(), first));
        session.plugin_rate.store(48_000, relaxed);
        host.wait_idle();
        // SAFETY: as above.
        unsafe { std::env::remove_var(rustel_vst3_fixture::SLOW_ACTIVATE_ENV) };

        // A lane with its first note at cycle 6. The plan of the commit
        // reads 2 cycles, and a look at the cycles ahead finds the note.
        let dry = r#"$: s("sine").gain(0)"#;
        let ahead = format!("{dry}\n{}.mask(\"<0!6 1>\")", lane("c3", 1));
        let edit = session.reload_live(&ahead, false, &stopped, || 0.0);
        assert!(matches!(edit, Ok(LiveReload::Installed(_))));
        assert_eq!(session.plugin_lead(), 0.0);
        assert!(cover(&session, 0.0));
        look_until(&mut session, 0.0, PLAN_CYCLES, false);
        assert_eq!(session.plugin_lead(), 0.0);
        assert!(cover(&session, 0.01), "the cycles ahead");
        session.raise_cover_lead(0.0);
        assert!(cover(&session, 0.01), "a smaller lead");

        // A new lane of 2 stages: its copies build for 800 ms, and the edit
        // waits. The cover keeps room for the lead of the held text.
        host.wait_idle();
        // SAFETY: one fixture test runs at a time, and the plugin thread has
        // no work. So no other thread reads the variable now.
        unsafe { std::env::set_var(rustel_vst3_fixture::SLOW_ACTIVATE_ENV, "400") };
        let held = format!("{ahead}\n{}", lane("g3", 2));
        let edit = session.reload_live(&held, false, &stopped, past_the_cover(0.0));
        assert_eq!(edit.expect("a held edit"), LiveReload::Waiting);
        assert_eq!(session.plugin_lead(), 0.0);
        assert!(cover(&session, 0.02), "the held text");
        assert!(near(session.takeover_headroom(), 0.02));
        // A commit with no seam takes no lead of the held text.
        session.reload_at(&ahead, false, 0.0).expect("a reload");
        assert_eq!(session.plugin_lead(), 0.0);
        session.cancel_plugin_hold();
        host.wait_idle();
        // SAFETY: as above.
        unsafe { std::env::remove_var(rustel_vst3_fixture::SLOW_ACTIVATE_ENV) };

        // A launch its arm prepared fires the lead of its plan earlier.
        assert_eq!(session.takeover_headroom(), 0.0);
        session.set_plugin_wait(false);
        let launch = format!("{dry}\n{}", lane("e3", 2));
        let prepared = session.prepare_reload(&launch, false, 0.0, &stopped, || 0.0);
        assert!(!prepared.expect("prepare"));
        assert!(near(session.takeover_headroom(), 0.02));
        session.cancel_plugin_hold();

        // The Arm preset is the Delay preset under a new name, so no slot
        // holds a copy of it. A launch through 2 stages of it at a rate with
        // no copy builds 2 new copies for 800 ms after its arm. The
        // head-room reads them again: 2 stages of 480 frames are 30 ms at
        // 32 kHz. A launch that waits for its copies looks at them again,
        // and the cover keeps room for their lead: 40 ms at 24 kHz.
        host.wait_idle();
        // SAFETY: one fixture test runs at a time, and the plugin thread has
        // no work. So no other thread reads the variable now.
        unsafe { std::env::set_var(rustel_vst3_fixture::SLOW_ACTIVATE_ENV, "400") };
        session.plugin_rate.store(32_000, relaxed);
        let arm = r#".vst("rustel fixture", { preset: "arm" })"#;
        let launch = format!("{dry}\n$: note(\"a3\").s(\"sine\"){}", arm.repeat(2));
        let prepared = session.prepare_reload(&launch, false, 0.0, &stopped, || 0.0);
        assert!(!prepared.expect("prepare"));
        assert_eq!(session.takeover_headroom(), 0.0, "no copy at the rate");
        host.wait_idle();
        assert!(near(session.takeover_headroom(), 0.03));
        session.cancel_plugin_hold();
        session.set_plugin_wait(true);
        session.plugin_rate.store(24_000, relaxed);
        let prepared = session.prepare_reload(&launch, false, 0.0, &stopped, || 0.0);
        assert!(prepared.expect("prepare"), "the launch waits");
        assert_eq!(session.takeover_headroom(), 0.0);
        host.wait_idle();
        let hold = session.plugin_hold.as_mut().expect("the wait");
        hold.looked = hold.looked.checked_sub(PLUGIN_LOOK_EVERY).expect("uptime");
        assert!(!session.waits_for_plugins(&launch));
        assert!(near(session.takeover_headroom(), 0.04));
        assert!(cover(&session, 0.04), "the copies of the held launch");
        session.cancel_plugin_hold();
        host.wait_idle();
        // SAFETY: as above.
        unsafe { std::env::remove_var(rustel_vst3_fixture::SLOW_ACTIVATE_ENV) };

        // At 8 kHz a stage of the Long preset has 4096 frames, 512 ms. A
        // chain of 2 such stages has 1024 ms, and the lead is 1 s at most.
        session.plugin_rate.store(8_000, relaxed);
        let stage = r#".vst("rustel fixture", { preset: "long" })"#;
        let longest = format!("$: note(\"c3\").s(\"sine\"){}", stage.repeat(2));
        session.reload_at(&longest, false, 0.0).expect("a reload");
        let started = Instant::now();
        while session.plugins_pending(0.0, 1.0, ceiling) > 0 || session.plugin_lead() == 0.0 {
            assert!(started.elapsed() < Duration::from_secs(20), "no copy");
            host.wait_idle();
            std::thread::sleep(PLUGIN_LOOK_EVERY);
        }
        assert_eq!(session.plugin_lead(), 1.0);
    }
}
