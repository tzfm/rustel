//! Bounded callback-owned voice, pool, and queue pressure telemetry.

#[cfg(any(feature = "device-audio", test))]
use std::sync::atomic::{AtomicU64, Ordering, fence};

pub const REALTIME_POOL_COUNT: usize = 5;
#[cfg(any(feature = "device-audio", test))]
pub(crate) const PRESSURE_PUBLISH_INTERVAL: u64 = 16;
#[cfg(any(feature = "device-audio", test))]
const SNAPSHOT_READ_ATTEMPTS: usize = 3;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum RealtimePool {
    Compressor,
    FxDelay,
    Stretch,
    ZzfxDelay,
    FxReverb,
}

impl RealtimePool {
    pub const ALL: [Self; REALTIME_POOL_COUNT] = [
        Self::Compressor,
        Self::FxDelay,
        Self::Stretch,
        Self::ZzfxDelay,
        Self::FxReverb,
    ];

    pub(crate) const fn index(self) -> usize {
        self as usize
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RealtimePressureSnapshot {
    pub publication: u64,
    pub total_callbacks: u64,
    pub window_callbacks: u64,
    pub max_polyphony: u64,
    pub active_voices: u64,
    pub peak_active_voices: u64,
    pub pending_events: u64,
    pub peak_pending_events: u64,
    pub active_orbits: u64,
    pub peak_active_orbits: u64,
    pub active_pool_leases: [u64; REALTIME_POOL_COUNT],
    pub active_orbit_delays: u64,
    pub active_orbit_reverbs: u64,
    pub active_dj_filters: u64,
    pub semantic_polyphony_fades: u64,
    pub window_semantic_polyphony_fades: u64,
    pub voice_ceiling_drops: u64,
    pub window_voice_ceiling_drops: u64,
    pub pending_ceiling_drops: u64,
    pub window_pending_ceiling_drops: u64,
    pub pool_misses: [u64; REALTIME_POOL_COUNT],
    pub window_pool_misses: [u64; REALTIME_POOL_COUNT],
    pub orbit_reverb_misses: u64,
    pub window_orbit_reverb_misses: u64,
    pub late_events: u64,
    pub window_late_events: u64,
    pub refused_voices: u64,
    pub window_refused_voices: u64,
}

impl RealtimePressureSnapshot {
    pub fn active_pool_leases(&self, pool: RealtimePool) -> u64 {
        self.active_pool_leases[pool.index()]
    }

    pub fn pool_misses(&self, pool: RealtimePool) -> u64 {
        self.pool_misses[pool.index()]
    }

    pub fn window_pool_misses(&self, pool: RealtimePool) -> u64 {
        self.window_pool_misses[pool.index()]
    }
}

#[cfg(any(feature = "device-audio", test))]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct RealtimePressureObservation {
    pub max_polyphony: u64,
    pub active_voices: u64,
    pub pending_events: u64,
    pub active_orbits: u64,
    pub active_pool_leases: [u64; REALTIME_POOL_COUNT],
    pub active_orbit_delays: u64,
    pub active_orbit_reverbs: u64,
    pub active_dj_filters: u64,
    pub semantic_polyphony_fades: u64,
    pub voice_ceiling_drops: u64,
    pub pending_ceiling_drops: u64,
    pub pool_misses: [u64; REALTIME_POOL_COUNT],
    pub orbit_reverb_misses: u64,
}

#[cfg(any(feature = "device-audio", test))]
pub(crate) struct RealtimePressurePublisher {
    sequence: AtomicU64,
    total_callbacks: AtomicU64,
    window_callbacks: AtomicU64,
    max_polyphony: AtomicU64,
    active_voices: AtomicU64,
    peak_active_voices: AtomicU64,
    pending_events: AtomicU64,
    peak_pending_events: AtomicU64,
    active_orbits: AtomicU64,
    peak_active_orbits: AtomicU64,
    active_pool_leases: [AtomicU64; REALTIME_POOL_COUNT],
    active_orbit_delays: AtomicU64,
    active_orbit_reverbs: AtomicU64,
    active_dj_filters: AtomicU64,
    semantic_polyphony_fades: AtomicU64,
    window_semantic_polyphony_fades: AtomicU64,
    voice_ceiling_drops: AtomicU64,
    window_voice_ceiling_drops: AtomicU64,
    pending_ceiling_drops: AtomicU64,
    window_pending_ceiling_drops: AtomicU64,
    pool_misses: [AtomicU64; REALTIME_POOL_COUNT],
    window_pool_misses: [AtomicU64; REALTIME_POOL_COUNT],
    orbit_reverb_misses: AtomicU64,
    window_orbit_reverb_misses: AtomicU64,
    late_events: AtomicU64,
    window_late_events: AtomicU64,
    refused_voices: AtomicU64,
    window_refused_voices: AtomicU64,
}

#[cfg(any(feature = "device-audio", test))]
impl RealtimePressurePublisher {
    pub(crate) fn new() -> Self {
        Self {
            sequence: AtomicU64::new(0),
            total_callbacks: AtomicU64::new(0),
            window_callbacks: AtomicU64::new(0),
            max_polyphony: AtomicU64::new(crate::MAX_POLYPHONY as u64),
            active_voices: AtomicU64::new(0),
            peak_active_voices: AtomicU64::new(0),
            pending_events: AtomicU64::new(0),
            peak_pending_events: AtomicU64::new(0),
            active_orbits: AtomicU64::new(0),
            peak_active_orbits: AtomicU64::new(0),
            active_pool_leases: std::array::from_fn(|_| AtomicU64::new(0)),
            active_orbit_delays: AtomicU64::new(0),
            active_orbit_reverbs: AtomicU64::new(0),
            active_dj_filters: AtomicU64::new(0),
            semantic_polyphony_fades: AtomicU64::new(0),
            window_semantic_polyphony_fades: AtomicU64::new(0),
            voice_ceiling_drops: AtomicU64::new(0),
            window_voice_ceiling_drops: AtomicU64::new(0),
            pending_ceiling_drops: AtomicU64::new(0),
            window_pending_ceiling_drops: AtomicU64::new(0),
            pool_misses: std::array::from_fn(|_| AtomicU64::new(0)),
            window_pool_misses: std::array::from_fn(|_| AtomicU64::new(0)),
            orbit_reverb_misses: AtomicU64::new(0),
            window_orbit_reverb_misses: AtomicU64::new(0),
            late_events: AtomicU64::new(0),
            window_late_events: AtomicU64::new(0),
            refused_voices: AtomicU64::new(0),
            window_refused_voices: AtomicU64::new(0),
        }
    }

