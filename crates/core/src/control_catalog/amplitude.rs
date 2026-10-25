use super::{ControlRow, ReferenceEntry, ReferenceParam};

pub(super) const VELOCITY: ControlRow = ControlRow {
    names: &["velocity"],
    aliases: &["vel"],
    reference: ReferenceEntry {
        name: "velocity",
        synonyms: &["vel"],
        summary: "Sets the velocity from 0 to 1.",
        description: "Sets the velocity from 0 to 1. Is multiplied together with gain.\n\nDefaults to 1. This is a linear multiplier for any native voice; 0..1 is the usual range. Its scale is independent of the selected source's own normalization.",
        params: &[ReferenceParam {
            name: "velocity",
            r#type: "number | Pattern",
            description: "from 0 to 1, multiplied with gain",
        }],
        examples: &[
            "s(\"hh*8\")\n.gain(\".4!2 1 .4!2 1 .4 1\")\n.velocity(\".4 1\")",
        ],
        tags: &["amplitude", "audio"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
};

pub(super) const GAIN: ControlRow = ControlRow {
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
};

pub(super) const POSTGAIN: ControlRow = ControlRow {
    names: &["postgain"],
    aliases: &[],
    reference: ReferenceEntry {
        name: "postgain",
        synonyms: &[],
        summary: "Output gain before per-voice limiting and sends.",
        description: "A linear output multiplier, default 1. It scales the processed voice before limit and the delay, reverb, and audio-bus sends. Applies to every native source.",
        params: &[ReferenceParam {
            name: "amount",
            r#type: "number | Pattern",
            description: "linear output gain; 1 leaves the level as it is",
        }],
        examples: &[
            "s(\"bd sd [~ bd] sd,hh*8\")\n.compressor(\"-20:20:10:.002:.02\").postgain(1.5)",
        ],
        tags: &["amplitude", "audio"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
};

pub(super) const ATTACK: ControlRow = ControlRow {
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
};

pub(super) const DECAY: ControlRow = ControlRow {
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
};

pub(super) const SUSTAIN: ControlRow = ControlRow {
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
};

pub(super) const RELEASE: ControlRow = ControlRow {
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
};

pub(super) const TREMOLO: ControlRow = ControlRow {
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
};

pub(super) const TREMOLOSYNC: ControlRow = ControlRow {
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
};

pub(super) const TREMOLODEPTH: ControlRow = ControlRow {
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
};

pub(super) const TREMOLOSKEW: ControlRow = ControlRow {
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
};

pub(super) const TREMOLOPHASE: ControlRow = ControlRow {
    names: &["tremolophase"],
    aliases: &["tremphase"],
    reference: ReferenceEntry {
        name: "tremolophase",
        synonyms: &["tremphase"],
        summary: "Phase offset of an enabled tremolo, in turns.",
        description: "Defaults to 0. A value of 1 is a full turn. The LFO follows the musical time of the event (its cycle divided by cycles per second) with this added phase. Requires tremolo or tremolosync. Phase alone creates no tremolo.",
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
};

pub(super) const TREMOLOSHAPE: ControlRow = ControlRow {
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
};

pub(super) const CLIP: ControlRow = ControlRow {
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
};

pub(super) const DURATION: ControlRow = ControlRow {
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
};

pub(super) const FXRELEASE: ControlRow = ControlRow {
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
};
