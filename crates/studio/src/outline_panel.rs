//! The score outline: a column at the score's right edge that shows the
//! score as a tree of its statements and calls, each one in the colour of
//! what runs it, native or the script engine.
//!
//! The outline is read only while the column is open, and again only when
//! the text of the score changes, so a closed column costs nothing.

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Modifier, Style},
    widgets::Widget,
};
use rustel_transpiler::{OutlineForm, OutlineNode};

use super::editor::Revision;
use super::scenes::SceneId;
use super::theme::Theme;

/// The column's width, its rule and its margins included.
pub const OUTLINE_WIDTH: u16 = 34;
/// The least height the column takes: a title, a row, two hint rows.
pub const OUTLINE_HEIGHT: u16 = 6;

/// The open column: the outline of the score on screen, and the row the
/// hand is on.
#[derive(Clone, Debug)]
pub struct OutlinePanel {
    pub selected: usize,
    pub scroll: usize,
    /// The score and the text the outline was read from.
    read_of: Option<(SceneId, Revision)>,
    /// The nodes, or why the text has none.
    pub nodes: Result<Vec<OutlineNode>, String>,
    /// The text changed to one that does not parse, and the nodes are
    /// still those of the last text that did.
    pub stale: bool,
    /// The rows of the statement that holds the caret. While the tree is
    /// stale, those of the last text that parsed.
    pub lit: std::ops::Range<usize>,
}

impl Default for OutlinePanel {
    fn default() -> Self {
        Self {
            selected: 0,
            scroll: 0,
            read_of: None,
            nodes: Ok(Vec::new()),
            stale: false,
            lit: 0..0,
        }
    }
}

impl OutlinePanel {
    /// Read the outline of `source` again when it is another score or
    /// another text than the last one read. The new tree takes the place
    /// of the old one in one step. A text that does not parse, as one
    /// does halfway through a word, keeps the last tree of the same score.
    pub fn refresh(&mut self, scene: SceneId, revision: Revision, source: impl FnOnce() -> String) {
        if self.read_of == Some((scene, revision)) {
            return;
        }
        let same_score = self.read_of.is_some_and(|(read, _)| read == scene);
        if !same_score {
            self.selected = 0;
            self.scroll = 0;
        }
        self.read_of = Some((scene, revision));
        match rustel_transpiler::outline(&source()) {
            Err(_) if same_score && self.nodes.as_ref().is_ok_and(|nodes| !nodes.is_empty()) => {
                self.stale = true;
            }
            read => {
                self.nodes = read;
                self.stale = false;
            }
        }
        self.clamp();
    }

    /// Light the statement that holds the caret's byte. A stale tree keeps
    /// its band: the caret's byte is one of the new text.
    pub fn light(&mut self, caret: usize) {
        if self.stale {
            return;
        }
        self.lit = match &self.nodes {
            Ok(nodes) => statement_rows(nodes, caret),
            Err(_) => 0..0,
        };
    }

    pub fn node_count(&self) -> usize {
        self.nodes.as_ref().map_or(0, Vec::len)
    }

    fn clamp(&mut self) {
        self.selected = self.selected.min(self.node_count().saturating_sub(1));
    }

    /// Move the hand `delta` rows, and keep it in view of `rows` rows.
    pub fn step(&mut self, delta: isize, rows: usize) {
        let last = self.node_count().saturating_sub(1);
        self.selected = self.selected.saturating_add_signed(delta).min(last);
        self.follow(rows);
    }

    /// Move the view `delta` rows, and leave the hand where it is.
    pub fn scroll_by(&mut self, delta: isize, rows: usize) {
        let last = self.node_count().saturating_sub(rows.max(1));
        self.scroll = self.scroll.saturating_add_signed(delta).min(last);
    }

    pub fn jump(&mut self, to: usize, rows: usize) {
        self.selected = to.min(self.node_count().saturating_sub(1));
        self.follow(rows);
    }

    fn follow(&mut self, rows: usize) {
        let rows = rows.max(1);
        if self.selected < self.scroll {
            self.scroll = self.selected;
        } else if self.selected >= self.scroll + rows {
            self.scroll = self.selected + 1 - rows;
        }
    }

    /// The byte of the score where the node under the hand starts.
    pub fn selected_start(&self) -> Option<usize> {
        let nodes = self.nodes.as_ref().ok()?;
        nodes.get(self.selected).map(|node| node.start as usize)
    }
}

/// Whether the native tier can take `node`. A call is native when the
/// score binds its name or the native tier lists it.
pub fn native(node: &OutlineNode) -> bool {
    match &node.form {
        OutlineForm::Read => true,
        OutlineForm::Script => false,
        OutlineForm::Call { bound: true, .. } => true,
        OutlineForm::Call { name, .. } => rustel_jsruntime::native_global(name),
        OutlineForm::Method { name } => rustel_jsruntime::native_method(name),
    }
}

