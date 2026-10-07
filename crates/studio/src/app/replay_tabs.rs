//! The replay tab: opening a recorded session tape as its own tab (the set's
//! panes are stashed until it closes), the block timeline above its editor, and
//! running the tape. Covers the timeline's keys, hit tests and scrollbar, a
//! click on a block and the wheel over the strip, loading a block into the
//! editor or playing from it, writing edits back to the tape, and the clock
//! that moves a running replay to its next block. Timeline drags live in
//! `replay_pointer.rs`, and block timing and deletion live in `replay_edit.rs`.

use super::*;

/// The set view as a tape found it, given back when the tape closes.
pub(super) struct SetStash {
    panes: Vec<Pane>,
    focused: usize,
    pub(super) current: SceneId,
    playing_pane: Option<usize>,
}

impl App {
    /// The replay tab on screen, if the current tab is one.
    pub(super) fn current_replay(&self) -> Option<SceneId> {
        let scene = self.scenes.current();
        scene.is_replay().then_some(scene.id)
    }

    /// Open a tape as a replay tab at the end of the strip, on its first
    /// block; one already open is selected.
    pub(super) fn open_replay_tab(&mut self, path: &std::path::Path) {
        if let Some(index) = self.scenes.replay_index(path) {
            self.select_scene(index);
            self.settle_focus();
            return;
        }
        let tab = match ReplayTab::open(path) {
            Ok(tab) => tab,
            Err(error) => {
                if let Some(panel) = self.set_panel.as_mut() {
                    panel.error = Some(error);
                }
                self.dirty_frame = true;
                return;
            }
        };
        let text = tab.selected_source().to_owned();
        // The set as it stands, to give back when the tape closes.
        let stash = SetStash {
            panes: self
                .panes
                .iter()
                .map(|pane| Pane {
                    scene: pane.scene,
                    last_map: None,
                })
                .collect(),
            focused: self.focused,
            current: self.scenes.current().id,
            playing_pane: self.playing_pane,
        };
        // One replay tab: choosing another tape replaces what it shows,
        // the run of the old one stopping with it.
        let opened = match self.scenes.replay_scene() {
            Some(id) => self.scenes.retarget_replay(id, path, &text).map(|()| id),
            None => self.scenes.open_replay(path, &text),
        };
        match opened {
            Ok(id) => {
                let blocks = tab.events.len();
                self.replays.insert(id, tab);
                self.rebind_replay_decorations(id);
                // The tape has the studio to itself: one pane, its own
                // strip; the set's panes wait in the stash.
                if self.replay_view.is_none() {
                    self.replay_view = Some(stash);
                    self.panes = vec![Pane {
                        scene: id,
                        last_map: None,
                    }];
                    self.focused = 0;
                    self.playing_pane = None;
                }
                self.settle_focus();
                self.land_on_current_scene();
                self.status = format!(
                    "replay - {blocks} saves · click a block or {} · {} plays from it · {} back to the set",
                    super::super::keybinds::shortcut_label("Alt+←/→"),
                    self.keybinds.hint(BindAction::Evaluate),
                    self.keybinds.hint(BindAction::CloseScene)
                );
            }
            Err(error) => self.set_error(ErrorOwner::Interface, error.to_string()),
        }
        self.dirty_frame = true;
    }

    /// Write a replay tab's edits back to its tape, where the tape can take
    /// them: not the one being written, whose record is the recorder's,
    /// and not a debug tape, whose diagnostics a rewrite would drop. Either
    /// keeps the edits in the tab and says so.
    pub(super) fn save_replay_tab(&mut self, id: SceneId) {
        let live = self.recording_path();
        let Some(tab) = self.replays.get_mut(&id) else {
            return;
        };
        if !tab.dirty {
            return;
        }
        if live.as_deref() == Some(tab.path.as_path()) {
            self.status =
                "the tape being written keeps its record - the edit lives in the tab".into();
            self.dirty_frame = true;
            return;
        }
        match tab.save() {
            Ok(()) => {
                let path = tab.path.display().to_string();
                self.log.push(
                    LogLevel::Info,
                    "replay",
                    format!("wrote the edit back to {path}"),
                );
            }
            Err(error) => {
                self.status = format!("{error} - the edit lives in the tab");
                self.dirty_frame = true;
            }
        }
    }

