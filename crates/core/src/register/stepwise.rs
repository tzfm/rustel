//! The stepwise family - transforms that read and keep `_steps`.
//!
//! Documentation text from the Strudel project (AGPL-3.0-or-later),
//! https://strudel.cc, where an entry carries upstream's words; entries this
//! port wrote itself say so in their own words.

use super::{Registry, add_step, arg_fraction, stepwise_amount, stepwise_input_refusal};
use crate::combinators as c;
use crate::ops::PatOps;
use rustel_fraction::Fraction;

const TAKE: crate::reference::ReferenceEntry = crate::reference::ReferenceEntry {
    name: "take",
    synonyms: &[],
    summary: "*Experimental*",
    description: "*Experimental*\n\nTakes the given number of steps from a pattern (dropping the rest).\nA positive number will take steps from the start of a pattern, and a negative number from the end.",
    params: &[crate::reference::ReferenceParam {
        name: "amount",
        r#type: "number | Pattern",
        description: "number of steps to take; negative takes from the end",
    }],
    examples: &[
        "\"bd cp ht mt\".take(\"2\").sound()\n// The same as \"bd cp\".sound()",
        "\"bd cp ht mt\".take(\"1 2 3\").sound()\n// The same as \"bd bd cp bd cp ht\".sound()",
        "\"bd cp ht mt\".take(\"-1 -2 -3\").sound()\n// The same as \"mt ht mt cp ht mt\".sound()",
    ],
    tags: &["stepwise"],
    no_autocomplete: false,
    deprecated: false,
    origin: "rustel",
};

const DROP: crate::reference::ReferenceEntry = crate::reference::ReferenceEntry {
    name: "drop",
    synonyms: &[],
    summary: "*Experimental*",
    description: "*Experimental*\n\nDrops the given number of steps from a pattern.\nA positive number will drop steps from the start of a pattern, and a negative number from the end.",
    params: &[crate::reference::ReferenceParam {
        name: "amount",
        r#type: "number | Pattern",
        description: "number of steps to drop; negative drops from the end",
    }],
    examples: &[
        "\"tha dhi thom nam\".drop(\"1\").sound().bank(\"mridangam\")",
        "\"tha dhi thom nam\".drop(\"-1\").sound().bank(\"mridangam\")",
        "\"tha dhi thom nam\".drop(\"0 1 2 3\").sound().bank(\"mridangam\")",
        "\"tha dhi thom nam\".drop(\"0 -1 -2 -3\").sound().bank(\"mridangam\")",
    ],
    tags: &["stepwise"],
    no_autocomplete: false,
    deprecated: false,
    origin: "rustel",
};

const EXPAND: crate::reference::ReferenceEntry = crate::reference::ReferenceEntry {
    name: "expand",
    synonyms: &[],
    summary: "*Experimental*",
    description: "*Experimental*\n\nExpands the step size of the pattern by the given factor.",
    params: &[crate::reference::ReferenceParam {
        name: "factor",
        r#type: "number | Pattern",
        description: "factor to multiply the step count by",
    }],
    examples: &["sound(\"tha dhi thom nam\").bank(\"mridangam\").expand(\"3 2 1 1 2 3\").pace(8)"],
    tags: &["stepwise"],
    no_autocomplete: false,
    deprecated: false,
    origin: "rustel",
};

const EXTEND: crate::reference::ReferenceEntry = crate::reference::ReferenceEntry {
    name: "extend",
    synonyms: &[],
    summary: "*Experimental*",
    description: "*Experimental*\n\n`extend` is similar to `fast` in that it increases its density, but it also increases the step count\naccordingly. So `stepcat(\"a b\".extend(2), \"c d\")` would be the same as `\"a b a b c d\"`, whereas\n`stepcat(\"a b\".fast(2), \"c d\")` would be the same as `\"[a b] [a b] c d\"`.",
    params: &[crate::reference::ReferenceParam {
        name: "factor",
        r#type: "number | Pattern",
        description: "factor to repeat the cycle by, scaling the step count to match",
    }],
    examples: &["stepcat(\n  sound(\"bd bd - cp\").extend(2),\n  sound(\"bd - sd -\")\n).pace(8)"],
    tags: &["stepwise"],
    no_autocomplete: false,
    deprecated: false,
    origin: "rustel",
};

const REPLICATE: crate::reference::ReferenceEntry = crate::reference::ReferenceEntry {
    name: "replicate",
    synonyms: &[],
    summary: "*Experimental*",
    description: "*Experimental*\n\n`replicate` is similar to `fast` in that it increases its density, but it also increases the step count\naccordingly. So `stepcat(\"a b\".replicate(2), \"c d\")` would be the same as `\"a b a b c d\"`, whereas\n`stepcat(\"a b\".fast(2), \"c d\")` would be the same as `\"[a b] [a b] c d\"`.\n\nFor a pattern that changes across cycles, `replicate(2)` repeats each source cycle twice before advancing. `extend(2)` advances through the source cycles twice as fast.",
    params: &[crate::reference::ReferenceParam {
        name: "factor",
        r#type: "number | Pattern",
        description: "factor to repeat whole cycles by, scaling the step count to match",
    }],
    examples: &[
        "stepcat(\n  sound(\"bd bd - cp\").replicate(2),\n  sound(\"bd - sd -\")\n).pace(8)",
    ],
    tags: &["stepwise"],
    no_autocomplete: false,
    deprecated: false,
    origin: "rustel",
};

