//! The tonal layer - transposition, scales and voicings; see crate::tonaljs.
//!
//! Documentation text from the Strudel project (AGPL-3.0-or-later),
//! https://strudel.cc, where an entry carries upstream's words; entries this
//! port wrote itself say so in their own words.

use super::{Registry, add};
use crate::combinators as c;

const TRANSPOSE: crate::reference::ReferenceEntry = crate::reference::ReferenceEntry {
    name: "transpose",
    synonyms: &["trans"],
    summary: "Change the pitch of each value by the given amount.",
    description: "Change the pitch of each value by the given amount. Expects numbers or note strings as values.\nThe amount can be given as a number of semitones or as a string in interval short notation.\nIf you don't care about enharmonic correctness, just use numbers. Otherwise, pass the interval of\nthe form: ST where S is the degree number and T the type of interval with\n\n- M = major\n- m = minor\n- P = perfect\n- A = augmented\n- d = diminished\n\nExamples intervals:\n\n- 1P = unison\n- 3M = major third\n- 3m = minor third\n- 4P = perfect fourth\n- 4A = augmented fourth\n- 5P = perfect fifth\n- 5d = diminished fifth",
    params: &[crate::reference::ReferenceParam {
        name: "amount",
        r#type: "string | number",
        description: "Either number of semitones or interval string.",
    }],
    examples: &[
        "\"c2 c3\".fast(2).transpose(\"<0 -2 5 3>\".slow(2)).note()",
        "\"c2 c3\".fast(2).transpose(\"<1P -2M 4P 3m>\".slow(2)).note()",
    ],
    tags: &["tonal"],
    no_autocomplete: false,
    deprecated: false,
    origin: "rustel",
};

const SCALE_TRANSPOSE: crate::reference::ReferenceEntry = crate::reference::ReferenceEntry {
    name: "scaleTranspose",
    synonyms: &["scaleTrans", "strans"],
    summary: "Transposes notes inside the scale by the number of steps.",
    description: "Transposes notes inside the scale by the number of steps.\nExpected to be called on a Pattern which already has a {@link Pattern#scale}",
    params: &[crate::reference::ReferenceParam {
        name: "offset",
        r#type: "offset",
        description: "number of steps inside the scale",
    }],
    examples: &[
        "\"-8 [2,4,6]\"\n.scale('C4 bebop major')\n.scaleTranspose(\"<0 -1 -2 -3 -4 -5 -6 -4>\")\n.note()",
    ],
    tags: &["tonal"],
    no_autocomplete: false,
    deprecated: false,
    origin: "rustel",
};

const VOICING: crate::reference::ReferenceEntry = crate::reference::ReferenceEntry {
    name: "voicing",
    synonyms: &[],
    summary: "Turns chord symbols into voicings.",
    description: "Turns chord symbols into voicings. You can use the following control params:\n\n- `chord`: Note, followed by chord symbol, e.g. C Am G7 Bb^7\n- `dict`: voicing dictionary to use, falls back to default dictionary\n- `anchor`: the note that is used to align the chord\n- `mode`: how the voicing is aligned to the anchor\n  - `below`: top note <= anchor\n  - `duck`: top note <= anchor, anchor excluded\n  - `above`: bottom note >= anchor\n- `offset`: whole number that shifts the voicing up or down to the next voicing\n- `n`: if set, the voicing is played like a scale. Overshooting numbers will be octaved\n\nAll of the above controls are optional, except `chord`.\nIf you pass a pattern of strings to voicing, they will be interpreted as chords.",
    params: &[],
    examples: &["n(\"0 1 2 3\").chord(\"<C Am F G>\").voicing()"],
    tags: &["tonal"],
    no_autocomplete: false,
    deprecated: false,
    origin: "rustel",
};

const VOICINGS: crate::reference::ReferenceEntry = crate::reference::ReferenceEntry {
    name: "voicings",
    synonyms: &[],
    summary: "DEPRECATED: still works, but it is recommended you use .voicing instead (without s).",
    description: "DEPRECATED: still works, but it is recommended you use .voicing instead (without s).\nTurns chord symbols into voicings, using the smoothest voice leading possible.\nUses [chord-voicings package](https://github.com/felixroos/chord-voicings#chord-voicings).",
    params: &[crate::reference::ReferenceParam {
        name: "dictionary",
        r#type: "string",
        description: "which voicing dictionary to use.",
    }],
    examples: &["stack(\"<C^7 A7 Dm7 G7>\".voicings('lefthand'), \"<C3 A2 D3 G2>\").note()"],
    tags: &["tonal"],
    no_autocomplete: false,
    deprecated: false,
    origin: "rustel",
};

