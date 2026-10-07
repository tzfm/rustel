//! Bounded callback-load measurement and lock-free publication.

use std::sync::atomic::{AtomicU64, Ordering, fence};

const LOAD_BASIS_POINTS: u64 = 10_000;
const FAST_EWMA_DENOMINATOR: u64 = 4;
const SLOW_EWMA_DENOMINATOR: u64 = 32;
pub(crate) const REALTIME_LOAD_PUBLISH_INTERVAL: u64 = 16;
const SNAPSHOT_READ_ATTEMPTS: usize = 3;

/// One consistent callback-load publication.
///
/// Loads use basis points where `10_000` is one complete callback period.
/// Values above `10_000` mean the callback exceeded its CPU deadline.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RealtimeLoadSnapshot {
    /// Monotonic publication number; zero means no callback has published.
    pub publication: u64,
    /// Actual stream rate used to calculate every callback period.
    pub sample_rate_hz: u32,
    /// Frames delivered by the host in the most recently published callback.
    pub last_callback_frames: u32,
    /// Render time of that callback.
    pub last_callback_busy_nanos: u64,
    /// CPU deadline derived from its frame count and sample rate.
    pub last_callback_period_nanos: u64,
    /// Callbacks observed since this stream rate became active.
    pub total_callbacks: u64,
    /// Render time accumulated over those callbacks.
    pub total_busy_nanos: u64,
    /// Frame-derived periods accumulated over those callbacks.
    pub total_period_nanos: u64,
    /// Load EWMA with alpha 1/4.
    pub fast_load_basis_points: u64,
    /// Load EWMA with alpha 1/32.
    pub slow_load_basis_points: u64,
    /// Highest load since the producer last read a consistent snapshot.
    pub peak_load_basis_points: u64,
    /// Cumulative callbacks above 80% of their period.
    pub callbacks_over_80_percent: u64,
    /// Cumulative callbacks above 90% of their period.
    pub callbacks_over_90_percent: u64,
    /// Cumulative callbacks that missed their period.
    pub callbacks_over_100_percent: u64,
    /// Cumulative callbacks above 125% of their period.
    pub callbacks_over_125_percent: u64,
    /// Current run of consecutive callbacks that missed their period.
    pub consecutive_over_budget: u64,
    /// Longest callback render time since this stream rate became active.
    pub max_callback_busy_nanos: u64,
}

/// Single-callback-writer, producer-reader atomic record.
pub(crate) struct RealtimeLoadPublisher {
    sequence: AtomicU64,
    acknowledged: AtomicU64,
    sample_rate_hz: AtomicU64,
    last_callback_frames: AtomicU64,
    last_callback_busy_nanos: AtomicU64,
    last_callback_period_nanos: AtomicU64,
    total_callbacks: AtomicU64,
    total_busy_nanos: AtomicU64,
    total_period_nanos: AtomicU64,
    fast_load_basis_points: AtomicU64,
    slow_load_basis_points: AtomicU64,
    peak_load_basis_points: AtomicU64,
    callbacks_over_80_percent: AtomicU64,
    callbacks_over_90_percent: AtomicU64,
    callbacks_over_100_percent: AtomicU64,
    callbacks_over_125_percent: AtomicU64,
    consecutive_over_budget: AtomicU64,
    max_callback_busy_nanos: AtomicU64,
}

impl RealtimeLoadPublisher {
    pub(crate) fn new() -> Self {
        Self {
            sequence: AtomicU64::new(0),
            acknowledged: AtomicU64::new(0),
            sample_rate_hz: AtomicU64::new(0),
            last_callback_frames: AtomicU64::new(0),
            last_callback_busy_nanos: AtomicU64::new(0),
            last_callback_period_nanos: AtomicU64::new(0),
            total_callbacks: AtomicU64::new(0),
            total_busy_nanos: AtomicU64::new(0),
            total_period_nanos: AtomicU64::new(0),
            fast_load_basis_points: AtomicU64::new(0),
            slow_load_basis_points: AtomicU64::new(0),
            peak_load_basis_points: AtomicU64::new(0),
            callbacks_over_80_percent: AtomicU64::new(0),
            callbacks_over_90_percent: AtomicU64::new(0),
            callbacks_over_100_percent: AtomicU64::new(0),
            callbacks_over_125_percent: AtomicU64::new(0),
            consecutive_over_budget: AtomicU64::new(0),
            max_callback_busy_nanos: AtomicU64::new(0),
        }
    }

