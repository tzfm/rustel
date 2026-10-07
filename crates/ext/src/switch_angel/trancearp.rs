use rustel_core::combinators as c;
use rustel_core::reference::{ReferenceEntry, ReferenceParam};
use rustel_core::{Pattern, Value};

use super::ORIGIN;
use super::shared::{pattern_argument, static_mini};
use crate::{CallableSurface, PatternCallable, PatternCallableBehavior};

pub(super) const REFERENCE: ReferenceEntry = simple_reference!(
    "trancearp",
    "build a patterned sixteen-step trance arpeggio",
    "Sixteen-step trance arpeggio: a reset preset (a number 0-5) chooses a forward/backward index shape, the supplied notes are indexed with wraparound, and a rhythm preset (0-2, or your own structure) gates the result. Give the notes as an array so they can be picked from",
    params: [
        ReferenceParam {
            name: "notes",
            r#type: "array | Pattern",
            description: "the notes to arpeggiate",
        },
        ReferenceParam {
            name: "reset",
            r#type: "number | Pattern",
            description: "restart shape: 0-5 pick the presets. Defaults to 0.",
        },
        ReferenceParam {
            name: "rhythm",
            r#type: "number | Pattern",
            description: "gate rhythm: 0-2 pick the presets. Defaults to 0.",
        },
    ],
    examples: [
        "note(trancearp(['c','e','g','b'], 0, 0)).s(\"sawtooth\")",
        "n(trancearp([0, 2, 4, 7], 3, 1)).scale(\"g:minor\").s(\"sawtooth\")"
    ],
    "tonal",
    "rhythm"
);

fn pattern_lookup(patterns: impl IntoIterator<Item = Pattern>) -> rustel_core::PickLookup {
    let entries = patterns.into_iter().enumerate().collect::<Vec<_>>();
    rustel_core::PickLookup::Array {
        enumerable_len: entries.len(),
        length: entries.len(),
        entries,
    }
}

fn preset_if_number(pattern: Pattern, presets: &[&str]) -> Pattern {
    if !pattern
        .as_pure()
        .is_some_and(|value| matches!(value, Value::F64(_)))
    {
        return pattern;
    }
    rustel_core::pick(
        pattern,
        pattern_lookup(presets.iter().map(|source| static_mini(source))),
        rustel_core::PickIndexMode::Remainder,
        rustel_core::JoinMode::Inner,
    )
}

fn apply(args: &[Pattern], _receiver: Option<&Pattern>) -> Pattern {
    const RESET_PRESETS: &[&str] = &[
        "F",
        "B",
        "<F@3 F@3 F@3 F@3 F@2 F@2>*16",
        "F B F B",
        "F@3 B@3 F F",
        "F@12 F@2 F@2",
    ];
    const RHYTHM_PRESETS: &[&str] = &[
        "x!16",
        "<1 1 _ 1 1 1 1 1>*16",
        "<1 1@2 1 1@2 1 1@2 1 1@2 1 1 1 1>*16",
    ];

    let notes = pattern_argument(args, 0, Value::Undefined);
    let reset = preset_if_number(pattern_argument(args, 1, Value::F64(0.0)), RESET_PRESETS);
    let rhythm = preset_if_number(pattern_argument(args, 2, Value::F64(0.0)), RHYTHM_PRESETS);
    let directions = rustel_core::PickLookup::Object {
        enumerable_len: 2,
        entries: vec![
            (
                "F".into(),
                static_mini("<0 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15>*16"),
            ),
            (
                "B".into(),
                static_mini("<15 14 13 12 11 10 9 8 7 6 5 4 3 2 1 0>*16"),
            ),
        ],
    };
    let indices = rustel_core::pick(
        reset,
        directions,
        rustel_core::PickIndexMode::Clamp,
        rustel_core::JoinMode::Restart,
    );
    let selected = rustel_core::pick_patternified(
        indices,
        notes,
        rustel_core::PickIndexMode::Remainder,
        rustel_core::JoinMode::Inner,
    );
    c::struct_with(&selected, &rhythm)
}

pub(super) const CALLABLE: PatternCallable = PatternCallable {
    names: &["trancearp"],
    arity: 1,
    origin: ORIGIN,
    surface: CallableSurface::GLOBAL,
    behavior: PatternCallableBehavior::Stateless(apply),
};
