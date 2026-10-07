//! The theme sheets. This covers the theme picker (Ctrl+T), where browsing
//! with the keyboard applies a theme after a short settle delay, and the theme
//! editor with its code tab, live draft and save sheet. It also holds what a
//! click or the wheel does on either sheet, and what keeping a theme does:
//! adopting the theme's suggested opacities, remembering the choice in prefs,
//! and the camera-theme status note.

use super::*;

/// How long keyboard browsing waits before the selected theme is applied.
/// A held arrow would otherwise compile a Hydra sketch per repeat; a click
/// still applies at once.
pub(super) const THEME_APPLY_SETTLE: Duration = Duration::from_millis(200);

impl App {
    /// The theme editor's sheet steps aside - still open, drawn again
    /// later - while any other surface is up, so the snippet shelf or the
    /// settings can actually be seen and used over it.
    pub(super) fn theme_editor_sheet_visible(&self) -> bool {
        self.theme_editor.is_some()
            && self.reference_panel.is_none()
            && self.settings_sheet.is_none()
            && self.log_panel.is_none()
            && self.export_sheet.is_none()
            && self.theme_picker.is_none()
            && self.panel.is_none()
    }

    /// The theme editor's sheet, over the stage pictures and under the
    /// menu's dropdown, drawing the editor lent to the frame.
    pub(super) fn paint_theme_editor_sheet(
        &self,
        frame: &mut ratatui::Frame<'_>,
        theme_editor_visible: bool,
        theme_editor: &mut Option<super::super::theme_editor::ThemeEditor>,
        theme_editor_focused: bool,
    ) {
        // The sheet steps aside while another surface is up - the
        // snippet shelf, the settings - and comes back when it closes;
        // the draft stays applied either way.
        if theme_editor_visible && let Some(editor) = theme_editor.as_mut() {
            super::super::theme_editor::ThemeEditorView {
                editor,
                theme: &self.theme,
                keybinds: &self.keybinds,
                focused: theme_editor_focused,
            }
            .render(frame.area(), frame.buffer_mut());
        }
    }

    /// Would this key edit the score if it fell through? Under the
    /// editor's sheet the score is invisible, so such a key is swallowed
    /// rather than let it edit a document nobody can see.
    fn key_would_edit_hidden_score(
        &self,
        code: KeyCode,
        primary: bool,
        shift: bool,
        alt: bool,
    ) -> bool {
        let mut modifiers = KeyModifiers::NONE;
        if primary {
            modifiers |= KeyModifiers::CONTROL;
        }
        if shift {
            modifiers |= KeyModifiers::SHIFT;
        }
        if alt {
            modifiers |= KeyModifiers::ALT;
        }
        let event = crossterm::event::KeyEvent::new(code, modifiers);
        key_to_command(event, self.capabilities).is_some_and(|command| edits_document(&command))
    }

    /// ⌘T / Ctrl+T: the theme picker, or close it keeping what is shown.
    pub(super) fn toggle_theme_picker(&mut self) {
        if self.theme_picker.is_some() && self.focus != Focus::Panel(PanelKind::Theme) {
            self.focus_panel(PanelKind::Theme);
            return;
        }
        if self.theme_picker.take().is_some() {
            self.keep_theme();
            self.settle_focus();
            return;
        }
        self.dismiss_dialogs(Some(PanelKind::Theme));
        self.theme_picker = Some(ThemePicker::open(&self.theme));
        self.focus_panel(PanelKind::Theme);
        self.status = "theme - type to search · ←/→/↑/↓ preview · Enter keep · Esc back".into();
        self.dirty_frame = true;
    }

    /// Ctrl+T: the theme picker, shown or hidden - refused out loud while
    /// the theme editor is open over it.
    pub(super) fn theme_picker_chord(&mut self) {
        // With the theme editor open the picker would come up
        // under its sheet, invisible but holding the keyboard -
        // and its n/e would silently replace the draft.
        if self.theme_editor.is_some() {
            self.status = "the theme editor is open - s saves, Esc goes back to the themes".into();
            self.dirty_frame = true;
            return;
        }
        self.toggle_theme_picker();
    }

    /// Remember the row the arrows landed on; apply it once they stop.
    pub(super) fn schedule_theme_preview(&mut self) {
        self.theme_apply_due = Some(Instant::now() + THEME_APPLY_SETTLE);
        if let Some(name) = self
            .theme_picker
            .as_ref()
            .and_then(|picker| picker.selected_name())
        {
            self.status = format!("theme - {name}");
        }
        self.dirty_frame = true;
    }

