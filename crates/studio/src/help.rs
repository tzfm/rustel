//! Contextual keyboard help, kept orthogonal to the studio's panel stack.
//!
//! Help is an overlay rather than a [`super::view::PanelKind`]: opening it
//! never moves the caret, changes panel focus, dismisses a nested sheet, or
//! lets a click reach whatever it covers.  The small context card stays put
//! while the complete shortcut list beneath it scrolls.

use crossterm::event::KeyCode;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::widgets::Widget;
use unicode_width::UnicodeWidthStr;

use super::editor::KeyboardCapabilities;
use super::keybinds::{BindAction, KeyCombo, Keybinds};
use super::theme::Theme;

/// What owns the keyboard underneath the help overlay.
///
/// Theme-editor substates are separate because their keys really are
/// different: plain `s` saves from the form, but types JSON on the code tab.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum HelpContext {
    #[default]
    Editor,
    SceneRename,
    SceneLearn,
    Slider,
    /// A replay's timeline has the keyboard.
    Timeline,
    ThemePicker,
    ThemeEditorForm,
    ThemeEditorEntry,
    ThemeEditorCode,
    ThemeEditorColor,
    ThemeEditorSave,
    Reference,
    Settings,
    SettingsAbout,
    SettingsSources,
    Log,
    Jobs,
    Export,
    Devices,
    Set,
    Viz,
    /// The mixer panel along the top or the bottom.
    Mixer,
    /// The memory breakdown, docked along the top or the bottom.
    Memory,
}

