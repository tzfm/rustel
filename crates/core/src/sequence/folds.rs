//! Pattern-level sequence folds: stacking, concatenating, dressing, meeting,
//! shifting, and rewriting folded sequences during score construction.

use super::*;

/// Whether moving these layers by `by` leaves every step in place.
pub(crate) fn repeats_at(layers: &[Layer], by: Fraction, span: TimeSpan) -> bool {
    // Both readings must fit, including each whole before the query cuts it.
    let small =
        |time: Fraction| i32::try_from(time.numer()).is_ok() && i32::try_from(time.denom()).is_ok();
    if by.denom() != 1 || !small(by) || !small(span.begin) || !small(span.end) {
        return false;
    }
    layers.iter().all(|layer| {
        let period = layer.period();
        // An integer multiple of the reduced numerator is a whole number of periods.
        layer.listed().is_some_and(Listing::small_denominators)
            && small(period)
            && (period.numer() == 1 || (by.numer() as i32) % (period.numer() as i32) == 0)
    })
}

/// Whether each hap of `layers` holds data only.
pub(crate) fn holds_data(layers: &[Layer]) -> bool {
    layers.iter().all(Layer::is_data)
}

/// Whether the graph of `pattern` holds each value that it answers, and
/// each is data only: a sequence or a constant of data values.
pub(crate) fn is_data(pattern: &Pattern) -> bool {
    match pattern.node_kind() {
        Node::Sequence(layers) => holds_data(layers),
        Node::IntRange { .. } | Node::SegmentSignal { .. } => true,
        Node::Pure(value) => value.is_data(0),
        Node::PureAt(boxed) => boxed.0.is_data(0),
        Node::AddContext(boxed) => is_data(&boxed.0),
        _ => false,
    }
}

/// The one data value of `layers`, when they are one value that fills each
/// cycle, as a `pure` is.
pub(crate) fn constant_value(layers: &[Layer]) -> Option<Cow<'_, Value>> {
    let cycle = TimeSpan::new(Fraction::ZERO, Fraction::ONE);
    match layers {
        // A reader makes a hap of the value alone, so keys make no constant.
        [layer] if layer.period() == Fraction::ONE && layer.dress().keys().is_empty() => {
            match layer.one_step() {
                Some((whole, false, value)) if whole == cycle => layer
                    .value(value as usize)
                    .filter(|value| value.is_data(0))
                    .map(Cow::Owned),
                _ => None,
            }
        }
        _ => None,
    }
}

/// The steps in a cycle of `layers` and their value, when the layers are
/// one data value that fills each of `steps` equal steps and holds nothing
/// more, as the pulse of a `segment` is. Nothing is made for a layer that
/// is not one, so that asking costs no memory.
pub(crate) fn pulse(layers: &[Layer]) -> Option<(i128, &Value)> {
    let [layer] = layers else {
        return None;
    };
    let period = layer.period();
    if period.numer() != 1 || !layer.dress().is_bare() {
        return None;
    }
    let first = TimeSpan::new(Fraction::ZERO, period);
    let (whole, cut, value) = layer.one_step()?;
    if whole != first || cut || !layer.locations_of(0).is_empty() {
        return None;
    }
    // A bare dress gives each value as the step holds it.
    let value = layer.values.get(value as usize)?;
    value.is_data(0).then_some((period.denom(), value))
}

#[cfg(test)]
thread_local! {
    /// Set by a test to build each node that a fold stands for.
    static UNFOLDED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Build with each node that a fold stands for: the graph that a fold must
/// answer as, and the graph of a test that reads the kinds of its nodes.
#[cfg(test)]
pub(crate) fn unfolded<R>(build: impl FnOnce() -> R) -> R {
    struct Restore(bool);
    impl Drop for Restore {
        fn drop(&mut self) {
            UNFOLDED.set(self.0);
        }
    }
    let _restore = Restore(UNFOLDED.replace(true));
    build()
}

/// Whether a test builds each node that a fold stands for now. See
/// [`unfolded`].
pub(crate) fn builds_each_node() -> bool {
    #[cfg(test)]
    return UNFOLDED.get();
    #[cfg(not(test))]
    false
}

/// Whether an application on `pattern` may fold into its steps: it is a
/// sequence or a constant.
pub(crate) fn holds_steps(pattern: &Pattern) -> bool {
    folded(pattern).is_some()
        || constant(pattern).is_some()
        || matches!(pattern.node_kind(), Node::IntRange { count, .. } if *count <= 1024 && folds(pattern))
}

/// Whether a constructor may fold `pattern`: its handle is not marked
/// volatile, and the runtime that it binds, if any, changes no answer.
pub(crate) fn folds(pattern: &Pattern) -> bool {
    if builds_each_node() {
        return false;
    }
    // A runtime goes to the functions and the patterns that a value holds.
    // Silence and data hold none, so they answer the same under each
    // runtime.
    let reads_no_runtime = || pattern.is_rest() || is_data(pattern);
    !pattern.is_volatile() && (!pattern.has_runtime_settings() || reads_no_runtime())
}

/// The layers of `pattern`, when it is a sequence that a constructor folds
/// into.
pub(super) fn folded(pattern: &Pattern) -> Option<&[Layer]> {
    match pattern.node_kind() {
        Node::Sequence(layers) if folds(pattern) => Some(layers),
        _ => None,
    }
}

/// A hap of a plain value that fills cycle zero, with no location.
pub(super) fn cycle_hap(value: Value) -> Hap {
    let cycle = TimeSpan::new(Fraction::ZERO, Fraction::ONE);
    Hap::new(Some(cycle), cycle, value)
}

/// Whether `hap` holds a value and its locations and nothing more: what a
/// step of a sequence holds.
pub(super) fn is_plain(hap: &Hap) -> bool {
    hap.keys.is_empty()
}

/// Whether `hap` holds a value, its locations and the keys of its context,
/// and nothing more: what a step holds with the keys of its layer.
pub(super) fn holds_a_value_and_keys(hap: &Hap) -> bool {
    hap.keys.has_only_context_keys()
}

/// The value that `values` answers in each cycle on one hap that fills the
/// cycle, when the graph says it with no query: a `pure` value, at its
/// locations, or a sequence of one value that fills each cycle.
pub(super) fn constant_value_of(values: &Pattern) -> Option<Cow<'_, Value>> {
    if !folds(values) {
        return None;
    }
    match values.node_kind() {
        Node::Pure(value) if value.is_data(0) => Some(Cow::Borrowed(value)),
        Node::PureAt(boxed) if boxed.0.is_data(0) => Some(Cow::Borrowed(&boxed.0)),
        Node::AddContext(boxed) => constant_value_of(&boxed.0),
        Node::Sequence(layers) => constant_value(layers),
        _ => None,
    }
}

/// The locations of the hap of `values`, a [constant](constant_value_of).
pub(super) fn constant_locations(values: &Pattern) -> Locations {
    match values.node_kind() {
        Node::PureAt(boxed) => boxed.1.clone(),
        Node::AddContext(boxed) => {
            let mut context = constant_locations(&boxed.0);
            context.extend_from_slice(&boxed.1);
            context
        }
        Node::Sequence(layers) => layers[0].context(0),
        _ => Locations::new(),
    }
}

/// The hap that `values` answers in each cycle, as of cycle zero, when the
/// graph says it with no query. See [`constant_value_of`].
pub(crate) fn constant(values: &Pattern) -> Option<Hap> {
    // A handle of the value, for the hap to hold.
    let mut hap = cycle_hap(constant_value_of(values)?.into_owned());
    hap.context = constant_locations(values);
    Some(hap)
}

/// Whether `values` is a constant of a plain data value, by its kinds
/// alone. See [`constant_value_at`].
pub(crate) fn is_constant_value(values: &Pattern) -> bool {
    constant_value_of(values).is_some()
}

/// The layers that `pattern` answers as, when it is a sequence or a
/// constant: a constant is one step that fills each cycle.
pub(super) enum Layers<'a> {
    Borrowed(&'a [Layer]),
    Single([Layer; 1]),
}

impl std::ops::Deref for Layers<'_> {
    type Target = [Layer];
    #[inline]
    fn deref(&self) -> &[Layer] {
        match self {
            Self::Borrowed(slice) => slice,
            Self::Single(single) => single,
        }
    }
}

impl Layers<'_> {
    fn into_owned(self) -> Vec<Layer> {
        match self {
            Self::Borrowed(slice) => slice.to_vec(),
            Self::Single([layer]) => vec![layer],
        }
    }
}

fn int_range_layer(
    start: i32,
    count: u32,
    loc0: u32,
    loc1: u32,
    named_id: u16,
    step_neg: bool,
) -> Option<Layer> {
    let count_usize = usize::try_from(count).ok()?;
    if count_usize == 0 || count_usize > 1024 {
        return None;
    }
    let ticks = NonZeroU32::new(count)?;
    let dir = if step_neg { -1i64 } else { 1i64 };
    let values: Arc<[Value]> = (0..count_usize)
        .map(|i| Value::F64((i64::from(start) + (i as i64) * dir) as f64))
        .collect();
    let steps = GridSteps::build(count_usize, |index| {
        Some(GridStep {
            part_begin: index as u32,
            part_end: (index + 1) as u32,
            value: index as u32,
            loc: PackedLoc(0),
        })
    })?;
    let (steps, rare) = match steps {
        GridSteps::Wide(wide_steps) => (
            GridSteps::Small(Box::from([])),
            Some(Box::new(SpareRare::WideSteps(wide_steps))),
        ),
        other => (other, None),
    };
    let listing = Listing::from_parts(ticks, Grid::SMALL, steps, PackedRunsInput::None, rare);
    let mut layer = Layer::holding_classified(
        Fraction::ONE,
        Steps::Listed(listing),
        true,
        values,
        true,
        false,
        true,
    );
    if loc0 != 0 {
        let mut ranges = [(0usize, 0usize); 2];
        let mut n = 1;
        ranges[0] = crate::unpack_run_loc(loc0);
        if loc1 != 0 {
            ranges[1] = crate::unpack_run_loc(loc1);
            n = 2;
        }
        layer.change_dress(|dress| dress.extend_after(&ranges[..n]));
    }
    if named_id != 0 {
        let spec = crate::controls::builtin_spec_by_id(named_id)?;
        Layer::named_in_place(std::slice::from_mut(&mut layer), spec);
    } else {
        layer.refresh_direct();
    }
    Some(layer)
}

pub(super) fn layers(pattern: &Pattern) -> Option<Layers<'_>> {
    if let Some(layers) = folded(pattern) {
        return Some(Layers::Borrowed(layers));
    }
    if folds(pattern)
        && let Node::IntRange {
            start,
            count,
            loc0,
            loc1,
            named_id,
            step_neg,
        } = pattern.node_kind()
        && *count <= 1024
    {
        return Some(Layers::Single([int_range_layer(
            *start, *count, *loc0, *loc1, *named_id, *step_neg,
        )?]));
    }
    let hap = constant(pattern)?;
    let raw = Raw {
        whole: hap.whole()?,
        part: None,
        value: 0,
        locations: hap.context,
    };
    Some(Layers::Single([Layer::of_single(
        Fraction::ONE,
        raw,
        crate::arc_single_value(hap.value),
    )]))
}

/// `layers` with each value that `change` makes of a value of them, as one
/// sequence. `None` when `change` gives none, or raises a query error: the
/// error belongs to each query of the node that the fold stands for.
pub(super) fn revalued(
    layers: Vec<Layer>,
    mut change: impl FnMut(&Value) -> Option<Value>,
) -> Option<Pattern> {
    without_query_signals(|| {
        let layers = layers
            .into_iter()
            .map(|layer| {
                let values: Option<Vec<Value>> =
                    layer.each_value().map(|value| change(&value)).collect();
                Some(layer.valued(values?.into()))
            })
            .collect::<Option<Vec<_>>>()?;
        Some(node_of_sound(layers))
    })
}

/// `pattern` with `locations` after the locations of each hap, as
/// [`Pattern::with_added_context`] gives, when `pattern` is a sequence. A
/// `pure` keeps its own node, which says that it is one.
pub(crate) fn located(pattern: &Pattern, locations: &[(usize, usize)]) -> Option<Pattern> {
    let layers = folded(pattern)?;
    if layers.len() <= 8 {
        let mut buf: [Option<Layer>; 8] = Default::default();
        let mut last: Option<(*const Dress, Arc<Dress>)> = None;
        for (slot, layer) in buf.iter_mut().zip(layers.iter()) {
            let from = layer.dress.as_ref().map_or(std::ptr::null(), Arc::as_ptr);
            let dress = match &last {
                Some((seen, dress)) if *seen == from => Arc::clone(dress),
                _ => {
                    let mut dress = layer.dress().clone();
                    dress.extend_after(locations);
                    let dress = dress.into_arc();
                    last = Some((from, Arc::clone(&dress)));
                    dress
                }
            };
            *slot = Some(layer.with(|l| {
                l.dress = Some(dress);
                l.refresh_direct();
            }));
        }
        return Some(node_of_sound_stack(&mut buf, layers.len()));
    }
    let layers = redressed_arc(layers, |dress| dress.extend_after(locations));
    Some(Pattern::of(Node::Sequence(layers)))
}

/// Append `locations` onto `layers` of an owned `Node::Sequence` in place when
/// unshared, avoiding `Layer`, `Arc<[Layer]>`, and `NodeCell` cloning.
pub(crate) fn locate_in_place(layers: &mut Arc<[Layer]>, locations: &[(usize, usize)]) -> bool {
    let Some(slice) = Arc::get_mut(layers) else {
        return false;
    };
    let mut last: Option<(*const Dress, Arc<Dress>)> = None;
    for layer in slice {
        let from = layer.dress.as_ref().map_or(std::ptr::null(), Arc::as_ptr);
        if !from.is_null()
            && let Some(dress) = layer.dress.as_mut().and_then(Arc::get_mut)
        {
            dress.extend_after(locations);
            layer.refresh_direct();
            continue;
        }
        let dress = match &last {
            Some((seen, dress)) if *seen == from => Arc::clone(dress),
            _ => {
                let mut dress = layer.dress().clone();
                dress.extend_after(locations);
                let dress = dress.into_arc();
                last = Some((from, Arc::clone(&dress)));
                dress
            }
        };
        layer.dress = Some(dress);
        layer.refresh_direct();
    }
    true
}

/// Set UI visual `slot` on `layers` of an owned `Node::Sequence` in place when
/// unshared, avoiding `Layer` and `NodeCell` cloning.
pub(crate) fn with_ui_visual_slot_in_place(layers: &mut Arc<[Layer]>, slot: u8) -> bool {
    let Some(slice) = Arc::get_mut(layers) else {
        return false;
    };
    let mut last: Option<(*const Dress, Arc<Dress>)> = None;
    for layer in slice {
        if let Some(dress) = layer.dress.as_mut().and_then(Arc::get_mut) {
            dress.add_ui_visual_slot(slot);
            continue;
        }
        let from = layer.dress.as_ref().map_or(std::ptr::null(), Arc::as_ptr);
        let dress = match &last {
            Some((seen, dress)) if *seen == from => Arc::clone(dress),
            _ => {
                let mut dress = layer.dress().clone();
                dress.add_ui_visual_slot(slot);
                let dress = dress.into_arc();
                last = Some((from, Arc::clone(&dress)));
                dress
            }
        };
        layer.dress = Some(dress);
    }
    true
}

/// `layers` with UI visual `slot` folded into every layer's dress.
pub(crate) fn with_ui_visual_slot_layers(layers: &[Layer], slot: u8) -> Arc<[Layer]> {
    redressed_arc(layers, |dress| dress.add_ui_visual_slot(slot))
}

/// `selector`, the sequence that selects for a pick, with `locations`
/// before the locations of each hap, when each hap of it holds its value
/// and its locations alone. The join of a pick puts the locations of its
/// selector first and keeps the keys of the hap that it picked, so a change
/// of the picked patterns is then a change of the pick. With no location,
/// the selector as it is.
///
/// `whole_cycles` asks more: each step begins on a whole cycle, so a
/// restart or a reset moves the picked pattern by whole cycles, and its
/// cycles stay the cycles of the query.
/// Whether `selector` is a plain selector sequence (no keys, and whole-cycle
/// steps when `whole_cycles` is true).
pub(crate) fn is_plain_selector(selector: &Pattern, whole_cycles: bool) -> bool {
    let whole = |time: Fraction| time.denom() == 1;
    let Some(layers) = folded(selector) else {
        return false;
    };
    layers.iter().all(|layer| {
        layer.dress().keys().is_empty()
            && (!whole_cycles
                || (whole(layer.period())
                    && (0..layer.count()).all(|index| whole(layer.listing().whole(index).begin))))
    })
}

/// Prepend `locations` onto `selector` in place when `selector` is uniquely
/// owned, falling back to [`selecting`] otherwise.
pub(crate) fn selecting_in_place(
    selector: &mut Pattern,
    locations: &Locations,
    whole_cycles: bool,
) -> bool {
    if !is_plain_selector(selector, whole_cycles) {
        return false;
    }
    if locations.is_empty() {
        return true;
    }
    let steps = selector.steps();
    let runtime = selector.runtime_settings().cloned();
    if let Some(cell) = selector.own_node()
        && let Node::Sequence(layers) = &mut cell.kind
    {
        if let Some(slice) = Arc::get_mut(layers) {
            let mut last: Option<(*const Dress, Arc<Dress>)> = None;
            for layer in slice.iter_mut() {
                let from = layer.dress.as_ref().map_or(std::ptr::null(), Arc::as_ptr);
                if !from.is_null()
                    && let Some(dress) = layer.dress.as_mut().and_then(Arc::get_mut)
                {
                    dress.prepend_before(locations.clone());
                    layer.refresh_direct();
                    continue;
                }
                let dress = match &last {
                    Some((seen, dress)) if *seen == from => Arc::clone(dress),
                    _ => {
                        let mut dress = layer.dress().clone();
                        dress.prepend_before(locations.clone());
                        let dress = dress.into_arc();
                        last = Some((from, Arc::clone(&dress)));
                        dress
                    }
                };
                layer.dress = Some(dress);
                layer.refresh_direct();
            }
        } else {
            *layers = redressed_arc(layers, |dress| dress.prepend_before(locations.clone()));
        }
        let node = Arc::clone(&selector.node);
        *selector = Pattern::holding(node).with_steps(steps);
        if let Some(runtime) = runtime {
            selector.attach_runtime_settings(runtime);
        }
        return true;
    }
    let Some(next) = selecting(selector, locations, whole_cycles) else {
        return false;
    };
    *selector = next;
    true
}

