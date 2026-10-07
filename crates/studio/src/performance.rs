use std::collections::VecDeque;
use std::time::{Duration, Instant};

use crossterm::event::{Event, KeyEventKind};
use serde::Serialize;

use rustel_runtime::EnginePressureReportV1;

const SCHEMA_VERSION: u32 = 1;

const PUBLICATION_INTERVAL: Duration = Duration::from_secs(1);
const MAX_PENDING_INPUTS: usize = 256;
const DURATION_BUCKET_UPPER_BOUNDS_NANOS: [u64; 18] = [
    250_000,
    500_000,
    1_000_000,
    2_000_000,
    4_000_000,
    8_000_000,
    12_000_000,
    16_667_000,
    25_000_000,
    33_300_000,
    50_000_000,
    75_000_000,
    100_000_000,
    150_000_000,
    250_000_000,
    500_000_000,
    1_000_000_000,
    u64::MAX,
];

#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "kebab-case")]
pub(super) enum Publication {
    Started,
    Running,
    Final,
}

#[derive(Clone, Debug, Serialize)]
pub(super) struct DurationHistogramSnapshot {
    pub samples: u64,
    pub upper_bounds_nanos: [u64; DURATION_BUCKET_UPPER_BOUNDS_NANOS.len()],
    pub bucket_counts: [u64; DURATION_BUCKET_UPPER_BOUNDS_NANOS.len()],
    pub max_nanos: u64,
}

#[derive(Clone, Debug, Serialize)]
pub(super) struct StudioPerformanceSnapshot {
    pub schema_version: u32,
    pub publication: Publication,
    pub frame_sequence: u64,
    pub frame_time: DurationHistogramSnapshot,
    pub input_to_paint: DurationHistogramSnapshot,
    pub input_samples_dropped: u64,
}

/// Hidden benchmark protocol projection of the engine state paired with one
/// terminal performance publication.
///
/// This is intentionally separate from [`StudioPerformanceSnapshot`]: UI
/// timing keeps its stable schema while transition collectors distinguish
/// producer publication from a consumer-confirmed finite window.
/// Neither observation proves physical delivery or a changed audible onset.
#[derive(Clone, Debug, Serialize)]
pub(super) struct StudioRuntimeSnapshot {
    pub schema_version: u32,
    pub publication: Publication,
    pub frame_sequence: u64,
    pub playing: bool,
    pub evaluating: bool,
    pub visible_error: Option<String>,
    pub session_generation: u64,
    pub published_generation: Option<u64>,
    pub confirmed_audio_generation: Option<u64>,
    pub source_revision: Option<String>,
    pub cps: f64,
    pub device_time_seconds: f64,
    pub submitted_frames: Option<u64>,
    pub callbacks: Option<u64>,
    pub accepted_events: Option<u64>,
    pub stale_events_filtered: Option<u64>,
    pub late_events: Option<u64>,
    pub ring_refusals: Option<u64>,
    pub callback_errors: Option<u64>,
    pub callback_scope_misses: Option<u64>,
    pub callback_allocations: Option<u64>,
    pub callback_frees: Option<u64>,
    pub allocator_tripwire_armed: Option<bool>,
    pub hydra_frames: Option<u64>,
    pub master_peak_db: f32,
    pub master_lufs: f32,
    pub process_cpu_percent: Option<f32>,
    pub process_resident_bytes: Option<u64>,
    pub pressure: Option<EnginePressureReportV1>,
}

#[derive(Clone, Copy, Debug, Serialize)]
pub(super) struct PaintedInput {
    pub schema_version: u32,
    pub sequence: u64,
    pub frame_sequence: u64,
    pub read_to_paint_nanos: u64,
}

#[derive(Default)]
struct DurationHistogram {
    counts: [u64; DURATION_BUCKET_UPPER_BOUNDS_NANOS.len()],
    samples: u64,
    max_nanos: u64,
}

impl DurationHistogram {
    fn record(&mut self, duration: Duration) {
        let nanos = u64::try_from(duration.as_nanos()).unwrap_or(u64::MAX);
        let bucket = DURATION_BUCKET_UPPER_BOUNDS_NANOS
            .iter()
            .position(|upper| nanos <= *upper)
            .unwrap_or(DURATION_BUCKET_UPPER_BOUNDS_NANOS.len() - 1);
        self.counts[bucket] = self.counts[bucket].saturating_add(1);
        self.samples = self.samples.saturating_add(1);
        self.max_nanos = self.max_nanos.max(nanos);
    }

