//! The editor's panes: splitting the editor into two panes and closing the
//! split, moving the caret between panes, keeping each pane on a scene that
//! exists, and saving and restoring the pane layout in the set. Also covers
//! scrolling a pane with the wheel, the vertical and horizontal scrollbars and
//! the minimap, plus the zen, word wrap and line-number toggles that change how
//! a pane is drawn.

use super::*;

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(super) struct HorizontalScrollbarGrab {
    offset: usize,
    column: u16,
    left: usize,
}

/// One editor pane on screen: which scene it shows, and the map of the
/// last frame it drew, for routing the pointer.
pub(super) struct Pane {
    pub(super) scene: SceneId,
    pub(super) last_map: Option<ScreenMap>,
}

impl App {
    /// The pane a screen point is over, if the layout still agrees with the
    /// panes there are. `regions` is refreshed only by a draw, and the event
    /// loop drains up to sixty-four events before it draws, so a split
    /// closed during this drain leaves `pane_at` naming a pane that has
    /// already gone - and indexing `panes` with that name panics.
    pub(super) fn pane_at(&self, x: u16, y: u16) -> Option<usize> {
        self.regions
            .pane_at(x, y)
            .filter(|&index| index < self.panes.len())
    }

    pub(super) fn focused_map(&self) -> Option<&ScreenMap> {
        self.panes[self.focused].last_map.as_ref()
    }

    /// Every pane's map is stale once the text, the layout or the viewport
    /// changed; the next frame rebuilds them.
    pub(super) fn invalidate_maps(&mut self) {
        for pane in &mut self.panes {
            pane.last_map = None;
        }
    }

    /// Give the caret to a pane. The set's current scene follows it.
    pub(super) fn focus_pane(&mut self, index: usize) {
        if index >= self.panes.len() {
            return;
        }
        // A slider armed in one pane must not catch arrows meant for
        // another.
        self.armed_slider = None;
        // Which pane has the caret is part of how the set was left, so a
        // hop owes the set file a write - debounced, because a hand going
        // back and forth between two panes is one thought, not six.
        if self.focused != index {
            self.save_manifest_soon();
        }
        self.focused = index;
        let scene = self.panes[index].scene;
        if let Some(position) = self.scenes.index_of(scene) {
            self.scenes.select(position);
        }
        self.close_smart_action_on_scene_change();
        self.note_scene_visit(scene);
        self.strip_mode = SceneStripMode::Idle;
        self.pointer = None;
        self.invalidate_maps();
        self.rebuild_slider_spans();
        self.lint_pending = true;
        self.dirty_frame = true;
    }

    /// A press on a pane's title, which only focuses the pane. Returns true
    /// when it took the press.
    pub(super) fn click_pane_title(&mut self, x: u16, y: u16) -> bool {
        if let Some(pane) = self.pane_at(x, y)
            && within(self.regions.panes[pane].title, x, y)
        {
            // A title focuses its pane and nothing else. Claim it
            // before an unfocused pane forwards its first click to
            // the editor, where Shift extends a selection.
            if pane != self.focused {
                self.focus_pane(pane);
            }
            self.armed_slider = None;
            self.pointer = Some(Pointer::Panel);
            self.dirty_frame = true;
            return true;
        }
        false
    }

