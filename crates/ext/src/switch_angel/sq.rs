use rustel_core::combinators as c;
use rustel_core::ops::PatOps;
use rustel_core::reference::ReferenceEntry;
use rustel_core::register::{DeclaredIn, Registry, add_in};

use super::ORIGIN;

pub(super) const REFERENCE: ReferenceEntry = simple_reference!(
    "sq",
    "square numeric values",
    "Squares each numeric value and passes everything else through - a quick way to bend a linear slider or ramp into a squared curve.",
    params: [],
    examples: ["\"<0 1 2 3>\".sq()"],
    "values"
);

fn apply<P: PatOps>(pattern: P) -> P {
    c::map_numeral(&pattern, |value| value * value)
}

pub(super) fn install(registry: &mut Registry) {
    add_in(
        registry,
        DeclaredIn::Extension(ORIGIN),
        &["sq"],
        REFERENCE,
        1,
        false,
        rustel_core::native_combinator!(|_args, pattern| apply(pattern)),
    );
}