const ROOT_NOTES: crate::reference::ReferenceEntry = crate::reference::ReferenceEntry {
    name: "rootNotes",
    synonyms: &[],
    summary: "Maps the chords of the incoming pattern to root notes in the given octave.",
    description: "Maps the chords of the incoming pattern to root notes in the given octave.",
    params: &[crate::reference::ReferenceParam {
        name: "octave",
        r#type: "octave",
        description: "octave to use",
    }],
    examples: &["\"<C^7 A7 Dm7 G7>\".rootNotes(2).note()"],
    tags: &["tonal"],
    no_autocomplete: false,
    deprecated: false,
    origin: "rustel",
};

const SCALE: crate::reference::ReferenceEntry = crate::reference::ReferenceEntry {
    name: "scale",
    synonyms: &[],
    summary: "Turns numbers into notes in the scale (zero indexed) or quantizes notes to a scale.",
    description: "Turns numbers into notes in the scale (zero indexed) or quantizes notes to a scale.\n\nWhen describing notes via numbers, note that negative numbers can be used to wrap backwards\nin the scale as well as sharps or flats to produce notes outside of the scale.\n\nAlso sets scale for other scale operations, like {@link Pattern#scaleTranspose}.\n\nA scale consists of a root note (e.g. `c4`, `c`, `f#`, `bb4`) followed by semicolon (':') and then a [scale type](https://github.com/tonaljs/tonal/blob/main/packages/scale-type/data.ts).\n\nThe scale name must be written without spaces (because it would be interpreted as a multi-step pattern otherwise).\nIf your scale name includes spaces, replace them with colons.\n\nThe root note defaults to octave 3, if no octave number is given.",
    params: &[crate::reference::ReferenceParam {
        name: "scale",
        r#type: "scale",
        description: "Name of scale",
    }],
    examples: &[
        "n(\"0 2 4 6 4 2\").scale(\"C:major\")",
        "n(\"[0,7] 4 [2,7] 4\")\n.scale(\"C:<major minor>/2\")\n.s(\"piano\")",
        "n(rand.range(0,12).segment(8))\n.scale(\"C:ritusen\")\n.s(\"piano\")",
        "n(\"<[0,7b] [-4# -4] [-2,7##] 4 [0,7] [-4# -4b] [-2,7###] 4b>*4\")\n.scale(\"C:<major minor>/2\")\n.s(\"piano\")",
        "note(\"C1*16\").transpose(irand(36)).scale('Cb2 major').scaleTranspose(3)",
        "n(\"[0 0] [1 2] [3 4] [5 6]\").scale(\"C:major:blues\")",
    ],
    tags: &["tonal"],
    no_autocomplete: false,
    deprecated: false,
    origin: "rustel",
};

pub(super) fn register(r: &mut Registry) {
    // The tonal layer - see crate::tonaljs. `scale` is registered
    // (name, fn, true, true): patternified AND step-preserving.
    add(
        r,
        &["transpose", "trans"],
        TRANSPOSE,
        2,
        false,
        crate::native_combinator!(|args, pat| c::transpose(
            &pat,
            args.first().cloned().unwrap_or(crate::Value::Undefined)
        )),
    );

    add(
        r,
        &["scaleTranspose", "scaleTrans", "strans"],
        SCALE_TRANSPOSE,
        2,
        false,
        crate::native_combinator!(|args, pat| c::scale_transpose(
            &pat,
            args.first().cloned().unwrap_or(crate::Value::Undefined)
        )),
    );

    add(
        r,
        &["voicing"],
        VOICING,
        1,
        false,
        crate::native_combinator!(|_args, pat| crate::voicings::voicing(&pat)),
    );

    add(
        r,
        &["voicings"],
        VOICINGS,
        2,
        false,
        crate::native_combinator!(|args, pat| crate::voicings::voicings(
            &pat,
            args.first().cloned().unwrap_or(crate::Value::Undefined)
        )),
    );

    add(
        r,
        &["rootNotes"],
        ROOT_NOTES,
        2,
        false,
        crate::native_combinator!(|args, pat| crate::voicings::root_notes(
            &pat,
            args.first().cloned().unwrap_or(crate::Value::Undefined)
        )),
    );

    add(
        r,
        &["scale"],
        SCALE,
        2,
        true,
        crate::native_combinator!(|args, pat| c::scale(
            &pat,
            args.first().cloned().unwrap_or(crate::Value::Undefined)
        )),
    );
}
