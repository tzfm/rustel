use rustel_core::Value;
use rustel_core::combinators as c;
use rustel_core::compose::ComposeOp;
use rustel_core::ops::PatOps;
use rustel_core::reference::{ReferenceEntry, ReferenceParam};
use rustel_core::register::{DeclaredIn, Registry, add_in};
use rustel_fraction::Fraction;

use super::shared::{binary_in, clip, fraction, scalar, signal};
use super::{NativeExtensionOperand, ORIGIN, fill_pattern};

pub(super) const REFERENCE: ReferenceEntry = simple_reference!(
    "trancegate",
    "make a seeded sixteen-step trance gate",
    "Builds a seeded sixteen-step gate: the rand signal times density + .5, rounded to steps, segmented into 16, ribboned from the seed across length cycles, and its gaps filled. The receiver is structured with it and clipped to .7.",
    params: [
        ReferenceParam {
            name: "density",
            r#type: "number | Pattern",
            description: "how many steps stay lit: .5 lands about half full, lower thins the gate, higher fills it. Missing argument means silence.",
        },
        ReferenceParam {
            name: "seed",
            r#type: "number | Pattern",
            description: "where the ribbon starts, choosing which sixteen steps repeat. Missing argument means silence.",
        },
        ReferenceParam {
            name: "length",
            r#type: "number | Pattern",
            description: "how many cycles the sixteen-step ribbon spans before repeating. Missing argument means silence.",
        },
    ],
    examples: ["note(\"c e g a\").s(\"sawtooth\").trancegate(0.5, 42, 4)"],
    "rhythm"
);

pub(super) fn ribbon_pattern<P: PatOps>(pattern: &P, offset: &P, cycles: &P) -> P {
    let pattern = pattern.clone();
    let cycles = cycles.clone();
    offset.inner_bind(move |offset| {
        let pattern = pattern.clone();
        let offset = fraction(offset);
        cycles.inner_bind(move |cycles| c::ribbon(&pattern, offset, fraction(cycles)))
    })
}

fn apply<P: PatOps + NativeExtensionOperand>(args: &[P], pattern: &P) -> P {
    let Some(density) = args.first() else {
        return P::pat_silence();
    };
    let Some(seed) = args.get(1) else {
        return P::pat_silence();
    };
    let Some(length) = args.get(2) else {
        return P::pat_silence();
    };
    let density = binary_in(density, &scalar(Value::F64(0.5)), ComposeOp::Add);
    let gate = binary_in(
        &signal(rustel_core::signal::rand()),
        &density,
        ComposeOp::Mul,
    );
    let gate = c::map_numeral(&gate, rustel_core::util::js_round);
    let gate = c::segment(&gate, Fraction::int(16));
    let gate = ribbon_pattern(&gate, seed, length);
    clip(&fill_pattern(&c::struct_with(pattern, &gate)), 0.7)
}

pub(super) fn install(registry: &mut Registry) {
    add_in(
        registry,
        DeclaredIn::Extension(ORIGIN),
        &["trancegate"],
        REFERENCE,
        4,
        false,
        rustel_core::native_patterned_combinator!(|args, pattern| apply(args, &pattern)),
    );
}
