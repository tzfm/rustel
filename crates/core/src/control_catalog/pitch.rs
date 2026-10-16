use super::{ControlRow, ReferenceEntry, ReferenceParam};

pub(super) const I: ControlRow = ControlRow {
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
};

pub(super) const NOTE: ControlRow = ControlRow {
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
};

pub(super) const VIB: ControlRow = ControlRow {
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
};

pub(super) const VIBMOD: ControlRow = ControlRow {
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
};

pub(super) const DETUNE: ControlRow = ControlRow {
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
};

pub(super) const FREQ: ControlRow = ControlRow {
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
};

pub(super) const PATTACK: ControlRow = ControlRow {
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
};

pub(super) const PDECAY: ControlRow = ControlRow {
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
};

pub(super) const PSUSTAIN: ControlRow = ControlRow {
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
};

pub(super) const PRELEASE: ControlRow = ControlRow {
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
};

pub(super) const PENV: ControlRow = ControlRow {
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
};

pub(super) const PCURVE: ControlRow = ControlRow {
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
};

pub(super) const PANCHOR: ControlRow = ControlRow {
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
};

pub(super) const DEGREE: ControlRow = ControlRow {
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
};

pub(super) const OCTAVE: ControlRow = ControlRow {
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
        tags: &["pitch", "audio"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
};

pub(super) const CHORD: ControlRow = ControlRow {
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
};

pub(super) const DICTIONARY: ControlRow = ControlRow {
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
};

pub(super) const ANCHOR: ControlRow = ControlRow {
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
};

pub(super) const OFFSET: ControlRow = ControlRow {
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
};

pub(super) const OCTAVES: ControlRow = ControlRow {
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
};

pub(super) const MODE: ControlRow = ControlRow {
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
};
