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

/// Merge only the keys carried by the right-hand value.
pub fn merge(left: &Value, right: &Value) -> Value {
    let right = rustel_core::materialize_js_value(right);
    let left = rustel_core::materialize_js_value(left);
    let (Value::Object(base), Value::Object(over)) = (&left, &right) else {
        return right;
    };
    let mut merged = base.clone();
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
