//! Presentation-neutral reference metadata for callable score surfaces.
//!
//! Modules expose these static records beside the code they document.
//! Consumers build their own UI or machine-readable output directly from
//! the records, without a separate JSON catalogue to keep in sync.

/// One documented argument.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReferenceParam {
    pub name: &'static str,
    /// The type as a reader sees it, such as `number | Pattern`. Some words
    /// are also read as declarations; the studio's `string_roles` and
    /// `sound_effect` modules say which.
    pub r#type: &'static str,
    pub description: &'static str,
}

/// One named value accepted by a finite-choice reference parameter.
///
/// Kept outside [`ReferenceParam`] so the generated reference tables stay
/// compact. Studio pickers, rendered documentation, and machine-readable
/// documentation all read this same catalogue.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReferenceChoice {
    pub value: &'static str,
    pub description: &'static str,
}

/// The choices belonging to one parameter of one reference entry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReferenceChoiceSet {
    pub entry: &'static str,
    pub parameter: &'static str,
    pub choices: &'static [ReferenceChoice],
    /// Whether a string directly inside this call can be completed from the
    /// choices. Compound calls such as `distort("3:.5:fold")` still show the
    /// choices in their documentation, but need position-aware editing before
    /// a picker can safely replace one field.
    pub completes_string: bool,
}

const LFO_SHAPES: &[ReferenceChoice] = &[
    ReferenceChoice {
        value: "triangle",
        description: "Rises and falls evenly (0; `tri` is a shorter alias).",
    },
    ReferenceChoice {
        value: "sine",
        description: "Smooth, rounded rise and fall (1).",
    },
    ReferenceChoice {
        value: "ramp",
        description: "Rises steadily, then jumps back to the start (2).",
    },
    ReferenceChoice {
        value: "saw",
        description: "Falls steadily, then jumps back to the top (3).",
    },
    ReferenceChoice {
        value: "square",
        description: "Switches abruptly between low and high (4).",
    },
];

const FM_WAVES: &[ReferenceChoice] = &[
    ReferenceChoice {
        value: "sine",
        description: "Smooth fundamental waveform; the default.",
    },
    ReferenceChoice {
        value: "triangle",
        description: "Soft angular waveform with fewer upper harmonics (`tri` is an alias).",
    },
    ReferenceChoice {
        value: "square",
        description: "Hollow waveform containing odd harmonics.",
    },
    ReferenceChoice {
        value: "sawtooth",
        description: "Bright waveform containing both even and odd harmonics (`saw` is an alias).",
    },
    ReferenceChoice {
        value: "white",
        description: "White noise: equal energy per frequency band.",
    },
    ReferenceChoice {
        value: "pink",
        description: "Pink noise: darker, with equal energy per octave.",
    },
    ReferenceChoice {
        value: "brown",
        description: "Brown noise: a deep, smoothly wandering noise.",
    },
    ReferenceChoice {
        value: "crackle",
        description: "Sparse random impulses for a crackling texture.",
    },
];

const ALIGNMENTS: &[ReferenceChoice] = &[
    ReferenceChoice {
        value: "in",
        description: "Uses the left pattern's event timing; the default.",
    },
    ReferenceChoice {
        value: "out",
        description: "Uses the right pattern's event timing.",
    },
    ReferenceChoice {
        value: "mix",
        description: "Keeps event timing from both patterns.",
    },
    ReferenceChoice {
        value: "squeeze",
        description: "Fits the right pattern inside each left event (`squeezein` is an alias).",
    },
    ReferenceChoice {
        value: "squeezeout",
        description: "Fits the left pattern inside each right event.",
    },
    ReferenceChoice {
        value: "reset",
        description: "Restarts the right pattern at the beginning of each left event.",
    },
    ReferenceChoice {
        value: "restart",
        description: "Restarts the left pattern at the beginning of each right event.",
    },
    ReferenceChoice {
        value: "poly",
        description: "Combines both patterns polyphonically without choosing one timing grid.",
    },
];

