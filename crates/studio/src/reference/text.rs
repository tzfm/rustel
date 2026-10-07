//! Entry body construction, wrapping, and styled text rendering.

use super::*;

/// Build the wrapped rows shared by drawing, pointer hit testing and
/// copying, so a selection matches the text it copies.
pub fn entry_body(entry: &Entry, width: usize) -> Vec<BodyLine> {
    let line = |text: String, kind: BodyKind| BodyLine {
        text,
        kind,
        code: Vec::new(),
    };
    let blank = || line(String::new(), BodyKind::Blank);
    let mut lines = Vec::new();
    lines.push(line(entry.signature(), BodyKind::Signature));
    // Show distinct aliases; case-only variants add no useful name to the
    // entry body.
    let aliases = entry
        .synonyms
        .iter()
        .filter(|synonym| !synonym.eq_ignore_ascii_case(&entry.name))
        .cloned()
        .collect::<Vec<_>>();
    if !aliases.is_empty() {
        lines.push(line(
            format!("also {}", aliases.join(", ")),
            BodyKind::Synonyms,
        ));
    }
    if entry.deprecated {
        lines.push(line("deprecated".to_owned(), BodyKind::Deprecated));
    }
    if !entry.origin.is_empty() {
        lines.push(line(
            format!("{} extension", display_origin(&entry.origin)),
            BodyKind::Origin,
        ));
    }
    if entry.snippet.is_some() {
        lines.push(line(
            "snippet · Enter pastes it at the caret".to_owned(),
            BodyKind::Origin,
        ));
    }
    lines.push(blank());
    for paragraph in entry.description.split("\n\n") {
        for (text, code) in wrap_inline(&paragraph.replace('\n', " "), width) {
            lines.push(BodyLine {
                text,
                kind: BodyKind::Prose,
                code,
            });
        }
        lines.push(blank());
    }
    let terminal_note = super::super::visuals::painter_terminal_entry(&entry.name);
    if !terminal_note.is_empty() {
        for (text, code) in wrap_inline(terminal_note, width) {
            lines.push(BodyLine {
                text,
                kind: BodyKind::Terminal,
                code,
            });
        }
        lines.push(blank());
    }
    if !entry.params.is_empty() {
        for param in &entry.params {
            let head = if param.r#type.is_empty() {
                param.name.clone()
            } else {
                format!("{} ({})", param.name, param.r#type)
            };
            lines.push(line(head, BodyKind::Param));
            for (text, code) in wrap_inline(&param.description, width.saturating_sub(2)) {
                lines.push(prefix_body("  ", text, code, BodyKind::ParamDetail));
            }
            for choice in &param.choices {
                let prefix = format!("  {} - ", choice.value);
                let continuation = " ".repeat(prefix.chars().count());
                let available = width.saturating_sub(UnicodeWidthStr::width(prefix.as_str()));
                for (row, (text, code)) in wrap_inline(&choice.description, available)
                    .into_iter()
                    .enumerate()
                {
                    lines.push(prefix_body(
                        if row == 0 { &prefix } else { &continuation },
                        text,
                        code,
                        BodyKind::ParamDetail,
                    ));
                }
            }
            let param_note =
                super::super::visuals::painter_terminal_option(&entry.name, &param.name);
            if !param_note.is_empty() {
                for (text, code) in wrap_inline(param_note, width.saturating_sub(4)) {
                    lines.push(prefix_body("  · ", text, code, BodyKind::Terminal));
                }
            }
        }
        lines.push(blank());
    }
    for example in &entry.examples {
        for code in example.lines() {
            for wrapped in wrap_code(code, width) {
                lines.push(line(wrapped, BodyKind::Example));
            }
        }
        lines.push(blank());
    }
    lines
}

/// Apply the theme's event mark over a syntax-highlighted row. Marking each
/// cell preserves its existing colours for outline and recolouring styles;
/// fills and fades use the panel's surface colour.
#[cfg(feature = "hydra")]
#[allow(clippy::too_many_arguments)]
pub(super) fn paint_mark(
    buffer: &mut Buffer,
    x: u16,
    y: u16,
    width: u16,
    line: &str,
    columns: std::ops::Range<usize>,
    color: ratatui::style::Color,
    strength: f32,
    theme: &Theme,
) {
    let span = super::super::textblock::cell_span(line, columns);
    let from = x.saturating_add(span.start);
    let to = x.saturating_add(span.end).min(x.saturating_add(width));
    for cell_x in from..to {
        let Some(cell) = buffer.cell_mut((cell_x, y)) else {
            continue;
        };
        let base = cell.style();
        let marked = theme.event_mark.apply(base, color, theme.surface);
        let style = if strength >= 1.0 {
            marked
        } else {
            super::super::theme::fade_mark(base, marked, strength, theme.foreground, theme.surface)
        };
        cell.set_style(style);
    }
}

