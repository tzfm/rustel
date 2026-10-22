use super::*;

impl Session {
    #[cfg(any(feature = "device-audio", test))]
    pub(crate) fn take_pending_producer_turn(&mut self) -> ProducerTurnRecord {
        self.pending_producer_turn.scheduler_queue_depth =
            u32::try_from(self.scheduler.queued()).unwrap_or(u32::MAX);
        self.pending_producer_turn.scheduler_queue_capacity =
            u32::try_from(rustel_scheduler::MAX_QUEUED_EVENTS).unwrap_or(u32::MAX);
        self.pending_producer_turn.scheduler_trace_depth =
            u32::try_from(self.scheduler.trace_queued()).unwrap_or(u32::MAX);
        self.pending_producer_turn.scheduler_trace_capacity =
            u32::try_from(rustel_scheduler::MAX_QUEUED_TRACE_EVENTS).unwrap_or(u32::MAX);
        self.pending_producer_turn.scheduler_trace_drops =
            self.scheduler.trace_events_dropped_total();
        std::mem::take(&mut self.pending_producer_turn)
    }

    #[cfg(any(feature = "device-audio", test))]
    pub(crate) fn record_producer_phase(&mut self, phase: ProducerPhase, elapsed: Duration) {
        self.pending_producer_turn.add_phase(phase, elapsed);
    }

    /// Add producer-side asset installation performed by a live host before
    /// its next scheduling step.
    #[cfg(feature = "device-audio")]
    #[doc(hidden)]
    pub fn record_live_asset_preparation(&mut self, elapsed: Duration) {
        self.record_producer_phase(ProducerPhase::AssetPreparation, elapsed);
    }

    #[cfg(any(feature = "device-audio", test))]
    pub(crate) fn record_producer_ring_push(
        &mut self,
        elapsed: Duration,
        pushed: usize,
        saturated: bool,
    ) {
        self.record_producer_phase(ProducerPhase::RingPush, elapsed);
        self.pending_producer_turn.ring_pushes = self
            .pending_producer_turn
            .ring_pushes
            .saturating_add(u64::try_from(pushed).unwrap_or(u64::MAX));
        self.pending_producer_turn.queue_saturated |= saturated;
    }

    #[cfg(any(feature = "device-audio", test))]
    pub(crate) fn record_producer_cover_age(&mut self, elapsed: Duration) {
        self.pending_producer_turn.cover_age_nanos = self
            .pending_producer_turn
            .cover_age_nanos
            .saturating_add(crate::producer::duration_nanos(elapsed));
    }

    #[cfg(any(feature = "device-audio", test))]
    pub(crate) fn record_atomic_producer_refusal(&mut self) {
        self.pending_producer_turn.atomic_refusal_count = self
            .pending_producer_turn
            .atomic_refusal_count
            .saturating_add(1);
    }

    #[cfg(any(feature = "device-audio", test))]
    fn merge_producer_turn(&mut self, record: ProducerTurnRecord) {
        for phase in ProducerPhase::ALL {
            let index = phase as usize;
            self.pending_producer_turn.phases_nanos[index] =
                self.pending_producer_turn.phases_nanos[index]
                    .saturating_add(record.phases_nanos[index]);
        }
        self.pending_producer_turn.outcome = record.outcome;
        self.pending_producer_turn.cover_start_nanos = record.cover_start_nanos;
        self.pending_producer_turn.cover_end_nanos = record.cover_end_nanos;
        self.pending_producer_turn.cover_gained_nanos = record.cover_gained_nanos;
        self.pending_producer_turn.minimum_grid_nanos = record.minimum_grid_nanos;
        self.pending_producer_turn.continuation_reserve_nanos = record.continuation_reserve_nanos;
        self.pending_producer_turn.budget_granted_nanos = record.budget_granted_nanos;
        self.pending_producer_turn.query_span_millicycles = record.query_span_millicycles;
        self.pending_producer_turn.query_span_nanos = record.query_span_nanos;
        self.pending_producer_turn.haps = record.haps;
        self.pending_producer_turn.scheduler_events = record.scheduler_events;
        self.pending_producer_turn.converted_audio_events = record.converted_audio_events;
        self.pending_producer_turn.js_callback_calls = record.js_callback_calls;
        if record.js_callback_kind_sample.is_some() {
            self.pending_producer_turn.js_callback_kind_sample = record.js_callback_kind_sample;
        }
        self.pending_producer_turn.refused_voices = record.refused_voices;
        self.pending_producer_turn.scheduler_queue_depth = record.scheduler_queue_depth;
        self.pending_producer_turn.scheduler_queue_capacity = record.scheduler_queue_capacity;
        self.pending_producer_turn.scheduler_trace_depth = record.scheduler_trace_depth;
        self.pending_producer_turn.scheduler_trace_capacity = record.scheduler_trace_capacity;
        self.pending_producer_turn.scheduler_trace_drops = record.scheduler_trace_drops;
        self.pending_producer_turn.queue_saturated |= record.queue_saturated;
        self.pending_producer_turn.rollback_count = self
            .pending_producer_turn
            .rollback_count
            .saturating_add(record.rollback_count);
        self.pending_producer_turn.gap_resync_count = self
            .pending_producer_turn
            .gap_resync_count
            .saturating_add(record.gap_resync_count);
        self.pending_producer_turn.cover_age_nanos = self
            .pending_producer_turn
            .cover_age_nanos
            .saturating_add(record.cover_age_nanos);
        self.pending_producer_turn.atomic_refusal_count = self
            .pending_producer_turn
            .atomic_refusal_count
            .saturating_add(record.atomic_refusal_count);
        self.pending_producer_turn.committed_refusal_count = self
            .pending_producer_turn
            .committed_refusal_count
            .saturating_add(record.committed_refusal_count);
        // The reasons those refusals were not pressure ride with them.
        // Dropping them here read every score-caused and library-waiting
        // refusal back as the engine falling behind, and the header went
        // red against numbers that were all healthy.
        self.pending_producer_turn.rejected_score_count = self
            .pending_producer_turn
            .rejected_score_count
            .saturating_add(record.rejected_score_count);
        self.pending_producer_turn.loading_refusal_count = self
            .pending_producer_turn
            .loading_refusal_count
            .saturating_add(record.loading_refusal_count);
    }

    /// What the active score does in the next
    /// [`REPLACEMENT_LOOK_AHEAD_CYCLES`] cycles from `now`. Asked only of a
    /// replacement whose first window refused every onset, to tell a score
    /// nothing can play from one whose first window fell where its sounds
    /// are missing.
    ///
    /// The span starts at `now`, not at the beginning of that cycle: an
    /// onset that has already passed is not evidence the replacement will
    /// play. The rest of the current cycle is still included, so a bank
    /// that alternates by half-cycle is still found.
    ///
    /// It runs on the producer thread, so query and conversion share the
    /// same 15 ms ceiling as [`Self::kick_sample_loads_from`]. A look-ahead
    /// whose query fails answers [`ReplacementLookAhead::Nothing`] and the
    /// replacement is refused as it was before this was asked; one that
    /// runs out of time answers the best evidence it has seen by then,
    /// rather than refusing a score already shown to play. Nothing is
    /// scheduled and nothing is reported: the onsets are converted and
    /// thrown away.
    #[cfg(feature = "device-audio")]
    fn replacement_renders_ahead(
        &self,
        now: f64,
        sample_rate: u32,
        lookup: &dyn rustel_voice::SampleLookup,
    ) -> ReplacementLookAhead {
        const CEILING: Duration = Duration::from_millis(15);
        let cycle = self.cycle_at_time(now);
        if !cycle.is_finite() {
            return ReplacementLookAhead::Nothing;
        }
        let Some(begin) = Fraction::from_f64(cycle.max(0.0)) else {
            return ReplacementLookAhead::Nothing;
        };
        let end = begin.add(Fraction::int(REPLACEMENT_LOOK_AHEAD_CYCLES));
        let deadline = Instant::now() + CEILING;
        let queried = rustel_core::with_query_deadline(deadline, || {
            self.query_with_js_budget(begin, end, CEILING)
        });
        // Not a scheduled window: drop any console the look-ahead produced
        // so those lines cannot surface as the next tick's logs.
        let _ = self.js.take_logs();
        let Ok(haps) = queried else {
            return ReplacementLookAhead::Nothing;
        };
        let cps = self.config.cps;
        let generation = self.generation();
        let (look_ahead, _notices) = rustel_voice::with_diagnostic_policy(false, || {
            // Every onset in the stretch is converted, not only up to the
            // first that renders: an invalid control anywhere ahead
            // refuses the replacement the way the same voice in the first
            // window would.
            let mut rendered = false;
            let mut loading = None;
            let mut invalid_control = None;
            for hap in haps.iter().filter(|hap| hap.has_onset()) {
                if Instant::now() >= deadline {
                    break;
                }
                let duration = hap.duration().to_f64();
                let duration_secs = if cps > 0.0 && duration.is_finite() && duration > 0.0 {
                    duration / cps
                } else {
                    0.25
                };
                let whole_begin = hap.whole_or_part().begin;
                let onset = OnsetEventJson {
                    live_controls: hap.live_controls,
                    onset_id: 0,
                    generation,
                    whole_begin: whole_begin.show(),
                    duration_secs,
                    target_time: self.time_at_cycle(whole_begin),
                    value: ValueJson::from_value(&hap.value),
                    value_show: String::new(),
                    ui_visuals: hap.ui_visuals_context(),
                    log_line: None,
                };
                match self.live_audio_event(&onset, sample_rate, cps, lookup) {
                    Ok(event) => {
                        self.csound.discard(&event.controls);
                        rendered = true;
                    }
                    Err(error @ rustel_voice::VoiceError::SampleLoading(_)) => {
                        loading = Some(error.to_string());
                    }
                    Err(error @ rustel_voice::VoiceError::InvalidControl(_)) => {
                        invalid_control.get_or_insert_with(|| error.to_string());
                    }
                    Err(_) => {}
                }
            }
            if let Some(message) = invalid_control {
                ReplacementLookAhead::InvalidControl(message)
            } else if rendered {
                ReplacementLookAhead::Renders
            } else if let Some(message) = loading {
                ReplacementLookAhead::Loading(message)
            } else {
                ReplacementLookAhead::Nothing
            }
        });
        look_ahead
    }

    /// Follow an outside clock: from `now` the scheduler runs at `cps` with
    /// `cycle` at `now`, and event durations and synced effects convert at
    /// the same rate. A steer the scheduler refuses changes neither rate.
    /// Re-query afterwards ([`Self::requery_active_at`]) so the device hears it.
    pub fn retime(&mut self, now: f64, cps: f64, cycle: f64) {
        self.scheduler.retime(now, cps, cycle);
        self.config.cps = self.scheduler.cps();
    }

    /// Make the next live reload take over at `time` (device seconds):
    /// the old generation keeps sounding until then, the new one is queried
    /// from there. Consumed by that reload.
    /// Drop a takeover time the reload it was set for never consumed.
    pub fn clear_next_takeover_time(&mut self) {
        self.takeover_override = None;
    }

    pub fn set_next_takeover_time(&mut self, time: f64) {
        self.takeover_override = Some(time);
    }

    /// Play the next live reload's score from its own beginning.
    ///
    /// A reload normally joins the cycle already running, which keeps a
    /// live edit in time. A different score, such as another scene or
    /// another take, often needs the opposite: to be heard from its first
    /// event, as after a stop and a start.
    ///
    /// The transport does not change. Only the installed score is shifted,
    /// so its cycle zero falls on the moment it takes over. Everything else
    /// that reads the clock continues unchanged: the tail of the replaced
    /// score, the outgoing MIDI clock, the visuals. Consumed by that reload.
    pub fn start_next_from_zero(&mut self) {
        self.next_from_zero = true;
    }

    /// Drop a from-zero request the reload it was set for never consumed.
    pub fn clear_next_from_zero(&mut self) {
        self.next_from_zero = false;
    }

    /// Take back a from-zero request the reload it was set for did not
    /// consume: true when the request was set. An edit that waits for its
    /// plugin copies installs nothing, so its caller keeps the request for
    /// the next try of the edit.
    pub fn take_next_from_zero(&mut self) -> bool {
        std::mem::take(&mut self.next_from_zero)
    }

    /// The live-reload re-query cursor margin: how far past `now` the audio
    /// consumer has already drained events. Old-generation events beyond it
    /// are dropped at the swap and re-scheduled by the new generation, so a
    /// margin at the true consumption frontier closes the reload gap.
    pub fn set_continuity_margin(&mut self, margin: f64) {
        self.continuity_margin = margin.is_finite().then(|| margin.max(0.0));
        self.sync_live_schedule_horizon();
    }

    /// How far ahead of `now` a live tick must query so a reload's takeover
    /// (the consumption frontier) still sits inside already-scheduled cover.
    ///
    /// Default horizon is 0.5 s. WSLg playback latency can grow past that;
    /// takeover then lands after the last queued onset and the tee records a
    /// silent second on the save.
    fn live_schedule_cover(&self) -> f64 {
        let margin = self.continuity_margin.unwrap_or(self.schedule_lead);
        let margin = if margin.is_finite() {
            margin.max(0.0)
        } else {
            0.0
        };
        self.config.horizon + margin
    }

    /// Seconds ahead of `now` a live producer keeps scheduled: the horizon
    /// plus the continuity margin, and one more cycle (capped) for a
    /// callback-bearing score whose cover already reaches the one-cycle query
    /// cap. The extra cycle adds retained batches, not query span.
    #[cfg(feature = "device-audio")]
    pub fn live_producer_schedule_cover(&self) -> f64 {
        let base = self.live_schedule_cover();
        if !self.active_needs_host() {
            return base;
        }
        let cps = self.scheduler.cps();
        if !cps.is_finite() || cps <= 0.0 || base * cps < MAX_LIVE_IMPURE_QUERY_SPAN_CYCLES {
            return base;
        }
        base + (LIVE_IMPURE_BURST_RESERVE_CYCLES / cps)
            .min(LIVE_IMPURE_BURST_RESERVE_MAX.as_secs_f64())
    }

    fn sync_live_schedule_horizon(&mut self) {
        self.scheduler.set_horizon(self.live_schedule_cover());
    }

    /// The re-query cursor time of the newest continuous reload, consumed by
    /// the live producer to publish the device takeover frame alongside the
    /// generation flip. `None` for initial installs and transport restarts
    /// (nothing earlier survives those).
    pub fn take_requery_takeover_time(&mut self) -> Option<f64> {
        self.requery_takeover_time.take()
    }

    /// Consume the newest reload's takeover **and** what the consumer
    /// should do to what sounds under it ([`TakeoverCut`]) - an edit lets
    /// it ring out, an immediate rewind cuts at the flip, a quantised
    /// rewind cuts at the takeover (its countdown plays). Reading these
    /// together keeps the pair atomic from the producer's view: a cut
    /// whose takeover was consumed separately would either fire twice or
    /// never.
    pub fn take_requery_takeover(&mut self) -> Option<(f64, TakeoverCut)> {
        let cut = std::mem::take(&mut self.requery_takeover_cut);
        self.take_requery_takeover_time().map(|time| (time, cut))
    }

    /// Put a consumed `(takeover, cut)` pair back, unchanged.
    ///
    /// The producer's rewind hold uses this: a from-zero first window whose
    /// samples are still loading defers its publication, but the takeover
    /// and its cut must still be consumed by THAT replacement when the
    /// window finally publishes. Reading and re-arming keeps the pair
    /// atomic - the alternative (peeking the cut, consuming the time)
    /// could separate them.
    pub fn rearm_requery_takeover(&mut self, takeover: (f64, TakeoverCut)) {
        self.requery_takeover_time = Some(takeover.0);
        self.requery_takeover_cut = takeover.1;
    }

    /// Slide an unpublished rewind's cycle zero to `now` once its anchor is
    /// behind the clock.
    ///
    /// A rewind's cycle zero is anchored at install: the line for a
    /// quantised launch, the edit instant for an immediate one. The producer
    /// can reach the anchor late: an immediate rewind always does, by the
    /// install-to-publication time; a quantised one does when its evaluation
    /// outlasts its line; any rewind does when it is held for a loading
    /// sample. The first window's onsets are then in the rendered past. A
    /// voice admitted that late starts partway into its sample: the downbeat
    /// loses its attack, and a one-shot shorter than the delay never sounds.
    /// Without the slide, a hold longer than the schedule cover also lets
    /// the gap skip move the cursor past cycle zero, and a retry that
    /// reaches cycle zero queries every missed onset as one burst.
    ///
    /// A restart is the loop from its top, so the mapping re-anchors cycle
    /// zero at `now`, the takeover moves with it, and the cut becomes the
    /// flip's (whatever line it was waiting for has passed; the consumer
    /// silences the old score where the new one begins). The window is
    /// reopened first so nothing an earlier unpublished turn queried stays
    /// marked emitted.
    ///
    /// Returns whether it slid. A takeover still ahead (a quantised launch
    /// mid-countdown) is left on its line.
    #[cfg(feature = "device-audio")]
    pub(crate) fn slide_pending_rewind_anchor(&mut self, now: f64) -> bool {
        if self.requery_takeover_cut == TakeoverCut::None || !now.is_finite() {
            return false;
        }
        let Some(anchor) = self.requery_anchor_time else {
            return false;
        };
        if now <= anchor {
            return false;
        }
        self.scheduler.reopen_window_at(now, anchor);
        self.scheduler.rebase_start_anchor(now);
        self.requery_anchor_time = Some(now);
        if self.requery_takeover_time.is_some() {
            self.requery_takeover_time = Some(now);
        }
        self.requery_takeover_cut = TakeoverCut::AtFlip;
        true
    }

    /// Forget the current replacement's requery anchor: consumed with its
    /// window (published or rolled back), or superseded by a newer install.
    #[cfg(feature = "device-audio")]
    pub(crate) fn clear_requery_anchor(&mut self) {
        self.requery_anchor_time = None;
    }

    /// The live producer published `generation` to its device, to take over
    /// on `takeover_frame` with `cut`. The ledger of latency-compensated
    /// events follows the flip: it keeps what the device keeps of the
    /// generations going out.
    #[cfg(feature = "device-audio")]
    pub(crate) fn audio_takeover_published(
        &mut self,
        generation: u64,
        takeover_frame: u64,
        cut: TakeoverCut,
    ) {
        self.compensated_onsets
            .published(generation, takeover_frame, cut);
        // A cut begins a new timeline. Its first window went out whole, and
        // no later window of it meets a message of the score it cut.
        if cut != TakeoverCut::None {
            self.forget_handed_out();
        }
    }

    /// Forget how far a host handed the OSC and serial messages out. A new
    /// timeline begins: a transport start, a takeover with a cut, or the
    /// refill of an output that discarded its ring.
    #[cfg(feature = "device-audio")]
    pub(super) fn forget_handed_out(&mut self) {
        #[cfg(feature = "osc")]
        self.osc_handed_out.clear();
        #[cfg(feature = "serial")]
        self.serial_handed_out.clear();
    }

    /// Re-open a consumed prefill window whose onsets never all converted.
    ///
    /// A rewind's first window that holds for a loading sample has already
    /// consumed the scheduler's cursor: its onsets, refused and ready alike,
    /// are marked emitted. A plain retry would resume past cycle zero and
    /// skip the restart's first beat. This moves the cursor back to the
    /// rewind's anchor ([`Self::requery_anchor_time`], where cycle zero
    /// sits) and clears this generation's emission marks from there, so the
    /// same span re-queries in full once the samples are ready. Nothing of
    /// the held window reached the device ring, so nothing can double.
    /// An ordinary loading skip has no anchor and reopens at its takeover.
    ///
    /// On success the takeover pair that the producer consumed on the held
    /// turn is re-armed, so the same replacement consumes it when the
    /// reopened window publishes. Returns `false`, with the pair not
    /// re-armed, when there is nothing past the anchor to reopen; the
    /// caller then puts the pair back itself.
    #[cfg(feature = "device-audio")]
    pub fn reopen_loading_window(&mut self, now: f64, takeover: (f64, TakeoverCut)) -> bool {
        // Only a cut takeover has a distinct, earlier anchor. An ordinary
        // loading skip keeps its far-cursor contract: its takeover IS its
        // window's open.
        let anchor = if takeover.1 == TakeoverCut::None {
            takeover.0
        } else {
            self.requery_anchor_time.unwrap_or(takeover.0)
        };
        let opened = self.scheduler.reopen_window_at(now, anchor);
        if opened {
            self.rearm_requery_takeover(takeover);
        }
        opened
    }

    /// Device schedule lead used by continuous live reloads (see
    /// `install_evaluated_score`); hosts set it once the device reports its
    /// playback latency.
    pub fn set_schedule_lead(&mut self, lead: f64) {
        self.schedule_lead = if lead.is_finite() { lead.max(0.0) } else { 0.0 };
        self.sync_live_schedule_horizon();
    }

    /// Limit haps produced by one query in the install probe, scheduler, and
    /// direct/preview query paths. Sessions retain the core default until set.
    pub fn set_query_hap_budget(&mut self, budget: u64) -> Result<(), RuntimeError> {
        if !(1..=rustel_core::DEFAULT_HAP_BUDGET).contains(&budget) {
            return Err(RuntimeError::Message(format!(
                "query hap budget must be between 1 and {}",
                rustel_core::DEFAULT_HAP_BUDGET
            )));
        }
        self.scheduler.set_query_hap_budget(budget);
        Ok(())
    }

