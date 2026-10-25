use super::*;

impl App {
    /// A press on a replay timeline's scrollbar: off the thumb it brings the
    /// thumb under the pointer, and the drag that follows scrolls the
    /// strip. Returns true when it took the press.
    pub(super) fn press_timeline_bar(&mut self, x: u16, y: u16) -> bool {
        if let Some(pane) = self.timeline_bar_at(x, y) {
            let (grab, on_thumb) = self.grab_timeline_thumb(pane, x);
            let scene = self.panes[pane].scene;
            let strip = self.regions.panes[pane].timeline;
            if !on_thumb {
                self.scroll_timeline_to(scene, strip, x, grab);
            }
            self.pointer = Some(Pointer::TimelineBar { scene, strip, grab });
            return true;
        }
        false
    }

    /// A press on where a block of a replay's timeline starts takes hold of
    /// it: the drag that follows makes the block before it longer or
    /// shorter, a second per cell. Returns true when it took the press.
    pub(super) fn press_block_start(&mut self, x: u16, y: u16) -> bool {
        if let Some((_, id, block)) = self.timeline_block_start_at(x, y)
            && let Some(origin_length) = self
                .replays
                .get(&id)
                .and_then(|tab| tab.duration_of(block - 1))
        {
            self.pointer = Some(Pointer::BlockStart {
                scene: id,
                block,
                origin_x: x,
                origin_length,
            });
            self.status =
                format!("block {block} - drag left or right: a second a cell; let go to keep it");
            self.dirty_frame = true;
            return true;
        }
        false
    }

    /// A replay drag belongs to its captured scene and geometry, including
    /// its final release outside the timeline or across another surface.
    pub(super) fn handle_replay_pointer(&mut self, mouse: MouseEvent) -> bool {
        if !matches!(
            mouse.kind,
            MouseEventKind::Drag(MouseButton::Left) | MouseEventKind::Up(MouseButton::Left)
        ) {
            return false;
        }
        let released = matches!(mouse.kind, MouseEventKind::Up(MouseButton::Left));
        match self.pointer.clone() {
            Some(Pointer::TimelineBar { scene, strip, grab }) => {
                self.scroll_timeline_to(scene, strip, mouse.column, grab);
            }
            Some(Pointer::BlockStart {
                scene,
                block,
                origin_x,
                origin_length,
            }) => {
                if let Some(tab) = self.replays.get_mut(&scene) {
                    let cells = f64::from(mouse.column) - f64::from(origin_x);
                    tab.set_duration(block - 1, origin_length + cells);
                    let length = tab.duration_of(block - 1).unwrap_or(0.0);
                    self.status = format!(
                        "block {block} plays {} - let go to keep it",
                        super::super::replay::countdown_text(length)
                    );
                    self.dirty_frame = true;
                }
                if released {
                    self.save_replay_tab(scene);
                }
            }
            _ => return false,
        }
        if released {
            self.pointer = None;
        }
        self.follow_pointer_shape(mouse.column, mouse.row);
        true
    }

    pub(super) fn stop_replay_drag(&mut self) {
        match self.pointer {
            Some(Pointer::BlockStart { scene, .. }) => {
                self.save_replay_tab(scene);
                self.pointer = None;
            }
            Some(Pointer::TimelineBar { .. }) => self.pointer = None,
            _ => {}
        }
    }

    pub(super) fn replay_drag_active(&self, id: SceneId) -> bool {
        matches!(self.pointer,
            Some(Pointer::TimelineBar { scene, .. } | Pointer::BlockStart { scene, .. }) if scene == id)
    }
}

#[cfg(test)]
mod tests {
    mod replay_pointer {
        //! Tests for replay pointer.

        use crate::app::*;

        fn replay_app(directory: &std::path::Path, count: usize) -> (App, SceneId) {
            let mut app = crate::app::tests::support::app_over(directory);
            let sources: Vec<_> = (0..count)
                .map(|i| (i as f64 * 2.0, format!("$: s(\"sine\") // block {i}")))
                .collect();
            let borrowed: Vec<_> = sources
                .iter()
                .map(|(at, text)| (*at, text.as_str()))
                .collect();
            let tape =
                crate::app::tests::support::write_tape(directory, "2026-09-08T12-00-00", &borrowed);
            app.open_replay_tab(&tape);
            app.frame = Rect::new(0, 0, 120, 30);
            app.features.pixel_mouse = false;
            let id = app.current_replay().unwrap();
            layout(&mut app);
            (app, id)
        }

