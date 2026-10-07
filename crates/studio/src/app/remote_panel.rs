//! Remote-control settings and the footer indicator.

use super::remote_edit::{RemoteEdit, RemoteField};
use super::*;
use ratatui::style::{Modifier, Style};
use ratatui::widgets::{Block, BorderType, Widget};

#[derive(Clone, Copy, Debug)]
struct PanelGeometry {
    area: Rect,
    toggle: Rect,
    address: Rect,
    token: Rect,
    reveal: Rect,
    copy: Rect,
    compact: bool,
}

struct RemotePanelView<'a> {
    theme: &'a Theme,
    address: &'a str,
    token: Option<&'a str>,
    active: bool,
    edit: Option<&'a RemoteEdit>,
    revealed: bool,
    selected: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PanelKeyAction {
    Close,
    Reveal,
    Copy,
    Previous,
    Next,
    Activate,
}

fn panel_key_action(key: &KeyEvent) -> Option<PanelKeyAction> {
    if key.kind != KeyEventKind::Press {
        return None;
    }
    if key.code == KeyCode::BackTab && key.modifiers == KeyModifiers::SHIFT {
        return Some(PanelKeyAction::Previous);
    }
    if !key.modifiers.is_empty() {
        return None;
    }
    match key.code {
        KeyCode::Esc => Some(PanelKeyAction::Close),
        KeyCode::Char('r' | 'R') => Some(PanelKeyAction::Reveal),
        KeyCode::Char('c' | 'C') => Some(PanelKeyAction::Copy),
        KeyCode::Up | KeyCode::BackTab => Some(PanelKeyAction::Previous),
        KeyCode::Down | KeyCode::Tab => Some(PanelKeyAction::Next),
        KeyCode::Enter => Some(PanelKeyAction::Activate),
        _ => None,
    }
}

impl RemotePanelView<'_> {
    /// Share row positions between drawing and mouse clicks.
    fn geometry(frame: Rect) -> Option<PanelGeometry> {
        if frame.width < 20 || frame.height < 9 {
            return None;
        }
        let width = frame.width.saturating_sub(2).min(64);
        let compact = width < 36 || frame.height < 11;
        let height = if compact { 9 } else { 11 };
        let bottom_margin = (frame.height - height).min(1);
        let area = Rect::new(
            frame.right() - width - 1,
            frame.bottom() - height - bottom_margin,
            width,
            height,
        );
        let action_y = area.y + if compact { 2 } else { 3 };
        let row = |offset| Rect::new(area.x + 2, action_y + offset, width - 4, 1);
        Some(PanelGeometry {
            area,
            toggle: row(0),
            address: row(1),
            token: row(2),
            reveal: row(3),
            copy: row(4),
            compact,
        })
    }
}

fn token_label(token: &str, revealed: bool, width: usize) -> String {
    if !revealed {
        return "••••••••".into();
    }
    let mut chars = token.chars();
    let visible: String = chars.by_ref().take(width).collect();
    if chars.next().is_none() {
        visible
    } else {
        let mut clipped: String = token.chars().take(width.saturating_sub(1)).collect();
        clipped.push('…');
        clipped
    }
}

