use rustel_core::extension_node::ExtensionPatternNode;
use rustel_core::reference::{ReferenceEntry, ReferenceParam};
use rustel_core::register::{DeclaredIn, Registry, add_in, value_to_fraction};
use rustel_core::{Hap, Pattern, State};
use rustel_fraction::Fraction;

use super::{NativeExtensionOperand, ORIGIN};

pub(super) const REFERENCE: ReferenceEntry = ReferenceEntry {
    name: "strum",
    synonyms: &[],
    summary: "spread a chord's onsets in time",
    description: "Moves the voices of each chord apart so they arrive one after another, the way a guitarist does not strike six strings at once. The first voice lands early by the amount you give and the last lands late by it, with the rest spaced evenly between, so the chord keeps its centre.\n\nA negative amount strums downward: the top voice arrives first. A single note is left where it is.",
    params: &[ReferenceParam {
        name: "amount",
        r#type: "number | Pattern",
        description: "how far, in cycles, the outer voices move. 0.02 is a quick chord; 0.125 is a slow roll.",
    }],
    examples: &["note(\"a1,c2,e3,g4\").strum(0.08).s(\"gm_slap_bass_2\").clip(0.125)"],
    tags: &["switch angel", "time"],
    no_autocomplete: false,
    deprecated: false,
    origin: "switch angel",
};

struct StrumNode {
    pattern: Option<Pattern>,
    amount: Fraction,
}

impl StrumNode {
    fn collect_congruent(haps: Vec<Hap>) -> Vec<Vec<Hap>> {
        let mut groups: Vec<Vec<Hap>> = Vec::new();
        for hap in haps {
            let mut placed = false;
            for group in &mut groups {
                let other = &group[0];
                let congruent = match (hap.whole, other.whole) {
                    (None, None) => true,
                    (None, Some(_)) => {
                        rustel_core::signal_query_error(|| {
                            "Cannot read properties of undefined (reading 'equals')".into()
                        });
                        return Vec::new();
                    }
                    (Some(_), None) => false,
                    (Some(left), Some(right)) => left == right,
                };
                if congruent {
                    group.push(hap.clone());
                    placed = true;
                    break;
                }
            }
            if !placed {
                groups.push(vec![hap]);
            }
        }
        groups
    }
}

impl ExtensionPatternNode for StrumNode {
    fn kind(&self) -> &'static str {
        "switch_angel.strum"
    }

    /// A pure rearrangement of what the child answered.
    fn volatile(&self) -> bool {
        false
    }

    fn children(&self) -> &[Pattern] {
        self.pattern.as_slice()
    }

    fn query(&self, state: &State) -> Vec<Hap> {
        let pattern = self.pattern.as_ref().expect("live extension node");
        let mut output = Vec::new();
        for group in Self::collect_congruent(pattern.query(state)) {
            let last = group.len().saturating_sub(1);
            if last == 0 {
                output.extend(group);
                continue;
            }
            let width = Fraction::int(last as i128);
            for (index, hap) in group.into_iter().enumerate() {
                let position = Fraction::int(2 * index as i128)
                    .div(width)
                    .sub(Fraction::ONE);
                let offset = self.amount.mul(position);
                output.push(hap.with_span(|span| span.with_time(|time| time.add(offset))));
            }
        }
        output
    }

    fn drain_children(&mut self, out: &mut Vec<Pattern>) {
        out.extend(self.pattern.take());
    }
}

fn apply<P: NativeExtensionOperand>(pattern: &P, amount: Fraction) -> P {
    P::from_extension_node(StrumNode {
        pattern: Some(pattern.pattern_handle()),
        amount,
    })
}

pub(super) fn install(registry: &mut Registry) {
    add_in(
        registry,
        DeclaredIn::Extension(ORIGIN),
        &["strum"],
        REFERENCE,
        2,
        false,
        rustel_core::native_combinator!(|args, pattern| {
            let amount = args
                .first()
                .and_then(value_to_fraction)
                .unwrap_or(Fraction::ZERO);
            apply(&pattern, amount)
        }),
    );
}
