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

    /// Answer tables have been removed in favour of direct graph walks.
    #[doc(hidden)]
    pub fn adopt_tables_from(&self, _previous: &Pattern) {}
}

/// Whether two apply functions of a meeting are the same one.
pub(crate) fn same_combine(left: &crate::Combine, right: &crate::Combine) -> bool {
    match (left, right) {
        (crate::Combine::Composed(left), crate::Combine::Composed(right)) => left == right,
        // The same controls in the same order, down to the bits of each
        // number.
        (crate::Combine::Sets(left), crate::Combine::Sets(right)) => {
            left.len() == right.len() && left.iter().zip(&**right).all(|(a, b)| a.same_data(b))
        }
        (crate::Combine::With(left), crate::Combine::With(right)) => Arc::ptr_eq(left, right),
        _ => false,
    }
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
        Node::ArpWith(a, id) => match right {
            Node::ArpWith(b, other) if id == other => child!(a, b),
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
        Node::UiVisual(a, slot, _) => match right {
            Node::UiVisual(b, other, _) if slot == other => child!(a, b),
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
        Node::AppLeft(a, _, flow) => match right {
            Node::AppLeft(b, _, other_flow) if flow == other_flow => {
                pending.push((&a.1, &b.1));
                child!(&a.0, &b.0)
            }
            _ => false,
        },
        Node::Constants(a, run) => match right {
            Node::Constants(b, other) if crate::RunMeeting::same(run, other) => child!(a, b),
            _ => false,
        },
        Node::AppRight(a, _, flow) => match right {
            Node::AppRight(b, _, other_flow) if flow == other_flow => {
                pending.push((&a.1, &b.1));
                child!(&a.0, &b.0)
            }
            _ => false,
        },
        Node::AppBoth(a, _, flow) => match right {
            Node::AppBoth(b, _, other_flow) if flow == other_flow => {
                pending.push((&a.1, &b.1));
                child!(&a.0, &b.0)
            }
            _ => false,
        },
        Node::AddContext(a, locations) => match right {
            Node::AddContext(b, other) if locations == other => child!(a, b),
            _ => false,
        },
        Node::PatternOfPure(a, _) => match right {
            Node::PatternOfPure(b, _) => child!(a, b),
            _ => false,
        },
        Node::PatternOfPureOperand(a, _) => match right {
            Node::PatternOfPureOperand(b, _) => child!(&a.0, &b.0) && child!(&a.1, &b.1),
            _ => false,
        },
        Node::PatternOfDynamic(a, _) => match right {
            Node::PatternOfDynamic(b, _) => child!(a, b),
            _ => false,
        },
        Node::PatternOfHap(a) => match right {
            Node::PatternOfHap(b) => {
                pending.push((&a.0, &b.0));
                same_children(&a.1, &b.1, pending)
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
