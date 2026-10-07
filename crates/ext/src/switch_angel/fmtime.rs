use rustel_core::Value;
use rustel_core::compose::ComposeOp;
use rustel_core::ops::PatOps;
use rustel_core::reference::{ReferenceEntry, ReferenceParam};
use rustel_core::register::{DeclaredIn, Registry, add_in};

use super::ORIGIN;
use super::shared::{binary_in, control, scalar, signal, value};

pub(super) const REFERENCE: ReferenceEntry = simple_reference!(
    "fmtime",
    "drive FM from cycle time",
    "Writes the same slow ramp into both fm and fmh: the running cycle time modulo length, plus a start offset. The longer the length, the slower the FM drift; the receiver must already be an FM-capable sound.",
    params: [
        ReferenceParam {
            name: "start",
            r#type: "number | Pattern",
            description: "offset added to the wrapped time; shifts where the drift begins. Both arguments are required at the score level.",
        },
        ReferenceParam {
            name: "length",
            r#type: "number | Pattern",
            description: "the modulus the cycle time wraps against, in cycles; longer means slower drift.",
        },
    ],
    examples: ["note(\"c2*4\").s(\"sine\").fmtime(0, 8)"],
    "synthesis"
);

fn apply<P: PatOps>(args: &[Value], pattern: P) -> P {
    let length = scalar(value(args, 1));
    let start = scalar(value(args, 0));
    let modu = binary_in(
        &signal(rustel_core::signal::time()),
        &length,
        ComposeOp::Mod,
    );
    let modu = binary_in(&modu, &start, ComposeOp::Add);
    let output = control(&pattern, "fm", &modu);
    control(&output, "fmh", &modu)
}

pub(super) fn install(registry: &mut Registry) {
    add_in(
        registry,
        DeclaredIn::Extension(ORIGIN),
        &["fmtime"],
        REFERENCE,
        3,
        false,
        rustel_core::native_combinator!(|args, pattern| apply(args, pattern)),
    );
}
