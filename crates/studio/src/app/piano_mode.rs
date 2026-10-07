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
