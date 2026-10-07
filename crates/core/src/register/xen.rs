//! The xen layer - xenharmonic scales and tunings; see crate::xen. These names install LAST in the core registry.
//!
//! Documentation text from the Strudel project (AGPL-3.0-or-later),
//! https://strudel.cc, where an entry carries upstream's words; entries this
//! port wrote itself say so in their own words.

use super::{DeclaredIn, Registry, add_in};
use crate::xen as x;

const XEN: crate::reference::ReferenceEntry = crate::reference::ReferenceEntry {
    name: "xen",
    synonyms: &[],
    summary: "Assumes a numerical pattern of scale steps, and a scale.",
    description: "Assumes a numerical pattern of scale steps, and a scale. Scales accepted are all preset scale names of `tune`, arbitrary edos such as 31edo, or an array of frequency ratios. Assumes scales repeat at octave (2/1). Returns a new pattern with all values mapped to their associated frequency, assuming a base frequency of 220hz.",
    params: &[crate::reference::ReferenceParam {
        name: "scaleNameOrRatios",
        r#type: "tuning | number[]",
        description: "an EDO name such as `31edo`, a tuning name, or the ratios themselves.",
    }],
    examples: &[
        "// A minor triad in 31edo:\ni(\"0 8 18\").xen(\"31edo\").piano()",
        "// You can also use xen with frequency ratios.\n// This is equivalent to the above:\ni(\"0 1 2\").xen([\n  Math.pow(2, 0/31),\n  Math.pow(2, 8/31),\n  Math.pow(2, 18/31),\n]).piano()",
        "// xen also supports all scale names that\n// tune does:\ni(\"0 1 2 3 4 5\").xen(\"hexany15\")\n// equiv to:\n// \"0 1 2 3 4 5\".tune(\"hexany15\").mul(\"220\").freq()",
        "i(\"0 1 2 3 4 5 6 7\").xen(\"<5edo 10edo 15edo hexany15>\")",
    ],
    tags: &["tonal"],
    no_autocomplete: false,
    deprecated: false,
    origin: "rustel",
};

const WITH_BASE: crate::reference::ReferenceEntry = crate::reference::ReferenceEntry {
    name: "withBase",
    synonyms: &[],
    summary: "Assumes pattern of frequencies tuned to some `base` frequency, such as the output of `xen` Because `xen` defaults to `220Hz`, so will `withBase`.",
    description: "Assumes pattern of frequencies tuned to some `base` frequency, such as the output of `xen`\nBecause `xen` defaults to `220Hz`, so will `withBase`.\nbut you can specify a different original base with the standard optional array syntax '`:`'",
    params: &[crate::reference::ReferenceParam {
        name: "base",
        r#type: "number",
        description: "",
    }],
    examples: &[
        "i(\"[0 1 2 3] [3 4] [4 3 2 1]\").xen(\"hexany23\").withBase(\"<220 [300 200]>\")",
        "mini([1 / 1, 16 / 15, 9 / 8, 6 / 5, 5 / 4].join(' ')).withBase(\"220:1\")\n// mini([1 / 1, 16 / 15, 9 / 8, 6 / 5, 5 / 4].join(' ')).mul(220).freq()",
    ],
    tags: &["tonal"],
    no_autocomplete: false,
    deprecated: false,
    origin: "rustel",
};

const FTRANS: crate::reference::ReferenceEntry = crate::reference::ReferenceEntry {
    name: "ftranspose",
    synonyms: &["ftrans", "fTrans", "ftranspose", "fTranspose"],
    summary: "Frequency transpose.",
    description: "Frequency transpose. Assumes pattern either has `freq` set, or has values that can be interpreted as frequencies\namt has optional `edoSize` param, defaults to 12.\nIf haps have edoSize param set, such as from the output of `xen(\"31edo\")`,\n`ftrans` will fallback to that instead of 12 as the default.\n\nTransposes the frequency by `amt` edoSteps",
    params: &[
        crate::reference::ReferenceParam {
            name: "amt",
            r#type: "number",
            description: "",
        },
        crate::reference::ReferenceParam {
            name: "edoSize",
            r#type: "number",
            description: "(optional)",
        },
    ],
    examples: &[
        "i(\"0 1 2\").xen(\"12edo\").ftrans(\"7\")\n// n(\"0 1 2\").scale(\"A:chromatic\").trans(\"7\")",
        "i(\"0 8 18\").xen(\"31edo\").ftrans(\"<8 -8>\")",
        "// to transpose by steps of an edo, use \"step:edo\" :\ni(\"0 7 8 18\").xen(\"31edo\").ftrans(\"<0 1:31 1:12>\")",
        "// it can also work with frequency values directly\nfreq(\"200 300 400\").ftrans(\"<0 7:31 7>\")",
    ],
    tags: &["tonal"],
    no_autocomplete: false,
    deprecated: false,
    origin: "rustel",
};

