//! File-watch producer for the continuous audio ring.
//!
//! The CLI owns device discovery and sleeping; this component owns the
//! ordering required by the live runtime: poll a stable
//! file, install its generation on the consumer before any new events cross,
//! discard producer-side backlog from the old generation, transfer one
//! scheduler horizon, and retain back-pressure without growing another batch.

use std::collections::VecDeque;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use crate::session::confirmation::WindowReservation;
use rustel_audio::QueuedAudioEvent;
use rustel_audio::SampleId;
use rustel_audio::TakeoverCut;
use rustel_audio::confirmation::{ConfirmationOnset, WindowOffer};

use crate::producer::{
    ProducerLoadMeter, ProducerLoadSnapshot, ProducerPhase, ProducerTurnOutcome, ProducerTurnRecord,
};
use crate::session::{
    LiveAudioScheduleError, LiveQueryBudgetMode, MIN_LIVE_QUERY_JS_BUDGET, RollbackAttempt,
};
use crate::{FileWatch, RuntimeError, Session, WatchLanguage, WatchPoll, WatchTarget};

/// A transient GC, page fault, or heavy score install must influence the next
/// setup attempt, but not disable setup for the lifetime of the process. One
/// nominal 0.5-second scheduler horizon at the 2 ms loop cadence retains recent
/// slow observations while eventually aging out one outlier.
const CONTINUATION_WINDOW_SAMPLES: usize = 256;

#[derive(Clone, Debug)]
struct ContinuationWindow {
    samples: [Duration; CONTINUATION_WINDOW_SAMPLES],
    next: usize,
    len: usize,
}

impl Default for ContinuationWindow {
    fn default() -> Self {
        Self {
            samples: [Duration::ZERO; CONTINUATION_WINDOW_SAMPLES],
            next: 0,
            len: 0,
        }
    }
}

impl ContinuationWindow {
    fn record(&mut self, sample: Duration) {
        self.samples[self.next] = sample;
        self.next = (self.next + 1) % CONTINUATION_WINDOW_SAMPLES;
        self.len = (self.len + 1).min(CONTINUATION_WINDOW_SAMPLES);
    }

