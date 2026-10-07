//! Generation-aware event scheduler.
//!
//! Its scheduling contract covers:
//!
//! * **Schedule generations.** Every event carries the generation that produced
//!   it. Replacing the pattern cancels the old generation, and cancelled events
//!   are dropped at a defined boundary rather than mixed with fresh ones.
//! * **A prefill horizon.** Events are produced ahead of the consumer. The
//!   bound on how long evaluation may block querying is `H_remaining` - what is
//!   *still buffered when evaluation starts*, not the nominal horizon.
//! * **Unique onset ids.** Every onset is emitted exactly once, so a stale
//!   replay cannot duplicate a note.
//! * **Immediate Stop.** Stop travels on its own path and is bounded
//!   independently of the event queue, so it is never stuck behind evaluation.
//! * **Stale-event expiry.** Reuse of an old schedule has a maximum duration,
//!   after which the transport degrades audibly-but-safely rather than
//!   replaying consumed onsets.
//!
//! The clock is injected, so tests drive it deterministically. The offline
//! renderer uses the same virtual-clock path.

use rustel_core::{Hap, Pattern, QueryLimit, State, TimeSpan, Value};
use rustel_fraction::Fraction;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

/// Monotonically increasing id for a pattern generation. Bumped on every
/// evaluation; events from older generations are never delivered.
pub type Generation = u64;

/// A scheduled event. Plain data - no closures, no JS handles, nothing that
/// could pull the audio thread into a heap it must not touch.
#[derive(Clone, Debug, PartialEq)]
pub struct Event {
    /// Unique for the scheduler's lifetime. Duplicate delivery is detectable.
    pub onset_id: u64,
    pub generation: Generation,
    /// Cycle position of the onset.
    pub whole_begin: Fraction,
    /// Cycle position at which the source hap's whole ends.
    ///
    /// Keeping the exact rational endpoint lets a downstream renderer derive
    /// the note gate without guessing from the next onset. It remains plain
    /// Rust data and never crosses into the real-time callback.
    pub whole_end: Fraction,
    /// Effective source-hap duration, including `duration`/`clip`
    /// controls. This is intentionally computed while the scheduler still has
    /// the `Hap`; reconstructing it from `whole` silently loses `clip` (and its
    /// `legato` alias) before audio conversion.
    pub duration: Fraction,
    /// Wall-clock seconds at which it should sound.
    pub target_time: f64,
    pub value: Value,
    /// Direct gain/cutoff slider tokens; zero means unbound.
    pub live_controls: [u64; 2],
    pub ui_visuals: u64,
    /// `.log()`'s already-formatted text, printed when this onset fires.
    ///
    /// Carrying formatted text rather than the hap keeps pattern state out
    /// of the queue while letting the consumer log accepted onsets.
    pub log_line: Option<std::sync::Arc<str>>,
}

/// One accepted scheduler onset, reduced to the plain data a UI needs.
///
/// This is deliberately separate from [`Event`]. In particular it owns the
/// already-rendered value text rather than a [`Value`], so draining a trace
/// cannot retain callback sidecars or other runtime-owned value state. Source
/// locations use the same byte-offset pairs carried by [`Hap::context`].
#[derive(Clone, Debug, PartialEq)]
pub struct ScheduleTraceEvent {
    pub onset_id: u64,
    pub generation: Generation,
    pub whole_begin: Fraction,
    pub whole_end: Fraction,
    pub part_begin: Fraction,
    pub part_end: Fraction,
    /// Effective source-hap duration, including `duration`/`clip` controls.
    pub duration: Fraction,
    pub target_time: f64,
    /// Bounded human-readable value text. An unusually large display value is
    /// omitted at capture time rather than retained in every queued trace.
    pub value_show: Option<String>,
    /// Typed visual metadata copied out before the trace gives up its `Value`.
    /// Keeping these fields separate avoids asking UI clients to parse the
    /// intentionally human-oriented `value_show` representation.
    pub color: Option<String>,
    pub label: Option<String>,
    /// The score's `activeLabel`, shown while the event sounds.
    pub active_label: Option<String>,
    /// What `edoScale` wrote on the value: the tuning a pitch wheel draws
    /// the event against.
    pub scale: Option<TraceEdoScale>,
    /// The pitch the score wrote - `freq`, else `note` as a name or a MIDI
    /// number - so a pitched sample, whose voice carries no frequency, still
    /// reaches the UI with one. `None` for an unpitched sound (`s("bd")`,
    /// `.n(3)` without a note), which keeps a drum on its own lane. The
    /// producer replaces it with the voice's own frequency once the onset
    /// crosses the audio ring.
    pub frequency_hz: Option<f32>,
    /// Filled only after the producer successfully submits the corresponding
    /// audio event. The scheduler itself deliberately leaves this unset.
    pub gain: Option<f32>,
    pub ui_visuals: u64,
    pub context: Vec<(usize, usize)>,
}

/// Tuning metadata from `edoScale` for pitch visualizations: the number of
/// octave divisions, root frequency, scale degrees, and their labels.
#[derive(Clone, Debug, PartialEq)]
pub struct TraceEdoScale {
    pub edo: u16,
    pub root_hz: f32,
    /// Ascending, each below `edo`.
    pub degree_indexes: Vec<u16>,
    /// One per degree, in `degree_indexes` order; empty when unnamed.
    pub interval_labels: Vec<String>,
}

/// Most divisions of the octave a traced `edoScale` carries.
pub const MAX_TRACE_EDO_DIVISIONS: usize = 256;
/// Most bytes in one traced interval label.
pub const MAX_TRACE_INTERVAL_LABEL_BYTES: usize = 16;

/// Maximum bytes retained for any human-readable trace field.
pub const MAX_TRACE_TEXT_BYTES: usize = 256;

/// Maximum source ranges copied into one trace at scheduler acceptance.
pub const MAX_TRACE_CONTEXT_RANGES: usize = 64;

fn trace_visual_text(value: &Value, name: &str) -> Option<String> {
    let text = value.get(name)?.show();
    (text.len() <= MAX_TRACE_TEXT_BYTES).then_some(text)
}

/// Copy `edoScale` metadata into a bounded trace record. Invalid or missing
/// tuning fields omit the record; missing or oversized labels become empty.
fn trace_edo_scale(value: &Value) -> Option<TraceEdoScale> {
    let edo = match value.get("edo")? {
        Value::F64(edo)
            if edo.is_finite() && *edo >= 1.0 && *edo <= MAX_TRACE_EDO_DIVISIONS as f64 =>
        {
            edo.round() as u16
        }
        _ => return None,
    };
    let root_hz = match value.get("root")? {
        Value::F64(hertz) => *hertz,
        Value::Str(text) => text.trim().parse::<f64>().ok()?,
        _ => return None,
    };
    if !root_hz.is_finite() || root_hz <= 0.0 {
        return None;
    }
    let Value::List(indexes) = value.get("degreeIndexes")? else {
        return None;
    };
    if indexes.len() > MAX_TRACE_EDO_DIVISIONS {
        return None;
    }
    let mut degree_indexes = Vec::with_capacity(indexes.len());
    for index in indexes {
        let Value::F64(index) = index else {
            return None;
        };
        if !index.is_finite() || *index < 0.0 || *index >= f64::from(edo) {
            return None;
        }
        degree_indexes.push(index.round() as u16);
    }
    // `intLabels` is one-based: degree `i` is named at `i + 1`.
    // Missing entries leave the corresponding degree unnamed.
    let labels = match value.get("intLabels") {
        Some(Value::List(labels)) => labels.as_slice(),
        _ => &[],
    };
    let interval_labels = (0..degree_indexes.len())
        .map(|degree| match labels.get(degree + 1) {
            Some(Value::Str(label)) if label.len() <= MAX_TRACE_INTERVAL_LABEL_BYTES => {
                label.clone()
            }
            _ => String::new(),
        })
        .collect();
    Some(TraceEdoScale {
        edo,
        root_hz: root_hz as f32,
        degree_indexes,
        interval_labels,
    })
}

impl ScheduleTraceEvent {
    /// The trace of a hap. The scheduler captures one at queue acceptance,
    /// and a preview of a hap that is not yet queued uses the same form. The
    /// texts are bounded, the pitch is what the score wrote, and `gain` is
    /// left for the producer. `None` for a hap without a whole span.
    pub fn from_hap(
        hap: &Hap,
        generation: Generation,
        onset_id: u64,
        target_time: f64,
    ) -> Option<Self> {
        let whole = hap.whole?;
        let value_show = hap.value.show();
        Some(Self {
            onset_id,
            generation,
            whole_begin: whole.begin,
            whole_end: whole.end,
            part_begin: hap.part.begin,
            part_end: hap.part.end,
            duration: hap.duration(),
            target_time,
            value_show: (value_show.len() <= MAX_TRACE_TEXT_BYTES).then_some(value_show),
            color: trace_visual_text(&hap.value, "color"),
            label: trace_visual_text(&hap.value, "label"),
            active_label: trace_visual_text(&hap.value, "activeLabel"),
            scale: trace_edo_scale(&hap.value),
            frequency_hz: trace_pitch_hz(&hap.value),
            gain: None,
            ui_visuals: hap.ui_visuals_context(),
            context: hap
                .context
                .iter()
                .copied()
                .take(MAX_TRACE_CONTEXT_RANGES)
                .collect(),
        })
    }
}

fn trace_pitch_hz(value: &Value) -> Option<f32> {
    let finite = |hertz: f64| (hertz.is_finite() && hertz > 0.0).then_some(hertz as f32);
    if let Some(Value::F64(freq)) = value.get("freq")
        && let Some(hertz) = finite(*freq)
    {
        return Some(hertz);
    }
    let midi = match value.get("note")? {
        Value::F64(midi) => *midi,
        Value::Str(name) => rustel_core::util::note_to_midi(name, 3).ok()?,
        _ => return None,
    };
    finite(rustel_core::util::midi_to_freq(midi))
}

/// Why a tick produced no events.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TickStatus {
    Filled,
    /// No query was made. Normally the horizon is already covered; the same
    /// status also reports a tick that could query no representable span -
    /// no pattern installed, a caller cap below the one-millicycle span
    /// quantum, or a non-finite deficit - and a queue already at
    /// [`MAX_QUEUED_EVENTS`], which a drain has to make room in first.
    HorizonFull,
    Stopped,
    /// A query was REFUSED by a resource limit. See `Scheduler::refusal`.
    Refused,
    /// The queue hit [`MAX_QUEUED_EVENTS`] and the fill was cut short; the
    /// next fill resumes at the earliest onset this one left unqueued.
    ///
    /// Distinct from `Filled` because the caller has to be able to STOP rather
    /// than tick again into the same wall: a dense pattern refills instantly,
    /// so treating this as ordinary progress is an unbounded loop.
    QueueFull,
}

/// Work attributed to one explicitly profiled scheduler tick.
///
/// Ordinary/offline ticks do not collect this data. The live producer opts in
/// at its existing query boundary, keeping the instrumentation off the audio
/// callback and off unprofiled library queries.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SchedulerTickProfile {
    pub query_nanos: u64,
    pub callback_nanos: u64,
    pub callback_calls: u64,
    pub callback_kind_sample: Option<rustel_core::CallbackQueryKindCounts>,
    pub acceptance_nanos: u64,
    pub queried_haps: u64,
    pub accepted_events: u64,
    pub query_span_millicycles: u64,
    pub horizon_full: bool,
    pub stopped: bool,
    pub refused: bool,
    pub queue_full: bool,
}

/// The most events the scheduler will hold before refusing to queue more.
///
/// `tick` materialises every onset in the horizon span in one pass, so a single
/// sufficiently dense tick can allocate without any downstream cap seeing it -
/// a caller-side limit is checked only after this function has already built
/// the vector. The bound therefore has to live here, at the allocation site.
///
/// Two hundred thousand queued events is far beyond any horizon a scheduler
/// legitimately holds (the default horizon is a fraction of a second) while
/// staying small enough that hitting it costs megabytes rather than gigabytes.
pub const MAX_QUEUED_EVENTS: usize = 200_000;

/// The most accepted onsets retained for the optional UI trace.
///
/// Trace storage is intentionally much smaller than the audio event queue and
/// has its own overflow accounting. A UI that stops draining therefore cannot
/// make an otherwise healthy audio schedule grow with it.
pub const MAX_QUEUED_TRACE_EVENTS: usize = 4_096;

/// The most cycles a single `tick` will query ahead.
///
/// `tick` queries the pattern once and gets back a `Vec<Hap>` for the whole
/// span in one allocation. A cap applied while that vector drains into the
/// queue, or by the caller on the finished timeline, is checked after the
/// memory has been taken. Only a limit on the span limits the allocation.
///
/// A large `horizon` is legitimate configuration, so the span is clamped
/// rather than refused: the horizon fills over several ticks.
///
/// Cycles alone do not bound the vector. Its size is the cycle count times
/// the haps in each cycle, and a `stack` makes that width unbounded.
/// `setcps(100)` makes half a second fifty cycles, and four hundred stacked
/// layers of `bd*64` are then 1.6 million haps in one allocation. The hap
/// budget does not prevent it: the count is charged once the vector already
/// exists. Measured, that render peaked at 3.3 GB and was OOM-killed; two
/// thousand layers reached 12.8 GB.
///
/// Four cycles still cover any real horizon in a single tick: the default
/// horizon is a fraction of one cycle at ordinary tempos. The same render
/// then takes a fraction of the memory.
const MAX_TICK_SPAN_CYCLES: f64 = 4.0;

/// The smallest deficit worth a query, in seconds of music.
///
/// The cost of a query is not proportional to the span it covers: `fill` and
/// its relatives widen the window they read by a fixed amount however little
/// is asked for, so a thousandth of a cycle costs nearly what a whole one
/// does. Measured on a seven-lane score using `fill`, a one-sixteenth-cycle
/// query cost 0.56 ms against 0.68 ms for a full cycle. A producer that
/// queries on every pass of its loop, about 470 times a second, turns 10 ms
/// of query work per cycle into roughly 560 ms and starves the audio thread.
///
/// The floor is the step of the offline loop, so the live producer and the
/// offline render use the same query grid. That matters beyond the cost: a
/// hap with no `whole` takes its duration from the query window, so two
/// different grids give two different renders.
///
/// The floor never exceeds the horizon, so `tick`'s covered-horizon gate
/// (`horizon_remaining > horizon - floor`) always lets a drained horizon query.
fn refill_floor_secs(horizon: f64) -> f64 {
    (horizon * 0.25)
        .clamp(MIN_REFILL_FLOOR_SECS, 0.05)
        .min(horizon)
}

/// The smallest refill floor, in seconds, for a horizon at least this long; a
/// shorter horizon's floor is the horizon itself.
pub const MIN_REFILL_FLOOR_SECS: f64 = 0.001;

fn duration_nanos(duration: std::time::Duration) -> u64 {
    duration.as_nanos().min(u128::from(u64::MAX)) as u64
}

/// Injected clock, so tests are deterministic and the offline renderer reuses
/// the same code path as live playback.
pub trait Clock {
    fn now(&self) -> f64;
}

/// A clock advanced explicitly by the caller.
#[derive(Default)]
pub struct VirtualClock {
    t: std::cell::Cell<f64>,
}

