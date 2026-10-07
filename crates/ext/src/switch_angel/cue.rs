use rustel_core::ops::PatOps;
use rustel_core::reference::{ReferenceEntry, ReferenceParam};
use rustel_core::{Pattern, Value};

use super::col::color_pattern;
use super::shared::pattern_argument;
use super::{ORIGIN, strictly_equal};
use crate::{CallableSurface, PatternCallable, PatternCallableBehavior, PatternState};

pub(super) const STATE_KEY: &str = "switch_angel.cue";

pub(super) const STATE: PatternState = PatternState {
    key: STATE_KEY,
    initial,
};

pub(super) const CUE_REFERENCE: ReferenceEntry = simple_reference!(
    "cue",
    "select or gate by the shared cue",
    "Called bare, cue picks among its arguments with the realm-local cue (initially the string '0'), clamped to the last argument. As a method it gates: where the cue matches, the receiver sounds - optionally through a transformer - and elsewhere it is silent, unless a transformer is given, in which case non-matching spans pass through. Matching events carry the cue value as their color. Values compare strictly, so a mini-notation number does not match a string cue; single-quote the cue ('0') to match the initial state.",
    params: [
        ReferenceParam {
            name: "choices",
            r#type: "Pattern (global form)",
            description: "the patterns to choose from; the current cue indexes them, clamped to the last one.",
        },
        ReferenceParam {
            name: "cues",
            r#type: "value | Pattern (method form)",
            description: "the cue value or values that let the receiver through, compared strictly against the current cue.",
        },
        ReferenceParam {
            name: "cb",
            r#type: "function (method form, optional)",
            description: "optional transformer applied while the cue matches; when present, non-matching spans pass through unchanged.",
        },
    ],
    examples: [
        "cue(note(\"c e g\").s(\"sawtooth\"), note(\"d f a\").s(\"square\"))",
        "note(\"c e g\").s(\"sawtooth\").cue('0')",
    ],
    "arrangement"
);

pub(super) const GET_REFERENCE: ReferenceEntry = simple_reference!(
    "getCue",
    "read the shared cue pattern",
    "Returns the realm-local cue pattern used by cue and oncue - the string '0' until setCue replaces it.",
    params: [],
    examples: ["getCue()"],
    "arrangement"
);

pub(super) const SET_REFERENCE: ReferenceEntry = simple_reference!(
    "setCue",
    "replace the shared cue pattern",
    "Replaces the realm-local cue pattern read by cue and oncue, and returns silence.",
    params: [ReferenceParam {
        name: "cue",
        r#type: "Pattern",
        description: "the pattern to store as the shared cue; double-quoted strings arrive as mini-notation, single-quoted ones stay literal.",
    }],
    examples: ["setCue(\"1\")"],
    "arrangement"
);

pub(super) const ONCUE_REFERENCE: ReferenceEntry = simple_reference!(
    "oncue",
    "transform only on selected cues",
    "Leaves the receiver alone while the cue does not match; where it does, applies the transformer on top of the cue's color. The transformer is required - a missing one raises 'cb is not a function' at query time.",
    params: [
        ReferenceParam {
            name: "cues",
            r#type: "value | Pattern",
            description: "the cue value or values to react to, compared strictly against the current cue.",
        },
        ReferenceParam {
            name: "cb",
            r#type: "function",
            description: "transformer applied to the receiver on matching cues; required.",
        },
    ],
    examples: ["note(\"c e g\").s(\"sawtooth\").oncue('0', x=>x.fast(2))"],
    "arrangement"
);

fn initial() -> Pattern {
    rustel_core::pure(Value::Str("0".into()))
}

fn contains(collection: &Value, wanted: &Value) -> bool {
    match rustel_core::materialize_js_value(collection) {
        Value::List(values) => values
            .iter()
            .any(|candidate| strictly_equal(candidate, wanted)),
        value => strictly_equal(&value, wanted),
    }
}