    fn publish(&self, snapshot: RealtimeLoadSnapshot) {
        let stable = self.sequence.load(Ordering::Relaxed) & !1;
        self.sequence
            .store(stable.wrapping_add(1), Ordering::Relaxed);
        // If a reader observes any new field, its acquire load orders this
        // odd sequence before the reader's final check, which rejects it.
        fence(Ordering::Release);
        self.sample_rate_hz
            .store(u64::from(snapshot.sample_rate_hz), Ordering::Relaxed);
        self.last_callback_frames
            .store(u64::from(snapshot.last_callback_frames), Ordering::Relaxed);
        self.last_callback_busy_nanos
            .store(snapshot.last_callback_busy_nanos, Ordering::Relaxed);
        self.last_callback_period_nanos
            .store(snapshot.last_callback_period_nanos, Ordering::Relaxed);
        self.total_callbacks
            .store(snapshot.total_callbacks, Ordering::Relaxed);
        self.total_busy_nanos
            .store(snapshot.total_busy_nanos, Ordering::Relaxed);
        self.total_period_nanos
            .store(snapshot.total_period_nanos, Ordering::Relaxed);
        self.fast_load_basis_points
            .store(snapshot.fast_load_basis_points, Ordering::Relaxed);
        self.slow_load_basis_points
            .store(snapshot.slow_load_basis_points, Ordering::Relaxed);
        self.peak_load_basis_points
            .store(snapshot.peak_load_basis_points, Ordering::Relaxed);
        self.callbacks_over_80_percent
            .store(snapshot.callbacks_over_80_percent, Ordering::Relaxed);
        self.callbacks_over_90_percent
            .store(snapshot.callbacks_over_90_percent, Ordering::Relaxed);
        self.callbacks_over_100_percent
            .store(snapshot.callbacks_over_100_percent, Ordering::Relaxed);
        self.callbacks_over_125_percent
            .store(snapshot.callbacks_over_125_percent, Ordering::Relaxed);
        self.consecutive_over_budget
            .store(snapshot.consecutive_over_budget, Ordering::Relaxed);
        self.max_callback_busy_nanos
            .store(snapshot.max_callback_busy_nanos, Ordering::Relaxed);
        self.sequence
            .store(stable.wrapping_add(2), Ordering::Release);
    }