    /// The boundary under the pointer: a block's time on the ruler row, for
    /// every block but the first - the first starts the tape.
    pub(super) fn timeline_block_start_at(
        &self,
        x: u16,
        y: u16,
    ) -> Option<(usize, SceneId, usize)> {
        let (pane, id, block) = self.timeline_block_at(x, y)?;
        let strip = self.regions.panes[pane].timeline;
        (block > 0 && y == ReplayTab::ruler_row(strip)).then_some((pane, id, block))
    }

    /// The pane whose timeline scrollbar is under the pointer.
    pub(super) fn timeline_bar_at(&self, x: u16, y: u16) -> Option<usize> {
        let pane = self.pane_at(x, y)?;
        let strip = self.regions.panes[pane].timeline;
        if strip.is_empty() || !within(strip, x, y) {
            return None;
        }
        let (track, _, _) = self
            .replays
            .get(&self.panes[pane].scene)?
            .bar_geometry(strip.width)?;
        (y == ReplayTab::scrollbar_row(strip) && x - strip.x < track).then_some(pane)
    }

    /// Where along the thumb a press took it: on the thumb, the cell
    /// pressed; off it, its middle, so the thumb jumps to sit under the
    /// pointer and a drag from there keeps it there.
    pub(super) fn grab_timeline_thumb(&self, pane: usize, x: u16) -> (u16, bool) {
        let strip = self.regions.panes[pane].timeline;
        let id = self.panes[pane].scene;
        let Some((_, thumb_x, thumb_len)) = self
            .replays
            .get(&id)
            .and_then(|tab| tab.bar_geometry(strip.width))
        else {
            return (0, false);
        };
        let along = x.saturating_sub(strip.x);
        if along >= thumb_x && along < thumb_x + thumb_len {
            (along - thumb_x, true)
        } else {
            (thumb_len / 2, false)
        }
    }

    /// Scroll a pane's timeline so its thumb starts `grab` cells left of
    /// `x`: the press or drag on its bar.
    pub(super) fn scroll_timeline_to(&mut self, scene: SceneId, strip: Rect, x: u16, grab: u16) {
        if strip.width == 0 {
            return;
        }
        let thumb_x = x.saturating_sub(strip.x).saturating_sub(grab);
        if let Some(tab) = self.replays.get_mut(&scene) {
            tab.scroll_thumb_to(thumb_x, strip.width);
            self.dirty_frame = true;
        }
    }

    /// Close the replay tab on screen. Edits stay with the tab and go with
    /// it: the tape on disk is never rewritten.
    pub(super) fn close_replay(&mut self) {
        let id = self.scenes.current().id;
        match self.scenes.close_current() {
            Ok(_) => {
                self.replays.remove(&id);
                if self.focus == Focus::Timeline {
                    self.focus = Focus::Editor;
                }
                // The set comes back as it was left: its panes, the caret's
                // pane, and the scene the caret was on.
                if let Some(stash) = self.replay_view.take() {
                    self.panes = stash.panes;
                    self.focused = stash.focused.min(self.panes.len().saturating_sub(1));
                    self.playing_pane = stash.playing_pane;
                    if let Some(position) = self.scenes.index_of(stash.current) {
                        self.scenes.select(position);
                    }
                }
                self.reconcile_panes();
                self.strip_mode = SceneStripMode::Idle;
                self.rebuild_slider_spans();
                self.lint.remove(&id);
                self.live_sliders.remove(&id);
                self.readiness.remove(&id);
                self.invalidate_maps();
                self.status = format!(
                    "closed the replay - back to the set; the set panel ({}) opens it again",
                    self.shortcut_or_menu(BindAction::SetPanel, "View > Set panel")
                );
            }
            Err(error) => self.set_error(ErrorOwner::Interface, error.to_string()),
        }
        self.dirty_frame = true;
    }

