//! `^J` - the quick action: what the caret is on, and what can be done to it.
//!
//! Typing `slider(800, 100, 4000)` by hand to get a fader is four numbers
//! and a lot of punctuation for one idea. Put the caret on the `800` that
//! is already there, press `^J`, and the menu that opens at the caret
//! offers to make it one. Enter turns the menu into a small form - value,
//! min, max, step - and Enter again writes the fader and closes. The form
//! is one Tab from any field, so a mistyped range is corrected in the form
//! rather than by starting the whole gesture again; what it will not do is
//! stay open over the score once it has written to it. A value outside its
//! own range is the one correction the form makes for you, and it makes it
//! on the form: the number is pulled in and said, nothing is written, and
//! the next Enter writes what you can now read.
//!
//! Once a sample is recorded, the menu also offers to paste its name,
//! `recordings:2`, wherever [`sample_paste`] finds a place the score
//! plays it from.
use super::*;
use ratatui::style::Style;
use ratatui::widgets::{Block, Borders, Clear, Paragraph};
use rustel_runtime::lint::scan;
use unicode_width::UnicodeWidthStr;

use super::super::number_form::{Field, SliderForm, field_value_x};

const STALE_SOURCE: &str = "the score or scene changed - close and reopen quick action";

/// What the quick action found at the caret, and so what it can offer.
#[derive(Clone, Debug, PartialEq)]
pub(super) enum Target {
    /// A plain number in the score: the `800` of `.lpf(800)`. Making it a
    /// fader replaces exactly those bytes.
    Number(std::ops::Range<usize>),
    /// A number that is already a slider: the whole `slider(...)` call,
    /// and what it currently holds. Editing it rewrites the call.
    Slider {
        call: std::ops::Range<usize>,
        value: f64,
        min: f64,
        max: f64,
        step: f64,
    },
    /// Nothing here a fader can be made of.
    ///
    /// A `slider(…)` is an expression, and an expression is only valid
    /// where a value goes. Dropping one at whatever byte the caret happens
    /// to be on - the first column of a statement, the middle of a comment -
    /// writes a score that does not parse, so the answer is to say there
    /// is nothing here rather than to guess. `why` is what to say. Where
    /// the latest recorded sample can be pasted, the menu opens over this
    /// with the paste row alone.
    Nothing(&'static str),
}

impl Target {
    /// What the menu's rows are, here. The first is the one Enter takes.
    fn rows(&self) -> &'static [&'static str] {
        match self {
            Self::Number(_) => &["slider\u{2026}  make this number a fader"],
            Self::Slider { .. } => &[
                "slider\u{2026}  edit this fader",
                "plain number  take the fader off",
            ],
            // No fader rows: the menu opens over it only to offer the
            // paste row, and otherwise `open_smart_action` says why.
            Self::Nothing(_) => &[],
        }
    }
}

/// What `^J` finds at a `slider(` the hand has started with nothing in it
/// yet - the most direct way anybody asks for a fader.
///
/// `literal_sliders` only sees calls with a value in them, so it does not
/// report an empty one. The form finishes the empty call where it stands.
#[derive(Clone, Debug, PartialEq)]
pub(super) enum EmptyCall {
    /// The bytes the form replaces: from `slider(` to the caret while the
    /// call is still open, or through the call's own `)` when it was closed
    /// empty - `slider()` with the caret between the two, which is how the
    /// reference column lands a call and how a hand that types brackets in
    /// pairs writes one. Replacing only `slider(` there would leave the old
    /// `)` behind and close the call twice, so the score would not parse.
    Span(std::ops::Range<usize>),
    /// Closed, but with a comment between the caret and the call's own
    /// `)`. The form writes whole calls; writing one over the comment would
    /// lose somebody's words, and writing one beside it would close the
    /// call twice. The player moves the comment, or finishes it by hand.
    Refused(&'static str),
}

/// The empty `slider(` call whose inside the caret stands in, if it does.
///
/// Only when what stands between `slider(` and the caret is blank:
/// `slider(0.5` is a call with an argument somebody is in the middle of
/// typing, and guessing at what they meant to finish is how a form earns
/// distrust. Read from the checker's blanking, as the menu row's
/// [`in_slider_call`] is, so a `slider(` inside a comment or a pattern
/// string is not one, and the row and the action agree.
pub(super) fn empty_call_at(source: &str, caret: usize) -> Option<EmptyCall> {
    let code = rustel_runtime::lint::code_only(source);
    let start = code.get(..caret)?.rfind("slider(")?;
    // A word of its own, not the tail of `mySlider(`.
    if start
        .checked_sub(1)
        .is_some_and(|at| scan::is_name_byte(code.as_bytes()[at]))
    {
        return None;
    }
    // Nothing typed inside it yet - not even a comment.
    if !source
        .get(start + "slider(".len()..caret)?
        .trim()
        .is_empty()
    {
        return None;
    }
    // The first code after the caret; a comment reads as blank here.
    let tail = code.get(caret..)?;
    let close = caret + (tail.len() - tail.trim_start().len());
    if code.as_bytes().get(close) != Some(&b')') || !owns_bracket(&code, start, close) {
        return Some(EmptyCall::Span(start..caret));
    }
    if !source.get(caret..close)?.trim().is_empty() {
        return Some(EmptyCall::Refused(
            "a comment sits inside this slider( - move it out, and the form can finish the call",
        ));
    }
    Some(EmptyCall::Span(start..close + 1))
}

/// Whether the `)` at `close` is the own close of the call opened at
/// `start`, in the checker's blanked copy of the score.
///
/// Whose `)` that is has two honest readings. `gain(slider())` is a call
/// closed and then closed again; `gain(slider()` is usually `gain()` typed
/// first, the caret stepped back inside and `slider(` typed there, so the
/// `)` belongs to `gain`. The brackets around the call decide: every call
/// still open where `slider(` starts has to find its own close after that
/// `)`, and when one of them cannot, the `)` was its. Counted within the
/// statement ([`statement_around`]), so a half-typed pattern elsewhere in
/// the score cannot move the answer, and blanked, so a bracket inside a
/// pattern string - the `(3,8)` of `"bd(3,8)"` - counts for nothing.
fn owns_bracket(code: &str, start: usize, close: usize) -> bool {
    let bytes = code.as_bytes();
    let statement = statement_around(code, start);
    let around = bytes[statement.start..start]
        .iter()
        .fold(0usize, |open, byte| match byte {
            b'(' => open + 1,
            b')' => open.saturating_sub(1),
            _ => open,
        });
    let mut from = close + 1;
    for _ in 0..around {
        match closing_bracket(&bytes[..statement.end], from) {
            Some(at) => from = at + 1,
            None => return false,
        }
    }
    true
}

/// The offset of the `)` that closes a bracket already open before
/// `from`, counting the brackets opened and closed after it. `None` when
/// the text runs out first.
fn closing_bracket(code: &[u8], from: usize) -> Option<usize> {
    let mut depth = 0usize;
    for (at, byte) in code.iter().enumerate().skip(from) {
        match byte {
            b'(' => depth += 1,
            b')' if depth == 0 => return Some(at),
            b')' => depth -= 1,
            _ => {}
        }
    }
    None
}

/// The top-level statement around `at`: from the line that starts it to
/// the line that starts the next. A statement starts on a line whose first
/// column is a label - `$:`, `_$:`, `drums:` - the way a score is written
/// pattern by pattern; a score with no labels is one statement.
fn statement_around(code: &str, at: usize) -> std::ops::Range<usize> {
    let mut statement = 0..code.len();
    let mut offset = 0;
    for line in code.split_inclusive('\n') {
        if label_len(line).is_some() {
            if offset <= at {
                statement.start = offset;
            } else {
                statement.end = offset;
                break;
            }
        }
        offset += line.len();
    }
    statement
}

/// The length of the label a line starts with, colon included: 2 for
/// `$: s("bd")`, 6 for `drums: …`.
fn label_len(line: &str) -> Option<usize> {
    let name = scan::name_starting_at(line, 0).len();
    (name > 0 && line.as_bytes().get(name) == Some(&b':')).then_some(name + 1)
}

/// Whether `range` is exactly one whole `slider(...)` call in `source`.
///
/// The range comes from `literal_sliders`, which reads the text rather
/// than a parse tree - so on a call that is already malformed it can name
/// an inner `slider(` of a nested pair. Replacing that inner range with a
/// fresh call leaves the outer call's arguments stranded beside it, and
/// the score ends up holding things like
///
/// ```text
/// slider(1.44, 0, 5, 0.01), 100, 20000, slider(100, 100, 20000, 0.01)))
/// ```
///
/// which is worse than the broken slider the player opened the form to
/// repair. The form writes into a document somebody is keeping, so when
/// the span is not a clean whole call it must decline rather than guess:
/// a refusal costs one message, a bad write costs the score.
pub(super) fn spans_one_whole_call(source: &str, range: &std::ops::Range<usize>) -> bool {
    let Some(text) = source.get(range.clone()) else {
        return false;
    };
    let Some(arguments) = text.strip_prefix("slider(") else {
        return false;
    };
    let Some(inside) = arguments.strip_suffix(')') else {
        return false;
    };
    // Balanced, and closed only at the very end: a nested `slider(` inside
    // would dip to zero early and that is the case this exists to catch.
    let mut depth = 0i32;
    for character in inside.chars() {
        match character {
            '(' => depth += 1,
            ')' => depth -= 1,
            _ => {}
        }
        if depth < 0 {
            return false;
        }
    }
    depth == 0 && !inside.contains("slider(")
}

/// The quick action, open at the caret.
#[derive(Clone, Debug)]
pub(super) struct SmartAction {
    pub(super) scene: SceneId,
    target: Target,
    /// The latest recorded sample, `recordings:2`, when the caret had a
    /// place to paste it as the menu opened: the menu's last row.
    sample: Option<String>,
    /// Where the caret was when it opened, on screen: the menu hangs off
    /// this, so it appears where the eye already is.
    anchor: (u16, u16),
    selected: usize,
    form: Option<SliderForm>,
    /// The score this target's span belongs to, captured when the menu
    /// opens; the form's opening numbers are read from it. A different
    /// score makes the span stale. Closing the menu leaves any applied edit
    /// in place: undo is undo's job.
    original: String,
}

impl SmartAction {
    /// The menu's rows, caret offers first, then the latest sample if
    /// there is one. The first is the one Enter takes.
    fn menu_rows(&self) -> Vec<String> {
        let mut rows: Vec<String> = self
            .target
            .rows()
            .iter()
            .map(|row| (*row).to_owned())
            .collect();
        if let Some(sound) = &self.sample {
            rows.push(format!("paste  {sound}"));
        }
        rows
    }