    /// A press on a pane that does not hold the caret gives it the caret,
    /// and - off its sliders and its timeline - goes on to its editor.
    /// Returns true when the press went to the editor. Returns false when
    /// it was not on such a pane, or was on one of its sliders or its
    /// timeline: the pane has the caret then, and the press is left to the
    /// checks that follow.
    pub(super) fn click_unfocused_pane(
        &mut self,
        mouse: MouseEvent,
        x: u16,
        y: u16,
    ) -> Result<bool, RuntimeError> {
        if let Some(index) = self.pane_at(x, y)
            && index != self.focused
        {
            // The pane's map from the last frame still describes
            // what was clicked: keep it through the focus change,
            // which throws every map away, so this click lands the
            // caret rather than only the focus.
            let map = self.panes[index].last_map.take();
            self.focus_pane(index);
            self.panes[index].last_map = map;
            if self.slider_at(x, y).is_none() && !within(self.regions.panes[index].timeline, x, y) {
                self.forward_mouse_to_editor(mouse)?;
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// Scroll a pane that does not hold the caret: its scene's editor
    /// moves by the wheel's rows, and its map is built again next frame.
    pub(super) fn scroll_pane(&mut self, index: usize, direction: f64) {
        let scene = self.panes[index].scene;
        let rows = super::super::editor::DEFAULT_WHEEL_ROWS;
        if let Some(scene) = self.scenes.get_mut(scene) {
            let mut viewport = scene.editor.viewport();
            viewport.top_row = if direction > 0.0 {
                viewport.top_row.saturating_sub(rows)
            } else {
                viewport.top_row.saturating_add(rows)
            };
            scene.editor.set_viewport(viewport);
        }
        self.panes[index].last_map = None;
        self.dirty_frame = true;
    }

    /// F10: the caret to the other pane. A click does the same.
    pub(super) fn hop_pane(&mut self) {
        if self.panes.len() < 2 {
            self.status = format!(
                "one pane - {} splits the editor",
                self.keybinds.hint(BindAction::Split)
            );
            self.dirty_frame = true;
            return;
        }
        let other = 1 - self.focused;
        self.focus_pane(other);
    }

    /// Ctrl+E: open the second pane on the next scene, or close it again.
    /// A tape has the studio to itself, so under one the second pane is a
    /// second window on the tape.
    pub(super) fn toggle_split(&mut self) {
        if self.panes.len() > 1 {
            self.close_split();
            return;
        }
        if self.replay_view.is_some() {
            self.status = format!(
                "the tape has the studio to itself - {} goes back to the set",
                self.shortcut_or_menu(BindAction::CloseScene, "Scene > Close scene")
            );
            self.dirty_frame = true;
            return;
        }
        if self.panes.len() == 1 {
            let len = self.scenes.len();
            if len < 2 {
                // One scene and a wish for two panes: make the second scene
                // right here, the way Ctrl+N then Ctrl+E would.
                match self.scenes.create("") {
                    Ok(id) => {
                        self.persist_manifest();
                        self.panes.push(Pane {
                            scene: id,
                            last_map: None,
                        });
                        self.focus_pane(1);
                        self.save_manifest_soon();
                        self.status = format!(
                            "split onto a new scene - {} hops panes, {} names it, {} closes the split",
                            self.shortcut_or_menu(BindAction::HopPane, "View > Switch pane"),
                            self.shortcut_or_menu(BindAction::RenameScene, "Scene > Rename scene"),
                            self.shortcut_or_menu(BindAction::Split, "View > Close the split")
                        );
                    }
                    Err(error) => self.status = format!("split: {error}"),
                }
                self.dirty_frame = true;
                return;
            }
            let next = (self.scenes.current_index() + 1) % len;
            let scene = self.scenes.scenes()[next].id;
            self.panes.push(Pane {
                scene,
                last_map: None,
            });
            self.focus_pane(1);
            self.save_manifest_soon();
            self.status = format!(
                "split - {} hops between panes, {} closes the split",
                self.shortcut_or_menu(BindAction::HopPane, "View > Switch pane"),
                self.shortcut_or_menu(BindAction::Split, "View > Close the split")
            );
        }
    }

    /// Ctrl+E again: back to one pane. The right pane's scene stays in the
    /// set and keeps playing if it was. (Hopping panes is ^⇧E and F10.)
    pub(super) fn close_split(&mut self) {
        if self.panes.len() < 2 {
            return;
        }
        self.panes.truncate(1);
        if self.playing_pane == Some(1) {
            self.playing_pane = None;
        }
        self.focus_pane(0);
        self.save_manifest_soon();
        self.status = "single pane".into();
    }

    /// After the set changed shape, keep every pane on a scene that exists
    /// and never show one scene twice.
    pub(super) fn reconcile_panes(&mut self) {
        self.close_smart_action_on_scene_change();
        let fallback = self.scenes.current().id;
        for pane in &mut self.panes {
            if self.scenes.get(pane.scene).is_none() {
                pane.scene = fallback;
            }
        }
        if self.panes.len() == 2 && self.panes[0].scene == self.panes[1].scene {
            self.panes.truncate(1);
            if self.playing_pane == Some(1) {
                self.playing_pane = None;
            }
            self.focused = 0;
        }
        self.panes[self.focused].scene = fallback;
        self.invalidate_maps();
    }

    /// Tell the set how the editor is laid out. A replay has the panes to
    /// itself and its stash holds the set's, so the set is not told about
    /// a layout that is not its own.
    pub(super) fn remember_panes(&mut self) {
        if self.replay_view.is_some() {
            return;
        }
        let panes: Vec<SceneId> = self.panes.iter().map(|pane| pane.scene).collect();
        self.scenes.set_panes(&panes, self.focused);
    }

    /// Open the panes the set was left in: the scores, left to right, and
    /// the caret in the pane that had it.
    ///
    /// Silent about a set that was left in one pane, which is most of
    /// them, and about a layout naming a score that has gone - the set
    /// then opens the ordinary way rather than half split.
    pub(super) fn restore_panes(&mut self) {
        if self.replay_view.is_some() {
            return;
        }
        let (scenes, focused) = self.scenes.pane_layout();
        if scenes.len() < 2 {
            return;
        }
        self.panes = scenes
            .into_iter()
            .map(|scene| Pane {
                scene,
                last_map: None,
            })
            .collect();
        self.playing_pane = None;
        // The caret first, so the set's current scene is the focused
        // pane's before anything reconciles the two.
        self.focused = focused.min(self.panes.len() - 1);
        self.focus_pane(self.focused);
        self.reconcile_panes();
    }

    pub(super) fn toggle_zen(&mut self) {
        self.ui_settings.zen = !self.ui_settings.zen;
        self.apply_zen();
        // F11/^K can be pressed from a panel. The stage that appears holds
        // only the editor, so the caret takes the keyboard here; otherwise
        // only the mouse could give it back to the score.
        //
        // Except from the settings sheet, which is modal and keeps its
        // keys: taking them here would leave the sheet open with nothing
        // answering it, which is the whole thing its hold exists to stop.
        // Esc is how that sheet is left, in zen as anywhere else.
        if self.ui_settings.zen && !self.settings_hold_the_keys() {
            self.focus = Focus::Editor;
        }
    }

    /// The line numbers on or off, for every scene: a setting, remembered
    /// in the preferences like the switches in the sheet.
    pub(super) fn toggle_line_numbers(&mut self) {
        self.ui_settings.line_numbers = !self.ui_settings.line_numbers;
        self.ui_settings.apply();
        self.prefs.set_ui_settings(&self.ui_settings);
        self.save_prefs_soon();
        self.invalidate_maps();
        self.status = if self.ui_settings.line_numbers {
            "line numbers".into()
        } else {
            "no line numbers".into()
        };
        self.dirty_frame = true;
    }

    /// A press or a drag on the scrollbar: the page goes where the pointer
    /// is along the bar, as on the minimap.
    pub(super) fn press_scrollbar(&mut self, y: u16) {
        let bar = self.regions.panes[self.focused].scrollbar;
        if bar.height < 2 {
            return;
        }
        let (_, _, total) = self.editor().scroll_extent();
        let furthest = total.saturating_sub(1);
        let along = usize::from(y.saturating_sub(bar.y).min(bar.height - 1));
        let top =
            (along * furthest + usize::from(bar.height - 1) / 2) / usize::from(bar.height - 1);
        let mut viewport = self.editor().viewport();
        viewport.top_row = top.min(furthest);
        self.editor_mut().set_viewport(viewport);
        self.invalidate_maps();
        self.dirty_frame = true;
    }

    pub(super) fn horizontal_scrollbar_at(&self, x: u16, y: u16) -> Option<usize> {
        if !self.ui_settings.show_scrollbars {
            return None;
        }
        self.regions
            .panes
            .iter()
            .enumerate()
            .find_map(|(pane, region)| {
                (!region.horizontal_scrollbar.is_empty()
                    && region.horizontal_scrollbar.contains((x, y).into()))
                .then_some(pane)
            })
    }

    pub(super) fn vertical_scrollbar_at(&self, pane: usize, x: u16, y: u16) -> bool {
        if !self.ui_settings.show_scrollbars {
            return false;
        }
        let Some(region) = self.regions.panes.get(pane) else {
            return false;
        };
        let Some(scene) = self
            .panes
            .get(pane)
            .and_then(|pane| self.scenes.get(pane.scene))
        else {
            return false;
        };
        let (_, page, total) = scene.editor.scroll_extent();
        total > page && within(region.scrollbar, x, y)
    }

    /// A press on the focused pane's vertical scrollbar brings the page to
    /// where the pointer is along the bar and grabs the bar, and the drag
    /// that follows keeps it. Returns true when it took the press.
    pub(super) fn press_vertical_scrollbar(&mut self, x: u16, y: u16) -> bool {
        if self.vertical_scrollbar_at(self.focused, x, y) {
            self.press_scrollbar(y);
            self.pointer = Some(Pointer::Scrollbar);
            return true;
        }
        false
    }

    /// A press on a pane's horizontal scrollbar focuses the pane and grabs
    /// the bar, and the drag that follows keeps it. Returns true when it
    /// took the press.
    pub(super) fn press_horizontal_scrollbar(&mut self, x: u16, y: u16) -> bool {
        if let Some(pane) = self.horizontal_scrollbar_at(x, y) {
            if pane != self.focused {
                self.focus_pane(pane);
            }
            let grab = self.grab_horizontal_scrollbar(pane, x);
            self.pointer = Some(Pointer::HorizontalScrollbar { pane, grab });
            return true;
        }
        false
    }

    pub(super) fn grab_horizontal_scrollbar(
        &mut self,
        pane: usize,
        x: u16,
    ) -> HorizontalScrollbarGrab {
        let Some(region) = self
            .regions
            .panes
            .get(pane)
            .map(|region| region.horizontal_scrollbar)
        else {
            return HorizontalScrollbarGrab::default();
        };
        let scene = self.panes[pane].scene;
        let Some(extent) = self
            .scenes
            .get(scene)
            .and_then(|scene| scene.editor.horizontal_scroll_extent())
        else {
            return HorizontalScrollbarGrab::default();
        };
        let (start, length) = view::horizontal_scrollbar_thumb(extent, region.width);
        let along = usize::from(
            x.saturating_sub(region.x)
                .min(region.width.saturating_sub(1)),
        );
        let on_thumb = along >= start && along < start + length;
        let offset = if on_thumb { along - start } else { length / 2 };
        let left = if on_thumb {
            extent.0
        } else {
            view::horizontal_scrollbar_left(extent, region.width, along.saturating_sub(offset))
        };
        let grab = HorizontalScrollbarGrab {
            offset,
            column: x,
            left,
        };
        if !on_thumb {
            self.drag_horizontal_scrollbar(pane, x, grab);
        }
        grab
    }

    /// A horizontal scrollbar drag keeps the whole gesture, wherever the
    /// pointer goes. Returns true when the drag took the event.
    pub(super) fn handle_horizontal_scrollbar_pointer(
        &mut self,
        mouse: MouseEvent,
        x: u16,
        y: u16,
    ) -> bool {
        // Capture the entire gesture, including motion above another panel
        // and a release that is the terminal's only final position report.
        if let Some(Pointer::HorizontalScrollbar { pane, grab }) = self.pointer.clone()
            && matches!(
                mouse.kind,
                MouseEventKind::Drag(MouseButton::Left) | MouseEventKind::Up(MouseButton::Left)
            )
        {
            self.drag_horizontal_scrollbar(pane, x, grab);
            if matches!(mouse.kind, MouseEventKind::Up(_)) {
                self.pointer = None;
            }
            self.follow_pointer_shape(x, y);
            return true;
        }
        false
    }

    pub(super) fn drag_horizontal_scrollbar(
        &mut self,
        pane: usize,
        x: u16,
        grab: HorizontalScrollbarGrab,
    ) {
        let Some(region) = self
            .regions
            .panes
            .get(pane)
            .map(|region| region.horizontal_scrollbar)
        else {
            return;
        };
        let scene = self.panes[pane].scene;
        let Some(extent) = self
            .scenes
            .get(scene)
            .and_then(|scene| scene.editor.horizontal_scroll_extent())
        else {
            return;
        };
        let along = usize::from(
            x.saturating_sub(region.x)
                .min(region.width.saturating_sub(1)),
        );
        // Re-inverting the rounded thumb cell on a stationary press/release
        // can jump several document columns. Retain the exact initial offset.
        let left = if x == grab.column {
            grab.left.min(extent.2.saturating_sub(extent.1))
        } else {
            view::horizontal_scrollbar_left(extent, region.width, along.saturating_sub(grab.offset))
        };
        if let Some(scene) = self.scenes.get_mut(scene) {
            let mut viewport = scene.editor.viewport();
            if viewport.left_column == left {
                return;
            }
            viewport.left_column = left;
            scene.editor.set_viewport(viewport);
        }
        self.invalidate_maps();
        self.dirty_frame = true;
    }

    /// Word wrap on or off, for every scene: a setting, remembered in the
    /// preferences like the switches in the sheet.
    pub(super) fn toggle_wrap(&mut self) {
        self.ui_settings.wrap = !self.ui_settings.wrap;
        self.prefs.set_ui_settings(&self.ui_settings);
        self.save_prefs_soon();
        self.invalidate_maps();
        let chord = self.keybinds.hint(BindAction::Wrap);
        self.status = if self.ui_settings.wrap {
            format!("word wrap - long lines continue on the next row · {chord} turns it off")
        } else {
            format!("no wrap - long lines run off the edge and scroll · {chord} wraps them")
        };
        self.dirty_frame = true;
    }

    /// Flip one of the menu bar, header and footer switches and remember it,
    /// as the settings sheet's Look rows do. Returns whether the row is now
    /// shown.
    fn toggle_chrome_row(&mut self, row: fn(&mut UiSettings) -> &mut bool) -> bool {
        let switch = row(&mut self.ui_settings);
        *switch = !*switch;
        let shown = *switch;
        self.prefs.set_ui_settings(&self.ui_settings);
        self.save_prefs_soon();
        self.invalidate_maps();
        self.settle_menu();
        self.dirty_frame = true;
        shown
    }

    /// The File / Edit menu bar. With it hidden the Settings chord is the
    /// way back to the switch, so the status names that chord, or the
    /// preferences file when Settings has none.
    pub(super) fn toggle_show_menu(&mut self) {
        let shown = self.toggle_chrome_row(|settings| &mut settings.show_menu);
        self.refresh_editor_shortcut_hints();
        self.status = if shown {
            "menu bar shown".into()
        } else if let Some(chord) = self.keybinds.hint_binding(BindAction::Settings) {
            format!(
                "menu bar hidden - {} opens settings to bring it back",
                chord.hint()
            )
        } else {
            format!(
                "menu bar hidden - settings has no available chord; show_menu in {} brings it back",
                super::super::prefs::PREFS_FILE_NAME
            )
        };
    }

    /// The rustel PLAYING tempo line.
    pub(super) fn toggle_show_header(&mut self) {
        self.status = if self.toggle_chrome_row(|settings| &mut settings.show_header) {
            "header shown".into()
        } else {
            "header hidden".into()
        };
    }

    /// The footer's meter, orbits and device chips. Off still keeps the
    /// notices and the status line.
    pub(super) fn toggle_show_footer(&mut self) {
        self.status = if self.toggle_chrome_row(|settings| &mut settings.show_footer) {
            "footer shown".into()
        } else {
            "footer hidden - the status line stays".into()
        };
    }

    /// Redraw for whatever zen is now, wherever it was changed - the F11 key
    /// or the settings sheet.
    pub(super) fn apply_zen(&mut self) {
        if self.ui_settings.zen {
            // The visuals docks are refused outright once zen is on - see
            // `toggle_viz_dock` - and a dock already open when zen turns
            // on has nowhere to stand either: closing it here is what
            // keeps it from becoming an invisible panel still holding the
            // keyboard, docked furniture zen no longer draws a stage for.
            for index in 0..self.viz_docks.len() {
                if self.viz_docks[index].is_some() {
                    self.close_viz_dock(index);
                }
            }
        }
        self.invalidate_maps();
        self.status = if self.ui_settings.zen {
            format!(
                "zen mode - {} restores the stage and status",
                self.keybinds.hint(BindAction::Zen)
            )
        } else {
            "stage restored".into()
        };
        self.dirty_frame = true;
    }

    /// A press on the minimap: inside the viewport band it grabs the band
    /// like a scrollbar thumb; anywhere else it jumps there first, and the
    /// drag that follows keeps that spot under the pointer.
    pub(super) fn press_minimap(&mut self, y: u16) {
        let minimap = self.regions.panes[self.focused].minimap;
        let row = self.scenes.current().minimap.line_at(minimap, y);
        let top = self.editor().viewport().top_row;
        let page_rows = self.editor().viewport().page_rows;
        let grab_rows = if row >= top && row < top.saturating_add(page_rows) {
            row - top
        } else {
            page_rows / 2
        };
        self.pointer = Some(Pointer::Minimap { grab_rows });
        self.drag_minimap(y, grab_rows);
    }

    pub(super) fn drag_minimap(&mut self, y: u16, grab_rows: usize) {
        let minimap = self.regions.panes[self.focused].minimap;
        let row = self.scenes.current().minimap.line_at(minimap, y);
        self.scroll_to_row(row.saturating_sub(grab_rows));
    }

    fn scroll_to_row(&mut self, row: usize) {
        let mut viewport = self.editor().viewport();
        viewport.top_row = row;
        self.editor_mut().set_viewport(viewport);
        self.invalidate_maps();
        self.dirty_frame = true;
    }
}

pub(super) fn reserve_horizontal_scrollbar(
    editor: &mut Editor,
    region: &mut view::PaneRegion,
    numbered: bool,
    show_scrollbars: bool,
) {
    // Measure without resizing: temporarily adding a row on every frame
    // reveals the caret again and undoes the user's mouse scroll.
    let provisional = view::source_grid(editor, region.editor, numbered);
    if show_scrollbars
        && editor
            .horizontal_scroll_extent_for(usize::from(provisional.width))
            .is_some()
        && region.editor.height > 1
    {
        region.horizontal_scrollbar = Rect::new(
            region.editor.x,
            region.editor.bottom() - 1,
            region.editor.width,
            1,
        );
        region.editor.height -= 1;
    }
    let grid = view::source_grid(editor, region.editor, numbered);
    editor.set_view_size(usize::from(grid.width), usize::from(grid.height));
}

#[cfg(test)]
mod tests {
    mod pane_launch {
        //! Tests for pane launch.

        use crate::app::*;
        use crate::editor::Selection;

        fn split_app(directory: &std::path::Path) -> App {
            let mut app = crate::app::tests::support::app_over(directory);
            app.toggle_split();
            app.scenes.current_mut().editor = Editor::new("$: s(\"hh*4\")\n").unwrap();
            app.frame = Rect::new(0, 0, 120, 30);
            app.features.pixel_mouse = false;
            app.focus = Focus::Editor;
            app.focus_pane(0);
            map_panes(&mut app);
            app
        }

        fn map_panes(app: &mut App) {
            app.regions =
                view::regions(app.frame, false, app.panes.len(), false, false, None, None);
            for pane in 0..app.panes.len() {
                let editor = &mut app.scenes.get_mut(app.panes[pane].scene).unwrap().editor;
                let grid = view::source_grid(editor, app.regions.panes[pane].editor, false);
                editor.set_view_size(usize::from(grid.width), usize::from(grid.height));
                app.panes[pane].last_map = Some(editor.screen_map(grid).unwrap());
            }
        }

        fn click(app: &mut App, x: u16, y: u16, modifiers: KeyModifiers) {
            let (column, row) = if app.features.pixel_mouse {
                let (width, height) = app.features.cell_pixels.unwrap();
                (x * width + width / 2, y * height + height / 2)
            } else {
                (x, y)
            };
            for kind in [
                MouseEventKind::Down(MouseButton::Left),
                MouseEventKind::Up(MouseButton::Left),
            ] {
                let mut event = Event::Mouse(MouseEvent {
                    kind,
                    column,
                    row,
                    modifiers,
                });
                app.pointer_to_cells(&mut event);
                app.handle_terminal_event(event).unwrap();
            }
        }

        fn evaluation(app: &App) -> Option<&PendingEvaluation> {
            app.pending_evaluation
                .as_ref()
                .or_else(|| app.inflight.values().next())
        }

        /// No click on a title plays its scene, whatever modifier is held. The
        /// click only chooses the scene.
        #[test]
        fn no_modifier_makes_a_title_click_play_its_scene() {
            for (focused, clicked, modifiers) in [
                (0, 0, KeyModifiers::CONTROL),
                (1, 1, KeyModifiers::SHIFT),
                (0, 1, KeyModifiers::CONTROL | KeyModifiers::SHIFT),
                (1, 0, KeyModifiers::NONE),
            ] {
                let directory = tempfile::tempdir().unwrap();
                let mut app = split_app(directory.path());
                app.focus_pane(focused);
                app.playing_pane = Some(1 - clicked);
                map_panes(&mut app);
                let scenes: Vec<_> = app.panes.iter().map(|pane| pane.scene).collect();
                let title = app.regions.panes[clicked].title;
                assert!(!title.is_empty());
                let selections: Vec<_> = scenes
                    .iter()
                    .map(|id| app.scenes.get(*id).unwrap().editor.primary_selection())
                    .collect();

                click(&mut app, title.x + 2, title.y, modifiers);

                assert!(
                    evaluation(&app).is_none(),
                    "{modifiers:?} on a title played a scene"
                );
                // What it does do: the caret moves to that pane, and nothing else
                // about the set changes.
                assert_eq!(app.focused, clicked);
                assert_eq!(app.scenes.current().id, scenes[clicked]);
                assert_eq!(app.playing_pane, Some(1 - clicked), "and nothing plays");
                assert_eq!(
                    app.panes.iter().map(|pane| pane.scene).collect::<Vec<_>>(),
                    scenes
                );
                assert_eq!(
                    scenes
                        .iter()
                        .map(|id| app.scenes.get(*id).unwrap().editor.primary_selection())
                        .collect::<Vec<_>>(),
                    selections
                );
            }
        }

        #[test]
        fn a_plain_split_title_click_only_focuses_and_keeps_the_caret() {
            let directory = tempfile::tempdir().unwrap();
            let mut app = split_app(directory.path());
            // Alternating panes: two plain clicks on the SAME title in quick
            // succession are a double click, which launches on purpose.
            for pane in [1, 0, 1] {
                let scene = app.panes[pane].scene;
                let selected = Selection::range(ByteOffset(2), ByteOffset(6));
                app.scenes
                    .get_mut(scene)
                    .unwrap()
                    .editor
                    .set_selection(selected)
                    .unwrap();
                map_panes(&mut app);
                let title = app.regions.panes[pane].title;
                click(&mut app, title.x + 2, title.y, KeyModifiers::NONE);
                assert_eq!(app.focused, pane);
                assert_eq!(app.scenes.current().id, scene);
                assert_eq!(app.editor().primary_selection(), selected);
                assert!(evaluation(&app).is_none());
            }
        }

        #[test]
        fn shift_click_in_split_source_extends_selection_without_launching() {
            let directory = tempfile::tempdir().unwrap();
            let mut app = split_app(directory.path());
            for pane in [0, 1] {
                app.focus_pane(0);
                let scene = app.panes[pane].scene;
                app.scenes
                    .get_mut(scene)
                    .unwrap()
                    .editor
                    .set_selection(Selection::caret(ByteOffset(2)))
                    .unwrap();
                map_panes(&mut app);
                let target = app.panes[pane]
                    .last_map
                    .as_ref()
                    .unwrap()
                    .cell_for_offset(ByteOffset(7))
                    .unwrap();
                let before = app.scenes.get(scene).unwrap().editor.source();
                click(&mut app, target.x, target.y, KeyModifiers::SHIFT);
                assert_eq!(app.focused, pane);
                assert_eq!(
                    app.editor().primary_selection(),
                    Selection::range(ByteOffset(2), ByteOffset(7))
                );
                assert_eq!(app.editor().source(), before);
                assert!(evaluation(&app).is_none());
            }
        }

        /// A setup tab is never evaluated as a score by being clicked, and it
        /// never was: its tab only takes the caret.
        #[test]
        fn a_click_on_a_split_setup_title_never_evaluates_it_as_a_score() {
            let directory = tempfile::tempdir().unwrap();
            let mut app = split_app(directory.path());
            app.open_prebake(PrebakeScope::Local);
            let setup = app.scenes.current().id;
            app.focus_pane(1);
            map_panes(&mut app);
            let title = app.regions.panes[0].title;
            for modifiers in [KeyModifiers::NONE, KeyModifiers::SHIFT] {
                click(&mut app, title.x + 2, title.y, modifiers);
                assert_eq!(app.scenes.current().id, setup);
                assert_eq!(app.focused, 0);
                assert!(evaluation(&app).is_none());
                assert!(app.prebake_inflight.is_none() && app.prebake_queue.is_empty());
                assert_eq!(app.next_launch, Launch::Now);
            }
        }
    }

    mod pane_navigation {
        //! Tests for pane navigation.

        use crate::app::keyboard::{SceneNavigation, scene_navigation_for_key};
        use crate::app::*;
        use crate::editor::Selection;

        fn navigation_app(directory: &std::path::Path, count: usize, split: bool) -> App {
            let mut app = crate::app::tests::support::app_over(directory);
            for index in 1..count {
                app.scenes
                    .create(&format!("// scene {index}\n$: s(\"hh\")\n"))
                    .unwrap();
            }
            app.frame = Rect::new(0, 0, 140, 40);
            app.capabilities = KeyboardCapabilities::enhanced();
            app.focus_pane(0);
            if split {
                app.toggle_split();
                app.focus_pane(0);
            }
            for index in 0..count {
                let id = app.scenes.scenes()[index].id;
                app.scenes
                    .get_mut(id)
                    .unwrap()
                    .editor
                    .set_selection(Selection::caret(ByteOffset(index + 1)))
                    .unwrap();
            }
            app.focus = Focus::Editor;
            app
        }

        fn press(app: &mut App, code: KeyCode, modifiers: KeyModifiers) {
            app.handle_terminal_event(Event::Key(KeyEvent::new(code, modifiers)))
                .unwrap();
        }

        fn editor_states(app: &App) -> Vec<(String, Selection, crate::editor::Viewport)> {
            app.scenes
                .scenes()
                .iter()
                .map(|scene| {
                    (
                        scene.editor.source(),
                        scene.editor.primary_selection(),
                        scene.editor.viewport(),
                    )
                })
                .collect()
        }

        #[test]
        fn plain_brackets_switch_split_focus_without_replacing_or_launching_scenes() {
            let directory = tempfile::tempdir().unwrap();
            let mut app = navigation_app(directory.path(), 4, true);
            let visible: Vec<_> = app.panes.iter().map(|pane| pane.scene).collect();
            let states = editor_states(&app);
            app.playing_pane = Some(1);
            app.audible_scene = Some(visible[1]);
            let launch = app.next_launch;

            for (glyph, expected) in [(']', 1), (']', 0), ('[', 1), ('[', 0)] {
                press(&mut app, KeyCode::Char(glyph), KeyModifiers::CONTROL);
                assert_eq!(app.focused, expected);
                assert_eq!(app.scenes.current().id, visible[expected]);
                assert_eq!(
                    app.panes.iter().map(|pane| pane.scene).collect::<Vec<_>>(),
                    visible
                );
            }
            assert_eq!(editor_states(&app), states);
            assert_eq!(app.playing_pane, Some(1));
            assert_eq!(app.audible_scene, Some(visible[1]));
            assert_eq!(app.next_launch, launch);
            assert!(app.pending_evaluation.is_none() && app.inflight.is_empty());
        }

        #[test]
        fn split_scene_cycles_skip_the_other_pane_in_both_directions_and_wrap() {
            for focused in [0, 1] {
                let directory = tempfile::tempdir().unwrap();
                let mut app = navigation_app(directory.path(), 4, true);
                app.focus_pane(focused);
                let ids: Vec<_> = app.scenes.scenes().iter().map(|scene| scene.id).collect();
                let other = app.panes[1 - focused].scene;
                let states = editor_states(&app);
                app.playing_pane = Some(1 - focused);
                app.audible_scene = Some(other);
                let launch = app.next_launch;
                for (code, modifiers, index) in [
                    (
                        KeyCode::Char(']'),
                        KeyModifiers::CONTROL | KeyModifiers::SHIFT,
                        2,
                    ),
                    (KeyCode::Char('}'), KeyModifiers::CONTROL, 3),
                    (KeyCode::F(7), KeyModifiers::NONE, focused),
                    (
                        KeyCode::Char('['),
                        KeyModifiers::CONTROL | KeyModifiers::SHIFT,
                        3,
                    ),
                    (KeyCode::Char('{'), KeyModifiers::CONTROL, 2),
                    (KeyCode::F(6), KeyModifiers::NONE, focused),
                ] {
                    press(&mut app, code, modifiers);
                    assert_eq!(app.focused, focused);
                    assert_eq!(app.scenes.current().id, ids[index]);
                    assert_eq!(app.panes[focused].scene, ids[index]);
                    assert_eq!(app.panes[1 - focused].scene, other);
                }
                assert_eq!(editor_states(&app), states);
                assert_eq!(app.playing_pane, Some(1 - focused));
                assert_eq!(app.audible_scene, Some(other));
                assert_eq!(app.next_launch, launch);
                assert!(app.pending_evaluation.is_none() && app.inflight.is_empty());
            }
        }

        #[test]
        fn a_click_on_the_other_panes_tab_moves_the_caret_there_and_keeps_both_panes() {
            let directory = tempfile::tempdir().unwrap();
            let mut app = navigation_app(directory.path(), 5, true);
            app.regions.scenes = Rect::new(0, 1, 140, 1);
            let ids: Vec<_> = app.scenes.scenes().iter().map(|scene| scene.id).collect();
            let click = |app: &mut App, index: usize| {
                let chip = view::scene_strip_hits(app.regions.scenes, &app.scene_chips())[index];
                app.handle_mouse(MouseEvent {
                    kind: MouseEventKind::Down(MouseButton::Left),
                    column: chip.x,
                    row: chip.y,
                    modifiers: KeyModifiers::NONE,
                })
                .unwrap();
            };
            // Left pane on the third scene, right pane on the fifth, then
            // the fourth.
            click(&mut app, 2);
            app.focus_pane(1);
            click(&mut app, 4);
            click(&mut app, 3);

            // The caret is in the right pane. The third scene is in the left.
            click(&mut app, 2);
            assert_eq!(app.focused, 0, "the caret went to the pane that has it");
            assert_eq!(app.scenes.current().id, ids[2]);
            assert_eq!(
                app.panes.iter().map(|pane| pane.scene).collect::<Vec<_>>(),
                [ids[2], ids[3]],
                "and neither pane changed its scene"
            );

            // And back, by the tab of the right pane.
            click(&mut app, 3);
            assert_eq!(app.focused, 1);
            assert_eq!(app.scenes.current().id, ids[3]);
        }

        #[test]
        fn single_pane_keeps_plain_and_shifted_scene_navigation_and_portable_aliases() {
            let directory = tempfile::tempdir().unwrap();
            let mut app = navigation_app(directory.path(), 3, false);
            for (code, modifiers, index) in [
                (KeyCode::Char(']'), KeyModifiers::CONTROL, 1),
                (KeyCode::Char('}'), KeyModifiers::CONTROL, 2),
                (
                    KeyCode::Char(']'),
                    KeyModifiers::CONTROL | KeyModifiers::SHIFT,
                    0,
                ),
                (KeyCode::Char('['), KeyModifiers::CONTROL, 2),
                (KeyCode::Char('{'), KeyModifiers::CONTROL, 1),
            ] {
                press(&mut app, code, modifiers);
                assert_eq!(app.scenes.current_index(), index);
                assert_eq!(app.focused, 0);
            }
            app.capabilities = KeyboardCapabilities::legacy();
            press(&mut app, KeyCode::F(6), KeyModifiers::NONE);
            assert_eq!(app.scenes.current_index(), 0);
            press(&mut app, KeyCode::F(7), KeyModifiers::NONE);
            assert_eq!(app.scenes.current_index(), 1);
        }

        #[test]
        fn two_open_scenes_have_no_cycle_target_and_menus_use_contextual_chords() {
            use crate::menu::{MenuAction, MenuItemKind};
            let directory = tempfile::tempdir().unwrap();
            let mut app = navigation_app(directory.path(), 2, true);
            let visible: Vec<_> = app.panes.iter().map(|pane| pane.scene).collect();
            for code in [KeyCode::F(6), KeyCode::F(7)] {
                press(&mut app, code, KeyModifiers::NONE);
                assert_eq!(app.focused, 0);
                assert_eq!(
                    app.panes.iter().map(|pane| pane.scene).collect::<Vec<_>>(),
                    visible
                );
            }
            for (split, previous, next, enabled) in
                [(true, "^⇧[", "^⇧]", false), (false, "^[", "^]", true)]
            {
                if !split {
                    app.close_split();
                }
                for (action, expected) in [
                    (MenuAction::PreviousScene, previous),
                    (MenuAction::NextScene, next),
                ] {
                    let row = app
                        .menus()
                        .into_iter()
                        .flat_map(|menu| menu.items)
                        .find(|item| item.kind == MenuItemKind::Action(action))
                        .unwrap();
                    assert_eq!(row.accel, expected);
                    assert_eq!(row.enabled, enabled);
                }
            }
        }

        #[test]
        fn scene_navigation_rejects_repeats_releases_and_ambiguous_modifiers() {
            let enhanced = KeyboardCapabilities::enhanced();
            for (code, modifiers) in [
                (KeyCode::Char(']'), KeyModifiers::CONTROL),
                (KeyCode::Char('{'), KeyModifiers::CONTROL),
                (KeyCode::F(6), KeyModifiers::NONE),
                (KeyCode::F(7), KeyModifiers::NONE),
            ] {
                for kind in [KeyEventKind::Repeat, KeyEventKind::Release] {
                    let mut key = KeyEvent::new(code, modifiers);
                    key.kind = kind;
                    assert_eq!(scene_navigation_for_key(&key, enhanced, true), None);
                }
            }
            for modifiers in [
                KeyModifiers::NONE,
                KeyModifiers::SHIFT,
                KeyModifiers::ALT,
                KeyModifiers::CONTROL | KeyModifiers::ALT,
            ] {
                assert_eq!(
                    scene_navigation_for_key(
                        &KeyEvent::new(KeyCode::Char(']'), modifiers),
                        enhanced,
                        true
                    ),
                    None
                );
            }
            for glyph in ['[', ']', '{', '}'] {
                assert_eq!(
                    scene_navigation_for_key(
                        &KeyEvent::new(KeyCode::Char(glyph), KeyModifiers::CONTROL),
                        KeyboardCapabilities::legacy(),
                        true
                    ),
                    None
                );
            }
            assert_eq!(
                scene_navigation_for_key(
                    &KeyEvent::new(KeyCode::Char('{'), KeyModifiers::SUPER),
                    enhanced,
                    true
                ),
                Some(SceneNavigation::Step(-1))
            );

            let directory = tempfile::tempdir().unwrap();
            let mut app = navigation_app(directory.path(), 3, true);
            let states = editor_states(&app);
            for kind in [KeyEventKind::Repeat, KeyEventKind::Release] {
                for (code, modifiers) in [
                    (KeyCode::Char(']'), KeyModifiers::CONTROL),
                    (KeyCode::Char('}'), KeyModifiers::CONTROL),
                    (KeyCode::F(7), KeyModifiers::NONE),
                ] {
                    let mut key = KeyEvent::new(code, modifiers);
                    key.kind = kind;
                    app.handle_terminal_event(Event::Key(key)).unwrap();
                }
            }
            assert_eq!(app.focused, 0);
            assert_eq!(app.scenes.current_index(), 0);
            assert_eq!(editor_states(&app), states);
        }

        #[test]
        fn navigation_respects_modal_owners_and_does_not_reorder_settings_sources() {
            use crate::prefs::SampleSourcePref;
            use crate::settings::SettingsPage;
            let directory = tempfile::tempdir().unwrap();
            let mut app = navigation_app(directory.path(), 3, true);
            app.help = Some(HelpState::default());
            press(&mut app, KeyCode::Char(']'), KeyModifiers::CONTROL);
            assert_eq!(app.focused, 0);
            app.help = None;
            app.begin_rename();
            press(&mut app, KeyCode::F(7), KeyModifiers::NONE);
            assert_eq!(app.scenes.current_index(), 0);
            assert!(matches!(app.strip_mode, SceneStripMode::Renaming(_)));
            app.strip_mode = SceneStripMode::Idle;
            app.open_set_prompt(SetPrompt::OpenSet);
            press(&mut app, KeyCode::F(7), KeyModifiers::NONE);
            assert_eq!(app.scenes.current_index(), 0);
            assert!(app.set_prompt.is_some());
            app.set_prompt = None;
            app.prefs.sample_sources = ["first", "second"]
                .into_iter()
                .map(|spec| SampleSourcePref {
                    spec: spec.into(),
                    enabled: true,
                })
                .collect();
            let sources = app.prefs.sample_sources.clone();
            let mut sheet = SettingsSheet::default();
            sheet.show_page(SettingsPage::Sources);
            app.settings_sheet = Some(sheet);
            app.focus = Focus::Panel(PanelKind::Settings);
            press(&mut app, KeyCode::Char(']'), KeyModifiers::CONTROL);
            // Settings owns the keyboard; navigation must not focus a covered pane.
            assert_eq!(app.focused, 0);
            assert_eq!(app.focus, Focus::Panel(PanelKind::Settings));
            assert_eq!(app.prefs.sample_sources, sources);
            assert_eq!(app.settings_sheet.unwrap().selected, 0);
        }

        /// ^E is not chrome, it is the score in two places, so zen keeps it -
        /// only the strip that would name the panes stays hidden, which is the
        /// whole point of zen.
        #[test]
        fn ctrl_e_splits_the_editor_in_zen_with_the_tabs_still_hidden() {
            let directory = tempfile::tempdir().unwrap();
            let mut app = navigation_app(directory.path(), 2, false);
            app.toggle_zen();
            assert!(app.ui_settings.zen);
            assert_eq!(app.panes.len(), 1);
            press(&mut app, KeyCode::Char('e'), KeyModifiers::CONTROL);
            assert_eq!(app.panes.len(), 2, "^E did not split in zen");
            let layout = crate::view::regions_with(
                app.frame,
                app.ui_settings.zen,
                app.panes.len(),
                false,
                app.ui_settings.minimap,
                None,
                [None, None],
                None,
                None,
            );
            assert_eq!(layout.pane_count, 2);
            assert!(layout.scenes.is_empty(), "the tabs stay hidden in zen");
            assert!(layout.menu.is_empty());
            assert!(!layout.panes[0].editor.is_empty());
            assert!(!layout.panes[1].editor.is_empty());
            assert_eq!(
                layout.panes[0].editor.width + layout.panes[1].editor.width,
                app.frame.width,
                "the two panes still share the whole stage"
            );
        }

        /// Outside zen, ^[/^] already hop panes rather than step scenes once a
        /// split is open (`scene_navigation_for_key`'s `FocusPane` reading) - and
        /// zen, which changes nothing about that function, must not change the
        /// answer either.
        #[test]
        fn plain_brackets_hop_panes_in_zen_once_split_instead_of_stepping_scenes() {
            let directory = tempfile::tempdir().unwrap();
            let mut app = navigation_app(directory.path(), 4, true);
            app.toggle_zen();
            assert!(app.ui_settings.zen);
            let visible: Vec<_> = app.panes.iter().map(|pane| pane.scene).collect();
            for (glyph, expected) in [(']', 1), (']', 0), ('[', 1), ('[', 0)] {
                press(&mut app, KeyCode::Char(glyph), KeyModifiers::CONTROL);
                assert_eq!(
                    app.focused, expected,
                    "zen with a split must hop panes, not step scenes"
                );
                assert_eq!(app.scenes.current().id, visible[expected]);
            }
        }
    }

    mod panes {
        //! Tests for `app/panes.rs`.

        use crate::app::tests::support::press;
        use crate::app::*;

        /// Word wrap is a setting: ^U flips it for every scene, the
        /// preferences remember it, and a long line takes more than one row.
        #[test]
        fn word_wrap_is_a_remembered_setting() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = crate::app::tests::support::app_over(directory.path());
            let long = "$: note(\"c3 e3 g3 b3 c4 e4 g4 b4 c5 e5 g5 b5\").s(\"sawtooth\").lpf(800).room(0.5)\n";
            app.scenes.current_mut().editor = Editor::new(long).expect("editor");
            assert!(app.ui_settings.wrap, "on by default in the terminal");

            let region = Rect::new(0, 0, 40, 10);
            let scene = app.scenes.current_mut();
            scene.editor.set_wrap(true);
            let grid = view::source_grid(&scene.editor, region, true);
            scene
                .editor
                .set_view_size(usize::from(grid.width), usize::from(grid.height));
            let map = scene.editor.screen_map(grid).expect("map");
            assert!(
                map.rows().len() > 2,
                "the long line wraps into rows: {}",
                map.rows().len()
            );

            press(&mut app, KeyCode::Char('u'), KeyModifiers::CONTROL);
            assert!(!app.ui_settings.wrap);
            assert!(!app.prefs.ui_settings().wrap, "explicit off is remembered");
            assert!(app.status.starts_with("no wrap"), "{}", app.status);

            press(&mut app, KeyCode::Char('u'), KeyModifiers::CONTROL);
            assert!(app.ui_settings.wrap);
            assert!(app.prefs.ui_settings().wrap, "explicit on is remembered");
            assert!(app.status.starts_with("word wrap"), "{}", app.status);
        }

        /// ^E is the split's one key: it opens the second pane and closes it
        /// again. F10 hops between the two, as the shifted chord does where a
        /// terminal delivers one.
        #[test]
        fn control_e_toggles_the_split_and_f10_hops() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = crate::app::tests::support::app_over(directory.path());
            press(&mut app, KeyCode::Char('n'), KeyModifiers::CONTROL);
            press(&mut app, KeyCode::Char('e'), KeyModifiers::CONTROL);
            assert_eq!(app.panes.len(), 2, "^E opened the split");
            assert_eq!(app.focused, 1);
            press(&mut app, KeyCode::F(10), KeyModifiers::NONE);
            assert_eq!(app.focused, 0, "F10 hopped");
            press(&mut app, KeyCode::Char('e'), KeyModifiers::CONTROL);
            assert_eq!(app.panes.len(), 1, "^E closed it");
            assert_eq!(app.status, "single pane");
        }

        /// One click in the other pane both focuses it and lands the caret
        /// where it fell.
        #[test]
        fn a_click_in_the_other_pane_lands_the_caret_there() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = crate::app::tests::support::app_over(directory.path());
            press(&mut app, KeyCode::Char('n'), KeyModifiers::CONTROL);
            press(&mut app, KeyCode::Char('e'), KeyModifiers::CONTROL);
            let other_scene = app.panes[1].scene;
            let source = (0..30)
                .map(|n| format!("// line {n}\n"))
                .collect::<String>();
            app.scenes.get_mut(other_scene).expect("scene").editor =
                Editor::new(&source).expect("editor");
            press(&mut app, KeyCode::F(10), KeyModifiers::NONE);
            assert_eq!(app.focused, 0);
            let other_scene = app.panes[1].scene;
            app.frame = Rect::new(0, 0, 160, 40);
            app.regions = view::regions(app.frame, false, 2, false, true, None, None);
            for index in 0..2 {
                let region = app.regions.panes[index];
                let scene = app.panes[index].scene;
                let editor = &mut app.scenes.get_mut(scene).expect("scene").editor;
                let grid = view::source_grid(editor, region.editor, true);
                app.panes[index].last_map = Some(editor.screen_map(grid).expect("map"));
            }
            let other = app.regions.panes[1].editor;
            let (x, y) = (other.x + 6, other.y + 2);
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
                .expect("mouse");
            }
            assert_eq!(app.focused, 1, "the click focused the pane");
            assert_eq!(app.scenes.current().id, other_scene);
            let head = app.editor().primary_selection().head;
            let line = app.editor().document().line_of(head).expect("a line");
            assert_eq!(line, 2, "and landed the caret on the third line it shows");
        }

