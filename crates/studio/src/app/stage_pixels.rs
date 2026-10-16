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

#[cfg(test)]
mod tests {
    mod stage_pixels {
        //! Tests for stage pixels.

        use crate::app::stage_pixels::*;
        use crate::editor::{GridRect, VirtualRowSpec};
        use crate::graphics::Tier;
        use crate::graphics::{self, PixelImage};
        use ratatui::style::Color;
        use ratatui::style::Style;
        use rustel_runtime::ui_analysis::{
            UI_SCOPE_SAMPLES, UI_SPECTRUM_BINS, UiAudioAnalysisFrame, UiAudioAnalysisSet,
        };
        use rustel_runtime::ui_events::UiAudioMetadata;
        use std::collections::HashSet;

        #[cfg(feature = "hydra")]
        #[test]
        fn late_shelf_pixels_follow_reference_stacking_and_leave_other_images_intact() {
            let directory = tempfile::tempdir().unwrap();
            let mut app = crate::app::tests::support::app_over(directory.path());
            app.frame = Rect::new(0, 0, 140, 40);
            app.regions = view::regions(app.frame, false, 1, true, true, None, None);
            app.reference_panel = Some(crate::reference::ReferencePanel::browse(&app.reference));
            app.settings_sheet = Some(crate::settings::SettingsSheet::default());
            let (sheet, _) = crate::settings::SettingsSheetView::geometry(app.frame).unwrap();
            let area =
                crate::reference::preview_area(crate::reference::inner_area(app.regions.reference));
            assert!(
                !area.intersection(sheet).is_empty(),
                "the preview overlaps Settings"
            );
            let image = PixelImage {
                area,
                width: u32::from(area.width) * 2,
                height: u32::from(area.height) * 3,
                rgba: [80, 90, 100, 255]
                    .repeat(usize::from(area.width) * usize::from(area.height) * 6),
                depth: 0,
                cells: Some((area.width, area.height)),
            };
            let (_, remaining) = graphics::capture_images(|| {
                let prior = PixelImage {
                    depth: -1,
                    ..image.clone()
                };
                graphics::push_image(prior.clone());
                app.raise(PanelKind::Reference);
                app.raise(PanelKind::Settings);
                let covered = app.shelf_exclusions(false);
                assert!(covered.contains(&sheet));
                let masked = mask_shelf(image.clone(), &covered);
                if let Some(masked) = masked {
                    assert_eq!(masked.cells, image.cells);
                    assert_eq!(masked.depth, 0);
                    assert_eq!(masked.area, area);
                    for (index, pixel) in masked.rgba.as_chunks::<4>().0.iter().enumerate() {
                        let x = area.x + ((index % image.width as usize) / 2) as u16;
                        let y = area.y + ((index / image.width as usize) / 3) as u16;
                        let hidden = covered.iter().any(|rect| rect.contains((x, y).into()));
                        assert_eq!(pixel[3], if hidden { 0 } else { 255 });
                    }
                } else {
                    assert_eq!(
                        area.intersection(sheet),
                        area,
                        "only a wholly covered preview disappears"
                    );
                }
                app.raise(PanelKind::Reference);
                let covered = app.shelf_exclusions(false);
                assert!(
                    !covered.contains(&sheet),
                    "a lower Settings sheet cannot cut out the raised shelf"
                );
                assert_eq!(mask_shelf(image.clone(), &covered), Some(image.clone()));
                app.set_prompt = Some((SetPrompt::OpenSet, FilePicker::text("open", "accept")));
                let prompt = app
                    .set_prompt
                    .as_ref()
                    .unwrap()
                    .1
                    .geometry(app.frame)
                    .unwrap()
                    .0;
                assert!(
                    app.shelf_exclusions(false).contains(&prompt),
                    "file prompts stay above a raised reference"
                );
                app.menu = Some(crate::menu::MenuState::dropped_on(&app.menus(), 0));
                let menus = crate::menu::dropdown_rects(
                    &app.menus(),
                    app.menu.as_ref().unwrap(),
                    app.menu_row(),
                    app.frame,
                );
                assert!(!menus.is_empty());
                assert!(
                    menus
                        .iter()
                        .all(|rect| app.shelf_exclusions(false).contains(rect))
                );
                assert_eq!(
                    graphics::take_images(),
                    vec![prior],
                    "masking the late shelf never damages existing layers"
                );
            });
            assert!(remaining.is_empty());
        }