    /// Show the theme under the picker's cursor, now.
    ///
    /// The sketch goes with the palette: browsing is debounced, so this is
    /// the theme landed on rather than every one passed over, and one apply
    /// is one compile. Nothing is deferred behind a timer of its own.
    pub(super) fn preview_theme(&mut self) {
        let Some(name) = self
            .theme_picker
            .as_ref()
            .and_then(|picker| picker.selected_name())
            .map(str::to_owned)
        else {
            return;
        };
        // The randomize row is a roll, not a file: the picker's seed decides
        // which one, so ← and → can change it without changing the row.
        let rolled = (name == super::super::theme::RANDOMIZE_THEME).then(|| {
            let seed = self
                .theme_picker
                .as_ref()
                .map_or_else(super::super::theme::randomize_seed, |picker| {
                    picker.randomize_seed
                });
            (super::super::theme::randomize_theme(seed), seed)
        });
        self.theme_apply_due = None;
        if let Some((theme, seed)) = rolled {
            self.theme = theme;
            self.adopt_theme_opacity();
            #[cfg(feature = "hydra")]
            self.sync_theme_sketch();
            self.status = format!("theme - chance #{seed:08x} · ←/→ rolls · e edits and saves");
            self.invalidate_maps();
            self.dirty_frame = true;
            return;
        }
        match Theme::resolve(Some(&name)) {
            Ok(theme) => {
                self.theme = theme;
                // Walking the list used to apply every theme the moment the
                // cursor touched it. A held arrow does that thirty times a
                // second; the caller now waits (`schedule_theme_preview`)
                // and a click comes straight here, so this is the last
                // theme, not every one on the way.
                self.adopt_theme_opacity();
                #[cfg(feature = "hydra")]
                self.sync_theme_sketch();
                self.status = format!("theme - {name}");
            }
            Err(error) => self.status = format!("theme {name}: {error}"),
        }
        self.invalidate_maps();
        self.dirty_frame = true;
    }

    /// Open the theme editor on a starting point, replacing the picker.
    pub(super) fn open_theme_editor(&mut self, base: &Theme, name: String, from_built_in: bool) {
        // The editor's Esc lands on the theme that is applied right now.
        // Browsing the picker keeps what it lands on rather than previewing
        // it, so the theme on screen when the editor opens is what the reader
        // had - there is no earlier state to go back to.
        if self.theme_apply_due.take().is_some() {
            self.preview_theme();
        }
        self.theme_picker = None;
        let restore = self.theme.clone();
        self.theme_editor = Some(super::super::theme_editor::ThemeEditor::open(
            base,
            &restore,
            name,
            from_built_in,
        ));
        // The draft applies from the first moment: the studio is the
        // preview - its ui opacity included, with the way back remembered.
        self.opacity_before_theme_editor = Some(self.ui_settings.interface_opacity);
        self.theme = base.clone();
        if let Some(opacity) = base.ui_opacity {
            self.ui_settings.interface_opacity = opacity.min(100);
        }
        #[cfg(feature = "hydra")]
        self.sync_theme_sketch();
        self.focus_panel(PanelKind::ThemeEditor);
        self.invalidate_maps();
        self.status =
            "theme editor - everything applies live; s saves, Esc goes back to the themes".into();
        self.dirty_frame = true;
    }

    /// The editor closes onto the theme list, the way it was opened. With
    /// `keep` the studio goes on wearing the draft a save just kept; without
    /// it the theme from before the editor comes back.
    pub(super) fn close_theme_editor(
        &mut self,
        editor: super::super::theme_editor::ThemeEditor,
        keep: bool,
    ) {
        if keep {
            self.opacity_before_theme_editor = None;
        } else {
            self.theme = *editor.original;
            self.restore_pre_editor_opacity();
        }
        #[cfg(feature = "hydra")]
        self.sync_theme_sketch();
        self.theme_picker = Some(ThemePicker::open(&self.theme));
        self.focus_panel(PanelKind::Theme);
        self.invalidate_maps();
        self.dirty_frame = true;
    }

    /// What stands between the reader and a camera theme's picture, for the
    /// picker's footer and the status line: the setting, the camera opening,
    /// or nothing any more.
    #[cfg(feature = "hydra")]
    pub(super) fn theme_camera_note(
        &self,
        status: Option<&rustel_runtime::hydra::HydraWebcamStatus>,
    ) -> Option<super::super::theme::CameraNote> {
        use super::super::theme::CameraNote;
        use rustel_runtime::hydra::HydraWebcamState;
        if !self.theme.camera_enabled() {
            return None;
        }
        if !self.ui_settings.hydra_webcam {
            return Some(CameraNote::Off);
        }
        Some(match status.map(|status| status.state) {
            Some(HydraWebcamState::Error) => CameraNote::Failed,
            Some(HydraWebcamState::Blocked) => CameraNote::Off,
            _ if self.hydra_theme_last.is_some() => CameraNote::Live,
            _ if self.theme_visual.camera_frame_live(Instant::now()) => CameraNote::Live,
            _ => CameraNote::Starting,
        })
    }

    /// The edited draft becomes the studio, live - ui opacity included,
    /// falling back to what the user had when the draft does not say.
    pub(super) fn apply_theme_draft(&mut self, draft: &Theme) {
        self.theme = draft.clone();
        // Editing one of the draft's opacity rows shows on the studio at once
        // - the point of editing it is to look at it. The draft is not the
        // theme on disk until it is saved, so this is a preview; closing
        // without keeping puts the reader's own back.
        let suggested = draft.opacities();
        self.ui_settings.backdrop_opacity = suggested.backdrop;
        self.ui_settings.interface_opacity = suggested.interface;
        self.ui_settings.editor_opacity = suggested.editor;
        self.apply_ui_settings();
        #[cfg(feature = "hydra")]
        self.sync_theme_sketch();
        self.invalidate_maps();
        self.dirty_frame = true;
    }

    /// Closing the editor without keeping: the pre-open opacity comes back.
    fn restore_pre_editor_opacity(&mut self) {
        if let Some(opacity) = self.opacity_before_theme_editor.take() {
            self.ui_settings.interface_opacity = opacity;
        }
    }