/// The column's rooms: its rule, title, body and hint rows.
pub struct OutlineParts {
    pub rule: Rect,
    pub title: Rect,
    pub body: Rect,
    pub hint: Rect,
}

pub fn parts(area: Rect) -> Option<OutlineParts> {
    if area.height < OUTLINE_HEIGHT || area.width < OUTLINE_WIDTH {
        return None;
    }
    // The rule and a space on the score's side, and a space on the other.
    let inner = Rect::new(
        area.x + 2,
        area.y,
        area.width.saturating_sub(3),
        area.height,
    );
    Some(OutlineParts {
        rule: Rect::new(area.x, area.y, 1, area.height),
        title: Rect::new(inner.x, inner.y, inner.width, 1),
        body: Rect::new(
            inner.x,
            inner.y + 1,
            inner.width,
            inner.height.saturating_sub(3),
        ),
        hint: Rect::new(inner.x, inner.bottom() - 2, inner.width, 2),
    })
}

/// The row of the outline a point is on.
pub fn row_at(area: Rect, scroll: usize, x: u16, y: u16) -> Option<usize> {
    let body = parts(area)?.body;
    let inside = x >= body.x && x < body.right() && y >= body.y && y < body.bottom();
    inside.then(|| scroll + usize::from(y - body.y))
}

pub struct OutlineView<'a> {
    pub panel: &'a OutlinePanel,
    pub theme: &'a Theme,
    pub focused: bool,
    pub area: Rect,
    /// What ran the score last, when the engine has said: native or not.
    pub native: Option<bool>,
}

/// The rows of the top-level statement that holds `caret`: the statement's
/// row and the rows under it.
pub fn statement_rows(nodes: &[OutlineNode], caret: usize) -> std::ops::Range<usize> {
    let tops = || {
        nodes
            .iter()
            .enumerate()
            .filter(|(_, node)| node.depth == 0)
            .map(|(index, _)| index)
    };
    let Some(first) = tops()
        .take_while(|index| nodes[*index].start as usize <= caret)
        .last()
    else {
        return 0..0;
    };
    let end = tops().find(|index| *index > first).unwrap_or(nodes.len());
    first..end
}