pub(crate) fn selecting(
    selector: &Pattern,
    locations: &Locations,
    whole_cycles: bool,
) -> Option<Pattern> {
    if !is_plain_selector(selector, whole_cycles) {
        return None;
    }
    if locations.is_empty() {
        return Some(selector.clone());
    }
    let layers = folded(selector)?;
    let layers = redressed_arc(layers, |dress| dress.prepend_before(locations.clone()));
    Some(Pattern::of(Node::Sequence(layers)).with_steps_of(selector))
}

/// Whether `pattern` is a sequence whose haps each hold a value and nothing
/// more: no location and no key of the context.
pub(crate) fn is_bare(pattern: &Pattern) -> bool {
    folded(pattern).is_some_and(|layers| {
        layers.iter().all(|layer| {
            layer.dress().keys().is_empty()
                && layer.dress().locates_alone()
                && layer
                    .listing()
                    .with_grid_view(|grid| grid.steps.has_no_locations())
                    .unwrap_or_else(|| {
                        let listing = layer.listing();
                        (0..listing.len()).all(|index| listing.locations(index).is_empty())
                    })
        })
    })
}

/// Whether `pattern` is a sequence whose steps each lie in one cycle, or
/// silence: a constant, which is another hap in each cycle, then cuts no
/// hap of it.
pub(crate) fn lies_in_cycles(pattern: &Pattern) -> bool {
    match folded(pattern) {
        Some(layers) => layers.iter().all(Layer::in_cycles),
        None => pattern.is_rest() && folds(pattern),
    }
}

/// The value of `values` and its locations, when `values` is a constant
/// of a plain data value. See [`constant`].
pub(crate) fn constant_value_at(values: &Pattern) -> Option<(Value, Locations)> {
    // A handle of the value, for the caller to hold.
    let value = constant_value_of(values)?.into_owned();
    Some((value, constant_locations(values)))
}

/// Build a single-cycle named sequence directly from a plain constant value
/// and source locations, skipping an intermediate `Node::PureAt` allocation.
/// Whether `layers` includes any lazy [`Steps::Source`] layer.
pub(crate) fn has_source(layers: &[Layer]) -> bool {
    layers
        .iter()
        .any(|layer| matches!(layer.steps.inner(), Steps::Source(_)))
}

pub(crate) fn of_single_named(
    value: Value,
    locations: Locations,
    spec: &crate::controls::ControlSpec,
) -> Pattern {
    let raw = Raw {
        whole: TimeSpan::new(Fraction::ZERO, Fraction::ONE),
        part: None,
        value: 0,
        locations,
    };
    let mut layer = Layer::of_single(Fraction::ONE, raw, crate::value::arc_single_value(value));
    Layer::named_in_place(std::slice::from_mut(&mut layer), spec);
    Pattern::of(Node::Sequence(Arc::from([layer]))).with_steps(Some(Fraction::ONE))
}

/// Build a single-step named sequence for a 2-part compound word (`"bd:2"`)
/// directly as an `OrderedMap` without allocating a temporary `Arc<[Value; 2]>`.
pub(crate) fn of_single_named_pair(
    a: Value,
    b: Value,
    locations: Locations,
    spec: &crate::controls::ControlSpec,
) -> Pattern {
    if spec.names.len() >= 2 && a.is_data(0) && b.is_data(0) {
        let map = crate::OrderedMap::of_keys([
            (crate::Key::from(Arc::clone(&spec.names[0])), a),
            (crate::Key::from(Arc::clone(&spec.names[1])), b),
        ]);
        let raw = Raw {
            whole: TimeSpan::new(Fraction::ZERO, Fraction::ONE),
            part: None,
            value: 0,
            locations,
        };
        let layer = Layer::of_single(Fraction::ONE, raw, Arc::new([Value::Object(map)]));
        return Pattern::of(Node::Sequence(Arc::from([layer]))).with_steps(Some(Fraction::ONE));
    }
    of_single_named(Value::List(Arc::from([a, b])), locations, spec)
}

/// `pattern` with each value named by the control of `names`, as
/// [`Pattern::map_control`] names it, when `pattern` is a sequence or a
/// constant that holds no name and no control yet. See [`Layer::named`].
pub(crate) fn named(
    pattern: &Pattern,
    spec: &crate::controls::ControlSpec,
    unnamed_only: bool,
) -> Option<Pattern> {
    let layers = layers(pattern)?;
    if !layers.iter().all(|layer| layer.takes_name(unnamed_only)) {
        return None;
    }
    // Each layer of a node has steps, so each one stays. The layers are
    // made where the new node holds them.
    let named = match layers {
        Layers::Single([mut layer]) => {
            Layer::named_in_place(std::slice::from_mut(&mut layer), spec);
            Arc::from([layer])
        }
        Layers::Borrowed(slice) => Layer::named_arc(slice, spec),
    };
    Some(Pattern::of(Node::Sequence(named)))
}

/// Try to name `layers` of an owned `Node::Sequence` in place when unshared.
/// Returns `true` if `layers` were updated in place.
pub(crate) fn named_in_place(
    layers: &mut Arc<[Layer]>,
    spec: &crate::controls::ControlSpec,
    unnamed_only: bool,
) -> bool {
    if !layers.iter().all(|layer| layer.takes_name(unnamed_only)) {
        return false;
    }
    let Some(slice) = Arc::get_mut(layers) else {
        return false;
    };
    Layer::named_in_place(slice, spec);
    true
}

/// `pattern` with each hap changed by `map`, a map that reads no span and
/// no state, when `pattern` is a sequence or a constant of data values and
/// the map gives a plain value for each.
pub(crate) fn mapped(pattern: &Pattern, map: &OwnedHapMapFn) -> Option<Pattern> {
    let layers = layers(pattern)?.into_owned();
    revalued(layers, |value| {
        // A value of the script is read through its host, at each query.
        if !value.is_data(0) {
            return None;
        }
        let hap = map(cycle_hap(value.clone()))?;
        // A map that reads no span moves no hap.
        let cycle = TimeSpan::new(Fraction::ZERO, Fraction::ONE);
        ((hap.whole(), hap.part()) == (Some(cycle), cycle)
            && is_plain(&hap)
            && hap.context.is_empty())
        .then_some(hap.value)
    })
}

/// What a rewrite of pitch makes of a note or a step that a hap holds alone.
pub(crate) type NoteFn = Box<dyn Fn(&Value) -> Option<Value> + Send + Sync>;

/// `pattern` with each hap changed by `map`, a rewrite of pitch, as
/// [`mapped_haps`] gives it, when the control `note` or `n` names the steps
/// of `pattern`: `note` gives the new note of each step, and no map of
/// controls is made. See [`Layer::renoted`].
/// Small cache mapping source [`Dress`] pointers to renoted [`Dress`] arcs
/// across layers and picked branches of a single `.scale(...)` or pitch pass.
pub(super) type RenoteDressSlot = (*const Dress, Option<Arc<Dress>>);

pub(super) type RenoteValuesSlot = (*const Value, usize, Arc<[Value]>, bool);

#[derive(Default)]
pub(crate) struct RenoteDressCache {
    slots: [Option<RenoteDressSlot>; 4],
    next: u8,
    values: [Option<RenoteValuesSlot>; 4],
    next_values: u8,
}

impl RenoteDressCache {
    #[inline]
    pub(crate) fn with_bare(bare: Option<Arc<Dress>>) -> Self {
        let mut cache = Self::default();
        cache.put(std::ptr::null(), bare);
        cache
    }

    #[inline]
    pub(super) fn get(&self, from: *const Dress) -> Option<Option<Arc<Dress>>> {
        self.slots
            .iter()
            .flatten()
            .find_map(|(seen, dress)| (*seen == from).then(|| dress.clone()))
    }

    #[inline]
    pub(super) fn put(&mut self, from: *const Dress, dress: Option<Arc<Dress>>) {
        let idx = usize::from(self.next) & 3;
        self.slots[idx] = Some((from, dress));
        self.next = self.next.wrapping_add(1);
    }

    #[inline]
    pub(super) fn get_values(&self, src: &Arc<[Value]>) -> Option<(Arc<[Value]>, bool)> {
        let ptr = Arc::as_ptr(src) as *const Value;
        let len = src.len();
        self.values.iter().flatten().find_map(|(p, l, vals, data)| {
            (*p == ptr && *l == len).then(|| (Arc::clone(vals), *data))
        })
    }

    #[inline]
    pub(super) fn put_values(
        &mut self,
        ptr: *const Value,
        len: usize,
        vals: Arc<[Value]>,
        data: bool,
    ) {
        let idx = usize::from(self.next_values) & 3;
        self.values[idx] = Some((ptr, len, vals, data));
        self.next_values = self.next_values.wrapping_add(1);
    }
}