    fn snapshot(&self) -> DurationHistogramSnapshot {
        DurationHistogramSnapshot {
            samples: self.samples,
            upper_bounds_nanos: DURATION_BUCKET_UPPER_BOUNDS_NANOS,
            bucket_counts: self.counts,
            max_nanos: self.max_nanos,
        }
    }
}

struct PendingInput {
    sequence: u64,
    read_at: Instant,
}

pub(super) struct Performance {
    enabled: bool,
    window_started: Instant,
    window_frames: u64,
    fps: f64,
    render_ms: f64,
    frame_sequence: u64,
    next_input_sequence: u64,
    pending_inputs: VecDeque<PendingInput>,
    input_samples_dropped: u64,
    frame_time: DurationHistogram,
    input_to_paint: DurationHistogram,
    next_publication: Instant,
}

impl Performance {
    pub(super) fn new(now: Instant, enabled: bool) -> Self {
        Self {
            enabled,
            window_started: now,
            window_frames: 0,
            fps: 0.0,
            render_ms: 0.0,
            frame_sequence: 0,
            next_input_sequence: 1,
            pending_inputs: VecDeque::new(),
            input_samples_dropped: 0,
            frame_time: DurationHistogram::default(),
            input_to_paint: DurationHistogram::default(),
            next_publication: now + PUBLICATION_INTERVAL,
        }
    }

    pub(super) fn enabled(&self) -> bool {
        self.enabled
    }

    pub(super) fn fps(&self) -> f64 {
        self.fps
    }

    pub(super) fn render_ms(&self) -> f64 {
        self.render_ms
    }

    pub(super) fn observe_input(&mut self, event: &Event, read_at: Instant) {
        if !self.enabled || !is_measured_input(event) {
            return;
        }
        let sequence = self.next_input_sequence;
        self.next_input_sequence = self.next_input_sequence.saturating_add(1);
        if self.pending_inputs.len() >= MAX_PENDING_INPUTS {
            self.input_samples_dropped = self.input_samples_dropped.saturating_add(1);
            return;
        }
        self.pending_inputs
            .push_back(PendingInput { sequence, read_at });
    }

    pub(super) fn record_frame(
        &mut self,
        elapsed: Duration,
        painted_at: Instant,
    ) -> Vec<PaintedInput> {
        let milliseconds = elapsed.as_secs_f64() * 1_000.0;
        self.render_ms = if self.window_frames == 0 {
            milliseconds
        } else {
            self.render_ms * 0.9 + milliseconds * 0.1
        };
        self.window_frames = self.window_frames.saturating_add(1);
        self.frame_sequence = self.frame_sequence.saturating_add(1);
        let window = painted_at.saturating_duration_since(self.window_started);
        if window >= Duration::from_secs(1) {
            self.fps = self.window_frames as f64 / window.as_secs_f64();
            self.window_frames = 0;
            self.window_started = painted_at;
        }

        if !self.enabled {
            return Vec::new();
        }
        self.frame_time.record(elapsed);
        let mut painted = Vec::with_capacity(self.pending_inputs.len());
        while let Some(input) = self.pending_inputs.pop_front() {
            let elapsed = painted_at.saturating_duration_since(input.read_at);
            self.input_to_paint.record(elapsed);
            painted.push(PaintedInput {
                schema_version: SCHEMA_VERSION,
                sequence: input.sequence,
                frame_sequence: self.frame_sequence,
                read_to_paint_nanos: u64::try_from(elapsed.as_nanos()).unwrap_or(u64::MAX),
            });
        }
        painted
    }

    pub(super) fn snapshot(&self, publication: Publication) -> Option<StudioPerformanceSnapshot> {
        self.enabled.then(|| StudioPerformanceSnapshot {
            schema_version: SCHEMA_VERSION,
            publication,
            frame_sequence: self.frame_sequence,
            frame_time: self.frame_time.snapshot(),
            input_to_paint: self.input_to_paint.snapshot(),
            input_samples_dropped: self.input_samples_dropped,
        })
    }

