//! Structural agreement of two independently built graphs.
//!
//! A native program and the JavaScript callback it stands for build their
//! graphs separately, so the two never share their new nodes. The callback-IR
//! differential mode asks this module whether both builds made the same
//! construction.

use std::sync::Arc;

use crate::{Node, Pattern, PickLookup, Value};

/// The most node pairs one comparison visits.
const MAX_COMPARED_NODES: usize = 1 << 20;

impl Pattern {
    /// Whether `other` comes from the same construction as this graph.
    ///
    /// Two graphs agree when their nodes have the same kinds in the same
    /// shape, equal scalar payloads and agreeing children, and every handle
    /// has the same step count, source location, purity and runtime owner.
    /// The reachable-callback set is the one part of purity left out: a
    /// callback's id is its per-bridge allocation ordinal, so two builds of
    /// one score reach the same callbacks under different ids, and the nodes
    /// that carry an id are compared as nodes already. A node shared by both
    /// sides agrees with itself without a walk. Closures, construction memos
    /// and precomputed tables are not compared. A comparison past
    /// [`MAX_COMPARED_NODES`] node pairs does not agree.
    #[doc(hidden)]
    pub fn same_construction(&self, other: &Pattern) -> bool {
        let mut pending = vec![(self, other)];
        let mut budget = MAX_COMPARED_NODES;
        while let Some((left, right)) = pending.pop() {
            if !same_handles(left, right) {
                return false;
            }
            if Arc::ptr_eq(&left.node, &right.node) {
                continue;
            }
            let Some(rest) = budget.checked_sub(1) else {
                return false;
            };
            budget = rest;
            if !same_node(&left.node.kind, &right.node.kind, &mut pending) {
                return false;
            }
        }
        true
    }

    /// Take the precomputed tables of `previous`, the graph an edit replaced
    /// with this one, wherever both are the same construction and the tables
    /// still answer. An edit rebuilds only what it changed, so the first
    /// query of the new graph pays for the changed parts alone.
    ///
    /// A node's answers embed its descendants' answers, so a pair takes
    /// tables only once every pair below it agreed: the walk refuses a pair
    /// whose subtree changed, and the siblings of a refused pair still
    /// carry. A pair that shares one cell with its counterpart is that
    /// subtree agreed entire: the cell serves both graphs, so the walk
    /// takes nothing and stops there.
    ///
    /// A table answers only while what it was built with still stands, so a
    /// table whose settings snapshot or compiled programs moved on is left
    /// behind. The check runs here, at handover, and again at every query:
    /// a filled table that later stops answering is replaced by the build
    /// of the state in force, never left to dead-end its node.
    #[doc(hidden)]
    pub fn adopt_tables_from(&self, previous: &Pattern) {
        // A graph that walks reads no table, so it takes none.
        if self.purity.walked {
            return;
        }
        /// A pair entered, waiting for the verdicts of its children.
        struct Leave<'a> {
            left: &'a Pattern,
            right: &'a Pattern,
            /// Whether this pair's own shape and payloads agreed.
            own: bool,
            /// How many verdicts of children it takes.
            children: usize,
        }
        enum Step<'a> {
            Enter(&'a Pattern, &'a Pattern),
            Leave(Leave<'a>),
        }
        let mut steps = vec![Step::Enter(self, previous)];
        let mut verdicts: Vec<bool> = Vec::new();
        let mut budget = MAX_COMPARED_NODES;
        while let Some(step) = steps.pop() {
            match step {
                Step::Enter(left, right) => {
                    // Two handles of one cell with different step counts or
                    // purity refuse the pair. The cell serves both graphs
                    // anyway, so nothing below it can move.
                    if !same_handles(left, right) {
                        verdicts.push(false);
                        continue;
                    }
                    if Arc::ptr_eq(&left.node, &right.node) {
                        verdicts.push(true);
                        continue;
                    }
                    let Some(rest) = budget.checked_sub(1) else {
                        return;
                    };
                    budget = rest;
                    let mut children = Vec::new();
                    let shape = same_node(&left.node.kind, &right.node.kind, &mut children);
                    // A pair whose shape refused keeps the children the
                    // shape compare already queued, if it compared that far.
                    let own = shape && same_opaque_payloads(&left.node.kind, &right.node.kind);
                    // The children carry whatever they carry even where this
                    // pair does not: their answers are their own.
                    steps.push(Step::Leave(Leave {
                        left,
                        right,
                        own,
                        children: children.len(),
                    }));
                    steps.extend(
                        children
                            .into_iter()
                            .map(|(left, right)| Step::Enter(left, right)),
                    );
                }
                Step::Leave(Leave {
                    left,
                    right,
                    own,
                    children,
                }) => {
                    let split = verdicts
                        .len()
                        .checked_sub(children)
                        .expect("one verdict per child");
                    let agreed = own && verdicts.drain(split..).all(|agreed| agreed);
                    verdicts.push(agreed);
                    if !agreed {
                        continue;
                    }
                    let handover = right.node.tables.read().ok().and_then(|slot| match &*slot {
                        crate::table::TableSlot::Ready(tables) if tables.current() => {
                            Some(Arc::clone(tables))
                        }
                        _ => None,
                    });
                    if left.tables_may_answer()
                        && let Some(tables) = handover
                        // A build may have taken the slot while this walk
                        // reached it; the build's verdict wins.
                        && let Ok(mut slot) = left.node.tables.write()
                        && matches!(&*slot, crate::table::TableSlot::Empty)
                    {
                        *slot = crate::table::TableSlot::Ready(tables);
                    }
                }
            }
        }
    }
}

