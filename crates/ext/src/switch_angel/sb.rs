use rustel_core::Value;
use rustel_core::combinators as c;
use rustel_core::reference::{ReferenceEntry, ReferenceParam};
use rustel_core::register::{DeclaredIn, Registry, add_fn_in};

use super::ORIGIN;
use super::shared::{number, value};

pub(super) const REFERENCE: ReferenceEntry = simple_reference!(
    "sb",
    "sometimes apply a callback",
    "Short alias for sometimesBy: with the given probability per cycle, the receiver is sent through the transformer instead of passing through untouched.",
    params: [
        ReferenceParam {
            name: "probability",
            r#type: "number | Pattern",
            description: "chance between 0 and 1 that the transformer fires on a given cycle; a missing argument acts as NaN and never fires.",
        },
        ReferenceParam {
            name: "fn",
            r#type: "function",
            description: "pattern transformer to apply sometimes; a missing or non-function argument leaves the receiver unchanged.",
        },
    ],
    examples: ["note(\"c e g a*4\").s(\"sawtooth\").sb(0.5, x=>x.rev())"],
    "conditional"
);

pub(super) fn install(registry: &mut Registry) {
    add_fn_in(
        registry,
        DeclaredIn::Extension(ORIGIN),
        &["sb"],
        REFERENCE,
        3,
        false,
        rustel_core::native_combinator!(|args, pattern| c::sometimes_by(
            &pattern,
            number(&value(args, 0)),
            args.get(1).and_then(Value::as_function)
        )),
    );
}