    fn high_water(&self) -> Option<Duration> {
        self.samples[..self.len].iter().copied().max()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LiveProducerStep {
    /// Score-file poll. Only an Installed event here may cut generations.
    pub watch: WatchPoll,
    /// Prebake-file poll. Successful setup mutates the heap but leaves the
    /// active score/generation alone.
    pub prebake_watch: WatchPoll,
    pub scheduled: usize,
    pub pushed: usize,
    pub pending: usize,
    pub backpressured: bool,
}

/// What a held turn did before it stopped: see [`LiveFileProducer::held_step`].
struct HeldTurn {
    pushed: usize,
    backpressured: bool,
    cover_age_started: Option<Instant>,
    /// The hold waits for samples to load, which is not starvation.
    loading: bool,
}

struct WatchResults {
    score: WatchPoll,
    prebake: WatchPoll,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PrebakeState {
    Ready,
    AwaitingSuccessfulIdentity,
    RetryingScore,
    ScoreRetryRejected,
}

/// One producer for one continuously running Session and one audio ring.
/// Consecutive empty scheduling steps that mean "this score cannot be
/// played". Generous enough that an ordinary rest or sparse bar never trips
/// it (the producer steps every few ms), tight enough that the ring still
/// holds cover when the rollback lands.
#[cfg(feature = "device-audio")]
/// How long every scheduling attempt must keep FAILING before the producer
/// gives up on the installed score and reinstalls the previous one.
///
/// Counting producer steps instead of wall-clock time was wrong twice over:
/// steps run ~290/s, so 64 of them is 0.2 s of nothing-yet, and an empty batch
/// is the ordinary steady state (the horizon is already covered, so most steps
/// schedule nothing). The first version rolled back healthy sets several times
/// a minute and undid valid edits mid-phrase.
const STARVED_BEFORE_ROLLBACK: Duration = Duration::from_secs(3);

/// How long, on the producer's clock, a rewind held for a loading sample
/// stays parked when nothing in the sample library has settled. The epoch
/// wakes it the moment a decode lands; this only bounds a wake the epoch
/// cannot see (a sound whose answer changed some other way).
const HELD_REWIND_RETRY_SECS: f64 = 0.05;

/// How many turns a parked rewind returns quietly before it retries anyway,
/// whatever the producer's clock says. In the studio that clock is the
/// device's count of completed frames, and it stops when the device does -
/// stalled, paused, or mid-recycle - so a retry timed on it alone would
/// never come. At the engine's 2 ms cadence this is the same 50 ms.
const HELD_REWIND_RETRY_TURNS: u32 = 25;

/// A rewind parked on a loading hold: see [`LiveFileProducer::parked_hold`].
#[derive(Clone, Debug)]
struct ParkedHold {
    /// The replacement that held.
    generation: u64,
    /// The library's settled epoch read before the query that held.
    epoch: u64,
    /// The library that epoch belongs to. Every library's epoch starts at
    /// zero, so a library swapped in under the hold can carry the very
    /// number the hold waits on. A weak handle, not an address: it keeps the
    /// allocation from being reused while the hold remembers it.
    library: Option<std::sync::Weak<crate::samples::SampleLibrary>>,
    /// The producer clock of that turn.
    at: f64,
    /// Quiet turns since the query that held.
    turns: u32,
}

impl ParkedHold {
    fn new(generation: u64, epoch: u64, session: &Session, at: f64) -> Self {
        Self {
            generation,
            epoch,
            library: session.sample_library().map(std::sync::Arc::downgrade),
            at,
            turns: 0,
        }
    }

    /// Whether the library the hold waited on is still the session's.
    fn same_library(&self, session: &Session) -> bool {
        match (&self.library, session.sample_library()) {
            (None, None) => true,
            (Some(parked), Some(current)) => {
                std::ptr::eq(parked.as_ptr(), std::sync::Arc::as_ptr(current))
            }
            _ => false,
        }
    }
}

/// Events the producer may hold undelivered. Generous for ordinary dense
/// scores (the ring drains as audio plays) and small enough that a runaway
/// span cannot lock out later edits for minutes.
#[cfg(feature = "device-audio")]
const MAX_PENDING_BACKLOG: usize = 4096;

pub struct LiveFileProducer {
    score_watch: FileWatch,
    prebake_watch: Option<FileWatch>,
    watch_poll: Duration,
    next_watch_poll: Duration,
    evaluation_continuation_floor: Duration,
    /// Full scheduling/conversion/ring continuations reserve watched setup and
    /// score work. Query deadlines use the separate post-query tail below so a
    /// slow successful query cannot recursively price out every later query.
    continuation_window: ContinuationWindow,
    /// Whether the backlog-overflow message has already been printed for the
    /// current overflow episode.
    backlog_dropped_reported: bool,
    /// Consecutive scheduling steps that produced NO events while the ring
    /// was hungry. A score can pass installation validation and still be
    /// unschedulable (astronomically many haps per cycle); without this the
    /// producer spins, the ring drains, and the set is silent for good.
    starving_since: Option<Instant>,
    query_tail_window: ContinuationWindow,
    prebake_state: PrebakeState,
    pending: VecDeque<QueuedAudioEvent>,
    /// One past the latest target any scheduled event has carried. Monotone;
    /// the engine freezes it when the library forgets a sample, since
    /// nothing scheduled afterwards can name that sample.
    scheduled_through_frame: u64,
    generation_needs_prefill: bool,
    /// Device generation handoff is deliberately later than Session install:
    /// retain it across an atomic query refusal and publish it only after a
    /// replacement batch has scheduled and converted successfully.
    pending_generation: Option<u64>,
    /// `FileWatch` has already delivered this identity internally, but the
    /// product-facing Installed event is not complete until the consumer
    /// generation is published. Retain it across a retryable prefill refusal.
    pending_reload_event: Option<crate::ReloadEvent>,
    /// Whether the retained cutover should surface as a score reload. Internal
    /// control requeries need the same atomic handoff without pretending that
    /// the musician saved or evaluated new source.
    pending_reload_visible: bool,
    /// Watch identities are committed by `FileWatch` before scheduling runs.
    /// Preserve non-cutover score and setup events across a later scheduling
    /// error so the product-facing poll still observes each one exactly once.
    pending_score_report: Option<crate::ReloadEvent>,
    pending_prebake_report: Option<crate::ReloadEvent>,
    /// Queue-cap and scalar-conversion failures can happen after Scheduler has
    /// committed or drained onsets. An unchanged retry could otherwise appear
    /// to succeed with an empty batch and complete an initial/replacement
    /// prefill as silence.
    prefill_candidate_committed: bool,
    /// Loading drops the attempted onsets. Wait for fresh scheduling or
    /// accepted output before treating a later empty transfer as prefill.
    prefill_waiting_for_progress: bool,
    /// A refused replacement whose rollback could not run yet (the session
    /// deferred it for want of horizon): retried at the top of every turn
    /// until the audible score is back or a newer replacement supersedes it,
    /// never latched.
    rollback_pending: bool,
    /// The generation a rollback installed. If THAT refuses every onset too
    /// there is nothing left to go back to, and rolling back again would
    /// burn a generation per turn forever.
    rolled_back_to: Option<u64>,
    /// Desktop IPC evaluate arms a cutover that the unwatched step is allowed
    /// to publish. A watched cutover must still finish on the watched route.
    unwatched_cutover_allowed: bool,
    /// Query-time control changes reuse generation cutover without becoming a
    /// source replacement. A failed control prefill must remain correctable by
    /// the still-audible layout and must never roll back unrelated source.
    control_requery_pending: bool,
    /// Source replacements reject a consumed takeover. Controls, Loading
    /// retries and output recovery retain their separate continuation policy.
    check_takeover_freshness: bool,
    /// A cut takeover (a rewind) deferred its publication because the
    /// replacement's first window had still-loading onsets. The engine reads
    /// this to withdraw the line cut it pre-armed at fire time. With the
    /// flip held for loading, that cut would fade out the old score before
    /// the replacement can sound, and the set would be silent until the
    /// decode finished. Every arm/clear site keeps this in sync with the
    /// pre-armed cut it mirrors.
    cut_takeover_deferred_for_loading: bool,
    /// A rewind held for a loading sample does not re-query its first window
    /// every turn. Each retry reopens and re-converts the whole window (up
    /// to four cycles of onsets, a refusal string each) at the engine's 2 ms
    /// cadence, for as long as the decode takes - a core spent competing
    /// with the decoder it waits on. Until the library's settled epoch
    /// moves (or the library itself is replaced), a short interval or a
    /// handful of turns passes, or the transport stops, a held turn returns
    /// quietly instead.
    parked_hold: Option<ParkedHold>,
    /// The newest replacement generation the producer gave up on: refused
    /// and rolled back (or latched), so it will never publish. Taken by the
    /// engine, which cannot tell from the session alone - a rollback that
    /// puts back the same score text leaves nothing else to see.
    abandoned_generation: Option<u64>,
    recovery_epoch: u64,
    load_meter: ProducerLoadMeter,
    completed_turn: Option<ProducerTurnRecord>,
    turn_rollbacks: u64,
}

impl LiveFileProducer {
    fn live_query_reserve(&self) -> Duration {
        self.evaluation_continuation_floor
            .saturating_add(self.query_tail_window.high_water().unwrap_or_default())
            .max(MIN_LIVE_QUERY_JS_BUDGET)
    }

    pub fn new(
        path: impl Into<PathBuf>,
        language: WatchLanguage,
        debounce: Duration,
        watch_poll: Duration,
    ) -> Result<Self, RuntimeError> {
        Self::new_with_prebake_floor(path, language, debounce, watch_poll, watch_poll)
    }

    /// Construct a producer for source installed directly by an in-process
    /// host rather than observed through [`FileWatch`].
    ///
    /// The unwatched stepping route never reads `score_watch`; seeding it with
    /// an in-memory baseline avoids a synthetic temporary file and keeps the
    /// same continuation floor used by the producer's query budget. Source
    /// replacements must be paired with [`Self::arm_replacement`] before the
    /// next [`Self::step_unwatched_with_clock_and_cutover`] call.
    pub fn unwatched(continuation_floor: Duration) -> Result<Self, RuntimeError> {
        Self::from_loaded_sources_with_prebake_floor(
            PathBuf::from("<studio>"),
            WatchLanguage::JavaScript,
            String::new(),
            None,
            Duration::ZERO,
            continuation_floor,
            continuation_floor,
        )
    }

    pub fn new_with_prebake_floor(
        path: impl Into<PathBuf>,
        language: WatchLanguage,
        debounce: Duration,
        watch_poll: Duration,
        evaluation_continuation_floor: Duration,
    ) -> Result<Self, RuntimeError> {
        if watch_poll.is_zero() {
            return Err(RuntimeError::Message(
                "live file poll interval must be greater than zero".into(),
            ));
        }
        if evaluation_continuation_floor.is_zero() {
            return Err(RuntimeError::Message(
                "live evaluation continuation reserve must be greater than zero".into(),
            ));
        }
        Ok(Self {
            score_watch: FileWatch::new(path, language, debounce),
            prebake_watch: None,
            watch_poll,
            next_watch_poll: Duration::ZERO,
            evaluation_continuation_floor,
            continuation_window: ContinuationWindow::default(),
            starving_since: None,
            backlog_dropped_reported: false,
            query_tail_window: ContinuationWindow::default(),
            prebake_state: PrebakeState::Ready,
            pending: VecDeque::new(),
            scheduled_through_frame: 0,
            generation_needs_prefill: true,
            pending_generation: None,
            pending_reload_event: None,
            pending_reload_visible: false,
            pending_score_report: None,
            pending_prebake_report: None,
            prefill_candidate_committed: false,
            prefill_waiting_for_progress: false,
            rollback_pending: false,
            rolled_back_to: None,
            unwatched_cutover_allowed: false,
            control_requery_pending: false,
            check_takeover_freshness: false,
            cut_takeover_deferred_for_loading: false,
            parked_hold: None,
            abandoned_generation: None,
            recovery_epoch: 0,
            load_meter: ProducerLoadMeter::default(),
            completed_turn: None,
            turn_rollbacks: 0,
        })
    }

    /// Product constructor using the exact score/setup bytes that were
    /// evaluated before the device loop began. This closes the save-between-
    /// load-and-watcher-construction race for both files.
    pub fn from_loaded_sources(
        score_path: impl Into<PathBuf>,
        language: WatchLanguage,
        loaded_score: impl Into<String>,
        prebake: Option<(PathBuf, String)>,
        debounce: Duration,
        watch_poll: Duration,
    ) -> Result<Self, RuntimeError> {
        Self::from_loaded_sources_with_prebake_floor(
            score_path,
            language,
            loaded_score,
            prebake,
            debounce,
            watch_poll,
            watch_poll,
        )
    }

    pub fn from_loaded_sources_with_prebake_floor(
        score_path: impl Into<PathBuf>,
        language: WatchLanguage,
        loaded_score: impl Into<String>,
        prebake: Option<(PathBuf, String)>,
        debounce: Duration,
        watch_poll: Duration,
        evaluation_continuation_floor: Duration,
    ) -> Result<Self, RuntimeError> {
        if watch_poll.is_zero() {
            return Err(RuntimeError::Message(
                "live file poll interval must be greater than zero".into(),
            ));
        }
        if evaluation_continuation_floor.is_zero() {
            return Err(RuntimeError::Message(
                "live evaluation continuation reserve must be greater than zero".into(),
            ));
        }
        Ok(Self {
            score_watch: FileWatch::from_loaded_source(
                score_path,
                WatchTarget::Score(language),
                loaded_score,
                debounce,
            ),
            prebake_watch: prebake.map(|(path, source)| {
                FileWatch::from_loaded_source(path, WatchTarget::Prebake, source, debounce)
            }),
            watch_poll,
            next_watch_poll: Duration::ZERO,
            evaluation_continuation_floor,
            continuation_window: ContinuationWindow::default(),
            starving_since: None,
            backlog_dropped_reported: false,
            query_tail_window: ContinuationWindow::default(),
            prebake_state: PrebakeState::Ready,
            pending: VecDeque::new(),
            scheduled_through_frame: 0,
            generation_needs_prefill: true,
            pending_generation: None,
            pending_reload_event: None,
            pending_reload_visible: false,
            pending_score_report: None,
            pending_prebake_report: None,
            prefill_candidate_committed: false,
            prefill_waiting_for_progress: false,
            rollback_pending: false,
            rolled_back_to: None,
            unwatched_cutover_allowed: false,
            control_requery_pending: false,
            check_takeover_freshness: false,
            cut_takeover_deferred_for_loading: false,
            parked_hold: None,
            abandoned_generation: None,
            recovery_epoch: 0,
            load_meter: ProducerLoadMeter::default(),
            completed_turn: None,
            turn_rollbacks: 0,
        })
    }

    pub fn path(&self) -> &std::path::Path {
        self.score_watch.path()
    }

    /// The replacement generation the producer last gave up on, once: a
    /// refused replacement it rolled back, is rolling back, or latched on
    /// will never publish. The engine withdraws a fired launch's line cut
    /// when this names the launch's install.
    pub fn take_abandoned_generation(&mut self) -> Option<u64> {
        self.abandoned_generation.take()
    }

    /// Whether the current cut-takeover replacement's publication is held
    /// for loading - the engine's cue to withdraw the pre-armed line cut so
    /// the outgoing score keeps sounding until the replacement can render.
    pub fn cut_takeover_deferred_for_loading(&self) -> bool {
        self.cut_takeover_deferred_for_loading
    }

    pub fn pending(&self) -> usize {
        self.pending.len()
    }

    /// What the backlog of events waiting for room in the ring holds: its
    /// capacity, which a burst raises and nothing lowers until the producer
    /// goes.
    pub fn backlog_bytes(&self) -> usize {
        self.pending.capacity() * std::mem::size_of::<QueuedAudioEvent>()
    }

    /// Finalize any unobserved turn and return a bounded producer snapshot.
    pub fn producer_load_snapshot(&mut self) -> ProducerLoadSnapshot {
        self.complete_turn(Duration::ZERO);
        self.load_meter.snapshot()
    }

    /// Attribute producer work completed by the host after a scheduling turn.
    #[doc(hidden)]
    pub fn record_asset_preparation(&mut self, elapsed: Duration) {
        let Some(record) = self.completed_turn.as_mut() else {
            return;
        };
        record.add_phase(ProducerPhase::AssetPreparation, elapsed);
        record.cover_age_nanos = record
            .cover_age_nanos
            .saturating_add(crate::producer::duration_nanos(elapsed));
    }

    /// Complete the staged turn after the caller has drained diagnostics and
    /// UI traces. Product loops call this once; a caller that does not have
    /// such work may omit it because the next turn or snapshot flushes with a
    /// zero trace duration.
    pub fn complete_turn(&mut self, trace_duration: Duration) {
        let Some(mut record) = self.completed_turn.take() else {
            return;
        };
        record.add_phase(ProducerPhase::Trace, trace_duration);
        let wall = record.phases_nanos[ProducerPhase::Total as usize]
            .saturating_add(crate::producer::duration_nanos(trace_duration));
        let attributed = [
            ProducerPhase::Evaluation,
            ProducerPhase::ReplacementProbe,
            ProducerPhase::Query,
            ProducerPhase::Scheduler,
            ProducerPhase::Conversion,
            ProducerPhase::AssetPreparation,
            ProducerPhase::RingPush,
            ProducerPhase::Trace,
        ]
        .into_iter()
        .fold(0u64, |total, phase| {
            total.saturating_add(record.phases_nanos[phase as usize])
        });
        let total = wall.max(attributed);
        record.set_phase_nanos(ProducerPhase::Total, total);
        // Session measured the frontier against the clock sampled immediately
        // before scheduling. Age only that measurement plus the later trace
        // work; evaluation and an initial backlog push happened before the
        // sample and are already reflected by it.
        record.cover_end_nanos = record
            .cover_end_nanos
            .saturating_sub(record.cover_age_nanos)
            .saturating_sub(crate::producer::duration_nanos(trace_duration));
        self.load_meter.record(record);
    }

    fn begin_turn(&mut self) -> Instant {
        self.complete_turn(Duration::ZERO);
        self.turn_rollbacks = 0;
        Instant::now()
    }

    fn stage_turn(
        &mut self,
        session: &mut Session,
        started: Instant,
        result: &Result<LiveProducerStep, RuntimeError>,
    ) {
        let mut record = session.take_pending_producer_turn();
        record.producer_backlog_depth = u32::try_from(self.pending.len()).unwrap_or(u32::MAX);
        record.producer_backlog_capacity = u32::try_from(MAX_PENDING_BACKLOG).unwrap_or(u32::MAX);
        record.rollback_count = record.rollback_count.saturating_add(self.turn_rollbacks);
        record.set_phase_nanos(
            ProducerPhase::Total,
            crate::producer::duration_nanos(started.elapsed()),
        );
        record.outcome = match result {
            Ok(step)
                if session.transport().is_stopped()
                    || step.watch == WatchPoll::Stopped
                    || step.prebake_watch == WatchPoll::Stopped =>
            {
                ProducerTurnOutcome::Stopped
            }
            Ok(_) => record.outcome,
            Err(RuntimeError::Cancelled) => ProducerTurnOutcome::Cancelled,
            Err(_) if record.outcome == ProducerTurnOutcome::CommittedRefusal => {
                ProducerTurnOutcome::CommittedRefusal
            }
            Err(_) => ProducerTurnOutcome::AtomicRefusal,
        };
        if result.is_err()
            && record.atomic_refusal_count == 0
            && record.committed_refusal_count == 0
        {
            match record.outcome {
                ProducerTurnOutcome::CommittedRefusal => record.committed_refusal_count = 1,
                _ => record.atomic_refusal_count = 1,
            }
        }
        self.completed_turn = Some(record);
    }

    /// Advance one producer instant.
    ///
    /// `set_generation` MUST happen before a replacement batch is pushed, so
    /// it is invoked here rather than returned as work for the caller. The
    /// callback remains an off-audio-thread operation (normally one atomic
    /// store on `LiveScalarDevice`). `push` returning false is ordinary ring
    /// back-pressure: the exact event is retained and retried next step.
    pub fn step(
        &mut self,
        session: &mut Session,
        observed_at: Duration,
        schedule_now: f64,
        sample_rate: u32,
        set_generation: impl FnMut(u64, u64, TakeoverCut),
        push: impl FnMut(QueuedAudioEvent) -> bool,
    ) -> Result<LiveProducerStep, RuntimeError> {
        self.step_with_clock(
            session,
            observed_at,
            || schedule_now,
            sample_rate,
            set_generation,
            push,
        )
    }

    /// Product form of [`Self::step`] with a freshly sampled audio clock at
    /// each ownership boundary around setup, score replacement and scheduling.
    /// A setup may occupy most of the affordable horizon, so reusing the clock
    /// sampled before it would anchor the replacement in the past.
    pub fn step_with_clock(
        &mut self,
        session: &mut Session,
        observed_at: Duration,
        schedule_now: impl FnMut() -> f64,
        sample_rate: u32,
        set_generation: impl FnMut(u64, u64, TakeoverCut),
        push: impl FnMut(QueuedAudioEvent) -> bool,
    ) -> Result<LiveProducerStep, RuntimeError> {
        self.sync_panic_recovery(session);
        let started = self.begin_turn();
        let result = self.step_with_clock_inner(
            session,
            observed_at,
            schedule_now,
            sample_rate,
            set_generation,
            push,
        );
        self.stage_turn(session, started, &result);
        result
    }

    fn step_with_clock_inner(
        &mut self,
        session: &mut Session,
        observed_at: Duration,
        mut schedule_now: impl FnMut() -> f64,
        sample_rate: u32,
        set_generation: impl FnMut(u64, u64, TakeoverCut),
        mut push: impl FnMut(QueuedAudioEvent) -> bool,
    ) -> Result<LiveProducerStep, RuntimeError> {
        session.consume_audio_confirmations();
        if session.transport().is_stopped() {
            self.pending.clear();
            return Ok(LiveProducerStep {
                watch: WatchPoll::Stopped,
                prebake_watch: WatchPoll::Stopped,
                scheduled: 0,
                pushed: 0,
                pending: 0,
                backpressured: false,
            });
        }
        if self.pending_score_report.is_some() || self.pending_prebake_report.is_some() {
            let mut pushed = 0usize;
            let mut backpressured = false;
            if !self.pending.is_empty() {
                let ring_started = Instant::now();
                while let Some(event) = self.pending.front().copied() {
                    if !push(event) {
                        backpressured = true;
                        break;
                    }
                    self.pending.pop_front();
                    pushed += 1;
                }
                session.record_producer_ring_push(ring_started.elapsed(), pushed, backpressured);
            }
            return Ok(LiveProducerStep {
                watch: self
                    .pending_score_report
                    .take()
                    .map(WatchPoll::Event)
                    .unwrap_or(WatchPoll::Unchanged),
                prebake_watch: self
                    .pending_prebake_report
                    .take()
                    .map(WatchPoll::Event)
                    .unwrap_or(WatchPoll::Unchanged),
                scheduled: 0,
                pushed,
                pending: self.pending.len(),
                backpressured,
            });
        }
        let observed_high_water = self.continuation_window.high_water();
        let query_reserve = self.live_query_reserve();
        let (watches, measure_continuation) = if observed_at >= self.next_watch_poll {
            self.next_watch_poll = observed_at.saturating_add(self.watch_poll);
            // Setup first: when both files settle together, the score is
            // constructed against the newly evaluated helper surface.
            let can_attempt_prebake = self.pending.is_empty() && observed_high_water.is_some();
            let continuation_reserve = observed_high_water.map(|observed| {
                self.evaluation_continuation_floor
                    .saturating_add(observed)
                    .max(query_reserve)
            });
            let (prebake, deferred_ready_setup) = match self.prebake_watch.as_mut() {
                Some(watch) if can_attempt_prebake => (
                    watch.poll_live_prebake_at(
                        session,
                        observed_at,
                        schedule_now(),
                        continuation_reserve.expect("checked above"),
                    ),
                    false,
                ),
                Some(watch) => {
                    // A queried horizon is not downstream cover while its
                    // events are still waiting to cross the fixed ring. With
                    // no continuation sample, a stable identity is likewise
                    // deferred for one measured normal step. In either case
                    // the exact bytes remain fully retryable.
                    watch.observe_live_prebake_at(session, observed_at)
                }
                None => (WatchPoll::Unchanged, false),
            };
            match &prebake {
                WatchPoll::Event(event) => match event.status {
                    crate::ReloadStatus::Installed => {
                        if self.prebake_state != PrebakeState::Ready {
                            self.prebake_state = PrebakeState::RetryingScore;
                        }
                    }
                    crate::ReloadStatus::Rejected => {
                        self.prebake_state = PrebakeState::AwaitingSuccessfulIdentity;
                    }
                },
                WatchPoll::Unchanged | WatchPoll::Pending | WatchPoll::Stopped => {}
            }
            let can_attempt_score = self.pending.is_empty() && continuation_reserve.is_some();

            let (score, observed_new_score, deferred_ready_score) = if prebake == WatchPoll::Stopped
            {
                (WatchPoll::Stopped, false, false)
            } else if prebake == WatchPoll::Pending
                || self.prebake_state == PrebakeState::AwaitingSuccessfulIdentity
            {
                // Observe without delivering. After a setup rejection this is
                // sticky until a NEW setup identity succeeds; otherwise a
                // score that depends on the setup can be rejected once,
                // marked delivered, and stranded permanently.
                self.score_watch.observe_live_score_at(session, observed_at)
            } else if self.prebake_state == PrebakeState::ScoreRetryRejected {
                self.score_watch.observe_live_score_at(session, observed_at)
            } else if self.prebake_state == PrebakeState::RetryingScore {
                // Commit only a successful retry. If the first syntactically
                // valid setup correction still does not satisfy this score,
                // retain the score identity for the next setup identity
                // instead of turning its rejection into a permanent delivery.
                if can_attempt_score {
                    (
                        self.score_watch.poll_live_score_with_clock(
                            session,
                            observed_at,
                            continuation_reserve.expect("checked above"),
                            false,
                            &mut schedule_now,
                        ),
                        false,
                        false,
                    )
                } else {
                    self.score_watch.observe_live_score_at(session, observed_at)
                }
            } else {
                if can_attempt_score {
                    (
                        self.score_watch.poll_live_score_with_clock(
                            session,
                            observed_at,
                            continuation_reserve.expect("checked above"),
                            true,
                            &mut schedule_now,
                        ),
                        false,
                        false,
                    )
                } else {
                    self.score_watch.observe_live_score_at(session, observed_at)
                }
            };
            // A save that has just appeared is still debouncing, and this
            // producer will soon need its sounds. Start loading them from the
            // text now: when the identity is stable enough to install, the
            // install and its first prefill query happen in one call,
            // microseconds apart, with no time for a decode. A file in the
            // disk cache decodes inside the debounce and its first onset
            // converts ready. An uncached file arrives later: the producer
            // never waits on a network.
            if let Some(source) = self.score_watch.take_changed_source() {
                session.warm_sounds_in_source(&source);
            }
            if self.prebake_state == PrebakeState::RetryingScore {
                self.prebake_state = match &score {
                    // A capacity refusal the watch still retries is not a
                    // verdict on the score; it gets the next turn's budget.
                    WatchPoll::Event(event) if event.status == crate::ReloadStatus::Rejected => {
                        if self.score_watch.holds_capacity_retry() {
                            PrebakeState::RetryingScore
                        } else {
                            PrebakeState::ScoreRetryRejected
                        }
                    }
                    WatchPoll::Pending | WatchPoll::Stopped => PrebakeState::RetryingScore,
                    WatchPoll::Unchanged | WatchPoll::Event(_) => PrebakeState::Ready,
                };
            } else if self.prebake_state == PrebakeState::ScoreRetryRejected && observed_new_score {
                // A new score save may be independent of the previously
                // rejected retry. Debounce it normally
                // against the last successfully installed setup. Only the
                // unchanged rejected identity waits for another setup success.
                self.prebake_state = PrebakeState::Ready;
            }
            let ordinary_unchanged =
                matches!(prebake, WatchPoll::Unchanged) && matches!(score, WatchPoll::Unchanged);
            (
                WatchResults { score, prebake },
                observed_high_water.is_some()
                    || deferred_ready_setup
                    || deferred_ready_score
                    || ordinary_unchanged,
            )
        } else if session.transport().is_stopped() {
            (
                WatchResults {
                    score: WatchPoll::Stopped,
                    prebake: WatchPoll::Stopped,
                },
                false,
            )
        } else {
            (
                WatchResults {
                    score: WatchPoll::Unchanged,
                    prebake: WatchPoll::Unchanged,
                },
                true,
            )
        };

        self.sync_panic_recovery(session);

        // Measure only the product continuation that remains AFTER setup and
        // score execution: scheduling, conversion and ring transfer. Feeding
        // evaluation time back into its own reserve makes every later budget
        // shrink recursively and is not the inequality the producer needs.
        let continuation_started = (measure_continuation
            && !matches!(watches.score, WatchPoll::Stopped)
            && !matches!(watches.prebake, WatchPoll::Stopped))
        .then(Instant::now);
        let step = self.finish_step(
            session,
            watches,
            &mut schedule_now,
            sample_rate,
            set_generation,
            push,
        )?;
        if let Some(started) = continuation_started {
            let elapsed = started.elapsed();
            self.continuation_window.record(elapsed);
        }
        Ok(step)
    }

    /// Arm a Session-installed replacement so the next unwatched step completes
    /// the device cutover the same way a watched Installed event does: first
    /// window must query and convert before `set_generation` runs.
    ///
    /// Desktop IPC evaluate cannot go through `FileWatch`; this is the door
    /// that keeps a typo or a query-throwing replacement off the consumer.
    pub fn arm_replacement(&mut self, generation_before: u64, generation_after: u64) {
        self.supersede_cutover(crate::ReloadEvent {
            path: PathBuf::from("<ipc>"),
            target: WatchTarget::Score(WatchLanguage::JavaScript),
            status: crate::ReloadStatus::Installed,
            generation_before,
            generation_after,
            error_kind: None,
            message: None,
        });
        self.unwatched_cutover_allowed = true;
    }

    /// Make `installed`'s generation the one unpublished cutover, awaiting a
    /// first window that queries and converts. It supersedes any earlier
    /// unpublished cutover with its backlog, conversion latch, deferred
    /// rollback and loading-deferred line cut.
    fn supersede_cutover(&mut self, installed: crate::ReloadEvent) {
        self.pending.clear();
        self.pending_generation = Some(installed.generation_after);
        self.pending_reload_event = Some(installed);
        self.pending_reload_visible = true;
        self.generation_needs_prefill = true;
        self.prefill_candidate_committed = false;
        self.prefill_waiting_for_progress = false;
        self.rollback_pending = false;
        self.control_requery_pending = false;
        self.check_takeover_freshness = true;
        self.cut_takeover_deferred_for_loading = false;
    }

    /// Arm a same-graph re-query after a query-time control mutation.
    ///
    /// It uses the full replacement prefill/cutover contract but deliberately
    /// emits no fake file-watch event: moving a slider is not a source save.
    pub fn arm_control_requery(&mut self, generation_before: u64, generation_after: u64) {
        self.arm_replacement(generation_before, generation_after);
        self.pending_reload_visible = false;
        self.control_requery_pending = true;
        self.check_takeover_freshness = false;
    }

    /// Take `session`'s current recovery epoch as already seen, so a rebuild
    /// from before this producer existed is not recovered again.
    pub fn adopt_recovery_epoch(&mut self, session: &Session) {
        self.recovery_epoch = session.recovery_epoch();
    }

    /// Discard work owned by a panicked Session after the host has rebuilt it.
    /// The failed candidate stays unpublished; the restored score receives a
    /// fresh prefill before its invisible generation handoff.
    pub fn recover_after_panic(
        &mut self,
        session: &Session,
        failed_generation: u64,
        message: &str,
    ) {
        self.recovery_epoch = session.recovery_epoch();
        if let Some(generation) = self.pending_generation {
            self.abandoned_generation = Some(generation);
        }
        if self.pending_reload_visible
            && let Some(mut event) = self.pending_reload_event.take()
        {
            event.status = crate::ReloadStatus::Rejected;
            event.generation_after = session.generation();
            event.error_kind = Some("panic".into());
            event.message = Some(message.into());
            self.pending_score_report = Some(event);
        }
        self.arm_replacement(failed_generation, session.generation());
        self.pending_reload_visible = false;
        self.check_takeover_freshness = false;
        self.starving_since = None;
        self.rolled_back_to = None;
        self.parked_hold = None;
    }

    fn sync_panic_recovery(&mut self, session: &Session) {
        if self.recovery_epoch != session.recovery_epoch() {
            // A host can submit another command before its next producer
            // step. Its new replacement already discarded the old backlog.
            if self.pending_generation == Some(session.generation())
                && self
                    .pending_reload_event
                    .as_ref()
                    .is_some_and(|event| event.generation_before >= session.recovery_generation())
            {
                self.recovery_epoch = session.recovery_epoch();
                return;
            }
            let failed_generation = self
                .pending_generation
                .unwrap_or_else(|| session.generation().saturating_sub(1));
            self.recover_after_panic(
                session,
                failed_generation,
                "score turn panicked; the Session was rebuilt",
            );
        }
    }

    /// Refill a replacement audio stream after its ring was deliberately
    /// drained. Any retained `QueuedAudioEvent` uses the discarded stream's frame
    /// rate and must be thrown away before the next push. If a source reload
    /// was already awaiting publication, preserve its visibility and advance
    /// its completion event to the recovery generation; otherwise this is an
    /// invisible same-graph cutover, not a synthetic save.
    pub fn arm_output_recovery_requery(&mut self, generation_before: u64, generation_after: u64) {
        self.pending.clear();
        self.starving_since = None;
        self.generation_needs_prefill = true;
        self.prefill_candidate_committed = false;
        self.prefill_waiting_for_progress = false;
        self.pending_generation = Some(generation_after);
        self.unwatched_cutover_allowed = true;
        self.cut_takeover_deferred_for_loading = false;
        if let Some(event) = self.pending_reload_event.as_mut() {
            debug_assert_eq!(event.generation_after, generation_before);
            event.generation_after = generation_after;
        } else {
            self.pending_reload_event = Some(crate::ReloadEvent {
                path: PathBuf::from("<audio-recycle>"),
                target: WatchTarget::Score(WatchLanguage::JavaScript),
                status: crate::ReloadStatus::Installed,
                generation_before,
                generation_after,
                error_kind: None,
                message: None,
            });
            self.pending_reload_visible = false;
        }
        // Output recovery has drained the audible ring, so even a pre-existing
        // slider transaction can no longer wait indefinitely for a corrective
        // value. Keep the ordinary starvation watchdog armed: if recovery
        // prefill repeatedly fails, rollback re-evaluates the bounded audible
        // snapshot instead of leaving the replacement stream silent forever.
        self.control_requery_pending = false;
        self.check_takeover_freshness = false;
    }

    /// Put the last audible score back after a replacement refused every
    /// onset of its first window. Applied re-arms the cutover to the
    /// restored generation; Deferred (the session would not run score code
    /// on this turn's horizon) leaves the rollback pending for the next
    /// turn; only "nothing to go back to" latches. Returns the message the
    /// turn ends with, already reported as a diagnostic.
    fn roll_back_refused_replacement(
        &mut self,
        session: &mut Session,
        clock: impl FnMut() -> f64,
        error: &RuntimeError,
    ) -> RuntimeError {
        let generation_before = session.generation();
        // Whatever the rollback's outcome, the refused replacement never
        // publishes: applied, it is replaced; deferred, the rollback runs
        // first on every later turn; latched or unavailable, the producer
        // commits to the refusal. Captured before an applied rollback arms
        // its own generation over it.
        if let Some(refused) = self.pending_generation {
            self.abandoned_generation = Some(refused);
        }
        // The refused candidate IS the last rollback: the score we went
        // back to cannot be played either. Latch rather than loop.
        if self.rolled_back_to == Some(generation_before) {
            self.rollback_pending = false;
            self.prefill_candidate_committed = true;
            let message = format!(
                "the new score could not be played ({error}); the last audible score could not be played either - save a score that can"
            );
            session.report_diagnostic(
                "live-error",
                message.clone(),
                serde_json::json!({
                    "live_error": {
                        "kind": "refused-replacement",
                        "message": message,
                        "recoverable": true,
                    }
                }),
            );
            return RuntimeError::Message(message);
        }
        let (message, kind) = match session.rollback_to_previous_source_with_clock(clock) {
            Ok(RollbackAttempt::Applied) => {
                self.turn_rollbacks = self.turn_rollbacks.saturating_add(1);
                self.arm_replacement(generation_before, session.generation());
                self.rolled_back_to = Some(session.generation());
                // The refused replacement never published; its requery anchor
                // belongs to the score that came back, not to a window that
                // will never fire again.
                session.clear_requery_anchor();
                // `arm_replacement` cleared the deferral with the cutover it
                // replaced: the engine must not keep a line cut withdrawn
                // for a replacement that is gone.
                debug_assert!(!self.cut_takeover_deferred_for_loading);
                (
                    format!(
                        "the new score could not be played ({error}); the last audible score keeps playing"
                    ),
                    "refused-replacement",
                )
            }
            Ok(RollbackAttempt::Deferred) => {
                self.rollback_pending = true;
                self.prefill_candidate_committed = false;
                // The deferred rollback is not a cut deferral: nothing is
                // waiting for loading on a cut line any more.
                self.cut_takeover_deferred_for_loading = false;
                (
                    format!(
                        "the new score could not be played ({error}); putting the last audible score back"
                    ),
                    "refused-replacement",
                )
            }
            Ok(RollbackAttempt::Unavailable) => {
                self.rollback_pending = false;
                self.prefill_candidate_committed = true;
                self.cut_takeover_deferred_for_loading = false;
                (
                    format!(
                        "the new score could not be played ({error}); no earlier audible score was available to roll back to"
                    ),
                    "refused-replacement",
                )
            }
            Err(rollback_error) => {
                self.rollback_pending = false;
                self.prefill_candidate_committed = true;
                self.cut_takeover_deferred_for_loading = false;
                (
                    format!(
                        "the new score could not be played ({error}); rolling back to the last audible score also failed ({rollback_error})"
                    ),
                    "refused-replacement",
                )
            }
        };
        session.report_diagnostic(
            "live-error",
            message.clone(),
            serde_json::json!({
                "live_error": {
                    "kind": kind,
                    "message": message,
                    "recoverable": true,
                }
            }),
        );
        RuntimeError::Message(message)
    }

    /// How long an atomic refusal has starved the ring. Loading refusals
    /// never get here: they are held turns (see [`Self::held_step`]), which
    /// reset the clock instead.
    fn retryable_starving_for(&mut self) -> Duration {
        if self.control_requery_pending {
            self.starving_since = None;
            Duration::ZERO
        } else {
            self.starving_since
                .get_or_insert_with(Instant::now)
                .elapsed()
        }
    }

    #[cfg(feature = "device-audio")]
    fn retryable_rollback_after(&self) -> Option<Duration> {
        if self.control_requery_pending {
            return None;
        }
        // A source replacement is still unpublished here, so the old device
        // generation remains authoritative. Fail closed on its first refused
        // prefill instead of spending that old generation's finite cover on
        // retries. Output recovery has no such untouched stream and keeps the
        // ordinary watchdog window.
        if self.generation_needs_prefill
            && self.pending_generation.is_some()
            && self.pending_reload_visible
        {
            return Some(Duration::ZERO);
        }
        Some(STARVED_BEFORE_ROLLBACK)
    }

    /// The step a held turn returns: it scheduled nothing and published
    /// nothing, because its replacement is waiting (for a sample to decode,
    /// for query progress, for a confirmation credit) and has not failed.
    ///
    /// A held turn delivers each watch event once. `finish_step` keeps a copy
    /// of this turn's events before it schedules: the Installed identity rides
    /// the pending cutover and is announced when the generation publishes, and
    /// a Rejected score or a setup event waits in its retained report. The
    /// raw event is not passed through as well, or the host would receive it
    /// on the held turn and again at publication.
    ///
    /// A loading hold also resets the starvation watchdog: a stretch of
    /// windows waiting for samples is not a score that cannot be scheduled,
    /// and a stale stamp from an earlier atomic refusal must not keep
    /// counting through it towards a rollback.
    fn held_step(
        &mut self,
        session: &mut Session,
        mut watch: WatchPoll,
        mut prebake_watch: WatchPoll,
        held: HeldTurn,
    ) -> LiveProducerStep {
        if let WatchPoll::Event(event) = &watch
            && event.status == crate::ReloadStatus::Installed
            && self.pending_reload_visible
            && self.pending_generation == Some(event.generation_after)
        {
            watch = WatchPoll::Unchanged;
        }
        if let Some(event) = self.pending_score_report.take() {
            watch = WatchPoll::Event(event);
        }
        if let Some(event) = self.pending_prebake_report.take() {
            prebake_watch = WatchPoll::Event(event);
        }
        if held.loading {
            self.starving_since = None;
        }
        if let Some(started) = held.cover_age_started {
            session.record_producer_cover_age(started.elapsed());
        }
        LiveProducerStep {
            watch,
            prebake_watch,
            scheduled: 0,
            pushed: held.pushed,
            pending: self.pending.len(),
            backpressured: held.backpressured,
        }
    }

    /// Extend the currently audible generation by one bounded query before
    /// synchronous replacement work occupies the producer thread.
    pub fn shield_reload_with_clock(
        &mut self,
        session: &mut Session,
        mut schedule_now: impl FnMut() -> f64,
        sample_rate: u32,
        mut push: impl FnMut(QueuedAudioEvent) -> bool,
    ) -> Result<bool, RuntimeError> {
        self.sync_panic_recovery(session);
        session.consume_audio_confirmations();
        if self.generation_needs_prefill || self.pending_generation.is_some() {
            return Ok(false);
        }
        let mut first_pushed = 0usize;
        if !self.pending.is_empty() {
            let first_push_started = Instant::now();
            while let Some(event) = self.pending.front().copied() {
                if !push(event) {
                    session.record_producer_ring_push(
                        first_push_started.elapsed(),
                        first_pushed,
                        true,
                    );
                    return Ok(false);
                }
                self.pending.pop_front();
                first_pushed += 1;
            }
            session.record_producer_ring_push(first_push_started.elapsed(), first_pushed, false);
        }

        let now = schedule_now();
        let query_generation = session.generation();
        let batch = match session.schedule_audio_reload_shield_at(
            now,
            sample_rate,
            self.live_query_reserve(),
        ) {
            Ok(batch) => batch,
            Err(LiveAudioScheduleError::Retryable(RuntimeError::Cancelled)) => {
                return Err(RuntimeError::Cancelled);
            }
            Err(
                LiveAudioScheduleError::Retryable(error)
                | LiveAudioScheduleError::AwaitingSamples(error)
                | LiveAudioScheduleError::CandidateCommitted(error)
                | LiveAudioScheduleError::CandidateRefusedScore(error),
            ) if matches!(error, RuntimeError::Panic(_)) => {
                self.recover_after_panic(session, query_generation, &error.to_string());
                return Err(error);
            }
            Err(
                LiveAudioScheduleError::Retryable(_)
                | LiveAudioScheduleError::AwaitingSamples(_)
                | LiveAudioScheduleError::CandidateCommitted(_)
                | LiveAudioScheduleError::CandidateRefusedScore(_),
            ) => return Ok(false),
        };
        let tail_started = batch.tail_started;
        let room = MAX_PENDING_BACKLOG.saturating_sub(self.pending.len());
        let dropped = batch.events.len().saturating_sub(room);
        self.note_scheduled_through(batch.events.iter().map(|event| event.target_frame));
        self.pending.extend(
            batch
                .events
                .into_iter()
                .take(room)
                .map(QueuedAudioEvent::from),
        );
        if dropped > 0 && !self.backlog_dropped_reported {
            self.backlog_dropped_reported = true;
            let message = format!(
                "score scheduled more events than the live backlog holds; {dropped} dropped so later edits stay possible"
            );
            session.report_diagnostic(
                "live-error",
                message.clone(),
                serde_json::json!({
                    "live_error": {
                        "kind": "resource-limit",
                        "message": message,
                        "recoverable": true,
                    }
                }),
            );
        }
        if dropped == 0 {
            self.backlog_dropped_reported = false;
        }
        let mut second_pushed = 0usize;
        let mut second_saturated = false;
        if !self.pending.is_empty() {
            let second_push_started = Instant::now();
            while let Some(event) = self.pending.front().copied() {
                if !push(event) {
                    second_saturated = true;
                    break;
                }
                self.pending.pop_front();
                second_pushed += 1;
            }
            session.record_producer_ring_push(
                second_push_started.elapsed(),
                second_pushed,
                second_saturated,
            );
        }
        self.query_tail_window.record(tail_started.elapsed());
        Ok(true)
    }

    /// Advance continuous playback without polling or applying file changes.
    ///
    /// `rustel play FILE` and `rustel play FILE --watch` share the exact same
    /// scheduling/back-pressure path; only the latter is allowed to produce a
    /// replacement generation. Keeping this distinction here prevents plain
    /// playback from becoming an undocumented watcher merely because it uses
    /// the live device loop.
    pub fn step_unwatched(
        &mut self,
        session: &mut Session,
        schedule_now: f64,
        sample_rate: u32,
        push: impl FnMut(QueuedAudioEvent) -> bool,
    ) -> Result<LiveProducerStep, RuntimeError> {
        self.step_unwatched_with_clock(session, || schedule_now, sample_rate, push)
    }

    /// Product form of [`Self::step_unwatched`] which samples the device clock
    /// only after any producer backlog has crossed the ring.
    pub fn step_unwatched_with_clock(
        &mut self,
        session: &mut Session,
        schedule_now: impl FnMut() -> f64,
        sample_rate: u32,
        push: impl FnMut(QueuedAudioEvent) -> bool,
    ) -> Result<LiveProducerStep, RuntimeError> {
        self.sync_panic_recovery(session);
        if self.pending_generation.is_some() && !self.unwatched_cutover_allowed {
            return Err(RuntimeError::Message(
                "a pending watched generation cutover must be completed through the watched producer route"
                    .into(),
            ));
        }
        self.step_unwatched_with_clock_and_cutover(
            session,
            schedule_now,
            sample_rate,
            |_, _, _| {},
            push,
        )
    }

    /// [`Self::step_unwatched_with_clock`] that can publish an armed
    /// replacement generation (desktop IPC evaluate).
    ///
    /// ```text
    /// arm_replacement(before, after)
    ///   |
    ///   v
    /// query and convert the first window of `after`
    ///   |-- window waits for samples --> held turn; a later turn retries
    ///   |-- window refused ------------> roll back to the last audible score
    ///   v
    /// set_generation(after, takeover_frame, cut)   the consumer flips
    ///   |
    ///   v
    /// push(event)                                  events of `after`
    /// ```
    ///
    /// The cutover closure takes `(generation, takeover_frame, cut)`; the
    /// [`TakeoverCut`] names what the consumer does to what sounds under
    /// the new generation - ring out (an edit), silence at the flip (an
    /// immediate rewind), silence at the takeover (a quantised rewind,
    /// its countdown plays).
    pub fn step_unwatched_with_clock_and_cutover(
        &mut self,
        session: &mut Session,
        schedule_now: impl FnMut() -> f64,
        sample_rate: u32,
        set_generation: impl FnMut(u64, u64, TakeoverCut),
        push: impl FnMut(QueuedAudioEvent) -> bool,
    ) -> Result<LiveProducerStep, RuntimeError> {
        self.sync_panic_recovery(session);
        let started = self.begin_turn();
        let result = self.step_unwatched_with_clock_and_cutover_inner(
            session,
            schedule_now,
            sample_rate,
            set_generation,
            push,
        );
        self.stage_turn(session, started, &result);
        result
    }

    fn step_unwatched_with_clock_and_cutover_inner(
        &mut self,
        session: &mut Session,
        mut schedule_now: impl FnMut() -> f64,
        sample_rate: u32,
        set_generation: impl FnMut(u64, u64, TakeoverCut),
        push: impl FnMut(QueuedAudioEvent) -> bool,
    ) -> Result<LiveProducerStep, RuntimeError> {
        session.consume_audio_confirmations();
        if session.transport().is_stopped() {
            self.pending.clear();
            return Ok(LiveProducerStep {
                watch: WatchPoll::Stopped,
                prebake_watch: WatchPoll::Stopped,
                scheduled: 0,
                pushed: 0,
                pending: 0,
                backpressured: false,
            });
        }
        let watch = if session.transport().is_stopped() {
            WatchPoll::Stopped
        } else {
            WatchPoll::Unchanged
        };
        let measure_continuation = watch != WatchPoll::Stopped;
        let continuation_started = measure_continuation.then(Instant::now);
        let step = self.finish_step(
            session,
            WatchResults {
                score: watch.clone(),
                prebake: watch,
            },
            &mut schedule_now,
            sample_rate,
            set_generation,
            push,
        )?;
        if let Some(started) = continuation_started {
            self.continuation_window.record(started.elapsed());
        }
        Ok(step)
    }

    fn finish_step(
        &mut self,
        session: &mut Session,
        watches: WatchResults,
        mut schedule_now: impl FnMut() -> f64,
        sample_rate: u32,
        mut set_generation: impl FnMut(u64, u64, TakeoverCut),
        mut push: impl FnMut(QueuedAudioEvent) -> bool,
    ) -> Result<LiveProducerStep, RuntimeError> {
        let WatchResults {
            score: mut watch,
            prebake: mut prebake_watch,
        } = watches;
        if let WatchPoll::Event(event) = &watch
            && event.status == crate::ReloadStatus::Rejected
        {
            self.pending_score_report = Some(event.clone());
        }
        if let WatchPoll::Event(event) = &prebake_watch {
            self.pending_prebake_report = Some(event.clone());
        }
        if let WatchPoll::Event(event) = &watch
            && event.status == crate::ReloadStatus::Installed
        {
            // Session owns the new graph already, but the consumer stays on
            // the old generation until the replacement has queried and
            // converted successfully. The watch has already committed these
            // bytes as delivered, so an earlier save's deferred rollback must
            // not discard them.
            self.supersede_cutover(event.clone());
        }
        if watch == WatchPoll::Stopped || prebake_watch == WatchPoll::Stopped {
            if matches!(watch, WatchPoll::Event(_)) {
                self.pending_score_report = None;
            }
            if matches!(prebake_watch, WatchPoll::Event(_)) {
                self.pending_prebake_report = None;
            }
            self.pending.clear();
            return Ok(LiveProducerStep {
                watch,
                prebake_watch,
                scheduled: 0,
                pushed: 0,
                pending: 0,
                backpressured: false,
            });
        }

        if self.generation_needs_prefill
            && self.prefill_candidate_committed
            && self.pending_score_report.is_some()
        {
            // A failed candidate can only be superseded by a new successfully
            // installed score. Surface a newly rejected identity before the
            // unchanged poisoned candidate reports its terminal error again.
            watch = WatchPoll::Event(
                self.pending_score_report
                    .take()
                    .expect("checked pending score report"),
            );
            if let Some(event) = self.pending_prebake_report.take() {
                prebake_watch = WatchPoll::Event(event);
            }
            return Ok(LiveProducerStep {
                watch,
                prebake_watch,
                scheduled: 0,
                pushed: 0,
                pending: self.pending.len(),
                backpressured: !self.pending.is_empty(),
            });
        }

        let mut pushed = 0usize;
        let mut backpressured = false;
        if !self.pending.is_empty() {
            let first_push_started = Instant::now();
            while let Some(event) = self.pending.front().copied() {
                if !push(event) {
                    backpressured = true;
                    break;
                }
                self.pending.pop_front();
                pushed += 1;
            }
            session.record_producer_ring_push(first_push_started.elapsed(), pushed, backpressured);
        }

        let mut scheduled = 0usize;
        let mut reported_cutover = false;
        let mut cover_age_started = None;
        if self.pending.is_empty() {
            let query_reserve = self.live_query_reserve();
            if let Some(generation) = self.pending_generation
                && session.generation() != generation
            {
                self.prefill_candidate_committed = true;
                return Err(RuntimeError::Message(format!(
                    "pending device generation {generation} no longer matches Session generation {}; save a new score identity or restart the producer",
                    session.generation()
                )));
            }
            if self.rollback_pending {
                // The rollback the last turn could not run: try again with
                // this turn's clock. It stays pending, never latched, until
                // the audible score is back or there is none to go back to.
                let refused = RuntimeError::Message("the new score could not be played".into());
                let outcome =
                    self.roll_back_refused_replacement(session, &mut schedule_now, &refused);
                if self.rollback_pending || self.prefill_candidate_committed {
                    return Err(outcome);
                }
            }
            if self.generation_needs_prefill && self.prefill_candidate_committed {
                return Err(RuntimeError::Message(
                    "live generation prefill remains unpublished after a non-transactional scheduling or scalar-audio conversion failure; save a new score identity or restart the producer to retry".into(),
                ));
            }

            // Backlogged events are not downstream cover until the ring has
            // accepted them. Sample the audible device clock only now, after
            // the exact retained backlog has crossed.
            let now = schedule_now();
            cover_age_started = Some(Instant::now());
            // A rewind that reaches this turn with its cycle zero already
            // behind the clock restarts from the top where it can sound:
            // cycle zero slides to now (see `slide_pending_rewind_anchor`).
            // Left in the rendered past, its downbeat started partway into
            // its sample; held past the cover, the gap skip began it bars
            // in.
            if self.generation_needs_prefill && self.pending_generation.is_some() {
                session.slide_pending_rewind_anchor(now);
            }
            // A rewind parked on a loading hold waits for the library to
            // settle something (or for a short safety interval) instead of
            // re-querying the window it just refused. The slide above has
            // already kept its cycle zero on the clock. A stop is let
            // through to the query path, which answers it at once, rather
            // than parked until the interval runs out.
            let generation_needs_prefill = self.generation_needs_prefill;
            let waiting_for_loading =
                self.prefill_waiting_for_progress && self.cut_takeover_deferred_for_loading;
            let pending_generation = self.pending_generation;
            if let Some(parked) = self.parked_hold.as_mut()
                && generation_needs_prefill
                && waiting_for_loading
                && pending_generation == Some(parked.generation)
                && !session.transport().is_stopped()
                && parked.same_library(session)
                && session.sample_settled_epoch() == parked.epoch
                && (0.0..HELD_REWIND_RETRY_SECS).contains(&(now - parked.at))
                && parked.turns < HELD_REWIND_RETRY_TURNS
            {
                parked.turns += 1;
                return Ok(self.held_step(
                    session,
                    watch,
                    prebake_watch,
                    HeldTurn {
                        pushed,
                        backpressured,
                        cover_age_started: None,
                        loading: true,
                    },
                ));
            }
            let budget_mode = if self.generation_needs_prefill {
                if self.pending_generation.is_some() {
                    LiveQueryBudgetMode::ReplacementPrefill
                } else {
                    LiveQueryBudgetMode::InitialPrefill
                }
            } else {
                LiveQueryBudgetMode::Steady
            };
            let reservation = if self.generation_needs_prefill {
                session.reserve_audio_confirmation()?
            } else {
                WindowReservation::Untracked
            };
            // Read before the query: a sample that settles while the query
            // runs moves the epoch past this, so a hold never parks on it.
            let settled_epoch = session.sample_settled_epoch();
            if reservation == WindowReservation::Deferred {
                // No query has run and no onset batch has been consumed. The
                // current reload identity stays staged until a copied-window
                // terminal returns one of the bounded payload credits.
                return Ok(self.held_step(
                    session,
                    watch,
                    prebake_watch,
                    HeldTurn {
                        pushed,
                        backpressured: true,
                        // Nothing was queried: there is no cover to age.
                        cover_age_started: None,
                        loading: false,
                    },
                ));
            }
            let query_generation = session.generation();
            let attempt =
                session.schedule_audio_live_at(now, sample_rate, query_reserve, budget_mode);
            // The variant carries this fact; reading it back out of the
            // message pinned the wording of a line written for people.
            let loading = attempt
                .as_ref()
                .err()
                .is_some_and(LiveAudioScheduleError::is_awaiting_samples);
            let batch = match attempt {
                Ok(batch) => {
                    self.starving_since = None;
                    batch
                }
                Err(
                    LiveAudioScheduleError::Retryable(error)
                    | LiveAudioScheduleError::AwaitingSamples(error)
                    | LiveAudioScheduleError::CandidateCommitted(error)
                    | LiveAudioScheduleError::CandidateRefusedScore(error),
                ) if matches!(error, RuntimeError::Panic(_)) => {
                    self.recover_after_panic(session, query_generation, &error.to_string());
                    return Err(error);
                }
                // Watch started on a leftover typo: keep the clock, play
                // silence, and wait for a valid save. Returning Err here
                // skipped continuation sampling, so the next save was never
                // attempted (`can_attempt_score` stays false) and a restart
                // on leftover-mini died with invalid-argument.
                Err(LiveAudioScheduleError::Retryable(RuntimeError::NoPattern)) => {
                    if let Some(event) = self.pending_score_report.take() {
                        watch = WatchPoll::Event(event);
                    }
                    if let Some(event) = self.pending_prebake_report.take() {
                        prebake_watch = WatchPoll::Event(event);
                    }
                    return Ok(LiveProducerStep {
                        watch,
                        prebake_watch,
                        scheduled: 0,
                        pushed,
                        pending: self.pending.len(),
                        backpressured,
                    });
                }
                Err(LiveAudioScheduleError::Retryable(error))
                | Err(LiveAudioScheduleError::AwaitingSamples(error)) => {
                    if matches!(&error, RuntimeError::Cancelled) {
                        return Err(error);
                    }
                    // A refused unpublished source replacement rolls back at
                    // once; a steady/recovery failure gets the longer
                    // starvation watchdog. Either way, retrying past the
                    // relevant window would leave the set silent.
                    //
                    // A sample library still in flight is NOT that: keep the
                    // same identity eligible for later scheduling.
                    // Rolling back on it threw away the edit that asked for
                    // the sample AND restarted its download, so the first hit
                    // of a fresh sample could remain silent until restart.
                    // `AwaitingSamples` is that case spelled as a variant;
                    // its message still says "still loading" either way.
                    if loading && self.generation_needs_prefill {
                        self.prefill_waiting_for_progress = true;
                        self.check_takeover_freshness = false;
                        // The refusal consumed the window on the scheduler's
                        // side: cursor advanced, onsets marked emitted. A
                        // rewind's first window must not be lost that way:
                        // cycle zero's onsets would dedup out of every retry
                        // and their tracks would enter late, alone. Reopen
                        // the window (cursor back to the takeover, emission
                        // marks cleared) so it re-queries whole once the
                        // samples decode, and put the takeover pair back for
                        // this replacement to consume at publication. An
                        // ordinary edit (or a control requery, cut=false)
                        // keeps its far-cursor contract: its loading span is
                        // deliberately skipped and the same identity
                        // continues from later scheduling, so its takeover
                        // pair goes back untouched for publication. The
                        // reservation is released; the next turn reserves a
                        // fresh one.
                        if self.pending_generation.is_some()
                            && let Some(takeover) = session.take_requery_takeover()
                        {
                            if takeover.1 != TakeoverCut::None {
                                if let WindowReservation::Ready(key) = reservation {
                                    session.cancel_unpublished_audio_confirmation(key);
                                }
                                // A window that cannot reopen (nothing past
                                // the anchor was consumed) still owns its
                                // takeover and cut: put the pair back as the
                                // partial hold below does, or the rewind
                                // would publish later as an edit - no cut,
                                // takeover frame zero - over the old score.
                                if !session.reopen_loading_window(now, takeover) {
                                    session.rearm_requery_takeover(takeover);
                                }
                                // The engine's cue: the pre-armed line cut
                                // must be withdrawn while this flip waits.
                                self.cut_takeover_deferred_for_loading = true;
                                self.parked_hold = self.pending_generation.map(|generation| {
                                    ParkedHold::new(generation, settled_epoch, session, now)
                                });
                            } else {
                                session.rearm_requery_takeover(takeover);
                                self.cut_takeover_deferred_for_loading = false;
                            }
                        } else {
                            // No cutover pending: a steady-state loading
                            // retry cuts nothing anywhere.
                            self.cut_takeover_deferred_for_loading = false;
                        }
                    }
                    // A loading refusal that still waits for its samples is a
                    // held turn, not a failed one: report Ok, leave the
                    // previous covers untouched, and let the next turn retry.
                    // An Err here would make the engine report the same
                    // "still loading" error once a window during a decode,
                    // and mark as failed the launch whose cut this producer
                    // defers. Loading is not starvation: the held turn
                    // resets the watchdog.
                    if loading {
                        return Ok(self.held_step(
                            session,
                            watch,
                            prebake_watch,
                            HeldTurn {
                                pushed,
                                backpressured,
                                cover_age_started,
                                loading: true,
                            },
                        ));
                    }
                    let starving_for = self.retryable_starving_for();
                    if self
                        .retryable_rollback_after()
                        .is_some_and(|rollback_after| starving_for >= rollback_after)
                    {
                        self.starving_since = None;
                        let generation_before = session.generation();
                        // Report whether recovery applied, deferred, or had no
                        // previous score available.
                        let message = match session
                            .rollback_to_previous_source_with_clock(&mut schedule_now)
                        {
                            Ok(RollbackAttempt::Applied) => {
                                self.turn_rollbacks = self.turn_rollbacks.saturating_add(1);
                                // The generation this re-arm replaces never
                                // publishes.
                                if let Some(starved) = self.pending_generation {
                                    self.abandoned_generation = Some(starved);
                                }
                                // A rollback INSTALLS the previous source, so
                                // the Session moves to a new generation and the
                                // one the device is still waiting on no longer
                                // exists. Left alone that mismatch is terminal:
                                // the next step returns "pending device
                                // generation N no longer matches Session
                                // generation M" on every iteration, forever, and
                                // the set stays silent until a human saves
                                // something else. So the recovery has to re-arm
                                // the cutover it just invalidated - otherwise
                                // the watchdog fires correctly and still strands
                                // the producer, which is exactly what a heavy
                                // score did.
                                self.arm_replacement(generation_before, session.generation());
                                format!(
                                    "installed score could not be scheduled ({error}); rolled back to the last audible score"
                                )
                            }
                            Ok(RollbackAttempt::Deferred) => format!(
                                "installed score could not be scheduled ({error}); rollback was deferred and its target was retained"
                            ),
                            Ok(RollbackAttempt::Unavailable) => format!(
                                "installed score could not be scheduled ({error}); no earlier audible score was available to roll back to"
                            ),
                            Err(rollback_error) => format!(
                                "installed score could not be scheduled ({error}); rolling back to the last audible score also failed ({rollback_error})"
                            ),
                        };
                        session.report_diagnostic(
                            "live-error",
                            message.clone(),
                            serde_json::json!({
                                "live_error": {
                                    "kind": "resource-limit",
                                    "message": message,
                                    "recoverable": true,
                                }
                            }),
                        );
                    }
                    return Err(error);
                }
                Err(
                    LiveAudioScheduleError::CandidateCommitted(error)
                    | LiveAudioScheduleError::CandidateRefusedScore(error),
                ) => {
                    // A replacement whose first window refuses every onset
                    // (an unknown sound forced past the check) must not
                    // silence the set. The scheduler has consumed the
                    // window, so the candidate cannot be retried, but the
                    // last audible score can be put back, as the retryable
                    // path does. A latch here would return an error on
                    // every turn with the old generation gone.
                    if self.generation_needs_prefill && self.pending_generation.is_some() {
                        return Err(self.roll_back_refused_replacement(
                            session,
                            &mut schedule_now,
                            &error,
                        ));
                    }
                    if self.generation_needs_prefill {
                        self.prefill_candidate_committed = true;
                    }
                    return Err(error);
                }
            };
            if session.transport().is_stopped() {
                if self.generation_needs_prefill {
                    self.prefill_candidate_committed = true;
                }
                return Err(RuntimeError::Cancelled);
            }

            if self.prefill_waiting_for_progress && !batch.prefill_progress {
                self.query_tail_window.record(batch.tail_started.elapsed());
                return Ok(self.held_step(
                    session,
                    watch,
                    prebake_watch,
                    HeldTurn {
                        pushed,
                        backpressured,
                        cover_age_started,
                        loading: false,
                    },
                ));
            }
            let takeover = self
                .pending_generation
                .and_then(|_| session.take_requery_takeover());
            let takeover_time = takeover.map(|(time, _)| time);
            let takeover_cut = takeover
                .map(|(_, intent)| intent)
                .unwrap_or(TakeoverCut::None);
            if self.check_takeover_freshness
                && !self.prefill_waiting_for_progress
                && batch.dispositions.skipped_loading == 0
                && let Some(takeover_time) = takeover_time
            {
                let publication_now = schedule_now();
                // A CUT takeover (a rewind's restart) publishes a moment
                // late rather than failing. Its cycle zero was slid to this
                // turn's clock before the query (`slide_pending_rewind_anchor`)
                // if it had fallen behind, so what is late here is only the
                // query and conversion time since: the consumer latches a
                // restart floor at the cut flip and sounds those past-due
                // first onsets whole, at the floor. Refusing instead would
                // roll back a restart whose line cut the consumer may
                // already have fired, leaving that room silent. An ordinary
                // edit still refuses to publish into the past: its takeover
                // is where the outgoing score's retained events stop, and a
                // late publication would ask rendered frames to change.
                if session.transport().is_stopped()
                    || (publication_now > takeover_time && takeover_cut == TakeoverCut::None)
                {
                    if let WindowReservation::Ready(key) = reservation {
                        session.cancel_unpublished_audio_confirmation(key);
                    }
                    // This window has already consumed its query. Neither its
                    // events nor external intents may survive as a retry.
                    #[cfg(feature = "midi")]
                    drop(session.take_pending_midi());
                    #[cfg(feature = "osc")]
                    drop(session.take_pending_osc());
                    #[cfg(feature = "serial")]
                    drop(session.take_pending_serial());
                    drop(batch);
                    if session.transport().is_stopped() {
                        return Err(RuntimeError::Cancelled);
                    }
                    let error = RuntimeError::Message(
                        "replacement prefill finished after its takeover".into(),
                    );
                    return Err(self.roll_back_refused_replacement(
                        session,
                        &mut schedule_now,
                        &error,
                    ));
                }
            }
            // A rewind's first window is a restart, shaped like a transport
            // start: every track the score names should land on its first
            // beat. A window that published with skipped-loading onsets
            // would drop those onsets for good (the scheduler cursor has
            // moved past them), and their tracks would enter late, alone.
            // So a cut takeover with loading skips holds publication the
            // same way an empty loading window does: wait for fresh
            // scheduling, never certify a partial first window, and put the
            // (takeover, cut) pair back so this replacement still consumes
            // it when it publishes. The takeover also sits ahead of the
            // device clock (a continuity margin), so deferring the flip
            // does not move the boundary: the old generation's voices are
            // cut at the flip whenever the ready window lands. An ordinary
            // edit keeps the skip-and-log contract: silence on one late
            // sample is better than a hold on the whole set.
            let rewind_loading_skip = takeover_cut != TakeoverCut::None
                && batch.dispositions.skipped_loading > 0
                && self.generation_needs_prefill;
            if rewind_loading_skip {
                // The query that built this batch already moved the cursor
                // past the window and marked its onsets emitted, the ready
                // ones too. Putting the pair back alone would lose them: a
                // converted kick is dropped with the batch and never comes
                // back, so the restart begins without its downbeat (and an
                // empty refill span can then publish the rewind with no
                // first window at all). Reopen to the anchor exactly as the
                // all-refused path does; nothing of this batch reached the
                // ring, and its external intents are re-collected with it.
                if let WindowReservation::Ready(key) = reservation {
                    session.cancel_unpublished_audio_confirmation(key);
                }
                if let Some(time) = takeover_time
                    && !session.reopen_loading_window(now, (time, takeover_cut))
                {
                    session.rearm_requery_takeover((time, takeover_cut));
                }
                #[cfg(feature = "midi")]
                drop(session.take_pending_midi());
                #[cfg(feature = "osc")]
                drop(session.take_pending_osc());
                #[cfg(feature = "serial")]
                drop(session.take_pending_serial());
                // The flip waits for loading: the engine must withdraw the
                // line cut pre-armed at fire time. Otherwise the outgoing
                // score fades out at the line before the replacement can
                // sound: a gap of silence, and a click when it ends.
                self.cut_takeover_deferred_for_loading = true;
                self.parked_hold = self
                    .pending_generation
                    .map(|generation| ParkedHold::new(generation, settled_epoch, session, now));
                self.prefill_waiting_for_progress = true;
                self.check_takeover_freshness = false;
                self.query_tail_window.record(batch.tail_started.elapsed());
                drop(batch);
                return Ok(self.held_step(
                    session,
                    watch,
                    prebake_watch,
                    HeldTurn {
                        pushed,
                        backpressured,
                        cover_age_started,
                        loading: true,
                    },
                ));
            }
            self.prefill_waiting_for_progress = false;
            let takeover_frame = takeover_time
                .map(|time| crate::render::takeover_frame_at(time, sample_rate))
                .unwrap_or(0);
            let (confirmation, certified_from) = match reservation {
                WindowReservation::Ready(key)
                    if batch.query_threw
                        || (batch.dispositions.intended != 0
                            && batch.dispositions.refused == batch.dispositions.intended) =>
                {
                    // Initial playback historically logs unsupported voices
                    // and continues. Keep that policy, without certifying its
                    // all-refused or caught-throw window as intentional silence.
                    // A replacement's FIRST window arrives here all-refused
                    // only after the look-ahead certified it renders
                    // somewhere in its stretch; steady windows later can
                    // still land all-refused in the score's missing
                    // stretches, which skip and report. One that rendered
                    // nothing ahead took the committed-refusal branch above.
                    session.cancel_unpublished_audio_confirmation(key);
                    (None, 0)
                }
                WindowReservation::Ready(key) if batch.events.len() <= MAX_PENDING_BACKLOG => {
                    // The device certifies only onsets its copied interval can
                    // cover: `start_frame` begins at the render frontier or
                    // the takeover, whichever is later. An edit-instant
                    // replacement's pre-takeover onsets - the very ones the
                    // overlap re-query keeps audible - sit before it and can
                    // never be copied, so tagging them would leave the window
                    // permanently uncertified (the old generation rings them
                    // until the takeover, and this generation's copy arrives
                    // too late to matter). They keep playing without a
                    // receipt: the offer counts only the certifiable onsets,
                    // and ordinals are numbered over those alone.
                    let start_frame = ((now * f64::from(sample_rate)).floor().max(0.0) as u64)
                        .max(takeover_frame);
                    // Count event by event, not with a binary search. The
                    // batch is in onset order, and a stretched voice is aimed
                    // a vocoder latency before its onset, so target frames
                    // can be out of order. If an event before `start_frame`
                    // has a receipt, the device never certifies the window.
                    let uncertifiable = batch
                        .events
                        .iter()
                        .filter(|event| event.target_frame < start_frame)
                        .count();
                    let last_frame = batch
                        .events
                        .iter()
                        .map(|event| event.target_frame)
                        .max()
                        .unwrap_or(start_frame)
                        .max(start_frame);
                    let end_frame = last_frame
                        .checked_add(1)
                        .ok_or_else(|| {
                            self.prefill_candidate_committed = true;
                            RuntimeError::ResourceLimit(
                                "audio confirmation frame counter exhausted".into(),
                            )
                        })?
                        .max(batch.queried_through_frame);
                    let counts = batch.dispositions;
                    // Each subtraction is clamped by its own minuend: the
                    // untagged count derives from `batch.events`, and it is
                    // bounded against `converted` and `intended` locally -
                    // not via the conversion loop's `converted ≤ intended`
                    // shape. If that shape ever changes (say a chord onset
                    // yielding several events), the offer certifies fewer
                    // intents instead of underflowing.
                    let untagged = u32::try_from(uncertifiable)
                        .unwrap_or(u32::MAX)
                        .min(counts.converted)
                        .min(counts.intended);
                    let intended = counts.intended - untagged;
                    // Mirror the ledger's all-refused rejection on the
                    // certifiable counts: a window whose converted onsets all
                    // sit before its certification start and whose remaining
                    // intents all refused cannot confirm anything. Pushing
                    // the events still plays them; they just carry no receipt.
                    if counts.refused != 0 && counts.refused == intended {
                        session.cancel_unpublished_audio_confirmation(key);
                        (None, 0)
                    } else {
                        let offer = WindowOffer {
                            key,
                            generation: session.generation(),
                            takeover_frame,
                            start_frame,
                            end_frame,
                            intended,
                            converted: counts.converted - untagged,
                            skipped_loading: counts.skipped_loading,
                            refused: counts.refused,
                            external: counts.external,
                        };
                        if let Err(error) = session.publish_audio_confirmation(offer) {
                            // The scheduler has consumed this exact window. Never
                            // retry it as empty successful silence after a broken
                            // publication invariant.
                            self.prefill_candidate_committed = true;
                            return Err(error);
                        }
                        (Some(key), start_frame)
                    }
                }
                WindowReservation::Ready(key) => {
                    session.cancel_unpublished_audio_confirmation(key);
                    session.report_diagnostic(
                        "live-confirmation",
                        "rollback confirmation unavailable: the first window exceeds the bounded audio backlog",
                        serde_json::json!({ "kind": "confirmation-window-limit" }),
                    );
                    (None, 0)
                }
                WindowReservation::Untracked => (None, 0),
                WindowReservation::Deferred => unreachable!("deferred before querying"),
            };

            // A silent successful replacement still owns the consumer. For a
            // non-silent one this remains strictly before the first new event
            // crosses the ring.
            if let Some(generation) = self.pending_generation {
                let completed = self.pending_reload_event.take().ok_or_else(|| {
                    RuntimeError::Message(
                        "replacement generation completed without its retained reload event".into(),
                    )
                })?;
                // The takeover frame - where this generation's re-query
                // cursor starts - rides with the flip so the consumer keeps
                // the old generation's earlier onsets sounding (the reload
                // takeover contract; dropping them cut the music by one
                // continuity margin on every save). The cut flag rides it
                // too: a from-zero reload (a rewind) silences what sounds
                // under it at exactly that frame, so the restarted loop is
                // heard alone, like a retriggered sample.
                // The offer precedes generation and event publication. Only
                // its exact later consumer result can promote the captured
                // payload to Session's rollback target.
                set_generation(generation, takeover_frame, takeover_cut);
                // The device keeps the outgoing generations' events aimed
                // before the takeover frame. The session ledger does the
                // same, so a latency-compensated onset does not sound twice.
                session.audio_takeover_published(generation, takeover_frame, takeover_cut);
                // The window that consumed the pair published; its requery
                // anchor served its purpose and must not outlive it.
                session.clear_requery_anchor();
                self.pending_generation = None;
                self.rolled_back_to = None;
                self.unwatched_cutover_allowed = false;
                // The cut the engine was told to withdraw is moot: the
                // replacement is ON the device now, cut intent intact.
                self.cut_takeover_deferred_for_loading = false;
                if self.pending_reload_visible {
                    watch = WatchPoll::Event(completed);
                    reported_cutover = true;
                }
                self.pending_reload_visible = false;
                self.control_requery_pending = false;
                self.check_takeover_freshness = false;
            }
            self.generation_needs_prefill = false;
            let tail_started = batch.tail_started;
            let events = batch.events;
            scheduled = events.len();
            // BOUNDED BACKLOG. The ring holds an event until its onset time
            // arrives, so a score that schedules a huge span leaves the
            // producer with a backlog covering minutes of audio. While it is
            // non-empty `can_attempt_score` is false, i.e. NO later save is
            // ever attempted. Keeping the oldest events preserves what is
            // about to sound; the excess is dropped and reported once.
            let room = MAX_PENDING_BACKLOG.saturating_sub(self.pending.len());
            let dropped = events.len().saturating_sub(room);
            self.note_scheduled_through(events.iter().map(|event| event.target_frame));
            let mut next_ordinal = 0u32;
            self.pending.extend(
                events
                    .into_iter()
                    .take(room)
                    .enumerate()
                    .map(|(index, event)| QueuedAudioEvent {
                        event,
                        // Only an event the device can certify carries a
                        // receipt. This condition is the opposite of the
                        // one that counts `uncertifiable`, so the ordinals
                        // match the offer.
                        confirmation: confirmation
                            .filter(|_| event.target_frame >= certified_from)
                            .map(|key| {
                                let ordinal = next_ordinal;
                                next_ordinal += 1;
                                ConfirmationOnset { key, ordinal }
                            }),
                        expected_sample_identity: batch
                            .sample_identities
                            .get(index)
                            .copied()
                            .flatten(),
                    }),
            );
            if dropped > 0 && !self.backlog_dropped_reported {
                self.backlog_dropped_reported = true;
                let message = format!(
                    "score scheduled more events than the live backlog holds; {dropped} dropped so later edits stay possible"
                );
                session.report_diagnostic(
                    "live-error",
                    message.clone(),
                    serde_json::json!({
                        "live_error": {
                            "kind": "resource-limit",
                            "message": message,
                            "recoverable": true,
                        }
                    }),
                );
            }
            if dropped == 0 {
                self.backlog_dropped_reported = false;
            }
            let pushed_before = pushed;
            if !self.pending.is_empty() {
                let second_push_started = Instant::now();
                while let Some(event) = self.pending.front().copied() {
                    if !push(event) {
                        backpressured = true;
                        break;
                    }
                    self.pending.pop_front();
                    pushed += 1;
                }
                session.record_producer_ring_push(
                    second_push_started.elapsed(),
                    pushed.saturating_sub(pushed_before),
                    backpressured,
                );
            }
            self.query_tail_window.record(tail_started.elapsed());
        }

        if !reported_cutover && let Some(event) = self.pending_score_report.take() {
            watch = WatchPoll::Event(event);
        }
        if let Some(event) = self.pending_prebake_report.take() {
            prebake_watch = WatchPoll::Event(event);
        }
        if let Some(started) = cover_age_started {
            session.record_producer_cover_age(started.elapsed());
        }

        Ok(LiveProducerStep {
            watch,
            prebake_watch,
            scheduled,
            pushed,
            pending: self.pending.len(),
            backpressured,
        })
    }
}

impl LiveFileProducer {
    /// Remember how far ahead the backlog now reaches.
    fn note_scheduled_through(&mut self, targets: impl Iterator<Item = u64>) {
        let last = targets.max().map_or(0, |last| last.saturating_add(1));
        self.scheduled_through_frame = self.scheduled_through_frame.max(last);
    }

    /// One past the latest target any scheduled event has carried.
    pub fn scheduled_through_frame(&self) -> u64 {
        self.scheduled_through_frame
    }

    /// The earliest target still waiting in the backlog, if any.
    pub fn pending_target_floor(&self) -> Option<u64> {
        self.pending
            .iter()
            .map(|queued| queued.event.target_frame)
            .min()
    }

    /// Decoded samples still needed by events waiting for ring space.
    pub fn pending_sample_ids(&self) -> impl Iterator<Item = SampleId> + '_ {
        self.pending.iter().filter_map(|queued| {
            let event = &queued.event;
            if event.synth.is_some() {
                return None;
            }
            event
                .wavetable
                .map(|table| table.table)
                .or_else(|| event.sample.map(|sample| sample.sample))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_prefill_that_outlasts_its_takeover_keeps_the_confirmed_score() {
        use rustel_audio::device::ManualLiveOutput;

        let mut session = Session::new().expect("session");
        session.set_schedule_lead(0.0);
        session.set_continuity_margin(0.125);
        session.evaluate_mini("~").expect("native silent score");
        session.restart_transport_at(0.0);
        let original = session.generation();
        let mut output = ManualLiveOutput::new(48_000, original).expect("output");
        session
            .bind_audio_confirmations(output.device().confirmations())
            .expect("bind output");
        let mut producer = LiveFileProducer::unwatched(Duration::from_millis(2)).expect("producer");
        producer
            .step_unwatched_with_clock_and_cutover(
                &mut session,
                || 0.0,
                48_000,
                |_, _, _| panic!("initial prefill must not replace a generation"),
                |event| output.device().push(event),
            )
            .expect("initial prefill");
        assert_eq!(session.confirmed_audio_generation(), None);
        output.render(&mut [0.0_f32; 256]);
        assert!(session.consume_audio_confirmations());
        assert_eq!(session.confirmed_audio_generation(), Some(original));

        let now = output.device().clock_seconds();
        let candidate = session.reload_at("~ ~", true, now).expect("replacement");
        producer.arm_replacement(original, candidate);
        // The query succeeds, but the real-time frontier advances while it
        // runs. Replay then finishes later still and must use that fresh time.
        let replay_now = now + 0.5;
        let mut clocks = [now, now + 0.25, replay_now].into_iter();
        let mut published = Vec::new();
        let result = producer.step_unwatched_with_clock_and_cutover(
            &mut session,
            || clocks.next().unwrap_or(replay_now),
            48_000,
            |generation, frame, cut| {
                published.push((generation, frame));
                output.device().set_generation(generation, frame, cut);
            },
            |_| panic!("silent replacement has no audio events"),
        );
        assert!(published.is_empty(), "expired replacement was published");
        let error = result.expect_err("consumed takeover must reject the replacement");
        assert!(error.to_string().contains("takeover"), "{error}");
        assert_eq!(output.device().generation(), original);
        assert_eq!(session.confirmed_audio_generation(), Some(original));
        assert_eq!(session.active_source(), Some("~"));
        assert_eq!(producer.pending_generation, Some(session.generation()));
        assert!(!producer.prefill_candidate_committed);
        assert!(producer.pending.is_empty());

        let restored = session.generation();
        producer
            .step_unwatched_with_clock_and_cutover(
                &mut session,
                || replay_now,
                48_000,
                |generation, frame, _cut| published.push((generation, frame)),
                |_| panic!("restored silence has no audio events"),
            )
            .expect("restored score receives a fresh prefill");
        assert_eq!(
            published,
            [(restored, ((replay_now + 0.125) * 48_000.0).round() as u64)]
        );
        assert!(!producer.check_takeover_freshness);
    }

    /// The producer stages the first window of a replacement and then rolls
    /// the replacement back. No host gets that window's OSC bundles, so they
    /// do not move the frontier: the restored score sends those onsets.
    #[cfg(feature = "osc")]
    #[test]
    fn a_rolled_back_replacement_leaves_its_osc_onsets_to_the_restored_score() {
        const SCORE: &str = r#"note("c3*8").s("sawtooth").osc(57120)"#;
        const RATE: u32 = 48_000;
        let frame = |(_, intent): &(f64, crate::osc_bridge::OscOnset)| {
            crate::render::onset_frame_at(intent.target_time, RATE)
        };
        let mut session = Session::with_config(crate::SessionConfig {
            cps: 1.0,
            horizon: 0.5,
            ..Default::default()
        })
        .expect("session");
        session.set_direct_diagnostic_logging(false);
        session.set_schedule_lead(0.0);
        session.set_continuity_margin(0.125);
        session.evaluate(SCORE).expect("score");
        session.restart_transport_at(0.0);
        let original = session.generation();
        // Inject replay policy, not consumer-copy evidence.
        session
            .mark_audible_generation(original)
            .expect("injected replay-policy target");
        let mut producer = LiveFileProducer::unwatched(Duration::from_millis(2)).expect("producer");
        producer
            .step_unwatched_with_clock_and_cutover(
                &mut session,
                || 0.0,
                RATE,
                |_, _, _| panic!("initial prefill must not replace a generation"),
                |_| true,
            )
            .expect("initial prefill");
        // The host hands the first window out: an onset every 6000 frames.
        let first = session.take_pending_osc();
        for (_, intent) in &first {
            session.note_osc_handed_out(intent.generation, intent.target_time);
        }
        let frontier = first.last().map(frame).expect("the score reaches OSC");
        assert_eq!(frontier, 24_000, "test premise");

        // The replacement's first window covers the onsets to 0.75 s. The
        // clock passes its takeover while it converts, so it is rolled back.
        let candidate = session
            .reload_at(&format!("{SCORE}\n// again\n"), false, 0.25)
            .expect("replacement");
        producer.arm_replacement(original, candidate);
        let mut clocks = [0.25, 0.5].into_iter();
        let error = producer
            .step_unwatched_with_clock_and_cutover(
                &mut session,
                || clocks.next().unwrap_or(0.5625),
                RATE,
                |_, _, _| panic!("an expired replacement was published"),
                |_| true,
            )
            .expect_err("the consumed takeover rejects the replacement");
        assert!(error.to_string().contains("takeover"), "{error}");
        assert!(
            session.take_pending_osc().is_empty(),
            "the rolled-back window left bundles for a host"
        );

        let mut published = Vec::new();
        producer
            .step_unwatched_with_clock_and_cutover(
                &mut session,
                || 0.5625,
                RATE,
                |generation, takeover, _cut| published.push((generation, takeover)),
                |_| true,
            )
            .expect("the restored score has a fresh prefill");
        assert_eq!(published, [(session.generation(), 33_000)], "test premise");
        let restored: Vec<u64> = session.take_pending_osc().iter().map(frame).collect();
        assert_eq!(
            restored.first(),
            Some(&36_000),
            "the rolled-back window staged the onset at 0.75 s, and no host sent it"
        );
    }

    #[test]
    fn a_direct_native_replacement_without_a_takeover_does_not_check_freshness() {
        let mut session = Session::new().expect("session");
        session
            .set_pattern(rustel_core::silence())
            .expect("initial graph");
        let original = session.generation();
        session
            .set_pattern(rustel_core::silence())
            .expect("native replacement");
        let candidate = session.generation();
        assert_eq!(session.take_requery_takeover_time(), None);
        let mut producer = LiveFileProducer::unwatched(Duration::from_millis(2)).expect("producer");
        producer.arm_replacement(original, candidate);
        let mut clocks = [1.0].into_iter();
        let mut published = Vec::new();
        producer
            .step_unwatched_with_clock_and_cutover(
                &mut session,
                || {
                    clocks
                        .next()
                        .expect("no takeover means no fresh-clock check")
                },
                48_000,
                |generation, frame, _cut| published.push((generation, frame)),
                |_| panic!("native silence has no events"),
            )
            .expect("direct replacement keeps its existing policy");
        assert_eq!(published, [(candidate, 0)]);
        assert!(!producer.check_takeover_freshness);
    }

    fn full_horizon_waiting_prefill() -> (Session, LiveFileProducer, u64) {
        let mut session = Session::with_config(crate::SessionConfig {
            cps: 1.0,
            horizon: 0.5,
            ..Default::default()
        })
        .expect("session");
        session.evaluate_mini("~").expect("native silence");
        session.restart_transport_at(0.0);
        session.set_continuity_margin(0.125);
        let before = session.generation();
        let generation = session.reload_at("~", true, 0.0).expect("replacement");
        let mut producer = LiveFileProducer::unwatched(Duration::from_millis(2)).expect("producer");
        producer.arm_replacement(before, generation);
        let batch = match session.schedule_audio_live_at(
            0.0,
            48_000,
            Duration::from_millis(2),
            LiveQueryBudgetMode::ReplacementPrefill,
        ) {
            Ok(batch) => batch,
            Err(_) => panic!("native silent fill must succeed"),
        };
        assert!(batch.prefill_progress);
        assert!(batch.events.is_empty());
        // The horizon was queried and drained before this wait began. Only
        // later progress may consume the retained cutover.
        producer.prefill_waiting_for_progress = true;
        (session, producer, generation)
    }

    #[test]
    fn prefill_wait_delivers_reports_without_consuming_the_cutover() {
        let (mut session, mut producer, generation) = full_horizon_waiting_prefill();
        let candidate = producer.pending_reload_event.clone().expect("candidate");
        let rejected = crate::ReloadEvent {
            path: PathBuf::from("<rejected-mini>"),
            target: WatchTarget::Score(WatchLanguage::Mini),
            status: crate::ReloadStatus::Rejected,
            generation_before: generation,
            generation_after: generation,
            error_kind: Some("parse-error".into()),
            message: Some("rejected replacement".into()),
        };
        let prebake = crate::ReloadEvent {
            path: PathBuf::from("<rejected-setup>"),
            target: WatchTarget::Prebake,
            ..rejected.clone()
        };
        for reports in [true, false] {
            let score = if reports {
                WatchPoll::Event(rejected.clone())
            } else {
                WatchPoll::Unchanged
            };
            let setup = if reports {
                WatchPoll::Event(prebake.clone())
            } else {
                WatchPoll::Unchanged
            };
            let step = producer
                .finish_step(
                    &mut session,
                    WatchResults {
                        score: score.clone(),
                        prebake: setup.clone(),
                    },
                    || 0.0,
                    48_000,
                    |_, _, _| panic!("empty waiting turn published a generation"),
                    |_| panic!("empty waiting turn emitted audio"),
                )
                .expect("waiting turn");
            assert_eq!(step.watch, score);
            assert_eq!(step.prebake_watch, setup);
            assert_eq!((step.scheduled, step.pushed, step.pending), (0, 0, 0));
            assert!(!step.backpressured);
            assert!(producer.pending_score_report.is_none());
            assert!(producer.pending_prebake_report.is_none());
            assert_eq!(producer.pending_reload_event.as_ref(), Some(&candidate));
            assert_eq!(producer.pending_generation, Some(generation));
            assert!(producer.generation_needs_prefill);
            assert!(producer.prefill_waiting_for_progress);
        }
        assert_eq!(session.take_requery_takeover_time(), Some(0.125));
    }

    #[test]
    fn prefill_wait_survives_stop_until_a_restarted_query() {
        let (mut session, mut producer, generation) = full_horizon_waiting_prefill();
        session.transport().stop();
        let stopped = producer
            .step_unwatched_with_clock_and_cutover(
                &mut session,
                || panic!("Stop must not sample the scheduling clock"),
                48_000,
                |_, _, _| panic!("Stop published a generation"),
                |_| panic!("Stop emitted audio"),
            )
            .expect("stopped producer");
        assert_eq!(stopped.watch, WatchPoll::Stopped);
        assert_eq!(
            (stopped.scheduled, stopped.pushed, stopped.pending),
            (0, 0, 0)
        );
        assert!(producer.prefill_waiting_for_progress);
        assert_eq!(producer.pending_generation, Some(generation));

        session.transport().start();
        producer
            .step_unwatched_with_clock_and_cutover(
                &mut session,
                || 0.0,
                48_000,
                |_, _, _| panic!("resuming an unchanged horizon published silence"),
                |_| panic!("unchanged horizon emitted audio"),
            )
            .expect("unchanged resumed horizon");
        assert!(producer.prefill_waiting_for_progress);

        session.restart_transport_at(0.0);
        let mut published = Vec::new();
        producer
            .step_unwatched_with_clock_and_cutover(
                &mut session,
                || 0.0,
                48_000,
                |generation, frame, _cut| published.push((generation, frame)),
                |_| panic!("native silence emitted audio"),
            )
            .expect("fresh restarted query");
        assert_eq!(published, vec![(generation, 6_000)]);
        assert!(!producer.prefill_waiting_for_progress);
        assert!(!producer.generation_needs_prefill);
        assert!(!producer.check_takeover_freshness);
        assert!(producer.pending_generation.is_none());
        assert!(producer.pending_reload_event.is_none());
        assert_eq!(session.take_requery_takeover_time(), None);
    }

    #[test]
    fn replacement_and_recovery_arms_reset_prefill_wait() {
        let mut producer = LiveFileProducer::unwatched(Duration::from_millis(2)).expect("producer");
        assert!(!producer.prefill_waiting_for_progress);
        assert!(!producer.check_takeover_freshness);
        let arms: [fn(&mut LiveFileProducer, u64, u64); 3] = [
            LiveFileProducer::arm_replacement,
            LiveFileProducer::arm_control_requery,
            LiveFileProducer::arm_output_recovery_requery,
        ];
        for (index, arm) in arms.into_iter().enumerate() {
            let before = 7 + index as u64;
            producer.prefill_waiting_for_progress = true;
            arm(&mut producer, before, before + 1);
            assert!(!producer.prefill_waiting_for_progress);
            assert_eq!(producer.check_takeover_freshness, index == 0);
            assert!(producer.generation_needs_prefill);
            assert_eq!(producer.pending_generation, Some(before + 1));
        }
    }

    #[test]
    fn watched_install_resets_prefill_wait_before_a_prebake_stop() {
        let (mut session, mut producer, before) = full_horizon_waiting_prefill();
        let after = session.reload_at("~", true, 0.0).expect("new replacement");
        let installed = crate::ReloadEvent {
            path: PathBuf::from("<watched-mini>"),
            target: WatchTarget::Score(WatchLanguage::Mini),
            status: crate::ReloadStatus::Installed,
            generation_before: before,
            generation_after: after,
            error_kind: None,
            message: None,
        };
        let step = producer
            .finish_step(
                &mut session,
                WatchResults {
                    score: WatchPoll::Event(installed.clone()),
                    prebake: WatchPoll::Stopped,
                },
                || panic!("prebake Stop must precede scheduling"),
                48_000,
                |_, _, _| panic!("prebake Stop published a generation"),
                |_| panic!("prebake Stop emitted audio"),
            )
            .expect("installed identity with stopped prebake");
        assert_eq!(step.prebake_watch, WatchPoll::Stopped);
        assert!(!producer.prefill_waiting_for_progress);
        assert!(producer.generation_needs_prefill);
        assert_eq!(producer.pending_generation, Some(after));
        assert_eq!(producer.pending_reload_event, Some(installed));
    }

    /// A watched Installed save arriving while an earlier save's rollback is
    /// deferred supersedes that rollback: the save's own cutover publishes and it
    /// is never named the abandoned candidate.
    #[test]
    fn a_watched_save_supersedes_its_deferred_rollback_instead_of_being_discarded() {
        let mut session = Session::new().expect("session");
        session.set_continuity_margin(0.0);
        session.evaluate_mini("~").expect("audible score");
        // Inject replay policy, not consumer-copy evidence.
        session
            .mark_audible_generation(session.generation())
            .expect("injected replay-policy target");
        let audible = session.generation();
        let mut producer = LiveFileProducer::unwatched(Duration::from_millis(2)).expect("producer");
        producer
            .step_unwatched_with_clock_and_cutover(
                &mut session,
                || 0.0,
                48_000,
                |_, _, _| panic!("initial prefill must not cut over"),
                |_| true,
            )
            .expect("initial prefill");

        // The refused replacement is still the Session generation; only its
        // rollback could not run yet. `roll_back_refused_replacement`'s
        // Deferred arm leaves exactly this state behind for the next turn.
        let refused = session
            .reload_at("~", true, 0.0)
            .expect("refused replacement");
        producer.arm_replacement(audible, refused);
        producer.rollback_pending = true;

        // The artist saves again; the watch installs the new identity before
        // the turn reaches its rollback retry.
        let saved = session
            .reload_at("~ ~", true, 0.0)
            .expect("the new save installs");
        let installed = crate::ReloadEvent {
            path: PathBuf::from("<watched-mini>"),
            target: WatchTarget::Score(WatchLanguage::Mini),
            status: crate::ReloadStatus::Installed,
            generation_before: refused,
            generation_after: saved,
            error_kind: None,
            message: None,
        };
        let mut published = Vec::new();
        producer
            .finish_step(
                &mut session,
                WatchResults {
                    score: WatchPoll::Event(installed),
                    prebake: WatchPoll::Unchanged,
                },
                || 0.0,
                48_000,
                |generation, frame, _cut| published.push((generation, frame)),
                |_| panic!("a silent save emitted audio"),
            )
            .expect("the new save owns the boundary");
        assert_eq!(
            published
                .iter()
                .map(|(generation, _)| *generation)
                .collect::<Vec<_>>(),
            [saved],
            "the new save's cutover publishes, not the old score's"
        );
        assert_eq!(session.generation(), saved, "nothing rolled the save back");
        assert!(!producer.rollback_pending);
        assert!(producer.pending_generation.is_none());
        assert!(
            producer.take_abandoned_generation().is_none(),
            "the new save was not named as the abandoned candidate"
        );
    }

    #[test]
    fn continuation_outlier_is_retained_for_one_window_then_ages_out() {
        let ordinary = Duration::from_millis(2);
        let outlier = Duration::from_millis(200);
        let mut window = ContinuationWindow::default();
        window.record(outlier);
        for _ in 1..CONTINUATION_WINDOW_SAMPLES {
            window.record(ordinary);
        }
        assert_eq!(window.high_water(), Some(outlier));
        window.record(ordinary);
        assert_eq!(
            window.high_water(),
            Some(ordinary),
            "a single transient disabled watched setup beyond one sample window"
        );
    }

    #[test]
    fn live_query_reserve_uses_only_the_post_query_tail_and_exact_floor() {
        let mut producer = LiveFileProducer::new_with_prebake_floor(
            "unused.strudel",
            WatchLanguage::JavaScript,
            Duration::ZERO,
            Duration::from_millis(2),
            Duration::from_millis(1),
        )
        .expect("producer");

        producer
            .continuation_window
            .record(Duration::from_millis(200));
        producer.query_tail_window.record(Duration::from_millis(2));
        assert_eq!(
            producer.live_query_reserve(),
            MIN_LIVE_QUERY_JS_BUDGET,
            "the full prior query recursively priced the next query out"
        );

        producer.query_tail_window.record(Duration::from_millis(40));
        assert_eq!(producer.live_query_reserve(), Duration::from_millis(41));
    }

    #[test]
    fn producer_snapshot_attributes_progress_idle_callbacks_and_trace_work() {
        let mut session = Session::new().expect("session");
        session
            .evaluate("note('c4').fast(4).fmap(value => value)")
            .expect("score");
        let mut producer = LiveFileProducer::unwatched(Duration::from_millis(2)).expect("producer");
        let first = producer
            .step_unwatched_with_clock_and_cutover(
                &mut session,
                || 0.0,
                48_000,
                |_, _, _| {},
                |_| true,
            )
            .expect("first fill");
        assert!(first.scheduled > 0);
        let asset_preparation_before = producer
            .completed_turn
            .as_ref()
            .expect("staged producer turn")
            .phases_nanos[ProducerPhase::AssetPreparation as usize];
        producer.record_asset_preparation(Duration::from_nanos(2_000));
        producer.complete_turn(Duration::from_nanos(1_000));

        let progress = producer.producer_load_snapshot();
        assert_eq!(progress.progress_turns, 1);
        assert_eq!(progress.last_outcome, ProducerTurnOutcome::Progress);
        assert!(progress.query_span_millicycles > 0);
        assert!(progress.haps > 0);
        assert!(progress.scheduler_events > 0);
        assert!(progress.converted_audio_events > 0);
        assert!(progress.ring_pushes > 0);
        assert!(progress.js_callback_calls > 0);
        #[cfg(feature = "callback-census")]
        {
            assert_eq!(progress.js_callback_kind_samples, 1);
            assert_eq!(
                progress
                    .js_callback_kind_sample
                    .expect("first profiled query has a kind census")
                    .total(),
                progress.js_callback_calls
            );
        }
        #[cfg(not(feature = "callback-census"))]
        {
            assert_eq!(progress.js_callback_kind_samples, 0);
            assert!(progress.js_callback_kind_sample.is_none());
        }
        assert_eq!(progress.producer_backlog_depth, 0);
        assert_eq!(
            progress.producer_backlog_capacity,
            MAX_PENDING_BACKLOG as u32
        );
        assert_eq!(
            progress.scheduler_queue_capacity,
            rustel_scheduler::MAX_QUEUED_EVENTS as u32
        );
        assert_eq!(
            progress.scheduler_trace_capacity,
            rustel_scheduler::MAX_QUEUED_TRACE_EVENTS as u32
        );
        assert!(progress.phase(ProducerPhase::Query).last_nanos > 0);
        assert!(progress.phase(ProducerPhase::JsCallbacks).last_nanos > 0);
        assert_eq!(
            progress.phase(ProducerPhase::AssetPreparation).last_nanos,
            asset_preparation_before + 2_000
        );
        assert_eq!(progress.phase(ProducerPhase::Trace).last_nanos, 1_000);
        assert!(
            progress.phase(ProducerPhase::Total).last_nanos
                >= progress.phase(ProducerPhase::Query).last_nanos
        );

        producer
            .step_unwatched_with_clock_and_cutover(
                &mut session,
                || 0.0,
                48_000,
                |_, _, _| {},
                |_| true,
            )
            .expect("full-horizon turn");
        producer.complete_turn(Duration::ZERO);
        let idle = producer.producer_load_snapshot();
        assert_eq!(idle.progress_turns, 1);
        assert_eq!(idle.idle_turns, 1);
        assert_eq!(idle.last_outcome, ProducerTurnOutcome::Idle);
        assert_eq!(idle.last_load_basis_points, 0);
    }

    #[test]
    fn producer_snapshot_distinguishes_a_host_gap_from_compute_pressure() {
        let mut session = Session::new().expect("session");
        session.evaluate("s('sine').fast(4)").expect("score");
        let mut producer = LiveFileProducer::unwatched(Duration::from_millis(2)).expect("producer");
        producer
            .step_unwatched_with_clock(&mut session, || 0.0, 48_000, |_| true)
            .expect("initial fill");
        let _ = producer.producer_load_snapshot();

        producer
            .step_unwatched_with_clock(&mut session, || 2.0, 48_000, |_| true)
            .expect("gap recovery");
        let recovered = producer.producer_load_snapshot();
        assert_eq!(recovered.gap_resync_count, 1);
        assert_eq!(recovered.atomic_refusals, 0);
        assert_eq!(recovered.committed_refusals, 0);
        assert_eq!(recovered.last_outcome, ProducerTurnOutcome::Progress);
    }

    #[test]
    fn producer_snapshot_keeps_committed_refusal_distinct() {
        let mut session = Session::new().expect("session");
        session.evaluate("s('sine')").expect("audible score");
        let mut producer = LiveFileProducer::unwatched(Duration::from_millis(2)).expect("producer");
        producer
            .step_unwatched_with_clock_and_cutover(
                &mut session,
                || 0.0,
                48_000,
                |_, _, _| panic!("initial generation must not cut over"),
                |_| true,
            )
            .expect("initial fill");
        let _ = producer.producer_load_snapshot();

        let before = session.generation();
        let transport = session.transport();
        let mut clocks = [0.0, 0.0].into_iter();
        let after = session
            .reload_with_clock_cancellable(
                "pure('not-an-audio-note').fast(16)",
                false,
                transport.stopped_flag(),
                || clocks.next().expect("two replacement clocks"),
            )
            .expect("queryable candidate");
        producer.arm_replacement(before, after);
        producer
            .step_unwatched_with_clock_and_cutover(
                &mut session,
                || 0.0,
                48_000,
                |_, _, _| panic!("unrenderable candidate must not cut over"),
                |_| true,
            )
            .expect_err("conversion must refuse after scheduler commit");

        let refused = producer.producer_load_snapshot();
        assert_eq!(refused.last_outcome, ProducerTurnOutcome::CommittedRefusal);
        assert_eq!(refused.atomic_refusals, 0);
        assert_eq!(refused.committed_refusals, 1);
        assert!(refused.scheduler_events > 0);
        assert_eq!(refused.converted_audio_events, 0);
        assert!(refused.refused_voices > 0);
    }

    #[test]
    fn reload_shield_pushes_one_extra_cycle_of_the_audible_generation() {
        let mut session = Session::new().expect("session");
        session
            .evaluate("note('c4').fast(4).fmap(value => value)")
            .expect("score");
        let mut producer = LiveFileProducer::unwatched(Duration::from_millis(2)).expect("producer");
        let mut initial = Vec::new();
        producer
            .step_unwatched_with_clock_and_cutover(
                &mut session,
                || 0.0,
                48_000,
                |_, _, _| panic!("initial generation must not cut over"),
                |event| {
                    initial.push(event);
                    true
                },
            )
            .expect("initial prefill");

        let generation = session.generation();
        let mut shielded = Vec::new();
        assert!(
            producer
                .shield_reload_with_clock(
                    &mut session,
                    || 0.0,
                    48_000,
                    |event| {
                        shielded.push(event);
                        true
                    }
                )
                .expect("reload shield")
        );
        assert!(!shielded.is_empty(), "reload shield scheduled no audio");
        assert!(
            shielded.iter().all(|event| event.generation == generation),
            "reload shield crossed generations"
        );
        assert!(
            shielded
                .iter()
                .map(|event| event.target_frame)
                .max()
                .unwrap_or_default()
                >= 96_000,
            "reload shield did not extend the audible ring by one 120 BPM cycle"
        );
    }

    #[test]
    fn slow_direct_reload_keeps_old_cover_and_anchors_the_new_generation_at_completion() {
        let old = "note('c4').fast(16).fmap(value => value)";
        let new = "note('d4').fast(16).fmap(value => value)";
        let mut session = Session::new().expect("session");
        session.evaluate(old).expect("old score");
        let mut producer = LiveFileProducer::unwatched(Duration::from_millis(2)).expect("producer");
        producer
            .step_unwatched_with_clock_and_cutover(
                &mut session,
                || 0.0,
                48_000,
                |_, _, _| panic!("initial generation must not cut over"),
                |_| true,
            )
            .expect("initial prefill");

        let before = session.generation();
        let mut old_cover = Vec::new();
        producer
            .shield_reload_with_clock(
                &mut session,
                || 0.1,
                48_000,
                |event| {
                    old_cover.push(event);
                    true
                },
            )
            .expect("reload shield");
        assert!(
            old_cover
                .iter()
                .any(|event| event.target_frame >= (1.05 * 48_000.0) as u64),
            "old generation did not cover the simulated reload"
        );

        let stopped = session.transport();
        let mut clocks = [0.1, 0.97].into_iter();
        let after = session
            .reload_with_clock_cancellable(new, false, stopped.stopped_flag(), || {
                clocks.next().expect("two reload clocks")
            })
            .expect("replacement");
        producer.arm_replacement(before, after);

        let mut published = None;
        let mut replacement = Vec::new();
        producer
            .step_unwatched_with_clock_and_cutover(
                &mut session,
                || 0.97,
                48_000,
                |generation, frame, _cut| published = Some((generation, frame)),
                |event| {
                    replacement.push(event);
                    true
                },
            )
            .expect("replacement prefill");
        assert_eq!(published, Some((after, (1.05 * 48_000.0) as u64)));
        assert!(!replacement.is_empty(), "replacement scheduled no audio");
        assert!(
            session
                .take_diagnostics()
                .iter()
                .all(|diagnostic| diagnostic.kind != "live-recovered"),
            "reload duration was misreported as a producer gap"
        );
    }

    #[test]
    fn fast_replacement_keeps_enough_runway_for_the_next_producer_turn() {
        let old = "setCpm(900/4); note('c4').fmap(value => value)";
        let new = "setCpm(12000/4); note('d4').fmap(value => value)";
        let mut session = Session::new().expect("session");
        session.set_continuity_margin(0.0);
        session.evaluate(old).expect("old score");
        let mut producer = LiveFileProducer::unwatched(Duration::from_millis(2)).expect("producer");
        producer
            .step_unwatched_with_clock_and_cutover(
                &mut session,
                || 0.0,
                48_000,
                |_, _, _| panic!("initial generation must not cut over"),
                |_| true,
            )
            .expect("initial prefill");
        producer
            .shield_reload_with_clock(&mut session, || 0.0, 48_000, |_| true)
            .expect("reload shield");

        let before = session.generation();
        let stopped = session.transport();
        let mut clocks = [0.0, 0.0].into_iter();
        let after = session
            .reload_with_clock_cancellable(new, false, stopped.stopped_flag(), || {
                clocks.next().expect("two reload clocks")
            })
            .expect("replacement");
        producer.arm_replacement(before, after);

        let mut published = None;
        producer
            .step_unwatched_with_clock_and_cutover(
                &mut session,
                || 0.0,
                48_000,
                |generation, frame, _cut| published = Some((generation, frame)),
                |_| true,
            )
            .expect("replacement prefill");
        assert_eq!(published, Some((after, 3_840)));
        let replacement_metrics = producer.producer_load_snapshot();
        assert_eq!(
            replacement_metrics.last_outcome,
            ProducerTurnOutcome::Progress
        );
        assert!(
            replacement_metrics
                .phase(ProducerPhase::Evaluation)
                .last_nanos
                > 0
        );
        assert!(
            replacement_metrics
                .phase(ProducerPhase::ReplacementProbe)
                .last_nanos
                > 0
        );

        producer
            .step_unwatched_with_clock_and_cutover(
                &mut session,
                || 0.045,
                48_000,
                |_, _, _| panic!("published replacement must not cut over twice"),
                |_| true,
            )
            .expect("next producer turn");
        assert!(
            session
                .take_diagnostics()
                .iter()
                .all(|diagnostic| diagnostic.kind != "live-recovered"),
            "replacement published with less runway than one ordinary producer delay"
        );
    }

    #[test]
    fn repeated_unsustainable_fast_switches_keep_the_audible_producer_running() {
        let old = "setCpm(900/4); note('c4').fast(16).fmap(value => value)";
        let new = r#"
      setCps(50);
      note('d4').fmap(value => {
        const until = Date.now() + 40
        while (Date.now() < until) {}
        return value
      })
    "#;
        let mut session = Session::new().expect("session");
        session.set_continuity_margin(0.0);
        session.evaluate(old).expect("old score");
        let mut producer = LiveFileProducer::unwatched(Duration::from_millis(2)).expect("producer");
        producer
            .step_unwatched_with_clock_and_cutover(
                &mut session,
                || 0.0,
                48_000,
                |_, _, _| panic!("initial generation must not cut over"),
                |_| true,
            )
            .expect("initial prefill");
        producer
            .shield_reload_with_clock(&mut session, || 0.0, 48_000, |_| true)
            .expect("reload shield");

        let generation = session.generation();
        let stopped = session.transport();
        let mut clocks = [0.0, 0.0].into_iter();
        let error = session
            .reload_with_clock_cancellable(new, false, stopped.stopped_flag(), || {
                clocks.next().expect("two reload clocks")
            })
            .expect_err("unsustainable replacement must keep the audible score");
        assert!(
            error.to_string().contains("cannot keep up at 50.00 CPS"),
            "replacement refusal did not explain its real-time limit: {error}"
        );
        assert_eq!(session.generation(), generation);

        let mut continued = Vec::new();
        let step = producer
            .step_unwatched_with_clock_and_cutover(
                &mut session,
                || 0.4,
                48_000,
                |_, _, _| panic!("rejected replacement changed device generation"),
                |event| {
                    continued.push(event);
                    true
                },
            )
            .expect("old producer continuation");
        assert!(step.scheduled > 0);
        assert!(
            continued.iter().all(|event| event.generation == generation),
            "rejected replacement leaked candidate audio"
        );
        let rejection_metrics = producer.producer_load_snapshot();
        assert_eq!(
            rejection_metrics.last_outcome,
            ProducerTurnOutcome::Progress
        );
        assert_eq!(rejection_metrics.atomic_refusals, 1);
        assert_eq!(rejection_metrics.committed_refusals, 0);
        assert!(
            session
                .take_diagnostics()
                .iter()
                .all(|diagnostic| diagnostic.kind != "live-recovered"),
            "rejecting an unsustainable replacement starved the old score"
        );

        producer
            .shield_reload_with_clock(&mut session, || 0.4, 48_000, |_| true)
            .expect("second reload shield");
        let sustainable = "setCpm(900/4); note('e4').fast(16).fmap(value => value)";
        let before = session.generation();
        let mut clocks = [0.4, 0.4].into_iter();
        let after = session
            .reload_with_clock_cancellable(sustainable, false, stopped.stopped_flag(), || {
                clocks.next().expect("two sustainable clocks")
            })
            .expect("sustainable replacement");
        producer.arm_replacement(before, after);
        let mut published = None;
        let mut replacement = Vec::new();
        producer
            .step_unwatched_with_clock_and_cutover(
                &mut session,
                || 0.4,
                48_000,
                |generation, frame, _cut| published = Some((generation, frame)),
                |event| {
                    replacement.push(event);
                    true
                },
            )
            .expect("sustainable replacement prefill");
        assert_eq!(published, Some((after, (0.48 * 48_000.0) as u64)));
        assert!(
            replacement.iter().all(|event| event.generation == after),
            "sustainable cutover leaked an older generation"
        );

        producer
            .shield_reload_with_clock(&mut session, || 0.5, 48_000, |_| true)
            .expect("third reload shield");
        let mut clocks = [0.5, 0.5].into_iter();
        let error = session
            .reload_with_clock_cancellable(new, false, stopped.stopped_flag(), || {
                clocks.next().expect("two repeated refusal clocks")
            })
            .expect_err("repeated unsustainable replacement must be refused");
        assert!(error.to_string().contains("cannot keep up at 50.00 CPS"));
        assert_eq!(session.generation(), after);

        let mut resumed = Vec::new();
        let step = producer
            .step_unwatched_with_clock_and_cutover(
                &mut session,
                || 0.9,
                48_000,
                |_, _, _| panic!("repeated refusal changed device generation"),
                |event| {
                    resumed.push(event);
                    true
                },
            )
            .expect("latest audible producer continuation");
        assert!(step.scheduled > 0);
        assert!(resumed.iter().all(|event| event.generation == after));
        assert!(
            session
                .take_diagnostics()
                .iter()
                .all(|diagnostic| diagnostic.kind != "live-recovered"),
            "repeated refusal starved the latest audible score"
        );
    }

    #[test]
    fn a_syntax_error_on_live_reload_is_not_counted_as_a_producer_refusal() {
        let mut session = Session::new().expect("session");
        session.evaluate("note('c4')").expect("audible score");
        let mut producer = LiveFileProducer::unwatched(Duration::from_millis(2)).expect("producer");
        producer
            .step_unwatched_with_clock(&mut session, || 0.0, 48_000, |_| true)
            .expect("initial fill");
        let _ = producer.producer_load_snapshot();

        let generation = session.generation();
        let stopped = session.transport();
        let error = session
            .reload_with_clock_cancellable("note(", false, stopped.stopped_flag(), || 0.0)
            .expect_err("invalid syntax");
        assert_eq!(error.kind(), "evaluation");
        assert_eq!(session.generation(), generation);

        producer
            .step_unwatched_with_clock(&mut session, || 0.4, 48_000, |_| true)
            .expect("old score continues");
        let snapshot = producer.producer_load_snapshot();
        assert_eq!(snapshot.atomic_refusals, 0);
        assert_eq!(snapshot.committed_refusals, 0);
    }

    #[test]
    fn a_candidate_that_slows_after_its_probe_rolls_back_before_cutover() {
        let old = "setCps(0.5); note('c4').fast(16).fmap(value => value)";
        let candidate = r#"
      setCps(10);
      (() => {
        let calls = 0
        return note('d4').fmap(value => {
          calls += 1
          if (calls > 1) while (true) {}
          return value
        })
      })()
    "#;
        let mut session = Session::new().expect("session");
        session.set_continuity_margin(0.0);
        session.evaluate(old).expect("old score");
        // Inject replay policy, not consumer-copy evidence.
        session
            .mark_audible_generation(session.generation())
            .expect("injected replay-policy target");
        let mut producer = LiveFileProducer::unwatched(Duration::from_millis(2)).expect("producer");
        producer
            .step_unwatched_with_clock_and_cutover(
                &mut session,
                || 0.0,
                48_000,
                |_, _, _| panic!("initial generation must not cut over"),
                |_| true,
            )
            .expect("initial prefill");
        producer
            .shield_reload_with_clock(&mut session, || 0.0, 48_000, |_| true)
            .expect("reload shield");

        let audible = session.generation();
        let transport = session.transport();
        let mut clocks = [0.0, 0.0].into_iter();
        let candidate_generation = session
            .reload_with_clock_cancellable(candidate, false, transport.stopped_flag(), || {
                clocks.next().expect("two candidate clocks")
            })
            .expect("the cheap first probe should stage the candidate");
        producer.arm_replacement(audible, candidate_generation);

        let error = producer
            .step_unwatched_with_clock_and_cutover(
                &mut session,
                || 0.0,
                48_000,
                |_, _, _| panic!("failed candidate reached device cutover"),
                |_| true,
            )
            .expect_err("the candidate's second query must hit its deadline");
        assert!(error.to_string().contains("CPU deadline"));
        let rollback_generation = session.generation();
        assert_eq!(rollback_generation, candidate_generation + 1);
        assert_eq!(producer.pending_generation, Some(rollback_generation));

        let mut published = None;
        let mut restored = Vec::new();
        producer
            .step_unwatched_with_clock_and_cutover(
                &mut session,
                || 0.0,
                48_000,
                |generation, frame, _cut| published = Some((generation, frame)),
                |event| {
                    restored.push(event);
                    true
                },
            )
            .expect("rollback prefill");
        assert_eq!(published, Some((rollback_generation, 3_840)));
        assert!(
            restored
                .iter()
                .all(|event| event.generation == rollback_generation)
        );
    }

    /// A score whose every sound is unknown, forced past the check, does not
    /// latch the producer. The last audible score comes back.
    #[test]
    fn a_replacement_that_refuses_every_onset_rolls_back_to_the_audible_score() {
        // Dense at a fast tempo, so the harness's few-millisecond first
        // window holds onsets on both sides of the switch.
        let old = "setCps(10); note('c4').fast(64)";
        let candidate = "setCps(10); s(\"sdb\").seg(64)";
        let mut session = Session::new().expect("session");
        session.set_continuity_margin(0.0);
        session.evaluate(old).expect("old score");
        // Inject replay policy, not consumer-copy evidence.
        session
            .mark_audible_generation(session.generation())
            .expect("injected replay-policy target");
        let mut producer = LiveFileProducer::unwatched(Duration::from_millis(2)).expect("producer");
        producer
            .step_unwatched_with_clock_and_cutover(
                &mut session,
                || 0.0,
                48_000,
                |_, _, _| panic!("initial generation must not cut over"),
                |_| true,
            )
            .expect("initial prefill");
        producer
            .shield_reload_with_clock(&mut session, || 0.0, 48_000, |_| true)
            .expect("reload shield");

        let audible = session.generation();
        let transport = session.transport();
        let candidate_generation = session
            .reload_with_clock_cancellable(candidate, false, transport.stopped_flag(), || 0.0)
            .expect("an unknown sound is not a JavaScript error");
        producer.arm_replacement(audible, candidate_generation);

        let error = producer
            .step_unwatched_with_clock_and_cutover(
                &mut session,
                || 0.0,
                48_000,
                |_, _, _| panic!("a refused candidate reached device cutover"),
                |_| true,
            )
            .expect_err("every onset is refused");
        assert!(
            error.to_string().contains("keeps playing"),
            "the error says what happened: {error}"
        );
        assert!(!producer.prefill_candidate_committed, "no latch");
        let rollback_generation = session.generation();
        assert_eq!(rollback_generation, candidate_generation + 1);
        assert_eq!(producer.pending_generation, Some(rollback_generation));
        assert_eq!(
            producer.take_abandoned_generation(),
            Some(candidate_generation),
            "the refused candidate is named once"
        );
        assert_eq!(producer.take_abandoned_generation(), None);

        let mut published = None;
        let mut restored = Vec::new();
        producer
            .step_unwatched_with_clock_and_cutover(
                &mut session,
                || 0.0,
                48_000,
                |generation, frame, _cut| published = Some((generation, frame)),
                |event| {
                    restored.push(event);
                    true
                },
            )
            .expect("the rollback prefill plays");
        assert_eq!(
            published.map(|(generation, _)| generation),
            Some(rollback_generation)
        );
        assert!(!restored.is_empty(), "the old notes are back");
        assert!(
            restored
                .iter()
                .all(|event| event.generation == rollback_generation)
        );
    }

    /// A rollback target that refuses every onset too must not become a
    /// rollback loop - one generation per turn, forever. The second
    /// refusal latches with a message that says what to do.
    #[test]
    fn a_rollback_that_refuses_too_latches_instead_of_looping() {
        // A missing sound evaluates successfully but every onset is refused.
        // Inject this unplayable replay-policy target to test loop prevention
        // without claiming consumer-copy evidence.
        let bad = "setCps(10); s('definitely_missing')";
        let mut session = Session::new().expect("session");
        session.set_continuity_margin(0.0);
        session.evaluate(bad).expect("evaluates");
        session
            .mark_audible_generation(session.generation())
            .expect("injected replay-policy target");
        let mut producer = LiveFileProducer::unwatched(Duration::from_millis(2)).expect("producer");
        producer
            .step_unwatched_with_clock_and_cutover(
                &mut session,
                || 0.0,
                48_000,
                |_, _, _| {},
                |_| true,
            )
            .expect("initial prefill");
        producer
            .shield_reload_with_clock(&mut session, || 0.0, 48_000, |_| true)
            .expect("reload shield");
        let audible = session.generation();
        let transport = session.transport();
        let candidate = session
            .reload_with_clock_cancellable(bad, false, transport.stopped_flag(), || 0.0)
            .expect("still evaluates");
        producer.arm_replacement(audible, candidate);
        let mut errors = Vec::new();
        for _ in 0..6 {
            if let Err(error) = producer.step_unwatched_with_clock_and_cutover(
                &mut session,
                || 0.0,
                48_000,
                |_, _, _| {},
                |_| true,
            ) {
                errors.push(error.to_string());
            }
        }
        assert!(
            session.generation() <= candidate + 1,
            "one rollback, then the latch: generation {} after candidate {candidate}",
            session.generation()
        );
        assert!(
            errors
                .iter()
                .any(|error| error.contains("could not be played either")),
            "{errors:?}"
        );
        assert_eq!(
            producer.take_abandoned_generation(),
            Some(session.generation()),
            "the latched rollback never publishes either"
        );
    }

    /// With no audible score to go back to, the refused candidate still
    /// never publishes, and the producer says so: a launch fired on it must
    /// give its line cut back like any other.
    #[test]
    fn a_refused_replacement_with_nothing_to_roll_back_to_is_abandoned() {
        let mut session = Session::new().expect("session");
        session.set_continuity_margin(0.0);
        session
            .evaluate("setCps(10); note('c4').fast(64)")
            .expect("old score");
        let mut producer = LiveFileProducer::unwatched(Duration::from_millis(2)).expect("producer");
        producer
            .step_unwatched_with_clock_and_cutover(
                &mut session,
                || 0.0,
                48_000,
                |_, _, _| panic!("initial generation must not cut over"),
                |_| true,
            )
            .expect("initial prefill");
        producer
            .shield_reload_with_clock(&mut session, || 0.0, 48_000, |_| true)
            .expect("reload shield");
        let audible = session.generation();
        let transport = session.transport();
        let candidate = session
            .reload_with_clock_cancellable(
                "setCps(10); s(\"sdb\").seg(64)",
                false,
                transport.stopped_flag(),
                || 0.0,
            )
            .expect("an unknown sound is not a JavaScript error");
        producer.arm_replacement(audible, candidate);
        assert_eq!(producer.take_abandoned_generation(), None);

        let error = producer
            .step_unwatched_with_clock_and_cutover(
                &mut session,
                || 0.0,
                48_000,
                |_, _, _| panic!("a refused candidate reached device cutover"),
                |_| true,
            )
            .expect_err("every onset is refused");
        assert!(
            error.to_string().contains("no earlier audible score"),
            "{error}"
        );
        assert_eq!(producer.take_abandoned_generation(), Some(candidate));
    }

    #[test]
    fn a_900_bpm_replacement_keeps_its_takeover_ahead_of_prefill() {
        let mut session = Session::new().expect("session");
        session.set_continuity_margin(0.0);
        session
            .evaluate("setCpm(100/4); note('c4').fast(4).fmap(value => value)")
            .expect("old score");
        let mut producer = LiveFileProducer::unwatched(Duration::from_millis(2)).expect("producer");
        producer
            .step_unwatched_with_clock_and_cutover(
                &mut session,
                || 0.0,
                48_000,
                |_, _, _| panic!("initial generation must not cut over"),
                |_| true,
            )
            .expect("initial prefill");

        let generation_before = session.generation();
        let transport = session.transport();
        let mut install_clocks = [10.0, 10.0].into_iter();
        let generation_after = session
            .reload_with_clock_cancellable(
                r#"
                setCpm(900/4)
                note('d4').fmap(value => {
                    const until = Date.now() + 70
                    while (Date.now() < until) {}
                    return value
                })
            "#,
                false,
                transport.stopped_flag(),
                || install_clocks.next().expect("two replacement clocks"),
            )
            .expect("900 BPM replacement");
        producer.arm_replacement(generation_before, generation_after);

        // Model a ~160 ms first scheduling turn. The 70 ms callback above
        // crosses the adaptive threshold. The synthetic device clock below
        // keeps the takeover assertion independent of test-runner scheduling.
        // The tempo-aware contract keeps one bounded 900-BPM query cycle
        // ahead of the producer clock.
        let producer_now = 10.160;
        let mut published = None;
        producer
            .step_unwatched_with_clock_and_cutover(
                &mut session,
                || producer_now,
                48_000,
                |generation, frame, _cut| published = Some((generation, frame)),
                |_| true,
            )
            .expect("replacement prefill");

        let expected_takeover = ((10.0 + 1.0 / 3.75) * 48_000.0_f64).round() as u64;
        assert_eq!(published, Some((generation_after, expected_takeover)));
        assert!(
            expected_takeover > (producer_now * 48_000.0_f64).round() as u64,
            "the accepted replacement published a takeover in the past"
        );
    }

    #[test]
    fn control_requery_arms_cutover_without_faking_a_source_reload() {
        let mut producer = LiveFileProducer::new(
            "unused.strudel",
            WatchLanguage::JavaScript,
            Duration::ZERO,
            Duration::from_millis(2),
        )
        .expect("producer");

        producer.arm_control_requery(7, 8);
        assert_eq!(producer.pending_generation, Some(8));
        assert!(producer.pending_reload_event.is_some());
        assert!(!producer.pending_reload_visible);
        assert!(!producer.check_takeover_freshness);
        assert!(producer.generation_needs_prefill);
        assert!(producer.unwatched_cutover_allowed);
    }

    #[test]
    fn only_unpublished_source_replacements_roll_back_on_the_first_refusal() {
        let mut producer = LiveFileProducer::new(
            "unused.strudel",
            WatchLanguage::JavaScript,
            Duration::ZERO,
            Duration::from_millis(2),
        )
        .expect("producer");

        producer.arm_replacement(7, 8);
        assert!(producer.check_takeover_freshness);
        assert_eq!(producer.retryable_rollback_after(), Some(Duration::ZERO));

        producer.arm_control_requery(8, 9);
        assert!(!producer.check_takeover_freshness);
        assert_eq!(producer.retryable_rollback_after(), None);

        producer.arm_output_recovery_requery(9, 10);
        assert!(!producer.check_takeover_freshness);
        assert_eq!(
            producer.retryable_rollback_after(),
            Some(STARVED_BEFORE_ROLLBACK)
        );
    }

    fn queued_at(target_frame: u64) -> QueuedAudioEvent {
        rustel_audio::AudioEvent {
            onset_id: target_frame,
            generation: 7,
            target_frame,
            onset_lead: 0.0,
            freq_hz: 440.0,
            gain: 0.5,
            duration_secs: 0.1,
            ui_visuals: 0,
            controls: Default::default(),
            sample: None,
            wavetable: None,
            synth: None,
            cut: None,
        }
        .into()
    }

    /// The frontier of everything ever scheduled: one past the latest
    /// target, and never backwards - a later, earlier batch does not
    /// shorten what an evicted sample has to outlive.
    #[test]
    fn scheduled_through_frame_is_one_past_the_latest_target_seen() {
        let mut producer = LiveFileProducer::new(
            "unused.strudel",
            WatchLanguage::JavaScript,
            Duration::ZERO,
            Duration::from_millis(2),
        )
        .expect("producer");
        assert_eq!(producer.scheduled_through_frame(), 0);
        producer.note_scheduled_through([48_000u64, 96_000].into_iter());
        assert_eq!(producer.scheduled_through_frame(), 96_001);
        producer.note_scheduled_through([10u64].into_iter());
        assert_eq!(
            producer.scheduled_through_frame(),
            96_001,
            "never backwards"
        );
        producer.note_scheduled_through(std::iter::empty());
        assert_eq!(
            producer.scheduled_through_frame(),
            96_001,
            "an empty batch is nothing"
        );
        producer.note_scheduled_through([u64::MAX].into_iter());
        assert_eq!(producer.scheduled_through_frame(), u64::MAX, "saturates");
    }

    /// The earliest target still waiting in the backlog, whatever order it
    /// was queued in; none once the backlog is drained.
    #[test]
    fn pending_target_floor_is_the_earliest_backlogged_target_or_none() {
        let mut producer = LiveFileProducer::new(
            "unused.strudel",
            WatchLanguage::JavaScript,
            Duration::ZERO,
            Duration::from_millis(2),
        )
        .expect("producer");
        assert_eq!(producer.pending_target_floor(), None);
        producer.pending.push_back(queued_at(96_000));
        producer.pending.push_back(queued_at(48_000));
        assert_eq!(producer.pending_target_floor(), Some(48_000));
        producer.pending.pop_front();
        assert_eq!(producer.pending_target_floor(), Some(48_000));
        producer.pending.clear();
        assert_eq!(producer.pending_target_floor(), None);
    }

    #[test]
    fn backpressure_retains_pending_sample_ids_until_replacement() {
        let mut session = Session::new().expect("session");
        session.evaluate_mini("~").expect("silent score");
        session.restart_transport_at(0.0);
        let mut producer = LiveFileProducer::unwatched(Duration::from_millis(2)).expect("producer");
        assert_eq!(producer.pending_sample_ids().count(), 0);
        let samples = [rustel_audio::SampleId(3), rustel_audio::SampleId(5)];
        for sample in samples {
            let mut queued = queued_at(48_000);
            queued.event.generation = session.generation();
            queued.event.sample = Some(rustel_audio::SampleControls {
                sample,
                playback_rate: 1.0,
                begin: 0.0,
                end: 1.0,
                hold: rustel_audio::SampleHold::Slice,
                muted: false,
                loop_secs: None,
                envelope_peak: 1.0,
                reversed: false,
                nudge_secs: 0.0,
                cut: None,
            });
            producer.pending.push_back(queued);
        }

        let first = producer
            .step_unwatched_with_clock(&mut session, || 0.0, 48_000, |_| false)
            .expect("backpressured batch");
        assert!(first.backpressured, "{first:?}");
        assert_eq!(first.pending, samples.len());
        assert_eq!(first.pushed, 0);
        let pending = producer.pending_sample_ids().collect::<Vec<_>>();
        assert_eq!(pending, samples);

        let second = producer
            .step_unwatched_with_clock(&mut session, || 0.0, 48_000, |_| false)
            .expect("backpressured retry");
        assert!(second.backpressured);
        assert_eq!(producer.pending_sample_ids().collect::<Vec<_>>(), pending);

        let generation = session.generation();
        producer.arm_replacement(generation, generation + 1);
        assert_eq!(producer.pending_sample_ids().count(), 0);
    }

    #[test]
    fn output_recovery_discards_old_rate_pending_and_stays_invisible() {
        let mut producer = LiveFileProducer::new(
            "unused.strudel",
            WatchLanguage::JavaScript,
            Duration::ZERO,
            Duration::from_millis(2),
        )
        .expect("producer");
        producer.pending.push_back(
            rustel_audio::AudioEvent {
                onset_id: 1,
                generation: 7,
                target_frame: 48_000,
                onset_lead: 0.0,
                freq_hz: 440.0,
                gain: 0.5,
                duration_secs: 0.1,
                ui_visuals: 0,
                controls: Default::default(),
                sample: None,
                wavetable: None,
                synth: None,
                cut: None,
            }
            .into(),
        );

        producer.arm_output_recovery_requery(7, 8);
        assert!(
            producer.pending.is_empty(),
            "old-rate event survived recycle"
        );
        assert_eq!(producer.pending_generation, Some(8));
        assert!(producer.generation_needs_prefill);
        assert!(!producer.prefill_candidate_committed);
        assert!(producer.unwatched_cutover_allowed);
        assert!(!producer.pending_reload_visible);
        assert!(!producer.check_takeover_freshness);
        assert!(
            !producer.control_requery_pending,
            "audio recovery disabled the starvation watchdog"
        );
        assert_eq!(
            producer
                .pending_reload_event
                .as_ref()
                .expect("recovery event")
                .path,
            PathBuf::from("<audio-recycle>")
        );
    }

    #[test]
    fn output_recovery_failure_can_reach_the_starvation_watchdog() {
        let mut producer = LiveFileProducer::new(
            "unused.strudel",
            WatchLanguage::JavaScript,
            Duration::ZERO,
            Duration::from_millis(2),
        )
        .expect("producer");
        producer.arm_output_recovery_requery(7, 8);
        producer.starving_since = Some(Instant::now() - STARVED_BEFORE_ROLLBACK);
        let starving_for = producer.retryable_starving_for();
        assert!(starving_for >= STARVED_BEFORE_ROLLBACK);
    }

    #[test]
    fn output_recovery_rearms_watchdog_after_a_pending_control_requery() {
        let mut producer = LiveFileProducer::new(
            "unused.strudel",
            WatchLanguage::JavaScript,
            Duration::ZERO,
            Duration::from_millis(2),
        )
        .expect("producer");
        producer.arm_control_requery(7, 8);
        assert!(producer.control_requery_pending);
        assert!(!producer.pending_reload_visible);

        producer.arm_output_recovery_requery(8, 9);
        assert!(
            !producer.control_requery_pending,
            "the pending control transaction disabled recovery rollback"
        );
        assert!(!producer.pending_reload_visible);
        assert_eq!(
            producer
                .pending_reload_event
                .as_ref()
                .expect("retained control event")
                .generation_after,
            9
        );

        producer.starving_since = Some(Instant::now() - STARVED_BEFORE_ROLLBACK);
        assert!(producer.retryable_starving_for() >= STARVED_BEFORE_ROLLBACK);
    }

    #[test]
    fn output_recovery_preserves_an_already_visible_source_reload() {
        let mut producer = LiveFileProducer::new(
            "unused.strudel",
            WatchLanguage::JavaScript,
            Duration::ZERO,
            Duration::from_millis(2),
        )
        .expect("producer");
        producer.arm_replacement(7, 8);
        producer.arm_output_recovery_requery(8, 9);

        assert!(producer.pending_reload_visible);
        let event = producer
            .pending_reload_event
            .as_ref()
            .expect("reload event");
        assert_eq!(event.generation_before, 7);
        assert_eq!(event.generation_after, 9);
        assert_eq!(producer.pending_generation, Some(9));
    }

    /// A 1-cps session whose sample library holds `heldsample` Loading until
    /// the test finishes it, with a silent score already prefilled and no
    /// schedule lead: `(session, producer, library)`.
    fn held_sample_session() -> (
        Session,
        LiveFileProducer,
        std::sync::Arc<crate::samples::SampleLibrary>,
    ) {
        let library = std::sync::Arc::new(
            crate::samples::SampleLibrary::with_loading_sample_for_test("heldsample"),
        );
        let mut session = Session::with_config(crate::SessionConfig {
            cps: 1.0,
            horizon: 0.5,
            ..Default::default()
        })
        .expect("session");
        session.set_sample_library_for_test(std::sync::Arc::clone(&library));
        session.set_direct_diagnostic_logging(false);
        session.set_schedule_lead(0.0);
        session.set_continuity_margin(0.0);
        session.evaluate_mini("~").expect("initial silent source");
        session.restart_transport_at(0.0);
        let mut producer = LiveFileProducer::unwatched(Duration::from_millis(2)).expect("producer");
        producer
            .step_unwatched_with_clock_and_cutover(
                &mut session,
                || 0.0,
                48_000,
                |_, _, _| panic!("initial prefill must not publish a replacement"),
                |_| panic!("initial rest emitted audio"),
            )
            .expect("initial silent prefill");
        (session, producer, library)
    }

    /// A watched save's sounds start loading while the save still debounces,
    /// before the turn that installs it and queries its first window.
    /// Otherwise the first onset starts the decode and is dropped.
    #[test]
    fn a_watched_save_warms_its_sounds_before_the_turn_that_installs_it() {
        let (mut session, _unwatched, _library) = held_sample_session();
        let directory = tempfile::tempdir().expect("tempdir");
        let path = directory.path().join("score.strudel");
        std::fs::write(&path, "~").expect("write baseline");
        let mut producer = LiveFileProducer::new(
            &path,
            WatchLanguage::Mini,
            Duration::from_millis(100),
            Duration::from_millis(2),
        )
        .expect("watched producer");
        // Whatever start-up asked for is not what this test is measuring.
        let _ = session.wait_for_sample_loads(Duration::ZERO);

        std::fs::write(&path, r#"s("heldsample*4")"#).expect("save");
        let step = producer
            .step_with_clock(
                &mut session,
                Duration::ZERO,
                || 0.0,
                48_000,
                |_, _, _| panic!("a debouncing save published"),
                |_| panic!("a debouncing save emitted audio"),
            )
            .expect("the save is seen, not yet installed");
        assert_eq!(
            step.watch,
            WatchPoll::Pending,
            "the save is still debouncing: nothing is installed yet"
        );
        let (requested, _) = session.wait_for_sample_loads(Duration::ZERO);
        assert!(
            requested > 0,
            "the save's sounds were asked for while it debounced"
        );
    }

    /// Install `s("heldsample*4")` natively over the running transport.
    fn install_held_sample(session: &mut Session) -> (u64, u64) {
        let before = session.generation();
        let values = rustel_mini::mini("heldsample*4").expect("native mini values");
        session
            .set_pattern(rustel_core::controls::ControlSpec::new(["s"]).pattern(&values))
            .expect("native sound replacement");
        session.restart_transport_at(0.0);
        (before, session.generation())
    }

    fn score_event(status: crate::ReloadStatus, before: u64, after: u64) -> crate::ReloadEvent {
        crate::ReloadEvent {
            path: PathBuf::from("<watched-mini>"),
            target: WatchTarget::Score(WatchLanguage::Mini),
            status,
            generation_before: before,
            generation_after: after,
            error_kind: None,
            message: None,
        }
    }

    /// A watched save whose first window waits for a loading sample reaches
    /// the host as Installed ONCE, when its generation publishes. The held
    /// turn used to hand back the raw Installed event while the cutover kept
    /// its copy: the command line counted two installs, recorded two saves,
    /// and announced the score before the old generation stopped sounding.
    #[test]
    fn a_save_held_for_loading_is_announced_once_at_publication() {
        let (mut session, mut producer, library) = held_sample_session();
        let (before, after) = install_held_sample(&mut session);
        let installed = score_event(crate::ReloadStatus::Installed, before, after);
        let mut announced = 0;
        let mut published = Vec::new();
        let count = |step: &LiveProducerStep| {
            usize::from(matches!(&step.watch, WatchPoll::Event(event) if *event == installed))
        };

        let held = producer
            .finish_step(
                &mut session,
                WatchResults {
                    score: WatchPoll::Event(installed.clone()),
                    prebake: WatchPoll::Unchanged,
                },
                || 0.0,
                48_000,
                |generation, frame, cut| published.push((generation, frame, cut)),
                |_| panic!("a loading window emitted audio"),
            )
            .expect("a loading hold is a quiet turn");
        assert_eq!((held.scheduled, held.pushed), (0, 0));
        assert_eq!(held.watch, WatchPoll::Unchanged, "not announced while held");
        announced += count(&held);
        // The clock stands still: the horizon is full, so these turns wait
        // for progress. (A later clock whose window has no onset would
        // publish the edit without its loading hits, by the edit's own
        // skip-and-log contract; that is not what this test is about.)
        for _ in 0..3 {
            let held = producer
                .finish_step(
                    &mut session,
                    WatchResults {
                        score: WatchPoll::Unchanged,
                        prebake: WatchPoll::Unchanged,
                    },
                    || 0.0,
                    48_000,
                    |generation, frame, cut| published.push((generation, frame, cut)),
                    |_| panic!("a loading window emitted audio"),
                )
                .expect("still held");
            announced += count(&held);
        }
        assert!(
            published.is_empty(),
            "published while loading: {published:?}"
        );
        assert_eq!(announced, 0);

        library.finish_loading_sample_for_test();
        let ready = producer
            .finish_step(
                &mut session,
                WatchResults {
                    score: WatchPoll::Unchanged,
                    prebake: WatchPoll::Unchanged,
                },
                || 0.5,
                48_000,
                |generation, frame, cut| published.push((generation, frame, cut)),
                |_| true,
            )
            .expect("the save publishes once its sample is ready");
        assert!(ready.scheduled > 0, "{ready:?}");
        announced += count(&ready);
        assert_eq!(published.len(), 1, "{published:?}");
        assert_eq!(published[0].0, after);
        assert_eq!(announced, 1, "one install, announced at its publication");
    }

    /// A rejected save that arrives on a steady turn whose window is still
    /// loading is delivered once, not once on the held turn and again from
    /// its retained report on the next.
    #[test]
    fn a_rejection_on_a_loading_turn_is_delivered_once() {
        let (mut session, _, _library) = held_sample_session();
        let (_, after) = install_held_sample(&mut session);
        let mut producer = LiveFileProducer::unwatched(Duration::from_millis(2)).expect("producer");
        producer.generation_needs_prefill = false;
        let rejected = crate::ReloadEvent {
            error_kind: Some("parse-error".into()),
            message: Some("rejected save".into()),
            ..score_event(crate::ReloadStatus::Rejected, after, after)
        };
        let mut delivered = 0;
        for score in [WatchPoll::Event(rejected.clone()), WatchPoll::Unchanged] {
            let step = producer
                .finish_step(
                    &mut session,
                    WatchResults {
                        score,
                        prebake: WatchPoll::Unchanged,
                    },
                    || 0.0,
                    48_000,
                    |_, _, _| panic!("a steady turn published a generation"),
                    |_| panic!("a loading window emitted audio"),
                )
                .expect("a steady loading turn is quiet");
            delivered += usize::from(step.watch == WatchPoll::Event(rejected.clone()));
        }
        assert_eq!(delivered, 1, "the rejection is delivered once");
        assert!(producer.pending_score_report.is_none());
    }

    /// A stretch of windows waiting for samples is not starvation. A stamp
    /// left by an earlier atomic refusal must not keep counting through a
    /// loading hold, or the next refusal after it rolls the set back.
    #[test]
    fn a_loading_hold_resets_the_starvation_watchdog() {
        let (mut session, _, _library) = held_sample_session();
        let (_, after) = install_held_sample(&mut session);
        let mut producer = LiveFileProducer::unwatched(Duration::from_millis(2)).expect("producer");
        producer.generation_needs_prefill = false;
        producer.starving_since = Some(Instant::now() - STARVED_BEFORE_ROLLBACK);
        let step = producer
            .step_unwatched_with_clock_and_cutover(
                &mut session,
                || 0.0,
                48_000,
                |_, _, _| panic!("a steady turn published a generation"),
                |_| panic!("a loading window emitted audio"),
            )
            .expect("a loading window is a held turn");
        assert_eq!((step.scheduled, step.pushed), (0, 0));
        assert!(
            producer.starving_since.is_none(),
            "the loading hold left the watchdog counting"
        );
        assert_eq!(session.generation(), after, "nothing rolled back");
    }

    #[test]
    fn unwatched_producer_uses_an_in_memory_identity_and_validates_its_floor() {
        let producer =
            LiveFileProducer::unwatched(Duration::from_millis(2)).expect("unwatched producer");
        assert_eq!(producer.path(), std::path::Path::new("<studio>"));
        assert!(LiveFileProducer::unwatched(Duration::ZERO).is_err());
    }

    #[test]
    fn a_from_zero_replacement_publishes_its_cut_with_the_flip() {
        let make = |from_zero: bool| {
            let mut session = Session::new().expect("session");
            session.set_continuity_margin(0.0);
            session
                .evaluate("setcps(1); note('c4')")
                .expect("initial score");
            let mut producer =
                LiveFileProducer::unwatched(Duration::from_millis(2)).expect("producer");
            producer
                .step_unwatched_with_clock_and_cutover(
                    &mut session,
                    || 0.0,
                    48_000,
                    |_, _, _| panic!("initial generation must not cut over"),
                    |_| true,
                )
                .expect("initial prefill");

            let generation_before = session.generation();
            let transport = session.transport();
            if from_zero {
                session.start_next_from_zero();
            }
            let generation_after = session
                .reload_with_clock_cancellable(
                    "setcps(1); note('d4')",
                    false,
                    transport.stopped_flag(),
                    || 1.0,
                )
                .expect("replacement");
            producer.arm_replacement(generation_before, generation_after);

            let mut published = None;
            producer
                .step_unwatched_with_clock_and_cutover(
                    &mut session,
                    || 1.0,
                    48_000,
                    |generation, frame, cut| published = Some((generation, frame, cut)),
                    |_| true,
                )
                .expect("replacement prefill");
            (generation_after, published)
        };

        // An ordinary edit: the flip carries no cut, so voices ring out.
        let (ordinary_generation, ordinary) = make(false);
        let (ordinary_pub_generation, ordinary_frame, ordinary_cut) =
            ordinary.expect("the ordinary replacement published");
        assert_eq!(
            ordinary_pub_generation, ordinary_generation,
            "generation matches"
        );
        assert_eq!(
            ordinary_cut,
            TakeoverCut::None,
            "an edit must not cut what sounds under it (frame {ordinary_frame})"
        );

        // A from-zero reload (a rewind): the flip carries the cut.
        let (rewind_generation, rewind) = make(true);
        let (rewind_pub_generation, rewind_frame, rewind_cut) =
            rewind.expect("the rewind published");
        assert_eq!(
            rewind_pub_generation, rewind_generation,
            "generation matches"
        );
        assert_eq!(
            rewind_cut,
            TakeoverCut::AtFlip,
            "a rewind's flip silences the outgoing rendition at the flip (frame {rewind_frame})"
        );
    }

    /// The freshness check, both ways, at the very same clocks: a turn that
    /// begins at 1.6 s and publishes at 1.7 s, for a replacement committed
    /// at 1.0 s. An ordinary edit's takeover (the commit plus its head-room,
    /// at most half a second) is then in rendered frames, so it is refused
    /// and the audible score comes back. A restart slides its cycle zero to
    /// the turn's clock and publishes a moment behind it with the flip's
    /// cut; the consumer admits its past-due downbeat at its restart floor.
    /// Refusing it as well would leave the room the cut silenced silent.
    #[test]
    fn a_late_publication_refuses_an_edit_but_lands_a_restart() {
        const OLD: &str = "setcps(1); note('c4')";
        let run = |from_zero: bool| {
            let mut session = Session::new().expect("session");
            session.set_direct_diagnostic_logging(false);
            session.set_continuity_margin(0.0);
            session.evaluate(OLD).expect("initial score");
            let mut producer =
                LiveFileProducer::unwatched(Duration::from_millis(2)).expect("producer");
            producer
                .step_unwatched_with_clock_and_cutover(
                    &mut session,
                    || 0.0,
                    48_000,
                    |_, _, _| panic!("initial generation must not cut over"),
                    |_| true,
                )
                .expect("initial prefill");
            let generation_before = session.generation();
            session
                .mark_audible_generation(generation_before)
                .expect("injected replay-policy target");
            let transport = session.transport();
            if from_zero {
                session.start_next_from_zero();
            }
            let generation_after = session
                .reload_with_clock_cancellable(
                    "setcps(1); note('d4')",
                    false,
                    transport.stopped_flag(),
                    || 1.0,
                )
                .expect("replacement");
            producer.arm_replacement(generation_before, generation_after);

            let mut clocks = [1.6, 1.7].into_iter();
            let mut published = Vec::new();
            let result = producer.step_unwatched_with_clock_and_cutover(
                &mut session,
                || clocks.next().unwrap_or(1.7),
                48_000,
                |generation, frame, cut| published.push((generation, frame, cut)),
                |_| true,
            );
            (session, generation_after, result, published)
        };

        let (session, edit, result, published) = run(false);
        let error = result.expect_err("a late edit is refused");
        assert!(
            error.to_string().contains("finished after its takeover"),
            "{error}"
        );
        assert!(
            published.is_empty(),
            "the late edit published: {published:?}"
        );
        assert_eq!(
            session.generation(),
            edit + 1,
            "the refusal installed the audible score again"
        );
        assert_eq!(session.active_source(), Some(OLD));

        let (session, restart, result, published) = run(true);
        result.expect("a late restart publishes");
        assert_eq!(
            published,
            [(restart, 76_800, TakeoverCut::AtFlip)],
            "cycle zero slid to the turn's clock, cut at the flip"
        );
        assert_eq!(session.generation(), restart, "no rollback");
    }

    /// A plain lane of `per_cycle` onsets a cycle: at the default half cycle
    /// a second and 48 kHz, one every `96000 / per_cycle` frames.
    fn plain_lane(per_cycle: u32) -> String {
        format!(r#"$: s("square*{per_cycle}").gain(0.1)"#)
    }

    /// That lane under a stretched one with an onset every 12000 frames. A
    /// stretched voice is aimed 40 ms and the vocoder's quantum lag before
    /// its onset: 2047 frames. The producer's batch is in onset order, so
    /// its target frames are out of order: the stretched onset of frame
    /// 12000, aimed at 9953, comes after every plain onset before 12000.
    fn stretched_over_plain_lane(per_cycle: u32) -> String {
        format!("$: s(\"sawtooth*8\").stretch(1)\n{}", plain_lane(per_cycle))
    }

    /// What one producer turn sent to the device.
    #[derive(Default)]
    struct Sent {
        /// Each published generation with its takeover frame.
        takeovers: Vec<(u64, u64)>,
        /// Each event's generation, target frame and receipt ordinal.
        events: Vec<(u64, u64, Option<u32>)>,
    }

    impl Sent {
        /// The target frame and receipt ordinal of each event of `generation`.
        fn window(&self, generation: u64) -> Vec<(u64, Option<u32>)> {
            self.events
                .iter()
                .filter(|(event_generation, _, _)| *event_generation == generation)
                .map(|&(_, target_frame, receipt)| (target_frame, receipt))
                .collect()
        }
    }

    /// One producer step at the device's render frontier, then one rendered
    /// block of 128 frames.
    fn turn(
        session: &mut Session,
        producer: &mut LiveFileProducer,
        output: &mut rustel_audio::device::ManualLiveOutput,
    ) -> Sent {
        let mut sent = Sent::default();
        let device = output.device();
        producer
            .step_unwatched_with_clock_and_cutover(
                session,
                || device.render_frontier_seconds(),
                48_000,
                |generation, takeover, cut| {
                    sent.takeovers.push((generation, takeover));
                    device.set_generation(generation, takeover, cut);
                },
                |event| {
                    let pushed = device.push(event);
                    if pushed {
                        sent.events.push((
                            event.generation,
                            event.target_frame,
                            event.confirmation.map(|receipt| receipt.ordinal),
                        ));
                    }
                    pushed
                },
            )
            .expect("producer step");
        output.render(&mut [0.0; 256]);
        assert!(session.consume_audio_confirmations());
        sent
    }

    /// Whether the device certifies the first window of `generation` within
    /// `turns` turns.
    fn certified_within(
        turns: usize,
        generation: u64,
        session: &mut Session,
        producer: &mut LiveFileProducer,
        output: &mut rustel_audio::device::ManualLiveOutput,
    ) -> bool {
        for _ in 0..turns {
            if session.confirmed_audio_generation() == Some(generation) {
                return true;
            }
            turn(session, producer, output);
        }
        session.confirmed_audio_generation() == Some(generation)
    }

    /// The receipts a window's events carry when the device certifies it
    /// from `start_frame`: none before that frame, and one each from it on,
    /// numbered in the order the events are sent.
    fn receipts_from(start_frame: u64, window: &[(u64, Option<u32>)]) -> Vec<Option<u32>> {
        let mut next = 0;
        window
            .iter()
            .map(|&(target_frame, _)| {
                (target_frame >= start_frame).then(|| {
                    next += 1;
                    next - 1
                })
            })
            .collect()
    }

    /// The device certifies a first window whose target frames are out of
    /// order. A stretched lane over a plain lane of 64 onsets a cycle gives
    /// the target frames 9000, 10500, 9953, 12000.
    #[test]
    fn a_first_window_out_of_target_order_is_certified() {
        use rustel_audio::device::ManualLiveOutput;

        let mut session = Session::new().expect("session");
        session
            .evaluate(&stretched_over_plain_lane(64))
            .expect("score");
        let generation = session.generation();
        let mut output = ManualLiveOutput::new(48_000, generation).expect("output");
        session
            .bind_audio_confirmations(output.device().confirmations())
            .expect("bind output");
        session.set_schedule_lead(0.0);
        session.restart_transport_at(0.0);
        let mut producer = LiveFileProducer::unwatched(Duration::from_millis(2)).expect("producer");

        let sent = turn(&mut session, &mut producer, &mut output);
        assert!(
            sent.takeovers.is_empty(),
            "initial prefill replaced a generation"
        );
        let window = sent.window(generation);
        assert!(
            window.windows(2).any(|pair| pair[0].0 > pair[1].0),
            "test premise: the window is out of target order: {window:?}"
        );
        // The window starts on frame zero, so the device can certify every
        // one of its events.
        let receipts: Vec<_> = window.iter().map(|&(_, receipt)| receipt).collect();
        assert_eq!(receipts, receipts_from(0, &window), "{window:?}");

        assert!(
            certified_within(512, generation, &mut session, &mut producer, &mut output),
            "the device did not certify the first window: {:?}",
            output.device().report()
        );
    }

    /// After an edit, only the events aimed at or after the takeover have a
    /// receipt, and the device certifies the window. The takeover is on
    /// frame 46576. The stretched onset of frame 48000 is aimed at 45953,
    /// and is fourth in the batch, after plain onsets on 46875 to 47625.
    #[test]
    fn an_edit_gives_no_receipt_to_a_stretched_onset_aimed_before_its_takeover() {
        use rustel_audio::device::ManualLiveOutput;

        /// Blocks of 128 frames rendered before the edit: frame 40576.
        const EDIT_BLOCK: usize = 317;

        let mut session = Session::new().expect("session");
        session.set_schedule_lead(0.0);
        session.set_continuity_margin(0.125);
        session.evaluate(&plain_lane(256)).expect("plain score");
        session.restart_transport_at(0.0);
        let original = session.generation();
        let mut output = ManualLiveOutput::new(48_000, original).expect("output");
        session
            .bind_audio_confirmations(output.device().confirmations())
            .expect("bind output");
        let mut producer = LiveFileProducer::unwatched(Duration::from_millis(2)).expect("producer");
        for _ in 0..EDIT_BLOCK {
            turn(&mut session, &mut producer, &mut output);
        }
        assert_eq!(
            session.confirmed_audio_generation(),
            Some(original),
            "test premise: the plain score is certified before the edit"
        );

        let now = output.device().render_frontier_seconds();
        let edit = session
            .reload_at(&stretched_over_plain_lane(256), false, now)
            .expect("the edit");
        producer.arm_replacement(original, edit);
        let sent = turn(&mut session, &mut producer, &mut output);
        assert_eq!(
            sent.takeovers,
            [(edit, 46_576)],
            "test premise: the edit publishes on its first turn"
        );
        let takeover = sent.takeovers[0].1;
        let window = sent.window(edit);
        let first_certifiable = window
            .iter()
            .position(|&(target_frame, _)| target_frame >= takeover)
            .expect("test premise: the window reaches past its takeover");
        assert!(
            window[first_certifiable..]
                .iter()
                .any(|&(target_frame, _)| target_frame < takeover),
            "test premise: an event aimed before the takeover comes after one aimed past it: {window:?}"
        );
        let receipts: Vec<_> = window.iter().map(|&(_, receipt)| receipt).collect();
        assert_eq!(receipts, receipts_from(takeover, &window), "{window:?}");

        assert!(
            certified_within(512, edit, &mut session, &mut producer, &mut output),
            "the device did not certify the edit's first window: {:?}",
            output.device().report()
        );
    }
}

#[cfg(test)]
mod panic_tests {
    use super::*;
    use rustel_audio::device::ManualLiveOutput;

    const GOOD: &str = "note('c3').s('sine').fast(8).gain(0.1)";

    fn confirmed_score() -> (Session, LiveFileProducer, ManualLiveOutput, u64) {
        let mut session = Session::new().expect("session");
        session.set_schedule_lead(0.0);
        session.set_continuity_margin(0.125);
        session.evaluate(GOOD).expect("audible score");
        session.restart_transport_at(0.0);
        let generation = session.generation();
        let mut output = ManualLiveOutput::new(48_000, generation).expect("output");
        session
            .bind_audio_confirmations(output.device().confirmations())
            .expect("bind output");
        let mut producer = LiveFileProducer::unwatched(Duration::from_millis(2)).expect("producer");
        let initial = producer
            .step_unwatched_with_clock_and_cutover(
                &mut session,
                || 0.0,
                48_000,
                |_, _, _| panic!("initial prefill changed the device generation"),
                |event| output.device().push(event),
            )
            .expect("initial prefill");
        assert!(initial.pushed > 0, "{initial:?}");
        let mut rendered = [0.0; 256];
        let mut sounded = false;
        for _ in 0..512 {
            output.render(&mut rendered);
            sounded |= rendered.iter().any(|sample| sample.abs() > 0.0001);
            assert!(session.consume_audio_confirmations());
            if session.confirmed_audio_generation() == Some(generation) {
                break;
            }
        }
        assert!(sounded, "no audible output: {:?}", output.device().report());
        assert_eq!(session.confirmed_audio_generation(), Some(generation));
        (session, producer, output, generation)
    }

    #[test]
    fn query_panic_rejects_the_candidate_and_refills_the_last_good_score() {
        let (mut session, mut producer, mut output, original) = confirmed_score();
        let old_transport = session.transport();
        session
            .set_pattern(rustel_core::signal(|_, _| {
                panic!("injected producer query panic")
            }))
            .expect("install injected query");
        let candidate = session.generation();
        producer.arm_replacement(original, candidate);
        let now = output.device().clock_seconds();
        let error = producer
            .step_unwatched_with_clock_and_cutover(
                &mut session,
                || now,
                48_000,
                |_, _, _| panic!("panicked candidate reached the device"),
                |_| panic!("panicked candidate emitted audio"),
            )
            .expect_err("injected query panicked");
        assert!(matches!(error, RuntimeError::Panic(_)), "{error:?}");
        assert!(error.to_string().contains("injected producer query panic"));
        assert_eq!(output.device().generation(), original);
        assert_eq!(producer.take_abandoned_generation(), Some(candidate));
        assert_eq!(session.active_source(), Some(GOOD));
        assert!(std::sync::Arc::ptr_eq(&old_transport, &session.transport()));
        assert!(producer.pending.is_empty());
        assert!(!producer.prefill_candidate_committed);
        let restored = session.generation();
        assert!(restored > candidate);
        assert_eq!(producer.pending_generation, Some(restored));

        let mut published = Vec::new();
        let mut takeover_frame = 0;
        let recovered = producer
            .step_unwatched_with_clock_and_cutover(
                &mut session,
                || now,
                48_000,
                |generation, frame, cut| {
                    published.push(generation);
                    takeover_frame = frame;
                    output.device().set_generation(generation, frame, cut);
                },
                |event| {
                    assert_eq!(event.generation, restored);
                    output.device().push(event)
                },
            )
            .expect("fresh realm resumes the last good score");
        assert_eq!(published, [restored]);
        // Recovery accounts for the rebuild time. Render to its takeover before
        // retrying, so the device clock can reach the restored score's cursor.
        let resume_frame = takeover_frame.checked_add(1).expect("takeover frame");
        let frames = resume_frame.saturating_sub(output.device().clock_frames());
        let mut rendered = [0.0; 1_024];
        for offset in (0..frames).step_by(512) {
            let count = (frames - offset).min(512) as usize;
            output.render(&mut rendered[..count * 2]);
        }
        let mut pushed = recovered.pushed;
        let mut last_step = recovered.clone();
        for _ in 0..8 {
            if pushed > 0 {
                break;
            }
            last_step = producer
                .step_unwatched_with_clock_and_cutover(
                    &mut session,
                    || output.device().clock_seconds(),
                    48_000,
                    |generation, frame, cut| output.device().set_generation(generation, frame, cut),
                    |event| {
                        assert_eq!(event.generation, restored);
                        output.device().push(event)
                    },
                )
                .expect("restored score keeps stepping");
            pushed += last_step.pushed;
            if pushed == 0 {
                output.render(&mut [0.0; 12_000]);
            }
        }
        assert!(
            pushed > 0,
            "the restored score never queued audio: before={now}, takeover_frame={takeover_frame}, clock={}, last={last_step:?}",
            output.device().clock_seconds()
        );
        let WatchPoll::Event(rejected) = recovered.watch else {
            panic!("failed candidate must be reported as rejected");
        };
        assert_eq!(rejected.status, crate::ReloadStatus::Rejected);
        assert_eq!(rejected.error_kind.as_deref(), Some("panic"));
        assert!(
            rejected
                .message
                .as_deref()
                .is_some_and(|message| message.contains("injected producer query panic"))
        );
        let mut rendered = vec![0.0; 48_000];
        output.render(&mut rendered);
        assert!(rendered.iter().any(|sample| sample.abs() > 0.0001));
    }

    #[test]
    fn reload_shield_query_panic_surfaces_and_rearms_the_restored_score() {
        let (mut session, mut producer, output, original) = confirmed_score();
        session
            .set_pattern(rustel_core::signal(|_, _| {
                panic!("injected reload shield panic")
            }))
            .expect("install injected query");
        let failed = session.generation();
        let error = producer
            .shield_reload_with_clock(
                &mut session,
                || output.device().clock_seconds(),
                48_000,
                |_| panic!("panicked shield query emitted audio"),
            )
            .expect_err("shield query must surface panic recovery");
        assert!(matches!(error, RuntimeError::Panic(_)), "{error:?}");
        assert!(error.to_string().contains("injected reload shield panic"));
        assert_eq!(output.device().generation(), original);
        assert_eq!(session.active_source(), Some(GOOD));
        assert!(session.generation() > failed);
        assert_eq!(producer.pending_generation, Some(session.generation()));
        assert!(!producer.pending_reload_visible);
        assert!(producer.pending.is_empty());
    }

    #[test]
    fn a_host_turn_panic_supersedes_an_unpublished_candidate_on_the_next_step() {
        let (mut session, mut producer, output, original) = confirmed_score();
        session
            .set_pattern(rustel_core::silence())
            .expect("candidate graph");
        let candidate = session.generation();
        producer.arm_replacement(original, candidate);
        let now = output.device().clock_seconds();
        let error = session
            .with_panic_recovery(now, |_| -> Result<(), RuntimeError> {
                panic!("injected host turn panic")
            })
            .expect_err("host score turn panicked");
        assert!(matches!(error, RuntimeError::Panic(_)));
        let restored = session.generation();
        assert!(restored > candidate);
        let mut published = Vec::new();
        let mut takeover_frame = 0;
        let result = producer
            .step_unwatched_with_clock_and_cutover(
                &mut session,
                || now,
                48_000,
                |generation, frame, _| {
                    published.push(generation);
                    takeover_frame = frame;
                },
                |event| {
                    assert_eq!(event.generation, restored);
                    true
                },
            )
            .expect("producer observes the rebuilt Session");
        assert_eq!(published, [restored]);
        assert_eq!(producer.take_abandoned_generation(), Some(candidate));
        // The first step must publish immediately. Later steps advance from the
        // reported takeover, which includes the time spent rebuilding the realm.
        let resume = takeover_frame.checked_add(1).expect("takeover frame") as f64 / 48_000.0;
        let mut pushed = result.pushed;
        let mut last_step = result.clone();
        for step in 0..8 {
            if pushed > 0 {
                break;
            }
            last_step = producer
                .step_unwatched_with_clock_and_cutover(
                    &mut session,
                    || resume + f64::from(step) * 0.125,
                    48_000,
                    |_, _, _| {},
                    |event| {
                        assert_eq!(event.generation, restored);
                        true
                    },
                )
                .expect("restored score keeps stepping");
            pushed += last_step.pushed;
        }
        assert!(
            pushed > 0,
            "the restored score never queued audio: before={now}, resume={resume}, last={last_step:?}"
        );
        assert_eq!(session.active_source(), Some(GOOD));
        assert_eq!(producer.recovery_epoch, session.recovery_epoch());
        assert!(matches!(
            result.watch,
            WatchPoll::Event(crate::ReloadEvent {
                status: crate::ReloadStatus::Rejected,
                ..
            })
        ));
    }

    #[test]
    fn a_new_valid_candidate_after_recovery_keeps_its_publication() {
        let (mut session, mut producer, output, _) = confirmed_score();
        let now = output.device().clock_seconds();
        session
            .with_panic_recovery(now, |_| -> Result<(), RuntimeError> {
                panic!("injected host turn panic")
            })
            .expect_err("host score turn panicked");
        let restored = session.generation();
        let source = "note('g4').s('triangle').fast(8).gain(0.1)";
        let replacement = session
            .reload_at(source, false, now)
            .expect("a later command installs valid source");
        producer.arm_replacement(restored, replacement);
        let mut published = Vec::new();
        let result = producer
            .step_unwatched_with_clock_and_cutover(
                &mut session,
                || now,
                48_000,
                |generation, _, _| published.push(generation),
                |_| true,
            )
            .expect("the next producer step publishes the new valid source");
        assert_eq!(published, [replacement]);
        assert_eq!(producer.take_abandoned_generation(), None);
        assert_eq!(session.active_source(), Some(source));
        assert!(matches!(
            result.watch,
            WatchPoll::Event(crate::ReloadEvent {
                status: crate::ReloadStatus::Installed,
                generation_after,
                ..
            }) if generation_after == replacement
        ));
    }

    #[test]
    fn a_watched_evaluation_panic_rejects_the_file_identity_once() {
        let (mut session, mut producer, output, _) = confirmed_score();
        let directory = tempfile::tempdir().expect("temporary score directory");
        let path = directory.path().join("score.strudel");
        producer.score_watch = FileWatch::from_loaded_source(
            &path,
            WatchTarget::Score(WatchLanguage::JavaScript),
            GOOD,
            Duration::ZERO,
        );
        std::fs::write(&path, "note('g4').s('triangle').fast(8)").expect("candidate save");
        let now = output.device().clock_seconds();
        producer
            .step_unwatched_with_clock(&mut session, || now, 48_000, |_| true)
            .expect("extend cover before evaluating");
        producer
            .step_with_clock(
                &mut session,
                Duration::from_secs(1),
                || now,
                48_000,
                |_, _, _| panic!("first observation must not install"),
                |_| true,
            )
            .expect("observe candidate");
        session.inject_panic_for_test(crate::SessionPanicPoint::Evaluation);
        let mut published = Vec::new();
        let step = producer
            .step_with_clock(
                &mut session,
                Duration::from_secs(2),
                || now,
                48_000,
                |generation, _, _| published.push(generation),
                |_| true,
            )
            .expect("watch reports rejection and keeps the producer alive");
        let WatchPoll::Event(event) = step.watch else {
            panic!("evaluation panic must reject the stable save");
        };
        assert_eq!(event.status, crate::ReloadStatus::Rejected);
        assert_eq!(event.error_kind.as_deref(), Some("panic"));
        assert!(
            event
                .message
                .is_some_and(|message| message.contains("injected native Evaluation panic"))
        );
        assert_eq!(session.active_source(), Some(GOOD));
        assert_eq!(published, [session.generation()]);
        let next = producer
            .step_with_clock(
                &mut session,
                Duration::from_secs(3),
                || now,
                48_000,
                |_, _, _| panic!("unchanged failed save must not republish"),
                |_| true,
            )
            .expect("keep playing after the rejected identity");
        assert_eq!(next.watch, WatchPoll::Unchanged);
    }

    #[test]
    fn a_producer_made_after_a_recovery_does_not_recover_again() {
        let (mut session, producer, output, _) = confirmed_score();
        let now = output.device().clock_seconds();
        session
            .with_panic_recovery(now, |_| -> Result<(), RuntimeError> {
                panic!("injected host turn panic")
            })
            .expect_err("host score turn panicked");
        drop(producer);
        output
            .device()
            .set_generation(session.generation(), 0, TakeoverCut::None);
        let mut producer = LiveFileProducer::unwatched(Duration::from_millis(2)).expect("producer");
        producer.adopt_recovery_epoch(&session);
        producer
            .step_unwatched_with_clock_and_cutover(
                &mut session,
                || now,
                48_000,
                |_, _, _| panic!("a fresh producer replayed an old recovery"),
                |event| output.device().push(event),
            )
            .expect("fresh producer steps");
        assert_eq!(producer.take_abandoned_generation(), None);
    }

    #[test]
    fn a_new_performance_never_recovers_to_the_previous_ones_score() {
        let (mut session, _, _, _) = confirmed_score();
        let next = "note('e3').s('sine').fast(8).gain(0.1)";
        session.evaluate(next).expect("next performance's score");
        let output = ManualLiveOutput::new(48_000, session.generation()).expect("new output");
        session
            .bind_audio_confirmations(output.device().confirmations())
            .expect("bind new output");
        session.restart_transport_at(0.0);
        session
            .with_panic_recovery(0.0, |_| -> Result<(), RuntimeError> {
                panic!("injected start panic")
            })
            .expect_err("start turn panicked");
        assert_eq!(session.active_source(), Some(next));
    }
}

/// A score played through a hand-rendered device, for the tests that compare
/// the audio of a handover with the audio without it.
#[cfg(test)]
mod hand_rendered {
    use super::*;
    use rustel_audio::device::ManualLiveOutput;

    pub(super) const RATE: u32 = 48_000;
    pub(super) const BLOCK: usize = 128;

    /// What takes the playing generation over part-way through a render.
    #[derive(Clone, Copy)]
    pub(super) enum Handover<'a> {
        /// A live edit: the score is saved with this text.
        Save(&'a str),
        /// A launch quantised to a line: a save with this text, told to
        /// take over at this time in seconds, without a rewind.
        Launch(&'a str, f64),
        /// A rewind: a save with this text, played from its beginning. It
        /// takes over with a cut.
        #[cfg(any(feature = "osc", feature = "serial"))]
        Rewind(&'a str),
        /// A control requery: the same graph under a fresh generation.
        Requery,
        /// A control requery whose first window takes this many device
        /// blocks: the device renders them before the producer publishes.
        LateRequery(usize),
        /// This note is pressed on the input named `keyboard`, and the
        /// control requery that a live host makes for a press follows.
        #[cfg(any(feature = "osc", feature = "serial"))]
        Press(u8),
    }

    /// What one pass played.
    pub(super) struct Played {
        /// The stereo output.
        pub(super) pcm: Vec<f32>,
        /// The takeover frames the producer published.
        pub(super) takeovers: Vec<u64>,
        /// The frame of every MIDI onset left for its port, in order. A
        /// takeover removes the outgoing generation's onsets from the
        /// takeover frame on, as the MIDI bridge does.
        #[cfg(feature = "midi")]
        pub(super) midi_frames: Vec<u64>,
        /// Every note-on a capture port got from the MIDI bridge, in order.
        #[cfg(feature = "midi")]
        pub(super) midi_note_ons: Vec<Vec<u8>>,
        /// The generation and the frame of every OSC bundle handed out, in
        /// order.
        #[cfg(feature = "osc")]
        pub(super) osc: Vec<(u64, u64)>,
        /// The generation and the frame of every serial write handed out,
        /// in order.
        #[cfg(feature = "serial")]
        pub(super) serial: Vec<(u64, u64)>,
    }

    /// One kind of message, OSC or serial, as a live host hands it out: a
    /// steady pass at once, a reload shield's pass once each message is within
    /// the steady cover, less what a takeover drops from its frame on.
    #[cfg(any(feature = "osc", feature = "serial"))]
    #[derive(Default)]
    struct HandedOut {
        /// `(generation, frame)` of every message handed out, in order.
        sent: Vec<(u64, u64)>,
        /// `(generation, target time)` of what a reload shield staged.
        held: Vec<(u64, f64)>,
    }

    #[cfg(any(feature = "osc", feature = "serial"))]
    impl HandedOut {
        fn frame(target_time: f64) -> u64 {
            crate::render::onset_frame_at(target_time, RATE)
        }

        /// Hand out `staged` and what is held and due before `through`, in
        /// seconds. `note` tells the session of each message, as a host does.
        fn send(&mut self, staged: Vec<(u64, f64)>, through: f64, mut note: impl FnMut(u64, f64)) {
            let due = self.held.extract_if(.., |(_, target)| *target < through);
            for (generation, target) in staged.into_iter().chain(due) {
                self.sent.push((generation, Self::frame(target)));
                note(generation, target);
            }
        }

        /// Follow a flip, as a host does in its cutover callback.
        fn take_over(&mut self, takeover: u64) {
            self.held
                .retain(|(_, target)| Self::frame(*target) < takeover);
        }
    }

    /// The MIDI bridge over a capture port, as a live host drives it. The
    /// sender sends an onset when the device clock reaches its frame: here
    /// the bridge gets it once the device has rendered the frame, due at once.
    #[cfg(feature = "midi")]
    struct PortMidi {
        outputs: crate::midi_bridge::MidiOutputs,
        capture: rustel_midi::CapturePort,
        /// The onsets the device has not rendered the frame of.
        waiting: Vec<crate::midi_bridge::MidiOnset>,
        /// The name the score gives the port.
        name: String,
        /// The markers sent to find out that the port is up to date.
        markers: usize,
    }

    #[cfg(feature = "midi")]
    impl PortMidi {
        fn new(name: String) -> Self {
            let capture = rustel_midi::CapturePort::new();
            let port = capture.clone();
            let outputs = crate::midi_bridge::MidiOutputs::with_opener(std::sync::Arc::new(
                move |name: &str| {
                    Ok(rustel_midi::MidiSender::with_port(
                        Box::new(port.clone()),
                        name.to_owned(),
                    ))
                },
            ));
            Self {
                outputs,
                capture,
                waiting: Vec::new(),
                name,
                markers: 0,
            }
        }

        fn frame(intent: &crate::midi_bridge::MidiOnset) -> u64 {
            crate::midi_bridge::onset_frame_at(intent.target_time, RATE)
        }

        /// Take the onsets a scheduling pass staged, as a host does.
        fn stage(&mut self, intents: Vec<crate::midi_bridge::MidiOnset>) {
            for intent in &intents {
                self.outputs
                    .reserve_generation_port(intent.generation, &intent.port)
                    .expect("MIDI port");
            }
            self.waiting.extend(intents);
        }

        /// Hand the bridge the onsets whose frame the device has rendered.
        /// A note-off waits an hour, so each note sounds for the whole pass.
        fn send_due(&mut self, audible: u64, session: u64, clock: u64) {
            self.outputs.poll_at(audible, session, clock);
            let now = Instant::now();
            let due: Vec<_> = self
                .waiting
                .extract_if(.., |intent| Self::frame(intent) < clock)
                .collect();
            for intent in due {
                let batch = rustel_midi::plan(&intent.controls, intent.duration_secs)
                    .into_iter()
                    .map(|(offset, message)| {
                        let wait = if offset > 0.0 { 3600 } else { 0 };
                        (now + Duration::from_secs(wait), message)
                    })
                    .collect();
                let frame = Self::frame(&intent);
                assert!(
                    self.outputs
                        .submit_batch_for_generation_at(
                            intent.generation,
                            &intent.port,
                            frame,
                            batch
                        )
                        .expect("MIDI batch")
                );
            }
        }

        /// Wait until the port has all that is due. The sender sends in due
        /// order, so a marker due now arrives after every earlier message.
        fn settle(&mut self, audible: u64, session: u64, clock: u64) {
            let marker = rustel_midi::MidiMessage::clock();
            assert!(
                self.outputs
                    .submit_batch_for_generation_at(
                        audible,
                        &self.name,
                        0,
                        vec![(Instant::now(), marker)]
                    )
                    .expect("marker")
            );
            self.markers += 1;
            let arrived = |capture: &rustel_midi::CapturePort| {
                capture
                    .messages()
                    .iter()
                    .filter(|(_, bytes)| bytes.as_slice() == marker.as_slice())
                    .count()
            };
            let deadline = Instant::now() + Duration::from_secs(10);
            while arrived(&self.capture) < self.markers {
                assert!(Instant::now() < deadline, "the MIDI port never settled");
                self.outputs.poll_at(audible, session, clock);
                std::thread::sleep(Duration::from_millis(1));
            }
        }

        /// Follow a flip, as a host does in its cutover callback. The port
        /// has every note handed to it so far. The sender prunes the
        /// outgoing batches that have not started: here, those that wait.
        fn take_over(
            &mut self,
            outgoing: u64,
            incoming: u64,
            takeover: u64,
            cut: TakeoverCut,
            clock: u64,
        ) {
            self.settle(outgoing, outgoing, clock);
            self.waiting
                .retain(|intent| intent.generation != outgoing || Self::frame(intent) < takeover);
            self.outputs
                .take_over_generation_from(outgoing, incoming, takeover, takeover, cut);
        }

        /// Every note-on the port got, in order.
        fn note_ons(mut self, audible: u64, session: u64, clock: u64) -> Vec<Vec<u8>> {
            self.settle(audible, session, clock);
            self.capture
                .messages()
                .into_iter()
                .map(|(_, bytes)| bytes)
                .filter(|bytes| bytes[0] & 0xf0 == 0x90)
                .collect()
        }
    }

    /// Play `score` through a hand-rendered device for `seconds`. The
    /// transport starts `early_frames` before the device's schedule lead.
    /// A handover runs once the device clock reaches its time, the way a
    /// live host does it: a save is the shield, the reload on the device
    /// clock, then the armed replacement; a launch names its takeover time
    /// first; a requery is armed as a control requery.
    pub(super) fn played(
        score: &str,
        early_frames: u32,
        handover: Option<(Handover<'_>, f64)>,
        seconds: f64,
    ) -> Played {
        played_each(score, early_frames, handover.into_iter().collect(), seconds)
    }

    /// [`played`] with several handovers, in the order of their times. One
    /// runs in each turn of the producer.
    pub(super) fn played_each(
        score: &str,
        early_frames: u32,
        handovers: Vec<(Handover<'_>, f64)>,
        seconds: f64,
    ) -> Played {
        let mut handovers = handovers.into_iter().peekable();
        let mut session = Session::new().expect("session");
        session.evaluate(score).expect("score");
        let mut output = ManualLiveOutput::new(RATE, session.generation()).expect("output");
        session
            .bind_audio_confirmations(output.device().confirmations())
            .expect("bind output");
        // The device's lead before its first callback: three buffers and two
        // more. Inside WSL the device's own value has a floor, and these
        // tests count their frames from the lead.
        let buffer = f64::from(output.device().requested_buffer_frames()) / f64::from(RATE);
        let lead = 3.0 * buffer + 2.0 * buffer;
        session.set_schedule_lead(lead);
        session.restart_transport_at(lead - f64::from(early_frames) / f64::from(RATE));
        let mut producer = LiveFileProducer::unwatched(Duration::from_millis(2)).expect("producer");
        let mut pcm = Vec::new();
        let mut takeovers = Vec::new();
        // `(generation, frame)` of each MIDI onset no takeover has pruned.
        #[cfg(feature = "midi")]
        let mut midi: Vec<(u64, u64)> = Vec::new();
        // The bridge and its port, made for the first MIDI onset.
        #[cfg(feature = "midi")]
        let mut port: Option<PortMidi> = None;
        #[cfg(feature = "midi")]
        let stage_midi =
            |session: &mut Session, midi: &mut Vec<(u64, u64)>, port: &mut Option<PortMidi>| {
                let intents = session.take_pending_midi();
                midi.extend(
                    intents
                        .iter()
                        .map(|intent| (intent.generation, PortMidi::frame(intent))),
                );
                if let Some(first) = intents.first() {
                    port.get_or_insert_with(|| PortMidi::new(first.port.clone()))
                        .stage(intents);
                }
            };
        #[cfg(feature = "osc")]
        let mut osc = HandedOut::default();
        #[cfg(feature = "serial")]
        let mut serial = HandedOut::default();
        let mut block = [0.0f32; BLOCK * 2];
        // Device blocks the producer still spends on its first window.
        let mut busy_blocks = 0;
        while output.device().clock_seconds() < seconds {
            let device = output.device();
            session.set_continuity_margin(device.continuity_margin_seconds());
            // The sender has a thread of its own: a busy producer does not
            // hold a due note back.
            #[cfg(feature = "midi")]
            if let Some(port) = port.as_mut() {
                port.send_due(
                    device.generation(),
                    session.generation(),
                    device.clock_frames(),
                );
            }
            if let Some((handover, _)) = handovers.next_if(|(_, at)| device.clock_seconds() >= *at)
            {
                #[cfg(any(feature = "osc", feature = "serial"))]
                if let Handover::Press(note) = handover {
                    // The host reads the keyboards of both generations.
                    let bus = session.midi_input_bus();
                    bus.snapshot_for(device.generation(), session.generation());
                    let keyboard = bus.find("keyboard").expect("the score's keyboard");
                    keyboard.observe_note_on(rustel_core::midi_in::now_nanos(), 1, note, 100);
                }
                let save = match handover {
                    Handover::Save(text) => Some(text),
                    Handover::Launch(text, line) => {
                        session.set_next_takeover_time(line);
                        Some(text)
                    }
                    #[cfg(any(feature = "osc", feature = "serial"))]
                    Handover::Rewind(text) => {
                        session.start_next_from_zero();
                        Some(text)
                    }
                    // A control requery, armed below. A press arms one too.
                    #[cfg(any(feature = "osc", feature = "serial"))]
                    Handover::Press(_) => None,
                    Handover::Requery | Handover::LateRequery(_) => None,
                };
                if save.is_none() {
                    let (before, after) = session
                        .requery_active_at(device.clock_seconds())
                        .expect("requery")
                        .expect("a running transport re-queries");
                    producer.arm_control_requery(before, after);
                    if let Handover::LateRequery(blocks) = handover {
                        busy_blocks = blocks;
                    }
                }
                if let Some(text) = save {
                    producer
                        .shield_reload_with_clock(
                            &mut session,
                            || device.clock_seconds(),
                            RATE,
                            |event| device.push(event),
                        )
                        .expect("reload shield");
                    #[cfg(feature = "midi")]
                    stage_midi(&mut session, &mut midi, &mut port);
                    #[cfg(feature = "osc")]
                    osc.held.extend(staged_osc(&mut session));
                    #[cfg(feature = "serial")]
                    serial.held.extend(staged_serial(&mut session));
                    let before = session.generation();
                    let transport = session.transport();
                    let after = session
                        .reload_with_clock_cancellable(
                            text,
                            false,
                            transport.stopped_flag(),
                            || device.clock_seconds(),
                        )
                        .expect("save");
                    producer.arm_replacement(before, after);
                }
            }
            if busy_blocks > 0 {
                busy_blocks -= 1;
                output.render(&mut block);
                pcm.extend_from_slice(&block);
                continue;
            }
            producer
                .step_unwatched_with_clock_and_cutover(
                    &mut session,
                    || device.render_frontier_seconds(),
                    RATE,
                    |generation, takeover, cut| {
                        takeovers.push(takeover);
                        #[cfg(feature = "midi")]
                        {
                            let outgoing = device.generation();
                            midi.retain(|(generation, frame)| {
                                *generation != outgoing || *frame < takeover
                            });
                            if let Some(port) = port.as_mut() {
                                let clock = device.clock_frames();
                                port.take_over(outgoing, generation, takeover, cut, clock);
                            }
                        }
                        #[cfg(feature = "osc")]
                        osc.take_over(takeover);
                        #[cfg(feature = "serial")]
                        serial.take_over(takeover);
                        device.set_generation(generation, takeover, cut);
                    },
                    |event| device.push(event),
                )
                .expect("producer step");
            #[cfg(feature = "midi")]
            stage_midi(&mut session, &mut midi, &mut port);
            // The span a steady pass sends ahead.
            #[cfg(any(feature = "osc", feature = "serial"))]
            let through = device.clock_seconds() + session.live_producer_schedule_cover();
            #[cfg(feature = "osc")]
            {
                let staged = staged_osc(&mut session);
                osc.send(staged, through, |generation, target_time| {
                    session.note_osc_handed_out(generation, target_time);
                });
            }
            #[cfg(feature = "serial")]
            {
                let staged = staged_serial(&mut session);
                serial.send(staged, through, |generation, target_time| {
                    session.note_serial_handed_out(generation, target_time);
                });
            }
            producer.complete_turn(Duration::ZERO);
            output.render(&mut block);
            pcm.extend_from_slice(&block);
        }
        #[cfg(feature = "midi")]
        let midi_note_ons = port.map_or_else(Vec::new, |port| {
            let device = output.device();
            port.note_ons(
                device.generation(),
                session.generation(),
                device.clock_frames(),
            )
        });
        Played {
            pcm,
            takeovers,
            #[cfg(feature = "midi")]
            midi_frames: {
                let mut frames: Vec<u64> = midi.into_iter().map(|(_, frame)| frame).collect();
                frames.sort_unstable();
                frames
            },
            #[cfg(feature = "midi")]
            midi_note_ons,
            #[cfg(feature = "osc")]
            osc: osc.sent,
            #[cfg(feature = "serial")]
            serial: serial.sent,
        }
    }

    /// `(generation, target time)` of the OSC bundles the last pass staged.
    #[cfg(feature = "osc")]
    fn staged_osc(session: &mut Session) -> Vec<(u64, f64)> {
        let staged = session.take_pending_osc();
        let key = |(_, intent): &(f64, crate::osc_bridge::OscOnset)| {
            (intent.generation, intent.target_time)
        };
        staged.iter().map(key).collect()
    }

    /// `(generation, target time)` of the serial writes the last pass staged.
    #[cfg(feature = "serial")]
    fn staged_serial(session: &mut Session) -> Vec<(u64, f64)> {
        let staged = session.take_pending_serial();
        let key = |(_, intent): &(f64, crate::serial_bridge::SerialOnset)| {
            (intent.generation, intent.target_time)
        };
        staged.iter().map(key).collect()
    }

    /// The interleaved sample two renders are furthest apart on, as
    /// `(seconds, difference)`.
    pub(super) fn furthest_apart(render: &[f32], reference: &[f32]) -> (f64, f32) {
        assert_eq!(render.len(), reference.len());
        let (sample, difference) = render
            .iter()
            .zip(reference)
            .map(|(render, reference)| (render - reference).abs())
            .enumerate()
            .fold((0, 0.0f32), |worst, (sample, difference)| {
                if difference > worst.1 {
                    (sample, difference)
                } else {
                    worst
                }
            });
        ((sample / 2) as f64 / f64::from(RATE), difference)
    }
}

#[cfg(test)]
mod edit_level_tests {
    use super::hand_rendered::{Handover, RATE, played};

    /// Play `score` through a hand-rendered device for `seconds` and return
    /// the stereo output. A `save` replaces the score with its text once the
    /// device clock reaches its time.
    fn rendered(score: &str, save: Option<(&str, f64)>, seconds: f64) -> Vec<f32> {
        let save = save.map(|(text, at)| (Handover::Save(text), at));
        played(score, 0, save, seconds).pcm
    }

    /// A save that leaves the sound alone leaves the audio alone. `unison` at
    /// its default changes each hap's value, not what a voice reads. The
    /// outgoing and the incoming copy of one chord must not both sound.
    #[test]
    fn a_save_that_leaves_the_sound_alone_leaves_the_audio_alone() {
        const PLAIN: &str = r#"$: s("supersaw")
    .dec(0.4)
    .seg(16)
    .note("<<e1,g2,c3>!4 <d2,f#3,g2>!2 <d2,g3,g2>!2>")
   // .unison(5)
"#;
        const WITH_UNISON: &str = r#"$: s("supersaw")
    .dec(0.4)
    .seg(16)
    .note("<<e1,g2,c3>!4 <d2,f#3,g2>!2 <d2,g3,g2>!2>")
    .unison(5)
"#;
        const SECONDS: f64 = 1.75;
        let reference = rendered(PLAIN, None, SECONDS);
        assert!(
            reference.iter().any(|sample| sample.abs() > 0.1),
            "test premise: the score sounds"
        );
        for step in 0..7 {
            let at = 1.0 + 0.02 * f64::from(step);
            let saved = rendered(PLAIN, Some((WITH_UNISON, at)), SECONDS);
            assert_eq!(saved.len(), reference.len());
            // The interleaved sample the two renders are furthest apart on.
            let (sample, difference) = saved
                .iter()
                .zip(&reference)
                .map(|(saved, reference)| (saved - reference).abs())
                .enumerate()
                .fold((0, 0.0f32), |worst, (sample, difference)| {
                    if difference > worst.1 {
                        (sample, difference)
                    } else {
                        worst
                    }
                });
            assert!(
                difference < 1e-4,
                "a save at {at:.2} s changed the audio by {difference} at {:.4} s",
                (sample / 2) as f64 / f64::from(RATE)
            );
        }
    }
}

#[cfg(test)]
mod handover_frame_tests {
    use super::hand_rendered::{BLOCK, Handover, Played, RATE, furthest_apart, played};

    const SECONDS: f64 = 1.6;
    /// The frame every handover below takes over on.
    const TAKEOVER_FRAME: u64 = 49_280;

    /// How the playing generation is taken over.
    #[derive(Clone, Copy)]
    enum Via {
        /// A live edit: the same score saved again under a comment.
        Save,
        /// A launch quantised to a line: the same save, told to take over
        /// on [`TAKEOVER_FRAME`], without a rewind.
        Launch,
        /// A control requery.
        Requery,
        /// A control requery that the producer publishes two device blocks
        /// after its takeover frame.
        LateRequery,
    }

    impl Via {
        const ALL: [Self; 3] = [Self::Save, Self::Launch, Self::Requery];
        /// The handovers tested with a chord exactly one frame before the
        /// takeover frame. A save pre-marks the chord. A requery starts
        /// after it and does not move the mapping. A launch re-anchors the
        /// mapping and pre-marks nothing, so rounding error decides which
        /// side of its line the chord is on.
        const EXACT_ONE_FRAME_BEFORE: [Self; 2] = [Self::Save, Self::Requery];

        /// The device block at whose start this handover runs, to take over
        /// on [`TAKEOVER_FRAME`]. A save takes over the replacement
        /// head-room later, and a requery two blocks later.
        const fn block(self) -> u64 {
            match self {
                Self::Save | Self::Launch => 355,
                Self::Requery | Self::LateRequery => 383,
            }
        }

        const fn name(self) -> &'static str {
            match self {
                Self::Save => "a save",
                Self::Launch => "a quantised launch",
                Self::Requery => "a control requery",
                Self::LateRequery => "a control requery published late",
            }
        }

        /// The handover and the time it runs at, with `again` as the text a
        /// save or a launch writes. The time is half a frame before the
        /// block's start: the device clock counts nanoseconds, and a time
        /// on the start itself can read as not yet reached.
        fn handover(self, again: &str) -> (Handover<'_>, f64) {
            let at = ((self.block() * BLOCK as u64) as f64 - 0.5) / f64::from(RATE);
            let handover = match self {
                Self::Save => Handover::Save(again),
                Self::Launch => Handover::Launch(again, TAKEOVER_FRAME as f64 / f64::from(RATE)),
                Self::Requery => Handover::Requery,
                Self::LateRequery => Handover::LateRequery(4),
            };
            (handover, at)
        }
    }

    /// Where the chords of a score lie against the device frames.
    #[derive(Clone, Copy)]
    struct Chords {
        /// The `.early(..)` call on the score, in cycles. At the default
        /// half cycle a second, 0.000003125 cycle is 0.3 frame.
        early: &'static str,
        /// Frames the transport starts before the schedule lead.
        early_frames: u32,
        /// Where that puts the chord nearest the takeover.
        place: &'static str,
    }

    impl Chords {
        /// The chord at 49279.7. Its audio frame is the takeover frame, so
        /// the device drops the outgoing copy.
        const IN_THE_LAST_FRAME_BEFORE: Self = Self {
            early: ".early(0.000003125)",
            early_frames: 0,
            place: "in the last frame before the takeover frame",
        };
        /// The chord at 49279, on a simple cycle position. The device keeps
        /// the outgoing copy. The hazard: `Fraction::from_f64` approximates
        /// the cycle of the takeover frame's edge to that same position, and
        /// a cursor placed there sounds the chord again.
        const ONE_FRAME_BEFORE: Self = Self {
            early: "",
            early_frames: 1,
            place: "one frame before the takeover frame",
        };
        const ON_THE_FRAME: Self = Self {
            early: "",
            early_frames: 0,
            place: "on the takeover frame",
        };
        const AFTER_THE_FRAME: Self = Self {
            early: ".early(-0.000003125)",
            early_frames: 0,
            place: "0.3 frame after the takeover frame",
        };

        /// Sixteen chords a cycle: one every 6000 frames from the transport
        /// start, which the schedule lead puts on frame 1280. `more` is
        /// chained on last.
        fn score(self, more: &str) -> String {
            format!(
                r#"$: s("supersaw").dec(0.4).seg(16).note("<<e1,g2,c3>!4 <d2,f#3,g2>!2 <d2,g3,g2>!2>"){}{more}"#,
                self.early
            )
        }

        fn played(self, score: &str, via: Option<Via>) -> Played {
            let again = format!("{score}\n// again\n");
            let handover = via.map(|via| via.handover(&again));
            let played = played(score, self.early_frames, handover, SECONDS);
            if let Some(via) = via {
                assert_eq!(
                    played.takeovers,
                    [TAKEOVER_FRAME],
                    "test premise: {} takes over on frame {TAKEOVER_FRAME}",
                    via.name()
                );
            }
            played
        }
    }

    /// The render with each of `handovers` is the render without one.
    fn assert_the_audio_is_kept(chords: Chords, handovers: &[Via]) {
        assert_the_audio_of_is_kept(chords, "", handovers);
    }

    /// [`assert_the_audio_is_kept`] for the chords with `more` chained on.
    fn assert_the_audio_of_is_kept(chords: Chords, more: &str, handovers: &[Via]) {
        let score = chords.score(more);
        let alone = chords.played(&score, None);
        assert!(
            alone.pcm.iter().any(|sample| sample.abs() > 0.1),
            "test premise: the score sounds"
        );
        for handover in handovers {
            let handed_over = chords.played(&score, Some(*handover));
            let (seconds, difference) = furthest_apart(&handed_over.pcm, &alone.pcm);
            assert!(
                difference < 1e-4,
                "{} with a chord {} changed the audio by {difference} at {seconds:.4} s",
                handover.name(),
                chords.place
            );
        }
    }

    /// The incoming generation plays an onset in the last frame before the
    /// takeover frame, because the device drops the outgoing copy. If the
    /// scheduler compares it with the takeover time, the chord is silent.
    #[test]
    fn a_save_keeps_the_onset_in_the_last_frame_before_its_takeover() {
        assert_the_audio_is_kept(Chords::IN_THE_LAST_FRAME_BEFORE, &[Via::Save]);
    }

    #[test]
    fn a_control_requery_keeps_the_onset_in_the_last_frame_before_its_takeover() {
        assert_the_audio_is_kept(Chords::IN_THE_LAST_FRAME_BEFORE, &[Via::Requery]);
    }

    /// A launch that does not rewind queries the score it launches from its
    /// line, and the line is a frame on the device too.
    #[test]
    fn a_quantised_launch_keeps_the_onset_in_the_last_frame_before_its_takeover() {
        assert_the_audio_is_kept(Chords::IN_THE_LAST_FRAME_BEFORE, &[Via::Launch]);
    }

    /// The onsets beside the frame edge keep their one copy too. One frame
    /// before the takeover frame the copy is the outgoing generation's, and
    /// the incoming one must not add its own.
    #[test]
    fn a_handover_does_not_double_the_onsets_beside_its_takeover_frame() {
        assert_the_audio_is_kept(Chords::ONE_FRAME_BEFORE, &Via::EXACT_ONE_FRAME_BEFORE);
        for chords in [Chords::ON_THE_FRAME, Chords::AFTER_THE_FRAME] {
            assert_the_audio_is_kept(chords, &Via::ALL);
        }
    }

    /// A control requery can publish after its takeover frame. The device
    /// has then started the outgoing chord on that frame, and the incoming
    /// generation sends the chord again, late. The device leaves it out.
    #[test]
    fn a_control_requery_published_late_sounds_each_onset_once() {
        assert_the_audio_is_kept(Chords::ON_THE_FRAME, &[Via::LateRequery]);
        assert_the_audio_is_kept(Chords::AFTER_THE_FRAME, &[Via::LateRequery]);
    }

    /// A stretched voice is aimed a vocoder's latency before its onset. For
    /// an onset on the takeover frame or the frame after it, that target
    /// frame is before the takeover, so the device keeps the outgoing copy.
    /// The session leaves the incoming copy out: the chord sounds once.
    #[test]
    fn a_handover_sounds_a_stretched_onset_on_its_takeover_once() {
        assert_the_audio_of_is_kept(Chords::ON_THE_FRAME, ".stretch(1)", &Via::ALL);
        for chords in [Chords::IN_THE_LAST_FRAME_BEFORE, Chords::AFTER_THE_FRAME] {
            assert_the_audio_of_is_kept(chords, ".stretch(1)", &[Via::Save, Via::Requery]);
        }
    }

    /// A save removes or adds `.stretch(1)` with a chord on the takeover
    /// frame, the ninth. The device keeps an outgoing stretched copy of it
    /// and drops a plain one, so that chord sounds once, as the reference.
    #[test]
    fn a_save_that_changes_the_stretch_sounds_the_onset_on_its_takeover_once() {
        let chords = Chords::ON_THE_FRAME;
        let lane = chords.score("");
        let lane = lane.strip_prefix("$: ").expect("a labelled lane");
        let (_, at) = Via::Save.handover("");
        for (before, after, kept) in [(".stretch(1)", "", 9), ("", ".stretch(1)", 8)] {
            // The reference plays the first `kept` chords of the cycle as
            // the score before the save, and the others as the saved one.
            let left = 16 - kept;
            let outgoing = format!(r#"{lane}{before}.mask("1!{kept} 0!{left}")"#);
            let incoming = format!(r#"{lane}{after}.mask("0!{kept} 1!{left}")"#);
            let reference = format!("$: stack({outgoing}, {incoming})");
            let reference = played(&reference, 0, None, SECONDS);
            assert!(
                reference.pcm.iter().any(|sample| sample.abs() > 0.1),
                "test premise: the score sounds"
            );
            let (score, save) = (chords.score(before), chords.score(after));
            let saved = played(&score, 0, Some((Handover::Save(&save), at)), SECONDS);
            assert_eq!(saved.takeovers, [TAKEOVER_FRAME], "test premise");
            let (seconds, difference) = furthest_apart(&saved.pcm, &reference.pcm);
            assert!(
                difference < 1e-4,
                "the save of `{before}` as `{after}` changed the audio by {difference} \
                 at {seconds:.4} s"
            );
        }
    }

    /// MIDI follows the audio across a takeover: its bridge prunes the
    /// outgoing generation from the same frame, and the onsets it is handed
    /// come from the same schedule. So every onset keeps one MIDI onset,
    /// the one in the last frame before the takeover frame included.
    #[cfg(feature = "midi")]
    #[test]
    fn a_handover_leaves_each_onset_one_midi_onset() {
        for (chords, handovers) in [
            (Chords::IN_THE_LAST_FRAME_BEFORE, &Via::ALL[..]),
            (Chords::ONE_FRAME_BEFORE, &Via::EXACT_ONE_FRAME_BEFORE[..]),
        ] {
            let score = chords.score(r#".midi("out")"#);
            let alone = chords.played(&score, None);
            assert!(
                alone.midi_frames.contains(&TAKEOVER_FRAME)
                    || alone.midi_frames.contains(&(TAKEOVER_FRAME - 1)),
                "test premise: the chords reach MIDI: {:?}",
                alone.midi_frames
            );
            for handover in handovers {
                let handed_over = chords.played(&score, Some(*handover));
                assert_eq!(
                    handed_over.midi_frames,
                    alone.midi_frames,
                    "{} with a chord {}",
                    handover.name(),
                    chords.place
                );
            }
        }
    }

    /// With each handover, the chords with `output` chained on hand out the
    /// same messages as with none. `sent` reads them from a pass.
    #[cfg(any(feature = "osc", feature = "serial"))]
    fn assert_each_message_goes_out_once(output: &str, sent: fn(Played) -> Vec<(u64, u64)>) {
        let frames = |played: Played| -> Vec<u64> {
            let mut frames: Vec<u64> = sent(played).iter().map(|(_, frame)| *frame).collect();
            frames.sort_unstable();
            frames
        };
        for chords in [
            Chords::IN_THE_LAST_FRAME_BEFORE,
            Chords::ONE_FRAME_BEFORE,
            Chords::ON_THE_FRAME,
            Chords::AFTER_THE_FRAME,
        ] {
            let score = chords.score(output);
            let alone = frames(chords.played(&score, None));
            assert!(
                alone.iter().any(|frame| *frame > TAKEOVER_FRAME),
                "test premise: the chords reach the output: {alone:?}"
            );
            for handover in [Via::Save, Via::Launch, Via::Requery, Via::LateRequery] {
                let handed_over = frames(chords.played(&score, Some(handover)));
                assert_eq!(
                    handed_over,
                    alone,
                    "{} with a chord {}",
                    handover.name(),
                    chords.place
                );
            }
        }
    }

    /// A host sends an OSC bundle as soon as it is staged, about half a
    /// second ahead, and cannot recall it. The incoming generation leaves
    /// out the onsets a bundle is already out for: each goes out once.
    #[cfg(feature = "osc")]
    #[test]
    fn a_handover_sends_each_osc_bundle_once() {
        assert_each_message_goes_out_once(".osc(57120)", |played| played.osc);
    }

    /// A serial write waits in its sender with no generation to prune it
    /// by. Each onset is written once across a handover.
    #[cfg(feature = "serial")]
    #[test]
    fn a_handover_sends_each_serial_write_once() {
        assert_each_message_goes_out_once(r#".serial(115200, true, false, "out")"#, |played| {
            played.serial
        });
    }

    /// A rewind plays the saved score from its beginning and takes over
    /// with a cut: a new timeline. Each chord of it goes out from the first,
    /// although the outgoing score has messages out past the takeover.
    #[cfg(any(feature = "osc", feature = "serial"))]
    fn assert_a_rewind_sends_every_message(output: &str, sent: fn(Played) -> Vec<(u64, u64)>) {
        let chords = Chords::ON_THE_FRAME;
        let score = chords.score(output);
        let again = format!("{score}\n// again\n");
        let (_, at) = Via::Save.handover("");
        let played = played(&score, 0, Some((Handover::Rewind(&again), at)), SECONDS);
        let [takeover] = played.takeovers[..] else {
            panic!("test premise: one takeover: {:?}", played.takeovers);
        };
        let sent = sent(played);
        let newest = sent.iter().map(|(generation, _)| *generation).max();
        // The frames of the rewound score's messages, or of the others.
        let frames_of = |rewound: bool| -> Vec<u64> {
            let of = sent
                .iter()
                .filter(|(generation, _)| (Some(*generation) == newest) == rewound);
            of.map(|(_, frame)| *frame).collect()
        };
        let (outgoing, rewound) = (frames_of(false), frames_of(true));
        let first = *rewound
            .first()
            .expect("the rewound score reaches the output");
        assert!(
            first.abs_diff(takeover) <= 1,
            "the rewound score starts on its takeover frame {takeover}: {rewound:?}"
        );
        assert!(
            outgoing.iter().any(|frame| *frame > first + 6_000),
            "test premise: the outgoing score has messages out past the takeover: {outgoing:?}"
        );
        // Three notes a chord, one chord every 6000 frames.
        for (index, frame) in rewound.iter().enumerate() {
            assert_eq!(
                *frame,
                first + 6_000 * (index as u64 / 3),
                "message {index} of the rewound score: {rewound:?}"
            );
        }
    }

    #[cfg(feature = "osc")]
    #[test]
    fn a_rewind_sends_every_osc_bundle_of_the_new_timeline() {
        assert_a_rewind_sends_every_message(".osc(57120)", |played| played.osc);
    }

    #[cfg(feature = "serial")]
    #[test]
    fn a_rewind_sends_every_serial_write_of_the_new_timeline() {
        assert_a_rewind_sends_every_message(r#".serial(115200, true, false, "out")"#, |played| {
            played.serial
        });
    }

    /// A loop of sixteen onsets a cycle, one every 6000 frames, and a
    /// keyboard beside it, both with `output` chained on.
    #[cfg(any(feature = "osc", feature = "serial"))]
    fn loop_and_keyboard(output: &str) -> String {
        format!(
            "const kb = await midikeys('keyboard')\n\
             $: stack(s(\"sawtooth*16\").note(\"c2\"), kb(0.05).s(\"triangle\")){output}"
        )
    }

    /// With `handovers`, each a key press or a control requery, the loop
    /// hands out the messages it hands out with none. Each press hands out
    /// one more, on the takeover frame of its requery.
    #[cfg(any(feature = "osc", feature = "serial"))]
    fn assert_each_key_and_onset_goes_out_once(
        output: &str,
        sent: fn(Played) -> Vec<(u64, u64)>,
        handovers: &[(Handover<'_>, f64)],
    ) {
        use super::hand_rendered::played_each;
        let frames = |sent: Vec<(u64, u64)>| -> Vec<u64> {
            let mut frames: Vec<u64> = sent.iter().map(|(_, frame)| *frame).collect();
            frames.sort_unstable();
            frames
        };
        let score = loop_and_keyboard(output);
        let alone = frames(sent(played(&score, 0, None, SECONDS)));
        assert!(
            alone.len() > 12,
            "test premise: the loop reaches the output: {alone:?}"
        );
        let with_keys = played_each(&score, 0, handovers.to_vec(), SECONDS);
        let takeovers = with_keys.takeovers.clone();
        assert_eq!(
            takeovers.len(),
            handovers.len(),
            "test premise: each requery takes over: {takeovers:?}"
        );
        let pressed = handovers
            .iter()
            .zip(&takeovers)
            .filter(|((handover, _), _)| matches!(handover, Handover::Press(_)))
            .map(|(_, takeover)| *takeover);
        let mut expected: Vec<u64> = alone.iter().copied().chain(pressed).collect();
        expected.sort_unstable();
        assert!(
            expected.len() > alone.len(),
            "test premise: a key is pressed"
        );
        assert_eq!(
            frames(sent(with_keys)),
            expected,
            "the takeovers are on {takeovers:?}"
        );
    }

    /// The key presses a played test makes: one key, two keys one device
    /// block apart, and a key between the requeries of a slider that moves,
    /// one every 120 ms.
    #[cfg(any(feature = "osc", feature = "serial"))]
    fn assert_keys_over_a_loop_go_out_once(output: &str, sent: fn(Played) -> Vec<(u64, u64)>) {
        let block = BLOCK as f64 / f64::from(RATE);
        let one_key = [(Handover::Press(60), 0.9)];
        let two_keys = [
            (Handover::Press(60), 0.9),
            (Handover::Press(64), 0.9 + block),
        ];
        let under_a_slider = [
            (Handover::Requery, 0.7),
            (Handover::Requery, 0.82),
            (Handover::Press(60), 0.9),
            (Handover::Requery, 0.94),
            (Handover::Requery, 1.06),
        ];
        assert_each_key_and_onset_goes_out_once(output, sent, &one_key);
        assert_each_key_and_onset_goes_out_once(output, sent, &two_keys);
        assert_each_key_and_onset_goes_out_once(output, sent, &under_a_slider);
    }

    /// A key played over a running loop has no bundle out, although the
    /// loop's bundles are out past its takeover. The key's bundle goes out
    /// once, and the loop's bundles still go out once each.
    #[cfg(feature = "osc")]
    #[test]
    fn a_key_over_a_running_loop_sends_each_osc_bundle_once() {
        assert_keys_over_a_loop_go_out_once(".osc(57120)", |played| played.osc);
    }

    #[cfg(feature = "serial")]
    #[test]
    fn a_key_over_a_running_loop_sends_each_serial_write_once() {
        assert_keys_over_a_loop_go_out_once(r#".serial(115200, true, false, "out")"#, |played| {
            played.serial
        });
    }

    /// A control requery can publish after its takeover frame. The port has
    /// then sent the outgoing chord on that frame, and the incoming generation
    /// hands it over again. The bridge leaves that copy out: each note once.
    #[cfg(feature = "midi")]
    #[test]
    fn a_control_requery_published_late_sends_each_midi_note_once() {
        for (chords, handovers) in [
            (
                Chords::ON_THE_FRAME,
                &[Via::LateRequery, Via::Requery, Via::Save][..],
            ),
            (Chords::AFTER_THE_FRAME, &[Via::LateRequery][..]),
        ] {
            let score = chords.score(r#".midi("out")"#);
            let alone = chords.played(&score, None);
            assert!(
                !alone.midi_note_ons.is_empty(),
                "test premise: the chords reach the port"
            );
            for handover in handovers {
                let handed_over = chords.played(&score, Some(*handover));
                assert_eq!(
                    handed_over.midi_note_ons,
                    alone.midi_note_ons,
                    "{} with a chord {}",
                    handover.name(),
                    chords.place
                );
            }
        }
    }
}