    fn publish(&self, snapshot: RealtimePressureSnapshot) {
        let stable = self.sequence.load(Ordering::Relaxed) & !1;
        self.sequence
            .store(stable.wrapping_add(1), Ordering::Relaxed);
        fence(Ordering::Release);
        macro_rules! store {
            ($field:ident) => {
                self.$field.store(snapshot.$field, Ordering::Relaxed)
            };
        }
        store!(total_callbacks);
        store!(window_callbacks);
        store!(max_polyphony);
        store!(active_voices);
        store!(peak_active_voices);
        store!(pending_events);
        store!(peak_pending_events);
        store!(active_orbits);
        store!(peak_active_orbits);
        for index in 0..REALTIME_POOL_COUNT {
            self.active_pool_leases[index]
                .store(snapshot.active_pool_leases[index], Ordering::Relaxed);
            self.pool_misses[index].store(snapshot.pool_misses[index], Ordering::Relaxed);
            self.window_pool_misses[index]
                .store(snapshot.window_pool_misses[index], Ordering::Relaxed);
        }
        store!(active_orbit_delays);
        store!(active_orbit_reverbs);
        store!(active_dj_filters);
        store!(semantic_polyphony_fades);
        store!(window_semantic_polyphony_fades);
        store!(voice_ceiling_drops);
        store!(window_voice_ceiling_drops);
        store!(pending_ceiling_drops);
        store!(window_pending_ceiling_drops);
        store!(orbit_reverb_misses);
        store!(window_orbit_reverb_misses);
        store!(late_events);
        store!(window_late_events);
        store!(refused_voices);
        store!(window_refused_voices);
        self.sequence
            .store(stable.wrapping_add(2), Ordering::Release);
    }

