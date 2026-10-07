//! Check that documentation names the output latency control and does not
//! promise an input latency control. Input lag is automatic. Compiled-in docs
//! need no Studio instance or audio device.

const STUDIO: &str = include_str!("../../../docs/studio.md");
const HARDWARE: &str = include_str!("../../../docs/hardware.md");
const CLI: &str = include_str!("../../../docs/cli.md");

/// The text in lower case with every run of whitespace, line breaks
/// included, read as one space, and with backticks and asterisks dropped:
/// a row name wrapped across two lines of prose, set as code, or set in the
/// Advanced list's italics is still that row name.
fn normalised(text: &str) -> String {
    text.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .filter(|c| !matches!(c, '`' | '*'))
        .collect::<String>()
        .to_lowercase()
}

#[test]
fn the_docs_never_offer_an_audio_in_latency_row() {
    for (name, text) in [
        ("docs/studio.md", STUDIO),
        ("docs/hardware.md", HARDWARE),
        ("docs/cli.md", CLI),
    ] {
        let text = normalised(text);
        for removed in ["audio in latency", "input latency row"] {
            assert!(
                !text.contains(removed),
                "{name} mentions {removed:?}: the input lag is always automatic \
                 and has no settings row"
            );
        }
    }
}

#[test]
fn the_output_latency_row_is_named_as_the_sheet_names_it() {
    for (name, text) in [
        ("docs/studio.md", STUDIO),
        ("docs/hardware.md", HARDWARE),
        ("docs/cli.md", CLI),
    ] {
        let text = normalised(text);
        assert!(
            text.contains("audio out latency"),
            "{name} no longer names the `audio out latency` row"
        );
        assert!(
            !text.contains("output latency row"),
            "{name} calls the row `output latency`; the sheet says `audio out latency`"
        );
    }
}