/// Whether the payloads [`same_node`] leaves out cannot tell this pair
/// apart. Only a pair `same_node` accepted reaches here, so `right` is the
/// same kind as `left`.
///
/// The differential mode may leave them out: two builds of one score make
/// equal payloads, so the node shape already settles agreement there. An
/// edit makes different payloads under a shape it did not change, and the
/// answers of the old payloads are not this node's. Such a pair refuses,
/// and a kind whose payloads `same_node` leaves out must be named here or
/// its edits never carry: the match names every kind, so a new one refuses
/// to compile until it is placed.
fn same_opaque_payloads(left: &Node, right: &Node) -> bool {
    use Node::*;
    match left {
        WithQueryTime(_, f) => matches!(right, WithQueryTime(_, g) if Arc::ptr_eq(f, g)),
        WithHapTime(_, f) => matches!(right, WithHapTime(_, g) if Arc::ptr_eq(f, g)),
        WithQuerySpan(_, f) => matches!(right, WithQuerySpan(_, g) if Arc::ptr_eq(f, g)),
        WithHapSpan(_, f) => matches!(right, WithHapSpan(_, g) if Arc::ptr_eq(f, g)),
        FmapNative(_, f) => matches!(right, FmapNative(_, g) if Arc::ptr_eq(f, g)),
        FilterValues(_, f) => matches!(right, FilterValues(_, g) if Arc::ptr_eq(f, g)),
        FilterHaps(_, f) => matches!(right, FilterHaps(_, g) if Arc::ptr_eq(f, g)),
        MapHapsNative(_, f, _) => matches!(right, MapHapsNative(_, g, _) if Arc::ptr_eq(f, g)),
        MapHapsOwned(_, f, _) => matches!(right, MapHapsOwned(_, g, _) if Arc::ptr_eq(f, g)),
        MapHapsWithState(_, f) => matches!(right, MapHapsWithState(_, g) if Arc::ptr_eq(f, g)),
        ExpandHapsNative(_, f, _) => {
            matches!(right, ExpandHapsNative(_, g, _) if Arc::ptr_eq(f, g))
        }
        Signal(f) => matches!(right, Signal(g) if Arc::ptr_eq(f, g)),
        InnerJoin(_, f) => matches!(right, InnerJoin(_, g) if Arc::ptr_eq(f, g)),
        PatternOfDynamic(_, f) => matches!(right, PatternOfDynamic(_, g) if Arc::ptr_eq(f, g)),
        PatternOfPure(_, f, _) => matches!(right, PatternOfPure(_, g, _) if Arc::ptr_eq(f, g)),
        PatternOfPureOperand(_, _, f, _) => {
            matches!(right, PatternOfPureOperand(_, _, g, _) if f.same(g))
        }
        PatternOfHap(_, _, f) => matches!(right, PatternOfHap(_, _, g) if Arc::ptr_eq(f, g)),
        AppLeft(_, _, f, _, _) => matches!(right, AppLeft(_, _, g, _, _) if same_combine(f, g)),
        AppRight(_, _, f, _) => matches!(right, AppRight(_, _, g, _) if same_combine(f, g)),
        AppBoth(_, _, f, _) => matches!(right, AppBoth(_, _, g, _) if same_combine(f, g)),
        FmapJsPreservingControls(_, _, f) => match right {
            FmapJsPreservingControls(_, _, g) => match (f, g) {
                (Some(f), Some(g)) => std::ptr::fn_addr_eq(*f, *g),
                (None, None) => true,
                _ => false,
            },
            _ => false,
        },
        Extension(f) => matches!(right, Extension(g) if std::ptr::eq(&**f, &**g)),
        #[cfg(non_send_node_mutation)]
        NonSendMutation(_) => false,
        // Every kind left carries its whole payload in the fields
        // `same_node` compares: the shape settles it, and the pair carries.
        Silence
        | Pure(_)
        | Sequence(_)
        | QueryError(_)
        | QueryLimit(_)
        | Fast(..)
        | Shift(..)
        | WithQuerySpanJs(..)
        | Rev(_)
        | StepJoin(_)
        | PolyJoin(_)
        | Arp(..)
        | ArpWith(..)
        | ArpWithPattern(..)
        | PurePattern(_)
        | PurePickLookup(..)
        | PatternOfJs(..)
        | FilterValuesJs(..)
        | FilterHapsJs(..)
        | FilterWhenJs(..)
        | SortHapsByPart(_)
        | Collect(_)
        | CycleOnsetIndex(_)
        | CycleOnsetCount(_)
        | WithRandSeed(..)
        | ChooseCycles(..)
        | WChoose { .. }
        | Pick { .. }
        | PickPatternified { .. }
        | Degrade(..)
        | Stack(_)
        | SlowCat(_)
        | SlowCatPrime(_)
        | SplitQueries(_)
        | FastGap(..)
        | RepeatCycles(..)
        | LoopAt(..)
        | Cpm(..)
        | MidiKeys(..)
        | AddContext(..)
        | XenPatterns(..)
        | Join(..)
        | FmapJs(..)
        | JsQuery(_)
        | Timeline(..) => true,
    }
}