/// In-place variant of [`renoted_cached`] when `layers` is uniquely owned:
/// mutates both `Arc<[Layer]>` and uniquely-owned `Arc<[Value]>` slices in
/// place with zero heap allocations after validating all values on the stack.
pub(crate) fn renote_in_place(
    layers: &mut Arc<[Layer]>,
    map: &HapMapFn,
    note: &NoteFn,
    known_keys: Option<&ContextKeys>,
    dress_cache: &mut RenoteDressCache,
) -> bool {
    if layers.is_empty() {
        return false;
    }
    let Some(slice) = Arc::get_mut(layers) else {
        return false;
    };
    let all_same_values = slice.len() > 1
        && slice[1..]
            .iter()
            .all(|layer| Arc::ptr_eq(&layer.values, &slice[0].values));
    let mut total_values = 0usize;
    for (idx, layer) in slice.iter().enumerate() {
        let Some(names) = layer.named_names().filter(|_| !layer.dress().has_set()) else {
            return false;
        };
        let [name] = names else {
            return false;
        };
        if !matches!(&**name, "note" | "n") {
            return false;
        }
        if !all_same_values || idx == 0 {
            total_values += layer.values.len();
        }
    }
    without_query_signals(|| {
        const INLINE_STAGED: usize = 16;
        let mut stack_staged: [Value; INLINE_STAGED] = [const { Value::Undefined }; INLINE_STAGED];
        let mut vec_staged: Vec<Value> = if total_values > INLINE_STAGED {
            Vec::with_capacity(total_values)
        } else {
            Vec::new()
        };
        let mut cached_shared: Option<(Arc<[Value]>, bool)> = None;
        if all_same_values {
            if let Some(hit) = dress_cache.get_values(&slice[0].values) {
                cached_shared = Some(hit);
            } else if total_values <= INLINE_STAGED {
                let mut cursor = 0usize;
                for val in slice[0].values.iter() {
                    match val {
                        Value::F64(_) | Value::Str(_) => {
                            stack_staged[cursor] = note(val)?;
                            cursor += 1;
                        }
                        _ => return None,
                    }
                }
            } else {
                for val in slice[0].values.iter() {
                    match val {
                        Value::F64(_) | Value::Str(_) => vec_staged.push(note(val)?),
                        _ => return None,
                    }
                }
            }
        } else if total_values <= INLINE_STAGED {
            let mut cursor = 0usize;
            for layer in slice.iter() {
                for val in layer.values.iter() {
                    match val {
                        Value::F64(_) | Value::Str(_) => {
                            stack_staged[cursor] = note(val)?;
                            cursor += 1;
                        }
                        _ => return None,
                    }
                }
            }
        } else {
            for layer in slice.iter() {
                for val in layer.values.iter() {
                    match val {
                        Value::F64(_) | Value::Str(_) => vec_staged.push(note(val)?),
                        _ => return None,
                    }
                }
            }
        }
        let staged: &mut [Value] = if total_values <= INLINE_STAGED {
            &mut stack_staged[..total_values]
        } else {
            &mut vec_staged
        };
        // Resolve every layer's new dress before mutating any layer so a `None`
        // from `map(&read)` leaves `layers` untouched.
        let note_id = crate::controls::note_control_id();
        let mut new_dresses: [Option<Option<Arc<Dress>>>; 8] = Default::default();
        let mut extra_dresses: Vec<Option<Arc<Dress>>> = Vec::new();
        for (idx, layer) in slice.iter().enumerate() {
            let from = layer.dress.as_ref().map_or(std::ptr::null(), Arc::as_ptr);
            let dress = match dress_cache.get(from) {
                Some(cached) => cached,
                None => {
                    let keys = match known_keys {
                        Some(added) => {
                            if layer.dress().keys().is_empty() {
                                added.clone()
                            } else if added.is_empty() {
                                layer.dress().keys().clone()
                            } else {
                                ContextKeys::merge(added, layer.dress().keys())
                            }
                        }
                        None => {
                            let names = layer.named_names()?;
                            let first_val = layer.values.first()?;
                            let first = crate::controls::named_value(names, first_val.clone());
                            let mut read = cycle_hap(first);
                            read.keys = layer.dress().keys().clone();
                            map(&read)?.keys
                        }
                    };
                    let dress = if note_id != 0
                        && layer.dress().custom_named().is_none()
                        && !layer.dress().has_set()
                        && keys.arc_ptr() == layer.dress().keys().arc_ptr()
                    {
                        layer.dress.clone()
                    } else {
                        let mut next = layer.dress().clone();
                        if note_id != 0 {
                            next.clear_custom_named();
                        } else {
                            next.set_custom_named(&crate::controls::note_names());
                        }
                        next.clear_set();
                        next.set_keys(keys);
                        (!next.is_bare()).then(|| next.into_arc())
                    };
                    dress_cache.put(from, dress.clone());
                    dress
                }
            };
            if idx < new_dresses.len() {
                new_dresses[idx] = Some(dress);
            } else {
                extra_dresses.push(dress);
            }
        }
        let shared_out = if all_same_values {
            Some(match cached_shared {
                Some(hit) => hit,
                None => {
                    let len = slice[0].values.len();
                    let data = staged[..len].iter().all(|v| v.is_data(0));
                    let built: Arc<[Value]> = staged[..len]
                        .iter_mut()
                        .map(|src| std::mem::replace(src, Value::Undefined))
                        .collect();
                    dress_cache.put_values(
                        Arc::as_ptr(&slice[0].values) as *const Value,
                        len,
                        Arc::clone(&built),
                        data,
                    );
                    (built, data)
                }
            })
        } else {
            None
        };
        let mut read_at = 0usize;
        let mut extra_idx = 0usize;
        for (idx, layer) in slice.iter_mut().enumerate() {
            let (data, no_objects) = if let Some((shared, data)) = &shared_out {
                layer.values = Arc::clone(shared);
                (*data, false)
            } else {
                let len = layer.values.len();
                let mut data = true;
                let mut no_objects = true;
                for v in &staged[read_at..read_at + len] {
                    if data && !v.is_data(0) {
                        data = false;
                    }
                    if matches!(v, Value::Object(_)) {
                        no_objects = false;
                    }
                }
                if let Some(vals) = Arc::get_mut(&mut layer.values) {
                    for (dst, src) in vals.iter_mut().zip(&mut staged[read_at..read_at + len]) {
                        *dst = std::mem::replace(src, Value::Undefined);
                    }
                } else {
                    layer.values = staged[read_at..read_at + len]
                        .iter_mut()
                        .map(|src| std::mem::replace(src, Value::Undefined))
                        .collect();
                }
                read_at += len;
                (data, no_objects)
            };
            layer.set_data(data);
            layer.set_maps(true);
            layer.named_id = note_id;
            layer.dress = if idx < new_dresses.len() {
                new_dresses[idx].take().expect("dress computed")
            } else {
                let d = extra_dresses[extra_idx].take();
                extra_idx += 1;
                d
            };
            layer.refresh_direct_inner(no_objects);
        }
        Some(())
    })
    .is_some()
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn renoted_int_range_layer(
    start: i32,
    count: u32,
    loc0: u32,
    loc1: u32,
    named_id: u16,
    step_neg: bool,
    map: &HapMapFn,
    note: &NoteFn,
    degree_notes: Option<&[crate::value::SharedStr; 32]>,
    known_keys: Option<&ContextKeys>,
    dress_cache: &mut RenoteDressCache,
) -> Option<Layer> {
    if count == 0
        || count > 1024
        || !matches!(
            crate::controls::builtin_name_str_by_id(named_id),
            Some("note" | "n")
        )
    {
        return None;
    }
    let count_usize = count as usize;
    let ticks = NonZeroU32::new(count)?;
    let dir = if step_neg { -1i64 } else { 1i64 };
    let first_deg = i64::from(start);
    let last_deg = first_deg + ((count - 1) as i64) * dir;
    let min_deg = first_deg.min(last_deg);
    let max_deg = first_deg.max(last_deg);
    if let Some(notes) = degree_notes
        && let Some(keys) = known_keys
        && count_usize <= 16
        && min_deg >= -8
        && max_deg < 24
    {
        let values: Arc<[Value]> = (0..count_usize)
            .map(|i| {
                let deg = first_deg + (i as i64) * dir;
                Value::Str(notes[(deg + 8) as usize].clone())
            })
            .collect();
        let listing = Listing::CompactGrid(CompactGrid::Uniform(count as u8));
        let mut layer = Layer::holding_classified(
            Fraction::ONE,
            Steps::Listed(listing),
            true,
            values,
            true,
            true,
            true,
        );
        layer.named_id = crate::controls::note_control_id();
        if loc0 != 0 {
            layer.change_dress(|dress| {
                dress.set_before_packed(loc0, loc1);
                dress.set_keys(keys.clone());
            });
        } else if !keys.is_empty() {
            if let Some(bare) = dress_cache.get(std::ptr::null()).flatten() {
                layer.dress = Some(bare);
            } else {
                layer.change_dress(|dress| dress.set_keys(keys.clone()));
            }
        }
        layer.refresh_direct_inner(true);
        return Some(layer);
    }
    without_query_signals(|| {
        let mut data = true;
        let mut no_objects = true;
        let values: Arc<[Value]> = if count_usize <= 16 {
            let mut stack_vals = [const { Value::Undefined }; 16];
            for (i, slot) in stack_vals[..count_usize].iter_mut().enumerate() {
                let raw = Value::F64((i64::from(start) + (i as i64) * dir) as f64);
                let mapped = note(&raw)?;
                if data && !mapped.is_data(0) {
                    data = false;
                }
                if matches!(mapped, Value::Object(_)) {
                    no_objects = false;
                }
                *slot = mapped;
            }
            stack_vals[..count_usize]
                .iter_mut()
                .map(|v| std::mem::replace(v, Value::Undefined))
                .collect()
        } else {
            let mut vals = Vec::with_capacity(count_usize);
            for i in 0..count_usize {
                let raw = Value::F64((i64::from(start) + (i as i64) * dir) as f64);
                let mapped = note(&raw)?;
                if data && !mapped.is_data(0) {
                    data = false;
                }
                if matches!(mapped, Value::Object(_)) {
                    no_objects = false;
                }
                vals.push(mapped);
            }
            vals.into()
        };
        let listing = if count_usize <= MAX_UNIFORM_GRID_LEN {
            Listing::CompactGrid(CompactGrid::Uniform(count as u8))
        } else {
            let steps = GridSteps::build(count_usize, |index| {
                Some(GridStep {
                    part_begin: index as u32,
                    part_end: (index + 1) as u32,
                    value: index as u32,
                    loc: PackedLoc(0),
                })
            })?;
            let (steps, rare) = match steps {
                GridSteps::Wide(wide_steps) => (
                    GridSteps::Small(Box::from([])),
                    Some(Box::new(SpareRare::WideSteps(wide_steps))),
                ),
                other => (other, None),
            };
            Listing::from_parts(ticks, Grid::SMALL, steps, PackedRunsInput::None, rare)
        };
        let mut layer = Layer::holding_classified(
            Fraction::ONE,
            Steps::Listed(listing),
            true,
            values,
            data,
            true,
            no_objects,
        );
        layer.named_id = crate::controls::note_control_id();
        let dress_keys = if let Some(keys) = known_keys {
            keys.clone()
        } else {
            let mut read = cycle_hap(Value::Object(crate::OrderedMap::from_single_packed(
                crate::controls::builtin_spec_by_id(named_id)?.packed_key,
                Value::F64(f64::from(start)),
            )));
            let hap = map(&read)?;
            let cycle = TimeSpan::new(Fraction::ZERO, Fraction::ONE);
            if (hap.whole(), hap.part()) != (Some(cycle), cycle)
                || !holds_a_value_and_keys(&hap)
                || !hap.context.is_empty()
            {
                return None;
            }
            read.keys = hap.keys;
            read.keys
        };
        if loc0 != 0 {
            // Every step in `IntRange` has empty per-step locations (`loc = 0`),
            // so storing `[loc0, loc1]` in `dress.before_packed` yields identical
            // final location order without allocating `Box<DressTail>`.
            layer.change_dress(|dress| {
                dress.set_before_packed(loc0, loc1);
                dress.set_keys(dress_keys);
            });
        } else if !dress_keys.is_empty() {
            if let Some(bare) = dress_cache.get(std::ptr::null()).flatten() {
                layer.dress = Some(bare);
            } else {
                layer.change_dress(|dress| dress.set_keys(dress_keys));
            }
        }
        layer.refresh_direct_inner(no_objects);
        Some(layer)
    })
}

pub(crate) fn renoted_cached(
    pattern: &Pattern,
    map: &HapMapFn,
    note: &NoteFn,
    known_keys: Option<&ContextKeys>,
    dress_cache: &mut RenoteDressCache,
) -> Option<Pattern> {
    if folds(pattern)
        && let Node::IntRange {
            start,
            count,
            loc0,
            loc1,
            named_id,
            step_neg,
        } = *pattern.node_kind()
    {
        let layer = renoted_int_range_layer(
            start,
            count,
            loc0,
            loc1,
            named_id,
            step_neg,
            map,
            note,
            None,
            known_keys,
            dress_cache,
        )?;
        return node_of_one(layer);
    }
    let layers = folded(pattern)?;
    without_query_signals(|| {
        if let [layer] = layers {
            return node_of_one(layer.renoted_cached(map, note, known_keys, dress_cache)?);
        }
        if layers.len() <= 8 {
            let mut arr: [Option<Layer>; 8] = Default::default();
            for (slot, layer) in arr.iter_mut().zip(layers.iter()) {
                *slot = Some(layer.renoted_cached(map, note, known_keys, dress_cache)?);
            }
            let arc: Arc<[Layer]> = arr
                .into_iter()
                .take(layers.len())
                .map(|layer| layer.expect("layer built"))
                .collect();
            return Some(Pattern::of(Node::Sequence(arc)));
        }
        let mut out = Vec::with_capacity(layers.len());
        for layer in layers {
            out.push(layer.renoted_cached(map, note, known_keys, dress_cache)?);
        }
        Some(node_of_sound(out))
    })
}

/// `pattern` with each hap changed by `map`, a map that reads no span, when
/// `pattern` is a sequence or a constant of data values. The map can set
/// the keys of the context, as `scale` does: the haps of a layer then hold
/// the same keys.
///
/// `None` when the map drops a hap, moves it, gives it more than a value
/// and keys, or gives two haps of one layer other keys.
pub(crate) fn mapped_haps(pattern: &Pattern, map: &HapMapFn) -> Option<Pattern> {
    let layers = layers(pattern)?.into_owned();
    let cycle = TimeSpan::new(Fraction::ZERO, Fraction::ONE);
    without_query_signals(|| {
        let layers = layers
            .into_iter()
            .map(|layer| {
                let mut keys: Option<ContextKeys> = None;
                // One hap for each value in turn: the map reads it and
                // makes its own.
                let mut read = cycle_hap(Value::Undefined);
                read.keys = layer.dress().keys().clone();
                let values = layer
                    .each_value()
                    .map(|value| {
                        // A value of the script is read through its host,
                        // at each query.
                        if !value.is_data(0) {
                            return None;
                        }
                        // A handle of the value, which the layer holds.
                        read.value = value;
                        let hap = map(&read)?;
                        let keys = keys.get_or_insert_with(|| hap.keys.clone());
                        ((hap.whole(), hap.part()) == (Some(cycle), cycle)
                            && holds_a_value_and_keys(&hap)
                            && hap.context.is_empty()
                            && keys.same(&hap.keys))
                        .then(|| {
                            let mut value = hap.value;
                            if let Value::Object(map) = &mut value {
                                map.undress_patched();
                            }
                            value
                        })
                    })
                    .collect::<Option<Vec<Value>>>()?;
                let mut layer = layer.valued(values.into());
                if let Some(keys) = keys {
                    layer.change_dress(|dress| dress.set_keys(keys));
                }
                Some(layer)
            })
            .collect::<Option<Vec<_>>>()?;
        Some(node_of_sound(layers))
    })
}

/// Whether a fold may run `apply` when a pattern is built: it is a compose
/// op, a function of its two values and nothing more.
pub(super) fn runs_at_the_build(apply: &Combine) -> bool {
    match apply {
        Combine::Composed(_) | Combine::SetMerged => true,
        Combine::With(closure) => is_op_closure(closure),
    }
}

/// What `function` and `value` make by `apply`, as the hap of a step makes
/// it where the two meet, when they make a value and nothing more.
pub(super) fn made(
    function: &Value,
    value: &Value,
    apply: &Combine,
    flow: LookupFlow,
) -> Option<Value> {
    if let Some(op) = apply.in_place_op()
        && Meeting::values_meet_in_place(function, value, apply, flow)
    {
        let mut out = function.clone();
        crate::compose::compose_data_flat(op, &mut out, value);
        return Some(out);
    }
    let mut hap = cycle_hap(function.clone());
    let meets = cycle_hap(value.clone());
    let part = hap.part();
    // A meeting that is not made in place is made as its node makes it.
    if !Meeting::in_place(&mut hap, &meets, part, apply, flow) {
        hap = applied_hap(&hap, &meets, hap.whole(), part, apply, flow);
    }
    is_plain(&hap).then(|| {
        let mut value = hap.value;
        if let Value::Object(map) = &mut value {
            map.undress_patched();
        }
        value
    })
}

/// The `appLeft` of `functions` and `values` by `apply`, when `functions`
/// is a sequence or a constant and `values` is one layer of plain values,
/// or a constant of one: each step meets the values where the step is, and
/// is cut to each that it meets, as the meeting cuts its hap. A constant is
/// another hap in each cycle, so it cuts a step that crosses a bound of a
/// cycle there.
///
/// `None` for values of more layers than one, which answer each step layer
/// by layer, for a value that is not data, for a function that is no
/// compose op, for a meeting that makes more than a value, and for cut
/// steps that are not in the order of time.
/// Owned variant of [`applied`] that mutates a uniquely-owned single-layer
/// `Node::Sequence` in place, avoiding `Arc<[Layer]>` and `Arc<NodeCell>` allocs.
pub(crate) fn applied_owned(
    mut functions: Pattern,
    values: &Pattern,
    apply: &Combine,
    flow: LookupFlow,
) -> Result<Pattern, Pattern> {
    if !runs_at_the_build(apply) || builds_each_node() {
        return Err(functions);
    }
    let can_in_place = matches!(
        functions.node_kind(),
        Node::Sequence(layers) if layers.len() == 1 && Arc::strong_count(layers) == 1
    ) && Arc::strong_count(&functions.node) == 1;
    if can_in_place {
        if let Some(next_layer) = applied_single_layer(&functions, values, apply, flow)
            && let Some(cell) = functions.own_node()
            && let Node::Sequence(layers) = &mut cell.kind
            && let Some(slice) = Arc::get_mut(layers)
        {
            slice[0] = next_layer;
            return Ok(functions);
        }
        return Err(functions);
    }
    match applied(&functions, values, apply, flow) {
        Some(done) => Ok(done),
        None => Err(functions),
    }
}

pub(super) fn apply_constant_to_layer(
    layer: &Layer,
    val: &Value,
    op: ComposeOp,
    flow: LookupFlow,
) -> Option<Layer> {
    let apply = Combine::Composed(op);
    if let Some(set) = layer.set_constant(val, Locations::new, &apply, flow) {
        return Some(set);
    }
    // Composing a control-object layer with a non-object, non-undefined scalar
    // (`compose_op`: `(Value::Object(kept), value) => Value::Object(kept)`)
    // keeps every control map unchanged without materializing `BuiltinMap`s.
    if op != ComposeOp::KeepIf
        && (flow != LookupFlow::Set || (op == ComposeOp::Set && !matches!(val, Value::Str(_))))
        && layer.is_direct_keyed()
        && !matches!(val, Value::Object(_) | Value::Undefined)
    {
        if layer.in_cycles() {
            return Some(layer.clone());
        }
        static EMPTY_VALUES: std::sync::LazyLock<Arc<[Value]>> =
            std::sync::LazyLock::new(|| Arc::from([]));
        let raw = Raw {
            whole: TimeSpan::new(Fraction::ZERO, Fraction::ONE),
            part: None,
            value: 0,
            locations: Locations::new(),
        };
        let rhs = Layer::of_single(Fraction::ONE, raw, Arc::clone(&EMPTY_VALUES));
        let (mut met, pairs) = layer.met(&rhs, Side::Functions)?;
        let pairs_slice = pairs.as_slice();
        let identity = pairs_slice.len() == layer.values.len()
            && pairs_slice
                .iter()
                .enumerate()
                .all(|(i, &(f, _))| f as usize == i);
        if !identity {
            met.values = pairs_slice
                .iter()
                .map(|&(f, _)| layer.values[f as usize].clone())
                .collect();
            met.refresh_direct();
        }
        return Some(met);
    }
    static EMPTY_VALUES: std::sync::LazyLock<Arc<[Value]>> =
        std::sync::LazyLock::new(|| Arc::from([]));
    let raw = Raw {
        whole: TimeSpan::new(Fraction::ZERO, Fraction::ONE),
        part: None,
        value: 0,
        locations: Locations::new(),
    };
    let rhs = Layer::of_single(Fraction::ONE, raw, Arc::clone(&EMPTY_VALUES));
    let (met, pairs) = layer.met(&rhs, Side::Functions)?;
    let fn_reader = layer.reader(None);
    let pairs_slice = pairs.as_slice();
    if pairs_slice.len() == 1 {
        let (function, _) = pairs_slice[0];
        let v = made(&fn_reader.get(function as usize)?, val, &apply, flow)?;
        return Some(met.valued(crate::value::arc_single_value(v)));
    }
    if pairs_slice.len() <= 16 {
        let mut stack_vals: [Value; 16] = std::array::from_fn(|_| Value::Undefined);
        for (slot, &(function, _)) in stack_vals.iter_mut().zip(pairs_slice) {
            *slot = made(&fn_reader.get(function as usize)?, val, &apply, flow)?;
        }
        let met_values: Arc<[Value]> = stack_vals[..pairs_slice.len()]
            .iter_mut()
            .map(|v| std::mem::replace(v, Value::Undefined))
            .collect();
        return Some(met.valued(met_values));
    }
    let values = pairs_slice
        .iter()
        .map(|&(function, _)| made(&fn_reader.get(function as usize)?, val, &apply, flow))
        .collect::<Option<Vec<Value>>>()?;
    Some(met.valued(values.into()))
}

/// Owned constant-RHS compose onto a folded `Node::Sequence`, avoiding any
/// temporary `Node::Pure`, `Layer`, or `Arc<[Value]>` allocation for the RHS.
pub(crate) fn applied_constant_owned(
    mut functions: Pattern,
    val: Value,
    op: ComposeOp,
    flow: LookupFlow,
) -> Result<Pattern, Pattern> {
    let apply = Combine::Composed(op);
    if !runs_at_the_build(&apply) || builds_each_node() {
        return Err(functions);
    }
    if !val.is_data(0) {
        return Err(functions);
    }
    if let Node::Pure(v) = functions.node_kind()
        && v.is_data(0)
        && folds(&functions)
        && let Some(out) = without_query_signals(|| made(v, &val, &apply, flow))
    {
        let steps = functions.steps();
        if let Some(cell) = functions.own_node()
            && let Node::Pure(slot) = &mut cell.kind
        {
            *slot = out;
            return Ok(functions.with_steps(steps));
        }
        return Ok(crate::pure(out).with_steps(steps));
    }
    let Some(layers) = folded(&functions) else {
        return Err(functions);
    };
    if !holds_data(layers) {
        return Err(functions);
    }
    let steps = functions.steps();
    let result = without_query_signals(|| {
        if layers.len() <= 8 {
            let mut buf: [Option<Layer>; 8] = Default::default();
            for (slot, layer) in buf.iter_mut().zip(layers.iter()) {
                *slot = Some(apply_constant_to_layer(layer, &val, op, flow)?);
            }
            return Some((Some(buf), None, layers.len()));
        }
        let met = layers
            .iter()
            .map(|layer| apply_constant_to_layer(layer, &val, op, flow))
            .collect::<Option<Vec<_>>>()?;
        let len = met.len();
        Some((None, Some(met), len))
    });
    let Some((mut stack_buf, vec_buf, len)) = result else {
        return Err(functions);
    };
    if let Some(cell) = functions.own_node()
        && let Node::Sequence(target) = &mut cell.kind
        && let Some(dst) = Arc::get_mut(target)
        && dst.len() == len
    {
        if let Some(buf) = &mut stack_buf {
            for (d, s) in dst.iter_mut().zip(buf[..len].iter_mut()) {
                *d = s.take().expect("layer present");
            }
        } else if let Some(vec) = vec_buf {
            for (d, s) in dst.iter_mut().zip(vec) {
                *d = s;
            }
        }
        return Ok(functions.with_steps(steps));
    }
    let out = if let Some(mut buf) = stack_buf {
        node_of_sound_stack(&mut buf, len)
    } else {
        node_of_sound(vec_buf.expect("vec present"))
    };
    Ok(out.with_steps(steps))
}

pub(super) fn applied_single_layer(
    functions: &Pattern,
    values: &Pattern,
    apply: &Combine,
    flow: LookupFlow,
) -> Option<Layer> {
    let Node::Sequence(layers) = functions.node_kind() else {
        return None;
    };
    let [layer] = &**layers else {
        return None;
    };
    let values = self::layers(values)?;
    let [values] = &*values else {
        return None;
    };
    if !(values.is_data() && layer.is_data()) {
        return None;
    }
    without_query_signals(|| apply_layer_with(layer, values, apply, flow))
}

pub(crate) fn applied(
    functions: &Pattern,
    values: &Pattern,
    apply: &Combine,
    flow: LookupFlow,
) -> Option<Pattern> {
    if !runs_at_the_build(apply) {
        return None;
    }
    let layers = layers(functions)?;
    let values = self::layers(values)?;
    let [values] = &*values else {
        return None;
    };
    // A value of the script is read through its host, at each query.
    if !(values.is_data() && holds_data(&layers)) {
        return None;
    }
    without_query_signals(|| {
        if layers.len() <= 8 {
            let mut buf: [Option<Layer>; 8] = Default::default();
            for (slot, layer) in buf.iter_mut().zip(layers.iter()) {
                *slot = Some(apply_layer_with(layer, values, apply, flow)?);
            }
            return Some(node_of_sound_stack(&mut buf, layers.len()));
        }
        let met = layers
            .iter()
            .map(|layer| apply_layer_with(layer, values, apply, flow))
            .collect::<Option<Vec<_>>>()?;
        Some(node_of_sound(met))
    })
}

/// Compute the per-cycle reverse-emission mask of `met` produced by combining
/// `layer` (left) with `values` (right), or `None` when the combined emission
/// order cannot be represented in `Layer`'s 6-bit mask.
fn tile_rev_mask(layer: &Layer, values: &Layer, met: &Layer) -> Option<u8> {
    if values.has_rev_order() {
        return None;
    }
    let l_mask = layer.rev_mask();
    if l_mask == 0 {
        return Some(0);
    }
    if values.source().is_some() || !values.in_cycles() {
        return None;
    }
    let vp = values.period();
    if vp.denom() != 1 || vp.numer() <= 0 {
        return None;
    }
    let at_most_one_per_cycle = values
        .listing()
        .with_grid_view(|grid| {
            let ticks = grid.ticks.get();
            let GridStepsRef::Small(steps) = grid.steps else {
                return false;
            };
            let mut prev_cycle: Option<u32> = None;
            for &s in steps {
                if s.part_end <= s.part_begin {
                    return false;
                }
                let c_begin = u32::from(s.part_begin) / ticks;
                let c_end = u32::from(s.part_end - 1) / ticks;
                if c_begin != c_end || prev_cycle.is_some_and(|p| c_begin <= p) {
                    return false;
                }
                prev_cycle = Some(c_begin);
            }
            true
        })
        .unwrap_or(false);
    if !at_most_one_per_cycle {
        return None;
    }
    let lp = layer.period();
    let mp = met.period();
    if lp.numer() == 1 && lp.denom() > 1 {
        let all = (l_mask & 1) != 0;
        if mp.numer() == 1 && mp.denom() > 1 {
            return Some(u8::from(all));
        }
        if mp.denom() == 1 && (1..=6).contains(&mp.numer()) {
            return Some(if all { (1u8 << mp.numer()) - 1 } else { 0 });
        }
        return None;
    }
    if lp.denom() == 1
        && (1..=6).contains(&lp.numer())
        && mp.denom() == 1
        && (1..=6).contains(&mp.numer())
    {
        let l_len = lp.numer();
        let m_len = mp.numer();
        let mut out = 0u8;
        for c in 0..m_len {
            if (l_mask >> (c % l_len)) & 1 != 0 {
                out |= 1u8 << c;
            }
        }
        return Some(out);
    }
    None
}

pub(super) fn apply_layer_with(
    layer: &Layer,
    values: &Layer,
    apply: &Combine,
    flow: LookupFlow,
) -> Option<Layer> {
    if values.has_rev_order() {
        return None;
    }
    if let Some(set) = layer.set(values, apply, flow) {
        return Some(set);
    }
    let (mut met, pairs) = layer.met(values, Side::Functions)?;
    let rev_mask = tile_rev_mask(layer, values, &met)?;
    met.set_rev_mask(rev_mask);
    // Fast path: single-value constant built-in control applied by Set
    // folds into `met.dress` directly without materializing per-step
    // `BuiltinMap` objects or copying `layer.values` when 1-to-1.
    if apply.in_place_op() == Some(crate::compose::ComposeOp::Set)
        && flow == LookupFlow::Set
        && values.values.len() == 1
        && layer.is_direct_keyed()
        && layer.dress().tail.is_none()
    {
        let rhs_packed: Option<(u16, &Value)> = if values.is_direct_keyed()
            && values.dress.as_deref().is_none_or(|d| !d.has_set_map())
        {
            Some((values.seq_packed_key(), &values.values[0]))
        } else if values.named_id == 0
            && values
                .dress
                .as_deref()
                .is_none_or(|d| !d.has_set_map() && d.tail.is_none())
            && let Value::Object(map) = &values.values[0]
            && let Some(entries) = map.as_builtin()
            && let crate::value::Slice::Builtin(keys, vals) = entries.as_slice()
            && keys.len() == 1
        {
            Some((keys[0], &vals[0]))
        } else {
            None
        };
        if let Some((k1, v1)) = rhs_packed {
            let id1 = k1 & 0x03ff;
            if id1 != 0
                && id1 != layer.named_id
                && crate::controls::builtin_name_str_by_id(id1) != Some("value")
                && v1.is_data(0)
            {
                let pairs_slice = pairs.as_slice();
                let identity = pairs_slice.len() == layer.values.len()
                    && pairs_slice
                        .iter()
                        .enumerate()
                        .all(|(i, &(f, _))| f as usize == i);
                if !identity {
                    met.values = pairs_slice
                        .iter()
                        .map(|&(f, _)| layer.values[f as usize].clone())
                        .collect();
                }
                met.change_dress(|dress| {
                    let mut shared = None;
                    dress.set_packed_here(
                        &[k1],
                        std::slice::from_ref(v1),
                        Locations::new(),
                        &mut shared,
                    );
                });
                met.refresh_direct();
                return Some(met);
            }
        }
    }
    // Fast path: two direct-keyed built-in layers combined by Set
    // build each exact-sized BuiltinMap directly without temporary
    // 1-key maps or intermediate Vec<Value>.
    if apply.in_place_op() == Some(crate::compose::ComposeOp::Set)
        && flow == LookupFlow::Set
        && layer.is_direct_keyed()
        && values.is_direct_keyed()
        && values.dress.as_deref().is_none_or(|d| !d.has_set_map())
        && layer.named_id != values.named_id
        && crate::controls::builtin_name_str_by_id(values.named_id) != Some("value")
    {
        let k0 = layer.seq_packed_key();
        let k1 = values.seq_packed_key();
        let tail_dress = layer.dress.as_deref().filter(|d| d.has_set_map());
        match tail_dress {
            None => {
                let met_values: Arc<[Value]> = pairs
                    .as_slice()
                    .iter()
                    .map(|&(f, v)| {
                        Value::Object(crate::OrderedMap::from_two_packed(
                            k0,
                            layer.values[f as usize].clone(),
                            k1,
                            values.values[v as usize].clone(),
                        ))
                    })
                    .collect();
                return Some(met.valued_data_maps(met_values));
            }
            Some(dress)
                if !dress.set_packed_keys().contains(&k1)
                    && dress.set_len() + 2 <= crate::value::MAX_BUILTIN_MAP_LEN =>
            {
                let tail_slice = dress.set_slice();
                let pairs_slice = pairs.as_slice();
                let tail_len = tail_slice.len();
                let total_keys = tail_len + 2;
                let pool_len = layer.values.len() + tail_len + values.values.len();
                // For 3+ key maps repeated across 4+ pairs (e.g. synth chain +
                // patterned control), pool the unique values once into a single
                // `Arc<[Value]>` and emit 48 B `PooledBuiltinMap`s instead of
                // 96+ B `BuiltinMap`s.
                if pairs_slice.len() >= 4 && tail_len >= 1 && pool_len <= usize::from(u16::MAX) {
                    let mut pool_vec = Vec::with_capacity(pool_len);
                    // Clone each unique source/tail/rhs value once into the shared pool.
                    pool_vec.extend(layer.values.iter().cloned());
                    let tail_base = pool_vec.len() as u16;
                    for idx in 0..tail_len {
                        pool_vec.push(tail_slice.value_at(idx).clone());
                    }
                    let rhs_base = pool_vec.len() as u16;
                    pool_vec.extend(values.values.iter().cloned());
                    let pool: Arc<[Value]> = Arc::from(pool_vec);
                    let mut out_keys = [0u16; crate::value::MAX_BUILTIN_MAP_LEN];
                    out_keys[0] = k0;
                    for (slot, &tk) in out_keys[1..1 + tail_len]
                        .iter_mut()
                        .zip(dress.set_packed_keys().iter())
                    {
                        *slot = tk;
                    }
                    out_keys[1 + tail_len] = k1;
                    let mut out_vals = [0u16; crate::value::MAX_BUILTIN_MAP_LEN];
                    for idx in 0..tail_len {
                        out_vals[1 + idx] = tail_base + idx as u16;
                    }
                    let met_values: Arc<[Value]> = pairs_slice
                        .iter()
                        .map(|&(f, v)| {
                            out_vals[0] = f as u16;
                            out_vals[1 + tail_len] = rhs_base + v as u16;
                            Value::Object(crate::OrderedMap::from_made_builtin(
                                crate::value::make_pooled_builtin_arc(
                                    // Share one value pool handle across each pooled pair map.
                                    Arc::clone(&pool),
                                    &out_keys[..total_keys],
                                    &out_vals[..total_keys],
                                ),
                            ))
                        })
                        .collect();
                    return Some(met.valued_data_maps(met_values));
                }
                let met_values: Arc<[Value]> = pairs_slice
                    .iter()
                    .map(|&(f, v)| {
                        Value::Object(crate::OrderedMap::from_keyed_tail_and_packed(
                            k0,
                            layer.values[f as usize].clone(),
                            tail_slice,
                            k1,
                            values.values[v as usize].clone(),
                        ))
                    })
                    .collect();
                return Some(met.valued_data_maps(met_values));
            }
            Some(_) => {}
        }
    }
    // Fast path: two direct-keyed built-in layers with the SAME key
    // combined by any scalar compose op (e.g. `note("c3 e3").add(note(7))`,
    // `n("0 2").add(n(7))`): apply `op.apply_scalar` directly on the
    // raw values and keep `met` direct-keyed without materializing any
    // `BuiltinMap<1>` objects on inputs or outputs.
    if let Some(op) = apply.in_place_op()
        && op != crate::compose::ComposeOp::KeepIf
        && (flow != LookupFlow::Set || op == crate::compose::ComposeOp::Set)
        && layer.is_direct_keyed()
        && layer.dress().tail.is_none()
        && values.is_direct_keyed()
        && layer.named_id == values.named_id
        && values
            .dress
            .as_deref()
            .is_none_or(|d| !d.has_set_map() && d.tail.is_none())
        && crate::controls::builtin_name_str_by_id(layer.named_id) != Some("value")
    {
        let pairs_slice = pairs.as_slice();
        let mut ok = true;
        let map_pair = |f: u32, v: u32| -> Option<Value> {
            let a = &layer.values[f as usize];
            let b = &values.values[v as usize];
            if matches!(a, Value::Object(_) | Value::Undefined)
                || matches!(b, Value::Object(_) | Value::Undefined)
            {
                return None;
            }
            let res = op.apply_scalar(a, b);
            if matches!(res, Value::Object(_) | Value::Undefined) || !res.is_data(0) {
                return None;
            }
            Some(res)
        };
        if pairs_slice.len() == 1 {
            let (f, v) = pairs_slice[0];
            if let Some(res) = map_pair(f, v) {
                met.values = crate::value::arc_single_value(res);
                met.refresh_direct_inner(true);
                return Some(met);
            }
        } else if pairs_slice.len() <= 16 {
            let mut stack_vals: [Value; 16] = std::array::from_fn(|_| Value::Undefined);
            for (slot, &(f, v)) in stack_vals.iter_mut().zip(pairs_slice) {
                if let Some(res) = map_pair(f, v) {
                    *slot = res;
                } else {
                    ok = false;
                    break;
                }
            }
            if ok {
                met.values = stack_vals[..pairs_slice.len()]
                    .iter_mut()
                    .map(|v| std::mem::replace(v, Value::Undefined))
                    .collect();
                met.refresh_direct_inner(true);
                return Some(met);
            }
        } else {
            let mut vec_vals = Vec::with_capacity(pairs_slice.len());
            for &(f, v) in pairs_slice {
                if let Some(res) = map_pair(f, v) {
                    vec_vals.push(res);
                } else {
                    ok = false;
                    break;
                }
            }
            if ok {
                met.values = vec_vals.into();
                met.refresh_direct_inner(true);
                return Some(met);
            }
        }
    }
    // Fast path: composing a direct-keyed control-object layer with a
    // non-object, non-undefined scalar layer keeps every control map unchanged
    // (`compose_op`: `(Value::Object(kept), value) => Value::Object(kept)`)
    // without materializing `BuiltinMap`s (matching `apply_constant_to_layer`).
    if let Some(op) = apply.in_place_op()
        && op != crate::compose::ComposeOp::KeepIf
        && layer.is_direct_keyed()
        && values.named_id == 0
        && values
            .dress
            .as_deref()
            .is_none_or(|d| !d.has_set_map() && d.tail.is_none())
        && values.values.iter().all(|v| {
            !matches!(v, Value::Object(_) | Value::Undefined)
                && (flow != LookupFlow::Set
                    || (op == crate::compose::ComposeOp::Set && !matches!(v, Value::Str(_))))
        })
    {
        let pairs_slice = pairs.as_slice();
        let identity = pairs_slice.len() == layer.values.len()
            && pairs_slice
                .iter()
                .enumerate()
                .all(|(i, &(f, _))| f as usize == i);
        if !identity {
            met.values = pairs_slice
                .iter()
                .map(|&(f, _)| layer.values[f as usize].clone())
                .collect();
            met.refresh_direct();
        }
        return Some(met);
    }
    // What each pair of values makes, with no location: the
    // locations of both sides are those of the steps.
    let fn_reader = layer.reader(None);
    let val_reader = values.reader(None);
    let pairs_slice = pairs.as_slice();
    if pairs_slice.len() == 1 {
        let (function, value) = pairs_slice[0];
        let v = made(
            &fn_reader.get(function as usize)?,
            &val_reader.get(value as usize)?,
            apply,
            flow,
        )?;
        return Some(met.valued(crate::value::arc_single_value(v)));
    }
    if pairs_slice.len() <= 16 {
        let mut stack_vals: [Value; 16] = std::array::from_fn(|_| Value::Undefined);
        for (slot, &(function, value)) in stack_vals.iter_mut().zip(pairs_slice) {
            *slot = made(
                &fn_reader.get(function as usize)?,
                &val_reader.get(value as usize)?,
                apply,
                flow,
            )?;
        }
        let met_values: Arc<[Value]> = stack_vals[..pairs_slice.len()]
            .iter_mut()
            .map(|v| std::mem::replace(v, Value::Undefined))
            .collect();
        return Some(met.valued(met_values));
    }
    let values = pairs_slice
        .iter()
        .map(|&(function, value)| {
            made(
                &fn_reader.get(function as usize)?,
                &val_reader.get(value as usize)?,
                apply,
                flow,
            )
        })
        .collect::<Option<Vec<Value>>>()?;
    Some(met.valued(values.into()))
}

/// Direct `Set` of a packed row of built-in constant controls onto a
/// cross-cycle folded sequence: cuts each layer against cycle boundaries with
/// `first_locs`, prepends `rest_locs` onto `dress.before`, and writes all
/// `(keys, vals)` in a single `BuiltinMap<N>` allocation.
pub(crate) fn applied_packed_row(
    mut functions: Pattern,
    keys: &[u16],
    vals: &[Value],
    first_locs: Locations,
    rest_locs: Locations,
) -> Result<Pattern, Pattern> {
    if keys.is_empty() {
        return Err(functions);
    }
    let Some(layers) = folded(&functions) else {
        return Err(functions);
    };
    if !holds_data(layers)
        || !layers.iter().all(|layer| {
            layer.is_direct_keyed()
                && layer.dress().tail.is_none()
                && !keys.iter().any(|&k| (k & 0x03ff) == layer.named_id)
        })
    {
        return Err(functions);
    }
    static EMPTY_VALUES: std::sync::LazyLock<Arc<[Value]>> =
        std::sync::LazyLock::new(|| Arc::from([]));
    let raw = Raw {
        whole: TimeSpan::new(Fraction::ZERO, Fraction::ONE),
        part: None,
        value: 0,
        locations: first_locs,
    };
    let mut rhs = Layer::of_single(Fraction::ONE, raw, Arc::clone(&EMPTY_VALUES));
    rhs.named_id = keys[0] & 0x03ff;
    let steps = functions.steps();
    let result = without_query_signals(|| {
        let mut shared = None;
        let mut apply_layer = |layer: &Layer| -> Option<Layer> {
            let (mut met, pairs) = layer.met(&rhs, Side::Functions)?;
            let pairs_slice = pairs.as_slice();
            let identity = pairs_slice.len() == layer.values.len()
                && pairs_slice
                    .iter()
                    .enumerate()
                    .all(|(i, &(f, _))| f as usize == i);
            if !identity {
                met.values = pairs_slice
                    .iter()
                    .map(|&(f, _)| layer.values[f as usize].clone())
                    .collect();
            }
            met.change_dress(|dress| {
                dress.set_packed_here(keys, vals, rest_locs.clone(), &mut shared);
            });
            met.refresh_direct();
            Some(met)
        };
        if layers.len() <= 8 {
            let mut buf: [Option<Layer>; 8] = Default::default();
            for (slot, layer) in buf.iter_mut().zip(layers.iter()) {
                *slot = Some(apply_layer(layer)?);
            }
            return Some((Some(buf), None, layers.len()));
        }
        let met = layers.iter().map(apply_layer).collect::<Option<Vec<_>>>()?;
        let len = met.len();
        Some((None, Some(met), len))
    });
    let Some((mut stack_buf, vec_buf, len)) = result else {
        return Err(functions);
    };
    if let Some(cell) = functions.own_node()
        && let Node::Sequence(target) = &mut cell.kind
        && let Some(dst) = Arc::get_mut(target)
        && dst.len() == len
    {
        if let Some(buf) = &mut stack_buf {
            for (d, s) in dst.iter_mut().zip(buf[..len].iter_mut()) {
                *d = s.take().expect("met layer present");
            }
        } else if let Some(met) = vec_buf {
            for (d, s) in dst.iter_mut().zip(met) {
                *d = s;
            }
        }
        return Ok(functions.with_steps(steps));
    }
    let out = match (stack_buf.as_mut(), vec_buf) {
        (Some(buf), _) => node_of_sound_stack(buf, len),
        (None, Some(met)) => node_of_sound(met),
        (None, None) => unreachable!("either stack or vec buffer present"),
    };
    Ok(out.with_steps(steps))
}

/// Direct `Set` of a single-value built-in constant with source `locations`
/// onto a folded sequence, avoiding temporary `PureAt`, `NodeCell`,
/// `Arc<[Layer; 1]>`, and `Arc<[Value; 1]>` allocations and updating uniquely
/// owned layer slices in place.
pub(crate) fn applied_constant(
    mut functions: Pattern,
    spec: &crate::controls::ControlSpec,
    value: Value,
    locations: Locations,
) -> Result<Pattern, (Pattern, Value, Locations)> {
    if spec.id == 0
        || spec.name() == "value"
        || !matches!(value, Value::F64(_) | Value::Str(_) | Value::Bool(_))
        || !value.is_data(0)
    {
        return Err((functions, value, locations));
    }
    let Some(layers) = folded(&functions) else {
        return Err((functions, value, locations));
    };
    if !holds_data(layers)
        || !layers.iter().all(|layer| {
            layer.is_direct_keyed() && layer.dress().tail.is_none() && layer.named_id != spec.id
        })
    {
        return Err((functions, value, locations));
    }
    static EMPTY_VALUES: std::sync::LazyLock<Arc<[Value]>> =
        std::sync::LazyLock::new(|| Arc::from([]));
    let k1 = crate::controls::builtin_packed_key(spec.id);
    let raw = Raw {
        whole: TimeSpan::new(Fraction::ZERO, Fraction::ONE),
        part: None,
        value: 0,
        locations: locations.clone(),
    };
    let mut rhs = Layer::of_single(Fraction::ONE, raw, Arc::clone(&EMPTY_VALUES));
    rhs.named_id = spec.id;
    let steps = functions.steps();
    let result = without_query_signals(|| {
        let mut shared = None;
        let mut apply_layer = |layer: &Layer| -> Option<Layer> {
            let (mut met, pairs) = layer.met(&rhs, Side::Functions)?;
            let pairs_slice = pairs.as_slice();
            let identity = pairs_slice.len() == layer.values.len()
                && pairs_slice
                    .iter()
                    .enumerate()
                    .all(|(i, &(f, _))| f as usize == i);
            if !identity {
                met.values = pairs_slice
                    .iter()
                    .map(|&(f, _)| layer.values[f as usize].clone())
                    .collect();
            }
            met.change_dress(|dress| {
                dress.set_packed_here(
                    &[k1],
                    std::slice::from_ref(&value),
                    Locations::new(),
                    &mut shared,
                );
            });
            met.refresh_direct();
            Some(met)
        };
        if layers.len() <= 8 {
            let mut buf: [Option<Layer>; 8] = Default::default();
            for (slot, layer) in buf.iter_mut().zip(layers.iter()) {
                *slot = Some(apply_layer(layer)?);
            }
            return Some((Some(buf), None, layers.len()));
        }
        let met = layers.iter().map(apply_layer).collect::<Option<Vec<_>>>()?;
        let len = met.len();
        Some((None, Some(met), len))
    });
    let Some((mut stack_buf, vec_buf, len)) = result else {
        return Err((functions, value, locations));
    };
    if let Some(cell) = functions.own_node()
        && let Node::Sequence(target) = &mut cell.kind
        && let Some(dst) = Arc::get_mut(target)
        && dst.len() == len
    {
        if let Some(buf) = &mut stack_buf {
            for (d, s) in dst.iter_mut().zip(buf[..len].iter_mut()) {
                *d = s.take().expect("met layer present");
            }
        } else if let Some(met) = vec_buf {
            for (d, s) in dst.iter_mut().zip(met) {
                *d = s;
            }
        }
        return Ok(functions.with_steps(steps));
    }
    let out = match (stack_buf.as_mut(), vec_buf) {
        (Some(buf), _) => node_of_sound_stack(buf, len),
        (None, Some(met)) => node_of_sound(met),
        (None, None) => unreachable!("either stack or vec buffer present"),
    };
    Ok(out.with_steps(steps))
}

/// Build a plain numeric sequence pattern from 2..=8 flat numeric steps when
/// [`applied_flat_numbers`] falls back to general composition.
pub(crate) fn pattern_of_flat_numbers(
    values: &[f64],
    locs: &[(u32, u16)],
    slowcat: bool,
) -> Option<Pattern> {
    let k = values.len();
    if !(2..=8).contains(&k) || locs.len() != k {
        return None;
    }
    let layer_values: Arc<[Value]> = values.iter().map(|&v| Value::F64(v)).collect();
    let mut raw_steps = [(0u16, 0u16, 0u16, 0u32, 0u16); 8];
    for (idx, &(b, l)) in locs.iter().enumerate() {
        raw_steps[idx] = (idx as u16, (idx + 1) as u16, idx as u16, b, l);
    }
    let (period, ticks, steps_meta) = if slowcat {
        (
            Fraction::int(i128::from(k as u32)),
            NonZeroU32::new(1)?,
            Some(Fraction::ONE),
        )
    } else {
        (
            Fraction::ONE,
            NonZeroU32::new(k as u32)?,
            Some(Fraction::int(i128::from(k as u32))),
        )
    };
    let pat = super::sequence_of_ticks_plain(period, ticks, layer_values, true, &raw_steps[..k])?;
    Some(pat.with_steps(steps_meta))
}

/// Apply a flat numeric control sequence (`"0.8 0.6"`, `"<0.15 0.3 0.2 0.25>"`)
/// directly onto a folded direct-keyed `Node::Sequence` in a single pass with
/// zero temporary RHS `Pattern`, `Layer`, or `Grid` allocations.
pub(crate) fn applied_flat_numbers(
    mut functions: Pattern,
    spec: &crate::controls::ControlSpec,
    values: &[f64],
    locs: &[(u32, u16)],
    slowcat: bool,
) -> Result<Pattern, Pattern> {
    let k = values.len();
    if !(2..=8).contains(&k)
        || locs.len() != k
        || crate::compose::default_alignment() != crate::compose::Alignment::In
        || builds_each_node()
        || spec.id == 0
        || spec.packed_key == 0
        || spec.name() == "value"
    {
        return Err(functions);
    }
    let Some(layers) = folded(&functions) else {
        return Err(functions);
    };
    if layers.is_empty() || !holds_data(layers) {
        return Err(functions);
    }
    let k1 = spec.packed_key;
    if !layers.iter().all(|layer| {
        layer.is_direct_keyed()
            && layer.named_id != spec.id
            && layer.dress.as_deref().is_none_or(|d| {
                !d.has_set_map()
                    || (!d.set_packed_keys().contains(&k1)
                        && d.set_len() + 2 <= crate::value::MAX_BUILTIN_MAP_LEN)
            })
    }) {
        return Err(functions);
    }
    let steps = functions.steps();
    let apply_one = |layer: &Layer| -> Option<Layer> {
        let (met, pairs) = layer.met_flat_numbers(locs, slowcat)?;
        let pairs_slice = pairs.as_slice();
        let k0 = layer.seq_packed_key();
        let tail_dress = layer.dress.as_deref().filter(|d| d.has_set_map());
        let met_values: Arc<[Value]> = match tail_dress {
            None => pairs_slice
                .iter()
                .map(|&(f, v)| {
                    Value::Object(crate::OrderedMap::from_two_packed(
                        k0,
                        layer.values[f as usize].clone(),
                        k1,
                        Value::F64(values[v as usize]),
                    ))
                })
                .collect(),
            Some(dress) => {
                let tail_slice = dress.set_slice();
                let tail_len = tail_slice.len();
                let total_keys = tail_len + 2;
                let pool_len = layer.values.len() + tail_len + k;
                if pairs_slice.len() >= 4 && tail_len >= 1 && pool_len <= usize::from(u16::MAX) {
                    let mut pool_vec = Vec::with_capacity(pool_len);
                    // Clone each unique source/tail value once into the shared pool.
                    pool_vec.extend(layer.values.iter().cloned());
                    let tail_base = pool_vec.len() as u16;
                    for idx in 0..tail_len {
                        pool_vec.push(tail_slice.value_at(idx).clone());
                    }
                    let rhs_base = pool_vec.len() as u16;
                    for &num in values {
                        pool_vec.push(Value::F64(num));
                    }
                    let pool: Arc<[Value]> = Arc::from(pool_vec);
                    let mut out_keys = [0u16; crate::value::MAX_BUILTIN_MAP_LEN];
                    out_keys[0] = k0;
                    for (slot, &tk) in out_keys[1..1 + tail_len]
                        .iter_mut()
                        .zip(dress.set_packed_keys().iter())
                    {
                        *slot = tk;
                    }
                    out_keys[1 + tail_len] = k1;
                    let mut out_vals = [0u16; crate::value::MAX_BUILTIN_MAP_LEN];
                    for idx in 0..tail_len {
                        out_vals[1 + idx] = tail_base + idx as u16;
                    }
                    pairs_slice
                        .iter()
                        .map(|&(f, v)| {
                            out_vals[0] = f as u16;
                            out_vals[1 + tail_len] = rhs_base + v as u16;
                            Value::Object(crate::OrderedMap::from_made_builtin(
                                crate::value::make_pooled_builtin_arc(
                                    // Share one value pool handle across each pooled pair map.
                                    Arc::clone(&pool),
                                    &out_keys[..total_keys],
                                    &out_vals[..total_keys],
                                ),
                            ))
                        })
                        .collect()
                } else {
                    pairs_slice
                        .iter()
                        .map(|&(f, v)| {
                            Value::Object(crate::OrderedMap::from_keyed_tail_and_packed(
                                k0,
                                layer.values[f as usize].clone(),
                                tail_slice,
                                k1,
                                Value::F64(values[v as usize]),
                            ))
                        })
                        .collect()
                }
            }
        };
        Some(met.valued_data_maps(met_values))
    };
    if layers.len() == 1 {
        let Some(one) = without_query_signals(|| apply_one(&layers[0])) else {
            return Err(functions);
        };
        if let Some(cell) = functions.own_node()
            && let Node::Sequence(target) = &mut cell.kind
            && let Some(dst) = Arc::get_mut(target)
            && dst.len() == 1
        {
            dst[0] = one;
            return Ok(functions.with_steps(steps));
        }
        let Some(out) = node_of_one(one) else {
            return Err(functions);
        };
        return Ok(out.with_steps(steps));
    }
    let built = without_query_signals(|| {
        if layers.len() <= 8 {
            let mut buf: [Option<Layer>; 8] = Default::default();
            for (slot, layer) in buf.iter_mut().zip(layers.iter()) {
                *slot = Some(apply_one(layer)?);
            }
            return Some((Some(buf), None, layers.len()));
        }
        let met = layers.iter().map(apply_one).collect::<Option<Vec<_>>>()?;
        let len = met.len();
        Some((None, Some(met), len))
    });
    let Some((mut stack_buf, vec_buf, len)) = built else {
        return Err(functions);
    };
    if let Some(cell) = functions.own_node()
        && let Node::Sequence(target) = &mut cell.kind
        && let Some(dst) = Arc::get_mut(target)
        && dst.len() == len
    {
        if let Some(buf) = &mut stack_buf {
            for (d, s) in dst.iter_mut().zip(buf[..len].iter_mut()) {
                *d = s.take().expect("met layer present");
            }
        } else if let Some(met) = vec_buf {
            for (d, s) in dst.iter_mut().zip(met) {
                *d = s;
            }
        }
        return Ok(functions.with_steps(steps));
    }
    let out = match (stack_buf.as_mut(), vec_buf) {
        (Some(buf), _) => node_of_sound_stack(buf, len),
        (None, Some(met)) => node_of_sound(met),
        (None, None) => unreachable!("either stack or vec buffer present"),
    };
    Ok(out.with_steps(steps))
}

/// Compose a direct-keyed `Node::Sequence` with a same-key flat numeric control
/// sequence (e.g. `.add(note("<0 12>"))`, `.sub(n("0 7"))`) in one pass without
/// allocating an intermediate RHS `Pattern`, `Layer`, or `Grid`.
pub(crate) fn composed_control_flat_numbers(
    mut functions: Pattern,
    spec_id: u16,
    values: &[f64],
    locs: &[(u32, u16)],
    slowcat: bool,
    op: crate::compose::ComposeOp,
    flow: LookupFlow,
) -> Result<Pattern, Pattern> {
    let k = values.len();
    if !(2..=8).contains(&k)
        || locs.len() != k
        || op == crate::compose::ComposeOp::KeepIf
        || (flow == LookupFlow::Set && op != crate::compose::ComposeOp::Set)
        || crate::compose::default_alignment() != crate::compose::Alignment::In
        || builds_each_node()
        || spec_id == 0
        || crate::controls::builtin_name_str_by_id(spec_id) == Some("value")
    {
        return Err(functions);
    }
    let Some(layers) = folded(&functions) else {
        return Err(functions);
    };
    if layers.is_empty() || !holds_data(layers) {
        return Err(functions);
    }
    if !layers.iter().all(|layer| {
        layer.is_direct_keyed()
            && layer.dress().tail.is_none()
            && layer.named_id == spec_id
            && (!layer.has_rev_order() || slowcat)
    }) {
        return Err(functions);
    }
    let steps = functions.steps();
    let apply_one = |layer: &Layer| -> Option<Layer> {
        let (mut met, pairs) = layer.met_flat_numbers(locs, slowcat)?;
        let l_mask = layer.rev_mask();
        let rev_mask = if l_mask == 0 {
            0u8
        } else {
            let lp = layer.period();
            let mp = met.period();
            if lp.numer() == 1 && lp.denom() > 1 {
                let all = (l_mask & 1) != 0;
                if mp.numer() == 1 && mp.denom() > 1 {
                    u8::from(all)
                } else if mp.denom() == 1 && (1..=6).contains(&mp.numer()) {
                    if all { (1u8 << mp.numer()) - 1 } else { 0 }
                } else {
                    return None;
                }
            } else if lp.denom() == 1
                && (1..=6).contains(&lp.numer())
                && mp.denom() == 1
                && (1..=6).contains(&mp.numer())
            {
                let l_len = lp.numer();
                let m_len = mp.numer();
                let mut out = 0u8;
                for c in 0..m_len {
                    if (l_mask >> (c % l_len)) & 1 != 0 {
                        out |= 1u8 << c;
                    }
                }
                out
            } else {
                return None;
            }
        };
        met.set_rev_mask(rev_mask);
        let pairs_slice = pairs.as_slice();
        let is_arith = matches!(
            op,
            crate::compose::ComposeOp::Add
                | crate::compose::ComposeOp::Sub
                | crate::compose::ComposeOp::Mul
                | crate::compose::ComposeOp::Div
                | crate::compose::ComposeOp::Mod
                | crate::compose::ComposeOp::Pow
                | crate::compose::ComposeOp::Band
                | crate::compose::ComposeOp::Bor
                | crate::compose::ComposeOp::Bxor
                | crate::compose::ComposeOp::Blshift
                | crate::compose::ComposeOp::Brshift
        );
        if is_arith && layer.values.len() <= 16 && pairs_slice.len() <= 16 {
            let mut lhs_nums = [0.0f64; 16];
            let mut parsed_all = true;
            for (dst, a) in lhs_nums[..layer.values.len()]
                .iter_mut()
                .zip(layer.values.iter())
            {
                if matches!(a, Value::Object(_) | Value::Undefined) {
                    parsed_all = false;
                    break;
                }
                match crate::util::parse_numeral(a) {
                    Ok(x) => *dst = x,
                    Err(_) => {
                        parsed_all = false;
                        break;
                    }
                }
            }
            if parsed_all {
                met.values = if pairs_slice.len() == 1 {
                    let (f, v) = pairs_slice[0];
                    crate::value::arc_single_value(Value::F64(
                        op.of_numbers(lhs_nums[f as usize], values[v as usize])?,
                    ))
                } else {
                    let mut ok = true;
                    let built: Arc<[Value]> = pairs_slice
                        .iter()
                        .map(|&(f, v)| {
                            match op.of_numbers(lhs_nums[f as usize], values[v as usize]) {
                                Some(n) => Value::F64(n),
                                None => {
                                    ok = false;
                                    Value::Undefined
                                }
                            }
                        })
                        .collect();
                    if !ok {
                        return None;
                    }
                    built
                };
                met.refresh_direct_inner(true);
                return Some(met);
            }
        }
        let map_pair = |f: u32, v: u32| -> Option<Value> {
            let a = &layer.values[f as usize];
            if matches!(a, Value::Object(_) | Value::Undefined) {
                return None;
            }
            let b = Value::F64(values[v as usize]);
            let res = op.apply_scalar(a, &b);
            if matches!(res, Value::Object(_) | Value::Undefined) || !res.is_data(0) {
                return None;
            }
            Some(res)
        };
        if pairs_slice.len() == 1 {
            let (f, v) = pairs_slice[0];
            met.values = crate::value::arc_single_value(map_pair(f, v)?);
        } else if pairs_slice.len() <= 16 {
            let mut stack_vals: [Value; 16] = std::array::from_fn(|_| Value::Undefined);
            for (slot, &(f, v)) in stack_vals.iter_mut().zip(pairs_slice) {
                *slot = map_pair(f, v)?;
            }
            met.values = stack_vals[..pairs_slice.len()]
                .iter_mut()
                .map(|v| std::mem::replace(v, Value::Undefined))
                .collect();
        } else {
            let mut vec_vals = Vec::with_capacity(pairs_slice.len());
            for &(f, v) in pairs_slice {
                vec_vals.push(map_pair(f, v)?);
            }
            met.values = vec_vals.into();
        }
        met.refresh_direct_inner(true);
        Some(met)
    };
    if layers.len() == 1 {
        let Some(one) = without_query_signals(|| apply_one(&layers[0])) else {
            return Err(functions);
        };
        if let Some(cell) = functions.own_node()
            && let Node::Sequence(target) = &mut cell.kind
            && let Some(dst) = Arc::get_mut(target)
            && dst.len() == 1
        {
            dst[0] = one;
            return Ok(functions.with_steps(steps));
        }
        let Some(out) = node_of_one(one) else {
            return Err(functions);
        };
        return Ok(out.with_steps(steps));
    }
    let built = without_query_signals(|| {
        if layers.len() <= 8 {
            let mut buf: [Option<Layer>; 8] = Default::default();
            for (slot, layer) in buf.iter_mut().zip(layers.iter()) {
                *slot = Some(apply_one(layer)?);
            }
            return Some((Some(buf), None, layers.len()));
        }
        let met = layers.iter().map(apply_one).collect::<Option<Vec<_>>>()?;
        let len = met.len();
        Some((None, Some(met), len))
    });
    let Some((mut stack_buf, vec_buf, len)) = built else {
        return Err(functions);
    };
    if let Some(cell) = functions.own_node()
        && let Node::Sequence(target) = &mut cell.kind
        && let Some(dst) = Arc::get_mut(target)
        && dst.len() == len
    {
        if let Some(buf) = &mut stack_buf {
            for (d, s) in dst.iter_mut().zip(buf[..len].iter_mut()) {
                *d = s.take().expect("met layer present");
            }
        } else if let Some(met) = vec_buf {
            for (d, s) in dst.iter_mut().zip(met) {
                *d = s;
            }
        }
        return Ok(functions.with_steps(steps));
    }
    let out = match (stack_buf.as_mut(), vec_buf) {
        (Some(buf), _) => node_of_sound_stack(buf, len),
        (None, Some(met)) => node_of_sound(met),
        (None, None) => unreachable!("either stack or vec buffer present"),
    };
    Ok(out.with_steps(steps))
}

/// Whether each step of the sequence `functions` takes the data value
/// `control` in place by a `set` of a constant of it, as [`applied`] folds
/// that `set`. See [`Layer::takes_set`].
/// Whether `layers` takes a single plain non-`"value"` data control without
/// needing a temporary `OrderedMap` wrapper.
pub(crate) fn takes_plain_set_layers(layers: &[Layer]) -> bool {
    holds_data(layers)
        && layers.iter().all(|layer| {
            layer.in_cycles() && layer.maps() && layer.is_data() && !layer.values.is_empty()
        })
}

/// Whether `functions` takes a single plain non-`"value"` data control without
/// needing a temporary `OrderedMap` wrapper.
pub(crate) fn takes_plain_set(functions: &Pattern) -> bool {
    folded(functions).is_some_and(takes_plain_set_layers)
}

/// Set one plain `(key, value)` control directly on each step of `layers`.
pub(crate) fn set_single_here(layers: &mut Arc<[Layer]>, key: &crate::Key, value: Value) {
    if Arc::get_mut(layers).is_none() {
        *layers = layers.iter().map(|layer| layer.with(|_| {})).collect();
    }
    let layers = Arc::get_mut(layers).expect("layers that no other sequence holds");
    if let [layer] = layers
        && let Some(dress) = layer.dress.as_mut().and_then(Arc::get_mut)
    {
        dress.set_single_here(key, value);
        layer.refresh_direct_after_set();
        return;
    }
    let mut last: Option<(*const Dress, Arc<Dress>)> = None;
    for layer in layers {
        if let Some(dress) = layer.dress.as_mut().and_then(Arc::get_mut) {
            dress.set_single_here(key, value.clone());
            layer.refresh_direct_after_set();
            continue;
        }
        let from = layer.dress.as_ref().map_or(std::ptr::null(), Arc::as_ptr);
        let dress = match &last {
            Some((seen, dress)) if *seen == from => Arc::clone(dress),
            _ => {
                let mut dress = layer.dress().clone();
                dress.set_single_here(key, value.clone());
                let dress = dress.into_arc();
                last = Some((from, Arc::clone(&dress)));
                dress
            }
        };
        layer.dress = Some(dress);
        layer.refresh_direct_after_set();
    }
}

pub(crate) fn takes_set_layers(layers: &[Layer], control: &Value) -> bool {
    holds_data(layers)
        && control.is_data(0)
        && layers
            .iter()
            .all(|layer| layer.takes_set(control, LookupFlow::Set))
}

pub(crate) fn takes_set(functions: &Pattern, control: &Value) -> bool {
    folded(functions).is_some_and(|layers| takes_set_layers(layers, control))
}

/// Owned fast path of [`set_packed_here`] for a top-level `Node::Sequence`,
/// moving values out of `vals` with zero clones for single-layer sequences and
/// bare multi-layer sequences.
pub(crate) fn set_packed_layers_owned(
    layers: &mut Arc<[Layer]>,
    keys: &[u16],
    vals: &mut [Value],
    context: Locations,
) {
    if layers.len() == 1 {
        set_packed_single_layer_owned(layers, keys, vals, context);
        return;
    }
    if layers.iter().all(|layer| layer.dress.is_none()) {
        if Arc::get_mut(layers).is_none() {
            *layers = layers.iter().map(|layer| layer.with(|_| {})).collect();
        }
        let layers = Arc::get_mut(layers).expect("layers that no other sequence holds");
        let mut dress = Dress::default();
        dress.set_packed_here_owned(keys, vals, context);
        let shared = dress.into_arc();
        for layer in layers {
            layer.dress = Some(Arc::clone(&shared));
            layer.refresh_direct_after_set();
        }
        return;
    }
    let mut shared_entries = None;
    let mut shared_bare_dress = None;
    let mut shared_last_dress = None;
    set_packed_here(
        layers,
        keys,
        vals,
        context,
        &mut shared_entries,
        &mut shared_bare_dress,
        &mut shared_last_dress,
    );
}

fn finish_single_map_context(layer: &mut Layer, context: Locations) {
    if !context.is_empty() {
        if let Some(dress) = layer.dress.as_mut().and_then(Arc::get_mut) {
            dress.prepend_before(context);
        } else {
            let mut dress = layer.dress().clone();
            dress.prepend_before(context);
            layer.dress = Some(dress.into_arc());
        }
        layer.refresh_direct();
    }
}

/// Merge packed built-in controls directly into a single-value compound control
/// map layer (such as `s("[~ oh:2]*2")`) when no `Dress::set` is present, so
/// both native and scripted tiers construct the same direct layer without
/// allocating a `Dress::set` or lazy `Table` cache.
fn try_merge_single_map_owned(
    layer: &mut Layer,
    keys: &[u16],
    vals: &mut [Value],
    context: &mut Locations,
) -> bool {
    if layer.named_id != 0
        || layer
            .dress
            .as_deref()
            .is_some_and(|d| d.has_set() || d.has_custom_name())
        || layer.values.len() != 1
        || !matches!(layer.values[0], Value::Object(_))
        || !vals
            .iter()
            .all(|v| matches!(v, Value::F64(_) | Value::Str(_) | Value::Bool(_)))
    {
        return false;
    }
    if let Some(vals_mut) = Arc::get_mut(&mut layer.values) {
        let Value::Object(map) = &mut vals_mut[0] else {
            return false;
        };
        if !map.try_merge_packed_owned(keys, vals) {
            return false;
        }
    } else {
        let Value::Object(existing) = &layer.values[0] else {
            return false;
        };
        let mut cloned = existing.clone();
        if !cloned.try_merge_packed_owned(keys, vals) {
            return false;
        }
        layer.values = crate::value::arc_single_value(Value::Object(cloned));
    }
    finish_single_map_context(layer, std::mem::take(context));
    true
}

/// Borrowing counterpart of [`try_merge_single_map_owned`].
fn try_merge_single_map_ref(
    layer: &mut Layer,
    keys: &[u16],
    vals: &[Value],
    context: &mut Locations,
) -> bool {
    if layer.named_id != 0
        || layer
            .dress
            .as_deref()
            .is_some_and(|d| d.has_set() || d.has_custom_name())
        || layer.values.len() != 1
        || !matches!(layer.values[0], Value::Object(_))
        || !vals
            .iter()
            .all(|v| matches!(v, Value::F64(_) | Value::Str(_) | Value::Bool(_)))
    {
        return false;
    }
    if let Some(vals_mut) = Arc::get_mut(&mut layer.values) {
        let Value::Object(map) = &mut vals_mut[0] else {
            return false;
        };
        if !map.try_merge_packed_ref(keys, vals) {
            return false;
        }
    } else {
        let Value::Object(existing) = &layer.values[0] else {
            return false;
        };
        let mut cloned = existing.clone();
        if !cloned.try_merge_packed_ref(keys, vals) {
            return false;
        }
        layer.values = crate::value::arc_single_value(Value::Object(cloned));
    }
    finish_single_map_context(layer, std::mem::take(context));
    true
}

/// Single-layer owned fast path of [`set_packed_here`], moving values out of
/// `vals` straight into the layer's built-in control map with zero clones.
pub(crate) fn set_packed_single_layer_owned(
    layers: &mut Arc<[Layer]>,
    keys: &[u16],
    vals: &mut [Value],
    mut context: Locations,
) {
    if Arc::get_mut(layers).is_none() {
        *layers = layers.iter().map(|layer| layer.with(|_| {})).collect();
    }
    let layers = Arc::get_mut(layers).expect("layers that no other sequence holds");
    let [layer] = layers else {
        unreachable!("caller verified single layer");
    };
    if try_merge_single_map_owned(layer, keys, vals, &mut context) {
        return;
    }
    if let Some(dress) = layer.dress.as_mut().and_then(Arc::get_mut) {
        dress.set_packed_here_owned(keys, vals, context);
        layer.refresh_direct_after_set();
        return;
    }
    let mut dress = match layer.dress.as_ref() {
        Some(existing) => (**existing).clone(),
        None => Dress::default(),
    };
    dress.set_packed_here_owned(keys, vals, context);
    layer.dress = Some(dress.into_arc());
    layer.refresh_direct_after_set();
}

/// Set stack-packed built-in controls `(keys, vals)` at `context` on each step
/// of `layers`, avoiding a temporary `BuiltinMap` allocation when merging into
/// an existing `Dress::set_map` and sharing `shared_entries` / `shared_bare_dress`
/// across sibling sequences in stacks and picks.
pub(crate) fn set_packed_here(
    layers: &mut Arc<[Layer]>,
    keys: &[u16],
    vals: &[Value],
    mut context: Locations,
    shared_entries: &mut Option<Arc<dyn crate::value::BuiltinEntries>>,
    shared_bare_dress: &mut Option<Arc<Dress>>,
    shared_last_dress: &mut Option<(Arc<Dress>, Arc<Dress>)>,
) {
    if Arc::get_mut(layers).is_none() {
        *layers = layers.iter().map(|layer| layer.with(|_| {})).collect();
    }
    let layers = Arc::get_mut(layers).expect("layers that no other sequence holds");
    if let [layer] = layers {
        if try_merge_single_map_ref(layer, keys, vals, &mut context) {
            return;
        }
        if let Some(dress) = layer.dress.as_mut().and_then(Arc::get_mut) {
            dress.set_packed_here(keys, vals, context, shared_entries);
            layer.refresh_direct_after_set();
            return;
        }
        let Some(existing) = layer.dress.as_ref() else {
            let dress = shared_bare_dress.get_or_insert_with(|| {
                let mut dress = Dress::default();
                dress.set_packed_here(keys, vals, context, shared_entries);
                dress.into_arc()
            });
            layer.dress = Some(Arc::clone(dress));
            layer.refresh_direct_after_set();
            return;
        };
        let next = match shared_last_dress {
            Some((seen, dress)) if Arc::ptr_eq(seen, existing) => Arc::clone(dress),
            _ => {
                let mut dress = (**existing).clone();
                dress.set_packed_here(keys, vals, context, shared_entries);
                let dress = dress.into_arc();
                *shared_last_dress = Some((Arc::clone(existing), Arc::clone(&dress)));
                dress
            }
        };
        layer.dress = Some(next);
        layer.refresh_direct_after_set();
        return;
    }
    for layer in layers {
        if let Some(dress) = layer.dress.as_mut().and_then(Arc::get_mut) {
            dress.set_packed_here(keys, vals, context.clone(), shared_entries);
            layer.refresh_direct_after_set();
            continue;
        }
        let Some(existing) = layer.dress.as_ref() else {
            let dress = shared_bare_dress.get_or_insert_with(|| {
                let mut dress = Dress::default();
                dress.set_packed_here(keys, vals, context.clone(), shared_entries);
                dress.into_arc()
            });
            layer.dress = Some(Arc::clone(dress));
            layer.refresh_direct_after_set();
            continue;
        };
        let next = match shared_last_dress {
            Some((seen, dress)) if Arc::ptr_eq(seen, existing) => Arc::clone(dress),
            _ => {
                let mut dress = (**existing).clone();
                dress.set_packed_here(keys, vals, context.clone(), shared_entries);
                let dress = dress.into_arc();
                *shared_last_dress = Some((Arc::clone(existing), Arc::clone(&dress)));
                dress
            }
        };
        layer.dress = Some(next);
        layer.refresh_direct_after_set();
    }
}

/// Set `control` at `context` on each step of `layers`, which
/// [take it](takes_set). The layers are written where no other sequence
/// holds them, and a copy of them otherwise.
pub(crate) fn set_here(layers: &mut Arc<[Layer]>, control: &Value, context: Locations) {
    let mut cache = RenoteDressCache::default();
    set_here_cached(layers, control, context, &mut cache);
}

/// [`set_here`] sharing newly constructed dresses across multiple sequences
/// (such as branches of a `Node::Pick`) via `cache`. Only immutable source
/// dresses (`None` or shared `Arc`) are inserted into `cache`.
pub(crate) fn set_here_cached(
    layers: &mut Arc<[Layer]>,
    control: &Value,
    mut context: Locations,
    cache: &mut RenoteDressCache,
) {
    if Arc::get_mut(layers).is_none() {
        *layers = layers.iter().map(|layer| layer.with(|_| {})).collect();
    }
    let layers = Arc::get_mut(layers).expect("layers that no other sequence holds");
    if let [layer] = layers {
        if let Value::Object(map) = control {
            if let Some((pk, val)) = map.single_packed_entry()
                && try_merge_single_map_ref(layer, &[pk], std::slice::from_ref(val), &mut context)
            {
                return;
            } else if let Some(entries) = map.as_builtin()
                && let crate::value::Slice::Builtin(keys, vals) = entries.as_slice()
                && try_merge_single_map_ref(layer, keys, vals, &mut context)
            {
                return;
            }
        }
        let from = layer.dress.as_ref().map_or(std::ptr::null(), Arc::as_ptr);
        if let Some(cached) = cache.get(from) {
            layer.dress = cached;
            layer.refresh_direct();
            return;
        }
        if let Some(dress) = layer.dress.as_mut().and_then(Arc::get_mut) {
            dress.set_here(control, context);
            layer.refresh_direct();
            return;
        }
        let mut dress = layer.dress().clone();
        dress.set_here(control, context);
        let dress = dress.into_arc();
        cache.put(from, Some(Arc::clone(&dress)));
        layer.dress = Some(dress);
        layer.refresh_direct();
        return;
    }
    for layer in layers {
        let from = layer.dress.as_ref().map_or(std::ptr::null(), Arc::as_ptr);
        if let Some(cached) = cache.get(from) {
            layer.dress = cached;
            layer.refresh_direct();
            continue;
        }
        if let Some(dress) = layer.dress.as_mut().and_then(Arc::get_mut) {
            dress.set_here(control, context.clone());
            layer.refresh_direct();
            continue;
        }
        let mut dress = layer.dress().clone();
        dress.set_here(control, context.clone());
        let dress = dress.into_arc();
        cache.put(from, Some(Arc::clone(&dress)));
        layer.dress = Some(dress);
        layer.refresh_direct();
    }
}

/// The `appRight` of `functions` and `values` by `apply`, when `values` is
/// a sequence and `functions` is one layer of data values, or a constant
/// of one, as the `struct` of a sequence is: each step of `values` keeps
/// its whole, and is cut to each step of `functions` that it meets.
///
/// `None` for functions of more layers than one, which answer each step
/// layer by layer, for a value that is not data, for a function that is no
/// compose op, for a meeting that makes more than a value, and for cut
/// steps that are not in the order of time.
pub(crate) fn applied_out(
    functions: &Pattern,
    values: &Pattern,
    apply: &Combine,
    flow: LookupFlow,
) -> Option<Pattern> {
    if !runs_at_the_build(apply) {
        return None;
    }
    let functions = layers(functions)?;
    let [functions] = &*functions else {
        return None;
    };
    let values = layers(values)?;
    // A value of the script is read through its host, at each query.
    if !(functions.is_data() && holds_data(&values))
        || functions.has_rev_order()
        || values.iter().any(Layer::has_rev_order)
    {
        return None;
    }
    without_query_signals(|| {
        let met = values
            .iter()
            .map(|layer| {
                let (met, pairs) = layer.met(functions, Side::Values)?;
                // What each pair of values makes, with no location: the
                // locations of both sides are those of the steps.
                let values = pairs
                    .as_slice()
                    .iter()
                    .map(|&(value, function)| {
                        let hf = cycle_hap(functions.value(function as usize)?);
                        let hv = cycle_hap(layer.value(value as usize)?);
                        let hap = applied_hap(&hf, &hv, hv.whole(), hv.part(), apply, flow);
                        is_plain(&hap).then(|| {
                            let mut value = hap.value;
                            if let Value::Object(map) = &mut value {
                                map.undress_patched();
                            }
                            value
                        })
                    })
                    .collect::<Option<Vec<Value>>>()?;
                Some(met.valued(values.into()))
            })
            .collect::<Option<Vec<_>>>()?;
        Some(node_of_sound(met))
    })
}

/// `pattern` with no step whose value `keep` refuses, when `pattern` is a
/// sequence of data values: what a filter of the values answers.
pub(crate) fn filtered(pattern: &Pattern, keep: &ValuePredicate) -> Option<Pattern> {
    let layers = folded(pattern)?;
    if !holds_data(layers) {
        return None;
    }
    without_query_signals(|| {
        let layers = layers
            .iter()
            .map(|layer| {
                let kept: Vec<bool> = layer.each_value().map(|value| keep(&value)).collect();
                if kept.iter().all(|kept| *kept) {
                    return layer.clone();
                }
                let listing = layer.listing();
                let raws: Vec<Raw> = (0..listing.len())
                    .filter(|index| {
                        kept.get(listing.value(*index) as usize)
                            .copied()
                            .unwrap_or(false)
                    })
                    .map(|index| listing.raw(index))
                    .collect();
                layer.with(|layer| {
                    layer.set_in_cycles(lie_in_spans(layer.period(), raws.iter().map(Raw::part)));
                    layer.set_steps(Steps::Listed(Listing::of(raws)));
                })
            })
            .collect();
        Some(node_of_sound(layers))
    })
}

/// The stack of `patterns`, when each is a sequence, a constant or silence:
/// the layers of each, one after the other.
pub(crate) fn stacked(patterns: &[Pattern]) -> Option<Pattern> {
    stacked_iter(patterns.iter())
}

pub(crate) fn stacked_opt(patterns: &[Option<Pattern>]) -> Option<Pattern> {
    stacked_iter(patterns.iter().filter_map(Option::as_ref))
}

pub(super) fn stacked_iter<'a>(
    patterns: impl Iterator<Item = &'a Pattern> + Clone,
) -> Option<Pattern> {
    let mut total = 0usize;
    for pattern in patterns.clone() {
        if pattern.is_rest() && folds(pattern) {
            continue;
        }
        total += layers(pattern)?.len();
    }
    if total <= 8 {
        let mut buf: [Option<Layer>; 8] = Default::default();
        let mut len = 0usize;
        for pattern in patterns {
            if pattern.is_rest() && folds(pattern) {
                continue;
            }
            for layer in layers(pattern)?.iter() {
                buf[len] = Some(layer.clone());
                len += 1;
            }
        }
        return Some(node_of_sound_stack(&mut buf, len));
    }
    let mut stacked = Vec::with_capacity(total);
    for pattern in patterns {
        if pattern.is_rest() && folds(pattern) {
            continue;
        }
        stacked.extend(layers(pattern)?.iter().cloned());
    }
    Some(node_of_sound(stacked))
}

