//! Inlet metadata ported from KabelSalat's graph and library definitions.
//! Editor descriptions, examples, and source generators are omitted.

use super::*;

type Inlet = (&'static str, Option<f64>, bool);
const SCHEMAS: &[(&str, Option<&[Inlet]>)] = &[
    ("register", None),
    ("module", None),
    ("n", Some(&[("value", Some(0.0), false)])),
    ("out", None),
    ("withIns", Some(&[("in", None, true)])),
    ("flatten", None),
    ("apply", None),
    ("clone", None),
    ("map", None),
    ("select", None),
    ("debug", None),
    ("exit", None),
    ("time", None),
    ("raw", Some(&[("in", None, false), ("code", None, false)])),
    (
        "bytebeat",
        Some(&[("t", None, false), ("code", None, false)]),
    ),
    (
        "floatbeat",
        Some(&[("t", None, false), ("code", None, false)]),
    ),
    (
        "adsr",
        Some(&[
            ("gate", Some(0.0), false),
            ("att", Some(0.02), false),
            ("dec", Some(0.1), false),
            ("sus", Some(0.2), false),
            ("rel", Some(0.1), false),
        ]),
    ),
    (
        "ar",
        Some(&[
            ("trig", Some(0.0), false),
            ("att", Some(0.02), false),
            ("rel", Some(0.1), false),
        ]),
    ),
    (
        "ad",
        Some(&[
            ("trig", Some(0.0), false),
            ("att", Some(0.02), false),
            ("dec", Some(0.1), false),
        ]),
    ),
    ("clock", Some(&[("bpm", Some(120.0), false)])),
    (
        "clockdiv",
        Some(&[("clock", Some(0.0), false), ("divisor", Some(2.0), false)]),
    ),
    (
        "distort",
        Some(&[("in", Some(0.0), false), ("amt", Some(0.0), false)]),
    ),
    ("noise", Some(&[("next", Some(1.0), false)])),
    (
        "lcgnoise",
        Some(&[("next", Some(1.0), false), ("reset", Some(0.0), false)]),
    ),
    ("pink", Some(&[])),
    ("brown", Some(&[])),
    ("dust", Some(&[("density", Some(0.0), false)])),
    (
        "impulse",
        Some(&[("freq", Some(0.0), false), ("phase", Some(0.0), false)]),
    ),
    ("saw", Some(&[("freq", Some(0.0), false)])),
    ("zaw", Some(&[("freq", Some(0.0), false)])),
    (
        "sine",
        Some(&[
            ("freq", Some(0.0), false),
            ("sync", Some(0.0), false),
            ("phase", Some(0.0), false),
        ]),
    ),
    ("tri", Some(&[("freq", Some(0.0), false)])),
    (
        "pulse",
        Some(&[("freq", Some(0.0), false), ("pw", Some(0.5), false)]),
    ),
    (
        "slide",
        Some(&[("in", Some(0.0), false), ("rate", Some(1.0), false)]),
    ),
    (
        "lag",
        Some(&[("in", Some(0.0), false), ("rate", Some(1.0), false)]),
    ),
    (
        "slew",
        Some(&[
            ("in", Some(0.0), false),
            ("up", Some(1.0), false),
            ("dn", Some(1.0), false),
        ]),
    ),
    (
        "filter",
        Some(&[
            ("in", Some(0.0), false),
            ("cutoff", Some(1.0), false),
            ("reso", Some(0.0), false),
        ]),
    ),
    (
        "fold",
        Some(&[("in", Some(0.0), false), ("rate", Some(0.0), false)]),
    ),
    (
        "seq",
        Some(&[("trig", Some(0.0), false), ("step", Some(0.0), true)]),
    ),
    (
        "delay",
        Some(&[("in", Some(0.0), false), ("time", Some(0.0), false)]),
    ),
    (
        "hold",
        Some(&[("in", Some(0.0), false), ("trig", Some(0.0), false)]),
    ),
    ("midifreq", Some(&[("channel", Some(-1.0), false)])),
    ("midigate", Some(&[("channel", Some(-1.0), false)])),
    ("midivel", Some(&[("channel", Some(-1.0), false)])),
    (
        "midicc",
        Some(&[
            ("ccnumber", Some(-1.0), false),
            ("channel", Some(-1.0), false),
            ("initValue", Some(-1.0), false),
        ]),
    ),
    (
        "cc",
        Some(&[("id", Some(0.0), false), ("value", Some(0.0), false)]),
    ),
    ("audioin", Some(&[])),
    ("log", Some(&[("in", None, false)])),
    ("exp", Some(&[("in", None, false)])),
    ("pow", Some(&[("in", None, false), ("power", None, false)])),
    ("sin", Some(&[("in", None, false)])),
    ("cos", Some(&[("in", None, false)])),
    ("tan", Some(&[("in", None, false)])),
    ("acos", Some(&[("in", None, false)])),
    ("asin", Some(&[("in", None, false)])),
    ("atan", Some(&[("in", None, false)])),
    ("mul", Some(&[("in", None, true)])),
    ("add", Some(&[("in", None, true)])),
    ("div", Some(&[("in", None, true)])),
    ("sub", Some(&[("in", None, true)])),
    ("mod", Some(&[("in", None, false), ("modulo", None, false)])),
    ("abs", Some(&[("in", None, false)])),
    ("round", Some(&[("in", None, false)])),
    (
        "clamp",
        Some(&[
            ("in", None, false),
            ("min", None, false),
            ("max", None, false),
        ]),
    ),
    ("floor", Some(&[("in", None, false)])),
    ("ceil", Some(&[("in", None, false)])),
    ("sign", Some(&[("in", None, false)])),
    ("min", Some(&[("in", None, true)])),
    ("max", Some(&[("in", None, true)])),
    ("argmin", Some(&[("in", None, true)])),
    ("argmax", Some(&[("in", None, true)])),
    (
        "greater",
        Some(&[("in", None, false), ("threshold", None, false)]),
    ),
    (
        "lower",
        Some(&[("in", None, false), ("threshold", None, false)]),
    ),
    ("xor", Some(&[("a", None, false), ("b", None, false)])),
    ("and", Some(&[("a", None, false), ("b", None, false)])),
    ("or", Some(&[("a", None, false), ("b", None, false)])),
    ("not", Some(&[("in", None, false)])),
    ("bool", Some(&[("a", None, false)])),
    (
        "ifelse",
        Some(&[
            ("control", None, false),
            ("a", None, false),
            ("b", None, false),
        ]),
    ),
    (
        "range",
        Some(&[
            ("in", None, false),
            ("min", None, false),
            ("max", None, false),
        ]),
    ),
    (
        "remap",
        Some(&[
            ("in", None, false),
            ("inmin", None, false),
            ("inmax", None, false),
            ("outmin", None, false),
            ("outmax", None, false),
        ]),
    ),
    ("thru", None),
    (
        "rangex",
        Some(&[
            ("in", None, false),
            ("min", None, false),
            ("max", None, false),
        ]),
    ),
    ("midinote", Some(&[("midi", None, false)])),
    ("src", None),
    ("output", None),
    ("fork", Some(&[("in", None, false), ("times", None, false)])),
    (
        "perc",
        Some(&[("gate", None, false), ("release", None, false)]),
    ),
    (
        "hpf",
        Some(&[
            ("in", None, false),
            ("cutoff", None, false),
            ("reso", None, false),
        ]),
    ),
    (
        "lpf",
        Some(&[
            ("in", None, false),
            ("cutoff", None, false),
            ("reso", None, false),
        ]),
    ),
    (
        "bpf",
        Some(&[
            ("in", None, false),
            ("cutoff", None, false),
            ("reso", None, false),
        ]),
    ),
    ("lfnoise", Some(&[("freq", None, false)])),
    ("bipolar", Some(&[("in", None, false)])),
    ("unipolar", Some(&[("in", None, false)])),
    ("pan", Some(&[("in", None, false), ("pos", None, false)])),
    (
        "pick",
        Some(&[("index", None, false), ("inputs", None, true)]),
    ),
    (
        "clip",
        Some(&[
            ("input", None, false),
            ("lo", None, false),
            ("hi", None, false),
        ]),
    ),
    (
        "trig",
        Some(&[
            ("input", Some(0.0), false),
            ("lo", Some(-1.0), false),
            ("hi", Some(1.0), false),
        ]),
    ),
    (
        "qf",
        Some(&[
            ("input", Some(0.0), false),
            ("type", Some(0.0), false),
            ("freq", Some(500.0), false),
            ("q", Some(1.0), false),
            ("gain", Some(1.0), false),
        ]),
    ),
    (
        "qlpf",
        Some(&[
            ("input", None, false),
            ("freq", None, false),
            ("q", None, false),
        ]),
    ),
    (
        "qhpf",
        Some(&[
            ("input", None, false),
            ("freq", None, false),
            ("q", None, false),
        ]),
    ),
    (
        "qbpf",
        Some(&[
            ("input", None, false),
            ("freq", None, false),
            ("q", None, false),
        ]),
    ),
    (
        "qnf",
        Some(&[
            ("input", None, false),
            ("freq", None, false),
            ("q", None, false),
        ]),
    ),
    (
        "qapf",
        Some(&[
            ("input", None, false),
            ("freq", None, false),
            ("q", None, false),
        ]),
    ),
    (
        "split",
        Some(&[("input", None, false), ("fn", None, false)]),
    ),
    (
        "mix",
        Some(&[("in", None, false), ("channels", Some(1.0), false)]),
    ),
    ("mouseX", Some(&[])),
    ("mouseY", Some(&[])),
    ("scope", None),
];

pub(super) fn install<'js>(ctx: &Ctx<'js>, registry: &Object<'js>) -> Result<()> {
    for (name, inlets) in SCHEMAS {
        let schema = Object::new(ctx.clone())?;
        if let Some(inlets) = inlets {
            let inputs = Array::new(ctx.clone())?;
            for (index, (name, default, dynamic)) in inlets.iter().enumerate() {
                let inlet = Object::new(ctx.clone())?;
                inlet.set("name", *name)?;
                if let Some(default) = default {
                    inlet.set("default", *default)?;
                }
                if *dynamic {
                    inlet.set("dynamic", true)?;
                }
                inputs.set(index, inlet)?;
            }
            schema.set("ins", inputs)?;
        }
        registry.set(*name, schema)?;
    }
    Ok(())
}
