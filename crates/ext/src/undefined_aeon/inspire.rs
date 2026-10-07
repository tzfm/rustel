use rustel_core::Value;
use rustel_core::combinators as c;
use rustel_core::compose::ComposeOp;
use rustel_core::ops::PatOps;
use rustel_core::reference::{ReferenceEntry, ReferenceParam};
use rustel_core::register::{DeclaredIn, Registry, add_in, value_to_fraction};
use rustel_fraction::Fraction;

use super::ORIGIN;

pub(super) const REFERENCE: ReferenceEntry = ReferenceEntry {
    name: "inspire",
    synonyms: &[],
    summary: "build a gapless random melody inside a scale",
    description: "Turns a sound pattern into a melodic texture: random notes span the requested octaves, scale maps them into the named scale, density controls how often the random mask is applied, tiny deterministic timing movement loosens the grid, fill holds each event until the next onset, and rib repeats the selected phrase. This is an intentionally opinionated undefined_aeon extension rather than a hidden optimization of similar-looking score code.",
    params: &[
        ReferenceParam {
            name: "scale",
            r#type: "scale | Pattern",
            description: "the tonal scale, such as \"ab:major\".",
        },
        ReferenceParam {
            name: "density",
            r#type: "number | Pattern",
            description: "how often events avoid the random mask; 1 keeps every event.",
        },
        ReferenceParam {
            name: "octaves",
            r#type: "number | Pattern",
            description: "the random note range, in twelve-semitone octaves.",
        },
        ReferenceParam {
            name: "seed",
            r#type: "number | Pattern",
            description: "the phrase offset passed to rib.",
        },
        ReferenceParam {
            name: "bars",
            r#type: "number | Pattern",
            description: "the rib phrase length in cycles.",
        },
    ],
    examples: &["s(\"piano\").seg(8).inspire(\"<ab:major>\", 0.4, 2, 10, 4)"],
    tags: &["undefined_aeon", "tonal", "random", "time"],
    no_autocomplete: false,
    deprecated: false,
    origin: "undefined_aeon",
};

fn arg(args: &[Value], index: usize) -> Value {
    args.get(index).cloned().unwrap_or(Value::Undefined)
}

fn fraction_arg(args: &[Value], index: usize) -> Fraction {
    args.get(index)
        .and_then(value_to_fraction)
        .unwrap_or(Fraction::ZERO)
}

fn rand_signal<P: PatOps>() -> P {
    P::from_pure_view(
        rustel_core::signal::rand()
            .as_pure_pattern()
            .expect("rand is pure by construction"),
    )
}

fn rand2_signal<P: PatOps>() -> P {
    c::to_bipolar(&rand_signal())
}

/// The random-mask operation is part of this named recipe, not a hidden
/// special case in the generic pattern graph.
fn sometimes_mask_rand_round<P: PatOps>(pattern: &P, probability: f64) -> P {
    let pattern = pattern.clone();
    P::pat_pure(Value::F64(probability)).inner_bind(move |_| {
        let plain = c::degrade_by(&pattern, probability);
        let transformed = c::undegrade_by(&pattern, 1.0 - probability);
        let rounded_rand = c::map_numeral(&rand_signal(), rustel_core::util::js_round);
        let transformed = c::mask(&transformed, &rounded_rand);
        P::pat_stack(vec![plain, transformed])
    })
}

fn early_pattern<P: PatOps>(pattern: &P, amount: &P) -> P {
    let pattern = pattern.clone();
    amount
        .inner_bind(move |value| pattern.early(value_to_fraction(value).unwrap_or(Fraction::ZERO)))
}

fn inspire<P: PatOps + crate::switch_angel::NativeExtensionOperand>(
    args: &[Value],
    pattern: P,
) -> P {
    let octave_span = ComposeOp::Mul.apply_scalar(&Value::F64(12.0), &arg(args, 2));
    let octave_span = octave_span.as_f64().unwrap_or(f64::NAN);
    let notes = c::range(&rand_signal(), 0.0, octave_span);
    let pattern = rustel_core::controls::apply_pattern("n", &pattern, &notes);

    let pattern = c::scale(&pattern, arg(args, 0));

    let probability = ComposeOp::Sub.apply_scalar(&Value::F64(1.0), &arg(args, 1));
    let probability = probability.as_f64().unwrap_or(f64::NAN);
    let pattern = sometimes_mask_rand_round(&pattern, probability);

    let jitter = c::range(&rand2_signal(), -0.001, 0.001);
    let pattern = early_pattern(&pattern, &jitter);
    let pattern = crate::switch_angel::fill_pattern(&pattern);

    c::ribbon(&pattern, fraction_arg(args, 3), fraction_arg(args, 4))
}

pub(super) fn install(registry: &mut Registry) {
    add_in(
        registry,
        DeclaredIn::Extension(ORIGIN),
        &["inspire"],
        REFERENCE,
        6,
        false,
        rustel_core::native_combinator!(|args, pattern| inspire(args, pattern)),
    );
}
