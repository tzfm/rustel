//! The pinned control surface, documented where it is installed.
//!
//! Each row installs one control: its accessor names, its aliases, and the
//! reference entry a musician reads about it. A control cannot exist without
//! an entry because the entry is a field of the row.
//!
//! Includes documentation text from the Strudel project (AGPL-3.0-or-later),
//! https://strudel.cc, alongside descriptions written for Rustel. The
//! `origin` field is `rustel` for both; it does not distinguish authorship.
//!
//! `REFERENCE_HIDDEN` lists compatibility controls omitted from the studio's
//! reference. They remain installed so existing scores can evaluate. The
//! combinator registry has a corresponding list in
//! `register::COMBINATORS_REFERENCE_HIDDEN`.

use crate::reference::{ReferenceEntry, ReferenceParam};

mod amplitude;
mod dynamics;
mod effects;
mod filter;
mod fm;
mod fm_matrix;
mod input;
mod midi;
mod osc;
mod pitch;
mod routing;
mod samples;
mod synthesis;
mod timing;
mod visualization;
mod wavetable;

/// One installed control: the names that set it, and the reference entry
/// that documents it.
///
/// Compound controls spread a `:`-separated mini-notation atom across their
/// positional names, so `s` maps "bd:3:0.5" onto s/n/gain. Only the FIRST
/// positional name is an accessor; the others are output keys, and each one
/// that a score can spell has its own row. Aliases resolve to the same
/// control.
pub struct ControlRow {
    pub names: &'static [&'static str],
    pub aliases: &'static [&'static str],
    pub reference: ReferenceEntry,
}

/// Shared documentation for the `limit` control and its extension entry.
///
/// Studio identifies extensions through the extension registry, not a
/// control's `origin` field. `rustel-ext` includes this entry so both places
/// use the same description.
pub const LIMIT_REFERENCE: ReferenceEntry = ReferenceEntry {
    name: "limit",
    synonyms: &[],
    summary: "Brickwall limiter on one voice: a ceiling in dBFS, and a character.",
    description: "A brickwall limiter on one voice. Called as `limit(-6, \"hard\")` or as `limit(\"-6:hard\")`: the ceiling in dBFS first, then the character - `transparent`, `punchy`, `warm` or `hard` - which may be left off. The ceiling is an ordinary argument, so `limit(slider(-6, -24, 0), \"hard\")` puts a fader on it.\n\nIt holds one voice at a time, so several voices each under the ceiling can still add up over it. Use `all(x => x.limit(\"-3\"))` to put it across the whole stack.\n\nOne sample of lookahead, the same for every character, so a limited voice stays in time with an unlimited one.\n\nIt runs after the voice\'s gain and `stretch`, and before its delay and reverb sends.",
    params: &[
        ReferenceParam {
            name: "ceiling",
            r#type: "number | Pattern",
            description: "the ceiling in dBFS, at or below 0",
        },
        ReferenceParam {
            name: "character",
            r#type: "string | Pattern",
            description: "transparent, punchy, warm or hard",
        },
    ],
    examples: &[
        "s(\"bd*4\").distort(\"8:.3\").limit(\"-6\")",
        "s(\"bd*2, hh*8\").limit(-9, \"hard\")",
        "s(\"bd*4\").distort(\"8:.3\").limit(slider(-6, -24, 0), \"hard\")",
    ],
    tags: &["dynamics", "audio"],
    no_autocomplete: false,
    deprecated: false,
    origin: "rustel",
};

