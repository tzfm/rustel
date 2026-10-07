//! A dock as drawn - the widgets stacked down a column or side by side
//! along a band - and the add sheet that offers the kinds.

use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Modifier, Style},
    widgets::Widget,
};

use unicode_width::UnicodeWidthStr;

use super::super::graphics;
use super::super::theme::Theme;
use super::super::visuals::VisualState;
use super::{
    ArtView, EventsView, Look, ScopeView, SpectrumView, VectorView, VizPanel, WidgetKind,
    WidgetSpec,
};

/// The hints that fit `rows` rows of `width` cells, in order, joined by
/// dots: what does not fit is left out rather than cut. A hint too wide
/// for a row of its own is skipped, and the ones after it still get their
/// chance.
pub(super) fn fit_hints(items: &[&str], width: usize, rows: usize) -> Vec<String> {
    const GAP: &str = " · ";
    let mut lines: Vec<String> = Vec::new();
    for item in items {
        let wanted = UnicodeWidthStr::width(*item);
        if let Some(line) = lines.last_mut()
            && UnicodeWidthStr::width(line.as_str()) + UnicodeWidthStr::width(GAP) + wanted <= width
        {
            line.push_str(GAP);
            line.push_str(item);
            continue;
        }
        if lines.len() < rows && wanted <= width {
            lines.push((*item).to_owned());
        }
    }
    lines
}

/// A dock as drawn: its widgets each in a slot - down a column, along a
/// band - the chosen one's header lit while the dock has the keyboard.
pub struct VizDockView<'a> {
    pub panel: &'a VizPanel,
    /// Which dock this is, for its title: the first or the second.
    pub index: usize,
    pub widgets: &'a [WidgetSpec],
    pub set_name: &'a str,
    pub state: &'a VisualState,
    pub theme: &'a Theme,
    /// Whether the pictures follow the mix, hold the last of it, or have
    /// nothing to show yet.
    pub motion: super::Motion,
    /// The mix's level, 0..1.
    pub level: f32,
    /// How long the studio has had sound, in seconds. It stops while the
    /// set does, so an animation stops with it.
    pub seconds: f32,
    /// The room the layout gave the dock.
    pub area: Rect,
    pub focused: bool,
    /// What the mixer widget draws, when a dock has one.
    pub mixer: Option<&'a super::MixerFacts>,
}