    pub(crate) fn try_snapshot(&self) -> Option<RealtimeLoadSnapshot> {
        for _ in 0..SNAPSHOT_READ_ATTEMPTS {
            let sequence = self.sequence.load(Ordering::Acquire);
            if sequence == 0 {
                return None;
            }
            if sequence & 1 != 0 {
                std::hint::spin_loop();
                continue;
            }
            let snapshot = RealtimeLoadSnapshot {
                publication: sequence / 2,
                sample_rate_hz: u32::try_from(self.sample_rate_hz.load(Ordering::Acquire))
                    .unwrap_or(u32::MAX),
                last_callback_frames: u32::try_from(
                    self.last_callback_frames.load(Ordering::Acquire),
                )
                .unwrap_or(u32::MAX),
                last_callback_busy_nanos: self.last_callback_busy_nanos.load(Ordering::Acquire),
                last_callback_period_nanos: self.last_callback_period_nanos.load(Ordering::Acquire),
                total_callbacks: self.total_callbacks.load(Ordering::Acquire),
                total_busy_nanos: self.total_busy_nanos.load(Ordering::Acquire),
                total_period_nanos: self.total_period_nanos.load(Ordering::Acquire),
                fast_load_basis_points: self.fast_load_basis_points.load(Ordering::Acquire),
                slow_load_basis_points: self.slow_load_basis_points.load(Ordering::Acquire),
                peak_load_basis_points: self.peak_load_basis_points.load(Ordering::Acquire),
                callbacks_over_80_percent: self.callbacks_over_80_percent.load(Ordering::Acquire),
                callbacks_over_90_percent: self.callbacks_over_90_percent.load(Ordering::Acquire),
                callbacks_over_100_percent: self.callbacks_over_100_percent.load(Ordering::Acquire),
                callbacks_over_125_percent: self.callbacks_over_125_percent.load(Ordering::Acquire),
                consecutive_over_budget: self.consecutive_over_budget.load(Ordering::Acquire),
                max_callback_busy_nanos: self.max_callback_busy_nanos.load(Ordering::Acquire),
            };
            if self.sequence.load(Ordering::Acquire) == sequence {
                self.acknowledged.fetch_max(sequence, Ordering::Release);
                return Some(snapshot);
            }
            std::hint::spin_loop();
        }
        None
    }

    pub(crate) fn acknowledged(&self) -> u64 {
        self.acknowledged.load(Ordering::Acquire)
    }

    /// The prior callback has stopped before this is called.
    pub(crate) fn reset(&self) {
        self.publish(RealtimeLoadSnapshot::default());
        self.acknowledged
            .store(self.sequence.load(Ordering::Acquire), Ordering::Release);
    }

    #[cfg(test)]
    fn begin_test_publication(&self) {
        let stable = self.sequence.load(Ordering::Relaxed) & !1;
        self.sequence
            .store(stable.wrapping_add(1), Ordering::Release);
    }
}

/// Plain callback-owned accumulator. It performs no allocation or locking.
pub(crate) struct RealtimeLoadMeter {
    sample_rate_hz: u64,
    last_callback_frames: u64,
    last_callback_busy_nanos: u64,
    last_callback_period_nanos: u64,
    total_callbacks: u64,
    total_busy_nanos: u64,
    total_period_nanos: u64,
    fast_load_basis_points: u64,
    slow_load_basis_points: u64,
    peak_load_basis_points: u64,
    callbacks_over_80_percent: u64,
    callbacks_over_90_percent: u64,
    callbacks_over_100_percent: u64,
    callbacks_over_125_percent: u64,
    consecutive_over_budget: u64,
    max_callback_busy_nanos: u64,
    acknowledged_publication: u64,
}

impl RealtimeLoadMeter {
    pub(crate) fn new(sample_rate_hz: u32, publisher: &RealtimeLoadPublisher) -> Self {
        Self {
            sample_rate_hz: u64::from(sample_rate_hz.max(1)),
            last_callback_frames: 0,
            last_callback_busy_nanos: 0,
            last_callback_period_nanos: 0,
            total_callbacks: 0,
            total_busy_nanos: 0,
            total_period_nanos: 0,
            fast_load_basis_points: 0,
            slow_load_basis_points: 0,
            peak_load_basis_points: 0,
            callbacks_over_80_percent: 0,
            callbacks_over_90_percent: 0,
            callbacks_over_100_percent: 0,
            callbacks_over_125_percent: 0,
            consecutive_over_budget: 0,
            max_callback_busy_nanos: 0,
            acknowledged_publication: publisher.acknowledged(),
        }
    }

