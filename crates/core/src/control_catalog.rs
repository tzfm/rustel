//! The pinned control surface, documented where it is installed.
//!
//! Each row installs one control: its accessor names, its aliases, and the
//! reference entry a musician reads about it. A control cannot exist without
//! an entry because the entry is a field of the row.
//!
//! Includes documentation text from the Strudel project (AGPL-3.0-or-later),
//! https://strudel.cc, alongside descriptions written for Rustel. The
//! `origin` field is `rustel` for both; it does not distinguish authorship.
//!
//! `REFERENCE_HIDDEN` lists compatibility controls omitted from the studio's
//! reference. They remain installed so existing scores can evaluate. The
//! combinator registry has a corresponding list in
//! `register::COMBINATORS_REFERENCE_HIDDEN`.

use crate::reference::{ReferenceEntry, ReferenceParam};

/// One installed control: the names that set it, and the reference entry
/// that documents it.
///
/// Compound controls spread a `:`-separated mini-notation atom across their
/// positional names, so `s` maps "bd:3:0.5" onto s/n/gain. Only the FIRST
/// positional name is an accessor; the others are output keys, and each one
/// that a score can spell has its own row. Aliases resolve to the same
/// control.
pub struct ControlRow {
    pub names: &'static [&'static str],
    pub aliases: &'static [&'static str],
    pub reference: ReferenceEntry,
}

/// Shared documentation for the `limit` control and its extension entry.
///
/// Studio identifies extensions through the extension registry, not a
/// control's `origin` field. `rustel-ext` includes this entry so both places
/// use the same description.
pub const LIMIT_REFERENCE: ReferenceEntry = ReferenceEntry {
    name: "limit",
    synonyms: &[],
    summary: "Brickwall limiter on one voice: a ceiling in dBFS, and a character.",
    description: "A brickwall limiter on one voice. Called as `limit(-6, \"hard\")` or as `limit(\"-6:hard\")`: the ceiling in dBFS first, then the character - `transparent`, `punchy`, `warm` or `hard` - which may be left off. The ceiling is an ordinary argument, so `limit(slider(-6, -24, 0), \"hard\")` puts a fader on it.\n\nIt holds one voice at a time, so several voices each under the ceiling can still add up over it. Use `all(x => x.limit(\"-3\"))` to put it across the whole stack.\n\nOne sample of lookahead, the same for every character, so a limited voice stays in time with an unlimited one.\n\nIt runs after the voice\'s gain and `stretch`, and before its delay and reverb sends.",
    params: &[
        ReferenceParam {
            name: "ceiling",
            r#type: "number | Pattern",
            description: "the ceiling in dBFS, at or below 0",
        },
        ReferenceParam {
            name: "character",
            r#type: "string | Pattern",
            description: "transparent, punchy, warm or hard",
        },
    ],
    examples: &[
        "s(\"bd*4\").distort(\"8:.3\").limit(\"-6\")",
        "s(\"bd*2, hh*8\").limit(-9, \"hard\")",
        "s(\"bd*4\").distort(\"8:.3\").limit(slider(-6, -24, 0), \"hard\")",
    ],
    tags: &["dynamics", "audio"],
    no_autocomplete: false,
    deprecated: false,
    origin: "rustel",
};

/// The complete pinned control surface.
pub static CONTROLS: &[ControlRow] = &[
    ControlRow {
        names: &["s", "n", "gain"],
        aliases: &["sound"],
        reference: ReferenceEntry {
            name: "s",
            synonyms: &["sound"],
            summary: "Select a sound / sample by name.",
            description: "Select a sound / sample by name. When using mininotation, you can also optionally supply 'n' and 'gain' parameters\nseparated by ':'.",
            params: &[
                ReferenceParam {
                    name: "sound",
                    r#type: "string | Pattern",
                    description: "The sound / pattern of sounds to pick",
                },
            ],
            examples: &[
                "s(\"bd hh\")",
                "s(\"bd:0 bd:1 bd:0:0.3 bd:1:1.4\")",
            ],
            tags: &["audio", "samples"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["wt"],
        aliases: &["wavetablePosition"],
        reference: ReferenceEntry {
            name: "wt",
            synonyms: &["wavetablePosition"],
            summary: "Position between the frames of a loaded wavetable.",
            description: "Defaults to 0, the first frame; 1 selects the last. The renderer interpolates between frames, adds the position envelope and LFO, then clamps the result to 0..1. A table needs different frames for moving its position to change the sound. Only `wt_*` wavetable sounds consume this control; ordinary samples, `gm_*` soundfonts, and other synths ignore it.",
            params: &[
                ReferenceParam {
                    name: "position",
                    r#type: "number | Pattern",
                    description: "Position in the wavetable from 0 to 1",
                },
            ],
            examples: &[
                "s(\"curses\").bank(\"wt_digital\").seg(8).note(\"F1\").wt(\"0 0.25 0.5 0.75 1\")",
            ],
            tags: &["wavetable", "audio"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["wtenv"],
        aliases: &[],
        reference: ReferenceEntry {
            name: "wtenv",
            synonyms: &[],
            summary: "Wavetable position envelope amount, in normalized units.",
            description: "Adds a linear ADSR envelope to wt. The amount defaults to 0, or 0.5 when any wtattack, wtdecay, wtsustain, or wtrelease is set; an explicit 0 disables it. With no ADSR fields, attack/decay/sustain/release are 0 s, 0.5 s, 0, and 0.1 s. Once any field is set, omitted attack/decay become 0.001 s and release 0.01 s; omitted sustain is 0.001 when decay is set, otherwise 1. The final position is clamped to 0..1. Only `wt_*` wavetable sounds consume this control; ordinary samples, `gm_*` soundfonts, and other synths ignore it.",
            params: &[
                ReferenceParam {
                    name: "amount",
                    r#type: "number | Pattern",
                    description: "between 0 and 1",
                },
            ],
            examples: &[],
            tags: &["wavetable", "envelope", "audio"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["wtattack"],
        aliases: &["wtatt"],
        reference: ReferenceEntry {
            name: "wtattack",
            synonyms: &["wtatt"],
            summary: "Wavetable position envelope attack time in seconds.",
            description: "The attack time in seconds of the position envelope. The default is 0 s when all ADSR fields are absent; an explicit or implicitly omitted value is floored at 0.001 s once any field is set. Setting this field supplies wtenv(0.5) unless an amount is explicit; wtenv(0) disables the envelope. See wtenv for the full ADSR defaults. Only `wt_*` wavetable sounds consume this control; ordinary samples, `gm_*` soundfonts, and other synths ignore it.",
            params: &[
                ReferenceParam {
                    name: "time",
                    r#type: "number | Pattern",
                    description: "attack time in seconds",
                },
            ],
            examples: &[],
            tags: &["wavetable", "envelope", "audio"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["wtdecay"],
        aliases: &["wtdec"],
        reference: ReferenceEntry {
            name: "wtdecay",
            synonyms: &["wtdec"],
            summary: "Wavetable position envelope decay time in seconds.",
            description: "The decay time in seconds of the position envelope. The default is 0.5 s when all ADSR fields are absent; an explicit or implicitly omitted value is floored at 0.001 s once any field is set. Setting this field supplies wtenv(0.5) unless an amount is explicit; wtenv(0) disables the envelope. See wtenv for the full ADSR defaults. Only `wt_*` wavetable sounds consume this control; ordinary samples, `gm_*` soundfonts, and other synths ignore it.",
            params: &[
                ReferenceParam {
                    name: "time",
                    r#type: "number | Pattern",
                    description: "decay time in seconds",
                },
            ],
            examples: &[],
            tags: &["wavetable", "envelope", "audio"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["wtsustain"],
        aliases: &["wtsus"],
        reference: ReferenceEntry {
            name: "wtsustain",
            synonyms: &["wtsus"],
            summary: "Wavetable position envelope sustain level.",
            description: "The sustain level, capped at 1, of the position envelope. The default is 0 when all ADSR fields are absent; otherwise an omitted value is 0.001 when decay is set, or 1 without decay. Setting this field supplies wtenv(0.5) unless an amount is explicit; wtenv(0) disables the envelope. See wtenv for the full ADSR defaults. Only `wt_*` wavetable sounds consume this control; ordinary samples, `gm_*` soundfonts, and other synths ignore it.",
            params: &[
                ReferenceParam {
                    name: "gain",
                    r#type: "number | Pattern",
                    description: "sustain level (0 to 1)",
                },
            ],
            examples: &[],
            tags: &["wavetable", "envelope", "audio"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["wtrelease"],
        aliases: &["wtrel"],
        reference: ReferenceEntry {
            name: "wtrelease",
            synonyms: &["wtrel"],
            summary: "Wavetable position envelope release time in seconds.",
            description: "The release time in seconds of the position envelope. The default is 0.1 s when all ADSR fields are absent; an explicit or implicitly omitted value is floored at 0.01 s once any field is set. Setting this field supplies wtenv(0.5) unless an amount is explicit; wtenv(0) disables the envelope. See wtenv for the full ADSR defaults. Only `wt_*` wavetable sounds consume this control; ordinary samples, `gm_*` soundfonts, and other synths ignore it.",
            params: &[
                ReferenceParam {
                    name: "time",
                    r#type: "number | Pattern",
                    description: "release time in seconds",
                },
            ],
            examples: &[],
            tags: &["wavetable", "envelope", "audio"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["wtrate"],
        aliases: &[],
        reference: ReferenceEntry {
            name: "wtrate",
            synonyms: &[],
            summary: "Wavetable position LFO rate in hertz.",
            description: "Defaults to 1 Hz. wtsync overrides this rate when both are set. Naming wtrate, wtsync, wtshape, or wtskew supplies depth 0.5 unless wtdepth is explicit; otherwise depth defaults to 0. An explicit zero depth disables the LFO. Only `wt_*` wavetable sounds consume this control; ordinary samples, `gm_*` soundfonts, and other synths ignore it.",
            params: &[
                ReferenceParam {
                    name: "rate",
                    r#type: "number | Pattern",
                    description: "rate in hertz",
                },
            ],
            examples: &[],
            tags: &["wavetable", "lfo", "audio"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["wtsync"],
        aliases: &[],
        reference: ReferenceEntry {
            name: "wtsync",
            synonyms: &[],
            summary: "Wavetable position LFO oscillations per cycle.",
            description: "Multiplies the current cycles per second to obtain hertz and overrides wtrate. It is absent by default; without it the rate is wtrate, default 1 Hz. Naming wtrate, wtsync, wtshape, or wtskew supplies depth 0.5 unless wtdepth is explicit; otherwise depth defaults to 0. An explicit zero depth disables the LFO. Only `wt_*` wavetable sounds consume this control; ordinary samples, `gm_*` soundfonts, and other synths ignore it.",
            params: &[
                ReferenceParam {
                    name: "rate",
                    r#type: "number | Pattern",
                    description: "oscillations per cycle",
                },
            ],
            examples: &[],
            tags: &["wavetable", "lfo", "audio"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["wtdepth"],
        aliases: &[],
        reference: ReferenceEntry {
            name: "wtdepth",
            synonyms: &[],
            summary: "Wavetable position LFO depth in normalized units.",
            description: "Naming wtrate, wtsync, wtshape, or wtskew supplies depth 0.5 unless wtdepth is explicit; otherwise depth defaults to 0. An explicit zero depth disables the LFO. A nonzero depth alone uses a 1 Hz triangle with skew 0.5 and DC offset 0. The LFO is added to wt, then the result is clamped to 0..1. Only `wt_*` wavetable sounds consume this control; ordinary samples, `gm_*` soundfonts, and other synths ignore it.",
            params: &[
                ReferenceParam {
                    name: "depth",
                    r#type: "number | Pattern",
                    description: "depth of modulation",
                },
            ],
            examples: &[],
            tags: &["wavetable", "lfo", "audio"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["wtshape"],
        aliases: &[],
        reference: ReferenceEntry {
            name: "wtshape",
            synonyms: &[],
            summary: "Wavetable position LFO waveform.",
            description: "Defaults to triangle. Accepts triangle/tri (0), sine (1), ramp (2), saw (3), or square (4); numeric values wrap over these five shapes. Naming wtrate, wtsync, wtshape, or wtskew supplies depth 0.5 unless wtdepth is explicit; otherwise depth defaults to 0. An explicit zero depth disables the LFO. Only `wt_*` wavetable sounds consume this control; ordinary samples, `gm_*` soundfonts, and other synths ignore it.",
            params: &[
                ReferenceParam {
                    name: "shape",
                    r#type: "number | string | Pattern",
                    description: "triangle/tri (0), sine (1), ramp (2), saw (3), or square (4)",
                },
            ],
            examples: &[],
            tags: &["wavetable", "lfo", "audio"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["wtdc"],
        aliases: &[],
        reference: ReferenceEntry {
            name: "wtdc",
            synonyms: &[],
            summary: "Wavetable position LFO offset before depth scaling.",
            description: "Defaults to 0. The dimensionless offset is added to the LFO waveform before multiplying by wtdepth. It has no effect when depth is 0, and naming this control alone does not activate the LFO. Set wtrate or a nonzero wtdepth. Only `wt_*` wavetable sounds consume this control; ordinary samples, `gm_*` soundfonts, and other synths ignore it.",
            params: &[
                ReferenceParam {
                    name: "dcoffset",
                    r#type: "number | Pattern",
                    description: "dc offset. set to 0 for unipolar",
                },
            ],
            examples: &[],
            tags: &["wavetable", "lfo", "audio"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["wtskew"],
        aliases: &[],
        reference: ReferenceEntry {
            name: "wtskew",
            synonyms: &[],
            summary: "Wavetable position LFO shape skew.",
            description: "Defaults to 0.5, the centered shape. Values toward 0 or 1 skew its shape or duty cycle; this is a dimensionless shape parameter. Naming wtrate, wtsync, wtshape, or wtskew supplies depth 0.5 unless wtdepth is explicit; otherwise depth defaults to 0. An explicit zero depth disables the LFO. Only `wt_*` wavetable sounds consume this control; ordinary samples, `gm_*` soundfonts, and other synths ignore it.",
            params: &[
                ReferenceParam {
                    name: "skew",
                    r#type: "number | Pattern",
                    description: "How much to bend the LFO shape",
                },
            ],
            examples: &[],
            tags: &["wavetable", "lfo", "audio"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["warp"],
        aliases: &["wavetableWarp"],
        reference: ReferenceEntry {
            name: "warp",
            synonyms: &["wavetableWarp"],
            summary: "Base waveform-warp amount for a wavetable.",
            description: "Defaults to 0. The warp envelope and LFO add to this base, then the amount is clamped to 0..1. Choose a non-none warpmode, for example warpmode(\"sync\").warp(0.5); the default mode leaves the waveform unchanged even when warp is nonzero. Only `wt_*` wavetable sounds consume this control; ordinary samples, `gm_*` soundfonts, and other synths ignore it.",
            params: &[
                ReferenceParam {
                    name: "amount",
                    r#type: "number | Pattern",
                    description: "Warp of the wavetable from 0 to 1",
                },
            ],
            examples: &[
                "s(\"basique\").bank(\"wt_digital\").seg(8).note(\"F1\").warp(\"0 0.25 0.5 0.75 1\")\n  .warpmode(\"spin\")",
            ],
            tags: &["wavetable", "audio"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["warpattack"],
        aliases: &["warpatt"],
        reference: ReferenceEntry {
            name: "warpattack",
            synonyms: &["warpatt"],
            summary: "Wavetable warp envelope attack time in seconds.",
            description: "The attack time in seconds of the warp envelope. The default is 0 s when all ADSR fields are absent; an explicit or implicitly omitted value is floored at 0.001 s once any field is set. Setting this field supplies warpenv(0.5) unless an amount is explicit; warpenv(0) disables the envelope. See warpenv for the full ADSR defaults. Choose a non-none warpmode to hear the warp. Only `wt_*` wavetable sounds consume this control; ordinary samples, `gm_*` soundfonts, and other synths ignore it.",
            params: &[
                ReferenceParam {
                    name: "time",
                    r#type: "number | Pattern",
                    description: "attack time in seconds",
                },
            ],
            examples: &[],
            tags: &["wavetable", "envelope", "audio"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["warpdecay"],
        aliases: &["warpdec"],
        reference: ReferenceEntry {
            name: "warpdecay",
            synonyms: &["warpdec"],
            summary: "Wavetable warp envelope decay time in seconds.",
            description: "The decay time in seconds of the warp envelope. The default is 0.5 s when all ADSR fields are absent; an explicit or implicitly omitted value is floored at 0.001 s once any field is set. Setting this field supplies warpenv(0.5) unless an amount is explicit; warpenv(0) disables the envelope. See warpenv for the full ADSR defaults. Choose a non-none warpmode to hear the warp. Only `wt_*` wavetable sounds consume this control; ordinary samples, `gm_*` soundfonts, and other synths ignore it.",
            params: &[
                ReferenceParam {
                    name: "time",
                    r#type: "number | Pattern",
                    description: "decay time in seconds",
                },
            ],
            examples: &[],
            tags: &["wavetable", "envelope", "audio"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["warpsustain"],
        aliases: &["warpsus"],
        reference: ReferenceEntry {
            name: "warpsustain",
            synonyms: &["warpsus"],
            summary: "Wavetable warp envelope sustain level.",
            description: "The sustain level, capped at 1, of the warp envelope. The default is 0 when all ADSR fields are absent; otherwise an omitted value is 0.001 when decay is set, or 1 without decay. Setting this field supplies warpenv(0.5) unless an amount is explicit; warpenv(0) disables the envelope. See warpenv for the full ADSR defaults. Choose a non-none warpmode to hear the warp. Only `wt_*` wavetable sounds consume this control; ordinary samples, `gm_*` soundfonts, and other synths ignore it.",
            params: &[
                ReferenceParam {
                    name: "gain",
                    r#type: "number | Pattern",
                    description: "sustain level (0 to 1)",
                },
            ],
            examples: &[],
            tags: &["wavetable", "envelope", "audio"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["warprelease"],
        aliases: &["warprel"],
        reference: ReferenceEntry {
            name: "warprelease",
            synonyms: &["warprel"],
            summary: "Wavetable warp envelope release time in seconds.",
            description: "The release time in seconds of the warp envelope. The default is 0.1 s when all ADSR fields are absent; an explicit or implicitly omitted value is floored at 0.01 s once any field is set. Setting this field supplies warpenv(0.5) unless an amount is explicit; warpenv(0) disables the envelope. See warpenv for the full ADSR defaults. Choose a non-none warpmode to hear the warp. Only `wt_*` wavetable sounds consume this control; ordinary samples, `gm_*` soundfonts, and other synths ignore it.",
            params: &[
                ReferenceParam {
                    name: "time",
                    r#type: "number | Pattern",
                    description: "release time in seconds",
                },
            ],
            examples: &[],
            tags: &["wavetable", "envelope", "audio"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["warprate"],
        aliases: &[],
        reference: ReferenceEntry {
            name: "warprate",
            synonyms: &[],
            summary: "Wavetable warp LFO rate in hertz.",
            description: "Defaults to 1 Hz. warpsync overrides this rate when both are set. Naming warprate, warpsync, warpshape, or warpskew supplies depth 0.5 unless warpdepth is explicit; otherwise depth defaults to 0. An explicit zero depth disables the LFO. Choose a non-none warpmode to hear the warp. Only `wt_*` wavetable sounds consume this control; ordinary samples, `gm_*` soundfonts, and other synths ignore it.",
            params: &[
                ReferenceParam {
                    name: "rate",
                    r#type: "number | Pattern",
                    description: "rate in hertz",
                },
            ],
            examples: &[],
            tags: &["wavetable", "lfo", "audio"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["warpdepth"],
        aliases: &[],
        reference: ReferenceEntry {
            name: "warpdepth",
            synonyms: &[],
            summary: "Wavetable warp LFO depth in normalized units.",
            description: "Naming warprate, warpsync, warpshape, or warpskew supplies depth 0.5 unless warpdepth is explicit; otherwise depth defaults to 0. An explicit zero depth disables the LFO. A nonzero depth alone uses a 1 Hz triangle with skew 0.5 and DC offset 0. The LFO is added to warp, then the result is clamped to 0..1. Choose a non-none warpmode to hear the warp. Only `wt_*` wavetable sounds consume this control; ordinary samples, `gm_*` soundfonts, and other synths ignore it.",
            params: &[
                ReferenceParam {
                    name: "depth",
                    r#type: "number | Pattern",
                    description: "depth of modulation",
                },
            ],
            examples: &[],
            tags: &["wavetable", "lfo", "audio"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["warpshape"],
        aliases: &[],
        reference: ReferenceEntry {
            name: "warpshape",
            synonyms: &[],
            summary: "Wavetable warp LFO waveform.",
            description: "Defaults to triangle. Accepts triangle/tri (0), sine (1), ramp (2), saw (3), or square (4); numeric values wrap over these five shapes. Naming warprate, warpsync, warpshape, or warpskew supplies depth 0.5 unless warpdepth is explicit; otherwise depth defaults to 0. An explicit zero depth disables the LFO. Choose a non-none warpmode to hear the warp. Only `wt_*` wavetable sounds consume this control; ordinary samples, `gm_*` soundfonts, and other synths ignore it.",
            params: &[
                ReferenceParam {
                    name: "shape",
                    r#type: "number | string | Pattern",
                    description: "triangle/tri (0), sine (1), ramp (2), saw (3), or square (4)",
                },
            ],
            examples: &[],
            tags: &["wavetable", "lfo", "audio"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["warpdc"],
        aliases: &[],
        reference: ReferenceEntry {
            name: "warpdc",
            synonyms: &[],
            summary: "Wavetable warp LFO offset before depth scaling.",
            description: "Defaults to 0. The dimensionless offset is added to the LFO waveform before multiplying by warpdepth. It has no effect when depth is 0, and naming this control alone does not activate the LFO. Set warprate or a nonzero warpdepth. Choose a non-none warpmode to hear the warp. Only `wt_*` wavetable sounds consume this control; ordinary samples, `gm_*` soundfonts, and other synths ignore it.",
            params: &[
                ReferenceParam {
                    name: "dcoffset",
                    r#type: "number | Pattern",
                    description: "dc offset. set to 0 for unipolar",
                },
            ],
            examples: &[],
            tags: &["wavetable", "lfo", "audio"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["warpskew"],
        aliases: &[],
        reference: ReferenceEntry {
            name: "warpskew",
            synonyms: &[],
            summary: "Wavetable warp LFO shape skew.",
            description: "Defaults to 0.5, the centered shape. Values toward 0 or 1 skew its shape or duty cycle; this is a dimensionless shape parameter. Naming warprate, warpsync, warpshape, or warpskew supplies depth 0.5 unless warpdepth is explicit; otherwise depth defaults to 0. An explicit zero depth disables the LFO. Choose a non-none warpmode to hear the warp. Only `wt_*` wavetable sounds consume this control; ordinary samples, `gm_*` soundfonts, and other synths ignore it.",
            params: &[
                ReferenceParam {
                    name: "skew",
                    r#type: "number | Pattern",
                    description: "How much to bend the LFO shape",
                },
            ],
            examples: &[],
            tags: &["wavetable", "lfo", "audio"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["warpmode"],
        aliases: &["wavetableWarpMode"],
        reference: ReferenceEntry {
            name: "warpmode",
            synonyms: &["wavetableWarpMode"],
            summary: "Select the waveform-warp algorithm for a wavetable.",
            description: "Defaults to none; unknown names or indices also select none. Choose a non-none mode to transform the waveform. Its effect at warp(0) depends on the mode; only none always bypasses the transformation. Use warp for the base amount, warpenv for its envelope, or warprate with warpdepth for an LFO. Only `wt_*` wavetable sounds consume this control; ordinary samples, `gm_*` soundfonts, and other synths ignore it.",
            params: &[
                ReferenceParam {
                    name: "mode",
                    r#type: "number | string | Pattern",
                    description: "Warp mode",
                },
            ],
            examples: &[
                "s(\"crickets\").bank(\"wt_digital\").seg(8).note(\"F1\").warp(\"0 0.25 0.5 0.75 1\")\n  .warpmode(\"<asym bendp spin logistic sync wormhole brownian>*2\")",
            ],
            tags: &["wavetable", "audio"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["wtphaserand"],
        aliases: &["wavetablePhaseRand"],
        reference: ReferenceEntry {
            name: "wtphaserand",
            synonyms: &["wavetablePhaseRand"],
            summary: "Choose a fixed or seeded random wavetable start phase.",
            description: "Zero starts each voice at phase zero; any nonzero value enables a seeded random phase rather than a proportional amount of randomness. It defaults to off for one voice and on for multi-voice unison. This changes the starting phase, not the selected wavetable frame. Only `wt_*` wavetable sounds consume this control; ordinary samples, `gm_*` soundfonts, and other synths ignore it.",
            params: &[
                ReferenceParam {
                    name: "amount",
                    r#type: "number | Pattern",
                    description: "0 for phase zero; any nonzero value for a seeded random phase",
                },
            ],
            examples: &[
                "s(\"basique\").bank(\"wt_digital\").seg(16).wtphaserand(\"<0 1>\")",
            ],
            tags: &["wavetable", "audio"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["warpenv"],
        aliases: &[],
        reference: ReferenceEntry {
            name: "warpenv",
            synonyms: &[],
            summary: "Wavetable warp envelope amount, in normalized units.",
            description: "Adds a linear ADSR envelope to warp. The amount defaults to 0, or 0.5 when any warpattack, warpdecay, warpsustain, or warprelease is set; an explicit 0 disables it. With no ADSR fields, attack/decay/sustain/release are 0 s, 0.5 s, 0, and 0.1 s. Once any field is set, omitted attack/decay become 0.001 s and release 0.01 s; omitted sustain is 0.001 when decay is set, otherwise 1. The final warp is clamped to 0..1. Choose a non-none warpmode to hear the warp. Only `wt_*` wavetable sounds consume this control; ordinary samples, `gm_*` soundfonts, and other synths ignore it.",
            params: &[
                ReferenceParam {
                    name: "amount",
                    r#type: "number | Pattern",
                    description: "between 0 and 1",
                },
            ],
            examples: &[],
            tags: &["wavetable", "envelope", "audio"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["warpsync"],
        aliases: &[],
        reference: ReferenceEntry {
            name: "warpsync",
            synonyms: &[],
            summary: "Wavetable warp LFO oscillations per cycle.",
            description: "Multiplies the current cycles per second to obtain hertz and overrides warprate. It is absent by default; without it the rate is warprate, default 1 Hz. Naming warprate, warpsync, warpshape, or warpskew supplies depth 0.5 unless warpdepth is explicit; otherwise depth defaults to 0. An explicit zero depth disables the LFO. Choose a non-none warpmode to hear the warp. Only `wt_*` wavetable sounds consume this control; ordinary samples, `gm_*` soundfonts, and other synths ignore it.",
            params: &[
                ReferenceParam {
                    name: "rate",
                    r#type: "number | Pattern",
                    description: "oscillations per cycle",
                },
            ],
            examples: &[],
            tags: &["wavetable", "lfo", "audio"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["source"],
        aliases: &["src"],
        reference: ReferenceEntry {
            name: "source",
            synonyms: &["src"],
            summary: "Store a source value; unused by native audio.",
            description: "Stores a `source` value on each event for compatibility. The native renderer does not use this control. Use `s` to select a built-in sound or sample.",
            params: &[
                ReferenceParam {
                    name: "getSource",
                    r#type: "function",
                    description: "",
                },
            ],
            examples: &[],
            tags: &["external_io", "audio"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["n"],
        aliases: &[],
        reference: ReferenceEntry {
            name: "n",
            synonyms: &[],
            summary: "Select a bank index, scale degree, or source-specific variant.",
            description: "For sample and wavetable banks, n selects an index with wraparound; the index defaults to 0, while note or freq controls pitch. scale and voicing interpret n as a scale degree or voice index. On sawtooth, square, triangle, and user oscillators, n is the partial-count fallback when partials is absent; sine ignores it. Supersaw uses n as its detune fallback. Bytebeat uses n to choose a built-in expression unless bbexpr is set. On s(\"in\") and s(\"bus\"), n selects the input channel or bus, starting at 0. Other dedicated synths do not use n as a pitch control.",
            params: &[
                ReferenceParam {
                    name: "value",
                    r#type: "number | Pattern",
                    description: "sample index starting from 0",
                },
            ],
            examples: &[
                "s(\"bd sd [~ bd] sd,hh*6\").n(\"<0 1>\")",
            ],
            tags: &["audio", "samples", "tonal"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["i"],
        aliases: &[],
        reference: ReferenceEntry {
            name: "i",
            synonyms: &[],
            summary: "Selects the given degree.",
            description: "Selects the given degree. Currently used in `xen` and `tune`:",
            params: &[
                ReferenceParam {
                    name: "value",
                    r#type: "number | Pattern",
                    description: "",
                },
            ],
            examples: &[
                "i(\"0 1 2 3 4 5 6 7\").xen(\"<5edo 10edo 15edo hexany15>\")",
            ],
            tags: &["tonal"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["note", "n"],
        aliases: &[],
        reference: ReferenceEntry {
            name: "note",
            synonyms: &[],
            summary: "Plays the given note name or midi number.",
            description: "Plays the given note name or midi number. A note name consists of\n\n- a letter (a-g or A-G)\n- optional accidentals (b or #)\n- optional (possibly negative) octave number (0-9). Defaults to 3\n\nExamples of valid note names: `c`, `bb`, `Bb`, `f#`, `c3`, `A4`, `Eb2`, `c#5`\n\nYou can also use midi numbers instead of note names, where 69 is mapped to A4 440Hz in 12EDO.",
            params: &[],
            examples: &[
                "note(\"c a f e\")",
                "note(\"c4 a4 f4 e4\")",
                "note(\"60 69 65 64\")",
                "note(\"fbb1 a#0 cbbb-1 e##-2\").sound(\"saw\")",
            ],
            tags: &["tonal"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["accelerate"],
        aliases: &[],
        reference: ReferenceEntry {
            name: "accelerate",
            synonyms: &[],
            summary: "SuperDirt (OSC): sample acceleration.",
            description: "SuperDirt via `.osc()`: changes sample playback speed over each sound. The final speed is `speed * (1 + accelerate)`.",
            params: &[
                ReferenceParam {
                    name: "amount",
                    r#type: "number | Pattern",
                    description: "fractional speed change",
                },
            ],
            examples: &[
                "s(\"sax\").accelerate(\"<0 1 2 4 8 16>\").slow(2).osc()",
            ],
            tags: &["superdirt", "osc", "samples"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["velocity"],
        aliases: &["vel"],
        reference: ReferenceEntry {
            name: "velocity",
            synonyms: &["vel"],
            summary: "Sets the velocity from 0 to 1.",
            description: "Sets the velocity from 0 to 1. Is multiplied together with gain.\n\nDefaults to 1. This is a linear multiplier for any native voice; 0..1 is the usual range. Its scale is independent of the selected source's own normalization.",
            params: &[],
            examples: &[
                "s(\"hh*8\")\n.gain(\".4!2 1 .4!2 1 .4 1\")\n.velocity(\".4 1\")",
            ],
            tags: &["amplitude", "audio"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["gain"],
        aliases: &[],
        reference: ReferenceEntry {
            name: "gain",
            synonyms: &[],
            summary: "Linear gain multiplier for the native voice.",
            description: "Defaults to 0.8. In the native renderer this is a linear amplitude multiplier: 0 silences the voice and 0.5 halves its level. velocity is multiplied with it and defaults to 1. Source families have their own normalization, so equal gain values need not produce equal loudness across sounds. Applies to synths, wavetables, samples including `gm_*` zones, and live input.",
            params: &[
                ReferenceParam {
                    name: "amount",
                    r#type: "number | Pattern",
                    description: "gain.",
                },
            ],
            examples: &[
                "s(\"hh*8\").gain(\".4!2 1 .4!2 1 .4 1\").fast(2)",
            ],
            tags: &["amplitude", "audio"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["postgain"],
        aliases: &[],
        reference: ReferenceEntry {
            name: "postgain",
            synonyms: &[],
            summary: "Output gain before per-voice limiting and sends.",
            description: "A linear output multiplier, default 1. It scales the processed voice before limit and the delay, reverb, and audio-bus sends. Applies to every native source.",
            params: &[],
            examples: &[
                "s(\"bd sd [~ bd] sd,hh*8\")\n.compressor(\"-20:20:10:.002:.02\").postgain(1.5)",
            ],
            tags: &["amplitude", "audio"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["amp"],
        aliases: &[],
        reference: ReferenceEntry {
            name: "amp",
            synonyms: &[],
            summary: "SuperDirt (OSC): linear amplitude multiplier.",
            description: "SuperDirt via `.osc()`: multiplies the sound amplitude by a linear factor.",
            params: &[
                ReferenceParam {
                    name: "amount",
                    r#type: "number | Pattern",
                    description: "amplitude multiplier",
                },
            ],
            examples: &[
                "s(\"bd*8\").amp(\".1*2 .5 .1*2 .5 .1 .5\").osc()",
            ],
            tags: &["superdirt", "osc", "amplitude"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["fmi00"],
        aliases: &["fm00"],
        reference:         ReferenceEntry {
            name: "fmi00",
            synonyms: &["fm00"],
            summary: "a matrix slot with source 0 - pinned surface, read nowhere",
            description: "The FM resolver walks sources 1..8: there is no operator 0 to send from, so the fmi0X spellings are accepted but never read. The value rides the event and leaves the engine through .osc(), and a midimaps entry can turn it into a CC; natively it is silent.",
            params: &[
                ReferenceParam {
                    name: "value",
                    r#type: "number | Pattern",
                    description: "modulation depth; never applied",
                },
            ],
            examples: &[
                "s(\"sine\").fmi00(1)",
            ],
            tags: &["control", "fm"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["fmi01"],
        aliases: &["fm01"],
        reference:         ReferenceEntry {
            name: "fmi01",
            synonyms: &["fm01"],
            summary: "a matrix slot with source 0 - pinned surface, read nowhere",
            description: "The FM resolver walks sources 1..8: there is no operator 0 to send from, so the fmi0X spellings are accepted but never read. The value rides the event and leaves the engine through .osc(), and a midimaps entry can turn it into a CC; natively it is silent.",
            params: &[
                ReferenceParam {
                    name: "value",
                    r#type: "number | Pattern",
                    description: "modulation depth; never applied",
                },
            ],
            examples: &[
                "s(\"sine\").fmi01(1)",
            ],
            tags: &["control", "fm"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["fmi02"],
        aliases: &["fm02"],
        reference:         ReferenceEntry {
            name: "fmi02",
            synonyms: &["fm02"],
            summary: "a matrix slot with source 0 - pinned surface, read nowhere",
            description: "The FM resolver walks sources 1..8: there is no operator 0 to send from, so the fmi0X spellings are accepted but never read. The value rides the event and leaves the engine through .osc(), and a midimaps entry can turn it into a CC; natively it is silent.",
            params: &[
                ReferenceParam {
                    name: "value",
                    r#type: "number | Pattern",
                    description: "modulation depth; never applied",
                },
            ],
            examples: &[
                "s(\"sine\").fmi02(1)",
            ],
            tags: &["control", "fm"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["fmi03"],
        aliases: &["fm03"],
        reference:         ReferenceEntry {
            name: "fmi03",
            synonyms: &["fm03"],
            summary: "a matrix slot with source 0 - pinned surface, read nowhere",
            description: "The FM resolver walks sources 1..8: there is no operator 0 to send from, so the fmi0X spellings are accepted but never read. The value rides the event and leaves the engine through .osc(), and a midimaps entry can turn it into a CC; natively it is silent.",
            params: &[
                ReferenceParam {
                    name: "value",
                    r#type: "number | Pattern",
                    description: "modulation depth; never applied",
                },
            ],
            examples: &[
                "s(\"sine\").fmi03(1)",
            ],
            tags: &["control", "fm"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["fmi04"],
        aliases: &["fm04"],
        reference:         ReferenceEntry {
            name: "fmi04",
            synonyms: &["fm04"],
            summary: "a matrix slot with source 0 - pinned surface, read nowhere",
            description: "The FM resolver walks sources 1..8: there is no operator 0 to send from, so the fmi0X spellings are accepted but never read. The value rides the event and leaves the engine through .osc(), and a midimaps entry can turn it into a CC; natively it is silent.",
            params: &[
                ReferenceParam {
                    name: "value",
                    r#type: "number | Pattern",
                    description: "modulation depth; never applied",
                },
            ],
            examples: &[
                "s(\"sine\").fmi04(1)",
            ],
            tags: &["control", "fm"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["fmi05"],
        aliases: &["fm05"],
        reference:         ReferenceEntry {
            name: "fmi05",
            synonyms: &["fm05"],
            summary: "a matrix slot with source 0 - pinned surface, read nowhere",
            description: "The FM resolver walks sources 1..8: there is no operator 0 to send from, so the fmi0X spellings are accepted but never read. The value rides the event and leaves the engine through .osc(), and a midimaps entry can turn it into a CC; natively it is silent.",
            params: &[
                ReferenceParam {
                    name: "value",
                    r#type: "number | Pattern",
                    description: "modulation depth; never applied",
                },
            ],
            examples: &[
                "s(\"sine\").fmi05(1)",
            ],
            tags: &["control", "fm"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["fmi06"],
        aliases: &["fm06"],
        reference:         ReferenceEntry {
            name: "fmi06",
            synonyms: &["fm06"],
            summary: "a matrix slot with source 0 - pinned surface, read nowhere",
            description: "The FM resolver walks sources 1..8: there is no operator 0 to send from, so the fmi0X spellings are accepted but never read. The value rides the event and leaves the engine through .osc(), and a midimaps entry can turn it into a CC; natively it is silent.",
            params: &[
                ReferenceParam {
                    name: "value",
                    r#type: "number | Pattern",
                    description: "modulation depth; never applied",
                },
            ],
            examples: &[
                "s(\"sine\").fmi06(1)",
            ],
            tags: &["control", "fm"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["fmi07"],
        aliases: &["fm07"],
        reference:         ReferenceEntry {
            name: "fmi07",
            synonyms: &["fm07"],
            summary: "a matrix slot with source 0 - pinned surface, read nowhere",
            description: "The FM resolver walks sources 1..8: there is no operator 0 to send from, so the fmi0X spellings are accepted but never read. The value rides the event and leaves the engine through .osc(), and a midimaps entry can turn it into a CC; natively it is silent.",
            params: &[
                ReferenceParam {
                    name: "value",
                    r#type: "number | Pattern",
                    description: "modulation depth; never applied",
                },
            ],
            examples: &[
                "s(\"sine\").fmi07(1)",
            ],
            tags: &["control", "fm"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["fmi08"],
        aliases: &["fm08"],
        reference:         ReferenceEntry {
            name: "fmi08",
            synonyms: &["fm08"],
            summary: "a matrix slot with source 0 - pinned surface, read nowhere",
            description: "The FM resolver walks sources 1..8: there is no operator 0 to send from, so the fmi0X spellings are accepted but never read. The value rides the event and leaves the engine through .osc(), and a midimaps entry can turn it into a CC; natively it is silent.",
            params: &[
                ReferenceParam {
                    name: "value",
                    r#type: "number | Pattern",
                    description: "modulation depth; never applied",
                },
            ],
            examples: &[
                "s(\"sine\").fmi08(1)",
            ],
            tags: &["control", "fm"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["fmi10"],
        aliases: &["fm10"],
        reference:         ReferenceEntry {
            name: "fmi10",
            synonyms: &["fm10"],
            summary: "operator 1 into the carrier - spelled fmi here",
            description: "This cell is the diagonal the resolver reads under the short spelling fmi: operator 1 is unsuffixed throughout, so the fmi10 form is pinned for surface compatibility but never read. The behaviour is the fmi entry's. The value is a modulation depth - an index, not hertz: the deviation it produces scales with the carrier and the source's harmonicity. A zero or absent amount makes no connection at all, and a gap severs the chain rather than being stepped over. Sixteen routes per event is the ceiling; beyond it the event is refused. The matrix bends the oscillator voices - sine, triangle, square, sawtooth, supersaw, pulse, bytebeat; samples, wavetables and the zzfx family ignore it.",
            params: &[
                ReferenceParam {
                    name: "value",
                    r#type: "number | Pattern",
                    description: "modulation index; 0 or absent = no route",
                },
            ],
            examples: &[
                "note(\"c2 e2\").s(\"sine\").fmi(\"<0 2 4 8>\")",
            ],
            tags: &["control", "fm"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["fmi11"],
        aliases: &["fm11"],
        reference:         ReferenceEntry {
            name: "fmi11",
            synonyms: &["fm11"],
            summary: "sends operator 1 into operator 1 - itself, as feedback",
            description: "One cell of the FM matrix: the modulation operator 1 bends operator 1 - itself, as feedback. An operator is built the first time any route names it at either end. The value is a modulation depth - an index, not hertz: the deviation it produces scales with the carrier and the source's harmonicity. A zero or absent amount makes no connection at all, and a gap severs the chain rather than being stepped over. Sixteen routes per event is the ceiling; beyond it the event is refused. The matrix bends the oscillator voices - sine, triangle, square, sawtooth, supersaw, pulse, bytebeat; samples, wavetables and the zzfx family ignore it.",
            params: &[
                ReferenceParam {
                    name: "value",
                    r#type: "number | Pattern",
                    description: "modulation index; 0 or absent = no route",
                },
            ],
            examples: &[
                "note(\"c2 e2\").s(\"sine\").fmi11(\"<0 1 4>\")",
            ],
            tags: &["control", "fm"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["fmi12"],
        aliases: &["fm12"],
        reference:         ReferenceEntry {
            name: "fmi12",
            synonyms: &["fm12"],
            summary: "sends operator 1 into operator 2",
            description: "One cell of the FM matrix: the modulation operator 1 bends operator 2. An operator is built the first time any route names it at either end. The value is a modulation depth - an index, not hertz: the deviation it produces scales with the carrier and the source's harmonicity. A zero or absent amount makes no connection at all, and a gap severs the chain rather than being stepped over. Sixteen routes per event is the ceiling; beyond it the event is refused. The matrix bends the oscillator voices - sine, triangle, square, sawtooth, supersaw, pulse, bytebeat; samples, wavetables and the zzfx family ignore it.",
            params: &[
                ReferenceParam {
                    name: "value",
                    r#type: "number | Pattern",
                    description: "modulation index; 0 or absent = no route",
                },
            ],
            examples: &[
                "note(\"c2 e2\").s(\"sine\").fmi12(\"<0 1 4>\")",
            ],
            tags: &["control", "fm"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["fmi13"],
        aliases: &["fm13"],
        reference:         ReferenceEntry {
            name: "fmi13",
            synonyms: &["fm13"],
            summary: "sends operator 1 into operator 3",
            description: "One cell of the FM matrix: the modulation operator 1 bends operator 3. An operator is built the first time any route names it at either end. The value is a modulation depth - an index, not hertz: the deviation it produces scales with the carrier and the source's harmonicity. A zero or absent amount makes no connection at all, and a gap severs the chain rather than being stepped over. Sixteen routes per event is the ceiling; beyond it the event is refused. The matrix bends the oscillator voices - sine, triangle, square, sawtooth, supersaw, pulse, bytebeat; samples, wavetables and the zzfx family ignore it.",
            params: &[
                ReferenceParam {
                    name: "value",
                    r#type: "number | Pattern",
                    description: "modulation index; 0 or absent = no route",
                },
            ],
            examples: &[
                "note(\"c2 e2\").s(\"sine\").fmi13(\"<0 1 4>\")",
            ],
            tags: &["control", "fm"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["fmi14"],
        aliases: &["fm14"],
        reference:         ReferenceEntry {
            name: "fmi14",
            synonyms: &["fm14"],
            summary: "sends operator 1 into operator 4",
            description: "One cell of the FM matrix: the modulation operator 1 bends operator 4. An operator is built the first time any route names it at either end. The value is a modulation depth - an index, not hertz: the deviation it produces scales with the carrier and the source's harmonicity. A zero or absent amount makes no connection at all, and a gap severs the chain rather than being stepped over. Sixteen routes per event is the ceiling; beyond it the event is refused. The matrix bends the oscillator voices - sine, triangle, square, sawtooth, supersaw, pulse, bytebeat; samples, wavetables and the zzfx family ignore it.",
            params: &[
                ReferenceParam {
                    name: "value",
                    r#type: "number | Pattern",
                    description: "modulation index; 0 or absent = no route",
                },
            ],
            examples: &[
                "note(\"c2 e2\").s(\"sine\").fmi14(\"<0 1 4>\")",
            ],
            tags: &["control", "fm"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["fmi15"],
        aliases: &["fm15"],
        reference:         ReferenceEntry {
            name: "fmi15",
            synonyms: &["fm15"],
            summary: "sends operator 1 into operator 5",
            description: "One cell of the FM matrix: the modulation operator 1 bends operator 5. An operator is built the first time any route names it at either end. The value is a modulation depth - an index, not hertz: the deviation it produces scales with the carrier and the source's harmonicity. A zero or absent amount makes no connection at all, and a gap severs the chain rather than being stepped over. Sixteen routes per event is the ceiling; beyond it the event is refused. The matrix bends the oscillator voices - sine, triangle, square, sawtooth, supersaw, pulse, bytebeat; samples, wavetables and the zzfx family ignore it.",
            params: &[
                ReferenceParam {
                    name: "value",
                    r#type: "number | Pattern",
                    description: "modulation index; 0 or absent = no route",
                },
            ],
            examples: &[
                "note(\"c2 e2\").s(\"sine\").fmi15(\"<0 1 4>\")",
            ],
            tags: &["control", "fm"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["fmi16"],
        aliases: &["fm16"],
        reference:         ReferenceEntry {
            name: "fmi16",
            synonyms: &["fm16"],
            summary: "sends operator 1 into operator 6",
            description: "One cell of the FM matrix: the modulation operator 1 bends operator 6. An operator is built the first time any route names it at either end. The value is a modulation depth - an index, not hertz: the deviation it produces scales with the carrier and the source's harmonicity. A zero or absent amount makes no connection at all, and a gap severs the chain rather than being stepped over. Sixteen routes per event is the ceiling; beyond it the event is refused. The matrix bends the oscillator voices - sine, triangle, square, sawtooth, supersaw, pulse, bytebeat; samples, wavetables and the zzfx family ignore it.",
            params: &[
                ReferenceParam {
                    name: "value",
                    r#type: "number | Pattern",
                    description: "modulation index; 0 or absent = no route",
                },
            ],
            examples: &[
                "note(\"c2 e2\").s(\"sine\").fmi16(\"<0 1 4>\")",
            ],
            tags: &["control", "fm"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["fmi17"],
        aliases: &["fm17"],
        reference:         ReferenceEntry {
            name: "fmi17",
            synonyms: &["fm17"],
            summary: "sends operator 1 into operator 7",
            description: "One cell of the FM matrix: the modulation operator 1 bends operator 7. An operator is built the first time any route names it at either end. The value is a modulation depth - an index, not hertz: the deviation it produces scales with the carrier and the source's harmonicity. A zero or absent amount makes no connection at all, and a gap severs the chain rather than being stepped over. Sixteen routes per event is the ceiling; beyond it the event is refused. The matrix bends the oscillator voices - sine, triangle, square, sawtooth, supersaw, pulse, bytebeat; samples, wavetables and the zzfx family ignore it.",
            params: &[
                ReferenceParam {
                    name: "value",
                    r#type: "number | Pattern",
                    description: "modulation index; 0 or absent = no route",
                },
            ],
            examples: &[
                "note(\"c2 e2\").s(\"sine\").fmi17(\"<0 1 4>\")",
            ],
            tags: &["control", "fm"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["fmi18"],
        aliases: &["fm18"],
        reference:         ReferenceEntry {
            name: "fmi18",
            synonyms: &["fm18"],
            summary: "sends operator 1 into operator 8",
            description: "One cell of the FM matrix: the modulation operator 1 bends operator 8. An operator is built the first time any route names it at either end. The value is a modulation depth - an index, not hertz: the deviation it produces scales with the carrier and the source's harmonicity. A zero or absent amount makes no connection at all, and a gap severs the chain rather than being stepped over. Sixteen routes per event is the ceiling; beyond it the event is refused. The matrix bends the oscillator voices - sine, triangle, square, sawtooth, supersaw, pulse, bytebeat; samples, wavetables and the zzfx family ignore it.",
            params: &[
                ReferenceParam {
                    name: "value",
                    r#type: "number | Pattern",
                    description: "modulation index; 0 or absent = no route",
                },
            ],
            examples: &[
                "note(\"c2 e2\").s(\"sine\").fmi18(\"<0 1 4>\")",
            ],
            tags: &["control", "fm"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["fmi20"],
        aliases: &["fm20"],
        reference:         ReferenceEntry {
            name: "fmi20",
            synonyms: &["fm20"],
            summary: "sends operator 2 into the carrier's frequency",
            description: "One cell of the FM matrix: the modulation operator 2 bends the carrier's frequency. An operator is built the first time any route names it at either end. The value is a modulation depth - an index, not hertz: the deviation it produces scales with the carrier and the source's harmonicity. A zero or absent amount makes no connection at all, and a gap severs the chain rather than being stepped over. Sixteen routes per event is the ceiling; beyond it the event is refused. The matrix bends the oscillator voices - sine, triangle, square, sawtooth, supersaw, pulse, bytebeat; samples, wavetables and the zzfx family ignore it.",
            params: &[
                ReferenceParam {
                    name: "value",
                    r#type: "number | Pattern",
                    description: "modulation index; 0 or absent = no route",
                },
            ],
            examples: &[
                "note(\"c2 e2\").s(\"sine\").fmi20(\"<0 1 4>\")",
            ],
            tags: &["control", "fm"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["fmi21"],
        aliases: &["fm21"],
        reference:         ReferenceEntry {
            name: "fmi21",
            synonyms: &["fm21"],
            summary: "operator 2 into operator 1 - spelled fmi2 here",
            description: "This cell is the diagonal the resolver reads under the short spelling fmi2: operator 1 is unsuffixed throughout, so the fmi21 form is pinned for surface compatibility but never read. The behaviour is the fmi2 entry's. The value is a modulation depth - an index, not hertz: the deviation it produces scales with the carrier and the source's harmonicity. A zero or absent amount makes no connection at all, and a gap severs the chain rather than being stepped over. Sixteen routes per event is the ceiling; beyond it the event is refused. The matrix bends the oscillator voices - sine, triangle, square, sawtooth, supersaw, pulse, bytebeat; samples, wavetables and the zzfx family ignore it.",
            params: &[
                ReferenceParam {
                    name: "value",
                    r#type: "number | Pattern",
                    description: "modulation index; 0 or absent = no route",
                },
            ],
            examples: &[
                "note(\"c2 e2\").s(\"sine\").fmi2(\"<0 2 4 8>\")",
            ],
            tags: &["control", "fm"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["fmi22"],
        aliases: &["fm22"],
        reference:         ReferenceEntry {
            name: "fmi22",
            synonyms: &["fm22"],
            summary: "sends operator 2 into operator 2 - itself, as feedback",
            description: "One cell of the FM matrix: the modulation operator 2 bends operator 2 - itself, as feedback. An operator is built the first time any route names it at either end. The value is a modulation depth - an index, not hertz: the deviation it produces scales with the carrier and the source's harmonicity. A zero or absent amount makes no connection at all, and a gap severs the chain rather than being stepped over. Sixteen routes per event is the ceiling; beyond it the event is refused. The matrix bends the oscillator voices - sine, triangle, square, sawtooth, supersaw, pulse, bytebeat; samples, wavetables and the zzfx family ignore it.",
            params: &[
                ReferenceParam {
                    name: "value",
                    r#type: "number | Pattern",
                    description: "modulation index; 0 or absent = no route",
                },
            ],
            examples: &[
                "note(\"c2 e2\").s(\"sine\").fmi22(\"<0 1 4>\")",
            ],
            tags: &["control", "fm"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["fmi23"],
        aliases: &["fm23"],
        reference:         ReferenceEntry {
            name: "fmi23",
            synonyms: &["fm23"],
            summary: "sends operator 2 into operator 3",
            description: "One cell of the FM matrix: the modulation operator 2 bends operator 3. An operator is built the first time any route names it at either end. The value is a modulation depth - an index, not hertz: the deviation it produces scales with the carrier and the source's harmonicity. A zero or absent amount makes no connection at all, and a gap severs the chain rather than being stepped over. Sixteen routes per event is the ceiling; beyond it the event is refused. The matrix bends the oscillator voices - sine, triangle, square, sawtooth, supersaw, pulse, bytebeat; samples, wavetables and the zzfx family ignore it.",
            params: &[
                ReferenceParam {
                    name: "value",
                    r#type: "number | Pattern",
                    description: "modulation index; 0 or absent = no route",
                },
            ],
            examples: &[
                "note(\"c2 e2\").s(\"sine\").fmi23(\"<0 1 4>\")",
            ],
            tags: &["control", "fm"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["fmi24"],
        aliases: &["fm24"],
        reference:         ReferenceEntry {
            name: "fmi24",
            synonyms: &["fm24"],
            summary: "sends operator 2 into operator 4",
            description: "One cell of the FM matrix: the modulation operator 2 bends operator 4. An operator is built the first time any route names it at either end. The value is a modulation depth - an index, not hertz: the deviation it produces scales with the carrier and the source's harmonicity. A zero or absent amount makes no connection at all, and a gap severs the chain rather than being stepped over. Sixteen routes per event is the ceiling; beyond it the event is refused. The matrix bends the oscillator voices - sine, triangle, square, sawtooth, supersaw, pulse, bytebeat; samples, wavetables and the zzfx family ignore it.",
            params: &[
                ReferenceParam {
                    name: "value",
                    r#type: "number | Pattern",
                    description: "modulation index; 0 or absent = no route",
                },
            ],
            examples: &[
                "note(\"c2 e2\").s(\"sine\").fmi24(\"<0 1 4>\")",
            ],
            tags: &["control", "fm"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["fmi25"],
        aliases: &["fm25"],
        reference:         ReferenceEntry {
            name: "fmi25",
            synonyms: &["fm25"],
            summary: "sends operator 2 into operator 5",
            description: "One cell of the FM matrix: the modulation operator 2 bends operator 5. An operator is built the first time any route names it at either end. The value is a modulation depth - an index, not hertz: the deviation it produces scales with the carrier and the source's harmonicity. A zero or absent amount makes no connection at all, and a gap severs the chain rather than being stepped over. Sixteen routes per event is the ceiling; beyond it the event is refused. The matrix bends the oscillator voices - sine, triangle, square, sawtooth, supersaw, pulse, bytebeat; samples, wavetables and the zzfx family ignore it.",
            params: &[
                ReferenceParam {
                    name: "value",
                    r#type: "number | Pattern",
                    description: "modulation index; 0 or absent = no route",
                },
            ],
            examples: &[
                "note(\"c2 e2\").s(\"sine\").fmi25(\"<0 1 4>\")",
            ],
            tags: &["control", "fm"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["fmi26"],
        aliases: &["fm26"],
        reference:         ReferenceEntry {
            name: "fmi26",
            synonyms: &["fm26"],
            summary: "sends operator 2 into operator 6",
            description: "One cell of the FM matrix: the modulation operator 2 bends operator 6. An operator is built the first time any route names it at either end. The value is a modulation depth - an index, not hertz: the deviation it produces scales with the carrier and the source's harmonicity. A zero or absent amount makes no connection at all, and a gap severs the chain rather than being stepped over. Sixteen routes per event is the ceiling; beyond it the event is refused. The matrix bends the oscillator voices - sine, triangle, square, sawtooth, supersaw, pulse, bytebeat; samples, wavetables and the zzfx family ignore it.",
            params: &[
                ReferenceParam {
                    name: "value",
                    r#type: "number | Pattern",
                    description: "modulation index; 0 or absent = no route",
                },
            ],
            examples: &[
                "note(\"c2 e2\").s(\"sine\").fmi26(\"<0 1 4>\")",
            ],
            tags: &["control", "fm"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["fmi27"],
        aliases: &["fm27"],
        reference:         ReferenceEntry {
            name: "fmi27",
            synonyms: &["fm27"],
            summary: "sends operator 2 into operator 7",
            description: "One cell of the FM matrix: the modulation operator 2 bends operator 7. An operator is built the first time any route names it at either end. The value is a modulation depth - an index, not hertz: the deviation it produces scales with the carrier and the source's harmonicity. A zero or absent amount makes no connection at all, and a gap severs the chain rather than being stepped over. Sixteen routes per event is the ceiling; beyond it the event is refused. The matrix bends the oscillator voices - sine, triangle, square, sawtooth, supersaw, pulse, bytebeat; samples, wavetables and the zzfx family ignore it.",
            params: &[
                ReferenceParam {
                    name: "value",
                    r#type: "number | Pattern",
                    description: "modulation index; 0 or absent = no route",
                },
            ],
            examples: &[
                "note(\"c2 e2\").s(\"sine\").fmi27(\"<0 1 4>\")",
            ],
            tags: &["control", "fm"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["fmi28"],
        aliases: &["fm28"],
        reference:         ReferenceEntry {
            name: "fmi28",
            synonyms: &["fm28"],
            summary: "sends operator 2 into operator 8",
            description: "One cell of the FM matrix: the modulation operator 2 bends operator 8. An operator is built the first time any route names it at either end. The value is a modulation depth - an index, not hertz: the deviation it produces scales with the carrier and the source's harmonicity. A zero or absent amount makes no connection at all, and a gap severs the chain rather than being stepped over. Sixteen routes per event is the ceiling; beyond it the event is refused. The matrix bends the oscillator voices - sine, triangle, square, sawtooth, supersaw, pulse, bytebeat; samples, wavetables and the zzfx family ignore it.",
            params: &[
                ReferenceParam {
                    name: "value",
                    r#type: "number | Pattern",
                    description: "modulation index; 0 or absent = no route",
                },
            ],
            examples: &[
                "note(\"c2 e2\").s(\"sine\").fmi28(\"<0 1 4>\")",
            ],
            tags: &["control", "fm"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["fmi30"],
        aliases: &["fm30"],
        reference:         ReferenceEntry {
            name: "fmi30",
            synonyms: &["fm30"],
            summary: "sends operator 3 into the carrier's frequency",
            description: "One cell of the FM matrix: the modulation operator 3 bends the carrier's frequency. An operator is built the first time any route names it at either end. The value is a modulation depth - an index, not hertz: the deviation it produces scales with the carrier and the source's harmonicity. A zero or absent amount makes no connection at all, and a gap severs the chain rather than being stepped over. Sixteen routes per event is the ceiling; beyond it the event is refused. The matrix bends the oscillator voices - sine, triangle, square, sawtooth, supersaw, pulse, bytebeat; samples, wavetables and the zzfx family ignore it.",
            params: &[
                ReferenceParam {
                    name: "value",
                    r#type: "number | Pattern",
                    description: "modulation index; 0 or absent = no route",
                },
            ],
            examples: &[
                "note(\"c2 e2\").s(\"sine\").fmi30(\"<0 1 4>\")",
            ],
            tags: &["control", "fm"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["fmi31"],
        aliases: &["fm31"],
        reference:         ReferenceEntry {
            name: "fmi31",
            synonyms: &["fm31"],
            summary: "sends operator 3 into operator 1",
            description: "One cell of the FM matrix: the modulation operator 3 bends operator 1. An operator is built the first time any route names it at either end. The value is a modulation depth - an index, not hertz: the deviation it produces scales with the carrier and the source's harmonicity. A zero or absent amount makes no connection at all, and a gap severs the chain rather than being stepped over. Sixteen routes per event is the ceiling; beyond it the event is refused. The matrix bends the oscillator voices - sine, triangle, square, sawtooth, supersaw, pulse, bytebeat; samples, wavetables and the zzfx family ignore it.",
            params: &[
                ReferenceParam {
                    name: "value",
                    r#type: "number | Pattern",
                    description: "modulation index; 0 or absent = no route",
                },
            ],
            examples: &[
                "note(\"c2 e2\").s(\"sine\").fmi31(\"<0 1 4>\")",
            ],
            tags: &["control", "fm"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["fmi32"],
        aliases: &["fm32"],
        reference:         ReferenceEntry {
            name: "fmi32",
            synonyms: &["fm32"],
            summary: "operator 3 into operator 2 - spelled fmi3 here",
            description: "This cell is the diagonal the resolver reads under the short spelling fmi3: operator 1 is unsuffixed throughout, so the fmi32 form is pinned for surface compatibility but never read. The behaviour is the fmi3 entry's. The value is a modulation depth - an index, not hertz: the deviation it produces scales with the carrier and the source's harmonicity. A zero or absent amount makes no connection at all, and a gap severs the chain rather than being stepped over. Sixteen routes per event is the ceiling; beyond it the event is refused. The matrix bends the oscillator voices - sine, triangle, square, sawtooth, supersaw, pulse, bytebeat; samples, wavetables and the zzfx family ignore it.",
            params: &[
                ReferenceParam {
                    name: "value",
                    r#type: "number | Pattern",
                    description: "modulation index; 0 or absent = no route",
                },
            ],
            examples: &[
                "note(\"c2 e2\").s(\"sine\").fmi3(\"<0 2 4 8>\")",
            ],
            tags: &["control", "fm"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["fmi33"],
        aliases: &["fm33"],
        reference:         ReferenceEntry {
            name: "fmi33",
            synonyms: &["fm33"],
            summary: "sends operator 3 into operator 3 - itself, as feedback",
            description: "One cell of the FM matrix: the modulation operator 3 bends operator 3 - itself, as feedback. An operator is built the first time any route names it at either end. The value is a modulation depth - an index, not hertz: the deviation it produces scales with the carrier and the source's harmonicity. A zero or absent amount makes no connection at all, and a gap severs the chain rather than being stepped over. Sixteen routes per event is the ceiling; beyond it the event is refused. The matrix bends the oscillator voices - sine, triangle, square, sawtooth, supersaw, pulse, bytebeat; samples, wavetables and the zzfx family ignore it.",
            params: &[
                ReferenceParam {
                    name: "value",
                    r#type: "number | Pattern",
                    description: "modulation index; 0 or absent = no route",
                },
            ],
            examples: &[
                "note(\"c2 e2\").s(\"sine\").fmi33(\"<0 1 4>\")",
            ],
            tags: &["control", "fm"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["fmi34"],
        aliases: &["fm34"],
        reference:         ReferenceEntry {
            name: "fmi34",
            synonyms: &["fm34"],
            summary: "sends operator 3 into operator 4",
            description: "One cell of the FM matrix: the modulation operator 3 bends operator 4. An operator is built the first time any route names it at either end. The value is a modulation depth - an index, not hertz: the deviation it produces scales with the carrier and the source's harmonicity. A zero or absent amount makes no connection at all, and a gap severs the chain rather than being stepped over. Sixteen routes per event is the ceiling; beyond it the event is refused. The matrix bends the oscillator voices - sine, triangle, square, sawtooth, supersaw, pulse, bytebeat; samples, wavetables and the zzfx family ignore it.",
            params: &[
                ReferenceParam {
                    name: "value",
                    r#type: "number | Pattern",
                    description: "modulation index; 0 or absent = no route",
                },
            ],
            examples: &[
                "note(\"c2 e2\").s(\"sine\").fmi34(\"<0 1 4>\")",
            ],
            tags: &["control", "fm"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["fmi35"],
        aliases: &["fm35"],
        reference:         ReferenceEntry {
            name: "fmi35",
            synonyms: &["fm35"],
            summary: "sends operator 3 into operator 5",
            description: "One cell of the FM matrix: the modulation operator 3 bends operator 5. An operator is built the first time any route names it at either end. The value is a modulation depth - an index, not hertz: the deviation it produces scales with the carrier and the source's harmonicity. A zero or absent amount makes no connection at all, and a gap severs the chain rather than being stepped over. Sixteen routes per event is the ceiling; beyond it the event is refused. The matrix bends the oscillator voices - sine, triangle, square, sawtooth, supersaw, pulse, bytebeat; samples, wavetables and the zzfx family ignore it.",
            params: &[
                ReferenceParam {
                    name: "value",
                    r#type: "number | Pattern",
                    description: "modulation index; 0 or absent = no route",
                },
            ],
            examples: &[
                "note(\"c2 e2\").s(\"sine\").fmi35(\"<0 1 4>\")",
            ],
            tags: &["control", "fm"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["fmi36"],
        aliases: &["fm36"],
        reference:         ReferenceEntry {
            name: "fmi36",
            synonyms: &["fm36"],
            summary: "sends operator 3 into operator 6",
            description: "One cell of the FM matrix: the modulation operator 3 bends operator 6. An operator is built the first time any route names it at either end. The value is a modulation depth - an index, not hertz: the deviation it produces scales with the carrier and the source's harmonicity. A zero or absent amount makes no connection at all, and a gap severs the chain rather than being stepped over. Sixteen routes per event is the ceiling; beyond it the event is refused. The matrix bends the oscillator voices - sine, triangle, square, sawtooth, supersaw, pulse, bytebeat; samples, wavetables and the zzfx family ignore it.",
            params: &[
                ReferenceParam {
                    name: "value",
                    r#type: "number | Pattern",
                    description: "modulation index; 0 or absent = no route",
                },
            ],
            examples: &[
                "note(\"c2 e2\").s(\"sine\").fmi36(\"<0 1 4>\")",
            ],
            tags: &["control", "fm"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["fmi37"],
        aliases: &["fm37"],
        reference:         ReferenceEntry {
            name: "fmi37",
            synonyms: &["fm37"],
            summary: "sends operator 3 into operator 7",
            description: "One cell of the FM matrix: the modulation operator 3 bends operator 7. An operator is built the first time any route names it at either end. The value is a modulation depth - an index, not hertz: the deviation it produces scales with the carrier and the source's harmonicity. A zero or absent amount makes no connection at all, and a gap severs the chain rather than being stepped over. Sixteen routes per event is the ceiling; beyond it the event is refused. The matrix bends the oscillator voices - sine, triangle, square, sawtooth, supersaw, pulse, bytebeat; samples, wavetables and the zzfx family ignore it.",
            params: &[
                ReferenceParam {
                    name: "value",
                    r#type: "number | Pattern",
                    description: "modulation index; 0 or absent = no route",
                },
            ],
            examples: &[
                "note(\"c2 e2\").s(\"sine\").fmi37(\"<0 1 4>\")",
            ],
            tags: &["control", "fm"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["fmi38"],
        aliases: &["fm38"],
        reference:         ReferenceEntry {
            name: "fmi38",
            synonyms: &["fm38"],
            summary: "sends operator 3 into operator 8",
            description: "One cell of the FM matrix: the modulation operator 3 bends operator 8. An operator is built the first time any route names it at either end. The value is a modulation depth - an index, not hertz: the deviation it produces scales with the carrier and the source's harmonicity. A zero or absent amount makes no connection at all, and a gap severs the chain rather than being stepped over. Sixteen routes per event is the ceiling; beyond it the event is refused. The matrix bends the oscillator voices - sine, triangle, square, sawtooth, supersaw, pulse, bytebeat; samples, wavetables and the zzfx family ignore it.",
            params: &[
                ReferenceParam {
                    name: "value",
                    r#type: "number | Pattern",
                    description: "modulation index; 0 or absent = no route",
                },
            ],
            examples: &[
                "note(\"c2 e2\").s(\"sine\").fmi38(\"<0 1 4>\")",
            ],
            tags: &["control", "fm"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["fmi40"],
        aliases: &["fm40"],
        reference:         ReferenceEntry {
            name: "fmi40",
            synonyms: &["fm40"],
            summary: "sends operator 4 into the carrier's frequency",
            description: "One cell of the FM matrix: the modulation operator 4 bends the carrier's frequency. An operator is built the first time any route names it at either end. The value is a modulation depth - an index, not hertz: the deviation it produces scales with the carrier and the source's harmonicity. A zero or absent amount makes no connection at all, and a gap severs the chain rather than being stepped over. Sixteen routes per event is the ceiling; beyond it the event is refused. The matrix bends the oscillator voices - sine, triangle, square, sawtooth, supersaw, pulse, bytebeat; samples, wavetables and the zzfx family ignore it.",
            params: &[
                ReferenceParam {
                    name: "value",
                    r#type: "number | Pattern",
                    description: "modulation index; 0 or absent = no route",
                },
            ],
            examples: &[
                "note(\"c2 e2\").s(\"sine\").fmi40(\"<0 1 4>\")",
            ],
            tags: &["control", "fm"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["fmi41"],
        aliases: &["fm41"],
        reference:         ReferenceEntry {
            name: "fmi41",
            synonyms: &["fm41"],
            summary: "sends operator 4 into operator 1",
            description: "One cell of the FM matrix: the modulation operator 4 bends operator 1. An operator is built the first time any route names it at either end. The value is a modulation depth - an index, not hertz: the deviation it produces scales with the carrier and the source's harmonicity. A zero or absent amount makes no connection at all, and a gap severs the chain rather than being stepped over. Sixteen routes per event is the ceiling; beyond it the event is refused. The matrix bends the oscillator voices - sine, triangle, square, sawtooth, supersaw, pulse, bytebeat; samples, wavetables and the zzfx family ignore it.",
            params: &[
                ReferenceParam {
                    name: "value",
                    r#type: "number | Pattern",
                    description: "modulation index; 0 or absent = no route",
                },
            ],
            examples: &[
                "note(\"c2 e2\").s(\"sine\").fmi41(\"<0 1 4>\")",
            ],
            tags: &["control", "fm"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["fmi42"],
        aliases: &["fm42"],
        reference:         ReferenceEntry {
            name: "fmi42",
            synonyms: &["fm42"],
            summary: "sends operator 4 into operator 2",
            description: "One cell of the FM matrix: the modulation operator 4 bends operator 2. An operator is built the first time any route names it at either end. The value is a modulation depth - an index, not hertz: the deviation it produces scales with the carrier and the source's harmonicity. A zero or absent amount makes no connection at all, and a gap severs the chain rather than being stepped over. Sixteen routes per event is the ceiling; beyond it the event is refused. The matrix bends the oscillator voices - sine, triangle, square, sawtooth, supersaw, pulse, bytebeat; samples, wavetables and the zzfx family ignore it.",
            params: &[
                ReferenceParam {
                    name: "value",
                    r#type: "number | Pattern",
                    description: "modulation index; 0 or absent = no route",
                },
            ],
            examples: &[
                "note(\"c2 e2\").s(\"sine\").fmi42(\"<0 1 4>\")",
            ],
            tags: &["control", "fm"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["fmi43"],
        aliases: &["fm43"],
        reference:         ReferenceEntry {
            name: "fmi43",
            synonyms: &["fm43"],
            summary: "operator 4 into operator 3 - spelled fmi4 here",
            description: "This cell is the diagonal the resolver reads under the short spelling fmi4: operator 1 is unsuffixed throughout, so the fmi43 form is pinned for surface compatibility but never read. The behaviour is the fmi4 entry's. The value is a modulation depth - an index, not hertz: the deviation it produces scales with the carrier and the source's harmonicity. A zero or absent amount makes no connection at all, and a gap severs the chain rather than being stepped over. Sixteen routes per event is the ceiling; beyond it the event is refused. The matrix bends the oscillator voices - sine, triangle, square, sawtooth, supersaw, pulse, bytebeat; samples, wavetables and the zzfx family ignore it.",
            params: &[
                ReferenceParam {
                    name: "value",
                    r#type: "number | Pattern",
                    description: "modulation index; 0 or absent = no route",
                },
            ],
            examples: &[
                "note(\"c2 e2\").s(\"sine\").fmi4(\"<0 2 4 8>\")",
            ],
            tags: &["control", "fm"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["fmi44"],
        aliases: &["fm44"],
        reference:         ReferenceEntry {
            name: "fmi44",
            synonyms: &["fm44"],
            summary: "sends operator 4 into operator 4 - itself, as feedback",
            description: "One cell of the FM matrix: the modulation operator 4 bends operator 4 - itself, as feedback. An operator is built the first time any route names it at either end. The value is a modulation depth - an index, not hertz: the deviation it produces scales with the carrier and the source's harmonicity. A zero or absent amount makes no connection at all, and a gap severs the chain rather than being stepped over. Sixteen routes per event is the ceiling; beyond it the event is refused. The matrix bends the oscillator voices - sine, triangle, square, sawtooth, supersaw, pulse, bytebeat; samples, wavetables and the zzfx family ignore it.",
            params: &[
                ReferenceParam {
                    name: "value",
                    r#type: "number | Pattern",
                    description: "modulation index; 0 or absent = no route",
                },
            ],
            examples: &[
                "note(\"c2 e2\").s(\"sine\").fmi44(\"<0 1 4>\")",
            ],
            tags: &["control", "fm"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["fmi45"],
        aliases: &["fm45"],
        reference:         ReferenceEntry {
            name: "fmi45",
            synonyms: &["fm45"],
            summary: "sends operator 4 into operator 5",
            description: "One cell of the FM matrix: the modulation operator 4 bends operator 5. An operator is built the first time any route names it at either end. The value is a modulation depth - an index, not hertz: the deviation it produces scales with the carrier and the source's harmonicity. A zero or absent amount makes no connection at all, and a gap severs the chain rather than being stepped over. Sixteen routes per event is the ceiling; beyond it the event is refused. The matrix bends the oscillator voices - sine, triangle, square, sawtooth, supersaw, pulse, bytebeat; samples, wavetables and the zzfx family ignore it.",
            params: &[
                ReferenceParam {
                    name: "value",
                    r#type: "number | Pattern",
                    description: "modulation index; 0 or absent = no route",
                },
            ],
            examples: &[
                "note(\"c2 e2\").s(\"sine\").fmi45(\"<0 1 4>\")",
            ],
            tags: &["control", "fm"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["fmi46"],
        aliases: &["fm46"],
        reference:         ReferenceEntry {
            name: "fmi46",
            synonyms: &["fm46"],
            summary: "sends operator 4 into operator 6",
            description: "One cell of the FM matrix: the modulation operator 4 bends operator 6. An operator is built the first time any route names it at either end. The value is a modulation depth - an index, not hertz: the deviation it produces scales with the carrier and the source's harmonicity. A zero or absent amount makes no connection at all, and a gap severs the chain rather than being stepped over. Sixteen routes per event is the ceiling; beyond it the event is refused. The matrix bends the oscillator voices - sine, triangle, square, sawtooth, supersaw, pulse, bytebeat; samples, wavetables and the zzfx family ignore it.",
            params: &[
                ReferenceParam {
                    name: "value",
                    r#type: "number | Pattern",
                    description: "modulation index; 0 or absent = no route",
                },
            ],
            examples: &[
                "note(\"c2 e2\").s(\"sine\").fmi46(\"<0 1 4>\")",
            ],
            tags: &["control", "fm"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["fmi47"],
        aliases: &["fm47"],
        reference:         ReferenceEntry {
            name: "fmi47",
            synonyms: &["fm47"],
            summary: "sends operator 4 into operator 7",
            description: "One cell of the FM matrix: the modulation operator 4 bends operator 7. An operator is built the first time any route names it at either end. The value is a modulation depth - an index, not hertz: the deviation it produces scales with the carrier and the source's harmonicity. A zero or absent amount makes no connection at all, and a gap severs the chain rather than being stepped over. Sixteen routes per event is the ceiling; beyond it the event is refused. The matrix bends the oscillator voices - sine, triangle, square, sawtooth, supersaw, pulse, bytebeat; samples, wavetables and the zzfx family ignore it.",
            params: &[
                ReferenceParam {
                    name: "value",
                    r#type: "number | Pattern",
                    description: "modulation index; 0 or absent = no route",
                },
            ],
            examples: &[
                "note(\"c2 e2\").s(\"sine\").fmi47(\"<0 1 4>\")",
            ],
            tags: &["control", "fm"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["fmi48"],
        aliases: &["fm48"],
        reference:         ReferenceEntry {
            name: "fmi48",
            synonyms: &["fm48"],
            summary: "sends operator 4 into operator 8",
            description: "One cell of the FM matrix: the modulation operator 4 bends operator 8. An operator is built the first time any route names it at either end. The value is a modulation depth - an index, not hertz: the deviation it produces scales with the carrier and the source's harmonicity. A zero or absent amount makes no connection at all, and a gap severs the chain rather than being stepped over. Sixteen routes per event is the ceiling; beyond it the event is refused. The matrix bends the oscillator voices - sine, triangle, square, sawtooth, supersaw, pulse, bytebeat; samples, wavetables and the zzfx family ignore it.",
            params: &[
                ReferenceParam {
                    name: "value",
                    r#type: "number | Pattern",
                    description: "modulation index; 0 or absent = no route",
                },
            ],
            examples: &[
                "note(\"c2 e2\").s(\"sine\").fmi48(\"<0 1 4>\")",
            ],
            tags: &["control", "fm"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["fmi50"],
        aliases: &["fm50"],
        reference:         ReferenceEntry {
            name: "fmi50",
            synonyms: &["fm50"],
            summary: "sends operator 5 into the carrier's frequency",
            description: "One cell of the FM matrix: the modulation operator 5 bends the carrier's frequency. An operator is built the first time any route names it at either end. The value is a modulation depth - an index, not hertz: the deviation it produces scales with the carrier and the source's harmonicity. A zero or absent amount makes no connection at all, and a gap severs the chain rather than being stepped over. Sixteen routes per event is the ceiling; beyond it the event is refused. The matrix bends the oscillator voices - sine, triangle, square, sawtooth, supersaw, pulse, bytebeat; samples, wavetables and the zzfx family ignore it.",
            params: &[
                ReferenceParam {
                    name: "value",
                    r#type: "number | Pattern",
                    description: "modulation index; 0 or absent = no route",
                },
            ],
            examples: &[
                "note(\"c2 e2\").s(\"sine\").fmi50(\"<0 1 4>\")",
            ],
            tags: &["control", "fm"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["fmi51"],
        aliases: &["fm51"],
        reference:         ReferenceEntry {
            name: "fmi51",
            synonyms: &["fm51"],
            summary: "sends operator 5 into operator 1",
            description: "One cell of the FM matrix: the modulation operator 5 bends operator 1. An operator is built the first time any route names it at either end. The value is a modulation depth - an index, not hertz: the deviation it produces scales with the carrier and the source's harmonicity. A zero or absent amount makes no connection at all, and a gap severs the chain rather than being stepped over. Sixteen routes per event is the ceiling; beyond it the event is refused. The matrix bends the oscillator voices - sine, triangle, square, sawtooth, supersaw, pulse, bytebeat; samples, wavetables and the zzfx family ignore it.",
            params: &[
                ReferenceParam {
                    name: "value",
                    r#type: "number | Pattern",
                    description: "modulation index; 0 or absent = no route",
                },
            ],
            examples: &[
                "note(\"c2 e2\").s(\"sine\").fmi51(\"<0 1 4>\")",
            ],
            tags: &["control", "fm"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["fmi52"],
        aliases: &["fm52"],
        reference:         ReferenceEntry {
            name: "fmi52",
            synonyms: &["fm52"],
            summary: "sends operator 5 into operator 2",
            description: "One cell of the FM matrix: the modulation operator 5 bends operator 2. An operator is built the first time any route names it at either end. The value is a modulation depth - an index, not hertz: the deviation it produces scales with the carrier and the source's harmonicity. A zero or absent amount makes no connection at all, and a gap severs the chain rather than being stepped over. Sixteen routes per event is the ceiling; beyond it the event is refused. The matrix bends the oscillator voices - sine, triangle, square, sawtooth, supersaw, pulse, bytebeat; samples, wavetables and the zzfx family ignore it.",
            params: &[
                ReferenceParam {
                    name: "value",
                    r#type: "number | Pattern",
                    description: "modulation index; 0 or absent = no route",
                },
            ],
            examples: &[
                "note(\"c2 e2\").s(\"sine\").fmi52(\"<0 1 4>\")",
            ],
            tags: &["control", "fm"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["fmi53"],
        aliases: &["fm53"],
        reference:         ReferenceEntry {
            name: "fmi53",
            synonyms: &["fm53"],
            summary: "sends operator 5 into operator 3",
            description: "One cell of the FM matrix: the modulation operator 5 bends operator 3. An operator is built the first time any route names it at either end. The value is a modulation depth - an index, not hertz: the deviation it produces scales with the carrier and the source's harmonicity. A zero or absent amount makes no connection at all, and a gap severs the chain rather than being stepped over. Sixteen routes per event is the ceiling; beyond it the event is refused. The matrix bends the oscillator voices - sine, triangle, square, sawtooth, supersaw, pulse, bytebeat; samples, wavetables and the zzfx family ignore it.",
            params: &[
                ReferenceParam {
                    name: "value",
                    r#type: "number | Pattern",
                    description: "modulation index; 0 or absent = no route",
                },
            ],
            examples: &[
                "note(\"c2 e2\").s(\"sine\").fmi53(\"<0 1 4>\")",
            ],
            tags: &["control", "fm"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["fmi54"],
        aliases: &["fm54"],
        reference:         ReferenceEntry {
            name: "fmi54",
            synonyms: &["fm54"],
            summary: "operator 5 into operator 4 - spelled fmi5 here",
            description: "This cell is the diagonal the resolver reads under the short spelling fmi5: operator 1 is unsuffixed throughout, so the fmi54 form is pinned for surface compatibility but never read. The behaviour is the fmi5 entry's. The value is a modulation depth - an index, not hertz: the deviation it produces scales with the carrier and the source's harmonicity. A zero or absent amount makes no connection at all, and a gap severs the chain rather than being stepped over. Sixteen routes per event is the ceiling; beyond it the event is refused. The matrix bends the oscillator voices - sine, triangle, square, sawtooth, supersaw, pulse, bytebeat; samples, wavetables and the zzfx family ignore it.",
            params: &[
                ReferenceParam {
                    name: "value",
                    r#type: "number | Pattern",
                    description: "modulation index; 0 or absent = no route",
                },
            ],
            examples: &[
                "note(\"c2 e2\").s(\"sine\").fmi5(\"<0 2 4 8>\")",
            ],
            tags: &["control", "fm"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["fmi55"],
        aliases: &["fm55"],
        reference:         ReferenceEntry {
            name: "fmi55",
            synonyms: &["fm55"],
            summary: "sends operator 5 into operator 5 - itself, as feedback",
            description: "One cell of the FM matrix: the modulation operator 5 bends operator 5 - itself, as feedback. An operator is built the first time any route names it at either end. The value is a modulation depth - an index, not hertz: the deviation it produces scales with the carrier and the source's harmonicity. A zero or absent amount makes no connection at all, and a gap severs the chain rather than being stepped over. Sixteen routes per event is the ceiling; beyond it the event is refused. The matrix bends the oscillator voices - sine, triangle, square, sawtooth, supersaw, pulse, bytebeat; samples, wavetables and the zzfx family ignore it.",
            params: &[
                ReferenceParam {
                    name: "value",
                    r#type: "number | Pattern",
                    description: "modulation index; 0 or absent = no route",
                },
            ],
            examples: &[
                "note(\"c2 e2\").s(\"sine\").fmi55(\"<0 1 4>\")",
            ],
            tags: &["control", "fm"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["fmi56"],
        aliases: &["fm56"],
        reference:         ReferenceEntry {
            name: "fmi56",
            synonyms: &["fm56"],
            summary: "sends operator 5 into operator 6",
            description: "One cell of the FM matrix: the modulation operator 5 bends operator 6. An operator is built the first time any route names it at either end. The value is a modulation depth - an index, not hertz: the deviation it produces scales with the carrier and the source's harmonicity. A zero or absent amount makes no connection at all, and a gap severs the chain rather than being stepped over. Sixteen routes per event is the ceiling; beyond it the event is refused. The matrix bends the oscillator voices - sine, triangle, square, sawtooth, supersaw, pulse, bytebeat; samples, wavetables and the zzfx family ignore it.",
            params: &[
                ReferenceParam {
                    name: "value",
                    r#type: "number | Pattern",
                    description: "modulation index; 0 or absent = no route",
                },
            ],
            examples: &[
                "note(\"c2 e2\").s(\"sine\").fmi56(\"<0 1 4>\")",
            ],
            tags: &["control", "fm"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["fmi57"],
        aliases: &["fm57"],
        reference:         ReferenceEntry {
            name: "fmi57",
            synonyms: &["fm57"],
            summary: "sends operator 5 into operator 7",
            description: "One cell of the FM matrix: the modulation operator 5 bends operator 7. An operator is built the first time any route names it at either end. The value is a modulation depth - an index, not hertz: the deviation it produces scales with the carrier and the source's harmonicity. A zero or absent amount makes no connection at all, and a gap severs the chain rather than being stepped over. Sixteen routes per event is the ceiling; beyond it the event is refused. The matrix bends the oscillator voices - sine, triangle, square, sawtooth, supersaw, pulse, bytebeat; samples, wavetables and the zzfx family ignore it.",
            params: &[
                ReferenceParam {
                    name: "value",
                    r#type: "number | Pattern",
                    description: "modulation index; 0 or absent = no route",
                },
            ],
            examples: &[
                "note(\"c2 e2\").s(\"sine\").fmi57(\"<0 1 4>\")",
            ],
            tags: &["control", "fm"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["fmi58"],
        aliases: &["fm58"],
        reference:         ReferenceEntry {
            name: "fmi58",
            synonyms: &["fm58"],
            summary: "sends operator 5 into operator 8",
            description: "One cell of the FM matrix: the modulation operator 5 bends operator 8. An operator is built the first time any route names it at either end. The value is a modulation depth - an index, not hertz: the deviation it produces scales with the carrier and the source's harmonicity. A zero or absent amount makes no connection at all, and a gap severs the chain rather than being stepped over. Sixteen routes per event is the ceiling; beyond it the event is refused. The matrix bends the oscillator voices - sine, triangle, square, sawtooth, supersaw, pulse, bytebeat; samples, wavetables and the zzfx family ignore it.",
            params: &[
                ReferenceParam {
                    name: "value",
                    r#type: "number | Pattern",
                    description: "modulation index; 0 or absent = no route",
                },
            ],
            examples: &[
                "note(\"c2 e2\").s(\"sine\").fmi58(\"<0 1 4>\")",
            ],
            tags: &["control", "fm"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["fmi60"],
        aliases: &["fm60"],
        reference:         ReferenceEntry {
            name: "fmi60",
            synonyms: &["fm60"],
            summary: "sends operator 6 into the carrier's frequency",
            description: "One cell of the FM matrix: the modulation operator 6 bends the carrier's frequency. An operator is built the first time any route names it at either end. The value is a modulation depth - an index, not hertz: the deviation it produces scales with the carrier and the source's harmonicity. A zero or absent amount makes no connection at all, and a gap severs the chain rather than being stepped over. Sixteen routes per event is the ceiling; beyond it the event is refused. The matrix bends the oscillator voices - sine, triangle, square, sawtooth, supersaw, pulse, bytebeat; samples, wavetables and the zzfx family ignore it.",
            params: &[
                ReferenceParam {
                    name: "value",
                    r#type: "number | Pattern",
                    description: "modulation index; 0 or absent = no route",
                },
            ],
            examples: &[
                "note(\"c2 e2\").s(\"sine\").fmi60(\"<0 1 4>\")",
            ],
            tags: &["control", "fm"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["fmi61"],
        aliases: &["fm61"],
        reference:         ReferenceEntry {
            name: "fmi61",
            synonyms: &["fm61"],
            summary: "sends operator 6 into operator 1",
            description: "One cell of the FM matrix: the modulation operator 6 bends operator 1. An operator is built the first time any route names it at either end. The value is a modulation depth - an index, not hertz: the deviation it produces scales with the carrier and the source's harmonicity. A zero or absent amount makes no connection at all, and a gap severs the chain rather than being stepped over. Sixteen routes per event is the ceiling; beyond it the event is refused. The matrix bends the oscillator voices - sine, triangle, square, sawtooth, supersaw, pulse, bytebeat; samples, wavetables and the zzfx family ignore it.",
            params: &[
                ReferenceParam {
                    name: "value",
                    r#type: "number | Pattern",
                    description: "modulation index; 0 or absent = no route",
                },
            ],
            examples: &[
                "note(\"c2 e2\").s(\"sine\").fmi61(\"<0 1 4>\")",
            ],
            tags: &["control", "fm"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["fmi62"],
        aliases: &["fm62"],
        reference:         ReferenceEntry {
            name: "fmi62",
            synonyms: &["fm62"],
            summary: "sends operator 6 into operator 2",
            description: "One cell of the FM matrix: the modulation operator 6 bends operator 2. An operator is built the first time any route names it at either end. The value is a modulation depth - an index, not hertz: the deviation it produces scales with the carrier and the source's harmonicity. A zero or absent amount makes no connection at all, and a gap severs the chain rather than being stepped over. Sixteen routes per event is the ceiling; beyond it the event is refused. The matrix bends the oscillator voices - sine, triangle, square, sawtooth, supersaw, pulse, bytebeat; samples, wavetables and the zzfx family ignore it.",
            params: &[
                ReferenceParam {
                    name: "value",
                    r#type: "number | Pattern",
                    description: "modulation index; 0 or absent = no route",
                },
            ],
            examples: &[
                "note(\"c2 e2\").s(\"sine\").fmi62(\"<0 1 4>\")",
            ],
            tags: &["control", "fm"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["fmi63"],
        aliases: &["fm63"],
        reference:         ReferenceEntry {
            name: "fmi63",
            synonyms: &["fm63"],
            summary: "sends operator 6 into operator 3",
            description: "One cell of the FM matrix: the modulation operator 6 bends operator 3. An operator is built the first time any route names it at either end. The value is a modulation depth - an index, not hertz: the deviation it produces scales with the carrier and the source's harmonicity. A zero or absent amount makes no connection at all, and a gap severs the chain rather than being stepped over. Sixteen routes per event is the ceiling; beyond it the event is refused. The matrix bends the oscillator voices - sine, triangle, square, sawtooth, supersaw, pulse, bytebeat; samples, wavetables and the zzfx family ignore it.",
            params: &[
                ReferenceParam {
                    name: "value",
                    r#type: "number | Pattern",
                    description: "modulation index; 0 or absent = no route",
                },
            ],
            examples: &[
                "note(\"c2 e2\").s(\"sine\").fmi63(\"<0 1 4>\")",
            ],
            tags: &["control", "fm"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["fmi64"],
        aliases: &["fm64"],
        reference:         ReferenceEntry {
            name: "fmi64",
            synonyms: &["fm64"],
            summary: "sends operator 6 into operator 4",
            description: "One cell of the FM matrix: the modulation operator 6 bends operator 4. An operator is built the first time any route names it at either end. The value is a modulation depth - an index, not hertz: the deviation it produces scales with the carrier and the source's harmonicity. A zero or absent amount makes no connection at all, and a gap severs the chain rather than being stepped over. Sixteen routes per event is the ceiling; beyond it the event is refused. The matrix bends the oscillator voices - sine, triangle, square, sawtooth, supersaw, pulse, bytebeat; samples, wavetables and the zzfx family ignore it.",
            params: &[
                ReferenceParam {
                    name: "value",
                    r#type: "number | Pattern",
                    description: "modulation index; 0 or absent = no route",
                },
            ],
            examples: &[
                "note(\"c2 e2\").s(\"sine\").fmi64(\"<0 1 4>\")",
            ],
            tags: &["control", "fm"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["fmi65"],
        aliases: &["fm65"],
        reference:         ReferenceEntry {
            name: "fmi65",
            synonyms: &["fm65"],
            summary: "operator 6 into operator 5 - spelled fmi6 here",
            description: "This cell is the diagonal the resolver reads under the short spelling fmi6: operator 1 is unsuffixed throughout, so the fmi65 form is pinned for surface compatibility but never read. The behaviour is the fmi6 entry's. The value is a modulation depth - an index, not hertz: the deviation it produces scales with the carrier and the source's harmonicity. A zero or absent amount makes no connection at all, and a gap severs the chain rather than being stepped over. Sixteen routes per event is the ceiling; beyond it the event is refused. The matrix bends the oscillator voices - sine, triangle, square, sawtooth, supersaw, pulse, bytebeat; samples, wavetables and the zzfx family ignore it.",
            params: &[
                ReferenceParam {
                    name: "value",
                    r#type: "number | Pattern",
                    description: "modulation index; 0 or absent = no route",
                },
            ],
            examples: &[
                "note(\"c2 e2\").s(\"sine\").fmi6(\"<0 2 4 8>\")",
            ],
            tags: &["control", "fm"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["fmi66"],
        aliases: &["fm66"],
        reference:         ReferenceEntry {
            name: "fmi66",
            synonyms: &["fm66"],
            summary: "sends operator 6 into operator 6 - itself, as feedback",
            description: "One cell of the FM matrix: the modulation operator 6 bends operator 6 - itself, as feedback. An operator is built the first time any route names it at either end. The value is a modulation depth - an index, not hertz: the deviation it produces scales with the carrier and the source's harmonicity. A zero or absent amount makes no connection at all, and a gap severs the chain rather than being stepped over. Sixteen routes per event is the ceiling; beyond it the event is refused. The matrix bends the oscillator voices - sine, triangle, square, sawtooth, supersaw, pulse, bytebeat; samples, wavetables and the zzfx family ignore it.",
            params: &[
                ReferenceParam {
                    name: "value",
                    r#type: "number | Pattern",
                    description: "modulation index; 0 or absent = no route",
                },
            ],
            examples: &[
                "note(\"c2 e2\").s(\"sine\").fmi66(\"<0 1 4>\")",
            ],
            tags: &["control", "fm"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["fmi67"],
        aliases: &["fm67"],
        reference:         ReferenceEntry {
            name: "fmi67",
            synonyms: &["fm67"],
            summary: "sends operator 6 into operator 7",
            description: "One cell of the FM matrix: the modulation operator 6 bends operator 7. An operator is built the first time any route names it at either end. The value is a modulation depth - an index, not hertz: the deviation it produces scales with the carrier and the source's harmonicity. A zero or absent amount makes no connection at all, and a gap severs the chain rather than being stepped over. Sixteen routes per event is the ceiling; beyond it the event is refused. The matrix bends the oscillator voices - sine, triangle, square, sawtooth, supersaw, pulse, bytebeat; samples, wavetables and the zzfx family ignore it.",
            params: &[
                ReferenceParam {
                    name: "value",
                    r#type: "number | Pattern",
                    description: "modulation index; 0 or absent = no route",
                },
            ],
            examples: &[
                "note(\"c2 e2\").s(\"sine\").fmi67(\"<0 1 4>\")",
            ],
            tags: &["control", "fm"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["fmi68"],
        aliases: &["fm68"],
        reference:         ReferenceEntry {
            name: "fmi68",
            synonyms: &["fm68"],
            summary: "sends operator 6 into operator 8",
            description: "One cell of the FM matrix: the modulation operator 6 bends operator 8. An operator is built the first time any route names it at either end. The value is a modulation depth - an index, not hertz: the deviation it produces scales with the carrier and the source's harmonicity. A zero or absent amount makes no connection at all, and a gap severs the chain rather than being stepped over. Sixteen routes per event is the ceiling; beyond it the event is refused. The matrix bends the oscillator voices - sine, triangle, square, sawtooth, supersaw, pulse, bytebeat; samples, wavetables and the zzfx family ignore it.",
            params: &[
                ReferenceParam {
                    name: "value",
                    r#type: "number | Pattern",
                    description: "modulation index; 0 or absent = no route",
                },
            ],
            examples: &[
                "note(\"c2 e2\").s(\"sine\").fmi68(\"<0 1 4>\")",
            ],
            tags: &["control", "fm"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["fmi70"],
        aliases: &["fm70"],
        reference:         ReferenceEntry {
            name: "fmi70",
            synonyms: &["fm70"],
            summary: "sends operator 7 into the carrier's frequency",
            description: "One cell of the FM matrix: the modulation operator 7 bends the carrier's frequency. An operator is built the first time any route names it at either end. The value is a modulation depth - an index, not hertz: the deviation it produces scales with the carrier and the source's harmonicity. A zero or absent amount makes no connection at all, and a gap severs the chain rather than being stepped over. Sixteen routes per event is the ceiling; beyond it the event is refused. The matrix bends the oscillator voices - sine, triangle, square, sawtooth, supersaw, pulse, bytebeat; samples, wavetables and the zzfx family ignore it.",
            params: &[
                ReferenceParam {
                    name: "value",
                    r#type: "number | Pattern",
                    description: "modulation index; 0 or absent = no route",
                },
            ],
            examples: &[
                "note(\"c2 e2\").s(\"sine\").fmi70(\"<0 1 4>\")",
            ],
            tags: &["control", "fm"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["fmi71"],
        aliases: &["fm71"],
        reference:         ReferenceEntry {
            name: "fmi71",
            synonyms: &["fm71"],
            summary: "sends operator 7 into operator 1",
            description: "One cell of the FM matrix: the modulation operator 7 bends operator 1. An operator is built the first time any route names it at either end. The value is a modulation depth - an index, not hertz: the deviation it produces scales with the carrier and the source's harmonicity. A zero or absent amount makes no connection at all, and a gap severs the chain rather than being stepped over. Sixteen routes per event is the ceiling; beyond it the event is refused. The matrix bends the oscillator voices - sine, triangle, square, sawtooth, supersaw, pulse, bytebeat; samples, wavetables and the zzfx family ignore it.",
            params: &[
                ReferenceParam {
                    name: "value",
                    r#type: "number | Pattern",
                    description: "modulation index; 0 or absent = no route",
                },
            ],
            examples: &[
                "note(\"c2 e2\").s(\"sine\").fmi71(\"<0 1 4>\")",
            ],
            tags: &["control", "fm"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["fmi72"],
        aliases: &["fm72"],
        reference:         ReferenceEntry {
            name: "fmi72",
            synonyms: &["fm72"],
            summary: "sends operator 7 into operator 2",
            description: "One cell of the FM matrix: the modulation operator 7 bends operator 2. An operator is built the first time any route names it at either end. The value is a modulation depth - an index, not hertz: the deviation it produces scales with the carrier and the source's harmonicity. A zero or absent amount makes no connection at all, and a gap severs the chain rather than being stepped over. Sixteen routes per event is the ceiling; beyond it the event is refused. The matrix bends the oscillator voices - sine, triangle, square, sawtooth, supersaw, pulse, bytebeat; samples, wavetables and the zzfx family ignore it.",
            params: &[
                ReferenceParam {
                    name: "value",
                    r#type: "number | Pattern",
                    description: "modulation index; 0 or absent = no route",
                },
            ],
            examples: &[
                "note(\"c2 e2\").s(\"sine\").fmi72(\"<0 1 4>\")",
            ],
            tags: &["control", "fm"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["fmi73"],
        aliases: &["fm73"],
        reference:         ReferenceEntry {
            name: "fmi73",
            synonyms: &["fm73"],
            summary: "sends operator 7 into operator 3",
            description: "One cell of the FM matrix: the modulation operator 7 bends operator 3. An operator is built the first time any route names it at either end. The value is a modulation depth - an index, not hertz: the deviation it produces scales with the carrier and the source's harmonicity. A zero or absent amount makes no connection at all, and a gap severs the chain rather than being stepped over. Sixteen routes per event is the ceiling; beyond it the event is refused. The matrix bends the oscillator voices - sine, triangle, square, sawtooth, supersaw, pulse, bytebeat; samples, wavetables and the zzfx family ignore it.",
            params: &[
                ReferenceParam {
                    name: "value",
                    r#type: "number | Pattern",
                    description: "modulation index; 0 or absent = no route",
                },
            ],
            examples: &[
                "note(\"c2 e2\").s(\"sine\").fmi73(\"<0 1 4>\")",
            ],
            tags: &["control", "fm"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["fmi74"],
        aliases: &["fm74"],
        reference:         ReferenceEntry {
            name: "fmi74",
            synonyms: &["fm74"],
            summary: "sends operator 7 into operator 4",
            description: "One cell of the FM matrix: the modulation operator 7 bends operator 4. An operator is built the first time any route names it at either end. The value is a modulation depth - an index, not hertz: the deviation it produces scales with the carrier and the source's harmonicity. A zero or absent amount makes no connection at all, and a gap severs the chain rather than being stepped over. Sixteen routes per event is the ceiling; beyond it the event is refused. The matrix bends the oscillator voices - sine, triangle, square, sawtooth, supersaw, pulse, bytebeat; samples, wavetables and the zzfx family ignore it.",
            params: &[
                ReferenceParam {
                    name: "value",
                    r#type: "number | Pattern",
                    description: "modulation index; 0 or absent = no route",
                },
            ],
            examples: &[
                "note(\"c2 e2\").s(\"sine\").fmi74(\"<0 1 4>\")",
            ],
            tags: &["control", "fm"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["fmi75"],
        aliases: &["fm75"],
        reference:         ReferenceEntry {
            name: "fmi75",
            synonyms: &["fm75"],
            summary: "sends operator 7 into operator 5",
            description: "One cell of the FM matrix: the modulation operator 7 bends operator 5. An operator is built the first time any route names it at either end. The value is a modulation depth - an index, not hertz: the deviation it produces scales with the carrier and the source's harmonicity. A zero or absent amount makes no connection at all, and a gap severs the chain rather than being stepped over. Sixteen routes per event is the ceiling; beyond it the event is refused. The matrix bends the oscillator voices - sine, triangle, square, sawtooth, supersaw, pulse, bytebeat; samples, wavetables and the zzfx family ignore it.",
            params: &[
                ReferenceParam {
                    name: "value",
                    r#type: "number | Pattern",
                    description: "modulation index; 0 or absent = no route",
                },
            ],
            examples: &[
                "note(\"c2 e2\").s(\"sine\").fmi75(\"<0 1 4>\")",
            ],
            tags: &["control", "fm"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["fmi76"],
        aliases: &["fm76"],
        reference:         ReferenceEntry {
            name: "fmi76",
            synonyms: &["fm76"],
            summary: "operator 7 into operator 6 - spelled fmi7 here",
            description: "This cell is the diagonal the resolver reads under the short spelling fmi7: operator 1 is unsuffixed throughout, so the fmi76 form is pinned for surface compatibility but never read. The behaviour is the fmi7 entry's. The value is a modulation depth - an index, not hertz: the deviation it produces scales with the carrier and the source's harmonicity. A zero or absent amount makes no connection at all, and a gap severs the chain rather than being stepped over. Sixteen routes per event is the ceiling; beyond it the event is refused. The matrix bends the oscillator voices - sine, triangle, square, sawtooth, supersaw, pulse, bytebeat; samples, wavetables and the zzfx family ignore it.",
            params: &[
                ReferenceParam {
                    name: "value",
                    r#type: "number | Pattern",
                    description: "modulation index; 0 or absent = no route",
                },
            ],
            examples: &[
                "note(\"c2 e2\").s(\"sine\").fmi7(\"<0 2 4 8>\")",
            ],
            tags: &["control", "fm"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["fmi77"],
        aliases: &["fm77"],
        reference:         ReferenceEntry {
            name: "fmi77",
            synonyms: &["fm77"],
            summary: "sends operator 7 into operator 7 - itself, as feedback",
            description: "One cell of the FM matrix: the modulation operator 7 bends operator 7 - itself, as feedback. An operator is built the first time any route names it at either end. The value is a modulation depth - an index, not hertz: the deviation it produces scales with the carrier and the source's harmonicity. A zero or absent amount makes no connection at all, and a gap severs the chain rather than being stepped over. Sixteen routes per event is the ceiling; beyond it the event is refused. The matrix bends the oscillator voices - sine, triangle, square, sawtooth, supersaw, pulse, bytebeat; samples, wavetables and the zzfx family ignore it.",
            params: &[
                ReferenceParam {
                    name: "value",
                    r#type: "number | Pattern",
                    description: "modulation index; 0 or absent = no route",
                },
            ],
            examples: &[
                "note(\"c2 e2\").s(\"sine\").fmi77(\"<0 1 4>\")",
            ],
            tags: &["control", "fm"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["fmi78"],
        aliases: &["fm78"],
        reference:         ReferenceEntry {
            name: "fmi78",
            synonyms: &["fm78"],
            summary: "sends operator 7 into operator 8",
            description: "One cell of the FM matrix: the modulation operator 7 bends operator 8. An operator is built the first time any route names it at either end. The value is a modulation depth - an index, not hertz: the deviation it produces scales with the carrier and the source's harmonicity. A zero or absent amount makes no connection at all, and a gap severs the chain rather than being stepped over. Sixteen routes per event is the ceiling; beyond it the event is refused. The matrix bends the oscillator voices - sine, triangle, square, sawtooth, supersaw, pulse, bytebeat; samples, wavetables and the zzfx family ignore it.",
            params: &[
                ReferenceParam {
                    name: "value",
                    r#type: "number | Pattern",
                    description: "modulation index; 0 or absent = no route",
                },
            ],
            examples: &[
                "note(\"c2 e2\").s(\"sine\").fmi78(\"<0 1 4>\")",
            ],
            tags: &["control", "fm"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["fmi80"],
        aliases: &["fm80"],
        reference:         ReferenceEntry {
            name: "fmi80",
            synonyms: &["fm80"],
            summary: "sends operator 8 into the carrier's frequency",
            description: "One cell of the FM matrix: the modulation operator 8 bends the carrier's frequency. An operator is built the first time any route names it at either end. The value is a modulation depth - an index, not hertz: the deviation it produces scales with the carrier and the source's harmonicity. A zero or absent amount makes no connection at all, and a gap severs the chain rather than being stepped over. Sixteen routes per event is the ceiling; beyond it the event is refused. The matrix bends the oscillator voices - sine, triangle, square, sawtooth, supersaw, pulse, bytebeat; samples, wavetables and the zzfx family ignore it.",
            params: &[
                ReferenceParam {
                    name: "value",
                    r#type: "number | Pattern",
                    description: "modulation index; 0 or absent = no route",
                },
            ],
            examples: &[
                "note(\"c2 e2\").s(\"sine\").fmi80(\"<0 1 4>\")",
            ],
            tags: &["control", "fm"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["fmi81"],
        aliases: &["fm81"],
        reference:         ReferenceEntry {
            name: "fmi81",
            synonyms: &["fm81"],
            summary: "sends operator 8 into operator 1",
            description: "One cell of the FM matrix: the modulation operator 8 bends operator 1. An operator is built the first time any route names it at either end. The value is a modulation depth - an index, not hertz: the deviation it produces scales with the carrier and the source's harmonicity. A zero or absent amount makes no connection at all, and a gap severs the chain rather than being stepped over. Sixteen routes per event is the ceiling; beyond it the event is refused. The matrix bends the oscillator voices - sine, triangle, square, sawtooth, supersaw, pulse, bytebeat; samples, wavetables and the zzfx family ignore it.",
            params: &[
                ReferenceParam {
                    name: "value",
                    r#type: "number | Pattern",
                    description: "modulation index; 0 or absent = no route",
                },
            ],
            examples: &[
                "note(\"c2 e2\").s(\"sine\").fmi81(\"<0 1 4>\")",
            ],
            tags: &["control", "fm"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["fmi82"],
        aliases: &["fm82"],
        reference:         ReferenceEntry {
            name: "fmi82",
            synonyms: &["fm82"],
            summary: "sends operator 8 into operator 2",
            description: "One cell of the FM matrix: the modulation operator 8 bends operator 2. An operator is built the first time any route names it at either end. The value is a modulation depth - an index, not hertz: the deviation it produces scales with the carrier and the source's harmonicity. A zero or absent amount makes no connection at all, and a gap severs the chain rather than being stepped over. Sixteen routes per event is the ceiling; beyond it the event is refused. The matrix bends the oscillator voices - sine, triangle, square, sawtooth, supersaw, pulse, bytebeat; samples, wavetables and the zzfx family ignore it.",
            params: &[
                ReferenceParam {
                    name: "value",
                    r#type: "number | Pattern",
                    description: "modulation index; 0 or absent = no route",
                },
            ],
            examples: &[
                "note(\"c2 e2\").s(\"sine\").fmi82(\"<0 1 4>\")",
            ],
            tags: &["control", "fm"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["fmi83"],
        aliases: &["fm83"],
        reference:         ReferenceEntry {
            name: "fmi83",
            synonyms: &["fm83"],
            summary: "sends operator 8 into operator 3",
            description: "One cell of the FM matrix: the modulation operator 8 bends operator 3. An operator is built the first time any route names it at either end. The value is a modulation depth - an index, not hertz: the deviation it produces scales with the carrier and the source's harmonicity. A zero or absent amount makes no connection at all, and a gap severs the chain rather than being stepped over. Sixteen routes per event is the ceiling; beyond it the event is refused. The matrix bends the oscillator voices - sine, triangle, square, sawtooth, supersaw, pulse, bytebeat; samples, wavetables and the zzfx family ignore it.",
            params: &[
                ReferenceParam {
                    name: "value",
                    r#type: "number | Pattern",
                    description: "modulation index; 0 or absent = no route",
                },
            ],
            examples: &[
                "note(\"c2 e2\").s(\"sine\").fmi83(\"<0 1 4>\")",
            ],
            tags: &["control", "fm"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["fmi84"],
        aliases: &["fm84"],
        reference:         ReferenceEntry {
            name: "fmi84",
            synonyms: &["fm84"],
            summary: "sends operator 8 into operator 4",
            description: "One cell of the FM matrix: the modulation operator 8 bends operator 4. An operator is built the first time any route names it at either end. The value is a modulation depth - an index, not hertz: the deviation it produces scales with the carrier and the source's harmonicity. A zero or absent amount makes no connection at all, and a gap severs the chain rather than being stepped over. Sixteen routes per event is the ceiling; beyond it the event is refused. The matrix bends the oscillator voices - sine, triangle, square, sawtooth, supersaw, pulse, bytebeat; samples, wavetables and the zzfx family ignore it.",
            params: &[
                ReferenceParam {
                    name: "value",
                    r#type: "number | Pattern",
                    description: "modulation index; 0 or absent = no route",
                },
            ],
            examples: &[
                "note(\"c2 e2\").s(\"sine\").fmi84(\"<0 1 4>\")",
            ],
            tags: &["control", "fm"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["fmi85"],
        aliases: &["fm85"],
        reference:         ReferenceEntry {
            name: "fmi85",
            synonyms: &["fm85"],
            summary: "sends operator 8 into operator 5",
            description: "One cell of the FM matrix: the modulation operator 8 bends operator 5. An operator is built the first time any route names it at either end. The value is a modulation depth - an index, not hertz: the deviation it produces scales with the carrier and the source's harmonicity. A zero or absent amount makes no connection at all, and a gap severs the chain rather than being stepped over. Sixteen routes per event is the ceiling; beyond it the event is refused. The matrix bends the oscillator voices - sine, triangle, square, sawtooth, supersaw, pulse, bytebeat; samples, wavetables and the zzfx family ignore it.",
            params: &[
                ReferenceParam {
                    name: "value",
                    r#type: "number | Pattern",
                    description: "modulation index; 0 or absent = no route",
                },
            ],
            examples: &[
                "note(\"c2 e2\").s(\"sine\").fmi85(\"<0 1 4>\")",
            ],
            tags: &["control", "fm"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["fmi86"],
        aliases: &["fm86"],
        reference:         ReferenceEntry {
            name: "fmi86",
            synonyms: &["fm86"],
            summary: "sends operator 8 into operator 6",
            description: "One cell of the FM matrix: the modulation operator 8 bends operator 6. An operator is built the first time any route names it at either end. The value is a modulation depth - an index, not hertz: the deviation it produces scales with the carrier and the source's harmonicity. A zero or absent amount makes no connection at all, and a gap severs the chain rather than being stepped over. Sixteen routes per event is the ceiling; beyond it the event is refused. The matrix bends the oscillator voices - sine, triangle, square, sawtooth, supersaw, pulse, bytebeat; samples, wavetables and the zzfx family ignore it.",
            params: &[
                ReferenceParam {
                    name: "value",
                    r#type: "number | Pattern",
                    description: "modulation index; 0 or absent = no route",
                },
            ],
            examples: &[
                "note(\"c2 e2\").s(\"sine\").fmi86(\"<0 1 4>\")",
            ],
            tags: &["control", "fm"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["fmi87"],
        aliases: &["fm87"],
        reference:         ReferenceEntry {
            name: "fmi87",
            synonyms: &["fm87"],
            summary: "operator 8 into operator 7 - spelled fmi8 here",
            description: "This cell is the diagonal the resolver reads under the short spelling fmi8: operator 1 is unsuffixed throughout, so the fmi87 form is pinned for surface compatibility but never read. The behaviour is the fmi8 entry's. The value is a modulation depth - an index, not hertz: the deviation it produces scales with the carrier and the source's harmonicity. A zero or absent amount makes no connection at all, and a gap severs the chain rather than being stepped over. Sixteen routes per event is the ceiling; beyond it the event is refused. The matrix bends the oscillator voices - sine, triangle, square, sawtooth, supersaw, pulse, bytebeat; samples, wavetables and the zzfx family ignore it.",
            params: &[
                ReferenceParam {
                    name: "value",
                    r#type: "number | Pattern",
                    description: "modulation index; 0 or absent = no route",
                },
            ],
            examples: &[
                "note(\"c2 e2\").s(\"sine\").fmi8(\"<0 2 4 8>\")",
            ],
            tags: &["control", "fm"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["fmi88"],
        aliases: &["fm88"],
        reference:         ReferenceEntry {
            name: "fmi88",
            synonyms: &["fm88"],
            summary: "sends operator 8 into operator 8 - itself, as feedback",
            description: "One cell of the FM matrix: the modulation operator 8 bends operator 8 - itself, as feedback. An operator is built the first time any route names it at either end. The value is a modulation depth - an index, not hertz: the deviation it produces scales with the carrier and the source's harmonicity. A zero or absent amount makes no connection at all, and a gap severs the chain rather than being stepped over. Sixteen routes per event is the ceiling; beyond it the event is refused. The matrix bends the oscillator voices - sine, triangle, square, sawtooth, supersaw, pulse, bytebeat; samples, wavetables and the zzfx family ignore it.",
            params: &[
                ReferenceParam {
                    name: "value",
                    r#type: "number | Pattern",
                    description: "modulation index; 0 or absent = no route",
                },
            ],
            examples: &[
                "note(\"c2 e2\").s(\"sine\").fmi88(\"<0 1 4>\")",
            ],
            tags: &["control", "fm"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["bank"],
        aliases: &[],
        reference: ReferenceEntry {
            name: "bank",
            synonyms: &[],
            summary: "Select the sound bank to use.",
            description: "Select the sound bank to use. To be used together with `s`. The bank name (+ \"_\") will be prepended to the value of `s`.",
            params: &[
                ReferenceParam {
                    name: "bank",
                    r#type: "string | Pattern",
                    description: "the name of the bank",
                },
            ],
            examples: &[
                "s(\"bd sd [~ bd] sd\").bank('RolandTR909') // = s(\"RolandTR909_bd RolandTR909_sd\")",
            ],
            tags: &["samples", "audio"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["chorus"],
        aliases: &[],
        reference: ReferenceEntry {
            name: "chorus",
            synonyms: &[],
            summary: "Chorus mix; unsupported in native audio.",
            description: "Rustel does not implement a chorus effect; this control does not change native audio.",
            params: &[
                ReferenceParam {
                    name: "chorus",
                    r#type: "string | Pattern",
                    description: "mix amount between 0 and 1",
                },
            ],
            examples: &[],
            tags: &["pitch"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["analyze"],
        aliases: &[],
        reference:         ReferenceEntry {
            name: "analyze",
            synonyms: &[],
            summary: "Analysis selection; unused by the native analyser.",
            description: "Rustel analyses the master output directly and does not use this control. The value can be sent through osc() or mapped to a MIDI CC with midimaps.",
            params: &[
                ReferenceParam {
                    name: "value",
                    r#type: "number | Pattern",
                    description: "analysis amount",
                },
            ],
            examples: &[],
            tags: &["control", "osc"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["fft"],
        aliases: &[],
        reference:         ReferenceEntry {
            name: "fft",
            synonyms: &[],
            summary: "FFT size; unused by the native analyser.",
            description: "Rustel analyses the master output directly and does not use this control. The value can be sent through osc() or mapped to a MIDI CC with midimaps.",
            params: &[
                ReferenceParam {
                    name: "value",
                    r#type: "number | Pattern",
                    description: "FFT size",
                },
            ],
            examples: &[],
            tags: &["control", "osc"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["attack"],
        aliases: &["att"],
        reference: ReferenceEntry {
            name: "attack",
            synonyms: &["att"],
            summary: "Amplitude-envelope attack time in seconds.",
            description: "Time from the onset to the peak. Ordinary envelope values are floored at 0.001 s. For ordinary synths, including noise and live input, the all-absent ADSR is 0.001 s attack, 0.05 s decay, 0.6 sustain, and 0.01 s release. Samples (including `gm_*` zones) and wavetables instead use 0.001 s, 0.001 s, 1, and 0.01 s. Once any ADSR field is explicit, omitted attack/decay become 0.001 s and release 0.01 s; omitted sustain is 0.001 when decay is set, otherwise 1. ZZFX has its own envelope: attack defaults to 0 and a raw zzfx array overrides this control. The sbd drum ignores attack and uses its own fixed onset shape.",
            params: &[
                ReferenceParam {
                    name: "attack",
                    r#type: "number | Pattern",
                    description: "time in seconds.",
                },
            ],
            examples: &[
                "note(\"c3 e3 f3 g3\").attack(\"<0 .1 .5>\")",
            ],
            tags: &["amplitude", "envelope", "audio"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["decay"],
        aliases: &["dec"],
        reference: ReferenceEntry {
            name: "decay",
            synonyms: &["dec"],
            summary: "Amplitude-envelope decay time in seconds.",
            description: "Time from the attack peak to the sustain level; a sustain of 1 leaves no decay to hear. Ordinary envelope values are floored at 0.001 s. For ordinary synths, including noise and live input, the all-absent ADSR is 0.001 s attack, 0.05 s decay, 0.6 sustain, and 0.01 s release. Samples (including `gm_*` zones) and wavetables instead use 0.001 s, 0.001 s, 1, and 0.01 s. Once any ADSR field is explicit, omitted attack/decay become 0.001 s and release 0.01 s; omitted sustain is 0.001 when decay is set, otherwise 1. ZZFX decay defaults to 0 and a raw zzfx array overrides it. The sbd drum instead uses decay as its own body-decay time, default 0.5 s, independent of sustain.",
            params: &[
                ReferenceParam {
                    name: "time",
                    r#type: "number | Pattern",
                    description: "decay time in seconds",
                },
            ],
            examples: &[
                "note(\"c3 e3 f3 g3\").decay(\"<.1 .2 .3 .4>\").sustain(0)",
            ],
            tags: &["amplitude", "envelope", "audio"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["sustain"],
        aliases: &["sus"],
        reference: ReferenceEntry {
            name: "sustain",
            synonyms: &["sus"],
            summary: "Amplitude-envelope sustain level, not a duration.",
            description: "The level after attack/decay, capped at 1 by the ordinary envelope. For ordinary synths, including noise and live input, the all-absent ADSR is 0.001 s attack, 0.05 s decay, 0.6 sustain, and 0.01 s release. Samples (including `gm_*` zones) and wavetables instead use 0.001 s, 0.001 s, 1, and 0.01 s. Once any ADSR field is explicit, omitted attack/decay become 0.001 s and release 0.01 s; omitted sustain is 0.001 when decay is set, otherwise 1. ZZFX uses sustain as its own sustain volume, default 0.8, unless a raw zzfx array overrides it. The sbd drum ignores sustain and follows its own decay.",
            params: &[
                ReferenceParam {
                    name: "gain",
                    r#type: "number | Pattern",
                    description: "sustain level between 0 and 1",
                },
            ],
            examples: &[
                "note(\"c3 e3 f3 g3\").decay(.2).sustain(\"<0 .1 .4 .6 1>\")",
            ],
            tags: &["amplitude", "envelope", "audio"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["release"],
        aliases: &["rel"],
        reference: ReferenceEntry {
            name: "release",
            synonyms: &["rel"],
            summary: "Amplitude-envelope release time in seconds.",
            description: "The time from the end of the event to silence. Ordinary envelope values are floored at 0.01 s. For ordinary synths, including noise and live input, the all-absent ADSR is 0.001 s attack, 0.05 s decay, 0.6 sustain, and 0.01 s release. Samples (including `gm_*` zones) and wavetables instead use 0.001 s, 0.001 s, 1, and 0.01 s. Once any ADSR field is explicit, omitted attack/decay become 0.001 s and release 0.01 s; omitted sustain is 0.001 when decay is set, otherwise 1. For a sample, explicitly setting release also makes the event duration govern its envelope instead of the slice duration. ZZFX release defaults to 0.1 s unless a raw zzfx array overrides it. The sbd drum ignores release for its source envelope.",
            params: &[
                ReferenceParam {
                    name: "time",
                    r#type: "number | Pattern",
                    description: "release time in seconds",
                },
            ],
            examples: &[
                "note(\"c3 e3 g3 c4\").release(\"<0 .1 .4 .6 1>/2\")",
            ],
            tags: &["amplitude", "envelope", "audio"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["hold"],
        aliases: &[],
        reference:         ReferenceEntry {
            name: "hold",
            synonyms: &[],
            summary: "SuperDirt (OSC): envelope hold time.",
            description: "SuperDirt via `.osc()`: sets the envelope plateau in seconds. Use with `attack` or `release`.",
            params: &[
                ReferenceParam {
                    name: "value",
                    r#type: "number | Pattern",
                    description: "hold time in seconds",
                },
            ],
            examples: &[],
            tags: &["superdirt", "osc"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["bandf", "bandq", "bpenv"],
        aliases: &["bpf", "bp"],
        reference: ReferenceEntry {
            name: "bpf",
            synonyms: &["bandf", "bp"],
            summary: "Sets the center frequency of the band-pass filter.",
            description: "Sets the center frequency of the band-pass filter. When using mininotation, you\ncan also optionally supply the 'bpq' parameter separated by ':'.\n\nThe filter is absent until bpf sets a frequency in hertz. Its resonance defaults to bpq(1). The corresponding bp envelope and LFO controls need this filter to exist; naming them alone does not filter the sound. Applies to any native audio source, including oscillators, wavetables, samples, `gm_*` soundfonts, and live input.",
            params: &[
                ReferenceParam {
                    name: "frequency",
                    r#type: "number | Pattern",
                    description: "center frequency",
                },
            ],
            examples: &[
                "s(\"bd sd [~ bd] sd,hh*6\").bpf(\"<1000 2000 4000 8000>\")",
            ],
            tags: &["filter", "audio"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["bandq"],
        aliases: &["bpq"],
        reference: ReferenceEntry {
            name: "bpq",
            synonyms: &["bandq"],
            summary: "Sets the band-pass q-factor (resonance).",
            description: "Sets the band-pass q-factor (resonance).\n\nDefaults to 1 and requires bpf; resonance alone creates no filter. It is a unitless Q value. The ladder model uses its own resonance mapping; see ftype.",
            params: &[
                ReferenceParam {
                    name: "q",
                    r#type: "number | Pattern",
                    description: "q factor",
                },
            ],
            examples: &[
                "s(\"bd sd [~ bd] sd\").bpf(500).bpq(\"<0 1 2 3>\")",
            ],
            tags: &["filter", "audio"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["begin"],
        aliases: &[],
        reference: ReferenceEntry {
            name: "begin",
            synonyms: &[],
            summary: "Start position within a recorded sample, from 0 to 1.",
            description: "Defaults to 0. For example, begin(0.25) skips the first quarter of the buffer. The slice must satisfy 0 <= begin < end <= 1; end defaults to 1. Negative speed reverses the buffer before these positions are applied. Applies to recorded sample banks. Synths, wavetables, live input, and `gm_*` soundfont zones ignore this control.",
            params: &[
                ReferenceParam {
                    name: "amount",
                    r#type: "number | Pattern",
                    description: "between 0 and 1, where 1 is the length of the sample",
                },
            ],
            examples: &[
                "samples({ rave: 'rave/AREUREADY.wav' }, 'github:tidalcycles/dirt-samples')\ns(\"rave\").begin(\"<0 .25 .5 .75>\").fast(2)",
            ],
            tags: &["samples"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["end"],
        aliases: &[],
        reference: ReferenceEntry {
            name: "end",
            synonyms: &[],
            summary: "End position within a recorded sample, from 0 to 1.",
            description: "Defaults to 1. For example, end(0.5) selects the first half. The slice must satisfy 0 <= begin < end <= 1; begin defaults to 0. Negative speed reverses the buffer before these positions are applied. Applies to recorded sample banks. Synths, wavetables, live input, and `gm_*` soundfont zones ignore this control.",
            params: &[
                ReferenceParam {
                    name: "length",
                    r#type: "number | Pattern",
                    description: "1 = whole sample, .5 = half sample, .25 = quarter sample etc..",
                },
            ],
            examples: &[
                "s(\"bd*2,oh*4\").end(\"<.1 .2 .5 1>\").fast(2)",
            ],
            tags: &["samples"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["loop"],
        aliases: &[],
        reference: ReferenceEntry {
            name: "loop",
            synonyms: &[],
            summary: "Repeat a sample region for the event's duration.",
            description: "Absent by default. A nonzero value loops between loopBegin (default 0) and loopEnd (default 1), as fractions of the whole buffer. Setting loop(0) disables an explicit loop but still gates playback by the event's duration instead of the slice's natural duration. The loop does not sync playback speed to tempo. These controls operate on sample playback, not synths, wavetables, or live input. A `gm_*` zone with its own loop keeps that region; explicit loop points apply only when the zone has no built-in loop.",
            params: &[
                ReferenceParam {
                    name: "on",
                    r#type: "number | Pattern",
                    description: "If 1, the sample is looped",
                },
            ],
            examples: &[
                "s(\"casio\").loop(1)",
            ],
            tags: &["samples"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["loopBegin"],
        aliases: &["loopb"],
        reference: ReferenceEntry {
            name: "loopBegin",
            synonyms: &["loopb"],
            summary: "Sample loop start as a fraction of the whole buffer.",
            description: "Defaults to 0. Use with loop(1), with 0 <= loopBegin < loopEnd <= 1. The point is before loopEnd; begin still chooses where playback starts. Naming this point alone does not turn looping on. These controls operate on sample playback, not synths, wavetables, or live input. A `gm_*` zone with its own loop keeps that region; explicit loop points apply only when the zone has no built-in loop.",
            params: &[
                ReferenceParam {
                    name: "time",
                    r#type: "number | Pattern",
                    description: "between 0 and 1, where 1 is the length of the sample",
                },
            ],
            examples: &[
                "s(\"space\").loop(1)\n.loopBegin(\"<0 .125 .25>\")._scope()",
            ],
            tags: &["samples"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["loopEnd"],
        aliases: &["loope"],
        reference: ReferenceEntry {
            name: "loopEnd",
            synonyms: &["loope"],
            summary: "Sample loop end as a fraction of the whole buffer.",
            description: "Defaults to 1. Use with loop(1), with 0 <= loopBegin < loopEnd <= 1. The point is after loopBegin; begin still chooses where playback starts. Naming this point alone does not turn looping on. These controls operate on sample playback, not synths, wavetables, or live input. A `gm_*` zone with its own loop keeps that region; explicit loop points apply only when the zone has no built-in loop.",
            params: &[
                ReferenceParam {
                    name: "time",
                    r#type: "number | Pattern",
                    description: "between 0 and 1, where 1 is the length of the sample",
                },
            ],
            examples: &[
                "s(\"space\").loop(1)\n.loopEnd(\"<1 .75 .5 .25>\")._scope()",
            ],
            tags: &["samples"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["crush"],
        aliases: &[],
        reference: ReferenceEntry {
            name: "crush",
            synonyms: &[],
            summary: "Bit crusher effect.",
            description: "Bit crusher effect.",
            params: &[
                ReferenceParam {
                    name: "depth",
                    r#type: "number | Pattern",
                    description: "between 1 (for drastic reduction in bit-depth) to 16 (for barely no reduction).",
                },
            ],
            examples: &[
                "s(\"<bd sd>,hh*3\").fast(2).crush(\"<16 8 7 6 5 4 3 2>\")",
            ],
            tags: &["audio"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["coarse"],
        aliases: &[],
        reference: ReferenceEntry {
            name: "coarse",
            synonyms: &[],
            summary: "Lowers the effective sample rate with sample-and-hold.",
            description: "Holds each channel's last sampled value between updates. A factor of 1 leaves the sample rate unchanged; larger values reduce the update rate.",
            params: &[
                ReferenceParam {
                    name: "factor",
                    r#type: "number | Pattern",
                    description: "Hold factor; values below 1 use 1.",
                },
            ],
            examples: &[
                "s(\"bd sd [~ bd] sd,hh*8\").coarse(\"<1 4 8 16 32>\")",
            ],
            tags: &["audio"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["tremolo", "tremolodepth", "tremoloskew", "tremolophase"],
        aliases: &["trem"],
        reference: ReferenceEntry {
            name: "tremolo",
            synonyms: &["trem"],
            summary: "Amplitude-modulation rate in hertz.",
            description: "Absent by default; tremolo or tremolosync must be set to create the effect. tremolosync overrides tremolo and multiplies cycles per second. Depth defaults to 1, phase to 0, and the default waveform is a ramp made from triangle with skew 1. Naming a shape changes the default skew to 0.5. Applies to any native audio source, including oscillators, wavetables, samples, `gm_*` soundfonts, and live input. On ZZFX, tremolo also sets the generator's own tremolo unless a raw zzfx array overrides that parameter; the shared effect still applies.",
            params: &[
                ReferenceParam {
                    name: "speed",
                    r#type: "number | Pattern",
                    description: "modulation speed in HZ",
                },
            ],
            examples: &[
                "note(\"d d d# d\".fast(4)).s(\"supersaw\").tremolo(\"<3 2 100> \").tremoloskew(\"<.5>\")",
            ],
            tags: &["amplitude", "lfo", "audio"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["tremolosync", "tremolodepth", "tremoloskew", "tremolophase"],
        aliases: &["tremsync"],
        reference: ReferenceEntry {
            name: "tremolosync",
            synonyms: &["tremsync"],
            summary: "Amplitude-modulation oscillations per cycle.",
            description: "Absent by default. Multiplies cycles per second to obtain hertz and overrides tremolo when both are present. Naming it activates the shared tremolo; see tremolo for its other defaults.",
            params: &[
                ReferenceParam {
                    name: "cycles",
                    r#type: "number | Pattern",
                    description: "modulation speed in cycles",
                },
            ],
            examples: &[
                "note(\"d d d# d\".fast(4)).s(\"supersaw\").tremolosync(\"4\").tremoloskew(\"<1 .5 0>\")",
            ],
            tags: &["amplitude", "lfo", "audio"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["tremolodepth"],
        aliases: &["tremdepth"],
        reference: ReferenceEntry {
            name: "tremolodepth",
            synonyms: &["tremdepth"],
            summary: "Depth of an enabled tremolo, normally 0 to 1.",
            description: "Defaults to 1. Requires tremolo or tremolosync. Zero depth leaves amplitude unchanged; depth alone creates no tremolo.",
            params: &[
                ReferenceParam {
                    name: "depth",
                    r#type: "number | Pattern",
                    description: "",
                },
            ],
            examples: &[
                "note(\"a1 a1 a#1 a1\".fast(4)).s(\"pulse\").tremsync(4).tremolodepth(\"<1 2 .7>\")",
            ],
            tags: &["amplitude", "lfo", "audio"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["tremoloskew"],
        aliases: &["tremskew"],
        reference: ReferenceEntry {
            name: "tremoloskew",
            synonyms: &["tremskew"],
            summary: "Skew of an enabled tremolo waveform.",
            description: "A dimensionless shape/duty-cycle value, normally 0..1. Defaults to 1 when no shape is named, or 0.5 when tremoloshape is set. Requires tremolo or tremolosync; see tremoloshape.",
            params: &[
                ReferenceParam {
                    name: "amount",
                    r#type: "number | Pattern",
                    description: "between 0 & 1, the shape of the waveform",
                },
            ],
            examples: &[
                "note(\"{f a c e}%16\").s(\"sawtooth\").tremsync(4).tremoloskew(\"<.5 0 1>\")",
            ],
            tags: &["amplitude", "lfo", "audio"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["tremolophase"],
        aliases: &["tremphase"],
        reference: ReferenceEntry {
            name: "tremolophase",
            synonyms: &["tremphase"],
            summary: "Phase offset of an enabled tremolo, in turns.",
            description: "Defaults to 0; 1 is a full turn. The LFO follows the audio clock with this added phase. Requires tremolo or tremolosync; phase alone creates no tremolo.",
            params: &[
                ReferenceParam {
                    name: "offset",
                    r#type: "number | Pattern",
                    description: "the offset in cycles of the modulation",
                },
            ],
            examples: &[
                "note(\"{f a c e}%16\").s(\"sawtooth\").tremsync(4).tremolophase(\"<0 .25 .66>\")",
            ],
            tags: &["amplitude", "lfo", "audio"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["tremoloshape"],
        aliases: &["tremshape"],
        reference: ReferenceEntry {
            name: "tremoloshape",
            synonyms: &["tremshape"],
            summary: "Waveform of an enabled tremolo.",
            description: "Requires tremolo or tremolosync. Accepts triangle/tri (0), sine (1), ramp (2), saw (3), and square (4); numeric values wrap through the five shapes. The default is triangle with skew 1, which produces a ramp. Naming a shape changes the default skew to 0.5; unknown names select triangle.",
            params: &[
                ReferenceParam {
                    name: "shape",
                    r#type: "number | string | Pattern",
                    description: "triangle/tri (0), sine (1), ramp (2), saw (3), or square (4)",
                },
            ],
            examples: &[
                "note(\"{f g c d}%16\").tremsync(4).tremoloshape(\"<sine tri square>\").s(\"sawtooth\")",
            ],
            tags: &["amplitude", "lfo", "audio"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["drive"],
        aliases: &[],
        reference: ReferenceEntry {
            name: "drive",
            synonyms: &[],
            summary: "Input drive for enabled ladder filters.",
            description: "Defaults to 0.69. Requires ftype(\"ladder\") and an enabled lpf, hpf, or bpf section. The DSP exponentiates this unitless amount and bounds the resulting multiplier to 0.1..2000. It has no effect with the default biquad models or without a filter. Applies to any native audio source, including oscillators, wavetables, samples, `gm_*` soundfonts, and live input.",
            params: &[
                ReferenceParam {
                    name: "amount",
                    r#type: "number | Pattern",
                    description: "",
                },
            ],
            examples: &[
                "note(\"{f g g c d a a#}%16\".sub(17)).s(\"supersaw\").lpenv(8).lpf(150).lpq(.8).ftype('ladder').drive(\"<.5 4>\")",
            ],
            tags: &["filter", "audio"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["duckorbit"],
        aliases: &["duck"],
        reference: ReferenceEntry {
            name: "duckorbit",
            synonyms: &["duck"],
            summary: "Modulate the amplitude of an orbit to create a \"sidechain\" like effect.",
            description: "Modulate the amplitude of an orbit to create a \"sidechain\" like effect.\n\nCan be applied to multiple orbits with the ':' mininotation, e.g. `duckorbit(\"2:3\")`\n\nAny native voice can trigger ducking. It requires audio already routed to the named target orbit to hear an effect; it is triggered by the event, not by measuring the trigger's loudness. There is no target by default. Target orbit numbers are integers 0..15; depth defaults to 1, onset to 0 seconds, and recovery (duckattack) to 0.1 seconds.",
            params: &[
                ReferenceParam {
                    name: "orbit",
                    r#type: "number | Pattern",
                    description: "target orbit",
                },
            ],
            examples: &[
                "$: n(run(16)).scale(\"c:minor:pentatonic\").s(\"sawtooth\").delay(.7).orbit(2)\n$: s(\"bd:4!4\").beat(\"0,4,8,11,14\",16).duckorbit(2).duckattack(0.2).duckdepth(1)",
                "$: n(run(16)).scale(\"c:minor:pentatonic\").s(\"sawtooth\").delay(.7).orbit(2)\n$: s(\"hh*16\").orbit(3)\n$: s(\"bd:4!4\").beat(\"0,4,8,11,14\",16).duckorbit(\"2:3\").duckattack(0.2).duckdepth(1)",
            ],
            tags: &["amplitude", "orbit", "audio"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["duckdepth"],
        aliases: &[],
        reference: ReferenceEntry {
            name: "duckdepth",
            synonyms: &[],
            summary: "The amount of ducking applied to target orbit",
            description: "The amount of ducking applied to target orbit\n\nCan vary across orbits with the ':' mininotation, e.g. `duckdepth(\"0.3:0.1\")`.\nNote: this requires first applying the effect to multiple orbits with e.g. `duckorbit(\"2:3\")`.\n\nDefaults to 1. Requires duckorbit; the depth alone does not install ducking. It is a unitless amount, normally 0..1, acting on the target orbit's combined signal rather than the trigger's source.",
            params: &[
                ReferenceParam {
                    name: "depth",
                    r#type: "number | Pattern",
                    description: "depth of modulation from 0 to 1",
                },
            ],
            examples: &[
                "stack( n(run(8)).scale(\"c:minor\").s(\"sawtooth\").delay(.7).orbit(2), s(\"bd:4!4\").beat(\"0,4,8,11,14\",16).duckorbit(2).duckattack(0.2).duckdepth(\"<1 .9 .6 0>\"))",
                "$: n(run(16)).scale(\"c:minor:pentatonic\").s(\"sawtooth\").delay(.7).orbit(2)\n$: s(\"hh*16\").orbit(3)\n$: s(\"bd:4!4\").beat(\"0,4,8,11,14\",16).duckorbit(\"2:3\").duckattack(0.2).duckdepth(\"1:0.5\")",
            ],
            tags: &["amplitude", "orbit", "audio"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["duckonset"],
        aliases: &["duckons"],
        reference: ReferenceEntry {
            name: "duckonset",
            synonyms: &["duckons"],
            summary: "The time required for the ducked signal(s) to reach their lowest volume.",
            description: "The time required for the ducked signal(s) to reach their lowest volume.\nCan be used to prevent clicking or for creative rhythmic effects.\n\nCan vary across orbits with the ':' mininotation, e.g. `duckonset(\"0:0.003\")`.\nNote: this requires first applying the effect to multiple orbits with e.g. `duckorbit(\"2:3\")`.\n\nDefaults to 0 seconds. Requires duckorbit; onset alone does not install ducking. This is the ramp into the attenuation, not the trigger voice's amplitude attack.",
            params: &[
                ReferenceParam {
                    name: "time",
                    r#type: "number | Pattern",
                    description: "The onset time in seconds",
                },
            ],
            examples: &[
                "// Clicks\nsound: freq(\"63.2388\").s(\"sine\").orbit(2).gain(4)\nduckerWithClick: s(\"bd*4\").duckorbit(2).duckattack(0.3).duckonset(0).postgain(0)",
                "// No clicks\nsound: freq(\"63.2388\").s(\"sine\").orbit(2).gain(4)\nduckerWithoutClick: s(\"bd*4\").duckorbit(2).duckattack(0.3).duckonset(0.01).postgain(0)",
                "// Rhythmic\nnoise: s(\"pink\").distort(\"2:1\").orbit(4) // used rhythmically with 0.3 onset below\nhhat: s(\"hh*16\").orbit(7)\nducker: s(\"bd*4\").bank(\"tr909\").duckorbit(\"4:7\").duckonset(\"0.3:0.003\").duckattack(0.25)",
            ],
            tags: &["amplitude", "envelope", "orbit", "audio"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["duckattack"],
        aliases: &["duckatt", "datt"],
        reference: ReferenceEntry {
            name: "duckattack",
            synonyms: &["duckatt", "datt"],
            summary: "The time required for the ducked signal(s) to return to their normal volume.",
            description: "The time required for the ducked signal(s) to return to their normal volume.\n\nCan vary across orbits with the ':' mininotation, e.g. `duckattack(\"0.4:0.1\")`.\nNote: this requires first applying the effect to multiple orbits with e.g. `duckorbit(\"2:3\")`.\n\nDefaults to 0.1 seconds. Requires duckorbit; attack alone does not install ducking. This is the recovery from attenuation, not the trigger voice's amplitude attack.",
            params: &[
                ReferenceParam {
                    name: "time",
                    r#type: "number | Pattern",
                    description: "The attack time in seconds",
                },
            ],
            examples: &[
                "sound: n(run(8)).scale(\"c:minor\").s(\"sawtooth\").delay(.7).orbit(2)\nducker: s(\"bd:4!4\").beat(\"0,4,8,11,14\",16).duckorbit(2).duckattack(\"<0.2 0 0.4>\").duckdepth(1)",
                "moreduck: n(run(8)).scale(\"c:minor\").s(\"sawtooth\").delay(.7).orbit(2)\nlessduck: s(\"hh*16\").orbit(5)\nducker: s(\"bd:4!4\").beat(\"0,4,8,11,14\",16).duckorbit(\"2:5\").duckattack(\"0.4:0.1\")",
            ],
            tags: &["amplitude", "envelope", "orbit", "audio"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["byteBeatExpression"],
        aliases: &["bbexpr", "bb"],
        reference: ReferenceEntry {
            name: "byteBeatExpression",
            synonyms: &["bbexpr", "bb"],
            summary: "Set the expression evaluated by s(\"bytebeat\").",
            description: "The expression reads the bytebeat counter t. It replaces the built-in expression selected by n; when absent, n defaults to 0. Unsupported expressions are refused during voice conversion. Only bytebeat consumes this control; other synths, wavetables, and samples including `gm_*` ignore it.",
            params: &[
                ReferenceParam {
                    name: "byteBeatExpression",
                    r#type: "number | Pattern",
                    description: "bitwise expression for creating bytebeat",
                },
            ],
            examples: &[
                "s(\"bytebeat\").bbexpr('t*(t>>15^t>>66)')",
            ],
            tags: &["audio"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["byteBeatStartTime"],
        aliases: &["bbst"],
        reference: ReferenceEntry {
            name: "byteBeatStartTime",
            synonyms: &["bbst"],
            summary: "Set the initial bytebeat counter offset.",
            description: "Absent by default: the counter follows the event's onset on the audio clock. An explicit value is floored to an integer, resets the local counter, and supplies its initial offset; 0 restarts the expression at its beginning for each note. Only s(\"bytebeat\") consumes this control; it is a counter offset, not seconds.",
            params: &[
                ReferenceParam {
                    name: "byteBeatStartTime",
                    r#type: "number | Pattern",
                    description: "in samples (t)",
                },
            ],
            examples: &[
                "note(\"c3!8\".add(\"{0 0 12 0 7 5 3}%8\")).s(\"bytebeat:5\").bbst(\"<3 1>\".mul(10000))._scope()",
            ],
            tags: &["audio"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["channels"],
        aliases: &["ch"],
        reference: ReferenceEntry {
            name: "channels",
            synonyms: &["ch"],
            summary: "Allows you to set the output channels on the interface",
            description: "Allows you to set the output channels on the interface",
            params: &[
                ReferenceParam {
                    name: "channels",
                    r#type: "number | Pattern",
                    description: "pattern the output channels",
                },
            ],
            examples: &[
                "note(\"e a d b g\").channels(\"3:4\")",
            ],
            tags: &["external_io", "audio"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["pw", "pwrate", "pwsweep"],
        aliases: &[],
        reference: ReferenceEntry {
            name: "pw",
            synonyms: &[],
            summary: "Width of the pulse synth",
            description: "Sets the width of s(\"pulse\") (default 0.5). It has no effect on square, other synths, or samples. The effective width, including any modulation, is clamped to -0.99..0.99. A colon-separated value can also set the width LFO's rate and sweep: pw(\"0.5:2:0.3\") is pw(0.5).pwrate(2).pwsweep(0.3). Without a rate or sweep, the width stays fixed.",
            params: &[
                ReferenceParam {
                    name: "pulsewidth",
                    r#type: "number | Pattern",
                    description: "width control, usually 0..1; default 0.5. Optional colon fields: LFO rate in Hz and total width sweep.",
                },
            ],
            examples: &[
                "note(\"{f a c e}%16\").s(\"pulse\").pw(\".8:1:.2\")",
                "n(run(8)).scale(\"D:pentatonic\").s(\"pulse\").pw(\"0 .75 .5 1\")",
            ],
            tags: &["audio"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["pwrate"],
        aliases: &["pwr"],
        reference: ReferenceEntry {
            name: "pwrate",
            synonyms: &["pwr"],
            summary: "Pulse synth width LFO rate in Hz",
            description: "Modulates the width of s(\"pulse\"); other synths and samples ignore it. Setting pwrate alone enables a width LFO with sweep 0.3. Setting only pwsweep uses rate 1 Hz. With neither control there is no width LFO, and pwsweep(0) disables it even when pwrate is set.",
            params: &[
                ReferenceParam {
                    name: "rate",
                    r#type: "number | Pattern",
                    description: "LFO cycles per second (Hz); default 1 when pwsweep is set alone.",
                },
            ],
            examples: &[
                "note(\"c3 e3 g3\").s(\"pulse\").pwrate(2)",
                "n(run(8)).scale(\"D:pentatonic\").s(\"pulse\").pw(\"0.5\").pwrate(\"<5 .1 25>\").pwsweep(\"<0.3 .8>\")",
            ],
            tags: &["audio", "lfo"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["pwsweep"],
        aliases: &["pws"],
        reference: ReferenceEntry {
            name: "pwsweep",
            synonyms: &["pws"],
            summary: "Pulse synth width LFO depth",
            description: "Sets the width LFO's total sweep around pw for s(\"pulse\"); other synths and samples ignore it. A sweep of 0.3 moves the width by up to 0.15 in either direction, before the effective width is clamped to -0.99..0.99. Setting pwsweep alone enables the LFO at 1 Hz. Setting only pwrate uses sweep 0.3; pwsweep(0) disables the LFO.",
            params: &[
                ReferenceParam {
                    name: "sweep",
                    r#type: "number | Pattern",
                    description: "total width excursion, usually 0..1; default 0.3 when pwrate is set alone; 0 disables modulation.",
                },
            ],
            examples: &[
                "note(\"c3 e3 g3\").s(\"pulse\").pwsweep(0.6)",
                "n(run(8)).scale(\"D:pentatonic\").s(\"pulse\").pw(\"0.5\").pwrate(\"<5 .1 25>\").pwsweep(\"<0.3 .8>\")",
            ],
            tags: &["audio", "lfo"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["phaserrate", "phaserdepth", "phasercenter", "phasersweep"],
        aliases: &["ph", "phaser"],
        reference: ReferenceEntry {
            name: "phaser",
            synonyms: &["ph", "phaserrate"],
            summary: "Enable the phaser with an LFO rate in hertz.",
            description: "phaser is an alias of phaserrate. Absent by default; naming a rate creates the effect when phaserdepth is positive, including a zero rate for a static sweep position. Depth defaults to 0.75, center to 1000 Hz, and sweep to 2000 cents. Secondary phaser controls alone do not activate it. Applies to any native audio source, including oscillators, wavetables, samples, `gm_*` soundfonts, and live input.",
            params: &[
                ReferenceParam {
                    name: "speed",
                    r#type: "number | Pattern",
                    description: "speed of modulation",
                },
            ],
            examples: &[
                "n(run(8)).scale(\"D:pentatonic\").s(\"sawtooth\").release(0.5)\n.phaser(\"<1 2 4 8>\")",
            ],
            tags: &["audio"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["phasersweep"],
        aliases: &["phs"],
        reference: ReferenceEntry {
            name: "phasersweep",
            synonyms: &["phs"],
            summary: "Phaser LFO sweep in cents.",
            description: "Defaults to 2000 cents. Requires phaser (phaserrate) and a positive phaserdepth; naming sweep alone does not activate the effect.",
            params: &[
                ReferenceParam {
                    name: "phasersweep",
                    r#type: "number | Pattern",
                    description: "most useful values are between 0 and 4000",
                },
            ],
            examples: &[
                "n(run(8)).scale(\"D:pentatonic\").s(\"sawtooth\").release(0.5)\n.phaser(2).phasersweep(\"<800 2000 4000>\")",
            ],
            tags: &["audio", "lfo"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["phasercenter"],
        aliases: &["phc"],
        reference: ReferenceEntry {
            name: "phasercenter",
            synonyms: &["phc"],
            summary: "Phaser center control in hertz.",
            description: "Defaults to 1000 Hz. Requires phaser (phaserrate) and a positive phaserdepth; naming center alone does not activate the effect.",
            params: &[
                ReferenceParam {
                    name: "centerfrequency",
                    r#type: "number | Pattern",
                    description: "in HZ",
                },
            ],
            examples: &[
                "n(run(8)).scale(\"D:pentatonic\").s(\"sawtooth\").release(0.5)\n.phaser(2).phasercenter(\"<800 2000 4000>\")",
            ],
            tags: &["audio"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["phaserdepth"],
        aliases: &["phd", "phasdp"],
        reference: ReferenceEntry {
            name: "phaserdepth",
            synonyms: &["phd", "phasdp"],
            summary: "Phaser notch width; requires phaser or phaserrate.",
            description: "Defaults to 0.75 when a phaser rate is present. Larger positive depths lower the notch Q and broaden the affected frequency range; this is not a wet/dry mix. Zero or negative depth disables the effect; depth alone does not activate it.",
            params: &[
                ReferenceParam {
                    name: "depth",
                    r#type: "number | Pattern",
                    description: "number between 0 and 1",
                },
            ],
            examples: &[
                "n(run(8)).scale(\"D:pentatonic\").s(\"sawtooth\").release(0.5)\n.phaser(2).phaserdepth(\"<0 .5 .75 1>\")",
            ],
            tags: &["audio", "superdirt"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["channel"],
        aliases: &[],
        reference: ReferenceEntry {
            name: "channel",
            synonyms: &[],
            summary: "SuperDirt (OSC): channel-based pan offset.",
            description: "SuperDirt via `.osc()`: offsets `pan` by `channel / numChannels`. Use `channels` for native output routing.",
            params: &[
                ReferenceParam {
                    name: "channel",
                    r#type: "number | Pattern",
                    description: "channel offset",
                },
            ],
            examples: &[],
            tags: &["superdirt", "osc", "audio"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["cut"],
        aliases: &[],
        reference: ReferenceEntry {
            name: "cut",
            synonyms: &[],
            summary: "Choke the previous recorded sample in the same numeric group.",
            description: "There is no group by default. A new sample with the same cut number fades out the most recently triggered voice in that group; different groups play independently. A sample nudge delays the choke along with its source. Applies to recorded sample banks. Synths, wavetables, live input, and `gm_*` soundfont zones ignore this control.",
            params: &[
                ReferenceParam {
                    name: "group",
                    r#type: "number | Pattern",
                    description: "cut group number",
                },
            ],
            examples: &[
                "s(\"[oh hh]*4\").cut(1)",
            ],
            tags: &["audio"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["cutoff", "resonance", "lpenv"],
        aliases: &["ctf", "lpf", "lp"],
        reference: ReferenceEntry {
            name: "lpf",
            synonyms: &["cutoff", "ctf", "lp"],
            summary: "Applies the cutoff frequency of the low-pass filter.",
            description: "Applies the cutoff frequency of the low-pass filter.\n\nWhen using mininotation, you can also optionally add the 'lpq' parameter, separated by ':'.\n\nThe filter is absent until lpf sets a frequency in hertz. Its resonance defaults to lpq(1). The corresponding lp envelope and LFO controls need this filter to exist; naming them alone does not filter the sound. Applies to any native audio source, including oscillators, wavetables, samples, `gm_*` soundfonts, and live input.",
            params: &[
                ReferenceParam {
                    name: "frequency",
                    r#type: "number | Pattern",
                    description: "audible between 0 and 20000",
                },
            ],
            examples: &[
                "s(\"bd sd [~ bd] sd,hh*6\").lpf(\"<4000 2000 1000 500 200 100>\")",
                "s(\"bd*16\").lpf(\"1000:0 1000:10 1000:20 1000:30\")",
            ],
            tags: &["filter", "audio"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["lpenv"],
        aliases: &["lpe"],
        reference: ReferenceEntry {
            name: "lpenv",
            synonyms: &["lpe"],
            summary: "Lowpass envelope range in octaves; requires lpf.",
            description: "Naming this or a lp ADSR field enables a cutoff envelope only when lpf is present. The amount defaults to 1 octave when an ADSR field enables it. Negative amounts reverse the range. With only lpenv, attack/decay/sustain/release are 0.005 s, 0.14 s, 0, and 0.1 s. Once any ADSR field is explicit, omitted attack/decay become 0.001 s and release 0.01 s; omitted sustain is 0.001 when decay is set, otherwise 1. fanchor places this range relative to the base cutoff. This affects the filter, not the source's amplitude or pitch. Applies to any native audio source, including oscillators, wavetables, samples, `gm_*` soundfonts, and live input.",
            params: &[
                ReferenceParam {
                    name: "modulation",
                    r#type: "number | Pattern",
                    description: "depth of the lowpass filter envelope between 0 and _n_",
                },
            ],
            examples: &[
                "note(\"c2 e2 f2 g2\")\n.sound('sawtooth')\n.lpf(300)\n.lpa(.5)\n.lpenv(\"<4 2 1 0 -1 -2 -4>/4\")",
            ],
            tags: &["filter", "envelope", "audio"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["hpenv"],
        aliases: &["hpe"],
        reference: ReferenceEntry {
            name: "hpenv",
            synonyms: &["hpe"],
            summary: "Highpass envelope range in octaves; requires hpf.",
            description: "Naming this or a hp ADSR field enables a cutoff envelope only when hpf is present. The amount defaults to 1 octave when an ADSR field enables it. Negative amounts reverse the range. With only hpenv, attack/decay/sustain/release are 0.005 s, 0.14 s, 0, and 0.1 s. Once any ADSR field is explicit, omitted attack/decay become 0.001 s and release 0.01 s; omitted sustain is 0.001 when decay is set, otherwise 1. fanchor places this range relative to the base cutoff. This affects the filter, not the source's amplitude or pitch. Applies to any native audio source, including oscillators, wavetables, samples, `gm_*` soundfonts, and live input.",
            params: &[
                ReferenceParam {
                    name: "modulation",
                    r#type: "number | Pattern",
                    description: "depth of the highpass filter envelope between 0 and _n_",
                },
            ],
            examples: &[
                "note(\"c2 e2 f2 g2\")\n.sound('sawtooth')\n.hpf(500)\n.hpa(.5)\n.hpenv(\"<4 2 1 0 -1 -2 -4>/4\")",
            ],
            tags: &["filter", "envelope", "audio"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["bpenv"],
        aliases: &["bpe"],
        reference: ReferenceEntry {
            name: "bpenv",
            synonyms: &["bpe"],
            summary: "Bandpass envelope range in octaves; requires bpf.",
            description: "Naming this or a bp ADSR field enables a cutoff envelope only when bpf is present. The amount defaults to 1 octave when an ADSR field enables it. Negative amounts reverse the range. With only bpenv, attack/decay/sustain/release are 0.005 s, 0.14 s, 0, and 0.1 s. Once any ADSR field is explicit, omitted attack/decay become 0.001 s and release 0.01 s; omitted sustain is 0.001 when decay is set, otherwise 1. fanchor places this range relative to the base cutoff. This affects the filter, not the source's amplitude or pitch. Applies to any native audio source, including oscillators, wavetables, samples, `gm_*` soundfonts, and live input.",
            params: &[
                ReferenceParam {
                    name: "modulation",
                    r#type: "number | Pattern",
                    description: "depth of the bandpass filter envelope between 0 and _n_",
                },
            ],
            examples: &[
                "note(\"c2 e2 f2 g2\")\n.sound('sawtooth')\n.bpf(500)\n.bpa(.5)\n.bpenv(\"<4 2 1 0 -1 -2 -4>/4\")",
            ],
            tags: &["filter", "envelope", "audio"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["lpattack"],
        aliases: &["lpa"],
        reference: ReferenceEntry {
            name: "lpattack",
            synonyms: &["lpa"],
            summary: "Sets the attack duration for the lowpass filter envelope.",
            description: "Sets the attack duration for the lowpass filter envelope.\n\nThis is an envelope time in seconds. Requires lpf; naming this field enables the filter envelope with lpenv default 1 octave. See lpenv for the all-absent and partial-ADSR defaults. It does not shape source amplitude.",
            params: &[
                ReferenceParam {
                    name: "attack",
                    r#type: "number | Pattern",
                    description: "time of the filter envelope",
                },
            ],
            examples: &[
                "note(\"c2 e2 f2 g2\")\n.sound('sawtooth')\n.lpf(300)\n.lpa(\"<.5 .25 .1 .01>/4\")\n.lpenv(4)",
            ],
            tags: &["filter", "envelope", "audio"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["hpattack"],
        aliases: &["hpa"],
        reference: ReferenceEntry {
            name: "hpattack",
            synonyms: &["hpa"],
            summary: "Sets the attack duration for the highpass filter envelope.",
            description: "Sets the attack duration for the highpass filter envelope.\n\nThis is an envelope time in seconds. Requires hpf; naming this field enables the filter envelope with hpenv default 1 octave. See hpenv for the all-absent and partial-ADSR defaults. It does not shape source amplitude.",
            params: &[
                ReferenceParam {
                    name: "attack",
                    r#type: "number | Pattern",
                    description: "time of the highpass filter envelope",
                },
            ],
            examples: &[
                "note(\"c2 e2 f2 g2\")\n.sound('sawtooth')\n.hpf(500)\n.hpa(\"<.5 .25 .1 .01>/4\")\n.hpenv(4)",
            ],
            tags: &["filter", "envelope", "audio"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["bpattack"],
        aliases: &["bpa"],
        reference: ReferenceEntry {
            name: "bpattack",
            synonyms: &["bpa"],
            summary: "Sets the attack duration for the bandpass filter envelope.",
            description: "Sets the attack duration for the bandpass filter envelope.\n\nThis is an envelope time in seconds. Requires bpf; naming this field enables the filter envelope with bpenv default 1 octave. See bpenv for the all-absent and partial-ADSR defaults. It does not shape source amplitude.",
            params: &[
                ReferenceParam {
                    name: "attack",
                    r#type: "number | Pattern",
                    description: "time of the bandpass filter envelope",
                },
            ],
            examples: &[
                "note(\"c2 e2 f2 g2\")\n.sound('sawtooth')\n.bpf(500)\n.bpa(\"<.5 .25 .1 .01>/4\")\n.bpenv(4)",
            ],
            tags: &["filter", "envelope", "audio"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["lpdecay"],
        aliases: &["lpd"],
        reference: ReferenceEntry {
            name: "lpdecay",
            synonyms: &["lpd"],
            summary: "Sets the decay duration for the lowpass filter envelope.",
            description: "Sets the decay duration for the lowpass filter envelope.\n\nThis is an envelope time in seconds. Requires lpf; naming this field enables the filter envelope with lpenv default 1 octave. See lpenv for the all-absent and partial-ADSR defaults. It does not shape source amplitude.",
            params: &[
                ReferenceParam {
                    name: "decay",
                    r#type: "number | Pattern",
                    description: "time of the filter envelope",
                },
            ],
            examples: &[
                "note(\"c2 e2 f2 g2\")\n.sound('sawtooth')\n.lpf(300)\n.lpd(\"<.5 .25 .1 0>/4\")\n.lpenv(4)",
            ],
            tags: &["filter", "envelope", "audio"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["hpdecay"],
        aliases: &["hpd"],
        reference: ReferenceEntry {
            name: "hpdecay",
            synonyms: &["hpd"],
            summary: "Sets the decay duration for the highpass filter envelope.",
            description: "Sets the decay duration for the highpass filter envelope.\n\nThis is an envelope time in seconds. Requires hpf; naming this field enables the filter envelope with hpenv default 1 octave. See hpenv for the all-absent and partial-ADSR defaults. It does not shape source amplitude.",
            params: &[
                ReferenceParam {
                    name: "decay",
                    r#type: "number | Pattern",
                    description: "time of the highpass filter envelope",
                },
            ],
            examples: &[
                "note(\"c2 e2 f2 g2\")\n.sound('sawtooth')\n.hpf(500)\n.hpd(\"<.5 .25 .1 0>/4\")\n.hps(0.2)\n.hpenv(4)",
            ],
            tags: &["filter", "envelope", "audio"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["bpdecay"],
        aliases: &["bpd"],
        reference: ReferenceEntry {
            name: "bpdecay",
            synonyms: &["bpd"],
            summary: "Sets the decay duration for the bandpass filter envelope.",
            description: "Sets the decay duration for the bandpass filter envelope.\n\nThis is an envelope time in seconds. Requires bpf; naming this field enables the filter envelope with bpenv default 1 octave. See bpenv for the all-absent and partial-ADSR defaults. It does not shape source amplitude.",
            params: &[
                ReferenceParam {
                    name: "decay",
                    r#type: "number | Pattern",
                    description: "time of the bandpass filter envelope",
                },
            ],
            examples: &[
                "note(\"c2 e2 f2 g2\")\n.sound('sawtooth')\n.bpf(500)\n.bpd(\"<.5 .25 .1 0>/4\")\n.bps(0.2)\n.bpenv(4)",
            ],
            tags: &["filter", "envelope", "audio"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["lpsustain"],
        aliases: &["lps"],
        reference: ReferenceEntry {
            name: "lpsustain",
            synonyms: &["lps"],
            summary: "Sets the sustain amplitude for the lowpass filter envelope.",
            description: "Sets the sustain amplitude for the lowpass filter envelope.\n\nThis is an envelope level, capped at 1. Requires lpf; naming this field enables the filter envelope with lpenv default 1 octave. See lpenv for the all-absent and partial-ADSR defaults. It does not shape source amplitude.",
            params: &[
                ReferenceParam {
                    name: "sustain",
                    r#type: "number | Pattern",
                    description: "amplitude of the lowpass filter envelope",
                },
            ],
            examples: &[
                "note(\"c2 e2 f2 g2\")\n.sound('sawtooth')\n.lpf(300)\n.lpd(.5)\n.lps(\"<0 .25 .5 1>/4\")\n.lpenv(4)",
            ],
            tags: &["filter", "envelope", "audio"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["hpsustain"],
        aliases: &["hps"],
        reference: ReferenceEntry {
            name: "hpsustain",
            synonyms: &["hps"],
            summary: "Sets the sustain amplitude for the highpass filter envelope.",
            description: "Sets the sustain amplitude for the highpass filter envelope.\n\nThis is an envelope level, capped at 1. Requires hpf; naming this field enables the filter envelope with hpenv default 1 octave. See hpenv for the all-absent and partial-ADSR defaults. It does not shape source amplitude.",
            params: &[
                ReferenceParam {
                    name: "sustain",
                    r#type: "number | Pattern",
                    description: "amplitude of the highpass filter envelope",
                },
            ],
            examples: &[
                "note(\"c2 e2 f2 g2\")\n.sound('sawtooth')\n.hpf(500)\n.hpd(.5)\n.hps(\"<0 .25 .5 1>/4\")\n.hpenv(4)",
            ],
            tags: &["filter", "envelope", "audio"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["bpsustain"],
        aliases: &["bps"],
        reference: ReferenceEntry {
            name: "bpsustain",
            synonyms: &["bps"],
            summary: "Sets the sustain amplitude for the bandpass filter envelope.",
            description: "Sets the sustain amplitude for the bandpass filter envelope.\n\nThis is an envelope level, capped at 1. Requires bpf; naming this field enables the filter envelope with bpenv default 1 octave. See bpenv for the all-absent and partial-ADSR defaults. It does not shape source amplitude.",
            params: &[
                ReferenceParam {
                    name: "sustain",
                    r#type: "number | Pattern",
                    description: "amplitude of the bandpass filter envelope",
                },
            ],
            examples: &[
                "note(\"c2 e2 f2 g2\")\n.sound('sawtooth')\n.bpf(500)\n.bpd(.5)\n.bps(\"<0 .25 .5 1>/4\")\n.bpenv(4)",
            ],
            tags: &["filter", "envelope", "audio"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["lprelease"],
        aliases: &["lpr"],
        reference: ReferenceEntry {
            name: "lprelease",
            synonyms: &["lpr"],
            summary: "Sets the release time for the lowpass filter envelope.",
            description: "Sets the release time for the lowpass filter envelope.\n\nThis is an envelope time in seconds. Requires lpf; naming this field enables the filter envelope with lpenv default 1 octave. See lpenv for the all-absent and partial-ADSR defaults. It does not shape source amplitude.",
            params: &[
                ReferenceParam {
                    name: "release",
                    r#type: "number | Pattern",
                    description: "time of the filter envelope",
                },
            ],
            examples: &[
                "note(\"c2 e2 f2 g2\")\n.sound('sawtooth')\n.clip(.5)\n.lpf(300)\n.lpenv(4)\n.lpr(\"<.5 .25 .1 0>/4\")\n.release(.5)",
            ],
            tags: &["filter", "envelope", "audio"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["hprelease"],
        aliases: &["hpr"],
        reference: ReferenceEntry {
            name: "hprelease",
            synonyms: &["hpr"],
            summary: "Sets the release time for the highpass filter envelope.",
            description: "Sets the release time for the highpass filter envelope.\n\nThis is an envelope time in seconds. Requires hpf; naming this field enables the filter envelope with hpenv default 1 octave. See hpenv for the all-absent and partial-ADSR defaults. It does not shape source amplitude.",
            params: &[
                ReferenceParam {
                    name: "release",
                    r#type: "number | Pattern",
                    description: "time of the highpass filter envelope",
                },
            ],
            examples: &[
                "note(\"c2 e2 f2 g2\")\n.sound('sawtooth')\n.clip(.5)\n.hpf(500)\n.hpenv(4)\n.hpr(\"<.5 .25 .1 0>/4\")\n.release(.5)",
            ],
            tags: &["filter", "envelope", "audio"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["bprelease"],
        aliases: &["bpr"],
        reference: ReferenceEntry {
            name: "bprelease",
            synonyms: &["bpr"],
            summary: "Sets the release time for the bandpass filter envelope.",
            description: "Sets the release time for the bandpass filter envelope.\n\nThis is an envelope time in seconds. Requires bpf; naming this field enables the filter envelope with bpenv default 1 octave. See bpenv for the all-absent and partial-ADSR defaults. It does not shape source amplitude.",
            params: &[
                ReferenceParam {
                    name: "release",
                    r#type: "number | Pattern",
                    description: "time of the bandpass filter envelope",
                },
            ],
            examples: &[
                "note(\"c2 e2 f2 g2\")\n.sound('sawtooth')\n.clip(.5)\n.bpf(500)\n.bpenv(4)\n.bpr(\"<.5 .25 .1 0>/4\")\n.release(.5)",
            ],
            tags: &["filter", "envelope", "audio"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["ftype"],
        aliases: &[],
        reference: ReferenceEntry {
            name: "ftype",
            synonyms: &[],
            summary: "Choose the model of each enabled filter section.",
            description: "Defaults to 12db, one biquad stage. 24db uses two stages; the string ladder replaces each enabled section with a nonlinear four-pole ladder. Numeric values wrap through 0/1 (one biquad) and 2 (two biquads); numbers do not select the ladder. Requires lpf, hpf, or bpf; a model alone creates no filter. drive affects only the ladder. Applies to any native audio source, including oscillators, wavetables, samples, `gm_*` soundfonts, and live input.",
            params: &[
                ReferenceParam {
                    name: "type",
                    r#type: "number | string | Pattern",
                    description: "12db, ladder, or 24db; numbers are also accepted",
                },
            ],
            examples: &[
                "note(\"{f g g c d a a#}%8\").s(\"sawtooth\").lpenv(4).lpf(500).ftype(\"<0 1 2>\").lpq(1)",
                "note(\"c f g g a c d4\").fast(2)\n.sound('sawtooth')\n.lpf(200).fanchor(0)\n.lpenv(3).lpq(1)\n.ftype(\"<ladder 12db 24db>\")",
            ],
            tags: &["filter", "audio"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["fanchor"],
        aliases: &[],
        reference: ReferenceEntry {
            name: "fanchor",
            synonyms: &[],
            summary: "Place an enabled filter-envelope range around its base cutoff.",
            description: "Defaults to 0: the range extends upward from the cutoff; 0.5 centers it and 1 extends it downward. lpenv/hpenv/bpenv set the span in octaves, with negative amounts reversing its direction. Requires the corresponding lpf/hpf/bpf plus an envelope amount or ADSR field. fanchor alone creates neither a filter nor an envelope.",
            params: &[
                ReferenceParam {
                    name: "center",
                    r#type: "number | Pattern",
                    description: "0 to 1",
                },
            ],
            examples: &[
                "note(\"{f g g c d a a#}%8\").s(\"sawtooth\").lpf(\"{1000}%2\")\n.lpenv(8).fanchor(\"<0 .5 1>\")",
            ],
            tags: &["filter", "envelope", "audio"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["lprate"],
        aliases: &[],
        reference: ReferenceEntry {
            name: "lprate",
            synonyms: &[],
            summary: "Rate of the LFO for the lowpass filter",
            description: "Rate of the LFO for the lowpass filter\n\nRate in hertz. If neither rate nor sync is set, an enabled LFO runs once per musical cycle. lpsync overrides lprate. Requires lpf. Naming rate, sync, depth, depthfrequency, shape, or skew in this filter's LFO family enables it; lpdc alone does not. This LFO's offset is bounded so adding it to the base cutoff stays within 30..20000 Hz.",
            params: &[
                ReferenceParam {
                    name: "rate",
                    r#type: "number | Pattern",
                    description: "rate in hertz",
                },
            ],
            examples: &[
                "note(\"<c c c# c c c4>*16\").s(\"sawtooth\").lpf(600).lprate(\"<4 8 2 1>\")",
            ],
            tags: &["filter", "lfo", "audio"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["lpsync"],
        aliases: &[],
        reference: ReferenceEntry {
            name: "lpsync",
            synonyms: &[],
            summary: "Cycle-synced rate of the LFO for the lowpass filter",
            description: "Cycle-synced rate of the LFO for the lowpass filter\n\nOscillations per musical cycle. Multiplies cycles per second to obtain hertz and overrides lprate; it is absent by default. Requires lpf. Naming rate, sync, depth, depthfrequency, shape, or skew in this filter's LFO family enables it; lpdc alone does not. This LFO's offset is bounded so adding it to the base cutoff stays within 30..20000 Hz.",
            params: &[
                ReferenceParam {
                    name: "rate",
                    r#type: "number | Pattern",
                    description: "rate in cycles",
                },
            ],
            examples: &[
                "note(\"<c c c# c c c4>*16\").s(\"sawtooth\").lpf(600).lpsync(\"<4 8 2 1>\")",
            ],
            tags: &["filter", "lfo", "audio"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["lpdepth"],
        aliases: &[],
        reference: ReferenceEntry {
            name: "lpdepth",
            synonyms: &[],
            summary: "Depth of the LFO for the lowpass filter",
            description: "Depth of the LFO for the lowpass filter\n\nA unitless multiplier of the base lpf frequency, default 1. lpdepthfrequency overrides it when explicit. With the default DC offset -0.5, the triangle swings above and below the base cutoff. Requires lpf. Naming rate, sync, depth, depthfrequency, shape, or skew in this filter's LFO family enables it; lpdc alone does not. This LFO's offset is bounded so adding it to the base cutoff stays within 30..20000 Hz.",
            params: &[
                ReferenceParam {
                    name: "depth",
                    r#type: "number | Pattern",
                    description: "depth of modulation",
                },
            ],
            examples: &[
                "note(\"<c c c# c c c4>*16\").s(\"sawtooth\").lpf(600).lpdepth(\"<1 .5 1.8 0>\")",
            ],
            tags: &["filter", "lfo", "audio"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["lpdepthfrequency"],
        aliases: &["lpdepthfreq"],
        reference: ReferenceEntry {
            name: "lpdepthfrequency",
            synonyms: &["lpdepthfreq"],
            summary: "Depth of the LFO for the lowpass filter, in HZ",
            description: "Depth of the LFO for the lowpass filter, in HZ\n\nDepth in hertz, overriding lpdepth. Absent by default; the default depth is the base lpf frequency. Requires lpf. Naming rate, sync, depth, depthfrequency, shape, or skew in this filter's LFO family enables it; lpdc alone does not. This LFO's offset is bounded so adding it to the base cutoff stays within 30..20000 Hz.",
            params: &[
                ReferenceParam {
                    name: "depth",
                    r#type: "number | Pattern",
                    description: "depth of modulation",
                },
            ],
            examples: &[
                "note(\"<c c c# c c c4>*16\").s(\"sawtooth\").lpf(600).lpdepthfrequency(\"<200 500 100 0>\")",
            ],
            tags: &["filter", "lfo", "audio"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["lpshape"],
        aliases: &[],
        reference: ReferenceEntry {
            name: "lpshape",
            synonyms: &[],
            summary: "Shape of the LFO for the lowpass filter",
            description: "Shape of the LFO for the lowpass filter\n\nAccepts triangle/tri (0), sine (1), ramp (2), saw (3), or square (4), default triangle. Numeric values wrap over these shapes. The default skew is 0.5. Requires lpf. Naming rate, sync, depth, depthfrequency, shape, or skew in this filter's LFO family enables it; lpdc alone does not. This LFO's offset is bounded so adding it to the base cutoff stays within 30..20000 Hz.",
            params: &[
                ReferenceParam {
                    name: "shape",
                    r#type: "number | string | Pattern",
                    description: "triangle/tri (0), sine (1), ramp (2), saw (3), or square (4)",
                },
            ],
            examples: &[],
            tags: &["filter", "lfo", "audio"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["lpdc"],
        aliases: &[],
        reference: ReferenceEntry {
            name: "lpdc",
            synonyms: &[],
            summary: "DC offset of the LFO for the lowpass filter",
            description: "DC offset of the LFO for the lowpass filter\n\nDimensionless waveform offset before depth scaling, default -0.5. Use lprate or another activating LFO field before changing this offset. Requires lpf. Naming rate, sync, depth, depthfrequency, shape, or skew in this filter's LFO family enables it; lpdc alone does not. This LFO's offset is bounded so adding it to the base cutoff stays within 30..20000 Hz.",
            params: &[
                ReferenceParam {
                    name: "dcoffset",
                    r#type: "number | Pattern",
                    description: "dc offset. set to 0 for unipolar",
                },
            ],
            examples: &[],
            tags: &["filter", "lfo", "audio"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["lpskew"],
        aliases: &[],
        reference: ReferenceEntry {
            name: "lpskew",
            synonyms: &[],
            summary: "Skew of the LFO for the lowpass filter",
            description: "Skew of the LFO for the lowpass filter\n\nDimensionless waveform shape/duty-cycle value, normally 0..1, default 0.5. Requires lpf. Naming rate, sync, depth, depthfrequency, shape, or skew in this filter's LFO family enables it; lpdc alone does not. This LFO's offset is bounded so adding it to the base cutoff stays within 30..20000 Hz.",
            params: &[
                ReferenceParam {
                    name: "skew",
                    r#type: "number | Pattern",
                    description: "How much to bend the LFO shape",
                },
            ],
            examples: &[],
            tags: &["filter", "lfo", "audio"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["bprate"],
        aliases: &[],
        reference: ReferenceEntry {
            name: "bprate",
            synonyms: &[],
            summary: "Rate of the LFO for the bandpass filter",
            description: "Rate of the LFO for the bandpass filter\n\nRate in hertz. If neither rate nor sync is set, an enabled LFO runs once per musical cycle. bpsync overrides bprate. Requires bpf. Naming rate, sync, depth, depthfrequency, shape, or skew in this filter's LFO family enables it; bpdc alone does not. This LFO's offset is bounded so adding it to the base cutoff stays within 30..20000 Hz.",
            params: &[
                ReferenceParam {
                    name: "rate",
                    r#type: "number | Pattern",
                    description: "rate in hertz",
                },
            ],
            examples: &[],
            tags: &["filter", "lfo", "audio"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["bpsync"],
        aliases: &[],
        reference: ReferenceEntry {
            name: "bpsync",
            synonyms: &[],
            summary: "Cycle-synced rate of the LFO for the bandpass filter",
            description: "Cycle-synced rate of the LFO for the bandpass filter\n\nOscillations per musical cycle. Multiplies cycles per second to obtain hertz and overrides bprate; it is absent by default. Requires bpf. Naming rate, sync, depth, depthfrequency, shape, or skew in this filter's LFO family enables it; bpdc alone does not. This LFO's offset is bounded so adding it to the base cutoff stays within 30..20000 Hz.",
            params: &[
                ReferenceParam {
                    name: "rate",
                    r#type: "number | Pattern",
                    description: "rate in cycles",
                },
            ],
            examples: &[],
            tags: &["filter", "lfo", "audio"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["bpdepth"],
        aliases: &[],
        reference: ReferenceEntry {
            name: "bpdepth",
            synonyms: &[],
            summary: "Depth of the LFO for the bandpass filter",
            description: "Depth of the LFO for the bandpass filter\n\nA unitless multiplier of the base bpf frequency, default 1. bpdepthfrequency overrides it when explicit. With the default DC offset -0.5, the triangle swings above and below the base cutoff. Requires bpf. Naming rate, sync, depth, depthfrequency, shape, or skew in this filter's LFO family enables it; bpdc alone does not. This LFO's offset is bounded so adding it to the base cutoff stays within 30..20000 Hz.",
            params: &[
                ReferenceParam {
                    name: "depth",
                    r#type: "number | Pattern",
                    description: "depth of modulation",
                },
            ],
            examples: &[],
            tags: &["filter", "lfo", "audio"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["bpdepthfrequency"],
        aliases: &["bpdepthfreq"],
        reference: ReferenceEntry {
            name: "bpdepthfrequency",
            synonyms: &["bpdepthfreq"],
            summary: "Depth of the LFO for the bandpass filter, in HZ",
            description: "Depth of the LFO for the bandpass filter, in HZ\n\nDepth in hertz, overriding bpdepth. Absent by default; the default depth is the base bpf frequency. Requires bpf. Naming rate, sync, depth, depthfrequency, shape, or skew in this filter's LFO family enables it; bpdc alone does not. This LFO's offset is bounded so adding it to the base cutoff stays within 30..20000 Hz.",
            params: &[
                ReferenceParam {
                    name: "depth",
                    r#type: "number | Pattern",
                    description: "depth of modulation",
                },
            ],
            examples: &[
                "note(\"<c c c# c c c4>*16\").s(\"sawtooth\").lpf(600).bpdepthfrequency(\"<200 500 100 0>\")",
            ],
            tags: &["filter", "lfo", "audio"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["bpshape"],
        aliases: &[],
        reference: ReferenceEntry {
            name: "bpshape",
            synonyms: &[],
            summary: "Shape of the LFO for the bandpass filter",
            description: "Shape of the LFO for the bandpass filter\n\nAccepts triangle/tri (0), sine (1), ramp (2), saw (3), or square (4), default triangle. Numeric values wrap over these shapes. The default skew is 0.5. Requires bpf. Naming rate, sync, depth, depthfrequency, shape, or skew in this filter's LFO family enables it; bpdc alone does not. This LFO's offset is bounded so adding it to the base cutoff stays within 30..20000 Hz.",
            params: &[
                ReferenceParam {
                    name: "shape",
                    r#type: "number | string | Pattern",
                    description: "triangle/tri (0), sine (1), ramp (2), saw (3), or square (4)",
                },
            ],
            examples: &[],
            tags: &["filter", "lfo", "audio"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["bpdc"],
        aliases: &[],
        reference: ReferenceEntry {
            name: "bpdc",
            synonyms: &[],
            summary: "DC offset of the LFO for the bandpass filter",
            description: "DC offset of the LFO for the bandpass filter\n\nDimensionless waveform offset before depth scaling, default -0.5. Use bprate or another activating LFO field before changing this offset. Requires bpf. Naming rate, sync, depth, depthfrequency, shape, or skew in this filter's LFO family enables it; bpdc alone does not. This LFO's offset is bounded so adding it to the base cutoff stays within 30..20000 Hz.",
            params: &[
                ReferenceParam {
                    name: "dcoffset",
                    r#type: "number | Pattern",
                    description: "dc offset. set to 0 for unipolar",
                },
            ],
            examples: &[],
            tags: &["filter", "lfo", "audio"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["bpskew"],
        aliases: &[],
        reference: ReferenceEntry {
            name: "bpskew",
            synonyms: &[],
            summary: "Skew of the LFO for the bandpass filter",
            description: "Skew of the LFO for the bandpass filter\n\nDimensionless waveform shape/duty-cycle value, normally 0..1, default 0.5. Requires bpf. Naming rate, sync, depth, depthfrequency, shape, or skew in this filter's LFO family enables it; bpdc alone does not. This LFO's offset is bounded so adding it to the base cutoff stays within 30..20000 Hz.",
            params: &[
                ReferenceParam {
                    name: "skew",
                    r#type: "number | Pattern",
                    description: "How much to bend the LFO shape",
                },
            ],
            examples: &[],
            tags: &["filter", "lfo", "audio"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["hprate"],
        aliases: &[],
        reference: ReferenceEntry {
            name: "hprate",
            synonyms: &[],
            summary: "Rate of the LFO for the highpass filter",
            description: "Rate of the LFO for the highpass filter\n\nRate in hertz. If neither rate nor sync is set, an enabled LFO runs once per musical cycle. hpsync overrides hprate. Requires hpf. Naming rate, sync, depth, depthfrequency, shape, or skew in this filter's LFO family enables it; hpdc alone does not. This LFO's offset is bounded so adding it to the base cutoff stays within 30..20000 Hz.",
            params: &[
                ReferenceParam {
                    name: "rate",
                    r#type: "number | Pattern",
                    description: "rate in hertz",
                },
            ],
            examples: &[],
            tags: &["filter", "lfo", "audio"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["hpsync"],
        aliases: &[],
        reference: ReferenceEntry {
            name: "hpsync",
            synonyms: &[],
            summary: "Cycle-synced rate of the LFO for the highpass filter",
            description: "Cycle-synced rate of the LFO for the highpass filter\n\nOscillations per musical cycle. Multiplies cycles per second to obtain hertz and overrides hprate; it is absent by default. Requires hpf. Naming rate, sync, depth, depthfrequency, shape, or skew in this filter's LFO family enables it; hpdc alone does not. This LFO's offset is bounded so adding it to the base cutoff stays within 30..20000 Hz.",
            params: &[
                ReferenceParam {
                    name: "rate",
                    r#type: "number | Pattern",
                    description: "rate in cycles",
                },
            ],
            examples: &[],
            tags: &["filter", "lfo", "audio"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["hpdepth"],
        aliases: &[],
        reference: ReferenceEntry {
            name: "hpdepth",
            synonyms: &[],
            summary: "Depth of the LFO for the highpass filter",
            description: "Depth of the LFO for the highpass filter\n\nA unitless multiplier of the base hpf frequency, default 1. hpdepthfrequency overrides it when explicit. With the default DC offset -0.5, the triangle swings above and below the base cutoff. Requires hpf. Naming rate, sync, depth, depthfrequency, shape, or skew in this filter's LFO family enables it; hpdc alone does not. This LFO's offset is bounded so adding it to the base cutoff stays within 30..20000 Hz.",
            params: &[
                ReferenceParam {
                    name: "depth",
                    r#type: "number | Pattern",
                    description: "depth of modulation",
                },
            ],
            examples: &[],
            tags: &["filter", "lfo", "audio"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["hpdepthfrequency"],
        aliases: &["hpdepthfreq"],
        reference: ReferenceEntry {
            name: "hpdepthfrequency",
            synonyms: &["hpdepthfreq"],
            summary: "Depth of the LFO for the hipass filter, in hz",
            description: "Depth of the LFO for the hipass filter, in hz\n\nDepth in hertz, overriding hpdepth. Absent by default; the default depth is the base hpf frequency. Requires hpf. Naming rate, sync, depth, depthfrequency, shape, or skew in this filter's LFO family enables it; hpdc alone does not. This LFO's offset is bounded so adding it to the base cutoff stays within 30..20000 Hz.",
            params: &[
                ReferenceParam {
                    name: "depth",
                    r#type: "number | Pattern",
                    description: "depth of modulation",
                },
            ],
            examples: &[
                "note(\"<c c c# c c c4>*16\").s(\"sawtooth\").lpf(600).hpdepthfrequency(\"<200 500 100 0>\")",
            ],
            tags: &["filter", "lfo", "audio"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["hpshape"],
        aliases: &[],
        reference: ReferenceEntry {
            name: "hpshape",
            synonyms: &[],
            summary: "Shape of the LFO for the highpass filter",
            description: "Shape of the LFO for the highpass filter\n\nAccepts triangle/tri (0), sine (1), ramp (2), saw (3), or square (4), default triangle. Numeric values wrap over these shapes. The default skew is 0.5. Requires hpf. Naming rate, sync, depth, depthfrequency, shape, or skew in this filter's LFO family enables it; hpdc alone does not. This LFO's offset is bounded so adding it to the base cutoff stays within 30..20000 Hz.",
            params: &[
                ReferenceParam {
                    name: "shape",
                    r#type: "number | string | Pattern",
                    description: "triangle/tri (0), sine (1), ramp (2), saw (3), or square (4)",
                },
            ],
            examples: &[],
            tags: &["filter", "lfo", "audio"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["hpdc"],
        aliases: &[],
        reference: ReferenceEntry {
            name: "hpdc",
            synonyms: &[],
            summary: "DC offset of the LFO for the highpass filter",
            description: "DC offset of the LFO for the highpass filter\n\nDimensionless waveform offset before depth scaling, default -0.5. Use hprate or another activating LFO field before changing this offset. Requires hpf. Naming rate, sync, depth, depthfrequency, shape, or skew in this filter's LFO family enables it; hpdc alone does not. This LFO's offset is bounded so adding it to the base cutoff stays within 30..20000 Hz.",
            params: &[
                ReferenceParam {
                    name: "dcoffset",
                    r#type: "number | Pattern",
                    description: "dc offset. set to 0 for unipolar",
                },
            ],
            examples: &[],
            tags: &["filter", "lfo", "audio"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["hpskew"],
        aliases: &[],
        reference: ReferenceEntry {
            name: "hpskew",
            synonyms: &[],
            summary: "Skew of the LFO for the highpass filter",
            description: "Skew of the LFO for the highpass filter\n\nDimensionless waveform shape/duty-cycle value, normally 0..1, default 0.5. Requires hpf. Naming rate, sync, depth, depthfrequency, shape, or skew in this filter's LFO family enables it; hpdc alone does not. This LFO's offset is bounded so adding it to the base cutoff stays within 30..20000 Hz.",
            params: &[
                ReferenceParam {
                    name: "skew",
                    r#type: "number | Pattern",
                    description: "How much to bend the LFO shape",
                },
            ],
            examples: &[],
            tags: &["filter", "lfo", "audio"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["vib", "vibmod"],
        aliases: &["vibrato", "v"],
        reference: ReferenceEntry {
            name: "vib",
            synonyms: &["vibrato", "v"],
            summary: "Vibrato rate in hertz for pitch-controlled voices.",
            description: "Absent by default; a positive rate enables vibrato. The depth is vibmod, default 0.5 semitones. A zero or negative rate disables it. The sbd synth uses its own pitch sweep and ignores vibrato. Retunes basic oscillators, supersaw, pulse, wavetables, and samples including `gm_*` zones. Bytebeat, ZZFX, standalone noise, live input, and buses ignore it.",
            params: &[
                ReferenceParam {
                    name: "frequency",
                    r#type: "number | Pattern",
                    description: "of the vibrato in hertz",
                },
            ],
            examples: &[
                "note(\"a e\")\n.vib(\"<.5 1 2 4 8 16>\")\n._scope()",
                "// change the modulation depth with \":\"\nnote(\"a e\")\n.vib(\"<.5 1 2 4 8 16>:12\")\n._scope()",
            ],
            tags: &["pitch", "lfo", "audio"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["noise"],
        aliases: &[],
        reference: ReferenceEntry {
            name: "noise",
            synonyms: &[],
            summary: "Mix pink noise into the basic oscillator voices.",
            description: "Defaults to 0, disabled. Positive amounts mix pink noise before the amplitude envelope: 0..0.5 raises the noise alongside the oscillator, then 0.5..1 fades the oscillator out. Applies to sine, triangle, square, sawtooth, and user oscillators. Supersaw, pulse, bytebeat, wavetables, recorded samples (including `gm_*`), standalone noise, and ZZFX ignore it. Use znoise for the ZZFX generator.",
            params: &[
                ReferenceParam {
                    name: "wet",
                    r#type: "number | Pattern",
                    description: "wet amount",
                },
            ],
            examples: &[
                "sound(\"<white pink brown>/2\")",
            ],
            tags: &["generators", "audio"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["vibmod", "vib"],
        aliases: &["vmod"],
        reference: ReferenceEntry {
            name: "vibmod",
            synonyms: &["vmod"],
            summary: "Vibrato depth in semitones; requires a positive vib rate.",
            description: "Defaults to 0.5 semitones. Set vib (or vibrato/v) to a positive rate in hertz to hear this depth; vibmod alone does not enable vibrato. The sbd synth ignores it. Retunes basic oscillators, supersaw, pulse, wavetables, and samples including `gm_*` zones. Bytebeat, ZZFX, standalone noise, live input, and buses ignore it.",
            params: &[
                ReferenceParam {
                    name: "depth",
                    r#type: "number | Pattern",
                    description: "of vibrato (in semitones)",
                },
            ],
            examples: &[
                "note(\"a e\").vib(4)\n.vibmod(\"<.25 .5 1 2 12>\")\n._scope()",
                "// change the vibrato frequency with \":\"\nnote(\"a e\")\n.vibmod(\"<.25 .5 1 2 12>:8\")\n._scope()",
            ],
            tags: &["pitch", "lfo", "audio"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["hcutoff", "hresonance", "hpenv"],
        aliases: &["hpf", "hp"],
        reference: ReferenceEntry {
            name: "hpf",
            synonyms: &["hp", "hcutoff"],
            summary: "Applies the cutoff frequency of the high-pass filter.",
            description: "Applies the cutoff frequency of the high-pass filter.\n\nWhen using mininotation, you can also optionally add the 'hpq' parameter, separated by ':'.\n\nThe filter is absent until hpf sets a frequency in hertz. Its resonance defaults to hpq(1). The corresponding hp envelope and LFO controls need this filter to exist; naming them alone does not filter the sound. Applies to any native audio source, including oscillators, wavetables, samples, `gm_*` soundfonts, and live input.",
            params: &[
                ReferenceParam {
                    name: "frequency",
                    r#type: "number | Pattern",
                    description: "audible between 0 and 20000",
                },
            ],
            examples: &[
                "s(\"bd sd [~ bd] sd,hh*8\").hpf(\"<4000 2000 1000 500 200 100>\")",
                "s(\"bd sd [~ bd] sd,hh*8\").hpf(\"<2000 2000:25>\")",
            ],
            tags: &["filter", "audio"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["hresonance"],
        aliases: &["hpq"],
        reference: ReferenceEntry {
            name: "hpq",
            synonyms: &["hresonance"],
            summary: "Controls the high-pass q-value.",
            description: "Controls the high-pass q-value.\n\nDefaults to 1 and requires hpf; resonance alone creates no filter. It is a decibel resonance control for the biquad models. The ladder model uses its own resonance mapping; see ftype.",
            params: &[
                ReferenceParam {
                    name: "q",
                    r#type: "number | Pattern",
                    description: "resonance factor between 0 and 50",
                },
            ],
            examples: &[
                "s(\"bd sd [~ bd] sd,hh*8\").hpf(2000).hpq(\"<0 10 20 30>\")",
            ],
            tags: &["filter", "audio"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["resonance"],
        aliases: &["lpq"],
        reference: ReferenceEntry {
            name: "lpq",
            synonyms: &["resonance"],
            summary: "Controls the low-pass q-value.",
            description: "Controls the low-pass q-value.\n\nDefaults to 1 and requires lpf; resonance alone creates no filter. It is a decibel resonance control for the biquad models. The ladder model uses its own resonance mapping; see ftype.",
            params: &[
                ReferenceParam {
                    name: "q",
                    r#type: "number | Pattern",
                    description: "resonance factor between 0 and 50",
                },
            ],
            examples: &[
                "s(\"bd sd [~ bd] sd,hh*8\").lpf(2000).lpq(\"<0 10 20 30>\")",
            ],
            tags: &["filter", "audio"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["djf"],
        aliases: &[],
        reference: ReferenceEntry {
            name: "djf",
            synonyms: &[],
            summary: "DJ filter, below 0.5 is low pass filter, above is high pass filter.",
            description: "DJ filter, below 0.5 is low pass filter, above is high pass filter.",
            params: &[
                ReferenceParam {
                    name: "cutoff",
                    r#type: "number | Pattern",
                    description: "below 0.5 is low pass filter, above is high pass filter",
                },
            ],
            examples: &[
                "n(irand(16).seg(8)).scale(\"d:phrygian\").s(\"supersaw\").djf(\"<.5 .3 .2 .75>\")",
            ],
            tags: &["filter", "audio"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["delay", "delaytime", "delayfeedback"],
        aliases: &[],
        reference: ReferenceEntry {
            name: "delay",
            synonyms: &[],
            summary: "Send a voice to its orbit's delay.",
            description: "Absent by default. A positive delay amount, a positive delay time, and positive delayfeedback are all required; zero feedback disables the send rather than producing one echo. delaytime is in seconds; without it, delaysync defaults to 3/16 cycles and is divided by cycles per second. Time is capped at 1 second and feedback defaults to 0.5, clamped to 0..0.98. The compound form is delay(\"amount:seconds:feedback\"). Applies to any native audio source, including oscillators, wavetables, samples, `gm_*` soundfonts, and live input.",
            params: &[
                ReferenceParam {
                    name: "level",
                    r#type: "number | Pattern",
                    description: "between 0 and 1",
                },
            ],
            examples: &[
                "s(\"bd bd\").delay(\"<0 .25 .5 1>\")",
                "s(\"bd bd\").delay(\"0.65:0.25:0.9 0.65:0.125:0.7\")",
            ],
            tags: &["orbit", "audio"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["delayfeedback"],
        aliases: &["delayfb", "dfb"],
        reference: ReferenceEntry {
            name: "delayfeedback",
            synonyms: &["delayfb", "dfb"],
            summary: "Feedback fraction for an enabled delay send.",
            description: "Defaults to 0.5 and is clamped to 0..0.98. Requires a positive delay send and positive delay time. A value at or below 0 disables the send entirely. This is a unitless feedback fraction, not decibels.",
            params: &[
                ReferenceParam {
                    name: "feedback",
                    r#type: "number | Pattern",
                    description: "between 0 and 1",
                },
            ],
            examples: &[
                "s(\"bd\").delay(.25).delayfeedback(\"<.25 .5 .75 1>\")",
            ],
            tags: &["orbit", "audio"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["delayspeed"],
        aliases: &[],
        reference: ReferenceEntry {
            name: "delayspeed",
            synonyms: &["delayt", "dt"],
            summary: "Sets the time of the delay effect.",
            description: "Sets the time of the delay effect.",
            params: &[
                ReferenceParam {
                    name: "delayspeed",
                    r#type: "number | Pattern",
                    description: "controls the pitch of the delay feedback",
                },
            ],
            examples: &[
                "note(\"d d a# a\".fast(2)).s(\"sawtooth\").delay(.8).delaytime(1/2).delayspeed(\"<2 .5 -1 -2>\")",
            ],
            tags: &["orbit", "audio"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["delaytime"],
        aliases: &["delayt", "dt"],
        reference: ReferenceEntry {
            name: "delaytime",
            synonyms: &["delayt", "dt"],
            summary: "Delay time in seconds; requires a positive delay send.",
            description: "Overrides delaysync when explicit. Without it the default is 3/16 cycles divided by cycles per second. Positive times are capped at 1 second; a nonpositive time disables the send. delay and delayfeedback must also be positive.",
            params: &[
                ReferenceParam {
                    name: "delay",
                    r#type: "number | Pattern",
                    description: "in seconds",
                },
            ],
            examples: &[
                "note(\"d d a# a\".fast(2))\n.s(\"sawtooth\")\n.delay(.8)\n.delaytime(1/2)\n.delayspeed(\"<2 .5 -1 -2>\")",
            ],
            tags: &["orbit", "audio"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["delaysync"],
        aliases: &["delays", "ds"],
        reference: ReferenceEntry {
            name: "delaysync",
            synonyms: &["delays", "ds"],
            summary: "Delay time in cycles; requires a positive delay send.",
            description: "Defaults to 3/16 cycles. Dividing by cycles per second gives seconds, capped at 1 second. An explicit delaytime overrides it. delay, the resulting time, and delayfeedback must all be positive.",
            params: &[
                ReferenceParam {
                    name: "cycles",
                    r#type: "number | Pattern",
                    description: "delay length in cycles",
                },
            ],
            examples: &[
                "s(\"bd bd\").delay(.25).delaysync(\"<1 2 3 5>\".div(8))",
            ],
            tags: &["orbit", "audio"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["lock"],
        aliases: &[],
        reference: ReferenceEntry {
            name: "lock",
            synonyms: &[],
            summary: "SuperDirt (OSC): delay time in cycles.",
            description: "SuperDirt via `.osc()`: with `lock(1)`, `delaytime` is measured in cycles; otherwise it is measured in seconds.",
            params: &[
                ReferenceParam {
                    name: "enable",
                    r#type: "number | Pattern",
                    description: "1 for cycles, 0 for seconds",
                },
            ],
            examples: &[
                "s(\"sd\").delay().lock(1).osc()",
            ],
            tags: &["superdirt", "osc"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["detune"],
        aliases: &["det"],
        reference: ReferenceEntry {
            name: "detune",
            synonyms: &["det"],
            summary: "Spread unison voices in pitch, in semitones.",
            description: "Spread the stacked voices of `supersaw` or a loaded `wt_` wavetable across a pitch range in semitones. The default is 0.18 semitones for either source. Set `unison` to at least 2 to hear the pitch spread; with one voice, `detune` has no audible effect. Other sound families do not use this control. On `supersaw` only, `n` supplies the detune amount when `detune` is absent; on wavetables, `n` selects a table instead.",
            params: &[
                ReferenceParam {
                    name: "amount",
                    r#type: "number | Pattern",
                    description: "Pitch range in semitones across the unison voices; default 0.18. Negative amounts act like 0 in the native renderer.",
                },
            ],
            examples: &[
                "note(\"d f a a#\").s(\"supersaw\").unison(5).detune(\"<0 .5>\")",
                "s(\"basique\").bank(\"wt_digital\").seg(8).note(\"F1\").unison(5).detune(\"<0 .5>\")",
            ],
            tags: &["pitch", "audio"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["unison"],
        aliases: &[],
        reference: ReferenceEntry {
            name: "unison",
            synonyms: &[],
            summary: "Set the unison voice count for supersaw or wavetables.",
            description: "Set how many voices `supersaw` or a loaded `wt_` wavetable stacks. `supersaw` defaults to 5 voices; wavetables default to 1. The native engine renders 1 through 32 voices. Values below 1 act like 1; values above 32 refuse the event. At one voice, `detune` and `spread` have no audible effect. Other sound families do not use this control.",
            params: &[
                ReferenceParam {
                    name: "numvoices",
                    r#type: "number | Pattern",
                    description: "Voice count, 1-32; default 5 for supersaw or 1 for wavetables. Values below 1 act like 1. Fractional counts render the next whole number of voices, but pitch spread requires a value of at least 2.",
                },
            ],
            examples: &[
                "note(\"d f a a#\").s(\"supersaw\").unison(\"<1 5 7>\")",
                "s(\"basique\").bank(\"wt_digital\").seg(8).note(\"F1\").unison(\"<1 5>\")",
            ],
            tags: &["audio"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["spread"],
        aliases: &[],
        reference: ReferenceEntry {
            name: "spread",
            synonyms: &[],
            summary: "Spread unison voices across the stereo field.",
            description: "Pan the stacked voices of `supersaw` or a loaded `wt_` wavetable apart. 0 centres them; 1 gives the widest spread. `supersaw` defaults to 0.6 and wavetables to 0.7. Add `unison` with at least two voices to hear the stereo spread; with one voice it has no audible effect. Other sound families do not use this control.",
            params: &[
                ReferenceParam {
                    name: "spread",
                    r#type: "number | Pattern",
                    description: "Stereo width from 0 (centre) to 1 (widest); values outside this range are clamped in the native renderer.",
                },
            ],
            examples: &[
                "note(\"d f a a#\").s(\"supersaw\").unison(5).spread(\"<0 .5 1>\")",
                "s(\"basique\").bank(\"wt_digital\").seg(8).note(\"F1\").unison(5).spread(\"<0 1>\")",
            ],
            tags: &["audio"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["dry"],
        aliases: &[],
        reference: ReferenceEntry {
            name: "dry",
            synonyms: &[],
            summary: "Direct-output gain, independent of reverb and other sends.",
            description: "Defaults to 1. dry(0) removes the direct signal while keeping delay, reverb, and audio-bus sends active. It requires no room setting; with no other audible route, dry(0) is silent. This is a linear multiplier on the processed voice, not a wet/dry crossfade. These are effects on any native audio source, including oscillators, wavetables, samples, `gm_*` soundfonts, and live input.",
            params: &[
                ReferenceParam {
                    name: "dry",
                    r#type: "number | Pattern",
                    description: "0 = wet, 1 = dry",
                },
            ],
            examples: &[
                "n(\"[0,3,7](3,8)\").s(\"superpiano\").room(.7).dry(\"<0 .5 .75 1>\").osc()",
            ],
            tags: &["superdirt"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["fadeTime"],
        aliases: &["fadeOutTime"],
        reference: ReferenceEntry {
            name: "fadeTime",
            synonyms: &["fadeOutTime"],
            summary: "SuperDirt (OSC): sample fade time.",
            description: "SuperDirt via `.osc()`: sets the sample fade-out time in seconds. It also sets the fade-in when `begin` starts partway through a sample.",
            params: &[
                ReferenceParam {
                    name: "time",
                    r#type: "number | Pattern",
                    description: "time in seconds",
                },
            ],
            examples: &[
                "s(\"oh*4\").end(.1).fadeTime(\"<0 .2 .4 .8>\").osc()",
            ],
            tags: &["superdirt", "osc"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["fadeInTime"],
        aliases: &[],
        reference:         ReferenceEntry {
            name: "fadeInTime",
            synonyms: &[],
            summary: "Fade-in time; unsupported in native audio.",
            description: "Rustel uses attack for the start of a native note. fadeInTime is forwarded through osc() for external synthesis.",
            params: &[
                ReferenceParam {
                    name: "value",
                    r#type: "number | Pattern",
                    description: "fade-in time in seconds",
                },
            ],
            examples: &[],
            tags: &["control"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["freq"],
        aliases: &[],
        reference: ReferenceEntry {
            name: "freq",
            synonyms: &[],
            summary: "Set a pitched source's frequency in hertz.",
            description: "Overrides note on pitched oscillators, supersaw, pulse, bytebeat, wavetables, and sbd. For samples and `gm_*` soundfonts it supplies the target pitch for bank selection/transposition, not a time-stretch. ZZFX uses it unless a raw zzfx array supplies its parameters. It is absent by default: note supplies pitch, with C2 as the ordinary synth fallback, F1 for sbd, and C3 for `gm_*` zones. Synth freq(0) falls back to note; a sample requires a positive frequency. Standalone noise, live input, and buses do not change pitch with freq.",
            params: &[
                ReferenceParam {
                    name: "frequency",
                    r#type: "number | Pattern",
                    description: "in Hz. the audible range is between 20 and 20000 Hz",
                },
            ],
            examples: &[
                "freq(\"220 110 440 110\").s(\"superzow\").osc()",
                "freq(\"110\".mul.out(\".5 1.5 .6 [2 3]\")).s(\"superzow\").osc()",
            ],
            tags: &["pitch", "audio"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["pattack"],
        aliases: &["patt"],
        reference: ReferenceEntry {
            name: "pattack",
            synonyms: &["patt"],
            summary: "Pitch-envelope attack time in seconds.",
            description: "Setting this field enables the pitch envelope with penv default 1 semitone. An explicit value is floored at 0.001 s; penv alone supplies an attack of 0.2 s. See psustain for the remaining conditional defaults. The sbd sweep ignores pattack. Retunes basic oscillators, supersaw, pulse, wavetables, and samples including `gm_*` zones. Bytebeat, ZZFX, standalone noise, live input, and buses ignore it.",
            params: &[
                ReferenceParam {
                    name: "time",
                    r#type: "number | Pattern",
                    description: "time in seconds",
                },
            ],
            examples: &[
                "note(\"c eb g bb\").pattack(\"0 .1 .25 .5\").slow(2)",
            ],
            tags: &["pitch", "envelope", "audio"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["pdecay"],
        aliases: &["pdec"],
        reference: ReferenceEntry {
            name: "pdecay",
            synonyms: &["pdec"],
            summary: "Pitch-envelope decay time in seconds.",
            description: "Setting this field enables the pitch envelope with penv default 1 semitone. Values are floored at 0.001 s. Naming pdecay without psustain selects sustain 0.001. For sbd this is instead its dedicated pitch-decay time, default 0.5 s, with penv default 36 semitones. Retunes basic oscillators, supersaw, pulse, wavetables, and samples including `gm_*` zones. Bytebeat, ZZFX, standalone noise, live input, and buses ignore it.",
            params: &[
                ReferenceParam {
                    name: "time",
                    r#type: "number | Pattern",
                    description: "time in seconds",
                },
            ],
            examples: &[
                "note(\"<c eb g bb>\").pdecay(\"<0 .1 .25 .5>\")",
            ],
            tags: &["pitch", "envelope", "audio"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["psustain"],
        aliases: &["psus"],
        reference:         ReferenceEntry {
            name: "psustain",
            synonyms: &["psus"],
            summary: "Pitch-envelope sustain level, capped at 1.",
            description: "Setting this field enables the pitch envelope with penv default 1 semitone. With penv alone, attack/decay/sustain/release are 0.2 s, 0.001 s, 1, and 0.001 s. Once any pitch ADSR field is set, omitted attack/decay become 0.001 s and release 0.01 s; omitted sustain is 0.001 if pdecay is set, otherwise 1. panchor defaults to the resolved sustain level. The sbd sweep ignores psustain. Retunes basic oscillators, supersaw, pulse, wavetables, and samples including `gm_*` zones. Bytebeat, ZZFX, standalone noise, live input, and buses ignore it.",
            params: &[
                ReferenceParam {
                    name: "value",
                    r#type: "number | Pattern",
                    description: "sustain level 0..1; default 1",
                },
            ],
            examples: &[
                "note(\"c2\").s(\"sawtooth\").penv(2).pattack(0.1).psustain(0.5)",
            ],
            tags: &["control", "pitch"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["prelease"],
        aliases: &["prel"],
        reference: ReferenceEntry {
            name: "prelease",
            synonyms: &["prel"],
            summary: "Pitch-envelope release time in seconds.",
            description: "Setting this field enables the pitch envelope with penv default 1 semitone. An explicit value is floored at 0.01 s; penv alone supplies 0.001 s. See psustain for conditional defaults. The sbd sweep ignores prelease. Retunes basic oscillators, supersaw, pulse, wavetables, and samples including `gm_*` zones. Bytebeat, ZZFX, standalone noise, live input, and buses ignore it.",
            params: &[
                ReferenceParam {
                    name: "time",
                    r#type: "number | Pattern",
                    description: "time in seconds",
                },
            ],
            examples: &[
                "note(\"<c eb g bb> ~\")\n.release(.5) // to hear the pitch release\n.prelease(\"<0 .1 .25 .5>\")",
            ],
            tags: &["pitch", "envelope", "audio"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["penv"],
        aliases: &[],
        reference: ReferenceEntry {
            name: "penv",
            synonyms: &[],
            summary: "Pitch-envelope range in semitones.",
            description: "The envelope activates when penv or any pitch ADSR field is set; its amount defaults to 1 semitone. Negative amounts reverse the sweep. With penv alone the ADSR is attack 0.2 s, decay 0.001 s, sustain 1, release 0.001 s. See psustain for conditional defaults, and panchor for the range relative to the base note. The sbd synth instead uses its own decay sweep: penv defaults to 36 semitones and pdecay to 0.5 s. Retunes basic oscillators, supersaw, pulse, wavetables, and samples including `gm_*` zones. Bytebeat, ZZFX, standalone noise, live input, and buses ignore it.",
            params: &[
                ReferenceParam {
                    name: "semitones",
                    r#type: "number | Pattern",
                    description: "change in semitones",
                },
            ],
            examples: &[
                "note(\"c\")\n.penv(\"<12 7 1 .5 0 -1 -7 -12>\")",
            ],
            tags: &["pitch", "envelope", "audio"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["pcurve"],
        aliases: &[],
        reference: ReferenceEntry {
            name: "pcurve",
            synonyms: &[],
            summary: "Pitch-envelope curve: 0 linear or 1 exponential.",
            description: "Defaults to 0. Requires penv or a pitch ADSR field; pcurve alone does not activate the envelope. Only 1 selects exponential; other values select linear. The sbd sweep ignores this control. Retunes basic oscillators, supersaw, pulse, wavetables, and samples including `gm_*` zones. Bytebeat, ZZFX, standalone noise, live input, and buses ignore it.",
            params: &[
                ReferenceParam {
                    name: "type",
                    r#type: "number | Pattern",
                    description: "0 = linear, 1 = exponential",
                },
            ],
            examples: &[
                "note(\"g1*4\")\n.s(\"sine\").pdec(.5)\n.penv(32)\n.pcurve(\"<0 1>\")",
            ],
            tags: &["pitch", "envelope", "audio"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["panchor"],
        aliases: &[],
        reference: ReferenceEntry {
            name: "panchor",
            synonyms: &[],
            summary: "Anchor the pitch-envelope range relative to the base note.",
            description: "Defaults to the resolved psustain level. Anchor 0 spans note to note + penv; anchor 1 spans note - penv to note, in semitones. Requires penv or a pitch ADSR field; panchor alone does not activate the envelope. The sbd sweep ignores this control. Retunes basic oscillators, supersaw, pulse, wavetables, and samples including `gm_*` zones. Bytebeat, ZZFX, standalone noise, live input, and buses ignore it.",
            params: &[
                ReferenceParam {
                    name: "anchor",
                    r#type: "number | Pattern",
                    description: "anchor offset",
                },
            ],
            examples: &[
                "note(\"c c4\").penv(12).panchor(\"<0 .5 1 .5>\")",
            ],
            tags: &["pitch", "envelope", "audio"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["gate"],
        aliases: &["gat"],
        reference:         ReferenceEntry {
            name: "gate",
            synonyms: &["gat"],
            summary: "Envelope gate; unsupported in native audio.",
            description: "Rustel uses attack, decay, sustain and release for its envelopes; gate has no native effect. The value can be sent through osc() or mapped to a MIDI CC with midimaps.",
            params: &[
                ReferenceParam {
                    name: "value",
                    r#type: "number | Pattern",
                    description: "gate level",
                },
            ],
            examples: &[],
            tags: &["control", "osc"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["leslie"],
        aliases: &[],
        reference: ReferenceEntry {
            name: "leslie",
            synonyms: &[],
            summary: "SuperDirt (OSC): rotating-speaker effect.",
            description: "SuperDirt via `.osc()`: mixes in a Leslie rotating-speaker effect.",
            params: &[
                ReferenceParam {
                    name: "wet",
                    r#type: "number | Pattern",
                    description: "wet mix from 0 to 1",
                },
            ],
            examples: &[
                "n(\"0,4,7\").s(\"supersquare\").leslie(\"<0 .4 .6 1>\").osc()",
            ],
            tags: &["superdirt", "osc"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["lrate"],
        aliases: &[],
        reference: ReferenceEntry {
            name: "lrate",
            synonyms: &[],
            summary: "SuperDirt (OSC): Leslie rotation rate.",
            description: "SuperDirt via `.osc()`: sets the rotation rate of the `leslie` effect in Hz.",
            params: &[
                ReferenceParam {
                    name: "rate",
                    r#type: "number | Pattern",
                    description: "rate in Hz; 0.7 is slow, 6.7 is fast",
                },
            ],
            examples: &[
                "n(\"0,4,7\").s(\"supersquare\").leslie(1).lrate(\"<1 2 4 8>\").osc()",
            ],
            tags: &["superdirt", "osc"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["lsize"],
        aliases: &[],
        reference: ReferenceEntry {
            name: "lsize",
            synonyms: &[],
            summary: "SuperDirt (OSC): Leslie cabinet size.",
            description: "SuperDirt via `.osc()`: sets the cabinet size in metres for the `leslie` effect, changing its Doppler pitch variation.",
            params: &[
                ReferenceParam {
                    name: "meters",
                    r#type: "number | Pattern",
                    description: "cabinet size in metres",
                },
            ],
            examples: &[
                "n(\"0,4,7\").s(\"supersquare\").leslie(1).lrate(2).lsize(\"<.1 .5 1>\").osc()",
            ],
            tags: &["superdirt", "osc"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["activeLabel"],
        aliases: &[],
        reference:         ReferenceEntry {
            name: "activeLabel",
            synonyms: &[],
            summary: "the label the painter shows while the event sounds",
            description: "label(\"a:b\") spreads onto label and activeLabel positionally. A painter shows the plain label while the event is quiet and this one while it sounds, falling back to the note name or the sound. The audio engine reads neither.",
            params: &[
                ReferenceParam {
                    name: "value",
                    r#type: "string | Pattern",
                    description: "text to show while sounding",
                },
            ],
            examples: &[
                "note(\"c e g\").label(\"quiet:LOUD\")",
            ],
            tags: &["control", "visualization"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["label", "activeLabel"],
        aliases: &[],
        reference: ReferenceEntry {
            name: "label",
            synonyms: &[],
            summary: "Sets the displayed text for an event on the pianoroll",
            description: "Sets the displayed text for an event on the pianoroll",
            params: &[
                ReferenceParam {
                    name: "label",
                    r#type: "string",
                    description: "text to display",
                },
            ],
            examples: &[],
            tags: &["visualization"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["degree"],
        aliases: &[],
        reference:         ReferenceEntry {
            name: "degree",
            synonyms: &[],
            summary: "the scale degree - written by edoScale, shown by the tuning trace",
            description: "Not a control the audio resolver reads: the edoScale layer writes it into the event - with the degree indexes, root, edo and resolved frequency - and the scheduler's trace carries it to the UI's tuning display. Set it yourself and it merely rides the event. The value rides the event and leaves the engine through .osc(), and a midimaps entry can turn it into a CC; natively it is silent.",
            params: &[
                ReferenceParam {
                    name: "value",
                    r#type: "number | Pattern",
                    description: "scale degree",
                },
            ],
            examples: &[
                "note(\"c e g\").degree(2)",
            ],
            tags: &["control", "tuning"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["mtranspose"],
        aliases: &[],
        reference:         ReferenceEntry {
            name: "mtranspose",
            synonyms: &[],
            summary: "SuperDirt (OSC): scale-degree transposition.",
            description: "SuperDirt via `.osc()`: transposes scale degrees through SuperCollider's pitch model. Requires the optional pitch-model setup in SuperDirt.",
            params: &[
                ReferenceParam {
                    name: "value",
                    r#type: "number | Pattern",
                    description: "transposition in scale degrees",
                },
            ],
            examples: &[],
            tags: &["superdirt", "osc"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["ctranspose"],
        aliases: &[],
        reference:         ReferenceEntry {
            name: "ctranspose",
            synonyms: &[],
            summary: "SuperDirt (OSC): semitone transposition.",
            description: "SuperDirt via `.osc()`: transposes notes in semitones through SuperCollider's pitch model. Requires the optional pitch-model setup in SuperDirt.",
            params: &[
                ReferenceParam {
                    name: "value",
                    r#type: "number | Pattern",
                    description: "transposition in semitones",
                },
            ],
            examples: &[],
            tags: &["superdirt", "osc"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["harmonic"],
        aliases: &[],
        reference:         ReferenceEntry {
            name: "harmonic",
            synonyms: &[],
            summary: "SuperDirt (OSC): pitch multiplier.",
            description: "SuperDirt via `.osc()`: multiplies the frequency through SuperCollider's pitch model. Requires the optional pitch-model setup in SuperDirt.",
            params: &[
                ReferenceParam {
                    name: "value",
                    r#type: "number | Pattern",
                    description: "frequency multiplier",
                },
            ],
            examples: &[],
            tags: &["superdirt", "osc"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["stepsPerOctave"],
        aliases: &[],
        reference:         ReferenceEntry {
            name: "stepsPerOctave",
            synonyms: &[],
            summary: "SuperDirt (OSC): tuning steps per octave.",
            description: "SuperDirt via `.osc()`: sets the number of tuning steps per octave in SuperCollider's pitch model. Requires the optional pitch-model setup in SuperDirt.",
            params: &[
                ReferenceParam {
                    name: "value",
                    r#type: "number | Pattern",
                    description: "steps per octave",
                },
            ],
            examples: &[],
            tags: &["superdirt", "osc"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["octaveR"],
        aliases: &[],
        reference:         ReferenceEntry {
            name: "octaveR",
            synonyms: &[],
            summary: "Tuning octave ratio; unsupported in native audio.",
            description: "Rustel does not apply this tuning control to native notes. The value can be sent through osc() or mapped to a MIDI CC with midimaps.",
            params: &[
                ReferenceParam {
                    name: "value",
                    r#type: "number | Pattern",
                    description: "the octave ratio",
                },
            ],
            examples: &[],
            tags: &["control", "osc"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["nudge"],
        aliases: &[],
        reference:         ReferenceEntry {
            name: "nudge",
            synonyms: &[],
            summary: "Delay a recorded sample's source inside its event, in seconds.",
            description: "The default is 0; negative values clamp to 0. Only the source starts later, on the frame at or after the nudged instant. Its envelope, the event time, and the clock stay unchanged, so a large nudge can outlast the envelope and produce silence. Applies to recorded sample banks. Synths, wavetables, live input, and `gm_*` soundfont zones ignore this control.",
            params: &[
                ReferenceParam {
                    name: "value",
                    r#type: "number | Pattern",
                    description: "delay of the source start in seconds; default 0, negatives clamp to 0",
                },
            ],
            examples: &[
                "s(\"cp*4\").nudge(\"0 0.01 0.02 0.04\")",
            ],
            tags: &["control", "samples"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["octave"],
        aliases: &["oct"],
        reference: ReferenceEntry {
            name: "octave",
            synonyms: &["oct"],
            summary: "Transpose supported pitched synths by an octave offset.",
            description: "Defaults to 0. Each +1 doubles frequency and each -1 halves it; fractional offsets work too. Applies after freq or note resolution to basic oscillators, supersaw, pulse, bytebeat, wavetables, and sbd. Recorded samples, `gm_*` soundfont zones, and ZZFX ignore this multiplier, as do standalone noise, live input, and buses. Use note or freq to repitch samples and ZZFX.",
            params: &[
                ReferenceParam {
                    name: "octave",
                    r#type: "number | Pattern",
                    description: "octave number",
                },
            ],
            examples: &[
                "n(\"0,4,7\").scale(\"F:minor\").s('supersaw').octave(\"<0 1 2 3>\")",
            ],
            tags: &["superdirt"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["orbit"],
        aliases: &["o"],
        reference: ReferenceEntry {
            name: "orbit",
            synonyms: &["o"],
            summary: "An `orbit` is a global parameter context for patterns.",
            description: "An `orbit` is a global parameter context for patterns. Patterns with the same orbit will share the same global effects.",
            params: &[
                ReferenceParam {
                    name: "number",
                    r#type: "number | Pattern",
                    description: "",
                },
            ],
            examples: &[
                "stack(\n  s(\"hh*6\").delay(.5).delaytime(.25).orbit(1),\n  s(\"~ sd ~ sd\").delay(.5).delaytime(.125).orbit(2)\n)",
            ],
            tags: &["audio"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["bus"],
        aliases: &[],
        reference: ReferenceEntry {
            name: "bus",
            synonyms: &[],
            summary: "A `bus` is a send which can be used for mixing patterns.",
            description: "A `bus` is a send which can be used for mixing patterns. It combines with..\n  s(\"bus\") to play that bus through another pattern (for, say, applying non-linear\n  effects like distortion to multiple signals)\n\n  otherPat.bmod(..) (to modulate another pattern with the bus)",
            params: &[
                ReferenceParam {
                    name: "number",
                    r#type: "number | Pattern",
                    description: "",
                },
            ],
            examples: &[],
            tags: &["superdirt"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["busgain"],
        aliases: &["bgain"],
        reference: ReferenceEntry {
            name: "busgain",
            synonyms: &["bgain"],
            summary: "Postgain multiplier prior to sending the signal to the audio bus.",
            description: "Postgain multiplier prior to sending the signal to the audio bus.\n\nDefaults to 1. Requires a bus send; naming busgain alone does not route audio. The send is tapped before dry, so dry(0) does not mute the bus.",
            params: &[
                ReferenceParam {
                    name: "number",
                    r#type: "number | Pattern",
                    description: "",
                },
            ],
            examples: &[],
            tags: &["superdirt"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["overgain"],
        aliases: &[],
        reference:         ReferenceEntry {
            name: "overgain",
            synonyms: &[],
            summary: "SuperDirt (OSC): additional gain.",
            description: "SuperDirt via `.osc()`: adds to `gain` before its fourth-power amplitude scaling.",
            params: &[
                ReferenceParam {
                    name: "value",
                    r#type: "number | Pattern",
                    description: "additional gain",
                },
            ],
            examples: &[],
            tags: &["superdirt", "osc"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["overshape"],
        aliases: &[],
        reference:         ReferenceEntry {
            name: "overshape",
            synonyms: &[],
            summary: "Waveshaper curve; unsupported in native audio.",
            description: "Rustel does not apply this control. Use shape or distort for native distortion. The value can be sent through osc() or mapped to a MIDI CC with midimaps.",
            params: &[
                ReferenceParam {
                    name: "value",
                    r#type: "number | Pattern",
                    description: "shaping curve",
                },
            ],
            examples: &[],
            tags: &["control", "osc"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["pan"],
        aliases: &[],
        reference: ReferenceEntry {
            name: "pan",
            synonyms: &[],
            summary: "Sets position in stereo.",
            description: "Sets position in stereo.",
            params: &[
                ReferenceParam {
                    name: "pan",
                    r#type: "number | Pattern",
                    description: "between 0 and 1, from left to right (assuming stereo), once round a circle (assuming multichannel)",
                },
            ],
            examples: &[
                "s(\"[bd hh]*2\").pan(\"<.5 1 .5 0>\")",
                "s(\"bd rim sd rim bd ~ cp rim\").pan(sine.slow(2))",
            ],
            tags: &["audio"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["panspan"],
        aliases: &[],
        reference: ReferenceEntry {
            name: "panspan",
            synonyms: &[],
            summary: "Multichannel pan span; unsupported in native audio.",
            description: "Rustel does not apply this control to native audio. `.osc()` sends `panspan` unchanged, but SuperDirt expects `span` instead. Use `pan` for native positioning.",
            params: &[
                ReferenceParam {
                    name: "span",
                    r#type: "number | Pattern",
                    description: "between -inf and inf, negative is backwards ordering",
                },
            ],
            examples: &[],
            tags: &["control", "osc"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["pansplay"],
        aliases: &[],
        reference: ReferenceEntry {
            name: "pansplay",
            synonyms: &[],
            summary: "Multichannel pan spread; unsupported in native audio.",
            description: "Rustel does not apply this control to native audio. `.osc()` sends `pansplay` unchanged, but SuperDirt expects `splay` instead. Use `pan` for native positioning.",
            params: &[
                ReferenceParam {
                    name: "spread",
                    r#type: "number | Pattern",
                    description: "between 0 and 1",
                },
            ],
            examples: &[],
            tags: &["control", "osc"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["panwidth"],
        aliases: &[],
        reference:         ReferenceEntry {
            name: "panwidth",
            synonyms: &[],
            summary: "SuperDirt (OSC): multichannel pan width.",
            description: "SuperDirt via `.osc()`: sets the panning width when SuperDirt has more than two output channels.",
            params: &[
                ReferenceParam {
                    name: "value",
                    r#type: "number | Pattern",
                    description: "pan width; default 2",
                },
            ],
            examples: &[],
            tags: &["superdirt", "osc"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["panorient"],
        aliases: &[],
        reference:         ReferenceEntry {
            name: "panorient",
            synonyms: &[],
            summary: "Multichannel pan orientation; unsupported in native audio.",
            description: "Rustel does not apply this control to native audio. `.osc()` sends `panorient` unchanged, but SuperDirt expects `orientation` instead. Use `pan` for native positioning.",
            params: &[
                ReferenceParam {
                    name: "value",
                    r#type: "number | Pattern",
                    description: "pan orientation",
                },
            ],
            examples: &[],
            tags: &["control", "osc"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["slide"],
        aliases: &[],
        reference:         ReferenceEntry {
            name: "slide",
            synonyms: &[],
            summary: "the ZZFX pitch slide",
            description: "Glides the generator's pitch across the note - positive climbs, negative falls; the default 0 plays straight. Applies to s(\"zzfx\") and the z_* sounds; the table declares it twice and both spellings are this one control. One of the twenty parameters of the ZZFX generator behind s(\"zzfx\") and the z_* sounds, which resolve their own pitch - freq, or the note control, or C2. A raw zzfx([...]) array overrides every parameter positionally and wins over these controls.",
            params: &[
                ReferenceParam {
                    name: "value",
                    r#type: "number | Pattern",
                    description: "slide amount; default 0",
                },
            ],
            examples: &[
                "s(\"zzfx\").note(\"c4\").slide(\"0 0.5 -0.5 1\")",
            ],
            tags: &["control", "synth"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["semitone"],
        aliases: &[],
        reference:         ReferenceEntry {
            name: "semitone",
            synonyms: &[],
            summary: "SuperDirt (OSC): secondary oscillator interval.",
            description: "SuperDirt via `.osc()`: sets the secondary oscillator interval in synths that expose this control, such as `supersquare`. The direction depends on the synth.",
            params: &[
                ReferenceParam {
                    name: "value",
                    r#type: "number | Pattern",
                    description: "interval in semitones",
                },
            ],
            examples: &[],
            tags: &["superdirt", "osc"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["voice"],
        aliases: &[],
        reference:         ReferenceEntry {
            name: "voice",
            synonyms: &[],
            summary: "SuperDirt (OSC): synth timbre control.",
            description: "SuperDirt via `.osc()`: changes timbre in synths that expose this control. Its meaning depends on the synth, such as pulse width for `supersquare`.",
            params: &[
                ReferenceParam {
                    name: "value",
                    r#type: "number | Pattern",
                    description: "synth-specific timbre value",
                },
            ],
            examples: &[],
            tags: &["superdirt", "osc"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["chord"],
        aliases: &[],
        reference: ReferenceEntry {
            name: "chord",
            synonyms: &[],
            summary: "The chord to voice",
            description: "The chord to voice",
            params: &[
                ReferenceParam {
                    name: "symbols",
                    r#type: "chord | Pattern",
                    description: "chord symbols to voice e.g., C, Eb, Fm7, G7. The symbols can be defined via addVoicings",
                },
            ],
            examples: &[
                "chord(\"<Am C D F Am E Am E>\").voicing()",
            ],
            tags: &["tonal"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["dictionary"],
        aliases: &["dict"],
        reference: ReferenceEntry {
            name: "dictionary",
            synonyms: &["dict"],
            summary: "Which dictionary to use for the voicings.",
            description: "Which dictionary to use for the voicings. This falls back to the default dictionary if not provided",
            params: &[
                ReferenceParam {
                    name: "dictionaryName",
                    r#type: "string",
                    description: "which dictionary (having been defined with `addVoicings`) to use",
                },
            ],
            examples: &[
                "addVoicings('house', {\n'': ['7 12 16', '0 7 16', '4 7 12'],\n'm': ['0 3 7']\n})\nchord(\"<Am C D F Am E Am E>\")\n.dict('house').anchor(66)\n.voicing().room(.5)",
            ],
            tags: &["tonal"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["anchor"],
        aliases: &[],
        reference: ReferenceEntry {
            name: "anchor",
            synonyms: &[],
            summary: "The top note to align the voicing to.",
            description: "The top note to align the voicing to. Defaults to c5",
            params: &[
                ReferenceParam {
                    name: "anchorNote",
                    r#type: "string | Pattern",
                    description: "the note to align the voicing or scale to",
                },
            ],
            examples: &[
                "anchor(\"<c4 g4 c5 g5>\").chord(\"C\").voicing()",
                "n(\"0 .. 7\").anchor(\"<c4 g4 c5 g5>\").scale(\"<C:major F:minor>\")",
            ],
            tags: &["tonal"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["offset"],
        aliases: &[],
        reference: ReferenceEntry {
            name: "offset",
            synonyms: &[],
            summary: "Sets how the voicing is offset from the anchored position",
            description: "Sets how the voicing is offset from the anchored position",
            params: &[
                ReferenceParam {
                    name: "shift",
                    r#type: "number | Pattern",
                    description: "the amount to shift the voicing up or down",
                },
            ],
            examples: &[
                "chord(\"<Am C D F Am E Am E>\").offset(\"<0 1 2 3 4 5>\") // alter the voicing each time",
            ],
            tags: &["tonal"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["octaves"],
        aliases: &[],
        reference: ReferenceEntry {
            name: "octaves",
            synonyms: &[],
            summary: "How many octaves are voicing steps spread apart, defaults to 1",
            description: "How many octaves are voicing steps spread apart, defaults to 1\n\n @name octaves\n @tags tonal\n @param {number | Pattern} count the number of octaves\n @example\n chord(\"<Am C D F Am E Am E>\").octaves(\"<2 4>\").voicing()",
            params: &[],
            examples: &[],
            tags: &["tonal"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["mode", "anchor"],
        aliases: &[],
        reference: ReferenceEntry {
            name: "mode",
            synonyms: &[],
            summary: "Remove anchor note from the voicing.",
            description: "Remove anchor note from the voicing. Useful for melody harmonization",
            params: &[
                ReferenceParam {
                    name: "modeName",
                    r#type: "string | Pattern",
                    description: "below, above, duck, root, oldabove, or oldroot",
                },
            ],
            examples: &[
                "mode(\"<below above duck root>\").chord(\"C\").voicing()",
            ],
            tags: &["tonal"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["room", "size"],
        aliases: &[],
        reference: ReferenceEntry {
            name: "room",
            synonyms: &[],
            summary: "Send a voice to its orbit's convolution reverb.",
            description: "Absent by default; a positive room amount enables the send. Secondary room/IR controls alone do not enable it. The generated response defaults to roomsize(2), roomfade(0.1), roomlp(15000), and roomdim(1000). Offline rendering can replace it with a loaded ir sample; unavailable IR samples fall back to the generated response. Live device audio currently prepares generated responses only, so ir, irspeed, and irbegin do not select a custom live response. dry controls the separate direct-output level, default 1. These are effects on any native audio source, including oscillators, wavetables, samples, `gm_*` soundfonts, and live input.",
            params: &[
                ReferenceParam {
                    name: "level",
                    r#type: "number | Pattern",
                    description: "between 0 and 1",
                },
            ],
            examples: &[
                "s(\"bd sd [~ bd] sd\").room(\"<0 .2 .4 .6 .8 1>\")",
                "s(\"bd sd [~ bd] sd\").room(\"<0.9:1 0.9:4>\")",
            ],
            tags: &["orbit", "audio"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["roomlp"],
        aliases: &["rlp"],
        reference: ReferenceEntry {
            name: "roomlp",
            synonyms: &["rlp"],
            summary: "Initial lowpass frequency of the generated reverb, in hertz.",
            description: "Defaults to 15000 Hz. Requires a positive room send and a generated response. roomlp(0) skips this damping filter; otherwise the cutoff moves toward roomdim as the response decays. A loaded custom ir does not use this filter.",
            params: &[
                ReferenceParam {
                    name: "frequency",
                    r#type: "number",
                    description: "between 0 and 20000hz",
                },
            ],
            examples: &[
                "s(\"bd sd [~ bd] sd\").room(0.5).rlp(10000)",
                "s(\"bd sd [~ bd] sd\").room(0.5).rlp(5000)",
            ],
            tags: &["orbit", "audio"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["roomdim"],
        aliases: &["rdim"],
        reference: ReferenceEntry {
            name: "roomdim",
            synonyms: &["rdim"],
            summary: "Generated reverb lowpass frequency at a 60 dB decay, in hertz.",
            description: "Defaults to 1000 Hz. Requires a positive room send, a generated response, and nonzero roomlp. A loaded custom ir does not use this damping setting.",
            params: &[
                ReferenceParam {
                    name: "frequency",
                    r#type: "number",
                    description: "between 0 and 20000hz",
                },
            ],
            examples: &[
                "s(\"bd sd [~ bd] sd\").room(0.5).rlp(10000).rdim(8000)",
                "s(\"bd sd [~ bd] sd\").room(0.5).rlp(5000).rdim(400)",
            ],
            tags: &["orbit", "audio"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["roomfade"],
        aliases: &["rfade"],
        reference: ReferenceEntry {
            name: "roomfade",
            synonyms: &["rfade"],
            summary: "Fade-in time of the generated reverb response, in seconds.",
            description: "Defaults to 0.1 seconds; negative values act as 0. Requires a positive room send and a generated response. A loaded custom ir does not use this fade. Changing it rebuilds the generated response.",
            params: &[
                ReferenceParam {
                    name: "seconds",
                    r#type: "number",
                    description: "for the reverb to fade",
                },
            ],
            examples: &[
                "s(\"bd sd [~ bd] sd\").room(0.5).rlp(10000).rfade(0.5)",
                "s(\"bd sd [~ bd] sd\").room(0.5).rlp(5000).rfade(4)",
            ],
            tags: &["orbit", "audio"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["ir", "i"],
        aliases: &["iresponse"],
        reference: ReferenceEntry {
            name: "iresponse",
            synonyms: &["ir"],
            summary: "Use a loaded sample as the reverb impulse response.",
            description: "The ir alias selects a sample for convolution; it does not change the voice's sound source. Requires a positive room send. If the named sample is unknown, loading, or failed, the voice uses the generated response and reports that fallback. irspeed defaults to 1 and irbegin to 0. With no ir, the generated response is used.\n\nCustom responses are supported by offline rendering. Live device audio currently uses generated responses and ignores this custom-IR setting.",
            params: &[
                ReferenceParam {
                    name: "sample",
                    r#type: "string | Pattern",
                    description: "to use as an impulse response",
                },
            ],
            examples: &[
                "s(\"bd sd [~ bd] sd\").room(.8).ir(\"<shaker_large:0 shaker_large:2>\")",
            ],
            tags: &["orbit", "audio"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["irspeed"],
        aliases: &[],
        reference: ReferenceEntry {
            name: "irspeed",
            synonyms: &[],
            summary: "Sampling-step multiplier when preparing a custom reverb response.",
            description: "Defaults to 1. Requires both a positive room send and a loaded ir sample. It changes the impulse response preparation, not the playing voice's sample speed. Generated responses do not use this control.\n\nCustom responses are supported by offline rendering. Live device audio currently uses generated responses and ignores this custom-IR setting.",
            params: &[
                ReferenceParam {
                    name: "speed",
                    r#type: "string | Pattern",
                    description: "",
                },
            ],
            examples: &[
                "samples('github:switchangel/pad')\n$: s(\"brk/2\").fit().scrub(irand(16).div(16).seg(8)).ir(\"swpad:4\").room(.2).irspeed(\"<2 1 .5>/2\").irbegin(.5).roomsize(.5)",
            ],
            tags: &["orbit", "audio"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["irbegin"],
        aliases: &[],
        reference: ReferenceEntry {
            name: "irbegin",
            synonyms: &["ir"],
            summary: "Starting position in a custom reverb sample, from 0 to 1.",
            description: "Defaults to 0 and is clamped to 0..1 while preparing the response. Requires a positive room send and a loaded ir sample. Generated responses do not use this position.\n\nCustom responses are supported by offline rendering. Live device audio currently uses generated responses and ignores this custom-IR setting.",
            params: &[
                ReferenceParam {
                    name: "begin",
                    r#type: "string | Pattern",
                    description: "between 0 and 1",
                },
            ],
            examples: &[
                "samples('github:switchangel/pad')\n$: s(\"brk/2\").fit().scrub(irand(16).div(16).seg(8)).ir(\"swpad:4\").room(.65).irspeed(\"-2\").irbegin(\"<0 .5 .75>/2\").roomsize(.6)",
            ],
            tags: &["orbit", "audio"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["roomsize"],
        aliases: &["size", "sz", "rsize"],
        reference: ReferenceEntry {
            name: "roomsize",
            synonyms: &["rsize", "sz", "size"],
            summary: "Reverb response length control in seconds.",
            description: "Defaults to 2 seconds and admits values from 0 to 10. Requires a positive room send. For generated reverb this is the time to decay by 60 dB (the generated tail lasts longer); for a custom ir it limits how much source audio fills the response. Changing it rebuilds the response.",
            params: &[
                ReferenceParam {
                    name: "size",
                    r#type: "number | Pattern",
                    description: "between 0 and 10",
                },
            ],
            examples: &[
                "s(\"bd sd [~ bd] sd\").room(.8).rsize(1)",
                "s(\"bd sd [~ bd] sd\").room(.8).rsize(4)",
            ],
            tags: &["orbit", "audio"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["shape", "shapevol"],
        aliases: &[],
        reference: ReferenceEntry {
            name: "shape",
            synonyms: &["shapevol"],
            summary: "(Deprecated) Wave shaping distortion.",
            description: "(Deprecated) Wave shaping distortion. WARNING: can suddenly get unpredictably loud.\nPlease use distort instead, which has a more predictable response curve\nsecond option in optional array syntax (ex: \".9:.5\") applies a postgain to the output\n\nThe shape stage is absent by default. Its second field, shapevol, defaults to 1; shapevol alone does not create the stage. These are effects on any native audio source, including oscillators, wavetables, samples, `gm_*` soundfonts, and live input.",
            params: &[
                ReferenceParam {
                    name: "distortion",
                    r#type: "number | Pattern",
                    description: "between 0 and 1",
                },
            ],
            examples: &[
                "s(\"bd sd [~ bd] sd,hh*8\").shape(\"<0 .2 .4 .6 .8>\")",
            ],
            tags: &["distortion", "audio"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["limitchar"],
        aliases: &[],
        reference: ReferenceEntry {
            name: "limitchar",
            synonyms: &[],
            summary: "Which character the voice\'s limiter has.",
            description: "Which character `limit` has: `transparent`, `punchy`, `warm` or `hard`. Usually written as the second value of `limit` in the optional array syntax - `.limit(\"-6:hard\")` - rather than on its own.",
            params: &[ReferenceParam {
                name: "character",
                r#type: "string | Pattern",
                description: "transparent, punchy, warm or hard",
            }],
            examples: &["s(\"bd*4\").limit(\"-6\").limitchar(\"hard\")"],
            tags: &["dynamics", "audio"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["limit", "limitchar"],
        aliases: &[],
        reference: LIMIT_REFERENCE,
    },
    ControlRow {
        names: &["distort", "distortvol", "distorttype"],
        aliases: &["dist"],
        reference: ReferenceEntry {
            name: "distort",
            synonyms: &["dist"],
            summary: "Wave shaping distortion.",
            description: "Wave shaping distortion. CAUTION: it can get loud.\nSecond option in optional array syntax (ex: \".9:.5\") applies a postgain to the output. Third option sets the waveshaping type.\nMost useful values are usually between 0 and 10 (depending on source gain). If you are feeling adventurous, you can turn it up to 11 and beyond ;)\n\nAbsent by default; setting distort creates the stage. distortvol defaults to 1 and is clamped to 0.001..1; distorttype selects the algorithm. Neither secondary field creates a stage without distort or diode. These are effects on any native audio source, including oscillators, wavetables, samples, `gm_*` soundfonts, and live input.",
            params: &[
                ReferenceParam {
                    name: "distortion",
                    r#type: "number | Pattern",
                    description: "amount of distortion to apply",
                },
                ReferenceParam {
                    name: "volume",
                    r#type: "number | Pattern",
                    description: "linear postgain of the distortion",
                },
                ReferenceParam {
                    name: "type",
                    r#type: "number | string | Pattern",
                    description: "type of distortion to apply",
                },
            ],
            examples: &[
                "s(\"bd sd [~ bd] sd,hh*8\").distort(\"<0 2 3 10:.5>\")",
                "note(\"d1!8\").s(\"sine\").penv(36).pdecay(.12).decay(.23).distort(\"8:.4\")",
                "s(\"bd:4*4\").bank(\"tr808\").distort(\"3:0.5:diode\")",
            ],
            tags: &["distortion", "audio"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["distortvol"],
        aliases: &["distvol"],
        reference: ReferenceEntry {
            name: "distortvol",
            synonyms: &["distortion", "distvol"],
            summary: "Output multiplier for an enabled distortion stage.",
            description: "Defaults to 1 and is clamped to 0.001..1. Requires distort or diode; it does not create distortion by itself. It scales the output of the distortion, while gain affects the level driven into it.",
            params: &[
                ReferenceParam {
                    name: "volume",
                    r#type: "number | Pattern",
                    description: "linear postgain of the distortion",
                },
            ],
            examples: &[
                "s(\"bd*4\").bank(\"tr909\").distort(2).distortvol(0.8)",
            ],
            tags: &["audio"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["diode"],
        aliases: &[],
        reference: ReferenceEntry {
            name: "diode",
            synonyms: &[],
            summary: "Diode-emulating distortion",
            description: "Diode-emulating distortion\n\nCreates the diode distortion stage on any native audio source. The compound form diode(\"amount:volume\") sets an output multiplier, default 1. If distort is also set it supplies the amount; an explicit distortvol or distorttype overrides the corresponding diode default.",
            params: &[
                ReferenceParam {
                    name: "distortion",
                    r#type: "number | Pattern",
                    description: "amount of distortion to apply",
                },
                ReferenceParam {
                    name: "volume",
                    r#type: "number | Pattern",
                    description: "linear postgain of the distortion",
                },
            ],
            examples: &["s(\"sawtooth\").diode(2)"],
            tags: &["distortion", "audio"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["distorttype"],
        aliases: &["disttype"],
        reference: ReferenceEntry {
            name: "distorttype",
            synonyms: &["disttype"],
            summary: "Type of waveshaping distortion to apply.",
            description: "Requires distort or diode; the algorithm selection alone does not create a stage. If no type is set, diode selects the diode algorithm even when distort supplies the amount; otherwise the first algorithm is the default. Unrecognized names fall back to the first algorithm.",
            params: &[
                ReferenceParam {
                    name: "type",
                    r#type: "number | string | Pattern",
                    description: "type of distortion to apply",
                },
            ],
            examples: &[
                "s(\"bd*4\").bank(\"tr909\").distort(2).distorttype(\"<0 1 2>\")",
                "s(\"sine\").note(\"F1*2\").release(1)\n  .penv(24).pdecay(0.05)\n  .distort(rand.range(1, 8))\n  .distorttype(\"<fold chebyshev scurve diode asym sinefold>\")",
            ],
            tags: &["distortion", "audio"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["compressor", "compressorRatio", "compressorKnee", "compressorAttack", "compressorRelease"],
        aliases: &[],
        reference: ReferenceEntry {
            name: "compressor",
            synonyms: &[],
            summary: "Dynamics Compressor.",
            description: "Dynamics Compressor. The params are `compressor(\"threshold:ratio:knee:attack:release\")`\nMore info [here](https://developer.mozilla.org/en-US/docs/Web/API/DynamicsCompressorNode?retiredLocale=de#instance_properties)\n\nThe stage is absent until compressor supplies a threshold in dB, clamped to -100..0. The secondary parameters alone do not create it. Ratio/knee/attack/release default to 10, 10 dB, 0.005 seconds, and 0.05 seconds. These are effects on any native audio source, including oscillators, wavetables, samples, `gm_*` soundfonts, and live input.",
            params: &[],
            examples: &[
                "s(\"bd sd [~ bd] sd,hh*8\")\n.compressor(\"-20:20:10:.002:.02\")",
            ],
            tags: &["audio"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["compressorKnee"],
        aliases: &[],
        reference:         ReferenceEntry {
            name: "compressorKnee",
            synonyms: &[],
            summary: "how wide the bend at the compressor's threshold is",
            description: "Sets, in decibels, how far above the threshold the gain reduction eases in rather than switching on at the full ratio: 0 is a hard knee, larger numbers round the bend. The default is 10, and the value clamps into 0..40, the range the underlying node declares.\n\nIt is one of the four spellings of compressor() - the others being compressorRatio, compressorAttack and compressorRelease - and none of them does anything unless compressor itself sets a threshold on the event, because that is the control that puts the node in the chain.",
            params: &[
                ReferenceParam {
                    name: "value",
                    r#type: "number | Pattern",
                    description: "knee width in dB; default 10, clamped 0..40",
                },
            ],
            examples: &[
                "s(\"oh*4\").compressor(-20).compressorKnee(\"0 10 20 40\")",
            ],
            tags: &["control", "dynamics"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["compressorRatio"],
        aliases: &[],
        reference:         ReferenceEntry {
            name: "compressorRatio",
            synonyms: &[],
            summary: "how hard the compressor squeezes what crosses the threshold",
            description: "The squeeze above the threshold: at ratio 4, four decibels over the threshold come out as one. The default is 10, and the value clamps into 1..20 - the gain computer divides by the ratio, and a 0 reaching it used to fill rendered audio with NaN before the clamp.\n\nOne of the four spellings of compressor(), doing nothing unless compressor itself sets a threshold on the event.",
            params: &[
                ReferenceParam {
                    name: "value",
                    r#type: "number | Pattern",
                    description: "compression ratio; default 10, clamped 1..20",
                },
            ],
            examples: &[
                "s(\"oh*4\").compressor(-20).compressorRatio(\"1 4 10 20\")",
            ],
            tags: &["control", "dynamics"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["compressorAttack"],
        aliases: &[],
        reference:         ReferenceEntry {
            name: "compressorAttack",
            synonyms: &[],
            summary: "how fast the compressor closes once the signal crosses the threshold",
            description: "In seconds, from the signal crossing the threshold to full gain reduction. The default is 0.005 - fast enough to catch a drum hit - and the value clamps into 0..1. One of the four spellings of compressor(), doing nothing unless compressor itself sets a threshold on the event.",
            params: &[
                ReferenceParam {
                    name: "value",
                    r#type: "number | Pattern",
                    description: "attack time in seconds; default 0.005, clamped 0..1",
                },
            ],
            examples: &[
                "s(\"oh*4\").compressor(-20).compressorAttack(\"0.001 0.01 0.1\")",
            ],
            tags: &["control", "dynamics"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["compressorRelease"],
        aliases: &[],
        reference:         ReferenceEntry {
            name: "compressorRelease",
            synonyms: &[],
            summary: "how fast the compressor lets go once the signal falls back",
            description: "In seconds, from the signal dropping below the threshold to the gain reduction undoing itself. The default is 0.05 and the value clamps into 0..1. One of the four spellings of compressor(), doing nothing unless compressor itself sets a threshold on the event.",
            params: &[
                ReferenceParam {
                    name: "value",
                    r#type: "number | Pattern",
                    description: "release time in seconds; default 0.05, clamped 0..1",
                },
            ],
            examples: &[
                "s(\"oh*4\").compressor(-20).compressorRelease(\"0.02 0.05 0.2 0.5\")",
            ],
            tags: &["control", "dynamics"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["speed"],
        aliases: &[],
        reference: ReferenceEntry {
            name: "speed",
            synonyms: &[],
            summary: "Playback-rate multiplier for recorded samples.",
            description: "Defaults to 1. A value of 2 doubles playback speed and raises pitch an octave; a negative value plays the buffer in reverse, and 0 mutes the event. The rate also includes the transposition from note or freq. See unit for the alternate rate calculation. Applies to recorded sample banks. Synths, wavetables, live input, and `gm_*` soundfont zones ignore this control.",
            params: &[
                ReferenceParam {
                    name: "speed",
                    r#type: "number | Pattern",
                    description: "inf to inf, negative numbers play the sample backwards.",
                },
            ],
            examples: &[
                "s(\"bd*6\").speed(\"1 2 4 1 -2 -4\")",
                "speed(\"1 1.5*2 [2 1.1]\").s(\"piano\").clip(1)",
            ],
            tags: &["pitch", "samples"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["stretch"],
        aliases: &[],
        reference: ReferenceEntry {
            name: "stretch",
            synonyms: &[],
            summary: "Pitch-shift a voice without changing its playback speed.",
            description: "A post-source effect for synths, wavetables, samples including `gm_*` zones, and live input. Absent by default. The pitch factor is value + 1 for nonnegative values and max(value / 4 + 1, 0) for negative values: 1 raises an octave and -2 lowers one. This shifts the rendered signal; it does not select a sample slice or change its playback rate. Naming stretch activates the processing stage even at 0.",
            params: &[
                ReferenceParam {
                    name: "factor",
                    r#type: "number | Pattern",
                    description: "between `-4` and `inf`. Positive increases pitch, 0 does nothing, negative decreases the pitch.",
                },
            ],
            examples: &[
                "s(\"gm_flute\").stretch(\"<2 1 0 -2>\")",
            ],
            tags: &["pitch", "samples"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["unit"],
        aliases: &[],
        reference: ReferenceEntry {
            name: "unit",
            synonyms: &[],
            summary: "Choose how a recorded sample's speed becomes a playback rate.",
            description: "The default r uses speed as a playback-rate multiplier. In the native renderer, c multiplies that rate by the file's original duration in seconds: at its base pitch, speed(1).unit(\"c\") plays a whole sample in one second, independently of tempo. Other values, including s, use the ordinary rate; s does not select a seconds-duration mode. Slicing and note transposition still apply. Applies to recorded sample banks. Synths, wavetables, live input, and `gm_*` soundfont zones ignore this control.",
            params: &[
                ReferenceParam {
                    name: "unit",
                    r#type: "number | string | Pattern",
                    description: "see description above",
                },
            ],
            examples: &[
                "speed(\"1 2 .5 3\").s(\"bd\").unit(\"c\").osc()",
            ],
            tags: &["superdirt"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["squiz"],
        aliases: &[],
        reference: ReferenceEntry {
            name: "squiz",
            synonyms: &[],
            summary: "SuperDirt (OSC): pitch-raising distortion.",
            description: "SuperDirt via `.osc()`: raises pitch by shortening waveform fragments and leaving gaps between them. Squiz was made by Calum Gunn. Requires SuperDirt's extra effects (sc3plugins).",
            params: &[
                ReferenceParam {
                    name: "squiz",
                    r#type: "number | Pattern",
                    description: "pitch ratio; try 2, 4 or 8",
                },
            ],
            examples: &[
                "squiz(\"2 4/2 6 [8 16]\").s(\"bd\").osc()",
            ],
            tags: &["superdirt", "osc"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["vowel"],
        aliases: &[],
        reference: ReferenceEntry {
            name: "vowel",
            synonyms: &[],
            summary: "Formant filter to make things sound like vowels.",
            description: "Formant filter to make things sound like vowels.",
            params: &[
                ReferenceParam {
                    name: "vowel",
                    r#type: "string | Pattern",
                    description: "You can use a e i o u ae aa oe ue y uh un en an on, corresponding to [a] [e] [i] [o] [u] [æ] [ɑ] [ø] [y] [ɯ] [ʌ] [œ̃] [ɛ̃] [ɑ̃] [ɔ̃]. Aliases: aa = å = ɑ, oe = ø = ö, y = ı, ae = æ.",
                },
            ],
            examples: &[
                "note(\"[c2 <eb2 <g2 g1>>]*2\").s('sawtooth')\n.vowel(\"<a e i <o u>>\")",
                "s(\"bd sd mt ht bd [~ cp] ht lt\").vowel(\"[a|e|i|o|u]\")",
            ],
            tags: &["audio"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["waveloss"],
        aliases: &[],
        reference:         ReferenceEntry {
            name: "waveloss",
            synonyms: &[],
            summary: "SuperDirt (OSC): waveform dropout.",
            description: "SuperDirt via `.osc()`: drops segments between zero crossings of the waveform. Requires SuperDirt's extra effects (sc3plugins).",
            params: &[
                ReferenceParam {
                    name: "value",
                    r#type: "number | Pattern",
                    description: "percentage of segments to drop",
                },
            ],
            examples: &[],
            tags: &["superdirt", "osc"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["density"],
        aliases: &[],
        reference: ReferenceEntry {
            name: "density",
            synonyms: &[],
            summary: "Impulse density of the crackle noise source",
            description: "Controls only s(\"crackle\"); white, pink, brown, other synths, and samples ignore it. Each sample has density × 0.01 probability of producing a random impulse. The default 0.02 is a sparse 0.02% chance per sample; 1 means 1%, and 100 means continuous noise. Zero produces silence. This changes the noise texture, not the number of pattern events.",
            params: &[
                ReferenceParam {
                    name: "density",
                    r#type: "number | Pattern",
                    description: "impulse probability in percent per sample, useful range 0..100; default 0.02.",
                },
            ],
            examples: &[
                "s(\"crackle*4\").density(\"<0.01 0.04 0.2 0.5>\".slow(4))",
                "s(\"crackle*4\").density(\"<0 1 10>\")",
            ],
            tags: &["audio"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["expression"],
        aliases: &[],
        reference:         ReferenceEntry {
            name: "expression",
            synonyms: &[],
            summary: "MIDI expression; unsupported in native audio.",
            description: "Rustel does not apply expression to native voices. The value can be sent through osc() or mapped to a MIDI CC with midimaps.",
            params: &[
                ReferenceParam {
                    name: "value",
                    r#type: "number | Pattern",
                    description: "expression amount",
                },
            ],
            examples: &[],
            tags: &["control", "osc"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["sustainpedal"],
        aliases: &[],
        reference:         ReferenceEntry {
            name: "sustainpedal",
            synonyms: &[],
            summary: "Sustain pedal; unsupported in native audio.",
            description: "Rustel does not apply sustain-pedal control to native voices. The value can be sent through osc() or mapped to a MIDI CC with midimaps.",
            params: &[
                ReferenceParam {
                    name: "value",
                    r#type: "number | Pattern",
                    description: "pedal down amount",
                },
            ],
            examples: &[],
            tags: &["control", "osc"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["fshift"],
        aliases: &[],
        reference:         ReferenceEntry {
            name: "fshift",
            synonyms: &[],
            summary: "SuperDirt (OSC): frequency shift.",
            description: "SuperDirt via `.osc()`: shifts frequencies by `fshift + fshiftnote * note frequency`, in Hz. Requires SuperDirt's extra effects (sc3plugins).",
            params: &[
                ReferenceParam {
                    name: "value",
                    r#type: "number | Pattern",
                    description: "frequency shift in Hz",
                },
            ],
            examples: &[],
            tags: &["superdirt", "osc"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["fshiftnote"],
        aliases: &[],
        reference:         ReferenceEntry {
            name: "fshiftnote",
            synonyms: &[],
            summary: "SuperDirt (OSC): note-relative frequency shift.",
            description: "SuperDirt via `.osc()`: adds a multiple of the current note frequency to the `fshift` amount. Use with `fshift`, including `fshift(0)`. Requires SuperDirt's extra effects (sc3plugins).",
            params: &[
                ReferenceParam {
                    name: "value",
                    r#type: "number | Pattern",
                    description: "note-frequency multiplier",
                },
            ],
            examples: &[],
            tags: &["superdirt", "osc"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["fshiftphase"],
        aliases: &[],
        reference:         ReferenceEntry {
            name: "fshiftphase",
            synonyms: &[],
            summary: "SuperDirt (OSC): frequency-shifter phase.",
            description: "SuperDirt via `.osc()`: sets the phase of the frequency shifter. Use with `fshift`. Requires SuperDirt's extra effects (sc3plugins).",
            params: &[
                ReferenceParam {
                    name: "value",
                    r#type: "number | Pattern",
                    description: "phase in radians",
                },
            ],
            examples: &[],
            tags: &["superdirt", "osc"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["triode"],
        aliases: &[],
        reference:         ReferenceEntry {
            name: "triode",
            synonyms: &[],
            summary: "SuperDirt (OSC): tube-style distortion.",
            description: "SuperDirt via `.osc()`: applies asymmetric distortion to the negative half of the waveform. Requires SuperDirt's extra effects (sc3plugins).",
            params: &[
                ReferenceParam {
                    name: "value",
                    r#type: "number | Pattern",
                    description: "distortion amount",
                },
            ],
            examples: &[],
            tags: &["superdirt", "osc"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["krush"],
        aliases: &[],
        reference:         ReferenceEntry {
            name: "krush",
            synonyms: &[],
            summary: "SuperDirt (OSC): Krush distortion.",
            description: "SuperDirt via `.osc()`: applies nonlinear distortion and filtering. Zero leaves the original signal unchanged; `kcutoff` sets the filter cutoff. Requires SuperDirt's extra effects (sc3plugins).",
            params: &[
                ReferenceParam {
                    name: "value",
                    r#type: "number | Pattern",
                    description: "distortion amount",
                },
            ],
            examples: &[],
            tags: &["superdirt", "osc"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["kcutoff"],
        aliases: &[],
        reference:         ReferenceEntry {
            name: "kcutoff",
            synonyms: &[],
            summary: "SuperDirt (OSC): Krush filter cutoff.",
            description: "SuperDirt via `.osc()`: sets the low-pass filter cutoff for `krush`. Use with `krush`. Requires SuperDirt's extra effects (sc3plugins).",
            params: &[
                ReferenceParam {
                    name: "value",
                    r#type: "number | Pattern",
                    description: "cutoff in Hz",
                },
            ],
            examples: &[],
            tags: &["superdirt", "osc"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["octer"],
        aliases: &[],
        reference:         ReferenceEntry {
            name: "octer",
            synonyms: &[],
            summary: "SuperDirt (OSC): octave-up level.",
            description: "SuperDirt via `.osc()`: mixes in sound one octave above the input. Requires SuperDirt's extra effects (sc3plugins).",
            params: &[
                ReferenceParam {
                    name: "value",
                    r#type: "number | Pattern",
                    description: "octave-up amount",
                },
            ],
            examples: &[],
            tags: &["superdirt", "osc"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["octersub"],
        aliases: &[],
        reference:         ReferenceEntry {
            name: "octersub",
            synonyms: &[],
            summary: "SuperDirt (OSC): octave-down level.",
            description: "SuperDirt via `.osc()`: mixes in sound one octave below the input. Requires SuperDirt's extra effects (sc3plugins).",
            params: &[
                ReferenceParam {
                    name: "value",
                    r#type: "number | Pattern",
                    description: "octave-down amount",
                },
            ],
            examples: &[],
            tags: &["superdirt", "osc"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["octersubsub"],
        aliases: &[],
        reference:         ReferenceEntry {
            name: "octersubsub",
            synonyms: &[],
            summary: "SuperDirt (OSC): two-octave-down level.",
            description: "SuperDirt via `.osc()`: mixes in sound two octaves below the input. Requires SuperDirt's extra effects (sc3plugins).",
            params: &[
                ReferenceParam {
                    name: "value",
                    r#type: "number | Pattern",
                    description: "two-octave-down amount",
                },
            ],
            examples: &[],
            tags: &["superdirt", "osc"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["ring"],
        aliases: &[],
        reference:         ReferenceEntry {
            name: "ring",
            synonyms: &[],
            summary: "SuperDirt (OSC): ring modulation.",
            description: "SuperDirt via `.osc()`: sets the modulation amount. `ringf` sets the starting frequency and `ringdf` its change. Requires SuperDirt's extra effects (sc3plugins).",
            params: &[
                ReferenceParam {
                    name: "value",
                    r#type: "number | Pattern",
                    description: "modulation amount",
                },
            ],
            examples: &[],
            tags: &["superdirt", "osc"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["ringf"],
        aliases: &[],
        reference:         ReferenceEntry {
            name: "ringf",
            synonyms: &[],
            summary: "SuperDirt (OSC): ring-modulator frequency.",
            description: "SuperDirt via `.osc()`: sets the starting modulation frequency in Hz. Use with `ring`. Requires SuperDirt's extra effects (sc3plugins).",
            params: &[
                ReferenceParam {
                    name: "value",
                    r#type: "number | Pattern",
                    description: "starting frequency in Hz",
                },
            ],
            examples: &[],
            tags: &["superdirt", "osc"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["ringdf"],
        aliases: &[],
        reference:         ReferenceEntry {
            name: "ringdf",
            synonyms: &[],
            summary: "SuperDirt (OSC): ring-modulator frequency change.",
            description: "SuperDirt via `.osc()`: changes the modulation frequency from `ringf` to `ringf + ringdf`. Use with `ring`. Requires SuperDirt's extra effects (sc3plugins).",
            params: &[
                ReferenceParam {
                    name: "value",
                    r#type: "number | Pattern",
                    description: "frequency change in Hz",
                },
            ],
            examples: &[],
            tags: &["superdirt", "osc"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["freeze"],
        aliases: &[],
        reference:         ReferenceEntry {
            name: "freeze",
            synonyms: &[],
            summary: "SuperDirt (OSC): spectral freeze.",
            description: "SuperDirt via `.osc()`: freezes the spectrum's magnitudes when positive. Requires SuperDirt's extra effects (sc3plugins).",
            params: &[
                ReferenceParam {
                    name: "value",
                    r#type: "number | Pattern",
                    description: "positive to freeze, zero to release",
                },
            ],
            examples: &[],
            tags: &["superdirt", "osc"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["xsdelay"],
        aliases: &[],
        reference:         ReferenceEntry {
            name: "xsdelay",
            synonyms: &[],
            summary: "SuperDirt (OSC): spectral delay pattern.",
            description: "SuperDirt via `.osc()`: sets the pattern of delays across frequency bands. `tsdelay` scales their duration. Requires SuperDirt's extra effects (sc3plugins).",
            params: &[
                ReferenceParam {
                    name: "value",
                    r#type: "number | Pattern",
                    description: "delay pattern value",
                },
            ],
            examples: &[],
            tags: &["superdirt", "osc"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["tsdelay"],
        aliases: &[],
        reference:         ReferenceEntry {
            name: "tsdelay",
            synonyms: &[],
            summary: "SuperDirt (OSC): spectral delay duration.",
            description: "SuperDirt via `.osc()`: scales the delay times across frequency bands. `xsdelay` sets their pattern. Requires SuperDirt's extra effects (sc3plugins).",
            params: &[
                ReferenceParam {
                    name: "value",
                    r#type: "number | Pattern",
                    description: "delay-time factor",
                },
            ],
            examples: &[],
            tags: &["superdirt", "osc"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["real"],
        aliases: &[],
        reference:         ReferenceEntry {
            name: "real",
            synonyms: &[],
            summary: "SuperDirt (OSC): spectral conformer, real part.",
            description: "SuperDirt via `.osc()`: sets the real component of the spectral conformer, which reshapes the spectrum together with `imag`. Requires SuperDirt's extra effects (sc3plugins).",
            params: &[
                ReferenceParam {
                    name: "value",
                    r#type: "number | Pattern",
                    description: "real component",
                },
            ],
            examples: &[],
            tags: &["superdirt", "osc"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["imag"],
        aliases: &[],
        reference:         ReferenceEntry {
            name: "imag",
            synonyms: &[],
            summary: "SuperDirt (OSC): spectral conformer, imaginary part.",
            description: "SuperDirt via `.osc()`: sets the imaginary component of the spectral conformer, which reshapes the spectrum together with `real`. Requires SuperDirt's extra effects (sc3plugins).",
            params: &[
                ReferenceParam {
                    name: "value",
                    r#type: "number | Pattern",
                    description: "imaginary component",
                },
            ],
            examples: &[],
            tags: &["superdirt", "osc"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["enhance"],
        aliases: &[],
        reference:         ReferenceEntry {
            name: "enhance",
            synonyms: &[],
            summary: "SuperDirt (OSC): spectral harmonic enhancement.",
            description: "SuperDirt via `.osc()`: adds and strengthens harmonics in the spectrum. Requires SuperDirt's extra effects (sc3plugins).",
            params: &[
                ReferenceParam {
                    name: "value",
                    r#type: "number | Pattern",
                    description: "enhancement amount",
                },
            ],
            examples: &[],
            tags: &["superdirt", "osc"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["comb"],
        aliases: &[],
        reference:         ReferenceEntry {
            name: "comb",
            synonyms: &[],
            summary: "SuperDirt (OSC): spectral comb filter.",
            description: "SuperDirt via `.osc()`: controls the spacing and width of a comb filter in the spectrum. Requires SuperDirt's extra effects (sc3plugins).",
            params: &[
                ReferenceParam {
                    name: "value",
                    r#type: "number | Pattern",
                    description: "comb amount",
                },
            ],
            examples: &[],
            tags: &["superdirt", "osc"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["smear"],
        aliases: &[],
        reference:         ReferenceEntry {
            name: "smear",
            synonyms: &[],
            summary: "SuperDirt (OSC): spectral smear.",
            description: "SuperDirt via `.osc()`: spreads spectral magnitudes across neighbouring frequency bins. Requires SuperDirt's extra effects (sc3plugins).",
            params: &[
                ReferenceParam {
                    name: "value",
                    r#type: "number | Pattern",
                    description: "smear amount",
                },
            ],
            examples: &[],
            tags: &["superdirt", "osc"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["scram"],
        aliases: &[],
        reference:         ReferenceEntry {
            name: "scram",
            synonyms: &[],
            summary: "SuperDirt (OSC): spectral scramble.",
            description: "SuperDirt via `.osc()`: scrambles the spectrum's frequency bins. Requires SuperDirt's extra effects (sc3plugins).",
            params: &[
                ReferenceParam {
                    name: "value",
                    r#type: "number | Pattern",
                    description: "scramble amount",
                },
            ],
            examples: &[],
            tags: &["superdirt", "osc"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["binshift"],
        aliases: &[],
        reference:         ReferenceEntry {
            name: "binshift",
            synonyms: &[],
            summary: "SuperDirt (OSC): spectral stretch and shift.",
            description: "SuperDirt via `.osc()`: stretches and shifts the spectrum's frequency bins. Requires SuperDirt's extra effects (sc3plugins).",
            params: &[
                ReferenceParam {
                    name: "value",
                    r#type: "number | Pattern",
                    description: "stretch and shift amount",
                },
            ],
            examples: &[],
            tags: &["superdirt", "osc"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["hbrick"],
        aliases: &[],
        reference:         ReferenceEntry {
            name: "hbrick",
            synonyms: &[],
            summary: "SuperDirt (OSC): spectral high-pass filter.",
            description: "SuperDirt via `.osc()`: removes the lower part of the spectrum with a brick-wall filter. Requires SuperDirt's extra effects (sc3plugins).",
            params: &[
                ReferenceParam {
                    name: "value",
                    r#type: "number | Pattern",
                    description: "filter amount from 0 to 1",
                },
            ],
            examples: &[],
            tags: &["superdirt", "osc"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["lbrick"],
        aliases: &[],
        reference:         ReferenceEntry {
            name: "lbrick",
            synonyms: &[],
            summary: "SuperDirt (OSC): spectral low-pass filter.",
            description: "SuperDirt via `.osc()`: removes the upper part of the spectrum with a brick-wall filter. Requires SuperDirt's extra effects (sc3plugins).",
            params: &[
                ReferenceParam {
                    name: "value",
                    r#type: "number | Pattern",
                    description: "filter amount from 0 to 1",
                },
            ],
            examples: &[],
            tags: &["superdirt", "osc"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["frameRate"],
        aliases: &[],
        reference:         ReferenceEntry {
            name: "frameRate",
            synonyms: &[],
            summary: "SuperDirt (OSC): MIDI timecode frame rate.",
            description: "SuperDirt via `.osc()`: sets the frame rate for `midicmd(\"smpte\")`. Requires a MIDI output configured in SuperDirt.",
            params: &[
                ReferenceParam {
                    name: "value",
                    r#type: "number | Pattern",
                    description: "MIDI frame-rate code from 0 to 3",
                },
            ],
            examples: &[],
            tags: &["superdirt", "osc"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["frames"],
        aliases: &[],
        reference:         ReferenceEntry {
            name: "frames",
            synonyms: &[],
            summary: "SuperDirt (OSC): MIDI timecode frames.",
            description: "SuperDirt via `.osc()`: sets the frame component for `midicmd(\"smpte\")`. Requires a MIDI output configured in SuperDirt.",
            params: &[
                ReferenceParam {
                    name: "value",
                    r#type: "number | Pattern",
                    description: "frame component",
                },
            ],
            examples: &[],
            tags: &["superdirt", "osc"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["hours"],
        aliases: &[],
        reference:         ReferenceEntry {
            name: "hours",
            synonyms: &[],
            summary: "SuperDirt (OSC): MIDI timecode hours.",
            description: "SuperDirt via `.osc()`: sets the hour component for `midicmd(\"smpte\")`. Requires a MIDI output configured in SuperDirt.",
            params: &[
                ReferenceParam {
                    name: "value",
                    r#type: "number | Pattern",
                    description: "hour component",
                },
            ],
            examples: &[],
            tags: &["superdirt", "osc"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["minutes"],
        aliases: &[],
        reference:         ReferenceEntry {
            name: "minutes",
            synonyms: &[],
            summary: "SuperDirt (OSC): MIDI timecode minutes.",
            description: "SuperDirt via `.osc()`: sets the minute component for `midicmd(\"smpte\")`. Requires a MIDI output configured in SuperDirt.",
            params: &[
                ReferenceParam {
                    name: "value",
                    r#type: "number | Pattern",
                    description: "minute component",
                },
            ],
            examples: &[],
            tags: &["superdirt", "osc"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["seconds"],
        aliases: &[],
        reference:         ReferenceEntry {
            name: "seconds",
            synonyms: &[],
            summary: "SuperDirt (OSC): MIDI timecode seconds.",
            description: "SuperDirt via `.osc()`: sets the second component for `midicmd(\"smpte\")`. Requires a MIDI output configured in SuperDirt.",
            params: &[
                ReferenceParam {
                    name: "value",
                    r#type: "number | Pattern",
                    description: "second component",
                },
            ],
            examples: &[],
            tags: &["superdirt", "osc"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["songPtr"],
        aliases: &[],
        reference:         ReferenceEntry {
            name: "songPtr",
            synonyms: &[],
            summary: "SuperDirt (OSC): MIDI song position.",
            description: "SuperDirt via `.osc()`: sets the position for `midicmd(\"songPtr\")`. Requires a MIDI output configured in SuperDirt.",
            params: &[
                ReferenceParam {
                    name: "value",
                    r#type: "number | Pattern",
                    description: "song position",
                },
            ],
            examples: &[],
            tags: &["superdirt", "osc"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["uid"],
        aliases: &[],
        reference:         ReferenceEntry {
            name: "uid",
            synonyms: &[],
            summary: "Event identity; stored without native processing.",
            description: "Rustel does not assign or read this identity automatically. The value can be sent through osc() or mapped to a MIDI CC with midimaps.",
            params: &[
                ReferenceParam {
                    name: "value",
                    r#type: "number | Pattern",
                    description: "an identity for the event",
                },
            ],
            examples: &[],
            tags: &["control", "osc"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["val"],
        aliases: &[],
        reference:         ReferenceEntry {
            name: "val",
            synonyms: &[],
            summary: "SuperDirt (OSC): MIDI message value.",
            description: "SuperDirt via `.osc()`: supplies the value for MIDI bend, touch or NRPN messages. Requires a MIDI output configured in SuperDirt.",
            params: &[
                ReferenceParam {
                    name: "value",
                    r#type: "number | Pattern",
                    description: "message value",
                },
            ],
            examples: &[],
            tags: &["superdirt", "osc"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["cps"],
        aliases: &[],
        reference:         ReferenceEntry {
            name: "cps",
            synonyms: &[],
            summary: "the tempo, in cycles per second - state of the engine, not of the event",
            description: "The free cps(value) form changes the session tempo atomically, like setCps(value), and may be placed in a labeled lane. The default is 0.5. At query time the scheduler injects _cps, which is what loopAt and glide read to tell a real trigger from a lookahead. A chained .cps(value) plays that pattern at the requested cycles per second.",
            params: &[ReferenceParam {
                name: "value",
                r#type: "number",
                description: "cycles per second",
            }],
            examples: &[
                "kick: s(\"bd*4\")\ntempochanges: cps(1).gain(0)",
            ],
            tags: &["control", "tempo"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["clip"],
        aliases: &["legato"],
        reference: ReferenceEntry {
            name: "clip",
            synonyms: &["legato"],
            summary: "Multiply the event duration and gate sample playback.",
            description: "A unitless multiplier, default 1, applied after duration (in cycles) or the event’s natural span. Synth envelopes and looping samples follow that effective duration. Naming clip, including clip(1), also makes ordinary samples and `gm_*` soundfont zones follow the event gate instead of their whole file or slice; their release may extend past the gate. Without clip, release, or a loop setting, a non-looping sample keeps its slice duration. sbd additionally caps its own stop time using clip; ZZFX raw parameter arrays keep their own generator durations.",
            params: &[
                ReferenceParam {
                    name: "factor",
                    r#type: "number | Pattern",
                    description: ">= 0",
                },
            ],
            examples: &[
                "note(\"c a f e\").s(\"piano\").clip(\"<.5 1 2>\")",
            ],
            tags: &["audio"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["duration"],
        aliases: &["dur"],
        reference: ReferenceEntry {
            name: "duration",
            synonyms: &["dur"],
            summary: "Set the event duration in musical cycles.",
            description: "Defaults to the event’s natural span. One cycle lasts 1/cps seconds; clip multiplies this duration afterward. Synth envelopes and already-gated samples follow it. On a non-looping recorded sample or `gm_*` zone, duration alone does not switch from whole-file/slice playback to event gating: add clip(1), release, or a loop setting. sbd uses its own decay/stop rule, and a raw zzfx array sets the generator’s own durations.",
            params: &[
                ReferenceParam {
                    name: "seconds",
                    r#type: "number | Pattern",
                    description: ">= 0",
                },
            ],
            examples: &[
                "note(\"c a f e\").s(\"piano\").dur(\"<.5 1 2>\")",
            ],
            tags: &["audio"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["zrand"],
        aliases: &[],
        reference:         ReferenceEntry {
            name: "zrand",
            synonyms: &[],
            summary: "ZZFX randomness - how far each hit strays from the recipe",
            description: "The amount the generator perturbs its own parameters, so the same pattern never lands twice alike. The generator's own default is 0.05, but here the default is 0: a zzfx voice is deterministic until you ask otherwise. One of the twenty parameters of the ZZFX generator behind s(\"zzfx\") and the z_* sounds, which resolve their own pitch - freq, or the note control, or C2. A raw zzfx([...]) array overrides every parameter positionally and wins over these controls.",
            params: &[
                ReferenceParam {
                    name: "value",
                    r#type: "number | Pattern",
                    description: "randomness 0..1; default 0 here",
                },
            ],
            examples: &[
                "s(\"zzfx*4\").note(\"c3\").zrand(\"0 0.2 0.5 1\")",
            ],
            tags: &["control", "synth"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["curve"],
        aliases: &[],
        reference:         ReferenceEntry {
            name: "curve",
            synonyms: &[],
            summary: "the ZZFX waveform's curve",
            description: "Shapes the contour of the generator's waveform; the default is 1. s(\"z_square\") forces it to 0 - the square there is the triangle shape through a flat curve. One of the twenty parameters of the ZZFX generator behind s(\"zzfx\") and the z_* sounds, which resolve their own pitch - freq, or the note control, or C2. A raw zzfx([...]) array overrides every parameter positionally and wins over these controls.",
            params: &[
                ReferenceParam {
                    name: "value",
                    r#type: "number | Pattern",
                    description: "shape curve; default 1",
                },
            ],
            examples: &[
                "s(\"z_sine*2\").note(\"c3 e3\").curve(\"0 0.5 1 2\")",
            ],
            tags: &["control", "synth"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["slide"],
        aliases: &[],
        reference:         ReferenceEntry {
            name: "slide",
            synonyms: &[],
            summary: "the ZZFX pitch slide",
            description: "Glides the generator's pitch across the note - positive climbs, negative falls; the default 0 plays straight. Applies to s(\"zzfx\") and the z_* sounds; the table declares it twice and both spellings are this one control. One of the twenty parameters of the ZZFX generator behind s(\"zzfx\") and the z_* sounds, which resolve their own pitch - freq, or the note control, or C2. A raw zzfx([...]) array overrides every parameter positionally and wins over these controls.",
            params: &[
                ReferenceParam {
                    name: "value",
                    r#type: "number | Pattern",
                    description: "slide amount; default 0",
                },
            ],
            examples: &[
                "s(\"zzfx\").note(\"c4\").slide(\"0 0.5 -0.5 1\")",
            ],
            tags: &["control", "synth"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["deltaSlide"],
        aliases: &[],
        reference:         ReferenceEntry {
            name: "deltaSlide",
            synonyms: &[],
            summary: "how fast the ZZFX slide itself changes",
            description: "The slide's own acceleration: where slide glides at a steady rate, deltaSlide bends that rate over the note. Default 0. One of the twenty parameters of the ZZFX generator behind s(\"zzfx\") and the z_* sounds, which resolve their own pitch - freq, or the note control, or C2. A raw zzfx([...]) array overrides every parameter positionally and wins over these controls.",
            params: &[
                ReferenceParam {
                    name: "value",
                    r#type: "number | Pattern",
                    description: "slide acceleration; default 0",
                },
            ],
            examples: &[
                "s(\"zzfx\").note(\"c4\").deltaSlide(\"0 0.2 -0.2\")",
            ],
            tags: &["control", "synth"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["pitchJump"],
        aliases: &[],
        reference:         ReferenceEntry {
            name: "pitchJump",
            synonyms: &[],
            summary: "an interval the ZZFX pitch jumps by mid-note",
            description: "Moves the generator's pitch by the amount, once, during the note; pitchJumpTime says when. Default 0: no jump. One of the twenty parameters of the ZZFX generator behind s(\"zzfx\") and the z_* sounds, which resolve their own pitch - freq, or the note control, or C2. A raw zzfx([...]) array overrides every parameter positionally and wins over these controls.",
            params: &[
                ReferenceParam {
                    name: "value",
                    r#type: "number | Pattern",
                    description: "jump amount; default 0",
                },
            ],
            examples: &[
                "s(\"zzfx*2\").note(\"c3\").pitchJump(\"0 4 0 7\")",
            ],
            tags: &["control", "synth"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["pitchJumpTime"],
        aliases: &[],
        reference:         ReferenceEntry {
            name: "pitchJumpTime",
            synonyms: &[],
            summary: "when the ZZFX pitch jump happens",
            description: "The point in the note, toward its length, at which pitchJump lands. Default 0. One of the twenty parameters of the ZZFX generator behind s(\"zzfx\") and the z_* sounds, which resolve their own pitch - freq, or the note control, or C2. A raw zzfx([...]) array overrides every parameter positionally and wins over these controls.",
            params: &[
                ReferenceParam {
                    name: "value",
                    r#type: "number | Pattern",
                    description: "jump time; default 0",
                },
            ],
            examples: &[
                "s(\"zzfx*2\").note(\"c3\").pitchJump(5).pitchJumpTime(\"0 0.5\")",
            ],
            tags: &["control", "synth"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["znoise"],
        aliases: &[],
        reference:         ReferenceEntry {
            name: "znoise",
            synonyms: &[],
            summary: "ZZFX's noise mix - not the noise control",
            description: "Mixes noise into the generator's output; the default is 0. The plain noise control is the oscillators' pink-noise mix and never reaches the zzfx voice - this spelling is the one it reads. One of the twenty parameters of the ZZFX generator behind s(\"zzfx\") and the z_* sounds, which resolve their own pitch - freq, or the note control, or C2. A raw zzfx([...]) array overrides every parameter positionally and wins over these controls.",
            params: &[
                ReferenceParam {
                    name: "value",
                    r#type: "number | Pattern",
                    description: "noise mix 0..1; default 0",
                },
            ],
            examples: &[
                "s(\"z_sine*4\").note(\"c3\").znoise(\"0 0.3 0.6 1\")",
            ],
            tags: &["control", "synth"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["zmod"],
        aliases: &[],
        reference:         ReferenceEntry {
            name: "zmod",
            synonyms: &[],
            summary: "ZZFX's modulation amount",
            description: "Pitch modulation inside the generator; default 0. One of the twenty parameters of the ZZFX generator behind s(\"zzfx\") and the z_* sounds, which resolve their own pitch - freq, or the note control, or C2. A raw zzfx([...]) array overrides every parameter positionally and wins over these controls.",
            params: &[
                ReferenceParam {
                    name: "value",
                    r#type: "number | Pattern",
                    description: "modulation amount; default 0",
                },
            ],
            examples: &[
                "s(\"z_sine*2\").note(\"c3\").zmod(\"0 20 60\")",
            ],
            tags: &["control", "synth"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["zcrush"],
        aliases: &[],
        reference:         ReferenceEntry {
            name: "zcrush",
            synonyms: &[],
            summary: "ZZFX's bit-crush",
            description: "Bit-depth reduction inside the generator itself; default 0 is uncrushed. For the engine-wide effect, use crush. One of the twenty parameters of the ZZFX generator behind s(\"zzfx\") and the z_* sounds, which resolve their own pitch - freq, or the note control, or C2. A raw zzfx([...]) array overrides every parameter positionally and wins over these controls.",
            params: &[
                ReferenceParam {
                    name: "value",
                    r#type: "number | Pattern",
                    description: "crush amount; default 0",
                },
            ],
            examples: &[
                "s(\"zzfx*4\").note(\"c3\").zcrush(\"0 2 8 32\")",
            ],
            tags: &["control", "synth"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["zdelay"],
        aliases: &[],
        reference:         ReferenceEntry {
            name: "zdelay",
            synonyms: &[],
            summary: "ZZFX's built-in delay",
            description: "The generator's own delay stage, folded into the voice; default 0. For the engine's delay send, use delay. One of the twenty parameters of the ZZFX generator behind s(\"zzfx\") and the z_* sounds, which resolve their own pitch - freq, or the note control, or C2. A raw zzfx([...]) array overrides every parameter positionally and wins over these controls.",
            params: &[
                ReferenceParam {
                    name: "value",
                    r#type: "number | Pattern",
                    description: "delay amount; default 0",
                },
            ],
            examples: &[
                "s(\"zzfx\").note(\"c3 e3\").zdelay(\"0 0.5\")",
            ],
            tags: &["control", "synth"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["zzfx"],
        aliases: &[],
        reference:         ReferenceEntry {
            name: "zzfx",
            synonyms: &[],
            summary: "the raw ZZFX parameter array - overrides everything",
            description: "The z_* sounds and s(\"zzfx\") build their voice from the ZZFX generator. The generator has twenty parameters. zzfx([...]) sets them in order, first to last. A short array keeps the generator's defaults for the rest. A long array is cut at twenty. The array overrides every control, zrand to zdelay. With no array, each control writes its own slot.\n\n1 `volume` - how loud the voice is. Default 1.\n\n2 `randomness` - how far each hit strays from the recipe. Default .05.\n\n3 `frequency` - the pitch, in Hz. Default 220.\n\n4 `attack` - the fade in, in seconds. Default 0.\n\n5 `sustain` - the hold, in seconds. Default 0.\n\n6 `release` - the fade out, in seconds. Default .1.\n\n7 `shape` - the wave: 0 sine, 1 triangle, 2 saw, 3 tan, 4 noise. -1 is the triangle that z_square turns square. Default 0.\n\n8 `shapeCurve` - how flat or pointed the wave is. Default 1.\n\n9 `slide` - the pitch glide across the note. Default 0.\n\n10 `deltaSlide` - how the glide itself changes. Default 0.\n\n11 `pitchJump` - one pitch step during the note. Default 0.\n\n12 `pitchJumpTime` - when the jump lands. Default 0.\n\n13 `repeatTime` - seconds before the pitch starts over. Default 0.\n\n14 `noise` - how much noise mixes in. Default 0.\n\n15 `modulation` - how deep the pitch warble is. Default 0.\n\n16 `bitCrush` - how coarse the sound is. Default 0.\n\n17 `delay` - seconds of echo. Default 0.\n\n18 `sustainVolume` - how loud the hold is. Default 1.\n\n19 `decay` - seconds from the attack's top down to the hold. Default 0.\n\n20 `tremolo` - how deep the loudness wobbles. Default 0.\n\nWhile the array plays, the `frequency` slot is the pitch. `note` and `freq` set nothing of it. A note still sets the rhythm.\n\nWith no array, these controls write the same slots: `attack`, `decay`, `release`, `slide`, `deltaSlide`, `pitchJump`, `pitchJumpTime`, `tremolo`, `curve` (shapeCurve), `zrand`, `znoise`, `zmod`, `zcrush`, `zdelay`, and `lfo` (repeatTime). On that path, volume is fixed at 0.25, `sustain` sets sustainVolume, and the note's own length is the hold time. The sound's name picks the shape.",
            params: &[
                ReferenceParam {
                    name: "value",
                    r#type: "number[] | Pattern",
                    description: "up to twenty numbers, in the generator's order - volume first, tremolo last; defaults fill the rest",
                },
            ],
            examples: &[
                "s(\"zzfx\").note(\"c3 eb3 g3\").zzfx([1, .05, 220, 0, 0, .1, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 0, 0])",
                "s(\"zzfx*4\").zzfx([1, .05, 220, 0, 0, .1, 2])",
            ],
            tags: &["control", "synth"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["color", "colour"],
        aliases: &[],
        reference: ReferenceEntry {
            name: "color",
            synonyms: &["colour"],
            summary: "Sets the color of the hap in visualizations like pianoroll or highlighting.",
            description: "Sets the color of the hap in visualizations like pianoroll or highlighting.",
            params: &[
                ReferenceParam {
                    name: "color",
                    r#type: "string",
                    description: "Hexadecimal or CSS color name",
                },
            ],
            examples: &[],
            tags: &["visualization"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["midichan"],
        aliases: &[],
        reference: ReferenceEntry {
            name: "midichan",
            synonyms: &[],
            summary: "MIDI channel: Sets the MIDI channel for the event.",
            description: "MIDI channel: Sets the MIDI channel for the event.",
            params: &[
                ReferenceParam {
                    name: "channel",
                    r#type: "number | Pattern",
                    description: "MIDI channel number (0-15)",
                },
            ],
            examples: &[
                "note(\"c4\").midichan(1).midi()",
            ],
            tags: &["external_io", "midi"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["midimap"],
        aliases: &[],
        reference:         ReferenceEntry {
            name: "midimap",
            synonyms: &[],
            summary: "pick which control-to-CC map a MIDI-bound pattern sends",
            description: "A named map - registered with midimaps - turns the controls a hap carries into CC messages, each value normalised into the entry's range. The name must be a string; null and numbers select nothing, and a hap naming a map that was never registered sends no mapped CCs at all.\n\nA pattern with no midimap falls back to the midimap option of its .midi(port, { midimap }) call, and then to the \"default\" map that defaultmidimap sets. Orthogonal to midichan and midiport, which pick the channel and the port the CCs go out on.",
            params: &[
                ReferenceParam {
                    name: "name",
                    r#type: "string | Pattern",
                    description: "the registered map to use",
                },
            ],
            examples: &[
                "midimaps({ lead: { lpf: 74 } })\nnote(\"c3 e3 g3\").midi().midimap(\"lead\").lpf(\"400 800 1600 3200\")",
            ],
            tags: &["control", "midi"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["midiport"],
        aliases: &[],
        reference: ReferenceEntry {
            name: "midiport",
            synonyms: &[],
            summary: "MIDI port: Sets the MIDI port for the event.",
            description: "MIDI port: Sets the MIDI port for the event.",
            params: &[
                ReferenceParam {
                    name: "port",
                    r#type: "number | Pattern",
                    description: "MIDI port",
                },
            ],
            examples: &[
                "note(\"c a f e\").midiport(\"<0 1 2 3>\").midi()",
            ],
            tags: &["external_io", "midi"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["midicmd"],
        aliases: &[],
        reference: ReferenceEntry {
            name: "midicmd",
            synonyms: &[],
            summary: "MIDI command: Sends a MIDI command message.",
            description: "MIDI command: Sends a MIDI command message.",
            params: &[
                ReferenceParam {
                    name: "command",
                    r#type: "number | Pattern",
                    description: "MIDI command",
                },
            ],
            examples: &[
                "midicmd(\"clock*48,<start stop>/2\").midi()",
            ],
            tags: &["external_io", "midi"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["ccn"],
        aliases: &[],
        reference: ReferenceEntry {
            name: "ccn",
            synonyms: &[],
            summary: "MIDI control number: Sends a MIDI control change message.",
            description: "MIDI control number: Sends a MIDI control change message.",
            params: &[
                ReferenceParam {
                    name: "MIDI",
                    r#type: "number | Pattern",
                    description: "control number (0-127)",
                },
            ],
            examples: &[],
            tags: &["external_io", "midi"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["ccv"],
        aliases: &[],
        reference: ReferenceEntry {
            name: "ccv",
            synonyms: &[],
            summary: "MIDI control value: Sends a MIDI control change message.",
            description: "MIDI control value: Sends a MIDI control change message.",
            params: &[
                ReferenceParam {
                    name: "MIDI",
                    r#type: "number | Pattern",
                    description: "control value (0-127)",
                },
            ],
            examples: &[],
            tags: &["external_io", "midi"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["ctlNum"],
        aliases: &[],
        reference:         ReferenceEntry {
            name: "ctlNum",
            synonyms: &[],
            summary: "SuperDirt's spelling of the MIDI CC number - ccn wins when both are set",
            description: "MIDI output only: when a hap carries ccv but no ccn, ctlNum supplies the controller number. A CC goes out only when the number is a finite integer in 0..127 and a value is present too.",
            params: &[
                ReferenceParam {
                    name: "value",
                    r#type: "number | Pattern",
                    description: "controller number 0..127",
                },
            ],
            examples: &[
                "note(\"a3\").midi().ctlNum(1).ccv(\"<0 64 127>\")",
            ],
            tags: &["control", "midi"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["nrpnn"],
        aliases: &[],
        reference: ReferenceEntry {
            name: "nrpnn",
            synonyms: &[],
            summary: "MIDI NRPN non-registered parameter number: Sends a MIDI NRPN non-registered parameter number message.",
            description: "MIDI NRPN non-registered parameter number: Sends a MIDI NRPN non-registered parameter number message.",
            params: &[
                ReferenceParam {
                    name: "nrpnn",
                    r#type: "number | Pattern",
                    description: "MIDI NRPN non-registered parameter number (0-127)",
                },
            ],
            examples: &[
                "note(\"c4\").nrpnn(\"1:8\").nrpv(\"123\").midichan(1).midi()",
            ],
            tags: &["external_io", "midi"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["nrpv"],
        aliases: &[],
        reference: ReferenceEntry {
            name: "nrpv",
            synonyms: &[],
            summary: "MIDI NRPN non-registered parameter value: Sends a MIDI NRPN non-registered parameter value message.",
            description: "MIDI NRPN non-registered parameter value: Sends a MIDI NRPN non-registered parameter value message.",
            params: &[
                ReferenceParam {
                    name: "nrpv",
                    r#type: "number | Pattern",
                    description: "MIDI NRPN non-registered parameter value (0-127)",
                },
            ],
            examples: &[
                "note(\"c4\").nrpnn(\"1:8\").nrpv(\"123\").midichan(1).midi()",
            ],
            tags: &["external_io", "midi"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["progNum"],
        aliases: &[],
        reference: ReferenceEntry {
            name: "progNum",
            synonyms: &[],
            summary: "MIDI program number: Sends a MIDI program change message.",
            description: "MIDI program number: Sends a MIDI program change message.",
            params: &[
                ReferenceParam {
                    name: "program",
                    r#type: "number | Pattern",
                    description: "MIDI program number (0-127)",
                },
            ],
            examples: &[
                "note(\"c4\").progNum(10).midichan(1).midi()",
            ],
            tags: &["external_io"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["sysexid"],
        aliases: &[],
        reference: ReferenceEntry {
            name: "sysexid",
            synonyms: &[],
            summary: "MIDI sysex ID: Sends a MIDI sysex identifier message.",
            description: "MIDI sysex ID: Sends a MIDI sysex identifier message.",
            params: &[
                ReferenceParam {
                    name: "id",
                    r#type: "number | Pattern",
                    description: "Sysex ID",
                },
            ],
            examples: &[
                "note(\"c4\").sysexid(\"0x77\").sysexdata(\"0x01:0x02:0x03:0x04\").midichan(1).midi()",
            ],
            tags: &["external_io", "midi"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["sysexdata"],
        aliases: &[],
        reference: ReferenceEntry {
            name: "sysexdata",
            synonyms: &[],
            summary: "MIDI sysex data: Sends a MIDI sysex message.",
            description: "MIDI sysex data: Sends a MIDI sysex message.",
            params: &[
                ReferenceParam {
                    name: "data",
                    r#type: "number | Pattern",
                    description: "Sysex data",
                },
            ],
            examples: &[
                "note(\"c4\").sysexid(\"0x77\").sysexdata(\"0x01:0x02:0x03:0x04\").midichan(1).midi()",
            ],
            tags: &["external_io", "midi"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["midibend"],
        aliases: &[],
        reference: ReferenceEntry {
            name: "midibend",
            synonyms: &[],
            summary: "MIDI pitch bend: Sends a MIDI pitch bend message.",
            description: "MIDI pitch bend: Sends a MIDI pitch bend message.",
            params: &[
                ReferenceParam {
                    name: "midibend",
                    r#type: "number | Pattern",
                    description: "MIDI pitch bend (-1 - 1)",
                },
            ],
            examples: &[
                "note(\"c4\").midibend(sine.slow(4).range(-0.4,0.4)).midi()",
            ],
            tags: &["external_io", "midi"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["miditouch"],
        aliases: &[],
        reference: ReferenceEntry {
            name: "miditouch",
            synonyms: &[],
            summary: "MIDI key after touch: Sends a MIDI key after touch message.",
            description: "MIDI key after touch: Sends a MIDI key after touch message.",
            params: &[
                ReferenceParam {
                    name: "miditouch",
                    r#type: "number | Pattern",
                    description: "MIDI key after touch (0-1)",
                },
            ],
            examples: &[
                "note(\"c4\").miditouch(sine.slow(4).range(0,1)).midi()",
            ],
            tags: &["external_io", "midi"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["polyTouch"],
        aliases: &[],
        reference:         ReferenceEntry {
            name: "polyTouch",
            synonyms: &[],
            summary: "polyphonic aftertouch - installed, but this engine's MIDI output never sends it",
            description: "Channel aftertouch goes out as miditouch; per-note pressure has no output path here. The value rides the event and leaves the engine through .osc(), and a midimaps entry can turn it into a CC; natively it is silent.",
            params: &[
                ReferenceParam {
                    name: "value",
                    r#type: "number | Pattern",
                    description: "per-note pressure",
                },
            ],
            examples: &[
                "note(\"c4 e4\").midi().polyTouch(\"<0 127>\")",
            ],
            tags: &["control", "midi"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["oschost"],
        aliases: &[],
        reference: ReferenceEntry {
            name: "oschost",
            synonyms: &[],
            summary: "The destination host for OSC messages.",
            description: "Sends OSC messages directly over UDP to this host. Defaults to 127.0.0.1. Use an IP address or localhost; other hostnames are not resolved during playback. Non-loopback destinations require an --allow-osc-host grant.",
            params: &[
                ReferenceParam {
                    name: "oschost",
                    r#type: "string | Pattern",
                    description: "IP address or localhost; default '127.0.0.1'",
                },
            ],
            examples: &[
                "note(\"c4\").oschost('127.0.0.1').oscport(57120).osc();",
            ],
            tags: &["external_io"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["oscport"],
        aliases: &[],
        reference: ReferenceEntry {
            name: "oscport",
            synonyms: &[],
            summary: "The destination UDP port for OSC messages.",
            description: "Sets the destination port for OSC messages sent directly over UDP. A per-event oscport takes precedence over the port supplied to osc().",
            params: &[
                ReferenceParam {
                    name: "oscport",
                    r#type: "number | Pattern",
                    description: "Whole number from 1 to 65535; osc() defaults to 57120.",
                },
            ],
            examples: &[
                "note(\"c4\").oschost('127.0.0.1').oscport(57120).osc();",
            ],
            tags: &["external_io"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["transient", "transsustain"],
        aliases: &[],
        reference: ReferenceEntry {
            name: "transient",
            synonyms: &["transsustain"],
            summary: "Shape a voice's attacks and sustained body independently.",
            description: "Absent by default. transient sets attack emphasis and creates the shaper even at 0; transsustain defaults to 0. The compound form transient(\"attack:sustain\") sets both. Values usually range from -1 (reduce) to 1 (emphasize). transsustain alone does not create the shaper; use transient(0) to change only sustain. Applies to any native audio source, including oscillators, wavetables, samples, `gm_*` soundfonts, and live input.",
            params: &[
                ReferenceParam {
                    name: "attack",
                    r#type: "number | Pattern",
                    description: "Emphasis on transients; between -1 (deaccentuate) and 1 (accentuate)",
                },
                ReferenceParam {
                    name: "sustain",
                    r#type: "number | Pattern",
                    description: "Emphasis on the sustains; between -1 (deaccentuate) and 1 (accentuate)",
                },
            ],
            examples: &[
                "s(\"bd\").transient(\"<-1 -0.5 0 0.5 1>\")",
                "s(\"hh*16\").bank(\"tr909\").transient(\"<-1:1 1:-1>\")",
            ],
            tags: &["audio"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["FXrelease"],
        aliases: &["FXrel", "FXr", "fxr"],
        reference:         ReferenceEntry {
            name: "FXrelease",
            synonyms: &["FXrel", "FXr", "fxr"],
            summary: "how long the modulators outlive the note",
            description: "Not the note's own release - that is release, and the envelope resolves it with a floor of 0.01 seconds. FXrelease extends how long the event's LFOs and modulators keep running after the note ends, and the two are maxed together, so a small FXrelease changes nothing. The default is 0: modulators stop with the note. Reach for it when a vibrato or a filter LFO should keep breathing after short notes.",
            params: &[
                ReferenceParam {
                    name: "value",
                    r#type: "number | Pattern",
                    description: "extra modulator lifetime in seconds; default 0",
                },
            ],
            examples: &[
                "note(\"c2 eb2 g2\").s(\"sawtooth\").vib(6).release(0.1).FXrelease(1)",
            ],
            tags: &["control", "envelope"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["fmh", "fmi"],
        aliases: &["fmh", "fmh1", "fmh1", "fmi1"],
        reference: ReferenceEntry {
            name: "fmh",
            synonyms: &[],
            summary: "FM operator 1 harmonicity ratio.",
            description: "Defaults to 1. For an oscillator operator, 2 doubles its frequency, while non-integer ratios create inharmonic sidebands. For a noise operator, harmonicity scales the outgoing modulation depth without changing the noise playback pitch. fmh2 through fmh8 address the other operators. Requires a nonzero FM route involving operator 1 and a connected path to the carrier; fmi (alias fm) supplies the direct route. The FM matrix affects basic oscillators, supersaw, pulse, and bytebeat. Recorded samples, `gm_*` soundfonts, `wt_*` wavetables, ZZFX, sbd, standalone noise, live input, and buses ignore it.",
            params: &[
                ReferenceParam {
                    name: "harmonicity",
                    r#type: "number | Pattern",
                    description: "",
                },
            ],
            examples: &[
                "note(\"c e g b g e\")\n.fm(4)\n.fmh(\"<1 2 1.5 1.61>\")\n._scope()",
            ],
            tags: &["fm", "audio"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["fmh2", "fmi2"],
        aliases: &["fmh2"],
        reference:         ReferenceEntry {
            name: "fmh2",
            synonyms: &[],
            summary: "operator 2's harmonicity ratio",
            description: "How operator 2's frequency relates to the carrier's: 1 tracks it, 2 doubles it, 1.5 lands a fifth above, and inharmonic decimals like 1.61 read as metallic. The default is 1. Whole numbers and simple ratios sound natural; the operator must exist - a route naming it - before this has anything to bend.",
            params: &[
                ReferenceParam {
                    name: "value",
                    r#type: "number | Pattern",
                    description: "harmonicity ratio; default 1",
                },
            ],
            examples: &[
                "note(\"c2 e2 g2\").s(\"sine\").fm2(2).fmh2(\"<1 2 1.5 1.61>\")",
            ],
            tags: &["control", "fm"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["fmh3", "fmi3"],
        aliases: &["fmh3"],
        reference:         ReferenceEntry {
            name: "fmh3",
            synonyms: &[],
            summary: "operator 3's harmonicity ratio",
            description: "How operator 3's frequency relates to the carrier's: 1 tracks it, 2 doubles it, 1.5 lands a fifth above, and inharmonic decimals like 1.61 read as metallic. The default is 1. Whole numbers and simple ratios sound natural; the operator must exist - a route naming it - before this has anything to bend.",
            params: &[
                ReferenceParam {
                    name: "value",
                    r#type: "number | Pattern",
                    description: "harmonicity ratio; default 1",
                },
            ],
            examples: &[
                "note(\"c2 e2 g2\").s(\"sine\").fm3(2).fmh3(\"<1 2 1.5 1.61>\")",
            ],
            tags: &["control", "fm"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["fmh4", "fmi4"],
        aliases: &["fmh4"],
        reference:         ReferenceEntry {
            name: "fmh4",
            synonyms: &[],
            summary: "operator 4's harmonicity ratio",
            description: "How operator 4's frequency relates to the carrier's: 1 tracks it, 2 doubles it, 1.5 lands a fifth above, and inharmonic decimals like 1.61 read as metallic. The default is 1. Whole numbers and simple ratios sound natural; the operator must exist - a route naming it - before this has anything to bend.",
            params: &[
                ReferenceParam {
                    name: "value",
                    r#type: "number | Pattern",
                    description: "harmonicity ratio; default 1",
                },
            ],
            examples: &[
                "note(\"c2 e2 g2\").s(\"sine\").fm4(2).fmh4(\"<1 2 1.5 1.61>\")",
            ],
            tags: &["control", "fm"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["fmh5", "fmi5"],
        aliases: &["fmh5"],
        reference:         ReferenceEntry {
            name: "fmh5",
            synonyms: &[],
            summary: "operator 5's harmonicity ratio",
            description: "How operator 5's frequency relates to the carrier's: 1 tracks it, 2 doubles it, 1.5 lands a fifth above, and inharmonic decimals like 1.61 read as metallic. The default is 1. Whole numbers and simple ratios sound natural; the operator must exist - a route naming it - before this has anything to bend.",
            params: &[
                ReferenceParam {
                    name: "value",
                    r#type: "number | Pattern",
                    description: "harmonicity ratio; default 1",
                },
            ],
            examples: &[
                "note(\"c2 e2 g2\").s(\"sine\").fm5(2).fmh5(\"<1 2 1.5 1.61>\")",
            ],
            tags: &["control", "fm"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["fmh6", "fmi6"],
        aliases: &["fmh6"],
        reference:         ReferenceEntry {
            name: "fmh6",
            synonyms: &[],
            summary: "operator 6's harmonicity ratio",
            description: "How operator 6's frequency relates to the carrier's: 1 tracks it, 2 doubles it, 1.5 lands a fifth above, and inharmonic decimals like 1.61 read as metallic. The default is 1. Whole numbers and simple ratios sound natural; the operator must exist - a route naming it - before this has anything to bend.",
            params: &[
                ReferenceParam {
                    name: "value",
                    r#type: "number | Pattern",
                    description: "harmonicity ratio; default 1",
                },
            ],
            examples: &[
                "note(\"c2 e2 g2\").s(\"sine\").fm6(2).fmh6(\"<1 2 1.5 1.61>\")",
            ],
            tags: &["control", "fm"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["fmh7", "fmi7"],
        aliases: &["fmh7"],
        reference:         ReferenceEntry {
            name: "fmh7",
            synonyms: &[],
            summary: "operator 7's harmonicity ratio",
            description: "How operator 7's frequency relates to the carrier's: 1 tracks it, 2 doubles it, 1.5 lands a fifth above, and inharmonic decimals like 1.61 read as metallic. The default is 1. Whole numbers and simple ratios sound natural; the operator must exist - a route naming it - before this has anything to bend.",
            params: &[
                ReferenceParam {
                    name: "value",
                    r#type: "number | Pattern",
                    description: "harmonicity ratio; default 1",
                },
            ],
            examples: &[
                "note(\"c2 e2 g2\").s(\"sine\").fm7(2).fmh7(\"<1 2 1.5 1.61>\")",
            ],
            tags: &["control", "fm"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["fmh8", "fmi8"],
        aliases: &["fmh8"],
        reference:         ReferenceEntry {
            name: "fmh8",
            synonyms: &[],
            summary: "operator 8's harmonicity ratio",
            description: "How operator 8's frequency relates to the carrier's: 1 tracks it, 2 doubles it, 1.5 lands a fifth above, and inharmonic decimals like 1.61 read as metallic. The default is 1. Whole numbers and simple ratios sound natural; the operator must exist - a route naming it - before this has anything to bend.",
            params: &[
                ReferenceParam {
                    name: "value",
                    r#type: "number | Pattern",
                    description: "harmonicity ratio; default 1",
                },
            ],
            examples: &[
                "note(\"c2 e2 g2\").s(\"sine\").fm8(2).fmh8(\"<1 2 1.5 1.61>\")",
            ],
            tags: &["control", "fm"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["fmi", "fmh"],
        aliases: &["fm", "fm1", "fmi1", "fmh1"],
        reference: ReferenceEntry {
            name: "fmi",
            synonyms: &["fm", "fm1", "fmi1", "fmh1"],
            summary: "Modulation index from FM operator 1 into the carrier.",
            description: "The unitless index sets brightness: the frequency deviation scales with carrier frequency and operator harmonicity. Absent or 0 makes no connection. fmi2/fm2 sends operator 2 into operator 1, continuing up to operator 8; a missing link breaks the chain. Matrix routes such as fmi20 send an operator directly to another target (0 is the carrier). At most 16 nonzero routes are accepted. The FM matrix affects basic oscillators, supersaw, pulse, and bytebeat. Recorded samples, `gm_*` soundfonts, `wt_*` wavetables, ZZFX, sbd, standalone noise, live input, and buses ignore it.",
            params: &[
                ReferenceParam {
                    name: "brightness",
                    r#type: "number | Pattern",
                    description: "modulation index",
                },
            ],
            examples: &[
                "note(\"c e g b g e\")\n.fm(\"<0 1 2 8 32>\")\n._scope()",
                "s(\"sine\").note(\"F1\").seg(8)\n .fm(4).fm2(rand.mul(4)).fm3(saw.mul(8).slow(8))\n .fmh(1.06).fmh2(10).fmh3(0.1)",
            ],
            tags: &["fm", "audio"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["fmi2", "fmh2"],
        aliases: &["fm2"],
        reference:         ReferenceEntry {
            name: "fmi2",
            synonyms: &["fm2"],
            summary: "how hard operator 2 modulates operator 1",
            description: "The plain-chain route, also spelled fm2: operator 2 into the one below it on the way to the carrier. The value is the modulation index - brightness - and zero or absent makes no route, which severs the chain at that link. The value is a modulation depth - an index, not hertz: the deviation it produces scales with the carrier and the source's harmonicity. A zero or absent amount makes no connection at all, and a gap severs the chain rather than being stepped over. Sixteen routes per event is the ceiling; beyond it the event is refused. The matrix bends the oscillator voices - sine, triangle, square, sawtooth, supersaw, pulse, bytebeat; samples, wavetables and the zzfx family ignore it.",
            params: &[
                ReferenceParam {
                    name: "value",
                    r#type: "number | Pattern",
                    description: "modulation index; 0 or absent = no route",
                },
            ],
            examples: &[
                "note(\"c2 e2 g2\").s(\"sine\").fm(2).fm2(\"<0 1 4 8>\")",
            ],
            tags: &["control", "fm"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["fmi3", "fmh3"],
        aliases: &["fm3"],
        reference:         ReferenceEntry {
            name: "fmi3",
            synonyms: &["fm3"],
            summary: "how hard operator 3 modulates operator 2",
            description: "The plain-chain route, also spelled fm3: operator 3 into the one below it on the way to the carrier. The value is the modulation index - brightness - and zero or absent makes no route, which severs the chain at that link. The value is a modulation depth - an index, not hertz: the deviation it produces scales with the carrier and the source's harmonicity. A zero or absent amount makes no connection at all, and a gap severs the chain rather than being stepped over. Sixteen routes per event is the ceiling; beyond it the event is refused. The matrix bends the oscillator voices - sine, triangle, square, sawtooth, supersaw, pulse, bytebeat; samples, wavetables and the zzfx family ignore it.",
            params: &[
                ReferenceParam {
                    name: "value",
                    r#type: "number | Pattern",
                    description: "modulation index; 0 or absent = no route",
                },
            ],
            examples: &[
                "note(\"c2 e2 g2\").s(\"sine\").fm(2).fm3(\"<0 1 4 8>\")",
            ],
            tags: &["control", "fm"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["fmi4", "fmh4"],
        aliases: &["fm4"],
        reference:         ReferenceEntry {
            name: "fmi4",
            synonyms: &["fm4"],
            summary: "how hard operator 4 modulates operator 3",
            description: "The plain-chain route, also spelled fm4: operator 4 into the one below it on the way to the carrier. The value is the modulation index - brightness - and zero or absent makes no route, which severs the chain at that link. The value is a modulation depth - an index, not hertz: the deviation it produces scales with the carrier and the source's harmonicity. A zero or absent amount makes no connection at all, and a gap severs the chain rather than being stepped over. Sixteen routes per event is the ceiling; beyond it the event is refused. The matrix bends the oscillator voices - sine, triangle, square, sawtooth, supersaw, pulse, bytebeat; samples, wavetables and the zzfx family ignore it.",
            params: &[
                ReferenceParam {
                    name: "value",
                    r#type: "number | Pattern",
                    description: "modulation index; 0 or absent = no route",
                },
            ],
            examples: &[
                "note(\"c2 e2 g2\").s(\"sine\").fm(2).fm4(\"<0 1 4 8>\")",
            ],
            tags: &["control", "fm"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["fmi5", "fmh5"],
        aliases: &["fm5"],
        reference:         ReferenceEntry {
            name: "fmi5",
            synonyms: &["fm5"],
            summary: "how hard operator 5 modulates operator 4",
            description: "The plain-chain route, also spelled fm5: operator 5 into the one below it on the way to the carrier. The value is the modulation index - brightness - and zero or absent makes no route, which severs the chain at that link. The value is a modulation depth - an index, not hertz: the deviation it produces scales with the carrier and the source's harmonicity. A zero or absent amount makes no connection at all, and a gap severs the chain rather than being stepped over. Sixteen routes per event is the ceiling; beyond it the event is refused. The matrix bends the oscillator voices - sine, triangle, square, sawtooth, supersaw, pulse, bytebeat; samples, wavetables and the zzfx family ignore it.",
            params: &[
                ReferenceParam {
                    name: "value",
                    r#type: "number | Pattern",
                    description: "modulation index; 0 or absent = no route",
                },
            ],
            examples: &[
                "note(\"c2 e2 g2\").s(\"sine\").fm(2).fm5(\"<0 1 4 8>\")",
            ],
            tags: &["control", "fm"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["fmi6", "fmh6"],
        aliases: &["fm6"],
        reference:         ReferenceEntry {
            name: "fmi6",
            synonyms: &["fm6"],
            summary: "how hard operator 6 modulates operator 5",
            description: "The plain-chain route, also spelled fm6: operator 6 into the one below it on the way to the carrier. The value is the modulation index - brightness - and zero or absent makes no route, which severs the chain at that link. The value is a modulation depth - an index, not hertz: the deviation it produces scales with the carrier and the source's harmonicity. A zero or absent amount makes no connection at all, and a gap severs the chain rather than being stepped over. Sixteen routes per event is the ceiling; beyond it the event is refused. The matrix bends the oscillator voices - sine, triangle, square, sawtooth, supersaw, pulse, bytebeat; samples, wavetables and the zzfx family ignore it.",
            params: &[
                ReferenceParam {
                    name: "value",
                    r#type: "number | Pattern",
                    description: "modulation index; 0 or absent = no route",
                },
            ],
            examples: &[
                "note(\"c2 e2 g2\").s(\"sine\").fm(2).fm6(\"<0 1 4 8>\")",
            ],
            tags: &["control", "fm"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["fmi7", "fmh7"],
        aliases: &["fm7"],
        reference:         ReferenceEntry {
            name: "fmi7",
            synonyms: &["fm7"],
            summary: "how hard operator 7 modulates operator 6",
            description: "The plain-chain route, also spelled fm7: operator 7 into the one below it on the way to the carrier. The value is the modulation index - brightness - and zero or absent makes no route, which severs the chain at that link. The value is a modulation depth - an index, not hertz: the deviation it produces scales with the carrier and the source's harmonicity. A zero or absent amount makes no connection at all, and a gap severs the chain rather than being stepped over. Sixteen routes per event is the ceiling; beyond it the event is refused. The matrix bends the oscillator voices - sine, triangle, square, sawtooth, supersaw, pulse, bytebeat; samples, wavetables and the zzfx family ignore it.",
            params: &[
                ReferenceParam {
                    name: "value",
                    r#type: "number | Pattern",
                    description: "modulation index; 0 or absent = no route",
                },
            ],
            examples: &[
                "note(\"c2 e2 g2\").s(\"sine\").fm(2).fm7(\"<0 1 4 8>\")",
            ],
            tags: &["control", "fm"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["fmi8", "fmh8"],
        aliases: &["fm8"],
        reference:         ReferenceEntry {
            name: "fmi8",
            synonyms: &["fm8"],
            summary: "how hard operator 8 modulates operator 7",
            description: "The plain-chain route, also spelled fm8: operator 8 into the one below it on the way to the carrier. The value is the modulation index - brightness - and zero or absent makes no route, which severs the chain at that link. The value is a modulation depth - an index, not hertz: the deviation it produces scales with the carrier and the source's harmonicity. A zero or absent amount makes no connection at all, and a gap severs the chain rather than being stepped over. Sixteen routes per event is the ceiling; beyond it the event is refused. The matrix bends the oscillator voices - sine, triangle, square, sawtooth, supersaw, pulse, bytebeat; samples, wavetables and the zzfx family ignore it.",
            params: &[
                ReferenceParam {
                    name: "value",
                    r#type: "number | Pattern",
                    description: "modulation index; 0 or absent = no route",
                },
            ],
            examples: &[
                "note(\"c2 e2 g2\").s(\"sine\").fm(2).fm8(\"<0 1 4 8>\")",
            ],
            tags: &["control", "fm"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["fmenv"],
        aliases: &["fme", "fme1", "fmenv1"],
        reference: ReferenceEntry {
            name: "fmenv",
            synonyms: &["fme", "fme1", "fmenv1"],
            summary: "Curve of FM operator 1’s depth envelope.",
            description: "Defaults to exponential. lin or linear selects a linear curve; other strings keep exponential. The curve only matters after fmattack, fmdecay, fmsustain, or fmrelease creates a depth envelope; fmenv alone leaves the depth flat. Numbered forms select operators 2 through 8. Requires a nonzero FM route involving operator 1 and a connected path to the carrier; fmi (alias fm) supplies the direct route. The FM matrix affects basic oscillators, supersaw, pulse, and bytebeat. Recorded samples, `gm_*` soundfonts, `wt_*` wavetables, ZZFX, sbd, standalone noise, live input, and buses ignore it.",
            params: &[
                ReferenceParam {
                    name: "type",
                    r#type: "string | Pattern",
                    description: "lin | exp",
                },
            ],
            examples: &[
                "note(\"c e g b g e\")\n.fm(4)\n.fmdecay(.2)\n.fmsustain(0)\n.fmenv(\"<exp lin>\")\n._scope()",
            ],
            tags: &["fm", "envelope", "audio"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["fmenv2"],
        aliases: &["fme2"],
        reference:         ReferenceEntry {
            name: "fmenv2",
            synonyms: &["fme2"],
            summary: "the shape of operator 2's depth envelope",
            description: "Exponential unless the value is \"lin\" or \"linear\". The envelope itself is the per-operator ADSR - fmattack2, fmdecay2, fmsustain2, fmrelease2 - which exists as soon as any of the four is named.",
            params: &[
                ReferenceParam {
                    name: "value",
                    r#type: "string | Pattern",
                    description: "envelope kind; exponential unless \"lin\"",
                },
            ],
            examples: &[
                "note(\"c2 e2\").s(\"sine\").fm2(3).fmenv2(\"lin\")",
            ],
            tags: &["control", "fm"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["fmenv3"],
        aliases: &["fme3"],
        reference:         ReferenceEntry {
            name: "fmenv3",
            synonyms: &["fme3"],
            summary: "the shape of operator 3's depth envelope",
            description: "Exponential unless the value is \"lin\" or \"linear\". The envelope itself is the per-operator ADSR - fmattack3, fmdecay3, fmsustain3, fmrelease3 - which exists as soon as any of the four is named.",
            params: &[
                ReferenceParam {
                    name: "value",
                    r#type: "string | Pattern",
                    description: "envelope kind; exponential unless \"lin\"",
                },
            ],
            examples: &[
                "note(\"c2 e2\").s(\"sine\").fm3(3).fmenv3(\"lin\")",
            ],
            tags: &["control", "fm"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["fmenv4"],
        aliases: &["fme4"],
        reference:         ReferenceEntry {
            name: "fmenv4",
            synonyms: &["fme4"],
            summary: "the shape of operator 4's depth envelope",
            description: "Exponential unless the value is \"lin\" or \"linear\". The envelope itself is the per-operator ADSR - fmattack4, fmdecay4, fmsustain4, fmrelease4 - which exists as soon as any of the four is named.",
            params: &[
                ReferenceParam {
                    name: "value",
                    r#type: "string | Pattern",
                    description: "envelope kind; exponential unless \"lin\"",
                },
            ],
            examples: &[
                "note(\"c2 e2\").s(\"sine\").fm4(3).fmenv4(\"lin\")",
            ],
            tags: &["control", "fm"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["fmenv5"],
        aliases: &["fme5"],
        reference:         ReferenceEntry {
            name: "fmenv5",
            synonyms: &["fme5"],
            summary: "the shape of operator 5's depth envelope",
            description: "Exponential unless the value is \"lin\" or \"linear\". The envelope itself is the per-operator ADSR - fmattack5, fmdecay5, fmsustain5, fmrelease5 - which exists as soon as any of the four is named.",
            params: &[
                ReferenceParam {
                    name: "value",
                    r#type: "string | Pattern",
                    description: "envelope kind; exponential unless \"lin\"",
                },
            ],
            examples: &[
                "note(\"c2 e2\").s(\"sine\").fm5(3).fmenv5(\"lin\")",
            ],
            tags: &["control", "fm"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["fmenv6"],
        aliases: &["fme6"],
        reference:         ReferenceEntry {
            name: "fmenv6",
            synonyms: &["fme6"],
            summary: "the shape of operator 6's depth envelope",
            description: "Exponential unless the value is \"lin\" or \"linear\". The envelope itself is the per-operator ADSR - fmattack6, fmdecay6, fmsustain6, fmrelease6 - which exists as soon as any of the four is named.",
            params: &[
                ReferenceParam {
                    name: "value",
                    r#type: "string | Pattern",
                    description: "envelope kind; exponential unless \"lin\"",
                },
            ],
            examples: &[
                "note(\"c2 e2\").s(\"sine\").fm6(3).fmenv6(\"lin\")",
            ],
            tags: &["control", "fm"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["fmenv7"],
        aliases: &["fme7"],
        reference:         ReferenceEntry {
            name: "fmenv7",
            synonyms: &["fme7"],
            summary: "the shape of operator 7's depth envelope",
            description: "Exponential unless the value is \"lin\" or \"linear\". The envelope itself is the per-operator ADSR - fmattack7, fmdecay7, fmsustain7, fmrelease7 - which exists as soon as any of the four is named.",
            params: &[
                ReferenceParam {
                    name: "value",
                    r#type: "string | Pattern",
                    description: "envelope kind; exponential unless \"lin\"",
                },
            ],
            examples: &[
                "note(\"c2 e2\").s(\"sine\").fm7(3).fmenv7(\"lin\")",
            ],
            tags: &["control", "fm"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["fmenv8"],
        aliases: &["fme8"],
        reference:         ReferenceEntry {
            name: "fmenv8",
            synonyms: &["fme8"],
            summary: "the shape of operator 8's depth envelope",
            description: "Exponential unless the value is \"lin\" or \"linear\". The envelope itself is the per-operator ADSR - fmattack8, fmdecay8, fmsustain8, fmrelease8 - which exists as soon as any of the four is named.",
            params: &[
                ReferenceParam {
                    name: "value",
                    r#type: "string | Pattern",
                    description: "envelope kind; exponential unless \"lin\"",
                },
            ],
            examples: &[
                "note(\"c2 e2\").s(\"sine\").fm8(3).fmenv8(\"lin\")",
            ],
            tags: &["control", "fm"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["fmattack"],
        aliases: &["fmatt", "fmatt1", "fmattack1"],
        reference: ReferenceEntry {
            name: "fmattack",
            synonyms: &["fmatt", "fmatt1", "fmattack1"],
            summary: "FM operator 1 depth attack, in seconds.",
            description: "Time for modulation depth to rise to its peak. Naming any fmattack, fmdecay, fmsustain, or fmrelease creates operator 1’s depth envelope; with none present the depth stays flat. Omitted attack/decay default to 0.001 s and release to 0.01 s. Omitted sustain is 0.001 when decay is set, otherwise 1. Attack/decay floor at 0.001 s, release at 0.01 s, and sustain caps at 1. Numbered forms select operators 2 through 8. Requires a nonzero FM route involving operator 1 and a connected path to the carrier; fmi (alias fm) supplies the direct route. The FM matrix affects basic oscillators, supersaw, pulse, and bytebeat. Recorded samples, `gm_*` soundfonts, `wt_*` wavetables, ZZFX, sbd, standalone noise, live input, and buses ignore it.",
            params: &[
                ReferenceParam {
                    name: "time",
                    r#type: "number | Pattern",
                    description: "attack time",
                },
            ],
            examples: &[
                "note(\"c e g b g e\")\n.fm(4)\n.fmattack(\"<0 .05 .1 .2>\")\n._scope()",
            ],
            tags: &["fm", "envelope", "audio"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["fmattack2"],
        aliases: &["fmatt2"],
        reference:         ReferenceEntry {
            name: "fmattack2",
            synonyms: &["fmatt2"],
            summary: "operator 2's depth attack",
            description: "How fast operator 2's modulation depth opens. The per-operator depth ADSR - fmattack2, fmdecay2, fmsustain2, fmrelease2 - is built the moment ANY of the four is named; left alone, the depth is flat. Base values are attack 0.001, decay 0.001, sustain 1, release 0.01, with the conditional rule: naming decay without sustain drops sustain to its 0.001 floor. Attack and decay floor at 0.001 s, release at 0.01 s, and sustain caps at 1.",
            params: &[
                ReferenceParam {
                    name: "value",
                    r#type: "number | Pattern",
                    description: "attack time in seconds; floored at 0.001",
                },
            ],
            examples: &[
                "note(\"c2 e2\").s(\"sine\").fm2(3).fmattack2(\"0.01 0.1 0.5\")",
            ],
            tags: &["control", "fm"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["fmattack3"],
        aliases: &["fmatt3"],
        reference:         ReferenceEntry {
            name: "fmattack3",
            synonyms: &["fmatt3"],
            summary: "operator 3's depth attack",
            description: "How fast operator 3's modulation depth opens. The per-operator depth ADSR - fmattack3, fmdecay3, fmsustain3, fmrelease3 - is built the moment ANY of the four is named; left alone, the depth is flat. Base values are attack 0.001, decay 0.001, sustain 1, release 0.01, with the conditional rule: naming decay without sustain drops sustain to its 0.001 floor. Attack and decay floor at 0.001 s, release at 0.01 s, and sustain caps at 1.",
            params: &[
                ReferenceParam {
                    name: "value",
                    r#type: "number | Pattern",
                    description: "attack time in seconds; floored at 0.001",
                },
            ],
            examples: &[
                "note(\"c2 e2\").s(\"sine\").fm3(3).fmattack3(\"0.01 0.1 0.5\")",
            ],
            tags: &["control", "fm"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["fmattack4"],
        aliases: &["fmatt4"],
        reference:         ReferenceEntry {
            name: "fmattack4",
            synonyms: &["fmatt4"],
            summary: "operator 4's depth attack",
            description: "How fast operator 4's modulation depth opens. The per-operator depth ADSR - fmattack4, fmdecay4, fmsustain4, fmrelease4 - is built the moment ANY of the four is named; left alone, the depth is flat. Base values are attack 0.001, decay 0.001, sustain 1, release 0.01, with the conditional rule: naming decay without sustain drops sustain to its 0.001 floor. Attack and decay floor at 0.001 s, release at 0.01 s, and sustain caps at 1.",
            params: &[
                ReferenceParam {
                    name: "value",
                    r#type: "number | Pattern",
                    description: "attack time in seconds; floored at 0.001",
                },
            ],
            examples: &[
                "note(\"c2 e2\").s(\"sine\").fm4(3).fmattack4(\"0.01 0.1 0.5\")",
            ],
            tags: &["control", "fm"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["fmattack5"],
        aliases: &["fmatt5"],
        reference:         ReferenceEntry {
            name: "fmattack5",
            synonyms: &["fmatt5"],
            summary: "operator 5's depth attack",
            description: "How fast operator 5's modulation depth opens. The per-operator depth ADSR - fmattack5, fmdecay5, fmsustain5, fmrelease5 - is built the moment ANY of the four is named; left alone, the depth is flat. Base values are attack 0.001, decay 0.001, sustain 1, release 0.01, with the conditional rule: naming decay without sustain drops sustain to its 0.001 floor. Attack and decay floor at 0.001 s, release at 0.01 s, and sustain caps at 1.",
            params: &[
                ReferenceParam {
                    name: "value",
                    r#type: "number | Pattern",
                    description: "attack time in seconds; floored at 0.001",
                },
            ],
            examples: &[
                "note(\"c2 e2\").s(\"sine\").fm5(3).fmattack5(\"0.01 0.1 0.5\")",
            ],
            tags: &["control", "fm"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["fmattack6"],
        aliases: &["fmatt6"],
        reference:         ReferenceEntry {
            name: "fmattack6",
            synonyms: &["fmatt6"],
            summary: "operator 6's depth attack",
            description: "How fast operator 6's modulation depth opens. The per-operator depth ADSR - fmattack6, fmdecay6, fmsustain6, fmrelease6 - is built the moment ANY of the four is named; left alone, the depth is flat. Base values are attack 0.001, decay 0.001, sustain 1, release 0.01, with the conditional rule: naming decay without sustain drops sustain to its 0.001 floor. Attack and decay floor at 0.001 s, release at 0.01 s, and sustain caps at 1.",
            params: &[
                ReferenceParam {
                    name: "value",
                    r#type: "number | Pattern",
                    description: "attack time in seconds; floored at 0.001",
                },
            ],
            examples: &[
                "note(\"c2 e2\").s(\"sine\").fm6(3).fmattack6(\"0.01 0.1 0.5\")",
            ],
            tags: &["control", "fm"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["fmattack7"],
        aliases: &["fmatt7"],
        reference:         ReferenceEntry {
            name: "fmattack7",
            synonyms: &["fmatt7"],
            summary: "operator 7's depth attack",
            description: "How fast operator 7's modulation depth opens. The per-operator depth ADSR - fmattack7, fmdecay7, fmsustain7, fmrelease7 - is built the moment ANY of the four is named; left alone, the depth is flat. Base values are attack 0.001, decay 0.001, sustain 1, release 0.01, with the conditional rule: naming decay without sustain drops sustain to its 0.001 floor. Attack and decay floor at 0.001 s, release at 0.01 s, and sustain caps at 1.",
            params: &[
                ReferenceParam {
                    name: "value",
                    r#type: "number | Pattern",
                    description: "attack time in seconds; floored at 0.001",
                },
            ],
            examples: &[
                "note(\"c2 e2\").s(\"sine\").fm7(3).fmattack7(\"0.01 0.1 0.5\")",
            ],
            tags: &["control", "fm"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["fmattack8"],
        aliases: &["fmatt8"],
        reference:         ReferenceEntry {
            name: "fmattack8",
            synonyms: &["fmatt8"],
            summary: "operator 8's depth attack",
            description: "How fast operator 8's modulation depth opens. The per-operator depth ADSR - fmattack8, fmdecay8, fmsustain8, fmrelease8 - is built the moment ANY of the four is named; left alone, the depth is flat. Base values are attack 0.001, decay 0.001, sustain 1, release 0.01, with the conditional rule: naming decay without sustain drops sustain to its 0.001 floor. Attack and decay floor at 0.001 s, release at 0.01 s, and sustain caps at 1.",
            params: &[
                ReferenceParam {
                    name: "value",
                    r#type: "number | Pattern",
                    description: "attack time in seconds; floored at 0.001",
                },
            ],
            examples: &[
                "note(\"c2 e2\").s(\"sine\").fm8(3).fmattack8(\"0.01 0.1 0.5\")",
            ],
            tags: &["control", "fm"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["fmwave"],
        aliases: &["fmwave1"],
        reference: ReferenceEntry {
            name: "fmwave",
            synonyms: &["fmwave1"],
            summary: "Waveform of FM operator 1.",
            description: "Defaults to sine. Accepts sine, triangle/tri, square, sawtooth/saw, white, pink, brown, or crackle; unknown names reject an active operator. A noise operator uses a looping buffer with no pitched oscillator, and incoming FM routes do not modulate it. Numbered forms select operators 2 through 8. Requires a nonzero FM route involving operator 1 and a connected path to the carrier; fmi (alias fm) supplies the direct route. The FM matrix affects basic oscillators, supersaw, pulse, and bytebeat. Recorded samples, `gm_*` soundfonts, `wt_*` wavetables, ZZFX, sbd, standalone noise, live input, and buses ignore it.",
            params: &[
                ReferenceParam {
                    name: "wave",
                    r#type: "string | Pattern",
                    description: "sine, triangle, square, sawtooth, white, pink, brown or crackle",
                },
            ],
            examples: &[
                "n(\"0 1 2 3\".fast(4)).scale(\"d:minor\").s(\"sine\").fmwave(\"<sine square sawtooth crackle>\").fm(4).fmh(2.01)",
                "n(\"0 1 2 3\".fast(4)).chord(\"<Dm Am F G>\").voicing().s(\"sawtooth\").fmwave(\"brown\").fm(.6)",
            ],
            tags: &["fm", "audio"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["fmwave2"],
        aliases: &[],
        reference:         ReferenceEntry {
            name: "fmwave2",
            synonyms: &[],
            summary: "operator 2's waveform",
            description: "The default is sine. Also accepted: triangle, square, sawtooth and the noise colours white, pink, brown and crackle - a noise operator is a looping buffer with no pitch, and nothing can modulate into one. Any other name refuses the event.",
            params: &[
                ReferenceParam {
                    name: "value",
                    r#type: "string | Pattern",
                    description: "waveform name; default \"sine\"",
                },
            ],
            examples: &[
                "note(\"c2 e2\").s(\"sine\").fm2(3).fmwave2(\"square\")",
            ],
            tags: &["control", "fm"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["fmwave3"],
        aliases: &[],
        reference:         ReferenceEntry {
            name: "fmwave3",
            synonyms: &[],
            summary: "operator 3's waveform",
            description: "The default is sine. Also accepted: triangle, square, sawtooth and the noise colours white, pink, brown and crackle - a noise operator is a looping buffer with no pitch, and nothing can modulate into one. Any other name refuses the event.",
            params: &[
                ReferenceParam {
                    name: "value",
                    r#type: "string | Pattern",
                    description: "waveform name; default \"sine\"",
                },
            ],
            examples: &[
                "note(\"c2 e2\").s(\"sine\").fm3(3).fmwave3(\"square\")",
            ],
            tags: &["control", "fm"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["fmwave4"],
        aliases: &[],
        reference:         ReferenceEntry {
            name: "fmwave4",
            synonyms: &[],
            summary: "operator 4's waveform",
            description: "The default is sine. Also accepted: triangle, square, sawtooth and the noise colours white, pink, brown and crackle - a noise operator is a looping buffer with no pitch, and nothing can modulate into one. Any other name refuses the event.",
            params: &[
                ReferenceParam {
                    name: "value",
                    r#type: "string | Pattern",
                    description: "waveform name; default \"sine\"",
                },
            ],
            examples: &[
                "note(\"c2 e2\").s(\"sine\").fm4(3).fmwave4(\"square\")",
            ],
            tags: &["control", "fm"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["fmwave5"],
        aliases: &[],
        reference:         ReferenceEntry {
            name: "fmwave5",
            synonyms: &[],
            summary: "operator 5's waveform",
            description: "The default is sine. Also accepted: triangle, square, sawtooth and the noise colours white, pink, brown and crackle - a noise operator is a looping buffer with no pitch, and nothing can modulate into one. Any other name refuses the event.",
            params: &[
                ReferenceParam {
                    name: "value",
                    r#type: "string | Pattern",
                    description: "waveform name; default \"sine\"",
                },
            ],
            examples: &[
                "note(\"c2 e2\").s(\"sine\").fm5(3).fmwave5(\"square\")",
            ],
            tags: &["control", "fm"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["fmwave6"],
        aliases: &[],
        reference:         ReferenceEntry {
            name: "fmwave6",
            synonyms: &[],
            summary: "operator 6's waveform",
            description: "The default is sine. Also accepted: triangle, square, sawtooth and the noise colours white, pink, brown and crackle - a noise operator is a looping buffer with no pitch, and nothing can modulate into one. Any other name refuses the event.",
            params: &[
                ReferenceParam {
                    name: "value",
                    r#type: "string | Pattern",
                    description: "waveform name; default \"sine\"",
                },
            ],
            examples: &[
                "note(\"c2 e2\").s(\"sine\").fm6(3).fmwave6(\"square\")",
            ],
            tags: &["control", "fm"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["fmwave7"],
        aliases: &[],
        reference:         ReferenceEntry {
            name: "fmwave7",
            synonyms: &[],
            summary: "operator 7's waveform",
            description: "The default is sine. Also accepted: triangle, square, sawtooth and the noise colours white, pink, brown and crackle - a noise operator is a looping buffer with no pitch, and nothing can modulate into one. Any other name refuses the event.",
            params: &[
                ReferenceParam {
                    name: "value",
                    r#type: "string | Pattern",
                    description: "waveform name; default \"sine\"",
                },
            ],
            examples: &[
                "note(\"c2 e2\").s(\"sine\").fm7(3).fmwave7(\"square\")",
            ],
            tags: &["control", "fm"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["fmwave8"],
        aliases: &[],
        reference:         ReferenceEntry {
            name: "fmwave8",
            synonyms: &[],
            summary: "operator 8's waveform",
            description: "The default is sine. Also accepted: triangle, square, sawtooth and the noise colours white, pink, brown and crackle - a noise operator is a looping buffer with no pitch, and nothing can modulate into one. Any other name refuses the event.",
            params: &[
                ReferenceParam {
                    name: "value",
                    r#type: "string | Pattern",
                    description: "waveform name; default \"sine\"",
                },
            ],
            examples: &[
                "note(\"c2 e2\").s(\"sine\").fm8(3).fmwave8(\"square\")",
            ],
            tags: &["control", "fm"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["fmdecay"],
        aliases: &["fmdec", "fmdec1", "fmdecay1"],
        reference: ReferenceEntry {
            name: "fmdecay",
            synonyms: &["fmdec", "fmdec1", "fmdecay1"],
            summary: "FM operator 1 depth decay, in seconds.",
            description: "Time for modulation depth to fall from its peak to sustain. Naming any fmattack, fmdecay, fmsustain, or fmrelease creates operator 1’s depth envelope; with none present the depth stays flat. Omitted attack/decay default to 0.001 s and release to 0.01 s. Omitted sustain is 0.001 when decay is set, otherwise 1. Attack/decay floor at 0.001 s, release at 0.01 s, and sustain caps at 1. Numbered forms select operators 2 through 8. Requires a nonzero FM route involving operator 1 and a connected path to the carrier; fmi (alias fm) supplies the direct route. The FM matrix affects basic oscillators, supersaw, pulse, and bytebeat. Recorded samples, `gm_*` soundfonts, `wt_*` wavetables, ZZFX, sbd, standalone noise, live input, and buses ignore it.",
            params: &[
                ReferenceParam {
                    name: "time",
                    r#type: "number | Pattern",
                    description: "decay time",
                },
            ],
            examples: &[
                "note(\"c e g b g e\")\n.fm(4)\n.fmdecay(\"<.01 .05 .1 .2>\")\n.fmsustain(.4)\n._scope()",
            ],
            tags: &["fm", "envelope", "audio"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["fmdecay2"],
        aliases: &["fmdec2"],
        reference:         ReferenceEntry {
            name: "fmdecay2",
            synonyms: &["fmdec2"],
            summary: "operator 2's depth decay",
            description: "How fast operator 2's modulation depth falls to its sustain. The per-operator depth ADSR - fmattack2, fmdecay2, fmsustain2, fmrelease2 - is built the moment ANY of the four is named; left alone, the depth is flat. Base values are attack 0.001, decay 0.001, sustain 1, release 0.01, with the conditional rule: naming decay without sustain drops sustain to its 0.001 floor. Attack and decay floor at 0.001 s, release at 0.01 s, and sustain caps at 1.",
            params: &[
                ReferenceParam {
                    name: "value",
                    r#type: "number | Pattern",
                    description: "decay time in seconds; floored at 0.001",
                },
            ],
            examples: &[
                "note(\"c2 e2\").s(\"sine\").fm2(3).fmdecay2(\"0.01 0.1 0.3\")",
            ],
            tags: &["control", "fm"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["fmdecay3"],
        aliases: &["fmdec3"],
        reference:         ReferenceEntry {
            name: "fmdecay3",
            synonyms: &["fmdec3"],
            summary: "operator 3's depth decay",
            description: "How fast operator 3's modulation depth falls to its sustain. The per-operator depth ADSR - fmattack3, fmdecay3, fmsustain3, fmrelease3 - is built the moment ANY of the four is named; left alone, the depth is flat. Base values are attack 0.001, decay 0.001, sustain 1, release 0.01, with the conditional rule: naming decay without sustain drops sustain to its 0.001 floor. Attack and decay floor at 0.001 s, release at 0.01 s, and sustain caps at 1.",
            params: &[
                ReferenceParam {
                    name: "value",
                    r#type: "number | Pattern",
                    description: "decay time in seconds; floored at 0.001",
                },
            ],
            examples: &[
                "note(\"c2 e2\").s(\"sine\").fm3(3).fmdecay3(\"0.01 0.1 0.3\")",
            ],
            tags: &["control", "fm"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["fmdecay4"],
        aliases: &["fmdec4"],
        reference:         ReferenceEntry {
            name: "fmdecay4",
            synonyms: &["fmdec4"],
            summary: "operator 4's depth decay",
            description: "How fast operator 4's modulation depth falls to its sustain. The per-operator depth ADSR - fmattack4, fmdecay4, fmsustain4, fmrelease4 - is built the moment ANY of the four is named; left alone, the depth is flat. Base values are attack 0.001, decay 0.001, sustain 1, release 0.01, with the conditional rule: naming decay without sustain drops sustain to its 0.001 floor. Attack and decay floor at 0.001 s, release at 0.01 s, and sustain caps at 1.",
            params: &[
                ReferenceParam {
                    name: "value",
                    r#type: "number | Pattern",
                    description: "decay time in seconds; floored at 0.001",
                },
            ],
            examples: &[
                "note(\"c2 e2\").s(\"sine\").fm4(3).fmdecay4(\"0.01 0.1 0.3\")",
            ],
            tags: &["control", "fm"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["fmdecay5"],
        aliases: &["fmdec5"],
        reference:         ReferenceEntry {
            name: "fmdecay5",
            synonyms: &["fmdec5"],
            summary: "operator 5's depth decay",
            description: "How fast operator 5's modulation depth falls to its sustain. The per-operator depth ADSR - fmattack5, fmdecay5, fmsustain5, fmrelease5 - is built the moment ANY of the four is named; left alone, the depth is flat. Base values are attack 0.001, decay 0.001, sustain 1, release 0.01, with the conditional rule: naming decay without sustain drops sustain to its 0.001 floor. Attack and decay floor at 0.001 s, release at 0.01 s, and sustain caps at 1.",
            params: &[
                ReferenceParam {
                    name: "value",
                    r#type: "number | Pattern",
                    description: "decay time in seconds; floored at 0.001",
                },
            ],
            examples: &[
                "note(\"c2 e2\").s(\"sine\").fm5(3).fmdecay5(\"0.01 0.1 0.3\")",
            ],
            tags: &["control", "fm"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["fmdecay6"],
        aliases: &["fmdec6"],
        reference:         ReferenceEntry {
            name: "fmdecay6",
            synonyms: &["fmdec6"],
            summary: "operator 6's depth decay",
            description: "How fast operator 6's modulation depth falls to its sustain. The per-operator depth ADSR - fmattack6, fmdecay6, fmsustain6, fmrelease6 - is built the moment ANY of the four is named; left alone, the depth is flat. Base values are attack 0.001, decay 0.001, sustain 1, release 0.01, with the conditional rule: naming decay without sustain drops sustain to its 0.001 floor. Attack and decay floor at 0.001 s, release at 0.01 s, and sustain caps at 1.",
            params: &[
                ReferenceParam {
                    name: "value",
                    r#type: "number | Pattern",
                    description: "decay time in seconds; floored at 0.001",
                },
            ],
            examples: &[
                "note(\"c2 e2\").s(\"sine\").fm6(3).fmdecay6(\"0.01 0.1 0.3\")",
            ],
            tags: &["control", "fm"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["fmdecay7"],
        aliases: &["fmdec7"],
        reference:         ReferenceEntry {
            name: "fmdecay7",
            synonyms: &["fmdec7"],
            summary: "operator 7's depth decay",
            description: "How fast operator 7's modulation depth falls to its sustain. The per-operator depth ADSR - fmattack7, fmdecay7, fmsustain7, fmrelease7 - is built the moment ANY of the four is named; left alone, the depth is flat. Base values are attack 0.001, decay 0.001, sustain 1, release 0.01, with the conditional rule: naming decay without sustain drops sustain to its 0.001 floor. Attack and decay floor at 0.001 s, release at 0.01 s, and sustain caps at 1.",
            params: &[
                ReferenceParam {
                    name: "value",
                    r#type: "number | Pattern",
                    description: "decay time in seconds; floored at 0.001",
                },
            ],
            examples: &[
                "note(\"c2 e2\").s(\"sine\").fm7(3).fmdecay7(\"0.01 0.1 0.3\")",
            ],
            tags: &["control", "fm"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["fmdecay8"],
        aliases: &["fmdec8"],
        reference:         ReferenceEntry {
            name: "fmdecay8",
            synonyms: &["fmdec8"],
            summary: "operator 8's depth decay",
            description: "How fast operator 8's modulation depth falls to its sustain. The per-operator depth ADSR - fmattack8, fmdecay8, fmsustain8, fmrelease8 - is built the moment ANY of the four is named; left alone, the depth is flat. Base values are attack 0.001, decay 0.001, sustain 1, release 0.01, with the conditional rule: naming decay without sustain drops sustain to its 0.001 floor. Attack and decay floor at 0.001 s, release at 0.01 s, and sustain caps at 1.",
            params: &[
                ReferenceParam {
                    name: "value",
                    r#type: "number | Pattern",
                    description: "decay time in seconds; floored at 0.001",
                },
            ],
            examples: &[
                "note(\"c2 e2\").s(\"sine\").fm8(3).fmdecay8(\"0.01 0.1 0.3\")",
            ],
            tags: &["control", "fm"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["fmsustain"],
        aliases: &["fmsus", "fmsus1", "fmsustain1"],
        reference: ReferenceEntry {
            name: "fmsustain",
            synonyms: &["fmsus", "fmsus1", "fmsustain1"],
            summary: "FM operator 1 sustain depth as a fraction of its peak.",
            description: "A unitless sustain multiplier, normally 0..1. Naming any fmattack, fmdecay, fmsustain, or fmrelease creates operator 1’s depth envelope; with none present the depth stays flat. Omitted attack/decay default to 0.001 s and release to 0.01 s. Omitted sustain is 0.001 when decay is set, otherwise 1. Attack/decay floor at 0.001 s, release at 0.01 s, and sustain caps at 1. Numbered forms select operators 2 through 8. Requires a nonzero FM route involving operator 1 and a connected path to the carrier; fmi (alias fm) supplies the direct route. The FM matrix affects basic oscillators, supersaw, pulse, and bytebeat. Recorded samples, `gm_*` soundfonts, `wt_*` wavetables, ZZFX, sbd, standalone noise, live input, and buses ignore it.",
            params: &[
                ReferenceParam {
                    name: "level",
                    r#type: "number | Pattern",
                    description: "sustain level",
                },
            ],
            examples: &[
                "note(\"c e g b g e\")\n.fm(4)\n.fmdecay(.1)\n.fmsustain(\"<1 .75 .5 0>\")\n._scope()",
            ],
            tags: &["fm", "envelope", "audio"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["fmsustain2"],
        aliases: &["fmsus2"],
        reference:         ReferenceEntry {
            name: "fmsustain2",
            synonyms: &["fmsus2"],
            summary: "operator 2's depth sustain",
            description: "The level operator 2's modulation depth holds at. The per-operator depth ADSR - fmattack2, fmdecay2, fmsustain2, fmrelease2 - is built the moment ANY of the four is named; left alone, the depth is flat. Base values are attack 0.001, decay 0.001, sustain 1, release 0.01, with the conditional rule: naming decay without sustain drops sustain to its 0.001 floor. Attack and decay floor at 0.001 s, release at 0.01 s, and sustain caps at 1.",
            params: &[
                ReferenceParam {
                    name: "value",
                    r#type: "number | Pattern",
                    description: "sustain level 0..1; capped at 1",
                },
            ],
            examples: &[
                "note(\"c2 e2\").s(\"sine\").fm2(3).fmsustain2(\"0.1 0.5 1\")",
            ],
            tags: &["control", "fm"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["fmsustain3"],
        aliases: &["fmsus3"],
        reference:         ReferenceEntry {
            name: "fmsustain3",
            synonyms: &["fmsus3"],
            summary: "operator 3's depth sustain",
            description: "The level operator 3's modulation depth holds at. The per-operator depth ADSR - fmattack3, fmdecay3, fmsustain3, fmrelease3 - is built the moment ANY of the four is named; left alone, the depth is flat. Base values are attack 0.001, decay 0.001, sustain 1, release 0.01, with the conditional rule: naming decay without sustain drops sustain to its 0.001 floor. Attack and decay floor at 0.001 s, release at 0.01 s, and sustain caps at 1.",
            params: &[
                ReferenceParam {
                    name: "value",
                    r#type: "number | Pattern",
                    description: "sustain level 0..1; capped at 1",
                },
            ],
            examples: &[
                "note(\"c2 e2\").s(\"sine\").fm3(3).fmsustain3(\"0.1 0.5 1\")",
            ],
            tags: &["control", "fm"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["fmsustain4"],
        aliases: &["fmsus4"],
        reference:         ReferenceEntry {
            name: "fmsustain4",
            synonyms: &["fmsus4"],
            summary: "operator 4's depth sustain",
            description: "The level operator 4's modulation depth holds at. The per-operator depth ADSR - fmattack4, fmdecay4, fmsustain4, fmrelease4 - is built the moment ANY of the four is named; left alone, the depth is flat. Base values are attack 0.001, decay 0.001, sustain 1, release 0.01, with the conditional rule: naming decay without sustain drops sustain to its 0.001 floor. Attack and decay floor at 0.001 s, release at 0.01 s, and sustain caps at 1.",
            params: &[
                ReferenceParam {
                    name: "value",
                    r#type: "number | Pattern",
                    description: "sustain level 0..1; capped at 1",
                },
            ],
            examples: &[
                "note(\"c2 e2\").s(\"sine\").fm4(3).fmsustain4(\"0.1 0.5 1\")",
            ],
            tags: &["control", "fm"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["fmsustain5"],
        aliases: &["fmsus5"],
        reference:         ReferenceEntry {
            name: "fmsustain5",
            synonyms: &["fmsus5"],
            summary: "operator 5's depth sustain",
            description: "The level operator 5's modulation depth holds at. The per-operator depth ADSR - fmattack5, fmdecay5, fmsustain5, fmrelease5 - is built the moment ANY of the four is named; left alone, the depth is flat. Base values are attack 0.001, decay 0.001, sustain 1, release 0.01, with the conditional rule: naming decay without sustain drops sustain to its 0.001 floor. Attack and decay floor at 0.001 s, release at 0.01 s, and sustain caps at 1.",
            params: &[
                ReferenceParam {
                    name: "value",
                    r#type: "number | Pattern",
                    description: "sustain level 0..1; capped at 1",
                },
            ],
            examples: &[
                "note(\"c2 e2\").s(\"sine\").fm5(3).fmsustain5(\"0.1 0.5 1\")",
            ],
            tags: &["control", "fm"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["fmsustain6"],
        aliases: &["fmsus6"],
        reference:         ReferenceEntry {
            name: "fmsustain6",
            synonyms: &["fmsus6"],
            summary: "operator 6's depth sustain",
            description: "The level operator 6's modulation depth holds at. The per-operator depth ADSR - fmattack6, fmdecay6, fmsustain6, fmrelease6 - is built the moment ANY of the four is named; left alone, the depth is flat. Base values are attack 0.001, decay 0.001, sustain 1, release 0.01, with the conditional rule: naming decay without sustain drops sustain to its 0.001 floor. Attack and decay floor at 0.001 s, release at 0.01 s, and sustain caps at 1.",
            params: &[
                ReferenceParam {
                    name: "value",
                    r#type: "number | Pattern",
                    description: "sustain level 0..1; capped at 1",
                },
            ],
            examples: &[
                "note(\"c2 e2\").s(\"sine\").fm6(3).fmsustain6(\"0.1 0.5 1\")",
            ],
            tags: &["control", "fm"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["fmsustain7"],
        aliases: &["fmsus7"],
        reference:         ReferenceEntry {
            name: "fmsustain7",
            synonyms: &["fmsus7"],
            summary: "operator 7's depth sustain",
            description: "The level operator 7's modulation depth holds at. The per-operator depth ADSR - fmattack7, fmdecay7, fmsustain7, fmrelease7 - is built the moment ANY of the four is named; left alone, the depth is flat. Base values are attack 0.001, decay 0.001, sustain 1, release 0.01, with the conditional rule: naming decay without sustain drops sustain to its 0.001 floor. Attack and decay floor at 0.001 s, release at 0.01 s, and sustain caps at 1.",
            params: &[
                ReferenceParam {
                    name: "value",
                    r#type: "number | Pattern",
                    description: "sustain level 0..1; capped at 1",
                },
            ],
            examples: &[
                "note(\"c2 e2\").s(\"sine\").fm7(3).fmsustain7(\"0.1 0.5 1\")",
            ],
            tags: &["control", "fm"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["fmsustain8"],
        aliases: &["fmsus8"],
        reference:         ReferenceEntry {
            name: "fmsustain8",
            synonyms: &["fmsus8"],
            summary: "operator 8's depth sustain",
            description: "The level operator 8's modulation depth holds at. The per-operator depth ADSR - fmattack8, fmdecay8, fmsustain8, fmrelease8 - is built the moment ANY of the four is named; left alone, the depth is flat. Base values are attack 0.001, decay 0.001, sustain 1, release 0.01, with the conditional rule: naming decay without sustain drops sustain to its 0.001 floor. Attack and decay floor at 0.001 s, release at 0.01 s, and sustain caps at 1.",
            params: &[
                ReferenceParam {
                    name: "value",
                    r#type: "number | Pattern",
                    description: "sustain level 0..1; capped at 1",
                },
            ],
            examples: &[
                "note(\"c2 e2\").s(\"sine\").fm8(3).fmsustain8(\"0.1 0.5 1\")",
            ],
            tags: &["control", "fm"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["fmrelease"],
        aliases: &["fmrel", "fmrel1", "fmrelease1"],
        reference: ReferenceEntry {
            name: "fmrelease",
            synonyms: &["fmrel", "fmrel1", "fmrelease1"],
            summary: "FM operator 1 depth release, in seconds.",
            description: "Time for modulation depth to fall after the note ends. Naming any fmattack, fmdecay, fmsustain, or fmrelease creates operator 1’s depth envelope; with none present the depth stays flat. Omitted attack/decay default to 0.001 s and release to 0.01 s. Omitted sustain is 0.001 when decay is set, otherwise 1. Attack/decay floor at 0.001 s, release at 0.01 s, and sustain caps at 1. Numbered forms select operators 2 through 8. Requires a nonzero FM route involving operator 1 and a connected path to the carrier; fmi (alias fm) supplies the direct route. The FM matrix affects basic oscillators, supersaw, pulse, and bytebeat. Recorded samples, `gm_*` soundfonts, `wt_*` wavetables, ZZFX, sbd, standalone noise, live input, and buses ignore it.",
            params: &[
                ReferenceParam {
                    name: "time",
                    r#type: "number | Pattern",
                    description: "release time",
                },
            ],
            examples: &[],
            tags: &["fm", "envelope", "audio"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["fmrelease2"],
        aliases: &["fmrel2"],
        reference:         ReferenceEntry {
            name: "fmrelease2",
            synonyms: &["fmrel2"],
            summary: "operator 2's depth release",
            description: "How operator 2's modulation depth lets go at the note's end. The per-operator depth ADSR - fmattack2, fmdecay2, fmsustain2, fmrelease2 - is built the moment ANY of the four is named; left alone, the depth is flat. Base values are attack 0.001, decay 0.001, sustain 1, release 0.01, with the conditional rule: naming decay without sustain drops sustain to its 0.001 floor. Attack and decay floor at 0.001 s, release at 0.01 s, and sustain caps at 1.",
            params: &[
                ReferenceParam {
                    name: "value",
                    r#type: "number | Pattern",
                    description: "release time in seconds; floored at 0.01",
                },
            ],
            examples: &[
                "note(\"c2 e2\").s(\"sine\").fm2(3).fmrelease2(\"0.05 0.2 0.5\")",
            ],
            tags: &["control", "fm"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["fmrelease3"],
        aliases: &["fmrel3"],
        reference:         ReferenceEntry {
            name: "fmrelease3",
            synonyms: &["fmrel3"],
            summary: "operator 3's depth release",
            description: "How operator 3's modulation depth lets go at the note's end. The per-operator depth ADSR - fmattack3, fmdecay3, fmsustain3, fmrelease3 - is built the moment ANY of the four is named; left alone, the depth is flat. Base values are attack 0.001, decay 0.001, sustain 1, release 0.01, with the conditional rule: naming decay without sustain drops sustain to its 0.001 floor. Attack and decay floor at 0.001 s, release at 0.01 s, and sustain caps at 1.",
            params: &[
                ReferenceParam {
                    name: "value",
                    r#type: "number | Pattern",
                    description: "release time in seconds; floored at 0.01",
                },
            ],
            examples: &[
                "note(\"c2 e2\").s(\"sine\").fm3(3).fmrelease3(\"0.05 0.2 0.5\")",
            ],
            tags: &["control", "fm"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["fmrelease4"],
        aliases: &["fmrel4"],
        reference:         ReferenceEntry {
            name: "fmrelease4",
            synonyms: &["fmrel4"],
            summary: "operator 4's depth release",
            description: "How operator 4's modulation depth lets go at the note's end. The per-operator depth ADSR - fmattack4, fmdecay4, fmsustain4, fmrelease4 - is built the moment ANY of the four is named; left alone, the depth is flat. Base values are attack 0.001, decay 0.001, sustain 1, release 0.01, with the conditional rule: naming decay without sustain drops sustain to its 0.001 floor. Attack and decay floor at 0.001 s, release at 0.01 s, and sustain caps at 1.",
            params: &[
                ReferenceParam {
                    name: "value",
                    r#type: "number | Pattern",
                    description: "release time in seconds; floored at 0.01",
                },
            ],
            examples: &[
                "note(\"c2 e2\").s(\"sine\").fm4(3).fmrelease4(\"0.05 0.2 0.5\")",
            ],
            tags: &["control", "fm"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["fmrelease5"],
        aliases: &["fmrel5"],
        reference:         ReferenceEntry {
            name: "fmrelease5",
            synonyms: &["fmrel5"],
            summary: "operator 5's depth release",
            description: "How operator 5's modulation depth lets go at the note's end. The per-operator depth ADSR - fmattack5, fmdecay5, fmsustain5, fmrelease5 - is built the moment ANY of the four is named; left alone, the depth is flat. Base values are attack 0.001, decay 0.001, sustain 1, release 0.01, with the conditional rule: naming decay without sustain drops sustain to its 0.001 floor. Attack and decay floor at 0.001 s, release at 0.01 s, and sustain caps at 1.",
            params: &[
                ReferenceParam {
                    name: "value",
                    r#type: "number | Pattern",
                    description: "release time in seconds; floored at 0.01",
                },
            ],
            examples: &[
                "note(\"c2 e2\").s(\"sine\").fm5(3).fmrelease5(\"0.05 0.2 0.5\")",
            ],
            tags: &["control", "fm"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["fmrelease6"],
        aliases: &["fmrel6"],
        reference:         ReferenceEntry {
            name: "fmrelease6",
            synonyms: &["fmrel6"],
            summary: "operator 6's depth release",
            description: "How operator 6's modulation depth lets go at the note's end. The per-operator depth ADSR - fmattack6, fmdecay6, fmsustain6, fmrelease6 - is built the moment ANY of the four is named; left alone, the depth is flat. Base values are attack 0.001, decay 0.001, sustain 1, release 0.01, with the conditional rule: naming decay without sustain drops sustain to its 0.001 floor. Attack and decay floor at 0.001 s, release at 0.01 s, and sustain caps at 1.",
            params: &[
                ReferenceParam {
                    name: "value",
                    r#type: "number | Pattern",
                    description: "release time in seconds; floored at 0.01",
                },
            ],
            examples: &[
                "note(\"c2 e2\").s(\"sine\").fm6(3).fmrelease6(\"0.05 0.2 0.5\")",
            ],
            tags: &["control", "fm"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["fmrelease7"],
        aliases: &["fmrel7"],
        reference:         ReferenceEntry {
            name: "fmrelease7",
            synonyms: &["fmrel7"],
            summary: "operator 7's depth release",
            description: "How operator 7's modulation depth lets go at the note's end. The per-operator depth ADSR - fmattack7, fmdecay7, fmsustain7, fmrelease7 - is built the moment ANY of the four is named; left alone, the depth is flat. Base values are attack 0.001, decay 0.001, sustain 1, release 0.01, with the conditional rule: naming decay without sustain drops sustain to its 0.001 floor. Attack and decay floor at 0.001 s, release at 0.01 s, and sustain caps at 1.",
            params: &[
                ReferenceParam {
                    name: "value",
                    r#type: "number | Pattern",
                    description: "release time in seconds; floored at 0.01",
                },
            ],
            examples: &[
                "note(\"c2 e2\").s(\"sine\").fm7(3).fmrelease7(\"0.05 0.2 0.5\")",
            ],
            tags: &["control", "fm"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
    ControlRow {
        names: &["fmrelease8"],
        aliases: &["fmrel8"],
        reference:         ReferenceEntry {
            name: "fmrelease8",
            synonyms: &["fmrel8"],
            summary: "operator 8's depth release",
            description: "How operator 8's modulation depth lets go at the note's end. The per-operator depth ADSR - fmattack8, fmdecay8, fmsustain8, fmrelease8 - is built the moment ANY of the four is named; left alone, the depth is flat. Base values are attack 0.001, decay 0.001, sustain 1, release 0.01, with the conditional rule: naming decay without sustain drops sustain to its 0.001 floor. Attack and decay floor at 0.001 s, release at 0.01 s, and sustain caps at 1.",
            params: &[
                ReferenceParam {
                    name: "value",
                    r#type: "number | Pattern",
                    description: "release time in seconds; floored at 0.01",
                },
            ],
            examples: &[
                "note(\"c2 e2\").s(\"sine\").fm8(3).fmrelease8(\"0.05 0.2 0.5\")",
            ],
            tags: &["control", "fm"],
            no_autocomplete: false,
            deprecated: false,
            origin: "rustel",
        },
    },
];

/// Every control's reference entry, in table order.
pub fn reference_entries() -> impl Iterator<Item = &'static ReferenceEntry> {
    CONTROLS.iter().map(|row| &row.reference)
}

/// FM spellings omitted from the reference because neither Rustel's nor
/// Strudel's FM resolver reads them. Unsupported features remain documented
/// so readers can find the native limitations.
///
/// Names match each entry's `name`; aliases follow the same visibility.
pub const REFERENCE_HIDDEN: &[&str] = &[
    // Both FM resolvers read sources 1..8. Source 0 does not exist, and
    // adjacent source/destination pairs use fmi, fmi2, ..., fmi8 instead.
    "fmi00", "fmi01", "fmi02", "fmi03", "fmi04", "fmi05", "fmi06", "fmi07", "fmi08",
    "fmi10", "fmi21", "fmi32", "fmi43", "fmi54", "fmi65", "fmi76", "fmi87",
];

/// Whether a control's reference entry shows in the reference.
pub fn reference_shows(name: &str) -> bool {
    !REFERENCE_HIDDEN.contains(&name)
}

/// Every control's reference entry the reference shows, in table order.
/// The engine's surface is unaffected: [`crate::controls`] registers every
/// row whatever this filter says, so a score spelling a hidden name still
/// evaluates.
pub fn visible_reference_entries() -> impl Iterator<Item = &'static ReferenceEntry> {
    CONTROLS
        .iter()
        .map(|row| &row.reference)
        .filter(|entry| reference_shows(entry.name))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Hidden names must identify catalog entries.
    #[test]
    fn every_hidden_name_is_a_table_entry() {
        let documented: std::collections::BTreeSet<&str> =
            CONTROLS.iter().map(|row| row.reference.name).collect();
        for hidden in REFERENCE_HIDDEN {
            assert!(
                documented.contains(*hidden),
                "`{hidden}` is hidden from the reference but no entry documents it"
            );
        }
    }

    /// The hidden list cannot contain duplicates or aliases.
    #[test]
    fn the_hidden_list_has_no_duplicates_or_aliases() {
        let mut seen = std::collections::BTreeSet::new();
        for hidden in REFERENCE_HIDDEN {
            assert!(seen.insert(*hidden), "`{hidden}` is hidden twice");
        }
        let aliases: std::collections::BTreeSet<&str> = CONTROLS
            .iter()
            .flat_map(|row| row.aliases.iter().copied())
            .collect();
        for hidden in REFERENCE_HIDDEN {
            assert!(
                !aliases.contains(hidden),
                "`{hidden}` is an alias; hide the entry's own name instead"
            );
        }
    }

    /// Hidden entries must remain registered.
    #[test]
    fn hiding_an_entry_hides_nothing_the_engine_installs() {
        for row in CONTROLS {
            if !reference_shows(row.reference.name) {
                for name in row.names.iter().chain(row.aliases.iter()) {
                    assert!(
                        crate::controls::canonical_control_name(name).is_some(),
                        "`{name}` is hidden from the reference but no longer installed"
                    );
                }
            }
        }
    }

    #[test]
    fn unsupported_controls_remain_documented() {
        let visible: std::collections::BTreeSet<&str> = visible_reference_entries()
            .map(|entry| entry.name)
            .collect();
        for name in [
            "chorus", "analyze", "fft", "waveloss", "fshift", "fshiftnote",
            "fshiftphase", "triode", "krush", "kcutoff", "octer", "octersub",
            "octersubsub", "ring", "ringf", "ringdf", "freeze", "xsdelay", "tsdelay",
            "real", "imag", "enhance", "comb", "smear", "scram", "binshift", "hbrick",
            "lbrick", "hold", "gate", "overgain", "overshape", "panspan", "pansplay",
            "panwidth", "panorient", "fadeInTime", "mtranspose", "ctranspose",
            "harmonic", "stepsPerOctave", "octaveR", "semitone", "voice", "expression",
            "sustainpedal", "frameRate", "frames", "hours", "minutes", "seconds",
            "songPtr", "uid", "val", "channel",
        ] {
            assert!(
                visible.contains(name),
                "`{name}` needs a reference entry explaining its native limitation"
            );
        }
    }

    /// Visible reference entries describe Rustel's behavior.
    #[test]
    fn visible_entries_never_name_the_upstream_implementation() {
        for entry in visible_reference_entries() {
            for text in [entry.summary, entry.description] {
                let lowered = text.to_lowercase();
                for word in ["upstream", "strudel.cc", "strudel"] {
                    assert!(
                        !lowered.contains(word),
                        "`{}`'s visible reference text names `{word}`: {}",
                        entry.name,
                        text
                    );
                }
            }
        }
    }
}
