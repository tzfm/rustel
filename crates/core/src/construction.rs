//! Structural agreement of two independently built graphs.
//!
//! A native program and the JavaScript callback it stands for build their
//! graphs separately, so the two never share their new nodes. The callback-IR
//! differential mode asks this module whether both builds made the same
//! construction.

use std::sync::Arc;

use crate::{CompactPickLookup, Node, NodeCell, Pattern, PickLookup, Value};

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
    /// Whether this handle and `other` are handles of one node: the same
    /// graph, not an equal one.
    pub fn shares_node(&self, other: &Pattern) -> bool {
        Arc::ptr_eq(&self.node, &other.node)
    }

    /// Whether another handle holds the node of this one, in a graph or as
    /// a value. A node that this handle alone holds is freed with it.
    pub fn is_held_elsewhere(&self) -> bool {
        Arc::strong_count(&self.node) > 1
    }

    /// Reference count of the underlying node cell.
    #[doc(hidden)]
    pub fn node_strong_count(&self) -> usize {
        Arc::strong_count(&self.node)
    }

    /// Address identity of the underlying node cell, for deduplicating handles
    /// within one evaluation pass.
    #[doc(hidden)]
    pub fn node_identity(&self) -> usize {
        Arc::as_ptr(&self.node) as usize
    }

    pub fn same_construction(&self, other: &Pattern) -> bool {
        let mut pending = vec![(self, other)];
        let mut plain: Vec<(&NodeCell, u8, &NodeCell, u8)> = Vec::new();
        let mut budget = MAX_COMPARED_NODES;
        loop {
            let (left_kind, right_kind) = if let Some((left, right)) = pending.pop() {
                if !same_handles(left, right) {
                    return false;
                }
                if Arc::ptr_eq(&left.node, &right.node) {
                    continue;
                }
                (&left.node.kind, &right.node.kind)
            } else if let Some((left, lf, right, rf)) = plain.pop() {
                if lf != rf {
                    return false;
                }
                if std::ptr::eq(left, right) {
                    continue;
                }
                (&left.kind, &right.kind)
            } else {
                break;
            };
            let Some(rest) = budget.checked_sub(1) else {
                return false;
            };
            budget = rest;
            if !same_node(left_kind, right_kind, &mut pending, &mut plain) {
                return false;
            }
        }
        true
    }

    /// Answer tables have been removed in favour of direct graph walks.
    #[doc(hidden)]
    pub fn adopt_tables_from(&self, _previous: &Pattern) {}
}