/// The authored table: one row for each control, with its names, its aliases
/// and its reference entry. [`CONTROLS`] and [`CONTROL_NAMES`] are built from
/// the rows, so a control has no way to exist without a reference entry.
const ROWS: &[ControlRow] = &[
    samples::S,
    wavetable::WT,
    wavetable::WTENV,
    wavetable::WTATTACK,
    wavetable::WTDECAY,
    wavetable::WTSUSTAIN,
    wavetable::WTRELEASE,
    wavetable::WTRATE,
    wavetable::WTSYNC,
    wavetable::WTDEPTH,
    wavetable::WTSHAPE,
    wavetable::WTDC,
    wavetable::WTSKEW,
    wavetable::WARP,
    wavetable::WARPATTACK,
    wavetable::WARPDECAY,
    wavetable::WARPSUSTAIN,
    wavetable::WARPRELEASE,
    wavetable::WARPRATE,
    wavetable::WARPDEPTH,
    wavetable::WARPSHAPE,
    wavetable::WARPDC,
    wavetable::WARPSKEW,
    wavetable::WARPMODE,
    wavetable::WTPHASERAND,
    wavetable::WARPENV,
    wavetable::WARPSYNC,
    input::SOURCE,
    samples::N,
    pitch::I,
    pitch::NOTE,
    osc::ACCELERATE,
    amplitude::VELOCITY,
    amplitude::GAIN,
    amplitude::POSTGAIN,
    osc::AMP,
    fm_matrix::FMI00,
    fm_matrix::FMI01,
    fm_matrix::FMI02,
    fm_matrix::FMI03,
    fm_matrix::FMI04,
    fm_matrix::FMI05,
    fm_matrix::FMI06,
    fm_matrix::FMI07,
    fm_matrix::FMI08,
    fm_matrix::FMI10,
    fm_matrix::FMI11,
    fm_matrix::FMI12,
    fm_matrix::FMI13,
    fm_matrix::FMI14,
    fm_matrix::FMI15,
    fm_matrix::FMI16,
    fm_matrix::FMI17,
    fm_matrix::FMI18,
    fm_matrix::FMI20,
    fm_matrix::FMI21,
    fm_matrix::FMI22,
    fm_matrix::FMI23,
    fm_matrix::FMI24,
    fm_matrix::FMI25,
    fm_matrix::FMI26,
    fm_matrix::FMI27,
    fm_matrix::FMI28,
    fm_matrix::FMI30,
    fm_matrix::FMI31,
    fm_matrix::FMI32,
    fm_matrix::FMI33,
    fm_matrix::FMI34,
    fm_matrix::FMI35,
    fm_matrix::FMI36,
    fm_matrix::FMI37,
    fm_matrix::FMI38,
    fm_matrix::FMI40,
    fm_matrix::FMI41,
    fm_matrix::FMI42,
    fm_matrix::FMI43,
    fm_matrix::FMI44,
    fm_matrix::FMI45,
    fm_matrix::FMI46,
    fm_matrix::FMI47,
    fm_matrix::FMI48,
    fm_matrix::FMI50,
    fm_matrix::FMI51,
    fm_matrix::FMI52,
    fm_matrix::FMI53,
    fm_matrix::FMI54,
    fm_matrix::FMI55,
    fm_matrix::FMI56,
    fm_matrix::FMI57,
    fm_matrix::FMI58,
    fm_matrix::FMI60,
    fm_matrix::FMI61,
    fm_matrix::FMI62,
    fm_matrix::FMI63,
    fm_matrix::FMI64,
    fm_matrix::FMI65,
    fm_matrix::FMI66,
    fm_matrix::FMI67,
    fm_matrix::FMI68,
    fm_matrix::FMI70,
    fm_matrix::FMI71,
    fm_matrix::FMI72,
    fm_matrix::FMI73,
    fm_matrix::FMI74,
    fm_matrix::FMI75,
    fm_matrix::FMI76,
    fm_matrix::FMI77,
    fm_matrix::FMI78,
    fm_matrix::FMI80,
    fm_matrix::FMI81,
    fm_matrix::FMI82,
    fm_matrix::FMI83,
    fm_matrix::FMI84,
    fm_matrix::FMI85,
    fm_matrix::FMI86,
    fm_matrix::FMI87,
    fm_matrix::FMI88,
    samples::BANK,
    osc::CHORUS,
    osc::ANALYZE,
    osc::FFT,
    amplitude::ATTACK,
    amplitude::DECAY,
    amplitude::SUSTAIN,
    amplitude::RELEASE,
    osc::HOLD,
    filter::BANDF,
    filter::BANDQ,
    samples::BEGIN,
    samples::END,
    samples::LOOP,
    samples::LOOP_BEGIN,
    samples::LOOP_END,
    effects::CRUSH,
    effects::COARSE,
    amplitude::TREMOLO,
    amplitude::TREMOLOSYNC,
    amplitude::TREMOLODEPTH,
    amplitude::TREMOLOSKEW,
    amplitude::TREMOLOPHASE,
    amplitude::TREMOLOSHAPE,
    filter::DRIVE,
    routing::DUCKORBIT,
    routing::DUCKDEPTH,
    routing::DUCKONSET,
    routing::DUCKATTACK,
    synthesis::BYTE_BEAT_EXPRESSION,
    synthesis::BYTE_BEAT_START_TIME,
    input::CHANNELS,
    synthesis::PW,
    synthesis::PWRATE,
    synthesis::PWSWEEP,
    effects::PHASERRATE,
    effects::PHASERSWEEP,
    effects::PHASERCENTER,
    effects::PHASERDEPTH,
    osc::CHANNEL,
    samples::CUT,
    filter::CUTOFF,
    filter::LPENV,
    filter::HPENV,
    filter::BPENV,
    filter::LPATTACK,
    filter::HPATTACK,
    filter::BPATTACK,
    filter::LPDECAY,
    filter::HPDECAY,
    filter::BPDECAY,
    filter::LPSUSTAIN,
    filter::HPSUSTAIN,
    filter::BPSUSTAIN,
    filter::LPRELEASE,
    filter::HPRELEASE,
    filter::BPRELEASE,
    filter::FTYPE,
    filter::FANCHOR,
    filter::LPRATE,
    filter::LPSYNC,
    filter::LPDEPTH,
    filter::LPDEPTHFREQUENCY,
    filter::LPSHAPE,
    filter::LPDC,
    filter::LPSKEW,
    filter::BPRATE,
    filter::BPSYNC,
    filter::BPDEPTH,
    filter::BPDEPTHFREQUENCY,
    filter::BPSHAPE,
    filter::BPDC,
    filter::BPSKEW,
    filter::HPRATE,
    filter::HPSYNC,
    filter::HPDEPTH,
    filter::HPDEPTHFREQUENCY,
    filter::HPSHAPE,
    filter::HPDC,
    filter::HPSKEW,
    pitch::VIB,
    synthesis::NOISE,
    pitch::VIBMOD,
    filter::HCUTOFF,
    filter::HRESONANCE,
    filter::RESONANCE,
    filter::DJF,
    routing::DELAY,
    routing::DELAYFEEDBACK,
    routing::DELAYSPEED,
    routing::DELAYTIME,
    routing::DELAYSYNC,
    osc::LOCK,
    pitch::DETUNE,
    synthesis::UNISON,
    synthesis::SPREAD,
    routing::DRY,
    osc::FADE_TIME,
    osc::FADE_IN_TIME,
    pitch::FREQ,
    pitch::PATTACK,
    pitch::PDECAY,
    pitch::PSUSTAIN,
    pitch::PRELEASE,
    pitch::PENV,
    pitch::PCURVE,
    pitch::PANCHOR,
    osc::GATE,
    osc::LESLIE,
    osc::LRATE,
    osc::LSIZE,
    visualization::ACTIVE_LABEL,
    visualization::LABEL,
    pitch::DEGREE,
    osc::MTRANSPOSE,
    osc::CTRANSPOSE,
    osc::HARMONIC,
    osc::STEPS_PER_OCTAVE,
    osc::OCTAVE_R,
    samples::NUDGE,
    pitch::OCTAVE,
    routing::ORBIT,
    routing::BUS,
    routing::BUSGAIN,
    osc::OVERGAIN,
    osc::OVERSHAPE,
    routing::PAN,
    osc::PANSPAN,
    osc::PANSPLAY,
    osc::PANWIDTH,
    osc::PANORIENT,
    synthesis::SLIDE,
    osc::SEMITONE,
    osc::VOICE,
    pitch::CHORD,
    pitch::DICTIONARY,
    pitch::ANCHOR,
    pitch::OFFSET,
    pitch::OCTAVES,
    pitch::MODE,
    routing::ROOM,
    routing::ROOMLP,
    routing::ROOMDIM,
    routing::ROOMFADE,
    routing::IR,
    routing::IRSPEED,
    routing::IRBEGIN,
    routing::ROOMSIZE,
    effects::SHAPE,
    dynamics::LIMITCHAR,
    dynamics::LIMIT,
    effects::DISTORT,
    effects::DISTORTVOL,
    effects::DIODE,
    effects::DISTORTTYPE,
    dynamics::COMPRESSOR,
    dynamics::COMPRESSOR_KNEE,
    dynamics::COMPRESSOR_RATIO,
    dynamics::COMPRESSOR_ATTACK,
    dynamics::COMPRESSOR_RELEASE,
    samples::SPEED,
    samples::STRETCH,
    samples::UNIT,
    osc::SQUIZ,
    effects::VOWEL,
    osc::WAVELOSS,
    effects::DENSITY,
    osc::EXPRESSION,
    osc::SUSTAINPEDAL,
    osc::FSHIFT,
    osc::FSHIFTNOTE,
    osc::FSHIFTPHASE,
    osc::TRIODE,
    osc::KRUSH,
    osc::KCUTOFF,
    osc::OCTER,
    osc::OCTERSUB,
    osc::OCTERSUBSUB,
    osc::RING,
    osc::RINGF,
    osc::RINGDF,
    osc::FREEZE,
    osc::XSDELAY,
    osc::TSDELAY,
    osc::REAL,
    osc::IMAG,
    osc::ENHANCE,
    osc::COMB,
    osc::SMEAR,
    osc::SCRAM,
    osc::BINSHIFT,
    osc::HBRICK,
    osc::LBRICK,
    osc::FRAME_RATE,
    osc::FRAMES,
    osc::HOURS,
    osc::MINUTES,
    osc::SECONDS,
    osc::SONG_PTR,
    osc::UID,
    osc::VAL,
    timing::CPS,
    amplitude::CLIP,
    amplitude::DURATION,
    synthesis::ZRAND,
    synthesis::CURVE,
    synthesis::SLIDE_2,
    synthesis::DELTA_SLIDE,
    synthesis::PITCH_JUMP,
    synthesis::PITCH_JUMP_TIME,
    synthesis::ZNOISE,
    synthesis::ZMOD,
    synthesis::ZCRUSH,
    synthesis::ZDELAY,
    synthesis::ZZFX,
    visualization::COLOR,
    midi::MIDICHAN,
    midi::MIDIMAP,
    midi::MIDIPORT,
    midi::MIDICMD,
    midi::CCN,
    midi::CCV,
    midi::CTL_NUM,
    midi::NRPNN,
    midi::NRPV,
    midi::PROG_NUM,
    midi::SYSEXID,
    midi::SYSEXDATA,
    midi::MIDIBEND,
    midi::MIDITOUCH,
    midi::POLY_TOUCH,
    osc::OSCHOST,
    osc::OSCPORT,
    effects::TRANSIENT,
    amplitude::FXRELEASE,
    fm::FMH,
    fm::FMH2,
    fm::FMH3,
    fm::FMH4,
    fm::FMH5,
    fm::FMH6,
    fm::FMH7,
    fm::FMH8,
    fm::FMI,
    fm::FMI2,
    fm::FMI3,
    fm::FMI4,
    fm::FMI5,
    fm::FMI6,
    fm::FMI7,
    fm::FMI8,
    fm::FMENV,
    fm::FMENV2,
    fm::FMENV3,
    fm::FMENV4,
    fm::FMENV5,
    fm::FMENV6,
    fm::FMENV7,
    fm::FMENV8,
    fm::FMATTACK,
    fm::FMATTACK2,
    fm::FMATTACK3,
    fm::FMATTACK4,
    fm::FMATTACK5,
    fm::FMATTACK6,
    fm::FMATTACK7,
    fm::FMATTACK8,
    fm::FMWAVE,
    fm::FMWAVE2,
    fm::FMWAVE3,
    fm::FMWAVE4,
    fm::FMWAVE5,
    fm::FMWAVE6,
    fm::FMWAVE7,
    fm::FMWAVE8,
    fm::FMDECAY,
    fm::FMDECAY2,
    fm::FMDECAY3,
    fm::FMDECAY4,
    fm::FMDECAY5,
    fm::FMDECAY6,
    fm::FMDECAY7,
    fm::FMDECAY8,
    fm::FMSUSTAIN,
    fm::FMSUSTAIN2,
    fm::FMSUSTAIN3,
    fm::FMSUSTAIN4,
    fm::FMSUSTAIN5,
    fm::FMSUSTAIN6,
    fm::FMSUSTAIN7,
    fm::FMSUSTAIN8,
    fm::FMRELEASE,
    fm::FMRELEASE2,
    fm::FMRELEASE3,
    fm::FMRELEASE4,
    fm::FMRELEASE5,
    fm::FMRELEASE6,
    fm::FMRELEASE7,
    fm::FMRELEASE8,
];

