use rustel_core::Value;
use rustel_core::ops::PatOps;
use rustel_core::reference::{ReferenceEntry, ReferenceParam};
use rustel_core::register::{DeclaredIn, Registry, add_in};

use super::ORIGIN;
use super::shared::{control, scalar, value};

pub(super) const REFERENCE: ReferenceEntry = simple_reference!(
    "stxt",
    "turn text bytes into synth controls",
    "Hashes a string into a full voice: rolling byte sums pick cutoff (100-3000), room, vibrato, wavetable, a note offset, delay, envelopes, detune, and more, plus one of nine sounds - the same text always yields the same voice. Empty text leaves the pattern unchanged. Some of those sounds are sample-backed and will not load without their samples; \"rustel\" lands on sawtooth.",
    params: [ReferenceParam {
        name: "text",
        r#type: "string",
        description: "the text to hash into controls; any value works, non-strings are shown as text first, and an empty string is a no-op.",
    }],
    examples: ["note(\"c2*4\").stxt(\"rustel\")"],
    "preset"
);

fn apply<P: PatOps>(args: &[Value], pattern: P) -> P {
    let text = match value(args, 0) {
        Value::Str(text) => text,
        value => value.show(),
    };
    let bytes = text.as_bytes();
    if bytes.is_empty() {
        return pattern;
    }
    let mut output = pattern;
    let mut accumulated = 0_u16;
    let ranges: &[(&str, f64, f64, bool)] = &[
        ("cutoff", 100.0, 3000.0, false),
        ("room", 0.0, 1.0, false),
        ("vib", 0.0, 16.0, false),
        ("vibmod", 0.0, 0.3, false),
        ("wt", 0.0, 1.0, false),
        ("note", -12.0, 8.0, true),
        ("wtrate", 0.0, 5.0, false),
        ("wtdepth", 0.0, 1.0, false),
        ("delay", 0.0, 1.0, false),
        ("delaytime", 0.0, 0.66, false),
        ("delayfeedback", 0.0, 0.6, false),
        ("decay", 0.1, 1.0, false),
        ("attack", 0.0, 0.1, false),
        ("lpenv", 0.0, 8.0, false),
        ("lpdecay", 0.0, 1.0, false),
        ("lpattack", 0.0, 0.5, false),
        ("detune", 0.0, 0.8, false),
    ];
    for (index, (name, min, max, integer)) in ranges.iter().enumerate() {
        let byte = u16::from(bytes[index % bytes.len()]);
        accumulated = accumulated.wrapping_add(byte);
        let adjusted = (byte + accumulated) % 255;
        let mut mapped = (max - min) / 255.0 * f64::from(adjusted) + min;
        if *integer {
            mapped = rustel_core::util::js_round(mapped);
        }
        output = control(&output, name, &scalar(Value::F64(mapped)));
    }
    const SOUNDS: &[&str] = &[
        "sawtooth",
        "supersaw",
        "wt_digital",
        "wt_digital_bad_day",
        "wt_digital_basique",
        "wt_digital_echoes",
        "sine",
        "triangle",
        "pulse",
    ];
    let index = ranges.len();
    let byte = u16::from(bytes[index % bytes.len()]);
    accumulated = accumulated.wrapping_add(byte);
    let adjusted = (byte + accumulated) % 255;
    control(
        &output,
        "s",
        &scalar(Value::Str(
            SOUNDS[usize::from(adjusted) % SOUNDS.len()].into(),
        )),
    )
}

pub(super) fn install(registry: &mut Registry) {
    add_in(
        registry,
        DeclaredIn::Extension(ORIGIN),
        &["stxt"],
        REFERENCE,
        2,
        false,
        rustel_core::native_combinator!(|args, pattern| apply(args, pattern)),
    );
}
