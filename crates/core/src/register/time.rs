//! Time: tempo, position, windows, repetition and echoes.
//!
//! Documentation text from the Strudel project (AGPL-3.0-or-later),
//! https://strudel.cc, where an entry carries upstream's words; entries this
//! port wrote itself say so in their own words.

use super::{
    Registry, add, add_fn, add_unpatternified, arg_fraction, arg_function, arg_number,
    stepwise_input_refusal,
};
use crate::combinators as c;
use crate::ops::PatOps;
use rustel_fraction::Fraction;

const SLOW: crate::reference::ReferenceEntry = crate::reference::ReferenceEntry {
    name: "slow",
    synonyms: &["sparsity"],
    summary: "Slow down a pattern over the given number of cycles.",
    description: "Slow down a pattern over the given number of cycles. Like the \"/\" operator in mini notation.",
    params: &[crate::reference::ReferenceParam {
        name: "factor",
        r#type: "number | Pattern",
        description: "slow down factor",
    }],
    examples: &["s(\"bd hh sd hh\").slow(2) // s(\"[bd hh sd hh]/2\")"],
    tags: &["temporal"],
    no_autocomplete: false,
    deprecated: false,
    origin: "rustel",
};

const EARLY: crate::reference::ReferenceEntry = crate::reference::ReferenceEntry {
    name: "early",
    synonyms: &[],
    summary: "Nudge a pattern to start earlier in time.",
    description: "Nudge a pattern to start earlier in time. Equivalent of Tidal's <~ operator",
    params: &[crate::reference::ReferenceParam {
        name: "cycles",
        r#type: "number | Pattern",
        description: "number of cycles to nudge left",
    }],
    examples: &["\"bd ~\".stack(\"hh ~\".early(.1)).s()"],
    tags: &["temporal"],
    no_autocomplete: false,
    deprecated: false,
    origin: "rustel",
};

const LATE: crate::reference::ReferenceEntry = crate::reference::ReferenceEntry {
    name: "late",
    synonyms: &[],
    summary: "Nudge a pattern to start later in time.",
    description: "Nudge a pattern to start later in time. Equivalent of Tidal's ~> operator",
    params: &[crate::reference::ReferenceParam {
        name: "cycles",
        r#type: "number | Pattern",
        description: "number of cycles to nudge right",
    }],
    examples: &["\"bd ~\".stack(\"hh ~\".late(.1)).s()"],
    tags: &["temporal"],
    no_autocomplete: false,
    deprecated: false,
    origin: "rustel",
};

const HURRY: crate::reference::ReferenceEntry = crate::reference::ReferenceEntry {
    name: "hurry",
    synonyms: &[],
    summary: "Both speeds up the pattern (like 'fast') and the sample playback (like 'speed').",
    description: "Both speeds up the pattern (like 'fast') and the sample playback (like 'speed').",
    params: &[crate::reference::ReferenceParam {
        name: "factor",
        r#type: "number | Pattern",
        description: "speedup factor for both the pattern and the speed control",
    }],
    examples: &["s(\"bd sd:2\").hurry(\"<1 2 4 3>\").slow(1.5)"],
    tags: &["temporal"],
    no_autocomplete: false,
    deprecated: false,
    origin: "rustel",
};

const CPM: crate::reference::ReferenceEntry = crate::reference::ReferenceEntry {
    name: "cpm",
    synonyms: &[],
    summary: "play the pattern at a tempo given in cycles per minute",
    description: "cpm(60) is one cycle a second; the pattern runs at cpm/60 cycles per second, so this is fast with the number pre-divided. setCpm sets the session tempo the same way; the control shapes one pattern instead.",
    params: &[crate::reference::ReferenceParam {
        name: "cpm",
        r#type: "number | Pattern",
        description: "cycles per minute",
    }],
    examples: &["s(\"bd sd\").cpm(120)"],
    tags: &["temporal", "tempo"],
    no_autocomplete: false,
    deprecated: false,
    origin: "rustel",
};

const CPS: crate::reference::ReferenceEntry = crate::reference::ReferenceEntry {
    name: "cps",
    synonyms: &[],
    summary: "play the pattern at a tempo given in cycles per second",
    description: "cps(1) plays the pattern at one cycle a second. Chained on a pattern it is the cycles-per-second form of cpm; used as a free function in the studio it sets the session tempo.",
    params: &[crate::reference::ReferenceParam {
        name: "cps",
        r#type: "number | Pattern",
        description: "cycles per second",
    }],
    examples: &["s(\"bd*4\").cps(1)"],
    tags: &["temporal"],
    no_autocomplete: false,
    deprecated: false,
    origin: "rustel",
};

const COMPRESS: crate::reference::ReferenceEntry = crate::reference::ReferenceEntry {
    name: "compress",
    synonyms: &[],
    summary: "Compress each cycle into the given timespan, leaving a gap",
    description: "Compress each cycle into the given timespan, leaving a gap",
    params: &[
        crate::reference::ReferenceParam {
            name: "begin",
            r#type: "number | Pattern",
            description: "start of the timespan to compress into",
        },
        crate::reference::ReferenceParam {
            name: "end",
            r#type: "number | Pattern",
            description: "end of the timespan to compress into",
        },
    ],
    examples: &["cat(\n  s(\"bd sd\").compress(.25,.75),\n  s(\"~ bd sd ~\")\n)"],
    tags: &["temporal"],
    no_autocomplete: false,
    deprecated: false,
    origin: "rustel",
};

