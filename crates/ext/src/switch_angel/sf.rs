use rustel_core::combinators as c;
use rustel_core::compose::ComposeOp;
use rustel_core::ops::PatOps;
use rustel_core::reference::{ReferenceEntry, ReferenceParam};
use rustel_core::{Pattern, Value};

use super::ORIGIN;
use super::shared::{apply_control, fraction, pattern_argument, pattern_binary};
use crate::{CallableSurface, PatternCallable, PatternCallableBehavior};

pub(super) const REFERENCE: ReferenceEntry = simple_reference!(
    "sf",
    "old-school segmented sample stretch",
    "Stretches a sample across the given number of cycles: a segmented saw drives the scrub position, the result slows to the length asked for, and each slice sustains its full span. The scrub rides the native scrub combinator - begin, speed and clip on the sample - and a missing cycles argument leaves the pattern silent.",
    params: [
        ReferenceParam {
            name: "cycles",
            r#type: "number | Pattern",
            description: "how many cycles the stretch spans - required; without it the pattern is silent.",
        },
        ReferenceParam {
            name: "segments",
            r#type: "number | Pattern",
            description: "how many slices the stretch is cut into; defaults to 16.",
        },
    ],
    examples: ["s(\"cp:1\").sf(4)"],
    "samples"
);

fn apply(args: &[Pattern], receiver: Option<&Pattern>) -> Pattern {
    let Some(receiver) = receiver else {
        return rustel_core::silence();
    };
    let cycles = pattern_argument(args, 0, Value::Undefined);
    let segments = pattern_argument(args, 1, Value::F64(16.0));
    let rate = pattern_binary(&cycles, &segments, ComposeOp::Mul);
    let scrub = rate.inner_bind(|rate| c::segment(&rustel_core::signal::saw(), fraction(rate)));
    let output = c::scrub(receiver, &scrub);
    let output = cycles.inner_bind(move |cycles| output.slow(fraction(cycles)));
    apply_control(&output, "sustain", rustel_core::pure(Value::F64(1.0)))
}

pub(super) const CALLABLE: PatternCallable = PatternCallable {
    names: &["sf"],
    arity: 1,
    origin: ORIGIN,
    surface: CallableSurface::METHOD,
    behavior: PatternCallableBehavior::Stateless(apply),
};

#[cfg(test)]
mod tests {
    use super::*;
    use rustel_fraction::Fraction;

    #[test]
    fn sf_uses_the_native_scrub_combinator() {
        let receiver = rustel_core::pure(Value::object([("s".into(), Value::Str("bd".into()))]));
        let output = apply(&[rustel_core::pure(Value::F64(2.0))], Some(&receiver));
        let haps = output.query_arc_sorted(Fraction::ZERO, Fraction::ONE);

        assert!(!haps.is_empty());
        for hap in haps {
            let controls = hap.value.as_object().expect("sf event controls");
            assert!(controls.contains_key("begin"));
            assert_eq!(controls.get("clip"), Some(&Value::F64(1.0)));
            assert_eq!(controls.get("sustain"), Some(&Value::F64(1.0)));
        }
    }
}