    pub(crate) fn try_snapshot(&self) -> Option<RealtimePressureSnapshot> {
        for _ in 0..SNAPSHOT_READ_ATTEMPTS {
            let sequence = self.sequence.load(Ordering::Acquire);
            if sequence == 0 {
                return None;
            }
            if sequence & 1 != 0 {
                std::hint::spin_loop();
                continue;
            }
            macro_rules! load {
                ($field:ident) => {
                    self.$field.load(Ordering::Acquire)
                };
            }
            let snapshot = RealtimePressureSnapshot {
                publication: sequence / 2,
                total_callbacks: load!(total_callbacks),
                window_callbacks: load!(window_callbacks),
                max_polyphony: load!(max_polyphony),
                active_voices: load!(active_voices),
                peak_active_voices: load!(peak_active_voices),
                pending_events: load!(pending_events),
                peak_pending_events: load!(peak_pending_events),
                active_orbits: load!(active_orbits),
                peak_active_orbits: load!(peak_active_orbits),
                active_pool_leases: std::array::from_fn(|index| {
                    self.active_pool_leases[index].load(Ordering::Acquire)
                }),
                active_orbit_delays: load!(active_orbit_delays),
                active_orbit_reverbs: load!(active_orbit_reverbs),
                active_dj_filters: load!(active_dj_filters),
                semantic_polyphony_fades: load!(semantic_polyphony_fades),
                window_semantic_polyphony_fades: load!(window_semantic_polyphony_fades),
                voice_ceiling_drops: load!(voice_ceiling_drops),
                window_voice_ceiling_drops: load!(window_voice_ceiling_drops),
                pending_ceiling_drops: load!(pending_ceiling_drops),
                window_pending_ceiling_drops: load!(window_pending_ceiling_drops),
                pool_misses: std::array::from_fn(|index| {
                    self.pool_misses[index].load(Ordering::Acquire)
                }),
                window_pool_misses: std::array::from_fn(|index| {
                    self.window_pool_misses[index].load(Ordering::Acquire)
                }),
                orbit_reverb_misses: load!(orbit_reverb_misses),
                window_orbit_reverb_misses: load!(window_orbit_reverb_misses),
                late_events: load!(late_events),
                window_late_events: load!(window_late_events),
                refused_voices: load!(refused_voices),
                window_refused_voices: load!(window_refused_voices),
            };
            if self.sequence.load(Ordering::Acquire) == sequence {
                return Some(snapshot);
            }
            std::hint::spin_loop();
        }
        None
    }

    #[cfg(feature = "device-audio")]
    pub(crate) fn reset(&self) {
        self.publish(RealtimePressureSnapshot::default());
    }

