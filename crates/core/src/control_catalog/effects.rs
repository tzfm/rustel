use super::{ControlRow, ReferenceEntry, ReferenceParam};

pub(super) const CRUSH: ControlRow = ControlRow {
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
};

pub(super) const COARSE: ControlRow = ControlRow {
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
};

pub(super) const PHASERRATE: ControlRow = ControlRow {
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
};

pub(super) const PHASERSWEEP: ControlRow = ControlRow {
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
};

pub(super) const PHASERCENTER: ControlRow = ControlRow {
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
};

pub(super) const PHASERDEPTH: ControlRow = ControlRow {
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
        tags: &["audio"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
};

pub(super) const SHAPE: ControlRow = ControlRow {
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
};

pub(super) const DISTORT: ControlRow = ControlRow {
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
};

pub(super) const DISTORTVOL: ControlRow = ControlRow {
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
};

pub(super) const DIODE: ControlRow = ControlRow {
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
};

pub(super) const DISTORTTYPE: ControlRow = ControlRow {
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
};

pub(super) const VOWEL: ControlRow = ControlRow {
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
};

pub(super) const DENSITY: ControlRow = ControlRow {
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
};

pub(super) const TRANSIENT: ControlRow = ControlRow {
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
};