    /// Opt in to the scheduler's bounded editor/visualizer observation stream.
    ///
    /// Disabled sessions pay only the scheduler's false branch. The trace is
    /// observational: a slow or absent UI cannot apply back-pressure to the
    /// audio schedule.
    pub fn set_schedule_trace_enabled(&mut self, enabled: bool) {
        self.scheduler.set_trace_enabled(enabled);
    }

    /// Drain accepted onsets waiting for an editor/visualizer client.
    pub fn take_schedule_trace_events(&mut self) -> Vec<rustel_scheduler::ScheduleTraceEvent> {
        self.scheduler.take_trace_events()
    }

    /// Drain trace events into a reusable host buffer.
    pub fn drain_schedule_trace_events_into(
        &mut self,
        events: &mut Vec<rustel_scheduler::ScheduleTraceEvent>,
    ) {
        self.scheduler.drain_trace_events_into(events);
    }

    /// Drain and reset trace records discarded at the scheduler's fixed cap.
    pub fn take_schedule_trace_events_dropped(&mut self) -> u64 {
        self.scheduler.take_trace_events_dropped()
    }

    /// Current cycles-per-second (read-only; changes ride evaluated source).
    pub fn cps(&self) -> f64 {
        self.scheduler.cps()
    }

    /// Wall-clock time of `cycle` under the current transport anchor.
    pub fn time_at_cycle(&self, cycle: Fraction) -> f64 {
        self.scheduler.time_at_cycle(cycle)
    }

    /// Cycle position at wall-clock `time` under the current anchor.
    pub fn cycle_at_time(&self, time: f64) -> f64 {
        self.scheduler.cycle_at_time(time)
    }

    /// The transport, so a caller can STOP a run in progress.
    ///
    /// `play` borrows the session mutably for its whole window, so cancellation
    /// cannot come through `&mut self` - it has to be a handle taken before the
    /// run starts. Stop is immediate by contract: the scheduler discards queued
    /// events rather than delivering them late, so a cancelled run does not
    /// finish playing what it had already scheduled.
    pub fn transport(&self) -> Arc<Transport> {
        self.transport.clone()
    }

    /// Fill and drain one live scheduler instant without resetting its clock,
    /// active graph, generation, or transport.
    ///
    /// This is the reusable boundary for file-watch playback and a future
    /// sample-clock producer. It may enter JavaScript on this caller thread;
    /// the returned values are fully owned plain data suitable for conversion
    /// into the audio ring's POD events off the real-time callback.
    pub fn schedule_at(&mut self, now: f64) -> Result<Vec<OnsetEventJson>, RuntimeError> {
        self.schedule_through(now, now)
    }

    /// Fill at `now` and transfer events through `through` for a downstream
    /// lookahead buffer. The deadline cannot exceed the live schedule cover
    /// (configured horizon plus any continuity margin).
    pub fn schedule_through(
        &mut self,
        now: f64,
        through: f64,
    ) -> Result<Vec<OnsetEventJson>, RuntimeError> {
        self.schedule_through_with_js_budget(now, through, QUERY_JS_CPU_BUDGET)
    }

    /// One scheduling fill with an explicit query-time JavaScript budget.
    ///
    /// Public/offline production routes supply [`QUERY_JS_CPU_BUDGET`], while
    /// the continuous-device route supplies its cover-derived live budget.
    /// The parameter also keeps refusal/state atomicity cheap to exercise in
    /// focused tests.
    pub(super) fn schedule_through_with_js_budget(
        &mut self,
        now: f64,
        through: f64,
        budget: Duration,
    ) -> Result<Vec<OnsetEventJson>, RuntimeError> {
        self.schedule_through_with_js_budget_and_tail(now, through, budget)
            .map(|(events, _)| events)
            .map_err(ScheduleAttemptError::into_runtime_error)
    }

    fn schedule_through_with_js_budget_and_tail(
        &mut self,
        now: f64,
        through: f64,
        budget: Duration,
    ) -> Result<(Vec<OnsetEventJson>, Instant), ScheduleAttemptError> {
        self.schedule_through_with_js_budget_and_tail_with_cover(
            now,
            through,
            budget,
            self.live_schedule_cover(),
            None,
        )
        .map(|window| (window.events, window.tail_started))
    }

    fn schedule_through_with_js_budget_and_tail_with_cover(
        &mut self,
        now: f64,
        through: f64,
        budget: Duration,
        max_cover: f64,
        profile: Option<&mut rustel_scheduler::SchedulerTickProfile>,
    ) -> Result<ScheduledWindow, ScheduleAttemptError> {
        self.with_panic_recovery(now, |session| {
            Ok(
                session.schedule_through_with_js_budget_and_tail_with_cover_inner(
                    now, through, budget, max_cover, profile,
                ),
            )
        })
        .map_err(ScheduleAttemptError::Atomic)?
    }

    fn schedule_through_with_js_budget_and_tail_with_cover_inner(
        &mut self,
        now: f64,
        through: f64,
        budget: Duration,
        max_cover: f64,
        mut profile: Option<&mut rustel_scheduler::SchedulerTickProfile>,
    ) -> Result<ScheduledWindow, ScheduleAttemptError> {
        if !now.is_finite() || now < 0.0 {
            return Err(ScheduleAttemptError::Atomic(RuntimeError::Message(
                format!("scheduler clock must be a finite non-negative number, got {now}"),
            )));
        }
        if !max_cover.is_finite()
            || max_cover < 0.0
            || !through.is_finite()
            || through < now
            || through > now + max_cover
        {
            return Err(ScheduleAttemptError::Atomic(RuntimeError::Message(
                format!(
                    "scheduler transfer deadline must be within [{now}, {}], got {through}",
                    now + max_cover
                ),
            )));
        }
        if !self.js.has_active_pattern() {
            return Err(ScheduleAttemptError::Atomic(RuntimeError::NoPattern));
        }
        self.panic_is_query.set(true);
        #[cfg(any(test, feature = "test-support"))]
        self.panic_if_injected(SessionPanicPoint::Query);
        let clock = VirtualClock::new(now);
        let cancellation = self.transport.stopped_flag();
        let js = &self.js;
        let scheduler = &mut self.scheduler;
        js.set_cycle_time(scheduler.cycle_at_time(now));
        js.set_cps(scheduler.cps());
        let status = js
            .with_runtime_settings(|| {
                rustel_core::with_cancellation(cancellation, || {
                    if js.active_needs_host() {
                        js.with_active_scope_cancellable(budget, cancellation, || {
                            match profile.as_deref_mut() {
                                Some(profile) => scheduler.tick_with_max_span_cycles_profiled(
                                    &clock,
                                    MAX_LIVE_IMPURE_QUERY_SPAN_CYCLES,
                                    profile,
                                ),
                                None => scheduler.tick_with_max_span_cycles(
                                    &clock,
                                    MAX_LIVE_IMPURE_QUERY_SPAN_CYCLES,
                                ),
                            }
                        })
                        .map_err(RuntimeError::from)
                    } else {
                        // Pure graphs need no JavaScript host, but long native
                        // queries still inherit the cancellation scope.
                        Ok(match profile {
                            Some(profile) => scheduler.tick_profiled(&clock, profile),
                            None => scheduler.tick(&clock),
                        })
                    }
                })
            })
            .map_err(ScheduleAttemptError::Atomic)?;
        match status {
            rustel_scheduler::TickStatus::Stopped => {
                return Err(ScheduleAttemptError::Atomic(RuntimeError::Cancelled));
            }
            rustel_scheduler::TickStatus::Refused => {
                let refusal = self.scheduler.refusal().cloned().ok_or_else(|| {
                    ScheduleAttemptError::Atomic(RuntimeError::Message(
                        "scheduler refused without a typed reason".into(),
                    ))
                })?;
                return Err(ScheduleAttemptError::Atomic(refusal.into()));
            }
            rustel_scheduler::TickStatus::QueueFull => {
                return Err(ScheduleAttemptError::Partial(RuntimeError::ResourceLimit(
                    format!(
                        "live scheduling exceeded the queue cap {}",
                        rustel_scheduler::MAX_QUEUED_EVENTS
                    ),
                )));
            }
            rustel_scheduler::TickStatus::Filled | rustel_scheduler::TickStatus::HorizonFull => {}
        }

        // A query that throws yields no haps and is reported, as on
        // strudel.cc. Without the report, a score whose lanes all throw
        // renders nothing and reports success.
        let thrown = self.scheduler.take_thrown();
        let query_threw = thrown.is_some();
        let generation = self.generation();
        if let Some(message) = thrown
            && query_failure_is_news(generation, &message, &mut self.reported_query_throw)
        {
            self.report_diagnostic(
                "query-threw",
                message.clone(),
                serde_json::json!({ "query_threw": { "message": message } }),
            );
        }
        if let Some(message) = self.scheduler.take_callback_failure()
            && query_failure_is_news(generation, &message, &mut self.reported_callback_failure)
        {
            self.report_diagnostic(
                "query-threw",
                message.clone(),
                serde_json::json!({ "query_threw": { "message": message } }),
            );
        }
        // Live playback warns of a stack-contained throw through the
        // callback-failure channel; taken here so no later offline window
        // reports it.
        let _ = self.scheduler.take_contained_throw();

        let tail_started = Instant::now();
        let cps = self.config.cps;
        let drained = self.scheduler.drain_through(&clock, through);
        let events = drained
            .into_iter()
            .map(|event| OnsetEventJson {
                onset_id: event.onset_id,
                generation: event.generation,
                whole_begin: event.whole_begin.show(),
                duration_secs: event.duration.to_f64() / cps,
                target_time: event.target_time,
                value: ValueJson::from_value(&event.value),
                value_show: event.value.show(),
                live_controls: event.live_controls,
                ui_visuals: event.ui_visuals,
                log_line: event.log_line.as_deref().map(str::to_owned),
            })
            .collect();
        Ok(ScheduledWindow {
            events,
            tail_started,
            status,
            query_threw,
        })
    }

    /// Live-device-only scheduler query boundary.
    ///
    /// Steady impure generations derive their QuickJS elapsed-time slice from
    /// the horizon actually left at `now`. The first fill cannot use that
    /// calculation because a newly anchored generation has zero queried
    /// horizon; initial startup retains the fixed ceiling, while a replacement
    /// receives a capped share of the wall time represented by one query.
    #[cfg(feature = "device-audio")]
    pub(crate) fn schedule_audio_live_at(
        &mut self,
        now: f64,
        sample_rate: u32,
        continuation_reserve: Duration,
        mode: LiveQueryBudgetMode,
    ) -> Result<LiveAudioBatch, LiveAudioScheduleError> {
        self.schedule_audio_live_with_cover_at(
            now,
            sample_rate,
            continuation_reserve,
            mode,
            self.live_producer_schedule_cover(),
            true,
        )
    }

    #[cfg(feature = "device-audio")]
    pub(crate) fn schedule_audio_reload_shield_at(
        &mut self,
        now: f64,
        sample_rate: u32,
        continuation_reserve: Duration,
    ) -> Result<LiveAudioBatch, LiveAudioScheduleError> {
        let cps = self.scheduler.cps();
        let extra = if cps.is_finite() && cps > 0.0 {
            (MAX_LIVE_IMPURE_QUERY_SPAN_CYCLES / cps)
                .min(LIVE_RELOAD_SHIELD_MAX_EXTRA.as_secs_f64())
        } else {
            0.0
        };
        // The evaluation after the shield waits for this turn, so the turn
        // looks at no plugin copy.
        self.schedule_audio_live_with_cover_at(
            now,
            sample_rate,
            continuation_reserve,
            LiveQueryBudgetMode::Steady,
            self.live_schedule_cover() + extra,
            false,
        )
    }

    #[cfg(feature = "device-audio")]
    fn schedule_audio_live_with_cover_at(
        &mut self,
        now: f64,
        sample_rate: u32,
        continuation_reserve: Duration,
        mode: LiveQueryBudgetMode,
        cover: f64,
        look_ahead: bool,
    ) -> Result<LiveAudioBatch, LiveAudioScheduleError> {
        // The producer publishes a takeover at this same rate.
        self.live_sample_rate = (sample_rate > 0).then_some(sample_rate);
        // The plugins of the output run at this rate too. A reopen of the
        // output gives a new rate with no new attach.
        #[cfg(feature = "vst")]
        if sample_rate > 0 {
            self.plugin_rate
                .store(sample_rate, std::sync::atomic::Ordering::Relaxed);
        }
        self.with_panic_recovery(now, |session| {
            Ok(session.schedule_audio_live_with_cover_at_guarded(
                now,
                sample_rate,
                continuation_reserve,
                mode,
                cover,
                look_ahead,
            ))
        })
        .map_err(LiveAudioScheduleError::Retryable)?
    }

    #[cfg(feature = "device-audio")]
    fn schedule_audio_live_with_cover_at_guarded(
        &mut self,
        now: f64,
        sample_rate: u32,
        continuation_reserve: Duration,
        mode: LiveQueryBudgetMode,
        cover: f64,
        look_ahead: bool,
    ) -> Result<LiveAudioBatch, LiveAudioScheduleError> {
        let mut record = ProducerTurnRecord {
            cover_start_nanos: crate::producer::seconds_nanos(
                self.scheduler.horizon_remaining(now),
            ),
            minimum_grid_nanos: crate::producer::seconds_nanos(
                self.scheduler.refill_floor_seconds(),
            ),
            continuation_reserve_nanos: crate::producer::duration_nanos(continuation_reserve),
            ..ProducerTurnRecord::default()
        };
        let mut profile = rustel_scheduler::SchedulerTickProfile::default();
        let result = self.schedule_audio_live_with_cover_at_inner(
            now,
            sample_rate,
            continuation_reserve,
            mode,
            cover,
            &mut record,
            &mut profile,
        );

        record.set_phase_nanos(ProducerPhase::Query, profile.query_nanos);
        record.set_phase_nanos(ProducerPhase::JsCallbacks, profile.callback_nanos);
        record.set_phase_nanos(ProducerPhase::Scheduler, profile.acceptance_nanos);
        record.query_span_millicycles = profile.query_span_millicycles;
        let cps = self.scheduler.cps();
        if cps.is_finite() && cps > 0.0 {
            record.query_span_nanos = crate::producer::seconds_nanos(
                profile.query_span_millicycles as f64 / 1000.0 / cps,
            );
        }
        record.haps = profile.queried_haps;
        record.scheduler_events = profile.accepted_events;
        record.js_callback_calls = profile.callback_calls;
        record.js_callback_kind_sample = profile.callback_kind_sample;
        record.queue_saturated |= profile.queue_full;
        let cover_after = self.scheduler.horizon_remaining(now);
        record.cover_end_nanos = crate::producer::seconds_nanos(cover_after);
        record.cover_gained_nanos = record
            .cover_end_nanos
            .saturating_sub(record.cover_start_nanos);
        record.outcome = match &result {
            Ok(_) if profile.horizon_full || record.cover_gained_nanos == 0 => {
                crate::ProducerTurnOutcome::Idle
            }
            Ok(_) => crate::ProducerTurnOutcome::Progress,
            Err(LiveAudioScheduleError::Retryable(RuntimeError::Cancelled)) => {
                crate::ProducerTurnOutcome::Cancelled
            }
            Err(LiveAudioScheduleError::Retryable(_))
            | Err(LiveAudioScheduleError::AwaitingSamples(_)) => {
                crate::ProducerTurnOutcome::AtomicRefusal
            }
            Err(
                LiveAudioScheduleError::CandidateCommitted(_)
                | LiveAudioScheduleError::CandidateRefusedScore(_),
            ) => crate::ProducerTurnOutcome::CommittedRefusal,
        };
        match record.outcome {
            crate::ProducerTurnOutcome::AtomicRefusal => record.atomic_refusal_count = 1,
            crate::ProducerTurnOutcome::CommittedRefusal => record.committed_refusal_count = 1,
            _ => {}
        }
        // A committed refusal that the score caused still latches, rolls
        // back and is reported. It is not the engine falling behind, so the
        // health readings count it separately.
        if result
            .as_ref()
            .err()
            .is_some_and(LiveAudioScheduleError::is_refused_score)
        {
            record.rejected_score_count = 1;
        }
        // Nor is a refusal that is the library still fetching the score's
        // samples: the engine turned nothing away, and the preview waiting
        // on it is not an overload.
        if result
            .as_ref()
            .err()
            .is_some_and(LiveAudioScheduleError::is_awaiting_samples)
        {
            record.loading_refusal_count = 1;
        }
        // A turn of the producer with its cover full looks at the plugin
        // copies of the cycles ahead. A shield does not look. The look caps
        // itself by the cover, so no reserve of the producer holds its time:
        // the tail of the turn starts after it.
        #[cfg(feature = "vst")]
        let result = if look_ahead {
            let looked = self.plugin_turn(now, continuation_reserve, mode, &profile, &mut record);
            result.map(|mut batch| {
                batch.tail_started += looked;
                batch
            })
        } else {
            result
        };
        #[cfg(not(feature = "vst"))]
        let _ = look_ahead;
        self.merge_producer_turn(record);
        result
    }

