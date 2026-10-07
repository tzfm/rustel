//! Building and drawing one studio frame: `draw` sends a synchronized frame to
//! the terminal and `paint_into` lays out the panes and composes the full
//! picture (docked chrome, scene chips, decorations, sounding-event marks,
//! selections, stage visuals, tempo and bracket highlights). It also keeps the
//! adaptive pixel budget, which makes pictures smaller or larger to match how
//! fast the terminal draws frames.

use super::focus::FOCUS_ROTATION_FLASH_DURATION;
use super::*;

/// How a TachyonFX theme draws this frame - over the finished cells or as a
/// picture behind them - and how strongly it may show through the score and
/// through the interface.
struct NativeVisual {
    mode: Option<TachyonMode>,
    editor_strength: f32,
    interface_strength: f32,
}

/// What the chrome, the panes and the sheets are drawn from, read off the
/// studio in one pass before any of them is put together.
struct FrameInputs {
    status: String,
    piano_notes: Option<String>,
    error: Option<String>,
    stopping: bool,
    clock: Option<(f64, f64, f64)>,
    device: Option<String>,
    input_device: Option<String>,
    stats: ProcessStats,
    chips: Vec<SceneChip>,
    lint_message: Option<String>,
    overridden_ranges: Vec<Vec<(usize, usize)>>,
    bracket_ranges: Vec<Vec<(usize, usize, bool)>>,
    lint_ranges: Vec<Vec<(usize, usize)>>,
    flashing: bool,
    locating_caret: Option<f32>,
    playing_scene: Option<SceneId>,
    export_glance: Option<super::super::export::ExportGlance>,
    launch_countdown: Option<(String, f64)>,
    menus: Vec<super::super::menu::Menu>,
    menu_row: Rect,
    menu_state: Option<super::super::menu::MenuState>,
    set_name: String,
    background_jobs: Vec<BackgroundJob>,
    jobs_note: Option<String>,
}

/// What the visuals read for this frame: what the pictures may do, the room
/// each dock asks for, the mix's level, the seconds of sound the widgets
/// animate on, and Hydra's camera. The chrome reads them; the view takes them
/// over.
struct VisualReadings {
    dock_extents: [u16; DOCKS],
    mix_level: f32,
    motion: super::super::viz_panel::Motion,
    sound_seconds: f32,
    #[cfg(feature = "hydra")]
    hydra_webcam: Option<rustel_runtime::hydra::HydraWebcamStatus>,
}

/// The smart action's popup as the frame draws it: the cell it hangs from,
/// its rows, and the caret in its form.
type SmartPopup = ((u16, u16), Vec<String>, Option<(u16, u16)>);

/// What sits over the view - the precision slider, the smart action, the
/// replay edit and help - and the theme editor, lent to the paint half for
/// the frame and handed back once it is painted.
struct Overlays {
    precision: Option<slider_precision::PrecisionView>,
    smart: Option<SmartPopup>,
    replay_edit: Option<replay_edit::ReplayEdit>,
    help: Option<HelpState>,
    help_context: HelpContext,
    theme_editor_visible: bool,
    theme_editor: Option<super::super::theme_editor::ThemeEditor>,
    theme_editor_focused: bool,
}

/// The layers the paint half lays over the rendered view, and what each of
/// them must leave alone.
struct FrameParts {
    #[cfg(feature = "hydra")]
    snippet_box: Option<Rect>,
    theme_background: ratatui::style::Color,
    theme_native_visible: bool,
    chrome_rects: Vec<Rect>,
    selection_cells: HashSet<(u16, u16)>,
    highlight_cells: HashSet<(u16, u16)>,
    selection_strength: f32,
    stage_background: ratatui::style::Color,
    stage: Vec<(Rect, Buffer)>,
    stage_images: Vec<super::super::graphics::PixelImage>,
    stage_strength: f32,
    native_keep: Vec<Rect>,
    slider_rails: Vec<Rect>,
    stage_keep: Vec<Rect>,
    stage_untouched: HashSet<(u16, u16)>,
    #[cfg(feature = "hydra")]
    shelf_keep: Vec<Rect>,
}

impl FrameParts {
    /// The selected cells, the highlights the picture never touches, and
    /// how much of the picture the selection takes.
    fn selection_wash(&self) -> super::super::view::SelectionWash<'_> {
        super::super::view::SelectionWash {
            cells: &self.selection_cells,
            untouched: &self.highlight_cells,
            strength: self.selection_strength,
        }
    }

    /// Keep the rail's glyphs while later painters change its background.
    fn slider_glyphs(&self, frame: &Buffer) -> Vec<((u16, u16), ratatui::buffer::Cell)> {
        self.slider_rails
            .iter()
            .flat_map(|rail| (rail.x..rail.right()).map(move |x| (x, rail.y)))
            .filter_map(|at| frame.cell(at).map(|cell| (at, cell.clone())))
            .collect()
    }

    /// Stage images are prepared against the view as `render` left it,
    /// before frame effects change backgrounds.
    fn prepare_stage_images(&mut self, frame: &Buffer) {
        self.stage_images = stage_pixels::prepare(
            std::mem::take(&mut self.stage_images),
            frame,
            self.stage_background,
            &self.stage_keep,
            &self.stage_untouched,
            self.stage_strength,
        );
    }

    /// The stage's pictures, each laid under the score of its pane.
    fn paint_stage(&self, frame: &mut ratatui::Frame<'_>) {
        for (pane_area, picture) in &self.stage {
            super::super::view::paint_stage(
                frame.buffer_mut(),
                picture,
                *pane_area,
                self.stage_background,
                &self.stage_keep,
                &self.stage_untouched,
                self.stage_strength,
            );
        }
    }
}

impl App {
    /// How long a frame may take before the pictures in it are made
    /// smaller, and how quick it has to be for them to grow again.
    const SLOW_FRAME: Duration = Duration::from_millis(20);

    /// Keep the pictures at a size this terminal can actually carry.
    ///
    /// A pixel frame is composited, compressed, and written down the same
    /// pipe the text goes down - all on the thread that draws - so the size
    /// of the pictures sets the frame rate. Terminals differ at this by more
    /// than an order of magnitude, and so do window sizes and what the
    /// score has on screen, so rather than guess, this measures what a
    /// whole frame actually cost and moves the size: a third off when
    /// frames run long, an eighth back on when they run comfortably short,
    /// between a floor that still looks like a picture and the ceiling
    /// above. One number, shared: the Hydra backdrop is asked for at it,
    /// and every canvas in the frame takes a share of it. It settles within
    /// a second or two of opening, and a resize starts it over.
    pub(super) fn watch_pixel_frame(&mut self, frame: Duration) {
        let budget = if frame > Self::SLOW_FRAME {
            self.pixel_budget * 2 / 3
        } else if frame * 2 < Self::SLOW_FRAME {
            self.pixel_budget + self.pixel_budget / 8
        } else {
            return;
        }
        .clamp(
            super::super::graphics::PIXEL_FLOOR,
            super::super::graphics::PIXEL_CEILING,
        );
        if budget == self.pixel_budget {
            return;
        }
        let lowering = budget < self.pixel_budget;
        self.set_pixel_budget(budget);
        if lowering && !self.pixel_budget_said {
            self.pixel_budget_said = true;
            self.log.push(
                LogLevel::Info,
                "rendering",
                format!(
                    "pixel frames took over {} ms - sending the pictures smaller and letting the terminal scale them",
                    Self::SLOW_FRAME.as_millis()
                ),
            );
        }
    }

    /// The measured size, to the renderer and to Hydra.
    pub(super) fn set_pixel_budget(&mut self, pixels: u64) {
        self.pixel_budget = pixels;
        super::super::graphics::set_pixel_budget(pixels);
        #[cfg(feature = "hydra")]
        self.sync_hydra_frame_size();
    }

    /// The furniture among the regions, for the backdrop to go behind
    /// rather than through.
    ///
    /// Everything the interface docked for itself takes the picture at the
    /// interface opacity, never the score's: the minimap bands (the
    /// settings sheet says `ui opacity` governs them, and on themes whose
    /// minimap ground equals the editor's, telling them apart by colour is
    /// impossible), the reference column, the set panel's column, the
    /// visualizer docks, and the mixer desk. A panel left out of this is
    /// washed at the score's own strength - a tint on the glyph tiers,
    /// where the picture is only a cell colour, and an unreadable panel on
    /// the pixel tier, where the picture really is painted over it.
    pub(super) fn docked_chrome(&self) -> Vec<Rect> {
        let mut rects: Vec<Rect> = self
            .regions
            .panes
            .iter()
            .take(self.regions.pane_count)
            .map(|pane| pane.minimap)
            .collect();
        if self.reference_panel.is_some() {
            rects.push(self.regions.reference);
        }
        rects.push(self.regions.sidebar);
        rects.extend(self.regions.viz);
        rects.push(self.regions.mixer);
        rects.push(self.regions.memory);
        rects.retain(|rect| !rect.is_empty());
        rects
    }