const BEAT: crate::reference::ReferenceEntry = crate::reference::ReferenceEntry {
    name: "beat",
    synonyms: &[],
    summary: "creates a structure pattern from divisions of a cycle especially useful for creating rhythms",
    description: "creates a structure pattern from divisions of a cycle\nespecially useful for creating rhythms",
    params: &[
        crate::reference::ReferenceParam {
            name: "t",
            r#type: "number | Pattern",
            description: "beat within the cycle to place each event on",
        },
        crate::reference::ReferenceParam {
            name: "div",
            r#type: "number | Pattern",
            description: "number of beats per cycle",
        },
    ],
    examples: &[
        "s(\"bd\").beat(\"0,7,10\", 16)",
        "s(\"sd\").beat(\"4,12\", 16)",
    ],
    tags: &["temporal"],
    no_autocomplete: false,
    deprecated: false,
    origin: "rustel",
};

const SEED: crate::reference::ReferenceEntry = crate::reference::ReferenceEntry {
    name: "seed",
    synonyms: &[],
    summary: "Change the seed for random signals.",
    description: "Change the seed for random signals. Normally, random signals depend on time,\nso two patterns at the same time will have the same random values. Specifying\na new seed changes the signal output by `rand`. This also affects other functions\nthat use randomness, like `shuffle` and `sometimes`.",
    params: &[crate::reference::ReferenceParam {
        name: "n",
        r#type: "number",
        description: "A new seed. Can be any number.",
    }],
    examples: &[
        "$: s(\"hh*4\").degrade();\n$: s(\"bd*4\").degrade().seed(1); // Will degrade different events from the hi-hat",
    ],
    tags: &["math"],
    no_autocomplete: false,
    deprecated: false,
    origin: "rustel",
};

const FAST_GAP: crate::reference::ReferenceEntry = crate::reference::ReferenceEntry {
    name: "fastGap",
    synonyms: &["fastgap"],
    summary: "speeds up a pattern like fast, but rather than it playing multiple times as fast would it instead leaves a gap in the remaining space of the cycle.",
    description: "speeds up a pattern like fast, but rather than it playing multiple times as fast would it instead leaves a gap in the remaining space of the cycle. For example, the following will play the sound pattern \"bd sn\" only once but compressed into the first half of the cycle, i.e. twice as fast.",
    params: &[crate::reference::ReferenceParam {
        name: "factor",
        r#type: "number | Pattern",
        description: "speedup factor",
    }],
    examples: &["s(\"bd sd\").fastGap(2)"],
    tags: &["temporal"],
    no_autocomplete: false,
    deprecated: false,
    origin: "rustel",
};

const FOCUS: crate::reference::ReferenceEntry = crate::reference::ReferenceEntry {
    name: "focus",
    synonyms: &[],
    summary: "Similar to `compress`, but doesn't leave gaps, and the 'focus' can be bigger than a cycle",
    description: "Similar to `compress`, but doesn't leave gaps, and the 'focus' can be bigger than a cycle",
    params: &[
        crate::reference::ReferenceParam {
            name: "begin",
            r#type: "number | Pattern",
            description: "start of the timespan to focus on",
        },
        crate::reference::ReferenceParam {
            name: "end",
            r#type: "number | Pattern",
            description: "end of the timespan to focus on",
        },
    ],
    examples: &["s(\"bd hh sd hh\").focus(1/4, 3/4)"],
    tags: &["temporal"],
    no_autocomplete: false,
    deprecated: false,
    origin: "rustel",
};

const ZOOM: crate::reference::ReferenceEntry = crate::reference::ReferenceEntry {
    name: "zoom",
    synonyms: &[],
    summary: "Plays a portion of a pattern, specified by the beginning and end of a time span.",
    description: "Plays a portion of a pattern, specified by the beginning and end of a time span. The new resulting pattern is played over the time period of the original pattern:",
    params: &[
        crate::reference::ReferenceParam {
            name: "start",
            r#type: "number | Pattern",
            description: "start of the portion of the cycle to play",
        },
        crate::reference::ReferenceParam {
            name: "end",
            r#type: "number | Pattern",
            description: "end of the portion of the cycle to play",
        },
    ],
    examples: &[
        "s(\"bd*2 hh*3 [sd bd]*2 cp\").zoom(0.25, 0.75)\n// s(\"hh*3 [sd bd]*2\") // equivalent",
    ],
    tags: &["temporal"],
    no_autocomplete: false,
    deprecated: false,
    origin: "rustel",
};

const LINGER: crate::reference::ReferenceEntry = crate::reference::ReferenceEntry {
    name: "linger",
    synonyms: &[],
    summary: "Selects the given fraction of the pattern and repeats that part to fill the remainder of the cycle.",
    description: "Selects the given fraction of the pattern and repeats that part to fill the remainder of the cycle.",
    params: &[crate::reference::ReferenceParam {
        name: "fraction",
        r#type: "number",
        description: "fraction to select",
    }],
    examples: &["s(\"lt ht mt cp, [hh oh]*2\").linger(\"<1 .5 .25 .125>\")"],
    tags: &["temporal"],
    no_autocomplete: false,
    deprecated: false,
    origin: "rustel",
};

const PLY: crate::reference::ReferenceEntry = crate::reference::ReferenceEntry {
    name: "ply",
    synonyms: &[],
    summary: "The ply function repeats each event the given number of times.",
    description: "The ply function repeats each event the given number of times.",
    params: &[crate::reference::ReferenceParam {
        name: "factor",
        r#type: "number | Pattern",
        description: "number of times each event is repeated",
    }],
    examples: &["s(\"bd ~ sd cp\").ply(\"<1 2 3>\")"],
    tags: &["temporal"],
    no_autocomplete: false,
    deprecated: false,
    origin: "rustel",
};

