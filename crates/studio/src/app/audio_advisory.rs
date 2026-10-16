//! A quiet footer hint for sustained audio trouble at a small live buffer.
//!
//! Counters are cumulative because optional worker snapshots can be dropped.
//! One late callback, startup work, and old pressure labels are not warnings.

use std::time::{Duration, Instant};

use super::{App, StudioSnapshot};
use rustel_runtime::ProducerLoadSnapshot;

const WARMUP: Duration = Duration::from_secs(2);
const WINDOW: Duration = Duration::from_secs(1);
const REQUIRED_WINDOWS: u8 = 3;
// One bit per retained producer window, set when that window was affected;
// bit 0 is the most recently completed window.
const PRODUCER_WINDOWS: u8 = 0b1_1111;
const RECOVERY: Duration = Duration::from_secs(5);
const MAX_SAMPLE_GAP: Duration = Duration::from_secs(1);

pub(super) const MESSAGE: &str =
    "Audio struggling · try raising audio out latency in Settings → Advanced";

/// Changes to these stream properties reset the observation baseline.
/// Include rate and buffer size because recycling can retain the stream ID.
/// Individual callback lengths can vary without changing the stream.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Stream {
    id: u64,
    sample_rate: u32,
    buffer_frames: u32,
}

/// The host-reported buffer size, falling back to the requested size.
/// This matches the pressure monitor's period and does not vary per callback.
fn buffer_frames(snapshot: &StudioSnapshot) -> Option<u32> {
    let output = snapshot.device.as_ref()?.audio.output();
    Some(
        output
            .reported_buffer_frames()
            .unwrap_or_else(|| output.requested_buffer_frames()),
    )
}

#[derive(Clone, Copy)]
struct ProducerCounters {
    turns: u64,
    gaps: u64,
    refusals: u64,
}

impl ProducerCounters {
    fn read(load: &ProducerLoadSnapshot) -> Option<Self> {
        (load.publication != 0).then_some(Self {
            turns: load.turns,
            gaps: load.gap_resync_count,
            refusals: load.capacity_refusals(),
        })
    }

    fn restarted_since(self, previous: Self) -> bool {
        self.turns < previous.turns
            || self.gaps < previous.gaps
            || self.refusals < previous.refusals
    }
}

struct Observation {
    stream: Stream,
    callbacks: u64,
    misses: u64,
    late_events: u64,
    producer: Option<ProducerCounters>,
    sampled_at: Instant,
    warm_until: Instant,
    window_started: Option<Instant>,
    window_affected: bool,
    affected_windows: u8,
    producer_window_affected: bool,
    producer_windows: u8,
    last_starvation: Option<Instant>,
    last_miss: Option<Instant>,
}

#[derive(Default)]
pub(super) struct AudioAdvisory {
    observation: Option<Observation>,
    visible: bool,
}