    /// The paste row's index, when the latest sample is on the menu.
    fn paste_row(&self) -> Option<usize> {
        self.sample.as_ref()?;
        Some(self.target.rows().len())
    }
}

/// Where ^J pastes a recorded sample for a caret at `caret`: the bytes to
/// replace and what to write there, or `None` where nothing near the
/// caret takes it.
///
/// A place [`paste_landing`] finds counts only when the score written
/// there plays `name:n`, with no bank, once more than before, as
/// [`rustel_runtime::lint::live_sound_names`] reads it. So a `note(…)`, `n(…)`,
/// `.scale(…)`, `.bank(…)` or `samples(…)` string takes no paste, nor a
/// pattern under a bank or a muted label, nor a name mini-notation splits
/// in two, a folder called `My Recordings`.
pub(super) fn sample_paste(
    source: &str,
    caret: usize,
    sound: &str,
) -> Option<(std::ops::Range<usize>, String)> {
    let (name, n) = match sound.split_once(':') {
        Some((name, index)) => (name, index.parse::<f64>().ok()?),
        None => (sound, 0.0),
    };
    let plays = |score: &str| {
        rustel_runtime::lint::live_sound_names(score)
            .into_iter()
            .filter(|named| named.name == name && named.n == n && named.banks.is_empty())
            .count()
    };
    let (range, text) = paste_landing(source, caret, sound)?;
    let mut written = source.to_owned();
    written.replace_range(range.clone(), &text);
    (plays(&written) > plays(source)).then_some((range, text))
}

/// Where the shape of the text around `caret` takes a sample's name,
/// without asking whether the score plays it there: [`sample_paste`]
/// asks.
///
/// - Strictly between a string's quotes, or in one left open at the end,
///   the bare name joins the pattern as a step of its own. A space parts
///   it from a neighbouring step, and a caret inside a step lands at that
///   step's end: `s("b|d")` gives `s("bd recordings:2")`.
/// - In a labelled statement with nothing after its label but `//`
///   comments, the `s("…")` call goes right after the label: `$: |`
///   gives `$: s("recordings:2")`.
/// - On a blank line of a score that has labels, or no code at all, a
///   `$: s("…")` line, when only blank lines and `//` comments stand
///   between it and the next label.
///
/// Anywhere else - in code, in a comment, just past a closing quote - a
/// paste would break the score or never play, so there is none.
fn paste_landing(
    source: &str,
    caret: usize,
    sound: &str,
) -> Option<(std::ops::Range<usize>, String)> {
    use rustel_runtime::lint::{Blanked, blanked_at};
    let caret = caret.min(source.len());
    // A newline past the end: a string or comment left open at the end of
    // the score covers it, so the end reads as inside that string.
    let padded = format!("{source}\n");
    let in_text = |at: usize| blanked_at(&padded, at) == Some(Blanked::Text);
    if caret > 0 && in_text(caret - 1) && in_text(caret) {
        let bytes = source.as_bytes();
        // Mini-notation steps start after these and end before them.
        let opens = |byte: u8| byte.is_ascii_whitespace() || b"\"'`[<{".contains(&byte);
        let closes = |byte: u8| byte.is_ascii_whitespace() || b"\"'`]>}".contains(&byte);
        let mut at = caret;
        if !opens(bytes[at - 1]) {
            while bytes.get(at).is_some_and(|byte| !closes(*byte)) && in_text(at + 1) {
                at += 1;
            }
        }
        let before = if opens(bytes[at - 1]) { "" } else { " " };
        let after = if bytes.get(at).is_none_or(|byte| closes(*byte)) {
            ""
        } else {
            " "
        };
        return Some((at..at, format!("{before}{sound}{after}")));
    }
    let only_comments = |text: &str| {
        text.lines().all(|line| {
            let line = line.trim();
            line.is_empty() || line.starts_with("//")
        })
    };
    let call = format!("s(\"{sound}\")");
    let code = rustel_runtime::lint::code_only(source);
    let statement = statement_around(&code, caret);
    let label = label_len(&code[statement.start..]).map(|len| statement.start + len);
    if let Some(after) = label
        && only_comments(&source[after..statement.end])
    {
        let line = &source[after..];
        let line = &line[..line.find('\n').unwrap_or(line.len())];
        let spaces = line.len() - line.trim_start_matches([' ', '\t']).len();
        let comment = !line[spaces..].trim().is_empty();
        let text = format!(" {call}{}", if comment { " " } else { "" });
        return Some((after..after + spaces, text));
    }
    let start = source[..caret].rfind('\n').map_or(0, |at| at + 1);
    let end = source[caret..]
        .find('\n')
        .map_or(source.len(), |at| caret + at);
    // A `\r` before the newline stays with the newline.
    let end = if source[start..end].ends_with('\r') {
        end - 1
    } else {
        end
    };
    let labels_or_empty = label.is_some() || statement.end < code.len() || code.trim().is_empty();
    (labels_or_empty
        && source[start..end].trim().is_empty()
        && blanked_at(&padded, end).is_none()
        && only_comments(&source[end..statement.end]))
    .then(|| (start..end, format!("$: {call}")))
}

/// A sensible range for a number that has never had one: nothing clever,
/// just something a player can drag straight away and correct in the form
/// if it is wrong. Zero to twice the number, on a round ceiling.
pub(super) fn guess_range(value: f64) -> (f64, f64, f64) {
    if !value.is_finite() || value == 0.0 {
        return (0.0, 1.0, 0.01);
    }
    let magnitude = value.abs();
    if magnitude <= 1.0 {
        let (low, high) = if value < 0.0 { (-1.0, 0.0) } else { (0.0, 1.0) };
        return (low, high, 0.01);
    }
    // A round ceiling above twice the number: 800 → 2000, 120 → 250.
    let wanted = magnitude * 2.0;
    let decade = 10f64.powf(wanted.log10().floor());
    let high = [1.0, 2.0, 2.5, 5.0, 10.0]
        .into_iter()
        .map(|multiple| multiple * decade)
        .find(|ceiling| *ceiling >= wanted)
        .unwrap_or(wanted);
    let step = if high >= 100.0 {
        1.0
    } else if high >= 10.0 {
        0.1
    } else {
        0.01
    };
    if value < 0.0 {
        (-high, 0.0, step)
    } else {
        (0.0, high, step)
    }
}

/// Whether a byte range is text rather than code: inside a string, or
/// inside a comment.
///
/// Both are places a `slider(…)` cannot become a control. In a pattern
/// string it is mini-notation and the parser rejects it; in a comment it
/// is valid text and does nothing at all, which is worse - the form would
/// appear to work and no fader would ever appear. The checker's own
/// blanking is the authority for what is code here, so the two agree
/// about `//` inside a string and a quote inside a comment.
fn is_text(source: &str, range: std::ops::Range<usize>) -> bool {
    is_blank(&rustel_runtime::lint::code_only(source), range)
}

/// [`is_text`] against a score the checker has already blanked.
fn is_blank(code: &str, range: std::ops::Range<usize>) -> bool {
    let end = range.end.max(range.start + 1).min(code.len());
    code.as_bytes()[range.start.min(code.len())..end]
        .iter()
        .all(|byte| *byte == b' ')
}

/// Why a number the checker blanked is not somewhere a fader can go.
///
/// Both halves come from the checker's own scanner rather than a second
/// reading of the quotes, so the message and the refusal can never
/// disagree - a `//` inside a pattern string is a pattern to both of them.
fn refusal(source: &str, at: usize) -> &'static str {
    match rustel_runtime::lint::blanked_at(source, at) {
        Some(rustel_runtime::lint::Blanked::Text) => {
            "that number is inside a pattern - a fader goes in the code around it"
        }
        // Blank and not text: a comment, or the space beside one.
        _ => "that number is in a comment - a fader there would never become a control",
    }
}

/// Whether an offset sits inside a `slider(` call, read off the score
/// the checker has blanked.
///
/// The action itself asks the transpiler, which knows the call's real
/// extent; this is the same question answered without a parse, for the
/// menu row that is rebuilt every frame. It reads the blanked score, so
/// a `slider(` written inside a pattern is not one, and an unclosed call
/// runs to the end - which is what the transpiler does with it too, so
/// typing `slider(0.5` and reaching for the menu finds the row live.
fn in_slider_call(code: &str, at: usize) -> bool {
    let bytes = code.as_bytes();
    let mut from = 0;
    while let Some(found) = code[from..].find("slider") {
        let start = from + found;
        from = start + "slider".len();
        // A word of its own, not the tail of `mySlider`.
        if start
            .checked_sub(1)
            .is_some_and(|at| scan::is_name_byte(bytes[at]))
        {
            continue;
        }
        let mut open = from;
        while bytes
            .get(open)
            .is_some_and(|byte| byte.is_ascii_whitespace())
        {
            open += 1;
        }
        if bytes.get(open) != Some(&b'(') {
            continue;
        }
        let end = closing_bracket(bytes, open + 1).unwrap_or(code.len());
        if start <= at && at <= end {
            return true;
        }
        from = end.max(from);
    }
    false
}

/// The number a press is about: a selection that is a number outright,
/// else the number the caret stands on.
///
/// Shared so the menu row and the action itself never disagree about
/// whether there is a number here at all.
fn number_meant(
    source: &str,
    from: usize,
    to: usize,
    caret: usize,
) -> Option<std::ops::Range<usize>> {
    (from < to)
        .then(|| {
            let trimmed = source[from..to].trim();
            let offset = source[from..to].find(trimmed).unwrap_or(0);
            from + offset..from + offset + trimmed.len()
        })
        .filter(|range| {
            source[range.clone()].parse::<f64>().is_ok() && stands_alone(source, range.clone())
        })
        .or_else(|| number_around(source, caret))
}

/// The run of digits around an offset, when there is one that parses.
fn number_around(source: &str, at: usize) -> Option<std::ops::Range<usize>> {
    let numeric = |byte: u8| byte.is_ascii_digit() || byte == b'.';
    let bytes = source.as_bytes();
    let at = at.min(source.len());
    // A caret just past a number belongs to it, as it does in every text
    // field: `800|` should still find the 800.
    let mut start = at;
    while start > 0 && numeric(bytes[start - 1]) {
        start -= 1;
    }
    let mut end = at;
    while end < bytes.len() && numeric(bytes[end]) {
        end += 1;
    }
    if start == end {
        return None;
    }
    // A leading minus is part of the number when nothing it could be
    // subtracted from stands before it.
    if start > 0 && bytes[start - 1] == b'-' {
        let before = source[..start - 1].trim_end().chars().next_back();
        if !matches!(before, Some(c) if c.is_alphanumeric() || matches!(c, ')' | ']' | '_' | '.')) {
            start -= 1;
        }
    }
    (source[start..end].parse::<f64>().is_ok() && stands_alone(source, start..end))
        .then_some(start..end)
}

/// Whether a run of digits is a whole number rather than part of a longer
/// token.
///
/// `1e3` scans as `1` and `3`, `0x1f` as `0` and `1`, `osc3` as `3`. Each
/// of them would take a `slider(…)` in the middle of a token and write a
/// score that does not parse - `.speed(1eslider(3, 0, 10, 0.1))`. A digit
/// with a letter against it is not a number this can offer to replace.
fn stands_alone(source: &str, range: std::ops::Range<usize>) -> bool {
    let bytes = source.as_bytes();
    let before = range.start.checked_sub(1).map(|at| bytes[at]);
    let after = bytes.get(range.end).copied();
    !before.is_some_and(scan::is_name_byte) && !after.is_some_and(scan::is_name_byte)
}

/// What the form says, judged the way Enter judges it: the four numbers
/// to write, or what to say on the form instead of writing. `whole` is
/// whether the span the form writes over is still one whole call.
///
/// The checks run in the order a player can act on them: a field that is
/// not a number, a range with no travel, a call that must be fixed in the
/// score first, and then a value outside its own range. The form corrects
/// only that last case, and it does so before anything is written: it puts
/// the moved number in its field and the warning under it. Nothing is
/// written on that Enter, because a write closes the form and the warning
/// would have nowhere to stay. Every other check has passed already, so
/// the next Enter writes what the form shows.
fn checked_numbers(form: &mut SliderForm, whole: bool) -> Result<[f64; 4], String> {
    let [value, min, max, step] = form.parse()?;
    if min >= max {
        return Err("min has to be below max".into());
    }
    if !whole {
        return Err("the call here is not one whole slider - fix it in the score first".into());
    }
    let clamped = value.clamp(min, max);
    if (clamped - value).abs() > f64::EPSILON {
        form.set_field(Field::Value, slider::format_value(clamped));
        return Err(PULLED.into());
    }
    let step = if step > 0.0 {
        step
    } else {
        (max - min) / 1000.0
    };
    Ok([clamped, min, max, step])
}

/// The form's word for a value it pulled into range. Short enough for the
/// form's row: the value and both ends of the range are on the rows above.
const PULLED: &str = "value pulled into range \u{b7} Enter writes";

/// Where the popup goes: hanging off the caret, pulled back onto the
/// screen when the caret is near an edge.
pub(super) fn geometry(frame: Rect, anchor: (u16, u16), rows: u16) -> Rect {
    let width = 44.min(frame.width.saturating_sub(2)).max(20);
    let height = (rows + 2).min(frame.height.saturating_sub(1)).max(3);
    let x = anchor
        .0
        .min(frame.right().saturating_sub(width + 1))
        .max(frame.x);
    // Below the caret if it fits, above it otherwise, never on top of it.
    let y = if anchor.1 + 1 + height <= frame.bottom() {
        anchor.1 + 1
    } else {
        anchor.1.saturating_sub(height).max(frame.y)
    };
    Rect::new(x, y, width, height)
}

impl App {
    /// Whether the quick action has anything to offer where the caret
    /// stands. The Edit menu greys its row out when it has not, rather
    /// than opening and answering with a status line nobody was looking
    /// at.
    ///
    /// Cheap on purpose: the menu is rebuilt on every frame, and the
    /// real target reads the transpiler's widget table, which is a
    /// parse of the whole score. The two agree about a number and about
    /// text - both ask the checker's own blanking. Where they can differ
    /// is a `slider(…)` whose arguments the transpiler will not take, a
    /// variable instead of a literal; there this says yes, the row stays
    /// live, and the action says why. Erring that way keeps a row that
    /// would have worked from ever being greyed out.
    ///
    /// It is also yes where [`sample_paste`] has a place for the latest
    /// recorded sample: the same check the menu makes for its paste row.
    pub(super) fn smart_action_offers(&self) -> bool {
        let source = self.editor().source();
        let selection = self.editor().primary_selection();
        let caret = selection.head.0.min(source.len());
        let (from, to) = (
            selection.anchor.0.min(selection.head.0),
            selection.anchor.0.max(selection.head.0),
        );
        // One blanking pass answers both halves: whether the number is
        // code, and whether the caret is in a `slider(` that is code.
        let code = rustel_runtime::lint::code_only(&source);
        if let Some(range) = number_meant(&source, from, to, caret)
            && !is_blank(&code, range)
        {
            return true;
        }
        in_slider_call(&code, caret)
            || self
                .last_recorded_sample
                .as_deref()
                .is_some_and(|sound| sample_paste(&source, caret, sound).is_some())
    }