/// Try to rescale an owned `Node::Sequence` by `factor` in place when unshared,
/// updating unshared `CompactGrid`s in place with zero heap allocations.
pub(crate) fn faster_in_place(layers: &mut Arc<[Layer]>, factor: Fraction) -> bool {
    if factor <= Fraction::ZERO {
        return false;
    }
    let small =
        |period: &Fraction| period.numer() <= MAX_PERIOD_TERM && period.denom() <= MAX_PERIOD_TERM;
    let Some(slice) = Arc::get_mut(layers) else {
        return false;
    };
    // Validate every layer's new period fits before mutating any layer.
    for layer in slice.iter() {
        if layer.has_rev_order() || layer.period().checked_div(factor).filter(small).is_none() {
            return false;
        }
    }
    // Fast path: when every layer has a listed CompactGrid, try in-place first,
    // falling back per-layer to `GridView::faster` (which never mutates until
    // all layers are validated).
    if slice.len() == 1 {
        let layer = &mut slice[0];
        let period = layer.period().checked_div(factor).expect("validated");
        if (layer.period_denom_flags & LAYER_DENOM_MASK) != 0
            && let Steps::Listed(Listing::CompactGrid(grid)) = &mut layer.steps
            && grid.try_faster_in_place(factor).is_some()
        {
            let in_cycles = match &layer.steps {
                Steps::Listed(listing) => listing
                    .with_grid_view(|view| view.in_cycles_for(period))
                    .unwrap_or(false),
                _ => false,
            };
            let steps = std::mem::replace(
                &mut layer.steps,
                Steps::Listed(Listing::CompactGrid(CompactGrid::OneStep([0; 12]))),
            );
            layer.set_in_cycles(in_cycles);
            layer.set_period_and_steps(period, steps);
            return true;
        }
        let listing = layer.listing();
        if let Some(Some((new_listing, in_cycles))) =
            listing.with_grid_view(|grid| grid.faster(factor, period))
        {
            layer.set_in_cycles(in_cycles);
            layer.set_period_and_steps(period, Steps::Listed(new_listing));
            return true;
        }
        return false;
    }
    // Multi-layer owned sequence: ensure every layer can rescale via GridView
    // before mutating any layer so partial mutation cannot happen.
    let can_grid = slice.iter().all(|layer| {
        let period = layer.period().checked_div(factor).expect("validated");
        // Verify factor fits u32 and period fits; actual rescaling is infallible
        // when grid.faster would succeed, so check via dry-run for non-inline or
        // only proceed when all layers are listed CompactGrid/Grid with small ticks.
        let _ = period;
        matches!(
            &layer.steps,
            Steps::Listed(Listing::CompactGrid(_)) if (layer.period_denom_flags & LAYER_DENOM_MASK) != 0
        )
    });
    if !can_grid {
        return false;
    }
    // Check that every layer's ticks won't overflow `GridView::faster` before mutating.
    let f_num = match u32::try_from(factor.numer()) {
        Ok(v) if v != 0 => v,
        _ => return false,
    };
    let f_den = match u32::try_from(factor.denom()) {
        Ok(v) if v != 0 => v,
        _ => return false,
    };
    let all_fit = slice.iter().all(|layer| {
        let Steps::Listed(listing) = &layer.steps else {
            return false;
        };
        listing
            .with_grid_view(|view| {
                let g0 = gcd(u64::from(view.ticks.get()), u64::from(f_den)) as u32;
                let ticks_1 = view.ticks.get() / g0;
                let mul = f_den / g0;
                let Some(ticks_2) = u64::from(ticks_1).checked_mul(u64::from(f_num)) else {
                    return false;
                };
                if u32::try_from(ticks_2)
                    .ok()
                    .and_then(|v| i32::try_from(v).ok())
                    .is_none()
                {
                    return false;
                }
                if mul > 1 {
                    for i in 0..view.steps.len() {
                        let (b, e) = view.steps.times_of(i);
                        if u64::from(b)
                            .checked_mul(u64::from(mul))
                            .and_then(|v| u32::try_from(v).ok())
                            .is_none()
                            || u64::from(e)
                                .checked_mul(u64::from(mul))
                                .and_then(|v| u32::try_from(v).ok())
                                .is_none()
                        {
                            return false;
                        }
                    }
                }
                true
            })
            .unwrap_or(false)
    });
    if !all_fit {
        return false;
    }
    for layer in slice.iter_mut() {
        let period = layer.period().checked_div(factor).expect("validated");
        if let Steps::Listed(Listing::CompactGrid(grid)) = &mut layer.steps
            && grid.try_faster_in_place(factor).is_some()
        {
            let in_cycles = match &layer.steps {
                Steps::Listed(listing) => listing
                    .with_grid_view(|view| view.in_cycles_for(period))
                    .unwrap_or(false),
                _ => false,
            };
            let steps = std::mem::replace(
                &mut layer.steps,
                Steps::Listed(Listing::CompactGrid(CompactGrid::OneStep([0; 12]))),
            );
            layer.set_in_cycles(in_cycles);
            layer.set_period_and_steps(period, steps);
            continue;
        }
        let listing = layer.listing();
        let (new_listing, in_cycles) = listing
            .with_grid_view(|grid| grid.faster(factor, period))
            .flatten()
            .expect("validated grid faster");
        layer.set_in_cycles(in_cycles);
        layer.set_period_and_steps(period, Steps::Listed(new_listing));
    }
    true
}

