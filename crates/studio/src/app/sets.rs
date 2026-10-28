//! Sets and the set panel: folders under the sets folder, the ^B panel
//! that lists a set's scores and tapes, and the prompts that make, open
//! and rename sets. The same prompt also asks for the sets and recordings
//! folders, a sample source, a bank's alias and a sample file's new name.

use super::super::set_panel;
use super::*;

impl App {
    /// ^B: the set panel, docked at the left when the terminal is wide
    /// enough and a sheet when it is not.
    ///
    /// A plain switch: ^B shows the panel or hides it. Outside zen, Esc
    /// gives the keyboard back and leaves the panel where it is, docked or
    /// as a sheet. In zen the panel is a popup, and Esc closes it.
    pub(super) fn toggle_set_panel(&mut self) {
        if self.set_panel.is_some() {
            self.hide_set_panel();
            return;
        }
        self.open_set_panel(true);
        self.prefs.set_panel = Some(true);
        self.save_prefs_soon();
    }

    /// Put the set panel away: ^B from inside it, and View ▸ Set panel,
    /// which is a ticked row and so turns off as readily as it turns on.
    fn hide_set_panel(&mut self) {
        if self.set_panel.take().is_none() {
            return;
        }
        self.close_set_prompt();
        self.invalidate_maps();
        self.prefs.set_panel = Some(false);
        self.save_prefs_soon();
        self.status = "set panel hidden".into();
        self.dirty_frame = true;
    }

    /// Which edge the set panel docks at, by the setting.
    pub(super) fn set_panel_side(&self) -> view::Side {
        if self.ui_settings.set_panel_right {
            view::Side::Right
        } else {
            view::Side::Left
        }
    }

    pub(super) fn set_sidebar(&self) -> Option<view::SetSidebar> {
        self.set_panel.as_ref().map(|panel| view::SetSidebar {
            side: self.set_panel_side(),
            width: panel.width,
        })
    }

    /// Show the set panel, with the keyboard or without it.
    pub(super) fn open_set_panel(&mut self, focus: bool) {
        let recording = self.recording_path();
        let sessions = self.sessions_directory();
        let mut panel = SetPanel::open(&self.scenes, &sessions, recording.as_deref());
        panel.on_right = self.ui_settings.set_panel_right;
        panel.width = self
            .prefs
            .set_panel_width
            .unwrap_or(set_panel::SIDEBAR_WIDTH)
            .clamp(set_panel::SIDEBAR_MIN_WIDTH, set_panel::SIDEBAR_MAX_WIDTH);
        self.set_panel = Some(panel);
        self.invalidate_maps();
        if focus {
            self.focus_panel(PanelKind::Set);
        }
        self.status = format!(
            "set - {} · Enter opens a score or a tape · +/- resizes · Del deletes · {} hides the panel",
            status_file_path(self.scenes.directory(), self.ui_settings.show_full_paths),
            self.shortcut_or_menu(BindAction::SetPanel, "View > Set panel")
        );
        self.dirty_frame = true;
    }

    /// Bring the panel's rows up to date with the folder, keeping the
    /// selection on `keep` - a score's or a tape's path - where it can.
    pub(super) fn refresh_set_panel(&mut self, keep: Option<PathBuf>) {
        let recording = self.recording_path();
        let sessions = self.sessions_directory();
        if let Some(panel) = self.set_panel.as_mut() {
            panel.refresh(
                &self.scenes,
                &sessions,
                recording.as_deref(),
                keep.as_deref(),
            );
        }
        // Whatever changed the folder's files changed what the memory
        // breakdown counts as closed.
        if self.log_panel.is_some() {
            self.memory_closed_files = self.closed_file_count();
        }
        self.dirty_frame = true;
    }

    /// The row a refresh should keep: the score or tape the reader has
    /// selected, so an update behind the scenes does not move the panel.
    pub(super) fn set_panel_selection(&self) -> Option<PathBuf> {
        self.set_panel.as_ref().and_then(|panel| {
            panel
                .selected_file()
                .map(|file| file.path.clone())
                .or_else(|| panel.selected_tape().map(|tape| tape.path.clone()))
        })
    }

    pub(super) fn set_reveal_target(&self) -> RevealTarget {
        if let Some(panel) = &self.set_panel {
            if let Some(file) = panel.selected_file() {
                return RevealTarget::File(file.path.clone());
            }
            if let Some(tape) = panel.selected_tape() {
                return RevealTarget::File(tape.path.clone());
            }
        }
        let sessions = self.sessions_directory();
        RevealTarget::Folder(if sessions.is_dir() {
            sessions
        } else {
            self.scenes.directory().to_path_buf()
        })
    }

    /// Every set row has a reveal target, including an empty sessions fold.
    pub(super) fn handle_set_file_key(&mut self, code: KeyCode) -> bool {
        if matches!(code, KeyCode::Char('o' | 'O')) {
            if let Some(panel) = self.set_panel.as_mut() {
                panel.deleting = None;
            }
            self.reveal_target(self.set_reveal_target());
            return true;
        }
        self.handle_session_file_key(code)
    }

    /// Whether the set panel's search box holds the keyboard.
    pub(super) fn set_search_typing(&self) -> bool {
        self.set_panel.as_ref().is_some_and(SetPanel::search_typing)
    }

    /// The set panel's keys. Returns true when the panel consumed the key.
    pub(super) fn handle_set_key(&mut self, code: KeyCode, primary: bool, shift: bool) -> bool {
        let where_it_stands = |app: &Self| {
            app.set_panel
                .as_ref()
                .map(|panel| (panel.selected, panel.lines.len()))
        };
        let before = where_it_stands(self);
        let handled = self.handle_set_key_inner(code, primary, shift);
        // A key that moved the selection, or opened or folded the tapes,
        // walks the list the keyboard's way, margin and all; one that only
        // asked to delete or opened a line leaves a clicked line where it is.
        if where_it_stands(self) != before
            && let Some(panel) = self.set_panel.as_mut()
        {
            panel.hold_scroll = false;
        }
        handled
    }

    fn handle_set_key_inner(&mut self, code: KeyCode, primary: bool, shift: bool) -> bool {
        if self.set_prompt.is_some() {
            // A prompt owns the keyboard: a key it has no use for goes
            // nowhere, rather than into the score under it. Global commands
            // that outrank a prompt were already handled before dispatch.
            let handled = self.handle_set_prompt_key(code, primary, shift);
            return handled || self.set_prompt.is_some();
        }
        // The search box owns the keyboard the same way: while it types,
        // every key is the box's.
        if self.set_search_typing() {
            if let Some(panel) = self.set_panel.as_mut() {
                panel.search_key(code);
            }
            // The box's row or the list it filters changed: the frame is
            // stale the moment the box takes a key. Nothing below marks
            // it, and a held Backspace that waits for the caret's blink
            // to show its work reads as lag.
            self.dirty_frame = true;
            return true;
        }
        // A set that could not go because a score could not be saved:
        // Enter retries, Del leaves without the score, Esc keeps the set.
        // Any other key withdraws the question and does its own thing.
        if let Some(panel) = self.set_panel.as_mut()
            && panel.unsaved.take().is_some()
        {
            match code {
                KeyCode::Enter if !primary => {
                    self.retry_switch();
                    return true;
                }
                KeyCode::Delete => {
                    self.leave_unsaved();
                    return true;
                }
                KeyCode::Esc => {
                    self.pending_switch = None;
                    self.status = "this set stays open".into();
                    self.dirty_frame = true;
                    return true;
                }
                _ => self.pending_switch = None,
            }
        }
        let vim = super::vim::applies(self);
        let Some(panel) = self.set_panel.as_mut() else {
            return false;
        };
        // A delete waiting to be confirmed: Enter deletes, anything else
        // keeps - Esc says so, and any other key goes on to do its own
        // thing with the question withdrawn.
        if panel.deleting == Some(panel.selected) {
            match code {
                KeyCode::Enter if !primary => {
                    self.confirm_delete_set_line();
                    self.dirty_frame = true;
                    return true;
                }
                KeyCode::Esc => {
                    panel.deleting = None;
                    panel.error = None;
                    self.status = "kept".into();
                    self.dirty_frame = true;
                    return true;
                }
                _ => panel.deleting = None,
            }
        }
        match code {
            // A filter standing drops on Esc, before the panel's own Esc
            // hands the keyboard back. The box's vim Insert stands down
            // with it: nothing types into a box that is no longer there.
            KeyCode::Esc if panel.search.is_some() => {
                panel.search = None;
                panel.vim_insert = false;
                panel.refilter();
            }
            // Zen has no docked furniture: the panel came up as a popup,
            // not the set's file tree standing beside the score, so Esc
            // closes it outright the way every other zen popup does -
            // rather than merely handing the keyboard back the way it
            // does docked, which would leave it standing with no ^B in
            // sight to put it away again.
            KeyCode::Esc if self.ui_settings.zen => {
                self.hide_set_panel();
                return true;
            }
            // The keyboard goes back to the score and the panel stays,
            // docked or as a sheet. The set panel is not a dialog to answer
            // and dismiss, and a narrow terminal is no reason for it to
            // close on a key not aimed at it. ^B hides it.
            KeyCode::Esc => {
                panel.error = None;
                self.focus = Focus::Editor;
                self.status = format!(
                    "back to the score - the set panel stays; {} hides it",
                    self.shortcut_or_menu(BindAction::SetPanel, "View > Set panel")
                );
            }
            KeyCode::Up => panel.move_by(-1),
            KeyCode::Down => panel.move_by(1),
            KeyCode::PageUp | KeyCode::PageDown => {
                let rows = panel
                    .list_area(self.regions.sidebar, self.frame)
                    .map_or(1, |list| list.height.max(1)) as isize;
                panel.step(if code == KeyCode::PageUp { -rows } else { rows });
            }
            KeyCode::Home => {
                panel.selected = 0;
                panel.error = None;
            }
            KeyCode::End => {
                panel.selected = panel.lines.len().saturating_sub(1);
                panel.error = None;
            }
            KeyCode::Right if !primary => {
                if panel.selected_line() == Some(SetLine::Sessions) && !panel.sessions_open {
                    panel.toggle_sessions();
                }
            }
            KeyCode::Left if !primary => {
                if matches!(
                    panel.selected_line(),
                    Some(SetLine::Sessions | SetLine::Tape(_))
                ) && panel.sessions_open
                {
                    panel.toggle_sessions();
                }
            }
            KeyCode::Enter if !primary => self.open_set_line(),
            // A strip number opens its score the way Enter on its row does.
            // The numbers run to nine; a strip past that stays on the arrows.
            KeyCode::Char(digit @ '1'..='9') if !primary => {
                let strip = (digit as u8 - b'0') as usize;
                match panel
                    .files
                    .iter()
                    .position(|file| file.strip == Some(strip))
                {
                    Some(row) => {
                        panel.selected = panel
                            .lines
                            .iter()
                            .position(|line| *line == SetLine::File(row))
                            .unwrap_or(panel.selected);
                        self.open_set_line();
                    }
                    None => self.status = format!("no scene {} on the strip", strip),
                }
            }
            KeyCode::Char(' ') if !primary => panel.toggle_sessions(),
            KeyCode::Char('+' | '=' | '-' | '_') if !primary => {
                let grow = matches!(code, KeyCode::Char('+' | '='));
                if panel.resize(grow) {
                    self.prefs.set_panel_width = Some(panel.width);
                    self.status = format!("set panel · {} columns", panel.width);
                    self.save_prefs_soon();
                    self.invalidate_maps();
                } else {
                    self.status = format!(
                        "set panel is as {} as it goes",
                        if grow { "wide" } else { "narrow" }
                    );
                }
            }
            // `/` opens the search box: it takes the keyboard, and the
            // list hides the scores the query does not answer. On a
            // filter standing it takes the box back up to type.
            KeyCode::Char('/') if !primary && panel.search.is_none() => {
                panel.open_search();
                // In Vim the box opens in Insert, the way Vim's own
                // search does. The panel cannot see the editor's
                // modes, so the arm that can arms it.
                if vim {
                    panel.vim_insert = true;
                }
            }
            KeyCode::Char('/') if !primary => {
                if let Some(search) = panel.search.as_mut() {
                    search.typing = true;
                }
            }
            // While a filter stands, Backspace takes the box back up and
            // deletes from the query. The key a hand reaches for to fix a
            // query must never arm a delete of the file under it - a
            // second press would confirm it. Delete keeps that arm.
            KeyCode::Backspace if panel.search.is_some() => panel.resume_search(),
            // The key the desk has for it: Delete, or the Mac's delete,
            // which a terminal sends as Backspace.
            KeyCode::Delete | KeyCode::Backspace => self.ask_delete_set_line(),
            _ => return false,
        }
        self.dirty_frame = true;
        true
    }

    /// Enter, or a double click, on a line: a score opens as a tab, one
    /// already open is selected, the sessions fold opens or shuts, and a
    /// tape opens as the replay view.
    fn open_set_line(&mut self) {
        let Some(line) = self.set_panel.as_ref().and_then(SetPanel::selected_line) else {
            return;
        };
        match line {
            SetLine::File(_) => {
                let Some(path) = self
                    .set_panel
                    .as_ref()
                    .and_then(SetPanel::selected_file)
                    .map(|file| file.path.clone())
                else {
                    return;
                };
                // A tape has the studio to itself: a score gives the set
                // back first.
                if self.replay_view.is_some() {
                    self.close_replay();
                }
                // A score already on the strip is selected: the focused
                // pane shows it, or the caret goes to the pane with the
                // score. Opening it again would move the set's current
                // scene under the panes without moving them.
                let open = self
                    .scenes
                    .scores()
                    .find(|scene| scene.path == path)
                    .map(|scene| scene.id)
                    .and_then(|id| self.scenes.index_of(id));
                if let Some(index) = open {
                    self.select_scene(index);
                    self.refresh_set_panel(Some(path));
                    self.focus = Focus::Editor;
                    self.dirty_frame = true;
                    return;
                }
                match self.scenes.open_file(&path) {
                    Ok(id) => {
                        let index = self.scenes.index_of(id).unwrap_or(0);
                        self.land_on_current_scene();
                        self.persist_manifest();
                        self.status =
                            format!("scene {} - {}", index + 1, self.scenes.current().name());
                        self.refresh_set_panel(Some(path));
                        // Opened, the score has the keyboard; the panel stays.
                        self.focus = Focus::Editor;
                    }
                    Err(error) => {
                        let message = error.to_string();
                        self.refresh_set_panel(Some(path));
                        if let Some(panel) = self.set_panel.as_mut() {
                            panel.error = Some(message);
                        }
                    }
                }
            }
            SetLine::Sessions => {
                if let Some(panel) = self.set_panel.as_mut() {
                    panel.toggle_sessions();
                }
            }
            SetLine::Tape(_) => {
                let Some(path) = self
                    .set_panel
                    .as_ref()
                    .and_then(SetPanel::selected_tape)
                    .map(|tape| tape.path.clone())
                else {
                    return;
                };
                self.open_replay_tab(&path);
                self.refresh_set_panel(Some(path));
                if self.current_replay().is_some() {
                    self.focus = Focus::Editor;
                }
            }
        }
        self.dirty_frame = true;
    }

    /// An Alt digit selects that strip's score, from one, the way the
    /// digit on its row does: only a score already on the strip answers,
    /// and the editor takes the keyboard back.
    pub(super) fn select_strip_by_number(&mut self, strip: usize) {
        let Some(id) = self.scenes.scores().nth(strip - 1).map(|scene| scene.id) else {
            self.status = format!("no scene {strip} on the strip");
            self.dirty_frame = true;
            return;
        };
        if let Some(index) = self.scenes.index_of(id)
            && self.select_scene(index)
        {
            self.refresh_set_panel(self.scenes.get(id).map(|scene| scene.path.clone()));
            self.focus = Focus::Editor;
            self.dirty_frame = true;
        }
    }

    /// What the chosen line names, and its path; whether it is the tape
    /// being written. None on the sessions fold.
    fn set_line_target(&self) -> Option<(String, PathBuf, bool)> {
        let panel = self.set_panel.as_ref()?;
        match panel.selected_line()? {
            SetLine::File(index) => panel
                .files
                .get(index)
                .map(|file| (file.name.clone(), file.path.clone(), false)),
            SetLine::Tape(index) => panel
                .tapes
                .get(index)
                .map(|tape| (tape.name.clone(), tape.path.clone(), tape.recording)),
            SetLine::Sessions => None,
        }
    }

    /// Open the set panel, or focus it when it is open, to ask a question.
    fn raise_set_panel(&mut self) {
        if self.set_panel.is_none() {
            self.open_set_panel(true);
            self.prefs.set_panel = Some(true);
            self.save_prefs_soon();
        } else {
            self.focus_panel(PanelKind::Set);
        }
    }

    /// Delete on a line asks: the panel names what would go, and Enter
    /// lets it, Esc keeps it. The tape being written cannot go.
    fn ask_delete_set_line(&mut self) {
        let Some((name, _, recording)) = self.set_line_target() else {
            return;
        };
        if recording {
            self.refuse_live_tape_delete();
            return;
        }
        if let Some(panel) = self.set_panel.as_mut() {
            panel.deleting = Some(panel.selected);
            panel.error = None;
        }
        self.status = format!("delete {name} from the disk? Enter deletes it · Esc keeps it");
        self.dirty_frame = true;
    }

    fn refuse_live_tape_delete(&mut self) {
        if let Some(panel) = self.set_panel.as_mut() {
            panel.deleting = None;
            panel.error = Some("recording · press n for a new tape".into());
        }
        self.status = "cannot delete the tape being written · n starts a new tape first".into();
        self.dirty_frame = true;
    }

