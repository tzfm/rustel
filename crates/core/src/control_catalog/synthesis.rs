use super::{ControlRow, ReferenceEntry, ReferenceParam};

pub(super) const BYTE_BEAT_EXPRESSION: ControlRow = ControlRow {
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
};

pub(super) const BYTE_BEAT_START_TIME: ControlRow = ControlRow {
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
};

pub(super) const PW: ControlRow = ControlRow {
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
};

pub(super) const PWRATE: ControlRow = ControlRow {
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
};

pub(super) const PWSWEEP: ControlRow = ControlRow {
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
};

pub(super) const NOISE: ControlRow = ControlRow {
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
};

pub(super) const UNISON: ControlRow = ControlRow {
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
};

pub(super) const SPREAD: ControlRow = ControlRow {
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
};

pub(super) const SLIDE: ControlRow = ControlRow {
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
};

pub(super) const ZRAND: ControlRow = ControlRow {
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
};

pub(super) const CURVE: ControlRow = ControlRow {
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
};

pub(super) const SLIDE_2: ControlRow = ControlRow {
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
};

pub(super) const DELTA_SLIDE: ControlRow = ControlRow {
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
};

pub(super) const PITCH_JUMP: ControlRow = ControlRow {
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
};

pub(super) const PITCH_JUMP_TIME: ControlRow = ControlRow {
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
};

pub(super) const ZNOISE: ControlRow = ControlRow {
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
};

pub(super) const ZMOD: ControlRow = ControlRow {
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
};

pub(super) const ZCRUSH: ControlRow = ControlRow {
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
};

pub(super) const ZDELAY: ControlRow = ControlRow {
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
};

pub(super) const ZZFX: ControlRow = ControlRow {
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
};