impl AudioAdvisory {
    pub(super) fn message(&self) -> Option<&'static str> {
        self.visible.then_some(MESSAGE)
    }

    pub(super) fn reset(&mut self) -> bool {
        let changed = self.visible;
        *self = Self::default();
        changed
    }

    pub(super) fn expire(&mut self, now: Instant) -> bool {
        if self.visible
            && self.observation.as_ref().is_some_and(|observation| {
                observation
                    .last_miss
                    .is_none_or(|last| now.saturating_duration_since(last) >= RECOVERY)
            })
        {
            self.visible = false;
            if let Some(observation) = self.observation.as_mut() {
                observation.affected_windows = 0;
                observation.window_affected = false;
                observation.producer_windows = 0;
                observation.producer_window_affected = false;
            }
            return true;
        }
        false
    }

    /// Returns whether the footer changed. All timing comes from UI receipt
    /// times; queued or repeated snapshots cannot manufacture elapsed windows.
    pub(super) fn observe(&mut self, snapshot: &StudioSnapshot, now: Instant) -> bool {
        let was_visible = self.visible;
        self.observe_inner(snapshot, now);
        self.expire(now);
        self.visible != was_visible
    }

    fn observe_inner(&mut self, snapshot: &StudioSnapshot, now: Instant) {
        if !snapshot.playing || snapshot.stopping {
            self.reset();
            return;
        }
        let Some(pressure) = snapshot.pressure.as_ref() else {
            return;
        };
        if self
            .observation
            .as_ref()
            .is_some_and(|previous| previous.stream.id != pressure.device.stream_id)
        {
            self.reset();
        }
        let load = &pressure.device.realtime_load;
        // A failed bounded telemetry read produces an empty sample. It is
        // unavailable, not proof of recovery or a new audio stream.
        if load.publication == 0
            || load.total_callbacks == 0
            || load.sample_rate_hz == 0
            || load.last_callback_frames == 0
        {
            return;
        }
        let Some(buffer_frames) = buffer_frames(snapshot) else {
            return;
        };
        // Limit this advice to small stream buffers. The preference and the
        // most recent callback length can differ from the host's buffer size.
        if buffer_frames >= 128 {
            self.reset();
            return;
        }
        let stream = Stream {
            id: pressure.device.stream_id,
            sample_rate: load.sample_rate_hz,
            buffer_frames,
        };
        let producer = ProducerCounters::read(&pressure.producer);
        let needs_baseline = self.observation.as_ref().is_none_or(|previous| {
            previous.stream != stream
                || load.total_callbacks < previous.callbacks
                || load.callbacks_over_100_percent < previous.misses
                || pressure.device.late_events < previous.late_events
                || producer
                    .zip(previous.producer)
                    .is_some_and(|(now, before)| now.restarted_since(before))
                || now.saturating_duration_since(previous.sampled_at) > MAX_SAMPLE_GAP
        });
        if needs_baseline {
            self.visible = false;
            self.observation = Some(Observation {
                stream,
                callbacks: load.total_callbacks,
                misses: load.callbacks_over_100_percent,
                late_events: pressure.device.late_events,
                producer,
                sampled_at: now,
                warm_until: now + WARMUP,
                window_started: None,
                window_affected: false,
                affected_windows: 0,
                producer_window_affected: false,
                producer_windows: 0,
                last_starvation: None,
                last_miss: None,
            });
            return;
        }
        let observation = self.observation.as_mut().expect("baseline installed");
        let producer_pair = producer.zip(observation.producer);
        if load.total_callbacks == observation.callbacks
            && producer_pair.is_none_or(|(now, before)| now.turns <= before.turns)
        {
            return;
        }
        let missed = load.callbacks_over_100_percent > observation.misses;
        let starved = pressure.device.late_events > observation.late_events
            || producer_pair.is_some_and(|(now, before)| now.gaps > before.gaps);
        let refused = producer_pair.is_some_and(|(now, before)| now.refusals > before.refusals);
        observation.callbacks = load.total_callbacks;
        observation.misses = load.callbacks_over_100_percent;
        observation.late_events = pressure.device.late_events;
        // Preserve the producer baseline across unavailable reads so the next
        // successful read still includes counters accumulated in the gap.
        if producer.is_some() {
            observation.producer = producer;
        }
        observation.sampled_at = now;
        if now < observation.warm_until {
            return;
        }
        let Some(window_started) = observation.window_started else {
            // Rebaseline at warmup's end too: misses in the interval crossing
            // that boundary must not leak into the first eligible window.
            observation.window_started = Some(now);
            return;
        };
        if now.saturating_duration_since(window_started) >= WINDOW {
            observation.affected_windows = if observation.window_affected {
                observation.affected_windows.saturating_add(1)
            } else {
                0
            };
            observation.window_started = Some(window_started + WINDOW);
            observation.window_affected = false;
            // Query recovery is intermittent, unlike callback DSP work. A
            // quiet second between recoveries must not hide sustained trouble;
            // a single hiccup spanning two windows still cannot activate this.
            observation.producer_windows = ((observation.producer_windows << 1)
                | u8::from(observation.producer_window_affected))
                & PRODUCER_WINDOWS;
            observation.producer_window_affected = false;
            if observation.affected_windows >= REQUIRED_WINDOWS
                || observation.producer_windows.count_ones() >= u32::from(REQUIRED_WINDOWS)
            {
                self.visible = true;
            }
        }
        if starved {
            observation.last_starvation = Some(now);
        }
        // Refusals include more than missed deadlines. Count them only near
        // measured audio starvation, not on their own or from a retained label.
        let producer_struggling = starved
            || (refused
                && observation
                    .last_starvation
                    .is_some_and(|last| now.saturating_duration_since(last) < RECOVERY));
        if producer_struggling {
            observation.producer_window_affected = true;
        }
        if missed {
            observation.window_affected = true;
        }
        if missed || producer_struggling {
            observation.last_miss = Some(now);
        }
    }
}

