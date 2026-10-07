//! `^J` - the smart action: what the caret is on, and what can be done to it.
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

/// A form row is a marker, then a label, then the value. Both the drawing
/// and the caret are reckoned from these - one definition, because a caret
/// placed from a second one drifts, and a caret one cell off its text is a
/// field you cannot type the end of.
const FIELD_MARKER: usize = 2;
const FIELD_LABEL: usize = 6;

/// The screen column a form field's value starts at. The renderer insets
/// each row by one from the popup's left edge.
fn field_value_x(area: Rect) -> u16 {
    area.x + 1 + (FIELD_MARKER + FIELD_LABEL) as u16
}

/// The most a form field will hold. A slider bound is a number, not an
/// essay, and an unbounded paste into a caret-anchored popup is a way to
/// make the popup unreadable.
const FIELD_CHARS: usize = 24;

const STALE_SOURCE: &str = "the score or scene changed - close and reopen smart action";

/// What the smart action found at the caret, and so what it can offer.
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

/// One number field of the slider form.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Field {
    Value,
    Min,
    Max,
    Step,
}

impl Field {
    const ALL: [Self; 4] = [Self::Value, Self::Min, Self::Max, Self::Step];

    /// Where this field's text sits in the form's `fields`.
    const fn index(self) -> usize {
        self as usize
    }

    fn label(self) -> &'static str {
        match self {
            Self::Value => "value",
            Self::Min => "min",
            Self::Max => "max",
            Self::Step => "step",
        }
    }
}

/// The form the menu becomes.
#[derive(Clone, Debug)]
struct SliderForm {
    fields: [String; 4],
    /// Where the caret is in the field under the keys, in bytes.
    cursor: usize,
    /// The whole field is selected: the next character replaces it, the
    /// way a freshly focused number input behaves.
    selected: bool,
    field: usize,
    /// What the last attempt to write it said, if it could not be written.
    error: Option<String>,
    /// It has been written to the score at least once, so Esc leaves the
    /// fader rather than the number it came from.
    written: bool,
}

impl SliderForm {
    fn text(&self) -> &str {
        &self.fields[self.field.min(3)]
    }

    fn text_mut(&mut self) -> &mut String {
        let field = self.field.min(3);
        &mut self.fields[field]
    }

    fn insert(&mut self, text: &str) {
        if !text
            .chars()
            .all(|c| c.is_ascii_digit() || matches!(c, '.' | '-' | '+' | 'e' | 'E'))
        {
            return;
        }
        if self.selected {
            self.text_mut().clear();
            self.cursor = 0;
            self.selected = false;
        }
        let room = FIELD_CHARS.saturating_sub(self.text().chars().count());
        let text: String = text.chars().take(room).collect();
        let cursor = self.cursor;
        self.text_mut().insert_str(cursor, &text);
        self.cursor += text.len();
    }

    fn erase(&mut self, backwards: bool) {
        if self.selected {
            self.text_mut().clear();
            self.cursor = 0;
            self.selected = false;
            return;
        }
        if backwards && self.cursor > 0 {
            self.cursor -= 1;
            let cursor = self.cursor;
            self.text_mut().remove(cursor);
        } else if !backwards && self.cursor < self.text().len() {
            let cursor = self.cursor;
            self.text_mut().remove(cursor);
        }
    }

    /// Put new text in a field. The field under the keys is selected whole
    /// again, the way a freshly focused field is: a caret left where it was
    /// can stand past the end of a shorter number, and the next frame
    /// slices the caret's position out of that text.
    fn set_field(&mut self, field: Field, text: String) {
        self.fields[field.index()] = text;
        if self.field == field.index() {
            self.focus(field.index());
        }
    }

    /// Move to another field, with the whole of it selected - Tab out of
    /// a number field and back in should not leave the caret mid-digit.
    fn focus(&mut self, field: usize) {
        self.field = field.min(Field::ALL.len() - 1);
        self.cursor = self.text().len();
        self.selected = true;
    }