/// Whether two apply functions of a meeting are the same one.
fn same_combine(left: &crate::Combine, right: &crate::Combine) -> bool {
    match (left, right) {
        (crate::Combine::Composed(left), crate::Combine::Composed(right)) => left == right,
        (crate::Combine::With(left), crate::Combine::With(right)) => Arc::ptr_eq(left, right),
        _ => false,
    }
}

/// Whether two handles carry the same metadata their nodes cannot express.
fn same_handles(left: &Pattern, right: &Pattern) -> bool {
    left.steps == right.steps
        && left.pure_loc == right.pure_loc
        && left.purity.impure == right.purity.impure
        && left.purity.opaque == right.purity.opaque
        && left.purity.volatile == right.purity.volatile
        && left.purity.host_voicings == right.purity.host_voicings
        && left.same_runtime_owner(right)
}

/// Whether two values are equal down to the bits of every number.
fn same_value(left: &Value, right: &Value) -> bool {
    crate::same_key(left, right)
}

fn same_children<'g>(
    left: &'g [Pattern],
    right: &'g [Pattern],
    pending: &mut Vec<(&'g Pattern, &'g Pattern)>,
) -> bool {
    if left.len() != right.len() {
        return false;
    }
    pending.extend(left.iter().zip(right));
    true
}

fn same_lookup<'g>(
    left: &'g PickLookup,
    right: &'g PickLookup,
    pending: &mut Vec<(&'g Pattern, &'g Pattern)>,
) -> bool {
    match (left, right) {
        (
            PickLookup::Array {
                enumerable_len: left_enumerable,
                length: left_length,
                entries: left_entries,
            },
            PickLookup::Array {
                enumerable_len: right_enumerable,
                length: right_length,
                entries: right_entries,
            },
        ) => {
            left_enumerable == right_enumerable
                && left_length == right_length
                && left_entries.len() == right_entries.len()
                && left_entries.iter().zip(right_entries).all(
                    |((left_index, left), (right_index, right))| {
                        pending.push((left, right));
                        left_index == right_index
                    },
                )
        }
        (
            PickLookup::Object {
                enumerable_len: left_enumerable,
                entries: left_entries,
            },
            PickLookup::Object {
                enumerable_len: right_enumerable,
                entries: right_entries,
            },
        ) => {
            left_enumerable == right_enumerable
                && left_entries.len() == right_entries.len()
                && left_entries.iter().zip(right_entries).all(
                    |((left_key, left), (right_key, right))| {
                        pending.push((left, right));
                        left_key == right_key
                    },
                )
        }
        _ => false,
    }
}

