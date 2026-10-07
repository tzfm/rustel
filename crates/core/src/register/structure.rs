//! Selection and arrangement: callbacks, tags, filters and the jux family.
//!
//! Documentation text from the Strudel project (AGPL-3.0-or-later),
//! https://strudel.cc, where an entry carries upstream's words; entries this
//! port wrote itself say so in their own words.

use super::{Registry, add, add_fn, arg_function, arg_number};
use crate::Value;
use crate::combinators as c;
use crate::ops::PatOps;

const TAG: crate::reference::ReferenceEntry = crate::reference::ReferenceEntry {
    name: "tag",
    synonyms: &[],
    summary: "Tags each Hap with an identifier.",
    description: "Tags each Hap with an identifier. Good for filtering. The function populates Hap.context.tags (Array).",
    params: &[crate::reference::ReferenceParam {
        name: "tag",
        r#type: "string",
        description: "anything unique",
    }],
    examples: &[
        "s(\"saw!16\").note(\"F1\")\n  .lpf(tri.range(40, 80).slow(4)).lpenv(5).lpq(4).lpd(0.15)\n  .when(rand.late(0.1).gte(0.5), x => x.transpose(\"12\").tag('altered'))\n  .when(rand.late(0.2).gte(0.5), x => x.s(\"square\").tag('altered'))\n  .when(\"<0 1>\", x => x.filter((hap) => hap.hasTag('altered')))",
    ],
    tags: &["temporal"],
    no_autocomplete: false,
    deprecated: false,
    origin: "rustel",
};

const WHEN: crate::reference::ReferenceEntry = crate::reference::ReferenceEntry {
    name: "when",
    synonyms: &[],
    summary: "Applies the given function whenever the given pattern is in a true state.",
    description: "Applies the given function whenever the given pattern is in a true state.",
    params: &[
        crate::reference::ReferenceParam {
            name: "binary_pat",
            r#type: "Pattern",
            description: "",
        },
        crate::reference::ReferenceParam {
            name: "func",
            r#type: "function",
            description: "",
        },
    ],
    examples: &["\"c3 eb3 g3\".when(\"<0 1>/2\", x=>x.sub(\"5\")).note()"],
    tags: &["temporal"],
    no_autocomplete: false,
    deprecated: false,
    origin: "rustel",
};

const APPLY: crate::reference::ReferenceEntry = crate::reference::ReferenceEntry {
    name: "apply",
    synonyms: &[],
    summary: "Applies the given function to the pattern.",
    description: "Applies the given function to the pattern. Like layer, but with a single function:",
    params: &[crate::reference::ReferenceParam {
        name: "f",
        r#type: "function",
        description: "the pattern transformer, e.g. rev or fast(2)",
    }],
    examples: &["\"<c3 eb3 g3>\".scale('C minor').apply(scaleTranspose(\"0,2,4\")).note()"],
    tags: &["combiners"],
    no_autocomplete: false,
    deprecated: false,
    origin: "rustel",
};

const APPLY_N: crate::reference::ReferenceEntry = crate::reference::ReferenceEntry {
    name: "applyN",
    synonyms: &[],
    summary: "apply a transformer to the pattern n times",
    description: "applyN(n, f) wraps the pattern in f, n deep - applyN(3, rev) is rev applied three times. A negative n counts as zero, which is the pattern unchanged. Each application wraps another node, so the engine refuses a count large enough to build an unreasonable graph before the query ever runs.",
    params: &[
        crate::reference::ReferenceParam {
            name: "n",
            r#type: "number",
            description: "how many times to apply; negatives count as 0",
        },
        crate::reference::ReferenceParam {
            name: "f",
            r#type: "function",
            description: "the pattern transformer, e.g. rev or fast(2)",
        },
    ],
    examples: &["s(\"bd sd\").applyN(3, rev)"],
    tags: &["structure"],
    no_autocomplete: false,
    deprecated: false,
    origin: "rustel",
};

const FIRST_OF: crate::reference::ReferenceEntry = crate::reference::ReferenceEntry {
    name: "firstOf",
    synonyms: &["every"],
    summary: "Applies the given function every n cycles, starting from the first cycle.",
    description: "Applies the given function every n cycles, starting from the first cycle.",
    params: &[
        crate::reference::ReferenceParam {
            name: "n",
            r#type: "number",
            description: "how many cycles",
        },
        crate::reference::ReferenceParam {
            name: "func",
            r#type: "function",
            description: "function to apply",
        },
    ],
    examples: &["note(\"c3 d3 e3 g3\").firstOf(4, x=>x.rev())"],
    tags: &["temporal"],
    no_autocomplete: false,
    deprecated: false,
    origin: "rustel",
};

