use super::{ControlRow, ReferenceEntry, ReferenceParam};

pub(super) const MIDICHAN: ControlRow = ControlRow {
    names: &["midichan"],
    aliases: &[],
    reference: ReferenceEntry {
        name: "midichan",
        synonyms: &[],
        summary: "MIDI channel: Sets the MIDI channel for the event.",
        description: "MIDI channel: Sets the MIDI channel for the event.",
        params: &[
            ReferenceParam {
                name: "channel",
                r#type: "number | Pattern",
                description: "MIDI channel number (0-15)",
            },
        ],
        examples: &[
            "note(\"c4\").midichan(1).midi()",
        ],
        tags: &["external_io", "midi"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
};

pub(super) const MIDIMAP: ControlRow = ControlRow {
    names: &["midimap"],
    aliases: &[],
    reference:         ReferenceEntry {
        name: "midimap",
        synonyms: &[],
        summary: "pick which control-to-CC map a MIDI-bound pattern sends",
        description: "A named map - registered with midimaps - turns the controls a hap carries into CC messages, each value normalised into the entry's range. The name must be a string; null and numbers select nothing, and a hap naming a map that was never registered sends no mapped CCs at all.\n\nA pattern with no midimap falls back to the midimap option of its .midi(port, { midimap }) call, and then to the \"default\" map that defaultmidimap sets. Orthogonal to midichan and midiport, which pick the channel and the port the CCs go out on.",
        params: &[
            ReferenceParam {
                name: "name",
                r#type: "string | Pattern",
                description: "the registered map to use",
            },
        ],
        examples: &[
            "midimaps({ lead: { lpf: 74 } })\nnote(\"c3 e3 g3\").midi().midimap(\"lead\").lpf(\"400 800 1600 3200\")",
        ],
        tags: &["control", "midi"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
};

pub(super) const MIDIPORT: ControlRow = ControlRow {
    names: &["midiport"],
    aliases: &[],
    reference: ReferenceEntry {
        name: "midiport",
        synonyms: &[],
        summary: "MIDI port: Sets the MIDI port for the event.",
        description: "MIDI port: Sets the MIDI port for the event.",
        params: &[
            ReferenceParam {
                name: "port",
                r#type: "number | Pattern",
                description: "MIDI port",
            },
        ],
        examples: &[
            "note(\"c a f e\").midiport(\"<0 1 2 3>\").midi()",
        ],
        tags: &["external_io", "midi"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
};

pub(super) const MIDICMD: ControlRow = ControlRow {
    names: &["midicmd"],
    aliases: &[],
    reference: ReferenceEntry {
        name: "midicmd",
        synonyms: &[],
        summary: "MIDI command: Sends a MIDI command message.",
        description: "MIDI command: Sends a MIDI command message.",
        params: &[
            ReferenceParam {
                name: "command",
                r#type: "number | Pattern",
                description: "MIDI command",
            },
        ],
        examples: &[
            "midicmd(\"clock*48,<start stop>/2\").midi()",
        ],
        tags: &["external_io", "midi"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
};

pub(super) const CCN: ControlRow = ControlRow {
    names: &["ccn"],
    aliases: &[],
    reference: ReferenceEntry {
        name: "ccn",
        synonyms: &[],
        summary: "MIDI control number: Sends a MIDI control change message.",
        description: "MIDI control number: Sends a MIDI control change message.",
        params: &[
            ReferenceParam {
                name: "MIDI",
                r#type: "number | Pattern",
                description: "control number (0-127)",
            },
        ],
        examples: &[],
        tags: &["external_io", "midi"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
};

pub(super) const CCV: ControlRow = ControlRow {
    names: &["ccv"],
    aliases: &[],
    reference: ReferenceEntry {
        name: "ccv",
        synonyms: &[],
        summary: "MIDI control value: Sends a MIDI control change message.",
        description: "MIDI control value: Sends a MIDI control change message.",
        params: &[
            ReferenceParam {
                name: "MIDI",
                r#type: "number | Pattern",
                description: "control value (0-127)",
            },
        ],
        examples: &[],
        tags: &["external_io", "midi"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
};

pub(super) const CTL_NUM: ControlRow = ControlRow {
    names: &["ctlNum"],
    aliases: &[],
    reference:         ReferenceEntry {
        name: "ctlNum",
        synonyms: &[],
        summary: "SuperDirt's spelling of the MIDI CC number - ccn wins when both are set",
        description: "MIDI output only: when a hap carries ccv but no ccn, ctlNum supplies the controller number. A CC goes out only when the number is a finite integer in 0..127 and a value is present too.",
        params: &[
            ReferenceParam {
                name: "value",
                r#type: "number | Pattern",
                description: "controller number 0..127",
            },
        ],
        examples: &[
            "note(\"a3\").midi().ctlNum(1).ccv(\"<0 64 127>\")",
        ],
        tags: &["control", "midi"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
};

pub(super) const NRPNN: ControlRow = ControlRow {
    names: &["nrpnn"],
    aliases: &[],
    reference: ReferenceEntry {
        name: "nrpnn",
        synonyms: &[],
        summary: "MIDI NRPN non-registered parameter number: Sends a MIDI NRPN non-registered parameter number message.",
        description: "MIDI NRPN non-registered parameter number: Sends a MIDI NRPN non-registered parameter number message.",
        params: &[
            ReferenceParam {
                name: "nrpnn",
                r#type: "number | Pattern",
                description: "MIDI NRPN non-registered parameter number (0-127)",
            },
        ],
        examples: &[
            "note(\"c4\").nrpnn(\"1:8\").nrpv(\"123\").midichan(1).midi()",
        ],
        tags: &["external_io", "midi"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
};

pub(super) const NRPV: ControlRow = ControlRow {
    names: &["nrpv"],
    aliases: &[],
    reference: ReferenceEntry {
        name: "nrpv",
        synonyms: &[],
        summary: "MIDI NRPN non-registered parameter value: Sends a MIDI NRPN non-registered parameter value message.",
        description: "MIDI NRPN non-registered parameter value: Sends a MIDI NRPN non-registered parameter value message.",
        params: &[
            ReferenceParam {
                name: "nrpv",
                r#type: "number | Pattern",
                description: "MIDI NRPN non-registered parameter value (0-127)",
            },
        ],
        examples: &[
            "note(\"c4\").nrpnn(\"1:8\").nrpv(\"123\").midichan(1).midi()",
        ],
        tags: &["external_io", "midi"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
};

pub(super) const PROG_NUM: ControlRow = ControlRow {
    names: &["progNum"],
    aliases: &[],
    reference: ReferenceEntry {
        name: "progNum",
        synonyms: &[],
        summary: "MIDI program number: Sends a MIDI program change message.",
        description: "MIDI program number: Sends a MIDI program change message.",
        params: &[
            ReferenceParam {
                name: "program",
                r#type: "number | Pattern",
                description: "MIDI program number (0-127)",
            },
        ],
        examples: &[
            "note(\"c4\").progNum(10).midichan(1).midi()",
        ],
        tags: &["external_io"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
};

pub(super) const SYSEXID: ControlRow = ControlRow {
    names: &["sysexid"],
    aliases: &[],
    reference: ReferenceEntry {
        name: "sysexid",
        synonyms: &[],
        summary: "MIDI sysex ID: Sends a MIDI sysex identifier message.",
        description: "MIDI sysex ID: Sends a MIDI sysex identifier message.",
        params: &[
            ReferenceParam {
                name: "id",
                r#type: "number | Pattern",
                description: "Sysex ID",
            },
        ],
        examples: &[
            "note(\"c4\").sysexid(\"0x77\").sysexdata(\"0x01:0x02:0x03:0x04\").midichan(1).midi()",
        ],
        tags: &["external_io", "midi"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
};

pub(super) const SYSEXDATA: ControlRow = ControlRow {
    names: &["sysexdata"],
    aliases: &[],
    reference: ReferenceEntry {
        name: "sysexdata",
        synonyms: &[],
        summary: "MIDI sysex data: Sends a MIDI sysex message.",
        description: "MIDI sysex data: Sends a MIDI sysex message.",
        params: &[
            ReferenceParam {
                name: "data",
                r#type: "number | Pattern",
                description: "Sysex data",
            },
        ],
        examples: &[
            "note(\"c4\").sysexid(\"0x77\").sysexdata(\"0x01:0x02:0x03:0x04\").midichan(1).midi()",
        ],
        tags: &["external_io", "midi"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
};

pub(super) const MIDIBEND: ControlRow = ControlRow {
    names: &["midibend"],
    aliases: &[],
    reference: ReferenceEntry {
        name: "midibend",
        synonyms: &[],
        summary: "MIDI pitch bend: Sends a MIDI pitch bend message.",
        description: "MIDI pitch bend: Sends a MIDI pitch bend message.",
        params: &[
            ReferenceParam {
                name: "midibend",
                r#type: "number | Pattern",
                description: "MIDI pitch bend (-1 - 1)",
            },
        ],
        examples: &[
            "note(\"c4\").midibend(sine.slow(4).range(-0.4,0.4)).midi()",
        ],
        tags: &["external_io", "midi"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
};

pub(super) const MIDITOUCH: ControlRow = ControlRow {
    names: &["miditouch"],
    aliases: &[],
    reference: ReferenceEntry {
        name: "miditouch",
        synonyms: &[],
        summary: "MIDI key after touch: Sends a MIDI key after touch message.",
        description: "MIDI key after touch: Sends a MIDI key after touch message.",
        params: &[
            ReferenceParam {
                name: "miditouch",
                r#type: "number | Pattern",
                description: "MIDI key after touch (0-1)",
            },
        ],
        examples: &[
            "note(\"c4\").miditouch(sine.slow(4).range(0,1)).midi()",
        ],
        tags: &["external_io", "midi"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
};

pub(super) const POLY_TOUCH: ControlRow = ControlRow {
    names: &["polyTouch"],
    aliases: &[],
    reference:         ReferenceEntry {
        name: "polyTouch",
        synonyms: &[],
        summary: "polyphonic aftertouch - installed, but this engine's MIDI output never sends it",
        description: "Channel aftertouch goes out as miditouch; per-note pressure has no output path here. The value rides the event and leaves the engine through .osc(), and a midimaps entry can turn it into a CC; natively it is silent.",
        params: &[
            ReferenceParam {
                name: "value",
                r#type: "number | Pattern",
                description: "per-note pressure",
            },
        ],
        examples: &[
            "note(\"c4 e4\").midi().polyTouch(\"<0 127>\")",
        ],
        tags: &["control", "midi"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
};
