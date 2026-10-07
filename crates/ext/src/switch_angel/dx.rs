use rustel_core::reference::{ReferenceEntry, ReferenceParam};
use rustel_core::{Pattern, Value};

use super::ORIGIN;
use super::shared::{apply_control, pattern_argument, source};
use crate::{CallableSurface, PatternCallable, PatternCallableBehavior};

pub(super) const REFERENCE: ReferenceEntry = simple_reference!(
    "DX",
    "make a compact FM sine voice",
    "A ready-made sine FM voice: the envelope argument drives fmenv, the other two set fm and fmh, and fmdecay is fixed at .2. All arguments are patterned, so an LFO or slider can steer the timbre; give it notes afterwards.",
    params: [
        ReferenceParam {
            name: "env",
            r#type: "number | Pattern",
            description: "FM envelope depth written to fmenv; defaults to 8.",
        },
        ReferenceParam {
            name: "fm",
            r#type: "number | Pattern",
            description: "FM modulation amount; defaults to 2.",
        },
        ReferenceParam {
            name: "harm",
            r#type: "number | Pattern",
            description: "FM harmonic ratio written to fmh; defaults to 2.",
        },
    ],
    examples: ["DX().note(\"c2 e2 g2\")", "DX(4, 1, 3).note(\"c2 e2 g2\")"],
    "preset"
);

fn apply(args: &[Pattern], _receiver: Option<&Pattern>) -> Pattern {
    let env = pattern_argument(args, 0, Value::F64(8.0));
    let fm = pattern_argument(args, 1, Value::F64(2.0));
    let harm = pattern_argument(args, 2, Value::F64(2.0));
    let mut output = source("sine");
    output = apply_control(&output, "fm", fm);
    output = apply_control(&output, "fmenv", env);
    output = apply_control(&output, "fmh", harm);
    apply_control(&output, "fmdecay", rustel_core::pure(Value::F64(0.2)))
}

pub(super) const CALLABLE: PatternCallable = PatternCallable {
    names: &["DX"],
    arity: 0,
    origin: ORIGIN,
    surface: CallableSurface::GLOBAL,
    behavior: PatternCallableBehavior::Stateless(apply),
};