impl App {
    pub(super) fn audio_warning(&self) -> Option<&'static str> {
        (!self.stop_requested)
            .then(|| self.audio_advisory.message())
            .flatten()
    }

    pub(super) fn footer_notice_rows(&self) -> u16 {
        u16::from(self.errors.visible().is_some()) + u16::from(self.audio_warning().is_some())
    }
}

#[cfg(test)]
mod tests {
    mod audio_advisory {
        //! Tests for `app/audio_advisory.rs`.

        use std::sync::{Arc, OnceLock};

        use crate::app::audio_advisory::*;
        use crate::app::*;
        use rustel_audio::{LiveDeviceReport, RealtimeLoadSnapshot};
        use rustel_runtime::{EnginePressureCause, EnginePressureSnapshot, ProducerLoadSnapshot};

        /// The opened stream's facts: what was asked for, and what the host said
        /// it granted.
        fn device(requested: u32, reported: Option<u32>) -> StudioDeviceInfo {
            static REGISTRY: OnceLock<Arc<rustel_runtime::CapabilityRegistry>> = OnceLock::new();
            StudioDeviceInfo {
                stream_id: 1,
                registry: Arc::clone(
                    REGISTRY
                        .get_or_init(|| Arc::new(rustel_runtime::capability_registry().clone())),
                ),
                audio: rustel_runtime::AudioStreamFacts::new(
                    rustel_runtime::AudioOutputFacts::new(
                        rustel_runtime::AudioHost::cpal("pulseaudio"),
                        "test output",
                        48_000,
                        2,
                        Some(rustel_runtime::AudioSampleFormat::F32),
                        requested,
                        reported,
                    ),
                    None,
                ),
                allocator_tripwire_armed: true,
            }
        }

        /// A playing stream whose buffer, and the callback that last published,
        /// are both `frames`.
        fn snapshot(tick: u64, misses: u64, frames: u32) -> StudioSnapshot {
            StudioSnapshot {
                playing: true,
                device: Some(device(frames, Some(frames))),
                pressure: Some(EnginePressureSnapshot {
                    device: LiveDeviceReport {
                        stream_id: 1,
                        realtime_load: RealtimeLoadSnapshot {
                            publication: tick + 1,
                            sample_rate_hz: 48_000,
                            last_callback_frames: frames,
                            total_callbacks: 10_000 + tick * 16,
                            callbacks_over_100_percent: misses,
                            ..Default::default()
                        },
                        ..Default::default()
                    },
                    // The old classifier can linger; the advisory must use fresh
                    // counters, not this label or its per-projection delta.
                    cause: EnginePressureCause::DspOverload,
                    callback_deadline_misses: 100,
                    ..Default::default()
                }),
                ..Default::default()
            }
        }

        fn at(start: Instant, tick: u64) -> Instant {
            start + Duration::from_millis(tick * 100)
        }

        fn producer_snapshot(
            tick: u64,
            gaps: u64,
            refusals: u64,
            late_events: u64,
        ) -> StudioSnapshot {
            let mut snapshot = snapshot(tick, 0, 32);
            let pressure = snapshot.pressure.as_mut().unwrap();
            pressure.device.late_events = late_events;
            pressure.producer = ProducerLoadSnapshot {
                publication: tick + 1,
                turns: 10_000 + tick,
                gap_resync_count: gaps,
                atomic_refusals: refusals,
                ..Default::default()
            };
            snapshot
        }

