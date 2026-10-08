//! What a key does, and the sheet row that changes it.
//!
//! Every chord the studio answers to goes through one table here. The
//! table holds the studio's own chords until a player learns a shortcut
//! over one: Settings ▸ Keybinds, Enter on a row, press the chord. What
//! is learnt is remembered in `studio.json` under `keybinds`, and every
//! surface - the editor, the chords on the menu, the help, the scene
//! strip, the footer hints - reads the same table, so a shortcut moves
//! everywhere at once and no footer advertises the old one.
//!
//! The model is the mapping page's, on keys: there are no per-feature key
//! maps to thread through, only one function - "what does this press
//! mean?" - and one inverse - "how is this action spelled?" - so a second
//! convention like the caret/⌘ drift cannot grow beside it.
//!
//! A chord can also be nobody's. Learning over an action's chord asks
//! first - Enter takes it and the action that held it goes unbound, Esc
//! keeps what the other action has - because an unbound action is quieter
//! than two actions on one key. And Enter itself is never a chord: it is
//! the key that arms a learn, confirms a take and answers every sheet,
//! and a page that could not press Enter could never be worked.

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers, ModifierKeyCode};
use serde::{Deserialize, Serialize};

/// Compact, platform-appropriate modifier labels for every shortcut hint.
/// Stored key names (such as `ctrl+shift+s`) stay unchanged.
pub fn shortcut_label(text: &str) -> std::borrow::Cow<'_, str> {
    shortcut_label_for_platform(text, cfg!(target_os = "macos"))
}

fn shortcut_label_for_platform(text: &str, macos: bool) -> std::borrow::Cow<'_, str> {
    let mut label = std::borrow::Cow::Borrowed(text);
    for (word, symbol) in [
        ("Control+", "^"),
        ("Ctrl+", "^"),
        ("Shift+", super::terminal::symbol("⇧")),
        ("Alt+", if macos { "⌥" } else { "Alt+" }),
        ("Option+", if macos { "⌥" } else { "Alt+" }),
    ] {
        if word != symbol && label.contains(word) {
            label = std::borrow::Cow::Owned(label.replace(word, symbol));
        }
    }
    label
}

/// Everything a keypress outside the text can ask the studio to do.
///
/// Actions are the stable names; keys are the changeable part. The
/// default chords are what the studio has always answered to, so a
/// binding nobody has touched behaves exactly as it did.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BindAction {
    // Transport
    Evaluate,
    Stop,
    RewindEvaluate,
    // Menus and surfaces
    MenuBar,
    Help,
    Settings,
    Devices,
    ThemePicker,
    /// The list for the place at the caret: the values an argument takes, or
    /// the names that complete a word. Elsewhere, the reference.
    Reference,
    /// The documentation of the function at the caret.
    Docs,
    PianoMode,
    SetPanel,
    Mixer,
    VisualsOne,
    VisualsTwo,
    Log,
    LogSticky,
    FocusPanels,
    Jobs,
    Memory,
    Export,
    SmartAction,
    Split,
    HopPane,
    // Editor
    Undo,
    Redo,
    Copy,
    Cut,
    Paste,
    SelectAll,
    ToggleComment,
    FirstError,
    // Scenes
    NewScene,
    DuplicateScene,
    RecordTake,
    /// Record a sample from the audio input into the recordings bank.
    RecordSample,
    RenameScene,
    CloseScene,
    LearnPad,
    ForgetPad,
    SceneRewind,
    PreviousScene,
    NextScene,
    // View
    Wrap,
    Zen,
    MasterUp,
    MasterDown,
    OpenSet,
    Quit,
    // Panels. Each one acts only where its panel has the keyboard, and
    // ships on an Alt letter: see `panel_letter`.
    /// Show a sample, score or tape file in the file manager.
    ShowFile,
    /// Rename a sample or a tape, or alias a bank.
    RenameFile,
    DeleteSample,
    /// Trim the silence around a take.
    TrimSample,
    FocusTimeline,
}

impl BindAction {
    /// Every action, in the order the Keybinds page lists them.
    pub const ALL: [BindAction; 54] = [
        // Transport
        Self::Evaluate,
        Self::Stop,
        Self::RewindEvaluate,
        // Menus and surfaces
        Self::MenuBar,
        Self::Help,
        Self::Settings,
        Self::Devices,
        Self::ThemePicker,
        Self::Reference,
        Self::Docs,
        Self::PianoMode,
        Self::SetPanel,
        Self::Mixer,
        Self::VisualsOne,
        Self::VisualsTwo,
        Self::Log,
        Self::LogSticky,
        Self::FocusPanels,
        Self::Jobs,
        Self::Memory,
        Self::Export,
        Self::SmartAction,
        Self::Split,
        Self::HopPane,
        // Editor
        Self::Undo,
        Self::Redo,
        Self::Copy,
        Self::Cut,
        Self::Paste,
        Self::SelectAll,
        Self::ToggleComment,
        Self::FirstError,
        // Scenes
        Self::NewScene,
        Self::DuplicateScene,
        Self::RecordTake,
        Self::RecordSample,
        Self::RenameScene,
        Self::CloseScene,
        Self::LearnPad,
        Self::ForgetPad,
        Self::SceneRewind,
        Self::PreviousScene,
        Self::NextScene,
        // View
        Self::Wrap,
        Self::Zen,
        Self::MasterUp,
        Self::MasterDown,
        Self::OpenSet,
        Self::Quit,
        // Panels
        Self::ShowFile,
        Self::RenameFile,
        Self::DeleteSample,
        Self::TrimSample,
        Self::FocusTimeline,
    ];

    /// The name the preferences file keeps, and what a row is found by.
    pub fn key(self) -> &'static str {
        match self {
            Self::Evaluate => "update",
            Self::Stop => "stop",
            Self::RewindEvaluate => "rewind-update",
            Self::MenuBar => "menu-bar",
            Self::Help => "help",
            Self::Settings => "settings",
            Self::Devices => "devices",
            Self::ThemePicker => "theme",
            // The name older preference files hold. It keeps the Ctrl+F action.
            Self::Reference => "reference",
            Self::Docs => "docs",
            Self::PianoMode => "piano-mode",
            Self::SetPanel => "set-panel",
            Self::Mixer => "mixer",
            Self::VisualsOne => "visuals-one",
            Self::VisualsTwo => "visuals-two",
            Self::Log => "log",
            Self::LogSticky => "log-sticky",
            Self::FocusPanels => "focus-panels",
            Self::Jobs => "jobs",
            Self::Memory => "memory",
            Self::Export => "export",
            Self::SmartAction => "smart-action",
            Self::Split => "split",
            Self::HopPane => "hop-pane",
            Self::Undo => "undo",
            Self::Redo => "redo",
            Self::Copy => "copy",
            Self::Cut => "cut",
            Self::Paste => "paste",
            Self::SelectAll => "select-all",
            Self::ToggleComment => "comment",
            Self::FirstError => "first-error",
            Self::NewScene => "new-scene",
            Self::DuplicateScene => "duplicate-scene",
            Self::RecordTake => "record-take",
            Self::RecordSample => "record-sample",
            Self::RenameScene => "rename-scene",
            Self::CloseScene => "close-scene",
            Self::LearnPad => "learn-pad",
            Self::ForgetPad => "forget-pad",
            Self::SceneRewind => "scene-rewind",
            Self::PreviousScene => "previous-scene",
            Self::NextScene => "next-scene",
            Self::Wrap => "word-wrap",
            Self::Zen => "zen-mode",
            Self::MasterUp => "master-up",
            Self::MasterDown => "master-down",
            Self::OpenSet => "open-set",
            Self::Quit => "quit",
            Self::ShowFile => "show-file",
            Self::RenameFile => "rename-file",
            Self::DeleteSample => "delete-sample",
            Self::TrimSample => "trim-sample",
            Self::FocusTimeline => "focus-timeline",
        }
    }

    /// The action a stored key names. Anything unreadable is nothing.
    pub fn parse_key(text: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|action| action.key() == text.trim())
    }

    /// What the row says the action is.
    pub fn label(self) -> &'static str {
        match self {
            Self::Evaluate => "update / play",
            Self::Stop => "stop all sound",
            Self::RewindEvaluate => "rewind update",
            Self::MenuBar => "open the menu bar",
            Self::Help => "keyboard reference",
            Self::Settings => "settings",
            Self::Devices => "devices",
            Self::ThemePicker => "theme picker",
            Self::Reference => "argument values, reference",
            Self::Docs => "docs for the function",
            Self::PianoMode => "piano mode",
            Self::SetPanel => "set panel",
            Self::Mixer => "mixer desk",
            Self::VisualsOne => "visuals 1",
            Self::VisualsTwo => "visuals 2",
            Self::Log => "log panel",
            Self::LogSticky => "keep the log on screen",
            Self::FocusPanels => "next panel",
            Self::Jobs => "background jobs",
            Self::Memory => "memory breakdown",
            Self::Export => "export scene",
            Self::SmartAction => "smart action",
            Self::Split => "split the editor",
            Self::HopPane => "hop between panes",
            Self::Undo => "undo",
            Self::Redo => "redo",
            Self::Copy => "copy",
            Self::Cut => "cut",
            Self::Paste => "paste",
            Self::SelectAll => "select all",
            Self::ToggleComment => "comment or uncomment",
            Self::FirstError => "first error / locate cursor",
            Self::NewScene => "new scene",
            Self::DuplicateScene => "duplicate scene",
            Self::RecordTake => "record take",
            Self::RecordSample => "record a sample",
            Self::RenameScene => "rename scene",
            Self::CloseScene => "close scene",
            Self::LearnPad => "learn MIDI pad",
            Self::ForgetPad => "forget pad",
            Self::SceneRewind => "rewind on play",
            Self::PreviousScene => "previous scene",
            Self::NextScene => "next scene",
            Self::Wrap => "word wrap",
            Self::Zen => "zen mode",
            Self::MasterUp => "master volume up",
            Self::MasterDown => "master volume down",
            Self::OpenSet => "open another set",
            Self::Quit => "quit",
            Self::ShowFile => "show selected file",
            Self::RenameFile => "rename sample / bank / session",
            Self::DeleteSample => "delete local sample",
            Self::TrimSample => "trim sample silence",
            Self::FocusTimeline => "focus tape timeline",
        }
    }

    /// The chord the action answers to until someone learns another.
    /// Written by hand rather than derived, because the defaults are what
    /// the studio has always answered to, and one look down this list
    /// says so.
    pub fn default_binding(self) -> KeyCombo {
        use KeyCode::*;
        let plain = |code: KeyCode| KeyCombo {
            code,
            control: false,
            shift: false,
        };
        let shift = |code: KeyCode| KeyCombo {
            code,
            control: false,
            shift: true,
        };
        let ctrl = |code: KeyCode| KeyCombo {
            code,
            control: true,
            shift: false,
        };
        let ctrl_shift = |code: KeyCode| KeyCombo {
            code,
            control: true,
            shift: true,
        };
        match self {
            // strudel.cc's own chords, the ones a Strudel player's hands
            // already know. Legacy input folds them into plain Enter and
            // plain `.`, and one terminal takes ^Enter for fullscreen: there
            // the fallback below takes over, and the menus follow it.
            Self::Evaluate => ctrl(Enter),
            Self::Stop => ctrl(Char('.')),
            Self::RewindEvaluate => ctrl_shift(Char('s')),
            Self::MenuBar => plain(F(1)),
            Self::Help => plain(F(1)),
            Self::Settings => ctrl(Char('o')),
            Self::Devices => ctrl(Char('p')),
            Self::ThemePicker => ctrl(Char('t')),
            Self::Reference => ctrl(Char('f')),
            Self::Docs => ctrl(Char('d')),
            // Ordinary letters keep editing until this explicit mode is
            // opened. Jobs ships unbound, leaving F12 free for the piano.
            Self::PianoMode => plain(F(12)),
            Self::SetPanel => ctrl(Char('b')),
            Self::Mixer => plain(F(4)),
            Self::VisualsOne => shift(F(1)),
            Self::VisualsTwo => shift(F(2)),
            Self::Log => plain(F(9)),
            // Beside the log's own F9, and beside F10, which hops the
            // editor's panes: ⇧F10 hops the panels.
            Self::LogSticky => shift(F(9)),
            Self::FocusPanels => shift(F(10)),
            Self::Jobs => plain(F(12)), // unused: Jobs ships unbound; open from View ▸ Background jobs
            Self::Memory => plain(F(12)), // unused: Memory ships unbound; open from the header
            Self::Export => ctrl_shift(Char('x')),
            Self::SmartAction => ctrl(Char('j')),
            Self::Split => ctrl(Char('e')),
            Self::HopPane => plain(F(10)),
            Self::Undo => ctrl(Char('z')),
            Self::Redo => ctrl_shift(Char('z')),
            Self::Copy => ctrl(Char('c')),
            Self::Cut => ctrl(Char('x')),
            Self::Paste => ctrl(Char('v')),
            Self::SelectAll => ctrl(Char('a')),
            Self::ToggleComment => ctrl(Char('/')),
            Self::FirstError => shift(F(3)),
            Self::NewScene => ctrl(Char('n')),
            Self::DuplicateScene => plain(F(3)),
            Self::RecordTake => ctrl_shift(Char('r')),
            // Pressed to start and again to stop, in the middle of playing,
            // so it has to arrive everywhere: a Ctrl letter is one byte
            // every terminal sends, where a function key needs Fn on a Mac
            // laptop and is taken by Guake, Yakuake and Tilix. H is the
            // letter nothing else here holds, and far from ^⇧R's take.
            // Backspace sends DEL, so ^H arrives as itself; only Ctrl+
            // Backspace shares its byte, on a legacy GNOME Terminal or
            // Konsole, where Alt+Backspace deletes the word instead.
            Self::RecordSample => ctrl(Char('h')),
            Self::RenameScene => ctrl(Char('r')),
            Self::CloseScene => ctrl(Char('w')),
            Self::LearnPad => ctrl(Char('l')),
            Self::ForgetPad => ctrl_shift(Char('l')),
            Self::SceneRewind => ctrl_shift(Char('u')),
            // The brackets are the chords the scene strip advertises; F6
            // and F7 stay as the plain aliases the app also answers to.
            Self::PreviousScene => ctrl(Char('[')),
            Self::NextScene => ctrl(Char(']')),
            Self::Wrap => ctrl(Char('u')),
            Self::Zen => ctrl(Char('k')),
            // The master fader is in the table so that it can be rebound. A
            // terminal can take ^⇧↑/↓ for itself (WezTerm gives ^⇧arrow to
            // its own pane navigation), and the studio then needs another
            // chord to change its volume from the keyboard.
            Self::MasterUp => ctrl_shift(KeyCode::Up),
            Self::MasterDown => ctrl_shift(KeyCode::Down),
            Self::OpenSet => ctrl_shift(Char('o')),
            Self::Quit => ctrl(Char('q')),
            // unused: a panel action ships on its Alt letter, outside the table
            Self::ShowFile
            | Self::RenameFile
            | Self::DeleteSample
            | Self::TrimSample
            | Self::FocusTimeline => plain(F(12)),
        }
    }

    /// The letter a panel reads with Alt for this action.
    ///
    /// The table holds no Alt chord, so a panel action ships with no chord
    /// in the table. The panel answers to Alt and this letter until the
    /// player learns a chord for the action. Trim and the timeline share
    /// `t`: the first belongs to the samples browser and the second to a
    /// tape, so the two never meet.
    pub fn panel_letter(self) -> Option<char> {
        match self {
            Self::ShowFile => Some('o'),
            Self::RenameFile => Some('r'),
            Self::DeleteSample => Some('d'),
            Self::TrimSample | Self::FocusTimeline => Some('t'),
            _ => None,
        }
    }

    /// Where to put this action when its first choice cannot arrive.
    ///
    /// Shifted letters lose their Shift in legacy input; other defaults
    /// conflict with terminal shortcuts. Function keys usually carry modifiers,
    /// subject to the reader limitations in terminals.json. Ctrl+Y / Ctrl+7
    /// preserve the editor's portable redo/comment spellings. Every candidate is still
    /// checked against the terminal profile before it is selected.
    ///
    /// Reachable preferred defaults stay in place; explicit learned choices
    /// are kept regardless of what the terminal profile predicts.
    pub fn fallback_binding(self) -> Option<KeyCombo> {
        let shift_key = |number: u8| KeyCombo {
            code: KeyCode::F(number),
            control: false,
            shift: true,
        };
        let ctrl = |code| KeyCombo {
            code,
            control: true,
            shift: false,
        };
        Some(match self {
            // Where ^Enter and ^. cannot arrive as themselves, the chords
            // every terminal delivers.
            Self::Evaluate => ctrl(KeyCode::Char('s')),
            Self::Stop => ctrl(KeyCode::Char('g')),
            // Beside F5, which is already the portable spelling of update.
            Self::RewindEvaluate => shift_key(5),
            Self::Redo => KeyCombo {
                code: KeyCode::Char('y'),
                control: true,
                shift: false,
            },
            Self::OpenSet => shift_key(6),
            Self::SceneRewind => shift_key(7),
            Self::ForgetPad => shift_key(8),
            Self::Export => shift_key(12),
            Self::RecordTake => shift_key(11),
            Self::PreviousScene => KeyCombo {
                code: KeyCode::F(6),
                control: false,
                shift: false,
            },
            Self::NextScene => KeyCombo {
                code: KeyCode::F(7),
                control: false,
                shift: false,
            },
            Self::ToggleComment => KeyCombo {
                code: KeyCode::Char('7'),
                control: true,
                shift: false,
            },
            // F11 is left of F12, so Ctrl+F11 lowers and Ctrl+F12 raises,
            // as minus is left of plus.
            Self::MasterUp => KeyCombo {
                code: KeyCode::F(12),
                control: true,
                shift: false,
            },
            Self::MasterDown => KeyCombo {
                code: KeyCode::F(11),
                control: true,
                shift: false,
            },
            // Some terminal context menus keep ⇧F10 for themselves.
            Self::FocusPanels => shift_key(4),
            // Legacy CSI 1;2R is mistaken for a cursor-position reply.
            Self::FirstError => shift_key(4),
            _ => return None,
        })
    }

    /// A control byte remains usable where even modified function keys are
    /// missing from the terminal's default keymap (notably Apple Terminal).
    pub fn fallback_bindings(self) -> impl Iterator<Item = KeyCombo> {
        self.fallback_binding()
            .into_iter()
            .chain((self == Self::FirstError).then_some(KeyCombo {
                code: KeyCode::Char('\\'),
                control: true,
                shift: false,
            }))
    }

    /// Older shortcuts whose context-sensitive behavior is still handled
    /// by the app/editor. Tracking ownership lets rebinding retire them and
    /// lets the learner ask before taking one.
    fn legacy_aliases(self) -> impl Iterator<Item = KeyCombo> {
        let ctrl = |code| KeyCombo {
            code,
            control: true,
            shift: false,
        };
        let ctrl_shift = |code| KeyCombo {
            code,
            control: true,
            shift: true,
        };
        let plain = |code| KeyCombo {
            code,
            control: false,
            shift: false,
        };
        use KeyCode::*;
        match self {
            // What the studio shipped with stays under the hand that learnt
            // it, beside strudel.cc's chords, without a third key on the row.
            Self::Evaluate => [Some(ctrl(Char('s'))), None],
            Self::Stop => [Some(ctrl(Char('g'))), None],
            Self::Reference => [Some(plain(F(2))), Some(ctrl(Null))],
            Self::Settings => [Some(ctrl_shift(Char('p'))), None],
            Self::HopPane => [Some(ctrl_shift(Char('e'))), None],
            Self::Zen => [Some(plain(F(11))), None],
            Self::ToggleComment => [Some(ctrl(Char('_'))), None],
            Self::PreviousScene => [Some(ctrl_shift(Char('['))), None],
            Self::NextScene => [Some(ctrl_shift(Char(']'))), None],
            _ => [None, None],
        }
        .into_iter()
        .flatten()
    }

    /// The second chord the studio also answers to - the transport's
    /// F-keys spelled on every footer, and the older spellings the panels
    /// grew beside their defaults: ^⇧D beside the log's F9, ^⇧M beside the
    /// mixer's F4, F6/F7 beside the scene strip's ^[/^]. An alias is never THE
    /// binding: nothing stores it, so the default beside it stays the
    /// chord the panel and the menus advertise. The model still knows
    /// them, though - the row shows it (`also F5`), the press fires from
    /// here while the default stands, a rebind landing on one is asked
    /// about like any key somebody holds - and rebinding the action's
    /// own chord retires the alias with it, so a second key can never be
    /// taken in silence or kept in secret.
    pub fn default_alias(self) -> Option<KeyCombo> {
        match self {
            Self::Redo => Some(KeyCombo {
                code: KeyCode::Char('y'),
                control: true,
                shift: false,
            }),
            Self::DuplicateScene => Some(KeyCombo {
                code: KeyCode::Char('n'),
                control: true,
                shift: true,
            }),
            Self::Evaluate => Some(KeyCombo {
                code: KeyCode::F(5),
                control: false,
                shift: false,
            }),
            Self::Stop => Some(KeyCombo {
                code: KeyCode::F(8),
                control: false,
                shift: false,
            }),
            Self::Log => Some(KeyCombo {
                code: KeyCode::Char('d'),
                control: true,
                shift: true,
            }),
            Self::Mixer => Some(KeyCombo {
                code: KeyCode::Char('m'),
                control: true,
                shift: true,
            }),
            Self::PreviousScene => Some(KeyCombo {
                code: KeyCode::F(6),
                control: false,
                shift: false,
            }),
            Self::NextScene => Some(KeyCombo {
                code: KeyCode::F(7),
                control: false,
                shift: false,
            }),
            _ => None,
        }
    }
}