        /// This is the layout preparation done by every real App draw.
        fn layout(app: &mut App) -> Rect {
            let mut regions = view::regions(app.frame, false, 1, false, false, None, None);
            app.split_replay_timelines(&mut regions);
            app.regions = regions;
            app.regions.panes[0].timeline
        }

        fn mouse(app: &mut App, kind: MouseEventKind, x: u16, y: u16, modifiers: KeyModifiers) {
            let (column, row) = if app.features.pixel_mouse {
                let (width, height) = app.features.cell_pixels.unwrap();
                (x * width + width / 2, y * height + height / 2)
            } else {
                (x, y)
            };
            let mut event = Event::Mouse(MouseEvent {
                kind,
                column,
                row,
                modifiers,
            });
            app.pointer_to_cells(&mut event);
            app.handle_terminal_event(event).unwrap();
        }

        fn click(app: &mut App, x: u16, y: u16, modifiers: KeyModifiers) {
            mouse(
                app,
                MouseEventKind::Down(MouseButton::Left),
                x,
                y,
                modifiers,
            );
            mouse(app, MouseEventKind::Up(MouseButton::Left), x, y, modifiers);
        }

        #[test]
        fn timeline_pages_use_visible_blocks_keep_focus_and_never_start_playback() {
            let directory = tempfile::tempdir().unwrap();
            let (mut app, id) = replay_app(directory.path(), 40);
            app.focus_timeline();
            let mut page = ReplayTab::blocks_across(app.regions.panes[0].timeline.width);
            let press = |app: &mut App, code, kind| {
                app.handle_terminal_event(Event::Key(KeyEvent::new_with_kind(
                    code,
                    KeyModifiers::NONE,
                    kind,
                )))
                .unwrap();
            };
            press(&mut app, KeyCode::PageDown, KeyEventKind::Press);
            assert_eq!(app.replays[&id].selected, page);
            press(&mut app, KeyCode::PageDown, KeyEventKind::Repeat);
            assert_eq!(app.replays[&id].selected, page * 2);
            app.frame.width = 72;
            layout(&mut app);
            page = ReplayTab::blocks_across(app.regions.panes[0].timeline.width);
            let selected = app.replays[&id].selected;
            press(&mut app, KeyCode::PageUp, KeyEventKind::Press);
            assert_eq!(app.replays[&id].selected, selected - page);
            for _ in 0..40 {
                press(&mut app, KeyCode::PageDown, KeyEventKind::Repeat);
            }
            assert_eq!(app.replays[&id].selected, 39);
            assert!(app.editor().source().contains("block 39"));
            let caret = crate::editor::Selection::caret(ByteOffset(5));
            app.scenes
                .current_mut()
                .editor
                .set_selection(caret)
                .unwrap();
            press(&mut app, KeyCode::PageDown, KeyEventKind::Repeat);
            assert_eq!(
                app.editor().primary_selection(),
                caret,
                "paging past the last block must not reload its editor"
            );
            for _ in 0..40 {
                press(&mut app, KeyCode::PageUp, KeyEventKind::Repeat);
            }
            assert_eq!(app.replays[&id].selected, 0);
            assert!(app.editor().source().contains("block 0"));
            assert_eq!(app.focus, Focus::Timeline);
            assert!(app.replays[&id].run.is_none());
            assert!(app.pending_evaluation.is_none());
            assert!(app.inflight.is_empty());
        }