impl Widget for RemotePanelView<'_> {
    fn render(self, frame: Rect, buffer: &mut Buffer) {
        let Some(geometry) = Self::geometry(frame) else {
            return;
        };
        let area = geometry.area;
        let theme = self.theme;
        crate::graphics::cover_images(area);
        view::clear_overlay(
            buffer,
            area,
            Style::default().bg(theme.overlay).fg(theme.foreground),
        );
        Block::bordered()
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(theme.rule).bg(theme.overlay))
            .render(area, buffer);
        buffer.set_stringn(
            area.x + 2,
            area.y,
            if geometry.compact {
                " remote "
            } else {
                " remote control "
            },
            usize::from(area.width.saturating_sub(4)),
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD),
        );
        let inner_x = area.x + 2;
        let inner_width = usize::from(area.width.saturating_sub(4));
        let write = |buffer: &mut Buffer, y: u16, value: &str, color| {
            buffer.set_stringn(
                inner_x,
                y,
                value,
                inner_width,
                Style::default().fg(color).bg(theme.overlay),
            );
        };
        let error = self.edit.and_then(|edit| edit.error.as_deref());
        write(
            buffer,
            area.y + if geometry.compact { 1 } else { 2 },
            error.unwrap_or(if self.active {
                "remote control on"
            } else {
                "remote control off"
            }),
            if error.is_some() {
                theme.error
            } else {
                theme.muted
            },
        );
        let token = self.token.map_or_else(
            || "automatic".to_owned(),
            |token| token_label(token, self.revealed, inner_width.saturating_sub(9)),
        );
        for (index, (rect, label)) in [
            (
                geometry.toggle,
                if self.active { "disable" } else { "enable" }.to_owned(),
            ),
            (geometry.address, format!("address: {}", self.address)),
            (geometry.token, format!("token: {token}")),
            (
                geometry.reveal,
                if self.revealed {
                    "hide token"
                } else {
                    "reveal token"
                }
                .to_owned(),
            ),
            (geometry.copy, "copy token".to_owned()),
        ]
        .into_iter()
        .enumerate()
        {
            if index > 2 && self.token.is_none() {
                break;
            }
            let selected = index == self.selected;
            if selected {
                buffer.set_style(rect, Style::default().bg(theme.selection));
            }
            buffer.set_stringn(
                rect.x,
                rect.y,
                format!("{} {label}", if selected { "▸" } else { " " }),
                usize::from(rect.width),
                Style::default()
                    .fg(if selected {
                        theme.selection_text
                    } else {
                        theme.foreground
                    })
                    .bg(if selected {
                        theme.selection
                    } else {
                        theme.overlay
                    }),
            );
        }
        if let Some(edit) = self.edit {
            let input = edit_rect(geometry, edit.field);
            let style = Style::default()
                .fg(theme.selection_text)
                .bg(theme.selection);
            for x in input.x..input.right() {
                buffer
                    .cell_mut((x, input.y))
                    .unwrap()
                    .set_symbol(" ")
                    .set_style(style);
            }
            buffer.set_stringn(
                input.x,
                input.y,
                edit.visible_text(usize::from(input.width), self.revealed),
                usize::from(input.width),
                if edit.selected {
                    style.add_modifier(Modifier::UNDERLINED)
                } else {
                    style
                },
            );
        }
        if !geometry.compact {
            write(
                buffer,
                area.bottom() - 3,
                if self.active {
                    "Disable to edit · connect with auth <token>"
                } else if self
                    .edit
                    .is_some_and(|edit| edit.field == RemoteField::Token)
                {
                    "Blank generates a code · Alt+R reveals"
                } else {
                    "Enter edits · blank token generates a code"
                },
                theme.muted,
            );
        }
        write(
            buffer,
            area.bottom() - 2,
            if self.edit.is_some() {
                if geometry.compact {
                    "Enter save Esc"
                } else {
                    "Enter saves · Esc cancels · Ctrl/Cmd+A selects all"
                }
            } else if geometry.compact {
                "↑↓ Enter Esc"
            } else {
                "↑/↓ Tab choose · Enter acts · R reveal · C copy · Esc closes"
            },
            theme.muted,
        );
    }
}

fn edit_rect(geometry: PanelGeometry, field: RemoteField) -> Rect {
    let (row, prefix) = match field {
        RemoteField::Address => (geometry.address, 11),
        RemoteField::Token => (geometry.token, 9),
    };
    Rect::new(row.x + prefix, row.y, row.width - prefix, 1)
}

impl App {
    pub(super) fn remote_panel_area(&self) -> Option<Rect> {
        self.remote_panel_open
            .then(|| RemotePanelView::geometry(self.frame))
            .flatten()
            .map(|geometry| geometry.area)
    }