    #[cfg(feature = "device-audio")]
    #[allow(clippy::too_many_arguments)]
    fn schedule_audio_live_with_cover_at_inner(
        &mut self,
        now: f64,
        sample_rate: u32,
        continuation_reserve: Duration,
        mode: LiveQueryBudgetMode,
        cover: f64,
        record: &mut ProducerTurnRecord,
        profile: &mut rustel_scheduler::SchedulerTickProfile,
    ) -> Result<LiveAudioBatch, LiveAudioScheduleError> {
        if self.transport.is_stopped() {
            return Err(LiveAudioScheduleError::Retryable(RuntimeError::Cancelled));
        }

        let active_needs_host = self.active_needs_host();
        let base_cover = self.live_schedule_cover();
        if !cover.is_finite() || cover < base_cover {
            return Err(LiveAudioScheduleError::Retryable(RuntimeError::Message(
                format!(
                    "live schedule cover must be finite and at least {base_cover}, got {cover}"
                ),
            )));
        }
        // Everything still un-queried from before `now` is unplayable, and
        // querying it is what makes the next tick even more expensive. Drop it
        // and resume at the clock.
        //
        // Not while a rewind is unpublished: its cursor stands at cycle zero
        // on purpose, and skipping it would begin the restart bars in, or
        // publish an empty window while its samples load. The producer
        // slides such a cycle zero to its clock before it queries
        // (`slide_pending_rewind_anchor`), so this never has a gap to skip
        // there anyway; the guard keeps it that way for any caller that
        // does not.
        if self.requery_takeover_cut == TakeoverCut::None
            && let Some(dropped) = self.scheduler.skip_past_gap(now, base_cover)
        {
            record.gap_resync_count = record.gap_resync_count.saturating_add(1);
            let report = self
                .last_gap_log
                .is_none_or(|at| at.elapsed() >= Duration::from_secs(2));
            if report {
                self.last_gap_log = Some(Instant::now());
                let message = "the producer fell behind the audio clock; resumed at the present";
                self.report_diagnostic(
                    "live-recovered",
                    format!("{message} ({:.3}s skipped)", dropped),
                    serde_json::json!({
                        "live_recovered": {
                            "message": message,
                            "dropped_seconds": (dropped * 1000.0).round() / 1000.0,
                        }
                    }),
                );
            }
        }
        let impure_fill_needed = active_needs_host && self.scheduler.horizon_remaining(now) < cover;
        let budget = if impure_fill_needed {
            match self
                .live_query_budget_at(now, continuation_reserve, mode)
                .map_err(LiveAudioScheduleError::Retryable)?
            {
                Some(budget) => budget,
                None if self.transport.is_stopped() => {
                    return Err(LiveAudioScheduleError::Retryable(RuntimeError::Cancelled));
                }
                // DEADLOCK, not a safeguard: this reserve protects the buffered
                // horizon, and by the time it cannot be covered that horizon is
                // already gone. Refusing saves nothing and only a query can
                // refill it, so the refusal becomes self-perpetuating after a
                // long CPU stall. Run a recovery slice instead.
                None => LIVE_QUERY_RECOVERY_BUDGET,
            }
        } else if active_needs_host {
            // The shared helper still opens the bounded host scope before
            // Scheduler reports HorizonFull, so give that non-querying scope
            // a valid token. No callback/getter/materialisation route runs and
            // the unaffordable steady-state calculation is intentionally not
            // consulted.
            MIN_LIVE_QUERY_JS_BUDGET
        } else {
            // Pure graphs remain host-free and therefore need no QuickJS
            // token. Core cancellation still applies in the shared helper.
            Duration::ZERO
        };
        record.budget_granted_nanos = crate::producer::duration_nanos(budget);

        // QuickJS's interrupt handler bounds only a query that enters
        // JavaScript. With no other bound, the pure native route
        // (`active_needs_host == false`) runs `Scheduler::tick` with no
        // ceiling: `chop(32).slow(0.001)` can block the producer thread until
        // the ring drains. A wall-clock deadline over the whole tick turns a
        // too-heavy query into a refusal: the cursor holds, the watchdog
        // rolls back, the music continues. A nested deadline can only
        // tighten, so the JS route keeps its own tighter budget. The deadline
        // is the slice the query itself gets: one that expires before the
        // budget would refuse the recovery it bounds.
        let wall_ceiling = Instant::now()
            + self
                .live_query_budget_at(now, continuation_reserve, mode)
                .ok()
                .flatten()
                .unwrap_or(LIVE_QUERY_RECOVERY_BUDGET)
                .max(MIN_LIVE_QUERY_JS_BUDGET);
        let through = now + cover;
        self.scheduler.set_horizon(cover);
        let query = rustel_core::with_query_deadline(wall_ceiling, || {
            self.schedule_through_with_js_budget_and_tail_with_cover(
                now,
                through,
                budget,
                cover,
                Some(profile),
            )
        });
        self.scheduler.set_horizon(base_cover);
        let ScheduledWindow {
            events: onsets,
            tail_started,
            status,
            query_threw,
        } = query.map_err(|error| match error {
            ScheduleAttemptError::Atomic(error) => LiveAudioScheduleError::Retryable(error),
            ScheduleAttemptError::Partial(error) => {
                LiveAudioScheduleError::CandidateCommitted(error)
            }
        })?;
        // Emit `.log()` text for scheduled onsets. Formatting happens during
        // the pattern query, but emission waits until an onset is accepted
        // so re-querying a span cannot print the same onset twice.
        //
        // Live conversion does not call `schedule_audio_through`, so this
        // path must also drain the onset logs.
        // `logger(...)` and `console.log(...)` drain in the same pass.
        let mut logged: Vec<String> = self.js.take_logs();
        logged.extend(onsets.iter().filter_map(|onset| onset.log_line.clone()));
        for line in logged {
            self.report_diagnostic(
                "log",
                line.clone(),
                serde_json::json!({ "log": { "message": line } }),
            );
        }
        // Same skip-and-log contract as `schedule_audio_through`: the reference
        // drops an unrenderable voice and plays the rest.
        let asset_started = Instant::now();
        self.log_sample_failures();
        let library = self.samples.clone();
        let bundled = rustel_voice::BundledOnly;
        let lookup: &dyn rustel_voice::SampleLookup = match &library {
            Some(library) => library.as_ref(),
            None => &bundled,
        };
        record.add_phase(ProducerPhase::AssetPreparation, asset_started.elapsed());
        let mut refusals = Vec::new();

        #[cfg(feature = "serial")]
        {
            self.collect_pending_serial(&onsets, now);
        }
        // The LIVE path, which is the one a set actually runs: it does its own
        // conversion and never reaches `schedule_audio_through`, so collecting
        // only there meant a live set sent no MIDI at all.
        #[cfg(feature = "midi")]
        {
            self.collect_pending_midi(&onsets);
        }
        let cps = self.config.cps;

        // The LIVE path, which is the one a set actually runs: it does its own
        // conversion and never reaches `schedule_audio_through`, so collecting
        // only there meant a live set sent no OSC at all.
        #[cfg(feature = "osc")]
        self.collect_pending_osc(&onsets, now, cps);

        let external_keys = if matches!(mode, LiveQueryBudgetMode::Steady) {
            Vec::new()
        } else {
            self.pending_external_onset_keys()
        };
        let mut dispositions = confirmation::WindowDispositions {
            intended: u32::try_from(onsets.len()).map_err(|_| {
                LiveAudioScheduleError::CandidateCommitted(RuntimeError::ResourceLimit(
                    "audio confirmation onset count exceeded its bound".into(),
                ))
            })?,
            ..Default::default()
        };
        let mut sample_identities = Vec::new();
        let mut invalid_control = None;
        let mut last_refusal_loading = false;
        let mut input_warnings: Vec<String> = Vec::new();
        // The takeover frame for this window's latency-compensated events
        // (see `handover`): the device keeps the outgoing events aimed
        // before it. A generation not yet published uses its pending
        // takeover. A cut, or no takeover time, keeps nothing.
        let generation = self.generation();
        // Only a new score can be refused in favour of the one that plays.
        let replaces_score = matches!(mode, LiveQueryBudgetMode::ReplacementPrefill)
            && self.requeried_generation != Some(generation);
        // A device that plays its first generation begins a timeline: no
        // earlier generation handed a message out on it.
        if matches!(mode, LiveQueryBudgetMode::InitialPrefill) {
            self.forget_handed_out();
        }
        let pending_takeover = match (self.requery_takeover_time, self.requery_takeover_cut) {
            (Some(time), TakeoverCut::None) => {
                Some(crate::render::takeover_frame_at(time, sample_rate))
            }
            _ => None,
        };
        let takeover_frame = self.compensated_onsets.begin_window(
            generation,
            mode,
            (now * f64::from(sample_rate)).floor().max(0.0) as u64,
            sample_rate,
            pending_takeover,
        );
        let mut compensated: Vec<handover::ConvertedOnset<'_>> = Vec::new();
        // The plain events, for a copy whose effect a save removed. Only a
        // window with a takeover frame can meet such a copy.
        let mut plain: Vec<handover::ConvertedOnset<'_>> = Vec::new();
        // The note of each event, for its plugin chain.
        #[cfg(feature = "vst")]
        let mut values = Vec::new();
        let conversion_started = Instant::now();
        let (mut events, notices) =
            rustel_voice::with_diagnostic_policy(self.direct_diagnostic_logging, || {
                onsets
                    .iter()
                    .filter_map(|onset| {
                        match self.live_audio_event(onset, sample_rate, cps, lookup) {
                            Ok(event) => {
                                #[cfg(feature = "vst")]
                                values.push(&onset.value);
                                if let Some(warning) = self.input_channel_warning(&event)
                                    && !input_warnings.contains(&warning)
                                {
                                    input_warnings.push(warning);
                                }
                                // A latency compensation aimed the voice
                                // before the frame of its onset. A time no
                                // frame holds reads `u64::MAX`.
                                let onset_frame =
                                    crate::render::onset_frame_at(onset.target_time, sample_rate);
                                let converted = handover::ConvertedOnset {
                                    event: dispositions.converted as usize,
                                    whole_begin: &onset.whole_begin,
                                    value: &onset.value_show,
                                    controls: &onset.value,
                                    target_frame: event.target_frame,
                                    onset_frame,
                                };
                                if event.target_frame < onset_frame && onset_frame != u64::MAX {
                                    compensated.push(converted);
                                } else if event.target_frame == onset_frame
                                    && takeover_frame.is_some()
                                {
                                    plain.push(converted);
                                }
                                dispositions.converted += 1;
                                if !matches!(mode, LiveQueryBudgetMode::Steady) {
                                    // Match the scalar source-selection order;
                                    // wavetable bodies also come from SampleBank.
                                    let sample_id = if event.synth.is_some() {
                                        None
                                    } else {
                                        event
                                            .wavetable
                                            .map(|table| table.table)
                                            .or_else(|| event.sample.map(|sample| sample.sample))
                                    };
                                    sample_identities.push(sample_id.and_then(|id| {
                                        library
                                            .as_ref()
                                            .and_then(|library| library.decoded_identity(id))
                                            .or_else(|| {
                                                (id == rustel_audio::BUNDLED_BD_SAMPLE_ID)
                                                    .then_some(
                                                        rustel_audio::BUNDLED_BD_SAMPLE_IDENTITY,
                                                    )
                                            })
                                    }));
                                }
                                Some(event)
                            }
                            Err(error) => {
                                let message = error.to_string();
                                last_refusal_loading =
                                    matches!(error, rustel_voice::VoiceError::SampleLoading(_));
                                if external_keys
                                    .binary_search(&(onset.generation, onset.onset_id))
                                    .is_ok()
                                {
                                    dispositions.external += 1;
                                } else if last_refusal_loading {
                                    dispositions.skipped_loading += 1;
                                } else {
                                    dispositions.refused += 1;
                                    if invalid_control.is_none()
                                        && replaces_score
                                        && matches!(
                                            error,
                                            rustel_voice::VoiceError::InvalidControl(_)
                                        )
                                    {
                                        invalid_control = Some(message.clone());
                                    }
                                }
                                // A run of the same refusal is one report:
                                // `s("nosuchsound*16")` is one mistake, not
                                // sixteen records a window on the command
                                // line's direct log. What decides a hold
                                // downstream is `last_refusal_loading` and
                                // the dispositions, never this list.
                                //
                                // Each refusal carries whether it was the
                                // library still loading, so the report below
                                // can tell waiting from wrong without reading
                                // the prose back.
                                if refusals.last().map(|(_, seen)| seen) != Some(&message) {
                                    refusals.push((last_refusal_loading, message));
                                }
                                None
                            }
                        }
                    })
                    .collect::<Vec<_>>()
            });
        #[cfg(feature = "vst")]
        self.chain_live_events(&mut events, &values);
        self.record_csound_chains(&events);
        // Leave out each compensated event whose onset has an outgoing copy
        // on the device: both copies would play, and the outgoing copy can
        // already sound. A left-out event leaves the converted and the
        // intended counts: like an onset before the takeover, it is not this
        // window's to play. Its MIDI intent stays, because MIDI hands over
        // on the frame of the onset. Its OSC and serial intents follow their
        // own frontier (`leave_out_handed_out_intents`).
        let left_out = match takeover_frame {
            Some(takeover_frame) if !compensated.is_empty() => self
                .compensated_onsets
                .claim_outgoing_copies(generation, takeover_frame, &compensated),
            _ => Vec::new(),
        };
        let mut left_out_events = Vec::new();
        if left_out.contains(&true) {
            let mut left_out = left_out.into_iter();
            compensated.retain(|converted| {
                let stays = !left_out.next().unwrap_or(false);
                if !stays {
                    left_out_events.push(converted.event);
                }
                stays
            });
        }
        // A copy that remains can stand for a plain event: a save removed
        // the effect from its onset. That event is left out the same way.
        if let Some(takeover_frame) = takeover_frame
            && !plain.is_empty()
        {
            let left_out = self.compensated_onsets.claim_outgoing_copies_for_plain(
                generation,
                takeover_frame,
                &plain,
            );
            let plain_events = plain.iter().zip(left_out).filter(|(_, left_out)| *left_out);
            left_out_events.extend(plain_events.map(|(converted, _)| converted.event));
            left_out_events.sort_unstable();
        }
        if !left_out_events.is_empty() {
            for &index in &left_out_events {
                self.csound.discard(&events[index].controls);
            }
            handover::remove_ascending(&mut events, &left_out_events);
            if !sample_identities.is_empty() {
                handover::remove_ascending(&mut sample_identities, &left_out_events);
            }
            let count = u32::try_from(left_out_events.len()).unwrap_or(u32::MAX);
            dispositions.converted = dispositions.converted.saturating_sub(count);
            dispositions.intended = dispositions.intended.saturating_sub(count);
        }
        record.add_phase(ProducerPhase::Conversion, conversion_started.elapsed());
        self.report_voice_notices(notices);
        record.converted_audio_events = u64::try_from(events.len()).unwrap_or(u64::MAX);
        record.refused_voices = u64::try_from(onsets.len() - left_out_events.len())
            .unwrap_or(u64::MAX)
            .saturating_sub(record.converted_audio_events);
        let refused = refusals.last().map(|(_, message)| message.clone());
        // A rewind whose whole first window waits for its samples is
        // reopened and heard whole once they are in, so its loading
        // refusals are notes awaited, not skipped.
        let held_replacement = matches!(mode, LiveQueryBudgetMode::ReplacementPrefill);
        let window_awaited = held_replacement
            && self.requery_takeover_cut != TakeoverCut::None
            && !onsets.is_empty()
            && events.is_empty()
            && !self.has_pending_external_output()
            && last_refusal_loading;
        for (loading, message) in refusals {
            let kind = match (loading, window_awaited) {
                (true, true) => SAMPLE_AWAITED_DIAGNOSTIC,
                (true, false) => SAMPLE_LOADING_DIAGNOSTIC,
                (false, _) => "voice-refused",
            };
            // Every refusal is said once per score text, like a plugin
            // line: a held replacement re-converts its first window on
            // every retry, a live set asks its score every window, and a
            // refusal that stays true is not news each time. The studio
            // still gathers the sounds a load skipped into one line, the
            // command line still says it is waiting, and a refusal first
            // scheduled in a later section lands when its onsets first
            // come up.
            if self.refusals_reported.insert((kind, message.clone())) {
                if loading {
                    self.report_diagnostic(
                        kind,
                        message.clone(),
                        serde_json::json!({ "sample_loading": { "message": message } }),
                    );
                } else {
                    self.report_diagnostic(
                        "voice-refused",
                        message.clone(),
                        serde_json::json!({ "voice_refused": { "message": message } }),
                    );
                }
            }
        }
        for message in input_warnings {
            if self.input_warnings_reported.insert(message.clone()) {
                self.report_diagnostic(
                    "audio-input",
                    message.clone(),
                    serde_json::json!({ "audio_input": { "message": message } }),
                );
            }
        }
        // Invalid controls reject the whole replacement before any sibling
        // reaches the output. Asset skips and external-only onsets keep their
        // separate policies; their diagnostic text is not a failure class.
        if let Some(message) = invalid_control {
            for event in &events {
                self.csound.discard(&event.controls);
            }
            let scope = if events.is_empty() && !self.has_pending_external_output() {
                "every onset of"
            } else {
                "a voice in"
            };
            // These intents were collected from this same rejected window.
            // They must not survive for an external host to drain after the
            // audio producer returns its refusal.
            #[cfg(feature = "midi")]
            self.pending_midi.clear();
            #[cfg(feature = "osc")]
            self.pending_osc.clear();
            #[cfg(feature = "serial")]
            self.pending_serial.clear();
            return Err(LiveAudioScheduleError::CandidateRefusedScore(
                RuntimeError::Message(format!(
                    "scalar audio refused {scope} the replacement's first window ({message})"
                )),
            ));
        }
        // In steady state an unrenderable voice is skipped and logged, and
        // the rest plays. A replacement whose first window refuses every
        // onset is different: publishing it would swap a sounding score for
        // one that no compiled output can render. A MIDI, OSC or serial
        // intent is a valid external-only replacement even when native scalar
        // conversion refuses it. The scheduler has already consumed the
        // window, so an unroutable window is a committed candidate: the
        // producer latches it until a new score identity supersedes it.
        //
        // A sample that is still loading does not latch, because the same
        // identity can continue from later scheduling. A first window that
        // falls where the score's sounds are missing does not latch either:
        // `s("ht").bank("BossDR110 AkaiXR10")` has toms only in its second
        // half. The refusal therefore looks a few cycles ahead of `now`.
        //
        //   every onset refused, no external output pending
        //     last refusal is "still loading" -> AwaitingSamples (retry)
        //     not a replacement prefill       -> Ok: skip and log
        //     replacement prefill, look ahead:
        //       Renders                       -> Ok: publish, skip and log
        //       Loading                       -> AwaitingSamples (retry)
        //       InvalidControl or Nothing     -> CandidateRefusedScore
        if !onsets.is_empty()
            && events.is_empty()
            && !self.has_pending_external_output()
            && let Some(message) = &refused
        {
            if last_refusal_loading {
                return Err(LiveAudioScheduleError::AwaitingSamples(
                    RuntimeError::Message(format!(
                        "scalar audio still loading samples ({message})"
                    )),
                ));
            }
            if replaces_score {
                match self.replacement_renders_ahead(now, sample_rate, lookup) {
                    ReplacementLookAhead::Renders => {}
                    ReplacementLookAhead::Loading(loading) => {
                        // The window itself refused nothing as loading, so
                        // nothing above said it: say the sample ahead once,
                        // as awaited, since the update waits for it rather
                        // than skipping its notes.
                        if self
                            .refusals_reported
                            .insert((SAMPLE_AWAITED_DIAGNOSTIC, loading.clone()))
                        {
                            self.report_diagnostic(
                                SAMPLE_AWAITED_DIAGNOSTIC,
                                loading.clone(),
                                serde_json::json!({ "sample_loading": { "message": loading } }),
                            );
                        }
                        return Err(LiveAudioScheduleError::AwaitingSamples(
                            RuntimeError::Message(format!(
                                "scalar audio still loading samples ({loading})"
                            )),
                        ));
                    }
                    ReplacementLookAhead::InvalidControl(message) => {
                        return Err(LiveAudioScheduleError::CandidateRefusedScore(
                            RuntimeError::Message(format!(
                                "scalar audio refused a voice ahead of the \
                                 replacement's first window ({message})"
                            )),
                        ));
                    }
                    ReplacementLookAhead::Nothing => {
                        return Err(LiveAudioScheduleError::CandidateRefusedScore(
                            RuntimeError::Message(format!(
                                "scalar audio refused every onset of the \
                                 replacement's first window ({message})"
                            )),
                        ));
                    }
                }
            }
        }
        // The window goes to the producer: its compensated events are the
        // device's to hold once it publishes them.
        self.compensated_onsets
            .record(generation, mode, &compensated);
        let batch = LiveAudioBatch {
            prefill_progress: status == rustel_scheduler::TickStatus::Filled
                || !events.is_empty()
                || self.has_pending_external_output(),
            events,
            dispositions,
            sample_identities,
            query_threw,
            queried_through_frame: ((now + self.scheduler.horizon_remaining(now)).min(through)
                * f64::from(sample_rate))
            .ceil()
            .max(0.0) as u64,
            tail_started,
        };
        // Last, so the window is judged with every intent it staged: an
        // onset whose message is already out is still an external onset.
        #[cfg(any(feature = "osc", feature = "serial"))]
        self.leave_out_handed_out_intents(sample_rate, &onsets);
        Ok(batch)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::tests::*;
    #[cfg(all(test, feature = "device-audio"))]
    mod voice_refusal_tests {
        use super::*;

        use crate::LiveFileProducer;
        use rustel_audio::device::ManualLiveOutput;
        use rustel_core::{Value, pure, stack};

        const RATE: u32 = 48_000;
        const SOURCE_A: &str =
            include_str!("../../tests/e2e/scores/corpus/regressions/begingate-pulse3.strudel");

        fn confirmed_score() -> (Session, LiveFileProducer, ManualLiveOutput) {
            assert_eq!(
                crate::ui_events::source_revision(SOURCE_A),
                "87e752905cf56ee74faa82520fc8ec32de83053ad10d417c325224f2a4f669d6"
            );
            let mut session = Session::with_config(SessionConfig {
                cps: 1.0,
                horizon: 0.5,
                sample_rate: RATE,
                ..SessionConfig::default()
            })
            .expect("session");
            session.set_direct_diagnostic_logging(false);
            session.set_schedule_lead(0.0);
            session.set_continuity_margin(0.0);
            session
                .evaluate(SOURCE_A)
                .expect("unchanged compatibility fixture A");
            let mut output =
                ManualLiveOutput::new(RATE, session.generation()).expect("manual output");
            session
                .bind_audio_confirmations(output.device().confirmations())
                .expect("bind receipts");
            let mut producer =
                LiveFileProducer::unwatched(Duration::from_millis(2)).expect("producer");
            let mut pcm = [0.0_f32; 256];
            let mut sounded = false;
            for _ in 0..256 {
                let device = output.device();
                producer
                    .step_unwatched_with_clock_and_cutover(
                        &mut session,
                        || device.clock_seconds(),
                        RATE,
                        |generation, takeover, cut| {
                            device.set_generation(generation, takeover, cut)
                        },
                        |event| device.push(event),
                    )
                    .expect("A continuation");
                output.render(&mut pcm);
                assert!(pcm.iter().all(|value| value.is_finite()));
                sounded |= pcm.iter().any(|value| *value != 0.0);
                assert!(session.consume_audio_confirmations());
                if session.audible_source.is_some() {
                    break;
                }
            }
            let audible = session
                .audible_source
                .as_ref()
                .expect("real copied A window");
            assert_eq!(audible.source.as_ref(), SOURCE_A);
            assert_eq!(audible.generation, session.generation());
            assert!(sounded);
            (session, producer, output)
        }

        fn voice(sound: &str, unison: f64) -> Pattern {
            sound_controls(sound, vec![("unison".into(), Value::F64(unison))])
        }

        fn sound_controls(sound: &str, extra: Vec<(String, Value)>) -> Pattern {
            let mut controls = vec![
                ("s".into(), Value::Str(sound.into())),
                ("note".into(), Value::F64(48.0)),
                ("gain".into(), Value::F64(0.05)),
            ];
            controls.extend(extra);
            pure(Value::object(controls)).fast(8.into())
        }

        fn ordered_pair(first: Pattern, second: Pattern, reverse: bool) -> Pattern {
            let mut lanes = vec![first, second];
            if reverse {
                lanes.reverse();
            }
            stack(lanes)
        }

        fn install_candidate(
            session: &mut Session,
            producer: &mut LiveFileProducer,
            output: &ManualLiveOutput,
            candidate: Pattern,
        ) -> u64 {
            let before = session.generation();
            // A native graph is not labelled as replayable source text. Anchor its
            // first query at the actual render frontier without resetting the device.
            session
                .set_pattern_at(candidate, output.device().clock_seconds(), false)
                .expect("native candidate construction succeeds");
            let generation = session.generation();
            producer.arm_replacement(before, generation);
            generation
        }

        fn assert_refused_candidate(
            session: &mut Session,
            producer: &mut LiveFileProducer,
            output: &ManualLiveOutput,
            candidate: Pattern,
        ) {
            install_candidate(session, producer, output, candidate);
            assert_refused_replacement(session, producer, output);
        }

        fn assert_refused_replacement(
            session: &mut Session,
            producer: &mut LiveFileProducer,
            output: &ManualLiveOutput,
        ) {
            let generation_a = session.audible_source.as_ref().unwrap().generation;
            let device = output.device();
            let mut published = Vec::new();
            let mut pushed = Vec::new();
            let result = producer.step_unwatched_with_clock_and_cutover(
                session,
                || device.clock_seconds(),
                RATE,
                |generation, takeover, _cut| {
                    published.push((generation, takeover));
                    device.set_generation(generation, takeover, TakeoverCut::None);
                },
                |event| {
                    pushed.push(event.clone());
                    device.push(event)
                },
            );
            assert!(
                published.is_empty(),
                "refused candidate was published: {published:?}"
            );
            assert!(
                pushed.is_empty(),
                "a valid sibling escaped the refused candidate"
            );
            let error = result.expect_err("one hard voice refusal rejects the entire replacement");
            assert!(
                error.to_string().contains("scalar audio refused")
                    && error.to_string().contains("the replacement's first window")
                    && error
                        .to_string()
                        .contains("the last audible score keeps playing"),
                "{error}"
            );
            assert_eq!(device.generation(), generation_a);
            assert_eq!(session.active_source(), Some(SOURCE_A));
            assert_eq!(
                session.audible_source.as_ref().unwrap().generation,
                generation_a
            );
            #[cfg(feature = "midi")]
            assert!(session.take_pending_midi().is_empty());
            #[cfg(feature = "osc")]
            assert!(session.take_pending_osc().is_empty());
            #[cfg(feature = "serial")]
            assert!(session.take_pending_serial().is_empty());
        }

        /// A channel past every input the engine can read is a score error and
        /// refuses the replacement, as the lint says it will; a channel this input
        /// happens not to have plays silence with a warning, and the siblings
        /// play. Either way a corrected score follows.
        #[test]
        fn out_of_range_channels_refuse_and_missing_ones_play_silence_then_a_correction_follows() {
            for reverse in [false, true] {
                let (mut session, mut producer, output) = confirmed_score();
                session.set_audio_input_channels(Some(2));
                assert_refused_candidate(
                    &mut session,
                    &mut producer,
                    &output,
                    ordered_pair(
                        voice("sine", 1.0),
                        sound_controls("in", vec![("n".into(), Value::F64(2323.0))]),
                        reverse,
                    ),
                );
                let diagnostics = session.take_diagnostics();
                assert!(diagnostics.iter().any(|diagnostic| {
                    diagnostic.kind == "voice-refused" && diagnostic.message.contains("in:2323")
                }));

                let events = publish_candidate(
                    &mut session,
                    &mut producer,
                    &output,
                    ordered_pair(
                        voice("sine", 1.0),
                        sound_controls("in", vec![("n".into(), Value::F64(2.0))]),
                        reverse,
                    ),
                );
                assert!(
                    events.iter().any(|event| matches!(
                        event.synth,
                        Some(rustel_audio::SynthSource::Input { channel: 2 })
                    )),
                    "the missing channel's voice is published, silent"
                );
                let diagnostics = session.take_diagnostics();
                assert!(diagnostics.iter().any(|diagnostic| {
                    diagnostic.kind == "audio-input"
                        && diagnostic.message.contains("in:2 is unavailable")
                        && diagnostic.message.contains("plays silence")
                }));
                assert!(
                    diagnostics
                        .iter()
                        .all(|diagnostic| diagnostic.kind != "voice-refused"),
                    "not a refusal"
                );

                // Input conversion keeps its existing fractional truncation:
                // channel 1.75 is the second channel of this stereo input.
                let events = publish_candidate(
                    &mut session,
                    &mut producer,
                    &output,
                    ordered_pair(
                        voice("sine", 1.0),
                        sound_controls("in", vec![("n".into(), Value::F64(1.75))]),
                        reverse,
                    ),
                );
                assert!(events.iter().any(|event| matches!(
                    event.synth,
                    Some(rustel_audio::SynthSource::Input { channel: 1 })
                )));
            }
        }

        /// A channel the open input does not have is silence with a warning, not
        /// a refusal: the replacement publishes, its siblings play, and the
        /// producer never latches over a microphone that turned out to be mono.
        #[test]
        fn a_missing_input_channel_plays_silence_and_warns_instead_of_refusing() {
            for reverse in [false, true] {
                let (mut session, mut producer, output) = confirmed_score();
                session.set_audio_input_channels(Some(2));
                let input = "s('in').n(sine.range(2, 3))";
                let source = if reverse {
                    format!("stack({input}, s('sine')).fast(16)")
                } else {
                    format!("stack(s('sine'), {input}).fast(16)")
                };
                let previous = session.generation();
                let candidate = session
                    .reload_at(&source, false, output.device().clock_seconds())
                    .expect("valid JavaScript with a computed input channel");
                producer.arm_replacement(previous, candidate);
                let device = output.device();
                let mut published = Vec::new();
                let mut pushed = Vec::new();
                producer
                    .step_unwatched_with_clock_and_cutover(
                        &mut session,
                        || device.clock_seconds(),
                        RATE,
                        |generation, takeover, _cut| {
                            published.push(generation);
                            device.set_generation(generation, takeover, TakeoverCut::None);
                        },
                        |event| {
                            pushed.push(event.clone());
                            device.push(event)
                        },
                    )
                    .expect("the replacement is published");
                assert_eq!(published, [candidate], "published, not refused");
                assert!(
                    pushed.iter().any(|event| matches!(
                        event.synth,
                        Some(rustel_audio::SynthSource::Input { channel: 2 })
                    )),
                    "the input voice is there - the ring reads it as silence"
                );
                assert!(
                    pushed.iter().any(|event| event.synth.is_none()
                        || !matches!(event.synth, Some(rustel_audio::SynthSource::Input { .. }))),
                    "and its sibling plays"
                );
                assert!(session.take_diagnostics().iter().any(|diagnostic| {
                    diagnostic.kind == "audio-input"
                        && diagnostic
                            .message
                            .contains("open input provides channels 0..1 - it plays silence")
                }));
            }
        }

        #[test]
        fn missing_or_expanded_input_facts_do_not_keep_an_obsolete_channel_limit() {
            for channels in [None, Some(0), Some(4)] {
                let (mut session, mut producer, output) = confirmed_score();
                session.set_audio_input_channels(Some(2));
                session.set_audio_input_channels(channels);
                let events = publish_candidate(
                    &mut session,
                    &mut producer,
                    &output,
                    sound_controls("in", vec![("n".into(), Value::F64(3.0))]),
                );
                assert!(!events.is_empty());
                assert!(events.iter().all(|event| matches!(
                    event.synth,
                    Some(rustel_audio::SynthSource::Input { channel: 3 })
                )));
                assert!(
                    session
                        .take_diagnostics()
                        .iter()
                        .all(|diagnostic| diagnostic.kind != "voice-refused")
                );
            }
        }

        /// In every mode a channel the input lacks is a silent voice with a
        /// warning, never a refusal that could latch the producer.
        #[test]
        fn unavailable_input_channels_play_silence_in_every_mode() {
            for mode in [
                LiveQueryBudgetMode::InitialPrefill,
                LiveQueryBudgetMode::Steady,
                LiveQueryBudgetMode::ReplacementPrefill,
            ] {
                let mut session = Session::with_config(SessionConfig {
                    cps: 1.0,
                    horizon: 0.5,
                    sample_rate: RATE,
                    ..SessionConfig::default()
                })
                .unwrap();
                session.set_direct_diagnostic_logging(false);
                session.set_audio_input_channels(Some(2));
                session
                    .set_pattern(ordered_pair(
                        voice("sine", 1.0),
                        sound_controls("in", vec![("n".into(), Value::F64(3.0))]),
                        false,
                    ))
                    .unwrap();
                let batch = session
                    .schedule_audio_live_at(0.0, RATE, Duration::ZERO, mode)
                    .unwrap_or_else(|_| panic!("a missing channel refuses nothing"));
                assert!(!batch.events.is_empty());
                assert_eq!(batch.dispositions.refused, 0, "{mode:?}");
                assert!(
                    batch.events.iter().any(|event| matches!(
                        event.synth,
                        Some(rustel_audio::SynthSource::Input { channel: 3 })
                    )),
                    "{mode:?}: the voice is there, reading silence"
                );
                assert!(session.take_diagnostics().iter().any(|diagnostic| {
                    diagnostic.kind == "audio-input" && diagnostic.message.contains("plays silence")
                }));
            }
        }

        #[test]
        fn explicit_audio_scheduling_warns_about_a_missing_input_channel_the_same_way() {
            for channels in [None, Some(2), Some(4)] {
                let mut session = Session::new().unwrap();
                session.set_direct_diagnostic_logging(false);
                session.set_audio_input_channels(channels);
                session
                    .set_pattern(sound_controls("in", vec![("n".into(), Value::F64(3.0))]))
                    .unwrap();
                let events = session.schedule_audio_at(0.0, RATE).unwrap();
                assert!(
                    !events.is_empty(),
                    "{channels:?}: the voice plays, silent or not"
                );
                let diagnostics = session.take_diagnostics();
                assert!(
                    diagnostics
                        .iter()
                        .all(|diagnostic| diagnostic.kind != "voice-refused")
                );
                assert_eq!(
                    diagnostics
                        .iter()
                        .any(|diagnostic| diagnostic.kind == "audio-input"),
                    channels == Some(2),
                    "{channels:?}"
                );
            }
        }

        fn publish_candidate(
            session: &mut Session,
            producer: &mut LiveFileProducer,
            output: &ManualLiveOutput,
            candidate: Pattern,
        ) -> Vec<rustel_audio::QueuedAudioEvent> {
            let generation = install_candidate(session, producer, output, candidate);
            let device = output.device();
            let mut publications = Vec::new();
            let mut events = Vec::new();
            producer
                .step_unwatched_with_clock_and_cutover(
                    session,
                    || device.clock_seconds(),
                    RATE,
                    |selected, takeover, _cut| {
                        publications.push(selected);
                        device.set_generation(selected, takeover, TakeoverCut::None);
                    },
                    |event| {
                        assert!(device.push(event.clone()));
                        events.push(event);
                        true
                    },
                )
                .expect("permitted candidate prefill");
            assert_eq!(publications, [generation]);
            assert_eq!(device.generation(), generation);
            assert_eq!(
                session.audible_source.as_ref().unwrap().source.as_ref(),
                SOURCE_A,
                "publication alone must not replace the copied source"
            );
            events
        }

        #[test]
        fn mixed_hard_voice_refusal_keeps_the_consumer_confirmed_score() {
            for reverse in [false, true] {
                let (mut session, mut producer, mut output) = confirmed_score();
                assert_refused_candidate(
                    &mut session,
                    &mut producer,
                    &output,
                    ordered_pair(voice("sine", 1.0), voice("supersaw", 100.0), reverse),
                );

                // The exact ceiling remains playable, and a corrected edit is not
                // poisoned by the refused candidate or its re-armed rollback.
                let events = publish_candidate(
                    &mut session,
                    &mut producer,
                    &output,
                    voice("supersaw", 32.0),
                );
                assert!(!events.is_empty());
                assert!(events.iter().all(|event| matches!(
                    event.synth,
                    Some(rustel_audio::SynthSource::Supersaw { voices, .. }) if voices == 32.0
                )));
                let mut pcm = [0.0_f32; 256];
                let mut sounded = false;
                for _ in 0..256 {
                    output.render(&mut pcm);
                    assert!(pcm.iter().all(|value| value.is_finite()));
                    sounded |= pcm.iter().any(|value| *value != 0.0);
                    assert!(session.consume_audio_confirmations());
                    if session.audible_source.is_none() {
                        break;
                    }
                }
                assert!(sounded);
                assert!(
                    session.audible_source.is_none(),
                    "copied native graph has no replay text"
                );
                let report = output.device().report();
                assert_eq!(report.callback_errors, 0);
                assert_eq!(report.callback_allocations, 0);
                assert_eq!(report.callback_frees, 0);
                assert_eq!(report.callback_scope_misses, 0);
            }
        }

        #[test]
        fn replacement_preserves_loading_failed_and_unknown_sample_skips() {
            for kind in ["loading", "failed", "unknown"] {
                for reverse in [false, true] {
                    let (mut session, mut producer, output) = confirmed_score();
                    let library =
                        crate::samples::SampleLibrary::with_loading_sample_for_test("held");
                    if kind == "failed" {
                        library.fail_loading_sample_for_test();
                    }
                    session.samples = Some(Arc::new(library));
                    let name = if kind == "unknown" {
                        "unknown-test-sample"
                    } else {
                        "held"
                    };
                    let events = publish_candidate(
                        &mut session,
                        &mut producer,
                        &output,
                        ordered_pair(voice("sine", 1.0), voice(name, 1.0), reverse),
                    );
                    assert!(!events.is_empty(), "{kind}");
                    assert!(events.iter().all(|event| event.sample.is_none()), "{kind}");
                    // A sound still on its way is waiting, not refused, and says so
                    // under its own kind; a failed or unknown one is the score's
                    // problem and keeps the refusal kind.
                    let expected = if kind == "loading" {
                        crate::SAMPLE_LOADING_DIAGNOSTIC
                    } else {
                        "voice-refused"
                    };
                    assert!(
                        session
                            .take_diagnostics()
                            .iter()
                            .any(|diagnostic| diagnostic.kind == expected),
                        "the skipped {kind} sample remains visible as {expected}"
                    );
                }
            }
        }

        /// A replacement whose first window has only missing sounds, but which
        /// plays a few cycles later, is published. The missing onsets are skipped
        /// and reported.
        #[test]
        fn a_first_window_of_missing_sounds_is_published_when_the_score_plays_ahead() {
            let (mut session, mut producer, output) = confirmed_score();
            // `set_pattern` re-anchors `now` at the scheduler's query cursor, so
            // the lane that will be current after install is `scheduled_to_cycle`,
            // not the audible cycle under the old score.
            let cycle = session.scheduled_to_cycle().floor().max(0.0) as usize;
            // The playable lane is the LAST one the look-ahead covers, so the
            // test pins the horizon itself, whatever it is tuned to.
            let horizon = REPLACEMENT_LOOK_AHEAD_CYCLES as usize;
            let lanes = (0..horizon)
                .map(|lane| {
                    if lane == (cycle + horizon - 1) % horizon {
                        voice("sine", 1.0)
                    } else {
                        voice("unknown-test-sample", 1.0)
                    }
                })
                .collect();
            let events = publish_candidate(
                &mut session,
                &mut producer,
                &output,
                rustel_core::slowcat(lanes),
            );
            assert!(
                events.is_empty(),
                "the first window is the missing stretch: {events:?}"
            );
            assert!(
                session.take_diagnostics().iter().any(|diagnostic| {
                    diagnostic.kind == "voice-refused"
                        && diagnostic.message.contains("unknown-test-sample")
                }),
                "the missing sound is still reported"
            );
        }

        /// A look-ahead that finds only a sample still loading does not publish.
        /// The replacement is held as retryable: the last audible score keeps
        /// playing and the candidate stays eligible.
        #[test]
        fn a_first_window_of_missing_sounds_with_only_a_loading_sample_ahead_waits() {
            let (mut session, mut producer, output) = confirmed_score();
            session.samples = Some(Arc::new(
                crate::samples::SampleLibrary::with_loading_sample_for_test("held"),
            ));
            let cycle = session.scheduled_to_cycle().floor().max(0.0) as usize;
            let horizon = REPLACEMENT_LOOK_AHEAD_CYCLES as usize;
            let lanes = (0..horizon)
                .map(|lane| {
                    if lane == (cycle + 1) % horizon {
                        voice("held", 1.0)
                    } else {
                        voice("unknown-test-sample", 1.0)
                    }
                })
                .collect();
            install_candidate(
                &mut session,
                &mut producer,
                &output,
                rustel_core::slowcat(lanes),
            );
            let generation_a = session.audible_source.as_ref().unwrap().generation;
            let device = output.device();
            let mut published = Vec::new();
            let result = producer.step_unwatched_with_clock_and_cutover(
                &mut session,
                || device.clock_seconds(),
                RATE,
                |generation, takeover, cut| {
                    published.push((generation, takeover));
                    device.set_generation(generation, takeover, cut);
                },
                |event| device.push(event),
            );
            assert!(
                published.is_empty(),
                "a loading promise published a score that renders nothing: {published:?}"
            );
            // The refusal is a quiet hold, the way a first window of loading
            // refusals is: Ok with nothing scheduled, so the engine does not read
            // the wait as a failed launch, and the next turn retries.
            let step = result.expect("a loading promise holds quietly");
            assert_eq!(
                (step.scheduled, step.pushed),
                (0, 0),
                "nothing of the held candidate reaches the device"
            );
            assert_eq!(device.generation(), generation_a);
            // No rollback either: the native candidate stays installed and
            // retry-eligible, while the old generation remains the audible one.
            assert_eq!(
                session.audible_source.as_ref().unwrap().generation,
                generation_a
            );
        }

        /// An onset that has already passed in the current cycle is not a reason
        /// to publish: look-ahead starts at `now`, so a score whose only playable
        /// hit is behind the cursor, with nothing in the look-ahead stretch, is
        /// still refused.
        #[test]
        fn a_playable_onset_already_past_does_not_save_a_silent_future() {
            let (mut session, mut producer, output) = confirmed_score();
            let cycle = session.scheduled_to_cycle();
            assert!(
                cycle > cycle.floor(),
                "this cycle's onset must already be behind `now` so look-ahead is asked, got {cycle}"
            );
            let current = cycle.floor().max(0.0) as usize;
            // Twice the look-ahead horizon, so the only playable lane, the one
            // `now` stands in, does not come round again inside it. `pure` gives
            // one onset per lane, at the cycle boundary behind `now`.
            let span = 2 * REPLACEMENT_LOOK_AHEAD_CYCLES as usize;
            let lanes = (0..span)
                .map(|lane| {
                    let sound = if lane == current % span {
                        "sine"
                    } else {
                        "unknown-test-sample"
                    };
                    pure(Value::object(vec![
                        ("s".into(), Value::Str(sound.into())),
                        ("note".into(), Value::F64(48.0)),
                        ("gain".into(), Value::F64(0.05)),
                    ]))
                })
                .collect();
            assert_refused_candidate(
                &mut session,
                &mut producer,
                &output,
                rustel_core::slowcat(lanes),
            );
        }

        /// An invalid control beyond the first window refuses the whole
        /// replacement, as one in the first window does, even beside a sibling
        /// that renders ahead.
        #[test]
        fn an_invalid_control_ahead_refuses_the_replacement() {
            let (mut session, mut producer, output) = confirmed_score();
            let cycle = session.scheduled_to_cycle().floor().max(0.0) as usize;
            let horizon = REPLACEMENT_LOOK_AHEAD_CYCLES as usize;
            let lanes = (0..horizon)
                .map(|lane| {
                    if lane == (cycle + horizon - 1) % horizon {
                        voice("sine", 1.0)
                    } else if lane == (cycle + horizon - 2) % horizon {
                        voice("supersaw", 100.0)
                    } else {
                        voice("unknown-test-sample", 1.0)
                    }
                })
                .collect();
            install_candidate(
                &mut session,
                &mut producer,
                &output,
                rustel_core::slowcat(lanes),
            );
            assert_refused_replacement(&mut session, &mut producer, &output);
        }

        /// The protection that rule exists for stays: a replacement with nothing
        /// to play in its first window or the cycles after it is refused, and the
        /// last audible score keeps playing.
        #[test]
        fn a_replacement_that_plays_nothing_ahead_is_still_refused() {
            let (mut session, mut producer, output) = confirmed_score();
            assert_refused_candidate(
                &mut session,
                &mut producer,
                &output,
                voice("unknown-test-sample", 1.0),
            );
        }

        #[test]
        fn loading_does_not_mask_a_hard_refusal_in_either_lane_order() {
            for reverse in [false, true] {
                let (mut session, mut producer, output) = confirmed_score();
                session.samples = Some(Arc::new(
                    crate::samples::SampleLibrary::with_loading_sample_for_test("held"),
                ));
                assert_refused_candidate(
                    &mut session,
                    &mut producer,
                    &output,
                    ordered_pair(voice("supersaw", 100.0), voice("held", 1.0), reverse),
                );
            }
        }

        #[test]
        fn true_silence_remains_a_valid_replacement_with_samples_loading() {
            let (mut session, mut producer, mut output) = confirmed_score();
            session.samples = Some(Arc::new(
                crate::samples::SampleLibrary::with_loading_sample_for_test("held"),
            ));
            assert!(
                publish_candidate(&mut session, &mut producer, &output, rustel_core::silence())
                    .is_empty()
            );
            let mut pcm = [0.0_f32; 256];
            output.render(&mut pcm);
            assert!(pcm.iter().all(|value| value.is_finite()));
            assert!(session.consume_audio_confirmations());
            assert!(
                session.audible_source.is_none(),
                "the selected zero-onset window reached a real host copy"
            );
        }

        /// One mistake repeated through a window is one refusal record on the
        /// command line's direct log, not one per onset. (The diagnostics queue
        /// collapses a run by itself, so only the direct log can show this.)
        /// A plugin problem is one line for each evaluation, in the queue and on
        /// the direct log. A note the voice refuses takes no plugin chain, and
        /// the notes after it keep their own chains.
        #[test]
        fn a_repeated_refusal_is_one_direct_record_a_window() {
            let mut session = Session::with_config(SessionConfig {
                cps: 1.0,
                horizon: 0.5,
                sample_rate: RATE,
                ..SessionConfig::default()
            })
            .unwrap();
            session.set_direct_diagnostic_logging(false);
            session
                .set_pattern(ordered_pair(
                    voice("sine", 1.0),
                    sound_controls("nosuchsound", Vec::new()),
                    false,
                ))
                .unwrap();
            session.set_direct_diagnostic_logging(true);
            let _ = direct_diagnostics_for_test::take();
            let batch = session
                .schedule_audio_live_at(0.0, RATE, Duration::ZERO, LiveQueryBudgetMode::Steady)
                .unwrap_or_else(|_| panic!("the sine plays, the unknown sound is skipped"));
            assert!(batch.dispositions.refused >= 4, "{:?}", batch.dispositions);
            let refusals = direct_diagnostics_for_test::take()
                .into_iter()
                .filter(|kind| kind == "voice-refused")
                .count();
            assert_eq!(refusals, 1, "one record for one repeated refusal");

            // Across windows the refusal is said once per score text, like a
            // plugin line: the set plays on, and a refusal that stays true is
            // not news each window. A reinstall says it again.
            session.set_direct_diagnostic_logging(false);
            let refusal_count = |session: &mut Session, now: f64| -> usize {
                session
                    .schedule_audio_live_at(now, RATE, Duration::ZERO, LiveQueryBudgetMode::Steady)
                    .unwrap_or_else(|_| panic!("the sine plays, the unknown sound is skipped"));
                session
                    .take_diagnostics()
                    .into_iter()
                    .filter(|diagnostic| diagnostic.kind == "voice-refused")
                    .count()
            };
            session
                .set_pattern(ordered_pair(
                    voice("sine", 1.0),
                    sound_controls("nosuchsound", Vec::new()),
                    false,
                ))
                .unwrap();
            assert_eq!(refusal_count(&mut session, 0.0), 1);
            assert_eq!(refusal_count(&mut session, 0.5), 0);
            session
                .set_pattern(ordered_pair(
                    voice("sine", 1.0),
                    sound_controls("nosuchsound", Vec::new()),
                    false,
                ))
                .unwrap();
            assert_eq!(refusal_count(&mut session, 0.0), 1);

            // Each note of each window gives the plugin notice again.
            session.set_direct_diagnostic_logging(false);
            let effect = Value::object(vec![("name".into(), Value::Str("delay".into()))]);
            let with_plugin = || {
                let effects = Value::List(vec![effect.clone()]);
                sound_controls("sine", vec![("vst".into(), effects)])
            };
            let plugin_lines = |session: &mut Session, now: f64| -> Vec<String> {
                session
                    .schedule_audio_live_at(now, RATE, Duration::ZERO, LiveQueryBudgetMode::Steady)
                    .unwrap_or_else(|_| panic!("a note with no plugin plays"));
                let diagnostics = session.take_diagnostics().into_iter();
                let lines = diagnostics.filter(|diagnostic| diagnostic.kind == PLUGIN_DIAGNOSTIC);
                lines.map(|diagnostic| diagnostic.message).collect()
            };
            session.set_pattern(with_plugin()).unwrap();
            assert_eq!(
                plugin_lines(&mut session, 0.0),
                ["vst skipped: this host has no plugins"]
            );
            assert!(plugin_lines(&mut session, 0.5).is_empty());
            session.set_pattern(with_plugin()).unwrap();
            assert_eq!(plugin_lines(&mut session, 0.0).len(), 1);

            // A text evaluation has its own clear of the set.
            let text = r#"note("c3").s("sine").vst("delay").fast(8)"#;
            for _ in 0..2 {
                session.evaluate(text).unwrap();
                assert_eq!(session.last_path, EvaluateSource::JavaScript);
                assert_eq!(plugin_lines(&mut session, 0.0).len(), 1);
            }

            // The direct log gets one record for each evaluation too.
            session.set_direct_diagnostic_logging(true);
            session.set_pattern(with_plugin()).unwrap();
            assert!(plugin_lines(&mut session, 0.0).is_empty());
            assert!(plugin_lines(&mut session, 0.5).is_empty());
            let records = direct_diagnostics_for_test::take().into_iter();
            assert_eq!(records.filter(|kind| kind == PLUGIN_DIAGNOSTIC).count(), 1);

            // A count of the live output that rises gives a plugin line: the
            // output had no bus for a chain, or a plugin worker ran late. A
            // new output counts from 0.
            #[cfg(feature = "vst")]
            {
                let _plugins = fixture_plugins();
                session.set_direct_diagnostic_logging(false);
                let _ = crate::vst::take_errors();
                let mut output = ManualLiveOutput::new(RATE, session.generation()).expect("output");
                session.attach_plugins(output.device());
                let output_lines = |session: &mut Session| -> Vec<String> {
                    session.report_voice_notices(Vec::new());
                    let diagnostics = session.take_diagnostics().into_iter();
                    let lines =
                        diagnostics.filter(|diagnostic| diagnostic.kind == PLUGIN_DIAGNOSTIC);
                    lines.map(|diagnostic| diagnostic.message).collect()
                };
                let refused = "the output had no room for a plugin chain: 64 chains play at most";
                let late = "a plugin worker ran late, and the audio waited for it";
                // The note of one chain more than the buses finds no bus.
                for chain in 0..=rustel_audio::MAX_INSERT_BUSES as u64 {
                    let mut controls = rustel_audio::OscillatorControls {
                        insert_chain: Some(100 + chain),
                        ..Default::default()
                    };
                    let key = rustel_audio::InsertKey {
                        plugin: 1,
                        preset: 0,
                    };
                    controls.effects[0] = Some(rustel_audio::InsertControls::new(key));
                    assert!(output.device().push(rustel_audio::AudioEvent {
                        onset_id: chain + 1,
                        generation: output.device().generation(),
                        target_frame: 0,
                        onset_lead: 0.0,
                        freq_hz: 220.0,
                        gain: 0.5,
                        duration_secs: 0.05,
                        ui_visuals: 0,
                        controls,
                        sample: None,
                        synth: None,
                        wavetable: None,
                        cut: None,
                    }));
                }
                let mut pcm = [0.0_f32; 256];
                output.render(&mut pcm);
                output.render(&mut pcm);
                assert!(output_lines(&mut session).is_empty());
                rustel_audio::LiveReverbBatch::install_inserts(output.device(), &[]);
                assert_eq!(output_lines(&mut session), [refused]);
                // The names source of the output hears each later count.
                let turn = |refusals, late_jobs| rustel_audio::InsertTurn {
                    refusals,
                    late_jobs,
                    inline_jobs: 0,
                };
                let counts = |session: &mut Session, refusals, late_jobs| -> Vec<String> {
                    crate::vst::named_chains(&session.chains, turn(refusals, late_jobs));
                    output_lines(session)
                };
                assert_eq!(counts(&mut session, 1, 3), [late]);
                session.set_pattern(voice("sine", 1.0)).unwrap();
                assert!(counts(&mut session, 1, 3).is_empty());
                assert_eq!(counts(&mut session, 4, 4), [refused, late]);
                // A new output counts from 0, also over 2 turns.
                session.set_pattern(voice("sine", 1.0)).unwrap();
                crate::vst::named_chains(&session.chains, turn(1, 0));
                assert_eq!(counts(&mut session, 1, 2), [refused, late]);
                // A session with no live output tells nothing.
                session.set_pattern(voice("sine", 1.0)).unwrap();
                let relaxed = std::sync::atomic::Ordering::Relaxed;
                session.plugins_live.store(false, relaxed);
                assert!(counts(&mut session, 2, 3).is_empty());

                // A note the voice refuses takes no chain, in both live
                // paths: each note of b has the chain of b.
                let mut session = Session::with_config(SessionConfig {
                    cps: 1.0,
                    horizon: 0.5,
                    sample_rate: RATE,
                    ..SessionConfig::default()
                })
                .unwrap();
                session.set_direct_diagnostic_logging(false);
                let text = "a: s(\"nosuchsound\").vst(\"delay\")\nb: s(\"sine\").vst(\"delay\")";
                session.evaluate(text).unwrap();
                let batch = session
                    .schedule_audio_live_at(0.0, RATE, Duration::ZERO, LiveQueryBudgetMode::Steady)
                    .unwrap_or_else(|_| panic!("b plays, the unknown sound is skipped"));
                session.restart_transport_at(0.0);
                let through = session.schedule_audio_at(0.0, RATE).expect("events");
                assert!(!batch.events.is_empty() && !through.is_empty());
                let events = batch.events.iter().chain(&through);
                let chains: Vec<Option<u64>> =
                    events.map(|event| event.controls.insert_chain).collect();
                assert!(chains[0].is_some() && chains.iter().all(|chain| *chain == chains[0]));
                let named = crate::vst::named_chains(&session.chains, turn(0, 0)).named;
                assert_eq!(named.len(), 1);
            }
        }

        #[test]
        fn initial_and_steady_windows_still_skip_individual_invalid_voices() {
            for mode in [
                LiveQueryBudgetMode::InitialPrefill,
                LiveQueryBudgetMode::Steady,
            ] {
                for reverse in [false, true] {
                    let mut session = Session::with_config(SessionConfig {
                        cps: 1.0,
                        horizon: 0.5,
                        sample_rate: RATE,
                        ..SessionConfig::default()
                    })
                    .unwrap();
                    session.set_direct_diagnostic_logging(false);
                    session
                        .set_pattern(ordered_pair(
                            voice("sine", 1.0),
                            voice("supersaw", 100.0),
                            reverse,
                        ))
                        .unwrap();
                    let batch = session
                        .schedule_audio_live_at(0.0, RATE, Duration::ZERO, mode)
                        .unwrap_or_else(|_| panic!("ordinary per-onset skip policy"));
                    assert!(!batch.events.is_empty());
                    assert!(batch.dispositions.refused > 0);
                    assert!(batch.dispositions.converted > 0);
                }
            }
        }

        /// A slider move re-queries the score that plays. A refusal of its first
        /// window would roll back to that same score and silence the set.
        #[test]
        fn a_requery_of_the_playing_score_still_skips_an_invalid_voice() {
            let mut session = Session::with_config(SessionConfig {
                cps: 1.0,
                horizon: 0.5,
                sample_rate: RATE,
                ..SessionConfig::default()
            })
            .unwrap();
            session.set_direct_diagnostic_logging(false);
            session
                .set_pattern(ordered_pair(
                    voice("sine", 1.0),
                    voice("supersaw", 100.0),
                    false,
                ))
                .unwrap();
            session
                .schedule_audio_live_at(
                    0.0,
                    RATE,
                    Duration::ZERO,
                    LiveQueryBudgetMode::InitialPrefill,
                )
                .unwrap_or_else(|_| panic!("the first window"));
            session
                .requery_active_at(0.25)
                .expect("requery")
                .expect("a playing score");
            let batch = session
                .schedule_audio_live_at(
                    0.25,
                    RATE,
                    Duration::ZERO,
                    LiveQueryBudgetMode::ReplacementPrefill,
                )
                .unwrap_or_else(|_| panic!("a requery keeps the per-onset skip policy"));
            assert!(batch.dispositions.refused > 0);
            assert!(batch.dispositions.converted > 0);
        }

        #[cfg(feature = "midi")]
        #[test]
        fn external_routing_is_valid_but_cannot_hide_a_hard_native_sibling() {
            for reverse in [false, true] {
                let external = || {
                    sound_controls(
                        "external-only-test-sample",
                        vec![("midiport".into(), Value::Str("test-port".into()))],
                    )
                };
                let (mut session, mut producer, output) = confirmed_score();
                assert_refused_candidate(
                    &mut session,
                    &mut producer,
                    &output,
                    ordered_pair(external(), voice("supersaw", 100.0), reverse),
                );

                // A valid route on the SAME onset preserves the existing external
                // policy, even when that onset cannot become native scalar audio.
                let routed_hard_voice = sound_controls(
                    "supersaw",
                    vec![
                        ("unison".into(), Value::F64(100.0)),
                        ("midiport".into(), Value::Str("test-port".into())),
                    ],
                );
                for routed in [external(), routed_hard_voice] {
                    let (mut session, mut producer, output) = confirmed_score();
                    assert!(
                        publish_candidate(
                            &mut session,
                            &mut producer,
                            &output,
                            ordered_pair(routed, rustel_core::silence(), reverse)
                        )
                        .is_empty()
                    );
                    let generation = session.generation();
                    let intents = session.take_pending_midi();
                    assert!(!intents.is_empty());
                    assert!(intents.iter().all(|intent| intent.generation == generation));
                }
            }
        }

        /// The warning is news once for a score and an input: a second window
        /// says nothing, another input or another score says it again.
        #[test]
        fn a_missing_input_channel_is_warned_about_once() {
            let mut session = Session::with_config(SessionConfig {
                cps: 1.0,
                horizon: 0.5,
                sample_rate: RATE,
                ..SessionConfig::default()
            })
            .unwrap();
            session.set_direct_diagnostic_logging(false);
            session.set_audio_input_channels(Some(2));
            session
                .set_pattern(sound_controls("in", vec![("n".into(), Value::F64(3.0))]))
                .unwrap();
            let warnings = |session: &mut Session| {
                session
                    .take_diagnostics()
                    .into_iter()
                    .filter(|diagnostic| diagnostic.kind == "audio-input")
                    .count()
            };
            session
                .schedule_audio_live_at(0.0, RATE, Duration::ZERO, LiveQueryBudgetMode::Steady)
                .unwrap_or_else(|_| panic!("scheduled"));
            assert_eq!(warnings(&mut session), 1);
            session
                .schedule_audio_live_at(0.5, RATE, Duration::ZERO, LiveQueryBudgetMode::Steady)
                .unwrap_or_else(|_| panic!("scheduled"));
            assert_eq!(
                warnings(&mut session),
                0,
                "the next window says nothing new"
            );
            session.set_audio_input_channels(Some(1));
            session
                .schedule_audio_live_at(1.0, RATE, Duration::ZERO, LiveQueryBudgetMode::Steady)
                .unwrap_or_else(|_| panic!("scheduled"));
            assert_eq!(warnings(&mut session), 1, "another input: news again");
            session
                .set_pattern(sound_controls("in", vec![("n".into(), Value::F64(3.0))]))
                .unwrap();
            session
                .schedule_audio_live_at(1.5, RATE, Duration::ZERO, LiveQueryBudgetMode::Steady)
                .unwrap_or_else(|_| panic!("scheduled"));
            assert_eq!(warnings(&mut session), 1, "another score: news again");
        }
    }

    #[cfg(all(test, feature = "device-audio"))]
    mod retime_tests {
        //! Following an outside clock ([`Session::retime`]).

        use super::*;

        /// A steer moves the conversion tempo with the scheduler: events scheduled
        /// after it last and are spaced one step at the steered rate, and a steer the
        /// scheduler refuses leaves both rates alone.
        #[test]
        fn a_clock_steer_retimes_the_conversion_tempo_alongside_the_scheduler() {
            let mut session = Session::new().expect("session");
            session.evaluate(r#"s("bd*16")"#).expect("score");
            assert_eq!(session.config.cps, DEFAULT_CPS);

            // An outside clock at twice the score's tempo takes over.
            session.retime(9.5, 1.0, session.cycle_at_time(9.5));
            assert_eq!(session.scheduler.cps(), 1.0);
            assert_eq!(
                session.config.cps, 1.0,
                "the conversion tempo stayed at the pre-steer one"
            );

            session.requery_active_at(9.5).expect("control requery");
            let events = session
                .schedule_through(9.5, 10.0)
                .expect("retimed schedule");
            assert!(!events.is_empty(), "the steered score still has music");
            // One sixteenth lasts 1/16 s at 1 cps; at the score's 0.5 cps it would
            // last twice that.
            for event in &events {
                assert!(
                    (event.duration_secs - 1.0 / 16.0).abs() < 1e-9,
                    "a gate length used the pre-steer tempo: {}",
                    event.duration_secs
                );
            }
            for pair in events.windows(2) {
                assert!(
                    (pair[1].target_time - pair[0].target_time - 1.0 / 16.0).abs() < 1e-9,
                    "onset spacing used the pre-steer tempo: {} -> {}",
                    pair[0].target_time,
                    pair[1].target_time
                );
            }

            session.retime(10.0, f64::NAN, 0.0);
            assert_eq!(session.scheduler.cps(), 1.0);
            assert_eq!(session.config.cps, 1.0);
        }
    }

    #[test]
    fn scheduler_query_refusal_is_atomic_and_same_now_retry_fills_the_gap() {
        let mut session = Session::new().expect("session");
        session
            .evaluate(
                r#"
                  globalThis.__schedulerQuerySpin = false;
                  pure('tick').fast(8).fmap(value => {
                    if (globalThis.__schedulerQuerySpin) {
                      while (true) {}
                    }
                    return value;
                  })
                "#,
            )
            .expect("evaluate scheduler value callback");
        assert!(session.active_needs_host(), "fixture must enter QuickJS");

        let tiny_budget = Duration::from_millis(40);
        let initial = session
            .schedule_through_with_js_budget(0.0, 0.0, tiny_budget)
            .expect("initial horizon fill");
        assert_eq!(initial.len(), 1, "fixture should drain only the zero onset");
        assert_eq!(initial[0].target_time, 0.0);
        let queued_before = session.scheduler.queued();
        assert!(
            queued_before > 0,
            "fixture left no queued future onset to protect"
        );

        let retry_now = 0.3;
        let horizon_before = session.scheduler.horizon_remaining(retry_now);
        assert!(
            horizon_before > 0.0 && horizon_before < session.config.horizon,
            "fixture did not leave a partially drained horizon: {horizon_before}"
        );
        session
            .js
            .eval("globalThis.__schedulerQuerySpin = true")
            .expect("arm scheduler runaway");
        let refused = session
            .schedule_through_with_js_budget(retry_now, retry_now, tiny_budget)
            .expect_err("runaway scheduler callback must refuse its tick");
        assert!(
            matches!(refused, RuntimeError::ResourceLimit(_)),
            "scheduler deadline lost the resource-limit channel: {refused:?}"
        );
        assert!(
            session.scheduler.refusal().is_some(),
            "scheduler did not retain the typed refusal"
        );
        assert_eq!(
            session.scheduler.queued(),
            queued_before,
            "refused tick changed the existing event queue"
        );
        assert!(
            (session.scheduler.horizon_remaining(retry_now) - horizon_before).abs() < 1e-12,
            "refused tick advanced the query cursor"
        );

        session
            .js
            .eval("globalThis.__schedulerQuerySpin = false")
            .expect("disarm scheduler runaway");
        let through = retry_now + session.config.horizon;
        let recovered = session
            .schedule_through_with_js_budget(retry_now, through, tiny_budget)
            .expect("same-now scheduler retry");
        assert!(
            recovered
                .iter()
                .any(|event| (event.target_time - 0.5).abs() < 1e-9),
            "same-now retry did not fill the onset skipped by the refusal: {recovered:?}"
        );
        let expected_first_id = initial[0].onset_id + 1;
        assert_eq!(
            recovered.first().map(|event| event.onset_id),
            Some(expected_first_id),
            "refused tick consumed an onset id"
        );
        assert!(
            recovered
                .windows(2)
                .all(|pair| pair[1].onset_id == pair[0].onset_id + 1),
            "onset ids changed across the refused tick: {recovered:?}"
        );
    }

    #[test]
    fn scheduler_query_jobs_are_refused_before_cursor_or_queue_commit() {
        let mut session = Session::new().expect("session");
        session
            .evaluate(
                r#"
                  globalThis.__schedulerQueryQueueJob = true;
                  new Pattern(state => {
                    if (globalThis.__schedulerQueryQueueJob) {
                      queueMicrotask(() => {
                        globalThis.__schedulerQueryJobRan = 1;
                      });
                    }
                    return pure('job').fast(8).query(state);
                  })
                "#,
            )
            .expect("evaluate scheduler pending-job fixture");
        assert!(session.active_needs_host(), "fixture must enter QuickJS");

        let now = 0.0;
        let queued_before = session.scheduler.queued();
        let horizon_before = session.scheduler.horizon_remaining(now);
        let error = session
            .schedule_through_with_js_budget(now, now, Duration::from_millis(100))
            .expect_err("a scheduler callback may not leave a runnable job");
        assert!(
            matches!(error, RuntimeError::ResourceLimit(_)),
            "pending query job lost the resource-limit channel: {error:?}"
        );
        assert!(
            error.to_string().contains("runnable JavaScript jobs"),
            "pending query job lost its typed reason: {error}"
        );
        assert_eq!(
            session.scheduler.queued(),
            queued_before,
            "pending-job refusal changed the scheduler queue"
        );
        assert_eq!(
            session.scheduler.horizon_remaining(now),
            horizon_before,
            "pending-job refusal advanced the scheduler cursor"
        );
        assert_eq!(
            session.js.get_number("__schedulerQueryJobRan"),
            None,
            "refused query job was executed"
        );
        assert!(
            !session.js.jobs_pending(),
            "refused query job remained runnable"
        );

        session
            .js
            .eval("globalThis.__schedulerQueryQueueJob = false")
            .expect("disable pending-job fixture");
        let recovered = session
            .schedule_through_with_js_budget(now, now, Duration::from_millis(100))
            .expect("same-now retry after pending-job refusal");
        assert_eq!(recovered.len(), 1, "retry did not emit the refused onset");
        assert_eq!(recovered[0].onset_id, 0, "refusal consumed an onset id");
        assert_eq!(recovered[0].target_time, 0.0);
        assert_eq!(recovered[0].value_show, "job");
        assert_eq!(session.js.get_number("__schedulerQueryJobRan"), None);
    }

    #[test]
    fn live_query_budget_uses_tempo_aware_prefill_remaining_horizon_floor_and_cap() {
        let reserve = Duration::from_millis(10);
        let mut initial = Session::new().expect("initial session");
        initial.evaluate("note('c4')").expect("initial score");

        assert_eq!(
            initial
                .live_query_budget_at(0.0, reserve, LiveQueryBudgetMode::InitialPrefill)
                .expect("valid initial prefill clock"),
            Some(QUERY_JS_CPU_BUDGET),
            "cold startup lost its separate fixed ceiling"
        );
        assert_eq!(
            initial
                .live_query_budget_at(0.0, reserve, LiveQueryBudgetMode::ReplacementPrefill)
                .expect("valid replacement prefill clock"),
            Some(LIVE_QUERY_RECOVERY_BUDGET),
            "slow-tempo replacement prefill bypassed its bounded recovery cap"
        );
        assert_eq!(
            initial
                .live_query_budget_at(
                    0.0,
                    Duration::from_secs(3),
                    LiveQueryBudgetMode::ReplacementPrefill,
                )
                .expect("large replacement reserve"),
            Some(QUERY_JS_CPU_BUDGET),
            "replacement prefill bypassed the two-second maximum"
        );

        let mut moderate = Session::new().expect("moderate-tempo session");
        moderate
            .evaluate("setCpm(900/4)\nnote('c4')")
            .expect("moderate-tempo score");
        assert!((moderate.cps() - 3.75).abs() < 1e-9);
        assert_eq!(
            moderate
                .live_query_budget_at(0.0, reserve, LiveQueryBudgetMode::ReplacementPrefill)
                .expect("moderate replacement prefill clock"),
            Some(Duration::from_millis(200)),
            "replacement budget did not retain one-quarter-cycle producer headroom"
        );

        let mut extreme = Session::new().expect("extreme-tempo session");
        extreme
            .evaluate("setcps(500)\nnote('c4')")
            .expect("extreme-tempo score");
        assert_eq!(
            extreme
                .live_query_budget_at(0.0, reserve, LiveQueryBudgetMode::ReplacementPrefill)
                .expect("extreme replacement prefill clock"),
            Some(MIN_LIVE_QUERY_JS_BUDGET),
            "an impossible high-CPS replacement escaped the minimum bounded slice"
        );

        let mut session = Session::new().expect("steady session");
        session.evaluate("note('c4')").expect("steady score");
        session
            .schedule_through_with_js_budget(0.0, 0.0, Duration::from_millis(100))
            .expect("fill the default horizon");
        let remaining = session
            .live_query_budget_at(0.2, reserve, LiveQueryBudgetMode::Steady)
            .expect("valid steady clock")
            .expect("partly drained horizon should afford a steady query");
        assert!(
            remaining.abs_diff(Duration::from_millis(270)) <= Duration::from_micros(1),
            "steady budget used nominal horizon instead of the 300ms remaining: {remaining:?}"
        );
        assert!(
            session
                .live_query_budget_at(
                    0.4719,
                    Duration::from_millis(1),
                    LiveQueryBudgetMode::Steady,
                )
                .expect("valid just-above-floor clock")
                .is_some(),
            "a steady query above the 25ms viable slice was refused"
        );
        assert_eq!(
            session
                .live_query_budget_at(
                    0.4721,
                    Duration::from_millis(1),
                    LiveQueryBudgetMode::Steady,
                )
                .expect("valid just-below-floor clock"),
            None,
            "a sub-25ms steady query slice was offered to QuickJS"
        );

        let mut large = Session::with_config(SessionConfig {
            horizon: 10.0,
            ..SessionConfig::default()
        })
        .expect("large-horizon session");
        large.evaluate("note('c4')").expect("large score");
        large
            .schedule_through_with_js_budget(0.0, 0.0, Duration::from_millis(100))
            .expect("fill large horizon");
        assert_eq!(
            large
                .live_query_budget_at(0.0, reserve, LiveQueryBudgetMode::Steady)
                .expect("large valid clock"),
            Some(QUERY_JS_CPU_BUDGET),
            "a large remaining horizon bypassed the live query maximum"
        );

        for invalid in [f64::NAN, f64::INFINITY, -0.001] {
            assert!(
                session
                    .live_query_budget_at(invalid, reserve, LiveQueryBudgetMode::Steady)
                    .is_err(),
                "invalid live query clock {invalid} was accepted"
            );
        }
        assert!(
            session
                .live_query_budget_at(0.0, Duration::ZERO, LiveQueryBudgetMode::Steady)
                .is_err(),
            "zero continuation reserve was accepted"
        );
    }

    #[test]
    #[cfg(feature = "device-audio")]
    fn impure_whole_cycle_live_queries_retain_one_bounded_burst_cycle() {
        let mut high = Session::new().expect("high-CPS session");
        high.evaluate("setCpm(900/4); new Pattern(state => pure('high').query(state))")
            .expect("high-CPS impure score");
        assert!(high.active_needs_host(), "fixture must enter JavaScript");
        let base = high.live_schedule_cover();
        let expected = base + 1.0 / 3.75;
        assert!(
            (high.live_producer_schedule_cover() - expected).abs() < 1e-9,
            "whole-cycle callback score did not receive one burst cycle"
        );

        let mut low = Session::new().expect("low-CPS session");
        low.evaluate("setcps(0.5); new Pattern(state => pure('low').query(state))")
            .expect("low-CPS impure score");
        assert!(low.active_needs_host(), "fixture must enter JavaScript");
        assert_eq!(
            low.live_producer_schedule_cover(),
            low.live_schedule_cover(),
            "sub-cycle queries must keep their exact established windows"
        );

        let mut pure = Session::new().expect("pure session");
        pure.evaluate("setCpm(900/4); pure('native')")
            .expect("high-CPS pure score");
        assert!(!pure.active_needs_host(), "fixture unexpectedly needs JS");
        assert_eq!(
            pure.live_producer_schedule_cover(),
            pure.live_schedule_cover(),
            "native graphs must not pay for a callback burst reserve"
        );
    }

    #[test]
    #[cfg(feature = "device-audio")]
    fn high_cps_impure_live_prefill_is_split_into_deadline_sized_queries() {
        let mut session = Session::new().expect("session");
        session
            .evaluate(
                r#"
                  setCpm(8200/4)
                  register('inspire', (_scale, density, octaves, seed, bars, x) =>
                    x.n(rand.range(0, pure(12).mul(octaves)))
                      .scale(_scale)
                      .sometimesBy(pure(1).sub(density), x => x.mask(rand.round()))
                      .early(rand2.range(-0.001, 0.001))
                      .rib(seed, bars))
                  $: s("sine").seg(8).inspire("<ab:major>", 0.4, 2, 10, 4)
                  $: s("sine").seg(8).inspire("<ab:major>", 0.5, 1, 14, 4)
                  $: s("sine").seg(8).inspire("<ab:major>", 0.9, 1, 14, 2)
                  $: s("sine").seg(8).inspire("<ab:major>", 1, 1/12, 15, 2)
                  $: s("sine").seg(8).inspire("<ab:major>", 1, 1, 70, 4)
                  $: s("sine").seg(16).inspire("<ab:major>", 0.2, 1, 60, 4)
                  $: s("sine").seg(4).inspire("<ab:major>", 1, 2, 42, 2)
                "#,
            )
            .expect("high-CPS score");
        assert!(session.active_needs_host(), "fixture must enter JavaScript");
        // This test pins cycle-sized chunking, not host speed under the Rust
        // test runner's parallel CPU contention. Deadline arithmetic has its
        // own deterministic tests below.
        let query_budget = Duration::from_millis(100);

        let first = session.schedule_audio_live_at(
            0.0,
            48_000,
            query_budget,
            LiveQueryBudgetMode::ReplacementPrefill,
        );
        if let Err(LiveAudioScheduleError::Retryable(error))
        | Err(
            LiveAudioScheduleError::CandidateCommitted(error)
            | LiveAudioScheduleError::CandidateRefusedScore(error),
        ) = first
        {
            panic!("one-cycle replacement prefill was refused: {error}");
        }

        let one_cycle_seconds = 1.0 / session.cps();
        let first_cover = session.scheduler.horizon_remaining(0.0);
        assert!(
            first_cover <= one_cycle_seconds + 1e-6,
            "replacement prefill queried {first_cover:.6}s, more than one cycle ({one_cycle_seconds:.6}s) under one deadline"
        );

        let target_cover = session.live_schedule_cover() - 0.05;
        for _ in 0..32 {
            if session.scheduler.horizon_remaining(0.0) >= target_cover {
                break;
            }
            if let Err(LiveAudioScheduleError::Retryable(error))
            | Err(
                LiveAudioScheduleError::CandidateCommitted(error)
                | LiveAudioScheduleError::CandidateRefusedScore(error),
            ) = session.schedule_audio_live_at(
                0.0,
                48_000,
                query_budget,
                LiveQueryBudgetMode::Steady,
            ) {
                panic!("subsequent high-CPS prefill chunk was refused: {error}");
            }
        }
        assert!(
            session.scheduler.horizon_remaining(0.0) >= target_cover,
            "bounded chunks did not fill the live horizon"
        );
    }

    #[test]
    #[cfg(feature = "device-audio")]
    fn replacement_prefill_can_spend_more_than_25ms_at_a_sustainable_tempo() {
        let mut session = Session::new().expect("session");
        session
            .evaluate(
                r#"
                  setCpm(900/4)
                  note('c4').fmap(value => {
                    const until = Date.now() + 50;
                    while (Date.now() < until) {}
                    return value;
                  })
                "#,
            )
            .expect("bounded replacement fixture");
        assert!((session.cps() - 3.75).abs() < 1e-9);
        assert!(session.active_needs_host(), "fixture must enter JavaScript");
        assert_eq!(
            session
                .live_query_budget_at(
                    0.0,
                    MIN_LIVE_QUERY_JS_BUDGET,
                    LiveQueryBudgetMode::ReplacementPrefill,
                )
                .expect("replacement budget"),
            Some(Duration::from_millis(200))
        );

        let batch = match session.schedule_audio_live_at(
            0.0,
            48_000,
            MIN_LIVE_QUERY_JS_BUDGET,
            LiveQueryBudgetMode::ReplacementPrefill,
        ) {
            Ok(batch) => batch,
            Err(LiveAudioScheduleError::Retryable(error))
            | Err(LiveAudioScheduleError::AwaitingSamples(error))
            | Err(
                LiveAudioScheduleError::CandidateCommitted(error)
                | LiveAudioScheduleError::CandidateRefusedScore(error),
            ) => {
                panic!("sustainable replacement was refused: {error}")
            }
        };
        assert!(
            !batch.events.is_empty(),
            "sustainable replacement scheduled no audio"
        );
        assert!(
            session.scheduler.horizon_remaining(0.0) > 0.0,
            "sustainable replacement did not advance its query cursor"
        );
    }

    #[test]
    #[cfg(feature = "device-audio")]
    fn full_horizon_impure_live_tick_does_not_enter_javascript() {
        let mut session = Session::new().expect("session");
        session
            .evaluate(
                r#"
                  globalThis.__fullHorizonCalls = 0;
                  note('c4').fast(8).fmap(value => {
                    globalThis.__fullHorizonCalls++;
                    return value;
                  })
                "#,
            )
            .expect("impure score");
        session
            .schedule_through_with_js_budget(0.0, 0.0, Duration::from_millis(100))
            .expect("fill horizon once");
        let calls_before = session
            .js
            .get_number("__fullHorizonCalls")
            .expect("callback count after initial fill");

        let result = session.schedule_audio_live_at(
            0.0,
            48_000,
            Duration::from_secs(1),
            LiveQueryBudgetMode::Steady,
        );
        match result {
            Ok(_) => {}
            Err(LiveAudioScheduleError::Retryable(error))
            | Err(LiveAudioScheduleError::AwaitingSamples(error)) => {
                panic!("full horizon was rejected without a query: {error:?}")
            }
            Err(
                LiveAudioScheduleError::CandidateCommitted(error)
                | LiveAudioScheduleError::CandidateRefusedScore(error),
            ) => {
                panic!("full horizon committed a failing candidate: {error:?}")
            }
        }
        assert_eq!(
            session.js.get_number("__fullHorizonCalls"),
            Some(calls_before),
            "HorizonFull entered the impure callback despite needing no fill"
        );
        assert_eq!(session.js.query_depth(), 0, "full-horizon scope leaked");
    }

    #[test]
    #[cfg(feature = "device-audio")]
    fn pure_live_tick_ignores_unaffordable_js_cover_and_remains_host_free() {
        let mut session = Session::new().expect("session");
        session.evaluate("note('c4')").expect("pure score");
        assert!(!session.active_needs_host(), "fixture is not pure");

        let batch = match session.schedule_audio_live_at(
            0.0,
            48_000,
            Duration::from_secs(1),
            LiveQueryBudgetMode::Steady,
        ) {
            Ok(batch) => batch,
            Err(LiveAudioScheduleError::Retryable(error))
            | Err(LiveAudioScheduleError::AwaitingSamples(error)) => {
                panic!("pure live tick required an affordable QuickJS slice: {error:?}")
            }
            Err(
                LiveAudioScheduleError::CandidateCommitted(error)
                | LiveAudioScheduleError::CandidateRefusedScore(error),
            ) => {
                panic!("pure live tick failed after scheduler commit: {error:?}")
            }
        };
        assert_eq!(batch.events.len(), 1);
        assert_eq!(
            session.js.query_depth(),
            0,
            "pure tick opened JS query scope"
        );
        assert_eq!(
            rustel_jsruntime::bridge_frame_depth(),
            0,
            "pure tick opened a bridge frame"
        );
    }

    #[test]
    #[cfg(feature = "device-audio")]
    fn live_ui_trace_keeps_transpiler_byte_ranges_on_audio_onset_ids() {
        let source = r#"$: arrange([1, note("c4 e4")])"#;
        let mut session = Session::with_config(SessionConfig {
            cps: 2.0,
            ..SessionConfig::default()
        })
        .expect("session");
        session.evaluate(source).expect("score");
        session.set_schedule_trace_enabled(true);

        let batch = match session.schedule_audio_live_at(
            0.0,
            48_000,
            Duration::from_secs(1),
            LiveQueryBudgetMode::Steady,
        ) {
            Ok(batch) => batch,
            Err(LiveAudioScheduleError::Retryable(error))
            | Err(LiveAudioScheduleError::AwaitingSamples(error))
            | Err(
                LiveAudioScheduleError::CandidateCommitted(error)
                | LiveAudioScheduleError::CandidateRefusedScore(error),
            ) => {
                panic!("live audio schedule failed: {error}")
            }
        };
        let traces = session.take_schedule_trace_events();

        assert_eq!(batch.events.len(), 2);
        assert_eq!(traces.len(), 2);
        // Each note carries its own span, then the cycle count of its section.
        assert_eq!(traces[0].context, vec![(21, 23), (12, 13)]);
        assert_eq!(&source.as_bytes()[21..23], b"c4");
        assert_eq!(traces[1].context, vec![(24, 26), (12, 13)]);
        assert_eq!(&source.as_bytes()[24..26], b"e4");
        assert_eq!(&source.as_bytes()[12..13], b"1");
        assert!(batch.events.iter().zip(&traces).all(|(audio, trace)| {
            audio.generation == trace.generation && audio.onset_id == trace.onset_id
        }));
    }

    #[test]
    #[cfg(feature = "device-audio")]
    fn live_query_preflight_and_deadline_are_atomic_then_same_now_retry_recovers() {
        let mut session = Session::new().expect("session");
        session
            .evaluate(
                r#"
                  globalThis.__liveBudgetSpin = false;
                  globalThis.__liveBudgetCalls = 0;
                  note('c4').fast(8).fmap(value => {
                    globalThis.__liveBudgetCalls++;
                    if (globalThis.__liveBudgetSpin) while (true) {}
                    return value;
                  })
                "#,
            )
            .expect("live budget fixture");
        let initial = session
            .schedule_through_with_js_budget(0.0, 0.0, Duration::from_millis(100))
            .expect("initial horizon fill");
        let now = 0.39;
        let queued_before = session.scheduler.queued();
        let horizon_before = session.scheduler.horizon_remaining(now);

        session
            .js
            .eval("globalThis.__liveBudgetSpin = true")
            .expect("arm live runaway");
        let started = Instant::now();
        let deadline = session.schedule_audio_live_at(
            now,
            48_000,
            Duration::from_millis(25),
            LiveQueryBudgetMode::Steady,
        );
        let deadline_message = match deadline {
            Err(LiveAudioScheduleError::Retryable(RuntimeError::ResourceLimit(message))) => message,
            _ => panic!("live runaway did not retain its typed atomic refusal"),
        };
        assert!(
            deadline_message.contains("CPU deadline"),
            "{deadline_message}"
        );
        assert!(
            started.elapsed() < Duration::from_millis(500),
            "remaining-horizon query deadline returned too late: {:?}",
            started.elapsed()
        );
        assert_eq!(session.scheduler.queued(), queued_before);
        assert!(
            (session.scheduler.horizon_remaining(now) - horizon_before).abs() < 1e-12,
            "deadline refusal advanced the scheduler cursor"
        );

        session
            .js
            .eval("globalThis.__liveBudgetSpin = false")
            .expect("disarm live runaway");
        let recovered = match session.schedule_audio_live_at(
            now,
            48_000,
            Duration::from_millis(25),
            LiveQueryBudgetMode::Steady,
        ) {
            Ok(batch) => batch,
            Err(LiveAudioScheduleError::Retryable(error))
            | Err(LiveAudioScheduleError::AwaitingSamples(error)) => {
                panic!("same-now live retry did not recover: {error:?}")
            }
            Err(
                LiveAudioScheduleError::CandidateCommitted(error)
                | LiveAudioScheduleError::CandidateRefusedScore(error),
            ) => {
                panic!("same-now live retry failed after commit: {error:?}")
            }
        };
        assert!(!recovered.events.is_empty(), "retry scheduled no events");
        assert_eq!(
            recovered.events.first().map(|event| event.onset_id),
            Some(initial[0].onset_id + 1),
            "atomic refusals consumed an onset id"
        );
    }

    /// A spent horizon must still evaluate a save, not defer it forever.
    ///
    /// Deferring is right while cover remains: the identity stays retryable and
    /// the horizon refills in milliseconds. Once the horizon is GONE the same
    /// refusal repeats on every attempt, and the set freezes on its last good
    /// score while every edit is rejected.
    #[test]
    #[cfg(feature = "device-audio")]
    fn a_spent_horizon_still_evaluates_a_save() {
        let reserve = Duration::from_millis(10);
        let mut session = Session::new().expect("session");
        session.evaluate("note('c4')").expect("score");
        session.schedule_at(0.0).expect("fill the horizon");

        // Cover left: defer, cheaply, and try again in a moment.
        assert_eq!(
            session
                .live_score_budget_at(0.4999, reserve)
                .expect("valid clock"),
            None,
            "a tight but SOUNDING horizon must still defer"
        );

        // Horizon spent: there is nothing left to protect, and no later
        // attempt that could go better.
        let recovered = session
            .live_score_budget_at(0.75, reserve)
            .expect("valid clock")
            .expect("a spent horizon refused the save that could restore it");
        assert!(
            recovered > MIN_LIVE_SCORE_CPU_BUDGET,
            "recovery slice {recovered:?} is no better than the floor that failed"
        );
    }

    /// A drained horizon gets a recovery slice. The 25 ms floor protects a
    /// horizon that still exists. Once it is spent, a dense score needs more
    /// than 25 ms for one window, so the floor alone refuses every attempt.
    #[test]
    #[cfg(feature = "device-audio")]
    fn a_drained_horizon_gets_a_recovery_slice_not_the_steady_state_floor() {
        let mut session = Session::new().expect("session");
        // Each hap costs real milliseconds inside QuickJS, so one window needs
        // far more than the steady-state floor and completes inside a recovery
        // slice - the shape of a dense live score, in miniature.
        session
            .evaluate(
                r#"
                  globalThis.__calls = 0;
                  note('c4').fast(24).fmap(value => {
                    globalThis.__calls++;
                    let sink = 0;
                    for (let i = 0; i < 40000; i++) sink += i;
                    return value;
                  })
                "#,
            )
            .expect("dense fixture");
        session
            .schedule_through_with_js_budget(0.0, 0.0, Duration::from_millis(500))
            .expect("initial horizon fill");

        // Far enough in that the reserve cannot be covered: the horizon this
        // budget protects is gone.
        let now = 0.39;
        assert!(
            session
                .live_query_budget_at(now, Duration::from_millis(29), LiveQueryBudgetMode::Steady)
                .expect("valid clock")
                .is_none(),
            "fixture no longer reaches the unaffordable branch"
        );

        let batch = match session.schedule_audio_live_at(
            now,
            48_000,
            Duration::from_millis(29),
            LiveQueryBudgetMode::Steady,
        ) {
            Ok(batch) => batch,
            Err(LiveAudioScheduleError::Retryable(error))
            | Err(LiveAudioScheduleError::AwaitingSamples(error))
            | Err(
                LiveAudioScheduleError::CandidateCommitted(error)
                | LiveAudioScheduleError::CandidateRefusedScore(error),
            ) => {
                panic!("a dense score could not recover a drained horizon: {error}")
            }
        };
        assert!(
            !batch.events.is_empty(),
            "recovery scheduled nothing: the slice was too small to finish a window"
        );
        assert!(
            LIVE_QUERY_RECOVERY_BUDGET > MIN_LIVE_QUERY_JS_BUDGET,
            "the recovery slice must exceed the steady-state floor"
        );
    }

    /// Falling behind the audio clock must cost the SAME as steady state.
    ///
    /// `Scheduler::tick` queries forward from its cursor, so a producer that
    /// loses time renders music whose moment has passed, and the further
    /// behind it is, the more each tick costs. Once a tick exceeds the query
    /// budget it is refused, the cursor holds, and the gap only grows.
    #[test]
    #[cfg(feature = "device-audio")]
    fn a_producer_far_behind_the_clock_resumes_at_the_present() {
        let mut session = Session::new().expect("session");
        session
            .evaluate(r#"stack(s("bd*4"), note("c3 e3").s("sawtooth").seg(8))"#)
            .expect("score");
        session
            .schedule_through_with_js_budget(0.0, 0.0, Duration::from_millis(100))
            .expect("initial horizon fill");

        // Ten minutes of clock with no scheduling: a stalled producer, or a
        // machine that lost the thread to something else entirely.
        let now = 600.0;
        let cover = session.live_schedule_cover();
        let phase_before = session.scheduler.cycle_at_time(now);

        let started = Instant::now();
        let batch = match session.schedule_audio_live_at(
            now,
            48_000,
            Duration::from_millis(25),
            LiveQueryBudgetMode::Steady,
        ) {
            Ok(batch) => batch,
            Err(LiveAudioScheduleError::Retryable(error))
            | Err(LiveAudioScheduleError::AwaitingSamples(error))
            | Err(
                LiveAudioScheduleError::CandidateCommitted(error)
                | LiveAudioScheduleError::CandidateRefusedScore(error),
            ) => {
                panic!("a producer behind the clock could not schedule at all: {error}")
            }
        };
        let took = started.elapsed();

        assert!(!batch.events.is_empty(), "recovery scheduled no events");
        assert!(
            took < Duration::from_millis(50),
            "catching up on ten minutes of past music took {took:?}; the cost              must not scale with the gap"
        );
        assert!(
            session.scheduler.horizon_remaining(now) >= cover * 0.9,
            "the horizon is still {} of {cover} after recovery: the tick spent              its budget in the past and never reached the present",
            session.scheduler.horizon_remaining(now)
        );
        // Only the CURSOR moves. Restarting the bar would be audible as a
        // dropped beat on every recovery.
        assert!(
            (session.scheduler.cycle_at_time(now) - phase_before).abs() < 1e-9,
            "recovery moved the cycle/time mapping: {} -> {}",
            phase_before,
            session.scheduler.cycle_at_time(now)
        );
    }

    /// An unaffordable continuation reserve does not refuse the query. The
    /// horizon that the reserve protects is already spent, and only a query
    /// refills it.
    #[test]
    #[cfg(feature = "device-audio")]
    fn an_unaffordable_reserve_fills_with_the_minimum_slice_instead_of_deadlocking() {
        let mut session = Session::new().expect("session");
        session
                .evaluate("globalThis.__calls = 0; note('c4').fast(8).fmap(v => { globalThis.__calls++; return v; })")
                .expect("fixture");
        session
            .schedule_through_with_js_budget(0.0, 0.0, Duration::from_millis(100))
            .expect("initial horizon fill");

        // Far enough into the horizon that the reserve cannot be covered.
        let now = 0.39;
        let calls_before = session.js.get_number("__calls").expect("callback count");
        let horizon_before = session.scheduler.horizon_remaining(now);

        let batch = match session.schedule_audio_live_at(
            now,
            48_000,
            Duration::from_millis(29),
            LiveQueryBudgetMode::Steady,
        ) {
            Ok(batch) => batch,
            Err(LiveAudioScheduleError::Retryable(error))
            | Err(LiveAudioScheduleError::AwaitingSamples(error))
            | Err(
                LiveAudioScheduleError::CandidateCommitted(error)
                | LiveAudioScheduleError::CandidateRefusedScore(error),
            ) => panic!(
                "unaffordable reserve refused the only query that can refill the horizon: {error}"
            ),
        };
        assert!(
            session.js.get_number("__calls").unwrap_or(calls_before) > calls_before,
            "minimum-slice fill never entered JavaScript"
        );
        assert!(
            session.scheduler.horizon_remaining(now) > horizon_before,
            "minimum-slice fill did not extend the horizon: {} -> {}",
            horizon_before,
            session.scheduler.horizon_remaining(now)
        );
        assert!(
            !batch.events.is_empty(),
            "minimum-slice fill scheduled nothing"
        );

        // And again once the clock has advanced past the refill: a set that
        // survives one stall must survive the next one too.
        let later = now + 0.4;
        if let Err(LiveAudioScheduleError::Retryable(error))
        | Err(
            LiveAudioScheduleError::CandidateCommitted(error)
            | LiveAudioScheduleError::CandidateRefusedScore(error),
        ) = session.schedule_audio_live_at(
            later,
            48_000,
            Duration::from_millis(29),
            LiveQueryBudgetMode::Steady,
        ) {
            panic!("second unaffordable reserve wedged the producer: {error}");
        }
    }

    #[test]
    fn scheduler_callback_cells_do_not_grow_with_playback_length() {
        // A `BridgeFrame` lives for one `Scheduler::tick` query, not for the
        // whole play loop, so live cells do not grow with playback length.
        // Compare growth: process-wide counters may include unrelated work.
        let (short_created, short_live, short_onsets) = peak_cells_during_play(1.0);
        let (long_created, long_live, long_onsets) = peak_cells_during_play(12.0);

        assert!(
            long_created > short_created * 4,
            "the longer run must actually invoke the callback many more times, \
             or the plateau below is vacuous: {short_created} vs {long_created}"
        );
        assert!(
            long_live <= short_live + 8,
            "callback cells accumulate with playback length: peak {short_live} \
             during 1s, {long_live} during 12s ({short_created} vs \
             {long_created} created). The bridge frame is outliving the tick \
             that created its cells."
        );
        // ...and the output is still right, so the teardown did not simply
        // break the callbacks.
        assert!(short_onsets > 0, "short run scheduled nothing");
        assert!(
            long_onsets > short_onsets,
            "the longer run scheduled no extra onsets: {short_onsets} vs \
             {long_onsets}"
        );
    }

    #[test]
    fn scheduled_callback_output_survives_per_tick_teardown() {
        // Tearing the frame down per tick must not cost correctness: the
        // callback graph is rebuilt on each tick's query and has to keep
        // producing the same events.
        let first = nested_callback_session().play(2.0).expect("play");
        let second = nested_callback_session().play(2.0).expect("play again");
        let show = |r: &PlayReport| {
            r.onsets
                .iter()
                .map(|o| format!("{}@{}", o.value_show, o.whole_begin))
                .collect::<Vec<_>>()
        };
        assert!(!first.onsets.is_empty(), "scheduled nothing");
        assert_eq!(
            show(&first),
            show(&second),
            "replaying the same graph produced different onsets"
        );
    }

    #[test]
    fn every_scheduling_scope_unwinds() {
        // Depth counters back to zero after a successful run, after a run whose
        // callback THROWS, and for a pure graph that opens no scope at all.
        for source in [
            NESTED_CALLBACK_SOURCE,
            r#"pure('bd').polyBind(x => { throw new Error('boom'); })"#,
            r#"s("bd sd")"#,
        ] {
            let mut session = Session::new().expect("session");
            session.ensure_bindings().expect("bindings");
            session.js.install_gc_binding().expect("gc binding");
            session.evaluate(source).expect("evaluate");
            let _ = session.play(2.0).expect("play");
            assert_eq!(session.js.query_depth(), 0, "{source}: query stack");
            assert_eq!(
                rustel_jsruntime::bridge_frame_depth(),
                0,
                "{source}: bridge frame still open"
            );
            assert_eq!(
                rustel_jsruntime::bridge_scratch_len(),
                0,
                "{source}: scratch not drained"
            );
        }
    }

    #[test]
    fn a_dense_tick_cannot_allocate_past_the_scheduler_queue_bound() {
        // `tick` materialises every onset in the horizon span in ONE pass, so a
        // caller-side cap on the finished timeline is checked too late by
        // construction. This asserts the refusal happens and is reported,
        // rather than the process growing into it.
        let mut session = Session::new().expect("session");
        session.evaluate(r#"s("bd*16 sd*16")"#).expect("evaluate");
        let error = session
            .play(86_400.0)
            .expect_err("a dense maximal window must be refused");
        let text = error.to_string();
        assert!(
            text.contains("allocation bound"),
            "the refusal did not name the allocation bound: {text}"
        );
    }

    /// "Sample X is still loading" is reported once per score, not once per
    /// retry window, and again for the next score, including one that a
    /// JavaScript evaluate installs.
    #[cfg(feature = "device-audio")]
    #[test]
    fn a_still_loading_sample_is_news_once_per_evaluated_score() {
        let (mut session, mut producer, _library) = loading_prefill_fixture();
        let _ = session.take_diagnostics();
        let still_loading = |session: &mut Session| {
            session
                .take_diagnostics()
                .iter()
                .filter(|diagnostic| diagnostic.kind == SAMPLE_LOADING_DIAGNOSTIC)
                .count()
        };
        let step = |session: &mut Session, producer: &mut crate::LiveFileProducer, clock| {
            producer
                .step_unwatched_with_clock_and_cutover(
                    session,
                    || clock,
                    48_000,
                    |_, _, _| panic!("a loading score published"),
                    |_| panic!("a loading onset emitted audio"),
                )
                .expect("a loading window is a held turn")
        };

        for score in ["first", "second"] {
            let before = session.generation();
            session
                .evaluate(r#"s("heldsample*4")"#)
                .expect("the JavaScript door");
            assert_eq!(session.last_path, EvaluateSource::JavaScript);
            session.restart_transport_at(0.0);
            producer.arm_replacement(before, session.generation());
            step(&mut session, &mut producer, 0.0);
            assert_eq!(still_loading(&mut session), 1, "{score} score: said once");
            step(&mut session, &mut producer, 0.5);
            assert_eq!(
                still_loading(&mut session),
                0,
                "{score} score: the next window says nothing new"
            );
        }
    }

    #[cfg(feature = "device-audio")]
    #[test]
    fn loading_prefill_does_not_publish_an_empty_retry() {
        use crate::samples::SoundReadiness;

        // An unrelated pending sample must not prevent a genuine silent save.
        for source in ["~", "heldsample*4"] {
            let (mut session, mut producer, library) = loading_prefill_fixture();

            let before = session.generation();
            let values = rustel_mini::mini(source).expect("native mini values");
            let pattern = rustel_core::controls::ControlSpec::new(["s"]).pattern(&values);
            session
                .set_pattern(pattern)
                .expect("native sound replacement");
            session.restart_transport_at(0.0);
            let after = session.generation();
            producer.arm_replacement(before, after);
            assert_eq!(after, before + 1);
            assert!(!session.active_needs_host(), "fixture must remain native");
            assert_eq!(
                library.readiness("heldsample", 0.0),
                SoundReadiness::Loading
            );
            assert!(library.take_ready().is_empty());
            assert_eq!(session.scheduler.queued(), 0);
            let cursor_before = session.scheduled_to_cycle();
            let mut published = Vec::new();
            let first = producer.step_unwatched_with_clock_and_cutover(
                &mut session,
                || 0.0,
                48_000,
                |generation, frame, _cut| published.push((generation, frame)),
                |_| panic!("rest or still-loading sample emitted audio"),
            );
            // A loading hold is a quiet turn, not a failed one: the held
            // replacement reports Ok with nothing scheduled and nothing
            // published, the same shape as the silent replacement's turn.
            let first = first.expect("held or silent replacement must not fail");
            assert_eq!((first.scheduled, first.pushed, first.pending), (0, 0, 0));
            let first_record = producer.producer_load_snapshot();
            assert!(first_record.query_span_millicycles > 0);
            assert_eq!(first_record.js_callback_calls, 0);
            assert_eq!(first_record.converted_audio_events, 0);
            assert_eq!(first_record.ring_pushes, 0);
            assert_eq!(producer.pending(), 0);
            assert_eq!(session.scheduler.queued(), 0);
            assert!(session.scheduled_to_cycle() > cursor_before);
            assert!(
                session.scheduler.horizon_remaining(0.0)
                    > session.config.horizon - session.scheduler.refill_floor_seconds()
            );

            if source == "~" {
                assert_eq!(first_record.scheduler_events, 0);
                assert_eq!(first_record.refused_voices, 0);
                assert_eq!(published, vec![(after, 0)]);
                continue;
            }

            assert_eq!(first_record.scheduler_events, 2);
            assert_eq!(first_record.refused_voices, 2);
            assert!(
                published.is_empty(),
                "failed conversion published a generation"
            );
            assert_eq!(session.generation(), after, "loading must not roll back");
            assert!(session.audible_source.is_none());

            // No clock advance, loader publication, requery or new score occurs.
            // The scheduler's full horizon must not turn lost onsets into silence.
            let retry = producer.step_unwatched_with_clock_and_cutover(
                &mut session,
                || 0.0,
                48_000,
                |generation, frame, _cut| published.push((generation, frame)),
                |_| panic!("the sample is still loading on retry"),
            );
            assert_eq!(
                library.readiness("heldsample", 0.0),
                SoundReadiness::Loading
            );
            assert!(library.take_ready().is_empty());
            assert_eq!(session.generation(), after);
            let retry = retry.expect("a loading retry is a held turn, not an error");
            assert_eq!((retry.scheduled, retry.pushed, retry.pending), (0, 0, 0));
            assert!(
                published.is_empty(),
                "still-loading full-horizon retry published {published:?}"
            );
        }
    }

    #[cfg(feature = "device-audio")]
    #[test]
    fn loading_prefill_accepts_only_later_query_progress() {
        use crate::samples::SoundReadiness;

        for (source, initial) in [
            ("heldsample*4", false),
            ("heldsample ~", false),
            ("heldsample sine", false),
            ("heldsample ~", true),
        ] {
            let (mut session, mut producer, library) = loading_prefill_fixture();
            let before = session.generation();
            let values = rustel_mini::mini(source).expect("native mini values");
            let pattern = rustel_core::controls::ControlSpec::new(["s"]).pattern(&values);
            session
                .set_pattern(pattern)
                .expect("native sound replacement");
            session.restart_transport_at(0.0);
            let after = session.generation();
            let expected_publications = if initial {
                producer = crate::LiveFileProducer::unwatched(Duration::from_millis(2))
                    .expect("fresh producer");
                Vec::new()
            } else {
                producer.arm_replacement(before, after);
                vec![(after, 0)]
            };
            let mut published = Vec::new();
            let first = producer.step_unwatched_with_clock_and_cutover(
                &mut session,
                || 0.0,
                48_000,
                |generation, frame, _cut| published.push((generation, frame)),
                |_| panic!("loading onset emitted audio"),
            );
            let first = first.expect("loading first window holds quietly");
            assert_eq!((first.scheduled, first.pushed, first.pending), (0, 0, 0));
            assert!(published.is_empty());
            assert!(
                !producer.cut_takeover_deferred_for_loading(),
                "an edit's loading hold has no line cut to withdraw"
            );
            let waiting = producer
                .step_unwatched_with_clock_and_cutover(
                    &mut session,
                    || 0.0,
                    48_000,
                    |generation, frame, _cut| published.push((generation, frame)),
                    |_| panic!("unchanged loading horizon emitted audio"),
                )
                .expect("empty retry waits");
            assert_eq!(
                (waiting.scheduled, waiting.pushed, waiting.pending),
                (0, 0, 0)
            );
            assert!(published.is_empty());
            assert!(!producer.cut_takeover_deferred_for_loading());
            assert!(session.audible_source.is_none());
            let _ = producer.producer_load_snapshot();

            let mut audio = Vec::new();
            let next = producer.step_unwatched_with_clock_and_cutover(
                &mut session,
                || 0.5,
                48_000,
                |generation, frame, _cut| published.push((generation, frame)),
                |event| {
                    audio.push(event);
                    true
                },
            );
            let record = producer.producer_load_snapshot();
            assert_eq!(record.query_span_millicycles, 500);
            assert_eq!(record.js_callback_calls, 0);
            if source != "heldsample*4" {
                let step = next.expect("fresh rest or oscillator window");
                let expected = usize::from(source == "heldsample sine");
                assert_eq!(
                    (step.scheduled, step.pushed, step.pending),
                    (expected, expected, 0)
                );
                assert_eq!(audio.len(), expected);
                assert!(audio.iter().all(|event| event.target_frame == 24_000));
                assert_eq!(published, expected_publications);
                assert!(session.audible_source.is_none());
                assert_eq!(
                    library.readiness("heldsample", 0.0),
                    SoundReadiness::Loading
                );
                continue;
            }

            let held = next.expect("held sample keeps waiting quietly");
            assert_eq!((held.scheduled, held.pushed, held.pending), (0, 0, 0));
            assert!(!producer.cut_takeover_deferred_for_loading());
            assert_eq!((record.scheduler_events, record.refused_voices), (2, 2));
            assert!(audio.is_empty());
            assert!(published.is_empty());
            let sample = library.finish_loading_sample_for_test();
            assert_eq!(library.readiness("heldsample", 0.0), SoundReadiness::Ready);
            assert_eq!(library.take_ready().len(), 1);
            let waiting = producer
                .step_unwatched_with_clock_and_cutover(
                    &mut session,
                    || 0.5,
                    48_000,
                    |generation, frame, _cut| published.push((generation, frame)),
                    |_| panic!("Ready must not replay the drained onsets"),
                )
                .expect("unchanged full horizon waits");
            assert_eq!(
                (waiting.scheduled, waiting.pushed, waiting.pending),
                (0, 0, 0)
            );
            assert!(published.is_empty());
            let _ = producer.producer_load_snapshot();
            let ready = producer
                .step_unwatched_with_clock_and_cutover(
                    &mut session,
                    || 1.0,
                    48_000,
                    |generation, frame, _cut| published.push((generation, frame)),
                    |event| {
                        audio.push(event);
                        true
                    },
                )
                .expect("new sample onsets after readiness");
            assert_eq!((ready.scheduled, ready.pushed, ready.pending), (2, 2, 0));
            assert_eq!(
                audio
                    .iter()
                    .map(|event| event.target_frame)
                    .collect::<Vec<_>>(),
                [48_000, 60_000]
            );
            assert!(
                audio
                    .iter()
                    .all(|event| event.sample.unwrap().sample == sample)
            );
            assert_eq!(published, expected_publications);
            assert!(session.audible_source.is_none());
        }
    }

    /// A REWIND whose first window names a still-loading sample holds the
    /// publication instead of certifying a partial first window.
    ///
    /// A restart is transport-start shaped: every track the score names
    /// should land on its first beat. A window published with
    /// skipped-loading onsets drops them for good (the scheduler cursor has
    /// moved past them), and their tracks enter late, alone. So the cut
    /// takeover's first prefill waits for load progress the way an empty
    /// loading window does; once the sample decodes, the whole window is
    /// scheduled, published and pushed in order. An ordinary edit keeps the
    /// skip-and-log contract and is not affected.
    #[test]
    #[cfg(feature = "device-audio")]
    fn a_rewind_first_window_holds_publication_while_a_sample_is_loading() {
        use crate::samples::SoundReadiness;

        let (mut session, mut producer, library) = loading_prefill_fixture();
        // The audible score the rewind replaces: silent (renderable), so
        // the contract under test is the rewind's, not the old voice's.
        let before = session.generation();
        let old = rustel_core::controls::ControlSpec::new(["s"])
            .pattern(&rustel_mini::mini("~").expect("silent old score"));
        session.set_pattern(old).expect("old silent score");
        session.restart_transport_at(0.0);
        let mid = session.generation();
        producer.arm_replacement(before, mid);
        let mut published: Vec<(u64, u64, TakeoverCut)> = Vec::new();
        producer
            .step_unwatched_with_clock_and_cutover(
                &mut session,
                || 0.0,
                48_000,
                |generation, frame, cut| published.push((generation, frame, cut)),
                |_| panic!("silence emitted audio"),
            )
            .expect("silent old score publishes");
        assert_eq!(published, vec![(mid, 0, TakeoverCut::None)]);
        // Inject the replay-policy target the ordinary rollback path needs
        // if the held window ever refused for real: this test exercises the
        // hold-and-ready route, not the rollback.
        session
            .mark_audible_generation(mid)
            .expect("injected replay-policy target");

        // Install the loading replacement as the TUI path does: set the
        // takeover override and the from-zero flag, then call `reload_at`,
        // which arms the (takeover, cut) pair. A raw `set_pattern` skips that.
        let rewinding = session.generation();
        session.set_next_takeover_time(0.25);
        session.start_next_from_zero();
        session
            .reload_at("s(\"heldsample\")", false, 0.0)
            .expect("native sound replacement");
        let after = session.generation();
        assert_eq!(after, rewinding + 1);
        producer.arm_replacement(rewinding, after);

        // First window: the sample is still loading, but the window is not
        // empty. The query ran and some onsets were refused as loading.
        assert_eq!(
            library.readiness("heldsample", 0.0),
            SoundReadiness::Loading
        );
        let first = producer
            .step_unwatched_with_clock_and_cutover(
                &mut session,
                || 0.0,
                48_000,
                |generation, frame, cut| published.push((generation, frame, cut)),
                |_| panic!("loading onset emitted audio"),
            )
            .expect("a loading first window is a held turn, not a failure");
        assert_eq!((first.scheduled, first.pushed), (0, 0));
        assert!(
            !published
                .iter()
                .any(|(generation, _, _)| *generation == after),
            "the partial loading window must not publish the rewind: {published:?}"
        );
        assert!(
            producer.cut_takeover_deferred_for_loading(),
            "the engine is told to withdraw the line cut while the flip waits"
        );

        // The sample decodes; the whole first window lands at once.
        let sample = library.finish_loading_sample_for_test();
        assert_eq!(library.readiness("heldsample", 0.0), SoundReadiness::Ready);
        let mut audio = Vec::new();
        let ready = producer.step_unwatched_with_clock_and_cutover(
            &mut session,
            || 0.0,
            48_000,
            |generation, frame, cut| published.push((generation, frame, cut)),
            |event| {
                audio.push(event);
                true
            },
        );
        let step = ready.expect("the whole rewind window publishes once ready");
        assert_eq!(
            published
                .iter()
                .filter(|(generation, _, _)| *generation == after)
                .collect::<Vec<_>>(),
            [&(after, 12_000, TakeoverCut::AtTakeover)],
            "exactly one publication, on the line, with the line's cut: {published:?}"
        );
        assert!(step.scheduled > 0 && step.pushed == step.scheduled);
        assert_eq!(
            audio.iter().map(|event| event.target_frame).min(),
            Some(12_000),
            "cycle zero's onset comes back, on the line"
        );
        assert!(
            audio.iter().all(|event| event
                .sample
                .is_some_and(|controls| controls.sample == sample)),
            "every first-beat onset carries the sample"
        );
        assert!(
            !producer.cut_takeover_deferred_for_loading(),
            "publication lowers the deferral: the next launch's line cut must stand"
        );
        assert_eq!(
            session.take_requery_takeover(),
            None,
            "the publication consumed the pair; nothing is left for the next reload"
        );
    }

    /// A rewind's held window that has nothing to reopen keeps its takeover
    /// and cut. Here the scheduler has not queried up to cycle zero, so the
    /// cursor stands short of the anchor.
    #[test]
    #[cfg(feature = "device-audio")]
    fn a_held_rewind_that_cannot_reopen_keeps_its_takeover_and_cut() {
        let (mut session, mut producer, _library) = rewind_hold_fixture();
        let rewinding = session.generation();
        session.set_next_takeover_time(0.25);
        session.start_next_from_zero();
        session
            .reload_at("s(\"heldsample\")", false, 0.0)
            .expect("native sound replacement");
        let after = session.generation();
        producer.arm_replacement(rewinding, after);
        // Queue the cycle-zero onset ahead of the producer, then name an
        // anchor past what that fill queried.
        session.scheduler.set_horizon(1.0);
        assert_eq!(
            session.scheduler.tick(&VirtualClock::new(0.0)),
            rustel_scheduler::TickStatus::Filled
        );
        session.scheduler.set_horizon(0.5);
        assert_eq!(session.scheduler.queued(), 1);
        session.requery_anchor_time = Some(5.0);

        let held = producer
            .step_unwatched_with_clock_and_cutover(
                &mut session,
                || 0.0,
                48_000,
                |_, _, _| panic!("a held rewind published"),
                |_| panic!("a loading onset emitted audio"),
            )
            .expect("the loading window holds quietly");
        assert_eq!((held.scheduled, held.pushed), (0, 0));
        assert!(producer.cut_takeover_deferred_for_loading());
        assert_eq!(
            session.take_requery_takeover(),
            Some((0.25, TakeoverCut::AtTakeover)),
            "the pair is back for this rewind's publication"
        );
    }

    /// The gap skip must not move an unpublished rewind's cursor off cycle
    /// zero. The producer slides a stale cycle zero to its clock before it
    /// queries, so it never presents such a gap; a direct schedule of the
    /// same state, long after the line, must still query the restart from
    /// its first beat rather than resume "at the present" bars in.
    #[test]
    #[cfg(feature = "device-audio")]
    fn the_gap_skip_leaves_an_unpublished_rewind_on_cycle_zero() {
        let (mut session, _producer, _library) = rewind_hold_fixture();
        session.set_next_takeover_time(0.25);
        session.start_next_from_zero();
        session
            .reload_at("s(\"sine\")", false, 0.0)
            .expect("native synth replacement");
        let batch = match session.schedule_audio_live_at(
            2.0,
            48_000,
            Duration::from_millis(2),
            LiveQueryBudgetMode::ReplacementPrefill,
        ) {
            Ok(batch) => batch,
            Err(_) => panic!("the synth window schedules"),
        };
        assert_eq!(
            batch.events.first().map(|event| event.target_frame),
            Some(12_000),
            "the restart is queried from its first beat"
        );
        assert_eq!(session.take_pending_producer_turn().gap_resync_count, 0);
        assert!(
            session
                .take_diagnostics()
                .iter()
                .all(|diagnostic| diagnostic.kind != "live-recovered"),
            "an unpublished restart is not a producer that fell behind"
        );
    }

    /// A rewind's first window where one track's sample is still loading and
    /// another's sound is ready lands whole once the sample decodes. The hold
    /// reopens the window, so the ready onset is not lost.
    #[test]
    #[cfg(feature = "device-audio")]
    fn a_partially_loading_rewind_window_lands_whole() {
        let (mut session, mut producer, library) = rewind_hold_fixture();
        let rewinding = session.generation();
        session.set_next_takeover_time(0.25);
        session.start_next_from_zero();
        session
            .reload_at("s(\"[heldsample, sine]\")", false, 0.0)
            .expect("native sound replacement");
        let after = session.generation();
        producer.arm_replacement(rewinding, after);

        let mut published: Vec<(u64, u64, TakeoverCut)> = Vec::new();
        for turn in 0..3 {
            let held = producer
                .step_unwatched_with_clock_and_cutover(
                    &mut session,
                    || 0.0,
                    48_000,
                    |generation, frame, cut| published.push((generation, frame, cut)),
                    |_| panic!("a held rewind window emitted audio"),
                )
                .expect("the partial window holds quietly");
            assert_eq!((held.scheduled, held.pushed), (0, 0), "turn {turn}");
            assert!(
                published
                    .iter()
                    .all(|(generation, _, _)| *generation != after),
                "turn {turn}: the rewind must not publish while a track loads: {published:?}"
            );
            assert!(producer.cut_takeover_deferred_for_loading());
        }

        library.finish_loading_sample_for_test();
        let mut audio = Vec::new();
        producer
            .step_unwatched_with_clock_and_cutover(
                &mut session,
                || 0.0,
                48_000,
                |generation, frame, cut| published.push((generation, frame, cut)),
                |event| {
                    audio.push(event);
                    true
                },
            )
            .expect("the whole first window publishes once ready");
        let rewind: Vec<_> = published
            .iter()
            .filter(|(generation, _, _)| *generation == after)
            .collect();
        assert_eq!(
            rewind,
            [&(after, 12_000, TakeoverCut::AtTakeover)],
            "one publication, on the line, with the line's cut"
        );
        let downbeat: Vec<bool> = audio
            .iter()
            .filter(|event| event.target_frame == 12_000)
            .map(|event| event.sample.is_some())
            .collect();
        assert!(
            downbeat.contains(&true) && downbeat.contains(&false),
            "both the sample track and the ready synth sound on cycle zero: {downbeat:?}"
        );
    }

    /// A rewind held for loading past its own takeover and past the schedule
    /// cover restarts from cycle zero when it can sound.
    #[test]
    #[cfg(feature = "device-audio")]
    fn a_rewind_held_past_its_takeover_restarts_where_it_can_sound() {
        let (mut session, mut producer, library) = rewind_hold_fixture();
        let rewinding = session.generation();
        session.set_next_takeover_time(0.25);
        session.start_next_from_zero();
        session
            .reload_at("s(\"heldsample\")", false, 0.0)
            .expect("native sound replacement");
        let after = session.generation();
        producer.arm_replacement(rewinding, after);
        let _ = producer.producer_load_snapshot();

        let mut published: Vec<(u64, u64, TakeoverCut)> = Vec::new();
        // Mid-countdown, then past the line, then past line + cover.
        for clock in [0.0, 0.8, 1.5] {
            let held = producer
                .step_unwatched_with_clock_and_cutover(
                    &mut session,
                    || clock,
                    48_000,
                    |generation, frame, cut| published.push((generation, frame, cut)),
                    |_| panic!("a held rewind emitted audio"),
                )
                .expect("the rewind holds quietly");
            assert_eq!((held.scheduled, held.pushed), (0, 0), "at {clock}");
            assert!(
                published
                    .iter()
                    .all(|(generation, _, _)| *generation != after),
                "at {clock}: no publication while the sample loads: {published:?}"
            );
        }
        assert_eq!(
            producer.producer_load_snapshot().gap_resync_count,
            0,
            "a held rewind is not a producer that fell behind"
        );

        library.finish_loading_sample_for_test();
        let mut audio = Vec::new();
        producer
            .step_unwatched_with_clock_and_cutover(
                &mut session,
                || 1.6,
                48_000,
                |generation, frame, cut| published.push((generation, frame, cut)),
                |event| {
                    audio.push(event);
                    true
                },
            )
            .expect("the rewind publishes once ready");
        let rewind: Vec<_> = published
            .iter()
            .filter(|(generation, _, _)| *generation == after)
            .collect();
        assert_eq!(
            rewind,
            [&(after, 76_800, TakeoverCut::AtFlip)],
            "cycle zero lands where the rewind can sound, and the old score is cut there"
        );
        let first = audio.iter().map(|event| event.target_frame).min();
        assert_eq!(
            first,
            Some(76_800),
            "the first onset is cycle zero, not a catch-up"
        );
        assert!(
            audio.iter().all(|event| event.target_frame >= 76_800),
            "no past-due burst of the onsets the decode missed"
        );
    }

    /// A rewind held for a loading sample parks instead of re-querying its
    /// first window on every 2 ms engine turn. Each retry reopened and
    /// re-converted the whole window, for as long as the decode took: a core
    /// busy re-refusing the same onsets beside the decoder it waited on.
    /// Parked, it retries once per safety interval of the producer's clock,
    /// and at once when the library settles the sample.
    #[test]
    #[cfg(feature = "device-audio")]
    fn a_rewind_held_for_loading_waits_for_the_decode_instead_of_requerying() {
        let (mut session, mut producer, library) = rewind_hold_fixture();
        let rewinding = session.generation();
        session.start_next_from_zero();
        session
            .reload_at("s(\"heldsample*4\")", false, 0.0)
            .expect("native sound replacement");
        let after = session.generation();
        producer.arm_replacement(rewinding, after);
        let refusals_before = producer.producer_load_snapshot().atomic_refusals;

        let mut published: Vec<(u64, u64, TakeoverCut)> = Vec::new();
        let mut clock = 0.0;
        for turn in 0..100 {
            clock = f64::from(turn) * 0.002;
            let held = producer
                .step_unwatched_with_clock_and_cutover(
                    &mut session,
                    || clock,
                    48_000,
                    |generation, frame, cut| published.push((generation, frame, cut)),
                    |_| panic!("a held rewind emitted audio"),
                )
                .expect("the rewind holds quietly");
            assert_eq!((held.scheduled, held.pushed), (0, 0), "turn {turn}");
            assert!(producer.cut_takeover_deferred_for_loading(), "turn {turn}");
        }
        assert!(published.is_empty(), "{published:?}");
        // One query at the start, then one per 50 ms of producer clock over
        // the ~200 ms the hundred turns span.
        let queries = producer.producer_load_snapshot().atomic_refusals - refusals_before;
        assert!(
            (2..=5).contains(&queries),
            "a held rewind queried {queries} times in 100 turns"
        );

        // The decode lands on the very next turn, well inside the interval:
        // the settled epoch wakes the hold.
        library.finish_loading_sample_for_test();
        let mut audio = Vec::new();
        producer
            .step_unwatched_with_clock_and_cutover(
                &mut session,
                || clock,
                48_000,
                |generation, frame, cut| published.push((generation, frame, cut)),
                |event| {
                    audio.push(event);
                    true
                },
            )
            .expect("the rewind publishes once its sample is ready");
        let frame = (clock * 48_000.0).round() as u64;
        assert_eq!(published, [(after, frame, TakeoverCut::AtFlip)]);
        assert_eq!(
            audio.iter().map(|event| event.target_frame).min(),
            Some(frame),
            "the restart begins on its first beat"
        );
    }

    /// The park's safety retry must not wait on the producer's clock alone.
    /// In the studio that clock is the device's count of completed frames,
    /// and a stalled, paused or recycling device stops it: a rewind parked
    /// there never retried at all, and only a settle the epoch could see
    /// would wake it. Turns count too.
    #[test]
    #[cfg(feature = "device-audio")]
    fn a_parked_rewind_retries_while_the_device_clock_stands_still() {
        let (mut session, mut producer, _library) = parked_rewind_fixture();
        let refusals_before = producer.producer_load_snapshot().atomic_refusals;
        for turn in 0..100 {
            let held = producer
                .step_unwatched_with_clock_and_cutover(
                    &mut session,
                    || 0.0,
                    48_000,
                    |_, _, _| panic!("a held rewind published"),
                    |_| panic!("a held rewind emitted audio"),
                )
                .expect("the rewind holds quietly");
            assert_eq!((held.scheduled, held.pushed), (0, 0), "turn {turn}");
        }
        let queries = producer.producer_load_snapshot().atomic_refusals - refusals_before;
        assert!(
            (3..=5).contains(&queries),
            "a hundred turns on a clock that never moves queried {queries} times"
        );
    }

    /// Every sample library's settled epoch starts at zero, so a library
    /// swapped in under a parked rewind can carry the very epoch the park
    /// waits on. The park belongs to the library it read, and a new one
    /// wakes it on the next turn.
    #[test]
    #[cfg(feature = "device-audio")]
    fn a_parked_rewind_wakes_when_the_sample_library_changes() {
        let (mut session, mut producer, library) = parked_rewind_fixture();
        let replacement = Arc::new(crate::samples::SampleLibrary::with_loading_sample_for_test(
            "heldsample",
        ));
        assert_eq!(
            replacement.settled_epoch(),
            library.settled_epoch(),
            "the new library carries the parked epoch"
        );
        session.set_sample_library_for_test(Arc::clone(&replacement));
        let refusals_before = producer.producer_load_snapshot().atomic_refusals;
        producer
            .step_unwatched_with_clock_and_cutover(
                &mut session,
                || 0.002,
                48_000,
                |_, _, _| panic!("the new library's sample is loading too"),
                |_| panic!("a held rewind emitted audio"),
            )
            .expect("the rewind holds quietly");
        assert_eq!(
            producer.producer_load_snapshot().atomic_refusals - refusals_before,
            1,
            "the turn after the swap asks the new library"
        );
    }

    /// A stop that lands during a parked turn reaches the producer on that
    /// turn, as it does on the query path, rather than waiting out the park.
    #[test]
    #[cfg(feature = "device-audio")]
    fn a_parked_rewind_lets_a_stop_through() {
        let (mut session, mut producer, _library) = parked_rewind_fixture();
        let transport = session.transport();
        let result = producer.step_unwatched_with_clock_and_cutover(
            &mut session,
            || {
                // The stop lands after the turn's own check at its start,
                // while it reads the clock.
                transport.stop();
                0.002
            },
            48_000,
            |_, _, _| panic!("a stopped rewind published"),
            |_| panic!("a stopped rewind emitted audio"),
        );
        assert!(
            matches!(result, Err(RuntimeError::Cancelled)),
            "the stop is answered on this turn: {:?}",
            result.map(|step| step.watch)
        );
    }

    /// An immediate rewind's cycle zero is anchored at the install clock,
    /// and the producer always publishes it later. Left there, the first
    /// window was aimed into rendered frames (attack lost). The producer
    /// slides cycle zero to the instant it publishes.
    #[test]
    #[cfg(feature = "device-audio")]
    fn an_immediate_rewind_restarts_where_the_producer_publishes_it() {
        let (mut session, mut producer, _library) = rewind_hold_fixture();
        let rewinding = session.generation();
        session.start_next_from_zero();
        session
            .reload_at("s(\"sine\")", false, 0.0)
            .expect("native sound replacement");
        let after = session.generation();
        producer.arm_replacement(rewinding, after);

        let mut published: Vec<(u64, u64, TakeoverCut)> = Vec::new();
        let mut audio = Vec::new();
        producer
            .step_unwatched_with_clock_and_cutover(
                &mut session,
                || 0.02,
                48_000,
                |generation, frame, cut| published.push((generation, frame, cut)),
                |event| {
                    audio.push(event);
                    true
                },
            )
            .expect("the rewind publishes");
        assert_eq!(published, [(after, 960, TakeoverCut::AtFlip)]);
        assert_eq!(
            audio.iter().map(|event| event.target_frame).min(),
            Some(960),
            "cycle zero is where the producer published, not 20 ms behind it"
        );
    }

    /// A rewind whose first window refuses an unknown sound while the look-
    /// ahead finds a sample still loading is held, reopened and asked again
    /// on every retry. Its refusals are said once per install, and the
    /// sample it waits for is said once, as awaited rather than skipped:
    /// before, the unknown sound was logged on every retry and nothing named
    /// the load.
    #[test]
    #[cfg(feature = "device-audio")]
    fn a_held_rewind_says_its_refusals_and_its_wait_once() {
        let (mut session, mut producer, _library) = rewind_hold_fixture();
        let rewinding = session.generation();
        session.set_next_takeover_time(0.25);
        session.start_next_from_zero();
        session
            .reload_at("s(\"<unknown-test-sample heldsample>\")", false, 0.0)
            .expect("native sound replacement");
        let after = session.generation();
        producer.arm_replacement(rewinding, after);
        let _ = session.take_diagnostics();

        let mut said = Vec::new();
        // Each retry is past the parked hold's interval, and all are before
        // the line, so every turn re-asks the reopened first window.
        for clock in [0.0, 0.06, 0.12, 0.18] {
            let held = producer
                .step_unwatched_with_clock_and_cutover(
                    &mut session,
                    || clock,
                    48_000,
                    |_, _, _| panic!("a held rewind published"),
                    |_| panic!("a held rewind emitted audio"),
                )
                .expect("the rewind holds quietly");
            assert_eq!((held.scheduled, held.pushed), (0, 0), "at {clock}");
            assert!(
                producer.cut_takeover_deferred_for_loading(),
                "at {clock}: the rewind is held for the sample ahead"
            );
            said.extend(
                session
                    .take_diagnostics()
                    .into_iter()
                    .filter(|diagnostic| {
                        diagnostic.kind == "voice-refused"
                            || diagnostic.kind == SAMPLE_LOADING_DIAGNOSTIC
                            || diagnostic.kind == SAMPLE_AWAITED_DIAGNOSTIC
                    })
                    .map(|diagnostic| (diagnostic.kind, diagnostic.message)),
            );
        }
        // Told apart by kind, not by reading the sentence back: a sound this
        // engine cannot play and a sound that has not arrived yet are
        // different news, and only one of them is the score's fault.
        let unknown = said
            .iter()
            .filter(|(kind, message)| {
                kind == "voice-refused" && message.contains("unknown-test-sample")
            })
            .count();
        let loading = said
            .iter()
            .filter(|(kind, _)| kind == SAMPLE_AWAITED_DIAGNOSTIC)
            .count();
        assert_eq!(
            unknown, 1,
            "the unknown sound is said once, not per retry: {said:?}"
        );
        assert_eq!(loading, 1, "the sample it waits for is said once: {said:?}");
        assert!(
            !said
                .iter()
                .any(|(kind, _)| kind == SAMPLE_LOADING_DIAGNOSTIC),
            "nothing is said skipped: {said:?}"
        );
    }

    #[cfg(feature = "device-audio")]
    #[test]
    fn loading_prefill_preserves_queued_audio_without_a_query() {
        check_loading_prefill_queued_progress(false);
    }

    #[cfg(all(feature = "device-audio", feature = "midi"))]
    #[test]
    fn loading_prefill_preserves_queued_midi_without_a_query() {
        check_loading_prefill_queued_progress(true);
    }

    #[test]
    fn pure_scheduling_installs_no_host() {
        // The complement: a graph reaching no JavaScript must schedule with
        // nothing installed, exercising the purity invariant on
        // this path.
        let mut session = Session::new().expect("session");
        session.evaluate(r#"s("bd sd").fast(2)"#).expect("evaluate");
        assert!(!session.active_needs_host());
        let report = session.play(2.0).expect("play");
        assert!(!report.onsets.is_empty());
    }

    #[test]
    fn pure_scheduling_uses_the_owning_sessions_module_settings() {
        let mut first = Session::new().expect("first session");
        let mut second = Session::new().expect("second session");
        first
            .evaluate("setDefaultVoicings('guidetones'); chord('C7').voicing()")
            .expect("first score");
        second
            .evaluate("setDefaultVoicings('lefthand'); chord('C7').voicing()")
            .expect("second score");

        assert!(!first.active_needs_host());
        assert!(!second.active_needs_host());
        assert_eq!(
            first
                .play(0.25)
                .expect("schedule first session")
                .onsets
                .len(),
            2
        );
        assert_eq!(
            second
                .play(0.25)
                .expect("schedule second session")
                .onsets
                .len(),
            4
        );
    }
}
