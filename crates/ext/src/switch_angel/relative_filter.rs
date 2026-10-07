use rustel_core::Value;
use rustel_core::compose::ComposeOp;
use rustel_core::ops::PatOps;
use rustel_core::reference::{ReferenceEntry, ReferenceParam};
use rustel_core::register::{DeclaredIn, Registry, add_in};

use super::ORIGIN;
use super::shared::{binary_in, control, scalar};

pub(super) const LOWPASS_REFERENCE: ReferenceEntry = simple_reference!(
    "rlpf",
    "exponential low-pass control",
    "Maps a normalized value exponentially into the low-pass range: cutoff = (amount × 12)⁴, so 0 closes the filter and 1 lands around 20.7 kHz. Suited to a 0-to-1 slider.",
    params: [ReferenceParam {
        name: "amount",
        r#type: "number | Pattern",
        description: "normalized filter position, usually 0 to 1; the cutoff grows with the fourth power of amount × 12. Missing argument means silence.",
    }],
    examples: ["note(\"c e g\").s(\"sawtooth\").rlpf(0.5)"],
    "filter"
);

pub(super) const HIGHPASS_REFERENCE: ReferenceEntry = simple_reference!(
    "rhpf",
    "exponential high-pass control",
    "Maps a normalized value exponentially into the high-pass range: hcutoff = (amount × 12)⁴, so 0 closes the filter and 1 lands around 20.7 kHz. Suited to a 0-to-1 slider.",
    params: [ReferenceParam {
        name: "amount",
        r#type: "number | Pattern",
        description: "normalized filter position, usually 0 to 1; the cutoff grows with the fourth power of amount × 12. Missing argument means silence.",
    }],
    examples: ["note(\"c e g\").s(\"sawtooth\").rhpf(0.5)"],
    "filter"
);

fn apply<P: PatOps>(amount: &P, pattern: &P, control_name: &str) -> P {
    let scaled = binary_in(amount, &scalar(Value::F64(12.0)), ComposeOp::Mul);
    let scaled = binary_in(&scaled, &scalar(Value::F64(4.0)), ComposeOp::Pow);
    control(pattern, control_name, &scaled)
}

pub(super) fn install(registry: &mut Registry) {
    add_in(
        registry,
        DeclaredIn::Extension(ORIGIN),
        &["rlpf"],
        LOWPASS_REFERENCE,
        2,
        false,
        rustel_core::native_patterned_combinator!(|args, pattern| {
            let Some(amount) = args.first() else {
                return PatOps::pat_silence();
            };
            apply(amount, &pattern, "cutoff")
        }),
    );
    add_in(
        registry,
        DeclaredIn::Extension(ORIGIN),
        &["rhpf"],
        HIGHPASS_REFERENCE,
        2,
        false,
        rustel_core::native_patterned_combinator!(|args, pattern| {
            let Some(amount) = args.first() else {
                return PatOps::pat_silence();
            };
            apply(amount, &pattern, "hcutoff")
        }),
    );
}
