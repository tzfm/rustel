use rustel_core::compose::ComposeOp;
use rustel_core::ops::PatOps;
use rustel_core::reference::{ReferenceEntry, ReferenceParam};
use rustel_core::{Pattern, Value};

use super::ORIGIN;
use super::shared::{number, pattern_argument, pattern_binary, source_control_pattern};
use crate::{CallableSurface, PatternCallable, PatternCallableBehavior};

pub(super) const REFERENCE: ReferenceEntry = simple_reference!(
    "chrd",
    "expand degree and variation into a chord",
    "Selects one of Switch Angel's 76 chord shapes and transposes it by the supplied scale degree, emitting n values you can hand a sound. A bare degree uses shape 0; a [degree, variation] pair picks the shape, clamping to the last shape past 75. A negative variation selects nothing and the event goes silent.",
    params: [ReferenceParam {
        name: "chords",
        r#type: "number | Pattern | array",
        description: "a scale degree, or a [degree, variation] pair where variation (default 0) picks the chord shape.",
    }],
    examples: ["chrd(\"<0 3 4>*1/2\").s(\"sawtooth\")"],
    "tonal"
);

const CHORD_SHAPES: &[&str] = &[
    "0,4",
    "0,2,4",
    "-7,0,2,4,7",
    "-7,0,2,3,7",
    "0,2,4,6",
    "0,2,3,6",
    "0,4,7,9",
    "0,4,7,8",
    "0,4,6,9",
    "0,2,6,9",
    "0,2,6,10",
    "0,2,4,6,8",
    "-7,0,2,6,9",
    "0,4,7,9,13",
    "0,4,8,9,13",
    "0,2,7,8,11",
    "0,2,8,9,11",
    "-7,0,2,3,7",
    "0,3,4",
    "0,1,4",
    "0,2,3,4",
    "0,2,4,8",
    "0,2,4,9",
    "0,4,6,8",
    "0,2,5,9",
    "0,4,7,11",
    "0,7,9,11",
    "0,2,7,11",
    "-7,0,4,7",
    "-7,0,4,6",
    "-7,0,2,9",
    "-7,0,4,9",
    "-7,0,2,4,9",
    "-7,0,2,4,6,9",
    "-7,0,2,4,8",
    "-7,0,3,7,10",
    "0,2,4,8,11",
    "0,2,4,6,11",
    "0,2,4,9,11",
    "0,2,6,9,11",
    "0,4,6,8,11",
    "0,2,3,7,11",
    "0,1,4,8,11",
    "0,1,2,4,11",
    "0,2,4,5,9",
    "0,4,5,9,11",
    "0,1,4,7",
    "0,1,3,7",
    "0,4,5,7",
    "0,5,7",
    "0,5,7,10",
    "0,5,10",
    "-7,0,5,7",
    "-7,0,1,5",
    "-7,-3,0,5",
    "0,1,5,8",
    "0,3,5,8",
    "0,2,4,7",
    "0,4,7,11,14",
    "-7,0,2,4,7,11",
    "0,2,4,6,9,11",
    "0,7,11,14",
    "0,2,9,11,14",
    "-12,0,4,7,11",
    "-12,-7,0,4,7",
    "0,1,2",
    "0,1,2,4",
    "0,2,3,5,8",
    "0,4,5,8,9",
    "0,2,5,7,11",
    "0,3,7,10,14",
    "0,2,4,7,9,11",
    "0,1,4,6,11",
    "0,2,6,7,11",
    "0,4,8,11,14",
];

fn degree_and_variation(value: &Value) -> (f64, f64) {
    match rustel_core::materialize_js_value(value) {
        Value::List(values) => (
            values.first().map(number).unwrap_or(f64::NAN),
            values.get(1).map(number).unwrap_or(0.0),
        ),
        value => (number(&value), 0.0),
    }
}

fn apply(args: &[Pattern], _receiver: Option<&Pattern>) -> Pattern {
    let chords = pattern_argument(args, 0, Value::Undefined);
    chords.outer_bind(|value| {
        let (degree, variation) = degree_and_variation(value);
        let index = variation.min((CHORD_SHAPES.len() - 1) as f64) as isize;
        let Some(shape) = usize::try_from(index)
            .ok()
            .and_then(|index| CHORD_SHAPES.get(index))
        else {
            return rustel_core::silence();
        };
        let notes = super::shared::static_mini(shape);
        let notes = source_control_pattern("n", notes);
        let degree = source_control_pattern("n", rustel_core::pure(Value::F64(degree)));
        pattern_binary(&notes, &degree, ComposeOp::Add)
    })
}

pub(super) const CALLABLE: PatternCallable = PatternCallable {
    names: &["chrd"],
    arity: 1,
    origin: ORIGIN,
    surface: CallableSurface::GLOBAL,
    behavior: PatternCallableBehavior::Stateless(apply),
};
