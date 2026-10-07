use rustel_core::reference::{ReferenceEntry, ReferenceParam};
use rustel_core::{Pattern, Value};

use super::ORIGIN;
use super::shared::{apply_control, pattern_argument};
use crate::{CallableSurface, PatternCallable, PatternCallableBehavior};

pub(super) const REFERENCE: ReferenceEntry = simple_reference!(
    "col",
    "choose one of seven display colors",
    "Maps a patterned selector across Switch Angel's seven-color hex palette (#50d1f8, #E91E63, #8EDF5F, #995CD0, #EC7744, #5549B7, #F9E03D), clamped at the ends, and writes the chosen color into the color control. With no selector no events survive the pick.",
    params: [ReferenceParam {
        name: "selector",
        r#type: "number | Pattern",
        description: "which palette entry to pick; clamped into the seven colors. Omitting it leaves the pattern empty.",
    }],
    examples: ["note(\"c e g\").s(\"sawtooth\").col(\"<0 1 2 3>\")"],
    "visual"
);

fn static_lookup(values: &[&str]) -> rustel_core::PickLookup {
    rustel_core::PickLookup::Array {
        enumerable_len: values.len(),
        length: values.len(),
        entries: values
            .iter()
            .enumerate()
            .map(|(index, value)| (index, rustel_core::pure(Value::Str((*value).into()))))
            .collect(),
    }
}

pub(super) fn color_pattern(pattern: &Pattern, selector: Pattern) -> Pattern {
    const COLORS: &[&str] = &[
        "#50d1f8", "#E91E63", "#8EDF5F", "#995CD0", "#EC7744", "#5549B7", "#F9E03D",
    ];
    let color = rustel_core::pick(
        selector,
        static_lookup(COLORS),
        rustel_core::PickIndexMode::Clamp,
        rustel_core::JoinMode::Inner,
    );
    apply_control(pattern, "color", color)
}

fn apply(args: &[Pattern], receiver: Option<&Pattern>) -> Pattern {
    let Some(receiver) = receiver else {
        return rustel_core::silence();
    };
    color_pattern(receiver, pattern_argument(args, 0, Value::Undefined))
}

pub(super) const CALLABLE: PatternCallable = PatternCallable {
    names: &["col"],
    arity: 1,
    origin: ORIGIN,
    surface: CallableSurface::METHOD,
    behavior: PatternCallableBehavior::Stateless(apply),
};