impl Widget for OutlineView<'_> {
    fn render(self, _: Rect, buffer: &mut Buffer) {
        let theme = self.theme;
        let Some(parts) = parts(self.area) else {
            return;
        };
        super::view::clear_overlay(
            buffer,
            self.area,
            Style::default().bg(theme.background).fg(theme.foreground),
        );
        let rule = Style::default().fg(theme.rule);
        for y in parts.rule.y..parts.rule.bottom() {
            buffer.set_stringn(parts.rule.x, y, "│", 1, rule);
        }
        let title_style = if self.focused {
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(theme.muted)
        };
        buffer.set_stringn(
            parts.title.x,
            parts.title.y,
            "outline",
            usize::from(parts.title.width),
            title_style,
        );
        // The tree of the last text that parsed, while the new one does not.
        if self.panel.stale {
            buffer.set_stringn(
                parts.title.x + 8,
                parts.title.y,
                "…",
                1,
                Style::default().fg(theme.muted),
            );
        }
        // What ran the score, at the title's end, in its colour.
        if let Some(native) = self.native {
            let word = if native { "native" } else { "script" };
            let x = parts.title.right().saturating_sub(word.len() as u16);
            if x > parts.title.x + 10 {
                buffer.set_stringn(
                    x,
                    parts.title.y,
                    word,
                    word.len(),
                    Style::default().fg(theme.engine(native)),
                );
            }
        }
        let body = parts.body;
        let muted = Style::default().fg(theme.muted);
        match &self.panel.nodes {
            Err(message) => {
                buffer.set_stringn(
                    body.x,
                    body.y,
                    "the score does not parse",
                    usize::from(body.width),
                    muted,
                );
                let message = super::view::ellipsize(message, body.width);
                buffer.set_stringn(body.x, body.y + 1, &message, usize::from(body.width), muted);
            }
            Ok(nodes) if nodes.is_empty() => {
                buffer.set_stringn(body.x, body.y, "- empty -", usize::from(body.width), muted);
            }
            Ok(nodes) => {
                let rows = usize::from(body.height);
                let lit = &self.panel.lit;
                let lane = theme.current_line.unwrap_or(theme.surface);
                for (row, (index, node)) in nodes
                    .iter()
                    .enumerate()
                    .skip(self.panel.scroll)
                    .take(rows)
                    .enumerate()
                {
                    let y = body.y + row as u16;
                    let chosen = self.focused && index == self.panel.selected;
                    let indent = (usize::from(node.depth) * 2).min(usize::from(body.width) / 2);
                    let branch = if node.depth == 0 { "" } else { "· " };
                    let text = format!("{}{branch}{}", " ".repeat(indent), node.label);
                    let text = super::view::ellipsize(&text, body.width);
                    let mut style = Style::default().fg(theme.engine(native(node)));
                    if lit.contains(&index) {
                        style = style.bg(lane);
                        // From the rule to the edge.
                        let x = parts.rule.x + 1;
                        let blank = " ".repeat(usize::from(self.area.right() - x));
                        buffer.set_stringn(x, y, &blank, blank.len(), style);
                    }
                    if chosen {
                        style = style.bg(theme.selection).add_modifier(Modifier::BOLD);
                    }
                    buffer.set_stringn(body.x, y, &text, usize::from(body.width), style);
                }
            }
        }
        let hints: [&str; 2] = if self.focused {
            ["↑/↓ node · Enter go to it", "Esc back to the score"]
        } else {
            ["", ""]
        };
        for (row, hint) in hints.iter().enumerate() {
            buffer.set_stringn(
                parts.hint.x,
                parts.hint.y + row as u16,
                hint,
                usize::from(parts.hint.width),
                muted,
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn panel(source: &str) -> OutlinePanel {
        let mut panel = OutlinePanel::default();
        panel.refresh(SceneId(1), Revision(1), || source.to_owned());
        panel
    }

    #[test]
    fn a_known_call_is_native_and_an_operator_is_not() {
        let panel = panel("$: s(\"bd\").fast(2).nosuchmethod()\nconst a = x + 2");
        let nodes = panel.nodes.as_ref().expect("nodes");
        let said: Vec<_> = nodes
            .iter()
            .map(|node| (node.label.as_str(), native(node)))
            .collect();
        assert_eq!(
            said,
            [
                ("$:", true),
                ("s \"bd\"", true),
                (".fast 2", true),
                (".nosuchmethod", false),
                ("const a", true),
                ("x + 2", false),
            ]
        );
    }

    #[test]
    fn the_outline_is_read_again_only_for_new_text() {
        let mut panel = panel("s(\"bd\")");
        panel.refresh(SceneId(1), Revision(1), || unreachable!("same text"));
        panel.refresh(SceneId(1), Revision(2), || "s(\"bd\")\nn(\"0\")".to_owned());
        assert_eq!(panel.node_count(), 2);
    }

    #[test]
    fn a_text_that_does_not_parse_keeps_the_last_tree() {
        let mut panel = panel("s(\"bd\")\nn(\"0\")");
        panel.refresh(SceneId(1), Revision(2), || "s(\"bd\")\nn(\"0\"".to_owned());
        assert!(panel.stale);
        assert_eq!(panel.node_count(), 2);
        panel.refresh(SceneId(1), Revision(3), || "s(\"bd\")".to_owned());
        assert!(!panel.stale);
        assert_eq!(panel.node_count(), 1);
        // Another score shows why its own text has no tree.
        panel.refresh(SceneId(2), Revision(1), || "s(".to_owned());
        assert!(panel.nodes.is_err());
    }

    #[test]
    fn the_statement_at_the_caret_is_lit() {
        let panel = panel("setcpm(30)\n$: s(\"bd\").fast(2)\n$: n(\"0\")");
        let nodes = panel.nodes.as_ref().expect("nodes");
        assert_eq!(statement_rows(nodes, 0), 0..1);
        assert_eq!(statement_rows(nodes, 15), 1..4);
        assert_eq!(statement_rows(nodes, 999), 4..6);
    }

    #[test]
    fn a_stale_tree_keeps_its_band() {
        let mut panel = panel("$: s(\"bd\")\n$: n(\"0\")");
        panel.light(0);
        assert_eq!(panel.lit, 0..2);
        panel.refresh(SceneId(1), Revision(2), || {
            "$: s(\"bd\").fast(\n$: n(\"0\")".to_owned()
        });
        panel.light(20);
        assert_eq!(panel.lit, 0..2);
    }

    #[test]
    fn the_hand_stays_in_view() {
        let mut panel = panel("a()\nb()\nc()\nd()");
        panel.step(3, 2);
        assert_eq!((panel.selected, panel.scroll), (3, 2));
        panel.step(-3, 2);
        assert_eq!((panel.selected, panel.scroll), (0, 0));
        panel.step(9, 2);
        assert_eq!(panel.selected, 3);
    }
}
