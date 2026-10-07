use rustel_core::Value;
use rustel_core::compose::ComposeOp;
use rustel_core::ops::PatOps;
use rustel_core::reference::{ReferenceEntry, ReferenceParam};
use rustel_core::register::{DeclaredIn, Registry, add_in};

use super::ORIGIN;
use super::shared::{binary, control, number, scalar};

pub(super) const REFERENCE: ReferenceEntry = simple_reference!(
    "accent",
    "accent notes with velocity and filter envelope",
    "Raises velocity to 1 + amount/8 and the low-pass envelope to 1 + amount/5 together, then tightens the filter with lpdecay .13 and lpattack .01; zero leaves the pattern unchanged. Note that this engine builds those multiplier controls on silence, which carries no events, so a non-zero amount currently silences the receiver instead of accenting it, unlike strudel.cc.",
    params: [ReferenceParam {
        name: "amount",
        r#type: "number | Pattern",
        description: "how hard to accent; 0 leaves the pattern unchanged, around 1 is a strong accent, and any non-zero value currently silences the receiver in this engine.",
    }],
    examples: ["note(\"c3 eb3 g3\").s(\"sawtooth\").accent(0)"],
    "dynamics"
);

fn apply<P: PatOps>(amount: &P, pattern: &P) -> P {
    let pattern = pattern.clone();
    amount.inner_bind(move |amount| {
        let amount = number(amount);
        if amount == 0.0 {
            return pattern.clone();
        }
        let velocity = control(
            &P::pat_silence(),
            "velocity",
            &scalar(Value::F64(1.0 + amount / 8.0)),
        );
        let mut output = binary(&pattern, &velocity, ComposeOp::Mul);
        let lpenv = control(
            &P::pat_silence(),
            "lpenv",
            &scalar(Value::F64(1.0 + amount / 5.0)),
        );
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