        fn struggling(start: Instant) -> (AudioAdvisory, StudioSnapshot) {
            let mut advisory = AudioAdvisory::default();
            let mut latest = snapshot(0, 500, 32);
            for tick in 0..=50 {
                latest = snapshot(tick, 500 + tick, 32);
                advisory.observe(&latest, at(start, tick));
                assert_eq!(advisory.message().is_some(), tick == 50, "tick {tick}");
            }
            (advisory, latest)
        }

        #[test]
        fn historical_and_warmup_misses_never_become_a_warning() {
            let start = Instant::now();
            let mut advisory = AudioAdvisory::default();
            for tick in 0..=80 {
                advisory.observe(&snapshot(tick, 500 + tick.min(20), 32), at(start, tick));
                assert!(advisory.message().is_none(), "tick {tick}");
            }
        }

        #[test]
        fn one_hiccup_across_a_window_boundary_stays_quiet() {
            let start = Instant::now();
            let mut advisory = AudioAdvisory::default();
            let mut misses = 500;
            for tick in 0..=90 {
                if tick == 29 || tick == 30 {
                    misses += 200;
                }
                advisory.observe(&snapshot(tick, misses, 32), at(start, tick));
                assert!(advisory.message().is_none(), "tick {tick}");
            }
        }

        #[test]
        fn recurring_producer_skips_warn_across_quiet_seconds_without_callback_overload() {
            let start = Instant::now();
            for late_events in [false, true] {
                let mut advisory = AudioAdvisory::default();
                let mut skips = 100;
                for tick in 0..=111 {
                    if [21, 41, 61].contains(&tick) {
                        skips += 1;
                    }
                    let snapshot = if late_events {
                        producer_snapshot(tick, 0, 0, skips)
                    } else {
                        producer_snapshot(tick, skips, 0, 0)
                    };
                    advisory.observe(&snapshot, at(start, tick));
                    assert_eq!(
                        advisory.message().is_some(),
                        (70..111).contains(&tick),
                        "tick {tick}, late events {late_events}"
                    );
                }
                assert!(advisory.message().is_none());
            }
        }

        #[test]
        fn one_producer_hiccup_and_historical_starvation_stay_quiet() {
            let start = Instant::now();
            let mut advisory = AudioAdvisory::default();
            let mut skips = 100;
            for tick in 0..=100 {
                if tick <= 20 || tick == 29 || tick == 30 {
                    skips += 1;
                }
                advisory.observe(&producer_snapshot(tick, skips, 0, skips), at(start, tick));
                assert!(advisory.message().is_none(), "tick {tick}");
            }
        }

        #[test]
        fn refusals_warn_only_near_fresh_audio_starvation_and_clear_when_it_settles() {
            let start = Instant::now();
            for fresh_starvation in [false, true] {
                let mut advisory = AudioAdvisory::default();
                for tick in 0..=130 {
                    let gaps = 100 + u64::from(fresh_starvation && tick >= 21);
                    advisory.observe(&producer_snapshot(tick, gaps, tick, 0), at(start, tick));
                    assert_eq!(
                        advisory.message().is_some(),
                        fresh_starvation && (50..120).contains(&tick),
                        "tick {tick}, fresh starvation {fresh_starvation}"
                    );
                }
            }
        }

        #[test]
        fn reported_query_burst_warns_at_32_frames_then_clears_before_an_isolated_skip() {
            let start = Instant::now();
            let mut advisory = AudioAdvisory::default();
            let mut gaps = 0;
            let mut refusals = 0;
            // Replay the reported deadline/refusal timing at 100ms UI intervals.
            // The audio callbacks themselves never exceed their deadline here.
            for tick in 0..=330 {
                if [180, 183, 188, 197, 200, 207, 211, 270].contains(&tick) {
                    refusals += 1;
                }
                if [189, 270].contains(&tick) {
                    gaps += 1;
                }
                advisory.observe(&producer_snapshot(tick, gaps, refusals, 0), at(start, tick));
                assert_eq!(
                    advisory.message().is_some(),
                    (210..261).contains(&tick),
                    "tick {tick}: first burst should warn; later isolated skip should not"
                );
            }
        }