const LAST_OF: crate::reference::ReferenceEntry = crate::reference::ReferenceEntry {
    name: "lastOf",
    synonyms: &[],
    summary: "Applies the given function every n cycles, starting from the last cycle.",
    description: "Applies the given function every n cycles, starting from the last cycle.",
    params: &[
        crate::reference::ReferenceParam {
            name: "n",
            r#type: "number",
            description: "how many cycles",
        },
        crate::reference::ReferenceParam {
            name: "func",
            r#type: "function",
            description: "function to apply",
        },
    ],
    examples: &["note(\"c3 d3 e3 g3\").lastOf(4, x=>x.rev())"],
    tags: &["temporal"],
    no_autocomplete: false,
    deprecated: false,
    origin: "rustel",
};

const JUX: crate::reference::ReferenceEntry = crate::reference::ReferenceEntry {
    name: "jux",
    synonyms: &[],
    summary: "The jux function creates strange stereo effects, by applying a function to a pattern, but only in the right-hand channel.",
    description: "The jux function creates strange stereo effects, by applying a function to a pattern, but only in the right-hand channel.",
    params: &[crate::reference::ReferenceParam {
        name: "func",
        r#type: "function",
        description: "function to apply to the right-hand channel",
    }],
    examples: &[
        "s(\"bd lt [~ ht] mt cp ~ bd hh\").jux(rev)",
        "s(\"bd lt [~ ht] mt cp ~ bd hh\").jux(press)",
        "s(\"bd lt [~ ht] mt cp ~ bd hh\").jux(iter(4))",
    ],
    tags: &["temporal", "audio"],
    no_autocomplete: false,
    deprecated: false,
    origin: "rustel",
};

const JUX_BY: crate::reference::ReferenceEntry = crate::reference::ReferenceEntry {
    name: "juxBy",
    synonyms: &["juxby"],
    summary: "Jux with adjustable stereo width.",
    description: "Jux with adjustable stereo width. 0 = mono, 1 = full stereo.",
    params: &[
        crate::reference::ReferenceParam {
            name: "by",
            r#type: "number | Pattern",
            description: "stereo width: 0 = mono, 1 = full stereo",
        },
        crate::reference::ReferenceParam {
            name: "func",
            r#type: "function",
            description: "function to apply to the right-hand channel",
        },
    ],
    examples: &["s(\"bd lt [~ ht] mt cp ~ bd hh\").juxBy(\"<0 .5 1>/2\", rev)"],
    tags: &["temporal"],
    no_autocomplete: false,
    deprecated: false,
    origin: "rustel",
};

const JUX_FLIP_BY: crate::reference::ReferenceEntry = crate::reference::ReferenceEntry {
    name: "juxFlipBy",
    synonyms: &["juxflipby", "fluxBy", "fluxby"],
    summary: "Like juxBy, except it flips the ears each cycle.",
    description: "Like juxBy, except it flips the ears each cycle.",
    params: &[
        crate::reference::ReferenceParam {
            name: "by",
            r#type: "number | Pattern",
            description: "stereo width: 0 = mono, 1 = full stereo",
        },
        crate::reference::ReferenceParam {
            name: "func",
            r#type: "function",
            description: "function to apply",
        },
    ],
    examples: &["s(\"bd lt [~ ht] mt cp ~ bd hh\").juxFlipBy(\".8\", rev)"],
    tags: &[],
    no_autocomplete: false,
    deprecated: false,
    origin: "rustel",
};

const JUX_FLIP: crate::reference::ReferenceEntry = crate::reference::ReferenceEntry {
    name: "juxFlip",
    synonyms: &["juxflip", "flux"],
    summary: "Like jux, but flips the ears each cycle.",
    description: "Like jux, but flips the ears each cycle.",
    params: &[crate::reference::ReferenceParam {
        name: "func",
        r#type: "function",
        description: "function to apply",
    }],
    examples: &[
        "s(\"bd lt [~ ht] mt cp ~ bd hh\").juxFlip(rev)",
        "s(\"bd lt [~ ht] mt cp ~ bd hh\").juxFlip(press)",
        "s(\"bd lt [~ ht] mt cp ~ bd hh\").juxFlip(iter(4))",
    ],
    tags: &[],
    no_autocomplete: false,
    deprecated: false,
    origin: "rustel",
};