    /// A tape's timeline takes the top of its pane, and the pane's editor
    /// the rows left, so the map, the view and the clicks agree. The
    /// regions the app keeps for its pointer are split too, so a click on
    /// a block does not go to the text under it.
    pub(super) fn split_replay_timelines(&mut self, regions: &mut view::StudioRegions) {
        for index in 0..self.panes.len().min(regions.panes.len()) {
            let scene_id = self.panes[index].scene;
            let region = &mut regions.panes[index];
            if self
                .scenes
                .get(scene_id)
                .is_some_and(|scene| scene.is_replay())
                && let Some((strip, rest)) = super::super::replay::split_timeline(region.editor)
            {
                region.timeline = strip;
                region.editor = rest;
                if let Some(tab) = self.replays.get_mut(&scene_id) {
                    // Drawing clamps the viewport but never follows an
                    // offscreen selection: that would undo every drag.
                    tab.scroll_by(0, strip.width);
                    tab.sync_thumbnails(strip);
                }
            }
        }
    }

    pub(super) fn timeline_visible(&self) -> bool {
        self.current_replay().is_some()
            && self
                .regions
                .panes
                .get(self.focused)
                .is_some_and(|pane| !pane.timeline.is_empty())
    }

    /// How many blocks the focused pane's timeline shows across, as last
    /// laid out - the screenful PgUp and PgDn move by. `None` while the
    /// focused pane has no timeline on screen.
    pub(super) fn timeline_page(&self) -> Option<usize> {
        let strip = self.regions.panes.get(self.focused)?.timeline;
        (strip.width > 0).then(|| ReplayTab::blocks_across(strip.width))
    }

    /// The timeline takes the keyboard: arrows choose a block from here.
    pub(super) fn focus_timeline(&mut self) {
        if self.current_replay().is_none() {
            return;
        }
        self.focus = Focus::Timeline;
        self.armed_slider = None;
        // Esc leaves the timeline; it does not leave the tape, and the two
        // are easy to confuse when the tape is the whole of what is on
        // screen. Say the one that gets you out.
        self.status = "tape timeline - Esc returns to the editor".into();
        self.dirty_frame = true;
    }

    /// A key while the timeline has the keyboard. Arrows and Home/End
    /// choose a block, whose code goes in the editor without playing;
    /// Enter plays from it; Esc gives the keyboard back to the text. Any
    /// other key goes to the text as well, and takes the keyboard with it.
    pub(super) fn handle_timeline_key(&mut self, code: KeyCode) -> Result<bool, RuntimeError> {
        let Some(id) = self.current_replay() else {
            self.focus = Focus::Editor;
            return Ok(false);
        };
        match code {
            KeyCode::Left => self.step_replay_block(-1),
            KeyCode::Right => self.step_replay_block(1),
            KeyCode::PageUp | KeyCode::PageDown => {
                let page = self.timeline_page().unwrap_or(1);
                if let Some(tab) = self.replays.get(&id) {
                    let next = page_selection(
                        tab.selected,
                        tab.events.len(),
                        page,
                        code == KeyCode::PageDown,
                    );
                    if next != tab.selected {
                        self.load_replay_block(id, next);
                    }
                }
            }
            KeyCode::Home => self.load_replay_block(id, 0),
            KeyCode::End => {
                let last = self
                    .replays
                    .get(&id)
                    .map_or(0, |tab| tab.events.len().saturating_sub(1));
                self.load_replay_block(id, last);
            }
            KeyCode::Enter => self.play_replay_from_selected()?,
            KeyCode::Delete | KeyCode::Backspace => self.open_replay_edit(false),
            KeyCode::Char('t' | 'T') => self.open_replay_edit(true),
            // Global function keys must see the original focus. In particular,
            // Shift+F10 continues past the timeline instead of returning to it.
            KeyCode::F(_) => return Ok(false),
            KeyCode::Esc => {
                self.focus = Focus::Editor;
                self.status = "back to the text".into();
                self.dirty_frame = true;
            }
            _ => {
                self.focus = Focus::Editor;
                return Ok(false);
            }
        }
        Ok(true)
    }

