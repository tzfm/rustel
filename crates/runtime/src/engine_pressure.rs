//! Presentation-neutral live engine pressure and overload classification.

use std::time::{Duration, Instant};

use rustel_audio::{
    LiveDeviceReport, MAX_ACTIVE_VOICES, MAX_PENDING_EVENTS, MAX_POLYPHONY, RealtimePool,
};

use crate::ProducerLoadSnapshot;

mod report;
pub use report::{
    ENGINE_PRESSURE_SCHEMA_VERSION, EnginePressureAssetQueuesV1, EnginePressureDeviceV1,
    EnginePressureDspV1, EnginePressurePoolValuesV1, EnginePressurePoolsV1,
    EnginePressureProcessV1, EnginePressureQueueV1, EnginePressureQueuesV1,
    EnginePressureReportContext, EnginePressureReportV1, EnginePressureSchedulerV1,
    EnginePressureStatusV1, EnginePressureVoicesV1,
};
#[cfg(test)]
mod report_tests {
    use super::*;
    use crate::{ProducerLoadSnapshot, ProducerPhase, ProducerPhaseSnapshot, ProducerTurnOutcome};
    use rustel_audio::{
        AssetQueuePressureSnapshot, LiveDeviceReport, RealtimeLoadSnapshot,
        RealtimePressureSnapshot,
    };

    fn device_fixture() -> LiveDeviceReport {
        LiveDeviceReport {
            stream_id: 9,
            generation: 4,
            callbacks: 81,
            playback_latency_nanos: 12_000_000,
            max_playback_latency_nanos: 18_000_000,
            max_callback_gap_nanos: 7_000_000,
            callback_errors: 2,
            callback_scope_misses: 3,
            callback_allocations: 4,
            callback_frees: 5,
            ring_depth: 6,
            ring_peak_depth: 7,
            ring_capacity: 8_192,
            ring_refusals: 8,
            realtime_load: RealtimeLoadSnapshot {
                publication: 10,
                sample_rate_hz: 48_000,
                last_callback_frames: 128,
                last_callback_busy_nanos: 640_000,
                last_callback_period_nanos: 2_666_666,
                total_callbacks: 80,
                fast_load_basis_points: 2_400,
                slow_load_basis_points: 2_400,
                peak_load_basis_points: 2_400,
                callbacks_over_100_percent: 11,
                max_callback_busy_nanos: 900_000,
                ..RealtimeLoadSnapshot::default()
            },
            realtime_pressure: RealtimePressureSnapshot {
                publication: 12,
                active_voices: 18,
                peak_active_voices: 23,
                pending_events: 5,
                peak_pending_events: 9,
                active_orbits: 2,
                peak_active_orbits: 3,
                active_pool_leases: [1, 2, 3, 4, 5],
                pool_misses: [6, 7, 8, 9, 10],
                window_pool_misses: [11, 12, 13, 14, 15],
                orbit_reverb_misses: 16,
                window_orbit_reverb_misses: 17,
                ..RealtimePressureSnapshot::default()
            },
            asset_queues: AssetQueuePressureSnapshot {
                capacity: 64,
                sample_installs: 1,
                sample_returns: 2,
                orbit_reverb_installs: 3,
                orbit_reverb_returns: 4,
                fx_reverb_installs: 5,
                fx_reverb_returns: 6,
                fx_reverb_refusals: 0,
                leaked: 7,
            },
            ..LiveDeviceReport::default()
        }
    }

    fn producer_fixture() -> ProducerLoadSnapshot {
        let mut producer = ProducerLoadSnapshot {
            publication: 13,
            turns: 200,
            window_turns: 100,
            last_outcome: ProducerTurnOutcome::Progress,
            fast_load_basis_points: 1_100,
            slow_load_basis_points: 1_100,
            peak_load_basis_points: 1_100,
            cover_end_nanos: 420_000_000,
            producer_backlog_depth: 1,
            peak_producer_backlog_depth: 2,
            producer_backlog_capacity: 4,
            scheduler_queue_depth: 3,
            peak_scheduler_queue_depth: 4,
            scheduler_queue_capacity: 8,
            scheduler_trace_depth: 5,
            peak_scheduler_trace_depth: 6,
            scheduler_trace_capacity: 16,
            scheduler_trace_drops: 7,
            ..ProducerLoadSnapshot::default()
        };
        producer.phases[ProducerPhase::Total as usize] = ProducerPhaseSnapshot {
            p50_nanos: 100,
            p95_nanos: 200,
            p99_nanos: 300,
            max_nanos: 400,
            ..ProducerPhaseSnapshot::default()
        };
        producer
    }