        #[test]
        fn loading_and_invalid_scores_do_not_extend_a_starvation_hiccup() {
            let start = Instant::now();
            let mut advisory = AudioAdvisory::default();
            for tick in 0..=100 {
                let mut snapshot = producer_snapshot(tick, u64::from(tick >= 21), tick, 0);
                let producer = &mut snapshot.pressure.as_mut().unwrap().producer;
                producer.loading_refusals = tick;
                producer.committed_refusals = tick;
                producer.rejected_scores = tick;
                advisory.observe(&snapshot, at(start, tick));
                assert!(advisory.message().is_none(), "tick {tick}");
            }
        }

        /// An unavailable producer read is a dropped sample and keeps the earlier
        /// baseline. A skip during the failed reads still counts when they return:
        /// here it is the third affected window of five. An empty read taken as a
        /// baseline loses that skip and leaves only two.
        #[test]
        fn a_skip_during_unavailable_producer_reads_counts_when_they_return() {
            let start = Instant::now();
            let mut advisory = AudioAdvisory::default();
            let mut gaps = 100;
            for tick in 0..=110 {
                if [25, 35, 55].contains(&tick) {
                    gaps += 1;
                }
                let mut snapshot = producer_snapshot(tick, gaps, 0, 0);
                if (30..40).contains(&tick) {
                    snapshot.pressure.as_mut().unwrap().producer = ProducerLoadSnapshot::default();
                }
                advisory.observe(&snapshot, at(start, tick));
                assert_eq!(
                    advisory.message().is_some(),
                    (60..105).contains(&tick),
                    "tick {tick}"
                );
            }
        }

        /// A producer counter that goes backwards belongs to a new meter. Its
        /// counts start a fresh baseline with its own warmup, so skips in the next
        /// two seconds are startup, not sustained trouble. Without the warmup the
        /// three skips below would fill three windows in a row.
        #[test]
        fn a_restarted_producer_counter_starts_a_fresh_warmup() {
            let start = Instant::now();
            let mut advisory = AudioAdvisory::default();
            for tick in 0..=130 {
                let gaps = match tick {
                    ..60 => 100,
                    60 => 1,
                    61..71 => 2,
                    71..81 => 3,
                    _ => 4,
                };
                advisory.observe(&producer_snapshot(tick, gaps, 0, 0), at(start, tick));
                assert!(advisory.message().is_none(), "tick {tick}");
            }
        }

        #[test]
        fn three_affected_windows_warn_then_five_quiet_seconds_clear() {
            let start = Instant::now();
            let (mut advisory, _) = struggling(start);
            for tick in 51..=100 {
                let changed = advisory.observe(&snapshot(tick, 550, 32), at(start, tick));
                assert_eq!(advisory.message().is_some(), tick < 100, "tick {tick}");
                assert_eq!(changed, tick == 100, "only the visible transition redraws");
            }
        }

        #[test]
        fn a_healthy_window_breaks_the_activation_streak() {
            let start = Instant::now();
            let mut advisory = AudioAdvisory::default();
            let mut misses = 500;
            for tick in 0..=80 {
                if (21..30).contains(&tick) || (40..60).contains(&tick) {
                    misses += 1;
                }
                advisory.observe(&snapshot(tick, misses, 32), at(start, tick));
                assert!(advisory.message().is_none(), "tick {tick}");
            }
        }

        #[test]
        fn duplicates_and_a_long_telemetry_gap_cannot_fill_windows() {
            let start = Instant::now();
            let mut advisory = AudioAdvisory::default();
            for tick in 0..=49 {
                advisory.observe(&snapshot(tick, 500 + tick, 32), at(start, tick));
            }
            let delayed = snapshot(80, 2_000, 32);
            advisory.observe(&delayed, at(start, 80));
            assert!(
                advisory.message().is_none(),
                "one late delivery is not sustained load"
            );
            for tick in 81..=150 {
                advisory.observe(&delayed, at(start, tick));
                assert!(advisory.message().is_none(), "duplicate at tick {tick}");
            }
            let (mut advisory, latest) = struggling(start);
            for tick in 51..=100 {
                advisory.observe(&latest, at(start, tick));
            }
            assert!(
                advisory.message().is_none(),
                "duplicates cannot keep a warning alive"
            );
        }