const PRESS_BY: crate::reference::ReferenceEntry = crate::reference::ReferenceEntry {
    name: "pressBy",
    synonyms: &[],
    summary: "Like press, but allows you to specify the amount by which each event is shifted.",
    description: "Like press, but allows you to specify the amount by which each\nevent is shifted. pressBy(0.5) is the same as press, while\npressBy(1/3) shifts each event by a third of its timespan.",
    params: &[crate::reference::ReferenceParam {
        name: "r",
        r#type: "number | Pattern",
        description: "shift amount, as a fraction of each event's timespan",
    }],
    examples: &[
        "stack(s(\"hh*4\"),\n      s(\"bd mt sd ht\").pressBy(\"<0 0.5 0.25>\")\n     ).slow(2)",
    ],
    tags: &["temporal"],
    no_autocomplete: false,
    deprecated: false,
    origin: "rustel",
};

const PRESS: crate::reference::ReferenceEntry = crate::reference::ReferenceEntry {
    name: "press",
    synonyms: &[],
    summary: "Syncopates a rhythm, by shifting each event halfway into its timespan.",
    description: "Syncopates a rhythm, by shifting each event halfway into its timespan.",
    params: &[],
    examples: &["stack(s(\"hh*4\"),\n      s(\"bd mt sd ht\").every(4, press)\n     ).slow(2)"],
    tags: &["temporal"],
    no_autocomplete: false,
    deprecated: false,
    origin: "rustel",
};

const SEGMENT: crate::reference::ReferenceEntry = crate::reference::ReferenceEntry {
    name: "segment",
    synonyms: &["seg"],
    summary: "Samples the pattern at a rate of n events per cycle.",
    description: "Samples the pattern at a rate of n events per cycle. Useful for turning a continuous pattern into a discrete one.",
    params: &[crate::reference::ReferenceParam {
        name: "segments",
        r#type: "number",
        description: "number of segments per cycle",
    }],
    examples: &["note(saw.range(40,52).segment(24))"],
    tags: &["temporal"],
    no_autocomplete: false,
    deprecated: false,
    origin: "rustel",
};

const ITER: crate::reference::ReferenceEntry = crate::reference::ReferenceEntry {
    name: "iter",
    synonyms: &[],
    summary: "Divides a pattern into a given number of subdivisions, plays the subdivisions in order, but increments the starting subdivision each cycle.",
    description: "Divides a pattern into a given number of subdivisions, plays the subdivisions in order, but increments the starting subdivision each cycle. The pattern wraps to the first subdivision after the last subdivision is played.",
    params: &[crate::reference::ReferenceParam {
        name: "n",
        r#type: "number | Pattern",
        description: "number of subdivisions",
    }],
    examples: &["note(\"0 1 2 3\".scale('A minor')).iter(4)"],
    tags: &["temporal"],
    no_autocomplete: false,
    deprecated: false,
    origin: "rustel",
};

const ITER_BACK: crate::reference::ReferenceEntry = crate::reference::ReferenceEntry {
    name: "iterBack",
    synonyms: &["iterback"],
    summary: "Like `iter`, but plays the subdivisions in reverse order.",
    description: "Like `iter`, but plays the subdivisions in reverse order. Known as iter' in tidalcycles",
    params: &[crate::reference::ReferenceParam {
        name: "n",
        r#type: "number | Pattern",
        description: "number of subdivisions",
    }],
    examples: &["note(\"0 1 2 3\".scale('A minor')).iterBack(4)"],
    tags: &["temporal"],
    no_autocomplete: false,
    deprecated: false,
    origin: "rustel",
};

const REPEAT_CYCLES: crate::reference::ReferenceEntry = crate::reference::ReferenceEntry {
    name: "repeatCycles",
    synonyms: &[],
    summary: "Repeats each cycle the given number of times.",
    description: "Repeats each cycle the given number of times.",
    params: &[crate::reference::ReferenceParam {
        name: "count",
        r#type: "number | Pattern",
        description: "number of times each cycle is repeated",
    }],
    examples: &[
        "note(irand(12).add(34)).segment(4).repeatCycles(2).s(\"gm_acoustic_guitar_nylon\")",
    ],
    tags: &["temporal"],
    no_autocomplete: false,
    deprecated: false,
    origin: "rustel",
};

const REVV: crate::reference::ReferenceEntry = crate::reference::ReferenceEntry {
    name: "revv",
    synonyms: &[],
    summary: "Reverse a whole pattern.",
    description: "Reverse a whole pattern. See also `rev` for reversing each cycle.",
    params: &[],
    examples: &[
        "// This is the same as `<[g e] [d c]>`. If `rev()` is used, you get\n// the same as `<[d c] [g e]>`, where each cycle reverses, but the order of\n// cycles stays the same.\nnote(\"<[c d] [e g]>\").revv()",
    ],
    tags: &["temporal"],
    no_autocomplete: false,
    deprecated: false,
    origin: "rustel",
};

const PALINDROME: crate::reference::ReferenceEntry = crate::reference::ReferenceEntry {
    name: "palindrome",
    synonyms: &[],
    summary: "Applies `rev` to a pattern every other cycle, so that the pattern alternates between forwards and backwards.",
    description: "Applies `rev` to a pattern every other cycle, so that the pattern alternates between forwards and backwards.",
    params: &[],
    examples: &["note(\"c d e g\").palindrome()"],
    tags: &["temporal"],
    no_autocomplete: false,
    deprecated: false,
    origin: "rustel",
};