    /// The code tab parses a beat after typing stops, and applies when it
    /// parses. Called every loop pass.
    pub(super) fn pump_theme_editor(&mut self) {
        let due = self
            .theme_editor
            .as_ref()
            .and_then(|editor| editor.code_dirty_at)
            .is_some_and(|at| at.elapsed() >= super::super::theme_editor::CODE_DEBOUNCE);
        if !due {
            return;
        }
        let draft = self
            .theme_editor
            .as_mut()
            .and_then(|editor| editor.parse_code().then(|| editor.draft.clone()));
        if let Some(draft) = draft {
            self.apply_theme_draft(&draft);
        }
        self.dirty_frame = true;
    }

    /// Returns true when the editor consumed the key.
    pub(super) fn handle_theme_editor_key(
        &mut self,
        code: KeyCode,
        primary: bool,
        shift: bool,
        alt: bool,
    ) -> bool {
        use super::super::theme_editor::EditorTab;
        // Taken out while the key runs, so the editor and the app never
        // fight over the borrow; every path either puts it back or closes.
        let Some(mut editor) = self.theme_editor.take() else {
            return false;
        };
        // The save sheet is modal within the sheet: name, button, go.
        if editor.saving.is_some() {
            match code {
                KeyCode::Esc => {
                    if editor.saving.as_ref().is_some_and(|sheet| sheet.closing) {
                        // The second Esc: leave, and let the changes go.
                        self.close_theme_editor(editor, false);
                        self.status = "theme changes discarded".into();
                        return true;
                    }
                    editor.saving = None;
                }
                KeyCode::Tab => {
                    if let Some(saving) = editor.saving.as_mut() {
                        saving.keep = !saving.keep;
                    }
                }
                KeyCode::Left => {
                    if let Some(saving) = editor.saving.as_mut() {
                        saving.keep = false;
                    }
                }
                KeyCode::Right => {
                    if let Some(saving) = editor.saving.as_mut() {
                        saving.keep = true;
                    }
                }
                KeyCode::Enter => {
                    return self.try_save_theme(editor);
                }
                KeyCode::Backspace => {
                    if let Some(saving) = editor.saving.as_mut() {
                        saving.name.pop();
                        saving.note = None;
                    }
                }
                KeyCode::Char(character) if !primary && !alt => {
                    if let Some(saving) = editor.saving.as_mut() {
                        saving.name.push(character);
                        saving.note = None;
                    }
                }
                _ => {}
            }
            self.theme_editor = Some(editor);
            self.dirty_frame = true;
            return true;
        }
        // The colour picker paints live; Esc puts the old colour back.
        if editor.picker.is_some() {
            let mut apply = false;
            match code {
                KeyCode::Esc => {
                    editor.unpaint();
                    editor.picker = None;
                    apply = true;
                }
                KeyCode::Enter => {
                    apply = editor.paint();
                    editor.picker = None;
                }
                KeyCode::Up | KeyCode::Down | KeyCode::Left | KeyCode::Right => {
                    if let Some(picker) = editor.picker.as_mut() {
                        let (dx, dy) = match code {
                            KeyCode::Left => (-1, 0),
                            KeyCode::Right => (1, 0),
                            KeyCode::Up => (0, -1),
                            _ => (0, 1),
                        };
                        picker.step(dx, dy);
                    }
                    apply = editor.paint();
                }
                KeyCode::Backspace => {
                    if let Some(picker) = editor.picker.as_mut() {
                        picker.hex.pop();
                    }
                    apply = editor.paint();
                }
                KeyCode::Char(character)
                    if !primary && (character.is_ascii_hexdigit() || character == '#') =>
                {
                    if let Some(picker) = editor.picker.as_mut() {
                        if !picker.hex.starts_with('#') && character != '#' && picker.hex.len() >= 6
                        {
                            picker.hex.clear();
                        }
                        picker.hex.push(character);
                    }
                    apply = editor.paint();
                }
                _ => {}
            }
            if apply {
                editor.rebuild_code();
                let draft = editor.draft.clone();
                self.apply_theme_draft(&draft);
            }
            self.theme_editor = Some(editor);
            self.dirty_frame = true;
            return true;
        }
        // Ctrl+C copies the whole theme as its file, ready to share - on
        // the code tab only while nothing is selected, so a real selection
        // still copies itself.
        if primary
            && matches!(code, KeyCode::Char('c' | 'C'))
            && (editor.tab == EditorTab::Form || editor.code.primary_selection().is_empty())
        {
            let json = editor.draft.to_json();
            match self.clipboard.set_text(json) {
                Ok(()) => self.toast("theme copied - paste it anywhere"),
                Err(error) => self.status = format!("cannot copy: {error}"),
            }
            self.theme_editor = Some(editor);
            self.dirty_frame = true;
            return true;
        }
        // Tab flips between the tabs, both ways. In this editor Tab means
        // the tabs, and code indents with spaces.
        if code == KeyCode::Tab && editor.tab == EditorTab::Code {
            if editor.parse_code() {
                let draft = editor.draft.clone();
                self.apply_theme_draft(&draft);
            }
            editor.tab = EditorTab::Form;
            self.theme_editor = Some(editor);
            self.dirty_frame = true;
            return true;
        }
        if code == KeyCode::Tab && editor.tab == EditorTab::Form {
            if editor.entry.is_some() {
                match editor.commit_entry() {
                    Ok(changed) => {
                        if changed {
                            editor.rebuild_code();
                            let draft = editor.draft.clone();
                            self.apply_theme_draft(&draft);
                        }
                    }
                    Err(error) => {
                        self.status = error;
                        self.theme_editor = Some(editor);
                        self.dirty_frame = true;
                        return true;
                    }
                }
            }
            editor.tab = EditorTab::Code;
            self.theme_editor = Some(editor);
            self.dirty_frame = true;
            return true;
        }
        // Esc is a ladder: entry off, code back to the form, form closes
        // and puts the theme back.
        if code == KeyCode::Esc && editor.entry.is_none() {
            // Esc goes back the way it came in: to the list, on either tab.
            // Unsaved work asks first - the sheet's Enter saves, its Esc
            // lets the changes go.
            if editor.is_modified() {
                let mut sheet = self.seed_save_sheet(&editor);
                sheet.closing = true;
                sheet.note = Some("unsaved changes - Enter saves, Esc discards".into());
                editor.saving = Some(sheet);
                self.theme_editor = Some(editor);
                self.dirty_frame = true;
                return true;
            }
            self.close_theme_editor(editor, false);
            self.status = "theme - type to search · ←/→/↑/↓ preview · Enter keep · Esc back".into();
            return true;
        }
        let consumed = match editor.tab {
            EditorTab::Form => {
                if editor.entry.is_some() {
                    match code {
                        KeyCode::Esc => {
                            editor.entry = None;
                            true
                        }
                        KeyCode::Enter => {
                            match editor.commit_entry() {
                                Ok(changed) => {
                                    if changed {
                                        editor.rebuild_code();
                                        let draft = editor.draft.clone();
                                        self.apply_theme_draft(&draft);
                                    }
                                }
                                Err(error) => self.status = error,
                            }
                            true
                        }
                        KeyCode::Backspace => {
                            if let Some(entry) = editor.entry.as_mut() {
                                entry.pop();
                            }
                            true
                        }
                        KeyCode::Char(character) if !primary && !alt => {
                            if let Some(entry) = editor.entry.as_mut() {
                                entry.push(character);
                            }
                            true
                        }
                        // An entry box holds the keyboard for its own kind
                        // of key, and swallows anything that would edit the
                        // score no one can see under the sheet.
                        KeyCode::Delete
                        | KeyCode::Left
                        | KeyCode::Right
                        | KeyCode::Up
                        | KeyCode::Down
                        | KeyCode::Home
                        | KeyCode::End => true,
                        _ => self.key_would_edit_hidden_score(code, primary, shift, alt),
                    }
                } else {
                    match code {
                        KeyCode::Up => {
                            editor.selected = editor.selected.saturating_sub(1);
                            true
                        }
                        KeyCode::Down => {
                            editor.selected = (editor.selected + 1).min(editor.rows() - 1);
                            true
                        }
                        KeyCode::PageUp | KeyCode::PageDown => {
                            let page =
                                super::super::theme_editor::ThemeEditorView::geometry(self.frame)
                                    .map_or(1, |area| {
                                        usize::from(area.height.saturating_sub(4)).max(1)
                                    });
                            editor.selected = page_selection(
                                editor.selected,
                                editor.rows(),
                                page,
                                code == KeyCode::PageDown,
                            );
                            true
                        }
                        KeyCode::Home => {
                            editor.selected = 0;
                            true
                        }
                        KeyCode::End => {
                            editor.selected = editor.rows().saturating_sub(1);
                            true
                        }
                        KeyCode::Left | KeyCode::Right => {
                            if editor.step(code == KeyCode::Right) {
                                editor.rebuild_code();
                                let draft = editor.draft.clone();
                                self.apply_theme_draft(&draft);
                            }
                            true
                        }
                        KeyCode::Enter => {
                            if editor.selected == super::super::theme_editor::ROW_SKETCH {
                                // The sketch is code; Enter goes where code
                                // is edited, seeding a start when there is
                                // none so the key is never dead.
                                if editor.seed_sketch() {
                                    editor.rebuild_code();
                                    let draft = editor.draft.clone();
                                    self.apply_theme_draft(&draft);
                                }
                                editor.tab = EditorTab::Code;
                            } else if super::super::theme_editor::ThemeEditor::is_color_row(
                                editor.selected,
                            ) {
                                // A colour row opens the painter, not a
                                // hex prompt.
                                if let Some(field) =
                                    super::super::theme_editor::ThemeEditor::color_field_of(
                                        editor.selected,
                                    )
                                {
                                    let previous = editor.color_of_row(editor.selected);
                                    editor.picker =
                                        Some(super::super::theme_editor::ColorPicker::open(
                                            field, previous,
                                        ));
                                }
                            } else {
                                let (_, seed) = editor.entry_seed();
                                editor.entry = Some(seed);
                            }
                            true
                        }
                        KeyCode::Char('s' | 'S') if !primary && !alt => {
                            editor.saving = Some(self.seed_save_sheet(&editor));
                            true
                        }
                        // The sheet covers a lot of the score, so a key
                        // that would edit it is swallowed; the rest -
                        // chords, the transport's function keys - falls
                        // through.
                        _ => self.key_would_edit_hidden_score(code, primary, shift, alt),
                    }
                }
            }
            EditorTab::Code => {
                // A resize or first key can arrive before the sheet is drawn.
                if let Some(body) =
                    super::super::theme_editor::ThemeEditorView::code_area(self.frame)
                {
                    editor
                        .code
                        .set_view_size(usize::from(body.width), usize::from(body.height));
                }
                // The code tab is a real editor: the studio's own keymap,
                // through the same translation the score uses.
                let mut modifiers = KeyModifiers::NONE;
                if primary {
                    modifiers |= KeyModifiers::CONTROL;
                }
                if shift {
                    modifiers |= KeyModifiers::SHIFT;
                }
                if alt {
                    modifiers |= KeyModifiers::ALT;
                }
                let event = crossterm::event::KeyEvent::new(code, modifiers);
                match key_to_command(event, self.capabilities) {
                    Some(command) => {
                        let before = editor.code.revision();
                        let moment = self.moment();
                        let _ = editor.code.dispatch(command, moment, &mut *self.clipboard);
                        if editor.code.revision() != before {
                            editor.code_dirty_at = Some(Instant::now());
                            editor.code_error = None;
                        }
                        true
                    }
                    // Unmapped chords fall through - the snippet-shelf
                    // chord among them - but every mapped command was
                    // dispatched above, so nothing can edit the hidden
                    // score from here.
                    None => false,
                }
            }
        };
        self.theme_editor = Some(editor);
        if consumed {
            self.dirty_frame = true;
        }
        consumed
    }