    /// Enter on the question: a score off the disk, and off the strip when
    /// open; a tape off the disk. The last score of a set stays.
    pub(super) fn confirm_delete_set_line(&mut self) {
        // Only the armed line goes. The question is what makes a delete a
        // delete, and holding that invariant here rather than at the one
        // key that asks means a second way in cannot skip it.
        if !self
            .set_panel
            .as_ref()
            .is_some_and(|panel| panel.deleting == Some(panel.selected))
        {
            return;
        }
        let Some((_, path, recording)) = self.set_line_target() else {
            return;
        };
        let line = self.set_panel.as_ref().and_then(SetPanel::selected_line);
        if recording {
            self.refuse_live_tape_delete();
            return;
        }
        // Resolved before the delete, which takes the tab off the strip.
        let open = self
            .scenes
            .scores()
            .find(|scene| scene.path == path)
            .map(|scene| scene.id);
        let result = match line {
            Some(SetLine::File(_)) => self.scenes.delete_file(&path).map(|_| ()),
            _ => match std::fs::remove_file(&path) {
                Ok(()) => Ok(()),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
                Err(error) => Err(SceneError::Io(error)),
            },
        };
        match result {
            Ok(()) => {
                if matches!(line, Some(SetLine::File(_))) {
                    if let Some(id) = open {
                        self.forget_scene(id);
                    }
                    self.reconcile_panes();
                    self.land_on_current_scene();
                    self.persist_manifest();
                }
                self.status = format!(
                    "deleted {}",
                    status_file_path(&path, self.ui_settings.show_full_paths)
                );
                self.refresh_set_panel(None);
                // The list stays where it was: the row that took the
                // deleted one's place, not the top of the panel.
                if let (Some(line), Some(panel)) = (line, self.set_panel.as_mut()) {
                    panel.select_after_delete(line);
                }
            }
            Err(error) => {
                let message = error.to_string();
                self.refresh_set_panel(Some(path));
                if let Some(panel) = self.set_panel.as_mut() {
                    panel.error = Some(message);
                }
            }
        }
    }

    /// Scene > Delete scene: the set panel, on this scene, with the
    /// question up - Enter deletes it, Esc keeps it.
    pub(super) fn arm_delete_scene(&mut self) {
        if self.current_prebake().is_some() || self.scenes.current().is_replay() {
            self.status = "only a score can be deleted".into();
            self.dirty_frame = true;
            return;
        }
        let path = self.scenes.current().path.clone();
        self.raise_set_panel();
        self.refresh_set_panel(Some(path.clone()));
        // The refresh can only land on this scene when the scene is one of
        // the panel's rows. A score that has never been written is not -
        // `rustel studio newidea.strudel` opens a starter scene the folder
        // has never seen - and the panel then falls back to its first line.
        // Arming that would put another score's name in the question and
        // delete it on Enter, so the menu says no instead.
        let landed = matches!(
            self.set_panel.as_ref().and_then(SetPanel::selected_line),
            Some(SetLine::File(_))
        ) && self
            .set_line_target()
            .is_some_and(|(_, target, _)| target == path);
        if landed {
            self.ask_delete_set_line();
            return;
        }
        let message = format!(
            "{} is not a file of this set - nothing to delete",
            self.scenes.current().name()
        );
        if let Some(panel) = self.set_panel.as_mut() {
            panel.error = Some(message.clone());
        }
        self.status = message;
        self.dirty_frame = true;
    }

    /// A click on a set prompt, which is a sheet and takes the press
    /// ahead of the panels. Returns true when it did.
    pub(super) fn click_set_prompt(&mut self, x: u16, y: u16, shift: bool) -> bool {
        let frame = self.frame;
        if let Some((prompt, picker)) = self.set_prompt.as_mut() {
            if let Some(at) = picker.field_at(frame, x, y) {
                let double = picker.click_field(at, (x, y), shift, Instant::now());
                self.focus_panel(PanelKind::Set);
                self.pointer = Some(if double {
                    Pointer::Panel
                } else {
                    Pointer::PromptField
                });
                self.dirty_frame = true;
                return true;
            }
            if let Some(row) = picker.row_at(frame, x, y) {
                let double = if matches!(prompt, SetPrompt::OpenSet | SetPrompt::OpenRecent) {
                    picker.click_row(row, Instant::now())
                } else {
                    picker.select_row(row);
                    false
                };
                self.focus_panel(PanelKind::Set);
                self.pointer = Some(Pointer::Panel);
                self.dirty_frame = true;
                if double {
                    self.handle_set_prompt_key(KeyCode::Enter, false, false);
                }
                return true;
            }
            picker.cancel_click();
            if picker.contains(frame, x, y) {
                self.focus_panel(PanelKind::Set);
                self.pointer = Some(Pointer::Panel);
                return true;
            }
        }
        false
    }

    /// A click on the set panel, docked or as a sheet, below every
    /// dialog. Returns true when it took the press.
    pub(super) fn click_set_panel(&mut self, x: u16, y: u16) -> bool {
        if self.regions.sidebar_hidden {
            return false;
        }
        let frame = self.frame;
        let sidebar = self.regions.sidebar;
        if let Some(panel) = self.set_panel.as_mut() {
            if let Some(line) = panel.row_at(sidebar, frame, x, y) {
                let double = panel.click(line, Instant::now());
                let fold = panel.selected_line() == Some(SetLine::Sessions);
                self.focus_panel(PanelKind::Set);
                self.pointer = Some(Pointer::Panel);
                self.dirty_frame = true;
                // The fold opens on one click; a score or a tape on two.
                if fold || double {
                    self.open_set_line();
                }
                return true;
            }
            if panel.contains(sidebar, frame, x, y) {
                self.focus_panel(PanelKind::Set);
                self.pointer = Some(Pointer::Panel);
                return true;
            }
        }
        false
    }

    /// The wheel over the set panel walks its lines. Returns true when
    /// the panel took the scroll.
    pub(super) fn scroll_set_panel(&mut self, x: u16, y: u16, direction: f32) -> bool {
        if self.regions.sidebar_hidden {
            return false;
        }
        let (frame, sidebar) = (self.frame, self.regions.sidebar);
        let Some(panel) = self.set_panel.as_mut() else {
            return false;
        };
        if !panel.contains(sidebar, frame, x, y) {
            return false;
        }
        panel.step(if direction > 0.0 { -1 } else { 1 });
        self.dirty_frame = true;
        true
    }

    /// Where new sets are made.
    pub(super) fn sets_root(&self) -> PathBuf {
        self.prefs.sets_directory().unwrap_or_else(|| {
            self.scenes
                .directory()
                .parent()
                .map(std::path::Path::to_path_buf)
                .unwrap_or_else(|| PathBuf::from("."))
        })
    }

    /// The folder a set writes into (`sessions/`, `exports/`) that `path`
    /// is or lies inside, when that folder belongs to the open set or to
    /// any folder holding a set file or a score.
    pub(super) fn set_output_folder(&self, path: &std::path::Path) -> Option<PathBuf> {
        use super::super::prefs::{names_below, same_file_name};
        let is_output = |name: &std::ffi::OsStr| {
            let name = name.to_string_lossy();
            rustel_runtime::product::SET_OUTPUT_DIRECTORY_NAMES
                .iter()
                .any(|output| same_file_name(&name, output))
        };
        let is_open = |set: &std::path::Path| {
            names_below(self.scenes.directory(), set).is_some_and(|below| below.is_empty())
        };
        std::path::absolute(path)
            .unwrap_or_else(|_| path.to_path_buf())
            .ancestors()
            .find(|folder| {
                folder.file_name().is_some_and(is_output)
                    && folder
                        .parent()
                        .is_some_and(|set| is_open(set) || is_set_folder(set))
            })
            .map(std::path::Path::to_path_buf)
    }

    /// The sets folder as the settings row reads it.
    pub(super) fn sets_folder_label(&self) -> String {
        shorten_home(&self.sets_root())
    }

    /// The recordings folder as the settings row reads it.
    pub(super) fn recordings_folder_label(&self) -> String {
        shorten_home(&self.recordings_directory())
    }

    /// File > New set: a folder named after the day under the sets
    /// folder, with a starter scene, taken up at once. Studio asks only
    /// when a score of the open set could not be saved.
    pub(super) fn new_set(&mut self) {
        // Checked first, so a refused switch does not leave a new folder.
        if let Some(name) = self.score_not_on_disk() {
            self.ask_to_leave(name, SetSwitch::New);
            return;
        }
        if let Some(set) = self.create_dated_set() {
            self.switch_set(set, "new set");
        }
    }

    /// A new set folder named after the day, or the error that stopped it.
    fn create_dated_set(&mut self) -> Option<SceneSet> {
        let root = self.sets_root();
        let starter = self.starter_score();
        match SceneSet::create_in(&root, &today(), starter) {
            Ok(set) => Some(set),
            Err(error) => {
                self.set_error(
                    ErrorOwner::Interface,
                    format!("cannot make a set in {}: {error}", root.display()),
                );
                None
            }
        }
    }

    /// Close a picker and return to the surface that opened it.
    pub(super) fn close_set_prompt(&mut self) {
        self.set_prompt = None;
        self.renaming_sample = None;
        self.renaming_session = None;
        if let Some(focus) = self.set_prompt_return_focus.take() {
            self.focus = focus;
            if let Focus::Panel(kind) = focus {
                self.raise(kind);
            }
        }
        self.settle_focus();
    }

    /// Open a set or source picker, retaining the initiating focus even
    /// when a bank chooser leads into another naming prompt.
    pub(super) fn open_set_prompt(&mut self, prompt: SetPrompt) {
        // A prompt is the panel's next question, and it takes the
        // keyboard: the search box stands down, and its vim modes with
        // it, so a letter the prompt is waiting for cannot land in the
        // box instead.
        if let Some(panel) = self.set_panel.as_mut() {
            panel.search = None;
            panel.vim_insert = false;
            panel.refilter();
        }
        if self.set_prompt.is_none() {
            let settings_prompt = matches!(
                prompt,
                SetPrompt::SetsFolder
                    | SetPrompt::RecordingsFolder
                    | SetPrompt::VimMappingsPath
                    | SetPrompt::AddSampleSource
                    | SetPrompt::EditSampleSource(_)
            );
            #[cfg(feature = "vst")]
            let settings_prompt = settings_prompt
                || prompt == SetPrompt::AddVstFolder
                || prompt == SetPrompt::AddPresetFolder;
            self.set_prompt_return_focus =
                Some(if settings_prompt && self.settings_sheet.is_some() {
                    Focus::Panel(PanelKind::Settings)
                } else {
                    self.focus
                });
        }
        // An import or bank alias asked for on Sources opens over the page,
        // which stays: the row it acts on is there, and Esc comes back to
        // it. Every other prompt clears the stage as before.
        let over_settings = matches!(
            prompt,
            SetPrompt::SetsFolder
                | SetPrompt::RecordingsFolder
                | SetPrompt::VimMappingsPath
                | SetPrompt::AddSampleSource
                | SetPrompt::EditSampleSource(_)
                | SetPrompt::PickBankToRename
                | SetPrompt::RenameBank
                | SetPrompt::RenameSample
        );
        #[cfg(feature = "vst")]
        let over_settings = over_settings
            || prompt == SetPrompt::AddVstFolder
            || prompt == SetPrompt::AddPresetFolder;
        if over_settings && self.settings_sheet.is_some() {
            self.dismiss_dialogs_except(&[PanelKind::Set, PanelKind::Settings]);
        } else {
            self.dismiss_dialogs(Some(PanelKind::Set));
        }
        let root = self.sets_root();
        let picker = match prompt {
            SetPrompt::OpenSet => FilePicker::new(
                "open a set or a Strudel export",
                "opens it",
                set_candidates(&root, self.scenes.directory())
                    .into_iter()
                    .chain(export_candidates(&root))
                    .collect(),
                &root,
            )
            .choosing_folders(),
            SetPrompt::OpenRecent => {
                let current = absolute_set(self.scenes.directory());
                // A relative entry is from a studio that kept the path as
                // it was typed: it names a different folder from each
                // working directory, so it is not offered.
                let recent: Vec<Candidate> = self
                    .prefs
                    .recent_sets
                    .iter()
                    .map(PathBuf::from)
                    .filter(|path| path.is_absolute() && path.is_dir() && *path != current)
                    .map(|path| set_candidate(&path))
                    .collect();
                FilePicker::new("open a recent set", "opens it", recent, &root).choosing_folders()
            }
            SetPrompt::ImportStrudel => FilePicker::new(
                "import Strudel patterns",
                "adds them to this set",
                export_candidates(&root),
                &root,
            ),
            SetPrompt::RenameSet => {
                let mut picker = FilePicker::naming("rename the set", "renames its folder");
                picker.offer(&self.scenes.name());
                picker
            }
            SetPrompt::SetsFolder => {
                let mut picker =
                    FilePicker::new("sets folder", "keeps new sets there", Vec::new(), &root)
                        .choosing_folders();
                picker.offer(&shorten_home(&root));
                picker
            }
            SetPrompt::RecordingsFolder => {
                let recordings = self.recordings_directory();
                let mut picker = FilePicker::new(
                    "recordings folder",
                    "saves finished takes there",
                    Vec::new(),
                    &recordings,
                )
                .choosing_folders();
                picker.offer(&shorten_home(&recordings));
                picker
            }
            SetPrompt::VimMappingsPath => {
                let mut picker =
                    FilePicker::text("Vim mappings path", "read Vim mappings from this file");
                picker.label = "path";
                picker.offer(&self.vim_mappings_path_label());
                picker
            }
            SetPrompt::RenameBank => {
                FilePicker::naming("bank alias", "changes the bank's playable name")
            }
            SetPrompt::RenameSample => {
                FilePicker::naming("rename sample file", "keeps its extension and bank index")
            }
            SetPrompt::RenameSession => {
                FilePicker::naming("rename session", "keeps the tape and its extension")
            }
            SetPrompt::PickBankToRename => {
                let mut picker = FilePicker::new(
                    "alias a bank",
                    "picks which sound to rename",
                    Vec::new(),
                    std::path::Path::new("."),
                );
                picker.browsable = false;
                picker.label = "bank";
                picker
            }
            // A source can be a folder OR an address, so the picker browses
            // folders and takes typed text either way: what is typed is
            // read as a source, not as a path that has to exist.
            SetPrompt::AddSampleSource | SetPrompt::EditSampleSource(_) => FilePicker::new(
                "import samples",
                "imports it for every set",
                Vec::new(),
                &root,
            )
            .choosing_folders(),
            #[cfg(feature = "vst")]
            SetPrompt::AddVstFolder => {
                FilePicker::new("plugin folder", "reads its plugins", Vec::new(), &root)
                    .choosing_folders()
            }
            #[cfg(feature = "vst")]
            SetPrompt::AddPresetFolder => {
                FilePicker::new("preset folder", "reads its presets", Vec::new(), &root)
                    .choosing_folders()
            }
        };
        self.set_prompt = Some((prompt, picker));
        self.focus_panel(PanelKind::Set);
        self.status = match prompt {
            SetPrompt::OpenSet => {
                "open a set or a Strudel export - Enter opens the chosen one · Tab browses · type a path · Esc back"
                    .into()
            }
            SetPrompt::OpenRecent => {
                "open a recent set - Enter opens the chosen one · Esc back".into()
            }
            SetPrompt::ImportStrudel => {
                "import Strudel patterns - Enter adds every entry to this set · Tab browses · type a path · Esc back"
                    .into()
            }
            SetPrompt::RenameSet => {
                "rename the set - type the new name and press Enter · Esc back".into()
            }
            SetPrompt::SetsFolder => {
                "sets folder - type a path, or Tab to browse and Enter on . · Esc back".into()
            }
            SetPrompt::RecordingsFolder => {
                "recordings folder - type a path, or Tab to browse and Enter on . · Esc back".into()
            }
            SetPrompt::VimMappingsPath => {
                "Vim mappings path - type a full path · Enter keeps · Esc back".into()
            }
            SetPrompt::AddSampleSource => {
                "import samples - a folder, a URL, or github:user/repo · Enter imports · Esc back"
                    .into()
            }
            SetPrompt::EditSampleSource(_) => {
                "edit sample source - change the folder or URL · Enter keeps · Esc back".into()
            }
            SetPrompt::PickBankToRename => {
                "alias - pick the bank that clashes, then name it · Esc back".into()
            }
            SetPrompt::RenameSample => {
                "rename file - name without extension · Enter saves · Esc back".into()
            }
            SetPrompt::RenameSession => {
                "rename session - name without extension · Enter saves · Esc back".into()
            }
            SetPrompt::RenameBank => {
                "bank alias - type its playable name · original name resets · Esc back".into()
            }
            #[cfg(feature = "vst")]
            SetPrompt::AddVstFolder => {
                "plugin folder - type a path, or Tab to browse and Enter on . · Esc back".into()
            }
            #[cfg(feature = "vst")]
            SetPrompt::AddPresetFolder => {
                "preset folder - any structure · Enter on . or a path adds it · Esc back".into()
            }
        };
        self.dirty_frame = true;
    }