impl VirtualClock {
    pub fn new(t: f64) -> Self {
        Self {
            t: std::cell::Cell::new(t),
        }
    }
    pub fn advance(&self, dt: f64) {
        self.t.set(self.t.get() + dt);
    }
    pub fn set(&self, t: f64) {
        self.t.set(t);
    }
}

impl Clock for VirtualClock {
    fn now(&self) -> f64 {
        self.t.get()
    }
}

/// The production wake path: the query thread parks on a condvar and is
/// signalled by the producer (in the real system, the audio callback at a
/// block boundary), rather than polling a timer.
///
/// Measure wake jitter through this type, not `thread::sleep`. Sleep
/// measures timer granularity, which is a different and usually smaller
/// quantity than the latency of a cross-thread signal under load.
#[derive(Default)]
pub struct Waker {
    m: std::sync::Mutex<bool>,
    cv: std::sync::Condvar,
}

impl Waker {
    pub fn signal(&self) {
        let mut g = self.m.lock().unwrap();
        *g = true;
        self.cv.notify_one();
    }

    /// Park until signalled or `timeout` elapses. Returns true if signalled.
    pub fn wait(&self, timeout: std::time::Duration) -> bool {
        let mut g = self.m.lock().unwrap();
        while !*g {
            let (ng, r) = self.cv.wait_timeout(g, timeout).unwrap();
            g = ng;
            if r.timed_out() && !*g {
                return false;
            }
        }
        *g = false;
        true
    }
}

/// Transport state, reachable without touching the event queue.
///
/// Stop must never queue behind evaluation, so it lives in an atomic the
/// consumer checks directly.
#[derive(Default)]
pub struct Transport {
    stopped: AtomicBool,
    generation: AtomicU64,
}

impl Transport {
    pub fn stop(&self) {
        self.stopped.store(true, Ordering::SeqCst);
    }
    pub fn start(&self) {
        self.stopped.store(false, Ordering::SeqCst);
    }
    /// The stop flag itself, for `rustel_core::with_cancellation`.
    ///
    /// A long query has to be able to observe a stop as it runs, and core
    /// cannot depend on this crate - so it takes a bare atomic.
    pub fn stopped_flag(&self) -> &AtomicBool {
        &self.stopped
    }

    pub fn is_stopped(&self) -> bool {
        self.stopped.load(Ordering::SeqCst)
    }
    pub fn generation(&self) -> Generation {
        self.generation.load(Ordering::SeqCst)
    }
    fn bump(&self) -> Generation {
        self.generation.fetch_add(1, Ordering::SeqCst) + 1
    }
}

pub struct Scheduler {
    pattern: Option<Pattern>,
    transport: Arc<Transport>,
    /// Seconds of schedule to keep buffered ahead of the clock.
    horizon: f64,
    /// Maximum haps one query may materialise before the scheduler refuses it.
    query_hap_budget: u64,
    cps: f64,
    /// Cycle position already queried up to.
    queried_to: Fraction,
    /// Explicit transport starts exclude earlier cycles, even if a control
    /// re-query occurs during preroll. Unset for an unrestricted timeline.
    query_start: Option<Fraction>,
    /// Wall-clock time corresponding to `queried_to`.
    anchor_time: f64,
    anchor_cycle: Fraction,
    queue: Vec<Event>,
    /// The message from the most recent query that threw, if any.
    ///
    /// A thrown query makes its window silent. Keep the error so callers can
    /// distinguish that failure from a pattern that intentionally plays nothing.
    thrown: Option<String>,
    /// The message from the most recent query whose per-hap callback failure
    /// was CONTAINED, if any.
    ///
    /// A throwing `filterValues`/`filterHaps` predicate keeps its haps playing
    /// (fail open) instead of emptying the arc; this is how the failure still
    /// reaches the caller once, rather than per tick.
    callback_failure: Option<String>,
    /// The message from the most recent query in which a stack contained a
    /// child's throw, if any. The siblings kept playing, but a lane threw.
    contained_throw: Option<String>,
    /// Why the last tick was refused, if it was.
    ///
    /// Keep the typed limit so callers can classify it without parsing text.
    refused: Option<QueryLimit>,
    next_onset_id: u64,
    /// Onsets already emitted, so a re-query can never duplicate one. Each
    /// has the time, in seconds, its copy on the device is aimed at. A copy
    /// keeps that time when a later generation changes the mapping.
    emitted: std::collections::HashMap<EmittedKey, f64>,
    /// The entries of `emitted` that the takeover of the current generation
    /// drops from the device. The producer publishes a generation after its
    /// first tick; see [`Self::settle_takeover`]. Until then the device
    /// still holds these copies, so the next takeover takes them back.
    superseded: Vec<DeviceCopy>,
    /// An outside clock moved the phase, so a cycle of `emitted` no longer
    /// names the same onset. The next generation clears it. A bend, which
    /// keeps the cycle at its instant, does not set it.
    retimed: bool,
    /// The outgoing copies the device still plays inside a continued
    /// replacement's takeover window, by begin. A copy stands in for a hap
    /// the edit changed or moved there, so that onset does not sound twice;
    /// see [`Self::replace_pattern_continued`]. A requery from an edge holds
    /// the copies the device keeps at or after its cursor in the same way.
    /// A continued replacement also holds a copy that has not played and
    /// has its mark before the cursor; see [`HeldOnset::behind`].
    overlap_held: std::collections::HashMap<Fraction, Vec<HeldOnset>>,
    /// Where that window ends. From there on the device has retired the
    /// outgoing copies and the replacement plays alone. A copy that an
    /// earlier tempo aimed before the takeover can lie past the end. It
    /// stands in for a hap on its own begin, or for a moved hap of the
    /// window with the same value.
    overlap_until: Fraction,
    /// Reusable ordinal table for otherwise-identical haps in one query pass.
    ///
    /// Entries are cleared after every pass; only bounded bucket storage is
    /// retained, so ordinary fills avoid rebuilding the hash table without an
    /// exceptional dense score permanently setting the scheduler footprint.
    pass_ordinals: std::collections::HashMap<EmittedKey, u32>,
    /// How long a stale schedule may be reused before the transport degrades.
    stale_limit: f64,
    /// When the schedule was last known to be fresh: a tick that queried, a
    /// tick that found the horizon already covered, or a cursor/generation
    /// reset. Only the stale check in [`Self::drain_through`] reads it.
    last_fill_time: f64,
    /// Optional observation channel for editor highlighting and visualizers.
    /// The disabled hot path is a single false branch after queue acceptance.
    trace_enabled: bool,
    trace_events: Vec<ScheduleTraceEvent>,
    trace_events_dropped: u64,
    trace_events_dropped_total: u64,
}

/// What makes one emitted onset distinct from another.
///
/// Duration is part of the identity: coincident haps with different end times
/// are distinct voices. Positions remain exact rationals.
#[derive(Clone, PartialEq, Eq, Hash)]
struct EmittedKey {
    generation: Generation,
    begin: Fraction,
    end: Fraction,
    /// Retain the complete rendering so a hash collision cannot drop a voice.
    /// `Arc` keeps the per-fill and persistent keys from copying the string.
    value: Arc<str>,
    /// Which occurrence of an otherwise identical hap this is, counted within
    /// one fill pass. Zero for the common case.
    occurrence: u32,
}

/// An emitted onset and the time, in seconds, its copy on the device is
/// aimed at.
type DeviceCopy = (EmittedKey, f64);

/// One onset the outgoing generation left on the device inside a continued
/// replacement's takeover window: an [`EmittedKey`] without the generation
/// and the begin. The begin is the key of the table that holds it.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord)]
struct HeldOnset {
    value: Arc<str>,
    end: Fraction,
    occurrence: u32,
    /// The begin is before the cursor of the takeover, and the copy has not
    /// played: it stood in for a hap moved earlier, or an earlier tempo
    /// aimed it. No query gets to its begin.
    behind: bool,
}

/// How alike two rendered values are: the words they share. An object
/// renders as one `name:value` word per control, so this counts the controls
/// two haps agree on.
fn shared_words(held: &str, incoming: &str) -> usize {
    incoming
        .split(' ')
        .filter(|word| held.split(' ').any(|other| other == *word))
        .count()
}

/// How far apart two cycle positions are.
fn apart(a: Fraction, b: Fraction) -> Fraction {
    if a < b { b.sub(a) } else { a.sub(b) }
}

/// The most (outgoing copy, incoming hap) pairs of one value that are
/// compared by distance. A takeover window holds a few. Past this limit the
/// copies and the haps pair in begin order, which costs only a sort. The
/// changed haps of one begin have the same limit when each gets a copy.
const MOVED_PAIRS_LIMIT: usize = 4_096;

/// The most seconds between the time of a copy and the time of a hap of the
/// same value for the two to be one onset. Each generation rounds its own
/// mapping, so the two times of one onset are seldom equal. Two voices of
/// one sound this near are one doubled voice to the ear.
const SAME_INSTANT_SECS: f64 = 1e-3;

/// The largest phase move, in cycles, that [`Scheduler::retime`] reads as
/// rounding. A clock bend passes the cycle at its instant back unchanged.
const RETIME_BEND_CYCLES: f64 = 1e-9;

/// Prune once the set is larger than a live set could plausibly need, keeping
/// only recent history.
const EMITTED_PRUNE_THRESHOLD: usize = 4_096;

/// Largest per-pass ordinal table whose buckets remain cached between fills.
/// Larger tables are valid, but their exceptional allocation is released as
/// soon as the pass finishes.
const PASS_ORDINAL_RETAIN_LIMIT: usize = 16_384;

/// Cycles of emitted history kept behind the horizon's own lead. The playhead
/// trails the query frontier by up to `horizon * cps` cycles, and
/// [`Scheduler::replace_pattern_continued`] re-queries from there, so its
/// overlap marks survive a prune.
const EMITTED_RETAIN_CYCLES: i128 = 8;

impl Scheduler {
    pub fn new(transport: Arc<Transport>, cps: f64, horizon: f64) -> Self {
        Self {
            pattern: None,
            transport,
            horizon,
            query_hap_budget: rustel_core::DEFAULT_HAP_BUDGET,
            cps,
            queried_to: Fraction::ZERO,
            query_start: None,
            anchor_time: 0.0,
            anchor_cycle: Fraction::ZERO,
            queue: Vec::new(),
            refused: None,
            thrown: None,
            callback_failure: None,
            contained_throw: None,
            next_onset_id: 0,
            emitted: std::collections::HashMap::new(),
            superseded: Vec::new(),
            retimed: false,
            overlap_held: std::collections::HashMap::new(),
            overlap_until: Fraction::ZERO,
            pass_ordinals: std::collections::HashMap::new(),
            stale_limit: 1.0,
            last_fill_time: 0.0,
            trace_enabled: false,
            trace_events: Vec::new(),
            trace_events_dropped: 0,
            trace_events_dropped_total: 0,
        }
    }

    /// Maximum haps accepted from one pattern query before scheduling begins.
    pub fn query_hap_budget(&self) -> u64 {
        self.query_hap_budget
    }

    /// Set the query cap used by every scheduler fill.
    pub fn set_query_hap_budget(&mut self, budget: u64) {
        self.query_hap_budget = budget;
    }

    /// Enable or disable the bounded scheduler trace.
    ///
    /// Disabling also discards unread trace data. Re-enabling starts a fresh
    /// observation window; it never exposes events accepted while tracing was
    /// off.
    pub fn set_trace_enabled(&mut self, enabled: bool) {
        if self.trace_enabled == enabled {
            return;
        }
        self.trace_enabled = enabled;
        self.clear_trace();
    }

    pub fn trace_enabled(&self) -> bool {
        self.trace_enabled
    }

    /// Take all unread trace events in scheduler acceptance order.
    ///
    /// A stopped transport has no live schedule, so stale observations are
    /// discarded even when Stop was signalled between scheduler calls.
    pub fn take_trace_events(&mut self) -> Vec<ScheduleTraceEvent> {
        let mut events = Vec::new();
        self.drain_trace_events_into(&mut events);
        events
    }

    /// Move unread trace events into a caller-owned reusable buffer.
    ///
    /// Unlike replacing the trace vector, `append` leaves its allocation in
    /// the scheduler for the next horizon fill.
    pub fn drain_trace_events_into(&mut self, events: &mut Vec<ScheduleTraceEvent>) {
        if !self.trace_enabled || self.transport.is_stopped() {
            self.clear_trace();
            return;
        }
        events.append(&mut self.trace_events);
    }

    /// Number of trace events dropped since the current observation window
    /// began. This counter is independent of the audio event queue.
    pub fn trace_events_dropped(&self) -> u64 {
        self.trace_events_dropped
    }

    pub fn trace_events_dropped_total(&self) -> u64 {
        self.trace_events_dropped_total
    }

    pub fn trace_queued(&self) -> usize {
        self.trace_events.len()
    }

    /// Take and reset the trace overflow count.
    pub fn take_trace_events_dropped(&mut self) -> u64 {
        std::mem::take(&mut self.trace_events_dropped)
    }

    fn clear_trace(&mut self) {
        self.trace_events.clear();
        self.trace_events_dropped = 0;
    }

    /// Drop emitted history that no future query can reach.
    fn prune_emitted(&mut self) {
        if self.emitted.len() <= EMITTED_PRUNE_THRESHOLD {
            return;
        }
        // A fill ends at most one horizon past its clock, so the playhead sits
        // within `lead` cycles of this frontier. The clamp keeps a huge
        // horizon inside the cycle arithmetic.
        let lead = if self.cps.is_finite() && self.cps > 0.0 {
            (self.horizon * self.cps).ceil().clamp(0.0, 1.0e9) as i128
        } else {
            0
        };
        let cutoff = self
            .queried_to
            .sub(Fraction::int(lead + EMITTED_RETAIN_CYCLES));
        self.emitted.retain(|key, _| !key.begin.lt(&cutoff));
    }

    /// Widen or shrink how far `tick` queries ahead of `now`. Used by live
    /// playback when the device's consumption frontier exceeds the default
    /// horizon, so a reload takeover still sits inside scheduled cover.
    pub fn set_horizon(&mut self, horizon: f64) {
        if horizon.is_finite() && horizon > 0.0 {
            self.horizon = horizon;
        }
    }

    pub fn set_stale_limit(&mut self, seconds: f64) {
        self.stale_limit = seconds;
    }

    /// Safety factor applied to the measured budget before it is compared with
    /// the remaining horizon.
    ///
    /// The measured terms are a *floor*, not a worst case: they come from a
    /// finite sample on an idle machine, and the real system will see GC,
    /// page faults, scheduler preemption and a busier heap. Setting `D` to the
    /// calculated ceiling means the first sample beyond the measured tail
    /// overruns the horizon and drops audio. The margin buys that headroom
    /// explicitly rather than pretending the measurement was exhaustive.
    pub const BUDGET_SAFETY_FACTOR: f64 = 3.0;

