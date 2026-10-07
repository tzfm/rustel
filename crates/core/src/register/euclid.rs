//! Euclidean rhythms.
//!
//! Documentation text from the Strudel project (AGPL-3.0-or-later),
//! https://strudel.cc, where an entry carries upstream's words; entries this
//! port wrote itself say so in their own words.

use super::{Registry, add, arg_fraction, arg_number};
use crate::Value;
use crate::combinators as c;

const EUCLID: crate::reference::ReferenceEntry = crate::reference::ReferenceEntry {
    name: "euclid",
    synonyms: &[],
    summary: "Changes the structure of the pattern to form an Euclidean rhythm.",
    description: "Changes the structure of the pattern to form an Euclidean rhythm.\nEuclidean rhythms are rhythms obtained using the greatest common\ndivisor of two numbers.  They were described in 2004 by Godfried\nToussaint, a Canadian computer scientist.  Euclidean rhythms are\nreally useful for computer/algorithmic music because they can\ndescribe a large number of rhythms with a couple of numbers.",
    params: &[
        crate::reference::ReferenceParam {
            name: "pulses",
            r#type: "number",
            description: "the number of onsets/beats",
        },
        crate::reference::ReferenceParam {
            name: "steps",
            r#type: "number",
            description: "the number of steps to fill",
        },
    ],
    examples: &["// The Cuban tresillo pattern.\nnote(\"c3\").euclid(3,8)"],
    tags: &["temporal"],
    no_autocomplete: false,
    deprecated: false,
    origin: "rustel",
};

const EUCLID_ROT: crate::reference::ReferenceEntry = crate::reference::ReferenceEntry {
    name: "euclidRot",
    synonyms: &["euclidrot"],
    summary: "a Euclidean rhythm with its starting point rotated",
    description: "euclid with a third argument: euclidRot(pulses, steps, rotation) spreads the pulses as evenly as possible and then rotates the circle of steps, so the same rhythm starts somewhere else. Steps beyond the engine's ceiling refuse the query rather than rendering.",
    params: &[
        crate::reference::ReferenceParam {
            name: "pulses",
            r#type: "number | Pattern",
            description: "how many steps sound",
        },
        crate::reference::ReferenceParam {
            name: "steps",
            r#type: "number | Pattern",
            description: "the length of the rhythm",
        },
        crate::reference::ReferenceParam {
            name: "rotation",
            r#type: "number | Pattern",
            description: "steps to rotate the rhythm by",
        },
    ],
    examples: &["s(\"bd*8\").euclidRot(3, 8, \"<0 1 2 3>\")"],
    tags: &["rhythm", "euclid"],
    no_autocomplete: false,
    deprecated: false,
    origin: "rustel",
};

const BJORK: crate::reference::ReferenceEntry = crate::reference::ReferenceEntry {
    name: "bjork",
    synonyms: &[],
    summary: "a Euclidean rhythm, spelled as one [pulses, steps, rotation] value",
    description: "Björklund's algorithm spread as evenly as possible: bjork(\"[3 8]\") keeps 3 of 8 steps. The third slot rotates the rhythm, and a missing steps falls back to pulses, a missing rotation to 0. Steps beyond the engine's ceiling refuse the query rather than rendering. The same rhythm as euclid(pulses, steps, rotation), spelled as a single mini-notation atom.",
    params: &[crate::reference::ReferenceParam {
        name: "value",
        r#type: "string | Pattern",
        description: "\"[pulses, steps?, rotation?]\"; steps defaults to pulses, rotation to 0",
    }],
    examples: &["s(\"bd*8\").bjork(\"[3 8 1]\")"],
    tags: &["rhythm", "euclid"],
    no_autocomplete: false,
    deprecated: false,
    origin: "rustel",
};

const EUCLID_LEGATO: crate::reference::ReferenceEntry = crate::reference::ReferenceEntry {
    name: "euclidLegato",
    synonyms: &[],
    summary: "Similar to `euclid`, but each pulse is held until the next pulse, so there will be no gaps.",
    description: "Similar to `euclid`, but each pulse is held until the next pulse,\nso there will be no gaps.",
    params: &[
        crate::reference::ReferenceParam {
            name: "pulses",
            r#type: "number",
            description: "the number of onsets/beats",
        },
        crate::reference::ReferenceParam {
            name: "steps",
            r#type: "number",
            description: "the number of steps to fill",
        },
        crate::reference::ReferenceParam {
            name: "rotation",
            r#type: "",
            description: "offset in steps",
        },
        crate::reference::ReferenceParam {
            name: "pat",
            r#type: "",
            description: "",
        },
    ],
    examples: &["note(\"c3\").euclidLegato(3,8)"],
    tags: &["temporal"],
    no_autocomplete: false,
    deprecated: false,
    origin: "rustel",
};