        #[test]
        fn stage_pixels_follow_opacity_and_exclusions_without_masking_other_layers() {
            let area = Rect::new(3, 4, 8, 1);
            let ground = Color::Rgb(10, 20, 30);
            let mut frame = Buffer::empty(area);
            frame.set_style(area, Style::default().bg(ground));
            frame.set_string(4, 4, "x", Style::default().fg(Color::White));
            frame[(5, 4)].set_bg(Color::Blue);
            let covered = [Rect::new(7, 4, 1, 1), Rect::new(8, 4, 1, 1)];
            let untouched = HashSet::from([(6, 4), (9, 4)]);
            let stage = PixelImage::inline(area, 16, 3, [90, 80, 70, 255].repeat(16 * 3));
            let other = PixelImage::inline(area, 16, 3, [1, 2, 3, 255].repeat(16 * 3));
            let (_, queued) = graphics::capture_images(|| {
                graphics::push_image(other.clone());
                let prepared = prepare(
                    vec![stage.clone()],
                    &frame,
                    ground,
                    &covered,
                    &untouched,
                    0.25,
                );
                assert_eq!(prepared.len(), 1);
                let image = &prepared[0];
                assert_eq!(image.depth, -1, "stage stays behind glyphs");
                assert_eq!(image.area, area);
                assert_eq!((image.width, image.height), (16, 3));
                for (index, pixel) in image.rgba.as_chunks::<4>().0.iter().enumerate() {
                    let column = (index % 16) / 2;
                    assert_eq!(&pixel[..3], &[90, 80, 70]);
                    assert_eq!(
                        pixel[3],
                        if [0, 1, 7].contains(&column) { 64 } else { 0 },
                        "own-background chips, highlights, selections and explicit rectangles stay clear"
                    );
                }
                assert_eq!(
                    graphics::take_images(),
                    vec![other.clone()],
                    "masking is local to the stage"
                );
                let backdrop = PixelImage {
                    depth: -1,
                    ..other.clone()
                };
                graphics::push_image(backdrop);
                for image in prepared {
                    graphics::push_image(image);
                }
            });
            assert_eq!(queued.len(), 2);
            assert_eq!(queued[0].rgba, other.rgba, "backdrop is queued first");
            assert_eq!(
                queued[1].rgba[3], 64,
                "stage is placed above it at equal negative depth"
            );
            assert!(prepare(vec![stage], &frame, ground, &[], &HashSet::new(), 0.0).is_empty());
        }

        struct Restore {
            tier: Tier,
            cells: Option<(u16, u16)>,
            settings: UiSettings,
            images: Vec<PixelImage>,
        }

        impl Drop for Restore {
            fn drop(&mut self) {
                let _ = graphics::take_images();
                graphics::set_tier(self.tier);
                graphics::set_cell_pixels(self.cells);
                self.settings.apply();
                for image in self.images.drain(..) {
                    graphics::push_image(image);
                }
            }
        }

