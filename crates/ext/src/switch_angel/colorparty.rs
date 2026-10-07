use rustel_core::Value;
use rustel_core::ops::PatOps;
use rustel_core::reference::{ReferenceEntry, ReferenceParam};
use rustel_core::register::{DeclaredIn, Registry, add_in};

use super::ORIGIN;
use super::shared::{control, number, scalar, value};

pub(super) const REFERENCE: ReferenceEntry = simple_reference!(
    "colorparty",
    "choose a named display color",
    "Maps a normalized 0-to-1 number across eight named colors - blue, yellow, violet, green, orange, cyan, magenta, white - and writes the name into the color control. floor(amount × 8) picks the color, and a negative index wraps from the end of the palette like JavaScript's at(): -1/8 is white and -1 is blue again. Anything still outside the palette writes the string \"undefined\".",
    params: [ReferenceParam {
        name: "amount",
        r#type: "number | Pattern",
        description: "normalized position in the palette, 0 to 1; negatives wrap around from the end, and amounts still outside the palette land on the color \"undefined\".",
    }],
    examples: ["note(\"c e g\").s(\"sawtooth\").colorparty(\"<0 .25 .5 .75>\")"],
    "visual"
);

fn apply<P: PatOps>(args: &[Value], pattern: P) -> P {
    const COLORS: &[&str] = &[
        "blue", "yellow", "violet", "green", "orange", "cyan", "magenta", "white",
    ];
    let amount = number(&value(args, 0));
    // Hers is `colors.at(Math.floor(p * colors.length))`, and `at` WRAPS a
    // negative index: -1 is the last color, -8 the first again, and only
    // below that falls off the palette. The `as i64` cast covers the other
    // two edges of `at`: NaN converts to 0 (its ToIntegerOrInfinity) and a
    // huge magnitude saturates far outside the palette.
    let mut index = (amount * COLORS.len() as f64).floor() as i64;
    if index < 0 {
        index += COLORS.len() as i64;
    }
    let color = usize::try_from(index)
        .ok()
        .and_then(|index| COLORS.get(index))
        .copied()
        .unwrap_or("undefined");
    control(&pattern, "color", &scalar(Value::Str(color.into())))
}

pub(super) fn install(registry: &mut Registry) {
    add_in(
        registry,
        DeclaredIn::Extension(ORIGIN),
        &["colorparty"],
        REFERENCE,
        2,
        false,
        rustel_core::native_combinator!(|args, pattern| apply(args, pattern)),
    );
}
