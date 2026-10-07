use rustel_core::combinators as c;
use rustel_core::compose::ComposeOp;
use rustel_core::extension_node::{ExtensionPatternNode, query_inner};
use rustel_core::reference::{ReferenceEntry, ReferenceParam};
use rustel_core::{Hap, Pattern, State, Value};
use rustel_fraction::Fraction;

use super::ORIGIN;
use super::shared::{pure_list, value_as_pattern};
use crate::{CallableSurface, PatternCallable, PatternCallableBehavior};

pub(super) const REFERENCE: ReferenceEntry = simple_reference!(
    "track",
    "arrange named source lanes in tracker sections",
    "Arranges sources in tracker-style sections. After the source array, arguments come in pairs: a length in cycles (or a [cycles, start] pair), then an arrangement string whose hyphen-separated tokens name lanes - token i plays source i, and sections are chained end to end over the total length. A final array of [marker, transformer] pairs may follow; a token equal to a marker sends that source through its transformer.",
    params: [
        ReferenceParam {
            name: "sources",
            r#type: "array",
            description: "the patterns the arrangement lanes point at, addressed by position.",
        },
        ReferenceParam {
            name: "sections",
            r#type: "number | array, then string",
            description: "one length and one arrangement string per section, alternating; lengths are cycles, or [cycles, start] offsets.",
        },
        ReferenceParam {
            name: "modifiers",
            r#type: "array",
            description: "optional final array of [marker, transformer] pairs; matching lane tokens transform the source.",
        },
    ],
    examples: ["track([note(\"c e g\").s(\"sawtooth\"), note(\"c3*2\").s(\"square\")], 4, \"a-b\", 4, \"b-a\")"],
    "arrangement"
);

#[derive(Clone)]
struct TrackModifier {
    marker: Value,
    callback_child: usize,
}

struct TrackSectionNode {
    children: Vec<Pattern>,
    source_count: usize,
    modifiers: Vec<TrackModifier>,
}

impl TrackSectionNode {
    fn selector(&self) -> &Pattern {
        &self.children[0]
    }

    fn source(&self, index: usize) -> Option<&Pattern> {
        (index < self.source_count).then(|| &self.children[index + 1])
    }

    fn callback(&self, index: usize) -> Option<rustel_core::value::FunctionRef> {
        self.children.get(index)?.as_pure()?.as_function().cloned()
    }

    fn split_sections(value: &str) -> Vec<&str> {
        let bytes = value.as_bytes();
        let mut output = Vec::new();
        let mut start = 0;
        let mut cursor = 0;
        while cursor < bytes.len() {
            if bytes[cursor] != b'-' {
                cursor += 1;
                continue;
            }
            output.push(&value[start..cursor]);
            while cursor < bytes.len() && bytes[cursor] == b'-' {
                cursor += 1;
            }
            start = cursor;
        }
        output.push(&value[start..]);
        output
    }
}

impl ExtensionPatternNode for TrackSectionNode {
    fn kind(&self) -> &'static str {
        "switch_angel.track.section"
    }

    /// Selects among its children by their own answers; nothing remembered.
    fn volatile(&self) -> bool {
        false
    }

    fn children(&self) -> &[Pattern] {
        &self.children
    }

    fn query(&self, state: &State) -> Vec<Hap> {
        let mut output = Vec::new();
        for outer in self.selector().query(state) {
            let Value::Str(text) = &outer.value else {
                rustel_core::signal_query_error(|| "str.split is not a function".into());
                return Vec::new();
            };
            let mut selected = Vec::new();
            for (index, token) in Self::split_sections(text).into_iter().enumerate() {
                let token = Value::Str(token.into());
                if ComposeOp::Eq.apply_scalar(&token, &Value::Bool(false)) == Value::Bool(true) {
                    continue;
                }
                let Some(source) = self.source(index) else {
                    continue;
                };
                let mut source = source.clone();
                for modifier in &self.modifiers {
                    if ComposeOp::Eq.apply_scalar(&token, &modifier.marker) != Value::Bool(true) {
                        continue;
                    }
                    let Some(callback) = self.callback(modifier.callback_child) else {
                        rustel_core::signal_query_error(|| {
                            "track modifier is not a pattern transformer".into()
                        });
                        return Vec::new();
                    };
                    source = callback.apply(source);
                }
                selected.push(source);
            }
            output.extend(query_inner(&outer, &rustel_core::stack(selected), state));
        }
        output
    }

    fn drain_children(&mut self, output: &mut Vec<Pattern>) {
        output.append(&mut self.children);
    }
}