    /// The evaluation deadline this scheduler can currently afford, given how
    /// much horizon is actually left.
    ///
    /// Returns `None` when the remaining horizon cannot cover the full budget,
    /// in which case the caller must **defer evaluation** rather than start
    /// one it cannot finish safely. Refusing is the correct behaviour: a
    /// deferred evaluation is a slightly late edit, an overrun is a dropped
    /// note.
    pub fn affordable_deadline(
        &self,
        now: f64,
        overhead: std::time::Duration,
    ) -> Option<std::time::Duration> {
        let h = self.horizon_remaining(now);
        let needed = overhead.as_secs_f64() * Self::BUDGET_SAFETY_FACTOR;
        if h <= needed {
            return None;
        }
        Some(std::time::Duration::from_secs_f64(h - needed))
    }

    /// Install a new pattern. **Cancels the previous generation**: everything
    /// already queued from it is dropped, so old and new can never interleave.
    pub fn set_pattern(&mut self, pattern: Pattern, now: f64) -> Generation {
        self.replace_pattern(pattern, now, None)
    }

    /// Atomically replace the pattern and, when supplied, its cycles-per-second
    /// clock rate.
    ///
    /// Score construction validates tempo before it reaches this boundary. A
    /// defensive guard still keeps an invalid internal value from poisoning
    /// scheduler divisions in release builds; such a value leaves the current
    /// rate unchanged. Pattern, tempo, cursor anchor, and generation-local
    /// queue state are then committed together with exactly one generation
    /// bump. `None` deliberately retains the current rate, so a later score
    /// without an in-file tempo directive does not snap back to the process
    /// default.
    pub fn replace_pattern(&mut self, pattern: Pattern, now: f64, cps: Option<f64>) -> Generation {
        if let Some(cps) = cps {
            debug_assert!(
                cps.is_finite() && cps > 0.0,
                "replacement cps must be finite and greater than zero"
            );
            if cps.is_finite() && cps > 0.0 {
                self.cps = cps;
            }
        }
        let generation = self.transport.bump();
        self.pattern = Some(pattern);
        // Re-anchor so the new generation continues from the current time
        // rather than replaying the old one's cycle positions.
        self.anchor_time = now;
        self.anchor_cycle = self.queried_to;
        self.queue.clear();
        self.forget_device_copies();
        self.clear_trace();
        self.refused = None;
        self.last_fill_time = now;
        generation
    }

    /// Forget each copy the device holds. The caller starts a generation
    /// that keeps none of them, or marks again the ones it keeps.
    fn forget_device_copies(&mut self) {
        self.emitted.clear();
        self.superseded.clear();
        self.overlap_held.clear();
        self.retimed = false;
    }

    /// Re-query the active graph under a fresh generation without replacing
    /// the graph itself.
    ///
    /// Query-time controls such as sliders mutate cells read by the existing
    /// pattern. Events already transferred to a device still contain the old
    /// value, so merely changing the cell leaves the full prefetched horizon
    /// stale. A fresh generation lets the device keep old events before
    /// `cursor_time`, reject them after that takeover, and accept a newly
    /// queried window from the same graph.
    pub fn requery_active(&mut self, now: f64, cursor_time: f64) -> Option<Generation> {
        if self.pattern.is_none()
            || !now.is_finite()
            || !cursor_time.is_finite()
            || cursor_time < now
        {
            return None;
        }
        let anchor_cycle = Fraction::from_f64(self.cycle_at_time(now))?;
        let cursor_cycle = Fraction::from_f64(self.cycle_at_time(cursor_time))?;
        let generation = self.transport.bump();
        self.anchor_time = now;
        self.anchor_cycle = anchor_cycle;
        self.queried_to = self.at_or_after_start(cursor_cycle.max(anchor_cycle));
        self.queue.clear();
        self.forget_device_copies();
        self.clear_trace();
        self.refused = None;
        self.last_fill_time = now;
        Some(generation)
    }

    /// [`Self::requery_active`] for a device that drops the outgoing
    /// generation from a takeover frame.
    ///
    /// `takeover_edge` is the first instant whose onset the device puts on
    /// the takeover frame. The device keeps the outgoing onsets before the
    /// edge and drops the others, so the query starts at the edge. The
    /// takeover time can be more than a frame later: a cursor there skips
    /// the onsets between the two, and no generation plays them.
    ///
    /// The mapping does not move. [`Self::requery_active`] re-anchors at
    /// `now` through [`Fraction::from_f64`], which shifts each later onset
    /// by a fraction of a frame. The device can still hold an earlier
    /// generation when the next takeover is decided. An edge read on a
    /// shifted mapping can put an onset in the frame before the takeover
    /// frame on the wrong side, and that onset sounds twice.
    ///
    /// An edge that is not after `now` leaves no outgoing onset to keep.
    /// The requery is then the plain one: cursor and anchor on the cycle
    /// at `now`.
    ///
    /// The copies the device keeps stay marked as emitted, each at its own
    /// time, so a continued replacement that comes before the cursor
    /// pre-marks them again.
    ///
    /// After a replacement at another tempo the device holds copies aimed
    /// with the old tempo, and for them the edge is on another cycle. On a
    /// slower tempo the device keeps copies at or after the cursor: each
    /// suppresses its hap, or stands in for it when the value changed. On a
    /// faster tempo the device drops copies before the cursor: the cursor
    /// goes back to the first of them, but not before the cycle at `now`.
    ///
    /// After a [`Self::retime`] that moved the phase, a cycle names another
    /// onset. Then only the marks before the cursor stay, and the cursor is
    /// on the edge.
    pub fn requery_active_from(&mut self, now: f64, takeover_edge: f64) -> Option<Generation> {
        // Read before `requery_active` moves the anchor.
        let mut cursor = self.cycle_at_or_after_time(takeover_edge)?;
        let (anchor_time, anchor_cycle) = (self.anchor_time, self.anchor_cycle);
        let retimed = self.retimed;
        let emitted = std::mem::take(&mut self.emitted);
        let superseded = std::mem::take(&mut self.superseded);
        let Some(generation) = self.requery_active(now, now) else {
            self.emitted = emitted;
            self.superseded = superseded;
            return None;
        };
        // The cursor is now on the cycle at `now`. No later query starts
        // before it.
        let from = self.queried_to;
        let (mut kept, mut dropped) = Self::split_device_copies(
            emitted.into_iter().chain(superseded),
            from,
            self.unplayed_from(from, retimed),
            |_, time| time < takeover_edge,
        );
        if retimed {
            kept.retain(|(key, _)| key.begin.lt(&cursor));
            dropped.clear();
        } else if let Some(first) = dropped
            .iter()
            .map(|(key, _)| key.begin)
            // A dropped copy with a mark before `from` has no hap to query.
            .filter(|begin| !begin.lt(&from))
            .min()
        {
            cursor = cursor.min(first);
        }
        if cursor > self.queried_to {
            self.anchor_time = anchor_time;
            self.anchor_cycle = anchor_cycle;
            self.queried_to = cursor;
        }
        self.carry_device_copies(generation, kept, self.queried_to, false);
        self.superseded = dropped;
        // No hap moves in a requery: a copy stands in on its own begin.
        self.overlap_until = self.queried_to;
        Some(generation)
    }

    /// Split the device's copies for a takeover. The first list has the
    /// copies that `keeps` selects: the device keeps them. The second list
    /// has the other copies. A copy is left out when its mark is before
    /// `from`, behind the query cursor, and its time is before
    /// `unplayed_from`: it has played, and no query gets to its begin.
    ///
    /// The mark and the time of a copy can disagree. A copy that stood in for
    /// a hap moved earlier has the mark of that hap and its own, later time.
    /// The device still holds it after the cursor passes the mark.
    fn split_device_copies(
        copies: impl Iterator<Item = DeviceCopy>,
        from: Fraction,
        unplayed_from: f64,
        keeps: impl Fn(&EmittedKey, f64) -> bool,
    ) -> (Vec<DeviceCopy>, Vec<DeviceCopy>) {
        let (mut kept, mut dropped) = (Vec::new(), Vec::new());
        for (key, time) in copies {
            let unplayed = time >= unplayed_from;
            if key.begin.lt(&from) && !unplayed {
                continue;
            }
            if keeps(&key, time) {
                kept.push((key, time));
            } else {
                dropped.push((key, time));
            }
        }
        (kept, dropped)
    }

    /// The time of the cursor `from`, in seconds: a copy aimed there or
    /// later has not played. After a retime that moved the phase a copy
    /// counts by its cycle alone, as a guess, so no time is late enough.
    fn unplayed_from(&self, from: Fraction, retimed: bool) -> f64 {
        if retimed {
            f64::INFINITY
        } else {
            self.time_at_cycle(from)
        }
    }

    /// Mark `kept`, the outgoing onsets the device keeps, as emitted under
    /// `generation`, each at the time of its copy. Tick acceptance skips a
    /// hap whose key `emitted` holds, so the onset keeps its old voice and
    /// is not doubled. A copy from `held_from` on can also stand in for a
    /// changed or moved hap. With `hold_behind`, a copy before `held_from`
    /// can stand in for a hap aimed at its own time: the split kept it, so
    /// it has not played.
    fn carry_device_copies(
        &mut self,
        generation: Generation,
        kept: Vec<DeviceCopy>,
        held_from: Fraction,
        hold_behind: bool,
    ) {
        for (mut key, time) in kept {
            let behind = key.begin.lt(&held_from);
            let held = (!behind || hold_behind).then(|| HeldOnset {
                value: Arc::clone(&key.value),
                end: key.end,
                occurrence: key.occurrence,
                behind,
            });
            let begin = key.begin;
            key.generation = generation;
            // One held copy for one mark.
            if self.emitted.insert(key, time).is_none()
                && let Some(held) = held
            {
                self.overlap_held.entry(begin).or_default().push(held);
            }
        }
        // The set the keys came from has no order; the matching must.
        for held in self.overlap_held.values_mut() {
            held.sort();
        }
    }

    /// Take each copy the device holds: the emitted onsets, and the ones
    /// set aside for a generation that has not had its first tick.
    fn take_device_copies(&mut self) -> impl Iterator<Item = DeviceCopy> + use<> {
        std::mem::take(&mut self.emitted)
            .into_iter()
            .chain(std::mem::take(&mut self.superseded))
    }

    /// The rate used for query controls and cycle-to-wall-clock conversion.
    pub fn cps(&self) -> f64 {
        self.cps
    }

    /// Wall-clock time of `cycle` under the current anchor.
    pub fn time_at_cycle(&self, cycle: Fraction) -> f64 {
        self.anchor_time + cycle.sub(self.anchor_cycle).to_f64() / self.cps
    }

    /// Inverse of [`Self::time_at_cycle`], as an f64 cycle position.
    pub fn cycle_at_time(&self, time: f64) -> f64 {
        self.anchor_cycle.to_f64() + (time - self.anchor_time) * self.cps
    }

    /// The query cursor for `time`: the nearest cycle position
    /// [`Fraction::from_f64`] gives whose own time is `time` or later.
    ///
    /// `from_f64` approximates: it can give a simple fraction a little
    /// before the cycle it gets. An onset on that fraction is before
    /// `time`, and a cursor there would query it again. So the candidate
    /// moves forward until its time is not before `time`. The step starts
    /// at one unit in the last place and doubles each turn.
    fn cycle_at_or_after_time(&self, time: f64) -> Option<Fraction> {
        if !time.is_finite() {
            return None;
        }
        let cycle = self.cycle_at_time(time);
        let mut step = 0.0;
        loop {
            let candidate = Fraction::from_f64(cycle + step)?;
            if self.time_at_cycle(candidate) >= time {
                return Some(candidate);
            }
            step = if step == 0.0 {
                cycle.abs().max(1.0) * f64::EPSILON
            } else {
                step * 2.0
            };
        }
    }

    /// Follow an outside clock: from `now` the mapping runs at `cps` with
    /// `cycle` at `now`. The query cursor is left where it is - the caller
    /// re-queries, which is what makes the change land on the device.
    pub fn retime(&mut self, now: f64, cps: f64, cycle: f64) {
        if !(now.is_finite() && cps.is_finite() && cps > 0.0 && cycle.is_finite()) {
            return;
        }
        // A bend keeps the cycle at `now`: each cycle still names the same
        // onset, and each copy on the device keeps its time. A jump does not.
        let jump = (cycle - self.cycle_at_time(now)).abs() > RETIME_BEND_CYCLES;
        let Some(cycle) = Fraction::from_f64(cycle) else {
            return;
        };
        self.cps = cps;
        self.anchor_time = now;
        self.anchor_cycle = cycle;
        self.retimed |= jump;
        // An outside clock owns its cycle domain, including negative cycles.
        self.query_start = None;
    }

    /// Begin an explicit transport lifetime at cycle zero. A later re-query
    /// can replace its future without querying music before this beginning.
    /// Generic rebases remain unrestricted unless a start set this boundary;
    /// an explicit outside-clock [`Self::retime`] releases it.
    pub fn rebase_start_anchor(&mut self, now: f64) {
        self.query_start = Some(Fraction::ZERO);
        // No device holds an outgoing copy for a new lifetime. The marks and
        // held copies of the lifetime before it would drop its first onsets.
        self.queue.clear();
        self.forget_device_copies();
        self.rebase_anchor(now, 0.0);
    }

    fn at_or_after_start(&self, cycle: Fraction) -> Fraction {
        self.query_start.map_or(cycle, |start| cycle.max(start))
    }

    /// Re-anchor to a caller-computed `(now, cycle)` point, moving the query
    /// cursor there too. Used after a live replacement to keep the
    /// cycle-to-time mapping continuous across the edit: the caller
    /// snapshots `cycle_at_time(now)` under the old mapping, reloads, then
    /// rebases here - the new generation re-queries from the cycle at `now`,
    /// refilling the horizon its stale-filtered predecessor left.
    pub fn rebase_anchor(&mut self, now: f64, cycle: f64) {
        self.rebase_anchor_cursor(now, cycle, cycle);
    }

    /// [`Self::rebase_anchor`] with an independent query cursor: the mapping
    /// anchors at `(now, cycle)` while querying resumes at `cursor_cycle`.
    /// A live reload uses a cursor one schedule-lead ahead - frames closer
    /// than the playback latency are already rendered, so events aimed there
    /// arrive late by construction.
    ///
    /// That far cursor also drops a replacement's first onset. No generation
    /// queries a hap that lands between the edit and the takeover: the
    /// outgoing generation's version of it (if any) rings until the takeover
    /// and is retired at or after it, while the incoming one starts past it.
    /// [`Self::replace_pattern_continued`] covers that gap.
    pub fn rebase_anchor_cursor(&mut self, now: f64, cycle: f64, cursor_cycle: f64) {
        let (Some(cycle), Some(cursor)) =
            (Fraction::from_f64(cycle), Fraction::from_f64(cursor_cycle))
        else {
            return;
        };
        self.anchor_time = now;
        self.anchor_cycle = cycle;
        self.queried_to = self.at_or_after_start(cursor.max(cycle));
        self.last_fill_time = now;
    }