/// Compare one node pair and queue its children.
///
/// The match names every left variant with no wildcard, so a new node kind
/// does not compile until it says what agreement means for it.
fn same_node<'g>(
    left: &'g Node,
    right: &'g Node,
    pending: &mut Vec<(&'g Pattern, &'g Pattern)>,
) -> bool {
    macro_rules! child {
        ($left:expr, $right:expr) => {{
            pending.push(($left, $right));
            true
        }};
    }
    match left {
        Node::Silence => matches!(right, Node::Silence),
        Node::Pure(a) => matches!(right, Node::Pure(b) if same_value(a, b)),
        Node::Sequence(a) => match right {
            Node::Sequence(b) => {
                a.len() == b.len() && a.iter().zip(b.iter()).all(|(a, b)| a.same(b, same_value))
            }
            _ => false,
        },
        #[cfg(non_send_node_mutation)]
        Node::NonSendMutation(_) => false,
        Node::QueryError(a) => matches!(right, Node::QueryError(b) if a == b),
        Node::QueryLimit(a) => matches!(right, Node::QueryLimit(b) if a == b),
        Node::WithQueryTime(a, _) => match right {
            Node::WithQueryTime(b, _) => child!(a, b),
            _ => false,
        },
        Node::WithHapTime(a, _) => match right {
            Node::WithHapTime(b, _) => child!(a, b),
            _ => false,
        },
        Node::Fast(a, factor, operation) => match right {
            Node::Fast(b, other_factor, other_operation)
                if factor == other_factor && operation == other_operation =>
            {
                child!(a, b)
            }
            _ => false,
        },
        Node::Shift(a, offset, operation) => match right {
            Node::Shift(b, other_offset, other_operation)
                if offset == other_offset && operation == other_operation =>
            {
                child!(a, b)
            }
            _ => false,
        },
        Node::WithQuerySpan(a, _) => match right {
            Node::WithQuerySpan(b, _) => child!(a, b),
            _ => false,
        },
        Node::WithQuerySpanJs(a, id) => match right {
            Node::WithQuerySpanJs(b, other) if id == other => child!(a, b),
            _ => false,
        },
        Node::WithHapSpan(a, _) => match right {
            Node::WithHapSpan(b, _) => child!(a, b),
            _ => false,
        },
        Node::Rev(a) => match right {
            Node::Rev(b) => child!(a, b),
            _ => false,
        },
        Node::StepJoin(a) => match right {
            Node::StepJoin(b) => child!(a, b),
            _ => false,
        },
        Node::PolyJoin(a) => match right {
            Node::PolyJoin(b) => child!(a, b),
            _ => false,
        },
        Node::Arp(a, indices) => match right {
            Node::Arp(b, other) => {
                pending.push((indices, other));
                child!(a, b)
            }
            _ => false,
        },
        Node::Extension(a) => match right {
            Node::Extension(b) => {
                a.kind() == b.kind() && same_children(a.children(), b.children(), pending)
            }
            _ => false,
        },
        Node::ArpWith(a, id) => match right {
            Node::ArpWith(b, other) if id == other => child!(a, b),
            _ => false,
        },
        Node::ArpWithPattern(a, callbacks) => match right {
            Node::ArpWithPattern(b, other) => {
                pending.push((callbacks, other));
                child!(a, b)
            }
            _ => false,
        },
        Node::PurePattern(a) => match right {
            Node::PurePattern(b) => child!(a, b),
            _ => false,
        },
        Node::PurePickLookup(value, lookup) => match right {
            Node::PurePickLookup(other_value, other_lookup) => {
                same_value(value, other_value) && same_lookup(lookup, other_lookup, pending)
            }
            _ => false,
        },
        Node::PatternOfJs(a, id) => match right {
            Node::PatternOfJs(b, other) if id == other => child!(a, b),
            _ => false,
        },
        Node::FmapNative(a, _) => match right {
            Node::FmapNative(b, _) => child!(a, b),
            _ => false,
        },
        Node::FilterValues(a, _) => match right {
            Node::FilterValues(b, _) => child!(a, b),
            _ => false,
        },
        Node::FilterValuesJs(a, id) => match right {
            Node::FilterValuesJs(b, other) if id == other => child!(a, b),
            _ => false,
        },
        Node::FilterHaps(a, _) => match right {
            Node::FilterHaps(b, _) => child!(a, b),
            _ => false,
        },
        Node::FilterHapsJs(a, id) => match right {
            Node::FilterHapsJs(b, other) if id == other => child!(a, b),
            _ => false,
        },
        Node::FilterWhenJs(a, id) => match right {
            Node::FilterWhenJs(b, other) if id == other => child!(a, b),
            _ => false,
        },
        Node::SortHapsByPart(a) => match right {
            Node::SortHapsByPart(b) => child!(a, b),
            _ => false,
        },
        Node::MapHapsNative(a, _, spans) => match right {
            Node::MapHapsNative(b, _, other) if spans == other => child!(a, b),
            _ => false,
        },
        Node::MapHapsOwned(a, _, spans) => match right {
            Node::MapHapsOwned(b, _, other) if spans == other => child!(a, b),
            _ => false,
        },
        Node::MapHapsWithState(a, _) => match right {
            Node::MapHapsWithState(b, _) => child!(a, b),
            _ => false,
        },
        Node::ExpandHapsNative(a, _, spans) => match right {
            Node::ExpandHapsNative(b, _, other) if spans == other => child!(a, b),
            _ => false,
        },
        Node::Collect(a) => match right {
            Node::Collect(b) => child!(a, b),
            _ => false,
        },
        Node::CycleOnsetIndex(a) => match right {
            Node::CycleOnsetIndex(b) => child!(a, b),
            _ => false,
        },
        Node::CycleOnsetCount(a) => match right {
            Node::CycleOnsetCount(b) => child!(a, b),
            _ => false,
        },
        Node::WithRandSeed(a, seed) => match right {
            Node::WithRandSeed(b, other) if seed.to_bits() == other.to_bits() => child!(a, b),
            _ => false,
        },
        Node::Signal(_) => matches!(right, Node::Signal(_)),
        Node::ChooseCycles(a, seed) => match right {
            Node::ChooseCycles(b, other) => seed == other && same_children(a, b, pending),
            _ => false,
        },
        Node::WChoose {
            chooser,
            total,
            cumulative,
            values,
            inner,
        } => match right {
            Node::WChoose {
                chooser: other_chooser,
                total: other_total,
                cumulative: other_cumulative,
                values: other_values,
                inner: other_inner,
            } => {
                pending.push((chooser, other_chooser));
                pending.push((total, other_total));
                inner == other_inner
                    && same_children(cumulative, other_cumulative, pending)
                    && same_children(values, other_values, pending)
            }
            _ => false,
        },
        Node::Pick {
            selector,
            lookup,
            index_mode,
            mode,
        } => match right {
            Node::Pick {
                selector: other_selector,
                lookup: other_lookup,
                index_mode: other_index_mode,
                mode: other_mode,
            } => {
                pending.push((selector, other_selector));
                index_mode == other_index_mode
                    && mode == other_mode
                    && same_lookup(lookup, other_lookup, pending)
            }
            _ => false,
        },
        Node::PickPatternified {
            selector,
            lookup,
            index_mode,
            mode,
        } => match right {
            Node::PickPatternified {
                selector: other_selector,
                lookup: other_lookup,
                index_mode: other_index_mode,
                mode: other_mode,
            } => {
                pending.push((selector, other_selector));
                pending.push((lookup, other_lookup));
                index_mode == other_index_mode && mode == other_mode
            }
            _ => false,
        },
        Node::Degrade(a, amount, seed) => match right {
            Node::Degrade(b, other_amount, other_seed)
                if amount.to_bits() == other_amount.to_bits() && seed == other_seed =>
            {
                child!(a, b)
            }
            _ => false,
        },
        Node::Stack(a) => match right {
            Node::Stack(b) => same_children(a, b, pending),
            _ => false,
        },
        Node::SlowCat(a) => match right {
            Node::SlowCat(b) => same_children(a, b, pending),
            _ => false,
        },
        Node::SlowCatPrime(a) => match right {
            Node::SlowCatPrime(b) => same_children(a, b, pending),
            _ => false,
        },
        Node::InnerJoin(a, _) => match right {
            Node::InnerJoin(b, _) => child!(a, b),
            _ => false,
        },
        Node::SplitQueries(a) => match right {
            Node::SplitQueries(b) => child!(a, b),
            _ => false,
        },
        Node::FastGap(a, factor) => match right {
            Node::FastGap(b, other) if factor == other => child!(a, b),
            _ => false,
        },
        Node::RepeatCycles(a, count) => match right {
            Node::RepeatCycles(b, other) if count == other => child!(a, b),
            _ => false,
        },
        Node::LoopAt(a, factor) => match right {
            Node::LoopAt(b, other) if factor == other => child!(a, b),
            _ => false,
        },
        Node::Cpm(a, rate) => match right {
            Node::Cpm(b, other) if rate.cpm == other.cpm => child!(a, b),
            _ => false,
        },
        Node::MidiKeys(a, port) => match right {
            Node::MidiKeys(b, other) if Arc::ptr_eq(port, other) => child!(a, b),
            _ => false,
        },
        Node::AppLeft(a, values, _, flow, _) => match right {
            Node::AppLeft(b, other_values, _, other_flow, _) if flow == other_flow => {
                pending.push((values, other_values));
                child!(a, b)
            }
            _ => false,
        },
        Node::AppRight(a, values, _, flow) => match right {
            Node::AppRight(b, other_values, _, other_flow) if flow == other_flow => {
                pending.push((values, other_values));
                child!(a, b)
            }
            _ => false,
        },
        Node::AppBoth(a, values, _, flow) => match right {
            Node::AppBoth(b, other_values, _, other_flow) if flow == other_flow => {
                pending.push((values, other_values));
                child!(a, b)
            }
            _ => false,
        },
        Node::AddContext(a, locations) => match right {
            Node::AddContext(b, other) if locations == other => child!(a, b),
            _ => false,
        },
        Node::PatternOfPure(a, _, _) => match right {
            Node::PatternOfPure(b, _, _) => child!(a, b),
            _ => false,
        },
        Node::PatternOfPureOperand(a, operand, _, _) => match right {
            Node::PatternOfPureOperand(b, other_operand, _, _) => {
                child!(a, b) && child!(operand, other_operand)
            }
            _ => false,
        },
        Node::PatternOfDynamic(a, _) => match right {
            Node::PatternOfDynamic(b, _) => child!(a, b),
            _ => false,
        },
        Node::PatternOfHap(a, dependencies, _) => match right {
            Node::PatternOfHap(b, other, _) => {
                pending.push((a, b));
                same_children(dependencies, other, pending)
            }
            _ => false,
        },
        Node::XenPatterns(a, ratios) => match right {
            Node::XenPatterns(b, other) => {
                pending.push((a, b));
                same_children(ratios, other, pending)
            }
            _ => false,
        },
        Node::Join(a, mode) => match right {
            Node::Join(b, other) if mode == other => child!(a, b),
            _ => false,
        },
        Node::FmapJs(a, id) => match right {
            Node::FmapJs(b, other) if id == other => child!(a, b),
            _ => false,
        },
        Node::FmapJsPreservingControls(a, id, preserved) => match right {
            Node::FmapJsPreservingControls(b, other, other_preserved)
                if id == other && preserved.is_some() == other_preserved.is_some() =>
            {
                child!(a, b)
            }
            _ => false,
        },
        Node::JsQuery(id) => matches!(right, Node::JsQuery(other) if id == other),
        Node::Timeline(time, a, state) => match right {
            Node::Timeline(other_time, b, other_state)
                if Arc::ptr_eq(&state.offsets, &other_state.offsets) =>
            {
                pending.push((time, other_time));
                child!(a, b)
            }
            _ => false,
        },
    }
}