const BRAK: crate::reference::ReferenceEntry = crate::reference::ReferenceEntry {
    name: "brak",
    synonyms: &[],
    summary: "Returns a new pattern where every other cycle is played once, twice as fast, and offset in time by one quarter of a cycle.",
    description: "Returns a new pattern where every other cycle is played once, twice as\nfast, and offset in time by one quarter of a cycle. Creates a kind of\nbreakbeat feel.",
    params: &[],
    examples: &[],
    tags: &["temporal"],
    no_autocomplete: false,
    deprecated: false,
    origin: "rustel",
};

const SWING_BY: crate::reference::ReferenceEntry = crate::reference::ReferenceEntry {
    name: "swingBy",
    synonyms: &[],
    summary: "The function `swingBy x n` breaks each cycle into `n` slices, and then delays events in the second half of each slice by the amount `x`, which is relative to th",
    description: "The function `swingBy x n` breaks each cycle into `n` slices, and then delays events in the second half of each slice by the amount `x`, which is relative to the size of the (half) slice. So if `x` is 0 it does nothing, `0.5` delays for half the note duration, and 1 will wrap around to doing nothing again. The end result is a shuffle or swing-like rhythm",
    params: &[
        crate::reference::ReferenceParam {
            name: "subdivision",
            r#type: "number",
            description: "",
        },
        crate::reference::ReferenceParam {
            name: "offset",
            r#type: "number",
            description: "",
        },
    ],
    examples: &["s(\"hh*8\").swingBy(1/3, 4)"],
    tags: &["temporal"],
    no_autocomplete: false,
    deprecated: false,
    origin: "rustel",
};

const SWING: crate::reference::ReferenceEntry = crate::reference::ReferenceEntry {
    name: "swing",
    synonyms: &[],
    summary: "Shorthand for swingBy with 1/3:",
    description: "Shorthand for swingBy with 1/3:",
    params: &[crate::reference::ReferenceParam {
        name: "subdivision",
        r#type: "number",
        description: "",
    }],
    examples: &["s(\"hh*8\").swing(4)\n// s(\"hh*8\").swingBy(1/3, 4)"],
    tags: &["temporal"],
    no_autocomplete: false,
    deprecated: false,
    origin: "rustel",
};

const RIBBON: crate::reference::ReferenceEntry = crate::reference::ReferenceEntry {
    name: "ribbon",
    synonyms: &["rib"],
    summary: "Loops the pattern inside an `offset` for `cycles`.",
    description: "Loops the pattern inside an `offset` for `cycles`.\nIf you think of the entire span of time in cycles as a ribbon, you can cut a single piece and loop it.",
    params: &[
        crate::reference::ReferenceParam {
            name: "offset",
            r#type: "number",
            description: "start point of loop in cycles",
        },
        crate::reference::ReferenceParam {
            name: "cycles",
            r#type: "number",
            description: "loop length in cycles",
        },
    ],
    examples: &[
        "note(\"<c d e f>\").ribbon(1, 2)",
        "// Looping a portion of randomness\nn(irand(8).segment(4)).scale(\"c:pentatonic\").ribbon(1337, 2)",
        "// rhythm generator\ns(\"bd!16?\").ribbon(29,.5)",
    ],
    tags: &["temporal"],
    no_autocomplete: false,
    deprecated: false,
    origin: "rustel",
};

const CHUNK: crate::reference::ReferenceEntry = crate::reference::ReferenceEntry {
    name: "chunk",
    synonyms: &["slowChunk", "slowchunk"],
    summary: "Divides a pattern into a given number of parts, then cycles through those parts in turn, applying the given function to each part in turn (one part per cycle).",
    description: "Divides a pattern into a given number of parts, then cycles through those parts in turn, applying the given function to each part in turn (one part per cycle).",
    params: &[
        crate::reference::ReferenceParam {
            name: "n",
            r#type: "number | Pattern",
            description: "number of parts to divide the pattern into",
        },
        crate::reference::ReferenceParam {
            name: "func",
            r#type: "function",
            description: "function to apply to each part in turn",
        },
    ],
    examples: &["\"0 1 2 3\".chunk(4, x=>x.add(7))\n.scale(\"A:minor\").note()"],
    tags: &["temporal", "functional"],
    no_autocomplete: false,
    deprecated: false,
    origin: "rustel",
};

const CHUNK_BACK: crate::reference::ReferenceEntry = crate::reference::ReferenceEntry {
    name: "chunkBack",
    synonyms: &["chunkback"],
    summary: "Like `chunk`, but cycles through the parts in reverse order.",
    description: "Like `chunk`, but cycles through the parts in reverse order. Known as chunk' in tidalcycles",
    params: &[
        crate::reference::ReferenceParam {
            name: "n",
            r#type: "number | Pattern",
            description: "number of parts to divide the pattern into",
        },
        crate::reference::ReferenceParam {
            name: "func",
            r#type: "function",
            description: "function to apply to each part in turn",
        },
    ],
    examples: &["\"0 1 2 3\".chunkBack(4, x=>x.add(7))\n.scale(\"A:minor\").note()"],
    tags: &["temporal"],
    no_autocomplete: false,
    deprecated: false,
    origin: "rustel",
};

const FASTCHUNK: crate::reference::ReferenceEntry = crate::reference::ReferenceEntry {
    name: "fastChunk",
    synonyms: &["fastchunk"],
    summary: "Like `chunk`, but the cycles of the source pattern aren't repeated for each set of chunks.",
    description: "Like `chunk`, but the cycles of the source pattern aren't repeated\nfor each set of chunks.",
    params: &[
        crate::reference::ReferenceParam {
            name: "n",
            r#type: "number | Pattern",
            description: "number of parts to divide the pattern into",
        },
        crate::reference::ReferenceParam {
            name: "func",
            r#type: "function",
            description: "function to apply to each part in turn",
        },
    ],
    examples: &[
        "\"<0 8> 1 2 3 4 5 6 7\"\n.scale(\"C2:major\").note()\n.fastChunk(4, x => x.color('red')).slow(2)",
    ],
    tags: &["temporal"],
    no_autocomplete: false,
    deprecated: false,
    origin: "rustel",
};

