//! Rendering for the settings sheet.

use super::*;

impl Widget for SettingsSheetView<'_> {
    fn render(self, area: Rect, buffer: &mut Buffer) {
        let Some((panel, rows)) = self.sheet.geometry_for(area) else {
            return;
        };
        let theme = self.theme;
        super::super::view::clear_overlay(
            buffer,
            panel,
            Style::default().bg(theme.overlay).fg(theme.foreground),
        );
        draw_border(buffer, panel, theme);
        let tab_style = |page| {
            if self.sheet.page == page {
                Style::default()
                    .fg(theme.accent)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(theme.muted)
            }
        };
        buffer.set_stringn(
            panel.x + 2,
            panel.y,
            " settings ",
            10,
            tab_style(SettingsPage::Settings),
        );
        buffer.set_stringn(
            panel.x + 13,
            panel.y,
            " advanced ",
            10,
            tab_style(SettingsPage::Advanced),
        );
        buffer.set_stringn(
            panel.x + 24,
            panel.y,
            " mapping ",
            9,
            tab_style(SettingsPage::Mapping),
        );
        buffer.set_stringn(
            panel.x + 34,
            panel.y,
            " keybinds ",
            10,
            tab_style(SettingsPage::Keybinds),
        );
        buffer.set_stringn(
            panel.x + 45,
            panel.y,
            " samples ",
            9,
            tab_style(SettingsPage::Sources),
        );
        buffer.set_stringn(
            panel.x + 56,
            panel.y,
            " about ",
            7,
            tab_style(SettingsPage::About),
        );
        if self.sheet.page == SettingsPage::About {
            render_about(&self, panel, rows, buffer);
            return;
        }
        if self.sheet.page == SettingsPage::Mapping {
            render_mapping(&self, panel, rows, buffer);
            return;
        }
        if self.sheet.page == SettingsPage::Keybinds {
            render_keybinds(&self, panel, rows, buffer);
            return;
        }
        if self.sheet.page == SettingsPage::Sources {
            render_sources(&self, panel, rows, buffer);
            return;
        }
        let width = usize::from(rows.width);
        let shown = usize::from(rows.height).max(1);
        let list = self.sheet.rows();
        let label_width = list
            .iter()
            .map(|row| row.label().len())
            .max()
            .unwrap_or(14)
            .max(14);
        let display = grouped_rows(list);
        let first = self.sheet.first_line(shown, &display);
        for (offset, display_row) in display.iter().skip(first).take(shown).enumerate() {
            let y = rows.y + offset as u16;
            let index = match display_row {
                DisplayRow::Header { label, first } => {
                    if offset + 1 == shown {
                        continue;
                    }
                    let rule = if *first {
                        GroupRule::Opens(label)
                    } else {
                        GroupRule::Between(label)
                    };
                    draw_group_border(buffer, rows, y, rule, theme);
                    continue;
                }
                DisplayRow::Footer => {
                    draw_group_border(buffer, rows, y, GroupRule::Closes, theme);
                    continue;
                }
                DisplayRow::Gap => continue,
                DisplayRow::Control(index) => *index,
            };
            let row = &list[index];
            let border = Style::default().fg(theme.muted);
            buffer.set_stringn(rows.x, y, "│", 1, border);
            buffer.set_stringn(rows.right() - 1, y, "│", 1, border);
            let content_x = rows.x + 1;
            let content_width = width.saturating_sub(2);
            let selected = index == self.sheet.selected;
            let value = match row {
                Row::ShowFullPaths => on_off(self.settings.show_full_paths),
                Row::Rendering if !self.settings.rendering.supported(self.features) => {
                    "Automatic (fallback)".to_owned()
                }
                Row::Rendering => self.settings.rendering.label().to_owned(),
                Row::FrameRate => self.settings.frame_rate.label().to_owned(),
                Row::Quantise => self.settings.quantise.label(),
                Row::LoadMode => self.settings.load_mode.label().to_owned(),
                Row::Animation => on_off(self.settings.animation),
                Row::Highlights => on_off(self.settings.highlights),
                Row::EvaluationFlash => self.settings.evaluation_flash.label().to_owned(),
                Row::TrimRecordings => on_off(self.settings.trim_recordings),
                Row::HighlightFade => self.settings.highlight_fade.label().to_owned(),
                Row::MetricDetail => self.settings.metric_detail.label().to_owned(),
                Row::PianoSound => self.settings.piano_sound.key().to_owned(),
                Row::PianoVolume => format!("{}%", self.settings.piano_volume),
                Row::OutputLatency => output_latency_readout(
                    self.settings.output_latency,
                    self.device.map(|device| device.audio.output()),
                ),
                // The switch says what new sets get. The open set's own
                // limiter is said beside it, because a player reading a
                // row that says `off` while a limiter is plainly working
                // deserves to be told which of the two they are looking
                // at.
                Row::MasterLimiter => {
                    let default = on_off(self.settings.master_limiter_on);
                    match &self.set_limiter {
                        Some(set) => format!("{default} \u{b7} set {set}"),
                        None => default,
                    }
                }
                Row::MasterLimiterCharacter => {
                    self.settings.master_limiter_character.key().to_owned()
                }
                Row::MasterLimiterMakeup => on_off(self.settings.master_limiter_makeup),
                Row::MasterLimiterCeiling => {
                    format!("{:.1} dBFS", self.settings.master_limiter_ceiling_db)
                }
                Row::Minimap => on_off(self.settings.minimap),
                Row::ShowScrollbars => on_off(self.settings.show_scrollbars),
                Row::LineNumbers => on_off(self.settings.line_numbers),
                Row::Wrap => on_off(self.settings.wrap),
                Row::MasterScope => on_off(self.settings.master_scope),
                Row::Brackets => on_off(self.settings.brackets),
                Row::CaretShape => self.settings.caret_shape.label().to_owned(),
                Row::SyntaxCheck => self.settings.syntax_check.label().to_owned(),
                Row::SliderSmoothing => on_off(self.settings.slider_smoothing),
                Row::FrequencySliderLog => {
                    if self.settings.frequency_slider_log {
                        "Log".to_owned()
                    } else {
                        "Linear".to_owned()
                    }
                }
                #[cfg(feature = "hydra")]
                Row::BackdropSmoothing => on_off(self.settings.backdrop_smoothing),
                #[cfg(feature = "hydra")]
                Row::HydraWebcam => self.hydra_webcam.as_ref().map_or_else(
                    || {
                        if self.settings.hydra_webcam {
                            "allowed".to_owned()
                        } else {
                            "blocked".to_owned()
                        }
                    },
                    |status| status.state.label().to_owned(),
                ),
                Row::BackdropStrength => format!("{}%", self.settings.backdrop_opacity),
                Row::InterfaceOpacity => format!("{}%", self.settings.interface_opacity),
                Row::EditorOpacity => format!("{}%", self.settings.editor_opacity),
                Row::ShowMenu => on_off(self.settings.show_menu),
                Row::ShowHeader => on_off(self.settings.show_header),
                Row::ShowFooter => on_off(self.settings.show_footer),
                Row::Zen => on_off(self.settings.zen),
                Row::VizEdgeOne => self.settings.viz_edges[0].name().to_owned(),
                Row::VizEdgeTwo => self.settings.viz_edges[1].name().to_owned(),
                Row::SetPanelSide => {
                    if self.settings.set_panel_right {
                        "right".to_owned()
                    } else {
                        "left".to_owned()
                    }
                }
                Row::MixerEdge => {
                    if self.settings.mixer_top {
                        "top".to_owned()
                    } else {
                        "bottom".to_owned()
                    }
                }
                Row::SetsFolder => self.sets_folder.clone(),
                Row::RecordingsFolder => self.recordings_folder.clone(),
                Row::GlobalPrebake => self.prebakes[PrebakeScope::Global.index()].value(),
                Row::LocalPrebake => self.prebakes[PrebakeScope::Local.index()].value(),
                Row::SampleCeiling => format!(
                    "{} · ~{:.0} min",
                    self.settings.sample_ceiling.label(),
                    self.settings.sample_ceiling.minutes()
                ),
                Row::MaxPolyphony => {
                    max_polyphony_readout(self.settings.max_polyphony, self.max_polyphony_override)
                }
                Row::PreviewBudget => self.settings.preview_budget.label().to_owned(),
                Row::UnusedSampleIdle => self.settings.unused_sample_idle.label().to_owned(),
                Row::PrecacheSources => on_off(self.settings.precache_sources),
                Row::CacheDefaults => match &self.precache {
                    Some(progress)
                        if progress.kind == PrecacheKind::Library && !progress.done() =>
                    {
                        progress.count()
                    }
                    _ => String::new(),
                },
                Row::RefreshSources => String::new(),
                Row::SampleCache => self.sample_cache.clone(),
            };
            let marker = if selected {
                format!("{} ", super::super::terminal::symbol("▸"))
            } else {
                "  ".to_owned()
            };
            let default_explain = row.explain();
            #[cfg(feature = "hydra")]
            let explain = if *row == Row::HydraWebcam {
                self.hydra_webcam
                    .as_ref()
                    .map(|status| status.detail.as_str())
                    .unwrap_or(default_explain)
            } else {
                default_explain
            };
            #[cfg(not(feature = "hydra"))]
            let explain = default_explain;
            // The value column is a column: a long value is trimmed to it,
            // never allowed to shove the explanation off its x and overlap
            // the copy drawn there.
            let value: String = if value.chars().count() > 22 {
                let mut cut: String = value.chars().take(21).collect();
                cut.push('…');
                cut
            } else {
                value
            };
            let line = format!(
                "{marker}{:<label_width$} {:<22} {}",
                row.label(),
                value,
                explain
            );
            let style = if selected {
                Style::default()
                    .fg(theme.foreground)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(theme.foreground)
            };
            buffer.set_stringn(content_x, y, &line, content_width, style);
            if !selected {
                // The explanation reads quieter than the switch, exactly over
                // the copy the line above carries: the column arithmetic must
                // mirror the format! - the marker's two cells, the shared
                // label width, and the 22-cell value column - or the overlay
                // lands early and the tail of the first copy pokes out past
                // it in the louder colour.
                let explain_x = content_x + (2 + label_width + 1 + 22 + 1) as u16;
                buffer.set_stringn(
                    explain_x,
                    y,
                    explain,
                    content_width.saturating_sub(usize::from(explain_x - content_x)),
                    Style::default().fg(theme.muted),
                );
            }
        }
        #[cfg(feature = "hydra")]
        if self.sheet.webcam_preview_open()
            && let Some(status) = self.hydra_webcam.as_ref()
        {
            render_webcam_status(status, panel, rows, buffer, theme);
        }
        #[cfg(feature = "hydra")]
        let webcam_preview = self.sheet.webcam_preview_open() && self.hydra_webcam.is_some();
        #[cfg(not(feature = "hydra"))]
        let webcam_preview = false;
        if !webcam_preview && let Some(row) = list.get(self.sheet.selected) {
            let top = rows.bottom().saturating_add(1);
            let height = panel.bottom().saturating_sub(2).saturating_sub(top);
            for (offset, line) in wrap_words(row.explain(), width)
                .into_iter()
                .take(usize::from(height))
                .enumerate()
            {
                buffer.set_stringn(
                    rows.x,
                    top + offset as u16,
                    line,
                    width,
                    Style::default().fg(theme.muted),
                );
            }
        }
        // What the terminal is and can do is on the About tab only.
        //
        // How much of the page is off screen goes here. The sheet scrolls
        // silently: the last row drawn looks like the last row there is,
        // so this count shows that more rows exist.
        let hidden_above = first;
        let hidden_below = display.len().saturating_sub(first + shown);
        let more = match (hidden_above, hidden_below) {
            (0, 0) => String::new(),
            (0, below) => format!(" · ↓ {below} more"),
            (above, 0) => format!(" · ↑ {above} more"),
            (above, below) => format!(" · ↑ {above} · ↓ {below} more"),
        };
        #[cfg(feature = "hydra")]
        let footer = if self.sheet.webcam_preview_open() && !self.settings.hydra_webcam {
            // The picture is up and the camera is still off: say the keys
            // that choose and the keys that put it away, and nothing else,
            // the way an armed cache-empty does.
            "←/→ switch the camera on · Enter or Esc puts the picture away".to_owned()
        } else {
            format!("Tab pages · ↑/↓ select · ←/→/Space change · Enter choose · Esc close{more}")
        };
        #[cfg(not(feature = "hydra"))]
        let footer =
            format!("Tab pages · ↑/↓ select · ←/→/Space change · Enter choose · Esc close{more}");
        buffer.set_stringn(
            rows.x,
            panel.bottom().saturating_sub(2),
            footer,
            width,
            Style::default().fg(theme.muted),
        );
    }
}