    pub(super) fn draw(&mut self, terminal: &mut TerminalSession) -> Result<(), RuntimeError> {
        let drawing = Instant::now();
        let started = Instant::now();
        // The frame goes out as one synchronized update - text, then the
        // pictures painted for it - so it is never seen half-drawn.
        let _ = super::super::graphics::take_images();
        terminal.set_cursor_color(self.theme.caret())?;
        terminal.set_cursor_shape(self.theme.caret_shape(self.ui_settings.caret_shape))?;
        terminal.begin_frame();
        // The paint callback always runs - `draw` renders the buffer, then
        // flushes - so its verdict comes back through the cell, and a compose
        // error still aborts the studio.
        let painted = std::cell::RefCell::new(Ok(()));
        {
            // `draw` swaps buffers on the way out. The frame on screen is the
            // completed one; the current buffer is already blank for the next frame.
            let completed = terminal.terminal_mut().draw(|frame| {
                if let Err(error) = self.paint_into(frame, started) {
                    *painted.borrow_mut() = Err(error);
                }
            })?;
            painted.into_inner()?;
            #[cfg(feature = "remote-control")]
            if self.remote.is_some() {
                self.reply_remote_screen(completed.buffer);
            }
            // A sketch that asked for `feedStrudel` reads this same frame.
            #[cfg(feature = "hydra")]
            if let Some(sink) = self.hydra_frames.as_ref()
                && sink.wanted()
            {
                let _ = sink.publish(&super::super::graphics::tui_frame(completed.buffer));
            }
            #[cfg(not(any(feature = "remote-control", feature = "hydra")))]
            let _ = completed;
        }
        // A synchronized update or a theme reset can leave the terminal
        // having re-asserted its own pointer shape; give the position under
        // the pointer the last word.
        if let Some((x, y)) = self.pointer_at {
            self.follow_pointer_shape(x, y);
        }
        // Pictures pushed for kitty's graphics protocol go out with the
        // frame's end.
        let images = super::super::graphics::take_images();
        let shipped_pixels = !images.is_empty()
            && super::super::graphics::tier() == super::super::graphics::Tier::Pixels;
        terminal.end_frame(&images);
        // The whole frame, not just the shipping: compositing the backdrop
        // and rasterising every canvas are the other two thirds of what the
        // budget governs, and they happen above.
        if shipped_pixels {
            self.watch_pixel_frame(drawing.elapsed());
        }
        Ok(())
    }

    /// Whether this frame's host readings are pinned to constants.
    ///
    /// Always `false` without the `harness` feature, where the field
    /// behind it does not exist - so the product cannot pin, and the branches
    /// at the call sites are not reachable scaffolding but the only behaviour
    /// the shipped binary has.
    #[cfg(feature = "harness")]
    #[inline]
    pub(super) fn pinned_frame_inputs(&self) -> bool {
        self.pin_frame_inputs
    }

    /// Always false: without the harness there is nothing to pin to.
    #[cfg(not(feature = "harness"))]
    #[inline(always)]
    pub(super) fn pinned_frame_inputs(&self) -> bool {
        false
    }

    /// Compose and paint one studio frame into any ratatui frame - the
    /// terminal's own in [`App::draw`], a test backend's in the end-to-end
    /// suite. The compute half builds the view, the chrome, and the paint
    /// plan; the paint half paints it, so what is drawn is decided entirely
    /// by what this computed and not by where it lands.
    pub(crate) fn paint_into(
        &mut self,
        out: &mut ratatui::Frame<'_>,
        started: Instant,
    ) -> Result<(), RuntimeError> {
        // Per-frame state the paint reads. It is computed here and not in
        // `draw`, because the test backend enters at this function and never
        // runs `draw`. The contract in this function's doc needs both the
        // stale precision slider dropped and Hydra's newest picture taken
        // here.
        if self.slider_precision.is_some() && self.precision_slider_view().is_none() {
            self.slider_precision = None;
        }
        // Hydra's newest picture, if a sketch is drawing for the terminal.
        // Kept across frames: the terminal redraws on its own schedule, and a
        // frame nothing has replaced is still the picture.
        #[cfg(feature = "hydra")]
        self.update_hydra_latest();
        let terminal_area: Rect = out.area();
        let native = self.sync_theme_visual(started);
        let regions = self.lay_out_regions(terminal_area);
        // The terminal query is authoritative even if its resize event was
        // coalesced or missed. Revoke a Settings-only camera before drawing
        // when the actual area can no longer show its consent surface.
        #[cfg(feature = "hydra")]
        self.sync_settings_webcam_preview();

        // Lay every pane out before decorating: the audible scene's marks
        // are mapped once, in its own coordinates, and drawn wherever it is.
        let maps = self.lay_out_panes(regions)?;
        self.refresh_decorations();
        self.prepare_log_sheet();

        let (inputs, visuals) = self.frame_inputs(started);
        // The overlays are read, and the theme editor lent to the paint half,
        // before the chrome, the panes and the view borrow the studio until
        // the frame is rendered; none of them reads the theme editor.
        let mut overlays = self.frame_overlays();
        let chrome = self.studio_chrome(&inputs, &visuals, started);
        let chip_sets = self.pane_slider_chips();
        let panes = self.pane_views(&maps, &chip_sets, &inputs);
        let view = self.studio_view(panes, &inputs, visuals, &overlays);
        let snippet_box = self.snippet_box();
        // Real pixels when the terminal can show them; cell colours when it
        // cannot. Never both, or the picture would be drawn twice.
        #[cfg(feature = "hydra")]
        let cells = super::super::graphics::tier() != super::super::graphics::Tier::Pixels;
        #[cfg(feature = "hydra")]
        let backdrop = self
            .hydra_last
            .as_ref()
            .or(self.hydra_theme_last.as_ref())
            .filter(|_| cells);
        #[cfg(feature = "hydra")]
        let shelf = self
            .hydra_shelf_last
            .as_ref()
            .filter(|_| cells && overlays.help.is_none());
        let mut parts = self.frame_parts(&maps, &chip_sets, &inputs.menus, &overlays, snippet_box);
        let locator = view.panes.iter().find_map(|pane| {
            let progress = pane.locate_flash?;
            let center = pane
                .map
                .cell_for_offset(pane.editor.primary_selection().head)?;
            Some((center, progress))
        });

        // The paint half runs on the frame the caller handed over - the
        // terminal's own in `draw`, a test backend's in the suite - and reads
        // the composed buffer inside the same scope, before the terminal can
        // swap it.
        let frame = out;
        view::render(frame, view, chrome);
        let slider_glyphs = parts.slider_glyphs(frame.buffer_mut());
        // `render` draws from the view and the chrome alone, never from the
        // theme's visual, which is lent to the paint half once they are done.
        let mut theme_visual = std::mem::take(&mut self.theme_visual);
        parts.prepare_stage_images(frame.buffer_mut());
        let native_picture = Self::paint_theme_native(
            &mut theme_visual,
            frame,
            terminal_area,
            &native,
            &parts,
            started,
        );
        parts.paint_stage(frame);
        self.paint_theme_editor_sheet(
            frame,
            overlays.theme_editor_visible,
            &mut overlays.theme_editor,
            overlays.theme_editor_focused,
        );
        // The dropdown is drawn here, not inside `view::render`. The stage
        // pictures (`FrameParts::paint_stage`) and the theme editor's sheet
        // (`paint_theme_editor_sheet`) are both painted after `render`
        // returns, so a dropdown drawn inside `render` - even after the
        // toast that calls itself last - would be painted over by them.
        self.paint_menu_dropdown(
            frame,
            inputs.menu_state.as_ref(),
            inputs.menu_row,
            &inputs.menus,
        );
        // Hydra last, over the finished frame and under nothing: it
        // recolours only the cells still showing the theme's plain
        // background, so every panel, selection and glyph keeps the
        // colour it chose and what changes is the space behind them.
        #[cfg(feature = "hydra")]
        Self::paint_hydra_cells(frame, terminal_area, &parts, backdrop, shelf);
        self.paint_theme_characters(&mut theme_visual, frame, terminal_area, &parts, started);
        for (at, glyph) in slider_glyphs {
            if let Some(cell) = frame.buffer_mut().cell_mut(at) {
                let background = cell.bg;
                *cell = glyph;
                cell.bg = background;
            }
        }
        self.paint_overlays(
            frame,
            &overlays,
            &inputs.status,
            inputs.piano_notes.as_deref(),
        );
        if let Some((center, progress)) = locator {
            view::render_caret_locator(
                frame.buffer_mut(),
                terminal_area,
                center,
                progress,
                &self.theme,
            );
        }
        #[cfg(feature = "remote-control")]
        self.paint_remote_panel(frame);
        // Every widget shares control-character filtering and the terminal's
        // decorative-glyph fallback before the completed frame is emitted.
        super::super::terminal::sanitize_buffer(frame.buffer_mut());
        self.theme_visual = theme_visual;
        self.theme_editor = overlays.theme_editor.take();
        self.push_pixel_pictures(frame, terminal_area, parts, native_picture, &overlays);
        self.finish_frame(maps, started, inputs.stats);
        Ok(())
    }

