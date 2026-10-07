use rustel_core::Value;
use rustel_core::ops::PatOps;
use rustel_core::reference::ReferenceEntry;
use rustel_core::register::{DeclaredIn, Registry, add_in};

use super::ORIGIN;
use super::shared::{control, scalar};

pub(super) const REFERENCE: ReferenceEntry = simple_reference!(
    "acid",
    "apply Switch Angel's acid voice preset",
    "One call switches the sound to supersaw and writes the authored acid voice: detune .5, unison 1, cutoff 100, lpsustain .2, lpdecay .2, lpenv 2, resonance 12. It takes no arguments; give it notes and it squelches.",
    params: [],
    examples: ["note(\"c2 eb2 g2 bb2\").acid()"],
    "preset"
);

fn apply<P: PatOps>(pattern: P) -> P {
    let mut output = control(&pattern, "s", &scalar(Value::Str("supersaw".into())));
    for (name, value) in [
        ("detune", 0.5),
        ("unison", 1.0),
        ("cutoff", 100.0),
        ("lpsustain", 0.2),
        ("lpdecay", 0.2),
        ("lpenv", 2.0),
        ("resonance", 12.0),
    ] {
        output = control(&output, name, &scalar(Value::F64(value)));
    }
    output
}

pub(super) fn install(registry: &mut Registry) {
    add_in(
        registry,
        DeclaredIn::Extension(ORIGIN),
        &["acid"],
        REFERENCE,
        1,
        false,
        rustel_core::native_combinator!(|_args, pattern| apply(pattern)),
    );
}
