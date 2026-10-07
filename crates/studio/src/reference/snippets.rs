//! Example tree navigation and generator controls.

use super::*;

impl ReferencePanel {
    #[cfg(feature = "hydra")]
    pub fn generator_row(&self) -> Option<super::super::ideas::Row> {
        (self.tab == Tab::Generator)
            .then(|| self.generator.rows().get(self.snippet_selected).copied())
            .flatten()
    }

    #[cfg(feature = "hydra")]
    pub fn snippet_layout(&self, inner: Rect) -> SnippetLayout {
        let mut layout = snippet_layout(inner, self.shows_picture());
        if self.tab == Tab::Generator {
            // Two footer rows and a taller code pane; all hit-testing uses
            // this same geometry as the renderer.
            layout.scope = layout.footer.saturating_sub(1).max(inner.y);
            let top = inner.y.saturating_add(1).min(layout.scope);
            let height = layout.scope.saturating_sub(top);
            let code_height = ((height * 45).div_ceil(100) + 2).min(height.saturating_sub(6));
            layout.list = Rect::new(inner.x, top, inner.width, height - code_height);
            layout.code = Rect::new(inner.x, layout.list.bottom(), inner.width, code_height);
        }
        layout
    }

    #[cfg(feature = "hydra")]
    pub fn generator_activate(&mut self) -> PanelAction {
        use super::super::ideas::Row;
        let changed = match self.generator_row() {
            Some(Row::Direction(direction)) => {
                let changed = self.generator.choose(direction);
                self.snippet_selected = self
                    .generator
                    .rows()
                    .iter()
                    .position(|row| *row == Row::Direction(direction))
                    .unwrap_or(0);
                changed
            }
            Some(Row::Generate(_)) => {
                self.generator.fresh();
                true
            }
            Some(Row::Similar) => {
                self.generator.similar();
                true
            }
            Some(Row::Copy) => return self.copy(),
            Some(Row::ControlsTop | Row::ControlsBottom) => false,
            Some(Row::Control(_)) => return self.copy(),
            None => false,
        };
        if changed {
            self.selection = None;
            self.snippet_code_scroll.set(0);
            PanelAction::GeneratorChanged
        } else {
            PanelAction::Nothing
        }
    }

    #[cfg(feature = "hydra")]
    pub fn generator_history_at(
        &self,
        inner: Rect,
        x: u16,
        y: u16,
    ) -> Option<(super::super::ideas::Action, bool, usize)> {
        if self.tab != Tab::Generator {
            return None;
        }
        let geometry = self.geometry(inner);
        if !geometry.list.contains((x, y).into()) {
            return None;
        }
        let index = geometry.first_row + usize::from(y - geometry.list.y);
        let action = self.generator.rows().get(index)?.action()?;
        let next_column =
            5 + UnicodeWidthStr::width(generator_action_label(action).as_str()) as u16;
        match x.saturating_sub(geometry.list.x) {
            // Consume the disabled back slot too, so it cannot fall through
            // to the row's Generate action when there is no earlier result.
            2 => Some((action, false, index)),
            column if column == next_column => Some((action, true, index)),
            _ => None,
        }
    }

    #[cfg(feature = "hydra")]
    pub fn generator_control_at(&self, inner: Rect, x: u16, y: u16) -> Option<(usize, Rect)> {
        if self.tab != Tab::Generator {
            return None;
        }
        let geometry = self.geometry(inner);
        if !geometry.list.contains((x, y).into()) {
            return None;
        }
        let row = geometry.first_row + usize::from(y - geometry.list.y);
        match self.generator.rows().get(row) {
            Some(super::super::ideas::Row::Control(index)) => {
                let rail = generator_rail(Rect::new(geometry.list.x, y, geometry.list.width, 1));
                rail.contains((x, y).into()).then_some((*index, rail))
            }
            _ => None,
        }
    }