    /// Bring the theme's own visual up to this frame, and say how it draws
    /// and how strongly it may show through the score and the interface.
    fn sync_theme_visual(&mut self, started: Instant) -> NativeVisual {
        self.theme_visual.sync(&self.theme, started);
        // The frozen picture keeps its last audio frame for the painters;
        // the theme's ear hears silence from a stop, or a bear kept hearing
        // the last beat for ever.
        self.theme_visual.update_audio(if self.visual.running() {
            self.visual.audio()
        } else {
            None
        });
        NativeVisual {
            mode: self.theme_visual.mode(),
            editor_strength: backdrop_strength(
                self.ui_settings.backdrop_opacity,
                self.ui_settings.editor_opacity,
            ),
            interface_strength: backdrop_strength(
                self.ui_settings.backdrop_opacity,
                self.ui_settings.interface_opacity,
            ),
        }
    }

    /// Lay the studio out over the terminal's area, and bring the visuals
    /// docks and the set panel to the room they got. The regions come back
    /// for the panes to be laid out in.
    fn lay_out_regions(&mut self, terminal_area: Rect) -> StudioRegions {
        let regions = view::regions_with_footer(
            terminal_area,
            view::ChromeLayout::from_settings(&self.ui_settings),
            self.panes.len(),
            self.reference_panel.is_some(),
            self.ui_settings.minimap,
            self.set_sidebar(),
            self.dock_requests(),
            self.mixer_panel.map(MixerPanel::dock),
            self.log_panel
                .as_ref()
                .and_then(|panel| view::log_dock(panel, terminal_area)),
            self.memory_dock.map(|dock| dock.dock(terminal_area)),
            self.footer_notice_rows(),
            self.has_piano_notes(),
        );
        self.frame = terminal_area;
        self.regions = regions;
        let hidden_focus = match self.focus {
            Focus::Panel(PanelKind::Set) => regions.sidebar_hidden && self.set_prompt.is_none(),
            Focus::Panel(PanelKind::Viz) => regions.viz[self.viz_focus].is_empty(),
            Focus::Panel(PanelKind::Log) => self.log_is_docked() && regions.log.is_empty(),
            _ => false,
        };
        if hidden_focus {
            self.focus = Focus::Editor;
        }
        let set_name = self.scenes.name();
        for (index, panel) in self.viz_docks.iter_mut().enumerate() {
            let Some(panel) = panel else { continue };
            let widgets = &self.prefs.visuals[index].widgets;
            panel.clamp(widgets.len());
            if let Some(parts) = VizPanel::parts(regions.viz[index], panel.edge) {
                panel.ensure_visible(widgets, &set_name, parts.body);
            }
            // The dock has been laid out: the region is now the truth about
            // the room it got, and its key claim rests on it alone.
            self.viz_unlaid[index] = false;
        }
        // The desk has been laid out too: its region is the truth about
        // the room it got, and its key claim rests on it.
        self.mixer_unlaid = false;
        self.memory_unlaid = false;
        if let Some(panel) = self.set_panel.as_mut() {
            panel.sync_marks(&self.scenes);
            if !regions.sidebar_hidden
                && let Some(list) = panel.list_area(regions.sidebar, terminal_area)
            {
                panel.ensure_visible(list.height);
            }
        }
        regions
    }

    /// Size each pane's editor to its region, give an unwrapped score's
    /// horizontal bar its row, and map each pane's text to the screen. The
    /// maps come back in pane order.
    fn lay_out_panes(
        &mut self,
        mut regions: StudioRegions,
    ) -> Result<Vec<ScreenMap>, RuntimeError> {
        let mut maps = Vec::with_capacity(self.panes.len());
        let wrap = self.ui_settings.wrap;
        let numbered = self.ui_settings.line_numbers;
        self.split_replay_timelines(&mut regions);
        // An unwrapped score that is wider than its pane gets a real row for
        // its horizontal bar. Measure first, then apply only the final text
        // dimensions so a redraw never scrolls back to the caret.
        for index in 0..self.panes.len() {
            let scene_id = self.panes[index].scene;
            let widths = self.slider_pill_widths(scene_id);
            let Some(scene) = self.scenes.get_mut(scene_id) else {
                continue;
            };
            scene.editor.set_inline_widths(widths);
            scene.editor.set_wrap(wrap);
            reserve_horizontal_scrollbar(
                &mut scene.editor,
                &mut regions.panes[index],
                numbered,
                self.ui_settings.show_scrollbars,
            );
        }
        self.regions = regions;
        for index in 0..self.panes.len() {
            let region = regions.panes[index];
            let scene_id = self.panes[index].scene;
            // The pills take their room on their rows before the map is
            // built, so the map has it: the cell before each value grows
            // by the pill's extra cells and the rest of the line moves
            // along, the way the web editor's widget takes its own space.
            let Some(scene) = self.scenes.get_mut(scene_id) else {
                continue;
            };
            let grid = view::source_grid(&scene.editor, region.editor, numbered);
            scene
                .editor
                .set_view_size(usize::from(grid.width), usize::from(grid.height));
            let map = scene
                .editor
                .screen_map(grid)
                .map_err(|error| RuntimeError::Message(error.to_string()))?;
            scene
                .minimap
                .sync_editor(&scene.editor, region.minimap)
                .map_err(|error| RuntimeError::Message(error.to_string()))?;
            maps.push(map);
        }
        Ok(maps)
    }

    /// Read what the chrome, the panes and the sheets are drawn from,
    /// sampling the host on the way.
    ///
    /// Not a pure read: it also changes the studio. A panel closed since
    /// the last key gives the caret back (`settle_focus`), and, with Hydra,
    /// the camera picture the studio draws itself is taken from the input
    /// policy and handed to the theme's painter. It also advances the sound
    /// clock (`count_sound`), so a frame reads it once.
    fn frame_inputs(&mut self, started: Instant) -> (FrameInputs, VisualReadings) {
        let status = if self.piano.open {
            self.piano_status()
        } else {
            self.footer_status()
        };
        let piano_notes = self.retained_piano_notes();
        let error = self.errors.visible().map(str::to_owned);
        // A panel closed by any path since the last key must not keep the
        // caret parked in it.
        self.settle_focus();
        let snapshot = self.snapshot.as_ref();
        // A graceful stop freezes score marks and visual canvases, but its
        // tails are still audible. Keep the header's beat and cycle alive
        // until the engine says those tails have ended; STOPPING supplies
        // the distinction from ordinary scheduling playback.
        let stopping = self.stop_requested || self.is_stopping();
        let clock = if stopping {
            self.visual.current_transport_clock()
        } else {
            self.visual.current_clock()
        };
        // The engine reports what the backend calls the device it opened
        // - `coreaudio:00-00-5E-00-53-01:output` - and a musician chose it
        // by its name. The list knows both, so the name is looked up and
        // the handle only shows for a device that has gone from the list.
        let device = snapshot
            .and_then(|value| value.device.as_ref())
            .map(|device| self.device_name_for(device.name(), false))
            .or_else(|| self.resting_output_name());
        let input_device = snapshot
            .and_then(|value| value.input_device.as_deref())
            .map(|input| self.device_name_for(input, true));
        let stats = if self.pinned_frame_inputs() {
            // The harness pins the host readouts so its golden frames hold
            // on every machine; see `pinned_frame_inputs`.
            ProcessStats {
                cpu_percent: Some(0.0),
                machine_cpu_percent: Some(0.0),
                resident_bytes: Some(84 * 1024 * 1024),
                footprint_bytes: None,
            }
        } else {
            self.process.sample(started)
        };
        let chips = self.scene_chips();
        let lint_message = self.lint_message();
        // While an outside clock owns the tempo, the score's own tempo
        // calls are shown for what they are: overridden.
        let following = self
            .snapshot
            .as_ref()
            .is_some_and(|snapshot| snapshot.clock.in_port.is_some());
        let overridden_ranges = self
            .panes
            .iter()
            .map(|pane| {
                if !following {
                    return Vec::new();
                }
                self.scenes
                    .get(pane.scene)
                    .map(|scene| tempo_call_spans(&scene.editor.source()))
                    .unwrap_or_default()
            })
            .collect::<Vec<_>>();
        let bracket_ranges = self
            .panes
            .iter()
            .enumerate()
            .map(|(index, pane)| {
                if index != self.focused || !settings::brackets() {
                    return Vec::new();
                }
                self.scenes
                    .get(pane.scene)
                    .and_then(|scene| bracket_spans(&scene.editor))
                    .unwrap_or_default()
            })
            .collect::<Vec<_>>();
        let lint_ranges = self
            .panes
            .iter()
            .map(|pane| self.lint_ranges(pane.scene))
            .collect::<Vec<_>>();
        let flashing = self
            .evaluation_flash
            .as_ref()
            .is_some_and(|flash| flash.is_lit(Instant::now()));
        let locating_caret = self.focus_rotation_flash.and_then(|(stop, until)| {
            (stop == RotationStop::Editor)
                .then(|| until.saturating_duration_since(Instant::now()))
                .filter(|remaining| !remaining.is_zero())
                .map(|remaining| {
                    (remaining.as_secs_f32() / FOCUS_ROTATION_FLASH_DURATION.as_secs_f32())
                        .clamp(0.0, 1.0)
                })
        });
        let playing_scene = self.is_playing().then_some(self.audible_scene).flatten();
        let export_glance = self.export_job.as_ref().map(|job| job.glance());
        // The countdown runs to the line: the engine reports the launch
        // while it is armed AND while it has fired but not yet landed, and
        // the name is remembered past the outcome that ends the arming. A
        // stop ends it; there is nothing to count down to.
        let launch_name: Option<String> = self
            .armed_scene
            .and_then(|id| self.scenes.get(id))
            .map(|scene| scene.name().to_owned())
            .or_else(|| self.landing_name.clone());
        let launch_countdown = self
            .snapshot
            .as_ref()
            .filter(|snapshot| !snapshot.stopping)
            .and_then(|snapshot| snapshot.launch)
            .and_then(|launch| Some((launch_name.clone()?, launch.cycles_left)));
        #[cfg(feature = "hydra")]
        let hydra_webcam = self
            .hydra_input_policy
            .as_ref()
            .map(rustel_runtime::hydra::HydraInputPolicy::webcam_status);
        // The camera the studio draws itself, handed to the theme's painter
        // for this frame. Taken rather than read: the painter holds it until
        // it ages out, so a camera at fifteen frames a second does not
        // flicker under a sixty-frame draw loop.
        #[cfg(feature = "hydra")]
        if let Some(picture) = self
            .hydra_input_policy
            .as_ref()
            .and_then(rustel_runtime::hydra::HydraInputPolicy::take_camera_picture)
        {
            self.theme_visual.update_camera(
                super::super::theme_visuals::CameraPicture {
                    width: picture.width,
                    height: picture.height,
                    luma: picture.luma,
                },
                Instant::now(),
            );
        }
        let menus = self.menus();
        // `lay_out_regions` has already brought the studio's frame and
        // regions to this terminal, so the row the dropdown hangs from is
        // the same one the bar is drawn on.
        let menu_row = self.menu_row();
        let menu_state = self.menu.clone();
        let dock_extents: [u16; DOCKS] =
            std::array::from_fn(|index| self.dock_extent_for_view(index));
        let set_name = self.scenes.name();
        let mix_level = self.mix_level();
        let now = Instant::now();
        let motion = self.viz_motion();
        let sound_seconds = self.count_sound(now, motion);
        let background_jobs = self.background_jobs();
        let jobs_note = super::super::jobs::jobs_chip(&background_jobs);
        (
            FrameInputs {
                status,
                piano_notes,
                error,
                stopping,
                clock,
                device,
                input_device,
                stats,
                chips,
                lint_message,
                overridden_ranges,
                bracket_ranges,
                lint_ranges,
                flashing,
                locating_caret,
                playing_scene,
                export_glance,
                launch_countdown,
                menus,
                menu_row,
                menu_state,
                set_name,
                background_jobs,
                jobs_note,
            },
            VisualReadings {
                dock_extents,
                mix_level,
                motion,
                sound_seconds,
                #[cfg(feature = "hydra")]
                hydra_webcam,
            },
        )
    }