    pub(super) fn handle_set_prompt_key(
        &mut self,
        code: KeyCode,
        primary: bool,
        shift: bool,
    ) -> bool {
        let Some((prompt, picker)) = self.set_prompt.as_mut() else {
            return false;
        };
        let prompt = *prompt;
        match picker_key(picker, code, primary, shift, self.frame) {
            PickerKey::Ignored => return false,
            PickerKey::Handled => {}
            PickerKey::Back => {
                self.close_set_prompt();
                self.renaming_sample = None;
                if matches!(prompt, SetPrompt::PickBankToRename | SetPrompt::RenameBank) {
                    self.renaming_bank = None;
                    self.renaming_bank_source = None;
                }
                self.status = match prompt {
                    SetPrompt::PickBankToRename | SetPrompt::RenameBank => "alias cancelled",
                    SetPrompt::AddSampleSource
                    | SetPrompt::EditSampleSource(_)
                    | SetPrompt::ImportStrudel => "import cancelled",
                    _ => "set prompt closed",
                }
                .into();
            }
            PickerKey::Chose(path) => match prompt {
                SetPrompt::OpenSet | SetPrompt::OpenRecent => self.open_set_at(&path),
                SetPrompt::ImportStrudel => self.import_strudel_into_current(&path),
                SetPrompt::RenameSet => {
                    let name = path.to_string_lossy().trim().to_owned();
                    self.rename_set(&name);
                }
                SetPrompt::SetsFolder => self.choose_sets_folder(&path),
                SetPrompt::RecordingsFolder => self.choose_recordings_folder(&path),
                SetPrompt::VimMappingsPath => {
                    if path.is_absolute() {
                        self.close_set_prompt();
                        self.set_vim_mappings_path(Some(path.to_string_lossy().into_owned()));
                    } else if let Some((_, picker)) = self.set_prompt.as_mut() {
                        picker.error = Some("enter a full file path".into());
                    }
                }
                SetPrompt::AddSampleSource => {
                    let spec = path.to_string_lossy().trim().to_owned();
                    self.return_from_source_prompt();
                    self.add_sample_source(&spec);
                }
                SetPrompt::EditSampleSource(at) => {
                    let spec = path.to_string_lossy().trim().to_owned();
                    self.return_from_source_prompt();
                    if self.refuse_set_output_source(&spec) {
                        return true;
                    }
                    if let Some(source) = self.prefs.sample_sources.get_mut(at) {
                        source.spec = spec.clone();
                        self.save_prefs_soon();
                        self.adopt_global_sources();
                        self.status = format!("sample source changed to {spec}");
                    }
                }
                SetPrompt::PickBankToRename => {
                    let name = path.to_string_lossy().trim().to_owned();
                    if name.is_empty() {
                        return true;
                    }
                    self.renaming_bank = Some(name.clone());
                    let offered = self
                        .renaming_bank_source
                        .as_deref()
                        .and_then(|source| {
                            self.worker
                                .library()
                                .map(|library| library.alias_for_import(source, &name))
                        })
                        .unwrap_or_else(|| name.clone());
                    self.open_set_prompt(SetPrompt::RenameBank);
                    if let Some((_, picker)) = self.set_prompt.as_mut() {
                        picker.offer(&offered);
                    }
                }
                SetPrompt::RenameSample => {
                    let stem = path.to_string_lossy().trim().to_owned();
                    self.rename_sample_file(&stem);
                }
                SetPrompt::RenameSession => {
                    let stem = path.to_string_lossy().trim().to_owned();
                    self.rename_session_file(&stem);
                }
                SetPrompt::RenameBank => {
                    let name = path.to_string_lossy().trim().to_owned();
                    self.close_set_prompt();
                    self.rename_bank(&name);
                }
                #[cfg(feature = "vst")]
                SetPrompt::AddVstFolder => self.add_vst_folder(&path),
                #[cfg(feature = "vst")]
                SetPrompt::AddPresetFolder => self.add_preset_folder(&path),
            },
        }
        self.dirty_frame = true;
        true
    }

    /// Open another set: a folder, or a score and its folder.
    fn open_set_at(&mut self, path: &std::path::Path) {
        // A Strudel pattern export - a .json object of scores - is not a
        // set on disk but the makings of one. The set's own file is a .json
        // too, and stays a refusal.
        if path
            .extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("json"))
            && !path.file_name().is_some_and(|name| {
                name.to_string_lossy()
                    .starts_with(crate::scenes::SET_FILE_NAME)
            })
        {
            self.import_strudel_set(path);
            return;
        }
        let starter = self.starter_score();
        match SceneSet::open(path, starter) {
            Ok(set) => {
                self.close_set_prompt();
                self.switch_set(set, "opened set");
            }
            Err(error) => {
                if let Some((_, picker)) = self.set_prompt.as_mut() {
                    picker.error = Some(error.to_string());
                }
            }
        }
    }

    /// A Strudel pattern export read as a new set: one score per entry,
    /// in a folder of its own under the sets folder, named after the file
    /// the export came from. Every entry is a named file, its `@title`
    /// when the code carries one and `pattern N` in the order the
    /// patterns were made when it does not. The status says what was
    /// taken up, and the panel lists the scores it made.
    fn import_strudel_set(&mut self, path: &std::path::Path) {
        let report = |app: &mut Self, error: String| {
            if let Some((_, picker)) = app.set_prompt.as_mut() {
                picker.error = Some(error);
            }
        };
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(error) => return report(self, format!("cannot read {}: {error}", path.display())),
        };
        let entries = match strudel_export(&text) {
            Ok(entries) => entries,
            Err(error) => return report(self, format!("{}: {error}", path.display())),
        };
        let stem = path
            .file_stem()
            .map(|stem| stem.to_string_lossy().into_owned())
            .unwrap_or_default();
        let name = crate::scenes::sanitize_name(&stem).unwrap_or_else(|| "imported set".to_owned());
        let scores = strudel_scores(entries);
        let root = self.sets_root();
        match SceneSet::import_in(&root, &name, &scores) {
            Ok(set) => {
                self.close_set_prompt();
                self.switch_set(set, "imported");
            }
            Err(error) => report(self, format!("cannot import {}: {error}", path.display())),
        }
    }

    /// A Strudel pattern export read into the set already open: every
    /// entry becomes a named score file of the set's folder, listed by
    /// the set panel and none of them taken up as tabs. A name the set
    /// already holds - on disk or open on the strip - is skipped and
    /// counted, never overwritten, so the same file twice brings nothing
    /// the second time and says so.
    fn import_strudel_into_current(&mut self, path: &std::path::Path) {
        let report = |app: &mut Self, error: String| {
            if let Some((_, picker)) = app.set_prompt.as_mut() {
                picker.error = Some(error);
            }
        };
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(error) => return report(self, format!("cannot read {}: {error}", path.display())),
        };
        let entries = match strudel_export(&text) {
            Ok(entries) => entries,
            Err(error) => return report(self, format!("{}: {error}", path.display())),
        };
        let scores = strudel_scores(entries);
        let directory = self.scenes.directory().to_path_buf();
        let mut written = 0usize;
        let mut skipped = 0usize;
        for (name, code) in &scores {
            let file = directory.join(format!("{name}.{}", crate::scenes::SCENE_EXTENSION));
            let taken = file.exists() || self.scenes.scores().any(|scene| scene.path == file);
            if taken {
                skipped += 1;
                continue;
            }
            match std::fs::write(&file, code) {
                Ok(()) => written += 1,
                Err(error) => {
                    return report(self, format!("cannot import {}: {error}", path.display()));
                }
            }
        }
        self.close_set_prompt();
        // The panel lists what the folder holds now, without moving the
        // reader's selection.
        self.refresh_set_panel(None);
        let set = self.scenes.name();
        let plural = |count: usize| if count == 1 { "pattern" } else { "patterns" };
        self.status = match (written, skipped) {
            (0, skipped) => format!(
                "no new {plural} - all {skipped} are already in {set}",
                plural = plural(skipped)
            ),
            (written, skipped) => format!(
                "imported {written} {imported} into {set}{skipped}",
                imported = plural(written),
                skipped = if skipped == 0 {
                    String::new()
                } else {
                    format!(" - {skipped} skipped, already in the set")
                }
            ),
        };
        self.log.push(LogLevel::Info, "set", self.status.clone());
        self.dirty_frame = true;
    }

    /// File > Rename set: the folder moves, and everything follows it -
    /// the tape being written too, whose open file the desk keeps writing
    /// where it moved to, the samples the folder itself brought, whose
    /// URLs name the path that moved, and the imported sources inside it.
    /// A take is the one thing that cannot follow, since the audio thread
    /// holds its path, so a set keeps its name until the take is finished.
    pub(super) fn rename_set(&mut self, name: &str) {
        if self.recording_chip().is_some() {
            if let Some((_, picker)) = self.set_prompt.as_mut() {
                picker.error = Some(SceneError::Recording.to_string());
            }
            return;
        }
        let before = self.scenes.directory().to_path_buf();
        match self.scenes.rename_set(name) {
            Ok(directory) => {
                if let Some(recorder) = self.recorder.as_mut()
                    && let Ok(rest) = recorder
                        .path()
                        .strip_prefix(&before)
                        .map(std::path::Path::to_path_buf)
                {
                    recorder.relocate(directory.join(rest));
                }
                self.close_set_prompt();
                self.prefs.forget_set(&absolute_set(&before));
                self.remember_current_set();
                // Sources inside the folder move with it, or they would
                // name a set that is no longer there.
                if self.prefs.follow_moved_folder(&before, &directory) {
                    self.save_prefs_soon();
                    self.adopt_global_sources();
                }
                // After the move, so it scans the folder that is there now.
                self.adopt_set_samples();
                self.refresh_set_panel(None);
                self.status = format!(
                    "renamed the set - {}",
                    status_file_path(&directory, self.ui_settings.show_full_paths)
                );
                self.log.push(LogLevel::Info, "set", self.status.clone());
            }
            Err(error) => {
                if let Some((_, picker)) = self.set_prompt.as_mut() {
                    picker.error = Some(error.to_string());
                }
            }
        }
        self.dirty_frame = true;
    }

    /// The settings sheet's sets folder row: where new sets are made.
    pub(super) fn choose_sets_folder(&mut self, path: &std::path::Path) {
        if let Err(error) = std::fs::create_dir_all(path) {
            if let Some((_, picker)) = self.set_prompt.as_mut() {
                picker.error = Some(format!("cannot use {}: {error}", path.display()));
            }
            return;
        }
        self.prefs.sets_directory = Some(path.to_string_lossy().into_owned());
        self.save_prefs_soon();
        self.close_set_prompt();
        self.status = format!(
            "new sets go in {}",
            status_file_path(path, self.ui_settings.show_full_paths)
        );
        self.dirty_frame = true;
    }

    /// The settings sheet's recordings folder row: where finished takes go.
    /// A set's own output folder is refused, since takes there never
    /// become samples.
    pub(super) fn choose_recordings_folder(&mut self, path: &std::path::Path) {
        if let Some(folder) = self.set_output_folder(path) {
            if let Some((_, picker)) = self.set_prompt.as_mut() {
                picker.error = Some(format!(
                    "cannot use {}: {}",
                    path.display(),
                    set_output_reason(&folder)
                ));
            }
            return;
        }
        if let Err(error) = std::fs::create_dir_all(path) {
            if let Some((_, picker)) = self.set_prompt.as_mut() {
                picker.error = Some(format!("cannot use {}: {error}", path.display()));
            }
            return;
        }
        self.prefs.recordings_directory = Some(path.to_string_lossy().into_owned());
        self.save_prefs_soon();
        self.close_set_prompt();
        self.status = format!(
            "recordings go in {}",
            status_file_path(path, self.ui_settings.show_full_paths)
        );
        self.dirty_frame = true;
    }

    /// The set open now is the one a bare launch opens next time, and the
    /// newest of the recent ones.
    ///
    /// The preferences keep the full path: a set opened as `emanate` from
    /// one folder has to be the same set when the studio starts in another.
    pub(super) fn remember_current_set(&mut self) {
        self.prefs
            .remember_set(&absolute_set(self.scenes.directory()));
        self.save_prefs_soon();
    }

    fn starter_score(&self) -> &'static str {
        if self.options.mini {
            DEFAULT_MINI_SCORE
        } else {
            DEFAULT_SCORE
        }
    }

    /// A score of the open set could not be written, so its text is only
    /// in its tab. The set panel asks before the set goes: Enter retries,
    /// Del leaves without the score, Esc keeps the set.
    fn ask_to_leave(&mut self, name: String, next: SetSwitch) {
        self.raise_set_panel();
        if let Some(panel) = self.set_panel.as_mut() {
            panel.deleting = None;
            panel.error = None;
            panel.unsaved = Some(name.clone());
        }
        self.pending_switch = Some(next);
        self.status = format!(
            "could not save {name:?} · Enter retries · Del leaves the set without it · Esc keeps the set"
        );
        self.dirty_frame = true;
    }

    /// Enter on the question: write again, and go on once every score is on
    /// disk. A write that fails again asks again.
    fn retry_switch(&mut self) {
        match self.pending_switch.take() {
            Some(SetSwitch::Open(set, verb)) => self.switch_set(*set, verb),
            Some(SetSwitch::New) => self.new_set(),
            None => {}
        }
    }

    /// Del on the question: the set goes, and the edits it could not save
    /// go with it.
    fn leave_unsaved(&mut self) {
        match self.pending_switch.take() {
            Some(SetSwitch::Open(set, verb)) => self.take_up_set(*set, verb),
            Some(SetSwitch::New) => {
                if let Some(set) = self.create_dated_set() {
                    self.take_up_set(set, "new set");
                }
            }
            None => {}
        }
    }

    /// Put the current set down whole - every dirty score written, its
    /// set file current - and take up `set` in its place. What is sounding
    /// keeps sounding: the engine has the last score it was given.
    pub(super) fn switch_set(&mut self, set: SceneSet, verb: &'static str) {
        // Whole, and not only its scores: a prebake tab still dirty here is
        // dropped by the replacement below without ever being written, and
        // the global prebake it edits belongs to no set at all.
        //
        // The old set is put down whole before the new one takes over:
        // wait out the save worker, so a set reopened moments later reads
        // what was on screen when it was left rather than whatever the
        // worker has not written yet.
        //
        // A score that could not be written keeps this set open and asks.
        if let Some(name) = self.score_not_on_disk() {
            self.ask_to_leave(name, SetSwitch::Open(Box::new(set), verb));
            return;
        }
        self.take_up_set(set, verb);
    }

    /// Put the current set down as it is and take up `set`.
    fn take_up_set(&mut self, mut set: SceneSet, verb: &str) {
        // A limiter mode still settling was chosen on this set, so it lands
        // here before the set is put down rather than on the one after.
        self.settle_limiter_mode();
        self.persist_manifest();
        self.finish_set_recording();
        // Ids go on from where this set's left off: the studio remembers
        // scenes by id, and a new set that started at one again would be
        // mistaken for the old.
        set.renumber_from(self.scenes.next_id());
        // Every tab of the old set leaves with it. Its ids are never handed
        // out again, so what was kept under them goes whole, as each tab's
        // would on closing, and the engine hears that tabs left.
        self.scene_visits.clear();
        self.lint.clear();
        self.evaluation_alerts.clear();
        self.live_sliders.clear();
        self.readiness.clear();
        self.tabs_closed += 1;
        self.scenes = set;
        // The tapes open were the old set's.
        self.replay_view = None;
        self.replays.clear();
        // And so were the knob positions. A slot remembers where its
        // control last spoke so the scaled takeover has a sweep to
        // measure from; that memory belongs to the faders of the set
        // that just closed. Keeping it means the first turn after
        // opening a set is read as a move - the exact fling the
        // takeover exists to stop - because the hand has been somewhere
        // else since.
        self.mapping_knob = [None; MAPPING_SLOTS];
        self.panes.truncate(1);
        self.focused = 0;
        self.playing_pane = None;
        self.reconcile_panes();
        self.land_on_current_scene();
        // The set opens laid out the way it was left: the same scores in
        // the same panes, with the caret in the pane that had it.
        self.restore_panes();
        // A set that has earned its file - one asked for by name - is
        // written as soon as it is taken up, so it exists to be opened.
        self.persist_manifest();
        self.remember_current_set();
        self.refresh_set_panel(None);
        for index in 0..super::super::viz_panel::DOCKS {
            if self.viz_docks[index].is_some() {
                self.ensure_default_widget(index);
            }
        }
        self.status = format!(
            "{verb} {} - {} scene(s) in {}",
            self.scenes.name(),
            self.scenes.score_count(),
            status_file_path(self.scenes.directory(), self.ui_settings.show_full_paths)
        );
        self.log.push(LogLevel::Info, "set", self.status.clone());
        // The limiter comes with the set when the set has one, and from
        // the settings when it does not. It is part of a sound, so a set
        // opened mid-performance brings its own rather than keeping
        // whatever the last one was left on.
        self.worker.master().set_limiter(self.live_master_limiter());
        // Its levels come with it for the same reason.
        self.restore_set_levels();
        self.adopt_set_samples();
        self.note_kept_manifest();
        self.dirty_frame = true;
    }

    /// Take up the audio sitting in this set's folder, so it is in the
    /// samples browser and in `s(...)` without a `samples()` call or a
    /// command-line grant. The set that was open takes its own back out.
    ///
    /// The scan reads the set's folder and its subfolders, without
    /// `sessions/` and `exports/`, up to the scan limits. It runs where the
    /// set changes and at each update (`refresh_set_samples`), never on a
    /// frame. A score opened by path has its parent folder as the set
    /// folder, so the scan can be large; only audio files cost a path
    /// resolution.
    pub(super) fn adopt_set_samples(&mut self) {
        let Some(library) = self.worker.library() else {
            return;
        };
        let read = library.adopt_set_folder(self.scenes.directory());
        match &read {
            Ok(0) => {}
            Ok(banks) => {
                self.log.push(
                    LogLevel::Info,
                    "samples",
                    format!("{banks} sound(s) from this set's folder"),
                );
            }
            Err(error) => {
                self.log.push(
                    LogLevel::Warn,
                    "samples",
                    format!("this set's folder was not read for samples: {error}"),
                );
            }
        }
        self.set_samples_error = read.err();
        self.dirty_frame = true;
    }

    /// An update re-reads the set's folder, so audio added or removed since
    /// the last read is what it plays. A folder that does not read keeps the
    /// last banks, and the reason is said once until it changes.
    pub(super) fn refresh_set_samples(&mut self) {
        let Some(library) = self.worker.library() else {
            return;
        };
        match library.refresh_set_folder(self.scenes.directory()) {
            Ok(_) => {
                self.set_samples_error = None;
                self.refresh_catalogue();
            }
            Err(error) => {
                if self.set_samples_error.as_ref() != Some(&error) {
                    self.log.push(
                        LogLevel::Warn,
                        "samples",
                        format!("this set's samples were not refreshed: {error}"),
                    );
                    self.dirty_frame = true;
                }
                self.set_samples_error = Some(error);
            }
        }
    }

    /// Say where an unreadable set file was kept, once, in the log and in
    /// the status line. A folder whose `rustel-set.json` does not parse
    /// still opens - as a plain scan of its scores - but the pads, the
    /// tab list and the set's prebake it held are not in that scan, and
    /// the player is owed the file's new name before they wonder where
    /// their pads went.
    pub(super) fn note_kept_manifest(&mut self) {
        let Some(kept) = self.scenes.take_kept_manifest() else {
            return;
        };
        let name = kept
            .path()
            .and_then(|path| path.file_name())
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        if name.is_empty() {
            return;
        }
        self.status = if kept.kept().is_some() {
            format!("this set's file did not read - kept as {name}, and the folder was scanned")
        } else {
            // It could not be moved either, so it is still there and the
            // set will not write over it: the player has to move it.
            format!("this set's file did not read and could not be moved - {name} is left alone")
        };
        self.log.push(LogLevel::Warn, "set", self.status.clone());
        self.dirty_frame = true;
    }

    /// File > Show set: the set's folder, in the desktop's file manager.
    pub(super) fn reveal_set_folder(&mut self) {
        let directory = self.scenes.directory().to_path_buf();
        self.reveal_target(RevealTarget::Folder(directory));
    }

    /// Restore the surface that opened the source picker.
    fn return_from_source_prompt(&mut self) {
        self.close_set_prompt();
    }
}

