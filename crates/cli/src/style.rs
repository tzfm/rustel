use std::io::IsTerminal as _;

use rustel_runtime::terminal_text;

/// Text from a score, tape or diagnostic as [`terminal_text::visible`] shows
/// it, but keeping line breaks (a CRLF prints as LF) and tabs, so source and
/// multi-line messages print as their lines.
pub fn safe_source(text: &str) -> String {
    text.replace("\r\n", "\n")
        .split('\n')
        .map(|line| {
            line.split('\t')
                .map(terminal_text::visible)
                .collect::<Vec<_>>()
                .join("\t")
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Whether escapes may be written, taking the environment as arguments so
/// the policy can be tested without setting a process-wide variable.
///
/// A pty does not prove that the terminal renders escapes. With `TERM=dumb`
/// (emacs' `M-x shell`, an editor's build pane), `ESC[32m` prints as literal
/// text. A job with no `TERM`, such as one started by `cron`, is treated the
/// same. Windows is the exception: its consoles render escapes and do not set
/// `TERM`, so an absent `TERM` keeps colour there.
///
/// `anstyle-query` asks the same questions for clap, and also reads an empty
/// `TERM` as a name that is not `dumb`. The help and the output below it
/// therefore agree.
fn on(stream_is_terminal: bool, no_color: bool, term: Option<&str>) -> bool {
    let term_renders_escapes = match term {
        Some(term) => term != "dumb",
        None => cfg!(windows),
    };
    stream_is_terminal && !no_color && term_renders_escapes
}

fn from_environment(stream_is_terminal: bool) -> bool {
    // `NO_COLOR` counts only when it has a value: no-color.org asks for
    // "present and not an empty string", and an empty one is how a shell
    // spells unsetting it for a single command.
    let no_color = super::color_disabled();
    // A `TERM` that is not UTF-8 is no terminfo name any terminal answers
    // to, so it is read the way an unset one is.
    let term = std::env::var("TERM").ok();
    on(stream_is_terminal, no_color, term.as_deref())
}

pub fn stdout_on() -> bool {
    from_environment(std::io::stdout().is_terminal())
}

pub fn stderr_on() -> bool {
    from_environment(std::io::stderr().is_terminal())
}

fn paint(enabled: bool, code: &str, text: &str) -> String {
    if enabled {
        format!("\x1b[{code}m{text}\x1b[0m")
    } else {
        text.to_string()
    }
}

pub fn bold(enabled: bool, text: &str) -> String {
    paint(enabled, "1", text)
}
pub fn dim(enabled: bool, text: &str) -> String {
    paint(enabled, "2", text)
}
pub fn red(enabled: bool, text: &str) -> String {
    paint(enabled, "31", text)
}
pub fn green(enabled: bool, text: &str) -> String {
    paint(enabled, "32", text)
}
pub fn yellow(enabled: bool, text: &str) -> String {
    paint(enabled, "33", text)
}
pub fn cyan(enabled: bool, text: &str) -> String {
    paint(enabled, "36", text)
}

#[cfg(test)]
mod colour_policy_tests {
    use super::*;

    #[test]
    fn a_dumb_term_leaves_every_painted_string_plain() {
        let enabled = on(true, false, Some("dumb"));
        assert!(!enabled, "a dumb terminal prints escapes instead of colour");
        for painted in [
            bold(enabled, "wrote"),
            dim(enabled, "line 3"),
            red(enabled, "✗ parse: unexpected `)`"),
            green(enabled, "✓ valid"),
            yellow(enabled, "no MIDI outputs"),
            cyan(enabled, "0"),
        ] {
            assert!(
                !painted.contains('\x1b'),
                "TERM=dumb was sent an escape in {painted:?}"
            );
        }
    }

    #[test]
    fn colour_asks_the_stream_no_color_and_term_in_turn() {
        assert!(
            on(true, false, Some("xterm-256color")),
            "a terminal with nothing to object is the whole point of the palette"
        );
        assert!(
            !on(false, false, Some("xterm-256color")),
            "a pipe takes plain text however the terminal is set"
        );
        assert!(
            !on(true, true, Some("xterm-256color")),
            "NO_COLOR outranks the terminal"
        );
        assert!(
            on(true, false, Some("")),
            "an exported-but-empty TERM is not the name `dumb`, and anstream \
         keeps colour for it - the help and the body must not disagree"
        );
        // cmd.exe and PowerShell colour happily and set no TERM at all, so
        // this is the one answer that has to differ by platform.
        assert_eq!(on(true, false, None), cfg!(windows));
    }
}

#[cfg(test)]
mod safe_source_tests {
    use super::*;

    #[test]
    fn score_source_keeps_its_layout_and_shows_embedded_controls() {
        let source = "note('c3')\r\n\t.s(\"sine\")\n\u{1b}]52;c;payload\u{7}\r\u{202e}\r";
        assert_eq!(
            safe_source(source),
            "note('c3')\n\t.s(\"sine\")\n␛]52;c;payload␇␍�␍"
        );
    }
}
