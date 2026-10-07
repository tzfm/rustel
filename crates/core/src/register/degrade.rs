//! The degrade / sometimes family - probabilistic filtering.
//!
//! Documentation text from the Strudel project (AGPL-3.0-or-later),
//! https://strudel.cc, where an entry carries upstream's words; entries this
//! port wrote itself say so in their own words.

use super::{Registry, add, add_fn, arg_function, arg_number};
use crate::combinators as c;
use crate::ops::PatOps;

const DEGRADE_BY: crate::reference::ReferenceEntry = crate::reference::ReferenceEntry {
    name: "degradeBy",
    synonyms: &[],
    summary: "Randomly removes events from the pattern by a given amount.",
    description: "Randomly removes events from the pattern by a given amount.\n0 = 0% chance of removal\n1 = 100% chance of removal",
    params: &[crate::reference::ReferenceParam {
        name: "amount",
        r#type: "number",
        description: "a number between 0 and 1",
    }],
    examples: &[
        "s(\"hh*8\").degradeBy(0.2)",
        "s(\"[hh?0.2]*8\")",
        "//beat generator\ns(\"bd\").segment(16).degradeBy(.5).ribbon(16,1)",
    ],
    tags: &["temporal"],
    no_autocomplete: false,
    deprecated: false,
    origin: "rustel",
};

const DEGRADE: crate::reference::ReferenceEntry = crate::reference::ReferenceEntry {
    name: "degrade",
    synonyms: &[],
    summary: "Randomly removes 50% of events from the pattern.",
    description: "Randomly removes 50% of events from the pattern. Shorthand for `.degradeBy(0.5)`",
    params: &[],
    examples: &["s(\"hh*8\").degrade()", "s(\"[hh?]*8\")"],
    tags: &["temporal"],
    no_autocomplete: false,
    deprecated: false,
    origin: "rustel",
};

const UNDEGRADE_BY: crate::reference::ReferenceEntry = crate::reference::ReferenceEntry {
    name: "undegradeBy",
    synonyms: &[],
    summary: "Inverse of `degradeBy`: Randomly removes events from the pattern by a given amount.",
    description: "Inverse of `degradeBy`: Randomly removes events from the pattern by a given amount.\n0 = 100% chance of removal\n1 = 0% chance of removal\nEvents that would be removed by degradeBy are let through by undegradeBy and vice versa (see second example).",
    params: &[crate::reference::ReferenceParam {
        name: "amount",
        r#type: "number",
        description: "a number between 0 and 1",
    }],
    examples: &[
        "s(\"hh*8\").undegradeBy(0.2)",
        "s(\"hh*10\").layer(\n  x => x.degradeBy(0.2).pan(0),\n  x => x.undegradeBy(0.8).pan(1)\n)",
    ],
    tags: &["temporal"],
    no_autocomplete: false,
    deprecated: false,
    origin: "rustel",
};

const UNDEGRADE: crate::reference::ReferenceEntry = crate::reference::ReferenceEntry {
    name: "undegrade",
    synonyms: &[],
    summary: "Inverse of `degrade`: Randomly removes 50% of events from the pattern.",
    description: "Inverse of `degrade`: Randomly removes 50% of events from the pattern. Shorthand for `.undegradeBy(0.5)`\nEvents that would be removed by degrade are let through by undegrade and vice versa (see second example).",
    params: &[],
    examples: &[
        "s(\"hh*8\").undegrade()",
        "s(\"hh*10\").layer(\n  x => x.degrade().pan(0),\n  x => x.undegrade().pan(1)\n)",
    ],
    tags: &["temporal"],
    no_autocomplete: false,
    deprecated: false,
    origin: "rustel",
};

const SOMETIMES_BY: crate::reference::ReferenceEntry = crate::reference::ReferenceEntry {
    name: "sometimesBy",
    synonyms: &[],
    summary: "Randomly applies the given function by the given probability.",
    description: "Randomly applies the given function by the given probability.\nSimilar to `someCyclesBy`",
    params: &[
        crate::reference::ReferenceParam {
            name: "probability",
            r#type: "number | Pattern",
            description: "a number between 0 and 1",
        },
        crate::reference::ReferenceParam {
            name: "function",
            r#type: "function",
            description: "the transformation to apply",
        },
    ],
    examples: &["s(\"hh*8\").sometimesBy(.4, x=>x.speed(\"0.5\"))"],
    tags: &["temporal"],
    no_autocomplete: false,
    deprecated: false,
    origin: "rustel",
};