fn draw_group_border(buffer: &mut Buffer, area: Rect, y: u16, rule: GroupRule<'_>, theme: &Theme) {
    let (left, right) = match rule {
        GroupRule::Opens(_) => ('┌', '┐'),
        GroupRule::Between(_) => ('├', '┤'),
        GroupRule::Closes => ('└', '┘'),
    };
    let title = rule.title();
    let line = format!(
        "{left}{}{right}",
        "─".repeat(usize::from(area.width.saturating_sub(2)))
    );
    buffer.set_stringn(
        area.x,
        y,
        line,
        usize::from(area.width),
        Style::default().fg(theme.muted),
    );
    if let Some(title) = title {
        buffer.set_stringn(
            area.x + 2,
            y,
            format!(" {title} "),
            usize::from(area.width.saturating_sub(4)),
            Style::default().fg(theme.accent).bg(theme.overlay),
        );
    }
}

#[cfg(feature = "hydra")]
fn render_webcam_status(
    status: &rustel_runtime::hydra::HydraWebcamStatus,
    panel: Rect,
    rows: Rect,
    buffer: &mut Buffer,
    theme: &Theme,
) {
    use ratatui::style::Color;

    // Instance geometry reserves enough room for the selected control and
    // an approximately square image at the panel's current size.
    let top = rows.bottom().saturating_add(1);
    let bottom = panel.bottom().saturating_sub(2);
    let height = bottom.saturating_sub(top);
    let columns = height.saturating_mul(2);
    if height == 0 || rows.width < columns + 12 {
        return;
    }
    let preview_x = rows.right().saturating_sub(columns);
    let text_width = usize::from(preview_x.saturating_sub(rows.x + 2));
    for (offset, line) in wrap_words(&status.detail, text_width)
        .into_iter()
        .take(usize::from(height))
        .enumerate()
    {
        buffer.set_stringn(
            rows.x,
            top + offset as u16,
            line,
            text_width,
            Style::default().fg(theme.foreground),
        );
    }

    if let Some(preview) = status.preview.as_ref()
        && preview.width > 0
        && preview.height > 0
        && preview.rgb.len() == usize::from(preview.width) * usize::from(preview.height) * 3
    {
        let source_width = usize::from(preview.width);
        let source_height = usize::from(preview.height);
        for cell_y in 0..height {
            for cell_x in 0..columns {
                let sample = |x: usize, y: usize, width: usize, height: usize| {
                    let source_x = x * source_width / width;
                    let source_y = y * source_height / height;
                    let at = (source_y * source_width + source_x) * 3;
                    [preview.rgb[at], preview.rgb[at + 1], preview.rgb[at + 2]]
                };
                if let Some(cell) = buffer.cell_mut((preview_x + cell_x, top + cell_y)) {
                    let [r, g, b] = sample(
                        usize::from(cell_x),
                        usize::from(cell_y) * 2,
                        usize::from(columns),
                        usize::from(height) * 2,
                    );
                    cell.set_char('▀');
                    cell.set_fg(Color::Rgb(r, g, b));
                    let [r, g, b] = sample(
                        usize::from(cell_x),
                        usize::from(cell_y) * 2 + 1,
                        usize::from(columns),
                        usize::from(height) * 2,
                    );
                    cell.set_bg(Color::Rgb(r, g, b));
                }
            }
        }
        return;
    }

    let (label, colour) = match status.state {
        rustel_runtime::hydra::HydraWebcamState::Blocked => ("off", theme.muted),
        rustel_runtime::hydra::HydraWebcamState::Allowed => ("idle", theme.muted),
        rustel_runtime::hydra::HydraWebcamState::Requested => ("wait", theme.warn),
        rustel_runtime::hydra::HydraWebcamState::Opening => ("open", theme.warn),
        rustel_runtime::hydra::HydraWebcamState::Ready => ("ready", theme.ok),
        rustel_runtime::hydra::HydraWebcamState::Error => ("error", theme.error),
    };
    for y in 0..height {
        let line = match y {
            0 => format!("┌{}┐", "─".repeat(usize::from(columns.saturating_sub(2)))),
            y if y + 1 == height => {
                format!("└{}┘", "─".repeat(usize::from(columns.saturating_sub(2))))
            }
            _ => format!("│{}│", " ".repeat(usize::from(columns.saturating_sub(2)))),
        };
        buffer.set_stringn(
            preview_x,
            top + y,
            line,
            usize::from(columns),
            Style::default().fg(colour),
        );
    }
    if height >= 3 {
        let x = preview_x + (columns.saturating_sub(label.len() as u16)) / 2;
        buffer.set_stringn(x, top + 1, label, label.len(), Style::default().fg(colour));
    }
}