/// What this terminal can actually deliver, so the studio never leaves a
/// default on a key that cannot arrive.
///
/// A shortcut can fail to arrive in two ways:
///
///   - The terminal binds the key itself. The player can remap it there,
///     and the conflicts table knows which terminals do it.
///   - The terminal cannot send the key. The legacy encoding has 32 control
///     codes and no bit for Shift, so `Ctrl+Shift+Z` and `Ctrl+Z` are the
///     same byte unless an enhanced keyboard protocol is negotiated. No
///     rebind at either end fixes that. Without a fallback, the plain
///     binding matches and the wrong action runs: ^⇧Z undoes, and redo is
///     out of reach on GNOME Terminal, Konsole, xterm, tmux and screen.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Reach {
    /// The kitty keyboard protocol answered, so Shift survives beside
    /// Control.
    pub enhanced: bool,
    /// What `terminal::identity()` reported, for the conflicts table.
    pub terminal: String,
}

impl Default for Reach {
    /// Before the studio has looked, every chord is taken at its word.
    ///
    /// Deliberately not derived: `enhanced: false` is the answer for a
    /// terminal known to lack the protocol, and it is the wrong answer for
    /// one nobody has asked yet. It would move defaults to their fallbacks
    /// on a table built in a test, and on the studio's own table before
    /// the handshake comes back.
    fn default() -> Self {
        Self {
            enhanced: true,
            terminal: String::new(),
        }
    }
}

impl Reach {
    /// Whether a chord pressed on this terminal reaches the studio as
    /// itself.
    pub fn delivers(&self, combo: &KeyCombo) -> bool {
        // Legacy input must both encode the chord distinctly and survive
        // our decoder. For example, modified F3 can look exactly like a
        // cursor-position reply even though it carries a modifier parameter.
        if !self.enhanced
            && super::terminal::profiles::legacy_input_limitation(&self.terminal, combo).is_some()
        {
            return false;
        }
        if combo.control && combo.shift && matches!(combo.code, KeyCode::Char(_)) && !self.enhanced
        {
            return false;
        }
        // Ctrl+[ collides with Esc in the legacy encoding. Keep both scene
        // directions on portable F-keys, as the scene decoder treats the
        // bracket pair alike. Ctrl+/ is decoded as Ctrl+7 on legacy input.
        if combo.control && !self.enhanced && matches!(combo.code, KeyCode::Char('[' | ']' | '/')) {
            return false;
        }
        // An empty name means terminal detection has not run yet. Reach's
        // default contract takes every chord at its word until it has.
        self.terminal.is_empty()
            || super::terminal::conflicts::steals(&self.terminal, combo).is_none()
    }
}

/// One chord: a key and the modifiers that have to be with it.
///
/// Alt is deliberately not part of the model. The studio binds nothing to
/// it - option is how a Mac types characters - so a chord learnt with alt
/// held is read without it rather than stored with it, and no binding can
/// ever take a key the terminal spends on text.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct KeyCombo {
    pub code: KeyCode,
    pub control: bool,
    pub shift: bool,
}

impl KeyCombo {
    /// The chord as `studio.json` keeps it: crossterm's own key names
    /// with explicit modifiers, which round-trips without a parser of its
    /// own and is editable by hand.
    pub fn key(&self) -> String {
        let modifiers = match (self.control, self.shift) {
            (true, true) => "ctrl+shift+",
            (true, false) => "ctrl+",
            (false, true) => "shift+",
            (false, false) => "",
        };
        format!("{modifiers}{}", key_name(self.code).to_ascii_lowercase())
    }

    /// Read a stored spelling; anything unreadable is no binding.
    pub fn parse(text: &str) -> Option<Self> {
        let mut control = false;
        let mut shift = false;
        let mut code = None;
        for part in text.trim().to_ascii_lowercase().split('+') {
            match part {
                "ctrl" | "control" => control = true,
                "shift" => shift = true,
                other if code.is_none() => code = key_named(other),
                _ => return None,
            }
        }
        Some(
            Self {
                code: code?,
                control,
                shift,
            }
            .normalized(),
        )
    }

    /// Whether a press IS this chord. An uppercase letter is shift's own
    /// evidence - the enhanced protocols deliver ⇧ chords as the shifted
    /// glyph with the shift bit dropped, so ⇧⌘Z arrives as `Z` with only
    /// ctrl set - and is matched the same way `key_to_command` reads it.
    pub fn matches(&self, event: &KeyEvent) -> bool {
        if event.kind == KeyEventKind::Release {
            return false;
        }
        let pressed = Self {
            code: event.code,
            control: event
                .modifiers
                .intersects(KeyModifiers::CONTROL | KeyModifiers::SUPER)
                || event.code == KeyCode::Null,
            shift: event.modifiers.contains(KeyModifiers::SHIFT),
        }
        .normalized();
        pressed == self.normalized()
    }

    fn normalized(mut self) -> Self {
        if let KeyCode::Char(character) = self.code {
            self.shift |= character.is_uppercase();
            self.code = KeyCode::Char(character.to_ascii_lowercase());
            if self.control {
                match character {
                    // Enhanced protocols may report the shifted glyph with
                    // or without SHIFT. These are one chord, not two owners.
                    '{' => {
                        self.code = KeyCode::Char('[');
                        self.shift = true;
                    }
                    '}' => {
                        self.code = KeyCode::Char(']');
                        self.shift = true;
                    }
                    ' ' => self.code = KeyCode::Null,
                    _ => {}
                }
            }
        }
        self
    }

    /// The chord the way every surface spells it: caret notation, the
    /// one convention the footer owns. `^S`, `^⇧S`, `F4`.
    pub fn hint(&self) -> String {
        let mut out = String::new();
        if self.control {
            out.push('^');
        }
        if self.shift {
            out.push_str(crate::terminal::symbol("⇧"));
        }
        out.push_str(&key_name(self.code));
        out
    }
}

/// The short names the studio already advertises - `F4`, `Esc`, `Del` -
/// with letters capitalised as the hints spell them; the file's spelling
/// is the lowercased form of the same.
fn key_name(code: KeyCode) -> String {
    match code {
        KeyCode::Char(' ') => "Space".to_owned(),
        KeyCode::Char(character) => character.to_ascii_uppercase().to_string(),
        KeyCode::F(number) => format!("F{number}"),
        KeyCode::Enter => "Enter".to_owned(),
        KeyCode::Esc => "Esc".to_owned(),
        KeyCode::Tab => "Tab".to_owned(),
        KeyCode::BackTab => "BackTab".to_owned(),
        KeyCode::Backspace => "Backspace".to_owned(),
        KeyCode::Delete => "Del".to_owned(),
        KeyCode::Insert => "Ins".to_owned(),
        KeyCode::Home => "Home".to_owned(),
        KeyCode::End => "End".to_owned(),
        KeyCode::PageUp => "PageUp".to_owned(),
        KeyCode::PageDown => "PageDown".to_owned(),
        KeyCode::Left => "Left".to_owned(),
        KeyCode::Right => "Right".to_owned(),
        KeyCode::Up => "Up".to_owned(),
        KeyCode::Down => "Down".to_owned(),
        KeyCode::Null => "Space".to_owned(), // A bare modifier press - refused as a chord, but a refused
        // learn still says what it would not take, and "Modifier(" is
        // not a word the footer owns.
        KeyCode::Modifier(which) => match which {
            ModifierKeyCode::LeftShift => "Left Shift".to_owned(),
            ModifierKeyCode::RightShift => "Right Shift".to_owned(),
            ModifierKeyCode::LeftControl => "Left Ctrl".to_owned(),
            ModifierKeyCode::RightControl => "Right Ctrl".to_owned(),
            ModifierKeyCode::LeftAlt => if cfg!(target_os = "macos") {
                "Left Option"
            } else {
                "Left Alt"
            }
            .to_owned(),
            ModifierKeyCode::RightAlt => if cfg!(target_os = "macos") {
                "Right Option"
            } else {
                "Right Alt"
            }
            .to_owned(),
            ModifierKeyCode::LeftSuper => "Left Super".to_owned(),
            ModifierKeyCode::RightSuper => "Right Super".to_owned(),
            ModifierKeyCode::LeftHyper => "Left Hyper".to_owned(),
            ModifierKeyCode::RightHyper => "Right Hyper".to_owned(),
            ModifierKeyCode::LeftMeta => "Left Meta".to_owned(),
            ModifierKeyCode::RightMeta => "Right Meta".to_owned(),
            ModifierKeyCode::IsoLevel3Shift => "AltGr".to_owned(),
            ModifierKeyCode::IsoLevel5Shift => "Level5 Shift".to_owned(),
        },
        other => format!("{other:?}"),
    }
}