fn prefix_body(prefix: &str, text: String, code: Vec<(usize, usize)>, kind: BodyKind) -> BodyLine {
    let shift = prefix.chars().count();
    BodyLine {
        text: format!("{prefix}{text}"),
        kind,
        code: code
            .into_iter()
            .map(|(from, to)| (from + shift, to + shift))
            .collect(),
    }
}

/// Greedy word wrap for the shelf's preview caption.
#[cfg(feature = "hydra")]
pub(super) fn wrap(text: &str, width: usize) -> Vec<String> {
    let width = width.max(8);
    let mut lines = Vec::new();
    let mut current = String::new();
    for word in text.split_whitespace() {
        let candidate_width = UnicodeWidthStr::width(current.as_str())
            + usize::from(!current.is_empty())
            + UnicodeWidthStr::width(word);
        if !current.is_empty() && candidate_width > width {
            lines.push(std::mem::take(&mut current));
        }
        if !current.is_empty() {
            current.push(' ');
        }
        current.push_str(word);
    }
    if !current.is_empty() {
        lines.push(current);
    }
    if lines.is_empty() {
        lines.push(String::new());
    }
    lines
}

/// Prose with inline `code` spans: the ticks are gone; the span is italic,
/// same colour as the sentence - markdown without a box.
pub(super) fn render_inline_line(
    buffer: &mut Buffer,
    x: u16,
    y: u16,
    width: u16,
    text: &str,
    code: &[(usize, usize)],
    prose: Style,
) {
    let chars: Vec<char> = text.chars().collect();
    let mut index = 0usize;
    let mut column = 0u16;
    while index < chars.len() && column < width {
        let marked = code.iter().any(|(from, to)| index >= *from && index < *to);
        let start = index;
        while index < chars.len() {
            let next = code.iter().any(|(from, to)| index >= *from && index < *to);
            if next != marked {
                break;
            }
            index += 1;
        }
        let piece: String = chars[start..index].iter().collect();
        let remaining = width.saturating_sub(column);
        let style = if marked {
            prose.add_modifier(Modifier::ITALIC)
        } else {
            prose
        };
        buffer.set_stringn(x + column, y, &piece, usize::from(remaining), style);
        column = column.saturating_add(UnicodeWidthStr::width(piece.as_str()) as u16);
    }
}

/// One example line in syntax colours, using the editor's classifier.
pub(super) fn render_code_line(
    buffer: &mut Buffer,
    x: u16,
    y: u16,
    width: u16,
    line: &str,
    theme: &Theme,
) {
    render_code_span(
        buffer,
        Rect::new(x, y, width, 1),
        line,
        0..line.len(),
        theme,
    );
}

/// Colour the complete logical line before clipping it to a wrapped row.
pub(super) fn render_code_span(
    buffer: &mut Buffer,
    area: Rect,
    line: &str,
    bytes: std::ops::Range<usize>,
    theme: &Theme,
) {
    let clusters = line
        .char_indices()
        .map(|(at, character)| &line[at..at + character.len_utf8()])
        .collect::<Vec<_>>();
    let tokens = syntax::classify(clusters.iter().copied());
    let mut column = 0u16;
    for ((at, _), (cluster, token)) in line.char_indices().zip(clusters.iter().zip(tokens)) {
        if !bytes.contains(&at) {
            continue;
        }
        if column >= area.width {
            break;
        }
        buffer.set_stringn(
            area.x + column,
            area.y,
            cluster,
            usize::from(area.width - column),
            Style::default().fg(token.color(theme)),
        );
        column += UnicodeWidthStr::width(*cluster).max(1) as u16;
    }
}