const WARP_MODES: &[ReferenceChoice] = &[
    ReferenceChoice {
        value: "none",
        description: "Leaves the wavetable phase unchanged (0).",
    },
    ReferenceChoice {
        value: "asym",
        description: "Moves the cycle's halfway point, stretching one half and squeezing the other (1).",
    },
    ReferenceChoice {
        value: "mirror",
        description: "Mirrors the asymmetric warp into a reflected rise-and-fall shape (2).",
    },
    ReferenceChoice {
        value: "bendp",
        description: "Bends phase toward the end of the wavetable (3).",
    },
    ReferenceChoice {
        value: "bendm",
        description: "Bends phase toward the start of the wavetable (4).",
    },
    ReferenceChoice {
        value: "bendmp",
        description: "Moves from a forward bend into a mirrored bend (5).",
    },
    ReferenceChoice {
        value: "sync",
        description: "Repeats the wavetable within each cycle, like oscillator sync (6).",
    },
    ReferenceChoice {
        value: "quant",
        description: "Turns phase into discrete steps for a stair-stepped waveform (7).",
    },
    ReferenceChoice {
        value: "fold",
        description: "Folds repeated phase segments back and forth (8).",
    },
    ReferenceChoice {
        value: "pwm",
        description: "Moves the cycle split point, like pulse-width modulation (9).",
    },
    ReferenceChoice {
        value: "orbit",
        description: "Loops phase around three smooth bends per cycle (10).",
    },
    ReferenceChoice {
        value: "spin",
        description: "Ripples phase with an amount-dependent number of smooth bends (11).",
    },
    ReferenceChoice {
        value: "chaos",
        description: "Blends the original phase toward a chaotic logistic curve (12).",
    },
    ReferenceChoice {
        value: "primes",
        description: "Quantizes phase into a prime number of steps (13).",
    },
    ReferenceChoice {
        value: "binary",
        description: "Reorders phase steps by reversing their binary digits (14).",
    },
    ReferenceChoice {
        value: "brownian",
        description: "Adds smooth, layered random motion to phase (15).",
    },
    ReferenceChoice {
        value: "reciprocal",
        description: "Pulls phase forward with a reciprocal curve (16).",
    },
    ReferenceChoice {
        value: "wormhole",
        description: "Collapses a widening region around the cycle's center (17).",
    },
    ReferenceChoice {
        value: "logistic",
        description: "Runs phase through the logistic map for a sharper chaotic fold (18).",
    },
    ReferenceChoice {
        value: "sigmoid",
        description: "Bends phase into a smooth S-curve (19).",
    },
    ReferenceChoice {
        value: "fractal",
        description: "Adds repeating sine detail and wraps it around the cycle (20).",
    },
    ReferenceChoice {
        value: "flip",
        description: "Inverts the sample for the opening part of each cycle (21).",
    },
];

const FILTER_TYPES: &[ReferenceChoice] = &[
    ReferenceChoice {
        value: "12db",
        description: "Gentle 12 dB/octave filter: one biquad stage. Numbers 0 and 1 also select it.",
    },
    ReferenceChoice {
        value: "ladder",
        description: "Aggressive resonant four-pole ladder filter.",
    },
    ReferenceChoice {
        value: "24db",
        description: "Steep 24 dB/octave filter: two biquad stages. Number 2 also selects it.",
    },
];

const PITCH_CURVES: &[ReferenceChoice] = &[
    ReferenceChoice {
        value: "0",
        description: "Linear: pitch moves at a constant rate.",
    },
    ReferenceChoice {
        value: "1",
        description: "Exponential: pitch bends quickly, especially useful for kick drops.",
    },
];

const VOICING_MODES: &[ReferenceChoice] = &[
    ReferenceChoice {
        value: "below",
        description: "Places the voicing's highest note at or below the anchor.",
    },
    ReferenceChoice {
        value: "above",
        description: "Places the voicing's lowest note at or above the anchor.",
    },
    ReferenceChoice {
        value: "duck",
        description: "Like `below`, but removes a note that exactly matches the anchor.",
    },
    ReferenceChoice {
        value: "root",
        description: "Uses the dictionary's first voicing and places its root at or above the anchor.",
    },
    ReferenceChoice {
        value: "oldabove",
        description: "Legacy `above` placement, kept for older scores.",
    },
    ReferenceChoice {
        value: "oldroot",
        description: "Legacy `root` placement, kept for older scores.",
    },
];

