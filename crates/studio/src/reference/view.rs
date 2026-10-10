//! Reference tab rendering and shortcut hints.

use super::*;

/// How many other names a row shows before saying `+n`.
pub(super) const ALIASES_SHOWN: usize = 3;

/// An entry's other names for its row, the one the query matched first:
/// the synonyms that are not the name itself in another case, the first
/// few spelled out and the rest counted.
pub(super) fn aliases_text(entry: &Entry, query: &str) -> String {
    let lowercase = entry.name.to_lowercase();
    let mut aliases: Vec<&str> = entry
        .synonyms
        .iter()
        .map(String::as_str)
        .filter(|alias| alias.to_lowercase() != lowercase)
        .collect();
    aliases.dedup();
    if aliases.is_empty() {
        return String::new();
    }
    let query = query.trim().to_lowercase();
    if !query.is_empty()
        && let Some(hit) = aliases
            .iter()
            .position(|alias| alias.to_lowercase().starts_with(&query))
    {
        let matched = aliases.remove(hit);
        aliases.insert(0, matched);
    }
    let more = aliases.len().saturating_sub(ALIASES_SHOWN);
    let mut text = aliases
        .iter()
        .take(ALIASES_SHOWN)
        .copied()
        .collect::<Vec<_>>()
        .join(" ");
    if more > 0 {
        text.push_str(&format!(" +{more}"));
    }
    text
}

/// The shortcut hints at a tab's foot. Omit trailing segments that do not
/// fit so each visible hint stays complete.
fn render_footer(buffer: &mut Buffer, inner: Rect, y: u16, segments: &[&str], theme: &Theme) {
    if y < inner.y || y >= inner.bottom() || inner.width == 0 {
        return;
    }
    let width = usize::from(inner.width);
    let mut line = String::new();
    for segment in segments {
        let segment = super::super::keybinds::shortcut_label(segment);
        let candidate = if line.is_empty() {
            segment.into_owned()
        } else {
            format!("{line} · {segment}")
        };
        if candidate.chars().count() > width {
            break;
        }
        line = candidate;
    }
    buffer.set_stringn(inner.x, y, line, width, Style::default().fg(theme.muted));
}

/// The rendered label also determines where its forward history arrow is hit.
#[cfg(feature = "hydra")]
pub(super) fn generator_action_label(action: super::super::ideas::Action) -> String {
    match action {
        super::super::ideas::Action::Generate => {
            format!("{}  Generate", super::super::terminal::symbol("⟳"))
        }
        super::super::ideas::Action::Similar => "Similar".to_owned(),
    }
}

pub(super) fn sample_source_line(panel: &ReferencePanel) -> String {
    panel.selected_sample_source().map_or_else(
        || "source: select a sound".to_owned(),
        |source| format!("source: {source}"),
    )
}

impl ReferenceView<'_> {
    fn return_hint(&self) -> String {
        self.keybinds
            .binding(super::super::keybinds::BindAction::Reference)
            .map_or_else(
                || "click returns".to_owned(),
                |binding| format!("{} returns", binding.hint()),
            )
    }
}

impl Widget for ReferenceView<'_> {
    fn render(self, area: Rect, buffer: &mut Buffer) {
        if area.width < 12 || area.height < 4 {
            return;
        }
        let theme = self.theme;
        super::super::view::clear_overlay(
            buffer,
            area,
            Style::default().bg(theme.surface).fg(theme.foreground),
        );
        for y in area.y..area.bottom() {
            if let Some(cell) = buffer.cell_mut((area.x, y)) {
                cell.set_symbol("│").set_fg(theme.rule);
            }
        }
        let inner = inner_area(area);
        // The one marking a tab bar wears anywhere in the studio: accent
        // and bold. See the note in `devices.rs`.
        let tab_style = |active: bool| {
            if active {
                Style::default()
                    .fg(theme.accent)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(theme.muted)
            }
        };
        let (placed, hidden) = tab_layout(inner, self.panel.tab);
        for (tab, label, at) in &placed {
            buffer.set_stringn(
                *at,
                inner.y,
                label,
                usize::from(inner.right().saturating_sub(*at)),
                tab_style(self.panel.tab == *tab),
            );
        }
        // Count tabs hidden by the panel's width; Tab still reaches them.
        if hidden > 0 {
            let more = format!("+{hidden}");
            let at = inner.right().saturating_sub(more.len() as u16);
            buffer.set_stringn(
                at,
                inner.y,
                &more,
                more.len(),
                Style::default().fg(theme.accent),
            );
        }
        let hint = "Tab switches";
        let hint_x = inner.right().saturating_sub(hint.len() as u16);
        // Keep the hint clear of the last visible label.
        let hint_floor = placed.last().map_or(inner.x, |(_, label, at)| {
            at + UnicodeWidthStr::width(*label) as u16 + 1
        });
        if hidden == 0 && hint_x > hint_floor {
            buffer.set_stringn(
                hint_x,
                inner.y,
                hint,
                hint.len(),
                Style::default().fg(theme.muted),
            );
        }
        if self.panel.bank_list() {
            self.render_browse(inner, buffer);
            return;
        }
        match self.panel.tab {
            #[cfg(feature = "hydra")]
            Tab::Examples | Tab::Generator => self.render_snippets(inner, buffer),
            Tab::Samples => self.render_samples(inner, buffer),
            Tab::Chords => self.render_chords(inner, buffer),
            Tab::Scales => self.render_scales(inner, buffer),
            #[cfg(feature = "vst")]
            Tab::Vst => self.render_vst(inner, buffer),
            Tab::Reference => match &self.panel.mode {
                ReferenceMode::Browse => self.render_browse(inner, buffer),
                ReferenceMode::Entry { index, scroll, .. } => {
                    self.render_entry(*index, *scroll, inner, buffer)
                }
            },
        }
    }
}

