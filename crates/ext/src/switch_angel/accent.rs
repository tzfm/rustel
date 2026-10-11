use rustel_core::Value;
use rustel_core::compose::ComposeOp;
use rustel_core::ops::PatOps;
use rustel_core::reference::{ReferenceEntry, ReferenceParam};
use rustel_core::register::{DeclaredIn, Registry, add_in};

use super::ORIGIN;
use super::shared::{binary, control, number, scalar, source_control_pattern};

pub(super) const REFERENCE: ReferenceEntry = simple_reference!(
    "accent",
    "accent notes with velocity and filter envelope",
    "Multiplies velocity by 1 + amount/8 and the low-pass envelope by 1 + amount/5, then tightens the filter with lpdecay .13 and lpattack .01. A note with no velocity or envelope takes the factor. Zero leaves the pattern unchanged.",
    params: [ReferenceParam {
        name: "amount",
        r#type: "number | Pattern",
        description: "how hard to accent; 0 leaves the pattern unchanged, and around 1 is a strong accent.",
    }],
    examples: ["note(\"c3 eb3 g3\").s(\"sawtooth\").accent(1)"],
    "dynamics"
);

fn apply<P: PatOps>(amount: &P, pattern: &P) -> P {
    let pattern = pattern.clone();
    amount.inner_bind(move |amount| {
        let amount = number(amount);
        if amount == 0.0 {
            return pattern.clone();
        }
        let velocity = source_control_pattern("velocity", scalar(Value::F64(1.0 + amount / 8.0)));
        let mut output = binary(&pattern, &velocity, ComposeOp::Mul);
        let lpenv = source_control_pattern("lpenv", scalar(Value::F64(1.0 + amount / 5.0)));
        output = binary(&output, &lpenv, ComposeOp::Mul);
        output = control(&output, "lpdecay", &scalar(Value::F64(0.13)));
        control(&output, "lpattack", &scalar(Value::F64(0.01)))
    })
}

pub(super) fn install(registry: &mut Registry) {
    add_in(
        registry,
        DeclaredIn::Extension(ORIGIN),
        &["accent"],
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