const CONTROL: crate::reference::ReferenceEntry = crate::reference::ReferenceEntry {
    name: "control",
    synonyms: &[],
    summary: "MIDI control: Sends a MIDI control change message.",
    description: "MIDI control: Sends a MIDI control change message.",
    params: &[
        crate::reference::ReferenceParam {
            name: "MIDI",
            r#type: "number | Pattern",
            description: "control number (0-127)",
        },
        crate::reference::ReferenceParam {
            name: "MIDI",
            r#type: "number | Pattern",
            description: "controller value (0-127)",
        },
    ],
    examples: &[],
    tags: &["external_io", "midi"],
    no_autocomplete: false,
    deprecated: false,
    origin: "rustel",
};

const AS: crate::reference::ReferenceEntry = crate::reference::ReferenceEntry {
    name: "as",
    synonyms: &[],
    summary: "Sets properties in a batch.",
    description: "Sets properties in a batch.",
    params: &[crate::reference::ReferenceParam {
        name: "mapping",
        r#type: "String | Array",
        description: "the control names that are set",
    }],
    examples: &[
        "\"c:.5 a:1 f:.25 e:.8\".as(\"note:clip\")",
        "\"{0@2 0.25 0 0.5 .3 .5}%8\".as(\"begin\").s(\"sax_vib\").clip(1)",
    ],
    tags: &["combiners"],
    no_autocomplete: false,
    deprecated: false,
    origin: "rustel",
};

const WHEN_KEY: crate::reference::ReferenceEntry = crate::reference::ReferenceEntry {
    name: "whenKey",
    synonyms: &[],
    summary: "Keyboard-triggered changes are unsupported in Rustel.",
    description: "Rustel does not expose keyboard state to scores. whenKey returns the pattern unchanged without calling the supplied function.",
    params: &[
        crate::reference::ReferenceParam {
            name: "key",
            r#type: "string | Array",
            description: "key or keys to listen for",
        },
        crate::reference::ReferenceParam {
            name: "func",
            r#type: "function",
            description: "transformation function",
        },
    ],
    examples: &[],
    tags: &["external_io"],
    no_autocomplete: false,
    deprecated: false,
    origin: "rustel",
};

const FILTER: crate::reference::ReferenceEntry = crate::reference::ReferenceEntry {
    name: "filter",
    synonyms: &[],
    summary: "Filters haps using the given function",
    description: "Filters haps using the given function",
    params: &[crate::reference::ReferenceParam {
        name: "test",
        r#type: "Function",
        description: "function to test Hap",
    }],
    examples: &["s(\"hh!7 oh\").filter(hap => hap.value.s === 'hh')"],
    tags: &["temporal", "functional"],
    no_autocomplete: false,
    deprecated: false,
    origin: "rustel",
};

const KEY_DOWN: crate::reference::ReferenceEntry = crate::reference::ReferenceEntry {
    name: "keyDown",
    synonyms: &[],
    summary: "Keyboard-state queries are unsupported in Rustel.",
    description: "Rustel does not expose keyboard state to scores. Applied to a pattern, keyDown replaces each event value with false regardless of which keys are pressed.",
    params: &[crate::reference::ReferenceParam {
        name: "key",
        r#type: "string | Array",
        description: "key or keys to test",
    }],
    examples: &[],
    tags: &["external_io"],
    no_autocomplete: false,
    deprecated: false,
    origin: "rustel",
};

const FILTER_WHEN: crate::reference::ReferenceEntry = crate::reference::ReferenceEntry {
    name: "filterWhen",
    synonyms: &[],
    summary: "Filters haps by their begin time",
    description: "Filters haps by their begin time",
    params: &[crate::reference::ReferenceParam {
        name: "test",
        r#type: "Function",
        description: "function to test Hap.whole.begin",
    }],
    examples: &["oneCycle: s(\"bd*4\").filterWhen((t) => t < 1)"],
    tags: &["temporal", "functional"],
    no_autocomplete: false,
    deprecated: false,
    origin: "rustel",
};