/// Step a fixture band from its laid-out height, within its limits and
/// without taking more room than the terminal can provide.
pub(super) fn step_fixture_band(
    name: &str,
    asked: u16,
    grow: bool,
    measured: bool,
    laid: impl Fn(u16) -> u16,
) -> Result<u16, String> {
    let now = match laid(asked) {
        0 => asked,
        rows => rows,
    };
    let next = super::super::viz_panel::step_band(now, grow);
    if next == now {
        return Err(format!(
            "{name} is as {} as a band goes - {now} rows",
            if grow { "tall" } else { "short" }
        ));
    }
    if grow && measured && laid(next) < next {
        return Err(format!(
            "{name} is as tall as this terminal allows - {now} rows"
        ));
    }
    Ok(next)
}

/// A path as a row reads it: the home folder as `~`.
pub(super) fn shorten_home(path: &std::path::Path) -> String {
    let text = path.display().to_string();
    match rustel_runtime::config_dir::home().map(PathBuf::from) {
        Some(home) if !home.as_os_str().is_empty() => match path.strip_prefix(&home) {
            Ok(rest) if rest.as_os_str().is_empty() => "~".to_owned(),
            Ok(rest) => format!("~{}{}", std::path::MAIN_SEPARATOR, rest.display()),
            Err(_) => text,
        },
        _ => text,
    }
}

/// One pattern of a Strudel pattern export: its code, when Strudel says
/// it was made, and whether its key is a name. The documented form keys
/// each entry by its name, while strudel.cc's own export keys it by an
/// opaque push id and names the song in a `@title` inside the code.
#[derive(Clone, Debug, PartialEq)]
struct StrudelPattern {
    key: String,
    code: String,
    created_at: Option<i64>,
    key_is_name: bool,
}

/// The pattern code of a Strudel pattern export: a JSON object whose
/// entries hold code. Strudel writes each entry as an object with a `code`
/// field, keyed by an id, and a plain string of code is taken too. The
/// documented form's `import` and `export` URL fields are not scores, and
/// an entry that holds none of these shapes is not one either.
fn strudel_export(text: &str) -> Result<Vec<StrudelPattern>, String> {
    let value: serde_json::Value = serde_json::from_str(text)
        .map_err(|error| format!("not a Strudel pattern export: {error}"))?;
    let Some(object) = value.as_object() else {
        return Err("not a Strudel pattern export - not a JSON object".to_owned());
    };
    let patterns: Vec<StrudelPattern> = object
        .iter()
        .filter_map(|(key, value)| match value {
            serde_json::Value::String(code) if !matches!(key.as_str(), "import" | "export") => {
                Some(StrudelPattern {
                    key: key.clone(),
                    code: code.clone(),
                    created_at: None,
                    key_is_name: true,
                })
            }
            serde_json::Value::Object(fields) => {
                let code = fields.get("code")?.as_str()?.to_owned();
                Some(StrudelPattern {
                    key: key.clone(),
                    code,
                    created_at: fields.get("created_at").and_then(|at| at.as_i64()),
                    key_is_name: false,
                })
            }
            _ => None,
        })
        .collect();
    if patterns.is_empty() {
        return Err("not a Strudel pattern export - no scores in it".to_owned());
    }
    Ok(patterns)
}

/// The scores a Strudel pattern export makes, named and in the order the
/// patterns were made. The `@title` inside the code leads, then the
/// entry's key when the export keys by name, then `pattern N` in the
/// order they were made - a push id is never a file name. The set's own
/// folder rule keeps two names that collide apart.
fn strudel_scores(patterns: Vec<StrudelPattern>) -> Vec<(String, String)> {
    // The sort is stable, so entries that say nothing about when they were
    // made keep the order the file holds them in.
    let mut patterns = patterns;
    patterns.sort_by_key(|pattern| pattern.created_at.unwrap_or(0));
    patterns
        .into_iter()
        .enumerate()
        .map(|(index, pattern)| {
            let titled = crate::metadata::ScoreMeta::parse(&pattern.code)
                .first("title")
                .and_then(crate::scenes::sanitize_name);
            let keyed = pattern
                .key_is_name
                .then(|| crate::scenes::sanitize_name(&pattern.key))
                .flatten();
            let name = titled
                .or(keyed)
                .unwrap_or_else(|| format!("pattern {}", index + 1));
            (name, pattern.code)
        })
        .collect()
}

/// The set folders under `root`, `current` left out: a folder with a set
/// file or a score in it. Order: last changed first, then by name.
fn set_candidates(root: &std::path::Path, current: &std::path::Path) -> Vec<Candidate> {
    let Ok(entries) = std::fs::read_dir(root) else {
        return Vec::new();
    };
    let mut candidates: Vec<(std::time::SystemTime, Candidate)> = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.is_dir() && path != current && is_set_folder(path))
        .map(|path| (set_changed(&path), set_candidate(&path)))
        .collect();
    candidates.sort_by(|(a_time, a), (b_time, b)| {
        b_time
            .cmp(a_time)
            .then_with(|| a.label.to_lowercase().cmp(&b.label.to_lowercase()))
    });
    candidates
        .into_iter()
        .map(|(_, candidate)| candidate)
        .collect()
}

/// The Strudel pattern exports at the sets root, newest first: a `.json`
/// file the open-set prompt can open is offered beside the sets, so an
/// export does not need a typed path or a folder walk to be found.
fn export_candidates(root: &std::path::Path) -> Vec<Candidate> {
    let Ok(entries) = std::fs::read_dir(root) else {
        return Vec::new();
    };
    let mut candidates: Vec<(std::time::SystemTime, Candidate)> = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.is_file()
                && path
                    .extension()
                    .is_some_and(|extension| extension.eq_ignore_ascii_case("json"))
        })
        .filter_map(|path| {
            // Reading the file tells it apart from any other json: only a
            // pattern export with scores in it is offered.
            let text = std::fs::read_to_string(&path).ok()?;
            let count = strudel_export(&text).ok()?.len();
            let changed = std::fs::metadata(&path)
                .and_then(|metadata| metadata.modified())
                .ok()?;
            let label = path
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| path.display().to_string());
            Some((
                changed,
                Candidate {
                    label,
                    detail: format!(
                        "Strudel export - {count} pattern{}",
                        if count == 1 { "" } else { "s" }
                    ),
                    path,
                },
            ))
        })
        .collect();
    candidates.sort_by(|(a_time, a), (b_time, b)| {
        b_time
            .cmp(a_time)
            .then_with(|| a.label.to_lowercase().cmp(&b.label.to_lowercase()))
    });
    candidates
        .into_iter()
        .map(|(_, candidate)| candidate)
        .collect()
}

/// When a set last changed: the newest of its folder and the files in it.
fn set_changed(path: &std::path::Path) -> std::time::SystemTime {
    let changed = |path: &std::path::Path| {
        std::fs::metadata(path)
            .and_then(|metadata| metadata.modified())
            .ok()
    };
    std::fs::read_dir(path)
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|entry| entry.is_file())
        .filter_map(|entry| changed(&entry))
        .chain(changed(path))
        .max()
        .unwrap_or(std::time::UNIX_EPOCH)
}

/// Why a set's output folder is not a place for samples, for a refusal.
pub(super) fn set_output_reason(folder: &std::path::Path) -> String {
    let name = folder
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    format!("it is a set's own {name} folder, which the studio writes into")
}

/// Whether a folder holds a set: a set file, or any score.
fn is_set_folder(path: &std::path::Path) -> bool {
    super::super::scenes::SetManifest::path_in(path).is_file() || score_count_in(path) > 0
}

fn score_count_in(path: &std::path::Path) -> usize {
    std::fs::read_dir(path)
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .filter(|entry| {
            entry.path().is_file()
                && entry
                    .path()
                    .extension()
                    .is_some_and(|extension| extension == super::super::scenes::SCENE_EXTENSION)
        })
        .count()
}

/// The set folder as a full path, which reads the same from any working
/// directory.
fn absolute_set(directory: &std::path::Path) -> PathBuf {
    std::path::absolute(directory).unwrap_or_else(|_| directory.to_path_buf())
}

/// A set folder as a picker row: its name and how many scores it holds.
fn set_candidate(path: &std::path::Path) -> Candidate {
    let scores = score_count_in(path);
    Candidate {
        label: path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.display().to_string()),
        detail: format!("{scores} score{}", if scores == 1 { "" } else { "s" }),
        path: path.to_path_buf(),
    }
}

/// Where a set switch was going when a score could not be saved.
pub(super) enum SetSwitch {
    Open(Box<SceneSet>, &'static str),
    New,
}

/// Which of the File menu's prompts is up: a set to open, a recent one,
/// a new name for this set, or the folder new sets are made in.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum SetPrompt {
    OpenSet,
    OpenRecent,
    /// A Strudel pattern export file, whose entries become score files of
    /// the set already open.
    ImportStrudel,
    RenameSet,
    SetsFolder,
    RecordingsFolder,
    VimMappingsPath,
    /// A folder of samples, or a pack's address, to import for every set.
    AddSampleSource,
    /// Replace one existing source while keeping its row and enabled state.
    EditSampleSource(usize),
    /// Pick which bank of a user import to alias, then [`Self::RenameBank`].
    PickBankToRename,
    /// A new name for an imported bank. Which bank is in `renaming_bank`.
    RenameBank,
    /// A local file in `renaming_sample`, with its extension preserved.
    RenameSample,
    /// A tape in `renaming_session`, with its extension preserved.
    RenameSession,
    /// A folder with VST3 plugins, for the vst page of the settings sheet.
    #[cfg(feature = "vst")]
    AddVstFolder,
    /// A folder with `.vstpreset` files in any structure, for the vst page
    /// of the settings sheet.
    #[cfg(feature = "vst")]
    AddPresetFolder,
}

#[cfg(test)]
mod tests {
    mod sets {
        //! Tests for `app/sets.rs`.

        use crate::app::sets::*;
        use crate::app::tests::support::*;

        #[test]
        fn paths_under_the_platform_home_are_shown_with_a_tilde() {
            let home = PathBuf::from(rustel_runtime::config_dir::home().expect("a test home"));
            assert_eq!(shorten_home(&home), "~");
            assert_eq!(
                shorten_home(&home.join(".rustel").join("recordings")),
                format!(
                    "~{}.rustel{}recordings",
                    std::path::MAIN_SEPARATOR,
                    std::path::MAIN_SEPARATOR
                )
            );
        }

        /// ^W closes a tab and no more: the file stays in the folder, the set
        /// panel (^B) lists it beside the open scores with their strip
        /// numbers, and Enter on it opens it again, after the current scene.
        #[test]
        fn a_closed_tab_stays_in_the_set_panel_and_opens_again() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = app_over(directory.path());
            app.new_scene(false);
            assert_eq!(app.scenes.score_count(), 2);
            assert_eq!(app.scenes.current().name(), "scene 1");
            press(&mut app, KeyCode::Char('w'), KeyModifiers::CONTROL);
            assert_eq!(app.scenes.score_count(), 1, "closed");
            assert!(
                directory.path().join("scene 1.strudel").exists(),
                "the file stays"
            );
            assert!(app.status.contains("closed"), "{}", app.status);

            press(&mut app, KeyCode::Char('b'), KeyModifiers::CONTROL);
            assert_eq!(app.focus, Focus::Panel(PanelKind::Set));
            assert_eq!(app.prefs.set_panel, Some(true), "remembered open");
            let panel = app.set_panel.as_ref().expect("the panel is open");
            assert_eq!(
                panel
                    .files
                    .iter()
                    .map(|file| (file.name.as_str(), file.strip))
                    .collect::<Vec<_>>(),
                [("first", Some(1)), ("scene 1", None)]
            );
            assert_eq!(panel.selected, 0, "opened on the scene on screen");
            assert_eq!(panel.lines.len(), 3, "two scores and the sessions fold");
            press(&mut app, KeyCode::Down, KeyModifiers::NONE);
            press(&mut app, KeyCode::Enter, KeyModifiers::NONE);
            assert_eq!(app.scenes.score_count(), 2, "open again");
            assert_eq!(app.scenes.current().name(), "scene 1", "and selected");
            assert_eq!(app.panes[app.focused].scene, app.scenes.current().id);
            assert_eq!(
                app.focus,
                Focus::Editor,
                "Enter hands the keyboard to the score"
            );
            let panel = app.set_panel.as_ref().expect("the panel stays open");
            assert!(panel.files.iter().all(|file| file.open.is_some()));
            assert_eq!(panel.selected, 1, "the selection followed the scene");
            // The set file remembers the open tabs.
            let manifest = crate::scenes::SetManifest::load(directory.path()).expect("set file");
            assert_eq!(
                manifest
                    .scenes
                    .iter()
                    .map(|entry| entry.file.as_str())
                    .collect::<Vec<_>>(),
                ["first.strudel", "scene 1.strudel"]
            );
            // ^B hides the panel and shows it again; Enter on an open score
            // selects it rather than opening it twice.
            press(&mut app, KeyCode::Char('b'), KeyModifiers::CONTROL);
            assert!(app.set_panel.is_none(), "^B again hides the panel");
            assert_eq!(app.prefs.set_panel, Some(false));
            press(&mut app, KeyCode::Char('b'), KeyModifiers::CONTROL);
            press(&mut app, KeyCode::Home, KeyModifiers::NONE);
            press(&mut app, KeyCode::Enter, KeyModifiers::NONE);
            assert_eq!(app.scenes.score_count(), 2);
            assert_eq!(app.scenes.current().name(), "first");
            assert_eq!(
                app.panes[app.focused].scene,
                app.scenes.current().id,
                "the pane shows it, as a click on its tab would"
            );
            assert_eq!(app.focus, Focus::Editor);
        }