#[cfg(test)]
mod tests {
    use crate::{Pattern, Value, fastcat, pure};
    use rustel_fraction::Fraction;

    fn build(factor: i128, value: f64) -> Pattern {
        fastcat(vec![pure(Value::F64(value)), pure(Value::Str("bd".into()))])
            .fast(Fraction::int(factor))
            .rev()
            .with_added_context(vec![(3, 5)])
    }

    #[test]
    fn independent_builds_of_one_construction_agree() {
        assert!(build(2, 1.0).same_construction(&build(2, 1.0)));
        let shared = build(2, 1.0);
        assert!(shared.same_construction(&shared.clone()));
    }

    #[test]
    fn a_changed_scalar_context_or_step_count_disagrees() {
        let reference = build(2, 1.0);
        assert!(!reference.same_construction(&build(3, 1.0)));
        assert!(!reference.same_construction(&build(2, 2.0)));
        assert!(!build(2, 0.0).same_construction(&build(2, -0.0)));
        let moved = fastcat(vec![pure(Value::F64(1.0)), pure(Value::Str("bd".into()))])
            .fast(Fraction::int(2))
            .rev()
            .with_added_context(vec![(3, 6)]);
        assert!(!reference.same_construction(&moved));
        let stepped = build(2, 1.0).with_steps(Some(Fraction::int(7)));
        assert!(!reference.same_construction(&stepped));
    }