    /// The four numbers, if they are four numbers.
    fn parse(&self) -> Result<[f64; 4], String> {
        let mut out = [0.0; 4];
        for (at, field) in Field::ALL.iter().enumerate() {
            let text = self.fields[at].trim();
            match text.parse::<f64>() {
                Ok(number) if number.is_finite() => out[at] = number,
                _ => return Err(format!("{} is not a number", field.label())),
            }
        }
        Ok(out)
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

/// The smart action, open at the caret.
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
fn guess_range(value: f64) -> (f64, f64, f64) {
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
    /// Whether the smart action has anything to offer where the caret
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

    /// `^J`, or Edit ▸ Smart action. Reads what the caret is on and opens
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
                self.status = format!("smart action: {why}");
                self.dirty_frame = true;
                return;
            }
            // A number the fader refuses is still said, beside the paste.
            (Target::Nothing(why), Some(sound)) if on_number => {
                format!("smart action: {why} \u{b7} Enter pastes {sound}")
            }
            (Target::Nothing(_), Some(sound)) => {
                format!("smart action: Enter pastes {sound} \u{b7} Esc closes")
            }
            _ => "smart action: Enter chooses \u{b7} Esc closes".into(),
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
        self.status = "Tab between the fields \u{b7} Enter applies \u{b7} Esc closes".into();
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
            form.error = Some("could not write the fader - close and reopen smart action".into());
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
            self.status = format!("smart action: {STALE_SOURCE}");
            self.dirty_frame = true;
            return;
        }
        if self.replace_range_in_scene(scene, range, &plain).is_none() {
            self.smart_action = Some(state);
            self.status = "could not remove the fader - close and reopen smart action".into();
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
                let mut rows: Vec<String> = Field::ALL
                    .iter()
                    .enumerate()
                    .map(|(at, field)| {
                        // The marker is padded to its own width rather
                        // than assumed to be one cell: a glyph a terminal
                        // draws wide would shift the whole row out from
                        // under the caret.
                        let marker = if at == form.field { "\u{25b8}" } else { " " };
                        let pad = FIELD_MARKER.saturating_sub(UnicodeWidthStr::width(marker));
                        format!(
                            "{marker}{:pad$}{:<FIELD_LABEL$}{}",
                            "",
                            field.label(),
                            form.fields[at]
                        )
                    })
                    .collect();
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

    /// Keys and pastes belong to the open smart action, including its
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
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);
        let in_form = self
            .smart_action
            .as_ref()
            .is_some_and(|state| state.form.is_some());
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
                    let forwards = matches!(key.code, KeyCode::Tab | KeyCode::Down)
                        && !(shift && key.code == KeyCode::Tab);
                    if let Some(form) = self
                        .smart_action
                        .as_mut()
                        .and_then(|state| state.form.as_mut())
                    {
                        let len = Field::ALL.len();
                        let next = (form.field + if forwards { 1 } else { len - 1 }) % len;
                        form.focus(next);
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
            match key.code {
                KeyCode::Char('a' | 'A') if primary => form.selected = true,
                KeyCode::Left if !alt && !primary => {
                    form.cursor = if form.selected {
                        0
                    } else {
                        form.cursor.saturating_sub(1)
                    };
                    form.selected = false;
                }
                KeyCode::Right if !alt && !primary => {
                    form.cursor = if form.selected {
                        form.text().len()
                    } else {
                        (form.cursor + 1).min(form.text().len())
                    };
                    form.selected = false;
                }
                KeyCode::Home => {
                    form.cursor = 0;
                    form.selected = false;
                }
                KeyCode::End => {
                    form.cursor = form.text().len();
                    form.selected = false;
                }
                KeyCode::Backspace => form.erase(true),
                KeyCode::Delete => form.erase(false),
                KeyCode::Char(character) if !primary && !alt => {
                    form.insert(&character.to_string());
                }
                _ => return true,
            }
            self.lint_smart_action_soon();
            return true;
        }
        match key.code {
            KeyCode::Esc => self.close_smart_action(),
            KeyCode::Up | KeyCode::Down => {
                if let Some(state) = self.smart_action.as_mut() {
                    let len = state.menu_rows().len().max(1);
                    let delta = if key.code == KeyCode::Down {
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
            self.status = format!("smart action: nowhere at the caret takes {sound}");
            return;
        };
        if self.replace_range_in_scene(scene, range, &text).is_none() {
            self.smart_action = Some(state);
            self.status = "could not paste the sample - close and reopen smart action".into();
            return;
        }
        self.status = format!("pasted {sound}");
    }

    /// A smart action belongs to the score it opened on. Leaving that
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
                "smart action closed".into()
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
            .title(" \u{2318} smart action ")
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
