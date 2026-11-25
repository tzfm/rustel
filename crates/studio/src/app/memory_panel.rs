//! Memory accounting for the log's left column and legacy dock tests.

use super::sets::step_fixture_band;
use super::*;
use crate::engine::{RECENT_TABS_BYTES, SampleMemory};
#[cfg(test)]
use crate::memory::MemoryDock;
use crate::memory::{self, MemoryFigures, VisualsMemory};
use crate::viz_panel::Dock;
#[cfg(test)]
use crate::viz_panel::Edge;

impl App {
    /// The breakdown as the preferences kept it: its edge, the bottom
    /// until moved, and its height, its default until resized.
    #[cfg(test)]
    fn kept_memory_dock(&self) -> MemoryDock {
        MemoryDock {
            edge: self.prefs.memory_edge.map_or(Edge::Bottom, Edge::band),
            height: self.prefs.memory_height,
        }
    }

    /// Dock the breakdown, with the keyboard or without it. On a terminal
    /// with no room for another band it is docked but not given the
    /// keyboard, and the status line says why nothing appeared.
    #[cfg(test)]
    pub(super) fn open_memory_dock(&mut self, focus: bool) {
        let dock = self.kept_memory_dock();
        self.memory_dock = Some(dock);
        self.count_for_the_breakdown();
        self.prefs.memory_docked = Some(true);
        self.save_prefs_soon();
        self.invalidate_maps();
        self.dirty_frame = true;
        let room = self.frame.is_empty()
            || !self
                .layout_with_fixtures(
                    self.mixer_panel.map(MixerPanel::dock),
                    self.log_dock_request(),
                    self.memory_dock_request(),
                )
                .memory
                .is_empty();
        if !room {
            self.status = format!(
                "memory: no room for another band on this terminal - make it taller, or {} hides it",
                self.shortcut_or_menu(BindAction::Memory, "View > Memory")
            );
            return;
        }
        // Laid at the next draw; the keys stay with it until then.
        self.memory_unlaid = true;
        if focus {
            self.focus_panel(PanelKind::Memory);
        }
        self.status = if !focus {
            format!("memory breakdown docked at the {}", dock.edge.name())
        } else if self.ui_settings.zen {
            // Zen has no docked furniture: the band lies over the score,
            // and Esc puts it away rather than leave it standing there.
            "memory - e top/bottom · -/+ height · Esc closes it".into()
        } else {
            format!(
                "memory docked at the {} - e top/bottom · -/+ height · Esc back to the score",
                dock.edge.name()
            )
        };
    }

    /// What the breakdown counts off the frame's clock: the closed files,
    /// a directory read, and the sample cache's size, a directory walk
    /// that goes off the UI thread and reads "-" until it lands.
    pub(super) fn count_for_the_breakdown(&mut self) {
        self.memory_closed_files = self.closed_file_count();
        self.measure_sample_cache();
        #[cfg(feature = "vst")]
        self.measure_plugins();
    }

    /// Put the breakdown away, and remember that it was.
    pub(super) fn close_memory_dock(&mut self) {
        if self.memory_dock.take().is_none() {
            return;
        }
        self.memory_unlaid = false;
        self.prefs.memory_docked = Some(false);
        self.save_prefs_soon();
        self.invalidate_maps();
        self.settle_focus();
        self.status = "memory breakdown hidden".into();
        self.dirty_frame = true;
    }

    /// The breakdown's keys while it has the keyboard: `e` for the other
    /// band, `-` and `+` - or `=`, the same key without Shift - for its
    /// height, and Esc to hand the keyboard back to the score, leaving it
    /// docked. In zen, where it is a popup over the score, Esc puts it
    /// away, as it does the sticky log there. Returns whether it took the
    /// key.
    pub(super) fn handle_memory_key(&mut self, code: KeyCode, primary: bool) -> bool {
        if primary {
            return false;
        }
        match code {
            KeyCode::Esc if self.ui_settings.zen => self.close_memory_dock(),
            KeyCode::Esc => {
                self.focus = Focus::Editor;
                self.status = format!(
                    "back to the score - the memory breakdown stays docked; {} hides it",
                    self.shortcut_or_menu(BindAction::Memory, "View > Memory")
                );
            }
            KeyCode::Char('e' | 'E') => self.move_memory_dock(),
            KeyCode::Char('-' | '+' | '=') => self.resize_memory_dock(code != KeyCode::Char('-')),
            _ => return false,
        }
        self.dirty_frame = true;
        true
    }

