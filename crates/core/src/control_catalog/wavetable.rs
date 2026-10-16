use super::{ControlRow, ReferenceEntry, ReferenceParam};

pub(super) const WT: ControlRow = ControlRow {
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
};

pub(super) const WTENV: ControlRow = ControlRow {
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
};

pub(super) const WTATTACK: ControlRow = ControlRow {
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
};

pub(super) const WTDECAY: ControlRow = ControlRow {
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
};

pub(super) const WTSUSTAIN: ControlRow = ControlRow {
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
};

pub(super) const WTRELEASE: ControlRow = ControlRow {
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
};

pub(super) const WTRATE: ControlRow = ControlRow {
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
};

pub(super) const WTSYNC: ControlRow = ControlRow {
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
};

pub(super) const WTDEPTH: ControlRow = ControlRow {
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
};

pub(super) const WTSHAPE: ControlRow = ControlRow {
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
};

pub(super) const WTDC: ControlRow = ControlRow {
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
};

pub(super) const WTSKEW: ControlRow = ControlRow {
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
};

pub(super) const WARP: ControlRow = ControlRow {
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
};

pub(super) const WARPATTACK: ControlRow = ControlRow {
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
};

pub(super) const WARPDECAY: ControlRow = ControlRow {
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
};

pub(super) const WARPSUSTAIN: ControlRow = ControlRow {
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
};

pub(super) const WARPRELEASE: ControlRow = ControlRow {
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
};

pub(super) const WARPRATE: ControlRow = ControlRow {
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
};

pub(super) const WARPDEPTH: ControlRow = ControlRow {
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
};

pub(super) const WARPSHAPE: ControlRow = ControlRow {
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
};

pub(super) const WARPDC: ControlRow = ControlRow {
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
};

pub(super) const WARPSKEW: ControlRow = ControlRow {
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
};

pub(super) const WARPMODE: ControlRow = ControlRow {
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
};

pub(super) const WTPHASERAND: ControlRow = ControlRow {
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
};

pub(super) const WARPENV: ControlRow = ControlRow {
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
};

pub(super) const WARPSYNC: ControlRow = ControlRow {
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
};