/// The reverse of `key_name`, for reading the file.
fn key_named(name: &str) -> Option<KeyCode> {
    use KeyCode::*;
    Some(match name {
        "enter" => Enter,
        "esc" | "escape" => Esc,
        "tab" => Tab,
        "backtab" => BackTab,
        "backspace" => Backspace,
        "del" | "delete" => Delete,
        "ins" | "insert" => Insert,
        "home" => Home,
        "end" => End,
        "pageup" => PageUp,
        "pagedown" => PageDown,
        "left" => Left,
        "right" => Right,
        "up" => Up,
        "down" => Down,
        "space" | "null" => Null,
        other => {
            if let Some(number) = other.strip_prefix('f').and_then(|n| n.parse::<u8>().ok()) {
                return Some(F(number));
            }
            let mut chars = other.chars();
            match (chars.next(), chars.next()) {
                (Some(one), None) => Char(one),
                _ => return None,
            }
        }
    })
}

/// One learnt chord, as `studio.json` keeps it. Only the overrides are
/// written: an untouched action keeps the studio's chord, so a new
/// default reaches everyone who has not re-bound it.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct KeybindPrefs {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub bindings: Vec<KeybindPref>,
}

impl KeybindPrefs {
    /// Whether nothing is kept: no learnt rows, nothing carried through.
    /// The preferences file skips the whole section when this holds, so
    /// a studio nobody has re-bound writes the file it always did.
    pub fn is_empty(&self) -> bool {
        self.bindings.is_empty()
    }
}

/// One override: the action, by its key, and the chord, by its spelling.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct KeybindPref {
    /// The action's [`BindAction::key`]. An unknown name is kept as-is and
    /// ignored, the way an unreadable limiter line is: a newer rustel's
    /// action must not be erased by an older one writing the file back.
    pub action: String,
    /// The chord as [`KeyCombo::key`] spells it.
    pub chord: String,
}

/// What the Keybinds page's learn is waiting for.
///
/// Armed, the next press is the chord. When another action already
/// answers to that chord, the learn asks before it takes it.
///
/// ```text
/// Armed ---press---> plan(chord)
///                      Refused: stays Armed
///                      Take:    chord learnt, learn ends
///                      Ask:     Confirm
/// Confirm --Enter--> chord learnt, the holder goes unbound
/// Confirm --press--> plan(chord) again
/// either ---Esc----> learn ends, nothing changes
/// ```
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum KeybindLearn {
    /// Waiting for the chord to learn onto the action.
    Armed(BindAction),
    /// The chord is in hand, but another action holds it. `held_by` is
    /// who would lose it if the take is confirmed.
    Confirm {
        action: BindAction,
        chord: KeyCombo,
        held_by: BindAction,
    },
}

impl KeybindLearn {
    /// The action the learn would change, confirming or not.
    pub fn action(self) -> BindAction {
        match self {
            Self::Armed(action) | Self::Confirm { action, .. } => action,
        }
    }

    /// Arm a learn: wait for the chord to learn onto the action.
    pub fn arm(action: BindAction) -> Self {
        Self::Armed(action)
    }

    /// Whether a chord may be learnt at all. Enter is never one: it arms
    /// a learn, confirms a take and answers every sheet, and a page whose
    /// Enter had been taken could never be worked again short of editing
    /// `studio.json` by hand. Beside it stand the rest of the keys the
    /// studio leans on to stay workable: Esc and the Tab that page the
    /// sheet, Backspace and Delete - the sheet's unbind keys, and half
    /// of every editor's editing - and Space, which types spaces and
    /// steps every list, in every spelling the terminal reports it (the
    /// spacebar arrives as a plain character, `Null` is the file's word
    /// for it, and both are refused). The four arrows are refused too:
    /// they walk the page, the score's caret and every list the studio
    /// owns, and a page that could not walk its rows would be a page you
    /// could not reach by keyboard at all. Nothing here is a judgement
    /// about the keys - it is that each of them is the only way OUT of
    /// something, and a shortcut taken for an action is a door bricked up.
    pub fn is_a_chord(combo: KeyCombo) -> bool {
        !matches!(
            combo.code,
            KeyCode::Enter
                | KeyCode::Esc
                | KeyCode::Tab
                | KeyCode::BackTab
                | KeyCode::Backspace
                | KeyCode::Delete
                | KeyCode::Char(' ')
                | KeyCode::Null
                | KeyCode::Left
                | KeyCode::Right
                | KeyCode::Up
                | KeyCode::Down
                // A bare modifier press is not a chord anyone can mean:
                // terminals that answer the kitty keyboard report deliver
                // the finger's coming DOWN on ctrl or shift as its own
                // key, and a chord of the modifier alone can never fire -
                // the studio would bind nothing to it, and the press the
                // pianist meant (the chord it was a part of) would be
                // swallowed. Refused with the sheet's own keys.
                | KeyCode::Modifier(_)
        )
    }

    /// A character key with no ctrl held is a typing key, not a chord:
    /// most terminals fold ctrl plus a symbol down to the bare character
    /// (ctrl+' arrives as just '), so what the learner meant is not what
    /// any terminal can deliver - and a binding on a bare character
    /// would fire every time that character was typed into the score.
    /// Every chord the studio ships is ctrl-held or an F-key, so nothing
    /// is taken away: this only refuses what cannot arrive whole.
    pub fn is_a_typing_key(combo: KeyCombo) -> bool {
        matches!(combo.code, KeyCode::Char(_)) && !combo.control
    }

    /// What an armed learn makes of the chord just captured - decided
    /// here, done by the app, which is the only side that can save prefs
    /// and redraw. A chord nobody holds is [`KeybindCapture::Take`]; a
    /// chord another action answers to is [`KeybindCapture::Ask`], and
    /// the app holds it as a `Confirm` until Enter takes it or Esc keeps
    /// what the other action has. A reserved key - or a bare character,
    /// which is a typing key no terminal delivers a ctrl'd symbol as -
    /// comes back as [`KeybindCapture::Refused`], chord in hand so the
    /// refusal can name it: the learn stays armed, nothing moved.
    pub fn plan(self, table: &Keybinds, chord: KeyCombo) -> KeybindCapture {
        let action = self.action();
        if !Self::is_a_chord(chord) || Self::is_a_typing_key(chord) {
            return KeybindCapture::Refused { chord };
        }
        match table.holder_of(chord).filter(|held| *held != action) {
            Some(held_by) => KeybindCapture::Ask {
                action,
                chord,
                held_by,
            },
            None => KeybindCapture::Take { action, chord },
        }
    }
}

/// What an armed learn makes of a chord, as [`KeybindLearn::plan`] says.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum KeybindCapture {
    /// A key that can never be a chord - the sheet's own, the reserved
    /// family, or a bare typing character - refused with the chord in
    /// hand so the word can name it. The learn stays armed as it was.
    Refused { chord: KeyCombo },
    /// The chord is free: learning it disturbs nobody.
    Take { action: BindAction, chord: KeyCombo },
    /// The chord is another action's; the app asks before taking it.
    Ask {
        action: BindAction,
        chord: KeyCombo,
        held_by: BindAction,
    },
}

/// The actions the studio ships with no chord, on purpose. Background jobs
/// and Memory open from their View menu rows, and the memory breakdown from
/// the header's figures too. They are looked at now and then, not played, and
/// every plain function key and Ctrl letter already means something a set
/// is performed with; a player who wants one learns it in Settings ▸
/// Keybinds.
///
/// The panel actions ship with no chord in the table too. Each one answers
/// to its [`BindAction::panel_letter`] with Alt until a chord is learnt.
pub const SHIPS_UNBOUND: &[BindAction] = &[
    BindAction::Jobs,
    BindAction::Memory,
    BindAction::ShowFile,
    BindAction::RenameFile,
    BindAction::DeleteSample,
    BindAction::TrimSample,
    BindAction::FocusTimeline,
];

/// The keybinding table: what every action answers to, once the learner
/// has had its say. With no overrides it is the studio's own chords.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Keybinds {
    overrides: Vec<(BindAction, KeyCombo)>,
    /// Actions told to mean nothing. A chord learnt away from an action
    /// leaves it here - quieter than two actions on one key - and the
    /// preferences file carries it, so a restart does not quietly hand
    /// the retired chord back to an action the player took it from.
    unbound: Vec<BindAction>,
    /// Rows this build does not know, carried untouched so an older
    /// rustel writing the file back does not erase a newer one's bindings.
    /// An unreadable limiter line is kept the same way.
    unknown: Vec<KeybindPref>,
    /// What the terminal can deliver. Empty until the studio has looked,
    /// which reads as "everything arrives" - the right answer for a test
    /// and for the moment before the handshake comes back.
    reach: Reach,
    /// Resolved once whenever preferences or terminal capabilities change.
    /// Every lookup and hint reads these same collision-free assignments.
    effective: [Option<KeyCombo>; BindAction::ALL.len()],
    aliases: [Option<KeyCombo>; BindAction::ALL.len()],
}

impl Default for Keybinds {
    fn default() -> Self {
        let mut table = Self {
            overrides: Vec::new(),
            unbound: SHIPS_UNBOUND.to_vec(),
            unknown: Vec::new(),
            reach: Reach::default(),
            effective: [None; BindAction::ALL.len()],
            aliases: [None; BindAction::ALL.len()],
        };
        table.resolve();
        table
    }
}

impl Keybinds {
    /// The effective primary shortcut, after terminal capabilities and
    /// user choices have been applied. No reachable candidate means None.
    pub fn binding(&self, action: BindAction) -> Option<KeyCombo> {
        self.effective[action as usize]
    }

    /// A secondary shortcut that really remains available on this terminal.
    /// An alias selected as the primary is not advertised a second time.
    pub fn effective_alias(&self, action: BindAction) -> Option<KeyCombo> {
        self.aliases[action as usize]
    }

    /// The second chord a surface names beside the primary.
    ///
    /// Ctrl+Space is the one older spelling that is named, because other
    /// editors use it for completion. The terminal profile decides whether
    /// it arrives, so a system that takes it does not show it.
    pub fn advertised_alias(&self, action: BindAction) -> Option<KeyCombo> {
        self.effective_alias(action).or_else(|| {
            let space = KeyCombo {
                code: KeyCode::Null,
                control: true,
                shift: false,
            };
            (action == BindAction::Reference
                && self.binding(action) != Some(space)
                && self.accepts_legacy_alias(
                    action,
                    &KeyEvent::new(KeyCode::Null, KeyModifiers::CONTROL),
                ))
            .then_some(space)
        })
    }

    /// Explicit user choices survive capability changes verbatim.
    pub fn set_reach(&mut self, reach: Reach) {
        self.reach = reach;
        self.resolve();
    }

    pub fn normalize_terminal_key(&self, event: &mut KeyEvent) {
        if self.reach.enhanced
            || event.modifiers.intersects(
                KeyModifiers::ALT | KeyModifiers::SUPER | KeyModifiers::HYPER | KeyModifiers::META,
            )
        {
            return;
        }
        let received = KeyCombo {
            code: event.code,
            control: event.modifiers.contains(KeyModifiers::CONTROL),
            shift: event.modifiers.contains(KeyModifiers::SHIFT),
        };
        if let Some(physical) =
            super::terminal::profiles::legacy_key(&self.reach.terminal, &received)
        {
            event.code = physical.code;
            event.modifiers.set(KeyModifiers::CONTROL, physical.control);
            event.modifiers.set(KeyModifiers::SHIFT, physical.shift);
        }
    }

    /// Reachability for contextual shortcut hints outside the primary row.
    pub fn delivers(&self, combo: &KeyCombo) -> bool {
        self.reach.delivers(&combo.normalized())
    }

    fn portable_candidates() -> impl Iterator<Item = KeyCombo> {
        (1..=12)
            .map(|number| KeyCombo {
                code: KeyCode::F(number),
                control: true,
                shift: false,
            })
            .chain((1..=12).map(|number| KeyCombo {
                code: KeyCode::F(number),
                control: false,
                shift: true,
            }))
            .chain((1..=12).map(|number| KeyCombo {
                code: KeyCode::F(number),
                control: true,
                shift: true,
            }))
            // Plain spare function keys also work in terminals with no
            // modifier mappings. Preferred primaries are already reserved.
            .chain((1..=12).map(|number| KeyCombo {
                code: KeyCode::F(number),
                control: false,
                shift: false,
            }))
    }

    fn default_for(&self, action: BindAction) -> Option<KeyCombo> {
        // Shipped with no key: its placeholder default is nobody's chord,
        // so a chord learnt onto it is always the player's, and saved.
        if Self::ships_unbound(action) {
            return None;
        }
        std::iter::once(action.default_binding())
            .chain(action.fallback_bindings())
            .chain(action.default_alias())
            .chain(action.legacy_aliases())
            .chain(Self::portable_candidates())
            .find(|combo| self.delivers_for(action, combo))
    }

    fn delivers_for(&self, action: BindAction, combo: &KeyCombo) -> bool {
        if matches!(
            (action, combo.code),
            (BindAction::Evaluate, KeyCode::Enter) | (BindAction::Stop, KeyCode::Char('.'))
        ) && !self.reach.enhanced
        {
            return false;
        }
        if action == BindAction::ToggleComment
            && combo.code == KeyCode::Char('_')
            && self.reach.enhanced
        {
            return false;
        }
        self.reach.delivers(combo)
    }

    /// Whether an older shortcut still belongs to this action. Contextual
    /// app/editor handlers ask this before applying their existing behavior.
    pub fn accepts_legacy_alias(&self, action: BindAction, event: &KeyEvent) -> bool {
        if event.kind != KeyEventKind::Press || self.is_unbound(action) || self.overridden(action) {
            return false;
        }
        action.legacy_aliases().any(|combo| {
            combo.matches(event)
                && self.delivers_for(action, &combo)
                && self
                    .direct_holder_of(combo)
                    .is_none_or(|owner| owner == action)
        })
    }

    fn direct_holder_of(&self, combo: KeyCombo) -> Option<BindAction> {
        self.default_holder_of(combo).or_else(|| {
            BindAction::ALL
                .into_iter()
                .find(|action| self.effective_alias(*action) == Some(combo))
        })
    }

    fn available(&self, action: BindAction, combo: KeyCombo) -> bool {
        BindAction::ALL.into_iter().all(|other| {
            self.binding(other) != Some(combo)
                // F1 intentionally retains its context-sensitive help/menu
                // behavior while both actions are on their shipped default.
                || (matches!((action, other),
                    (BindAction::Help, BindAction::MenuBar)
                    | (BindAction::MenuBar, BindAction::Help))
                    && !self.overridden(other)
                    && combo == action.default_binding())
        })
    }