/// `pattern`, `factor` times as fast, when it is a sequence or a constant
/// and the factor is above zero.
pub(crate) fn faster(pattern: &Pattern, factor: Fraction) -> Option<Pattern> {
    let layers = layers(pattern)?;
    if factor <= Fraction::ZERO {
        return None;
    }
    let small =
        |period: &Fraction| period.numer() <= MAX_PERIOD_TERM && period.denom() <= MAX_PERIOD_TERM;
    let faster = |span: TimeSpan| {
        Some(TimeSpan::new(
            span.begin.checked_div(factor)?,
            span.end.checked_div(factor)?,
        ))
    };
    let rescale_layer = |layer: &Layer| -> Option<Layer> {
        if layer.has_rev_order() {
            return None;
        }
        let period = layer.period().checked_div(factor).filter(small)?;
        // The times of the steps are in cycles, so they change with
        // the speed.
        let listing = layer.listing();
        if let Some(Some((new_listing, in_cycles))) =
            listing.with_grid_view(|grid| grid.faster(factor, period))
        {
            return Some(layer.with(|layer| {
                layer.set_in_cycles(in_cycles);
                layer.set_period_and_steps(period, Steps::Listed(new_listing));
            }));
        }
        let raws = (0..listing.len())
            .map(|index| {
                let (whole, part) = listing.times(index);
                let part = match part {
                    Some(part) => Some(faster(part)?),
                    None => None,
                };
                Some(Raw {
                    whole: faster(whole)?,
                    part,
                    value: listing.value(index),
                    locations: listing.locations(index),
                })
            })
            .collect::<Option<Vec<_>>>()?;
        Some(layer.with(|layer| {
            layer.set_in_cycles(lie_in_spans(period, raws.iter().map(Raw::part)));
            layer.set_period_and_steps(period, Steps::Listed(Listing::of(raws)));
        }))
    };
    if layers.len() <= 8 {
        let mut buf: [Option<Layer>; 8] = Default::default();
        for (slot, layer) in buf.iter_mut().zip(layers.iter()) {
            *slot = Some(rescale_layer(layer)?);
        }
        return Some(node_of_sound_stack(&mut buf, layers.len()));
    }
    let layers = layers
        .iter()
        .map(rescale_layer)
        .collect::<Option<Vec<_>>>()?;
    Some(node_of_sound(layers))
}