/// The twelve slots, as boxes. A slot says what drives it, what it drives,
/// and - the point of drawing a bar at all - moves when its control does,
/// so a player can confirm a mapping without arming a learn.
fn render_mapping(view: &SettingsSheetView<'_>, panel: Rect, rows: Rect, buffer: &mut Buffer) {
    let theme = view.theme;
    let width = usize::from(rows.width);
    let Some(grid) = SlotGrid::scrolled(rows, view.sheet.mapping_first(rows)) else {
        buffer.set_stringn(
            rows.x,
            rows.y,
            "no room for the slots - a wider terminal shows them",
            width,
            Style::default().fg(theme.muted),
        );
        return;
    };
    for slot in 0..MAPPING_SLOTS {
        let Some(cell) = grid.cell(slot) else {
            continue;
        };
        let state = &view.mappings[slot];
        let selected = view.sheet.selected.min(MAPPING_SLOTS - 1) == slot;
        let inner = usize::from(cell.width.saturating_sub(2));
        // The rule across the top carries the slot's number: it is the
        // number a score's faders are counted by, so it has to be the
        // thing the eye lands on first.
        // The number, and how this slot reads its control: the rule has
        // room for both, and a mapping that jumps when you expected it not
        // to is a thing you want to see without pressing anything.
        let number = if state.chip.is_empty() {
            format!(" {} ", slot + 1)
        } else {
            format!(" {} \u{00b7} {} ", slot + 1, state.takeover)
        };
        let rule: String = std::iter::once('\u{256d}')
            .chain(number.chars())
            .chain(std::iter::repeat_n(
                '\u{2500}',
                usize::from(cell.width).saturating_sub(number.chars().count() + 2),
            ))
            .chain(std::iter::once('\u{256e}'))
            .collect();
        let frame = if selected {
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD)
        } else if state.chip.is_empty() {
            Style::default().fg(theme.muted)
        } else {
            Style::default().fg(theme.foreground)
        };
        buffer.set_stringn(cell.x, cell.y, &rule, usize::from(cell.width), frame);

        // What it is bound to, and what it drives.
        let bound = if state.learning {
            "move a control\u{2026}".to_owned()
        } else if state.chip.is_empty() {
            "- not assigned".to_owned()
        } else if state.fader.is_empty() {
            state.chip.clone()
        } else {
            format!("{} \u{2192} {}", state.chip, state.fader)
        };
        let bound: String = bound.chars().take(inner).collect();
        let bound_style = if state.learning {
            Style::default()
                .fg(theme.selection_text)
                .bg(theme.selection)
                .add_modifier(Modifier::BOLD)
        } else if state.chip.is_empty() {
            Style::default().fg(theme.muted)
        } else if state.live {
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(theme.foreground)
        };
        buffer.set_stringn(cell.x, cell.y + 1, "\u{2502}", 1, frame);
        buffer.set_stringn(
            cell.x + 1,
            cell.y + 1,
            format!("{bound:<inner$}"),
            inner,
            bound_style,
        );
        buffer.set_stringn(cell.right() - 1, cell.y + 1, "\u{2502}", 1, frame);

        // The floor rule doubles as the slot's bar: where the fader it
        // drives is standing, in the same glyph the score's pill uses, so
        // the confirmation reads without a legend.
        let floor: String = match state.notch {
            Some(notch) if inner > 0 => {
                let knob =
                    ((f64::from(notch) * (inner - 1) as f64).round() as usize).min(inner - 1);
                std::iter::once('\u{2570}')
                    .chain((0..inner).map(|at| if at == knob { '\u{2588}' } else { '\u{2500}' }))
                    .chain(std::iter::once('\u{256f}'))
                    .collect()
            }
            _ => std::iter::once('\u{2570}')
                .chain(std::iter::repeat_n('\u{2500}', inner))
                .chain(std::iter::once('\u{256f}'))
                .collect(),
        };
        let floor_style = if state.notch.is_some() {
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD)
        } else {
            frame
        };
        buffer.set_stringn(
            cell.x,
            cell.y + 2,
            &floor,
            usize::from(cell.width),
            floor_style,
        );
    }
    // What the page is for, and how to use it, under the grid.
    let hint_y = grid.area.bottom();
    if hint_y < rows.bottom() {
        buffer.set_stringn(
            rows.x,
            hint_y,
            "Enter learns \u{b7} move a knob, a fader or a stick \u{b7} t reads it another way \u{b7} Space unbinds",
            width,
            Style::default().fg(theme.muted),
        );
    }
    if hint_y + 1 < rows.bottom() {
        buffer.set_stringn(
            rows.x,
            hint_y + 1,
            "slot 1 drives the score's first slider, slot 2 the second - the mixer's desk, in its order",
            width,
            Style::default().fg(theme.muted),
        );
    }
    let _ = panel;
}

