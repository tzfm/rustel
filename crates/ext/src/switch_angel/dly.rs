use rustel_core::Value;
use rustel_core::combinators as c;
use rustel_core::compose::ComposeOp;
use rustel_core::ops::PatOps;
use rustel_core::reference::{ReferenceEntry, ReferenceParam};
use rustel_core::register::{DeclaredIn, Registry, add_in};

use super::ORIGIN;
use super::shared::{binary_in, control, scalar};

pub(super) const REFERENCE: ReferenceEntry = simple_reference!(
    "dly",
    "couple delay mix and feedback",
    "Sets delay to amount × 0.8 and delayfeedback to amount squared, so feedback grows with the mix. Once the amount reaches 1, the dry events are masked away and only the echoes remain.",
    params: [ReferenceParam {
        name: "amount",
        r#type: "number | Pattern",
        description: "delay mix from 0 upward; 1 removes the dry events entirely. Missing argument means silence.",
    }],
    examples: ["note(\"c e g\").s(\"sawtooth\").dly(0.5)"],
    "effects"
);

pub(super) fn apply<P: PatOps>(amount: &P, pattern: &P) -> P {
    let delay = binary_in(amount, &scalar(Value::F64(0.8)), ComposeOp::Mul);
    let feedback = binary_in(amount, &scalar(Value::F64(2.0)), ComposeOp::Pow);
    let mut output = control(pattern, "delay", &delay);
    output = control(&output, "delayfeedback", &feedback);
    let mask = c::invert(&c::map_numeral(amount, f64::floor));
    c::mask(&output, &mask)
}

pub(super) fn install(registry: &mut Registry) {
    add_in(
        registry,
        DeclaredIn::Extension(ORIGIN),
        &["dly"],
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
