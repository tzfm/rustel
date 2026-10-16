use super::{ControlRow, ReferenceEntry, ReferenceParam};

pub(super) const CPS: ControlRow = ControlRow {
    names: &["cps"],
    aliases: &[],
    reference:         ReferenceEntry {
        name: "cps",
        synonyms: &[],
        summary: "the tempo, in cycles per second - state of the engine, not of the event",
        description: "The free cps(value) form changes the session tempo atomically, like setCps(value), and may be placed in a labeled lane. The default is 0.5. At query time the scheduler injects _cps, which is what loopAt and glide read to tell a real trigger from a lookahead. A chained .cps(value) plays that pattern at the requested cycles per second.",
        params: &[ReferenceParam {
            name: "value",
            r#type: "number",
            description: "cycles per second",
        }],
        examples: &[
            "kick: s(\"bd*4\")\ntempochanges: cps(1).gain(0)",
        ],
        tags: &["control", "tempo"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
};
