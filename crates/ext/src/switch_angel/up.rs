use rustel_core::Value;
use rustel_core::ops::PatOps;
use rustel_core::reference::{ReferenceEntry, ReferenceParam};
use rustel_core::register::{DeclaredIn, Registry, add_in};

use super::ORIGIN;

pub(super) const REFERENCE: ReferenceEntry = ReferenceEntry {
    name: "up",
    synonyms: &[],
    summary: "set several controls and the rhythm from one pattern",
    description: "Writes every control the given pattern carries over the events underneath, and leaves alone every one it does not. Because the structure comes from both sides, a single string sets the rhythm AND the values at once - Elektron-style parameter locks.\n\nA rest removes the event; a value that is simply absent from the mapping leaves what was there. This is what separates it from set.mix, which writes the whole value.",
    params: &[ReferenceParam {
        name: "values",
        r#type: "Pattern",
        description: "usually a mini-notation string with .as(...) naming the controls it carries.",
    }],
    examples: &["s(\"bd\").n(5).room(0.1).up(\"1 0.5 ~ 0.7:4\".as(\"velocity:n\"))"],
    tags: &["switch angel", "value modifiers"],
    no_autocomplete: false,
    deprecated: false,
    origin: "switch angel",
};

/// Merge only the keys carried by the right-hand value. A value that is
/// not an object carries no keys, so it leaves the event as it was; an
/// event that is not an object takes the right-hand keys alone.
pub fn merge(left: &Value, right: &Value) -> Value {
    let right = rustel_core::materialize_js_value(right);
    let left = rustel_core::materialize_js_value(left);
    let Value::Object(over) = &right else {
        return left;
    };
    let mut merged = match left {
        Value::Object(base) => base,
        _ => Default::default(),
    };
    for (key, value) in over.iter() {
        if matches!(value, Value::Undefined) {
            continue;
        }
        merged.insert(key.to_owned(), value.clone());
    }
    Value::Object(merged)
}

pub(super) fn install(registry: &mut Registry) {
    add_in(
        registry,
        DeclaredIn::Extension(ORIGIN),
        &["up"],
        REFERENCE,
        2,
        false,
        rustel_core::native_patterned_combinator!(|args, pattern| {
            let Some(values) = args.first() else {
                return PatOps::pat_silence();
            };
            pattern.app_both_with(values.clone(), merge)
        }),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn object(pairs: &[(&str, Value)]) -> Value {
        Value::object(
            pairs
                .iter()
                .map(|(key, value)| ((*key).to_owned(), value.clone())),
        )
    }

    /// A right-hand value that is not an object carries no keys.
    #[test]
    fn a_value_without_keys_leaves_the_event() {
        let event = object(&[("note", Value::F64(48.0))]);
        for right in [
            Value::F64(7.0),
            Value::Str("rev".into()),
            Value::list(vec![Value::F64(1.0)]),
            Value::Null,
            Value::Undefined,
        ] {
            assert_eq!(merge(&event, &right), event, "{right:?}");
        }
        assert_eq!(merge(&Value::F64(1.0), &Value::F64(2.0)), Value::F64(1.0));
    }

    /// An event that is not an object takes the right-hand keys, without
    /// the undefined ones.
    #[test]
    fn an_event_without_keys_takes_the_defined_right_hand_keys() {
        let right = object(&[("gain", Value::F64(0.5)), ("pan", Value::Undefined)]);
        assert_eq!(
            merge(&Value::F64(1.0), &right),
            object(&[("gain", Value::F64(0.5))])
        );
    }
}