/// The complete pinned control surface, reference entries included. A build
/// links the reference text only when the build reads this table. Code which
/// shows the text reads this table.
pub static CONTROLS: &[ControlRow] = ROWS;

/// The names and aliases of one control, with no reference entry. The slices
/// are the ones of the matching [`ControlRow`].
#[derive(Clone, Copy)]
pub struct ControlNames {
    /// The accessor names, as in [`ControlRow::names`].
    pub names: &'static [&'static str],
    /// The aliases, as in [`ControlRow::aliases`].
    pub aliases: &'static [&'static str],
}

/// The names and aliases of every control, in table order. No item points to
/// a reference entry. The control registry reads this table. A build with a
/// size limit reads this table and not [`CONTROLS`], so the linker drops the
/// reference text.
pub static CONTROL_NAMES: [ControlNames; ROWS.len()] = {
    let mut table = [ControlNames { names: &[], aliases: &[] }; ROWS.len()];
    let mut index = 0;
    while index < ROWS.len() {
        table[index] = ControlNames {
            names: ROWS[index].names,
            aliases: ROWS[index].aliases,
        };
        index += 1;
    }
    table
};

/// Every control's reference entry, in table order.
pub fn reference_entries() -> impl Iterator<Item = &'static ReferenceEntry> {
    CONTROLS.iter().map(|row| &row.reference)
}

