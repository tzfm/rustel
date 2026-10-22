use super::{ControlRow, LIMIT_REFERENCE, ReferenceEntry, ReferenceParam};

pub(super) const LIMITCHAR: ControlRow = ControlRow {
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
};

pub(super) const LIMIT: ControlRow = ControlRow {
    names: &["limit", "limitchar"],
    aliases: &[],
    reference: LIMIT_REFERENCE,
};

pub(super) const COMPRESSOR: ControlRow = ControlRow {
    names: &["compressor", "compressorRatio", "compressorKnee", "compressorAttack", "compressorRelease"],
    aliases: &[],
    reference: ReferenceEntry {
        name: "compressor",
        synonyms: &[],
        summary: "Dynamics Compressor.",
        description: "Dynamics Compressor. The params are `compressor(\"threshold:ratio:knee:attack:release\")`\nMore info [here](https://developer.mozilla.org/en-US/docs/Web/API/DynamicsCompressorNode?retiredLocale=de#instance_properties)\n\nThe stage is absent until compressor supplies a threshold in dB, clamped to -100..0. The secondary parameters alone do not create it. Ratio/knee/attack/release default to 10, 10 dB, 0.005 seconds, and 0.05 seconds. These are effects on any native audio source, including oscillators, wavetables, samples, `gm_*` soundfonts, and live input.",
        params: &[ReferenceParam {
            name: "threshold",
            r#type: "number | string | Pattern",
            description: "in dB from -100 to 0, or the whole threshold:ratio:knee:attack:release string",
        }],
        examples: &[
            "s(\"bd sd [~ bd] sd,hh*8\")\n.compressor(\"-20:20:10:.002:.02\")",
        ],
        tags: &["audio"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
};

pub(super) const COMPRESSOR_KNEE: ControlRow = ControlRow {
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
};

pub(super) const COMPRESSOR_RATIO: ControlRow = ControlRow {
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
};

pub(super) const COMPRESSOR_ATTACK: ControlRow = ControlRow {
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
};

pub(super) const COMPRESSOR_RELEASE: ControlRow = ControlRow {
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
};