        /// A digit opens the score holding that strip number, the way Enter
        /// on its row does. A digit no score holds names what it missed, and
        /// the panel keeps the keyboard. Vim mode changes none of it: the
        /// panel is a list, and a digit is not one of its letters.
        #[test]
        fn a_digit_opens_the_score_with_that_strip_number() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = app_over(directory.path());
            app.new_scene(false);
            press(&mut app, KeyCode::Char('b'), KeyModifiers::CONTROL);
            // first holds strip 1, scene 1 holds strip 2.
            press(&mut app, KeyCode::Char('3'), KeyModifiers::NONE);
            assert_eq!(app.scenes.current().name(), "scene 1", "nothing opened");
            assert!(app.status.contains("no scene 3"), "{}", app.status);
            assert_eq!(
                app.focus,
                Focus::Panel(PanelKind::Set),
                "the panel keeps the keyboard"
            );
            press(&mut app, KeyCode::Char('1'), KeyModifiers::NONE);
            assert_eq!(app.scenes.current().name(), "first", "strip 1 opened");
            assert_eq!(app.focus, Focus::Editor, "the score takes the keyboard");
            let panel = app.set_panel.as_ref().expect("the panel stays open");
            assert_eq!(panel.selected, 0, "the selection followed the scene");
            app.ui_settings.editor_keybindings = crate::editor::EditorKeybindings::Vim;
            app.sync_editor_keybindings();
            press(&mut app, KeyCode::Char('b'), KeyModifiers::CONTROL);
            press(&mut app, KeyCode::Char('b'), KeyModifiers::CONTROL);
            press(&mut app, KeyCode::Char('2'), KeyModifiers::NONE);
            assert_eq!(
                app.scenes.current().name(),
                "scene 1",
                "strip 2 in Vim mode"
            );
            assert_eq!(app.focus, Focus::Editor);
        }

        /// The details block stands at the panel's foot and follows the
        /// selection: the selected score's detail is there with no key
        /// and nothing to close. Vim mode changes none of it, the panel
        /// being a list, not a text surface.
        #[test]
        fn the_details_block_follows_the_selection() {
            let directory = tempfile::tempdir().expect("temp dir");
            std::fs::write(directory.path().join("second.strudel"), "$: s(\"hh\")").unwrap();
            let mut app = app_over(directory.path());
            press(&mut app, KeyCode::Char('b'), KeyModifiers::CONTROL);
            assert_eq!(app.focus, Focus::Panel(PanelKind::Set));
            assert_eq!(
                app.set_panel.as_ref().unwrap().detail_rows()[0],
                "first",
                "the selected score's detail is up"
            );
            press(&mut app, KeyCode::Down, KeyModifiers::NONE);
            assert_eq!(
                app.set_panel.as_ref().unwrap().detail_rows()[0],
                "second",
                "the block follows the selection"
            );
            // Vim mode: the same keys do the same job.
            app.ui_settings.editor_keybindings = crate::editor::EditorKeybindings::Vim;
            app.sync_editor_keybindings();
            press(&mut app, KeyCode::Up, KeyModifiers::NONE);
            assert_eq!(
                app.set_panel.as_ref().unwrap().detail_rows()[0],
                "first",
                "vim mode: the block still follows"
            );
        }

        /// A save that lands re-reads the rows, so the foot follows the
        /// file without the score being reopened, and the reader's
        /// selection does not move while it happens.
        #[test]
        fn the_details_follow_a_save_without_reopening_the_score() {
            let directory = tempfile::tempdir().expect("temp dir");
            std::fs::write(directory.path().join("second.strudel"), "// plain").unwrap();
            let mut app = app_over(directory.path());
            press(&mut app, KeyCode::Char('b'), KeyModifiers::CONTROL);
            press(&mut app, KeyCode::Down, KeyModifiers::NONE);
            assert_eq!(app.set_panel.as_ref().unwrap().detail_rows()[0], "second");
            // The header of the open score gains an author: the update
            // writes it to disk behind the panel's back.
            app.dispatch_editor(crate::editor::Command::Move {
                motion: crate::editor::Motion::DocumentStart,
                extend: false,
            })
            .expect("top");
            app.dispatch_editor(crate::editor::Command::InsertText(
                "/*\n@by you\n*/\n".into(),
            ))
            .expect("edit");
            app.dispatch_editor(crate::editor::Command::Evaluate)
                .expect("update");
            app.wait_for_saves();
            assert_eq!(
                app.set_panel.as_ref().unwrap().detail_rows()[0],
                "second",
                "the save moved no row"
            );
            press(&mut app, KeyCode::Up, KeyModifiers::NONE);
            let rows = app.set_panel.as_ref().unwrap().detail_rows();
            assert!(
                rows.contains(&"@by you".to_owned()),
                "the foot reads the saved header: {rows:?}"
            );
        }

        /// `/` opens a filter that owns the keyboard: letters hide the
        /// scores the query does not answer - `n` types into the query
        /// rather than starting anything - Enter leaves the filter
        /// standing on the first score it kept, Esc drops it whole, and a
        /// set prompt opening stands the box down. In vim the box keeps
        /// its own modes: `i` types, Esc returns to command, and the
        /// second Esc drops the filter.
        #[test]
        fn the_search_box_filters_keeps_and_drops() {
            let directory = tempfile::tempdir().expect("temp dir");
            std::fs::write(
                directory.path().join("spare.strudel"),
                "/*\n@title night drive\n*/\n$: s(\"bd\")\n",
            )
            .expect("spare");
            let mut app = app_over(directory.path());
            app.new_scene(false);
            press(&mut app, KeyCode::Char('b'), KeyModifiers::CONTROL);
            let query = |app: &App| {
                app.set_panel
                    .as_ref()
                    .unwrap()
                    .search
                    .as_ref()
                    .map(|search| search.query.clone())
            };
            let names = |app: &App| {
                let panel = app.set_panel.as_ref().unwrap();
                panel
                    .lines
                    .iter()
                    .filter_map(|line| match line {
                        SetLine::File(index) => {
                            panel.files.get(*index).map(|file| file.name.clone())
                        }
                        _ => None,
                    })
                    .collect::<Vec<_>>()
            };

            press(&mut app, KeyCode::Char('/'), KeyModifiers::NONE);
            press(&mut app, KeyCode::Char('n'), KeyModifiers::NONE);
            assert_eq!(query(&app).as_deref(), Some("n"), "n types into the query");
            assert!(
                !app.set_panel.as_ref().unwrap().vim_insert,
                "plain keys do not open the box in Insert"
            );
            press(&mut app, KeyCode::Char('i'), KeyModifiers::NONE);
            assert_eq!(
                names(&app),
                ["spare"],
                "the query answers a title, not a name"
            );
            // A key the box takes marks the frame: a held Backspace
            // repaints at the key's own pace, not the caret blink's.
            app.dirty_frame = false;
            press(&mut app, KeyCode::Backspace, KeyModifiers::NONE);
            assert_eq!(
                query(&app).as_deref(),
                Some("n"),
                "Backspace popped a letter"
            );
            assert!(app.dirty_frame, "the box's key marked the frame");
            press(&mut app, KeyCode::Char('i'), KeyModifiers::NONE);
            press(&mut app, KeyCode::Enter, KeyModifiers::NONE);
            {
                let panel = app.set_panel.as_ref().unwrap();
                assert!(!panel.search.as_ref().unwrap().typing, "Enter ends typing");
                assert_eq!(panel.selected_file().unwrap().name, "spare");
            }
            press(&mut app, KeyCode::Esc, KeyModifiers::NONE);
            assert_eq!(
                names(&app),
                ["first", "scene 1", "spare"],
                "Esc restores the folder"
            );

            // A prompt takes the keyboard, so the box stands down.
            press(&mut app, KeyCode::Char('/'), KeyModifiers::NONE);
            assert!(app.set_panel.as_ref().unwrap().search.is_some());
            app.open_set_prompt(SetPrompt::OpenSet);
            assert!(
                app.set_panel.as_ref().unwrap().search.is_none(),
                "a prompt stands the search box down"
            );
            press(&mut app, KeyCode::Esc, KeyModifiers::NONE);

            // Vim: `/` opens the box typing, the way Vim's own search does,
            // and the Esc ladder leaves insert before it drops the filter.
            app.ui_settings.editor_keybindings = crate::editor::EditorKeybindings::Vim;
            app.sync_editor_keybindings();
            press(&mut app, KeyCode::Char('/'), KeyModifiers::NONE);
            assert!(
                app.set_panel.as_ref().unwrap().vim_insert,
                "`/` opens the box in insert"
            );
            press(&mut app, KeyCode::Char('s'), KeyModifiers::NONE);
            press(&mut app, KeyCode::Char('p'), KeyModifiers::NONE);
            assert_eq!(query(&app).as_deref(), Some("sp"), "letters type at once");
            assert_eq!(names(&app), ["spare"]);
            // The list keys type too: none of j k h l waits for a mapping
            // or walks the list while the box holds the keyboard.
            press(&mut app, KeyCode::Char('j'), KeyModifiers::NONE);
            press(&mut app, KeyCode::Char('k'), KeyModifiers::NONE);
            assert_eq!(
                query(&app).as_deref(),
                Some("spjk"),
                "the list letters typed, at once"
            );
            assert!(
                app.set_panel.as_ref().unwrap().vim_insert,
                "the shipped jk mapping did not leave Insert"
            );
            press(&mut app, KeyCode::Backspace, KeyModifiers::NONE);
            press(&mut app, KeyCode::Backspace, KeyModifiers::NONE);
            assert_eq!(query(&app).as_deref(), Some("sp"), "back to the query");
            press(&mut app, KeyCode::Esc, KeyModifiers::NONE);
            assert!(
                !app.set_panel.as_ref().unwrap().vim_insert,
                "the first Esc only leaves insert"
            );
            assert!(app.set_panel.as_ref().unwrap().search.is_some());
            press(&mut app, KeyCode::Char('/'), KeyModifiers::NONE);
            assert!(
                app.set_panel.as_ref().unwrap().vim_insert,
                "`/` on a standing filter retypes in insert"
            );
            press(&mut app, KeyCode::Char('o'), KeyModifiers::NONE);
            assert_eq!(
                query(&app).as_deref(),
                Some("spo"),
                "a retyped box continues its query"
            );
            press(&mut app, KeyCode::Enter, KeyModifiers::NONE);
            assert!(
                !app.set_panel.as_ref().unwrap().vim_insert,
                "Enter stands the box down in Vim"
            );
            press(&mut app, KeyCode::Esc, KeyModifiers::NONE);
            press(&mut app, KeyCode::Esc, KeyModifiers::NONE);
            assert!(app.set_panel.as_ref().unwrap().search.is_none());
        }

        /// Backspace over a filter standing edits the query, never the
        /// file: the hand's way back into the box can press it any
        /// number of times without arming a delete, where Delete still
        /// asks.
        #[test]
        fn backspace_edits_a_standing_filter_never_the_file() {
            let directory = tempfile::tempdir().expect("temp dir");
            std::fs::write(directory.path().join("spare.strudel"), "// spare\n").expect("spare");
            let mut app = app_over(directory.path());
            app.new_scene(false);
            press(&mut app, KeyCode::Char('b'), KeyModifiers::CONTROL);
            press(&mut app, KeyCode::Char('/'), KeyModifiers::NONE);
            press(&mut app, KeyCode::Char('s'), KeyModifiers::NONE);
            press(&mut app, KeyCode::Char('p'), KeyModifiers::NONE);
            press(&mut app, KeyCode::Enter, KeyModifiers::NONE);
            let panel = app.set_panel.as_ref().unwrap();
            assert!(
                !panel.search.as_ref().unwrap().typing,
                "Enter left the filter standing"
            );

            // Backspace takes the box back up and deletes from the query;
            // pressed on and on, onto an empty query, it never arms a
            // delete of the file under the panel.
            press(&mut app, KeyCode::Backspace, KeyModifiers::NONE);
            {
                let panel = app.set_panel.as_ref().unwrap();
                assert!(panel.search.as_ref().unwrap().typing, "the box types again");
                assert_eq!(panel.search.as_ref().unwrap().query, "s");
                assert!(panel.deleting.is_none(), "no delete was armed");
            }
            press(&mut app, KeyCode::Backspace, KeyModifiers::NONE);
            press(&mut app, KeyCode::Backspace, KeyModifiers::NONE);
            {
                let panel = app.set_panel.as_ref().unwrap();
                assert_eq!(
                    panel.search.as_ref().unwrap().query,
                    "",
                    "the query is gone"
                );
                assert!(panel.deleting.is_none(), "still no delete was armed");
            }
            assert!(
                directory.path().join("spare.strudel").is_file(),
                "every file stayed on disk"
            );

            // Delete keeps its own arm, as the hints say.
            press(&mut app, KeyCode::Esc, KeyModifiers::NONE);
            press(&mut app, KeyCode::Delete, KeyModifiers::NONE);
            assert!(
                app.set_panel.as_ref().unwrap().deleting.is_some(),
                "Delete asks, as it always did"
            );
        }

        /// Delete in the panel asks and Enter answers: an open score off the
        /// strip and the disk, a closed one off the disk, and the last score
        /// never. Esc keeps what was asked about, and Scene > Delete scene
        /// asks the same question.
        #[test]
        fn the_set_panel_deletes_after_asking() {
            let directory = tempfile::tempdir().expect("temp dir");
            std::fs::write(directory.path().join("spare.strudel"), "// spare\n").expect("spare");
            let mut app = app_over(directory.path());
            app.new_scene(false);
            press(&mut app, KeyCode::Char('b'), KeyModifiers::CONTROL);
            let panel = app.set_panel.as_ref().expect("open");
            assert_eq!(
                panel
                    .files
                    .iter()
                    .map(|file| file.name.as_str())
                    .collect::<Vec<_>>(),
                ["first", "scene 1", "spare"]
            );
            assert_eq!(panel.selected_file().expect("a score").name, "scene 1");
            press(&mut app, KeyCode::Delete, KeyModifiers::NONE);
            assert!(
                directory.path().join("scene 1.strudel").exists(),
                "Delete only asks"
            );
            assert_eq!(app.set_panel.as_ref().expect("open").deleting, Some(1));
            assert!(app.status.starts_with("delete scene 1"), "{}", app.status);
            press(&mut app, KeyCode::Esc, KeyModifiers::NONE);
            assert_eq!(
                app.set_panel.as_ref().expect("open").deleting,
                None,
                "Esc keeps it"
            );
            assert_eq!(
                app.focus,
                Focus::Panel(PanelKind::Set),
                "and stays in the panel"
            );
            press(&mut app, KeyCode::Backspace, KeyModifiers::NONE);
            press(&mut app, KeyCode::Down, KeyModifiers::NONE);
            assert_eq!(
                app.set_panel.as_ref().expect("open").deleting,
                None,
                "any other key withdraws the question"
            );
            press(&mut app, KeyCode::Up, KeyModifiers::NONE);
            press(&mut app, KeyCode::Delete, KeyModifiers::NONE);
            press(&mut app, KeyCode::Enter, KeyModifiers::NONE);
            assert!(!directory.path().join("scene 1.strudel").exists());
            assert_eq!(app.scenes.score_count(), 2);
            assert_eq!(app.set_panel.as_ref().expect("open").files.len(), 2);

            // A closed score is deleted as a file.
            press(&mut app, KeyCode::End, KeyModifiers::NONE);
            press(&mut app, KeyCode::Up, KeyModifiers::NONE);
            assert_eq!(
                app.set_panel
                    .as_ref()
                    .and_then(SetPanel::selected_file)
                    .expect("a score")
                    .name,
                "spare"
            );
            press(&mut app, KeyCode::Enter, KeyModifiers::NONE);
            assert_eq!(app.scenes.current().name(), "spare");
            press(&mut app, KeyCode::Char('w'), KeyModifiers::CONTROL);
            assert_eq!(app.scenes.score_count(), 1);
            assert!(directory.path().join("spare.strudel").exists());
            press(&mut app, KeyCode::Char('b'), KeyModifiers::CONTROL);
            press(&mut app, KeyCode::Char('b'), KeyModifiers::CONTROL);
            press(&mut app, KeyCode::Down, KeyModifiers::NONE);
            press(&mut app, KeyCode::Delete, KeyModifiers::NONE);
            press(&mut app, KeyCode::Enter, KeyModifiers::NONE);
            assert!(!directory.path().join("spare.strudel").exists());

            // The last score stays.
            press(&mut app, KeyCode::Home, KeyModifiers::NONE);
            press(&mut app, KeyCode::Delete, KeyModifiers::NONE);
            press(&mut app, KeyCode::Enter, KeyModifiers::NONE);
            assert!(directory.path().join("first.strudel").exists());
            assert!(
                app.set_panel
                    .as_ref()
                    .and_then(|panel| panel.error.as_deref())
                    .is_some_and(|error| error.contains("last scene")),
                "{:?}",
                app.set_panel.as_ref().and_then(|panel| panel.error.clone())
            );

            // The Scene menu's row arms the panel the same way.
            press(&mut app, KeyCode::Esc, KeyModifiers::NONE);
            app.dispatch_menu_action(crate::menu::MenuAction::DeleteScene)
                .expect("menu");
            assert_eq!(app.focus, Focus::Panel(PanelKind::Set));
            assert!(app.set_panel.as_ref().expect("open").deleting.is_some());
        }

        /// File > Open set… offers the sets folder's sets, Tab browses from
        /// there, and `.` in the browser chooses the folder it is in; a set
        /// file named by mistake is refused with its folder suggested.
        #[test]
        fn open_set_browses_folders_and_a_folder_entry_chooses_the_set() {
            let directory = tempfile::tempdir().expect("temp dir");
            let sets = directory.path().join("sets");
            let other = sets.join("other");
            std::fs::create_dir_all(&other).expect("other");
            std::fs::write(other.join("a.strudel"), "$: s(\"oh\")\n").expect("a");
            std::fs::write(other.join("b.strudel"), "$: s(\"cp\")\n").expect("b");
            std::fs::create_dir_all(sets.join("empty")).expect("empty");
            let home = directory.path().join("home");
            std::fs::create_dir_all(&home).expect("home");
            let mut app = app_over(&home);
            app.prefs.sets_directory = Some(sets.display().to_string());

            app.open_set_prompt(SetPrompt::OpenSet);
            let (prompt, picker) = app.set_prompt.as_ref().expect("the prompt is up");
            assert_eq!(*prompt, SetPrompt::OpenSet);
            assert_eq!(
                picker
                    .candidates
                    .iter()
                    .map(|candidate| (candidate.label.as_str(), candidate.detail.as_str()))
                    .collect::<Vec<_>>(),
                [("other", "2 scores")],
                "folders with scores, not empty ones"
            );
            press(&mut app, KeyCode::Tab, KeyModifiers::NONE);
            let browser = app
                .set_prompt
                .as_ref()
                .and_then(|(_, picker)| picker.browser.as_ref())
                .expect("browsing");
            assert_eq!(browser.dir, sets);
            assert_eq!(
                browser
                    .entries
                    .iter()
                    .map(|entry| entry.name.as_str())
                    .collect::<Vec<_>>(),
                ["..", ".", "empty", "other"]
            );
            press(&mut app, KeyCode::End, KeyModifiers::NONE);
            press(&mut app, KeyCode::Enter, KeyModifiers::NONE);
            let browser = app
                .set_prompt
                .as_ref()
                .and_then(|(_, picker)| picker.browser.as_ref())
                .expect("browsing");
            assert_eq!(browser.dir, other, "Enter went into the folder");
            assert_eq!(
                browser.entries[browser.selected].name, ".",
                "the folder itself is on offer"
            );
            press(&mut app, KeyCode::Enter, KeyModifiers::NONE);
            assert!(app.set_prompt.is_none());
            assert_eq!(app.scenes.directory(), other);
            assert_eq!(app.scenes.score_count(), 2);
            assert_eq!(app.focus, Focus::Editor);
            assert_eq!(app.prefs.last_set.as_deref(), other.to_str());
            assert_eq!(app.prefs.recent_sets[0], other.display().to_string());
            assert!(app.status.starts_with("opened set"), "{}", app.status);

            // A set file is not a set.
            app.open_set_prompt(SetPrompt::OpenSet);
            for character in other.join("rustel-set.json").display().to_string().chars() {
                press(&mut app, KeyCode::Char(character), KeyModifiers::NONE);
            }
            press(&mut app, KeyCode::Enter, KeyModifiers::NONE);
            assert!(
                app.set_prompt
                    .as_ref()
                    .and_then(|(_, picker)| picker.error.as_deref())
                    .is_some_and(|error| error.contains("set file")),
                "{:?}",
                app.set_prompt
                    .as_ref()
                    .and_then(|(_, picker)| picker.error.clone())
            );
        }

        /// File > Open set… offers the sets root's Strudel pattern
        /// exports beside its sets, so an export opens with Enter the way
        /// a set does. A json that is not an export is not offered.
        #[test]
        fn open_set_offers_the_strudel_exports_beside_the_sets() {
            let directory = tempfile::tempdir().expect("temp dir");
            let sets = directory.path().join("sets");
            std::fs::create_dir_all(sets.join("guest")).expect("guest");
            std::fs::write(sets.join("guest").join("a.strudel"), "$: s(\"oh\")\n")
                .expect("a score");
            std::fs::write(
                sets.join("gig.json"),
                concat!(
                    "{\n",
                    "  \"V1StGXR8_Z5j\": { \"code\": \"$: s(\\\"bd*4\\\")\" }\n",
                    "}\n"
                ),
            )
            .expect("export");
            std::fs::write(sets.join("not one.json"), "[1, 2, 3]").expect("not an export");
            let home = directory.path().join("home");
            std::fs::create_dir_all(&home).expect("home");
            let mut app = app_over(&home);
            app.prefs.sets_directory = Some(sets.display().to_string());

            app.open_set_prompt(SetPrompt::OpenSet);
            let (_, picker) = app.set_prompt.as_ref().expect("the prompt is up");
            assert_eq!(picker.title, "open a set or a Strudel export");
            assert_eq!(
                picker
                    .candidates
                    .iter()
                    .map(|candidate| (candidate.label.as_str(), candidate.detail.as_str()))
                    .collect::<Vec<_>>(),
                [
                    ("guest", "1 score"),
                    ("gig.json", "Strudel export - 1 pattern"),
                ],
                "sets first, then the exports; a json that is not one stays out"
            );

            // Enter opens the export as a set, as the typed path always did.
            press(&mut app, KeyCode::Down, KeyModifiers::NONE);
            press(&mut app, KeyCode::Enter, KeyModifiers::NONE);
            assert!(app.set_prompt.is_none(), "the prompt closed");
            assert!(
                app.status.starts_with("imported gig - 1 scene(s)"),
                "{}",
                app.status
            );
            assert_eq!(app.scenes.directory(), sets.join("gig"));
        }

        /// File > Import patterns… reads an export into the set
        /// already open: one named score file an entry, in the set's own
        /// folder with no tab taken up, and a name the set already holds
        /// skipped and counted, never overwritten. The same file twice
        /// says there is nothing new in it.
        #[test]
        fn importing_strudel_patterns_adds_them_to_the_open_set() {
            let directory = tempfile::tempdir().expect("temp dir");
            let sets = directory.path().join("sets");
            std::fs::create_dir_all(&sets).expect("sets");
            let home = directory.path().join("home");
            std::fs::create_dir_all(&home).expect("home");
            let mut app = app_over(&home);
            app.prefs.sets_directory = Some(sets.display().to_string());
            // The set holds `first`; the export's `first` must lose to it.
            let starter = std::fs::read_to_string(home.join("first.strudel")).expect("starter");

            std::fs::write(
                sets.join("gig.json"),
                concat!(
                    "{\n",
                    "  \"XKOBdiQB6vMm\": { \"code\": \"$: s(\\\"oh\\\")\", \"created_at\": 30 },\n",
                    "  \"ALGnQXNiUsZ4\": { \"code\": \"// @title night drive\\n$: s(\\\"bd\\\")\", \"created_at\": 20 },\n",
                    "  \"first\": \"$: s(\\\"cp\\\")\"\n",
                    "}\n"
                ),
            )
            .expect("export");
            app.open_set_prompt(SetPrompt::ImportStrudel);
            let (_, picker) = app.set_prompt.as_ref().expect("the prompt is up");
            assert_eq!(
                picker
                    .candidates
                    .iter()
                    .map(|candidate| (candidate.label.as_str(), candidate.detail.as_str()))
                    .collect::<Vec<_>>(),
                [("gig.json", "Strudel export - 3 patterns")]
            );
            press(&mut app, KeyCode::Enter, KeyModifiers::NONE);

            assert!(app.set_prompt.is_none(), "the prompt closed");
            assert_eq!(
                app.status, "imported 2 patterns into home - 1 skipped, already in the set",
                "{}",
                app.status
            );
            assert_eq!(
                std::fs::read_to_string(home.join("night drive.strudel")).unwrap(),
                "// @title night drive\n$: s(\"bd\")"
            );
            assert_eq!(
                std::fs::read_to_string(home.join("pattern 3.strudel")).unwrap(),
                "$: s(\"oh\")"
            );
            assert_eq!(
                std::fs::read_to_string(home.join("first.strudel")).unwrap(),
                starter,
                "the set's own score was not overwritten"
            );
            assert_eq!(
                app.scenes.score_count(),
                1,
                "the import writes files, not tabs"
            );

            // The same file again brings nothing new and says so.
            app.open_set_prompt(SetPrompt::ImportStrudel);
            press(&mut app, KeyCode::Enter, KeyModifiers::NONE);
            assert_eq!(app.status, "no new patterns - all 3 are already in home");
        }

        /// A Strudel pattern export gives its patterns, or says what it is
        /// not. Strudel writes each entry as an object with a `code` field,
        /// and a plain string of code is taken too. Entries that hold
        /// neither shape are left out, and so are the documented form's
        /// `import` and `export` URL fields.
        #[test]
        fn a_strudel_export_gives_its_patterns_or_a_refusal() {
            let strudel = r#"{
              "V1StGXR8_Z5j": { "code": "$: s(\"bd*4\")", "created_at": 1, "collection": "user" },
              "second": { "code": "$: s(\"cp\")" },
              "plain string": "$: s(\"hh\")",
              "import": "https://strudel.cc/",
              "export": "https://strudel.cc/whatever",
              "third": { "not": "code" }
            }"#;
            assert_eq!(
                strudel_export(strudel).unwrap(),
                [
                    StrudelPattern {
                        key: "V1StGXR8_Z5j".to_owned(),
                        code: "$: s(\"bd*4\")".to_owned(),
                        created_at: Some(1),
                        key_is_name: false,
                    },
                    StrudelPattern {
                        key: "second".to_owned(),
                        code: "$: s(\"cp\")".to_owned(),
                        created_at: None,
                        key_is_name: false,
                    },
                    StrudelPattern {
                        key: "plain string".to_owned(),
                        code: "$: s(\"hh\")".to_owned(),
                        created_at: None,
                        key_is_name: true,
                    },
                ]
            );
            assert!(
                strudel_export(r#"[{ "code": "$: s(\"oh\")" }]"#)
                    .unwrap_err()
                    .contains("not a JSON object")
            );
            assert!(
                strudel_export("{ no closing brace")
                    .unwrap_err()
                    .contains("not a Strudel pattern export")
            );
            assert!(
                strudel_export(r#"{ "import": {} }"#)
                    .unwrap_err()
                    .contains("no scores in it")
            );
        }

        /// Every pattern takes a name a musician can read on a file. The
        /// `@title` inside the code leads, then the key when the export
        /// keys by name, then `pattern N` in the order the patterns were
        /// made - a push id is never a file name.
        #[test]
        fn strudel_patterns_take_readable_names_in_the_order_they_were_made() {
            // strudel.cc's own form: keys are push ids, and the file's key
            // order need not be the order the patterns were made in.
            let pushed = strudel_export(
                r#"{
                  "XKOBdiQB6vMm": { "code": "// @title night drive\n$: s(\"bd\")", "created_at": 30 },
                  "ALGnQXNiUsZ4": { "code": "$: s(\"cp\")", "created_at": 10 },
                  "SkySkxWlk9OI": { "code": "$: s(\"hh\")", "created_at": 20 }
                }"#,
            )
            .unwrap();
            assert_eq!(
                strudel_scores(pushed)
                    .into_iter()
                    .map(|(name, _)| name)
                    .collect::<Vec<_>>(),
                ["pattern 1", "pattern 2", "night drive"],
                "numbered by when they were made, the title leading its own"
            );

            // The documented form: keys are names, kept as they come.
            let named = strudel_export(
                r#"{
                  "vamp bass": "$: s(\"bd*4\")",
                  "beat": "$: s(\"cp\")",
                  "///??": "$: s(\"oh\")"
                }"#,
            )
            .unwrap();
            assert_eq!(
                strudel_scores(named)
                    .into_iter()
                    .map(|(name, _)| name)
                    .collect::<Vec<_>>(),
                ["vamp bass", "beat", "pattern 3"],
                "a key with no usable characters falls back to its number"
            );
        }

        /// Opening a Strudel pattern export makes a set of it: a folder
        /// named after the export file, one score per entry, the title
        /// naming the score that carries one and the rest numbered in the
        /// order they were made. A file that is not an export is refused
        /// and the prompt stays.
        #[test]
        fn opening_a_strudel_export_makes_a_set() {
            let directory = tempfile::tempdir().expect("temp dir");
            let sets = directory.path().join("sets");
            let home = directory.path().join("home");
            std::fs::create_dir_all(&home).expect("home");
            let mut app = app_over(&home);
            app.prefs.sets_directory = Some(sets.display().to_string());

            let export = directory.path().join("gig.json");
            std::fs::write(
                &export,
                concat!(
                    "{\n",
                    "  \"V1StGXR8_Z5j\": { \"code\": \"// @title night drive\\n$: s(\\\"bd*4\\\")\" },\n",
                    "  \"kZ4_hT9pQw2x\": { \"code\": \"$: s(\\\"cp\\\")\" }\n",
                    "}\n"
                ),
            )
            .expect("export");
            app.open_set_prompt(SetPrompt::OpenSet);
            for character in export.display().to_string().chars() {
                press(&mut app, KeyCode::Char(character), KeyModifiers::NONE);
            }
            press(&mut app, KeyCode::Enter, KeyModifiers::NONE);

            assert!(app.set_prompt.is_none(), "the prompt closed");
            assert!(
                app.status.starts_with("imported gig - 2 scene(s)"),
                "{}",
                app.status
            );
            assert_eq!(app.scenes.directory(), sets.join("gig"));
            assert_eq!(app.focus, Focus::Editor);
            assert_eq!(
                app.scenes
                    .scores()
                    .map(|scene| scene.name().to_owned())
                    .collect::<Vec<_>>(),
                ["night drive", "pattern 2"],
                "the title leads when it is there, the rest are numbered"
            );
            assert_eq!(app.scenes.current().name(), "night drive");
            assert_eq!(
                std::fs::read_to_string(sets.join("gig").join("night drive.strudel")).unwrap(),
                "// @title night drive\n$: s(\"bd*4\")"
            );

            // A json file that is not an export says so and stays refused.
            let not_an_export = directory.path().join("solo.json");
            std::fs::write(&not_an_export, "[1, 2, 3]").expect("not an export");
            app.open_set_prompt(SetPrompt::OpenSet);
            for character in not_an_export.display().to_string().chars() {
                press(&mut app, KeyCode::Char(character), KeyModifiers::NONE);
            }
            press(&mut app, KeyCode::Enter, KeyModifiers::NONE);
            assert!(
                app.set_prompt
                    .as_ref()
                    .and_then(|(_, picker)| picker.error.as_deref())
                    .is_some_and(|error| error.contains("not a Strudel pattern export")),
                "{:?}",
                app.set_prompt
                    .as_ref()
                    .and_then(|(_, picker)| picker.error.clone())
            );
            assert_eq!(app.scenes.directory(), sets.join("gig"), "still in the set");
        }

        /// The set list starts with the set changed last, whatever its name.
        /// The newest file in a set gives the set its time. Sets with the
        /// same time stay in name order.
        #[test]
        fn the_set_list_leads_with_the_set_changed_last() {
            let directory = tempfile::tempdir().expect("temp dir");
            let sets = directory.path().join("sets");
            let home = directory.path().join("home");
            std::fs::create_dir_all(&home).expect("home");
            let mut app = app_over(&home);
            app.prefs.sets_directory = Some(sets.display().to_string());
            // The score times are ahead of the clock, so each one is newer
            // than the folder's own time.
            let ahead = std::time::SystemTime::now() + std::time::Duration::from_secs(3_600);
            for (name, seconds) in [("alpha", 10), ("beta", 30), ("delta", 20), ("gamma", 20)] {
                let path = sets.join(name);
                std::fs::create_dir_all(&path).expect("set folder");
                let score = path.join("a.strudel");
                std::fs::write(&score, "$: s(\"oh\")\n").expect("score");
                std::fs::File::options()
                    .write(true)
                    .open(&score)
                    .and_then(|file| {
                        file.set_modified(ahead + std::time::Duration::from_secs(seconds))
                    })
                    .expect("score time");
            }
            app.open_set_prompt(SetPrompt::OpenSet);
            let picker = &app.set_prompt.as_ref().expect("prompt").1;
            assert_eq!(
                picker
                    .candidates
                    .iter()
                    .map(|candidate| candidate.label.as_str())
                    .collect::<Vec<_>>(),
                ["beta", "delta", "gamma", "alpha"]
            );
        }

        #[test]
        fn double_clicking_a_set_picker_row_opens_the_clicked_set() {
            for prompt in [SetPrompt::OpenRecent, SetPrompt::OpenSet] {
                let directory = tempfile::tempdir().expect("temp dir");
                let sets = directory.path().join("sets");
                let home = directory.path().join("home");
                std::fs::create_dir_all(&home).expect("home");
                let mut app = app_over(&home);
                app.frame = Rect::new(0, 0, 120, 40);
                app.prefs.sets_directory = Some(sets.display().to_string());
                app.prefs.recent_sets.clear();
                for name in ["first", "second"] {
                    let path = sets.join(name);
                    std::fs::create_dir_all(&path).expect("set folder");
                    std::fs::write(path.join("a.strudel"), "$: s(\"oh\")\n").expect("score");
                    app.prefs.remember_set(&path);
                }
                app.open_set_prompt(prompt);
                let picker = &app.set_prompt.as_ref().expect("prompt").1;
                let (_, list) = picker.geometry(app.frame).expect("list");
                let chosen = picker.candidates[1].path.clone();
                let click = MouseEvent {
                    kind: MouseEventKind::Down(MouseButton::Left),
                    column: list.x + 3,
                    row: list.y,
                    modifiers: KeyModifiers::NONE,
                };
                app.handle_mouse(click).expect("first row click");
                let click = MouseEvent {
                    row: list.y + 1,
                    ..click
                };
                app.handle_mouse(click).expect("second row click");
                assert_eq!(app.scenes.directory(), home, "different rows only select");
                assert_eq!(app.set_prompt.as_ref().expect("still open").1.selected, 1);

                // Moving away and back by keyboard breaks the pending click pair.
                press(&mut app, KeyCode::Up, KeyModifiers::NONE);
                press(&mut app, KeyCode::Down, KeyModifiers::NONE);
                app.handle_mouse(click).expect("first click after keys");
                app.handle_mouse(MouseEvent {
                    kind: MouseEventKind::Up(MouseButton::Left),
                    ..click
                })
                .expect("release");
                assert_eq!(app.scenes.directory(), home, "one click only selects");
                assert!(app.set_prompt.is_some());

                app.handle_mouse(click).expect("double click");
                assert!(app.set_prompt.is_none(), "opening closes the picker");
                assert_eq!(app.scenes.directory(), chosen);
                assert_eq!(app.scenes.current().name(), "a");
                assert_eq!(app.focus, Focus::Editor);
                assert_eq!(app.prefs.last_set.as_deref(), chosen.to_str());
                assert!(app.status.starts_with("opened set"), "{}", app.status);
            }
        }

        /// File > New set makes a folder named after the day under the sets
        /// folder with a starter scene, and takes it up; Rename set… moves
        /// the folder; Open recent… offers what was open before.
        #[test]
        fn sets_come_and_go_from_the_file_menu() {
            let directory = tempfile::tempdir().expect("temp dir");
            let sets = directory.path().join("sets");
            let home = directory.path().join("home");
            std::fs::create_dir_all(&home).expect("home");
            let mut app = app_over(&home);
            app.prefs.sets_directory = Some(sets.display().to_string());
            assert_eq!(
                app.prefs.last_set.as_deref(),
                home.to_str(),
                "the set opened is remembered"
            );
            // The preferences are one file for the whole test process: only
            // this set on the list, so the order below is this test's own.
            app.prefs.recent_sets.clear();
            app.prefs.remember_set(&home);

            app.dispatch_menu_action(crate::menu::MenuAction::NewSet)
                .expect("menu");
            let day = today();
            assert_eq!(app.scenes.directory(), sets.join(&day));
            assert_eq!(app.scenes.name(), day);
            assert_eq!(app.scenes.score_count(), 1);
            assert!(sets.join(&day).join("scene 1.strudel").exists());
            assert!(
                sets.join(&day).join("rustel-set.json").exists(),
                "made by the studio, it has its set file at once"
            );
            assert_eq!(app.prefs.last_set.as_deref(), sets.join(&day).to_str());
            assert_eq!(
                app.prefs.recent_sets,
                [
                    sets.join(&day).display().to_string(),
                    home.display().to_string()
                ]
            );
            assert!(app.status.starts_with("new set"), "{}", app.status);

            app.dispatch_menu_action(crate::menu::MenuAction::NewSet)
                .expect("menu");
            assert_eq!(
                app.scenes.name(),
                format!("{day} b"),
                "the day already has one"
            );

            app.dispatch_menu_action(crate::menu::MenuAction::RenameSet)
                .expect("menu");
            let (prompt, picker) = app.set_prompt.as_ref().expect("prompt");
            assert_eq!(*prompt, SetPrompt::RenameSet);
            assert_eq!(picker.path, format!("{day} b"), "the name is there to edit");
            for _ in 0..picker.path.chars().count() {
                press(&mut app, KeyCode::Backspace, KeyModifiers::NONE);
            }
            for character in "opening night".chars() {
                press(&mut app, KeyCode::Char(character), KeyModifiers::NONE);
            }
            press(&mut app, KeyCode::Enter, KeyModifiers::NONE);
            assert!(app.set_prompt.is_none());
            assert_eq!(app.scenes.directory(), sets.join("opening night"));
            assert!(sets.join("opening night").join("scene 1.strudel").exists());
            assert!(!sets.join(format!("{day} b")).exists());
            assert_eq!(
                app.scenes.current().path,
                sets.join("opening night").join("scene 1.strudel")
            );
            assert_eq!(
                app.prefs.last_set.as_deref(),
                sets.join("opening night").to_str()
            );
            assert!(
                !app.prefs
                    .recent_sets
                    .iter()
                    .any(|recent| recent.ends_with(&format!("{day} b")))
            );

            // A name in use is refused, in the prompt.
            app.dispatch_menu_action(crate::menu::MenuAction::RenameSet)
                .expect("menu");
            for _ in 0.."opening night".len() {
                press(&mut app, KeyCode::Backspace, KeyModifiers::NONE);
            }
            for character in day.chars() {
                press(&mut app, KeyCode::Char(character), KeyModifiers::NONE);
            }
            press(&mut app, KeyCode::Enter, KeyModifiers::NONE);
            assert!(
                app.set_prompt
                    .as_ref()
                    .and_then(|(_, picker)| picker.error.as_deref())
                    .is_some_and(|error| error.contains("already in use"))
            );
            press(&mut app, KeyCode::Esc, KeyModifiers::NONE);
            assert!(app.set_prompt.is_none());

            // Open recent lists the others, newest first.
            app.dispatch_menu_action(crate::menu::MenuAction::OpenRecent)
                .expect("menu");
            let (prompt, picker) = app.set_prompt.as_ref().expect("prompt");
            assert_eq!(*prompt, SetPrompt::OpenRecent);
            assert_eq!(
                picker
                    .candidates
                    .iter()
                    .map(|candidate| candidate.label.as_str())
                    .collect::<Vec<_>>(),
                [day.as_str(), "home"]
            );
            press(&mut app, KeyCode::Enter, KeyModifiers::NONE);
            assert_eq!(app.scenes.directory(), sets.join(&day));
            assert_eq!(app.focus, Focus::Editor);
        }

        #[test]
        fn set_panel_resize_keeps_selection_and_remembers_the_preferred_width() {
            use crate::set_panel;

            let directory = tempfile::tempdir().unwrap();
            let mut app = app_over(directory.path());
            app.frame = Rect::new(0, 0, 140, 40);
            app.open_set_panel(true);
            let selected = app.set_panel.as_ref().unwrap().selected;
            let source = app.editor().source();
            assert_eq!(app.set_panel.as_ref().unwrap().width, 40);
            press(&mut app, KeyCode::Char('+'), KeyModifiers::SHIFT);
            assert_eq!(app.set_panel.as_ref().unwrap().width, 44);
            press(&mut app, KeyCode::Char('-'), KeyModifiers::NONE);
            assert_eq!(app.set_panel.as_ref().unwrap().width, 40);
            for _ in 0..30 {
                press(&mut app, KeyCode::Char('-'), KeyModifiers::NONE);
            }
            assert_eq!(
                app.set_panel.as_ref().unwrap().width,
                set_panel::SIDEBAR_MIN_WIDTH
            );
            for _ in 0..30 {
                press(&mut app, KeyCode::Char('='), KeyModifiers::NONE);
            }
            assert_eq!(
                app.set_panel.as_ref().unwrap().width,
                set_panel::SIDEBAR_MAX_WIDTH
            );
            assert_eq!(app.set_panel.as_ref().unwrap().selected, selected);
            assert_eq!(app.editor().source(), source);
            app.flush_prefs();
            assert_eq!(
                StudioPrefs::load().set_panel_width,
                Some(set_panel::SIDEBAR_MAX_WIDTH)
            );

            let narrow = view::regions_with_footer(
                Rect::new(0, 0, 70, 40),
                view::ChromeLayout::shown(false),
                1,
                false,
                true,
                app.set_sidebar(),
                app.dock_requests(),
                None,
                None,
                None,
                false,
                0,
                false,
            );
            assert_eq!(narrow.sidebar.width, 30);
            assert_eq!(
                app.set_panel.as_ref().unwrap().width,
                set_panel::SIDEBAR_MAX_WIDTH
            );
            press(&mut app, KeyCode::Char('b'), KeyModifiers::CONTROL);
            assert!(app.set_panel.is_none());
            app.open_set_panel(true);
            assert_eq!(
                app.set_panel.as_ref().unwrap().width,
                set_panel::SIDEBAR_MAX_WIDTH
            );
        }

        /// The set panel keeps its place when Esc gives the keyboard back,
        /// including as a sheet on a narrow terminal. A double click opens a row.
        #[test]
        fn the_set_panel_docks_when_wide_and_esc_keeps_it() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = app_over(directory.path());
            app.new_scene(false);
            press(&mut app, KeyCode::Char('w'), KeyModifiers::CONTROL);
            press(&mut app, KeyCode::Char('b'), KeyModifiers::CONTROL);
            app.frame = Rect::new(0, 0, 120, 40);
            app.regions = view::regions(
                app.frame,
                false,
                1,
                false,
                true,
                Some(app.set_panel_side()),
                None,
            );
            assert!(!app.regions.sidebar.is_empty());
            assert_eq!(
                app.regions.sidebar_side,
                view::Side::Right,
                "the right, by default"
            );
            assert_eq!(app.regions.sidebar.right(), app.frame.right());
            assert!(app.set_panel.as_ref().is_some_and(|panel| panel.on_right));
            press(&mut app, KeyCode::Esc, KeyModifiers::NONE);
            assert!(
                app.set_panel.is_some(),
                "docked, Esc only hands the keyboard back"
            );
            assert_eq!(app.focus, Focus::Editor);
            press(&mut app, KeyCode::Esc, KeyModifiers::NONE);
            assert!(app.set_panel.is_some(), "Esc from the score leaves it too");

            let list = app
                .set_panel
                .as_ref()
                .and_then(|panel| panel.list_area(app.regions.sidebar, app.frame))
                .expect("a list");
            let click = MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: list.x + 3,
                row: list.y + 1,
                modifiers: KeyModifiers::NONE,
            };
            app.handle_mouse(click).expect("click");
            assert_eq!(app.focus, Focus::Panel(PanelKind::Set));
            assert_eq!(app.scenes.score_count(), 1, "one click only chooses");
            app.handle_mouse(click).expect("click");
            assert_eq!(app.scenes.score_count(), 2, "the second opens the score");
            assert_eq!(app.scenes.current().name(), "scene 1");

            // With room for both, the reference column sits inside the set
            // panel. Narrower layouts temporarily hide the set for reference.
            app.frame = Rect::new(0, 0, 180, 40);
            app.regions = view::regions(
                app.frame,
                false,
                1,
                true,
                true,
                Some(app.set_panel_side()),
                None,
            );
            assert_eq!(app.regions.reference.right(), app.regions.sidebar.x);
            let list = app
                .set_panel
                .as_ref()
                .and_then(|panel| panel.list_area(app.regions.sidebar, app.frame))
                .expect("a list");
            assert_eq!(
                list.x,
                app.regions.sidebar.x + 2,
                "past the rule on the left"
            );
            app.ui_settings.set_panel_right = false;
            app.apply_ui_settings();
            assert!(app.set_panel.as_ref().is_some_and(|panel| !panel.on_right));
            app.regions = view::regions(
                app.frame,
                false,
                1,
                true,
                true,
                Some(app.set_panel_side()),
                None,
            );
            assert_eq!(app.regions.sidebar_side, view::Side::Left);
            assert_eq!(app.regions.sidebar.x, 0);
            assert!(app.regions.reference.x > app.regions.sidebar.right());

            // In a narrow terminal the panel is a sheet, and Esc keeps it open
            // there too.
            app.frame = Rect::new(0, 0, 60, 24);
            app.regions = view::regions(
                app.frame,
                false,
                1,
                false,
                true,
                Some(app.set_panel_side()),
                None,
            );
            assert!(app.regions.sidebar.is_empty());
            app.focus_panel(PanelKind::Set);
            press(&mut app, KeyCode::Esc, KeyModifiers::NONE);
            assert!(app.set_panel.is_some());
            assert_eq!(app.focus, Focus::Editor);
            // And ^B, the one thing that does hide it.
            press(&mut app, KeyCode::Char('b'), KeyModifiers::CONTROL);
            assert!(app.set_panel.is_none());
            assert_eq!(app.prefs.set_panel, Some(false));
            assert_eq!(app.focus, Focus::Editor);
        }

        /// A bare launch opens the set open last time, or makes a dated one in
        /// the sets folder when there is none; a path named wins.
        #[test]
        fn a_bare_launch_opens_the_last_set_or_makes_one() {
            crate::config::isolate_for_tests();
            let directory = tempfile::tempdir().expect("temp dir");
            let sets = directory.path().join("sets");
            let mut prefs = StudioPrefs {
                sets_directory: Some(sets.display().to_string()),
                ..StudioPrefs::default()
            };
            let made = open_startup_set(None, &prefs, "// starter").expect("a new set");
            assert_eq!(made.directory(), sets.join(today()));
            assert_eq!(made.current().editor.source(), "// starter");
            let named = directory.path().join("named");
            std::fs::create_dir_all(&named).expect("named");
            std::fs::write(named.join("x.strudel"), "// x").expect("x");
            prefs.remember_set(&named);
            let last = open_startup_set(None, &prefs, "// starter").expect("the last set");
            assert_eq!(last.directory(), named);
            // Gone from the disk, the last set is no set: a new one instead.
            prefs.remember_set(&directory.path().join("gone"));
            let fresh = open_startup_set(None, &prefs, "// starter").expect("a new set");
            assert_eq!(fresh.directory(), sets.join(format!("{} b", today())));
            let chosen =
                open_startup_set(Some(&named), &prefs, "// starter").expect("the path named");
            assert_eq!(chosen.directory(), named);
        }

        /// Delete on a tape asks and Enter deletes; Space opens and shuts the
        /// fold the tapes sit under.
        #[test]
        fn the_set_panel_deletes_a_tape_after_asking() {
            let directory = tempfile::tempdir().expect("temp dir");
            let tapes = directory.path().join("sessions");
            std::fs::create_dir_all(&tapes).expect("sessions");
            let old = write_tape(&tapes, "2026-09-01T10-00-00", &[(0.0, "$: s(\"bd\")")]);
            let new = write_tape(&tapes, "2026-09-02T10-00-00", &[(0.0, "$: s(\"sd\")")]);
            let mut app = app_over(directory.path());
            press(&mut app, KeyCode::Char('b'), KeyModifiers::CONTROL);
            press(&mut app, KeyCode::End, KeyModifiers::NONE);
            press(&mut app, KeyCode::Char(' '), KeyModifiers::NONE);
            assert!(app.set_panel.as_ref().expect("open").sessions_open);
            press(&mut app, KeyCode::Down, KeyModifiers::NONE);
            press(&mut app, KeyCode::Down, KeyModifiers::NONE);
            assert_eq!(
                app.set_panel
                    .as_ref()
                    .and_then(SetPanel::selected_tape)
                    .expect("a tape")
                    .path,
                old,
                "newest first"
            );
            // Alt+R renames the selected tape.
            press(&mut app, KeyCode::Char('r'), KeyModifiers::ALT);
            assert!(matches!(
                app.set_prompt,
                Some((SetPrompt::RenameSession, _))
            ));
            press(&mut app, KeyCode::Esc, KeyModifiers::NONE);
            assert!(app.set_prompt.is_none());
            press(&mut app, KeyCode::Delete, KeyModifiers::NONE);
            assert!(old.exists(), "Delete only asks");
            press(&mut app, KeyCode::Enter, KeyModifiers::NONE);
            assert!(!old.exists());
            assert!(new.exists());
            assert_eq!(app.set_panel.as_ref().expect("open").tapes.len(), 1);
            // The cursor stays in the list: the tape that took its place, not
            // the top of the panel.
            assert!(
                matches!(
                    app.set_panel.as_ref().expect("open").selected_line(),
                    Some(SetLine::Tape(0))
                ),
                "left the tapes: {:?}",
                app.set_panel.as_ref().expect("open").selected_line()
            );
            assert!(
                app.set_panel
                    .as_ref()
                    .and_then(SetPanel::selected_tape)
                    .is_some_and(|tape| tape.path == new)
            );
            press(&mut app, KeyCode::Char(' '), KeyModifiers::NONE);
            assert!(!app.set_panel.as_ref().expect("open").sessions_open);
            // Esc hands the keyboard back and leaves the panel - in the
            // sheet form as in the docked one. Only ^B hides it.
            press(&mut app, KeyCode::Esc, KeyModifiers::NONE);
            assert!(app.set_panel.is_some());
            assert_eq!(app.focus, Focus::Editor);
        }

        /// In zen the set panel is a popup, so Esc closes it. Outside zen Esc
        /// only returns focus to the score.
        #[test]
        fn esc_closes_the_set_panel_in_zen_instead_of_merely_unfocusing_it() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = app_over(directory.path());
            app.frame = Rect::new(0, 0, 120, 40);
            press(&mut app, KeyCode::F(11), KeyModifiers::NONE);
            press(&mut app, KeyCode::Char('b'), KeyModifiers::CONTROL);
            assert!(app.set_panel.is_some());
            assert_eq!(app.focus, Focus::Panel(PanelKind::Set));

            press(&mut app, KeyCode::Esc, KeyModifiers::NONE);
            assert!(
                app.set_panel.is_none(),
                "Esc did not close the set panel in zen"
            );
            assert_eq!(app.focus, Focus::Editor);
        }

        /// ^B is the plain switch and must still close the popup in zen, not
        /// just the docked panel: a player who missed Esc reaches for the
        /// same key that opened it, exactly as they would outside zen.
        #[test]
        fn ctrl_b_still_toggles_the_set_panel_closed_in_zen() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = app_over(directory.path());
            app.frame = Rect::new(0, 0, 120, 40);
            press(&mut app, KeyCode::F(11), KeyModifiers::NONE);
            press(&mut app, KeyCode::Char('b'), KeyModifiers::CONTROL);
            assert!(app.set_panel.is_some());
            assert_eq!(app.focus, Focus::Panel(PanelKind::Set));

            press(&mut app, KeyCode::Char('b'), KeyModifiers::CONTROL);
            assert!(
                app.set_panel.is_none(),
                "^B did not close the set panel in zen"
            );
            assert_eq!(app.focus, Focus::Editor);
        }

        /// Outside zen the set panel is the set, not a dialog, and Esc only
        /// gives the keyboard back - ^B is still the only thing that hides
        /// it. Zen's popup must not have changed that.
        #[test]
        fn esc_still_only_unfocuses_the_set_panel_outside_zen() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = app_over(directory.path());
            app.frame = Rect::new(0, 0, 120, 40);
            press(&mut app, KeyCode::Char('b'), KeyModifiers::CONTROL);
            assert!(app.set_panel.is_some());

            press(&mut app, KeyCode::Esc, KeyModifiers::NONE);
            assert!(
                app.set_panel.is_some(),
                "Esc closed the set panel outside zen"
            );
            assert_eq!(app.focus, Focus::Editor);
        }

        #[test]
        fn settings_pickers_return_to_the_same_page_and_row() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = app_over(directory.path());

            let mut sources = SettingsSheet::default();
            sources.show_page(crate::settings::SettingsPage::Sources);
            sources.selected = 3;
            app.settings_sheet = Some(sources);
            app.open_set_prompt(SetPrompt::EditSampleSource(0));
            assert_eq!(
                app.settings_sheet.unwrap().page(),
                crate::settings::SettingsPage::Sources
            );
            assert_eq!(app.settings_sheet.unwrap().selected, 3);
            assert!(app.handle_set_prompt_key(KeyCode::Esc, false, false));
            assert_eq!(app.focus, Focus::Panel(PanelKind::Settings));
            assert_eq!(
                app.settings_sheet.unwrap().page(),
                crate::settings::SettingsPage::Sources
            );
            assert_eq!(app.settings_sheet.unwrap().selected, 3);

            let mut settings = SettingsSheet::default();
            settings.show_page(crate::settings::SettingsPage::Settings);
            settings.selected = 4;
            app.settings_sheet = Some(settings);
            app.open_set_prompt(SetPrompt::SetsFolder);
            assert_eq!(
                app.set_prompt.as_ref().unwrap().1.path,
                shorten_home(&app.sets_root()),
                "the current folder is ready to keep or replace"
            );
            let chosen = directory.path().join("sets");
            app.choose_sets_folder(&chosen);
            assert_eq!(app.focus, Focus::Panel(PanelKind::Settings));
            assert_eq!(
                app.settings_sheet.unwrap().page(),
                crate::settings::SettingsPage::Settings
            );
            assert_eq!(app.settings_sheet.unwrap().selected, 4);

            let mut settings = SettingsSheet::default();
            settings.show_page(crate::settings::SettingsPage::Settings);
            settings.selected = 5;
            app.settings_sheet = Some(settings);
            app.open_set_prompt(SetPrompt::RecordingsFolder);
            assert_eq!(
                app.set_prompt.as_ref().unwrap().1.path,
                shorten_home(&app.recordings_directory()),
                "the current folder is ready to keep or replace"
            );
            let recordings = directory.path().join("my recordings");
            app.choose_recordings_folder(&recordings);
            assert_eq!(app.focus, Focus::Panel(PanelKind::Settings));
            assert_eq!(app.settings_sheet.unwrap().selected, 5);
            assert_eq!(
                app.prefs.recordings_directory.as_deref(),
                recordings.to_str()
            );
            assert_eq!(app.recordings_directory(), recordings);
            assert_ne!(app.recordings_directory(), app.sessions_directory());
        }

        /// The recordings picker refuses a set's own output folder: takes there
        /// would never become samples. The picker stays up and says why.
        #[test]
        fn the_recordings_picker_refuses_a_sets_output_folder() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = app_over(directory.path());
            let before = app.prefs.recordings_directory.clone();
            for folder in [
                app.scenes.sessions_directory(),
                directory
                    .path()
                    .join(rustel_runtime::product::EXPORT_DIRECTORY_NAME)
                    .join("mixes"),
            ] {
                app.open_set_prompt(SetPrompt::RecordingsFolder);
                app.choose_recordings_folder(&folder);
                let error = app
                    .set_prompt
                    .as_ref()
                    .and_then(|(_, picker)| picker.error.clone())
                    .unwrap_or_else(|| panic!("{} was taken", folder.display()));
                assert!(error.contains("a set's own"), "{error}");
                assert_eq!(app.prefs.recordings_directory, before);
                assert!(!folder.exists(), "nothing was made for a refused folder");
                app.close_set_prompt();
            }
        }

        /// Scene > Delete scene refreshes the panel onto the current scene and
        /// then arms the selected line. An unsaved starter, as
        /// `rustel studio newidea.strudel` opens, is in no listing, so the panel
        /// falls back to its first row. The command must not arm that row.
        #[test]
        fn deleting_an_unwritten_scene_arms_nothing_and_says_so() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = app_over(directory.path());
            let stranger = directory.path().join("first.strudel");
            let unwritten = directory.path().join("newidea.strudel");
            let set = SceneSet::open(&unwritten, "// starter").expect("set");
            app.switch_set(set, "opened set");
            assert_eq!(app.scenes.current().path, unwritten);

            app.arm_delete_scene();
            assert!(
                app.set_panel
                    .as_ref()
                    .is_some_and(|panel| panel.deleting.is_none()),
                "nothing is armed: {}",
                app.status
            );
            assert!(
                app.status.contains("nothing to delete"),
                "and it says why: {}",
                app.status
            );
            app.confirm_delete_set_line();
            assert!(
                stranger.exists(),
                "the score nobody asked about is still there"
            );
        }

        /// The same menu on a scene the folder does hold still arms that one.
        #[test]
        fn deleting_a_written_scene_arms_that_scene() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = app_over(directory.path());
            std::fs::write(directory.path().join("second.strudel"), "$: s(\"hh\")\n")
                .expect("second scene");
            let set = SceneSet::open(&directory.path().join("second.strudel"), "// starter")
                .expect("set");
            app.switch_set(set, "opened set");

            app.arm_delete_scene();
            assert!(
                app.set_panel
                    .as_ref()
                    .is_some_and(|panel| panel.deleting == Some(panel.selected)),
                "the scene is armed: {}",
                app.status
            );
            assert!(app.status.contains("second"), "{}", app.status);
        }

        /// The bank a set's folder brought, as the browser would show it.
        fn set_bank_location(app: &App, name: &str) -> Option<String> {
            app.worker
                .library()
                .expect("the studio has a sample library")
                .catalogue()
                .into_iter()
                .find(|entry| entry.name == name)
                .and_then(|entry| entry.location)
        }

        /// Opening a set takes up the audio beside its scores, and leaving it
        /// puts that audio down again. Nothing here adopts by hand: a launch
        /// that reaches a constructed `App` is already looking at its own
        /// folder.
        #[test]
        fn a_set_brings_the_samples_in_its_own_folder() {
            let directory = tempfile::tempdir().expect("temp dir");
            std::fs::create_dir_all(directory.path().join("kicks")).expect("bank folder");
            std::fs::write(directory.path().join("kicks").join("1.wav"), b"").expect("sample");
            let mut app = app_over(directory.path());
            let library = app
                .worker
                .library()
                .expect("the studio has a sample library");
            assert!(
                library.knows("kicks"),
                "the set's folder is a sample source"
            );

            let elsewhere = tempfile::tempdir().expect("second set");
            std::fs::write(elsewhere.path().join("first.strudel"), "$: s(\"bd\")\n")
                .expect("scene");
            let set = SceneSet::open(elsewhere.path(), "// starter").expect("set");
            app.switch_set(set, "opened set");
            assert!(
                !library.knows("kicks"),
                "and the set that closed took it with it"
            );
        }

        /// An update re-reads the open set's folder before the check judges the
        /// score's sound names, so audio added since the last read plays from it.
        #[test]
        fn update_picks_up_audio_added_to_the_open_set() {
            let directory = tempfile::tempdir().expect("temp dir");
            std::fs::create_dir(directory.path().join("kicks")).expect("bank folder");
            std::fs::write(directory.path().join("kicks/1.wav"), b"").expect("first variant");
            let mut app = app_over(directory.path());
            // A library with no manifests on their way, so the check judges names.
            app.worker
                .set_library(Arc::new(rustel_runtime::samples::SampleLibrary::empty()));
            app.adopt_set_samples();
            let library = app.worker.library().expect("sample library");
            app.reference_panel =
                Some(ReferencePanel::samples(&app.reference, library.catalogue()));

            std::fs::write(directory.path().join("kicks/2.wav"), b"").expect("second variant");
            std::fs::create_dir(directory.path().join("fresh")).expect("new bank folder");
            std::fs::write(directory.path().join("fresh/1.wav"), b"").expect("new bank");
            assert!(
                !library.knows("fresh"),
                "the set has not been rescanned yet"
            );
            let source = "$: s(\"fresh\")";
            app.scenes.current_mut().editor = Editor::new(source).expect("score");
            let request_id = app.next_request_id;
            press(&mut app, KeyCode::Char('s'), KeyModifiers::CONTROL);
            assert!(library.knows("fresh"), "the new sound is playable");
            assert_eq!(app.next_request_id, request_id + 1, "the score passed lint");
            assert!(
                app.pending_evaluation.is_some() || !app.inflight.is_empty(),
                "the update was sent to the engine"
            );

            let deadline = Instant::now() + Duration::from_secs(10);
            while !app.worker.poll_catalogue() {
                assert!(Instant::now() < deadline, "the catalogue was not rebuilt");
                std::thread::sleep(Duration::from_millis(5));
            }
            app.install_catalogue();
            let panel = app.reference_panel.as_ref().expect("samples browser");
            assert!(panel.sounds.iter().any(|sound| sound.name == "fresh"));
            assert_eq!(
                panel
                    .sounds
                    .iter()
                    .find(|sound| sound.name == "kicks")
                    .map(|sound| sound.variants),
                Some(2)
            );
        }

        /// Renaming a set moves its folder. The banks from that folder must name
        /// the new path, or the browser lists a sound that the loader cannot
        /// open.
        #[test]
        fn a_renamed_set_keeps_the_samples_in_its_folder_playable() {
            let parent = tempfile::tempdir().expect("temp parent");
            let directory = parent.path().join("monday");
            std::fs::create_dir_all(directory.join("kicks")).expect("bank folder");
            std::fs::write(directory.join("kicks").join("1.wav"), b"").expect("sample");
            let mut app = app_over(&directory);
            assert!(
                set_bank_location(&app, "kicks").is_some(),
                "the set's folder is a sample source before the rename"
            );

            app.rename_set("tuesday");
            assert_eq!(app.scenes.name(), "tuesday", "the set took the new name");
            let location = set_bank_location(&app, "kicks").expect("still a bank");
            let path = location
                .strip_prefix("file://")
                .unwrap_or_else(|| panic!("a file URL, not {location}"));
            assert!(
                std::path::Path::new(path).exists(),
                "the bank names a file that is there: {location}"
            );
        }

        /// A sample source inside a set's folder follows the set when it is
        /// renamed, in the preferences and in the library - also when the set
        /// was opened by a relative path, as `rustel studio monday/` opens it,
        /// while the source is absolute.
        #[test]
        fn a_renamed_set_carries_the_sample_sources_inside_it() {
            let here = std::env::current_dir().expect("working directory");
            let parent = tempfile::tempdir_in(&here).expect("temp parent");
            let relative = parent
                .path()
                .strip_prefix(&here)
                .expect("made in the working directory")
                .join("monday");
            let kit = parent.path().join("monday").join("kit");
            std::fs::create_dir_all(&kit).expect("kit folder");
            std::fs::write(kit.join("kick.wav"), b"").expect("sample");
            let mut app = app_over(&relative);
            let spec = kit.display().to_string();
            assert_eq!(app.add_sample_sources(std::slice::from_ref(&spec)), 1);
            app.settle_sample_sources();

            app.rename_set("tuesday");

            assert_eq!(app.scenes.name(), "tuesday", "the set took the new name");
            let moved = parent
                .path()
                .join("tuesday")
                .join("kit")
                .display()
                .to_string();
            assert_eq!(app.prefs.sample_sources.len(), 1);
            assert_eq!(app.prefs.sample_sources[0].spec, moved);
            app.settle_sample_sources();
            let state = app
                .source_reports
                .iter()
                .find(|report| report.spec == moved)
                .map(|report| report.state.clone());
            assert_eq!(
                state,
                Some(rustel_runtime::samples::GlobalSourceState::Ready { banks: 1 })
            );
        }

        #[test]
        fn sample_file_rename_keeps_selection_focus_and_the_bank_address() {
            let directory = tempfile::tempdir().unwrap();
            let mut app = app_over(directory.path());
            let takes = directory.path().join("takes");
            std::fs::create_dir_all(&takes).unwrap();
            std::fs::write(takes.join("a.wav"), b"first").unwrap();
            std::fs::write(takes.join("b.wav"), b"second").unwrap();
            app.add_sample_sources(&[takes.display().to_string()]);
            app.settle_sample_sources();
            let library = app.worker.library().unwrap();
            app.open_set_panel(false);
            app.reference_panel =
                Some(ReferencePanel::samples(&app.reference, library.catalogue()));
            app.focus_panel(PanelKind::Reference);
            let panel = app.reference_panel.as_mut().unwrap();
            for (section, _) in panel.visible_categories() {
                panel.open_categories.insert(section);
            }
            panel.sound_selected = panel
                .sound_rows()
                .iter()
                .position(
                    |row| matches!(row, SoundRow::Bank(i) if panel.sounds[*i].name == "takes"),
                )
                .unwrap();
            assert!(panel.expand());
            panel.move_by(1);
            let selected = panel.sound_selected;
            // Cancelling must neither rename the file nor move selection.
            app.handle_reference_key(KeyCode::Char('r'), false, true)
                .unwrap();
            assert!(matches!(app.set_prompt, Some((SetPrompt::RenameSample, _))));
            app.handle_set_prompt_key(KeyCode::Esc, false, false);
            assert!(takes.join("a.wav").exists());
            assert_eq!(app.focus, Focus::Panel(PanelKind::Reference));
            // A conflict keeps the prompt open, so it can be corrected in place.
            app.handle_reference_key(KeyCode::Char('r'), false, true)
                .unwrap();
            app.set_prompt.as_mut().unwrap().1.offer("b");
            app.handle_set_prompt_key(KeyCode::Enter, false, false);
            assert!(app.set_prompt.as_ref().unwrap().1.error.is_some());
            app.set_prompt.as_mut().unwrap().1.offer("vox1");
            app.handle_set_prompt_key(KeyCode::Enter, false, false);
            assert!(app.set_prompt.is_none());
            assert_eq!(app.focus, Focus::Panel(PanelKind::Reference));
            assert_eq!(std::fs::read(takes.join("vox1.wav")).unwrap(), b"first");
            assert!(!takes.join("a.wav").exists());
            let panel = app.reference_panel.as_ref().unwrap();
            assert_eq!(panel.sound_selected, selected);
            assert!(panel.expanded.is_some());
            assert_eq!(panel.selected_bank().unwrap().0, "takes");
            assert!(app.status.contains("takes:0 (vox1)"));
            app.settle_sample_sources();
            assert_eq!(
                library.file_location("takes", Some(0)),
                library.file_location("vox1", None)
            );
            assert!(
                app.prefs.sample_renames.is_empty(),
                "file rename must not alias the bank"
            );
        }

        #[test]
        fn prompts_restore_the_initiating_panel_with_the_set_browser_open() {
            use rustel_runtime::samples::{SoundCategory, SoundEntry, SoundOrigin};
            let directory = tempfile::tempdir().unwrap();
            let mut app = app_over(directory.path());
            app.open_set_panel(false);
            app.reference_panel = Some(ReferencePanel::samples(
                &app.reference,
                vec![SoundEntry {
                    name: "takes".into(),
                    variants: 2,
                    variant_names: Vec::new(),
                    origin: SoundOrigin::Global,
                    category: SoundCategory::Drums,
                    location: None,
                    import: None,
                }],
            ));
            let panel = app.reference_panel.as_mut().unwrap();
            for (section, _) in panel.visible_categories() {
                panel.open_categories.insert(section);
            }
            let selected = panel
                .sound_rows()
                .iter()
                .position(|row| matches!(row, SoundRow::Bank(_)))
                .unwrap();
            panel.sound_selected = selected;
            for key in [KeyCode::Esc, KeyCode::Enter] {
                app.focus_panel(PanelKind::Reference);
                app.handle_reference_key(KeyCode::Char('r'), false, true)
                    .unwrap();
                assert!(app.set_prompt.is_some());
                app.handle_set_prompt_key(key, false, false);
                assert!(app.set_prompt.is_none());
                assert_eq!(app.focus, Focus::Panel(PanelKind::Reference));
                assert_eq!(
                    app.reference_panel.as_ref().unwrap().sound_selected,
                    selected
                );
            }
            for origin in [
                Focus::Editor,
                Focus::Panel(PanelKind::Reference),
                Focus::Panel(PanelKind::Set),
            ] {
                app.focus = origin;
                app.open_set_prompt(SetPrompt::OpenSet);
                app.handle_set_prompt_key(KeyCode::Esc, false, false);
                assert_eq!(app.focus, origin);
            }
            let mut sheet = SettingsSheet::default();
            sheet.show_page(crate::settings::SettingsPage::Sources);
            app.settings_sheet = Some(sheet);
            app.focus_panel(PanelKind::Settings);
            app.open_set_prompt(SetPrompt::PickBankToRename);
            app.renaming_bank = Some("takes".into());
            app.open_set_prompt(SetPrompt::RenameBank);
            app.handle_set_prompt_key(KeyCode::Esc, false, false);
            assert_eq!(app.focus, Focus::Panel(PanelKind::Settings));
            assert!(app.set_panel.is_some());
        }

        #[test]
        fn set_pages_use_the_visible_list_in_docked_and_sheet_layouts() {
            let directory = tempfile::tempdir().unwrap();
            let mut app = app_over(directory.path());
            app.open_set_panel(true);
            app.frame = Rect::new(0, 0, 110, 45);
            let source = app.editor().source().to_owned();
            for sidebar in [Rect::new(0, 2, 25, 31), Rect::default()] {
                app.regions.sidebar = sidebar;
                let panel = app.set_panel.as_mut().unwrap();
                panel.lines = (0..80).map(SetLine::File).collect();
                panel.selected = 0;
                let rows = panel.list_area(sidebar, app.frame).unwrap().height as usize;
                assert!(app.handle_set_key(KeyCode::PageDown, false, false));
                assert_eq!(app.set_panel.as_ref().unwrap().selected, rows);
                assert!(app.handle_set_key(KeyCode::PageUp, false, false));
                assert_eq!(app.set_panel.as_ref().unwrap().selected, 0);
                app.handle_set_key(KeyCode::PageUp, false, false);
                assert_eq!(app.set_panel.as_ref().unwrap().selected, 0);
                app.set_panel.as_mut().unwrap().selected = 79;
                app.handle_set_key(KeyCode::PageDown, false, false);
                assert_eq!(app.set_panel.as_ref().unwrap().selected, 79);
                assert_eq!(app.editor().source(), source);
                assert_eq!(app.focus, Focus::Panel(PanelKind::Set));
            }
        }

        #[test]
        fn device_page_keys_keep_the_selection_visible_without_choosing_a_device() {
            use crate::devices::{DeviceEntry, DeviceInventory};

            let directory = tempfile::tempdir().unwrap();
            let mut app = app_over(directory.path());
            app.frame = Rect::new(0, 0, 110, 45);
            app.devices.install_for_tests(DeviceInventory {
                audio_outputs: (0..35)
                    .map(|index| DeviceEntry {
                        name: format!("output {index}"),
                        id: index.to_string(),
                        detail: String::new(),
                        is_default: false,
                    })
                    .collect(),
                ..DeviceInventory::default()
            });
            app.panel = Some(DevicePanel::default());
            app.focus = Focus::Panel(PanelKind::Devices);
            let status = app.status.clone();
            let source = app.editor().source().to_owned();
            for (key, selected) in [
                (KeyCode::PageDown, 12),
                (KeyCode::PageDown, 24),
                (KeyCode::PageDown, 34),
                (KeyCode::PageDown, 34),
                (KeyCode::PageUp, 22),
                (KeyCode::PageUp, 10),
                (KeyCode::PageUp, 0),
                (KeyCode::PageUp, 0),
            ] {
                assert!(app.handle_panel_key(key, false).unwrap());
                let panel = app.panel.unwrap();
                let geometry = DevicePanelView::geometry(app.frame, panel, 35).unwrap();
                assert_eq!(panel.selected, selected);
                assert!(selected >= geometry.first);
                assert!(selected < geometry.first + usize::from(geometry.list.height));
                assert_eq!(app.focus, Focus::Panel(PanelKind::Devices));
                assert_eq!(app.status, status);
                assert_eq!(app.editor().source(), source);
            }
        }

        #[test]
        fn session_fold_arrow_keys_expand_and_collapse_without_toggling_back() {
            let directory = tempfile::tempdir().unwrap();
            let mut app = app_over(directory.path());
            let sessions = app.sessions_directory();
            std::fs::create_dir_all(&sessions).unwrap();
            write_tape(&sessions, "2026-09-20T12-00-00", &[(0.0, "s('sine')")]);
            app.open_set_panel(true);
            let panel = app.set_panel.as_mut().unwrap();
            let fold = panel
                .lines
                .iter()
                .position(|row| *row == SetLine::Sessions)
                .unwrap();
            panel.selected = fold;
            for _ in 0..2 {
                app.handle_terminal_event(Event::Key(KeyEvent::new(
                    KeyCode::Right,
                    KeyModifiers::NONE,
                )))
                .unwrap();
                assert!(app.set_panel.as_ref().unwrap().sessions_open);
                assert_eq!(app.set_panel.as_ref().unwrap().selected, fold);
            }
            app.handle_terminal_event(Event::Key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE)))
                .unwrap();
            assert!(matches!(
                app.set_panel.as_ref().unwrap().selected_line(),
                Some(SetLine::Tape(_))
            ));
            for _ in 0..2 {
                app.handle_terminal_event(Event::Key(KeyEvent::new(
                    KeyCode::Left,
                    KeyModifiers::NONE,
                )))
                .unwrap();
                assert!(!app.set_panel.as_ref().unwrap().sessions_open);
                assert_eq!(app.set_panel.as_ref().unwrap().selected, fold);
            }
            app.handle_terminal_event(Event::Key(KeyEvent::new(
                KeyCode::Enter,
                KeyModifiers::NONE,
            )))
            .unwrap();
            assert!(
                app.set_panel.as_ref().unwrap().sessions_open,
                "Enter remains supported"
            );
        }

        #[test]
        fn every_set_selection_has_a_reveal_target() {
            let directory = tempfile::tempdir().unwrap();
            let mut app = app_over(directory.path());
            app.open_set_panel(false);
            let score = app.scenes.current().path.clone();
            assert_eq!(app.set_reveal_target(), RevealTarget::File(score));
            let panel = app.set_panel.as_mut().unwrap();
            panel.selected = panel
                .lines
                .iter()
                .position(|line| *line == SetLine::Sessions)
                .unwrap();
            assert_eq!(
                app.set_reveal_target(),
                RevealTarget::Folder(directory.path().to_path_buf())
            );
            let sessions = app.sessions_directory();
            std::fs::create_dir_all(&sessions).unwrap();
            assert_eq!(
                app.set_reveal_target(),
                RevealTarget::Folder(sessions.clone())
            );
            let tape = write_tape(&sessions, "2026-09-24T12-00-00", &[(0.0, "s(\"bd\")")]);
            app.refresh_set_panel(Some(tape.clone()));
            assert_eq!(app.set_reveal_target(), RevealTarget::File(tape));
        }

        #[test]
        fn a_set_hidden_for_reference_does_not_capture_input() {
            let directory = tempfile::tempdir().unwrap();
            let mut app = app_over(directory.path());
            app.open_set_panel(true);
            app.toggle_reference_browse();
            app.focus_panel(PanelKind::Set);
            let mut terminal =
                ratatui::Terminal::new(ratatui::backend::TestBackend::new(100, 40)).unwrap();
            terminal
                .draw(|frame| app.paint_into(frame, Instant::now()).unwrap())
                .unwrap();
            assert!(app.regions.sidebar_hidden);
            assert_eq!(app.focus, Focus::Editor);
            for (x, y) in [(50, 10), (90, 20)] {
                assert!(!app.click_set_panel(x, y));
                assert!(!app.scroll_set_panel(x, y, 1.0));
            }
            app.rotate_panel_focus();
            assert_eq!(app.focus, Focus::Panel(PanelKind::Reference));
            app.rotate_panel_focus();
            assert_eq!(app.focus, Focus::Editor);
            assert!(app.set_panel.is_some(), "the requested panel stays open");
        }
    }
}
