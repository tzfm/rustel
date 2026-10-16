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

#[cfg(test)]
mod tests {
    mod replay_edit {
        //! Tests for replay edit.

        use crate::app::replay_edit::*;
        use rustel_runtime::session_log::{SessionRecorder, SessionScript};

        fn replay_app(directory: &std::path::Path) -> (App, SceneId) {
            let mut app = crate::app::tests::support::app_over(directory);
            let path = directory.join("blocks.rustel-session");
            let mut recorder =
                SessionRecorder::create(path.clone(), SessionMode::Normal, None).unwrap();
            for (at, source) in [(0.0, "s(\"bd\")"), (2.0, "s(\"sd\")"), (5.0, "s(\"hh\")")] {
                recorder.record_save(at, SaveStatus::Installed, source, None);
            }
            recorder.record_save_via(9.0, SaveStatus::Installed, "silence", None, Some("stop"));
            drop(recorder);
            app.frame = Rect::new(0, 0, 140, 40);
            app.open_replay_tab(&path);
            app.focus_timeline();
            let id = app.scenes.current().id;
            (app, id)
        }

        fn press(app: &mut App, code: KeyCode) {
            app.handle_terminal_event(Event::Key(KeyEvent::new(code, KeyModifiers::NONE)))
                .unwrap();
        }

        #[test]
        fn exact_duration_seconds_and_cycles_persist_without_rounding_other_blocks() {
            let directory = tempfile::tempdir().unwrap();
            let (mut app, id) = replay_app(directory.path());
            let source = app.editor().source();
            press(&mut app, KeyCode::Char('t'));
            assert!(app.replay_edit.is_some());
            app.handle_terminal_event(Event::Paste("0.03125".into()))
                .unwrap();
            press(&mut app, KeyCode::Enter);
            assert!(app.replay_edit.is_none());
            let tab = ReplayTab::open(&app.replays[&id].path).unwrap();
            assert_eq!(tab.duration_of(0), Some(0.03125));
            assert_eq!(tab.duration_of(1), Some(3.0));
            assert_eq!(tab.duration_of(2), Some(4.0));
            assert_eq!(app.editor().source(), source);
            app.handle_terminal_event(Event::Key(KeyEvent::new_with_kind(
                KeyCode::Enter,
                KeyModifiers::NONE,
                KeyEventKind::Repeat,
            )))
            .unwrap();
            assert_eq!(
                app.editor().source(),
                source,
                "held confirmation cannot type a newline"
            );
            app.options.session.cps = 2.0;
            app.load_replay_block(id, 2);
            app.focus_timeline();
            press(&mut app, KeyCode::Char('t'));
            press(&mut app, KeyCode::Tab);
            assert!(
                app.replay_edit
                    .as_ref()
                    .unwrap()
                    .duration
                    .as_ref()
                    .unwrap()
                    .cycles
            );
            app.handle_terminal_event(Event::Paste("6".into())).unwrap();
            press(&mut app, KeyCode::Enter);
            let tab = ReplayTab::open(&app.replays[&id].path).unwrap();
            assert_eq!(tab.duration_of(2), Some(3.0));
        }

        #[test]
        fn delete_confirmation_owns_paste_and_can_remove_the_last_remaining_block() {
            let directory = tempfile::tempdir().unwrap();
            let (mut app, id) = replay_app(directory.path());
            app.load_replay_block(id, 1);
            app.focus_timeline();
            let source = app.editor().source();
            press(&mut app, KeyCode::Delete);
            assert!(
                app.accepts_raw_paste(),
                "legacy pasted Enter is collected before confirmation"
            );
            app.handle_terminal_event(Event::Paste("delete\nEnter\n".into()))
                .unwrap();
            assert_eq!(app.replays[&id].events.len(), 3);
            assert!(app.replay_edit.is_some());
            assert_eq!(app.editor().source(), source);
            press(&mut app, KeyCode::Esc);
            assert_eq!(app.replays[&id].events.len(), 3);
            for remaining in [2, 1, 0] {
                app.focus_timeline();
                press(&mut app, KeyCode::Delete);
                press(&mut app, KeyCode::Enter);
                assert!(app.replay_edit.is_none());
                assert_eq!(app.replays[&id].events.len(), remaining);
            }
            assert!(app.editor().source().is_empty());
            assert!(
                ReplayTab::open(&app.replays[&id].path)
                    .unwrap()
                    .events
                    .is_empty()
            );
            assert!(
                SessionScript::load(&app.replays[&id].path).is_err(),
                "CLI replay still needs a block"
            );
        }

        #[test]
        fn duration_prompt_rejects_invalid_paste_and_never_evaluates_enter() {
            let directory = tempfile::tempdir().unwrap();
            let (mut app, id) = replay_app(directory.path());
            press(&mut app, KeyCode::Char('t'));
            assert!(app.accepts_raw_paste());
            app.handle_terminal_event(Event::Paste("9\nthrow Error()".into()))
                .unwrap();
            assert_eq!(
                app.replay_edit
                    .as_ref()
                    .unwrap()
                    .duration
                    .as_ref()
                    .unwrap()
                    .text,
                "2"
            );
            app.handle_terminal_event(Event::Paste("-3".into()))
                .unwrap();
            press(&mut app, KeyCode::Enter);
            assert!(app.replay_edit.as_ref().unwrap().error.is_some());
            assert_eq!(app.replays[&id].duration_of(0), Some(2.0));
            assert!(app.pending_evaluation.is_none() && app.inflight.is_empty());
            assert!(app.replays[&id].run.is_none());
        }

