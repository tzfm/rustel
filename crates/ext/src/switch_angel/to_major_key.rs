use rustel_core::Value;
use rustel_core::reference::{ReferenceEntry, ReferenceParam};

use super::ORIGIN;
use crate::ValueCallable;

pub(super) const REFERENCE: ReferenceEntry = simple_reference!(
    "toMajorKey",
    "find a scale's associated major key",
    "Finds the relative major key for a scale: minor and phrygian shift the root up three semitones, locrian up one, everything else keeps it. Give a root string, or a [root, scale] pair; unknown roots return undefined, and because the result is a plain string, wrap the call in a control to see it in the query console.",
    params: [ReferenceParam {
        name: "scale",
        r#type: "scale | array",
        description: "a root name like 'c', or a [root, scale] pair such as ['c','minor']; the scale defaults to major.",
    }],
    examples: ["note(\"c e g\").n(toMajorKey(['c','minor']))"],
    "tonal"
);

pub(super) fn convert(input: &Value) -> Result<Value, &'static str> {
    const NOTES: &[&str] = &[
        "C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B",
    ];
    let (root, scale) = match rustel_core::materialize_js_value(input) {
        Value::List(values) => (
            values.first().cloned().unwrap_or(Value::Undefined),
            values
                .get(1)
                .cloned()
                .unwrap_or_else(|| Value::Str("major".into())),
        ),
        value => (value, Value::Str("major".into())),
    };
    let Value::Str(root) = root else {
        return Err("toMajorKey requires a note name");
    };
    let Value::Str(scale) = scale else {
        return Err("toMajorKey requires a scale name");
    };
    let Some(index) = NOTES
        .iter()
        .position(|note| *note == root.to_ascii_uppercase())
    else {
        return Ok(Value::Undefined);
    };
    let offset = match scale.to_ascii_lowercase().as_str() {
        "minor" | "phrygian" => 3,
        "locrian" => 1,
        _ => 0,
    };
    Ok(Value::Str(NOTES[(index + offset) % NOTES.len()].into()))
}

fn apply(args: &[Value]) -> Result<Value, &'static str> {
    convert(args.first().unwrap_or(&Value::Undefined))
}

pub(super) const CALLABLE: ValueCallable = ValueCallable {
    names: &["toMajorKey"],
    arity: 1,
    origin: ORIGIN,
    call: apply,
};