pub(super) fn register(r: &mut Registry) {
    // `tag` marks haps so a later `filter` can select them by
    // `hap.hasTag(...)`. The name is the tag itself, not a pattern of tags:
    // the argument is taken plain and appended verbatim.
    add(
        r,
        &["tag"],
        TAG,
        2,
        false,
        crate::native_combinator!(|args, pat| {
            let name: std::sync::Arc<str> = match args.first() {
                Some(crate::Value::Str(text)) => std::sync::Arc::from(text.as_str()),
                Some(other) => std::sync::Arc::from(other.show().as_str()),
                None => std::sync::Arc::from(""),
            };
            pat.tag(name)
        }),
    );

    // -- combinators taking a pattern transformer ---------------------------
    //
    // `arg_function(args, i)` reads the transformer out of the reified value.
    // A missing one is the identity, which is what `undefined` does after
    // `reify` in the fast path.
    add_fn(
        r,
        &["when"],
        WHEN,
        3,
        false,
        crate::native_combinator!(|args, pat| c::when(
            &pat,
            args.first().is_some_and(Value::js_truthy),
            arg_function(args, 1)
        )),
    );

    add_fn(
        r,
        &["apply"],
        APPLY,
        2,
        false,
        crate::native_combinator!(|args, pat| c::apply(&pat, arg_function(args, 0))),
    );

    add_fn(
        r,
        &["applyN"],
        APPLY_N,
        3,
        false,
        crate::native_combinator!(|args, pat| c::apply_n(
            &pat,
            arg_number(args, 0) as i64,
            arg_function(args, 1)
        )),
    );

    add_fn(
        r,
        &["firstOf", "every"],
        FIRST_OF,
        3,
        false,
        crate::native_combinator!(|args, pat| c::first_of(
            &pat,
            arg_number(args, 0) as i64,
            arg_function(args, 1)
        )),
    );

    add_fn(
        r,
        &["lastOf"],
        LAST_OF,
        3,
        false,
        crate::native_combinator!(|args, pat| c::last_of(
            &pat,
            arg_number(args, 0) as i64,
            arg_function(args, 1)
        )),
    );

    add_fn(
        r,
        &["jux"],
        JUX,
        2,
        false,
        crate::native_combinator!(|args, pat| c::jux(&pat, arg_function(args, 0))),
    );

    add_fn(
        r,
        &["juxBy", "juxby"],
        JUX_BY,
        3,
        false,
        crate::native_combinator!(|args, pat| c::jux_by(
            &pat,
            arg_number(args, 0),
            arg_function(args, 1)
        )),
    );

    add_fn(
        r,
        &["juxFlipBy", "juxflipby"],
        JUX_FLIP_BY,
        3,
        false,
        crate::native_combinator!(|args, pat| c::jux_flip_by(
            &pat,
            arg_number(args, 0),
            arg_function(args, 1)
        )),
    );

    add_fn(
        r,
        &["juxFlip", "juxflip"],
        JUX_FLIP,
        2,
        false,
        crate::native_combinator!(|args, pat| c::jux_flip(&pat, arg_function(args, 0))),
    );

    add(
        r,
        &["control"],
        CONTROL,
        2,
        false,
        crate::native_combinator!(|args, pat| c::control(
            &pat,
            args.first().unwrap_or(&Value::Undefined)
        )),
    );

    add(
        r,
        &["as"],
        AS,
        2,
        false,
        crate::native_combinator!(|args, pat| c::as_controls(
            &pat,
            args.first().unwrap_or(&Value::Undefined)
        )),
    );

    add_fn(
        r,
        &["whenKey"],
        WHEN_KEY,
        3,
        false,
        crate::native_combinator!(|_args, pat| c::when_key(&pat)),
    );

    add_fn(
        r,
        &["filter"],
        FILTER,
        2,
        false,
        crate::native_combinator!(|args, pat| c::filter_callback(&pat, arg_function(args, 0))),
    );

    add(
        r,
        &["keyDown"],
        KEY_DOWN,
        2,
        false,
        crate::native_combinator!(|_args, pat| c::key_down(&pat)),
    );

    add_fn(
        r,
        &["filterWhen"],
        FILTER_WHEN,
        2,
        false,
        crate::native_combinator!(|args, pat| c::filter_when_callback(&pat, arg_function(args, 0))),
    );
}