    /// Read what sits over the view this frame, then lend the theme editor
    /// to the paint half: whether its sheet shows, and the help context,
    /// are read from it before it is taken.
    fn frame_overlays(&mut self) -> Overlays {
        let precision = self.precision_slider_view();
        let smart = self.smart_action_view().map(|(state, rows)| {
            let anchor = state.anchor();
            let area = smart_action::geometry(self.frame, anchor, rows.len() as u16);
            (anchor, rows, state.form_caret(area))
        });
        let replay_edit = self.replay_edit.clone();
        let help = self.help;
        let help_context = self.help_context();
        let theme_editor_visible = self.theme_editor_sheet_visible();
        let theme_editor = self.theme_editor.take();
        let theme_editor_focused = self.focus == Focus::Panel(PanelKind::ThemeEditor);
        Overlays {
            precision,
            smart,
            replay_edit,
            help,
            help_context,
            theme_editor_visible,
            theme_editor,
            theme_editor_focused,
        }
    }

    /// The header, the footer and the bar as this frame draws them.
    fn studio_chrome<'a>(
        &'a self,
        inputs: &'a FrameInputs,
        visuals: &VisualReadings,
        started: Instant,
    ) -> StudioChrome<'a> {
        let snapshot = self.snapshot.as_ref();
        let scene = self.scenes.current();
        StudioChrome {
            caret_visible: self.pinned_frame_inputs() || self.caret_visible,
            keybinds: &self.keybinds,
            registry: &self.registry,
            build_features: self.options.build_features,
            prebake: scene.prebake(),
            replay: scene.is_replay(),
            path: &scene.path,
            set_name: &inputs.set_name,
            theme: &self.theme,
            playing: self.is_playing(),
            motion: visuals.motion,
            stopping: inputs.stopping,
            evaluating: self.is_evaluating(),
            dirty: scene.dirty,
            status: &inputs.status,
            remote_control: {
                #[cfg(feature = "remote-control")]
                {
                    self.remote.is_some()
                }
                #[cfg(not(feature = "remote-control"))]
                {
                    false
                }
            },
            remote_receiving: {
                #[cfg(feature = "remote-control")]
                {
                    self.remote
                        .as_ref()
                        .is_some_and(|remote| remote.activity_until.is_some())
                }
                #[cfg(not(feature = "remote-control"))]
                {
                    false
                }
            },
            piano: self.piano_pulse(),
            piano_notes: inputs.piano_notes.as_deref(),
            error: inputs.error.as_deref(),
            audio_warning: self.audio_warning(),
            capabilities: self.capabilities,
            menus: &inputs.menus,
            menu: self.menu.as_ref(),
            fps: if self.pinned_frame_inputs() {
                0.0
            } else {
                self.performance.fps()
            },
            render_ms: if self.pinned_frame_inputs() {
                0.0
            } else {
                self.performance.render_ms()
            },
            caret: self.caret_position(),
            cycle: inputs
                .clock
                .map(|clock| clock.1)
                .or_else(|| snapshot.map(|value| value.cycle)),
            cps: inputs
                .clock
                .map(|clock| clock.2)
                .or_else(|| snapshot.map(|value| value.cps)),
            zen: self.ui_settings.zen,
            show_menu: self.ui_settings.show_menu,
            show_header: self.ui_settings.show_header,
            show_footer: self.ui_settings.show_footer,
            evaluation_flash: inputs.flashing,
            focus_flash: self.focus_rotation_flash_room(),
            stats: inputs.stats,
            metric_detail: self.ui_settings.metric_detail,
            pressure: snapshot.and_then(|snapshot| snapshot.pressure.as_ref()),
            max_polyphony_override: self.worker.master().max_polyphony_override(),
            master: &self.master,
            master_limiter: self
                .live_master_limiter()
                .map(|settings| settings.character.short()),
            audio: self.visual.audio(),
            audition_audio: self.visual.audition_audio(),
            lint: inputs.lint_message.as_deref(),
            go: self.go_state(),
            #[cfg(feature = "hydra")]
            webcam: visuals
                .hydra_webcam
                .as_ref()
                .filter(|status| status.requested)
                .map(|status| status.state),
            recording: self.recording_chip(),
            take_notice: self.take_notice.as_ref().map(|(text, _)| text.as_str()),
            toast: self.toast.as_ref().map(|(text, _)| text.as_str()),
            unseen_warnings: self.log.unseen(),
            exporting: inputs.export_glance.as_ref(),
            jobs: inputs.jobs_note.as_deref(),
            launch: inputs
                .launch_countdown
                .as_ref()
                .map(|(name, cycles)| (name.as_str(), *cycles)),
            loading: self.loading_line(started),
            clock_in: self.snapshot.as_ref().and_then(|snapshot| {
                let clock = &snapshot.clock;
                clock
                    .in_port
                    .as_ref()
                    .and(clock.external_bpm.map(|bpm| (bpm, clock.locked)))
            }),
            clock_out: self
                .snapshot
                .as_ref()
                .is_some_and(|snapshot| snapshot.clock.out_port.is_some()),
            now: started,
            device: inputs.device.as_deref(),
            input: inputs.input_device.as_deref(),
            input_chosen: self.input_chosen(),
            device_info: snapshot.and_then(|snapshot| snapshot.device.as_ref()),
            midi_ports: if self.pinned_frame_inputs() {
                MidiPortCounts::default()
            } else {
                self.devices.inventory().midi_port_counts()
            },
            pads: if self.pinned_frame_inputs() {
                0
            } else {
                rustel_core::gamepad::connected_pads().len()
            },
            pad_active: if self.pinned_frame_inputs() {
                false
            } else {
                rustel_core::gamepad::connected_pads()
                    .iter()
                    .filter_map(|(slot, _)| rustel_core::gamepad::pad(*slot))
                    .any(|pad| {
                        pad.idle_for_millis()
                            .is_some_and(|idle| idle < ACTIVITY_LIGHT_MILLIS)
                    })
            },
            midi_active: if self.pinned_frame_inputs() {
                false
            } else {
                self.pads.active_within(ACTIVITY_LIGHT_MILLIS)
            },
            input_active: if self.pinned_frame_inputs() {
                false
            } else {
                self.input_lit_at
                    .is_some_and(|at| at.elapsed() < Duration::from_millis(ACTIVITY_LIGHT_MILLIS))
            },
            input_peak_db: self.input_meter.peak_db(),
            orbits: self
                .snapshot
                .as_ref()
                .map(|snapshot| snapshot.orbits.as_slice())
                .unwrap_or(&[]),
            output_pairs: self
                .snapshot
                .as_ref()
                .map(|snapshot| snapshot.output_pairs)
                .unwrap_or(1),
        }
    }

    /// Each pane's slider chips, in pane order. Controls are not
    /// highlights: a slider is on screen wherever its call is, evaluated or
    /// not, in every pane.
    fn pane_slider_chips(&self) -> Vec<Vec<view::SliderChip>> {
        self.panes
            .iter()
            .map(|pane| self.slider_chip_views(pane.scene))
            .collect::<Vec<_>>()
    }

    /// Each pane as this frame draws it. Only the pane showing the audible
    /// scene carries its sounding marks; every pane carries its sliders,
    /// lint, brackets and overridden tempo calls.
    fn pane_views<'a>(
        &'a self,
        maps: &'a [ScreenMap],
        chip_sets: &'a [Vec<view::SliderChip>],
        inputs: &'a FrameInputs,
    ) -> Vec<PaneView<'a>> {
        let audible = self.audible_scene;
        self.panes
            .iter()
            .zip(maps)
            .enumerate()
            .filter_map(|(index, (pane, map))| {
                let scene = self.scenes.get(pane.scene)?;
                let decorations = if Some(pane.scene) == audible && settings::highlights() {
                    Decorations {
                        active: &self.marks,
                        mini: &self.mini_spans,
                        sliders: &chip_sets[index],
                        errors: &inputs.lint_ranges[index],
                        brackets: &inputs.bracket_ranges[index],
                        caret_shape: self.ui_settings.caret_shape,
                        overridden: &inputs.overridden_ranges[index],
                    }
                } else {
                    Decorations {
                        sliders: &chip_sets[index],
                        errors: &inputs.lint_ranges[index],
                        brackets: &inputs.bracket_ranges[index],
                        caret_shape: self.ui_settings.caret_shape,
                        overridden: &inputs.overridden_ranges[index],
                        ..Decorations::default()
                    }
                };
                Some(PaneView {
                    prebake: scene.prebake(),
                    replay: self.replays.get(&scene.id),
                    editor: &scene.editor,
                    map,
                    minimap: &scene.minimap,
                    decorations,
                    name: scene.name(),
                    dirty: scene.dirty,
                    playing: inputs.playing_scene == Some(pane.scene),
                    focused: index == self.focused,
                    flash: inputs.flashing
                        && self
                            .evaluation_flash
                            .as_ref()
                            .is_some_and(|flash| flash.pane == index && flash.scene == pane.scene),
                    locate_flash: (index == self.focused)
                        .then_some(inputs.locating_caret)
                        .flatten(),
                    timeline_focus: index == self.focused && self.focus == Focus::Timeline,
                    line_numbers: self.ui_settings.line_numbers,
                })
            })
            .collect::<Vec<_>>()
    }

    /// Everything the frame draws beyond the chrome: the panes, the docks,
    /// and every panel and sheet that is open.
    fn studio_view<'a>(
        &'a self,
        panes: Vec<PaneView<'a>>,
        inputs: &'a FrameInputs,
        visuals: VisualReadings,
        overlays: &Overlays,
    ) -> StudioView<'a> {
        StudioView {
            show_scrollbars: self.ui_settings.show_scrollbars,
            prebake_rows: self.prebake_rows(),
            sets_folder: self.sets_folder_label(),
            recordings_folder: self.recordings_folder_label(),
            set_limiter: self.set_limiter_label(),
            sample_cache: self.sample_cache_label(),
            precache: self.precache.clone(),
            sources: self.source_rows(),
            mappings: self.mapping_slot_views(),
            keybind_rows: self.keybind_rows(),
            latency: self.latency_report(),
            // Only built while the panel could draw them: merging the port
            // lists is cheap, but not free, and most frames have no panel.
            midi_rows: if self.panel.is_some() {
                self.midi_rows()
            } else {
                Vec::new()
            },
            clock_in: self.ui_settings.clock_in.as_deref(),
            clock_out: self.ui_settings.clock_out.as_deref(),
            caching_samples: self.caching_samples,
            library_loading: self.library_loading,
            // How far into the sample the preview has got, on this frame.
            // Measured from when it was asked for rather than reported by
            // the engine: an audition is one shot with no transport of its
            // own, and the clock that started it is the one that knows.
            preview_shape: self.preview_shape.as_ref().and_then(|(shape, seconds)| {
                let (_, started) = self.preview_armed.as_ref()?;
                let played = if *seconds > 0.0 {
                    (started.elapsed().as_secs_f64() / seconds) as f32
                } else {
                    0.0
                };
                (played <= 1.0).then_some((&shape[..], played))
            }),
            importing_sources: self
                .source_reports
                .iter()
                .filter(|report| {
                    report.state == rustel_runtime::samples::GlobalSourceState::Loading
                })
                .count(),
            panes,
            focused_panel: match self.focus {
                Focus::Panel(kind) => Some(kind),
                Focus::Editor | Focus::Timeline => None,
            },
            help_open: overlays.help.is_some()
                || overlays.precision.is_some()
                || overlays.replay_edit.is_some()
                || self.piano.open,
            preview_gain: self.preview_gain,
            // Which note of a previewed run should be lit: the step the
            // clock has reached since it started.
            sounding_note: self.sounding_preview_note(),
            // Only while the shelf is showing the snippet that is playing:
            // moving to another row is not a reason to light that one up.
            #[cfg(feature = "hydra")]
            snippet_playing: self
                .snippet_preview
                .as_ref()
                .map(|preview| (preview.code.as_str(), self.snippet_marks.as_slice())),
            #[cfg(feature = "hydra")]
            snippet_preview_status: self.snippet_preview.as_ref().map(|preview| {
                // The strip rides only when there is no word: a wait that
                // says loading has a position to show, and both together
                // is more than the row needs to say.
                let note = self.snippet_preview_note();
                let progress = note
                    .is_none()
                    .then(|| self.snippet_preview_progress())
                    .flatten();
                super::super::view::SnippetPreviewStatus {
                    row: preview.row,
                    note,
                    progress,
                }
            }),
            audition_loading: self
                .snapshot
                .as_ref()
                .and_then(|snapshot| snapshot.audition_loading.clone()),
            visual: &self.visual,
            devices: self.devices.inventory(),
            scanning: !self.devices.has_scanned(),
            panel: self.panel,
            scenes: &inputs.chips,
            strip_mode: &self.strip_mode,
            reference: self
                .reference_panel
                .as_ref()
                .map(|panel| (&self.reference, panel)),
            #[cfg(feature = "hydra")]
            snippet_picture: self.hydra_shelf_last.is_some(),
            #[cfg(not(feature = "hydra"))]
            snippet_picture: false,
            #[cfg(feature = "hydra")]
            snippet_refused: self.snippet_refusal(),
            #[cfg(not(feature = "hydra"))]
            snippet_refused: None,
            log: self.log_panel.as_ref().map(|panel| (&self.log, panel)),
            log_memory: self
                .log_panel
                .as_ref()
                .map(|_| self.memory_figures(inputs.stats)),
            memory: self
                .memory_dock
                .map(|dock| (self.memory_figures(inputs.stats), dock.dock(self.frame))),
            jobs: self
                .jobs_panel
                .as_ref()
                .map(|panel| (panel, inputs.background_jobs.as_slice())),
            export: self.export_sheet.as_ref(),
            theme_picker: self.theme_picker.as_ref(),
            set_panel: self.set_panel.as_ref(),
            set_prompt: self.set_prompt.as_ref().map(|(_, picker)| picker),
            viz: std::array::from_fn(|index| {
                self.viz_docks[index].as_ref().map(|panel| view::VizView {
                    panel,
                    widgets: &self.prefs.visuals[index].widgets,
                    set_name: &inputs.set_name,
                    level: visuals.mix_level,
                    seconds: visuals.sound_seconds,
                    extent: visuals.dock_extents[index],
                })
            }),
            viz_focus: self.viz_focus,
            mixer: self.mixer_facts(),
            mixer_panel: self.mixer_panel,
            mixer_selection: self.mixer_selection.as_ref(),
            reference_on_top: self.reference_on_top(),
            #[cfg(feature = "hydra")]
            theme_camera: self
                .theme_picker
                .as_ref()
                .and_then(|_| self.theme_camera_note(visuals.hydra_webcam.as_ref())),
            #[cfg(not(feature = "hydra"))]
            theme_camera: None,
            #[cfg(feature = "hydra")]
            hydra_webcam: visuals.hydra_webcam,
            settings: self
                .settings_sheet
                .map(|sheet| (sheet, &self.ui_settings, &self.features)),
        }
    }

    /// The layers the paint half lays over the view, and what each must
    /// leave alone: the interface's rectangles, the selection and the
    /// sounding marks, and the stage's pictures with their exclusions.
    fn frame_parts(
        &self,
        maps: &[ScreenMap],
        chip_sets: &[Vec<view::SliderChip>],
        menus: &[super::super::menu::Menu],
        overlays: &Overlays,
        snippet_box: Option<Rect>,
    ) -> FrameParts {
        let theme_background = self.theme.background;
        // A score-owned Hydra picture takes the same precedence over a cell
        // theme that it takes over a Hydra theme.
        #[cfg(feature = "hydra")]
        let theme_native_visible = self.hydra_last.is_none();
        #[cfg(not(feature = "hydra"))]
        let theme_native_visible = true;
        let chrome_rects = self.chrome_rects(menus, overlays);
        let selection_cells = self.selected_cells(maps);
        // A sounding event's highlight is the top layer: the picture does
        // not run through it.
        let highlight_cells = self.marked_cells(maps);
        // The selection's wash is the theme's own budget - the most its
        // selection colours can take before the text on them drops below
        // legible - scaled by how far `ui opacity` sits from its default, so
        // a solid interface takes the selection solid with it.
        let selection_strength = {
            let budget = super::super::theme::selection_wash(&self.theme);
            let leak = f32::from(100 - self.ui_settings.interface_opacity) / 100.0;
            (budget * (leak / 0.2)).clamp(0.0, budget)
        };

        // The global painters - `pianoroll()` without its underscore - draw
        // on the stage: the audible pane's whole area, behind the text and
        // over Hydra, at the editor's own opacity. Rendered each to a
        // scratch buffer of the pane and laid under the score after the
        // frame is drawn.
        let stage_background = self.theme.background;
        let (stage, stage_images) =
            super::super::graphics::capture_images(|| self.stage_pictures());
        let stage_strength = {
            let leak = f32::from(100 - self.ui_settings.editor_opacity.min(100)) / 100.0;
            leak.powf(2.2)
        };
        let mut native_keep = snippet_box.iter().copied().collect::<Vec<_>>();
        let mut slider_rails = Vec::new();
        // Protect each complete rail. Its gaps remain transparent unless
        // the theme, an armed control or a selection supplies a fill.
        for (map, chips) in maps.iter().zip(chip_sets) {
            for row in map.rows() {
                let super::super::editor::ScreenRow::Text(row) = row else {
                    continue;
                };
                // DecorationIndex addresses at most 256 distinct controls.
                for chip in chips.iter().take(usize::from(u8::MAX) + 1) {
                    let Some((left, right)) = view::slider_cover_bounds(row, chip.from, chip.to)
                    else {
                        continue;
                    };
                    let filled = self.theme.slider_fill.is_some()
                        || chip.armed
                        || (left..right).any(|x| selection_cells.contains(&(x, row.screen_y)));
                    let rails = if filled {
                        &mut native_keep
                    } else {
                        &mut slider_rails
                    };
                    // A sheet covering the rail keeps its own interface wash.
                    let mut start = left;
                    for x in left..right {
                        if chrome_rects
                            .iter()
                            .any(|rect| within(*rect, x, row.screen_y))
                        {
                            if start < x {
                                rails.push(Rect::new(start, row.screen_y, x - start, 1));
                            }
                            start = x + 1;
                        }
                    }
                    if start < right {
                        rails.push(Rect::new(start, row.screen_y, right - start, 1));
                    }
                }
            }
        }
        let selection_cells = self.visible_selection(selection_cells, menus, overlays);
        native_keep.extend(self.piano_zen_row());
        #[cfg(feature = "remote-control")]
        let remote_panel_area = self.remote_panel_area();
        #[cfg(feature = "remote-control")]
        native_keep.extend(remote_panel_area);
        if overlays.precision.is_some() {
            native_keep.push(slider_precision::geometry(self.frame));
        }
        if overlays.replay_edit.is_some() {
            native_keep.push(replay_edit::geometry(self.frame));
        }
        if overlays.theme_editor_visible
            && let Some(sheet) = super::super::theme_editor::ThemeEditorView::geometry(self.frame)
        {
            native_keep.push(sheet);
        }
        if overlays.help.is_some()
            && let Some(sheet) = HelpView::geometry(self.frame)
        {
            native_keep.push(sheet);
        }
        let mut stage_keep = self.stage_exclusions(maps);
        stage_keep.extend_from_slice(&chrome_rects);
        stage_keep.extend_from_slice(&native_keep);
        let stage_untouched = highlight_cells.union(&selection_cells).copied().collect();
        #[cfg(feature = "hydra")]
        let shelf_keep = self.shelf_exclusions(overlays.theme_editor_visible);
        #[cfg(all(feature = "hydra", feature = "remote-control"))]
        let shelf_keep = shelf_keep.into_iter().chain(remote_panel_area).collect();
        FrameParts {
            #[cfg(feature = "hydra")]
            snippet_box,
            theme_background,
            theme_native_visible,
            chrome_rects,
            selection_cells,
            highlight_cells,
            selection_strength,
            stage_background,
            stage,
            stage_images,
            stage_strength,
            native_keep,
            slider_rails,
            stage_keep,
            stage_untouched,
            #[cfg(feature = "hydra")]
            shelf_keep,
        }
    }

    /// Rectangles that remain interface under either visual renderer. A
    /// colour comparison alone cannot protect themes whose panels share
    /// the editor background, and a Hydra snippet thumbnail must never
    /// inherit characters from a cell effect.
    fn chrome_rects(&self, menus: &[super::super::menu::Menu], overlays: &Overlays) -> Vec<Rect> {
        // The interface, by rect rather than by colour: a theme that
        // paints its chrome in `reset` (terminal) or in the editor's own
        // background (mono) is indistinguishable from the score by colour.
        let mut rects = vec![
            // The bar and any open dropdown are interface by the same
            // argument: a fresh menu surface is exactly "a cell still
            // showing the theme's plain background", so without these
            // the picture repaints straight through it and the menu
            // pulses with the music.
            self.menu_row(),
            self.regions.header,
            self.regions.scenes,
            self.regions.footer,
        ];
        if let Some(state) = self.menu.as_ref() {
            rects.extend(super::super::menu::dropdown_rects(
                menus,
                state,
                self.menu_row(),
                self.frame,
            ));
        }
        rects.extend(self.docked_chrome());
        if self.log_panel.is_some()
            && let Some((sheet, _)) =
                super::super::log::LogPanelView::geometry(self.log_sheet_frame(), self.log_extent())
        {
            rects.push(sheet);
        }
        if self.jobs_panel.is_some() {
            let count = self.background_jobs().len();
            if let Some(sheet) = JobsPanelView::sheet_area(self.frame, count) {
                rects.push(sheet);
            }
        }
        if self.export_sheet.is_some()
            && let Some(sheet) = super::super::export::ExportSheetView::geometry(self.frame)
        {
            rects.push(sheet);
        }
        if self.settings_sheet.is_some()
            && let Some((sheet, _)) =
                super::super::settings::SettingsSheetView::geometry(self.settings_sheet_frame())
        {
            rects.push(sheet);
        }
        if let Some(picker) = self.theme_picker.as_ref()
            && let Some((sheet, _)) = picker.geometry(self.frame)
        {
            rects.push(sheet);
        }
        if self.theme_editor_sheet_visible()
            && let Some(sheet) = super::super::theme_editor::ThemeEditorView::geometry(self.frame)
        {
            rects.push(sheet);
        }
        if overlays.help.is_some()
            && let Some(sheet) = HelpView::geometry(self.frame)
        {
            rects.push(sheet);
        }
        if overlays.precision.is_some() {
            rects.push(slider_precision::geometry(self.frame));
        }
        if let Some((anchor, rows, _)) = &overlays.smart {
            rects.push(smart_action::geometry(
                self.frame,
                *anchor,
                rows.len() as u16,
            ));
        }
        if overlays.replay_edit.is_some() {
            rects.push(replay_edit::geometry(self.frame));
        }
        rects.retain(|rect| !rect.is_empty());
        rects
    }

    /// The selected cells still in sight: those a sheet or an open dropdown
    /// is drawn over are left out.
    fn visible_selection(
        &self,
        mut cells: HashSet<(u16, u16)>,
        menus: &[super::super::menu::Menu],
        overlays: &Overlays,
    ) -> HashSet<(u16, u16)> {
        // A sheet drawn over a selection hides it; washing the sheet's
        // cells at the selection's strength would tint furniture for
        // text nobody can see.
        let mut covers: Vec<Rect> = Vec::new();
        if self.log_panel.is_some()
            && let Some((sheet, _)) =
                super::super::log::LogPanelView::geometry(self.log_sheet_frame(), self.log_extent())
        {
            covers.push(sheet);
        }
        covers.push(self.regions.memory);
        if self.export_sheet.is_some()
            && let Some(sheet) = super::super::export::ExportSheetView::geometry(self.frame)
        {
            covers.push(sheet);
        }
        if self.settings_sheet.is_some()
            && let Some((sheet, _)) =
                super::super::settings::SettingsSheetView::geometry(self.settings_sheet_frame())
        {
            covers.push(sheet);
        }
        if let Some(picker) = self.theme_picker.as_ref()
            && let Some((sheet, _)) = picker.geometry(self.frame)
        {
            covers.push(sheet);
        }
        if overlays.theme_editor_visible
            && let Some(sheet) = super::super::theme_editor::ThemeEditorView::geometry(self.frame)
        {
            covers.push(sheet);
        }
        if overlays.help.is_some()
            && let Some(sheet) = HelpView::geometry(self.frame)
        {
            covers.push(sheet);
        }
        // A dropdown hides the selection under it like any other sheet.
        if let Some(state) = self.menu.as_ref() {
            covers.extend(super::super::menu::dropdown_rects(
                menus,
                state,
                self.menu_row(),
                self.frame,
            ));
        }
        if !covers.is_empty() {
            cells.retain(|&(x, y)| !covers.iter().any(|sheet| within(*sheet, x, y)));
        }
        cells
    }

    /// TachyonFX themes choose whether to transform the completed
    /// Ratatui cells or become a stretched RGBA backdrop. Both obey
    /// the same editor, interface and visuals opacity limits. The
    /// backdrop comes back for the pixel tier to send.
    fn paint_theme_native(
        theme_visual: &mut ThemeVisualEngine,
        frame: &mut ratatui::Frame<'_>,
        area: Rect,
        native: &NativeVisual,
        parts: &FrameParts,
        started: Instant,
    ) -> Option<super::super::view::VisualBackdrop> {
        let mut native_picture = None;
        if parts.theme_native_visible {
            match native.mode {
                Some(TachyonMode::Text) => theme_visual.paint_text(
                    started,
                    frame.buffer_mut(),
                    area,
                    ThemeVisualSurfaces {
                        editor_strength: native.editor_strength,
                        interface_strength: native.interface_strength,
                        interface: &parts.chrome_rects,
                        protected: &parts.native_keep,
                    },
                ),
                Some(TachyonMode::Image) => {
                    if let Some(image) = theme_visual.image(started, frame.buffer_mut(), area) {
                        let picture = super::super::view::VisualBackdrop {
                            width: image.width,
                            height: image.height,
                            rgba: image.rgba,
                            strength: native.editor_strength,
                            interface: native.interface_strength,
                        };
                        if super::super::graphics::tier() != super::super::graphics::Tier::Pixels {
                            picture.paint_excluding(
                                frame.buffer_mut(),
                                area,
                                parts.theme_background,
                                &parts.native_keep,
                                &parts.chrome_rects,
                                Some(&parts.selection_wash()),
                            );
                        }
                        native_picture = Some(picture);
                    }
                }
                None => {}
            }
        }
        native_picture
    }

    /// Hydra's pictures as cell colours. The score's picture fills the
    /// screen behind the code; the shelf's fills its own box. They are two
    /// renderers, so both can be on at once and neither waits for the other.
    #[cfg(feature = "hydra")]
    fn paint_hydra_cells(
        frame: &mut ratatui::Frame<'_>,
        area: Rect,
        parts: &FrameParts,
        backdrop: Option<&super::super::view::VisualBackdrop>,
        shelf: Option<&super::super::view::VisualBackdrop>,
    ) {
        if let Some(picture) = backdrop {
            // The reference column takes the interface wash like any
            // other panel - by rect, because on themes that reuse the
            // editor's background (mono) or the terminal's (reset)
            // its cells are indistinguishable from the score's.
            picture.paint_excluding(
                frame.buffer_mut(),
                area,
                parts.theme_background,
                &parts.native_keep,
                &parts.chrome_rects,
                Some(&parts.selection_wash()),
            );
        }
        if let (Some(picture), Some(box_area)) = (shelf, parts.snippet_box) {
            picture.paint_excluding(
                frame.buffer_mut(),
                box_area,
                parts.theme_background,
                &[],
                &[],
                None,
            );
        }
    }

    /// The theme's character effects, at the backdrop's opacity, around
    /// the interface, the protected surfaces, the selection and the
    /// sounding marks.
    fn paint_theme_characters(
        &self,
        theme_visual: &mut ThemeVisualEngine,
        frame: &mut ratatui::Frame<'_>,
        area: Rect,
        parts: &FrameParts,
        started: Instant,
    ) {
        theme_visual.paint_characters(
            started,
            frame.buffer_mut(),
            area,
            CharacterVisualSurfaces {
                strength: f32::from(self.ui_settings.backdrop_opacity.min(100)) / 100.0,
                interface: &parts.chrome_rects,
                protected: &parts.native_keep,
                selected: &parts.selection_cells,
                highlighted: &parts.highlight_cells,
            },
        );
    }

    /// The dialogs over the score - the precision slider, the replay edit,
    /// the smart action - then help over all of it, and on the zen row the
    /// piano's indicator, or the notes it has kept when no pulse is lit.
    fn paint_overlays(
        &self,
        frame: &mut ratatui::Frame<'_>,
        overlays: &Overlays,
        status: &str,
        piano_notes: Option<&str>,
    ) {
        if let Some(precision) = &overlays.precision {
            precision.render(
                frame,
                &self.theme,
                overlays.help.is_none() && self.menu.is_none() && !self.piano.open,
            );
        }
        if let Some(edit) = &overlays.replay_edit {
            edit.render(
                frame,
                &self.theme,
                overlays.help.is_none() && self.menu.is_none() && !self.piano.open,
            );
        }
        if let Some((anchor, rows, caret)) = &overlays.smart {
            smart_action::render(frame, &self.theme, *anchor, rows, *caret);
        }
        // Absolutely last: help covers the theme editor, every other
        // panel, toasts, the cell-Hydra wash, and stage pictures alike.
        if let Some(state) = overlays.help {
            frame.render_widget(
                HelpView {
                    state,
                    context: overlays.help_context,
                    capabilities: self.capabilities,
                    keybinds: &self.keybinds,
                    theme: &self.theme,
                },
                frame.area(),
            );
        }
        if let (Some(pulse), Some(row)) = (self.piano_pulse(), self.piano_zen_row()) {
            view::render_piano_indicator(frame.buffer_mut(), row, status, &self.theme, pulse);
        } else if let (Some(notes), Some(row)) = (piano_notes, self.piano_zen_row()) {
            view::render_piano_notes(frame.buffer_mut(), row, notes, &self.theme);
        }
    }

    /// Send the frame's pictures as real pixels, when the terminal takes
    /// them: the theme's backdrop, Hydra's backdrop and shelf, and the
    /// stage.
    fn push_pixel_pictures(
        &self,
        frame: &mut ratatui::Frame<'_>,
        terminal_area: Rect,
        parts: FrameParts,
        native_picture: Option<super::super::view::VisualBackdrop>,
        #[cfg_attr(not(feature = "hydra"), allow(unused_variables))] overlays: &Overlays,
    ) {
        // On a terminal that speaks kitty's graphics protocol the picture goes
        // out as real pixels, placed under the text and scaled into the whole
        // grid - so a modest render fills the screen without a screen's worth
        // of pixels crossing every frame. Elsewhere it has already been drawn
        // as cell colours, by `paint_theme_native` and `paint_hydra_cells`.
        // Read from the frame's own buffer, inside the terminal's draw that
        // `paint_into` runs in: the terminal swaps buffers when its draw
        // returns, so a buffer read afterwards would see the next frame's
        // cleared glass.
        if super::super::graphics::tier() == super::super::graphics::Tier::Pixels {
            // An image drawn under the glyphs still sits over the cell
            // backgrounds, so the interface is composited into the picture
            // cell by cell - the reference column included, on the same
            // terms as every other panel.
            let score = Rect::new(0, 0, terminal_area.width, terminal_area.height);
            // The frame that just went to the screen, borrowed rather than
            // copied: this runs once a frame over the whole grid.
            let frame = &*frame.buffer_mut();
            if let Some(picture) = native_picture.as_ref() {
                super::super::graphics::push_image(super::super::graphics::PixelImage {
                    area: score,
                    width: u32::from(picture.width),
                    height: u32::from(picture.height),
                    rgba: picture.composited_excluding(
                        parts.theme_background,
                        Some((frame, score)),
                        &parts.native_keep,
                        &parts.chrome_rects,
                        Some(&parts.selection_wash()),
                    ),
                    depth: -1,
                    cells: Some((score.width, score.height)),
                });
            }
            #[cfg(feature = "hydra")]
            {
                // A score or Hydra theme is a backdrop; the shelf remains an
                // inline picture above its panel. Native and Hydra backdrops
                // are mutually exclusive, but either may coexist with it.
                let pictures = [
                    (
                        self.hydra_last.as_ref().or(self.hydra_theme_last.as_ref()),
                        Some(score),
                        -1,
                        true,
                    ),
                    (
                        self.hydra_shelf_last.as_ref().filter(|_| {
                            overlays.help.is_none()
                                && overlays.precision.is_none()
                                && overlays.replay_edit.is_none()
                        }),
                        parts.snippet_box,
                        0,
                        false,
                    ),
                ];
                for (picture, area, depth, backdrop) in pictures {
                    let (Some(picture), Some(area)) = (picture, area) else {
                        continue;
                    };
                    let image = super::super::graphics::PixelImage {
                        area,
                        width: u32::from(picture.width),
                        height: u32::from(picture.height),
                        rgba: picture.composited_excluding(
                            parts.theme_background,
                            backdrop.then_some((frame, area)),
                            if backdrop {
                                &parts.native_keep[..]
                            } else {
                                &[]
                            },
                            if backdrop {
                                &parts.chrome_rects[..]
                            } else {
                                &[]
                            },
                            backdrop.then_some(&parts.selection_wash()),
                        ),
                        depth,
                        cells: Some((area.width, area.height)),
                    };
                    if backdrop {
                        super::super::graphics::push_image(image);
                    } else if let Some(image) = stage_pixels::mask_shelf(image, &parts.shelf_keep) {
                        super::super::graphics::push_image(image);
                    }
                }
            }
            // These were captured before the frame reset. Equal negative
            // depth and later placement put the stage above the backdrop,
            // while inline widgets and all text remain above it.
            for image in parts.stage_images {
                super::super::graphics::push_image(image);
            }
        }
    }

    /// Leave each pane the map it was drawn with, and count the frame's
    /// time into the performance record.
    fn finish_frame(&mut self, maps: Vec<ScreenMap>, started: Instant, stats: ProcessStats) {
        for (pane, map) in self.panes.iter_mut().zip(maps) {
            pane.last_map = Some(map);
        }
        let painted_at = Instant::now();
        for input in self
            .performance
            .record_frame(painted_at.saturating_duration_since(started), painted_at)
        {
            eprintln!("{}", serde_json::json!({ "studio_input_painted": input }));
        }
        if let Some(snapshot) = self.performance.take_running_snapshot(painted_at) {
            self.emit_performance_records(snapshot, stats);
        }
    }

    /// Recompute what the source pane decorates.
    ///
    /// Marks belong to the evaluated generation, but the text on screen may
    /// already have moved on. Each range is followed through the edits made
    /// since that revision, so a highlight keeps sitting on its own notes
    /// while they are being retyped - and disappears only when the text it
    /// marked is actually deleted, which is how the web editor behaves.
    /// Another scene on screen gets no decorations at all: they describe
    /// text it does not show.
    pub(super) fn refresh_decorations(&mut self) {
        self.marks.clear();
        self.mini_spans.clear();
        #[cfg(feature = "hydra")]
        self.snippet_marks.clear();
        self.follow_slider_spans();
        // A graceful stop leaves voices and effects ringing, but the score
        // itself has stopped scheduling. Do not describe those tails as
        // events that are still active in the source. `stop_requested`
        // closes the snapshot's one-frame lag as soon as the key is pressed.
        let sounding_marks_visible = self.sounding_marks_visible();
        let Some(revision) = self.visual_revision else {
            return;
        };
        // Borrow the scene directly so the mark lists stay writable.
        let Some(editor) = self
            .audible_scene
            .and_then(|id| self.scenes.get(id))
            .map(|scene| &scene.editor)
        else {
            return;
        };
        let fade = settings::highlight_fade().unwrap_or_else(|| self.theme.event_fade());
        #[cfg(feature = "hydra")]
        let preview = self.snippet_preview.clone();
        if sounding_marks_visible {
            for mark in self.visual.fading_marks(self.theme.event, f64::from(fade)) {
                // A snippet playing under the score sits past the end of it:
                // its marks belong to the shelf, and following them into the
                // score would light up whatever text happens to be there.
                #[cfg(feature = "hydra")]
                if let Some(preview) = preview.as_ref().filter(|it| mark.from >= it.offset) {
                    if let Some(mark) = preview.mark_in_snippet(&mark) {
                        self.snippet_marks.push(mark);
                    }
                    continue;
                }
                if let Some(range) = editor.map_range_since(revision, mark.from..mark.to) {
                    self.marks.push(SourceMark {
                        from: range.start,
                        to: range.end,
                        ..mark
                    });
                }
            }
        }
        // A snippet lighting up is a frame to draw, the same as the score's
        // - and the first one is how the shelf knows it is sounding.
        #[cfg(feature = "hydra")]
        if !self.snippet_marks.is_empty() {
            self.snippet_heard = true;
            self.dirty_frame = true;
        }
        // A mark letting go keeps the screen drawing until it has.
        if self.marks.iter().any(|mark| mark.strength < 1.0) {
            self.dirty_frame = true;
        }
        if let Some(layout) = self.visual.layout() {
            for &(from, to) in &layout.mini_locations {
                if let Some(range) = editor.map_range_since(revision, from..to) {
                    self.mini_spans.push((range.start, range.end));
                }
            }
        }
    }

    /// The screen cells under the sounding events' marks, in the pane that
    /// shows the audible scene.
    fn marked_cells(&self, maps: &[ScreenMap]) -> std::collections::HashSet<(u16, u16)> {
        let mut cells = std::collections::HashSet::new();
        if self.marks.is_empty() {
            return cells;
        }
        for (pane, map) in self.panes.iter().zip(maps) {
            if Some(pane.scene) != self.audible_scene {
                continue;
            }
            for row in map.rows() {
                let super::super::editor::ScreenRow::Text(row) = row else {
                    continue;
                };
                for cell in &row.cells {
                    if self
                        .marks
                        .iter()
                        .any(|mark| cell.bytes.start.0 < mark.to && mark.from < cell.bytes.end.0)
                    {
                        for x in cell.screen_x.start..cell.screen_x.end {
                            cells.insert((x, row.screen_y));
                        }
                    }
                }
            }
        }
        cells
    }

    /// Every screen cell the editor's text selections cover, across the
    /// panes - the cells [`super::super::view::SelectionWash`] names for the
    /// backdrop, computed from the same maps the frame was drawn with.
    fn selected_cells(&self, maps: &[ScreenMap]) -> std::collections::HashSet<(u16, u16)> {
        let mut cells = std::collections::HashSet::new();
        for (pane, map) in self.panes.iter().zip(maps) {
            let Some(scene) = self.scenes.get(pane.scene) else {
                continue;
            };
            let ranges: Vec<std::ops::Range<usize>> = scene
                .editor
                .selections()
                .ranges()
                .iter()
                .filter(|selection| !selection.is_empty())
                .map(|selection| {
                    let ordered = selection.ordered();
                    ordered.start.0..ordered.end.0
                })
                .collect();
            if ranges.is_empty() {
                continue;
            }
            for row in map.rows() {
                let super::super::editor::ScreenRow::Text(row) = row else {
                    continue;
                };
                for cell in &row.cells {
                    let covered = ranges.iter().any(|range| {
                        cell.bytes.start.0 < range.end && range.start < cell.bytes.end.0
                    });
                    if covered {
                        for x in cell.screen_x.start..cell.screen_x.end {
                            cells.insert((x, row.screen_y));
                        }
                    }
                }
            }
        }
        cells
    }
}

