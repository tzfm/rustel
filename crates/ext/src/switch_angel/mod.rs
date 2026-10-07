//! Native Rust adaptations of Switch Angel's pinned `prebake.strudel`,
//! granted for use under AGPL-3.0-or-later.
//!
//! Each shipped function owns its implementation, registration, and reference
//! metadata in one file. This module contains only collection-wide identity,
//! assembly, and invariants.

/// One Switch Angel name's reference entry.
///
/// The params group and at least one example are required: an invocation
/// without them does not compile. An entry that has neither is not verified.
macro_rules! simple_reference {
    (
        $name:literal,
        $summary:literal,
        $description:literal,
        params: [$($param:expr),* $(,)?],
        examples: [$first:expr $(, $rest:expr)* $(,)?],
        $($tag:literal),+ $(,)?
    ) => {
        rustel_core::reference::ReferenceEntry {
            name: $name,
            synonyms: &[],
            summary: $summary,
            description: $description,
            params: &[$($param),*],
            examples: &[$first $(, $rest)*],
            tags: &["switch angel", $($tag),+],
            no_autocomplete: false,
            deprecated: false,
            origin: "switch angel",
        }
    };
}

mod accent;
mod acid;
mod acidenv;
mod block_arrange;
mod catalog;
mod chrd;
mod col;
mod colorparty;
mod cue;
mod dly;
mod dx;
mod fill;
mod filtval;
mod flood;
mod fmtime;
mod glide;
mod glitch;
mod grab;
mod humanize;
mod ifit;
mod irando;
mod max;
mod min;
mod mpan;
mod noisehat;
mod notearp;
mod operand;
mod over;
mod overin;
mod p;
mod pg;
mod pk;
mod randm;
mod relative_filter;
mod roller;
mod roller2;
mod sb;
mod scale;
mod sf;
mod shared;
mod sq;
mod strum;
mod stxt;
mod swap;
mod tgate;
mod to_major_key;
mod track;
mod trancearp;
mod trancegate;
mod up;
mod vstruct;
mod zap;

use rustel_core::reference::ReferenceEntry;
use rustel_core::register::{Origin, Registry};

use crate::{Extension, PatternCallable, PatternState, ValueCallable};

pub use acidenv::merge as acidenv_merge;
pub use catalog::{
    OMITTED_CALLABLES, OMITTED_GLOBAL_SIDE_EFFECTS, OmittedCallable, REVIEWED_CALLABLES,
};
pub(crate) use fill::apply as fill_pattern;
pub(crate) use filtval::strictly_equal;
pub(crate) use operand::NativeExtensionOperand;
pub use up::merge as up_merge;

/// Switch Angel's `strudel-scripts/prebake.strudel` collection.
pub const ORIGIN: Origin = Origin::new("switch angel");

/// Exact authored source used for the compatibility inventory.
pub const PREBAKE_SOURCE_URL: &str =
    "https://github.com/switchangel/strudel-scripts/blob/main/prebake.strudel";

/// Git blob of [`PREBAKE_SOURCE_URL`] last reviewed for this extension.
pub const PREBAKE_SOURCE_BLOB: &str = "e85abf952bf718d2ee8bb4e78a074bede21ea328";

const SURFACE_REFERENCES: &[ReferenceEntry] = &[
    acidenv::REFERENCE,
    fill::REFERENCE,
    filtval::REFERENCE,
    grab::REFERENCE,
    strum::REFERENCE,
    up::REFERENCE,
];