/// Fit a summary to one row and mark truncation with an ellipsis, preferring
/// a nearby word boundary.
pub(super) fn elide(text: &str, room: usize) -> String {
    if room == 0 || UnicodeWidthStr::width(text) <= room {
        return text.to_owned();
    }
    // One cell for the mark.
    let budget = room.saturating_sub(1);
    let mut end = 0usize;
    let mut width = 0usize;
    for (at, character) in text.char_indices() {
        let next = UnicodeWidthStr::width(character.to_string().as_str());
        if width + next > budget {
            break;
        }
        width += next;
        end = at + character.len_utf8();
    }
    let cut = text[..end].trim_end();
    // Back to a word boundary, unless that would leave almost nothing.
    let trimmed = match cut.rfind(' ') {
        Some(space) if space * 2 > cut.len() => &cut[..space],
        _ => cut,
    };
    format!("{}\u{2026}", trimmed.trim_end())
}

/// Wrap an example, breaking where the code already reads as jointed.
///
/// Prose wraps on spaces; score code has almost none. A chain like
/// `n(run(16)).scale("c:minor").s("sawtooth").delay(.7)` is one long word to
/// a word-wrapper, and a line clipped at the panel width loses text without
/// a sign: the reader cannot tell a short example from a truncated one.
///
/// A break goes before a `.` that starts a method call, at any call depth,
/// or after a `,`. Quotes are tracked, so a break never lands inside
/// `s("bd sd")`. A piece with no seam that fits is split on a character
/// boundary.
pub(super) fn wrap_code(text: &str, width: usize) -> Vec<String> {
    wrap_code_spans(text, width)
        .into_iter()
        .map(|row| row.text)
        .collect()
}

pub(super) fn wrap_code_spans(text: &str, width: usize) -> Vec<WrappedRow> {
    let width = width.max(12);
    if UnicodeWidthStr::width(text) <= width {
        return vec![WrappedRow {
            text: text.to_owned(),
            from: 0,
            to: text.len(),
            indent: 0,
        }];
    }
    // The continuation indent lines the links up under the head of the chain
    // and says, without a marker, that this is one statement.
    let indent = "  ";
    let mut breaks = Vec::new();
    let (mut quote, mut escaped) = (None::<char>, false);
    for (at, character) in text.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        match (quote, character) {
            (Some(_), '\\') => escaped = true,
            (Some(open), c) if c == open => quote = None,
            (Some(_), _) => {}
            (None, '"' | '\'') => quote = Some(character),
            // A dot that starts a method call, at any depth: a chain nested
            // inside `stack(...)` is still a chain and needs a seam. Never
            // the dot in a number.
            (None, '.') if at > 0 => {
                let before = text[..at].chars().next_back().unwrap_or(' ');
                let after = text[at + 1..].chars().next().unwrap_or(' ');
                if !before.is_ascii_digit() && after.is_alphabetic() {
                    breaks.push(at);
                }
            }
            // And after an argument separator, which is where a call taking
            // several patterns wants to break.
            (None, ',') => breaks.push(at + 1),
            (None, _) => {}
        }
    }
    let mut lines: Vec<WrappedRow> = Vec::new();
    let mut start = 0usize;
    let mut first = true;
    while start < text.len() {
        let room = width.saturating_sub(if first { 0 } else { indent.len() });
        // The furthest seam that still fits, or the next one if none does.
        let end = breaks
            .iter()
            .copied()
            .filter(|at| *at > start)
            .take_while(|at| UnicodeWidthStr::width(&text[start..*at]) <= room)
            .last()
            .or_else(|| breaks.iter().copied().find(|at| *at > start))
            .unwrap_or(text.len());
        // A call wider than the panel still needs wrapping. Split at a
        // character boundary when no chain break fits.
        let mut end = end;
        if UnicodeWidthStr::width(&text[start..end]) > room {
            let mut width_so_far = 0;
            end = start;
            for (at, character) in text[start..].char_indices() {
                let next = UnicodeWidthStr::width(character.to_string().as_str());
                if width_so_far + next > room && at > 0 {
                    break;
                }
                width_so_far += next;
                end = start + at + character.len_utf8();
            }
            end = end.max(start + text[start..].chars().next().map_or(1, char::len_utf8));
        }
        let piece = &text[start..end];
        // A break after `, ` leaves the space at the head of the next line,
        // where the indent already does that job.
        let trimmed = if first { piece } else { piece.trim_start() };
        let lead = piece.len() - trimmed.len();
        lines.push(WrappedRow {
            text: if first {
                trimmed.to_owned()
            } else {
                format!("{indent}{trimmed}")
            },
            from: start + lead,
            to: end,
            indent: if first { 0 } else { indent.len() },
        });
        first = false;
        start = end;
    }
    lines
}