    /// [`Self::rebase_anchor_cursor`] for a device that drops the outgoing
    /// generation from a takeover frame.
    ///
    /// The query resumes at `takeover_edge`, read on the mapping anchored
    /// here. [`Self::requery_active_from`] describes the edge and why the
    /// cursor is not at the takeover time.
    pub fn rebase_anchor_from(&mut self, now: f64, cycle: f64, takeover_edge: f64) {
        let Some(cycle) = Fraction::from_f64(cycle) else {
            return;
        };
        let anchor = (self.anchor_time, self.anchor_cycle);
        self.anchor_time = now;
        self.anchor_cycle = cycle;
        let Some(cursor) = self.cycle_at_or_after_time(takeover_edge) else {
            (self.anchor_time, self.anchor_cycle) = anchor;
            return;
        };
        self.queried_to = self.at_or_after_start(cursor.max(cycle));
        self.last_fill_time = now;
    }

    /// Replace the pattern and continue through the takeover gap.
    ///
    /// With the plain [`Self::replace_pattern`] and a far cursor, the
    /// replacement's onsets between the edit instant and the takeover are
    /// silent: the outgoing generation is retired at the takeover frame, and
    /// the incoming one is queried from a cursor already past them. When the
    /// edit lands just after a cycle boundary, which is common for a
    /// hand-timed save, that silence is the replacement's first hap.
    ///
    /// This method moves the query cursor back to `from_cycle` (the edit
    /// instant) so the new generation renders that gap itself, and pre-marks
    /// the outgoing generation's onsets over `[from_cycle, takeover_cycle)`
    /// as already emitted:
    ///
    /// ```text
    ///              from_cycle (edit)          takeover_cycle
    /// old gen  --------+--- plays on the device ---+ retired
    /// new gen          +--- old key: skipped ------+--- all emitted --->
    ///                  |    changed or moved:      |
    ///                  |      left to the old copy |
    ///                  |    added: emitted late    |
    /// ```
    ///
    /// The existing dedup suppresses every onset the device still carries
    /// under the old generation, which it keeps playing until the takeover
    /// frame. A hap the edit added has no old key, is emitted normally, and
    /// reaches the device late (which admits it). At the takeover the device
    /// swaps to the new generation exactly as before, so the takeover
    /// contract is untouched.
    ///
    /// A hap the edit changed or moved in time has no old key either, but
    /// it is not an added hap: the device still plays the outgoing copy of
    /// that onset until the takeover. Emitted, a changed hap sounds together
    /// with its copy, and a moved hap a moment before or after it. So
    /// `overlap_held` keeps the outgoing onsets by begin. A copy that no
    /// incoming hap matches exactly can stand in for one incoming hap with
    /// no old key until the takeover; see [`Self::overlap_stand_ins`]. The
    /// edit sounds from the takeover on. The haps that get no copy are the
    /// added ones.
    ///
    /// The pre-marked window must end at the takeover: past that frame the
    /// old generation's pending events are retired, so their replacements are
    /// the only copy and must be emitted. `takeover_cycle` is at most the
    /// cycle of the takeover on the outgoing mapping, at any new rate: the
    /// device holds the outgoing copies at the times that mapping gave
    /// them. A caller can end the window sooner. The replacement then plays
    /// the outgoing copies past the end of the window again.
    ///
    /// A degenerate window (a takeover not after the edit instant, a zero
    /// continuity margin) still continues: there is no gap for the
    /// replacement to render and nothing to suppress, but the mapping must
    /// not move any more than it does with a wide window. Only an
    /// unrepresentable edit cycle falls back to the plain far-cursor
    /// contract.
    pub fn replace_pattern_continued(
        &mut self,
        pattern: Pattern,
        now: f64,
        cps: Option<f64>,
        from_cycle: f64,
        takeover_cycle: f64,
    ) -> Generation {
        // The overlap is exactly the span whose old copies the device still
        // holds. An inverted window simply has no suppression to do.
        let overlap = match (
            Fraction::from_f64(from_cycle),
            Fraction::from_f64(takeover_cycle),
        ) {
            (Some(from), Some(takeover)) if from < takeover => Some((from, takeover)),
            _ => None,
        };
        // Taken BEFORE the replace: the keys are the outgoing generation's
        // emission record, and `replace_pattern` clears that set outright.
        let copies = match overlap {
            Some((from, takeover)) => {
                let unplayed_from = self.unplayed_from(from, self.retimed);
                Self::split_device_copies(
                    self.take_device_copies(),
                    from,
                    unplayed_from,
                    |key, _| key.begin.lt(&takeover),
                )
            }
            None => (Vec::new(), Vec::new()),
        };
        let until = overlap.map(|(_, takeover)| takeover);
        self.replace_pattern_carrying(pattern, now, cps, from_cycle, copies, until)
    }

    /// [`Self::replace_pattern_continued`] for a device that drops the
    /// outgoing generation from a takeover frame.
    ///
    /// `takeover_edge`, in seconds, is the first instant whose onset the
    /// device puts on the takeover frame. The pre-marked window ends there:
    /// an outgoing onset is pre-marked when the time its copy is aimed at
    /// is before the edge, which is when the device keeps its copy. The
    /// takeover time can be more than a frame later. A window that ends
    /// there pre-marks an onset between the two, and the onset never
    /// sounds: the device drops its outgoing copy.
    ///
    /// The time of a copy is the one its own generation gave it. The
    /// replace re-anchors the mapping through [`Fraction::from_f64`], which
    /// shifts each onset by a fraction of a frame. An onset in the frame
    /// before the takeover frame could then cross the edge and sound twice.
    /// A copy that an earlier tempo aimed is not on the outgoing mapping
    /// at all.
    ///
    /// `end_cycle` ends the window sooner, as a shorter `takeover_cycle`
    /// does for [`Self::replace_pattern_continued`].
    pub fn replace_pattern_continued_until(
        &mut self,
        pattern: Pattern,
        now: f64,
        cps: Option<f64>,
        from_cycle: f64,
        takeover_edge: f64,
        end_cycle: Option<f64>,
    ) -> Generation {
        let Some(from) = Fraction::from_f64(from_cycle) else {
            return self.replace_pattern(pattern, now, cps);
        };
        let end = end_cycle.and_then(Fraction::from_f64);
        // The end of the window as a cycle: where the outgoing mapping
        // reaches the edge, or `end_cycle` when that is sooner.
        let edge = self.cycle_at_or_after_time(takeover_edge);
        let until = match (edge, end) {
            (Some(edge), Some(end)) => Some(edge.min(end)),
            (edge, end) => edge.or(end),
        };
        // Taken before the replace, as above. After a retime that moved the
        // phase, the cycle of a copy names another onset. The window then
        // reads each time on the current mapping, as a guess.
        let retimed = self.retimed;
        let unplayed_from = self.unplayed_from(from, retimed);
        let copies = self.take_device_copies();
        let copies = Self::split_device_copies(copies, from, unplayed_from, |key, time| {
            let time = if retimed {
                self.time_at_cycle(key.begin)
            } else {
                time
            };
            time < takeover_edge && end.is_none_or(|end| key.begin.lt(&end))
        });
        self.replace_pattern_carrying(pattern, now, cps, from_cycle, copies, until)
    }

    /// Replace the pattern from `from_cycle`, with `carried` (the outgoing
    /// onsets the device keeps) pre-marked under the new generation and the
    /// takeover window ending at `until`. `dropped` are the outgoing onsets
    /// the device drops at the takeover.
    fn replace_pattern_carrying(
        &mut self,
        pattern: Pattern,
        now: f64,
        cps: Option<f64>,
        from_cycle: f64,
        (carried, dropped): (Vec<DeviceCopy>, Vec<DeviceCopy>),
        until: Option<Fraction>,
    ) -> Generation {
        // Lower the cursor before the replace. `replace_pattern` re-anchors
        // the mapping at `(now, queried_to)`, so the lowered cursor keeps
        // the anchor at the edit instant's true cycle instead of the far
        // cursor, and the mapping's phase survives the edit. A cursor
        // behind the edit instant only means the producer fell behind. The
        // span between them is already in the past and cannot be heard, so
        // raising the cursor to the edit loses nothing.
        let Some(from) = Fraction::from_f64(from_cycle) else {
            return self.replace_pattern(pattern, now, cps);
        };
        self.queried_to = from;
        let retimed = self.retimed;
        let generation = self.replace_pattern(pattern, now, cps);
        // The explicit-start boundary still holds: a preroll edit before the
        // transport's first cycle must not re-query into it. Clamp only the
        // cursor, and only after the replace. The pre-replace value above
        // became the mapping anchor, and a clamp on that value would move
        // cycle zero back to the edit instant and shift the whole timeline
        // (the first onset would land at the edit clock instead of the
        // promised beginning). The preroll overlap window sits entirely
        // before the beginning, so nothing was pre-marked there and nothing
        // is suppressed.
        self.queried_to = self.at_or_after_start(self.queried_to);
        // Re-mark under the new generation. The same onsets, by begin, are
        // what a changed or moved hap takes instead.
        self.carry_device_copies(generation, carried, from, true);
        // After a retime that moved the phase, a dropped copy must not come
        // back as a mark.
        if !retimed {
            self.superseded = dropped;
        }
        if let Some(until) = until {
            self.overlap_until = until;
        }
        generation
    }

    /// The haps of one query pass that the device's outgoing copies stand
    /// in for: position in `haps` to (the copy's begin, the copy).
    ///
    /// Inside a continued replacement's takeover window the device plays
    /// each outgoing copy. The pre-marks suppress an incoming hap that
    /// matches a copy exactly. The other copies go to haps of the window
    /// that have no old key, one copy to one hap:
    ///
    /// 1. A moved hap: the copy's own value at another begin or length.
    ///    The nearest pair goes first.
    /// 2. A changed hap: on the copy's begin, the hap that shares the most
    ///    controls with the copy. Of equals, the earliest in the query.
    ///
    /// The haps that remain are the added ones, and they are emitted. A
    /// count per begin is not enough: an added lane that the query yields
    /// before a changed lane would take the copy, and the changed hap would
    /// sound together with it.
    ///
    /// A copy with its mark behind the cursor follows neither rule. No
    /// pass has the hap its mark names, so the copy never has an exact
    /// match, and as a moved copy it would take an added hap. It stands in
    /// only for a hap of the window with its value and, within
    /// [`SAME_INSTANT_SECS`], its time: emitted, that hap sounds together
    /// with the copy.
    ///
    /// Nothing is consumed here: a pass that the queue cap cuts short
    /// queries its unqueued onsets again and must decide the same. The
    /// acceptance loop removes a copy when it skips the copy's hap, and
    /// marks that hap as emitted so the next pass does not count it again.
    ///
    /// A window can be wider than one pass. A pass uses only the copies
    /// before `query_end`: the haps that match the later copies are not in
    /// `haps`, and a later copy with no match here would go to an added
    /// hap of this pass. The cost: a hap that moved to before `query_end`
    /// from a begin after it is emitted, and its copy sounds too.
    fn overlap_stand_ins(
        &self,
        haps: &[Hap],
        generation: Generation,
        query_end: Fraction,
    ) -> std::collections::HashMap<usize, (Fraction, HeldOnset)> {
        use std::collections::HashMap;
        let mut stand_ins = HashMap::new();
        if self.overlap_held.is_empty() {
            return stand_ins;
        }
        // The outgoing copies no incoming hap matches exactly, and the
        // incoming haps of the window with no old key, in query order.
        let mut unclaimed = self.overlap_held.clone();
        unclaimed.retain(|begin, _| begin.lt(&query_end));
        // The last field: the hap is inside the window, so it can be a
        // moved hap.
        let mut waiting: Vec<(usize, Fraction, Arc<str>, bool)> = Vec::new();
        let mut ordinals: HashMap<EmittedKey, u32> = HashMap::new();
        for (position, hap) in haps.iter().enumerate() {
            if !hap.has_onset() {
                continue;
            }
            let Some(whole) = hap.whole else { continue };
            // From the takeover on the replacement plays alone. The
            // exception is a hap on the begin of a copy the device keeps
            // past it.
            let inside = whole.begin.lt(&self.overlap_until);
            if !inside && !unclaimed.contains_key(&whole.begin) {
                continue;
            }
            // The same key, with the same ordinal, as the acceptance loop.
            let mut key = EmittedKey {
                generation,
                begin: whole.begin,
                end: whole.end,
                value: Arc::from(hap.value.show().as_str()),
                occurrence: 0,
            };
            let occurrence = ordinals.entry(key.clone()).or_insert(0);
            key.occurrence = *occurrence;
            *occurrence += 1;
            let exact = unclaimed.get_mut(&whole.begin).and_then(|copies| {
                let exact = copies.iter().position(|copy| {
                    copy.end == key.end
                        && copy.occurrence == key.occurrence
                        && copy.value == key.value
                })?;
                Some(copies.remove(exact))
            });
            if exact.is_none() && !self.emitted.contains_key(&key) {
                waiting.push((position, whole.begin, key.value, inside));
            }
            // Otherwise the pre-mark has it, or an earlier pass dealt with it.
        }
        // The table has no order, and every pass must decide the same.
        let mut copies: Vec<(Fraction, HeldOnset)> = unclaimed
            .into_iter()
            .flat_map(|(begin, copies)| copies.into_iter().map(move |copy| (begin, copy)))
            .collect();
        copies.sort();
        let mut copy_free = vec![true; copies.len()];
        let mut hap_free = vec![true; waiting.len()];

        // A copy behind the cursor: only a hap of the window with its value
        // and its time. The hap its begin names is in no pass, so the rules
        // below do not apply to it. Past the pair limit it stands in for
        // nothing.
        let behind = copies.iter().filter(|(_, held)| held.behind).count();
        let compared = behind.saturating_mul(waiting.len()) <= MOVED_PAIRS_LIMIT;
        for (copy, (copy_begin, held)) in copies.iter().enumerate() {
            if !held.behind {
                continue;
            }
            copy_free[copy] = false;
            if !compared {
                continue;
            }
            let Some(&time) = self.emitted.get(&EmittedKey {
                generation,
                begin: *copy_begin,
                end: held.end,
                value: Arc::clone(&held.value),
                occurrence: held.occurrence,
            }) else {
                continue;
            };
            // The nearest in time. Of equals, the earliest in the query.
            let same_instant = waiting
                .iter()
                .enumerate()
                .filter(|(hap, (_, _, value, inside))| {
                    hap_free[*hap] && *inside && *value == held.value
                })
                .map(|(hap, (_, begin, _, _))| ((self.time_at_cycle(*begin) - time).abs(), hap))
                .filter(|(gap, _)| *gap <= SAME_INSTANT_SECS)
                .min_by(|a, b| a.0.total_cmp(&b.0));
            if let Some((_, hap)) = same_instant {
                hap_free[hap] = false;
                stand_ins.insert(waiting[hap].0, copies[copy].clone());
            }
        }

        // A moved hap: the copy's own value, the nearest begin first.
        let mut by_value: HashMap<&str, (Vec<usize>, Vec<usize>)> = HashMap::new();
        for (copy, (_, held)) in copies.iter().enumerate() {
            if copy_free[copy] {
                by_value.entry(&held.value).or_default().0.push(copy);
            }
        }
        for (hap, (_, _, value, inside)) in waiting.iter().enumerate() {
            if *inside
                && hap_free[hap]
                && let Some(same) = by_value.get_mut(&**value)
            {
                same.1.push(hap);
            }
        }
        let mut pairs: Vec<(Fraction, usize, usize)> = Vec::new();
        for (same_copies, same_haps) in by_value.values() {
            if same_copies.len().saturating_mul(same_haps.len()) <= MOVED_PAIRS_LIMIT {
                for &copy in same_copies {
                    for &hap in same_haps {
                        pairs.push((apart(copies[copy].0, waiting[hap].1), hap, copy));
                    }
                }
            } else {
                let mut in_order = same_haps.clone();
                in_order.sort_by_key(|hap| waiting[*hap].1);
                for (&copy, &hap) in same_copies.iter().zip(&in_order) {
                    pairs.push((apart(copies[copy].0, waiting[hap].1), hap, copy));
                }
            }
        }
        pairs.sort();
        for (_, hap, copy) in pairs {
            if copy_free[copy] && hap_free[hap] {
                copy_free[copy] = false;
                hap_free[hap] = false;
                stand_ins.insert(waiting[hap].0, copies[copy].clone());
            }
        }

        // A changed hap: on the copy's begin, the most alike first.
        let mut by_begin: HashMap<Fraction, (Vec<usize>, Vec<usize>)> = HashMap::new();
        for (copy, (begin, _)) in copies.iter().enumerate() {
            if copy_free[copy] {
                by_begin.entry(*begin).or_default().0.push(copy);
            }
        }
        for (hap, (_, begin, _, _)) in waiting.iter().enumerate() {
            if hap_free[hap]
                && let Some(same) = by_begin.get_mut(begin)
            {
                same.1.push(hap);
            }
        }
        for (same_copies, mut same_haps) in by_begin.into_values() {
            // No more haps than copies: every one of them is a changed hap.
            // The most alike still go together: a hap takes the time of its
            // copy, and two copies on one begin can have two times. Past
            // the pair limit each copy takes the first hap.
            let in_order = same_haps.len() <= same_copies.len()
                && same_copies.len().saturating_mul(same_haps.len()) > MOVED_PAIRS_LIMIT;
            for copy in same_copies {
                let nearest = if in_order {
                    0
                } else {
                    // The first of the most alike; `max_by_key` keeps the
                    // last, so the search runs backwards.
                    (0..same_haps.len())
                        .rev()
                        .max_by_key(|at| {
                            shared_words(&copies[copy].1.value, &waiting[same_haps[*at]].2)
                        })
                        .unwrap_or(0)
                };
                if nearest >= same_haps.len() {
                    break;
                }
                let hap = same_haps.remove(nearest);
                stand_ins.insert(waiting[hap].0, copies[copy].clone());
            }
        }
        stand_ins
    }