        #[test]
        fn playing_a_timeline_block_keeps_the_timeline_keyboard() {
            let directory = tempfile::tempdir().unwrap();
            let (mut app, id) = replay_app(directory.path(), 4);
            app.load_replay_block(id, 1);
            app.focus_timeline();
            for code in [KeyCode::Enter, KeyCode::Right, KeyCode::Enter] {
                app.handle_terminal_event(Event::Key(KeyEvent::new(code, KeyModifiers::NONE)))
                    .unwrap();
                assert_eq!(app.focus, Focus::Timeline);
            }
            assert_eq!(app.replays[&id].selected, 2);
            assert!(app.pending_evaluation.is_some() || !app.inflight.is_empty());
            app.handle_terminal_event(Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)))
                .unwrap();
            assert_eq!(app.focus, Focus::Editor);
        }

        #[test]
        fn focus_rotation_leaves_the_playing_timeline_for_the_other_panels() {
            let directory = tempfile::tempdir().unwrap();
            let (mut app, id) = replay_app(directory.path(), 4);
            app.open_viz_dock(0, false);
            app.open_set_panel(false);
            app.toggle_reference_browse();
            app.replays.get_mut(&id).unwrap().start(0, Instant::now());
            app.focus = Focus::Editor;
            let mut terminal =
                ratatui::Terminal::new(ratatui::backend::TestBackend::new(300, 40)).unwrap();
            terminal
                .draw(|frame| app.paint_into(frame, Instant::now()).unwrap())
                .unwrap();
            for _ in 0..2 {
                for focus in [
                    Focus::Timeline,
                    Focus::Panel(PanelKind::Viz),
                    Focus::Panel(PanelKind::Set),
                    Focus::Panel(PanelKind::Reference),
                    Focus::Editor,
                ] {
                    app.handle_terminal_event(Event::Key(KeyEvent::new(
                        KeyCode::F(10),
                        KeyModifiers::SHIFT,
                    )))
                    .unwrap();
                    assert_eq!(app.focus, focus);
                    assert!(app.replays[&id].run.is_some());
                }
            }
        }

        #[test]
        fn the_timeline_has_direct_keyboard_focus_and_esc_returns_to_the_editor() {
            let directory = tempfile::tempdir().unwrap();
            let (mut app, id) = replay_app(directory.path(), 4);
            app.load_replay_block(id, 2);
            let source = app.editor().source();
            app.focus = Focus::Editor;
            for _ in 0..2 {
                app.handle_terminal_event(Event::Key(KeyEvent::new(
                    KeyCode::Char('t'),
                    KeyModifiers::ALT,
                )))
                .unwrap();
                assert_eq!(app.focus, Focus::Timeline);
                assert_eq!(
                    app.replays[&id].selected, 2,
                    "focus must not select a different block"
                );
                assert_eq!(app.editor().source(), source);
            }
            app.handle_terminal_event(Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)))
                .unwrap();
            assert_eq!(app.focus, Focus::Editor);
            assert_eq!(app.current_replay(), Some(id));
            app.handle_terminal_event(Event::Key(KeyEvent::new(
                KeyCode::F(10),
                KeyModifiers::SHIFT,
            )))
            .unwrap();
            assert_eq!(app.focus, Focus::Timeline);
            assert_eq!(
                app.focus_rotation_flash_room(),
                Some(app.regions.panes[0].timeline)
            );
            app.handle_terminal_event(Event::Key(KeyEvent::new(
                KeyCode::Right,
                KeyModifiers::NONE,
            )))
            .unwrap();
            assert_eq!(app.replays[&id].selected, 3);
            assert!(
                app.keybind_rows()
                    .iter()
                    .any(|row| row.action == "focus tape timeline"
                        && row.chord == crate::keybinds::shortcut_label("Alt+T"))
            );

            // iTerm2's Option as Normal sends a dagger, not Alt+T. In a
            // tape the composed key still focuses the timeline.
            app.focus = Focus::Editor;
            app.keybinds.set_reach(crate::keybinds::Reach {
                enhanced: false,
                terminal: "iTerm2".into(),
            });
            app.handle_terminal_event(Event::KeyWithText {
                key: KeyEvent::new(KeyCode::Char('t'), KeyModifiers::ALT),
                text: "†".into(),
            })
            .unwrap();
            assert_eq!(
                app.focus,
                Focus::Timeline,
                "a composed Option+T focuses the tape"
            );

            // Without the keyboard protocol the terminal sends only the
            // dagger, and the profile still knows what it stood for.
            app.focus = Focus::Editor;
            app.handle_terminal_event(Event::Key(KeyEvent::new(
                KeyCode::Char('†'),
                KeyModifiers::NONE,
            )))
            .unwrap();
            assert_eq!(app.focus, Focus::Timeline, "a bare dagger focuses the tape");

            // A learnt chord takes the place of Alt+T, here and in the hint.
            let chord = crate::keybinds::KeyCombo::parse("ctrl+shift+f8").unwrap();
            app.learn_keybind(BindAction::FocusTimeline, Some(chord));
            let press = |app: &mut App, code, modifiers| {
                app.focus = Focus::Editor;
                app.handle_terminal_event(Event::Key(KeyEvent::new(code, modifiers)))
                    .unwrap();
                app.focus
            };
            let source = app.editor().source();
            assert_eq!(
                press(&mut app, KeyCode::Char('t'), KeyModifiers::ALT),
                Focus::Editor
            );
            assert_eq!(
                app.editor().source(),
                source,
                "the dead chord types nothing"
            );
            assert_eq!(
                press(
                    &mut app,
                    KeyCode::F(8),
                    KeyModifiers::CONTROL | KeyModifiers::SHIFT
                ),
                Focus::Timeline
            );
            assert!(app.keybind_rows().iter().any(|row| {
                row.action == "focus tape timeline" && row.chord == chord.hint() && row.learnt
            }));
            app.learn_keybind(BindAction::FocusTimeline, None);
            assert_eq!(
                press(&mut app, KeyCode::Char('t'), KeyModifiers::ALT),
                Focus::Timeline
            );
            app.focus = Focus::Editor;
            app.regions.panes[0].timeline = Rect::default();
            app.rotate_panel_focus();
            assert_ne!(
                app.focus,
                Focus::Timeline,
                "hidden timelines are not focus targets"
            );
        }

        #[test]
        fn replay_scrollbar_reaches_both_ends_inside_its_rail_and_survives_redraws() {
            let directory = tempfile::tempdir().unwrap();
            let (mut app, id) = replay_app(directory.path(), 40);
            let strip = layout(&mut app);
            let (track, _, thumb) = app.replays[&id].bar_geometry(strip.width).unwrap();
            let grab = thumb - 1;
            let left = strip.x + grab;
            let right = strip.x + track - thumb + grab;
            let middle = (left + right) / 2;
            let y = ReplayTab::scrollbar_row(strip);
            let most = 40 - ReplayTab::blocks_across(strip.width);
            assert!(
                right < strip.x + track,
                "both endpoints stay on the drawn scrollbar"
            );
            mouse(
                &mut app,
                MouseEventKind::Down(MouseButton::Left),
                left,
                y,
                KeyModifiers::NONE,
            );
            assert_eq!(app.replays[&id].scroll, 0, "grabbing the thumb never jumps");
            mouse(
                &mut app,
                MouseEventKind::Drag(MouseButton::Left),
                middle,
                0,
                KeyModifiers::NONE,
            );
            let halfway = app.replays[&id].scroll;
            assert!(halfway > 0 && halfway < most);
            layout(&mut app);
            assert_eq!(
                app.replays[&id].scroll, halfway,
                "redraw must not reveal the selected first block"
            );
            mouse(
                &mut app,
                MouseEventKind::Drag(MouseButton::Left),
                right,
                29,
                KeyModifiers::NONE,
            );
            layout(&mut app);
            assert_eq!(app.replays[&id].scroll, most);
            mouse(
                &mut app,
                MouseEventKind::Drag(MouseButton::Left),
                u16::MAX,
                0,
                KeyModifiers::NONE,
            );
            assert_eq!(app.replays[&id].scroll, most);
            mouse(
                &mut app,
                MouseEventKind::Drag(MouseButton::Left),
                middle,
                0,
                KeyModifiers::NONE,
            );
            assert_eq!(
                app.replays[&id].scroll, halfway,
                "returning from outside uses the same absolute position"
            );
            mouse(
                &mut app,
                MouseEventKind::Up(MouseButton::Left),
                left,
                29,
                KeyModifiers::NONE,
            );
            assert_eq!(
                app.replays[&id].scroll, 0,
                "the final release applies even outside the timeline"
            );
            assert!(app.pointer.is_none());
            mouse(
                &mut app,
                MouseEventKind::Down(MouseButton::Left),
                left,
                y,
                KeyModifiers::NONE,
            );
            mouse(
                &mut app,
                MouseEventKind::Up(MouseButton::Left),
                right,
                0,
                KeyModifiers::NONE,
            );
            layout(&mut app);
            assert_eq!(
                app.replays[&id].scroll, most,
                "a fast gesture can have no intermediate drag report"
            );
            assert_eq!(app.replays[&id].selected, 0);
            app.focus_timeline();
            app.handle_timeline_key(KeyCode::End).unwrap();
            assert_eq!(app.replays[&id].selected, 39);
            assert_eq!(app.replays[&id].scroll, most);
            app.handle_timeline_key(KeyCode::Home).unwrap();
            assert_eq!(
                app.replays[&id].scroll, 0,
                "explicit keyboard selection still reveals its block"
            );
        }

        /// The strip runs left to right, so a swipe follows the fingers.
        #[test]
        fn a_sideways_swipe_walks_the_tape_the_way_the_fingers_went() {
            let directory = tempfile::tempdir().unwrap();
            let (mut app, id) = replay_app(directory.path(), 40);
            let strip = layout(&mut app);
            let (x, y) = (strip.x + 4, strip.y + 2);
            app.replays.get_mut(&id).unwrap().scroll = 8;

            // Fingers right to left: the strip slides left, later blocks show.
            mouse(
                &mut app,
                MouseEventKind::ScrollRight,
                x,
                y,
                KeyModifiers::NONE,
            );
            assert_eq!(app.replays[&id].scroll, 9, "right-to-left goes forward");
            mouse(
                &mut app,
                MouseEventKind::ScrollLeft,
                x,
                y,
                KeyModifiers::NONE,
            );
            assert_eq!(app.replays[&id].scroll, 8, "and left-to-right goes back");

            // A wheel keeps the list's convention: down is later.
            mouse(
                &mut app,
                MouseEventKind::ScrollDown,
                x,
                y,
                KeyModifiers::NONE,
            );
            assert_eq!(app.replays[&id].scroll, 9);
            mouse(&mut app, MouseEventKind::ScrollUp, x, y, KeyModifiers::NONE);
            assert_eq!(app.replays[&id].scroll, 8);
        }

        /// A press on a block chooses that block and moves nothing else.
        /// Only the bar under the strip pans.
        #[test]
        fn pressing_a_block_chooses_it_and_does_not_slide_the_strip() {
            let directory = tempfile::tempdir().unwrap();
            let (mut app, id) = replay_app(directory.path(), 40);
            let tab = app.replays.get_mut(&id).unwrap();
            tab.scroll = 8;
            tab.start(2, Instant::now());
            let run = tab.run.unwrap();
            let times: Vec<_> = tab.events.iter().map(|event| event.at).collect();
            let path = tab.path.clone();
            let original = std::fs::read(&path).unwrap();
            let strip = layout(&mut app);
            let x = strip.x + 2 * crate::replay::BLOCK_WIDTH + 3;
            let y = strip.y + 2;
            mouse(
                &mut app,
                MouseEventKind::Down(MouseButton::Left),
                x,
                y,
                KeyModifiers::NONE,
            );
            assert_eq!(app.replays[&id].selected, 10, "the block under the press");
            layout(&mut app);
            assert_eq!(app.replays[&id].scroll, 8);
            // A wandering hand, all the way across the screen and back: the
            // strip does not follow it anywhere.
            for position in [x - 24, u16::MAX, x, strip.x] {
                mouse(
                    &mut app,
                    MouseEventKind::Drag(MouseButton::Left),
                    position,
                    29,
                    KeyModifiers::NONE,
                );
                layout(&mut app);
                assert_eq!(
                    app.replays[&id].scroll, 8,
                    "a drag from a block moved the strip"
                );
            }
            mouse(
                &mut app,
                MouseEventKind::Up(MouseButton::Left),
                x - 24,
                0,
                KeyModifiers::NONE,
            );
            assert_eq!(app.replays[&id].scroll, 8);
            // And nothing about the tape was touched: not the times, not the
            // run, not the file.
            assert_eq!(
                app.replays[&id]
                    .events
                    .iter()
                    .map(|event| event.at)
                    .collect::<Vec<_>>(),
                times
            );
            assert_eq!(app.replays[&id].run.unwrap().event, run.event);
            assert_eq!(app.replays[&id].run.unwrap().started, run.started);
            assert!(app.pending_evaluation.is_none() && app.inflight.is_empty());
            assert_eq!(std::fs::read(path).unwrap(), original);
        }

        /// A click on a block chooses it, on every row of it and whatever is
        /// held down. Enter is what plays from it. No modifier plays, because a
        /// terminal can claim Ctrl or Shift before the studio sees it.
        #[test]
        fn a_click_on_a_block_chooses_it_on_every_row_and_plays_nothing() {
            for (modifiers, pixels) in [
                (KeyModifiers::CONTROL, false),
                (KeyModifiers::SHIFT, false),
                (KeyModifiers::NONE, true),
            ] {
                let directory = tempfile::tempdir().unwrap();
                let (mut app, id) = replay_app(directory.path(), 5);
                app.features.pixel_mouse = pixels;
                app.features.cell_pixels = Some((10, 20));
                app.ui_settings.quantise = settings::Quantise::Phrase(4);
                let times: Vec<_> = app.replays[&id]
                    .events
                    .iter()
                    .map(|event| event.at)
                    .collect();
                for offset in 0..5 {
                    app.load_replay_block(id, 0);
                    app.replays.get_mut(&id).unwrap().stop();
                    app.pending_evaluation = None;
                    app.inflight.clear();
                    let strip = layout(&mut app);
                    let x = strip.x + 2 * crate::replay::BLOCK_WIDTH + 2;
                    click(&mut app, x, strip.y + offset, modifiers);
                    // Every row of a block chooses it, except the ruler row.
                    // The ruler row is the handle that drags the block's start.
                    if strip.y + offset != ReplayTab::ruler_row(strip) {
                        assert_eq!(
                            app.replays[&id].selected, 2,
                            "row {offset} did not choose the block under it"
                        );
                    }
                    assert!(
                        app.replays[&id].run.is_none(),
                        "row {offset} played from it"
                    );
                    assert!(
                        app.pending_evaluation.is_none() && app.inflight.is_empty(),
                        "row {offset} sent a score"
                    );
                    assert_eq!(
                        app.replays[&id]
                            .events
                            .iter()
                            .map(|event| event.at)
                            .collect::<Vec<_>>(),
                        times,
                        "and moved nothing"
                    );
                }
            }
        }

        #[test]
        fn ruler_drag_applies_the_release_position_before_saving() {
            let directory = tempfile::tempdir().unwrap();
            let (mut app, id) = replay_app(directory.path(), 5);
            let strip = layout(&mut app);
            let x = strip.x + crate::replay::BLOCK_WIDTH + 2;
            mouse(
                &mut app,
                MouseEventKind::Down(MouseButton::Left),
                x,
                ReplayTab::ruler_row(strip),
                KeyModifiers::NONE,
            );
            mouse(
                &mut app,
                MouseEventKind::Up(MouseButton::Left),
                x + 4,
                0,
                KeyModifiers::NONE,
            );
            assert_eq!(app.replays[&id].duration_of(0), Some(6.0));
            assert!(!app.replays[&id].dirty);
            assert_eq!(
                ReplayTab::open(&app.replays[&id].path)
                    .unwrap()
                    .duration_of(0),
                Some(6.0)
            );
        }

        #[test]
        fn replay_gaps_and_totals_do_not_select_or_resize_blocks() {
            let directory = tempfile::tempdir().unwrap();
            let (mut app, id) = replay_app(directory.path(), 40);
            let strip = layout(&mut app);
            let selected = app.replays[&id].selected;
            let (track, _, _) = app.replays[&id].bar_geometry(strip.width).unwrap();
            for (x, y) in [
                (strip.x + 23, strip.y + 2),
                (strip.x + track + 1, ReplayTab::scrollbar_row(strip)),
            ] {
                assert!(app.timeline_block_at(x, y).is_none());
                assert!(app.timeline_bar_at(x, y).is_none());
                click(&mut app, x, y, KeyModifiers::CONTROL);
                assert_eq!(app.replays[&id].selected, selected);
                assert!(app.replays[&id].run.is_none());
            }
        }
    }
}
