use rustel_core::combinators as c;
use rustel_core::ops::PatOps;
use rustel_core::reference::{ReferenceEntry, ReferenceParam};
use rustel_core::{Pattern, Value};
use rustel_fraction::Fraction;

use super::ORIGIN;
use super::shared::{
    apply_control, fraction, number, pattern_argument, register_func_result, source,
};
use crate::{CallableSurface, PatternCallable, PatternCallableBehavior};

pub(super) const REFERENCE: ReferenceEntry = simple_reference!(
    "noisehat",
    "make a modulated noise hat",
    "White noise segmented into steps, its decay riding a modulation pattern (a triangle sped up 4× by default) ranged between a minimum and maximum. Called bare it simply plays; as a method it replaces the receiver's values with the hat.",
    params: [
        ReferenceParam {
            name: "segments",
            r#type: "number | Pattern",
            description: "how many hat steps per cycle; defaults to 16.",
        },
        ReferenceParam {
            name: "modulation",
            r#type: "Pattern",
            description: "the shape that drives the decay; defaults to a triangle, sped up 4×.",
        },
        ReferenceParam {
            name: "min",
            r#type: "number | Pattern",
            description: "shortest decay in seconds; defaults to .05. It is the fourth positional argument - the third is ignored.",
        },
        ReferenceParam {
            name: "max",
            r#type: "number | Pattern",
            description: "longest decay in seconds; defaults to .12. It is the fifth positional argument.",
        },
    ],
    examples: ["noisehat()", "noisehat(32)"],
    "preset"
);

fn apply(args: &[Pattern], receiver: Option<&Pattern>) -> Pattern {
    let segments = pattern_argument(args, 0, Value::F64(16.0));
    let modulation = args
        .get(1)
        .cloned()
        .unwrap_or_else(rustel_core::signal::tri);
    let min = pattern_argument(args, 3, Value::F64(0.05));
    let max = pattern_argument(args, 4, Value::F64(0.12));
    let pattern = segments.inner_bind(|segments| c::segment(&source("white"), fraction(segments)));
    let modulation = modulation.fast(Fraction::int(4));
    let range = min.inner_bind(move |min| {
        let modulation = modulation.clone();
        let min = number(min);
        max.inner_bind(move |max| c::range(&modulation, min, number(max)))
    });
    register_func_result(apply_control(&pattern, "decay", range), receiver)
}

pub(super) const CALLABLE: PatternCallable = PatternCallable {
    names: &["noisehat"],
    arity: 0,
    origin: ORIGIN,
    surface: CallableSurface::BOTH,
    behavior: PatternCallableBehavior::Stateless(apply),
};