        #[test]
        fn failed_block_write_keeps_the_original_model_and_error_paths_private() {
            let directory = tempfile::tempdir().unwrap();
            let (mut app, id) = replay_app(directory.path());
            let blocked = directory.path().join("private-parent-file");
            std::fs::write(&blocked, "not a directory").unwrap();
            app.replays.get_mut(&id).unwrap().path = blocked.join("tape.rustel-session");
            press(&mut app, KeyCode::Delete);
            press(&mut app, KeyCode::Enter);
            assert_eq!(app.replays[&id].events.len(), 3);
            let error = app.replay_edit.as_ref().unwrap().error.as_ref().unwrap();
            assert!(!error.contains(&directory.path().display().to_string()));
            assert!(
                app.log
                    .lines()
                    .iter()
                    .any(|line| line.text.contains("private-parent-file"))
            );
        }

        #[test]
        fn block_edit_stops_its_replay_run_and_only_the_audio_that_replay_owns() {
            for audible in [true, false] {
                let directory = tempfile::tempdir().unwrap();
                let (mut app, id) = replay_app(directory.path());
                let other = app.replay_view.as_ref().unwrap().current;
                app.audible_scene = Some(if audible { id } else { other });
                app.installed_revision = Some("sounding".into());
                app.stop_requested = false;
                app.replays.get_mut(&id).unwrap().start(0, Instant::now());
                press(&mut app, KeyCode::Delete);
                press(&mut app, KeyCode::Enter);
                assert!(app.replays[&id].run.is_none());
                assert_eq!(app.stop_requested, audible);
                assert_eq!(app.installed_revision.is_none(), audible);
            }
        }

        #[test]
        fn live_and_debug_tapes_refuse_block_editing() {
            let directory = tempfile::tempdir().unwrap();
            let (mut app, id) = replay_app(directory.path());
            let original = app.replays[&id].path.clone();
            app.replays.get_mut(&id).unwrap().keeps = "all-saves-and-diagnostics".into();
            press(&mut app, KeyCode::Delete);
            assert!(app.replay_edit.is_none());
            // A refusal, in the footer's error colour, not a remark in the status
            // line's: pressing Delete again has to look different from pressing it
            // the first time.
            let refusal = app.errors.visible().expect("a refusal is an error");
            assert!(refusal.contains("debug"), "{refusal}");
            app.replays.get_mut(&id).unwrap().keeps = "installed-saves".into();
            let live = directory.path().join("live.rustel-session");
            app.recorder =
                Some(SessionRecorder::create(live.clone(), SessionMode::Normal, None).unwrap());
            app.replays.get_mut(&id).unwrap().path = live;
            press(&mut app, KeyCode::Char('t'));
            assert!(app.replay_edit.is_none());
            let refusal = app.errors.visible().expect("a refusal is an error");
            assert!(refusal.contains("still recording"), "{refusal}");
            assert_eq!(ReplayTab::open(&original).unwrap().events.len(), 3);
        }

        #[test]
        fn final_duration_expiry_stops_only_its_audible_replay() {
            for audible in [true, false] {
                let directory = tempfile::tempdir().unwrap();
                let (mut app, id) = replay_app(directory.path());
                let other = app.replay_view.as_ref().unwrap().current;
                app.audible_scene = Some(if audible { id } else { other });
                app.installed_revision = Some("sounding".into());
                app.stop_requested = false;
                let tab = app.replays.get_mut(&id).unwrap();
                tab.set_duration_exact(2, 0.01).unwrap();
                tab.start(2, Instant::now() - Duration::from_secs(1));
                app.tick_replays();
                assert!(app.replays[&id].run.is_none());
                assert_eq!(app.stop_requested, audible);
                assert_eq!(app.installed_revision.is_none(), audible);
                assert!(app.pending_evaluation.is_none() && app.inflight.is_empty());
            }
        }

        #[test]
        fn duration_field_shows_selection_and_keeps_the_editing_cursor_visible() {
            use ratatui::{Terminal, backend::TestBackend};
            let directory = tempfile::tempdir().unwrap();
            let (mut app, _) = replay_app(directory.path());
            app.open_replay_edit(true);
            let mut state = app.replay_edit.take().unwrap();
            let text = "123456789012345678901234567890123456789012345678901234567890";
            let entry = state.duration.as_mut().unwrap();
            entry.text = text.into();
            entry.cursor = 48;
            let mut terminal = Terminal::new(TestBackend::new(28, 9)).unwrap();
            terminal
                .draw(|frame| state.render(frame, &app.theme, true))
                .unwrap();
            let area = geometry(Rect::new(0, 0, 28, 9));
            let width = area.width - 4;
            let buffer = terminal.backend().buffer();
            let cell = buffer.cell((area.x + 2, area.y + 2)).unwrap();
            assert_eq!(cell.bg, app.theme.selection);
            assert_eq!(cell.fg, app.theme.selection_text);
            let visible = (area.x + 2..area.x + 2 + width)
                .map(|x| buffer.cell((x, area.y + 2)).unwrap().symbol())
                .collect::<String>();
            let start = 48 - (usize::from(width) - 1);
            assert_eq!(visible, text[start..start + usize::from(width)]);
            let cursor = terminal.backend().cursor_position();
            assert_eq!(cursor.x, area.x + 2 + width - 1);
            assert_eq!(cursor.y, area.y + 2);
        }
    }
}
