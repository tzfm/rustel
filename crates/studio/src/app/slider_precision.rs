//! An expanded absolute fader and exact entry share the inline control's value.
use super::*;
use ratatui::style::Style;
use ratatui::widgets::{Block, Borders, Clear, Paragraph};

#[derive(Clone, Debug)]
pub(super) struct PrecisionSlider {
    scene: SceneId,
    call_from: usize,
    entry: Option<NumberEntry>,
    drag: Option<SliderTrack>,
    invalid: bool,
}

#[derive(Clone, Debug)]
struct NumberEntry {
    text: String,
    cursor: usize,
    selected: bool,
}

impl NumberEntry {
    fn new(value: f64) -> Self {
        let text = slider::format_value(value);
        Self {
            cursor: text.len(),
            text,
            selected: true,
        }
    }

    fn insert(&mut self, text: &str) {
        // A number field never accepts newlines, expressions or unbounded pastes.
        if text.len() > 64
            || !text
                .chars()
                .all(|c| c.is_ascii_digit() || matches!(c, '.' | '-' | '+' | 'e' | 'E'))
        {
            return;
        }
        if self.selected {
            self.text.clear();
            self.cursor = 0;
            self.selected = false;
        }
        if self.text.len() + text.len() <= 64 {
            self.text.insert_str(self.cursor, text);
            self.cursor += text.len();
        }
    }

    fn erase(&mut self, backwards: bool) {
        if self.selected {
            self.text.clear();
            self.cursor = 0;
            self.selected = false;
        } else if backwards && self.cursor > 0 {
            self.cursor -= 1;
            self.text.remove(self.cursor);
        } else if !backwards && self.cursor < self.text.len() {
            self.text.remove(self.cursor);
        }
    }
}

pub(super) fn geometry(frame: Rect) -> Rect {
    let width = frame.width.saturating_sub(4).min(100);
    let height = frame.height.min(10);
    Rect::new(
        frame.x + (frame.width - width) / 2,
        frame.y + (frame.height - height) / 2,
        width,
        height,
    )
}

fn track(area: Rect) -> Option<(SliderTrack, u16)> {
    (area.width >= 12 && area.height >= 10).then_some((
        SliderTrack {
            x: area.x + 2,
            width: area.width.saturating_sub(4),
        },
        area.y + 3,
    ))
}

pub(super) struct PrecisionView {
    state: PrecisionSlider,
    span: SliderSpan,
    travel: slider::Travel,
    frequency: bool,
    pixels: bool,
    smoothing: bool,
}