    /// Abandon un-queried music whose time has already passed, resuming at
    /// `now`. Returns the seconds dropped, or `None` if the cursor is within
    /// `keep` seconds of the clock.
    ///
    /// For realtime use only: an offline render must produce every cycle.
    /// `tick` always queries forward from `queried_to`, so a producer that
    /// falls behind spends each tick on history that nobody can hear, and
    /// each tick costs more as the gap grows. Past the query budget every
    /// tick is refused, the cursor holds, and the gap never closes.
    ///
    /// The skipped span is silence either way. The next tick must be cheap
    /// and audible. Only the cursor moves: the cycle-to-time mapping does
    /// not change, so the music resumes in phase and does not restart the
    /// bar.
    pub fn skip_past_gap(&mut self, now: f64, keep: f64) -> Option<f64> {
        let behind = now - self.scheduled_to();
        if !behind.is_finite() || behind <= keep.max(0.0) {
            return None;
        }
        let resume = Fraction::from_f64(self.cycle_at_time(now))?;
        if resume <= self.queried_to {
            return None;
        }
        self.queried_to = resume;
        self.last_fill_time = now;
        Some(behind)
    }

    /// Re-open a scheduling window the scheduler already consumed, without
    /// bumping the generation: rewind the query cursor to `cursor_time`, drop
    /// the events still queued from the consumed pass, and clear the emission
    /// marks of the current generation at or after the cursor, so the same
    /// span re-queries and re-emits in full.
    ///
    /// The use case is the first window after a rewind, when its onsets are
    /// refused while their samples still decode. The cursor is already past
    /// them, so a plain retry would resume beyond the takeover and lose the
    /// onsets of cycle zero. Nothing of a refused prefill window reaches the
    /// device ring, so re-emitting it cannot double anything audible. A
    /// re-query that must keep the generation's earlier audible windows uses
    /// [`Self::requery_active`] (whose generation bump clears the set
    /// outright); the current-generation filter here is what makes a reopen
    /// safe under one.
    pub fn reopen_window_at(&mut self, now: f64, cursor_time: f64) -> bool {
        if self.pattern.is_none() || !now.is_finite() || !cursor_time.is_finite() {
            return false;
        }
        let Some(cursor) = Fraction::from_f64(self.cycle_at_time(cursor_time)) else {
            return false;
        };
        let cursor = self.at_or_after_start(cursor);
        if cursor >= self.queried_to {
            return false;
        }
        let generation = self.transport.generation();
        self.emitted
            .retain(|key, _| key.generation != generation || key.begin.lt(&cursor));
        self.overlap_held.clear();
        self.queue.clear();
        self.refused = None;
        self.queried_to = cursor;
        self.last_fill_time = now;
        true
    }

    /// How far the pattern has been queried, in cycles. Onsets from here on
    /// are not the audio's yet; a painter that wants to show them has to
    /// ask the pattern itself.
    pub fn scheduled_to_cycle(&self) -> f64 {
        self.queried_to.to_f64()
    }

    /// Wall-clock time up to which the pattern has been queried.
    fn scheduled_to(&self) -> f64 {
        self.anchor_time + self.queried_to.sub(self.anchor_cycle).to_f64() / self.cps
    }

    /// Seconds of schedule still buffered ahead of `now` - the real bound on
    /// how long evaluation may block querying.
    ///
    /// Measured from how far the pattern has been queried, not from the latest
    /// queued event: a span may legitimately contain no onsets (silence, a
    /// sparse pattern), and treating that as "no horizon" would report a
    /// drained schedule while the transport is perfectly healthy.
    pub fn horizon_remaining(&self, now: f64) -> f64 {
        (self.scheduled_to() - now).max(0.0)
    }

    /// Smallest schedule deficit worth querying for this horizon.
    pub fn refill_floor_seconds(&self) -> f64 {
        refill_floor_secs(self.horizon)
    }

    /// The refusal from the last [`TickStatus::Refused`], if any.
    pub fn refusal(&self) -> Option<&QueryLimit> {
        self.refused.as_ref()
    }

    /// Take the message from the most recent query that threw.
    ///
    /// Taken rather than read so a caller reports each throw once, however
    /// many times a second the producer queries.
    pub fn take_thrown(&mut self) -> Option<String> {
        self.thrown.take()
    }

    /// Take the contained callback failure from the most recent tick, if any.
    ///
    /// Taken rather than read so a caller reports each failure once, however
    /// many times a second the producer queries.
    pub fn take_callback_failure(&mut self) -> Option<String> {
        self.callback_failure.take()
    }

    /// Take the throw a stack contained in the most recent tick, if any.
    ///
    /// Its message also arrives through [`Self::take_callback_failure`]
    /// unless an earlier failure in the same tick took that slot.
    pub fn take_contained_throw(&mut self) -> Option<String> {
        self.contained_throw.take()
    }

    pub fn queued(&self) -> usize {
        self.queue.len()
    }

    /// Fill the horizon by querying the pattern. This is the step that may
    /// enter JavaScript, so it never runs on the audio thread.
    pub fn tick(&mut self, clock: &dyn Clock) -> TickStatus {
        self.tick_with_max_span_cycles(clock, MAX_TICK_SPAN_CYCLES)
    }

    /// Profile one ordinary tick without changing its query-span policy.
    pub fn tick_profiled(
        &mut self,
        clock: &dyn Clock,
        profile: &mut SchedulerTickProfile,
    ) -> TickStatus {
        self.tick_with_max_span_cycles_profiled(clock, MAX_TICK_SPAN_CYCLES, profile)
    }

    /// Fill the horizon while limiting the amount of pattern work committed
    /// by one atomic query. The global allocation guard remains authoritative;
    /// callers may only tighten it.
    pub fn tick_with_max_span_cycles(
        &mut self,
        clock: &dyn Clock,
        max_span_cycles: f64,
    ) -> TickStatus {
        let status = self.tick_with_max_span_cycles_inner(clock, max_span_cycles, None);
        self.settle_takeover(status)
    }

    /// Profile one tick at the same semantic boundary as
    /// [`Self::tick_with_max_span_cycles`].
    pub fn tick_with_max_span_cycles_profiled(
        &mut self,
        clock: &dyn Clock,
        max_span_cycles: f64,
        profile: &mut SchedulerTickProfile,
    ) -> TickStatus {
        *profile = SchedulerTickProfile::default();
        let status = self.tick_with_max_span_cycles_inner(clock, max_span_cycles, Some(profile));
        self.settle_takeover(status)
    }

    /// The producer publishes a generation after its first tick that fills
    /// the horizon or finds it full. The device then drops the copies that
    /// the takeover of the generation does not keep.
    fn settle_takeover(&mut self, status: TickStatus) -> TickStatus {
        if matches!(status, TickStatus::Filled | TickStatus::HorizonFull) {
            self.superseded.clear();
        }
        status
    }

    fn tick_with_max_span_cycles_inner(
        &mut self,
        clock: &dyn Clock,
        max_span_cycles: f64,
        mut profile: Option<&mut SchedulerTickProfile>,
    ) -> TickStatus {
        if self.transport.is_stopped() {
            self.clear_trace();
            if let Some(profile) = profile.as_deref_mut() {
                profile.stopped = true;
            }
            return TickStatus::Stopped;
        }
        let now = clock.now();
        // A query has to be worth making; see `refill_floor_secs`. A queue at
        // its cap has no room for one: the caller drains before the fill
        // resumes at the held cursor.
        if self.queue_at_cap()
            || self.horizon_remaining(now) > self.horizon - refill_floor_secs(self.horizon)
        {
            // A covered horizon is still fresh even when no query is needed.
            // At slow tempos, rounding up to one millicycle can cover more than
            // `stale_limit`; draining must not expire those scheduled onsets.
            self.last_fill_time = now;
            if let Some(profile) = profile.as_deref_mut() {
                profile.horizon_full = true;
            }
            return TickStatus::HorizonFull;
        }
        let Some(pattern) = self.pattern.clone() else {
            if let Some(profile) = profile.as_deref_mut() {
                profile.horizon_full = true;
            }
            return TickStatus::HorizonFull;
        };
        let generation = self.transport.generation();

        // Query forward until the horizon is covered from `now`, not a fixed
        // span: after a stall the schedule can be arbitrarily far behind.
        let target_time = now + self.horizon;
        // Clamped BEFORE the query, which is where the allocation happens.
        let max_span_cycles = if max_span_cycles.is_finite() && max_span_cycles > 0.0 {
            max_span_cycles.min(MAX_TICK_SPAN_CYCLES)
        } else {
            MAX_TICK_SPAN_CYCLES
        };
        let cycles_needed =
            ((target_time - self.scheduled_to()) * self.cps).clamp(0.0, max_span_cycles);
        // Quantize to whole millicycles so query windows stay exact rationals
        // over 1000. Truncation normally leaves a small deficit for the next tick.
        //
        // Round a smaller positive deficit up, when the cap allows, so even a
        // very slow tempo advances. At 0.0005 cps one millicycle is two seconds,
        // beyond the default horizon; the covered-horizon gate above keeps that
        // queue fresh until another query is needed.
        let mut millicycles = (cycles_needed * 1000.0) as i128;
        if millicycles == 0 && cycles_needed > 0.0 && max_span_cycles >= 0.001 {
            millicycles = 1;
        }
        let span_cycles = Fraction::new(millicycles, 1000);
        // A zero span does not prove the horizon is covered: the caller's cap
        // may be below one millicycle, or the deficit may be nonpositive or NaN.
        // Leave the stale clock unchanged.
        if span_cycles.numer() == 0 {
            if let Some(profile) = profile.as_deref_mut() {
                profile.horizon_full = true;
            }
            return TickStatus::HorizonFull;
        }
        let begin = self.queried_to;
        let end = begin.add(span_cycles);
        if let Some(profile) = profile.as_deref_mut() {
            profile.query_span_millicycles =
                (span_cycles.to_f64().max(0.0) * 1000.0).round() as u64;
        }

        let mut state = State::new(TimeSpan::new(begin, end));
        // `_cps` marks this as a real trigger query, which `glide`-style
        // patterns read to distinguish it from a lookahead.
        state.controls.push(("_cps".into(), Value::F64(self.cps)));
        // Apply the top-level query error boundary: thrown queries produce
        // no onsets, while resource refusals remain distinct from silence so
        // callers can retry or report an incomplete render.
        let profiling = profile.is_some();
        let query_started = profiling.then(std::time::Instant::now);
        let mut callback = rustel_core::CallbackQueryMetrics::default();
        let query = if profiling {
            rustel_core::with_callback_query_metrics(&mut callback, || {
                pattern.query_arc_outcome_with_budget(&state, self.query_hap_budget)
            })
        } else {
            pattern.query_arc_outcome_with_budget(&state, self.query_hap_budget)
        };
        if let Some(profile) = profile.as_deref_mut() {
            profile.query_nanos =
                duration_nanos(query_started.expect("profiled query has a start").elapsed());
            profile.callback_calls = callback.calls();
            profile.callback_kind_sample = callback.kind_sample();
            profile.callback_nanos = callback.busy_nanos();
        }
        let haps: Vec<Hap> = match query {
            Ok(rustel_core::QueryArcOutcome::Haps(haps)) => haps,
            // The `queryArc` catch: this window is silent, and the throw is
            // kept so somebody can say why.
            Ok(rustel_core::QueryArcOutcome::Thrown(message)) => {
                self.thrown = Some(message);
                Vec::new()
            }
            Err(limit) => {
                // The cursor does not advance, by design. A refusal means
                // the work could not be done, and the same span must be
                // schedulable when the pressure passes (the recovery tests
                // check this). The live layer bounds the retries: see the
                // starvation rollback in `live.rs`.
                self.refused = Some(limit);
                if let Some(profile) = profile.as_deref_mut() {
                    profile.refused = true;
                }
                return TickStatus::Refused;
            }
        };
        // A contained per-hap callback failure (a throwing filter predicate)
        // did not abort the query; remember it so the caller can report it
        // once. First failure per tick wins.
        if self.callback_failure.is_none()
            && let Some(failure) = rustel_core::take_query_callback_failure()
        {
            self.callback_failure = Some(failure);
        }
        if self.contained_throw.is_none()
            && let Some(thrown) = rustel_core::take_query_contained_throw()
        {
            self.contained_throw = Some(thrown);
        }
        if let Some(profile) = profile.as_deref_mut() {
            profile.queried_haps = u64::try_from(haps.len()).unwrap_or(u64::MAX);
        }

        let acceptance_started = profiling.then(std::time::Instant::now);
        let mut queue_full = false;
        // Earliest onset the cap left unqueued; the cursor stops there.
        let mut unprocessed_begin: Option<Fraction> = None;
        // Inside a continued replacement's takeover window: the changed or
        // moved haps the device's outgoing copies stand in for.
        let mut stand_ins = self.overlap_stand_ins(&haps, generation, end);
        // Assign stable ordinals to identical haps so stacked voices remain
        // distinct while a repeated query remains deduplicated.
        self.pass_ordinals.clear();
        let mut position = 0usize;
        let mut pending = haps.into_iter();
        while let Some(hap) = pending.next() {
            let at = position;
            position += 1;
            // Checked INSIDE the fill loop, not after it: the point is to stop
            // allocating, so a caller-side check on the finished vector is too
            // late by construction.
            if self.queue_at_cap() {
                queue_full = true;
                unprocessed_begin = std::iter::once(&hap)
                    .chain(pending.as_slice())
                    .filter(|hap| hap.has_onset())
                    .map(|hap| hap.part.begin)
                    .min();
                break;
            }
            if !hap.has_onset() {
                continue;
            }
            let Some(whole) = hap.whole else { continue };
            let value_show = hap.value.show();
            let mut key = EmittedKey {
                generation,
                begin: whole.begin,
                end: whole.end,
                value: Arc::from(value_show.as_str()),
                occurrence: 0,
            };
            let occurrence = self.pass_ordinals.entry(key.clone()).or_insert(0);
            key.occurrence = *occurrence;
            *occurrence += 1;
            let cycles_from_anchor = whole.begin.sub(self.anchor_cycle).to_f64();
            let target_time = self.anchor_time + cycles_from_anchor / self.cps;
            if let Some((copy_begin, copy)) = stand_ins.remove(&at) {
                // The device plays an outgoing copy in this hap's place until
                // the takeover. The copy is spent and the hap counts as
                // emitted, so a re-query of this begin decides the same
                // again.
                if let Some(held) = self.overlap_held.get_mut(&copy_begin)
                    && let Some(spent) = held.iter().position(|held| *held == copy)
                {
                    held.remove(spent);
                }
                // The hap takes the mark of the copy, at the time of the
                // copy: one mark for one copy on the device.
                let copy_time = self.emitted.remove(&EmittedKey {
                    generation,
                    begin: copy_begin,
                    end: copy.end,
                    value: copy.value,
                    occurrence: copy.occurrence,
                });
                self.emitted.insert(key, copy_time.unwrap_or(target_time));
                continue;
            }
            match self.emitted.entry(key) {
                std::collections::hash_map::Entry::Occupied(marked) => {
                    // The copy with this key is this hap's. A later pass of
                    // the window must not give it to another hap.
                    let key = marked.key();
                    if let Some(held) = self.overlap_held.get_mut(&key.begin)
                        && let Some(own) = held.iter().position(|held| {
                            held.end == key.end
                                && held.occurrence == key.occurrence
                                && held.value == key.value
                        })
                    {
                        held.remove(own);
                    }
                    continue;
                }
                std::collections::hash_map::Entry::Vacant(free) => {
                    free.insert(target_time);
                }
            }
            let duration = hap.duration();
            let onset_id = self.next_onset_id;
            // Trace reads the complete hap, so capture it before transferring
            // the already-owned value into the scheduler event. A full or
            // disabled trace queue keeps the same short-circuit behavior.
            let trace = if self.trace_enabled && self.trace_events.len() < MAX_QUEUED_TRACE_EVENTS {
                ScheduleTraceEvent::from_hap(&hap, generation, onset_id, target_time)
            } else {
                None
            };
            let ui_visuals = hap.ui_visuals_context();
            let log_line = hap.log_line().map(std::sync::Arc::from);
            self.queue.push(Event {
                onset_id,
                generation,
                whole_begin: whole.begin,
                whole_end: whole.end,
                duration,
                target_time,
                value: hap.value,
                live_controls: hap.live_controls,
                ui_visuals,
                log_line,
            });
            if let Some(profile) = profile.as_deref_mut() {
                profile.accepted_events = profile.accepted_events.saturating_add(1);
            }
            self.next_onset_id += 1;
            // Scheduler-queue acceptance is the trace boundary: analog haps
            // and duplicate onsets never appear here. The live host correlates
            // these IDs with its downstream audio-ring pushes before exposing
            // them as device-submitted UI events.
            if self.trace_enabled {
                if let Some(trace) = trace {
                    self.trace_events.push(trace);
                } else {
                    self.trace_events_dropped = self.trace_events_dropped.saturating_add(1);
                    self.trace_events_dropped_total =
                        self.trace_events_dropped_total.saturating_add(1);
                }
            }
        }
        self.clear_pass_ordinals();
        // The cursor advances only past onsets this fill queued, so a
        // queue-full fill resumes at its earliest unqueued onset; the `emitted`
        // keys keep the retry from queueing an onset twice.
        self.queried_to = unprocessed_begin
            .map(|onset| onset.max(begin).min(end))
            .unwrap_or(end);
        // Past the takeover a copy behind the cursor has nothing left to
        // stand in for. A copy ahead waits for the hap on its own begin.
        if !self.overlap_held.is_empty() && !self.queried_to.lt(&self.overlap_until) {
            let cursor = self.queried_to;
            self.overlap_held.retain(|begin, _| !begin.lt(&cursor));
        }
        // Once per fill, not once per hap: `retain` is a full scan, and a
        // dense pattern whose live window genuinely exceeds the threshold
        // would otherwise scan the whole set for every onset in it.
        self.prune_emitted();
        if let Some(profile) = profile.as_deref_mut() {
            profile.acceptance_nanos = duration_nanos(
                acceptance_started
                    .expect("profiled acceptance has a start")
                    .elapsed(),
            );
        }
        self.last_fill_time = now;
        if queue_full {
            if let Some(profile) = profile {
                profile.queue_full = true;
            }
            return TickStatus::QueueFull;
        }
        TickStatus::Filled
    }