    /// A held Enter while the timeline has the keyboard is dropped.
    /// Confirming a block edit returns to the timeline, and the held
    /// Enter's repeat must not become a newline in its source editor.
    /// `true` when the event was one.
    pub(super) fn timeline_held_enter(&self, terminal_event: &Event) -> bool {
        self.focus == Focus::Timeline
            && matches!(terminal_event, Event::Key(key)
                if key.code == KeyCode::Enter && key.kind == KeyEventKind::Repeat
                    && key.modifiers.is_empty())
    }

    /// Put a block's code in its tab's editor, and choose it. The tab's
    /// text is the block's, so dirty means edited since the tape.
    pub(super) fn load_replay_block(&mut self, id: SceneId, index: usize) {
        let reveal_width = self
            .panes
            .iter()
            .position(|pane| pane.scene == id)
            .filter(|_| !self.replay_drag_active(id))
            .map(|pane| self.regions.panes[pane].timeline.width)
            .filter(|width| *width > 0);
        let Some(tab) = self.replays.get_mut(&id) else {
            return;
        };
        tab.select(index);
        if let Some(width) = reveal_width {
            tab.ensure_visible(width);
        }
        let source = tab.selected_source().to_owned();
        let Some(scene) = self.scenes.get_mut(id) else {
            return;
        };
        match super::super::editor::Editor::new(&source) {
            Ok(editor) => {
                scene.editor = editor;
                // The new document restarts at revision zero, the cache key.
                scene.minimap = Default::default();
                scene.saved_source_revision = source_revision(&source);
                scene.refresh_dirty();
            }
            Err(error) => {
                self.set_error(ErrorOwner::Interface, error.to_string());
                return;
            }
        }
        self.rebind_replay_decorations(id);
        self.armed_slider = None;
        self.invalidate_maps();
        self.lint_pending = true;
        self.dirty_frame = true;
    }

    /// Replacing a replay editor resets its revision counter. Source ranges
    /// from another block must never inherit that counter, including layouts
    /// still in flight. Returning to the exact evaluated text can rebind them.
    fn rebind_replay_decorations(&mut self, id: SceneId) {
        let Some(tab) = self.replays.get(&id) else {
            return;
        };
        let Some(scene) = self.scenes.get(id) else {
            return;
        };
        let block = ReplayBlock {
            path: tab.path.clone(),
            index: tab.selected,
        };
        let source = scene.editor.source();
        let revision = scene.editor.revision();
        for entry in self
            .evaluation_revisions
            .entries
            .values_mut()
            .filter(|entry| entry.scene == id)
        {
            entry.revision = (entry.replay_block.as_ref() == Some(&block)
                && entry.source.as_ref() == source)
                .then_some(revision);
        }
        self.live_sliders.remove(&id);
        if self.audible_scene == Some(id) {
            self.visual_revision = (self.visual_replay_block.as_ref() == Some(&block)
                && self.evaluated_source.as_deref() == Some(source.as_str()))
            .then_some(revision);
            self.pin_visual_revision();
            if let Some(revision) = self.visual_revision {
                self.install_virtual_rows(revision);
            }
            self.rebuild_slider_spans();
            self.refresh_decorations();
        }
    }

    /// Enter on the timeline, or Shift+click on a block: play from the
    /// chosen block, which is what ^S does in the tab.
    fn play_replay_from_selected(&mut self) -> Result<(), RuntimeError> {
        let revision = self.editor().revision();
        let source = self.editor().source().into();
        self.handle_editor_effect(EditorEffect::Evaluate { revision, source })
    }

    /// Alt+←/→: the block before or after the chosen one.
    pub(super) fn step_replay_block(&mut self, delta: isize) {
        let Some(id) = self.current_replay() else {
            return;
        };
        let Some(next) = self.replays.get(&id).and_then(|tab| {
            let count = tab.events.len() as isize;
            (count > 0).then(|| ((tab.selected as isize + delta).rem_euclid(count)) as usize)
        }) else {
            return;
        };
        self.load_replay_block(id, next);
    }