impl PrecisionView {
    pub(super) fn render(
        &self,
        frame: &mut ratatui::Frame<'_>,
        theme: &Theme,
        cursor_visible: bool,
    ) {
        let area = geometry(frame.area());
        super::super::graphics::cover_images(area);
        frame.render_widget(Clear, area);
        frame.render_widget(
            Block::default()
                .borders(Borders::ALL)
                .title(" Slider ")
                .style(Style::default().bg(theme.overlay).fg(theme.foreground)),
            area,
        );
        let Some((rail, y)) = track(area) else { return };
        let mut line = |row: u16, text: String, colour| {
            frame.render_widget(
                Paragraph::new(text).style(Style::default().fg(colour)),
                Rect::new(rail.x, row, rail.width, 1),
            );
        };
        line(
            area.y + 1,
            format!(
                "{} to {} · step {} · mouse: {}",
                slider::format_value(self.span.min),
                slider::format_value(self.span.max),
                slider::format_value(self.span.step),
                if self.pixels { "pixels" } else { "cells" }
            ),
            theme.muted,
        );
        line(y, "─".repeat(usize::from(rail.width)), theme.accent);
        let ratio = self
            .travel
            .ratio(self.span.value, self.span.min, self.span.max);
        let knob = rail.x + (ratio.clamp(0.0, 1.0) * f64::from(rail.width - 1)).round() as u16;
        frame.render_widget(
            Paragraph::new("█").style(Style::default().fg(theme.accent)),
            Rect::new(knob, y, 1, 1),
        );
        let value = self.state.entry.as_ref().map_or_else(
            || slider::format_value(self.span.value),
            |entry| entry.text.clone(),
        );
        let selected = self
            .state
            .entry
            .as_ref()
            .is_some_and(|entry| entry.selected);
        let style = if selected {
            Style::default()
                .fg(theme.selection_text)
                .bg(theme.selection)
        } else {
            Style::default().fg(if self.state.invalid {
                theme.error
            } else {
                theme.foreground
            })
        };
        let scroll = self.state.entry.as_ref().map_or(0, |entry| {
            (7 + entry.cursor as u16).saturating_sub(rail.width - 1)
        });
        frame.render_widget(
            Paragraph::new(format!("value: {value}"))
                .style(style)
                .scroll((0, scroll)),
            Rect::new(rail.x, area.y + 5, rail.width, 1),
        );
        let hints = if self.state.invalid {
            "Enter a finite number · Esc cancels"
        } else if self.state.entry.is_some() {
            "Enter applies · Esc cancels entry"
        } else {
            "←/→ adjust · Shift: one step · type value · Esc closes"
        };
        frame.render_widget(
            Paragraph::new(hints).style(Style::default().fg(theme.muted)),
            Rect::new(rail.x, area.y + 6, rail.width, 1),
        );
        let travel_hint = if self.frequency && self.span.min > 0.0 {
            format!(
                "L: frequency travel {} (all Hz sliders)",
                if self.travel == slider::Travel::Logarithmic {
                    "Log"
                } else {
                    "Linear"
                }
            )
        } else if self.frequency {
            "Linear travel · Log needs a positive minimum".to_owned()
        } else {
            "Linear travel".to_owned()
        };
        frame.render_widget(
            Paragraph::new(travel_hint).style(Style::default().fg(theme.muted)),
            Rect::new(rail.x, area.y + 7, rail.width, 1),
        );
        frame.render_widget(
            Paragraph::new(format!(
                "S: audio smoothing {} (direct gain, lpf, lpq)",
                if self.smoothing { "on" } else { "off" }
            ))
            .style(Style::default().fg(theme.muted)),
            Rect::new(rail.x, area.y + 8, rail.width, 1),
        );
        if cursor_visible && let Some(entry) = &self.state.entry {
            frame.set_cursor_position((
                rail.x
                    + (7 + entry.cursor as u16)
                        .saturating_sub(scroll)
                        .min(rail.width - 1),
                area.y + 5,
            ));
        }
    }
}

