use rustel_core::reference::{ReferenceEntry, ReferenceParam};
use rustel_core::{Pattern, Value};

use super::ORIGIN;
use super::dly;
use super::shared::{apply_control, apply_distortion, pattern_argument, source};
use crate::{CallableSurface, PatternCallable, PatternCallableBehavior};

pub(super) const REFERENCE: ReferenceEntry = simple_reference!(
    "roller",
    "make Switch Angel's wavetable roller",
    "The authored wavetable roller: a wt_digi source with the wavetable position from the argument, wtenv 0, wtdecay .2, warp 0 with warpmode 7, warpenv .5 and warpdecay .1, decay .2, no resonance, room .7 at roomsize 4, diode 1, an acid low-pass (cutoff 100, lpenv 3.96, lpsustain .2, lpdecay .12), and dly at .8 on top.",
    params: [ReferenceParam {
        name: "wt",
        r#type: "number | Pattern",
        description: "wavetable position; defaults to 0.",
    }],
    examples: ["roller()"],
    "preset"
);

fn apply(args: &[Pattern], _receiver: Option<&Pattern>) -> Pattern {
    let wt = pattern_argument(args, 0, Value::F64(0.0));
    let mut output = source("wt_digi");
    output = apply_control(&output, "wt", wt);
    for (name, value) in [
        ("wtenv", 0.0),
        ("wtdecay", 0.2),
        ("warp", 0.0),
        ("warpmode", 7.0),
        ("warpenv", 0.5),
        ("warpdecay", 0.1),
        ("decay", 0.2),
        ("resonance", 0.0),
        ("room", 0.7),
        ("roomsize", 4.0),
        ("cutoff", 100.0),
        ("lpenv", 3.96),
        ("lpsustain", 0.2),
        ("lpdecay", 0.12),
    ] {
        output = apply_control(&output, name, rustel_core::pure(Value::F64(value)));
    }
    output = apply_distortion(&output, "diode", Value::F64(1.0), Value::F64(1.0));
    dly::apply(&rustel_core::pure(Value::F64(0.8)), &output)
}

pub(super) const CALLABLE: PatternCallable = PatternCallable {
    names: &["roller"],
    arity: 0,
    origin: ORIGIN,
    surface: CallableSurface::GLOBAL,
    behavior: PatternCallableBehavior::Stateless(apply),
};