const EUCLID_LEGATO_ROT: crate::reference::ReferenceEntry = crate::reference::ReferenceEntry {
    name: "euclidLegatoRot",
    synonyms: &[],
    summary: "Similar to `euclid`, but each pulse is held until the next pulse, so there will be no gaps, and has an additional parameter for 'rotating' the resulting sequenc",
    description: "Similar to `euclid`, but each pulse is held until the next pulse,\nso there will be no gaps, and has an additional parameter for 'rotating'\nthe resulting sequence",
    params: &[
        crate::reference::ReferenceParam {
            name: "pulses",
            r#type: "number",
            description: "the number of onsets/beats",
        },
        crate::reference::ReferenceParam {
            name: "steps",
            r#type: "number",
            description: "the number of steps to fill",
        },
        crate::reference::ReferenceParam {
            name: "rotation",
            r#type: "number",
            description: "offset in steps",
        },
    ],
    examples: &["note(\"c3\").euclidLegatoRot(3,5,2)"],
    tags: &["temporal"],
    no_autocomplete: false,
    deprecated: false,
    origin: "rustel",
};

const EUCLIDISH: crate::reference::ReferenceEntry = crate::reference::ReferenceEntry {
    name: "euclidish",
    synonyms: &["eish"],
    summary: "A 'euclid' variant with an additional parameter that morphs the resulting rhythm from 0 (no morphing) to 1 (completely 'even').",
    description: "A 'euclid' variant with an additional parameter that morphs the resulting\nrhythm from 0 (no morphing) to 1 (completely 'even'). For example\n`sound(\"bd\").euclidish(3,8,0)` would be the same as\n`sound(\"bd\").euclid(3,8)`, and `sound(\"bd\").euclidish(3,8,1)` would be the\nsame as `sound(\"bd bd bd\")`. `sound(\"bd\").euclidish(3,8,0.5)` would have a\ngroove somewhere between.\nInspired by the work of Malcom Braff.",
    params: &[
        crate::reference::ReferenceParam {
            name: "pulses",
            r#type: "number",
            description: "the number of onsets",
        },
        crate::reference::ReferenceParam {
            name: "steps",
            r#type: "number",
            description: "the number of steps to fill",
        },
        crate::reference::ReferenceParam {
            name: "groove",
            r#type: "number",
            description: "exists between the extremes of 0 (straight euclidian) and 1 (straight pulse)",
        },
    ],
    examples: &["sound(\"hh\").euclidish(7,12,sine.slow(8))\n.pan(sine.slow(8))"],
    tags: &["temporal"],
    no_autocomplete: false,
    deprecated: false,
    origin: "rustel",
};

pub(super) fn register(r: &mut Registry) {
    // -- euclidean rhythms ---------------------------------------------------
    add(
        r,
        &["euclid"],
        EUCLID,
        3,
        false,
        crate::native_combinator!(|args, pat| c::euclid_rot(
            &pat,
            arg_number(args, 0) as i32,
            arg_number(args, 1) as i32,
            0
        )),
    );

    add(
        r,
        &["euclidRot", "euclidrot"],
        EUCLID_ROT,
        4,
        false,
        crate::native_combinator!(|args, pat| c::euclid_rot(
            &pat,
            arg_number(args, 0) as i32,
            arg_number(args, 1) as i32,
            arg_number(args, 2) as i32
        )),
    );

    add(
        r,
        &["bjork"],
        BJORK,
        2,
        false,
        crate::native_combinator!(|args, pat| c::bjork(
            &pat,
            args.first().unwrap_or(&Value::Undefined)
        )),
    );

    add(
        r,
        &["euclidLegato"],
        EUCLID_LEGATO,
        3,
        false,
        crate::native_combinator!(|args, pat| c::euclid_legato(
            &pat,
            arg_number(args, 0) as i32,
            arg_number(args, 1) as i32,
            0
        )),
    );

    add(
        r,
        &["euclidLegatoRot"],
        EUCLID_LEGATO_ROT,
        4,
        false,
        crate::native_combinator!(|args, pat| c::euclid_legato(
            &pat,
            arg_number(args, 0) as i32,
            arg_number(args, 1) as i32,
            arg_number(args, 2) as i32
        )),
    );

    add(
        r,
        &["euclidish", "eish"],
        EUCLIDISH,
        4,
        false,
        crate::native_combinator!(|args, pat| c::euclidish(
            &pat,
            arg_number(args, 0) as i32,
            arg_number(args, 1) as i32,
            arg_fraction(args, 2)
        )),
    );
}