/// The keybinds page, as rows. A row says the action, the chord, and -
/// the point of the dot - whether the chord is yours or the studio's, so
/// an unlearnt row reads as a map and a learnt one as a change.
pub(super) fn render_keybinds(
    view: &SettingsSheetView<'_>,
    panel: Rect,
    rows: Rect,
    buffer: &mut Buffer,
) {
    let theme = view.theme;
    let width = usize::from(rows.width);
    let shown = usize::from(rows.height).max(1);
    let first = view.sheet.first_row(shown, view.bindings.len());
    render_keybind_rows(
        &view.bindings,
        view.sheet.selected,
        first,
        rows,
        theme,
        buffer,
    );
    // The count of what is off screen, said rather than left to be found
    // by reaching for it.
    let hidden_below = view.bindings.len().saturating_sub(first + shown);
    let more = if hidden_below > 0 {
        format!(" · ↓ {hidden_below} more")
    } else {
        String::new()
    };
    let hint = match view.bindings.get(view.sheet.selected) {
        Some(row) if row.confirm && view.sheet.selected != RESET_KEYBINDS_ROW => {
            "Enter: confirm rebind · Esc: cancel".to_owned()
        }
        Some(row) if row.learning => {
            "Press the shortcut · Esc: cancel · No response? Try another key or check terminal/system shortcuts."
                .to_owned()
        }
        _ => keybinds_footer_hint(view.sheet, view.settings, view.features),
    };
    // Use the explanation space already reserved below the list so narrow
    // panels keep the whole hint, including reset confirmation controls.
    let profile = terminal_row_label(view.settings, view.features);
    let text = if view.sheet.selected == TERMINAL_PROFILE_ROW
        && format!("  Terminal: {profile}").width() > width
    {
        // Detection can include versions and nested multiplexers. The full
        // identity stays readable below when it cannot fit on its row.
        format!("{profile} · {hint}{more}")
    } else {
        format!("{hint}{more}")
    };
    let lines = wrap_words(&text, width);
    let height = usize::from(
        panel
            .bottom()
            .saturating_sub(1)
            .saturating_sub(rows.bottom()),
    );
    let count = lines.len().min(height);
    let top = panel.bottom().saturating_sub(1 + count as u16);
    for (offset, line) in lines.iter().take(count).enumerate() {
        buffer.set_stringn(
            rows.x,
            top + offset as u16,
            line,
            width,
            Style::default().fg(theme.muted),
        );
    }
}

