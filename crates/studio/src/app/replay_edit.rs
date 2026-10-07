//! Exact block timing and confirmed deletion, independent of timeline dragging.

use super::*;
use ratatui::style::Style;
use ratatui::widgets::{Block, Borders, Clear, Paragraph};

#[derive(Clone, Debug)]
pub(super) struct ReplayEdit {
    scene: SceneId,
    index: usize,
    pub(super) duration: Option<DurationEntry>,
    pub(super) error: Option<String>,
}

#[derive(Clone, Debug)]
pub(super) struct DurationEntry {
    pub(super) text: String,
    pub(super) cursor: usize,
    selected: bool,
    pub(super) cycles: bool,
    cps: f64,
}

impl DurationEntry {
    fn insert(&mut self, text: &str) {
        let text = text.trim();
        if text.is_empty()
            || text.len() > 64
            || !text
                .chars()
                .all(|ch| ch.is_ascii_digit() || matches!(ch, '.' | '+' | '-' | 'e' | 'E'))
        {
            return;
        }
        if self.selected {
            self.text.clear();
            self.cursor = 0;
            self.selected = false;
        }
        if self.text.len() + text.len() <= 64 {
            self.text.insert_str(self.cursor, text);
            self.cursor += text.len();
        }
    }

    fn seconds(&self) -> Option<f64> {
        let number = self.text.parse::<f64>().ok()?;
        let seconds = if self.cycles {
            number / self.cps
        } else {
            number
        };
        (seconds.is_finite() && seconds > 0.0 && seconds <= 86_400.0).then_some(seconds)
    }

    fn toggle_units(&mut self) {
        let seconds = self.seconds();
        self.cycles = !self.cycles;
        if let Some(seconds) = seconds {
            self.text = slider::format_value(if self.cycles {
                seconds * self.cps
            } else {
                seconds
            });
        }
        self.cursor = self.text.len();
        self.selected = true;
    }
}

pub(super) fn geometry(frame: Rect) -> Rect {
    let width = frame.width.saturating_sub(2).min(68);
    let height = frame.height.min(9);
    Rect::new(
        frame.x + (frame.width - width) / 2,
        frame.y + (frame.height - height) / 2,
        width,
        height,
    )
}

impl ReplayEdit {
    pub(super) fn render(
        &self,
        frame: &mut ratatui::Frame<'_>,
        theme: &Theme,
        cursor_visible: bool,
    ) {
        let area = geometry(frame.area());
        super::super::graphics::cover_images(area);
        frame.render_widget(Clear, area);
        frame.render_widget(
            Block::default()
                .borders(Borders::ALL)
                .title(if self.duration.is_some() {
                    " Block duration "
                } else {
                    " Delete block "
                })
                .style(Style::default().fg(theme.foreground).bg(theme.overlay)),
            area,
        );
        let mut line = |offset: u16, text: String, colour| {
            if area.height > offset + 1 {
                frame.render_widget(
                    Paragraph::new(text).style(Style::default().fg(colour).bg(theme.overlay)),
                    Rect::new(area.x + 2, area.y + offset, area.width.saturating_sub(4), 1),
                );
            }
        };
        line(
            1,
            format!(
                "Block {}{}",
                self.index + 1,
                self.duration
                    .as_ref()
                    .map(|entry| if entry.cycles {
                        " · cycles"
                    } else {
                        " · seconds"
                    })
                    .unwrap_or("")
            ),
            theme.foreground,
        );
        if let Some(entry) = &self.duration {
            line(
                3,
                format!(
                    "Cycles use current tempo: {} CPM",
                    slider::format_value(entry.cps * 60.0)
                ),
                theme.muted,
            );
            line(
                4,
                "Later blocks keep their durations; this replay stops.".into(),
                theme.muted,
            );
            line(
                6,
                "Tab units · Enter saves · Esc cancels".into(),
                theme.foreground,
            );
        } else {
            line(
                2,
                "Delete this block from the tape?".into(),
                theme.foreground,
            );
            line(
                3,
                "Later blocks move earlier; their durations stay the same.".into(),
                theme.muted,
            );
            line(
                4,
                "This replay stops. Other music keeps playing.".into(),
                theme.muted,
            );
            line(6, "Enter deletes · Esc cancels".into(), theme.foreground);
        }
        if let Some(error) = &self.error {
            line(7, error.clone(), theme.error);
        }
        if let Some(entry) = &self.duration
            && area.height > 3
        {
            let width = usize::from(area.width.saturating_sub(4));
            if width > 0 {
                let start = entry.cursor.saturating_sub(width - 1);
                let end = (start + width).min(entry.text.len());
                let style = if entry.selected {
                    Style::default()
                        .fg(theme.selection_text)
                        .bg(theme.selection)
                } else {
                    Style::default().fg(theme.accent).bg(theme.overlay)
                };
                frame.render_widget(
                    Paragraph::new(entry.text[start..end].to_owned()).style(style),
                    Rect::new(area.x + 2, area.y + 2, width as u16, 1),
                );
                if cursor_visible {
                    frame.set_cursor_position((
                        area.x + 2 + (entry.cursor - start) as u16,
                        area.y + 2,
                    ));
                }
            }
        }
    }
}

