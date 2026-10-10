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
                // A score already on the strip is chosen the way its tab
                // is clicked - the focused pane shows it - rather than
                // opened again, which would move the set's current scene
                // under the panes without moving them.
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
        if self.set_panel.is_none() {
            self.open_set_panel(true);
            self.prefs.set_panel = Some(true);
            self.save_prefs_soon();
        } else {
            self.focus_panel(PanelKind::Set);
        }
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
                picker.cancel_click();
                if !picker.typing {
                    picker.start_typing();
                }
                picker.caret_to(at, shift);
                self.focus_panel(PanelKind::Set);
                self.pointer = Some(Pointer::PromptField);
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
    /// folder, with a starter scene, taken up at once. Nothing is asked.
    pub(super) fn new_set(&mut self) {
        // Checked first, so a refused switch does not leave a new folder.
        if self.keeps_set_for_unsaved_score() {
            return;
        }
        let root = self.sets_root();
        let starter = self.starter_score();
        match SceneSet::create_in(&root, &today(), starter) {
            Ok(set) => self.switch_set(set, "new set"),
            Err(error) => self.set_error(
                ErrorOwner::Interface,
                format!("cannot make a set in {}: {error}", root.display()),
            ),
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
        if self.set_prompt.is_none() {
            let settings_prompt = matches!(
                prompt,
                SetPrompt::SetsFolder
                    | SetPrompt::RecordingsFolder
                    | SetPrompt::AddSampleSource
                    | SetPrompt::EditSampleSource(_)
            );
            #[cfg(feature = "vst")]
            let settings_prompt = settings_prompt || prompt == SetPrompt::AddVstFolder;
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
                | SetPrompt::AddSampleSource
                | SetPrompt::EditSampleSource(_)
                | SetPrompt::PickBankToRename
                | SetPrompt::RenameBank
                | SetPrompt::RenameSample
        );
        #[cfg(feature = "vst")]
        let over_settings = over_settings || prompt == SetPrompt::AddVstFolder;
        if over_settings && self.settings_sheet.is_some() {
            self.dismiss_dialogs_except(&[PanelKind::Set, PanelKind::Settings]);
        } else {
            self.dismiss_dialogs(Some(PanelKind::Set));
        }
        let root = self.sets_root();
        let picker = match prompt {
            SetPrompt::OpenSet => FilePicker::new(
                "open a set",
                "opens it",
                set_candidates(&root, self.scenes.directory()),
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
        };
        self.set_prompt = Some((prompt, picker));
        self.focus_panel(PanelKind::Set);
        self.status = match prompt {
            SetPrompt::OpenSet => {
                "open a set - Enter opens the chosen set · Tab browses · type a path · Esc back"
                    .into()
            }
            SetPrompt::OpenRecent => {
                "open a recent set - Enter opens the chosen one · Esc back".into()
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
                    SetPrompt::AddSampleSource | SetPrompt::EditSampleSource(_) => {
                        "import cancelled"
                    }
                    _ => "set prompt closed",
                }
                .into();
            }
            PickerKey::Chose(path) => match prompt {
                SetPrompt::OpenSet | SetPrompt::OpenRecent => self.open_set_at(&path),
                SetPrompt::RenameSet => {
                    let name = path.to_string_lossy().trim().to_owned();
                    self.rename_set(&name);
                }
                SetPrompt::SetsFolder => self.choose_sets_folder(&path),
                SetPrompt::RecordingsFolder => self.choose_recordings_folder(&path),
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
            },
        }
        self.dirty_frame = true;
        true
    }

    /// Open another set: a folder, or a score and its folder.
    fn open_set_at(&mut self, path: &std::path::Path) {
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

    /// Write the scores of the open set. True when one could not be written:
    /// its text is only in its tab, so the set must stay open. The status
    /// names the score.
    fn keeps_set_for_unsaved_score(&mut self) -> bool {
        let Some(name) = self.score_not_on_disk() else {
            return false;
        };
        self.status = format!("this set stays open - {name:?} could not be saved");
        self.dirty_frame = true;
        true
    }

    /// Put the current set down whole - every dirty score written, its
    /// set file current - and take up `set` in its place. What is sounding
    /// keeps sounding: the engine has the last score it was given.
    pub(super) fn switch_set(&mut self, mut set: SceneSet, verb: &str) {
        // A limiter mode still settling was chosen on this set, so it lands
        // here before the set is put down rather than on the one after.
        self.settle_limiter_mode();
        // Whole, and not only its scores: a prebake tab still dirty here is
        // dropped by the replacement below without ever being written, and
        // the global prebake it edits belongs to no set at all.
        //
        // The old set is put down whole before the new one takes over:
        // wait out the save worker, so a set reopened moments later reads
        // what was on screen when it was left rather than whatever the
        // worker has not written yet.
        //
        // A score that could not be written keeps this set open.
        if self.keeps_set_for_unsaved_score() {
            return;
        }
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

/// Which of the File menu's prompts is up: a set to open, a recent one,
/// a new name for this set, or the folder new sets are made in.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum SetPrompt {
    OpenSet,
    OpenRecent,
    RenameSet,
    SetsFolder,
    RecordingsFolder,
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
}