pub(super) fn render_keybind_rows(
    bindings: &[KeybindRow],
    selected_row: usize,
    first: usize,
    rows: Rect,
    theme: &Theme,
    buffer: &mut Buffer,
) {
    // Fixed cell boundaries keep every row aligned, including long action
    // names, terminal fallbacks and user-defined chords.
    let width = usize::from(rows.width);
    let action_width = width.saturating_sub(28).clamp(8, 26);
    let chord_x = 2 + action_width + 2;
    let chord_width = bindings
        .iter()
        .skip(KEYBIND_ACTION_START)
        .map(|row| row.chord.width())
        .max()
        .unwrap_or(7)
        .clamp(7, 12);
    let alternate_x = chord_x + chord_width + 2;
    let alternate_width = bindings
        .iter()
        .map(|row| row.also.width())
        .max()
        .unwrap_or(0)
        .max(6)
        + 5;
    let state_x = alternate_x + alternate_width + 2;
    for (offset, (index, row)) in bindings
        .iter()
        .enumerate()
        .skip(first)
        .take(usize::from(rows.height))
        .enumerate()
    {
        let y = rows.y + offset as u16;
        let selected = index == selected_row;
        let style = if row.confirm || row.learning {
            Style::default()
                .fg(theme.selection_text)
                .bg(theme.selection)
                .add_modifier(Modifier::BOLD)
        } else if selected {
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD)
        } else if row.learnt {
            Style::default().fg(theme.foreground)
        } else {
            Style::default().fg(theme.muted)
        };
        let mut cell = |offset: usize, text: &str, limit: usize| {
            if offset < width {
                buffer.set_stringn(
                    rows.x + offset as u16,
                    y,
                    text,
                    limit.min(width - offset),
                    style,
                );
            }
        };
        cell(
            0,
            if selected {
                super::super::terminal::symbol("▸")
            } else {
                " "
            },
            1,
        );
        // The terminal name is the setting's value, not a short key chord.
        // Give it the whole row instead of clipping it to the chord column.
        if index == TERMINAL_PROFILE_ROW {
            cell(2, &format!("{}: {}", row.action, row.chord), width);
            continue;
        }
        cell(2, row.action, action_width);
        if index == RESET_KEYBINDS_ROW {
            cell(chord_x, &row.chord, width);
        } else if row.confirm {
            cell(
                chord_x,
                &format!("rebind {}? Enter yes · Esc no", row.chord),
                width,
            );
        } else if row.learning {
            cell(chord_x, "press keys…", width);
        } else {
            cell(
                chord_x,
                if row.chord.is_empty() {
                    "unbound"
                } else {
                    &row.chord
                },
                chord_width,
            );
            if !row.also.is_empty() {
                cell(alternate_x, &format!("also {}", row.also), alternate_width);
            }
            cell(
                state_x,
                if row.learnt { "overridden" } else { "default" },
                width,
            );
        }
    }
}