/// `pattern` with each step `count` times in the time of the step, one
/// after the other: what a `ply` makes of a sequence of whole steps of
/// data, with no join at each query. `None` for any other pattern, for a
/// step that is cut or has no width, for more than [`MAX_FOLDED_STEPS`]
/// steps in a layer, and for a step that begins before the step before it
/// ends: the join answers each of the one step and then each of the
/// other, which is not the order of time that a layer has.
pub(crate) fn plied(pattern: &Pattern, count: usize) -> Option<Pattern> {
    let layers = layers(pattern)?;
    let share = Fraction::int(i128::try_from(count).ok().filter(|count| *count > 0)?);
    let layers = layers
        .iter()
        .map(|layer| {
            if layer.has_rev_order() {
                return None;
            }
            let steps = layer.listing();
            let listed = steps.len().checked_mul(count)?;
            if !layer.is_data() || listed > MAX_FOLDED_STEPS {
                return None;
            }
            let mut raws = Vec::with_capacity(listed);
            let mut before = None;
            for index in 0..steps.len() {
                let whole = steps.whole(index);
                let (begin, end) = (whole.begin, whole.end);
                let sounds_on = count > 1 && before.is_some_and(|before| begin < before);
                if steps.is_cut(index) || begin >= end || sounds_on {
                    return None;
                }
                before = Some(end);
                let width = end.checked_sub(begin)?.checked_div(share)?;
                let mut from = begin;
                for at in 1..=count {
                    // The last one ends where the step ends.
                    let to = match at == count {
                        true => end,
                        false => {
                            let at = Fraction::int(i128::try_from(at).ok()?);
                            begin.checked_add(width.checked_mul(at)?)?
                        }
                    };
                    raws.push(Raw {
                        whole: TimeSpan::new(from, to),
                        part: None,
                        value: steps.value(index),
                        locations: steps.locations(index),
                    });
                    from = to;
                }
            }
            Some(layer.with(|layer| {
                layer.set_in_cycles(lie_in_spans(layer.period(), raws.iter().map(Raw::part)));
                layer.set_steps(Steps::Listed(Listing::of(raws)));
            }))
        })
        .collect::<Option<Vec<_>>>()?;
    Some(node_of_sound(layers))
}

