use rustel_core::Value;
use rustel_core::combinators as c;
use rustel_core::ops::PatOps;
use rustel_core::reference::{ReferenceEntry, ReferenceParam};
use rustel_core::register::{DeclaredIn, Registry, add_fn_in};

use super::ORIGIN;

pub(super) const REFERENCE: ReferenceEntry = ReferenceEntry {
    name: "filtval",
    synonyms: &[],
    summary: "transform only where a control holds a value",
    description: "Applies a function to the pattern only at the times where one control equals the value you name. It is how you sidechain from the kicks alone without splitting the lane in two and stacking it back together.\n\nThe comparison is type-strict, though mini-notation reads a bare numeral as a number before it arrives.",
    params: &[
        ReferenceParam {
            name: "control",
            r#type: "string",
            description: "the control to read, e.g. \"s\" or \"n\".",
        },
        ReferenceParam {
            name: "value",
            r#type: "any",
            description: "the value it must hold.",
        },
        ReferenceParam {
            name: "func",
            r#type: "function",
            description: "applied where it does.",
        },
    ],
    examples: &["s(\"bd hh sd hh\").filtval(\"s\", \"bd\", x => x.duck(2))"],
    tags: &["switch angel", "conditional"],
    no_autocomplete: false,
    deprecated: false,
    origin: "switch angel",
};

pub(crate) fn strictly_equal(left: &Value, right: &Value) -> bool {
    match (left, right) {
        (Value::Str(left), Value::Str(right)) => left == right,
        (Value::F64(left), Value::F64(right)) => left == right,
        (Value::Bool(left), Value::Bool(right)) => left == right,
        (Value::Null, Value::Null) | (Value::Undefined, Value::Undefined) => true,
        _ => false,
    }
}

pub(super) fn install(registry: &mut Registry) {
    add_fn_in(
        registry,
        DeclaredIn::Extension(ORIGIN),
        &["filtval"],
        REFERENCE,
        4,
        false,
        rustel_core::native_combinator!(|args, pattern| {
            let (Some(key), Some(wanted)) = (args.first(), args.get(1)) else {
                return pattern;
            };
            let (Value::Str(key), wanted) = (key.clone(), wanted.clone()) else {
                return pattern;
            };
            let function = args.get(2).and_then(Value::as_function).cloned();
            let subject = pattern.clone();
            pattern
                .fmap(move |value| {
                    let matched = value
                        .as_object()
                        .and_then(|object| object.get(&key))
                        .is_some_and(|held| strictly_equal(held, &wanted));
                    Value::Bool(matched)
                })
                .inner_bind(move |flag| {
                    if matches!(flag, Value::Bool(true)) {
                        c::apply(&subject, function.as_ref())
                    } else {
                        subject.clone()
                    }
                })
        }),
    );
}