    /// `^J`, or Edit ▸ Quick action. Reads what the caret is on and opens
    /// the menu over it.
    pub(super) fn open_smart_action(&mut self) {
        let scene = self.scenes.current().id;
        let source = self.editor().source();
        let selection = self.editor().primary_selection();
        let caret = selection.head.0.min(source.len());
        // A selected number is the number, even when the caret sits past
        // its last digit: selecting `800` and pressing the key is the most
        // explicit way anyone can say which number they meant.
        let (from, to) = (
            selection.anchor.0.min(selection.head.0),
            selection.anchor.0.max(selection.head.0),
        );
        // What the caret is on, in the order the answers get more
        // specific. A number already inside a `slider(…)` is that fader;
        // otherwise it is a number, and a number is only offerable where
        // it is code - the checker's own blanking is the authority for
        // that, so this and the linter agree about a `//` inside a string
        // and a quote inside a comment, and about a template literal that
        // runs over several lines.
        let number = number_meant(&source, from, to, caret);
        let on_number = number.is_some();
        let target = if let Some(widget) = rustel_runtime::ui_events::literal_sliders(&source)
            .into_iter()
            .find(|live| live.call.0 <= caret && caret <= live.call.1)
        {
            Target::Slider {
                call: widget.call.0..widget.call.1,
                value: widget.slider.value,
                min: widget.slider.min,
                max: widget.slider.max,
                step: widget.slider.step,
            }
        } else if let Some(empty) = empty_call_at(&source, caret) {
            match empty {
                // A fader with nothing in it yet: the form opens on the same
                // guesses a bare number gets, and Enter finishes the call.
                EmptyCall::Span(range) => {
                    let (min, max, step) = guess_range(1.0);
                    Target::Slider {
                        call: range,
                        value: 1.0,
                        min,
                        max,
                        step,
                    }
                }
                EmptyCall::Refused(why) => Target::Nothing(why),
            }
        } else if let Some(range) = number {
            if is_text(&source, range.clone()) {
                Target::Nothing(refusal(&source, range.start))
            } else {
                Target::Number(range)
            }
        } else if let Some(kind) = rustel_runtime::lint::blanked_at(&source, caret) {
            Target::Nothing(match kind {
                rustel_runtime::lint::Blanked::Text => {
                    "that is inside a pattern - a fader goes in the code around it"
                }
                rustel_runtime::lint::Blanked::Comment => {
                    "that is in a comment - a fader there would never become a control"
                }
            })
        } else {
            Target::Nothing("put the caret on a number first - that is what a fader is made of")
        };
        let anchor = self
            .focused_map()
            .and_then(|map| map.cell_for_offset(crate::editor::ByteOffset(caret)))
            .map_or(
                (self.regions.panes[self.focused].editor.x, self.frame.y),
                |cell| (cell.x, cell.y),
            );
        let sample = self
            .last_recorded_sample
            .clone()
            .filter(|sound| sample_paste(&source, caret, sound).is_some());
        let status = match (&target, &sample) {
            (Target::Nothing(why), None) => {
                self.status = format!("quick action: {why}");
                self.dirty_frame = true;
                return;
            }
            // A number the fader refuses is still said, beside the paste.
            (Target::Nothing(why), Some(sound)) if on_number => {
                format!("quick action: {why} \u{b7} Enter pastes {sound}")
            }
            (Target::Nothing(_), Some(sound)) => {
                format!("quick action: Enter pastes {sound} \u{b7} Esc closes")
            }
            _ => "quick action: Enter chooses \u{b7} Esc closes".into(),
        };
        self.dismiss_dialogs(None);
        self.smart_action = Some(SmartAction {
            scene,
            target,
            sample,
            anchor,
            selected: 0,
            form: None,
            original: source,
        });
        self.status = status;
        self.dirty_frame = true;
    }

    /// Enter on the menu's first row: the menu becomes the form, filled
    /// in from whatever was there.
    fn open_slider_form(&mut self) {
        let Some(state) = self.smart_action.as_mut() else {
            return;
        };
        let numbers = match &state.target {
            Target::Slider {
                value,
                min,
                max,
                step,
                ..
            } => [*value, *min, *max, *step],
            Target::Number(range) => {
                let value = state.original[range.clone()].parse::<f64>().unwrap_or(0.0);
                let (min, max, step) = guess_range(value);
                [value, min, max, step]
            }
            Target::Nothing(_) => return,
        };
        let mut form = SliderForm {
            fields: numbers.map(slider::format_value),
            cursor: 0,
            selected: true,
            field: 0,
            error: None,
            written: false,
        };
        form.focus(0);
        state.form = Some(form);
        self.status =
            "Tab or the arrows between the fields \u{b7} Enter applies \u{b7} Esc closes".into();
        self.dirty_frame = true;
    }

    /// Write the form into the score and close. Evaluation is explicit.
    ///
    /// The form stays open, with nothing written, only when there is
    /// something to read before a write: a field that is not a number, a
    /// range with no travel, a span that is not one whole call, a score or
    /// scene that changed under the form, a write the editor refused - and
    /// a value it had to pull into its range, which the next Enter writes.
    fn apply_slider_form(&mut self) {
        let Some(state) = self.smart_action.as_mut() else {
            return;
        };
        let Some(form) = state.form.as_mut() else {
            return;
        };
        let range = match &state.target {
            Target::Number(range) => range.clone(),
            Target::Slider { call, .. } => call.clone(),
            Target::Nothing(_) => return,
        };
        let scene = state.scene;
        // Captured spans belong to one scene and its unchanged source.
        // The mouse can move to another scene while this form is open.
        let current = self
            .scenes
            .get(scene)
            .map(|scene| scene.editor.source())
            .unwrap_or_default();
        if self.scenes.current().id != scene || current != state.original {
            form.error = Some(STALE_SOURCE.into());
            self.dirty_frame = true;
            return;
        }
        let open_ended = current
            .get(range.clone())
            .is_some_and(|text| text.starts_with("slider(") && !text.ends_with(')'));
        let whole = !matches!(state.target, Target::Slider { .. })
            || open_ended
            || spans_one_whole_call(&current, &range);
        let [value, min, max, step] = match checked_numbers(form, whole) {
            Ok(numbers) => numbers,
            Err(problem) => {
                form.error = Some(problem);
                self.dirty_frame = true;
                return;
            }
        };
        // What the form shows is what was written, should it stay up.
        form.set_field(Field::Step, slider::format_value(step));
        let call = format!(
            "slider({}, {}, {}, {})",
            slider::format_value(value),
            slider::format_value(min),
            slider::format_value(max),
            slider::format_value(step)
        );
        let written = self.replace_range_in_scene(scene, range, &call).is_some();
        let Some(form) = self
            .smart_action
            .as_mut()
            .and_then(|state| state.form.as_mut())
        else {
            return;
        };
        if !written {
            form.error = Some("could not write the fader - close and reopen quick action".into());
            self.dirty_frame = true;
            return;
        }
        form.written = true;
        // Written and away: the form has done its job and the fader it
        // made is the thing to reach for now.
        self.close_smart_action();
    }

    /// Take the fader off again, leaving the number it was holding.
    fn unwrap_slider(&mut self) {
        let Some(state) = self.smart_action.take() else {
            return;
        };
        let Target::Slider { call, value, .. } = &state.target else {
            self.smart_action = Some(state);
            return;
        };
        let plain = slider::format_value(*value);
        let range = call.clone();
        let scene = state.scene;
        if self.scenes.current().id != scene
            || self
                .scenes
                .get(scene)
                .is_none_or(|scene| scene.editor.source() != state.original)
        {
            self.smart_action = Some(state);
            self.status = format!("quick action: {STALE_SOURCE}");
            self.dirty_frame = true;
            return;
        }
        if self.replace_range_in_scene(scene, range, &plain).is_none() {
            self.smart_action = Some(state);
            self.status = "could not remove the fader - close and reopen quick action".into();
            self.dirty_frame = true;
            return;
        }
        self.status = format!("fader off \u{b7} {plain}");
        self.lint_pending = true;
        self.dirty_frame = true;
    }

    /// The rows the menu is showing, and which one is under the keys.
    pub(super) fn smart_action_view(&self) -> Option<(&SmartAction, Vec<String>)> {
        let state = self.smart_action.as_ref()?;
        let rows = match &state.form {
            Some(form) => {
                let mut rows = form.rows();
                rows.push(match &form.error {
                    Some(problem) => format!("  \u{26a0} {problem}"),
                    None => "  Tab \u{b7} Enter applies \u{b7} Esc closes".to_owned(),
                });
                rows
            }
            None => state
                .menu_rows()
                .into_iter()
                .enumerate()
                .map(|(at, row)| {
                    format!(
                        "{}{row}",
                        if at == state.selected {
                            "\u{25b8} "
                        } else {
                            "  "
                        }
                    )
                })
                .collect(),
        };
        Some((state, rows))
    }

    /// Keys and pastes belong to the open quick action, including its
    /// menu, which ignores pasted text. The mouse can still reach the score.
    pub(super) fn smart_action_event(&mut self, terminal_event: &Event) -> bool {
        match terminal_event {
            Event::Key(key) => self.smart_action_key(key),
            Event::Paste(text) => self.smart_action_paste(text),
            _ => false,
        }
    }

    fn smart_action_paste(&mut self, text: &str) -> bool {
        let Some(state) = self.smart_action.as_mut() else {
            return false;
        };
        if let Some(form) = state.form.as_mut() {
            form.insert(text.trim());
            self.lint_smart_action_soon();
        }
        true
    }

    pub(super) fn smart_action_key(&mut self, key: &crossterm::event::KeyEvent) -> bool {
        if self.smart_action.is_none() || key.kind == KeyEventKind::Release {
            return self.smart_action.is_some();
        }
        let primary = key
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::SUPER);
        let in_form = self
            .smart_action
            .as_ref()
            .is_some_and(|state| state.form.is_some());
        let menu_code = if in_form {
            key.code
        } else {
            self.list_navigation_alias(key)
                .map_or(key.code, |alias| alias.code)
        };
        if in_form {
            match key.code {
                KeyCode::Esc => {
                    self.close_smart_action();
                    return true;
                }
                KeyCode::Enter => {
                    self.apply_slider_form();
                    return true;
                }
                KeyCode::Tab | KeyCode::BackTab | KeyCode::Down | KeyCode::Up => {
                    if let Some(form) = self
                        .smart_action
                        .as_mut()
                        .and_then(|state| state.form.as_mut())
                    {
                        form.navigate(key);
                    }
                    self.dirty_frame = true;
                    return true;
                }
                KeyCode::Char('v' | 'V') if primary => {
                    match self.clipboard.get_text() {
                        Ok(text) => {
                            self.smart_action_paste(&text);
                        }
                        Err(error) => {
                            self.status = format!("could not paste: {error}");
                            self.dirty_frame = true;
                        }
                    }
                    return true;
                }
                _ => {}
            }
            let Some(form) = self
                .smart_action
                .as_mut()
                .and_then(|state| state.form.as_mut())
            else {
                return true;
            };
            if !form.key(key) {
                return true;
            }
            self.lint_smart_action_soon();
            return true;
        }
        match menu_code {
            KeyCode::Esc => self.close_smart_action(),
            KeyCode::Up | KeyCode::Down => {
                if let Some(state) = self.smart_action.as_mut() {
                    let len = state.menu_rows().len().max(1);
                    let delta = if menu_code == KeyCode::Down {
                        1
                    } else {
                        len - 1
                    };
                    state.selected = (state.selected + delta) % len;
                }
                self.dirty_frame = true;
            }
            KeyCode::Enter => self.choose_smart_action(),
            _ => {}
        }
        true
    }

    /// The row under the keys, taken.
    fn choose_smart_action(&mut self) {
        let Some(state) = self.smart_action.as_ref() else {
            return;
        };
        if Some(state.selected) == state.paste_row() {
            self.paste_latest_sample();
            return;
        }
        let unwrap = matches!(state.target, Target::Slider { .. }) && state.selected == 1;
        if unwrap {
            self.unwrap_slider();
        } else {
            self.open_slider_form();
        }
    }

    /// Write the latest recorded sample where [`sample_paste`] lands it
    /// for the caret. The status says `pasted recordings:2`; evaluation
    /// remains a separate action.
    fn paste_latest_sample(&mut self) {
        let Some(state) = self.smart_action.take() else {
            return;
        };
        let Some(sound) = state.sample.as_deref() else {
            self.smart_action = Some(state);
            return;
        };
        let scene = state.scene;
        self.lint_pending = true;
        self.dirty_frame = true;
        let landing = self.scenes.get(scene).and_then(|entry| {
            let caret = entry.editor.primary_selection().head.0;
            sample_paste(&entry.editor.source(), caret, sound)
        });
        let Some((range, text)) = landing else {
            self.status = format!("quick action: nowhere at the caret takes {sound}");
            return;
        };
        if self.replace_range_in_scene(scene, range, &text).is_none() {
            self.smart_action = Some(state);
            self.status = "could not paste the sample - close and reopen quick action".into();
            return;
        }
        self.status = format!("pasted {sound}");
    }

    /// A quick action belongs to the score it opened on. Leaving that
    /// score cancels it before an edit and evaluation can target different scenes.
    pub(super) fn close_smart_action_on_scene_change(&mut self) {
        if self
            .smart_action
            .as_ref()
            .is_some_and(|state| state.scene != self.scenes.current().id)
        {
            self.close_smart_action();
        }
    }

    /// A form that has written a fader leaves it ready to evaluate. A form
    /// that never got as far as writing leaves the score untouched.
    fn close_smart_action(&mut self) {
        if let Some(state) = self.smart_action.take() {
            let wrote = state.form.is_some_and(|form| form.written);
            self.status = if wrote {
                "fader added \u{b7} evaluate when ready".into()
            } else {
                "quick action closed".into()
            };
        }
        self.lint_pending = true;
        self.dirty_frame = true;
    }

    /// Typing clears a complaint; it never makes one.
    ///
    /// A half-typed number is not a mistake, so the form does not judge a
    /// field on each keystroke: an empty field, or a number typed one digit
    /// at a time, would show an error until the last digit. Enter judges
    /// the form. An edit still clears the message, so a stale message does
    /// not outlive the text it was about.
    fn lint_smart_action_soon(&mut self) {
        self.lint_pending = true;
        self.last_edit_at = Instant::now();
        if let Some(form) = self
            .smart_action
            .as_mut()
            .and_then(|state| state.form.as_mut())
        {
            form.error = None;
        }
        self.dirty_frame = true;
    }

    /// One edit of one scene's text, undoable like any other; `None` when
    /// the editor refuses it.
    fn replace_range_in_scene(
        &mut self,
        scene: SceneId,
        range: std::ops::Range<usize>,
        text: &str,
    ) -> Option<()> {
        use crate::editor::{ByteOffset, Selection};
        let moment = self.moment();
        let entry = self.scenes.get_mut(scene)?;
        let before = entry.editor.revision();
        let selection = Selection {
            anchor: ByteOffset(range.start),
            head: ByteOffset(range.end),
            goal_column: None,
        };
        entry.editor.set_selection(selection).ok()?;
        entry
            .editor
            .dispatch(
                Command::InsertText(text.to_owned()),
                moment,
                &mut *self.clipboard,
            )
            .ok()?;
        // No evaluation follows, so this is the only place that marks the
        // scene unsaved. Quit and set switch save from that mark.
        self.after_edit(scene, before, false);
        Some(())
    }
}