impl App {
    pub(super) fn precision_slider_shape(&self, x: u16, y: u16) -> Option<&'static str> {
        let state = self.slider_precision.as_ref()?;
        let area = geometry(self.frame);
        Some(if state.drag.is_some() {
            SHAPE_GRABBING
        } else if within(area, x, y) && y == area.y + 5 {
            SHAPE_TEXT
        } else if track(area)
            .is_some_and(|(rail, row)| y == row && x >= rail.x && x < rail.x + rail.width)
        {
            SHAPE_POINTER
        } else {
            SHAPE_DEFAULT
        })
    }

    pub(super) fn open_precision_slider(&mut self, scene: SceneId, call_from: usize) {
        if self.live_chip_at(scene, call_from).is_none() {
            return;
        }
        self.dismiss_dialogs(None);
        self.pointer = None;
        self.slider_precision = Some(PrecisionSlider {
            scene,
            call_from,
            entry: None,
            drag: None,
            invalid: false,
        });
        self.dirty_frame = true;
    }

    /// Alt+Enter's answer on a fader: arm the live slider under the caret
    /// and open its precision control. `false` when the caret is on none.
    pub(super) fn open_precision_slider_at_caret(&mut self) -> bool {
        let scene = self.scenes.current().id;
        let caret = self.editor().primary_selection().head.0;
        if let Some(chip) = self
            .live_chips(scene)
            .into_iter()
            .find(|chip| chip.call.contains(&caret))
        {
            self.armed_slider = Some((scene, chip.call.start));
            self.open_precision_slider(scene, chip.call.start);
            return true;
        }
        false
    }

    pub(super) fn precision_slider_view(&self) -> Option<PrecisionView> {
        let state = self.slider_precision.as_ref()?;
        let chip = self.live_chip_at(state.scene, state.call_from)?;
        Some(PrecisionView {
            state: state.clone(),
            span: chip.span,
            travel: chip.travel,
            frequency: chip.frequency,
            pixels: self.features.pixel_mouse,
            smoothing: self.ui_settings.slider_smoothing,
        })
    }

    pub(super) fn stop_precision_drag(&mut self) {
        if let Some(state) = &mut self.slider_precision {
            state.drag = None;
        }
    }

    pub(super) fn precision_slider_key(&mut self, key: &crossterm::event::KeyEvent) -> bool {
        let Some(mut state) = self.slider_precision.take() else {
            return false;
        };
        if key.kind == KeyEventKind::Release {
            self.slider_precision = Some(state);
            return true;
        }
        let Some(chip) = self.live_chip_at(state.scene, state.call_from) else {
            self.dirty_frame = true;
            return true;
        };
        state.drag = None;
        let primary = key
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::SUPER);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);
        if let Some(entry) = &mut state.entry {
            match key.code {
                KeyCode::Esc => {
                    state.entry = None;
                    state.invalid = false;
                }
                KeyCode::Enter => {
                    if let Ok(value) = entry.text.parse::<f64>()
                        && value.is_finite()
                    {
                        self.set_slider(state.scene, &chip, value);
                        state.entry = None;
                        state.invalid = false;
                    } else {
                        state.invalid = true;
                    }
                }
                KeyCode::Char('a' | 'A') if primary => entry.selected = true,
                KeyCode::Char('v' | 'V') if primary => {
                    if let Ok(text) = self.clipboard.get_text() {
                        entry.insert(text.trim());
                    }
                }
                KeyCode::Char('c' | 'C') if primary => {
                    let _ = self.clipboard.set_text(entry.text.clone());
                }

                KeyCode::Left if !alt => {
                    entry.cursor = if entry.selected {
                        0
                    } else {
                        entry.cursor.saturating_sub(1)
                    };
                    entry.selected = false;
                }
                KeyCode::Right if !alt => {
                    entry.cursor = if entry.selected {
                        entry.text.len()
                    } else {
                        (entry.cursor + 1).min(entry.text.len())
                    };
                    entry.selected = false;
                }
                KeyCode::Home => {
                    entry.cursor = 0;
                    entry.selected = false;
                }
                KeyCode::End => {
                    entry.cursor = entry.text.len();
                    entry.selected = false;
                }
                KeyCode::Backspace => entry.erase(true),
                KeyCode::Delete => entry.erase(false),
                KeyCode::Char(c) if !primary && !alt => {
                    entry.insert(&c.to_string());
                    state.invalid = false;
                }
                _ => {}
            }
        } else {
            match key.code {
                KeyCode::Esc => {
                    self.dirty_frame = true;
                    return true;
                }
                KeyCode::Left | KeyCode::Right if !primary && !alt => self.nudge_live_slider(
                    state.scene,
                    &chip,
                    if key.code == KeyCode::Right {
                        1.0
                    } else {
                        -1.0
                    },
                    shift,
                ),
                KeyCode::Home if !primary && !alt => {
                    self.set_slider(state.scene, &chip, chip.span.min)
                }
                KeyCode::End if !primary && !alt => {
                    self.set_slider(state.scene, &chip, chip.span.max)
                }
                KeyCode::Char('v' | 'V') if primary => {
                    let mut entry = NumberEntry::new(chip.span.value);
                    if let Ok(text) = self.clipboard.get_text() {
                        entry.insert(text.trim());
                    }
                    state.entry = Some(entry);
                }
                KeyCode::Enter | KeyCode::Tab if !primary && !alt => {
                    state.entry = Some(NumberEntry::new(chip.span.value))
                }
                KeyCode::Char('l' | 'L')
                    if !primary
                        && !alt
                        && key.kind == KeyEventKind::Press
                        && chip.frequency
                        && chip.span.min > 0.0 =>
                {
                    self.ui_settings.frequency_slider_log = !self.ui_settings.frequency_slider_log;
                    if self.armed_slider == Some((state.scene, state.call_from))
                        && let Some(updated) = self.live_chip_at(state.scene, state.call_from)
                    {
                        self.status =
                            self.slider_hint(updated.span.value, updated.span.step, updated.travel);
                    }
                    self.prefs.set_ui_settings(&self.ui_settings);
                    self.save_prefs_soon();
                }
                KeyCode::Char('s' | 'S') if !primary && !alt && key.kind == KeyEventKind::Press => {
                    self.ui_settings.slider_smoothing = !self.ui_settings.slider_smoothing;
                    if !self.ui_settings.slider_smoothing {
                        self.set_slider(state.scene, &chip, chip.span.value);
                    }
                    self.prefs.set_ui_settings(&self.ui_settings);
                    self.save_prefs_soon();
                }
                KeyCode::Char(c)
                    if !primary && !alt && (c.is_ascii_digit() || matches!(c, '.' | '-' | '+')) =>
                {
                    let mut entry = NumberEntry::new(chip.span.value);
                    entry.insert(&c.to_string());
                    state.entry = Some(entry);
                }
                _ => {}
            }
        }
        self.slider_precision = Some(state);
        self.dirty_frame = true;
        true
    }

    pub(super) fn precision_slider_paste(&mut self, text: &str) {
        let Some(mut state) = self.slider_precision.take() else {
            return;
        };
        if let Some(chip) = self.live_chip_at(state.scene, state.call_from) {
            let entry = state
                .entry
                .get_or_insert_with(|| NumberEntry::new(chip.span.value));
            entry.insert(text.trim());
            state.invalid = false;
            state.drag = None;
            self.slider_precision = Some(state);
        }
        self.dirty_frame = true;
    }

    /// While the precision control is open its keys and its pastes are
    /// its own. `true` when it took the event.
    pub(super) fn precision_slider_event(&mut self, terminal_event: &Event) -> bool {
        if self.slider_precision.is_some() {
            match terminal_event {
                Event::Key(key) => {
                    self.precision_slider_key(key);
                    return true;
                }
                Event::Paste(text) => {
                    self.precision_slider_paste(text);
                    return true;
                }
                _ => {}
            }
        }
        false
    }

    pub(super) fn precision_slider_mouse(&mut self, mouse: MouseEvent) -> bool {
        let Some(mut state) = self.slider_precision.take() else {
            return false;
        };
        let area = geometry(self.frame);
        let Some(chip) = self.live_chip_at(state.scene, state.call_from) else {
            self.dirty_frame = true;
            return true;
        };
        let (x, y) = (mouse.column, mouse.row);
        match mouse.kind {
            MouseEventKind::Down(MouseButton::Left) if !within(area, x, y) => {
                self.pointer = Some(Pointer::Panel);
                self.dirty_frame = true;
                return true;
            }
            MouseEventKind::Down(MouseButton::Left) => {
                if let Some((rail, row)) = track(area) {
                    if y == row && x >= rail.x && x < rail.x + rail.width {
                        state.entry = None;
                        state.invalid = false;
                        state.drag = Some(rail);
                        self.drag_slider(state.scene, state.call_from, rail, x);
                    } else if y == area.y + 5 {
                        state.entry = Some(NumberEntry::new(chip.span.value));
                        state.drag = None;
                    }
                }
            }
            MouseEventKind::Drag(MouseButton::Left) | MouseEventKind::Up(MouseButton::Left) => {
                if let Some(rail) = state.drag {
                    self.drag_slider(state.scene, state.call_from, rail, x);
                }
                if matches!(mouse.kind, MouseEventKind::Up(_)) {
                    state.drag = None;
                }
            }
            MouseEventKind::ScrollUp | MouseEventKind::ScrollDown
                if within(area, x, y) && state.entry.is_none() =>
            {
                self.nudge_live_slider(
                    state.scene,
                    &chip,
                    if mouse.kind == MouseEventKind::ScrollUp {
                        1.0
                    } else {
                        -1.0
                    },
                    mouse.modifiers.contains(KeyModifiers::SHIFT),
                )
            }
            _ => {}
        }
        self.slider_precision = Some(state);
        self.dirty_frame = true;
        true
    }
}