    #[cfg(test)]
    fn begin_test_publication(&self) {
        let stable = self.sequence.load(Ordering::Relaxed) & !1;
        self.sequence
            .store(stable.wrapping_add(1), Ordering::Release);
    }
}

#[derive(Default)]
#[cfg(any(feature = "device-audio", test))]
pub(crate) struct RealtimePressureMeter {
    total_callbacks: u64,
    window_callbacks: u64,
    peak_active_voices: u64,
    peak_pending_events: u64,
    peak_active_orbits: u64,
    late_events: u64,
    window_late_events: u64,
    refused_voices: u64,
    window_refused_voices: u64,
    window_start: RealtimePressureObservation,
}

#[cfg(any(feature = "device-audio", test))]
impl RealtimePressureMeter {
    pub(crate) fn observe(
        &mut self,
        observation: RealtimePressureObservation,
        late_events: u64,
        refused_voices: u64,
        publisher: &RealtimePressurePublisher,
    ) {
        self.total_callbacks = self.total_callbacks.saturating_add(1);
        self.window_callbacks = self.window_callbacks.saturating_add(1);
        self.peak_active_voices = self.peak_active_voices.max(observation.active_voices);
        self.peak_pending_events = self.peak_pending_events.max(observation.pending_events);
        self.peak_active_orbits = self.peak_active_orbits.max(observation.active_orbits);
        self.late_events = self.late_events.saturating_add(late_events);
        self.window_late_events = self.window_late_events.saturating_add(late_events);
        self.refused_voices = self.refused_voices.saturating_add(refused_voices);
        self.window_refused_voices = self.window_refused_voices.saturating_add(refused_voices);

        if !self
            .total_callbacks
            .is_multiple_of(PRESSURE_PUBLISH_INTERVAL)
        {
            return;
        }

        let pool_misses = observation.pool_misses;
        let window_pool_misses = std::array::from_fn(|index| {
            pool_misses[index].saturating_sub(self.window_start.pool_misses[index])
        });
        publisher.publish(RealtimePressureSnapshot {
            publication: 0,
            total_callbacks: self.total_callbacks,
            window_callbacks: self.window_callbacks,
            max_polyphony: observation.max_polyphony,
            active_voices: observation.active_voices,
            peak_active_voices: self.peak_active_voices,
            pending_events: observation.pending_events,
            peak_pending_events: self.peak_pending_events,
            active_orbits: observation.active_orbits,
            peak_active_orbits: self.peak_active_orbits,
            active_pool_leases: observation.active_pool_leases,
            active_orbit_delays: observation.active_orbit_delays,
            active_orbit_reverbs: observation.active_orbit_reverbs,
            active_dj_filters: observation.active_dj_filters,
            semantic_polyphony_fades: observation.semantic_polyphony_fades,
            window_semantic_polyphony_fades: observation
                .semantic_polyphony_fades
                .saturating_sub(self.window_start.semantic_polyphony_fades),
            voice_ceiling_drops: observation.voice_ceiling_drops,
            window_voice_ceiling_drops: observation
                .voice_ceiling_drops
                .saturating_sub(self.window_start.voice_ceiling_drops),
            pending_ceiling_drops: observation.pending_ceiling_drops,
            window_pending_ceiling_drops: observation
                .pending_ceiling_drops
                .saturating_sub(self.window_start.pending_ceiling_drops),
            pool_misses,
            window_pool_misses,
            orbit_reverb_misses: observation.orbit_reverb_misses,
            window_orbit_reverb_misses: observation
                .orbit_reverb_misses
                .saturating_sub(self.window_start.orbit_reverb_misses),
            late_events: self.late_events,
            window_late_events: self.window_late_events,
            refused_voices: self.refused_voices,
            window_refused_voices: self.window_refused_voices,
        });
        self.window_callbacks = 0;
        self.peak_active_voices = 0;
        self.peak_pending_events = 0;
        self.peak_active_orbits = 0;
        self.window_late_events = 0;
        self.window_refused_voices = 0;
        self.window_start = observation;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };

    #[test]
    fn fixed_windows_publish_current_peaks_and_exact_deltas() {
        let publisher = RealtimePressurePublisher::new();
        let mut meter = RealtimePressureMeter::default();
        for callback in 0..PRESSURE_PUBLISH_INTERVAL {
            let observation = RealtimePressureObservation {
                max_polyphony: 192,
                active_voices: callback + 1,
                pending_events: 20 - callback,
                active_orbits: 2,
                semantic_polyphony_fades: callback,
                voice_ceiling_drops: callback / 4,
                pending_ceiling_drops: callback / 8,
                pool_misses: [callback, 0, 0, 0, 0],
                orbit_reverb_misses: callback / 2,
                ..RealtimePressureObservation::default()
            };
            meter.observe(observation, 1, 2, &publisher);
        }
        let snapshot = publisher.try_snapshot().expect("published window");
        assert_eq!(snapshot.total_callbacks, PRESSURE_PUBLISH_INTERVAL);
        assert_eq!(snapshot.window_callbacks, PRESSURE_PUBLISH_INTERVAL);
        assert_eq!(snapshot.max_polyphony, 192);
        assert_eq!(snapshot.active_voices, PRESSURE_PUBLISH_INTERVAL);
        assert_eq!(snapshot.peak_active_voices, PRESSURE_PUBLISH_INTERVAL);
        assert_eq!(snapshot.pending_events, 5);
        assert_eq!(snapshot.peak_pending_events, 20);
        assert_eq!(snapshot.window_semantic_polyphony_fades, 15);
        assert_eq!(snapshot.window_voice_ceiling_drops, 3);
        assert_eq!(snapshot.window_pending_ceiling_drops, 1);
        assert_eq!(snapshot.window_pool_misses(RealtimePool::Compressor), 15);
        assert_eq!(snapshot.window_orbit_reverb_misses, 7);
        assert_eq!(snapshot.window_late_events, PRESSURE_PUBLISH_INTERVAL);
        assert_eq!(
            snapshot.window_refused_voices,
            2 * PRESSURE_PUBLISH_INTERVAL
        );
    }