    fn resolve(&mut self) {
        self.effective.fill(None);
        self.aliases.fill(None);
        // Preferences are promises, including keys the profile thinks are
        // blocked. A person may have reconfigured their terminal already.
        for &(action, combo) in &self.overrides {
            if !self.is_unbound(action) {
                self.effective[action as usize] = Some(combo);
            }
        }
        // Reserve all preferred defaults before selecting any fallback, so
        // adapting an early row can never silently take a later row's key.
        for action in BindAction::ALL {
            if self.is_unbound(action) || self.overridden(action) {
                continue;
            }
            let combo = action.default_binding();
            if self.delivers_for(action, &combo) && self.available(action, combo) {
                self.effective[action as usize] = Some(combo);
            }
        }
        // Reserve dedicated fallbacks before using the general spare keys.
        for action in BindAction::ALL {
            if self.is_unbound(action) || self.binding(action).is_some() {
                continue;
            }
            self.effective[action as usize] = action
                .fallback_bindings()
                .chain(action.default_alias())
                .chain(action.legacy_aliases())
                .find(|combo| self.delivers_for(action, combo) && self.available(action, *combo));
        }
        for action in BindAction::ALL {
            if self.is_unbound(action) || self.binding(action).is_some() {
                continue;
            }
            self.effective[action as usize] = Self::portable_candidates()
                .find(|combo| self.reach.delivers(combo) && self.available(action, *combo));
        }
        // A master key that moved to a spare key can land left of the other
        // one. Keep the lower key left of the raise key on a shared row.
        let (up, down) = (
            BindAction::MasterUp as usize,
            BindAction::MasterDown as usize,
        );
        if !self.overridden(BindAction::MasterUp)
            && !self.overridden(BindAction::MasterDown)
            && let (Some(raise), Some(lower)) = (self.effective[up], self.effective[down])
            && let (KeyCode::F(raise_key), KeyCode::F(lower_key)) = (raise.code, lower.code)
            && (raise.control, raise.shift) == (lower.control, lower.shift)
            && raise_key < lower_key
        {
            self.effective.swap(up, down);
        }
        for action in BindAction::ALL {
            if self.is_unbound(action) || self.overridden(action) {
                continue;
            }
            if let Some(alias) = action.default_alias()
                && self.reach.delivers(&alias)
                && self.available(action, alias)
                && !self.aliases.contains(&Some(alias))
            {
                self.aliases[action as usize] = Some(alias);
            }
        }
    }

    /// The chord as every surface spells it: `^S`, `F4` - or nothing for
    /// an unbound action, which every surface reads as no shortcut. A
    /// panel action with no learnt chord is spelled as its Alt letter.
    pub fn hint(&self, action: BindAction) -> String {
        match (self.binding(action), action.panel_letter()) {
            (Some(combo), _) => combo.hint(),
            (None, Some(letter)) => {
                shortcut_label(&format!("Alt+{}", letter.to_ascii_uppercase())).into_owned()
            }
            (None, None) => String::new(),
        }
    }

    /// Whether the action has been told to mean nothing: no chord is
    /// theirs until one is learnt back onto them.
    pub fn is_unbound(&self, action: BindAction) -> bool {
        self.unbound.contains(&action)
    }

    /// Whether the studio ships the action without a chord on purpose:
    /// see [`SHIPS_UNBOUND`]. Such an unbinding is the shipped state, not
    /// the user's departure from it, so the preferences do not write a
    /// row for it: the file is the delta, and an untouched table writes
    /// nothing at all.
    fn ships_unbound(action: BindAction) -> bool {
        SHIPS_UNBOUND.contains(&action)
    }

    /// Resolve a press using the same effective chords that the UI shows.
    pub fn action_for(&self, event: &KeyEvent) -> Option<BindAction> {
        if event.kind != KeyEventKind::Press {
            return None;
        }
        BindAction::ALL
            .into_iter()
            .find(|action| {
                self.binding(*action)
                    .is_some_and(|combo| combo.matches(event))
                    || self
                        .effective_alias(*action)
                        .is_some_and(|combo| combo.matches(event))
            })
            .or_else(|| {
                BindAction::ALL
                    .into_iter()
                    .find(|action| self.accepts_legacy_alias(*action, event))
            })
            .or_else(|| self.browser_spelling(event))
    }

    /// Alt+Enter and Alt+. - what Firefox leaves a Strudel player, since it
    /// keeps ^Enter and ^. for itself.
    ///
    /// Answered, never advertised: the row keeps one chord and one `also`,
    /// and a terminal that takes Alt+Enter (Windows Terminal's fullscreen)
    /// simply never sends it. Only for an action on its shipped chords: a
    /// rebind retires every spelling the studio knew, this one with them.
    pub fn browser_spelling(&self, event: &KeyEvent) -> Option<BindAction> {
        if event.modifiers != KeyModifiers::ALT {
            return None;
        }
        let action = match event.code {
            KeyCode::Enter => BindAction::Evaluate,
            KeyCode::Char('.') => BindAction::Stop,
            _ => return None,
        };
        (!self.is_unbound(action) && !self.overridden(action)).then_some(action)
    }

    /// Overrides and adapted defaults run before the legacy context-sensitive
    /// handlers. Unchanged defaults keep their established panel behavior.
    pub fn adapted_action_for(&self, event: &KeyEvent) -> Option<BindAction> {
        let action = self.action_for(event)?;
        (self.overridden(action)
            || self
                .binding(action)
                .is_some_and(|combo| combo != action.default_binding() && combo.matches(event)))
        .then_some(action)
    }

    /// Whether an action has a learnt chord rather than the default.
    pub fn overridden(&self, action: BindAction) -> bool {
        self.overrides.iter().any(|(bound, _)| *bound == action)
    }

    /// The learnt chord on an action, if it has one. The app's cascade
    /// asks this of every press: a chord an override names dispatches as
    /// that action before the built-in spellings get their turn, and the
    /// key an override moved off is not here, so the cascade's own
    /// reading of it stands, freed.
    pub fn override_for(&self, action: BindAction) -> Option<KeyCombo> {
        self.overrides
            .iter()
            .find(|(bound, _)| *bound == action)
            .map(|(_, combo)| *combo)
    }

    /// The action a press claims as a LEARNT chord, or nothing. The
    /// cascade's one question: only an override answers, never a default.
    pub fn override_action_for(&self, event: &KeyEvent) -> Option<BindAction> {
        self.overrides
            .iter()
            .find(|(_, combo)| combo.matches(event))
            .map(|(action, _)| *action)
    }

    /// Keep the legacy cascade from firing moved, blocked or retired
    /// spellings. In particular, retiring a fallback also retires its old
    /// hardcoded behavior (Ctrl+Y redo, for example).
    pub fn overrules(&self, event: &KeyEvent) -> bool {
        if event.kind != KeyEventKind::Press || event.modifiers.contains(KeyModifiers::ALT) {
            return false;
        }
        if self.adapted_action_for(event).is_some() {
            return true;
        }
        if self.action_for(event).is_some() {
            return false;
        }
        BindAction::ALL.into_iter().any(|action| {
            std::iter::once(action.default_binding())
                .chain(action.fallback_bindings())
                .chain(action.default_alias())
                .chain(action.legacy_aliases())
                .any(|combo| combo.matches(event))
        }) || Self::portable_candidates().any(|combo| combo.matches(event))
    }

    /// The current primary or secondary owner of a shortcut.
    pub fn holder_of(&self, combo: KeyCombo) -> Option<BindAction> {
        let combo = combo.normalized();
        let mut modifiers = KeyModifiers::NONE;
        if combo.control {
            modifiers |= KeyModifiers::CONTROL;
        }
        if combo.shift {
            modifiers |= KeyModifiers::SHIFT;
        }
        let event = KeyEvent::new(combo.code, modifiers);
        self.direct_holder_of(combo).or_else(|| {
            BindAction::ALL
                .into_iter()
                .find(|action| self.accepts_legacy_alias(*action, &event))
        })
    }

    /// The primary owner displaced by learning a shortcut. Taking only an
    /// alias shadows it without unbinding its action's separate primary.
    pub fn default_holder_of(&self, combo: KeyCombo) -> Option<BindAction> {
        let combo = combo.normalized();
        BindAction::ALL
            .into_iter()
            .find(|action| self.binding(*action) == Some(combo))
    }

    /// Learn a chord for an action, or put the studio's own back.
    ///
    /// `Some` takes the chord away from whoever held it - one chord, one
    /// meaning - and the action displaced goes unbound rather than
    /// falling back into a chord that has just been promised to somebody
    /// else. `None` puts the default back and clears an unbinding, and for
    /// an action shipped with no key, the default is no key.
    pub fn learn(&mut self, action: BindAction, combo: Option<KeyCombo>) {
        let combo = combo.map(KeyCombo::normalized);
        if combo.is_some_and(|combo| {
            !KeybindLearn::is_a_chord(combo) || KeybindLearn::is_a_typing_key(combo)
        }) {
            return;
        }
        let Some(combo) = combo else {
            self.overrides.retain(|(bound, _)| *bound != action);
            self.unbound.retain(|bound| *bound != action);
            if Self::ships_unbound(action) {
                self.unbound.push(action);
            }
            self.resolve();
            return;
        };
        if self.binding(action) == Some(combo) {
            return;
        }
        let displaced = self.default_holder_of(combo).filter(|held| *held != action);
        self.overrides.retain(|(bound, _)| *bound != action);
        self.unbound.retain(|bound| *bound != action);
        if let Some(displaced) = displaced {
            self.overrides.retain(|(bound, _)| *bound != displaced);
            if !self.unbound.contains(&displaced) {
                self.unbound.push(displaced);
            }
        }
        if Some(combo) != self.default_for(action) {
            self.overrides.push((action, combo));
        }
        self.resolve();
    }

    /// Unbinding retires the primary, every fallback and every alias.
    pub fn unbind(&mut self, action: BindAction) {
        self.overrides.retain(|(bound, _)| *bound != action);
        if !self.unbound.contains(&action) {
            self.unbound.push(action);
        }
        self.resolve();
    }

    /// Restore every shortcut to the defaults for the active terminal profile.
    /// This also clears explicit unbindings and unknown saved action overrides.
    pub fn reset_all(&mut self) {
        let reach = self.reach.clone();
        *self = Self::default();
        self.set_reach(reach);
    }

    /// The overrides, as the preferences keep them: the learnt rows and
    /// whatever this build did not recognise, carried through.
    pub fn prefs(&self) -> KeybindPrefs {
        let mut bindings = self
            .overrides
            .iter()
            .map(|(action, combo)| KeybindPref {
                action: action.key().to_owned(),
                chord: combo.key(),
            })
            .collect::<Vec<_>>();
        // An unbound action is a row too: an empty chord is the file's
        // word for "means nothing", so a restart does not quietly hand
        // the retired chord back - unless the studio shipped them
        // unbound on purpose, where the row would be a transcript of
        // the default rather than a departure from it.
        bindings.extend(
            self.unbound
                .iter()
                .filter(|action| !Self::ships_unbound(**action))
                .map(|action| KeybindPref {
                    action: action.key().to_owned(),
                    chord: String::new(),
                }),
        );
        bindings.extend(self.unknown.iter().cloned());
        KeybindPrefs { bindings }
    }

    /// Take the overrides the file holds. Unreadable rows are skipped;
    /// the file's own spelling is written back untouched on the next
    /// save, so nothing a newer rustel wrote is lost here either.
    pub fn restore(&mut self, prefs: &KeybindPrefs) {
        self.overrides.clear();
        self.unbound = SHIPS_UNBOUND.to_vec();
        self.unknown.clear();
        for row in &prefs.bindings {
            match (
                BindAction::parse_key(&row.action),
                KeyCombo::parse(&row.chord),
            ) {
                (Some(action), None) if row.chord.trim().is_empty() => {
                    self.overrides.retain(|(bound, _)| *bound != action);
                    if !self.unbound.contains(&action) {
                        self.unbound.push(action);
                    }
                }
                (Some(action), Some(combo))
                    if (KeybindLearn::is_a_chord(combo)
                        && !KeybindLearn::is_a_typing_key(combo))
                        || combo == action.default_binding() =>
                {
                    // Loading is not learning. An explicit stored chord must
                    // survive even when it happens to equal this terminal's
                    // default, or moving between terminals would erase it.
                    self.overrides
                        .retain(|(bound, held)| *bound != action && *held != combo);
                    self.unbound.retain(|bound| *bound != action);
                    self.overrides.push((action, combo));
                }
                (None, _) => self.unknown.push(row.clone()),
                _ => {}
            }
        }
        self.resolve();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shortcut_hints_use_compact_platform_modifiers_without_changing_bindings() {
        let text = "Ctrl+Shift+S · Control+O · Alt+R · Option+O";
        assert_eq!(
            shortcut_label_for_platform(text, true),
            "^⇧S · ^O · ⌥R · ⌥O"
        );
        assert_eq!(
            shortcut_label_for_platform(text, false),
            "^⇧S · ^O · Alt+R · Alt+O"
        );
        assert_eq!(shortcut_label_for_platform("^⇧S · ⌥R", true), "^⇧S · ⌥R");
        let binding = Keybinds::default();
        assert_eq!(
            binding.binding(BindAction::RewindEvaluate).unwrap().key(),
            "ctrl+shift+s"
        );
    }

    fn press(code: KeyCode, modifiers: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, modifiers)
    }

    /// The defaults are the chords a hand already knows. Update and stop
    /// are strudel.cc's own, ^Enter and ^.; the chords the studio shipped
    /// with, ^S and ^G, keep answering beside them, and a terminal that
    /// cannot send ^Enter as itself gets them as the default instead.
    /// Renaming one is a breaking change for every hand that knows it.
    #[test]
    fn defaults_are_the_studio_s_own_chords() {
        let binds = Keybinds::default();
        let evaluate = binds.binding(BindAction::Evaluate).unwrap();
        assert_eq!(evaluate.key(), "ctrl+enter");
        assert_eq!(evaluate.hint(), "^Enter");
        assert_eq!(binds.binding(BindAction::Stop).unwrap().key(), "ctrl+.");
        assert_eq!(
            binds.action_for(&press(KeyCode::Char('s'), KeyModifiers::CONTROL)),
            Some(BindAction::Evaluate),
            "the shipped ^S still updates"
        );
        assert_eq!(
            binds.action_for(&press(KeyCode::Char('g'), KeyModifiers::CONTROL)),
            Some(BindAction::Stop),
            "the shipped ^G still stops"
        );
        assert_eq!(
            binds.action_for(&press(KeyCode::F(5), KeyModifiers::NONE)),
            Some(BindAction::Evaluate)
        );
        assert_eq!(binds.binding(BindAction::Mixer).unwrap().key(), "f4");
        assert_eq!(binds.binding(BindAction::Mixer).unwrap().hint(), "F4");
        assert_eq!(
            binds.binding(BindAction::RewindEvaluate).unwrap().key(),
            "ctrl+shift+s"
        );
        assert_eq!(binds.hint(BindAction::RewindEvaluate), "^\u{21e7}S");
        assert_eq!(binds.hint(BindAction::Quit), "^Q");
    }

    /// A terminal that folds ^Enter into Enter and ^. into `.` gets the
    /// shipped chords as the defaults it shows, and ^Enter is not claimed.
    #[test]
    fn a_legacy_terminal_updates_on_ctrl_s_and_stops_on_ctrl_g() {
        let mut binds = Keybinds::default();
        binds.set_reach(Reach {
            enhanced: false,
            terminal: "xterm-256color".into(),
        });
        assert_eq!(binds.binding(BindAction::Evaluate).unwrap().key(), "ctrl+s");
        assert_eq!(binds.binding(BindAction::Stop).unwrap().key(), "ctrl+g");
        assert_eq!(
            binds.effective_alias(BindAction::Evaluate).unwrap().key(),
            "f5",
            "one also, as before"
        );
    }