pub(super) fn clone_packed_runs(packed: PackedRunsRef<'_>) -> PackedRunsInput {
    match packed {
        PackedRunsRef::None => PackedRunsInput::None,
        PackedRunsRef::Small(runs) => PackedRunsInput::Small(SmallPackedRuns::from_slice(runs)),
        PackedRunsRef::Wide(runs) => PackedRunsInput::Wide(Box::from(runs)),
    }
}

pub(super) fn pack_small_steps(steps: Vec<SmallGridStep>) -> GridSteps {
    if steps.len() <= MAX_INLINE_GRID_LEN {
        let mut arr = [GridSteps::ZERO_SMALL; MAX_INLINE_GRID_LEN];
        arr[..steps.len()].copy_from_slice(&steps);
        GridSteps::Inline(steps.len() as u8, arr)
    } else {
        GridSteps::Small(steps.into_boxed_slice())
    }
}

/// Fold `rev()` onto a `Node::Sequence` whose layers are uncut, disjoint
/// small grids with `period = 1/k` (`k >= 1`, `ticks % k == 0`).
pub(crate) fn reversed(pattern: &Pattern) -> Option<Pattern> {
    let layers = layers(pattern)?;
    let rev_layer = |layer: &Layer| -> Option<Layer> {
        if layer.source().is_some() || !layer.in_cycles() {
            return None;
        }
        let period = layer.period();
        if period.numer() != 1 {
            return None;
        }
        let k = u32::try_from(period.denom()).ok()?;
        if k == 0 {
            return None;
        }
        let listing = layer.listing();
        let (new_listing, multi_step) = listing.with_grid_view(|grid| {
            if grid.rare.is_some() || grid.ticks.get() % k != 0 {
                return None;
            }
            let period_ticks = u16::try_from(grid.ticks.get() / k).ok()?;
            let GridStepsRef::Small(steps) = grid.steps else {
                return None;
            };
            // Multi-step cycles require disjoint, in-order steps so reversing
            // keeps the grid sorted; `Layer::rev_mask` preserves the exact
            // descending emission order for `Order::Graph` queries.
            if !steps.windows(2).all(|w| w[0].part_end <= w[1].part_begin) {
                return None;
            }
            if steps.last().is_some_and(|s| s.part_end > period_ticks) {
                return None;
            }
            let grid_steps = if steps.len() <= MAX_INLINE_GRID_LEN {
                let mut arr = [GridSteps::ZERO_SMALL; MAX_INLINE_GRID_LEN];
                for (dst, &s) in arr.iter_mut().zip(steps.iter().rev()) {
                    *dst = SmallGridStep {
                        part_begin: period_ticks - s.part_end,
                        part_end: period_ticks - s.part_begin,
                        value: s.value,
                        loc: s.loc,
                    };
                }
                GridSteps::Inline(steps.len() as u8, arr)
            } else {
                let mut rev_steps = Vec::with_capacity(steps.len());
                for &s in steps.iter().rev() {
                    rev_steps.push(SmallGridStep {
                        part_begin: period_ticks - s.part_end,
                        part_end: period_ticks - s.part_begin,
                        value: s.value,
                        loc: s.loc,
                    });
                }
                GridSteps::Small(rev_steps.into_boxed_slice())
            };
            Some((
                Listing::from_parts(
                    grid.ticks,
                    grid.base | Grid::SMALL,
                    grid_steps,
                    clone_packed_runs(grid.packed),
                    None,
                ),
                steps.len() > 1,
            ))
        })??;
        let next_rev = if multi_step {
            if (layer.rev_mask() & 1) != 0 { 0 } else { 1 }
        } else {
            0
        };
        Some(layer.with(|l| {
            l.set_period_and_steps(period, Steps::Listed(new_listing));
            l.set_rev_mask(next_rev);
        }))
    };
    if layers.len() <= 8 {
        let mut buf: [Option<Layer>; 8] = Default::default();
        for (slot, layer) in buf.iter_mut().zip(layers.iter()) {
            *slot = Some(rev_layer(layer)?);
        }
        return Some(node_of_sound_stack(&mut buf, layers.len()));
    }
    let out = layers.iter().map(rev_layer).collect::<Option<Vec<_>>>()?;
    Some(node_of_sound(out))
}