/// Every remote pack on disk and idle - for the cache-all row's checkmark.
fn remote_all_cached(view: &SettingsSheetView<'_>) -> bool {
    if view
        .precache
        .as_ref()
        .is_some_and(|progress| progress.kind == PrecacheKind::Library && !progress.done())
    {
        return false;
    }
    let remotes: Vec<_> = view
        .sources
        .iter()
        .filter(|source| !source.local && !source.dimmed)
        .collect();
    !remotes.is_empty()
        && remotes.iter().all(|source| {
            source.cache.as_ref().is_none_or(|progress| progress.done())
                && source
                    .cache_fill
                    .is_some_and(|(held, total)| total == 0 || held >= total)
        })
}

fn source_control_value(view: &SettingsSheetView<'_>, row: Row) -> String {
    let on_off = |on: bool| {
        if on {
            "● on".to_owned()
        } else {
            "○ off".to_owned()
        }
    };
    match row {
        Row::PrecacheSources => on_off(view.settings.precache_sources),
        Row::CacheDefaults => match &view.precache {
            Some(progress) if progress.kind == PrecacheKind::Library && !progress.done() => {
                progress.count()
            }
            _ if remote_all_cached(view) => "✓ all cached".to_owned(),
            _ => String::new(),
        },
        Row::SampleCache if view.sheet.confirm_clear_cache => "Enter again".to_owned(),
        Row::SampleCache => view.sample_cache.clone(),
        _ => String::new(),
    }
}

fn source_control_explain(view: &SettingsSheetView<'_>, row: Row) -> String {
    if row == Row::SampleCache && view.sheet.confirm_clear_cache {
        return "empties downloaded samples · Esc cancels".to_owned();
    }
    if row == Row::CacheDefaults
        && let Some(progress) = &view.precache
        && progress.kind == PrecacheKind::Library
        && let Some(running) = progress.explain()
    {
        return running;
    }
    row.explain().to_owned()
}

fn sources_footer_hint(sheet: &SettingsSheet, sources: &[SourceRow], show_file: &str) -> String {
    let at = sheet.selected;
    let hint = if at < SOURCE_CONTROL_COUNT {
        match SOURCES_CONTROLS[at] {
            Row::PrecacheSources => "Tab pages · ←/→/Space change · Esc closes".to_owned(),
            Row::CacheDefaults => {
                "Tab pages · Enter caches all remote packs · Esc closes".to_owned()
            }
            Row::RefreshSources => {
                "Tab pages · Enter refetches every remote pack's list · Esc closes".to_owned()
            }
            Row::SampleCache if sheet.confirm_clear_cache => {
                "Enter again empties · Esc cancels".to_owned()
            }
            Row::SampleCache => "Tab pages · Enter clears · Esc closes".to_owned(),
            _ => "Tab pages · Esc closes".to_owned(),
        }
    } else if at < SOURCE_CONTROL_COUNT + sheet.source_count {
        let import = at - SOURCE_CONTROL_COUNT;
        let remote = sources.get(import).is_some_and(|row| !row.local);
        if remote {
            "Tab pages · e edits · Space on/off · Enter refetches · r aliases · c caches this pack · d removes"
                .to_owned()
        } else {
            format!(
                "Tab pages · e edits · Space on/off · Enter refetches · r aliases · {show_file} opens folder · d removes"
            )
        }
    } else {
        "Tab pages · c caches this pack".to_owned()
    };
    format!("a adds · {hint}")
}