    /// Route a mouse event into the theme editor's code tab, with the map
    /// rebuilt at the code area's own grid - drags extend the selection,
    /// the wheel scrolls, exactly as the score's editor behaves.
    pub(super) fn forward_mouse_to_theme_code(&mut self, mouse: MouseEvent) {
        let Some(body) = super::super::theme_editor::ThemeEditorView::code_area(self.frame) else {
            return;
        };
        let moment = self.moment();
        if let Some(editor) = self.theme_editor.as_mut() {
            let grid = super::super::editor::GridRect::new(body.x, body.y, body.width, body.height);
            editor
                .code
                .set_view_size(usize::from(body.width), usize::from(body.height));
            if let Ok(map) = editor.code.screen_map(grid) {
                let _ = editor.code.mouse_event(mouse, &map, moment);
            }
        }
        self.dirty_frame = true;
    }

    /// A press on the theme picker: a row chooses its theme and shows it at
    /// once, and anywhere else on the sheet is still the picker's. Returns
    /// true when the picker took the press.
    pub(super) fn click_theme_picker(&mut self, x: u16, y: u16) -> bool {
        if let Some(row) = self
            .theme_picker
            .as_ref()
            .and_then(|picker| picker.row_at(self.frame, x, y))
        {
            self.focus_panel(PanelKind::Theme);
            self.pointer = Some(Pointer::Panel);
            if let Some(picker) = self.theme_picker.as_mut() {
                picker.selected = row;
                picker.hold_scroll = true;
            }
            self.preview_theme();
            return true;
        }
        if let Some(picker) = self.theme_picker.as_ref()
            && picker
                .geometry(self.frame)
                .is_some_and(|(sheet, _)| within(sheet, x, y))
        {
            self.focus_panel(PanelKind::Theme);
            self.pointer = Some(Pointer::Panel);
            return true;
        }
        false
    }