    fn assert_keys(value: &serde_json::Value, expected: &[&str]) {
        let actual = value
            .as_object()
            .expect("JSON object")
            .keys()
            .map(String::as_str)
            .collect::<std::collections::BTreeSet<_>>();
        let expected = expected
            .iter()
            .copied()
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(actual, expected);
    }

    #[test]
    fn report_v1_uses_null_for_every_unavailable_section() {
        let report = EnginePressureSnapshot::default().report_v1(EnginePressureReportContext {
            process_cpu_percent: Some(f64::NAN),
            ..EnginePressureReportContext::default()
        });
        assert_eq!(
            serde_json::to_value(report).expect("serialize pressure report"),
            serde_json::json!({
                "schema_version": 1,
                "status": null,
                "dsp": null,
                "scheduler": null,
                "voices": null,
                "queues": null,
                "pools": null,
                "device": null,
                "process": {
                    "cpu_percent": null,
                    "resident_bytes": null,
                },
            })
        );
    }

    #[test]
    fn report_v1_preserves_available_scheduler_queues_without_inventing_device_values() {
        let mut producer = ProducerLoadSnapshot {
            publication: 1,
            producer_backlog_depth: 2,
            producer_backlog_capacity: 4,
            scheduler_queue_depth: 3,
            scheduler_queue_capacity: 8,
            scheduler_trace_depth: 5,
            scheduler_trace_capacity: 16,
            ..ProducerLoadSnapshot::default()
        };
        producer.last_outcome = ProducerTurnOutcome::Idle;
        let value = serde_json::to_value(
            EnginePressureSnapshot {
                producer,
                ..EnginePressureSnapshot::default()
            }
            .report_v1(EnginePressureReportContext::default()),
        )
        .expect("serialize pressure report");

        assert_eq!(value["queues"]["event_ring"], serde_json::Value::Null);
        assert_eq!(value["queues"]["assets"], serde_json::Value::Null);
        assert_eq!(value["queues"]["producer_backlog"]["depth"], 2);
        assert_eq!(value["queues"]["scheduler"]["depth"], 3);
        assert_eq!(value["queues"]["scheduler_trace"]["depth"], 5);
    }

