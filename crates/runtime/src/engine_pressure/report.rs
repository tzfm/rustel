//! Versioned machine-readable projection of live engine pressure.

use rustel_audio::RealtimePool;
use serde::Serialize;

use crate::{ProducerPhase, ProducerTurnOutcome};

use super::EnginePressureSnapshot;

pub const ENGINE_PRESSURE_SCHEMA_VERSION: u32 = 1;

/// Producer-owned facts that do not belong to the callback snapshot.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct EnginePressureReportContext {
    pub device_name: Option<String>,
    pub requested_buffer_frames: Option<u32>,
    pub reported_buffer_frames: Option<u32>,
    pub process_cpu_percent: Option<f64>,
    pub process_resident_bytes: Option<u64>,
}

/// Stable machine-readable projection of [`EnginePressureSnapshot`].
///
/// The version belongs to this projection rather than to the internal meter
/// structs, which may gain fields without silently changing the wire contract.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct EnginePressureReportV1 {
    pub schema_version: u32,
    pub status: Option<EnginePressureStatusV1>,
    pub dsp: Option<EnginePressureDspV1>,
    pub scheduler: Option<EnginePressureSchedulerV1>,
    pub voices: Option<EnginePressureVoicesV1>,
    pub queues: Option<EnginePressureQueuesV1>,
    pub pools: Option<EnginePressurePoolsV1>,
    pub device: Option<EnginePressureDeviceV1>,
    pub process: EnginePressureProcessV1,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct EnginePressureStatusV1 {
    pub level: &'static str,
    pub cause: &'static str,
    pub message: &'static str,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct EnginePressureDspV1 {
    pub publication: u64,
    pub fast_load_basis_points: u64,
    pub slow_load_basis_points: u64,
    pub peak_load_basis_points: u64,
    pub callback_busy_nanos: u64,
    pub callback_period_nanos: u64,
    pub total_callbacks: u64,
    pub callbacks_over_80_percent: u64,
    pub callbacks_over_90_percent: u64,
    pub callbacks_over_100_percent: u64,
    pub callbacks_over_125_percent: u64,
    pub recent_deadline_misses: u64,
    pub consecutive_over_budget: u64,
    pub max_callback_busy_nanos: u64,
    pub max_callback_gap_nanos: u64,
    pub recent_host_gap_nanos: u64,
    pub callback_errors: u64,
    pub callback_scope_misses: u64,
    pub callback_allocations: u64,
    pub callback_frees: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct EnginePressureSchedulerV1 {
    pub publication: u64,
    pub turns: u64,
    pub window_index: u64,
    pub window_turns: u64,
    pub progress_turns: u64,
    pub idle_turns: u64,
    pub last_outcome: &'static str,
    pub last_load_basis_points: u32,
    pub fast_load_basis_points: u32,
    pub slow_load_basis_points: u32,
    pub peak_load_basis_points: u32,
    pub consecutive_over_budget: u64,
    pub turn_p50_nanos: u64,
    pub turn_p95_nanos: u64,
    pub turn_p99_nanos: u64,
    pub turn_max_nanos: u64,
    pub cover_start_nanos: u64,
    pub cover_end_nanos: u64,
    pub cover_low_water_nanos: u64,
    pub cover_gained_nanos: u64,
    pub continuation_reserve_nanos: u64,
    pub budget_granted_nanos: u64,
    pub atomic_refusals: u64,
    pub committed_refusals: u64,
    pub recent_refusals: u64,
    pub cancelled_turns: u64,
    pub stopped_turns: u64,
    pub rollback_count: u64,
    pub gap_resync_count: u64,
    pub queue_saturation_count: u64,
    pub haps: u64,
    pub scheduler_events: u64,
    pub converted_audio_events: u64,
    pub ring_pushes: u64,
    pub js_callback_calls: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct EnginePressureVoicesV1 {
    pub active: u64,
    pub peak: u64,
    pub semantic_capacity: u64,
    pub hard_capacity: u64,
    pub pending: u64,
    pub peak_pending: u64,
    pub pending_capacity: u64,
    pub active_orbits: u64,
    pub peak_active_orbits: u64,
    pub active_orbit_delays: u64,
    pub active_orbit_reverbs: u64,
    pub active_dj_filters: u64,
    pub semantic_polyphony_fades: u64,
    pub recent_semantic_polyphony_fades: u64,
    pub voice_ceiling_drops: u64,
    pub recent_voice_ceiling_drops: u64,
    pub pending_ceiling_drops: u64,
    pub recent_pending_ceiling_drops: u64,
    pub refused: u64,
    pub recent_refused: u64,
    pub late_events: u64,
    pub recent_late_events: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct EnginePressureQueueV1 {
    pub depth: u64,
    pub peak_depth: u64,
    pub capacity: u64,
    pub refusals_or_drops: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct EnginePressureAssetQueuesV1 {
    pub depth: u64,
    pub capacity_each: u64,
    pub leaked: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct EnginePressureQueuesV1 {
    pub event_ring: Option<EnginePressureQueueV1>,
    pub producer_backlog: Option<EnginePressureQueueV1>,
    pub scheduler: Option<EnginePressureQueueV1>,
    pub scheduler_trace: Option<EnginePressureQueueV1>,
    pub assets: Option<EnginePressureAssetQueuesV1>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct EnginePressurePoolValuesV1 {
    pub compressor: u64,
    pub fx_delay: u64,
    pub stretch: u64,
    pub zzfx_delay: u64,
    pub fx_reverb: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct EnginePressurePoolsV1 {
    pub active: EnginePressurePoolValuesV1,
    pub misses: EnginePressurePoolValuesV1,
    pub recent_misses: EnginePressurePoolValuesV1,
    pub orbit_reverb_misses: u64,
    pub recent_orbit_reverb_misses: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct EnginePressureDeviceV1 {
    pub name: Option<String>,
    pub stream_id: u64,
    pub generation: u64,
    pub sample_rate_hz: Option<u32>,
    pub requested_buffer_frames: Option<u32>,
    pub reported_buffer_frames: Option<u32>,
    pub callback_frames: Option<u32>,
    pub callbacks: u64,
    pub playback_latency_nanos: u64,
    pub max_playback_latency_nanos: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct EnginePressureProcessV1 {
    pub cpu_percent: Option<f64>,
    pub resident_bytes: Option<u64>,
}

impl EnginePressureSnapshot {
    pub fn report_v1(self, context: EnginePressureReportContext) -> EnginePressureReportV1 {
        let realtime_load = self.device.realtime_load;
        let pressure = self.device.realtime_pressure;
        let producer = self.producer;
        let dsp_available = realtime_load.publication > 0;
        let scheduler_available = producer.publication > 0;
        let pressure_available = pressure.publication > 0;
        let device_available = self.device.stream_id > 0 || dsp_available || pressure_available;
        let status_available = dsp_available || scheduler_available || pressure_available;
        let total_turn = producer.phase(ProducerPhase::Total);
        let asset_queues = self.device.asset_queues;

        EnginePressureReportV1 {
            schema_version: ENGINE_PRESSURE_SCHEMA_VERSION,
            status: status_available.then_some(EnginePressureStatusV1 {
                level: self.level.code(),
                cause: self.cause.code(),
                message: self.cause.label(),
            }),
            dsp: dsp_available.then_some(EnginePressureDspV1 {
                publication: realtime_load.publication,
                fast_load_basis_points: realtime_load.fast_load_basis_points,
                slow_load_basis_points: realtime_load.slow_load_basis_points,
                peak_load_basis_points: realtime_load.peak_load_basis_points,
                callback_busy_nanos: realtime_load.last_callback_busy_nanos,
                callback_period_nanos: realtime_load.last_callback_period_nanos,
                total_callbacks: realtime_load.total_callbacks,
                callbacks_over_80_percent: realtime_load.callbacks_over_80_percent,
                callbacks_over_90_percent: realtime_load.callbacks_over_90_percent,
                callbacks_over_100_percent: realtime_load.callbacks_over_100_percent,
                callbacks_over_125_percent: realtime_load.callbacks_over_125_percent,
                recent_deadline_misses: self.callback_deadline_misses,
                consecutive_over_budget: realtime_load.consecutive_over_budget,
                max_callback_busy_nanos: realtime_load.max_callback_busy_nanos,
                max_callback_gap_nanos: self.device.max_callback_gap_nanos,
                recent_host_gap_nanos: self.host_gap_nanos,
                callback_errors: self.device.callback_errors,
                callback_scope_misses: self.device.callback_scope_misses,
                callback_allocations: self.device.callback_allocations,
                callback_frees: self.device.callback_frees,
            }),
            scheduler: scheduler_available.then_some(EnginePressureSchedulerV1 {
                publication: producer.publication,
                turns: producer.turns,
                window_index: producer.window_index,
                window_turns: producer.window_turns,
                progress_turns: producer.progress_turns,
                idle_turns: producer.idle_turns,
                last_outcome: producer_outcome_code(producer.last_outcome),
                last_load_basis_points: producer.last_load_basis_points,
                fast_load_basis_points: producer.fast_load_basis_points,
                slow_load_basis_points: producer.slow_load_basis_points,
                peak_load_basis_points: producer.peak_load_basis_points,
                consecutive_over_budget: producer.consecutive_over_budget,
                turn_p50_nanos: total_turn.p50_nanos,
                turn_p95_nanos: total_turn.p95_nanos,
                turn_p99_nanos: total_turn.p99_nanos,
                turn_max_nanos: total_turn.max_nanos,
                cover_start_nanos: producer.cover_start_nanos,
                cover_end_nanos: producer.cover_end_nanos,
                cover_low_water_nanos: producer.cover_low_water_nanos,
                cover_gained_nanos: producer.cover_gained_nanos,
                continuation_reserve_nanos: producer.continuation_reserve_nanos,
                budget_granted_nanos: producer.budget_granted_nanos,
                atomic_refusals: producer.atomic_refusals,
                committed_refusals: producer.committed_refusals,
                recent_refusals: self.producer_refusals,
                cancelled_turns: producer.cancelled_turns,
                stopped_turns: producer.stopped_turns,
                rollback_count: producer.rollback_count,
                gap_resync_count: producer.gap_resync_count,
                queue_saturation_count: producer.queue_saturation_count,
                haps: producer.haps,
                scheduler_events: producer.scheduler_events,
                converted_audio_events: producer.converted_audio_events,
                ring_pushes: producer.ring_pushes,
                js_callback_calls: producer.js_callback_calls,
            }),
            voices: pressure_available.then_some(EnginePressureVoicesV1 {
                active: pressure.active_voices,
                peak: pressure.peak_active_voices,
                semantic_capacity: self.semantic_voice_capacity(),
                hard_capacity: self.hard_voice_capacity(),
                pending: pressure.pending_events,
                peak_pending: pressure.peak_pending_events,
                pending_capacity: self.pending_capacity(),
                active_orbits: pressure.active_orbits,
                peak_active_orbits: pressure.peak_active_orbits,
                active_orbit_delays: pressure.active_orbit_delays,
                active_orbit_reverbs: pressure.active_orbit_reverbs,
                active_dj_filters: pressure.active_dj_filters,
                semantic_polyphony_fades: pressure.semantic_polyphony_fades,
                recent_semantic_polyphony_fades: pressure.window_semantic_polyphony_fades,
                voice_ceiling_drops: pressure.voice_ceiling_drops,
                recent_voice_ceiling_drops: pressure.window_voice_ceiling_drops,
                pending_ceiling_drops: pressure.pending_ceiling_drops,
                recent_pending_ceiling_drops: pressure.window_pending_ceiling_drops,
                refused: pressure.refused_voices,
                recent_refused: pressure.window_refused_voices,
                late_events: pressure.late_events,
                recent_late_events: pressure.window_late_events,
            }),
            queues: (device_available || scheduler_available).then_some(EnginePressureQueuesV1 {
                event_ring: device_available.then_some(EnginePressureQueueV1 {
                    depth: self.device.ring_depth,
                    peak_depth: self.device.ring_peak_depth,
                    capacity: self.device.ring_capacity,
                    refusals_or_drops: self.device.ring_refusals,
                }),
                producer_backlog: scheduler_available.then_some(EnginePressureQueueV1 {
                    depth: u64::from(producer.producer_backlog_depth),
                    peak_depth: u64::from(producer.peak_producer_backlog_depth),
                    capacity: u64::from(producer.producer_backlog_capacity),
                    refusals_or_drops: producer.queue_saturation_count,
                }),
                scheduler: scheduler_available.then_some(EnginePressureQueueV1 {
                    depth: u64::from(producer.scheduler_queue_depth),
                    peak_depth: u64::from(producer.peak_scheduler_queue_depth),
                    capacity: u64::from(producer.scheduler_queue_capacity),
                    refusals_or_drops: producer.queue_saturation_count,
                }),
                scheduler_trace: scheduler_available.then_some(EnginePressureQueueV1 {
                    depth: u64::from(producer.scheduler_trace_depth),
                    peak_depth: u64::from(producer.peak_scheduler_trace_depth),
                    capacity: u64::from(producer.scheduler_trace_capacity),
                    refusals_or_drops: producer.scheduler_trace_drops,
                }),
                assets: device_available.then_some(EnginePressureAssetQueuesV1 {
                    depth: asset_queues
                        .sample_installs
                        .saturating_add(asset_queues.sample_returns)
                        .saturating_add(asset_queues.orbit_reverb_installs)
                        .saturating_add(asset_queues.orbit_reverb_returns)
                        .saturating_add(asset_queues.fx_reverb_installs)
                        .saturating_add(asset_queues.fx_reverb_returns),
                    capacity_each: asset_queues.capacity,
                    leaked: asset_queues.leaked,
                }),
            }),
            pools: pressure_available.then_some(EnginePressurePoolsV1 {
                active: pool_values(|pool| pressure.active_pool_leases(pool)),
                misses: pool_values(|pool| pressure.pool_misses(pool)),
                recent_misses: pool_values(|pool| pressure.window_pool_misses(pool)),
                orbit_reverb_misses: pressure.orbit_reverb_misses,
                recent_orbit_reverb_misses: pressure.window_orbit_reverb_misses,
            }),
            device: device_available.then_some(EnginePressureDeviceV1 {
                name: context.device_name,
                stream_id: self.device.stream_id,
                generation: self.device.generation,
                sample_rate_hz: (realtime_load.sample_rate_hz > 0)
                    .then_some(realtime_load.sample_rate_hz),
                requested_buffer_frames: context.requested_buffer_frames,
                reported_buffer_frames: context.reported_buffer_frames,
                callback_frames: (realtime_load.last_callback_frames > 0)
                    .then_some(realtime_load.last_callback_frames),
                callbacks: self.device.callbacks,
                playback_latency_nanos: self.device.playback_latency_nanos,
                max_playback_latency_nanos: self.device.max_playback_latency_nanos,
            }),
            process: EnginePressureProcessV1 {
                cpu_percent: context
                    .process_cpu_percent
                    .filter(|value| value.is_finite()),
                resident_bytes: context.process_resident_bytes,
            },
        }
    }
}

fn producer_outcome_code(outcome: ProducerTurnOutcome) -> &'static str {
    match outcome {
        ProducerTurnOutcome::Progress => "progress",
        ProducerTurnOutcome::Idle => "idle",
        ProducerTurnOutcome::AtomicRefusal => "atomic_refusal",
        ProducerTurnOutcome::CommittedRefusal => "committed_refusal",
        ProducerTurnOutcome::Cancelled => "cancelled",
        ProducerTurnOutcome::Stopped => "stopped",
    }
}

fn pool_values(mut read: impl FnMut(RealtimePool) -> u64) -> EnginePressurePoolValuesV1 {
    EnginePressurePoolValuesV1 {
        compressor: read(RealtimePool::Compressor),
        fx_delay: read(RealtimePool::FxDelay),
        stretch: read(RealtimePool::Stretch),
        zzfx_delay: read(RealtimePool::ZzfxDelay),
        fx_reverb: read(RealtimePool::FxReverb),
    }
}