impl SmartAction {
    pub(super) fn anchor(&self) -> (u16, u16) {
        self.anchor
    }

    /// Where the field's caret is drawn, while the form is up.
    pub(super) fn form_caret(&self, area: Rect) -> Option<(u16, u16)> {
        let form = self.form.as_ref()?;
        let typed = UnicodeWidthStr::width(&form.text()[..form.cursor]) as u16;
        let column = field_value_x(area) + typed;
        Some((column.min(area.right() - 2), area.y + 1 + form.field as u16))
    }
}

/// The popup, drawn over the score at the caret.
pub(super) fn render(
    frame: &mut ratatui::Frame<'_>,
    theme: &Theme,
    anchor: (u16, u16),
    rows: &[String],
    caret: Option<(u16, u16)>,
) {
    let area = geometry(frame.area(), anchor, rows.len() as u16);
    super::super::graphics::cover_images(area);
    frame.render_widget(Clear, area);
    frame.render_widget(
        Block::default()
            .borders(Borders::ALL)
            .title(" \u{2318} quick action ")
            .style(Style::default().bg(theme.overlay).fg(theme.foreground)),
        area,
    );
    for (at, row) in rows.iter().enumerate() {
        let y = area.y + 1 + at as u16;
        if y + 1 >= area.bottom() {
            break;
        }
        let chosen = row.starts_with('\u{25b8}');
        let warning = row.contains('\u{26a0}');
        let style = if chosen {
            Style::default()
                .fg(theme.selection_text)
                .bg(theme.selection)
        } else if warning {
            Style::default().fg(theme.error)
        } else {
            Style::default().fg(theme.muted)
        };
        frame.render_widget(
            Paragraph::new(row.clone()).style(style),
            Rect::new(area.x + 1, y, area.width.saturating_sub(2), 1),
        );
    }
    if let Some((x, y)) = caret {
        frame.set_cursor_position((x, y));
    }
}

#[cfg(test)]
mod tests {
    mod smart_action {
        //! Tests for quick action.

        use crate::app::tests::support::*;
        use crate::app::*;
        use crate::editor::{MemoryClipboard, Selection};
        use crate::keybinds::KeyCombo;
        use std::cell::RefCell;

        fn open_action_on(app: &mut App, needle: &str) {
            let at = app.editor().source().find(needle).expect("action target");
            app.editor_mut()
                .set_selection(Selection::caret(ByteOffset(at)))
                .expect("caret on the target");
            precision_key(app, KeyCode::Char('j'), KeyModifiers::CONTROL);
            assert!(app.smart_action_view().is_some(), "the menu opens");
        }

        #[test]
        fn bracketed_and_legacy_paste_edit_the_form_before_one_undoable_write() {
            for legacy in [false, true] {
                let directory = tempfile::tempdir().expect("temp dir");
                let (mut app, scene) = plain_app(directory.path(), "$: s(\"bd\").lpf(2300)\n");
                app.audible_scene = Some(scene);
                let before = app.editor().source();
                let revision = app.editor().revision();
                open_action_on(&mut app, "2300");
                precision_key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
                assert!(!app.editor_accepts_raw_paste());
                assert!(app.accepts_raw_paste());

                let events = if legacy {
                    let mut queued = [KeyCode::Char('5'), KeyCode::Char('0'), KeyCode::Enter]
                        .into_iter()
                        .map(|code| Event::Key(KeyEvent::new(code, KeyModifiers::NONE)))
                        .collect::<VecDeque<_>>();
                    let first = queued.pop_front().expect("first key");
                    let queued = RefCell::new(queued);
                    let events = read_raw_paste_batch(
                        first,
                        |_| Ok(!queued.borrow().is_empty()),
                        || Ok(queued.borrow_mut().pop_front().expect("polled key")),
                    )
                    .expect("legacy paste batch");
                    assert_eq!(events, vec![Event::Paste("50\n".into())]);
                    events
                } else {
                    vec![Event::Paste("50".into())]
                };
                for event in events {
                    app.handle_terminal_event(event).expect("paste into form");
                }
                let (_, rows) = app.smart_action_view().expect("form stays open");
                assert_eq!(rows[0], "▸ value 50", "{rows:?}");
                assert_eq!(app.editor().source(), before);
                assert_eq!(app.editor().revision(), revision);
                assert!(app.pending_evaluation.is_none() && app.inflight.is_empty());

                precision_key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
                assert_eq!(
                    app.editor().source(),
                    before.replacen("lpf(2300)", "lpf(slider(50, 0, 5000, 1))", 1)
                );
                assert!(app.smart_action_view().is_none());
                app.dispatch_editor(Command::Undo).expect("undo the fader");
                assert_eq!(
                    app.editor().source(),
                    before,
                    "one undo restores the number"
                );
            }
        }

        #[test]
        fn menu_pastes_and_invalid_form_pastes_never_reach_the_score() {
            let directory = tempfile::tempdir().expect("temp dir");
            let (mut app, _) = plain_app(directory.path(), "$: s(\"bd\").lpf(2300)\n");
            let before = app.editor().source();
            let revision = app.editor().revision();
            open_action_on(&mut app, "2300");
            assert!(!app.editor_accepts_raw_paste());
            assert!(app.accepts_raw_paste());
            let menu = app.smart_action_view().expect("menu").1;
            app.handle_terminal_event(Event::Paste("50".into()))
                .expect("paste at menu");
            assert_eq!(app.smart_action_view().expect("menu stays open").1, menu);

            precision_key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
            for text in ["s(\"bd\")\n", "/tmp/samples.wav", "50\n60"] {
                app.handle_terminal_event(Event::Paste(text.into()))
                    .expect("invalid numeric paste");
                let (_, rows) = app.smart_action_view().expect("form stays open");
                assert_eq!(rows[0], "▸ value 2300", "{rows:?}");
            }
            precision_key(&mut app, KeyCode::Tab, KeyModifiers::NONE);
            app.handle_terminal_event(Event::Paste(" \t-1.5e2\n ".into()))
                .expect("numeric paste with surrounding whitespace");
            let (_, rows) = app.smart_action_view().expect("form stays open");
            assert!(rows[1].ends_with("-1.5e2"), "{rows:?}");
            precision_key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
            assert!(app.smart_action_view().is_none());
            assert_eq!(app.editor().source(), before);
            assert_eq!(app.editor().revision(), revision);
        }

        #[test]
        fn clipboard_paste_chords_belong_to_the_form_over_a_focused_panel() {
            for (code, modifiers) in [
                (KeyCode::Char('v'), KeyModifiers::CONTROL),
                (KeyCode::Char('v'), KeyModifiers::SUPER),
                (KeyCode::F(9), KeyModifiers::CONTROL),
                (KeyCode::F(7), KeyModifiers::NONE),
            ] {
                let directory = tempfile::tempdir().expect("temp dir");
                let (mut app, scene) = plain_app(directory.path(), "$: s(\"bd\").lpf(2300)\n");
                app.audible_scene = Some(scene);
                let before = app.editor().source();
                open_action_on(&mut app, "2300");
                precision_key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
                app.reference_panel = Some(ReferencePanel::browse(&app.reference));
                app.focus = Focus::Panel(PanelKind::Reference);
                app.clipboard.set_text("50".into()).expect("seed clipboard");
                if code == KeyCode::F(9) {
                    app.keybinds
                        .learn(BindAction::Paste, KeyCombo::parse("ctrl+f9"));
                } else if code == KeyCode::F(7) {
                    app.keybinds.learn(BindAction::Paste, KeyCombo::parse("f7"));
                }

                precision_key(&mut app, code, modifiers);
                let (_, rows) = app.smart_action_view().expect("form stays open");
                assert_eq!(rows[0], "▸ value 50", "{code:?}: {rows:?}");
                assert_eq!(app.editor().source(), before);
                assert_eq!(app.reference_panel.as_ref().expect("panel").query, "");
                precision_key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
                assert_eq!(
                    app.editor().source(),
                    before.replacen("lpf(2300)", "lpf(slider(50, 0, 5000, 1))", 1)
                );
                assert!(app.smart_action_view().is_none());
            }
        }

        #[test]
        fn alt_up_inside_the_form_cannot_adjust_the_score_fader() {
            let directory = tempfile::tempdir().expect("temp dir");
            let (mut app, _, _) = slider_app(directory.path());
            let before = app.editor().source();
            let revision = app.editor().revision();
            open_action_on(&mut app, "0.5");
            precision_key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
            precision_key(&mut app, KeyCode::Up, KeyModifiers::ALT);
            let (_, rows) = app.smart_action_view().expect("form stays open");
            assert!(
                rows[3].starts_with("▸ step"),
                "Up moves to the step field: {rows:?}"
            );
            assert_eq!(app.editor().source(), before);
            assert_eq!(app.editor().revision(), revision);
            assert!(app.pending_evaluation.is_none() && app.inflight.is_empty());
        }

        #[test]
        fn changed_source_refuses_both_form_writes_and_unwraps() {
            for unwrap in [false, true] {
                let directory = tempfile::tempdir().expect("temp dir");
                let original = if unwrap {
                    "$: s(\"bd\").gain(slider(2300, 0, 5000, 1))\n"
                } else {
                    "$: s(\"bd\").lpf(2300)\n"
                };
                let (mut app, _) = plain_app(directory.path(), original);
                open_action_on(&mut app, "2300");
                precision_key(
                    &mut app,
                    if unwrap {
                        KeyCode::Down
                    } else {
                        KeyCode::Enter
                    },
                    KeyModifiers::NONE,
                );
                let at = app.editor().source().find("2300").expect("target");
                app.editor_mut()
                    .set_selection(Selection::caret(ByteOffset(at)))
                    .expect("external edit at target");
                app.editor_mut()
                    .dispatch(
                        Command::InsertText("50".into()),
                        HistoryMoment(100),
                        &mut MemoryClipboard::default(),
                    )
                    .expect("source changes while modal is open");
                let changed = app.editor().source();
                let revision = app.editor().revision();
                precision_key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
                assert_eq!(app.editor().source(), changed);
                assert_eq!(app.editor().revision(), revision);
                assert!(app.smart_action_view().is_some());
                assert!(app.pending_evaluation.is_none() && app.inflight.is_empty());
                let (_, rows) = app.smart_action_view().expect("modal stays open");
                assert!(
                    app.status.contains("the score or scene changed")
                        || rows
                            .iter()
                            .any(|row| row.contains("the score or scene changed")),
                    "{}: {rows:?}",
                    app.status
                );
            }
        }

        #[test]
        fn changing_to_an_identical_scene_cancels_form_writes_and_unwraps() {
            for unwrap in [false, true] {
                let directory = tempfile::tempdir().expect("temp dir");
                let (mut app, original, _) = slider_app(directory.path());
                let before = app.editor().source();
                let other = app.scenes.create(&before).expect("second identical scene");
                let original_index = app.scenes.index_of(original).expect("original scene");
                assert!(app.select_scene(original_index));
                open_action_on(&mut app, "0.5");
                precision_key(
                    &mut app,
                    if unwrap {
                        KeyCode::Down
                    } else {
                        KeyCode::Enter
                    },
                    KeyModifiers::NONE,
                );
                let other_index = app.scenes.index_of(other).expect("other scene");
                assert!(app.select_scene(other_index));
                assert_eq!(
                    app.scenes.get(original).expect("original").editor.source(),
                    before
                );
                assert_eq!(
                    app.scenes.get(other).expect("other").editor.source(),
                    before
                );
                assert!(app.pending_evaluation.is_none() && app.inflight.is_empty());
                assert!(
                    app.smart_action_view().is_none(),
                    "changing scenes closes the modal"
                );
            }
        }

