//! `undefined_aeon`'s opinionated native extensions.
//!
//! Each public extension function owns its implementation, registration, and
//! reference metadata in one source file. This module only assembles the
//! statically linked collection.

mod inspire;

use rustel_core::reference::ReferenceEntry;
use rustel_core::register::{Origin, Registry};

use crate::Extension;

/// `undefined_aeon`'s score extensions.
pub const ORIGIN: Origin = Origin::new("undefined_aeon");

const REFERENCE_ENTRIES: &[ReferenceEntry] = &[inspire::REFERENCE];

pub(crate) const EXTENSION: Extension = Extension {
    origin: ORIGIN,
    install_patterns: install,
    pattern_states: &[],
    pattern_callables: &[],
    value_callables: &[],
    reference_groups: &[REFERENCE_ENTRIES],
};

fn install(registry: &mut Registry) {
    inspire::install(registry);
}

#[cfg(test)]
mod tests {
    use crate::default_registry;
    use rustel_core::Value;
    use rustel_core::pure;

    #[test]
    fn native_inspire_keeps_a_native_receiver_pure() {
        let registry = default_registry();
        let registration = registry.get("inspire").expect("inspire registration");
        let pattern = registration.call(
            &[
                pure(Value::Str("ab:major".into())),
                pure(Value::F64(0.4)),
                pure(Value::F64(2.0)),
                pure(Value::F64(10.0)),
                pure(Value::F64(4.0)),
            ],
            pure(Value::object([("s".into(), Value::Str("piano".into()))])),
        );
        assert!(pattern.is_pure());
    }
}