    #[test]
    fn report_v1_schema_and_named_pool_projection_are_exact() {
        let value = serde_json::to_value(
            EnginePressureSnapshot {
                device: device_fixture(),
                producer: producer_fixture(),
                callback_deadline_misses: 2,
                producer_refusals: 3,
                host_gap_nanos: 4,
                cause: EnginePressureCause::DspOverload,
                level: EnginePressureLevel::Error,
            }
            .report_v1(EnginePressureReportContext {
                device_name: Some("silent".into()),
                requested_buffer_frames: Some(256),
                reported_buffer_frames: None,
                process_cpu_percent: Some(12.5),
                process_resident_bytes: Some(1_024),
            }),
        )
        .expect("serialize pressure report");

        assert_keys(
            &value,
            &[
                "schema_version",
                "status",
                "dsp",
                "scheduler",
                "voices",
                "queues",
                "pools",
                "device",
                "process",
            ],
        );
        assert_keys(&value["status"], &["level", "cause", "message"]);
        assert_keys(
            &value["dsp"],
            &[
                "publication",
                "fast_load_basis_points",
                "slow_load_basis_points",
                "peak_load_basis_points",
                "callback_busy_nanos",
                "callback_period_nanos",
                "total_callbacks",
                "callbacks_over_80_percent",
                "callbacks_over_90_percent",
                "callbacks_over_100_percent",
                "callbacks_over_125_percent",
                "recent_deadline_misses",
                "consecutive_over_budget",
                "max_callback_busy_nanos",
                "max_callback_gap_nanos",
                "recent_host_gap_nanos",
                "callback_errors",
                "callback_scope_misses",
                "callback_allocations",
                "callback_frees",
            ],
        );
        assert_keys(
            &value["scheduler"],
            &[
                "publication",
                "turns",
                "window_index",
                "window_turns",
                "progress_turns",
                "idle_turns",
                "last_outcome",
                "last_load_basis_points",
                "fast_load_basis_points",
                "slow_load_basis_points",
                "peak_load_basis_points",
                "consecutive_over_budget",
                "turn_p50_nanos",
                "turn_p95_nanos",
                "turn_p99_nanos",
                "turn_max_nanos",
                "cover_start_nanos",
                "cover_end_nanos",
                "cover_low_water_nanos",
                "cover_gained_nanos",
                "continuation_reserve_nanos",
                "budget_granted_nanos",
                "atomic_refusals",
                "committed_refusals",
                "recent_refusals",
                "cancelled_turns",
                "stopped_turns",
                "rollback_count",
                "gap_resync_count",
                "queue_saturation_count",
                "haps",
                "scheduler_events",
                "converted_audio_events",
                "ring_pushes",
                "js_callback_calls",
            ],
        );
        assert_keys(
            &value["voices"],
            &[
                "active",
                "peak",
                "semantic_capacity",
                "hard_capacity",
                "pending",
                "peak_pending",
                "pending_capacity",
                "active_orbits",
                "peak_active_orbits",
                "active_orbit_delays",
                "active_orbit_reverbs",
                "active_dj_filters",
                "semantic_polyphony_fades",
                "recent_semantic_polyphony_fades",
                "voice_ceiling_drops",
                "recent_voice_ceiling_drops",
                "pending_ceiling_drops",
                "recent_pending_ceiling_drops",
                "refused",
                "recent_refused",
                "late_events",
                "recent_late_events",
            ],
        );
        assert_keys(
            &value["queues"],
            &[
                "event_ring",
                "producer_backlog",
                "scheduler",
                "scheduler_trace",
                "assets",
            ],
        );
        for queue in [
            "event_ring",
            "producer_backlog",
            "scheduler",
            "scheduler_trace",
        ] {
            assert_keys(
                &value["queues"][queue],
                &["depth", "peak_depth", "capacity", "refusals_or_drops"],
            );
        }
        assert_keys(
            &value["queues"]["assets"],
            &["depth", "capacity_each", "leaked"],
        );
        assert_keys(
            &value["pools"],
            &[
                "active",
                "misses",
                "recent_misses",
                "orbit_reverb_misses",
                "recent_orbit_reverb_misses",
            ],
        );
        for pool_set in ["active", "misses", "recent_misses"] {
            assert_keys(
                &value["pools"][pool_set],
                &[
                    "compressor",
                    "fx_delay",
                    "stretch",
                    "zzfx_delay",
                    "fx_reverb",
                ],
            );
        }
        assert_keys(
            &value["device"],
            &[
                "name",
                "stream_id",
                "generation",
                "sample_rate_hz",
                "requested_buffer_frames",
                "reported_buffer_frames",
                "callback_frames",
                "callbacks",
                "playback_latency_nanos",
                "max_playback_latency_nanos",
            ],
        );
        assert_keys(&value["process"], &["cpu_percent", "resident_bytes"]);
        assert_eq!(value["schema_version"], 1);
        assert_eq!(value["status"]["cause"], "dsp_overload");
        assert_eq!(value["scheduler"]["last_outcome"], "progress");
        assert_eq!(value["queues"]["assets"]["depth"], 21);
        assert_eq!(value["pools"]["active"]["compressor"], 1);
        assert_eq!(value["pools"]["active"]["fx_reverb"], 5);
        assert_eq!(value["pools"]["recent_misses"]["zzfx_delay"], 14);
        assert_eq!(
            value["device"]["reported_buffer_frames"],
            serde_json::Value::Null
        );
        assert_eq!(value["process"]["cpu_percent"], 12.5);
    }
}

const RECENT_FAILURE_HOLD: Duration = Duration::from_secs(2);
/// Incidents closer than this count as one hiccup. The studio samples
/// every frame, so one stall can show up in several samples.
const HICCUP_SPAN: Duration = Duration::from_millis(500);
/// A second hiccup within this window turns the header red.
const REPEAT_WINDOW: Duration = Duration::from_secs(10);

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum EnginePressureCause {
    #[default]
    Healthy,
    DspOverload,
    ProducerOverload,
    HostStarvation,
    VoicePressure,
    /// The producer, ring, or callback turned work away because it could
    /// not keep up. A rejected score (syntax, a throw) is not this: those
    /// keep the last good graph and are not counted as refusals.
    Refusal,
}

impl EnginePressureCause {
    pub const fn code(self) -> &'static str {
        match self {
            Self::Healthy => "healthy",
            Self::DspOverload => "dsp_overload",
            Self::ProducerOverload => "producer_overload",
            Self::HostStarvation => "host_starvation",
            Self::VoicePressure => "voice_pressure",
            Self::Refusal => "refusal",
        }
    }

    pub const fn label(self) -> &'static str {
        match self {
            Self::Healthy => "healthy",
            Self::DspOverload => "DSP deadline missed",
            Self::ProducerOverload => "scheduler fell behind",
            Self::HostStarvation => "audio host gap",
            Self::VoicePressure => "voice capacity pressure",
            Self::Refusal => "engine refused work",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, Ord, PartialEq, PartialOrd)]
pub enum EnginePressureLevel {
    #[default]
    Normal,
    Caution,
    Warning,
    Error,
}

impl EnginePressureLevel {
    pub const fn code(self) -> &'static str {
        match self {
            Self::Normal => "normal",
            Self::Caution => "caution",
            Self::Warning => "warning",
            Self::Error => "error",
        }
    }
}

