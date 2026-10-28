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
        self.status = if super::vim::enabled(self) {
            "theme - j/k browse · i types · Enter keep · Esc back".into()
        } else {
            "theme - type to search · ←/→/↑/↓ preview · Enter keep · Esc back".into()
        };
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
                        self.set_notice("theme changes discarded".into());
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
                    self.set_notice(format!(
                        "theme saved and kept - {}",
                        status_file_path(&path, self.ui_settings.show_full_paths)
                    ));
                    self.close_theme_editor(editor, true);
                } else if saving.closing {
                    // Saved on the way out: the file is written and in the
                    // list, and the studio wears what it had before.
                    self.set_notice(format!(
                        "theme saved - {}",
                        status_file_path(&path, self.ui_settings.show_full_paths)
                    ));
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
                    self.set_notice(format!(
                        "theme saved - {}",
                        status_file_path(&path, self.ui_settings.show_full_paths)
                    ));
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
        self.set_notice(match super::super::prefs::StudioPrefs::path() {
            Some(path) => format!(
                "theme {} - remembered in {}",
                self.theme.name,
                status_file_path(&path, self.ui_settings.show_full_paths)
            ),
            None => format!("theme {}", self.theme.name),
        });
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
                let from_built_in = Theme::built_in_names().any(|known| known == current.name)
                    && !super::super::theme::user_theme_exists(&current.name);
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
                        let built_in = Theme::built_in_names().any(|known| known == name)
                            && !super::super::theme::user_theme_exists(&name);
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
            // second lets it. A name only the studio ships has no file
            // behind it, so the first d says that and never arms: only a
            // theme of the player's own is a file there is to remove.
            KeyCode::Char('d' | 'D') if primary => {
                let Some(name) = picker.selected_name().map(str::to_owned) else {
                    return true;
                };
                if Theme::built_in_names().any(|known| known == name)
                    && !super::super::theme::user_theme_exists(&name)
                {
                    picker.deleting = None;
                    self.status =
                        format!("theme {name} ships with the studio and cannot be deleted");
                    self.dirty_frame = true;
                    return true;
                }
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

#[cfg(test)]
mod tests {
    mod theme_degenerate_sizes {
        //! Every built-in theme's visual must survive frames at degenerate sizes.
        //! This sweeps all of them. Run:
        //! `cargo test -p rustel-studio --features harness --lib
        //! theme_visuals_survive_degenerate_sizes -- --nocapture`.

        use ratatui::{Terminal, backend::TestBackend};
        use std::time::Instant;

        #[test]
        fn theme_visuals_survive_degenerate_sizes() {
            let sizes = [
                (80, 11),
                (80, 6),
                (3, 40),
                (30, 24),
                (80, 10),
                (2, 2),
                (1, 1),
            ];
            // The sweep reads the theme directory, which another test may
            // be scoping elsewhere: hold the shared lock so the sweep sees
            // one directory for its whole run.
            let _theme_dir = crate::theme::test_env::THEME_DIR_LOCK
                .lock()
                .unwrap_or_else(|poison| poison.into_inner());
            let names = crate::theme::Theme::available_names();
            for required in ["forest", "rustel-live"] {
                assert!(names.iter().any(|name| name == required));
                crate::theme::Theme::resolve(Some(required)).expect("bundled theme");
            }
            let mut failures = Vec::new();
            for name in names {
                if name == "randomize" {
                    continue;
                }
                let Ok(theme) = crate::theme::Theme::resolve(Some(&name)) else {
                    continue;
                };
                for &(width, height) in &sizes {
                    let directory = tempfile::tempdir().unwrap();
                    let mut app = crate::app::tests::support::app_over(directory.path());
                    app.theme = theme.clone();
                    app.invalidate_maps();
                    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
                    let drawn = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        // Two frames: animated state advances between them.
                        terminal
                            .draw(|frame| {
                                app.paint_into(frame, Instant::now()).expect("paint");
                            })
                            .unwrap();
                        terminal
                            .draw(|frame| {
                                app.paint_into(frame, Instant::now()).expect("paint");
                            })
                            .unwrap();
                    }));
                    if drawn.is_err() {
                        failures.push(format!("{name} at {width}x{height}"));
                    }
                }
            }
            assert!(failures.is_empty(), "{failures:#?}");
        }
    }

    mod theme_sheets {
        //! Tests for `app/theme_sheets.rs`.

        use crate::app::tests::support::*;
        use crate::app::*;

        /// Scrolling the theme editor's code must scroll it, and nothing else.
        ///
        /// The wheel extends a live drag on purpose, so a selection can continue
        /// past the bottom of the screen while the button is held. The release
        /// must end the drag, or a later scroll changes the selection.
        #[test]
        fn scrolling_the_theme_editor_does_not_drag_a_selection() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = app_over(directory.path());
            app.frame = Rect::new(0, 0, 120, 40);
            let theme = app.theme.clone();
            app.open_theme_editor(&theme, String::new(), true);
            if let Some(editor) = app.theme_editor.as_mut() {
                editor.tab = crate::theme_editor::EditorTab::Code;
            }
            let code = crate::theme_editor::ThemeEditorView::code_area(app.frame)
                .expect("the code area is drawn at this size");
            let (x, y) = (code.x + 2, code.y + 1);

            let click = |app: &mut App, kind: MouseEventKind| {
                app.handle_terminal_event(Event::Mouse(MouseEvent {
                    kind,
                    column: x,
                    row: y,
                    modifiers: KeyModifiers::NONE,
                }))
                .expect("mouse handled");
            };
            click(&mut app, MouseEventKind::Down(MouseButton::Left));
            click(&mut app, MouseEventKind::Up(MouseButton::Left));

            let before = app
                .theme_editor
                .as_ref()
                .expect("editor open")
                .code
                .viewport()
                .top_row;
            for _ in 0..4 {
                click(&mut app, MouseEventKind::ScrollDown);
            }
            let editor = app.theme_editor.as_ref().expect("editor open");
            assert!(
                editor.code.primary_selection().is_empty(),
                "the wheel dragged a selection out of a click that had already ended"
            );
            assert_ne!(
                editor.code.viewport().top_row,
                before,
                "the wheel did not scroll at all"
            );
        }

        #[test]
        fn held_arrows_set_theme_save_buttons_without_repeat_flicker() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = app_over(directory.path());
            let theme = app.theme.clone();
            app.open_theme_editor(&theme, "mine".into(), false);
            app.theme_editor.as_mut().expect("editor").saving =
                Some(crate::theme_editor::SaveSheet {
                    name: "mine".into(),
                    keep: false,
                    note: None,
                    closing: false,
                });

            for kind in [KeyEventKind::Press, KeyEventKind::Repeat] {
                app.handle_terminal_event(Event::Key(KeyEvent::new_with_kind(
                    KeyCode::Right,
                    KeyModifiers::NONE,
                    kind,
                )))
                .expect("right handled");
            }
            assert!(
                app.theme_editor
                    .as_ref()
                    .and_then(|editor| editor.saving.as_ref())
                    .is_some_and(|saving| saving.keep),
                "right consistently selects save & keep"
            );

            for kind in [KeyEventKind::Press, KeyEventKind::Repeat] {
                app.handle_terminal_event(Event::Key(KeyEvent::new_with_kind(
                    KeyCode::Left,
                    KeyModifiers::NONE,
                    kind,
                )))
                .expect("left handled");
            }
            assert!(
                app.theme_editor
                    .as_ref()
                    .and_then(|editor| editor.saving.as_ref())
                    .is_some_and(|saving| !saving.keep),
                "left consistently selects save"
            );
        }

        #[test]
        fn typing_filters_the_theme_picker_and_the_arrows_belong_to_the_chance_row() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = app_over(directory.path());
            app.toggle_theme_picker();
            for character in "snow".chars() {
                press(&mut app, KeyCode::Char(character), KeyModifiers::NONE);
            }
            let picker = app.theme_picker.as_ref().expect("picker open");
            assert_eq!(picker.query, "snow");
            assert_eq!(picker.match_count(), 1);
            assert_eq!(picker.selected_name(), Some("snow"));
            // Typing waits for the settle, like the arrows; settled, it previews
            // the only match.
            app.theme_apply_due = None;
            app.preview_theme();
            assert_eq!(app.theme.name, "snow", "typing previews the only match");

            for _ in 0..4 {
                press(&mut app, KeyCode::Backspace, KeyModifiers::NONE);
            }
            app.theme_apply_due = None;
            app.preview_theme();

            // ↑/↓ walk the list.
            let before = app.theme.name.clone();
            press(&mut app, KeyCode::Down, KeyModifiers::NONE);
            app.theme_apply_due = None;
            app.preview_theme();
            assert_ne!(app.theme.name, before, "Down previews the next theme");
            press(&mut app, KeyCode::Up, KeyModifiers::NONE);
            app.theme_apply_due = None;
            app.preview_theme();
            assert_eq!(app.theme.name, before, "Up previews the previous theme");

            // ←/→ do not walk the list: they belong to the randomize row
            // and to the terminal family, where they step between variants.
            press(&mut app, KeyCode::Right, KeyModifiers::NONE);
            assert_eq!(
                app.theme.name, before,
                "Right walked the list instead of leaving it alone"
            );
            press(&mut app, KeyCode::Left, KeyModifiers::NONE);
            assert_eq!(app.theme.name, before, "Left walked the list");

            // On the randomize row they roll it, and the row grows a back arrow
            // once there is a roll to go back to.
            let chance = app
                .theme_picker
                .as_ref()
                .expect("picker open")
                .matches()
                .position(|name| name == crate::theme::RANDOMIZE_THEME)
                .expect("the randomize row is in the list");
            let at = app.theme_picker.as_ref().expect("picker").selected;
            for _ in at..chance {
                press(&mut app, KeyCode::Down, KeyModifiers::NONE);
            }
            assert!(app.theme_picker.as_ref().expect("picker").on_randomize());
            assert!(!app.theme_picker.as_ref().expect("picker").rolled);
            let seed = app.theme_picker.as_ref().expect("picker").randomize_seed;
            press(&mut app, KeyCode::Right, KeyModifiers::NONE);
            let picker = app.theme_picker.as_ref().expect("picker");
            assert_ne!(
                picker.randomize_seed, seed,
                "Right did not roll the randomize row"
            );
            assert!(picker.rolled, "the back arrow never appears");
        }

        /// The picker's modified E opens the editor. A built-in theme arrives
        /// without its name, so a save needs a new name: the built-in themes
        /// cannot be overwritten.
        #[test]
        fn editing_a_built_in_theme_starts_the_editor_without_its_name() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = app_over(directory.path());
            app.toggle_theme_picker();
            press(&mut app, KeyCode::Char('e'), KeyModifiers::CONTROL);

            assert!(app.theme_picker.is_none(), "the picker hands over");
            let editor = app.theme_editor.as_ref().expect("editor open");
            assert!(editor.from_built_in);
            assert!(editor.name.is_empty(), "a built-in cannot keep its name");
            assert_eq!(app.focus, Focus::Panel(PanelKind::ThemeEditor));

            // s raises the save sheet, seeded with a free name. The sheet never
            // offers a built-in's own name.
            press(&mut app, KeyCode::Char('s'), KeyModifiers::NONE);
            let editor = app.theme_editor.as_ref().expect("still open");
            let sheet = editor.saving.as_ref().expect("save sheet up");
            assert!(!sheet.name.is_empty(), "a suggestion is seeded");
            assert!(
                !Theme::built_in_names().any(|known| known == sheet.name),
                "never a built-in's name: {}",
                sheet.name
            );
        }

        /// The picker's modified d deletes only a theme of the player's
        /// own, and asks twice before it does. A theme the studio ships
        /// has no file behind its name, so the first d says that rather
        /// than arming a delete whose second press would go looking for a
        /// file that is not there.
        #[test]
        fn a_built_in_theme_is_never_armed_for_deletion() {
            let _theme_dir = THEME_DIR_LOCK
                .lock()
                .unwrap_or_else(|poison| poison.into_inner());
            let directory = tempfile::tempdir().expect("temp dir");
            let themes = directory.path().join("themes");
            std::fs::create_dir_all(&themes).expect("theme directory");
            std::fs::write(themes.join("a-yours.json"), "{}").expect("theme file");
            let _scoped = ScopedThemeDir::set(&themes);
            let mut app = app_over(directory.path());
            app.toggle_theme_picker();

            // The picker opens on the studio's own default theme.
            let opened_on = app
                .theme_picker
                .as_ref()
                .expect("picker open")
                .selected_name()
                .expect("a row is selected")
                .to_owned();
            assert!(
                Theme::built_in_names().any(|known| known == opened_on),
                "the picker opens on a built-in: {opened_on}"
            );
            press(&mut app, KeyCode::Char('d'), KeyModifiers::CONTROL);
            assert!(
                app.status.contains("cannot be deleted"),
                "the first press says why: {}",
                app.status
            );
            // The second press - a capital one, both spellings arm nothing -
            // is the same refusal, not a file hunt.
            press(&mut app, KeyCode::Char('D'), KeyModifiers::CONTROL);
            assert!(
                app.status.contains("cannot be deleted"),
                "the second press refuses the same way: {}",
                app.status
            );

            // A theme of the player's own is a file, and the same keys
            // take it, twice asked. Walk to its row the way a reader
            // would.
            let rows = app
                .theme_picker
                .as_ref()
                .expect("picker open")
                .match_count();
            for _ in 0..=rows {
                if app
                    .theme_picker
                    .as_ref()
                    .expect("picker open")
                    .selected_name()
                    .is_some_and(|name| name == "a-yours")
                {
                    break;
                }
                press(&mut app, KeyCode::Down, KeyModifiers::NONE);
            }
            assert_eq!(
                app.theme_picker
                    .as_ref()
                    .expect("picker open")
                    .selected_name(),
                Some("a-yours"),
                "walking the list reaches the file's row"
            );
            press(&mut app, KeyCode::Char('d'), KeyModifiers::CONTROL);
            assert!(app.status.contains("again"), "asks first: {}", app.status);
            press(&mut app, KeyCode::Char('d'), KeyModifiers::CONTROL);
            // The same press reopens the picker on what is left and
            // previews it, so the status the deletion set is already
            // overwritten. The file and the list are the record.
            assert!(!themes.join("a-yours.json").is_file(), "the file is gone");
            assert!(
                !app.theme_picker
                    .as_ref()
                    .expect("picker reopened on what is left")
                    .names
                    .iter()
                    .any(|name| name == "a-yours"),
                "the reopened list has no row for it"
            );
        }

        /// A theme of the player's own saved under a name the studio ships
        /// too stays theirs: the editor opens it under its name, a new
        /// theme from it is no built-in's variation, and the modified d
        /// deletes the file. Without the file the name is the shipped
        /// theme's again.
        #[test]
        fn a_user_theme_under_a_shipped_name_is_edited_and_deleted_as_its_own() {
            let _theme_dir = THEME_DIR_LOCK
                .lock()
                .unwrap_or_else(|poison| poison.into_inner());
            let directory = tempfile::tempdir().expect("temp dir");
            let themes = directory.path().join("themes");
            std::fs::create_dir_all(&themes).expect("theme directory");
            let mine = themes.join("terminal.json");
            // A save names the file and the theme inside it alike.
            let mut theme = Theme::built_in_default();
            theme.name = "terminal".into();
            std::fs::write(&mine, theme.to_json()).expect("theme file");
            let _scoped = ScopedThemeDir::set(&themes);
            let mut app = app_over(directory.path());
            app.toggle_theme_picker();
            for character in "terminal".chars() {
                press(&mut app, KeyCode::Char(character), KeyModifiers::NONE);
            }
            assert_eq!(
                app.theme_picker
                    .as_ref()
                    .expect("picker open")
                    .selected_name(),
                Some("terminal")
            );
            // The selection settles, as the event loop lets it.
            app.preview_theme();

            press(&mut app, KeyCode::Char('e'), KeyModifiers::CONTROL);
            let editor = app.theme_editor.as_ref().expect("editor open");
            assert!(!editor.from_built_in, "the file is the player's");
            assert_eq!(editor.name, "terminal", "a save keeps its name");
            press(&mut app, KeyCode::Esc, KeyModifiers::NONE);
            press(&mut app, KeyCode::Char('n'), KeyModifiers::CONTROL);
            assert!(
                !app.theme_editor
                    .as_ref()
                    .expect("editor open")
                    .from_built_in,
                "a new theme from the player's file is not a built-in's variation"
            );
            press(&mut app, KeyCode::Esc, KeyModifiers::NONE);

            press(&mut app, KeyCode::Char('d'), KeyModifiers::CONTROL);
            assert!(app.status.contains("again"), "asks first: {}", app.status);
            press(&mut app, KeyCode::Char('d'), KeyModifiers::CONTROL);
            assert!(!mine.is_file(), "the file is gone");
            press(&mut app, KeyCode::Char('d'), KeyModifiers::CONTROL);
            assert!(
                app.status.contains("cannot be deleted"),
                "the shipped theme is left: {}",
                app.status
            );
        }

        /// Hand the sketch to the worker the way the event loop does.
        ///
        /// `sync_theme_sketch` sends over the shared two-slot command queue and
        /// is allowed to drop - the loop pumps it again next pass, so a dropped
        /// send is a beat late rather than lost. A test that asserts on what was
        /// sent has to pump too, or it is asserting on the worker thread's
        /// scheduling.
        #[cfg(feature = "hydra")]
        fn settle_theme_sketch(app: &mut App) {
            for _ in 0..400 {
                app.sync_theme_sketch();
                if app.theme_sketch_sent.is_some() {
                    return;
                }
                std::thread::sleep(Duration::from_millis(5));
            }
            panic!("the theme sketch was never accepted by the worker");
        }

        #[cfg(feature = "hydra")]
        #[test]
        fn a_taken_snippet_becomes_the_theme_sketch_while_the_editor_is_open() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = app_over(directory.path());
            app.toggle_theme_picker();
            press(&mut app, KeyCode::Char('n'), KeyModifiers::CONTROL);
            assert!(app.theme_editor.is_some());

            // Open the shelf on the snippets tab and take the first snippet.
            let mut shelf = crate::reference::ReferencePanel::browse(&app.reference);
            shelf.tab = crate::reference::Tab::Examples;
            // The chains are the hydra section's, which the tab does not open on.
            let hydra =
                crate::examples::section_of(crate::examples::Kind::Hydra).expect("a hydra section");
            shelf.section_open.clear();
            shelf.section_open.insert(hydra);
            shelf.snippet_open.insert((hydra, 0));
            shelf.snippet_selected = shelf
                .snippet_lines()
                .iter()
                .position(|line| *line == crate::reference::SnippetLine::Snippet(hydra, 0, 0))
                .unwrap();
            app.reference_panel = Some(shelf);
            app.focus_panel(PanelKind::Reference);
            press(&mut app, KeyCode::Enter, KeyModifiers::NONE);

            let editor = app.theme_editor.as_ref().expect("editor still open");
            let sketch = editor.draft.hydra.as_deref().expect("the chain landed");
            assert!(sketch.contains(".out()"), "a whole chain, not a name");
            assert_eq!(app.theme.hydra.as_deref(), Some(sketch), "and it is live");
            assert!(app.reference_panel.is_none(), "the shelf closed behind it");
            assert_eq!(app.focus, Focus::Panel(PanelKind::ThemeEditor));
        }

        #[cfg(feature = "hydra")]
        #[test]
        fn keyboard_and_mouse_theme_previews_never_confirm_camera_acquisition() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = app_over(directory.path());
            app.theme_picker = Some(ThemePicker {
                names: vec!["rustel-dark".into(), "webcam-clean".into()],
                query: String::new(),
                selected: 0,
                original: Box::new(app.theme.clone()),
                deleting: None,
                randomize_seed: 1,
                rolled: false,
                scroll: std::cell::Cell::new(0),
                hold_scroll: false,
                vim_insert: false,
            });
            app.focus = Focus::Panel(PanelKind::Theme);
            press(&mut app, KeyCode::Down, KeyModifiers::NONE);
            assert_eq!(
                app.theme_picker.as_ref().map(|picker| picker.selected),
                Some(1),
                "the cursor moved"
            );
            assert_ne!(
                app.theme.name, "webcam-clean",
                "a held arrow must not apply every theme it passes"
            );
            assert!(
                app.theme_apply_due.is_some(),
                "keyboard browsing waits before applying"
            );
            app.theme_apply_due = None;
            app.preview_theme();
            assert_eq!(app.theme.name, "webcam-clean");
            app.theme_sketch_sent = None;
            settle_theme_sketch(&mut app);
            assert!(matches!(
                app.theme_sketch_sent.as_ref(),
                Some((Some(_), false))
            ));

            // Enter/keep is the explicit confirmation boundary. Avoid writing a
            // real preferences file in this unit test; establish the state that
            // `keep_theme` establishes and prove the next sync carries consent.
            app.theme_picker = None;
            app.theme_camera_confirmed = Some(app.theme.name.clone());
            app.theme_sketch_sent = None;
            settle_theme_sketch(&mut app);
            assert!(matches!(
                app.theme_sketch_sent.as_ref(),
                Some((Some(_), true))
            ));

            let mut mouse_app = app_over(directory.path());
            mouse_app.frame = Rect::new(0, 0, 80, 30);
            mouse_app.theme_picker = Some(ThemePicker {
                names: vec!["rustel-dark".into(), "webcam-distort".into()],
                query: String::new(),
                selected: 0,
                original: Box::new(mouse_app.theme.clone()),
                deleting: None,
                randomize_seed: 1,
                rolled: false,
                scroll: std::cell::Cell::new(0),
                hold_scroll: false,
                vim_insert: false,
            });
            let (_, list) = mouse_app
                .theme_picker
                .as_ref()
                .unwrap()
                .geometry(mouse_app.frame)
                .expect("picker geometry");
            mouse_app
                .handle_mouse(MouseEvent {
                    kind: MouseEventKind::Down(MouseButton::Left),
                    column: list.x,
                    row: list.y + 1,
                    modifiers: KeyModifiers::NONE,
                })
                .expect("mouse preview");
            assert_eq!(mouse_app.theme.name, "webcam-distort");
            assert!(
                mouse_app.theme_apply_due.is_none(),
                "a click applies the theme immediately"
            );
            mouse_app.theme_sketch_sent = None;
            settle_theme_sketch(&mut mouse_app);
            assert!(matches!(
                mouse_app.theme_sketch_sent.as_ref(),
                Some((Some(_), false))
            ));
        }

        #[test]
        fn keyboard_theme_browsing_applies_only_the_last_row() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = app_over(directory.path());
            let original = app.theme.name.clone();
            app.theme_picker = Some(ThemePicker {
                names: vec![
                    "rustel-dark".into(),
                    "rustel-light".into(),
                    "strudel".into(),
                ],
                query: String::new(),
                selected: 0,
                original: Box::new(app.theme.clone()),
                deleting: None,
                randomize_seed: 1,
                rolled: false,
                scroll: std::cell::Cell::new(0),
                hold_scroll: false,
                vim_insert: false,
            });
            app.focus = Focus::Panel(PanelKind::Theme);
            press(&mut app, KeyCode::Down, KeyModifiers::NONE);
            press(&mut app, KeyCode::Down, KeyModifiers::NONE);
            assert_eq!(
                app.theme_picker.as_ref().map(|picker| picker.selected),
                Some(2)
            );
            assert_eq!(
                app.theme.name, original,
                "fast arrows must not activate every theme they pass"
            );
            app.theme_apply_due = None;
            app.preview_theme();
            assert_eq!(app.theme.name, "strudel");
        }

        /// The shared guard and scoped directory for the process-wide
        /// theme directory variable live beside the variable's reader in
        /// `theme.rs`, so every test that touches it takes the same lock.
        use crate::theme::test_env::{ScopedThemeDir, THEME_DIR_LOCK};

        /// A taken name is refused and a free variation is offered in its
        /// place. The existing theme is not overwritten.
        #[test]
        fn a_conflicting_save_name_is_turned_into_a_free_suggestion() {
            let _theme_dir = THEME_DIR_LOCK
                .lock()
                .unwrap_or_else(|poison| poison.into_inner());
            let directory = tempfile::tempdir().expect("temp dir");
            // The suggestion depends on the names that the theme directory
            // already holds. Use an empty directory, so a `rustel-dark-2.json` in
            // the real one cannot change the result.
            let themes = directory.path().join("themes");
            std::fs::create_dir_all(&themes).expect("theme directory");
            let _scoped = ScopedThemeDir::set(&themes);
            let mut app = app_over(directory.path());
            app.toggle_theme_picker();
            press(&mut app, KeyCode::Char('n'), KeyModifiers::CONTROL);
            press(&mut app, KeyCode::Char('s'), KeyModifiers::NONE);
            // Type a built-in's name over the suggestion.
            if let Some(editor) = app.theme_editor.as_mut()
                && let Some(sheet) = editor.saving.as_mut()
            {
                sheet.name = "rustel-dark".into();
            }
            press(&mut app, KeyCode::Enter, KeyModifiers::NONE);
            let editor = app.theme_editor.as_ref().expect("editor still open");
            let sheet = editor
                .saving
                .as_ref()
                .expect("sheet still up - nothing saved");
            assert!(
                sheet
                    .note
                    .as_deref()
                    .is_some_and(|note| note.contains("taken")),
                "the refusal says why: {:?}",
                sheet.note
            );
            assert_eq!(sheet.name, "rustel-dark-2", "and offers the free variation");
        }

        /// Enter on a colour row opens the painter; the arrows paint LIVE and
        /// Esc puts the old colour back.
        #[test]
        fn the_colour_picker_paints_live_and_esc_reverts() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = app_over(directory.path());
            app.toggle_theme_picker();
            press(&mut app, KeyCode::Char('n'), KeyModifiers::CONTROL);
            // Walk to the first colour row and open the picker.
            for _ in 0..crate::theme_editor::HEAD_ROWS {
                press(&mut app, KeyCode::Down, KeyModifiers::NONE);
            }
            let before = app.theme.background;
            press(&mut app, KeyCode::Enter, KeyModifiers::NONE);
            assert!(
                app.theme_editor
                    .as_ref()
                    .is_some_and(|e| e.picker.is_some()),
                "the painter opens"
            );
            press(&mut app, KeyCode::Right, KeyModifiers::NONE);
            assert_ne!(app.theme.background, before, "painting applies live");
            press(&mut app, KeyCode::Esc, KeyModifiers::NONE);
            assert!(
                app.theme_editor
                    .as_ref()
                    .is_some_and(|e| e.picker.is_none()),
                "Esc closes the painter"
            );
            assert_eq!(app.theme.background, before, "and puts the colour back");
            // The editor itself is still open - Esc walked one rung only.
            assert!(app.theme_editor.is_some());
        }

        /// The editor's Esc lands on the theme that was applied when it opened.
        ///
        /// Walking the picker changes the theme, so the theme in force when the
        /// editor opens is the one that Esc restores.
        #[test]
        fn escaping_the_theme_editor_puts_the_applied_theme_back() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = app_over(directory.path());
            let opened_on = app.theme.name.clone();
            app.toggle_theme_picker();
            press(&mut app, KeyCode::Down, KeyModifiers::NONE);
            app.theme_apply_due = None;
            app.preview_theme();
            let walked_to = app.theme.name.clone();
            assert_ne!(walked_to, opened_on, "walking the list changes the theme");
            press(&mut app, KeyCode::Char('e'), KeyModifiers::CONTROL);
            assert!(app.theme_editor.is_some());

            press(&mut app, KeyCode::Esc, KeyModifiers::NONE);
            assert!(app.theme_editor.is_none());
            assert_eq!(
                app.theme.name, walked_to,
                "Esc lands on the theme the editor opened from"
            );
            assert!(
                app.theme_picker.is_some(),
                "Esc goes back to the list it came from"
            );
            assert_eq!(app.focus, Focus::Panel(PanelKind::Theme));
        }

        /// Keeping a camera theme says what to expect: the picture in a moment,
        /// or the setting that stands in its way.
        #[cfg(feature = "hydra")]
        #[test]
        fn keeping_a_camera_theme_says_what_stands_between_the_reader_and_it() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = app_over(directory.path());
            app.theme = Theme::built_in("webcam-hacker").expect("webcam-hacker");
            app.ui_settings.hydra_webcam = false;
            app.theme_picker = Some(ThemePicker::open(&app.theme));
            assert_eq!(
                app.theme_camera_note(None),
                Some(crate::theme::CameraNote::Off)
            );
            app.keep_theme();
            assert!(
                app.status.contains("turn on Hydra webcam"),
                "{}",
                app.status
            );

            app.ui_settings.hydra_webcam = true;
            app.theme_picker = Some(ThemePicker::open(&app.theme));
            assert_eq!(
                app.theme_camera_note(None),
                Some(crate::theme::CameraNote::Starting)
            );
            app.keep_theme();
            assert!(app.status.contains("webcam starting"), "{}", app.status);
            // A theme without a camera says nothing of the kind.
            app.theme = Theme::built_in_default();
            assert_eq!(app.theme_camera_note(None), None);
        }

        /// The wheel over the picker walks its list, like the arrows.
        #[test]
        fn the_wheel_walks_the_theme_picker() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = app_over(directory.path());
            app.frame = Rect::new(0, 0, 120, 40);
            app.toggle_theme_picker();
            let wheel = |app: &mut App, kind: MouseEventKind| {
                app.handle_mouse(MouseEvent {
                    kind,
                    column: 10,
                    row: 10,
                    modifiers: KeyModifiers::NONE,
                })
                .expect("mouse");
            };
            // The picker opens on the theme in use, wherever that is in the list.
            let start = app.theme_picker.as_ref().expect("picker").selected;
            wheel(&mut app, MouseEventKind::ScrollDown);
            wheel(&mut app, MouseEventKind::ScrollDown);
            assert_eq!(
                app.theme_picker.as_ref().expect("picker").selected,
                start + 2
            );
            wheel(&mut app, MouseEventKind::ScrollUp);
            assert_eq!(
                app.theme_picker.as_ref().expect("picker").selected,
                start + 1
            );
            assert!(app.status.starts_with("theme - "), "{}", app.status);
        }

        /// Esc over unsaved work asks; a second Esc lets it go.
        #[test]
        fn escaping_a_changed_theme_editor_asks_and_a_second_esc_discards() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = app_over(directory.path());
            let opened_on = app.theme.name.clone();
            app.toggle_theme_picker();
            press(&mut app, KeyCode::Char('e'), KeyModifiers::CONTROL);
            let editor = app.theme_editor.as_mut().expect("editor");
            assert!(!editor.is_modified(), "an untouched draft is not a change");
            editor.draft.accent = ratatui::style::Color::Rgb(1, 2, 3);
            assert!(editor.is_modified());

            press(&mut app, KeyCode::Esc, KeyModifiers::NONE);
            let editor = app.theme_editor.as_ref().expect("still open, asking");
            assert!(
                editor.saving.as_ref().is_some_and(|sheet| sheet.closing),
                "the save sheet is up, in its leaving mode"
            );
            assert!(app.theme_picker.is_none());

            press(&mut app, KeyCode::Esc, KeyModifiers::NONE);
            assert!(app.theme_editor.is_none(), "the second Esc leaves");
            assert!(app.theme_picker.is_some(), "onto the list");
            assert_eq!(app.theme.name, opened_on, "with the change let go");
            assert_ne!(app.theme.accent, ratatui::style::Color::Rgb(1, 2, 3));
        }

        /// Enter on that sheet saves, keeps, and still lands on the list.
        #[test]
        fn saving_from_the_leaving_sheet_keeps_the_theme_and_lands_on_the_list() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = app_over(directory.path());
            let opened_on = app.theme.name.clone();
            app.toggle_theme_picker();
            press(&mut app, KeyCode::Char('e'), KeyModifiers::CONTROL);
            app.theme_editor.as_mut().expect("editor").draft.accent =
                ratatui::style::Color::Rgb(1, 2, 3);
            press(&mut app, KeyCode::Esc, KeyModifiers::NONE);
            assert!(
                app.theme_editor
                    .as_ref()
                    .is_some_and(|editor| editor.saving.is_some())
            );

            press(&mut app, KeyCode::Enter, KeyModifiers::NONE);
            assert!(
                app.theme_editor.is_none(),
                "saved and closed: {}",
                app.status
            );
            assert!(app.theme_picker.is_some(), "onto the list");
            assert_eq!(
                app.theme.accent,
                ratatui::style::Color::Rgb(1, 2, 3),
                "wearing the saved theme"
            );
            assert_ne!(app.theme.name, opened_on, "under a name of its own");
            assert!(
                app.status.starts_with("theme saved and kept"),
                "{}",
                app.status
            );
            app.status_notice.as_mut().expect("theme save notice").1 = Instant::now();
            app.dirty_frame = false;
            app.pump_turn();
            assert!(app.status.is_empty());
            assert!(app.dirty_frame, "expiry redraws without a keypress");
        }

        /// A roll of the randomize row waits until the arrows stop. The palette
        /// and the sketch then apply together.
        ///
        /// Every distinct sketch compiles three GPU objects, and a held arrow
        /// rolls about thirty a second - enough to trip the renderer's
        /// shaders-per-device guard, whose answer is to rebuild the whole device.
        #[cfg(feature = "hydra")]
        #[test]
        fn rolling_waits_for_the_settle_then_lands_palette_and_sketch_together() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = app_over(directory.path());
            app.frame = Rect::new(0, 0, 100, 30);
            app.toggle_theme_picker();
            let at = app
                .theme_picker
                .as_ref()
                .expect("picker")
                .matches()
                .position(|name| name == crate::theme::RANDOMIZE_THEME)
                .expect("randomize is in the list");
            if let Some(picker) = app.theme_picker.as_mut() {
                picker.selected = at;
            }
            app.preview_theme();
            let before = app.theme.background;

            press(&mut app, KeyCode::Right, KeyModifiers::NONE);
            assert!(app.theme_apply_due.is_some(), "a roll left a settle owed");
            assert_eq!(
                app.theme.background, before,
                "a roll must not apply while the arrows are still repeating"
            );

            // Each settled roll changes the palette, and the sketch goes with it
            // rather than waiting behind a timer of its own.
            let mut palettes = Vec::new();
            for _ in 0..8 {
                press(&mut app, KeyCode::Right, KeyModifiers::NONE);
                app.theme_apply_due = None;
                app.preview_theme();
                palettes.push(app.theme.background);
            }
            palettes.dedup();
            assert!(
                palettes.len() > 1,
                "the palette must change across settled rolls"
            );

            // Closing settles the apply rather than leaving the timer to no one.
            press(&mut app, KeyCode::Enter, KeyModifiers::NONE);
            assert!(app.theme_apply_due.is_none(), "the apply stayed owed");
        }

        /// A theme suggests its opacities; the reader's choice outranks it and
        /// outlives a restart.
        ///
        /// The whole contract turns on one distinction that a value alone cannot
        /// carry: 85 because this theme asked for it is not the same as 85 because
        /// the reader chose it. The first must be replaceable by the next theme,
        /// the second must not.
        #[test]
        fn a_theme_suggests_opacity_and_the_reader_overrules_it() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = app_over(directory.path());

            // Applying a theme takes its three, and records that none of them is
            // the reader's.
            app.theme.opacity = crate::theme::ThemeOpacity {
                backdrop: Some(30),
                interface: Some(40),
                editor: Some(20),
            };
            app.keep_theme();
            assert_eq!(app.ui_settings.backdrop_opacity, 30);
            assert_eq!(app.ui_settings.interface_opacity, 40);
            assert_eq!(app.ui_settings.editor_opacity, 20);
            assert_eq!(
                app.prefs.backdrop_opacity, None,
                "a suggestion is not a choice"
            );

            // A theme that says nothing asks for the placeholder, not for
            // whatever the last theme happened to want.
            app.theme.opacity = crate::theme::ThemeOpacity::default();
            app.theme.ui_opacity = None;
            app.keep_theme();
            let default = crate::theme::THEME_OPACITY_DEFAULT;
            assert_eq!(app.ui_settings.backdrop_opacity, default);
            assert_eq!(app.ui_settings.editor_opacity, default);

            // The reader turning a row claims all three.
            app.ui_settings.editor_opacity = 77;
            app.prefs.claim_opacities(&app.ui_settings);
            assert_eq!(app.prefs.editor_opacity, Some(77));

            // And a claimed value survives the next theme's suggestion on a
            // restart, which is the half a value alone cannot express.
            let mut loud = crate::theme::Theme::built_in_default();
            loud.opacity = crate::theme::ThemeOpacity {
                backdrop: Some(11),
                interface: Some(12),
                editor: Some(13),
            };
            let reopened = app.prefs.ui_settings_for(&loud);
            assert_eq!(
                reopened.editor_opacity, 77,
                "the reader's choice was overwritten by a theme on restart"
            );

            // Whereas an unclaimed one follows the theme.
            app.prefs.follow_theme_opacity();
            assert_eq!(app.prefs.ui_settings_for(&loud).editor_opacity, 13);

            // Selecting a calibrated preset explicitly replaces even manually
            // claimed settings; a restart alone, above, still preserves them.
            for name in ["rustel-drift", "unicorn", "smoke", "randomize"] {
                app.ui_settings.backdrop_opacity = 0;
                app.ui_settings.interface_opacity = 100;
                app.ui_settings.editor_opacity = 100;
                app.prefs.claim_opacities(&app.ui_settings);
                app.theme = Theme::built_in(name).unwrap();
                let expected = app.theme.opacities();
                app.keep_theme();
                assert_eq!(
                    app.ui_settings.backdrop_opacity, expected.backdrop,
                    "{name}"
                );
                assert_eq!(
                    app.ui_settings.interface_opacity, expected.interface,
                    "{name}"
                );
                assert_eq!(app.ui_settings.editor_opacity, expected.editor, "{name}");
                assert_eq!(app.prefs.backdrop_opacity, None);
                assert_eq!(app.prefs.interface_opacity, None);
                assert_eq!(app.prefs.editor_opacity, None);
            }
        }

        /// Both doors out of the picker keep what is on screen.
        #[test]
        fn the_picker_keeps_whatever_it_is_closed_on() {
            let directory = tempfile::tempdir().expect("temp dir");
            for close in [KeyCode::Esc, KeyCode::Enter] {
                let mut app = app_over(directory.path());
                let opened_on = app.theme.name.clone();
                app.toggle_theme_picker();
                press(&mut app, KeyCode::Down, KeyModifiers::NONE);
                // Walking is debounced: the row is landed on only at close.
                assert_eq!(
                    app.theme.name, opened_on,
                    "{close:?}: the walk applied before the settle"
                );
                let walked_to = app
                    .theme_picker
                    .as_ref()
                    .expect("picker still open")
                    .selected_name()
                    .map(str::to_owned)
                    .expect("a named row");
                press(&mut app, close, KeyModifiers::NONE);
                assert!(app.theme_picker.is_none(), "{close:?} left the picker open");
                assert_eq!(
                    app.theme.name, walked_to,
                    "{close:?} put the old theme back instead of keeping this one"
                );
            }
        }

        /// The form applies live: stepping ui opacity changes the draft, and the
        /// change is on the studio's theme immediately, before any save.
        #[test]
        fn stepping_a_form_row_applies_to_the_studio_at_once() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = app_over(directory.path());
            app.toggle_theme_picker();
            press(&mut app, KeyCode::Char('n'), KeyModifiers::CONTROL);
            let editor = app.theme_editor.as_ref().expect("editor open");
            assert!(!editor.from_built_in || editor.name.is_empty());

            // Row 1 is the first of the three opacities; → steps it up.
            press(&mut app, KeyCode::Down, KeyModifiers::NONE);
            press(&mut app, KeyCode::Right, KeyModifiers::NONE);
            let stepped = app
                .theme_editor
                .as_ref()
                .expect("still open")
                .draft
                .opacity
                .backdrop;
            assert!(stepped.is_some(), "the step writes the field");
            assert_eq!(
                Some(app.ui_settings.backdrop_opacity),
                stepped,
                "and the studio wears it"
            );
        }

        /// With the editor focused on its code tab, letters belong to the theme's
        /// JSON, not the score.
        #[test]
        fn code_tab_typing_stays_out_of_the_score() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = app_over(directory.path());
            let score = app.scenes.current().editor.source().to_owned();
            app.toggle_theme_picker();
            press(&mut app, KeyCode::Char('n'), KeyModifiers::CONTROL);
            press(&mut app, KeyCode::Tab, KeyModifiers::NONE);
            let before = app
                .theme_editor
                .as_ref()
                .expect("editor open")
                .code
                .revision();
            press(&mut app, KeyCode::Char('x'), KeyModifiers::NONE);
            let editor = app.theme_editor.as_ref().expect("still open");
            assert_ne!(
                editor.code.revision(),
                before,
                "the letter reached the JSON"
            );
            assert!(editor.code_dirty_at.is_some(), "and started the debounce");
            assert_eq!(
                app.scenes.current().editor.source(),
                score,
                "the score never saw it"
            );
        }

        /// Ctrl+T with the editor open must not raise a picker under its
        /// sheet - an invisible focused picker whose n would silently replace
        /// the draft.
        #[test]
        fn the_theme_chord_cannot_open_a_picker_under_the_editor() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = app_over(directory.path());
            app.toggle_theme_picker();
            press(&mut app, KeyCode::Char('n'), KeyModifiers::CONTROL);
            assert!(app.theme_editor.is_some());

            press(&mut app, KeyCode::Char('t'), KeyModifiers::CONTROL);
            assert!(app.theme_picker.is_none(), "no picker under the sheet");
            assert!(app.theme_editor.is_some(), "the draft survives");
            assert!(app.status.contains("theme editor"));
        }

        /// Tab with a half-typed entry commits it rather than stranding it -
        /// a stranded entry made Esc a dead key on the code tab.
        #[test]
        fn tab_commits_the_open_entry_before_flipping() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = app_over(directory.path());
            app.toggle_theme_picker();
            press(&mut app, KeyCode::Char('n'), KeyModifiers::CONTROL);
            press(&mut app, KeyCode::Down, KeyModifiers::NONE); // first opacity row
            press(&mut app, KeyCode::Enter, KeyModifiers::NONE); // entry opens
            for digit in ['4', '2'] {
                press(&mut app, KeyCode::Char(digit), KeyModifiers::NONE);
            }
            press(&mut app, KeyCode::Tab, KeyModifiers::NONE);

            let editor = app.theme_editor.as_ref().expect("open");
            assert!(editor.entry.is_none(), "the entry is not stranded");
            assert_eq!(editor.draft.opacity.backdrop, Some(42), "and it committed");
            // …and Esc from the code tab leaves too - asking first, since the
            // committed entry is an unsaved change.
            press(&mut app, KeyCode::Esc, KeyModifiers::NONE);
            assert!(
                app.theme_editor.as_ref().is_some_and(|editor| editor
                    .saving
                    .as_ref()
                    .is_some_and(|sheet| sheet.closing)),
                "the first Esc asks about the unsaved change"
            );
            press(&mut app, KeyCode::Esc, KeyModifiers::NONE);
            assert!(app.theme_editor.is_none(), "the second lets it go");
            assert!(app.theme_picker.is_some(), "and lands on the list");
        }

        /// The draft's ui opacity applies live and Esc puts the old one back;
        /// entry typing that would edit the invisible score is swallowed.
        #[test]
        fn draft_opacity_is_live_and_the_hidden_score_is_safe() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = app_over(directory.path());
            let before_opacity = app.ui_settings.backdrop_opacity;
            let score = app.scenes.current().editor.source().to_owned();
            app.toggle_theme_picker();
            press(&mut app, KeyCode::Char('n'), KeyModifiers::CONTROL);
            press(&mut app, KeyCode::Down, KeyModifiers::NONE);
            press(&mut app, KeyCode::Right, KeyModifiers::NONE); // step opacity
            let stepped = app.theme.opacity.backdrop.expect("stepped");
            assert_eq!(
                app.ui_settings.backdrop_opacity, stepped,
                "the step is on the interface at once"
            );
            // A paste with the form focused must not reach the hidden score.
            app.handle_terminal_event(Event::Paste("s(\"jvbass\")".into()))
                .expect("paste handled");
            assert_eq!(app.scenes.current().editor.source(), score);

            press(&mut app, KeyCode::Esc, KeyModifiers::NONE);
            assert_eq!(
                app.ui_settings.backdrop_opacity, before_opacity,
                "Esc puts the opacity back with the theme"
            );
        }
    }
}