    /// Every line the snippets tab draws, in order.
    #[cfg(feature = "hydra")]
    pub fn snippet_lines(&self) -> Vec<SnippetLine> {
        if self.tab == Tab::Generator {
            return self
                .generator
                .rows()
                .into_iter()
                .map(SnippetLine::Generator)
                .collect();
        }
        // Sections contain shelves, and expanded shelves contain examples.
        let mut lines = Vec::new();
        for (section_index, section) in super::super::examples::SECTIONS.iter().enumerate() {
            lines.push(SnippetLine::Section(section_index));
            if !self.section_open.contains(&section_index) {
                continue;
            }
            for (shelf_index, shelf) in section.shelves.iter().enumerate() {
                lines.push(SnippetLine::Shelf(section_index, shelf_index));
                if !self.snippet_open.contains(&(section_index, shelf_index)) {
                    continue;
                }
                for snippet in 0..shelf.snippets.len() {
                    lines.push(SnippetLine::Snippet(section_index, shelf_index, snippet));
                }
            }
        }
        lines
    }

    #[cfg(feature = "hydra")]
    pub(super) fn toggle_section(&mut self, section: usize) {
        if !self.section_open.remove(&section) {
            self.section_open.insert(section);
        }
    }

    #[cfg(feature = "hydra")]
    pub(super) fn toggle_shelf(&mut self, section: usize, shelf: usize) {
        if !self.snippet_open.remove(&(section, shelf)) {
            self.snippet_open.insert((section, shelf));
        }
    }

    /// Whether the cursor is within the Hydra branch. A selected sketch
    /// temporarily supplies the studio's background.
    #[cfg(feature = "hydra")]
    pub fn shows_picture(&self) -> bool {
        if self.tab != Tab::Examples {
            return false;
        }
        match self.snippet_lines().get(self.snippet_selected) {
            Some(
                SnippetLine::Section(section)
                | SnippetLine::Shelf(section, _)
                | SnippetLine::Snippet(section, ..),
            ) => super::super::examples::SECTIONS
                .get(*section)
                .is_some_and(|section| section.kind == super::super::examples::Kind::Hydra),
            _ => false,
        }
    }

    /// The snippet under the cursor: the catalogue's own on a snippet line,
    /// and nothing on a heading.
    #[cfg(feature = "hydra")]
    pub fn selected_snippet(&self) -> Option<&'static super::super::examples::Snippet> {
        match self.snippet_lines().get(self.snippet_selected).copied() {
            Some(SnippetLine::Snippet(section, shelf, index)) => super::super::examples::SECTIONS
                .get(section)
                .and_then(|section| section.shelves.get(shelf))
                .and_then(|shelf| shelf.snippets.get(index)),
            _ => None,
        }
    }

    /// What the row under the cursor is written in - a Hydra chain gets a
    /// picture and can be a theme's sketch, a score line neither. Nothing
    /// on a heading.
    #[cfg(feature = "hydra")]
    pub fn selected_snippet_kind(&self) -> Option<super::super::examples::Kind> {
        if self.tab == Tab::Generator {
            return Some(super::super::examples::Kind::Music);
        }
        match self.snippet_lines().get(self.snippet_selected).copied() {
            Some(SnippetLine::Snippet(section, ..)) => super::super::examples::SECTIONS
                .get(section)
                .map(|section| section.kind),
            _ => None,
        }
    }

    /// The code to preview and to copy.
    #[cfg(feature = "hydra")]
    pub fn selected_snippet_code(&self) -> Option<std::borrow::Cow<'_, str>> {
        let code = if self.tab == Tab::Generator {
            self.generator.code.as_str()
        } else {
            self.selected_snippet()?.code
        };
        // One `$:` a voice is how a set is played; folded into a stack is
        // how one is taken somewhere that wants a single pattern. The
        // switch decides which of the two the reader is looking at, so what
        // is on screen and what is copied are the same music either way.
        let reread = if super::super::settings::examples_stack() {
            super::super::examples::stacked(code)
        } else {
            super::super::examples::laned(code)
        };
        if reread != code {
            return Some(std::borrow::Cow::Owned(reread));
        }
        Some(std::borrow::Cow::Borrowed(code))
    }
}