    /// The block under a pointer: the pane, the tab and the block.
    pub(super) fn timeline_block_at(&self, x: u16, y: u16) -> Option<(usize, SceneId, usize)> {
        let pane = self.pane_at(x, y)?;
        let strip = self.regions.panes[pane].timeline;
        if !within(strip, x, y) {
            return None;
        }
        let id = self.panes[pane].scene;
        let block = self.replays.get(&id)?.block_at(strip, x, y)?;
        Some((pane, id, block))
    }

    /// A press on a block of a replay's timeline puts its code up and gives
    /// the timeline the keyboard. Returns true when it took the press.
    pub(super) fn click_timeline_block(&mut self, x: u16, y: u16) -> bool {
        if let Some((pane, id, block)) = self.timeline_block_at(x, y) {
            if pane != self.focused {
                self.focus_pane(pane);
            }
            self.load_replay_block(id, block);
            // A press on a block chooses it and nothing else, and
            // Enter is what plays from it. The press does not arm a
            // pan: one gesture that both selected a block and slid
            // the timeline would move the block being clicked. The
            // scrollbar under the strip is how it pans.
            self.focus_timeline();
            self.pointer = Some(Pointer::Panel);
            return true;
        }
        false
    }

    /// Over a replay's timeline the wheel scrolls its blocks; `sideways`
    /// says the swipe went across rather than up or down. Whether it took
    /// the wheel.
    pub(super) fn scroll_timeline(
        &mut self,
        x: u16,
        y: u16,
        direction: f32,
        sideways: bool,
    ) -> bool {
        if let Some(index) = self.pane_at(x, y)
            && within(self.regions.panes[index].timeline, x, y)
        {
            let id = self.panes[index].scene;
            let width = self.regions.panes[index].timeline.width;
            if let Some(tab) = self.replays.get_mut(&id) {
                // The strip runs left to right, so a swipe that
                // pushes it leftwards shows what comes after. The
                // wheel keeps the list convention: down is later.
                let step = match (sideways, direction > 0.0) {
                    (true, true) | (false, false) => 1,
                    _ => -1,
                };
                tab.scroll_by(step, width);
                self.dirty_frame = true;
            }
            return true;
        }
        false
    }

    /// A stop stops every run: the tape stops walking, the sound stops.
    pub(super) fn stop_replays(&mut self) {
        for tab in self.replays.values_mut() {
            tab.stop();
        }
    }

    /// The run's clock: a block whose time is up gives way to the next,
    /// which loads into the tab and plays - whether or not the tab is on
    /// screen, the way the tape was recorded.
    pub(super) fn tick_replays(&mut self) {
        let now = Instant::now();
        let due: Vec<SceneId> = self
            .replays
            .iter()
            .filter(|(_, tab)| tab.due(now))
            .map(|(id, _)| *id)
            .collect();
        for id in due {
            let Some(next) = self.replays.get_mut(&id).and_then(|tab| tab.advance(now)) else {
                self.stop_replay_audio(id);
                self.status = "replay - the tape ended".into();
                self.dirty_frame = true;
                continue;
            };
            // The editor follows the run only while it was showing the
            // sounding block: a block the reader chose to read stays up,
            // and the run plays on beside it, from the tape rather than
            // the editor - under no revision of the editor's, so nothing
            // of the block sounding lands on the block being read.
            let Some((source, follows)) = self.replays.get(&id).map(|tab| {
                (
                    tab.events
                        .get(next)
                        .map(|event| event.source.clone())
                        .unwrap_or_default(),
                    tab.selected == next,
                )
            }) else {
                continue;
            };
            if follows {
                self.load_replay_block(id, next);
                let Some(scene) = self.scenes.get(id) else {
                    continue;
                };

                let revision = scene.editor.revision();
                let source = Arc::<str>::from(scene.editor.source());
                self.queue_evaluation_for(id, revision, source, false);
            } else {
                self.queue_evaluation_for(id, Revision(u64::MAX), source, false);
            }
        }
    }
}