    /// Firefox keeps ^Enter and ^. for itself, so Alt+Enter and Alt+. are
    /// answered here. The hint does not show them, and they retire with
    /// the action's own chord like every other spelling.
    #[test]
    fn the_browser_spellings_answer_without_being_shown() {
        let mut binds = Keybinds::default();
        assert_eq!(
            binds.action_for(&press(KeyCode::Enter, KeyModifiers::ALT)),
            Some(BindAction::Evaluate)
        );
        assert_eq!(
            binds.action_for(&press(KeyCode::Char('.'), KeyModifiers::ALT)),
            Some(BindAction::Stop)
        );
        assert_eq!(
            binds.action_for(&press(KeyCode::Enter, KeyModifiers::NONE)),
            None,
            "a plain Enter is the editor's newline"
        );
        assert_eq!(
            binds.hint(BindAction::Evaluate),
            "^Enter",
            "nothing added to the row"
        );

        binds.learn(BindAction::Evaluate, Some(KeyCombo::parse("f12").unwrap()));
        assert_eq!(
            binds.action_for(&press(KeyCode::Enter, KeyModifiers::ALT)),
            None,
            "a rebind retires the spelling the studio knew"
        );
        binds.unbind(BindAction::Stop);
        assert_eq!(
            binds.action_for(&press(KeyCode::Char('.'), KeyModifiers::ALT)),
            None,
            "an unbound stop answers to nothing"
        );
    }

    /// Every action has a label and a chord that names it, and the file
    /// round-trips: what is saved is what comes back.
    #[test]
    fn every_action_is_named_and_round_trips() {
        let mut binds = Keybinds::default();
        for action in BindAction::ALL {
            assert!(!action.label().is_empty());
            assert!(!action.key().is_empty());
            if SHIPS_UNBOUND.contains(&action) {
                assert!(
                    binds.binding(action).is_none(),
                    "{} has no default shortcut",
                    action.label()
                );
                continue;
            }
            let chord = binds.binding(action).expect("every default is a chord");
            let spelled = chord.key();
            let read = KeyCombo::parse(&spelled).expect("the spelling parses");
            assert_eq!(read, chord, "{spelled} does not read back");
            assert!(chord.matches(&press(chord.code, chord_modifiers(chord))));
        }
        binds.learn(
            BindAction::Evaluate,
            Some(KeyCombo::parse("ctrl+shift+f9").unwrap()),
        );
        let prefs = binds.prefs();
        let json = serde_json::to_string(&prefs).unwrap();
        let read: KeybindPrefs = serde_json::from_str(&json).unwrap();
        let mut restored = Keybinds::default();
        restored.restore(&read);
        assert_eq!(restored, binds);
        // Only the override is written, never the untouched defaults;
        // Ctrl+Shift+F9 is free, so this take displaces no other action.
    }

    #[test]
    fn piano_mode_is_explicit_rebindable_and_never_claims_typing_keys() {
        let mut binds = Keybinds::default();
        assert_eq!(
            BindAction::parse_key("piano-mode"),
            Some(BindAction::PianoMode)
        );
        assert_eq!(binds.hint(BindAction::PianoMode), "F12");
        assert_eq!(
            binds.action_for(&press(KeyCode::F(12), KeyModifiers::NONE)),
            Some(BindAction::PianoMode)
        );
        for letter in "awsedftgyhujkolzxcvm".chars() {
            assert_eq!(
                binds.action_for(&press(KeyCode::Char(letter), KeyModifiers::NONE)),
                None,
                "{letter} must still type outside piano mode"
            );
        }
        let replacement = KeyCombo::parse("ctrl+shift+f9").unwrap();
        binds.learn(BindAction::PianoMode, Some(replacement));
        assert_eq!(
            binds.action_for(&combo_event(replacement)),
            Some(BindAction::PianoMode)
        );
        assert_eq!(
            binds.action_for(&press(KeyCode::F(12), KeyModifiers::NONE)),
            None
        );
        let mut restored = Keybinds::default();
        restored.restore(&binds.prefs());
        assert_eq!(restored.binding(BindAction::PianoMode), Some(replacement));
        restored.reset_all();
        assert_eq!(restored.hint(BindAction::PianoMode), "F12");
    }

    fn chord_modifiers(chord: KeyCombo) -> KeyModifiers {
        let mut modifiers = KeyModifiers::empty();
        if chord.control {
            modifiers |= KeyModifiers::CONTROL;
        }
        if chord.shift {
            modifiers |= KeyModifiers::SHIFT;
        }
        modifiers
    }

    /// A learnt chord IS the action, at every modifier the terminal may
    /// deliver it in, and is nobody else's afterwards.
    #[test]
    fn a_learnt_chord_answers_everywhere() {
        let mut binds = Keybinds::default();
        let chord = KeyCombo::parse("ctrl+shift+f9").unwrap();
        binds.learn(BindAction::Stop, Some(chord));
        assert_eq!(
            binds.action_for(&combo_event(chord)),
            Some(BindAction::Stop)
        );
        // The old chord is free again, not a second stop.
        assert_eq!(
            binds.action_for(&press(KeyCode::Char('g'), KeyModifiers::CONTROL)),
            None,
            "the chord moved off stop, so ^G means nothing now"
        );
    }

    /// One chord, one meaning: learning over a chord the studio already
    /// uses moves it, and the action that held it falls back to nothing -
    /// an unbound action is quieter than two actions on one key.
    #[test]
    fn learning_moves_the_chord_off_its_previous_owner() {
        let mut binds = Keybinds::default();
        binds.learn(BindAction::Zen, Some(KeyCombo::parse("ctrl+o").unwrap()));
        assert_eq!(binds.binding(BindAction::Zen).unwrap().key(), "ctrl+o");
        assert_eq!(
            binds.action_for(&press(KeyCode::Char('o'), KeyModifiers::CONTROL)),
            Some(BindAction::Zen)
        );
        // Settings held ^O; the chord was taken whole: no override, but
        // no default either. The action was displaced, not silently
        // repointed, and ^O means only zen from now on.
        assert!(!binds.overridden(BindAction::Settings));
        assert!(binds.is_unbound(BindAction::Settings));
        assert_eq!(binds.binding(BindAction::Settings), None);
        assert_eq!(
            binds.action_for(&press(KeyCode::Char('o'), KeyModifiers::CONTROL)),
            Some(BindAction::Zen)
        );
        // And the file says so: an empty chord, so the displacement
        // survives a restart instead of the default creeping back.
        let written = binds.prefs();
        assert!(
            written
                .bindings
                .iter()
                .any(|row| row.action == "settings" && row.chord.is_empty()),
            "the displaced action is carried as unbound: {:?}",
            written.bindings
        );
    }

    /// On a terminal without an enhanced keyboard protocol, Ctrl+Shift+Z
    /// arrives as the same byte as Ctrl+Z: `Char('z')` with CONTROL only.
    #[test]
    fn a_legacy_terminal_cannot_tell_ctrl_shift_from_ctrl() {
        use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
        let legacy = KeyEvent {
            code: KeyCode::Char('z'),
            modifiers: KeyModifiers::CONTROL,
            kind: KeyEventKind::Press,
            state: crossterm::event::KeyEventState::NONE,
        };
        let undo = BindAction::Undo.default_binding();
        let redo = BindAction::Redo.default_binding();
        assert_eq!(undo.key(), "ctrl+z");
        assert_eq!(redo.key(), "ctrl+shift+z");
        assert!(undo.matches(&legacy), "the byte reads as plain Ctrl+Z");
        assert!(
            !redo.matches(&legacy),
            "and there is nothing in it that could say Shift"
        );
    }

    /// Every shifted-letter default, and the plain binding a legacy
    /// terminal runs instead. The list is pinned so that a new
    /// shifted-letter default is a deliberate choice.
    #[test]
    fn which_shifted_letters_a_legacy_terminal_swallows() {
        use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};
        let legacy = |letter: char| KeyEvent {
            code: KeyCode::Char(letter),
            modifiers: KeyModifiers::CONTROL,
            kind: KeyEventKind::Press,
            state: KeyEventState::NONE,
        };