        #[test]
        fn changing_scenes_closes_the_numeric_smart_action() {
            for key in [KeyCode::F(3), KeyCode::F(7), KeyCode::F(10)] {
                let directory = tempfile::tempdir().expect("temp dir");
                let (mut app, scene) = plain_app(directory.path(), "$: s(\"bd*4\").gain(0.4)\n");
                app.audible_scene = Some(scene);
                if key != KeyCode::F(3) {
                    app.new_scene(true);
                    app.select_scene(app.scenes.index_of(scene).expect("original scene"));
                }
                if key == KeyCode::F(10) {
                    app.toggle_split();
                    app.focus_pane(0);
                }
                let source = app.editor().source();
                let at = source.find("0.4").expect("the number");
                app.editor_mut()
                    .set_selection(Selection::caret(ByteOffset(at + 1)))
                    .expect("caret on the number");
                precision_key(&mut app, KeyCode::Char('j'), KeyModifiers::CONTROL);
                precision_key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
                assert!(app.smart_action_view().is_some());
                app.select_scene(app.scenes.current_index());
                assert!(
                    app.smart_action_view().is_some(),
                    "staying on the same scene"
                );

                precision_key(&mut app, key, KeyModifiers::NONE);
                assert_ne!(app.scenes.current().id, scene, "{key:?}");
                assert!(app.smart_action_view().is_none(), "{key:?}");
                precision_key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
                assert_eq!(app.scenes.get(scene).unwrap().editor.source(), source);
                assert!(!app.editor().source().contains("slider("));
                assert_eq!(app.audible_scene, Some(scene));
                assert!(app.pending_evaluation.is_none());
            }
        }

        #[test]
        fn learned_scene_actions_close_the_numeric_smart_action() {
            for action in [
                BindAction::DuplicateScene,
                BindAction::NextScene,
                BindAction::HopPane,
            ] {
                let directory = tempfile::tempdir().expect("temp dir");
                let (mut app, scene) = plain_app(directory.path(), "$: s(\"bd\").gain(0.4)\n");
                if action != BindAction::DuplicateScene {
                    app.new_scene(true);
                    app.select_scene(app.scenes.index_of(scene).expect("original scene"));
                }
                if action == BindAction::HopPane {
                    app.toggle_split();
                    app.focus_pane(0);
                }
                let source = app.editor().source();
                open_action_on(&mut app, "0.4");
                precision_key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
                app.keybinds.learn(action, KeyCombo::parse("ctrl+f12"));
                precision_key(&mut app, KeyCode::F(12), KeyModifiers::CONTROL);
                assert_ne!(app.scenes.current().id, scene, "{action:?}");
                assert!(app.smart_action_view().is_none(), "{action:?}");
                assert_eq!(
                    app.scenes.get(scene).expect("original").editor.source(),
                    source
                );
                assert!(app.pending_evaluation.is_none() && app.inflight.is_empty());
            }
        }

        /// `^J` on a plain number turns it into a fader without anyone typing
        /// `slider(`. The menu opens at the caret with its one row already
        /// chosen, Enter opens the form, Tab walks the fields, and Enter writes
        /// and closes the form without evaluating. A form that could not write
        /// stays open.
        #[test]
        fn ctrl_j_turns_a_number_into_a_fader() {
            let directory = tempfile::tempdir().expect("temp dir");
            let (mut app, scene) = plain_app(directory.path(), "$: s(\"bd*4\").lpf(800)\n");
            app.audible_scene = Some(scene);
            // The caret on the 800.
            let at = app.editor().source().find("800").expect("the number");
            app.editor_mut()
                .set_selection(Selection::caret(ByteOffset(at + 1)))
                .expect("caret on the number");
            precision_key(&mut app, KeyCode::Char('j'), KeyModifiers::CONTROL);
            let (_, rows) = app.smart_action_view().expect("the menu is open");
            assert!(
                rows[0].starts_with('▸'),
                "the first row is chosen: {rows:?}"
            );
            assert!(rows[0].contains("slider"), "{rows:?}");

            // Enter opens the form, filled in from the number that was there.
            precision_key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
            let (_, rows) = app.smart_action_view().expect("the form is open");
            assert!(
                rows[0].contains("value") && rows[0].contains("800"),
                "{rows:?}"
            );
            assert!(rows[1].contains("min"), "{rows:?}");
            assert!(rows[2].contains("max"), "{rows:?}");
            assert!(rows[3].contains("step"), "{rows:?}");

            // Tab to min and type one: a fresh field is selected whole, so the
            // digits replace it rather than joining it.
            precision_key(&mut app, KeyCode::Tab, KeyModifiers::NONE);
            for digit in "100".chars() {
                precision_key(&mut app, KeyCode::Char(digit), KeyModifiers::NONE);
            }
            precision_key(&mut app, KeyCode::Tab, KeyModifiers::NONE);
            for digit in "4000".chars() {
                precision_key(&mut app, KeyCode::Char(digit), KeyModifiers::NONE);
            }
            precision_key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
            let source = app.editor().source();
            assert!(
                source.contains("slider(800, 100, 4000,"),
                "the fader is in the score: {source}"
            );
            // The form closes after the write. An open form would let a second
            // Enter rewrite the value during a drag.
            assert!(
                app.smart_action_view().is_none(),
                "a form that wrote its fader closes"
            );
            assert_eq!(source.matches("slider(").count(), 1, "one call, not nested");
            assert!(app.status.contains("fader"), "{}", app.status);
            assert!(
                app.pending_evaluation.is_none() && app.inflight.is_empty(),
                "applying a slider leaves evaluation to the player"
            );

            // And ^J on it again edits the same one rather than wrapping it.
            let at = source.find("slider(").expect("the fader");
            app.editor_mut()
                .set_selection(Selection::caret(ByteOffset(at + 8)))
                .expect("caret inside the call");
            precision_key(&mut app, KeyCode::Char('j'), KeyModifiers::CONTROL);
            let (_, rows) = app.smart_action_view().expect("open on the fader");
            assert!(rows[0].contains("edit this fader"), "{rows:?}");
        }

        /// Backspace on a fresh field takes one character off its end, the
        /// way the caret the form draws says it will. The selection a fresh
        /// field opens with exists so a typed character replaces the guess;
        /// one erase key must not erase the whole default.
        #[test]
        fn backspace_trims_a_fresh_field_instead_of_erasing_it() {
            let directory = tempfile::tempdir().expect("temp dir");
            let (mut app, _) = plain_app(directory.path(), "$: s(\"bd\").lpf(800)\n");
            open_action_on(&mut app, "800");
            precision_key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
            precision_key(&mut app, KeyCode::Backspace, KeyModifiers::NONE);
            let (_, rows) = app.smart_action_view().expect("the form stays open");
            assert_eq!(rows[0], "\u{25b8} value 80", "{rows:?}");
            // The trim took the selection with it: a typed digit joins the
            // number it trimmed.
            precision_key(&mut app, KeyCode::Char('5'), KeyModifiers::NONE);
            let (_, rows) = app.smart_action_view().expect("the form stays open");
            assert_eq!(rows[0], "\u{25b8} value 805", "{rows:?}");
            // A field Tab lands on is trimmed the same, not emptied.
            precision_key(&mut app, KeyCode::Tab, KeyModifiers::NONE);
            precision_key(&mut app, KeyCode::Tab, KeyModifiers::NONE);
            precision_key(&mut app, KeyCode::Backspace, KeyModifiers::NONE);
            let (_, rows) = app.smart_action_view().expect("the form stays open");
            assert_eq!(rows[2], "\u{25b8} max   200", "{rows:?}");
        }

        #[test]
        fn a_written_fader_marks_the_scene_unsaved() {
            let directory = tempfile::tempdir().expect("temp dir");
            let (mut app, _) = plain_app(directory.path(), "$: s(\"bd*4\").lpf(800)\n");
            app.scenes.current_mut().dirty = false;
            open_action_on(&mut app, "800");
            precision_key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
            precision_key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
            assert!(app.editor().source().contains("slider(800,"));
            assert!(app.scenes.current().dirty, "quit saves the fader");
        }

        /// A min above the value pulls the value up with it: a range that does
        /// not contain its own value is a fader that starts off its rail. The
        /// pull is said on the form before anything is written, and the next
        /// Enter writes the value the form now shows.
        #[test]
        fn raising_the_min_carries_the_value_with_it() {
            let directory = tempfile::tempdir().expect("temp dir");
            let (mut app, scene) = plain_app(directory.path(), "$: s(\"bd\").gain(0.2)\n");
            app.audible_scene = Some(scene);
            let before = app.editor().source();
            let at = before.find("0.2").expect("the number");
            app.editor_mut()
                .set_selection(Selection::caret(ByteOffset(at)))
                .expect("caret on the number");
            precision_key(&mut app, KeyCode::Char('j'), KeyModifiers::CONTROL);
            precision_key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
            // min := 0.5, above the value of 0.2.
            precision_key(&mut app, KeyCode::Tab, KeyModifiers::NONE);
            for digit in "0.5".chars() {
                precision_key(&mut app, KeyCode::Char(digit), KeyModifiers::NONE);
            }
            precision_key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
            let (_, rows) = app.smart_action_view().expect("the pull keeps the form");
            assert!(rows[0].contains("value 0.5"), "{rows:?}");
            assert_eq!(
                rows.last().map(String::as_str),
                Some("  \u{26a0} value pulled into range \u{b7} Enter writes"),
                "{rows:?}"
            );
            assert_eq!(
                app.editor().source(),
                before,
                "nothing is written until the pull has been read"
            );
            precision_key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
            let source = app.editor().source();
            assert!(
                source.contains("slider(0.5, 0.5, 1,"),
                "the value came up to the floor: {source}"
            );
            // A written fader closes the form, so the refusal is a fresh gesture.
            assert!(app.smart_action_view().is_none(), "written and away");
            // And a max at or below the min is refused, with a reason, and the
            // form stays up - a form that could not be used is the one case that
            // wants a second try.
            let at = source.find("0.5, 0.5").expect("the fader");
            app.editor_mut()
                .set_selection(Selection::caret(ByteOffset(at)))
                .expect("caret on the fader");
            precision_key(&mut app, KeyCode::Char('j'), KeyModifiers::CONTROL);
            precision_key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
            precision_key(&mut app, KeyCode::Tab, KeyModifiers::NONE);
            precision_key(&mut app, KeyCode::Tab, KeyModifiers::NONE);
            for digit in "0.1".chars() {
                precision_key(&mut app, KeyCode::Char(digit), KeyModifiers::NONE);
            }
            precision_key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
            let (_, rows) = app.smart_action_view().expect("still open");
            assert!(
                rows.last()
                    .is_some_and(|row| row.contains("min has to be below max")),
                "{rows:?}"
            );
            assert_eq!(app.editor().source(), source, "and nothing was written");
        }

        /// A value typed past its ceiling is pulled onto the rail and the pull
        /// stays on screen. It used to be said as the write closed the form, in a
        /// status line the evaluate that write started answered over a frame
        /// later; now nothing is written, so nothing is evaluated, and the warning
        /// is still on the form once every queue has drained.
        #[test]
        fn a_pulled_value_is_said_on_the_form_and_outlives_the_engine() {
            let directory = tempfile::tempdir().expect("temp dir");
            let (mut app, scene) = plain_app(directory.path(), "$: s(\"bd\").lpf(800)\n");
            app.audible_scene = Some(scene);
            let before = app.editor().source();
            let at = before.find("800").expect("the number");
            app.editor_mut()
                .set_selection(Selection::caret(ByteOffset(at)))
                .expect("caret on the number");
            precision_key(&mut app, KeyCode::Char('j'), KeyModifiers::CONTROL);
            precision_key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
            // The value field opens selected: typing replaces the 800. The guessed
            // range for 800 is 0-2000.
            for digit in "30000".chars() {
                precision_key(&mut app, KeyCode::Char(digit), KeyModifiers::NONE);
            }
            precision_key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
            assert!(
                app.pending_evaluation.is_none() && app.inflight.is_empty(),
                "a pull writes nothing, so it evaluates nothing"
            );
            settle(&mut app);
            let (_, rows) = app.smart_action_view().expect("the form is still up");
            assert_eq!(rows[0], "\u{25b8} value 2000", "{rows:?}");
            assert_eq!(
                rows.last().map(String::as_str),
                Some("  \u{26a0} value pulled into range \u{b7} Enter writes"),
                "{rows:?}"
            );
            assert_eq!(app.editor().source(), before, "nothing was written");

            // The field was re-selected with its shorter number: typing replaces
            // it, and the edit takes the warning away with it.
            for digit in "1500".chars() {
                precision_key(&mut app, KeyCode::Char(digit), KeyModifiers::NONE);
            }
            let (_, rows) = app.smart_action_view().expect("the form is still up");
            assert_eq!(rows[0], "\u{25b8} value 1500", "{rows:?}");
            assert!(!rows.iter().any(|row| row.contains('\u{26a0}')), "{rows:?}");

            // Enter on a value inside its range writes it at once.
            precision_key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
            assert_eq!(
                app.editor().source(),
                before.replacen("lpf(800)", "lpf(slider(1500, 0, 2000, 1))", 1)
            );
            assert!(app.smart_action_view().is_none(), "written and away");
        }