/// The `setcpm(…)` / `setcps(…)` calls in a score, as byte ranges: what an
/// outside clock overrides.
pub(super) fn tempo_call_spans(source: &str) -> Vec<(usize, usize)> {
    let mut spans = Vec::new();
    for name in ["setcpm(", "setcps(", "setCpm(", "setCps("] {
        let mut from = 0;
        while let Some(at) = source[from..].find(name) {
            let start = from + at;
            let open = start + name.len() - 1;
            let mut depth = 0;
            let mut end = None;
            for (offset, character) in source[open..].char_indices() {
                match character {
                    '(' => depth += 1,
                    ')' => {
                        depth -= 1;
                        if depth == 0 {
                            end = Some(open + offset + 1);
                            break;
                        }
                    }
                    _ => {}
                }
            }
            let end = end.unwrap_or(source.len());
            spans.push((start, end));
            from = end;
        }
    }
    spans.sort_unstable();
    spans
}

/// The bracket at the caret and its partner, in the focused editor. Only a
/// window of the text around the caret is read, so a huge score costs no
/// more than a small one.
fn bracket_spans(editor: &Editor) -> Option<Vec<(usize, usize, bool)>> {
    const WINDOW: usize = 64 * 1024;
    let caret = editor.primary_selection().head.0;
    let document = editor.document();
    let len = document.len_bytes();
    let mut start = caret.saturating_sub(WINDOW);
    let mut end = (caret + WINDOW).min(len);
    let text = loop {
        match document.slice(ByteOffset(start)..ByteOffset(end)) {
            Ok(text) => break text,
            Err(_) => {
                // Not on character boundaries: widen to ones that are.
                if start == 0 && end == len {
                    return None;
                }
                start = start.saturating_sub(4);
                end = (end + 4).min(len);
            }
        }
    };
    super::super::editor::brackets::bracket_at(&text, start, caret).map(|found| found.ranges())
}