impl VizDockView<'_> {
    /// One widget drawn whole into a scratch buffer of its slot's size:
    /// its header on the first row while the dock has the keyboard, its
    /// picture under it.
    fn draw_slot(
        &self,
        index: usize,
        spec: &WidgetSpec,
        size: Rect,
        seconds: f32,
        memory: &mut Vec<f32>,
    ) -> Buffer {
        let theme = self.theme;
        let mut scratch = Buffer::empty(size);
        scratch.set_style(size, Style::default().bg(theme.background));
        let selected = index == self.panel.selected;
        if self.focused {
            let header_style = if selected {
                Style::default()
                    .fg(theme.selection_text)
                    .bg(theme.selection)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(theme.muted)
            };
            let marker = if selected {
                format!("{} ", crate::terminal::symbol("▸"))
            } else {
                "  ".to_owned()
            };
            let title = format!("{marker}{}", spec.title_at(self.panel.edge));
            let padded = format!("{title:<width$}", width = usize::from(size.width));
            scratch.set_stringn(0, 0, &padded, usize::from(size.width), header_style);
        }
        let widget_area = Rect::new(0, 1, size.width, size.height.saturating_sub(1));
        if widget_area.is_empty() {
            return scratch;
        }
        // Nothing playing: a widget that shows the mix shows nothing, and
        // leaves its slot where it is so the dock does not rearrange
        // itself every time the set stops. The art and the spacer stay.
        if !spec.kind.always_visible() && !self.motion.shows_sound() {
            return scratch;
        }
        let look = Look {
            theme,
            colour: spec.colour(),
            seconds,
            level: self.level,
        };
        match spec.kind {
            WidgetKind::Art => ArtView {
                set_name: spec.art_text(self.set_name),
                style: spec.art_style(),
                colour: spec.colour(),
                alignment: spec.alignment,
                edge: self.panel.edge,
                theme,
                seconds,
                level: self.level,
            }
            .render(widget_area, &mut scratch),
            WidgetKind::Scope => ScopeView {
                audio: self.state.audio(),
                motion: self.motion,
                style: spec.scope_style(),
                look,
            }
            .render(widget_area, &mut scratch),
            WidgetKind::Spectrum => SpectrumView {
                bands: self.state.analyser(None),
                history: self.state.spectrogram(None),
                motion: self.motion,
                style: spec.spectrum_style(),
                look,
            }
            .render(widget_area, &mut scratch),
            WidgetKind::Vector => VectorView {
                sides: self.state.sides(),
                motion: self.motion,
                style: spec.vector_style(),
                look,
                memory,
            }
            .render(widget_area, &mut scratch),
            WidgetKind::Events => EventsView {
                state: self.state,
                theme,
            }
            .render(widget_area, &mut scratch),
            WidgetKind::Mixer => super::MixerView {
                facts: self.mixer,
                theme,
            }
            .render(widget_area, &mut scratch),
            WidgetKind::Spacer => {}
        }
        scratch
    }

    /// The hint rows, or the error in their place. The hints are laid
    /// out to the room there is rather than written as two fixed lines: a
    /// column is narrow, and a line cut mid-word says less than a shorter
    /// list of whole ones.
    fn draw_hints(&self, parts: &super::VizParts, buffer: &mut Buffer) {
        let theme = self.theme;
        let width = usize::from(parts.hint.width);
        if let Some(error) = &self.panel.error {
            buffer.set_stringn(
                parts.hint.x,
                parts.hint.y,
                error,
                width,
                Style::default().fg(theme.error),
            );
            return;
        }
        // A band has no title row of its own, so its name heads the hints.
        let title = format!("visuals {}", self.index + 1);
        let alignment = self
            .widgets
            .get(self.panel.selected)
            .filter(|spec| spec.kind == WidgetKind::Art)
            .map(|spec| format!("j align: {}", spec.alignment.name(self.panel.edge)));
        let mut items: Vec<&str> = Vec::new();
        if let Some(hint) = &alignment {
            items.push(hint);
        }
        if parts.title.is_empty() {
            items.push(&title);
        }
        items.extend(if self.widgets.is_empty() {
            ["a adds a widget", "e moves", "+/- size", "Esc"].as_slice()
        } else {
            [
                "Tab picks",
                "↑/↓ kind",
                "←/→ style",
                "Space colour",
                "⇧←/→ order",
                "a adds",
                "Delete removes",
                "Enter text",
                "e moves",
                "+/- size",
                "Esc",
            ]
            .as_slice()
        });
        let items: Vec<String> = items
            .iter()
            .map(|item| super::super::terminal::safe_text(item).into_owned())
            .collect();
        let items: Vec<&str> = items.iter().map(String::as_str).collect();
        for (row, line) in fit_hints(&items, width, usize::from(parts.hint.height))
            .iter()
            .enumerate()
        {
            buffer.set_stringn(
                parts.hint.x,
                parts.hint.y + row as u16,
                line,
                width,
                Style::default().fg(theme.muted),
            );
        }
    }
}