    /// The numbers of the values of the haps of one cycle, in query order.
    fn numbers(pattern: &Pattern) -> Vec<f64> {
        pattern
            .query_arc(Fraction::int(0), Fraction::int(1))
            .iter()
            .map(|hap| match &hap.value {
                Value::F64(number) => *number,
                other => panic!("a number, not {other:?}"),
            })
            .collect()
    }

    /// A query of the pattern that made its tables, and a second one that
    /// reads them.
    fn queried_twice(pattern: &Pattern) {
        numbers(pattern);
        numbers(pattern);
    }

    #[test]
    fn a_graph_that_walks_takes_no_table() {
        let build = || fastcat(vec![pure(Value::F64(1.0)), pure(Value::F64(2.0))]).rev();
        let one = build();
        queried_twice(&one);
        assert!(one.built_tables().is_some());
        let (walked, tabled) = (build().mark_walked(), build());
        walked.adopt_tables_from(&one);
        assert!(walked.built_tables().is_none());
        tabled.adopt_tables_from(&one);
        assert!(tabled.built_tables().is_some());
    }

    #[test]
    fn an_edit_that_changes_a_closure_carries_no_tables() {
        let plus_one = |value: &Value| match value {
            Value::F64(number) => Value::F64(number + 1.0),
            other => other.clone(),
        };
        let plus_two = |value: &Value| match value {
            Value::F64(number) => Value::F64(number + 2.0),
            other => other.clone(),
        };
        let one = pure(Value::F64(1.0)).fmap(plus_one);
        queried_twice(&one);
        // The shape is the same; the closure is not.
        let two = pure(Value::F64(1.0)).fmap(plus_two);
        two.adopt_tables_from(&one);
        // The first query answers from the graph either way; the second is
        // the one a handover of the old answers would corrupt.
        assert_eq!(numbers(&two), [3.0]);
        assert_eq!(numbers(&two), [3.0]);
    }

    #[test]
    fn an_edit_below_a_shape_that_did_not_change_carries_nothing_above_it() {
        let plus_one = |value: &Value| match value {
            Value::F64(number) => Value::F64(number + 1.0),
            other => other.clone(),
        };
        let plus_two = |value: &Value| match value {
            Value::F64(number) => Value::F64(number + 2.0),
            other => other.clone(),
        };
        let one = crate::stack(vec![
            pure(Value::F64(1.0)).fmap(plus_one),
            pure(Value::F64(5.0)),
        ]);
        queried_twice(&one);
        // The stack kept its shape and one child kept its answers; the other
        // child's closure changed, so the stack's own tables, which embed
        // that child's answers, are not its to take.
        let two = crate::stack(vec![
            pure(Value::F64(1.0)).fmap(plus_two),
            pure(Value::F64(5.0)),
        ]);
        two.adopt_tables_from(&one);
        let mut found = numbers(&two);
        found.sort_by(|left, right| left.total_cmp(right));
        assert_eq!(found, [3.0, 5.0]);
        let mut found = numbers(&two);
        found.sort_by(|left, right| left.total_cmp(right));
        assert_eq!(found, [3.0, 5.0]);
    }
}