        /// A form that stays open keeps its caret inside its text, because the
        /// next frame slices the text at the caret. The step keeps its typed
        /// spelling, such as "0.50", while the form can stay open. A form whose
        /// write would be refused shows the refusal first and does not pull a
        /// value into range.
        #[test]
        fn a_form_kept_open_keeps_its_caret_inside_its_text() {
            use crate::app::smart_action;

            let directory = tempfile::tempdir().expect("temp dir");
            let (mut app, scene) = plain_app(
                directory.path(),
                "$: s(\"bd\").gain(slider(0.5, 0, 1, 0.01))\n",
            );
            app.audible_scene = Some(scene);
            let at = app.editor().source().find("0.5").expect("the value");
            app.editor_mut()
                .set_selection(Selection::caret(ByteOffset(at + 1)))
                .expect("caret on the fader");
            precision_key(&mut app, KeyCode::Char('j'), KeyModifiers::CONTROL);
            precision_key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
            // A value above the fader's ceiling: the form would pull it in, but
            // only a write it can make is worth promising with "Enter writes".
            precision_key(&mut app, KeyCode::Char('5'), KeyModifiers::NONE);
            // Onto step, and a spelling of it the form would tidy.
            for _ in 0..3 {
                precision_key(&mut app, KeyCode::Tab, KeyModifiers::NONE);
            }
            for digit in "0.50".chars() {
                precision_key(&mut app, KeyCode::Char(digit), KeyModifiers::NONE);
            }
            // The score changes under the form: the write is refused and the form
            // stays up, with the step field under the keys.
            app.editor_mut()
                .set_selection(Selection::caret(ByteOffset(0)))
                .expect("caret at the top");
            app.editor_mut()
                .dispatch(
                    Command::InsertText("// moved\n".into()),
                    HistoryMoment(0),
                    &mut MemoryClipboard::default(),
                )
                .expect("the text moves");
            precision_key(&mut app, KeyCode::Enter, KeyModifiers::NONE);

            let (state, rows) = app.smart_action_view().expect("the form stays up");
            assert!(
                rows.last()
                    .is_some_and(|row| row.contains("the score or scene changed")),
                "the refusal the next Enter would meet, not the pull: {rows:?}"
            );
            assert!(rows[0].ends_with(" 5"), "the value as typed: {rows:?}");
            assert!(rows[3].ends_with("0.50"), "the step as typed: {rows:?}");
            let area = smart_action::geometry(app.frame, state.anchor(), rows.len() as u16);
            let (x, y) = state.form_caret(area).expect("a caret");
            assert_eq!(y, area.y + 1 + 3, "on the step row");
            assert!(x < area.right(), "inside the popup");
        }

        /// `^J` on a number that is already a fader offers to edit it or to take
        /// the fader off; inside a pattern string it offers nothing, because
        /// `slider()` is JavaScript and mini-notation is not.
        #[test]
        fn ctrl_j_reads_what_the_caret_is_actually_on() {
            let directory = tempfile::tempdir().expect("temp dir");
            let (mut app, scene, call) = slider_app(directory.path());
            app.audible_scene = Some(scene);
            app.editor_mut()
                .set_selection(Selection::caret(ByteOffset(call + 8)))
                .expect("caret inside the slider call");
            precision_key(&mut app, KeyCode::Char('j'), KeyModifiers::CONTROL);
            let (_, rows) = app.smart_action_view().expect("open");
            assert_eq!(rows.len(), 2, "{rows:?}");
            assert!(rows[0].contains("edit this fader"), "{rows:?}");
            assert!(rows[1].contains("take the fader off"), "{rows:?}");
            // Down then Enter takes it off, leaving the number it was holding.
            precision_key(&mut app, KeyCode::Down, KeyModifiers::NONE);
            precision_key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
            let source = app.editor().source();
            assert!(!source.contains("slider("), "{source}");
            assert!(source.contains("gain(0.5)"), "{source}");
            assert!(
                app.pending_evaluation.is_none() && app.inflight.is_empty(),
                "removing a slider leaves evaluation to the player"
            );

            // Inside a pattern string there is nothing honest to offer.
            let at = source.find("bd").expect("the pattern");
            app.editor_mut()
                .set_selection(Selection::caret(ByteOffset(at)))
                .expect("caret in the string");
            precision_key(&mut app, KeyCode::Char('j'), KeyModifiers::CONTROL);
            assert!(app.smart_action_view().is_none());
            assert!(app.status.contains("inside a pattern"), "{}", app.status);

            // The refusal says which of the two it was. It comes from the
            // checker's own scan of the score, not from a second reading of
            // the quotes, so a `//` inside a pattern is still a pattern.
            let refusal_at = |app: &mut App, source: &str, needle: &str| {
                app.editor_mut()
                    .dispatch(
                        Command::SelectAll,
                        HistoryMoment(0),
                        &mut MemoryClipboard::default(),
                    )
                    .expect("select the score");
                app.editor_mut()
                    .dispatch(
                        Command::PasteText(source.into()),
                        HistoryMoment(0),
                        &mut MemoryClipboard::default(),
                    )
                    .expect("replace the score");
                let at = app.editor().source().find(needle).expect("the needle");
                app.editor_mut()
                    .set_selection(Selection::caret(ByteOffset(at)))
                    .expect("caret on the needle");
                precision_key(app, KeyCode::Char('j'), KeyModifiers::CONTROL);
                assert!(app.smart_action_view().is_none(), "{}", app.status);
                app.status.clone()
            };

            let comment = refusal_at(&mut app, "// gain 800 one day\n$: s(\"bd\")\n", "800");
            assert!(comment.contains("in a comment"), "{comment}");

            let pattern = refusal_at(&mut app, "$: s(\"bd//800\").gain(0.5)\n", "800");
            assert!(
                pattern.contains("inside a pattern"),
                "a `//` inside a string is still the string: {pattern}"
            );

            // And the same both ways round: a quote inside a comment is a comment.
            let quoted = refusal_at(&mut app, "// a \"pattern\" of 800\n$: s(\"bd\")\n", "800");
            assert!(quoted.contains("in a comment"), "{quoted}");
        }

        /// The Edit menu's Quick action row is grey where the action has nothing
        /// to offer: inside a pattern string or a comment. It is live on a number
        /// in code and inside a `slider(` call.
        #[test]
        fn the_smart_action_row_greys_out_where_there_is_nothing_to_offer() {
            use crate::menu::{MenuAction, MenuItemKind};

            let directory = tempfile::tempdir().expect("temp dir");
            let (mut app, _, call) = slider_app(directory.path());
            let live = |app: &App| {
                app.menus()
                    .into_iter()
                    .flat_map(|menu| menu.items)
                    .find(|item| item.kind == MenuItemKind::Action(MenuAction::SmartAction))
                    .expect("the Quick action row")
                    .enabled
            };
            let caret = |app: &mut App, at: usize| {
                app.editor_mut()
                    .set_selection(Selection::caret(ByteOffset(at)))
                    .expect("move the caret");
            };

            // `$: s("bd").gain(slider(0.5, 0, 1, 0.0001))` - on the fader's own
            // value, and on the call around it where there is no digit at all.
            let source = app.editor().source();
            caret(&mut app, source.find("0.5").expect("the value"));
            assert!(live(&app), "a number in code");
            caret(&mut app, call + 2);
            assert!(live(&app), "inside the slider call, off any digit");

            // The pattern string is text: a fader cannot go there, and the row
            // says so by being grey rather than by opening and refusing.
            caret(&mut app, source.find("bd").expect("the pattern"));
            assert!(!live(&app), "inside a pattern");

            // And a score with nothing to offer anywhere.
            app.editor_mut()
                .dispatch(
                    Command::SelectAll,
                    HistoryMoment(0),
                    &mut MemoryClipboard::default(),
                )
                .expect("select the score");
            app.editor_mut()
                .dispatch(
                    Command::PasteText("// gain 800 one day\n$: s(\"bd\")\n".into()),
                    HistoryMoment(0),
                    &mut MemoryClipboard::default(),
                )
                .expect("replace the score");
            let source = app.editor().source();
            caret(&mut app, source.find("800").expect("the number"));
            assert!(!live(&app), "a number in a comment");
            caret(&mut app, source.find("gain").expect("the word"));
            assert!(!live(&app), "a word in a comment");
        }

        /// `^J` must not corrupt a score it cannot read. A number beside
        /// multi-byte text, a caret at the very end, an unterminated string, a
        /// bare minus: none of these may panic or write a call that does not
        /// parse.
        #[test]
        fn ctrl_j_survives_the_awkward_places_a_caret_can_be() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut offered = 0usize;
            for source in [
                "$: s(\"bd\").gain(0.5) // ♥ réverbère 日本語\n",
                "$: note(\"c\").lpf(-200)\n",
                "$: s(\"bd\").gain(\n",
                "// ♥♥♥ 800 ♥♥♥\n",
                "$: s(\"bd\").speed(1e3)\n",
                "$: s(\"bd:3\")\n",
                "$: s(\"bd\").n(0x1f)\n",
                "$: osc3.gain(0.5)\n",
                "$: s(\"bd\").gain(slider(0.5, 0, 1))\n",
                "$: s(\"bd\").gain(slider())\n",
                "$: s(\"bd\").gain(slider( /* soon */ ))\n",
                "$: s(\"bd\").gain(slider( // soon\n))\n",
                "$: s(\"bd\") // gain(0.5)\n",
                "$: s(\"bd\") /* 800 */ .lpf(400)\n",
                "$: s(`bd\n800\n`).lpf(400)\n",
                "$: note(\"c\").lpf(1_000)\n",
                "\n",
            ] {
                let (mut app, _scene) = plain_app(directory.path(), source);
                let text = app.editor().source();
                for caret in 0..=text.len() {
                    if !text.is_char_boundary(caret) {
                        continue;
                    }
                    // Each caret starts from the same score: without this the
                    // writes pile up and the test measures its own leftovers.
                    reseed(&mut app, &text);
                    app.editor_mut()
                        .set_selection(Selection::caret(ByteOffset(caret)))
                        .expect("caret");
                    app.smart_action = None;
                    precision_key(&mut app, KeyCode::Char('j'), KeyModifiers::CONTROL);
                    if app.smart_action_view().is_none() {
                        continue;
                    }
                    offered += 1;
                    // Take it all the way through the form and out again.
                    precision_key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
                    precision_key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
                    precision_key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
                    let written = app.editor().source();
                    // Whatever it wrote, the score is still a score: every slider
                    // call it produced is one the transpiler reads back.
                    for (at, _) in written.match_indices("slider(") {
                        let call = &written[at..];
                        let close = call.find(')').unwrap_or(0);
                        let inside = &call["slider(".len()..close];
                        let parts: Vec<&str> = inside.split(',').map(str::trim).collect();
                        assert_eq!(
                            parts.len(),
                            4,
                            "at caret {caret} in {source:?}: {written:?}"
                        );
                        for part in parts {
                            assert!(
                                part.parse::<f64>().is_ok(),
                                "at caret {caret} in {source:?}: {part:?} is not a number in {written:?}"
                            );
                        }
                    }
                    assert_eq!(
                        rustel_runtime::ui_events::literal_sliders(&written).len(),
                        written.matches("slider(").count(),
                        "at caret {caret} in {source:?}: a call the transpiler cannot read: {written:?}"
                    );
                    // And the score still parses. Counting numbers inside a
                    // `slider(…)` says nothing about the text around it -
                    // `.speed(1eslider(3, 0, 10, 0.1))` has four good numbers in
                    // it and is a syntax error.
                    let checked = rustel_runtime::lint::lint(&written, false, None);
                    assert!(
                        !checked
                            .iter()
                            .any(|problem| problem.level == rustel_runtime::lint::Level::Syntax),
                        "at caret {caret} in {source:?}: {written:?} does not parse: {:?}",
                        checked
                            .iter()
                            .filter(|problem| problem.level == rustel_runtime::lint::Level::Syntax)
                            .collect::<Vec<_>>()
                    );
                }
            }
            // And the sweep is not vacuous: it really did open the form, over and
            // over, rather than being refused everywhere and proving nothing.
            assert!(offered > 20, "only {offered} carets offered a fader");
        }

