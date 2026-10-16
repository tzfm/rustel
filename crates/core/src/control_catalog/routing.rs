use super::{ControlRow, ReferenceEntry, ReferenceParam};

pub(super) const DUCKORBIT: ControlRow = ControlRow {
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
};

pub(super) const DUCKDEPTH: ControlRow = ControlRow {
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
};

pub(super) const DUCKONSET: ControlRow = ControlRow {
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
};

pub(super) const DUCKATTACK: ControlRow = ControlRow {
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
};

pub(super) const DELAY: ControlRow = ControlRow {
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
};

pub(super) const DELAYFEEDBACK: ControlRow = ControlRow {
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
};

pub(super) const DELAYSPEED: ControlRow = ControlRow {
    names: &["delayspeed"],
    aliases: &[],
    reference: ReferenceEntry {
        name: "delayspeed",
        synonyms: &[],
        summary: "Delay playback speed. Unsupported in native audio.",
        description: "Stores a delay playback speed for external output. Rustel does not apply this control to native audio. Use delaytime to set the time between echoes.",
        params: &[
            ReferenceParam {
                name: "delayspeed",
                r#type: "number | Pattern",
                description: "playback speed multiplier",
            },
        ],
        examples: &[],
        tags: &["orbit", "osc"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
};

pub(super) const DELAYTIME: ControlRow = ControlRow {
    names: &["delaytime"],
    aliases: &["delayt", "dt"],
    reference: ReferenceEntry {
        name: "delaytime",
        synonyms: &["delayt", "dt"],
        summary: "Delay time in seconds. Requires a positive delay send.",
        description: "Overrides delaysync when explicit. Without it the default is 3/16 cycles divided by cycles per second. Positive times are capped at 1 second. A nonpositive time disables the send. delay and delayfeedback must also be positive.",
        params: &[
            ReferenceParam {
                name: "delay",
                r#type: "number | Pattern",
                description: "in seconds",
            },
        ],
        examples: &[
            "note(\"d d a# a\".fast(2))\n.s(\"sawtooth\")\n.delay(.8)\n.delaytime(1/2)",
        ],
        tags: &["orbit", "audio"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
};

pub(super) const DELAYSYNC: ControlRow = ControlRow {
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
};

pub(super) const DRY: ControlRow = ControlRow {
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
        tags: &["amplitude", "orbit", "audio"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
};

pub(super) const ORBIT: ControlRow = ControlRow {
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
};

pub(super) const BUS: ControlRow = ControlRow {
    names: &["bus"],
    aliases: &[],
    reference: ReferenceEntry {
        name: "bus",
        synonyms: &[],
        summary: "A `bus` is a send which can be used for mixing patterns.",
        description: "A `bus` is a send which can be used for mixing patterns. It combines with..\n  s(\"bus\") to play that bus through another pattern (for, say, applying non-linear\n  effects like distortion to multiple signals)\n\n  otherPat.bmod(..) (to modulate another pattern with the bus)\n\nRustel has 16 buses. A number from 0 to 15 selects one. A name such as 'drums' takes a bus no number uses. When each bus has a name, a new name takes the bus of the name used longest ago.",
        params: &[
            ReferenceParam {
                name: "bus",
                r#type: "number | string | Pattern",
                description: "a bus number from 0 to 15, or a name.",
            },
        ],
        examples: &[],
        tags: &["orbit", "audio"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
};

pub(super) const BUSGAIN: ControlRow = ControlRow {
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
        tags: &["amplitude", "orbit", "audio"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
};

pub(super) const PAN: ControlRow = ControlRow {
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
};

pub(super) const ROOM: ControlRow = ControlRow {
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
                description: "Reverb send amount. Values above 1 increase the send.",
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
};

pub(super) const ROOMLP: ControlRow = ControlRow {
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
};

pub(super) const ROOMDIM: ControlRow = ControlRow {
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
};

pub(super) const ROOMFADE: ControlRow = ControlRow {
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
};

pub(super) const IR: ControlRow = ControlRow {
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
};

pub(super) const IRSPEED: ControlRow = ControlRow {
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
};

pub(super) const IRBEGIN: ControlRow = ControlRow {
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
};

pub(super) const ROOMSIZE: ControlRow = ControlRow {
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
};