    pub(super) fn take_running_snapshot(
        &mut self,
        now: Instant,
    ) -> Option<StudioPerformanceSnapshot> {
        if !self.enabled || now < self.next_publication {
            return None;
        }
        self.next_publication = now + PUBLICATION_INTERVAL;
        self.snapshot(Publication::Running)
    }
}

fn is_measured_input(event: &Event) -> bool {
    match event {
        Event::Key(key) => matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat),
        Event::Mouse(_) | Event::Paste(_) => true,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    #[test]
    fn a_frame_correlates_every_pending_input_and_updates_cumulative_histograms() {
        let started = Instant::now();
        let mut performance = Performance::new(started, true);
        let key = Event::Key(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE));
        performance.observe_input(&key, started + Duration::from_millis(2));
        performance.observe_input(&key, started + Duration::from_millis(5));

        let painted = performance.record_frame(
            Duration::from_millis(4),
            started + Duration::from_millis(20),
        );

        assert_eq!(painted.len(), 2);
        assert_eq!(painted[0].sequence, 1);
        assert_eq!(painted[0].read_to_paint_nanos, 18_000_000);
        assert_eq!(painted[1].sequence, 2);
        assert_eq!(painted[1].read_to_paint_nanos, 15_000_000);
        let snapshot = performance
            .snapshot(Publication::Running)
            .expect("enabled telemetry");
        assert_eq!(snapshot.frame_sequence, 1);
        assert_eq!(snapshot.frame_time.samples, 1);
        assert_eq!(snapshot.frame_time.max_nanos, 4_000_000);
        assert_eq!(snapshot.input_to_paint.samples, 2);
        assert_eq!(snapshot.input_to_paint.max_nanos, 18_000_000);
        assert_eq!(snapshot.frame_time.bucket_counts.iter().sum::<u64>(), 1);
        assert_eq!(snapshot.input_to_paint.bucket_counts.iter().sum::<u64>(), 2);
    }

    #[test]
    fn disabled_telemetry_keeps_the_existing_display_meter_without_samples() {
        let started = Instant::now();
        let mut performance = Performance::new(started, false);
        let key = Event::Key(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE));
        performance.observe_input(&key, started);
        assert!(
            performance
                .record_frame(Duration::from_millis(5), started + Duration::from_secs(1))
                .is_empty()
        );
        assert_eq!(performance.render_ms(), 5.0);
        assert_eq!(performance.fps(), 1.0);
        assert!(performance.snapshot(Publication::Final).is_none());
    }

    #[test]
    fn pending_input_memory_is_bounded_and_drops_are_reported() {
        let started = Instant::now();
        let mut performance = Performance::new(started, true);
        let key = Event::Key(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE));
        for _ in 0..MAX_PENDING_INPUTS + 3 {
            performance.observe_input(&key, started);
        }
        let painted = performance.record_frame(Duration::ZERO, started);
        assert_eq!(painted.len(), MAX_PENDING_INPUTS);
        assert_eq!(
            performance
                .snapshot(Publication::Final)
                .expect("snapshot")
                .input_samples_dropped,
            3
        );
    }

    #[test]
    fn publications_are_cumulative_versioned_and_rate_limited() {
        let started = Instant::now();
        let mut performance = Performance::new(started, true);
        assert!(
            performance
                .take_running_snapshot(started + Duration::from_millis(999))
                .is_none()
        );
        performance.record_frame(Duration::from_millis(2), started + Duration::from_secs(1));
        let snapshot = performance
            .take_running_snapshot(started + Duration::from_secs(1))
            .expect("one-second publication");
        let event = serde_json::json!({ "studio_performance": snapshot });
        assert_eq!(
            event.pointer("/studio_performance/schema_version"),
            Some(&serde_json::json!(SCHEMA_VERSION))
        );
        assert_eq!(
            event.pointer("/studio_performance/publication"),
            Some(&serde_json::json!("running"))
        );
        assert_eq!(
            event.pointer("/studio_performance/frame_time/samples"),
            Some(&serde_json::json!(1))
        );
        assert!(
            performance
                .take_running_snapshot(started + Duration::from_millis(1_999))
                .is_none()
        );
    }
}