        /// The caret in the quick action's form sits on the text it is editing.
        ///
        /// It was drawn one cell to its left, because the row's prefix was
        /// counted in two places and the two disagreed by the inset the renderer
        /// adds. A caret one cell off its text is a field whose end you cannot
        /// reach: Right stopped before the last digit and Home landed in the
        /// label.
        #[test]
        fn the_smart_action_caret_sits_on_the_text_it_edits() {
            use crate::app::smart_action;

            let directory = tempfile::tempdir().expect("temp dir");
            let (mut app, scene) = plain_app(directory.path(), "$: s(\"bd\").lpf(5000)\n");
            app.audible_scene = Some(scene);
            let at = app.editor().source().find("5000").expect("the number");
            app.editor_mut()
                .set_selection(Selection::caret(ByteOffset(at + 1)))
                .expect("caret on the number");
            precision_key(&mut app, KeyCode::Char('j'), KeyModifiers::CONTROL);
            precision_key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
            // Onto min, and type a value of a known width.
            precision_key(&mut app, KeyCode::Tab, KeyModifiers::NONE);
            for digit in "1122".chars() {
                precision_key(&mut app, KeyCode::Char(digit), KeyModifiers::NONE);
            }

            // The expected caret column comes from the text of the row that the
            // renderer draws, not from a second copy of the prefix arithmetic.
            let caret_of = |app: &App| -> (u16, u16, u16, u16) {
                let (state, rows) = app.smart_action_view().expect("the form is open");
                let area = smart_action::geometry(app.frame, state.anchor(), rows.len() as u16);
                let row = rows
                    .iter()
                    .position(|row| row.contains("min"))
                    .expect("the min row");
                // Columns, not bytes: the row's marker is a multi-byte glyph, and
                // a caret is placed in cells.
                let before = rows[row].split("1122").next().expect("the value");
                let value_x = area.x + 1 + unicode_width::UnicodeWidthStr::width(before) as u16;
                let (x, y) = state.form_caret(area).expect("a caret");
                (x, y, value_x, area.y + 1 + row as u16)
            };

            for typed in 0..=4u16 {
                precision_key(&mut app, KeyCode::Home, KeyModifiers::NONE);
                for _ in 0..typed {
                    precision_key(&mut app, KeyCode::Right, KeyModifiers::NONE);
                }
                let (x, y, value_x, row_y) = caret_of(&app);
                assert_eq!(y, row_y, "the caret is on another row");
                assert_eq!(
                    x,
                    value_x + typed,
                    "{typed} presses of Right: the caret is not on its digit"
                );
            }

            // End reaches past the last digit, or you cannot append to a field.
            precision_key(&mut app, KeyCode::End, KeyModifiers::NONE);
            let (x, _, value_x, _) = caret_of(&app);
            assert_eq!(x, value_x + 4);
            precision_key(&mut app, KeyCode::Home, KeyModifiers::NONE);
            let (x, _, value_x, _) = caret_of(&app);
            assert_eq!(x, value_x, "and Home reaches the first");
        }

        /// The write refuses a span that is not one whole call.
        ///
        /// `literal_sliders` reads text, not a parse tree, so on a malformed
        /// nesting it can name an inner `slider(`. A write over that span leaves
        /// the outer call's arguments beside the new call, as in
        /// `slider(1.44, 0, 5, 0.01), 100, 20000, slider(...)))`. The form must
        /// refuse that write.
        #[test]
        fn only_one_whole_call_is_ever_written_over() {
            let whole = "slider(100, 100, 20000, 0.01)";
            assert!(crate::app::smart_action::spans_one_whole_call(
                whole,
                &(0..whole.len())
            ));

            // A slider holding a slider: the span the form must never replace
            // in place, because the arguments beside it would be stranded.
            let nested = "slider(slider(100, 100, 20000, 0.01), 100, 20000, 0.01)";
            assert!(
                !crate::app::smart_action::spans_one_whole_call(nested, &(0..nested.len())),
                "a slider holding a slider is not one whole call"
            );

            // Half-typed, and unbalanced: also not something to write over.
            assert!(!crate::app::smart_action::spans_one_whole_call(
                "slider(1, 0, 2",
                &(0..14)
            ));
            assert!(!crate::app::smart_action::spans_one_whole_call(
                "lpf(800)",
                &(0..8)
            ));
        }

        /// A half-typed `slider(` at the caret is a target for the form, although
        /// the call is not complete.
        #[test]
        fn a_half_typed_slider_call_is_something_the_form_can_finish() {
            use crate::app::smart_action::{EmptyCall, empty_call_at};
            let source = "$: s(\"bd\").lpf(slider(";
            let Some(EmptyCall::Span(range)) = empty_call_at(source, source.len()) else {
                panic!("a fader");
            };
            assert_eq!(&source[range], "slider(");

            // Whitespace after it is still nothing typed.
            let spaced = "$: s(\"bd\").lpf(slider(  ";
            assert!(matches!(
                empty_call_at(spaced, spaced.len()),
                Some(EmptyCall::Span(_))
            ));

            // An argument half typed is NOT: guessing at what somebody meant
            // to finish is how a form earns distrust.
            let started = "$: s(\"bd\").lpf(slider(0.5";
            assert_eq!(empty_call_at(started, started.len()), None);

            // A call that is already closed is the complete-call case.
            let closed = "$: s(\"bd\").lpf(slider(1, 0, 2, 0.01))";
            assert_eq!(empty_call_at(closed, closed.len()), None);

            // And a `slider(` that is not code is no call at all - the same
            // answer the Edit menu's greyed row gives for that caret.
            for text in ["// try slider(", "$: s(\"slider(", "$: myslider("] {
                assert_eq!(empty_call_at(text, text.len()), None, "{text:?}");
            }
        }

        /// `slider()` with the caret between its brackets is the same request.
        /// The reference column inserts a call this way, and so does typing
        /// brackets in pairs. The span includes the call's own `)`, so the write
        /// does not leave a second `)` behind.
        #[test]
        fn an_empty_closed_slider_call_is_finished_bracket_and_all() {
            use crate::app::smart_action::{EmptyCall, empty_call_at};
            let span = |source: &str| {
                let caret = source.find("slider(").expect("the call") + "slider(".len();
                match empty_call_at(source, caret) {
                    Some(EmptyCall::Span(range)) => source[range].to_owned(),
                    other => panic!("{source:?}: {other:?}"),
                }
            };

            // The `)` is the call's own when everything around it still closes.
            assert_eq!(span("$: s(\"bd\").gain(slider())"), "slider()");
            assert_eq!(span("$: slider()"), "slider()");
            assert_eq!(
                span("$: s(\"bd\").gain(slider(  ))"),
                "slider(  )",
                "blank between the brackets is still nothing typed"
            );
            // A bracket inside a pattern string is not code, and counts for
            // nothing either way.
            assert_eq!(
                span("$: s(\"bd(3,8)\").gain(slider()).lpf(800)"),
                "slider()"
            );
            // A pattern still being typed above is its own statement: its open
            // bracket does not decide whose `)` this one is.
            assert_eq!(
                span("$: note(\"c e g\").lpf(\n$: s(\"bd\").gain(slider())"),
                "slider()"
            );

            // `gain()` typed first and `slider(` typed inside it: that `)` is
            // gain's, and taking it would leave gain open.
            assert_eq!(span("$: s(\"bd\").gain(slider()"), "slider(");
            assert_eq!(
                span("$: stack(\n  s(\"bd\").gain(slider(),\n  s(\"hh\")\n)"),
                "slider(",
                "the one close missing is gain's, even with stack's still to come"
            );
            assert_eq!(
                span("$: s(\"bd\").gain(slider()\n$: s(\"hh\").gain(1))"),
                "slider(",
                "a stray bracket in the next statement is not gain's close"
            );

            // A comment between the caret and the call's own `)` is somebody's
            // words: the form neither writes over it nor leaves the bracket
            // stranded behind it, and says why.
            for source in [
                "$: s(\"bd\").gain(slider( /* soon */ ))",
                "$: s(\"bd\").gain(slider( // soon\n))",
            ] {
                let caret = source.find("slider(").expect("the call") + "slider(".len();
                assert!(
                    matches!(empty_call_at(source, caret), Some(EmptyCall::Refused(_))),
                    "{source:?}"
                );
            }
            // Where the `)` after the comment is gain's, the call is simply open
            // and the comment stays where it is.
            assert_eq!(span("$: s(\"bd\").gain(slider( /* soon */ )"), "slider(");

            // Finished through the form, each is a whole call that parses.
            for (before, after) in [
                (
                    "$: s(\"bd\").gain(slider())\n",
                    "$: s(\"bd\").gain(slider(1, 0, 1, 0.01))\n",
                ),
                (
                    "$: s(\"bd\").gain(slider()\n",
                    "$: s(\"bd\").gain(slider(1, 0, 1, 0.01))\n",
                ),
                (
                    "$: note(\"c e g\").lpf(\n$: s(\"bd\").gain(slider())\n",
                    "$: note(\"c e g\").lpf(\n$: s(\"bd\").gain(slider(1, 0, 1, 0.01))\n",
                ),
                (
                    "$: s(\"bd\").gain(slider( /* soon */ )\n",
                    "$: s(\"bd\").gain(slider(1, 0, 1, 0.01) /* soon */ )\n",
                ),
            ] {
                let directory = tempfile::tempdir().expect("temp dir");
                let (mut app, scene) = plain_app(directory.path(), before);
                app.audible_scene = Some(scene);
                // The fixture seeds its text ahead of the set's starter line.
                let seeded = app.editor().source();
                assert!(seeded.starts_with(before), "{seeded:?}");
                let caret = before.find("slider(").expect("the call") + "slider(".len();
                app.editor_mut()
                    .set_selection(Selection::caret(ByteOffset(caret)))
                    .expect("caret between the brackets");
                precision_key(&mut app, KeyCode::Char('j'), KeyModifiers::CONTROL);
                precision_key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
                precision_key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
                assert_eq!(
                    app.editor().source(),
                    seeded.replacen(before, after, 1),
                    "from {before:?}"
                );
                assert!(app.smart_action_view().is_none(), "written and away");
            }

            // The refusal through the App: no form, the reason in the status, and
            // the score as it was.
            let before = "$: s(\"bd\").gain(slider( /* soon */ ))\n";
            let directory = tempfile::tempdir().expect("temp dir");
            let (mut app, _) = plain_app(directory.path(), before);
            let seeded = app.editor().source();
            let caret = before.find("slider(").expect("the call") + "slider(".len();
            app.editor_mut()
                .set_selection(Selection::caret(ByteOffset(caret)))
                .expect("caret inside the call");
            precision_key(&mut app, KeyCode::Char('j'), KeyModifiers::CONTROL);
            assert!(
                app.smart_action_view().is_none(),
                "no form over the comment"
            );
            assert!(
                app.status
                    .starts_with("quick action: a comment sits inside this slider("),
                "{}",
                app.status
            );
            assert_eq!(app.editor().source(), seeded);
        }

        /// Replace the whole score with `source`.
        fn reseed(app: &mut App, source: &str) {
            app.editor_mut()
                .dispatch(
                    Command::SelectAll,
                    HistoryMoment(0),
                    &mut MemoryClipboard::default(),
                )
                .expect("select");
            app.editor_mut()
                .dispatch(
                    Command::PasteText(source.to_owned()),
                    HistoryMoment(0),
                    &mut MemoryClipboard::default(),
                )
                .expect("reseed");
        }

        /// An app whose score is exactly `source`, with `recordings:2` saved
        /// and a library that knows the `recordings` and `bd` banks.
        fn app_with_sample(directory: &std::path::Path, source: &str) -> App {
            let (mut app, scene) = plain_app(directory, source);
            reseed(&mut app, source);
            app.worker.set_library(std::sync::Arc::new(
                rustel_runtime::samples::SampleLibrary::with_unasked_banks_for_test(&[
                    "recordings",
                    "bd",
                ]),
            ));
            app.audible_scene = Some(scene);
            app.last_recorded_sample = Some("recordings:2".into());
            app
        }

        /// What a paste of `recordings:2` makes of `marked`, whose `⌶` is the
        /// caret: the score after it, or `None` where nothing is offered.
        fn pasted(marked: &str) -> Option<String> {
            let caret = marked.find('⌶').expect("a caret mark");
            let source = marked.replace('⌶', "");
            let (range, text) =
                crate::app::smart_action::sample_paste(&source, caret, "recordings:2")?;
            let mut written = source;
            written.replace_range(range, &text);
            Some(written)
        }

