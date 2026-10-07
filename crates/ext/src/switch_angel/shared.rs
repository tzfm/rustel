use rustel_core::compose::{Alignment, ComposeOp};
use rustel_core::controls;
use rustel_core::ops::PatOps;
use rustel_core::{Pattern, Value};
use rustel_fraction::Fraction;

pub(super) fn value(args: &[Value], index: usize) -> Value {
    args.get(index).cloned().unwrap_or(Value::Undefined)
}

pub(super) fn number(value: &Value) -> f64 {
    rustel_core::util::parse_numeral(value).unwrap_or(f64::NAN)
}

pub(super) fn fraction(value: &Value) -> Fraction {
    rustel_core::register::value_to_fraction(value).unwrap_or(Fraction::ZERO)
}

pub(super) fn scalar<P: PatOps>(value: impl Into<Value>) -> P {
    P::pat_pure(value.into())
}

pub(super) fn binary<P: PatOps>(left: &P, right: &P, operation: ComposeOp) -> P {
    rustel_core::compose::compose(
        left,
        right,
        operation,
        rustel_core::compose::default_alignment(),
    )
}

pub(super) fn binary_in<P: PatOps>(left: &P, right: &P, operation: ComposeOp) -> P {
    rustel_core::compose::compose(left, right, operation, Alignment::In)
}

pub(super) fn control<P: PatOps>(pattern: &P, name: &str, value: &P) -> P {
    controls::apply_pattern(name, pattern, value)
}

pub(super) fn signal<P: PatOps>(pattern: Pattern) -> P {
    P::from_pure_view(
        pattern
            .as_pure_pattern()
            .expect("native signals contain no callbacks"),
    )
}

/// A preset written inside the extension, parsed as mini-notation with its
/// source spans removed.
///
/// The parser locates every leaf relative to the text it was given. A
/// score's own strings are placed at their offset in the score, but a
/// preset's text is nowhere in the score, so a span it carried would land
/// on whatever sits at those offsets - the `t` of `trancearp`, a stray
/// `0` - and be highlighted there while the preset's steps play.
pub(super) fn static_mini(source: &str) -> Pattern {
    rustel_mini::mini(source)
        .unwrap_or_else(|error| panic!("extension mini `{source}` failed: {error}"))
        .map_haps_native(|hap| {
            let mut hap = hap.clone();
            hap.context.clear();
            Some(hap)
        })
}

pub(super) fn mini<P: PatOps>(source: &str) -> P {
    P::from_pure_view(
        static_mini(source)
            .as_pure_pattern()
            .expect("static extension mini contains no callbacks"),
    )
}

pub(super) fn clip<P: PatOps>(pattern: &P, amount: f64) -> P {
    control(pattern, "clip", &scalar(Value::F64(amount)))
}

pub(super) fn pattern_argument(args: &[Pattern], index: usize, default: Value) -> Pattern {
    args.get(index)
        .cloned()
        .unwrap_or_else(|| rustel_core::pure(default))
}

/// Yields each held value where `keeps` applied to it and the bound is
/// truthy, and the bound otherwise, converting neither. A pair that does not
/// compare, such as a word and a number, yields the bound, as in her prebake.
pub(super) fn keep_or_bound(
    args: &[Pattern],
    receiver: Option<&Pattern>,
    keeps: ComposeOp,
) -> Pattern {
    let Some(receiver) = receiver else {
        return rustel_core::silence();
    };
    let bound = pattern_argument(args, 0, Value::Undefined);
    receiver.app_left_with(bound, move |held, bound| {
        if keeps.apply_scalar(held, bound).js_truthy() {
            held.clone()
        } else {
            bound.clone()
        }
    })
}

pub(super) fn source(name: &str) -> Pattern {
    rustel_core::pure(Value::object([("s".into(), Value::Str(name.into()))]))
}

pub(super) fn source_pattern(values: Pattern) -> Pattern {
    rustel_core::controls::default_control_registry()
        .get("s")
        .expect("the upstream control registry contains s")
        .pattern(&values)
}

pub(super) fn source_control_pattern(name: &str, values: Pattern) -> Pattern {
    rustel_core::controls::default_control_registry()
        .get(name)
        .unwrap_or_else(|| panic!("the upstream control registry contains {name}"))
        .pattern(&values)
}

pub(super) fn apply_control(pattern: &Pattern, name: &str, value: Pattern) -> Pattern {
    controls::apply_pattern(name, pattern, &value)
}

/// A named distortion shape, the way `.diode(x)` writes it: the algorithm is
/// the third slot of the `distort` control, not a control of its own.
pub(super) fn apply_distortion(
    pattern: &Pattern,
    algorithm: &str,
    amount: Value,
    shape: Value,
) -> Pattern {
    apply_control(
        pattern,
        "distort",
        rustel_core::pure(Value::List(vec![
            amount,
            shape,
            Value::Str(algorithm.into()),
        ])),
    )
}

pub(super) fn pattern_binary(left: &Pattern, right: &Pattern, operation: ComposeOp) -> Pattern {
    rustel_core::compose::compose(left, right, operation, Alignment::In)
}

pub(super) fn value_as_pattern(value: &Value) -> Pattern {
    match value {
        Value::Pattern(pattern) => pattern.pattern().clone(),
        value => rustel_core::pure(value.clone()),
    }
}

pub(super) fn pure_list(pattern: &Pattern) -> Option<Vec<Value>> {
    match pattern.as_pure()? {
        Value::List(values) => Some(values),
        _ => None,
    }
}

pub(super) fn register_func_result(pattern: Pattern, receiver: Option<&Pattern>) -> Pattern {
    match receiver {
        Some(receiver) => receiver.set(&pattern),
        None => pattern,
    }
}