/// Whether two handles carry the same metadata their nodes cannot express.
fn same_handles(left: &Pattern, right: &Pattern) -> bool {
    left.same_steps(right)
        && left.pure_loc() == right.pure_loc()
        && left.same_purity_flags(right)
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

fn same_compact_lookup<'g>(
    left: &'g CompactPickLookup,
    right: &'g CompactPickLookup,
    pending: &mut Vec<(&'g Pattern, &'g Pattern)>,
) -> bool {
    match (left, right) {
        (CompactPickLookup::Dense(a), CompactPickLookup::Dense(b)) => same_children(a, b, pending),
        (
            CompactPickLookup::Array {
                enumerable_len: left_enumerable,
                length: left_length,
                entries: left_entries,
            },
            CompactPickLookup::Array {
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
            CompactPickLookup::Object {
                enumerable_len: left_enumerable,
                entries: left_entries,
            },
            CompactPickLookup::Object {
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
    plain: &mut Vec<(&'g NodeCell, u8, &'g NodeCell, u8)>,
) -> bool {
    macro_rules! child {
        ($left:expr, $right:expr) => {{
            pending.push(($left, $right));
            true
        }};
    }
    macro_rules! plain_child {
        ($left:expr, $lf:expr, $right:expr, $rf:expr) => {{
            plain.push(($left, *$lf, $right, *$rf));
            true
        }};
    }
    match left {
        Node::Silence => matches!(right, Node::Silence),
        Node::Pure(a) => matches!(right, Node::Pure(b) if same_value(a, b)),
        Node::PureAt(a) => matches!(right, Node::PureAt(b) if a.1 == b.1 && same_value(&a.0, &b.0)),
        Node::IntRange {
            start: as_,
            count: ac,
            loc0: al0,
            loc1: al1,
            named_id: an,
            step_neg: ag,
        } => matches!(
            right,
            Node::IntRange {
                start: bs,
                count: bc,
                loc0: bl0,
                loc1: bl1,
                named_id: bn,
                step_neg: bg,
            } if as_ == bs && ac == bc && al0 == bl0 && al1 == bl1 && an == bn && ag == bg
        ),
        Node::Iter(a) => match right {
            Node::Iter(b) => {
                a.parts == b.parts
                    && a.times == b.times
                    && a.back == b.back
                    && child!(&a.inner, &b.inner)
            }
            _ => false,
        },
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
        Node::WithQueryTime(a) => match right {
            Node::WithQueryTime(b) => child!(&a.0, &b.0),
            _ => false,
        },
        Node::WithHapTime(a) => match right {
            Node::WithHapTime(b) => child!(&a.0, &b.0),
            _ => false,
        },
        Node::Fast(a) => match right {
            Node::Fast(b) if a.1 == b.1 && a.2 == b.2 => child!(&a.0, &b.0),
            _ => false,
        },
        Node::FastSmall(a_cell, a_f, a_n, a_d, a_slow) => match right {
            Node::FastSmall(b_cell, b_f, b_n, b_d, b_slow)
                if a_n == b_n && a_d == b_d && a_slow == b_slow =>
            {
                plain_child!(a_cell, a_f, b_cell, b_f)
            }
            _ => false,
        },
        Node::Shift(a) => match right {
            Node::Shift(b) if a.1 == b.1 && a.2 == b.2 => child!(&a.0, &b.0),
            _ => false,
        },
        Node::ShiftSmall(a_cell, a_f, a_n, a_d, a_early) => match right {
            Node::ShiftSmall(b_cell, b_f, b_n, b_d, b_early)
                if a_n == b_n && a_d == b_d && a_early == b_early =>
            {
                plain_child!(a_cell, a_f, b_cell, b_f)
            }
            _ => false,
        },
        Node::WithQuerySpan(a) => match right {
            Node::WithQuerySpan(b) => child!(&a.0, &b.0),
            _ => false,
        },
        Node::WithQuerySpanJs(a) => match right {
            Node::WithQuerySpanJs(b) if a.1 == b.1 => child!(&a.0, &b.0),
            _ => false,
        },
        Node::WithHapSpan(a) => match right {
            Node::WithHapSpan(b) => child!(&a.0, &b.0),
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
        Node::Arp(a) => match right {
            Node::Arp(b) => {
                pending.push((&a.1, &b.1));
                child!(&a.0, &b.0)
            }
            _ => false,
        },
        Node::Extension(a) => match right {
            Node::Extension(b) => {
                a.kind() == b.kind() && same_children(a.children(), b.children(), pending)
            }
            _ => false,
        },
        Node::ArpWith(a) => match right {
            Node::ArpWith(b) if a.1 == b.1 => child!(&a.0, &b.0),
            _ => false,
        },
        Node::ArpWithPattern(a) => match right {
            Node::ArpWithPattern(b) => {
                pending.push((&a.1, &b.1));
                child!(&a.0, &b.0)
            }
            _ => false,
        },
        Node::PurePattern(a) => match right {
            Node::PurePattern(b) => child!(a, b),
            _ => false,
        },
        Node::PurePickLookup(boxed) => match right {
            Node::PurePickLookup(other_boxed) => {
                same_value(&boxed.0, &other_boxed.0)
                    && same_lookup(&boxed.1, &other_boxed.1, pending)
            }
            _ => false,
        },
        Node::PatternOfJs(a) => match right {
            Node::PatternOfJs(b) if a.1 == b.1 => child!(&a.0, &b.0),
            _ => false,
        },
        Node::FmapNative(a) => match right {
            Node::FmapNative(b) => child!(&a.0, &b.0),
            _ => false,
        },
        Node::FilterValues(a) => match right {
            Node::FilterValues(b) => child!(&a.0, &b.0),
            _ => false,
        },
        Node::FilterValuesJs(a) => match right {
            Node::FilterValuesJs(b) if a.1 == b.1 => child!(&a.0, &b.0),
            _ => false,
        },
        Node::FilterHaps(a) => match right {
            Node::FilterHaps(b) => child!(&a.0, &b.0),
            _ => false,
        },
        Node::FilterHapsJs(a) => match right {
            Node::FilterHapsJs(b) if a.1 == b.1 => child!(&a.0, &b.0),
            _ => false,
        },
        Node::FilterWhenJs(a) => match right {
            Node::FilterWhenJs(b) if a.1 == b.1 => child!(&a.0, &b.0),
            _ => false,
        },
        Node::SortHapsByPart(a) => match right {
            Node::SortHapsByPart(b) => child!(a, b),
            _ => false,
        },
        Node::ControlMap(a, af, id, unnamed) => match right {
            Node::ControlMap(b, bf, other_id, other_unnamed)
                if id == other_id && unnamed == other_unnamed =>
            {
                plain_child!(a, af, b, bf)
            }
            _ => false,
        },
        Node::ControlMapBoxed(a) => match right {
            Node::ControlMapBoxed(b) if a.1 == b.1 && a.2 == b.2 => child!(&a.0, &b.0),
            _ => false,
        },
        Node::MapHapsNative(a) => match right {
            Node::MapHapsNative(b) if a.2 == b.2 => child!(&a.0, &b.0),
            _ => false,
        },
        Node::MapHapsOwned(a) => match right {
            Node::MapHapsOwned(b) if a.2 == b.2 => child!(&a.0, &b.0),
            _ => false,
        },
        Node::UiVisual(a) => match right {
            Node::UiVisual(b) if a.slot == b.slot => child!(&a.inner, &b.inner),
            _ => false,
        },
        Node::MapHapsWithState(a) => match right {
            Node::MapHapsWithState(b) => child!(&a.0, &b.0),
            _ => false,
        },
        Node::ExpandHapsNative(a) => match right {
            Node::ExpandHapsNative(b) if a.2 == b.2 => child!(&a.0, &b.0),
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
        Node::WithRandSeed(a) => match right {
            Node::WithRandSeed(b) if a.1.to_bits() == b.1.to_bits() => child!(&a.0, &b.0),
            _ => false,
        },
        Node::Signal(_) => matches!(right, Node::Signal(_)),
        Node::NumberSignal(a) => {
            matches!(right, Node::NumberSignal(b) if std::ptr::fn_addr_eq(*a, *b))
        }
        Node::SegmentSignal { body, steps } => matches!(
            right,
            Node::SegmentSignal { body: b, steps: s }
                if *steps == *s && std::ptr::fn_addr_eq(*body, *b)
        ),
        Node::ChooseCycles(a) => match right {
            Node::ChooseCycles(b) => a.1 == b.1 && same_children(&a.0, &b.0, pending),
            _ => false,
        },
        Node::WChoose(a) => match right {
            Node::WChoose(b) => {
                pending.push((&a.chooser, &b.chooser));
                pending.push((&a.total, &b.total));
                a.inner == b.inner
                    && same_children(&a.cumulative, &b.cumulative, pending)
                    && same_children(&a.values, &b.values, pending)
            }
            _ => false,
        },
        Node::Pick(a) => match right {
            Node::Pick(b) => {
                pending.push((&a.selector, &b.selector));
                a.index_mode == b.index_mode
                    && a.mode == b.mode
                    && same_compact_lookup(&a.lookup, &b.lookup, pending)
            }
            _ => false,
        },
        Node::PickPatternified {
            patterns,
            index_mode,
            mode,
        } => match right {
            Node::PickPatternified {
                patterns: other,
                index_mode: other_index_mode,
                mode: other_mode,
            } => {
                pending.push((&patterns.0, &other.0));
                pending.push((&patterns.1, &other.1));
                index_mode == other_index_mode && mode == other_mode
            }
            _ => false,
        },
        Node::Degrade(a) => match right {
            Node::Degrade(b) if a.1.to_bits() == b.1.to_bits() && a.2 == b.2 => {
                child!(&a.0, &b.0)
            }
            _ => false,
        },
        Node::Stack(a) => match right {
            Node::Stack(b) => same_children(a, b, pending),
            _ => false,
        },
        Node::StackPure(a) => match right {
            Node::StackPure(b) => {
                if a.len() != b.len() {
                    return false;
                }
                for (l, r) in a.iter().zip(b.iter()) {
                    plain.push((l, 0, r, 0));
                }
                true
            }
            _ => false,
        },
        Node::SlowCat(a) => match right {
            Node::SlowCat(b) => same_children(a, b, pending),
            _ => false,
        },
        Node::SlowCatPure(a) => match right {
            Node::SlowCatPure(b) => {
                if a.len() != b.len() {
                    return false;
                }
                for (l, r) in a.iter().zip(b.iter()) {
                    plain.push((l, 0, r, 0));
                }
                true
            }
            _ => false,
        },
        Node::SlowCatPrime(a) => match right {
            Node::SlowCatPrime(b) => same_children(a, b, pending),
            _ => false,
        },
        Node::SlowCatPrimePure(a) => match right {
            Node::SlowCatPrimePure(b) => {
                if a.len() != b.len() {
                    return false;
                }
                for (l, r) in a.iter().zip(b.iter()) {
                    plain.push((l, 0, r, 0));
                }
                true
            }
            _ => false,
        },
        Node::InnerJoin(a) => match right {
            Node::InnerJoin(b) => child!(&a.0, &b.0),
            _ => false,
        },
        Node::SplitQueries(a, af) => match right {
            Node::SplitQueries(b, bf) => plain_child!(a, af, b, bf),
            _ => false,
        },
        Node::SplitQueriesBoxed(a) => match right {
            Node::SplitQueriesBoxed(b) => child!(a, b),
            _ => false,
        },
        Node::FastGap(a) => match right {
            Node::FastGap(b) if a.1 == b.1 => child!(&a.0, &b.0),
            _ => false,
        },
        Node::RepeatCycles(a) => match right {
            Node::RepeatCycles(b) if a.1 == b.1 => child!(&a.0, &b.0),
            _ => false,
        },
        Node::LoopAt(a) => match right {
            Node::LoopAt(b) if a.1 == b.1 => child!(&a.0, &b.0),
            _ => false,
        },
        Node::Cpm(a) => match right {
            Node::Cpm(b) if a.1.cpm == b.1.cpm => child!(&a.0, &b.0),
            _ => false,
        },
        Node::MidiKeys(a) => match right {
            Node::MidiKeys(b) if Arc::ptr_eq(&a.1, &b.1) => child!(&a.0, &b.0),
            _ => false,
        },
        Node::AppLeft(a) => match right {
            Node::AppLeft(b) if a.4 == b.4 => {
                pending.push((&a.1, &b.1));
                child!(&a.0, &b.0)
            }
            _ => false,
        },
        Node::AppWave(a) => match right {
            Node::AppWave(b)
                if a.waves.len() == b.waves.len()
                    && a.waves.iter().zip(b.waves.iter()).all(|(x, y)| {
                        x.key_id == y.key_id
                            && x.math_inside == y.math_inside
                            && x.steps == y.steps
                            && x.wave.same(&y.wave)
                    }) =>
            {
                child!(&a.inner, &b.inner)
            }
            _ => false,
        },
        Node::Constants(a) => match right {
            Node::Constants(b) if crate::RunMeeting::same(&a.1, &b.1) => child!(&a.0, &b.0),
            _ => false,
        },
        Node::AppRight(a) => match right {
            Node::AppRight(b) if a.3 == b.3 => {
                pending.push((&a.1, &b.1));
                child!(&a.0, &b.0)
            }
            _ => false,
        },
        Node::AppBoth(a) => match right {
            Node::AppBoth(b) if a.3 == b.3 => {
                pending.push((&a.1, &b.1));
                child!(&a.0, &b.0)
            }
            _ => false,
        },
        Node::AddContext(a) => match right {
            Node::AddContext(b) if a.1 == b.1 => child!(&a.0, &b.0),
            _ => false,
        },
        Node::PatternOfPure(a) => match right {
            Node::PatternOfPure(b) => child!(&a.0, &b.0),
            _ => false,
        },
        Node::PatternOfPureOperand(a, _) => match right {
            Node::PatternOfPureOperand(b, _) => child!(&a.0, &b.0) && child!(&a.1, &b.1),
            _ => false,
        },
        Node::PatternOfDynamic(a) => match right {
            Node::PatternOfDynamic(b) => child!(&a.0, &b.0),
            _ => false,
        },
        Node::PatternOfHap(a) => match right {
            Node::PatternOfHap(b) => {
                pending.push((&a.0, &b.0));
                same_children(&a.1, &b.1, pending)
            }
            _ => false,
        },
        Node::XenPatterns(a) => match right {
            Node::XenPatterns(b) => {
                pending.push((&a.0, &b.0));
                same_children(&a.1, &b.1, pending)
            }
            _ => false,
        },
        Node::Join(a, af, mode) => match right {
            Node::Join(b, bf, other) if mode == other => plain_child!(a, af, b, bf),
            _ => false,
        },
        Node::JoinBoxed(a) => match right {
            Node::JoinBoxed(b) if a.1 == b.1 => child!(&a.0, &b.0),
            _ => false,
        },
        Node::FmapJs(a) => match right {
            Node::FmapJs(b) if a.1 == b.1 => child!(&a.0, &b.0),
            _ => false,
        },
        Node::FmapJsPreservingControls(a) => match right {
            Node::FmapJsPreservingControls(b) if a.1 == b.1 && a.2.is_some() == b.2.is_some() => {
                child!(&a.0, &b.0)
            }
            _ => false,
        },
        Node::JsQuery(id) => matches!(right, Node::JsQuery(other) if id == other),
        Node::Timeline(a, state) => match right {
            Node::Timeline(b, other_state) if Arc::ptr_eq(&state.offsets, &other_state.offsets) => {
                pending.push((&a.0, &b.0));
                child!(&a.1, &b.1)
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
}