        #[test]
        fn global_scope_survives_frame_reset_and_excludes_inline_rows_and_sheets() {
            let directory = tempfile::tempdir().unwrap();
            let mut app = crate::app::tests::support::app_over(directory.path());
            let _restore = Restore {
                tier: graphics::tier(),
                cells: graphics::cell_pixels(),
                settings: app.ui_settings.clone(),
                images: graphics::take_images(),
            };
            app.ui_settings.animation = true;
            app.ui_settings.apply();
            graphics::set_tier(Tier::Pixels);
            graphics::set_cell_pixels(Some((2, 3)));
            app.frame = Rect::new(0, 0, 120, 40);
            app.regions = view::regions(app.frame, false, 1, false, false, None, None);
            app.audible_scene = Some(app.scenes.current().id);
            let layout = visual_layout("s(\"bd\").scope()._scope()", 1).unwrap();
            let audio = UiAudioAnalysisFrame {
                scope: [0.5; UI_SCOPE_SAMPLES],
                spectrum: [-120.0; UI_SPECTRUM_BINS],
            };
            let taps = layout
                .ui_layout
                .visuals
                .iter()
                .filter_map(|visual| visual.slot)
                .map(|slot| (slot, audio.clone()))
                .collect();
            app.visual.install_layout(layout);
            app.visual.start();
            assert!(app.visual.install_audio(
                UiAudioMetadata {
                    sequence: 1,
                    generation: 1,
                    device_time: 0.5,
                    stream_id: 1,
                    epoch: 1,
                    end_frame: 24_000,
                    sample_rate: 48_000
                },
                UiAudioAnalysisSet {
                    master: audio,
                    visuals: taps,
                    sides: Vec::new(),
                }
            ));
            let (buffers, images) = graphics::capture_images(|| app.stage_pictures());
            assert_eq!(
                buffers.len(),
                1,
                "only the global scope belongs to the stage"
            );
            assert_eq!(
                images.len(),
                1,
                "the actual scope renderer produced a pixel image"
            );
            let area = app.regions.panes[0].editor;
            assert_eq!(images[0].area, area);
            assert!(
                images[0]
                    .rgba
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .any(|pixel| pixel[3] > 0)
            );
            assert!(
                graphics::take_images().is_empty(),
                "the frame reset cannot discard captured stage images"
            );
            let mut frame = Buffer::empty(app.frame);
            frame.set_style(app.frame, Style::default().bg(app.theme.background));
            let prepared = prepare(
                images,
                &frame,
                app.theme.background,
                &[],
                &HashSet::new(),
                0.5,
            );
            assert_eq!(prepared.len(), 1);
            assert!(
                prepared[0]
                    .rgba
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .any(|pixel| pixel[3] == 128)
            );

            let revision = app.editor().revision();
            app.editor_mut()
                .set_virtual_rows(
                    revision,
                    vec![VirtualRowSpec::new(
                        Arc::<str>::from("inline"),
                        ByteOffset(0),
                        2,
                    )],
                )
                .unwrap();
            app.editor_mut()
                .set_view_size(usize::from(area.width), usize::from(area.height));
            let map = app
                .editor_mut()
                .screen_map(GridRect::new(area.x, area.y, area.width, area.height))
                .unwrap();
            app.panel = Some(DevicePanel::default());
            app.set_prompt = Some((SetPrompt::OpenSet, FilePicker::text("test", "accept")));
            app.viz_docks[0] = Some(VizPanel {
                adding: Some(0),
                prompt: Some(FilePicker::text("art", "accept")),
                ..VizPanel::default()
            });
            let covered = app.stage_exclusions(std::slice::from_ref(&map));
            for row in map.rows() {
                if let crate::editor::ScreenRow::Virtual(row) = row {
                    assert!(covered.contains(&Rect::new(area.x, row.screen_y, area.width, 1)));
                }
            }
            let device = app.panel.unwrap();
            let count = app.panel_row_count(device.kind).max(1);
            assert!(
                covered.contains(
                    &DevicePanelView::geometry(app.frame, device, count)
                        .unwrap()
                        .area
                )
            );
            assert!(
                covered.contains(
                    &app.set_prompt
                        .as_ref()
                        .unwrap()
                        .1
                        .geometry(app.frame)
                        .unwrap()
                        .0
                )
            );
            assert!(
                covered.contains(
                    &app.viz_docks[0]
                        .as_ref()
                        .unwrap()
                        .prompt
                        .as_ref()
                        .unwrap()
                        .geometry(app.frame)
                        .unwrap()
                        .0
                )
            );
            assert!(covered.contains(&VizPanel::add_sheet_geometry(app.frame).unwrap().0));
        }

        /// A stage painter with nothing to draw yet leaves its picture blank: no
        /// word about waiting is written behind the score.
        #[test]
        fn a_stage_painter_with_nothing_to_draw_writes_nothing() {
            let directory = tempfile::tempdir().unwrap();
            let mut app = crate::app::tests::support::app_over(directory.path());
            let _restore = Restore {
                tier: graphics::tier(),
                cells: graphics::cell_pixels(),
                settings: app.ui_settings.clone(),
                images: graphics::take_images(),
            };
            app.ui_settings.animation = true;
            app.ui_settings.apply();
            app.frame = Rect::new(0, 0, 120, 40);
            app.regions = view::regions(app.frame, false, 1, false, false, None, None);
            app.audible_scene = Some(app.scenes.current().id);
            app.visual
                .install_layout(visual_layout("s(\"bd\").scope().pianoroll()", 1).unwrap());
            let pictures = app.stage_pictures();
            assert_eq!(pictures.len(), 2, "both painters are on the stage");
            for (_, picture) in pictures {
                let written: String = picture
                    .content
                    .iter()
                    .map(|cell| cell.symbol())
                    .filter(|symbol| *symbol != " ")
                    .collect();
                assert!(written.is_empty(), "the stage stays blank: {written:?}");
            }
        }
    }
}
