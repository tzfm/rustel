//! Pixel delivery of the global stage, with the same exclusions as the
//! cell painter. Stage images are prepared before frame effects change
//! backgrounds, then placed above backdrops and below the score's glyphs.

use super::*;
use crate::graphics::{self, PixelImage};
use ratatui::style::Color;
use std::collections::HashSet;

pub(super) fn prepare(
    images: Vec<PixelImage>,
    frame: &Buffer,
    background: Color,
    covered: &[Rect],
    untouched: &HashSet<(u16, u16)>,
    strength: f32,
) -> Vec<PixelImage> {
    if images.is_empty() || !strength.is_finite() || strength <= 0.0 {
        return Vec::new();
    }
    let strength = strength.min(1.0);
    // Keep masks local to the stage. Inline pictures already drawn for
    // this frame retain their pixels and their place in the layer order.
    let (_, images) = graphics::capture_images(|| {
        for mut image in images {
            image.depth = -1;
            for pixel in image.rgba.as_chunks_mut::<4>().0 {
                pixel[3] = (f32::from(pixel[3]) * strength).round() as u8;
            }
            graphics::push_image(image);
        }
        for &area in covered {
            graphics::cover_images(area);
        }
        // A whole selected line or panel is one mask per row, rather than
        // one queue lock for every cell in it.
        for y in frame.area.y..frame.area.bottom() {
            let mut start = None;
            for x in frame.area.x..frame.area.right() {
                let cell = frame.cell((x, y)).expect("frame cell");
                let blocked = untouched.contains(&(x, y))
                    || (cell.bg != background && cell.bg != Color::Reset);
                if blocked {
                    start.get_or_insert(x);
                } else if let Some(from) = start.take() {
                    graphics::cover_images(Rect::new(from, y, x - from, 1));
                }
            }
            if let Some(from) = start {
                graphics::cover_images(Rect::new(from, y, frame.area.right() - from, 1));
            }
        }
    });
    images
}

/// The shelf is transmitted after the text frame, but belongs at the
/// reference panel's place in the stack. Mask only its own late image.
#[cfg(feature = "hydra")]
pub(super) fn mask_shelf(image: PixelImage, covered: &[Rect]) -> Option<PixelImage> {
    let (_, mut images) = graphics::capture_images(|| {
        graphics::push_image(image);
        for &area in covered {
            graphics::cover_images(area);
        }
    });
    images.pop()
}

impl App {
    /// Surfaces painted after the reference panel by `view::render` and
    /// `App::draw`. A reference raised above sheets keeps its full preview.
    #[cfg(feature = "hydra")]
    pub(super) fn shelf_exclusions(&self, theme_editor_visible: bool) -> Vec<Rect> {
        let mut covered = Vec::new();
        covered.extend(self.piano_zen_row());
        if !self.reference_on_top() {
            if self.log_panel.is_some()
                && let Some((sheet, _)) =
                    super::super::log::LogPanelView::geometry(self.frame, self.log_extent())
            {
                covered.push(sheet);
            }
            if self.export_sheet.is_some()
                && let Some(sheet) = super::super::export::ExportSheetView::geometry(self.frame)
            {
                covered.push(sheet);
            }
            if self.settings_sheet.is_some()
                && let Some((sheet, _)) =
                    super::super::settings::SettingsSheetView::geometry(self.frame)
            {
                covered.push(sheet);
            }
            if let Some(panel) = self.panel {
                let entries = self.panel_row_count(panel.kind).max(1);
                if let Some(geometry) = DevicePanelView::geometry(self.frame, panel, entries) {
                    covered.push(geometry.area);
                }
            }
            if let Some((sheet, _)) = self
                .theme_picker
                .as_ref()
                .and_then(|picker| picker.geometry(self.frame))
            {
                covered.push(sheet);
            }
        }
        if let Some((_, picker)) = &self.set_prompt
            && let Some((sheet, _)) = picker.geometry(self.frame)
        {
            covered.push(sheet);
        }
        if self
            .viz_docks
            .iter()
            .flatten()
            .any(|panel| panel.adding.is_some())
            && let Some((sheet, _)) = VizPanel::add_sheet_geometry(self.frame)
        {
            covered.push(sheet);
        }
        if let Some((sheet, _)) = self
            .viz_docks
            .iter()
            .flatten()
            .find_map(|panel| panel.prompt.as_ref())
            .and_then(|picker| picker.geometry(self.frame))
        {
            covered.push(sheet);
        }
        if theme_editor_visible
            && let Some(sheet) = super::super::theme_editor::ThemeEditorView::geometry(self.frame)
        {
            covered.push(sheet);
        }
        if let Some(menu) = &self.menu {
            covered.extend(super::super::menu::dropdown_rects(
                &self.menus(),
                menu,
                self.menu_row(),
                self.frame,
            ));
        }
        // Toasts are drawn after every panel, at the first row below chrome.
        if let Some((text, _)) = &self.toast {
            let width =
                (unicode_width::UnicodeWidthStr::width(text.as_str()) as u16).saturating_add(4);
            if self.frame.width >= width && self.frame.height >= 3 {
                let top = self
                    .regions
                    .header
                    .bottom()
                    .max(self.menu_row().bottom())
                    .max(self.frame.y + 1);
                covered.push(Rect::new(
                    self.frame.x + self.frame.width.saturating_sub(width) / 2,
                    top.min(self.frame.bottom().saturating_sub(1)),
                    width,
                    1,
                ));
            }
        }
        covered.retain(|area| !area.is_empty());
        covered
    }