        #[test]
        fn empty_telemetry_is_not_recovery_but_a_new_stream_resets_immediately() {
            let start = Instant::now();
            let (mut advisory, mut latest) = struggling(start);
            latest.pressure.as_mut().unwrap().device.realtime_load =
                RealtimeLoadSnapshot::default();
            advisory.observe(&latest, at(start, 51));
            assert!(advisory.message().is_some());
            latest.pressure.as_mut().unwrap().device.stream_id += 1;
            advisory.observe(&latest, at(start, 52));
            assert!(advisory.message().is_none());
        }

        #[test]
        fn stream_rate_and_counter_changes_start_fresh() {
            let start = Instant::now();
            for change in 0..5 {
                let (mut advisory, mut latest) = struggling(start);
                if change == 4 {
                    // A recycle at a new latency keeps the stream id and, here, the
                    // counters: the newly granted buffer alone starts afresh.
                    latest.device = Some(device(64, Some(64)));
                } else {
                    let report = &mut latest.pressure.as_mut().unwrap().device;
                    match change {
                        0 => report.stream_id += 1,
                        1 => report.realtime_load.sample_rate_hz = 44_100,
                        2 => report.realtime_load.total_callbacks = 16,
                        3 => report.realtime_load.callbacks_over_100_percent = 0,
                        _ => unreachable!(),
                    }
                }
                assert!(advisory.observe(&latest, at(start, 51)));
                assert!(advisory.message().is_none(), "change {change}");
            }
        }

        /// PulseAudio asked for 32 frames delivers 32 to 64 a callback, and more
        /// when the client is late; the meter publishes whichever callback came
        /// sixteenth. The same stream under the same sustained misses must warn on
        /// the same schedule as a steady one, and keep warning while they last.
        #[test]
        fn varying_callback_sizes_on_one_small_stream_warn_and_stay() {
            let start = Instant::now();
            let mut advisory = AudioAdvisory::default();
            for tick in 0..=100 {
                let mut latest = snapshot(tick, 500 + tick, 32);
                let callback = match tick % 7 {
                    6 => 128,
                    odd if odd % 2 == 1 => 64,
                    _ => 32,
                };
                latest
                    .pressure
                    .as_mut()
                    .unwrap()
                    .device
                    .realtime_load
                    .last_callback_frames = callback;
                advisory.observe(&latest, at(start, tick));
                assert_eq!(
                    advisory.message().is_some(),
                    tick >= 50,
                    "tick {tick}, {callback}-frame callback"
                );
            }
        }

        #[test]
        fn normal_buffers_and_stopped_audio_never_keep_the_hint() {
            let start = Instant::now();
            for frames in [128, 256, 512] {
                let mut advisory = AudioAdvisory::default();
                for tick in 0..=100 {
                    advisory.observe(&snapshot(tick, 500 + tick, frames), at(start, tick));
                }
                assert!(
                    advisory.message().is_none(),
                    "the stream's buffer is {frames} frames"
                );
            }
            // What the host granted decides, not what was asked for. With nothing
            // reported, the request is all there is to go by.
            for (reported, warns) in [(Some(256), false), (None, true)] {
                let mut advisory = AudioAdvisory::default();
                for tick in 0..=50 {
                    let mut latest = snapshot(tick, 500 + tick, 32);
                    latest.device = Some(device(32, reported));
                    advisory.observe(&latest, at(start, tick));
                }
                assert_eq!(advisory.message().is_some(), warns, "reported {reported:?}");
            }
            for state in 0..4 {
                let (mut advisory, mut latest) = struggling(start);
                match state {
                    0 => latest.playing = false,
                    1 => latest.stopping = true,
                    2 => latest.device = Some(device(256, Some(256))),
                    3 => latest.device = Some(device(32, Some(256))),
                    _ => unreachable!(),
                }
                advisory.observe(&latest, at(start, 51));
                assert!(advisory.message().is_none(), "state {state}");
            }
        }
    }
}
