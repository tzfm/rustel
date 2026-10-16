//! Computer-piano input with a compact footer indicator, never score text.

use super::*;
use crate::piano::{Piano, PianoNote, note_label};

pub(super) const TIMED_NOTE: Duration = Duration::from_millis(600);
const RELEASE_RETRY: Duration = Duration::from_millis(10);
const PULSE_PERIOD: Duration = Duration::from_secs(2);
pub(super) const NOTES_RETENTION: Duration = Duration::from_secs(60);

#[derive(Default)]
pub(super) struct PianoMode {
    pub open: bool,
    pub keyboard: Piano,
    pub key_releases: bool,
    show_octave: bool,
    pub(super) pulse_started: Option<Instant>,
    pulse: f32,
    pub(super) notes_until: Option<Instant>,
    deadlines: [Option<Instant>; 15],
    pending_off: u16,
    pub(super) pending_on: [Option<PianoNote>; 15],
    pending_stop: bool,
    pending_settings: bool,
    muted_until_up: u16,
}

impl App {
    pub(super) fn toggle_piano_mode(&mut self) {
        if self.piano.open {
            self.close_piano_mode();
            return;
        }
        // Keep the current screen and focus. Only note keys change meaning.
        self.piano.open = true;
        self.piano.show_octave = false;
        self.piano.notes_until = None;
        self.piano.keyboard.forget_notes();
        self.configure_piano();
        self.piano.pulse_started = Some(Instant::now());
        self.piano.pulse = 1.0;
        self.dirty_frame = true;
    }

    pub(super) fn close_piano_mode(&mut self) {
        if self.piano.open {
            self.piano.open = false;
            // The engine closes Piano mode with these settings. A Stop or a
            // focus loss sends no settings, and Piano mode stays open.
            self.piano.pending_settings = true;
            self.silence_piano();
            self.piano.pulse_started = None;
            self.piano.notes_until =
                (!self.piano.keyboard.notes().is_empty()).then(|| Instant::now() + NOTES_RETENTION);
            self.dirty_frame = true;
        }
    }

    pub(super) fn silence_piano(&mut self) {
        for key in self.piano.keyboard.release_all() {
            if self.piano.key_releases {
                self.piano.muted_until_up |= 1 << key;
            }
        }
        self.piano.deadlines.fill(None);
        self.piano.pending_on.fill(None);
        self.piano.pending_off = 0;
        self.piano.pending_stop = true;
        self.flush_piano_input();
        self.dirty_frame = true;
    }

    pub(super) fn configure_piano(&mut self) {
        self.piano.pending_settings = true;
        self.flush_piano_input();
    }

    fn flush_piano_input(&mut self) {
        // A full command queue must not strand a held note. A stop is a
        // barrier: no new note may overtake it when the mode is reopened.
        if self.piano.pending_stop {
            if !self.worker.try_stop_piano() {
                return;
            }
            self.piano.pending_stop = false;
        }
        if self.piano.pending_settings {
            if !self.worker.try_configure_piano(
                self.ui_settings.piano_sound,
                self.ui_settings.piano_volume,
                self.piano.open,
            ) {
                return;
            }
            self.piano.pending_settings = false;
        }
        for key in 0..15 {
            let bit = 1 << key;
            if self.piano.pending_off & bit != 0 && self.worker.try_piano_note_off(key) {
                self.piano.pending_off &= !bit;
            }
        }
        for (slot, pending) in self.piano.pending_on.iter_mut().enumerate() {
            if let Some(note) = pending
                && self.piano.pending_off & (1 << slot) == 0
                && self.worker.try_piano_note_on(note.key, note.note, 100)
            {
                *pending = None;
            }
        }
    }

    fn release_piano_key(&mut self, key: u8) {
        if self.piano.keyboard.release_key(key).is_some() {
            self.piano.deadlines[usize::from(key)] = None;
            self.piano.pending_on[usize::from(key)] = None;
            self.piano.pending_off |= 1 << key;
            self.flush_piano_input();
            self.dirty_frame = true;
        }
    }

    pub(super) fn pump_piano(&mut self, now: Instant) {
        if self.piano.notes_until.is_some_and(|until| now >= until) {
            self.piano.notes_until = None;
            self.piano.keyboard.forget_notes();
            self.dirty_frame = true;
        }
        if let Some(started) = self.piano.pulse_started {
            let elapsed = now.saturating_duration_since(started).as_secs_f64();
            let phase = (elapsed % PULSE_PERIOD.as_secs_f64()) / PULSE_PERIOD.as_secs_f64();
            // Ease gently at both ends instead of abruptly switching colours.
            let pulse = ((phase * std::f64::consts::TAU).cos() * 0.5 + 0.5) as f32;
            if pulse != self.piano.pulse {
                self.piano.pulse = pulse;
                self.dirty_frame = true;
            }
        }
        for key in 0..15 {
            if self.piano.deadlines[usize::from(key)].is_some_and(|until| now >= until) {
                self.release_piano_key(key);
            }
        }
        self.flush_piano_input();
    }