impl Widget for VizDockView<'_> {
    fn render(self, _: Rect, buffer: &mut Buffer) {
        let theme = self.theme;
        let edge = self.panel.edge;
        let Some(parts) = VizPanel::parts(self.area, edge) else {
            return;
        };
        // The dock stands on the theme's own ground, so a theme's picture
        // runs under the widgets at the interface opacity.
        super::super::view::clear_overlay(
            buffer,
            self.area,
            Style::default().bg(theme.background).fg(theme.foreground),
        );
        let rule = Style::default().fg(theme.rule);
        if edge.is_column() {
            for y in parts.rule.y..parts.rule.bottom() {
                buffer.set_stringn(parts.rule.x, y, "│", 1, rule);
            }
        } else {
            buffer.set_stringn(
                parts.rule.x,
                parts.rule.y,
                "─".repeat(usize::from(parts.rule.width)),
                usize::from(parts.rule.width),
                rule,
            );
        }
        // Unfocused, the dock is the pictures and nothing else: no title,
        // no headers, no hints. Focused, the furniture comes out.
        if self.focused && !parts.title.is_empty() {
            buffer.set_stringn(
                parts.title.x,
                parts.title.y,
                format!(" visuals {} ", self.index + 1),
                usize::from(parts.title.width),
                Style::default()
                    .fg(theme.accent)
                    .add_modifier(Modifier::BOLD),
            );
        }
        let body = parts.body;
        let seconds = self.seconds;
        let mut memory = self.panel.memory.borrow_mut();
        if memory.len() < self.widgets.len() {
            memory.resize(self.widgets.len(), Vec::new());
        }
        if edge.is_column() {
            let stack = VizPanel::stack_in(self.widgets, self.set_name, body);
            for (index, (spec, &(top, height))) in self.widgets.iter().zip(&stack).enumerate() {
                let bottom = top.saturating_add(height);
                let visible_top = self.panel.scroll;
                let visible_bottom = self.panel.scroll.saturating_add(body.height);
                if bottom <= visible_top || top >= visible_bottom {
                    continue;
                }
                // Drawn whole, then the rows in view copied over: a widget
                // half scrolled off the top keeps its picture, clipped
                // rather than redrawn smaller.
                let (scratch, images) = graphics::capture_images(|| {
                    self.draw_slot(
                        index,
                        spec,
                        Rect::new(0, 0, body.width, height),
                        seconds,
                        &mut memory[index],
                    )
                });
                let from = visible_top.saturating_sub(top);
                let until = height.min(visible_bottom.saturating_sub(top));
                let source = Rect::new(0, from, body.width, until.saturating_sub(from));
                let destination = (
                    body.x,
                    body.y.saturating_add(top.saturating_sub(visible_top)),
                );
                for row in from..until {
                    let y = destination.1 + (row - from);
                    for column in 0..body.width {
                        if let (Some(from), Some(to)) = (
                            scratch.cell((column, row)),
                            buffer.cell_mut((body.x + column, y)),
                        ) {
                            *to = from.clone();
                        }
                    }
                }
                for image in images {
                    if let Some(image) = image.crop_and_move(source, destination) {
                        graphics::push_image(image);
                    }
                }
            }
        } else {
            let slots = VizPanel::band_slots(self.widgets.len(), body);
            for (index, (spec, slot)) in self.widgets.iter().zip(&slots).enumerate() {
                let size = Rect::new(0, 0, slot.width, slot.height);
                let (scratch, images) = graphics::capture_images(|| {
                    self.draw_slot(index, spec, size, seconds, &mut memory[index])
                });
                for row in 0..slot.height {
                    for column in 0..slot.width {
                        if let (Some(from), Some(to)) = (
                            scratch.cell((column, row)),
                            buffer.cell_mut((slot.x + column, slot.y + row)),
                        ) {
                            *to = from.clone();
                        }
                    }
                }
                for image in images {
                    if let Some(image) = image.crop_and_move(size, (slot.x, slot.y)) {
                        graphics::push_image(image);
                    }
                }
            }
        }
        if self.focused {
            self.draw_hints(&parts, buffer);
        }
    }
}

/// The add sheet: every kind of widget, one a row, to add under the
/// chosen widget. A sheet, so it comes up over the panels like the rest.
pub struct VizAddView<'a> {
    pub panel: &'a VizPanel,
    pub theme: &'a Theme,
}