    fn queue_at_cap(&self) -> bool {
        self.queue.len() >= MAX_QUEUED_EVENTS
    }

    fn clear_pass_ordinals(&mut self) {
        self.pass_ordinals.clear();
        if self.pass_ordinals.capacity() > PASS_ORDINAL_RETAIN_LIMIT {
            self.pass_ordinals = std::collections::HashMap::new();
        }
    }

    /// Consume everything due by `now`.
    ///
    /// Events from cancelled generations are dropped here - the defined
    /// boundary - so a replaced pattern can never interleave with its
    /// successor. Stop is checked first and independently of the queue.
    pub fn drain_due(&mut self, clock: &dyn Clock) -> Vec<Event> {
        self.drain_through(clock, clock.now())
    }

    /// Transfer queued events through a future deadline to a downstream
    /// lookahead buffer such as the audio SPSC ring.
    ///
    /// Staleness is still judged against the REAL clock, not the future
    /// transfer deadline. Treating lookahead as current time would expire a
    /// healthy schedule merely because the consumer asked for prefilling.
    pub fn drain_through(&mut self, clock: &dyn Clock, through: f64) -> Vec<Event> {
        if self.transport.is_stopped() {
            // Stop is immediate: pending events are discarded, not delivered
            // late. It is never stuck behind an evaluation in progress.
            self.queue.clear();
            self.clear_trace();
            return Vec::new();
        }
        let now = clock.now();
        let through = through.max(now);
        let generation = self.transport.generation();

        // Stale-schedule expiry: if no tick has refilled the horizon or found
        // it covered within the limit (evaluation blocked querying for too
        // long), degrade rather than delivering increasingly stale events.
        if now - self.last_fill_time > self.stale_limit {
            self.queue.retain(|e| e.target_time > now);
            return Vec::new();
        }

        let mut due = Vec::new();
        for event in self.queue.extract_if(.., |event| {
            event.generation != generation || event.target_time <= through
        }) {
            if event.generation != generation {
                continue; // cancelled generation: dropped, never delivered
            }
            // The queue already owns the event. Move it across the producer
            // boundary instead of cloning its potentially deep control value
            // immediately before dropping the original.
            due.push(event);
        }
        // `total_cmp` keeps a non-finite target from panicking the live set.
        // Non-finite onsets are still delivered in a deterministic order and
        // filtered by ordinary downstream validation where applicable.
        due.sort_by(|a, b| a.target_time.total_cmp(&b.target_time));
        due
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rustel_core::{fastcat, pure, stack};

    mod hap_budget {
        use super::*;

        #[test]
        fn a_hap_budget_refusal_leaves_the_window_available_to_retry() {
            let clock = VirtualClock::new(0.0);
            let mut scheduler = Scheduler::new(Arc::new(Transport::default()), 1.0, 1.0);
            scheduler.set_pattern(fastcat(vec![atom("hh"); 16]), clock.now());
            scheduler.set_query_hap_budget(8);

            assert_eq!(scheduler.tick(&clock), TickStatus::Refused);
            assert!(matches!(
                scheduler.refusal(),
                Some(QueryLimit::HapBudget { budget: 8 })
            ));
            assert_eq!(scheduler.queued(), 0);

            scheduler.set_query_hap_budget(16);
            assert_eq!(scheduler.tick(&clock), TickStatus::Filled);
            assert_eq!(scheduler.queued(), 16);
        }
    }

    fn atom(value: &str) -> Pattern {
        pure(Value::Str(value.into()))
    }

    fn object(entries: &[(&str, Value)]) -> Value {
        Value::Object(rustel_core::OrderedMap::from_entries(
            entries
                .iter()
                .map(|(key, value)| ((*key).to_owned(), value.clone())),
        ))
    }

    #[test]
    fn pass_ordinal_scratch_reuses_ordinary_buckets_and_drops_exceptional_capacity() {
        let mut scheduler = Scheduler::new(Arc::new(Transport::default()), 1.0, 0.5);

        scheduler.pass_ordinals.reserve(128);
        let ordinary_capacity = scheduler.pass_ordinals.capacity();
        scheduler.clear_pass_ordinals();
        assert_eq!(scheduler.pass_ordinals.capacity(), ordinary_capacity);

        scheduler
            .pass_ordinals
            .reserve(PASS_ORDINAL_RETAIN_LIMIT + 1);
        assert!(scheduler.pass_ordinals.capacity() > PASS_ORDINAL_RETAIN_LIMIT);
        scheduler.clear_pass_ordinals();
        assert_eq!(scheduler.pass_ordinals.capacity(), 0);
    }

    /// `from_hap` carries strudel.cc's `activeLabel` and the tuning
    /// `edoScale` writes on a value; a malformed tuning is left off rather
    /// than half-read.
    #[test]
    fn a_trace_carries_the_active_label_and_the_edo_scale_the_score_wrote() {
        let span = TimeSpan::new(Fraction::ZERO, Fraction::ONE);
        let scale = |indexes: &[f64]| {
            object(&[
                ("note", Value::Str("c4".into())),
                ("label", Value::Str("do".into())),
                ("activeLabel", Value::Str("!".into())),
                ("edo", Value::F64(12.0)),
                ("root", Value::Str("130.8128".into())),
                (
                    "degreeIndexes",
                    Value::List(indexes.iter().copied().map(Value::F64).collect()),
                ),
                (
                    "intLabels",
                    Value::List(vec![
                        Value::Null,
                        Value::Str("P1".into()),
                        Value::Str("M2".into()),
                    ]),
                ),
            ])
        };
        let hap = Hap::new(
            Some(span),
            span,
            scale(&[0.0, 2.0, 4.0, 5.0, 7.0, 9.0, 11.0]),
        );
        let trace = ScheduleTraceEvent::from_hap(&hap, 3, 7, 1.5).expect("a whole span");
        assert_eq!(trace.onset_id, 7);
        assert_eq!(trace.generation, 3);
        assert_eq!(trace.target_time, 1.5);
        assert_eq!(trace.label.as_deref(), Some("do"));
        assert_eq!(trace.active_label.as_deref(), Some("!"));
        let tuning = trace.scale.expect("the tuning rides the trace");
        assert_eq!(tuning.edo, 12);
        assert!((tuning.root_hz - 130.8128).abs() < 0.001);
        assert_eq!(tuning.degree_indexes, vec![0, 2, 4, 5, 7, 9, 11]);
        assert_eq!(
            tuning.interval_labels,
            vec!["P1", "M2", "", "", "", "", ""]
                .into_iter()
                .map(String::from)
                .collect::<Vec<_>>()
        );

        // An index at or past the octave is not a degree of it.
        let hap = Hap::new(Some(span), span, scale(&[0.0, 12.0]));
        let trace = ScheduleTraceEvent::from_hap(&hap, 3, 8, 1.5).expect("a whole span");
        assert_eq!(trace.scale, None);
        assert_eq!(trace.active_label.as_deref(), Some("!"));

        // No whole span, no trace.
        let analog = Hap::new(None, span, Value::Str("bd".into()));
        assert!(ScheduleTraceEvent::from_hap(&analog, 3, 9, 1.5).is_none());
    }

    /// A pitched sample carries its pitch into the trace; an unpitched one
    /// carries none, and a bare `n` is a sample index, not a note.
    #[test]
    fn a_trace_carries_the_pitch_the_score_wrote_and_nothing_for_a_drum() {
        let c4 = 440.0 * 2f32.powf((60.0 - 69.0) / 12.0);
        let close = |hertz: Option<f32>, expected: f32| {
            hertz.is_some_and(|hertz| (hertz - expected).abs() < 0.01)
        };
        let piano = object(&[
            ("note", Value::Str("c4".into())),
            ("s", Value::Str("piano".into())),
        ]);
        assert!(
            close(trace_pitch_hz(&piano), c4),
            "{:?}",
            trace_pitch_hz(&piano)
        );
        let midi = object(&[
            ("note", Value::F64(60.0)),
            ("s", Value::Str("piano".into())),
        ]);
        assert!(close(trace_pitch_hz(&midi), c4));
        let bare_name = object(&[("note", Value::Str("c".into()))]);
        assert!(close(
            trace_pitch_hz(&bare_name),
            440.0 * 2f32.powf((48.0 - 69.0) / 12.0)
        ));
        let freq = object(&[
            ("freq", Value::F64(100.0)),
            ("note", Value::Str("c4".into())),
        ]);
        assert_eq!(trace_pitch_hz(&freq), Some(100.0), "freq wins over note");
        let drum = object(&[("s", Value::Str("bd".into()))]);
        assert_eq!(trace_pitch_hz(&drum), None);
        let indexed = object(&[("s", Value::Str("bd".into())), ("n", Value::F64(3.0))]);
        assert_eq!(trace_pitch_hz(&indexed), None, "n is a sample index");
        assert_eq!(trace_pitch_hz(&Value::Str("bd".into())), None);
        let nonsense = object(&[("note", Value::Str("not a note".into()))]);
        assert_eq!(trace_pitch_hz(&nonsense), None);
        let silent = object(&[("freq", Value::F64(0.0))]);
        assert_eq!(trace_pitch_hz(&silent), None);

        // Through the scheduler: the trace of a pitched sample has the
        // pitch, the drum's has none, and neither has a gain yet.
        let first_trace = |value: Value| {
            let mut scheduler = Scheduler::new(Arc::new(Transport::default()), 1.0, 0.5);
            let clock = VirtualClock::new(0.0);
            scheduler.set_trace_enabled(true);
            scheduler.set_pattern(pure(value), clock.now());
            assert_eq!(scheduler.tick(&clock), TickStatus::Filled);
            scheduler
                .take_trace_events()
                .into_iter()
                .next()
                .expect("one trace")
        };
        let pitched = first_trace(piano);
        assert!(
            close(pitched.frequency_hz, c4),
            "{:?}",
            pitched.frequency_hz
        );
        assert_eq!(pitched.gain, None);
        let unpitched = first_trace(drum);
        assert_eq!(unpitched.frequency_hz, None);
        assert_eq!(unpitched.gain, None);
    }

    #[test]
    fn profiled_tick_preserves_scheduling_and_reports_exact_work_counts() {
        let clock = VirtualClock::new(0.0);
        let mut ordinary = Scheduler::new(Arc::new(Transport::default()), 1.0, 0.5);
        let mut profiled = Scheduler::new(Arc::new(Transport::default()), 1.0, 0.5);
        ordinary.set_pattern(atom("bd"), clock.now());
        profiled.set_pattern(atom("bd"), clock.now());

        assert_eq!(ordinary.tick(&clock), TickStatus::Filled);
        let mut profile = SchedulerTickProfile::default();
        assert_eq!(
            profiled.tick_profiled(&clock, &mut profile),
            TickStatus::Filled
        );
        assert_eq!(profile.callback_calls, 0);
        if let Some(sample) = profile.callback_kind_sample {
            assert_eq!(sample.total(), 0);
        }
        assert_eq!(profile.callback_nanos, 0);
        assert_eq!(profile.queried_haps, 1);
        assert_eq!(profile.accepted_events, 1);
        assert_eq!(profile.query_span_millicycles, 500);
        assert!(!profile.horizon_full);
        assert_eq!(
            ordinary.drain_through(&clock, 0.5),
            profiled.drain_through(&clock, 0.5)
        );

        assert_eq!(
            profiled.tick_profiled(&clock, &mut profile),
            TickStatus::HorizonFull
        );
        assert!(profile.horizon_full);
        assert_eq!(profile.queried_haps, 0);
        assert_eq!(profile.accepted_events, 0);
        assert_eq!(profile.query_nanos, 0);
        assert_eq!(profile.acceptance_nanos, 0);
    }

    #[test]
    fn schedule_trace_is_disabled_by_default() {
        let transport = Arc::new(Transport::default());
        let mut scheduler = Scheduler::new(transport, 1.0, 0.5);
        let clock = VirtualClock::new(0.0);
        scheduler.set_pattern(atom("bd"), clock.now());

        assert_eq!(scheduler.tick(&clock), TickStatus::Filled);
        assert!(!scheduler.trace_enabled());
        assert!(scheduler.take_trace_events().is_empty());
        assert_eq!(scheduler.trace_events_dropped(), 0);
        assert!(scheduler.queued() > 0, "tracing changed audio scheduling");
    }

    #[test]
    fn schedule_trace_copies_exact_accepted_hap_data() {
        let transport = Arc::new(Transport::default());
        let mut scheduler = Scheduler::new(transport, 1.0, 0.5);
        let clock = VirtualClock::new(0.0);
        scheduler.set_trace_enabled(true);
        let generation = scheduler.set_pattern(
            atom("bd")
                .map_haps_native(|hap| Some(hap.clone().with_context(vec![(4, 8), (13, 21)]))),
            clock.now(),
        );

        assert_eq!(scheduler.tick(&clock), TickStatus::Filled);
        let traces = scheduler.take_trace_events();
        assert_eq!(traces.len(), 1);
        assert_eq!(
            traces[0],
            ScheduleTraceEvent {
                onset_id: 0,
                generation,
                whole_begin: Fraction::ZERO,
                whole_end: Fraction::ONE,
                part_begin: Fraction::ZERO,
                part_end: Fraction::new(1, 2),
                duration: Fraction::ONE,
                target_time: 0.0,
                value_show: Some("bd".into()),
                color: None,
                label: None,
                active_label: None,
                scale: None,
                frequency_hz: None,
                gain: None,
                ui_visuals: 0,
                context: vec![(4, 8), (13, 21)],
            }
        );
        assert!(
            scheduler.take_trace_events().is_empty(),
            "taking a trace did not drain it"
        );
    }

    #[test]
    fn schedule_trace_keeps_typed_visual_text_apart_from_the_display_string() {
        let transport = Arc::new(Transport::default());
        let mut scheduler = Scheduler::new(transport, 1.0, 0.5);
        let clock = VirtualClock::new(0.0);
        scheduler.set_trace_enabled(true);
        scheduler.set_pattern(
            pure(Value::object([
                ("s".into(), Value::Str("sine".into())),
                ("color".into(), Value::Str("magenta".into())),
                ("label".into(), Value::Str("lead voice".into())),
            ])),
            clock.now(),
        );

        assert_eq!(scheduler.tick(&clock), TickStatus::Filled);
        let traces = scheduler.take_trace_events();
        assert_eq!(traces.len(), 1);
        assert_eq!(traces[0].color.as_deref(), Some("magenta"));
        assert_eq!(traces[0].label.as_deref(), Some("lead voice"));
        assert!(
            traces[0]
                .value_show
                .as_deref()
                .is_some_and(|value| value.contains("color:magenta"))
        );
    }

    #[test]
    fn due_sort_orders_finite_onsets_and_tolerates_non_finite_keys() {
        let transport = Arc::new(Transport::default());
        let mut scheduler = Scheduler::new(Arc::clone(&transport), 1.0, 0.5);
        let clock = VirtualClock::new(0.0);
        let generation = transport.generation();
        scheduler.last_fill_time = clock.now();
        scheduler.queue.extend([
            Event {
                onset_id: 2,
                generation,
                whole_begin: Fraction::ZERO,
                whole_end: Fraction::ONE,
                duration: Fraction::ONE,
                target_time: 0.50,
                value: Value::Str("late".into()),
                live_controls: [0; 2],
                ui_visuals: 0,
                log_line: None,
            },
            Event {
                onset_id: 1,
                generation,
                whole_begin: Fraction::ZERO,
                whole_end: Fraction::ONE,
                duration: Fraction::ONE,
                target_time: 0.25,
                value: Value::Str("early".into()),
                live_controls: [0; 2],
                ui_visuals: 0,
                log_line: None,
            },
        ]);
        let due = scheduler.drain_through(&clock, 1.0);
        assert_eq!(
            due.iter().map(|event| event.onset_id).collect::<Vec<_>>(),
            vec![1, 2]
        );

        // Defense in depth for the live drain path: a non-finite key must not
        // panic the producer even if one ever reaches the sort.
        let mut synthetic = due;
        synthetic.push(Event {
            onset_id: 3,
            generation,
            whole_begin: Fraction::ZERO,
            whole_end: Fraction::ONE,
            duration: Fraction::ONE,
            target_time: f64::NAN,
            value: Value::Str("nan".into()),
            live_controls: [0; 2],
            ui_visuals: 0,
            log_line: None,
        });
        synthetic.sort_by(|a, b| a.target_time.total_cmp(&b.target_time));
        assert_eq!(
            synthetic
                .iter()
                .map(|event| event.onset_id)
                .collect::<Vec<_>>(),
            vec![1, 2, 3]
        );
    }

    #[test]
    fn scheduler_events_keep_direct_live_slider_bindings() {
        let transport = Arc::new(Transport::default());
        let mut scheduler = Scheduler::new(transport, 1.0, 0.5);
        let clock = VirtualClock::new(0.0);
        let sound = pure(Value::object([("s".into(), Value::Str("sine".into()))]));
        let gain = rustel_core::controls::default_control_registry()
            .get("gain")
            .unwrap();
        let pattern = gain.apply(&sound, Some(pure(Value::F64(0.5)).with_slider_binding(73)));
        scheduler.set_pattern(pattern, clock.now());
        assert_eq!(scheduler.tick(&clock), TickStatus::Filled);
        let events = scheduler.drain_through(&clock, 1.0);
        assert!(!events.is_empty());
        assert!(events.iter().all(|event| event.live_controls == [73, 0]));
    }

    #[test]
    fn drain_filters_generations_and_preserves_the_future_queue_order() {
        let transport = Arc::new(Transport::default());
        let mut scheduler = Scheduler::new(Arc::clone(&transport), 1.0, 0.5);
        let clock = VirtualClock::new(0.0);
        let generation = transport.generation();
        let cancelled = generation + 1;
        scheduler.last_fill_time = clock.now();
        let event = |onset_id, generation, target_time, name: &str| Event {
            onset_id,
            generation,
            whole_begin: Fraction::ZERO,
            whole_end: Fraction::ONE,
            duration: Fraction::ONE,
            target_time,
            value: Value::object([
                ("s".into(), Value::Str(name.into())),
                ("gain".into(), Value::F64(0.5)),
            ]),
            live_controls: [0; 2],
            ui_visuals: 0,
            log_line: None,
        };
        scheduler.queue.extend([
            event(3, generation, 2.0, "future-first"),
            event(90, cancelled, 0.1, "cancelled-due"),
            event(2, generation, 0.75, "due-late"),
            event(4, generation, 1.5, "future-second"),
            event(91, cancelled, 9.0, "cancelled-future"),
            event(1, generation, 0.25, "due-early"),
        ]);

        let due = scheduler.drain_through(&clock, 1.0);
        assert_eq!(
            due.iter().map(|event| event.onset_id).collect::<Vec<_>>(),
            vec![1, 2],
            "due events did not retain target-time ordering"
        );
        assert_eq!(
            scheduler
                .queue
                .iter()
                .map(|event| event.onset_id)
                .collect::<Vec<_>>(),
            vec![3, 4],
            "extracting due and cancelled events reordered future work"
        );
        assert!(
            due.iter()
                .chain(&scheduler.queue)
                .all(|event| event.generation == generation),
            "a cancelled generation survived the drain boundary"
        );
    }

    #[test]
    fn schedule_trace_bounds_source_ranges_and_display_text_at_capture() {
        let transport = Arc::new(Transport::default());
        let mut scheduler = Scheduler::new(transport, 1.0, 0.5);
        let clock = VirtualClock::new(0.0);
        scheduler.set_trace_enabled(true);
        let context = (0..MAX_TRACE_CONTEXT_RANGES + 5)
            .map(|index| (index, index + 1))
            .collect::<Vec<_>>();
        let expected_context = context[..MAX_TRACE_CONTEXT_RANGES].to_vec();
        scheduler.set_pattern(
            atom(&"x".repeat(MAX_TRACE_TEXT_BYTES + 1))
                .map_haps_native(move |hap| Some(hap.clone().with_context(context.clone()))),
            clock.now(),
        );

        assert_eq!(scheduler.tick(&clock), TickStatus::Filled);
        let traces = scheduler.take_trace_events();
        assert_eq!(traces.len(), 1);
        assert_eq!(traces[0].value_show, None);
        assert_eq!(traces[0].context.len(), MAX_TRACE_CONTEXT_RANGES);
        assert_eq!(traces[0].context, expected_context);
    }

    #[test]
    fn schedule_trace_has_an_independent_cap_and_drop_count() {
        let transport = Arc::new(Transport::default());
        let mut scheduler = Scheduler::new(transport, 1.0, 0.5);
        let clock = VirtualClock::new(0.0);
        scheduler.set_trace_enabled(true);
        let overflow = 7;
        scheduler.set_pattern(
            stack(vec![atom("hit"); MAX_QUEUED_TRACE_EVENTS + overflow]),
            clock.now(),
        );

        assert_eq!(scheduler.tick(&clock), TickStatus::Filled);
        assert_eq!(scheduler.queued(), MAX_QUEUED_TRACE_EVENTS + overflow);
        assert_eq!(scheduler.trace_queued(), MAX_QUEUED_TRACE_EVENTS);
        assert_eq!(scheduler.take_trace_events().len(), MAX_QUEUED_TRACE_EVENTS);
        assert_eq!(scheduler.trace_events_dropped(), overflow as u64);
        assert_eq!(scheduler.trace_events_dropped_total(), overflow as u64);
        assert_eq!(scheduler.take_trace_events_dropped(), overflow as u64);
        assert_eq!(scheduler.trace_events_dropped(), 0);
        assert_eq!(scheduler.trace_events_dropped_total(), overflow as u64);
    }

    #[test]
    fn replacement_and_stop_discard_stale_trace_generations() {
        let transport = Arc::new(Transport::default());
        let mut scheduler = Scheduler::new(transport.clone(), 1.0, 0.5);
        let clock = VirtualClock::new(0.0);
        scheduler.set_trace_enabled(true);
        let old_generation = scheduler.set_pattern(atom("old"), clock.now());
        assert_eq!(scheduler.tick(&clock), TickStatus::Filled);
        assert!(
            scheduler
                .trace_events
                .iter()
                .any(|event| event.generation == old_generation)
        );

        let new_generation =
            scheduler.set_pattern(fastcat(vec![atom("new-a"), atom("new-b")]), 0.0);
        assert_ne!(new_generation, old_generation);
        assert!(
            scheduler.trace_events.is_empty(),
            "replacement retained an old generation's trace"
        );
        assert_eq!(scheduler.tick(&clock), TickStatus::Filled);
        let current = scheduler.take_trace_events();
        assert!(!current.is_empty());
        assert!(current.iter().all(|event| {
            event.generation == new_generation
                && event
                    .value_show
                    .as_deref()
                    .is_some_and(|value| value.starts_with("new"))
        }));

        scheduler.set_pattern(fastcat(vec![atom("stop"), atom("pending")]), 0.0);
        assert_eq!(scheduler.tick(&clock), TickStatus::Filled);
        assert!(!scheduler.trace_events.is_empty());
        transport.stop();
        assert!(
            scheduler.take_trace_events().is_empty(),
            "Stop exposed pending trace events"
        );
        assert_eq!(scheduler.trace_events_dropped(), 0);
    }

    #[test]
    fn active_requery_clears_prefetch_and_resumes_at_the_takeover() {
        let transport = Arc::new(Transport::default());
        let mut scheduler = Scheduler::new(transport, 1.0, 1.0);
        let clock = VirtualClock::new(0.0);
        scheduler.set_pattern(atom("bd"), clock.now());
        assert_eq!(scheduler.tick(&clock), TickStatus::Filled);
        assert!(scheduler.queued() > 0);
        let generation_before = scheduler.transport.generation();
        let cycle_before = scheduler.cycle_at_time(0.1);

        let generation_after = scheduler
            .requery_active(0.1, 0.2)
            .expect("active graph can be requeried");
        assert_eq!(generation_after, generation_before + 1);
        assert_eq!(scheduler.queued(), 0);
        assert!((scheduler.cycle_at_time(0.1) - cycle_before).abs() < 1e-9);
        assert!((scheduler.horizon_remaining(0.1) - 0.1).abs() < 1e-9);

        clock.set(0.1);
        assert_eq!(scheduler.tick(&clock), TickStatus::Filled);
        let events = scheduler.drain_through(&clock, 1.1);
        assert!(!events.is_empty());
        assert!(events.iter().all(|event| {
            event.generation == generation_after && event.target_time >= 0.2 - 1e-9
        }));
    }

    /// Four onsets a cycle at one cycle a second, queried through cycle 1.
    fn quarter_grid() -> (Scheduler, VirtualClock) {
        let mut scheduler = Scheduler::new(Arc::new(Transport::default()), 1.0, 1.0);
        let clock = VirtualClock::new(0.0);
        scheduler.set_pattern(
            fastcat(vec![atom("a"), atom("b"), atom("c"), atom("d")]),
            clock.now(),
        );
        assert_eq!(scheduler.tick(&clock), TickStatus::Filled);
        (scheduler, clock)
    }

    fn requeried_begins(scheduler: &mut Scheduler, clock: &VirtualClock, now: f64) -> Vec<f64> {
        clock.set(now);
        assert_eq!(scheduler.tick(clock), TickStatus::Filled);
        scheduler
            .drain_through(clock, now + 1.0)
            .iter()
            .map(|event| event.whole_begin.to_f64())
            .collect()
    }

    /// A requery from a takeover edge starts past an onset just before the
    /// edge: the device keeps that onset, and a cursor on it plays it twice.
    /// The mapping does not move, so the later onsets keep their exact times.
    #[test]
    fn a_requery_from_an_edge_starts_past_the_onset_just_before_it() {
        let (mut scheduler, clock) = quarter_grid();
        let edge = 0.5 + 1e-6 / 48_000.0;
        assert_eq!(
            Fraction::from_f64(scheduler.cycle_at_time(edge)),
            Some(Fraction::new(1, 2)),
            "test premise: the edge's cycle approximates to the onset before it"
        );
        let generation_before = scheduler.transport.generation();

        let generation = scheduler
            .requery_active_from(0.3, edge)
            .expect("active graph can be requeried");
        assert_eq!(generation, generation_before + 1);
        assert_eq!(scheduler.queued(), 0);
        assert!(scheduler.queried_to > Fraction::new(1, 2));
        assert!(scheduler.time_at_cycle(scheduler.queried_to) >= edge);
        assert!(scheduler.scheduled_to_cycle() - 0.5 < 1e-6);
        assert_eq!(
            (scheduler.anchor_time, scheduler.anchor_cycle),
            (0.0, Fraction::ZERO),
            "the mapping stays on its anchor"
        );

        assert_eq!(
            requeried_begins(&mut scheduler, &clock, 0.3),
            [0.75, 1.0, 1.25],
            "the onset at 0.5 is the device's copy to play"
        );
    }

    /// An onset at or after the edge belongs to the new generation. The
    /// onset at 0.5 is 0.3 frame after this edge: the device puts it on the
    /// takeover frame and drops the outgoing copy.
    #[test]
    fn a_requery_from_an_edge_emits_the_onset_just_after_it() {
        let (mut scheduler, clock) = quarter_grid();
        let edge = 0.5 - 0.3 / 48_000.0;
        scheduler
            .requery_active_from(0.3, edge)
            .expect("active graph can be requeried");
        assert_eq!(
            requeried_begins(&mut scheduler, &clock, 0.3),
            [0.5, 0.75, 1.0, 1.25]
        );
    }

    /// An edge behind the clock does not move the cursor back before `now`,
    /// an explicit start still bounds it, and an edge that is not a time
    /// requeries nothing.
    #[test]
    fn a_requery_from_an_edge_keeps_the_cursor_bounds() {
        let (mut scheduler, clock) = quarter_grid();
        scheduler
            .requery_active_from(0.6, 0.1)
            .expect("active graph can be requeried");
        assert_eq!(scheduler.scheduled_to_cycle(), 0.6);
        assert_eq!(
            requeried_begins(&mut scheduler, &clock, 0.6),
            [0.75, 1.0, 1.25, 1.5]
        );

        let generation = scheduler.transport.generation();
        assert_eq!(scheduler.requery_active_from(0.6, f64::NAN), None);
        assert_eq!(scheduler.requery_active_from(0.6, f64::INFINITY), None);
        assert_eq!(scheduler.requery_active_from(f64::NAN, 0.7), None);
        assert_eq!(scheduler.transport.generation(), generation);

        let (mut scheduler, _clock) = quarter_grid();
        scheduler.rebase_start_anchor(2.0);
        scheduler
            .requery_active_from(1.0, 1.5)
            .expect("a preroll requery");
        assert_eq!(
            scheduler.scheduled_to_cycle(),
            0.0,
            "nothing before the start is queried"
        );
        assert_eq!(scheduler.time_at_cycle(Fraction::ZERO), 2.0);
    }

    /// An outside clock puts the transport 0.3 cycle back. The requery
    /// plays the cycles from its cursor again. The outgoing copy of the
    /// onset at 0.5 is before the edge, but its cycle named another moment.
    #[test]
    fn a_requery_after_a_retime_plays_the_cycles_from_its_cursor_again() {
        let (mut scheduler, clock) = quarter_grid();
        scheduler.retime(0.6, 1.0, 0.3);
        scheduler
            .requery_active_from(0.6, 0.7)
            .expect("active graph can be requeried");
        assert_eq!(scheduler.scheduled_to_cycle(), 0.4);
        assert!(scheduler.overlap_held.is_empty());
        assert!(scheduler.superseded.is_empty());
        assert_eq!(
            requeried_begins(&mut scheduler, &clock, 0.6),
            [0.5, 0.75, 1.0, 1.25]
        );
    }

    /// A continued replacement after a retime, with no requery between
    /// them, reads its window on the new mapping. There the cycle 0.75 is
    /// 0.2 s after the edge, so the replacement plays it.
    #[test]
    fn a_continued_replacement_after_a_retime_reads_the_new_mapping() {
        let (mut scheduler, clock) = quarter_grid();
        scheduler.retime(0.6, 1.0, 0.3);
        let grid = fastcat(vec![atom("a"), atom("b"), atom("c"), atom("d")]);
        let _ = scheduler.replace_pattern_continued_until(grid, 0.6, None, 0.3, 0.85, None);
        assert!(scheduler.superseded.is_empty());
        assert_eq!(
            requeried_begins(&mut scheduler, &clock, 0.6),
            [0.75, 1.0, 1.25]
        );
    }

    /// Two copies with one key leave one mark, so one copy is held for it.
    #[test]
    fn two_copies_with_one_key_hold_one_stand_in() {
        let (mut scheduler, _clock) = quarter_grid();
        let copy = |generation| EmittedKey {
            generation,
            begin: Fraction::new(1, 2),
            end: Fraction::new(3, 4),
            value: Arc::from("c"),
            occurrence: 0,
        };
        scheduler.overlap_held.clear();
        scheduler.carry_device_copies(
            9,
            vec![(copy(7), 0.5), (copy(8), 0.5)],
            Fraction::ZERO,
            false,
        );
        assert_eq!(scheduler.overlap_held[&Fraction::new(1, 2)].len(), 1);
        assert!(scheduler.emitted.contains_key(&copy(9)));
    }

    /// The takeover of a generation drops copies from the device only when
    /// the producer publishes it, after its first tick. Until then a new
    /// takeover gets them back.
    #[test]
    fn a_takeover_sets_the_dropped_copies_aside_until_the_first_tick() {
        let (mut scheduler, clock) = quarter_grid();
        let begins = |copies: &[DeviceCopy]| {
            let mut begins: Vec<f64> = copies.iter().map(|(key, _)| key.begin.to_f64()).collect();
            begins.sort_by(f64::total_cmp);
            begins
        };
        scheduler
            .requery_active_from(0.3, 0.55)
            .expect("active graph can be requeried");
        assert_eq!(begins(&scheduler.superseded), [0.75]);
        assert_eq!(scheduler.emitted.len(), 1, "the onset at 0.5 stays marked");

        scheduler
            .requery_active_from(0.35, 0.8)
            .expect("active graph can be requeried");
        assert!(scheduler.superseded.is_empty());
        let mut marked: Vec<(f64, f64)> = scheduler
            .emitted
            .iter()
            .map(|(key, time)| (key.begin.to_f64(), *time))
            .collect();
        marked.sort_by(|a, b| a.partial_cmp(b).expect("finite times"));
        assert_eq!(marked, [(0.5, 0.5), (0.75, 0.75)]);

        scheduler
            .requery_active_from(0.4, 0.55)
            .expect("active graph can be requeried");
        assert_eq!(begins(&scheduler.superseded), [0.75]);
        clock.set(0.4);
        assert_eq!(scheduler.tick(&clock), TickStatus::Filled);
        assert!(scheduler.superseded.is_empty());

        // A first tick that finds the horizon full is published too.
        clock.set(0.51);
        assert_eq!(scheduler.tick(&clock), TickStatus::Filled);
        scheduler
            .requery_active_from(0.51, 1.49)
            .expect("active graph can be requeried");
        assert_eq!(begins(&scheduler.superseded), [1.5]);
        assert_eq!(scheduler.tick(&clock), TickStatus::HorizonFull);
        assert!(scheduler.superseded.is_empty());
    }

    /// A rebase from a takeover edge re-anchors and starts the cursor on the
    /// edge: past an onset just before it, not past an onset just after it,
    /// and not behind the anchor. An edge that is not a time changes nothing.
    #[test]
    fn a_rebase_from_an_edge_starts_the_cursor_on_it() {
        let (mut scheduler, _clock) = quarter_grid();
        scheduler.rebase_anchor_from(0.3, 0.3, 0.5 + 1e-6 / 48_000.0);
        assert_eq!(
            (scheduler.anchor_time, scheduler.anchor_cycle),
            (0.3, Fraction::new(3, 10))
        );
        assert!(scheduler.queried_to > Fraction::new(1, 2));
        assert!(scheduler.scheduled_to_cycle() - 0.5 < 1e-6);

        scheduler.rebase_anchor_from(0.3, 0.3, 0.5 - 0.3 / 48_000.0);
        assert!(scheduler.queried_to <= Fraction::new(1, 2));
        assert!(0.5 - scheduler.scheduled_to_cycle() < 0.3 / 48_000.0);

        scheduler.rebase_anchor_from(0.6, 0.6, 0.1);
        assert_eq!(scheduler.scheduled_to_cycle(), 0.6);

        let before = (
            scheduler.anchor_time,
            scheduler.anchor_cycle,
            scheduler.queried_to,
        );
        scheduler.rebase_anchor_from(0.7, 0.7, f64::NAN);
        scheduler.rebase_anchor_from(0.7, f64::NAN, 0.9);
        assert_eq!(
            (
                scheduler.anchor_time,
                scheduler.anchor_cycle,
                scheduler.queried_to
            ),
            before
        );
    }

    /// A rewind's first window held for a loading sample is reopened, not
    /// re-queried under a new generation: the cursor goes back, the rest of
    /// the consumed pass is dropped from the queue, and only the CURRENT
    /// generation's emission marks at or after the cursor are forgotten.
    /// The same onsets then come back exactly once; marks before the cursor
    /// and marks another generation left keep their meaning.
    #[test]
    fn reopening_a_consumed_window_re_emits_it_once_for_this_generation() {
        let transport = Arc::new(Transport::default());
        let mut scheduler = Scheduler::new(transport, 1.0, 0.5);
        let clock = VirtualClock::new(0.0);
        let generation = scheduler.set_pattern(
            fastcat(vec![atom("a"), atom("b"), atom("c"), atom("d")]),
            clock.now(),
        );
        assert_eq!(scheduler.tick(&clock), TickStatus::Filled);
        assert_eq!(scheduler.queued(), 2, "a and b fill the half-cycle horizon");
        let delivered = scheduler.drain_through(&clock, 0.1);
        assert_eq!(delivered.len(), 1, "a was taken before the reopen");
        let elsewhere = EmittedKey {
            generation: generation - 1,
            begin: Fraction::new(1, 4),
            end: Fraction::new(1, 2),
            value: Arc::from("b"),
            occurrence: 0,
        };
        scheduler.emitted.insert(elsewhere.clone(), 0.25);
        let marks_of = |scheduler: &Scheduler| {
            let mut begins: Vec<f64> = scheduler
                .emitted
                .keys()
                .filter(|key| key.generation == generation)
                .map(|key| key.begin.to_f64())
                .collect();
            begins.sort_by(f64::total_cmp);
            begins
        };
        assert_eq!(marks_of(&scheduler), [0.0, 0.25]);

        // Refused: a cursor not behind the queried frontier, or not a time.
        assert!(!scheduler.reopen_window_at(0.0, 5.0));
        assert!(!scheduler.reopen_window_at(f64::NAN, 0.0));
        assert!(!scheduler.reopen_window_at(0.0, f64::INFINITY));
        assert_eq!(scheduler.scheduled_to_cycle(), 0.5);
        assert_eq!(scheduler.queued(), 1, "a refused reopen touches nothing");

        // A reopen inside the window keeps what came before its cursor.
        assert!(scheduler.reopen_window_at(0.0, 0.25));
        assert_eq!(scheduler.queued(), 0);
        assert_eq!(scheduler.scheduled_to_cycle(), 0.25);
        assert_eq!(marks_of(&scheduler), [0.0]);
        assert!(scheduler.emitted.contains_key(&elsewhere));
        assert_eq!(scheduler.tick(&clock), TickStatus::Filled);
        let again = scheduler.drain_through(&clock, 0.5);
        assert_eq!(
            again
                .iter()
                .map(|event| (event.generation, event.target_time))
                .collect::<Vec<_>>(),
            [(generation, 0.25)],
            "b comes back once; a, still marked, does not double"
        );

        // A reopen at the window's start re-emits the whole window once.
        assert!(scheduler.reopen_window_at(0.0, 0.0));
        assert_eq!(scheduler.scheduled_to_cycle(), 0.0);
        assert!(marks_of(&scheduler).is_empty());
        assert!(scheduler.emitted.contains_key(&elsewhere));
        assert_eq!(scheduler.tick(&clock), TickStatus::Filled);
        let whole = scheduler.drain_through(&clock, 0.5);
        assert_eq!(
            whole
                .iter()
                .map(|event| (event.generation, event.target_time))
                .collect::<Vec<_>>(),
            [(generation, 0.0), (generation, 0.25)]
        );
        assert_ne!(scheduler.tick(&clock), TickStatus::Filled);
        assert!(
            scheduler.drain_through(&clock, 0.5).is_empty(),
            "nothing emits twice"
        );
    }

    #[test]
    fn emitted_history_stays_bounded_over_a_long_session() {
        let transport = Arc::new(Transport::default());
        let mut scheduler = Scheduler::new(transport, 1.0, 0.5);
        let clock = VirtualClock::new(0.0);
        let dense = stack(vec![
            fastcat((0..16).map(|i| atom(&format!("a{i}"))).collect()),
            fastcat((0..16).map(|i| atom(&format!("b{i}"))).collect()),
        ]);
        scheduler.set_pattern(dense, clock.now());

        let mut delivered = 0usize;
        for _ in 0..8_000 {
            scheduler.tick(&clock);
            delivered += scheduler.drain_due(&clock).len();
            clock.advance(0.05);
        }

        assert!(delivered > 10_000, "the session barely played: {delivered}");
        assert!(
            scheduler.emitted.len() <= EMITTED_PRUNE_THRESHOLD,
            "emitted history grew to {} over {delivered} onsets",
            scheduler.emitted.len()
        );
    }
}
