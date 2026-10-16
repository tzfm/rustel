use super::{ControlRow, ReferenceEntry, ReferenceParam};

pub(super) const S: ControlRow = ControlRow {
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
};

pub(super) const N: ControlRow = ControlRow {
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
};

pub(super) const BANK: ControlRow = ControlRow {
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
};

pub(super) const BEGIN: ControlRow = ControlRow {
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
};

pub(super) const END: ControlRow = ControlRow {
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
};

pub(super) const LOOP: ControlRow = ControlRow {
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
};

pub(super) const LOOP_BEGIN: ControlRow = ControlRow {
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
};

pub(super) const LOOP_END: ControlRow = ControlRow {
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
};

pub(super) const CUT: ControlRow = ControlRow {
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
};

pub(super) const NUDGE: ControlRow = ControlRow {
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
};

pub(super) const SPEED: ControlRow = ControlRow {
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
};

pub(super) const STRETCH: ControlRow = ControlRow {
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
};

pub(super) const UNIT: ControlRow = ControlRow {
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
        tags: &["samples", "pitch"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
};