const PACE: crate::reference::ReferenceEntry = crate::reference::ReferenceEntry {
    name: "pace",
    // `steps` is the alias the score realm installs beside `pace`, so both
    // spellings open this page.
    synonyms: &["steps"],
    summary: "*Experimental*",
    description: "*Experimental*\n\nSpeeds a pattern up or down, to fit to the given number of steps per cycle.",
    params: &[crate::reference::ReferenceParam {
        name: "steps",
        r#type: "number | Pattern",
        description: "number of steps per cycle to fit the pattern to",
    }],
    examples: &[
        "sound(\"bd sd cp\").pace(4)\n// The same as sound(\"{bd sd cp}%4\") or sound(\"<bd sd cp>*4\")",
    ],
    tags: &["stepwise"],
    no_autocomplete: false,
    deprecated: false,
    origin: "rustel",
};

const INSIDE: crate::reference::ReferenceEntry = crate::reference::ReferenceEntry {
    name: "inside",
    synonyms: &[],
    summary: "Carries out an operation 'inside' a cycle.",
    description: "Carries out an operation 'inside' a cycle.",
    params: &[
        crate::reference::ReferenceParam {
            name: "factor",
            r#type: "number | Pattern",
            description: "factor the pattern is slowed by while the function runs",
        },
        crate::reference::ReferenceParam {
            name: "func",
            r#type: "function",
            description: "function to apply",
        },
    ],
    examples: &[
        "\"0 1 2 3 4 3 2 1\".inside(4, rev).scale('C major').note()\n// \"0 1 2 3 4 3 2 1\".slow(4).rev().fast(4).scale('C major').note()",
    ],
    tags: &["temporal"],
    no_autocomplete: false,
    deprecated: false,
    origin: "rustel",
};

const OUTSIDE: crate::reference::ReferenceEntry = crate::reference::ReferenceEntry {
    name: "outside",
    synonyms: &[],
    summary: "Carries out an operation 'outside' a cycle.",
    description: "Carries out an operation 'outside' a cycle.",
    params: &[
        crate::reference::ReferenceParam {
            name: "factor",
            r#type: "number | Pattern",
            description: "factor the pattern is sped up by while the function runs",
        },
        crate::reference::ReferenceParam {
            name: "func",
            r#type: "function",
            description: "function to apply",
        },
    ],
    examples: &[
        "\"<[0 1] 2 [3 4] 5>\".outside(4, rev).scale('C major').note()\n// \"<[0 1] 2 [3 4] 5>\".fast(4).rev().slow(4).scale('C major').note()",
    ],
    tags: &["temporal"],
    no_autocomplete: false,
    deprecated: false,
    origin: "rustel",
};

const OFF: crate::reference::ReferenceEntry = crate::reference::ReferenceEntry {
    name: "off",
    synonyms: &[],
    summary: "Superimposes the function result on top of the original pattern, delayed by the given time.",
    description: "Superimposes the function result on top of the original pattern, delayed by the given time.",
    params: &[
        crate::reference::ReferenceParam {
            name: "time",
            r#type: "Pattern | number",
            description: "offset time",
        },
        crate::reference::ReferenceParam {
            name: "func",
            r#type: "function",
            description: "function to apply",
        },
    ],
    examples: &["\"c3 eb3 g3\".off(1/8, x=>x.add(7)).note()"],
    tags: &["temporal"],
    no_autocomplete: false,
    deprecated: false,
    origin: "rustel",
};

const ECHO: crate::reference::ReferenceEntry = crate::reference::ReferenceEntry {
    name: "echo",
    synonyms: &[],
    summary: "Superimpose and offset multiple times, gradually decreasing the velocity",
    description: "Superimpose and offset multiple times, gradually decreasing the velocity",
    params: &[
        crate::reference::ReferenceParam {
            name: "times",
            r#type: "number",
            description: "how many times to repeat",
        },
        crate::reference::ReferenceParam {
            name: "time",
            r#type: "number",
            description: "cycle offset between iterations",
        },
        crate::reference::ReferenceParam {
            name: "feedback",
            r#type: "number",
            description: "velocity multiplicator for each iteration",
        },
    ],
    examples: &["s(\"bd sd\").echo(3, 1/6, .8)"],
    tags: &["temporal"],
    no_autocomplete: false,
    deprecated: false,
    origin: "rustel",
};

const STUT: crate::reference::ReferenceEntry = crate::reference::ReferenceEntry {
    name: "stut",
    synonyms: &[],
    summary: "Deprecated.",
    description: "Deprecated. Like echo, but the last 2 parameters are flipped.",
    params: &[
        crate::reference::ReferenceParam {
            name: "times",
            r#type: "number",
            description: "how many times to repeat",
        },
        crate::reference::ReferenceParam {
            name: "feedback",
            r#type: "number",
            description: "velocity multiplicator for each iteration",
        },
        crate::reference::ReferenceParam {
            name: "time",
            r#type: "number",
            description: "cycle offset between iterations",
        },
    ],
    examples: &["s(\"bd sd\").stut(3, .8, 1/6)"],
    tags: &["temporal"],
    no_autocomplete: false,
    deprecated: false,
    origin: "rustel",
};

