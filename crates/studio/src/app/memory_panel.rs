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

    /// The band takes its own rows: the score and the reference column
    /// stop at it rather than lying under it, so nothing is covered and it
    /// can stay open while the score is worked on.
    #[test]
    fn it_takes_its_own_room_and_covers_nothing() {
        let directory = tempfile::tempdir().unwrap();
        let (mut app, mut terminal) = painted(directory.path());
        press(&mut app, KeyCode::Char('f'), KeyModifiers::CONTROL);
        app.focus = Focus::Editor;
        paint(&mut app, &mut terminal);
        let (pane, reference) = (app.regions.panes[0].editor, app.regions.reference);
        assert!(!reference.is_empty(), "the reference is open");

        let band = opened(&mut app, &mut terminal);
        let (shrunk, stopped) = (app.regions.panes[0].editor, app.regions.reference);
        assert_eq!(
            shrunk.height,
            pane.height - band.height,
            "the score gave the rows"
        );
        assert!(shrunk.bottom() <= band.y);
        assert_eq!(stopped.height, reference.height - band.height);
        assert!(stopped.bottom() <= band.y, "the reference stops at it");
    }

    /// With the keyboard, keys the breakdown has no use for are dropped
    /// with a word rather than typed into a score the eye is not on; Esc
    /// hands the keyboard back and leaves it docked, and after that the
    /// score types and Esc from it puts nothing away.
    #[test]
    fn esc_hands_the_keyboard_back_and_leaves_it_docked() {
        let directory = tempfile::tempdir().unwrap();
        let (mut app, mut terminal) = painted(directory.path());
        opened(&mut app, &mut terminal);
        let source = app.editor().source();

        press(&mut app, KeyCode::Char('x'), KeyModifiers::NONE);
        assert_eq!(app.editor().source(), source, "not typed into the score");
        assert!(
            app.status.contains("the memory has the keyboard"),
            "{}",
            app.status
        );

        press(&mut app, KeyCode::Esc, KeyModifiers::NONE);
        assert_eq!(app.focus, Focus::Editor);
        assert!(app.memory_dock.is_some(), "Esc leaves it docked");
        press(&mut app, KeyCode::Char('x'), KeyModifiers::NONE);
        assert!(app.editor().source().starts_with('x'), "the score took it");
        assert_eq!(app.front_panel(), None, "docked furniture, not a sheet");
        press(&mut app, KeyCode::Esc, KeyModifiers::NONE);
        assert!(
            app.memory_dock.is_some(),
            "Esc from the score leaves it too"
        );
    }

    /// `e` moves it to the top and back, and the band goes with it: under
    /// the scene strip, then over the footer again. The edge is kept.
    #[test]
    fn e_flips_it_between_the_bottom_and_the_top_and_the_band_moves() {
        let directory = tempfile::tempdir().unwrap();
        let (mut app, mut terminal) = painted(directory.path());
        let bottom = opened(&mut app, &mut terminal);

        press(&mut app, KeyCode::Char('e'), KeyModifiers::NONE);
        assert_eq!(app.memory_dock.unwrap().edge, Edge::Top);
        assert_eq!(app.prefs.memory_edge, Some(Edge::Top));
        paint(&mut app, &mut terminal);
        let top = app.regions.memory;
        assert_eq!(top.y, app.regions.scenes.bottom(), "under the strip");
        assert_eq!(top.height, bottom.height);
        assert!(
            app.regions.panes[0].editor.y >= top.bottom(),
            "the score below it"
        );

        press(&mut app, KeyCode::Char('e'), KeyModifiers::NONE);
        assert_eq!(app.prefs.memory_edge, Some(Edge::Bottom));
        paint(&mut app, &mut terminal);
        assert_eq!(app.regions.memory, bottom, "and back, never down a side");
    }

    /// `-` and `+` - `=` without Shift - make the band a row shorter or
    /// taller within a band's bounds, and the height is kept.
    #[test]
    fn minus_and_plus_resize_it_and_the_height_is_kept() {
        use crate::viz_panel::BAND_MIN_HEIGHT;
        let directory = tempfile::tempdir().unwrap();
        let (mut app, mut terminal) = painted(directory.path());
        let band = opened(&mut app, &mut terminal);

        press(&mut app, KeyCode::Char('+'), KeyModifiers::SHIFT);
        press(&mut app, KeyCode::Char('='), KeyModifiers::NONE);
        paint(&mut app, &mut terminal);
        assert_eq!(app.regions.memory.height, band.height + 2);
        assert_eq!(app.prefs.memory_height, Some(band.height + 2));
        assert_eq!(app.regions.memory.bottom(), band.bottom(), "grown upwards");

        for _ in 0..30 {
            press(&mut app, KeyCode::Char('-'), KeyModifiers::NONE);
        }
        paint(&mut app, &mut terminal);
        assert_eq!(app.regions.memory.height, BAND_MIN_HEIGHT);
        assert!(
            app.status.contains("as short as a band goes"),
            "{}",
            app.status
        );
        let screen = paint(&mut app, &mut terminal);
        assert!(screen.contains("script engine"), "the parts stay: {screen}");
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

    /// On one edge the mixer, the sticky log and the breakdown stack: the
    /// desk outermost, then the log, then the breakdown nearest the score,
    /// and the score keeps its least. Moved to the top, the breakdown
    /// leaves the other two where they were.
    #[test]
    fn it_stacks_inside_the_mixer_and_the_log_on_one_edge() {
        let directory = tempfile::tempdir().unwrap();
        let (mut app, mut terminal) = painted_at(directory.path(), 200, 60);
        app.open_mixer_panel(false);
        app.toggle_log_sticky();
        app.focus = Focus::Editor;
        let band = opened(&mut app, &mut terminal);
        let regions = app.regions;
        assert_eq!(
            regions.mixer.bottom(),
            regions.footer.y,
            "the desk outermost"
        );
        assert_eq!(regions.log.bottom(), regions.mixer.y, "the log inside it");
        assert_eq!(band.bottom(), regions.log.y, "the breakdown inside both");
        assert_eq!(band.height, memory::default_height(200));
        assert!(regions.panes[0].editor.bottom() <= band.y);
        assert!(
            regions.panes[0].editor.height >= 8,
            "the score keeps its least"
        );

        press(&mut app, KeyCode::Char('e'), KeyModifiers::NONE);
        paint(&mut app, &mut terminal);
        assert_eq!(app.regions.memory.y, app.regions.scenes.bottom());
        assert_eq!(app.regions.mixer, regions.mixer, "the desk stays put");
        assert_eq!(
            app.regions.log.bottom(),
            regions.log.bottom(),
            "and the log"
        );
    }

    /// On a short terminal the breakdown gives up rows first, to leave the
    /// panes their least. With no row left it is docked but not drawn, says
    /// so, and keeps no keyboard; a taller terminal brings it back. Narrow,
    /// its rows go in one column.
    #[test]
    fn a_small_terminal_shrinks_it_first_and_a_narrow_one_lays_one_column() {
        let directory = tempfile::tempdir().unwrap();
        let (mut app, mut terminal) = painted_at(directory.path(), 200, 24);
        app.prefs.log_height = Some(9);
        app.toggle_log_sticky();
        app.focus = Focus::Editor;
        paint(&mut app, &mut terminal);
        assert_eq!(app.regions.log.height, 9);

        app.open_memory_dock(true);
        assert!(
            app.status.contains("no room for another band"),
            "{}",
            app.status
        );
        assert_eq!(app.focus, Focus::Editor, "no keyboard for a band not shown");
        paint(&mut app, &mut terminal);
        assert!(app.memory_dock.is_some() && app.regions.memory.is_empty());

        terminal.backend_mut().resize(200, 34);
        paint(&mut app, &mut terminal);
        // Twenty-nine rows of body, nine for the log, eight kept by the
        // panes: twelve left, and the breakdown asks for eleven. Three
        // rows fewer and it gives two of its own; the log keeps its nine.
        assert_eq!(app.regions.memory.height, 11, "back once there is room");
        terminal.backend_mut().resize(200, 31);
        paint(&mut app, &mut terminal);
        assert_eq!(app.regions.memory.height, 9, "shrunk before the log is");
        assert_eq!(app.regions.log.height, 9);

        terminal.backend_mut().resize(90, 40);
        let screen = paint(&mut app, &mut terminal);
        let band = app.regions.memory;
        assert_eq!(band.width, 90);
        let first = screen.lines().nth(usize::from(band.y) + 1).unwrap();
        assert!(first.contains("memory"), "{first}");
        assert!(!first.contains("interface"), "one column: {first}");
    }

    /// Zen has no docked furniture: the breakdown is a band laid over the
    /// score, which keeps its full height, as the sticky log is a sheet
    /// there; Esc from the score, or from the band, puts it away, and it
    /// is no stop on ⇧F10's walk. The counters' button goes with the
    /// header.
    #[test]
    fn zen_lays_it_over_the_score_and_esc_puts_it_away() {
        let directory = tempfile::tempdir().unwrap();
        let (mut app, mut terminal) = painted(directory.path());
        opened(&mut app, &mut terminal);
        app.ui_settings.zen = true;
        paint(&mut app, &mut terminal);
        assert_eq!(memory::memory_chip(), None);
        let band = app.regions.memory;
        assert_eq!(band.bottom(), 40, "over the foot of the frame");
        assert_eq!(
            app.regions.panes[0].editor.height, 40,
            "the score keeps it all"
        );

        app.focus = Focus::Editor;
        press(&mut app, KeyCode::F(10), KeyModifiers::SHIFT);
        assert_ne!(app.focus, Focus::Panel(PanelKind::Memory), "no stop in zen");
        app.focus = Focus::Editor;
        press(&mut app, KeyCode::Esc, KeyModifiers::NONE);
        assert!(app.memory_dock.is_none(), "Esc from the score puts it away");

        app.open_memory_dock(true);
        assert_eq!(app.focus, Focus::Panel(PanelKind::Memory));
        press(&mut app, KeyCode::Esc, KeyModifiers::NONE);
        assert!(app.memory_dock.is_none(), "and Esc from the band");
    }

    /// ⇧F10 walks to the docked breakdown, as it does to the sticky log,
    /// and a press on the band gives it the keyboard too.
    #[test]
    fn shift_f10_and_a_press_on_the_band_give_it_the_keyboard() {
        let directory = tempfile::tempdir().unwrap();
        let (mut app, mut terminal) = painted(directory.path());
        let band = opened(&mut app, &mut terminal);
        press(&mut app, KeyCode::Esc, KeyModifiers::NONE);
        assert_eq!(app.focus, Focus::Editor);

        press(&mut app, KeyCode::F(10), KeyModifiers::SHIFT);
        assert_eq!(app.focus, Focus::Panel(PanelKind::Memory));
        press(&mut app, KeyCode::F(10), KeyModifiers::SHIFT);
        assert_eq!(app.focus, Focus::Editor, "and on round to the score");

        click(&mut app, band.x + 10, band.y + 3);
        assert_eq!(app.focus, Focus::Panel(PanelKind::Memory));
        assert!(
            app.memory_dock.is_some(),
            "a press on the band leaves it open"
        );
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

    /// The settings sheet opened beside it leaves it docked and stands
    /// clear of it, so the line a setting moves is in view while it is
    /// changed; the sheet keeps the keyboard, and the counters still put
    /// the breakdown away and bring it back from over it.
    #[test]
    fn the_settings_sheet_stands_clear_of_it() {
        let directory = tempfile::tempdir().unwrap();
        let (mut app, mut terminal) = painted(directory.path());
        let band = opened(&mut app, &mut terminal);

        press(&mut app, KeyCode::Char('o'), KeyModifiers::CONTROL);
        assert!(app.settings_sheet.is_some());
        assert_eq!(app.focus, Focus::Panel(PanelKind::Settings));
        assert!(app.memory_dock.is_some(), "the settings did not close it");
        let screen = paint(&mut app, &mut terminal);
        let (sheet, _) = crate::settings::SettingsSheetView::geometry(app.settings_sheet_frame())
            .expect("the sheet has room");
        assert!(
            sheet.bottom() <= band.y,
            "clear of the band: {sheet:?} {band:?}"
        );
        assert!(screen.contains("script engine"), "in view: {screen}");

        app.close_memory_dock();
        assert!(app.memory_dock.is_none());
        assert!(app.settings_sheet.is_some(), "and the sheet stays");
        app.open_memory_dock(false);
        assert!(app.memory_dock.is_some());
        assert_eq!(
            app.focus,
            Focus::Panel(PanelKind::Settings),
            "the sheet keeps the keys"
        );
    }

    /// Along the top the band starts under the menu, the header and the
    /// strip. The settings sheet is drawn below it, clear of the band, with
    /// the band's lines in view, and not in the chrome above it.
    #[test]
    fn along_the_top_the_settings_sheet_stands_below_it() {
        use crate::settings::SettingsSheetView;
        let directory = tempfile::tempdir().unwrap();
        let (mut app, mut terminal) = painted(directory.path());
        opened(&mut app, &mut terminal);
        press(&mut app, KeyCode::Char('e'), KeyModifiers::NONE);
        paint(&mut app, &mut terminal);
        let band = app.regions.memory;
        assert_eq!(band.y, app.regions.scenes.bottom(), "under the chrome");

        press(&mut app, KeyCode::Char('o'), KeyModifiers::CONTROL);
        assert_eq!(app.focus, Focus::Panel(PanelKind::Settings));
        let screen = paint(&mut app, &mut terminal);
        // The room the menu's Settings and About rows are offered by, too.
        let (sheet, _) =
            SettingsSheetView::geometry(app.settings_sheet_frame()).expect("the sheet has room");
        assert!(
            sheet.y >= band.bottom(),
            "below the band: {sheet:?} {band:?}"
        );
        let tabs = screen.lines().nth(usize::from(sheet.y)).unwrap();
        assert!(tabs.contains(" advanced "), "the sheet is drawn: {tabs}");
        assert!(screen.contains("script engine"), "in view: {screen}");
    }

    /// In zen the band lies over the score and can be most of a short
    /// terminal. Where neither side of it can hold the settings sheet, the
    /// sheet covers the band rather than vanish while it holds the keys.
    #[test]
    fn in_zen_a_band_with_no_room_beside_it_is_covered_by_the_settings() {
        use crate::settings::SettingsSheetView;
        let directory = tempfile::tempdir().unwrap();
        let (mut app, mut terminal) = painted_at(directory.path(), 100, 30);
        opened(&mut app, &mut terminal);
        app.ui_settings.zen = true;
        paint(&mut app, &mut terminal);
        let band = app.regions.memory;
        let above = Rect {
            height: band.y,
            ..app.frame
        };
        assert_eq!(band.bottom(), app.frame.bottom(), "along the foot");
        assert_eq!(
            SettingsSheetView::geometry(above),
            None,
            "too few rows above it: {band:?}"
        );

        press(&mut app, KeyCode::Char('o'), KeyModifiers::CONTROL);
        assert_eq!(app.focus, Focus::Panel(PanelKind::Settings));
        let screen = paint(&mut app, &mut terminal);
        let (sheet, _) =
            SettingsSheetView::geometry(app.settings_sheet_frame()).expect("the sheet draws");
        assert!(!sheet.intersection(band).is_empty(), "over the band");
        let tabs = screen.lines().nth(usize::from(sheet.y)).unwrap();
        assert!(tabs.contains(" advanced "), "the sheet is drawn: {tabs}");
    }

    /// The set panel laid as a sheet - in zen, or on a narrow split - sits
    /// over the band's rows. The band is painted under it, so the sheet that
    /// takes a press on those rows, and the keys after it, is the one seen.
    #[test]
    fn the_set_sheet_is_painted_over_the_band_where_the_two_meet() {
        let directory = tempfile::tempdir().unwrap();
        let (mut app, mut terminal) = painted(directory.path());
        opened(&mut app, &mut terminal);
        app.ui_settings.zen = true;
        app.toggle_set_panel();
        paint(&mut app, &mut terminal);
        let band = app.regions.memory;
        let (sheet, _) = app
            .set_panel
            .as_ref()
            .and_then(|panel| panel.sheet_geometry(app.frame))
            .expect("the set panel is a sheet in zen");
        let meet = sheet.intersection(band);
        assert!(!meet.is_empty(), "{sheet:?} {band:?}");
        let buffer = terminal.backend().buffer();
        for y in meet.y..meet.bottom() {
            let edge = buffer[(sheet.x, y)].symbol();
            assert!(
                matches!(edge, "│" | "╭" | "╰" | "├"),
                "the sheet's own border at row {y}, not the band: {edge:?}"
            );
        }

        let (x, y) = (meet.x + 2, meet.y + 1);
        click(&mut app, x, y);
        assert_eq!(
            app.focus,
            Focus::Panel(PanelKind::Set),
            "the press goes to what is drawn there"
        );
    }

    /// A visuals dock's art text and its add list own the keyboard while
    /// they are up, as the set's prompt does. Docked from over either, the
    /// breakdown leaves them the keys, so the `-` typed next is text, not
    /// a row off the band, and `e` does not move it.
    #[test]
    fn docked_over_a_visuals_prompt_or_add_list_it_leaves_them_the_keys() {
        use crate::viz_panel::WidgetSpec;
        let directory = tempfile::tempdir().unwrap();
        let (mut app, mut terminal) = painted(directory.path());
        app.prefs.visuals[0].widgets = vec![WidgetSpec::default_art()];
        app.open_viz_dock(0, true);
        paint(&mut app, &mut terminal);
        assert_eq!(app.focus, Focus::Panel(PanelKind::Viz));
        let text = |app: &App| {
            app.viz_docks[0]
                .as_ref()
                .and_then(|panel| panel.prompt.as_ref())
                .map(|picker| picker.path.clone())
        };
        press(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        let offered = text(&app).expect("the art's text is asked for");

        app.open_memory_dock(false);
        paint(&mut app, &mut terminal);
        assert!(app.memory_dock.is_some(), "docked");
        assert_eq!(
            app.focus,
            Focus::Panel(PanelKind::Viz),
            "the prompt keeps them"
        );
        let height = app.prefs.memory_height;
        press(&mut app, KeyCode::Char('-'), KeyModifiers::NONE);
        let typed = text(&app).expect("the prompt is still up");
        assert!(typed != offered && typed.contains('-'), "{typed:?}");
        assert_eq!(app.prefs.memory_height, height, "the band was not resized");

        press(&mut app, KeyCode::Esc, KeyModifiers::NONE);
        assert_eq!(text(&app), None, "the prompt put away");
        app.close_memory_dock();
        app.focus = Focus::Panel(PanelKind::Viz);
        press(&mut app, KeyCode::Char('a'), KeyModifiers::NONE);
        let adding = |app: &App| app.viz_docks[0].as_ref().and_then(|panel| panel.adding);
        assert_eq!(adding(&app), Some(0), "the add list is up");

        app.open_memory_dock(false);
        paint(&mut app, &mut terminal);
        assert!(app.memory_dock.is_some());
        assert_eq!(
            app.focus,
            Focus::Panel(PanelKind::Viz),
            "the list keeps them"
        );
        press(&mut app, KeyCode::Down, KeyModifiers::NONE);
        assert_eq!(adding(&app), Some(1), "the list took the arrow");
        press(&mut app, KeyCode::Char('e'), KeyModifiers::NONE);
        assert_eq!(
            app.memory_dock.map(|dock| dock.edge),
            Some(Edge::Bottom),
            "e did not move the band"
        );
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