const LIMITER_CHARACTERS: &[ReferenceChoice] = &[
    ReferenceChoice {
        value: "transparent",
        description: "Longer lookahead and gentle recovery; keeps the source natural.",
    },
    ReferenceChoice {
        value: "punchy",
        description: "Fast recovery between hits; keeps transients pointed.",
    },
    ReferenceChoice {
        value: "warm",
        description: "Slow recovery that rides the phrase rather than each hit.",
    },
    ReferenceChoice {
        value: "hard",
        description: "Shortest lookahead and fastest recovery; deliberately audible.",
    },
];

const DISTORTION_TYPES: &[ReferenceChoice] = &[
    ReferenceChoice {
        value: "scurve",
        description: "Smooth S-shaped saturation; the default.",
    },
    ReferenceChoice {
        value: "soft",
        description: "Rounds peaks with tanh-style soft clipping.",
    },
    ReferenceChoice {
        value: "hard",
        description: "Clamps peaks flat for hard clipping.",
    },
    ReferenceChoice {
        value: "cubic",
        description: "Adds a fuller cubic bend before soft saturation.",
    },
    ReferenceChoice {
        value: "diode",
        description: "Balanced diode-like clipping.",
    },
    ReferenceChoice {
        value: "asym",
        description: "Clips positive and negative halves differently, adding even harmonics.",
    },
    ReferenceChoice {
        value: "fold",
        description: "Folds the signal back when it crosses the limits.",
    },
    ReferenceChoice {
        value: "sinefold",
        description: "Rounds a folded signal through a sine curve.",
    },
    ReferenceChoice {
        value: "chebyshev",
        description: "Builds dense upper harmonics with polynomial waveshaping.",
    },
];

const SPEED_UNITS: &[ReferenceChoice] = &[
    ReferenceChoice {
        value: "r",
        description: "Rate: `speed` is a direct playback-rate multiplier (default).",
    },
    ReferenceChoice {
        value: "c",
        description: "Cycles: stretches the sample so `speed` is its duration in cycles.",
    },
    ReferenceChoice {
        value: "s",
        description: "Seconds: asks for a duration in seconds; native playback currently treats it like rate.",
    },
];

const VOWELS: &[ReferenceChoice] = &[
    ReferenceChoice {
        value: "a",
        description: "Open 'ah' vowel [a].",
    },
    ReferenceChoice {
        value: "e",
        description: "Mid-front 'eh' vowel [e].",
    },
    ReferenceChoice {
        value: "i",
        description: "Close-front 'ee' vowel [i].",
    },
    ReferenceChoice {
        value: "o",
        description: "Rounded 'oh' vowel [o].",
    },
    ReferenceChoice {
        value: "u",
        description: "Close rounded 'oo' vowel [u].",
    },
    ReferenceChoice {
        value: "ae",
        description: "Flat 'a' as in cat [æ]; `æ` is an alias.",
    },
    ReferenceChoice {
        value: "aa",
        description: "Broad 'a' as in father [ɑ]; `ɑ` and `å` are aliases.",
    },
    ReferenceChoice {
        value: "oe",
        description: "Front rounded vowel [ø]; `ø` and `ö` are aliases.",
    },
    ReferenceChoice {
        value: "ue",
        description: "Close front rounded vowel [y]; `ü` is an alias.",
    },
    ReferenceChoice {
        value: "y",
        description: "Close unrounded vowel [ɯ]; `ı` is an alias.",
    },
    ReferenceChoice {
        value: "uh",
        description: "Central 'uh' vowel [ʌ].",
    },
    ReferenceChoice {
        value: "un",
        description: "French-like nasal 'un' [œ̃].",
    },
    ReferenceChoice {
        value: "en",
        description: "French-like nasal 'in' [ɛ̃].",
    },
    ReferenceChoice {
        value: "an",
        description: "French-like nasal 'an' [ɑ̃].",
    },
    ReferenceChoice {
        value: "on",
        description: "French-like nasal 'on' [ɔ̃].",
    },
];