    /// `e`: the other band - the top, or the bottom - remembered, as the
    /// sticky log's and the mixer's own `e` are. Never a column: its rows
    /// are a label and a figure, read across.
    fn move_memory_dock(&mut self) {
        let Some(dock) = self.memory_dock.as_mut() else {
            return;
        };
        dock.edge = dock.edge.flipped_band();
        let edge = dock.edge;
        self.prefs.memory_edge = Some(edge);
        self.save_prefs_soon();
        self.invalidate_maps();
        self.memory_unlaid = true;
        self.status = format!("memory breakdown along the {}", edge.name());
    }

    /// `-` and `+`: its band a row shorter or taller, remembered, stepped
    /// from the rows it has on screen and no taller than the terminal
    /// allows, as the sticky log's is (`step_fixture_band`).
    fn resize_memory_dock(&mut self, grow: bool) {
        let Some(dock) = self.memory_dock else {
            return;
        };
        let frame = self.frame;
        let edge = dock.edge.band();
        let mixer = self.mixer_panel.map(MixerPanel::dock);
        let log = self.log_dock_request();
        let laid = |extent| {
            self.layout_with_fixtures(mixer, log, Some(Dock { edge, extent }))
                .memory
                .height
        };
        let asked = dock.height_on(frame.width);
        let next =
            match step_fixture_band("the memory breakdown", asked, grow, !frame.is_empty(), laid) {
                Ok(next) => next,
                Err(status) => {
                    self.status = status;
                    return;
                }
            };
        if let Some(dock) = self.memory_dock.as_mut() {
            dock.height = Some(next);
        }
        self.prefs.memory_height = Some(next);
        self.save_prefs_soon();
        self.invalidate_maps();
        self.memory_unlaid = true;
        self.status = format!("memory breakdown {next} rows tall");
    }

    /// Scores in the set's folder that no tab shows. A directory read, so
    /// it is taken when the breakdown opens and when the set panel
    /// refreshes, never per frame.
    pub(super) fn closed_file_count(&self) -> usize {
        self.scenes
            .folder_files()
            .iter()
            .filter(|file| file.open.is_none())
            .count()
    }

    /// Everything the breakdown shows: the header's own figures, the engine's
    /// last snapshot, and what the interface holds, summed over at most a
    /// set's tabs, the audio taps and three pictures.
    ///
    /// Under pinned frame inputs the machine's and the engine's parts are
    /// constants, as the header's are, so a golden holds on every runner.
    pub(super) fn memory_figures(&self, stats: ProcessStats) -> MemoryFigures {
        let pinned = self.pinned_frame_inputs();
        let snapshot = self.snapshot.as_ref().filter(|_| !pinned);
        let samples = if pinned {
            Some(SampleMemory {
                preview_budget_bytes: self.ui_settings.preview_budget.bytes(),
                unused_idle: self.ui_settings.unused_sample_idle.duration(),
                recent_limit_bytes: RECENT_TABS_BYTES,
                ..SampleMemory::default()
            })
        } else {
            snapshot.map(|snapshot| snapshot.sample_memory)
        };
        let (width, height) = (self.frame.width, self.frame.height);
        MemoryFigures {
            total: stats.memory_bytes(),
            resident: stats.resident_bytes,
            samples,
            script_heap: if pinned {
                Some(0)
            } else {
                snapshot.map(|snapshot| snapshot.script_heap_bytes)
            },
            score_engine: snapshot.and_then(|snapshot| snapshot.score_engine),
            audio: snapshot.and_then(|snapshot| snapshot.audio_memory),
            visuals: if pinned { None } else { self.visuals_memory() },
            screen: (width, height),
            // The terminal keeps two frames, the one on screen and the one
            // being drawn, each a `Cell` per character cell.
            screen_bytes: 2
                * usize::from(width)
                * usize::from(height)
                * std::mem::size_of::<ratatui::buffer::Cell>(),
            pictures: if pinned { 0 } else { self.held_picture_bytes() },
            spectrograms: if pinned {
                0
            } else {
                self.visual.history_bytes()
            },
            tabs_open: self.scenes.len(),
            tabs_closed: self.memory_closed_files,
            // The document's length, not its text: `source` would build a
            // string of the whole score on every frame to measure it.
            tab_bytes: self
                .scenes
                .scenes()
                .iter()
                .map(|scene| scene.editor.document().len_bytes() + scene.editor.undo_bytes())
                .sum(),
            playing: self.is_playing(),
            returned_when_stopped: rustel_runtime::free_memory::RELEASES_FREE_MEMORY,
            sample_cache: self.sample_cache_bytes.filter(|_| !pinned),
            fetch_imports: self.ui_settings.precache_sources,
            #[cfg(feature = "vst")]
            plugins: self.plugin_memory.clone(),
            #[cfg(not(feature = "vst"))]
            plugins: Vec::new(),
        }
    }