/// Shift one uncut `period == 1` small-grid layer by `by` when `by * ticks` is
/// an integer and no step straddles the wrap boundary.
pub(super) fn shift_one_layer(layer: &Layer, by: Fraction) -> Option<Layer> {
    if layer.has_rev_order()
        || layer.source().is_some()
        || !layer.in_cycles()
        || layer.period() != Fraction::ONE
    {
        return None;
    }
    let listing = layer.listing();
    let new_listing = listing.with_grid_view(|grid| {
        if grid.rare.is_some() {
            return None;
        }
        let GridStepsRef::Small(steps) = grid.steps else {
            return None;
        };
        let ticks_i = i128::from(grid.ticks.get());
        let ticks_u16 = u16::try_from(ticks_i).ok()?;
        let prod = by.checked_mul(Fraction::int(ticks_i))?;
        if prod.denom() != 1 {
            return None;
        }
        let shift_mod = u16::try_from(prod.numer().rem_euclid(ticks_i)).ok()?;
        if shift_mod == 0 {
            return Some(None);
        }
        let cut = ticks_u16 - shift_mod;
        if !steps
            .iter()
            .all(|s| s.part_end <= ticks_u16 && (s.part_end <= cut || s.part_begin >= cut))
        {
            return None;
        }
        let split = steps.partition_point(|s| s.part_begin < cut);
        if !steps[..split].iter().all(|s| s.part_end <= cut)
            || !steps[split..].iter().all(|s| s.part_begin >= cut)
        {
            return None;
        }
        let grid_steps = if steps.len() <= MAX_INLINE_GRID_LEN {
            let mut arr = [GridSteps::ZERO_SMALL; MAX_INLINE_GRID_LEN];
            let mut idx = 0usize;
            for &s in &steps[split..] {
                arr[idx] = SmallGridStep {
                    part_begin: s.part_begin - cut,
                    part_end: s.part_end - cut,
                    value: s.value,
                    loc: s.loc,
                };
                idx += 1;
            }
            for &s in &steps[..split] {
                arr[idx] = SmallGridStep {
                    part_begin: s.part_begin + shift_mod,
                    part_end: s.part_end + shift_mod,
                    value: s.value,
                    loc: s.loc,
                };
                idx += 1;
            }
            GridSteps::Inline(steps.len() as u8, arr)
        } else {
            let mut rotated = Vec::with_capacity(steps.len());
            for &s in &steps[split..] {
                rotated.push(SmallGridStep {
                    part_begin: s.part_begin - cut,
                    part_end: s.part_end - cut,
                    value: s.value,
                    loc: s.loc,
                });
            }
            for &s in &steps[..split] {
                rotated.push(SmallGridStep {
                    part_begin: s.part_begin + shift_mod,
                    part_end: s.part_end + shift_mod,
                    value: s.value,
                    loc: s.loc,
                });
            }
            GridSteps::Small(rotated.into_boxed_slice())
        };
        Some(Some(Listing::from_parts(
            grid.ticks,
            grid.base | Grid::SMALL,
            grid_steps,
            clone_packed_runs(grid.packed),
            None,
        )))
    })??;
    Some(match new_listing {
        Some(listing) => layer.with(|l| {
            l.set_period_and_steps(Fraction::ONE, Steps::Listed(listing));
        }),
        None => layer.clone(),
    })
}

/// Fold `late(by)` / `early(-by)` onto a `Node::Sequence` whose layers have
/// `period == 1`, uncut small grids, integer tick shift `by * ticks`, and no
/// step straddling the wrap boundary.
pub(crate) fn shifted(pattern: &Pattern, by: Fraction) -> Option<Pattern> {
    let layers = layers(pattern)?;
    if by == Fraction::ZERO {
        return Some(pattern.clone());
    }
    if by.numer().abs() > MAX_PERIOD_TERM || by.denom() > MAX_PERIOD_TERM {
        return None;
    }
    if layers.len() <= 8 {
        let mut buf: [Option<Layer>; 8] = Default::default();
        for (slot, layer) in buf.iter_mut().zip(layers.iter()) {
            *slot = Some(shift_one_layer(layer, by)?);
        }
        return Some(node_of_sound_stack(&mut buf, layers.len()));
    }
    let out = layers
        .iter()
        .map(|layer| shift_one_layer(layer, by))
        .collect::<Option<Vec<_>>>()?;
    Some(node_of_sound(out))
}

/// Fold `off(by, f)` directly onto a `Node::Sequence` without allocating an
/// intermediate shifted `Arc<[Layer]>` or `Arc<NodeCell>`.
pub(crate) fn off_shifted(
    pattern: &Pattern,
    by: Fraction,
    f: Option<&crate::value::FunctionRef>,
) -> Option<Pattern> {
    use crate::compose::{Alignment, ComposeOp, default_alignment};
    use crate::value::OffLayerOp;
    if !folds(pattern)
        || pattern.pure_loc_raw().is_some()
        || by.numer().abs() > MAX_PERIOD_TERM
        || by.denom() > MAX_PERIOD_TERM
    {
        return None;
    }
    let Node::Sequence(layers) = pattern.node_kind() else {
        return None;
    };
    if layers.is_empty() || layers.len() > 4 || !holds_data(layers) {
        return None;
    }
    let op = match f {
        None => None,
        Some(func) => Some(func.as_off_layer_op()?),
    };
    // Pre-extract RHS layer for `OffLayerOp::Compose` so we only check once.
    let compose_rhs = match op {
        Some(OffLayerOp::Compose {
            other, op, align, ..
        }) => {
            if op == ComposeOp::KeepIf || align.unwrap_or_else(default_alignment) != Alignment::In {
                return None;
            }
            let rhs_layers = self::layers(other)?;
            if rhs_layers.len() != 1 || !rhs_layers[0].is_data() {
                return None;
            }
            Some((rhs_layers, op))
        }
        Some(OffLayerOp::ComposeNumber { op, align, .. }) => {
            if op == ComposeOp::KeepIf || align.unwrap_or_else(default_alignment) != Alignment::In {
                return None;
            }
            None
        }
        Some(OffLayerOp::ControlNumber { spec, .. }) => {
            if spec.names.len() != 1 || spec.packed_key == 0 {
                return None;
            }
            None
        }
        None => None,
    };
    let n = layers.len();
    let mut buf: [Option<Layer>; 8] = Default::default();
    for (i, layer) in layers.iter().enumerate() {
        let mut shifted = if by == Fraction::ZERO {
            layer.clone()
        } else {
            shift_one_layer(layer, by)?
        };
        match op {
            None => {}
            Some(OffLayerOp::Compose { op, .. }) => {
                let (rhs_layers, _) = compose_rhs.as_ref()?;
                let apply = Combine::Composed(op);
                let flow = op.lookup_flow();
                shifted = without_query_signals(|| {
                    apply_layer_with(&shifted, &rhs_layers[0], &apply, flow)
                })?;
            }
            Some(OffLayerOp::ComposeNumber { op, value, .. }) => {
                let val = Value::F64(value);
                let flow = op.lookup_flow();
                shifted =
                    without_query_signals(|| apply_constant_to_layer(&shifted, &val, op, flow))?;
            }
            Some(OffLayerOp::ControlNumber { spec, value }) => {
                let k = spec.packed_key;
                if !shifted.is_direct_keyed()
                    || shifted.dress().tail.is_some()
                    || (k & 0x03ff) == shifted.named_id
                {
                    return None;
                }
                let mut shared = None;
                let v = Value::F64(value);
                shifted.change_dress(|dress| {
                    dress.set_packed_here(
                        &[k],
                        std::slice::from_ref(&v),
                        Locations::new(),
                        &mut shared,
                    );
                });
                shifted.refresh_direct();
            }
        }
        buf[i] = Some(layer.clone());
        buf[n + i] = Some(shifted);
    }
    Some(node_of_sound_stack(&mut buf, n * 2).with_steps(pattern.steps()))
}

/// Fold `every(n, rev)` / `firstOf(n, rev)` directly on a single-layer
/// `Node::Sequence` (`2 <= n <= 6`, `period = 1/k`, disjoint uncut steps) in
/// one pass, mutating uniquely-owned patterns in place without allocating a new
/// `NodeCell`, `Arc<[Layer]>`, or intermediate `rev()` node.
pub(crate) fn every_rev_owned(mut pattern: Pattern, n: usize) -> Result<Pattern, Pattern> {
    if !(2..=6).contains(&n) {
        return Err(pattern);
    }
    let Some(ls) = layers(&pattern) else {
        return Err(pattern);
    };
    if ls.len() != 1 {
        return Err(pattern);
    }
    let first = &ls[0];
    if first.source().is_some() || !first.in_cycles() || first.has_rev_order() {
        return Err(pattern);
    }
    let period = first.period();
    if period.numer() != 1 {
        return Err(pattern);
    }
    let Ok(k) = u32::try_from(period.denom()) else {
        return Err(pattern);
    };
    if k == 0 {
        return Err(pattern);
    }
    let listing = first.listing();
    let Some((new_listing, multi_step)) = listing
        .with_grid_view(|grid| {
            if grid.rare.is_some() {
                return None;
            }
            let ticks_u32 = grid.ticks.get();
            if ticks_u32 % k != 0 || n.checked_mul(ticks_u32 as usize)? > usize::from(u16::MAX) {
                return None;
            }
            let sub_ticks = ticks_u32 / k;
            let sub_ticks_u16 = u16::try_from(sub_ticks).ok()?;
            let GridStepsRef::Small(steps) = grid.steps else {
                return None;
            };
            if !steps.windows(2).all(|w| w[0].part_end <= w[1].part_begin) {
                return None;
            }
            if steps.last().is_some_and(|s| s.part_end > sub_ticks_u16) {
                return None;
            }
            let per_cycle = steps.len().checked_mul(k as usize)?;
            let total_steps = per_cycle.checked_mul(n)?;
            if total_steps > 256 {
                return None;
            }
            let mut stack_steps = [GridSteps::ZERO_SMALL; 64];
            let mut heap_steps = (total_steps > 64).then(|| Vec::with_capacity(total_steps));
            let mut cursor = 0usize;
            let mut push_step = |step: SmallGridStep| {
                if let Some(vec) = &mut heap_steps {
                    vec.push(step);
                } else {
                    stack_steps[cursor] = step;
                    cursor += 1;
                }
            };
            // Cycle 0: reversed within each `1/k` sub-cycle.
            for copy in 0..k {
                let offset = (copy * sub_ticks) as u16;
                for &s in steps.iter().rev() {
                    push_step(SmallGridStep {
                        part_begin: offset + (sub_ticks_u16 - s.part_end),
                        part_end: offset + (sub_ticks_u16 - s.part_begin),
                        value: s.value,
                        loc: s.loc,
                    });
                }
            }
            // Cycles 1..n: original order.
            for cycle_idx in 1..n {
                let cycle_off = (cycle_idx as u32) * ticks_u32;
                for copy in 0..k {
                    let offset = (cycle_off + copy * sub_ticks) as u16;
                    for &s in steps {
                        push_step(SmallGridStep {
                            part_begin: offset + s.part_begin,
                            part_end: offset + s.part_end,
                            value: s.value,
                            loc: s.loc,
                        });
                    }
                }
            }
            let grid_steps = if let Some(vec) = heap_steps {
                pack_small_steps(vec)
            } else if total_steps <= MAX_INLINE_GRID_LEN {
                let mut arr = [GridSteps::ZERO_SMALL; MAX_INLINE_GRID_LEN];
                arr[..total_steps].copy_from_slice(&stack_steps[..total_steps]);
                GridSteps::Inline(total_steps as u8, arr)
            } else {
                GridSteps::Small(Box::from(&stack_steps[..total_steps]))
            };
            Some((
                Listing::from_parts(
                    grid.ticks,
                    grid.base | Grid::SMALL,
                    grid_steps,
                    clone_packed_runs(grid.packed),
                    None,
                ),
                steps.len() > 1,
            ))
        })
        .flatten()
    else {
        return Err(pattern);
    };
    let rev_mask = if multi_step { 1u8 } else { 0u8 };
    let new_period = Fraction::int(n as i128);
    if let Some(cell) = pattern.own_node()
        && let Node::Sequence(target) = &mut cell.kind
        && let Some(dst) = Arc::get_mut(target)
        && dst.len() == 1
    {
        let l = &mut dst[0];
        l.set_in_cycles(true);
        l.set_period_and_steps(new_period, Steps::Listed(new_listing));
        l.set_rev_mask(rev_mask);
        return Ok(pattern);
    }
    let steps = pattern.steps();
    let cloned = layers(&pattern).expect("single sequence layer verified above")[0].with(|l| {
        l.set_in_cycles(true);
        l.set_period_and_steps(new_period, Steps::Listed(new_listing));
        l.set_rev_mask(rev_mask);
    });
    let Some(mut out) = node_of_one(cloned) else {
        return Err(pattern);
    };
    if let Some(s) = steps {
        out = out.with_steps(Some(s));
    }
    Ok(out)
}

/// Fold `slowcat_prime` (`every` / `firstOf` / `lastOf`) of `2..=16`
/// single-layer sequences sharing values/dress/ticks and integer-divisor
/// periods (`period = 1/k`) into one multi-cycle `Node::Sequence`.
pub(crate) fn slowcat_primed(pats: &[Pattern]) -> Option<Pattern> {
    let n = pats.len();
    if !(2..=16).contains(&n) {
        return None;
    }
    let first_layers = layers(&pats[0])?;
    if first_layers.len() != 1 {
        return None;
    }
    let first = &first_layers[0];
    if first.source().is_some() || !first.in_cycles() {
        return None;
    }
    let (ticks, base, packed_in) = first.listing().with_grid_view(|grid| {
        if grid.rare.is_some() {
            return None;
        }
        Some((grid.ticks, grid.base, clone_packed_runs(grid.packed)))
    })??;
    let ticks_u32 = ticks.get();
    if n.checked_mul(ticks_u32 as usize)? > usize::from(u16::MAX) {
        return None;
    }
    let mut total_steps = 0usize;
    let mut rev_mask = 0u8;
    let mut inline_steps = [GridSteps::ZERO_SMALL; MAX_INLINE_GRID_LEN];
    let mut all_steps: Option<Vec<SmallGridStep>> = None;
    for (cycle_idx, pat) in pats.iter().enumerate() {
        let ls = layers(pat)?;
        if ls.len() != 1 {
            return None;
        }
        let layer = &ls[0];
        if layer.has_rev_order() {
            if n > 6 {
                return None;
            }
            if (layer.rev_mask() & 1) != 0 {
                rev_mask |= 1u8 << cycle_idx;
            }
        }
        if layer.source().is_some()
            || !layer.in_cycles()
            || layer.named_id != first.named_id
            || !Arc::ptr_eq(&layer.values, &first.values)
        {
            return None;
        }
        let same_dress = match (&layer.dress, &first.dress) {
            (None, None) => true,
            (Some(a), Some(b)) => Arc::ptr_eq(a, b),
            _ => false,
        };
        if !same_dress {
            return None;
        }
        let period = layer.period();
        if period.numer() != 1 {
            return None;
        }
        let k = u32::try_from(period.denom()).ok()?;
        if k == 0 || ticks_u32 % k != 0 {
            return None;
        }
        let sub_ticks = ticks_u32 / k;
        let ok = layer.listing().with_grid_view(|grid| {
            if grid.rare.is_some() || grid.ticks != ticks || grid.base != base {
                return false;
            }
            let same_packed = match (&packed_in, grid.packed) {
                (PackedRunsInput::None, PackedRunsRef::None) => true,
                (PackedRunsInput::Small(a), PackedRunsRef::Small(b)) => a.as_slice() == b,
                (PackedRunsInput::Wide(a), PackedRunsRef::Wide(b)) => a.as_ref() == b,
                _ => false,
            };
            if !same_packed {
                return false;
            }
            let GridStepsRef::Small(steps) = grid.steps else {
                return false;
            };
            if !steps.is_empty() && u32::from(steps[steps.len() - 1].part_end) > sub_ticks {
                return false;
            }
            let add = match steps.len().checked_mul(k as usize) {
                Some(a) => a,
                None => return false,
            };
            let prev_len = total_steps;
            total_steps = total_steps.saturating_add(add);
            if total_steps > 256 {
                return false;
            }
            if all_steps.is_none() && total_steps > MAX_INLINE_GRID_LEN {
                let mut vec = Vec::with_capacity(total_steps);
                vec.extend_from_slice(&inline_steps[..prev_len]);
                all_steps = Some(vec);
            } else if let Some(vec) = &mut all_steps {
                vec.reserve(add);
            }
            let mut cursor = prev_len;
            for copy in 0..k {
                let offset = (cycle_idx as u32) * ticks_u32 + copy * sub_ticks;
                let offset_u16 = offset as u16;
                for &s in steps {
                    let step = SmallGridStep {
                        part_begin: s.part_begin + offset_u16,
                        part_end: s.part_end + offset_u16,
                        value: s.value,
                        loc: s.loc,
                    };
                    if let Some(vec) = &mut all_steps {
                        vec.push(step);
                    } else {
                        inline_steps[cursor] = step;
                        cursor += 1;
                    }
                }
            }
            true
        })?;
        if !ok {
            return None;
        }
    }
    let grid_steps = match all_steps {
        Some(vec) => pack_small_steps(vec),
        None => GridSteps::Inline(total_steps as u8, inline_steps),
    };
    let listing = Listing::from_parts(ticks, base | Grid::SMALL, grid_steps, packed_in, None);
    node_of_one(first.with(|l| {
        l.set_in_cycles(true);
        l.set_period_and_steps(Fraction::int(n as i128), Steps::Listed(listing));
        l.set_rev_mask(rev_mask);
    }))
}
