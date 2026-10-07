use rustel_core::Value;
use rustel_core::reference::{ReferenceEntry, ReferenceParam};
use rustel_core::register::{DeclaredIn, Registry, add_in};

use super::ORIGIN;

pub(super) const REFERENCE: ReferenceEntry = ReferenceEntry {
    name: "grab",
    synonyms: &[],
    summary: "snap each note to the nearest of a set of pitches",
    description: "Moves every note to the closest pitch in the set you give it, keeping the note's octave so a melody stays in its register. Where scale() offers named modes, this takes any collection at all: grab(\"e:g:b\") pins a line to an E minor triad.",
    params: &[ReferenceParam {
        name: "pitches",
        r#type: "string | number | Pattern",
        description: "the set to snap to, as note names or semitones: \"e:g:b\", \"0:5:7\".",
    }],
    examples: &["n(\"0 3 5 7 12\").grab(\"e:g:b\").s(\"piano\")"],
    tags: &["switch angel", "tonal"],
    no_autocomplete: false,
    deprecated: false,
    origin: "switch angel",
};

fn targets(arg: Option<&Value>) -> Vec<f64> {
    let items: Vec<&Value> = match arg {
        Some(Value::List(items)) => items.iter().collect(),
        Some(value) => vec![value],
        None => vec![],
    };
    items
        .into_iter()
        .filter_map(|value| match value {
            Value::F64(number) => Some(*number),
            Value::Str(name) => rustel_core::util::note_to_midi(name, 3)
                .ok()
                .map(|number| number - 48.0),
            _ => None,
        })
        .collect()
}

fn snap(value: &Value, targets: &[f64]) -> Value {
    let object = value.as_object();
    let note = match object {
        Some(map) => map.get("n").or_else(|| map.get("note")),
        None => Some(value),
    };
    let Some(note) = note.and_then(|note| match note {
        Value::F64(number) => Some(*number),
        Value::Str(name) => rustel_core::util::note_to_midi(name, 3).ok(),
        _ => None,
    }) else {
        return value.clone();
    };
    let octave = (note / 12.0).trunc();
    let transpose = octave * 12.0;
    let goal = note - transpose;
    let nearest = targets
        .iter()
        .copied()
        .reduce(|best, candidate| {
            if (candidate - goal).abs() < (best - goal).abs() {
                candidate
            } else {
                best
            }
        })
        .unwrap_or(goal);
    let snapped = nearest + transpose;
    match object {
        Some(map) => {
            let mut output = map.clone();
            output.remove("n");
            output.insert("note".to_owned(), Value::F64(snapped));
            Value::Object(output)
        }
        None => Value::F64(snapped),
    }
}

pub(super) fn install(registry: &mut Registry) {
    add_in(
        registry,
        DeclaredIn::Extension(ORIGIN),
        &["grab"],
        REFERENCE,
        2,
        false,
        rustel_core::native_combinator!(|args, pattern| {
            let targets = targets(args.first());
            if targets.is_empty() {
                return pattern;
            }
            pattern.fmap(move |value| snap(value, &targets))
        }),
    );
}