    pub(super) fn paint_remote_panel(&self, frame: &mut ratatui::Frame<'_>) {
        if !self.remote_panel_open {
            return;
        }
        let address = self
            .remote
            .as_ref()
            .map_or_else(|| self.remote_bind_address(), |remote| remote.address())
            .to_string();
        frame.render_widget(
            RemotePanelView {
                theme: &self.theme,
                address: &address,
                token: self.remote_panel_token(),
                active: self.remote.is_some(),
                edit: self.remote_edit.as_deref(),
                revealed: self.remote_token_revealed,
                selected: self.remote_panel_selected,
            },
            frame.area(),
        );
        if let Some(edit) = self.remote_edit.as_deref()
            && let Some(geometry) = RemotePanelView::geometry(frame.area())
        {
            let input = edit_rect(geometry, edit.field);
            frame.set_cursor_position((
                input.x + edit.cursor_column(usize::from(input.width)),
                input.y,
            ));
        }
    }

    fn remote_indicator_rect(&self) -> Rect {
        view::remote_indicator_rect(self.footer_hits().status, self.remote.is_some())
    }

    pub(super) fn remote_panel_available(&self) -> bool {
        RemotePanelView::geometry(self.frame).is_some()
    }

    fn remote_panel_unobstructed(&self) -> bool {
        self.help.is_none()
            && self.panel.is_none()
            && self.set_prompt.is_none()
            && self.export_sheet.is_none()
            && self.theme_picker.is_none()
            && self.theme_editor.is_none()
            && self.settings_sheet.is_none()
            && self.slider_precision.is_none()
            && self.replay_edit.is_none()
            && self.smart_action.is_none()
            && self.keybind_learn.is_none()
            && self.mapping_learn.is_none()
            && !self.piano.open
    }

    fn remote_panel_can_open(&self) -> bool {
        self.remote_panel_available() && self.menu.is_none() && self.remote_panel_unobstructed()
    }

    pub(super) fn open_remote_panel(&mut self) {
        self.menu = None;
        if !self.remote_panel_available() {
            return;
        }
        self.dismiss_dialogs(None);
        self.settle_focus();
        if !self.remote_panel_unobstructed() {
            return;
        }
        self.remote_edit = None;
        self.remote_panel_open = true;
        self.remote_token_revealed = false;
        self.remote_panel_selected = 0;
        self.pointer = None;
        self.dirty_frame = true;
    }

    fn close_remote_panel(&mut self) {
        self.remote_edit = None;
        self.remote_panel_open = false;
        self.remote_token_revealed = false;
        self.remote_panel_selected = 0;
        self.dirty_frame = true;
    }

    pub(super) fn remote_bind_address(&self) -> std::net::SocketAddr {
        self.options.remote_control.unwrap_or_else(|| {
            std::net::SocketAddr::from(([127, 0, 0, 1], crate::remote::REMOTE_PORT))
        })
    }

    fn toggle_remote_control(&mut self) {
        self.status = if let Some(remote) = self.remote.take() {
            drop(remote);
            "remote control disabled".into()
        } else {
            let address = self.remote_bind_address();
            match crate::remote::listen(address, self.options.auth_token.as_deref()) {
                Ok(remote) => {
                    self.remote = Some(Box::new(remote));
                    "remote control enabled".into()
                }
                Err(error) => format!("remote control on {address}: {error}"),
            }
        };
        self.remote_token_revealed = false;
        self.remote_panel_selected = 0;
        self.toast(self.status.clone());
        self.dirty_frame = true;
    }

    fn remote_panel_token(&self) -> Option<&str> {
        self.remote
            .as_ref()
            .map(|remote| remote.token())
            .or(self.options.auth_token.as_deref())
    }

    fn reveal_remote_token(&mut self) {
        if self.remote_panel_token().is_some() {
            self.remote_token_revealed = !self.remote_token_revealed;
            self.dirty_frame = true;
        }
    }

    fn copy_remote_token(&mut self) {
        let Some(token) = self.remote_panel_token().map(str::to_owned) else {
            return;
        };
        self.status = match self.clipboard.set_text(token) {
            Ok(()) => "remote token copied".into(),
            Err(_) => "could not copy remote token".into(),
        };
        self.toast(self.status.clone());
        self.dirty_frame = true;
    }

    fn activate_remote_panel_row(&mut self, row: usize) {
        self.remote_panel_selected = row;
        match row {
            0 => self.toggle_remote_control(),
            1 => self.begin_remote_edit(RemoteField::Address),
            2 => self.begin_remote_edit(RemoteField::Token),
            3 => self.reveal_remote_token(),
            _ => self.copy_remote_token(),
        }
    }