const ECHO_WITH: crate::reference::ReferenceEntry = crate::reference::ReferenceEntry {
    name: "echoWith",
    synonyms: &["echowith", "stutWith", "stutwith"],
    summary: "Superimpose and offset multiple times, applying the given function each time.",
    description: "Superimpose and offset multiple times, applying the given function each time.",
    params: &[
        crate::reference::ReferenceParam {
            name: "times",
            r#type: "number",
            description: "how many times to repeat",
        },
        crate::reference::ReferenceParam {
            name: "time",
            r#type: "number",
            description: "cycle offset between iterations",
        },
        crate::reference::ReferenceParam {
            name: "func",
            r#type: "function",
            description: "function to apply, given the pattern and the iteration index",
        },
    ],
    examples: &[
        "\"<0 [2 4]>\"\n.echoWith(4, 1/8, (p,n) => p.add(n*2))\n.scale(\"C:minor\").note()",
    ],
    tags: &["temporal", "functional"],
    no_autocomplete: false,
    deprecated: false,
    origin: "rustel",
};

const WITHIN: crate::reference::ReferenceEntry = crate::reference::ReferenceEntry {
    name: "within",
    synonyms: &[],
    summary: "Use within to apply a function to only a part of a pattern.",
    description: "Use within to apply a function to only a part of a pattern.",
    params: &[
        crate::reference::ReferenceParam {
            name: "start",
            r#type: "number",
            description: "start within cycle (0 - 1)",
        },
        crate::reference::ReferenceParam {
            name: "end",
            r#type: "number",
            description: "end within cycle (0 - 1). Must be > start",
        },
        crate::reference::ReferenceParam {
            name: "func",
            r#type: "Function",
            description: "function to be applied to the sub-pattern",
        },
    ],
    examples: &[],
    tags: &["temporal", "functional"],
    no_autocomplete: false,
    deprecated: false,
    origin: "rustel",
};

const LOOP_AT_CPS: crate::reference::ReferenceEntry = crate::reference::ReferenceEntry {
    name: "loopAtCps",
    synonyms: &["loopatcps"],
    summary: "Makes the sample fit the given number of cycles and cps value, by changing the speed.",
    description: "Makes the sample fit the given number of cycles and cps value, by\nchanging the speed. deprecated: use loopAt or fit instead, together with setCps / setCpm.",
    params: &[
        crate::reference::ReferenceParam {
            name: "factor",
            r#type: "number | Pattern",
            description: "number of cycles to fit the sample into",
        },
        crate::reference::ReferenceParam {
            name: "cps",
            r#type: "number | Pattern",
            description: "tempo in cycles per second",
        },
    ],
    examples: &[
        "samples({ rhodes: 'https://cdn.freesound.org/previews/132/132051_316502-lq.mp3' })\ns(\"rhodes\").loopAtCps(4,1.5).cps(1.5)",
    ],
    tags: &["samples", "pitch"],
    no_autocomplete: false,
    deprecated: true,
    origin: "rustel",
};

const STRIATE: crate::reference::ReferenceEntry = crate::reference::ReferenceEntry {
    name: "striate",
    synonyms: &[],
    summary: "Cuts each sample into the given number of parts, triggering progressive portions of each sample at each loop.",
    description: "Cuts each sample into the given number of parts, triggering progressive portions of each sample at each loop.",
    params: &[crate::reference::ReferenceParam {
        name: "n",
        r#type: "number | Pattern",
        description: "number of parts to cut each sample into",
    }],
    examples: &["s(\"numbers:0 numbers:1 numbers:2\").striate(6).slow(3)"],
    tags: &["samples"],
    no_autocomplete: false,
    deprecated: false,
    origin: "rustel",
};

const CHOP: crate::reference::ReferenceEntry = crate::reference::ReferenceEntry {
    name: "chop",
    synonyms: &[],
    summary: "Cuts each sample into the given number of parts, allowing you to explore a technique known as 'granular synthesis'.",
    description: "Cuts each sample into the given number of parts, allowing you to explore a technique known as 'granular synthesis'.\nIt turns a pattern of samples into a pattern of parts of samples.",
    params: &[crate::reference::ReferenceParam {
        name: "n",
        r#type: "number | Pattern",
        description: "number of parts to cut each sample into",
    }],
    examples: &[
        "samples({ rhodes: 'https://cdn.freesound.org/previews/132/132051_316502-lq.mp3' })\ns(\"rhodes\")\n .chop(4)\n .rev() // reverse order of chops\n .loopAt(2) // fit sample into 2 cycles",
    ],
    tags: &["samples"],
    no_autocomplete: false,
    deprecated: false,
    origin: "rustel",
};

const SHUFFLE: crate::reference::ReferenceEntry = crate::reference::ReferenceEntry {
    name: "shuffle",
    synonyms: &[],
    summary: "Slices a pattern into the given number of parts, then plays those parts in random order.",
    description: "Slices a pattern into the given number of parts, then plays those parts in random order.\nEach part will be played exactly once per cycle.",
    params: &[crate::reference::ReferenceParam {
        name: "n",
        r#type: "number | Pattern",
        description: "number of parts to slice the pattern into",
    }],
    examples: &[
        "note(\"c d e f\").sound(\"piano\").shuffle(4)",
        "seq(\"c d e f\".shuffle(4), \"g\").note().sound(\"piano\")",
    ],
    tags: &["temporal"],
    no_autocomplete: false,
    deprecated: false,
    origin: "rustel",
};

