use super::{ControlRow, ReferenceEntry, ReferenceParam};

pub(super) const ACTIVE_LABEL: ControlRow = ControlRow {
    names: &["activeLabel"],
    aliases: &[],
    reference:         ReferenceEntry {
        name: "activeLabel",
        synonyms: &[],
        summary: "the label the painter shows while the event sounds",
        description: "label(\"a:b\") spreads onto label and activeLabel positionally. A painter shows the plain label while the event is quiet and this one while it sounds, falling back to the note name or the sound. The audio engine reads neither.",
        params: &[
            ReferenceParam {
                name: "value",
                r#type: "string | Pattern",
                description: "text to show while sounding",
            },
        ],
        examples: &[
            "note(\"c e g\").label(\"quiet:LOUD\")",
        ],
        tags: &["control", "visualization"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
};

pub(super) const LABEL: ControlRow = ControlRow {
    names: &["label", "activeLabel"],
    aliases: &[],
    reference: ReferenceEntry {
        name: "label",
        synonyms: &[],
        summary: "Sets the displayed text for an event on the pianoroll",
        description: "Sets the displayed text for an event on the pianoroll",
        params: &[
            ReferenceParam {
                name: "label",
                r#type: "string",
                description: "text to display",
            },
        ],
        examples: &[],
        tags: &["visualization"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
};

pub(super) const COLOR: ControlRow = ControlRow {
    names: &["color", "colour"],
    aliases: &[],
    reference: ReferenceEntry {
        name: "color",
        synonyms: &["colour"],
        summary: "Sets the color of the hap in visualizations like pianoroll or highlighting.",
        description: "Sets the color of the hap in visualizations like pianoroll or highlighting.",
        params: &[
            ReferenceParam {
                name: "color",
                r#type: "string",
                description: "Hexadecimal or CSS color name",
            },
        ],
        examples: &[],
        tags: &["visualization"],
        no_autocomplete: false,
        deprecated: false,
        origin: "rustel",
    },
};