    /// While the theme picker is up it has the keyboard, and the wheel is
    /// the arrows: it walks the list. Whether it took the wheel.
    pub(super) fn scroll_theme_picker(&mut self, direction: f32) -> bool {
        if self.theme_picker.is_some() {
            if let Some(picker) = self.theme_picker.as_mut() {
                picker.move_by(if direction > 0.0 { -1 } else { 1 });
            }
            self.preview_theme();
            self.dirty_frame = true;
            return true;
        }
        false
    }

    /// A press on the theme editor's sheet: a tab, a swatch while the
    /// painter is up, a form row, or a caret placement in the code. A press
    /// anywhere on the open sheet is a claim on it, clickable row or not:
    /// it must not focus the editor and teleport the caret through the
    /// panel. Returns true when the sheet took the press.
    pub(super) fn click_theme_editor(&mut self, mouse: MouseEvent, x: u16, y: u16) -> bool {
        if self.theme_editor_sheet_visible()
            && super::super::theme_editor::ThemeEditorView::geometry(self.frame)
                .is_some_and(|sheet| within(sheet, x, y))
        {
            self.focus_panel(PanelKind::ThemeEditor);
            self.pointer = Some(Pointer::Panel);
            if let Some(tab) = super::super::theme_editor::ThemeEditorView::tab_at(self.frame, x, y)
                && let Some(editor) = self.theme_editor.as_mut()
                && editor.saving.is_none()
                && editor.picker.is_none()
            {
                // Leaving the code tab by mouse settles it, the
                // way Esc-to-form does.
                if editor.tab == super::super::theme_editor::EditorTab::Code
                    && tab == super::super::theme_editor::EditorTab::Form
                    && editor.parse_code()
                {
                    let draft = editor.draft.clone();
                    self.apply_theme_draft(&draft);
                }
                if let Some(editor) = self.theme_editor.as_mut() {
                    editor.tab = tab;
                }
                self.dirty_frame = true;
                return true;
            }
            // With the painter up, a click on a swatch takes that
            // colour - live, like the arrows do.
            if self
                .theme_editor
                .as_ref()
                .is_some_and(|editor| editor.picker.is_some())
            {
                if let Some(sheet) =
                    super::super::theme_editor::ThemeEditorView::geometry(self.frame)
                    && let Some((column, row)) =
                        super::super::theme_editor::picker_cell_at(sheet, x, y)
                    && let Some(editor) = self.theme_editor.as_mut()
                {
                    if let Some(picker) = editor.picker.as_mut() {
                        picker.column = column;
                        picker.row = row;
                        picker.hex =
                            super::super::theme_editor::ColorPicker::hex_of(picker.current());
                    }
                    if editor.paint() {
                        editor.rebuild_code();
                        let draft = editor.draft.clone();
                        self.apply_theme_draft(&draft);
                    }
                }
                self.dirty_frame = true;
                return true;
            }
            let in_form = self.theme_editor.as_ref().is_some_and(|editor| {
                editor.tab == super::super::theme_editor::EditorTab::Form
                    && editor.saving.is_none()
                    && editor.picker.is_none()
            });
            if in_form
                && let Some(row) = self.theme_editor.as_ref().and_then(|editor| {
                    super::super::theme_editor::ThemeEditorView::form_row_at(
                        editor, self.frame, x, y,
                    )
                })
            {
                // The click chooses the row - and on a colour row it
                // opens the painter outright: the swatch is a button.
                if let Some(editor) = self.theme_editor.as_mut() {
                    editor.selected = row;
                    editor.entry = None;
                    if super::super::theme_editor::ThemeEditor::is_color_row(row)
                        && let Some(field) =
                            super::super::theme_editor::ThemeEditor::color_field_of(row)
                    {
                        let previous = editor.color_of_row(row);
                        editor.picker = Some(super::super::theme_editor::ColorPicker::open(
                            field, previous,
                        ));
                    }
                }
                self.dirty_frame = true;
                return true;
            }
            let in_code = self.theme_editor.as_ref().is_some_and(|editor| {
                editor.tab == super::super::theme_editor::EditorTab::Code
                    && editor.saving.is_none()
                    && editor.picker.is_none()
            });
            if in_code
                && let Some(body) =
                    super::super::theme_editor::ThemeEditorView::code_area(self.frame)
                && within(body, x, y)
            {
                // A click in the code is a caret placement - and
                // the press starts a drag, so a selection can be
                // pulled through the text like any editor.
                let moment = self.moment();
                if let Some(editor) = self.theme_editor.as_mut() {
                    let grid = super::super::editor::GridRect::new(
                        body.x,
                        body.y,
                        body.width,
                        body.height,
                    );
                    editor
                        .code
                        .set_view_size(usize::from(body.width), usize::from(body.height));
                    if let Ok(map) = editor.code.screen_map(grid) {
                        let _ = editor.code.mouse_event(mouse, &map, moment);
                    }
                }
                self.own_text_selection(TextSurface::ThemeCode);
                self.pointer = Some(Pointer::ThemeCode);
                self.dirty_frame = true;
                return true;
            }
            return true;
        }
        false
    }