const FM_ENVELOPES: &[ReferenceChoice] = &[
    ReferenceChoice {
        value: "lin",
        description: "Linear: modulation depth moves at a constant rate (`linear` is an alias).",
    },
    ReferenceChoice {
        value: "exp",
        description: "Exponential: modulation depth changes fastest near the start.",
    },
];

const RNG_MODES: &[ReferenceChoice] = &[
    ReferenceChoice {
        value: "legacy",
        description: "Historical generator; preserves the sequences older scores expect.",
    },
    ReferenceChoice {
        value: "precise",
        description: "Newer generator with better statistical quality; sequences differ from legacy.",
    },
];

/// Finite choices documented by an entry and parameter.
pub const REFERENCE_CHOICE_SETS: &[ReferenceChoiceSet] = &[
    ReferenceChoiceSet {
        entry: "warpmode",
        parameter: "mode",
        choices: WARP_MODES,
        completes_string: true,
    },
    ReferenceChoiceSet {
        entry: "wtshape",
        parameter: "shape",
        choices: LFO_SHAPES,
        completes_string: true,
    },
    ReferenceChoiceSet {
        entry: "lfo",
        parameter: "config.shape",
        choices: LFO_SHAPES,
        completes_string: true,
    },
    ReferenceChoiceSet {
        entry: "warpshape",
        parameter: "shape",
        choices: LFO_SHAPES,
        completes_string: true,
    },
    ReferenceChoiceSet {
        entry: "tremoloshape",
        parameter: "shape",
        choices: LFO_SHAPES,
        completes_string: true,
    },
    ReferenceChoiceSet {
        entry: "lpshape",
        parameter: "shape",
        choices: LFO_SHAPES,
        completes_string: true,
    },
    ReferenceChoiceSet {
        entry: "bpshape",
        parameter: "shape",
        choices: LFO_SHAPES,
        completes_string: true,
    },
    ReferenceChoiceSet {
        entry: "hpshape",
        parameter: "shape",
        choices: LFO_SHAPES,
        completes_string: true,
    },
    ReferenceChoiceSet {
        entry: "ftype",
        parameter: "type",
        choices: FILTER_TYPES,
        completes_string: true,
    },
    ReferenceChoiceSet {
        entry: "pcurve",
        parameter: "type",
        choices: PITCH_CURVES,
        completes_string: true,
    },
    ReferenceChoiceSet {
        entry: "mode",
        parameter: "modeName",
        choices: VOICING_MODES,
        completes_string: true,
    },
    ReferenceChoiceSet {
        entry: "limit",
        parameter: "character",
        choices: LIMITER_CHARACTERS,
        completes_string: false,
    },
    ReferenceChoiceSet {
        entry: "limitchar",
        parameter: "character",
        choices: LIMITER_CHARACTERS,
        completes_string: true,
    },
    ReferenceChoiceSet {
        entry: "distort",
        parameter: "type",
        choices: DISTORTION_TYPES,
        completes_string: false,
    },
    ReferenceChoiceSet {
        entry: "distorttype",
        parameter: "type",
        choices: DISTORTION_TYPES,
        completes_string: true,
    },
    ReferenceChoiceSet {
        entry: "unit",
        parameter: "unit",
        choices: SPEED_UNITS,
        completes_string: true,
    },
    ReferenceChoiceSet {
        entry: "vowel",
        parameter: "vowel",
        choices: VOWELS,
        completes_string: true,
    },
    ReferenceChoiceSet {
        entry: "fmenv",
        parameter: "type",
        choices: FM_ENVELOPES,
        completes_string: true,
    },
    ReferenceChoiceSet {
        entry: "fmenv2",
        parameter: "value",
        choices: FM_ENVELOPES,
        completes_string: true,
    },
    ReferenceChoiceSet {
        entry: "fmenv3",
        parameter: "value",
        choices: FM_ENVELOPES,
        completes_string: true,
    },
    ReferenceChoiceSet {
        entry: "fmenv4",
        parameter: "value",
        choices: FM_ENVELOPES,
        completes_string: true,
    },
    ReferenceChoiceSet {
        entry: "fmenv5",
        parameter: "value",
        choices: FM_ENVELOPES,
        completes_string: true,
    },
    ReferenceChoiceSet {
        entry: "fmenv6",
        parameter: "value",
        choices: FM_ENVELOPES,
        completes_string: true,
    },
    ReferenceChoiceSet {
        entry: "fmenv7",
        parameter: "value",
        choices: FM_ENVELOPES,
        completes_string: true,
    },
    ReferenceChoiceSet {
        entry: "fmenv8",
        parameter: "value",
        choices: FM_ENVELOPES,
        completes_string: true,
    },
    ReferenceChoiceSet {
        entry: "fmwave",
        parameter: "wave",
        choices: FM_WAVES,
        completes_string: true,
    },
    ReferenceChoiceSet {
        entry: "fmwave2",
        parameter: "value",
        choices: FM_WAVES,
        completes_string: true,
    },
    ReferenceChoiceSet {
        entry: "fmwave3",
        parameter: "value",
        choices: FM_WAVES,
        completes_string: true,
    },
    ReferenceChoiceSet {
        entry: "fmwave4",
        parameter: "value",
        choices: FM_WAVES,
        completes_string: true,
    },
    ReferenceChoiceSet {
        entry: "fmwave5",
        parameter: "value",
        choices: FM_WAVES,
        completes_string: true,
    },
    ReferenceChoiceSet {
        entry: "fmwave6",
        parameter: "value",
        choices: FM_WAVES,
        completes_string: true,
    },
    ReferenceChoiceSet {
        entry: "fmwave7",
        parameter: "value",
        choices: FM_WAVES,
        completes_string: true,
    },
    ReferenceChoiceSet {
        entry: "fmwave8",
        parameter: "value",
        choices: FM_WAVES,
        completes_string: true,
    },
    ReferenceChoiceSet {
        entry: "setDefaultJoin",
        parameter: "method",
        choices: ALIGNMENTS,
        completes_string: true,
    },
    ReferenceChoiceSet {
        entry: "useRNG",
        parameter: "mod",
        choices: RNG_MODES,
        completes_string: true,
    },
];