impl Widget for VizAddView<'_> {
    fn render(self, area: Rect, buffer: &mut Buffer) {
        let Some(chosen) = self.panel.adding else {
            return;
        };
        let Some((sheet, list)) = VizPanel::add_sheet_geometry(area) else {
            return;
        };
        let theme = self.theme;
        super::super::view::clear_overlay(
            buffer,
            sheet,
            Style::default().bg(theme.overlay).fg(theme.foreground),
        );
        super::super::devices::draw_border(buffer, sheet, theme);
        let bold = Style::default()
            .fg(theme.accent)
            .add_modifier(Modifier::BOLD);
        buffer.set_stringn(sheet.x + 2, sheet.y, " add a widget ", 14, bold);
        buffer.set_stringn(
            list.x,
            sheet.y + 1,
            "↑/↓ choose · Enter adds under the chosen widget",
            usize::from(list.width),
            Style::default().fg(theme.muted),
        );
        let first = chosen.saturating_sub(usize::from(list.height).saturating_sub(1));
        for (row, kind) in WidgetKind::ALL
            .iter()
            .enumerate()
            .skip(first)
            .take(usize::from(list.height))
        {
            let selected = row == chosen;
            let style = if selected {
                Style::default()
                    .fg(theme.selection_text)
                    .bg(theme.selection)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(theme.foreground)
            };
            let marker = if selected {
                format!("{} ", crate::terminal::symbol("▸"))
            } else {
                "  ".to_owned()
            };
            let text = format!("{marker}{:<12}{}", kind.name(), kind.describe());
            let padded = format!("{text:<width$}", width = usize::from(list.width));
            buffer.set_stringn(
                list.x,
                list.y + (row - first) as u16,
                &padded,
                usize::from(list.width),
                style,
            );
        }
        buffer.set_stringn(
            list.x,
            list.bottom(),
            "Esc back",
            usize::from(list.width),
            Style::default().fg(theme.muted),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graphics::{PixelImage, Tier};
    use crate::viz_panel::{ART_MAX_LINES, Colouring, Edge, Motion};

    struct GraphicsRestore {
        tier: Tier,
        cells: Option<(u16, u16)>,
        images: Vec<PixelImage>,
    }

    impl GraphicsRestore {
        fn pixels() -> Self {
            let previous = Self {
                tier: graphics::tier(),
                cells: graphics::cell_pixels(),
                images: graphics::take_images(),
            };
            graphics::set_cell_pixels(Some((2, 3)));
            graphics::set_tier(Tier::Pixels);
            previous
        }
    }

    impl Drop for GraphicsRestore {
        fn drop(&mut self) {
            let _ = graphics::take_images();
            graphics::set_tier(self.tier);
            graphics::set_cell_pixels(self.cells);
            for image in self.images.drain(..) {
                graphics::push_image(image);
            }
        }
    }

    #[test]
    fn dock_pixel_images_follow_real_slots_and_crop_scrolled_rgba_rows() {
        let _graphics = GraphicsRestore::pixels();
        let theme = Theme::resolve(None).expect("theme");
        let state = VisualState::default();
        let mut first = WidgetSpec::default_art();
        first.set_text("I");
        first.colour = Some(Colouring::Mono);
        let mut second = first.clone();
        second.set_text("H");
        let widgets = [first, second];
        for (edge, area, scroll) in [
            (Edge::Right, Rect::new(60, 3, 26, 19), 3),
            (Edge::Bottom, Rect::new(5, 20, 70, 10), 0),
        ] {
            let panel = VizPanel {
                edge,
                scroll,
                ..VizPanel::default()
            };
            let view = VizDockView {
                panel: &panel,
                index: 0,
                widgets: &widgets,
                set_name: "set",
                state: &state,
                theme: &theme,
                motion: Motion::Off,
                level: 0.0,
                seconds: 0.0,
                area,
                focused: true,
                mixer: None,
            };
            let parts = VizPanel::parts(area, edge).unwrap();
            let slots: Vec<_> = if edge.is_column() {
                VizPanel::stack_in(&widgets, "set", parts.body)
                    .into_iter()
                    .map(|(top, height)| (Rect::new(0, 0, parts.body.width, height), top))
                    .collect()
            } else {
                VizPanel::band_slots(widgets.len(), parts.body)
                    .into_iter()
                    .map(|slot| (Rect::new(0, 0, slot.width, slot.height), slot.x))
                    .collect()
            };
            let mut expected = Vec::new();
            for (index, &(size, position)) in slots.iter().enumerate() {
                let (_, images) = graphics::capture_images(|| {
                    view.draw_slot(index, &widgets[index], size, 0.0, &mut Vec::new())
                });
                assert_eq!(images.len(), 1, "one canvas per art widget");
                let image = &images[0];
                let (top, bottom, screen_x, screen_y) = if edge.is_column() {
                    let top = scroll.saturating_sub(position).max(1);
                    let bottom = size.height.min(scroll + parts.body.height - position);
                    (
                        top,
                        bottom,
                        parts.body.x,
                        parts.body.y + position + top - scroll,
                    )
                } else {
                    (1, size.height, position, parts.body.y + 1)
                };
                // Expected RGBA is an exact run of full-width source rows,
                // starting below the clipped cell rows and preserving alpha.
                let stride = image.width as usize * 4;
                let pixel_top = usize::from(top - 1) * 3;
                let pixel_bottom = usize::from(bottom - 1) * 3;
                // A canvas names the cells it covers, and the crop carries
                // that naming along with the pixels.
                expected.push(PixelImage {
                    cells: Some((size.width, bottom - top)),
                    ..PixelImage::inline(
                        Rect::new(screen_x, screen_y, size.width, bottom - top),
                        image.width,
                        u32::from(bottom - top) * 3,
                        image.rgba[pixel_top * stride..pixel_bottom * stride].to_vec(),
                    )
                });
            }
            let earlier = PixelImage {
                depth: -1,
                ..PixelImage::inline(Rect::new(1, 1, 1, 1), 1, 1, vec![7, 8, 9, 255])
            };
            graphics::push_image(earlier.clone());
            let mut buffer = Buffer::empty(Rect::new(0, 0, 100, 40));
            view.render(area, &mut buffer);
            let actual = graphics::take_images();
            assert_eq!(actual.len(), 3);
            assert_eq!(
                actual[0], earlier,
                "earlier layers keep their coordinates and order"
            );
            assert_eq!(
                actual[1..],
                expected,
                "{edge:?}: translated placement and cropped source pixels"
            );
            assert!(
                actual[1..]
                    .iter()
                    .all(|image| image.area.intersection(parts.body) == image.area)
            );
        }
    }

    #[test]
    fn oversized_legacy_art_does_not_allocate_a_huge_slot_or_hide_following_widgets() {
        let theme = Theme::resolve(None).expect("theme");
        let state = VisualState::default();
        let mut art = WidgetSpec::default_art();
        art.text = Some("x\n".repeat(65_534));
        let mut widgets = vec![art, WidgetSpec::new(super::super::WidgetKind::Scope)];
        let area = Rect::new(50, 2, 30, 20);
        let mut panel = VizPanel::default();
        let parts = VizPanel::parts(area, panel.edge).unwrap();
        let stack = VizPanel::stack(&widgets, "set", parts.body.width);
        assert_eq!(
            stack[0].1, 2,
            "one header and a refusal message, regardless of source length"
        );
        assert_eq!(stack[1].0, 2, "the next widget remains reachable");
        let mut buffer = Buffer::empty(Rect::new(0, 0, 90, 30));
        VizDockView {
            panel: &panel,
            index: 0,
            widgets: &widgets,
            set_name: "set",
            state: &state,
            theme: &theme,
            motion: Motion::Off,
            level: 0.0,
            seconds: 0.0,
            area,
            focused: true,
            mixer: None,
        }
        .render(area, &mut buffer);
        let text = buffer
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(text.contains("artwork too large"));
        // The largest supported drawing remains scrollable, and the widget
        // after it still maps and renders correctly at the end of the column.
        widgets[0].text = Some("x\n".repeat(ART_MAX_LINES));
        assert_eq!(
            VizPanel::stack(&widgets, "set", parts.body.width)[0].1,
            ART_MAX_LINES as u16 + 1
        );
        panel.selected = 1;
        panel.ensure_visible(&widgets, "set", parts.body);
        let following = VizPanel::stack_in(&widgets, "set", parts.body)[1].0;
        let row = parts.body.y + following - panel.scroll;
        assert_eq!(
            panel.widget_at(&widgets, "set", parts.body, parts.body.x, row),
            Some(1)
        );
        VizDockView {
            panel: &panel,
            index: 0,
            widgets: &widgets,
            set_name: "set",
            state: &state,
            theme: &theme,
            motion: Motion::Off,
            level: 0.0,
            seconds: 0.0,
            area,
            focused: true,
            mixer: None,
        }
        .render(area, &mut buffer);
        // A stale scroll near the integer boundary is harmless until the
        // next layout clamps it; the additions must not overflow.
        panel.scroll = u16::MAX;
        assert_eq!(
            panel.widget_at(&widgets, "set", parts.body, parts.body.x, parts.body.y + 2),
            None
        );
        VizDockView {
            panel: &panel,
            index: 0,
            widgets: &widgets,
            set_name: "set",
            state: &state,
            theme: &theme,
            motion: Motion::Off,
            level: 0.0,
            seconds: 0.0,
            area,
            focused: true,
            mixer: None,
        }
        .render(area, &mut buffer);
    }

    /// The hints fill the rows they are given and stop: never a word cut
    /// in half, never a row over the width, and the order is the order
    /// they were asked for.
    #[test]
    fn hints_fill_the_room_they_are_given() {
        let items = [
            "Tab picks",
            "↑/↓ kind",
            "←/→ style",
            "Space colour",
            "a adds",
        ];
        let lines = fit_hints(&items, 34, 2);
        assert_eq!(
            lines,
            ["Tab picks · ↑/↓ kind · ←/→ style", "Space colour · a adds"]
        );
        assert!(
            lines
                .iter()
                .all(|line| UnicodeWidthStr::width(line.as_str()) <= 34),
            "{lines:?}"
        );
        // One row, and only what fits on it.
        assert_eq!(
            fit_hints(&items, 34, 1),
            ["Tab picks · ↑/↓ kind · ←/→ style"]
        );
        // A wide row takes them all; a hint too wide for any row is left
        // out and the next one still gets in.
        assert_eq!(fit_hints(&items, 200, 2).len(), 1);
        assert_eq!(fit_hints(&["a hint far too wide", "e"], 6, 2), ["e"]);
        assert!(fit_hints(&items, 4, 2).is_empty());
    }
}