impl HelpContext {
    fn title(self) -> &'static str {
        match self {
            Self::Editor => "editor",
            Self::SceneRename => "scene rename",
            Self::SceneLearn => "scene pad learn",
            Self::Slider => "live slider",
            Self::Timeline => "timeline",
            Self::ThemePicker => "theme picker",
            Self::ThemeEditorForm => "theme editor · form",
            Self::ThemeEditorEntry => "theme editor · value",
            Self::ThemeEditorCode => "theme editor · JSON",
            Self::ThemeEditorColor => "theme editor · colour",
            Self::ThemeEditorSave => "theme editor · save",
            Self::Reference => "reference",
            Self::Settings => "settings",
            Self::SettingsAbout => "settings · about",
            Self::SettingsSources => "settings · samples",
            Self::Log => "log",
            Self::Jobs => "background jobs",
            Self::Export => "export",
            Self::Devices => "devices",
            Self::Set => "set panel",
            Self::Viz => "visuals panel",
            Self::Mixer => "mixer",
            Self::Memory => "memory breakdown",
        }
    }

    /// A summary names a panel chord as `{timeline}`, `{rename}` or
    /// `{show}`. The sheet fills in the chord the table holds.
    fn summary(self) -> &'static str {
        match self {
            Self::Editor => "Type normally; this is a modeless source editor.",
            Self::SceneRename => "The scene name is being edited; the score stays untouched.",
            Self::SceneLearn => "The current scene is waiting for a MIDI pad press.",
            Self::Slider => "The last pointer-touched slider owns the plain left/right arrows.",
            Self::Timeline => {
                "{timeline} focuses the tape timeline; its arrows choose the block shown in the editor."
            }
            Self::ThemePicker => {
                "Moving the selection previews each theme immediately; on the chance row \
                 ←/→ rolls a new one, and ^E opens the roll on screen in the editor."
            }
            Self::ThemeEditorForm => "Every change applies live; nothing is kept until saved.",
            Self::ThemeEditorEntry => "A theme field is being typed; commit or cancel the value.",
            Self::ThemeEditorCode => "Edit the theme JSON with the normal editor keymap.",
            Self::ThemeEditorColor => "The colour under the painter is applied live.",
            Self::ThemeEditorSave => "Name the theme, then save it or save and keep it.",
            Self::Reference => "Search, browse, preview, and insert from the reference column.",
            Self::Settings => "Settings change live and are remembered when they change.",
            Self::SettingsAbout => "This page describes the terminal and runtime capabilities.",
            Self::SettingsSources => {
                "Fetch imports, cache all remote packs, refresh their lists, and clear the cache; then your folders and the studio's packs. r aliases a bank on a user import; c caches a remote pack."
            }
            Self::Log => {
                "The newest studio messages are at the bottom of the log. v shows the running commentary underneath them."
            }
            Self::Jobs => {
                "Background work - sample downloads, exports, cache clears - with a name and how far each has got. The header chip spins while anything is running."
            }
            Self::Export => "Choose a length and format for the focused scene's render.",
            Self::Devices => "Select audio input/output hardware or copy a MIDI port call.",
            Self::Set => {
                "The set's folder: its scores, numbered when open on the strip, and under \
                 the sessions fold the tapes recorded from it. {rename} renames a tape; {show} reveals its file."
            }
            Self::Viz => {
                "Widgets that move with the music, in two docks - a column down a side or a \
                 band across the top or the bottom, each - kept with the preferences for \
                 every set: the set's name as ANSI art, a scope, an analyser, a vectorscope, \
                 the events, each in its own styles and colourings."
            }
            Self::Mixer => {
                "The desk: a strip each for the audio input, the orbits the score names, \
                 the master and this set's limiter if it has one, meters up the strips and \
                 faders on the input and the master, with the MIDI ports and the gamepads \
                 beside them. It stays while you edit."
            }
            Self::Memory => {
                "Where the header's mem goes: the parts that add up to it, each against \
                 the limit that holds it, docked along the bottom or the top beside the \
                 score. It stays while you work, and the settings sheet stands clear of it."
            }
        }
    }

    /// The three most useful bindings for the surface underneath.  Keeping a
    /// fixed number makes the scroll viewport stable as the context changes.
    fn bindings(self) -> [(&'static str, &'static str); 3] {
        match self {
            Self::Editor => [
                ("^S / F5", "write and play the current scene"),
                ("^G / F8", "stop all sound"),
                ("^F / ^D", "argument values / function docs"),
            ],
            Self::SceneRename => [
                ("type · Backspace", "edit the scene name"),
                ("Enter", "accept the new name"),
                ("Esc", "cancel and keep the old name"),
            ],
            Self::SceneLearn => [
                ("MIDI pad", "bind that pad to the current scene"),
                ("Esc", "cancel learning"),
                ("^⇧L", "forget the scene's existing pad"),
            ],
            Self::Timeline => [
                (
                    "← / → · Home / End",
                    "choose a block; its code goes in the editor",
                ),
                (
                    "Enter",
                    "play the chosen block; the bar under the strip pans it",
                ),
                (
                    "T · Delete · Esc · ^W",
                    "exact duration / delete block / back to the text / close the tape",
                ),
            ],
            Self::Slider => [
                ("Left / Right", "step the armed slider"),
                ("Enter / Esc", "expand for exact entry / let go"),
                (
                    "drag / wheel",
                    "set or nudge under the pointer - a controller reaches a slider through the mixer's desk and a mapping slot, not through this",
                ),
            ],
            Self::Viz => [
                (
                    "Tab · Up/Down · ←/→ · Space",
                    "choose a widget / its kind / its style / its colour",
                ),
                (
                    "a · Delete · Enter · j",
                    "add / remove / artwork text / artwork alignment",
                ),
                (
                    "e · +/- · Esc",
                    "move the dock to the next edge / size it / back to the score",
                ),
            ],
            Self::Memory => [
                ("e", "the top or the bottom"),
                ("- / +", "a row shorter / taller"),
                ("Esc · click ×", "back to the score, docked / hide it"),
            ],
            Self::Mixer => [
                ("←/→ · Enter", "choose a strip / the next one"),
                ("↑/↓ · 0", "the strip's fader a decibel / back to unity"),
                (
                    "e · +/- · Esc · F4",
                    "top or bottom / taller or shorter / back to the score / hide",
                ),
            ],
            Self::Set => [
                (
                    "Up/Down · Enter",
                    "choose a score or a tape / open it (a double click too)",
                ),
                ("Space · n", "fold sessions / start a new session tape"),
                (
                    "Delete · Esc · ^B",
                    "delete (Enter confirms) / back to the score / hide the panel",
                ),
            ],
            Self::ThemePicker => [
                ("type · Backspace", "filter themes by name"),
                ("Left/Right · Up/Down", "preview the previous / next theme"),
                (
                    "Enter/Esc · ^N/E/D",
                    "keep/back · new/edit/delete (delete twice)",
                ),
            ],
            Self::ThemeEditorForm => [
                ("Up/Down · PgUp/PgDn", "choose a theme field"),
                ("Left/Right · Enter", "step a value / edit or paint it"),
                ("s · Tab · Esc", "save / edit JSON / back to the themes"),
            ],
            Self::ThemeEditorEntry => [
                ("type · Backspace", "edit this field's value"),
                ("Enter", "apply the value"),
                ("Esc", "discard this field edit"),
            ],
            Self::ThemeEditorCode => [
                ("editor keys", "edit and select the theme JSON"),
                (
                    "Tab / Esc",
                    "parse and return to the form / back to the themes",
                ),
                ("s on the Form tab", "open the save sheet"),
            ],
            Self::ThemeEditorColor => [
                ("arrow keys", "move through the colour grid and apply live"),
                ("#rrggbb · Backspace", "type an exact colour"),
                ("Enter / Esc", "keep this colour / restore the old one"),
            ],
            Self::ThemeEditorSave => [
                ("type · Backspace", "edit the theme name"),
                ("Tab / Left / Right", "choose save or save & keep"),
                (
                    "Enter / Esc",
                    "save / back - or discard, when Esc raised the sheet",
                ),
            ],
            Self::Reference => [
                ("type · Backspace", "search the current tab"),
                (
                    "arrows · PgUp/PgDn · Tab",
                    "navigate, fold/open, change tab",
                ),
                ("Enter · Space · Esc", "use · preview · go back/close"),
            ],
            Self::Settings => [
                ("Up / Down", "choose a setting"),
                ("Left / Right / Space", "change the selected setting live"),
                (
                    "Tab · Enter · Esc",
                    "pages · open webcam preview / close · close",
                ),
            ],
            Self::SettingsAbout => [
                ("Tab / BackTab", "return to settings"),
                ("Esc", "close settings"),
                ("editor keys", "unclaimed keys still belong to the score"),
            ],
            Self::SettingsSources => [
                ("a / +", "import a folder, a URL, or github:user/repo"),
                (
                    "Space · Enter · r · d",
                    "on/off · refetch · alias a bank · remove",
                ),
                (
                    "c · Tab · Esc",
                    "cache this remote pack · other pages · close",
                ),
            ],
            Self::Log => [
                (
                    "Up/Down · PgUp/PgDn · Home/End",
                    "scroll / oldest messages / follow newest",
                ),
                (
                    "e · -/+",
                    "docked: the top or the bottom / shorter or taller",
                ),
                (
                    "v · Esc / F9",
                    "the running commentary too / close / toggle",
                ),
            ],
            Self::Jobs => [
                ("Esc", "close the list"),
                ("click the header chip", "opens this list while jobs run"),
                ("click elsewhere", "closes the list"),
            ],
            Self::Export => [
                ("Tab/Up/Down", "move between export fields"),
                ("Left/Right · type", "change a choice or numeric value"),
                ("Enter / Esc", "start rendering / close"),
            ],
            Self::Devices => [
                ("Up / Down", "choose a device or port"),
                ("Tab/BackTab · Left/Right", "switch device families"),
                ("Enter / Esc", "use the selection / close"),
            ],
        }
    }

    fn live_bindings(self, keybinds: &Keybinds) -> [(String, &'static str); 3] {
        let mut rows = self
            .bindings()
            .map(|(keys, action)| (keys.to_owned(), action));
        let hint = |action| shortcut_with_alias(keybinds, action);
        let append = |prefix: &str, action| join_hints([prefix.to_owned(), hint(action)], " · ");
        match self {
            Self::Editor => {
                rows[0].0 = hint(BindAction::Evaluate);
                rows[1].0 = hint(BindAction::Stop);
                // Two actions on one row, in the order of the two texts. A
                // chord that is alone on the row keeps only its own text.
                let values = shortcut_hint(keybinds, BindAction::Reference);
                let docs = shortcut_hint(keybinds, BindAction::Docs);
                rows[2].1 = match (values.is_empty(), docs.is_empty()) {
                    (false, true) => "argument values",
                    (true, false) => "function docs",
                    _ => rows[2].1,
                };
                rows[2].0 = join_hints([values, docs], " / ");
            }
            Self::SceneLearn => rows[2].0 = hint(BindAction::ForgetPad),
            Self::Timeline => rows[2].0 = append("T · Delete · Esc", BindAction::CloseScene),
            Self::Mixer => rows[2].0 = append("e · +/- · Esc", BindAction::Mixer),
            Self::Set => rows[2].0 = append("Delete · Esc", BindAction::SetPanel),
            Self::Log => rows[2].0 = append("v · Esc", BindAction::Log),
            Self::Memory => rows[2].0 = append("Esc · click ×", BindAction::Memory),
            _ => {}
        }
        rows
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct HelpRow {
    keys: String,
    action: &'static str,
}

impl HelpRow {
    fn new(keys: impl Into<String>, action: &'static str) -> Self {
        Self {
            keys: super::keybinds::shortcut_label(&keys.into()).into_owned(),
            action,
        }
    }
}

/// Every shortcut promise comes from the effective dispatch table.
fn shortcut_hint(keybinds: &Keybinds, action: BindAction) -> String {
    keybinds.hint(action)
}

fn shortcut_with_alias(keybinds: &Keybinds, action: BindAction) -> String {
    let mut hints = vec![shortcut_hint(keybinds, action)];
    if let Some(alias) = keybinds.advertised_alias(action) {
        hints.push(alias.hint());
    }
    join_hints(hints, " / ")
}

fn join_hints(hints: impl IntoIterator<Item = String>, separator: &str) -> String {
    hints
        .into_iter()
        .filter(|hint| !hint.is_empty())
        .collect::<Vec<_>>()
        .join(separator)
}

/// The complete studio-wide list. Context-only keys stay in the pinned card;
/// this list is the map back to features from anywhere in the studio.
fn shortcut_rows(capabilities: KeyboardCapabilities) -> Vec<HelpRow> {
    shortcut_rows_with_keybinds(capabilities, &Keybinds::default())
}

fn shortcut_rows_with_keybinds(
    capabilities: KeyboardCapabilities,
    keybinds: &Keybinds,
) -> Vec<HelpRow> {
    let hint = |action: BindAction| shortcut_hint(keybinds, action);
    let scene_switch = |split| {
        join_hints(
            [
                super::menu::scene_shortcut_hint(
                    keybinds,
                    capabilities,
                    BindAction::PreviousScene,
                    split,
                ),
                super::menu::scene_shortcut_hint(
                    keybinds,
                    capabilities,
                    BindAction::NextScene,
                    split,
                ),
            ],
            " · ",
        )
    };
    let rows = vec![
        HelpRow::new(
            hint(BindAction::MenuBar),
            "open the menu bar; Help opens keyboard reference",
        ),
        HelpRow::new(
            shortcut_with_alias(keybinds, BindAction::Evaluate),
            "write and play the current scene",
        ),
        HelpRow::new(
            hint(BindAction::RewindEvaluate),
            "rewind update: play it from its own cycle zero, once",
        ),
        HelpRow::new(
            shortcut_with_alias(keybinds, BindAction::Stop),
            "stop all sound",
        ),
        HelpRow::new(
            hint(BindAction::FirstError),
            "jump to the first blocking error; otherwise reveal the editor cursor",
        ),
        HelpRow::new(hint(BindAction::Undo), "undo"),
        HelpRow::new(shortcut_with_alias(keybinds, BindAction::Redo), "redo"),
        HelpRow::new(
            hint(BindAction::ToggleComment),
            "comment or uncomment lines",
        ),
        HelpRow::new(hint(BindAction::Docs), "docs for the function at the caret"),
        HelpRow::new(
            shortcut_with_alias(keybinds, BindAction::Reference),
            "values for the argument at the caret; elsewhere the reference",
        ),
        HelpRow::new(hint(BindAction::Devices), "open devices"),
        #[cfg(feature = "remote-control")]
        HelpRow::new(
            "F1, O, R",
            "remote control: Enter enables/disables, R reveals, C copies",
        ),
        HelpRow::new(
            hint(BindAction::PianoMode),
            "piano mode: audition notes and chords",
        ),
        HelpRow::new(hint(BindAction::Settings), "open settings"),
        HelpRow::new(
            "Options > Settings > prebakes",
            "open the selected global/local prebake",
        ),
        HelpRow::new(hint(BindAction::ThemePicker), "open the theme picker"),
        HelpRow::new(hint(BindAction::SetPanel), "show or hide the set panel"),
        HelpRow::new(hint(BindAction::OpenSet), "open another set"),
        HelpRow::new(
            "n in set panel",
            "start a new session tape; keep scores and playback",
        ),
        HelpRow::new(
            shortcut_with_alias(keybinds, BindAction::Log),
            "open the studio log",
        ),
        HelpRow::new(hint(BindAction::Export), "export the focused scene"),
        HelpRow::new(
            "Transport \u{25b8} Reveal last export or take",
            "show the last export or take in the file manager",
        ),
        HelpRow::new(
            format!("File \u{25b8} {}", super::menu::show_set_label()),
            "show the set's folder in the file manager",
        ),
        HelpRow::new(hint(BindAction::RecordTake), "start or finish a WAV take"),
        HelpRow::new(scene_switch(false), "one pane: previous / next scene"),
        HelpRow::new(
            scene_switch(true),
            "split: cycle the focused pane, skipping the other pane's scene",
        ),
        HelpRow::new(hint(BindAction::NewScene), "new scene"),
        HelpRow::new(
            shortcut_with_alias(keybinds, BindAction::DuplicateScene),
            "duplicate the current scene",
        ),
        HelpRow::new(hint(BindAction::RenameScene), "rename the current scene"),
        HelpRow::new(
            hint(BindAction::CloseScene),
            "write and close the current scene",
        ),
        HelpRow::new("MIDI pad / Scene menu", "launch a scene on its cycle line"),
        HelpRow::new(
            hint(BindAction::LearnPad),
            "learn a MIDI pad for this scene",
        ),
        HelpRow::new(hint(BindAction::ForgetPad), "forget this scene's pad"),
        HelpRow::new(
            hint(BindAction::SceneRewind),
            "rewind on play: this scene always starts at its own cycle zero \u{21ba}",
        ),
        HelpRow::new(
            hint(BindAction::Split),
            "split the editor, or close the split",
        ),
        HelpRow::new(
            hint(BindAction::HopPane),
            "split: switch focus between the two visible panes",
        ),
        HelpRow::new(
            hint(BindAction::FocusPanels),
            "switch focus between visible panels",
        ),
        HelpRow::new(
            shortcut_with_alias(keybinds, BindAction::Mixer),
            "show or hide the mixer",
        ),
        HelpRow::new(
            hint(BindAction::VisualsOne),
            "show or hide the first visuals dock",
        ),
        HelpRow::new(
            hint(BindAction::VisualsTwo),
            "show or hide the second visuals dock",
        ),
        HelpRow::new(hint(BindAction::Wrap), "word wrap on or off"),
        HelpRow::new(
            format!(
                "{}/{}",
                keybinds.hint(BindAction::MasterUp),
                keybinds.hint(BindAction::MasterDown)
            ),
            "raise / lower master volume",
        ),
        HelpRow::new(
            "Alt+Up/Down",
            "nudge the slider under the caret by its step",
        ),
        HelpRow::new(hint(BindAction::Zen), "toggle zen mode"),
        HelpRow::new(hint(BindAction::SelectAll), "select all score text"),
        HelpRow::new(
            format!(
                "{}/{}/{}",
                keybinds.hint(BindAction::Copy),
                keybinds.hint(BindAction::Cut),
                keybinds.hint(BindAction::Paste)
            ),
            "copy / cut / paste",
        ),
        HelpRow::new("Shift+movement", "extend the text selection"),
        HelpRow::new(
            "Shift+Home/End",
            // Ghostty on Linux scrolls its history with these two keys and
            // sends neither of them to the studio.
            if keybinds.delivers(&KeyCombo {
                code: KeyCode::End,
                control: false,
                shift: true,
            }) {
                "select to the start / end of the line"
            } else {
                "not sent by this terminal - free them in its config"
            },
        ),
        HelpRow::new("Alt+Left/Right", "move by word"),
        HelpRow::new(
            hint(BindAction::Quit),
            "write edited scenes and quit (press it twice)",
        ),
    ];
    rows
}

/// Scroll position for the broad list. No focus or panel state lives here.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct HelpState {
    pub scroll: usize,
}

impl HelpState {
    fn max_scroll(area: Rect, capabilities: KeyboardCapabilities) -> usize {
        shortcut_rows(capabilities)
            .len()
            .saturating_sub(usize::from(
                HelpView::list_area(area).map_or(0, |area| area.height),
            ))
    }

    fn move_by(&mut self, delta: isize, area: Rect, capabilities: KeyboardCapabilities) {
        let next = if delta < 0 {
            self.scroll.saturating_sub(delta.unsigned_abs())
        } else {
            self.scroll.saturating_add(delta as usize)
        };
        self.scroll = next.min(Self::max_scroll(area, capabilities));
    }

    /// Handle a navigation key. Closing is deliberately left to the app so
    /// `Esc` and `F1` have one obvious routing point.
    pub fn scroll_key(
        &mut self,
        code: KeyCode,
        area: Rect,
        capabilities: KeyboardCapabilities,
    ) -> bool {
        let page = usize::from(HelpView::list_area(area).map_or(1, |area| area.height)).max(1);
        match code {
            KeyCode::Up => self.move_by(-1, area, capabilities),
            KeyCode::Down => self.move_by(1, area, capabilities),
            KeyCode::PageUp => self.move_by(-(page as isize), area, capabilities),
            KeyCode::PageDown => self.move_by(page as isize, area, capabilities),
            KeyCode::Home => self.scroll = 0,
            KeyCode::End => self.scroll = Self::max_scroll(area, capabilities),
            _ => return false,
        }
        true
    }

    pub fn wheel(&mut self, upwards: bool, area: Rect, capabilities: KeyboardCapabilities) {
        self.move_by(if upwards { -3 } else { 3 }, area, capabilities);
    }
}

/// The top-most modal help sheet.
pub struct HelpView<'a> {
    pub state: HelpState,
    pub context: HelpContext,
    pub capabilities: KeyboardCapabilities,
    pub keybinds: &'a Keybinds,
    pub theme: &'a Theme,
}