    /// Handle the footer click and capture input while the sheet is open.
    pub(super) fn remote_panel_event(&mut self, event: &Event) -> bool {
        if matches!(event, Event::FocusLost) {
            if self.remote_panel_open {
                self.close_remote_panel();
            }
            return false;
        }
        if let Event::Resize(width, height) = event {
            if self.remote_panel_open
                && RemotePanelView::geometry(Rect::new(0, 0, *width, *height)).is_none()
            {
                self.close_remote_panel();
            }
            return false;
        }
        if matches!(event, Event::FocusGained) {
            return false;
        }
        if !self.remote_panel_open {
            if self.remote_panel_can_open()
                && let Event::Mouse(mouse) = event
                && mouse.kind == MouseEventKind::Down(MouseButton::Left)
                && within(self.remote_indicator_rect(), mouse.column, mouse.row)
            {
                self.open_remote_panel();
                return true;
            }
            return false;
        }
        if self.remote_edit_event(event) {
            return true;
        }
        match event {
            Event::Key(key) => match panel_key_action(key) {
                Some(PanelKeyAction::Close) => self.close_remote_panel(),
                Some(PanelKeyAction::Reveal) => self.reveal_remote_token(),
                Some(PanelKeyAction::Copy) => self.copy_remote_token(),
                Some(PanelKeyAction::Previous) => {
                    self.remote_panel_selected = self.remote_panel_selected.saturating_sub(1);
                    self.dirty_frame = true;
                }
                Some(PanelKeyAction::Next) => {
                    let last = if self.remote_panel_token().is_some() {
                        4
                    } else {
                        2
                    };
                    self.remote_panel_selected = (self.remote_panel_selected + 1).min(last);
                    self.dirty_frame = true;
                }
                Some(PanelKeyAction::Activate) => {
                    self.activate_remote_panel_row(self.remote_panel_selected);
                }
                _ => {}
            },
            Event::Mouse(mouse) if mouse.kind == MouseEventKind::Down(MouseButton::Left) => {
                let Some(geometry) = RemotePanelView::geometry(self.frame) else {
                    self.close_remote_panel();
                    return true;
                };
                let (x, y) = (mouse.column, mouse.row);
                let rows = if self.remote_panel_token().is_some() {
                    5
                } else {
                    3
                };
                let selected = [
                    geometry.toggle,
                    geometry.address,
                    geometry.token,
                    geometry.reveal,
                    geometry.copy,
                ]
                .into_iter()
                .take(rows)
                .position(|rect| within(rect, x, y));
                if let Some(row) = selected {
                    self.activate_remote_panel_row(row);
                } else if !within(geometry.area, x, y) {
                    self.close_remote_panel();
                }
            }
            _ => {}
        }
        true
    }

    pub(super) fn remote_panel_pointer_shape(&self, x: u16, y: u16) -> Option<&'static str> {
        use super::pointer_shape::{SHAPE_DEFAULT, SHAPE_POINTER};
        if self.remote_panel_open {
            return Some(
                RemotePanelView::geometry(self.frame)
                    .filter(|geometry| {
                        self.remote_edit.is_none()
                            && (within(geometry.toggle, x, y)
                                || (self.remote.is_none()
                                    && (within(geometry.address, x, y)
                                        || within(geometry.token, x, y)))
                                || (self.remote_panel_token().is_some()
                                    && (within(geometry.reveal, x, y)
                                        || within(geometry.copy, x, y))))
                    })
                    .map_or(SHAPE_DEFAULT, |_| SHAPE_POINTER),
            );
        }
        (self.remote_panel_can_open() && within(self.remote_indicator_rect(), x, y))
            .then_some(SHAPE_POINTER)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(buffer: &Buffer, y: u16) -> String {
        (0..buffer.area.width)
            .map(|x| buffer.cell((x, y)).expect("cell").symbol().to_owned())
            .collect()
    }