/// FM spellings omitted from the reference because neither Rustel's nor
/// Strudel's FM resolver reads them. Unsupported features remain documented
/// so readers can find the native limitations.
///
/// Names match each entry's `name`; aliases follow the same visibility.
pub const REFERENCE_HIDDEN: &[&str] = &[
    // Both FM resolvers read sources 1..8. Source 0 does not exist, and
    // adjacent source/destination pairs use fmi, fmi2, ..., fmi8 instead.
    "fmi00", "fmi01", "fmi02", "fmi03", "fmi04", "fmi05", "fmi06", "fmi07", "fmi08",
    "fmi10", "fmi21", "fmi32", "fmi43", "fmi54", "fmi65", "fmi76", "fmi87",
];

/// Whether a control's reference entry shows in the reference.
pub fn reference_shows(name: &str) -> bool {
    !REFERENCE_HIDDEN.contains(&name)
}

/// Every control's reference entry the reference shows, in table order.
/// The engine's surface is unaffected: [`crate::controls`] registers every
/// row whatever this filter says, so a score spelling a hidden name still
/// evaluates.
pub fn visible_reference_entries() -> impl Iterator<Item = &'static ReferenceEntry> {
    CONTROLS
        .iter()
        .map(|row| &row.reference)
        .filter(|entry| reference_shows(entry.name))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Hidden names must identify catalog entries.
    #[test]
    fn every_hidden_name_is_a_table_entry() {
        let documented: std::collections::BTreeSet<&str> =
            CONTROLS.iter().map(|row| row.reference.name).collect();
        for hidden in REFERENCE_HIDDEN {
            assert!(
                documented.contains(*hidden),
                "`{hidden}` is hidden from the reference but no entry documents it"
            );
        }
    }

    /// The hidden list cannot contain duplicates or aliases.
    #[test]
    fn the_hidden_list_has_no_duplicates_or_aliases() {
        let mut seen = std::collections::BTreeSet::new();
        for hidden in REFERENCE_HIDDEN {
            assert!(seen.insert(*hidden), "`{hidden}` is hidden twice");
        }
        let aliases: std::collections::BTreeSet<&str> = CONTROLS
            .iter()
            .flat_map(|row| row.aliases.iter().copied())
            .collect();
        for hidden in REFERENCE_HIDDEN {
            assert!(
                !aliases.contains(hidden),
                "`{hidden}` is an alias; hide the entry's own name instead"
            );
        }
    }

    /// Hidden entries must remain registered.
    #[test]
    fn hiding_an_entry_hides_nothing_the_engine_installs() {
        for row in CONTROLS {
            if !reference_shows(row.reference.name) {
                for name in row.names.iter().chain(row.aliases.iter()) {
                    assert!(
                        crate::controls::canonical_control_name(name).is_some(),
                        "`{name}` is hidden from the reference but no longer installed"
                    );
                }
            }
        }
    }

    #[test]
    fn unsupported_controls_remain_documented() {
        let visible: std::collections::BTreeSet<&str> = visible_reference_entries()
            .map(|entry| entry.name)
            .collect();
        for name in [
            "chorus", "analyze", "fft", "waveloss", "fshift", "fshiftnote",
            "fshiftphase", "triode", "krush", "kcutoff", "octer", "octersub",
            "octersubsub", "ring", "ringf", "ringdf", "freeze", "xsdelay", "tsdelay",
            "real", "imag", "enhance", "comb", "smear", "scram", "binshift", "hbrick",
            "lbrick", "hold", "gate", "overgain", "overshape", "panspan", "pansplay",
            "panwidth", "panorient", "fadeInTime", "mtranspose", "ctranspose",
            "harmonic", "stepsPerOctave", "octaveR", "semitone", "voice", "expression",
            "sustainpedal", "frameRate", "frames", "hours", "minutes", "seconds",
            "songPtr", "uid", "val", "channel",
        ] {
            assert!(
                visible.contains(name),
                "`{name}` needs a reference entry explaining its native limitation"
            );
        }
    }

    /// Visible reference entries describe Rustel's behavior.
    #[test]
    fn visible_entries_never_name_the_upstream_implementation() {
        for entry in visible_reference_entries() {
            for text in [entry.summary, entry.description] {
                let lowered = text.to_lowercase();
                for word in ["upstream", "strudel.cc", "strudel"] {
                    assert!(
                        !lowered.contains(word),
                        "`{}`'s visible reference text names `{word}`: {}",
                        entry.name,
                        text
                    );
                }
            }
        }
    }
}