impl HelpView<'_> {
    /// A centred sheet, tall enough to keep the context card visible and to
    /// show at least one broad shortcut below it.
    pub fn geometry(available: Rect) -> Option<Rect> {
        let width = available.width.saturating_sub(2).min(96);
        let height = available.height.saturating_sub(2).min(30);
        if width < 28 || height < 11 {
            return None;
        }
        Some(Rect::new(
            available.x + available.width.saturating_sub(width) / 2,
            available.y + available.height.saturating_sub(height) / 2,
            width,
            height,
        ))
    }

    fn list_area(available: Rect) -> Option<Rect> {
        let panel = Self::geometry(available)?;
        let y = panel.y + 8;
        Some(Rect::new(
            panel.x + 2,
            y,
            panel.width.saturating_sub(4),
            panel.bottom().saturating_sub(2).saturating_sub(y),
        ))
    }
}

fn draw_binding(
    buffer: &mut Buffer,
    area: Rect,
    keys: &str,
    action: &str,
    theme: &Theme,
    muted: bool,
) {
    if area.is_empty() {
        return;
    }
    let key_width = if area.width >= 62 { 24 } else { 18 }.min(area.width);
    // The reference is the one page whose whole job is naming keys, so a
    // modifier the console cannot draw is a box where the answer should
    // be. Every row and the learn strip alike are spelled through here.
    let keys = super::keybinds::shortcut_label(keys);
    let keys = crate::terminal::safe_text(&keys);
    buffer.set_stringn(
        area.x,
        area.y,
        keys.as_ref(),
        usize::from(key_width),
        Style::default()
            .fg(theme.accent)
            .add_modifier(Modifier::BOLD),
    );
    if area.width > key_width + 1 {
        buffer.set_stringn(
            area.x + key_width + 1,
            area.y,
            action,
            usize::from(area.width - key_width - 1),
            Style::default().fg(if muted { theme.muted } else { theme.foreground }),
        );
    }
}