    pub(super) fn piano_wait(&self, wait: Duration, now: Instant) -> Duration {
        let wait = if self.piano.pending_settings
            || self.piano.pending_stop
            || self.piano.pending_off != 0
            || self.piano.pending_on.iter().any(Option::is_some)
        {
            wait.min(RELEASE_RETRY)
        } else {
            wait
        };
        self.piano
            .deadlines
            .iter()
            .flatten()
            .chain(self.piano.notes_until.as_ref())
            .map(|until| until.saturating_duration_since(now))
            .fold(wait, Duration::min)
    }

    pub(super) fn piano_status(&self) -> String {
        if self.piano.show_octave {
            let octave = i16::from(self.piano.keyboard.base_note() / 12) - 1;
            return format!("PIANO  Octave {octave}");
        }
        let names = self
            .piano
            .keyboard
            .notes()
            .into_iter()
            .map(note_label)
            .collect::<Vec<_>>()
            .join(" + ");
        if names.is_empty() {
            "PIANO  A S D F G H J K L notes · Z/X octave".into()
        } else {
            format!("PIANO  {names}")
        }
    }

    pub(super) fn piano_pulse(&self) -> Option<f32> {
        self.piano.open.then_some(self.piano.pulse)
    }

    pub(super) fn has_piano_notes(&self) -> bool {
        self.piano.notes_until.is_some()
    }

    pub(super) fn retained_piano_notes(&self) -> Option<String> {
        self.has_piano_notes().then(|| {
            let notes = self.piano.keyboard.notes();
            let names = notes
                .iter()
                .copied()
                .map(note_label)
                .collect::<Vec<_>>()
                .join(" + ");
            let midi = notes
                .iter()
                .map(u8::to_string)
                .collect::<Vec<_>>()
                .join(",");
            format!("Notes  {names} · [{midi}]")
        })
    }

    pub(super) fn piano_zen_row(&self) -> Option<Rect> {
        ((self.piano.open || self.has_piano_notes())
            && self.regions.footer.is_empty()
            && self.frame.height > 0)
            .then(|| Rect::new(self.frame.x, self.frame.bottom() - 1, self.frame.width, 1))
    }

    pub(super) fn piano_zen_row_at(&self, x: u16, y: u16) -> bool {
        self.piano_zen_row().is_some_and(|row| within(row, x, y))
    }