const SCRAMBLE: crate::reference::ReferenceEntry = crate::reference::ReferenceEntry {
    name: "scramble",
    synonyms: &[],
    summary: "Slices a pattern into the given number of parts, then plays those parts at random.",
    description: "Slices a pattern into the given number of parts, then plays those parts at random. Similar to `shuffle`,\nbut parts might be played more than once, or not at all, per cycle.",
    params: &[crate::reference::ReferenceParam {
        name: "n",
        r#type: "number | Pattern",
        description: "number of parts to slice the pattern into",
    }],
    examples: &[
        "note(\"c d e f\").sound(\"piano\").scramble(4)",
        "seq(\"c d e f\".scramble(4), \"g\").note().sound(\"piano\")",
    ],
    tags: &["temporal"],
    no_autocomplete: false,
    deprecated: false,
    origin: "rustel",
};

const REV: crate::reference::ReferenceEntry = crate::reference::ReferenceEntry {
    name: "rev",
    synonyms: &[],
    summary: "Reverse all cycles in a pattern.",
    description: "Reverse all cycles in a pattern. See also `revv` for reversing a whole pattern.",
    params: &[],
    examples: &["note(\"c d e g\").rev()"],
    tags: &["temporal"],
    no_autocomplete: false,
    deprecated: false,
    origin: "rustel",
};

