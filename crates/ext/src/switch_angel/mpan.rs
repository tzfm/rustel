use rustel_core::Value;
use rustel_core::ops::PatOps;
use rustel_core::reference::{ReferenceEntry, ReferenceParam};
use rustel_core::register::{DeclaredIn, Registry, add_in};

use super::ORIGIN;
use super::shared::{control, number, scalar, value};

pub(super) const REFERENCE: ReferenceEntry = simple_reference!(
    "mpan",
    "pan across several output orbits",
    "Spreads one normalized position across a list of output orbits: round(amount × (orbits − 1)) picks the orbit, and the fractional remainder of amount × orbits sets pan inside it. An empty orbit list leaves the pattern untouched.",
    params: [
        ReferenceParam {
            name: "orbits",
            r#type: "array | number | Pattern",
            description: "the orbits to spread across; a single value works too, and an empty list is a no-op.",
        },
        ReferenceParam {
            name: "amount",
            r#type: "number | Pattern",
            description: "normalized position from 0 to 1 along the orbit list; it selects the orbit and the pan within it.",
        },
    ],
    examples: ["note(\"c e g\").s(\"sawtooth\").mpan([0, 1, 2, 3], \"<0 .25 .5 .75>\")"],
    "spatial"
);

fn apply<P: PatOps>(args: &[Value], pattern: P) -> P {
    let orbits = match rustel_core::materialize_js_value(&value(args, 0)) {
        Value::List(values) => values,
        value => vec![value],
    };
    if orbits.is_empty() {
        return pattern;
    }
    let amount = number(&value(args, 1));
    let index = rustel_core::util::js_round(amount * (orbits.len() - 1) as f64) as isize;
    let orbit = usize::try_from(index)
        .ok()
        .and_then(|index| orbits.get(index))
        .cloned()
        .unwrap_or(Value::Undefined);
    let mut output = control(&pattern, "orbit", &scalar(orbit));
    let pan = (amount * orbits.len() as f64) % 1.0;
    output = control(&output, "pan", &scalar(Value::F64(pan)));
    output
}

pub(super) fn install(registry: &mut Registry) {
    add_in(
        registry,
        DeclaredIn::Extension(ORIGIN),
        &["mpan"],
        REFERENCE,
        3,
        false,
        rustel_core::native_combinator!(|args, pattern| apply(args, pattern)),
    );
}