impl Widget for HelpView<'_> {
    fn render(self, available: Rect, buffer: &mut Buffer) {
        let Some(panel) = Self::geometry(available) else {
            return;
        };
        let theme = self.theme;
        super::view::clear_overlay(
            buffer,
            panel,
            Style::default().bg(theme.overlay).fg(theme.foreground),
        );
        super::devices::draw_border(buffer, panel, theme);

        let title = " keyboard help ";
        buffer.set_stringn(
            panel.x + 2,
            panel.y,
            title,
            usize::from(panel.width.saturating_sub(4)),
            Style::default()
                .fg(theme.accent)
                .bg(theme.overlay)
                .add_modifier(Modifier::BOLD),
        );
        let context_title = format!(" {} ", self.context.title());
        let context_width = UnicodeWidthStr::width(context_title.as_str()) as u16;
        if panel.width > context_width + UnicodeWidthStr::width(title) as u16 + 5 {
            buffer.set_stringn(
                panel.right().saturating_sub(context_width + 2),
                panel.y,
                &context_title,
                usize::from(context_width),
                Style::default().fg(theme.muted).bg(theme.overlay),
            );
        }

        let inner = Rect::new(
            panel.x + 2,
            panel.y + 1,
            panel.width.saturating_sub(4),
            panel.height.saturating_sub(2),
        );
        draw_binding(
            buffer,
            Rect::new(inner.x, inner.y, inner.width, 1),
            &join_hints(
                [
                    shortcut_hint(self.keybinds, BindAction::MenuBar),
                    "Esc".to_owned(),
                ],
                " / ",
            ),
            "close help and return exactly where you were",
            theme,
            false,
        );
        let transport = format!(
            "{} · {}",
            shortcut_with_alias(self.keybinds, BindAction::Evaluate).replace(" / ", "/"),
            shortcut_with_alias(self.keybinds, BindAction::Stop).replace(" / ", "/")
        );
        draw_binding(
            buffer,
            Rect::new(inner.x, inner.y + 1, inner.width, 1),
            &transport,
            "update and stop stay live while help is open",
            theme,
            false,
        );
        buffer.set_stringn(
            inner.x,
            inner.y + 2,
            format!(
                "NOW  {}",
                super::keybinds::shortcut_label(self.context.summary())
                    .replace("{timeline}", &self.keybinds.hint(BindAction::FocusTimeline))
                    .replace("{rename}", &self.keybinds.hint(BindAction::RenameFile))
                    .replace("{show}", &self.keybinds.hint(BindAction::ShowFile))
            ),
            usize::from(inner.width),
            Style::default().fg(theme.ok).add_modifier(Modifier::BOLD),
        );
        for (offset, (keys, action)) in self
            .context
            .live_bindings(self.keybinds)
            .into_iter()
            .enumerate()
        {
            draw_binding(
                buffer,
                Rect::new(inner.x, inner.y + 3 + offset as u16, inner.width, 1),
                &keys,
                action,
                theme,
                true,
            );
        }

        let divider_y = panel.y + 7;
        for x in inner.x..inner.right() {
            if let Some(cell) = buffer.cell_mut((x, divider_y)) {
                cell.set_symbol("─")
                    .set_style(Style::default().fg(theme.rule).bg(theme.overlay));
            }
        }
        buffer.set_stringn(
            inner.x + 1,
            divider_y,
            " all studio shortcuts ",
            usize::from(inner.width.saturating_sub(2)),
            Style::default()
                .fg(theme.foreground)
                .bg(theme.overlay)
                .add_modifier(Modifier::BOLD),
        );

        let rows = shortcut_rows_with_keybinds(self.capabilities, self.keybinds);
        let list = Self::list_area(available).unwrap_or_default();
        let max_scroll = rows.len().saturating_sub(usize::from(list.height));
        let first = self.state.scroll.min(max_scroll);
        for (offset, row) in rows
            .iter()
            .skip(first)
            .take(usize::from(list.height))
            .enumerate()
        {
            draw_binding(
                buffer,
                Rect::new(list.x, list.y + offset as u16, list.width, 1),
                &row.keys,
                row.action,
                theme,
                false,
            );
        }

        let shown = usize::from(list.height).min(rows.len().saturating_sub(first));
        let position = if rows.is_empty() {
            "0 / 0".to_owned()
        } else {
            format!("{}-{} / {}", first + 1, first + shown, rows.len())
        };
        let position_width = UnicodeWidthStr::width(position.as_str()) as u16;
        let footer_y = panel.bottom() - 2;
        let hint = "Up/Down · PgUp/PgDn · Home/End · wheel";
        buffer.set_stringn(
            inner.x,
            footer_y,
            hint,
            usize::from(inner.width.saturating_sub(position_width + 2)),
            Style::default().fg(theme.muted),
        );
        if position_width < inner.width {
            buffer.set_stringn(
                inner.right().saturating_sub(position_width),
                footer_y,
                &position,
                usize::from(position_width),
                Style::default().fg(theme.muted),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text_of(buffer: &Buffer) -> String {
        (buffer.area.y..buffer.area.bottom())
            .map(|y| {
                (buffer.area.x..buffer.area.right())
                    .filter_map(|x| buffer.cell((x, y)))
                    .map(|cell| cell.symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// The table decides what help names, not the keys seen so far. A rebind
    /// or an unbind retires Ctrl+Space with the default chord.
    #[test]
    fn reference_help_follows_the_table_and_not_the_keys_seen() {
        use super::super::keybinds::KeyCombo;
        let mut binds = Keybinds::default();
        let browse = |binds: &Keybinds, space_seen| {
            shortcut_rows_with_keybinds(
                KeyboardCapabilities {
                    space_seen,
                    ..KeyboardCapabilities::legacy()
                },
                binds,
            )
            .into_iter()
            .find(|row| row.action.starts_with("values for the argument"))
            .unwrap()
            .keys
        };
        assert_eq!(browse(&binds, false), "^F / ^Space");
        assert_eq!(browse(&binds, true), "^F / ^Space");
        binds.learn(BindAction::Reference, Some(KeyCombo::parse("f2").unwrap()));
        assert_eq!(browse(&binds, true), "F2");
        binds.unbind(BindAction::Reference);
        assert_eq!(browse(&binds, true), "");
    }

    /// Help names the values chord and the docs chord on two rows, with
    /// Ctrl+Space beside Ctrl+F. Each row follows its own action.
    #[test]
    fn help_names_the_values_chord_and_the_docs_chord_apart() {
        use super::super::keybinds::KeyCombo;
        let keys_for = |binds: &Keybinds, action: &str| {
            shortcut_rows_with_keybinds(KeyboardCapabilities::legacy(), binds)
                .into_iter()
                .find(|row| row.action == action)
                .unwrap_or_else(|| panic!("help contains {action:?}"))
                .keys
        };
        let values = "values for the argument at the caret; elsewhere the reference";
        let docs = "docs for the function at the caret";
        let mut binds = Keybinds::default();
        assert_eq!(keys_for(&binds, values), "^F / ^Space");
        assert_eq!(keys_for(&binds, docs), "^D");
        let editor = HelpContext::Editor.live_bindings(&binds);
        assert_eq!(editor[2].0, "^F / ^D");
        assert_eq!(editor[2].1, "argument values / function docs");

        binds.learn(BindAction::Docs, KeyCombo::parse("f3"));
        assert_eq!(keys_for(&binds, values), "^F / ^Space");
        assert_eq!(keys_for(&binds, docs), "F3");
        assert_eq!(HelpContext::Editor.live_bindings(&binds)[2].0, "^F / F3");
        binds.unbind(BindAction::Docs);
        assert_eq!(
            HelpContext::Editor.live_bindings(&binds)[2],
            ("^F".to_owned(), "argument values")
        );
    }

    #[test]
    fn context_and_transport_guidance_stay_pinned_when_the_list_scrolls() {
        let theme = Theme::built_in_default();
        let keybinds = Keybinds::default();
        let area = Rect::new(0, 0, 90, 20);
        let mut state = HelpState::default();
        state.scroll_key(KeyCode::End, area, KeyboardCapabilities::legacy());
        let mut buffer = Buffer::empty(area);
        HelpView {
            state,
            context: HelpContext::ThemePicker,
            capabilities: KeyboardCapabilities::legacy(),
            keybinds: &keybinds,
            theme: &theme,
        }
        .render(area, &mut buffer);
        let text = text_of(&buffer);

        assert!(text.contains("theme picker"), "{text}");
        assert!(text.contains("Moving the selection previews"), "{text}");
        assert!(text.contains("type · Backspace"), "{text}");
        assert!(text.contains("Left/Right · Up/Down"), "{text}");
        assert!(text.contains("^Enter/F5 · ^./F8"), "{text}");
        assert!(
            !text.contains("Ctrl+") && !text.contains('\u{2318}'),
            "one convention in help too: {text}"
        );
        assert!(text.contains("write edited scenes and quit"), "{text}");
        assert!(
            !text.contains("open the menu bar; Help opens keyboard reference"),
            "the broad list really moved: {text}"
        );
    }

    #[test]
    fn every_supported_scroll_key_clamps_to_the_list() {
        let area = Rect::new(0, 0, 80, 18);
        let capabilities = KeyboardCapabilities::legacy();
        let mut state = HelpState::default();
        assert!(state.scroll_key(KeyCode::Down, area, capabilities));
        assert_eq!(state.scroll, 1);
        assert!(state.scroll_key(KeyCode::PageDown, area, capabilities));
        assert!(state.scroll > 1);
        assert!(state.scroll_key(KeyCode::End, area, capabilities));
        let end = state.scroll;
        state.wheel(false, area, capabilities);
        assert_eq!(state.scroll, end);
        state.wheel(true, area, capabilities);
        assert!(state.scroll < end);
        assert!(state.scroll_key(KeyCode::Home, area, capabilities));
        assert_eq!(state.scroll, 0);
        assert!(!state.scroll_key(KeyCode::Char('x'), area, capabilities));
    }

    #[test]
    fn theme_editor_submodals_name_the_keys_they_actually_accept() {
        let theme = Theme::built_in_default();
        let keybinds = Keybinds::default();
        let area = Rect::new(0, 0, 90, 20);
        for (context, needle) in [
            (HelpContext::ThemeEditorForm, "s · Tab · Esc"),
            (HelpContext::ThemeEditorCode, "s on the Form tab"),
            (HelpContext::ThemeEditorColor, "#rrggbb · Backspace"),
            (HelpContext::ThemeEditorSave, "choose save or save & keep"),
        ] {
            let mut buffer = Buffer::empty(area);
            HelpView {
                state: HelpState::default(),
                context,
                capabilities: KeyboardCapabilities::legacy(),
                keybinds: &keybinds,
                theme: &theme,
            }
            .render(area, &mut buffer);
            assert!(text_of(&buffer).contains(needle), "{context:?}");
        }
    }

    #[test]
    fn the_global_list_does_not_advertise_the_unimplemented_find_prompt() {
        for capabilities in [
            KeyboardCapabilities::legacy(),
            KeyboardCapabilities::enhanced(),
        ] {
            assert!(
                shortcut_rows(capabilities)
                    .iter()
                    .all(|row| !row.action.contains("find in"))
            );
        }
    }

    #[test]
    fn shortcut_help_says_when_the_terminal_does_not_send_shift_home_and_end() {
        use super::super::keybinds::Reach;
        use super::super::terminal::conflicts::{ForceDesktopForTest, ForcePlatformForTest};

        let _platform = ForcePlatformForTest::set("linux");
        let _desktop = ForceDesktopForTest::set("");
        for (terminal, sent) in [("kitty", true), ("Ghostty", false)] {
            let mut keybinds = Keybinds::default();
            keybinds.set_reach(Reach {
                enhanced: true,
                terminal: terminal.to_owned(),
            });
            let rows = shortcut_rows_with_keybinds(KeyboardCapabilities::enhanced(), &keybinds);
            let row = rows
                .iter()
                .find(|row| row.keys.ends_with("Home/End"))
                .expect("help has the row");
            assert_eq!(row.action.starts_with("select"), sent, "{terminal}");
        }
    }

    #[test]
    fn shortcut_help_reads_terminal_fallbacks_and_learnt_bindings() {
        use super::super::keybinds::{KeyCombo, Reach};

        let mut keybinds = Keybinds::default();
        keybinds.set_reach(Reach {
            enhanced: false,
            terminal: "conhost".to_owned(),
        });
        let rows = shortcut_rows_with_keybinds(KeyboardCapabilities::legacy(), &keybinds);
        let keys_for = |action: &str| {
            rows.iter()
                .find(|row| row.action.starts_with(action))
                .unwrap_or_else(|| panic!("help contains {action:?}"))
                .keys
                .as_str()
        };
        assert_eq!(keys_for("rewind update"), "⇧F5");
        assert_eq!(keys_for("redo"), "^Y");
        assert_eq!(keys_for("open another set"), "⇧F6");

        keybinds.learn(
            BindAction::RewindEvaluate,
            Some(KeyCombo::parse("f2").expect("F2 parses")),
        );
        let rows = shortcut_rows_with_keybinds(KeyboardCapabilities::legacy(), &keybinds);
        assert_eq!(
            rows.iter()
                .find(|row| row.action.starts_with("rewind update"))
                .expect("rewind row")
                .keys,
            "F2"
        );
    }

    #[test]
    fn context_cards_retire_rebound_and_unbound_chords() {
        use super::super::keybinds::{KeyCombo, Reach};
        let mut binds = Keybinds::default();
        binds.set_reach(Reach {
            enhanced: false,
            terminal: "conhost".to_owned(),
        });
        assert_eq!(HelpContext::SceneLearn.live_bindings(&binds)[2].0, "⇧F8");
        binds.learn(
            BindAction::Evaluate,
            Some(KeyCombo::parse("ctrl+y").unwrap()),
        );
        binds.unbind(BindAction::Stop);
        binds.unbind(BindAction::Reference);
        assert_eq!(
            HelpContext::Editor.live_bindings(&binds)[2],
            ("^D".to_owned(), "function docs")
        );
        binds.unbind(BindAction::Docs);
        let editor = HelpContext::Editor.live_bindings(&binds);
        assert_eq!(editor[0].0, "^Y");
        assert_eq!(editor[1].0, "");
        assert_eq!(editor[2].0, "");
        binds.unbind(BindAction::Mixer);
        assert_eq!(
            HelpContext::Mixer.live_bindings(&binds)[2].0,
            "e · +/- · Esc"
        );
        binds.unbind(BindAction::CloseScene);
        assert_eq!(
            HelpContext::Timeline.live_bindings(&binds)[2].0,
            "T · Delete · Esc"
        );
    }

    #[test]
    fn help_hides_blocked_and_shadowed_aliases() {
        use super::super::keybinds::{KeyCombo, Reach};
        let mut binds = Keybinds::default();
        binds.set_reach(Reach {
            enhanced: true,
            terminal: "Windows Terminal".to_owned(),
        });
        assert_eq!(shortcut_with_alias(&binds, BindAction::Log), "F9");
        assert_eq!(shortcut_with_alias(&binds, BindAction::Mixer), "F4");
        binds.learn(BindAction::Stop, Some(KeyCombo::parse("f5").unwrap()));
        assert_eq!(shortcut_with_alias(&binds, BindAction::Evaluate), "^Enter");
        assert_eq!(shortcut_with_alias(&binds, BindAction::Stop), "F5");
        binds.set_reach(Reach {
            enhanced: false,
            terminal: "conhost".to_owned(),
        });
        assert_eq!(shortcut_with_alias(&binds, BindAction::Redo), "^Y");
    }

    #[test]
    fn scene_help_names_the_split_scene_chords() {
        let binds = Keybinds::default();
        let rows = shortcut_rows_with_keybinds(KeyboardCapabilities::enhanced(), &binds);
        let scene_keys = |prefix: &str| {
            rows.iter()
                .find(|row| row.action.starts_with(prefix))
                .unwrap()
                .keys
                .as_str()
        };
        assert_eq!(scene_keys("one pane:"), "^[ · ^]");
        assert_eq!(scene_keys("split: cycle"), "^⇧[ · ^⇧]");
    }
}
