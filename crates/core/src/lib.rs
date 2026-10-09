//! Pattern engine core: `TimeSpan`, `Hap`, `State`, `Pattern` and the joins.
//!
//! A `Pattern` is a pure function from a `State` to the haps active during its
//! span. Query semantics track the Strudel language exactly, including its
//! observable error behaviour.

pub mod callback_ir;
pub mod combinators;
pub mod compose;
pub mod controls;
pub mod gamepad;
pub mod midi_in;
pub mod midimap;
// Keep the public module path stable.
#[path = "control_catalog.rs"]
#[rustfmt::skip]
pub mod controls_generated;
pub mod euclid;
pub mod extension_node;
pub mod fdlibm;
pub mod host_value;
pub mod ops;
pub mod purity;
pub mod reference;
pub mod register;
pub mod rng;
pub mod settings;
pub mod signal;
pub mod tonal;
pub mod tonaljs;
pub mod tonaljs_scales;
pub mod tune;
pub mod util;
pub mod value;
pub mod voicings;
pub mod xen;

use rustel_fraction::Fraction;
use std::collections::BTreeMap;
use std::fmt;
use std::sync::{Arc, Mutex};
pub use value::{OrderedMap, Value};

// ---------------------------------------------------------------------------
// TimeSpan
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct TimeSpan {
    pub begin: Fraction,
    pub end: Fraction,
}

impl TimeSpan {
    pub fn new(begin: Fraction, end: Fraction) -> Self {
        Self { begin, end }
    }

    /// Splits the span at cycle boundaries.
    pub fn span_cycles(&self) -> Vec<TimeSpan> {
        let mut spans = Vec::new();
        let mut begin = self.begin;
        let end = self.end;
        let end_sam = end.sam();

        // Zero-width spans are supported and yield exactly themselves.
        if begin == end {
            return vec![TimeSpan::new(begin, end)];
        }

        // This loop pushes one entry per cycle, so its allocation is driven by
        // user-controlled span width. `fast(1e30)` would otherwise request a
        // vector with roughly 10^30 entries; no observable result exists for
        // that input, so the runtime refuses it.
        //
        // The failure travels on the typed resource-refusal channel so a
        // caller cannot mistake an uncomputed oversized query for silence.
        if self.cycle_count_exceeds(MAX_QUERY_SPAN_CYCLES) {
            refuse(QueryLimit::QuerySpan {
                cycles: MAX_QUERY_SPAN_CYCLES,
            });
            return Vec::new();
        }

        while end > begin {
            if begin.sam() == end_sam {
                spans.push(TimeSpan::new(begin, self.end));
                break;
            }
            let next_begin = begin.next_sam();
            spans.push(TimeSpan::new(begin, next_begin));
            begin = next_begin;
        }
        spans
    }

    /// Would `span_cycles` produce more than `limit` entries?
    ///
    /// Computed from the endpoints, never by iterating - iteration is the
    /// thing being guarded against.
    fn cycle_count_exceeds(&self, limit: i128) -> bool {
        if self.end <= self.begin {
            return false;
        }
        // `span_cycles` emits one entry for every integer cycle bucket the
        // half-open span touches. `ceil(end) - floor(begin)` is that count
        // exactly; using floor(width) allowed one extra allocation whenever
        // either edge was fractional.
        let first_cycle = self.begin.floor().numer();
        let after_last_cycle = self.end.ceil().numer();
        after_last_cycle
            .checked_sub(first_cycle)
            .is_none_or(|count| count > limit)
    }

    pub fn duration(&self) -> Fraction {
        self.end.sub(self.begin)
    }

    /// Shift to an equal-duration span whose begin is in cycle zero.
    pub fn cycle_arc(&self) -> TimeSpan {
        let begin = self.begin.cycle_pos();
        TimeSpan::new(begin, begin.add(self.duration()))
    }

    pub fn with_time(&self, f: impl Fn(Fraction) -> Fraction) -> TimeSpan {
        TimeSpan::new(f(self.begin), f(self.end))
    }

    pub fn with_end(&self, f: impl Fn(Fraction) -> Fraction) -> TimeSpan {
        TimeSpan::new(self.begin, f(self.end))
    }

    pub fn with_cycle(&self, f: impl Fn(Fraction) -> Fraction) -> TimeSpan {
        let sam = self.begin.sam();
        TimeSpan::new(
            sam.add(f(self.begin.sub(sam))),
            sam.add(f(self.end.sub(sam))),
        )
    }

    /// Returns `None` when the spans do not intersect.
    ///
    /// Zero-width rule (the classic off-by-one source): a point intersection
    /// does NOT count if it sits at the end of a non-zero-width span.
    pub fn intersection(&self, other: &TimeSpan) -> Option<TimeSpan> {
        let b = self.begin.max(other.begin);
        let e = self.end.min(other.end);
        if b > e {
            return None;
        }
        if b == e {
            if b == self.end && self.begin < self.end {
                return None;
            }
            if b == other.end && other.begin < other.end {
                return None;
            }
        }
        Some(TimeSpan::new(b, e))
    }

    pub fn midpoint(&self) -> Fraction {
        self.begin.add(self.duration().div(Fraction::int(2)))
    }

    pub fn show(&self) -> String {
        format!("{} → {}", self.begin.show(), self.end.show())
    }
}

/// Largest number of cycle splits `span_cycles` will materialise.
///
/// Real queries span a handful of cycles; the offline renderer's longest
/// documented run is far below this. The bound exists so a hostile or mistaken
/// factor fails fast instead of exhausting memory.
pub const MAX_QUERY_SPAN_CYCLES: i128 = 1_000_000;

/// How deep `Pattern::query` may recurse before refusing.
///
/// The limit is sized for unoptimised stack frames, which are substantially
/// larger than release frames. A value of 512 leaves conservative headroom on
/// the CLI's 64 MiB worker stack and still permits roughly 128 chained
/// operations because a combinator such as `fast` creates multiple nodes.
pub const MAX_PATTERN_DEPTH: u32 = 512;

/// `sortHapsByPart()`, including its throw on analog haps.
///
/// Shared by every entry point that claims this ordering, so a second path
/// cannot quietly use a more tolerant comparator. See
/// [`Pattern::query_arc_sorted`] for why the throw is preserved.
pub fn sort_haps_by_part(mut haps: Vec<Hap>) -> Vec<Hap> {
    // One element: `Array.prototype.sort` never calls the comparator, so an
    // analog hap survives.
    if haps.len() < 2 {
        return haps;
    }
    if haps.iter().any(|hap| hap.whole.is_none()) {
        signal_query_error(|| {
            "sortHapsByPart: Cannot read properties of undefined (reading 'begin')".into()
        });
        return Vec::new();
    }
    haps.sort_by(|a, b| {
        // Safe: the analog case returned above.
        let (aw, bw) = (a.whole.expect("discrete"), b.whole.expect("discrete"));
        a.part
            .begin
            .cmp(&b.part.begin)
            .then_with(|| a.part.end.cmp(&b.part.end))
            .then_with(|| aw.begin.cmp(&bw.begin))
            .then_with(|| aw.end.cmp(&bw.end))
    });
    haps
}

/// Sort host results without discarding continuous haps that have no whole span.
/// Explicit `sortHapsByPart()` calls keep the comparator above.
pub fn sort_haps_for_query(mut haps: Vec<Hap>) -> Vec<Hap> {
    haps.sort_by(|a, b| {
        a.part
            .begin
            .cmp(&b.part.begin)
            .then_with(|| a.part.end.cmp(&b.part.end))
            .then_with(|| {
                a.whole
                    .map(|whole| (whole.begin, whole.end))
                    .cmp(&b.whole.map(|whole| (whole.begin, whole.end)))
            })
    });
    haps
}

/// The whole cycle containing `t`.
pub fn whole_cycle(t: Fraction) -> TimeSpan {
    TimeSpan::new(t.sam(), t.next_sam())
}

// ---------------------------------------------------------------------------
// Hap
// ---------------------------------------------------------------------------

#[derive(Clone)]
pub struct Hap {
    /// `None` means analog/continuous: the value was sampled at the part's
    /// midpoint and has no notional extent.
    pub whole: Option<TimeSpan>,
    pub part: TimeSpan,
    pub value: Value,
    /// Source spans, concatenated on every bind. Drives editor highlighting,
    /// so it must survive every combinator that carries context.
    pub context: Vec<(usize, usize)>,
    /// Direct live slider bindings: gain, then low-pass cutoff. Zero is unbound.
    /// These are host tokens, never score values or source-location guesses.
    pub live_controls: [u64; 2],
    slider_binding: u64,
    ui_visuals: u64,
    /// Exact JavaScript lookup shape (holes, length, enumerable keys) when the
    /// visible value alone cannot represent it. Nested Patterns also live in
    /// `value`; this side channel is shape metadata, not their only owner.
    pick_lookup: Option<Arc<PickLookup>>,
    /// `hap.context.scale`: set by `.scale()`, read by `.scaleTranspose()`.
    /// Context merges are extending-side-wins, which `Option::or` mirrors at
    /// each combine site.
    scale: Option<Arc<str>>,
    /// `hap.context.edoSize`: set by xen's `.xen()`/`.edo()`, read by
    /// `.ftrans()`. Same merge contract as `scale`.
    edo_size: Option<f64>,
    scale_definition: Option<Arc<Value>>,
    /// `hap.context.tags`: appended by `.tag()`, read by `hap.hasTag()` inside
    /// a `filter`. Same merge rule as `scale`. Shared rather than cloned: a
    /// tag list is copied at every combine site and never mutated in place.
    tags: Option<Arc<[Arc<str>]>>,
    /// `hap.context.logLine`: the text `.log()` will print when this hap
    /// TRIGGERS. Same merge rule as `scale`.
    ///
    /// It rides the context rather than the value because a value is what a
    /// score means: `"0 1".log()` carries a bare number, and adding a key to
    /// it would make the pattern mean something else. The formatter runs when
    /// the hap is produced; only the printing waits for the onset.
    log_line: Option<Arc<str>>,
}

/// Read a `tags` key off a JS context object: an array of strings, anything
/// else ignored. A score that somehow writes a non-array there loses the tag,
/// not the hap.
fn tags_from_value(value: &Value) -> Option<Arc<[Arc<str>]>> {
    let Value::List(items) = value else {
        return None;
    };
    let tags: Vec<Arc<str>> = items
        .iter()
        .filter_map(|item| match item {
            Value::Str(text) => Some(Arc::from(text.as_str())),
            _ => None,
        })
        .collect();
    (!tags.is_empty()).then(|| Arc::from(tags))
}

impl Hap {
    pub fn new(whole: Option<TimeSpan>, part: TimeSpan, value: Value) -> Self {
        Self {
            whole,
            part,
            value,
            context: Vec::new(),
            live_controls: [0; 2],
            slider_binding: 0,
            ui_visuals: 0,
            pick_lookup: None,
            scale: None,
            tags: None,
            log_line: None,
            edo_size: None,
            scale_definition: None,
        }
    }

    fn with_pick_lookup(mut self, lookup: Arc<PickLookup>) -> Self {
        self.pick_lookup = Some(lookup);
        self
    }

    /// `hap.context.scale`, exactly as `.scale()` tagged it.
    pub fn scale_context(&self) -> Option<&str> {
        self.scale.as_deref()
    }

    /// Set the scale context tag.
    pub fn with_scale_context(mut self, scale: Arc<str>) -> Self {
        self.scale = Some(scale);
        self
    }

    /// `hap.context.edoSize`, exactly as `.xen()` tagged it.
    pub fn edo_size_context(&self) -> Option<f64> {
        self.edo_size
    }

    /// Every tag this hap carries, oldest first.
    pub fn tags_context(&self) -> Option<&[Arc<str>]> {
        self.tags.as_deref()
    }

    /// The text `.log()` will print when this hap triggers, if any.
    pub fn log_line(&self) -> Option<&str> {
        self.log_line.as_deref()
    }

    /// `hap.hasTag(t)` - what a `filter` callback asks.
    pub fn has_tag(&self, tag: &str) -> bool {
        self.tags
            .as_deref()
            .is_some_and(|tags| tags.iter().any(|held| &**held == tag))
    }

    /// Append a tag - never replaces, so tagging twice keeps both.
    pub fn with_tag_context(mut self, tag: Arc<str>) -> Self {
        let mut tags: Vec<Arc<str>> = self.tags.as_deref().unwrap_or_default().to_vec();
        tags.push(tag);
        self.tags = Some(Arc::from(tags));
        self
    }

    /// `.log()`'s already-formatted text, to be printed when this hap
    /// triggers.
    pub fn with_log_line_context(mut self, line: impl Into<Arc<str>>) -> Self {
        self.log_line = Some(line.into());
        self
    }

    pub fn with_edo_size_context(mut self, edo_size: f64) -> Self {
        self.edo_size = Some(edo_size);
        self
    }

    /// `hap.context.scaleDefinition`, as `edoScale` tagged it. Stored behind
    /// an `Arc`; callback-visible JSON matches the raw argument object.
    pub fn scale_definition_context(&self) -> Option<&Value> {
        self.scale_definition.as_deref()
    }

    /// Set the scale-definition context tag.
    pub fn with_scale_definition_context(mut self, definition: Arc<Value>) -> Self {
        self.scale_definition = Some(definition);
        self
    }

    pub fn with_context(mut self, ctx: Vec<(usize, usize)>) -> Self {
        self.context = ctx;
        self
    }

    pub fn ui_visuals_context(&self) -> u64 {
        self.ui_visuals
    }

    pub fn with_ui_visual_slot(mut self, slot: u8) -> Self {
        if slot < 64 {
            self.ui_visuals |= 1_u64 << slot;
        }
        self
    }

    fn with_ui_visuals_context(mut self, visuals: u64) -> Self {
        self.ui_visuals = visuals;
        self
    }

    pub fn pick_lookup(&self) -> Option<&PickLookup> {
        self.pick_lookup.as_deref()
    }

    pub fn whole_or_part(&self) -> TimeSpan {
        self.whole.unwrap_or(self.part)
    }

    /// Effective duration, honoring the `duration` and `clip` controls. A clip
    /// whose product with the duration leaves the native fraction range is
    /// ignored, and the un-clipped duration is returned.
    pub fn duration(&self) -> Fraction {
        let mut duration = self
            .value
            .get("duration")
            .and_then(Value::as_f64)
            .and_then(Fraction::from_f64)
            .or_else(|| self.whole.map(|whole| whole.duration()))
            .unwrap_or_else(|| self.part.duration());
        if let Some(clip) = self
            .value
            .get("clip")
            .and_then(Value::as_f64)
            .and_then(Fraction::from_f64)
        {
            duration = duration.checked_mul(clip).unwrap_or(duration);
        }
        duration
    }

    pub fn end_clipped(&self) -> Fraction {
        self.whole_or_part().begin.add(self.duration())
    }

    pub fn is_active(&self, current: Fraction) -> bool {
        let whole = self.whole_or_part();
        whole.begin <= current && self.end_clipped() >= current
    }

    pub fn is_in_past(&self, current: Fraction) -> bool {
        current > self.end_clipped()
    }

    pub fn is_in_near_past(&self, margin: Fraction, current: Fraction) -> bool {
        current.sub(margin) <= self.end_clipped()
    }

    pub fn is_in_future(&self, current: Fraction) -> bool {
        current < self.whole_or_part().begin
    }

    pub fn is_in_near_future(&self, margin: Fraction, current: Fraction) -> bool {
        let begin = self.whole_or_part().begin;
        current < begin && current > begin.sub(margin)
    }

    pub fn is_within_time(&self, min: Fraction, max: Fraction) -> bool {
        self.whole_or_part().begin <= max && self.end_clipped() >= min
    }

    /// Whether the whole begins exactly where the part does.
    pub fn has_onset(&self) -> bool {
        self.whole
            .map(|w| w.begin == self.part.begin)
            .unwrap_or(false)
    }

    pub fn with_span(&self, f: impl Fn(&TimeSpan) -> TimeSpan) -> Hap {
        Hap {
            whole: self.whole.as_ref().map(&f),
            part: f(&self.part),
            value: self.value.clone(),
            context: self.context.clone(),
            ui_visuals: self.ui_visuals,
            live_controls: self.live_controls,
            slider_binding: self.slider_binding,
            pick_lookup: self.pick_lookup.clone(),
            scale: self.scale.clone(),
            tags: self.tags.clone(),
            log_line: self.log_line.clone(),
            edo_size: self.edo_size,
            scale_definition: self.scale_definition.clone(),
        }
    }

    /// Replace only the visible part of a hap, retaining its notional whole.
    ///
    /// Native extension nodes use this for the same intersected-child shape
    /// as JavaScript's `new Hap(whole, part, value, context)` constructor.
    pub fn with_part(mut self, part: TimeSpan) -> Hap {
        self.part = part;
        self
    }

    /// JavaScript `hap.combineContext(other)`, retaining this hap's spans and
    /// value while merging the other hap's contextual metadata.
    pub fn combine_context(&self, other: &Hap) -> Hap {
        let mut context = self.context.clone();
        context.extend_from_slice(&other.context);
        Hap {
            whole: self.whole,
            part: self.part,
            value: self.value.clone(),
            context,
            ui_visuals: self.ui_visuals | other.ui_visuals,
            live_controls: self.live_controls,
            slider_binding: self.slider_binding,
            pick_lookup: self.pick_lookup.clone(),
            scale: other.scale.clone().or_else(|| self.scale.clone()),
            tags: other.tags.clone().or_else(|| self.tags.clone()),
            log_line: other.log_line.clone().or_else(|| self.log_line.clone()),
            edo_size: other.edo_size.or(self.edo_size),
            scale_definition: other
                .scale_definition
                .clone()
                .or_else(|| self.scale_definition.clone()),
        }
    }

    pub fn with_value(&self, f: impl Fn(&Value) -> Value) -> Hap {
        let value = f(&self.value);
        Hap {
            whole: self.whole,
            part: self.part,
            pick_lookup: self
                .pick_lookup
                .as_ref()
                .filter(|_| value == self.value)
                .cloned(),
            value,
            context: self.context.clone(),
            live_controls: [0; 2],
            slider_binding: 0,
            ui_visuals: self.ui_visuals,
            scale: self.scale.clone(),
            tags: self.tags.clone(),
            log_line: self.log_line.clone(),
            edo_size: self.edo_size,
            scale_definition: self.scale_definition.clone(),
        }
    }

    fn without_live_controls(mut self) -> Self {
        self.live_controls = [0; 2];
        self.slider_binding = 0;
        self
    }

    /// Trusted pitch rewrites can carry named audio controls unchanged.
    /// A raw slider value still loses its identity when its pitch changes.
    fn with_pitch_live_controls(mut self, input: &Hap) -> Self {
        self.live_controls = [0; 2];
        self.slider_binding = 0;
        for (index, key) in ["gain", "cutoff"].into_iter().enumerate() {
            if input.live_controls[index] != 0
                && let Some(before) = input.value.get(key).and_then(Value::as_f64)
                && self.value.get(key).and_then(Value::as_f64) == Some(before)
            {
                self.live_controls[index] = input.live_controls[index];
            }
        }
        self
    }

    /// `Hap.show(true)` - the compact snapshot serialiser.
    ///
    /// Sigils: `⇜` whole starts before part · `(…)` fragment · `⇝` whole ends
    /// after part · `~` analog.
    ///
    pub fn show(&self) -> String {
        self.show_with(true)
    }

    pub fn show_with(&self, compact: bool) -> String {
        let value = match &self.value {
            Value::List(_) | Value::Object(_) => {
                if compact {
                    self.value.compact_json()
                } else {
                    self.value
                        .json_stringify()
                        .unwrap_or_else(|| "undefined".into())
                }
            }
            value => value.show(),
        };
        let spans = match self.whole {
            // Compatibility quirk: strudel.cc stringifies the show method
            // itself for analog haps (a missing `()`), and the exact text is
            // observable - so it is reproduced verbatim.
            None => "~show() {\n    return this.begin.show() + ' → ' + this.end.show();\n  }"
                .to_string(),
            Some(w) => {
                let is_whole = w.begin == self.part.begin && w.end == self.part.end;
                let mut s = String::new();
                if w.begin != self.part.begin {
                    s.push_str(&format!("{} ⇜ ", w.begin.show()));
                }
                if !is_whole {
                    s.push('(');
                }
                s.push_str(&self.part.show());
                if !is_whole {
                    s.push(')');
                }
                if w.end != self.part.end {
                    s.push_str(&format!(" ⇝ {}", w.end.show()));
                }
                s
            }
        };
        format!("[ {spans} | {value} ]")
    }

    pub fn show_whole(&self, compact: bool) -> String {
        let span = self
            .whole
            .map(|whole| whole.show())
            .unwrap_or_else(|| "~".into());
        let value = match &self.value {
            Value::List(_) | Value::Object(_) if compact => self.value.compact_json(),
            Value::List(_) | Value::Object(_) => self
                .value
                .json_stringify()
                .unwrap_or_else(|| "undefined".into()),
            value => value.show(),
        };
        format!("{span}: {value}")
    }
}

impl fmt::Debug for Hap {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Hap")
            .field("whole", &self.whole)
            .field("part", &self.part)
            .field("value", &self.value)
            .field("context", &self.context)
            .finish_non_exhaustive()
    }
}

/// A value-preserving bind may rebuild `pure(value)` to retime the hap. Carry
/// the exact lookup only when the inner result kept the SAME identity-bearing
/// value; a merely equal user value must not inherit hidden Pattern entries.
fn joined_pick_lookup(outer: &Hap, inner: &Hap) -> Option<Arc<PickLookup>> {
    inner.pick_lookup.clone().or_else(|| {
        (outer.pick_lookup.is_some() && inner.value == outer.value)
            .then(|| outer.pick_lookup.clone())
            .flatten()
    })
}

impl fmt::Display for Hap {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.show())
    }
}

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
pub struct State {
    pub span: TimeSpan,
    /// Controls carried through the query. `_cps` lives here - `glide`-style
    /// user patterns read it to tell a real trigger from a lookahead query.
    pub controls: OrderedMap,
}

impl State {
    pub fn new(span: TimeSpan) -> Self {
        Self {
            span,
            controls: OrderedMap::new(),
        }
    }
    pub fn set_span(&self, span: TimeSpan) -> Self {
        Self {
            span,
            controls: self.controls.clone(),
        }
    }
    pub fn with_span(&self, f: impl Fn(&TimeSpan) -> TimeSpan) -> Self {
        self.set_span(f(&self.span))
    }

    /// Shallow object spread: existing key order is retained when overwritten,
    /// and newly introduced controls append in source order.
    pub fn set_controls(&self, controls: &OrderedMap) -> Self {
        let mut merged = self.controls.clone();
        for (key, value) in controls {
            merged.insert(key.clone(), value.clone());
        }
        Self {
            span: self.span,
            controls: merged,
        }
    }
}

// ---------------------------------------------------------------------------
// Pattern
//
// `Pattern` is a pure function from a State to the Haps active during it.
// The node enum keeps the graph inspectable, which is what makes purity
// classification a structural property rather than a guess.
// ---------------------------------------------------------------------------

#[derive(Clone)]
pub struct Pattern {
    node: Arc<Node>,
    /// `_steps` metadata, used by `pace`/`expand`/`contract`/`stepcat`.
    pub steps: Option<Fraction>,
    /// `__pure_loc`: source span of a pure value, merged by `register()`'s
    /// fast path.
    pure_loc: Option<(usize, usize)>,
    /// Computed at construction, never inspected later. See `purity` module.
    purity: purity::Purity,
    /// Native module state selected for the complete query operation.
    ///
    /// This lives on the handle rather than as a graph node: repeated exports
    /// stay flat, and a derived query-time callback remains inside the same
    /// runtime scope as its receiver.
    runtime_settings: Option<settings::RuntimeSettings>,
}

#[derive(Clone, Debug, Default)]
pub struct TimelineState {
    offsets: Arc<Mutex<BTreeMap<String, Fraction>>>,
}

impl TimelineState {
    pub fn reset(&self) {
        self.offsets
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clear();
    }

    fn get(&self, key: &str) -> Option<Fraction> {
        self.offsets
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(key)
            .copied()
    }

    fn insert(&self, key: String, value: Fraction) {
        self.offsets
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(key, value);
    }

    fn remove(&self, key: &str) {
        self.offsets
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(key);
    }
}

/// Which operand's `whole` survives a join.
///
/// The first three are `bindWhole` specialisations and share one loop; the
/// last three align the inner pattern differently and have their own loops.
/// One enum because they all consume the same `PatternOf*` node, which is
/// what carries the purity proof.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JoinMode {
    /// `innerJoin`: `(_, b) => b`
    Inner,
    /// `outerJoin`: `(a) => a`
    Outer,
    /// `join`/`bind`: `a.intersection_e(b)`
    Mix,
    /// `squeezeJoin`: each inner pattern is `focusSpan`ed onto the outer hap's
    /// `wholeOrPart`, so a whole cycle of the inner fits each outer event.
    Squeeze,
    /// `resetJoin(false)`: inner patterns are re-aligned so their **cycle
    /// start** lands on each outer onset.
    Reset,
    /// `resetJoin(true)` / `restartJoin`: inner patterns are re-aligned so their
    /// **cycle zero** lands on each outer onset.
    Restart,
}

/// How a binary value application carries an out-of-band pick lookup.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LookupFlow {
    /// Preserve the source whose identity-bearing value was returned.
    Infer,
    /// Canonical default-alignment `add`, semantically identical to `Infer`
    /// but retained as a non-executing structural witness for callback IR.
    Add,
    /// `keep` / `keepif`: the left value is the result.
    Left,
    /// The right value is the result.
    Right,
    /// `set`: right replaces scalars/arrays; objects merge right over left.
    Set,
}

// ---------------------------------------------------------------------------
// The JS boundary, from L1's side.
//
// L1 never contains a QuickJS value. A node that invokes user code stores an
// opaque `CallbackId`; the host that can resolve it is provided by L2 and
// reached through a thread-local, because a QuickJS runtime is owned by exactly
// one thread and is not `Send`.
//
// Pure patterns never consult the host. Queries of pure patterns with no
// host installed assert this property.
// ---------------------------------------------------------------------------

/// Opaque index into L2's trace-managed callback sidecar.
pub type CallbackId = usize;

/// What a bind callback is handed.
///
/// `fmap(func)` gives `func` whatever the outer hap holds, and after
/// transpilation that is a pattern as often as it is a scalar: `pure("bd")`
/// transpiles to a pattern-of-patterns, `pure('bd')` stays a string.
pub enum BindArg<'a> {
    Value(&'a Value),
    Pattern(Pattern),
}

/// What a bind callback produced.
///
/// `fmap` replaces the hap's value with the callback's return value. A
/// non-pattern return is therefore the new value, and the join words its
/// error from that value. This is why `stepBind(x => undefined)` and
/// `stepBind(x => 42)` throw different `TypeError`s.
pub enum BindResult {
    Pattern(Pattern),
    Value(Value),
}

/// Implemented by L2 (`rustel-jsruntime`). L1 depends only on this trait.
pub trait CallbackHost {
    /// Report an ordinary query error contained by one stacked child.
    fn log_query_error(&self, _message: &str) {}

    /// `fmap`-style: transform one value.
    fn call_value(&self, id: CallbackId, v: &Value) -> Result<Value, String>;
    fn call_ref(&self, id: CallbackId) -> Result<Pattern, String> {
        Err(format!("callback {id} is not a ref accessor"))
    }
    fn call_pick_lookup(&self, id: CallbackId) -> Result<PickLookup, String> {
        Err(format!("JavaScript value {id} is not a pick lookup"))
    }
    fn call_materialize_value(&self, id: CallbackId) -> Result<Value, String> {
        Err(format!("JavaScript value {id} cannot be materialized"))
    }
    /// A user-authored `new Pattern(state => …)`.
    fn call_query(&self, id: CallbackId, state: &State) -> Result<Vec<Hap>, String>;
    /// A bind callback - `polyBind(x => ...)` / `stepBind(x => ...)`.
    ///
    /// Two things separate this from `call_pattern`, which is a pattern
    /// transformer (`every(4, x => x.fast(2))`):
    ///
    /// * the argument may be either a hap value or a whole pattern, because
    ///   `fmap` passes on whatever the outer hap carried and transpiled
    ///   mini-notation makes that a pattern;
    /// * a return value that is not a pattern is not an `Err` - it becomes
    ///   the hap's value and the failure surfaces only in the join. An error
    ///   here would make `stepBind(x => 42)` (throws during construction)
    ///   indistinguishable from `stepBind(x => { throw })` (constructs, then
    ///   empties the query).
    fn call_bind(&self, id: CallbackId, _arg: BindArg<'_>) -> Result<BindResult, String> {
        Err(format!("callback {id} is not a bind callback"))
    }

    /// A pattern transformer - `every(4, x => x.fast(2))`.
    ///
    /// Defaults to an error so an older host that never sees a function-valued
    /// argument does not have to implement it.
    fn call_pattern(&self, id: CallbackId, _pattern: Pattern) -> Result<Pattern, String> {
        Err(format!("callback {id} is not a pattern transformer"))
    }

    /// A pattern transformer with a bounded native candidate and its original
    /// JavaScript callback retained as the compatibility fallback.
    ///
    /// Hosts that do not implement callback IR remain correct by taking the
    /// ordinary callback path. The runtime that owns the callable decides
    /// whether to execute, dual-run, or bypass the candidate.
    fn call_pattern_ir(
        &self,
        id: CallbackId,
        _program: &callback_ir::PatternTransformProgram,
        pattern: Pattern,
    ) -> Result<Pattern, String> {
        self.call_pattern(id, pattern)
    }

    /// An indexed transformer as `echoWith` uses it.
    ///
    /// Deliberately a batch rather than a unary callback with an extra
    /// argument: every `func(pattern, index)` call runs first, and only then
    /// are the raw results reified `stack`-style (scalars, strings and nested
    /// arrays are all acceptable). The phase boundary lives in L2 because a
    /// later callback may mutate an earlier result or replace the string
    /// parser before any result is reified.
    fn call_pattern_indexed_batch(
        &self,
        id: CallbackId,
        _patterns: Vec<(Pattern, i64)>,
    ) -> Result<Vec<Pattern>, String> {
        Err(format!(
            "callback {id} is not an indexed pattern transformer"
        ))
    }

    /// An `arpWith` callback - receives one congruent chord as `Hap[]` and
    /// returns something `reify()` turns into a pattern.
    ///
    /// The result stays a [`Pattern`] rather than being collapsed to a chosen
    /// value here: callbacks may return `fastcat(...haps)` or `stack(...haps)`,
    /// and that returned pattern's timing is part of `arpWith` semantics.
    fn call_haps(&self, id: CallbackId, _haps: &[Hap]) -> Result<Pattern, String> {
        Err(format!("callback {id} is not a hap-array callback"))
    }
    fn call_hap_predicate(&self, id: CallbackId, _hap: &Hap) -> Result<bool, String> {
        Err(format!("callback {id} is not a hap predicate"))
    }
    /// A `filterValues` predicate - receive one value, answer whether to keep
    /// the hap.
    ///
    /// Distinct from [`CallbackHost::call_value`] so the filter path can
    /// contain a throw per hap (see [`crate::signal_callback_failure`])
    /// without changing `fmap`'s abort-and-empty contract.
    fn call_value_predicate(&self, id: CallbackId, v: &Value) -> Result<bool, String> {
        self.call_value(id, v).map(|value| value.js_truthy())
    }
    fn call_time_predicate(&self, id: CallbackId, _time: Fraction) -> Result<bool, String> {
        Err(format!("callback {id} is not a time predicate"))
    }
    fn call_span_transform(&self, id: CallbackId, _span: TimeSpan) -> Result<TimeSpan, String> {
        Err(format!("callback {id} is not a span transform"))
    }
}

thread_local! {
    /// Erased to a thin pointer + vtable pair with a fabricated lifetime; only
    /// ever set by `with_callback_host`, which guarantees the referent outlives
    /// the scope in which it is readable.
    /// `Cell`, not `RefCell`: queries NEST (a JS callback can query another
    /// pattern), so an outstanding `borrow()` during the inner install would
    /// panic with "RefCell already borrowed". `Cell` has no runtime borrow
    /// state to conflict.
    static CURRENT_HOST: std::cell::Cell<Option<*const (dyn CallbackHost + 'static)>> =
        const { std::cell::Cell::new(None) };
    /// An `arpWith` callback may return a pattern that reaches `arpWith` again.
    /// Count that logical re-entry while the returned pattern is queried;
    /// ordinary native pattern composition remains unlimited by this guard.
    static CALLBACK_RECURSION_DEPTH: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    /// Optional producer-side observation of JavaScript callback crossings.
    /// The pointer is installed only around one profiled query and never
    /// escapes that scope. Nested callback re-entry is counted but timed only
    /// at the outermost boundary so elapsed time is not double-counted.
    static CALLBACK_QUERY_METRICS: std::cell::Cell<Option<*mut CallbackQueryMetrics>> =
        const { std::cell::Cell::new(None) };
    static CALLBACK_QUERY_DEPTH: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
    #[cfg(feature = "callback-census")]
    static CALLBACK_QUERY_KIND_SAMPLE_SEQUENCE: std::cell::Cell<u64> =
        const { std::cell::Cell::new(0) };
}

/// A callback-host operation counted during one pattern query.
///
/// This is plain producer-thread data. It is neither shared with nor updated
/// by the audio callback.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum CallbackQueryKind {
    Value,
    Reference,
    PickLookup,
    MaterializeValue,
    Pattern,
    PatternIr,
    IndexedPatternBatch,
    Haps,
    HapPredicate,
    TimePredicate,
    SpanTransform,
    Bind,
    Query,
}

impl CallbackQueryKind {
    pub const ALL: [Self; CALLBACK_QUERY_KIND_COUNT] = [
        Self::Value,
        Self::Reference,
        Self::PickLookup,
        Self::MaterializeValue,
        Self::Pattern,
        Self::PatternIr,
        Self::IndexedPatternBatch,
        Self::Haps,
        Self::HapPredicate,
        Self::TimePredicate,
        Self::SpanTransform,
        Self::Bind,
        Self::Query,
    ];

    const fn index(self) -> usize {
        self as usize
    }
}

const CALLBACK_QUERY_KIND_COUNT: usize = 13;

/// Exact callback crossings grouped by host-boundary operation.
///
/// The fixed-size array keeps collection bounded and allocation-free. Timing
/// remains sampled separately by [`CallbackQueryMetrics`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CallbackQueryKindCounts {
    counts: [u64; CALLBACK_QUERY_KIND_COUNT],
}

impl CallbackQueryKindCounts {
    pub fn get(&self, kind: CallbackQueryKind) -> u64 {
        self.counts[kind.index()]
    }

    pub fn total(&self) -> u64 {
        self.counts.iter().copied().fold(0, u64::saturating_add)
    }

    #[cfg(any(feature = "callback-census", test))]
    fn increment(&mut self, kind: CallbackQueryKind) {
        let count = &mut self.counts[kind.index()];
        *count = count.saturating_add(1);
    }
}

/// Aggregate host-boundary work for one pattern query.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CallbackQueryMetrics {
    calls: u64,
    #[cfg(any(feature = "callback-census", test))]
    kinds: CallbackQueryKindCounts,
    #[cfg(any(feature = "callback-census", test))]
    kinds_sampled: bool,
    busy_nanos: u64,
    outer_calls: u64,
    sampled_outer_calls: u64,
    sampled_busy_nanos: u64,
}

impl CallbackQueryMetrics {
    pub fn calls(&self) -> u64 {
        self.calls
    }

    /// Exact kind counts when this query was selected for the bounded census.
    pub fn kind_sample(&self) -> Option<CallbackQueryKindCounts> {
        #[cfg(any(feature = "callback-census", test))]
        {
            self.kinds_sampled.then_some(self.kinds)
        }
        #[cfg(not(any(feature = "callback-census", test)))]
        {
            None
        }
    }

    /// Estimated time inside the callback host boundary.
    ///
    /// Calls are exact. Timing samples one out of every eight outermost
    /// crossings and scales their mean across the query, avoiding a clock
    /// read around every small callback while retaining a useful phase split.
    pub fn busy_nanos(&self) -> u64 {
        self.busy_nanos
    }
}

const CALLBACK_TIMING_SAMPLE_INTERVAL: u64 = 8;
#[cfg(feature = "callback-census")]
const CALLBACK_KIND_SAMPLE_INTERVAL: u64 = 8;

/// Observe JavaScript callback crossings made by `f`.
///
/// Nested installs restore the previous recorder and depth on every exit,
/// including unwinding. The query owner supplies the storage, so recording is
/// bounded and allocation-free.
pub fn with_callback_query_metrics<R>(
    metrics: &mut CallbackQueryMetrics,
    f: impl FnOnce() -> R,
) -> R {
    #[cfg(feature = "callback-census")]
    let sample_kinds = CALLBACK_QUERY_KIND_SAMPLE_SEQUENCE.with(|sequence| {
        let current = sequence.get();
        sequence.set(current.wrapping_add(1));
        current.is_multiple_of(CALLBACK_KIND_SAMPLE_INTERVAL)
    });
    #[cfg(not(feature = "callback-census"))]
    let sample_kinds = false;
    with_callback_query_metrics_policy(metrics, sample_kinds, f)
}

fn with_callback_query_metrics_policy<R>(
    metrics: &mut CallbackQueryMetrics,
    sample_kinds: bool,
    f: impl FnOnce() -> R,
) -> R {
    struct Guard {
        metrics: *mut CallbackQueryMetrics,
        previous: Option<*mut CallbackQueryMetrics>,
        previous_depth: u32,
    }
    impl Drop for Guard {
        fn drop(&mut self) {
            // SAFETY: the caller-owned recorder outlives this complete guard
            // scope and no reference to it crosses a callback.
            let metrics = unsafe { &mut *self.metrics };
            if let Some(busy_nanos) = metrics
                .sampled_busy_nanos
                .saturating_mul(metrics.outer_calls)
                .checked_div(metrics.sampled_outer_calls)
            {
                metrics.busy_nanos = busy_nanos;
            }
            CALLBACK_QUERY_METRICS.with(|slot| slot.set(self.previous));
            CALLBACK_QUERY_DEPTH.with(|slot| slot.set(self.previous_depth));
        }
    }

    *metrics = CallbackQueryMetrics::default();
    #[cfg(any(feature = "callback-census", test))]
    {
        metrics.kinds_sampled = sample_kinds;
    }
    #[cfg(not(any(feature = "callback-census", test)))]
    {
        let _ = sample_kinds;
    }
    let previous = CALLBACK_QUERY_METRICS.with(|slot| slot.replace(Some(metrics)));
    let previous_depth = CALLBACK_QUERY_DEPTH.with(|slot| slot.replace(0));
    let _guard = Guard {
        metrics,
        previous,
        previous_depth,
    };
    f()
}

struct CallbackQueryTimer {
    metrics: Option<*mut CallbackQueryMetrics>,
    started: Option<std::time::Instant>,
    previous_depth: u32,
}

impl CallbackQueryTimer {
    fn enter(kind: CallbackQueryKind) -> Self {
        #[cfg(not(any(feature = "callback-census", test)))]
        let _ = kind;
        let metrics = CALLBACK_QUERY_METRICS.with(|slot| slot.get());
        let Some(metrics) = metrics else {
            return Self {
                metrics: None,
                started: None,
                previous_depth: 0,
            };
        };
        // SAFETY: `with_callback_query_metrics` owns this pointer for the
        // complete dynamic scope. No reference is retained across a callback,
        // so nested re-entry cannot alias a live `&mut`.
        unsafe {
            (*metrics).calls = (*metrics).calls.saturating_add(1);
            #[cfg(any(feature = "callback-census", test))]
            if (*metrics).kinds_sampled {
                (*metrics).kinds.increment(kind);
            }
        }
        let previous_depth = CALLBACK_QUERY_DEPTH.with(|depth| {
            let current = depth.get();
            depth.set(current.saturating_add(1));
            current
        });
        let sampled = if previous_depth == 0 {
            // SAFETY: same scoped pointer contract as above.
            let outer_call = unsafe {
                (*metrics).outer_calls = (*metrics).outer_calls.saturating_add(1);
                (*metrics).outer_calls
            };
            outer_call % CALLBACK_TIMING_SAMPLE_INTERVAL == 1
        } else {
            false
        };
        if sampled {
            // SAFETY: same scoped pointer contract as above.
            unsafe {
                (*metrics).sampled_outer_calls = (*metrics).sampled_outer_calls.saturating_add(1);
            }
        }
        Self {
            metrics: Some(metrics),
            started: sampled.then(std::time::Instant::now),
            previous_depth,
        }
    }
}

impl Drop for CallbackQueryTimer {
    fn drop(&mut self) {
        CALLBACK_QUERY_DEPTH.with(|depth| depth.set(self.previous_depth));
        let (Some(metrics), Some(started)) = (self.metrics, self.started) else {
            return;
        };
        let elapsed = started.elapsed().as_nanos().min(u128::from(u64::MAX)) as u64;
        // SAFETY: same scoped pointer contract as `enter`; the recorder still
        // outlives this guard and no mutable reference crosses the callback.
        unsafe {
            (*metrics).sampled_busy_nanos = (*metrics).sampled_busy_nanos.saturating_add(elapsed);
        }
    }
}

#[inline]
fn measure_host_callback<R>(kind: CallbackQueryKind, f: impl FnOnce() -> R) -> R {
    let _timer = CallbackQueryTimer::enter(kind);
    f()
}

// Low enough to unwind safely on a 1 MiB process stack. Self-re-entry must
// become a query error before Rust's non-unwinding stack abort.
const MAX_CALLBACK_RECURSION_DEPTH: usize = 16;

struct CallbackRecursionGuard;

impl CallbackRecursionGuard {
    fn enter() -> Option<Self> {
        CALLBACK_RECURSION_DEPTH.with(|depth| {
            let current = depth.get();
            if current >= MAX_CALLBACK_RECURSION_DEPTH {
                None
            } else {
                depth.set(current + 1);
                Some(Self)
            }
        })
    }
}

impl Drop for CallbackRecursionGuard {
    fn drop(&mut self) {
        CALLBACK_RECURSION_DEPTH.with(|depth| {
            let current = depth.get();
            debug_assert!(current > 0, "callback recursion depth underflow");
            depth.set(current.saturating_sub(1));
        });
    }
}

/// Installs `host` for the duration of `f`. L2 wraps every query in this.
///
/// # Safety
/// The raw pointer never escapes this call: it is stored, used only while `f`
/// runs on this thread, and cleared before returning (including on unwind).
pub fn with_callback_host<R>(host: &dyn CallbackHost, f: impl FnOnce() -> R) -> R {
    struct Guard(Option<*const (dyn CallbackHost + 'static)>);
    impl Drop for Guard {
        fn drop(&mut self) {
            CURRENT_HOST.with(|h| h.set(self.0));
        }
    }
    let prev = CURRENT_HOST.with(|h| {
        // SAFETY: the fabricated 'static lifetime never escapes - the guard
        // restores the previous value before this function returns, including
        // on unwind, so the pointer is unreadable after `host` could die.
        let p: *const (dyn CallbackHost + 'static) =
            unsafe { std::mem::transmute::<*const dyn CallbackHost, _>(host) };
        h.replace(Some(p))
    });
    let _guard = Guard(prev);
    f()
}

fn host_call_value(id: CallbackId, v: &Value) -> Value {
    CURRENT_HOST.with(|h| match h.get() {
        // SAFETY: set only by `with_callback_host`, which outlives this call.
        // As with `host_call_pattern`: a throw inside the user's function is
        // a query error, not a reason to abort the process.
        Some(p) => match measure_host_callback(CallbackQueryKind::Value, || {
            unsafe { &*p }.call_value(id, v)
        }) {
            Ok(value) => value,
            Err(error) => {
                signal_query_error(move || format!("callback {id} failed: {error}"));
                Value::Undefined
            }
        },
        None => panic!(
            "callback {id} invoked with no host installed. A pattern classified \
             PURE must never reach here: purity is what guarantees it needs no host."
        ),
    })
}

fn host_call_ref(id: CallbackId) -> Pattern {
    CURRENT_HOST.with(|host| match host.get() {
        // SAFETY: set only by `with_callback_host`, which outlives this call.
        Some(pointer) => match measure_host_callback(CallbackQueryKind::Reference, || {
            unsafe { &*pointer }.call_ref(id)
        }) {
            Ok(pattern) => pattern,
            Err(error) => {
                signal_query_error(move || format!("ref callback {id} failed: {error}"));
                silence()
            }
        },
        None => panic!(
            "ref callback {id} invoked with no host installed. A pattern \
             classified PURE must never reach here"
        ),
    })
}

fn host_call_pick_lookup(id: CallbackId) -> Option<PickLookup> {
    CURRENT_HOST.with(|host| match host.get() {
        Some(pointer) => {
            match measure_host_callback(CallbackQueryKind::PickLookup, || {
                unsafe { &*pointer }.call_pick_lookup(id)
            }) {
                Ok(lookup) => Some(lookup),
                Err(error) => {
                    signal_query_error(move || error);
                    None
                }
            }
        }
        None => panic!("JavaScript lookup {id} reached with no host installed"),
    })
}

pub fn materialize_js_value(value: &Value) -> Value {
    match value {
        Value::JsValue(reference) => CURRENT_HOST.with(|host| match host.get() {
            Some(pointer) => {
                match measure_host_callback(CallbackQueryKind::MaterializeValue, || {
                    unsafe { &*pointer }.call_materialize_value(reference.id())
                }) {
                    Ok(value) => value,
                    Err(error) => {
                        signal_query_error(move || error);
                        Value::Undefined
                    }
                }
            }
            None => {
                // A top-level query materialises JS-owned containers before
                // its host scope unwinds. Keep this fallback non-fatal for
                // raw-query callers: rendering a hap must never turn a
                // live-coding value into a Rust process abort merely because
                // it crossed the wrong phase boundary.
                signal_query_error(|| {
                    format!(
                        "JavaScript value {} reached with no host installed",
                        reference.id()
                    )
                });
                Value::Undefined
            }
        }),
        // Controls can wrap a JS-owned value before the outer boundary sees
        // it (`s(pure(['bd']))` is `{s: <opaque array>}`). Recurse through the
        // native container or the inner identity would still escape the host.
        Value::List(values) => Value::List(values.iter().map(materialize_js_value).collect()),
        Value::Object(values) => Value::object(
            values
                .iter()
                .map(|(key, value)| (key.to_string(), materialize_js_value(value))),
        ),
        Value::Haps(haps) => Value::Haps(value::HapList::new(
            haps.as_slice()
                .iter()
                .map(|hap| hap.with_value(materialize_js_value))
                .collect(),
        )),
        _ => value.clone(),
    }
}

/// Apply a user JavaScript function to a pattern, through the host.
///
/// `every(4, x => x.fast(2))` puts the closure in a hap; this is where it is
/// called. Like the other host entry points, a pattern classified PURE must
/// never reach here.
pub(crate) fn host_call_pattern(id: CallbackId, pattern: Pattern) -> Pattern {
    CURRENT_HOST.with(|h| match h.get() {
        // SAFETY: as above.
        Some(p) => match measure_host_callback(CallbackQueryKind::Pattern, || {
            unsafe { &*p }.call_pattern(id, pattern)
        }) {
            Ok(result) => result,
            // A throw inside the user's function is a user error. It reaches
            // the `queryArc` boundary, which yields no haps. A panic here
            // would end the process for a mistake in a live-coded expression.
            // A missing host is a false-pure classification, and it panics
            // below.
            Err(error) => {
                signal_query_error(move || format!("pattern callback {id} failed: {error}"));
                silence()
            }
        },
        None => panic!(
            "pattern callback {id} invoked with no host installed. A pattern \
             classified PURE must never reach here: purity is what guarantees it needs no host."
        ),
    })
}

/// Apply a bounded native callback candidate through its owning host.
///
/// The host retains the original JavaScript function and owns selection and
/// differential policy. A missing host remains a false-pure classification,
/// exactly like the compatibility-only pattern callback path.
pub(crate) fn host_call_pattern_ir(
    id: CallbackId,
    program: &callback_ir::PatternTransformProgram,
    pattern: Pattern,
) -> Pattern {
    CURRENT_HOST.with(|host| match host.get() {
        // SAFETY: set only by `with_callback_host`, which outlives this call.
        Some(pointer) => match measure_host_callback(CallbackQueryKind::PatternIr, || {
            unsafe { &*pointer }.call_pattern_ir(id, program, pattern)
        }) {
            Ok(result) => result,
            Err(error) => {
                signal_query_error(move || format!("pattern callback IR {id} failed: {error}"));
                silence()
            }
        },
        None => panic!(
            "pattern callback IR {id} invoked with no host installed. A pattern \
             classified PURE must never reach here: purity is what guarantees it needs no host."
        ),
    })
}

/// Apply one JavaScript callback to a complete indexed batch.
///
/// `echoWith`'s two phases live behind this one host call: JavaScript invokes
/// every callback first, then performs `stack`-style reification of every raw
/// result. A throw stops the batch and becomes the same query error as the
/// unary transformer path; a missing host remains a hard purity failure.
pub(crate) fn host_call_pattern_indexed_batch(
    id: CallbackId,
    patterns: Vec<(Pattern, i64)>,
) -> Vec<Pattern> {
    CURRENT_HOST.with(|host| match host.get() {
        // SAFETY: set only by `with_callback_host`, which outlives this call.
        Some(pointer) => match measure_host_callback(CallbackQueryKind::IndexedPatternBatch, || {
            unsafe { &*pointer }.call_pattern_indexed_batch(id, patterns)
        }) {
            Ok(result) => result,
            Err(error) => {
                signal_query_error(move || {
                    format!("indexed pattern callback {id} failed: {error}")
                });
                Vec::new()
            }
        },
        None => panic!(
            "indexed pattern callback {id} invoked with no host installed. A pattern \
             classified PURE must never reach here: purity is what guarantees it needs no host."
        ),
    })
}

/// Apply an `arpWith` callback to one congruent chord through the host.
///
/// A throw is a query error; the outer `queryArc` boundary discards the
/// entire partial query. A missing host
/// remains a hard purity failure.
fn host_call_haps(id: CallbackId, haps: &[Hap]) -> Pattern {
    CURRENT_HOST.with(|h| match h.get() {
        // SAFETY: set only by `with_callback_host`, which outlives this call.
        Some(p) => match measure_host_callback(CallbackQueryKind::Haps, || {
            unsafe { &*p }.call_haps(id, haps)
        }) {
            Ok(result) => result,
            Err(error) => {
                signal_query_error(move || format!("hap-array callback {id} failed: {error}"));
                silence()
            }
        },
        None => panic!(
            "hap-array callback {id} invoked with no host installed. A pattern \
             classified PURE must never reach here: purity is what guarantees it needs no host."
        ),
    })
}

/// Apply a user JavaScript `filterHaps` predicate through the host.
///
/// A throw inside the predicate does NOT abort the query: the hap it was
/// asked to judge keeps playing (fail open), the failure is recorded on the
/// contained-callback-failure channel, and the query's caller reports it
/// once. On stage, one broken predicate must not stop the set.
fn host_call_hap_predicate(id: CallbackId, hap: &Hap) -> bool {
    CURRENT_HOST.with(|host| match host.get() {
        Some(pointer) => match measure_host_callback(CallbackQueryKind::HapPredicate, || unsafe {
            &*pointer
        }
        .call_hap_predicate(id, hap)) {
            Ok(result) => result,
            Err(error) => {
                signal_callback_failure(move || format!("hap predicate {id} failed: {error}"));
                true
            }
        },
        None => panic!(
            "hap predicate {id} invoked with no host installed. A pattern classified PURE must never reach here"
        ),
    })
}

/// Apply a user JavaScript `filterValues` predicate through the host, with
/// the same contain-and-fail-open contract as [`host_call_hap_predicate`].
fn host_call_value_predicate(id: CallbackId, v: &Value) -> bool {
    CURRENT_HOST.with(|host| match host.get() {
        Some(pointer) => match measure_host_callback(CallbackQueryKind::Value, || {
            unsafe { &*pointer }.call_value_predicate(id, v)
        }) {
            Ok(keep) => keep,
            Err(error) => {
                signal_callback_failure(move || format!("callback {id} failed: {error}"));
                true
            }
        },
        None => panic!(
            "callback {id} invoked with no host installed. A pattern classified \
             PURE must never reach here: purity is what guarantees it needs no host."
        ),
    })
}

fn host_call_time_predicate(id: CallbackId, time: Fraction) -> bool {
    CURRENT_HOST.with(|host| match host.get() {
        Some(pointer) => match measure_host_callback(CallbackQueryKind::TimePredicate, || unsafe {
            &*pointer
        }
        .call_time_predicate(id, time)) {
            Ok(result) => result,
            Err(error) => {
                signal_query_error(move || format!("time predicate {id} failed: {error}"));
                false
            }
        },
        None => panic!(
            "time predicate {id} invoked with no host installed. A pattern classified PURE must never reach here"
        ),
    })
}

fn host_call_span_transform(id: CallbackId, span: TimeSpan) -> TimeSpan {
    CURRENT_HOST.with(|host| match host.get() {
        Some(pointer) => match measure_host_callback(CallbackQueryKind::SpanTransform, || unsafe {
            &*pointer
        }
        .call_span_transform(id, span)) {
            Ok(result) => result,
            Err(error) => {
                signal_query_error(move || format!("span transform {id} failed: {error}"));
                span
            }
        },
        None => panic!(
            "span transform {id} invoked with no host installed. A pattern classified PURE must never reach here"
        ),
    })
}

/// Apply a user JavaScript bind callback, through the host.
///
/// A non-pattern return becomes the hap's value, and the join fails on it
/// later. Only a throw (or an argument that is not callable) is a query
/// error here: `fmap` never inspects what the callback returns.
pub(crate) fn host_call_bind(id: CallbackId, arg: BindArg<'_>) -> BindResult {
    CURRENT_HOST.with(|h| match h.get() {
        // SAFETY: as above.
        Some(p) => match measure_host_callback(CallbackQueryKind::Bind, || {
            unsafe { &*p }.call_bind(id, arg)
        }) {
            Ok(result) => result,
            // A throw inside the user's function is user error: it unwinds to
            // the `queryArc` boundary, which yields no haps. Reporting a
            // PATTERN rather than a value keeps this distinct from a
            // non-pattern return, which does NOT stop the query here.
            Err(error) => {
                signal_query_error(move || format!("bind callback {id} failed: {error}"));
                BindResult::Pattern(silence())
            }
        },
        None => panic!(
            "bind callback {id} invoked with no host installed. A pattern \
             classified PURE must never reach here: purity is what guarantees it needs no host."
        ),
    })
}

fn host_call_query(id: CallbackId, state: &State) -> Vec<Hap> {
    CURRENT_HOST.with(|h| match h.get() {
        // SAFETY: as above.
        Some(p) => match measure_host_callback(CallbackQueryKind::Query, || {
            unsafe { &*p }.call_query(id, state)
        }) {
            Ok(haps) => haps,
            Err(error) => {
                signal_query_error(move || format!("query callback {id} failed: {error}"));
                Vec::new()
            }
        },
        None => panic!(
            "query callback {id} invoked with no host installed. A pattern \
             classified PURE must never reach here: purity is what guarantees it needs no host."
        ),
    })
}

/// Combines an accumulator value with the next positional argument during
/// `register()`'s appLeft fold.
type CombineFn = Arc<dyn Fn(&Value, &Value) -> Value + Send + Sync>;
/// Maps a value to a whole pattern, for a `PatternOf` awaiting a join.
type ToPatternFn = Arc<dyn Fn(&Value) -> Pattern + Send + Sync>;
type HapToPatternFn = Arc<dyn Fn(&Hap) -> Pattern + Send + Sync>;
/// A closure whose RETURN TYPE proves it cannot materialise JavaScript.
type ToPurePatternFn = Arc<dyn Fn(&Value) -> purity::PurePattern + Send + Sync>;

/// The most constructions one `PatternOfPure` node keeps.
///
/// Argument values repeat: a patterned argument such as `"0,1,2,3"` names
/// four values, and every query of every cycle asks the same four again. A
/// bind's carrier repeats even more - `pure(x).inner_bind(f)` asks about
/// `x` once per cycle per query. Keeping the answers is what lets the graph
/// they build (and any cache inside it) live across queries instead of being
/// rebuilt and thrown away each time.
const CONSTRUCTION_MEMO_ENTRIES: usize = 32;

/// Patterns a `PatternOfPure` closure has already built, by the value it was
/// asked about and the settings snapshot it was asked under.
///
/// Exact: a native body is a function of its argument values and the
/// selected settings, so the pattern it built for a value IS the pattern it
/// would build again. NaN never equals itself and is never kept; a volatile
/// construction (one that would start fresh memory each time) is not kept
/// either, so sharing cannot change what it answers. Bounded, and drained
/// onto the destruction worklist like any other child.
#[derive(Default)]
struct ConstructionMemo {
    entries: Mutex<Vec<ConstructionEntry>>,
    /// Round-robin victim once full. The key set is a handful of argument
    /// values, so plain replacement is enough and never thrashes on it.
    victim: std::sync::atomic::AtomicUsize,
}

struct ConstructionEntry {
    value: Value,
    settings: settings::SettingsStateId,
    pattern: purity::PurePattern,
}

/// Whether two memo keys name the same construction.
///
/// Stricter than `Value`'s equality for numbers: `-0.0 == 0.0` there, while a
/// body may well tell them apart (`1 / x`, or how the value prints), so the
/// bits have to agree. Containers compare element by element the same way;
/// everything else as `Value` compares.
fn same_key(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::F64(x), Value::F64(y)) => x.to_bits() == y.to_bits(),
        (Value::List(xs), Value::List(ys)) => {
            xs.len() == ys.len() && xs.iter().zip(ys).all(|(x, y)| same_key(x, y))
        }
        (Value::Object(xs), Value::Object(ys)) => {
            xs.len() == ys.len()
                && xs
                    .iter()
                    .zip(ys.iter())
                    .all(|((kx, x), (ky, y))| kx == ky && same_key(x, y))
        }
        _ => a == b,
    }
}

/// Whether a value may serve as a memo key: numbers, strings, booleans and
/// nothings, and lists and objects of those. A NaN could never be found
/// again; a pattern, function, hap list or host-owned value can hold a graph,
/// and a key holding a graph is how a cycle would form.
fn plain_key(value: &Value) -> bool {
    match value {
        Value::Undefined | Value::Null | Value::Bool(_) | Value::Str(_) => true,
        Value::F64(number) => !number.is_nan(),
        Value::List(items) => items.iter().all(plain_key),
        Value::Object(map) => map.iter().all(|(_, item)| plain_key(item)),
        Value::Pattern(_) | Value::JsValue(_) | Value::Function(_) | Value::Haps(_) => false,
    }
}

impl ConstructionMemo {
    fn lock(&self) -> std::sync::MutexGuard<'_, Vec<ConstructionEntry>> {
        self.entries
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// The pattern built for `value` under `settings`, building it with
    /// `construct` when nothing kept answers.
    fn construct(
        &self,
        value: &Value,
        settings: &settings::SettingsStateId,
        construct: impl FnOnce(&Value) -> purity::PurePattern,
    ) -> purity::PurePattern {
        if let Some(kept) = self
            .lock()
            .iter()
            .find(|entry| entry.settings == *settings && same_key(&entry.value, value))
        {
            return kept.pattern.clone();
        }
        // Built outside the lock: construction is arbitrary native code.
        let built = construct(value);
        // A construction that raised a query error or a refusal is what the
        // boundary is about to discard, and NaN would never be found again.
        // A key that holds a pattern handle is not kept either: a callback
        // can hand a graph its own ancestor as a value, and keeping that
        // would tie the cycle no worklist can take apart.
        let keep = plain_key(value)
            && !built.pattern().purity().volatile
            && !query_error_pending()
            && !budget_exhausted();
        if keep {
            let mut entries = self.lock();
            let entry = ConstructionEntry {
                value: value.clone(),
                settings: settings.clone(),
                pattern: built.clone(),
            };
            if entries.len() < CONSTRUCTION_MEMO_ENTRIES {
                entries.push(entry);
            } else {
                let victim = self
                    .victim
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
                    % CONSTRUCTION_MEMO_ENTRIES;
                entries[victim] = entry;
            }
        }
        built
    }

    /// Hand every kept pattern to the destruction worklist.
    fn drain_into(&self, out: &mut Vec<Pattern>) {
        out.extend(self.lock().drain(..).map(|entry| entry.pattern.0));
    }
}
/// A native value transform.
type ValueFn = Arc<dyn Fn(&Value) -> Value + Send + Sync>;
/// A native value predicate.
type ValuePredicate = Arc<dyn Fn(&Value) -> bool + Send + Sync>;
/// A native hap predicate - `filterHaps`, which sees whole/part, not just the
/// value. `discreteOnly`/`onsetsOnly` are the two uses the joins need.
type HapPredicate = Arc<dyn Fn(&Hap) -> bool + Send + Sync>;
type HapMapFn = Arc<dyn Fn(&Hap) -> Option<Hap> + Send + Sync>;
type StateHapMapFn = Arc<dyn Fn(&State, &Hap) -> Option<Hap> + Send + Sync>;
type HapExpandFn = Arc<dyn Fn(&Hap) -> Vec<Hap> + Send + Sync>;
/// A native continuous signal sampled once per query.
type SignalFn = Arc<dyn Fn(&State) -> Value + Send + Sync>;
/// A time transform applied to query or hap spans.
type TimeFn = Arc<dyn Fn(Fraction) -> Fraction + Send + Sync>;
/// A whole-span transform - `withQuerySpan` / `withHapSpan`. Distinct from
/// `TimeFn` because `zoom` and `revv` map begin and end **jointly**, and
/// `withCycle` needs the span's own `sam()`.
type SpanFn = Arc<dyn Fn(&TimeSpan) -> TimeSpan + Send + Sync>;

/// Live `Node` allocations. This cheap always-on counter observes Rust graph
/// lifetime independently of the JavaScript heap; aggregate RSS cannot make
/// that distinction.
static LIVE_NODES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Number of `Node`s currently allocated across all patterns.
pub fn live_node_count() -> u64 {
    LIVE_NODES.load(std::sync::atomic::Ordering::Relaxed)
}

// ---------------------------------------------------------------------------
// Query errors - the `queryArc` try/catch.
//
// Several operations **throw** mid-query: `parseNumeral` on a value that is
// neither numeric nor a note name, `TimeSpan.intersection_e` on disjoint
// spans. A stack contains the failure to one child; otherwise it reaches
// `queryArc`, which logs it and returns no haps.
//
// Rust has no exception to unwind, and making `query` return a `Result` would
// put a branch on the hottest path in the scheduler for a case only user
// error reaches. A thread-local flag carries the error to the nearest stack
// child boundary or the outer query boundary, which discards its result.
//
// writer                     slot                       reader
// signal_query_error      -> QUERY_ERROR             -> stack child boundary
//                                                       or queryArc boundary
// stack child boundary    -> CALLBACK_FAILURE,       -> queryArc boundary
//                            CONTAINED_THROW
// signal_callback_failure -> CALLBACK_FAILURE        -> queryArc boundary
// queryArc boundary       -> QUERY_CALLBACK_FAILURE, -> top-level caller
// (no query error)           QUERY_CONTAINED_THROW
// ---------------------------------------------------------------------------

thread_local! {
    static QUERY_ERROR: std::cell::RefCell<Option<String>> =
        const { std::cell::RefCell::new(None) };
    /// A failure the query contained rather than aborted on. The failure is
    /// reported once by the query's caller; first one wins per boundary.
    static CALLBACK_FAILURE: std::cell::RefCell<Option<String>> =
        const { std::cell::RefCell::new(None) };
    /// The contained callback failure published by the last finished
    /// `queryArc` boundary, for the top-level caller to take and report.
    /// Cleared at top-level query entry so a stale failure cannot be
    /// attributed to the next independent query.
    static QUERY_CALLBACK_FAILURE: std::cell::RefCell<Option<String>> =
        const { std::cell::RefCell::new(None) };
    /// A query error a stack contained to one child. The siblings keep
    /// playing, but the caller still reports a throw; first one wins per
    /// boundary.
    static CONTAINED_THROW: std::cell::RefCell<Option<String>> =
        const { std::cell::RefCell::new(None) };
    /// The contained throw published by the last finished `queryArc`
    /// boundary, cleared with `QUERY_CALLBACK_FAILURE`.
    static QUERY_CONTAINED_THROW: std::cell::RefCell<Option<String>> =
        const { std::cell::RefCell::new(None) };
    /// How many `queryArc`-style boundaries are live on this thread. See
    /// [`query_in_progress`].
    static QUERY_ERROR_BOUNDARY_DEPTH: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
}

/// Whether a query is in progress on this thread: a `Pattern::query` frame,
/// or a `queryArc`-style boundary around one - including the cycle-0 probe
/// `stepJoin`/`stepBind` run while their pattern is still being constructed,
/// which resolves a pattern-of-patterns outside any query frame.
///
/// A host uses this to tell a callback the score's own method call invoked
/// synchronously, outside every query, from one a query reached. Only the
/// former throws straight back into the score; the latter belongs to the
/// query's boundary, which answers silence, as `queryArc` does.
pub fn query_in_progress() -> bool {
    QUERY_ERROR_BOUNDARY_DEPTH.with(|depth| depth.get() > 0)
        || QUERY_DEPTH.with(|depth| depth.get() > 0)
}

/// Record a mid-query failure. The first one wins, as a thrown exception
/// would ensure.
pub fn signal_query_error(message: impl FnOnce() -> String) {
    QUERY_ERROR.with(|slot| {
        let mut slot = slot.borrow_mut();
        if slot.is_none() {
            *slot = Some(message());
        }
    });
}

/// Take and clear the pending query error, if any.
pub fn take_query_error() -> Option<String> {
    QUERY_ERROR.with(|slot| slot.borrow_mut().take())
}

/// Record a failure the query contains, reported once by its caller instead
/// of emptying the whole arc. Filters keep the affected hap; stacks discard
/// the failed child's haps and keep the other children playing.
///
/// The flag also prevents result caches from retaining an incomplete answer.
pub fn signal_callback_failure(message: impl FnOnce() -> String) {
    CALLBACK_FAILURE.with(|slot| {
        let mut slot = slot.borrow_mut();
        if slot.is_none() {
            *slot = Some(message());
        }
    });
}

/// Take the contained callback failure published by the last completed
/// `queryArc` boundary, if any.
pub fn take_query_callback_failure() -> Option<String> {
    QUERY_CALLBACK_FAILURE.with(|slot| slot.borrow_mut().take())
}

/// Take the query error a stack contained, published by the last completed
/// `queryArc` boundary, if any. The same failure is also published as a
/// contained callback failure; this channel says that a stack child threw.
pub fn take_query_contained_throw() -> Option<String> {
    QUERY_CONTAINED_THROW.with(|slot| slot.borrow_mut().take())
}

fn signal_contained_throw(message: String) {
    CONTAINED_THROW.with(|slot| {
        slot.borrow_mut().get_or_insert(message);
    });
}

/// Whether the query in progress can no longer be trusted to have answered
/// completely: a mid-query error, a contained callback failure, a resource
/// refusal, an expired deadline, or a cancellation request.
///
/// A result cache asks this before keeping what a child answered. What an
/// interrupted query produced is what the `queryArc` boundary is about to
/// discard, and keeping it would serve the discarded answer to the next,
/// healthy query. The flags are sticky for the top-level query, so an
/// interruption anywhere earlier in it also refuses the cache - the
/// conservative side.
pub fn query_interrupted() -> bool {
    query_error_pending()
        || CALLBACK_FAILURE.with(|slot| slot.borrow().is_some())
        || budget_exhausted()
        || cancellation_observed()
        || cancellation_requested()
}

thread_local! {
    /// How many times a query on this thread has reached something no
    /// result cache may keep: a volatile pattern, a handle bound to a
    /// runtime of its own, or an incomplete raw stack query. Monotonic, so a
    /// cache reads it before and after the evaluation it is about to keep.
    static UNCACHEABLE_TOUCHES: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

fn note_uncacheable() {
    UNCACHEABLE_TOUCHES.with(|touches| touches.set(touches.get().wrapping_add(1)));
}

/// How many times a query on this thread has so far reached something a
/// result cache must not keep - see [`purity::Purity::volatile`].
///
/// The static classification is what a node reads when it is built; this is
/// what it reads around the evaluation itself, because a pattern-of-patterns
/// materialises its inner graphs only at query time, and one of those may be
/// volatile although the carrier is not. A cache that sees this move during
/// an evaluation keeps nothing from it.
pub fn uncacheable_touches() -> u64 {
    UNCACHEABLE_TOUCHES.with(|touches| touches.get())
}

fn publish_query_callback_failure(message: String) {
    QUERY_CALLBACK_FAILURE.with(|slot| *slot.borrow_mut() = Some(message));
}

fn publish_query_contained_throw(message: String) {
    QUERY_CONTAINED_THROW.with(|slot| *slot.borrow_mut() = Some(message));
}

/// A fresh top-level query starts with no contained failure to report.
fn clear_query_callback_failure() {
    QUERY_CALLBACK_FAILURE.with(|slot| *slot.borrow_mut() = None);
    QUERY_CONTAINED_THROW.with(|slot| *slot.borrow_mut() = None);
}

/// One `queryArc`-style error boundary.
///
/// The pending outer exception must survive nested queries, and any exception
/// raised inside this boundary must be discarded on unwind just as JavaScript
/// stack unwinding would discard it. Keeping that restoration in `Drop` avoids
/// poisoning the next query when a native checked-arithmetic panic is caught by
/// an embedding host.
///
/// The contained-callback-failure channel gets the same treatment: stashed on
/// entry, restored on exit, and whatever the inner query contained is
/// published for the outermost caller to take.
struct QueryErrorBoundary {
    outer: Option<String>,
    outer_callback_failure: Option<String>,
    outer_contained_throw: Option<String>,
    restored: bool,
}

impl QueryErrorBoundary {
    fn enter() -> Self {
        QUERY_ERROR_BOUNDARY_DEPTH.with(|depth| depth.set(depth.get().saturating_add(1)));
        Self {
            outer: take_query_error(),
            outer_callback_failure: CALLBACK_FAILURE.with(|slot| slot.borrow_mut().take()),
            outer_contained_throw: CONTAINED_THROW.with(|slot| slot.borrow_mut().take()),
            restored: false,
        }
    }

    fn finish(mut self) -> Option<String> {
        let inner = take_query_error();
        let inner_callback_failure = CALLBACK_FAILURE.with(|slot| slot.borrow_mut().take());
        let inner_contained_throw = CONTAINED_THROW.with(|slot| slot.borrow_mut().take());
        self.restore();
        if inner.is_some() {
            // The queryArc aborted. Discard contained failures with the
            // partial haps, including one that a nested boundary already
            // published. Report only the throw; a "kept playing" warning
            // would be false for a silent window.
            clear_query_callback_failure();
        } else {
            if let Some(failure) = inner_callback_failure {
                publish_query_callback_failure(failure);
            }
            if let Some(thrown) = inner_contained_throw {
                publish_query_contained_throw(thrown);
            }
        }
        inner
    }

    fn restore(&mut self) {
        if self.restored {
            return;
        }
        let _ = take_query_error();
        CALLBACK_FAILURE.with(|slot| *slot.borrow_mut() = None);
        CONTAINED_THROW.with(|slot| *slot.borrow_mut() = None);
        if let Some(outer) = self.outer.take() {
            signal_query_error(move || outer);
        }
        if let Some(outer) = self.outer_callback_failure.take() {
            signal_callback_failure(move || outer);
        }
        if let Some(outer) = self.outer_contained_throw.take() {
            signal_contained_throw(outer);
        }
        self.restored = true;
    }
}

impl Drop for QueryErrorBoundary {
    fn drop(&mut self) {
        self.restore();
        QUERY_ERROR_BOUNDARY_DEPTH.with(|depth| depth.set(depth.get().saturating_sub(1)));
    }
}

/// Whether a query exception has already been raised.
///
/// Callback-bearing loops use this to stop at the first throw. Continuing to
/// invoke user code would expose side effects an unwinding exception would
/// have prevented, even though `queryArc` discards all partial haps anyway.
pub(crate) fn query_error_pending() -> bool {
    QUERY_ERROR.with(|slot| slot.borrow().is_some())
}

/// Contain ordinary child errors at a stack boundary, for both event queries
/// and pattern-of-pattern resolution. Structural refusals remain query-wide.
fn query_stack_child<T>(query: impl FnOnce() -> Vec<T>) -> Vec<T> {
    if query_error_pending() {
        return Vec::new();
    }
    let haps = query();
    if budget_exhausted() || cancellation_observed() || cancellation_requested() {
        return Vec::new();
    }
    if let Some(message) = take_query_error() {
        CURRENT_HOST.with(|host| {
            if let Some(pointer) = host.get() {
                // SAFETY: installed only by `with_callback_host`, which
                // outlives this call, including nested queries.
                unsafe { &*pointer }.log_query_error(&message);
            }
        });
        if QUERY_ERROR_BOUNDARY_DEPTH.with(|depth| depth.get() > 0) {
            signal_callback_failure(|| message.clone());
            signal_contained_throw(message);
        } else {
            // Raw queries have no boundary to clear a contained failure.
            // Invalidate enclosing caches without poisoning later queries.
            note_uncacheable();
        }
        Vec::new()
    } else {
        haps
    }
}

// ---------------------------------------------------------------------------
// Hap budget.
//
// `MAX_QUERY_SPAN_CYCLES` bounds how wide one query may be. It does not bound
// how dense it is: `s("bd").ply(2000000)` covers a single cycle and still
// asks for two million haps. Nesting multiplies: `ply(1000).ply(1000)` asks
// for a million. The span guard cannot see this, because the expansion
// happens inside one cycle.
//
// The budget is charged as haps are produced, not after. A check on a finished
// `Vec<Hap>`, in the scheduler's queue or at the caller, is too late: the
// allocation it is meant to prevent has already happened.
//
// Exhaustion does not go through `signal_query_error`. That channel means
// "the query threw here", and `queryArc` turns it into an empty result. That
// is correct for a user's throwing callback and wrong for resource
// exhaustion, where an empty result looks like a pattern that produced
// nothing. Exhaustion has its own flag, which the query error boundary does
// not clear.
// ---------------------------------------------------------------------------

/// A query refused for RESOURCE reasons rather than pattern semantics.
///
/// Deliberately not a `String` and deliberately not the query-error channel:
/// this has to be impossible to mistake for "the pattern produced nothing".
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QueryLimit {
    /// More haps than [`DEFAULT_HAP_BUDGET`] were produced.
    HapBudget { budget: u64 },
    /// The query span was wider than [`MAX_QUERY_SPAN_CYCLES`].
    ///
    /// This is a resource refusal and travels on the typed channel. On the
    /// query-error channel, the `queryArc` boundary would turn it into an
    /// empty result, which looks like a pattern that produces nothing.
    QuerySpan { cycles: i128 },
    /// The JavaScript host ran out of heap.
    ///
    /// Distinct from a user throw, which `queryArc` turns into silence. An
    /// exhausted heap is a resource refusal: reporting it as an empty query
    /// would say "this pattern produces nothing" about a pattern that was
    /// never evaluated.
    HostMemory,
    /// One setup turn queued more runnable JavaScript jobs than the host will
    /// execute. This is independent of the CPU deadline: an unbounded chain of
    /// individually cheap microtasks can stay below every per-job timing
    /// sample while still monopolising the session.
    JsJobBudget { budget: usize },
    /// A bounded synchronous score or query turn found runnable JavaScript
    /// jobs.
    ///
    /// Setup evaluation has an explicit async microtask contract; score
    /// construction and query callbacks do not. Treating this host-policy
    /// refusal as an ordinary JavaScript message would let Session's Mini
    /// compatibility fallback install a different score, or let `queryArc`
    /// turn an incomplete query into plausible silence.
    JsPendingJobs,
    /// A bounded JavaScript evaluation occupied the runtime past its CPU
    /// deadline. This is a resource refusal, not a syntax/user exception.
    JsCpuDeadline { millis: u64 },
    /// A query ran past the wall-clock deadline installed around it - the
    /// live probe's ceiling, a scheduler tick's slice - in native work, with
    /// no JavaScript involved: `fill` under `struct` under `fill` spends its
    /// time in pattern recursion. `millis` is how long the deadline allowed
    /// from the moment it was installed.
    QueryDeadline { millis: u64 },
    /// A Euclidean rhythm would allocate an oversized intermediate mask.
    EuclidSteps { steps: u64, limit: u64 },
    /// A mini-notation range would materialise too many pattern nodes.
    MiniRange { elements: u64, limit: u64 },
    /// `echoWith` would allocate and transform too many delayed copies during
    /// pattern construction, before any query hap budget exists.
    EchoCopies { copies: u64, limit: u64 },
    /// `iter`/`chunk` would materialise too many rotated copies during pattern
    /// construction.
    ///
    /// Both build a `Vec` sized directly from the caller's part count, before
    /// any query hap budget exists. An unguarded `chunk(1e9)` would request a
    /// `Vec` of 10^9 patterns, and a failed allocation aborts the process.
    /// strudel.cc throws a catchable `RangeError` here and plays on, so a
    /// refusal is also the compatible behaviour.
    IterParts {
        operation: &'static str,
        parts: u64,
        limit: u64,
    },
    /// A stepwise operation would materialise too many retained or query-split
    /// entries.
    ///
    /// `minimum_entries` is exact for ordinary finite inputs and otherwise a
    /// conservative lower bound discovered by the bounded preflight. The
    /// operation is carried structurally so callers do not have to infer which
    /// constructor was refused from an error string.
    StepwiseExpansion {
        operation: &'static str,
        minimum_entries: u64,
        limit: u64,
    },
    /// A public numeric input or exact intermediate cannot be represented by
    /// the native checked `i128/i128` Fraction layer.
    NativeFraction { operation: &'static str },
    /// A pattern graph nested deeper than the query traversal can safely
    /// recurse through.
    ///
    /// `query_node` descends one native frame per wrapper, so a source that
    /// chains enough combinators (`p = p.fast(2).slow(2)` in a loop) would
    /// overflow the stack. A Rust stack overflow aborts the process: it is
    /// not a panic, `catch_unwind` never sees it, and the session cannot
    /// recover. `Pattern::query` refuses a depth above [`MAX_PATTERN_DEPTH`]
    /// instead.
    GraphDepth { depth: u32, limit: u32 },
    /// The caller asked for the query to stop.
    ///
    /// `Pattern::query` reads the cancellation flag as it recurses, so a
    /// dense query stops between nodes instead of running to completion.
    Cancelled,
}

impl std::fmt::Display for QueryLimit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::GraphDepth { depth, limit } => write!(
                f,
                "pattern graph nests {depth} levels deep, past the {limit} the \
                 query can traverse without overflowing the stack"
            ),
            Self::Cancelled => write!(f, "the query was cancelled"),
            Self::HostMemory => write!(
                f,
                "the JavaScript heap was exhausted and the query was refused. \
                 This is a resource limit, not an empty pattern."
            ),
            Self::JsJobBudget { budget } => write!(
                f,
                "the JavaScript setup queued more than {budget} runnable jobs and was refused"
            ),
            Self::JsPendingJobs => write!(
                f,
                "bounded synchronous JavaScript execution does not support runnable JavaScript jobs"
            ),
            Self::JsCpuDeadline { millis } => write!(
                f,
                "JavaScript evaluation exceeded its {millis} ms CPU deadline and was refused"
            ),
            Self::QueryDeadline { millis } => write!(
                f,
                "the query exceeded its {millis} ms deadline and was refused"
            ),
            Self::EuclidSteps { steps, limit } => write!(
                f,
                "the Euclidean rhythm requested {steps} steps, above the native limit of {limit}, and was refused. This is a resource limit, not an empty pattern."
            ),
            Self::MiniRange { elements, limit } => write!(
                f,
                "the mini range requested {elements} elements, above the native limit of {limit}, and was refused. This is a resource limit, not an empty pattern."
            ),
            Self::EchoCopies { copies, limit } => write!(
                f,
                "echoWith requested {copies} copies, above the native limit of {limit}, and was refused. This is a resource limit, not an empty pattern."
            ),
            Self::IterParts {
                operation,
                parts,
                limit,
            } => write!(
                f,
                "{operation} requested {parts} parts, above the native limit of {limit}, and was refused. This is a resource limit, not an empty pattern."
            ),
            Self::StepwiseExpansion {
                operation,
                minimum_entries,
                limit,
            } => write!(
                f,
                "{operation} would require at least {minimum_entries} stepwise entries, above the native limit of {limit}, and was refused. This is a resource limit, not an empty pattern."
            ),
            Self::NativeFraction { operation } => write!(
                f,
                "{operation} requires fraction arithmetic outside the native checked i128 range and was refused. This is a native representation limit, not an empty pattern."
            ),
            Self::QuerySpan { cycles } => write!(
                f,
                "the query span covers more than {cycles} cycles and was \
                 refused. This is a resource limit, not an empty pattern."
            ),
            Self::HapBudget { budget } => write!(
                f,
                "the query produced more than {budget} haps and was refused. \
                 This is a resource limit, not an empty pattern: the result \
                 would have been truncated, so nothing is returned instead."
            ),
        }
    }
}

impl std::error::Error for QueryLimit {}

/// Outcome of one outer `queryArc` boundary, distinguishing genuine silence
/// from a mid-query throw. The boundary turns a throw into no haps; live
/// reload must not treat that as a successful replacement of a sounding score.
#[derive(Debug, Clone)]
pub enum QueryArcOutcome {
    Haps(Vec<Hap>),
    Thrown(String),
}

/// Haps one top-level query may produce before it is refused.
///
/// Five million is far past any musical query - a 32-per-cycle pattern would
/// need 150,000 cycles to reach it - while bounding one query's haps to a few
/// hundred megabytes rather than all of memory.
pub const DEFAULT_HAP_BUDGET: u64 = 5_000_000;

/// Logical source/expansion high-water one stepwise scalar invocation or
/// top-level query may plan.
///
/// This counts the largest charged construction stage, not the sum of every
/// simultaneously live implementation buffer or its bytes. JavaScript rest
/// arguments, normalized sources, sidecars, and reserved output vectors can
/// overlap while remaining individually bounded by their checked plans.
pub const MAX_STEPWISE_ENTRIES: u64 = 16_384;

/// Alias kept for the shrink/grow boundary.
pub const MAX_STEPWISE_SEGMENTS: u64 = MAX_STEPWISE_ENTRIES;

thread_local! {
    /// Haps still allowed in the current top-level query. `None` outside one.
    static HAP_BUDGET: std::cell::Cell<Option<u64>> = const { std::cell::Cell::new(None) };
    /// How many `Pattern::query` frames are live on this thread.
    ///
    /// Counted at the boundary rather than derived from the graph because
    /// `Pattern::of` cannot see a node's children without a per-variant match,
    /// and a match that must be extended for every future combinator is a
    /// limit that will eventually be forgotten. One counter on the one
    /// recursive entry point cannot be.
    static QUERY_DEPTH: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
    /// Set when a RESOURCE limit refuses the query. Separate from
    /// `QUERY_ERROR` so the `queryArc` boundary cannot turn a refusal into a
    /// plausible silence, and typed so the caller learns which limit fired.
    static REFUSAL: std::cell::RefCell<Option<QueryLimit>> =
        const { std::cell::RefCell::new(None) };
    /// Remaining stepwise construction entries in this top-level query.
    /// `None` outside a query means the scalar body receives the same limit per
    /// invocation; nested queries share the enclosing remaining balance.
    static STEPWISE_ENTRIES_REMAINING: std::cell::Cell<Option<u64>> =
        const { std::cell::Cell::new(None) };
    /// Entries materialised after preflight. Thread-local so parallel tests do
    /// not race.
    static STEPWISE_ENTRIES_MATERIALISED: std::cell::Cell<u64> =
        const { std::cell::Cell::new(0) };
}

/// Charge a complete planned stepwise expansion before any Pattern nodes are
/// constructed or raw inputs are reified. A top-level query is cumulative; a
/// scalar body outside one is bounded independently and carries its typed
/// refusal in the returned graph.
pub fn charge_stepwise_entries(operation: &'static str, entries: u64) -> Result<(), QueryLimit> {
    STEPWISE_ENTRIES_REMAINING.with(|remaining| match remaining.get() {
        None if entries <= MAX_STEPWISE_ENTRIES => Ok(()),
        None => Err(QueryLimit::StepwiseExpansion {
            operation,
            minimum_entries: entries,
            limit: MAX_STEPWISE_ENTRIES,
        }),
        Some(left) if entries <= left => {
            remaining.set(Some(left - entries));
            Ok(())
        }
        Some(left) => {
            let used = MAX_STEPWISE_ENTRIES - left;
            let limit = QueryLimit::StepwiseExpansion {
                operation,
                minimum_entries: used.saturating_add(entries),
                limit: MAX_STEPWISE_ENTRIES,
            };
            refuse(limit.clone());
            Err(limit)
        }
    })
}

/// Progressive-segment spelling retained for the core combinators.
pub(crate) fn charge_stepwise_segments(segments: u64) -> Result<(), QueryLimit> {
    charge_stepwise_entries("shrink/grow", segments)
}

/// Publish a non-expansion stepwise refusal immediately when a patterned body
/// is running inside a query, while leaving scalar construction to carry the
/// limit in its returned graph.
pub fn mark_stepwise_refusal(limit: QueryLimit) -> QueryLimit {
    STEPWISE_ENTRIES_REMAINING.with(|remaining| {
        if remaining.get().is_some() {
            refuse(limit.clone());
        }
    });
    limit
}

pub fn note_stepwise_entries_materialised(entries: u64) {
    STEPWISE_ENTRIES_MATERIALISED.with(|count| {
        count.set(count.get().saturating_add(entries));
    });
}

pub(crate) fn note_stepwise_segments_materialised(segments: u64) {
    note_stepwise_entries_materialised(segments);
}

/// Completed expansion entries reported by stepwise constructors on this
/// thread since the last reset. This is a mutation-sensitive construction
/// counter, not an allocator or total-resident-memory measurement.
pub fn stepwise_entries_materialised() -> u64 {
    STEPWISE_ENTRIES_MATERIALISED.with(std::cell::Cell::get)
}

/// Getter kept for the progressive-segment tests.
pub fn stepwise_segments_materialised() -> u64 {
    stepwise_entries_materialised()
}

pub fn reset_stepwise_entries_materialised() {
    STEPWISE_ENTRIES_MATERIALISED.with(|count| count.set(0));
}

/// Reset kept for the progressive-segment tests.
pub fn reset_stepwise_segments_materialised() {
    reset_stepwise_entries_materialised();
}

/// Check `n` haps against the budget, returning `false` if it is exceeded.
///
/// This is a high-water bound (the largest vector any single step may hold),
/// not a running total. `query` charges every node's result, and a hap
/// produced at a leaf passes up through each of its ancestors. A running
/// total would count the same haps once per level: a 64-hap pattern four
/// levels deep would be charged 256. That measures tree depth, not size.
///
/// The high-water bound also matches the purpose: the concern is a single
/// oversized `Vec<Hap>`, and sibling vectors are bounded by depth times the
/// budget.
///
/// Callers must check the result and stop producing; the flag makes the failure
/// visible at the boundary either way.
pub(crate) fn charge_haps(n: usize) -> bool {
    HAP_BUDGET.with(|budget| match budget.get() {
        // No budget installed: not inside a top-level query (a unit test
        // calling `query` directly, say). Unbudgeted rather than zero-budget,
        // so nothing silently starts failing outside the boundary.
        None => true,
        Some(limit) if n as u64 > limit => {
            refuse(QueryLimit::HapBudget { budget: limit });
            false
        }
        Some(_) => true,
    })
}

/// Push into an accumulating result, refusing before the budget is exceeded.
///
/// Returns `false` when the caller must STOP producing. The generic charge in
/// `query` sees a node's result only once it is finished, which is too late for
/// an AMPLIFIER: a join's output is the product of its inputs, so two legal
/// operands can multiply into something enormous inside one loop. Charging per
/// push bounds the vector as it grows rather than inspecting it after completion.
pub(crate) fn push_budgeted(out: &mut Vec<Hap>, hap: Hap) -> bool {
    if !charge_haps(out.len() + 1) {
        return false;
    }
    out.push(hap);
    note_accumulated(out.len());
    true
}

/// Concatenate per-branch results, refusing before the budget is exceeded.
///
/// The `flat_map(...).collect()` producers (`stack`, `splitQueries`,
/// `chooseCycles`, `slowcat`) grow by concatenation, so each branch can be
/// within the budget while the total is not: two 100-hap siblings make 200
/// under a budget of 100. Extending under the bound stops at the first
/// branch that would exceed it.
///
/// Returns `None` once the budget is spent, so callers propagate rather than
/// truncate.
pub(crate) fn extend_budgeted(branches: impl IntoIterator<Item = Vec<Hap>>) -> Option<Vec<Hap>> {
    let mut out: Vec<Hap> = Vec::new();
    for branch in branches {
        if !charge_haps(out.len() + branch.len()) {
            return None;
        }
        out.extend(branch);
        note_accumulated(out.len());
    }
    Some(out)
}

/// Report that the JavaScript host exhausted its heap.
///
/// Uses the resource channel rather than `signal_query_error`. The latter
/// means "the query threw here", which `queryArc` turns into an empty
/// result, and an out-of-memory callback must not be reported as silence.
///
/// L2 calls this when its budget-aware allocator structurally denies a request;
/// no exception text participates in the classification.
pub fn refuse_host_memory() {
    refuse(QueryLimit::HostMemory);
}

/// Report that synchronous JavaScript crossed its elapsed-time interrupt
/// deadline.
///
/// The JavaScript bridge calls this while a core query boundary is active so
/// callback-driven patterns cannot turn an interrupt exception into ordinary
/// `queryArc` silence. Like every structural refusal, the first limit raised
/// by the query wins. The stable public variant is named `JsCpuDeadline`, but
/// this mechanism is not process CPU-time accounting.
pub fn refuse_js_cpu_deadline(millis: u64) {
    refuse(QueryLimit::JsCpuDeadline { millis });
}

/// Report that a synchronous JavaScript query left runnable jobs behind.
///
/// Query-time JavaScript has no async-job contract. This therefore travels on
/// the typed resource channel rather than the query-error channel, where it
/// would be indistinguishable from a genuinely silent pattern.
pub fn refuse_js_pending_jobs() {
    refuse(QueryLimit::JsPendingJobs);
}

/// Refuse an oversized Euclidean mask through the typed resource channel.
pub fn refuse_euclid_steps(steps: u64, limit: u64) {
    refuse(QueryLimit::EuclidSteps { steps, limit });
}

/// Charge a growing collection outside this crate, by its current length.
///
/// `charge_haps` is a high-water bound on a single vector, so the caller
/// must pass the length the vector is about to reach. A constant 1 never
/// exceeds a positive budget and would accept unlimited pushes.
///
/// Returns `false` when the caller must stop.
pub fn charge_hap_at(len_after_push: usize) -> bool {
    charge_haps(len_after_push)
}

/// Whether the current query has already exceeded its budget.
///
/// The expired deadline counts even when no refusal is recorded: every
/// `with_hap_budget` boundary swaps the refusal out, and a JS `queryArc`
/// callback crosses one on every re-entry. Without this, a deep pattern
/// keeps making re-entries after its deadline. Each one is cheap, but tens
/// of thousands of them cost seconds on the producer thread.
pub(crate) fn budget_exhausted() -> bool {
    REFUSAL.with(|slot| slot.borrow().is_some()) || DEADLINE_EXPIRED.with(|slot| slot.get())
}

/// Record a resource refusal. The first one wins, as with query errors.
pub(crate) fn refuse(limit: QueryLimit) {
    REFUSAL.with(|slot| {
        let mut slot = slot.borrow_mut();
        if slot.is_none() {
            *slot = Some(limit);
        }
    });
}

thread_local! {
    /// Haps this thread has actually MATERIALISED at a leaf.
    static HAPS_MATERIALISED: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// The largest hap vector that actually EXISTED since the last reset.
///
/// Distinct from [`haps_materialised`], which counts leaf production, and from
/// the amount charged, which records an intended size rather than a realised
/// one. This is the only observable that separates "refused while
/// concatenating" from "concatenated everything, then refused": both end in the
/// same error, and the leaves are identical in both, so nothing else can tell
/// them apart.
pub fn peak_hap_vector() -> u64 {
    PEAK_CHARGE.with(|c| c.get())
}

/// Record a vector that has just grown to `len`.
fn note_accumulated(len: usize) {
    PEAK_CHARGE.with(|c| c.set(c.get().max(len as u64)));
}

thread_local! {
    static PEAK_CHARGE: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
    /// Set while a cancellable query runs. Erased to a pointer because the flag
    /// is owned by the caller and shared with whatever thread will set it.
    static CANCEL_FLAG: std::cell::Cell<Option<*const std::sync::atomic::AtomicBool>> =
        const { std::cell::Cell::new(None) };
}

/// Run `f` with `flag` as the cancellation signal for any query it performs.
///
/// The flag is read by `query` as it recurses, so a long query stops between
/// nodes rather than at the end. Another thread sets it - a signal watcher, a
/// UI, a scheduler - which is why this takes a shared atomic rather than a
/// thread-local.
///
/// # Safety
/// The pointer never escapes: it is cleared before returning, including on
/// unwind, so it cannot be read after `flag` could die.
pub fn with_cancellation<R>(flag: &std::sync::atomic::AtomicBool, f: impl FnOnce() -> R) -> R {
    struct Guard(Option<*const std::sync::atomic::AtomicBool>);
    impl Drop for Guard {
        fn drop(&mut self) {
            CANCEL_FLAG.with(|slot| slot.set(self.0));
        }
    }
    let previous = CANCEL_FLAG.with(|slot| slot.replace(Some(flag as *const _)));
    let _guard = Guard(previous);
    // A fresh scope starts with nothing observed; the enclosing scope's
    // observation is restored when it ends.
    struct ObservedGuard(bool);
    impl Drop for ObservedGuard {
        fn drop(&mut self) {
            CANCELLATION_OBSERVED.with(|observed| observed.set(self.0));
        }
    }
    let _observed = ObservedGuard(CANCELLATION_OBSERVED.with(|observed| observed.replace(false)));
    f()
}

thread_local! {
    /// Wall-clock deadline for native query work, mirroring the JS CPU
    /// deadline. QuickJS's interrupt handler only fires while JavaScript
    /// runs; a pattern like `chop(32).slow(0.001)` spends its time in native
    /// query recursion (millions of cheap spans, each below every hap
    /// budget). Without this deadline a single save could occupy the producer
    /// for minutes: the live set goes silent while the process stays alive.
    static QUERY_DEADLINE: std::cell::Cell<Option<std::time::Instant>> =
        const { std::cell::Cell::new(None) };
    /// How long the installed deadline allowed, in milliseconds from its
    /// installation, so a refusal can say what it was measured against
    /// rather than reporting a zero it never had.
    static QUERY_DEADLINE_MILLIS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
    /// Polling counter: `Instant::now()` is far too expensive per node, but
    /// the sample has to be frequent enough that a deep recursion notices
    /// the deadline in tens of milliseconds, not seconds.
    static DEADLINE_TICK: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
    /// Sticky for the lifetime of the installed deadline.
    ///
    /// If the first expiry cleared the deadline, the rest of the query would
    /// have no bound: every later `Pattern::query` would read an empty slot
    /// and run at full speed. The refusal flag is the only other check, and
    /// `with_hap_budget` swaps it out at every non-nested boundary. A JS
    /// `queryArc` callback re-entry is such a boundary. Keeping expiry sticky
    /// ensures nested queries cannot escape the deadline.
    static DEADLINE_EXPIRED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Run `f` with a native query deadline installed (restored afterwards).
pub fn with_query_deadline<T>(deadline: std::time::Instant, f: impl FnOnce() -> T) -> T {
    struct Guard(Option<std::time::Instant>, u64);
    impl Drop for Guard {
        fn drop(&mut self) {
            QUERY_DEADLINE.with(|slot| slot.set(self.0));
            QUERY_DEADLINE_MILLIS.with(|slot| slot.set(self.1));
        }
    }
    // A NESTED install must never buy more time than the deadline that
    // contains it: the live probe's 150 ms ceiling was being replaced by the
    // 2 s JS budget installed for a callback inside it, so nothing bounded
    // the probe at all.
    let (previous, previous_millis) = QUERY_DEADLINE.with(|slot| {
        let previous = slot.get();
        let effective = match previous {
            Some(outer) => outer.min(deadline),
            None => deadline,
        };
        slot.set(Some(effective));
        let allowed = effective
            .saturating_duration_since(std::time::Instant::now())
            .as_millis()
            .min(u128::from(u64::MAX)) as u64;
        let previous_millis = QUERY_DEADLINE_MILLIS.with(|slot| slot.replace(allowed));
        (previous, previous_millis)
    });
    let _guard = Guard(previous, previous_millis);
    // A nested scope starts un-expired and re-detects within its own sampling
    // interval; the enclosing scope's verdict is restored when it ends.
    let previously_expired = DEADLINE_EXPIRED.with(|slot| slot.replace(false));
    let _expiry_guard = ExpiryGuard(previously_expired);
    DEADLINE_TICK.with(|tick| tick.set(0));
    f()
}

struct ExpiryGuard(bool);
impl Drop for ExpiryGuard {
    fn drop(&mut self) {
        DEADLINE_EXPIRED.with(|slot| slot.set(self.0));
    }
}

/// Whether the installed native query deadline has passed. Sampled every
/// 64th call so the clock read stays off the hot path.
pub(crate) fn query_deadline_expired() -> bool {
    // Once expired, STAY expired: every remaining query in this scope returns
    // empty immediately, so a deep recursion unwinds in microseconds instead
    // of continuing to build haps nobody will hear. Re-refusing is harmless
    // (the first limit wins) and is what re-arms the refusal after an inner
    // `with_hap_budget` boundary has swapped it out.
    if DEADLINE_EXPIRED.with(|slot| slot.get()) {
        refuse(QueryLimit::QueryDeadline {
            millis: QUERY_DEADLINE_MILLIS.with(|slot| slot.get()),
        });
        return true;
    }
    QUERY_DEADLINE.with(|slot| {
        let Some(deadline) = slot.get() else {
            return false;
        };
        let due = DEADLINE_TICK.with(|tick| {
            let next = tick.get().wrapping_add(1);
            tick.set(next);
            next % 64 == 0
        });
        if !due {
            return false;
        }
        if std::time::Instant::now() < deadline {
            return false;
        }
        DEADLINE_EXPIRED.with(|expired| expired.set(true));
        refuse(QueryLimit::QueryDeadline {
            millis: QUERY_DEADLINE_MILLIS.with(|slot| slot.get()),
        });
        true
    })
}

/// How long the deadline installed around the current query allowed, in
/// milliseconds from its installation; `None` outside any deadline.
pub fn installed_query_deadline_millis() -> Option<u64> {
    QUERY_DEADLINE
        .with(|slot| slot.get())
        .map(|_| QUERY_DEADLINE_MILLIS.with(|slot| slot.get()))
}

/// Whether the current query has been asked to stop.
pub(crate) fn cancellation_requested() -> bool {
    let requested = CANCEL_FLAG.with(|slot| match slot.get() {
        None => false,
        // SAFETY: set only by `with_cancellation`, which outlives this read.
        Some(flag) => unsafe { &*flag }.load(std::sync::atomic::Ordering::Relaxed),
    });
    if requested {
        CANCELLATION_OBSERVED.with(|observed| observed.set(true));
    }
    requested
}

thread_local! {
    /// Whether a cancellation has been observed inside the innermost
    /// `with_cancellation` scope. The shared flag belongs to another thread
    /// and may be cleared again at any moment, so a result cache asks this
    /// rather than the flag: a window cut short by a stop stays cut short.
    static CANCELLATION_OBSERVED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Whether a cancellation was observed inside the innermost cancellable
/// scope, whatever the shared flag says now.
pub(crate) fn cancellation_observed() -> bool {
    CANCELLATION_OBSERVED.with(|observed| observed.get())
}

/// Haps materialised on this thread since [`reset_haps_materialised`].
///
/// Tests use this to distinguish "refused" from "refused after building the
/// haps". Both return the same result, so the return value cannot tell them
/// apart. The budget is charged before production so that the allocation
/// does not happen. Without this counter the pre-charge could be deleted
/// and every budget test would still pass.
pub fn haps_materialised() -> u64 {
    HAPS_MATERIALISED.with(|c| c.get())
}

/// Reset the materialisation counter.
pub fn reset_haps_materialised() {
    HAPS_MATERIALISED.with(|c| c.set(0));
    PEAK_CHARGE.with(|c| c.set(0));
}

fn note_materialised(n: usize) {
    HAPS_MATERIALISED.with(|c| c.set(c.get().saturating_add(n as u64)));
}

/// Install a budget for `f`, restoring the previous one afterwards.
///
/// Nested top-level queries (a callback querying another pattern) share the
/// outer budget rather than getting a fresh one. Otherwise a pattern could
/// evade the limit by querying in a loop.
fn with_hap_budget<R>(budget: u64, f: impl FnOnce() -> R) -> (R, Option<QueryLimit>) {
    let (previous, nested) = HAP_BUDGET.with(|slot| {
        let previous = slot.get();
        if previous.is_none() {
            slot.set(Some(budget));
        }
        (previous, previous.is_some())
    });
    let previous_refusal = (!nested).then(|| REFUSAL.with(|f| f.borrow_mut().take()));
    let previous_stepwise = STEPWISE_ENTRIES_REMAINING.with(|slot| {
        let previous = slot.get();
        if !nested {
            slot.set(Some(MAX_STEPWISE_ENTRIES));
        }
        previous
    });
    struct Guard {
        previous_budget: Option<u64>,
        previous_refusal: Option<Option<QueryLimit>>,
        previous_stepwise: Option<u64>,
        nested: bool,
    }
    impl Drop for Guard {
        fn drop(&mut self) {
            HAP_BUDGET.with(|slot| slot.set(self.previous_budget));
            if !self.nested {
                STEPWISE_ENTRIES_REMAINING.with(|slot| slot.set(self.previous_stepwise));
                let previous = self.previous_refusal.take().flatten();
                REFUSAL.with(|slot| *slot.borrow_mut() = previous);
            }
        }
    }
    let guard = Guard {
        previous_budget: previous,
        previous_refusal,
        previous_stepwise,
        nested,
    };
    let out = f();
    let refusal = REFUSAL.with(|f| f.borrow().clone());
    drop(guard);
    (out, refusal)
}

/// The message for a hap whose value would have to be a `Pattern`.
///
/// Strudel can produce this intermediate value but cannot serialize it. Reject
/// it here with a useful message rather than exposing an opaque host value.
fn pattern_valued_hap_error() -> String {
    "a top-level pattern-of-patterns node is join-only - query a join \
     (polyJoin/stepJoin/polyBind/stepBind) rather than the intermediate node itself"
        .into()
}

/// Step slicing calls `value.withHap(...)` with no guard, and V8 words the
/// resulting `TypeError` two different ways depending on the value.
///
/// `undefined.withHap` fails on the property read; `'bd'.withHap` reads
/// `undefined` and fails on the call. Both are reachable
/// (`pure(undefined).stepJoin()` and `pure('bd').stepJoin()`), and the
/// message is observable through `try`/`catch`, so both wordings are
/// reproduced.
fn step_join_value_error(value: &Value) -> String {
    match value {
        Value::Undefined => "Cannot read properties of undefined (reading 'withHap')".to_string(),
        Value::Null => "Cannot read properties of null (reading 'withHap')".to_string(),
        _ => "x.value.withHap is not a function".to_string(),
    }
}

/// `groupHapsBy(congruent, haps)` - haps with equal wholes, first-seen order.
///
/// Congruence short-circuits only when both wholes are missing. An analog
/// hap compared with a discrete one dereferences its missing whole and
/// throws. The throw is reproduced here as a query error, because a stack
/// of an analog signal and a discrete note is easy to write.
fn collect_congruent(haps: Vec<Hap>) -> Vec<Vec<Hap>> {
    let mut groups: Vec<Vec<Hap>> = Vec::new();
    for hap in haps {
        let mut placed = false;
        for group in &mut groups {
            let other = &group[0];
            let congruent = match (hap.whole, other.whole) {
                (None, None) => true,
                // `this.whole.equals(other.whole)` with `this.whole` undefined.
                (None, Some(_)) => {
                    signal_query_error(|| {
                        "Cannot read properties of undefined (reading 'equals')".into()
                    });
                    return Vec::new();
                }
                (Some(_), None) => false,
                (Some(a), Some(b)) => a == b,
            };
            if congruent {
                group.push(hap.clone());
                placed = true;
                break;
            }
        }
        if !placed {
            groups.push(vec![hap]);
        }
    }
    groups
}

/// `haps[_mod(i, haps.length)]` - JavaScript array indexing with the
/// Euclidean `_mod`, so negative integers wrap.
///
/// `None` where JS yields `undefined`: a fractional index (`_mod` keeps the
/// fraction and `haps[2.5]` is undefined), a non-numeric value, or an empty
/// group.
fn js_index(value: &Value, len: usize) -> Option<usize> {
    if len == 0 {
        return None;
    }
    let i = value.as_f64().or_else(|| match value {
        Value::Str(s) => s.trim().parse::<f64>().ok(),
        Value::Bool(b) => Some(f64::from(u8::from(*b))),
        Value::Null => Some(0.0),
        _ => None,
    })?;
    // `_mod`'s `((i % n) + n) % n` maps a tiny negative `i` to 0;
    // `rem_euclid` would round it up to `n`. The clamp keeps it in bounds.
    let rem = crate::util::modulo_f64(i, len as f64);
    if rem.fract() != 0.0 {
        return None;
    }
    Some((rem as usize).min(len - 1))
}

/// The cumulative-weight sequence: a bind fold that concatenates each weight
/// onto a list, one pattern at a time.
///
/// Intermediate cumulative weights are ordinary `Value::List`s. Each
/// iteration uses bind's MIX whole and context order exactly.
fn query_weight_sequence(cumulative: &[Pattern], state: &State) -> Vec<Hap> {
    let spans = state.span.span_cycles();
    if !charge_haps(spans.len()) {
        return Vec::new();
    }
    note_materialised(spans.len());
    let mut current: Vec<Hap> = spans
        .into_iter()
        .map(|part| Hap::new(Some(whole_cycle(part.begin)), part, Value::List(Vec::new())))
        .collect();

    for pattern in cumulative {
        let mut next = Vec::new();
        for outer in current {
            let inners = pattern.query(&state.set_span(outer.part));
            if query_error_pending() {
                return Vec::new();
            }
            for inner in inners {
                let whole = match (outer.whole, inner.whole) {
                    (Some(a), Some(b)) => match a.intersection(&b) {
                        Some(span) => Some(span),
                        None => {
                            signal_query_error(|| "TimeSpans do not intersect".into());
                            return Vec::new();
                        }
                    },
                    _ => None,
                };
                let mut context = outer.context.clone();
                context.extend_from_slice(&inner.context);
                let mut values = match &outer.value {
                    Value::List(values) => values.clone(),
                    _ => Vec::new(),
                };
                values.push(inner.value.clone());
                if !push_budgeted(
                    &mut next,
                    Hap {
                        whole,
                        part: inner.part,
                        value: Value::List(values),
                        context,
                        ui_visuals: inner.ui_visuals | outer.ui_visuals,
                        live_controls: [0; 2],
                        slider_binding: 0,
                        pick_lookup: None,
                        scale: inner.scale.clone().or_else(|| outer.scale.clone()),
                        tags: inner.tags.clone().or_else(|| outer.tags.clone()),
                        log_line: inner.log_line.clone().or_else(|| outer.log_line.clone()),
                        edo_size: inner.edo_size.or(outer.edo_size),
                        scale_definition: inner
                            .scale_definition
                            .clone()
                            .or(outer.scale_definition.clone()),
                    },
                ) {
                    return Vec::new();
                }
            }
        }
        current = next;
    }
    current
}

/// One selected-pattern carrier produced by the complete `pat.bind(match)`
/// phase, before the final outerJoin/innerJoin queries any selected value.
struct WChooseCarrier {
    // Selection can miss (no threshold exceeds the random value). A miss is
    // merely an undefined selection; the failure is not raised until the
    // final join tries to query that carrier. Keeping the absence here
    // preserves callbacks/errors from earlier joined carriers.
    selected: Option<usize>,
    outer_whole: Option<TimeSpan>,
    part: TimeSpan,
    context: Vec<(usize, usize)>,
    ui_visuals: u64,
    scale: Option<Arc<str>>,
    edo_size: Option<f64>,
    scale_definition: Option<Arc<Value>>,
    tags: Option<Arc<[Arc<str>]>>,
    log_line: Option<Arc<str>>,
}

#[derive(Clone)]
struct PickLookupHap {
    hap: Hap,
    lookup: Arc<PickLookup>,
}

/// Query the weighted-choice node out of line.
///
/// This is deliberately not an inline match arm in `Pattern::query_node`.
/// That function recurses once per graph node; putting weighted choice's
/// nested-loop locals in its frame made an unrelated 64-layer native graph
/// overflow the CLI's main-thread stack even though it never selected this
/// variant. Keeping the large frame here makes its cost pay-for-play.
#[inline(never)]
fn query_wchoose(
    chooser: &Pattern,
    total: &Pattern,
    cumulative: &[Pattern],
    values: &[Pattern],
    inner: bool,
    state: &State,
) -> Vec<Hap> {
    debug_assert_eq!(
        cumulative.len(),
        values.len(),
        "one cumulative threshold must exist for every weighted value"
    );
    // The COMPLETE carrier pattern is built and queried first; only after
    // that succeeds does the final outerJoin/innerJoin query selected values.
    // Interleaving these phases leaks selected-value callback side effects
    // before a later weight throws.
    let random_haps = chooser.query(state);
    if query_error_pending() {
        return Vec::new();
    }
    let mut carriers = Vec::new();
    for random_hap in random_haps {
        // `pat.bind(match)`: match is queried over the random hap's part, and
        // MIX whole/context are applied below after the weighted threshold has
        // selected a value pattern.
        let match_state = state.set_span(random_hap.part);
        let weights_haps = query_weight_sequence(cumulative, &match_state);
        if query_error_pending() {
            return Vec::new();
        }
        for weights_hap in weights_haps {
            // `findpat = total.mul(r)`, queried by appLeft over the weight-list
            // hap's whole. `total` always retains the discrete pure(0)
            // structure, so scalar multiplication changes only its value.
            let find_state = state.set_span(weights_hap.whole_or_part());
            let total_haps = total.query(&find_state);
            if query_error_pending() {
                return Vec::new();
            }
            for total_hap in total_haps {
                let Some(part) = weights_hap.part.intersection(&total_hap.part) else {
                    continue;
                };
                let find = compose::compose_op(
                    compose::ComposeOp::Mul,
                    &total_hap.value,
                    &random_hap.value,
                );
                let Value::List(weights) = &weights_hap.value else {
                    // Internal invariant: `query_weight_sequence` constructs
                    // every carrier itself and always stores a Value::List.
                    debug_assert!(false, "weighted sequence carrier was not a list");
                    return Vec::new();
                };
                let selected = weights.iter().position(|weight| {
                    compose::ComposeOp::Gt
                        .apply_scalar(weight, &find)
                        .js_truthy()
                });

                // `weightspat.fmap(...).appLeft(findpat)`: function side whole,
                // find context first.
                let chooser_whole = weights_hap.whole;
                let mut chooser_context = total_hap.context.clone();
                chooser_context.extend_from_slice(&weights_hap.context);

                // `pat.bind(match)`: MIX the random hap with the
                // selected-pattern carrier.
                let outer_whole = match (random_hap.whole, chooser_whole) {
                    (Some(a), Some(b)) => match a.intersection(&b) {
                        Some(span) => Some(span),
                        None => {
                            signal_query_error(|| "TimeSpans do not intersect".into());
                            return Vec::new();
                        }
                    },
                    _ => None,
                };
                let mut outer_context = random_hap.context.clone();
                outer_context.extend(chooser_context);

                if !charge_haps(carriers.len() + 1) {
                    return Vec::new();
                }
                carriers.push(WChooseCarrier {
                    selected,
                    outer_whole,
                    part,
                    context: outer_context,
                    ui_visuals: weights_hap.ui_visuals
                        | total_hap.ui_visuals
                        | random_hap.ui_visuals,
                    scale: weights_hap
                        .scale
                        .clone()
                        .or_else(|| total_hap.scale.clone())
                        .or_else(|| random_hap.scale.clone()),
                    tags: weights_hap
                        .tags
                        .clone()
                        .or_else(|| total_hap.tags.clone())
                        .or_else(|| random_hap.tags.clone()),
                    log_line: weights_hap
                        .log_line
                        .clone()
                        .or_else(|| total_hap.log_line.clone())
                        .or_else(|| random_hap.log_line.clone()),
                    edo_size: weights_hap
                        .edo_size
                        .or(total_hap.edo_size)
                        .or(random_hap.edo_size),
                    scale_definition: weights_hap
                        .scale_definition
                        .clone()
                        .or_else(|| total_hap.scale_definition.clone())
                        .or_else(|| random_hap.scale_definition.clone()),
                });
            }
        }
    }

    // Final `.outerJoin()` for wchoose or `.innerJoin()` for wchooseCycles.
    let mut out = Vec::new();
    for carrier in carriers {
        let Some(selected) = carrier.selected else {
            // A missed selection is undefined and only dereferenced when
            // outerJoin/innerJoin reaches this carrier, so earlier carriers
            // have already run.
            signal_query_error(|| "Cannot read properties of undefined (reading 'query')".into());
            return Vec::new();
        };
        let chosen_haps = values[selected].query(&state.set_span(carrier.part));
        if query_error_pending() {
            return Vec::new();
        }
        for chosen in chosen_haps {
            let mut context = carrier.context.clone();
            context.extend_from_slice(&chosen.context);
            if !push_budgeted(
                &mut out,
                Hap {
                    whole: if inner {
                        chosen.whole
                    } else {
                        carrier.outer_whole
                    },
                    part: chosen.part,
                    pick_lookup: chosen.pick_lookup.clone(),
                    scale: chosen.scale.clone().or_else(|| carrier.scale.clone()),
                    tags: chosen.tags.clone().or_else(|| carrier.tags.clone()),
                    log_line: chosen.log_line.clone().or_else(|| carrier.log_line.clone()),
                    edo_size: chosen.edo_size.or(carrier.edo_size),
                    scale_definition: chosen
                        .scale_definition
                        .clone()
                        .or(carrier.scale_definition.clone()),
                    value: chosen.value,
                    context,
                    ui_visuals: chosen.ui_visuals | carrier.ui_visuals,
                    live_controls: chosen.live_controls,
                    slider_binding: chosen.slider_binding,
                },
            ) {
                return Vec::new();
            }
        }
    }
    out
}

/// A JavaScript lookup after `objectMap(lookup, reify)`.
///
/// Arrays keep their JavaScript `length`, their separate `Object.keys` count,
/// and only the indices that `Array.prototype.map` visited. The distinction is
/// observable for sparse arrays and arrays with enumerable non-index fields.
#[derive(Clone)]
pub enum PickLookup {
    Array {
        enumerable_len: usize,
        length: usize,
        entries: Vec<(usize, Pattern)>,
    },
    Object {
        enumerable_len: usize,
        entries: Vec<(String, Pattern)>,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PickIndexMode {
    Clamp,
    Remainder,
    Modulo,
}

impl PickLookup {
    fn attach_runtime_settings(&mut self, settings: &settings::RuntimeSettings) -> bool {
        if cancellation_requested() || query_deadline_expired() {
            return false;
        }
        match self {
            Self::Array { entries, .. } => {
                for (_, pattern) in entries {
                    if cancellation_requested() || query_deadline_expired() {
                        return false;
                    }
                    pattern.attach_runtime_settings(settings.clone());
                }
            }
            Self::Object { entries, .. } => {
                for (_, pattern) in entries {
                    if cancellation_requested() || query_deadline_expired() {
                        return false;
                    }
                    pattern.attach_runtime_settings(settings.clone());
                }
            }
        }
        true
    }

    fn reify_value(value: &Value) -> Pattern {
        match value {
            Value::Pattern(pattern) => pattern.pattern().clone(),
            value => pure(value.clone()),
        }
    }

    pub fn from_value(value: &Value) -> Option<Self> {
        match value {
            Value::List(values) => Some(Self::Array {
                enumerable_len: values.len(),
                length: values.len(),
                entries: values
                    .iter()
                    .cloned()
                    .enumerate()
                    .map(|(index, value)| (index, Self::reify_value(&value)))
                    .collect(),
            }),
            Value::Object(values) => Some(Self::Object {
                enumerable_len: values.len(),
                entries: values
                    .iter()
                    .map(|(key, value)| (key.to_string(), Self::reify_value(value)))
                    .collect(),
            }),
            // Object.entries boxes strings and exposes one enumerable
            // character per index. Other non-null primitives have no keys.
            Value::Str(value) => {
                let entries: Vec<_> = value
                    .chars()
                    .enumerate()
                    .map(|(index, value)| (index.to_string(), pure(Value::Str(value.to_string()))))
                    .collect();
                Some(Self::Object {
                    enumerable_len: entries.len(),
                    entries,
                })
            }
            Value::Bool(_)
            | Value::F64(_)
            | Value::Function(_)
            | Value::Pattern(_)
            | Value::Haps(_)
            | Value::JsValue(_) => Some(Self::Object {
                enumerable_len: 0,
                entries: Vec::new(),
            }),
            Value::Null | Value::Undefined => None,
        }
    }

    fn enumerable_len(&self) -> usize {
        match self {
            Self::Array { enumerable_len, .. } | Self::Object { enumerable_len, .. } => {
                *enumerable_len
            }
        }
    }

    fn patterns(&self) -> Box<dyn Iterator<Item = &Pattern> + '_> {
        match self {
            Self::Array { entries, .. } => Box::new(entries.iter().map(|(_, pattern)| pattern)),
            Self::Object { entries, .. } => Box::new(entries.iter().map(|(_, pattern)| pattern)),
        }
    }

    fn select(&self, value: &Value, index_mode: PickIndexMode) -> Option<&Pattern> {
        // Property access coerces the actual JavaScript selector.
        // `pure(['1']).pick(['a', 'b'])` therefore selects `b`; treating an
        // opaque JS-owned array as an ordinary object turns it into NaN and
        // silently misses. Materialise at this semantic boundary, just as the
        // value-combining operators do.
        let materialized;
        let value = if matches!(value, Value::JsValue(_)) {
            materialized = materialize_js_value(value);
            &materialized
        } else {
            value
        };
        match self {
            Self::Array {
                enumerable_len,
                length,
                entries,
            } => {
                let rounded = crate::util::js_round(pick_js_number(value));
                let numeric = match index_mode {
                    PickIndexMode::Clamp if rounded.is_nan() => f64::NAN,
                    PickIndexMode::Clamp => rounded.max(0.0).min(length.saturating_sub(1) as f64),
                    PickIndexMode::Remainder => rounded % *enumerable_len as f64,
                    PickIndexMode::Modulo => {
                        let len = *enumerable_len as f64;
                        ((rounded % len) + len) % len
                    }
                };
                // JavaScript property access turns -0 into "0". Every other
                // negative, NaN, infinite or out-of-range number misses.
                if !numeric.is_finite() || numeric < 0.0 || numeric > usize::MAX as f64 {
                    return None;
                }
                let index = numeric as usize;
                entries
                    .iter()
                    .find_map(|(candidate, pattern)| (*candidate == index).then_some(pattern))
            }
            Self::Object { entries, .. } => {
                let key = pick_property_key(value);
                entries
                    .iter()
                    .find_map(|(candidate, pattern)| (candidate == &key).then_some(pattern))
            }
        }
    }

    fn set_object(left: &Hap, right: &Hap, output: &Value) -> Option<Arc<Self>> {
        let Self::Object {
            enumerable_len,
            mut entries,
        } = Self::from_value(output)?
        else {
            return None;
        };
        // Begin with the complete visible output (including scalar promotion
        // to `{value: ...}`), then retain exact JavaScript shape metadata in
        // Object.assign order: left first, right wins.
        for source in [left, right] {
            let Some(Self::Object { entries: exact, .. }) = source
                .pick_lookup
                .as_deref()
                .cloned()
                .or_else(|| Self::from_value(&source.value))
            else {
                continue;
            };
            for (key, pattern) in exact {
                if let Some((_, current)) = entries.iter_mut().find(|(name, _)| name == &key) {
                    *current = pattern;
                }
            }
        }
        scoped_pick_lookup(Self::Object {
            enumerable_len,
            entries,
        })
    }
}

fn scoped_pick_lookup(mut lookup: PickLookup) -> Option<Arc<PickLookup>> {
    if let Some(settings) = settings::RuntimeSettings::current_requested()
        && !lookup.attach_runtime_settings(&settings)
    {
        return None;
    }
    Some(Arc::new(lookup))
}

/// True when the eager `set` lookup equals the lookup a pick derives from
/// the merged value.
///
/// `set_object` starts from the members of the merged value, then overwrites
/// each entry with the member of the left value, then of the right value.
/// The two lookups are equal when each overwriting member has the bits of
/// the merged member. `same_key` compares bits, so a zero of the other sign
/// keeps the eager path.
fn set_lookup_follows_value(value: &Value, left: &Hap, right: &Hap) -> bool {
    if left.pick_lookup.is_some()
        || right.pick_lookup.is_some()
        || value.has_js_function()
        // A string gives one lookup entry for each character.
        || matches!(left.value, Value::Str(_))
        || matches!(right.value, Value::Str(_))
    {
        return false;
    }
    let merged = |key: &str, member: &Value| {
        value
            .get(key)
            .is_some_and(|merged| same_key(merged, member))
    };
    let right_members = right.value.as_object();
    if !right_members.is_none_or(|members| members.iter().all(|(key, member)| merged(key, member)))
    {
        return false;
    }
    left.value.as_object().is_none_or(|members| {
        members.iter().all(|(key, member)| {
            right_members.is_some_and(|right| right.contains_key(key)) || merged(key, member)
        })
    })
}

fn applied_pick_lookup(
    flow: LookupFlow,
    value: &Value,
    left: &Hap,
    right: &Hap,
) -> Option<Arc<PickLookup>> {
    match flow {
        LookupFlow::Left => {
            let lookup = left.pick_lookup.as_ref()?;
            (value == &left.value).then(|| lookup.clone())
        }
        LookupFlow::Right => {
            let lookup = right.pick_lookup.as_ref()?;
            (value == &right.value).then(|| lookup.clone())
        }
        LookupFlow::Set => {
            // A pick derives this lookup from the value. Plain data needs no
            // eager copy.
            if set_lookup_follows_value(value, left, right) {
                return None;
            }
            if value == &right.value {
                right.pick_lookup.clone()
            } else {
                PickLookup::set_object(left, right, value).or_else(|| {
                    (value == &left.value)
                        .then(|| left.pick_lookup.clone())
                        .flatten()
                })
            }
        }
        LookupFlow::Infer | LookupFlow::Add => {
            if left.pick_lookup.is_none() && right.pick_lookup.is_none() {
                return None;
            }
            if value == &left.value {
                left.pick_lookup.clone()
            } else if value == &right.value {
                right.pick_lookup.clone()
            } else {
                None
            }
        }
    }
}

/// Provenance follows semantic ownership, never equality of sampled numbers.
fn applied_live_controls(flow: LookupFlow, left: &Hap, right: &Hap) -> ([u64; 2], u64) {
    match flow {
        LookupFlow::Left => (left.live_controls, left.slider_binding),
        LookupFlow::Right => (right.live_controls, right.slider_binding),
        LookupFlow::Set => {
            // Opaque objects can run score code when materialized. Refuse the
            // optional binding rather than inspecting them a second time.
            if matches!(left.value, Value::JsValue(_)) || matches!(right.value, Value::JsValue(_)) {
                return ([0; 2], 0);
            }
            if !matches!(
                (&left.value, &right.value),
                (Value::Object(_), _) | (_, Value::Object(_))
            ) {
                return (right.live_controls, right.slider_binding);
            }
            let mut bindings = left.live_controls;
            if let Value::Object(values) = &right.value {
                for (slot, key) in ["gain", "cutoff"].iter().enumerate() {
                    if values.contains_key(key) {
                        bindings[slot] = right.live_controls[slot];
                    }
                }
            }
            (bindings, 0)
        }
        LookupFlow::Infer | LookupFlow::Add => ([0; 2], 0),
    }
}

pub(crate) fn pick_js_number(value: &Value) -> f64 {
    match value {
        Value::Undefined => f64::NAN,
        Value::Null => 0.0,
        Value::Bool(value) => f64::from(*value),
        Value::F64(value) => *value,
        Value::Str(value) => pick_js_string_number(value),
        Value::List(values) => pick_js_string_number(&pick_array_string(values)),
        Value::Object(_)
        | Value::Function(_)
        | Value::Pattern(_)
        | Value::Haps(_)
        | Value::JsValue(_) => f64::NAN,
    }
}

pub(crate) fn pick_js_string_number(value: &str) -> f64 {
    // ECMAScript WhiteSpace includes U+FEFF; Rust's `str::trim` does not.
    let value = value.trim_matches(|ch: char| ch.is_whitespace() || ch == '\u{feff}');
    if value.is_empty() {
        return 0.0;
    }
    match value {
        "Infinity" | "+Infinity" => return f64::INFINITY,
        "-Infinity" => return f64::NEG_INFINITY,
        _ => {}
    }
    if value.eq_ignore_ascii_case("inf") || value.eq_ignore_ascii_case("infinity") {
        return f64::NAN;
    }
    for (prefix, radix) in [
        ("0x", 16),
        ("0X", 16),
        ("0b", 2),
        ("0B", 2),
        ("0o", 8),
        ("0O", 8),
    ] {
        if let Some(digits) = value.strip_prefix(prefix) {
            if digits.is_empty() {
                return f64::NAN;
            }
            let mut out = 0.0;
            for digit in digits.chars() {
                let Some(digit) = digit.to_digit(radix) else {
                    return f64::NAN;
                };
                out = out * f64::from(radix) + f64::from(digit);
            }
            return out;
        }
    }
    value.parse().unwrap_or(f64::NAN)
}

pub(crate) fn pick_array_string(values: &[Value]) -> String {
    values
        .iter()
        .map(|value| match value {
            Value::Undefined | Value::Null => String::new(),
            Value::List(values) => pick_array_string(values),
            Value::Object(_) => "[object Object]".into(),
            Value::Function(function) => format!("{function:?}"),
            Value::Pattern(_) => "[object Object]".into(),
            Value::F64(value) => pick_js_number_string(*value),
            other => other.show(),
        })
        .collect::<Vec<_>>()
        .join(",")
}

pub(crate) fn pick_js_number_string(value: f64) -> String {
    if !value.is_finite() || value == 0.0 {
        return Value::F64(value).show();
    }
    let absolute = value.abs();
    if !(1e-6..1e21).contains(&absolute) {
        let scientific = format!("{:e}", value);
        let (mantissa, exponent) = scientific
            .split_once('e')
            .expect("Rust scientific formatting always has an exponent");
        let exponent: i32 = exponent
            .parse()
            .expect("Rust scientific exponent is an integer");
        return format!("{mantissa}e{exponent:+}");
    }
    value.to_string()
}

fn pick_property_key(value: &Value) -> String {
    match value {
        Value::Undefined => "undefined".into(),
        Value::Null => "null".into(),
        Value::Bool(value) => value.to_string(),
        Value::F64(value) => pick_js_number_string(*value),
        Value::Str(value) => value.clone(),
        Value::List(values) => values
            .iter()
            .map(|value| match value {
                Value::Undefined | Value::Null => String::new(),
                Value::List(_) => pick_property_key(value),
                Value::Object(_) => "[object Object]".into(),
                other => other.show(),
            })
            .collect::<Vec<_>>()
            .join(","),
        Value::Object(_) => "[object Object]".into(),
        Value::Function(function) => format!("{function:?}"),
        Value::Haps(haps) => vec!["[object Object]"; haps.as_slice().len()].join(","),
        Value::Pattern(_) | Value::JsValue(_) => "[object Object]".into(),
    }
}

/// `_pick(...).{inner,outer,squeeze,reset,restart}Join()` out of line, so the
/// join's nested locals do not enlarge every recursive `query_node` frame.
#[inline(never)]
fn query_pick(
    selector: &Pattern,
    lookup: &PickLookup,
    index_mode: PickIndexMode,
    mode: JoinMode,
    state: &State,
) -> Vec<Hap> {
    let selectors = selector.query(state);
    if query_error_pending() {
        return Vec::new();
    }
    let mut out = Vec::new();
    for outer in selectors {
        if matches!(
            mode,
            JoinMode::Squeeze | JoinMode::Reset | JoinMode::Restart
        ) && outer.whole.is_none()
        {
            // All three special joins begin with `discreteOnly()`: analog
            // carriers are removed before their selected value is dereferenced.
            continue;
        }
        let Some(inner_pattern) = lookup.select(&outer.value, index_mode) else {
            // `lookup[key]` is `undefined`; the selected value becomes a hap
            // normally and fails only when the join reaches it.
            signal_query_error(|| match mode {
                JoinMode::Squeeze => {
                    "Cannot read properties of undefined (reading '_focusSpan')".into()
                }
                JoinMode::Reset | JoinMode::Restart => {
                    "Cannot read properties of undefined (reading 'late')".into()
                }
                _ => "x.value.query is not a function".into(),
            });
            return Vec::new();
        };
        match mode {
            JoinMode::Inner | JoinMode::Outer | JoinMode::Mix => {
                for inner in inner_pattern.query(&state.set_span(outer.part)) {
                    let whole = match mode {
                        JoinMode::Inner => inner.whole,
                        JoinMode::Outer => outer.whole,
                        JoinMode::Mix => match (outer.whole, inner.whole) {
                            (Some(a), Some(b)) => match a.intersection(&b) {
                                Some(span) => Some(span),
                                None => {
                                    signal_query_error(|| "TimeSpans do not intersect".into());
                                    return Vec::new();
                                }
                            },
                            _ => None,
                        },
                        _ => unreachable!(),
                    };
                    let mut context = outer.context.clone();
                    context.extend_from_slice(&inner.context);
                    if !push_budgeted(
                        &mut out,
                        Hap {
                            whole,
                            part: inner.part,
                            pick_lookup: inner.pick_lookup.clone(),
                            scale: inner.scale.clone().or_else(|| outer.scale.clone()),
                            tags: inner.tags.clone().or_else(|| outer.tags.clone()),
                            log_line: inner.log_line.clone().or_else(|| outer.log_line.clone()),
                            edo_size: inner.edo_size.or(outer.edo_size),
                            scale_definition: inner
                                .scale_definition
                                .clone()
                                .or(outer.scale_definition.clone()),
                            value: inner.value,
                            context,
                            ui_visuals: inner.ui_visuals | outer.ui_visuals,
                            live_controls: inner.live_controls,
                            slider_binding: inner.slider_binding,
                        },
                    ) {
                        return Vec::new();
                    }
                }
            }
            JoinMode::Squeeze => {
                let Some(outer_whole) = outer.whole else {
                    continue;
                };
                let focused = inner_pattern.focus_span(outer.whole_or_part());
                for inner in focused.query(&state.set_span(outer.part)) {
                    let whole = match inner.whole {
                        Some(inner_whole) => match inner_whole.intersection(&outer_whole) {
                            Some(whole) => Some(whole),
                            None => continue,
                        },
                        None => None,
                    };
                    let Some(part) = inner.part.intersection(&outer.part) else {
                        continue;
                    };
                    let mut context = inner.context.clone();
                    context.extend_from_slice(&outer.context);
                    if !push_budgeted(
                        &mut out,
                        Hap {
                            whole,
                            part,
                            pick_lookup: inner.pick_lookup.clone(),
                            scale: outer.scale.clone().or_else(|| inner.scale.clone()),
                            tags: outer.tags.clone().or_else(|| inner.tags.clone()),
                            log_line: outer.log_line.clone().or_else(|| inner.log_line.clone()),
                            edo_size: outer.edo_size.or(inner.edo_size),
                            scale_definition: outer
                                .scale_definition
                                .clone()
                                .or(inner.scale_definition.clone()),
                            value: inner.value,
                            context,
                            ui_visuals: inner.ui_visuals | outer.ui_visuals,
                            live_controls: inner.live_controls,
                            slider_binding: inner.slider_binding,
                        },
                    ) {
                        return Vec::new();
                    }
                }
            }
            JoinMode::Reset | JoinMode::Restart => {
                let Some(outer_whole) = outer.whole else {
                    continue;
                };
                let shift = if mode == JoinMode::Restart {
                    outer_whole.begin
                } else {
                    outer_whole.begin.cycle_pos()
                };
                for inner in inner_pattern.late(shift).query(state) {
                    let Some(part) = inner.part.intersection(&outer.part) else {
                        continue;
                    };
                    let whole = inner
                        .whole
                        .and_then(|inner_whole| inner_whole.intersection(&outer_whole));
                    let mut context = outer.context.clone();
                    context.extend_from_slice(&inner.context);
                    if !push_budgeted(
                        &mut out,
                        Hap {
                            whole,
                            part,
                            pick_lookup: inner.pick_lookup.clone(),
                            scale: inner.scale.clone().or_else(|| outer.scale.clone()),
                            tags: inner.tags.clone().or_else(|| outer.tags.clone()),
                            log_line: inner.log_line.clone().or_else(|| outer.log_line.clone()),
                            edo_size: inner.edo_size.or(outer.edo_size),
                            scale_definition: inner
                                .scale_definition
                                .clone()
                                .or(outer.scale_definition.clone()),
                            value: inner.value,
                            context,
                            ui_visuals: inner.ui_visuals | outer.ui_visuals,
                            live_controls: inner.live_controls,
                            slider_binding: inner.slider_binding,
                        },
                    ) {
                        return Vec::new();
                    }
                }
            }
        }
        if query_error_pending() {
            return Vec::new();
        }
    }
    out
}

#[inline(never)]
fn query_patternified_pick(
    selector: &Pattern,
    lookup_pattern: &Pattern,
    index_mode: PickIndexMode,
    mode: JoinMode,
    state: &State,
) -> Vec<Hap> {
    // The general registered path: pick from each lookup-carrier hap, then
    // inner-join. Query the complete lookup carrier pattern first, then join
    // each selected result in carrier order so query errors and callback side
    // effects keep their phase.
    let mut lookups = Vec::new();
    for hap in lookup_pattern.query(state) {
        let lookup = hap
            .pick_lookup
            .clone()
            .or_else(|| match hap.value {
                Value::JsValue(reference) => {
                    host_call_pick_lookup(reference.id()).and_then(scoped_pick_lookup)
                }
                _ => None,
            })
            .or_else(|| PickLookup::from_value(&hap.value).and_then(scoped_pick_lookup));
        let Some(lookup) = lookup else {
            if budget_exhausted() || cancellation_requested() || query_deadline_expired() {
                return Vec::new();
            }
            signal_query_error(|| "Cannot convert undefined or null to object".into());
            return Vec::new();
        };
        // `lookup_pattern.query` already enforces the per-vector hap budget
        // before returning. This second carrier vector therefore cannot be
        // longer than the budget; charging the same length again would be a
        // dead check, not an independent safety boundary.
        lookups.push(PickLookupHap { hap, lookup });
    }
    if query_error_pending() {
        return Vec::new();
    }
    let mut out = Vec::new();
    for lookup_hap in lookups {
        let selected = if lookup_hap.lookup.enumerable_len() == 0 {
            Vec::new()
        } else {
            query_pick(
                selector,
                lookup_hap.lookup.as_ref(),
                index_mode,
                mode,
                &state.set_span(lookup_hap.hap.part),
            )
        };
        if query_error_pending() {
            return Vec::new();
        }
        for inner in selected {
            let mut context = lookup_hap.hap.context.clone();
            context.extend_from_slice(&inner.context);
            if !push_budgeted(
                &mut out,
                Hap {
                    whole: inner.whole,
                    part: inner.part,
                    pick_lookup: inner.pick_lookup.clone(),
                    scale: inner.scale.clone().or_else(|| lookup_hap.hap.scale.clone()),
                    tags: inner.tags.clone().or_else(|| lookup_hap.hap.tags.clone()),
                    log_line: inner
                        .log_line
                        .clone()
                        .or_else(|| lookup_hap.hap.log_line.clone()),
                    edo_size: inner.edo_size.or(lookup_hap.hap.edo_size),
                    scale_definition: inner
                        .scale_definition
                        .clone()
                        .or(lookup_hap.hap.scale_definition.clone()),
                    value: inner.value,
                    context,
                    ui_visuals: inner.ui_visuals | lookup_hap.hap.ui_visuals,
                    live_controls: inner.live_controls,
                    slider_binding: inner.slider_binding,
                },
            ) {
                return Vec::new();
            }
        }
    }
    out
}

/// One outer hap of a pattern-of-patterns, with the pattern it carries.
///
/// `inner` is `None` where the hap value is not a pattern, as in
/// `stepBind(x => 42)`. Hap values are untyped, so the value is stored and
/// fails only when a join dereferences it. Treating it as "no hap" would
/// erase the distinction between `stepBind(x => 42)`, which throws during
/// construction, and `stepBind(x => { throw })`, which constructs and
/// empties the query.
struct ResolvedHap {
    hap: Hap,
    inner: Option<Pattern>,
}

impl ResolvedHap {
    fn of(hap: Hap, inner: Pattern) -> Self {
        Self {
            hap,
            inner: Some(inner),
        }
    }

    /// `fmap` replaces the hap's value with what the callback returned, so a
    /// non-pattern return is stored in the hap. The join words its error
    /// from that value, not from the one the callback was given.
    fn from_bind(hap: Hap, result: BindResult) -> Self {
        match result {
            BindResult::Pattern(inner) => Self {
                hap: hap.with_value(|_| Value::Undefined),
                inner: Some(inner),
            },
            BindResult::Value(value) => Self {
                hap: hap.with_value(|_| value.clone()),
                inner: None,
            },
        }
    }

    /// `Err` carries the value of the first hap whose value is not a
    /// pattern. Every join then fails as a whole (the `TypeError` unwinds
    /// past the remaining haps), and the value is needed because the message
    /// V8 produces depends on it.
    fn all_patterns(resolved: Vec<Self>) -> Result<Vec<(Hap, Pattern)>, Value> {
        let mut out = Vec::with_capacity(resolved.len());
        for r in resolved {
            match r.inner {
                Some(inner) => out.push((r.hap, inner)),
                None => return Err(r.hap.value),
            }
        }
        Ok(out)
    }
}

/// Asserted against the production types.
///
/// L1 must stay ownable by the scheduler, which means the actual private
/// [`Node`] and the public [`Pattern`] must be `Send + Sync`. That is not a
/// style preference: it is what makes "no QuickJS value can be stored in a
/// pattern node" a compile-time fact rather than a convention.
///
/// The connection to QuickJS is exact. `rquickjs::Function<'js>` contains
/// `Ctx<'js>` → `NonNull<JSContext>`, and `Persistent<Function>` contains
/// `*mut JSRuntime`; both are `!Send + !Sync`. A `Node` variant holding
/// either - directly or transitively - stops being `Send + Sync`, and this
/// assertion stops compiling. `rustel-core` deliberately has no dependency
/// on rquickjs.
///
/// Callbacks are held as an opaque [`CallbackId`], an integer index resolved by
/// L2 through a thread-local host. That indirection is the whole design, and
/// this is where it is enforced.
///
/// The integration test verifies this assertion by compiling a mutation with a
/// `!Send + !Sync` payload in the real `Node`.
const _: fn() = || {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<Node>();
    assert_send_sync::<Pattern>();
    assert_send_sync::<std::sync::Arc<Node>>();
    assert_send_sync::<Value>();
    assert_send_sync::<Hap>();
};

enum Node {
    Silence,
    Pure(Value),
    /// Mutation control - never built.
    ///
    /// Stands in for a QuickJS value stored in L1: `Rc` is `!Send + !Sync`
    /// for the same reason `rquickjs::Function` is, so its presence must break
    /// the `Send + Sync` assertion above. If that assertion stops having an
    /// effect, this variant compiles.
    #[cfg(non_send_node_mutation)]
    NonSendMutation(std::rc::Rc<()>),
    /// A pattern that always fails at query time.
    ///
    /// Several `TypeError`s are reachable only once the query runs. One is
    /// `pat.polyBind(42)`, where `fmap(42)` builds and then calls a
    /// non-function. This node keeps the failure in the query phase instead
    /// of throwing at the host boundary: an expression the language can
    /// build must build here too, and it must empty the query rather than
    /// abort the evaluation.
    QueryError(&'static str),
    /// A resource refusal whose construction is valid but whose query cannot
    /// be answered within the native runtime's bound. The refusal is emitted
    /// here, inside the top-level query boundary; setting the thread-local at
    /// construction time would be cleared as stale before the query begins.
    QueryLimit(QueryLimit),
    /// Query-time transform of the query span, then of the resulting hap spans.
    /// `fast`/`slow` are expressed with these two.
    WithQueryTime(Pattern, TimeFn),
    WithHapTime(Pattern, TimeFn),
    /// `withQuerySpan` / `withHapSpan` - begin and end mapped jointly.
    WithQuerySpan(Pattern, SpanFn),
    WithQuerySpanJs(Pattern, CallbackId),
    WithHapSpan(Pattern, SpanFn),
    /// `rev` - reflect each cycle. Its own node because the reflection pivot
    /// comes from the query span's cycle, so the hap transform is not a
    /// function of the hap alone.
    Rev(Pattern),
    /// `stepJoin` - re-slices the outer cycle on every query, so it cannot be
    /// expressed as a `Join` mode.
    StepJoin(Pattern),
    /// `polyJoin` - extends each inner pattern to the outer's step count, then
    /// joins taking the outer whole. Resolved at query time so it survives
    /// wrapping.
    PolyJoin(Pattern),
    /// `arp(indices, pat)` - index into each chord.
    ///
    /// Its own node because the collect/select/unwrap pipeline's intermediate
    /// values are a `Hap[]` and then a `Hap`, neither of which `Value` holds.
    /// Doing the grouping, the index selection and the final unwrap in one
    /// place means the haps never have to be representable as values at all.
    Arp(Pattern, Pattern),
    /// Native semantics supplied by a statically linked extension.
    ///
    /// Core retains only the execution contract. Public names, authored
    /// behavior and any specialized query work remain in the owning crate.
    Extension(Box<dyn extension_node::ExtensionPatternNode>),
    /// `arpWith(func, pat)` - hand each congruent chord to a JavaScript
    /// callback, reify its result, join it over the chord span, then unwrap the
    /// returned Hap-shaped value.
    ///
    /// The callback id is opaque to L1. `Hap[]` and the callback's JavaScript
    /// values exist only at the L2 boundary; the returned pattern is native.
    ArpWith(Pattern, CallbackId),
    /// The general `register()` path for `arpWith`: the callback itself is a
    /// pattern (for example `.arpWith(f, g)` sequences two functions). Each
    /// callback-valued hap selects one ordinary [`Node::ArpWith`] over its
    /// part, then inner-joins that result.
    ArpWithPattern(Pattern, Pattern),
    /// `pure(pattern)` - a pattern whose every hap value is a known inner
    /// pattern.
    ///
    /// The top-level `pure(pattern)` form keeps a dedicated node so joins can
    /// resolve it without confusing it with ordinary nested container values.
    /// Its purity is inherited exactly: a pure inner stays pure and queryable
    /// with no host, and an impure one keeps its precise reachable set.
    /// Routing it through `PatternOfDynamic` would make it falsely opaque.
    PurePattern(Pattern),
    /// A pure JavaScript array/object whose exact lookup shape travels beside
    /// its ordinary value until pick consumes it.
    PurePickLookup(Value, PickLookup),
    /// `polyBind`/`stepBind` - the inner pattern comes from a user JavaScript
    /// callback applied to each outer hap value.
    ///
    /// Holds only the opaque id. Always impure and opaque: the callback may
    /// return a graph whose own callbacks cannot be enumerated statically, so
    /// `gc_mark` must retain conservatively.
    PatternOfJs(Pattern, CallbackId),
    /// Native value transform.
    FmapNative(Pattern, ValueFn),
    FilterValues(Pattern, ValuePredicate),
    FilterValuesJs(Pattern, CallbackId),
    /// `filterHaps` - the predicate sees the whole hap. `discreteOnly` and
    /// `onsetsOnly` are the instances the joins need.
    FilterHaps(Pattern, HapPredicate),
    FilterHapsJs(Pattern, CallbackId),
    FilterWhenJs(Pattern, CallbackId),
    SortHapsByPart(Pattern),
    /// `withHaps` with native per-hap drop: the map-then-`removeUndefineds`
    /// shape (`scale` uses it). A closure that hits a query-time throw signals
    /// through `signal_query_error` and returns `None`; the flag surfaces at
    /// the query boundary like every other query-time error.
    MapHapsNative(Pattern, HapMapFn),
    MapHapsWithState(Pattern, StateHapMapFn),
    /// One hap expands to N haps sharing its span - `voicing()`'s
    /// `stack(...notes).note().set(rest)` + outerJoin collapses to exactly
    /// this shape natively (each note covers the source hap's whole/part).
    ExpandHapsNative(Pattern, HapExpandFn),
    Collect(Pattern),
    /// `withSeed(() => n)`: rewrite `state.controls.randSeed`
    /// for the subtree, which every random signal reads. The registered
    /// `seed(n)` combinator is the constant form; function-valued `withSeed`
    /// is a JS surface the native scope does not expose.
    WithRandSeed(Pattern, f64),
    Signal(SignalFn),
    ChooseCycles(Vec<Pattern>, u32),
    /// Weighted random choice. `values` and cumulative `weights` are patterns,
    /// not construction-time scalars; `chooser` is `rand` for `wchoose` and
    /// `rand.segment(1)` for `wchooseCycles`. The boolean selects the final
    /// inner join (`true`) versus outer join (`false`).
    WChoose {
        chooser: Pattern,
        total: Pattern,
        cumulative: Vec<Pattern>,
        values: Vec<Pattern>,
        inner: bool,
    },
    /// A selector pattern choosing reified entries from a JavaScript array or
    /// object, followed by the requested join.
    Pick {
        selector: Pattern,
        lookup: PickLookup,
        index_mode: PickIndexMode,
        mode: JoinMode,
    },
    PickPatternified {
        selector: Pattern,
        lookup: Pattern,
        index_mode: PickIndexMode,
        mode: JoinMode,
    },
    Degrade(Pattern, f64, u32),
    Stack(Vec<Pattern>),
    SlowCat(Vec<Pattern>),
    /// `slowcatPrime` - cycle-skipping concatenation used by `every`/`lastOf`.
    SlowCatPrime(Vec<Pattern>),
    /// `innerJoin` over a pattern-of-patterns produced by a native function.
    InnerJoin(Pattern, ToPatternFn),
    SplitQueries(Pattern),
    /// `fastGap`: speed up, leaving a gap rather than repeating.
    FastGap(Pattern, Fraction),
    RepeatCycles(Pattern, Fraction),
    LoopAt(Pattern, Fraction),
    /// Run this pattern at an absolute cycles-per-minute rate. Unlike a
    /// plain `fast(cpm / 60)`, this divides by the scheduler's `_cps`, so
    /// `.cpm(30)` means the same thing when the session default is 0.5 CPS.
    Cpm(Pattern, Fraction),
    MidiKeys(Pattern, Arc<midi_in::InputPort>),
    /// `appLeft`: a pattern of functions applied to a pattern of values,
    /// keeping the FUNCTION side's whole. This is the fold `register()` uses
    /// when a leading argument is itself a pattern.
    AppLeft(Pattern, Pattern, CombineFn, LookupFlow),
    /// `appRight`: keeps the VALUE side's whole and structure.
    AppRight(Pattern, Pattern, CombineFn, LookupFlow),
    /// `appBoth`: both sides are queried over the SAME span and the wholes
    /// intersect. Tidal's `<*>`.
    AppBoth(Pattern, Pattern, CombineFn, LookupFlow),
    /// Appends source locations to every hap's context.
    AddContext(Pattern, Arc<Vec<(usize, usize)>>),
    /// A pattern whose values become patterns via a closure that is
    /// **statically proven pure by its return type** (`-> PurePattern`).
    /// The constructions this node has answered so far ride beside the
    /// closure; see [`ConstructionMemo`].
    PatternOfPure(Pattern, ToPurePatternFn, ConstructionMemo),
    /// A pattern whose values become patterns via an **opaque** closure. The
    /// closure may materialise anything, including JavaScript, so this is
    /// always classified impure with an incomplete reachable set.
    PatternOfDynamic(Pattern, ToPatternFn),
    /// Native metadata-aware bind. The closure receives the complete outer
    /// hap, including exact lookup shape, while `dependencies` makes every
    /// graph it may return explicit to purity/ownership analysis.
    PatternOfHap(Pattern, Vec<Pattern>, HapToPatternFn),
    /// Flatten a `PatternOf`, choosing whose whole survives.
    Join(Pattern, JoinMode),
    /// `fmap` with a USER callback. Holds only the opaque id.
    FmapJs(Pattern, CallbackId),
    /// A user-authored `new Pattern(state => …)`. Holds only the opaque id.
    JsQuery(CallbackId),
    Timeline(Pattern, Pattern, TimelineState),
}

/// Derived combinators keep the runtime of their receiver without adding a
/// wrapper node. Children with their own different runtime still rebind when
/// queried; this selection governs work performed by the parent itself, such
/// as an `fmap` callback returning a Pattern value.
fn inherited_runtime_settings(node: &Node) -> Option<settings::RuntimeSettings> {
    fn from_patterns<'a>(
        patterns: impl IntoIterator<Item = &'a Pattern>,
    ) -> Option<settings::RuntimeSettings> {
        patterns
            .into_iter()
            .find_map(|pattern| pattern.runtime_settings.clone())
    }

    match node {
        Node::Silence
        | Node::Pure(_)
        | Node::QueryError(_)
        | Node::QueryLimit(_)
        | Node::Signal(_)
        | Node::JsQuery(_) => None,
        #[cfg(non_send_node_mutation)]
        Node::NonSendMutation(_) => None,
        Node::WithQueryTime(pattern, _)
        | Node::WithHapTime(pattern, _)
        | Node::WithQuerySpan(pattern, _)
        | Node::WithQuerySpanJs(pattern, _)
        | Node::WithHapSpan(pattern, _)
        | Node::Rev(pattern)
        | Node::StepJoin(pattern)
        | Node::PolyJoin(pattern)
        | Node::ArpWith(pattern, _)
        | Node::PurePattern(pattern)
        | Node::PatternOfJs(pattern, _)
        | Node::FmapNative(pattern, _)
        | Node::FilterValues(pattern, _)
        | Node::FilterValuesJs(pattern, _)
        | Node::FilterHaps(pattern, _)
        | Node::FilterHapsJs(pattern, _)
        | Node::FilterWhenJs(pattern, _)
        | Node::SortHapsByPart(pattern)
        | Node::MapHapsNative(pattern, _)
        | Node::MapHapsWithState(pattern, _)
        | Node::ExpandHapsNative(pattern, _)
        | Node::Collect(pattern)
        | Node::WithRandSeed(pattern, _)
        | Node::Degrade(pattern, _, _)
        | Node::InnerJoin(pattern, _)
        | Node::SplitQueries(pattern)
        | Node::FastGap(pattern, _)
        | Node::RepeatCycles(pattern, _)
        | Node::LoopAt(pattern, _)
        | Node::Cpm(pattern, _)
        | Node::MidiKeys(pattern, _)
        | Node::AddContext(pattern, _)
        | Node::PatternOfPure(pattern, _, _)
        | Node::PatternOfDynamic(pattern, _)
        | Node::Join(pattern, _)
        | Node::FmapJs(pattern, _) => pattern.runtime_settings.clone(),
        Node::Extension(extension) => from_patterns(extension.children()),
        Node::Timeline(time_pattern, pattern, _) => pattern
            .runtime_settings
            .clone()
            .or_else(|| time_pattern.runtime_settings.clone()),
        Node::Arp(first, second)
        | Node::ArpWithPattern(first, second)
        | Node::PickPatternified {
            selector: first,
            lookup: second,
            ..
        }
        | Node::AppLeft(first, second, _, _)
        | Node::AppRight(first, second, _, _)
        | Node::AppBoth(first, second, _, _) => first
            .runtime_settings
            .clone()
            .or_else(|| second.runtime_settings.clone()),
        Node::PurePickLookup(_, lookup) => from_patterns(lookup.patterns()),
        Node::ChooseCycles(patterns, _)
        | Node::Stack(patterns)
        | Node::SlowCat(patterns)
        | Node::SlowCatPrime(patterns) => from_patterns(patterns),
        Node::WChoose {
            chooser,
            total,
            cumulative,
            values,
            ..
        } => chooser
            .runtime_settings
            .clone()
            .or_else(|| total.runtime_settings.clone())
            .or_else(|| from_patterns(cumulative))
            .or_else(|| from_patterns(values)),
        Node::Pick {
            selector, lookup, ..
        } => selector
            .runtime_settings
            .clone()
            .or_else(|| from_patterns(lookup.patterns())),
        Node::PatternOfHap(pattern, dependencies, _) => pattern
            .runtime_settings
            .clone()
            .or_else(|| from_patterns(dependencies)),
    }
}

impl Pattern {
    /// Every constructor routes through here, so purity is computed exactly
    /// once per node and can never be forgotten by a new combinator.
    fn of(node: Node) -> Self {
        LIVE_NODES.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let purity = compute_purity(&node);
        let runtime_settings = inherited_runtime_settings(&node);
        Pattern {
            node: Arc::new(node),
            steps: None,
            pure_loc: None,
            purity,
            runtime_settings,
        }
    }

    /// The cached classification. Impurity is monotonic: once impure, a
    /// pattern never becomes pure again.
    pub fn purity(&self) -> &purity::Purity {
        &self.purity
    }

    pub fn is_pure(&self) -> bool {
        !self.purity.impure
    }

    /// Whether a result cache may keep what this pattern answers: no
    /// JavaScript, and nothing reachable that answers differently to the
    /// same query. See [`purity::Purity::volatile`].
    pub fn is_cacheable(&self) -> bool {
        !self.purity.impure && !self.purity.volatile
    }

    /// This handle, declared volatile - see [`purity::Purity::volatile`].
    ///
    /// On the handle rather than as a node: every constructor reads its
    /// children's cached classification from their handles, so the mark
    /// propagates upward exactly as impurity does.
    pub fn mark_volatile(&self) -> Self {
        let mut pattern = self.clone();
        pattern.purity.volatile = true;
        pattern
    }

    /// Callbacks reachable from this subtree - what L2's `gc_mark` marks.
    pub fn reachable_callbacks(&self) -> &[CallbackId] {
        &self.purity.reachable
    }

    /// Upgrade to the type the tight-lookahead path accepts. `None` for an
    /// impure graph, so the guarantee is enforced by the type system.
    pub fn as_pure_pattern(&self) -> Option<purity::PurePattern> {
        if self.is_pure() {
            Some(purity::PurePattern(self.clone()))
        } else {
            None
        }
    }

    /// Exact graph-handle comparison for callback-IR differential checks.
    ///
    /// This is intentionally stronger than output equivalence: the initial
    /// identity IR is valid only when its JavaScript fallback returns the
    /// callback argument unchanged, including metadata and runtime ownership.
    #[doc(hidden)]
    pub fn same_graph_handle(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.node, &other.node)
            && self.steps == other.steps
            && self.pure_loc == other.pure_loc
            && self.same_runtime_owner(other)
    }

    fn same_runtime_owner(&self, other: &Self) -> bool {
        match (&self.runtime_settings, &other.runtime_settings) {
            (Some(left), Some(right)) => left.same_handle(right),
            (None, None) => true,
            _ => false,
        }
    }

    pub fn with_steps(mut self, steps: Option<Fraction>) -> Self {
        self.steps = steps;
        self
    }

    /// Attach the module state that must be active whenever this graph is
    /// queried outside its owning runtime. An existing owner is preserved.
    #[doc(hidden)]
    pub fn with_runtime_settings(mut self, settings: settings::RuntimeSettings) -> Self {
        self.attach_runtime_settings(settings);
        self
    }

    /// Attach an owner without allocating a graph wrapper. The innermost owner
    /// wins, matching nested runtime scopes while flattening repeated exports.
    pub(crate) fn attach_runtime_settings(&mut self, settings: settings::RuntimeSettings) {
        if self.runtime_settings.is_none() {
            self.runtime_settings = Some(settings);
        }
    }

    /// Select the lexical owner of a newly returned root while leaving the
    /// owners already carried by its child graph intact.
    pub(crate) fn force_runtime_settings(&mut self, settings: settings::RuntimeSettings) {
        self.runtime_settings = Some(settings);
    }

    /// The query. Everything else is built on this.
    pub fn query(&self, state: &State) -> Vec<Hap> {
        let outermost = QUERY_DEPTH.with(|depth| depth.get() == 0);
        if self.purity.volatile {
            note_uncacheable();
        }
        match &self.runtime_settings {
            Some(settings) => {
                let crosses_runtime = !settings.is_current_requested();
                if crosses_runtime {
                    // The child binds a runtime of its own, whose snapshot
                    // can move without the enclosing one changing.
                    note_uncacheable();
                }
                settings.with(|| {
                    self.query_with_bound_runtime(
                        state,
                        (outermost || crosses_runtime).then_some(settings),
                    )
                })
            }
            None => {
                let metadata_owner = if outermost {
                    settings::RuntimeSettings::current_requested()
                } else {
                    None
                };
                self.query_with_bound_runtime(state, metadata_owner.as_ref())
            }
        }
    }

    fn query_with_bound_runtime(
        &self,
        state: &State,
        metadata_owner: Option<&settings::RuntimeSettings>,
    ) -> Vec<Hap> {
        // Restores the counter however this frame leaves - including on a
        // panic, so a caught query error cannot leave the thread's depth
        // permanently charged and refuse every later query.
        struct DepthGuard;
        impl Drop for DepthGuard {
            fn drop(&mut self) {
                QUERY_DEPTH.with(|depth| depth.set(depth.get().saturating_sub(1)));
            }
        }
        let depth = QUERY_DEPTH.with(|depth| {
            let next = depth.get().saturating_add(1);
            depth.set(next);
            next
        });
        let _guard = DepthGuard;
        if depth > MAX_PATTERN_DEPTH {
            refuse(QueryLimit::GraphDepth {
                depth,
                limit: MAX_PATTERN_DEPTH,
            });
            return Vec::new();
        }
        // Every node's result is charged, and an exhausted budget short-circuits
        // the recursion so it unwinds immediately rather than continuing to
        // build vectors nobody will read.
        //
        // Charging here rather than at the caller is the whole point: a check on
        // the finished timeline runs after the allocation it exists to prevent.
        // Combined with `MAX_QUERY_SPAN_CYCLES`, which bounds what a single leaf
        // can produce in one pass, no unbounded `Vec<Hap>` can form.
        if budget_exhausted() || cancellation_requested() || query_deadline_expired() {
            return Vec::new();
        }
        let mut haps = self.query_node(state);
        if let Some(settings) = metadata_owner {
            for hap in &mut haps {
                if !hap.value.attach_runtime_settings(settings, 0) {
                    return Vec::new();
                }
            }
        }
        // Already built by the time we see it - recorded so a producer that
        // grows without an incremental check is visible as the vector it made.
        note_accumulated(haps.len());
        if !charge_haps(haps.len()) {
            return Vec::new();
        }
        haps
    }

    fn query_node(&self, state: &State) -> Vec<Hap> {
        match &*self.node {
            Node::Silence => Vec::new(),

            Node::QueryError(message) => {
                signal_query_error(|| (*message).into());
                Vec::new()
            }

            Node::QueryLimit(limit) => {
                refuse(limit.clone());
                Vec::new()
            }

            // One hap per cycle, each aligned to its whole cycle.
            Node::Pure(v) | Node::PurePickLookup(v, _) => {
                // Charged before the haps are built. This is the one node that
                // can produce a large vector in a single pass (one hap per
                // cycle, up to `MAX_QUERY_SPAN_CYCLES` of them), so charging it
                // afterwards, like the generic wrapper in `query` does, would
                // let a million haps exist first. Everything else grows by
                // combining smaller results, which the wrapper catches.
                let spans = state.span.span_cycles();
                if !charge_haps(spans.len()) {
                    return Vec::new();
                }
                note_materialised(spans.len());
                let lookup = match &*self.node {
                    Node::PurePickLookup(_, _) => {
                        let Some(lookup) = self.as_pick_lookup().map(Arc::new) else {
                            return Vec::new();
                        };
                        Some(lookup)
                    }
                    _ => None,
                };
                // The one node that can spend seconds in a single loop. Poll
                // the stop signals while it builds: without this, SIGTERM
                // during `s("bd*2000").queryArc(0, 500)` waited out the whole
                // construction even though every recursive node would have
                // unwound within milliseconds.
                let mut haps = Vec::with_capacity(spans.len());
                for subspan in spans {
                    if cancellation_requested() || query_deadline_expired() {
                        return Vec::new();
                    }
                    let hap = Hap::new(Some(whole_cycle(subspan.begin)), subspan, v.clone());
                    match &lookup {
                        Some(lookup) => haps.push(hap.with_pick_lookup(Arc::clone(lookup))),
                        None => haps.push(hap),
                    }
                }
                haps
            }

            Node::WithQueryTime(pat, f) => {
                let s = state.with_span(|sp| sp.with_time(|t| f(t)));
                pat.query(&s)
            }

            Node::WithHapTime(pat, f) => pat
                .query(state)
                .into_iter()
                .map(|h| h.with_span(|sp| sp.with_time(|t| f(t))))
                .collect(),

            Node::WithQuerySpan(pat, f) => pat.query(&state.with_span(|sp| f(sp))),

            Node::WithQuerySpanJs(pat, id) => {
                let span = host_call_span_transform(*id, state.span);
                if query_error_pending() {
                    Vec::new()
                } else {
                    pat.query(&state.set_span(span))
                }
            }

            Node::WithHapSpan(pat, f) => pat
                .query(state)
                .into_iter()
                .map(|h| h.with_span(|sp| f(sp)))
                .collect(),

            // Mirror the query span within its cycle, query, mirror the haps
            // back. The endpoint swap after mirroring is essential - mirroring
            // alone leaves begin > end - and `reflect` uses the original query
            // span's cycle in both places, which is why this cannot be built
            // from `withHapSpan`.
            Node::Rev(pat) => {
                let cycle = state.span.begin.sam();
                let next_cycle = state.span.begin.next_sam();
                let reflect = |sp: &TimeSpan| {
                    let b = cycle.add(next_cycle.sub(sp.begin));
                    let e = cycle.add(next_cycle.sub(sp.end));
                    TimeSpan::new(e, b)
                };
                pat.query(&state.set_span(reflect(&state.span)))
                    .into_iter()
                    .map(|h| h.with_span(reflect))
                    .collect()
            }

            Node::FmapNative(pat, f) => pat
                .query(state)
                .into_iter()
                .map(|h| h.with_value(|v| f(v)))
                .collect(),

            Node::FilterValues(pat, predicate) => pat
                .query(state)
                .into_iter()
                .filter(|hap| predicate(&hap.value))
                .collect(),

            Node::FilterValuesJs(pat, id) => {
                let mut output = Vec::new();
                for hap in pat.query(state) {
                    // A throwing predicate CONTAINS the failure and keeps the
                    // hap (fail open): one broken filter must not stop the set.
                    let keep = host_call_value_predicate(*id, &hap.value);
                    if query_error_pending() {
                        return Vec::new();
                    }
                    if keep {
                        output.push(hap);
                    }
                }
                output
            }

            Node::FilterHaps(pat, predicate) => pat
                .query(state)
                .into_iter()
                .filter(|h| predicate(h))
                .collect(),

            Node::FilterHapsJs(pat, id) => {
                let mut output = Vec::new();
                for hap in pat.query(state) {
                    // A throwing predicate CONTAINS the failure and keeps the
                    // hap (fail open): one broken filter must not stop the set.
                    let keep = host_call_hap_predicate(*id, &hap);
                    if query_error_pending() {
                        return Vec::new();
                    }
                    if keep {
                        output.push(hap);
                    }
                }
                output
            }

            Node::FilterWhenJs(pat, id) => {
                let mut output = Vec::new();
                for hap in pat.query(state) {
                    let Some(whole) = hap.whole else {
                        signal_query_error(|| {
                            "Cannot read properties of undefined (reading 'begin')".into()
                        });
                        return Vec::new();
                    };
                    let keep = host_call_time_predicate(*id, whole.begin);
                    if query_error_pending() {
                        return Vec::new();
                    }
                    if keep {
                        output.push(hap);
                    }
                }
                output
            }

            Node::SortHapsByPart(pat) => sort_haps_by_part(pat.query(state)),

            Node::MapHapsNative(pat, f) => {
                pat.query(state).into_iter().filter_map(|h| f(&h)).collect()
            }

            Node::MapHapsWithState(pat, f) => pat
                .query(state)
                .into_iter()
                .filter_map(|hap| f(state, &hap))
                .collect(),

            Node::ExpandHapsNative(pat, f) => {
                pat.query(state).into_iter().flat_map(|h| f(&h)).collect()
            }

            Node::Collect(pat) => collect_congruent(pat.query(state))
                .into_iter()
                .filter_map(|group| {
                    let carrier = group.first()?;
                    let ui_visuals = group
                        .iter()
                        .fold(0_u64, |visuals, hap| visuals | hap.ui_visuals);
                    // Every span in the group, not the carrier's alone: a
                    // collected chord is heard as one thing and was typed
                    // as several, and an editor lighting only the first of
                    // them says a note sounded that did not.
                    let mut context: Vec<(usize, usize)> = group
                        .iter()
                        .flat_map(|hap| hap.context.iter().copied())
                        .collect();
                    context.sort_unstable();
                    context.dedup();
                    Some(
                        Hap::new(
                            carrier.whole,
                            carrier.part,
                            Value::Haps(value::HapList::new(group)),
                        )
                        .with_ui_visuals_context(ui_visuals)
                        .with_context(context),
                    )
                })
                .collect(),

            // Extension implementations may rewrite public Hap values. They
            // have no provenance contract, so only later direct controls bind.
            Node::Extension(extension) => extension
                .query(state)
                .into_iter()
                .map(Hap::without_live_controls)
                .collect(),

            // Out of line: locals in query_node's frame multiply across the
            // recursion (see query_wchoose's note; the 64-layer fast/slow
            // control aborts otherwise).
            Node::WithRandSeed(pat, seed) => query_with_rand_seed(pat, *seed, state),

            Node::Signal(signal) => vec![Hap::new(None, state.span, signal(state))],

            Node::LoopAt(pat, factor) => {
                let cps = state
                    .controls
                    .get("_cps")
                    .and_then(Value::as_f64)
                    .unwrap_or(0.5);
                combinators::loop_at_cps(pat, *factor, cps).query(state)
            }
            Node::Cpm(pat, cpm) => {
                let scheduler_cps = state
                    .controls
                    .get("_cps")
                    .and_then(Value::as_f64)
                    .filter(|cps| cps.is_finite() && *cps > 0.0)
                    .unwrap_or(0.5);
                let target_cps = cpm.to_f64() / 60.0;
                pat.fast(Fraction::from_f64(target_cps / scheduler_cps).unwrap_or(Fraction::ZERO))
                    .query(state)
            }

            Node::MidiKeys(lengths, port) => query_midi_keys(lengths, port, state),

            Node::ChooseCycles(patterns, seed) => {
                if patterns.is_empty() {
                    return Vec::new();
                }
                let random_seed = state
                    .controls
                    .get("randSeed")
                    .and_then(Value::as_f64)
                    .unwrap_or(0.0);
                let branches = state.span.span_cycles().into_iter().map(|span| {
                    // `segment(1)` is `struct(pure(true))`, whose appRight
                    // queries `rand` over the pure hap's whole (a full
                    // cycle), so the signal is sampled at the cycle start.
                    // A sample at the sub-span's begin would make the choice
                    // vary within a cycle: `vowel("[a|e|i|o|u]")` would pick
                    // a different vowel per event instead of per cycle.
                    let sample_time = span.begin.sam().to_f64() + 0.0003 * f64::from(*seed);
                    let random = rng::rand_at_time(sample_time, random_seed);
                    let index =
                        ((random * patterns.len() as f64).floor() as usize).min(patterns.len() - 1);
                    patterns[index].query(&state.set_span(span))
                });
                extend_budgeted(branches).unwrap_or_default()
            }

            Node::WChoose {
                chooser,
                total,
                cumulative,
                values,
                inner,
            } => query_wchoose(chooser, total, cumulative, values, *inner, state),

            Node::Pick {
                selector,
                lookup,
                index_mode,
                mode,
            } => query_pick(selector, lookup, *index_mode, *mode, state),
            Node::PickPatternified {
                selector,
                lookup,
                index_mode,
                mode,
            } => query_patternified_pick(selector, lookup, *index_mode, *mode, state),

            Node::Degrade(pattern, amount, seed) => {
                let random_seed = state
                    .controls
                    .get("randSeed")
                    .and_then(Value::as_f64)
                    .unwrap_or(0.0);
                pattern
                    .query(state)
                    .into_iter()
                    .filter(|hap| {
                        let time = hap.whole_or_part().begin.to_f64() + 0.0003 * f64::from(*seed);
                        rng::rand_at_time(time, random_seed) > *amount
                    })
                    .collect()
            }

            Node::Stack(pats) => {
                extend_budgeted(pats.iter().map(|p| query_stack_child(|| p.query(state))))
                    .unwrap_or_default()
            }

            // One constituent per cycle. `offset` keeps constituent cycles
            // contiguous: the fourth cycle of a three-pattern slowcat is the
            // SECOND cycle of the first pattern, not the fourth. The wrapping
            // `split_queries` keeps a multi-cycle query from being served by
            // a single constituent. A shifted time outside the native range
            // refuses as `NativeFraction { operation: "slowcat" }`.
            Node::SlowCat(pats) => {
                if pats.is_empty() {
                    return Vec::new();
                }
                let n = pats.len() as i128;
                let cycle = state.span.begin.sam().numer();
                let pat = &pats[cycle.rem_euclid(n) as usize];

                // `cycle - floor(cycle / n)` lies between 0 and `cycle` and is
                // never `i128::MIN`, so it and its negation fit.
                let offset = Fraction::int(cycle - cycle.div_euclid(n));
                let shift = |span: TimeSpan, by: Fraction| {
                    Some(TimeSpan::new(
                        span.begin.checked_add(by)?,
                        span.end.checked_add(by)?,
                    ))
                };
                let refused = || {
                    refuse(QueryLimit::NativeFraction {
                        operation: "slowcat",
                    });
                    Vec::new()
                };
                let Some(inner_span) = shift(state.span, offset.neg()) else {
                    return refused();
                };
                let mapped: Option<Vec<Hap>> = pat
                    .query(&state.set_span(inner_span))
                    .into_iter()
                    .map(|mut hap| {
                        hap.part = shift(hap.part, offset)?;
                        if let Some(whole) = hap.whole {
                            hap.whole = Some(shift(whole, offset)?);
                        }
                        Some(hap)
                    })
                    .collect();
                mapped.unwrap_or_else(refused)
            }

            // innerBind: the inner hap's whole survives; context locations
            // concatenate.
            Node::InnerJoin(pat, f) => {
                let mut out = Vec::new();
                for outer in pat.query(state) {
                    let inner_pat = f(&outer.value);
                    for inner in inner_pat.query(&state.set_span(outer.part)) {
                        let mut ctx = outer.context.clone();
                        ctx.extend_from_slice(&inner.context);
                        if !push_budgeted(
                            &mut out,
                            Hap {
                                whole: inner.whole, // innerJoin: (_, b) => b
                                part: inner.part,
                                pick_lookup: joined_pick_lookup(&outer, &inner),
                                scale: inner.scale.clone().or_else(|| outer.scale.clone()),
                                tags: inner.tags.clone().or_else(|| outer.tags.clone()),
                                log_line: inner.log_line.clone().or_else(|| outer.log_line.clone()),
                                edo_size: inner.edo_size.or(outer.edo_size),
                                scale_definition: inner
                                    .scale_definition
                                    .clone()
                                    .or(outer.scale_definition.clone()),
                                value: inner.value.clone(),
                                context: ctx,
                                ui_visuals: inner.ui_visuals | outer.ui_visuals,
                                live_controls: inner.live_controls,
                                slider_binding: inner.slider_binding,
                            },
                        ) {
                            return Vec::new();
                        }
                    }
                }
                out
            }

            // Euclidean cycle index: negative cycles wrap.
            Node::SlowCatPrime(pats) => {
                if pats.is_empty() {
                    return Vec::new();
                }
                let n = pats.len() as i128;
                let index = state.span.begin.floor().numer().rem_euclid(n);
                pats[index as usize].query(state)
            }

            // See `Pattern::fast_gap`.
            Node::FastGap(pat, factor) => {
                let factor = *factor;
                let span = state.span;
                let cycle = span.begin.sam();
                let one = Fraction::ONE;
                // Checked on the way IN as well as out: the span scaled by the
                // factor overflows just as readily as the haps scaled back by
                // it, and only fixing the mapping left this side still able to
                // panic.
                let Some(inner_span) = (|| {
                    let bpos = span.begin.checked_sub(cycle)?.checked_mul(factor)?.min(one);
                    let epos = span.end.checked_sub(cycle)?.checked_mul(factor)?.min(one);
                    // `if (bpos >= 1) return undefined` - drops zero-width
                    // queries landing at the start of the next cycle.
                    if bpos >= one {
                        return None;
                    }
                    Some(TimeSpan::new(
                        cycle.checked_add(bpos)?,
                        cycle.checked_add(epos)?,
                    ))
                })() else {
                    // An unrepresentable span and the empty `bpos >= 1` case
                    // both land here. The empty case is ordinary and silent;
                    // an overflow additionally records a refusal below, so the
                    // two stay distinguishable to the caller.
                    if span
                        .begin
                        .checked_sub(cycle)
                        .and_then(|d| d.checked_mul(factor))
                        .is_none()
                    {
                        refuse(QueryLimit::NativeFraction {
                            operation: "fastGap",
                        });
                    }
                    return Vec::new();
                };
                // The division by `factor` can leave the representable range:
                // a very small factor scales a span into numerators no i128
                // can hold. An unchecked overflow would panic, which ends the
                // set, not the query.
                //
                // Refuse as a resource limit instead, as the stepwise family
                // does when its exact arithmetic leaves the native range.
                // Refuse the whole node rather than drop the offending hap:
                // a partial span plays wrong events with no error, while a
                // refusal is visible and the last good score keeps playing.
                let mapped: Option<Vec<Hap>> = pat
                    .query(&state.set_span(inner_span))
                    .into_iter()
                    .map(|hap| Self::fast_gap_unmap_hap(&hap, factor))
                    .collect();
                match mapped {
                    Some(haps) => haps,
                    None => {
                        refuse(QueryLimit::NativeFraction {
                            operation: "fastGap",
                        });
                        Vec::new()
                    }
                }
            }

            Node::RepeatCycles(pattern, count) => {
                let cycle = state.span.begin.sam();
                let source_cycle = cycle.div(*count).sam();
                let delta = cycle.sub(source_cycle);
                let shifted = state.with_span(|span| span.with_time(|time| time.sub(delta)));
                pattern
                    .query(&shifted)
                    .into_iter()
                    .map(|hap| hap.with_span(|span| span.with_time(|time| time.add(delta))))
                    .collect()
            }

            // Function haps drive: values are queried over each function
            // hap's wholeOrPart, and the function side's whole wins.
            Node::AppLeft(pat_func, pat_val, apply, lookup_flow) => {
                let mut out = Vec::new();
                for hf in pat_func.query(state) {
                    let inner = pat_val.query(&state.set_span(hf.whole_or_part()));
                    for hv in inner {
                        if let Some(part) = hf.part.intersection(&hv.part) {
                            let mut ctx = hv.context.clone();
                            ctx.extend_from_slice(&hf.context);
                            let value = apply(&hf.value, &hv.value);
                            let pick_lookup = applied_pick_lookup(*lookup_flow, &value, &hf, &hv);
                            let live_controls = applied_live_controls(*lookup_flow, &hf, &hv);
                            if budget_exhausted()
                                || cancellation_requested()
                                || query_deadline_expired()
                            {
                                return Vec::new();
                            }
                            if !push_budgeted(
                                &mut out,
                                Hap {
                                    whole: hf.whole,
                                    part,
                                    value,
                                    context: ctx,
                                    ui_visuals: hf.ui_visuals | hv.ui_visuals,
                                    live_controls: live_controls.0,
                                    slider_binding: live_controls.1,
                                    pick_lookup,
                                    scale: hf.scale.clone().or_else(|| hv.scale.clone()),
                                    tags: hf.tags.clone().or_else(|| hv.tags.clone()),
                                    log_line: hf.log_line.clone().or_else(|| hv.log_line.clone()),
                                    edo_size: hf.edo_size.or(hv.edo_size),
                                    scale_definition: hf
                                        .scale_definition
                                        .clone()
                                        .or(hv.scale_definition.clone()),
                                },
                            ) {
                                return Vec::new();
                            }
                        }
                    }
                }
                out
            }

            // appRight: loop value haps first, query function haps in each
            // value whole, and preserve the value side's whole.
            Node::AppRight(pat_func, pat_val, apply, lookup_flow) => {
                let mut out = Vec::new();
                for hv in pat_val.query(state) {
                    let funcs = pat_func.query(&state.set_span(hv.whole_or_part()));
                    for hf in funcs {
                        if let Some(part) = hf.part.intersection(&hv.part) {
                            let mut ctx = hv.context.clone();
                            ctx.extend_from_slice(&hf.context);
                            let value = apply(&hf.value, &hv.value);
                            let pick_lookup = applied_pick_lookup(*lookup_flow, &value, &hf, &hv);
                            let live_controls = applied_live_controls(*lookup_flow, &hf, &hv);
                            if budget_exhausted()
                                || cancellation_requested()
                                || query_deadline_expired()
                            {
                                return Vec::new();
                            }
                            if !push_budgeted(
                                &mut out,
                                Hap {
                                    whole: hv.whole,
                                    part,
                                    value,
                                    context: ctx,
                                    ui_visuals: hf.ui_visuals | hv.ui_visuals,
                                    live_controls: live_controls.0,
                                    slider_binding: live_controls.1,
                                    pick_lookup,
                                    scale: hf.scale.clone().or_else(|| hv.scale.clone()),
                                    tags: hf.tags.clone().or_else(|| hv.tags.clone()),
                                    log_line: hf.log_line.clone().or_else(|| hv.log_line.clone()),
                                    edo_size: hf.edo_size.or(hv.edo_size),
                                    scale_definition: hf
                                        .scale_definition
                                        .clone()
                                        .or(hv.scale_definition.clone()),
                                },
                            ) {
                                return Vec::new();
                            }
                        }
                    }
                }
                out
            }

            // Both sides are queried over the same span; parts intersect, and
            // `appBoth`'s whole is `intersection_e` - undefined if either side
            // is analog.
            Node::AppBoth(pat_func, pat_val, apply, lookup_flow) => {
                let funcs = pat_func.query(state);
                let vals = pat_val.query(state);
                let mut out = Vec::new();
                for hf in &funcs {
                    for hv in &vals {
                        let Some(part) = hf.part.intersection(&hv.part) else {
                            continue;
                        };
                        let whole = match (hf.whole, hv.whole) {
                            (Some(a), Some(b)) => match a.intersection(&b) {
                                Some(w) => Some(w),
                                // `intersection_e` THROWS; see `queryArc`.
                                None => {
                                    signal_query_error(|| "TimeSpans do not intersect".into());
                                    continue;
                                }
                            },
                            _ => None,
                        };
                        let mut ctx = hv.context.clone();
                        ctx.extend_from_slice(&hf.context);
                        let value = apply(&hf.value, &hv.value);
                        let pick_lookup = applied_pick_lookup(*lookup_flow, &value, hf, hv);
                        let live_controls = applied_live_controls(*lookup_flow, hf, hv);
                        if budget_exhausted()
                            || cancellation_requested()
                            || query_deadline_expired()
                        {
                            return Vec::new();
                        }
                        if !push_budgeted(
                            &mut out,
                            Hap {
                                whole,
                                part,
                                value,
                                context: ctx,
                                ui_visuals: hf.ui_visuals | hv.ui_visuals,
                                live_controls: live_controls.0,
                                slider_binding: live_controls.1,
                                pick_lookup,
                                scale: hf.scale.clone().or_else(|| hv.scale.clone()),
                                tags: hf.tags.clone().or_else(|| hv.tags.clone()),
                                log_line: hf.log_line.clone().or_else(|| hv.log_line.clone()),
                                edo_size: hf.edo_size.or(hv.edo_size),
                                scale_definition: hf
                                    .scale_definition
                                    .clone()
                                    .or(hv.scale_definition.clone()),
                            },
                        ) {
                            return Vec::new();
                        }
                    }
                }
                out
            }

            Node::FmapJs(pat, id) => {
                let id = *id;
                let haps = pat.query(state);
                let mut out = Vec::with_capacity(haps.len());
                for hap in haps {
                    // Every iteration enters QuickJS, and a wide query holds
                    // tens of thousands of haps: `chop(32).slow(0.001)` maps
                    // about 69k of them. With a JS heap that large each call
                    // adds GC work, so an unbounded loop can occupy the
                    // producer thread for minutes with no audio and no
                    // response to SIGINT. The check must be inside the loop:
                    // a check on the finished vector is too late.
                    if budget_exhausted() || cancellation_requested() || query_deadline_expired() {
                        return Vec::new();
                    }
                    out.push(hap.with_value(|v| host_call_value(id, v)));
                }
                out
            }

            Node::JsQuery(id) => host_call_query(*id, state),

            Node::Timeline(time_pattern, pattern, timelines) => {
                let scheduler = state.controls.get("cyclist").is_some_and(Value::js_truthy);
                let mut result = Vec::new();
                for time_hap in time_pattern.query(state) {
                    let key = pick_property_key(&time_hap.value);
                    let is_zero = matches!(&time_hap.value, Value::F64(value) if *value == 0.0);
                    let offset = if is_zero {
                        Fraction::ZERO
                    } else if let Some(offset) = timelines.get(&key) {
                        offset
                    } else {
                        let arc = time_hap.whole_or_part();
                        if !scheduler || state.span.begin < arc.midpoint() {
                            arc.begin
                        } else {
                            arc.end
                        }
                    };
                    if scheduler {
                        timelines.insert(key, offset);
                        if !is_zero {
                            let negative = Value::F64(-pick_js_number(&time_hap.value));
                            timelines.remove(&pick_property_key(&negative));
                        }
                    }
                    for hap in pattern.late(offset).query(&state.set_span(time_hap.part)) {
                        let mut context = hap.context.clone();
                        context.extend_from_slice(&time_hap.context);
                        if !push_budgeted(
                            &mut result,
                            Hap {
                                whole: hap.whole,
                                part: hap.part,
                                value: hap.value,
                                context,
                                ui_visuals: time_hap.ui_visuals | hap.ui_visuals,
                                live_controls: hap.live_controls,
                                slider_binding: hap.slider_binding,
                                pick_lookup: hap.pick_lookup,
                                scale: time_hap.scale.clone().or(hap.scale),
                                tags: time_hap.tags.clone().or(hap.tags),
                                log_line: time_hap.log_line.clone().or(hap.log_line),
                                edo_size: time_hap.edo_size.or(hap.edo_size),
                                scale_definition: time_hap
                                    .scale_definition
                                    .clone()
                                    .or(hap.scale_definition),
                            },
                        ) {
                            return Vec::new();
                        }
                    }
                }
                result
            }

            Node::AddContext(pat, locs) => pat
                .query(state)
                .into_iter()
                .map(|mut h| {
                    h.context.extend_from_slice(locs);
                    h
                })
                .collect(),

            // A `PatternOf*` queried without a join would yield PATTERN-VALUED
            // haps, which nothing downstream can serialise; the node resolves
            // only through a join. Routing the failure through
            // `signal_query_error` matters because the shape is reachable from
            // user-authored source (`pure(pure("bd")).polyJoin()` after
            // transpilation), and a live-coded mistake must not abort the
            // process.
            Node::PatternOfPure(..) | Node::PatternOfDynamic(..) | Node::PatternOfHap(..) => {
                signal_query_error(pattern_valued_hap_error);
                Vec::new()
            }

            // bindWhole: each inner pattern is queried over its outer hap's
            // part; the mode picks whose whole survives, and contexts
            // concatenate outer-then-inner.
            Node::Join(inner, mode) => {
                // Every mode queries the pattern-of-patterns over the SAME
                // state, so one resolution serves all of them. `resolve_for_join`
                // is total: a receiver that is not a pattern-of at all still
                // produces haps, whose values simply are not patterns.
                let resolved = inner.resolve_for_join(state);
                // The join dereferences each value as a pattern with no guard,
                // so a non-pattern value is a `TypeError` that the `queryArc`
                // boundary turns into no haps at all.
                let resolved = match ResolvedHap::all_patterns(resolved) {
                    Ok(resolved) => resolved,
                    Err(_) => {
                        signal_query_error(|| "x.value.query is not a function".into());
                        return Vec::new();
                    }
                };

                match mode {
                    JoinMode::Inner | JoinMode::Outer | JoinMode::Mix => {
                        let mut out = Vec::new();
                        for (a, inner_pat) in resolved {
                            for b in inner_pat.query(&state.set_span(a.part)) {
                                let whole = match mode {
                                    JoinMode::Inner => b.whole,
                                    JoinMode::Outer => a.whole,
                                    // `bind`'s whole_func: undefined if either
                                    // side is analog, else `intersection_e`.
                                    JoinMode::Mix => match (a.whole, b.whole) {
                                        (Some(x), Some(y)) => match x.intersection(&y) {
                                            Some(i) => Some(i),
                                            // `intersection_e` THROWS, and the
                                            // throw reaches `queryArc`, which
                                            // then yields no haps at all.
                                            None => {
                                                signal_query_error(|| {
                                                    "TimeSpans do not intersect".into()
                                                });
                                                continue;
                                            }
                                        },
                                        _ => None,
                                    },
                                    _ => unreachable!("handled by the outer match"),
                                };
                                let mut ctx = a.context.clone();
                                ctx.extend_from_slice(&b.context);
                                if !push_budgeted(
                                    &mut out,
                                    Hap {
                                        whole,
                                        part: b.part,
                                        pick_lookup: joined_pick_lookup(&a, &b),
                                        scale: b.scale.clone().or_else(|| a.scale.clone()),
                                        tags: b.tags.clone().or_else(|| a.tags.clone()),
                                        log_line: b.log_line.clone().or_else(|| a.log_line.clone()),
                                        edo_size: b.edo_size.or(a.edo_size),
                                        scale_definition: b
                                            .scale_definition
                                            .clone()
                                            .or(a.scale_definition.clone()),
                                        value: b.value.clone(),
                                        context: ctx,
                                        ui_visuals: a.ui_visuals | b.ui_visuals,
                                        live_controls: b.live_controls,
                                        slider_binding: b.slider_binding,
                                    },
                                ) {
                                    return Vec::new();
                                }
                            }
                        }
                        out
                    }

                    // Each inner pattern is focused onto its outer hap's
                    // wholeOrPart. Two PRESENT but disjoint wholes drop the
                    // hap; context order is INNER-then-outer, the reverse of
                    // bindWhole.
                    JoinMode::Squeeze => {
                        let mut out = Vec::new();
                        for (outer, inner_pat) in resolved {
                            // `discreteOnly()`: analog outer haps are dropped.
                            let Some(outer_whole) = outer.whole else {
                                continue;
                            };
                            let focused = inner_pat.focus_span(outer.whole_or_part());
                            for inner in focused.query(&state.set_span(outer.part)) {
                                let whole = match inner.whole {
                                    Some(iw) => match iw.intersection(&outer_whole) {
                                        Some(w) => Some(w),
                                        None => continue,
                                    },
                                    None => None,
                                };
                                let Some(part) = inner.part.intersection(&outer.part) else {
                                    continue;
                                };
                                let mut ctx = inner.context.clone();
                                ctx.extend_from_slice(&outer.context);
                                if !push_budgeted(
                                    &mut out,
                                    Hap {
                                        whole,
                                        part,
                                        pick_lookup: joined_pick_lookup(&outer, &inner),
                                        scale: outer.scale.clone().or_else(|| inner.scale.clone()),
                                        tags: outer.tags.clone().or_else(|| inner.tags.clone()),
                                        log_line: outer
                                            .log_line
                                            .clone()
                                            .or_else(|| inner.log_line.clone()),
                                        edo_size: outer.edo_size.or(inner.edo_size),
                                        scale_definition: outer
                                            .scale_definition
                                            .clone()
                                            .or(inner.scale_definition.clone()),
                                        value: inner.value.clone(),
                                        context: ctx,
                                        ui_visuals: outer.ui_visuals | inner.ui_visuals,
                                        live_controls: inner.live_controls,
                                        slider_binding: inner.slider_binding,
                                    },
                                ) {
                                    return Vec::new();
                                }
                            }
                        }
                        out
                    }

                    // The inner pattern is queried over the whole state span,
                    // not the outer part. A whole that fails to intersect
                    // yields an analog hap rather than dropping it; only a
                    // missing `part` filters.
                    JoinMode::Reset | JoinMode::Restart => {
                        let mut out = Vec::new();
                        for (outer, inner_pat) in resolved {
                            let Some(outer_whole) = outer.whole else {
                                continue;
                            };
                            let shift = if *mode == JoinMode::Restart {
                                outer_whole.begin
                            } else {
                                outer_whole.begin.cycle_pos()
                            };
                            let shifted = inner_pat.late(shift);
                            for inner in shifted.query(state) {
                                let Some(part) = inner.part.intersection(&outer.part) else {
                                    continue;
                                };
                                let whole =
                                    inner.whole.and_then(|iw| iw.intersection(&outer_whole));
                                let mut ctx = outer.context.clone();
                                ctx.extend_from_slice(&inner.context);
                                if !push_budgeted(
                                    &mut out,
                                    Hap {
                                        whole,
                                        part,
                                        pick_lookup: joined_pick_lookup(&outer, &inner),
                                        scale: inner.scale.clone().or_else(|| outer.scale.clone()),
                                        tags: inner.tags.clone().or_else(|| outer.tags.clone()),
                                        log_line: inner
                                            .log_line
                                            .clone()
                                            .or_else(|| outer.log_line.clone()),
                                        edo_size: inner.edo_size.or(outer.edo_size),
                                        scale_definition: inner
                                            .scale_definition
                                            .clone()
                                            .or(outer.scale_definition.clone()),
                                        value: inner.value.clone(),
                                        context: ctx,
                                        ui_visuals: outer.ui_visuals | inner.ui_visuals,
                                        live_controls: inner.live_controls,
                                        slider_binding: inner.slider_binding,
                                    },
                                ) {
                                    return Vec::new();
                                }
                            }
                        }
                        out
                    }
                }
            }

            // The outer pattern is queried over CYCLE ZERO after shifting, so
            // the slice boundaries are always cycle-relative.
            Node::StepJoin(pat) => {
                let sam = state.span.begin.sam();
                let shifted = pat.early(sam);
                let cycle = state.set_span(TimeSpan::new(Fraction::ZERO, Fraction::ONE));
                let resolved = shifted.resolve_for_join(&cycle);
                if budget_exhausted() {
                    return Vec::new();
                }
                // Slicing calls `value.withHap(...)`, so a hap whose value
                // is not a pattern is a `TypeError` here. At query time that
                // reaches `queryArc` and empties the result; at construction
                // time it escapes as a real throw - see `try_step_join`.
                let resolved = match ResolvedHap::all_patterns(resolved) {
                    Ok(resolved) => resolved,
                    Err(value) => {
                        signal_query_error(|| step_join_value_error(&value));
                        return Vec::new();
                    }
                };
                step_slices_pattern(&resolved).query(state)
            }

            // outerJoin semantics: the OUTER hap's whole survives.
            Node::PolyJoin(pat) => {
                let resolved = pat.resolve_for_join(state);
                // A non-pattern value reads a missing step count, which the
                // fraction layer turns into `Division by Zero`; `queryArc`
                // catches it. `polyJoin`'s `fmap` is lazy, so unlike
                // `stepJoin` this is always a query-time failure, never a
                // construction one.
                let resolved = match ResolvedHap::all_patterns(resolved) {
                    Ok(resolved) => resolved,
                    Err(_) => {
                        signal_query_error(|| "Division by Zero".into());
                        return Vec::new();
                    }
                };
                let outer_steps = pat.steps;
                let mut out = Vec::new();
                for (outer, inner) in resolved {
                    let extended = poly_extend(&inner, outer_steps);
                    for b in extended.query(&state.set_span(outer.part)) {
                        let mut ctx = outer.context.clone();
                        ctx.extend_from_slice(&b.context);
                        if !push_budgeted(
                            &mut out,
                            Hap {
                                whole: outer.whole,
                                part: b.part,
                                pick_lookup: b.pick_lookup.clone(),
                                scale: b.scale.clone().or_else(|| outer.scale.clone()),
                                tags: b.tags.clone().or_else(|| outer.tags.clone()),
                                log_line: b.log_line.clone().or_else(|| outer.log_line.clone()),
                                edo_size: b.edo_size.or(outer.edo_size),
                                scale_definition: b
                                    .scale_definition
                                    .clone()
                                    .or(outer.scale_definition.clone()),
                                value: b.value.clone(),
                                context: ctx,
                                ui_visuals: b.ui_visuals | outer.ui_visuals,
                                live_controls: b.live_controls,
                                slider_binding: b.slider_binding,
                            },
                        ) {
                            return Vec::new();
                        }
                    }
                }
                out
            }

            Node::Arp(pat, indices) => {
                let mut out = Vec::new();
                // `collect()`: congruent haps - equal whole, analog haps
                // grouping together - in first-seen order, which is the order
                // `groupHapsBy` produces and therefore the chord's voice order.
                for group in collect_congruent(pat.query(state)) {
                    // The carrier hap has no context of its own, so
                    // `innerJoin` contributes only the index hap's context.
                    let carrier = &group[0];
                    for index_hap in indices.query(&state.set_span(carrier.part)) {
                        // Negative integers wrap; a fractional index misses
                        // and the following dereference throws.
                        let Some(picked) = js_index(&index_hap.value, group.len()) else {
                            signal_query_error(|| {
                                "Cannot read properties of undefined (reading 'value')".into()
                            });
                            return Vec::new();
                        };
                        let picked = &group[picked];
                        // `withHap`: the joined hap's context first, then the
                        // selected hap's - `h.combineContext(h.value)`.
                        let mut context = index_hap.context.clone();
                        context.extend_from_slice(&picked.context);
                        if !push_budgeted(
                            &mut out,
                            Hap {
                                // `innerJoin`: the INNER (index) hap's whole.
                                whole: index_hap.whole,
                                part: index_hap.part,
                                pick_lookup: picked.pick_lookup.clone(),
                                scale: picked.scale.clone().or_else(|| index_hap.scale.clone()),
                                tags: picked.tags.clone().or_else(|| index_hap.tags.clone()),
                                log_line: picked
                                    .log_line
                                    .clone()
                                    .or_else(|| index_hap.log_line.clone()),
                                edo_size: picked.edo_size.or(index_hap.edo_size),
                                scale_definition: picked
                                    .scale_definition
                                    .clone()
                                    .or(index_hap.scale_definition.clone()),
                                value: picked.value.clone(),
                                context,
                                ui_visuals: picked.ui_visuals | index_hap.ui_visuals,
                                live_controls: picked.live_controls,
                                slider_binding: picked.slider_binding,
                            },
                        ) {
                            return Vec::new();
                        }
                    }
                }
                out
            }

            Node::ArpWith(pat, id) => {
                let mut out = Vec::new();
                for group in collect_congruent(pat.query(state)) {
                    let carrier = &group[0];
                    // `fmap((v) => reify(func(v))).innerJoin()`: the callback
                    // result is a real pattern and is queried over the
                    // collected chord's part. Its haps provide the joined
                    // timing and their VALUES are Hap-shaped objects.
                    let selected = host_call_haps(*id, &group);
                    let Some(_recursion) = CallbackRecursionGuard::enter() else {
                        signal_query_error(|| "Maximum call stack size exceeded".into());
                        return Vec::new();
                    };
                    for inner in selected.query(&state.set_span(carrier.part)) {
                        let materialized = materialize_js_value(&inner.value);
                        let Some(object) = materialized.as_object() else {
                            // `null.value` / `undefined.value` throw at the
                            // first dereference. Other primitives are boxed,
                            // yield `undefined` for `.value`, then fail in
                            // `combineContext` at `b.context.locations`.
                            let message = match materialized {
                                Value::Null => "Cannot read properties of null (reading 'value')",
                                Value::Undefined => {
                                    "Cannot read properties of undefined (reading 'value')"
                                }
                                _ => "Cannot read properties of undefined (reading 'locations')",
                            };
                            signal_query_error(|| message.into());
                            return Vec::new();
                        };
                        // `h.value.value`: a missing `value` property is legal
                        // JavaScript and produces an undefined-valued hap.
                        let value = object.get("value").cloned().unwrap_or(Value::Undefined);
                        // `h.combineContext(h.value)` reads
                        // `b.context.locations`. Missing/undefined/null throws;
                        // other primitives are boxed by JavaScript and simply
                        // contribute no `locations` property.
                        let context = match object.get("context") {
                            None | Some(Value::Undefined) => {
                                signal_query_error(|| {
                                    "Cannot read properties of undefined (reading 'locations')"
                                        .into()
                                });
                                return Vec::new();
                            }
                            Some(Value::Null) => {
                                signal_query_error(|| {
                                    "Cannot read properties of null (reading 'locations')".into()
                                });
                                return Vec::new();
                            }
                            Some(Value::Object(context)) => Some(context),
                            Some(_) => None,
                        };
                        let mut locations = inner.context.clone();
                        if let Some(Value::List(items)) =
                            context.and_then(|context| context.get("locations"))
                        {
                            for item in items {
                                let Some(location) = item.as_object() else {
                                    continue;
                                };
                                let Some(start) = location.get("start").and_then(Value::as_f64)
                                else {
                                    continue;
                                };
                                let Some(end) = location.get("end").and_then(Value::as_f64) else {
                                    continue;
                                };
                                if start.is_finite()
                                    && end.is_finite()
                                    && start >= 0.0
                                    && end >= 0.0
                                    && start.fract() == 0.0
                                    && end.fract() == 0.0
                                    && start <= usize::MAX as f64
                                    && end <= usize::MAX as f64
                                {
                                    locations.push((start as usize, end as usize));
                                }
                            }
                        }
                        if !push_budgeted(
                            &mut out,
                            Hap {
                                whole: inner.whole,
                                part: inner.part,
                                value,
                                context: locations,
                                ui_visuals: inner.ui_visuals,
                                live_controls: [0; 2],
                                slider_binding: 0,
                                pick_lookup: None,
                                // `combineContext(h.value)`: a scale key on
                                // the value-side context object wins; the
                                // receiving hap's tag survives otherwise.
                                scale: context
                                    .and_then(|context| context.get("scale"))
                                    .and_then(|scale| match scale {
                                        Value::Str(name) => Some(Arc::from(name.as_str())),
                                        _ => None,
                                    })
                                    .or_else(|| inner.scale.clone()),
                                // `combineContext(h.value)`: a numeric
                                // edoSize on the value-side context wins;
                                // otherwise the receiving hap's tag survives.
                                edo_size: context
                                    .and_then(|context| context.get("edoSize"))
                                    .and_then(Value::as_f64)
                                    .or(inner.edo_size),
                                // `combineContext(h.value)`: a value-side
                                // scaleDefinition wins the same way.
                                scale_definition: context
                                    .and_then(|context| context.get("scaleDefinition"))
                                    .cloned()
                                    .map(Arc::new)
                                    .or_else(|| inner.scale_definition.clone()),
                                // A tag list arriving on the value-side
                                // context wins the same way; otherwise the
                                // receiving hap keeps its own.
                                tags: context
                                    .and_then(|context| context.get("tags"))
                                    .and_then(tags_from_value)
                                    .or_else(|| inner.tags.clone()),
                                // `.log()` writes its already-formatted text
                                // here; the runtime prints it when the hap
                                // triggers. Same value-side-wins rule.
                                log_line: context
                                    .and_then(|context| context.get("logLine"))
                                    .and_then(|value| match value {
                                        Value::Str(text) => Some(Arc::from(text.as_str())),
                                        _ => None,
                                    })
                                    .or_else(|| inner.log_line.clone()),
                            },
                        ) {
                            return Vec::new();
                        }
                    }
                    if query_error_pending() {
                        return Vec::new();
                    }
                }
                out
            }

            Node::ArpWithPattern(pat, callbacks) => {
                let mut out = Vec::new();
                for callback_hap in callbacks.query(state) {
                    let Value::Function(function) = &callback_hap.value else {
                        signal_query_error(|| "func is not a function".into());
                        return Vec::new();
                    };
                    let Some(id) = function.callback_id() else {
                        signal_query_error(|| "func is not a function".into());
                        return Vec::new();
                    };
                    let selected =
                        arp_with(pat.clone(), id).query(&state.set_span(callback_hap.part));
                    if query_error_pending() {
                        return Vec::new();
                    }
                    for mut hap in selected {
                        let mut context = callback_hap.context.clone();
                        context.append(&mut hap.context);
                        hap.context = context;
                        if !push_budgeted(&mut out, hap) {
                            return Vec::new();
                        }
                    }
                }
                out
            }

            // Pattern-valued haps, as above. Reachable from user source:
            // `pure(pure("bd")).polyJoin()` transpiles to a THREE-level nest,
            // so the join flattens one level and leaves a `PurePattern` to be
            // queried directly.
            Node::PurePattern(_) | Node::PatternOfJs(..) => {
                signal_query_error(pattern_valued_hap_error);
                Vec::new()
            }

            Node::SplitQueries(pat) => {
                let branches = state
                    .span
                    .span_cycles()
                    .into_iter()
                    .map(|sub| pat.query(&state.set_span(sub)));
                extend_budgeted(branches).unwrap_or_default()
            }
        }
    }

    /// Query a `PatternOf*` (possibly behind time transforms) and resolve each
    /// hap's inner pattern.
    ///
    /// `Join` does this inline, but `stepJoin` needs the inner patterns as
    /// values so it can slice and `stepcat` them. Top-level pattern-of-pattern
    /// nodes retain their exact timing here rather than being flattened.
    ///
    /// `None` means "this node is not a pattern-of-patterns at all", which is
    /// a different thing from a pattern-of-patterns that resolves to nothing.
    /// Callers that just want the haps a join consumes want
    /// [`Pattern::resolve_for_join`] instead.
    fn resolve_pattern_of(&self, state: &State) -> Option<Vec<ResolvedHap>> {
        let resolve = || {
            let mut resolved = self.resolve_pattern_of_inner(state);
            if let (Some(settings), Some(resolved)) = (&self.runtime_settings, &mut resolved) {
                for hap in resolved {
                    if let Some(inner) = &mut hap.inner {
                        inner.attach_runtime_settings(settings.clone());
                    }
                }
            }
            resolved
        };
        match &self.runtime_settings {
            Some(settings) => settings.with(resolve),
            None => resolve(),
        }
    }

    fn resolve_pattern_of_inner(&self, state: &State) -> Option<Vec<ResolvedHap>> {
        match &*self.node {
            Node::PatternOfPure(outer, f, memo) => {
                let mut resolved = Vec::new();
                let settings = settings::current_state_id();
                for hap in outer.query(state) {
                    let inner = memo
                        .construct(&hap.value, &settings, |value| f(value))
                        .pattern()
                        .clone();
                    if query_error_pending() || budget_exhausted() {
                        return Some(Vec::new());
                    }
                    resolved.push(ResolvedHap::of(hap, inner));
                }
                Some(resolved)
            }
            Node::PatternOfDynamic(outer, f) => {
                let mut resolved = Vec::new();
                for hap in outer.query(state) {
                    let inner = f(&hap.value);
                    // Iteration unwinds on the first throwing callback, as
                    // Array.map would. A host callback records that throw in the
                    // shared query-error channel and returns silence so Rust
                    // itself stays panic-free; stop HERE or later carrier haps
                    // would still expose user side effects before queryArc
                    // eventually discarded the answer.
                    if query_error_pending() || budget_exhausted() {
                        return Some(Vec::new());
                    }
                    resolved.push(ResolvedHap::of(hap, inner));
                }
                Some(resolved)
            }
            Node::PatternOfHap(outer, _, f) => {
                let mut resolved = Vec::new();
                for hap in outer.query(state) {
                    let inner = f(&hap);
                    if query_error_pending() || budget_exhausted() {
                        return Some(Vec::new());
                    }
                    resolved.push(ResolvedHap::of(hap, inner));
                }
                Some(resolved)
            }
            // `pure(pattern)`: one hap per cycle, each carrying the same known
            // inner pattern. Mirrors `Node::Pure`'s query exactly.
            Node::PurePattern(inner) => Some(
                state
                    .span
                    .span_cycles()
                    .into_iter()
                    .map(|subspan| {
                        ResolvedHap::of(
                            Hap::new(Some(whole_cycle(subspan.begin)), subspan, Value::Undefined),
                            inner.clone(),
                        )
                    })
                    .collect(),
            ),

            // `fmap(func)` with a user callback. `fmap` is representation-
            // agnostic: whatever the outer hap carries is what `func`
            // receives. After transpilation the outer is often itself
            // pattern-valued - `pure("bd")` becomes a pattern-of-patterns
            // while `pure('bd')` stays a string - and the callback must then
            // be handed a PATTERN. Squeezing that through `Value` is
            // impossible by design, so the two cases dispatch on the outer's
            // representation, per hap. The single/double quote distinction
            // is the transpiler's.
            Node::PatternOfJs(outer, id) => {
                let mut out_haps = Vec::new();
                match outer.resolve_pattern_of(state) {
                    Some(resolved) => {
                        for resolved in resolved {
                            let out = match resolved.inner {
                                Some(inner) => host_call_bind(*id, BindArg::Pattern(inner)),
                                // A nested level that is itself not a pattern:
                                // the hap's own value is what the callback gets.
                                None => host_call_bind(*id, BindArg::Value(&resolved.hap.value)),
                            };
                            if query_error_pending() || budget_exhausted() {
                                return Some(Vec::new());
                            }
                            out_haps.push(ResolvedHap::from_bind(resolved.hap, out));
                        }
                    }
                    None => {
                        for hap in outer.query(state) {
                            let out = host_call_bind(*id, BindArg::Value(&hap.value));
                            if query_error_pending() || budget_exhausted() {
                                return Some(Vec::new());
                            }
                            out_haps.push(ResolvedHap::from_bind(hap, out));
                        }
                    }
                }
                Some(out_haps)
            }

            // Unwrap through the ordinary structure/time transformations a
            // pattern-of-patterns may be wrapped in before the join. Matching
            // only the bare node made `polyJoin` return silence for anything
            // that had been `fast`ed, `early`d or split.
            Node::SplitQueries(inner) => inner.resolve_pattern_of(state),
            Node::AddContext(inner, locs) => inner.resolve_pattern_of(state).map(|resolved| {
                resolved
                    .into_iter()
                    .map(|mut resolved| {
                        resolved.hap.context.extend_from_slice(locs);
                        resolved
                    })
                    .collect()
            }),
            Node::WithQueryTime(inner, f) => {
                inner.resolve_pattern_of(&state.with_span(|sp| sp.with_time(|t| f(t))))
            }
            // `rev` reflects the query span and then reflects the haps back, so
            // it cannot delegate the way the time transforms above do: the
            // same reflection applies on both sides. Without this arm,
            // `pure("bd sd").rev().polyJoin()` falls through to a plain query
            // of the pattern-of-patterns and produces nothing instead of its
            // two haps.
            Node::Rev(inner) => {
                let cycle = state.span.begin.sam();
                let next_cycle = state.span.begin.next_sam();
                let reflect = |sp: &TimeSpan| {
                    let b = cycle.add(next_cycle.sub(sp.begin));
                    let e = cycle.add(next_cycle.sub(sp.end));
                    TimeSpan::new(e, b)
                };
                inner
                    .resolve_pattern_of(&state.set_span(reflect(&state.span)))
                    .map(|resolved| {
                        resolved
                            .into_iter()
                            .map(|resolved| ResolvedHap {
                                hap: resolved.hap.with_span(reflect),
                                inner: resolved.inner,
                            })
                            .collect()
                    })
            }
            Node::WithHapTime(inner, f) => inner.resolve_pattern_of(state).map(|resolved| {
                resolved
                    .into_iter()
                    .map(|resolved| ResolvedHap {
                        hap: resolved.hap.with_span(|sp| sp.with_time(|t| f(t))),
                        inner: resolved.inner,
                    })
                    .collect()
            }),
            // `stack` resolves each branch and concatenates, mirroring the
            // query's flatten. `seqPLoop` (and any join over a stacked
            // pattern-of-patterns) otherwise fell through to the plain-query
            // fallback and hit the pattern-valued refusal with no haps at all.
            Node::Stack(children) => Some(
                children
                    .iter()
                    .flat_map(|child| query_stack_child(|| child.resolve_for_join(state)))
                    .collect(),
            ),
            // `fastGap` on a pattern-of-patterns: the join must consume the
            // same compressed outer windows the query path produces, or
            // `pure(p).compress(b, e).innerJoin()` - the shape `seqPLoop`
            // builds - falls through to the plain-query fallback and hits the
            // pattern-valued refusal. The in/out scaling mirrors the query
            // arm exactly.
            Node::FastGap(inner, factor) => {
                let factor = *factor;
                let span = state.span;
                let one = Fraction::ONE;
                let mut resolved_haps = Vec::new();
                for subspan in span.span_cycles() {
                    let cycle = subspan.begin.sam();
                    let Some(inner_span) = (|| {
                        let bpos = subspan
                            .begin
                            .checked_sub(cycle)?
                            .checked_mul(factor)?
                            .min(one);
                        let epos = subspan
                            .end
                            .checked_sub(cycle)?
                            .checked_mul(factor)?
                            .min(one);
                        if bpos >= one {
                            return None;
                        }
                        Some(TimeSpan::new(
                            cycle.checked_add(bpos)?,
                            cycle.checked_add(epos)?,
                        ))
                    })() else {
                        if subspan
                            .begin
                            .checked_sub(cycle)
                            .and_then(|d| d.checked_mul(factor))
                            .is_none()
                        {
                            refuse(QueryLimit::NativeFraction {
                                operation: "fastGap",
                            });
                            return Some(Vec::new());
                        }
                        // The compressed window lies before this cycle; the
                        // query path drops it silently, so skip it here too.
                        continue;
                    };
                    let mapped: Option<Vec<ResolvedHap>> = inner
                        .resolve_pattern_of(&state.set_span(inner_span))?
                        .into_iter()
                        .map(|mut resolved| {
                            resolved.hap = Self::fast_gap_unmap_hap(&resolved.hap, factor)?;
                            Some(resolved)
                        })
                        .collect();
                    let Some(haps) = mapped else {
                        refuse(QueryLimit::NativeFraction {
                            operation: "fastGap",
                        });
                        return Some(Vec::new());
                    };
                    resolved_haps.extend(haps);
                }
                Some(resolved_haps)
            }
            _ => None,
        }
    }

    /// What a join actually consumes: total where [`Pattern::resolve_pattern_of`]
    /// is partial.
    ///
    /// A receiver that is not a pattern-of-patterns still produces haps -
    /// `pure('bd').polyJoin()` queries one hap holding the string `'bd'`.
    /// `fmap` never inspects what it stores, so the failure belongs to
    /// the join that dereferences the value, and each join fails differently
    /// (`polyJoin` reads `._steps`, `stepJoin` calls `.withHap`). Reporting
    /// "not a pattern-of" from here instead would collapse those into one
    /// shape.
    fn resolve_for_join(&self, state: &State) -> Vec<ResolvedHap> {
        self.resolve_pattern_of(state).unwrap_or_else(|| {
            self.query(state)
                .into_iter()
                .map(|hap| match &hap.value {
                    // Ordinary `fmap` is representation-agnostic: a
                    // callback may return a Pattern, store it in the hap, and
                    // a following join must consume it. `FmapJs` materialises
                    // that wrapper as `Value::Pattern`; treating every
                    // fallback hap as scalar made
                    // `p.fmap(x => pure(x)).innerJoin()` silently empty.
                    Value::Pattern(value) => ResolvedHap {
                        inner: Some(value.pattern().clone()),
                        hap: hap.with_value(|_| Value::Undefined),
                    },
                    _ => ResolvedHap { hap, inner: None },
                })
                .collect()
        })
    }

    /// The hap mapping `fastGap` applies on the way OUT: within-cycle
    /// positions divided back by the factor, clamped at the cycle boundary.
    /// Shared by the query path and the pattern-of-patterns resolution so a
    /// join consumes exactly the same compressed windows the query produces.
    fn fast_gap_unmap_hap(hap: &Hap, factor: Fraction) -> Option<Hap> {
        let begin = hap.part.begin;
        let end = hap.part.end;
        let cyc = begin.sam();
        let one = Fraction::ONE;
        let begin_pos = begin.checked_sub(cyc)?.checked_div(factor)?.min(one);
        let end_pos = end.checked_sub(cyc)?.checked_div(factor)?.min(one);
        let new_part = TimeSpan::new(cyc.checked_add(begin_pos)?, cyc.checked_add(end_pos)?);
        let new_whole = match hap.whole {
            Some(w) => Some(TimeSpan::new(
                new_part
                    .begin
                    .checked_sub(begin.checked_sub(w.begin)?.checked_div(factor)?)?,
                new_part
                    .end
                    .checked_add(w.end.checked_sub(end)?.checked_div(factor)?)?,
            )),
            None => None,
        };
        Some(Hap {
            whole: new_whole,
            part: new_part,
            pick_lookup: hap.pick_lookup.clone(),
            value: hap.value.clone(),
            context: hap.context.clone(),
            ui_visuals: hap.ui_visuals,
            live_controls: hap.live_controls,
            slider_binding: hap.slider_binding,
            scale: hap.scale.clone(),
            tags: hap.tags.clone(),
            log_line: hap.log_line.clone(),
            edo_size: hap.edo_size,
            scale_definition: hap.scale_definition.clone(),
        })
    }

    /// `__pure` - the value if this is a `pure` pattern, else `None`.
    /// `register()`'s fast path keys off this, and it is semantically
    /// observable because that path also merges source locations.
    fn as_pure_inner(&self) -> Option<Value> {
        match &*self.node {
            Node::Pure(v) | Node::PurePickLookup(v, _) => Some(v.clone()),
            // `withContext` preserves the pure fast-path metadata;
            // AddContext is that operation's native representation.
            Node::AddContext(inner, _) => inner.as_pure_inner(),
            _ => None,
        }
    }

    pub fn as_pure(&self) -> Option<Value> {
        let mut value = self.as_pure_inner()?;
        let owner = self
            .runtime_settings
            .as_ref()
            .cloned()
            .or_else(settings::RuntimeSettings::current_requested);
        if let Some(settings) = owner
            && !value.attach_runtime_settings(&settings, 0)
        {
            return None;
        }
        Some(value)
    }

    fn pick_lookup_with_runtime_settings(&self, mut lookup: PickLookup) -> Option<PickLookup> {
        let owner = self
            .runtime_settings
            .as_ref()
            .cloned()
            .or_else(settings::RuntimeSettings::current_requested);
        if let Some(settings) = owner
            && !lookup.attach_runtime_settings(&settings)
        {
            return None;
        }
        Some(lookup)
    }

    pub fn as_pick_lookup(&self) -> Option<PickLookup> {
        let lookup = match &*self.node {
            Node::PurePickLookup(_, lookup) => Some(lookup.clone()),
            _ => None,
        }?;
        self.pick_lookup_with_runtime_settings(lookup)
    }

    /// `__pure_loc` - the source span attached to a pure value, if any.
    pub fn pure_loc(&self) -> Option<(usize, usize)> {
        self.pure_loc
    }

    pub fn with_pure_loc(mut self, loc: (usize, usize)) -> Self {
        self.pure_loc = Some(loc);
        self
    }

    /// Append source locations to every hap's context, as
    /// `withContext(ctx => ({...ctx, locations: [...ctx.locations, ...locs]}))`.
    pub fn with_added_context(&self, locs: Vec<(usize, usize)>) -> Self {
        let locs = Arc::new(locs);
        let mut pattern = Pattern::of(Node::AddContext(self.clone(), locs)).with_steps(self.steps);
        pattern.pure_loc = self.pure_loc;
        pattern
    }

    pub fn with_ui_visual_slot(&self, slot: u8) -> Self {
        let mut pattern =
            self.map_haps_preserving_live(move |hap| Some(hap.clone().with_ui_visual_slot(slot)));
        pattern.steps = self.steps;
        pattern.pure_loc = self.pure_loc;
        pattern
    }

    /// Collect this pattern's values into a one-element `Value::List`, the seed
    /// of `register()`'s positional-argument fold.
    pub fn fmap_collect(&self) -> Self {
        self.fmap(|v| Value::List(vec![v.clone()]))
    }

    /// Append the next positional argument via `appLeft`.
    pub fn app_left_collect(&self, other: Pattern) -> Self {
        Pattern::of(Node::AppLeft(
            self.clone(),
            other,
            Arc::new(|acc: &Value, v: &Value| match acc {
                Value::List(xs) => {
                    let mut xs = xs.clone();
                    xs.push(v.clone());
                    Value::List(xs)
                }
                other => Value::List(vec![other.clone(), v.clone()]),
            }),
            LookupFlow::Infer,
        ))
    }

    /// Apply a native curried-value equivalent with `appLeft` alignment.
    ///
    /// `result._steps = this._steps` - structure comes from the function side.
    pub fn app_left_with(
        &self,
        other: Pattern,
        combine: impl Fn(&Value, &Value) -> Value + Send + Sync + 'static,
    ) -> Self {
        self.app_left_with_lookup(other, combine, LookupFlow::Infer)
    }

    pub fn app_left_with_lookup(
        &self,
        other: Pattern,
        combine: impl Fn(&Value, &Value) -> Value + Send + Sync + 'static,
        lookup_flow: LookupFlow,
    ) -> Self {
        let steps = self.steps;
        Pattern::of(Node::AppLeft(
            self.clone(),
            other,
            Arc::new(combine),
            lookup_flow,
        ))
        .with_steps(steps)
    }

    /// Apply a native curried-value equivalent with `appRight` alignment.
    ///
    /// `result._steps = pat_val._steps` - structure comes from the value side.
    pub fn app_right_with(
        &self,
        other: Pattern,
        combine: impl Fn(&Value, &Value) -> Value + Send + Sync + 'static,
    ) -> Self {
        self.app_right_with_lookup(other, combine, LookupFlow::Infer)
    }

    pub fn app_right_with_lookup(
        &self,
        other: Pattern,
        combine: impl Fn(&Value, &Value) -> Value + Send + Sync + 'static,
        lookup_flow: LookupFlow,
    ) -> Self {
        let steps = other.steps;
        Pattern::of(Node::AppRight(
            self.clone(),
            other,
            Arc::new(combine),
            lookup_flow,
        ))
        .with_steps(steps)
    }

    /// `appBoth` - both sides queried over the same span, wholes intersected.
    ///
    /// `result._steps = lcm(pat_val._steps, pat_func._steps)`.
    pub fn app_both_with(
        &self,
        other: Pattern,
        combine: impl Fn(&Value, &Value) -> Value + Send + Sync + 'static,
    ) -> Self {
        self.app_both_with_lookup(other, combine, LookupFlow::Infer)
    }

    pub fn app_both_with_lookup(
        &self,
        other: Pattern,
        combine: impl Fn(&Value, &Value) -> Value + Send + Sync + 'static,
        lookup_flow: LookupFlow,
    ) -> Self {
        let steps = match (other.steps, self.steps) {
            (Some(a), Some(b)) => match a.checked_lcm(b) {
                Some(steps) => Some(steps),
                None => {
                    return query_limit_pattern(mark_stepwise_refusal(
                        QueryLimit::NativeFraction {
                            operation: "appBoth",
                        },
                    ));
                }
            },
            (Some(a), None) => Some(a),
            (None, b) => b,
        };
        Pattern::of(Node::AppBoth(
            self.clone(),
            other,
            Arc::new(combine),
            lookup_flow,
        ))
        .with_steps(steps)
    }

    /// Map each value to a **provably pure** pattern.
    ///
    /// The closure's return type is the proof: a `PurePattern` can only be
    /// obtained from a pattern already classified pure, so this cannot smuggle
    /// in a callback. Purity is therefore inherited from the accumulator
    /// *soundly*, rather than by a promise that can lie.
    pub fn fmap_to_pure_pattern(
        &self,
        f: impl Fn(&Value) -> purity::PurePattern + Send + Sync + 'static,
    ) -> Self {
        // `fmap` carries `_steps`; these are `fmap` with a pattern-valued
        // result, so they must too - `stepJoin` reads the outer step count.
        let steps = self.steps;
        Pattern::of(Node::PatternOfPure(
            self.clone(),
            Arc::new(f),
            ConstructionMemo::default(),
        ))
        .with_steps(steps)
    }

    /// Map each value to an arbitrary pattern.
    ///
    /// **Always classified impure**, because the closure is opaque and may
    /// materialise JavaScript at query time. This is the constructor every
    /// unknown or dynamic path must use - including `register()` with a
    /// non-native combinator body.
    ///
    /// There is deliberately no way for a caller to declare purity: a
    /// caller-supplied flag could carry a false-pure classification.
    pub fn fmap_to_pattern(&self, f: impl Fn(&Value) -> Pattern + Send + Sync + 'static) -> Self {
        let steps = self.steps;
        Pattern::of(Node::PatternOfDynamic(self.clone(), Arc::new(f))).with_steps(steps)
    }

    /// `fmap` with a user JS callback. **Marks the pattern impure.**
    ///
    /// The step count survives, as in every other `fmap` here. Without it the
    /// result is unusable as a `polyJoin` input: the join divides by an
    /// undefined step count and throws, which empties any poly-aligned
    /// expression that contains the JS-mapped pattern.
    pub fn fmap_js(&self, id: CallbackId) -> Self {
        let steps = self.steps;
        Pattern::of(Node::FmapJs(self.clone(), id)).with_steps(steps)
    }

    /// `innerJoin` - the inner hap's whole survives. `register()`'s default.
    pub fn inner_join(&self) -> Self {
        Pattern::of(Node::Join(self.clone(), JoinMode::Inner))
    }
    /// `outerJoin` - the outer hap's whole survives.
    pub fn outer_join(&self) -> Self {
        Pattern::of(Node::Join(self.clone(), JoinMode::Outer))
    }
    /// `join`/`bind` - wholes intersect.
    pub fn mix_join(&self) -> Self {
        Pattern::of(Node::Join(self.clone(), JoinMode::Mix))
    }
    /// `squeezeJoin` - a whole cycle of each inner pattern is squeezed into the
    /// corresponding outer hap. What `ply`, `chop`, `euclid`'s squeeze variants
    /// and mini-notation's `@`-weighted replication are built from.
    pub fn squeeze_join(&self) -> Self {
        Pattern::of(Node::Join(self.clone(), JoinMode::Squeeze))
    }
    /// `resetJoin` - inner cycle start re-aligned to each outer onset.
    pub fn reset_join(&self) -> Self {
        Pattern::of(Node::Join(self.clone(), JoinMode::Reset))
    }
    /// `restartJoin` - inner cycle zero re-aligned to each outer onset.
    pub fn restart_join(&self) -> Self {
        Pattern::of(Node::Join(self.clone(), JoinMode::Restart))
    }

    /// `polyBind(func)` with a USER JavaScript callback: `fmap` then
    /// `polyJoin`.
    pub fn poly_bind_js(&self, id: CallbackId) -> Self {
        pattern_of_js(self.clone(), id).poly_join()
    }

    /// `stepBind(func)` with a user JavaScript callback.
    ///
    /// `Err` on the construction-time failure - see [`Pattern::try_step_join`].
    /// This call can invoke the callback before any query, so the host's
    /// bridge frame must still be open.
    pub fn try_step_bind_js(&self, id: CallbackId) -> Result<Self, String> {
        pattern_of_js(self.clone(), id).try_step_join()
    }

    /// `polyJoin` - each inner pattern is `extend`ed to the outer's step count
    /// before an `outerJoin`.
    ///
    /// A node rather than a construction-time rewrite because structure and
    /// time transformations can wrap the pattern before join resolution, which
    /// therefore happens through the same query-time walker `stepJoin` uses.
    ///
    /// A missing step count on either side throws - which `queryArc` turns
    /// into no haps. Reproduced rather than smoothed over.
    pub fn poly_join(&self) -> Self {
        let steps = self.steps;
        Pattern::of(Node::PolyJoin(self.clone())).with_steps(steps)
    }

    /// `queryArc(begin, end)` - the public pattern-query boundary.
    ///
    /// Also the boundary at which a mid-query failure becomes an empty
    /// result. Nested `query_arc` calls (a JS callback querying another
    /// pattern) absorb their own errors: the inner boundary catches first.
    pub fn query_arc(&self, begin: Fraction, end: Fraction) -> Vec<Hap> {
        self.query_state(&State::new(TimeSpan::new(begin, end)))
    }

    /// `queryArc`'s error boundary for callers that need to supply a full
    /// [`State`] rather than just a span - the scheduler attaches `_cps`, and
    /// the JS host attaches scheduler controls.
    ///
    /// **Use this, not `query`, at any top-level entry point.** `query` is the
    /// raw recursive step and has no boundary: a pattern whose query fails
    /// mid-way (`note("c").add("x")` reaches `parseNumeral`'s throw) hands
    /// back NaN-valued haps instead of the empty result the boundary
    /// guarantees. On the scheduling path that is not cosmetic - those haps
    /// become onset events, scheduling audio where the score must be silent.
    pub fn query_state(&self, state: &State) -> Vec<Hap> {
        self.try_query_state(state).unwrap_or_default()
    }

    /// `query_state`, distinguishing RESOURCE EXHAUSTION from an ordinary
    /// query throw.
    ///
    /// Both end in no haps, and conflating them is how a limit becomes a lie: a
    /// caller cannot tell "this pattern produces nothing" from "this pattern
    /// was refused", so a truncated render looks like a correct silent one.
    /// Exhaustion therefore travels on its own flag, which the `queryArc`
    /// boundary below deliberately does not clear.
    pub fn try_query_state(&self, state: &State) -> Result<Vec<Hap>, QueryLimit> {
        self.try_query_state_with_budget(state, DEFAULT_HAP_BUDGET)
    }

    /// One outer `queryArc` that keeps the throw distinct from empty haps.
    ///
    /// Steady-state scheduling still uses [`Self::try_query_state`] (silence
    /// for that window, clock keeps running). A live replacement must call
    /// this so a typo that constructs but throws at query time cannot replace
    /// the sounding score with silence.
    pub fn query_arc_outcome(&self, state: &State) -> Result<QueryArcOutcome, QueryLimit> {
        self.query_arc_outcome_with_budget(state, DEFAULT_HAP_BUDGET)
    }

    /// `try_query_state` with an explicit budget.
    ///
    /// Public so embedders and tests can exercise the same resource boundary
    /// with a smaller limit instead of allocating millions of haps.
    ///
    /// Also genuinely useful: an embedder scheduling on a small device can pick
    /// a tighter bound than this build's default.
    pub fn try_query_state_with_budget(
        &self,
        state: &State,
        budget: u64,
    ) -> Result<Vec<Hap>, QueryLimit> {
        match self.query_arc_outcome_with_budget(state, budget)? {
            QueryArcOutcome::Haps(haps) => Ok(haps),
            // The `queryArc` catch: this window is silent. The throw is
            // not a resource refusal and must not become scheduled NaNs.
            QueryArcOutcome::Thrown(_) => Ok(Vec::new()),
        }
    }

    /// [`Self::query_arc_outcome`] with an explicit hap budget.
    pub fn query_arc_outcome_with_budget(
        &self,
        state: &State,
        budget: u64,
    ) -> Result<QueryArcOutcome, QueryLimit> {
        // A fresh top-level query reports only its own contained failures; a
        // stale publication from a construction probe or an earlier query
        // must not be attributed here.
        clear_query_callback_failure();
        // The flag is thread-local and shared, so an enclosing boundary's
        // pending error is stashed and restored around this one. Nesting then
        // behaves right: the INNER boundary catches first and the outer one
        // is unaffected.
        let error_boundary = QueryErrorBoundary::enter();
        let metadata_owner = self
            .runtime_settings
            .clone()
            .or_else(settings::RuntimeSettings::current_requested);
        let query = || {
            with_hap_budget(budget, || {
                let raw_query_attaches_metadata = QUERY_DEPTH.with(|depth| depth.get() == 0);
                let mut haps = self.query(state);
                // JS-owned arrays/objects preserve their identity throughout the
                // graph, but the host that can resolve that identity exists only
                // during the outer query. Convert returned values here so every
                // later renderer (CLI JSON, scheduler events, Hap::show) receives
                // an ordinary host-independent Value.
                for hap in &mut haps {
                    let materialized = hap.value.contains_js_value();
                    if materialized {
                        hap.value = materialize_js_value(&hap.value);
                        hap.live_controls = [0; 2];
                        hap.slider_binding = 0;
                    }
                    if let Some(settings) = &metadata_owner
                        && (materialized || !raw_query_attaches_metadata)
                        && !hap.value.attach_runtime_settings(settings, 0)
                    {
                        return Vec::new();
                    }
                }
                haps
            })
        };
        // The owner governs the complete public query operation, including
        // JS container materialisation after the recursive graph returns.
        let (haps, refusal) = match &metadata_owner {
            Some(settings) => settings.with(query),
            None => query(),
        };
        let thrown = error_boundary.finish();
        // Cancellation is checked FIRST: a query stopped part way will usually
        // also look under-budget, and reporting the limit would be misleading.
        if cancellation_requested() {
            return Err(QueryLimit::Cancelled);
        }
        if let Some(limit) = refusal {
            return Err(limit);
        }
        Ok(match thrown {
            Some(message) => QueryArcOutcome::Thrown(message),
            None => QueryArcOutcome::Haps(haps),
        })
    }

    /// Sorted as Strudel's `sortHapsByPart()` does. The order is part of the
    /// serialized query output.
    ///
    /// Four sort keys: part.begin, part.end, **whole.begin, whole.end** -
    /// omitting the whole keys leaves ties in the wrong order whenever two
    /// haps share a part.
    ///
    /// # Analog haps make the comparator throw
    ///
    /// The comparator reads `whole` unconditionally, so any analog hap among
    /// two or more results makes it throw and the enclosing `queryArc` yields
    /// no haps. Substituting `whole_or_part()` would return both haps; the
    /// throw-and-silence behaviour is preserved instead. A single element is
    /// never compared, so exactly one analog hap survives untouched.
    pub fn query_arc_sorted(&self, begin: Fraction, end: Fraction) -> Vec<Hap> {
        // `query_arc` owns the recursive query's error boundary, but sorting
        // is itself observable JavaScript-compatible work: two analog haps
        // make the comparator throw. Keep a second boundary around that final
        // phase so its error becomes this call's empty result and cannot poison
        // the next independent query on the worker thread.
        let error_boundary = QueryErrorBoundary::enter();
        let haps = sort_haps_by_part(self.query_arc(begin, end));
        match error_boundary.finish() {
            Some(_) => Vec::new(),
            None => haps,
        }
    }

    /// `query_arc_sorted`, propagating a resource refusal.
    ///
    /// The untyped form above discards the refusal. That is the right default
    /// for internal call sites that cannot act on one, but not for a
    /// top-level entry point: there, a discarded refusal looks like a pattern
    /// that produced nothing.
    pub fn try_query_arc_sorted(
        &self,
        begin: Fraction,
        end: Fraction,
    ) -> Result<Vec<Hap>, QueryLimit> {
        self.try_query_arc_sorted_with_budget(begin, end, DEFAULT_HAP_BUDGET)
    }

    /// `try_query_arc_sorted` with an explicit budget. See
    /// [`Pattern::try_query_state_with_budget`].
    pub fn try_query_arc_sorted_with_budget(
        &self,
        begin: Fraction,
        end: Fraction,
        budget: u64,
    ) -> Result<Vec<Hap>, QueryLimit> {
        let state = State::new(TimeSpan::new(begin, end));
        let error_boundary = QueryErrorBoundary::enter();
        let haps = sort_haps_by_part(self.try_query_state_with_budget(&state, budget)?);
        Ok(match error_boundary.finish() {
            Some(_) => Vec::new(),
            None => haps,
        })
    }

    /// Query and sort a window while preserving query and comparator errors.
    /// The outcome distinguishes a thrown error from an empty window.
    pub fn try_query_arc_sorted_outcome_with_budget(
        &self,
        begin: Fraction,
        end: Fraction,
        budget: u64,
    ) -> Result<QueryArcOutcome, QueryLimit> {
        let state = State::new(TimeSpan::new(begin, end));
        let error_boundary = QueryErrorBoundary::enter();
        let outcome = self.query_arc_outcome_with_budget(&state, budget)?;
        Ok(match outcome {
            // Keep the boundary active until the comparator has finished.
            QueryArcOutcome::Haps(haps) => {
                let sorted = sort_haps_by_part(haps);
                match error_boundary.finish() {
                    Some(message) => QueryArcOutcome::Thrown(message),
                    None => QueryArcOutcome::Haps(sorted),
                }
            }
            // The inner query boundary already captured this error.
            thrown @ QueryArcOutcome::Thrown(_) => {
                let _ = error_boundary.finish();
                thrown
            }
        })
    }
    // -- combinators --------------------------------------------------------

    pub fn with_query_time(
        &self,
        f: impl Fn(Fraction) -> Fraction + Send + Sync + 'static,
    ) -> Self {
        Pattern::of(Node::WithQueryTime(self.clone(), Arc::new(f)))
    }

    pub fn with_hap_time(&self, f: impl Fn(Fraction) -> Fraction + Send + Sync + 'static) -> Self {
        Pattern::of(Node::WithHapTime(self.clone(), Arc::new(f)))
    }

    pub fn with_query_span(
        &self,
        f: impl Fn(&TimeSpan) -> TimeSpan + Send + Sync + 'static,
    ) -> Self {
        Pattern::of(Node::WithQuerySpan(self.clone(), Arc::new(f)))
    }

    pub fn with_query_span_js(&self, id: CallbackId) -> Self {
        Pattern::of(Node::WithQuerySpanJs(self.clone(), id))
    }

    pub fn with_hap_span(&self, f: impl Fn(&TimeSpan) -> TimeSpan + Send + Sync + 'static) -> Self {
        Pattern::of(Node::WithHapSpan(self.clone(), Arc::new(f)))
    }

    /// `rev` - reverse each cycle. `preserveSteps = true`, `patternify = false`.
    pub fn rev(&self) -> Self {
        Pattern::of(Node::Rev(self.clone()))
            .split_queries()
            .with_steps(self.steps)
    }

    /// `revv` - reverse the whole timeline, not each cycle: negate both the
    /// query span and the hap spans.
    pub fn revv(&self) -> Self {
        fn negate(span: &TimeSpan) -> TimeSpan {
            TimeSpan::new(span.end.neg(), span.begin.neg())
        }
        self.with_query_span(negate).with_hap_span(negate)
    }

    /// `zoom(s, e)` - play the `[s, e)` slice of the pattern over the full
    /// cycle. Unlike `focus`, the slice is taken **within** each cycle.
    ///
    /// The width, scaled step count, and cycle maps must fit the native
    /// fraction range; otherwise the query reports `NativeFraction`.
    pub fn zoom(&self, s: Fraction, e: Fraction) -> Self {
        combinators::checked_zoom(self, s, e, "zoom")
    }

    /// Map every value.
    ///
    /// `_steps` is carried through: mapping VALUES does not change structure.
    /// Dropping it made `s("bd sd cp").pace(4)` a no-op, because `pace` reads
    /// the step count it is supposed to rescale.
    pub fn fmap(&self, f: impl Fn(&Value) -> Value + Send + Sync + 'static) -> Self {
        let steps = self.steps;
        Pattern::of(Node::FmapNative(self.clone(), Arc::new(f))).with_steps(steps)
    }

    /// Host-only provenance for a native query-time slider. Arbitrary value
    /// maps discard this tag before it can become an audio parameter binding.
    pub fn with_slider_binding(&self, binding: u64) -> Self {
        self.map_haps_preserving_live(move |hap| {
            let mut hap = hap.clone().without_live_controls();
            hap.slider_binding = binding;
            Some(hap)
        })
        .with_steps(self.steps)
    }

    fn map_control(&self, spec: controls::ControlSpec, unnamed_only: bool) -> Self {
        self.map_haps_preserving_live(move |hap| {
            let already_controls = unnamed_only
                && match &hap.value {
                    Value::Object(object) => !object.contains_key("value"),
                    Value::JsValue(reference) if !reference.is_array() => matches!(
                        materialize_js_value(&hap.value),
                        Value::Object(object) if !object.contains_key("value")
                    ),
                    _ => false,
                };
            if already_controls {
                return Some(hap.clone());
            }
            let mut mapped = hap.with_value(|value| spec.with_value(value.clone()));
            if hap.slider_binding != 0 && matches!(hap.value, Value::F64(_)) {
                match spec.name() {
                    "gain" => mapped.live_controls[0] = hap.slider_binding,
                    "cutoff" => mapped.live_controls[1] = hap.slider_binding,
                    _ => {}
                }
            }
            Some(mapped)
        })
        .with_steps(self.steps)
    }

    pub fn filter_values(
        &self,
        predicate: impl Fn(&Value) -> bool + Send + Sync + 'static,
    ) -> Self {
        Pattern::of(Node::FilterValues(self.clone(), Arc::new(predicate))).with_steps(self.steps)
    }

    pub fn filter_values_js(&self, id: CallbackId) -> Self {
        Pattern::of(Node::FilterValuesJs(self.clone(), id))
    }

    /// `filterHaps(hap_test)` - the predicate sees whole and part, not just
    /// the value. Unlike `filterValues`, `_steps` is not carried through.
    pub fn filter_haps(&self, predicate: impl Fn(&Hap) -> bool + Send + Sync + 'static) -> Self {
        Pattern::of(Node::FilterHaps(self.clone(), Arc::new(predicate)))
    }

    pub fn filter_haps_js(&self, id: CallbackId) -> Self {
        Pattern::of(Node::FilterHapsJs(self.clone(), id))
    }

    pub fn filter_when_js(&self, id: CallbackId) -> Self {
        Pattern::of(Node::FilterWhenJs(self.clone(), id))
    }

    pub fn sort_haps_by_part_pattern(&self) -> Self {
        Pattern::of(Node::SortHapsByPart(self.clone()))
    }

    /// `seed(n)` - pin the random stream for this subtree.
    pub fn with_rand_seed(&self, seed: f64) -> Self {
        Pattern::of(Node::WithRandSeed(self.clone(), seed)).with_steps(self.steps)
    }

    /// Per-hap rewrite where `None` drops the hap. Steps are preserved by the
    /// callers registered `preserveSteps`.
    pub fn map_haps_native(&self, f: impl Fn(&Hap) -> Option<Hap> + Send + Sync + 'static) -> Self {
        self.map_haps_preserving_live(move |hap| f(hap).map(Hap::without_live_controls))
    }

    fn map_haps_preserving_live(
        &self,
        f: impl Fn(&Hap) -> Option<Hap> + Send + Sync + 'static,
    ) -> Self {
        Pattern::of(Node::MapHapsNative(self.clone(), Arc::new(f)))
    }

    fn map_pitch_haps_native(
        &self,
        f: impl Fn(&Hap) -> Option<Hap> + Send + Sync + 'static,
    ) -> Self {
        self.map_haps_preserving_live(move |hap| {
            f(hap).map(|mapped| mapped.with_pitch_live_controls(hap))
        })
    }

    fn expand_pitch_haps_native(
        &self,
        f: impl Fn(&Hap) -> Vec<Hap> + Send + Sync + 'static,
    ) -> Self {
        Pattern::of(Node::ExpandHapsNative(
            self.clone(),
            Arc::new(move |hap| {
                f(hap)
                    .into_iter()
                    .map(|mapped| mapped.with_pitch_live_controls(hap))
                    .collect()
            }),
        ))
    }

    pub fn map_haps_with_state(
        &self,
        f: impl Fn(&State, &Hap) -> Option<Hap> + Send + Sync + 'static,
    ) -> Self {
        Pattern::of(Node::MapHapsWithState(
            self.clone(),
            Arc::new(move |state, hap| f(state, hap).map(Hap::without_live_controls)),
        ))
    }

    /// One hap → N haps (same classification rules as `map_haps_native`).
    pub fn expand_haps_native(&self, f: impl Fn(&Hap) -> Vec<Hap> + Send + Sync + 'static) -> Self {
        Pattern::of(Node::ExpandHapsNative(
            self.clone(),
            Arc::new(move |hap| f(hap).into_iter().map(Hap::without_live_controls).collect()),
        ))
    }

    pub fn collect(&self) -> Self {
        Pattern::of(Node::Collect(self.clone()))
    }

    pub fn loop_at(&self, factor: Fraction) -> Self {
        Pattern::of(Node::LoopAt(self.clone(), factor))
    }

    pub fn cpm(&self, cpm: Fraction) -> Self {
        Pattern::of(Node::Cpm(self.clone(), cpm))
    }

    /// Sets each [`combinators::slice`] event's `speed` so its slice fills the
    /// event; keeps the receiver's step count.
    pub fn splice(&self) -> Self {
        self.map_haps_with_state(|state, hap| {
            let Value::Object(value) = materialize_js_value(&hap.value) else {
                signal_query_error(|| "splice requires object-valued haps".into());
                return None;
            };
            let Some(whole) = hap.whole else {
                signal_query_error(|| {
                    "Cannot read properties of undefined (reading 'duration')".into()
                });
                return None;
            };
            let truthy_number = |value: Option<&Value>, fallback: f64| {
                value
                    .filter(|value| value.js_truthy())
                    .map(combinators::as_js_number)
                    .unwrap_or(fallback)
            };
            let cps = truthy_number(state.controls.get("_cps"), 1.0);
            let slices =
                combinators::as_js_number(value.get("_slices").unwrap_or(&Value::Undefined));
            let prior_speed = truthy_number(value.get("speed"), 1.0);
            let speed = cps / slices / whole.duration().to_f64() * prior_speed;
            let mut next = OrderedMap::from_entries([
                ("speed".into(), Value::F64(speed)),
                ("unit".into(), Value::Str("c".into())),
            ]);
            for (name, value) in &value {
                next.insert(name.clone(), value.clone());
            }
            Some(hap.with_value(|_| Value::Object(next.clone())))
        })
        .with_steps(self.steps)
    }

    pub fn fit(&self) -> Self {
        self.map_haps_with_state(|state, hap| {
            let Value::Object(mut value) = materialize_js_value(&hap.value) else {
                signal_query_error(|| "Cannot use 'in' operator on a primitive value".into());
                return None;
            };
            let Some(whole) = hap.whole else {
                signal_query_error(|| {
                    "Cannot read properties of undefined (reading 'duration')".into()
                });
                return None;
            };
            let begin = value
                .get("begin")
                .map(combinators::as_js_number)
                .unwrap_or(0.0);
            let end = value
                .get("end")
                .map(combinators::as_js_number)
                .unwrap_or(1.0);
            let cps = state
                .controls
                .get("_cps")
                .filter(|value| value.js_truthy())
                .map(combinators::as_js_number)
                .unwrap_or(1.0);
            let speed = cps / whole.duration().to_f64() * (end - begin);
            value.insert("speed".into(), Value::F64(speed));
            value.insert("unit".into(), Value::Str("c".into()));
            Some(hap.with_value(|_| Value::Object(value.clone())))
        })
    }

    /// `tag(name)` - append a context tag to every hap.
    ///
    /// The point is `hap.hasTag(name)` inside a later `filter`: mark haps in
    /// one branch of a `when`, then keep only those, which is how a score
    /// selects what an earlier transform touched.
    pub fn tag(&self, name: Arc<str>) -> Self {
        self.map_haps_preserving_live(move |hap| Some(hap.clone().with_tag_context(name.clone())))
    }

    /// Add a native graph node whose semantics are owned by a statically
    /// linked extension.
    pub fn extension_node(node: impl extension_node::ExtensionPatternNode + 'static) -> Self {
        Pattern::of(Node::Extension(Box::new(node)))
    }

    /// `discreteOnly()` - drop analog (whole-less) haps.
    pub fn discrete_only(&self) -> Self {
        self.filter_haps(|hap| hap.whole.is_some())
    }

    /// `onsetsOnly()` - keep only haps whose whole begins where their part does.
    pub fn onsets_only(&self) -> Self {
        self.filter_haps(Hap::has_onset)
    }

    pub fn remove_undefineds(&self) -> Self {
        self.filter_values(|value| !matches!(value, Value::Undefined | Value::Null))
    }

    pub fn degrade_by_seeded(&self, amount: f64, seed: u32) -> Self {
        Pattern::of(Node::Degrade(self.clone(), amount, seed)).with_steps(self.steps)
    }

    pub fn repeat_cycles(&self, count: Fraction) -> Self {
        if count == Fraction::ZERO {
            return silence();
        }
        Pattern::of(Node::RepeatCycles(self.clone(), count))
            .split_queries()
            .with_steps(self.steps)
    }

    /// Structure `self` with a binary pattern. The structure pattern's wholes
    /// survive, as in `keepif.out(structure)`.
    pub fn struct_with(&self, structure: Pattern) -> Self {
        self.app_right_with_lookup(
            structure,
            |value, keep| {
                if keep.js_truthy() {
                    value.clone()
                } else {
                    Value::Undefined
                }
            },
            LookupFlow::Left,
        )
        .remove_undefineds()
    }

    /// Default `set.in` alignment: preserve this pattern's structure and let
    /// the right-hand value override common object controls.
    pub fn set(&self, other: &Pattern) -> Self {
        self.app_left_with_lookup(other.clone(), controls::set_value, LookupFlow::Set)
            .with_steps(self.steps)
    }

    pub fn inner_join_with(&self, f: impl Fn(&Value) -> Pattern + Send + Sync + 'static) -> Self {
        Pattern::of(Node::InnerJoin(self.clone(), Arc::new(f)))
    }

    pub fn split_queries(&self) -> Self {
        Pattern::of(Node::SplitQueries(self.clone()))
    }

    /// Speed up by `factor`; `_steps` is kept.
    ///
    /// A representable factor can still overflow when applied to a query
    /// time, especially after another speed change. Both time maps are
    /// checked and report `NativeFraction` on overflow.
    pub fn fast(&self, factor: Fraction) -> Self {
        combinators::checked_fast(self, factor, "fast")
    }

    pub fn slow(&self, factor: Fraction) -> Self {
        if factor.numer() == 0 {
            return silence();
        }
        // The reciprocal of i128::MIN needs an unrepresentable denominator.
        // Keep `slow` in the diagnostic, including failures in the time maps.
        match Fraction::ONE.checked_div(factor) {
            Some(rate) => combinators::checked_fast(self, rate, "slow"),
            None => query_limit_pattern(mark_stepwise_refusal(QueryLimit::NativeFraction {
                operation: "slow",
            })),
        }
    }

    /// Shift later in time. `_late(t)`.
    ///
    /// `_steps` survives a shift: moving a pattern in time does not change
    /// how many steps it has. Without it, `focus()` (which is
    /// `_early`/`_fast`/`_late`) would return a step-less pattern, and
    /// `polyJoin` would take the missing-`_steps` throw path.
    pub fn late(&self, t: Fraction) -> Self {
        self.shift_time(t, "late")
    }

    /// Shift earlier in time. `_early(t)`.
    pub fn early(&self, t: Fraction) -> Self {
        // i128::MIN has no representable positive counterpart.
        match t.checked_neg() {
            Some(shifted) => self.shift_time(shifted, "early"),
            None => query_limit_pattern(mark_stepwise_refusal(QueryLimit::NativeFraction {
                operation: "early",
            })),
        }
    }

    /// Subtract `t` from query times and add it to event times, preserving
    /// steps. On overflow, record `NativeFraction` and use zero until the query
    /// boundary reports the refusal.
    fn shift_time(&self, t: Fraction, operation: &'static str) -> Self {
        let steps = self.steps;
        let refused = move || {
            mark_stepwise_refusal(QueryLimit::NativeFraction { operation });
            Fraction::ZERO
        };
        self.with_query_time(move |x| x.checked_sub(t).unwrap_or_else(refused))
            .with_hap_time(move |x| x.checked_add(t).unwrap_or_else(refused))
            .with_steps(steps)
    }

    /// Speeds up like `fast`, but leaves a **gap** for the rest of the cycle
    /// rather than repeating. This is what weighted sequences are built from -
    /// plain `fast` repeats and is the wrong primitive.
    pub fn fast_gap(&self, factor: Fraction) -> Self {
        Pattern::of(Node::FastGap(self.clone(), factor)).split_queries()
    }

    /// `compress(b, e)` - squeeze into the `[b, e)` slot of each cycle.
    /// Out-of-range or inverted bounds yield silence; a window whose width
    /// leaves the native fraction range refuses as
    /// `NativeFraction { operation: "compress" }`.
    pub fn compress(&self, b: Fraction, e: Fraction) -> Self {
        if b > e
            || b > Fraction::ONE
            || e > Fraction::ONE
            || b < Fraction::ZERO
            || e < Fraction::ZERO
        {
            return silence();
        }
        // A zero-width window is a division by zero, which THROWS. `compress`
        // runs inside `pressBy`'s `fmap`, i.e. at query time, so the throw is
        // caught by `queryArc` and the whole query yields nothing.
        // `pressBy(1)` is the reachable case.
        if e == b {
            signal_query_error(|| "Division by zero".into());
            return silence();
        }
        let Some(rate) = e
            .checked_sub(b)
            .and_then(|width| Fraction::ONE.checked_div(width))
        else {
            return query_limit_pattern(mark_stepwise_refusal(QueryLimit::NativeFraction {
                operation: "compress",
            }));
        };
        self.fast_gap(rate).late(b)
    }

    /// `focus(b, e)` - like `compress`, but leaves no gaps and the window may
    /// be wider than a cycle.
    ///
    /// The `early(b.sam())` is what lets the window straddle cycles: only the
    /// whole-cycle part of `b` is removed before the speed change.
    ///
    /// The window width, its reciprocal, and the whole-cycle shift amount are
    /// checked at construction even when both bounds fit individually; an
    /// unrepresentable one refuses as `NativeFraction { operation: "focus" }`.
    /// Overflow in the query-time maps refuses as `late` or `fast`.
    pub fn focus(&self, b: Fraction, e: Fraction) -> Self {
        if e == b {
            // Division by zero throws. See `compress`.
            signal_query_error(|| "Division by zero".into());
            return silence();
        }
        let Some(rate) = e
            .checked_sub(b)
            .and_then(|span| Fraction::ONE.checked_div(span))
        else {
            return focus_native_fraction_refusal();
        };
        // `early(t)` is `late(t.neg())`; taking the checked negation keeps
        // the one unrepresentable shift a refusal rather than a panic.
        let Some(whole_cycles) = b.sam().checked_neg() else {
            return focus_native_fraction_refusal();
        };
        self.late(whole_cycles).fast(rate).late(b)
    }

    /// `stepJoin` - flatten a pattern of patterns by slicing the outer cycle
    /// at every hap boundary and `stepcat`ing the pieces.
    ///
    /// The step count is taken from cycle zero once, at construction, while
    /// the query re-slices per cycle. A pattern whose structure varies by
    /// cycle therefore keeps cycle zero's step count.
    ///
    /// Ignores the construction-time throw: native combinators build the
    /// pattern-of-patterns themselves, so it is unreachable for them.
    /// Anything driven by user source wants [`Pattern::try_step_join`].
    pub fn step_join(&self) -> Self {
        self.try_step_join()
            .unwrap_or_else(|_| Pattern::of(Node::StepJoin(self.clone())))
    }

    /// `stepJoin`, reporting the CONSTRUCTION-time failure.
    ///
    /// The phase boundary is load-bearing and not where it first appears:
    ///
    /// * the cycle-zero probe is a full `queryArc`, so anything that throws
    ///   during it is CAUGHT and yields no haps. That is why
    ///   `pure('bd').stepBind(x => { throw })` constructs fine and only empties
    ///   the later query.
    /// * slicing then runs OUTSIDE that boundary and dereferences each value
    ///   as a pattern. A hap whose value is not one therefore throws here,
    ///   before the pattern exists at all - `pure('bd').stepJoin()` and
    ///   `pure('bd').stepBind(x => 42)` both fail this way.
    /// * No haps at all is not a failure: `silence.stepJoin()` constructs.
    pub fn try_step_join(&self) -> Result<Self, String> {
        let cycle = State::new(TimeSpan::new(Fraction::ZERO, Fraction::ONE));
        // A `queryArc` boundary: stash any pending error, absorb the
        // ones this construction query raises, restore. Without the stash a
        // callback that throws here would leak its error into the NEXT
        // top-level query and empty an unrelated result.
        let error_boundary = QueryErrorBoundary::enter();
        // This construction-time `queryArc(0, 1)` needs the same resource
        // ownership as a public query. In particular, patterned shrink/grow
        // bodies can build many retained inner graphs here before StepJoin
        // exists. A local boundary captures that refusal; a nested call shares
        // its enclosing query's cumulative construction budget.
        let (resolved, refusal) =
            with_hap_budget(DEFAULT_HAP_BUDGET, || self.resolve_for_join(&cycle));
        let query_failed = error_boundary.finish().is_some();
        if let Some(limit) = refusal {
            return Ok(query_limit_pattern(limit));
        }

        let first_steps = if query_failed {
            // `queryArc` caught it, so an EMPTY hap list is sliced.
            step_slices_pattern(&[]).steps
        } else {
            let resolved = ResolvedHap::all_patterns(resolved)
                .map_err(|value| step_join_value_error(&value))?;
            step_slices_pattern(&resolved).steps
        };
        Ok(Pattern::of(Node::StepJoin(self.clone())).with_steps(first_steps))
    }

    /// `_focusSpan(span)` - `focus` over a `TimeSpan`. `squeezeJoin` uses this
    /// to fit a whole inner cycle into each outer hap.
    pub fn focus_span(&self, span: TimeSpan) -> Self {
        self.focus(span.begin, span.end)
    }
}

/// Carry a focus-window overflow to the next query, or mark the active query
/// immediately when construction happens inside one.
fn focus_native_fraction_refusal() -> Pattern {
    query_limit_pattern(mark_stepwise_refusal(QueryLimit::NativeFraction {
        operation: "focus",
    }))
}

/// A childless stand-in, cheap to produce: cloning it is a refcount bump and
/// `Silence` carries an empty purity, so no allocation happens on the drop
/// path where this is used.
fn silence_placeholder() -> Pattern {
    thread_local! {
        static SILENCE: Pattern = Pattern::of(Node::Silence);
    }
    SILENCE
        .try_with(Pattern::clone)
        // During thread-local teardown the cache may already be gone; a fresh
        // one is correct, just not free.
        .unwrap_or_else(|_| Pattern::of(Node::Silence))
}

/// Swap a child out for the placeholder, so the parent stops owning it.
fn take_child(child: &mut Pattern, out: &mut Vec<Pattern>) {
    out.push(std::mem::replace(child, silence_placeholder()));
}

/// Move every `Pattern` child out of a node, leaving it childless.
///
/// Children are swapped out rather than destructured because `Node`
/// implements `Drop`, and Rust will not let a value move out of one. The
/// match is EXHAUSTIVE on purpose: a wildcard would let a future combinator
/// reintroduce recursive destruction silently, and the symptom of that is a
/// process abort rather than a test failure. A new variant breaks this build.
///
/// Patterns reached through a `Value` are not drained. A value nests patterns
/// only as deeply as the source literal that wrote it, which the parser
/// bounds, whereas the combinator chain drained here is what a loop can grow
/// without limit.
fn drain_pattern_children(node: &mut Node, out: &mut Vec<Pattern>) {
    fn drain_lookup(lookup: &mut PickLookup, out: &mut Vec<Pattern>) {
        let entries: Vec<&mut Pattern> = match lookup {
            PickLookup::Array { entries, .. } => {
                entries.iter_mut().map(|(_, pattern)| pattern).collect()
            }
            PickLookup::Object { entries, .. } => {
                entries.iter_mut().map(|(_, pattern)| pattern).collect()
            }
        };
        for entry in entries {
            take_child(entry, out);
        }
    }
    match node {
        Node::Silence
        | Node::Pure(_)
        | Node::QueryError(_)
        | Node::QueryLimit(_)
        | Node::Signal(_)
        | Node::JsQuery(_) => {}
        #[cfg(non_send_node_mutation)]
        Node::NonSendMutation(_) => {}
        Node::WithQueryTime(pattern, _)
        | Node::WithHapTime(pattern, _)
        | Node::WithQuerySpan(pattern, _)
        | Node::WithQuerySpanJs(pattern, _)
        | Node::WithHapSpan(pattern, _)
        | Node::Rev(pattern)
        | Node::StepJoin(pattern)
        | Node::PolyJoin(pattern)
        | Node::ArpWith(pattern, _)
        | Node::PurePattern(pattern)
        | Node::PatternOfJs(pattern, _)
        | Node::FmapNative(pattern, _)
        | Node::FilterValues(pattern, _)
        | Node::FilterValuesJs(pattern, _)
        | Node::FilterHaps(pattern, _)
        | Node::FilterHapsJs(pattern, _)
        | Node::FilterWhenJs(pattern, _)
        | Node::SortHapsByPart(pattern)
        | Node::MapHapsNative(pattern, _)
        | Node::MapHapsWithState(pattern, _)
        | Node::ExpandHapsNative(pattern, _)
        | Node::Collect(pattern)
        | Node::WithRandSeed(pattern, _)
        | Node::Degrade(pattern, _, _)
        | Node::InnerJoin(pattern, _)
        | Node::SplitQueries(pattern)
        | Node::FastGap(pattern, _)
        | Node::RepeatCycles(pattern, _)
        | Node::LoopAt(pattern, _)
        | Node::Cpm(pattern, _)
        | Node::MidiKeys(pattern, _)
        | Node::AddContext(pattern, _)
        | Node::PatternOfDynamic(pattern, _)
        | Node::Join(pattern, _)
        | Node::FmapJs(pattern, _) => take_child(pattern, out),
        // The kept constructions are children in every sense that matters
        // here: each is a graph of its own, taken apart on the same worklist.
        Node::PatternOfPure(pattern, _, memo) => {
            take_child(pattern, out);
            memo.drain_into(out);
        }
        Node::Extension(extension) => extension.drain_children(out),
        Node::Arp(a, b)
        | Node::ArpWithPattern(a, b)
        | Node::Timeline(a, b, _)
        | Node::AppLeft(a, b, _, _)
        | Node::AppRight(a, b, _, _)
        | Node::AppBoth(a, b, _, _) => {
            take_child(a, out);
            take_child(b, out);
        }
        Node::PickPatternified {
            selector, lookup, ..
        } => {
            take_child(selector, out);
            take_child(lookup, out);
        }
        Node::ChooseCycles(patterns, _)
        | Node::Stack(patterns)
        | Node::SlowCat(patterns)
        | Node::SlowCatPrime(patterns) => out.extend(std::mem::take(patterns)),
        Node::PatternOfHap(pattern, patterns, _) => {
            take_child(pattern, out);
            out.extend(std::mem::take(patterns));
        }
        Node::WChoose {
            chooser,
            total,
            cumulative,
            values,
            ..
        } => {
            take_child(chooser, out);
            take_child(total, out);
            out.extend(std::mem::take(cumulative));
            out.extend(std::mem::take(values));
        }
        Node::PurePickLookup(_, lookup) => drain_lookup(lookup, out),
        Node::Pick {
            selector, lookup, ..
        } => {
            take_child(selector, out);
            drain_lookup(lookup, out);
        }
    }
}

impl Drop for Node {
    /// Destroy the graph iteratively.
    ///
    /// A recursive drop uses one native frame per level. A chain built by a
    /// loop (`p = p.fast(2).slow(2)`) would overflow the stack on
    /// destruction, and a Rust stack overflow aborts the process: no panic,
    /// no `catch_unwind`, no recovery. The query depth limit does not help,
    /// because the drop can run before any query.
    ///
    /// Children move onto a heap worklist instead, so graph depth costs heap
    /// rather than stack. Every node reached here is childless by the time it
    /// drops, so it recurses no further.
    fn drop(&mut self) {
        LIVE_NODES.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
        let mut pending = Vec::new();
        drain_pattern_children(self, &mut pending);
        while let Some(pattern) = pending.pop() {
            // Only the last owner has a graph to take apart; anyone else is
            // just releasing a reference.
            if let Some(mut node) = Arc::into_inner(pattern.node) {
                drain_pattern_children(&mut node, &mut pending);
            }
        }
    }
}

/// Slice `[0, 1)` at every hap boundary, stack the patterns whose part meets
/// each slice (each carrying its outer hap's context), and `stepcat` the
/// pieces.
///
/// No total step count is computed: Strudel's retiming pass has a
/// destructuring slip that always leaves it undefined, so each slice keeps
/// its own `_steps` or none. Reproduced exactly rather than repaired.
fn step_slices_pattern(resolved: &[(Hap, Pattern)]) -> Pattern {
    let mut breakpoints: Vec<Fraction> = vec![Fraction::ZERO, Fraction::ONE];
    for (hap, _) in resolved {
        breakpoints.push(hap.part.begin);
        breakpoints.push(hap.part.end);
    }
    breakpoints.sort();
    breakpoints.dedup();

    let mut items: Vec<(Option<Fraction>, Pattern)> = Vec::new();
    for window in breakpoints.windows(2) {
        let span = TimeSpan::new(window[0], window[1]);
        // The inner patterns whose part meets this slice, each carrying the
        // outer hap's context.
        let mut layers: Vec<Pattern> = Vec::new();
        for (hap, inner) in resolved {
            if span.intersection(&hap.part).is_none() {
                continue;
            }
            let locations = hap.context.clone();
            layers.push(if locations.is_empty() {
                inner.clone()
            } else {
                inner.with_added_context(locations)
            });
        }
        let stacked = stack(layers);
        // Retiming keeps each pattern's own `_steps`, or leaves it undefined.
        items.push((stacked.steps, stacked));
    }
    crate::combinators::stepcat(&items)
}

/// The per-inner scaling `polyJoin` applies: extend each inner pattern to the
/// outer step count. A missing step count on either side is a query error.
fn poly_extend(pattern: &Pattern, outer_steps: Option<Fraction>) -> Pattern {
    match (outer_steps, pattern.steps) {
        (Some(outer), Some(inner)) if inner != Fraction::ZERO => {
            let Some(factor) = outer.checked_div(inner) else {
                return query_limit_pattern(mark_stepwise_refusal(QueryLimit::NativeFraction {
                    operation: "polyJoin",
                }));
            };
            let fast = pattern.fast(factor);
            let steps = match fast.steps.map(|steps| steps.checked_mul(factor)) {
                Some(None) => {
                    return query_limit_pattern(mark_stepwise_refusal(
                        QueryLimit::NativeFraction {
                            operation: "polyJoin",
                        },
                    ));
                }
                Some(Some(steps)) => Some(steps),
                None => None,
            };
            fast.with_steps(steps)
        }
        _ => {
            signal_query_error(|| "polyJoin: a pattern has no step count".into());
            silence()
        }
    }
}

/// The single place impurity is derived. Adding a `Node` variant without a
/// branch here fails to compile, so a new combinator cannot silently be
/// classified pure.
fn compute_purity(node: &Node) -> purity::Purity {
    use purity::Purity;
    fn value_purity(value: &Value) -> Purity {
        match value {
            Value::Function(function) => function
                .callback_id()
                .map(Purity::with_callback)
                .unwrap_or_else(Purity::pure),
            Value::Pattern(pattern) => Purity::merge(vec![
                pattern.pattern().purity.clone(),
                Purity::with_callback(pattern.id()),
            ]),
            Value::JsValue(reference) => Purity::with_callback(reference.id()),
            Value::List(values) => Purity::merge(values.iter().map(value_purity)),
            Value::Object(values) => {
                Purity::merge(values.iter().map(|(_, value)| value_purity(value)))
            }
            Value::Haps(haps) => {
                Purity::merge(haps.as_slice().iter().map(|hap| value_purity(&hap.value)))
            }
            _ => Purity::pure(),
        }
    }
    match node {
        // Query-time errors and limits hold no callback or JavaScript, so they
        // stay queryable with no host installed.
        Node::Pure(value) => value_purity(value),
        Node::PurePickLookup(_, lookup) => {
            Purity::merge(lookup.patterns().map(|pattern| pattern.purity.clone()))
        }
        Node::Silence | Node::Signal(_) | Node::QueryError(_) | Node::QueryLimit(_) => {
            Purity::pure()
        }

        // Any node holding a callback id is impure and contributes it.
        Node::FmapJs(p, id)
        | Node::WithQuerySpanJs(p, id)
        | Node::FilterValuesJs(p, id)
        | Node::FilterHapsJs(p, id)
        | Node::FilterWhenJs(p, id) => {
            Purity::merge(vec![p.purity.clone(), Purity::with_callback(*id)])
        }
        Node::JsQuery(id) => Purity::with_callback(*id),

        Node::WithQueryTime(p, _)
        | Node::WithHapTime(p, _)
        | Node::WithQuerySpan(p, _)
        | Node::WithHapSpan(p, _)
        | Node::Rev(p)
        | Node::StepJoin(p)
        | Node::PolyJoin(p)
        // Exact inheritance: a known pure inner stays pure.
        | Node::PurePattern(p)
        | Node::FmapNative(p, _)
        | Node::FilterValues(p, _)
        | Node::FilterHaps(p, _)
        | Node::SortHapsByPart(p)
        | Node::MapHapsNative(p, _)
        | Node::MapHapsWithState(p, _)
        | Node::ExpandHapsNative(p, _)
        | Node::Collect(p)
        | Node::WithRandSeed(p, _)
        | Node::Degrade(p, _, _)
        | Node::SplitQueries(p)
        | Node::FastGap(p, _)
        | Node::RepeatCycles(p, _)
        | Node::LoopAt(p, _)
        | Node::Cpm(p, _)
        | Node::AddContext(p, _)
        // Sound: the closure's return type forbids JavaScript.
        | Node::PatternOfPure(p, _, _)
        | Node::Join(p, _)
        | Node::InnerJoin(p, _) => p.purity.clone(),

        // A live port answers with whatever was played since the last query.
        Node::MidiKeys(p, _) => Purity::merge(vec![p.purity.clone(), Purity::volatile()]),

        // The bug this design replaces: inheriting the accumulator's purity
        // through an OPAQUE closure classified `fast("<1 2>").fmapJs(cb)` as
        // pure, because the impure pattern is only materialised at query time.
        // An opaque closure is by definition an unknown path.
        // Impure, contributes its own id, AND opaque: the callback's result may
        // reach callbacks that cannot be enumerated here.
        Node::PatternOfJs(p, id) => Purity::merge(vec![
            p.purity.clone(),
            Purity::with_callback(*id),
            Purity::opaque(),
        ]),

        Node::Extension(extension) => {
            let children =
                Purity::merge_refs(extension.children().iter().map(|pattern| &pattern.purity));
            if extension.volatile() {
                Purity::merge(vec![children, Purity::volatile()])
            } else {
                children
            }
        }
        Node::PatternOfDynamic(p, _) => {
            purity::Purity::merge(vec![p.purity.clone(), purity::Purity::opaque()])
        }
        Node::PatternOfHap(outer, dependencies, _) => {
            let mut parts = vec![outer.purity.clone()];
            parts.extend(
                dependencies
                    .iter()
                    .map(|pattern| pattern.purity.clone()),
            );
            Purity::merge(parts)
        }

        Node::Stack(ps) | Node::SlowCat(ps) | Node::SlowCatPrime(ps) => {
            Purity::merge_refs(ps.iter().map(|p| &p.purity))
        }
        Node::ChooseCycles(ps, _) => Purity::merge_refs(ps.iter().map(|p| &p.purity)),
        Node::WChoose {
            chooser,
            total,
            cumulative,
            values,
            ..
        } => {
            let mut parts = vec![chooser.purity.clone(), total.purity.clone()];
            parts.extend(cumulative.iter().map(|pattern| pattern.purity.clone()));
            parts.extend(values.iter().map(|pattern| pattern.purity.clone()));
            Purity::merge(parts)
        }
        Node::Pick {
            selector, lookup, ..
        } => {
            let mut parts = vec![selector.purity.clone()];
            parts.extend(lookup.patterns().map(|pattern| pattern.purity.clone()));
            Purity::merge(parts)
        }
        Node::PickPatternified {
            selector, lookup, ..
        } => Purity::merge_refs([&selector.purity, &lookup.purity]),
        Node::AppLeft(a, b, _, _)
        | Node::AppRight(a, b, _, _)
        | Node::AppBoth(a, b, _, _) => {
            Purity::merge_refs([&a.purity, &b.purity])
        }

        // `arp` reaches no JavaScript of its own: it is grouping plus an index
        // lookup, so it is exactly as pure as the chord and the indices are.
        Node::Arp(p, indices) => Purity::merge_refs([&p.purity, &indices.purity]),

        // The return pattern is chosen by JavaScript at query time. Its
        // callback reachability cannot be enumerated statically, so this is
        // impure, names its own cell, and is opaque for ownership marking.
        Node::ArpWith(p, id) => Purity::merge(vec![
            p.purity.clone(),
            Purity::with_callback(*id),
            Purity::opaque(),
        ]),
        Node::ArpWithPattern(p, callbacks) => Purity::merge(vec![
            p.purity.clone(),
            callbacks.purity.clone(),
            Purity::opaque(),
        ]),
        // Its offsets are remembered from one query to the next.
        Node::Timeline(time_pattern, pattern, _) => Purity::merge(vec![
            time_pattern.purity.clone(),
            pattern.purity.clone(),
            Purity::volatile(),
        ]),
    }
}

// -- constructors -----------------------------------------------------------

/// A user-authored `new Pattern(state => …)`. **Always impure.**
pub fn js_query(id: CallbackId) -> Pattern {
    Pattern::of(Node::JsQuery(id))
}

pub fn timeline(time_pattern: Pattern, pattern: Pattern, state: TimelineState) -> Pattern {
    Pattern::of(Node::Timeline(time_pattern, pattern, state))
}

/// `arp(indices, pat)` - play a chord one voice at a time.
///
/// `register('arp', ..., false)`: the indices are not patternified, so a
/// pattern argument stays one pattern rather than being lifted hap-by-hap.
pub fn arp(pattern: Pattern, indices: Pattern) -> Pattern {
    Pattern::of(Node::Arp(pattern, indices))
}

/// `arpWith(func, pat)` - select or pattern a chord through a JS callback.
///
/// Always impure: the callback is invoked at query time and may return a
/// pattern that itself reaches callbacks not visible in this graph yet.
pub fn arp_with(pattern: Pattern, callback: CallbackId) -> Pattern {
    Pattern::of(Node::ArpWith(pattern, callback))
}

/// General registered `arpWith` path where callback functions vary in time.
pub fn arp_with_pattern(pattern: Pattern, callbacks: Pattern) -> Pattern {
    Pattern::of(Node::ArpWithPattern(pattern, callbacks))
}

#[inline(never)]
fn query_midi_keys(lengths: &Pattern, port: &Arc<midi_in::InputPort>, state: &State) -> Vec<Hap> {
    let place = state.controls.contains_key("_cps").then(|| {
        (
            i64::try_from(state.span.begin.numer()).ok(),
            i64::try_from(state.span.begin.denom()).ok(),
        )
    });
    let place = place.and_then(|(numerator, denominator)| Some((numerator?, denominator?)));
    let mut hits = Vec::new();
    port.keys.select(
        state.span.begin.to_f64(),
        state.span.end.to_f64(),
        midi_in::now_nanos(),
        place,
        &mut hits,
    );
    // How fast the cycle is running, for turning the distance back to the
    // finger into a distance back in cycles.
    let cps = state
        .controls
        .get("_cps")
        .and_then(|value| match value {
            Value::F64(cps) => Some(*cps),
            _ => None,
        })
        .filter(|cps| cps.is_finite() && *cps > 0.0);
    let mut out = Vec::new();
    for hit in hits {
        let at = Fraction::new(i128::from(hit.num), i128::from(hit.den));
        // Sample the length where the key was struck, not where the note
        // first sounds.
        //
        // A note sounds at the frontier the scheduler has reached, which
        // is a few milliseconds after the key press. Near a boundary in the
        // length pattern, a sample at the frontier can read the wrong side:
        // `keys("0.01 1")` played just before the half cycle would get the
        // long length instead of the short one. The onset cannot move
        // earlier than the frontier, but the strike time is known.
        let struck = cps
            .and_then(|cps| {
                let back = hit.struck_nanos_ago as f64 / 1_000_000_000.0 * cps;
                Fraction::from_f64(back).map(|back| at.sub(back))
            })
            .unwrap_or(at);
        let sampled = lengths.query(&state.set_span(TimeSpan::new(struck, struck)));
        let length = sampled
            .first()
            .filter(|hap| !hap.value.is_nullish())
            .map(|hap| register::value_to_fraction(&hap.value))
            .unwrap_or_else(|| Fraction::from_f64(0.5));
        let Some(length) = length else {
            signal_query_error(|| "Invalid MIDI note length".into());
            return Vec::new();
        };
        let whole = TimeSpan::new(at, at.add(length));
        // The length's own source span comes with it, so the editor can
        // highlight the element that chose it: `keys("0.01 1")` highlights
        // `0.01` or `1`. The note itself comes from a keyboard, not from
        // the score, so no other part of this hap has a source span.
        let context = sampled
            .first()
            .map(|hap| hap.context.clone())
            .unwrap_or_default();
        if !push_budgeted(
            &mut out,
            Hap::new(
                Some(whole),
                whole,
                Value::object([
                    ("note".into(), Value::F64(f64::from(hit.note))),
                    (
                        "velocity".into(),
                        Value::F64(f64::from(hit.velocity) / 127.0),
                    ),
                    ("midichan".into(), Value::F64(f64::from(hit.channel))),
                ]),
            )
            .with_context(context),
        ) {
            return Vec::new();
        }
    }
    out
}

/// `withSeed`'s query, out of `query_node`'s recursive frame.
#[inline(never)]
fn query_with_rand_seed(pat: &Pattern, seed: f64, state: &State) -> Vec<Hap> {
    let mut controls = state.controls.clone();
    controls.insert("randSeed".into(), Value::F64(seed));
    pat.query(&state.set_controls(&controls))
}

/// The port name `.midi()` records when its argument did not reach it as a
/// literal string or number.
///
/// Matched by exact equality, before any device is consulted, so it can be
/// something a musician reading a session log can understand rather than an
/// unprintable byte. Carried as a port name, not as a refusal, because a
/// mistyped device must not take the audio down with it.
pub const UNREADABLE_MIDI_PORT: &str = "(unreadable port name)";

/// A pattern that fails when queried, for a `TypeError` that only surfaces at
/// query time.
///
/// `pat.polyBind(42)` is the reachable case: `fmap(42)` constructs without
/// complaint and calls the non-function later, so the host must build
/// something rather than throw. See
/// `crates/jsruntime/tests/bind_ownership.rs`.
pub fn query_error_pattern(message: &'static str) -> Pattern {
    Pattern::of(Node::QueryError(message))
}

/// A pure pattern that refuses through the typed resource channel when it is
/// queried.
///
/// Resource limits discovered while constructing a registered combinator
/// cannot set the thread-local refusal immediately: a later top-level query
/// intentionally clears stale refusals on entry. Carrying the typed limit in
/// the graph makes it fire at the boundary that can return it to the caller.
pub fn query_limit_pattern(limit: QueryLimit) -> Pattern {
    Pattern::of(Node::QueryLimit(limit))
}

pub fn ref_pattern(id: CallbackId) -> Pattern {
    pure(Value::F64(1.0))
        .fmap_to_pattern(move |_| host_call_ref(id))
        .inner_join()
}

/// `silence` carries one step (`gap(1)`); `nothing` is `gap(0)`. Without the
/// step count, `polyJoin(silence)` takes the missing-`_steps` throw path,
/// which empties the whole `queryArc`, including the haps of a sibling in
/// the same `sequence`.
pub fn silence() -> Pattern {
    Pattern::of(Node::Silence).with_steps(Some(Fraction::ONE))
}

/// `pure(pattern)` - one known inner pattern per cycle. Outer `_steps` is 1,
/// as `pure`'s is.
pub fn pure_pattern(inner: Pattern) -> Pattern {
    Pattern::of(Node::PurePattern(inner)).with_steps(Some(Fraction::ONE))
}

/// A pattern-of-patterns whose inner patterns come from a JS callback.
///
/// `polyBind`/`stepBind` both begin with `fmap`, which carries `_steps`.
/// `polyJoin` divides by the outer step count, so without it every bind
/// would take the missing-`_steps` throw path.
pub fn pattern_of_js(outer: Pattern, id: CallbackId) -> Pattern {
    let steps = outer.steps;
    Pattern::of(Node::PatternOfJs(outer, id)).with_steps(steps)
}

pub fn pure(v: Value) -> Pattern {
    Pattern::of(Node::Pure(v)).with_steps(Some(Fraction::ONE))
}

pub fn pure_pick_lookup(value: Value, lookup: PickLookup) -> Pattern {
    Pattern::of(Node::PurePickLookup(value, lookup)).with_steps(Some(Fraction::ONE))
}

/// A continuous value sampled once at the query span begin.
pub fn signal(f: impl Fn(Fraction, &OrderedMap) -> Value + Send + Sync + 'static) -> Pattern {
    Pattern::of(Node::Signal(Arc::new(move |state| {
        f(state.span.begin, &state.controls)
    })))
}

pub fn state_signal(f: impl Fn(&State) -> Value + Send + Sync + 'static) -> Pattern {
    Pattern::of(Node::Signal(Arc::new(f)))
}

pub fn midi_keys(lengths: Pattern, port: Arc<midi_in::InputPort>) -> Pattern {
    Pattern::of(Node::MidiKeys(lengths, port))
}

pub fn unjoin(pattern: Pattern, pieces: Pattern, function: Option<&value::FunctionRef>) -> Pattern {
    let source = pattern.clone();
    let transform = function.cloned();
    let mut dependencies = vec![pattern];
    if let Some(function) = &transform {
        dependencies.push(pure(Value::Function(function.clone())));
    }
    Pattern::of(Node::PatternOfHap(
        pieces,
        dependencies,
        Arc::new(move |hap| {
            if !hap.value.js_truthy() {
                return source.clone();
            }
            let Some(whole) = hap.whole else {
                signal_query_error(|| {
                    "Cannot read properties of undefined (reading 'begin')".into()
                });
                return silence();
            };
            let section = combinators::ribbon(&source, whole.begin, whole.duration());
            transform
                .as_ref()
                .map_or(section.clone(), |function| function.apply(section))
        }),
    ))
}

pub fn chunk_into(pattern: Pattern, count: f64, function: Option<&value::FunctionRef>) -> Pattern {
    if !count.is_finite() || count.fract() != 0.0 || count < 1.0 {
        return query_error_pattern("Invalid array length");
    }
    let parts = count.min(u64::MAX as f64) as u64;
    if parts > combinators::MAX_ITER_PARTS {
        return query_limit_pattern(QueryLimit::IterParts {
            operation: "chunkInto",
            parts,
            limit: combinators::MAX_ITER_PARTS,
        });
    }
    let selector = fastcat(
        std::iter::once(pure(Value::Bool(true)))
            .chain((1..parts).map(|_| pure(Value::Bool(false))))
            .collect(),
    );
    let pieces = combinators::iter(&selector, Fraction::from(i128::from(parts)), true);
    unjoin(pattern, pieces, function).inner_join()
}

pub fn steady(value: Value) -> Pattern {
    signal(move |_, _| value.clone())
}

pub fn choose_cycles(patterns: Vec<Pattern>, seed: u32) -> Pattern {
    if patterns.is_empty() {
        silence()
    } else {
        Pattern::of(Node::ChooseCycles(patterns, seed))
    }
}

/// `wchoose` / `wchooseCycles`.
///
/// Each pair is `(value_pattern, weight_pattern)`. Weights are accumulated
/// through the ordinary patternified `add.in` route, so their temporal
/// structure and query-time errors are preserved. `cycles` switches the
/// chooser from continuous `rand` to `rand.segment(1)` and the final join
/// from outer to inner - the whole difference between the two.
pub fn wchoose(pairs: Vec<(Pattern, Pattern)>, cycles: bool) -> Pattern {
    let mut values = Vec::with_capacity(pairs.len());
    let mut cumulative = Vec::with_capacity(pairs.len());
    let mut total = pure(Value::F64(0.0));
    for (value, weight) in pairs {
        values.push(value);
        total = compose::compose(
            &total,
            &weight,
            compose::ComposeOp::Add,
            compose::Alignment::In,
        );
        cumulative.push(total.clone());
    }
    let chooser = if cycles {
        combinators::segment(&signal::rand(), Fraction::ONE)
    } else {
        signal::rand()
    };
    Pattern::of(Node::WChoose {
        chooser,
        total,
        cumulative,
        values,
        inner: cycles,
    })
}

/// The shared `_pick` core plus its selected join.
pub fn pick(
    selector: Pattern,
    lookup: PickLookup,
    index_mode: PickIndexMode,
    mode: JoinMode,
) -> Pattern {
    if lookup.enumerable_len() == 0 {
        // `_pick` returns `silence`, then the public family still applies its
        // join. Every join drops `_steps`, including on an empty pattern.
        silence().with_steps(None)
    } else {
        let steps = (mode == JoinMode::Outer)
            .then_some(selector.steps)
            .flatten();
        Pattern::of(Node::Pick {
            selector,
            lookup,
            index_mode,
            mode,
        })
        .with_steps(steps)
    }
}

pub fn pick_patternified(
    selector: Pattern,
    lookup: Pattern,
    index_mode: PickIndexMode,
    mode: JoinMode,
) -> Pattern {
    Pattern::of(Node::PickPatternified {
        selector,
        lookup,
        index_mode,
        mode,
    })
}

/// `pickF`'s historical argument swap is tested against the value delivered
/// by the first argument pattern for EACH hap. A metadata-aware bind keeps
/// that one query and gives array haps their exact holes/keys lookup.
pub fn pick_f_compat(
    receiver: Pattern,
    first: Pattern,
    second: Pattern,
    index_mode: PickIndexMode,
) -> Pattern {
    let steps = first.steps;
    let receiver_dep = receiver.clone();
    let second_dep = second.clone();
    let mapped = Pattern::of(Node::PatternOfHap(
        first,
        vec![receiver.clone(), second.clone()],
        Arc::new(move |hap| {
            let functions = if matches!(hap.value, Value::List(_))
                || matches!(hap.value, Value::JsValue(reference) if reference.is_array())
            {
                let Some(lookup) = hap
                    .pick_lookup
                    .clone()
                    .or_else(|| match hap.value {
                        Value::JsValue(reference) => {
                            host_call_pick_lookup(reference.id()).and_then(scoped_pick_lookup)
                        }
                        _ => None,
                    })
                    .or_else(|| PickLookup::from_value(&hap.value).and_then(scoped_pick_lookup))
                else {
                    if budget_exhausted() || cancellation_requested() || query_deadline_expired() {
                        return silence();
                    }
                    return query_error_pattern("Cannot convert undefined or null to object");
                };
                pick(
                    second_dep.clone(),
                    lookup.as_ref().clone(),
                    index_mode,
                    JoinMode::Inner,
                )
            } else {
                pick_patternified(
                    pure(hap.value.clone()),
                    second_dep.clone(),
                    index_mode,
                    JoinMode::Inner,
                )
            };
            apply_functions_strict(receiver_dep.clone(), functions)
        }),
    ))
    .with_steps(steps);
    mapped.inner_join()
}

/// Value-level function application for the JS `appLeft`/`appBoth` surface:
/// the left pattern's values are JS closures (the
/// `withValue((v) => (x) => ...)` idiom); combining calls the closure with
/// the sampled right value. A non-function left value is a query error,
/// exactly like calling a non-function in JavaScript.
fn call_function_value(f: &Value, x: &Value) -> Value {
    match f {
        Value::Function(function) => match function.callback_id() {
            Some(id) => host_call_value(id, x),
            None => {
                signal_query_error(|| "appLeft function has no callable host".into());
                Value::Undefined
            }
        },
        _ => {
            signal_query_error(|| "appLeft left value is not a function".into());
            Value::Undefined
        }
    }
}

/// `left.appLeft(right)` with function-valued left haps.
pub fn app_left_call(left: &Pattern, right: &Pattern) -> Pattern {
    left.app_left_with(right.clone(), call_function_value)
}

/// `left.appBoth(right)` with function-valued left haps.
pub fn app_both_call(left: &Pattern, right: &Pattern) -> Pattern {
    left.app_both_with(right.clone(), call_function_value)
}

/// `pat.apply(function_pattern)` as `pickF` reaches it: a selected
/// non-function is a query-time TypeError, not an identity fallback.
pub fn apply_functions_strict(receiver: Pattern, functions: Pattern) -> Pattern {
    functions
        .fmap_to_pattern(move |value| match value {
            Value::Function(function) => function.apply(receiver.clone()),
            _ => query_error_pattern("func is not a function"),
        })
        .inner_join()
}

/// Step-count lcm. Unknowns are **removed first**, so a single known step
/// count survives rather than poisoning the result; only an all-unknown list
/// yields `None`.
fn lcm_steps(pats: &[Pattern]) -> Result<Option<Fraction>, ()> {
    // The variadic fraction lcm starts with the final known operand. Keep
    // that ordering, including the sign of a lone step count, while refusing
    // any intermediate result that is outside the native representation.
    let mut known = pats.iter().filter_map(|p| p.steps);
    let Some(mut steps) = known.next_back() else {
        return Ok(None);
    };
    for next in known {
        steps = steps.checked_lcm(next).ok_or(())?;
    }
    Ok(Some(steps))
}

pub fn stack(pats: Vec<Pattern>) -> Pattern {
    let steps = match lcm_steps(&pats) {
        Ok(steps) => steps,
        Err(()) => {
            return query_limit_pattern(mark_stepwise_refusal(QueryLimit::NativeFraction {
                operation: "stack",
            }));
        }
    };
    Pattern::of(Node::Stack(pats)).with_steps(steps)
}

fn slowcat_with_steps(mut pats: Vec<Pattern>, steps: Option<Fraction>) -> Pattern {
    if pats.len() == 1 {
        return pats.pop().expect("one slowcat input");
    }
    Pattern::of(Node::SlowCat(pats))
        .split_queries()
        .with_steps(steps)
}

/// One pattern per cycle.
///
/// A single input is returned as-is - identity, not a wrapper. The
/// `split_queries` is essential: without it a query spanning several cycles
/// is served entirely by whichever pattern the *start* cycle selects.
pub fn slowcat(pats: Vec<Pattern>) -> Pattern {
    if pats.is_empty() {
        return silence();
    }
    let steps = match lcm_steps(&pats) {
        Ok(steps) => steps,
        Err(_) => {
            return query_limit_pattern(mark_stepwise_refusal(QueryLimit::NativeFraction {
                operation: "slowcat",
            }));
        }
    };
    slowcat_with_steps(pats, steps)
}

/// Align every pattern's known step grid, concatenate one slowed pattern per
/// cycle, then restore the least-common step rate.
///
/// This is the graph behind the free `zip(...pats)`. Missing
/// step metadata is filtered before construction. For two or more retained
/// operands, both the retained list and the query-split fanout implied by the
/// LCM share the query-wide stepwise allowance.
pub fn zip(mut pats: Vec<Pattern>) -> Pattern {
    pats.retain(|pattern| pattern.steps.is_some());
    if pats.is_empty() {
        return slowcat_with_steps(Vec::new(), None);
    }

    if pats
        .iter()
        .any(|pattern| pattern.steps.is_some_and(|steps| steps.numer() == 0))
    {
        // The JavaScript boundary throws this eagerly. Keep direct core calls
        // panic-free and non-silent if one bypasses that boundary.
        return query_error_pattern("Division by Zero");
    }

    // Validate every `_slow(pat._steps)` reciprocal before building any of
    // the slowed graph. `checked_div` also catches a denominator outside the
    // native Fraction representation (notably a step count of i128::MIN).
    if pats.iter().any(|pattern| {
        Fraction::ONE
            .checked_div(pattern.steps.expect("filtered known steps"))
            .is_none()
    }) {
        let limit = mark_stepwise_refusal(QueryLimit::NativeFraction { operation: "zip" });
        return query_limit_pattern(limit);
    }

    // Variadic `lcm` seeds its fold with the final operand. Preserve the lone
    // operand's sign while making every actual LCM non-negative.
    let (last, rest) = pats.split_last().expect("non-empty zip inputs");
    let mut steps = last.steps.expect("filtered known steps");
    for pattern in rest {
        let Some(next) = steps.checked_lcm(pattern.steps.expect("filtered known steps")) else {
            let limit = mark_stepwise_refusal(QueryLimit::NativeFraction { operation: "zip" });
            return query_limit_pattern(limit);
        };
        steps = next;
    }

    if pats.len() >= 2 {
        let retained = u64::try_from(pats.len()).unwrap_or(u64::MAX);
        let magnitude = steps.numer().unsigned_abs();
        let denominator = steps.denom() as u128;
        let rounded =
            (magnitude / denominator).saturating_add(u128::from(magnitude % denominator != 0));
        let query_splits = u64::try_from(rounded).unwrap_or(u64::MAX);
        let work = retained.max(query_splits);
        if let Err(limit) = charge_stepwise_entries("zip", work) {
            return query_limit_pattern(limit);
        }
    }

    let retained = u64::try_from(pats.len()).unwrap_or(u64::MAX);
    let mut slowed = Vec::new();
    if slowed.try_reserve_exact(pats.len()).is_err() {
        let limit = mark_stepwise_refusal(QueryLimit::HostMemory);
        return query_limit_pattern(limit);
    }
    for pattern in pats {
        let reciprocal = Fraction::ONE
            .checked_div(pattern.steps.expect("filtered known steps"))
            .expect("zip reciprocals were validated before construction");
        slowed.push(pattern.fast(reciprocal));
    }
    note_stepwise_entries_materialised(retained);

    slowcat_with_steps(slowed, Some(steps))
        .fast(steps)
        .with_steps(Some(steps))
}

/// `slowcatPrime` - like `slowcat`, but **skips** cycles instead of shifting
/// them. The cycle index is Euclidean, so negative cycles wrap instead of
/// going silent.
///
/// `every`/`firstOf`/`lastOf` use this, not `slowcat`; the difference is
/// visible the moment the operand has more than one cycle of its own.
pub fn slowcat_prime(pats: Vec<Pattern>) -> Pattern {
    if pats.is_empty() {
        return silence();
    }
    Pattern::of(Node::SlowCatPrime(pats)).split_queries()
}

/// All patterns squeezed into one cycle.
///
/// The guard is `> 1`, not `> 0`: a single-element `fastcat` is just that
/// element, and keeps ITS step count rather than being forced to 1.
pub fn fastcat(pats: Vec<Pattern>) -> Pattern {
    let n = pats.len();
    if n == 0 {
        return silence();
    }
    if n > 1 {
        // The step count is `n`, so the inputs' step LCM is not computed and
        // cannot refuse.
        slowcat_with_steps(pats, None)
            .fast(Fraction::int(n as i128))
            .with_steps(Some(Fraction::int(n as i128)))
    } else {
        slowcat(pats)
    }
}

#[cfg(test)]
mod step_combination_overflow_tests {
    use super::*;

    fn stepped(steps: Fraction) -> Pattern {
        pure(Value::Str("bd".into())).with_steps(Some(steps))
    }

    fn refusal(pattern: &Pattern) -> Option<QueryLimit> {
        pattern
            .try_query_arc_sorted(Fraction::ZERO, Fraction::ONE)
            .err()
    }

    #[test]
    fn stacking_and_catenation_refuse_unrepresentable_lcm() {
        let pair = || vec![stepped(Fraction::int(i128::MAX)), stepped(Fraction::int(2))];
        for (pattern, operation) in [(stack(pair()), "stack"), (slowcat(pair()), "slowcat")] {
            assert_eq!(
                refusal(&pattern),
                Some(QueryLimit::NativeFraction { operation }),
                "{operation} should refuse an unrepresentable step LCM"
            );
        }
    }

    /// `fastcat`'s step count is its length, so an unrepresentable LCM of its
    /// inputs' step counts does not refuse it.
    #[test]
    fn fastcat_plays_inputs_whose_step_lcm_is_unrepresentable() {
        let pattern = fastcat(vec![
            stepped(Fraction::int(i128::MAX)),
            stepped(Fraction::int(2)),
        ]);
        assert_eq!(pattern.steps, Some(Fraction::int(2)));
        let haps = pattern
            .try_query_arc_sorted(Fraction::ZERO, Fraction::ONE)
            .expect("fastcat plays");
        let wholes: Vec<_> = haps.into_iter().map(|hap| hap.whole).collect();
        assert_eq!(
            wholes,
            [
                Some(TimeSpan::new(Fraction::ZERO, Fraction::new(1, 2))),
                Some(TimeSpan::new(Fraction::new(1, 2), Fraction::ONE)),
            ]
        );
    }

    #[test]
    fn app_both_refuses_unrepresentable_lcm() {
        let pattern = stepped(Fraction::int(i128::MAX))
            .app_both_with(stepped(Fraction::int(2)), |a, _| a.clone());
        assert_eq!(
            refusal(&pattern),
            Some(QueryLimit::NativeFraction {
                operation: "appBoth"
            })
        );
    }

    #[test]
    fn poly_join_refuses_unrepresentable_step_ratio() {
        let pattern = stepped(Fraction::int(i128::MAX))
            .fmap_to_pattern(|_| stepped(Fraction::new(1, 2)))
            .poly_join();
        assert_eq!(
            refusal(&pattern),
            Some(QueryLimit::NativeFraction {
                operation: "polyJoin"
            })
        );
    }

    #[test]
    fn small_step_combinations_keep_their_steps() {
        let pair = || vec![stepped(Fraction::int(3)), stepped(Fraction::int(2))];
        assert_eq!(stack(pair()).steps, Some(Fraction::int(6)));
        assert_eq!(slowcat(pair()).steps, Some(Fraction::int(6)));
        assert_eq!(fastcat(pair()).steps, Some(Fraction::int(2)));
        assert_eq!(
            stepped(Fraction::int(3))
                .app_both_with(stepped(Fraction::int(2)), |a, _| a.clone())
                .steps,
            Some(Fraction::int(6))
        );
    }
}

#[cfg(test)]
mod fastgap_overflow_tests {
    use super::*;

    /// `fastGap` divides a span by its factor, so a small factor far from
    /// cycle zero can leave the i128 fraction range. No factor and no span
    /// may panic: the query returns its haps or a refusal.
    #[test]
    fn a_hostile_gap_is_refused_or_returned_but_never_panics() {
        for (numer, denom) in [(1, 1), (1, 1_000_000), (1, i128::MAX / 4), (3, 7)] {
            let factor = Fraction::new(numer, denom);
            let pattern = pure(Value::Str("x".into())).fast_gap(factor);
            for from in [0i128, 1, 1_000_000, i128::MAX / 8] {
                let begin = Fraction::int(from);
                let end = begin.checked_add(Fraction::ONE).unwrap_or(begin);
                // The assertion is that this returns at all.
                let _ = pattern.try_query_arc_sorted(begin, end);
            }
        }
    }
}

#[cfg(test)]
mod focus_overflow_tests {
    use super::*;

    /// The two spellings a hostile score can put in `focus("…", "…")`:
    /// `arg_fraction` converts them (each is an in-range `i128` rational),
    /// and their difference is `2^128 − 2`, which no native fraction holds.
    /// The unchecked `e − b` used to panic at score-evaluation time, where
    /// nothing on the producer path catches it.
    #[test]
    fn extreme_bounds_refuse_the_window_subtraction() {
        let b = "-170141183460469231731687303715884105727"
            .parse::<Fraction>()
            .expect("an in-range literal converts");
        let e = "170141183460469231731687303715884105727"
            .parse::<Fraction>()
            .expect("an in-range literal converts");
        let focused = pure(Value::Str("bd".into())).focus(b, e);
        assert_eq!(
            focused
                .try_query_arc_sorted(Fraction::ZERO, Fraction::ONE)
                .err(),
            Some(QueryLimit::NativeFraction { operation: "focus" }),
            "focus across the whole i128 range must refuse, not panic"
        );
    }

    /// A span of exactly `i128::MIN` is representable, but its reciprocal is
    /// `−1/2^127`, whose denominator no `i128/i128` fraction holds: `div` is
    /// a component swap for a numerator of one, and the swapped denominator
    /// is the one magnitude past `i128::MAX`. The unchecked reciprocal
    /// panicked there.
    #[test]
    fn a_min_width_window_refuses_the_reciprocal() {
        let focused =
            pure(Value::Str("bd".into())).focus(Fraction::int(i128::MAX), Fraction::int(-1));
        assert_eq!(
            focused
                .try_query_arc_sorted(Fraction::ZERO, Fraction::ONE)
                .err(),
            Some(QueryLimit::NativeFraction { operation: "focus" }),
            "the reciprocal of an i128::MIN-wide window must refuse, not panic"
        );
    }

    /// `b` at `i128::MIN` shifts by a whole cycle of `i128::MIN`. `early(t)`
    /// is `late(t.neg())`, and `i128::MIN` has no `i128` negation. The shift
    /// must not panic.
    #[test]
    fn a_min_whole_cycle_shift_refuses_the_negation() {
        let focused =
            pure(Value::Str("bd".into())).focus(Fraction::int(i128::MIN), Fraction::int(-1));
        assert_eq!(
            focused
                .try_query_arc_sorted(Fraction::ZERO, Fraction::ONE)
                .err(),
            Some(QueryLimit::NativeFraction { operation: "focus" }),
            "shifting by a whole i128::MIN cycle must refuse, not panic"
        );
    }

    /// The ordinary windows the reference documents keep their events and
    /// their refusal-free queries.
    #[test]
    fn ordinary_windows_still_focus_their_events() {
        let bd = pure(Value::Str("bd".into()));
        let halved = bd
            .focus(Fraction::ZERO, Fraction::new(1, 2))
            .try_query_arc_sorted(Fraction::ZERO, Fraction::ONE)
            .expect("focus(0, 1/2) is ordinary");
        assert_eq!(halved.len(), 2, "focus(0, 1/2) is fast(2)");
        let focused = bd
            .focus(Fraction::new(1, 4), Fraction::new(3, 4))
            .try_query_arc_sorted(Fraction::ZERO, Fraction::ONE)
            .expect("focus(1/4, 3/4) is ordinary");
        // The window is half a cycle wide, so its content repeats twice per
        // cycle, and the copy straddling the query start from the previous
        // cycle covers `[0, 1/4)`: bd sounds across the whole cycle, thrice.
        assert_eq!(focused.len(), 3, "the middle half loops to fill the cycle");
        for hap in halved.iter().chain(focused.iter()) {
            assert_eq!(hap.value, Value::Str("bd".into()));
        }
    }
}

#[cfg(test)]
mod time_mapping_overflow_tests {
    use super::*;

    /// One large factor or shift is legal, but a chain of two leaves the
    /// i128 range at query time. Each chain must return the typed
    /// `NativeFraction` refusal instead of a panic.
    #[test]
    fn chained_time_factors_refuse_instead_of_panicking() {
        let bd = || pure(Value::Str("bd".into()));
        let factor = Fraction::from_f64(1e30).expect("1e30 is inside the conversion bound");
        let shift = Fraction::from_f64(1.7e38).expect("1.7e38 is inside the conversion bound");
        let twice_hurried = {
            let once = combinators::hurry(&pure(Value::Str("c".into())), 1e30);
            combinators::hurry(&once, 1e30)
        };
        for (built, operation) in [
            (bd().fast(factor).fast(factor), "fast"),
            // `hurry` is `fast` plus a speed control; the temporal half is
            // what overflows, so the refusal names it.
            (twice_hurried, "fast"),
            (bd().late(shift).late(shift), "late"),
            (bd().early(shift).early(shift), "early"),
            // `slow` is `fast` by the reciprocal; the refusal names the
            // operation the score wrote.
            (
                bd().slow(Fraction::ONE.div(factor))
                    .slow(Fraction::ONE.div(factor)),
                "slow",
            ),
        ] {
            assert!(
                matches!(
                    built.try_query_arc_sorted(Fraction::ZERO, Fraction::ONE),
                    Err(QueryLimit::NativeFraction {
                        operation: got
                    }) if got == operation
                ),
                "the chained {operation} must refuse through the typed channel"
            );
        }
    }

    /// A zoom slot built from exact string literals at ±(2^127 - 1) has a
    /// width (`2^128 - 2`) no i128 fraction can hold. The `e.sub(s)` at
    /// construction used to panic on it; the slot must instead build and
    /// refuse when queried.
    #[test]
    fn an_extreme_zoom_slot_refuses_instead_of_panicking() {
        let begin: Fraction = "-170141183460469231731687303715884105727"
            .parse()
            .expect("-i128::MAX parses exactly");
        let end: Fraction = "170141183460469231731687303715884105727"
            .parse()
            .expect("i128::MAX parses exactly");
        let zoomed = pure(Value::Str("bd".into())).zoom(begin, end);
        assert!(
            matches!(
                zoomed.try_query_arc_sorted(Fraction::ZERO, Fraction::ONE),
                Err(QueryLimit::NativeFraction { operation: "zoom" })
            ),
            "the unrepresentable zoom slot must refuse through the typed channel"
        );
    }

    /// Single arguments at the i128 edge, which a score reaches from a plain
    /// number (`Fraction::from_f64(-2^127)` is exactly `i128::MIN`), must
    /// build without a panic and refuse when queried.
    #[test]
    fn edge_arguments_refuse_at_query_time_instead_of_panicking() {
        let bd = || pure(Value::Str("bd".into()));
        let min = Fraction::from_f64(-(2f64.powi(127))).expect("-2^127 converts exactly");
        assert_eq!(min, Fraction::int(i128::MIN));
        let max = Fraction::int(i128::MAX);
        for (built, operation) in [
            (bd().early(min), "early"),
            (bd().slow(min), "slow"),
            (bd().focus(min, max), "focus"),
        ] {
            assert!(
                matches!(
                    built.try_query_arc_sorted(Fraction::ZERO, Fraction::ONE),
                    Err(QueryLimit::NativeFraction {
                        operation: got
                    }) if got == operation
                ),
                "{operation} at the i128 edge must refuse through the typed channel"
            );
        }
        // Ordinary arguments on the same paths are untouched.
        let slowed = bd()
            .slow(Fraction::int(2))
            .try_query_arc_sorted(Fraction::ZERO, Fraction::int(2))
            .expect("slow(2) is legal");
        assert_eq!(slowed.len(), 1, "slow(2) lost or duplicated events");
        let focused = bd()
            .focus(Fraction::ZERO, Fraction::new(1, 2))
            .try_query_arc_sorted(Fraction::ZERO, Fraction::ONE)
            .expect("focus(0, 1/2) is legal");
        assert_eq!(focused.len(), 2, "focus(0, 1/2) lost or duplicated events");
    }

    /// The single `fast(1e30)` keeps its existing behavior: the span it
    /// requests is refused by the cycle-count guard (`MAX_QUERY_SPAN_CYCLES`),
    /// not by fraction overflow - the mapping itself is representable.
    #[test]
    fn a_single_huge_fast_keeps_its_cycle_span_refusal() {
        let factor = Fraction::from_f64(1e30).expect("1e30 is inside the conversion bound");
        let once = pure(Value::Str("bd".into())).fast(factor);
        assert!(
            matches!(
                once.try_query_arc_sorted(Fraction::ZERO, Fraction::ONE),
                Err(QueryLimit::QuerySpan { .. })
            ),
            "one legal factor must stay the ordinary span refusal, not a new one"
        );
        // An ordinary factor on the same paths is untouched.
        let doubled = pure(Value::Str("bd".into())).fast(Fraction::int(2));
        let haps = doubled
            .try_query_arc_sorted(Fraction::ZERO, Fraction::ONE)
            .expect("fast(2) is legal");
        assert_eq!(haps.len(), 2, "fast(2) lost or duplicated events");
    }
}

#[cfg(test)]
mod query_in_progress_tests {
    //! `query_in_progress` distinguishes ordinary score construction from callbacks
    //! reached by a query or a construction-time probe.

    use super::*;
    use std::sync::{Arc, Mutex};

    /// Ordinary construction is outside a query. The `stepJoin` cycle-0 probe
    /// counts as a query because it calls the pattern's function. Its boundary
    /// remains active after the carrier's `Pattern::query` call returns.
    #[test]
    fn a_construction_probe_and_a_query_are_in_progress_and_construction_is_not() {
        assert!(!query_in_progress(), "nothing is being queried");
        let seen: Arc<Mutex<Vec<bool>>> = Arc::default();
        let record = seen.clone();
        let carrier = pure(Value::Str("bd".into())).fmap_to_pattern(move |value| {
            record.lock().expect("record").push(query_in_progress());
            pure(value.clone())
        });

        let joined = carrier.try_step_join().expect("stepJoin");
        assert!(!query_in_progress(), "the probe's boundary was left open");
        assert_eq!(
            *seen.lock().expect("seen"),
            [true],
            "the construction-time probe must count as a query"
        );

        let haps = joined.query_arc_sorted(Fraction::ZERO, Fraction::ONE);
        assert!(!haps.is_empty(), "the joined pattern plays");
        assert!(!query_in_progress(), "the query's boundary was left open");
        let seen = seen.lock().expect("seen");
        assert!(
            seen.len() > 1 && seen.iter().all(|inside| *inside),
            "a query's callbacks run inside it: {seen:?}"
        );
    }
}

#[cfg(test)]
mod slowcat_overflow_tests {
    //! `slowcat` answers every query whose shifted times fit the native fraction
    //! range and refuses the rest as `NativeFraction { operation: "slowcat" }`.

    use super::*;

    fn bd_sd() -> Pattern {
        slowcat(vec![
            pure(Value::Str("bd".into())),
            pure(Value::Str("sd".into())),
        ])
    }

    #[test]
    fn a_large_time_denominator_still_gets_its_exact_answer() {
        let denominator = 100_000_000_000_000_000_000_000_000_000_000_000_000i128;
        let begin = Fraction::new(1, denominator);
        let end = Fraction::new(2, denominator);
        let haps = bd_sd()
            .try_query_arc_sorted(begin, end)
            .expect("the exact answer fits");
        assert_eq!(haps.len(), 1);
        assert_eq!(haps[0].value, Value::Str("bd".into()));
        assert_eq!(haps[0].part, TimeSpan::new(begin, end));
        assert_eq!(
            haps[0].whole,
            Some(TimeSpan::new(Fraction::ZERO, Fraction::ONE))
        );
    }

    #[test]
    fn a_hap_shifted_past_the_native_range_refuses() {
        let pat = slowcat(vec![
            pure(Value::Str("bd".into())).slow(Fraction::int(i128::MAX)),
            pure(Value::Str("sd".into())),
        ]);
        assert_eq!(
            pat.try_query_arc_sorted(Fraction::int(2), Fraction::int(3))
                .err(),
            Some(QueryLimit::NativeFraction {
                operation: "slowcat"
            })
        );
    }

    #[test]
    fn ordinary_slowcat_queries_keep_their_cycle_order() {
        let values: Vec<_> = bd_sd()
            .try_query_arc_sorted(Fraction::ZERO, Fraction::int(4))
            .expect("ordinary slowcat query")
            .into_iter()
            .map(|hap| hap.value)
            .collect();
        assert_eq!(
            values,
            ["bd", "sd", "bd", "sd"].map(|value| Value::Str(value.into()))
        );
    }
}

#[cfg(test)]
mod pattern_of_join_resolution_tests {
    use super::*;

    /// `seqPLoop`'s exact graph shape:
    /// `stack(pure(p).compress(a, b), ...).slow(total).innerJoin()`.
    /// The join must resolve through the stack and fastGap wrappers.
    #[test]
    fn a_join_over_stacked_compressed_patterns_resolves_each_window() {
        let bd = pure(Value::Str("bd".into()));
        let cp = pure(Value::Str("cp".into()));
        let sections = stack(vec![
            pure_pattern(bd).compress(Fraction::ZERO, Fraction::new(2, 3)),
            pure_pattern(cp).compress(Fraction::new(1, 3), Fraction::ONE),
        ]);
        let pattern = sections.slow(Fraction::int(3)).inner_join();
        let haps = pattern
            .try_query_arc_sorted(Fraction::ZERO, Fraction::int(4))
            .expect("the seqPLoop shape must not be refused");
        assert!(!haps.is_empty(), "seqPLoop windows must produce haps");
        for hap in &haps {
            assert!(
                matches!(hap.value, Value::Str(_)),
                "a pattern-valued hap leaked through the join: {hap:?}"
            );
        }
        // bd owns [0, 2) and [3, 4) of the four queried cycles; cp owns
        // [1, 3). Both branches must be audible.
        let has_bd = haps.iter().any(|hap| hap.value == Value::Str("bd".into()));
        let has_cp = haps.iter().any(|hap| hap.value == Value::Str("cp".into()));
        assert!(has_bd, "the first seqPLoop section is missing: {haps:?}");
        assert!(has_cp, "the second seqPLoop section is missing: {haps:?}");
    }
}

#[cfg(test)]
mod callback_query_metrics_tests {
    use super::*;

    struct NestedHost;

    impl CallbackHost for NestedHost {
        fn call_value(&self, id: CallbackId, value: &Value) -> Result<Value, String> {
            if id == 0 {
                let _ = host_call_value(1, value);
            }
            Ok(value.clone())
        }

        fn call_query(&self, _id: CallbackId, _state: &State) -> Result<Vec<Hap>, String> {
            Ok(Vec::new())
        }
    }

    #[test]
    fn callback_metrics_count_nested_crossings_without_losing_scope() {
        let host = NestedHost;
        let mut metrics = CallbackQueryMetrics::default();
        with_callback_host(&host, || {
            with_callback_query_metrics_policy(&mut metrics, true, || {
                let value = Value::F64(1.0);
                assert_eq!(host_call_value(0, &value), value);
            });
        });
        assert_eq!(metrics.calls(), 2);
        let kinds = metrics.kind_sample().expect("forced callback-kind sample");
        assert_eq!(kinds.total(), metrics.calls());
        assert_eq!(kinds.get(CallbackQueryKind::Value), 2);
        for kind in CallbackQueryKind::ALL {
            if kind != CallbackQueryKind::Value {
                assert_eq!(kinds.get(kind), 0, "unexpected {kind:?}");
            }
        }
        assert!(metrics.busy_nanos() > 0);
        CALLBACK_QUERY_DEPTH.with(|depth| assert_eq!(depth.get(), 0));
        CALLBACK_QUERY_METRICS.with(|slot| assert!(slot.get().is_none()));
    }

    #[test]
    fn nested_metric_recorders_restore_the_outer_recorder() {
        let host = NestedHost;
        let mut outer = CallbackQueryMetrics::default();
        let mut inner = CallbackQueryMetrics::default();
        with_callback_host(&host, || {
            with_callback_query_metrics_policy(&mut outer, true, || {
                let value = Value::F64(1.0);
                let _ = host_call_value(1, &value);
                with_callback_query_metrics_policy(&mut inner, true, || {
                    let _ = host_call_value(1, &value);
                });
                let _ = host_call_value(1, &value);
            });
        });
        assert_eq!(outer.calls(), 2);
        assert_eq!(inner.calls(), 1);
        assert_eq!(
            outer
                .kind_sample()
                .expect("outer kind sample")
                .get(CallbackQueryKind::Value),
            2
        );
        assert_eq!(
            inner
                .kind_sample()
                .expect("inner kind sample")
                .get(CallbackQueryKind::Value),
            1
        );
    }

    #[test]
    fn an_unsampled_kind_census_keeps_the_exact_aggregate() {
        let host = NestedHost;
        let mut metrics = CallbackQueryMetrics::default();
        with_callback_host(&host, || {
            with_callback_query_metrics_policy(&mut metrics, false, || {
                let _ = host_call_value(1, &Value::F64(1.0));
            });
        });
        assert_eq!(metrics.calls(), 1);
        assert!(metrics.kind_sample().is_none());
    }

    #[test]
    fn each_host_call_reads_the_installed_host_and_counts_its_kind() {
        let host = NestedHost;
        let mut metrics = CallbackQueryMetrics::default();
        let value = Value::F64(1.0);
        let haps = pure(value.clone()).query_arc(Fraction::ZERO, Fraction::ONE);
        let span = TimeSpan::new(Fraction::ZERO, Fraction::ONE);
        let program = callback_ir::PatternTransformProgram::new([]).unwrap();
        with_callback_host(&host, || {
            with_callback_query_metrics_policy(&mut metrics, true, || {
                let _ = host_call_value(1, &value);
                let _ = host_call_value_predicate(1, &value);
                let _ = host_call_ref(1);
                let _ = host_call_pick_lookup(1);
                let _ = host_call_pattern(1, pure(value.clone()));
                let _ = host_call_pattern_ir(1, &program, pure(value.clone()));
                let _ = host_call_pattern_indexed_batch(1, vec![(pure(value.clone()), 0)]);
                let _ = host_call_haps(1, &haps);
                let _ = host_call_hap_predicate(1, &haps[0]);
                let _ = host_call_time_predicate(1, Fraction::ZERO);
                let _ = host_call_span_transform(1, span);
                let _ = host_call_bind(1, BindArg::Value(&value));
                let _ = host_call_query(1, &State::new(span));
            });
        });
        let kinds = metrics.kind_sample().expect("forced callback-kind sample");
        for kind in CallbackQueryKind::ALL {
            let expected = match kind {
                // The value call and the value predicate share this kind.
                CallbackQueryKind::Value => 2,
                CallbackQueryKind::MaterializeValue => 0,
                _ => 1,
            };
            assert_eq!(kinds.get(kind), expected, "{kind:?}");
        }
    }
}

#[cfg(test)]
mod set_lookup_tests {
    use super::*;

    fn object(entries: &[(&str, Value)]) -> Value {
        Value::object(
            entries
                .iter()
                .map(|(key, value)| ((*key).into(), value.clone())),
        )
    }

    fn entries(lookup: &PickLookup) -> Vec<(String, Value)> {
        let PickLookup::Object { entries, .. } = lookup else {
            panic!("a set lookup is an object");
        };
        entries
            .iter()
            .map(|(key, pattern)| (key.clone(), pattern.as_pure().expect("a pure entry")))
            .collect()
    }

    #[test]
    fn a_skipped_set_lookup_equals_the_lookup_of_the_merged_value() {
        let members = [
            Value::F64(0.0),
            Value::F64(-0.0),
            Value::F64(1.0),
            Value::Bool(true),
            Value::Str("x".into()),
            Value::List(vec![Value::F64(-0.0)]),
        ];
        let mut values = vec![Value::Undefined, Value::Null, Value::Str("ab".into())];
        values.extend(members.iter().cloned());
        for key in ["value", "0", "a"] {
            for member in &members {
                values.push(object(&[(key, member.clone())]));
            }
        }
        for first in &members {
            for second in &members {
                values.push(object(&[("value", first.clone()), ("a", second.clone())]));
            }
        }

        let span = TimeSpan::new(Fraction::ZERO, Fraction::ONE);
        let mut skipped = 0;
        for left in &values {
            for right in &values {
                let left_hap = Hap::new(Some(span), span, left.clone());
                let right_hap = Hap::new(Some(span), span, right.clone());
                for merged in [
                    controls::set_value(left, right),
                    compose::compose_op(compose::ComposeOp::Set, left, right),
                ] {
                    if !set_lookup_follows_value(&merged, &left_hap, &right_hap) {
                        continue;
                    }
                    skipped += 1;
                    // The lookup the merge stored before, then the one a pick derives.
                    let Some(eager) = (merged != *right)
                        .then(|| PickLookup::set_object(&left_hap, &right_hap, &merged))
                        .flatten()
                    else {
                        continue;
                    };
                    let derived = PickLookup::from_value(&merged).expect("an object value");
                    assert_eq!(eager.enumerable_len(), derived.enumerable_len());
                    let (eager, derived) = (entries(&eager), entries(&derived));
                    assert_eq!(eager.len(), derived.len());
                    for ((eager_key, eager_value), (key, value)) in eager.iter().zip(&derived) {
                        assert!(
                            eager_key == key && same_key(eager_value, value),
                            "{left:?} set {right:?}: {eager_key} {eager_value:?}, {key} {value:?}"
                        );
                    }
                }
            }
        }
        assert!(skipped > 1_000, "only {skipped} merges skipped the lookup");
    }
}