const CONTRACT: crate::reference::ReferenceEntry = crate::reference::ReferenceEntry {
    name: "contract",
    synonyms: &[],
    summary: "*Experimental*",
    description: "*Experimental*\n\nContracts the step size of the pattern by the given factor. See also `expand`.",
    params: &[crate::reference::ReferenceParam {
        name: "factor",
        r#type: "number | Pattern",
        description: "factor to divide the step count by",
    }],
    examples: &[
        "sound(\"tha dhi thom nam\").bank(\"mridangam\").contract(\"3 2 1 1 2 3\").pace(8)",
    ],
    tags: &["stepwise"],
    no_autocomplete: false,
    deprecated: false,
    origin: "rustel",
};

const SHRINK: crate::reference::ReferenceEntry = crate::reference::ReferenceEntry {
    name: "shrink",
    synonyms: &[],
    summary: "*Experimental*",
    description: "*Experimental*\n\nProgressively shrinks the pattern by 'n' steps until there's nothing left, or if a second value is given (using mininotation list syntax with `:`),\nthat number of times.\nA positive number will progressively drop steps from the start of a pattern, and a negative number from the end.",
    params: &[crate::reference::ReferenceParam {
        name: "amount",
        r#type: "number | Pattern",
        description: "steps to drop from one side each cycle; negative drops from the other side",
    }],
    examples: &[
        "\"tha dhi thom nam\".shrink(\"1\").sound()\n.bank(\"mridangam\")",
        "\"tha dhi thom nam\".shrink(\"-1\").sound()\n.bank(\"mridangam\")",
        "\"tha dhi thom nam\".shrink(\"1 -1\").sound().bank(\"mridangam\").pace(4)",
        "note(\"0 1 2 3 4 5 6 7\".scale(\"C:ritusen\")).sound(\"folkharp\")\n.shrink(\"1 -1\").pace(8)",
    ],
    tags: &["stepwise"],
    no_autocomplete: false,
    deprecated: false,
    origin: "rustel",
};

const GROW: crate::reference::ReferenceEntry = crate::reference::ReferenceEntry {
    name: "grow",
    synonyms: &[],
    summary: "*Experimental*",
    description: "*Experimental*\n\nProgressively grows the pattern by 'n' steps until the full pattern is played, or if a second value is given (using mininotation list syntax with `:`),\nthat number of times.\nA positive number will progressively grow steps from the start of a pattern, and a negative number from the end.",
    params: &[crate::reference::ReferenceParam {
        name: "amount",
        r#type: "number | Pattern",
        description: "steps to add from one side each cycle; negative grows from the other side",
    }],
    examples: &[
        "\"tha dhi thom nam\".grow(\"1\").sound()\n.bank(\"mridangam\")",
        "\"tha dhi thom nam\".grow(\"-1\").sound()\n.bank(\"mridangam\")",
        "\"tha dhi thom nam\".grow(\"1 -1\").sound().bank(\"mridangam\").pace(4)",
        "note(\"0 1 2 3 4 5 6 7\".scale(\"C:ritusen\")).sound(\"folkharp\")\n.grow(\"1 -1\").pace(8)",
    ],
    tags: &["stepwise"],
    no_autocomplete: false,
    deprecated: false,
    origin: "rustel",
};

pub(super) fn register(r: &mut Registry) {
    // -- stepwise (_steps) ---------------------------------------------------
    add_step(
        r,
        &["take"],
        TAKE,
        2,
        false,
        crate::native_combinator!(|args, pat| c::take(&pat, arg_fraction(args, 0))),
    );

    add_step(
        r,
        &["drop"],
        DROP,
        2,
        false,
        crate::native_combinator!(|args, pat| c::drop(&pat, arg_fraction(args, 0))),
    );

    add_step(
        r,
        &["expand"],
        EXPAND,
        2,
        false,
        crate::native_combinator!(|args, pat| c::expand(&pat, arg_fraction(args, 0))),
    );

    add_step(
        r,
        &["extend"],
        EXTEND,
        2,
        false,
        crate::native_combinator!(|args, pat| c::extend(&pat, arg_fraction(args, 0))),
    );

    add_step(
        r,
        &["replicate"],
        REPLICATE,
        2,
        false,
        crate::native_combinator!(|args, pat| c::replicate(&pat, arg_fraction(args, 0))),
    );

    add_step(
        r,
        &["contract"],
        CONTRACT,
        2,
        false,
        crate::native_combinator!(|args, pat| c::contract(&pat, arg_fraction(args, 0))),
    );

    add_step(
        r,
        &["shrink"],
        SHRINK,
        2,
        false,
        crate::native_combinator!(|args, pat| {
            if pat.pat_steps().is_none() {
                c::shrink(&pat, Fraction::ZERO)
            } else {
                match stepwise_amount(args, 0) {
                    Ok(amount) => c::shrink(&pat, amount),
                    Err(_) => stepwise_input_refusal("shrink input"),
                }
            }
        }),
    );

    add_step(
        r,
        &["grow"],
        GROW,
        2,
        false,
        crate::native_combinator!(|args, pat| {
            if pat.pat_steps().is_none() {
                c::grow(&pat, Fraction::ZERO)
            } else {
                match stepwise_amount(args, 0) {
                    Ok(amount) => c::grow(&pat, amount),
                    Err(_) => stepwise_input_refusal("grow input"),
                }
            }
        }),
    );
}
