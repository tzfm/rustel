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
/// These two are the keys a set cannot afford to lose. Whatever is on screen -
/// the reference column, a picker, the settings sheet - the player has to be
/// able to push a change out and to cut the transport, so they are recognised
/// before any panel is offered the key.
///
/// The keybinding table rides along: a chord learnt onto update or stop
/// claims the press exactly as the built-in spelling would, so a moved
/// transport keeps working from anywhere. The table is asked for the two
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
        match kind {
            PanelKind::Set => self.set_prompt.is_some(),
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
            PanelKind::Set
                if code == KeyCode::Char('n')
                    && !primary
                    && !shift
                    && !alt
                    && self.set_prompt.is_none() =>
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
        })
    }

    /// Every terminal event, offered to its owners in priority order. The
    /// order is the behaviour - which surface is on top, which key wins -
    /// so each step sees the event only when every step above it has let
    /// it go, and the first to claim it ends the turn.
    pub(super) fn handle_terminal_event(
        &mut self,
        mut terminal_event: Event,
    ) -> Result<(), RuntimeError> {
        if let Event::Key(key) = &mut terminal_event {
            self.keybinds.normalize_terminal_key(key);
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
        // Ctrl+Q is the one unconditional way out. Claim it before keybind
        // learning and every modal surface so no menu, prompt or remapped
        // action can make the studio impossible to quit from the keyboard.
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
        if is_stray_control_key(&terminal_event) {
            return Ok(());
        }
        let Some(terminal_event) = without_stray_controls(terminal_event) else {
            return Ok(());
        };
        self.latch_keyboard_capabilities(&terminal_event);
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
        if self.paste_shortcut(key, alt) {
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
    /// sheets, the split, wrap and zen, and the smart action. `true` when
    /// one answered.
    fn surface_chord(
        &mut self,
        key: &KeyEvent,
        primary: bool,
        shift: bool,
        alt: bool,
        letter: Option<char>,
    ) -> bool {
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
        // The visuals docks have no chord, only View ▸ Visuals 1 and 2:
        // nobody shows and hides one mid-set, and the chord the second
        // had, Ctrl+Shift+V, is Windows Terminal's paste.
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
        // The smart action. ^J is a plain 0x0A byte: every terminal
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
