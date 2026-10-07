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