impl App {
    fn replay_edit_allowed(&self, id: SceneId) -> Result<(), &'static str> {
        let tab = self.replays.get(&id).ok_or("this replay is closed")?;
        if self.recording_path().as_deref() == Some(tab.path.as_path()) {
            return Err(
                "this tape is still recording - a block cannot be removed while it is being written · File ▸ New session finishes it",
            );
        }
        if !tab.writable() {
            return Err("debug tapes keep their diagnostics and cannot be edited");
        }
        if self
            .pending_evaluation
            .as_ref()
            .is_some_and(|pending| pending.scene == id)
            || self.inflight.values().any(|pending| pending.scene == id)
        {
            return Err("the replay is updating - try again when it finishes");
        }
        Ok(())
    }

    pub(super) fn open_replay_edit(&mut self, duration: bool) {
        let Some(id) = self.current_replay() else {
            return;
        };
        if let Err(error) = self.replay_edit_allowed(id) {
            // A refusal, not a remark. On the status line this read like
            // any other note about what the studio was doing, in the same
            // colour, and a player pressing Delete twice more had no way
            // to tell it had been heard and declined.
            self.set_error(ErrorOwner::Interface, error.to_owned());
            self.dirty_frame = true;
            return;
        }
        let tab = &self.replays[&id];
        if tab.events.is_empty() {
            self.status = "empty tape - type a score and update to add its first block".into();
            self.dirty_frame = true;
            return;
        }
        let index = tab.selected;
        let cps = self
            .visual
            .current_clock()
            .map(|clock| clock.2)
            .or_else(|| self.snapshot.as_ref().map(|snapshot| snapshot.cps))
            .filter(|cps| cps.is_finite() && *cps > 0.0)
            .unwrap_or(self.options.session.cps.max(f64::EPSILON));
        let entry = duration.then(|| {
            let text = tab
                .duration_of(index)
                .map(slider::format_value)
                .unwrap_or_default();
            DurationEntry {
                cursor: text.len(),
                text,
                selected: true,
                cycles: false,
                cps,
            }
        });
        self.stop_replay_drag();
        self.replay_edit = Some(ReplayEdit {
            scene: id,
            index,
            duration: entry,
            error: None,
        });
        self.slider_precision = None;
        self.dirty_frame = true;
        #[cfg(feature = "hydra")]
        self.sync_settings_webcam_preview();
    }

    pub(super) fn replay_edit_key(&mut self, key: &KeyEvent) -> bool {
        let Some(mut state) = self.replay_edit.take() else {
            return false;
        };
        if key.kind == KeyEventKind::Release {
            self.replay_edit = Some(state);
            return true;
        }
        let primary = key
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::SUPER);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        if key.code == KeyCode::Esc && key.kind == KeyEventKind::Press {
            self.status = "block edit cancelled".into();
        } else if key.code == KeyCode::Enter
            && key.modifiers.is_empty()
            && key.kind == KeyEventKind::Press
        {
            if let Err(error) = self.apply_replay_edit(&state) {
                state.error = Some(error);
                self.replay_edit = Some(state);
            }
        } else {
            if let Some(entry) = state.duration.as_mut() {
                match key.code {
                    KeyCode::Char('a' | 'A') if primary && !alt => entry.selected = true,
                    KeyCode::Char('v' | 'V')
                        if primary && !alt && key.kind == KeyEventKind::Press =>
                    {
                        if let Ok(text) = self.clipboard.get_text() {
                            entry.insert(&text);
                        }
                    }
                    KeyCode::Tab | KeyCode::BackTab
                        if !primary && !alt && key.kind == KeyEventKind::Press =>
                    {
                        entry.toggle_units()
                    }
                    KeyCode::Char(ch) if !primary && !alt => entry.insert(&ch.to_string()),
                    KeyCode::Backspace | KeyCode::Delete if !primary && !alt => {
                        if entry.selected {
                            entry.text.clear();
                            entry.cursor = 0;
                            entry.selected = false;
                        } else if key.code == KeyCode::Backspace && entry.cursor > 0 {
                            entry.cursor -= 1;
                            entry.text.remove(entry.cursor);
                        } else if key.code == KeyCode::Delete && entry.cursor < entry.text.len() {
                            entry.text.remove(entry.cursor);
                        }
                    }
                    KeyCode::Left if !primary && !alt => {
                        entry.cursor = entry.cursor.saturating_sub(1);
                        entry.selected = false;
                    }
                    KeyCode::Right if !primary && !alt => {
                        entry.cursor = (entry.cursor + 1).min(entry.text.len());
                        entry.selected = false;
                    }
                    KeyCode::Home if !primary && !alt => {
                        entry.cursor = 0;
                        entry.selected = false;
                    }
                    KeyCode::End if !primary && !alt => {
                        entry.cursor = entry.text.len();
                        entry.selected = false;
                    }
                    _ => {}
                }
                state.error = None;
            }
            self.replay_edit = Some(state);
        }
        self.dirty_frame = true;
        #[cfg(feature = "hydra")]
        self.sync_settings_webcam_preview();
        true
    }

    pub(super) fn replay_edit_paste(&mut self, text: &str) {
        if let Some(state) = self.replay_edit.as_mut()
            && let Some(entry) = state.duration.as_mut()
        {
            entry.insert(text);
            state.error = None;
        }
        self.dirty_frame = true;
    }

    /// While the block editor is open it has every key and every paste,
    /// and nothing reaches past it. `true` while it is open.
    pub(super) fn replay_edit_event(&mut self, terminal_event: &Event) -> bool {
        if self.replay_edit.is_some() {
            match terminal_event {
                Event::Key(key) => {
                    self.replay_edit_key(key);
                }
                Event::Paste(text) => self.replay_edit_paste(text),
                _ => {}
            }
            return true;
        }
        false
    }

    fn apply_replay_edit(&mut self, state: &ReplayEdit) -> Result<(), String> {
        self.replay_edit_allowed(state.scene)?;
        let mut candidate = self.replays[&state.scene].clone();
        if candidate.events.get(state.index).is_none() {
            return Err("this block is no longer present".into());
        }
        if state.duration.is_some() && self.current_replay() == Some(state.scene) {
            candidate.set_source(candidate.selected, &self.editor().source())?;
        }
        if let Some(entry) = &state.duration {
            let seconds = entry
                .seconds()
                .ok_or("enter a positive duration of at most 24 hours")?;
            candidate.set_duration_exact(state.index, seconds)?;
        } else {
            candidate.delete_event(state.index)?;
        }
        candidate.stop();
        if let Err(error) = candidate.save() {
            self.log.push(LogLevel::Error, "replay", error);
            return Err("could not save the tape; the original is kept - see the log".into());
        }
        self.stop_replay_audio(state.scene);
        let selected = candidate.selected;
        let path = candidate.path.clone();
        self.replays.insert(state.scene, candidate);
        if state.duration.is_none() && self.current_replay() == Some(state.scene) {
            self.load_replay_block(state.scene, selected);
        } else if let Some(scene) = self.scenes.get_mut(state.scene) {
            scene.saved_source_revision = source_revision(&scene.editor.source());
            scene.refresh_dirty();
        }
        self.focus = Focus::Timeline;
        self.refresh_set_panel(Some(path));
        self.status = if state.duration.is_some() {
            "block duration saved"
        } else {
            "block deleted"
        }
        .into();
        Ok(())
    }

    /// End only this replay. A score already sounding elsewhere is left on.
    pub(super) fn stop_replay_audio(&mut self, id: SceneId) {
        self.stop_replay_drag();
        if let Some(tab) = self.replays.get_mut(&id) {
            tab.stop();
        }
        if self.audible_scene == Some(id) {
            self.worker.request_stop();
            self.installed_revision = None;
            self.stop_requested = true;
            self.forget_arming();
        }
    }
}
