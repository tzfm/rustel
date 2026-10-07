use rustel_core::extension_node::{ExtensionPatternNode, query_inner};
use rustel_core::ops::PatOps;
use rustel_core::reference::{ReferenceEntry, ReferenceParam};
use rustel_core::register::{DeclaredIn, Registry, add_in};
use rustel_core::{Hap, Pattern, State, Value};

use super::shared::number;
use super::{NativeExtensionOperand, ORIGIN};

pub(super) const REFERENCE: ReferenceEntry = simple_reference!(
    "glitch",
    "randomly perturb event controls",
    "Multiplies every numeric control in each event by 1 ± amount at random, and pushes note names upward by up to half the amount, never below MIDI 24; orbit and duckorbit routing stay untouched. Rustel uses its deterministic score RNG (seeded by randSeed), so re-querying a span glitches it the same way.",
    params: [ReferenceParam {
        name: "amount",
        r#type: "number | Pattern",
        description: "how far values may wander; 0 leaves events unchanged. Missing argument means silence.",
    }],
    examples: ["note(\"c e g\").s(\"sawtooth\").glitch(0.3)"],
    "random",
    "values"
);

struct GlitchNode {
    children: Vec<Pattern>,
}

impl ExtensionPatternNode for GlitchNode {
    fn kind(&self) -> &'static str {
        "switch_angel.glitch"
    }

    /// Randomness comes from the query's time, seed and settings alone.
    fn volatile(&self) -> bool {
        false
    }

    fn children(&self) -> &[Pattern] {
        &self.children
    }

    fn query(&self, state: &State) -> Vec<Hap> {
        let seed = state
            .controls
            .get("randSeed")
            .and_then(Value::as_f64)
            .unwrap_or(0.0);
        let mut output = Vec::new();
        for (amount_index, outer) in self.children[0].query(state).into_iter().enumerate() {
            let amount = number(&outer.value);
            for (hap_index, hap) in query_inner(&outer, &self.children[1], state)
                .into_iter()
                .enumerate()
            {
                let time = hap.whole_or_part().begin.to_f64();
                let stream =
                    seed + 65_537.0 + amount_index as f64 * 1_009.0 + hap_index as f64 * 9_973.0;
                let value = match &hap.value {
                    Value::Object(object) => {
                        let mut object = object.clone();
                        let keys = object
                            .js_entries()
                            .into_iter()
                            .map(|(key, _)| key.to_owned())
                            .collect::<Vec<_>>();
                        let random = rustel_core::rng::rands_at_time(
                            time,
                            keys.len().saturating_mul(2),
                            stream,
                        );
                        let mut cursor = 0;
                        for key in keys {
                            let signed = random.get(cursor).copied().unwrap_or(0.5) * 2.0 - 1.0;
                            cursor = cursor.saturating_add(1);
                            if key == "orbit" || key == "duckorbit" {
                                continue;
                            }
                            let Some(current) = object.get(&key).cloned() else {
                                continue;
                            };
                            let next = if key == "note" && !matches!(current, Value::F64(_)) {
                                let upward = random.get(cursor).copied().unwrap_or(0.5);
                                cursor = cursor.saturating_add(1);
                                let midi = match &current {
                                    Value::Str(note) => {
                                        rustel_core::util::note_to_midi(note, 3).unwrap_or(f64::NAN)
                                    }
                                    _ => f64::NAN,
                                };
                                Value::F64(rustel_core::util::js_round(
                                    (midi * (1.0 + amount * 0.5 * upward)).max(24.0),
                                ))
                            } else if let Value::F64(current) = current {
                                Value::F64(current * (1.0 + signed * amount))
                            } else {
                                current
                            };
                            object.insert(key, next);
                        }
                        Value::Object(object)
                    }
                    value => value.clone(),
                };
                output.push(hap.with_value(|_| value.clone()));
            }
        }
        output
    }

    fn drain_children(&mut self, output: &mut Vec<Pattern>) {
        output.append(&mut self.children);
    }
}

fn apply<P: NativeExtensionOperand>(amount: &P, pattern: &P) -> P {
    P::from_extension_node(GlitchNode {
        children: vec![amount.pattern_handle(), pattern.pattern_handle()],
    })
}

pub(super) fn install(registry: &mut Registry) {
    add_in(
        registry,
        DeclaredIn::Extension(ORIGIN),
        &["glitch"],
        REFERENCE,
        2,
        false,
        rustel_core::native_patterned_combinator!(|args, pattern| {
            let Some(amount) = args.first() else {
                return PatOps::pat_silence();
            };
            apply(amount, &pattern)
        }),
    );
}