/// One bounded producer-side projection of callback and scheduling pressure.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct EnginePressureSnapshot {
    pub device: LiveDeviceReport,
    pub producer: ProducerLoadSnapshot,
    /// Callback deadline misses since the prior projection.
    pub callback_deadline_misses: u64,
    /// Transactional producer refusals since the prior projection.
    pub producer_refusals: u64,
    /// A newly observed host gap beyond two device periods, otherwise zero.
    pub host_gap_nanos: u64,
    pub cause: EnginePressureCause,
    pub level: EnginePressureLevel,
}

impl EnginePressureSnapshot {
    pub const fn semantic_voice_capacity(&self) -> u64 {
        let configured = self.device.realtime_pressure.max_polyphony;
        if configured == 0 {
            MAX_POLYPHONY as u64
        } else {
            configured
        }
    }

    pub const fn hard_voice_capacity(&self) -> u64 {
        MAX_ACTIVE_VOICES as u64
    }

    pub const fn pending_capacity(&self) -> u64 {
        MAX_PENDING_EVENTS as u64
    }

    pub const fn dsp_load_basis_points(&self) -> u64 {
        self.device.realtime_load.slow_load_basis_points
    }

    pub const fn dsp_fast_load_basis_points(&self) -> u64 {
        self.device.realtime_load.fast_load_basis_points
    }

    pub const fn dsp_peak_load_basis_points(&self) -> u64 {
        self.device.realtime_load.peak_load_basis_points
    }

    pub const fn scheduler_load_basis_points(&self) -> u64 {
        self.producer.slow_load_basis_points as u64
    }

    pub const fn scheduler_fast_load_basis_points(&self) -> u64 {
        self.producer.fast_load_basis_points as u64
    }

    pub const fn scheduler_peak_load_basis_points(&self) -> u64 {
        self.producer.peak_load_basis_points as u64
    }

    pub const fn cover_millis(&self) -> u64 {
        self.producer.cover_end_nanos / 1_000_000
    }

    pub const fn callback_period_nanos(&self) -> u64 {
        self.device.realtime_load.last_callback_period_nanos
    }

    pub const fn active_voices(&self) -> u64 {
        self.device.realtime_pressure.active_voices
    }

    pub const fn peak_active_voices(&self) -> u64 {
        self.device.realtime_pressure.peak_active_voices
    }

    pub fn window_pool_misses(&self) -> u64 {
        RealtimePool::ALL
            .into_iter()
            .map(|pool| self.device.realtime_pressure.window_pool_misses(pool))
            .fold(
                self.device.realtime_pressure.window_orbit_reverb_misses,
                u64::saturating_add,
            )
    }

    pub fn window_hard_drops(&self) -> u64 {
        self.device
            .realtime_pressure
            .window_voice_ceiling_drops
            .saturating_add(self.device.realtime_pressure.window_pending_ceiling_drops)
    }
}

/// Converts cumulative callback/producer counters into recent deltas.
#[derive(Clone, Copy, Debug, Default)]
pub struct EnginePressureMonitor {
    stream_id: u64,
    callback_deadline_misses: u64,
    producer_refusals: u64,
    callback_errors: u64,
    ring_refusals: u64,
    max_callback_gap_nanos: u64,
    recent_cause: EnginePressureCause,
    recent_level: EnginePressureLevel,
    recent_until: Option<Instant>,
    /// When the current hiccup began, and when it last showed an incident.
    hiccup_start: Option<Instant>,
    last_incident: Option<Instant>,
    /// The current hiccup followed another within [`REPEAT_WINDOW`].
    hiccup_repeated: bool,
}

impl EnginePressureMonitor {
    pub fn sample(
        &mut self,
        device: LiveDeviceReport,
        producer: ProducerLoadSnapshot,
        buffer_frames: u32,
    ) -> EnginePressureSnapshot {
        self.sample_at(device, producer, buffer_frames, Instant::now())
    }

