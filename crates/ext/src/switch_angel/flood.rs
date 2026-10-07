use rustel_core::Value;
use rustel_core::combinators as c;
use rustel_core::compose::ComposeOp;
use rustel_core::ops::PatOps;
use rustel_core::reference::{ReferenceEntry, ReferenceParam};
use rustel_core::register::{DeclaredIn, Registry, add_in};

use super::ORIGIN;
use super::shared::{binary_in, control, scalar};

pub(super) const REFERENCE: ReferenceEntry = simple_reference!(
    "flood",
    "crossfade into a large room",
    "Crossfades the receiver into a big space: room rises as amount × 1.1 while dry falls as 2 − 2 × amount, clamped between 0 and 1 - at amount 1 the dry signal is gone.",
    params: [ReferenceParam {
        name: "amount",
        r#type: "number | Pattern",
        description: "0 is dry, 1 is fully flooded; values beyond 1 keep growing the room while dry stays clamped at 0. Missing argument means silence.",
    }],
    examples: ["note(\"c e g\").s(\"sawtooth\").flood(0.5)"],
    "effects"
);

fn apply<P: PatOps>(amount: &P, pattern: &P) -> P {
    let room = binary_in(amount, &scalar(Value::F64(1.1)), ComposeOp::Mul);
    let mut output = control(pattern, "room", &room);
    let dry = binary_in(amount, &scalar(Value::F64(-2.0)), ComposeOp::Mul);
    let dry = binary_in(&dry, &scalar(Value::F64(2.0)), ComposeOp::Add);
    let dry = c::map_numeral(&dry, |value| value.clamp(0.0, 1.0));
    output = control(&output, "dry", &dry);
    output
}

pub(super) fn install(registry: &mut Registry) {
    add_in(
        registry,
        DeclaredIn::Extension(ORIGIN),
        &["flood"],
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
