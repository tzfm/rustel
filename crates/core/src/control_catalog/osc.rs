use super::{ControlRow, ReferenceEntry, ReferenceParam};

pub(super) const ACCELERATE: ControlRow = ControlRow {
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
};

pub(super) const AMP: ControlRow = ControlRow {
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
};

pub(super) const CHORUS: ControlRow = ControlRow {
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
        tags: &["pitch", "osc"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
};

pub(super) const ANALYZE: ControlRow = ControlRow {
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
};

pub(super) const FFT: ControlRow = ControlRow {
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
};

pub(super) const HOLD: ControlRow = ControlRow {
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
};

pub(super) const CHANNEL: ControlRow = ControlRow {
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
};

pub(super) const LOCK: ControlRow = ControlRow {
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
};

pub(super) const FADE_TIME: ControlRow = ControlRow {
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
};

pub(super) const FADE_IN_TIME: ControlRow = ControlRow {
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
        tags: &["control", "osc"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
};

pub(super) const GATE: ControlRow = ControlRow {
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
};

pub(super) const LESLIE: ControlRow = ControlRow {
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
};

pub(super) const LRATE: ControlRow = ControlRow {
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
};

pub(super) const LSIZE: ControlRow = ControlRow {
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
};

pub(super) const MTRANSPOSE: ControlRow = ControlRow {
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
};

pub(super) const CTRANSPOSE: ControlRow = ControlRow {
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
};

pub(super) const HARMONIC: ControlRow = ControlRow {
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
};

pub(super) const STEPS_PER_OCTAVE: ControlRow = ControlRow {
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
};

pub(super) const OCTAVE_R: ControlRow = ControlRow {
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
};

pub(super) const OVERGAIN: ControlRow = ControlRow {
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
};

pub(super) const OVERSHAPE: ControlRow = ControlRow {
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
};

pub(super) const PANSPAN: ControlRow = ControlRow {
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
};

pub(super) const PANSPLAY: ControlRow = ControlRow {
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
};

pub(super) const PANWIDTH: ControlRow = ControlRow {
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
};

pub(super) const PANORIENT: ControlRow = ControlRow {
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
};

pub(super) const SEMITONE: ControlRow = ControlRow {
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
};

pub(super) const VOICE: ControlRow = ControlRow {
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
};

pub(super) const SQUIZ: ControlRow = ControlRow {
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
};

pub(super) const WAVELOSS: ControlRow = ControlRow {
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
};

pub(super) const EXPRESSION: ControlRow = ControlRow {
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
};

pub(super) const SUSTAINPEDAL: ControlRow = ControlRow {
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
};

pub(super) const FSHIFT: ControlRow = ControlRow {
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
};

pub(super) const FSHIFTNOTE: ControlRow = ControlRow {
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
};

pub(super) const FSHIFTPHASE: ControlRow = ControlRow {
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
};

pub(super) const TRIODE: ControlRow = ControlRow {
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
};

pub(super) const KRUSH: ControlRow = ControlRow {
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
};

pub(super) const KCUTOFF: ControlRow = ControlRow {
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
};

pub(super) const OCTER: ControlRow = ControlRow {
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
};

pub(super) const OCTERSUB: ControlRow = ControlRow {
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
};

pub(super) const OCTERSUBSUB: ControlRow = ControlRow {
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
};

pub(super) const RING: ControlRow = ControlRow {
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
};

pub(super) const RINGF: ControlRow = ControlRow {
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
};

pub(super) const RINGDF: ControlRow = ControlRow {
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
};

pub(super) const FREEZE: ControlRow = ControlRow {
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
};

pub(super) const XSDELAY: ControlRow = ControlRow {
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
};

pub(super) const TSDELAY: ControlRow = ControlRow {
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
};

pub(super) const REAL: ControlRow = ControlRow {
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
};

pub(super) const IMAG: ControlRow = ControlRow {
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
};

pub(super) const ENHANCE: ControlRow = ControlRow {
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
};

pub(super) const COMB: ControlRow = ControlRow {
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
};

pub(super) const SMEAR: ControlRow = ControlRow {
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
};

pub(super) const SCRAM: ControlRow = ControlRow {
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
};

pub(super) const BINSHIFT: ControlRow = ControlRow {
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
};

pub(super) const HBRICK: ControlRow = ControlRow {
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
};

pub(super) const LBRICK: ControlRow = ControlRow {
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
};

pub(super) const FRAME_RATE: ControlRow = ControlRow {
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
};

pub(super) const FRAMES: ControlRow = ControlRow {
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
};

pub(super) const HOURS: ControlRow = ControlRow {
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
};

pub(super) const MINUTES: ControlRow = ControlRow {
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
};

pub(super) const SECONDS: ControlRow = ControlRow {
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
};

pub(super) const SONG_PTR: ControlRow = ControlRow {
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
};

pub(super) const UID: ControlRow = ControlRow {
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
};

pub(super) const VAL: ControlRow = ControlRow {
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
};

pub(super) const OSCHOST: ControlRow = ControlRow {
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
        tags: &["external_io", "osc"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
};

pub(super) const OSCPORT: ControlRow = ControlRow {
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
        tags: &["external_io", "osc"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
};