    #[test]
    fn token_starts_masked_and_reveal_shows_it() {
        let frame = Rect::new(0, 0, 80, 24);
        let geometry = RemotePanelView::geometry(frame).unwrap();
        let theme = Theme::built_in_default();
        let token = "a-visible-test-token";
        let mut buffer = Buffer::empty(frame);
        RemotePanelView {
            theme: &theme,
            address: "127.0.0.1:8888",
            token: Some(token),
            active: true,
            edit: None,
            revealed: false,
            selected: 0,
        }
        .render(frame, &mut buffer);
        assert!(row(&buffer, geometry.token.y).contains("token: ••••••••"));
        assert!(!row(&buffer, geometry.token.y).contains(token));
        RemotePanelView {
            theme: &theme,
            address: "127.0.0.1:8888",
            token: Some(token),
            active: true,
            edit: None,
            revealed: true,
            selected: 0,
        }
        .render(frame, &mut buffer);
        assert!(row(&buffer, geometry.token.y).contains(token));
    }

    #[test]
    fn panel_stays_at_bottom_right_with_room_for_footer() {
        for frame in [Rect::new(0, 0, 80, 24), Rect::new(4, 3, 120, 40)] {
            let geometry = RemotePanelView::geometry(frame).unwrap();
            assert_eq!(geometry.area.right(), frame.right() - 1);
            assert_eq!(geometry.area.bottom(), frame.bottom() - 1);
        }
    }

    #[test]
    fn long_revealed_token_stays_inside_panel() {
        let frame = Rect::new(0, 0, 36, 9);
        let geometry = RemotePanelView::geometry(frame).unwrap();
        let theme = Theme::built_in_default();
        let mut buffer = Buffer::empty(frame);
        RemotePanelView {
            theme: &theme,
            address: "127.0.0.1:8888",
            token: Some(&"x".repeat(256)),
            active: true,
            edit: None,
            revealed: true,
            selected: 0,
        }
        .render(frame, &mut buffer);
        assert!(row(&buffer, geometry.token.y).contains('…'));
        assert_eq!(
            buffer
                .cell((geometry.area.right() - 1, geometry.token.y))
                .unwrap()
                .symbol(),
            "│"
        );
    }

    #[test]
    fn compact_panel_shows_actions_when_indicator_can_fit() {
        let frame = Rect::new(0, 0, 20, 9);
        let geometry = RemotePanelView::geometry(frame).unwrap();
        let theme = Theme::built_in_default();
        let mut buffer = Buffer::empty(frame);
        RemotePanelView {
            theme: &theme,
            address: "127.0.0.1:8888",
            token: Some("a-secret-token"),
            active: true,
            edit: None,
            revealed: false,
            selected: 0,
        }
        .render(frame, &mut buffer);
        assert!(geometry.compact);
        assert!(row(&buffer, geometry.toggle.y).contains("▸ disable"));
        assert!(row(&buffer, geometry.reveal.y).contains("reveal token"));
        assert!(row(&buffer, geometry.copy.y).contains("copy token"));
        assert!(geometry.copy.bottom() < geometry.area.bottom());
        assert_eq!(
            buffer
                .cell((geometry.area.x, geometry.area.y))
                .unwrap()
                .symbol(),
            "╭"
        );
        assert!(RemotePanelView::geometry(Rect::new(0, 0, 20, 8)).is_none());
    }

    #[test]
    fn token_hotkeys_ignore_modifiers_and_repeats() {
        let key = |code, modifiers, kind| KeyEvent::new_with_kind(code, modifiers, kind);
        assert_eq!(
            panel_key_action(&key(
                KeyCode::Char('c'),
                KeyModifiers::NONE,
                KeyEventKind::Press
            )),
            Some(PanelKeyAction::Copy)
        );
        assert_eq!(
            panel_key_action(&key(
                KeyCode::Char('c'),
                KeyModifiers::CONTROL,
                KeyEventKind::Press
            )),
            None
        );
        assert_eq!(
            panel_key_action(&key(
                KeyCode::Char('r'),
                KeyModifiers::ALT,
                KeyEventKind::Press
            )),
            None
        );
        assert_eq!(
            panel_key_action(&key(
                KeyCode::Char('r'),
                KeyModifiers::NONE,
                KeyEventKind::Repeat
            )),
            None
        );
    }
}
