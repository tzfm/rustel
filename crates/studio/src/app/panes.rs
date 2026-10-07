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
        self.status = if self.toggle_chrome_row(|settings| &mut settings.show_menu) {
            "menu bar shown".into()
        } else if let Some(chord) = self.keybinds.binding(BindAction::Settings) {
            format!(
                "menu bar hidden - {} opens settings to bring it back",
                chord.hint()
            )
        } else {
            format!(
                "menu bar hidden - settings has no chord; show_menu in {} brings it back",
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