    /// What Hydra's renderers and inputs hold, read off the renderer
    /// thread's gauges through the backdrop's handle, which shares the
    /// host with the worker that owns it: a few loads, never a wait on the
    /// GPU.
    #[cfg(feature = "hydra")]
    fn visuals_memory(&self) -> Option<VisualsMemory> {
        let memory = self.hydra_backdrop.as_ref()?.memory();
        VisualsMemory::from_hydra(memory)
    }

    /// A build without Hydra has no visuals to hold anything.
    #[cfg(not(feature = "hydra"))]
    fn visuals_memory(&self) -> Option<VisualsMemory> {
        None
    }

    /// The pictures the interface holds on to between frames: Hydra's
    /// last frames, and the pixels a graphics terminal was sent.
    fn held_picture_bytes(&self) -> usize {
        #[cfg(feature = "hydra")]
        let hydra: usize = [
            &self.hydra_last,
            &self.hydra_shelf_last,
            &self.hydra_theme_last,
        ]
        .into_iter()
        .flatten()
        .map(|frame| frame.rgba.capacity())
        .sum();
        #[cfg(not(feature = "hydra"))]
        let hydra = 0;
        hydra + crate::graphics::held_bytes()
    }

    /// Whether a point is on the breakdown's band.
    pub(super) fn memory_dock_at(&self, x: u16, y: u16) -> bool {
        self.memory_dock.is_some() && within(self.regions.memory, x, y)
    }

    /// A press on the breakdown: on its close mark it puts it away, and
    /// anywhere else it gives it the keyboard, as a press on the desk
    /// does. Returns whether it took the press.
    pub(super) fn click_memory_dock(&mut self, x: u16, y: u16) -> bool {
        if !self.memory_dock_at(x, y) {
            return false;
        }
        if memory::close_at(self.regions.memory, x, y) {
            self.close_memory_dock();
        } else {
            self.focus_panel(PanelKind::Memory);
        }
        self.pointer = Some(Pointer::Panel);
        self.dirty_frame = true;
        true
    }