    /// The wheel over the editor's sheet walks its form, or scrolls its
    /// code; it must never reach a live slider hidden underneath, which
    /// would edit the score and change the sound from a settings surface.
    /// Whether it took the wheel.
    pub(super) fn scroll_theme_editor(
        &mut self,
        mouse: MouseEvent,
        x: u16,
        y: u16,
        direction: f32,
    ) -> bool {
        if self.theme_editor_sheet_visible()
            && super::super::theme_editor::ThemeEditorView::geometry(self.frame)
                .is_some_and(|sheet| within(sheet, x, y))
        {
            let code_tab = self.theme_editor.as_ref().is_some_and(|editor| {
                editor.tab == super::super::theme_editor::EditorTab::Code
                    && editor.saving.is_none()
                    && editor.picker.is_none()
            });
            if code_tab {
                self.forward_mouse_to_theme_code(mouse);
                return true;
            }
            if let Some(editor) = self.theme_editor.as_mut()
                && editor.tab == super::super::theme_editor::EditorTab::Form
                && editor.entry.is_none()
            {
                let rows = editor.rows();
                editor.selected = if direction > 0.0 {
                    editor.selected.saturating_sub(1)
                } else {
                    (editor.selected + 1).min(rows - 1)
                };
                self.dirty_frame = true;
            }
            return true;
        }
        false
    }

    /// A fresh save sheet: the editor's own name when it has one, else a
    /// free name derived from the draft's.
    fn seed_save_sheet(
        &self,
        editor: &super::super::theme_editor::ThemeEditor,
    ) -> super::super::theme_editor::SaveSheet {
        let base = if editor.name.trim().is_empty() {
            let stem = editor.draft.name.trim();
            if stem.is_empty() { "my-theme" } else { stem }
        } else {
            editor.name.trim()
        };
        let name = super::super::theme_editor::unique_name(base, |candidate| {
            candidate != editor.opened_as
                && (Theme::built_in_names().any(|known| known == candidate)
                    || super::super::theme::user_theme_exists(candidate))
        });
        super::super::theme_editor::SaveSheet {
            name,
            keep: true,
            note: None,
            closing: false,
        }
    }