fn callback(args: &[Pattern], index: usize) -> Option<rustel_core::value::FunctionRef> {
    args.get(index)
        .and_then(Pattern::as_pure)
        .and_then(|value| value.as_function().cloned())
}

fn method(current: &Pattern, args: &[Pattern], receiver: Option<&Pattern>) -> Pattern {
    let Some(receiver) = receiver else {
        return rustel_core::silence();
    };
    let cues = pattern_argument(args, 0, Value::Undefined);
    let callback = callback(args, 1);
    let receiver = receiver.clone();
    current.inner_bind(move |current| {
        let current = current.clone();
        let receiver = receiver.clone();
        let callback = callback.clone();
        cues.inner_bind(move |cues| {
            if contains(cues, &current) {
                let selected = callback.as_ref().map_or_else(
                    || receiver.clone(),
                    |function| function.apply(receiver.clone()),
                );
                color_pattern(&selected, rustel_core::pure(current.clone()))
            } else if callback.is_none() {
                rustel_core::silence()
            } else {
                color_pattern(&receiver, rustel_core::pure(current.clone()))
            }
        })
    })
}

fn oncue(current: &Pattern, args: &[Pattern], receiver: Option<&Pattern>) -> Pattern {
    let Some(receiver) = receiver else {
        return rustel_core::silence();
    };
    let cues = pattern_argument(args, 0, Value::Undefined);
    let callback = callback(args, 1);
    let receiver = receiver.clone();
    current.inner_bind(move |current| {
        let current = current.clone();
        let receiver = receiver.clone();
        let callback = callback.clone();
        cues.inner_bind(move |cues| {
            if !contains(cues, &current) {
                return receiver.clone();
            }
            let colored = color_pattern(&receiver, rustel_core::pure(current.clone()));
            callback.as_ref().map_or_else(
                || rustel_core::query_error_pattern("cb is not a function"),
                |function| function.apply(colored),
            )
        })
    })
}

fn get(state: &Pattern, _args: &[Pattern], _receiver: Option<&Pattern>) -> Pattern {
    state.clone()
}

fn pick(state: &Pattern, args: &[Pattern], _receiver: Option<&Pattern>) -> Pattern {
    let lookup = rustel_core::PickLookup::Array {
        enumerable_len: args.len(),
        length: args.len(),
        entries: args.iter().cloned().enumerate().collect(),
    };
    rustel_core::pick(
        state.clone(),
        lookup,
        rustel_core::PickIndexMode::Clamp,
        rustel_core::JoinMode::Inner,
    )
}

pub(super) const CALLABLES: &[PatternCallable] = &[
    PatternCallable {
        names: &["cue"],
        arity: 0,
        origin: ORIGIN,
        surface: CallableSurface::GLOBAL,
        behavior: PatternCallableBehavior::ReadState {
            key: STATE_KEY,
            call: pick,
        },
    },
    PatternCallable {
        names: &["getCue"],
        arity: 0,
        origin: ORIGIN,
        surface: CallableSurface::GLOBAL,
        behavior: PatternCallableBehavior::ReadState {
            key: STATE_KEY,
            call: get,
        },
    },
    PatternCallable {
        names: &["setCue"],
        arity: 1,
        origin: ORIGIN,
        surface: CallableSurface::GLOBAL,
        behavior: PatternCallableBehavior::WriteState { key: STATE_KEY },
    },
    PatternCallable {
        names: &["cue"],
        arity: 2,
        origin: ORIGIN,
        surface: CallableSurface::METHOD,
        behavior: PatternCallableBehavior::ReadState {
            key: STATE_KEY,
            call: method,
        },
    },
    PatternCallable {
        names: &["oncue"],
        arity: 2,
        origin: ORIGIN,
        surface: CallableSurface::METHOD,
        behavior: PatternCallableBehavior::ReadState {
            key: STATE_KEY,
            call: oncue,
        },
    },
];
