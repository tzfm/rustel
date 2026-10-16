use super::{ControlRow, ReferenceEntry, ReferenceParam};

pub(super) const SOURCE: ControlRow = ControlRow {
    names: &["source"],
    aliases: &["src"],
    reference: ReferenceEntry {
        name: "source",
        synonyms: &["src"],
        summary: "Store a source value; unused by native audio.",
        description: "Stores a `source` value on each event for compatibility. The native renderer does not use this control. Use `s` to select a built-in sound or sample.",
        params: &[
            ReferenceParam {
                name: "getSource",
                r#type: "function",
                description: "",
            },
        ],
        examples: &[],
        tags: &["external_io", "audio"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
};

pub(super) const CHANNELS: ControlRow = ControlRow {
    names: &["channels"],
    aliases: &["ch"],
    reference: ReferenceEntry {
        name: "channels",
        synonyms: &["ch"],
        summary: "Allows you to set the output channels on the interface",
        description: "Allows you to set the output channels on the interface",
        params: &[
            ReferenceParam {
                name: "channels",
                r#type: "number | Pattern",
                description: "pattern the output channels",
            },
        ],
        examples: &[
            "note(\"e a d b g\").channels(\"3:4\")",
        ],
        tags: &["external_io", "audio"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
};