    /// Enter on the save sheet: write the file, or say exactly why not.
    /// Takes the editor and either puts it back (plain save, or a refusal)
    /// or closes it (save & keep).
    fn try_save_theme(&mut self, mut editor: super::super::theme_editor::ThemeEditor) -> bool {
        let Some(saving) = editor.saving.clone() else {
            self.theme_editor = Some(editor);
            return true;
        };
        let name = saving.name.trim().to_owned();
        if name.is_empty() {
            if let Some(sheet) = editor.saving.as_mut() {
                sheet.note = Some("a theme needs a name".into());
            }
            self.theme_editor = Some(editor);
            self.dirty_frame = true;
            return true;
        }
        // A name that collides with anything that is not THIS theme is a
        // conflict: the sheet says so and offers a free variation instead
        // of quietly writing over someone.
        let taken = name != editor.opened_as
            && (Theme::built_in_names().any(|known| known == name)
                || super::super::theme::user_theme_exists(&name));
        if taken {
            let free = super::super::theme_editor::unique_name(&name, |candidate| {
                candidate != editor.opened_as
                    && (Theme::built_in_names().any(|known| known == candidate)
                        || super::super::theme::user_theme_exists(candidate))
            });
            if let Some(sheet) = editor.saving.as_mut() {
                sheet.note = Some(format!("{name} is taken - {free}?"));
                sheet.name = free;
            }
            self.theme_editor = Some(editor);
            self.dirty_frame = true;
            return true;
        }
        // What is half-typed comes into the draft first: a save must never
        // quietly write an older draft than the one on screen.
        if let Err(error) = editor.settle() {
            if let Some(sheet) = editor.saving.as_mut() {
                sheet.note = Some(error);
            }
            self.theme_editor = Some(editor);
            self.dirty_frame = true;
            return true;
        }
        // The file's own name field is the file's name - a theme saved as
        // `dusk` must not claim to be `rustel-dark` inside.
        let mut draft = editor.draft.clone();
        draft.name = name.clone();
        match super::super::theme::save_named(&draft, &name) {
            Ok(path) => {
                if saving.keep {
                    self.theme = draft;
                    self.opacity_before_theme_editor = None;
                    if let Some(opacity) = self.theme.ui_opacity {
                        self.ui_settings.interface_opacity = opacity.min(100);
                    }
                    self.prefs.set_ui_settings(&self.ui_settings);
                    #[cfg(feature = "hydra")]
                    {
                        self.theme_camera_confirmed =
                            self.theme.camera_enabled().then(|| self.theme.name.clone());
                        self.sync_theme_sketch();
                    }
                    self.prefs.theme = Some(name);
                    self.save_prefs_soon();
                    self.flush_prefs();
                    self.status = format!(
                        "theme saved and kept - {}",
                        status_file_path(&path, self.ui_settings.show_full_paths)
                    );
                    self.close_theme_editor(editor, true);
                } else if saving.closing {
                    // Saved on the way out: the file is written and in the
                    // list, and the studio wears what it had before.
                    self.status = format!(
                        "theme saved - {}",
                        status_file_path(&path, self.ui_settings.show_full_paths)
                    );
                    self.close_theme_editor(editor, false);
                } else {
                    editor.draft = draft.clone();
                    editor.name = name.clone();
                    editor.opened_as = name;
                    editor.from_built_in = false;
                    editor.saving = None;
                    editor.rebuild_code();
                    editor.loaded = editor.draft.to_json();
                    self.apply_theme_draft(&draft);
                    self.status = format!(
                        "theme saved - {}",
                        status_file_path(&path, self.ui_settings.show_full_paths)
                    );
                    self.theme_editor = Some(editor);
                }
            }
            Err(error) => {
                if let Some(sheet) = editor.saving.as_mut() {
                    sheet.note = Some(error.to_string());
                }
                self.theme_editor = Some(editor);
            }
        }
        self.dirty_frame = true;
        true
    }

    /// Take the theme's suggested opacities, and forget that the reader ever
    /// chose otherwise.
    ///
    /// Every theme is tuned to be looked at through a particular amount of
    /// its own backdrop, so the three opacities travel with it. They are a
    /// suggestion: the moment the reader touches one of the settings rows it
    /// becomes theirs, outlives a restart, and no theme overwrites it again.
    /// Which of the two is in force is carried by whether the preference is
    /// present at all - see `StudioPrefs`.
    fn adopt_theme_opacity(&mut self) {
        let suggested = self.theme.opacities();
        self.ui_settings.backdrop_opacity = suggested.backdrop;
        self.ui_settings.interface_opacity = suggested.interface;
        self.ui_settings.editor_opacity = suggested.editor;
        self.prefs.follow_theme_opacity();
        self.apply_ui_settings();
    }

    /// Keep the theme on screen and remember it for next time.
    pub(super) fn keep_theme(&mut self) {
        if self.theme_apply_due.take().is_some() {
            self.preview_theme();
        }
        self.theme_picker = None;
        // The keys go back to the score: the picker is gone.
        self.settle_focus();
        self.adopt_theme_opacity();
        #[cfg(feature = "hydra")]
        {
            self.theme_camera_confirmed =
                self.theme.camera_enabled().then(|| self.theme.name.clone());
            // Closing IS settling: an apply the picker still owed has just
            // landed above, so the sketch it carries goes with it.
            self.sync_theme_sketch();
        }
        self.prefs.theme = Some(self.theme.name.clone());
        self.save_prefs_soon();
        self.status = match super::super::prefs::StudioPrefs::path() {
            Some(path) => format!(
                "theme {} - remembered in {}",
                self.theme.name,
                status_file_path(&path, self.ui_settings.show_full_paths)
            ),
            None => format!("theme {}", self.theme.name),
        };
        // A camera theme kept is a solid background until the camera opens,
        // or for good while the setting is off: say which, or the first
        // frame arrives as a surprise - or never.
        #[cfg(feature = "hydra")]
        if self.theme.camera_enabled() {
            self.status = if self.ui_settings.hydra_webcam {
                format!(
                    "theme {} - webcam starting; the picture lands in a moment",
                    self.theme.name
                )
            } else {
                format!(
                    "theme {} - uses your webcam: turn on Hydra webcam in Settings ({}) to see it",
                    self.theme.name,
                    self.keybinds.hint(BindAction::Settings)
                )
            };
        }
        self.dirty_frame = true;
    }