    /// Floating sheets and inline pictures are explicit exclusions even on
    /// themes that give them the same background as the editor.
    pub(super) fn stage_exclusions(&self, maps: &[ScreenMap]) -> Vec<Rect> {
        let mut covered = Vec::new();
        for (index, map) in maps.iter().enumerate() {
            let area = self.regions.panes[index].editor;
            for row in map.rows() {
                if let super::super::editor::ScreenRow::Virtual(row) = row {
                    covered.push(Rect::new(area.x, row.screen_y, area.width, 1));
                }
            }
        }
        if let Some(panel) = self.panel {
            let entries = self.panel_row_count(panel.kind).max(1);
            if let Some(geometry) = DevicePanelView::geometry(self.frame, panel, entries) {
                covered.push(geometry.area);
            }
        }
        if self.regions.sidebar.is_empty()
            && let Some((area, _)) = self
                .set_panel
                .as_ref()
                .and_then(|panel| panel.sheet_geometry(self.frame))
        {
            covered.push(area);
        }
        if let Some((area, _)) = self
            .set_prompt
            .as_ref()
            .and_then(|(_, picker)| picker.geometry(self.frame))
        {
            covered.push(area);
        }
        for panel in self.viz_docks.iter().flatten() {
            if let Some((area, _)) = panel
                .prompt
                .as_ref()
                .and_then(|picker| picker.geometry(self.frame))
            {
                covered.push(area);
            }
            if panel.adding.is_some()
                && let Some((area, _)) = VizPanel::add_sheet_geometry(self.frame)
            {
                covered.push(area);
            }
        }
        covered
    }

    /// The global painters' pictures - every `pianoroll()`, `scope()`,
    /// `spiral()` called without the underscore - each rendered over the
    /// whole editor area of the pane showing the audible scene. They go
    /// under the score as the stage (`paint_stage`): over Hydra, under the
    /// text, at the editor's opacity. Their inline twins take rows instead.
    pub(super) fn stage_pictures(&self) -> Vec<(Rect, Buffer)> {
        if !super::super::settings::animation() {
            return Vec::new();
        }
        let Some(layout) = self.visual.layout() else {
            return Vec::new();
        };
        let mut pictures = Vec::new();
        for (index, pane) in self.panes.iter().enumerate() {
            if Some(pane.scene) != self.audible_scene {
                continue;
            }
            let Some(region) = self.regions.panes.get(index) else {
                continue;
            };
            let area = region.editor;
            if area.width == 0 || area.height == 0 {
                continue;
            }
            for visual in layout
                .visuals
                .iter()
                .filter(|visual| !visual.inline && visual.kind != "markcss")
            {
                let options = super::super::theme::VisualOptions::parse(&visual.options);
                let mut picture = Buffer::empty(area);
                super::super::visuals::render(
                    super::super::visuals::VisualRequest {
                        kind: &visual.kind,
                        slot: visual.slot,
                        options: &options,
                        state: &self.visual,
                        theme: &self.theme,
                        background: self.theme.background,
                        inline: false,
                    },
                    area,
                    &mut picture,
                );
                pictures.push((area, picture));
            }
        }
        pictures
    }
}