    #[test]
    fn snapshot_is_fixed_and_small() {
        assert!(std::mem::size_of::<RealtimePressureSnapshot>() <= 320);
        assert!(!std::mem::needs_drop::<RealtimePressureSnapshot>());
    }

    #[test]
    fn reader_gives_up_while_a_publication_is_in_progress() {
        let publisher = RealtimePressurePublisher::new();
        publisher.begin_test_publication();
        assert_eq!(publisher.try_snapshot(), None);
    }

    #[test]
    fn concurrent_reader_never_combines_publications() {
        const PUBLICATIONS: u64 = 20_000;
        let publisher = Arc::new(RealtimePressurePublisher::new());
        let done = Arc::new(AtomicBool::new(false));
        let writer_publisher = Arc::clone(&publisher);
        let writer_done = Arc::clone(&done);
        let writer = std::thread::spawn(move || {
            for value in 1..=PUBLICATIONS {
                writer_publisher.publish(RealtimePressureSnapshot {
                    total_callbacks: value,
                    window_callbacks: value,
                    active_voices: value,
                    peak_active_voices: value,
                    pending_events: value,
                    peak_pending_events: value,
                    active_orbits: value,
                    peak_active_orbits: value,
                    active_pool_leases: [value; REALTIME_POOL_COUNT],
                    active_orbit_delays: value,
                    active_orbit_reverbs: value,
                    active_dj_filters: value,
                    semantic_polyphony_fades: value,
                    window_semantic_polyphony_fades: value,
                    voice_ceiling_drops: value,
                    window_voice_ceiling_drops: value,
                    pending_ceiling_drops: value,
                    window_pending_ceiling_drops: value,
                    pool_misses: [value; REALTIME_POOL_COUNT],
                    window_pool_misses: [value; REALTIME_POOL_COUNT],
                    orbit_reverb_misses: value,
                    window_orbit_reverb_misses: value,
                    late_events: value,
                    window_late_events: value,
                    refused_voices: value,
                    window_refused_voices: value,
                    ..RealtimePressureSnapshot::default()
                });
            }
            writer_done.store(true, Ordering::Release);
        });

        let assert_consistent = |snapshot: RealtimePressureSnapshot| {
            let value = snapshot.publication;
            assert_eq!(snapshot.total_callbacks, value);
            assert_eq!(snapshot.window_callbacks, value);
            assert_eq!(snapshot.active_voices, value);
            assert_eq!(snapshot.peak_active_voices, value);
            assert_eq!(snapshot.pending_events, value);
            assert_eq!(snapshot.peak_pending_events, value);
            assert_eq!(snapshot.active_orbits, value);
            assert_eq!(snapshot.peak_active_orbits, value);
            assert_eq!(snapshot.active_pool_leases, [value; REALTIME_POOL_COUNT]);
            assert_eq!(snapshot.active_orbit_delays, value);
            assert_eq!(snapshot.active_orbit_reverbs, value);
            assert_eq!(snapshot.active_dj_filters, value);
            assert_eq!(snapshot.semantic_polyphony_fades, value);
            assert_eq!(snapshot.window_semantic_polyphony_fades, value);
            assert_eq!(snapshot.voice_ceiling_drops, value);
            assert_eq!(snapshot.window_voice_ceiling_drops, value);
            assert_eq!(snapshot.pending_ceiling_drops, value);
            assert_eq!(snapshot.window_pending_ceiling_drops, value);
            assert_eq!(snapshot.pool_misses, [value; REALTIME_POOL_COUNT]);
            assert_eq!(snapshot.window_pool_misses, [value; REALTIME_POOL_COUNT]);
            assert_eq!(snapshot.orbit_reverb_misses, value);
            assert_eq!(snapshot.window_orbit_reverb_misses, value);
            assert_eq!(snapshot.late_events, value);
            assert_eq!(snapshot.window_late_events, value);
            assert_eq!(snapshot.refused_voices, value);
            assert_eq!(snapshot.window_refused_voices, value);
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