const TUNING: crate::reference::ReferenceEntry = crate::reference::ReferenceEntry {
    name: "tuning",
    synonyms: &[],
    summary: "index each event's numeral straight into a table of ratios",
    description: "The argument is a table of frequency ratios; each event's value is the index into it, with octave folding and no base scaling - multiply by a base and call .freq() to hear it. A table that is not a list is empty, and every lookup answers NaN. Where tune resolves scale NAMES, tuning takes the raw ratios.",
    params: &[crate::reference::ReferenceParam {
        name: "ratios",
        r#type: "number[] | Pattern",
        description: "the ratio table to index into",
    }],
    examples: &["\"0 1 2 3\".tuning([1, 1.125, 1.25, 1.5]).mul(220).freq()"],
    tags: &["tuning", "xen"],
    no_autocomplete: false,
    deprecated: false,
    origin: "rustel",
};

const TUNE: crate::reference::ReferenceEntry = crate::reference::ReferenceEntry {
    name: "tune",
    synonyms: &[],
    summary: "Assumes pattern contains numerical scale degrees on the `i` control (see examples below).",
    description: "Assumes pattern contains numerical scale degrees on the `i` control (see examples below). Accepts a scale name or list of frequencies (see all available names at the link on the reference). Returns a new pattern with all values mapped to a frequency ratio. Similar to `xen`.",
    params: &[crate::reference::ReferenceParam {
        name: "scale",
        r#type: "tuning | number[]",
        description: "a tune.js scale name, or frequencies to use directly.",
    }],
    examples: &[
        "i(\"0 1 2 3 4 5\").tune(\"hexany15\").mul(\"220\").freq()",
        "// You can set your root to be a\n// particular note with getFreq:\ni(\"4 8 9 10 - - 5 7 9 11 - -\").tune(\"tranh3\")\n  .mul(getFreq('c3'))\n  .freq().clip(.5).room(1)",
        "// You can also give tune a list of\n// frequencies to use as the scale:\ni(\"0 1 2 3 4\").tune([\n  261.6255653006,\n  302.72962012827,\n  350.29154279212,\n  405.32593044476,\n  469.00678383895,\n  523.2511306012\n]).mul(220).freq();",
    ],
    tags: &["tonal"],
    no_autocomplete: false,
    deprecated: false,
    origin: "rustel",
};

const EDO_SCALE: crate::reference::ReferenceEntry = crate::reference::ReferenceEntry {
    name: "edoScale",
    synonyms: &[],
    summary: "resolve each event's degree through an EDO scale definition",
    description: "The definition is an array of [root, sequence, large, small]; each event's n becomes a degree the scale resolves into a frequency, written to the hap along with the degree trace the studio's tuning display reads. Arrays only - a string definition is refused with flat()'s own parse error. A scale name instead of a definition is scale()'s territory; edoScale is for the raw [root, sequence, large, small] form.",
    params: &[crate::reference::ReferenceParam {
        name: "definition",
        r#type: "array",
        description: "[root, sequence, large, small] - arrays only",
    }],
    examples: &["n(\"0 1 2 3 4 5 6 7\").edoScale(['C3', 'LLsLLLs', 2, 1]).s(\"triangle\")"],
    tags: &["tuning", "xen"],
    no_autocomplete: false,
    deprecated: false,
    origin: "rustel",
};

pub(super) fn register(r: &mut Registry) {
    // The xen layer - see crate::xen. These names install LAST, after every
    // core and control declaration, so they take the late declaration site.
    // Structural arrays remain pure values through reification, while a
    // patterned string definition needs the ordinary patternified path.
    add_in(
        r,
        DeclaredIn::ControlsModule,
        &["xen"],
        XEN,
        2,
        false,
        crate::native_combinator!(|args, pat| x::xen(
            &pat,
            args.first().cloned().unwrap_or(crate::Value::Undefined)
        )),
    );

    add_in(
        r,
        DeclaredIn::ControlsModule,
        &["withBase"],
        WITH_BASE,
        2,
        false,
        crate::native_combinator!(|args, pat| x::with_base(
            &pat,
            args.first().cloned().unwrap_or(crate::Value::Undefined)
        )),
    );

    add_in(
        r,
        DeclaredIn::ControlsModule,
        &["ftrans", "fTrans", "ftranspose", "fTranspose"],
        FTRANS,
        2,
        false,
        crate::native_combinator!(|args, pat| x::ftrans(
            &pat,
            args.first().cloned().unwrap_or(crate::Value::Undefined)
        )),
    );

    add_in(
        r,
        DeclaredIn::ControlsModule,
        &["tuning"],
        TUNING,
        2,
        false,
        crate::native_combinator!(|args, pat| x::tuning(
            &pat,
            args.first().cloned().unwrap_or(crate::Value::Undefined)
        )),
    );

    add_in(
        r,
        DeclaredIn::ControlsModule,
        &["tune"],
        TUNE,
        2,
        false,
        crate::native_combinator!(|args, pat| x::tune(
            &pat,
            args.first().cloned().unwrap_or(crate::Value::Undefined)
        )),
    );

    add_in(
        r,
        DeclaredIn::ControlsModule,
        &["edoScale"],
        EDO_SCALE,
        2,
        true,
        crate::native_combinator!(|args, pat| x::edo_scale(
            &pat,
            args.first().cloned().unwrap_or(crate::Value::Undefined)
        )),
    );
}