fn render_sources(view: &SettingsSheetView<'_>, panel: Rect, rows: Rect, buffer: &mut Buffer) {
    let theme = view.theme;
    let width = usize::from(rows.width);
    let imported = view.sources.iter().filter(|row| !row.shipped).count();
    let shipped = view.sources.len().saturating_sub(imported);
    let lines = source_lines(imported, shipped);
    let shown = usize::from(rows.height).max(1);
    let first = view.sheet.first_sources(shown, imported, shipped);
    let label_width = SOURCES_CONTROLS
        .iter()
        .map(|row| row.label().len())
        .max()
        .unwrap_or(14)
        .max(14);
    let border = Style::default().fg(theme.muted);
    for (offset, line) in lines.iter().skip(first).take(shown).enumerate() {
        let y = rows.y + offset as u16;
        match line {
            SourceLine::Header { section, first } => {
                if offset + 1 == shown {
                    continue;
                }
                let title = match section {
                    SourceSection::Defaults => match &view.precache {
                        Some(progress) if progress.kind == PrecacheKind::Library => {
                            format!("default samples · {}", progress.count())
                        }
                        _ => section.title().to_owned(),
                    },
                    _ => section.title().to_owned(),
                };
                let rule = if *first {
                    GroupRule::Opens(title.as_str())
                } else {
                    GroupRule::Between(title.as_str())
                };
                draw_group_border(buffer, rows, y, rule, theme);
            }
            SourceLine::Gap => {}
            SourceLine::Footer => {
                draw_group_border(buffer, rows, y, GroupRule::Closes, theme);
            }
            SourceLine::Control(index) => {
                let row = SOURCES_CONTROLS[*index];
                let selected = *index == view.sheet.selected;
                let marker = if selected {
                    format!("{} ", super::super::terminal::symbol("▸"))
                } else {
                    "  ".to_owned()
                };
                let value = source_control_value(view, row);
                let value: String = if value.chars().count() > 22 {
                    let mut cut: String = value.chars().take(21).collect();
                    cut.push('…');
                    cut
                } else {
                    value
                };
                let explain = source_control_explain(view, row);
                buffer.set_stringn(rows.x, y, "│", 1, border);
                buffer.set_stringn(rows.right() - 1, y, "│", 1, border);
                let content_x = rows.x + 1;
                let content_width = width.saturating_sub(2);
                let text = format!(
                    "{marker}{:<label_width$} {:<22} {explain}",
                    row.label(),
                    value,
                    explain = explain.as_str(),
                );
                let style = if selected {
                    Style::default()
                        .fg(theme.foreground)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(theme.foreground)
                };
                buffer.set_stringn(content_x, y, &text, content_width, style);
                if !selected {
                    let explain_x = content_x + (2 + label_width + 1 + 22 + 1) as u16;
                    buffer.set_stringn(
                        explain_x,
                        y,
                        explain.as_str(),
                        content_width.saturating_sub(usize::from(explain_x - content_x)),
                        Style::default().fg(theme.muted),
                    );
                }
            }
            SourceLine::NoImports => {
                buffer.set_stringn(rows.x, y, "│", 1, border);
                buffer.set_stringn(rows.right() - 1, y, "│", 1, border);
                buffer.set_stringn(
                    rows.x + 1,
                    y,
                    "  none yet · a adds a folder or pack",
                    width.saturating_sub(2),
                    Style::default().fg(theme.muted),
                );
            }
            SourceLine::Row(index) => {
                let source_index = index.saturating_sub(SOURCE_CONTROL_COUNT);
                let source = &view.sources[source_index];
                let selected = *index == view.sheet.selected;
                let marker = if selected {
                    format!("{} ", super::super::terminal::symbol("▸"))
                } else {
                    "  ".to_owned()
                };
                // A shipped pack wears a glyph the imports do not, so the two
                // lists read apart even where colour does not carry.
                let glyph = if source.shipped {
                    format!("{} ", super::super::terminal::symbol("◇"))
                } else {
                    String::new()
                };
                let shown_label = if source.local {
                    format!("{} (local)", source.label)
                } else {
                    source.label.clone()
                };
                let state = source
                    .cache
                    .as_ref()
                    .filter(|progress| !progress.done())
                    .map_or_else(|| source.state.clone(), SourceCacheProgress::label);
                let text = format!("{marker}{glyph}{shown_label} · {state}");
                buffer.set_stringn(rows.x, y, "│", 1, border);
                buffer.set_stringn(rows.right() - 1, y, "│", 1, border);
                let content_x = rows.x + 1;
                let content_width = width.saturating_sub(2);
                let style = if selected {
                    Style::default()
                        .fg(theme.accent)
                        .add_modifier(Modifier::BOLD)
                } else if source.shipped {
                    Style::default()
                        .fg(theme.muted)
                        .add_modifier(Modifier::ITALIC)
                } else if source.dimmed {
                    Style::default().fg(theme.muted)
                } else {
                    Style::default().fg(theme.foreground)
                };
                buffer.set_stringn(content_x, y, &text, content_width, style);
                if let Some((held, total)) = source.cache_fill {
                    let needle = format!("{held}/{total} cached");
                    if let Some(at) = text.find(&needle) {
                        let prefix = UnicodeWidthStr::width(&text[..at]) as u16;
                        let mut fill = Style::default().fg(cache_fill_color(held, total, theme));
                        if selected {
                            fill = fill.add_modifier(Modifier::BOLD);
                        } else if source.shipped {
                            fill = fill.add_modifier(Modifier::ITALIC);
                        }
                        buffer.set_stringn(
                            content_x.saturating_add(prefix),
                            y,
                            &needle,
                            content_width.saturating_sub(usize::from(prefix)),
                            fill,
                        );
                    }
                }
                // The address of the row you are on, where there is room for
                // it: a short name is what you pick from, and the full spec is
                // what you check before removing or refetching it.
                if selected && source.label != source.spec {
                    let drawn = UnicodeWidthStr::width(text.as_str()) as u16;
                    let spec_width = UnicodeWidthStr::width(source.spec.as_str()) as u16;
                    let spec_x = (rows.right() - 1).saturating_sub(spec_width);
                    if spec_x > content_x + drawn + 2 {
                        buffer.set_stringn(
                            spec_x,
                            y,
                            &source.spec,
                            usize::from(spec_width),
                            Style::default().fg(theme.muted),
                        );
                    }
                }
            }
        }
    }
    // The chord on the Keybinds row for the action. The row is empty
    // while a learn waits for the chord.
    let show_file = view
        .bindings
        .iter()
        .find(|row| row.action == BindAction::ShowFile.label() && !row.chord.is_empty())
        .map_or_else(
            || crate::keybinds::shortcut_label("Alt+O").into_owned(),
            |row| row.chord.clone(),
        );
    buffer.set_stringn(
        rows.x,
        panel.bottom().saturating_sub(2),
        sources_footer_hint(&view.sheet, &view.sources, &show_file),
        width,
        Style::default().fg(theme.muted),
    );
    buffer.set_stringn(
        rows.x,
        panel.bottom().saturating_sub(3),
        "Drag a sample folder onto this window to import it.",
        width,
        Style::default().fg(theme.muted),
    );
}

