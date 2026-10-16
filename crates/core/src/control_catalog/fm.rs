use super::{ControlRow, ReferenceEntry, ReferenceParam};

pub(super) const FMH: ControlRow = ControlRow {
    names: &["fmh", "fmi"],
    aliases: &["fmh", "fmh1", "fmh1", "fmi1"],
    reference: ReferenceEntry {
        name: "fmh",
        synonyms: &[],
        summary: "FM operator 1 harmonicity ratio.",
        description: "Defaults to 1. For an oscillator operator, 2 doubles its frequency, while non-integer ratios create inharmonic sidebands. For a noise operator, harmonicity scales the outgoing modulation depth without changing the noise playback pitch. fmh2 through fmh8 address the other operators. Requires a nonzero FM route involving operator 1 and a connected path to the carrier; fmi (alias fm) supplies the direct route. The FM matrix affects basic oscillators, supersaw, pulse, and bytebeat. Recorded samples, `gm_*` soundfonts, `wt_*` wavetables, ZZFX, sbd, standalone noise, live input, and buses ignore it.",
        params: &[
            ReferenceParam {
                name: "harmonicity",
                r#type: "number | Pattern",
                description: "",
            },
        ],
        examples: &[
            "note(\"c e g b g e\")\n.fm(4)\n.fmh(\"<1 2 1.5 1.61>\")\n._scope()",
        ],
        tags: &["fm", "audio"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
};

pub(super) const FMH2: ControlRow = ControlRow {
    names: &["fmh2", "fmi2"],
    aliases: &["fmh2"],
    reference:         ReferenceEntry {
        name: "fmh2",
        synonyms: &[],
        summary: "operator 2's harmonicity ratio",
        description: "How operator 2's frequency relates to the carrier's: 1 tracks it, 2 doubles it, 1.5 lands a fifth above, and inharmonic decimals like 1.61 read as metallic. The default is 1. Whole numbers and simple ratios sound natural; the operator must exist - a route naming it - before this has anything to bend.",
        params: &[
            ReferenceParam {
                name: "value",
                r#type: "number | Pattern",
                description: "harmonicity ratio; default 1",
            },
        ],
        examples: &[
            "note(\"c2 e2 g2\").s(\"sine\").fm2(2).fmh2(\"<1 2 1.5 1.61>\")",
        ],
        tags: &["control", "fm"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
};

pub(super) const FMH3: ControlRow = ControlRow {
    names: &["fmh3", "fmi3"],
    aliases: &["fmh3"],
    reference:         ReferenceEntry {
        name: "fmh3",
        synonyms: &[],
        summary: "operator 3's harmonicity ratio",
        description: "How operator 3's frequency relates to the carrier's: 1 tracks it, 2 doubles it, 1.5 lands a fifth above, and inharmonic decimals like 1.61 read as metallic. The default is 1. Whole numbers and simple ratios sound natural; the operator must exist - a route naming it - before this has anything to bend.",
        params: &[
            ReferenceParam {
                name: "value",
                r#type: "number | Pattern",
                description: "harmonicity ratio; default 1",
            },
        ],
        examples: &[
            "note(\"c2 e2 g2\").s(\"sine\").fm3(2).fmh3(\"<1 2 1.5 1.61>\")",
        ],
        tags: &["control", "fm"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
};

pub(super) const FMH4: ControlRow = ControlRow {
    names: &["fmh4", "fmi4"],
    aliases: &["fmh4"],
    reference:         ReferenceEntry {
        name: "fmh4",
        synonyms: &[],
        summary: "operator 4's harmonicity ratio",
        description: "How operator 4's frequency relates to the carrier's: 1 tracks it, 2 doubles it, 1.5 lands a fifth above, and inharmonic decimals like 1.61 read as metallic. The default is 1. Whole numbers and simple ratios sound natural; the operator must exist - a route naming it - before this has anything to bend.",
        params: &[
            ReferenceParam {
                name: "value",
                r#type: "number | Pattern",
                description: "harmonicity ratio; default 1",
            },
        ],
        examples: &[
            "note(\"c2 e2 g2\").s(\"sine\").fm4(2).fmh4(\"<1 2 1.5 1.61>\")",
        ],
        tags: &["control", "fm"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
};

pub(super) const FMH5: ControlRow = ControlRow {
    names: &["fmh5", "fmi5"],
    aliases: &["fmh5"],
    reference:         ReferenceEntry {
        name: "fmh5",
        synonyms: &[],
        summary: "operator 5's harmonicity ratio",
        description: "How operator 5's frequency relates to the carrier's: 1 tracks it, 2 doubles it, 1.5 lands a fifth above, and inharmonic decimals like 1.61 read as metallic. The default is 1. Whole numbers and simple ratios sound natural; the operator must exist - a route naming it - before this has anything to bend.",
        params: &[
            ReferenceParam {
                name: "value",
                r#type: "number | Pattern",
                description: "harmonicity ratio; default 1",
            },
        ],
        examples: &[
            "note(\"c2 e2 g2\").s(\"sine\").fm5(2).fmh5(\"<1 2 1.5 1.61>\")",
        ],
        tags: &["control", "fm"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
};

pub(super) const FMH6: ControlRow = ControlRow {
    names: &["fmh6", "fmi6"],
    aliases: &["fmh6"],
    reference:         ReferenceEntry {
        name: "fmh6",
        synonyms: &[],
        summary: "operator 6's harmonicity ratio",
        description: "How operator 6's frequency relates to the carrier's: 1 tracks it, 2 doubles it, 1.5 lands a fifth above, and inharmonic decimals like 1.61 read as metallic. The default is 1. Whole numbers and simple ratios sound natural; the operator must exist - a route naming it - before this has anything to bend.",
        params: &[
            ReferenceParam {
                name: "value",
                r#type: "number | Pattern",
                description: "harmonicity ratio; default 1",
            },
        ],
        examples: &[
            "note(\"c2 e2 g2\").s(\"sine\").fm6(2).fmh6(\"<1 2 1.5 1.61>\")",
        ],
        tags: &["control", "fm"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
};

pub(super) const FMH7: ControlRow = ControlRow {
    names: &["fmh7", "fmi7"],
    aliases: &["fmh7"],
    reference:         ReferenceEntry {
        name: "fmh7",
        synonyms: &[],
        summary: "operator 7's harmonicity ratio",
        description: "How operator 7's frequency relates to the carrier's: 1 tracks it, 2 doubles it, 1.5 lands a fifth above, and inharmonic decimals like 1.61 read as metallic. The default is 1. Whole numbers and simple ratios sound natural; the operator must exist - a route naming it - before this has anything to bend.",
        params: &[
            ReferenceParam {
                name: "value",
                r#type: "number | Pattern",
                description: "harmonicity ratio; default 1",
            },
        ],
        examples: &[
            "note(\"c2 e2 g2\").s(\"sine\").fm7(2).fmh7(\"<1 2 1.5 1.61>\")",
        ],
        tags: &["control", "fm"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
};

pub(super) const FMH8: ControlRow = ControlRow {
    names: &["fmh8", "fmi8"],
    aliases: &["fmh8"],
    reference:         ReferenceEntry {
        name: "fmh8",
        synonyms: &[],
        summary: "operator 8's harmonicity ratio",
        description: "How operator 8's frequency relates to the carrier's: 1 tracks it, 2 doubles it, 1.5 lands a fifth above, and inharmonic decimals like 1.61 read as metallic. The default is 1. Whole numbers and simple ratios sound natural; the operator must exist - a route naming it - before this has anything to bend.",
        params: &[
            ReferenceParam {
                name: "value",
                r#type: "number | Pattern",
                description: "harmonicity ratio; default 1",
            },
        ],
        examples: &[
            "note(\"c2 e2 g2\").s(\"sine\").fm8(2).fmh8(\"<1 2 1.5 1.61>\")",
        ],
        tags: &["control", "fm"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
};

pub(super) const FMI: ControlRow = ControlRow {
    names: &["fmi", "fmh"],
    aliases: &["fm", "fm1", "fmi1", "fmh1"],
    reference: ReferenceEntry {
        name: "fmi",
        synonyms: &["fm", "fm1", "fmi1", "fmh1"],
        summary: "Modulation index from FM operator 1 into the carrier.",
        description: "The unitless index sets brightness: the frequency deviation scales with carrier frequency and operator harmonicity. Absent or 0 makes no connection. fmi2/fm2 sends operator 2 into operator 1, continuing up to operator 8; a missing link breaks the chain. Matrix routes such as fmi20 send an operator directly to another target (0 is the carrier). At most 16 nonzero routes are accepted. The FM matrix affects basic oscillators, supersaw, pulse, and bytebeat. Recorded samples, `gm_*` soundfonts, `wt_*` wavetables, ZZFX, sbd, standalone noise, live input, and buses ignore it.",
        params: &[
            ReferenceParam {
                name: "brightness",
                r#type: "number | Pattern",
                description: "modulation index",
            },
        ],
        examples: &[
            "note(\"c e g b g e\")\n.fm(\"<0 1 2 8 32>\")\n._scope()",
            "s(\"sine\").note(\"F1\").seg(8)\n .fm(4).fm2(rand.mul(4)).fm3(saw.mul(8).slow(8))\n .fmh(1.06).fmh2(10).fmh3(0.1)",
        ],
        tags: &["fm", "audio"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
};

pub(super) const FMI2: ControlRow = ControlRow {
    names: &["fmi2", "fmh2"],
    aliases: &["fm2"],
    reference:         ReferenceEntry {
        name: "fmi2",
        synonyms: &["fm2"],
        summary: "how hard operator 2 modulates operator 1",
        description: "The plain-chain route, also spelled fm2: operator 2 into the one below it on the way to the carrier. The value is the modulation index - brightness - and zero or absent makes no route, which severs the chain at that link. The value is a modulation depth - an index, not hertz: the deviation it produces scales with the carrier and the source's harmonicity. A zero or absent amount makes no connection at all, and a gap severs the chain rather than being stepped over. Sixteen routes per event is the ceiling; beyond it the event is refused. The matrix bends the oscillator voices - sine, triangle, square, sawtooth, supersaw, pulse, bytebeat; samples, wavetables and the zzfx family ignore it.",
        params: &[
            ReferenceParam {
                name: "value",
                r#type: "number | Pattern",
                description: "modulation index; 0 or absent = no route",
            },
        ],
        examples: &[
            "note(\"c2 e2 g2\").s(\"sine\").fm(2).fm2(\"<0 1 4 8>\")",
        ],
        tags: &["control", "fm"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
};

pub(super) const FMI3: ControlRow = ControlRow {
    names: &["fmi3", "fmh3"],
    aliases: &["fm3"],
    reference:         ReferenceEntry {
        name: "fmi3",
        synonyms: &["fm3"],
        summary: "how hard operator 3 modulates operator 2",
        description: "The plain-chain route, also spelled fm3: operator 3 into the one below it on the way to the carrier. The value is the modulation index - brightness - and zero or absent makes no route, which severs the chain at that link. The value is a modulation depth - an index, not hertz: the deviation it produces scales with the carrier and the source's harmonicity. A zero or absent amount makes no connection at all, and a gap severs the chain rather than being stepped over. Sixteen routes per event is the ceiling; beyond it the event is refused. The matrix bends the oscillator voices - sine, triangle, square, sawtooth, supersaw, pulse, bytebeat; samples, wavetables and the zzfx family ignore it.",
        params: &[
            ReferenceParam {
                name: "value",
                r#type: "number | Pattern",
                description: "modulation index; 0 or absent = no route",
            },
        ],
        examples: &[
            "note(\"c2 e2 g2\").s(\"sine\").fm(2).fm3(\"<0 1 4 8>\")",
        ],
        tags: &["control", "fm"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
};

pub(super) const FMI4: ControlRow = ControlRow {
    names: &["fmi4", "fmh4"],
    aliases: &["fm4"],
    reference:         ReferenceEntry {
        name: "fmi4",
        synonyms: &["fm4"],
        summary: "how hard operator 4 modulates operator 3",
        description: "The plain-chain route, also spelled fm4: operator 4 into the one below it on the way to the carrier. The value is the modulation index - brightness - and zero or absent makes no route, which severs the chain at that link. The value is a modulation depth - an index, not hertz: the deviation it produces scales with the carrier and the source's harmonicity. A zero or absent amount makes no connection at all, and a gap severs the chain rather than being stepped over. Sixteen routes per event is the ceiling; beyond it the event is refused. The matrix bends the oscillator voices - sine, triangle, square, sawtooth, supersaw, pulse, bytebeat; samples, wavetables and the zzfx family ignore it.",
        params: &[
            ReferenceParam {
                name: "value",
                r#type: "number | Pattern",
                description: "modulation index; 0 or absent = no route",
            },
        ],
        examples: &[
            "note(\"c2 e2 g2\").s(\"sine\").fm(2).fm4(\"<0 1 4 8>\")",
        ],
        tags: &["control", "fm"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
};

pub(super) const FMI5: ControlRow = ControlRow {
    names: &["fmi5", "fmh5"],
    aliases: &["fm5"],
    reference:         ReferenceEntry {
        name: "fmi5",
        synonyms: &["fm5"],
        summary: "how hard operator 5 modulates operator 4",
        description: "The plain-chain route, also spelled fm5: operator 5 into the one below it on the way to the carrier. The value is the modulation index - brightness - and zero or absent makes no route, which severs the chain at that link. The value is a modulation depth - an index, not hertz: the deviation it produces scales with the carrier and the source's harmonicity. A zero or absent amount makes no connection at all, and a gap severs the chain rather than being stepped over. Sixteen routes per event is the ceiling; beyond it the event is refused. The matrix bends the oscillator voices - sine, triangle, square, sawtooth, supersaw, pulse, bytebeat; samples, wavetables and the zzfx family ignore it.",
        params: &[
            ReferenceParam {
                name: "value",
                r#type: "number | Pattern",
                description: "modulation index; 0 or absent = no route",
            },
        ],
        examples: &[
            "note(\"c2 e2 g2\").s(\"sine\").fm(2).fm5(\"<0 1 4 8>\")",
        ],
        tags: &["control", "fm"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
};

pub(super) const FMI6: ControlRow = ControlRow {
    names: &["fmi6", "fmh6"],
    aliases: &["fm6"],
    reference:         ReferenceEntry {
        name: "fmi6",
        synonyms: &["fm6"],
        summary: "how hard operator 6 modulates operator 5",
        description: "The plain-chain route, also spelled fm6: operator 6 into the one below it on the way to the carrier. The value is the modulation index - brightness - and zero or absent makes no route, which severs the chain at that link. The value is a modulation depth - an index, not hertz: the deviation it produces scales with the carrier and the source's harmonicity. A zero or absent amount makes no connection at all, and a gap severs the chain rather than being stepped over. Sixteen routes per event is the ceiling; beyond it the event is refused. The matrix bends the oscillator voices - sine, triangle, square, sawtooth, supersaw, pulse, bytebeat; samples, wavetables and the zzfx family ignore it.",
        params: &[
            ReferenceParam {
                name: "value",
                r#type: "number | Pattern",
                description: "modulation index; 0 or absent = no route",
            },
        ],
        examples: &[
            "note(\"c2 e2 g2\").s(\"sine\").fm(2).fm6(\"<0 1 4 8>\")",
        ],
        tags: &["control", "fm"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
};

pub(super) const FMI7: ControlRow = ControlRow {
    names: &["fmi7", "fmh7"],
    aliases: &["fm7"],
    reference:         ReferenceEntry {
        name: "fmi7",
        synonyms: &["fm7"],
        summary: "how hard operator 7 modulates operator 6",
        description: "The plain-chain route, also spelled fm7: operator 7 into the one below it on the way to the carrier. The value is the modulation index - brightness - and zero or absent makes no route, which severs the chain at that link. The value is a modulation depth - an index, not hertz: the deviation it produces scales with the carrier and the source's harmonicity. A zero or absent amount makes no connection at all, and a gap severs the chain rather than being stepped over. Sixteen routes per event is the ceiling; beyond it the event is refused. The matrix bends the oscillator voices - sine, triangle, square, sawtooth, supersaw, pulse, bytebeat; samples, wavetables and the zzfx family ignore it.",
        params: &[
            ReferenceParam {
                name: "value",
                r#type: "number | Pattern",
                description: "modulation index; 0 or absent = no route",
            },
        ],
        examples: &[
            "note(\"c2 e2 g2\").s(\"sine\").fm(2).fm7(\"<0 1 4 8>\")",
        ],
        tags: &["control", "fm"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
};

pub(super) const FMI8: ControlRow = ControlRow {
    names: &["fmi8", "fmh8"],
    aliases: &["fm8"],
    reference:         ReferenceEntry {
        name: "fmi8",
        synonyms: &["fm8"],
        summary: "how hard operator 8 modulates operator 7",
        description: "The plain-chain route, also spelled fm8: operator 8 into the one below it on the way to the carrier. The value is the modulation index - brightness - and zero or absent makes no route, which severs the chain at that link. The value is a modulation depth - an index, not hertz: the deviation it produces scales with the carrier and the source's harmonicity. A zero or absent amount makes no connection at all, and a gap severs the chain rather than being stepped over. Sixteen routes per event is the ceiling; beyond it the event is refused. The matrix bends the oscillator voices - sine, triangle, square, sawtooth, supersaw, pulse, bytebeat; samples, wavetables and the zzfx family ignore it.",
        params: &[
            ReferenceParam {
                name: "value",
                r#type: "number | Pattern",
                description: "modulation index; 0 or absent = no route",
            },
        ],
        examples: &[
            "note(\"c2 e2 g2\").s(\"sine\").fm(2).fm8(\"<0 1 4 8>\")",
        ],
        tags: &["control", "fm"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
};

pub(super) const FMENV: ControlRow = ControlRow {
    names: &["fmenv"],
    aliases: &["fme", "fme1", "fmenv1"],
    reference: ReferenceEntry {
        name: "fmenv",
        synonyms: &["fme", "fme1", "fmenv1"],
        summary: "Curve of FM operator 1’s depth envelope.",
        description: "Defaults to exponential. lin or linear selects a linear curve; other strings keep exponential. The curve only matters after fmattack, fmdecay, fmsustain, or fmrelease creates a depth envelope; fmenv alone leaves the depth flat. Numbered forms select operators 2 through 8. Requires a nonzero FM route involving operator 1 and a connected path to the carrier; fmi (alias fm) supplies the direct route. The FM matrix affects basic oscillators, supersaw, pulse, and bytebeat. Recorded samples, `gm_*` soundfonts, `wt_*` wavetables, ZZFX, sbd, standalone noise, live input, and buses ignore it.",
        params: &[
            ReferenceParam {
                name: "type",
                r#type: "string | Pattern",
                description: "lin | exp",
            },
        ],
        examples: &[
            "note(\"c e g b g e\")\n.fm(4)\n.fmdecay(.2)\n.fmsustain(0)\n.fmenv(\"<exp lin>\")\n._scope()",
        ],
        tags: &["fm", "envelope", "audio"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
};

pub(super) const FMENV2: ControlRow = ControlRow {
    names: &["fmenv2"],
    aliases: &["fme2"],
    reference:         ReferenceEntry {
        name: "fmenv2",
        synonyms: &["fme2"],
        summary: "the shape of operator 2's depth envelope",
        description: "Exponential unless the value is \"lin\" or \"linear\". The envelope itself is the per-operator ADSR - fmattack2, fmdecay2, fmsustain2, fmrelease2 - which exists as soon as any of the four is named.",
        params: &[
            ReferenceParam {
                name: "value",
                r#type: "string | Pattern",
                description: "envelope kind; exponential unless \"lin\"",
            },
        ],
        examples: &[
            "note(\"c2 e2\").s(\"sine\").fm2(3).fmenv2(\"lin\")",
        ],
        tags: &["control", "fm"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
};

pub(super) const FMENV3: ControlRow = ControlRow {
    names: &["fmenv3"],
    aliases: &["fme3"],
    reference:         ReferenceEntry {
        name: "fmenv3",
        synonyms: &["fme3"],
        summary: "the shape of operator 3's depth envelope",
        description: "Exponential unless the value is \"lin\" or \"linear\". The envelope itself is the per-operator ADSR - fmattack3, fmdecay3, fmsustain3, fmrelease3 - which exists as soon as any of the four is named.",
        params: &[
            ReferenceParam {
                name: "value",
                r#type: "string | Pattern",
                description: "envelope kind; exponential unless \"lin\"",
            },
        ],
        examples: &[
            "note(\"c2 e2\").s(\"sine\").fm3(3).fmenv3(\"lin\")",
        ],
        tags: &["control", "fm"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
};

pub(super) const FMENV4: ControlRow = ControlRow {
    names: &["fmenv4"],
    aliases: &["fme4"],
    reference:         ReferenceEntry {
        name: "fmenv4",
        synonyms: &["fme4"],
        summary: "the shape of operator 4's depth envelope",
        description: "Exponential unless the value is \"lin\" or \"linear\". The envelope itself is the per-operator ADSR - fmattack4, fmdecay4, fmsustain4, fmrelease4 - which exists as soon as any of the four is named.",
        params: &[
            ReferenceParam {
                name: "value",
                r#type: "string | Pattern",
                description: "envelope kind; exponential unless \"lin\"",
            },
        ],
        examples: &[
            "note(\"c2 e2\").s(\"sine\").fm4(3).fmenv4(\"lin\")",
        ],
        tags: &["control", "fm"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
};

pub(super) const FMENV5: ControlRow = ControlRow {
    names: &["fmenv5"],
    aliases: &["fme5"],
    reference:         ReferenceEntry {
        name: "fmenv5",
        synonyms: &["fme5"],
        summary: "the shape of operator 5's depth envelope",
        description: "Exponential unless the value is \"lin\" or \"linear\". The envelope itself is the per-operator ADSR - fmattack5, fmdecay5, fmsustain5, fmrelease5 - which exists as soon as any of the four is named.",
        params: &[
            ReferenceParam {
                name: "value",
                r#type: "string | Pattern",
                description: "envelope kind; exponential unless \"lin\"",
            },
        ],
        examples: &[
            "note(\"c2 e2\").s(\"sine\").fm5(3).fmenv5(\"lin\")",
        ],
        tags: &["control", "fm"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
};

pub(super) const FMENV6: ControlRow = ControlRow {
    names: &["fmenv6"],
    aliases: &["fme6"],
    reference:         ReferenceEntry {
        name: "fmenv6",
        synonyms: &["fme6"],
        summary: "the shape of operator 6's depth envelope",
        description: "Exponential unless the value is \"lin\" or \"linear\". The envelope itself is the per-operator ADSR - fmattack6, fmdecay6, fmsustain6, fmrelease6 - which exists as soon as any of the four is named.",
        params: &[
            ReferenceParam {
                name: "value",
                r#type: "string | Pattern",
                description: "envelope kind; exponential unless \"lin\"",
            },
        ],
        examples: &[
            "note(\"c2 e2\").s(\"sine\").fm6(3).fmenv6(\"lin\")",
        ],
        tags: &["control", "fm"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
};

pub(super) const FMENV7: ControlRow = ControlRow {
    names: &["fmenv7"],
    aliases: &["fme7"],
    reference:         ReferenceEntry {
        name: "fmenv7",
        synonyms: &["fme7"],
        summary: "the shape of operator 7's depth envelope",
        description: "Exponential unless the value is \"lin\" or \"linear\". The envelope itself is the per-operator ADSR - fmattack7, fmdecay7, fmsustain7, fmrelease7 - which exists as soon as any of the four is named.",
        params: &[
            ReferenceParam {
                name: "value",
                r#type: "string | Pattern",
                description: "envelope kind; exponential unless \"lin\"",
            },
        ],
        examples: &[
            "note(\"c2 e2\").s(\"sine\").fm7(3).fmenv7(\"lin\")",
        ],
        tags: &["control", "fm"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
};

pub(super) const FMENV8: ControlRow = ControlRow {
    names: &["fmenv8"],
    aliases: &["fme8"],
    reference:         ReferenceEntry {
        name: "fmenv8",
        synonyms: &["fme8"],
        summary: "the shape of operator 8's depth envelope",
        description: "Exponential unless the value is \"lin\" or \"linear\". The envelope itself is the per-operator ADSR - fmattack8, fmdecay8, fmsustain8, fmrelease8 - which exists as soon as any of the four is named.",
        params: &[
            ReferenceParam {
                name: "value",
                r#type: "string | Pattern",
                description: "envelope kind; exponential unless \"lin\"",
            },
        ],
        examples: &[
            "note(\"c2 e2\").s(\"sine\").fm8(3).fmenv8(\"lin\")",
        ],
        tags: &["control", "fm"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
};

pub(super) const FMATTACK: ControlRow = ControlRow {
    names: &["fmattack"],
    aliases: &["fmatt", "fmatt1", "fmattack1"],
    reference: ReferenceEntry {
        name: "fmattack",
        synonyms: &["fmatt", "fmatt1", "fmattack1"],
        summary: "FM operator 1 depth attack, in seconds.",
        description: "Time for modulation depth to rise to its peak. Naming any fmattack, fmdecay, fmsustain, or fmrelease creates operator 1’s depth envelope; with none present the depth stays flat. Omitted attack/decay default to 0.001 s and release to 0.01 s. Omitted sustain is 0.001 when decay is set, otherwise 1. Attack/decay floor at 0.001 s, release at 0.01 s, and sustain caps at 1. Numbered forms select operators 2 through 8. Requires a nonzero FM route involving operator 1 and a connected path to the carrier; fmi (alias fm) supplies the direct route. The FM matrix affects basic oscillators, supersaw, pulse, and bytebeat. Recorded samples, `gm_*` soundfonts, `wt_*` wavetables, ZZFX, sbd, standalone noise, live input, and buses ignore it.",
        params: &[
            ReferenceParam {
                name: "time",
                r#type: "number | Pattern",
                description: "attack time",
            },
        ],
        examples: &[
            "note(\"c e g b g e\")\n.fm(4)\n.fmattack(\"<0 .05 .1 .2>\")\n._scope()",
        ],
        tags: &["fm", "envelope", "audio"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
};

pub(super) const FMATTACK2: ControlRow = ControlRow {
    names: &["fmattack2"],
    aliases: &["fmatt2"],
    reference:         ReferenceEntry {
        name: "fmattack2",
        synonyms: &["fmatt2"],
        summary: "operator 2's depth attack",
        description: "How fast operator 2's modulation depth opens. The per-operator depth ADSR - fmattack2, fmdecay2, fmsustain2, fmrelease2 - is built the moment ANY of the four is named; left alone, the depth is flat. Base values are attack 0.001, decay 0.001, sustain 1, release 0.01, with the conditional rule: naming decay without sustain drops sustain to its 0.001 floor. Attack and decay floor at 0.001 s, release at 0.01 s, and sustain caps at 1.",
        params: &[
            ReferenceParam {
                name: "value",
                r#type: "number | Pattern",
                description: "attack time in seconds; floored at 0.001",
            },
        ],
        examples: &[
            "note(\"c2 e2\").s(\"sine\").fm2(3).fmattack2(\"0.01 0.1 0.5\")",
        ],
        tags: &["control", "fm"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
};

pub(super) const FMATTACK3: ControlRow = ControlRow {
    names: &["fmattack3"],
    aliases: &["fmatt3"],
    reference:         ReferenceEntry {
        name: "fmattack3",
        synonyms: &["fmatt3"],
        summary: "operator 3's depth attack",
        description: "How fast operator 3's modulation depth opens. The per-operator depth ADSR - fmattack3, fmdecay3, fmsustain3, fmrelease3 - is built the moment ANY of the four is named; left alone, the depth is flat. Base values are attack 0.001, decay 0.001, sustain 1, release 0.01, with the conditional rule: naming decay without sustain drops sustain to its 0.001 floor. Attack and decay floor at 0.001 s, release at 0.01 s, and sustain caps at 1.",
        params: &[
            ReferenceParam {
                name: "value",
                r#type: "number | Pattern",
                description: "attack time in seconds; floored at 0.001",
            },
        ],
        examples: &[
            "note(\"c2 e2\").s(\"sine\").fm3(3).fmattack3(\"0.01 0.1 0.5\")",
        ],
        tags: &["control", "fm"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
};

pub(super) const FMATTACK4: ControlRow = ControlRow {
    names: &["fmattack4"],
    aliases: &["fmatt4"],
    reference:         ReferenceEntry {
        name: "fmattack4",
        synonyms: &["fmatt4"],
        summary: "operator 4's depth attack",
        description: "How fast operator 4's modulation depth opens. The per-operator depth ADSR - fmattack4, fmdecay4, fmsustain4, fmrelease4 - is built the moment ANY of the four is named; left alone, the depth is flat. Base values are attack 0.001, decay 0.001, sustain 1, release 0.01, with the conditional rule: naming decay without sustain drops sustain to its 0.001 floor. Attack and decay floor at 0.001 s, release at 0.01 s, and sustain caps at 1.",
        params: &[
            ReferenceParam {
                name: "value",
                r#type: "number | Pattern",
                description: "attack time in seconds; floored at 0.001",
            },
        ],
        examples: &[
            "note(\"c2 e2\").s(\"sine\").fm4(3).fmattack4(\"0.01 0.1 0.5\")",
        ],
        tags: &["control", "fm"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
};

pub(super) const FMATTACK5: ControlRow = ControlRow {
    names: &["fmattack5"],
    aliases: &["fmatt5"],
    reference:         ReferenceEntry {
        name: "fmattack5",
        synonyms: &["fmatt5"],
        summary: "operator 5's depth attack",
        description: "How fast operator 5's modulation depth opens. The per-operator depth ADSR - fmattack5, fmdecay5, fmsustain5, fmrelease5 - is built the moment ANY of the four is named; left alone, the depth is flat. Base values are attack 0.001, decay 0.001, sustain 1, release 0.01, with the conditional rule: naming decay without sustain drops sustain to its 0.001 floor. Attack and decay floor at 0.001 s, release at 0.01 s, and sustain caps at 1.",
        params: &[
            ReferenceParam {
                name: "value",
                r#type: "number | Pattern",
                description: "attack time in seconds; floored at 0.001",
            },
        ],
        examples: &[
            "note(\"c2 e2\").s(\"sine\").fm5(3).fmattack5(\"0.01 0.1 0.5\")",
        ],
        tags: &["control", "fm"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
};

pub(super) const FMATTACK6: ControlRow = ControlRow {
    names: &["fmattack6"],
    aliases: &["fmatt6"],
    reference:         ReferenceEntry {
        name: "fmattack6",
        synonyms: &["fmatt6"],
        summary: "operator 6's depth attack",
        description: "How fast operator 6's modulation depth opens. The per-operator depth ADSR - fmattack6, fmdecay6, fmsustain6, fmrelease6 - is built the moment ANY of the four is named; left alone, the depth is flat. Base values are attack 0.001, decay 0.001, sustain 1, release 0.01, with the conditional rule: naming decay without sustain drops sustain to its 0.001 floor. Attack and decay floor at 0.001 s, release at 0.01 s, and sustain caps at 1.",
        params: &[
            ReferenceParam {
                name: "value",
                r#type: "number | Pattern",
                description: "attack time in seconds; floored at 0.001",
            },
        ],
        examples: &[
            "note(\"c2 e2\").s(\"sine\").fm6(3).fmattack6(\"0.01 0.1 0.5\")",
        ],
        tags: &["control", "fm"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
};

pub(super) const FMATTACK7: ControlRow = ControlRow {
    names: &["fmattack7"],
    aliases: &["fmatt7"],
    reference:         ReferenceEntry {
        name: "fmattack7",
        synonyms: &["fmatt7"],
        summary: "operator 7's depth attack",
        description: "How fast operator 7's modulation depth opens. The per-operator depth ADSR - fmattack7, fmdecay7, fmsustain7, fmrelease7 - is built the moment ANY of the four is named; left alone, the depth is flat. Base values are attack 0.001, decay 0.001, sustain 1, release 0.01, with the conditional rule: naming decay without sustain drops sustain to its 0.001 floor. Attack and decay floor at 0.001 s, release at 0.01 s, and sustain caps at 1.",
        params: &[
            ReferenceParam {
                name: "value",
                r#type: "number | Pattern",
                description: "attack time in seconds; floored at 0.001",
            },
        ],
        examples: &[
            "note(\"c2 e2\").s(\"sine\").fm7(3).fmattack7(\"0.01 0.1 0.5\")",
        ],
        tags: &["control", "fm"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
};

pub(super) const FMATTACK8: ControlRow = ControlRow {
    names: &["fmattack8"],
    aliases: &["fmatt8"],
    reference:         ReferenceEntry {
        name: "fmattack8",
        synonyms: &["fmatt8"],
        summary: "operator 8's depth attack",
        description: "How fast operator 8's modulation depth opens. The per-operator depth ADSR - fmattack8, fmdecay8, fmsustain8, fmrelease8 - is built the moment ANY of the four is named; left alone, the depth is flat. Base values are attack 0.001, decay 0.001, sustain 1, release 0.01, with the conditional rule: naming decay without sustain drops sustain to its 0.001 floor. Attack and decay floor at 0.001 s, release at 0.01 s, and sustain caps at 1.",
        params: &[
            ReferenceParam {
                name: "value",
                r#type: "number | Pattern",
                description: "attack time in seconds; floored at 0.001",
            },
        ],
        examples: &[
            "note(\"c2 e2\").s(\"sine\").fm8(3).fmattack8(\"0.01 0.1 0.5\")",
        ],
        tags: &["control", "fm"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
};

pub(super) const FMWAVE: ControlRow = ControlRow {
    names: &["fmwave"],
    aliases: &["fmwave1"],
    reference: ReferenceEntry {
        name: "fmwave",
        synonyms: &["fmwave1"],
        summary: "Waveform of FM operator 1.",
        description: "Defaults to sine. Accepts sine, triangle/tri, square, sawtooth/saw, white, pink, brown, or crackle; unknown names reject an active operator. A noise operator uses a looping buffer with no pitched oscillator, and incoming FM routes do not modulate it. Numbered forms select operators 2 through 8. Requires a nonzero FM route involving operator 1 and a connected path to the carrier; fmi (alias fm) supplies the direct route. The FM matrix affects basic oscillators, supersaw, pulse, and bytebeat. Recorded samples, `gm_*` soundfonts, `wt_*` wavetables, ZZFX, sbd, standalone noise, live input, and buses ignore it.",
        params: &[
            ReferenceParam {
                name: "wave",
                r#type: "string | Pattern",
                description: "sine, triangle, square, sawtooth, white, pink, brown or crackle",
            },
        ],
        examples: &[
            "n(\"0 1 2 3\".fast(4)).scale(\"d:minor\").s(\"sine\").fmwave(\"<sine square sawtooth crackle>\").fm(4).fmh(2.01)",
            "n(\"0 1 2 3\".fast(4)).chord(\"<Dm Am F G>\").voicing().s(\"sawtooth\").fmwave(\"brown\").fm(.6)",
        ],
        tags: &["fm", "audio"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
};

pub(super) const FMWAVE2: ControlRow = ControlRow {
    names: &["fmwave2"],
    aliases: &[],
    reference:         ReferenceEntry {
        name: "fmwave2",
        synonyms: &[],
        summary: "operator 2's waveform",
        description: "The default is sine. Also accepted: triangle, square, sawtooth and the noise colours white, pink, brown and crackle - a noise operator is a looping buffer with no pitch, and nothing can modulate into one. Any other name refuses the event.",
        params: &[
            ReferenceParam {
                name: "value",
                r#type: "string | Pattern",
                description: "waveform name; default \"sine\"",
            },
        ],
        examples: &[
            "note(\"c2 e2\").s(\"sine\").fm2(3).fmwave2(\"square\")",
        ],
        tags: &["control", "fm"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
};

pub(super) const FMWAVE3: ControlRow = ControlRow {
    names: &["fmwave3"],
    aliases: &[],
    reference:         ReferenceEntry {
        name: "fmwave3",
        synonyms: &[],
        summary: "operator 3's waveform",
        description: "The default is sine. Also accepted: triangle, square, sawtooth and the noise colours white, pink, brown and crackle - a noise operator is a looping buffer with no pitch, and nothing can modulate into one. Any other name refuses the event.",
        params: &[
            ReferenceParam {
                name: "value",
                r#type: "string | Pattern",
                description: "waveform name; default \"sine\"",
            },
        ],
        examples: &[
            "note(\"c2 e2\").s(\"sine\").fm3(3).fmwave3(\"square\")",
        ],
        tags: &["control", "fm"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
};

pub(super) const FMWAVE4: ControlRow = ControlRow {
    names: &["fmwave4"],
    aliases: &[],
    reference:         ReferenceEntry {
        name: "fmwave4",
        synonyms: &[],
        summary: "operator 4's waveform",
        description: "The default is sine. Also accepted: triangle, square, sawtooth and the noise colours white, pink, brown and crackle - a noise operator is a looping buffer with no pitch, and nothing can modulate into one. Any other name refuses the event.",
        params: &[
            ReferenceParam {
                name: "value",
                r#type: "string | Pattern",
                description: "waveform name; default \"sine\"",
            },
        ],
        examples: &[
            "note(\"c2 e2\").s(\"sine\").fm4(3).fmwave4(\"square\")",
        ],
        tags: &["control", "fm"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
};

pub(super) const FMWAVE5: ControlRow = ControlRow {
    names: &["fmwave5"],
    aliases: &[],
    reference:         ReferenceEntry {
        name: "fmwave5",
        synonyms: &[],
        summary: "operator 5's waveform",
        description: "The default is sine. Also accepted: triangle, square, sawtooth and the noise colours white, pink, brown and crackle - a noise operator is a looping buffer with no pitch, and nothing can modulate into one. Any other name refuses the event.",
        params: &[
            ReferenceParam {
                name: "value",
                r#type: "string | Pattern",
                description: "waveform name; default \"sine\"",
            },
        ],
        examples: &[
            "note(\"c2 e2\").s(\"sine\").fm5(3).fmwave5(\"square\")",
        ],
        tags: &["control", "fm"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
};

pub(super) const FMWAVE6: ControlRow = ControlRow {
    names: &["fmwave6"],
    aliases: &[],
    reference:         ReferenceEntry {
        name: "fmwave6",
        synonyms: &[],
        summary: "operator 6's waveform",
        description: "The default is sine. Also accepted: triangle, square, sawtooth and the noise colours white, pink, brown and crackle - a noise operator is a looping buffer with no pitch, and nothing can modulate into one. Any other name refuses the event.",
        params: &[
            ReferenceParam {
                name: "value",
                r#type: "string | Pattern",
                description: "waveform name; default \"sine\"",
            },
        ],
        examples: &[
            "note(\"c2 e2\").s(\"sine\").fm6(3).fmwave6(\"square\")",
        ],
        tags: &["control", "fm"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
};

pub(super) const FMWAVE7: ControlRow = ControlRow {
    names: &["fmwave7"],
    aliases: &[],
    reference:         ReferenceEntry {
        name: "fmwave7",
        synonyms: &[],
        summary: "operator 7's waveform",
        description: "The default is sine. Also accepted: triangle, square, sawtooth and the noise colours white, pink, brown and crackle - a noise operator is a looping buffer with no pitch, and nothing can modulate into one. Any other name refuses the event.",
        params: &[
            ReferenceParam {
                name: "value",
                r#type: "string | Pattern",
                description: "waveform name; default \"sine\"",
            },
        ],
        examples: &[
            "note(\"c2 e2\").s(\"sine\").fm7(3).fmwave7(\"square\")",
        ],
        tags: &["control", "fm"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
};

pub(super) const FMWAVE8: ControlRow = ControlRow {
    names: &["fmwave8"],
    aliases: &[],
    reference:         ReferenceEntry {
        name: "fmwave8",
        synonyms: &[],
        summary: "operator 8's waveform",
        description: "The default is sine. Also accepted: triangle, square, sawtooth and the noise colours white, pink, brown and crackle - a noise operator is a looping buffer with no pitch, and nothing can modulate into one. Any other name refuses the event.",
        params: &[
            ReferenceParam {
                name: "value",
                r#type: "string | Pattern",
                description: "waveform name; default \"sine\"",
            },
        ],
        examples: &[
            "note(\"c2 e2\").s(\"sine\").fm8(3).fmwave8(\"square\")",
        ],
        tags: &["control", "fm"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
};

pub(super) const FMDECAY: ControlRow = ControlRow {
    names: &["fmdecay"],
    aliases: &["fmdec", "fmdec1", "fmdecay1"],
    reference: ReferenceEntry {
        name: "fmdecay",
        synonyms: &["fmdec", "fmdec1", "fmdecay1"],
        summary: "FM operator 1 depth decay, in seconds.",
        description: "Time for modulation depth to fall from its peak to sustain. Naming any fmattack, fmdecay, fmsustain, or fmrelease creates operator 1’s depth envelope; with none present the depth stays flat. Omitted attack/decay default to 0.001 s and release to 0.01 s. Omitted sustain is 0.001 when decay is set, otherwise 1. Attack/decay floor at 0.001 s, release at 0.01 s, and sustain caps at 1. Numbered forms select operators 2 through 8. Requires a nonzero FM route involving operator 1 and a connected path to the carrier; fmi (alias fm) supplies the direct route. The FM matrix affects basic oscillators, supersaw, pulse, and bytebeat. Recorded samples, `gm_*` soundfonts, `wt_*` wavetables, ZZFX, sbd, standalone noise, live input, and buses ignore it.",
        params: &[
            ReferenceParam {
                name: "time",
                r#type: "number | Pattern",
                description: "decay time",
            },
        ],
        examples: &[
            "note(\"c e g b g e\")\n.fm(4)\n.fmdecay(\"<.01 .05 .1 .2>\")\n.fmsustain(.4)\n._scope()",
        ],
        tags: &["fm", "envelope", "audio"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
};

pub(super) const FMDECAY2: ControlRow = ControlRow {
    names: &["fmdecay2"],
    aliases: &["fmdec2"],
    reference:         ReferenceEntry {
        name: "fmdecay2",
        synonyms: &["fmdec2"],
        summary: "operator 2's depth decay",
        description: "How fast operator 2's modulation depth falls to its sustain. The per-operator depth ADSR - fmattack2, fmdecay2, fmsustain2, fmrelease2 - is built the moment ANY of the four is named; left alone, the depth is flat. Base values are attack 0.001, decay 0.001, sustain 1, release 0.01, with the conditional rule: naming decay without sustain drops sustain to its 0.001 floor. Attack and decay floor at 0.001 s, release at 0.01 s, and sustain caps at 1.",
        params: &[
            ReferenceParam {
                name: "value",
                r#type: "number | Pattern",
                description: "decay time in seconds; floored at 0.001",
            },
        ],
        examples: &[
            "note(\"c2 e2\").s(\"sine\").fm2(3).fmdecay2(\"0.01 0.1 0.3\")",
        ],
        tags: &["control", "fm"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
};

pub(super) const FMDECAY3: ControlRow = ControlRow {
    names: &["fmdecay3"],
    aliases: &["fmdec3"],
    reference:         ReferenceEntry {
        name: "fmdecay3",
        synonyms: &["fmdec3"],
        summary: "operator 3's depth decay",
        description: "How fast operator 3's modulation depth falls to its sustain. The per-operator depth ADSR - fmattack3, fmdecay3, fmsustain3, fmrelease3 - is built the moment ANY of the four is named; left alone, the depth is flat. Base values are attack 0.001, decay 0.001, sustain 1, release 0.01, with the conditional rule: naming decay without sustain drops sustain to its 0.001 floor. Attack and decay floor at 0.001 s, release at 0.01 s, and sustain caps at 1.",
        params: &[
            ReferenceParam {
                name: "value",
                r#type: "number | Pattern",
                description: "decay time in seconds; floored at 0.001",
            },
        ],
        examples: &[
            "note(\"c2 e2\").s(\"sine\").fm3(3).fmdecay3(\"0.01 0.1 0.3\")",
        ],
        tags: &["control", "fm"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
};

pub(super) const FMDECAY4: ControlRow = ControlRow {
    names: &["fmdecay4"],
    aliases: &["fmdec4"],
    reference:         ReferenceEntry {
        name: "fmdecay4",
        synonyms: &["fmdec4"],
        summary: "operator 4's depth decay",
        description: "How fast operator 4's modulation depth falls to its sustain. The per-operator depth ADSR - fmattack4, fmdecay4, fmsustain4, fmrelease4 - is built the moment ANY of the four is named; left alone, the depth is flat. Base values are attack 0.001, decay 0.001, sustain 1, release 0.01, with the conditional rule: naming decay without sustain drops sustain to its 0.001 floor. Attack and decay floor at 0.001 s, release at 0.01 s, and sustain caps at 1.",
        params: &[
            ReferenceParam {
                name: "value",
                r#type: "number | Pattern",
                description: "decay time in seconds; floored at 0.001",
            },
        ],
        examples: &[
            "note(\"c2 e2\").s(\"sine\").fm4(3).fmdecay4(\"0.01 0.1 0.3\")",
        ],
        tags: &["control", "fm"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
};

pub(super) const FMDECAY5: ControlRow = ControlRow {
    names: &["fmdecay5"],
    aliases: &["fmdec5"],
    reference:         ReferenceEntry {
        name: "fmdecay5",
        synonyms: &["fmdec5"],
        summary: "operator 5's depth decay",
        description: "How fast operator 5's modulation depth falls to its sustain. The per-operator depth ADSR - fmattack5, fmdecay5, fmsustain5, fmrelease5 - is built the moment ANY of the four is named; left alone, the depth is flat. Base values are attack 0.001, decay 0.001, sustain 1, release 0.01, with the conditional rule: naming decay without sustain drops sustain to its 0.001 floor. Attack and decay floor at 0.001 s, release at 0.01 s, and sustain caps at 1.",
        params: &[
            ReferenceParam {
                name: "value",
                r#type: "number | Pattern",
                description: "decay time in seconds; floored at 0.001",
            },
        ],
        examples: &[
            "note(\"c2 e2\").s(\"sine\").fm5(3).fmdecay5(\"0.01 0.1 0.3\")",
        ],
        tags: &["control", "fm"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
};

pub(super) const FMDECAY6: ControlRow = ControlRow {
    names: &["fmdecay6"],
    aliases: &["fmdec6"],
    reference:         ReferenceEntry {
        name: "fmdecay6",
        synonyms: &["fmdec6"],
        summary: "operator 6's depth decay",
        description: "How fast operator 6's modulation depth falls to its sustain. The per-operator depth ADSR - fmattack6, fmdecay6, fmsustain6, fmrelease6 - is built the moment ANY of the four is named; left alone, the depth is flat. Base values are attack 0.001, decay 0.001, sustain 1, release 0.01, with the conditional rule: naming decay without sustain drops sustain to its 0.001 floor. Attack and decay floor at 0.001 s, release at 0.01 s, and sustain caps at 1.",
        params: &[
            ReferenceParam {
                name: "value",
                r#type: "number | Pattern",
                description: "decay time in seconds; floored at 0.001",
            },
        ],
        examples: &[
            "note(\"c2 e2\").s(\"sine\").fm6(3).fmdecay6(\"0.01 0.1 0.3\")",
        ],
        tags: &["control", "fm"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
};

pub(super) const FMDECAY7: ControlRow = ControlRow {
    names: &["fmdecay7"],
    aliases: &["fmdec7"],
    reference:         ReferenceEntry {
        name: "fmdecay7",
        synonyms: &["fmdec7"],
        summary: "operator 7's depth decay",
        description: "How fast operator 7's modulation depth falls to its sustain. The per-operator depth ADSR - fmattack7, fmdecay7, fmsustain7, fmrelease7 - is built the moment ANY of the four is named; left alone, the depth is flat. Base values are attack 0.001, decay 0.001, sustain 1, release 0.01, with the conditional rule: naming decay without sustain drops sustain to its 0.001 floor. Attack and decay floor at 0.001 s, release at 0.01 s, and sustain caps at 1.",
        params: &[
            ReferenceParam {
                name: "value",
                r#type: "number | Pattern",
                description: "decay time in seconds; floored at 0.001",
            },
        ],
        examples: &[
            "note(\"c2 e2\").s(\"sine\").fm7(3).fmdecay7(\"0.01 0.1 0.3\")",
        ],
        tags: &["control", "fm"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
};

pub(super) const FMDECAY8: ControlRow = ControlRow {
    names: &["fmdecay8"],
    aliases: &["fmdec8"],
    reference:         ReferenceEntry {
        name: "fmdecay8",
        synonyms: &["fmdec8"],
        summary: "operator 8's depth decay",
        description: "How fast operator 8's modulation depth falls to its sustain. The per-operator depth ADSR - fmattack8, fmdecay8, fmsustain8, fmrelease8 - is built the moment ANY of the four is named; left alone, the depth is flat. Base values are attack 0.001, decay 0.001, sustain 1, release 0.01, with the conditional rule: naming decay without sustain drops sustain to its 0.001 floor. Attack and decay floor at 0.001 s, release at 0.01 s, and sustain caps at 1.",
        params: &[
            ReferenceParam {
                name: "value",
                r#type: "number | Pattern",
                description: "decay time in seconds; floored at 0.001",
            },
        ],
        examples: &[
            "note(\"c2 e2\").s(\"sine\").fm8(3).fmdecay8(\"0.01 0.1 0.3\")",
        ],
        tags: &["control", "fm"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
};

pub(super) const FMSUSTAIN: ControlRow = ControlRow {
    names: &["fmsustain"],
    aliases: &["fmsus", "fmsus1", "fmsustain1"],
    reference: ReferenceEntry {
        name: "fmsustain",
        synonyms: &["fmsus", "fmsus1", "fmsustain1"],
        summary: "FM operator 1 sustain depth as a fraction of its peak.",
        description: "A unitless sustain multiplier, normally 0..1. Naming any fmattack, fmdecay, fmsustain, or fmrelease creates operator 1’s depth envelope; with none present the depth stays flat. Omitted attack/decay default to 0.001 s and release to 0.01 s. Omitted sustain is 0.001 when decay is set, otherwise 1. Attack/decay floor at 0.001 s, release at 0.01 s, and sustain caps at 1. Numbered forms select operators 2 through 8. Requires a nonzero FM route involving operator 1 and a connected path to the carrier; fmi (alias fm) supplies the direct route. The FM matrix affects basic oscillators, supersaw, pulse, and bytebeat. Recorded samples, `gm_*` soundfonts, `wt_*` wavetables, ZZFX, sbd, standalone noise, live input, and buses ignore it.",
        params: &[
            ReferenceParam {
                name: "level",
                r#type: "number | Pattern",
                description: "sustain level",
            },
        ],
        examples: &[
            "note(\"c e g b g e\")\n.fm(4)\n.fmdecay(.1)\n.fmsustain(\"<1 .75 .5 0>\")\n._scope()",
        ],
        tags: &["fm", "envelope", "audio"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
};

pub(super) const FMSUSTAIN2: ControlRow = ControlRow {
    names: &["fmsustain2"],
    aliases: &["fmsus2"],
    reference:         ReferenceEntry {
        name: "fmsustain2",
        synonyms: &["fmsus2"],
        summary: "operator 2's depth sustain",
        description: "The level operator 2's modulation depth holds at. The per-operator depth ADSR - fmattack2, fmdecay2, fmsustain2, fmrelease2 - is built the moment ANY of the four is named; left alone, the depth is flat. Base values are attack 0.001, decay 0.001, sustain 1, release 0.01, with the conditional rule: naming decay without sustain drops sustain to its 0.001 floor. Attack and decay floor at 0.001 s, release at 0.01 s, and sustain caps at 1.",
        params: &[
            ReferenceParam {
                name: "value",
                r#type: "number | Pattern",
                description: "sustain level 0..1; capped at 1",
            },
        ],
        examples: &[
            "note(\"c2 e2\").s(\"sine\").fm2(3).fmsustain2(\"0.1 0.5 1\")",
        ],
        tags: &["control", "fm"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
};

pub(super) const FMSUSTAIN3: ControlRow = ControlRow {
    names: &["fmsustain3"],
    aliases: &["fmsus3"],
    reference:         ReferenceEntry {
        name: "fmsustain3",
        synonyms: &["fmsus3"],
        summary: "operator 3's depth sustain",
        description: "The level operator 3's modulation depth holds at. The per-operator depth ADSR - fmattack3, fmdecay3, fmsustain3, fmrelease3 - is built the moment ANY of the four is named; left alone, the depth is flat. Base values are attack 0.001, decay 0.001, sustain 1, release 0.01, with the conditional rule: naming decay without sustain drops sustain to its 0.001 floor. Attack and decay floor at 0.001 s, release at 0.01 s, and sustain caps at 1.",
        params: &[
            ReferenceParam {
                name: "value",
                r#type: "number | Pattern",
                description: "sustain level 0..1; capped at 1",
            },
        ],
        examples: &[
            "note(\"c2 e2\").s(\"sine\").fm3(3).fmsustain3(\"0.1 0.5 1\")",
        ],
        tags: &["control", "fm"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
};

pub(super) const FMSUSTAIN4: ControlRow = ControlRow {
    names: &["fmsustain4"],
    aliases: &["fmsus4"],
    reference:         ReferenceEntry {
        name: "fmsustain4",
        synonyms: &["fmsus4"],
        summary: "operator 4's depth sustain",
        description: "The level operator 4's modulation depth holds at. The per-operator depth ADSR - fmattack4, fmdecay4, fmsustain4, fmrelease4 - is built the moment ANY of the four is named; left alone, the depth is flat. Base values are attack 0.001, decay 0.001, sustain 1, release 0.01, with the conditional rule: naming decay without sustain drops sustain to its 0.001 floor. Attack and decay floor at 0.001 s, release at 0.01 s, and sustain caps at 1.",
        params: &[
            ReferenceParam {
                name: "value",
                r#type: "number | Pattern",
                description: "sustain level 0..1; capped at 1",
            },
        ],
        examples: &[
            "note(\"c2 e2\").s(\"sine\").fm4(3).fmsustain4(\"0.1 0.5 1\")",
        ],
        tags: &["control", "fm"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
};

pub(super) const FMSUSTAIN5: ControlRow = ControlRow {
    names: &["fmsustain5"],
    aliases: &["fmsus5"],
    reference:         ReferenceEntry {
        name: "fmsustain5",
        synonyms: &["fmsus5"],
        summary: "operator 5's depth sustain",
        description: "The level operator 5's modulation depth holds at. The per-operator depth ADSR - fmattack5, fmdecay5, fmsustain5, fmrelease5 - is built the moment ANY of the four is named; left alone, the depth is flat. Base values are attack 0.001, decay 0.001, sustain 1, release 0.01, with the conditional rule: naming decay without sustain drops sustain to its 0.001 floor. Attack and decay floor at 0.001 s, release at 0.01 s, and sustain caps at 1.",
        params: &[
            ReferenceParam {
                name: "value",
                r#type: "number | Pattern",
                description: "sustain level 0..1; capped at 1",
            },
        ],
        examples: &[
            "note(\"c2 e2\").s(\"sine\").fm5(3).fmsustain5(\"0.1 0.5 1\")",
        ],
        tags: &["control", "fm"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
};

pub(super) const FMSUSTAIN6: ControlRow = ControlRow {
    names: &["fmsustain6"],
    aliases: &["fmsus6"],
    reference:         ReferenceEntry {
        name: "fmsustain6",
        synonyms: &["fmsus6"],
        summary: "operator 6's depth sustain",
        description: "The level operator 6's modulation depth holds at. The per-operator depth ADSR - fmattack6, fmdecay6, fmsustain6, fmrelease6 - is built the moment ANY of the four is named; left alone, the depth is flat. Base values are attack 0.001, decay 0.001, sustain 1, release 0.01, with the conditional rule: naming decay without sustain drops sustain to its 0.001 floor. Attack and decay floor at 0.001 s, release at 0.01 s, and sustain caps at 1.",
        params: &[
            ReferenceParam {
                name: "value",
                r#type: "number | Pattern",
                description: "sustain level 0..1; capped at 1",
            },
        ],
        examples: &[
            "note(\"c2 e2\").s(\"sine\").fm6(3).fmsustain6(\"0.1 0.5 1\")",
        ],
        tags: &["control", "fm"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
};

pub(super) const FMSUSTAIN7: ControlRow = ControlRow {
    names: &["fmsustain7"],
    aliases: &["fmsus7"],
    reference:         ReferenceEntry {
        name: "fmsustain7",
        synonyms: &["fmsus7"],
        summary: "operator 7's depth sustain",
        description: "The level operator 7's modulation depth holds at. The per-operator depth ADSR - fmattack7, fmdecay7, fmsustain7, fmrelease7 - is built the moment ANY of the four is named; left alone, the depth is flat. Base values are attack 0.001, decay 0.001, sustain 1, release 0.01, with the conditional rule: naming decay without sustain drops sustain to its 0.001 floor. Attack and decay floor at 0.001 s, release at 0.01 s, and sustain caps at 1.",
        params: &[
            ReferenceParam {
                name: "value",
                r#type: "number | Pattern",
                description: "sustain level 0..1; capped at 1",
            },
        ],
        examples: &[
            "note(\"c2 e2\").s(\"sine\").fm7(3).fmsustain7(\"0.1 0.5 1\")",
        ],
        tags: &["control", "fm"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
};

pub(super) const FMSUSTAIN8: ControlRow = ControlRow {
    names: &["fmsustain8"],
    aliases: &["fmsus8"],
    reference:         ReferenceEntry {
        name: "fmsustain8",
        synonyms: &["fmsus8"],
        summary: "operator 8's depth sustain",
        description: "The level operator 8's modulation depth holds at. The per-operator depth ADSR - fmattack8, fmdecay8, fmsustain8, fmrelease8 - is built the moment ANY of the four is named; left alone, the depth is flat. Base values are attack 0.001, decay 0.001, sustain 1, release 0.01, with the conditional rule: naming decay without sustain drops sustain to its 0.001 floor. Attack and decay floor at 0.001 s, release at 0.01 s, and sustain caps at 1.",
        params: &[
            ReferenceParam {
                name: "value",
                r#type: "number | Pattern",
                description: "sustain level 0..1; capped at 1",
            },
        ],
        examples: &[
            "note(\"c2 e2\").s(\"sine\").fm8(3).fmsustain8(\"0.1 0.5 1\")",
        ],
        tags: &["control", "fm"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
};

pub(super) const FMRELEASE: ControlRow = ControlRow {
    names: &["fmrelease"],
    aliases: &["fmrel", "fmrel1", "fmrelease1"],
    reference: ReferenceEntry {
        name: "fmrelease",
        synonyms: &["fmrel", "fmrel1", "fmrelease1"],
        summary: "FM operator 1 depth release, in seconds.",
        description: "Time for modulation depth to fall after the note ends. Naming any fmattack, fmdecay, fmsustain, or fmrelease creates operator 1’s depth envelope; with none present the depth stays flat. Omitted attack/decay default to 0.001 s and release to 0.01 s. Omitted sustain is 0.001 when decay is set, otherwise 1. Attack/decay floor at 0.001 s, release at 0.01 s, and sustain caps at 1. Numbered forms select operators 2 through 8. Requires a nonzero FM route involving operator 1 and a connected path to the carrier; fmi (alias fm) supplies the direct route. The FM matrix affects basic oscillators, supersaw, pulse, and bytebeat. Recorded samples, `gm_*` soundfonts, `wt_*` wavetables, ZZFX, sbd, standalone noise, live input, and buses ignore it.",
        params: &[
            ReferenceParam {
                name: "time",
                r#type: "number | Pattern",
                description: "release time",
            },
        ],
        examples: &[],
        tags: &["fm", "envelope", "audio"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
};

pub(super) const FMRELEASE2: ControlRow = ControlRow {
    names: &["fmrelease2"],
    aliases: &["fmrel2"],
    reference:         ReferenceEntry {
        name: "fmrelease2",
        synonyms: &["fmrel2"],
        summary: "operator 2's depth release",
        description: "How operator 2's modulation depth lets go at the note's end. The per-operator depth ADSR - fmattack2, fmdecay2, fmsustain2, fmrelease2 - is built the moment ANY of the four is named; left alone, the depth is flat. Base values are attack 0.001, decay 0.001, sustain 1, release 0.01, with the conditional rule: naming decay without sustain drops sustain to its 0.001 floor. Attack and decay floor at 0.001 s, release at 0.01 s, and sustain caps at 1.",
        params: &[
            ReferenceParam {
                name: "value",
                r#type: "number | Pattern",
                description: "release time in seconds; floored at 0.01",
            },
        ],
        examples: &[
            "note(\"c2 e2\").s(\"sine\").fm2(3).fmrelease2(\"0.05 0.2 0.5\")",
        ],
        tags: &["control", "fm"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
};

pub(super) const FMRELEASE3: ControlRow = ControlRow {
    names: &["fmrelease3"],
    aliases: &["fmrel3"],
    reference:         ReferenceEntry {
        name: "fmrelease3",
        synonyms: &["fmrel3"],
        summary: "operator 3's depth release",
        description: "How operator 3's modulation depth lets go at the note's end. The per-operator depth ADSR - fmattack3, fmdecay3, fmsustain3, fmrelease3 - is built the moment ANY of the four is named; left alone, the depth is flat. Base values are attack 0.001, decay 0.001, sustain 1, release 0.01, with the conditional rule: naming decay without sustain drops sustain to its 0.001 floor. Attack and decay floor at 0.001 s, release at 0.01 s, and sustain caps at 1.",
        params: &[
            ReferenceParam {
                name: "value",
                r#type: "number | Pattern",
                description: "release time in seconds; floored at 0.01",
            },
        ],
        examples: &[
            "note(\"c2 e2\").s(\"sine\").fm3(3).fmrelease3(\"0.05 0.2 0.5\")",
        ],
        tags: &["control", "fm"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
};

pub(super) const FMRELEASE4: ControlRow = ControlRow {
    names: &["fmrelease4"],
    aliases: &["fmrel4"],
    reference:         ReferenceEntry {
        name: "fmrelease4",
        synonyms: &["fmrel4"],
        summary: "operator 4's depth release",
        description: "How operator 4's modulation depth lets go at the note's end. The per-operator depth ADSR - fmattack4, fmdecay4, fmsustain4, fmrelease4 - is built the moment ANY of the four is named; left alone, the depth is flat. Base values are attack 0.001, decay 0.001, sustain 1, release 0.01, with the conditional rule: naming decay without sustain drops sustain to its 0.001 floor. Attack and decay floor at 0.001 s, release at 0.01 s, and sustain caps at 1.",
        params: &[
            ReferenceParam {
                name: "value",
                r#type: "number | Pattern",
                description: "release time in seconds; floored at 0.01",
            },
        ],
        examples: &[
            "note(\"c2 e2\").s(\"sine\").fm4(3).fmrelease4(\"0.05 0.2 0.5\")",
        ],
        tags: &["control", "fm"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
};

pub(super) const FMRELEASE5: ControlRow = ControlRow {
    names: &["fmrelease5"],
    aliases: &["fmrel5"],
    reference:         ReferenceEntry {
        name: "fmrelease5",
        synonyms: &["fmrel5"],
        summary: "operator 5's depth release",
        description: "How operator 5's modulation depth lets go at the note's end. The per-operator depth ADSR - fmattack5, fmdecay5, fmsustain5, fmrelease5 - is built the moment ANY of the four is named; left alone, the depth is flat. Base values are attack 0.001, decay 0.001, sustain 1, release 0.01, with the conditional rule: naming decay without sustain drops sustain to its 0.001 floor. Attack and decay floor at 0.001 s, release at 0.01 s, and sustain caps at 1.",
        params: &[
            ReferenceParam {
                name: "value",
                r#type: "number | Pattern",
                description: "release time in seconds; floored at 0.01",
            },
        ],
        examples: &[
            "note(\"c2 e2\").s(\"sine\").fm5(3).fmrelease5(\"0.05 0.2 0.5\")",
        ],
        tags: &["control", "fm"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
};

pub(super) const FMRELEASE6: ControlRow = ControlRow {
    names: &["fmrelease6"],
    aliases: &["fmrel6"],
    reference:         ReferenceEntry {
        name: "fmrelease6",
        synonyms: &["fmrel6"],
        summary: "operator 6's depth release",
        description: "How operator 6's modulation depth lets go at the note's end. The per-operator depth ADSR - fmattack6, fmdecay6, fmsustain6, fmrelease6 - is built the moment ANY of the four is named; left alone, the depth is flat. Base values are attack 0.001, decay 0.001, sustain 1, release 0.01, with the conditional rule: naming decay without sustain drops sustain to its 0.001 floor. Attack and decay floor at 0.001 s, release at 0.01 s, and sustain caps at 1.",
        params: &[
            ReferenceParam {
                name: "value",
                r#type: "number | Pattern",
                description: "release time in seconds; floored at 0.01",
            },
        ],
        examples: &[
            "note(\"c2 e2\").s(\"sine\").fm6(3).fmrelease6(\"0.05 0.2 0.5\")",
        ],
        tags: &["control", "fm"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
};

pub(super) const FMRELEASE7: ControlRow = ControlRow {
    names: &["fmrelease7"],
    aliases: &["fmrel7"],
    reference:         ReferenceEntry {
        name: "fmrelease7",
        synonyms: &["fmrel7"],
        summary: "operator 7's depth release",
        description: "How operator 7's modulation depth lets go at the note's end. The per-operator depth ADSR - fmattack7, fmdecay7, fmsustain7, fmrelease7 - is built the moment ANY of the four is named; left alone, the depth is flat. Base values are attack 0.001, decay 0.001, sustain 1, release 0.01, with the conditional rule: naming decay without sustain drops sustain to its 0.001 floor. Attack and decay floor at 0.001 s, release at 0.01 s, and sustain caps at 1.",
        params: &[
            ReferenceParam {
                name: "value",
                r#type: "number | Pattern",
                description: "release time in seconds; floored at 0.01",
            },
        ],
        examples: &[
            "note(\"c2 e2\").s(\"sine\").fm7(3).fmrelease7(\"0.05 0.2 0.5\")",
        ],
        tags: &["control", "fm"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
};

pub(super) const FMRELEASE8: ControlRow = ControlRow {
    names: &["fmrelease8"],
    aliases: &["fmrel8"],
    reference:         ReferenceEntry {
        name: "fmrelease8",
        synonyms: &["fmrel8"],
        summary: "operator 8's depth release",
        description: "How operator 8's modulation depth lets go at the note's end. The per-operator depth ADSR - fmattack8, fmdecay8, fmsustain8, fmrelease8 - is built the moment ANY of the four is named; left alone, the depth is flat. Base values are attack 0.001, decay 0.001, sustain 1, release 0.01, with the conditional rule: naming decay without sustain drops sustain to its 0.001 floor. Attack and decay floor at 0.001 s, release at 0.01 s, and sustain caps at 1.",
        params: &[
            ReferenceParam {
                name: "value",
                r#type: "number | Pattern",
                description: "release time in seconds; floored at 0.01",
            },
        ],
        examples: &[
            "note(\"c2 e2\").s(\"sine\").fm8(3).fmrelease8(\"0.05 0.2 0.5\")",
        ],
        tags: &["control", "fm"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
};
