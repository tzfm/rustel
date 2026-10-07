//! Numeric maps over the pattern's values.
//!
//! Documentation text from the Strudel project (AGPL-3.0-or-later),
//! https://strudel.cc, where an entry carries upstream's words; entries this
//! port wrote itself say so in their own words.

use super::{Registry, add, arg_number};
use crate::combinators as c;

const ROUND: crate::reference::ReferenceEntry = crate::reference::ReferenceEntry {
    name: "round",
    synonyms: &[],
    summary: "Assumes a numerical pattern.",
    description: "Assumes a numerical pattern. Returns a new pattern with all values rounded\nto the nearest integer.",
    params: &[],
    examples: &["n(\"0.5 1.5 2.5\".round()).scale(\"C:major\")"],
    tags: &["math"],
    no_autocomplete: false,
    deprecated: false,
    origin: "rustel",
};

const FLOOR: crate::reference::ReferenceEntry = crate::reference::ReferenceEntry {
    name: "floor",
    synonyms: &[],
    summary: "Assumes a numerical pattern.",
    description: "Assumes a numerical pattern. Returns a new pattern with all values set to\ntheir mathematical floor. E.g. `3.7` replaced with to `3`, and `-4.2`\nreplaced with `-5`.",
    params: &[],
    examples: &["note(\"42 42.1 42.5 43\".floor())"],
    tags: &["math"],
    no_autocomplete: false,
    deprecated: false,
    origin: "rustel",
};

const CEIL: crate::reference::ReferenceEntry = crate::reference::ReferenceEntry {
    name: "ceil",
    synonyms: &[],
    summary: "Assumes a numerical pattern.",
    description: "Assumes a numerical pattern. Returns a new pattern with all values set to\ntheir mathematical ceiling. E.g. `3.2` replaced with `4`, and `-4.2`\nreplaced with `-4`.",
    params: &[],
    examples: &["note(\"42 42.1 42.5 43\".ceil())"],
    tags: &["math"],
    no_autocomplete: false,
    deprecated: false,
    origin: "rustel",
};

const LOG2: crate::reference::ReferenceEntry = crate::reference::ReferenceEntry {
    name: "log2",
    synonyms: &[],
    summary: "map every number in the pattern through log base 2",
    description: "Each value is parsed as a numeral and replaced by its base-2 logarithm - 1, 2, 4, 8 become 0, 1, 2, 3. A value that is no numeral refuses the query and says so.",
    params: &[],
    examples: &["note(run(8).add(1).log2().mul(12)).s(\"triangle\")"],
    tags: &["value"],
    no_autocomplete: false,
    deprecated: false,
    origin: "rustel",
};

const TO_BIPOLAR: crate::reference::ReferenceEntry = crate::reference::ReferenceEntry {
    name: "toBipolar",
    synonyms: &[],
    summary: "Assumes a numerical pattern, containing unipolar values in the range 0 ..",
    description: "Assumes a numerical pattern, containing unipolar values in the range 0 ..\n1. Returns a new pattern with values scaled to the bipolar range -1 .. 1",
    params: &[],
    examples: &[],
    tags: &["math"],
    no_autocomplete: true,
    deprecated: false,
    origin: "rustel",
};

const FROM_BIPOLAR: crate::reference::ReferenceEntry = crate::reference::ReferenceEntry {
    name: "fromBipolar",
    synonyms: &[],
    summary: "Assumes a numerical pattern, containing bipolar values in the range -1 ..",
    description: "Assumes a numerical pattern, containing bipolar values in the range -1 .. 1\nReturns a new pattern with values scaled to the unipolar range 0 .. 1",
    params: &[],
    examples: &[],
    tags: &["math"],
    no_autocomplete: true,
    deprecated: false,
    origin: "rustel",
};

const RATIO: crate::reference::ReferenceEntry = crate::reference::ReferenceEntry {
    name: "ratio",
    synonyms: &[],
    summary: "Allows dividing numbers via list notation using \":\".",
    description: "Allows dividing numbers via list notation using \":\".\nReturns a new pattern with just numbers.",
    params: &[],
    examples: &["ratio(\"1, 5:4, 3:2\").mul(110)\n.freq().s(\"piano\")"],
    tags: &["math"],
    no_autocomplete: false,
    deprecated: false,
    origin: "rustel",
};

const INVERT: crate::reference::ReferenceEntry = crate::reference::ReferenceEntry {
    name: "invert",
    synonyms: &["inv"],
    summary: "Swaps 1s and 0s in a binary pattern.",
    description: "Swaps 1s and 0s in a binary pattern.",
    params: &[],
    examples: &["s(\"bd\").struct(\"1 0 0 1 0 0 1 0\".lastOf(4, invert))"],
    tags: &["temporal"],
    no_autocomplete: false,
    deprecated: false,
    origin: "rustel",
};