const SOME_CYCLES_BY: crate::reference::ReferenceEntry = crate::reference::ReferenceEntry {
    name: "someCyclesBy",
    synonyms: &[],
    summary: "Randomly applies the given function by the given probability on a cycle by cycle basis.",
    description: "Randomly applies the given function by the given probability on a cycle by cycle basis.\nSimilar to `sometimesBy`",
    params: &[
        crate::reference::ReferenceParam {
            name: "probability",
            r#type: "number | Pattern",
            description: "a number between 0 and 1",
        },
        crate::reference::ReferenceParam {
            name: "function",
            r#type: "function",
            description: "the transformation to apply",
        },
    ],
    examples: &["s(\"bd,hh*8\").someCyclesBy(.3, x=>x.speed(\"0.5\"))"],
    tags: &["temporal"],
    no_autocomplete: false,
    deprecated: false,
    origin: "rustel",
};

const SOME_CYCLES: crate::reference::ReferenceEntry = crate::reference::ReferenceEntry {
    name: "someCycles",
    synonyms: &[],
    summary: "Shorthand for `.someCyclesBy(0.5, fn)`",
    description: "Shorthand for `.someCyclesBy(0.5, fn)`",
    params: &[crate::reference::ReferenceParam {
        name: "function",
        r#type: "function",
        description: "the transformation to apply",
    }],
    examples: &["s(\"bd,hh*8\").someCycles(x=>x.speed(\"0.5\"))"],
    tags: &["temporal"],
    no_autocomplete: false,
    deprecated: false,
    origin: "rustel",
};

const SOMETIMES: crate::reference::ReferenceEntry = crate::reference::ReferenceEntry {
    name: "sometimes",
    synonyms: &[],
    summary: "Applies the given function with a 50% chance",
    description: "Applies the given function with a 50% chance",
    params: &[crate::reference::ReferenceParam {
        name: "function",
        r#type: "function",
        description: "the transformation to apply",
    }],
    examples: &["s(\"hh*8\").sometimes(x=>x.speed(\"0.5\"))"],
    tags: &["temporal"],
    no_autocomplete: false,
    deprecated: false,
    origin: "rustel",
};

const OFTEN: crate::reference::ReferenceEntry = crate::reference::ReferenceEntry {
    name: "often",
    synonyms: &[],
    summary: "Shorthand for `.sometimesBy(0.75, fn)`",
    description: "Shorthand for `.sometimesBy(0.75, fn)`",
    params: &[crate::reference::ReferenceParam {
        name: "function",
        r#type: "function",
        description: "the transformation to apply",
    }],
    examples: &["s(\"hh*8\").often(x=>x.speed(\"0.5\"))"],
    tags: &["temporal"],
    no_autocomplete: false,
    deprecated: false,
    origin: "rustel",
};

const RARELY: crate::reference::ReferenceEntry = crate::reference::ReferenceEntry {
    name: "rarely",
    synonyms: &[],
    summary: "Shorthand for `.sometimesBy(0.25, fn)`",
    description: "Shorthand for `.sometimesBy(0.25, fn)`",
    params: &[crate::reference::ReferenceParam {
        name: "function",
        r#type: "function",
        description: "the transformation to apply",
    }],
    examples: &["s(\"hh*8\").rarely(x=>x.speed(\"0.5\"))"],
    tags: &["temporal"],
    no_autocomplete: false,
    deprecated: false,
    origin: "rustel",
};

const ALMOST_NEVER: crate::reference::ReferenceEntry = crate::reference::ReferenceEntry {
    name: "almostNever",
    synonyms: &[],
    summary: "Shorthand for `.sometimesBy(0.1, fn)`",
    description: "Shorthand for `.sometimesBy(0.1, fn)`",
    params: &[crate::reference::ReferenceParam {
        name: "function",
        r#type: "function",
        description: "the transformation to apply",
    }],
    examples: &["s(\"hh*8\").almostNever(x=>x.speed(\"0.5\"))"],
    tags: &["temporal"],
    no_autocomplete: false,
    deprecated: false,
    origin: "rustel",
};

const ALMOST_ALWAYS: crate::reference::ReferenceEntry = crate::reference::ReferenceEntry {
    name: "almostAlways",
    synonyms: &[],
    summary: "Shorthand for `.sometimesBy(0.9, fn)`",
    description: "Shorthand for `.sometimesBy(0.9, fn)`",
    params: &[crate::reference::ReferenceParam {
        name: "function",
        r#type: "function",
        description: "the transformation to apply",
    }],
    examples: &["s(\"hh*8\").almostAlways(x=>x.speed(\"0.5\"))"],
    tags: &["temporal"],
    no_autocomplete: false,
    deprecated: false,
    origin: "rustel",
};