    /// The pointer's shape over the breakdown: a hand on the close mark,
    /// the arrow over the rest of the band, which has nothing else to
    /// press.
    pub(super) fn memory_dock_shape(&self, x: u16, y: u16) -> Option<&'static str> {
        self.memory_dock_at(x, y).then(|| {
            if memory::close_at(self.regions.memory, x, y) {
                SHAPE_POINTER
            } else {
                SHAPE_DEFAULT
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::support::{app_over, press};
    use super::*;
    use crate::keybinds::KeyCombo;
    use crate::menu::MenuAction;
    use crate::settings::MetricDetail;
    use ratatui::{Terminal, backend::TestBackend};

    /// A studio painted once, wide enough for the header to draw its
    /// counters, with a score of a few lines for the caret to land in.
    fn painted_at(
        directory: &std::path::Path,
        width: u16,
        height: u16,
    ) -> (App, Terminal<TestBackend>) {
        let mut app = app_over(directory);
        app.ui_settings.metric_detail = MetricDetail::Basic;
        app.ui_settings.zen = false;
        app.scenes.current_mut().editor =
            Editor::new("$: s(\"bd\")\n$: s(\"hh*4\")\n$: s(\"cp\")\n").expect("score");
        app.focus = Focus::Editor;
        let mut terminal = Terminal::new(TestBackend::new(width, height)).expect("terminal");
        paint(&mut app, &mut terminal);
        (app, terminal)
    }

    fn painted(directory: &std::path::Path) -> (App, Terminal<TestBackend>) {
        painted_at(directory, 200, 40)
    }

    fn paint(app: &mut App, terminal: &mut Terminal<TestBackend>) -> String {
        terminal
            .draw(|frame| app.paint_into(frame, Instant::now()).expect("frame paints"))
            .expect("draw");
        let buffer = terminal.backend().buffer();
        (0..buffer.area.height)
            .map(|y| {
                (0..buffer.area.width)
                    .map(|x| buffer[(x, y)].symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// A click, Down then Up, through the whole routing as the terminal
    /// sends it.
    fn click(app: &mut App, x: u16, y: u16) {
        for kind in [
            MouseEventKind::Down(MouseButton::Left),
            MouseEventKind::Up(MouseButton::Left),
        ] {
            app.handle_terminal_event(Event::Mouse(MouseEvent {
                kind,
                column: x,
                row: y,
                modifiers: KeyModifiers::NONE,
            }))
            .expect("the click is handled");
        }
    }

    fn counters() -> Rect {
        memory::memory_chip().expect("the header drew its counters")
    }

    /// Open the legacy dock directly for its layout and input tests.
    fn opened(app: &mut App, terminal: &mut Terminal<TestBackend>) -> Rect {
        app.open_memory_dock(true);
        paint(app, terminal);
        let band = app.regions.memory;
        assert!(!band.is_empty(), "the band has room");
        band
    }

    /// The header's memory figure opens the log, with its memory column;
    /// clicking it again simply brings the log back into focus.
    #[test]
    fn clicking_the_counters_opens_the_shared_log_and_memory_panel() {
        let directory = tempfile::tempdir().unwrap();
        let (mut app, mut terminal) = painted(directory.path());
        let chip = counters();

        click(&mut app, chip.x + 1, chip.y);
        assert!(app.log_panel.is_some());
        assert!(app.memory_dock.is_none());
        assert_eq!(app.focus, Focus::Panel(PanelKind::Log));
        let screen = paint(&mut app, &mut terminal);
        assert!(screen.contains(" log - "), "{screen}");
        assert!(screen.contains(" mem "), "{screen}");
        assert!(screen.contains(" memory "), "{screen}");
        assert!(screen.contains(" sounds "), "{screen}");
        assert!(app.regions.memory.is_empty());

        app.toggle_log_sticky();
        let screen = paint(&mut app, &mut terminal);
        assert!(screen.contains("script engine"), "{screen}");
        assert!(screen.contains("not itemised"), "{screen}");

        click(&mut app, chip.right() - 1, chip.y);
        assert!(app.memory_dock.is_none());
        assert!(app.log_panel.is_some());
        assert_eq!(app.focus, Focus::Panel(PanelKind::Log));
    }

    /// Old memory dock preferences no longer restore a separate band.
    #[test]
    fn old_memory_dock_preferences_do_not_restore_a_separate_band() {
        let directory = tempfile::tempdir().unwrap();
        let (mut app, mut terminal) = painted(directory.path());
        app.prefs.memory_docked = Some(true);
        app.prefs.memory_edge = Some(Edge::Top);
        app.prefs.memory_height = Some(9);
        app.restore_panels_from_prefs();
        assert!(app.memory_dock.is_none());
        assert_eq!(app.focus, Focus::Editor, "keyless");
        paint(&mut app, &mut terminal);
        assert!(app.regions.memory.is_empty());

        let mut closed = app_over(directory.path());
        closed.prefs.memory_docked = Some(false);
        closed.restore_panels_from_prefs();
        assert!(closed.memory_dock.is_none());
    }

    /// View lists the shared log panel, with no separate memory row.
    #[test]
    fn view_has_no_separate_memory_row() {
        let directory = tempfile::tempdir().unwrap();
        let (app, _terminal) = painted(directory.path());
        let rows: Vec<_> = app
            .menus()
            .iter()
            .flat_map(|menu| menu.items.clone())
            .collect();
        assert!(
            !rows
                .iter()
                .any(|item| item.id() == Some(MenuAction::Memory))
        );
        assert!(rows.iter().any(|item| item.id() == Some(MenuAction::Log)));
    }

    /// Over the counters the pointer is a hand; over the band it is the
    /// arrow but for the close mark, which is a hand and puts it away.
    #[test]
    fn the_pointer_is_a_hand_over_the_counters_and_the_close_mark() {
        let directory = tempfile::tempdir().unwrap();
        let (mut app, mut terminal) = painted(directory.path());
        let chip = counters();
        assert_eq!(app.pointer_shape_at(chip.x, chip.y), SHAPE_POINTER);
        assert_eq!(
            app.pointer_shape_at(chip.right() - 1, chip.y),
            SHAPE_POINTER
        );

        let band = opened(&mut app, &mut terminal);
        let mark = band.right() - 3;
        assert_eq!(app.pointer_shape_at(mark, band.y), SHAPE_POINTER);
        assert_eq!(app.pointer_shape_at(band.x + 10, band.y + 3), SHAPE_DEFAULT);
        click(&mut app, mark, band.y);
        assert!(app.memory_dock.is_none(), "the close mark puts it away");
        assert_eq!(app.focus, Focus::Editor);
    }

    /// A learnt memory chord opens the shared panel even from settings.
    #[test]
    fn a_learnt_memory_chord_opens_the_shared_panel() {
        let directory = tempfile::tempdir().unwrap();
        let (mut app, _terminal) = painted(directory.path());
        assert!(app.keybinds.is_unbound(BindAction::Memory));
        let chord = KeyCombo::parse("ctrl+shift+f8").expect("the chord parses");
        app.keybinds.learn(BindAction::Memory, Some(chord));
        let modifiers = KeyModifiers::CONTROL | KeyModifiers::SHIFT;

        press(&mut app, KeyCode::F(8), modifiers);
        assert!(app.log_panel.is_some());
        assert!(app.memory_dock.is_none());
        assert_eq!(app.focus, Focus::Panel(PanelKind::Log));
        press(&mut app, KeyCode::Esc, KeyModifiers::NONE);

        press(&mut app, KeyCode::Char('o'), KeyModifiers::CONTROL);
        assert_eq!(app.focus, Focus::Panel(PanelKind::Settings));
        press(&mut app, KeyCode::F(8), modifiers);
        assert!(
            app.log_panel.is_some(),
            "the chord reached the shared panel"
        );
        assert!(app.settings_sheet.is_none(), "the log replaces the sheet");
        assert_eq!(app.focus, Focus::Panel(PanelKind::Log));
    }

    /// The breakdown reads Hydra through the backdrop's handle: a host that
    /// holds something is a visuals line, and an empty host or no host is
    /// none.
    #[cfg(feature = "hydra")]
    #[test]
    fn the_visuals_are_read_through_the_backdrops_handle() {
        let directory = tempfile::tempdir().unwrap();
        let (mut app, _terminal) = painted(directory.path());
        let visuals = |app: &App| app.memory_figures(ProcessStats::default()).visuals;
        assert_eq!(visuals(&app), None, "no host");

        let host = rustel_hydra::HydraHost::new();
        app.hydra_backdrop = Some(host.frames());
        assert_eq!(visuals(&app), None, "a host holding nothing");

        let camera = host.input_sink().bind(0).expect("an s0 lease");
        assert!(camera.publish(2, 2, vec![0; 16]).unwrap());
        assert_eq!(
            visuals(&app),
            Some(VisualsMemory {
                renderers: Vec::new(),
                inputs: 16,
                gpu: 0,
            }),
            "a camera frame waiting for the GPU"
        );
    }

    /// The closed files are counted when it opens and again when the set
    /// changes, not per frame; the open tabs' text is counted without
    /// building it.
    #[test]
    fn closed_files_are_counted_on_opening_and_on_a_refresh() {
        let directory = tempfile::tempdir().unwrap();
        let (mut app, mut terminal) = painted(directory.path());
        // A score the set did not open as a tab.
        std::fs::write(directory.path().join("second.strudel"), "$: s(\"hh\")\n").unwrap();
        app.open_log_from_memory();
        paint(&mut app, &mut terminal);
        assert_eq!(app.memory_closed_files, 1);
        let figures = app.memory_figures(ProcessStats::default());
        assert_eq!((figures.tabs_open, figures.tabs_closed), (1, 1));
        assert_eq!(
            figures.tab_bytes,
            app.editor().document().len_bytes() + app.editor().undo_bytes()
        );

        std::fs::write(directory.path().join("third.strudel"), "$: s(\"cp\")\n").unwrap();
        paint(&mut app, &mut terminal);
        assert_eq!(
            app.memory_closed_files, 1,
            "a frame does not read the folder"
        );
        app.refresh_set_panel(None);
        assert_eq!(app.memory_closed_files, 2);
    }
}
