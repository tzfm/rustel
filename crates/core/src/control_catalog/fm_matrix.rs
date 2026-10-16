use super::{ControlRow, ReferenceEntry, ReferenceParam};

pub(super) const FMI00: ControlRow = ControlRow {
    names: &["fmi00"],
    aliases: &["fm00"],
    reference:         ReferenceEntry {
        name: "fmi00",
        synonyms: &["fm00"],
        summary: "a matrix slot with source 0 - pinned surface, read nowhere",
        description: "The FM resolver walks sources 1..8: there is no operator 0 to send from, so the fmi0X spellings are accepted but never read. The value rides the event and leaves the engine through .osc(), and a midimaps entry can turn it into a CC; natively it is silent.",
        params: &[
            ReferenceParam {
                name: "value",
                r#type: "number | Pattern",
                description: "modulation depth; never applied",
            },
        ],
        examples: &[
            "s(\"sine\").fmi00(1)",
        ],
        tags: &["control", "fm", "fm_matrix"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
};

pub(super) const FMI01: ControlRow = ControlRow {
    names: &["fmi01"],
    aliases: &["fm01"],
    reference:         ReferenceEntry {
        name: "fmi01",
        synonyms: &["fm01"],
        summary: "a matrix slot with source 0 - pinned surface, read nowhere",
        description: "The FM resolver walks sources 1..8: there is no operator 0 to send from, so the fmi0X spellings are accepted but never read. The value rides the event and leaves the engine through .osc(), and a midimaps entry can turn it into a CC; natively it is silent.",
        params: &[
            ReferenceParam {
                name: "value",
                r#type: "number | Pattern",
                description: "modulation depth; never applied",
            },
        ],
        examples: &[
            "s(\"sine\").fmi01(1)",
        ],
        tags: &["control", "fm", "fm_matrix"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
};

pub(super) const FMI02: ControlRow = ControlRow {
    names: &["fmi02"],
    aliases: &["fm02"],
    reference:         ReferenceEntry {
        name: "fmi02",
        synonyms: &["fm02"],
        summary: "a matrix slot with source 0 - pinned surface, read nowhere",
        description: "The FM resolver walks sources 1..8: there is no operator 0 to send from, so the fmi0X spellings are accepted but never read. The value rides the event and leaves the engine through .osc(), and a midimaps entry can turn it into a CC; natively it is silent.",
        params: &[
            ReferenceParam {
                name: "value",
                r#type: "number | Pattern",
                description: "modulation depth; never applied",
            },
        ],
        examples: &[
            "s(\"sine\").fmi02(1)",
        ],
        tags: &["control", "fm", "fm_matrix"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
};

pub(super) const FMI03: ControlRow = ControlRow {
    names: &["fmi03"],
    aliases: &["fm03"],
    reference:         ReferenceEntry {
        name: "fmi03",
        synonyms: &["fm03"],
        summary: "a matrix slot with source 0 - pinned surface, read nowhere",
        description: "The FM resolver walks sources 1..8: there is no operator 0 to send from, so the fmi0X spellings are accepted but never read. The value rides the event and leaves the engine through .osc(), and a midimaps entry can turn it into a CC; natively it is silent.",
        params: &[
            ReferenceParam {
                name: "value",
                r#type: "number | Pattern",
                description: "modulation depth; never applied",
            },
        ],
        examples: &[
            "s(\"sine\").fmi03(1)",
        ],
        tags: &["control", "fm", "fm_matrix"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
};

pub(super) const FMI04: ControlRow = ControlRow {
    names: &["fmi04"],
    aliases: &["fm04"],
    reference:         ReferenceEntry {
        name: "fmi04",
        synonyms: &["fm04"],
        summary: "a matrix slot with source 0 - pinned surface, read nowhere",
        description: "The FM resolver walks sources 1..8: there is no operator 0 to send from, so the fmi0X spellings are accepted but never read. The value rides the event and leaves the engine through .osc(), and a midimaps entry can turn it into a CC; natively it is silent.",
        params: &[
            ReferenceParam {
                name: "value",
                r#type: "number | Pattern",
                description: "modulation depth; never applied",
            },
        ],
        examples: &[
            "s(\"sine\").fmi04(1)",
        ],
        tags: &["control", "fm", "fm_matrix"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
};

pub(super) const FMI05: ControlRow = ControlRow {
    names: &["fmi05"],
    aliases: &["fm05"],
    reference:         ReferenceEntry {
        name: "fmi05",
        synonyms: &["fm05"],
        summary: "a matrix slot with source 0 - pinned surface, read nowhere",
        description: "The FM resolver walks sources 1..8: there is no operator 0 to send from, so the fmi0X spellings are accepted but never read. The value rides the event and leaves the engine through .osc(), and a midimaps entry can turn it into a CC; natively it is silent.",
        params: &[
            ReferenceParam {
                name: "value",
                r#type: "number | Pattern",
                description: "modulation depth; never applied",
            },
        ],
        examples: &[
            "s(\"sine\").fmi05(1)",
        ],
        tags: &["control", "fm", "fm_matrix"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
};

pub(super) const FMI06: ControlRow = ControlRow {
    names: &["fmi06"],
    aliases: &["fm06"],
    reference:         ReferenceEntry {
        name: "fmi06",
        synonyms: &["fm06"],
        summary: "a matrix slot with source 0 - pinned surface, read nowhere",
        description: "The FM resolver walks sources 1..8: there is no operator 0 to send from, so the fmi0X spellings are accepted but never read. The value rides the event and leaves the engine through .osc(), and a midimaps entry can turn it into a CC; natively it is silent.",
        params: &[
            ReferenceParam {
                name: "value",
                r#type: "number | Pattern",
                description: "modulation depth; never applied",
            },
        ],
        examples: &[
            "s(\"sine\").fmi06(1)",
        ],
        tags: &["control", "fm", "fm_matrix"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
};

pub(super) const FMI07: ControlRow = ControlRow {
    names: &["fmi07"],
    aliases: &["fm07"],
    reference:         ReferenceEntry {
        name: "fmi07",
        synonyms: &["fm07"],
        summary: "a matrix slot with source 0 - pinned surface, read nowhere",
        description: "The FM resolver walks sources 1..8: there is no operator 0 to send from, so the fmi0X spellings are accepted but never read. The value rides the event and leaves the engine through .osc(), and a midimaps entry can turn it into a CC; natively it is silent.",
        params: &[
            ReferenceParam {
                name: "value",
                r#type: "number | Pattern",
                description: "modulation depth; never applied",
            },
        ],
        examples: &[
            "s(\"sine\").fmi07(1)",
        ],
        tags: &["control", "fm", "fm_matrix"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
};

pub(super) const FMI08: ControlRow = ControlRow {
    names: &["fmi08"],
    aliases: &["fm08"],
    reference:         ReferenceEntry {
        name: "fmi08",
        synonyms: &["fm08"],
        summary: "a matrix slot with source 0 - pinned surface, read nowhere",
        description: "The FM resolver walks sources 1..8: there is no operator 0 to send from, so the fmi0X spellings are accepted but never read. The value rides the event and leaves the engine through .osc(), and a midimaps entry can turn it into a CC; natively it is silent.",
        params: &[
            ReferenceParam {
                name: "value",
                r#type: "number | Pattern",
                description: "modulation depth; never applied",
            },
        ],
        examples: &[
            "s(\"sine\").fmi08(1)",
        ],
        tags: &["control", "fm", "fm_matrix"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
};

pub(super) const FMI10: ControlRow = ControlRow {
    names: &["fmi10"],
    aliases: &["fm10"],
    reference:         ReferenceEntry {
        name: "fmi10",
        synonyms: &["fm10"],
        summary: "operator 1 into the carrier - spelled fmi here",
        description: "This cell is the diagonal the resolver reads under the short spelling fmi: operator 1 is unsuffixed throughout, so the fmi10 form is pinned for surface compatibility but never read. The behaviour is the fmi entry's. The value is a modulation depth - an index, not hertz: the deviation it produces scales with the carrier and the source's harmonicity. A zero or absent amount makes no connection at all, and a gap severs the chain rather than being stepped over. Sixteen routes per event is the ceiling; beyond it the event is refused. The matrix bends the oscillator voices - sine, triangle, square, sawtooth, supersaw, pulse, bytebeat; samples, wavetables and the zzfx family ignore it.",
        params: &[
            ReferenceParam {
                name: "value",
                r#type: "number | Pattern",
                description: "modulation index; 0 or absent = no route",
            },
        ],
        examples: &[
            "note(\"c2 e2\").s(\"sine\").fmi(\"<0 2 4 8>\")",
        ],
        tags: &["control", "fm", "fm_matrix"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
};

pub(super) const FMI11: ControlRow = ControlRow {
    names: &["fmi11"],
    aliases: &["fm11"],
    reference:         ReferenceEntry {
        name: "fmi11",
        synonyms: &["fm11"],
        summary: "sends operator 1 into operator 1 - itself, as feedback",
        description: "One cell of the FM matrix: the modulation operator 1 bends operator 1 - itself, as feedback. An operator is built the first time any route names it at either end. The value is a modulation depth - an index, not hertz: the deviation it produces scales with the carrier and the source's harmonicity. A zero or absent amount makes no connection at all, and a gap severs the chain rather than being stepped over. Sixteen routes per event is the ceiling; beyond it the event is refused. The matrix bends the oscillator voices - sine, triangle, square, sawtooth, supersaw, pulse, bytebeat; samples, wavetables and the zzfx family ignore it.",
        params: &[
            ReferenceParam {
                name: "value",
                r#type: "number | Pattern",
                description: "modulation index; 0 or absent = no route",
            },
        ],
        examples: &[
            "note(\"c2 e2\").s(\"sine\").fmi11(\"<0 1 4>\")",
        ],
        tags: &["control", "fm", "fm_matrix"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
};

pub(super) const FMI12: ControlRow = ControlRow {
    names: &["fmi12"],
    aliases: &["fm12"],
    reference:         ReferenceEntry {
        name: "fmi12",
        synonyms: &["fm12"],
        summary: "sends operator 1 into operator 2",
        description: "One cell of the FM matrix: the modulation operator 1 bends operator 2. An operator is built the first time any route names it at either end. The value is a modulation depth - an index, not hertz: the deviation it produces scales with the carrier and the source's harmonicity. A zero or absent amount makes no connection at all, and a gap severs the chain rather than being stepped over. Sixteen routes per event is the ceiling; beyond it the event is refused. The matrix bends the oscillator voices - sine, triangle, square, sawtooth, supersaw, pulse, bytebeat; samples, wavetables and the zzfx family ignore it.",
        params: &[
            ReferenceParam {
                name: "value",
                r#type: "number | Pattern",
                description: "modulation index; 0 or absent = no route",
            },
        ],
        examples: &[
            "note(\"c2 e2\").s(\"sine\").fmi12(\"<0 1 4>\")",
        ],
        tags: &["control", "fm", "fm_matrix"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
};

pub(super) const FMI13: ControlRow = ControlRow {
    names: &["fmi13"],
    aliases: &["fm13"],
    reference:         ReferenceEntry {
        name: "fmi13",
        synonyms: &["fm13"],
        summary: "sends operator 1 into operator 3",
        description: "One cell of the FM matrix: the modulation operator 1 bends operator 3. An operator is built the first time any route names it at either end. The value is a modulation depth - an index, not hertz: the deviation it produces scales with the carrier and the source's harmonicity. A zero or absent amount makes no connection at all, and a gap severs the chain rather than being stepped over. Sixteen routes per event is the ceiling; beyond it the event is refused. The matrix bends the oscillator voices - sine, triangle, square, sawtooth, supersaw, pulse, bytebeat; samples, wavetables and the zzfx family ignore it.",
        params: &[
            ReferenceParam {
                name: "value",
                r#type: "number | Pattern",
                description: "modulation index; 0 or absent = no route",
            },
        ],
        examples: &[
            "note(\"c2 e2\").s(\"sine\").fmi13(\"<0 1 4>\")",
        ],
        tags: &["control", "fm", "fm_matrix"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
};

pub(super) const FMI14: ControlRow = ControlRow {
    names: &["fmi14"],
    aliases: &["fm14"],
    reference:         ReferenceEntry {
        name: "fmi14",
        synonyms: &["fm14"],
        summary: "sends operator 1 into operator 4",
        description: "One cell of the FM matrix: the modulation operator 1 bends operator 4. An operator is built the first time any route names it at either end. The value is a modulation depth - an index, not hertz: the deviation it produces scales with the carrier and the source's harmonicity. A zero or absent amount makes no connection at all, and a gap severs the chain rather than being stepped over. Sixteen routes per event is the ceiling; beyond it the event is refused. The matrix bends the oscillator voices - sine, triangle, square, sawtooth, supersaw, pulse, bytebeat; samples, wavetables and the zzfx family ignore it.",
        params: &[
            ReferenceParam {
                name: "value",
                r#type: "number | Pattern",
                description: "modulation index; 0 or absent = no route",
            },
        ],
        examples: &[
            "note(\"c2 e2\").s(\"sine\").fmi14(\"<0 1 4>\")",
        ],
        tags: &["control", "fm", "fm_matrix"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
};

pub(super) const FMI15: ControlRow = ControlRow {
    names: &["fmi15"],
    aliases: &["fm15"],
    reference:         ReferenceEntry {
        name: "fmi15",
        synonyms: &["fm15"],
        summary: "sends operator 1 into operator 5",
        description: "One cell of the FM matrix: the modulation operator 1 bends operator 5. An operator is built the first time any route names it at either end. The value is a modulation depth - an index, not hertz: the deviation it produces scales with the carrier and the source's harmonicity. A zero or absent amount makes no connection at all, and a gap severs the chain rather than being stepped over. Sixteen routes per event is the ceiling; beyond it the event is refused. The matrix bends the oscillator voices - sine, triangle, square, sawtooth, supersaw, pulse, bytebeat; samples, wavetables and the zzfx family ignore it.",
        params: &[
            ReferenceParam {
                name: "value",
                r#type: "number | Pattern",
                description: "modulation index; 0 or absent = no route",
            },
        ],
        examples: &[
            "note(\"c2 e2\").s(\"sine\").fmi15(\"<0 1 4>\")",
        ],
        tags: &["control", "fm", "fm_matrix"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
};

pub(super) const FMI16: ControlRow = ControlRow {
    names: &["fmi16"],
    aliases: &["fm16"],
    reference:         ReferenceEntry {
        name: "fmi16",
        synonyms: &["fm16"],
        summary: "sends operator 1 into operator 6",
        description: "One cell of the FM matrix: the modulation operator 1 bends operator 6. An operator is built the first time any route names it at either end. The value is a modulation depth - an index, not hertz: the deviation it produces scales with the carrier and the source's harmonicity. A zero or absent amount makes no connection at all, and a gap severs the chain rather than being stepped over. Sixteen routes per event is the ceiling; beyond it the event is refused. The matrix bends the oscillator voices - sine, triangle, square, sawtooth, supersaw, pulse, bytebeat; samples, wavetables and the zzfx family ignore it.",
        params: &[
            ReferenceParam {
                name: "value",
                r#type: "number | Pattern",
                description: "modulation index; 0 or absent = no route",
            },
        ],
        examples: &[
            "note(\"c2 e2\").s(\"sine\").fmi16(\"<0 1 4>\")",
        ],
        tags: &["control", "fm", "fm_matrix"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
};

pub(super) const FMI17: ControlRow = ControlRow {
    names: &["fmi17"],
    aliases: &["fm17"],
    reference:         ReferenceEntry {
        name: "fmi17",
        synonyms: &["fm17"],
        summary: "sends operator 1 into operator 7",
        description: "One cell of the FM matrix: the modulation operator 1 bends operator 7. An operator is built the first time any route names it at either end. The value is a modulation depth - an index, not hertz: the deviation it produces scales with the carrier and the source's harmonicity. A zero or absent amount makes no connection at all, and a gap severs the chain rather than being stepped over. Sixteen routes per event is the ceiling; beyond it the event is refused. The matrix bends the oscillator voices - sine, triangle, square, sawtooth, supersaw, pulse, bytebeat; samples, wavetables and the zzfx family ignore it.",
        params: &[
            ReferenceParam {
                name: "value",
                r#type: "number | Pattern",
                description: "modulation index; 0 or absent = no route",
            },
        ],
        examples: &[
            "note(\"c2 e2\").s(\"sine\").fmi17(\"<0 1 4>\")",
        ],
        tags: &["control", "fm", "fm_matrix"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
};

pub(super) const FMI18: ControlRow = ControlRow {
    names: &["fmi18"],
    aliases: &["fm18"],
    reference:         ReferenceEntry {
        name: "fmi18",
        synonyms: &["fm18"],
        summary: "sends operator 1 into operator 8",
        description: "One cell of the FM matrix: the modulation operator 1 bends operator 8. An operator is built the first time any route names it at either end. The value is a modulation depth - an index, not hertz: the deviation it produces scales with the carrier and the source's harmonicity. A zero or absent amount makes no connection at all, and a gap severs the chain rather than being stepped over. Sixteen routes per event is the ceiling; beyond it the event is refused. The matrix bends the oscillator voices - sine, triangle, square, sawtooth, supersaw, pulse, bytebeat; samples, wavetables and the zzfx family ignore it.",
        params: &[
            ReferenceParam {
                name: "value",
                r#type: "number | Pattern",
                description: "modulation index; 0 or absent = no route",
            },
        ],
        examples: &[
            "note(\"c2 e2\").s(\"sine\").fmi18(\"<0 1 4>\")",
        ],
        tags: &["control", "fm", "fm_matrix"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
};

pub(super) const FMI20: ControlRow = ControlRow {
    names: &["fmi20"],
    aliases: &["fm20"],
    reference:         ReferenceEntry {
        name: "fmi20",
        synonyms: &["fm20"],
        summary: "sends operator 2 into the carrier's frequency",
        description: "One cell of the FM matrix: the modulation operator 2 bends the carrier's frequency. An operator is built the first time any route names it at either end. The value is a modulation depth - an index, not hertz: the deviation it produces scales with the carrier and the source's harmonicity. A zero or absent amount makes no connection at all, and a gap severs the chain rather than being stepped over. Sixteen routes per event is the ceiling; beyond it the event is refused. The matrix bends the oscillator voices - sine, triangle, square, sawtooth, supersaw, pulse, bytebeat; samples, wavetables and the zzfx family ignore it.",
        params: &[
            ReferenceParam {
                name: "value",
                r#type: "number | Pattern",
                description: "modulation index; 0 or absent = no route",
            },
        ],
        examples: &[
            "note(\"c2 e2\").s(\"sine\").fmi20(\"<0 1 4>\")",
        ],
        tags: &["control", "fm", "fm_matrix"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
};

pub(super) const FMI21: ControlRow = ControlRow {
    names: &["fmi21"],
    aliases: &["fm21"],
    reference:         ReferenceEntry {
        name: "fmi21",
        synonyms: &["fm21"],
        summary: "operator 2 into operator 1 - spelled fmi2 here",
        description: "This cell is the diagonal the resolver reads under the short spelling fmi2: operator 1 is unsuffixed throughout, so the fmi21 form is pinned for surface compatibility but never read. The behaviour is the fmi2 entry's. The value is a modulation depth - an index, not hertz: the deviation it produces scales with the carrier and the source's harmonicity. A zero or absent amount makes no connection at all, and a gap severs the chain rather than being stepped over. Sixteen routes per event is the ceiling; beyond it the event is refused. The matrix bends the oscillator voices - sine, triangle, square, sawtooth, supersaw, pulse, bytebeat; samples, wavetables and the zzfx family ignore it.",
        params: &[
            ReferenceParam {
                name: "value",
                r#type: "number | Pattern",
                description: "modulation index; 0 or absent = no route",
            },
        ],
        examples: &[
            "note(\"c2 e2\").s(\"sine\").fmi2(\"<0 2 4 8>\")",
        ],
        tags: &["control", "fm", "fm_matrix"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
};

pub(super) const FMI22: ControlRow = ControlRow {
    names: &["fmi22"],
    aliases: &["fm22"],
    reference:         ReferenceEntry {
        name: "fmi22",
        synonyms: &["fm22"],
        summary: "sends operator 2 into operator 2 - itself, as feedback",
        description: "One cell of the FM matrix: the modulation operator 2 bends operator 2 - itself, as feedback. An operator is built the first time any route names it at either end. The value is a modulation depth - an index, not hertz: the deviation it produces scales with the carrier and the source's harmonicity. A zero or absent amount makes no connection at all, and a gap severs the chain rather than being stepped over. Sixteen routes per event is the ceiling; beyond it the event is refused. The matrix bends the oscillator voices - sine, triangle, square, sawtooth, supersaw, pulse, bytebeat; samples, wavetables and the zzfx family ignore it.",
        params: &[
            ReferenceParam {
                name: "value",
                r#type: "number | Pattern",
                description: "modulation index; 0 or absent = no route",
            },
        ],
        examples: &[
            "note(\"c2 e2\").s(\"sine\").fmi22(\"<0 1 4>\")",
        ],
        tags: &["control", "fm", "fm_matrix"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
};

pub(super) const FMI23: ControlRow = ControlRow {
    names: &["fmi23"],
    aliases: &["fm23"],
    reference:         ReferenceEntry {
        name: "fmi23",
        synonyms: &["fm23"],
        summary: "sends operator 2 into operator 3",
        description: "One cell of the FM matrix: the modulation operator 2 bends operator 3. An operator is built the first time any route names it at either end. The value is a modulation depth - an index, not hertz: the deviation it produces scales with the carrier and the source's harmonicity. A zero or absent amount makes no connection at all, and a gap severs the chain rather than being stepped over. Sixteen routes per event is the ceiling; beyond it the event is refused. The matrix bends the oscillator voices - sine, triangle, square, sawtooth, supersaw, pulse, bytebeat; samples, wavetables and the zzfx family ignore it.",
        params: &[
            ReferenceParam {
                name: "value",
                r#type: "number | Pattern",
                description: "modulation index; 0 or absent = no route",
            },
        ],
        examples: &[
            "note(\"c2 e2\").s(\"sine\").fmi23(\"<0 1 4>\")",
        ],
        tags: &["control", "fm", "fm_matrix"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
};

pub(super) const FMI24: ControlRow = ControlRow {
    names: &["fmi24"],
    aliases: &["fm24"],
    reference:         ReferenceEntry {
        name: "fmi24",
        synonyms: &["fm24"],
        summary: "sends operator 2 into operator 4",
        description: "One cell of the FM matrix: the modulation operator 2 bends operator 4. An operator is built the first time any route names it at either end. The value is a modulation depth - an index, not hertz: the deviation it produces scales with the carrier and the source's harmonicity. A zero or absent amount makes no connection at all, and a gap severs the chain rather than being stepped over. Sixteen routes per event is the ceiling; beyond it the event is refused. The matrix bends the oscillator voices - sine, triangle, square, sawtooth, supersaw, pulse, bytebeat; samples, wavetables and the zzfx family ignore it.",
        params: &[
            ReferenceParam {
                name: "value",
                r#type: "number | Pattern",
                description: "modulation index; 0 or absent = no route",
            },
        ],
        examples: &[
            "note(\"c2 e2\").s(\"sine\").fmi24(\"<0 1 4>\")",
        ],
        tags: &["control", "fm", "fm_matrix"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
};

pub(super) const FMI25: ControlRow = ControlRow {
    names: &["fmi25"],
    aliases: &["fm25"],
    reference:         ReferenceEntry {
        name: "fmi25",
        synonyms: &["fm25"],
        summary: "sends operator 2 into operator 5",
        description: "One cell of the FM matrix: the modulation operator 2 bends operator 5. An operator is built the first time any route names it at either end. The value is a modulation depth - an index, not hertz: the deviation it produces scales with the carrier and the source's harmonicity. A zero or absent amount makes no connection at all, and a gap severs the chain rather than being stepped over. Sixteen routes per event is the ceiling; beyond it the event is refused. The matrix bends the oscillator voices - sine, triangle, square, sawtooth, supersaw, pulse, bytebeat; samples, wavetables and the zzfx family ignore it.",
        params: &[
            ReferenceParam {
                name: "value",
                r#type: "number | Pattern",
                description: "modulation index; 0 or absent = no route",
            },
        ],
        examples: &[
            "note(\"c2 e2\").s(\"sine\").fmi25(\"<0 1 4>\")",
        ],
        tags: &["control", "fm", "fm_matrix"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
};

pub(super) const FMI26: ControlRow = ControlRow {
    names: &["fmi26"],
    aliases: &["fm26"],
    reference:         ReferenceEntry {
        name: "fmi26",
        synonyms: &["fm26"],
        summary: "sends operator 2 into operator 6",
        description: "One cell of the FM matrix: the modulation operator 2 bends operator 6. An operator is built the first time any route names it at either end. The value is a modulation depth - an index, not hertz: the deviation it produces scales with the carrier and the source's harmonicity. A zero or absent amount makes no connection at all, and a gap severs the chain rather than being stepped over. Sixteen routes per event is the ceiling; beyond it the event is refused. The matrix bends the oscillator voices - sine, triangle, square, sawtooth, supersaw, pulse, bytebeat; samples, wavetables and the zzfx family ignore it.",
        params: &[
            ReferenceParam {
                name: "value",
                r#type: "number | Pattern",
                description: "modulation index; 0 or absent = no route",
            },
        ],
        examples: &[
            "note(\"c2 e2\").s(\"sine\").fmi26(\"<0 1 4>\")",
        ],
        tags: &["control", "fm", "fm_matrix"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
};

pub(super) const FMI27: ControlRow = ControlRow {
    names: &["fmi27"],
    aliases: &["fm27"],
    reference:         ReferenceEntry {
        name: "fmi27",
        synonyms: &["fm27"],
        summary: "sends operator 2 into operator 7",
        description: "One cell of the FM matrix: the modulation operator 2 bends operator 7. An operator is built the first time any route names it at either end. The value is a modulation depth - an index, not hertz: the deviation it produces scales with the carrier and the source's harmonicity. A zero or absent amount makes no connection at all, and a gap severs the chain rather than being stepped over. Sixteen routes per event is the ceiling; beyond it the event is refused. The matrix bends the oscillator voices - sine, triangle, square, sawtooth, supersaw, pulse, bytebeat; samples, wavetables and the zzfx family ignore it.",
        params: &[
            ReferenceParam {
                name: "value",
                r#type: "number | Pattern",
                description: "modulation index; 0 or absent = no route",
            },
        ],
        examples: &[
            "note(\"c2 e2\").s(\"sine\").fmi27(\"<0 1 4>\")",
        ],
        tags: &["control", "fm", "fm_matrix"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
};

pub(super) const FMI28: ControlRow = ControlRow {
    names: &["fmi28"],
    aliases: &["fm28"],
    reference:         ReferenceEntry {
        name: "fmi28",
        synonyms: &["fm28"],
        summary: "sends operator 2 into operator 8",
        description: "One cell of the FM matrix: the modulation operator 2 bends operator 8. An operator is built the first time any route names it at either end. The value is a modulation depth - an index, not hertz: the deviation it produces scales with the carrier and the source's harmonicity. A zero or absent amount makes no connection at all, and a gap severs the chain rather than being stepped over. Sixteen routes per event is the ceiling; beyond it the event is refused. The matrix bends the oscillator voices - sine, triangle, square, sawtooth, supersaw, pulse, bytebeat; samples, wavetables and the zzfx family ignore it.",
        params: &[
            ReferenceParam {
                name: "value",
                r#type: "number | Pattern",
                description: "modulation index; 0 or absent = no route",
            },
        ],
        examples: &[
            "note(\"c2 e2\").s(\"sine\").fmi28(\"<0 1 4>\")",
        ],
        tags: &["control", "fm", "fm_matrix"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
};

pub(super) const FMI30: ControlRow = ControlRow {
    names: &["fmi30"],
    aliases: &["fm30"],
    reference:         ReferenceEntry {
        name: "fmi30",
        synonyms: &["fm30"],
        summary: "sends operator 3 into the carrier's frequency",
        description: "One cell of the FM matrix: the modulation operator 3 bends the carrier's frequency. An operator is built the first time any route names it at either end. The value is a modulation depth - an index, not hertz: the deviation it produces scales with the carrier and the source's harmonicity. A zero or absent amount makes no connection at all, and a gap severs the chain rather than being stepped over. Sixteen routes per event is the ceiling; beyond it the event is refused. The matrix bends the oscillator voices - sine, triangle, square, sawtooth, supersaw, pulse, bytebeat; samples, wavetables and the zzfx family ignore it.",
        params: &[
            ReferenceParam {
                name: "value",
                r#type: "number | Pattern",
                description: "modulation index; 0 or absent = no route",
            },
        ],
        examples: &[
            "note(\"c2 e2\").s(\"sine\").fmi30(\"<0 1 4>\")",
        ],
        tags: &["control", "fm", "fm_matrix"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
};

pub(super) const FMI31: ControlRow = ControlRow {
    names: &["fmi31"],
    aliases: &["fm31"],
    reference:         ReferenceEntry {
        name: "fmi31",
        synonyms: &["fm31"],
        summary: "sends operator 3 into operator 1",
        description: "One cell of the FM matrix: the modulation operator 3 bends operator 1. An operator is built the first time any route names it at either end. The value is a modulation depth - an index, not hertz: the deviation it produces scales with the carrier and the source's harmonicity. A zero or absent amount makes no connection at all, and a gap severs the chain rather than being stepped over. Sixteen routes per event is the ceiling; beyond it the event is refused. The matrix bends the oscillator voices - sine, triangle, square, sawtooth, supersaw, pulse, bytebeat; samples, wavetables and the zzfx family ignore it.",
        params: &[
            ReferenceParam {
                name: "value",
                r#type: "number | Pattern",
                description: "modulation index; 0 or absent = no route",
            },
        ],
        examples: &[
            "note(\"c2 e2\").s(\"sine\").fmi31(\"<0 1 4>\")",
        ],
        tags: &["control", "fm", "fm_matrix"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
};

pub(super) const FMI32: ControlRow = ControlRow {
    names: &["fmi32"],
    aliases: &["fm32"],
    reference:         ReferenceEntry {
        name: "fmi32",
        synonyms: &["fm32"],
        summary: "operator 3 into operator 2 - spelled fmi3 here",
        description: "This cell is the diagonal the resolver reads under the short spelling fmi3: operator 1 is unsuffixed throughout, so the fmi32 form is pinned for surface compatibility but never read. The behaviour is the fmi3 entry's. The value is a modulation depth - an index, not hertz: the deviation it produces scales with the carrier and the source's harmonicity. A zero or absent amount makes no connection at all, and a gap severs the chain rather than being stepped over. Sixteen routes per event is the ceiling; beyond it the event is refused. The matrix bends the oscillator voices - sine, triangle, square, sawtooth, supersaw, pulse, bytebeat; samples, wavetables and the zzfx family ignore it.",
        params: &[
            ReferenceParam {
                name: "value",
                r#type: "number | Pattern",
                description: "modulation index; 0 or absent = no route",
            },
        ],
        examples: &[
            "note(\"c2 e2\").s(\"sine\").fmi3(\"<0 2 4 8>\")",
        ],
        tags: &["control", "fm", "fm_matrix"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
};

pub(super) const FMI33: ControlRow = ControlRow {
    names: &["fmi33"],
    aliases: &["fm33"],
    reference:         ReferenceEntry {
        name: "fmi33",
        synonyms: &["fm33"],
        summary: "sends operator 3 into operator 3 - itself, as feedback",
        description: "One cell of the FM matrix: the modulation operator 3 bends operator 3 - itself, as feedback. An operator is built the first time any route names it at either end. The value is a modulation depth - an index, not hertz: the deviation it produces scales with the carrier and the source's harmonicity. A zero or absent amount makes no connection at all, and a gap severs the chain rather than being stepped over. Sixteen routes per event is the ceiling; beyond it the event is refused. The matrix bends the oscillator voices - sine, triangle, square, sawtooth, supersaw, pulse, bytebeat; samples, wavetables and the zzfx family ignore it.",
        params: &[
            ReferenceParam {
                name: "value",
                r#type: "number | Pattern",
                description: "modulation index; 0 or absent = no route",
            },
        ],
        examples: &[
            "note(\"c2 e2\").s(\"sine\").fmi33(\"<0 1 4>\")",
        ],
        tags: &["control", "fm", "fm_matrix"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
};

pub(super) const FMI34: ControlRow = ControlRow {
    names: &["fmi34"],
    aliases: &["fm34"],
    reference:         ReferenceEntry {
        name: "fmi34",
        synonyms: &["fm34"],
        summary: "sends operator 3 into operator 4",
        description: "One cell of the FM matrix: the modulation operator 3 bends operator 4. An operator is built the first time any route names it at either end. The value is a modulation depth - an index, not hertz: the deviation it produces scales with the carrier and the source's harmonicity. A zero or absent amount makes no connection at all, and a gap severs the chain rather than being stepped over. Sixteen routes per event is the ceiling; beyond it the event is refused. The matrix bends the oscillator voices - sine, triangle, square, sawtooth, supersaw, pulse, bytebeat; samples, wavetables and the zzfx family ignore it.",
        params: &[
            ReferenceParam {
                name: "value",
                r#type: "number | Pattern",
                description: "modulation index; 0 or absent = no route",
            },
        ],
        examples: &[
            "note(\"c2 e2\").s(\"sine\").fmi34(\"<0 1 4>\")",
        ],
        tags: &["control", "fm", "fm_matrix"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
};

pub(super) const FMI35: ControlRow = ControlRow {
    names: &["fmi35"],
    aliases: &["fm35"],
    reference:         ReferenceEntry {
        name: "fmi35",
        synonyms: &["fm35"],
        summary: "sends operator 3 into operator 5",
        description: "One cell of the FM matrix: the modulation operator 3 bends operator 5. An operator is built the first time any route names it at either end. The value is a modulation depth - an index, not hertz: the deviation it produces scales with the carrier and the source's harmonicity. A zero or absent amount makes no connection at all, and a gap severs the chain rather than being stepped over. Sixteen routes per event is the ceiling; beyond it the event is refused. The matrix bends the oscillator voices - sine, triangle, square, sawtooth, supersaw, pulse, bytebeat; samples, wavetables and the zzfx family ignore it.",
        params: &[
            ReferenceParam {
                name: "value",
                r#type: "number | Pattern",
                description: "modulation index; 0 or absent = no route",
            },
        ],
        examples: &[
            "note(\"c2 e2\").s(\"sine\").fmi35(\"<0 1 4>\")",
        ],
        tags: &["control", "fm", "fm_matrix"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
};

pub(super) const FMI36: ControlRow = ControlRow {
    names: &["fmi36"],
    aliases: &["fm36"],
    reference:         ReferenceEntry {
        name: "fmi36",
        synonyms: &["fm36"],
        summary: "sends operator 3 into operator 6",
        description: "One cell of the FM matrix: the modulation operator 3 bends operator 6. An operator is built the first time any route names it at either end. The value is a modulation depth - an index, not hertz: the deviation it produces scales with the carrier and the source's harmonicity. A zero or absent amount makes no connection at all, and a gap severs the chain rather than being stepped over. Sixteen routes per event is the ceiling; beyond it the event is refused. The matrix bends the oscillator voices - sine, triangle, square, sawtooth, supersaw, pulse, bytebeat; samples, wavetables and the zzfx family ignore it.",
        params: &[
            ReferenceParam {
                name: "value",
                r#type: "number | Pattern",
                description: "modulation index; 0 or absent = no route",
            },
        ],
        examples: &[
            "note(\"c2 e2\").s(\"sine\").fmi36(\"<0 1 4>\")",
        ],
        tags: &["control", "fm", "fm_matrix"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
};

pub(super) const FMI37: ControlRow = ControlRow {
    names: &["fmi37"],
    aliases: &["fm37"],
    reference:         ReferenceEntry {
        name: "fmi37",
        synonyms: &["fm37"],
        summary: "sends operator 3 into operator 7",
        description: "One cell of the FM matrix: the modulation operator 3 bends operator 7. An operator is built the first time any route names it at either end. The value is a modulation depth - an index, not hertz: the deviation it produces scales with the carrier and the source's harmonicity. A zero or absent amount makes no connection at all, and a gap severs the chain rather than being stepped over. Sixteen routes per event is the ceiling; beyond it the event is refused. The matrix bends the oscillator voices - sine, triangle, square, sawtooth, supersaw, pulse, bytebeat; samples, wavetables and the zzfx family ignore it.",
        params: &[
            ReferenceParam {
                name: "value",
                r#type: "number | Pattern",
                description: "modulation index; 0 or absent = no route",
            },
        ],
        examples: &[
            "note(\"c2 e2\").s(\"sine\").fmi37(\"<0 1 4>\")",
        ],
        tags: &["control", "fm", "fm_matrix"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
};

pub(super) const FMI38: ControlRow = ControlRow {
    names: &["fmi38"],
    aliases: &["fm38"],
    reference:         ReferenceEntry {
        name: "fmi38",
        synonyms: &["fm38"],
        summary: "sends operator 3 into operator 8",
        description: "One cell of the FM matrix: the modulation operator 3 bends operator 8. An operator is built the first time any route names it at either end. The value is a modulation depth - an index, not hertz: the deviation it produces scales with the carrier and the source's harmonicity. A zero or absent amount makes no connection at all, and a gap severs the chain rather than being stepped over. Sixteen routes per event is the ceiling; beyond it the event is refused. The matrix bends the oscillator voices - sine, triangle, square, sawtooth, supersaw, pulse, bytebeat; samples, wavetables and the zzfx family ignore it.",
        params: &[
            ReferenceParam {
                name: "value",
                r#type: "number | Pattern",
                description: "modulation index; 0 or absent = no route",
            },
        ],
        examples: &[
            "note(\"c2 e2\").s(\"sine\").fmi38(\"<0 1 4>\")",
        ],
        tags: &["control", "fm", "fm_matrix"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
};

pub(super) const FMI40: ControlRow = ControlRow {
    names: &["fmi40"],
    aliases: &["fm40"],
    reference:         ReferenceEntry {
        name: "fmi40",
        synonyms: &["fm40"],
        summary: "sends operator 4 into the carrier's frequency",
        description: "One cell of the FM matrix: the modulation operator 4 bends the carrier's frequency. An operator is built the first time any route names it at either end. The value is a modulation depth - an index, not hertz: the deviation it produces scales with the carrier and the source's harmonicity. A zero or absent amount makes no connection at all, and a gap severs the chain rather than being stepped over. Sixteen routes per event is the ceiling; beyond it the event is refused. The matrix bends the oscillator voices - sine, triangle, square, sawtooth, supersaw, pulse, bytebeat; samples, wavetables and the zzfx family ignore it.",
        params: &[
            ReferenceParam {
                name: "value",
                r#type: "number | Pattern",
                description: "modulation index; 0 or absent = no route",
            },
        ],
        examples: &[
            "note(\"c2 e2\").s(\"sine\").fmi40(\"<0 1 4>\")",
        ],
        tags: &["control", "fm", "fm_matrix"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
};

pub(super) const FMI41: ControlRow = ControlRow {
    names: &["fmi41"],
    aliases: &["fm41"],
    reference:         ReferenceEntry {
        name: "fmi41",
        synonyms: &["fm41"],
        summary: "sends operator 4 into operator 1",
        description: "One cell of the FM matrix: the modulation operator 4 bends operator 1. An operator is built the first time any route names it at either end. The value is a modulation depth - an index, not hertz: the deviation it produces scales with the carrier and the source's harmonicity. A zero or absent amount makes no connection at all, and a gap severs the chain rather than being stepped over. Sixteen routes per event is the ceiling; beyond it the event is refused. The matrix bends the oscillator voices - sine, triangle, square, sawtooth, supersaw, pulse, bytebeat; samples, wavetables and the zzfx family ignore it.",
        params: &[
            ReferenceParam {
                name: "value",
                r#type: "number | Pattern",
                description: "modulation index; 0 or absent = no route",
            },
        ],
        examples: &[
            "note(\"c2 e2\").s(\"sine\").fmi41(\"<0 1 4>\")",
        ],
        tags: &["control", "fm", "fm_matrix"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
};

pub(super) const FMI42: ControlRow = ControlRow {
    names: &["fmi42"],
    aliases: &["fm42"],
    reference:         ReferenceEntry {
        name: "fmi42",
        synonyms: &["fm42"],
        summary: "sends operator 4 into operator 2",
        description: "One cell of the FM matrix: the modulation operator 4 bends operator 2. An operator is built the first time any route names it at either end. The value is a modulation depth - an index, not hertz: the deviation it produces scales with the carrier and the source's harmonicity. A zero or absent amount makes no connection at all, and a gap severs the chain rather than being stepped over. Sixteen routes per event is the ceiling; beyond it the event is refused. The matrix bends the oscillator voices - sine, triangle, square, sawtooth, supersaw, pulse, bytebeat; samples, wavetables and the zzfx family ignore it.",
        params: &[
            ReferenceParam {
                name: "value",
                r#type: "number | Pattern",
                description: "modulation index; 0 or absent = no route",
            },
        ],
        examples: &[
            "note(\"c2 e2\").s(\"sine\").fmi42(\"<0 1 4>\")",
        ],
        tags: &["control", "fm", "fm_matrix"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
};

pub(super) const FMI43: ControlRow = ControlRow {
    names: &["fmi43"],
    aliases: &["fm43"],
    reference:         ReferenceEntry {
        name: "fmi43",
        synonyms: &["fm43"],
        summary: "operator 4 into operator 3 - spelled fmi4 here",
        description: "This cell is the diagonal the resolver reads under the short spelling fmi4: operator 1 is unsuffixed throughout, so the fmi43 form is pinned for surface compatibility but never read. The behaviour is the fmi4 entry's. The value is a modulation depth - an index, not hertz: the deviation it produces scales with the carrier and the source's harmonicity. A zero or absent amount makes no connection at all, and a gap severs the chain rather than being stepped over. Sixteen routes per event is the ceiling; beyond it the event is refused. The matrix bends the oscillator voices - sine, triangle, square, sawtooth, supersaw, pulse, bytebeat; samples, wavetables and the zzfx family ignore it.",
        params: &[
            ReferenceParam {
                name: "value",
                r#type: "number | Pattern",
                description: "modulation index; 0 or absent = no route",
            },
        ],
        examples: &[
            "note(\"c2 e2\").s(\"sine\").fmi4(\"<0 2 4 8>\")",
        ],
        tags: &["control", "fm", "fm_matrix"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
};

pub(super) const FMI44: ControlRow = ControlRow {
    names: &["fmi44"],
    aliases: &["fm44"],
    reference:         ReferenceEntry {
        name: "fmi44",
        synonyms: &["fm44"],
        summary: "sends operator 4 into operator 4 - itself, as feedback",
        description: "One cell of the FM matrix: the modulation operator 4 bends operator 4 - itself, as feedback. An operator is built the first time any route names it at either end. The value is a modulation depth - an index, not hertz: the deviation it produces scales with the carrier and the source's harmonicity. A zero or absent amount makes no connection at all, and a gap severs the chain rather than being stepped over. Sixteen routes per event is the ceiling; beyond it the event is refused. The matrix bends the oscillator voices - sine, triangle, square, sawtooth, supersaw, pulse, bytebeat; samples, wavetables and the zzfx family ignore it.",
        params: &[
            ReferenceParam {
                name: "value",
                r#type: "number | Pattern",
                description: "modulation index; 0 or absent = no route",
            },
        ],
        examples: &[
            "note(\"c2 e2\").s(\"sine\").fmi44(\"<0 1 4>\")",
        ],
        tags: &["control", "fm", "fm_matrix"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
};

pub(super) const FMI45: ControlRow = ControlRow {
    names: &["fmi45"],
    aliases: &["fm45"],
    reference:         ReferenceEntry {
        name: "fmi45",
        synonyms: &["fm45"],
        summary: "sends operator 4 into operator 5",
        description: "One cell of the FM matrix: the modulation operator 4 bends operator 5. An operator is built the first time any route names it at either end. The value is a modulation depth - an index, not hertz: the deviation it produces scales with the carrier and the source's harmonicity. A zero or absent amount makes no connection at all, and a gap severs the chain rather than being stepped over. Sixteen routes per event is the ceiling; beyond it the event is refused. The matrix bends the oscillator voices - sine, triangle, square, sawtooth, supersaw, pulse, bytebeat; samples, wavetables and the zzfx family ignore it.",
        params: &[
            ReferenceParam {
                name: "value",
                r#type: "number | Pattern",
                description: "modulation index; 0 or absent = no route",
            },
        ],
        examples: &[
            "note(\"c2 e2\").s(\"sine\").fmi45(\"<0 1 4>\")",
        ],
        tags: &["control", "fm", "fm_matrix"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
};

pub(super) const FMI46: ControlRow = ControlRow {
    names: &["fmi46"],
    aliases: &["fm46"],
    reference:         ReferenceEntry {
        name: "fmi46",
        synonyms: &["fm46"],
        summary: "sends operator 4 into operator 6",
        description: "One cell of the FM matrix: the modulation operator 4 bends operator 6. An operator is built the first time any route names it at either end. The value is a modulation depth - an index, not hertz: the deviation it produces scales with the carrier and the source's harmonicity. A zero or absent amount makes no connection at all, and a gap severs the chain rather than being stepped over. Sixteen routes per event is the ceiling; beyond it the event is refused. The matrix bends the oscillator voices - sine, triangle, square, sawtooth, supersaw, pulse, bytebeat; samples, wavetables and the zzfx family ignore it.",
        params: &[
            ReferenceParam {
                name: "value",
                r#type: "number | Pattern",
                description: "modulation index; 0 or absent = no route",
            },
        ],
        examples: &[
            "note(\"c2 e2\").s(\"sine\").fmi46(\"<0 1 4>\")",
        ],
        tags: &["control", "fm", "fm_matrix"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
};

pub(super) const FMI47: ControlRow = ControlRow {
    names: &["fmi47"],
    aliases: &["fm47"],
    reference:         ReferenceEntry {
        name: "fmi47",
        synonyms: &["fm47"],
        summary: "sends operator 4 into operator 7",
        description: "One cell of the FM matrix: the modulation operator 4 bends operator 7. An operator is built the first time any route names it at either end. The value is a modulation depth - an index, not hertz: the deviation it produces scales with the carrier and the source's harmonicity. A zero or absent amount makes no connection at all, and a gap severs the chain rather than being stepped over. Sixteen routes per event is the ceiling; beyond it the event is refused. The matrix bends the oscillator voices - sine, triangle, square, sawtooth, supersaw, pulse, bytebeat; samples, wavetables and the zzfx family ignore it.",
        params: &[
            ReferenceParam {
                name: "value",
                r#type: "number | Pattern",
                description: "modulation index; 0 or absent = no route",
            },
        ],
        examples: &[
            "note(\"c2 e2\").s(\"sine\").fmi47(\"<0 1 4>\")",
        ],
        tags: &["control", "fm", "fm_matrix"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
};

pub(super) const FMI48: ControlRow = ControlRow {
    names: &["fmi48"],
    aliases: &["fm48"],
    reference:         ReferenceEntry {
        name: "fmi48",
        synonyms: &["fm48"],
        summary: "sends operator 4 into operator 8",
        description: "One cell of the FM matrix: the modulation operator 4 bends operator 8. An operator is built the first time any route names it at either end. The value is a modulation depth - an index, not hertz: the deviation it produces scales with the carrier and the source's harmonicity. A zero or absent amount makes no connection at all, and a gap severs the chain rather than being stepped over. Sixteen routes per event is the ceiling; beyond it the event is refused. The matrix bends the oscillator voices - sine, triangle, square, sawtooth, supersaw, pulse, bytebeat; samples, wavetables and the zzfx family ignore it.",
        params: &[
            ReferenceParam {
                name: "value",
                r#type: "number | Pattern",
                description: "modulation index; 0 or absent = no route",
            },
        ],
        examples: &[
            "note(\"c2 e2\").s(\"sine\").fmi48(\"<0 1 4>\")",
        ],
        tags: &["control", "fm", "fm_matrix"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
};

pub(super) const FMI50: ControlRow = ControlRow {
    names: &["fmi50"],
    aliases: &["fm50"],
    reference:         ReferenceEntry {
        name: "fmi50",
        synonyms: &["fm50"],
        summary: "sends operator 5 into the carrier's frequency",
        description: "One cell of the FM matrix: the modulation operator 5 bends the carrier's frequency. An operator is built the first time any route names it at either end. The value is a modulation depth - an index, not hertz: the deviation it produces scales with the carrier and the source's harmonicity. A zero or absent amount makes no connection at all, and a gap severs the chain rather than being stepped over. Sixteen routes per event is the ceiling; beyond it the event is refused. The matrix bends the oscillator voices - sine, triangle, square, sawtooth, supersaw, pulse, bytebeat; samples, wavetables and the zzfx family ignore it.",
        params: &[
            ReferenceParam {
                name: "value",
                r#type: "number | Pattern",
                description: "modulation index; 0 or absent = no route",
            },
        ],
        examples: &[
            "note(\"c2 e2\").s(\"sine\").fmi50(\"<0 1 4>\")",
        ],
        tags: &["control", "fm", "fm_matrix"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
};

pub(super) const FMI51: ControlRow = ControlRow {
    names: &["fmi51"],
    aliases: &["fm51"],
    reference:         ReferenceEntry {
        name: "fmi51",
        synonyms: &["fm51"],
        summary: "sends operator 5 into operator 1",
        description: "One cell of the FM matrix: the modulation operator 5 bends operator 1. An operator is built the first time any route names it at either end. The value is a modulation depth - an index, not hertz: the deviation it produces scales with the carrier and the source's harmonicity. A zero or absent amount makes no connection at all, and a gap severs the chain rather than being stepped over. Sixteen routes per event is the ceiling; beyond it the event is refused. The matrix bends the oscillator voices - sine, triangle, square, sawtooth, supersaw, pulse, bytebeat; samples, wavetables and the zzfx family ignore it.",
        params: &[
            ReferenceParam {
                name: "value",
                r#type: "number | Pattern",
                description: "modulation index; 0 or absent = no route",
            },
        ],
        examples: &[
            "note(\"c2 e2\").s(\"sine\").fmi51(\"<0 1 4>\")",
        ],
        tags: &["control", "fm", "fm_matrix"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
};

pub(super) const FMI52: ControlRow = ControlRow {
    names: &["fmi52"],
    aliases: &["fm52"],
    reference:         ReferenceEntry {
        name: "fmi52",
        synonyms: &["fm52"],
        summary: "sends operator 5 into operator 2",
        description: "One cell of the FM matrix: the modulation operator 5 bends operator 2. An operator is built the first time any route names it at either end. The value is a modulation depth - an index, not hertz: the deviation it produces scales with the carrier and the source's harmonicity. A zero or absent amount makes no connection at all, and a gap severs the chain rather than being stepped over. Sixteen routes per event is the ceiling; beyond it the event is refused. The matrix bends the oscillator voices - sine, triangle, square, sawtooth, supersaw, pulse, bytebeat; samples, wavetables and the zzfx family ignore it.",
        params: &[
            ReferenceParam {
                name: "value",
                r#type: "number | Pattern",
                description: "modulation index; 0 or absent = no route",
            },
        ],
        examples: &[
            "note(\"c2 e2\").s(\"sine\").fmi52(\"<0 1 4>\")",
        ],
        tags: &["control", "fm", "fm_matrix"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
};

pub(super) const FMI53: ControlRow = ControlRow {
    names: &["fmi53"],
    aliases: &["fm53"],
    reference:         ReferenceEntry {
        name: "fmi53",
        synonyms: &["fm53"],
        summary: "sends operator 5 into operator 3",
        description: "One cell of the FM matrix: the modulation operator 5 bends operator 3. An operator is built the first time any route names it at either end. The value is a modulation depth - an index, not hertz: the deviation it produces scales with the carrier and the source's harmonicity. A zero or absent amount makes no connection at all, and a gap severs the chain rather than being stepped over. Sixteen routes per event is the ceiling; beyond it the event is refused. The matrix bends the oscillator voices - sine, triangle, square, sawtooth, supersaw, pulse, bytebeat; samples, wavetables and the zzfx family ignore it.",
        params: &[
            ReferenceParam {
                name: "value",
                r#type: "number | Pattern",
                description: "modulation index; 0 or absent = no route",
            },
        ],
        examples: &[
            "note(\"c2 e2\").s(\"sine\").fmi53(\"<0 1 4>\")",
        ],
        tags: &["control", "fm", "fm_matrix"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
};

pub(super) const FMI54: ControlRow = ControlRow {
    names: &["fmi54"],
    aliases: &["fm54"],
    reference:         ReferenceEntry {
        name: "fmi54",
        synonyms: &["fm54"],
        summary: "operator 5 into operator 4 - spelled fmi5 here",
        description: "This cell is the diagonal the resolver reads under the short spelling fmi5: operator 1 is unsuffixed throughout, so the fmi54 form is pinned for surface compatibility but never read. The behaviour is the fmi5 entry's. The value is a modulation depth - an index, not hertz: the deviation it produces scales with the carrier and the source's harmonicity. A zero or absent amount makes no connection at all, and a gap severs the chain rather than being stepped over. Sixteen routes per event is the ceiling; beyond it the event is refused. The matrix bends the oscillator voices - sine, triangle, square, sawtooth, supersaw, pulse, bytebeat; samples, wavetables and the zzfx family ignore it.",
        params: &[
            ReferenceParam {
                name: "value",
                r#type: "number | Pattern",
                description: "modulation index; 0 or absent = no route",
            },
        ],
        examples: &[
            "note(\"c2 e2\").s(\"sine\").fmi5(\"<0 2 4 8>\")",
        ],
        tags: &["control", "fm", "fm_matrix"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
};

pub(super) const FMI55: ControlRow = ControlRow {
    names: &["fmi55"],
    aliases: &["fm55"],
    reference:         ReferenceEntry {
        name: "fmi55",
        synonyms: &["fm55"],
        summary: "sends operator 5 into operator 5 - itself, as feedback",
        description: "One cell of the FM matrix: the modulation operator 5 bends operator 5 - itself, as feedback. An operator is built the first time any route names it at either end. The value is a modulation depth - an index, not hertz: the deviation it produces scales with the carrier and the source's harmonicity. A zero or absent amount makes no connection at all, and a gap severs the chain rather than being stepped over. Sixteen routes per event is the ceiling; beyond it the event is refused. The matrix bends the oscillator voices - sine, triangle, square, sawtooth, supersaw, pulse, bytebeat; samples, wavetables and the zzfx family ignore it.",
        params: &[
            ReferenceParam {
                name: "value",
                r#type: "number | Pattern",
                description: "modulation index; 0 or absent = no route",
            },
        ],
        examples: &[
            "note(\"c2 e2\").s(\"sine\").fmi55(\"<0 1 4>\")",
        ],
        tags: &["control", "fm", "fm_matrix"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
};

pub(super) const FMI56: ControlRow = ControlRow {
    names: &["fmi56"],
    aliases: &["fm56"],
    reference:         ReferenceEntry {
        name: "fmi56",
        synonyms: &["fm56"],
        summary: "sends operator 5 into operator 6",
        description: "One cell of the FM matrix: the modulation operator 5 bends operator 6. An operator is built the first time any route names it at either end. The value is a modulation depth - an index, not hertz: the deviation it produces scales with the carrier and the source's harmonicity. A zero or absent amount makes no connection at all, and a gap severs the chain rather than being stepped over. Sixteen routes per event is the ceiling; beyond it the event is refused. The matrix bends the oscillator voices - sine, triangle, square, sawtooth, supersaw, pulse, bytebeat; samples, wavetables and the zzfx family ignore it.",
        params: &[
            ReferenceParam {
                name: "value",
                r#type: "number | Pattern",
                description: "modulation index; 0 or absent = no route",
            },
        ],
        examples: &[
            "note(\"c2 e2\").s(\"sine\").fmi56(\"<0 1 4>\")",
        ],
        tags: &["control", "fm", "fm_matrix"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
};

pub(super) const FMI57: ControlRow = ControlRow {
    names: &["fmi57"],
    aliases: &["fm57"],
    reference:         ReferenceEntry {
        name: "fmi57",
        synonyms: &["fm57"],
        summary: "sends operator 5 into operator 7",
        description: "One cell of the FM matrix: the modulation operator 5 bends operator 7. An operator is built the first time any route names it at either end. The value is a modulation depth - an index, not hertz: the deviation it produces scales with the carrier and the source's harmonicity. A zero or absent amount makes no connection at all, and a gap severs the chain rather than being stepped over. Sixteen routes per event is the ceiling; beyond it the event is refused. The matrix bends the oscillator voices - sine, triangle, square, sawtooth, supersaw, pulse, bytebeat; samples, wavetables and the zzfx family ignore it.",
        params: &[
            ReferenceParam {
                name: "value",
                r#type: "number | Pattern",
                description: "modulation index; 0 or absent = no route",
            },
        ],
        examples: &[
            "note(\"c2 e2\").s(\"sine\").fmi57(\"<0 1 4>\")",
        ],
        tags: &["control", "fm", "fm_matrix"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
};

pub(super) const FMI58: ControlRow = ControlRow {
    names: &["fmi58"],
    aliases: &["fm58"],
    reference:         ReferenceEntry {
        name: "fmi58",
        synonyms: &["fm58"],
        summary: "sends operator 5 into operator 8",
        description: "One cell of the FM matrix: the modulation operator 5 bends operator 8. An operator is built the first time any route names it at either end. The value is a modulation depth - an index, not hertz: the deviation it produces scales with the carrier and the source's harmonicity. A zero or absent amount makes no connection at all, and a gap severs the chain rather than being stepped over. Sixteen routes per event is the ceiling; beyond it the event is refused. The matrix bends the oscillator voices - sine, triangle, square, sawtooth, supersaw, pulse, bytebeat; samples, wavetables and the zzfx family ignore it.",
        params: &[
            ReferenceParam {
                name: "value",
                r#type: "number | Pattern",
                description: "modulation index; 0 or absent = no route",
            },
        ],
        examples: &[
            "note(\"c2 e2\").s(\"sine\").fmi58(\"<0 1 4>\")",
        ],
        tags: &["control", "fm", "fm_matrix"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
};

pub(super) const FMI60: ControlRow = ControlRow {
    names: &["fmi60"],
    aliases: &["fm60"],
    reference:         ReferenceEntry {
        name: "fmi60",
        synonyms: &["fm60"],
        summary: "sends operator 6 into the carrier's frequency",
        description: "One cell of the FM matrix: the modulation operator 6 bends the carrier's frequency. An operator is built the first time any route names it at either end. The value is a modulation depth - an index, not hertz: the deviation it produces scales with the carrier and the source's harmonicity. A zero or absent amount makes no connection at all, and a gap severs the chain rather than being stepped over. Sixteen routes per event is the ceiling; beyond it the event is refused. The matrix bends the oscillator voices - sine, triangle, square, sawtooth, supersaw, pulse, bytebeat; samples, wavetables and the zzfx family ignore it.",
        params: &[
            ReferenceParam {
                name: "value",
                r#type: "number | Pattern",
                description: "modulation index; 0 or absent = no route",
            },
        ],
        examples: &[
            "note(\"c2 e2\").s(\"sine\").fmi60(\"<0 1 4>\")",
        ],
        tags: &["control", "fm", "fm_matrix"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
};

pub(super) const FMI61: ControlRow = ControlRow {
    names: &["fmi61"],
    aliases: &["fm61"],
    reference:         ReferenceEntry {
        name: "fmi61",
        synonyms: &["fm61"],
        summary: "sends operator 6 into operator 1",
        description: "One cell of the FM matrix: the modulation operator 6 bends operator 1. An operator is built the first time any route names it at either end. The value is a modulation depth - an index, not hertz: the deviation it produces scales with the carrier and the source's harmonicity. A zero or absent amount makes no connection at all, and a gap severs the chain rather than being stepped over. Sixteen routes per event is the ceiling; beyond it the event is refused. The matrix bends the oscillator voices - sine, triangle, square, sawtooth, supersaw, pulse, bytebeat; samples, wavetables and the zzfx family ignore it.",
        params: &[
            ReferenceParam {
                name: "value",
                r#type: "number | Pattern",
                description: "modulation index; 0 or absent = no route",
            },
        ],
        examples: &[
            "note(\"c2 e2\").s(\"sine\").fmi61(\"<0 1 4>\")",
        ],
        tags: &["control", "fm", "fm_matrix"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
};

pub(super) const FMI62: ControlRow = ControlRow {
    names: &["fmi62"],
    aliases: &["fm62"],
    reference:         ReferenceEntry {
        name: "fmi62",
        synonyms: &["fm62"],
        summary: "sends operator 6 into operator 2",
        description: "One cell of the FM matrix: the modulation operator 6 bends operator 2. An operator is built the first time any route names it at either end. The value is a modulation depth - an index, not hertz: the deviation it produces scales with the carrier and the source's harmonicity. A zero or absent amount makes no connection at all, and a gap severs the chain rather than being stepped over. Sixteen routes per event is the ceiling; beyond it the event is refused. The matrix bends the oscillator voices - sine, triangle, square, sawtooth, supersaw, pulse, bytebeat; samples, wavetables and the zzfx family ignore it.",
        params: &[
            ReferenceParam {
                name: "value",
                r#type: "number | Pattern",
                description: "modulation index; 0 or absent = no route",
            },
        ],
        examples: &[
            "note(\"c2 e2\").s(\"sine\").fmi62(\"<0 1 4>\")",
        ],
        tags: &["control", "fm", "fm_matrix"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
};

pub(super) const FMI63: ControlRow = ControlRow {
    names: &["fmi63"],
    aliases: &["fm63"],
    reference:         ReferenceEntry {
        name: "fmi63",
        synonyms: &["fm63"],
        summary: "sends operator 6 into operator 3",
        description: "One cell of the FM matrix: the modulation operator 6 bends operator 3. An operator is built the first time any route names it at either end. The value is a modulation depth - an index, not hertz: the deviation it produces scales with the carrier and the source's harmonicity. A zero or absent amount makes no connection at all, and a gap severs the chain rather than being stepped over. Sixteen routes per event is the ceiling; beyond it the event is refused. The matrix bends the oscillator voices - sine, triangle, square, sawtooth, supersaw, pulse, bytebeat; samples, wavetables and the zzfx family ignore it.",
        params: &[
            ReferenceParam {
                name: "value",
                r#type: "number | Pattern",
                description: "modulation index; 0 or absent = no route",
            },
        ],
        examples: &[
            "note(\"c2 e2\").s(\"sine\").fmi63(\"<0 1 4>\")",
        ],
        tags: &["control", "fm", "fm_matrix"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
};

pub(super) const FMI64: ControlRow = ControlRow {
    names: &["fmi64"],
    aliases: &["fm64"],
    reference:         ReferenceEntry {
        name: "fmi64",
        synonyms: &["fm64"],
        summary: "sends operator 6 into operator 4",
        description: "One cell of the FM matrix: the modulation operator 6 bends operator 4. An operator is built the first time any route names it at either end. The value is a modulation depth - an index, not hertz: the deviation it produces scales with the carrier and the source's harmonicity. A zero or absent amount makes no connection at all, and a gap severs the chain rather than being stepped over. Sixteen routes per event is the ceiling; beyond it the event is refused. The matrix bends the oscillator voices - sine, triangle, square, sawtooth, supersaw, pulse, bytebeat; samples, wavetables and the zzfx family ignore it.",
        params: &[
            ReferenceParam {
                name: "value",
                r#type: "number | Pattern",
                description: "modulation index; 0 or absent = no route",
            },
        ],
        examples: &[
            "note(\"c2 e2\").s(\"sine\").fmi64(\"<0 1 4>\")",
        ],
        tags: &["control", "fm", "fm_matrix"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
};

pub(super) const FMI65: ControlRow = ControlRow {
    names: &["fmi65"],
    aliases: &["fm65"],
    reference:         ReferenceEntry {
        name: "fmi65",
        synonyms: &["fm65"],
        summary: "operator 6 into operator 5 - spelled fmi6 here",
        description: "This cell is the diagonal the resolver reads under the short spelling fmi6: operator 1 is unsuffixed throughout, so the fmi65 form is pinned for surface compatibility but never read. The behaviour is the fmi6 entry's. The value is a modulation depth - an index, not hertz: the deviation it produces scales with the carrier and the source's harmonicity. A zero or absent amount makes no connection at all, and a gap severs the chain rather than being stepped over. Sixteen routes per event is the ceiling; beyond it the event is refused. The matrix bends the oscillator voices - sine, triangle, square, sawtooth, supersaw, pulse, bytebeat; samples, wavetables and the zzfx family ignore it.",
        params: &[
            ReferenceParam {
                name: "value",
                r#type: "number | Pattern",
                description: "modulation index; 0 or absent = no route",
            },
        ],
        examples: &[
            "note(\"c2 e2\").s(\"sine\").fmi6(\"<0 2 4 8>\")",
        ],
        tags: &["control", "fm", "fm_matrix"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
};

pub(super) const FMI66: ControlRow = ControlRow {
    names: &["fmi66"],
    aliases: &["fm66"],
    reference:         ReferenceEntry {
        name: "fmi66",
        synonyms: &["fm66"],
        summary: "sends operator 6 into operator 6 - itself, as feedback",
        description: "One cell of the FM matrix: the modulation operator 6 bends operator 6 - itself, as feedback. An operator is built the first time any route names it at either end. The value is a modulation depth - an index, not hertz: the deviation it produces scales with the carrier and the source's harmonicity. A zero or absent amount makes no connection at all, and a gap severs the chain rather than being stepped over. Sixteen routes per event is the ceiling; beyond it the event is refused. The matrix bends the oscillator voices - sine, triangle, square, sawtooth, supersaw, pulse, bytebeat; samples, wavetables and the zzfx family ignore it.",
        params: &[
            ReferenceParam {
                name: "value",
                r#type: "number | Pattern",
                description: "modulation index; 0 or absent = no route",
            },
        ],
        examples: &[
            "note(\"c2 e2\").s(\"sine\").fmi66(\"<0 1 4>\")",
        ],
        tags: &["control", "fm", "fm_matrix"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
};

pub(super) const FMI67: ControlRow = ControlRow {
    names: &["fmi67"],
    aliases: &["fm67"],
    reference:         ReferenceEntry {
        name: "fmi67",
        synonyms: &["fm67"],
        summary: "sends operator 6 into operator 7",
        description: "One cell of the FM matrix: the modulation operator 6 bends operator 7. An operator is built the first time any route names it at either end. The value is a modulation depth - an index, not hertz: the deviation it produces scales with the carrier and the source's harmonicity. A zero or absent amount makes no connection at all, and a gap severs the chain rather than being stepped over. Sixteen routes per event is the ceiling; beyond it the event is refused. The matrix bends the oscillator voices - sine, triangle, square, sawtooth, supersaw, pulse, bytebeat; samples, wavetables and the zzfx family ignore it.",
        params: &[
            ReferenceParam {
                name: "value",
                r#type: "number | Pattern",
                description: "modulation index; 0 or absent = no route",
            },
        ],
        examples: &[
            "note(\"c2 e2\").s(\"sine\").fmi67(\"<0 1 4>\")",
        ],
        tags: &["control", "fm", "fm_matrix"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
};

pub(super) const FMI68: ControlRow = ControlRow {
    names: &["fmi68"],
    aliases: &["fm68"],
    reference:         ReferenceEntry {
        name: "fmi68",
        synonyms: &["fm68"],
        summary: "sends operator 6 into operator 8",
        description: "One cell of the FM matrix: the modulation operator 6 bends operator 8. An operator is built the first time any route names it at either end. The value is a modulation depth - an index, not hertz: the deviation it produces scales with the carrier and the source's harmonicity. A zero or absent amount makes no connection at all, and a gap severs the chain rather than being stepped over. Sixteen routes per event is the ceiling; beyond it the event is refused. The matrix bends the oscillator voices - sine, triangle, square, sawtooth, supersaw, pulse, bytebeat; samples, wavetables and the zzfx family ignore it.",
        params: &[
            ReferenceParam {
                name: "value",
                r#type: "number | Pattern",
                description: "modulation index; 0 or absent = no route",
            },
        ],
        examples: &[
            "note(\"c2 e2\").s(\"sine\").fmi68(\"<0 1 4>\")",
        ],
        tags: &["control", "fm", "fm_matrix"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
};

pub(super) const FMI70: ControlRow = ControlRow {
    names: &["fmi70"],
    aliases: &["fm70"],
    reference:         ReferenceEntry {
        name: "fmi70",
        synonyms: &["fm70"],
        summary: "sends operator 7 into the carrier's frequency",
        description: "One cell of the FM matrix: the modulation operator 7 bends the carrier's frequency. An operator is built the first time any route names it at either end. The value is a modulation depth - an index, not hertz: the deviation it produces scales with the carrier and the source's harmonicity. A zero or absent amount makes no connection at all, and a gap severs the chain rather than being stepped over. Sixteen routes per event is the ceiling; beyond it the event is refused. The matrix bends the oscillator voices - sine, triangle, square, sawtooth, supersaw, pulse, bytebeat; samples, wavetables and the zzfx family ignore it.",
        params: &[
            ReferenceParam {
                name: "value",
                r#type: "number | Pattern",
                description: "modulation index; 0 or absent = no route",
            },
        ],
        examples: &[
            "note(\"c2 e2\").s(\"sine\").fmi70(\"<0 1 4>\")",
        ],
        tags: &["control", "fm", "fm_matrix"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
};

pub(super) const FMI71: ControlRow = ControlRow {
    names: &["fmi71"],
    aliases: &["fm71"],
    reference:         ReferenceEntry {
        name: "fmi71",
        synonyms: &["fm71"],
        summary: "sends operator 7 into operator 1",
        description: "One cell of the FM matrix: the modulation operator 7 bends operator 1. An operator is built the first time any route names it at either end. The value is a modulation depth - an index, not hertz: the deviation it produces scales with the carrier and the source's harmonicity. A zero or absent amount makes no connection at all, and a gap severs the chain rather than being stepped over. Sixteen routes per event is the ceiling; beyond it the event is refused. The matrix bends the oscillator voices - sine, triangle, square, sawtooth, supersaw, pulse, bytebeat; samples, wavetables and the zzfx family ignore it.",
        params: &[
            ReferenceParam {
                name: "value",
                r#type: "number | Pattern",
                description: "modulation index; 0 or absent = no route",
            },
        ],
        examples: &[
            "note(\"c2 e2\").s(\"sine\").fmi71(\"<0 1 4>\")",
        ],
        tags: &["control", "fm", "fm_matrix"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
};

pub(super) const FMI72: ControlRow = ControlRow {
    names: &["fmi72"],
    aliases: &["fm72"],
    reference:         ReferenceEntry {
        name: "fmi72",
        synonyms: &["fm72"],
        summary: "sends operator 7 into operator 2",
        description: "One cell of the FM matrix: the modulation operator 7 bends operator 2. An operator is built the first time any route names it at either end. The value is a modulation depth - an index, not hertz: the deviation it produces scales with the carrier and the source's harmonicity. A zero or absent amount makes no connection at all, and a gap severs the chain rather than being stepped over. Sixteen routes per event is the ceiling; beyond it the event is refused. The matrix bends the oscillator voices - sine, triangle, square, sawtooth, supersaw, pulse, bytebeat; samples, wavetables and the zzfx family ignore it.",
        params: &[
            ReferenceParam {
                name: "value",
                r#type: "number | Pattern",
                description: "modulation index; 0 or absent = no route",
            },
        ],
        examples: &[
            "note(\"c2 e2\").s(\"sine\").fmi72(\"<0 1 4>\")",
        ],
        tags: &["control", "fm", "fm_matrix"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
};

pub(super) const FMI73: ControlRow = ControlRow {
    names: &["fmi73"],
    aliases: &["fm73"],
    reference:         ReferenceEntry {
        name: "fmi73",
        synonyms: &["fm73"],
        summary: "sends operator 7 into operator 3",
        description: "One cell of the FM matrix: the modulation operator 7 bends operator 3. An operator is built the first time any route names it at either end. The value is a modulation depth - an index, not hertz: the deviation it produces scales with the carrier and the source's harmonicity. A zero or absent amount makes no connection at all, and a gap severs the chain rather than being stepped over. Sixteen routes per event is the ceiling; beyond it the event is refused. The matrix bends the oscillator voices - sine, triangle, square, sawtooth, supersaw, pulse, bytebeat; samples, wavetables and the zzfx family ignore it.",
        params: &[
            ReferenceParam {
                name: "value",
                r#type: "number | Pattern",
                description: "modulation index; 0 or absent = no route",
            },
        ],
        examples: &[
            "note(\"c2 e2\").s(\"sine\").fmi73(\"<0 1 4>\")",
        ],
        tags: &["control", "fm", "fm_matrix"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
};

pub(super) const FMI74: ControlRow = ControlRow {
    names: &["fmi74"],
    aliases: &["fm74"],
    reference:         ReferenceEntry {
        name: "fmi74",
        synonyms: &["fm74"],
        summary: "sends operator 7 into operator 4",
        description: "One cell of the FM matrix: the modulation operator 7 bends operator 4. An operator is built the first time any route names it at either end. The value is a modulation depth - an index, not hertz: the deviation it produces scales with the carrier and the source's harmonicity. A zero or absent amount makes no connection at all, and a gap severs the chain rather than being stepped over. Sixteen routes per event is the ceiling; beyond it the event is refused. The matrix bends the oscillator voices - sine, triangle, square, sawtooth, supersaw, pulse, bytebeat; samples, wavetables and the zzfx family ignore it.",
        params: &[
            ReferenceParam {
                name: "value",
                r#type: "number | Pattern",
                description: "modulation index; 0 or absent = no route",
            },
        ],
        examples: &[
            "note(\"c2 e2\").s(\"sine\").fmi74(\"<0 1 4>\")",
        ],
        tags: &["control", "fm", "fm_matrix"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
};

pub(super) const FMI75: ControlRow = ControlRow {
    names: &["fmi75"],
    aliases: &["fm75"],
    reference:         ReferenceEntry {
        name: "fmi75",
        synonyms: &["fm75"],
        summary: "sends operator 7 into operator 5",
        description: "One cell of the FM matrix: the modulation operator 7 bends operator 5. An operator is built the first time any route names it at either end. The value is a modulation depth - an index, not hertz: the deviation it produces scales with the carrier and the source's harmonicity. A zero or absent amount makes no connection at all, and a gap severs the chain rather than being stepped over. Sixteen routes per event is the ceiling; beyond it the event is refused. The matrix bends the oscillator voices - sine, triangle, square, sawtooth, supersaw, pulse, bytebeat; samples, wavetables and the zzfx family ignore it.",
        params: &[
            ReferenceParam {
                name: "value",
                r#type: "number | Pattern",
                description: "modulation index; 0 or absent = no route",
            },
        ],
        examples: &[
            "note(\"c2 e2\").s(\"sine\").fmi75(\"<0 1 4>\")",
        ],
        tags: &["control", "fm", "fm_matrix"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
};

pub(super) const FMI76: ControlRow = ControlRow {
    names: &["fmi76"],
    aliases: &["fm76"],
    reference:         ReferenceEntry {
        name: "fmi76",
        synonyms: &["fm76"],
        summary: "operator 7 into operator 6 - spelled fmi7 here",
        description: "This cell is the diagonal the resolver reads under the short spelling fmi7: operator 1 is unsuffixed throughout, so the fmi76 form is pinned for surface compatibility but never read. The behaviour is the fmi7 entry's. The value is a modulation depth - an index, not hertz: the deviation it produces scales with the carrier and the source's harmonicity. A zero or absent amount makes no connection at all, and a gap severs the chain rather than being stepped over. Sixteen routes per event is the ceiling; beyond it the event is refused. The matrix bends the oscillator voices - sine, triangle, square, sawtooth, supersaw, pulse, bytebeat; samples, wavetables and the zzfx family ignore it.",
        params: &[
            ReferenceParam {
                name: "value",
                r#type: "number | Pattern",
                description: "modulation index; 0 or absent = no route",
            },
        ],
        examples: &[
            "note(\"c2 e2\").s(\"sine\").fmi7(\"<0 2 4 8>\")",
        ],
        tags: &["control", "fm", "fm_matrix"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
};

pub(super) const FMI77: ControlRow = ControlRow {
    names: &["fmi77"],
    aliases: &["fm77"],
    reference:         ReferenceEntry {
        name: "fmi77",
        synonyms: &["fm77"],
        summary: "sends operator 7 into operator 7 - itself, as feedback",
        description: "One cell of the FM matrix: the modulation operator 7 bends operator 7 - itself, as feedback. An operator is built the first time any route names it at either end. The value is a modulation depth - an index, not hertz: the deviation it produces scales with the carrier and the source's harmonicity. A zero or absent amount makes no connection at all, and a gap severs the chain rather than being stepped over. Sixteen routes per event is the ceiling; beyond it the event is refused. The matrix bends the oscillator voices - sine, triangle, square, sawtooth, supersaw, pulse, bytebeat; samples, wavetables and the zzfx family ignore it.",
        params: &[
            ReferenceParam {
                name: "value",
                r#type: "number | Pattern",
                description: "modulation index; 0 or absent = no route",
            },
        ],
        examples: &[
            "note(\"c2 e2\").s(\"sine\").fmi77(\"<0 1 4>\")",
        ],
        tags: &["control", "fm", "fm_matrix"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
};

pub(super) const FMI78: ControlRow = ControlRow {
    names: &["fmi78"],
    aliases: &["fm78"],
    reference:         ReferenceEntry {
        name: "fmi78",
        synonyms: &["fm78"],
        summary: "sends operator 7 into operator 8",
        description: "One cell of the FM matrix: the modulation operator 7 bends operator 8. An operator is built the first time any route names it at either end. The value is a modulation depth - an index, not hertz: the deviation it produces scales with the carrier and the source's harmonicity. A zero or absent amount makes no connection at all, and a gap severs the chain rather than being stepped over. Sixteen routes per event is the ceiling; beyond it the event is refused. The matrix bends the oscillator voices - sine, triangle, square, sawtooth, supersaw, pulse, bytebeat; samples, wavetables and the zzfx family ignore it.",
        params: &[
            ReferenceParam {
                name: "value",
                r#type: "number | Pattern",
                description: "modulation index; 0 or absent = no route",
            },
        ],
        examples: &[
            "note(\"c2 e2\").s(\"sine\").fmi78(\"<0 1 4>\")",
        ],
        tags: &["control", "fm", "fm_matrix"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
};

pub(super) const FMI80: ControlRow = ControlRow {
    names: &["fmi80"],
    aliases: &["fm80"],
    reference:         ReferenceEntry {
        name: "fmi80",
        synonyms: &["fm80"],
        summary: "sends operator 8 into the carrier's frequency",
        description: "One cell of the FM matrix: the modulation operator 8 bends the carrier's frequency. An operator is built the first time any route names it at either end. The value is a modulation depth - an index, not hertz: the deviation it produces scales with the carrier and the source's harmonicity. A zero or absent amount makes no connection at all, and a gap severs the chain rather than being stepped over. Sixteen routes per event is the ceiling; beyond it the event is refused. The matrix bends the oscillator voices - sine, triangle, square, sawtooth, supersaw, pulse, bytebeat; samples, wavetables and the zzfx family ignore it.",
        params: &[
            ReferenceParam {
                name: "value",
                r#type: "number | Pattern",
                description: "modulation index; 0 or absent = no route",
            },
        ],
        examples: &[
            "note(\"c2 e2\").s(\"sine\").fmi80(\"<0 1 4>\")",
        ],
        tags: &["control", "fm", "fm_matrix"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
};

pub(super) const FMI81: ControlRow = ControlRow {
    names: &["fmi81"],
    aliases: &["fm81"],
    reference:         ReferenceEntry {
        name: "fmi81",
        synonyms: &["fm81"],
        summary: "sends operator 8 into operator 1",
        description: "One cell of the FM matrix: the modulation operator 8 bends operator 1. An operator is built the first time any route names it at either end. The value is a modulation depth - an index, not hertz: the deviation it produces scales with the carrier and the source's harmonicity. A zero or absent amount makes no connection at all, and a gap severs the chain rather than being stepped over. Sixteen routes per event is the ceiling; beyond it the event is refused. The matrix bends the oscillator voices - sine, triangle, square, sawtooth, supersaw, pulse, bytebeat; samples, wavetables and the zzfx family ignore it.",
        params: &[
            ReferenceParam {
                name: "value",
                r#type: "number | Pattern",
                description: "modulation index; 0 or absent = no route",
            },
        ],
        examples: &[
            "note(\"c2 e2\").s(\"sine\").fmi81(\"<0 1 4>\")",
        ],
        tags: &["control", "fm", "fm_matrix"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
};

pub(super) const FMI82: ControlRow = ControlRow {
    names: &["fmi82"],
    aliases: &["fm82"],
    reference:         ReferenceEntry {
        name: "fmi82",
        synonyms: &["fm82"],
        summary: "sends operator 8 into operator 2",
        description: "One cell of the FM matrix: the modulation operator 8 bends operator 2. An operator is built the first time any route names it at either end. The value is a modulation depth - an index, not hertz: the deviation it produces scales with the carrier and the source's harmonicity. A zero or absent amount makes no connection at all, and a gap severs the chain rather than being stepped over. Sixteen routes per event is the ceiling; beyond it the event is refused. The matrix bends the oscillator voices - sine, triangle, square, sawtooth, supersaw, pulse, bytebeat; samples, wavetables and the zzfx family ignore it.",
        params: &[
            ReferenceParam {
                name: "value",
                r#type: "number | Pattern",
                description: "modulation index; 0 or absent = no route",
            },
        ],
        examples: &[
            "note(\"c2 e2\").s(\"sine\").fmi82(\"<0 1 4>\")",
        ],
        tags: &["control", "fm", "fm_matrix"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
};

pub(super) const FMI83: ControlRow = ControlRow {
    names: &["fmi83"],
    aliases: &["fm83"],
    reference:         ReferenceEntry {
        name: "fmi83",
        synonyms: &["fm83"],
        summary: "sends operator 8 into operator 3",
        description: "One cell of the FM matrix: the modulation operator 8 bends operator 3. An operator is built the first time any route names it at either end. The value is a modulation depth - an index, not hertz: the deviation it produces scales with the carrier and the source's harmonicity. A zero or absent amount makes no connection at all, and a gap severs the chain rather than being stepped over. Sixteen routes per event is the ceiling; beyond it the event is refused. The matrix bends the oscillator voices - sine, triangle, square, sawtooth, supersaw, pulse, bytebeat; samples, wavetables and the zzfx family ignore it.",
        params: &[
            ReferenceParam {
                name: "value",
                r#type: "number | Pattern",
                description: "modulation index; 0 or absent = no route",
            },
        ],
        examples: &[
            "note(\"c2 e2\").s(\"sine\").fmi83(\"<0 1 4>\")",
        ],
        tags: &["control", "fm", "fm_matrix"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
};

pub(super) const FMI84: ControlRow = ControlRow {
    names: &["fmi84"],
    aliases: &["fm84"],
    reference:         ReferenceEntry {
        name: "fmi84",
        synonyms: &["fm84"],
        summary: "sends operator 8 into operator 4",
        description: "One cell of the FM matrix: the modulation operator 8 bends operator 4. An operator is built the first time any route names it at either end. The value is a modulation depth - an index, not hertz: the deviation it produces scales with the carrier and the source's harmonicity. A zero or absent amount makes no connection at all, and a gap severs the chain rather than being stepped over. Sixteen routes per event is the ceiling; beyond it the event is refused. The matrix bends the oscillator voices - sine, triangle, square, sawtooth, supersaw, pulse, bytebeat; samples, wavetables and the zzfx family ignore it.",
        params: &[
            ReferenceParam {
                name: "value",
                r#type: "number | Pattern",
                description: "modulation index; 0 or absent = no route",
            },
        ],
        examples: &[
            "note(\"c2 e2\").s(\"sine\").fmi84(\"<0 1 4>\")",
        ],
        tags: &["control", "fm", "fm_matrix"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
};

pub(super) const FMI85: ControlRow = ControlRow {
    names: &["fmi85"],
    aliases: &["fm85"],
    reference:         ReferenceEntry {
        name: "fmi85",
        synonyms: &["fm85"],
        summary: "sends operator 8 into operator 5",
        description: "One cell of the FM matrix: the modulation operator 8 bends operator 5. An operator is built the first time any route names it at either end. The value is a modulation depth - an index, not hertz: the deviation it produces scales with the carrier and the source's harmonicity. A zero or absent amount makes no connection at all, and a gap severs the chain rather than being stepped over. Sixteen routes per event is the ceiling; beyond it the event is refused. The matrix bends the oscillator voices - sine, triangle, square, sawtooth, supersaw, pulse, bytebeat; samples, wavetables and the zzfx family ignore it.",
        params: &[
            ReferenceParam {
                name: "value",
                r#type: "number | Pattern",
                description: "modulation index; 0 or absent = no route",
            },
        ],
        examples: &[
            "note(\"c2 e2\").s(\"sine\").fmi85(\"<0 1 4>\")",
        ],
        tags: &["control", "fm", "fm_matrix"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
};

pub(super) const FMI86: ControlRow = ControlRow {
    names: &["fmi86"],
    aliases: &["fm86"],
    reference:         ReferenceEntry {
        name: "fmi86",
        synonyms: &["fm86"],
        summary: "sends operator 8 into operator 6",
        description: "One cell of the FM matrix: the modulation operator 8 bends operator 6. An operator is built the first time any route names it at either end. The value is a modulation depth - an index, not hertz: the deviation it produces scales with the carrier and the source's harmonicity. A zero or absent amount makes no connection at all, and a gap severs the chain rather than being stepped over. Sixteen routes per event is the ceiling; beyond it the event is refused. The matrix bends the oscillator voices - sine, triangle, square, sawtooth, supersaw, pulse, bytebeat; samples, wavetables and the zzfx family ignore it.",
        params: &[
            ReferenceParam {
                name: "value",
                r#type: "number | Pattern",
                description: "modulation index; 0 or absent = no route",
            },
        ],
        examples: &[
            "note(\"c2 e2\").s(\"sine\").fmi86(\"<0 1 4>\")",
        ],
        tags: &["control", "fm", "fm_matrix"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
};

pub(super) const FMI87: ControlRow = ControlRow {
    names: &["fmi87"],
    aliases: &["fm87"],
    reference:         ReferenceEntry {
        name: "fmi87",
        synonyms: &["fm87"],
        summary: "operator 8 into operator 7 - spelled fmi8 here",
        description: "This cell is the diagonal the resolver reads under the short spelling fmi8: operator 1 is unsuffixed throughout, so the fmi87 form is pinned for surface compatibility but never read. The behaviour is the fmi8 entry's. The value is a modulation depth - an index, not hertz: the deviation it produces scales with the carrier and the source's harmonicity. A zero or absent amount makes no connection at all, and a gap severs the chain rather than being stepped over. Sixteen routes per event is the ceiling; beyond it the event is refused. The matrix bends the oscillator voices - sine, triangle, square, sawtooth, supersaw, pulse, bytebeat; samples, wavetables and the zzfx family ignore it.",
        params: &[
            ReferenceParam {
                name: "value",
                r#type: "number | Pattern",
                description: "modulation index; 0 or absent = no route",
            },
        ],
        examples: &[
            "note(\"c2 e2\").s(\"sine\").fmi8(\"<0 2 4 8>\")",
        ],
        tags: &["control", "fm", "fm_matrix"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
};

pub(super) const FMI88: ControlRow = ControlRow {
    names: &["fmi88"],
    aliases: &["fm88"],
    reference:         ReferenceEntry {
        name: "fmi88",
        synonyms: &["fm88"],
        summary: "sends operator 8 into operator 8 - itself, as feedback",
        description: "One cell of the FM matrix: the modulation operator 8 bends operator 8 - itself, as feedback. An operator is built the first time any route names it at either end. The value is a modulation depth - an index, not hertz: the deviation it produces scales with the carrier and the source's harmonicity. A zero or absent amount makes no connection at all, and a gap severs the chain rather than being stepped over. Sixteen routes per event is the ceiling; beyond it the event is refused. The matrix bends the oscillator voices - sine, triangle, square, sawtooth, supersaw, pulse, bytebeat; samples, wavetables and the zzfx family ignore it.",
        params: &[
            ReferenceParam {
                name: "value",
                r#type: "number | Pattern",
                description: "modulation index; 0 or absent = no route",
            },
        ],
        examples: &[
            "note(\"c2 e2\").s(\"sine\").fmi88(\"<0 1 4>\")",
        ],
        tags: &["control", "fm", "fm_matrix"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
};