        /// A press on the scrollbar puts the page where the pointer is along
        /// the bar; a drag keeps following.
        #[test]
        fn a_press_on_the_scrollbar_scrolls_to_where_the_pointer_is() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = crate::app::tests::support::app_over(directory.path());
            let source = (0..200)
                .map(|n| format!("// line {n}\n"))
                .collect::<String>();
            app.scenes.current_mut().editor = Editor::new(&source).expect("editor");
            app.frame = Rect::new(0, 0, 100, 30);
            app.regions = view::regions(app.frame, false, 1, false, false, None, None);
            let bar = app.regions.panes[0].scrollbar;
            assert!(!bar.is_empty(), "no minimap, so a scrollbar");
            let region = app.regions.panes[0];
            let editor = &mut app.scenes.current_mut().editor;
            let grid = view::source_grid(editor, region.editor, true);
            editor.set_view_size(usize::from(grid.width), usize::from(grid.height));
            app.panes[0].last_map = Some(editor.screen_map(grid).expect("map"));
            app.handle_terminal_event(Event::Mouse(MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: bar.x,
                row: bar.bottom() - 1,
                modifiers: KeyModifiers::NONE,
            }))
            .expect("press");
            let (top, _, total) = app.editor().scroll_extent();
            assert_eq!(top, total - 1, "the bar's foot is the text's last row");
            assert!(matches!(app.pointer, Some(Pointer::Scrollbar)));
            app.handle_terminal_event(Event::Mouse(MouseEvent {
                kind: MouseEventKind::Drag(MouseButton::Left),
                column: bar.x,
                row: bar.y,
                modifiers: KeyModifiers::NONE,
            }))
            .expect("drag");
            assert_eq!(app.editor().scroll_extent().0, 0, "dragged back to the top");

            app.ui_settings.show_scrollbars = false;
            assert!(
                !app.vertical_scrollbar_at(0, bar.x, bar.y),
                "a hidden scrollbar is not an invisible mouse target"
            );
        }

        #[test]
        fn horizontal_scrollbar_drag_survives_every_redraw() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = crate::app::tests::support::app_over(directory.path());
            app.scenes.current_mut().editor = Editor::new(&"0123456789".repeat(100)).unwrap();
            app.ui_settings.wrap = false;
            app.ui_settings.minimap = false;
            app.ui_settings.apply();
            let mut terminal =
                ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 24)).unwrap();
            let paint =
                |app: &mut App, terminal: &mut ratatui::Terminal<ratatui::backend::TestBackend>| {
                    terminal
                        .draw(|frame| app.paint_into(frame, Instant::now()).unwrap())
                        .unwrap();
                };
            let mouse = |app: &mut App, kind, column, row| {
                app.handle_terminal_event(Event::Mouse(MouseEvent {
                    kind,
                    column,
                    row,
                    modifiers: KeyModifiers::NONE,
                }))
                .unwrap();
            };
            paint(&mut app, &mut terminal);
            let bar = app.regions.panes[0].horizontal_scrollbar;
            assert!(!bar.is_empty());
            let selection = app.editor().primary_selection();
            mouse(
                &mut app,
                MouseEventKind::Down(MouseButton::Left),
                bar.x + 1,
                bar.y,
            );
            paint(&mut app, &mut terminal);
            for column in bar.x + 2..bar.right() {
                let row = if column % 2 == 0 { bar.y } else { 0 };
                mouse(
                    &mut app,
                    MouseEventKind::Drag(MouseButton::Left),
                    column,
                    row,
                );
                let dragged = app.editor().viewport().left_column;
                assert!(dragged > 0);
                paint(&mut app, &mut terminal);
                assert_eq!(
                    app.editor().viewport().left_column,
                    dragged,
                    "a redraw must not pull horizontal scrolling back to the caret"
                );
            }
            mouse(
                &mut app,
                MouseEventKind::Up(MouseButton::Left),
                bar.right() - 1,
                bar.y,
            );
            for _ in 0..3 {
                paint(&mut app, &mut terminal);
                let (left, page, total) = app.editor().horizontal_scroll_extent().unwrap();
                assert_eq!(
                    left,
                    total - page,
                    "the viewport remains at the end after release"
                );
                let (start, length) =
                    view::horizontal_scrollbar_thumb((left, page, total), bar.width);
                assert_eq!(start + length, usize::from(bar.width));
                let edge = terminal
                    .backend()
                    .buffer()
                    .cell((bar.right() - 1, bar.y))
                    .unwrap();
                assert_eq!(edge.symbol(), "─");
                assert_eq!(
                    edge.fg, app.theme.muted,
                    "the painted handle reaches the edge"
                );
            }
            mouse(
                &mut app,
                MouseEventKind::Down(MouseButton::Left),
                bar.right() - 1,
                bar.y,
            );
            for column in (bar.x..bar.right() - 1).rev() {
                mouse(&mut app, MouseEventKind::Drag(MouseButton::Left), column, 0);
                let dragged = app.editor().viewport().left_column;
                paint(&mut app, &mut terminal);
                assert_eq!(app.editor().viewport().left_column, dragged);
            }
            mouse(&mut app, MouseEventKind::Up(MouseButton::Left), bar.x, 0);
            paint(&mut app, &mut terminal);
            assert_eq!(app.editor().viewport().left_column, 0);
            assert!(
                app.pointer.is_none(),
                "release outside the row ends capture"
            );
            assert!(app.menu.is_none(), "passing over the menu never opens it");
            assert_eq!(
                selection,
                app.editor().primary_selection(),
                "scrolling never moves the caret"
            );
        }

        #[test]
        fn every_horizontal_thumb_cell_can_be_grabbed_without_jumping() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = crate::app::tests::support::app_over(directory.path());
            app.scenes.current_mut().editor = Editor::new(&"0123456789".repeat(100)).unwrap();
            app.ui_settings.wrap = false;
            app.ui_settings.minimap = false;
            app.ui_settings.apply();
            let mut terminal =
                ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 24)).unwrap();
            let paint =
                |app: &mut App, terminal: &mut ratatui::Terminal<ratatui::backend::TestBackend>| {
                    terminal
                        .draw(|frame| app.paint_into(frame, Instant::now()).unwrap())
                        .unwrap();
                };
            paint(&mut app, &mut terminal);
            let bar = app.regions.panes[0].horizontal_scrollbar;
            for left in [1, 41, 149, 450] {
                let mut viewport = app.editor().viewport();
                viewport.left_column = left;
                app.editor_mut().set_viewport(viewport);
                paint(&mut app, &mut terminal);
                let extent = app.editor().horizontal_scroll_extent().unwrap();
                let (start, length) = view::horizontal_scrollbar_thumb(extent, bar.width);
                for cell in start..start + length {
                    let column = bar.x + cell as u16;
                    let painted = terminal.backend().buffer().cell((column, bar.y)).unwrap();
                    assert_eq!(painted.symbol(), "─");
                    assert_eq!(painted.fg, app.theme.muted);
                    for (kind, row) in [
                        (MouseEventKind::Down(MouseButton::Left), bar.y),
                        (MouseEventKind::Drag(MouseButton::Left), 0),
                        (MouseEventKind::Up(MouseButton::Left), 0),
                    ] {
                        app.handle_terminal_event(Event::Mouse(MouseEvent {
                            kind,
                            column,
                            row,
                            modifiers: KeyModifiers::NONE,
                        }))
                        .unwrap();
                        paint(&mut app, &mut terminal);
                        assert_eq!(
                            app.editor().viewport().left_column,
                            left,
                            "grabbing cell {cell} at scroll offset {left} must not jump"
                        );
                    }
                    assert!(app.pointer.is_none());
                }
            }
        }

        #[test]
        fn the_horizontal_scrollbar_has_its_own_draggable_row() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = crate::app::tests::support::app_over(directory.path());
            app.scenes.current_mut().editor =
                Editor::new(&"0123456789".repeat(20)).expect("editor");
            app.ui_settings.wrap = false;
            app.scenes.current_mut().editor.set_wrap(false);
            app.frame = Rect::new(0, 0, 40, 12);
            let mut regions = view::regions(app.frame, false, 1, false, false, None, None);
            reserve_horizontal_scrollbar(
                &mut app.scenes.current_mut().editor,
                &mut regions.panes[0],
                true,
                true,
            );
            app.regions = regions;

            let text = app.regions.panes[0].editor;
            let bar = app.regions.panes[0].horizontal_scrollbar;
            assert_eq!(bar.y, text.bottom(), "the bar is below the text");
            assert_eq!(bar.height, 1);
            assert_eq!(
                app.horizontal_scrollbar_at(text.x, text.bottom() - 1),
                None,
                "the last text row remains a caret target"
            );
            assert_eq!(app.horizontal_scrollbar_at(bar.x, bar.y), Some(0));

            app.grab_horizontal_scrollbar(0, bar.right() - 1);
            assert!(
                app.editor().viewport().left_column > 0,
                "dragging the bar reaches the hidden columns"
            );
            let mut viewport = app.editor().viewport();
            viewport.left_column = 41;
            app.editor_mut().set_viewport(viewport);
            let left = app.editor().viewport().left_column;
            let extent = app.editor().horizontal_scroll_extent().expect("extent");
            let (thumb, length) = view::horizontal_scrollbar_thumb(extent, bar.width);
            let grabbed_x = bar.x + u16::try_from(thumb + length / 2).unwrap();
            let grab = app.grab_horizontal_scrollbar(0, grabbed_x);
            assert_eq!(
                app.editor().viewport().left_column,
                left,
                "pressing a mid-range thumb must not make it jump"
            );
            app.drag_horizontal_scrollbar(0, grabbed_x, grab);
            assert_eq!(
                app.editor().viewport().left_column,
                left,
                "a stationary drag event must preserve the exact viewport"
            );

            app.handle_terminal_event(Event::Mouse(MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: grabbed_x,
                row: bar.y,
                modifiers: KeyModifiers::NONE,
            }))
            .expect("grab thumb");
            app.handle_terminal_event(Event::Mouse(MouseEvent {
                kind: MouseEventKind::Up(MouseButton::Left),
                column: bar.right() - 1,
                row: bar.y,
                modifiers: KeyModifiers::NONE,
            }))
            .expect("release at end");
            let (_, page, total) = app.editor().horizontal_scroll_extent().expect("extent");
            assert_eq!(
                app.editor().viewport().left_column,
                total - page,
                "a release without an intermediate drag reaches the exact end"
            );

            let extent = app.editor().horizontal_scroll_extent().expect("extent");
            let (thumb, length) = view::horizontal_scrollbar_thumb(extent, bar.width);
            let grabbed_x = bar.x + u16::try_from(thumb + length / 2).unwrap();
            app.handle_terminal_event(Event::Mouse(MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: grabbed_x,
                row: bar.y,
                modifiers: KeyModifiers::NONE,
            }))
            .expect("grab thumb at end");
            app.handle_terminal_event(Event::Mouse(MouseEvent {
                kind: MouseEventKind::Up(MouseButton::Left),
                column: bar.x,
                row: bar.y,
                modifiers: KeyModifiers::NONE,
            }))
            .expect("release at start");
            assert_eq!(
                app.editor().viewport().left_column,
                0,
                "the same sparse release reaches the exact start"
            );

            let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(40, 12))
                .expect("test terminal");
            terminal
                .draw(|frame| app.paint_into(frame, Instant::now()).expect("frame paints"))
                .expect("draw");
            let painted = app.regions.panes[0].horizontal_scrollbar;
            assert!(!painted.is_empty(), "the frame reserved the bar");
            let symbols = (painted.x..painted.right())
                .filter_map(|x| {
                    terminal
                        .backend()
                        .buffer()
                        .cell((x, painted.y))
                        .map(|cell| cell.symbol())
                })
                .collect::<String>();
            assert!(
                symbols.chars().all(|symbol| symbol == '─'),
                "the reserved row is visibly painted as a scrollbar: {symbols:?}"
            );

            app.ui_settings.show_scrollbars = false;
            terminal
                .draw(|frame| app.paint_into(frame, Instant::now()).expect("frame paints"))
                .expect("draw without scrollbars");
            assert!(
                app.regions.panes[0].horizontal_scrollbar.is_empty(),
                "turning scrollbars off gives the row back to the editor"
            );
            assert_eq!(app.horizontal_scrollbar_at(painted.x, painted.y), None);
        }

        /// Line numbers are a switch, on by default; off, the text starts at
        /// the pane's edge.
        #[test]
        fn line_numbers_are_a_remembered_switch() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = crate::app::tests::support::app_over(directory.path());
            assert!(app.ui_settings.line_numbers);
            app.dispatch_menu_action(crate::menu::MenuAction::LineNumbers)
                .expect("menu");
            assert!(!app.ui_settings.line_numbers);
            assert!(!app.prefs.ui_settings().line_numbers, "remembered");
            let region = Rect::new(0, 0, 60, 10);
            let grid = view::source_grid(
                &app.scenes.current().editor,
                region,
                app.ui_settings.line_numbers,
            );
            assert_eq!(grid.width, 59, "a one-cell margin, no numbers");
            app.dispatch_menu_action(crate::menu::MenuAction::LineNumbers)
                .expect("menu");
            assert!(app.ui_settings.line_numbers);
            let grid = view::source_grid(
                &app.scenes.current().editor,
                region,
                app.ui_settings.line_numbers,
            );
            assert!(grid.width < 59, "the gutter is back: {}", grid.width);
        }

        /// The wheel over the other pane scrolls that pane's scene, and the
        /// caret's pane stays where it was.
        #[test]
        fn the_wheel_scrolls_the_pane_under_the_pointer() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = crate::app::tests::support::app_over(directory.path());
            press(&mut app, KeyCode::Char('n'), KeyModifiers::CONTROL);
            press(&mut app, KeyCode::Char('e'), KeyModifiers::CONTROL);
            let other_scene = app.panes[1].scene;
            let source = (0..80)
                .map(|n| format!("// line {n}\n"))
                .collect::<String>();
            app.scenes.get_mut(other_scene).expect("scene").editor =
                Editor::new(&source).expect("editor");
            press(&mut app, KeyCode::F(10), KeyModifiers::NONE);
            assert_eq!(app.focused, 0);
            app.frame = Rect::new(0, 0, 160, 40);
            app.regions = view::regions(app.frame, false, 2, false, false, None, None);
            let other = app.regions.panes[1].editor;
            app.handle_terminal_event(Event::Mouse(MouseEvent {
                kind: MouseEventKind::ScrollDown,
                column: other.x + 5,
                row: other.y + 5,
                modifiers: KeyModifiers::NONE,
            }))
            .expect("wheel");
            let scrolled = app
                .scenes
                .get(other_scene)
                .expect("scene")
                .editor
                .viewport()
                .top_row;
            assert_eq!(scrolled, crate::editor::DEFAULT_WHEEL_ROWS);
            assert_eq!(
                app.editor().viewport().top_row,
                0,
                "the caret's pane did not move"
            );
            assert_eq!(app.focused, 0, "and keeps the caret");
        }

        /// Turning zen on from a focused panel focuses the editor.
        #[test]
        fn turning_zen_on_focuses_the_editor() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = crate::app::tests::support::app_over(directory.path());
            app.frame = Rect::new(0, 0, 120, 40);
            // Devices, not settings: the settings sheet is deliberately modal
            // (`settle_focus` reclaims it on every key for as long as it is
            // open, which this fix must not undo) and so proves nothing about
            // an ordinary panel giving the keyboard up.
            press(&mut app, KeyCode::Char('p'), KeyModifiers::CONTROL);
            assert_eq!(app.focus, Focus::Panel(PanelKind::Devices));
            press(&mut app, KeyCode::F(11), KeyModifiers::NONE);
            assert!(app.ui_settings.zen);
            assert_eq!(
                app.focus,
                Focus::Editor,
                "zen turned on with the devices panel still holding the keyboard"
            );
            // Off again is not this rule's business, but the caret keeping
            // the keyboard is the least it should do.
            press(&mut app, KeyCode::F(11), KeyModifiers::NONE);
            assert!(!app.ui_settings.zen);
            assert_eq!(app.focus, Focus::Editor);
        }

        /// The settings sheet is modal and keeps the keyboard while it is open.
        /// Turning zen on does not move the focus off it: `toggle_zen` skips
        /// the focus change when `settings_hold_the_keys` is true.
        #[test]
        fn turning_zen_on_over_the_settings_sheet_does_not_undo_its_modal_hold() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = crate::app::tests::support::app_over(directory.path());
            app.frame = Rect::new(0, 0, 120, 40);
            press(&mut app, KeyCode::Char('o'), KeyModifiers::CONTROL);
            assert_eq!(app.focus, Focus::Panel(PanelKind::Settings));
            press(&mut app, KeyCode::F(11), KeyModifiers::NONE);
            assert!(app.ui_settings.zen, "zen still turns on");
            // Zen hands the keys to the score from an ordinary panel, but the
            // modal sheet keeps the keyboard.
            assert_eq!(
                app.focus,
                Focus::Panel(PanelKind::Settings),
                "the settings sheet lost its modal hold in zen"
            );
            press(&mut app, KeyCode::Esc, KeyModifiers::NONE);
            assert!(app.settings_sheet.is_none(), "Esc is still the way out");
            assert_eq!(app.focus, Focus::Editor);
        }

        /// Every sheet still opens, focuses and closes over the editor in zen:
        /// settings, devices, themes, export and the log sheet.
        #[test]
        fn zen_lets_every_sheet_open_and_focus_over_the_editor() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = crate::app::tests::support::app_over(directory.path());
            app.frame = Rect::new(0, 0, 120, 40);
            press(&mut app, KeyCode::F(11), KeyModifiers::NONE);
            assert!(app.ui_settings.zen);

            press(&mut app, KeyCode::Char('o'), KeyModifiers::CONTROL);
            assert!(app.settings_sheet.is_some(), "settings did not open in zen");
            assert_eq!(app.focus, Focus::Panel(PanelKind::Settings));
            press(&mut app, KeyCode::Esc, KeyModifiers::NONE);
            assert!(app.settings_sheet.is_none(), "Esc did not close settings");

            press(&mut app, KeyCode::Char('p'), KeyModifiers::CONTROL);
            assert!(app.panel.is_some(), "devices did not open in zen");
            assert_eq!(app.focus, Focus::Panel(PanelKind::Devices));
            press(&mut app, KeyCode::Esc, KeyModifiers::NONE);
            assert!(app.panel.is_none(), "Esc did not close devices");

            press(&mut app, KeyCode::Char('t'), KeyModifiers::CONTROL);
            assert!(app.theme_picker.is_some(), "themes did not open in zen");
            assert_eq!(app.focus, Focus::Panel(PanelKind::Theme));
            press(&mut app, KeyCode::Esc, KeyModifiers::NONE);
            assert!(app.theme_picker.is_none(), "Esc did not close themes");

            press(
                &mut app,
                KeyCode::Char('x'),
                KeyModifiers::CONTROL | KeyModifiers::SHIFT,
            );
            assert!(app.export_sheet.is_some(), "export did not open in zen");
            assert_eq!(app.focus, Focus::Panel(PanelKind::Export));
            press(&mut app, KeyCode::Esc, KeyModifiers::NONE);
            assert!(app.export_sheet.is_none(), "Esc did not close export");

            press(&mut app, KeyCode::F(9), KeyModifiers::NONE);
            assert!(app.log_panel.is_some(), "the log did not open in zen");
            assert_eq!(app.focus, Focus::Panel(PanelKind::Log));
            press(&mut app, KeyCode::Esc, KeyModifiers::NONE);
            assert!(app.log_panel.is_none(), "Esc did not close the log");
        }

        /// The set panel and the mixer have no dedicated sheet of their own
        /// the way settings or the log do: the set panel falls back to its
        /// sheet form only because zen leaves its sidebar column empty, and
        /// the mixer is given a band over the whole frame instead of one
        /// carved from the body. Both must open, focus, and leave the editor
        /// its full room underneath them.
        #[test]
        fn zen_pops_up_the_set_panel_and_the_mixer_instead_of_docking_them() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = crate::app::tests::support::app_over(directory.path());
            app.frame = Rect::new(0, 0, 120, 40);
            press(&mut app, KeyCode::F(11), KeyModifiers::NONE);
            assert!(app.ui_settings.zen);

            press(&mut app, KeyCode::Char('b'), KeyModifiers::CONTROL);
            assert!(app.set_panel.is_some(), "the set panel did not open in zen");
            assert_eq!(app.focus, Focus::Panel(PanelKind::Set));

            press(&mut app, KeyCode::F(4), KeyModifiers::NONE);
            assert!(app.mixer_panel.is_some(), "the mixer did not open in zen");
            assert_eq!(app.focus, Focus::Panel(PanelKind::Mixer));

            let layout = view::regions_with(
                app.frame,
                app.ui_settings.zen,
                app.panes.len(),
                false,
                app.ui_settings.minimap,
                app.set_panel.is_some().then_some(app.set_panel_side()),
                app.dock_requests(),
                app.mixer_panel.map(MixerPanel::dock),
                app.log_panel
                    .as_ref()
                    .and_then(|panel| view::log_dock(panel, app.frame)),
            );
            assert!(
                layout.sidebar.is_empty(),
                "an empty sidebar is what draws the set panel's sheet form"
            );
            assert!(
                !layout.mixer.is_empty(),
                "the mixer got no popup room at all"
            );
            assert_eq!(
                layout.panes[0].editor, app.frame,
                "neither popup took room away from the editor"
            );
        }

        /// A dock left open from before zen turned on has nowhere left to
        /// stand once the stage is bare: `apply_zen` closes it rather than
        /// leaving an invisible panel that still has the keyboard.
        #[test]
        fn zen_closes_a_visuals_dock_that_was_already_open() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = crate::app::tests::support::app_over(directory.path());
            app.frame = Rect::new(0, 0, 120, 40);
            press(&mut app, KeyCode::F(1), KeyModifiers::SHIFT);
            assert!(app.viz_docks[0].is_some());

            press(&mut app, KeyCode::F(11), KeyModifiers::NONE);
            assert!(app.ui_settings.zen);
            assert!(
                app.viz_docks[0].is_none(),
                "zen turned on with a visuals dock still open"
            );
        }

        /// Turning zen off must restore exactly what it hid: a docked set
        /// panel and mixer come back to the same room they had before, not
        /// the popups zen gave them instead.
        #[test]
        fn turning_zen_off_restores_the_docked_furniture_it_hid() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = crate::app::tests::support::app_over(directory.path());
            app.frame = Rect::new(0, 0, 120, 40);
            press(&mut app, KeyCode::Char('b'), KeyModifiers::CONTROL);
            press(&mut app, KeyCode::F(4), KeyModifiers::NONE);
            let layout_for = |app: &App| {
                view::regions_with(
                    app.frame,
                    app.ui_settings.zen,
                    app.panes.len(),
                    false,
                    app.ui_settings.minimap,
                    app.set_panel.is_some().then_some(app.set_panel_side()),
                    app.dock_requests(),
                    app.mixer_panel.map(MixerPanel::dock),
                    app.log_panel
                        .as_ref()
                        .and_then(|panel| view::log_dock(panel, app.frame)),
                )
            };
            let docked = layout_for(&app);
            assert!(!docked.sidebar.is_empty());
            assert!(!docked.mixer.is_empty());
            assert!(
                docked.panes[0].editor.width < app.frame.width,
                "docked furniture takes room from the editor"
            );

            press(&mut app, KeyCode::F(11), KeyModifiers::NONE);
            assert!(app.ui_settings.zen);
            press(&mut app, KeyCode::F(11), KeyModifiers::NONE);
            assert!(!app.ui_settings.zen);

            let restored = layout_for(&app);
            assert_eq!(
                restored.sidebar, docked.sidebar,
                "the set panel's dock did not come back the way it was left"
            );
            assert_eq!(
                restored.mixer, docked.mixer,
                "the mixer's dock did not come back the way it was left"
            );
        }

        /// A set opens laid out the way it was left: the same scores in the
        /// same panes, with the caret in the pane that had it.
        #[test]
        fn a_split_set_opens_split_again_on_the_pane_that_had_the_caret() {
            let directory = tempfile::tempdir().expect("temp dir");
            let reopen = |directory: &std::path::Path| {
                let scenes = SceneSet::open(directory, "first").expect("scenes");
                let mut app = crate::app::tests::support::app_over(directory);
                // `app_over` opens the folder itself; take the set again so the
                // manifest on disk is what is read.
                app.scenes = scenes;
                app.restore_panes();
                app
            };

            let mut app = crate::app::tests::support::app_over(directory.path());
            assert_eq!(app.panes.len(), 1, "a set opens in one pane");
            app.new_scene(false);
            let second = app.scenes.current().id;
            app.toggle_split();
            assert_eq!(app.panes.len(), 2, "split");
            // The second pane opens on the other scene; put the caret back in
            // the left one so the layout has something to say beyond "split".
            app.focus_pane(0);
            let left = app.panes[0].scene;
            let right = app.panes[1].scene;
            assert_ne!(left, right);
            assert!(app.manifest_pending, "the layout is owed a write");
            app.flush_manifest();

            let names = |app: &App| -> Vec<String> {
                app.panes
                    .iter()
                    .filter_map(|pane| app.scenes.get(pane.scene))
                    .map(|scene| scene.name())
                    .collect()
            };
            let before = names(&app);
            assert_eq!(before.len(), 2);

            let again = reopen(directory.path());
            assert_eq!(names(&again), before, "the same scores, in the same panes");
            assert_eq!(again.focused, 0, "and the caret where it was left");

            // The other pane, remembered as itself: two panes can hold the
            // same score, so the name alone could not say which you were in.
            let mut app = reopen(directory.path());
            app.focus_pane(1);
            assert_eq!(
                app.scenes.current().id,
                app.panes[1].scene,
                "the caret moved"
            );
            app.flush_manifest();
            assert_eq!(reopen(directory.path()).focused, 1);

            // Closing the split is remembered too, rather than leaving the set
            // opening into a layout it no longer has.
            let mut app = reopen(directory.path());
            app.close_split();
            assert_eq!(app.panes.len(), 1);
            app.flush_manifest();
            let again = reopen(directory.path());
            assert_eq!(again.panes.len(), 1, "one pane, as it was left");
            assert!(again.scenes.get(second).is_some(), "both scores kept");
        }

        /// Letting go of the pane's scrollbar is the scrollbar's alone: a score
        /// drag whose release never arrived is not pulled to where the bar was let
        /// go.
        #[test]
        fn the_scrollbar_release_never_reaches_the_score() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = crate::app::tests::support::app_over(directory.path());
            let source = (0..200)
                .map(|n| format!("// line {n}\n"))
                .collect::<String>();
            app.scenes.current_mut().editor = Editor::new(&source).expect("editor");
            let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(100, 30))
                .expect("test terminal");
            let mut draw = |app: &mut App| {
                terminal
                    .draw(|frame| app.paint_into(frame, Instant::now()).expect("frame paints"))
                    .expect("draw");
            };
            let mouse = |app: &mut App, kind, column, row| {
                app.handle_terminal_event(Event::Mouse(MouseEvent {
                    kind,
                    column,
                    row,
                    modifiers: KeyModifiers::NONE,
                }))
                .unwrap();
            };
            draw(&mut app);

            // A drag in the score whose release never arrives.
            let score = app.focused_map().expect("the score is drawn").area();
            mouse(
                &mut app,
                MouseEventKind::Down(MouseButton::Left),
                score.x + 2,
                score.y,
            );
            mouse(
                &mut app,
                MouseEventKind::Drag(MouseButton::Left),
                score.x + 6,
                score.y,
            );
            draw(&mut app);
            let selection = app.editor().primary_selection();
            assert!(!selection.is_empty(), "a score drag is live");

            let bar = app.regions.panes[app.focused].scrollbar;
            assert!(app.vertical_scrollbar_at(app.focused, bar.x, bar.y));
            assert!(
                bar.x >= score.x + score.width,
                "the bar is right of the grid"
            );

            // Grab the bar's foot, drag to its middle and let go there, painting
            // between reports as the loop does between input batches.
            mouse(
                &mut app,
                MouseEventKind::Down(MouseButton::Left),
                bar.x,
                bar.bottom() - 1,
            );
            assert_eq!(app.pointer, Some(Pointer::Scrollbar));
            draw(&mut app);
            let middle = bar.y + bar.height / 2;
            mouse(
                &mut app,
                MouseEventKind::Drag(MouseButton::Left),
                bar.x,
                middle,
            );
            draw(&mut app);
            let dragged = app.editor().viewport();
            let (top, _, total) = app.editor().scroll_extent();
            assert!(
                top > 0 && top < total - 1,
                "the drag moved the page to the middle"
            );
            mouse(
                &mut app,
                MouseEventKind::Up(MouseButton::Left),
                bar.x,
                middle,
            );

            assert_eq!(app.pointer, None, "the release let go of the bar");
            assert_eq!(
                app.editor().primary_selection(),
                selection,
                "the score's selection did not follow the scrollbar's release"
            );
            assert_eq!(
                app.editor().viewport(),
                dragged,
                "the page stayed where the drag put it"
            );
        }

        #[test]
        fn minimap_click_and_drag_follow_wrapped_screen_rows() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = crate::app::tests::support::app_over(directory.path());
            let area = Rect::new(20, 3, 5, 10);
            app.regions.panes[0].minimap = area;
            let scene = app.scenes.current_mut();
            scene.editor = Editor::new(&"abcdefgh".repeat(20)).expect("editor");
            scene.editor.set_view_size(9, 2);
            scene.editor.set_wrap(true);
            scene.editor.set_wrap_indents(vec![0]);
            scene
                .minimap
                .sync_editor(&scene.editor, area)
                .expect("minimap");

            app.press_minimap(area.y + 1);
            assert_eq!(
                app.editor().viewport().top_row,
                3,
                "jump centres screen row four"
            );
            assert!(matches!(
                app.pointer,
                Some(Pointer::Minimap { grab_rows: 1 })
            ));
            app.drag_minimap(area.y + 2, 1);
            assert_eq!(
                app.editor().viewport().top_row,
                7,
                "drag retains the grabbed row offset"
            );

            app.press_minimap(area.y + 2);
            assert_eq!(
                app.editor().viewport().top_row,
                7,
                "pressing the band preserves its position"
            );
        }
    }
}