impl ReferenceView<'_> {
    /// The example tree and the highlighted code that is about to be copied.
    #[cfg(feature = "hydra")]
    fn render_snippets(&self, inner: Rect, buffer: &mut Buffer) {
        use super::super::examples::SECTIONS;

        let theme = self.theme;
        let panel = self.panel;
        let layout = panel.snippet_layout(inner);
        let preview = layout.preview;

        // The studio paints the frame; reserve its box and show a reason until
        // a frame arrives, or when this example cannot be drawn.
        if preview.height > 0 {
            buffer.set_style(preview, Style::default().bg(theme.background));
            let (caption, colour) = match (
                panel.selected_snippet().is_some(),
                &self.refused,
                self.picture,
            ) {
                (false, _, _) => ("select a Hydra example", theme.muted),
                (true, Some(why), _) => (why.as_str(), theme.error),
                (true, None, false) => ("warming up…", theme.muted),
                (true, None, true) => ("", theme.muted),
            };
            if !caption.is_empty() && preview.height >= 3 && preview.width > 8 {
                for (line, text) in wrap(caption, usize::from(preview.width.saturating_sub(2)))
                    .into_iter()
                    .take(usize::from(preview.height).saturating_sub(1))
                    .enumerate()
                {
                    buffer.set_stringn(
                        preview.x + 1,
                        preview.y + 1 + line as u16,
                        text,
                        usize::from(preview.width.saturating_sub(2)),
                        Style::default().fg(colour),
                    );
                }
            }
        }
        let stacked: &str = if super::super::settings::examples_stack() {
            "s: one $:"
        } else {
            "s: stack()"
        };
        let return_hint = self.return_hint();
        let code_hint = if inner.width >= 38 {
            "PgUp/Dn code"
        } else {
            "PgUp/Dn"
        };
        let segments: &[&str] = if self.focused && panel.tab == Tab::Generator {
            if panel
                .generator_row()
                .and_then(super::super::ideas::Row::action)
                .is_some()
            {
                &[
                    "Space play",
                    "←→ history",
                    code_hint,
                    stacked,
                    "wheel scroll",
                ]
            } else {
                &[
                    "Space play",
                    "←→ adjust",
                    code_hint,
                    stacked,
                    "wheel scroll",
                ]
            }
        } else if self.focused {
            &[
                "↑↓ move",
                "←→ open",
                "Enter copies",
                "Space plays",
                stacked,
                "drag selects",
            ]
        } else {
            &[&return_hint, "Esc closes"]
        };
        render_footer(buffer, inner, layout.footer, segments, theme);
        if self.focused && panel.tab == Tab::Generator {
            render_footer(
                buffer,
                inner,
                layout.scope,
                &["g Generate", "v Similar", "c Copy"],
                theme,
            );
        }

        // The code panel sits below the example tree.
        let list = layout.list;
        if list.height == 0 {
            return;
        }

        let lines = panel.snippet_lines();
        let first = panel.first_visible_row(list.height);
        for (offset, line) in lines
            .iter()
            .skip(first)
            .take(usize::from(list.height))
            .enumerate()
        {
            let y = list.y + offset as u16;
            let chosen = first + offset == panel.snippet_selected;
            let (text, style) = match line {
                SnippetLine::Generator(row) => {
                    use super::super::ideas::Row;
                    let text = match row {
                        Row::Direction(direction) => format!(
                            "{} {}",
                            super::super::terminal::symbol(
                                if panel.generator.open && *direction == panel.generator.direction()
                                {
                                    "▾"
                                } else {
                                    "▸"
                                }
                            ),
                            direction.name()
                        ),
                        Row::Generate(_) | Row::Similar => {
                            let action = row.action().unwrap();
                            let back = if panel.generator.can_history(action, false) {
                                super::super::terminal::symbol("◀")
                            } else {
                                " "
                            };
                            // Like Theme Randomize, forward also makes another
                            // result once the newest history entry is reached.
                            let next = super::super::terminal::symbol("▶");
                            let label = generator_action_label(action);
                            format!("  {back} {label} {next}")
                        }
                        Row::Copy => "    Copy".into(),
                        Row::ControlsTop | Row::ControlsBottom => {
                            let top = matches!(row, Row::ControlsTop);
                            let left = if top { '┌' } else { '└' };
                            let right = if top { '┐' } else { '┘' };
                            format!(
                                "  {left}{}{right}",
                                "─".repeat(usize::from(list.width.saturating_sub(4)))
                            )
                        }
                        Row::Control(index) => {
                            format!("    {}", super::super::ideas::CONTROLS[*index])
                        }
                    };
                    let style = if matches!(row, Row::Generate(_) | Row::Similar | Row::Copy) {
                        Style::default()
                            .fg(theme.accent)
                            .add_modifier(Modifier::BOLD)
                    } else if matches!(row, Row::ControlsTop | Row::ControlsBottom) {
                        Style::default().fg(theme.rule)
                    } else {
                        Style::default().fg(theme.foreground)
                    };
                    (text, style)
                }

                SnippetLine::Section(index) => {
                    let section = &SECTIONS[*index];
                    let mark = if panel.section_open.contains(index) {
                        super::super::terminal::symbol("▾")
                    } else {
                        super::super::terminal::symbol("▸")
                    };
                    (
                        format!(
                            "{mark} {}  ({})",
                            section.name.to_uppercase(),
                            section.count()
                        ),
                        Style::default()
                            .fg(theme.accent)
                            .add_modifier(Modifier::BOLD),
                    )
                }
                SnippetLine::Shelf(section, index) => {
                    let shelf = &SECTIONS[*section].shelves[*index];
                    let mark = if panel.snippet_open.contains(&(*section, *index)) {
                        super::super::terminal::symbol("▾")
                    } else {
                        super::super::terminal::symbol("▸")
                    };
                    (
                        format!("  {mark}  {}  ({})", shelf.name, shelf.snippets.len()),
                        Style::default()
                            .fg(theme.foreground)
                            .add_modifier(Modifier::BOLD),
                    )
                }
                SnippetLine::Snippet(section, shelf, index) => (
                    format!(
                        "       {}",
                        SECTIONS[*section].shelves[*shelf].snippets[*index].name
                    ),
                    Style::default().fg(theme.foreground),
                ),
            };
            let style = if chosen {
                style.bg(theme.selection)
            } else {
                style
            };
            if chosen {
                buffer.set_style(
                    Rect::new(list.x, y, list.width, 1),
                    Style::default().bg(theme.selection),
                );
            }
            buffer.set_stringn(list.x, y, &text, usize::from(list.width), style);
            if let SnippetLine::Generator(super::super::ideas::Row::ControlsTop) = line
                && list.width >= 7
            {
                buffer.set_stringn(
                    list.x + 4,
                    y,
                    " Controls ",
                    usize::from(list.width.saturating_sub(6)),
                    Style::default().fg(theme.accent),
                );
            }
            if let SnippetLine::Generator(super::super::ideas::Row::Control(index)) = line {
                if list.width >= 4 {
                    buffer.set_string(list.x + 2, y, "│", Style::default().fg(theme.rule));
                    buffer.set_string(list.right() - 1, y, "│", Style::default().fg(theme.rule));
                }
                let rail = generator_rail(Rect::new(list.x, y, list.width, 1));
                let value = panel.generator.controls[*index];
                super::super::view::draw_slider_control(
                    buffer,
                    rail,
                    &super::super::view::SliderChip {
                        from: 0,
                        to: 0,
                        notch: f64::from(value) / 100.0,
                        armed: chosen,
                    },
                    theme,
                    theme.surface,
                    chosen,
                );
                if list.width >= 20 {
                    buffer.set_stringn(list.right() - 5, y, format!("{value:3}"), 3, style);
                }
            }

            // Preview status stays beside the playing snippet when the
            // cursor moves to another row.
            let generator_status = matches!(
                line,
                SnippetLine::Generator(super::super::ideas::Row::Generate(_))
            );
            let status = self
                .preview_note
                .as_ref()
                .filter(|(row, _, _)| *row == Some(*line));
            #[cfg(feature = "hydra")]
            if let Some((_, note, sounding)) = self
                .preview_note
                .as_ref()
                .filter(|(row, _, _)| *row == Some(*line))
            {
                let width = UnicodeWidthStr::width(note.as_str()) as u16;
                let tag_x = list.right().saturating_sub(width);
                let text_width = UnicodeWidthStr::width(text.as_str()) as u16;
                if tag_x > list.x + text_width {
                    let colour = if *sounding { theme.ok } else { theme.warn };
                    buffer.set_stringn(tag_x, y, note, usize::from(width), style.fg(colour));
                }
            }
            // The playhead strip rides the sounding preview's row - alone,
            // with or without a word beside it: eight cells, the fill
            // walking left to right through the bar. Where it stands is
            // where the pattern is in the music, and a bar already half
            // drawn under silence says the cycle moved on without an ear
            // to hear it.
            #[cfg(feature = "hydra")]
            if let Some(progress) = self
                .preview_progress
                .filter(|_| {
                    status.is_none()
                        && self
                            .preview_row
                            .as_ref()
                            .is_some_and(|row| *row == Some(*line))
                })
                .or_else(|| (generator_status && status.is_none()).then_some(0.0))
            {
                const STRIP: u16 = 8;
                let sounding = self
                    .preview_note
                    .as_ref()
                    .filter(|(row, _, _)| *row == Some(*line))
                    .map(|(_, _, sounding)| *sounding)
                    .unwrap_or(true);
                let text_width = UnicodeWidthStr::width(text.as_str()) as u16;
                let right = if generator_status {
                    list.right()
                } else {
                    list.right().saturating_sub(2)
                };
                let left = right.saturating_sub(STRIP);
                if left > list.x + text_width && right > left {
                    let width = right - left;
                    let filled = ((progress.clamp(0.0, 1.0) * width as f32) as u16).min(width);
                    let active = self
                        .preview_row
                        .as_ref()
                        .is_some_and(|row| *row == Some(*line));
                    for cell in left..right {
                        let on = cell - left < filled;
                        if let Some(cell) = buffer.cell_mut((cell, y)) {
                            cell.set_char(if on { '▓' } else { '░' })
                                .set_fg(if !active {
                                    theme.muted
                                } else if sounding {
                                    theme.ok
                                } else {
                                    theme.warn
                                });
                        }
                    }
                }
            }
        }

        // What the cursor is on, in full, ready to be taken. The code is
        // all the description a snippet needs: what it says is what it
        // does, and a line of prose above it was a row of shelf it ate.
        if layout.code.height == 0 {
            return;
        }
        if let Some(code) = panel.selected_snippet_code() {
            let (title, colour) = if panel.shows_picture() {
                match (&self.refused, self.picture) {
                    (Some(why), _) => (format!(" Code · {why} "), theme.error),
                    (None, false) => (" Code · warming up… ".to_owned(), theme.muted),
                    (None, true) => (" Code ".to_owned(), theme.muted),
                }
            } else {
                (" Code ".to_owned(), theme.muted)
            };
            ratatui::widgets::Block::bordered()
                .border_type(ratatui::widgets::BorderType::Rounded)
                .border_style(Style::default().fg(theme.rule))
                .style(Style::default().bg(theme.surface))
                .title(ratatui::text::Span::styled(
                    title,
                    Style::default().fg(colour),
                ))
                .render(layout.code, buffer);
            let rows = layout.code_rows();
            if rows.is_empty() {
                return;
            }
            // The snippet is playing and this is it: its marks are drawn
            // over its own text, so a preview lights up the way the score
            // does. Another row's snippet gets nothing - the sound is not
            // coming from it.
            #[cfg(feature = "hydra")]
            let marks = self
                .playing
                .filter(|(playing, _)| *playing == code.as_ref())
                .map(|(_, marks)| marks)
                .unwrap_or(&[]);
            let source_lines: Vec<_> = code.lines().collect();
            let mut offset = 0;
            let offsets: Vec<_> = source_lines
                .iter()
                .map(|line| {
                    let at = offset;
                    offset += line.len() + 1;
                    at
                })
                .collect();
            for (screen, (index, row)) in panel
                .snippet_code_rows(usize::from(rows.height), usize::from(rows.width))
                .into_iter()
                .enumerate()
            {
                let y = rows.y + screen as u16;
                let line = source_lines[index];
                let line_at = offsets[index];
                // Classify the source before taking its wrapped slice, so
                // continuation rows keep their string/comment/function colours.
                let indent = (row.indent as u16).min(rows.width);
                render_code_span(
                    buffer,
                    Rect::new(rows.x + indent, y, rows.width - indent, 1),
                    line,
                    row.from..row.to,
                    theme,
                );
                // Where this row's characters sit in the line, so a
                // highlight or a selection lands on them after the break.
                let column_of = |offset: usize| -> usize {
                    row.indent
                        + line[row.from..offset.clamp(row.from, row.to)]
                            .chars()
                            .count()
                };
                #[cfg(feature = "hydra")]
                for mark in marks
                    .iter()
                    .filter(|mark| mark.from < line_at + row.to && mark.to > line_at + row.from)
                {
                    let columns = column_of(mark.from.saturating_sub(line_at))
                        ..column_of(mark.to.saturating_sub(line_at));
                    if columns.start >= columns.end {
                        continue;
                    }
                    // Use the theme's event mark with this panel's surface
                    // colour, so fades return to the correct background.
                    paint_mark(
                        buffer,
                        rows.x,
                        y,
                        rows.width,
                        &row.text,
                        columns,
                        mark.color,
                        mark.strength,
                        theme,
                    );
                }
                if let Some(held) = &panel.selection
                    && let SelectionTarget::Snippet {
                        row: at_row,

                        width: at_width,
                    } = held.target
                    && at_row == panel.snippet_selected
                    && at_width == rows.width
                    && let Some(columns) = held
                        .selection
                        .columns_on(usize::from(y - rows.y), row.text.chars().count())
                {
                    // A drag is held in the screen's rows, which is what
                    // this is one of; a mark below is held in the line's
                    // bytes, which is why that one is remapped and this
                    // one is not.
                    if columns.start < columns.end {
                        super::super::textblock::paint_band(
                            buffer,
                            rows.x,
                            y,
                            rows.width,
                            &row.text,
                            columns,
                            theme.selection,
                        );
                    }
                }
            }
            if let Some((rail, top, length, _)) = panel.snippet_scrollbar(inner) {
                for row in 0..rail.height {
                    let on = row >= top && row < top + length;
                    buffer.set_string(
                        rail.x,
                        rail.y + row,
                        if on { "┃" } else { "│" },
                        Style::default().fg(if on { theme.muted } else { theme.rule }),
                    );
                }
            }
        }
    }

    /// Chord qualities and their twelve roots, with the selected chord on
    /// a keyboard and preview controls below.
    fn render_chords(&self, inner: Rect, buffer: &mut Buffer) {
        let theme = self.theme;
        buffer.set_stringn(
            inner.x,
            inner.y + 1,
            format!("search: {}", self.panel.chord_query),
            usize::from(inner.width),
            Style::default().fg(theme.foreground),
        );
        let rows = self.panel.chord_rows();
        let qualities = self.panel.chord_qualities();
        buffer.set_stringn(
            inner.x,
            inner.y + 2,
            format!("{} chords · 12 roots each", qualities.len()),
            usize::from(inner.width),
            Style::default().fg(theme.muted),
        );
        if let Some(pulse) = &self.pulse {
            render_samples_pulse(pulse, inner, self.panel.tab, buffer, theme);
        }
        let geometry = self.panel.geometry(inner);
        let list = geometry.list;
        // The keyboard takes the row above the pulse block, when there is
        // one to take.
        let keyboard_y = list.bottom();
        let (scope_y, _) = samples_pulse_rows(inner);
        for (row, chord_row) in rows
            .iter()
            .enumerate()
            .skip(geometry.first_row)
            .take(usize::from(list.height))
            .map(|(position, row)| (position - geometry.first_row, row))
        {
            let position = geometry.first_row + row;
            let selected = position == self.panel.chord_selected;
            let y = list.y + row as u16;
            let (text, muted) = match *chord_row {
                ChordRow::Quality(index) => {
                    let Some(quality) = qualities.get(index) else {
                        continue;
                    };
                    let marker = if self.panel.open_quality.as_deref() == Some(quality.symbol) {
                        super::super::terminal::symbol("▾")
                    } else {
                        super::super::terminal::symbol("▸")
                    };
                    (
                        format!(
                            " {marker} {} · {} · type {} ",
                            quality.name,
                            pretty_notation(quality.common),
                            quality.symbol
                        ),
                        false,
                    )
                }
                ChordRow::Chord(index, root) => {
                    let Some(quality) = qualities.get(index) else {
                        continue;
                    };
                    let chord = format!("{}{}", CHORD_ROOTS[root], quality.symbol);
                    let notes =
                        rustel_core::voicings::chord_notes(&chord, None, CHORD_PREVIEW_OCTAVE)
                            .unwrap_or_default()
                            .iter()
                            .map(|midi| super::super::visuals::note_name(*midi as f32))
                            .collect::<Vec<_>>()
                            .join(" ");
                    (
                        format!(
                            "     {}   {} ",
                            pretty_notation(&chord),
                            pretty_notation(&notes)
                        ),
                        true,
                    )
                }
            };
            let style = if selected {
                Style::default()
                    .fg(theme.selection_text)
                    .bg(theme.selection)
                    .add_modifier(Modifier::BOLD)
            } else if muted {
                Style::default().fg(theme.foreground)
            } else {
                Style::default()
                    .fg(theme.accent)
                    .add_modifier(Modifier::BOLD)
            };
            buffer.set_stringn(list.x, y, &text, usize::from(list.width), style);
        }
        if keyboard_y < scope_y {
            render_keyboard(
                buffer,
                inner,
                keyboard_y,
                scope_y - keyboard_y,
                self.panel.selected_chord().as_deref(),
                &self.panel.selected_chord_notes(),
                self.sounding_note,
                theme,
            );
        }
        if list.height > 0 {
            let return_hint = self.return_hint();
            let segments: &[&str] = if !self.focused {
                &[&return_hint, "Esc closes"]
            } else {
                &[
                    "↑↓ move",
                    "→ opens or plays",
                    "Space plays",
                    "← folds",
                    "Enter takes it",
                ]
            };
            render_footer(buffer, inner, inner.bottom() - 1, segments, theme);
        }
    }

    /// Scales and their twelve tonics, with the selected scale's notes on
    /// a keyboard and preview controls below.
    fn render_scales(&self, inner: Rect, buffer: &mut Buffer) {
        let theme = self.theme;
        buffer.set_stringn(
            inner.x,
            inner.y + 1,
            format!("search: {}", self.panel.scale_query),
            usize::from(inner.width),
            Style::default().fg(theme.foreground),
        );
        let names = self.panel.scale_names();
        buffer.set_stringn(
            inner.x,
            inner.y + 2,
            format!("{} scales · 12 tonics each", names.len()),
            usize::from(inner.width),
            Style::default().fg(theme.muted),
        );
        if let Some(pulse) = &self.pulse {
            render_samples_pulse(pulse, inner, self.panel.tab, buffer, theme);
        }
        let geometry = self.panel.geometry(inner);
        let list = geometry.list;
        let rows = self.panel.scale_rows();
        for (row, scale_row) in rows
            .iter()
            .enumerate()
            .skip(geometry.first_row)
            .take(usize::from(list.height))
            .map(|(position, row)| (position - geometry.first_row, row))
        {
            let position = geometry.first_row + row;
            let selected = position == self.panel.scale_selected;
            let y = list.y + row as u16;
            let (text, heading) = match *scale_row {
                ScaleRow::Scale(index) => {
                    let Some(name) = names.get(index) else {
                        continue;
                    };
                    let marker = if self.panel.open_scale.as_deref() == Some(name.as_str()) {
                        super::super::terminal::symbol("▾")
                    } else {
                        super::super::terminal::symbol("▸")
                    };
                    (format!(" {marker} {} ", pretty_notation(name)), true)
                }
                ScaleRow::Tonic(index, tonic) => {
                    let Some(name) = names.get(index) else {
                        continue;
                    };
                    let scale = format!("{}:{name}", CHORD_ROOTS[tonic]);
                    let notes = scale_notes(&scale)
                        .iter()
                        .take(8)
                        .map(|midi| super::super::visuals::note_name(*midi as f32))
                        .collect::<Vec<_>>()
                        .join(" ");
                    (
                        format!(
                            "     {}   {} ",
                            pretty_notation(&scale),
                            pretty_notation(&notes)
                        ),
                        false,
                    )
                }
            };
            let style = if selected {
                Style::default()
                    .fg(theme.selection_text)
                    .bg(theme.selection)
                    .add_modifier(Modifier::BOLD)
            } else if heading {
                Style::default()
                    .fg(theme.accent)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(theme.foreground)
            };
            buffer.set_stringn(list.x, y, &text, usize::from(list.width), style);
        }
        // The notes of the scale under the cursor, on the same keys the
        // chords tab lights.
        let keyboard_y = list.bottom();
        let (scope_y, _) = samples_pulse_rows(inner);
        if keyboard_y < scope_y {
            render_keyboard(
                buffer,
                inner,
                keyboard_y,
                scope_y - keyboard_y,
                self.panel.selected_scale().as_deref(),
                &self.panel.selected_scale_notes(),
                self.sounding_note,
                theme,
            );
        }
        if list.height > 0 {
            let return_hint = self.return_hint();
            let segments: &[&str] = if !self.focused {
                &[&return_hint, "Esc closes"]
            } else {
                &[
                    "↑↓ move",
                    "→ opens or plays",
                    "Space plays/stops",
                    "← folds",
                    "Enter takes it",
                ]
            };
            render_footer(buffer, inner, inner.bottom() - 1, segments, theme);
        }
    }

    /// The plugins the host found. The open plugin lists its parameters,
    /// its parameter groups and its presets under its row.
    #[cfg(feature = "vst")]
    fn render_vst(&self, inner: Rect, buffer: &mut Buffer) {
        use rustel_runtime::vst::Status;
        let theme = self.theme;
        let tab = &self.panel.vst;
        buffer.set_stringn(
            inner.x,
            inner.y + 1,
            format!("search: {}", tab.query()),
            usize::from(inner.width),
            Style::default().fg(theme.foreground),
        );
        let rows = tab.rows();
        let plugins = tab.plugins();
        let shown = rows
            .iter()
            .filter(|row| matches!(row, PluginRow::Plugin(_)))
            .count();
        let count = if plugins.is_empty() {
            "no plugin found · add a folder in settings, vst tab".to_owned()
        } else if rows.is_empty() {
            format!("no plugin named {:?}", tab.query().trim())
        } else {
            format!("{shown} of {} plugins", plugins.len())
        };
        buffer.set_stringn(
            inner.x,
            inner.y + 2,
            count,
            usize::from(inner.width),
            Style::default().fg(theme.muted),
        );
        let geometry = self.panel.geometry(inner);
        let list = geometry.list;
        let open = tab.open_plugin();
        let marker = |open: bool| super::super::terminal::symbol(if open { "▾" } else { "▸" });
        let heading = Style::default()
            .fg(theme.accent)
            .add_modifier(Modifier::BOLD);
        let foreground = Style::default().fg(theme.foreground);
        let muted = Style::default().fg(theme.muted);
        for (position, row) in rows
            .iter()
            .enumerate()
            .skip(geometry.first_row)
            .take(usize::from(list.height))
        {
            let y = list.y + (position - geometry.first_row) as u16;
            // The row from the left, piece by piece, and the tag at the
            // right edge.
            let (pieces, tag) = match *row {
                PluginRow::Plugin(index) => {
                    let plugin = &plugins[index];
                    let is_open = open.is_some_and(|open| open.name == plugin.name);
                    let tag = match &plugin.status {
                        // A bundle with no load and no test yet has no kind.
                        Status::Found if plugin.categories.is_empty() => (String::new(), muted),
                        Status::Loading => ("loading…".to_owned(), Style::default().fg(theme.warn)),
                        Status::Found | Status::Ready => {
                            let kind = if plugin.instrument {
                                "instrument"
                            } else {
                                "effect"
                            };
                            let facts = [kind, &plugin.vendor, &plugin.categories];
                            let facts: Vec<_> =
                                facts.into_iter().filter(|fact| !fact.is_empty()).collect();
                            (facts.join(" · "), muted)
                        }
                        Status::Failed(reason) => {
                            (reason.clone(), Style::default().fg(theme.error))
                        }
                    };
                    let text = format!(" {} {} ", marker(is_open), plugin.name);
                    (vec![(text, heading)], tag)
                }
                PluginRow::Group(index) => {
                    let (name, count) = &tab.groups()[index];
                    let text = format!("   {} {name} ", marker(tab.group_is_open(name)));
                    let style = foreground.add_modifier(Modifier::BOLD);
                    (vec![(text, style)], (count.to_string(), muted))
                }
                PluginRow::Param(index) => {
                    let Some(param) = open.and_then(|plugin| plugin.params.get(index)) else {
                        continue;
                    };
                    // A search lists the parameters flat and names the group
                    // at the right. An open group has its parameters one
                    // step in.
                    let (indent, tag) = if tab.searching() {
                        ("     ", param.group.clone())
                    } else if param.group.is_empty() {
                        ("     ", String::new())
                    } else {
                        ("       ", String::new())
                    };
                    let default = param.default_shown();
                    let pieces = vec![
                        (
                            format!("{indent}{}", param.key),
                            Style::default().fg(theme.ok),
                        ),
                        (format!("  {}", param.name), foreground),
                        (format!("  {default} "), muted),
                    ];
                    (pieces, (tag, muted))
                }
                PluginRow::Preset(index) => {
                    let pieces = vec![
                        ("     preset".to_owned(), muted),
                        (format!("  {} ", tab.presets()[index]), foreground),
                    ];
                    (pieces, (String::new(), muted))
                }
            };
            let selected = position == tab.selected;
            let mut x = list.x;
            for (text, style) in pieces {
                let style = if selected {
                    Style::default()
                        .fg(theme.selection_text)
                        .bg(theme.selection)
                        .add_modifier(Modifier::BOLD)
                } else {
                    style
                };
                buffer.set_stringn(x, y, &text, usize::from(list.right() - x), style);
                x = x
                    .saturating_add(UnicodeWidthStr::width(text.as_str()) as u16)
                    .min(list.right());
            }
            // A tag longer than the room loses its end, so a failure
            // reason still starts on the row.
            let (tag, style) = tag;
            let tag_width = UnicodeWidthStr::width(tag.as_str()) as u16;
            let tag_x = list.right().saturating_sub(tag_width + 1).max(x + 1);
            if tag_x < list.right() {
                buffer.set_stringn(tag_x, y, &tag, usize::from(list.right() - tag_x), style);
            }
        }
        if list.height > 0 {
            let return_hint = self.return_hint();
            let segments: &[&str] = if !self.focused {
                &[&return_hint, "Esc closes"]
            } else {
                &["↑↓ move", "→ loads or opens", "← folds", "Enter inserts"]
            };
            render_footer(buffer, inner, inner.bottom() - 1, segments, theme);
        }
    }

    /// Files the loader still has in its line, for the caching line under
    /// the count.
    fn render_samples(&self, inner: Rect, buffer: &mut Buffer) {
        let theme = self.theme;
        let query = format!("search: {}", self.panel.sound_query);
        buffer.set_stringn(
            inner.x,
            inner.y + 1,
            &query,
            usize::from(inner.width),
            Style::default().fg(theme.foreground),
        );
        // The way out of a full box, offered on the row it applies to. The
        // footer drops segments from the right and this one would never
        // survive there, which is precisely when it is wanted.
        if self.focused && !self.panel.sound_query.is_empty() {
            let hint = "^⌫ clears";
            let hint_width = UnicodeWidthStr::width(hint) as u16;
            let typed = UnicodeWidthStr::width(query.as_str()) as u16;
            let hint_x = inner.right().saturating_sub(hint_width);
            if hint_x > inner.x + typed {
                buffer.set_stringn(
                    hint_x,
                    inner.y + 1,
                    hint,
                    usize::from(hint_width),
                    Style::default().fg(theme.muted),
                );
            }
        }
        let from_score = self
            .panel
            .sounds
            .iter()
            .filter(|entry| entry.origin == SoundOrigin::Score)
            .count();
        // Distinguish a library still loading from a completed empty one.
        let count = if self.panel.sounds.is_empty() {
            match (self.importing, self.library_loading) {
                (0, false) => "no sounds".to_owned(),
                (0, true) => "reading the library…".to_owned(),
                (1, _) => "reading 1 import…".to_owned(),
                (many, _) => format!("reading {many} imports…"),
            }
        } else if let Some(missed) = self.panel.sounds_miss_line() {
            missed
        } else if from_score > 0 {
            format!(
                "{} of {} sounds · {from_score} from this score",
                self.panel.sound_results.len(),
                self.panel.sounds.len()
            )
        } else {
            format!(
                "{} of {} sounds",
                self.panel.sound_results.len(),
                self.panel.sounds.len()
            )
        };
        // A bar has to know its end, and nothing here does until every
        // manifest is in: a pre-cache learns how many files it is as it
        // goes. Download progress lives on Settings ▸ Samples and in the
        // header jobs chip - not on this catalogue line.
        let count = match self.importing {
            0 => count,
            _ if self.panel.sounds.is_empty() => count,
            1 => format!("{count} · reading 1 import…"),
            many => format!("{count} · reading {many} imports…"),
        };
        buffer.set_stringn(
            inner.x,
            inner.y + 2,
            count,
            usize::from(inner.width),
            Style::default().fg(theme.muted),
        );
        if let Some(pulse) = &self.pulse {
            render_samples_pulse(pulse, inner, self.panel.tab, buffer, theme);
        }
        let geometry = self.panel.geometry(inner);
        let list = geometry.list;
        let rows = self.panel.sound_rows();
        // Once for the pane, not once a row: the headers are named as a
        // set, since what makes one folder's name enough is the other
        // folders it is on screen with.
        let categories = self.panel.visible_categories();
        // Banks drawn in their source's place wear the source's colours.
        let lone_banks = self.panel.lone_banks();
        let section_names = section_labels(
            &categories
                .iter()
                .map(|(section, _)| section.clone())
                .collect::<Vec<_>>(),
        );
        let families = self.panel.sound_families();
        let family_label = |index: usize| {
            families
                .iter()
                .find(|family| family.members.contains(&index))
                .map(|family| family.label.as_str())
        };
        for (row, sound_row) in rows
            .iter()
            .enumerate()
            .skip(geometry.first_row)
            .take(usize::from(list.height))
            .map(|(position, row)| (position - geometry.first_row, row))
        {
            let position = geometry.first_row + row;
            let selected = position == self.panel.sound_selected;
            let y = list.y + row as u16;
            // The outermost group: a kind of sound, what it holds, and
            // whether it is open.
            if let SoundRow::Category(position) = *sound_row {
                let Some((category, count)) = categories.get(position) else {
                    continue;
                };
                let label = section_names
                    .get(position)
                    .map_or_else(|| category.label().to_owned(), Clone::clone);
                let marker = if self.panel.open_categories.contains(category) {
                    super::super::terminal::symbol("▾")
                } else {
                    super::super::terminal::symbol("▸")
                };
                // An import with nothing in it yet says why: on its way, or
                // refused.
                let pending = match category {
                    SoundSection::Import(spec) if *count == 0 => {
                        Some(self.panel.import_state(spec))
                    }
                    _ => None,
                };
                let detail = match pending {
                    Some(Some(SourceState::Failed(_))) => "could not be read".to_owned(),
                    Some(_) => "loading…".to_owned(),
                    None => count.to_string(),
                };
                let text = format!(" {marker} {label} · {detail} ");
                let style = if selected {
                    Style::default()
                        .fg(theme.selection_text)
                        .bg(theme.selection)
                        .add_modifier(Modifier::BOLD)
                } else if matches!(pending, Some(Some(SourceState::Failed(_)))) {
                    Style::default().fg(theme.error)
                } else if pending.is_some() {
                    Style::default().fg(theme.muted)
                } else {
                    Style::default()
                        .fg(theme.foreground)
                        .add_modifier(Modifier::BOLD)
                };
                buffer.set_stringn(list.x, y, &text, usize::from(list.width), style);
                continue;
            }
            let (text, origin) = match *sound_row {
                SoundRow::Category(_) => continue,
                SoundRow::Family(family) => {
                    let family = &families[family];
                    let marker = if self.panel.open_family.as_deref() == Some(family.key.as_str()) {
                        super::super::terminal::symbol("▾")
                    } else {
                        super::super::terminal::symbol("▸")
                    };
                    (
                        format!(
                            " {marker} {} · {} sounds ",
                            family.label,
                            family.members.len()
                        ),
                        None,
                    )
                }
                SoundRow::Bank(index) => {
                    let entry = &self.panel.sounds[index];
                    // A marker promises something to open. A synth and a
                    // one-sample bank have nothing inside them, so they
                    // wear none.
                    let marker = if !self.panel.bank_opens(index) {
                        " "
                    } else if self.panel.expanded == Some(index) {
                        super::super::terminal::symbol("▾")
                    } else {
                        super::super::terminal::symbol("▸")
                    };
                    // Inside its family the shared prefix goes without
                    // saying: `AkaiLinn_bd` reads `bd` under `AkaiLinn`.
                    // A rule down the left of the group says the rows
                    // belong together without spending a column on saying
                    // it. Indenting each level instead walks the names
                    // rightwards until a deep pack has no room left for
                    // them, and these trees are three and four deep.
                    let (indent, name) = match family_label(index) {
                        Some(label) => (
                            " │ ",
                            entry
                                .name
                                .strip_prefix(&format!("{label}_"))
                                .unwrap_or(&entry.name),
                        ),
                        None => (" ", entry.name.as_str()),
                    };
                    // A synth has no variants to count.
                    let text = if entry.origin == SoundOrigin::Synth {
                        format!("{indent}{marker} {name} ")
                    } else {
                        format!("{indent}{marker} {name} ({}) ", entry.variants)
                    };
                    if lone_banks.contains(&index) {
                        // In its source's place, it is drawn as the source.
                        let style = if selected {
                            Style::default()
                                .fg(theme.selection_text)
                                .bg(theme.selection)
                                .add_modifier(Modifier::BOLD)
                        } else {
                            Style::default()
                                .fg(theme.foreground)
                                .add_modifier(Modifier::BOLD)
                        };
                        buffer.set_stringn(list.x, y, &text, usize::from(list.width), style);
                        continue;
                    }
                    (text, Some(entry.origin))
                }
                SoundRow::Variant(index, variant) => {
                    let indent = if family_label(index).is_some() {
                        " │     "
                    } else {
                        "     "
                    };
                    (
                        format!(
                            "{indent}{} ",
                            self.panel.sounds[index].variant_label(variant)
                        ),
                        None,
                    )
                }
            };
            let style = if selected {
                Style::default()
                    .fg(theme.background)
                    .bg(theme.accent)
                    .add_modifier(Modifier::BOLD)
            } else if matches!(sound_row, SoundRow::Variant(..)) {
                Style::default().fg(theme.foreground)
            } else if matches!(sound_row, SoundRow::Family(_)) {
                Style::default()
                    .fg(theme.accent)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(theme.accent)
            };
            buffer.set_stringn(list.x, y, &text, usize::from(list.width), style);
            // The rule is the group's, not the row's: it recedes while the
            // names stay legible. On the selected row it keeps the
            // selection's own colours, so the band is not cut in half.
            if text.starts_with(" │") && !selected {
                buffer.set_stringn(list.x + 1, y, "│", 1, Style::default().fg(theme.rule));
            }
            let loading_here = self.loading.as_deref().is_some_and(|sound| {
                self.panel.sound_of(*sound_row, true).as_deref() == Some(sound)
            });
            let tag = if loading_here {
                // A little life while the sample fetches: the frame loop is
                // already ticking with the engine's snapshots.
                let spin = spinner_glyph();
                Some((if selected { "loading" } else { spin }, theme.warn))
            } else {
                match origin {
                    Some(SoundOrigin::Score) => Some(("score", theme.ok)),
                    Some(SoundOrigin::Font) => Some(("gm", theme.muted)),
                    Some(SoundOrigin::Synth) => Some(("synth", theme.accent)),
                    Some(SoundOrigin::Input) => Some(("input", theme.ok)),
                    _ => None,
                }
            };
            if let Some((tag, color)) = tag {
                let tag_x = list
                    .right()
                    .saturating_sub(UnicodeWidthStr::width(tag) as u16 + 1);
                let text_width = UnicodeWidthStr::width(text.as_str()) as u16;
                if tag_x > list.x + text_width {
                    buffer.set_stringn(tag_x, y, tag, tag.len(), Style::default().fg(color));
                }
            }
        }
        if list.height > 0 {
            if !self.focused {
                let return_hint = self.return_hint();
                render_footer(
                    buffer,
                    inner,
                    inner.bottom() - 1,
                    &[&return_hint, "Esc closes"],
                    theme,
                );
                return;
            }
            if let Some((_, _, path)) = &self.panel.confirm_delete {
                let name = path.file_name().unwrap_or_default().to_string_lossy();
                for (y, text) in [
                    (inner.bottom() - 3, "Delete permanently?"),
                    (inner.bottom() - 2, name.as_ref()),
                ] {
                    buffer.set_stringn(
                        inner.x,
                        y,
                        text,
                        usize::from(inner.width),
                        Style::default().fg(theme.muted),
                    );
                }
                render_footer(
                    buffer,
                    inner,
                    inner.bottom() - 1,
                    &["Enter deletes", "Esc cancels"],
                    theme,
                );
                return;
            }
            render_footer(
                buffer,
                inner,
                inner.bottom() - 2,
                &[
                    "↑↓ move",
                    "→ expand or preview",
                    "← folds",
                    "Space plays/stops",
                    "Esc stops too",
                    "Alt+A auto-play",
                    "Alt+± volume",
                ],
                theme,
            );
            let source_line = sample_source_line(self.panel);
            buffer.set_stringn(
                inner.x,
                inner.bottom() - 3,
                &source_line,
                usize::from(inner.width),
                Style::default().fg(theme.muted),
            );
            use super::super::keybinds::BindAction;
            let hint = |action, does: &str| format!("{} {does}", self.keybinds.hint(action));
            let show = hint(BindAction::ShowFile, "shows file");
            let rename_file = hint(BindAction::RenameFile, "renames file");
            let rename_bank = hint(BindAction::RenameFile, "bank alias");
            let trim = hint(BindAction::TrimSample, "trims");
            let delete = hint(BindAction::DeleteSample, "deletes file");
            let mut actions = vec![show.as_str()];
            if self.panel.selected_bank_is_editable() {
                match self.panel.rename() {
                    PanelAction::RenameSample { .. } => actions.push(&rename_file),
                    PanelAction::RenameBank { .. } => actions.push(&rename_bank),
                    _ => {}
                }
                actions.push(&trim);
                if self.panel.selected_sample_file().is_some() {
                    actions.push(&delete);
                }
            }
            actions.extend([
                match self.panel.intent {
                    PanelIntent::Insert => "Enter inserts",
                    PanelIntent::Copy => "Enter copies",
                },
                "/ categories",
                "right-click copies",
            ]);
            render_footer(buffer, inner, inner.bottom() - 1, &actions, theme);
        }
    }

    fn render_browse(&self, inner: Rect, buffer: &mut Buffer) {
        let theme = self.theme;
        let query = format!("search: {}", self.panel.query);
        buffer.set_stringn(
            inner.x,
            inner.y + 1,
            query,
            usize::from(inner.width),
            Style::default().fg(theme.foreground),
        );
        let count = match &self.panel.vocabulary {
            Some(vocabulary) => self.panel.banks_miss_line().unwrap_or_else(|| {
                let (subject, total) = if self.panel.compatible_banks_only() {
                    (
                        "compatible banks".to_owned(),
                        vocabulary
                            .bank_compatibility
                            .as_ref()
                            .expect("filtered banks")
                            .count,
                    )
                } else if vocabulary.details.iter().any(|detail| !detail.is_empty()) {
                    (
                        format!("{} choices", vocabulary.subject),
                        vocabulary.names.len(),
                    )
                } else {
                    (vocabulary.subject.to_owned(), vocabulary.names.len())
                };
                format!("{} of {} {}", self.panel.results.len(), total, subject)
            }),
            None => {
                let filter = TagFilter::parse(&self.panel.query);
                let tags = self.reference.tags();
                let offer = filter.name.is_some_and(|name| {
                    // Halfway through typing one, or having typed one that
                    // does not exist. Either way the useful answer is which
                    // tags there are, not that nothing matched.
                    name.is_empty()
                        || !tags.iter().any(|(tag, _)| {
                            tag.to_ascii_lowercase()
                                .starts_with(&name.to_ascii_lowercase())
                        })
                });
                if offer {
                    let names = tags
                        .iter()
                        .map(|(tag, _)| *tag)
                        .collect::<Vec<_>>()
                        .join(", ");
                    format!("tags: {names}")
                } else {
                    // Hidden entries are not in the count. The empty box
                    // says how many are hidden, so a short list does not
                    // look broken.
                    let hidden = self.reference.hidden_len();
                    let shown = self.reference.len() - hidden;
                    match filter.label() {
                        // Naming the filter is what tells a reader the short
                        // list is the whole answer and not a search that went
                        // badly.
                        Some(label) => format!(
                            "{} of {} functions · {label}",
                            self.panel.results.len(),
                            self.reference.len()
                        ),
                        // An empty box has nothing to count: the number is
                        // the same on both sides of the `of`. The row
                        // names the `tag:` filter instead, so a reader can
                        // find it.
                        None if self.panel.query.is_empty() && hidden > 0 => {
                            format!("{shown} functions · {hidden} hidden · type tag: to narrow")
                        }
                        None if self.panel.query.is_empty() => {
                            format!("{shown} functions · type tag: to narrow by kind")
                        }
                        None => format!("{} of {shown} functions", self.panel.results.len()),
                    }
                }
            }
        };
        buffer.set_stringn(
            inner.x,
            inner.y + 2,
            count,
            usize::from(inner.width),
            Style::default().fg(theme.muted),
        );
        let geometry = self.panel.geometry(inner);
        let list = geometry.list;
        for (row, browse_row) in self
            .panel
            .browse_rows()
            .iter()
            .enumerate()
            .skip(geometry.first_row)
            .take(usize::from(list.height))
            .map(|(at, browse_row)| (at - geometry.first_row, browse_row))
        {
            let y = list.y + row as u16;
            // The tag a run of results is filed under, over the first of
            // them: the samples tab's heading without its fold marker,
            // because nothing folds here.
            if let BrowseRow::Tag(position) = *browse_row {
                if let Some(entry) = self
                    .panel
                    .results
                    .get(position)
                    .and_then(|&index| self.reference.entry(index))
                {
                    buffer.set_stringn(
                        list.x,
                        y,
                        format!(" {} ", entry.heading()),
                        usize::from(list.width),
                        Style::default()
                            .fg(theme.foreground)
                            .add_modifier(Modifier::BOLD),
                    );
                }
                continue;
            }
            let BrowseRow::Entry(position) = *browse_row else {
                continue;
            };
            let Some(&index) = self.panel.results.get(position) else {
                continue;
            };
            let (name, summary, snippet, aliases) = match &self.panel.vocabulary {
                Some(vocabulary) => match vocabulary.names.get(index) {
                    Some(name) => (
                        name.as_str(),
                        vocabulary
                            .details
                            .get(index)
                            .map(String::as_str)
                            .unwrap_or(""),
                        false,
                        String::new(),
                    ),
                    None => continue,
                },
                None => match self.reference.entry(index) {
                    Some(entry) => (
                        entry.name.as_str(),
                        entry.summary.as_str(),
                        entry.snippet.is_some(),
                        aliases_text(entry, &self.panel.query),
                    ),
                    None => continue,
                },
            };
            let selected = geometry.first_row + row == self.panel.selected;
            let name_style = if selected {
                Style::default()
                    .fg(theme.background)
                    .bg(theme.accent)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(theme.accent)
            };
            // A colour's row wears the colour: a swatch, then the name in
            // it. Reading "steelblue" tells you less than seeing it.
            let swatch = self
                .panel
                .vocabulary
                .as_ref()
                .filter(|vocabulary| vocabulary.swatches)
                .and_then(|_| super::super::theme::parse_color(name));
            let mut x = list.x;
            if let Some(colour) = swatch {
                buffer.set_stringn(
                    x,
                    y,
                    " ██",
                    usize::from(list.width),
                    Style::default().fg(colour),
                );
                x = x.saturating_add(3);
            }
            let name_style = match swatch {
                Some(colour) if !selected => Style::default().fg(colour),
                _ => name_style,
            };
            let name = format!(" {name} ");
            let room = usize::from(list.right().saturating_sub(x));
            buffer.set_stringn(x, y, &name, room, name_style);
            let name_width = UnicodeWidthStr::width(name.as_str()) as u16;
            let mut summary_x = x.saturating_add(name_width + 1);
            // The other names the thing goes by, muted beside the name -
            // the one the search matched first - so `bpe` typed shows
            // `bpenv` with `bpe` right beside it, and the hit makes sense.
            if !aliases.is_empty() && summary_x < list.right() {
                let room = usize::from(list.right() - summary_x);
                let aliases = elide(&aliases, room);
                buffer.set_stringn(
                    summary_x,
                    y,
                    &aliases,
                    room,
                    Style::default()
                        .fg(theme.muted)
                        .add_modifier(Modifier::ITALIC),
                );
                summary_x =
                    summary_x.saturating_add(UnicodeWidthStr::width(aliases.as_str()) as u16 + 1);
            }
            // A snippet says so on its row: it is lines to paste, not a
            // function, and Enter will do something different on it.
            if snippet && summary_x < list.right() {
                let label = "snippet";
                buffer.set_stringn(
                    summary_x,
                    y,
                    label,
                    usize::from(list.right() - summary_x),
                    Style::default().fg(theme.ok).add_modifier(Modifier::ITALIC),
                );
                summary_x = summary_x.saturating_add(label.len() as u16 + 1);
            }
            if !summary.is_empty() && summary_x < list.right() {
                let room = usize::from(list.right() - summary_x);
                let summary = &elide(&strip_inline_ticks(summary), room);
                buffer.set_stringn(
                    summary_x,
                    y,
                    summary,
                    room,
                    Style::default().fg(if selected {
                        theme.foreground
                    } else {
                        theme.muted
                    }),
                );
            }
        }
        if list.height > 0 {
            let return_hint = self.return_hint();
            let segments: &[&str] = if !self.focused {
                // The way back leads, so a narrow column keeps it.
                &[&return_hint, "Esc closes"]
            } else if self.panel.compatible_banks_only() && self.panel.query.is_empty() {
                &[
                    "Backspace shows all",
                    "Enter replaces the word",
                    "↑↓ move",
                    "Esc closes",
                ]
            } else if self.panel.vocabulary.is_some() {
                &["↑↓ move", "Enter replaces the word", "Esc closes"]
            } else if self.panel.quick {
                &[
                    "↑↓ move",
                    "→ reads it",
                    "Enter replaces the word",
                    "Esc closes",
                ]
            } else {
                // The column is narrow and `render_footer` drops segments
                // from the right, so the filter hint goes last: it is the
                // one a reader can also find in the docs.
                &["↑↓ move", "→ or Enter opens", "Esc closes", "tag:… filters"]
            };
            render_footer(buffer, inner, inner.bottom() - 1, segments, theme);
        }
    }

    fn render_entry(&self, index: usize, scroll: u16, inner: Rect, buffer: &mut Buffer) {
        let theme = self.theme;
        let Some(entry) = self.reference.entry(index) else {
            return;
        };
        let width = usize::from(inner.width);
        // Drawing consumes the same lines hit-testing and copying read, from
        // the one producer, so the three can never disagree about what sits
        // on a row.
        let lines = entry_body(entry, width);
        let body = entry_body_area(inner);
        let max_scroll = lines.len().saturating_sub(usize::from(body.height)) as u16;
        let scroll = scroll.min(max_scroll);
        for (row, line) in lines
            .iter()
            .enumerate()
            .skip(usize::from(scroll))
            .take(usize::from(body.height))
            .map(|(position, line)| (position - usize::from(scroll), line))
        {
            let y = body.y + row as u16;
            let position = row + usize::from(scroll);
            if line.kind == BodyKind::Example {
                render_code_line(buffer, body.x, y, body.width, &line.text, theme);
            } else {
                let style = match line.kind {
                    BodyKind::Signature => Style::default()
                        .fg(theme.accent)
                        .add_modifier(Modifier::BOLD),
                    BodyKind::Synonyms | BodyKind::ParamDetail => Style::default().fg(theme.muted),
                    BodyKind::Deprecated | BodyKind::Origin => Style::default().fg(theme.warn),
                    BodyKind::Terminal => Style::default().fg(theme.accent),
                    BodyKind::Param => Style::default().fg(theme.ok),
                    BodyKind::Prose => Style::default().fg(theme.foreground),
                    BodyKind::Blank | BodyKind::Example => Style::default(),
                };
                if line.code.is_empty() {
                    buffer.set_stringn(body.x, y, &line.text, width, style);
                } else {
                    render_inline_line(
                        buffer, body.x, y, body.width, &line.text, &line.code, style,
                    );
                }
            }
            // The dragged band, background only, over whatever the row drew:
            // the syntax colours keep their say.
            if let Some(held) = &self.panel.selection
                && let SelectionTarget::Entry {
                    index: held_index,
                    width: held_width,
                } = held.target
                && held_index == index
                && held_width == inner.width
                && let Some(columns) = held
                    .selection
                    .columns_on(position, line.text.chars().count())
            {
                super::super::textblock::paint_band(
                    buffer,
                    body.x,
                    y,
                    body.width,
                    &line.text,
                    columns,
                    theme.selection,
                );
            }
        }
        let return_hint = self.return_hint();
        let segments: &[&str] = if !self.focused {
            &[&return_hint, "Esc closes"]
        } else {
            match self.panel.mode {
                ReferenceMode::Entry {
                    from_browse: true, ..
                } => &[
                    "↑↓ scroll",
                    "← the list",
                    "drag selects, c copies",
                    "Enter inserts",
                    "Esc back",
                ],
                _ => &[
                    "↑↓ scroll",
                    "← the list",
                    "drag selects, c copies",
                    "Enter inserts",
                    "Esc closes",
                ],
            }
        };
        render_footer(buffer, inner, inner.bottom() - 1, segments, theme);
    }
}
