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
