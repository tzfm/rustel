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
                    .expect("the coordinates are inside the buffer")
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
            Event::Key(key) => {
                let alias = self.list_navigation_alias(key);
                match panel_key_action(alias.as_ref().unwrap_or(key)) {
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
                }
            }
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

    #[cfg(feature = "remote-control")]
    mod remote_activity {
        //! The remote activity light follows accepted commands and settles without animation.

        use crate::app::tests::support::{app_over, press};
        use crate::app::*;
        use std::io::{BufRead, BufReader, Write};
        use std::net::TcpStream;

        #[test]
        fn vim_list_keys_do_not_steal_the_remote_sheet() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = app_over(directory.path());
            app.frame = Rect::new(0, 0, 100, 30);
            app.ui_settings.editor_keybindings = crate::editor::EditorKeybindings::Vim;
            app.ui_settings.vim_list_navigation = true;
            app.sync_editor_keybindings();
            app.open_remote_panel();
            assert!(app.remote_panel_open, "the sheet opens");

            let before = app.remote_panel_selected;
            press(&mut app, KeyCode::Char('j'), KeyModifiers::NONE);
            assert_eq!(
                app.remote_panel_selected,
                before + 1,
                "j moves the remote rows"
            );

            app.activate_remote_panel_row(1);
            assert!(app.remote_edit.is_some(), "the address row edits");
            press(&mut app, KeyCode::Char('x'), KeyModifiers::NONE);
            assert_eq!(
                app.remote_edit
                    .as_ref()
                    .expect("edit")
                    .visible_text(64, true),
                "x",
                "the edit takes the letter, not a Vim command"
            );
        }

        #[test]
        fn remote_keys_use_normal_editing_with_vim_on() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = app_over(directory.path());
            app.frame = Rect::new(0, 0, 100, 30);
            app.ui_settings.editor_keybindings = crate::editor::EditorKeybindings::Vim;
            app.sync_editor_keybindings();
            app.focus = Focus::Editor;
            app.editor_mut()
                .set_selection(crate::editor::Selection::caret(crate::editor::ByteOffset(
                    0,
                )))
                .expect("caret at the start");
            app.remote = Some(Box::new(
                crate::remote::listen("127.0.0.1:0".parse().unwrap(), Some("gig1"))
                    .expect("start listener"),
            ));
            let address = app.remote.as_ref().unwrap().address();
            let mut client = TcpStream::connect(address).expect("connect client");
            client
                .set_read_timeout(Some(Duration::from_secs(3)))
                .expect("read timeout");
            let mut reader = BufReader::new(client.try_clone().expect("clone client"));
            let mut reply = String::new();
            writeln!(client, "auth gig1").expect("authenticate");
            reader.read_line(&mut reply).expect("auth reply");
            assert_eq!(reply, "ok\n");

            writeln!(client, "key j").expect("send key");
            reply.clear();
            reader.read_line(&mut reply).expect("key reply");
            assert_eq!(reply, "ok\n");
            app.poll_remote().expect("apply key");
            assert!(
                app.editor().source().starts_with('j'),
                "the remote key typed like normal editing: {:?}",
                app.editor().source()
            );

            // The same holds over a search box: a machine's j types.
            app.ui_settings.vim_list_navigation = true;
            let panel = crate::reference::ReferencePanel::browse(&app.reference);
            app.set_reference_panel(Some(panel));
            app.focus_panel(PanelKind::Reference);
            app.reference_panel
                .as_mut()
                .expect("panel")
                .select_tab(crate::reference::Tab::Chords);

            writeln!(client, "key j").expect("send list letter");
            reply.clear();
            reader.read_line(&mut reply).expect("key reply");
            assert_eq!(reply, "ok\n");
            app.poll_remote().expect("apply list letter");
            assert_eq!(
                app.reference_panel.as_ref().expect("panel").chord_query,
                "j",
                "the remote j types instead of walking the list"
            );

            // The local user keeps their modal keys after the remote key.
            press(&mut app, KeyCode::Char('x'), KeyModifiers::NONE);
            assert_eq!(
                app.reference_panel.as_ref().expect("panel").chord_query,
                "j",
                "local Command still keeps letters out"
            );
        }

        #[test]
        fn remote_commands_light_the_indicator_and_expire_with_one_repaint() {
            let directory = tempfile::tempdir().expect("score directory");
            let mut app = app_over(directory.path());
            app.frame = Rect::new(0, 0, 100, 30);
            app.remote = Some(Box::new(
                crate::remote::listen("127.0.0.1:0".parse().unwrap(), Some("gig1"))
                    .expect("start listener"),
            ));
            let address = app.remote.as_ref().unwrap().address();
            let mut client = TcpStream::connect(address).expect("connect client");
            client
                .set_read_timeout(Some(Duration::from_secs(3)))
                .expect("read timeout");
            let mut reader = BufReader::new(client.try_clone().expect("clone client"));
            let mut reply = String::new();
            writeln!(client, "auth gig1").expect("authenticate");
            reader.read_line(&mut reply).expect("auth reply");
            assert_eq!(reply, "ok\n");
            app.dirty_frame = false;
            app.poll_remote().expect("poll authenticated connection");
            assert!(app.remote.as_ref().unwrap().activity_until.is_none());
            assert!(!app.dirty_frame, "an idle connection must not animate");

            writeln!(client, "key x").expect("send key");
            reply.clear();
            reader.read_line(&mut reply).expect("key reply");
            assert_eq!(reply, "ok\n");
            app.poll_remote().expect("apply key");
            let key_until = app
                .remote
                .as_ref()
                .unwrap()
                .activity_until
                .expect("key lit indicator");
            assert!(app.dirty_frame);

            // A screen request also pulses, while preserving its usual frame pacing.
            app.input_dirty = false;
            app.dirty_frame = false;
            writeln!(client, "screen").expect("request screen");
            let deadline = Instant::now() + Duration::from_secs(3);
            while app.remote.as_ref().unwrap().pending_screens.is_empty() {
                assert!(Instant::now() < deadline, "screen request was not queued");
                app.poll_remote().expect("poll screen request");
                std::thread::sleep(Duration::from_millis(5));
            }
            let screen_until = app
                .remote
                .as_ref()
                .unwrap()
                .activity_until
                .expect("screen lit indicator");
            assert!(screen_until > key_until, "a burst should extend the pulse");
            assert!(app.dirty_frame);
            assert!(
                !app.input_dirty,
                "screen requests must not bypass frame pacing"
            );

            app.dirty_frame = false;
            app.expire_remote_activity(screen_until - Duration::from_millis(1));
            assert!(app.remote.as_ref().unwrap().activity_until.is_some());
            assert!(!app.dirty_frame);
            app.expire_remote_activity(screen_until);
            assert!(app.remote.as_ref().unwrap().activity_until.is_none());
            assert!(
                app.dirty_frame,
                "expiry needs a repaint to dim the indicator"
            );
            app.dirty_frame = false;
            app.expire_remote_activity(screen_until + Duration::from_secs(1));
            assert!(
                !app.dirty_frame,
                "an idle light must not request more frames"
            );

            // Disabling during a pulse drops it; enabling again starts unlit.
            app.remote.as_mut().unwrap().activity_until = Some(screen_until);
            app.options.remote_control = Some("127.0.0.1:0".parse().unwrap());
            app.options.auth_token = Some("gig1".into());
            app.dispatch_menu_action(crate::menu::MenuAction::RemoteControl)
                .expect("open remote controls");
            assert!(app.remote_panel_open);
            press(&mut app, KeyCode::Enter, KeyModifiers::NONE);
            assert!(app.remote.is_none());
            press(&mut app, KeyCode::Enter, KeyModifiers::NONE);
            assert!(app.remote.as_ref().unwrap().activity_until.is_none());
        }
    }

    #[cfg(feature = "remote-control")]
    mod remote_control {
        //! Integration of authenticated automation with the real editor and frame pacing.

        use crate::app::tests::support::{app_over, press};
        use crate::app::*;
        use std::io::{BufRead, BufReader, Write};
        use std::net::{TcpListener, TcpStream};

        #[test]
        fn included_remote_control_stays_dormant_without_runtime_opt_in() {
            let directory = tempfile::tempdir().expect("score directory");
            let mut app = app_over(directory.path());
            assert!(app.options.remote_control.is_none());
            assert!(app.remote.is_none());
            app.input_dirty = false;
            app.dirty_frame = false;
            app.poll_remote().expect("inactive poll");
            assert!(!app.input_dirty && !app.dirty_frame);
        }

        #[test]
        fn options_remote_control_replaces_devices_and_returns_to_editor() {
            use crate::menu::MenuAction;

            let directory = tempfile::tempdir().expect("score directory");
            let mut app = app_over(directory.path());
            app.frame = Rect::new(0, 0, 100, 30);
            app.dispatch_menu_action(MenuAction::Devices)
                .expect("open Devices");
            assert!(app.panel.is_some());
            assert_eq!(app.focus, Focus::Panel(PanelKind::Devices));

            press(&mut app, KeyCode::F(1), KeyModifiers::NONE);
            press(&mut app, KeyCode::Char('o'), KeyModifiers::NONE);
            press(&mut app, KeyCode::Char('r'), KeyModifiers::NONE);
            assert!(app.remote_panel_open, "F1, O, R should open remote control");
            assert!(app.panel.is_none(), "remote control should replace Devices");
            assert!(app.menu.is_none());
            assert_eq!(app.focus, Focus::Editor);

            press(&mut app, KeyCode::Esc, KeyModifiers::NONE);
            assert!(!app.remote_panel_open);
            let before = app.editor().source();
            press(&mut app, KeyCode::Char('x'), KeyModifiers::NONE);
            assert_ne!(app.editor().source(), before, "editor lost keyboard focus");
        }

        #[test]
        fn remote_panel_keys_cancel_armed_quit() {
            let directory = tempfile::tempdir().expect("score directory");
            for code in [KeyCode::Down, KeyCode::Esc] {
                let mut app = app_over(directory.path());
                app.frame = Rect::new(0, 0, 100, 30);
                app.dispatch_menu_action(crate::menu::MenuAction::RemoteControl)
                    .expect("open remote control");
                assert!(app.remote_panel_open);

                press(&mut app, KeyCode::Char('q'), KeyModifiers::CONTROL);
                assert!(app.quit_armed && !app.quit);
                press(&mut app, code, KeyModifiers::NONE);
                assert!(!app.quit_armed, "{code:?} left quit armed behind the panel");
                assert!(!app.quit);
                assert_eq!(app.remote_panel_open, code != KeyCode::Esc);

                press(&mut app, KeyCode::Char('q'), KeyModifiers::CONTROL);
                assert!(app.quit_armed, "a later Ctrl+Q should arm a fresh quit");
                assert!(!app.quit, "a later Ctrl+Q unexpectedly quit after {code:?}");
            }
        }

        #[test]
        fn options_menu_controls_remote_listener_without_a_pointer() {
            use crate::menu::MenuAction;

            let directory = tempfile::tempdir().expect("score directory");
            let mut app = app_over(directory.path());
            app.frame = Rect::new(0, 0, 100, 30);
            let remote_row = |app: &App| {
                app.menus()
                    .into_iter()
                    .find(|menu| menu.title == "Options")
                    .expect("Options menu")
                    .items
                    .into_iter()
                    .find(|item| item.id() == Some(MenuAction::RemoteControl))
            };
            assert!(app.remote.is_none());
            assert!(remote_row(&app).expect("remote control row").enabled);
            press(&mut app, KeyCode::F(1), KeyModifiers::NONE);
            press(&mut app, KeyCode::Char('o'), KeyModifiers::NONE);
            press(&mut app, KeyCode::Char('r'), KeyModifiers::NONE);
            assert!(
                app.remote_panel_open,
                "F1, O, R did not open remote details"
            );
            assert!(
                app.menu.is_none(),
                "the menu should close before the panel opens"
            );
            assert!(app.remote.is_none(), "opening the panel must not enable it");
            assert!(!app.remote_token_revealed);

            // Use an ephemeral port for the toggle, after proving the entry is
            // reachable in an ordinary launch with no --remote-control flag.
            app.options.remote_control = Some("127.0.0.1:0".parse().unwrap());
            press(&mut app, KeyCode::Enter, KeyModifiers::NONE);
            let remote = app.remote.as_ref().expect("Enter enabled listener");
            assert!(remote.address().ip().is_loopback());
            let first_address = remote.address();
            let first_token = remote.token().to_owned();
            let first_token_file = remote
                .token_path()
                .expect("generated token file")
                .to_owned();
            assert!(first_token_file.exists());
            assert!(app.remote_panel_open, "toggle should keep the panel open");

            for _ in 0..3 {
                press(&mut app, KeyCode::Down, KeyModifiers::NONE);
            }
            press(&mut app, KeyCode::Enter, KeyModifiers::NONE);
            assert!(
                app.remote_token_revealed,
                "choosing reveal with the keyboard did not show the token"
            );

            let mut client =
                TcpStream::connect(first_address).expect("connect authenticated client");
            client
                .set_read_timeout(Some(Duration::from_secs(3)))
                .expect("read timeout");
            let mut reader = BufReader::new(client.try_clone().expect("clone client"));
            writeln!(client, "auth {first_token}").expect("authenticate");
            let mut reply = String::new();
            reader.read_line(&mut reply).expect("auth reply");
            assert_eq!(reply, "ok\n");

            for _ in 0..3 {
                press(&mut app, KeyCode::Up, KeyModifiers::NONE);
            }
            press(&mut app, KeyCode::Enter, KeyModifiers::NONE);
            assert!(app.remote.is_none(), "Enter disabled listener");
            assert!(app.remote_panel_open, "toggle should keep the panel open");
            assert!(
                !app.remote_token_revealed,
                "disabling should hide the token"
            );
            assert!(!first_token_file.exists(), "old token file was not removed");
            reply.clear();
            assert_eq!(reader.read_line(&mut reply).expect("client disconnect"), 0);

            press(&mut app, KeyCode::Enter, KeyModifiers::NONE);
            let remote = app.remote.as_ref().expect("Enter re-enabled listener");
            assert_ne!(
                remote.token(),
                first_token,
                "re-enable must issue a fresh code"
            );
            let second_address = remote.address();
            let second_token = remote.token().to_owned();
            let second_token_file = remote.token_path().expect("new token file").to_owned();
            assert!(second_token_file.exists());

            // Both commands reach the same listener before the UI drains its queue.
            // Once the first Enter disables it, the second must not re-enable it.
            let mut queued_client =
                TcpStream::connect(second_address).expect("connect queued client");
            queued_client
                .set_read_timeout(Some(Duration::from_secs(3)))
                .expect("read timeout");
            let mut queued_reader =
                BufReader::new(queued_client.try_clone().expect("clone queued client"));
            writeln!(queued_client, "auth {second_token}").expect("authenticate queued client");
            reply.clear();
            queued_reader.read_line(&mut reply).expect("auth reply");
            assert_eq!(reply, "ok\n");
            for _ in 0..2 {
                writeln!(queued_client, "key enter").expect("queue Enter");
                reply.clear();
                queued_reader
                    .read_line(&mut reply)
                    .expect("queued key reply");
                assert_eq!(reply, "ok\n");
            }
            app.poll_remote().expect("apply queued keys");
            assert!(app.remote.is_none(), "queued Enter re-enabled the listener");
            app.poll_remote().expect("inactive poll after disconnect");
            assert!(app.remote.is_none());
            assert!(!second_token_file.exists());
            press(&mut app, KeyCode::Esc, KeyModifiers::NONE);
            assert!(!app.remote_panel_open);
            assert!(
                !app.remote_token_revealed,
                "closing must hide the token again"
            );
        }

        #[test]
        fn bind_failure_keeps_remote_panel_usable_and_explicit_token_stable() {
            let directory = tempfile::tempdir().expect("score directory");
            let mut app = app_over(directory.path());
            app.frame = Rect::new(0, 0, 100, 30);
            let occupied = TcpListener::bind("127.0.0.1:0").expect("occupy port");
            let address = occupied.local_addr().expect("occupied address");
            app.options.remote_control = Some(address);
            app.options.auth_token = Some("a-long-private-token".into());

            press(&mut app, KeyCode::F(1), KeyModifiers::NONE);
            press(&mut app, KeyCode::Char('o'), KeyModifiers::NONE);
            press(&mut app, KeyCode::Char('r'), KeyModifiers::NONE);
            assert!(app.remote_panel_open);
            press(&mut app, KeyCode::Enter, KeyModifiers::NONE);
            assert!(app.remote.is_none(), "occupied port unexpectedly bound");
            assert!(app.remote_panel_open, "bind failure closed the panel");
            assert_eq!(app.remote_panel_selected, 0);

            drop(occupied);
            press(&mut app, KeyCode::Enter, KeyModifiers::NONE);
            let remote = app.remote.as_ref().expect("retry enabled listener");
            assert_eq!(remote.address(), address);
            assert_eq!(remote.token(), "a-long-private-token");
            assert!(remote.token_path().is_none(), "explicit code needs no file");

            press(&mut app, KeyCode::Enter, KeyModifiers::NONE);
            assert!(app.remote.is_none());
            press(&mut app, KeyCode::Enter, KeyModifiers::NONE);
            assert_eq!(
                app.remote.as_ref().expect("re-enabled").token(),
                "a-long-private-token"
            );
        }

        fn open_remote_sheet(app: &mut App) {
            if app.remote_panel_open {
                press(app, KeyCode::Esc, KeyModifiers::NONE);
            }
            app.frame = Rect::new(0, 0, 100, 30);
            press(app, KeyCode::F(1), KeyModifiers::NONE);
            press(app, KeyCode::Char('o'), KeyModifiers::NONE);
            press(app, KeyCode::Char('r'), KeyModifiers::NONE);
            assert!(app.remote_panel_open);
        }

        fn replace_remote_input(app: &mut App, text: &str) {
            press(app, KeyCode::Char('a'), KeyModifiers::CONTROL);
            app.handle_terminal_event(Event::Paste(text.into()))
                .expect("paste into remote field");
        }

        #[test]
        fn remote_fields_accept_keyboard_edits_and_apply_them_on_enable() {
            let directory = tempfile::tempdir().expect("score directory");
            let mut app = app_over(directory.path());
            app.ui_settings.editor_keybindings = crate::editor::EditorKeybindings::Vim;
            assert_eq!(app.focus, Focus::Editor);
            let before = app.editor().source();
            let reserved = TcpListener::bind("127.0.0.1:0").expect("reserve port");
            let address = reserved.local_addr().expect("reserved address");
            open_remote_sheet(&mut app);

            press(&mut app, KeyCode::Down, KeyModifiers::NONE);
            press(&mut app, KeyCode::Enter, KeyModifiers::NONE);
            press(&mut app, KeyCode::Char('a'), KeyModifiers::CONTROL);
            for character in address.to_string().chars() {
                press(&mut app, KeyCode::Char(character), KeyModifiers::NONE);
            }
            press(&mut app, KeyCode::Enter, KeyModifiers::NONE);
            assert_eq!(app.options.remote_control, Some(address));

            press(&mut app, KeyCode::Tab, KeyModifiers::NONE);
            press(&mut app, KeyCode::Enter, KeyModifiers::NONE);
            let token = "live";
            app.clipboard
                .set_text(token.into())
                .expect("seed clipboard");
            press(&mut app, KeyCode::Char('v'), KeyModifiers::CONTROL);
            press(&mut app, KeyCode::Enter, KeyModifiers::NONE);
            assert_eq!(app.options.auth_token.as_deref(), Some(token));
            assert!(!app.remote_token_revealed, "saving should mask the token");
            assert!(app.remote.is_none(), "editing must not start the listener");
            assert_eq!(
                app.editor().source(),
                before,
                "field edits reached the score"
            );

            for _ in 0..2 {
                press(&mut app, KeyCode::Up, KeyModifiers::NONE);
            }
            drop(reserved);
            press(&mut app, KeyCode::Enter, KeyModifiers::NONE);
            let remote = app.remote.as_ref().expect("Enable uses edited fields");
            assert_eq!(remote.address(), address);
            assert_eq!(remote.token(), token);
            assert!(remote.token_path().is_none());
            press(&mut app, KeyCode::Esc, KeyModifiers::NONE);
            assert!(!app.remote_panel_open);
            assert_eq!(app.editor().source(), before);
        }

        #[test]
        fn remote_field_validation_and_cancel_preserve_saved_configuration() {
            let directory = tempfile::tempdir().expect("score directory");
            let mut app = app_over(directory.path());
            let original_address = "127.0.0.1:9247".parse().unwrap();
            let original_token = "original-private-code";
            app.options.remote_control = Some(original_address);
            app.options.auth_token = Some(original_token.into());
            let before = app.editor().source();
            open_remote_sheet(&mut app);

            press(&mut app, KeyCode::Down, KeyModifiers::NONE);
            press(&mut app, KeyCode::Enter, KeyModifiers::NONE);
            replace_remote_input(&mut app, "127.0.0.1:0");
            press(&mut app, KeyCode::Enter, KeyModifiers::NONE);
            assert_eq!(app.options.remote_control, Some(original_address));
            assert!(app.remote_panel_open);

            // A validation failure retains the draft so it can be corrected in place.
            let updated_address = "127.0.0.1:9357".parse().unwrap();
            replace_remote_input(&mut app, "127.0.0.1:9357");
            press(&mut app, KeyCode::Enter, KeyModifiers::NONE);
            assert_eq!(app.options.remote_control, Some(updated_address));

            press(&mut app, KeyCode::Down, KeyModifiers::NONE);
            press(&mut app, KeyCode::Enter, KeyModifiers::NONE);
            replace_remote_input(&mut app, "abc");
            press(&mut app, KeyCode::Enter, KeyModifiers::NONE);
            assert_eq!(app.options.auth_token.as_deref(), Some(original_token));
            press(&mut app, KeyCode::Esc, KeyModifiers::NONE);
            assert!(app.remote_panel_open, "Escape should first cancel the edit");
            assert_eq!(app.options.auth_token.as_deref(), Some(original_token));

            press(&mut app, KeyCode::Enter, KeyModifiers::NONE);
            replace_remote_input(&mut app, "discard-this-private-code");
            app.handle_terminal_event(Event::FocusLost)
                .expect("focus lost");
            assert!(!app.remote_panel_open);
            assert!(!app.remote_token_revealed);
            assert_eq!(app.options.auth_token.as_deref(), Some(original_token));
            open_remote_sheet(&mut app);
            assert_eq!(app.remote_panel_selected, 0);
            assert_eq!(
                app.editor().source(),
                before,
                "cancelled edits reached the score"
            );
        }

        #[test]
        fn clearing_the_remote_token_restores_generated_codes() {
            let directory = tempfile::tempdir().expect("score directory");
            let mut app = app_over(directory.path());
            // Port zero is assigned directly only for the test's listener; the UI
            // rejects it because a user must choose a stable destination port.
            app.options.remote_control = Some("127.0.0.1:0".parse().unwrap());
            app.options.auth_token = Some("replace-this-private-code".into());
            open_remote_sheet(&mut app);
            for _ in 0..2 {
                press(&mut app, KeyCode::Tab, KeyModifiers::NONE);
            }
            press(&mut app, KeyCode::Enter, KeyModifiers::NONE);
            press(&mut app, KeyCode::Char('a'), KeyModifiers::CONTROL);
            press(&mut app, KeyCode::Backspace, KeyModifiers::NONE);
            press(&mut app, KeyCode::Enter, KeyModifiers::NONE);
            assert!(app.options.auth_token.is_none());
            assert!(!app.remote_token_revealed);

            for _ in 0..2 {
                press(&mut app, KeyCode::Up, KeyModifiers::NONE);
            }
            press(&mut app, KeyCode::Enter, KeyModifiers::NONE);
            let remote = app.remote.as_ref().expect("generated-code listener");
            assert!(remote.token_path().is_some());
            assert_eq!(remote.token().len(), 14);
        }

        #[test]
        fn running_remote_fields_require_disabling_before_edits() {
            let directory = tempfile::tempdir().expect("score directory");
            let mut app = app_over(directory.path());
            app.options.remote_control = Some("127.0.0.1:0".parse().unwrap());
            app.options.auth_token = Some("running-private-code".into());
            open_remote_sheet(&mut app);
            press(&mut app, KeyCode::Enter, KeyModifiers::NONE);
            let address = app.remote.as_ref().expect("enabled listener").address();
            let configured_address = app.options.remote_control;
            let configured_token = app.options.auth_token.clone();

            for (row, replacement) in [(1, "127.0.0.1:9357"), (2, "new-private-code-for-later")] {
                open_remote_sheet(&mut app);
                for _ in 0..row {
                    press(&mut app, KeyCode::Down, KeyModifiers::NONE);
                }
                press(&mut app, KeyCode::Enter, KeyModifiers::NONE);
                replace_remote_input(&mut app, replacement);
                press(&mut app, KeyCode::Esc, KeyModifiers::NONE);
                assert!(
                    !app.remote_panel_open,
                    "active field unexpectedly began editing"
                );
                assert_eq!(app.options.remote_control, configured_address);
                assert_eq!(app.options.auth_token, configured_token);
                let remote = app.remote.as_ref().expect("listener should stay running");
                assert_eq!(remote.address(), address);
                assert_eq!(remote.token(), "running-private-code");
            }
        }

        fn remote_sheet_buffer(app: &App) -> ratatui::buffer::Buffer {
            let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(100, 30))
                .expect("test terminal");
            terminal
                .draw(|frame| app.paint_remote_panel(frame))
                .expect("paint remote sheet");
            terminal.backend().buffer().clone()
        }

        fn remote_sheet_text(app: &App) -> String {
            remote_sheet_buffer(app)
                .content
                .iter()
                .map(|cell| cell.symbol())
                .collect()
        }

        #[test]
        fn remote_token_drafts_are_masked_until_revealed_and_remasked_on_save() {
            let directory = tempfile::tempdir().expect("score directory");
            let mut app = app_over(directory.path());
            let saved_token = "original-private-code";
            let draft_token = "new-secret-for-sharing";
            app.options.auth_token = Some(saved_token.into());
            open_remote_sheet(&mut app);
            for _ in 0..2 {
                press(&mut app, KeyCode::Down, KeyModifiers::NONE);
            }
            press(&mut app, KeyCode::Enter, KeyModifiers::NONE);
            assert!(!remote_sheet_text(&app).contains(saved_token));
            replace_remote_input(&mut app, draft_token);
            assert!(!remote_sheet_text(&app).contains(draft_token));
            let buffer = remote_sheet_buffer(&app);
            let mask_start = buffer
                .content
                .iter()
                .position(|cell| cell.symbol() == "•")
                .expect("draft uses bullet masking");
            let label_style = buffer.content[mask_start - 3].style();
            for cell in &buffer.content[mask_start..mask_start + draft_token.len()] {
                assert_eq!(cell.symbol(), "•");
                assert_eq!(cell.style(), label_style, "mask keeps the row styling");
            }

            press(&mut app, KeyCode::Char('r'), KeyModifiers::ALT);
            assert!(remote_sheet_text(&app).contains(draft_token));
            // Plain letters remain token input, even when they are sheet shortcuts.
            press(&mut app, KeyCode::Char('r'), KeyModifiers::NONE);
            press(&mut app, KeyCode::Char('c'), KeyModifiers::NONE);
            let final_token = format!("{draft_token}rc");
            assert!(remote_sheet_text(&app).contains(&final_token));
            press(&mut app, KeyCode::Char('r'), KeyModifiers::ALT);
            assert!(!remote_sheet_text(&app).contains(&final_token));

            press(&mut app, KeyCode::Char('r'), KeyModifiers::ALT);
            press(&mut app, KeyCode::Enter, KeyModifiers::NONE);
            assert_eq!(
                app.options.auth_token.as_deref(),
                Some(final_token.as_str())
            );
            assert!(!app.remote_token_revealed);
            assert!(!remote_sheet_text(&app).contains(&final_token));

            press(&mut app, KeyCode::Enter, KeyModifiers::NONE);
            press(&mut app, KeyCode::Char('r'), KeyModifiers::ALT);
            assert!(remote_sheet_text(&app).contains(&final_token));
            press(&mut app, KeyCode::Esc, KeyModifiers::NONE);
            assert!(app.remote_panel_open);
            assert!(!app.remote_token_revealed);
            assert!(!remote_sheet_text(&app).contains(&final_token));
        }

        #[test]
        fn remote_fields_reject_unsafe_or_oversized_pastes_without_partial_changes() {
            let directory = tempfile::tempdir().expect("score directory");
            let mut app = app_over(directory.path());
            let address = "[::1]:9357";
            let token = "original-private-code";
            app.options.remote_control = Some(address.parse().unwrap());
            app.options.auth_token = Some(token.into());
            let before = app.editor().source();
            open_remote_sheet(&mut app);

            for (expected, limit) in [(address, 64), (token, 256)] {
                press(&mut app, KeyCode::Down, KeyModifiers::NONE);
                press(&mut app, KeyCode::Enter, KeyModifiers::NONE);
                for invalid in [
                    "valid-prefix-é".to_owned(),
                    "valid-prefix-😀".to_owned(),
                    "valid-prefix-\nsecond-line".to_owned(),
                    "valid-prefix-\0suffix".to_owned(),
                    "valid-prefix- suffix".to_owned(),
                    "x".repeat(limit + 1),
                ] {
                    replace_remote_input(&mut app, &invalid);
                    let edit = app.remote_edit.as_ref().expect("still editing");
                    assert_eq!(edit.visible_text(512, true), expected);
                    assert!(edit.error.is_some(), "invalid paste needs feedback");
                    assert!(edit.selected, "rejected paste consumed the selection");
                    // Rendering the rejected Unicode must also remain safe.
                    let _ = remote_sheet_text(&app);
                }
                press(&mut app, KeyCode::Enter, KeyModifiers::NONE);
            }
            assert_eq!(app.options.remote_control, Some(address.parse().unwrap()));
            assert_eq!(app.options.auth_token.as_deref(), Some(token));

            // Appending to a full token is rejected too, without a partial suffix.
            press(&mut app, KeyCode::Enter, KeyModifiers::NONE);
            let full_token = "x".repeat(256);
            replace_remote_input(&mut app, &full_token);
            press(&mut app, KeyCode::End, KeyModifiers::NONE);
            app.handle_terminal_event(Event::Paste("yz".into()))
                .expect("reject overflowing append");
            press(&mut app, KeyCode::Enter, KeyModifiers::NONE);
            assert_eq!(app.options.auth_token.as_deref(), Some(full_token.as_str()));
            assert_eq!(app.editor().source(), before);
        }

        #[test]
        fn remote_token_cursor_edits_and_shift_tab_stay_in_the_sheet() {
            let directory = tempfile::tempdir().expect("score directory");
            let mut app = app_over(directory.path());
            app.options.auth_token = Some("xabcdefghijklmnopz".into());
            let before = app.editor().source();
            open_remote_sheet(&mut app);
            for _ in 0..2 {
                press(&mut app, KeyCode::Tab, KeyModifiers::NONE);
            }
            press(&mut app, KeyCode::Enter, KeyModifiers::NONE);
            for code in [
                KeyCode::Home,
                KeyCode::Backspace, // no character before the start
                KeyCode::Delete,    // remove the leading x
                KeyCode::End,
                KeyCode::Delete,    // no character after the end
                KeyCode::Backspace, // remove the trailing z
                KeyCode::Left,
                KeyCode::Char('-'),
                KeyCode::Right,
                KeyCode::Backspace, // replace the p after the inserted separator
                KeyCode::Char('q'),
                KeyCode::Enter,
            ] {
                press(&mut app, code, KeyModifiers::NONE);
            }
            assert_eq!(app.options.auth_token.as_deref(), Some("abcdefghijklmno-q"));
            assert_eq!(app.remote_panel_selected, 2);
            press(&mut app, KeyCode::BackTab, KeyModifiers::SHIFT);
            assert_eq!(app.remote_panel_selected, 1);
            press(&mut app, KeyCode::BackTab, KeyModifiers::NONE);
            assert_eq!(app.remote_panel_selected, 0);
            assert_eq!(app.editor().source(), before);
        }

        #[test]
        fn authenticated_keys_and_concurrent_snapshots_preserve_frame_pacing() {
            let directory = tempfile::tempdir().expect("score directory");
            let mut app = app_over(directory.path());
            let remote =
                crate::remote::listen("127.0.0.1:0".parse().unwrap(), None).expect("listener");
            let token =
                std::fs::read_to_string(remote.token_path().unwrap()).expect("launch token");
            let mut client = TcpStream::connect(remote.address()).expect("connect");
            client
                .set_read_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            let mut reader = BufReader::new(client.try_clone().unwrap());
            app.remote = Some(Box::new(remote));
            writeln!(client, "auth {}", token.trim()).unwrap();
            let mut reply = String::new();
            reader.read_line(&mut reply).unwrap();
            assert_eq!(reply, "ok\n");

            let before = app.editor().source();
            writeln!(client, "key x").unwrap();
            reply.clear();
            reader.read_line(&mut reply).unwrap();
            assert_eq!(reply, "ok\n");
            app.poll_remote().expect("apply acknowledged key");
            assert_ne!(app.editor().source(), before);
            assert!(app.input_dirty);

            let mut other = TcpStream::connect(app.remote.as_ref().unwrap().address()).unwrap();
            other
                .set_read_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            let mut other_reader = BufReader::new(other.try_clone().unwrap());
            writeln!(other, "auth {}", token.trim()).unwrap();
            reply.clear();
            other_reader.read_line(&mut reply).unwrap();
            assert_eq!(reply, "ok\n");

            // An abandoned request must not stop either client getting a frame.
            let (stale, abandoned) = std::sync::mpsc::channel();
            app.remote.as_mut().unwrap().queue_screen(stale);
            drop(abandoned);
            app.input_dirty = false;
            app.dirty_frame = false;
            writeln!(client, "screen").unwrap();
            writeln!(other, "screen").unwrap();
            let deadline = Instant::now() + Duration::from_secs(3);
            while app.remote.as_ref().unwrap().pending_screens.len() < 3 {
                assert!(
                    Instant::now() < deadline,
                    "screen requests did not reach UI"
                );
                app.poll_remote().expect("snapshot poll");
                std::thread::sleep(Duration::from_millis(5));
            }
            assert!(!app.input_dirty, "screen must not bypass animation pacing");
            assert!(app.dirty_frame);

            let mut buffer = ratatui::buffer::Buffer::empty(Rect::new(0, 0, 5, 1));
            buffer[(0, 0)].set_symbol("x");
            app.reply_remote_screen(&buffer);
            for reader in [&mut reader, &mut other_reader] {
                reply.clear();
                reader.read_line(&mut reply).unwrap();
                let screen: serde_json::Value =
                    serde_json::from_str(reply.strip_prefix("screen ").expect("snapshot reply"))
                        .unwrap();
                assert_eq!(screen["cols"], 5);
                assert!(screen["text"].as_str().unwrap().starts_with('x'));
            }
            assert!(app.remote.as_ref().unwrap().pending_screens.is_empty());
        }
    }
}