    pub(super) fn handle_piano_event(&mut self, event: &Event) -> Result<bool, RuntimeError> {
        // A release in another window never reaches us. Start a fresh input
        // epoch on focus gain, matching the native reader's held-key reset.
        if matches!(event, Event::FocusGained) {
            self.piano.muted_until_up = 0;
        }
        // After focus loss, Stop or leaving piano mode, a still-held key
        // must not restart from OS repeat. Its real key-up re-arms it.
        if let Event::Key(key) = event
            && key.kind == KeyEventKind::Release
            && let KeyCode::Char(character) = key.code
            && let Some(slot) = Piano::key_for_char(character)
        {
            self.piano.muted_until_up &= !(1 << slot);
        }
        if let Event::Key(key) = event
            && key.kind == KeyEventKind::Press
            && !key.modifiers.contains(KeyModifiers::ALT)
            && self.keybinds.action_for(key) == Some(BindAction::PianoMode)
        {
            self.toggle_piano_mode();
            return Ok(true);
        }
        // Zen overlays one editor row, including while notes are retained.
        if matches!(event, Event::Mouse(mouse)
            if matches!(mouse.kind, MouseEventKind::Down(_))
                && self.piano_zen_row_at(mouse.column, mouse.row))
        {
            return Ok(true);
        }
        if !self.piano.open {
            return Ok(false);
        }
        if matches!(event, Event::FocusLost) {
            self.silence_piano();
            return Ok(false);
        }
        let Event::Key(key) = event else {
            // Keep all visible controls usable; a paste is still text and
            // must not edit the score while the keyboard belongs to notes.
            return Ok(matches!(event, Event::Paste(_)));
        };
        // Modifier state can change before key-up; release by physical
        // letter/slot even when Ctrl or Shift is currently held.
        if key.kind == KeyEventKind::Release {
            if let KeyCode::Char(character) = key.code
                && let Some(slot) = Piano::key_for_char(character)
            {
                self.release_piano_key(slot);
            }
            return Ok(true);
        }
        let plain = !key
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::SUPER | KeyModifiers::ALT);
        if key.kind == KeyEventKind::Press {
            if key.code == KeyCode::Esc && plain {
                self.close_piano_mode();
                return Ok(true);
            }
            if !key.modifiers.contains(KeyModifiers::ALT) {
                let action = self.keybinds.action_for(key);
                if action == Some(BindAction::FirstError) {
                    self.jump_to_first_error();
                    return Ok(true);
                }
                if action == Some(BindAction::Quit) {
                    self.close_piano_mode();
                    self.request_quit();
                    return Ok(true);
                }
                if matches!(
                    action,
                    Some(BindAction::Evaluate | BindAction::Stop | BindAction::RewindEvaluate)
                ) {
                    self.dispatch_bind_action(action.unwrap(), false)?;
                    return Ok(true);
                }
                if let Some(command) = transport_key(key, self.capabilities, &self.keybinds) {
                    self.dispatch_editor(command)?;
                    return Ok(true);
                }
            }
        }
        if !plain {
            return Ok(false);
        }
        // Space is the reference's play/stop control, not a piano note.
        // Let the normal routing deliver it to the focused preview panel.
        if key.kind == KeyEventKind::Press
            && key.code == KeyCode::Char(' ')
            && self.focus == Focus::Panel(PanelKind::Reference)
            && self
                .reference_panel
                .as_ref()
                .is_some_and(|panel| !matches!(panel.preview(), PanelAction::Nothing))
        {
            return Ok(false);
        }
        let KeyCode::Char(character) = key.code else {
            return Ok(false);
        };
        let character = character.to_ascii_lowercase();
        match character {
            'z' | 'x' => {
                self.piano
                    .keyboard
                    .octave(if character == 'x' { 1 } else { -1 });
                self.piano.show_octave = true;
            }
            _ => {
                let Some(slot) = Piano::key_for_char(character) else {
                    return Ok(true);
                };
                if self.piano.muted_until_up & (1 << slot) != 0
                    || (self.piano.key_releases && key.kind == KeyEventKind::Repeat)
                {
                    return Ok(true);
                }
                // Native repeats refresh no voices. With legacy input the
                // same timed voice is extended while repeats keep arriving.
                if !self.piano.key_releases {
                    self.piano.deadlines[usize::from(slot)] = Some(Instant::now() + TIMED_NOTE);
                }
                if let Some(note) = self.piano.keyboard.press(character) {
                    self.piano.show_octave = false;
                    // The worker's structural queue is deliberately tiny.
                    // Keep every chord key until accepted, but cancel it on
                    // key-up so a busy worker can never play a late note.
                    self.piano.pending_on[usize::from(slot)] = Some(note);
                }
                self.flush_piano_input();
            }
        }
        self.dirty_frame = true;
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    mod piano_mode {
        //! Tests for piano mode.

        use crate::app::piano_mode::*;
        use crate::app::tests::support::*;

        fn key(app: &mut App, character: char, kind: KeyEventKind) {
            app.handle_terminal_event(Event::Key(KeyEvent::new_with_kind(
                KeyCode::Char(character),
                KeyModifiers::NONE,
                kind,
            )))
            .unwrap();
        }

        #[test]
        fn piano_chords_keep_the_score_and_initiating_panel_untouched() {
            let directory = tempfile::tempdir().unwrap();
            let mut app = crate::app::tests::support::app_over(directory.path());
            app.toggle_reference_browse();
            let focus = app.focus;
            let source = app.editor().source();
            let caret = app.editor().primary_selection().head;
            app.piano.key_releases = true;
            app.handle_terminal_event(Event::Key(KeyEvent::new(
                KeyCode::F(12),
                KeyModifiers::NONE,
            )))
            .unwrap();
            assert!(app.piano.open);
            assert!(
                !app.accepts_raw_paste(),
                "chords must never become text pastes"
            );
            for character in ['a', 'd', 'g'] {
                key(&mut app, character, KeyEventKind::Press);
            }
            key(&mut app, 'a', KeyEventKind::Repeat);
            assert_eq!(app.piano.keyboard.notes(), [60, 64, 67]);
            key(&mut app, 'd', KeyEventKind::Release);
            assert_eq!(app.piano.keyboard.notes(), [60, 64, 67]);
            app.handle_terminal_event(Event::Paste("don't edit".into()))
                .unwrap();
            app.handle_terminal_event(Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)))
                .unwrap();
            assert!(!app.piano.open);
            assert_eq!(app.focus, focus);
            assert_eq!(app.editor().source(), source);
            assert_eq!(app.editor().primary_selection().head, caret);
            assert_eq!(app.piano.keyboard.held_notes().count(), 0);
        }

        #[test]
        fn piano_octave_changes_release_original_keys_and_focus_loss_releases_chords() {
            let directory = tempfile::tempdir().unwrap();
            let mut app = crate::app::tests::support::app_over(directory.path());
            app.piano.key_releases = true;
            app.toggle_piano_mode();
            assert_eq!(
                app.piano_status(),
                "PIANO  A S D F G H J K L notes · Z/X octave"
            );
            key(&mut app, 'a', KeyEventKind::Press);
            key(&mut app, 'x', KeyEventKind::Press);
            assert_eq!(app.piano_status(), "PIANO  Octave 5");
            app.pump_piano(Instant::now() + Duration::from_secs(120));
            key(&mut app, 'x', KeyEventKind::Release);
            key(&mut app, 'a', KeyEventKind::Repeat);
            assert_eq!(app.piano_status(), "PIANO  Octave 5");
            key(&mut app, 'd', KeyEventKind::Press);
            assert_eq!(app.piano_status(), "PIANO  C4 + E5");
            assert_eq!(app.piano.keyboard.notes(), [60, 76]);
            key(&mut app, 'a', KeyEventKind::Release);
            assert_eq!(app.piano.keyboard.notes(), [60, 76]);
            app.handle_terminal_event(Event::FocusLost).unwrap();
            assert!(app.piano.open);
            assert_eq!(app.piano.keyboard.held_notes().count(), 0);
            key(&mut app, 'd', KeyEventKind::Repeat);
            key(&mut app, 'd', KeyEventKind::Press); // native repeats can also be presses
            assert_eq!(app.piano.keyboard.held_notes().count(), 0);
            key(&mut app, 'd', KeyEventKind::Release);
            key(&mut app, 'd', KeyEventKind::Press);
            assert_eq!(app.piano.keyboard.notes(), [76]);
            key(&mut app, 'z', KeyEventKind::Press);
            assert_eq!(app.piano_status(), "PIANO  Octave 4");
            app.close_piano_mode();
            assert_eq!(
                app.retained_piano_notes().as_deref(),
                Some("Notes  E5 · [76]")
            );
            app.toggle_piano_mode();
            assert_eq!(
                app.piano_status(),
                "PIANO  A S D F G H J K L notes · Z/X octave"
            );
        }

        #[test]
        fn piano_focus_return_rearms_keys_released_in_another_window() {
            let directory = tempfile::tempdir().unwrap();
            let mut app = crate::app::tests::support::app_over(directory.path());
            app.piano.key_releases = true;
            app.toggle_piano_mode();
            key(&mut app, 'a', KeyEventKind::Press);
            app.handle_terminal_event(Event::FocusLost).unwrap();
            // The physical release happened outside our terminal, so no key-up.
            app.handle_terminal_event(Event::FocusGained).unwrap();
            key(&mut app, 'a', KeyEventKind::Repeat);
            assert_eq!(app.piano.keyboard.held_notes().count(), 0);
            key(&mut app, 'a', KeyEventKind::Press);
            assert_eq!(app.piano.keyboard.notes(), [60]);
            key(&mut app, 'a', KeyEventKind::Repeat);
            assert_eq!(app.piano.keyboard.held_notes().count(), 1);
            key(&mut app, 'a', KeyEventKind::Release);
            assert_eq!(app.piano.keyboard.held_notes().count(), 0);
        }

        #[test]
        fn piano_notes_expire_after_one_minute_or_clear_when_reopening() {
            let directory = tempfile::tempdir().unwrap();
            let mut app = app_over(directory.path());
            app.toggle_piano_mode();
            app.close_piano_mode();
            assert!(
                !app.has_piano_notes(),
                "opening alone has no notes to retain"
            );
            app.toggle_piano_mode();
            for character in ['a', 'd', 'g'] {
                key(&mut app, character, KeyEventKind::Press);
            }
            app.close_piano_mode();
            let first_deadline = app.piano.notes_until.unwrap();
            let notes = "Notes  C4 + E4 + G4 · [60,64,67]";
            assert_eq!(app.retained_piano_notes().as_deref(), Some(notes));
            app.dirty_frame = false;
            app.pump_piano(first_deadline - Duration::from_millis(1));
            assert!(!app.dirty_frame);
            assert_eq!(app.retained_piano_notes().as_deref(), Some(notes));
            assert!(
                app.piano_wait(Duration::from_secs(120), first_deadline)
                    .is_zero()
            );

            app.toggle_piano_mode();
            assert!(!app.has_piano_notes(), "the active indicator takes over");
            assert!(
                app.piano.keyboard.notes().is_empty(),
                "reopening clears the chord"
            );
            app.pump_piano(first_deadline + NOTES_RETENTION);
            app.close_piano_mode();
            assert!(
                !app.has_piano_notes(),
                "leaving without playing keeps the scratchpad empty"
            );
            app.toggle_piano_mode();
            key(&mut app, 'w', KeyEventKind::Press);
            assert_eq!(app.piano.keyboard.notes(), [61], "a new chord replaces it");
            app.close_piano_mode();
            let deadline = app.piano.notes_until.unwrap();
            assert!(deadline >= first_deadline);
            app.dirty_frame = false;
            app.pump_piano(deadline);
            assert!(
                app.dirty_frame,
                "an idle screen must repaint the expired row"
            );
            assert!(!app.has_piano_notes());
            assert!(app.piano.keyboard.notes().is_empty());
            app.toggle_piano_mode();
            assert_eq!(
                app.piano_status(),
                "PIANO  A S D F G H J K L notes · Z/X octave",
                "expired notes stay forgotten"
            );
        }

        #[test]
        fn retained_piano_notes_survive_messages_and_leave_the_editor_usable() {
            use ratatui::{Terminal, backend::TestBackend};
            let directory = tempfile::tempdir().unwrap();
            let mut app = app_over(directory.path());
            let mut terminal = Terminal::new(TestBackend::new(110, 32)).unwrap();
            app.toggle_piano_mode();
            for character in ['a', 'd', 'g'] {
                key(&mut app, character, KeyEventKind::Press);
            }
            app.close_piano_mode();
            let source = app.editor().source();
            app.handle_terminal_event(Event::Paste("note(\"[60,64,67]\")".into()))
                .unwrap();
            assert_ne!(app.editor().source(), source);
            for error in [false, true] {
                app.status = "saved recording".into();
                if error {
                    app.errors
                        .set(ErrorOwner::Interface, "last scene cannot be closed".into());
                }
                terminal
                    .draw(|frame| app.paint_into(frame, Instant::now()).unwrap())
                    .unwrap();
                let buffer = terminal.backend().buffer();
                let row = |y| {
                    (0..buffer.area.width)
                        .map(|x| buffer.cell((x, y)).unwrap().symbol())
                        .collect::<String>()
                };
                let footer = app.regions.footer;
                assert_eq!(footer.height, if error { 4 } else { 3 });
                assert!(
                    row(footer.y + u16::from(error)).contains("Notes  C4 + E4 + G4 · [60,64,67]")
                );
                let hits = app.footer_hits();
                assert_eq!(hits.status.y, footer.bottom() - 2);
                assert!(row(hits.status.y).contains("saved recording"));
                if error {
                    assert!(row(footer.y).contains("last scene cannot be closed"));
                }
            }
            app.pump_piano(app.piano.notes_until.unwrap());
            terminal
                .draw(|frame| app.paint_into(frame, Instant::now()).unwrap())
                .unwrap();
            assert_eq!(
                app.regions.footer.height, 3,
                "only the error needs an extra row now"
            );
            let screen = terminal
                .backend()
                .buffer()
                .content
                .iter()
                .map(|cell| cell.symbol())
                .collect::<String>();
            assert!(!screen.contains("Notes  C4"));
        }

        /// Hiding the footer takes only its meter and chip row: an error and the
        /// retained notes keep their own rows above the status line.
        #[test]
        fn retained_piano_notes_keep_their_row_with_the_footer_hidden() {
            use ratatui::{Terminal, backend::TestBackend};
            let directory = tempfile::tempdir().unwrap();
            let mut app = app_over(directory.path());
            app.ui_settings.show_footer = false;
            let mut terminal = Terminal::new(TestBackend::new(110, 32)).unwrap();
            app.toggle_piano_mode();
            for character in ['a', 'd'] {
                key(&mut app, character, KeyEventKind::Press);
            }
            app.close_piano_mode();
            app.status = "saved recording".into();
            app.errors
                .set(ErrorOwner::Interface, "last scene cannot be closed".into());
            terminal
                .draw(|frame| app.paint_into(frame, Instant::now()).unwrap())
                .unwrap();
            let buffer = terminal.backend().buffer();
            let row = |y| {
                (0..buffer.area.width)
                    .map(|x| buffer.cell((x, y)).unwrap().symbol())
                    .collect::<String>()
            };
            let footer = app.regions.footer;
            assert_eq!(footer.height, 3, "the error, the notes and the status line");
            assert!(
                row(footer.y).contains("last scene cannot be closed"),
                "{}",
                row(footer.y)
            );
            assert!(
                row(footer.y + 1).contains("Notes  C4 + E4 · [60,64]"),
                "{}",
                row(footer.y + 1)
            );
            let hits = app.footer_hits();
            assert_eq!(hits.status.y, footer.bottom() - 1);
            assert!(row(hits.status.y).contains("saved recording"));
            assert!(hits.dock.is_empty() && hits.device_chip.is_empty());
        }

        #[test]
        fn piano_legacy_chords_expire_without_releases_or_another_input_event() {
            let directory = tempfile::tempdir().unwrap();
            let mut app = crate::app::tests::support::app_over(directory.path());
            app.toggle_piano_mode();
            for character in ['a', 'd', 'g'] {
                key(&mut app, character, KeyEventKind::Press);
            }
            assert_eq!(app.piano.keyboard.held_notes().count(), 3);
            assert!(app.piano_wait(Duration::from_secs(5), Instant::now()) <= TIMED_NOTE);
            app.pump_piano(Instant::now() + TIMED_NOTE);
            assert_eq!(app.piano.keyboard.held_notes().count(), 0);
            assert_eq!(app.piano.keyboard.notes(), [60, 64, 67]);
        }

        #[test]
        fn piano_burst_preserves_all_fifteen_keys_through_worker_backpressure() {
            let directory = tempfile::tempdir().unwrap();
            let mut app = crate::app::tests::support::app_over(directory.path());
            app.piano.key_releases = true;
            app.toggle_piano_mode();
            for character in "awsedftgyhujkol".chars() {
                key(&mut app, character, KeyEventKind::Press);
            }
            assert_eq!(app.piano.keyboard.held_notes().count(), 15);
            let until = Instant::now() + Duration::from_secs(5);
            while app.piano.pending_on.iter().any(Option::is_some) {
                assert!(
                    Instant::now() < until,
                    "chord notes never reached the worker"
                );
                app.pump_piano(Instant::now());
                std::thread::sleep(Duration::from_millis(2));
            }
            // If a note was released while waiting, there must be no delayed
            // attack waiting to replay it once the queue has room again.
            app.piano.pending_on[0] = app.piano.keyboard.held_notes().find(|note| note.key == 0);
            key(&mut app, 'a', KeyEventKind::Release);
            assert!(app.piano.pending_on[0].is_none());
            app.close_piano_mode();
            assert!(app.piano.pending_on.iter().all(Option::is_none));
        }

        #[test]
        fn piano_rebound_shortcut_works_over_settings_and_stop_clears_keys() {
            let directory = tempfile::tempdir().unwrap();
            let mut app = crate::app::tests::support::app_over(directory.path());
            app.keybinds.learn(
                BindAction::PianoMode,
                Some(crate::keybinds::KeyCombo::parse("ctrl+shift+f9").unwrap()),
            );
            app.settings_sheet = Some(SettingsSheet::default());
            app.focus = Focus::Panel(PanelKind::Settings);
            app.handle_terminal_event(Event::Key(KeyEvent::new(
                KeyCode::F(9),
                KeyModifiers::CONTROL | KeyModifiers::SHIFT,
            )))
            .unwrap();
            assert!(app.piano.open);
            key(&mut app, 'w', KeyEventKind::Press);
            assert!(app.piano_status().contains("C#4 / Db4"));
            app.handle_terminal_event(Event::Key(KeyEvent::new(KeyCode::F(8), KeyModifiers::NONE)))
                .unwrap();
            assert_eq!(app.piano.keyboard.held_notes().count(), 0);
            app.handle_terminal_event(Event::Key(KeyEvent::new(
                KeyCode::F(9),
                KeyModifiers::CONTROL | KeyModifiers::SHIFT,
            )))
            .unwrap();
            assert!(!app.piano.open);
            assert!(app.settings_sheet.is_some());
            assert_eq!(app.focus, Focus::Panel(PanelKind::Settings));
        }

        #[test]
        #[cfg(feature = "harness")]
        fn piano_footer_keeps_the_existing_screen_and_hides_only_the_typing_caret() {
            use ratatui::{Terminal, backend::TestBackend, style::Color};
            let directory = tempfile::tempdir().unwrap();
            let mut app = crate::app::tests::support::app_over(directory.path());
            app.pin_frame_inputs = true;
            app.theme = Theme::built_in_default();
            app.settings_sheet = Some(SettingsSheet::default());
            app.piano.key_releases = true;
            let mut terminal = Terminal::new(TestBackend::new(110, 32)).unwrap();
            terminal
                .draw(|frame| app.paint_into(frame, Instant::now()).unwrap())
                .unwrap();
            let before = terminal.backend().buffer().clone();
            app.toggle_piano_mode();
            terminal
                .draw(|frame| app.paint_into(frame, Instant::now()).unwrap())
                .unwrap();
            let idle_screen = terminal
                .backend()
                .buffer()
                .content
                .iter()
                .map(|cell| cell.symbol())
                .collect::<String>();
            assert!(idle_screen.contains("A S D F G H J K L"));
            assert!(idle_screen.contains("Z/X octave"));
            for character in ['a', 'd', 'g'] {
                key(&mut app, character, KeyEventKind::Press);
            }
            terminal
                .draw(|frame| app.paint_into(frame, Instant::now()).unwrap())
                .unwrap();
            assert!(!terminal.backend().cursor_visible());
            let buffer = terminal.backend().buffer();
            let screen = buffer
                .content
                .iter()
                .map(|cell| cell.symbol())
                .collect::<String>();
            assert!(screen.contains("PIANO"));
            assert!(screen.contains("C4 + E4 + G4"));
            assert!(!screen.contains("Z/X octave"));
            assert!(!screen.contains("Velocity"));
            for y in 0..app.regions.footer.y {
                for x in 0..buffer.area.width {
                    assert_eq!(
                        buffer.cell((x, y)),
                        before.cell((x, y)),
                        "piano mode changed the screen at ({x}, {y})"
                    );
                }
            }
            // Optional visual QA export; normal tests never write a screenshot.
            if let Ok(path) = std::env::var("RUSTEL_PIANO_RENDER_EXPORT") {
                let rgb = |color: Color| match color {
                    Color::Rgb(r, g, b) => [r, g, b],
                    _ => [0, 0, 0],
                };
                let frame = serde_json::json!({
                    "width": buffer.area.width,
                    "height": buffer.area.height,
                    "cells": buffer.content.iter().map(|cell| serde_json::json!({
                        "text": cell.symbol(), "fg": rgb(cell.fg), "bg": rgb(cell.bg),
                        "bold": cell.modifier.contains(ratatui::style::Modifier::BOLD),
                    })).collect::<Vec<_>>()
                });
                std::fs::write(path, frame.to_string()).unwrap();
            }
        }

        #[test]
        fn piano_fades_on_an_idle_screen_without_animating_the_transport() {
            use ratatui::{Terminal, backend::TestBackend};
            let directory = tempfile::tempdir().unwrap();
            let mut app = app_over(directory.path());
            let mut terminal = Terminal::new(TestBackend::new(110, 32)).unwrap();
            app.toggle_piano_mode();
            let started = app.piano.pulse_started.unwrap();
            assert_eq!(app.piano_pulse(), Some(1.0));
            assert_eq!(
                app.piano_status(),
                "PIANO  A S D F G H J K L notes · Z/X octave",
                "no note has been played"
            );
            app.dirty_frame = false;
            for (millis, expected) in [
                (250, 0.853_553_4),
                (500, 0.5),
                (750, 0.146_446_6),
                (1000, 0.0),
                (1500, 0.5),
                (2000, 1.0),
            ] {
                let now = started + Duration::from_millis(millis);
                app.pump_piano(now);
                assert!(app.dirty_frame, "the idle screen must repaint the fade");
                let pulse = app.piano_pulse().unwrap();
                assert!((pulse - expected).abs() < 0.000_001);
                terminal
                    .draw(|frame| app.paint_into(frame, now).unwrap())
                    .unwrap();
                let label = terminal
                    .backend()
                    .buffer()
                    .content
                    .windows(5)
                    .find(|cells| {
                        cells.iter().map(|cell| cell.symbol()).collect::<String>() == "PIANO"
                    })
                    .expect("the mode owns its footer even before playing a note");
                assert_eq!(label[0].fg, app.theme.surface);
                assert_eq!(
                    label[0].bg,
                    crate::theme::mix(app.theme.surface, app.theme.accent, pulse)
                );
                assert!(!app.is_playing());
                assert!(!app.is_evaluating());
                app.dirty_frame = false;
            }
            app.close_piano_mode();
            assert_eq!(app.piano_pulse(), None);
        }

        #[test]
        fn piano_drops_velocity_controls_and_keeps_mouse_and_global_shortcuts_usable() {
            let directory = tempfile::tempdir().unwrap();
            let mut app = crate::app::tests::support::app_over(directory.path());
            let source = app.editor().source();
            app.toggle_piano_mode();
            for character in ['c', 'v', 'm', ' '] {
                key(&mut app, character, KeyEventKind::Press);
            }
            assert!(app.piano.open);
            assert_eq!(app.piano.keyboard.held_notes().count(), 0);
            assert_eq!(app.editor().source(), source);
            assert!(
                !app.handle_piano_event(&Event::Mouse(MouseEvent {
                    kind: MouseEventKind::ScrollDown,
                    column: 4,
                    row: 4,
                    modifiers: KeyModifiers::NONE,
                }))
                .unwrap()
            );
            app.handle_terminal_event(Event::Key(KeyEvent::new(
                KeyCode::Char('o'),
                KeyModifiers::CONTROL,
            )))
            .unwrap();
            assert!(
                app.settings_sheet.is_some(),
                "the settings shortcut still works"
            );
            assert!(app.piano.open);
        }

        #[test]
        fn piano_stays_visible_in_zen_and_does_not_expose_a_hidden_file_action() {
            use ratatui::{Terminal, backend::TestBackend};
            let directory = tempfile::tempdir().unwrap();
            let mut app = crate::app::tests::support::app_over(directory.path());
            app.ui_settings.zen = true;
            app.status = "saved recording".into();
            app.remember_status_file(directory.path().join("take.wav"));
            assert!(app.status_names_last_file());
            app.toggle_piano_mode();
            assert!(!app.status_names_last_file());
            key(&mut app, 'w', KeyEventKind::Press);
            let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
            terminal
                .draw(|frame| app.paint_into(frame, Instant::now()).unwrap())
                .unwrap();
            assert!(app.regions.footer.is_empty());
            let buffer = terminal.backend().buffer();
            let bottom = (0..80)
                .map(|x| buffer.cell((x, 23)).unwrap().symbol())
                .collect::<String>();
            assert!(bottom.contains("PIANO  C#4 / Db4"));
            let caret = app.editor().primary_selection();
            app.handle_terminal_event(Event::Mouse(MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: 12,
                row: 23,
                modifiers: KeyModifiers::NONE,
            }))
            .unwrap();
            assert_eq!(app.editor().primary_selection(), caret);
            assert_eq!(app.pointer_shape_at(12, 23), SHAPE_DEFAULT);
            app.close_piano_mode();
            assert!(app.status_names_last_file());
            assert_eq!(app.status, "saved recording");
            terminal
                .draw(|frame| app.paint_into(frame, Instant::now()).unwrap())
                .unwrap();
            let bottom = (0..80)
                .map(|x| terminal.backend().buffer().cell((x, 23)).unwrap().symbol())
                .collect::<String>();
            assert!(bottom.contains("Notes  C#4 / Db4 · [61]"));
            assert!(
                app.handle_piano_event(&Event::Mouse(MouseEvent {
                    kind: MouseEventKind::Down(MouseButton::Left),
                    column: 12,
                    row: 23,
                    modifiers: KeyModifiers::NONE,
                }))
                .unwrap()
            );
            assert_eq!(app.editor().primary_selection(), caret);
            app.pump_piano(app.piano.notes_until.unwrap());
            assert!(app.piano_zen_row().is_none());
        }

        #[test]
        fn piano_menu_stop_and_error_navigation_match_their_shortcuts() {
            let directory = tempfile::tempdir().unwrap();
            let mut app = crate::app::tests::support::app_over(directory.path());
            app.piano.key_releases = true;
            app.toggle_piano_mode();
            key(&mut app, 'a', KeyEventKind::Press);
            app.dispatch_menu_action(crate::menu::MenuAction::Stop)
                .unwrap();
            assert!(app.piano.open);
            assert_eq!(app.piano.keyboard.held_notes().count(), 0);
            assert!(app.piano.pending_on.iter().all(Option::is_none));
            key(&mut app, 'd', KeyEventKind::Press);
            app.dispatch_menu_action(crate::menu::MenuAction::FirstError)
                .unwrap();
            assert!(!app.piano.open);
            assert_eq!(app.piano.keyboard.held_notes().count(), 0);
        }
    }
}