const PREBAKE_REFERENCES: &[ReferenceEntry] = &[
    track::REFERENCE,
    block_arrange::REFERENCE,
    pg::REFERENCE,
    accent::REFERENCE,
    trancegate::REFERENCE,
    tgate::REFERENCE,
    dly::REFERENCE,
    colorparty::REFERENCE,
    mpan::REFERENCE,
    relative_filter::LOWPASS_REFERENCE,
    relative_filter::HIGHPASS_REFERENCE,
    vstruct::REFERENCE,
    fmtime::REFERENCE,
    acid::REFERENCE,
    stxt::REFERENCE,
    notearp::REFERENCE,
    swap::REFERENCE,
    sb::REFERENCE,
    ifit::REFERENCE,
    flood::REFERENCE,
    sq::REFERENCE,
    glitch::REFERENCE,
    humanize::REFERENCE,
    glide::REFERENCE,
    dx::REFERENCE,
    col::REFERENCE,
    cue::CUE_REFERENCE,
    cue::GET_REFERENCE,
    cue::SET_REFERENCE,
    cue::ONCUE_REFERENCE,
    p::REFERENCE,
    irando::REFERENCE,
    randm::REFERENCE,
    pk::REFERENCE,
    chrd::REFERENCE,
    scale::SET_REFERENCE,
    scale::SC_REFERENCE,
    over::REFERENCE,
    overin::REFERENCE,
    min::REFERENCE,
    max::REFERENCE,
    sf::REFERENCE,
    scale::NSC_REFERENCE,
    trancearp::REFERENCE,
    noisehat::REFERENCE,
    zap::REFERENCE,
    roller::REFERENCE,
    roller2::REFERENCE,
    to_major_key::REFERENCE,
];

const PATTERN_STATES: &[PatternState] = &[cue::STATE, scale::STATE];

const PATTERN_CALLABLES: &[PatternCallable] = &[
    track::CALLABLE,
    block_arrange::CALLABLE,
    dx::CALLABLE,
    col::CALLABLE,
    cue::CALLABLES[0],
    cue::CALLABLES[1],
    cue::CALLABLES[2],
    cue::CALLABLES[3],
    cue::CALLABLES[4],
    p::CALLABLE,
    irando::CALLABLE,
    randm::CALLABLE,
    pk::CALLABLE,
    chrd::CALLABLE,
    scale::CALLABLES[0],
    scale::CALLABLES[1],
    over::CALLABLE,
    overin::CALLABLE,
    min::CALLABLE,
    max::CALLABLE,
    sf::CALLABLE,
    scale::CALLABLES[2],
    trancearp::CALLABLE,
    noisehat::CALLABLE,
    zap::CALLABLE,
    roller::CALLABLE,
    roller2::CALLABLE,
];

const VALUE_CALLABLES: &[ValueCallable] = &[to_major_key::CALLABLE];

pub(crate) const EXTENSION: Extension = Extension {
    origin: ORIGIN,
    install_patterns: install,
    pattern_states: PATTERN_STATES,
    pattern_callables: PATTERN_CALLABLES,
    value_callables: VALUE_CALLABLES,
    reference_groups: &[SURFACE_REFERENCES, PREBAKE_REFERENCES],
};