pub(super) fn register(r: &mut Registry) {
    // -- time ---------------------------------------------------------------
    // `slow` is not step-preserving. The scalar fast path still keeps
    // `_steps` because `_fast` sets them itself. The patterned general path
    // must leave them undefined: a step count there lets `polyJoin` produce
    // six haps where strudel.cc throws and yields none.
    add(
        r,
        &["slow", "sparsity"],
        SLOW,
        2,
        false,
        crate::native_combinator!(|args, pat| pat.slow(arg_fraction(args, 0))),
    );

    add(
        r,
        &["early"],
        EARLY,
        2,
        true,
        crate::native_combinator!(|args, pat| pat.early(arg_fraction(args, 0))),
    );

    add(
        r,
        &["late"],
        LATE,
        2,
        true,
        crate::native_combinator!(|args, pat| pat.late(arg_fraction(args, 0))),
    );

    add(
        r,
        &["hurry"],
        HURRY,
        2,
        false,
        crate::native_combinator!(|args, pat| c::hurry(&pat, arg_number(args, 0))),
    );

    add(
        r,
        &["cps"],
        CPS,
        2,
        false,
        crate::native_combinator!(|args, pat| {
            match arg_fraction(args, 0).checked_mul(Fraction::from(60)) {
                Some(cpm) => c::cpm(&pat, cpm),
                None => stepwise_input_refusal("cps"),
            }
        }),
    );

    add(
        r,
        &["cpm"],
        CPM,
        2,
        false,
        crate::native_combinator!(|args, pat| c::cpm(&pat, arg_fraction(args, 0))),
    );

    add(
        r,
        &["compress"],
        COMPRESS,
        3,
        false,
        crate::native_combinator!(
            |args, pat| pat.compress(arg_fraction(args, 0), arg_fraction(args, 1))
        ),
    );

    add(
        r,
        &["beat"],
        BEAT,
        3,
        false,
        crate::native_combinator!(|args, pat| c::beat(
            &pat,
            arg_fraction(args, 0),
            arg_fraction(args, 1)
        )),
    );

    // `seed(n)`: constant-form withSeed. Patternified like every default
    // register(), so `seed("<1 2>")` alternates streams.
    add(
        r,
        &["seed"],
        SEED,
        2,
        false,
        crate::native_combinator!(|args, pat| {
            let seed = args
                .first()
                .and_then(crate::Value::as_f64)
                .unwrap_or(f64::NAN);
            pat.with_rand_seed(seed)
        }),
    );

    add(
        r,
        &["fastGap", "fastgap"],
        FAST_GAP,
        2,
        false,
        crate::native_combinator!(|args, pat| pat.fast_gap(arg_fraction(args, 0))),
    );

    add(
        r,
        &["focus"],
        FOCUS,
        3,
        false,
        crate::native_combinator!(
            |args, pat| pat.focus(arg_fraction(args, 0), arg_fraction(args, 1))
        ),
    );

    add(
        r,
        &["zoom"],
        ZOOM,
        3,
        false,
        crate::native_combinator!(
            |args, pat| pat.zoom(arg_fraction(args, 0), arg_fraction(args, 1))
        ),
    );

    add(
        r,
        &["linger"],
        LINGER,
        2,
        true,
        crate::native_combinator!(|args, pat| c::linger(&pat, arg_fraction(args, 0))),
    );

    add(
        r,
        &["ply"],
        PLY,
        2,
        false,
        crate::native_combinator!(|args, pat| c::ply(&pat, arg_fraction(args, 0))),
    );

    add(
        r,
        &["pressBy"],
        PRESS_BY,
        2,
        false,
        crate::native_combinator!(|args, pat| c::press_by(&pat, arg_fraction(args, 0))),
    );

    add(
        r,
        &["press"],
        PRESS,
        1,
        false,
        crate::native_combinator!(|_args, pat| c::press(&pat)),
    );

    add(
        r,
        &["segment", "seg"],
        SEGMENT,
        2,
        false,
        crate::native_combinator!(|args, pat| c::segment(&pat, arg_fraction(args, 0))),
    );

    add(
        r,
        &["iter"],
        ITER,
        2,
        true,
        crate::native_combinator!(|args, pat| c::iter(&pat, arg_fraction(args, 0), false)),
    );

    add(
        r,
        &["iterBack", "iterback"],
        ITER_BACK,
        2,
        true,
        crate::native_combinator!(|args, pat| c::iter(&pat, arg_fraction(args, 0), true)),
    );

    add(
        r,
        &["repeatCycles"],
        REPEAT_CYCLES,
        2,
        true,
        crate::native_combinator!(|args, pat| pat.repeat_cycles(arg_fraction(args, 0))),
    );
    add_unpatternified(
        r,
        &["rev"],
        REV,
        1,
        true,
        crate::native_combinator!(|_args, pat| pat.rev()),
    );

    add(
        r,
        &["revv"],
        REVV,
        1,
        false,
        crate::native_combinator!(|_args, pat| pat.revv()),
    );

    add(
        r,
        &["palindrome"],
        PALINDROME,
        1,
        true,
        crate::native_combinator!(|_args, pat| c::palindrome(&pat)),
    );

    add(
        r,
        &["brak"],
        BRAK,
        1,
        false,
        crate::native_combinator!(|_args, pat| c::brak(&pat)),
    );

    add(
        r,
        &["swingBy"],
        SWING_BY,
        3,
        false,
        crate::native_combinator!(|args, pat| c::swing_by(
            &pat,
            arg_fraction(args, 0),
            arg_fraction(args, 1)
        )),
    );

    add(
        r,
        &["swing"],
        SWING,
        2,
        false,
        crate::native_combinator!(|args, pat| c::swing(&pat, arg_fraction(args, 0))),
    );

    add(
        r,
        &["ribbon", "rib"],
        RIBBON,
        3,
        false,
        crate::native_combinator!(|args, pat| c::ribbon(
            &pat,
            arg_fraction(args, 0),
            arg_fraction(args, 1)
        )),
    );

    add_fn(
        r,
        &["chunk", "slowchunk", "slowChunk"],
        CHUNK,
        3,
        false,
        crate::native_combinator!(|args, pat| c::chunk(
            &pat,
            arg_number(args, 0) as i64,
            arg_function(args, 1),
            false,
            false
        )),
    );

    add_fn(
        r,
        &["chunkBack", "chunkback"],
        CHUNK_BACK,
        3,
        false,
        crate::native_combinator!(|args, pat| c::chunk(
            &pat,
            arg_number(args, 0) as i64,
            arg_function(args, 1),
            true,
            false
        )),
    );

    add_fn(
        r,
        &["fastchunk", "fastChunk"],
        FASTCHUNK,
        3,
        false,
        crate::native_combinator!(|args, pat| c::chunk(
            &pat,
            arg_number(args, 0) as i64,
            arg_function(args, 1),
            false,
            true
        )),
    );

    add(
        r,
        &["pace"],
        PACE,
        2,
        false,
        crate::native_combinator!(|args, pat| c::pace(&pat, arg_fraction(args, 0))),
    );

    add_fn(
        r,
        &["inside"],
        INSIDE,
        3,
        false,
        crate::native_combinator!(|args, pat| c::inside(
            &pat,
            arg_fraction(args, 0),
            arg_function(args, 1)
        )),
    );

    add_fn(
        r,
        &["outside"],
        OUTSIDE,
        3,
        false,
        crate::native_combinator!(|args, pat| c::outside(
            &pat,
            arg_fraction(args, 0),
            arg_function(args, 1)
        )),
    );

    add_fn(
        r,
        &["off"],
        OFF,
        3,
        false,
        crate::native_combinator!(|args, pat| c::off(
            &pat,
            arg_fraction(args, 0),
            arg_function(args, 1)
        )),
    );

    add(
        r,
        &["echo"],
        ECHO,
        4,
        false,
        crate::native_combinator!(|args, pat| c::echo(
            &pat,
            arg_number(args, 0) as i64,
            arg_fraction(args, 1),
            arg_number(args, 2),
            "echo"
        )),
    );

    add(
        r,
        &["stut"],
        STUT,
        4,
        false,
        crate::native_combinator!(|args, pat| c::echo(
            &pat,
            arg_number(args, 0) as i64,
            arg_fraction(args, 2),
            arg_number(args, 1),
            "stut"
        )),
    );

    add_fn(
        r,
        &["echoWith", "echowith", "stutWith", "stutwith"],
        ECHO_WITH,
        4,
        false,
        crate::native_combinator!(|args, pat| c::echo_with(
            &pat,
            arg_number(args, 0) as i64,
            arg_fraction(args, 1),
            arg_function(args, 2),
            "echoWith"
        )),
    );

    add_fn(
        r,
        &["within"],
        WITHIN,
        4,
        false,
        crate::native_combinator!(|args, pat| c::within(
            &pat,
            arg_fraction(args, 0),
            arg_fraction(args, 1),
            arg_function(args, 2)
        )),
    );

    add(
        r,
        &["loopAtCps", "loopatcps"],
        LOOP_AT_CPS,
        3,
        false,
        crate::native_combinator!(|args, pat| c::loop_at_cps(
            &pat,
            arg_fraction(args, 0),
            arg_number(args, 1)
        )),
    );

    add(
        r,
        &["striate"],
        STRIATE,
        2,
        false,
        crate::native_combinator!(|args, pat| c::striate(&pat, arg_number(args, 0) as i64)),
    );

    add(
        r,
        &["chop"],
        CHOP,
        2,
        false,
        crate::native_combinator!(|args, pat| c::chop(&pat, arg_number(args, 0) as i64)),
    );

    add(
        r,
        &["shuffle"],
        SHUFFLE,
        2,
        false,
        crate::native_combinator!(|args, pat| c::shuffle(&pat, arg_number(args, 0) as i64)),
    );

    add(
        r,
        &["scramble"],
        SCRAMBLE,
        2,
        false,
        crate::native_combinator!(|args, pat| c::scramble(&pat, arg_number(args, 0) as i64)),
    );
}
