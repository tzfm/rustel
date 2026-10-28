//! Keyboard routing for the studio. `handle_terminal_event` takes each terminal
//! event and decides who gets a key: the transport (update/stop), the focused
//! panel through `dispatch_panel_key`, or the score. It reads as its steps in
//! priority order; a press climbs `route_key_press`, a held key
//! `route_key_repeat`, and a paste `route_paste`. The small helpers that
//! sort keys live here too: which keys type text, which move the caret, which
//! step scenes or hop panes, and which may keep repeating into a panel while
//! held.

use super::*;

pub(super) fn scene_step_for_key(
    key: &KeyEvent,
    capabilities: KeyboardCapabilities,
) -> Option<isize> {
    if key.kind != KeyEventKind::Press {
        return None;
    }
    if key.modifiers.is_empty() {
        return match key.code {
            KeyCode::F(6) => Some(-1),
            KeyCode::F(7) => Some(1),
            _ => None,
        };
    }
    if !capabilities.enhanced
        || !matches!(
            key.modifiers - KeyModifiers::SHIFT,
            KeyModifiers::CONTROL | KeyModifiers::SUPER
        )
    {
        return None;
    }
    match key.code {
        KeyCode::Char('[' | '{') => Some(-1),
        KeyCode::Char(']' | '}') => Some(1),
        _ => None,
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum SceneNavigation {
    FocusPane,
    Step(isize),
}

pub(super) fn scene_navigation_for_key(
    key: &KeyEvent,
    capabilities: KeyboardCapabilities,
    split: bool,
) -> Option<SceneNavigation> {
    let delta = scene_step_for_key(key, capabilities)?;
    // Enhanced protocols may deliver the shifted glyph without SHIFT.
    // Legacy Esc/control bytes never enter this path: `scene_step_for_key`
    // requires a distinct modified punctuation report.
    let plain_bracket = matches!(key.code, KeyCode::Char('[' | ']'))
        && !key.modifiers.contains(KeyModifiers::SHIFT);
    Some(if split && plain_bracket {
        SceneNavigation::FocusPane
    } else {
        SceneNavigation::Step(delta)
    })
}

/// Navigation keys whose operating-system repeat belongs to the focused
/// sheet or menu. Crossterm reports these as `Repeat` with enhanced keyboard
/// protocols (including iTerm2); routing only `Press` makes a held arrow look
/// stuck until it is released and pressed again.
fn panel_key_repeats(code: KeyCode) -> bool {
    matches!(
        code,
        KeyCode::Up
            | KeyCode::Down
            | KeyCode::Left
            | KeyCode::Right
            | KeyCode::PageUp
            | KeyCode::PageDown
            | KeyCode::Home
            | KeyCode::End
    )
}

/// A held horizontal key may move or set a control, but must not repeatedly
/// trigger a preview or generate new content. Reference leaves deliberately
/// keep Left/Right edge-triggered; vertical list navigation still repeats.
pub(super) fn panel_repeat_is_safe(kind: PanelKind, code: KeyCode) -> bool {
    panel_key_repeats(code)
        && !(kind == PanelKind::Reference && matches!(code, KeyCode::Left | KeyCode::Right))
}

/// Panels where a held j, k, h, or l stands in for an arrow once the panel
/// has declined the letter. Panels that bind the letters themselves, or
/// move controls with the arrows, keep their own reading.
/// Whether this key WALKS rather than types: what a column being read
/// beside the score still hands to the caret.
fn moves_the_caret(code: KeyCode) -> bool {
    matches!(
        code,
        KeyCode::Left
            | KeyCode::Right
            | KeyCode::Up
            | KeyCode::Down
            | KeyCode::Home
            | KeyCode::End
            | KeyCode::PageUp
            | KeyCode::PageDown
    )
}

/// A key that types or erases: held, it keeps typing or erasing where a
/// text field has the keyboard.
fn text_key(code: KeyCode) -> bool {
    matches!(
        code,
        KeyCode::Char(_) | KeyCode::Backspace | KeyCode::Delete
    )
}

/// A control character is never something to type. macOS's screenshot
/// shortcut leaves one behind in some terminals - an SOH from the key
/// report the overlay let through - and it must not go into the score.
fn is_stray_control_key(terminal_event: &Event) -> bool {
    if let Event::Key(key) = terminal_event
        && let KeyCode::Char(character) = key.code
        && super::super::editor::is_stray_control(character)
    {
        return true;
    }
    false
}

/// The same character inside a paste - a terminal that folds stray
/// bytes into one - is dropped from it, the rest of the paste kept.
/// `None` when nothing of it is left.
fn without_stray_controls(terminal_event: Event) -> Option<Event> {
    Some(match terminal_event {
        Event::Paste(text) if text.chars().any(super::super::editor::is_stray_control) => {
            let cleaned: String = text
                .chars()
                .filter(|character| !super::super::editor::is_stray_control(*character))
                .collect();
            if cleaned.is_empty() {
                return None;
            }
            Event::Paste(cleaned)
        }
        other => other,
    })
}

/// Update and stop, in every spelling the terminal might send them in.
///
/// The focused Vim editor claims its keys first. Otherwise, update and stop
/// are recognised before panels, pickers and settings receive the key.
///
/// The keybinding table rides along: a chord learnt onto update or stop
/// claims the press exactly as the built-in spelling would, so a moved
/// transport keeps the same priority. The table is asked for the two
/// transport actions alone - never for the whole table, whose other
/// chords belong to the surfaces they serve.
pub(super) fn transport_key(
    key: &KeyEvent,
    capabilities: KeyboardCapabilities,
    keybinds: &super::super::keybinds::Keybinds,
) -> Option<Command> {
    // A learnt transport chord is claimed before the built-in reading of
    // the same key: whatever update used to mean, it means update again.
    // Alt never reaches a binding - option is how a Mac types characters
    // - so with option held the learnt chord is not the chord, and the
    // press is not claimed here for the `bind_action_for` arm in
    // `transport_press` to refuse.
    let alt = key.modifiers.contains(KeyModifiers::ALT);
    for candidate in [
        super::super::keybinds::BindAction::Evaluate,
        super::super::keybinds::BindAction::Stop,
    ] {
        if let Some(combo) = keybinds.override_for(candidate)
            && !alt
            && combo.matches(key)
        {
            return Some(match candidate {
                super::super::keybinds::BindAction::Evaluate => Command::Evaluate,
                _ => Command::Stop,
            });
        }
    }
    // The function keys are the portable spelling and carry no modifier of
    // their own; the chords are Ctrl+S / Ctrl+G, plus Ctrl+Enter and Ctrl+.
    // where the terminal reports them apart from plain Enter and a full stop.
    // The key a chord was moved off is freed rather than re-meaning: a
    // built-in transport spelling whose action now answers elsewhere
    // stands down here, so the moved chord and the key it left do not
    // both work. A press the table has not rearranged reaches the
    // built-in spellings as always.
    if keybinds.overrules(key) {
        return None;
    }
    match key.code {
        KeyCode::F(5) if key.modifiers.is_empty() => return Some(Command::Evaluate),
        KeyCode::F(8) if key.modifiers.is_empty() => return Some(Command::Stop),
        _ => {}
    }
    match key_to_command(*key, capabilities) {
        Some(command @ (Command::Evaluate | Command::Stop)) => Some(command),
        _ => None,
    }
}

impl App {
    /// Only control rows accept repeated horizontal keys.
    fn generator_control_repeats(&self, kind: PanelKind, code: KeyCode) -> bool {
        #[cfg(feature = "hydra")]
        return kind == PanelKind::Reference
            && matches!(code, KeyCode::Left | KeyCode::Right)
            && self.reference_panel.as_ref().is_some_and(|panel| {
                matches!(
                    panel.generator_row(),
                    Some(super::super::ideas::Row::Control(_))
                )
            });
        #[cfg(not(feature = "hydra"))]
        {
            let _ = (kind, code);
            false
        }
    }

    /// Whether the panel has a text field that owns held typing keys.
    fn panel_is_typing(&self, kind: PanelKind) -> bool {
        // The set prompt types whether the vim surface below it answers
        // or not.
        if kind == PanelKind::Set && self.set_prompt.is_some() {
            return true;
        }
        if let Some(typing) = super::vim::surface_typing(self) {
            return typing;
        }
        match kind {
            PanelKind::Set => self.set_search_typing(),
            PanelKind::Viz => self.viz_docks[self.viz_focus]
                .as_ref()
                .is_some_and(|panel| panel.prompt.is_some()),
            PanelKind::ThemeEditor => self
                .theme_editor
                .as_ref()
                .is_some_and(|editor| editor.entry.is_some()),
            // The column types where it has a search box. The examples tab
            // has none: letters act once to copy or switch the code format.
            PanelKind::Reference => self
                .reference_panel
                .as_ref()
                .is_some_and(|panel| panel.wants_text()),
            // The column's own sheets: the add list searches, and the form
            // types its numbers.
            PanelKind::Knobs => self
                .knobs_panel
                .as_ref()
                .is_some_and(|panel| panel.adding.is_some() || panel.editing.is_some()),
            _ => false,
        }
    }

    /// Whether this panel is being read beside the score rather than
    /// answered, so a movement key it declines still belongs to the caret.
    ///
    /// Movement only. Which letters belong to the score is a different
    /// question with a different answer - `panel_types_through`, which is
    /// yes for an entry's page alone.
    ///
    /// The reference column with no anchor is the one case: nothing in the
    /// text is waiting for a name, so the column is a thing to read while
    /// the score is worked on, and the arrows go on walking the caret. An
    /// anchored column has a place in the text to protect and keeps them.
    fn panel_reads_beside_score(&self, kind: PanelKind) -> bool {
        kind == PanelKind::Reference && self.reference_anchor.is_none()
    }

    /// The one surface where a letter the panel had no use for is the
    /// score's after all: the reference open on an entry's page.
    ///
    /// A reader types what the entry teaches, and an entry page has no
    /// search box to take the letter. Every other panel keeps the keyboard
    /// it is given: a letter over a visuals dock or a settings sheet must
    /// not appear in the score. That includes the examples shelf in the same
    /// column, which has no search box either.
    fn panel_types_through(&self, kind: PanelKind) -> bool {
        kind == PanelKind::Reference
            && self
                .reference_panel
                .as_ref()
                .is_some_and(super::super::reference::ReferencePanel::types_through)
    }

    /// The focused panel's key handler, and no other's.
    pub(super) fn dispatch_panel_key(
        &mut self,
        kind: PanelKind,
        code: KeyCode,
        primary: bool,
        shift: bool,
        alt: bool,
    ) -> Result<bool, RuntimeError> {
        Ok(match kind {
            PanelKind::Reference
                if primary && shift && !alt && matches!(code, KeyCode::Char('r' | 'R')) =>
            {
                // The global recording chord must not become the sample
                // browser's unshifted bank-rename command.
                false
            }
            PanelKind::Reference => {
                self.handle_reference_key_with_modifiers(code, primary, shift, alt)?
            }
            PanelKind::Devices => self.handle_panel_key(code, primary)?,
            PanelKind::Set if self.regions.sidebar_hidden && self.set_prompt.is_none() => false,
            PanelKind::Set if alt && !primary && self.set_prompt.is_none() => {
                self.handle_set_file_key(code)
            }
            // A chord is not text: while the search box types, ^B, ^O and
            // the rest go on to the studio's chords. Ctrl with Alt is how
            // a legacy terminal sends AltGr, and AltGr still types.
            PanelKind::Set if primary && !alt && self.set_search_typing() => false,
            PanelKind::Set
                if code == KeyCode::Char('n')
                    && !primary
                    && !shift
                    && !alt
                    && self.set_prompt.is_none()
                    // `n` is a letter the search box may be typing.
                    && !self.set_search_typing() =>
            {
                self.new_session();
                true
            }
            PanelKind::Set => self.handle_set_key(code, primary, shift),
            PanelKind::Viz => {
                // A dock the layout found no room for is not on screen, so
                // it must not take the key either. Swallowing one -
                // Backspace above all - from a score the musician can still
                // see and is still editing leaves a dead keyboard with
                // nothing on screen to explain it. This catches a dock
                // resized past the room there is, one opened on a screen
                // too small for it, and a terminal shrunk after the fact -
                // except before the next draw has laid the dock out, when
                // the empty region is not yet the truth (`viz_unlaid`).
                if self
                    .viz_unlaid
                    .get(self.viz_focus)
                    .copied()
                    .unwrap_or(false)
                {
                    return Ok(self.handle_viz_key(code, primary, shift));
                }
                let shown = self
                    .regions
                    .viz
                    .get(self.viz_focus)
                    .is_some_and(|region| !region.is_empty());
                if shown {
                    self.handle_viz_key(code, primary, shift)
                } else {
                    false
                }
            }
            PanelKind::Log => self.handle_log_key(code, primary, shift),
            PanelKind::Jobs => self.handle_jobs_key(code, primary, shift),
            PanelKind::Export => self.handle_export_key(code, primary, shift),
            PanelKind::Theme => self.handle_theme_key(code, primary),
            PanelKind::ThemeEditor => self.handle_theme_editor_key(code, primary, shift, alt),
            PanelKind::Settings if alt && !primary && matches!(code, KeyCode::Char('o' | 'O')) => {
                self.reveal_selected_sample_source_folder()
            }
            PanelKind::Settings => self.handle_settings_key(code, primary, shift),
            // A desk the layout found no room for is not on screen, and
            // must not take the key either - the same as a dock.
            PanelKind::Mixer => {
                // From opening (or a resize) until the next draw, the
                // region is not yet the truth about the room the desk
                // got - the same `viz_unlaid` bargain: the desk keeps the
                // keyboard while the status claims it, rather than
                // falling through to the score behind an empty region.
                if self.mixer_unlaid {
                    return Ok(self.handle_mixer_panel_key(code, primary, shift));
                }
                if self.regions.mixer.is_empty() {
                    // Esc still gives the score the keyboard back.
                    if code == KeyCode::Esc {
                        self.focus = Focus::Editor;
                        self.dirty_frame = true;
                    }
                    code == KeyCode::Esc
                } else {
                    self.handle_mixer_panel_key(code, primary, shift)
                }
            }
            // The breakdown has the keyboard while it is on screen or
            // waiting for its first layout. If no room remains, Esc can
            // still return focus to the score.
            PanelKind::Memory => {
                if !self.memory_unlaid && self.regions.memory.is_empty() {
                    if code == KeyCode::Esc {
                        self.focus = Focus::Editor;
                        self.dirty_frame = true;
                    }
                    code == KeyCode::Esc
                } else {
                    self.handle_memory_key(code, primary)
                }
            }
            // The knobs column keeps the keyboard the same way: on screen
            // or waiting for its first layout, and Esc hands it back to
            // the score when the layout found it no room.
            PanelKind::Knobs => {
                if !self.knobs_unlaid && self.regions.knobs.is_empty() {
                    if code == KeyCode::Esc {
                        self.focus = Focus::Editor;
                        self.dirty_frame = true;
                    }
                    code == KeyCode::Esc
                } else {
                    self.handle_knobs_key(code, primary, shift, alt)
                }
            }
        })
    }

    /// Every terminal event, offered to its owners in priority order. The
    /// order is the behaviour - which surface is on top, which key wins -
    /// so each step sees the event only when every step above it has let
    /// it go, and the first to claim it ends the turn.
    pub(super) fn handle_terminal_event(
        &mut self,
        terminal_event: Event,
    ) -> Result<(), RuntimeError> {
        let terminal_event = self.option_panel_spelling(terminal_event);
        let terminal_event = composed_text_event(terminal_event);
        let result = self.route_terminal_event(terminal_event, true);
        self.refresh_editor_shortcut_hints();
        result
    }

    /// Whether the text a terminal composed for an Option key spells the
    /// Alt chord over typing. Only an exotic one-character composition
    /// does: the US Option+2, ™, is no character anyone types. A plain
    /// ASCII one steps aside, because on a Swedish layout Option+2 is the
    /// one way to type @, and a longer composition pastes.
    fn composition_spells_the_chord(text: &str) -> bool {
        sole_composed_character(text).is_some_and(|character| !character.is_ascii())
    }

    /// Recover an Option panel chord from the composed character a terminal
    /// sends. Only where the focused surface answers to that Option letter,
    /// so the same character types normally everywhere else. Alt+., the
    /// browser spelling of stop, answers on every surface.
    ///
    /// A terminal that carries the keyboard protocol reports Alt itself, in
    /// the key the composition rides, so no terminal table is needed for
    /// that spelling: the protocol is the fact. A legacy terminal sends the
    /// composed character alone, and the table below reads it.
    fn option_panel_spelling(&self, event: Event) -> Event {
        match &event {
            Event::KeyWithText { key, text }
                if key.modifiers == KeyModifiers::ALT
                    && let KeyCode::Char(letter) = key.code =>
            {
                let letter = letter.to_ascii_lowercase();
                let chord = self.alt_panel_action_applies(letter)
                    || self.keybinds.browser_spelling(key).is_some();
                if chord && Self::composition_spells_the_chord(text) {
                    return Event::Key(KeyEvent::new_with_kind_and_state(
                        KeyCode::Char(letter),
                        KeyModifiers::ALT,
                        key.kind,
                        key.state,
                    ));
                }
            }
            Event::Key(key) if key.modifiers.is_empty() => {
                if let KeyCode::Char(character) = key.code
                    && let Some(letter) = self.keybinds.option_letter_for(character)
                    && self.alt_panel_action_applies(letter)
                {
                    return Event::Key(KeyEvent::new_with_kind_and_state(
                        KeyCode::Char(letter),
                        KeyModifiers::ALT,
                        key.kind,
                        key.state,
                    ));
                }
            }
            _ => {}
        }
        event
    }

    /// Whether the focused surface has a use for an Option letter. A chord
    /// learnt over the action retires the letter, so it is not recovered.
    /// An Option digit is the strip's own: it names a score on the strip.
    fn alt_panel_action_applies(&self, letter: char) -> bool {
        if letter.is_ascii_digit() {
            return self.strip_digit_applies(letter);
        }
        if let Some(action) = self.panel_action_at(letter) {
            return !self.keybinds.overridden(action);
        }
        letter == 'a'
            && self.focus == Focus::Panel(PanelKind::Reference)
            && self
                .reference_panel
                .as_ref()
                .is_some_and(|panel| crate::reference::tab_previews(panel.tab))
    }

    /// Whether an Alt digit picks a strip's score right now: the score has
    /// the keyboard, and nothing over it is reading the digit. Alt is not a
    /// chord the keybind table can hold, so no learnt key can retire it.
    fn strip_digit_applies(&self, digit: char) -> bool {
        matches!(digit, '1'..='9')
            && self.focus == Focus::Editor
            && !self.editor().find_open()
            && self.smart_action.is_none()
            && matches!(self.strip_mode, SceneStripMode::Idle)
    }

    pub(super) fn route_terminal_event(
        &mut self,
        mut terminal_event: Event,
        allow_modal_keys: bool,
    ) -> Result<(), RuntimeError> {
        if let Event::Key(key) = &mut terminal_event {
            self.keybinds.normalize_terminal_key(key);
        }
        self.sync_editor_keybindings();
        self.latch_keyboard_capabilities(&terminal_event);
        if is_stray_control_key(&terminal_event) {
            return Ok(());
        }
        self.cancel_sample_delete_for_event(&terminal_event);
        if matches!(&terminal_event,
            Event::Key(key) if key.kind != KeyEventKind::Release)
            || matches!(&terminal_event, Event::Paste(_) | Event::FocusGained)
            || matches!(&terminal_event, Event::Mouse(mouse)
                if mouse.kind != MouseEventKind::Moved)
        {
            self.caret_activity = Instant::now();
            self.dirty_frame |= !self.caret_visible;
            self.caret_visible = true;
        }
        self.note_pointer_shape_event(&terminal_event);
        // A find prompt owns the keys before Vim mappings and every editor
        // command: a typed pattern is never a motion or an edit.
        if allow_modal_keys && self.route_find_event(&terminal_event)? {
            return Ok(());
        }
        // Quit and the transport outrank even Vim, so they must not be
        // handed to the prompt's Vim branch.
        let find_global = matches!(&terminal_event, Event::Key(key)
            if self.editor_accepts_raw_paste()
                && self.editor().find_open()
                && self.find_global_chord(key));
        let quit_was_armed = self.quit_armed;
        if !find_global
            && allow_modal_keys
            && self.editor_accepts_raw_paste()
            && let Event::Key(key) = &terminal_event
            && super::vim::dispatch(self, *key)?
        {
            // A mapping that calls Quit just armed it; any other key is
            // the "another key calls it off" press.
            if quit_was_armed && self.quit_armed && key.kind == KeyEventKind::Press {
                self.quit_armed = false;
                self.status = "quit cancelled".into();
                self.dirty_frame = true;
            }
            return Ok(());
        }
        let non_key_interrupt = matches!(&terminal_event, Event::Paste(_) | Event::FocusLost)
            || matches!(&terminal_event, Event::Mouse(mouse) if mouse.kind != MouseEventKind::Moved);
        if non_key_interrupt {
            super::vim::flush_pending(self)?;
        }
        let interrupts_key_mode = non_key_interrupt
            || matches!(&terminal_event, Event::Key(key)
                if key.kind != KeyEventKind::Release
                    && (key.modifiers.intersects(
                        KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SUPER
                            | KeyModifiers::HYPER | KeyModifiers::META,
                    ) || matches!(key.code, KeyCode::F(_) | KeyCode::Tab | KeyCode::BackTab)
                        || self.keybinds.override_action_for(key).is_some()));
        if interrupts_key_mode {
            for scene in self.scenes.scenes_mut() {
                scene.editor.interrupt_key_mode();
            }
        }
        // A Vim mapping has first use of the focused editor's keys.
        // Otherwise quit still precedes every modal surface.
        if self.quit_key(&terminal_event) {
            return Ok(());
        }
        // Cancellation is global for the same reason: a keybind learner must
        // not capture another chord and leave an invisible quit armed behind
        // it. A rebound Quit chord remains a valid confirmation.
        self.cancel_armed_quit(&terminal_event);
        #[cfg(feature = "remote-control")]
        if self.remote_panel_event(&terminal_event) {
            return Ok(());
        }
        if self.capture_keybind_chord(&terminal_event) {
            return Ok(());
        }
        if self.handle_piano_event(&terminal_event)? {
            return Ok(());
        }
        self.settle_mapping_learn();
        if let Event::Resize(width, height) = terminal_event {
            self.handle_resize(width, height);
            return Ok(());
        }
        if matches!(terminal_event, Event::FocusLost) {
            self.stop_precision_drag();
            self.stop_replay_drag();
        }
        let Some(terminal_event) = without_stray_controls(terminal_event) else {
            return Ok(());
        };
        if let Event::Key(key) = &terminal_event
            && key.kind == KeyEventKind::Press
        {
            // A chord learnt for a panel action stops here even when the
            // panel has no use for the press.
            match self.panel_spelling(key) {
                None => return Ok(()),
                Some(spelling) if self.route_key_press(&spelling)? || spelling != *key => {
                    return Ok(());
                }
                Some(_) => {}
            }
        }

        // `route_key_press` is offered presses alone, so a held key and a
        // paste never reach the menu there: an open menu is offered them
        // here. Holding an arrow over an open dropdown would otherwise walk
        // the reference list underneath it, and a paste would land in the
        // score.
        if self.menu_event(&terminal_event)? {
            return Ok(());
        }
        // Enhanced keyboard protocols report held navigation as Repeat;
        // it, the mouse and a paste never reach `route_key_press`. While
        // help is open all of them belong to the overlay, including clicks
        // that would otherwise change panel focus or move the score caret.
        if self.help_event(&terminal_event) {
            return Ok(());
        }

        if self.replay_edit_event(&terminal_event) {
            return Ok(());
        }
        if self.smart_action_event(&terminal_event) {
            return Ok(());
        }
        if self.precision_slider_event(&terminal_event) {
            return Ok(());
        }
        if self.timeline_held_enter(&terminal_event) {
            return Ok(());
        }

        // `route_key_press` is intentionally Press-only: holding a chord
        // must not repeatedly open and close its sheet. A focused menu's
        // navigation is different - held arrows are an ordinary way to walk
        // a list or an opacity slider - so `route_key_repeat` offers Repeat
        // records to the same panel handler. Release records remain inert.
        if let Event::Key(key) = &terminal_event
            && key.kind == KeyEventKind::Repeat
            && self.route_key_repeat(key)?
        {
            return Ok(());
        }

        if let Event::Paste(text) = &terminal_event
            && self.route_paste(text)
        {
            return Ok(());
        }
        self.hand_to_editor(terminal_event)
    }

    /// A resized window: drags end, the cell's pixels and the picture
    /// budget are measured again, every picture is sent afresh, and the
    /// new size is the frame at once.
    fn handle_resize(&mut self, width: u16, height: u16) {
        self.stop_precision_drag();
        self.stop_replay_drag();
        // A resize can be a font change too: the pixel size of a cell
        // is read again, and the tier follows.
        self.refresh_cell_pixels(cell_pixels_now());
        // A window half the size is a different measurement, and one
        // twice the size certainly is: what the pictures may cost is
        // found again rather than inherited from the old shape.
        self.pixel_budget_said = false;
        self.set_pixel_budget(super::super::graphics::PIXEL_CEILING);
        // A resized window is a reset window as far as the pictures on
        // it go: send them all again rather than place ones the
        // terminal may have dropped with the old geometry.
        super::super::graphics::forget_sent_images();
        // The captured rail belongs to the old layout. End the gesture
        // before a resize can move it out from under the pointer.
        if matches!(self.pointer, Some(Pointer::Slider { .. })) {
            self.pointer = None;
        }
        // The next draw will confirm the terminal's area, but privacy
        // decisions cannot wait for it: a sheet that just became too
        // small to draw is no longer a visible camera consent surface.
        self.frame = Rect::new(0, 0, width, height);
        #[cfg(feature = "hydra")]
        self.sync_settings_webcam_preview();
        self.invalidate_maps();
        self.dirty_frame = true;
    }

    /// What the keys that actually arrive prove about this terminal,
    /// latched for the surfaces that advertise chords.
    fn latch_keyboard_capabilities(&mut self, terminal_event: &Event) {
        // A Command chord that actually arrived is the only proof that this
        // terminal delivers ⌘ at all. The protocol handshake says the terminal
        // speaks the protocol, never that macOS let the key through: iTerm2
        // answers it and still eats every ⌘ at the menu layer. Latched here so
        // Settings ▸ About can say so honestly; the footer stays caret-spelled
        // either way, because the chords worth advertising are the ones a Mac
        // terminal has already claimed for itself.
        if let Event::Key(key) = terminal_event
            && key.modifiers.contains(KeyModifiers::SUPER)
            && !self.capabilities.super_seen
        {
            self.capabilities.super_seen = true;
        }
        // And the same for `^Space`, for the same reason. macOS puts "Select
        // the previous input source" on it and enables it by default, so on a
        // stock Mac it never reaches the terminal - and nothing the terminal
        // reports up front says whether it will. The menus, help and Settings
        // name `^Space` beside `^F` only where the terminal profile lets it
        // through, never on macOS. `Null` is what a legacy terminal sends for
        // the chord.
        if let Event::Key(key) = terminal_event
            && !self.capabilities.space_seen
            && (key.code == KeyCode::Null
                || (key.code == KeyCode::Char(' ')
                    && key
                        .modifiers
                        .intersects(KeyModifiers::CONTROL | KeyModifiers::SUPER)))
        {
            self.capabilities.space_seen = true;
        }
    }

    /// A key PRESS, offered from the most global owner down: error
    /// navigation and the transport, the menu and help, the editors that
    /// float over the score, the scene strip, the focused panel, and last
    /// the studio's own chords. `true` when one of them took it; a press
    /// nobody took goes on to the score.
    fn route_key_press(&mut self, key: &KeyEvent) -> Result<bool, RuntimeError> {
        let primary = key
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::SUPER);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        let shift = key.modifiers.contains(KeyModifiers::SHIFT)
            || matches!(key.code, KeyCode::Char(character) if character.is_uppercase());
        let letter = match key.code {
            KeyCode::Char(character) => Some(character.to_ascii_lowercase()),
            _ => None,
        };
        // Error navigation escapes every surface. Keybind capture still
        // owns the key while learning, including this very chord:
        // `capture_keybind_chord` has the press before it is routed here.
        if self.keybinds.action_for(key) == Some(BindAction::FirstError) && !alt {
            self.jump_to_first_error();
            return Ok(true);
        }
        if self.transport_press(key, alt)? {
            return Ok(true);
        }
        // The effective menu/help chord toggles its surface before either
        // modal layer can swallow it. If MenuBar has moved, F1 may now
        // belong only to Help; always respect the resolved owner.
        if self.menu_or_help_chord(key, alt)? {
            return Ok(true);
        }
        self.settle_menu();
        // A menu that has the keyboard outranks help, a rename in
        // progress and every panel; only the transport, claimed by
        // `transport_press`, outranks it.
        //
        // It must run ahead of `handle_strip_key`. A rename swallows every
        // unmodified Char, and the menu's mnemonics are unmodified Chars -
        // placed after it, choosing "New scene" would type "n" into the
        // scene name instead.
        if self.menu_press(key)? {
            return Ok(true);
        }
        if self.help_press(key) {
            return Ok(true);
        }
        if self.retired_editor_binding(key) || self.retired_panel_toggle(key) {
            return Ok(true);
        }
        // A terminal replacement keeps the action's normal context:
        // clipboard shortcuts belong to an active text field, and a
        // scene rename may swallow navigation without losing its draft.
        let (adapted, contextual) = self.contextual_key(key, alt);
        let changes_scene = !alt
            && matches!(
                self.keybinds.action_for(key),
                Some(
                    BindAction::NewScene
                        | BindAction::DuplicateScene
                        | BindAction::CloseScene
                        | BindAction::PreviousScene
                        | BindAction::NextScene
                        | BindAction::HopPane
                        | BindAction::Split
                )
            );
        if !changes_scene && self.smart_action_key(&contextual) {
            return Ok(true);
        }
        if self.prompt_copy_shortcut(key, alt) || self.paste_shortcut(key, alt) {
            return Ok(true);
        }
        if self.replay_edit_key(&contextual) {
            return Ok(true);
        }
        if self.precision_slider_key(&contextual) {
            return Ok(true);
        }
        if self.settings_dialog_chord(key, alt)? {
            return Ok(true);
        }
        // Learned editing commands keep the focused field's meaning,
        // just like automatically adapted terminal defaults. A custom
        // Copy key in the log must copy the log, never the hidden score.
        if let Some(action) = self.keybinds.override_action_for(key)
            && !Self::contextual_editor_binding(action)
            && self.dispatch_bind_action(action, alt)?
        {
            return Ok(true);
        }
        let context_primary = contextual
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::SUPER);
        let context_shift = contextual.modifiers.contains(KeyModifiers::SHIFT)
            || matches!(contextual.code, KeyCode::Char(character) if character.is_uppercase());
        if self.handle_strip_key(contextual.code, context_primary) {
            return Ok(true);
        }
        self.settle_focus();
        // A slider the pointer touched stays on the arrows until it is
        // let go, and comes before the panels: the Esc the status
        // promises lets the control go rather than closing whatever is
        // open beside it.
        if self.armed_slider_key(key) {
            return Ok(true);
        }
        // A panel you are not in is a thing on the screen, and Esc puts
        // it away - the front-most first, outright, no ladder.
        if key.code == KeyCode::Esc
            && self.focus == Focus::Editor
            && let Some(kind) = self.front_panel()
        {
            self.close_panel(kind);
            return Ok(true);
        }
        match super::vim::handle_text_key(self, key) {
            super::vim::VimTextKey::Ignored => {}
            super::vim::VimTextKey::Consumed => return Ok(true),
            super::vim::VimTextKey::Arrow(kind, code) => {
                let _ = self.dispatch_panel_key(kind, code, false, false, false)?;
                return Ok(true);
            }
        }
        if self.focused_panel_key(
            key,
            &contextual,
            context_primary,
            context_shift,
            primary,
            alt,
        )? {
            return Ok(true);
        }
        if self.focus == Focus::Timeline
            && !primary
            && !alt
            && self.handle_timeline_key(key.code)?
        {
            return Ok(true);
        }
        if key.code == KeyCode::Enter
            && alt
            && !primary
            && self.focus == Focus::Editor
            && self.open_precision_slider_at_caret()
        {
            return Ok(true);
        }
        // Contextual owners have had the same chance they get for the
        // preferred spelling. Any unclaimed adapted binding now acts globally.
        if let Some(action) = adapted
            && self.dispatch_bind_action(action, alt)?
        {
            return Ok(true);
        }
        if self.surface_chord(key, primary, shift, alt, letter) {
            return Ok(true);
        }
        if self.performance_chord(key, primary, shift, alt, letter) {
            return Ok(true);
        }
        Ok(self.control_chord(key, primary, shift, alt, letter))
    }

    /// Update and stop, and ^⇧S's rewinding update, in every spelling:
    /// learnt, built-in, and the browser spelling from
    /// `Keybinds::browser_spelling`. `true` when one of them answered.
    fn transport_press(&mut self, key: &KeyEvent, alt: bool) -> Result<bool, RuntimeError> {
        // ^⇧S is ^S with a rewind: the score plays from its own
        // beginning rather than joining the cycle already running.
        // Claimed before the transport itself, which reads `s` with or
        // without shift as an ordinary update.
        //
        // A chord learnt onto update or rewind-update answers here as
        // its default would - and the key it was moved off stops
        // meaning it, here as everywhere else the table is asked. The
        // table's untouched defaults are not asked here: they belong
        // to the steps `route_key_press` takes after this one, which
        // know the context a table cannot hold - the split's brackets
        // hop a pane, a prompt keeps ^A for its field, a theme editor
        // keeps ^N for its draft.
        let rewind_bound = self.keybinds.binding(BindAction::RewindEvaluate);
        if rewind_bound.is_some_and(|combo| combo.matches(key)) && !alt {
            self.next_rewind = true;
            self.dispatch_editor(Command::Evaluate)?;
            return Ok(true);
        }
        if let Some(action) = self.bind_action_for(key)
            && matches!(
                action,
                BindAction::Evaluate | BindAction::Stop | BindAction::RewindEvaluate
            )
            // As at the scene rename in `performance_chord`: a press
            // the table will not take (alt held - option is how a Mac
            // types) falls through rather than being consumed for
            // nothing.
            && self.dispatch_bind_action(action, alt)?
        {
            return Ok(true);
        }
        // The transport is global, and is claimed before anything else
        // on screen gets a look in: playing while reading is the point.
        if let Some(command) = transport_key(key, self.capabilities, &self.keybinds) {
            self.dispatch_editor(command)?;
            return Ok(true);
        }
        // Alt+Enter and Alt+. are the browser spellings of update and stop.
        // Alt+Enter on a live slider keeps opening its precision control:
        // the caret on a fader is asking for the fader.
        if let Some(action) = self.keybinds.browser_spelling(key)
            && !(action == BindAction::Evaluate && self.caret_on_live_slider())
            && self.dispatch_bind_action(action, false)?
        {
            return Ok(true);
        }
        Ok(false)
    }

    /// The press as the owners that read keys by context see it: a
    /// terminal replacement arrives spelled as its action's default chord,
    /// alt kept. Also the adapted action itself, which acts globally once
    /// those owners have had their chance.
    fn contextual_key(&self, key: &KeyEvent, alt: bool) -> (Option<BindAction>, KeyEvent) {
        let adapted = if alt {
            None
        } else {
            self.keybinds.adapted_action_for(key).filter(|action| {
                !self.keybinds.overridden(*action) || Self::contextual_editor_binding(*action)
            })
        };
        let contextual = adapted.map_or(*key, |action| {
            let combo = action.default_binding();
            let mut modifiers = KeyModifiers::NONE;
            if combo.control {
                modifiers |= KeyModifiers::CONTROL;
            }
            if combo.shift {
                modifiers |= KeyModifiers::SHIFT;
            }
            if alt {
                modifiers |= KeyModifiers::ALT;
            }
            KeyEvent::new(combo.code, modifiers)
        });
        (adapted, contextual)
    }

    /// The arrow a plain j, k, h, or l means in a list while Vim editor
    /// keys and list navigation are on. Surfaces try the letter first, so
    /// their own commands and menu mnemonics win.
    /// The focused panel's turn. It takes what it has a use for, and what
    /// it declines is swallowed and said out loud rather than typed into a
    /// score the eye is not on. `true` when the key stops here.
    fn focused_panel_key(
        &mut self,
        key: &KeyEvent,
        contextual: &KeyEvent,
        context_primary: bool,
        context_shift: bool,
        primary: bool,
        alt: bool,
    ) -> Result<bool, RuntimeError> {
        // Only the focused panel is offered the key, and it takes only
        // what it has a use for; the rest falls through to the score.
        // A panel being open is not enough - that is what lets the
        // reference stay readable while the score is typed into.
        if let Focus::Panel(kind) = self.focus {
            if self.dispatch_panel_key(
                kind,
                contextual.code,
                context_primary,
                context_shift,
                alt,
            )? {
                return Ok(true);
            }
            // A panel with the keyboard has the keyboard. What it has no
            // use for is dropped, never handed to the score: a plain `a`
            // pressed over a visuals dock must not appear in the music,
            // where the caret is not even on screen, and a bare Tab must
            // not indent it.
            // Chords stay global - ^S has to update from anywhere, and
            // the function keys reach their panels or the chord tables
            // later in `route_key_press` - but every other key that would
            // otherwise reach the score is swallowed here rather than
            // typed or moved into a document the eye is not on.
            //
            // Said out loud rather than swallowed in silence: a key that
            // does nothing and explains nothing is indistinguishable
            // from a hung studio.
            //
            // Two keys are never the score's from here, whatever the
            // panel would otherwise pass on: Tab and Shift+Tab, which
            // indent. Everything else a reading panel declines can
            // still reach the caret - see `panel_reads_beside_score`.
            let indents = matches!(key.code, KeyCode::Tab | KeyCode::BackTab);
            let passes = self.panel_types_through(kind)
                || (self.panel_reads_beside_score(kind) && moves_the_caret(key.code));
            // A plain j, k, h, or l the panel had no use for is the arrow
            // it stands in for. The four letters navigate even where the
            // panel otherwise passes letters to the score: with Vim list
            // keys on they are list keys, not typing. An arrow with no
            // target still belongs to the panel: letting it through would
            // move a caret the reader cannot see.
            if super::vim::panel_takes_list_navigation(kind)
                && let Some(alias) = self.list_navigation_alias(key)
            {
                let _ = self.dispatch_panel_key(kind, alias.code, false, false, false)?;
                return Ok(true);
            }
            if !primary
                && !alt
                // `Null` is what a legacy terminal sends for Ctrl+Space,
                // which is a chord - the reference's oldest spelling -
                // and `surface_chord` is where it is read.
                && key.code != KeyCode::Null
                && !matches!(key.code, KeyCode::F(_))
                && (indents || !passes)
            {
                self.status = format!(
                    "the {} has the keyboard - Esc gives it back to the score",
                    kind.label()
                );
                self.dirty_frame = true;
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// The studio's own chords for what is on screen: the panels and
    /// sheets, the split, wrap and zen, and the quick action. `true` when
    /// one answered.
    fn surface_chord(
        &mut self,
        key: &KeyEvent,
        primary: bool,
        shift: bool,
        alt: bool,
        letter: Option<char>,
    ) -> bool {
        // An Alt digit selects that strip's score, the way the digit on
        // its row in the set panel does, from wherever the score has the
        // keyboard. The strip runs to nine; past that, the arrows.
        if alt
            && !primary
            && !shift
            && let KeyCode::Char(digit) = key.code
            && self.strip_digit_applies(digit)
        {
            self.select_strip_by_number((digit as u8 - b'0') as usize);
            return true;
        }
        // Ctrl with Shift spells the same selection where Option types
        // real characters, as the Swedish layout's Option row does for
        // five of the nine digits: Ctrl composes nothing on any layout,
        // so this spelling holds on every one of them. Unlike the Alt
        // digit, a chord the keybind table learns over the key retires
        // it, the way every other primary chord retires.
        if primary
            && shift
            && !alt
            && let KeyCode::Char(digit) = key.code
            && self.strip_digit_applies(digit)
            && !self.key_is_a_learnt_chord(key)
        {
            self.select_strip_by_number((digit as u8 - b'0') as usize);
            return true;
        }
        if primary && !shift && letter == Some('d') && !self.key_is_a_learnt_chord(key) {
            self.toggle_reference_at_caret();
            return true;
        }
        if primary && shift && letter == Some('d') && !self.key_is_a_learnt_chord(key) {
            self.toggle_log_panel();
            return true;
        }
        if primary
            && shift
            && letter == Some('m')
            && !self.key_is_a_learnt_chord(key)
            && !self.settings_hold_the_keys()
        {
            self.toggle_mixer_panel();
            return true;
        }
        // The knobs column: K for knobs, beside ^K's zen, where the
        // terminal tells ^⇧K from ^K.
        if primary
            && shift
            && letter == Some('k')
            && !self.key_is_a_learnt_chord(key)
            && !self.settings_hold_the_keys()
        {
            self.toggle_knobs_panel();
            return true;
        }
        if primary && shift && letter == Some('x') && !self.key_is_a_learnt_chord(key) {
            self.toggle_export_sheet();
            return true;
        }
        if primary && shift && letter == Some('o') && !self.key_is_a_learnt_chord(key) {
            self.open_set_prompt(SetPrompt::OpenSet);
            return true;
        }
        if primary && !shift && letter == Some('t') && !self.key_is_a_learnt_chord(key) {
            self.theme_picker_chord();
            return true;
        }
        // ⇧F1 and ⇧F2 show a visuals dock or hide it; ⇧F9 makes the log
        // stay on screen; ⇧F10 walks the keyboard round the panels. The
        // table only carries chords somebody has MOVED, so a default
        // answers from here. Settings blocks the show/hide commands,
        // while ⇧F10 still lands on and outlines the visible sheet.
        if !self.key_is_a_learnt_chord(key)
            && key.modifiers == KeyModifiers::SHIFT
            && (!self.settings_hold_the_keys() || key.code == KeyCode::F(10))
        {
            match key.code {
                KeyCode::F(1) => {
                    self.toggle_viz_dock(0);
                    return true;
                }
                KeyCode::F(2) => {
                    self.toggle_viz_dock(1);
                    return true;
                }
                KeyCode::F(9) => {
                    self.toggle_log_sticky();
                    return true;
                }
                KeyCode::F(10) => {
                    self.rotate_panel_focus();
                    return true;
                }
                _ => {}
            }
        }
        // The set panel, as an editor's file tree: shown and hidden by
        // the one chord.
        if primary
            && letter == Some('b')
            && !shift
            && !self.key_is_a_learnt_chord(key)
            && !self.settings_hold_the_keys()
        {
            self.toggle_set_panel();
            return true;
        }
        // Settings answers to two chords. Ctrl+O is the one the menu
        // advertises, because a terminal emulator is free to keep
        // Ctrl+Shift+P for itself - Windows Terminal binds it to its own
        // command palette, and the chord never reaches this loop there.
        if primary && letter == Some('o') && !shift && !self.key_is_a_learnt_chord(key) {
            self.toggle_settings_sheet();
            return true;
        }
        if self
            .keybinds
            .accepts_legacy_alias(BindAction::Settings, key)
            && !alt
        {
            self.toggle_settings_sheet();
            return true;
        }
        // Three spellings, because the advertised one cannot be relied
        // on. `Null` is what a legacy terminal sends for Ctrl+Space; on
        // macOS nothing sends it at all, since "Select the previous
        // input source" holds that chord system-wide and is on out of
        // the box - iTerm2 and Ghostty alike never see it.
        //
        // F2 covers that, but only for a Mac set to send real function
        // keys; the default sends brightness instead, and F2 never
        // arrives either. Ctrl+F is the one with nothing in its way: a
        // plain ASCII 0x06 every terminal delivers, claimed by no
        // platform, needing no setting. It is what the narrow places
        // advertise for that reason.
        if ((primary && !shift && letter == Some('f'))
            || self
                .keybinds
                .accepts_legacy_alias(BindAction::Reference, key))
            && !alt
            && !self.key_is_a_learnt_chord(key)
            && !self.settings_hold_the_keys()
        {
            self.toggle_reference_browse();
            return true;
        }
        if self.keybinds.action_for(key) == Some(BindAction::Find)
            && !alt
            && !self.key_is_a_learnt_chord(key)
            && !self.settings_hold_the_keys()
        {
            self.open_find();
            return true;
        }
        if primary && !shift && letter == Some('u') && !self.key_is_a_learnt_chord(key) {
            self.toggle_wrap();
            return true;
        }
        if primary
            && letter == Some('e')
            && (!shift || self.keybinds.accepts_legacy_alias(BindAction::HopPane, key))
            && !alt
            && !self.key_is_a_learnt_chord(key)
            && !self.settings_hold_the_keys()
        {
            if shift {
                self.hop_pane();
            } else {
                self.toggle_split();
            }
            return true;
        }
        // Zen is a setting, and it lives in the settings sheet and the
        // View menu; F11 is the historical binding. On a stock Mac F11
        // is Mission Control's, and never reaches the terminal, so the
        // plain chord is the one that works everywhere.
        if (self.keybinds.accepts_legacy_alias(BindAction::Zen, key)
            || (primary && !shift && letter == Some('k')))
            && !alt
            && !self.key_is_a_learnt_chord(key)
        {
            self.toggle_zen();
            return true;
        }
        // Hopping panes has a plain key as well as the shifted chord: a
        // terminal is free to fold Ctrl+Shift+E into Ctrl+E, and then
        // the chord closes the split instead of hopping.
        if key.code == KeyCode::F(10)
            && key.modifiers.is_empty()
            && !self.key_is_a_learnt_chord(key)
            && !self.settings_hold_the_keys()
        {
            self.hop_pane();
            return true;
        }
        // The mixer's desk: a function key, since every terminal
        // delivers one; ^⇧M too where the terminal tells it from ^M,
        // which is Enter's byte and cannot be a chord of its own.
        if key.code == KeyCode::F(4)
            && key.modifiers.is_empty()
            && !self.key_is_a_learnt_chord(key)
            && !self.settings_hold_the_keys()
        {
            self.toggle_mixer_panel();
            return true;
        }
        // The quick action. ^J is a plain 0x0A byte: every terminal
        // delivers it, no platform has claimed it, and it needs no
        // setting - the same reason ^F carries the browser.
        if primary && !shift && letter == Some('j') && !self.key_is_a_learnt_chord(key) {
            self.open_smart_action();
            return true;
        }
        // The log's portable spelling: a legacy terminal folds
        // Ctrl+Shift+D onto Ctrl+D, and Windows Terminal is one.
        if key.code == KeyCode::F(9)
            && key.modifiers.is_empty()
            && !self.key_is_a_learnt_chord(key)
            && !self.settings_hold_the_keys()
        {
            self.toggle_log_panel();
            return true;
        }
        if primary && !shift && letter == Some('p') && !self.key_is_a_learnt_chord(key) {
            self.toggle_device_panel();
            return true;
        }
        false
    }

    /// The chords a set is played with: stepping, making, renaming,
    /// closing and rewinding scenes, takes and samples, and pads. `true`
    /// when one answered.
    fn performance_chord(
        &mut self,
        key: &KeyEvent,
        primary: bool,
        shift: bool,
        alt: bool,
        letter: Option<char>,
    ) -> bool {
        // The scene strip's spellings - ^[/^] and the F6/F7 aliases
        // beside them - belong to the table like every other chord:
        // a previous/next-scene chord moved elsewhere frees these
        // keys rather than leaving them stepping scenes in secret.
        if !self.key_is_a_learnt_chord(key)
            && let Some(navigation) =
                scene_navigation_for_key(key, self.capabilities, self.panes.len() > 1)
        {
            match navigation {
                SceneNavigation::FocusPane => self.hop_pane(),
                SceneNavigation::Step(delta) => self.step_scene(delta),
            }
            return true;
        }
        // Windows Terminal reserves Ctrl+Shift+N for a new window.
        // Advertise a plain function key; retain the old chord below.
        // Modal surfaces and focused panels have already had their turn.
        if key.code == KeyCode::F(3) && key.modifiers.is_empty() && !self.key_is_a_learnt_chord(key)
        {
            self.new_scene(true);
            return true;
        }
        if primary && letter == Some('n') && !self.key_is_a_learnt_chord(key) {
            self.new_scene(shift);
            return true;
        }
        // Rename answers to whatever chord the table resolved for it.
        if !alt && self.keybinds.action_for(key) == Some(BindAction::RenameScene) {
            self.begin_rename();
            return true;
        }
        if primary && shift && letter == Some('r') && !self.key_is_a_learnt_chord(key) {
            self.toggle_take();
            return true;
        }
        // Recording a sample has no built-in spelling of its own to fall
        // back on: it answers to whatever chord the table resolved for
        // this terminal, default or fallback alike.
        if !alt && self.keybinds.action_for(key) == Some(BindAction::RecordSample) {
            self.toggle_sample();
            return true;
        }
        if primary && !shift && letter == Some('w') && !self.key_is_a_learnt_chord(key) {
            self.close_scene();
            return true;
        }
        if primary && letter == Some('l') && !self.key_is_a_learnt_chord(key) {
            if shift {
                self.forget_pad();
            } else {
                self.begin_learn();
            }
            return true;
        }
        // ^⇧U: whether THIS scene rewinds every time it is played,
        // which is the scene's own answer rather than this press's.
        // ^⇧S beside it is the once-off for a scene without the flag.
        if primary && shift && letter == Some('u') && !self.key_is_a_learnt_chord(key) {
            self.toggle_scene_rewind();
            return true;
        }
        false
    }

    /// The keys that work a control rather than a surface: the master
    /// volume, a replay tape's file (Alt with the tape's own letters), the
    /// tape timeline and its blocks, and the slider under the caret. `true`
    /// when one answered.
    fn control_chord(
        &mut self,
        key: &KeyEvent,
        primary: bool,
        shift: bool,
        alt: bool,
        letter: Option<char>,
    ) -> bool {
        if primary
            && shift
            && matches!(key.code, KeyCode::Up | KeyCode::Down)
            && !self.key_is_a_learnt_chord(key)
        {
            let direction = if key.code == KeyCode::Up { 1.0 } else { -1.0 };
            self.nudge_master_gain_db(direction * VOLUME_KEY_STEP_DB);
            return true;
        }
        if alt
            && !primary
            && matches!(self.focus, Focus::Editor | Focus::Timeline)
            && self.current_replay().is_some()
            && self.handle_session_file_key(key.code)
        {
            return true;
        }
        if alt
            && !primary
            && letter == Some('t')
            && matches!(self.focus, Focus::Editor | Focus::Timeline)
            && self.timeline_visible()
        {
            self.focus_timeline();
            return true;
        }
        // Alt+←/→ on a replay tab walk its blocks, and give the timeline
        // the keyboard: plain arrows walk on from there.
        if alt
            && !primary
            && matches!(key.code, KeyCode::Left | KeyCode::Right)
            && self.current_replay().is_some()
        {
            let delta = if key.code == KeyCode::Right { 1 } else { -1 };
            self.step_replay_block(delta);
            self.focus_timeline();
            return true;
        }
        // Alt+↑/↓ nudge the slider under the caret, the one control a
        // score offers that is not text.
        if self.nudge_slider_key(key) {
            return true;
        }
        false
    }

    /// A held key, for whatever took its press: an armed slider, the
    /// timeline, a scene being renamed, the focused panel, or the slider
    /// under the caret that a held Alt+↑/↓ keeps nudging. `true` when it
    /// stops here; held over none of them, it goes on to the score.
    fn route_key_repeat(&mut self, key: &KeyEvent) -> Result<bool, RuntimeError> {
        let primary = key
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::SUPER);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        let shift = key.modifiers.contains(KeyModifiers::SHIFT)
            || matches!(key.code, KeyCode::Char(character) if character.is_uppercase());
        self.settle_focus();
        // A held arrow keeps stepping an armed slider. Sent to the score
        // instead, the repeats walked the caret away from the control
        // while the status still said the arrows were its.
        if self.armed_slider_key(key) {
            return Ok(true);
        }
        if self.focus == Focus::Timeline
            && !primary
            && !alt
            && matches!(
                key.code,
                KeyCode::Left
                    | KeyCode::Right
                    | KeyCode::Home
                    | KeyCode::End
                    | KeyCode::PageUp
                    | KeyCode::PageDown
            )
        {
            self.handle_timeline_key(key.code)?;
            return Ok(true);
        }
        // A held key belongs to whatever took its press. A scene being
        // renamed keeps typing and erasing; a panel takes the repeats
        // it is safe with, and a text field of its own the typing
        // ones; and nothing held ever falls through to the score from
        // under them: a held Backspace in a prompt must not delete
        // the score.
        if matches!(self.strip_mode, SceneStripMode::Renaming(_)) {
            if text_key(key.code) {
                self.handle_strip_key(key.code, primary);
            }
            return Ok(true);
        }
        if let Focus::Panel(kind) = self.focus {
            match super::vim::handle_text_key(self, key) {
                super::vim::VimTextKey::Ignored => {}
                super::vim::VimTextKey::Consumed => return Ok(true),
                super::vim::VimTextKey::Arrow(kind, code) => {
                    let _ = self.dispatch_panel_key(kind, code, false, false, false)?;
                    return Ok(true);
                }
            }
            // Space auditions the reference row even on tabs with a
            // search box. A held preview key must not toggle it off.
            if kind == PanelKind::Reference
                && key.code == KeyCode::Char(' ')
                && self
                    .reference_panel
                    .as_ref()
                    .is_some_and(|panel| !matches!(panel.preview(), PanelAction::Nothing))
            {
                return Ok(true);
            }
            if self.generator_control_repeats(kind, key.code)
                || panel_repeat_is_safe(kind, key.code)
                || (text_key(key.code) && self.panel_is_typing(kind))
            {
                self.dispatch_panel_key(kind, key.code, primary, shift, alt)?;
            } else if super::vim::panel_takes_list_navigation(kind)
                && !self.panel_is_typing(kind)
                && let Some(alias) = self.list_navigation_alias(key)
                && (panel_repeat_is_safe(kind, alias.code)
                    || self.generator_control_repeats(kind, alias.code))
            {
                self.dispatch_panel_key(kind, alias.code, false, false, false)?;
            }
            return Ok(true);
        }
        // A repeat keeps the same modified-arrow meaning as its press;
        // otherwise held Option+arrows fall through to caret navigation.
        if self.nudge_slider_key(key) {
            return Ok(true);
        }
        Ok(false)
    }

    /// A paste: to the text field that owns it, to the drop its path
    /// describes, or nowhere at all. `false` only where the score's editor
    /// takes raw pastes and nothing here claimed it.
    fn route_paste(&mut self, text: &str) -> bool {
        // Text fields own their paste even when it names a real folder or
        // score. Only an unclaimed paste may be interpreted as a file drop.
        //
        // A search box is the exception, and it has to be: it filters names
        // that are already in the studio, so no absolute path could ever
        // match one. A path arriving there is a folder someone dropped on
        // the window while the browser happened to hold the keyboard - the
        // gesture that most wants to work, since the samples panel is where
        // you look to see whether the drop landed. Typed into the box, it
        // would cost one re-rank of the whole catalogue per character.
        if !self.editor_accepts_raw_paste()
            && self.accepts_raw_paste()
            && !(self.focus_filters_names() && dropped_paths(text).is_some())
            && self.paste_to_prompt(text)
        {
            // paste_to_prompt owns the edit - including the theme editor's
            // entry and code, which paste_to_theme_editor already took -
            // so this is only the frame.
            self.dirty_frame = true;
            return true;
        }
        // A file dropped on the window arrives as a paste of its path: a
        // terminal has no drop event, so this is the whole of drag and
        // drop. Dispatched on what the path is rather than on where the
        // keyboard happens to be - you aimed at the window, not at a
        // panel, and the terminal could not have told us where you let go
        // anyway.
        if let Some(paths) = dropped_paths(text) {
            self.accept_drop(&paths);
            return true;
        }
        // A paste that no text field claimed is ignored: not dumped into
        // the score, not run as panel shortcuts. Where the editor takes raw
        // pastes, this says `false` and `hand_to_editor` keeps it.
        if self.paste_to_prompt(text) {
            return true;
        }
        !self.editor_accepts_raw_paste()
    }

    /// What no earlier step of `handle_terminal_event` claimed goes to the
    /// score: a key or a paste its editor reads as a command, and the mouse
    /// to its own router.
    fn hand_to_editor(&mut self, terminal_event: Event) -> Result<(), RuntimeError> {
        match event_to_input_with_binds(
            terminal_event,
            self.capabilities,
            &|key| self.editor_command_for(key),
            &|key| self.keybinds.overrules(key),
        ) {
            Some(EditorInput::Command(command)) => {
                if self.focus == Focus::Editor
                    && matches!(command, Command::InsertText(_))
                    && !self.editor_takes_typed_text()
                {
                    return Ok(());
                }
                // Typing into the score is a claim on it: after an edit the
                // arrows follow the caret again without anyone having to say
                // so. Motion, copying and the transport steal nothing - a
                // Ctrl+S from inside a search box must not.
                if edits_document(&command) {
                    self.focus = Focus::Editor;
                    #[cfg(feature = "hydra")]
                    self.sync_settings_webcam_preview();
                    self.armed_slider = None;
                    self.drop_pane_selection();
                }
                if self.delete_slider_call(&command)? {
                    return Ok(());
                }
                self.dispatch_editor(command)?
            }
            Some(EditorInput::Mouse(mouse)) => self.handle_mouse(mouse)?,
            None => {}
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    mod fuzz_keys {
        //! In-process key fuzz over the kinds of key event that the remote
        //! `key` command can send: single characters with any modifier stack,
        //! the named keys, F1 to F24, Press kind only. Run with:
        //!
        //! ```sh
        //! RUSTEL_HEADLESS=1 cargo test -p rustel-studio --features harness remote_expressible_key_storm \
        //!   -- --ignored --nocapture --test-threads=1
        //! ```
        //!
        //! FUZZ_SEED and FUZZ_EVENTS environment variables steer it. The fuzzer
        //! draws a frame after every 16 events at rotating terminal sizes, as a
        //! remote `screen` request forces a frame, and settles the engine after
        //! every 400 events.

        use crate::app::*;
        use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
        use ratatui::{Terminal, backend::TestBackend};
        use std::panic::AssertUnwindSafe;
        use std::time::Instant;

        /// Remote parse_key's modifier set, excluding NONE (handled separately).
        fn modifiers() -> Vec<KeyModifiers> {
            use KeyModifiers as M;
            vec![
                M::CONTROL,
                M::ALT,
                M::SHIFT,
                M::SUPER,
                M::META,
                M::CONTROL.union(M::SHIFT),
                M::CONTROL.union(M::ALT),
                M::ALT.union(M::SHIFT),
                M::CONTROL.union(M::SUPER).union(M::META),
                M::CONTROL.union(M::ALT).union(M::SHIFT).union(M::SUPER),
            ]
        }

        const NAMED: &[KeyCode] = &[
            KeyCode::Enter,
            KeyCode::Esc,
            KeyCode::Tab,
            KeyCode::BackTab,
            KeyCode::Backspace,
            KeyCode::Delete,
            KeyCode::Insert,
            KeyCode::Left,
            KeyCode::Right,
            KeyCode::Up,
            KeyCode::Down,
            KeyCode::Home,
            KeyCode::End,
            KeyCode::PageUp,
            KeyCode::PageDown,
        ];

        /// Characters a terminal cannot send but `key <char>` can, plus the ascii
        /// range that drives mnemonics and fields.
        const EXOTIC: &[char] = &[
            '\u{FEFF}',
            '\u{200B}',
            '\u{200D}',
            '\u{202E}',
            '\u{00AD}',
            '\u{0301}',
            '\u{3164}',
            '\u{FFE3}',
            '\u{4E00}',
            '\u{0644}',
            '\u{FFFD}',
            '\u{10FFFF}',
            '\u{1F600}',
            '\u{1F1FA}',
            '\u{E0061}',
            '\u{07FF}',
            '\u{0800}',
            '\u{2603}',
            '\u{1F3F3}',
            'x',
            'a',
            'q',
            'e',
            'r',
            't',
            'y',
            'u',
            'i',
            'o',
            'p',
            's',
            'd',
            'f',
            'g',
            'h',
            'j',
            'k',
            'l',
            'z',
            'c',
            'v',
            'b',
            'n',
            'm',
            '0',
            '1',
            '9',
            '-',
            '+',
            '=',
            '_',
            '.',
            ',',
            '/',
            '\\',
            '<',
            '>',
            '[',
            ']',
            '{',
            '}',
            '(',
            ')',
            '"',
            '\'',
            '!',
            '@',
            '#',
            '$',
            '%',
            '^',
            '&',
            '*',
            ';',
            ':',
            '|',
            '~',
            '`',
        ];

        struct Rng(u64);

        impl Rng {
            fn next(&mut self) -> u64 {
                // xorshift64*
                let mut x = self.0;
                x ^= x >> 12;
                x ^= x << 25;
                x ^= x >> 27;
                self.0 = x;
                x.wrapping_mul(0x2545F4914F6CDD1D)
            }

            fn below(&mut self, n: usize) -> usize {
                (self.next() % n as u64) as usize
            }

            fn pick<'a, T>(&mut self, items: &'a [T]) -> &'a T {
                &items[self.below(items.len())]
            }
        }

        fn random_key(rng: &mut Rng, pool: &[KeyModifiers]) -> KeyEvent {
            let roll = rng.next() % 100;
            let modifiers = if roll < 55 {
                KeyModifiers::NONE
            } else {
                *rng.pick(pool)
            };
            let code = match roll % 5 {
                0 => *rng.pick(NAMED),
                1 => KeyCode::F(1 + rng.below(24) as u8),
                2 => KeyCode::Char(*rng.pick(EXOTIC)),
                3 => KeyCode::Char(char::from_u32(0x20 + rng.below(0x5f) as u32).unwrap()),
                _ => *rng.pick(NAMED),
            };
            let mut event = KeyEvent::new(code, modifiers);
            event.kind = KeyEventKind::Press;
            event
        }

        fn describe(event: &KeyEvent) -> String {
            format!("{:?}-{:?}", event.modifiers, event.code)
        }

        #[test]
        #[ignore = "a fuzzer, not a test: run it explicitly"]
        fn remote_expressible_key_storm() {
            assert_eq!(
                std::env::var_os(crate::reveal::HEADLESS_ENV).as_deref(),
                Some(std::ffi::OsStr::new("1")),
                "run this test with RUSTEL_HEADLESS=1 to keep desktop actions disabled"
            );
            let seed: u64 = std::env::var("FUZZ_SEED")
                .ok()
                .and_then(|text| text.parse().ok())
                .unwrap_or(0xC0FFEE);
            let events: usize = std::env::var("FUZZ_EVENTS")
                .ok()
                .and_then(|text| text.parse().ok())
                .unwrap_or(30_000);
            println!("fuzz seed {seed}, {events} events");
            let directory = tempfile::tempdir().unwrap();
            let mut app = crate::app::tests::support::app_over(directory.path());
            let mut rng = Rng(seed ^ events as u64);
            let modifier_pool = modifiers();
            let mut ring: Vec<String> = Vec::new();
            let sizes = [(80, 24), (40, 6), (101, 17), (267, 64), (20, 3), (60, 200)];
            let mut failures = 0;

            for step in 0..events {
                let event = Event::Key(random_key(&mut rng, &modifier_pool));
                ring.push(format!(
                    "{step}:{}",
                    describe(match &event {
                        Event::Key(key) => key,
                        _ => unreachable!(),
                    })
                ));
                if ring.len() > 64 {
                    ring.remove(0);
                }
                let outcome =
                    std::panic::catch_unwind(AssertUnwindSafe(|| app.handle_terminal_event(event)));
                match outcome {
                    Err(_) => {
                        failures += 1;
                        println!("!!! PANIC on event {} - recent events:", step);
                        for line in &ring {
                            println!("    {line}");
                        }
                        if failures >= 3 {
                            break;
                        }
                    }
                    Ok(Err(error)) => {
                        failures += 1;
                        println!("!!! ERR on event {step}: {error:?} - recent events:");
                        for line in &ring {
                            println!("    {line}");
                        }
                        if failures >= 3 {
                            break;
                        }
                    }
                    Ok(Ok(())) => {}
                }
                if step % 16 == 15 {
                    let (width, height) = sizes[(step / 16) % sizes.len()];
                    let draw = std::panic::catch_unwind(AssertUnwindSafe(|| {
                        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
                        terminal
                            .draw(|frame| app.paint_into(frame, Instant::now()).unwrap())
                            .unwrap();
                    }));
                    if draw.is_err() {
                        failures += 1;
                        println!("!!! PANIC drawing at {width}x{height} after event {step}");
                        for line in &ring {
                            println!("    {line}");
                        }
                        if failures >= 3 {
                            break;
                        }
                    }
                }
                if step % 400 == 399
                    && std::panic::catch_unwind(AssertUnwindSafe(|| {
                        crate::app::tests::support::settle(&mut app);
                    }))
                    .is_err()
                {
                    failures += 1;
                    println!("!!! PANIC settling after event {step}");
                    for line in &ring {
                        println!("    {line}");
                    }
                    if failures >= 3 {
                        break;
                    }
                }
            }
            assert_eq!(failures, 0, "{failures} panics with seed {seed}");
        }
    }

    mod keyboard {
        //! Tests for `app/keyboard.rs`.

        use crate::app::keyboard::*;
        use crate::app::tests::support::*;

        #[test]
        fn esc_closes_an_open_sheet_from_vim_normal_behind_the_editor() {
            let directory = tempfile::tempdir().expect("temp dir");
            let (mut app, _scene) = plain_app(directory.path(), "// one\n");
            app.ui_settings.editor_keybindings = crate::editor::EditorKeybindings::Vim;
            app.sync_editor_keybindings();
            app.toggle_settings_sheet();
            assert!(app.settings_sheet.is_some());
            // The editor takes the keyboard while the sheet stays open.
            app.focus = Focus::Editor;

            precision_key(&mut app, KeyCode::Char('i'), KeyModifiers::NONE);
            assert!(app.editor().vim_is_insert(), "i enters Insert");
            precision_key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
            assert!(!app.editor().vim_is_insert(), "Esc leaves Insert");
            assert!(app.settings_sheet.is_some(), "the sheet waits for Normal");

            precision_key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
            assert!(
                app.settings_sheet.is_none(),
                "Esc in Normal closes the sheet"
            );
        }

        #[test]
        fn vim_p_pastes_the_clipboard_once() {
            let directory = tempfile::tempdir().expect("temp dir");
            let (mut app, _scene) = plain_app(directory.path(), "// one\n");
            app.ui_settings.editor_keybindings = crate::editor::EditorKeybindings::Vim;
            app.sync_editor_keybindings();
            app.focus = Focus::Editor;
            app.clipboard
                .set_text("// two\n".into())
                .expect("clipboard holds text");

            precision_key(&mut app, KeyCode::Char('p'), KeyModifiers::NONE);
            let source = app.editor().source();
            assert_eq!(source.matches("// two").count(), 1, "{source:?}");

            // A release is not a second press.
            app.handle_terminal_event(Event::Key(KeyEvent::new_with_kind(
                KeyCode::Char('p'),
                KeyModifiers::NONE,
                KeyEventKind::Release,
            )))
            .unwrap();
            assert_eq!(
                app.editor().source().matches("// two").count(),
                1,
                "a release pasted again"
            );
        }

        #[test]
        fn a_shift_arrow_selection_is_deleted_by_d() {
            let directory = tempfile::tempdir().expect("temp dir");
            let (mut app, _scene) = plain_app(directory.path(), "hello\n");
            app.ui_settings.editor_keybindings = crate::editor::EditorKeybindings::Vim;
            app.sync_editor_keybindings();
            app.focus = Focus::Editor;

            precision_key(&mut app, KeyCode::Right, KeyModifiers::SHIFT);
            assert!(
                !app.editor().primary_selection().is_empty(),
                "Shift+Right makes a selection"
            );
            assert!(
                app.editor().vim_is_visual(),
                "a Shift+arrow selection enters Visual"
            );
            precision_key(&mut app, KeyCode::Char('d'), KeyModifiers::NONE);
            assert!(
                app.editor().source().starts_with("ello\n"),
                "the selected h is gone: {:?}",
                app.editor().source()
            );
        }

        #[test]
        fn a_learnt_ctrl_d_wins_over_page_down() {
            let directory = tempfile::tempdir().expect("temp dir");
            let (mut app, _scene) = plain_app(directory.path(), &"a\n".repeat(12));
            app.ui_settings.editor_keybindings = crate::editor::EditorKeybindings::Vim;
            app.sync_editor_keybindings();
            app.focus = Focus::Editor;
            app.keybinds.learn(
                crate::keybinds::BindAction::Docs,
                crate::keybinds::KeyCombo::parse("ctrl+d"),
            );

            precision_key(&mut app, KeyCode::Char('d'), KeyModifiers::CONTROL);
            assert!(app.reference_panel.is_some(), "the learnt chord opens Docs");
            assert_eq!(app.focus, Focus::Panel(PanelKind::Reference));
        }

        #[test]
        fn vim_ignores_the_caret_shape_setting() {
            let directory = tempfile::tempdir().expect("temp dir");
            let (mut app, _scene) = plain_app(directory.path(), "// one\n");
            app.ui_settings.caret_shape = crate::terminal::CaretShape::BlinkingBar;
            app.ui_settings.editor_keybindings = crate::editor::EditorKeybindings::Vim;
            app.sync_editor_keybindings();
            app.focus = Focus::Editor;

            assert_eq!(
                app.editor_cursor_shape(),
                crate::terminal::CaretShape::SteadyBlock,
                "Normal uses a steady block"
            );
            precision_key(&mut app, KeyCode::Char('i'), KeyModifiers::NONE);
            assert_eq!(
                app.editor_cursor_shape(),
                crate::terminal::CaretShape::SteadyBar,
                "Insert uses a steady bar"
            );

            app.ui_settings.editor_keybindings = crate::editor::EditorKeybindings::Default;
            app.sync_editor_keybindings();
            assert_eq!(
                app.editor_cursor_shape(),
                crate::terminal::CaretShape::BlinkingBar,
                "Default editing keeps the setting"
            );
        }

        #[test]
        fn vim_yank_fills_the_clipboard_and_p_takes_the_newest_copy() {
            let directory = tempfile::tempdir().expect("temp dir");
            let (mut app, _scene) = plain_app(directory.path(), "one two\n");
            app.ui_settings.editor_keybindings = crate::editor::EditorKeybindings::Vim;
            app.sync_editor_keybindings();
            app.focus = Focus::Editor;

            for key in ['v', 'e', 'y'] {
                precision_key(&mut app, KeyCode::Char(key), KeyModifiers::NONE);
            }
            assert_eq!(
                app.clipboard.get_text().expect("yank in clipboard"),
                "one",
                "the visual yank filled the clipboard"
            );

            // A newer copy from another surface beats the old register.
            app.clipboard
                .set_text("generated\n".into())
                .expect("newer copy");
            precision_key(&mut app, KeyCode::Char('p'), KeyModifiers::NONE);
            let source = app.editor().source();
            assert!(source.contains("generated"), "{source:?}");
            assert_eq!(source.matches("generated").count(), 1, "{source:?}");
        }

        /// A newer copy from another surface is put by Vim's rules: after
        /// the caret, a copied line below the current one, and over a
        /// Visual selection, which ends Visual. V keeps the line break.
        #[test]
        fn vim_p_puts_a_newer_copy_by_vim_rules() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = app_over(directory.path());
            app.scenes.current_mut().editor =
                crate::editor::Editor::new("one two\nthree\n").expect("editor");
            app.ui_settings.editor_keybindings = crate::editor::EditorKeybindings::Vim;
            app.sync_editor_keybindings();
            app.focus = Focus::Editor;
            let copy = |app: &mut App, text: &str| {
                app.clipboard.set_text(text.into()).expect("newer copy");
            };

            copy(&mut app, "X");
            precision_key(&mut app, KeyCode::Char('p'), KeyModifiers::NONE);
            assert_eq!(
                app.editor().source(),
                "oXne two\nthree\n",
                "after the caret"
            );
            copy(&mut app, "new\n");
            precision_key(&mut app, KeyCode::Char('p'), KeyModifiers::NONE);
            assert_eq!(
                app.editor().source(),
                "oXne two\nnew\nthree\n",
                "a copied line goes below"
            );

            copy(&mut app, "last");
            precision_key(&mut app, KeyCode::Char('V'), KeyModifiers::SHIFT);
            precision_key(&mut app, KeyCode::Char('p'), KeyModifiers::NONE);
            assert_eq!(
                app.editor().source(),
                "oXne two\nlast\nthree\n",
                "the line it replaced keeps its line break"
            );
            assert!(app.editor().vim_is_normal(), "the put ends Visual");
        }

        #[test]
        fn vim_list_keys_move_panels_and_follow_the_toggle() {
            let directory = tempfile::tempdir().expect("temp dir");
            let (mut app, _scene) = plain_app(directory.path(), "// one\n");
            app.ui_settings.editor_keybindings = crate::editor::EditorKeybindings::Vim;
            app.sync_editor_keybindings();
            app.toggle_settings_sheet();
            let selected = |app: &App| {
                app.settings_sheet
                    .as_ref()
                    .expect("the settings sheet")
                    .selected
            };

            let before = selected(&app);
            precision_key(&mut app, KeyCode::Char('j'), KeyModifiers::NONE);
            assert_eq!(selected(&app), before + 1, "j is Down");
            precision_key(&mut app, KeyCode::Char('k'), KeyModifiers::NONE);
            assert_eq!(selected(&app), before, "k is Up");

            let quantise = app.ui_settings.quantise;
            precision_key(&mut app, KeyCode::Char('l'), KeyModifiers::NONE);
            assert_ne!(app.ui_settings.quantise, quantise, "l is Right");
            precision_key(&mut app, KeyCode::Char('h'), KeyModifiers::NONE);
            assert_eq!(app.ui_settings.quantise, quantise, "h is Left");

            let before = selected(&app);
            precision_key(&mut app, KeyCode::Down, KeyModifiers::NONE);
            assert_eq!(selected(&app), before + 1, "the arrows still work");

            app.ui_settings.vim_list_navigation = false;
            let before = selected(&app);
            let source = app.editor().source();
            precision_key(&mut app, KeyCode::Char('j'), KeyModifiers::NONE);
            assert_eq!(selected(&app), before, "the toggle is off");
            assert_eq!(app.editor().source(), source);

            app.ui_settings.vim_list_navigation = true;
            app.ui_settings.editor_keybindings = crate::editor::EditorKeybindings::Default;
            let before = selected(&app);
            precision_key(&mut app, KeyCode::Char('j'), KeyModifiers::NONE);
            assert_eq!(
                selected(&app),
                before,
                "Default editor keys keep j for text"
            );
        }

        #[test]
        fn a_held_vim_list_key_keeps_moving_the_rows() {
            let directory = tempfile::tempdir().expect("temp dir");
            let (mut app, _scene) = plain_app(directory.path(), "// one\n");
            app.ui_settings.editor_keybindings = crate::editor::EditorKeybindings::Vim;
            app.sync_editor_keybindings();
            app.toggle_settings_sheet();
            let before = app
                .settings_sheet
                .as_ref()
                .expect("the settings sheet")
                .selected;
            app.handle_terminal_event(Event::Key(crossterm::event::KeyEvent::new_with_kind(
                KeyCode::Char('j'),
                KeyModifiers::NONE,
                crossterm::event::KeyEventKind::Repeat,
            )))
            .expect("repeat handled");
            assert_eq!(
                app.settings_sheet
                    .as_ref()
                    .expect("the settings sheet")
                    .selected,
                before + 1,
                "a held j keeps stepping"
            );
        }

        #[test]
        fn vim_list_keys_leave_mixer_faders_to_the_arrows() {
            let directory = tempfile::tempdir().expect("temp dir");
            let (mut app, _scene) = plain_app(directory.path(), "$: s(\"bd\")\n");
            app.ui_settings.editor_keybindings = crate::editor::EditorKeybindings::Vim;
            app.prefs.audio_input = Some("Mic".into());
            app.open_mixer_panel(true);
            assert_eq!(app.focus, Focus::Panel(PanelKind::Mixer));
            app.mixer.selected = crate::app::mixer::MixerTarget::Input;
            let before = app.mixer.input_gain_db;

            precision_key(&mut app, KeyCode::Char('j'), KeyModifiers::NONE);
            assert_eq!(app.mixer.input_gain_db, before, "j is not a fader nudge");
            precision_key(&mut app, KeyCode::Down, KeyModifiers::NONE);
            assert_ne!(app.mixer.input_gain_db, before, "the arrow still nudges");
        }

        #[test]
        fn vim_list_keys_walk_menus_and_other_letters_keep_their_meaning() {
            let directory = tempfile::tempdir().expect("temp dir");
            let (mut app, _scene) = plain_app(directory.path(), "// one\n");
            app.ui_settings.editor_keybindings = crate::editor::EditorKeybindings::Vim;
            app.menu = Some(crate::menu::MenuState::opened());

            precision_key(&mut app, KeyCode::Char('l'), KeyModifiers::NONE);
            assert_eq!(
                app.menu.as_ref().expect("menu").open_title(),
                1,
                "l moves to the next menu"
            );
            // A held l with no press behind it is still the arrow.
            app.menu = Some(crate::menu::MenuState::opened());
            app.handle_terminal_event(Event::Key(crossterm::event::KeyEvent::new_with_kind(
                KeyCode::Char('l'),
                KeyModifiers::NONE,
                crossterm::event::KeyEventKind::Repeat,
            )))
            .expect("repeat handled");
            assert_eq!(
                app.menu.as_ref().expect("menu").open_title(),
                1,
                "a held l walks the bar"
            );
            // h is the Help title mnemonic, but the list keys own it
            // while the mode is on: h moves left.
            precision_key(&mut app, KeyCode::Char('h'), KeyModifiers::NONE);
            assert_eq!(
                app.menu.as_ref().expect("menu").open_title(),
                0,
                "h moves back to the previous menu"
            );
            // A held title mnemonic stays that menu instead of firing a
            // row mnemonic in the level it opened.
            app.menu = Some(crate::menu::MenuState::opened());
            precision_key(&mut app, KeyCode::Char('v'), KeyModifiers::NONE);
            assert_eq!(
                app.menu.as_ref().expect("menu").open_title(),
                4,
                "v opened View"
            );
            app.handle_terminal_event(Event::Key(crossterm::event::KeyEvent::new_with_kind(
                KeyCode::Char('v'),
                KeyModifiers::NONE,
                crossterm::event::KeyEventKind::Repeat,
            )))
            .expect("repeat handled");
            assert!(
                app.menu.as_ref().is_some_and(|menu| menu.is_dropped()),
                "the held v did not fire a View row"
            );
            assert_eq!(app.menu.as_ref().expect("menu").open_title(), 4);

            app.menu = Some(crate::menu::MenuState::opened());
            app.ui_settings.vim_list_navigation = false;
            precision_key(&mut app, KeyCode::Char('l'), KeyModifiers::NONE);
            assert_eq!(app.menu.as_ref().expect("menu").open_title(), 0);
            // With the mode off, the menu letters are mnemonics again.
            precision_key(&mut app, KeyCode::Char('h'), KeyModifiers::NONE);
            assert_eq!(
                app.menu.as_ref().expect("menu").open_title(),
                6,
                "h opens Help when Vim list keys are off"
            );
        }

        #[test]
        fn vim_list_keys_scroll_help_and_follow_the_toggle() {
            let directory = tempfile::tempdir().expect("temp dir");
            let (mut app, _scene) = plain_app(directory.path(), "// one\n");
            app.ui_settings.editor_keybindings = crate::editor::EditorKeybindings::Vim;
            app.toggle_help();
            app.help.as_mut().expect("help").scroll = 0;
            precision_key(&mut app, KeyCode::Char('j'), KeyModifiers::NONE);
            assert_eq!(app.help.as_ref().expect("help").scroll, 1, "j scrolls");

            app.help.as_mut().expect("help").scroll = 0;
            app.ui_settings.vim_list_navigation = false;
            precision_key(&mut app, KeyCode::Char('j'), KeyModifiers::NONE);
            assert_eq!(app.help.as_ref().expect("help").scroll, 0);
        }

        #[test]
        fn a_focused_panel_never_leaks_typing_into_the_score() {
            let directory = tempfile::tempdir().expect("temp dir");
            let (mut app, _scene) = plain_app(directory.path(), "$: s(\"bd*4\")\n");
            let source = app.editor().source();
            app.toggle_viz_dock(0);
            app.focus = Focus::Panel(PanelKind::Viz);
            app.settle_focus();
            assert_eq!(app.focus, Focus::Panel(PanelKind::Viz), "the dock has it");
            for key in [
                KeyCode::Char('Z'),
                KeyCode::Char('q'),
                KeyCode::Char(' '),
                KeyCode::Backspace,
                KeyCode::Delete,
                KeyCode::Enter,
            ] {
                precision_key(&mut app, key, KeyModifiers::NONE);
                assert_eq!(
                    app.editor().source(),
                    source,
                    "{key:?} reached the score from a focused panel"
                );
            }
            // A paste with nowhere to go is dropped too.
            app.handle_terminal_event(Event::Paste("rm -rf".into()))
                .expect("paste handled");
            assert_eq!(app.editor().source(), source);
            // The transport is still global: a set has to be updatable from
            // anywhere, and a chord is not typing.
            assert_eq!(app.focus, Focus::Panel(PanelKind::Viz), "and focus stays");
            // Typing reaches the score again once the score has the keyboard.
            app.focus = Focus::Editor;
            precision_key(&mut app, KeyCode::Char('a'), KeyModifiers::NONE);
            assert_ne!(app.editor().source(), source, "the score still types");
        }

        /// Which panels swallow a stray letter, and which one still hands it to
        /// the score. Written down because this is a behaviour change with a
        /// blast radius: every panel below used to let an unhandled letter reach
        /// the music, and now only the reference-on-a-page does.
        #[test]
        fn only_the_reference_on_a_page_still_types_through() {
            let directory = tempfile::tempdir().expect("temp dir");
            let (mut app, _scene) = plain_app(directory.path(), "$: s(\"bd*4\")\n");
            app.toggle_viz_dock(0);
            app.open_mixer_panel(false);
            app.toggle_log_panel();
            app.toggle_settings_sheet();
            app.toggle_set_panel();
            for kind in [
                PanelKind::Viz,
                PanelKind::Mixer,
                PanelKind::Log,
                PanelKind::Settings,
                PanelKind::Set,
            ] {
                let source = app.editor().source();
                app.focus = Focus::Panel(kind);
                app.settle_focus();
                if app.focus != Focus::Panel(kind) {
                    continue;
                }
                precision_key(&mut app, KeyCode::Char('q'), KeyModifiers::NONE);
                assert_eq!(
                    app.editor().source(),
                    source,
                    "{kind:?} let a letter through to the score"
                );
            }
            // The one exception is the reference on an entry page. In the
            // search box a letter is the query. An entry page has no box, so
            // the letter goes to the score.
            app.dismiss_dialogs(None);
            app.toggle_reference_browse();
            assert_eq!(app.focus, Focus::Panel(PanelKind::Reference));
            let source = app.editor().source();
            precision_key(&mut app, KeyCode::Char('l'), KeyModifiers::NONE);
            assert_eq!(app.editor().source(), source, "the search box took it");
            assert!(app.reference_panel.as_mut().expect("panel").expand());
            precision_key(&mut app, KeyCode::Char('x'), KeyModifiers::NONE);
            assert!(
                app.editor().source().contains('x'),
                "a page has no box, so the letter is the score's"
            );

            // Not the shelf, though, which shares the column and has no box
            // either. Its unclaimed letters were reaching the score through the
            // same test the page is the exception to.
            #[cfg(feature = "hydra")]
            {
                let panel = app.reference_panel.as_mut().expect("panel");
                panel.tab = crate::reference::Tab::Examples;
                // Typing on the page handed the focus to the editor, which is the
                // point of it. Back to the column for the shelf's turn.
                app.focus = Focus::Panel(PanelKind::Reference);
                app.settle_focus();
                assert_eq!(app.focus, Focus::Panel(PanelKind::Reference));
                let source = app.editor().source();
                for letter in ['q', 'x', 'z', 'j'] {
                    precision_key(&mut app, KeyCode::Char(letter), KeyModifiers::NONE);
                }
                assert_eq!(
                    app.editor().source(),
                    source,
                    "the shelf let a letter through to the score"
                );
            }
        }

        #[test]
        fn plain_f3_duplicates_once_and_modified_function_keys_do_not() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = app_over(directory.path());
            let original = app.scenes.current().id;
            let source = app.editor().source();
            let before = app.scenes.score_count();
            press(&mut app, KeyCode::F(3), KeyModifiers::NONE);
            assert_eq!(app.scenes.score_count(), before + 1);
            assert_ne!(app.scenes.current().id, original);
            assert_eq!(app.editor().source(), source);
            assert_eq!(
                std::fs::read_to_string(&app.scenes.current().path).unwrap(),
                source
            );
            for kind in [KeyEventKind::Repeat, KeyEventKind::Release] {
                app.handle_terminal_event(Event::Key(KeyEvent::new_with_kind(
                    KeyCode::F(3),
                    KeyModifiers::NONE,
                    kind,
                )))
                .unwrap();
            }
            for modifiers in [
                KeyModifiers::CONTROL,
                KeyModifiers::ALT,
                KeyModifiers::SUPER,
                KeyModifiers::SHIFT,
            ] {
                press(&mut app, KeyCode::F(3), modifiers);
            }
            assert_eq!(app.scenes.score_count(), before + 1);
            assert_eq!(app.editor().source(), source);
            press(
                &mut app,
                KeyCode::Char('n'),
                KeyModifiers::CONTROL | KeyModifiers::SHIFT,
            );
            assert_eq!(
                app.scenes.score_count(),
                before + 2,
                "legacy duplicate chord remains an alias"
            );
        }

        #[test]
        fn f3_stays_with_modal_input_owners() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = app_over(directory.path());
            app.frame = Rect::new(0, 0, 100, 30);
            let source = app.editor().source();
            let before = app.scenes.score_count();
            app.begin_rename();
            let rename = app.strip_mode.clone();
            press(&mut app, KeyCode::F(3), KeyModifiers::NONE);
            assert_eq!(app.strip_mode, rename);
            app.strip_mode = SceneStripMode::Idle;
            app.toggle_help();
            press(&mut app, KeyCode::F(3), KeyModifiers::NONE);
            assert!(app.help.is_some());
            app.help = None;
            app.menu = Some(crate::menu::MenuState::opened());
            press(&mut app, KeyCode::F(3), KeyModifiers::NONE);
            assert!(app.menu.is_some());
            app.menu = None;
            app.open_set_prompt(SetPrompt::OpenSet);
            let path = app.set_prompt.as_ref().unwrap().1.path.clone();
            press(&mut app, KeyCode::F(3), KeyModifiers::NONE);
            assert_eq!(app.set_prompt.as_ref().unwrap().1.path, path);
            assert_eq!(
                app.scenes.score_count(),
                before,
                "a modal surface leaked F3 to the scene commands"
            );
            assert_eq!(app.editor().source(), source);
        }

        /// A letter typed over a reference entry page reaches the score.
        ///
        /// An entry page has no search box. Every letter goes to the score,
        /// the panel stays open, and the first edit returns focus to the
        /// editor. Every other focused panel keeps the letters it is given.
        #[test]
        fn typing_with_the_reference_open_reaches_the_score() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = app_over(directory.path());
            let before = app.scenes.current().editor.source().len();
            app.toggle_reference_browse();
            assert_eq!(app.focus, Focus::Panel(PanelKind::Reference));

            // In the search box, a letter is a query…
            press(&mut app, KeyCode::Char('l'), KeyModifiers::NONE);
            assert_eq!(
                app.reference_panel
                    .as_ref()
                    .map(|panel| panel.query.as_str()),
                Some("l")
            );
            assert_eq!(app.scenes.current().editor.source().len(), before);

            // …and on an entry's page there is no box, so it is the score's.
            assert!(app.reference_panel.as_mut().expect("panel").expand());
            press(&mut app, KeyCode::Char('x'), KeyModifiers::NONE);
            assert!(app.scenes.current().editor.source().contains('x'));
            assert!(app.reference_panel.is_some(), "the panel stays readable");
            assert_eq!(app.focus, Focus::Editor, "typing claimed the score");
        }

        /// The settings sheet keeps the keys it is given.
        ///
        /// A letter the sheet does not use must not reach the score: the
        /// user looks at the sheet, not at the caret, so the edit would go
        /// unseen. A focused panel keeps its keys and Esc returns them. The
        /// reference on an entry page is the only exception, because that
        /// page has no text field.
        #[test]
        fn the_settings_sheet_keeps_the_keys_it_is_given() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = app_over(directory.path());
            app.toggle_settings_sheet();
            assert_eq!(app.focus, Focus::Panel(PanelKind::Settings));

            // Focused, the arrows belong to the sheet…
            press(&mut app, KeyCode::Down, KeyModifiers::NONE);
            assert_eq!(app.settings_sheet.expect("sheet").selected, 1);
            // …and so is a letter. A sheet that has the keyboard has it: a
            // stray `a` over the settings appearing in the music is a bug, not
            // a shortcut, and the footer says where the keyboard went rather
            // than dropping the key in silence.
            let source = app.scenes.current().editor.source();
            press(&mut app, KeyCode::Char('a'), KeyModifiers::NONE);
            assert_eq!(app.scenes.current().editor.source(), source);
            assert!(app.settings_sheet.is_some());
            assert_eq!(app.focus, Focus::Panel(PanelKind::Settings));
            assert!(app.status.contains("has the keyboard"), "{}", app.status);
            // Esc gives it back, and then the score types again.
            press(&mut app, KeyCode::Esc, KeyModifiers::NONE);
            press(&mut app, KeyCode::Char('a'), KeyModifiers::NONE);
            assert!(app.scenes.current().editor.source().contains('a'));
        }

        /// A focused panel keeps a Tab it does not use, the same as a letter,
        /// so Tab cannot indent the score while the user looks at the panel.
        /// The test uses a visuals dock because that panel has no use for Tab;
        /// the settings sheet has one. A reading panel does not pass Tab on
        /// either.
        #[test]
        fn a_focused_panel_swallows_a_tab_instead_of_indenting_the_score() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = app_over(directory.path());
            app.open_viz_dock(0, true);
            assert_eq!(app.focus, Focus::Panel(PanelKind::Viz));

            let source = app.scenes.current().editor.source();
            press(&mut app, KeyCode::Tab, KeyModifiers::NONE);
            assert_eq!(
                app.scenes.current().editor.source(),
                source,
                "a Tab the dock has no use for must not indent the score"
            );
            assert_eq!(app.focus, Focus::Panel(PanelKind::Viz));
            assert!(app.status.contains("has the keyboard"), "{}", app.status);

            // And the reference column read beside the score is the exception
            // that proves the rule: it passes the arrows to the caret, but not
            // Tab, because Tab would edit rather than move.
            let mut app = app_over(directory.path());
            app.toggle_reference_browse();
            app.reference_anchor = None;
            let source = app.scenes.current().editor.source();
            press(&mut app, KeyCode::Tab, KeyModifiers::NONE);
            assert_eq!(
                app.scenes.current().editor.source(),
                source,
                "not even a reading column hands Tab to the score"
            );
        }

        /// iTerm2 reports a held arrow as Press, Repeat..., Release when the
        /// enhanced keyboard protocol is active. The settings sheet must consume
        /// every navigation step except the release record.
        #[test]
        fn focused_settings_navigation_accepts_repeat_but_not_release() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = app_over(directory.path());
            app.settings_sheet = Some(SettingsSheet::default());
            app.focus = Focus::Panel(PanelKind::Settings);

            for kind in [
                KeyEventKind::Press,
                KeyEventKind::Repeat,
                KeyEventKind::Release,
            ] {
                app.handle_terminal_event(Event::Key(KeyEvent::new_with_kind(
                    KeyCode::Down,
                    KeyModifiers::NONE,
                    kind,
                )))
                .expect("key handled");
            }

            assert_eq!(
                app.settings_sheet.expect("settings remain open").selected,
                2,
                "Press and Repeat move; Release does not"
            );
            assert_eq!(app.focus, Focus::Panel(PanelKind::Settings));
        }

        #[test]
        fn held_horizontal_keys_do_not_retrigger_reference_actions() {
            assert!(panel_repeat_is_safe(PanelKind::Settings, KeyCode::Right));
            assert!(panel_repeat_is_safe(PanelKind::Reference, KeyCode::Down));
            assert!(!panel_repeat_is_safe(PanelKind::Reference, KeyCode::Right));
            assert!(!panel_repeat_is_safe(PanelKind::Reference, KeyCode::Left));
        }

        /// The transport still wins from inside a search box.
        #[test]
        fn the_transport_still_wins_from_inside_a_search_box() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = app_over(directory.path());
            app.toggle_reference_browse();
            press(&mut app, KeyCode::Char('g'), KeyModifiers::CONTROL);
            assert_eq!(
                app.reference_panel
                    .as_ref()
                    .map(|panel| panel.query.as_str()),
                Some(""),
                "Ctrl+G was the transport's, not the query's"
            );
            assert_eq!(app.focus, Focus::Panel(PanelKind::Reference));
        }

        #[test]
        fn scene_switching_uses_function_keys_until_modified_keys_are_precise() {
            let key = |code, modifiers| KeyEvent::new(code, modifiers);
            for capabilities in [
                KeyboardCapabilities::legacy(),
                KeyboardCapabilities::enhanced(),
            ] {
                assert_eq!(
                    scene_step_for_key(&key(KeyCode::F(6), KeyModifiers::NONE), capabilities),
                    Some(-1)
                );
                assert_eq!(
                    scene_step_for_key(&key(KeyCode::F(7), KeyModifiers::NONE), capabilities),
                    Some(1)
                );
            }
            assert_eq!(
                scene_step_for_key(
                    &key(KeyCode::Char('['), KeyModifiers::CONTROL),
                    KeyboardCapabilities::legacy(),
                ),
                None
            );
            assert_eq!(
                scene_step_for_key(
                    &key(KeyCode::Char(']'), KeyModifiers::CONTROL),
                    KeyboardCapabilities::enhanced(),
                ),
                Some(1)
            );
            assert_eq!(
                scene_step_for_key(
                    &key(KeyCode::Char('['), KeyModifiers::ALT),
                    KeyboardCapabilities::enhanced(),
                ),
                None
            );
            assert_eq!(
                scene_step_for_key(
                    &key(KeyCode::F(6), KeyModifiers::CONTROL),
                    KeyboardCapabilities::enhanced(),
                ),
                None
            );
        }

        /// Update and stop work from the keyboard whatever is on screen: the
        /// transport reads a key before any panel. It must claim every spelling
        /// of the two and no other key, or a panel loses a key that it needs.
        #[test]
        fn the_transport_claims_its_keys_and_only_its_keys() {
            use crossterm::event::KeyEvent;
            let key = |code, modifiers| KeyEvent::new(code, modifiers);
            const CTRL: KeyModifiers = KeyModifiers::CONTROL;

            for capabilities in [
                KeyboardCapabilities::legacy(),
                KeyboardCapabilities::enhanced(),
            ] {
                let binds = crate::keybinds::Keybinds::default();
                let claims =
                    |code, modifiers| transport_key(&key(code, modifiers), capabilities, &binds);
                // The portable spelling, and the one the footer advertises.
                assert_eq!(
                    claims(KeyCode::F(5), KeyModifiers::NONE),
                    Some(Command::Evaluate)
                );
                assert_eq!(
                    claims(KeyCode::F(8), KeyModifiers::NONE),
                    Some(Command::Stop)
                );
                assert_eq!(claims(KeyCode::Char('s'), CTRL), Some(Command::Evaluate));
                assert_eq!(claims(KeyCode::Char('g'), CTRL), Some(Command::Stop));

                // Plain Enter belongs to whatever is on screen: it confirms a
                // rename, opens a reference entry, picks a device. Claiming it
                // would take the one key those panels cannot do without.
                assert_eq!(claims(KeyCode::Enter, KeyModifiers::NONE), None);
                // Neither are the keys the panels themselves are listening for:
                // Plain letters belong to the active panel.
                assert_eq!(claims(KeyCode::Char('s'), KeyModifiers::NONE), None);
                assert_eq!(claims(KeyCode::Char('g'), KeyModifiers::NONE), None);
                assert_eq!(claims(KeyCode::Char('c'), CTRL), None);
                assert_eq!(claims(KeyCode::Esc, KeyModifiers::NONE), None);
                // Scene stepping is next door and stays there.
                assert_eq!(claims(KeyCode::F(6), KeyModifiers::NONE), None);
                assert_eq!(claims(KeyCode::F(7), KeyModifiers::NONE), None);
            }

            // Ctrl+Enter and Ctrl+. only where the terminal reports them apart
            // from plain Enter and a full stop. On a legacy terminal they are
            // the same bytes, and claiming them would cost the panels Enter.
            let binds = crate::keybinds::Keybinds::default();
            let enhanced =
                |code| transport_key(&key(code, CTRL), KeyboardCapabilities::enhanced(), &binds);
            let legacy =
                |code| transport_key(&key(code, CTRL), KeyboardCapabilities::legacy(), &binds);
            assert_eq!(enhanced(KeyCode::Enter), Some(Command::Evaluate));
            assert_eq!(enhanced(KeyCode::Char('.')), Some(Command::Stop));
            assert_eq!(legacy(KeyCode::Enter), None);
            assert_eq!(legacy(KeyCode::Char('.')), None);
        }

        /// A held key belongs to the prompt that took its press: repeats of
        /// Backspace erase in the field and never reach the score, and a
        /// held letter keeps typing there.
        #[test]
        fn a_held_key_in_a_prompt_never_reaches_the_score() {
            use crossterm::event::{KeyEvent, KeyEventKind};
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = app_over(directory.path());
            app.frame = Rect::new(0, 0, 140, 40);
            let score_before = app.editor().source();
            app.dispatch_menu_action(crate::menu::MenuAction::RenameSet)
                .expect("menu");
            let send = |app: &mut App, code: KeyCode, kind: KeyEventKind| {
                app.handle_terminal_event(Event::Key(KeyEvent::new_with_kind(
                    code,
                    KeyModifiers::NONE,
                    kind,
                )))
                .expect("key handled");
            };
            send(&mut app, KeyCode::Char('x'), KeyEventKind::Press);
            send(&mut app, KeyCode::Char('y'), KeyEventKind::Press);
            for _ in 0..6 {
                send(&mut app, KeyCode::Backspace, KeyEventKind::Repeat);
            }
            send(&mut app, KeyCode::Char('z'), KeyEventKind::Repeat);
            send(&mut app, KeyCode::Char('z'), KeyEventKind::Repeat);
            let field = app
                .set_prompt
                .as_ref()
                .map(|(_, picker)| picker.path.clone());
            assert_eq!(field.as_deref(), Some("zz"), "the field took every repeat");
            assert_eq!(
                app.editor().source(),
                score_before,
                "the score under the prompt is untouched"
            );
            assert_eq!(app.focus, Focus::Panel(PanelKind::Set));
            // Selection in the field: Shift+Left twice, then a letter replaces.
            send(&mut app, KeyCode::Left, KeyEventKind::Press);
            press(&mut app, KeyCode::Left, KeyModifiers::SHIFT);
            let selection = app
                .set_prompt
                .as_ref()
                .and_then(|(_, picker)| picker.selection());
            assert_eq!(selection, Some(0..1));
            send(&mut app, KeyCode::Char('q'), KeyEventKind::Press);
            let field = app
                .set_prompt
                .as_ref()
                .map(|(_, picker)| picker.path.clone());
            assert_eq!(field.as_deref(), Some("qz"));
            press(&mut app, KeyCode::Char('a'), KeyModifiers::CONTROL);
            let selection = app
                .set_prompt
                .as_ref()
                .and_then(|(_, picker)| picker.selection());
            assert_eq!(selection, Some(0..2), "Ctrl+A selects it all");
        }

        /// A control character that reaches the studio as a key - macOS's
        /// screenshot shortcut leaves an SOH behind in some terminals - is not
        /// typed into the score.
        #[test]
        fn a_control_character_key_is_not_typed() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = app_over(directory.path());
            type_text(&mut app, "s(\"bd\")");
            let before = app.scenes.current().editor.source();
            for character in ['\u{1}', '\u{1b}', '\u{7f}'] {
                app.handle_terminal_event(Event::Key(KeyEvent::new(
                    KeyCode::Char(character),
                    KeyModifiers::NONE,
                )))
                .expect("event");
                assert_eq!(
                    app.scenes.current().editor.source(),
                    before,
                    "{character:?} was typed"
                );
            }
            // Inside a paste the same characters are dropped, the rest kept.
            app.handle_terminal_event(Event::Paste("\u{1}hh\u{1b}".into()))
                .expect("paste");
            let source = app.scenes.current().editor.source();
            assert!(source.contains("hh"), "{source:?}");
            assert!(
                !source.chars().any(crate::editor::is_stray_control),
                "{source:?}"
            );
        }

        #[test]
        fn typing_resets_the_caret_blink_phase() {
            let directory = tempfile::tempdir().unwrap();
            let mut app = app_over(directory.path());
            app.caret_visible = false;
            app.caret_activity = Instant::now() - Duration::from_secs(10);
            let before = Instant::now();
            press(&mut app, KeyCode::Char('x'), KeyModifiers::NONE);
            assert!(app.caret_visible);
            assert!(app.caret_activity >= before);
        }

        #[test]
        fn a_composed_option_character_types_in_every_editor_mode() {
            let directory = tempfile::tempdir().expect("temp dir");
            let (mut app, _scene) = plain_app(directory.path(), "ax\n");
            let composed = || Event::KeyWithText {
                key: KeyEvent::new(KeyCode::Char('4'), KeyModifiers::ALT),
                text: "$".into(),
            };
            app.handle_terminal_event(composed())
                .expect("composed text");
            assert!(
                app.editor().source().starts_with('$'),
                "Default typing takes it"
            );

            let directory = tempfile::tempdir().expect("temp dir");
            let (mut app, _scene) = plain_app(directory.path(), "ax\n");
            app.ui_settings.editor_keybindings = crate::editor::EditorKeybindings::Vim;
            app.sync_editor_keybindings();
            precision_key(&mut app, KeyCode::Char('i'), KeyModifiers::NONE);
            app.handle_terminal_event(composed())
                .expect("composed text");
            assert!(
                app.editor().source().starts_with('$'),
                "Vim Insert takes it"
            );

            let directory = tempfile::tempdir().expect("temp dir");
            let (mut app, _scene) = plain_app(directory.path(), "ax\n");
            app.ui_settings.editor_keybindings = crate::editor::EditorKeybindings::Vim;
            app.sync_editor_keybindings();
            let source = app.editor().source();
            app.handle_terminal_event(composed())
                .expect("composed text");
            assert_eq!(app.editor().source(), source, "Vim Normal does not type it");
            assert_ne!(
                app.editor().primary_selection().head.0,
                0,
                "Vim Normal moves the caret"
            );
        }

        #[test]
        fn composed_text_with_many_characters_pastes() {
            let directory = tempfile::tempdir().expect("temp dir");
            let (mut app, _scene) = plain_app(directory.path(), "ax\n");
            app.handle_terminal_event(Event::KeyWithText {
                key: KeyEvent::new(KeyCode::Char('4'), KeyModifiers::ALT),
                text: "ab".into(),
            })
            .expect("composed text");
            assert!(app.editor().source().contains("ab"));
        }

        /// Option+. stops on a terminal that reports the composed `≥` too:
        /// the browser spelling of stop is a chord, not text.
        #[test]
        fn a_composed_option_period_stops_instead_of_typing() {
            let directory = tempfile::tempdir().expect("temp dir");
            let (mut app, _scene) = plain_app(directory.path(), "ax\n");
            app.snapshot = Some(sounding(true, false));
            let source = app.editor().source();
            app.handle_terminal_event(Event::KeyWithText {
                key: KeyEvent::new(KeyCode::Char('.'), KeyModifiers::ALT),
                text: "≥".into(),
            })
            .expect("composed stop");
            assert_eq!(app.editor().source(), source, "nothing typed");
            assert!(app.stop_requested, "the stop reached the engine");
        }

        #[test]
        fn composed_text_on_a_lock_key_does_not_type() {
            let directory = tempfile::tempdir().expect("temp dir");
            let (mut app, _scene) = plain_app(directory.path(), "ax\n");
            let source = app.editor().source();
            app.handle_terminal_event(Event::KeyWithText {
                key: KeyEvent::new(KeyCode::CapsLock, KeyModifiers::NONE),
                text: "A".into(),
            })
            .expect("composed lock key");
            assert_eq!(app.editor().source(), source);
        }

        #[test]
        fn a_composed_release_does_not_type() {
            let directory = tempfile::tempdir().expect("temp dir");
            let (mut app, _scene) = plain_app(directory.path(), "ax\n");
            let source = app.editor().source();
            app.handle_terminal_event(Event::KeyWithText {
                key: KeyEvent::new_with_kind(
                    KeyCode::Char('4'),
                    KeyModifiers::ALT,
                    KeyEventKind::Release,
                ),
                text: "$".into(),
            })
            .expect("composed release");
            assert_eq!(app.editor().source(), source);
        }
    }

    mod strip_digits {
        //! Alt with a digit selects that strip's score, the way the digit
        //! on its row in the set panel does - and nothing that shares the
        //! editor's keyboard loses its keys to it.

        use crate::app::keyboard::*;
        use crate::app::tests::support::*;
        use crossterm::event::{KeyCode, KeyModifiers};

        /// A studio with two scores on the strip, the second current, and
        /// the editor holding the keyboard.
        fn app_with_two_scores(directory: &std::path::Path) -> App {
            let mut app = app_over(directory);
            std::fs::write(directory.join("second.strudel"), "$: s(\"hh\")\n")
                .expect("a second score");
            app.scenes
                .open_file(&directory.join("second.strudel"))
                .expect("open the second score");
            app.land_on_current_scene();
            app.focus = Focus::Editor;
            app
        }

        #[test]
        fn an_alt_digit_selects_that_strips_score() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = app_with_two_scores(directory.path());
            assert_eq!(app.scenes.current().name(), "second");
            press(&mut app, KeyCode::Char('1'), KeyModifiers::ALT);
            assert_eq!(
                app.scenes.current().name(),
                "first",
                "Alt+1 selects the strip's first score"
            );
            assert_eq!(app.status, "scene 1 - first");
            press(&mut app, KeyCode::Char('2'), KeyModifiers::ALT);
            assert_eq!(app.scenes.current().name(), "second");
            // A number the strip does not hold says so and moves nothing.
            press(&mut app, KeyCode::Char('9'), KeyModifiers::ALT);
            assert_eq!(app.scenes.current().name(), "second");
            assert_eq!(app.status, "no scene 9 on the strip");
        }

        /// The chord answers in vim mode too, from Insert as from Normal.
        #[test]
        fn an_alt_digit_selects_in_vim_mode() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = app_with_two_scores(directory.path());
            app.ui_settings.editor_keybindings = crate::editor::EditorKeybindings::Vim;
            app.sync_editor_keybindings();
            assert!(!app.editor().vim_is_insert(), "the score opens in Normal");
            press(&mut app, KeyCode::Char('1'), KeyModifiers::ALT);
            assert_eq!(app.scenes.current().name(), "first");
            press(&mut app, KeyCode::Char('i'), KeyModifiers::NONE);
            assert!(app.editor().vim_is_insert());
            press(&mut app, KeyCode::Char('2'), KeyModifiers::ALT);
            assert_eq!(
                app.scenes.current().name(),
                "second",
                "Alt+2 selects from Insert"
            );
        }

        /// Ctrl with Shift spells the same selection where Option types
        /// real characters, as the Swedish layout's Option row does, and a
        /// chord learnt over the key retires it.
        #[test]
        fn a_ctrl_shift_digit_selects_the_strip_score_and_retires() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = app_with_two_scores(directory.path());
            app.ui_settings.editor_keybindings = crate::editor::EditorKeybindings::Vim;
            app.sync_editor_keybindings();
            press(
                &mut app,
                KeyCode::Char('1'),
                KeyModifiers::CONTROL | KeyModifiers::SHIFT,
            );
            assert_eq!(
                app.scenes.current().name(),
                "first",
                "Ctrl+Shift+1 selected from vim Normal"
            );
            press(
                &mut app,
                KeyCode::Char('2'),
                KeyModifiers::CONTROL | KeyModifiers::SHIFT,
            );
            assert_eq!(
                app.scenes.current().name(),
                "second",
                "Ctrl+Shift+2 selected too"
            );
            // A chord learnt over the key takes it back: the press now
            // comments the line instead of selecting a score.
            let chord =
                crate::keybinds::KeyCombo::parse("ctrl+shift+1").expect("a parseable chord");
            app.keybinds.learn(BindAction::ToggleComment, Some(chord));
            press(
                &mut app,
                KeyCode::Char('1'),
                KeyModifiers::CONTROL | KeyModifiers::SHIFT,
            );
            assert_eq!(
                app.scenes.current().name(),
                "second",
                "the learnt chord retired the strip digit"
            );
        }

        /// The find bar keeps its digits: the chord waits until it is shut.
        #[test]
        fn the_find_bar_keeps_its_keys() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = app_with_two_scores(directory.path());
            press(
                &mut app,
                KeyCode::Char('y'),
                KeyModifiers::CONTROL | KeyModifiers::SHIFT,
            );
            assert!(app.editor().find_open());
            press(&mut app, KeyCode::Char('1'), KeyModifiers::ALT);
            assert_eq!(
                app.scenes.current().name(),
                "second",
                "the find bar had the keyboard"
            );
            press(&mut app, KeyCode::Esc, KeyModifiers::NONE);
            press(&mut app, KeyCode::Char('1'), KeyModifiers::ALT);
            assert_eq!(app.scenes.current().name(), "first");
        }

        /// iTerm2 without the keyboard protocol sends a composed Option
        /// digit as its character alone, the way a layout's own key sends
        /// it, so the character types and the strip stays.
        #[test]
        fn iterm2s_composed_digit_types() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = app_with_two_scores(directory.path());
            app.keybinds.set_reach(crate::keybinds::Reach {
                enhanced: false,
                terminal: "iTerm2".into(),
            });
            press(&mut app, KeyCode::Char('§'), KeyModifiers::NONE);
            assert_eq!(app.scenes.current().name(), "second", "the strip moved");
            assert!(
                app.editor().source().contains('§'),
                "the character typed: {:?}",
                app.editor().source()
            );
        }

        /// A terminal that carries the keyboard protocol reports the Alt
        /// key itself, with the composition riding beside it. The exotic
        /// composition gives up the chord, with no terminal table at all.
        /// A plain ASCII one keeps typing, because on a Swedish layout
        /// Option+2 is the one way to type @.
        #[test]
        fn a_protocols_composed_digit_recovers_the_chord() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = app_with_two_scores(directory.path());
            app.handle_terminal_event(Event::KeyWithText {
                key: KeyEvent::new(KeyCode::Char('1'), KeyModifiers::ALT),
                text: "¡".into(),
            })
            .expect("composed digit");
            assert_eq!(
                app.scenes.current().name(),
                "first",
                "the protocol's Alt+1 reached the strip"
            );
            app.handle_terminal_event(Event::KeyWithText {
                key: KeyEvent::new(KeyCode::Char('2'), KeyModifiers::ALT),
                text: "@".into(),
            })
            .expect("composed digit");
            assert_eq!(
                app.scenes.current().name(),
                "first",
                "the plain composition did not reach the strip"
            );
            assert!(
                app.editor().source().contains('@'),
                "the plain composition typed: {:?}",
                app.editor().source()
            );
        }
    }

    mod paging {
        //! Paging belongs to the visible, focused surface and uses its current size.

        use crate::app::*;
        use crate::editor::Selection;
        use crate::reference::{ReferenceMode, entry_body, entry_body_area};
        use crate::settings::{SettingsPage, SettingsSheetView};
        use crate::theme_editor::{EditorTab, ThemeEditorView};

        fn app(directory: &std::path::Path) -> App {
            let mut app = crate::app::tests::support::app_over(directory);
            app.frame = Rect::new(0, 0, 120, 30);
            app.focus = Focus::Editor;
            app
        }

        fn page(app: &mut App, code: KeyCode, kind: KeyEventKind) {
            app.handle_terminal_event(Event::Key(KeyEvent::new_with_kind(
                code,
                KeyModifiers::NONE,
                kind,
            )))
            .unwrap();
        }

        #[test]
        fn keybinding_pages_handle_press_repeat_and_endpoints_without_editing_the_score() {
            let directory = tempfile::tempdir().unwrap();
            let mut app = app(directory.path());
            app.toggle_settings_sheet();
            app.settings_sheet
                .as_mut()
                .unwrap()
                .show_page(SettingsPage::Keybinds);
            app.focus = Focus::Panel(PanelKind::Settings);
            let (_, list) = SettingsSheetView::geometry(app.settings_sheet_frame()).unwrap();
            let rows = usize::from(list.height);
            let source = app.editor().source();
            let caret = app.editor().primary_selection();
            page(&mut app, KeyCode::PageDown, KeyEventKind::Press);
            assert_eq!(app.settings_sheet.as_ref().unwrap().selected, rows - 1);
            page(&mut app, KeyCode::PageDown, KeyEventKind::Repeat);
            assert_eq!(
                app.settings_sheet.as_ref().unwrap().selected,
                (rows * 2 - 1).min(crate::settings::RESET_KEYBINDS_ROW)
            );
            for _ in 0..10 {
                page(&mut app, KeyCode::PageDown, KeyEventKind::Press);
            }
            assert_eq!(
                app.settings_sheet.as_ref().unwrap().selected,
                crate::settings::RESET_KEYBINDS_ROW
            );
            for _ in 0..10 {
                page(&mut app, KeyCode::PageUp, KeyEventKind::Repeat);
            }
            assert_eq!(app.settings_sheet.as_ref().unwrap().selected, 0);
            assert_eq!(app.editor().source(), source);
            assert_eq!(app.editor().primary_selection(), caret);
            assert_eq!(app.focus, Focus::Panel(PanelKind::Settings));
            assert!(app.keybind_learn.is_none());
        }

        #[test]
        fn file_picker_pages_candidates_and_browser_without_wrapping_or_editing_paths() {
            let directory = tempfile::tempdir().unwrap();
            let candidates = (0..35)
                .map(|i| Candidate {
                    label: format!("file{i}"),
                    detail: String::new(),
                    path: directory.path().join(format!("file{i}")),
                })
                .collect();
            let mut picker = FilePicker::new("Open", "Open", candidates, directory.path());
            let frame = Rect::new(0, 0, 120, 30);
            let rows = usize::from(picker.geometry(frame).unwrap().1.height);
            assert_eq!(rows, 12);
            picker.hold_scroll = true;
            assert!(matches!(
                picker_key(&mut picker, KeyCode::PageDown, false, false, frame),
                PickerKey::Handled
            ));
            assert_eq!(picker.selected, rows);
            assert!(!picker.hold_scroll);
            picker.selected = 34;
            picker_key(&mut picker, KeyCode::PageDown, false, false, frame);
            assert_eq!(picker.selected, 34);
            picker.selected = 0;
            picker_key(&mut picker, KeyCode::PageUp, false, false, frame);
            assert_eq!(picker.selected, 0);
            picker.toggle_browsing();
            let entries = picker
                .candidates
                .iter()
                .map(|candidate| crate::file_picker::Entry {
                    name: candidate.label.clone(),
                    path: candidate.path.clone(),
                    folder: false,
                    detail: String::new(),
                })
                .collect();
            picker.browser.as_mut().unwrap().entries = entries;
            picker.browser.as_mut().unwrap().selected = 0;
            picker_key(&mut picker, KeyCode::PageDown, false, false, frame);
            assert_eq!(picker.browser.as_ref().unwrap().selected, rows);
            picker.browser.as_mut().unwrap().selected = 34;
            picker_key(&mut picker, KeyCode::PageDown, false, false, frame);
            assert_eq!(picker.browser.as_ref().unwrap().selected, 34);
            picker.toggle_browsing();
            picker.start_typing();
            picker.path = "C:\\samples".into();
            let selected = picker.selected;
            picker_key(&mut picker, KeyCode::PageDown, false, false, frame);
            assert_eq!(picker.path, "C:\\samples");
            assert_eq!(picker.selected, selected);
        }

        #[test]
        fn reference_pages_use_visible_rows_and_entry_scroll_stops_at_the_end() {
            let directory = tempfile::tempdir().unwrap();
            let mut app = app(directory.path());
            app.reference_panel = Some(ReferencePanel::browse(&app.reference));
            app.focus = Focus::Panel(PanelKind::Reference);
            app.regions.reference = Rect::new(80, 2, 38, 25);
            let inner = inner_area(app.regions.reference);
            let rows = usize::from(
                app.reference_panel
                    .as_ref()
                    .unwrap()
                    .geometry(inner)
                    .list
                    .height,
            );
            let before = app.reference_panel.as_ref().unwrap().selected;
            let source = app.editor().source();
            page(&mut app, KeyCode::PageDown, KeyEventKind::Press);
            let selected = app.reference_panel.as_ref().unwrap().selected;
            assert!(selected >= before + rows);
            assert!(
                selected <= before + rows + 1,
                "only section headings may be skipped"
            );
            let index = (0..app.reference.len())
                .max_by_key(|index| {
                    entry_body(
                        app.reference.entry(*index).unwrap(),
                        usize::from(inner.width),
                    )
                    .len()
                })
                .unwrap();
            app.reference_panel = Some(ReferencePanel::open(&app.reference, index));
            let body = entry_body_area(inner);
            let count = entry_body(
                app.reference.entry(index).unwrap(),
                usize::from(inner.width),
            )
            .len();
            let last = count.saturating_sub(usize::from(body.height));
            assert!(last > usize::from(body.height));
            page(&mut app, KeyCode::PageDown, KeyEventKind::Press);
            assert!(
                matches!(app.reference_panel.as_ref().unwrap().mode, ReferenceMode::Entry { scroll, .. } if usize::from(scroll) == usize::from(body.height))
            );
            for _ in 0..count {
                page(&mut app, KeyCode::PageDown, KeyEventKind::Repeat);
            }
            assert!(
                matches!(app.reference_panel.as_ref().unwrap().mode, ReferenceMode::Entry { scroll, .. } if usize::from(scroll) == last)
            );
            page(&mut app, KeyCode::PageUp, KeyEventKind::Press);
            assert!(
                matches!(app.reference_panel.as_ref().unwrap().mode, ReferenceMode::Entry { scroll, .. } if usize::from(scroll) == last.saturating_sub(usize::from(body.height)))
            );
            assert_eq!(app.editor().source(), source);
            assert_eq!(app.focus, Focus::Panel(PanelKind::Reference));
        }

        #[test]
        fn theme_picker_pages_the_visible_list_without_wrapping() {
            let directory = tempfile::tempdir().unwrap();
            let mut app = app(directory.path());
            app.theme_picker = Some(ThemePicker::open(&app.theme));
            app.focus = Focus::Panel(PanelKind::Theme);
            let picker = app.theme_picker.as_mut().unwrap();
            picker.selected = 0;
            let rows = usize::from(picker.geometry(app.frame).unwrap().1.height);
            let last = picker.match_count().saturating_sub(1);
            let source = app.editor().source();
            page(&mut app, KeyCode::PageDown, KeyEventKind::Press);
            assert_eq!(app.theme_picker.as_ref().unwrap().selected, rows.min(last));
            page(&mut app, KeyCode::End, KeyEventKind::Press);
            page(&mut app, KeyCode::PageDown, KeyEventKind::Repeat);
            assert_eq!(app.theme_picker.as_ref().unwrap().selected, last);
            page(&mut app, KeyCode::Home, KeyEventKind::Press);
            page(&mut app, KeyCode::PageUp, KeyEventKind::Repeat);
            assert_eq!(app.theme_picker.as_ref().unwrap().selected, 0);
            assert_eq!(app.editor().source(), source);
            assert_eq!(app.focus, Focus::Panel(PanelKind::Theme));
        }

        #[test]
        fn theme_editor_pages_form_and_code_at_current_size_before_redraw() {
            let directory = tempfile::tempdir().unwrap();
            let mut app = app(directory.path());
            let theme = app.theme.clone();
            app.open_theme_editor(&theme, "paging-test".into(), false);
            let source = app.editor().source();
            let rows = usize::from(
                ThemeEditorView::geometry(app.frame)
                    .unwrap()
                    .height
                    .saturating_sub(4),
            );
            page(&mut app, KeyCode::PageDown, KeyEventKind::Press);
            assert_eq!(app.theme_editor.as_ref().unwrap().selected, rows);
            page(&mut app, KeyCode::PageUp, KeyEventKind::Repeat);
            assert_eq!(app.theme_editor.as_ref().unwrap().selected, 0);
            let code = (0..120).map(|i| format!("line {i}\n")).collect::<String>();
            {
                let editor = app.theme_editor.as_mut().unwrap();
                editor.tab = EditorTab::Code;
                editor.code = Editor::new(&code).unwrap();
                editor
                    .code
                    .set_selection(Selection::caret(ByteOffset::ZERO))
                    .unwrap();
            }
            app.frame.height = 18;
            let body = ThemeEditorView::code_area(app.frame).unwrap();
            page(&mut app, KeyCode::PageDown, KeyEventKind::Press);
            let editor = &app.theme_editor.as_ref().unwrap().code;
            assert_eq!(editor.viewport().page_rows, usize::from(body.height));
            let line = editor
                .document()
                .line_of(editor.primary_selection().head)
                .unwrap();
            assert_eq!(line, usize::from(body.height));
            page(&mut app, KeyCode::PageUp, KeyEventKind::Repeat);
            assert_eq!(
                app.theme_editor
                    .as_ref()
                    .unwrap()
                    .code
                    .primary_selection()
                    .head,
                ByteOffset::ZERO
            );
            assert_eq!(app.theme_editor.as_ref().unwrap().code.source(), code);
            assert_eq!(app.editor().source(), source);
            assert_eq!(app.focus, Focus::Panel(PanelKind::ThemeEditor));
        }

        #[test]
        fn deliberately_rebound_page_key_keeps_its_configured_action() {
            let directory = tempfile::tempdir().unwrap();
            let mut app = app(directory.path());
            app.keybinds.learn(
                BindAction::Settings,
                Some(crate::keybinds::KeyCombo {
                    code: KeyCode::PageDown,
                    control: false,
                    shift: false,
                }),
            );
            page(&mut app, KeyCode::PageDown, KeyEventKind::Press);
            assert!(app.settings_sheet.is_some());
            assert_eq!(app.focus, Focus::Panel(PanelKind::Settings));
        }

        #[test]
        fn editor_paging_routes_press_repeat_and_shift_selection_to_the_visible_score() {
            let directory = tempfile::tempdir().unwrap();
            let mut app = app(directory.path());
            let source = (0..80).map(|i| format!("line {i}\n")).collect::<String>();
            app.scenes.current_mut().editor = Editor::new(&source).unwrap();
            app.scenes.current_mut().editor.set_view_size(40, 6);
            app.scenes
                .current_mut()
                .editor
                .set_selection(Selection::caret(ByteOffset::ZERO))
                .unwrap();
            page(&mut app, KeyCode::PageDown, KeyEventKind::Press);
            let editor = app.editor();
            assert_eq!(
                editor
                    .document()
                    .line_of(editor.primary_selection().head)
                    .unwrap(),
                6
            );
            assert_eq!(editor.viewport().top_row, 6);
            page(&mut app, KeyCode::PageDown, KeyEventKind::Repeat);
            let anchor = app.editor().primary_selection().head;
            assert_eq!(app.editor().document().line_of(anchor).unwrap(), 12);
            assert_eq!(app.editor().viewport().top_row, 12);
            app.handle_terminal_event(Event::Key(KeyEvent::new(
                KeyCode::PageUp,
                KeyModifiers::SHIFT,
            )))
            .unwrap();
            let selection = app.editor().primary_selection();
            assert_eq!(selection.anchor, anchor);
            assert_eq!(app.editor().document().line_of(selection.head).unwrap(), 6);
            assert_eq!(app.editor().viewport().top_row, 6);
            page(&mut app, KeyCode::PageDown, KeyEventKind::Release);
            assert_eq!(app.editor().primary_selection(), selection);
            assert_eq!(app.editor().source(), source);
            assert_eq!(app.focus, Focus::Editor);
        }
    }

    mod terminal_shortcuts {
        //! Terminal-selected shortcuts through the real Studio event cascade.

        use crate::app::*;
        use crate::keybinds::{KeyCombo, Reach};

        fn app_for(directory: &std::path::Path, terminal: &str, enhanced: bool) -> App {
            let mut app = crate::app::tests::support::app_over(directory);
            app.frame = Rect::new(0, 0, 140, 40);
            app.capabilities = if enhanced {
                KeyboardCapabilities::enhanced()
            } else {
                KeyboardCapabilities::legacy()
            };
            app.focus_pane(0);
            app.keybinds.set_reach(Reach {
                enhanced,
                terminal: terminal.into(),
            });
            app.focus = Focus::Editor;
            app
        }

        fn press(app: &mut App, chord: KeyCombo) {
            let mut modifiers = KeyModifiers::NONE;
            if chord.control {
                modifiers |= KeyModifiers::CONTROL;
            }
            if chord.shift {
                modifiers |= KeyModifiers::SHIFT;
            }
            app.handle_terminal_event(Event::Key(KeyEvent::new(chord.code, modifiers)))
                .unwrap();
        }

        fn action(app: &mut App, action: BindAction) {
            let chord = app.keybinds.binding(action).expect("reachable default");
            press(app, chord);
        }

        #[test]
        fn apple_terminal_decoded_keys_reach_the_advertised_actions() {
            let _platform = crate::terminal::profiles::ForcePlatformForTest::set("macos");
            let directory = tempfile::tempdir().unwrap();
            let mut app = app_for(directory.path(), "Apple Terminal", false);
            let scene = app.scenes.current().id;
            app.lint.insert(
                scene,
                LintResult {
                    scene,
                    revision: app.editor().revision(),
                    input_channels: app.lint_input_channels(),
                    diagnostics: vec![],
                },
            );
            app.lint_pending = false;
            let source = app.editor().source();
            // Ctrl+backslash is byte 0x1c, decoded by Crossterm as Ctrl+4.
            press(&mut app, KeyCombo::parse("ctrl+4").unwrap());
            assert!(
                app.status.starts_with("no errors · cursor:"),
                "{}",
                app.status
            );
            assert_eq!(app.editor().source(), source);
            // Shift+F6 and Shift+F8 now have distinct decoded reports.
            assert_eq!(
                app.keybinds.binding(BindAction::OpenSet).unwrap().key(),
                "shift+f6"
            );
            press(&mut app, KeyCombo::parse("f14").unwrap());
            assert!(matches!(app.set_prompt, Some((SetPrompt::OpenSet, _))));
            app.set_prompt = None;
            app.scenes.learn_pad(Pad {
                note: 60,
                channel: 0,
            });
            assert_eq!(
                app.keybinds.binding(BindAction::ForgetPad).unwrap().key(),
                "shift+f8"
            );
            let before_stop = app.stop_requested;
            press(&mut app, KeyCombo::parse("f16").unwrap());
            assert!(app.scenes.current().pad.is_none());
            assert_eq!(app.stop_requested, before_stop);
            // Terminal sends Shift+F12 as F20.
            assert_eq!(
                app.keybinds.binding(BindAction::Export).unwrap().key(),
                "shift+f12"
            );
            press(&mut app, KeyCombo::parse("f20").unwrap());
            assert!(app.export_sheet.is_some());
        }

        #[test]
        fn legacy_open_export_rewind_and_comment_use_the_advertised_fallbacks() {
            for command in [
                BindAction::OpenSet,
                BindAction::Export,
                BindAction::SceneRewind,
                BindAction::ToggleComment,
                BindAction::RewindEvaluate,
            ] {
                let directory = tempfile::tempdir().unwrap();
                let mut app = app_for(directory.path(), "xterm-256color", false);
                let chord = app.keybinds.binding(command).unwrap();
                assert_ne!(chord, command.default_binding(), "{command:?} should adapt");
                assert_eq!(
                    app.keybinds.action_for(&KeyEvent::new(
                        chord.code,
                        (if chord.control {
                            KeyModifiers::CONTROL
                        } else {
                            KeyModifiers::NONE
                        }) | (if chord.shift {
                            KeyModifiers::SHIFT
                        } else {
                            KeyModifiers::NONE
                        })
                    )),
                    Some(command)
                );
                let rewind = app.scenes.current().rewind;
                if command == BindAction::RewindEvaluate {
                    // The default `bd` scene has no sample in an isolated catalogue;
                    // use a built-in synth so this assertion measures the shortcut.
                    app.scenes.current_mut().editor =
                        Editor::new("$: note('c4').s('sine')\n").expect("valid synth score");
                }
                action(&mut app, command);
                match command {
                    BindAction::OpenSet => {
                        assert!(matches!(app.set_prompt, Some((SetPrompt::OpenSet, _))))
                    }
                    BindAction::Export => assert!(app.export_sheet.is_some()),
                    BindAction::SceneRewind => assert_eq!(app.scenes.current().rewind, !rewind),
                    BindAction::ToggleComment => assert!(app.editor().source().starts_with("//")),
                    BindAction::RewindEvaluate => {
                        assert!(app.pending_evaluation.is_some() || !app.inflight.is_empty())
                    }
                    _ => unreachable!(),
                }
            }
        }

        #[test]
        fn shifted_f8_forgets_the_pad_without_stopping_playback() {
            let directory = tempfile::tempdir().unwrap();
            let mut app = app_for(directory.path(), "xterm-256color", false);
            app.scenes.learn_pad(Pad {
                note: 60,
                channel: 0,
            });
            app.snapshot = Some(StudioSnapshot {
                playing: true,
                ..Default::default()
            });
            app.audible_scene = Some(app.scenes.current().id);
            let before_stop = app.stop_requested;
            let chord = app.keybinds.binding(BindAction::ForgetPad).unwrap();
            assert_eq!(chord.key(), "shift+f8");
            assert_eq!(
                transport_key(
                    &KeyEvent::new(KeyCode::F(8), KeyModifiers::SHIFT),
                    app.capabilities,
                    &app.keybinds
                ),
                None
            );
            press(&mut app, chord);
            assert!(app.scenes.current().pad.is_none());
            assert_eq!(app.stop_requested, before_stop);
            assert!(app.snapshot.as_ref().unwrap().playing);
            assert!(app.status.contains("no longer launches"), "{}", app.status);
        }

        #[test]
        fn terminal_volume_fallbacks_change_gain_without_toggling_zen_or_jobs() {
            for terminal in ["Windows Terminal", "WezTerm 20240203"] {
                let directory = tempfile::tempdir().unwrap();
                let mut app = app_for(directory.path(), terminal, true);
                let up = app.keybinds.binding(BindAction::MasterUp).unwrap();
                assert_ne!(up, BindAction::MasterUp.default_binding());
                let before = app.master.gain_db();
                let zen = app.ui_settings.zen;
                action(&mut app, BindAction::MasterDown);
                assert!(app.master.gain_db() < before);
                action(&mut app, BindAction::MasterUp);
                assert_eq!(app.master.gain_db(), before);
                assert_eq!(app.ui_settings.zen, zen);
                assert!(app.jobs_panel.is_none());
            }
        }

        #[test]
        fn recording_and_duplication_have_effective_configurable_shortcuts() {
            let directory = tempfile::tempdir().unwrap();
            let mut app = app_for(directory.path(), "xterm-256color", false);
            let record = app.keybinds.binding(BindAction::RecordTake).unwrap();
            assert_ne!(record, BindAction::RecordTake.default_binding());
            press(&mut app, record);
            assert!(app.pending_recording.is_some(), "{}", app.status);
            let count = app.scenes.len();
            let source = app.editor().source();
            action(&mut app, BindAction::DuplicateScene);
            assert_eq!(app.scenes.len(), count + 1);
            assert_eq!(app.editor().source(), source);
            app.keybinds.learn(
                BindAction::DuplicateScene,
                Some(KeyCombo::parse("ctrl+f9").unwrap()),
            );
            press(&mut app, KeyCombo::parse("f3").unwrap());
            assert_eq!(app.scenes.len(), count + 1, "old duplicate chord retired");
            action(&mut app, BindAction::DuplicateScene);
            assert_eq!(app.scenes.len(), count + 2);
        }

        #[test]
        fn retiring_or_taking_a_fallback_removes_its_old_action_from_the_cascade() {
            let directory = tempfile::tempdir().unwrap();
            let mut app = app_for(directory.path(), "xterm-256color", false);
            let original = app.editor().source();
            app.dispatch_editor(Command::InsertText("edited ".into()))
                .unwrap();
            action(&mut app, BindAction::Undo);
            assert_eq!(app.editor().source(), original);
            app.keybinds.unbind(BindAction::Redo);
            press(&mut app, KeyCombo::parse("ctrl+y").unwrap());
            assert_eq!(
                app.editor().source(),
                original,
                "retired redo cannot run through editor fallback"
            );

            let fallback = app.keybinds.binding(BindAction::OpenSet).unwrap();
            app.keybinds.learn(BindAction::Undo, Some(fallback));
            app.dispatch_editor(Command::InsertText("second edit ".into()))
                .unwrap();
            press(&mut app, fallback);
            assert_eq!(app.editor().source(), original);
            assert!(
                app.set_prompt.is_none(),
                "taking a fallback displaces its old action"
            );
            assert!(app.keybinds.is_unbound(BindAction::OpenSet));
        }

        #[test]
        fn native_windows_modifier_support_matches_the_keybinding_profile() {
            let directory = tempfile::tempdir().unwrap();
            let mut app = app_for(directory.path(), "unknown", false);
            app.adopt_terminal(TerminalFeatures {
                name: "conhost".into(),
                keyboard: false,
                ..Default::default()
            });
            let rewind = app.keybinds.binding(BindAction::RewindEvaluate).unwrap();
            assert_eq!(
                rewind == BindAction::RewindEvaluate.default_binding(),
                cfg!(windows)
            );
        }

        #[test]
        fn settings_alternate_keys_follow_the_effective_terminal_profile() {
            let directory = tempfile::tempdir().unwrap();
            let mut app = app_for(directory.path(), "Windows Terminal", true);
            let rows = app.keybind_rows();
            let log = rows
                .iter()
                .find(|row| row.action == BindAction::Log.label())
                .unwrap();
            assert!(
                log.also.is_empty(),
                "Windows Terminal consumes Ctrl+Shift+D"
            );
            let mixer = rows
                .iter()
                .find(|row| row.action == BindAction::Mixer.label())
                .unwrap();
            assert!(
                mixer.also.is_empty(),
                "Windows Terminal consumes Ctrl+Shift+M"
            );
            app.keybinds
                .learn(BindAction::Undo, Some(KeyCombo::parse("f5").unwrap()));
            let rows = app.keybind_rows();
            let evaluate = rows
                .iter()
                .find(|row| row.action == BindAction::Evaluate.label())
                .unwrap();
            assert!(
                evaluate.also.is_empty(),
                "an alias taken by Undo must disappear"
            );
        }

        /// The key page lists the values chord and the docs chord on two rows.
        /// The values row names Ctrl+Space only where the terminal sends it.
        #[test]
        fn the_key_page_lists_values_and_docs_on_two_rows() {
            use crate::terminal::conflicts;
            let _desktop = conflicts::ForceDesktopForTest::set("");
            for (platform, also) in [("linux", "^Space"), ("macos", "")] {
                let _platform = conflicts::ForcePlatformForTest::set(platform);
                let directory = tempfile::tempdir().unwrap();
                let app = app_for(directory.path(), "kitty", true);
                let rows = app.keybind_rows();
                let row = |label: &str| {
                    rows.iter()
                        .find(|row| row.action == label)
                        .unwrap_or_else(|| panic!("{platform}: no row reads {label:?}"))
                };
                let values = row("argument values, reference");
                assert_eq!(
                    (values.chord.as_str(), values.also.as_str()),
                    ("^F", also),
                    "{platform}"
                );
                let docs = row("docs for the function");
                assert_eq!(
                    (docs.chord.as_str(), docs.also.as_str()),
                    ("^D", ""),
                    "{platform}"
                );
                assert!(rows.iter().all(|row| row.also != "^D"), "{platform}");
            }
        }

        #[test]
        fn menu_and_help_follow_their_effective_keys_even_when_already_open() {
            let directory = tempfile::tempdir().unwrap();
            let mut app = app_for(directory.path(), "Konsole", false);
            let chord = KeyCombo::parse("ctrl+f9").unwrap();
            app.keybinds.learn(BindAction::MenuBar, Some(chord));
            press(&mut app, chord);
            assert!(app.menu.is_some());
            press(&mut app, chord);
            assert!(app.menu.is_none());
            // Moving only MenuBar leaves the shared old F1 to Help.
            press(&mut app, KeyCombo::parse("f1").unwrap());
            assert!(app.help.is_some());
            assert!(app.menu.is_none());
            press(&mut app, chord);
            assert!(app.help.is_none());
            let help = KeyCombo::parse("ctrl+f8").unwrap();
            app.keybinds.learn(BindAction::Help, Some(help));
            press(&mut app, help);
            assert!(app.help.is_some());
            press(&mut app, help);
            assert!(app.help.is_none());
        }

        #[test]
        fn a_rebound_quit_key_can_confirm_its_own_first_press() {
            let directory = tempfile::tempdir().unwrap();
            let mut app = app_for(directory.path(), "Konsole", false);
            let chord = KeyCombo::parse("ctrl+f8").unwrap();
            app.keybinds.learn(BindAction::Quit, Some(chord));
            press(&mut app, chord);
            assert!(app.quit_armed);
            press(&mut app, chord);
            assert!(
                app.quit,
                "the effective quit chord must not cancel its own confirmation"
            );
        }

        #[test]
        fn unbound_compatibility_shortcuts_cannot_resurrect_their_actions() {
            for (command, chord, enhanced) in [
                (BindAction::Evaluate, "ctrl+enter", true),
                (BindAction::Stop, "ctrl+.", true),
                (BindAction::Reference, "f2", true),
                (BindAction::Reference, "ctrl+space", false),
                (BindAction::Settings, "ctrl+shift+p", true),
                (BindAction::HopPane, "ctrl+shift+e", true),
                (BindAction::Zen, "f11", true),
                (BindAction::ToggleComment, "ctrl+_", false),
            ] {
                let directory = tempfile::tempdir().unwrap();
                let mut app = app_for(directory.path(), "unknown", enhanced);
                let before = app.editor().source();
                let zen = app.ui_settings.zen;
                app.keybinds.unbind(command);
                press(&mut app, KeyCombo::parse(chord).unwrap());
                assert_eq!(app.editor().source(), before, "{command:?}: {chord}");
                assert!(
                    app.pending_evaluation.is_none() && app.inflight.is_empty(),
                    "{chord}"
                );
                assert!(!app.stop_requested, "{chord}");
                assert!(app.reference_panel.is_none(), "{chord}");
                assert!(app.settings_sheet.is_none(), "{chord}");
                assert_eq!(app.ui_settings.zen, zen, "{chord}");
            }
        }

        #[test]
        fn extra_modifiers_cannot_bypass_retired_shortcuts() {
            let directory = tempfile::tempdir().unwrap();
            let mut app = app_for(directory.path(), "unknown", true);
            let source = app.editor().source();
            for command in [
                BindAction::Quit,
                BindAction::Reference,
                BindAction::Stop,
                BindAction::Evaluate,
                BindAction::Redo,
            ] {
                app.keybinds.unbind(command);
            }
            for chord in [
                "ctrl+shift+q",
                "ctrl+shift+f",
                "ctrl+shift+space",
                "ctrl+shift+g",
                "ctrl+shift+y",
            ] {
                press(&mut app, KeyCombo::parse(chord).unwrap());
                assert!(!app.quit_armed && !app.quit, "{chord}");
                assert!(!app.stop_requested, "{chord}");
                assert!(app.reference_panel.is_none(), "{chord}");
                assert_eq!(app.editor().source(), source, "{chord}");
            }
        }

        #[test]
        fn unbinding_scene_navigation_retires_shifted_bracket_spellings() {
            let directory = tempfile::tempdir().unwrap();
            let mut app = app_for(directory.path(), "unknown", true);
            app.new_scene(false);
            app.new_scene(false);
            let scene = app.scenes.current_index();
            for command in [BindAction::PreviousScene, BindAction::NextScene] {
                app.keybinds.unbind(command);
            }
            for chord in [
                "ctrl+[",
                "ctrl+]",
                "ctrl+shift+[",
                "ctrl+shift+]",
                "ctrl+{",
                "ctrl+}",
                "f6",
                "f7",
            ] {
                press(&mut app, KeyCombo::parse(chord).unwrap());
                assert_eq!(app.scenes.current_index(), scene, "{chord}");
            }
        }

        #[test]
        fn adapted_shortcuts_respect_rename_and_prompt_owners() {
            let directory = tempfile::tempdir().unwrap();
            let mut app = app_for(directory.path(), "unknown", false);
            app.new_scene(false);
            let scene = app.scenes.current_index();
            app.begin_rename();
            let rename = format!("{:?}", app.strip_mode);
            action(&mut app, BindAction::PreviousScene);
            assert_eq!(app.scenes.current_index(), scene);
            assert_eq!(format!("{:?}", app.strip_mode), rename);
            app.strip_mode = SceneStripMode::Idle;
            app.dispatch_editor(Command::InsertText("edited ".into()))
                .unwrap();
            action(&mut app, BindAction::Undo);
            let source = app.editor().source();
            app.open_set_prompt(SetPrompt::OpenSet);
            for command in [
                BindAction::Redo,
                BindAction::ToggleComment,
                BindAction::NextScene,
            ] {
                action(&mut app, command);
                assert_eq!(app.editor().source(), source, "{command:?}");
                assert_eq!(app.scenes.current_index(), scene, "{command:?}");
                assert!(app.set_prompt.is_some(), "{command:?}");
            }
        }

        #[test]
        fn terminal_profile_switches_live_and_preserves_user_bindings_and_capabilities() {
            use crate::settings::SettingsPage;
            let directory = tempfile::tempdir().unwrap();
            let mut app = app_for(directory.path(), "unknown", true);
            app.features = TerminalFeatures {
                name: "unrecognized terminal".into(),
                keyboard: false,
                ..Default::default()
            };
            app.keybinds
                .learn(BindAction::Undo, KeyCombo::parse("ctrl+shift+q"));
            let saved = app.keybinds.prefs();
            let mut sheet = SettingsSheet::default();
            sheet.show_page(SettingsPage::Keybinds); // Terminal profile is the first row.
            app.settings_sheet = Some(sheet);
            app.focus = Focus::Panel(PanelKind::Settings);
            press(&mut app, KeyCombo::parse("right").unwrap());
            assert!(
                app.keybind_learn.is_none(),
                "the terminal row cannot arm Evaluate"
            );
            assert_eq!(
                app.ui_settings.terminal_profile.as_deref(),
                Some("Windows Terminal")
            );
            assert_eq!(
                app.prefs.ui_settings().terminal_profile,
                app.ui_settings.terminal_profile
            );
            assert_eq!(
                app.keybinds.binding(BindAction::MasterUp).unwrap().key(),
                "ctrl+f12"
            );
            assert_eq!(
                app.keybinds.binding(BindAction::MasterDown).unwrap().key(),
                "ctrl+f11"
            );
            assert_eq!(
                app.keybinds.prefs(),
                saved,
                "selecting a profile keeps custom bindings"
            );
            assert_eq!(app.features.name, "unrecognized terminal");
            assert!(!app.features.keyboard);
            press(&mut app, KeyCombo::parse("left").unwrap());
            assert!(app.ui_settings.terminal_profile.is_none());
            assert!(app.prefs.terminal_profile.is_none());
            assert_eq!(
                app.keybinds.binding(BindAction::MasterUp),
                Some(BindAction::MasterUp.default_binding())
            );
            assert_eq!(app.keybinds.prefs(), saved);
        }

        #[test]
        fn reset_all_confirms_and_restores_the_selected_or_automatic_terminal_defaults() {
            use crate::settings::{RESET_KEYBINDS_ROW, SettingsPage};
            for profile in [None, Some("Windows Terminal")] {
                let directory = tempfile::tempdir().unwrap();
                let mut app = app_for(directory.path(), "unknown", true);
                app.features = TerminalFeatures {
                    name: "kitty".into(),
                    keyboard: true,
                    ..Default::default()
                };
                app.ui_settings.terminal_profile = profile.map(str::to_owned);
                app.apply_shortcut_profile();
                let expected = app.keybinds.clone();
                app.keybinds
                    .learn(BindAction::Undo, KeyCombo::parse("ctrl+shift+q"));
                app.keybinds.unbind(BindAction::Stop);
                let changed = app.keybinds.prefs();
                app.prefs.set_keybinds(&app.keybinds);
                let mut sheet = SettingsSheet::default();
                sheet.show_page(SettingsPage::Keybinds);
                sheet.selected = RESET_KEYBINDS_ROW;
                app.settings_sheet = Some(sheet);
                app.focus = Focus::Panel(PanelKind::Settings);
                press(&mut app, KeyCombo::parse("enter").unwrap());
                assert!(app.settings_sheet.unwrap().confirm_reset_keybinds);
                assert_eq!(app.keybinds.prefs(), changed);
                press(&mut app, KeyCombo::parse("esc").unwrap());
                assert!(app.settings_sheet.is_some());
                assert!(!app.settings_sheet.unwrap().confirm_reset_keybinds);
                assert_eq!(app.keybinds.prefs(), changed);
                press(&mut app, KeyCombo::parse("enter").unwrap());
                press(&mut app, KeyCombo::parse("enter").unwrap());
                assert_eq!(app.keybinds, expected);
                assert!(app.prefs.keybinds.is_empty());
                assert_eq!(app.ui_settings.terminal_profile.as_deref(), profile);
                assert_eq!(app.settings_sheet.unwrap().selected, RESET_KEYBINDS_ROW);
                assert!(app.keybind_learn.is_none());
                assert_eq!(app.focus, Focus::Panel(PanelKind::Settings));
            }
        }

        #[test]
        fn startup_keeps_saved_profile_separate_from_detected_terminal() {
            let directory = tempfile::tempdir().unwrap();
            let mut app = app_for(directory.path(), "unknown", true);
            app.prefs.terminal_profile = Some("Windows Terminal".into());
            app.adopt_terminal(TerminalFeatures {
                name: "kitty".into(),
                keyboard: false,
                ..Default::default()
            });
            assert_eq!(app.features.name, "kitty");
            assert!(!app.features.keyboard);
            assert_eq!(
                app.ui_settings.terminal_profile.as_deref(),
                Some("Windows Terminal")
            );
            assert_eq!(
                app.keybinds.binding(BindAction::MasterUp).unwrap().key(),
                "ctrl+f12"
            );
            assert_eq!(
                app.keybinds.binding(BindAction::MasterDown).unwrap().key(),
                "ctrl+f11"
            );
        }

        /// A saved profile for another terminal is named on the row and said
        /// at start: in the status line and as one log warning.
        #[test]
        fn a_start_with_a_profile_for_another_terminal_says_so() {
            let directory = tempfile::tempdir().unwrap();
            let mut app = app_for(directory.path(), "unknown", true);
            app.prefs.terminal_profile = Some("Windows Terminal".into());
            let warnings = app.log.at_least(LogLevel::Warn);
            app.adopt_terminal(TerminalFeatures {
                name: "kitty 0.35.2".into(),
                keyboard: true,
                ..Default::default()
            });
            assert_eq!(
                app.keybind_rows()[0].chord,
                "Windows Terminal (this terminal: kitty 0.35.2)"
            );
            assert!(
                app.status.contains("Windows Terminal")
                    && app.status.contains("this terminal is kitty 0.35.2"),
                "{}",
                app.status
            );
            assert_eq!(app.log.at_least(LogLevel::Warn), warnings + 1);
            assert!(
                app.log
                    .lines()
                    .iter()
                    .any(|line| line.level == LogLevel::Warn && line.text == app.status)
            );
        }

        /// A profile that agrees with the detected terminal, or that names an
        /// unknown one, starts with no warning.
        #[test]
        fn a_start_with_a_fitting_profile_says_nothing() {
            for (profile, detected) in [
                (None, "kitty 0.35.2"),
                (Some("kitty"), "kitty 0.35.2"),
                (Some("kitty"), "tmux 3.5 via kitty"),
                (Some("kitty"), "unknown (xterm-256color)"),
            ] {
                let directory = tempfile::tempdir().unwrap();
                let mut app = app_for(directory.path(), "unknown", true);
                app.prefs.terminal_profile = profile.map(str::to_owned);
                let (warnings, status) = (app.log.at_least(LogLevel::Warn), app.status.clone());
                app.adopt_terminal(TerminalFeatures {
                    name: detected.into(),
                    keyboard: true,
                    ..Default::default()
                });
                assert_eq!(app.log.at_least(LogLevel::Warn), warnings, "{detected}");
                assert_eq!(app.status, status, "{detected}");
                assert!(
                    !app.keybind_rows()[0].chord.contains("this terminal"),
                    "{detected}"
                );
            }
        }

        /// One arrow press on the Terminal row can pin another terminal. The
        /// status line says so, and Delete returns to the detected terminal.
        #[test]
        fn pinning_a_profile_for_another_terminal_says_so_in_the_status_line() {
            use crate::settings::{SettingsPage, TERMINAL_PROFILE_ROW};
            let directory = tempfile::tempdir().unwrap();
            let mut app = app_for(directory.path(), "unknown", true);
            app.adopt_terminal(TerminalFeatures {
                name: "kitty 0.35.2".into(),
                keyboard: true,
                ..Default::default()
            });
            let mut sheet = SettingsSheet::default();
            sheet.show_page(SettingsPage::Keybinds);
            sheet.selected = TERMINAL_PROFILE_ROW;
            app.settings_sheet = Some(sheet);
            app.focus = Focus::Panel(PanelKind::Settings);
            press(&mut app, KeyCombo::parse("right").unwrap());
            let pinned = app.ui_settings.terminal_profile.clone().unwrap();
            assert_ne!(pinned, "kitty");
            assert!(
                app.status.contains(&pinned)
                    && app.status.contains("this terminal is kitty 0.35.2"),
                "{}",
                app.status
            );
            assert_eq!(
                app.keybind_rows()[0].chord,
                format!("{pinned} (this terminal: kitty 0.35.2)")
            );
            press(&mut app, KeyCombo::parse("del").unwrap());
            assert_eq!(
                app.status,
                "shortcut profile: automatic (kitty 0.35.2) - custom bindings kept"
            );
            assert_eq!(app.keybind_rows()[0].chord, "automatic (kitty 0.35.2)");
        }

        #[test]
        fn manual_terminal_profiles_and_reset_keep_active_multiplexer_prefixes_reserved() {
            use crate::settings::{RESET_KEYBINDS_ROW, SettingsPage};
            for (multiplexer, prefix, command) in [
                ("tmux", "ctrl+b", BindAction::SetPanel),
                ("screen", "ctrl+a", BindAction::SelectAll),
            ] {
                let directory = tempfile::tempdir().unwrap();
                let mut app = app_for(directory.path(), "unknown", true);
                let detected = format!("{multiplexer} via unknown");
                app.features = TerminalFeatures {
                    name: detected.clone(),
                    keyboard: true,
                    ..Default::default()
                };
                app.ui_settings.terminal_profile = Some("kitty".into());
                app.apply_shortcut_profile();
                let adapted = app.keybinds.binding(command).unwrap();
                assert_ne!(adapted.key(), prefix);
                assert_eq!(
                    app.shortcut_profile_label(),
                    format!("{multiplexer} via kitty")
                );
                app.keybinds.learn(command, KeyCombo::parse(prefix));
                let mut sheet = SettingsSheet::default();
                sheet.show_page(SettingsPage::Keybinds);
                sheet.selected = RESET_KEYBINDS_ROW;
                app.settings_sheet = Some(sheet);
                app.focus = Focus::Panel(PanelKind::Settings);
                press(&mut app, KeyCombo::parse("enter").unwrap());
                press(&mut app, KeyCombo::parse("enter").unwrap());
                assert_eq!(app.keybinds.binding(command), Some(adapted));
                assert_eq!(app.features.name, detected);
                assert_eq!(app.ui_settings.terminal_profile.as_deref(), Some("kitty"));
                assert!(app.keybinds.prefs().is_empty());
            }
        }

        /// iTerm2 with Option as Normal composes Alt+O into `ø` and sends
        /// no Alt bit. The composed character still reveals the set panel's
        /// selected file, because the terminal profile recovers the letter.
        /// A chord learnt over the action retires the letter again, and the
        /// character then reaches no panel.
        #[test]
        fn an_iterm2_composed_option_o_still_reveals_the_set_panels_selection() {
            // SAFETY: process-global environment, held only by this test
            // while the studio suite runs one test at a time. The reveal
            // must answer headless, or the press opens the runner's desktop.
            unsafe { std::env::set_var(crate::reveal::HEADLESS_ENV, "1") };
            let directory = tempfile::tempdir().unwrap();
            let mut app = app_for(directory.path(), "iTerm2", false);
            action(&mut app, BindAction::SetPanel);
            assert_eq!(app.focus, Focus::Panel(PanelKind::Set));
            assert!(app.set_prompt.is_none(), "{}", app.status);
            app.status.clear();
            app.handle_terminal_event(Event::Key(KeyEvent::new(
                KeyCode::Char('ø'),
                KeyModifiers::NONE,
            )))
            .unwrap();
            assert!(app.reveal_pending.is_some(), "{}", app.status);
            assert!(app.status.starts_with("opening"), "{}", app.status);
            // The letter retires with a learnt chord, so `ø` types again.
            app.reveal_pending = None;
            app.status.clear();
            app.keybinds
                .learn(BindAction::ShowFile, KeyCombo::parse("ctrl+f9"));
            app.handle_terminal_event(Event::Key(KeyEvent::new(
                KeyCode::Char('ø'),
                KeyModifiers::NONE,
            )))
            .unwrap();
            assert!(app.reveal_pending.is_none(), "{}", app.status);
            // SAFETY: give the environment back before other tests run.
            unsafe { std::env::remove_var(crate::reveal::HEADLESS_ENV) };
        }

        /// A terminal that carries the keyboard protocol composes Alt+O
        /// into `ø` as well, but reports the Alt bit beside it. The chord
        /// still reveals the set panel's selected file, with no terminal
        /// table behind it.
        #[test]
        fn a_protocols_composed_option_o_still_reveals_the_set_panels_selection() {
            // SAFETY: process-global environment, held only by this test
            // while the studio suite runs one test at a time.
            unsafe { std::env::set_var(crate::reveal::HEADLESS_ENV, "1") };
            let directory = tempfile::tempdir().unwrap();
            let mut app = app_for(directory.path(), "Ghostty", true);
            action(&mut app, BindAction::SetPanel);
            assert_eq!(app.focus, Focus::Panel(PanelKind::Set));
            assert!(app.set_prompt.is_none(), "{}", app.status);
            app.status.clear();
            app.handle_terminal_event(Event::KeyWithText {
                key: KeyEvent::new(KeyCode::Char('o'), KeyModifiers::ALT),
                text: "ø".into(),
            })
            .unwrap();
            assert!(app.reveal_pending.is_some(), "{}", app.status);
            assert!(app.status.starts_with("opening"), "{}", app.status);
            // SAFETY: give the environment back before other tests run.
            unsafe { std::env::remove_var(crate::reveal::HEADLESS_ENV) };
        }
    }
}