    /// Returns true when the picker consumed the key.
    pub(super) fn handle_theme_key(&mut self, code: KeyCode, primary: bool) -> bool {
        let Some(picker) = self.theme_picker.as_mut() else {
            return false;
        };
        // Any key that is not the second d calls a pending delete off.
        if !matches!(code, KeyCode::Char('d' | 'D')) {
            picker.deleting = None;
        }
        match code {
            // Moving the cursor does not preview a theme, it CHANGES it, so
            // there is nothing for Esc to put back: both doors out keep what
            // is on screen. A picker that reverts on Esc has to be driven with
            // one hand on Enter, and browsing themes is the one thing here
            // worth doing idly.
            KeyCode::Esc => self.keep_theme(),
            KeyCode::Enter if !primary => self.keep_theme(),
            // n shapes a new theme from the current one; e opens the
            // selected one in the editor. A built-in opens too - the editor
            // just demands a new name to save, because the compiled-in
            // themes are a fixed vocabulary.
            KeyCode::Char('n' | 'N') if primary => {
                let current = self.theme.clone();
                let from_built_in = Theme::built_in_names().any(|known| known == current.name);
                self.open_theme_editor(&current, String::new(), from_built_in);
                return true;
            }
            KeyCode::Char('e' | 'E') if primary => {
                let Some(name) = picker.selected_name().map(str::to_owned) else {
                    return true;
                };
                // Resolving the randomize row would roll a different theme, and
                // the one worth editing is the one on screen. It is already
                // in `self.theme`, put there by the preview.
                if name == super::super::theme::RANDOMIZE_THEME {
                    let rolled = self.theme.clone();
                    self.open_theme_editor(&rolled, String::new(), true);
                    return true;
                }
                match Theme::resolve(Some(&name)) {
                    Ok(mut theme) => {
                        // The ROW is the identity: a file is a user theme
                        // whatever its inner name field claims (older saves
                        // wrote the base theme's name in there).
                        let built_in = Theme::built_in_names().any(|known| known == name);
                        if !built_in {
                            theme.name = name.clone();
                        }
                        let keep = if built_in { String::new() } else { name };
                        self.open_theme_editor(&theme, keep, built_in);
                    }
                    Err(error) => self.status = format!("theme {name}: {error}"),
                }
                return true;
            }
            // Deleting asks twice: the first d names what would go, the
            // second lets it. Built-ins have no file and refuse politely.
            KeyCode::Char('d' | 'D') if primary => {
                let Some(name) = picker.selected_name().map(str::to_owned) else {
                    return true;
                };
                if picker.deleting.as_deref() == Some(name.as_str()) {
                    picker.deleting = None;
                    match super::super::theme::delete_named(&name) {
                        Ok(path) => {
                            self.status = format!(
                                "theme {name} deleted - {}",
                                status_file_path(&path, self.ui_settings.show_full_paths)
                            );
                            // The list no longer has it; reopen on what is
                            // left.
                            self.theme_picker = Some(ThemePicker::open(&self.theme));
                            self.preview_theme();
                        }
                        Err(error) => self.status = format!("theme {name}: {error}"),
                    }
                } else {
                    picker.deleting = Some(name.clone());
                    // The picker's verbs moved behind the primary
                    // modifier when typing became its filter, so a bare `d`
                    // now types into the search rather than confirming.
                    self.status = format!(
                        "press {} again to delete {name}",
                        self.capabilities.chord("D")
                    );
                }
                self.dirty_frame = true;
                return true;
            }
            KeyCode::Char('t' | 'T') if primary => self.keep_theme(),
            // The randomize row comes first: a guarded arm placed after an
            // unguarded one that covers the same keys never runs. On that
            // row the list is infinite in both directions, so ←/→ roll the
            // chance seed.
            KeyCode::Left | KeyCode::Right if picker.on_randomize() => {
                picker.roll_randomize(code == KeyCode::Right);
                self.schedule_theme_preview();
            }
            // ↑/↓ walk the list. ←/→ do not: they belong to the randomize
            // row above, the one place the list is infinite in both
            // directions.
            //
            // They are still the picker's keys and are swallowed here. If
            // they fell through, they would move the caret in the score
            // under the sheet, where nobody can see it.
            KeyCode::Left | KeyCode::Right => {}
            KeyCode::Up => {
                picker.move_by(-1);
                self.schedule_theme_preview();
            }
            KeyCode::Down => {
                picker.move_by(1);
                self.schedule_theme_preview();
            }
            KeyCode::PageUp | KeyCode::PageDown => {
                let page = picker
                    .geometry(self.frame)
                    .map_or(1, |(_, list)| usize::from(list.height).max(1));
                picker.selected = page_selection(
                    picker.selected,
                    picker.match_count(),
                    page,
                    code == KeyCode::PageDown,
                );
                picker.hold_scroll = false;
                self.schedule_theme_preview();
            }
            KeyCode::Home => {
                picker.selected = 0;
                picker.hold_scroll = false;
                self.schedule_theme_preview();
            }
            KeyCode::End => {
                picker.selected = picker.match_count().saturating_sub(1);
                picker.hold_scroll = false;
                self.schedule_theme_preview();
            }
            KeyCode::Backspace if !primary => {
                picker.pop_query();
                self.schedule_theme_preview();
            }
            KeyCode::Char(character) if !primary && !character.is_control() => {
                picker.push_query(character);
                self.schedule_theme_preview();
            }
            _ => return false,
        }
        self.dirty_frame = true;
        true
    }
}