        /// Inside a pattern the name is a step of its own: spaced from its
        /// neighbours, and past the end of a step the caret stands inside.
        #[test]
        fn a_pasted_sample_is_a_step_of_its_own_inside_a_pattern() {
            for (marked, expected) in [
                (r#"$: s("bd⌶")"#, r#"$: s("bd recordings:2")"#),
                (r#"$: s("⌶bd")"#, r#"$: s("recordings:2 bd")"#),
                (r#"$: s("b⌶d")"#, r#"$: s("bd recordings:2")"#),
                (r#"$: s("bd:⌶3")"#, r#"$: s("bd:3 recordings:2")"#),
                (r#"$: s("bd⌶*2 sd")"#, r#"$: s("bd*2 recordings:2 sd")"#),
                (r#"$: s("[bd ⌶sd]")"#, r#"$: s("[bd recordings:2 sd]")"#),
                (r#"$: s("⌶")"#, r#"$: s("recordings:2")"#),
                (r#"$: s('bd⌶')"#, r#"$: s('bd recordings:2')"#),
                // A string still being typed at the end of the score.
                (r#"$: s("bd⌶"#, r#"$: s("bd recordings:2"#),
            ] {
                assert_eq!(pasted(marked).as_deref(), Some(expected), "{marked}");
            }
        }

        /// Only a string the score plays as sounds, with no bank on it and no
        /// muting label, takes the name: in any other string it would be a
        /// note, a scale, a bank or a source, and plays no sample.
        #[test]
        fn a_string_that_plays_no_sample_takes_no_paste() {
            for (marked, expected) in [
                (r#"$: sound("bd⌶")"#, r#"$: sound("bd recordings:2")"#),
                (
                    r#"$: note("c e").s("saw⌶")"#,
                    r#"$: note("c e").s("saw recordings:2")"#,
                ),
            ] {
                assert_eq!(pasted(marked).as_deref(), Some(expected), "{marked}");
            }
            for marked in [
                r#"$: note("c e⌶ g").s("sawtooth")"#,
                r#"$: n("0 2").scale("C:ma⌶jor")"#,
                r#"$: n("0 ⌶2").s("bd")"#,
                r#"$: s("bd").gain("0.5 1⌶")"#,
                r#"samples('github:a/b⌶')"#,
                // A bank looks the name up as `tr909_recordings`.
                r#"$: s("bd⌶").bank("tr909")"#,
                r#"$: s("bd").bank("tr909⌶")"#,
                // A muted lane plays nothing.
                r#"_$: s("bd⌶")"#,
                "_$: ⌶\n",
                // A score that already plays the sample gains nothing.
                "$: s(\"recordings:2\")\n$: note(\"c⌶\")\n",
            ] {
                assert_eq!(pasted(marked), None, "{marked:?}");
            }
        }

        /// Outside a pattern the whole `s("…")` call goes only where a statement
        /// starts: after an empty label, or as a new `$:` line on a blank line
        /// of a score that has labels or no code at all.
        #[test]
        fn a_pasted_call_goes_only_where_a_statement_starts() {
            for (marked, expected) in [
                ("$: ⌶\n", "$: s(\"recordings:2\")\n"),
                ("$:⌶\n", "$: s(\"recordings:2\")\n"),
                (
                    "drums:  // later⌶\n",
                    "drums: s(\"recordings:2\") // later\n",
                ),
                ("⌶$: \n// later\n", "$: s(\"recordings:2\")\n// later\n"),
                (
                    "$: s(\"bd\")\n⌶\n$: s(\"hh\")\n",
                    "$: s(\"bd\")\n$: s(\"recordings:2\")\n$: s(\"hh\")\n",
                ),
                (
                    "$: s(\"bd\")\r\n⌶\r\n",
                    "$: s(\"bd\")\r\n$: s(\"recordings:2\")\r\n",
                ),
                ("$: s(\"bd\")\n⌶", "$: s(\"bd\")\n$: s(\"recordings:2\")"),
                ("⌶", "$: s(\"recordings:2\")"),
                ("// drums\n⌶", "// drums\n$: s(\"recordings:2\")"),
            ] {
                assert_eq!(pasted(marked).as_deref(), Some(expected), "{marked:?}");
            }
            for marked in [
                // Against a closing quote, which is outside the pattern.
                "$: s(\"bd\"⌶)\n",
                "$: \"bd sd\"⌶\n",
                // At either end of a statement, or inside a word of code.
                "$: s(\"bd\")⌶\n",
                "⌶$: s(\"bd\")\n",
                "$: s(\"bd\").fa⌶st(2)\n",
                "$: s(\"bd\").gain(0.⌶5)\n",
                // In a comment, where nothing plays.
                "// drums⌶\n$: s(\"bd\")\n",
                "$: s(\"bd\") // dr⌶ums\n",
                // A blank line the statement above still runs past.
                "$: s(\"bd\")\n⌶\n  .fast(2)\n",
                // A blank line inside a block comment.
                "$: s(\"bd\")\n/*\n⌶\n*/\n",
                // A score with no labels: a new last expression would replace
                // the pattern above it, and a labelled one would silence it.
                "s(\"bd\")\n⌶\n",
            ] {
                assert_eq!(pasted(marked), None, "{marked:?}");
            }
            // A bank named with a space is two steps to mini-notation.
            assert_eq!(
                crate::app::smart_action::sample_paste("$: \n", 3, "My Recordings:2"),
                None
            );
        }

        /// After a sample is saved, ^J where it can be pasted opens a menu with
        /// the paste row, and Enter writes it and closes without evaluating.
        #[test]
        fn ctrl_j_pastes_the_latest_recorded_sample() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = app_with_sample(directory.path(), "$: \n");
            let at = app.editor().source().find("$: ").expect("the label") + 3;
            app.editor_mut()
                .set_selection(Selection::caret(ByteOffset(at)))
                .expect("caret after the label");
            precision_key(&mut app, KeyCode::Char('j'), KeyModifiers::CONTROL);
            let (_, rows) = app.smart_action_view().expect("the menu is open");
            assert_eq!(rows.len(), 1, "the paste is the only row: {rows:?}");
            assert!(
                rows[0].contains("paste") && rows[0].contains("recordings:2"),
                "{rows:?}"
            );
            precision_key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
            let source = app.editor().source();
            assert_eq!(source, "$: s(\"recordings:2\")\n");
            assert!(app.smart_action_view().is_none(), "a paste closes the menu");
            assert_eq!(app.status, "pasted recordings:2");
            assert!(
                app.pending_evaluation.is_none() && app.inflight.is_empty(),
                "pasting a sample leaves evaluation to the player"
            );

            // Inside the pattern: the name, not another call.
            let mut app = app_with_sample(directory.path(), "$: s(\"bd\")\n");
            let at = app.editor().source().find("bd").expect("the pattern") + 2;
            app.editor_mut()
                .set_selection(Selection::caret(ByteOffset(at)))
                .expect("caret after bd");
            precision_key(&mut app, KeyCode::Char('j'), KeyModifiers::CONTROL);
            precision_key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
            assert_eq!(app.editor().source(), "$: s(\"bd recordings:2\")\n");
        }

        #[test]
        fn refused_sample_paste_keeps_the_menu_and_reports_failure() {
            let directory = tempfile::tempdir().expect("temp dir");
            let source = "$: \n";
            let mut app = app_with_sample(directory.path(), source);
            *app.editor_mut() = crate::editor::Editor::with_config(
                source,
                crate::editor::EditorConfig {
                    maximum_document_bytes: source.len(),
                    ..Default::default()
                },
            )
            .expect("bounded editor");
            app.editor_mut()
                .set_selection(Selection::caret(ByteOffset(3)))
                .expect("caret after label");
            let revision = app.editor().revision();
            precision_key(&mut app, KeyCode::Char('j'), KeyModifiers::CONTROL);
            precision_key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
            assert_eq!(app.editor().source(), source);
            assert_eq!(app.editor().revision(), revision);
            assert!(app.smart_action_view().is_some());
            assert!(app.status.starts_with("could not paste the sample"));
            assert!(app.pending_evaluation.is_none() && app.inflight.is_empty());
        }

        /// Pasting succeeds even when evaluation would refuse the score. The
        /// refusal appears only after the player explicitly evaluates it.
        #[test]
        fn a_pasted_sample_waits_for_explicit_evaluation() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = app_with_sample(directory.path(), "$: \n");
            app.worker.set_library(std::sync::Arc::new(
                rustel_runtime::samples::SampleLibrary::with_unasked_banks_for_test(&["bd"]),
            ));
            app.editor_mut()
                .set_selection(Selection::caret(ByteOffset(3)))
                .expect("caret after the label");
            precision_key(&mut app, KeyCode::Char('j'), KeyModifiers::CONTROL);
            precision_key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
            assert_eq!(app.editor().source(), "$: s(\"recordings:2\")\n");
            assert_eq!(app.status, "pasted recordings:2");
            assert!(
                app.pending_evaluation.is_none() && app.inflight.is_empty(),
                "the paste did not request evaluation"
            );
            assert!(app.update_failure.is_none());

            app.dispatch_editor(Command::Evaluate)
                .expect("explicit evaluation");
            assert!(
                app.status.starts_with("refused") && app.status.contains("recordings"),
                "{}",
                app.status
            );
        }

        /// Where no paste leaves a score that plays the sample, ^J offers none
        /// and says why it has nothing for the caret, as it does with no sample.
        #[test]
        fn ctrl_j_offers_no_paste_where_the_score_would_break() {
            let directory = tempfile::tempdir().expect("temp dir");
            let seeded = "$: s(\"bd\")\n";
            let mut app = app_with_sample(directory.path(), seeded);
            for caret in [seeded.find(')').expect("the call's end") + 1, 0] {
                app.editor_mut()
                    .set_selection(Selection::caret(ByteOffset(caret)))
                    .expect("caret in code");
                precision_key(&mut app, KeyCode::Char('j'), KeyModifiers::CONTROL);
                assert!(app.smart_action_view().is_none(), "at {caret}");
                assert!(
                    app.status.contains("put the caret on a number first"),
                    "{}",
                    app.status
                );
                assert_eq!(app.editor().source(), seeded);
            }
        }

        /// On a number inside a pattern the paste row stands alone, and the
        /// status still says why that number is no fader.
        #[test]
        fn a_number_in_a_pattern_offers_the_paste_and_says_why_it_is_no_fader() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = app_with_sample(directory.path(), "$: s(\"bd:3\")\n");
            let at = app.editor().source().find('3').expect("the number");
            app.editor_mut()
                .set_selection(Selection::caret(ByteOffset(at)))
                .expect("caret on the number");
            precision_key(&mut app, KeyCode::Char('j'), KeyModifiers::CONTROL);
            let (_, rows) = app.smart_action_view().expect("the menu is open");
            assert_eq!(rows.len(), 1, "{rows:?}");
            assert!(
                app.status.contains("inside a pattern") && app.status.contains("Enter pastes"),
                "{}",
                app.status
            );
            precision_key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
            assert_eq!(app.editor().source(), "$: s(\"bd:3 recordings:2\")\n");
        }

        /// Every caret in a few scores: wherever ^J offers the paste, choosing
        /// it leaves a score that parses and plays the sample, and everywhere
        /// else the score is left as it was.
        #[test]
        fn every_paste_ctrl_j_offers_parses_and_plays() {
            let directory = tempfile::tempdir().expect("temp dir");
            let syntax = |source: &str| {
                rustel_runtime::lint::lint(source, false, None)
                    .into_iter()
                    .filter(|problem| problem.level == rustel_runtime::lint::Level::Syntax)
                    .collect::<Vec<_>>()
            };
            let plays = |source: &str| {
                rustel_runtime::lint::live_sound_names(source)
                    .iter()
                    .any(|sound| {
                        sound.name == "recordings" && sound.n == 2.0 && sound.banks.is_empty()
                    })
            };
            let mut offered = 0usize;
            for source in [
                "$: s(\"bd sd\").gain(0.5) // drums\n",
                "$: s(\"<bd [sd hh]>\")\n\n$: s(\"hh*4, cp(3,8)\").fast(2)\n",
                "setcps(0.5)\n\n$: s(\"bd:3\")\n  .lpf(800)\n\n",
                "// a comment\n\n$: s(\"bd\")\n",
                "s(\"bd sd\")\n\n",
                "$: s(`bd\nsd`)\n",
                "$: note(\"c e g\").s(\"sawtooth\")\n\n",
                "$: s(\"bd sd\").bank(\"tr909\")\n\n_$: s(\"hh\")\n",
                "",
            ] {
                assert!(syntax(source).is_empty(), "{source:?} parses to begin with");
                let mut app = app_with_sample(directory.path(), source);
                for caret in 0..=source.len() {
                    if !source.is_char_boundary(caret) {
                        continue;
                    }
                    reseed(&mut app, source);
                    app.editor_mut()
                        .set_selection(Selection::caret(ByteOffset(caret)))
                        .expect("caret");
                    app.smart_action = None;
                    precision_key(&mut app, KeyCode::Char('j'), KeyModifiers::CONTROL);
                    let offers_paste = app
                        .smart_action_view()
                        .is_some_and(|(_, rows)| rows.iter().any(|row| row.contains("paste")));
                    if !offers_paste {
                        precision_key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
                        assert_eq!(app.editor().source(), source, "at caret {caret}");
                        continue;
                    }
                    offered += 1;
                    // The paste is the last row; Up wraps round to it.
                    precision_key(&mut app, KeyCode::Up, KeyModifiers::NONE);
                    precision_key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
                    let written = app.editor().source();
                    assert!(
                        syntax(&written).is_empty(),
                        "at caret {caret} in {source:?}: {written:?} does not parse: {:?}",
                        syntax(&written)
                    );
                    assert!(
                        plays(&written),
                        "at caret {caret} in {source:?}: {written:?} does not play the sample"
                    );
                }
            }
            assert!(offered > 30, "only {offered} carets offered the paste");
        }

        /// The Edit menu's Quick action row is live where the latest sample can
        /// be pasted, and stays grey where it cannot.
        #[test]
        fn a_recorded_sample_lights_the_smart_action_row_where_it_can_be_pasted() {
            use crate::menu::{MenuAction, MenuItemKind};

            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = app_with_sample(directory.path(), "$: note(\"c\").s(\"bd\")\n\n");
            app.last_recorded_sample = None;
            let live = |app: &App| {
                app.menus()
                    .into_iter()
                    .flat_map(|menu| menu.items)
                    .find(|item| item.kind == MenuItemKind::Action(MenuAction::SmartAction))
                    .expect("the Quick action row")
                    .enabled
            };
            let caret = |app: &mut App, at: usize| {
                app.editor_mut()
                    .set_selection(Selection::caret(ByteOffset(at)))
                    .expect("move the caret");
            };
            let source = app.editor().source();
            let blank_line = source.len();
            let line_end = source.find('\n').expect("the first line's end");
            let pattern = source.find("bd").expect("the pattern") + 1;
            let note = source.find('c').expect("the note") + 1;

            caret(&mut app, blank_line);
            assert!(!live(&app), "nothing to offer yet");
            app.last_recorded_sample = Some("recordings:2".into());
            assert!(live(&app), "a blank line takes a new line of the sample");
            caret(&mut app, pattern);
            assert!(live(&app), "a pattern takes the name");
            caret(&mut app, note);
            assert!(!live(&app), "a note takes nothing");
            caret(&mut app, line_end);
            assert!(!live(&app), "the end of a call takes nothing");
        }
    }
}
