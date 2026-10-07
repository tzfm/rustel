use rustel_core::compose::ComposeOp;
use rustel_core::reference::{ReferenceEntry, ReferenceParam};
use rustel_core::{Pattern, Value};

use super::ORIGIN;
use super::shared::{
    apply_control, apply_distortion, pattern_argument, pattern_binary, source_pattern,
};
use crate::{CallableSurface, PatternCallable, PatternCallableBehavior};

pub(super) const REFERENCE: ReferenceEntry = simple_reference!(
    "roller2",
    "make Switch Angel's layered roller",
    "The authored layered preset: a stab, a supersaw and white noise sounding together with random detune, staggered begin offsets, room at half, and an acid low-pass - cutoff 100, resonance 2 - opened by the envelope argument times nine.",
    params: [ReferenceParam {
        name: "envelope",
        r#type: "number | Pattern",
        description: "how far the acid envelope opens; defaults to 0.5.",
    }],
    examples: ["roller2()"],
    "preset"
);

fn apply(args: &[Pattern], _receiver: Option<&Pattern>) -> Pattern {
    let envelope = pattern_argument(args, 0, Value::F64(0.5));
    let values = super::shared::static_mini("bstab:1:.6,supersaw:1:.6,white:0:.3");
    let mut output = source_pattern(values);
    output = apply_control(&output, "detune", rustel_core::signal::rand());
    output = apply_control(&output, "begin", super::shared::static_mini("<0 .02>/4"));
    for (name, value) in [
        ("room", 0.5),
        ("resonance", 0.0),
        ("postgain", 1.2),
        ("sustain", 0.45),
        ("decay", 0.2),
    ] {
        output = apply_control(&output, name, rustel_core::pure(Value::F64(value)));
    }
    let output = apply_distortion(&output, "diode", Value::F64(2.5), Value::F64(0.6));
    let output = apply_control(&output, "cutoff", rustel_core::pure(Value::F64(100.0)));
    let output = apply_control(
        &output,
        "lpenv",
        pattern_binary(
            &envelope,
            &rustel_core::pure(Value::F64(9.0)),
            ComposeOp::Mul,
        ),
    );
    let output = apply_control(&output, "lpsustain", rustel_core::pure(Value::F64(0.2)));
    let output = apply_control(&output, "lpdecay", rustel_core::pure(Value::F64(0.12)));
    apply_control(&output, "resonance", rustel_core::pure(Value::F64(2.0)))
}

pub(super) const CALLABLE: PatternCallable = PatternCallable {
    names: &["roller2"],
    arity: 0,
    origin: ORIGIN,
    surface: CallableSurface::GLOBAL,
    behavior: PatternCallableBehavior::Stateless(apply),
};
