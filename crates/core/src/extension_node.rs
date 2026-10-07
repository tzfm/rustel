//! Open execution boundary for statically linked pattern extensions.
//!
//! Core owns the graph's safety machinery: cached purity, runtime ownership,
//! bounded queries, cancellation, and iterative destruction. An extension
//! owns the semantics of each node it contributes. This keeps extension
//! operations out of [`crate::Node`] without turning an extension query into
//! a JavaScript callback.

use crate::{Hap, Pattern, State};

/// One native graph node implemented by a statically linked extension.
///
/// The contract is deliberately small. `children` is the complete set of
/// pattern handles retained by the node: core derives purity and runtime
/// ownership from it. `drain_children` must remove that same set into core's
/// worklist so arbitrarily deep graphs are destroyed iteratively rather than
/// overflowing the Rust stack. An implementation must not hide additional
/// patterns or JavaScript callback identities in other fields.
///
/// `query` runs inside the ordinary core query boundary. Calls to a child
/// pattern therefore inherit the existing depth, hap, cancellation, and
/// deadline accounting. Dispatch happens once per extension node query, not
/// once per hap or audio sample.
pub trait ExtensionPatternNode: Send + Sync {
    /// Stable diagnostic identity, scoped by the extension that owns it.
    fn kind(&self) -> &'static str;

    /// Every child pattern retained by this node.
    fn children(&self) -> &[Pattern];

    /// Evaluate this node for one ordinary core query.
    fn query(&self, state: &State) -> Vec<Hap>;

    /// Whether two identical queries of this node may answer differently
    /// even when every child answers the same: the node keeps memory from
    /// one query to the next, or reads something the query state and the
    /// selected settings do not name. Core never caches a result reached
    /// through a volatile node.
    ///
    /// The default is the conservative one. A node overrides it only once
    /// its `query` provably reads nothing but its children, the state it is
    /// handed, and the settings snapshot selected for the operation.
    fn volatile(&self) -> bool {
        true
    }

    /// Remove every entry exposed by [`Self::children`] into core's drop
    /// worklist.
    fn drain_children(&mut self, out: &mut Vec<Pattern>);
}

/// Append one extension-produced hap through core's live query budget.
///
/// Nodes that only rewrite one child hap in place inherit that child's bound;
/// nodes that multiply or concatenate results must call this before growing
/// their own output vector.
pub fn push_hap(output: &mut Vec<Hap>, hap: Hap) -> bool {
    crate::push_budgeted(output, hap)
}

/// Query a dynamically selected child with ordinary `innerJoin` metadata
/// semantics.
///
/// Extension nodes use this instead of duplicating core's context, tag,
/// scale, lookup, and visual merge rules. `pattern` must already be reachable
/// through [`ExtensionPatternNode::children`].
pub fn query_inner(outer: &Hap, pattern: &Pattern, state: &State) -> Vec<Hap> {
    let mut output = Vec::new();
    for inner in pattern.query(&state.set_span(outer.part)) {
        let mut context = outer.context.clone();
        context.extend_from_slice(&inner.context);
        if !crate::push_budgeted(
            &mut output,
            Hap {
                whole: inner.whole,
                part: inner.part,
                pick_lookup: crate::joined_pick_lookup(outer, &inner),
                scale: inner.scale.clone().or_else(|| outer.scale.clone()),
                tags: inner.tags.clone().or_else(|| outer.tags.clone()),
                log_line: inner.log_line.clone().or_else(|| outer.log_line.clone()),
                edo_size: inner.edo_size.or(outer.edo_size),
                scale_definition: inner
                    .scale_definition
                    .clone()
                    .or_else(|| outer.scale_definition.clone()),
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
    output
}