pub fn reference_choices(entry: &str, parameter: &str) -> Option<&'static ReferenceChoiceSet> {
    REFERENCE_CHOICE_SETS
        .iter()
        .find(|set| set.entry == entry && set.parameter == parameter)
}

/// Documentation for one callable score name.
///
/// All data is static so a compiled extension contributes no parsing,
/// allocation, or file access when its reference entry is discovered.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReferenceEntry {
    pub name: &'static str,
    pub synonyms: &'static [&'static str],
    pub summary: &'static str,
    pub description: &'static str,
    pub params: &'static [ReferenceParam],
    pub examples: &'static [&'static str],
    /// The headings the entry is listed under; the first is where it is
    /// filed. Some tags are also read as declarations: `combiners` and
    /// `selectors` by the studio's `sound_effect`, `selectors` by the lint's
    /// `rebinds_words`, and `sound` by the studio reference's
    /// `Entry::inserts_as_call`.
    pub tags: &'static [&'static str],
    pub no_autocomplete: bool,
    pub deprecated: bool,
    /// Stable lower-case grouping key such as `rustel` or an extension ID.
    pub origin: &'static str,
}

impl ReferenceEntry {
    /// An entry spelling out nothing but its name. Authors fill in only what a
    /// name actually has, so a control with no documented argument does not
    /// carry an empty-parameter apology and a name upstream never documented
    /// can still own a truthful summary. `origin` is set by the owner, not by
    /// the blank, so it stays required at the literal site.
    pub const fn blank(name: &'static str) -> Self {
        Self {
            name,
            synonyms: &[],
            summary: "",
            description: "",
            params: &[],
            examples: &[],
            tags: &[],
            no_autocomplete: false,
            deprecated: false,
            origin: "",
        }
    }
}