const NEVER: crate::reference::ReferenceEntry = crate::reference::ReferenceEntry {
    name: "never",
    synonyms: &[],
    summary: "Shorthand for `.sometimesBy(0, fn)` (never calls fn)",
    description: "Shorthand for `.sometimesBy(0, fn)` (never calls fn)",
    params: &[crate::reference::ReferenceParam {
        name: "function",
        r#type: "function",
        description: "the transformation to apply - never called; the pattern passes through unchanged",
    }],
    examples: &["s(\"hh*8\").never(x=>x.speed(\"0.5\"))"],
    tags: &["temporal"],
    no_autocomplete: false,
    deprecated: false,
    origin: "rustel",
};

const ALWAYS: crate::reference::ReferenceEntry = crate::reference::ReferenceEntry {
    name: "always",
    synonyms: &[],
    summary: "Shorthand for `.sometimesBy(1, fn)` (always calls fn)",
    description: "Shorthand for `.sometimesBy(1, fn)` (always calls fn)",
    params: &[crate::reference::ReferenceParam {
        name: "function",
        r#type: "function",
        description: "the transformation to apply",
    }],
    examples: &["s(\"hh*8\").always(x=>x.speed(\"0.5\"))"],
    tags: &["temporal"],
    no_autocomplete: false,
    deprecated: false,
    origin: "rustel",
};

pub(super) fn register(r: &mut Registry) {
    // -- the degrade / sometimes family -------------------------------------
    add(
        r,
        &["degradeBy"],
        DEGRADE_BY,
        2,
        true,
        crate::native_combinator!(|args, pat| c::degrade_by(&pat, arg_number(args, 0))),
    );

    add(
        r,
        &["degrade"],
        DEGRADE,
        1,
        true,
        crate::native_combinator!(|_args, pat| pat.degrade_by_seeded(0.5, 0)),
    );

    add(
        r,
        &["undegradeBy"],
        UNDEGRADE_BY,
        2,
        true,
        crate::native_combinator!(|args, pat| c::undegrade_by(&pat, arg_number(args, 0))),
    );

    add(
        r,
        &["undegrade"],
        UNDEGRADE,
        1,
        true,
        crate::native_combinator!(|_args, pat| c::undegrade_by(&pat, 0.5)),
    );

    add_fn(
        r,
        &["sometimesBy"],
        SOMETIMES_BY,
        3,
        false,
        crate::native_combinator!(|args, pat| c::sometimes_by(
            &pat,
            arg_number(args, 0),
            arg_function(args, 1)
        )),
    );

    add_fn(
        r,
        &["someCyclesBy"],
        SOME_CYCLES_BY,
        3,
        false,
        crate::native_combinator!(|args, pat| c::some_cycles_by(
            &pat,
            arg_number(args, 0),
            arg_function(args, 1)
        )),
    );

    add_fn(
        r,
        &["someCycles"],
        SOME_CYCLES,
        2,
        false,
        crate::native_combinator!(|args, pat| c::some_cycles_by(&pat, 0.5, arg_function(args, 0))),
    );

    add_fn(
        r,
        &["sometimes"],
        SOMETIMES,
        2,
        false,
        crate::native_combinator!(|args, pat| c::sometimes_by(&pat, 0.5, arg_function(args, 0))),
    );

    add_fn(
        r,
        &["often"],
        OFTEN,
        2,
        false,
        crate::native_combinator!(|args, pat| c::sometimes_by(&pat, 0.75, arg_function(args, 0))),
    );

    add_fn(
        r,
        &["rarely"],
        RARELY,
        2,
        false,
        crate::native_combinator!(|args, pat| c::sometimes_by(&pat, 0.25, arg_function(args, 0))),
    );

    add_fn(
        r,
        &["almostNever"],
        ALMOST_NEVER,
        2,
        false,
        crate::native_combinator!(|args, pat| c::sometimes_by(&pat, 0.1, arg_function(args, 0))),
    );

    add_fn(
        r,
        &["almostAlways"],
        ALMOST_ALWAYS,
        2,
        false,
        crate::native_combinator!(|args, pat| c::sometimes_by(&pat, 0.9, arg_function(args, 0))),
    );

    add_fn(
        r,
        &["never"],
        NEVER,
        2,
        false,
        crate::native_combinator!(|_args, pat| pat),
    );

    add_fn(
        r,
        &["always"],
        ALWAYS,
        2,
        false,
        crate::native_combinator!(|args, pat| c::apply(&pat, arg_function(args, 0))),
    );
}