    pub fn sample_at(
        &mut self,
        device: LiveDeviceReport,
        producer: ProducerLoadSnapshot,
        buffer_frames: u32,
        now: Instant,
    ) -> EnginePressureSnapshot {
        if self.stream_id != device.stream_id {
            self.stream_id = device.stream_id;
            self.callback_deadline_misses = 0;
            self.callback_errors = 0;
            self.ring_refusals = 0;
            self.max_callback_gap_nanos = 0;
            self.recent_cause = EnginePressureCause::Healthy;
            self.recent_level = EnginePressureLevel::Normal;
            self.recent_until = None;
            self.hiccup_start = None;
            self.last_incident = None;
            self.hiccup_repeated = false;
        }
        let callback_deadline_misses = device
            .realtime_load
            .callbacks_over_100_percent
            .saturating_sub(self.callback_deadline_misses);
        self.callback_deadline_misses = device.realtime_load.callbacks_over_100_percent;

        // A refusal the score caused is not the engine falling behind. The
        // candidate still latches and rolls back, and the footer and the log
        // tell the player. A score that asks for an orbit past the last bus
        // says nothing about whether this machine can keep up, so it does
        // not count as pressure.
        //
        // A refusal while the library still fetches the score's samples
        // does not count either: the engine turned nothing away and the
        // machine is not overloaded. That one is an atomic refusal, so it
        // comes off that count.
        let producer_refusals_total = producer.capacity_refusals();
        let producer_refusals = producer_refusals_total.saturating_sub(self.producer_refusals);
        self.producer_refusals = producer_refusals_total;

        let callback_errors = device.callback_errors.saturating_sub(self.callback_errors);
        self.callback_errors = device.callback_errors;
        let ring_refusals = device.ring_refusals.saturating_sub(self.ring_refusals);
        self.ring_refusals = device.ring_refusals;

        let callback_period = if device.realtime_load.last_callback_period_nanos > 0 {
            device.realtime_load.last_callback_period_nanos
        } else {
            u64::from(buffer_frames.max(1)).saturating_mul(1_000_000_000)
                / u64::from(device.realtime_load.sample_rate_hz.max(1))
        };
        let new_max_gap = device.max_callback_gap_nanos > self.max_callback_gap_nanos;
        self.max_callback_gap_nanos = device.max_callback_gap_nanos;
        let host_gap_nanos = if new_max_gap
            && device.max_callback_gap_nanos > callback_period.saturating_mul(2)
            && callback_deadline_misses == 0
            && device.realtime_load.peak_load_basis_points <= 10_000
        {
            device.max_callback_gap_nanos
        } else {
            0
        };

        let pressure = device.realtime_pressure;
        let hard_drops = pressure
            .window_voice_ceiling_drops
            .saturating_add(pressure.window_pending_ceiling_drops);
        let pool_misses = RealtimePool::ALL
            .into_iter()
            .map(|pool| pressure.window_pool_misses(pool))
            .fold(pressure.window_orbit_reverb_misses, u64::saturating_add);
        let semantic_capacity = if pressure.max_polyphony == 0 {
            MAX_POLYPHONY as u64
        } else {
            pressure.max_polyphony
        };
        let voice_pressure = pressure.active_voices > semantic_capacity
            || pressure.window_semantic_polyphony_fades > 0
            || pressure.window_refused_voices > 0
            || hard_drops > 0
            || pool_misses > 0;

        let immediate = if callback_deadline_misses > 0
            || device.realtime_load.slow_load_basis_points > 10_000
        {
            EnginePressureCause::DspOverload
        } else if host_gap_nanos > 0 {
            EnginePressureCause::HostStarvation
        } else if producer.slow_load_basis_points > 10_000 || producer.consecutive_over_budget > 0 {
            EnginePressureCause::ProducerOverload
        } else if producer_refusals > 0 || callback_errors > 0 || ring_refusals > 0 {
            // Producer refusals counted here are capacity refusals
            // (resource limits, a full ring, a callback error). A
            // rejected score is not recorded as one.
            EnginePressureCause::Refusal
        } else if voice_pressure {
            EnginePressureCause::VoicePressure
        } else {
            EnginePressureCause::Healthy
        };

        let load = device
            .realtime_load
            .slow_load_basis_points
            .max(u64::from(producer.slow_load_basis_points));
        // One isolated hiccup (a late callback, a host gap) happens on
        // healthy machines, so it only warns. Red needs sustained load, a
        // hiccup that lasts, or a second one soon after.
        let immediate_level = match immediate {
            EnginePressureCause::Healthy => EnginePressureLevel::Normal,
            EnginePressureCause::VoicePressure if hard_drops == 0 && pool_misses == 0 => {
                EnginePressureLevel::Warning
            }
            EnginePressureCause::VoicePressure
            | EnginePressureCause::DspOverload
            | EnginePressureCause::ProducerOverload
            | EnginePressureCause::HostStarvation
            | EnginePressureCause::Refusal => self.incident_level(now, load > 10_000),
        };
        if immediate != EnginePressureCause::Healthy {
            self.recent_cause = immediate;
            self.recent_level = immediate_level;
            self.recent_until = now.checked_add(RECENT_FAILURE_HOLD);
        }
        let (cause, cause_level) = if immediate != EnginePressureCause::Healthy {
            (immediate, immediate_level)
        } else if self.recent_until.is_some_and(|until| now < until) {
            (self.recent_cause, self.recent_level)
        } else {
            self.recent_cause = EnginePressureCause::Healthy;
            self.recent_level = EnginePressureLevel::Normal;
            self.recent_until = None;
            (EnginePressureCause::Healthy, EnginePressureLevel::Normal)
        };

        let level = match load {
            0..7_000 => EnginePressureLevel::Normal,
            7_000..9_000 => EnginePressureLevel::Caution,
            9_000..=10_000 => EnginePressureLevel::Warning,
            _ => EnginePressureLevel::Error,
        }
        .max(cause_level);

        EnginePressureSnapshot {
            device,
            producer,
            callback_deadline_misses,
            producer_refusals,
            host_gap_nanos,
            cause,
            level,
        }
    }
}

