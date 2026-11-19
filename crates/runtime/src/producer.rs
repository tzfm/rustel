//! Bounded producer-thread load and phase telemetry.

#[cfg(any(feature = "device-audio", test))]
use std::time::Duration;

pub const PRODUCER_PHASE_COUNT: usize = 10;
pub const PRODUCER_REPORTING_WINDOW_TURNS: u64 = 256;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum ProducerPhase {
    Evaluation,
    ReplacementProbe,
    Query,
    JsCallbacks,
    Scheduler,
    Conversion,
    AssetPreparation,
    RingPush,
    Trace,
    Total,
}

impl ProducerPhase {
    pub const ALL: [Self; PRODUCER_PHASE_COUNT] = [
        Self::Evaluation,
        Self::ReplacementProbe,
        Self::Query,
        Self::JsCallbacks,
        Self::Scheduler,
        Self::Conversion,
        Self::AssetPreparation,
        Self::RingPush,
        Self::Trace,
        Self::Total,
    ];

    const fn index(self) -> usize {
        self as usize
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ProducerTurnOutcome {
    /// The queried frontier advanced and added future music.
    Progress,
    /// No query was needed because the horizon was already covered.
    #[default]
    Idle,
    /// Work was refused before scheduler state committed.
    AtomicRefusal,
    /// Work failed after scheduler state may have committed.
    CommittedRefusal,
    Cancelled,
    Stopped,
}

/// One producer turn reduced to bounded scalar observations.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg(any(feature = "device-audio", test))]
pub(crate) struct ProducerTurnRecord {
    pub phases_nanos: [u64; PRODUCER_PHASE_COUNT],
    pub outcome: ProducerTurnOutcome,
    pub cover_start_nanos: u64,
    pub cover_end_nanos: u64,
    pub cover_gained_nanos: u64,
    pub minimum_grid_nanos: u64,
    pub continuation_reserve_nanos: u64,
    pub budget_granted_nanos: u64,
    pub query_span_millicycles: u64,
    pub query_span_nanos: u64,
    pub haps: u64,
    pub scheduler_events: u64,
    pub converted_audio_events: u64,
    pub ring_pushes: u64,
    pub js_callback_calls: u64,
    pub js_callback_kind_sample: Option<rustel_core::CallbackQueryKindCounts>,
    pub refused_voices: u64,
    pub producer_backlog_depth: u32,
    pub producer_backlog_capacity: u32,
    pub scheduler_queue_depth: u32,
    pub scheduler_queue_capacity: u32,
    pub scheduler_trace_depth: u32,
    pub scheduler_trace_capacity: u32,
    pub scheduler_trace_drops: u64,
    pub queue_saturated: bool,
    pub rollback_count: u64,
    pub gap_resync_count: u64,
    pub atomic_refusal_count: u64,
    pub committed_refusal_count: u64,
    /// Of the committed refusal above, whether the score's own content was
    /// the reason. It still latches and still rolls back; it is simply not
    /// the engine falling behind, and the health readings subtract it.
    pub rejected_score_count: u64,
    /// Of the atomic refusal above, whether the sample library was still
    /// fetching what the score asked for. The engine turned nothing away;
    /// the health readings subtract it the same way.
    pub loading_refusal_count: u64,
    /// Time elapsed after the audio clock sample used to measure `cover_end`.
    pub cover_age_nanos: u64,
}

#[cfg(any(feature = "device-audio", test))]
impl Default for ProducerTurnRecord {
    fn default() -> Self {
        Self {
            phases_nanos: [0; PRODUCER_PHASE_COUNT],
            outcome: ProducerTurnOutcome::Idle,
            cover_start_nanos: 0,
            cover_end_nanos: 0,
            cover_gained_nanos: 0,
            minimum_grid_nanos: 0,
            continuation_reserve_nanos: 0,
            budget_granted_nanos: 0,
            query_span_millicycles: 0,
            query_span_nanos: 0,
            haps: 0,
            scheduler_events: 0,
            converted_audio_events: 0,
            ring_pushes: 0,
            js_callback_calls: 0,
            js_callback_kind_sample: None,
            refused_voices: 0,
            producer_backlog_depth: 0,
            producer_backlog_capacity: 0,
            scheduler_queue_depth: 0,
            scheduler_queue_capacity: 0,
            scheduler_trace_depth: 0,
            scheduler_trace_capacity: 0,
            scheduler_trace_drops: 0,
            queue_saturated: false,
            rollback_count: 0,
            gap_resync_count: 0,
            atomic_refusal_count: 0,
            committed_refusal_count: 0,
            rejected_score_count: 0,
            loading_refusal_count: 0,
            cover_age_nanos: 0,
        }
    }
}

#[cfg(any(feature = "device-audio", test))]
impl ProducerTurnRecord {
    pub(crate) fn add_phase(&mut self, phase: ProducerPhase, duration: Duration) {
        self.phases_nanos[phase.index()] =
            self.phases_nanos[phase.index()].saturating_add(duration_nanos(duration));
    }

    pub(crate) fn set_phase_nanos(&mut self, phase: ProducerPhase, nanos: u64) {
        self.phases_nanos[phase.index()] = nanos;
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ProducerPhaseSnapshot {
    pub samples: u64,
    pub last_nanos: u64,
    pub total_nanos: u64,
    pub p50_nanos: u64,
    pub p95_nanos: u64,
    pub p99_nanos: u64,
    pub max_nanos: u64,
}

/// Producer telemetry copied at a producer boundary.
///
/// Percentiles use a fixed log2 nanosecond histogram. They are bounded upper
/// estimates rather than retained raw samples; telemetry memory therefore
/// does not grow with set duration or score complexity.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProducerLoadSnapshot {
    pub publication: u64,
    pub turns: u64,
    pub window_index: u64,
    pub window_turns: u64,
    pub progress_turns: u64,
    pub idle_turns: u64,
    pub atomic_refusals: u64,
    pub committed_refusals: u64,
    /// The subset of `committed_refusals` a score's own content caused: an
    /// orbit past the last bus, a sound with no bank. The pressure monitor
    /// leaves these out of its refusal count. Deliberately not in the v1
    /// report: that schema is pinned key for key, and this is an internal
    /// reading rather than something a consumer asked for.
    pub rejected_scores: u64,
    /// The subset of `atomic_refusals` that was the sample library still
    /// fetching what a score asked for: waiting, not falling behind. The
    /// pressure monitor leaves these out too, for the same reason and with
    /// the same report omission.
    pub loading_refusals: u64,
    pub cancelled_turns: u64,
    pub stopped_turns: u64,
    pub rollback_count: u64,
    pub gap_resync_count: u64,
    pub queue_saturation_count: u64,
    pub last_outcome: ProducerTurnOutcome,
    pub last_load_basis_points: u32,
    pub fast_load_basis_points: u32,
    pub slow_load_basis_points: u32,
    pub peak_load_basis_points: u32,
    pub consecutive_over_budget: u64,
    pub cover_start_nanos: u64,
    pub cover_end_nanos: u64,
    pub cover_low_water_nanos: u64,
    pub cover_gained_nanos: u64,
    pub continuation_reserve_nanos: u64,
    pub budget_granted_nanos: u64,
    pub query_span_millicycles: u64,
    pub query_span_nanos: u64,
    pub haps: u64,
    pub scheduler_events: u64,
    pub converted_audio_events: u64,
    pub ring_pushes: u64,
    pub js_callback_calls: u64,
    pub js_callback_kind_samples: u64,
    pub js_callback_kind_sample: Option<rustel_core::CallbackQueryKindCounts>,
    pub refused_voices: u64,
    pub producer_backlog_depth: u32,
    pub peak_producer_backlog_depth: u32,
    pub producer_backlog_capacity: u32,
    pub scheduler_queue_depth: u32,
    pub peak_scheduler_queue_depth: u32,
    pub scheduler_queue_capacity: u32,
    pub scheduler_trace_depth: u32,
    pub peak_scheduler_trace_depth: u32,
    pub scheduler_trace_capacity: u32,
    pub scheduler_trace_drops: u64,
    pub phases: [ProducerPhaseSnapshot; PRODUCER_PHASE_COUNT],
}

impl Default for ProducerLoadSnapshot {
    fn default() -> Self {
        Self {
            publication: 0,
            turns: 0,
            window_index: 0,
            window_turns: 0,
            progress_turns: 0,
            idle_turns: 0,
            atomic_refusals: 0,
            committed_refusals: 0,
            rejected_scores: 0,
            loading_refusals: 0,
            cancelled_turns: 0,
            stopped_turns: 0,
            rollback_count: 0,
            gap_resync_count: 0,
            queue_saturation_count: 0,
            last_outcome: ProducerTurnOutcome::Idle,
            last_load_basis_points: 0,
            fast_load_basis_points: 0,
            slow_load_basis_points: 0,
            peak_load_basis_points: 0,
            consecutive_over_budget: 0,
            cover_start_nanos: 0,
            cover_end_nanos: 0,
            cover_low_water_nanos: 0,
            cover_gained_nanos: 0,
            continuation_reserve_nanos: 0,
            budget_granted_nanos: 0,
            query_span_millicycles: 0,
            query_span_nanos: 0,
            haps: 0,
            scheduler_events: 0,
            converted_audio_events: 0,
            ring_pushes: 0,
            js_callback_calls: 0,
            js_callback_kind_samples: 0,
            js_callback_kind_sample: None,
            refused_voices: 0,
            producer_backlog_depth: 0,
            peak_producer_backlog_depth: 0,
            producer_backlog_capacity: 0,
            scheduler_queue_depth: 0,
            peak_scheduler_queue_depth: 0,
            scheduler_queue_capacity: 0,
            scheduler_trace_depth: 0,
            peak_scheduler_trace_depth: 0,
            scheduler_trace_capacity: 0,
            scheduler_trace_drops: 0,
            phases: [ProducerPhaseSnapshot::default(); PRODUCER_PHASE_COUNT],
        }
    }
}

impl ProducerLoadSnapshot {
    pub fn phase(&self, phase: ProducerPhase) -> ProducerPhaseSnapshot {
        self.phases[phase.index()]
    }

    /// Refusals that say the engine could not keep up, cumulative. A
    /// rejected score (a mistake in the score) and a sample still loading
    /// (waiting, not falling behind) are left out; the pressure monitor and
    /// the studio's audio hint both count by this one rule.
    pub fn capacity_refusals(&self) -> u64 {
        self.atomic_refusals
            .saturating_sub(self.loading_refusals)
            .saturating_add(self.committed_refusals.saturating_sub(self.rejected_scores))
    }
}

#[cfg(any(feature = "device-audio", test))]
const HISTOGRAM_BUCKETS: usize = 65;

#[derive(Clone, Copy, Debug)]
#[cfg(any(feature = "device-audio", test))]
struct PhaseHistogram {
    buckets: [u64; HISTOGRAM_BUCKETS],
    samples: u64,
    last: u64,
    total: u64,
    max: u64,
}

#[cfg(any(feature = "device-audio", test))]
impl Default for PhaseHistogram {
    fn default() -> Self {
        Self {
            buckets: [0; HISTOGRAM_BUCKETS],
            samples: 0,
            last: 0,
            total: 0,
            max: 0,
        }
    }
}

#[cfg(any(feature = "device-audio", test))]
impl PhaseHistogram {
    fn record(&mut self, nanos: u64) {
        let bucket = histogram_bucket(nanos);
        self.buckets[bucket] = self.buckets[bucket].saturating_add(1);
        self.samples = self.samples.saturating_add(1);
        self.last = nanos;
        self.total = self.total.saturating_add(nanos);
        self.max = self.max.max(nanos);
    }

    fn snapshot(&self) -> ProducerPhaseSnapshot {
        ProducerPhaseSnapshot {
            samples: self.samples,
            last_nanos: self.last,
            total_nanos: self.total,
            p50_nanos: self.percentile(50),
            p95_nanos: self.percentile(95),
            p99_nanos: self.percentile(99),
            max_nanos: self.max,
        }
    }

    fn percentile(&self, percentile: u64) -> u64 {
        if self.samples == 0 {
            return 0;
        }
        let target = self.samples.saturating_mul(percentile).saturating_add(99) / 100;
        let mut seen = 0u64;
        for (bucket, count) in self.buckets.iter().copied().enumerate() {
            seen = seen.saturating_add(count);
            if seen >= target {
                return histogram_upper_bound(bucket);
            }
        }
        u64::MAX
    }
}

#[derive(Debug)]
#[cfg(any(feature = "device-audio", test))]
pub(crate) struct ProducerLoadMeter {
    snapshot: ProducerLoadSnapshot,
    phases: [PhaseHistogram; PRODUCER_PHASE_COUNT],
    has_load: bool,
    has_cover: bool,
}

#[cfg(any(feature = "device-audio", test))]
impl Default for ProducerLoadMeter {
    fn default() -> Self {
        Self {
            snapshot: ProducerLoadSnapshot::default(),
            phases: [PhaseHistogram::default(); PRODUCER_PHASE_COUNT],
            has_load: false,
            has_cover: false,
        }
    }
}

#[cfg(any(feature = "device-audio", test))]
impl ProducerLoadMeter {
    pub(crate) fn record(&mut self, record: ProducerTurnRecord) {
        if self.snapshot.window_turns >= PRODUCER_REPORTING_WINDOW_TURNS {
            self.snapshot.window_index = self.snapshot.window_index.saturating_add(1);
            self.snapshot.window_turns = 0;
            self.snapshot.cover_low_water_nanos = 0;
            self.snapshot.peak_load_basis_points = 0;
            self.snapshot.peak_producer_backlog_depth = 0;
            self.snapshot.peak_scheduler_queue_depth = 0;
            self.snapshot.peak_scheduler_trace_depth = 0;
            self.phases = [PhaseHistogram::default(); PRODUCER_PHASE_COUNT];
            self.has_cover = false;
        }
        self.snapshot.publication = self.snapshot.publication.saturating_add(1);
        self.snapshot.turns = self.snapshot.turns.saturating_add(1);
        self.snapshot.window_turns = self.snapshot.window_turns.saturating_add(1);
        self.snapshot.last_outcome = record.outcome;
        self.snapshot.atomic_refusals = self
            .snapshot
            .atomic_refusals
            .saturating_add(record.atomic_refusal_count);
        self.snapshot.committed_refusals = self
            .snapshot
            .committed_refusals
            .saturating_add(record.committed_refusal_count);
        self.snapshot.rejected_scores = self
            .snapshot
            .rejected_scores
            .saturating_add(record.rejected_score_count);
        self.snapshot.loading_refusals = self
            .snapshot
            .loading_refusals
            .saturating_add(record.loading_refusal_count);
        match record.outcome {
            ProducerTurnOutcome::Progress => {
                self.snapshot.progress_turns = self.snapshot.progress_turns.saturating_add(1);
                let denominator = record
                    .cover_gained_nanos
                    .max(record.minimum_grid_nanos)
                    .max(1);
                let total = record.phases_nanos[ProducerPhase::Total.index()];
                let load = ratio_basis_points(total, denominator);
                self.snapshot.last_load_basis_points = load;
                if self.has_load {
                    self.snapshot.fast_load_basis_points =
                        ewma(self.snapshot.fast_load_basis_points, load, 4);
                    self.snapshot.slow_load_basis_points =
                        ewma(self.snapshot.slow_load_basis_points, load, 32);
                } else {
                    self.snapshot.fast_load_basis_points = load;
                    self.snapshot.slow_load_basis_points = load;
                    self.has_load = true;
                }
                self.snapshot.peak_load_basis_points =
                    self.snapshot.peak_load_basis_points.max(load);
                if load > 10_000 {
                    self.snapshot.consecutive_over_budget =
                        self.snapshot.consecutive_over_budget.saturating_add(1);
                } else {
                    self.snapshot.consecutive_over_budget = 0;
                }
            }
            ProducerTurnOutcome::Idle => {
                self.snapshot.idle_turns = self.snapshot.idle_turns.saturating_add(1);
                self.snapshot.last_load_basis_points = 0;
            }
            ProducerTurnOutcome::AtomicRefusal | ProducerTurnOutcome::CommittedRefusal => {
                self.snapshot.last_load_basis_points = 0;
            }
            ProducerTurnOutcome::Cancelled => {
                self.snapshot.cancelled_turns = self.snapshot.cancelled_turns.saturating_add(1);
                self.snapshot.last_load_basis_points = 0;
            }
            ProducerTurnOutcome::Stopped => {
                self.snapshot.stopped_turns = self.snapshot.stopped_turns.saturating_add(1);
                self.snapshot.last_load_basis_points = 0;
            }
        }

        self.snapshot.rollback_count = self
            .snapshot
            .rollback_count
            .saturating_add(record.rollback_count);
        self.snapshot.gap_resync_count = self
            .snapshot
            .gap_resync_count
            .saturating_add(record.gap_resync_count);
        if record.queue_saturated {
            self.snapshot.queue_saturation_count =
                self.snapshot.queue_saturation_count.saturating_add(1);
        }
        self.snapshot.cover_start_nanos = record.cover_start_nanos;
        self.snapshot.cover_end_nanos = record.cover_end_nanos;
        self.snapshot.cover_gained_nanos = record.cover_gained_nanos;
        let low = record.cover_start_nanos.min(record.cover_end_nanos);
        self.snapshot.cover_low_water_nanos = if self.has_cover {
            self.snapshot.cover_low_water_nanos.min(low)
        } else {
            self.has_cover = true;
            low
        };
        self.snapshot.continuation_reserve_nanos = record.continuation_reserve_nanos;
        self.snapshot.budget_granted_nanos = record.budget_granted_nanos;
        self.snapshot.query_span_millicycles = record.query_span_millicycles;
        self.snapshot.query_span_nanos = record.query_span_nanos;
        self.snapshot.haps = record.haps;
        self.snapshot.scheduler_events = record.scheduler_events;
        self.snapshot.converted_audio_events = record.converted_audio_events;
        self.snapshot.ring_pushes = record.ring_pushes;
        self.snapshot.js_callback_calls = record.js_callback_calls;
        if let Some(sample) = record.js_callback_kind_sample {
            self.snapshot.js_callback_kind_samples =
                self.snapshot.js_callback_kind_samples.saturating_add(1);
            self.snapshot.js_callback_kind_sample = Some(sample);
        }
        self.snapshot.refused_voices = record.refused_voices;
        self.snapshot.producer_backlog_depth = record.producer_backlog_depth;
        self.snapshot.peak_producer_backlog_depth = self
            .snapshot
            .peak_producer_backlog_depth
            .max(record.producer_backlog_depth);
        self.snapshot.producer_backlog_capacity = record.producer_backlog_capacity;
        self.snapshot.scheduler_queue_depth = record.scheduler_queue_depth;
        self.snapshot.peak_scheduler_queue_depth = self
            .snapshot
            .peak_scheduler_queue_depth
            .max(record.scheduler_queue_depth);
        self.snapshot.scheduler_queue_capacity = record.scheduler_queue_capacity;
        self.snapshot.scheduler_trace_depth = record.scheduler_trace_depth;
        self.snapshot.peak_scheduler_trace_depth = self
            .snapshot
            .peak_scheduler_trace_depth
            .max(record.scheduler_trace_depth);
        self.snapshot.scheduler_trace_capacity = record.scheduler_trace_capacity;
        self.snapshot.scheduler_trace_drops = record.scheduler_trace_drops;

        for phase in ProducerPhase::ALL {
            let nanos = record.phases_nanos[phase.index()];
            if nanos != 0 || phase == ProducerPhase::Total {
                self.phases[phase.index()].record(nanos);
            }
            self.snapshot.phases[phase.index()] = self.phases[phase.index()].snapshot();
        }
    }

    pub(crate) fn snapshot(&self) -> ProducerLoadSnapshot {
        self.snapshot
    }
}

#[cfg(any(feature = "device-audio", test))]
pub(crate) fn duration_nanos(duration: Duration) -> u64 {
    duration.as_nanos().min(u128::from(u64::MAX)) as u64
}

#[cfg(any(feature = "device-audio", test))]
pub(crate) fn seconds_nanos(seconds: f64) -> u64 {
    if !seconds.is_finite() || seconds <= 0.0 {
        0
    } else {
        Duration::from_secs_f64(seconds.min(Duration::MAX.as_secs_f64()))
            .as_nanos()
            .min(u128::from(u64::MAX)) as u64
    }
}

#[cfg(any(feature = "device-audio", test))]
fn ratio_basis_points(numerator: u64, denominator: u64) -> u32 {
    let value = u128::from(numerator)
        .saturating_mul(10_000)
        .checked_div(u128::from(denominator.max(1)))
        .unwrap_or_default()
        .min(u128::from(u32::MAX));
    value as u32
}

#[cfg(any(feature = "device-audio", test))]
fn ewma(previous: u32, sample: u32, denominator: u64) -> u32 {
    let denominator = denominator.max(1);
    let previous = u64::from(previous);
    let sample = u64::from(sample);
    let weighted = previous
        .saturating_mul(denominator.saturating_sub(1))
        .saturating_add(sample)
        .saturating_add(denominator / 2)
        / denominator;
    weighted.min(u64::from(u32::MAX)) as u32
}

#[cfg(any(feature = "device-audio", test))]
fn histogram_bucket(value: u64) -> usize {
    if value == 0 {
        0
    } else {
        usize::try_from(u64::BITS - (value - 1).leading_zeros()).unwrap_or(HISTOGRAM_BUCKETS - 1)
    }
}

#[cfg(any(feature = "device-audio", test))]
fn histogram_upper_bound(bucket: usize) -> u64 {
    match bucket {
        0 => 0,
        64.. => u64::MAX,
        bucket => 1u64 << bucket,
    }
}

#[cfg(test)]
mod ewma_tests {
    use super::ewma;

    #[test]
    fn a_zero_denominator_uses_the_current_sample() {
        assert_eq!(ewma(10, 20, 0), 20);
        assert_eq!(ewma(100, 200, 0), 200);
        assert_eq!(ewma(u32::MAX, 0, 0), 0);
        assert_eq!(ewma(0, u32::MAX, 0), u32::MAX);
    }

    #[test]
    fn positive_denominators_keep_the_weighted_average() {
        assert_eq!(ewma(100, 200, 1), 200);
        assert_eq!(ewma(100, 200, 4), 125);
        assert_eq!(ewma(100, 200, 32), 103);
        assert_eq!(ewma(100, 100, 4), 100);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn successful_load_is_calibrated_against_cover_gained() {
        let mut meter = ProducerLoadMeter::default();
        let mut turn = ProducerTurnRecord {
            outcome: ProducerTurnOutcome::Progress,
            cover_gained_nanos: 10_000,
            minimum_grid_nanos: 1_000,
            ..ProducerTurnRecord::default()
        };
        turn.set_phase_nanos(ProducerPhase::Total, 5_000);
        meter.record(turn);

        let snapshot = meter.snapshot();
        assert_eq!(snapshot.last_load_basis_points, 5_000);
        assert_eq!(snapshot.fast_load_basis_points, 5_000);
        assert_eq!(snapshot.slow_load_basis_points, 5_000);
        assert_eq!(snapshot.progress_turns, 1);
    }

    #[test]
    fn idle_and_refused_turns_are_not_throughput_samples() {
        let mut meter = ProducerLoadMeter::default();
        let mut progress = ProducerTurnRecord {
            outcome: ProducerTurnOutcome::Progress,
            cover_gained_nanos: 10,
            minimum_grid_nanos: 10,
            ..ProducerTurnRecord::default()
        };
        progress.set_phase_nanos(ProducerPhase::Total, 5);
        meter.record(progress);
        meter.record(ProducerTurnRecord::default());
        meter.record(ProducerTurnRecord {
            outcome: ProducerTurnOutcome::AtomicRefusal,
            atomic_refusal_count: 1,
            ..ProducerTurnRecord::default()
        });

        let snapshot = meter.snapshot();
        assert_eq!(snapshot.progress_turns, 1);
        assert_eq!(snapshot.idle_turns, 1);
        assert_eq!(snapshot.atomic_refusals, 1);
        assert_eq!(snapshot.slow_load_basis_points, 5_000);
        assert_eq!(snapshot.last_load_basis_points, 0);
    }

    #[test]
    fn phase_percentiles_are_bounded_upper_estimates() {
        let mut histogram = PhaseHistogram::default();
        for value in [1, 2, 3, 4, 5, 8, 9, 16, 17, 32, 33, 64, 65] {
            histogram.record(value);
        }
        let snapshot = histogram.snapshot();
        assert_eq!(snapshot.samples, 13);
        assert!(snapshot.p50_nanos >= 9);
        assert!(snapshot.p50_nanos <= 16);
        assert!(snapshot.p95_nanos >= 64);
        assert!(snapshot.p99_nanos >= snapshot.p95_nanos);
        assert_eq!(snapshot.max_nanos, 65);
    }

    #[test]
    fn an_awaiting_samples_turn_refuses_but_waits() {
        let mut meter = ProducerLoadMeter::default();
        meter.record(ProducerTurnRecord {
            outcome: ProducerTurnOutcome::AtomicRefusal,
            atomic_refusal_count: 1,
            loading_refusal_count: 1,
            ..ProducerTurnRecord::default()
        });
        let snapshot = meter.snapshot();
        assert_eq!(snapshot.atomic_refusals, 1);
        assert_eq!(snapshot.loading_refusals, 1, "the wait is counted apart");
    }

    #[test]
    fn overload_streak_ignores_idle_but_resets_on_healthy_progress() {
        let mut meter = ProducerLoadMeter::default();
        for total in [11, 12] {
            let mut turn = ProducerTurnRecord {
                outcome: ProducerTurnOutcome::Progress,
                cover_gained_nanos: 10,
                minimum_grid_nanos: 10,
                ..ProducerTurnRecord::default()
            };
            turn.set_phase_nanos(ProducerPhase::Total, total);
            meter.record(turn);
        }
        meter.record(ProducerTurnRecord::default());
        assert_eq!(meter.snapshot().consecutive_over_budget, 2);

        let mut healthy = ProducerTurnRecord {
            outcome: ProducerTurnOutcome::Progress,
            cover_gained_nanos: 10,
            minimum_grid_nanos: 10,
            ..ProducerTurnRecord::default()
        };
        healthy.set_phase_nanos(ProducerPhase::Total, 9);
        meter.record(healthy);
        assert_eq!(meter.snapshot().consecutive_over_budget, 0);
    }

    #[test]
    fn telemetry_storage_is_fixed_and_small() {
        assert!(std::mem::size_of::<ProducerLoadSnapshot>() < 1_024);
        assert!(std::mem::size_of::<ProducerLoadMeter>() < 8 * 1_024);
    }

    #[test]
    fn queue_pressure_tracks_current_and_window_peaks() {
        let mut meter = ProducerLoadMeter::default();
        meter.record(ProducerTurnRecord {
            producer_backlog_depth: 12,
            producer_backlog_capacity: 64,
            scheduler_queue_depth: 34,
            scheduler_queue_capacity: 200,
            scheduler_trace_depth: 56,
            scheduler_trace_capacity: 100,
            scheduler_trace_drops: 7,
            ..ProducerTurnRecord::default()
        });
        meter.record(ProducerTurnRecord {
            producer_backlog_depth: 3,
            producer_backlog_capacity: 64,
            scheduler_queue_depth: 4,
            scheduler_queue_capacity: 200,
            scheduler_trace_depth: 5,
            scheduler_trace_capacity: 100,
            scheduler_trace_drops: 9,
            ..ProducerTurnRecord::default()
        });
        let snapshot = meter.snapshot();
        assert_eq!(snapshot.producer_backlog_depth, 3);
        assert_eq!(snapshot.peak_producer_backlog_depth, 12);
        assert_eq!(snapshot.scheduler_queue_depth, 4);
        assert_eq!(snapshot.peak_scheduler_queue_depth, 34);
        assert_eq!(snapshot.scheduler_trace_depth, 5);
        assert_eq!(snapshot.peak_scheduler_trace_depth, 56);
        assert_eq!(snapshot.scheduler_trace_drops, 9);

        for _ in 2..PRODUCER_REPORTING_WINDOW_TURNS {
            meter.record(ProducerTurnRecord::default());
        }
        meter.record(ProducerTurnRecord {
            producer_backlog_depth: 2,
            scheduler_queue_depth: 3,
            scheduler_trace_depth: 4,
            ..ProducerTurnRecord::default()
        });
        let next = meter.snapshot();
        assert_eq!(next.window_index, 1);
        assert_eq!(next.peak_producer_backlog_depth, 2);
        assert_eq!(next.peak_scheduler_queue_depth, 3);
        assert_eq!(next.peak_scheduler_trace_depth, 4);
    }

    #[test]
    fn percentiles_and_cover_low_water_use_a_fixed_reporting_window() {
        let mut meter = ProducerLoadMeter::default();
        for _ in 0..PRODUCER_REPORTING_WINDOW_TURNS {
            let mut turn = ProducerTurnRecord {
                cover_start_nanos: 10,
                cover_end_nanos: 20,
                ..ProducerTurnRecord::default()
            };
            turn.set_phase_nanos(ProducerPhase::Total, 100);
            meter.record(turn);
        }
        let mut next = ProducerTurnRecord {
            cover_start_nanos: 1_000,
            cover_end_nanos: 2_000,
            ..ProducerTurnRecord::default()
        };
        next.set_phase_nanos(ProducerPhase::Total, 1_000);
        meter.record(next);

        let snapshot = meter.snapshot();
        assert_eq!(snapshot.turns, PRODUCER_REPORTING_WINDOW_TURNS + 1);
        assert_eq!(snapshot.window_index, 1);
        assert_eq!(snapshot.window_turns, 1);
        assert_eq!(snapshot.cover_low_water_nanos, 1_000);
        assert_eq!(snapshot.phase(ProducerPhase::Total).samples, 1);
        assert_eq!(snapshot.phase(ProducerPhase::Total).max_nanos, 1_000);
    }
}
