//! The Lanes panel's picture: the lanes view in a band along the top or the
//! bottom, under a rule that names it on the side facing the score. The rows
//! are the ones `lanes()` draws.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::widgets::Widget;

use super::theme::{Theme, VisualOptions};
use super::visuals::{VisualRequest, VisualState};
use super::viz_panel::Edge;

pub struct LanesPanelView<'a> {
    pub edge: Edge,
    pub state: &'a VisualState,
    pub theme: &'a Theme,
    pub focused: bool,
    /// Zen lays the band over the score, and Esc puts it away there.
    pub closes_on_esc: bool,
}

impl Widget for LanesPanelView<'_> {
    fn render(self, area: Rect, buffer: &mut Buffer) {
        if area.width < 8 || area.height < 2 {
            return;
        }
        let theme = self.theme;
        super::view::clear_surface(
            buffer,
            area,
            Style::default().bg(theme.background).fg(theme.foreground),
        );
        let (rule_y, body) = match self.edge {
            Edge::Top => (
                area.bottom() - 1,
                Rect {
                    height: area.height - 1,
                    ..area
                },
            ),
            _ => (
                area.y,
                Rect {
                    y: area.y + 1,
                    height: area.height - 1,
                    ..area
                },
            ),
        };
        for x in area.x..area.right() {
            buffer.set_stringn(x, rule_y, "─", 1, Style::default().fg(theme.rule));
        }
        let title = Style::default().fg(theme.accent);
        let title = if self.focused {
            title.add_modifier(Modifier::BOLD)
        } else {
            title
        };
        buffer.set_stringn(area.x + 2, rule_y, " lanes ", 7, title);
        if self.focused {
            let hint = if self.closes_on_esc {
                " e top/bottom · -/+ height · Esc closes "
            } else {
                " e top/bottom · -/+ height · Esc to the score "
            };
            buffer.set_stringn(
                area.x + 10,
                rule_y,
                hint,
                usize::from(area.width.saturating_sub(10)),
                Style::default().fg(theme.muted),
            );
        }
        super::visuals::render(
            VisualRequest {
                kind: "lanes",
                slot: None,
                options: &VisualOptions::default(),
                state: self.state,
                theme,
                background: theme.background,
                inline: true,
            },
            body,
            buffer,
        );
    }
}