impl EnginePressureMonitor {
    /// Error if the load is sustained, the hiccup lasts past
    /// [`HICCUP_SPAN`], or it follows another within [`REPEAT_WINDOW`].
    /// Warning otherwise.
    fn incident_level(&mut self, now: Instant, sustained: bool) -> EnginePressureLevel {
        let continuing = self
            .last_incident
            .is_some_and(|last| now.saturating_duration_since(last) < HICCUP_SPAN);
        if !continuing {
            self.hiccup_repeated = self
                .last_incident
                .is_some_and(|last| now.saturating_duration_since(last) < REPEAT_WINDOW);
            self.hiccup_start = Some(now);
        }
        self.last_incident = Some(now);
        let lasting = self
            .hiccup_start
            .is_some_and(|start| now.saturating_duration_since(start) >= HICCUP_SPAN);
        if sustained || lasting || self.hiccup_repeated {
            EnginePressureLevel::Error
        } else {
            EnginePressureLevel::Warning
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustel_audio::{RealtimeLoadSnapshot, RealtimePressureSnapshot};

    fn report(load: u64) -> LiveDeviceReport {
        LiveDeviceReport {
            realtime_load: RealtimeLoadSnapshot {
                sample_rate_hz: 48_000,
                last_callback_frames: 128,
                last_callback_period_nanos: 2_666_666,
                fast_load_basis_points: load,
                slow_load_basis_points: load,
                peak_load_basis_points: load,
                ..RealtimeLoadSnapshot::default()
            },
            ..LiveDeviceReport::default()
        }
    }

    fn producer(load: u32) -> ProducerLoadSnapshot {
        ProducerLoadSnapshot {
            fast_load_basis_points: load,
            slow_load_basis_points: load,
            peak_load_basis_points: load,
            cover_end_nanos: 420_000_000,
            ..ProducerLoadSnapshot::default()
        }
    }

    #[test]
    fn max_polyphony_pressure_uses_the_effective_callback_limit() {
        let mut device = report(1_000);
        device.realtime_pressure.max_polyphony = 256;
        device.realtime_pressure.active_voices = 192;
        let sample =
            EnginePressureMonitor::default().sample_at(device, producer(0), 128, Instant::now());
        assert_eq!(sample.semantic_voice_capacity(), 256);
        assert_eq!(sample.cause, EnginePressureCause::Healthy);
        device.realtime_pressure.max_polyphony = 64;
        let sample =
            EnginePressureMonitor::default().sample_at(device, producer(0), 128, Instant::now());
        assert_eq!(sample.semantic_voice_capacity(), 64);
        assert_eq!(sample.cause, EnginePressureCause::VoicePressure);
    }

    #[test]
    fn sustained_load_uses_the_documented_four_bands() {
        for (load, expected) in [
            (6_999, EnginePressureLevel::Normal),
            (7_000, EnginePressureLevel::Caution),
            (8_999, EnginePressureLevel::Caution),
            (9_000, EnginePressureLevel::Warning),
            (10_000, EnginePressureLevel::Warning),
            (10_001, EnginePressureLevel::Error),
        ] {
            let snapshot = EnginePressureMonitor::default().sample_at(
                report(load),
                producer(0),
                128,
                Instant::now(),
            );
            assert_eq!(snapshot.level, expected, "load {load}");
        }
    }

    #[test]
    fn dsp_producer_host_voice_and_refusal_are_distinct() {
        let now = Instant::now();

        let mut dsp = report(8_000);
        dsp.realtime_load.callbacks_over_100_percent = 1;
        assert_eq!(
            EnginePressureMonitor::default()
                .sample_at(dsp, producer(0), 128, now)
                .cause,
            EnginePressureCause::DspOverload
        );

        assert_eq!(
            EnginePressureMonitor::default()
                .sample_at(report(1_000), producer(10_001), 128, now)
                .cause,
            EnginePressureCause::ProducerOverload
        );

        let mut host = report(1_000);
        host.max_callback_gap_nanos = 20_000_000;
        assert_eq!(
            EnginePressureMonitor::default()
                .sample_at(host, producer(0), 128, now)
                .cause,
            EnginePressureCause::HostStarvation
        );

        // A committed refusal is pressure: a full ring, a limit, a
        // callback that errored.
        let mut refused = producer(1_000);
        refused.committed_refusals = 1;
        assert_eq!(
            EnginePressureMonitor::default()
                .sample_at(report(1_000), refused, 128, now)
                .cause,
            EnginePressureCause::Refusal
        );

        // A refusal the score's own content caused is not pressure. It
        // still latched, rolled back and was reported to the player.
        let mut rejected = producer(1_000);
        rejected.committed_refusals = 1;
        rejected.rejected_scores = 1;
        let healthy = EnginePressureMonitor::default().sample_at(report(1_000), rejected, 128, now);
        assert_eq!(healthy.cause, EnginePressureCause::Healthy);
        assert_eq!(healthy.level, EnginePressureLevel::Normal);

        // One of each in the same window still reports the capacity one.
        let mut both = producer(1_000);
        both.committed_refusals = 2;
        both.rejected_scores = 1;
        assert_eq!(
            EnginePressureMonitor::default()
                .sample_at(report(1_000), both, 128, now)
                .cause,
            EnginePressureCause::Refusal
        );

        // A refusal while the library still fetches the score's samples
        // is not pressure either: the engine turned nothing away.
        let mut loading = producer(1_000);
        loading.atomic_refusals = 2;
        loading.loading_refusals = 1;
        let waiting = EnginePressureMonitor::default().sample_at(report(1_000), loading, 128, now);
        assert_eq!(waiting.producer_refusals, 1);
        assert_eq!(waiting.cause, EnginePressureCause::Refusal);

        let mut only_loading = producer(1_000);
        only_loading.atomic_refusals = 1;
        only_loading.loading_refusals = 1;
        let healthy_wait =
            EnginePressureMonitor::default().sample_at(report(1_000), only_loading, 128, now);
        assert_eq!(healthy_wait.producer_refusals, 0);
        assert_eq!(healthy_wait.cause, EnginePressureCause::Healthy);
        assert_eq!(healthy_wait.level, EnginePressureLevel::Normal);

        let mut voices = report(1_000);
        voices.realtime_pressure = RealtimePressureSnapshot {
            active_voices: MAX_POLYPHONY as u64 + 1,
            ..RealtimePressureSnapshot::default()
        };
        assert_eq!(
            EnginePressureMonitor::default()
                .sample_at(voices, producer(0), 128, now)
                .cause,
            EnginePressureCause::VoicePressure
        );

        let mut refusal = producer(1_000);
        refusal.atomic_refusals = 1;
        assert_eq!(
            EnginePressureMonitor::default()
                .sample_at(report(1_000), refusal, 128, now)
                .cause,
            EnginePressureCause::Refusal
        );

        let mut saturated_and_refused = producer(10_001);
        saturated_and_refused.atomic_refusals = 1;
        assert_eq!(
            EnginePressureMonitor::default()
                .sample_at(report(1_000), saturated_and_refused, 128, now)
                .cause,
            EnginePressureCause::ProducerOverload,
            "a generic refusal must not hide the measured saturated side"
        );
    }

    #[test]
    fn a_recent_failure_lingers_then_expires_without_recounting() {
        let now = Instant::now();
        let mut monitor = EnginePressureMonitor::default();
        let mut host = report(1_000);
        host.max_callback_gap_nanos = 20_000_000;
        assert_eq!(
            monitor.sample_at(host, producer(0), 128, now).cause,
            EnginePressureCause::HostStarvation
        );
        assert_eq!(
            monitor
                .sample_at(host, producer(0), 128, now + Duration::from_secs(1),)
                .cause,
            EnginePressureCause::HostStarvation
        );
        assert_eq!(
            monitor
                .sample_at(host, producer(0), 128, now + Duration::from_secs(3),)
                .cause,
            EnginePressureCause::Healthy
        );
    }

    #[test]
    fn the_shared_snapshot_exposes_exact_status_values_and_capacities() {
        let mut device = report(2_400);
        device.realtime_pressure.active_voices = 18;
        device.realtime_pressure.peak_active_voices = 23;
        let snapshot = EnginePressureMonitor::default().sample_at(
            device,
            producer(1_100),
            128,
            Instant::now(),
        );
        assert_eq!(snapshot.dsp_load_basis_points(), 2_400);
        assert_eq!(snapshot.scheduler_load_basis_points(), 1_100);
        assert_eq!(snapshot.cover_millis(), 420);
        assert_eq!(snapshot.active_voices(), 18);
        assert_eq!(snapshot.peak_active_voices(), 23);
        assert_eq!(snapshot.semantic_voice_capacity(), 128);
        assert_eq!(snapshot.hard_voice_capacity(), 512);
        assert_eq!(snapshot.pending_capacity(), 8_192);
    }

    #[test]
    fn a_recycled_stream_restarts_callback_deltas_without_recounting_producer_failures() {
        let now = Instant::now();
        let mut monitor = EnginePressureMonitor::default();
        let mut first_device = report(1_000);
        first_device.stream_id = 1;
        first_device.realtime_load.callbacks_over_100_percent = 5;
        let mut first_producer = producer(1_000);
        first_producer.atomic_refusals = 7;
        let first = monitor.sample_at(first_device, first_producer, 128, now);
        assert_eq!(first.callback_deadline_misses, 5);
        assert_eq!(first.producer_refusals, 7);

        let mut recycled_device = report(1_000);
        recycled_device.stream_id = 2;
        recycled_device.realtime_load.callbacks_over_100_percent = 2;
        let mut same_producer = producer(1_000);
        same_producer.atomic_refusals = 7;
        let recycled = monitor.sample_at(
            recycled_device,
            same_producer,
            128,
            now + Duration::from_secs(3),
        );
        assert_eq!(recycled.callback_deadline_misses, 2);
        assert_eq!(recycled.producer_refusals, 0);
    }

    /// The level read after one late callback at each offset in `at`.
    fn levels_for_misses_at(now: Instant, at: &[Duration]) -> Vec<EnginePressureLevel> {
        let mut monitor = EnginePressureMonitor::default();
        let mut device = report(2_000);
        at.iter()
            .map(|offset| {
                device.realtime_load.callbacks_over_100_percent += 1;
                monitor
                    .sample_at(device, producer(0), 128, now + *offset)
                    .level
            })
            .collect()
    }

    #[test]
    fn one_isolated_hiccup_warns_and_does_not_turn_red() {
        let now = Instant::now();
        let mut monitor = EnginePressureMonitor::default();
        let mut device = report(2_000);
        device.realtime_load.callbacks_over_100_percent = 1;
        let first = monitor.sample_at(device, producer(0), 128, now);
        assert_eq!(first.cause, EnginePressureCause::DspOverload);
        assert_eq!(first.level, EnginePressureLevel::Warning);
        // Held for the usual moment at the same level, then gone.
        let held = monitor.sample_at(device, producer(0), 128, now + Duration::from_secs(1));
        assert_eq!(held.cause, EnginePressureCause::DspOverload);
        assert_eq!(held.level, EnginePressureLevel::Warning);
        let later = monitor.sample_at(device, producer(0), 128, now + Duration::from_secs(3));
        assert_eq!(later.level, EnginePressureLevel::Normal);
    }

    #[test]
    fn one_hiccup_seen_by_consecutive_frames_is_still_one() {
        // At 240 fps a stall spans several samples; each sees a new miss.
        let levels = levels_for_misses_at(
            Instant::now(),
            &[
                Duration::ZERO,
                Duration::from_millis(4),
                Duration::from_millis(8),
                Duration::from_millis(100),
            ],
        );
        assert!(
            levels
                .iter()
                .all(|level| *level == EnginePressureLevel::Warning),
            "{levels:?}"
        );
    }

    #[test]
    fn a_hiccup_that_lasts_turns_red() {
        let offsets = (0..8)
            .map(|step| Duration::from_millis(step * 100))
            .collect::<Vec<_>>();
        let levels = levels_for_misses_at(Instant::now(), &offsets);
        assert_eq!(levels[0], EnginePressureLevel::Warning);
        assert_eq!(
            levels.last(),
            Some(&EnginePressureLevel::Error),
            "{levels:?}"
        );
    }

    #[test]
    fn a_second_hiccup_soon_after_the_first_turns_red() {
        let levels =
            levels_for_misses_at(Instant::now(), &[Duration::ZERO, Duration::from_secs(4)]);
        assert_eq!(
            levels,
            [EnginePressureLevel::Warning, EnginePressureLevel::Error]
        );
    }

    #[test]
    fn hiccups_far_apart_each_only_warn() {
        let levels = levels_for_misses_at(
            Instant::now(),
            &[
                Duration::ZERO,
                Duration::from_secs(15),
                Duration::from_secs(30),
            ],
        );
        assert!(
            levels
                .iter()
                .all(|level| *level == EnginePressureLevel::Warning),
            "{levels:?}"
        );
    }

    #[test]
    fn a_miss_under_sustained_overload_is_red_at_once() {
        let mut device = report(10_500);
        device.realtime_load.callbacks_over_100_percent = 1;
        let snapshot =
            EnginePressureMonitor::default().sample_at(device, producer(0), 128, Instant::now());
        assert_eq!(snapshot.level, EnginePressureLevel::Error);
    }
}