fn duration(pattern: &Pattern) -> Option<(Fraction, Fraction)> {
    let value = pattern.as_pure()?;
    let values = match value {
        Value::List(values) => values,
        value => vec![value],
    };
    let cycles = values.first().and_then(|value| {
        rustel_core::register::value_to_fraction(&rustel_core::materialize_js_value(value))
    })?;
    let start = values
        .get(1)
        .and_then(|value| {
            rustel_core::register::value_to_fraction(&rustel_core::materialize_js_value(value))
        })
        .unwrap_or(Fraction::ZERO);
    Some((cycles, start))
}

fn modifiers(pattern: &Pattern) -> Option<Vec<(Value, Pattern)>> {
    let values = pure_list(pattern)?;
    let mut output = Vec::with_capacity(values.len());
    for value in values {
        let Value::List(pair) = rustel_core::materialize_js_value(&value) else {
            return None;
        };
        let marker = pair.first()?.clone();
        let callback = pair.get(1)?.as_function()?.clone();
        output.push((marker, rustel_core::pure(Value::Function(callback))));
    }
    Some(output)
}

fn apply(args: &[Pattern], _receiver: Option<&Pattern>) -> Result<Pattern, &'static str> {
    let Some(source_values) = args.first().and_then(pure_list) else {
        return Err("track requires an array of source patterns");
    };
    let sources = source_values
        .iter()
        .map(value_as_pattern)
        .collect::<Vec<_>>();
    let mut sections = &args[1..];
    let parsed_modifiers = sections.last().and_then(modifiers);
    if parsed_modifiers.is_some() {
        sections = &sections[..sections.len() - 1];
    }
    let modifiers = parsed_modifiers.unwrap_or_default();
    if !sections.len().is_multiple_of(2) {
        return Err("track needs one length for each arrangement pattern");
    }

    let mut total = Fraction::ZERO;
    let mut steps = Vec::with_capacity(sections.len() / 2);
    for pair in sections.as_chunks::<2>().0 {
        let Some((cycles, start)) = duration(&pair[0]) else {
            return Err("track lengths must be constant numbers or [cycles, start]");
        };
        total = total
            .checked_add(cycles)
            .ok_or("track total cycles overflow")?;
        let mut children = Vec::with_capacity(1 + sources.len() + modifiers.len());
        children.push(pair[1].clone());
        children.extend(sources.iter().cloned());
        let mut node_modifiers = Vec::with_capacity(modifiers.len());
        for (marker, callback) in &modifiers {
            let callback_child = children.len();
            children.push(callback.clone());
            node_modifiers.push(TrackModifier {
                marker: marker.clone(),
                callback_child,
            });
        }
        let section = Pattern::extension_node(TrackSectionNode {
            children,
            source_count: sources.len(),
            modifiers: node_modifiers,
        });
        let section = c::ribbon(&section, start, cycles).fast(cycles);
        steps.push((Some(cycles), section));
    }
    Ok(c::stepcat(&steps).slow(total))
}

pub(super) const CALLABLE: PatternCallable = PatternCallable {
    names: &["track"],
    arity: 0,
    origin: ORIGIN,
    surface: CallableSurface::GLOBAL,
    behavior: PatternCallableBehavior::Fallible(apply),
};