/// Visible text of an inline-code string: backticks dropped, unmatched
/// openers kept as a literal tick.
pub(super) fn strip_inline_ticks(text: &str) -> String {
    parse_inline(text).0
}

pub(super) fn parse_inline(text: &str) -> (String, Vec<bool>) {
    let mut visible = String::new();
    let mut marks = Vec::new();
    let mut in_code = false;
    let mut code_from = 0usize;
    for character in text.chars() {
        if character == '`' {
            if in_code {
                in_code = false;
            } else {
                in_code = true;
                code_from = visible.chars().count();
            }
            continue;
        }
        visible.push(character);
        marks.push(in_code);
    }
    if in_code {
        let tail: String = visible.chars().skip(code_from).collect();
        visible = visible.chars().take(code_from).collect();
        marks.truncate(code_from);
        visible.push('`');
        marks.push(false);
        for character in tail.chars() {
            visible.push(character);
            marks.push(false);
        }
    }
    (visible, marks)
}

fn mark_ranges(marks: &[bool]) -> Vec<(usize, usize)> {
    let mut ranges = Vec::new();
    let mut index = 0;
    while index < marks.len() {
        if !marks[index] {
            index += 1;
            continue;
        }
        let start = index;
        while index < marks.len() && marks[index] {
            index += 1;
        }
        ranges.push((start, index));
    }
    ranges
}

fn char_width(character: char) -> usize {
    UnicodeWidthStr::width(character.encode_utf8(&mut [0; 4]) as &str).max(1)
}

/// Word-wrap that treats `` `code` `` as visible text (no ticks) and
/// remembers which characters were code, so the pane can lean them italic.
pub(super) fn wrap_inline(text: &str, width: usize) -> Vec<(String, Vec<(usize, usize)>)> {
    let width = width.max(8);
    let (visible, marks) = parse_inline(text);
    let chars: Vec<char> = visible.chars().collect();
    if chars.is_empty() {
        return vec![(String::new(), Vec::new())];
    }
    let mut tokens = Vec::new();
    let mut index = 0usize;
    while index < chars.len() {
        if chars[index].is_whitespace() {
            index += 1;
            continue;
        }
        let start = index;
        if marks[index] {
            while index < chars.len() && marks[index] {
                index += 1;
            }
        } else {
            while index < chars.len() && !chars[index].is_whitespace() && !marks[index] {
                index += 1;
            }
        }
        tokens.push((start, index));
    }
    let token_width =
        |from: usize, to: usize| -> usize { chars[from..to].iter().copied().map(char_width).sum() };
    let take_chars = |from: usize, room: usize| -> usize {
        let mut used = 0usize;
        let mut at = from;
        for i in from..chars.len() {
            let next = char_width(chars[i]);
            if used + next > room && i > from {
                break;
            }
            used += next;
            at = i + 1;
            if at == chars.len() {
                break;
            }
        }
        at.max(from + 1)
    };
    let flush = |tokens: &[(usize, usize)]| -> (String, Vec<(usize, usize)>) {
        let mut text = String::new();
        let mut line_marks = Vec::new();
        for (i, &(from, to)) in tokens.iter().enumerate() {
            if i > 0 {
                text.push(' ');
                let space_code = marks[tokens[i - 1].1 - 1] && marks[from];
                line_marks.push(space_code);
            }
            for j in from..to {
                text.push(chars[j]);
                line_marks.push(marks[j]);
            }
        }
        (text, mark_ranges(&line_marks))
    };

    let mut lines = Vec::new();
    let mut current: Vec<(usize, usize)> = Vec::new();
    let mut current_width = 0usize;
    for &(from, to) in &tokens {
        let wide = token_width(from, to);
        let gap = usize::from(!current.is_empty());
        if !current.is_empty() && current_width + gap + wide > width {
            lines.push(flush(&current));
            current.clear();
            current_width = 0;
        }
        if current.is_empty() && wide > width {
            let mut start = from;
            while start < to {
                let room = width;
                let mut end = take_chars(start, room).min(to);
                if end <= start {
                    end = (start + 1).min(to);
                }
                lines.push(flush(&[(start, end)]));
                start = end;
            }
            continue;
        }
        current.push((from, to));
        current_width += if current.len() == 1 { wide } else { gap + wide };
    }
    if !current.is_empty() {
        lines.push(flush(&current));
    }
    if lines.is_empty() {
        lines.push((String::new(), Vec::new()));
    }
    lines
}