fn install(registry: &mut Registry) {
    pg::install(registry);
    accent::install(registry);
    trancegate::install(registry);
    tgate::install(registry);
    dly::install(registry);
    colorparty::install(registry);
    mpan::install(registry);
    relative_filter::install(registry);
    vstruct::install(registry);
    fmtime::install(registry);
    acid::install(registry);
    stxt::install(registry);
    notearp::install(registry);
    swap::install(registry);
    sb::install(registry);
    ifit::install(registry);
    flood::install(registry);
    sq::install(registry);
    glitch::install(registry);
    humanize::install(registry);
    glide::install(registry);
    fill::install(registry);
    grab::install(registry);
    filtval::install(registry);
    up::install(registry);
    acidenv::install(registry);
    strum::install(registry);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::default_registry;
    use rustel_core::Value;
    use rustel_core::register::{DeclaredIn, add_in};
    use std::collections::{BTreeSet, HashMap};

    #[test]
    fn every_callable_in_the_pinned_prebake_is_implemented_or_explained() {
        let registry = default_registry();
        let mut implemented = registry
            .names()
            .into_iter()
            .filter(|name| {
                registry
                    .get(name)
                    .is_some_and(|entry| entry.declared_in.origin() == Some(ORIGIN))
            })
            .collect::<BTreeSet<_>>();
        for callable in PATTERN_CALLABLES {
            implemented.extend(callable.names.iter().copied());
        }
        for callable in VALUE_CALLABLES {
            implemented.extend(callable.names.iter().copied());
        }
        let omitted = OMITTED_CALLABLES
            .iter()
            .map(|entry| entry.name)
            .collect::<BTreeSet<_>>();
        assert!(
            implemented.is_disjoint(&omitted),
            "a callable cannot be both shipped and omitted"
        );
        let reviewed = REVIEWED_CALLABLES.iter().copied().collect::<BTreeSet<_>>();
        assert_eq!(
            implemented
                .union(&omitted)
                .copied()
                .collect::<BTreeSet<_>>(),
            reviewed
        );
        assert_eq!(
            reviewed.len(),
            REVIEWED_CALLABLES.len(),
            "the reviewed source inventory contains a duplicate"
        );
    }

    #[test]
    fn extension_nodes_preserve_child_purity_and_callback_ownership() {
        let filled = fill_pattern(&rustel_core::js_query(41));
        assert!(filled.purity().impure);
        assert_eq!(filled.purity().reachable, vec![41]);
        assert!(!filled.purity().opaque);
    }

    #[test]
    fn deeply_nested_extension_nodes_destroy_iteratively() {
        let mut pattern = rustel_core::pure(Value::F64(1.0));
        for _ in 0..50_000 {
            pattern = fill_pattern(&pattern);
        }
        assert!(!pattern.purity().impure);
        drop(pattern);
    }

    #[test]
    fn every_extension_name_is_declared_with_where_it_came_from() {
        let registry = default_registry();
        let extensions: Vec<&str> = registry
            .names()
            .into_iter()
            .filter(|name| {
                registry
                    .get(name)
                    .expect("name from registry")
                    .declared_in
                    .origin()
                    == Some(ORIGIN)
            })
            .collect();
        assert_eq!(
            extensions,
            vec![
                "pg",
                "accent",
                "trancegate",
                "tgate",
                "dly",
                "colorparty",
                "mpan",
                "rlpf",
                "rhpf",
                "vstruct",
                "fmtime",
                "acid",
                "stxt",
                "notearp",
                "swap",
                "sb",
                "ifit",
                "flood",
                "sq",
                "glitch",
                "humanize",
                "glide",
                "fill",
                "grab",
                "filtval",
                "up",
                "acidenv",
                "strum",
            ]
        );
        assert_eq!(
            registry.get("fill").expect("fill").declared_in,
            DeclaredIn::Extension(ORIGIN)
        );
    }

    #[test]
    fn no_name_is_registered_twice() {
        let registry = default_registry();
        let mut seen: HashMap<&str, usize> = HashMap::new();
        for name in registry.names() {
            *seen.entry(name).or_default() += 1;
        }
        let twice: Vec<&str> = seen
            .into_iter()
            .filter(|(_, count)| *count > 1)
            .map(|(name, _)| name)
            .collect();
        assert!(twice.is_empty(), "registered more than once: {twice:?}");
    }

    #[test]
    fn porting_a_name_upstream_ships_displaces_our_extension_of_it() {
        let mut registry = default_registry();
        assert!(
            registry
                .get("fill")
                .expect("fill")
                .declared_in
                .is_extension()
        );
        add_in(
            &mut registry,
            DeclaredIn::PatternModule,
            &["fill"],
            fill::REFERENCE,
            1,
            false,
            rustel_core::native_combinator!(|_args, pattern| fill_pattern(&pattern)),
        );
        assert_eq!(
            registry.get("fill").expect("fill").declared_in,
            DeclaredIn::PatternModule
        );
        assert_eq!(
            registry
                .names()
                .iter()
                .filter(|name| **name == "fill")
                .count(),
            1,
            "the displaced entry is gone, not merely outranked"
        );
    }
}