    pub(crate) fn observe(
        &mut self,
        callback_frames: usize,
        busy_nanos: u64,
        sample_rate_hz: u32,
        publisher: &RealtimeLoadPublisher,
    ) {
        let sample_rate_hz_u32 = sample_rate_hz.max(1);
        let sample_rate_hz = u64::from(sample_rate_hz_u32);
        if sample_rate_hz != self.sample_rate_hz {
            *self = Self::new(sample_rate_hz_u32, publisher);
        }

        let acknowledged = publisher.acknowledged();
        if acknowledged != self.acknowledged_publication {
            self.peak_load_basis_points = 0;
            self.acknowledged_publication = acknowledged;
        }

        let callback_frames = u64::try_from(callback_frames).unwrap_or(u64::MAX);
        let period_nanos = callback_period_nanos(callback_frames, sample_rate_hz);
        let load_basis_points = if period_nanos == 0 {
            0
        } else {
            u64::try_from(
                u128::from(busy_nanos).saturating_mul(u128::from(LOAD_BASIS_POINTS))
                    / u128::from(period_nanos),
            )
            .unwrap_or(u64::MAX)
        };

        self.last_callback_frames = callback_frames;
        self.last_callback_busy_nanos = busy_nanos;
        self.last_callback_period_nanos = period_nanos;
        if self.total_callbacks == 0 {
            self.fast_load_basis_points = load_basis_points;
            self.slow_load_basis_points = load_basis_points;
        } else {
            update_ewma(
                &mut self.fast_load_basis_points,
                load_basis_points,
                FAST_EWMA_DENOMINATOR,
            );
            update_ewma(
                &mut self.slow_load_basis_points,
                load_basis_points,
                SLOW_EWMA_DENOMINATOR,
            );
        }
        self.total_callbacks = self.total_callbacks.saturating_add(1);
        self.total_busy_nanos = self.total_busy_nanos.saturating_add(busy_nanos);
        self.total_period_nanos = self.total_period_nanos.saturating_add(period_nanos);
        self.peak_load_basis_points = self.peak_load_basis_points.max(load_basis_points);
        self.max_callback_busy_nanos = self.max_callback_busy_nanos.max(busy_nanos);
        self.callbacks_over_80_percent = self
            .callbacks_over_80_percent
            .saturating_add(u64::from(load_basis_points > 8_000));
        self.callbacks_over_90_percent = self
            .callbacks_over_90_percent
            .saturating_add(u64::from(load_basis_points > 9_000));
        self.callbacks_over_100_percent = self
            .callbacks_over_100_percent
            .saturating_add(u64::from(load_basis_points > 10_000));
        self.callbacks_over_125_percent = self
            .callbacks_over_125_percent
            .saturating_add(u64::from(load_basis_points > 12_500));
        self.consecutive_over_budget = if load_basis_points > 10_000 {
            self.consecutive_over_budget.saturating_add(1)
        } else {
            0
        };

        if self
            .total_callbacks
            .is_multiple_of(REALTIME_LOAD_PUBLISH_INTERVAL)
        {
            publisher.publish(self.snapshot());
        }
    }

    fn snapshot(&self) -> RealtimeLoadSnapshot {
        RealtimeLoadSnapshot {
            publication: 0,
            sample_rate_hz: u32::try_from(self.sample_rate_hz).unwrap_or(u32::MAX),
            last_callback_frames: u32::try_from(self.last_callback_frames).unwrap_or(u32::MAX),
            last_callback_busy_nanos: self.last_callback_busy_nanos,
            last_callback_period_nanos: self.last_callback_period_nanos,
            total_callbacks: self.total_callbacks,
            total_busy_nanos: self.total_busy_nanos,
            total_period_nanos: self.total_period_nanos,
            fast_load_basis_points: self.fast_load_basis_points,
            slow_load_basis_points: self.slow_load_basis_points,
            peak_load_basis_points: self.peak_load_basis_points,
            callbacks_over_80_percent: self.callbacks_over_80_percent,
            callbacks_over_90_percent: self.callbacks_over_90_percent,
            callbacks_over_100_percent: self.callbacks_over_100_percent,
            callbacks_over_125_percent: self.callbacks_over_125_percent,
            consecutive_over_budget: self.consecutive_over_budget,
            max_callback_busy_nanos: self.max_callback_busy_nanos,
        }
    }
}