const RANGE: crate::reference::ReferenceEntry = crate::reference::ReferenceEntry {
    name: "range",
    synonyms: &[],
    summary: "Assumes a numerical pattern, containing unipolar values in the range 0 ..",
    description: "Assumes a numerical pattern, containing unipolar values in the range 0 .. 1.\nReturns a new pattern with values scaled to the given min/max range.\nMost useful in combination with continuous patterns.",
    params: &[
        crate::reference::ReferenceParam {
            name: "min",
            r#type: "number | Pattern",
            description: "lower bound of the range",
        },
        crate::reference::ReferenceParam {
            name: "max",
            r#type: "number | Pattern",
            description: "upper bound of the range",
        },
    ],
    examples: &["s(\"[bd sd]*2,hh*8\")\n.cutoff(sine.range(500,4000))"],
    tags: &["math"],
    no_autocomplete: false,
    deprecated: false,
    origin: "rustel",
};

const RANGEX: crate::reference::ReferenceEntry = crate::reference::ReferenceEntry {
    name: "rangex",
    synonyms: &[],
    summary: "Assumes a numerical pattern, containing unipolar values in the range 0 ..",
    description: "Assumes a numerical pattern, containing unipolar values in the range 0 .. 1\nReturns a new pattern with values scaled to the given min/max range,\nfollowing an exponential curve.",
    params: &[
        crate::reference::ReferenceParam {
            name: "min",
            r#type: "number | Pattern",
            description: "lower bound of the range",
        },
        crate::reference::ReferenceParam {
            name: "max",
            r#type: "number | Pattern",
            description: "upper bound of the range",
        },
    ],
    examples: &["s(\"[bd sd]*2,hh*8\")\n.cutoff(sine.rangex(500,4000))"],
    tags: &["math"],
    no_autocomplete: false,
    deprecated: false,
    origin: "rustel",
};

const RANGE2: crate::reference::ReferenceEntry = crate::reference::ReferenceEntry {
    name: "range2",
    synonyms: &[],
    summary: "Assumes a numerical pattern, containing bipolar values in the range -1 ..",
    description: "Assumes a numerical pattern, containing bipolar values in the range -1 .. 1\nReturns a new pattern with values scaled to the given min/max range.",
    params: &[
        crate::reference::ReferenceParam {
            name: "min",
            r#type: "number | Pattern",
            description: "lower bound of the range",
        },
        crate::reference::ReferenceParam {
            name: "max",
            r#type: "number | Pattern",
            description: "upper bound of the range",
        },
    ],
    examples: &["s(\"[bd sd]*2,hh*8\")\n.cutoff(sine2.range2(500,4000))"],
    tags: &["math"],
    no_autocomplete: false,
    deprecated: false,
    origin: "rustel",
};

pub(super) fn register(r: &mut Registry) {
    // -- math ---------------------------------------------------------------
    add(
        r,
        &["round"],
        ROUND,
        1,
        false,
        crate::native_combinator!(|_args, pat| c::map_numeral(&pat, crate::util::js_round)),
    );

    add(
        r,
        &["floor"],
        FLOOR,
        1,
        false,
        crate::native_combinator!(|_args, pat| c::map_numeral(&pat, f64::floor)),
    );

    add(
        r,
        &["ceil"],
        CEIL,
        1,
        false,
        crate::native_combinator!(|_args, pat| c::map_numeral(&pat, f64::ceil)),
    );

    add(
        r,
        &["log2"],
        LOG2,
        1,
        false,
        crate::native_combinator!(|_args, pat| c::map_numeral(&pat, f64::log2)),
    );

    add(
        r,
        &["toBipolar"],
        TO_BIPOLAR,
        1,
        false,
        crate::native_combinator!(|_args, pat| c::to_bipolar(&pat)),
    );

    add(
        r,
        &["fromBipolar"],
        FROM_BIPOLAR,
        1,
        false,
        crate::native_combinator!(|_args, pat| c::from_bipolar(&pat)),
    );

    add(
        r,
        &["ratio"],
        RATIO,
        1,
        false,
        crate::native_combinator!(|_args, pat| c::ratio(&pat)),
    );

    add(
        r,
        &["invert", "inv"],
        INVERT,
        1,
        true,
        crate::native_combinator!(|_args, pat| c::invert(&pat)),
    );

    add(
        r,
        &["range"],
        RANGE,
        3,
        false,
        crate::native_combinator!(|args, pat| c::range(
            &pat,
            arg_number(args, 0),
            arg_number(args, 1)
        )),
    );

    add(
        r,
        &["rangex"],
        RANGEX,
        3,
        false,
        crate::native_combinator!(|args, pat| c::rangex(
            &pat,
            arg_number(args, 0),
            arg_number(args, 1)
        )),
    );

    add(
        r,
        &["range2"],
        RANGE2,
        3,
        false,
        crate::native_combinator!(|args, pat| c::range2(
            &pat,
            arg_number(args, 0),
            arg_number(args, 1)
        )),
    );
}
