use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseEvent};

use super::{Command, Motion};

const RAW_PASTE_MIN_KEY_EVENTS: usize = 8;

/// How the primary modifier is spelled, everywhere in the studio.  Caret
/// notation is what `stty` prints and what nano's footer uses, it is a third
/// the width of `Ctrl+`, and it is the same on every platform, so every
/// surface uses one convention.  See `KeyboardCapabilities::primary_label`
/// for why there is no ⌘ form.
pub const PRIMARY_MODIFIER: &str = "^";

/// What the terminal in front of the musician actually delivers.
///
/// Two facts, deliberately kept apart, because folding them together is what
/// put dead ⌘ chords in the footer.  `enhanced` is the progressive keyboard
/// protocol handshake: it means modified Enter and punctuation arrive
/// distinctly.  It says nothing about Command.  iTerm2 answers the handshake
/// and macOS goes on consuming ⌘ at the menu layer, so a terminal can be
/// fully enhanced and never deliver a single ⌘ chord.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct KeyboardCapabilities {
    /// The app queries this before its event reader starts; querying
    /// concurrently with `read`/`poll` races Crossterm's response.
    pub enhanced: bool,
    /// Set once `^Space` has arrived from the terminal.
    ///
    /// Nothing reads it to decide what to show. The menus, help and Settings
    /// name `^Space` from the terminal profile and the platform table, and
    /// never on macOS, where the system keeps the chord.
    pub space_seen: bool,
    /// Set once a key event has actually carried SUPER, which is the only
    /// proof that ⌘ reaches this process.  Nothing a terminal reports about
    /// itself establishes it, and a hard-coded list of ⌘-capable terminals
    /// goes stale the next time one of them claims a chord for itself.
    pub super_seen: bool,
}

impl KeyboardCapabilities {
    pub const fn enhanced() -> Self {
        Self {
            enhanced: true,
            space_seen: false,
            super_seen: false,
        }
    }

    pub const fn legacy() -> Self {
        Self {
            enhanced: false,
            space_seen: false,
            super_seen: false,
        }
    }