fn callback_period_nanos(callback_frames: u64, sample_rate_hz: u64) -> u64 {
    u64::try_from(
        u128::from(callback_frames).saturating_mul(1_000_000_000) / u128::from(sample_rate_hz),
    )
    .unwrap_or(u64::MAX)
}

fn update_ewma(current: &mut u64, sample: u64, denominator: u64) {
    if sample >= *current {
        *current = current.saturating_add(sample.saturating_sub(*current) / denominator);
    } else {
        *current = current.saturating_sub(current.saturating_sub(sample) / denominator);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };

    fn publish_after(frames: usize, busy_nanos: u64, sample_rate: u32) -> RealtimeLoadSnapshot {
        let publisher = RealtimeLoadPublisher::new();
        let mut meter = RealtimeLoadMeter::new(sample_rate, &publisher);
        for _ in 0..REALTIME_LOAD_PUBLISH_INTERVAL {
            meter.observe(frames, busy_nanos, sample_rate, &publisher);
        }
        publisher.try_snapshot().expect("load publication")
    }

    #[test]
    fn known_busy_period_pairs_produce_exact_basis_points() {
        let half = publish_after(480, 5_000_000, 48_000);
        assert_eq!(half.last_callback_period_nanos, 10_000_000);
        assert_eq!(half.fast_load_basis_points, 5_000);
        assert_eq!(half.slow_load_basis_points, 5_000);
        assert_eq!(half.peak_load_basis_points, 5_000);

        let overloaded = publish_after(480, 12_500_000, 48_000);
        assert_eq!(overloaded.peak_load_basis_points, 12_500);
        assert_eq!(overloaded.callbacks_over_80_percent, 16);
        assert_eq!(overloaded.callbacks_over_90_percent, 16);
        assert_eq!(overloaded.callbacks_over_100_percent, 16);
        assert_eq!(overloaded.callbacks_over_125_percent, 0);
        assert_eq!(overloaded.consecutive_over_budget, 16);
    }

    #[test]
    fn fast_and_slow_windows_use_their_declared_weights() {
        let publisher = RealtimeLoadPublisher::new();
        let mut meter = RealtimeLoadMeter::new(48_000, &publisher);
        meter.observe(480, 5_000_000, 48_000, &publisher);
        meter.observe(480, 9_000_000, 48_000, &publisher);
        let snapshot = meter.snapshot();
        assert_eq!(snapshot.fast_load_basis_points, 6_000);
        assert_eq!(snapshot.slow_load_basis_points, 5_125);
    }

    #[test]
    fn each_callback_size_uses_its_own_period() {
        for frames in [64usize, 128, 256, 2_048] {
            let period = callback_period_nanos(frames as u64, 48_000);
            let snapshot = publish_after(frames, period, 48_000);
            assert_eq!(snapshot.last_callback_frames, frames as u32);
            assert_eq!(snapshot.last_callback_period_nanos, period);
            assert_eq!(snapshot.peak_load_basis_points, 10_000);
        }
    }

    #[test]
    fn changing_sample_rate_starts_a_fresh_window() {
        let publisher = RealtimeLoadPublisher::new();
        let mut meter = RealtimeLoadMeter::new(48_000, &publisher);
        for _ in 0..REALTIME_LOAD_PUBLISH_INTERVAL {
            meter.observe(480, 5_000_000, 48_000, &publisher);
        }
        assert_eq!(
            publisher.try_snapshot().expect("48 kHz").total_period_nanos,
            160_000_000
        );

        for _ in 0..REALTIME_LOAD_PUBLISH_INTERVAL {
            meter.observe(441, 2_000_000, 44_100, &publisher);
        }
        let changed = publisher.try_snapshot().expect("44.1 kHz");
        assert_eq!(changed.sample_rate_hz, 44_100);
        assert_eq!(changed.total_callbacks, REALTIME_LOAD_PUBLISH_INTERVAL);
        assert_eq!(changed.total_busy_nanos, 32_000_000);
        assert_eq!(changed.total_period_nanos, 160_000_000);
    }

    #[test]
    fn acknowledged_snapshot_restarts_the_peak_window() {
        let publisher = RealtimeLoadPublisher::new();
        let mut meter = RealtimeLoadMeter::new(48_000, &publisher);
        for busy in [12_000_000]
            .into_iter()
            .chain(std::iter::repeat_n(1_000_000, 15))
        {
            meter.observe(480, busy, 48_000, &publisher);
        }
        assert_eq!(
            publisher
                .try_snapshot()
                .expect("first peak")
                .peak_load_basis_points,
            12_000
        );

        for _ in 0..REALTIME_LOAD_PUBLISH_INTERVAL {
            meter.observe(480, 1_000_000, 48_000, &publisher);
        }
        assert_eq!(
            publisher
                .try_snapshot()
                .expect("second peak")
                .peak_load_basis_points,
            1_000
        );
    }

    #[test]
    fn reader_gives_up_while_a_publication_is_in_progress() {
        let publisher = RealtimeLoadPublisher::new();
        publisher.begin_test_publication();
        assert_eq!(publisher.try_snapshot(), None);
    }

    #[test]
    fn concurrent_reader_never_combines_publications() {
        const PUBLICATIONS: u64 = 20_000;
        let publisher = Arc::new(RealtimeLoadPublisher::new());
        let done = Arc::new(AtomicBool::new(false));
        let writer_publisher = Arc::clone(&publisher);
        let writer_done = Arc::clone(&done);
        let writer = std::thread::spawn(move || {
            for value in 1..=PUBLICATIONS {
                writer_publisher.publish(RealtimeLoadSnapshot {
                    sample_rate_hz: value as u32,
                    last_callback_frames: value as u32,
                    last_callback_busy_nanos: value,
                    last_callback_period_nanos: value,
                    total_callbacks: value,
                    total_busy_nanos: value,
                    total_period_nanos: value,
                    fast_load_basis_points: value,
                    slow_load_basis_points: value,
                    peak_load_basis_points: value,
                    callbacks_over_80_percent: value,
                    callbacks_over_90_percent: value,
                    callbacks_over_100_percent: value,
                    callbacks_over_125_percent: value,
                    consecutive_over_budget: value,
                    max_callback_busy_nanos: value,
                    ..RealtimeLoadSnapshot::default()
                });
            }
            writer_done.store(true, Ordering::Release);
        });

        let assert_consistent = |snapshot: RealtimeLoadSnapshot| {
            let value = snapshot.publication;
            assert_eq!(u64::from(snapshot.sample_rate_hz), value);
            assert_eq!(u64::from(snapshot.last_callback_frames), value);
            assert_eq!(snapshot.last_callback_busy_nanos, value);
            assert_eq!(snapshot.last_callback_period_nanos, value);
            assert_eq!(snapshot.total_callbacks, value);
            assert_eq!(snapshot.total_busy_nanos, value);
            assert_eq!(snapshot.total_period_nanos, value);
            assert_eq!(snapshot.fast_load_basis_points, value);
            assert_eq!(snapshot.slow_load_basis_points, value);
            assert_eq!(snapshot.peak_load_basis_points, value);
            assert_eq!(snapshot.callbacks_over_80_percent, value);
            assert_eq!(snapshot.callbacks_over_90_percent, value);
            assert_eq!(snapshot.callbacks_over_100_percent, value);
            assert_eq!(snapshot.callbacks_over_125_percent, value);
            assert_eq!(snapshot.consecutive_over_budget, value);
            assert_eq!(snapshot.max_callback_busy_nanos, value);
        };
        while !done.load(Ordering::Acquire) {
            if let Some(snapshot) = publisher.try_snapshot() {
                assert_consistent(snapshot);
            }
        }
        writer.join().expect("snapshot writer");
        assert_consistent(publisher.try_snapshot().expect("final publication"));
    }
}