/// Colour of the `n/m cached` fraction: full, some, or none.
fn cache_fill_color(held: usize, total: usize, theme: &Theme) -> ratatui::style::Color {
    if total == 0 || held == 0 {
        theme.muted
    } else if held >= total {
        theme.ok
    } else {
        theme.warn
    }
}

fn render_about(view: &SettingsSheetView<'_>, panel: Rect, rows: Rect, buffer: &mut Buffer) {
    let height = about_content_height(rows.width);
    let content_rows = Rect::new(0, 0, rows.width, height as u16);
    let content_panel = Rect::new(0, 0, rows.width, height as u16 + 2);
    let mut content = Buffer::empty(content_rows);
    render_about_content(view, content_panel, content_rows, &mut content);
    let shown = usize::from(rows.height);
    let first = view.sheet.first.min(height.saturating_sub(shown));
    for offset in 0..shown.min(height.saturating_sub(first)) {
        for x in 0..rows.width {
            if let (Some(from), Some(to)) = (
                content.cell((x, (first + offset) as u16)),
                buffer.cell_mut((rows.x + x, rows.y + offset as u16)),
            ) {
                let background = to.bg;
                *to = from.clone();
                to.set_bg(background);
            }
        }
    }
    buffer.set_stringn(
        rows.x,
        panel.bottom().saturating_sub(2),
        "PgUp/PgDn scroll · Home/End · Tab tabs · Esc closes",
        usize::from(rows.width),
        Style::default().fg(view.theme.muted),
    );
}

fn render_about_content(
    view: &SettingsSheetView<'_>,
    panel: Rect,
    rows: Rect,
    buffer: &mut Buffer,
) {
    let report = about_report(view);
    let width = usize::from(rows.width);
    let features = view.features;
    let theme = view.theme;
    let name = if features.name.is_empty() {
        "not checked"
    } else {
        &features.name
    };
    buffer.set_stringn(
        rows.x,
        rows.y,
        format!("Terminal: {name}"),
        width,
        Style::default().fg(theme.foreground),
    );
    let active = match view.tier {
        Tier::Cells => "Cells",
        Tier::Fine => "Fine glyphs",
        Tier::Pixels => "Kitty pixels",
    };
    let rendering = if !view.settings.rendering.supported(features) {
        format!(
            "{} unavailable here; using {active}",
            view.settings.rendering.label()
        )
    } else if view.settings.rendering == RenderingMode::Automatic {
        format!("Automatic ({active})")
    } else {
        view.settings.rendering.label().to_owned()
    };
    buffer.set_stringn(
        rows.x,
        rows.y + 1,
        format!("Rendering: {rendering}"),
        width,
        Style::default().fg(theme.muted),
    );

    buffer.set_stringn(
        rows.x,
        rows.y + 2,
        format!(
            "Shortcut profile: {}",
            terminal_row_label(view.settings, features)
        ),
        width,
        Style::default().fg(theme.muted),
    );
    let mut checklist = terminal_checklist(features, view.capabilities);
    if let Some(profile) = view.settings.terminal_profile.as_deref()
        && let Some((_, known)) = checklist
            .iter_mut()
            .find(|(label, _)| *label == "Shortcut profile")
    {
        let effective =
            super::super::terminal::conflicts::effective_profile(&features.name, Some(profile));
        *known = super::super::terminal::conflicts::profile_known(&effective).then_some(true);
    }
    let title = if checklist
        .iter()
        .all(|(_, available)| *available == Some(true))
    {
        "Terminal features · complete"
    } else {
        "Terminal features"
    };
    draw_group_border(buffer, rows, rows.y + 3, GroupRule::Opens(title), theme);
    let columns = if rows.width >= 78 { 3 } else { 2 };
    let column_width = usize::from(rows.width.saturating_sub(2)) / columns;
    let checklist_rows = checklist.len().div_ceil(columns);
    for line in 0..checklist_rows {
        let y = rows.y + 4 + line as u16;
        if y >= panel.bottom().saturating_sub(2) {
            break;
        }
        buffer.set_stringn(rows.x, y, "│", 1, Style::default().fg(theme.muted));
        buffer.set_stringn(
            rows.right() - 1,
            y,
            "│",
            1,
            Style::default().fg(theme.muted),
        );
        for column in 0..columns {
            let Some((label, available)) = checklist.get(line * columns + column) else {
                continue;
            };
            let (mark, colour) = match available {
                Some(true) => ('✓', theme.ok),
                Some(false) => ('×', theme.muted),
                None => ('?', theme.muted),
            };
            let x = rows.x + 1 + (column * column_width) as u16;
            buffer.set_stringn(
                x,
                y,
                format!(" {mark} {label}"),
                column_width,
                Style::default().fg(colour),
            );
        }
    }
    let end = rows.y + 4 + checklist_rows as u16;
    if end < panel.bottom().saturating_sub(2) {
        draw_group_border(buffer, rows, end, GroupRule::Closes, theme);
    }
    for (offset, line) in report.human_lines().iter().enumerate() {
        let y = end + 1 + offset as u16;
        if y >= panel.bottom().saturating_sub(2) {
            break;
        }
        // The report's [x]/[~]/[-] markers read as cipher; the sheet
        // spells them: in use, present but unused, not there.
        let line = line
            .replace("[x]", "✓")
            .replace("[~]", "○")
            .replace("[-]", "✗");
        buffer.set_stringn(
            rows.x,
            y,
            &line,
            width,
            Style::default().fg(theme.foreground),
        );
    }
}

fn on_off(value: bool) -> String {
    if value {
        "● on".to_owned()
    } else {
        "○ off".to_owned()
    }
}
