use rustel_core::extension_node::{ExtensionPatternNode, query_inner};
use rustel_core::ops::PatOps;
use rustel_core::reference::{ReferenceEntry, ReferenceParam};
use rustel_core::register::{DeclaredIn, Registry, add_in};
use rustel_core::{Hap, Pattern, State, Value};
use rustel_fraction::Fraction;

use super::shared::number;
use super::{NativeExtensionOperand, ORIGIN};

pub(super) const REFERENCE: ReferenceEntry = simple_reference!(
    "humanize",
    "loosen timing and velocity",
    "Shifts each event by up to ±0.1 × amount of a cycle off the grid and nudges its velocity by up to ±0.5 × amount. The amount is clamped to 0-1 first. Rustel uses its deterministic score RNG (seeded by randSeed), so scheduler lookahead remains repeatable.",
    params: [ReferenceParam {
        name: "amount",
        r#type: "number | Pattern",
        description: "how loose the timing and velocity get, clamped to 0-1; 0 keeps everything on grid. Missing argument means silence.",
    }],
    examples: ["note(\"c*8 e*8 g*8\").s(\"square\").humanize(0.5)"],
    "random",
    "time"
);

struct HumanizeNode {
    children: Vec<Pattern>,
}

impl ExtensionPatternNode for HumanizeNode {
    fn kind(&self) -> &'static str {
        "switch_angel.humanize"
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
            let amount = number(&outer.value).clamp(0.0, 1.0);
            for (hap_index, hap) in query_inner(&outer, &self.children[1], state)
                .into_iter()
                .enumerate()
            {
                let time = hap.whole_or_part().begin.to_f64();
                let stream =
                    seed + 131_071.0 + amount_index as f64 * 1_009.0 + hap_index as f64 * 9_973.0;
                let random = rustel_core::rng::rands_at_time(time, 2, stream);
                let offset = Fraction::from_f64(0.1 * amount * (2.0 * random[0] - 1.0))
                    .unwrap_or(Fraction::ZERO);
                let velocity_delta = 0.5 * amount * (2.0 * random[1] - 1.0);
                let moved = hap.with_span(|span| span.with_time(|position| position.add(offset)));
                output.push(moved.with_value(|value| match value {
                    Value::Object(object) => {
                        let mut object = object.clone();
                        let velocity = object
                            .get("velocity")
                            .and_then(Value::as_f64)
                            .unwrap_or(1.0)
                            + velocity_delta;
                        object.insert("velocity".into(), Value::F64(velocity));
                        Value::Object(object)
                    }
                    value => Value::object([
                        ("value".into(), value.clone()),
                        ("velocity".into(), Value::F64(1.0 + velocity_delta)),
                    ]),
                }));
            }
        }
        output
    }

    fn drain_children(&mut self, output: &mut Vec<Pattern>) {
        output.append(&mut self.children);
    }
}

fn apply<P: NativeExtensionOperand>(amount: &P, pattern: &P) -> P {
    P::from_extension_node(HumanizeNode {
        children: vec![amount.pattern_handle(), pattern.pattern_handle()],
    })
}

pub(super) fn install(registry: &mut Registry) {
    add_in(
        registry,
        DeclaredIn::Extension(ORIGIN),
        &["humanize"],
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
