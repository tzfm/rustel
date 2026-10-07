//! Credited extensions that are not part of upstream Strudel.
//!
//! This crate is statically linked: the separation is architectural, not a
//! dynamic-plugin boundary. Core owns reusable pattern primitives and the
//! host owns JavaScript mechanics; each extension owns its public names,
//! musical recipes, attribution, and installation policy here.

pub mod rustel;
pub mod switch_angel;
pub mod undefined_aeon;

use rustel_core::reference::ReferenceEntry;
use rustel_core::register::{Origin, Registry};
use rustel_core::{Pattern, Value};

pub use rustel::ORIGIN as RUSTEL;
pub use switch_angel::ORIGIN as SWITCH_ANGEL;
pub use undefined_aeon::ORIGIN as UNDEFINED_AEON;

/// One statically linked extension collection.
///
/// The descriptor is the collection's single build-time registration point:
/// pattern installation, native callables, and reference metadata are discovered
/// together. Adding a collection does not require changes in the runtime or
/// Studio.
pub struct Extension {
    pub origin: Origin,
    install_patterns: fn(&mut Registry),
    pattern_states: &'static [PatternState],
    pattern_callables: &'static [PatternCallable],
    value_callables: &'static [ValueCallable],
    reference_groups: &'static [&'static [ReferenceEntry]],
}

/// Every extension collection compiled into this binary.
pub const EXTENSIONS: &[Extension] = &[
    rustel::EXTENSION,
    switch_angel::EXTENSION,
    undefined_aeon::EXTENSION,
];

/// Install every credited pattern extension.
pub fn install_pattern_extensions(registry: &mut Registry) {
    for extension in EXTENSIONS {
        (extension.install_patterns)(registry);
    }
}

/// The complete surface shipped by Rustel: credited extensions first, then
/// upstream Strudel. Installing core second lets a future upstream spelling
/// displace an extension automatically.
pub fn default_registry() -> Registry {
    let mut registry = Registry::new();
    install_pattern_extensions(&mut registry);
    rustel_core::register::install_default_registry(&mut registry);
    registry
}

/// One realm-local native pattern slot owned by an extension.
///
/// The JavaScript host keeps the wrapper that owns this pattern's callback
/// cells, but the key, default value, and every operation using it remain in
/// the extension crate. Keys are opaque identities, not public score names.
#[derive(Clone, Copy)]
pub struct PatternState {
    pub key: &'static str,
    pub initial: fn() -> Pattern,
}

/// Where a directly installed native pattern callable is visible.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CallableSurface {
    pub global: bool,
    pub method: bool,
}

impl CallableSurface {
    pub const GLOBAL: Self = Self {
        global: true,
        method: false,
    };
    pub const METHOD: Self = Self {
        global: false,
        method: true,
    };
    pub const BOTH: Self = Self {
        global: true,
        method: true,
    };
}

/// Native behavior for a direct pattern callable.
///
/// Registered/currified combinators continue through `Registry`. This smaller
/// boundary exists for authored constructors, variadic prototype helpers, and
/// realm-local pattern state that do not have register()'s trailing-pattern
/// calling convention.
#[derive(Clone, Copy)]
pub enum PatternCallableBehavior {
    Stateless(fn(&[Pattern], Option<&Pattern>) -> Pattern),
    Fallible(fn(&[Pattern], Option<&Pattern>) -> Result<Pattern, &'static str>),
    ReadState {
        key: &'static str,
        call: fn(&Pattern, &[Pattern], Option<&Pattern>) -> Pattern,
    },
    WriteState {
        key: &'static str,
    },
}

/// One native pattern-returning global and/or Pattern method.
#[derive(Clone, Copy)]
pub struct PatternCallable {
    pub names: &'static [&'static str],
    /// JavaScript's observable `Function.length`; optional/rest handling stays
    /// in the extension-owned Rust body.
    pub arity: usize,
    pub origin: Origin,
    pub surface: CallableSurface,
    pub behavior: PatternCallableBehavior,
}

/// A direct native primitive/list/object helper.
///
/// Unlike pattern callables, arguments are materialized without mini-notation
/// parsing and the returned core value is converted straight back to a
/// JavaScript value. This is for small utilities such as a key-name mapping,
/// not for graph construction.
#[derive(Clone, Copy)]
pub struct ValueCallable {
    pub names: &'static [&'static str],
    pub arity: usize,
    pub origin: Origin,
    pub call: fn(&[Value]) -> Result<Value, &'static str>,
}

/// Every realm-local extension pattern slot.
pub fn pattern_states() -> impl Iterator<Item = &'static PatternState> {
    EXTENSIONS
        .iter()
        .flat_map(|extension| extension.pattern_states.iter())
}

/// Every direct native pattern callable contributed by extensions.
pub fn pattern_callables() -> impl Iterator<Item = &'static PatternCallable> {
    EXTENSIONS
        .iter()
        .flat_map(|extension| extension.pattern_callables.iter())
}

/// Every direct native value callable contributed by extensions.
pub fn value_callables() -> impl Iterator<Item = &'static ValueCallable> {
    EXTENSIONS
        .iter()
        .flat_map(|extension| extension.value_callables.iter())
}

/// Every extension reference entry, discovered from the same compiled
/// descriptors that install the implementation.
pub fn reference_entries() -> impl Iterator<Item = &'static ReferenceEntry> {
    EXTENSIONS
        .iter()
        .flat_map(|extension| extension.reference_groups.iter())
        .flat_map(|group| group.iter())
}

#[cfg(test)]
mod boundary_tests {
    use super::*;

    #[test]
    fn upstream_core_has_no_extension_names_and_the_combined_registry_does() {
        let core = rustel_core::register::default_registry();
        let combined = default_registry();
        for name in ["fill", "strum", "up", "acidenv", "inspire"] {
            assert!(core.get(name).is_none(), "core unexpectedly owns `{name}`");
            assert!(
                combined
                    .get(name)
                    .is_some_and(|entry| entry.declared_in.is_extension()),
                "combined registry is missing extension `{name}`"
            );
        }
        assert!(core.get("seed").is_some(), "per-pattern seed is Strudel");
    }

    #[test]
    fn every_compiled_extension_surface_is_documented_once_under_its_owner() {
        let registry = default_registry();
        let docs = reference_entries().collect::<Vec<_>>();
        for extension in EXTENSIONS {
            for name in registry.names() {
                let Some(registration) = registry.get(name) else {
                    continue;
                };
                if registration.declared_in.origin() != Some(extension.origin) {
                    continue;
                }
                let matches = docs
                    .iter()
                    .filter(|doc| doc.name == name || doc.synonyms.contains(&name));
                assert_eq!(
                    matches.count(),
                    1,
                    "extension pattern `{name}` must have exactly one reference entry"
                );
            }
            for callable in extension.pattern_callables {
                for name in callable.names {
                    let matches = docs
                        .iter()
                        .filter(|doc| doc.name == *name || doc.synonyms.contains(name));
                    assert_eq!(
                        matches.count(),
                        1,
                        "extension pattern callable `{name}` must have exactly one reference entry"
                    );
                }
            }
            for callable in extension.value_callables {
                for name in callable.names {
                    let matches = docs
                        .iter()
                        .filter(|doc| doc.name == *name || doc.synonyms.contains(name));
                    assert_eq!(
                        matches.count(),
                        1,
                        "extension value callable `{name}` must have exactly one reference entry"
                    );
                }
            }
            for group in extension.reference_groups {
                for doc in *group {
                    assert_eq!(doc.origin, extension.origin.label());
                }
            }
        }
    }
}
