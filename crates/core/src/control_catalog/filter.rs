use super::{ControlRow, ReferenceEntry, ReferenceParam};

pub(super) const BANDF: ControlRow = ControlRow {
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
};

pub(super) const BANDQ: ControlRow = ControlRow {
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
};

pub(super) const DRIVE: ControlRow = ControlRow {
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
};

pub(super) const CUTOFF: ControlRow = ControlRow {
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
};

pub(super) const LPENV: ControlRow = ControlRow {
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
};

pub(super) const HPENV: ControlRow = ControlRow {
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
};

pub(super) const BPENV: ControlRow = ControlRow {
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
};

pub(super) const LPATTACK: ControlRow = ControlRow {
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
};

pub(super) const HPATTACK: ControlRow = ControlRow {
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
};

pub(super) const BPATTACK: ControlRow = ControlRow {
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
};

pub(super) const LPDECAY: ControlRow = ControlRow {
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
};

pub(super) const HPDECAY: ControlRow = ControlRow {
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
};

pub(super) const BPDECAY: ControlRow = ControlRow {
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
};

pub(super) const LPSUSTAIN: ControlRow = ControlRow {
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
};

pub(super) const HPSUSTAIN: ControlRow = ControlRow {
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
};

pub(super) const BPSUSTAIN: ControlRow = ControlRow {
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
};

pub(super) const LPRELEASE: ControlRow = ControlRow {
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
};

pub(super) const HPRELEASE: ControlRow = ControlRow {
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
};

pub(super) const BPRELEASE: ControlRow = ControlRow {
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
};

pub(super) const FTYPE: ControlRow = ControlRow {
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
};

pub(super) const FANCHOR: ControlRow = ControlRow {
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
};

pub(super) const LPRATE: ControlRow = ControlRow {
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
};

pub(super) const LPSYNC: ControlRow = ControlRow {
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
};

pub(super) const LPDEPTH: ControlRow = ControlRow {
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
};

pub(super) const LPDEPTHFREQUENCY: ControlRow = ControlRow {
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
};

pub(super) const LPSHAPE: ControlRow = ControlRow {
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
};

pub(super) const LPDC: ControlRow = ControlRow {
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
};

pub(super) const LPSKEW: ControlRow = ControlRow {
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
};

pub(super) const BPRATE: ControlRow = ControlRow {
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
};

pub(super) const BPSYNC: ControlRow = ControlRow {
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
};

pub(super) const BPDEPTH: ControlRow = ControlRow {
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
};

pub(super) const BPDEPTHFREQUENCY: ControlRow = ControlRow {
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
};

pub(super) const BPSHAPE: ControlRow = ControlRow {
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
};

pub(super) const BPDC: ControlRow = ControlRow {
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
};

pub(super) const BPSKEW: ControlRow = ControlRow {
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
};

pub(super) const HPRATE: ControlRow = ControlRow {
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
};

pub(super) const HPSYNC: ControlRow = ControlRow {
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
};

pub(super) const HPDEPTH: ControlRow = ControlRow {
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
};

pub(super) const HPDEPTHFREQUENCY: ControlRow = ControlRow {
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
};

pub(super) const HPSHAPE: ControlRow = ControlRow {
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
};

pub(super) const HPDC: ControlRow = ControlRow {
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
};

pub(super) const HPSKEW: ControlRow = ControlRow {
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
};

pub(super) const HCUTOFF: ControlRow = ControlRow {
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
};

pub(super) const HRESONANCE: ControlRow = ControlRow {
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
};

pub(super) const RESONANCE: ControlRow = ControlRow {
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
};

pub(super) const DJF: ControlRow = ControlRow {
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
};
