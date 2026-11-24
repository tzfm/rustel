//! The score outline column: opening it, reading the score when its text
//! changes, and its keys and pointer.

use super::*;
use crate::outline_panel::{OutlinePanel, parts, row_at};

impl App {
    /// View ▸ Score outline. A fixture like the knobs: it reads the score
    /// only while it is open.
    pub(super) fn toggle_outline_panel(&mut self) {
        if self.outline_panel.is_some() {
            self.close_panel(PanelKind::Outline);
            return;
        }
        if self.ui_settings.zen {
            self.status = "outline: zen has no docked furniture - leave zen for the column".into();
            self.dirty_frame = true;
            return;
        }
        self.outline_panel = Some(OutlinePanel::default());
        self.refresh_outline();
        self.invalidate_maps();
        self.focus_panel(PanelKind::Outline);
        self.status = "outline: ↑/↓ node · Enter go to it · Esc back to the score".into();
        self.dirty_frame = true;
    }

    /// Read the outline of the score on screen again when its text
    /// changed. Nothing while the column is closed.
    pub(super) fn refresh_outline(&mut self) {
        let Some(panel) = self.outline_panel.as_mut() else {
            return;
        };
        let scene = self.scenes.current();
        let editor = &scene.editor;
        panel.refresh(scene.id, editor.revision(), || editor.source());
    }

    /// The rows of nodes the column shows at once.
    fn outline_rows(&self) -> usize {
        parts(self.regions.outline).map_or(1, |parts| usize::from(parts.body.height))
    }

    /// Returns true when the column took the key.
    pub(super) fn handle_outline_key(&mut self, code: KeyCode) -> bool {
        self.refresh_outline();
        let rows = self.outline_rows();
        let Some(panel) = self.outline_panel.as_mut() else {
            return false;
        };
        match code {
            KeyCode::Up => panel.step(-1, rows),
            KeyCode::Down => panel.step(1, rows),
            KeyCode::PageUp => panel.step(-(rows as isize), rows),
            KeyCode::PageDown => panel.step(rows as isize, rows),
            KeyCode::Home => panel.jump(0, rows),
            KeyCode::End => panel.jump(usize::MAX, rows),
            KeyCode::Enter => self.go_to_outline_node(),
            KeyCode::Esc => {
                self.focus = Focus::Editor;
                self.status = "keyboard back on the score".into();
            }
            _ => return false,
        }
        self.dirty_frame = true;
        true
    }

    /// Put the caret where the node under the hand starts.
    fn go_to_outline_node(&mut self) {
        let Some(start) = self
            .outline_panel
            .as_ref()
            .and_then(OutlinePanel::selected_start)
        else {
            return;
        };
        let document = self.editor().document();
        let mut at = ByteOffset(start.min(document.len_bytes()));
        while at.0 > 0 && document.validate_caret_offset(at).is_err() {
            at.0 -= 1;
        }
        self.focus = Focus::Editor;
        self.status = "keyboard back on the score".into();
        if self
            .editor_mut()
            .set_selection(super::super::editor::Selection::caret(at))
            .is_ok()
        {
            self.editor_mut().reveal_selection();
            self.invalidate_maps();
        }
    }

    /// A press on a node puts the caret on it, as Enter does.
    pub(super) fn press_outline(&mut self, x: u16, y: u16) -> bool {
        let area = self.regions.outline;
        if self.outline_panel.is_none() || !within(area, x, y) {
            return false;
        }
        self.focus_panel(PanelKind::Outline);
        let rows = self.outline_rows();
        if let Some(panel) = self.outline_panel.as_mut()
            && let Some(row) = row_at(area, panel.scroll, x, y)
            && row < panel.node_count()
        {
            panel.jump(row, rows);
            self.go_to_outline_node();
        }
        self.dirty_frame = true;
        true
    }

    /// The wheel over the column scrolls its nodes.
    pub(super) fn wheel_outline_over(&mut self, x: u16, y: u16, direction: f32) -> bool {
        if !within(self.regions.outline, x, y) {
            return false;
        }
        let rows = self.outline_rows();
        let Some(panel) = self.outline_panel.as_mut() else {
            return false;
        };
        panel.step(if direction < 0.0 { -3 } else { 3 }, rows);
        self.dirty_frame = true;
        true
    }
}