    /// How to spell the shortcut modifier: caret notation, everywhere.
    ///
    /// `^S`, `^⇧Z`. What `stty` prints, what nano's own footer uses, and a
    /// third the width of `Ctrl+Shift+` - the strip needs 203 columns spelled
    /// out and never gets them, since `render_shortcuts` only starts after the
    /// device and MIDI chips have taken their share of the row.
    ///
    /// There is deliberately **no ⌘ spelling, on any platform**. Dispatch
    /// accepts Command wherever it accepts Control - `key_to_command` folds
    /// both into `primary` - but the chords worth advertising are precisely
    /// the ones every Mac terminal has already claimed: ⌘T is New Tab, ⌘D
    /// splits, ⌘W closes, and ⌘Q quits the terminal out from under the studio.
    /// A footer cannot mix two conventions, and cannot use the one that fails,
    /// so it uses the one that works in every terminal on every OS. ⌘ is
    /// mentioned once, in Help ▸ About, and only where `super_seen` has proved
    /// it arrives.
    pub fn primary_label(self) -> &'static str {
        PRIMARY_MODIFIER
    }

    fn shift_label(self) -> &'static str {
        crate::terminal::symbol("⇧")
    }

    /// A primary-modifier chord: `^S`.
    pub fn chord(self, key: &str) -> String {
        format!("{}{key}", self.primary_label())
    }

    /// A primary+shift chord: `^⇧Z`.
    pub fn shift_chord(self, key: &str) -> String {
        format!("{}{}{key}", self.primary_label(), self.shift_label())
    }

    /// A shift-and-function-key chord: `⇧F1`.
    ///
    /// Shift with an F-key is the one corner of the keyboard nothing had
    /// claimed: every bare F-key is a default already, the ^⇧ letters run
    /// into the terminal emulator's own - Windows Terminal keeps ^⇧V for
    /// its paste, ^⇧F for find, ^⇧W for close - and Alt belongs to the
    /// menu mnemonics.
    pub fn shift_function_key(self, number: u8) -> String {
        format!("{}F{number}", self.shift_label())
    }

    /// The transport, spelled like every other chip.  These were pinned to
    /// `Ctrl+` while `primary_label` flipped to ⌘ on nothing more than the
    /// protocol handshake, which is what made the footer read as a mix of two
    /// conventions with half of them dead.  One convention now, so they can
    /// join it; F5 and F8 stay as the portable aliases.
    pub fn evaluate_hint(self) -> String {
        self.chord("S")
    }

    pub fn stop_hint(self) -> String {
        self.chord("G")
    }

    pub fn scene_switch_hints(self) -> (String, String) {
        self.scene_switch_hints_for_panes(false)
    }

    /// Scene cycling changes the focused pane; plain brackets focus the
    /// other pane while a split is open.
    pub fn scene_switch_hints_for_panes(self, split: bool) -> (String, String) {
        if self.enhanced && split {
            (self.shift_chord("["), self.shift_chord("]"))
        } else if self.enhanced {
            (self.chord("["), self.chord("]"))
        } else {
            ("F6".to_owned(), "F7".to_owned())
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum EditorInput {
    Command(Command),
    Mouse(MouseEvent),
}

#[derive(Default)]
pub(crate) struct TerminalEventBatch {
    events: Vec<Event>,
    raw_keys: Vec<KeyEvent>,
    raw_text: String,
    raw_key_count: usize,
    raw_has_line_break: bool,
    raw_is_paste: bool,
}

impl TerminalEventBatch {
    /// A bounded read stopped inside an already recognized paste. Even a
    /// lone trailing Enter in its next chunk is text, not confirmation.
    pub(crate) fn with_paste_continuation(continuing: bool) -> Self {
        Self {
            raw_is_paste: continuing,
            ..Self::default()
        }
    }

    pub(crate) fn push(&mut self, event: Event) {
        let Event::Key(key) = event else {
            self.flush_raw_keys();
            self.events.push(event);
            return;
        };
        let Some(character) = plain_text_character(&key) else {
            self.flush_raw_keys();
            self.events.push(Event::Key(key));
            return;
        };

        self.raw_text.push(character);
        self.raw_key_count = self.raw_key_count.saturating_add(1);
        self.raw_has_line_break |= matches!(character, '\r' | '\n');
        if !self.raw_is_paste {
            self.raw_keys.push(key);
            self.raw_is_paste = (self.raw_has_line_break && self.raw_key_count >= 2)
                || self.raw_key_count >= RAW_PASTE_MIN_KEY_EVENTS;
            if self.raw_is_paste {
                self.raw_keys.clear();
            }
        }
    }

    pub(crate) fn finish(mut self) -> Vec<Event> {
        self.flush_raw_keys();
        self.events
    }

    fn flush_raw_keys(&mut self) {
        if self.raw_key_count == 0 {
            return;
        }
        // A short dropped path must stay atomic too: `C:\test` has seven
        // characters, below the ordinary paste threshold. Replaying those
        // as panel shortcuts can open a field halfway through the drop.
        // Decide on the completed burst, without touching the filesystem;
        // the app still validates real drops and preserves field ownership.
        if self.raw_is_paste || (self.raw_key_count > 1 && looks_like_absolute_path(&self.raw_text))
        {
            self.raw_keys.clear();
            self.events
                .push(Event::Paste(std::mem::take(&mut self.raw_text)));
        } else {
            self.events.extend(self.raw_keys.drain(..).map(Event::Key));
            self.raw_text.clear();
        }
        self.raw_key_count = 0;
        self.raw_has_line_break = false;
        self.raw_is_paste = false;
    }
}

/// Only classifies an already collected burst, never individual keystrokes.
/// A missing path is still literal text; filesystem/drop handling belongs to
/// the app. Recognize both shell spellings so short drive, UNC and Unix paths
/// behave alike, including the quotes supplied by terminal drag-and-drop.
fn looks_like_absolute_path(text: &str) -> bool {
    let text = text.trim();
    let text = match text.chars().next() {
        Some(quote @ ('\'' | '"')) => {
            let Some(unquoted) = text[quote.len_utf8()..].strip_suffix(quote) else {
                return false;
            };
            unquoted
        }
        _ => text,
    };
    let bytes = text.as_bytes();
    text.starts_with('/')
        || text.starts_with(r"\\")
        || (bytes.len() >= 3
            && bytes[0].is_ascii_alphabetic()
            && bytes[1] == b':'
            && matches!(bytes[2], b'/' | b'\\'))
}

pub(crate) fn is_plain_text_key_event(event: &Event) -> bool {
    matches!(event, Event::Key(key) if plain_text_character(key).is_some())
}

/// A control character that is never text. A newline or a tab as a
/// character is text a terminal or a test may well type; the rest - an
/// SOH left behind by a screenshot shortcut, a stray escape - is not.
pub fn is_stray_control(character: char) -> bool {
    character.is_control() && !matches!(character, '\n' | '\r' | '\t')
}

/// Whether a click keeps the anchor of the selection and moves its head.
///
/// Shift is the usual key. kitty keeps Shift+click for its own selection
/// and never reports it, so Alt and Ctrl extend too.
pub fn click_extends(modifiers: KeyModifiers) -> bool {
    modifiers.intersects(KeyModifiers::SHIFT | KeyModifiers::ALT | KeyModifiers::CONTROL)
}

fn plain_text_character(key: &KeyEvent) -> Option<char> {
    if key.kind != KeyEventKind::Press
        || !(key.modifiers.is_empty() || key.modifiers == KeyModifiers::SHIFT)
    {
        return None;
    }
    match key.code {
        KeyCode::Char(character) if !is_stray_control(character) => Some(character),
        KeyCode::Enter => Some('\n'),
        KeyCode::Tab => Some('\t'),
        _ => None,
    }
}

pub fn event_to_input(event: Event, capabilities: KeyboardCapabilities) -> Option<EditorInput> {
    event_to_input_with_binds(event, capabilities, &|_| None, &|_| false)
}

/// The same, with the keybinding table's say. The app passes its table so
/// a learnt EDITOR chord - undo, copy, comment - dispatches as its new key
/// everywhere the editor is typed into; every other caller passes the
/// empty table, which leaves the built-in chords alone.
///
/// A key the table names that is NOT an editor command (the transport,
/// the panels) resolves to `None` here and is answered higher up, where
/// the whole studio is reachable.
pub fn event_to_input_with_binds(
    event: Event,
    capabilities: KeyboardCapabilities,
    binds: &dyn Fn(&KeyEvent) -> Option<Command>,
    moved: &dyn Fn(&KeyEvent) -> bool,
) -> Option<EditorInput> {
    match event {
        Event::Key(key) => {
            key_to_command_with_binds(key, capabilities, binds, moved).map(EditorInput::Command)
        }
        Event::Mouse(mouse) => Some(EditorInput::Mouse(mouse)),
        Event::Paste(text) => Some(EditorInput::Command(Command::PasteText(text))),
        Event::FocusGained | Event::FocusLost | Event::Resize(_, _) => None,
    }
}

pub fn key_to_command(event: KeyEvent, capabilities: KeyboardCapabilities) -> Option<Command> {
    key_to_command_with_binds(event, capabilities, &|_| None, &|_| false)
}

/// The chord reader, with the keybinding table's say first: the app is
/// asked what this press means, and a chord it names dispatches as its
/// command before the built-in table is consulted, so a moved undo or
/// copy keeps working at its new key. `moved` reports a key whose
/// built-in chord was moved. The reader drops the built-in
/// shortcut on that key, so the moved chord and the key it left do not
/// both work. The app resolves learnt chords only; the reader falls
/// through to the built-in chords for everything else.
pub fn key_to_command_with_binds(
    event: KeyEvent,
    capabilities: KeyboardCapabilities,
    binds: &dyn Fn(&KeyEvent) -> Option<Command>,
    moved: &dyn Fn(&KeyEvent) -> bool,
) -> Option<Command> {
    if event.kind == KeyEventKind::Release {
        return None;
    }
    let modifiers = event.modifiers;
    let control = modifiers.contains(KeyModifiers::CONTROL);
    let super_key = modifiers.contains(KeyModifiers::SUPER);
    let primary = control || super_key;
    let alt = modifiers.contains(KeyModifiers::ALT);
    // Under the progressive keyboard protocol Crossterm resolves a shifted
    // key to its shifted character and drops the Shift bit: ⇧⌘Z arrives as
    // `Char('Z')` with only Super set. An uppercase letter is therefore as
    // much evidence of Shift as the modifier itself, which is what makes
    // redo reachable on a Mac at all.
    let shift = modifiers.contains(KeyModifiers::SHIFT)
        || matches!(event.code, KeyCode::Char(character) if character.is_uppercase());

    // The table's own reading of this press first: a chord the app
    // resolves from it wins, whatever the built-in reading of the same
    // key would have been - including the enhanced Enter and stop
    // shortcuts below. Alt never reaches a binding - option is how a Mac
    // types characters.
    if !alt
        && event.kind == KeyEventKind::Press
        && let Some(command) = binds(&event)
    {
        return Some(command);
    }

    // Modified Enter is a shortcut only when the terminal has proved that it
    // reports it without the legacy collision with plain Enter.
    if capabilities.enhanced && matches!(event.code, KeyCode::Enter) && primary && !alt {
        return (!moved(&event) && !shift && event.kind == KeyEventKind::Press)
            .then_some(Command::Evaluate);
    }
    if capabilities.enhanced && matches!(event.code, KeyCode::Char('.')) && primary && !alt {
        return (!moved(&event) && !shift && event.kind == KeyEventKind::Press)
            .then_some(Command::Stop);
    }

    if super_key {
        let motion = match event.code {
            KeyCode::Left => Some(Motion::LineStart),
            KeyCode::Right => Some(Motion::LineEnd),
            KeyCode::Up => Some(Motion::DocumentStart),
            KeyCode::Down => Some(Motion::DocumentEnd),
            _ => None,
        };
        if let Some(motion) = motion {
            return Some(Command::Move {
                motion,
                extend: shift,
            });
        }
    }

    if alt {
        match event.code {
            KeyCode::Left => {
                return Some(Command::Move {
                    motion: Motion::GroupLeft,
                    extend: shift,
                });
            }
            KeyCode::Right => {
                return Some(Command::Move {
                    motion: Motion::GroupRight,
                    extend: shift,
                });
            }
            KeyCode::Backspace => return Some(Command::DeleteWordBackward),
            KeyCode::Delete => return Some(Command::DeleteWordForward),
            _ => {}
        }
    }

    if primary {
        match event.code {
            KeyCode::Char(character) => {
                // A key the table has re-meaned elsewhere is freed here too:
                // if its default spelling were still answered to, the moved
                // chord and the key it left would both work, and one chord
                // would mean two things again. The remaining built-in
                // shortcuts stand; the single letters fall through to the
                // ordinary reading, which the editor would not have answered
                // to anyway.
                let shortcut = if moved(&event) {
                    None
                } else {
                    match character.to_ascii_lowercase() {
                        'a' if !shift => Some(Command::SelectAll),
                        'c' if !shift => Some(Command::Copy),
                        'x' if !shift => Some(Command::Cut),
                        'v' if !shift => Some(Command::Paste),
                        // Saving IS updating in the studio, as it is for a
                        // watched set: the file is what you hear.
                        's' if !shift => Some(Command::Evaluate),
                        'g' if !shift => Some(Command::Stop),
                        'z' if shift => Some(Command::Redo),
                        'z' => Some(Command::Undo),
                        'y' if !shift => Some(Command::Redo),
                        // A legacy terminal collapses Ctrl+/ onto the 0x1F byte,
                        // which Crossterm reports as Ctrl+7; there is no way to
                        // tell the two apart, so the alias exists only where the
                        // real chord cannot arrive.
                        '/' if !shift => Some(Command::ToggleComment),
                        '_' | '7' if !capabilities.enhanced => Some(Command::ToggleComment),
                        _ => None,
                    }
                };
                if shortcut.is_some() {
                    return if event.kind == KeyEventKind::Press {
                        shortcut
                    } else {
                        None
                    };
                }
                // Ctrl+Alt is how legacy terminals report AltGr. Preserve an
                // unbound printable character, including key repeats.
                if alt && modifiers.contains(KeyModifiers::CONTROL) {
                    return Some(Command::InsertText(character.to_string()));
                }
            }
            KeyCode::Left => {
                return Some(Command::Move {
                    motion: Motion::GroupLeft,
                    extend: shift,
                });
            }
            KeyCode::Right => {
                return Some(Command::Move {
                    motion: Motion::GroupRight,
                    extend: shift,
                });
            }
            KeyCode::Home => {
                return Some(Command::Move {
                    motion: Motion::DocumentStart,
                    extend: shift,
                });
            }
            KeyCode::End => {
                return Some(Command::Move {
                    motion: Motion::DocumentEnd,
                    extend: shift,
                });
            }
            KeyCode::Backspace => return Some(Command::DeleteWordBackward),
            KeyCode::Delete => return Some(Command::DeleteWordForward),
            _ => {}
        }
        return None;
    }

    match event.code {
        KeyCode::Left => Some(Command::Move {
            motion: Motion::Left,
            extend: shift,
        }),
        KeyCode::Right => Some(Command::Move {
            motion: Motion::Right,
            extend: shift,
        }),
        KeyCode::Up => Some(Command::Move {
            motion: Motion::Up,
            extend: shift,
        }),
        KeyCode::Down => Some(Command::Move {
            motion: Motion::Down,
            extend: shift,
        }),
        KeyCode::Home => Some(Command::Move {
            motion: Motion::LineStart,
            extend: shift,
        }),
        KeyCode::End => Some(Command::Move {
            motion: Motion::LineEnd,
            extend: shift,
        }),
        KeyCode::PageUp => Some(Command::Move {
            motion: Motion::PageUp,
            extend: shift,
        }),
        KeyCode::PageDown => Some(Command::Move {
            motion: Motion::PageDown,
            extend: shift,
        }),
        KeyCode::Enter => Some(Command::Newline),
        KeyCode::Backspace => Some(Command::DeleteBackward),
        KeyCode::Delete => Some(Command::DeleteForward),
        KeyCode::Tab => Some(Command::Indent),
        KeyCode::BackTab => Some(Command::Outdent),
        // Option/Alt may be part of entering a printable character. Reserved
        // transport/navigation chords were handled above. A control
        // character is never text.
        KeyCode::Char(character) if !is_stray_control(character) => {
            Some(Command::InsertText(character.to_string()))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode, modifiers: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, modifiers)
    }

    /// The regression guard for the whole dead-shortcut class of bug: no
    /// combination of platform and capability may ever spell a chord with ⌘.
    /// Dispatch still accepts Command everywhere (`primary` in
    /// `key_to_command`), but ⌘T, ⌘D, ⌘W and ⌘Q belong to the terminal, and a
    /// footer that advertises them sends the musician's keypress to iTerm2 or
    /// Ghostty instead of to the studio.
    #[test]
    fn no_chord_is_ever_spelled_with_command() {
        for capabilities in [
            KeyboardCapabilities::legacy(),
            KeyboardCapabilities::enhanced(),
            KeyboardCapabilities {
                enhanced: true,
                space_seen: false,
                super_seen: true,
            },
            KeyboardCapabilities {
                enhanced: false,
                space_seen: false,
                super_seen: true,
            },
        ] {
            let (previous, next) = capabilities.scene_switch_hints();
            let spellings = [
                capabilities.primary_label().to_owned(),
                capabilities.chord("Q"),
                capabilities.shift_chord("Z"),
                capabilities.evaluate_hint(),
                capabilities.stop_hint(),
                previous,
                next,
            ];
            for spelling in spellings {
                assert!(
                    !spelling.contains('\u{2318}'),
                    "{capabilities:?} spelled a chord with Command: {spelling}"
                );
            }
        }
        assert_eq!(KeyboardCapabilities::legacy().chord("Z"), "^Z");
        assert_eq!(
            KeyboardCapabilities::legacy().shift_chord("Z"),
            "^\u{21e7}Z"
        );
    }

    /// Conhost draws `\u{21e7}` as tofu (Consolas has no glyph for it), so a
    /// terminal without the capability gets the arrow stand-in instead -
    /// see `terminal::symbol`.
    #[test]
    fn shift_chord_falls_back_to_the_arrow_without_the_capability() {
        let _forced = crate::terminal::ForceSymbolsForTest::set(false);
        assert_eq!(
            KeyboardCapabilities::legacy().shift_chord("Z"),
            "^\u{2191}Z"
        );
        assert_eq!(
            KeyboardCapabilities::legacy().shift_function_key(1),
            "\u{2191}F1"
        );
    }

    #[test]
    fn portable_transport_and_enhanced_scene_hints_are_distinct() {
        for capabilities in [
            KeyboardCapabilities::legacy(),
            KeyboardCapabilities::enhanced(),
        ] {
            assert_eq!(capabilities.evaluate_hint(), "^S");
            assert_eq!(capabilities.stop_hint(), "^G");
        }
        assert_eq!(
            KeyboardCapabilities::legacy().scene_switch_hints(),
            ("F6".to_owned(), "F7".to_owned())
        );
        assert_eq!(
            KeyboardCapabilities::enhanced().scene_switch_hints(),
            ("^[".to_owned(), "^]".to_owned())
        );
        for capabilities in [
            KeyboardCapabilities::legacy(),
            KeyboardCapabilities::enhanced(),
        ] {
            assert_eq!(
                key_to_command(key(KeyCode::Char('s'), KeyModifiers::CONTROL), capabilities),
                Some(Command::Evaluate)
            );
            assert_eq!(
                key_to_command(key(KeyCode::Char('g'), KeyModifiers::CONTROL), capabilities),
                Some(Command::Stop)
            );
        }
        assert_eq!(
            key_to_command(
                key(KeyCode::Enter, KeyModifiers::CONTROL),
                KeyboardCapabilities::enhanced()
            ),
            Some(Command::Evaluate)
        );
        assert_eq!(
            key_to_command(
                key(KeyCode::Char(','), KeyModifiers::CONTROL),
                KeyboardCapabilities::enhanced()
            ),
            None
        );
        assert_eq!(
            key_to_command(
                key(KeyCode::Char('.'), KeyModifiers::CONTROL),
                KeyboardCapabilities::enhanced()
            ),
            Some(Command::Stop)
        );
        assert_eq!(
            key_to_command(
                key(KeyCode::Enter, KeyModifiers::CONTROL),
                KeyboardCapabilities::legacy()
            ),
            None
        );
        assert_eq!(
            key_to_command(
                key(KeyCode::Char(','), KeyModifiers::CONTROL),
                KeyboardCapabilities::legacy()
            ),
            None
        );
        assert_eq!(
            key_to_command(
                key(KeyCode::Char('.'), KeyModifiers::CONTROL),
                KeyboardCapabilities::legacy()
            ),
            None
        );
        assert_eq!(
            key_to_command(
                key(KeyCode::Enter, KeyModifiers::NONE),
                KeyboardCapabilities::legacy()
            ),
            Some(Command::Newline)
        );
    }

    #[test]
    fn releases_and_repeated_effectful_shortcuts_are_ignored() {
        let release = KeyEvent::new_with_kind(
            KeyCode::Char('x'),
            KeyModifiers::NONE,
            KeyEventKind::Release,
        );
        assert_eq!(
            key_to_command(release, KeyboardCapabilities::enhanced()),
            None
        );
        for (code, modifiers) in [
            (KeyCode::Enter, KeyModifiers::CONTROL),
            (KeyCode::Char('.'), KeyModifiers::CONTROL),
            (KeyCode::Char('g'), KeyModifiers::CONTROL),
            (KeyCode::Char('s'), KeyModifiers::CONTROL),
            (KeyCode::Char('z'), KeyModifiers::CONTROL),
            (KeyCode::Char('y'), KeyModifiers::CONTROL),
            (KeyCode::Char('f'), KeyModifiers::CONTROL),
            (KeyCode::Char('x'), KeyModifiers::CONTROL),
            (KeyCode::Char('v'), KeyModifiers::CONTROL),
        ] {
            let repeat = KeyEvent::new_with_kind(code, modifiers, KeyEventKind::Repeat);
            assert_eq!(
                key_to_command(repeat, KeyboardCapabilities::enhanced()),
                None
            );
        }
    }

    #[test]
    fn editable_keys_keep_repeating() {
        let repeat = |code, modifiers| {
            key_to_command(
                KeyEvent::new_with_kind(code, modifiers, KeyEventKind::Repeat),
                KeyboardCapabilities::enhanced(),
            )
        };
        assert_eq!(
            repeat(KeyCode::Char('x'), KeyModifiers::NONE),
            Some(Command::InsertText("x".into()))
        );
        assert_eq!(
            repeat(KeyCode::Backspace, KeyModifiers::NONE),
            Some(Command::DeleteBackward)
        );
        assert_eq!(
            repeat(KeyCode::Left, KeyModifiers::CONTROL),
            Some(Command::Move {
                motion: Motion::GroupLeft,
                extend: false,
            })
        );
        assert_eq!(
            repeat(
                KeyCode::Char('€'),
                KeyModifiers::CONTROL | KeyModifiers::ALT
            ),
            Some(Command::InsertText("€".into()))
        );
    }

    #[test]
    fn a_shifted_character_without_the_shift_bit_still_means_redo() {
        // What a Mac terminal speaking the progressive protocol delivers for
        // ⇧⌘Z: the shifted character, Super, and no Shift modifier.
        assert_eq!(
            key_to_command(
                key(KeyCode::Char('Z'), KeyModifiers::SUPER),
                KeyboardCapabilities::enhanced()
            ),
            Some(Command::Redo)
        );
        assert_eq!(
            key_to_command(
                key(KeyCode::Char('z'), KeyModifiers::SUPER),
                KeyboardCapabilities::enhanced()
            ),
            Some(Command::Undo)
        );
        assert_eq!(
            key_to_command(
                key(KeyCode::Char('Z'), KeyModifiers::CONTROL),
                KeyboardCapabilities::enhanced()
            ),
            Some(Command::Redo)
        );
    }

    #[test]
    fn standard_history_and_selection_keys_map() {
        assert_eq!(
            key_to_command(
                key(
                    KeyCode::Char('z'),
                    KeyModifiers::CONTROL | KeyModifiers::SHIFT
                ),
                KeyboardCapabilities::legacy()
            ),
            Some(Command::Redo)
        );
        assert_eq!(
            key_to_command(
                key(KeyCode::Left, KeyModifiers::ALT),
                KeyboardCapabilities::legacy()
            ),
            Some(Command::Move {
                motion: Motion::GroupLeft,
                extend: false
            })
        );
        assert_eq!(
            key_to_command(
                key(KeyCode::Left, KeyModifiers::SUPER),
                KeyboardCapabilities::enhanced()
            ),
            Some(Command::Move {
                motion: Motion::LineStart,
                extend: false
            })
        );
        assert_eq!(
            key_to_command(
                key(KeyCode::Left, KeyModifiers::CONTROL | KeyModifiers::SHIFT),
                KeyboardCapabilities::legacy()
            ),
            Some(Command::Move {
                motion: Motion::GroupLeft,
                extend: true
            })
        );
    }

    #[test]
    fn bracketed_paste_is_atomic_and_altgr_text_is_not_swallowed() {
        assert_eq!(
            event_to_input(
                Event::Paste("two\nlines".into()),
                KeyboardCapabilities::legacy()
            ),
            Some(EditorInput::Command(Command::PasteText(
                "two\nlines".into()
            )))
        );
        assert_eq!(
            key_to_command(
                key(
                    KeyCode::Char('€'),
                    KeyModifiers::CONTROL | KeyModifiers::ALT
                ),
                KeyboardCapabilities::legacy()
            ),
            Some(Command::InsertText("€".into()))
        );
    }

    #[test]
    fn a_raw_multiline_key_run_becomes_one_paste_event() {
        let text = "first\n  second\nthird";
        let mut batch = TerminalEventBatch::default();
        for character in text.chars() {
            let code = match character {
                '\n' => KeyCode::Enter,
                '\t' => KeyCode::Tab,
                character => KeyCode::Char(character),
            };
            batch.push(Event::Key(key(code, KeyModifiers::NONE)));
        }
        assert_eq!(batch.finish(), vec![Event::Paste(text.into())]);
    }

    fn text_keys(text: &str) -> Vec<Event> {
        text.chars()
            .map(|character| Event::Key(key(KeyCode::Char(character), KeyModifiers::NONE)))
            .collect()
    }

    #[test]
    fn short_absolute_paths_are_atomic_without_lowering_the_typing_threshold() {
        for text in [
            r"C:\test",
            r"c:\test",
            r"C:\x",
            r"C:\",
            "C:/x",
            r#""C:\x""#,
            "'/tmp'",
            r"\\s\x",
            "/tmp",
            r"C:\test2",
        ] {
            let mut batch = TerminalEventBatch::default();
            for event in text_keys(text) {
                batch.push(event);
            }
            assert_eq!(batch.finish(), vec![Event::Paste(text.into())], "{text:?}");
        }
        for text in [
            "c",
            "/",
            "a",
            "test",
            "nrat",
            "abcdefg",
            "C:foo",
            "C:",
            "./test",
            r"x\test",
            r#"s("bd")"#,
            r#""C:\x"#,
        ] {
            let keys = text_keys(text);
            let mut batch = TerminalEventBatch::default();
            for event in keys.clone() {
                batch.push(event);
            }
            assert_eq!(batch.finish(), keys, "ordinary typing: {text:?}");
        }
    }

    #[test]
    fn a_short_path_flushes_before_a_shortcut_without_leaking_into_later_keys() {
        let mut batch = TerminalEventBatch::default();
        for event in text_keys(r"C:\test") {
            batch.push(event);
        }
        let stop = Event::Key(key(KeyCode::F(8), KeyModifiers::NONE));
        batch.push(stop.clone());
        let letter = Event::Key(key(KeyCode::Char('a'), KeyModifiers::NONE));
        batch.push(letter.clone());
        assert_eq!(
            batch.finish(),
            vec![Event::Paste(r"C:\test".into()), stop, letter]
        );
    }

    #[test]
    fn ordinary_keys_and_key_repeats_are_not_relabelled_as_paste() {
        let enter = Event::Key(key(KeyCode::Enter, KeyModifiers::NONE));
        let repeat = Event::Key(KeyEvent::new_with_kind(
            KeyCode::Char('x'),
            KeyModifiers::NONE,
            KeyEventKind::Repeat,
        ));
        let mut batch = TerminalEventBatch::default();
        batch.push(enter.clone());
        batch.push(repeat.clone());
        assert_eq!(batch.finish(), vec![enter, repeat]);
    }
}