        let mut lost: Vec<(&str, &str)> = Vec::new();
        for action in BindAction::ALL {
            let combo = action.default_binding();
            let (true, KeyCode::Char(letter)) = (combo.control && combo.shift, combo.code) else {
                continue;
            };
            let press = legacy(letter);
            assert!(
                !combo.matches(&press),
                "{} cannot be told apart from its plain twin",
                action.key()
            );
            // Whichever plain binding eats the byte instead - on a legacy
            // terminal that is the fallback where the default cannot arrive.
            if let Some(thief) = BindAction::ALL.into_iter().find(|other| {
                other.default_binding().matches(&press)
                    || other
                        .fallback_binding()
                        .is_some_and(|fallback| fallback.matches(&press))
            }) {
                lost.push((action.key(), thief.key()));
            }
        }
        lost.sort_unstable();
        assert_eq!(
            lost,
            [
                ("export", "cut"),
                ("forget-pad", "learn-pad"),
                ("open-set", "settings"),
                ("record-take", "rename-scene"),
                ("redo", "undo"),
                ("rewind-update", "update"),
                ("scene-rewind", "word-wrap"),
            ],
            "the shifted defaults a legacy terminal loses, and what runs instead"
        );
    }

    /// Without enhanced key reports, shifted-letter and ambiguous punctuation
    /// defaults move to their portable spellings. This isolates protocol
    /// behavior from the independently tested terminal conflict profiles.
    ///
    /// The cost of getting this wrong in the other direction is a player
    /// on kitty losing a chord they already learnt for no reason, so the
    /// substitution is deliberately narrow.
    #[test]
    fn a_legacy_terminal_gets_the_second_choice_and_a_modern_one_keeps_the_first() {
        let legacy = Reach {
            enhanced: false,
            terminal: "xterm-256color".to_owned(),
        };
        let modern = Reach {
            enhanced: true,
            terminal: "terminal-with-no-local-bindings".to_owned(),
        };

        let mut binds = Keybinds::default();
        binds.set_reach(modern);
        let first: Vec<String> = BindAction::ALL
            .into_iter()
            .map(|action| binds.hint(action))
            .collect();

        binds.set_reach(legacy);
        let mut moved: Vec<(&str, String)> = Vec::new();
        for (action, before) in BindAction::ALL.into_iter().zip(&first) {
            let now = binds.hint(action);
            if &now != before {
                moved.push((action.key(), now));
            }
        }
        moved.sort_unstable();
        assert_eq!(
            moved,
            {
                let mut expected = [
                    ("export", "\u{21e7}F12".to_owned()),
                    ("first-error", "\u{21e7}F4".to_owned()),
                    ("forget-pad", "\u{21e7}F8".to_owned()),
                    ("next-scene", "F7".to_owned()),
                    ("open-set", "\u{21e7}F6".to_owned()),
                    ("previous-scene", "F6".to_owned()),
                    ("record-take", "\u{21e7}F11".to_owned()),
                    ("redo", "^Y".to_owned()),
                    ("rewind-update", "\u{21e7}F5".to_owned()),
                    ("scene-rewind", "\u{21e7}F7".to_owned()),
                    ("comment", "^7".to_owned()),
                    // Legacy input folds ^Enter into Enter and ^. into `.`.
                    ("stop", "^G".to_owned()),
                    ("update", "^S".to_owned()),
                ];
                expected.sort_unstable();
                expected
            },
            "only the chords a legacy terminal cannot send move, and only there"
        );
    }

    /// Every second choice is one the terminal it is meant for can send -
    /// a fallback that is itself unreachable is worse than none, because
    /// it moves a chord somebody knew for nothing.
    #[test]
    fn every_second_choice_survives_the_terminal_it_is_for() {
        let _platform = super::super::terminal::conflicts::ForcePlatformForTest::set("windows");
        let legacy = Reach {
            enhanced: false,
            terminal: "xterm-256color".to_owned(),
        };
        let mut candidates = 0;
        for action in BindAction::ALL {
            let Some(second) = action.fallback_binding() else {
                continue;
            };
            assert!(
                legacy.delivers(&second),
                "{}: its second choice cannot arrive either",
                action.key()
            );
            // Candidates may overlap when different profiles need them.
            // The all-profile resolution test verifies actual assignments
            // remain distinct after availability checks select a fallback.
            candidates += 1;
        }
        assert!(candidates > 0, "no second choices at all");
    }

    /// A chord somebody LEARNT is never second-guessed: they pressed it,
    /// so it plainly arrives, whatever the table believes about their
    /// terminal.
    #[test]
    fn a_learnt_chord_outranks_what_the_terminal_is_thought_to_swallow() {
        let mut binds = Keybinds::default();
        binds.set_reach(Reach {
            enhanced: false,
            terminal: "xterm-256color".to_owned(),
        });
        assert_eq!(binds.binding(BindAction::Redo).unwrap().key(), "ctrl+y");
        binds.learn(
            BindAction::Redo,
            Some(KeyCombo::parse("ctrl+shift+z").expect("a chord")),
        );
        assert_eq!(
            binds.binding(BindAction::Redo).unwrap().key(),
            "ctrl+shift+z",
            "the studio does not argue with a key somebody just pressed"
        );
    }

    /// Unlearning puts the studio's own chord back.
    #[test]
    fn unlearning_restores_the_default() {
        let mut binds = Keybinds::default();
        // ^⇧Q because it is the free one: every bare F-key is a default,
        // and ⇧F1/⇧F2/⇧F9/⇧F10 belong to the panels.
        binds.learn(
            BindAction::Undo,
            Some(KeyCombo::parse("ctrl+shift+q").unwrap()),
        );
        assert_eq!(
            binds.binding(BindAction::Undo).unwrap().key(),
            "ctrl+shift+q"
        );
        binds.learn(BindAction::Undo, None);
        assert_eq!(binds.binding(BindAction::Undo).unwrap().key(), "ctrl+z");
        assert!(binds.prefs().bindings.is_empty());
    }

    /// ⇧ with control: the enhanced protocols deliver ⇧⌘Z as `Z` with the
    /// shift bit dropped, so the uppercase letter IS the shift. The same
    /// match the editor's chord reader makes.
    #[test]
    fn an_uppercase_letter_is_its_own_shift() {
        let redo = BindAction::Redo.default_binding();
        assert!(redo.shift);
        assert!(redo.matches(&press(KeyCode::Char('Z'), KeyModifiers::CONTROL)));
        assert!(redo.matches(&press(
            KeyCode::Char('z'),
            KeyModifiers::CONTROL | KeyModifiers::SHIFT
        )));
        assert!(!redo.matches(&press(KeyCode::Char('z'), KeyModifiers::CONTROL)));
        let undo = BindAction::Undo.default_binding();
        assert!(!undo.matches(&press(KeyCode::Char('Z'), KeyModifiers::CONTROL)));
    }

    /// A file from a newer rustel keeps its unknown rows on the way past,
    /// and an unreadable chord costs only its own row.
    #[test]
    fn unknown_rows_are_kept_and_skipped() {
        let prefs: KeybindPrefs = serde_json::from_str(
            r#"{"bindings":[
                {"action":"undo","chord":"f3"},
                {"action":"stop","chord":"nope"},
                {"action":"brand-new-thing","chord":"f2"}
            ]}"#,
        )
        .unwrap();
        let mut binds = Keybinds::default();
        binds.restore(&prefs);
        assert_eq!(binds.binding(BindAction::Undo).unwrap().key(), "f3");
        assert_eq!(
            binds.binding(BindAction::Stop).unwrap().key(),
            "ctrl+.",
            "an unreadable chord is no binding, not a broken one"
        );
        // The unknown action survives the round trip untouched.
        let written = binds.prefs();
        assert!(
            written
                .bindings
                .iter()
                .any(|row| row.action == "brand-new-thing" && row.chord == "f2")
        );
    }

    /// An explicit unbinding is carried too: the action answers to
    /// nothing, and a restart does not quietly hand the chord back.
    #[test]
    fn an_unbound_action_answers_to_nothing_and_stays_so() {
        let mut binds = Keybinds::default();
        binds.unbind(BindAction::Evaluate);
        assert_eq!(binds.binding(BindAction::Evaluate), None);
        assert_eq!(binds.hint(BindAction::Evaluate), "");
        assert_eq!(
            binds.action_for(&press(KeyCode::Char('s'), KeyModifiers::CONTROL)),
            None,
            "^S is nobody's while evaluate is unbound"
        );
        assert!(binds.overrules(&press(KeyCode::Char('s'), KeyModifiers::CONTROL)));
        // Through the file and back, the silence holds.
        let mut restored = Keybinds::default();
        restored.restore(&binds.prefs());
        assert_eq!(restored.binding(BindAction::Evaluate), None);
        // Learning back onto the action is the way out.
        binds.learn(BindAction::Evaluate, Some(KeyCombo::parse("f5").unwrap()));
        assert_eq!(binds.binding(BindAction::Evaluate).unwrap().key(), "f5");
        assert!(!binds.is_unbound(BindAction::Evaluate));
    }

    /// The take the Keybinds page asks about, end to end: confirming
    /// moves the chord and leaves the loser unbound; keeping changes
    /// nothing; and Enter on the loser's row is how it comes back.
    #[test]
    fn confirming_a_take_moves_the_chord_and_unbinds_the_loser() {
        let mut binds = Keybinds::default();
        let combo = KeyCombo::parse("ctrl+o").unwrap();
        // Enter on quit's row while settings holds ^O: the learn asks.
        let learn = KeybindLearn::arm(BindAction::Quit);
        assert!(
            matches!(
                learn.plan(&binds, combo),
                KeybindCapture::Ask {
                    held_by: BindAction::Settings,
                    ..
                }
            ),
            "settings would lose ^O and the learn says so"
        );
        // Keeping: nothing moved, the learn is over.
        binds.learn(BindAction::Quit, None);
        assert_eq!(binds.binding(BindAction::Quit).unwrap().key(), "ctrl+q");
        assert_eq!(binds.binding(BindAction::Settings).unwrap().key(), "ctrl+o");
        // Confirming with the same chord on the same table: the chord
        // moves, and the action displaced goes unbound.
        binds.learn(BindAction::Quit, Some(combo));
        assert_eq!(binds.binding(BindAction::Quit).unwrap(), combo);
        assert!(binds.is_unbound(BindAction::Settings));
        assert_eq!(
            binds.holder_of(combo),
            Some(BindAction::Quit),
            "one chord, one meaning: the holder is quit alone"
        );
        // Enter on settings' row - the action the sheet holds a cursor
        // on - learns settings back onto its own chord, which frees it.
        binds.learn(BindAction::Settings, Some(combo));
        assert!(!binds.is_unbound(BindAction::Settings));
        assert!(binds.is_unbound(BindAction::Quit));
    }

    /// Rebinding an action to the chord its row already wears - its
    /// default included - writes nothing: the row must not wear an
    /// "overridden" that spells the studio's own key.
    #[test]
    fn rebinding_to_the_chord_the_row_already_wears_writes_nothing() {
        let mut binds = Keybinds::default();
        // Its own default, pressed again.
        binds.learn(
            BindAction::Undo,
            Some(KeyCombo::parse("ctrl+z").expect("^Z parses")),
        );
        assert!(!binds.overridden(BindAction::Undo));
        // Its own default after a detour: the detour is cleared, and the
        // reset still frees the chord from whoever took it meanwhile.
        binds.learn(BindAction::Undo, Some(KeyCombo::parse("f2").unwrap()));
        binds.learn(BindAction::Stop, Some(KeyCombo::parse("f2").unwrap()));
        binds.learn(
            BindAction::Undo,
            Some(KeyCombo::parse("ctrl+z").expect("^Z parses")),
        );
        assert!(!binds.overridden(BindAction::Undo), "the detour is gone");
        // Nobody was displaced by the reset: stop took F2 from undo (the
        // detour's taker), and an unbound undo held nothing to take ^Z
        // back from - the reset just lifted undo out of the unbound set.
        assert!(!binds.is_unbound(BindAction::Evaluate));
        assert_eq!(binds.binding(BindAction::Stop).unwrap().key(), "f2");
        assert_eq!(binds.binding(BindAction::Undo).unwrap().key(), "ctrl+z");
        // The same override pressed twice changes nothing either.
        binds.learn(BindAction::Undo, Some(KeyCombo::parse("f3").unwrap()));
        let once = binds.prefs();
        binds.learn(BindAction::Undo, Some(KeyCombo::parse("f3").unwrap()));
        assert_eq!(binds.prefs(), once);
        // An explicit stored choice remains explicit, even when it happens
        // to equal the default on the terminal loading it.
        let mut healed = Keybinds::default();
        healed.restore(&KeybindPrefs {
            bindings: vec![KeybindPref {
                action: "undo".to_owned(),
                chord: "ctrl+z".to_owned(),
            }],
        });
        assert!(healed.overridden(BindAction::Undo));
        assert_eq!(healed.binding(BindAction::Undo).unwrap().key(), "ctrl+z");
    }

    /// An F-key alias (evaluate's F5, stop's F8) is a second chord for the
    /// same action: shown on the row, answered while the default stands,
    /// asked about on a rebind, and retired when the action's chord moves.
    #[test]
    fn an_alias_is_shown_answered_and_shadowed_never_stolen() {
        let mut binds = Keybinds::default();
        // F5 is evaluate's: the row would show it, and the chord fires.
        assert_eq!(BindAction::Evaluate.default_alias().unwrap().key(), "f5");
        assert!(
            binds
                .action_for(&press(KeyCode::F(5), KeyModifiers::NONE))
                .is_some(),
            "F5 evaluates while the default stands"
        );
        // Learning F5 onto stop asks first, because F5 is evaluate's
        // alias. Confirming shadows the alias: stop answers on F5, and
        // evaluate keeps ^Enter.
        let learn = KeybindLearn::arm(BindAction::Stop);
        assert!(matches!(
            learn.plan(&binds, KeyCombo::parse("f5").unwrap()),
            KeybindCapture::Ask {
                held_by: BindAction::Evaluate,
                ..
            }
        ));
        binds.learn(BindAction::Stop, Some(KeyCombo::parse("f5").unwrap()));
        assert!(binds.overridden(BindAction::Stop));
        assert_eq!(binds.binding(BindAction::Stop).unwrap().key(), "f5");
        assert_eq!(
            binds.binding(BindAction::Evaluate).unwrap().key(),
            "ctrl+enter",
            "the primary was never stolen"
        );
        assert!(!binds.is_unbound(BindAction::Evaluate));
        assert!(
            binds.action_for(&press(KeyCode::F(5), KeyModifiers::NONE)) == Some(BindAction::Stop),
            "the override shadows the alias"
        );
        // Rebinding evaluate's own chord retires its alias for real: ^S
        // goes to F2 and F5 belongs to stop alone. Through the file and
        // back, the arrangement holds.
        binds.learn(BindAction::Evaluate, Some(KeyCombo::parse("f2").unwrap()));
        assert!(binds.overridden(BindAction::Evaluate));
        assert!(
            binds.action_for(&press(KeyCode::F(5), KeyModifiers::NONE)) == Some(BindAction::Stop),
            "F5 is stop's own now, not a retired ghost"
        );
        assert!(
            binds
                .action_for(&press(KeyCode::Char('s'), KeyModifiers::CONTROL))
                .is_none(),
            "^S moved off evaluate means nothing"
        );
        let mut restored = Keybinds::default();
        restored.restore(&binds.prefs());
        assert_eq!(restored.binding(BindAction::Evaluate).unwrap().key(), "f2");
        assert_eq!(restored.binding(BindAction::Stop).unwrap().key(), "f5");
    }

    /// Enter and the other reserved keys are never chords: Esc, Tab,
    /// Backspace, Delete, Space, the arrows, and a modifier pressed alone.
    #[test]
    fn enter_and_the_reserved_family_are_never_chords() {
        let mut binds = Keybinds::default();
        let reserved = [
            KeyCode::Enter,
            KeyCode::Esc,
            KeyCode::Tab,
            KeyCode::BackTab,
            KeyCode::Backspace,
            KeyCode::Delete,
            KeyCode::Null,
            KeyCode::Char(' '),
            KeyCode::Left,
            KeyCode::Right,
            KeyCode::Up,
            KeyCode::Down,
            KeyCode::Modifier(ModifierKeyCode::LeftControl),
            KeyCode::Modifier(ModifierKeyCode::RightShift),
            KeyCode::Modifier(ModifierKeyCode::LeftSuper),
        ];
        for code in reserved {
            let plain = KeyCombo {
                code,
                control: false,
                shift: false,
            };
            assert!(!KeybindLearn::is_a_chord(plain), "{code:?} is reserved");
            // Modifier-held spellings are refused with the plain ones.
            assert!(!KeybindLearn::is_a_chord(KeyCombo {
                code,
                control: true,
                shift: true,
            }));
        }
        // The learn cannot be smuggled past the model either: every
        // reserved key pressed while evaluate is being learnt leaves
        // ^Enter exactly where it was.
        for code in reserved {
            binds.learn(
                BindAction::Evaluate,
                Some(KeyCombo {
                    code,
                    control: false,
                    shift: false,
                }),
            );
            assert_eq!(
                binds.binding(BindAction::Evaluate).unwrap().key(),
                "ctrl+enter",
                "{code:?} is refused and changes nothing"
            );
        }
        assert!(binds.prefs().bindings.is_empty());
        // A refused chord is still SAID, and its saying spells the key
        // the way the footer would, not the way a debug print would.
        assert_eq!(
            KeyCombo {
                code: KeyCode::Modifier(ModifierKeyCode::LeftControl),
                control: false,
                shift: false,
            }
            .hint(),
            "Left Ctrl"
        );
        assert_eq!(
            KeyCombo {
                code: KeyCode::Char(' '),
                control: true,
                shift: false,
            }
            .hint(),
            "^Space"
        );
        // A character with no ctrl held is a typing key, not a shortcut.
        // Ctrl-held characters and F-keys stay bindable.
        assert!(KeybindLearn::is_a_typing_key(KeyCombo {
            code: KeyCode::Char('\''),
            control: false,
            shift: false,
        }));
        assert!(!KeybindLearn::is_a_typing_key(KeyCombo {
            code: KeyCode::Char('\''),
            control: true,
            shift: false,
        }));
        assert!(!KeybindLearn::is_a_typing_key(KeyCombo {
            code: KeyCode::F(2),
            control: false,
            shift: false,
        }));
        // And the plan refuses it with the chord in hand, so the word
        // can name the key: ctrl+' arrives as a bare ' and is refused
        // as the typing key it really was.
        assert!(matches!(
            KeybindLearn::arm(BindAction::Undo).plan(
                &binds,
                KeyCombo {
                    code: KeyCode::Char('\''),
                    control: false,
                    shift: false,
                }
            ),
            KeybindCapture::Refused { .. }
        ));
    }

    /// Alt is never stored: the chord a learner presses with option held
    /// arrives without it, so no binding can take a key the terminal
    /// spends on typing characters.
    #[test]
    fn alt_never_reaches_a_binding() {
        let binds = Keybinds::default();
        // With alt held the ^S press still reads as ^S, the default.
        assert_eq!(
            binds.action_for(&press(
                KeyCode::Char('s'),
                KeyModifiers::CONTROL | KeyModifiers::ALT
            )),
            Some(BindAction::Evaluate)
        );
    }
    fn combo_event(combo: KeyCombo) -> KeyEvent {
        let mut modifiers = KeyModifiers::NONE;
        if combo.control {
            modifiers |= KeyModifiers::CONTROL;
        }
        if combo.shift {
            modifiers |= KeyModifiers::SHIFT;
        }
        press(combo.code, modifiers)
    }

    /// The memory breakdown ships with no chord: F12 stays the piano's, and
    /// an untouched table writes no row for it. A learnt chord is saved and
    /// restored like any other.
    #[test]
    fn memory_ships_unbound_and_an_untouched_table_writes_no_row_for_it() {
        let binds = Keybinds::default();
        assert!(binds.is_unbound(BindAction::Memory));
        assert_eq!(binds.binding(BindAction::Memory), None);
        assert_eq!(binds.hint(BindAction::Memory), "");
        assert_eq!(
            binds.action_for(&press(KeyCode::F(12), KeyModifiers::NONE)),
            Some(BindAction::PianoMode)
        );
        assert!(binds.prefs().is_empty());
        let mut restored = Keybinds::default();
        restored.restore(&binds.prefs());
        assert!(restored.is_unbound(BindAction::Memory));

        let chord = KeyCombo::parse("ctrl+shift+f8").expect("the chord parses");
        let mut learnt = Keybinds::default();
        learnt.learn(BindAction::Memory, Some(chord));
        assert_eq!(learnt.binding(BindAction::Memory), Some(chord));
        assert_eq!(
            learnt.action_for(&combo_event(chord)),
            Some(BindAction::Memory)
        );
        let prefs = learnt.prefs();
        assert_eq!(
            prefs
                .bindings
                .iter()
                .map(|row| (row.action.as_str(), row.chord.as_str()))
                .collect::<Vec<_>>(),
            [("memory", chord.key().as_str())]
        );
        let mut reread = Keybinds::default();
        reread.restore(&prefs);
        assert_eq!(reread.binding(BindAction::Memory), Some(chord));
    }

    #[test]
    fn a_panel_action_ships_on_its_alt_letter_and_takes_a_learnt_chord() {
        let mut binds = Keybinds::default();
        assert_eq!(binds.binding(BindAction::TrimSample), None);
        assert_eq!(binds.hint(BindAction::TrimSample), shortcut_label("Alt+T"));
        assert!(binds.prefs().is_empty());

        let chord = KeyCombo::parse("ctrl+shift+f8").expect("the chord parses");
        binds.learn(BindAction::TrimSample, Some(chord));
        assert_eq!(binds.hint(BindAction::TrimSample), chord.hint());
        assert_eq!(
            binds.override_action_for(&combo_event(chord)),
            Some(BindAction::TrimSample)
        );
        // The tape timeline keeps the letter the two actions share.
        assert_eq!(
            binds.hint(BindAction::FocusTimeline),
            shortcut_label("Alt+T")
        );
        let mut reread = Keybinds::default();
        reread.restore(&binds.prefs());
        assert_eq!(reread.binding(BindAction::TrimSample), Some(chord));

        binds.learn(BindAction::TrimSample, None);
        assert_eq!(binds.hint(BindAction::TrimSample), shortcut_label("Alt+T"));
        assert!(binds.prefs().is_empty());
    }

    /// Delete on the row of an action shipped with no key puts back its
    /// default, which is no key: never a spare chord for the session that
    /// a restart then takes away. Learnt or not, the table writes nothing.
    #[test]
    fn clearing_an_action_shipped_unbound_leaves_it_with_no_key() {
        for action in SHIPS_UNBOUND.iter().copied() {
            let mut untouched = Keybinds::default();
            untouched.learn(action, None);
            assert!(untouched.is_unbound(action), "{action:?}");
            assert_eq!(untouched.binding(action), None, "{action:?}");
            assert!(untouched.prefs().is_empty(), "{action:?}");

            let mut learnt = Keybinds::default();
            learnt.learn(action, KeyCombo::parse("ctrl+shift+f8"));
            learnt.learn(action, None);
            assert!(learnt.is_unbound(action), "{action:?}");
            assert_eq!(learnt.binding(action), None, "{action:?}");
            assert!(learnt.prefs().is_empty(), "{action:?}");
        }
    }

    /// Its placeholder is F12, which is the piano's; taking F12 for the
    /// popup is a chord the player chose, saved and restored like any
    /// other, and the piano it was taken from stays unbound.
    #[test]
    fn f12_learnt_for_memory_is_saved() {
        let f12 = KeyCombo::parse("f12").expect("the chord parses");
        let mut binds = Keybinds::default();
        binds.learn(BindAction::Memory, Some(f12));
        assert_eq!(binds.binding(BindAction::Memory), Some(f12));
        assert!(binds.is_unbound(BindAction::PianoMode));

        let mut restored = Keybinds::default();
        restored.restore(&binds.prefs());
        assert_eq!(restored.binding(BindAction::Memory), Some(f12));
        assert!(restored.is_unbound(BindAction::PianoMode));
    }

    #[test]
    fn reset_all_uses_the_current_profile_and_clears_every_override() {
        for terminal in ["unknown", "Windows Terminal", "Konsole"] {
            let reach = Reach {
                enhanced: false,
                terminal: terminal.into(),
            };
            let mut expected = Keybinds::default();
            expected.set_reach(reach.clone());
            let mut binds = expected.clone();
            binds.learn(BindAction::Undo, KeyCombo::parse("ctrl+f9"));
            binds.unbind(BindAction::Stop);
            binds.unknown.push(KeybindPref {
                action: "future-action".into(),
                chord: "f20".into(),
            });
            binds.reset_all();
            assert_eq!(binds, expected, "{terminal}");
            assert!(binds.prefs().is_empty());
            assert!(SHIPS_UNBOUND.iter().all(|action| binds.is_unbound(*action)));
        }
    }

    #[test]
    fn effective_primaries_and_aliases_round_trip_for_every_terminal_profile() {
        let names: Vec<_> = super::super::terminal::conflicts::known()
            .chain(["xterm-256color", "kitty", "conhost", "unknown-terminal"])
            .collect();
        for (platform, desktop) in [
            ("linux", ""),
            ("linux", "plasma"),
            ("macos", ""),
            ("windows", ""),
        ] {
            let _platform = super::super::terminal::conflicts::ForcePlatformForTest::set(platform);
            let _desktop = super::super::terminal::conflicts::ForceDesktopForTest::set(desktop);
            for &terminal in &names {
                for enhanced in [false, true] {
                    let reach = Reach {
                        enhanced,
                        terminal: terminal.into(),
                    };
                    let mut binds = Keybinds::default();
                    binds.set_reach(reach.clone());
                    for action in BindAction::ALL {
                        if SHIPS_UNBOUND.contains(&action) {
                            assert_eq!(binds.binding(action), None);
                            continue;
                        }
                        let combo = binds.binding(action).unwrap_or_else(|| panic!(
                            "{platform}/{desktop}/{terminal}/{enhanced}: {action:?} has no reachable primary"));
                        assert!(
                            reach.delivers(&combo),
                            "{platform}/{terminal}: {action:?} has unreachable {}",
                            combo.key()
                        );
                        let owner = binds.action_for(&combo_event(combo));
                        let expected = if action == BindAction::Help
                            && binds.binding(BindAction::MenuBar) == Some(combo)
                        {
                            BindAction::MenuBar
                        } else {
                            action
                        };
                        assert_eq!(
                            owner,
                            Some(expected),
                            "{platform}/{terminal}/{enhanced}: {action:?} {}",
                            combo.key()
                        );
                        assert_eq!(binds.default_holder_of(combo), Some(expected));
                        if combo != action.default_binding() {
                            assert_eq!(binds.adapted_action_for(&combo_event(combo)), Some(action));
                            assert!(binds.overrules(&combo_event(combo)));
                        }
                        if let Some(alias) = binds.effective_alias(action) {
                            assert!(reach.delivers(&alias));
                            assert_ne!(binds.binding(action), Some(alias));
                            assert_eq!(binds.action_for(&combo_event(alias)), Some(action));
                            assert_eq!(binds.holder_of(alias), Some(action));
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn legacy_recording_and_ambiguous_punctuation_have_deliverable_shortcuts() {
        let mut binds = Keybinds::default();
        binds.set_reach(Reach {
            enhanced: false,
            terminal: "xterm-256color".into(),
        });
        for (action, spelling) in [
            (BindAction::OpenSet, "shift+f6"),
            (BindAction::ForgetPad, "shift+f8"),
            (BindAction::Export, "shift+f12"),
            (BindAction::RecordTake, "shift+f11"),
            (BindAction::PreviousScene, "f6"),
            (BindAction::NextScene, "f7"),
            (BindAction::ToggleComment, "ctrl+7"),
        ] {
            let chord = KeyCombo::parse(spelling).unwrap();
            assert_eq!(binds.binding(action), Some(chord));
            assert_eq!(binds.adapted_action_for(&combo_event(chord)), Some(action));
            assert!(binds.overrules(&combo_event(action.default_binding())));
        }
    }

    #[test]
    fn modified_f3_is_only_advertised_with_unambiguous_input() {
        let _platform = super::super::terminal::profiles::ForcePlatformForTest::set("macos");
        for terminal in [
            "iTerm2 3.6.11",
            "tmux via iTerm2 3.6.11",
            "unknown-terminal",
        ] {
            let mut binds = Keybinds::default();
            binds.set_reach(Reach {
                terminal: terminal.into(),
                enhanced: false,
            });
            for action in BindAction::ALL {
                for combo in [binds.binding(action), binds.effective_alias(action)]
                    .into_iter()
                    .flatten()
                {
                    assert!(
                        !(combo.code == KeyCode::F(3) && (combo.control || combo.shift)),
                        "{terminal}: {action:?} advertises {}",
                        combo.key()
                    );
                }
            }
            assert_eq!(
                binds.binding(BindAction::FirstError).unwrap().key(),
                "shift+f4"
            );
            binds.set_reach(Reach {
                terminal: terminal.into(),
                enhanced: true,
            });
            assert_eq!(
                binds.binding(BindAction::FirstError).unwrap().key(),
                "shift+f3"
            );
            for key in ["shift+f1", "shift+f2", "shift+f3"] {
                assert!(binds.delivers(&KeyCombo::parse(key).unwrap()));
            }
        }
    }

    #[test]
    fn apple_terminal_defaults_match_its_legacy_keymap_and_decoder() {
        let _platform = super::super::terminal::profiles::ForcePlatformForTest::set("macos");
        for terminal in ["Apple Terminal", "tmux via Apple Terminal"] {
            let mut binds = Keybinds::default();
            binds.set_reach(Reach {
                terminal: terminal.into(),
                enhanced: false,
            });
            assert_eq!(
                binds.binding(BindAction::FirstError).unwrap().key(),
                "ctrl+\\"
            );
            for spelling in [
                "shift+f1",
                "shift+f2",
                "shift+f3",
                "shift+f4",
                "ctrl+f9",
                "ctrl+shift+f12",
                "ctrl+4",
            ] {
                assert!(
                    !binds.delivers(&KeyCombo::parse(spelling).unwrap()),
                    "{spelling}"
                );
            }
            for key in 5..=12 {
                let physical = KeyCombo::parse(&format!("shift+f{key}")).unwrap();
                let received = KeyCombo::parse(&format!("f{}", key + 8)).unwrap();
                assert!(binds.delivers(&physical), "{}", physical.key());
                // The received code belongs to the physical Shift+Fn chord.
                assert!(!binds.delivers(&received), "{}", received.key());
                for kind in [
                    crossterm::event::KeyEventKind::Press,
                    crossterm::event::KeyEventKind::Repeat,
                    crossterm::event::KeyEventKind::Release,
                ] {
                    let mut event = combo_event(received);
                    event.kind = kind;
                    event.state = crossterm::event::KeyEventState::CAPS_LOCK;
                    binds.normalize_terminal_key(&mut event);
                    let mut expected = combo_event(physical);
                    expected.kind = kind;
                    expected.state = crossterm::event::KeyEventState::CAPS_LOCK;
                    assert_eq!(event, expected);
                }
            }
            let mut event = combo_event(KeyCombo::parse("ctrl+4").unwrap());
            binds.normalize_terminal_key(&mut event);
            assert_eq!(event, combo_event(KeyCombo::parse("ctrl+\\").unwrap()));
            binds.set_reach(Reach {
                terminal: terminal.into(),
                enhanced: true,
            });
            assert_eq!(
                binds.binding(BindAction::FirstError).unwrap().key(),
                "shift+f3"
            );
            for received in
                std::iter::once("ctrl+4".to_owned()).chain((13..=20).map(|key| format!("f{key}")))
            {
                let mut event = combo_event(KeyCombo::parse(&received).unwrap());
                let original = event;
                binds.normalize_terminal_key(&mut event);
                assert_eq!(event, original);
            }
        }
    }

    #[test]
    fn taking_an_adaptive_primary_unbinds_its_owner_and_survives_a_restart() {
        let legacy = Reach {
            enhanced: false,
            terminal: "xterm-256color".into(),
        };
        let mut binds = Keybinds::default();
        binds.set_reach(legacy.clone());
        let chord = KeyCombo::parse("shift+f6").unwrap();
        assert_eq!(binds.holder_of(chord), Some(BindAction::OpenSet));
        binds.learn(BindAction::Undo, Some(chord));
        assert!(binds.is_unbound(BindAction::OpenSet));
        assert_eq!(
            binds.action_for(&combo_event(chord)),
            Some(BindAction::Undo)
        );
        let mut restored = Keybinds::default();
        restored.set_reach(legacy);
        restored.restore(&binds.prefs());
        assert_eq!(restored.prefs(), binds.prefs());
        assert!(restored.is_unbound(BindAction::OpenSet));
        assert_eq!(restored.binding(BindAction::Undo), Some(chord));
    }

    #[test]
    fn redo_rebinding_or_unbinding_retires_ctrl_y_on_every_terminal() {
        for enhanced in [false, true] {
            let mut binds = Keybinds::default();
            binds.set_reach(Reach {
                enhanced,
                terminal: "xterm-256color".into(),
            });
            let ctrl_y = combo_event(KeyCombo::parse("ctrl+y").unwrap());
            assert_eq!(binds.action_for(&ctrl_y), Some(BindAction::Redo));
            binds.learn(BindAction::Redo, Some(KeyCombo::parse("ctrl+f9").unwrap()));
            assert_eq!(binds.action_for(&ctrl_y), None);
            assert!(binds.overrules(&ctrl_y));
            binds.learn(BindAction::Redo, None);
            assert_eq!(binds.action_for(&ctrl_y), Some(BindAction::Redo));
            binds.unbind(BindAction::Redo);
            assert_eq!(binds.action_for(&ctrl_y), None);
            assert!(binds.overrules(&ctrl_y));
        }
    }

    #[test]
    fn explicit_preferences_do_not_disappear_when_the_terminal_default_matches_them() {
        for spelling in ["ctrl+y", "ctrl+shift+z"] {
            let prefs = KeybindPrefs {
                bindings: vec![KeybindPref {
                    action: "redo".into(),
                    chord: spelling.into(),
                }],
            };
            for enhanced in [false, true] {
                let mut binds = Keybinds::default();
                binds.set_reach(Reach {
                    enhanced,
                    terminal: "xterm-256color".into(),
                });
                binds.restore(&prefs);
                assert_eq!(binds.prefs(), prefs);
                assert_eq!(binds.binding(BindAction::Redo).unwrap().key(), spelling);
                binds.set_reach(Reach {
                    enhanced: !enhanced,
                    terminal: "Windows Terminal".into(),
                });
                assert_eq!(binds.prefs(), prefs);
                assert_eq!(binds.binding(BindAction::Redo).unwrap().key(), spelling);
            }
        }
        // A learnt preferred spelling on a legacy terminal must also survive.
        let mut binds = Keybinds::default();
        binds.set_reach(Reach {
            enhanced: false,
            terminal: "xterm-256color".into(),
        });
        binds.learn(
            BindAction::Redo,
            Some(KeyCombo::parse("ctrl+shift+z").unwrap()),
        );
        let saved = binds.prefs();
        let mut restored = Keybinds::default();
        restored.restore(&saved);
        assert_eq!(restored.prefs(), saved);
    }

    #[test]
    fn volume_fallbacks_avoid_saved_shortcuts_and_never_reuse_a_blocked_default() {
        let _platform = super::super::terminal::conflicts::ForcePlatformForTest::set("windows");
        let mut binds = Keybinds::default();
        binds.set_reach(Reach {
            enhanced: true,
            terminal: "Windows Terminal".into(),
        });
        assert_eq!(
            binds.binding(BindAction::MasterUp).unwrap().key(),
            "ctrl+f12"
        );
        assert_eq!(
            binds.binding(BindAction::MasterDown).unwrap().key(),
            "ctrl+f11"
        );

        // Saved choices are reserved before terminal adaptation, even when
        // moving this file from a terminal that did not need the fallback.
        // Occupying the lower key moves that chord aside; the raise key stays.
        let prefs = KeybindPrefs {
            bindings: vec![KeybindPref {
                action: "undo".into(),
                chord: "ctrl+f11".into(),
            }],
        };
        binds.restore(&prefs);
        assert_eq!(binds.binding(BindAction::Undo).unwrap().key(), "ctrl+f11");
        assert_eq!(
            binds.binding(BindAction::MasterUp).unwrap().key(),
            "ctrl+f12"
        );
        assert_eq!(
            binds.binding(BindAction::MasterDown).unwrap().key(),
            "ctrl+f1"
        );
        assert_eq!(binds.prefs(), prefs);

        // Occupy every modified function key. Plain function keys remain
        // valid fallbacks, but a blocked volume default must never return.
        let prefs = KeybindPrefs {
            bindings: BindAction::ALL
                .into_iter()
                .zip(Keybinds::portable_candidates().take(36))
                .map(|(action, chord)| KeybindPref {
                    action: action.key().into(),
                    chord: chord.key(),
                })
                .collect(),
        };
        binds.restore(&prefs);
        for action in [BindAction::MasterUp, BindAction::MasterDown] {
            let fallback = binds.binding(action).expect("a spare plain function key");
            assert!(matches!(fallback.code, KeyCode::F(_)));
            assert!(!fallback.control && !fallback.shift);
            assert!(binds.reach.delivers(&fallback));
            assert_eq!(binds.holder_of(fallback), Some(action));
        }
        assert_ne!(
            binds.binding(BindAction::MasterUp),
            binds.binding(BindAction::MasterDown)
        );
        assert!(binds.overrules(&combo_event(BindAction::MasterUp.default_binding())));
        assert!(binds.overrules(&combo_event(BindAction::MasterDown.default_binding())));
    }

    /// Plasma takes Ctrl+F12, so the raise key moves to a spare key. The
    /// lower key stays left of it.
    #[test]
    fn master_keys_keep_their_order_when_one_moves_to_a_spare_key() {
        let _platform = super::super::terminal::conflicts::ForcePlatformForTest::set("linux");
        let _desktop = super::super::terminal::conflicts::ForceDesktopForTest::set("plasma");
        let mut binds = Keybinds::default();
        binds.set_reach(Reach {
            enhanced: true,
            terminal: "kitty".into(),
        });
        let raise = binds.binding(BindAction::MasterUp).expect("raise key");
        let lower = binds.binding(BindAction::MasterDown).expect("lower key");
        assert_ne!(raise.key(), "ctrl+f12", "Plasma takes this key");
        let (KeyCode::F(raise_key), KeyCode::F(lower_key)) = (raise.code, lower.code) else {
            panic!("function keys: {} and {}", raise.key(), lower.key());
        };
        assert_eq!((raise.control, raise.shift), (lower.control, lower.shift));
        assert!(
            lower_key < raise_key,
            "lower {} is left of raise {}",
            lower.key(),
            raise.key()
        );
    }

    /// kitty takes Ctrl+Shift+S to paste the selection. With the kitty
    /// profile, rewind update moves to a key that kitty delivers.
    #[test]
    fn rewind_update_resolves_to_a_key_that_kitty_delivers() {
        use super::super::terminal::conflicts;
        let _platform = conflicts::ForcePlatformForTest::set("linux");
        let _desktop = conflicts::ForceDesktopForTest::set("");
        let taken = BindAction::RewindEvaluate.default_binding();
        assert!(conflicts::steals("kitty", &taken).is_some());
        for terminal in ["kitty", "kitty 0.35.2", "tmux 3.5 via kitty"] {
            for enhanced in [true, false] {
                let reach = Reach {
                    enhanced,
                    terminal: terminal.into(),
                };
                let mut binds = Keybinds::default();
                binds.set_reach(reach.clone());
                let chord = binds.binding(BindAction::RewindEvaluate).expect("a key");
                assert_eq!(chord.key(), "shift+f5", "{terminal}/{enhanced}");
                assert!(reach.delivers(&chord), "{terminal}/{enhanced}");
                assert_eq!(conflicts::steals(terminal, &chord), None, "{terminal}");
                assert_eq!(
                    binds.action_for(&combo_event(chord)),
                    Some(BindAction::RewindEvaluate),
                    "{terminal}/{enhanced}"
                );
            }
        }
        // The Windows Terminal list keeps the chord that kitty takes. A
        // profile pinned over kitty leaves rewind update on a dead key.
        let mut pinned = Keybinds::default();
        pinned.set_reach(Reach {
            enhanced: true,
            terminal: "Windows Terminal".into(),
        });
        assert_eq!(pinned.binding(BindAction::RewindEvaluate), Some(taken));
    }

    #[test]
    fn a_terminal_change_cannot_create_primary_or_alias_collisions() {
        let mut binds = Keybinds::default();
        binds.learn(BindAction::Undo, Some(KeyCombo::parse("shift+f6").unwrap()));
        let saved = binds.prefs();
        binds.set_reach(Reach {
            enhanced: false,
            terminal: "xterm-256color".into(),
        });
        assert_eq!(binds.binding(BindAction::Undo).unwrap().key(), "shift+f6");
        assert_ne!(
            binds.binding(BindAction::OpenSet),
            binds.binding(BindAction::Undo)
        );
        for action in BindAction::ALL {
            if let Some(chord) = binds.binding(action) {
                let owner = if action == BindAction::Help
                    && binds.binding(BindAction::MenuBar) == binds.binding(action)
                {
                    BindAction::MenuBar
                } else {
                    action
                };
                assert_eq!(binds.action_for(&combo_event(chord)), Some(owner));
            }
        }
        assert_eq!(binds.prefs(), saved);
    }

    #[test]
    fn supplemental_aliases_obey_reach_ownership_and_retirement() {
        let families = [
            (BindAction::Evaluate, "ctrl+s", true),
            (BindAction::Stop, "ctrl+g", true),
            (BindAction::Reference, "f2", true),
            (BindAction::Reference, "ctrl+space", true),
            (BindAction::Settings, "ctrl+shift+p", true),
            (BindAction::HopPane, "ctrl+shift+e", true),
            (BindAction::Zen, "f11", true),
            (BindAction::ToggleComment, "ctrl+_", false),
        ];
        for (action, spelling, enhanced) in families {
            let mut binds = Keybinds::default();
            // This block isolates encoding and ownership from the OS and
            // terminal conflict profiles, which have dedicated tests.
            binds.set_reach(Reach {
                enhanced,
                terminal: String::new(),
            });
            let chord = KeyCombo::parse(spelling).unwrap();
            let event = combo_event(chord);
            assert!(binds.accepts_legacy_alias(action, &event), "{spelling}");
            assert_eq!(binds.action_for(&event), Some(action), "{spelling}");
            assert_eq!(binds.holder_of(chord), Some(action), "{spelling}");
            if KeybindLearn::is_a_chord(chord) {
                let mut shadowed = binds.clone();
                shadowed.learn(BindAction::Jobs, Some(chord));
                assert!(!shadowed.accepts_legacy_alias(action, &event));
                assert_eq!(shadowed.action_for(&event), Some(BindAction::Jobs));
                assert!(
                    !shadowed.is_unbound(action),
                    "taking an alias keeps the primary"
                );
            }
            binds.learn(action, Some(KeyCombo::parse("ctrl+f9").unwrap()));
            assert!(!binds.accepts_legacy_alias(action, &event));
            assert_eq!(binds.action_for(&event), None);
            assert!(binds.overrules(&event));
            binds.learn(action, None);
            binds.unbind(action);
            assert!(!binds.accepts_legacy_alias(action, &event));
            assert_eq!(binds.action_for(&event), None);
            assert!(binds.overrules(&event));
        }
        let mut legacy = Keybinds::default();
        legacy.set_reach(Reach {
            enhanced: false,
            terminal: "xterm-256color".into(),
        });
        for (action, spelling) in [
            (BindAction::Evaluate, "ctrl+enter"),
            (BindAction::Stop, "ctrl+."),
        ] {
            let event = combo_event(KeyCombo::parse(spelling).unwrap());
            assert!(!legacy.accepts_legacy_alias(action, &event));
            assert!(legacy.overrules(&event));
        }
        let mut windows = Keybinds::default();
        windows.set_reach(Reach {
            enhanced: true,
            terminal: "Windows Terminal".into(),
        });
        assert!(!windows.accepts_legacy_alias(
            BindAction::Zen,
            &combo_event(KeyCombo::parse("f11").unwrap())
        ));
    }

    /// Ctrl+F lists values and Ctrl+D opens docs: two behaviours, so two
    /// actions with two names. Ctrl+Space belongs to the values action.
    #[test]
    fn values_and_docs_are_two_actions_with_two_names() {
        let binds = Keybinds::default();
        let values = binds.holder_of(KeyCombo::parse("ctrl+f").unwrap()).unwrap();
        let docs = binds.holder_of(KeyCombo::parse("ctrl+d").unwrap()).unwrap();
        assert_ne!(values, docs);
        assert_eq!(values.key(), "reference");
        assert_eq!(docs.key(), "docs");
        assert_eq!(values.label(), "argument values, reference");
        assert_eq!(docs.label(), "docs for the function");
        assert_eq!(BindAction::parse_key("docs"), Some(docs));
        assert_eq!(binds.default_holder_of(docs.default_binding()), Some(docs));
        assert_eq!(
            binds.action_for(&press(KeyCode::Char(' '), KeyModifiers::CONTROL)),
            Some(values)
        );
        assert_eq!(binds.effective_alias(values), None);
        assert_eq!(binds.effective_alias(docs), None);
    }

    /// A file saved before the split names only "reference". Its chord
    /// loads, moves the values action alone, and leaves Ctrl+D on docs.
    #[test]
    fn a_saved_reference_chord_loads_and_leaves_the_docs_chord() {
        let ctrl_d = press(KeyCode::Char('d'), KeyModifiers::CONTROL);
        let ctrl_f = press(KeyCode::Char('f'), KeyModifiers::CONTROL);
        for chord in ["f3", ""] {
            let prefs = KeybindPrefs {
                bindings: vec![KeybindPref {
                    action: "reference".into(),
                    chord: chord.into(),
                }],
            };
            let mut binds = Keybinds::default();
            binds.restore(&prefs);
            assert_eq!(binds.prefs(), prefs, "{chord:?}");
            assert_eq!(
                binds.binding(BindAction::Reference),
                KeyCombo::parse(chord),
                "{chord:?}"
            );
            assert_eq!(binds.action_for(&ctrl_f), None, "{chord:?}");
            assert!(binds.overrules(&ctrl_f), "{chord:?}");
            let docs = binds.action_for(&ctrl_d).expect("Ctrl+D keeps an owner");
            assert_eq!(docs.key(), "docs", "{chord:?}");
            assert!(!binds.overrules(&ctrl_d), "{chord:?}");
        }
    }

    /// A file saved before the split can hold Ctrl+D for another action.
    /// That action keeps the chord, and docs moves to a key nobody holds.
    #[test]
    fn a_saved_chord_on_ctrl_d_keeps_its_action_and_docs_moves_to_a_free_key() {
        let ctrl_d = press(KeyCode::Char('d'), KeyModifiers::CONTROL);
        let prefs = KeybindPrefs {
            bindings: vec![KeybindPref {
                action: "mixer".into(),
                chord: "ctrl+d".into(),
            }],
        };
        let mut binds = Keybinds::default();
        binds.restore(&prefs);
        assert_eq!(binds.prefs(), prefs);
        assert_eq!(binds.action_for(&ctrl_d), Some(BindAction::Mixer));
        let docs = binds.binding(BindAction::Docs).expect("docs keeps a key");
        assert_ne!(docs, BindAction::Docs.default_binding());
        assert_eq!(binds.holder_of(docs), Some(BindAction::Docs));
        assert_eq!(
            binds.binding(BindAction::Reference),
            Some(BindAction::Reference.default_binding())
        );
    }

    /// Ctrl+Space is named beside Ctrl+F only where the terminal sends it,
    /// and only while the values action keeps its own chord.
    #[test]
    fn ctrl_space_is_named_only_where_the_terminal_sends_it() {
        use super::super::terminal::conflicts;
        let space = KeyCombo::parse("ctrl+space").unwrap();
        let _desktop = conflicts::ForceDesktopForTest::set("");
        for (platform, terminal, named) in [
            ("linux", "kitty", true),
            ("linux", "xterm-256color", true),
            ("macos", "kitty", false),
            ("linux", "vscode 1.99.0", false),
        ] {
            let _platform = conflicts::ForcePlatformForTest::set(platform);
            let mut binds = Keybinds::default();
            binds.set_reach(Reach {
                enhanced: false,
                terminal: terminal.into(),
            });
            assert_eq!(
                binds.advertised_alias(BindAction::Reference),
                named.then_some(space),
                "{platform}/{terminal}"
            );
            assert_ne!(binds.binding(BindAction::Reference), Some(space));
        }
        let mut binds = Keybinds::default();
        assert_eq!(binds.advertised_alias(BindAction::Reference), Some(space));
        assert_eq!(
            binds.advertised_alias(BindAction::Evaluate),
            binds.effective_alias(BindAction::Evaluate)
        );
        binds.learn(BindAction::Reference, KeyCombo::parse("f3"));
        assert_eq!(binds.advertised_alias(BindAction::Reference), None);
    }

    #[test]
    fn ctrl_space_alias_handles_both_terminal_encodings_without_claiming_typing() {
        let mut binds = Keybinds::default();
        for event in [
            press(KeyCode::Null, KeyModifiers::NONE),
            press(KeyCode::Null, KeyModifiers::CONTROL),
            press(KeyCode::Char(' '), KeyModifiers::CONTROL),
        ] {
            assert_eq!(binds.action_for(&event), Some(BindAction::Reference));
        }
        assert_eq!(
            binds.action_for(&press(KeyCode::Char(' '), KeyModifiers::NONE)),
            None
        );
        binds.unbind(BindAction::Reference);
        for event in [
            press(KeyCode::Null, KeyModifiers::NONE),
            press(KeyCode::Char(' '), KeyModifiers::CONTROL),
        ] {
            assert_eq!(binds.action_for(&event), None);
            assert!(binds.overrules(&event));
        }
    }

    #[test]
    fn shifted_bracket_spellings_have_one_owner_and_retire_together() {
        for (action, bracket, glyph) in [
            (BindAction::PreviousScene, '[', '{'),
            (BindAction::NextScene, ']', '}'),
        ] {
            let events = [
                press(
                    KeyCode::Char(bracket),
                    KeyModifiers::CONTROL | KeyModifiers::SHIFT,
                ),
                press(KeyCode::Char(glyph), KeyModifiers::CONTROL),
                press(
                    KeyCode::Char(glyph),
                    KeyModifiers::CONTROL | KeyModifiers::SHIFT,
                ),
            ];
            let mut binds = Keybinds::default();
            for event in &events {
                assert!(binds.accepts_legacy_alias(action, event));
                assert_eq!(binds.action_for(event), Some(action));
            }
            let chord = KeyCombo::parse(&format!("ctrl+{glyph}")).unwrap();
            assert_eq!(
                chord,
                KeyCombo::parse(&format!("ctrl+shift+{bracket}")).unwrap()
            );
            assert_eq!(binds.holder_of(chord), Some(action));
            binds.learn(BindAction::Jobs, Some(chord));
            for event in &events {
                assert!(!binds.accepts_legacy_alias(action, event));
                assert_eq!(binds.action_for(event), Some(BindAction::Jobs));
            }
            let mut binds = Keybinds::default();
            binds.unbind(action);
            for event in &events {
                assert_eq!(binds.action_for(event), None);
                assert!(binds.overrules(event));
            }
            binds.learn(action, None);
            binds.set_reach(Reach {
                enhanced: false,
                terminal: "xterm-256color".into(),
            });
            for event in &events {
                assert!(!binds.accepts_legacy_alias(action, event));
                assert!(binds.overrules(event));
            }
        }
    }

    #[test]
    fn choosing_an_existing_alias_makes_it_the_explicit_primary() {
        let mut binds = Keybinds::default();
        let f2 = KeyCombo::parse("f2").unwrap();
        assert_eq!(binds.holder_of(f2), Some(BindAction::Reference));
        binds.learn(BindAction::Reference, Some(f2));
        assert_eq!(binds.binding(BindAction::Reference), Some(f2));
        assert!(binds.overridden(BindAction::Reference));
        assert!(binds.overrules(&combo_event(BindAction::Reference.default_binding())));
        let mut restored = Keybinds::default();
        restored.restore(&binds.prefs());
        assert_eq!(restored.binding(BindAction::Reference), Some(f2));
    }
}
