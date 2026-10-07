use rustel_core::combinators as c;
use rustel_core::compose::{Alignment, ComposeOp};
use rustel_core::ops::PatOps;
use rustel_core::reference::{ReferenceEntry, ReferenceParam};
use rustel_core::{Pattern, Value};

use super::ORIGIN;
use super::shared::{apply_control, pattern_argument, source_control_pattern};
use crate::{CallableSurface, PatternCallable, PatternCallableBehavior, PatternState};

pub(super) const STATE_KEY: &str = "switch_angel.scale";

pub(super) const STATE: PatternState = PatternState {
    key: STATE_KEY,
    initial,
};

pub(super) const SET_REFERENCE: ReferenceEntry = simple_reference!(
    "setScale",
    "replace the shared scale pattern",
    "Replaces the realm-local scale pattern used by sc and nsc, and returns silence. The scale starts life as 'e:minor'. Single-quote a 'root:mode' string to store it verbatim - double quotes are parsed as mini-notation.",
    params: [ReferenceParam {
        name: "scale",
        r#type: "scale",
        description: "the scale to store, usually a 'root:mode' string such as 'c:minor'.",
    }],
    examples: ["setScale('c:minor')"],
    "tonal"
);

pub(super) const SC_REFERENCE: ReferenceEntry = simple_reference!(
    "sc",
    "apply the shared scale",
    "Scales the receiver with the shared scale set by setScale. An optional mode argument replaces the scale's mode while keeping its root.",
    params: [ReferenceParam {
        name: "mode",
        r#type: "string | Pattern",
        description: "optional mode that overrides the shared scale's mode; the root still comes from the shared scale.",
    }],
    examples: ["n(\"0 2 4 6\").sc().s(\"sawtooth\")"],
    "tonal"
);

pub(super) const NSC_REFERENCE: ReferenceEntry = simple_reference!(
    "nsc",
    "set degrees in the shared scale",
    "Writes patterned scale degrees onto the receiver through the shared scale, then sets the octave - a quick way to play degrees without choosing notes.",
    params: [
        ReferenceParam {
            name: "degrees",
            r#type: "number | Pattern",
            description: "scale degrees to write; defaults to 0.",
        },
        ReferenceParam {
            name: "octaves",
            r#type: "number | Pattern",
            description: "octave the degrees land in; defaults to 3.",
        },
    ],
    examples: ["note(\"c e g\").nsc(\"<0 2 4>\").s(\"sawtooth\")"],
    "tonal"
);

fn initial() -> Pattern {
    rustel_core::pure(Value::Str("e:minor".into()))
}

fn apply_scale(pattern: &Pattern, state: &Pattern, mode: Option<Pattern>) -> Pattern {
    let pattern = pattern.clone();
    let mode = mode.clone();
    state.inner_bind(move |scale| {
        let pattern = pattern.clone();
        let root = match rustel_core::materialize_js_value(scale) {
            Value::List(values) => values.first().cloned().unwrap_or(Value::Undefined),
            Value::Str(text) => Value::Str(text.split(':').next().unwrap_or("").into()),
            value => value,
        };
        match &mode {
            None => c::scale(&pattern, scale.clone()),
            Some(mode) => {
                let root = root.clone();
                let pattern = pattern.clone();
                mode.inner_bind(move |mode| {
                    c::scale(&pattern, Value::List(vec![root.clone(), mode.clone()]))
                })
            }
        }
    })
}

fn sc(state: &Pattern, args: &[Pattern], receiver: Option<&Pattern>) -> Pattern {
    let Some(receiver) = receiver else {
        return rustel_core::silence();
    };
    apply_scale(receiver, state, args.first().cloned())
}

fn nsc(state: &Pattern, args: &[Pattern], receiver: Option<&Pattern>) -> Pattern {
    let Some(receiver) = receiver else {
        return rustel_core::silence();
    };
    let degrees = pattern_argument(args, 0, Value::F64(0.0));
    let octaves = pattern_argument(args, 1, Value::F64(3.0));
    let notes = source_control_pattern("n", degrees);
    let notes = apply_scale(&notes, state, None);
    let output = rustel_core::compose::compose(receiver, &notes, ComposeOp::Set, Alignment::Out);
    apply_control(&output, "octave", octaves)
}

pub(super) const CALLABLES: &[PatternCallable] = &[
    PatternCallable {
        names: &["setScale"],
        arity: 1,
        origin: ORIGIN,
        surface: CallableSurface::GLOBAL,
        behavior: PatternCallableBehavior::WriteState { key: STATE_KEY },
    },
    PatternCallable {
        names: &["sc"],
        arity: 1,
        origin: ORIGIN,
        surface: CallableSurface::METHOD,
        behavior: PatternCallableBehavior::ReadState {
            key: STATE_KEY,
            call: sc,
        },
    },
    PatternCallable {
        names: &["nsc"],
        arity: 0,
        origin: ORIGIN,
        surface: CallableSurface::METHOD,
        behavior: PatternCallableBehavior::ReadState {
            key: STATE_KEY,
            call: nsc,
        },
    },
];
