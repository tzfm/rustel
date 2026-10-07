//! The menu bar: seven menus on a row of their own above the header.
//!
//! Turbo Vision's arrangement, for Turbo Vision's reason. A bar is about
//! forty columns however many actions hide behind it, where the footer's
//! shortcut strip wants two hundred and three and never gets them - it starts
//! only after the device and MIDI chips have taken their share of the row. A
//! menu also teaches its own accelerator as a side effect of being used,
//! which a strip squeezed off the right-hand end of the footer cannot do.
//!
//! **No modifier is involved in opening or working it.** F1 or a click
//! focuses the bar; once focused, the mnemonics are unmodified letters.
//! Alt+letter is the usual spelling and is exactly wrong here: on macOS
//! Option is text composition (Option+F is `ƒ`) and both iTerm2 and Ghostty
//! send Option as Meta by default, so Alt-mnemonics would have recreated the
//! dead-chord bug that caret notation was introduced to kill. F10, the other
//! convention, is Mission Control.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Modifier, Style},
};
use unicode_width::UnicodeWidthStr;

use super::theme::Theme;

/// Cells of padding either side of a title on the bar.
const TITLE_PAD: u16 = 2;
/// Columns between an item's label and its accelerator.
const ACCEL_GAP: u16 = 3;

/// What choosing an item does. One variant per action rather than a boxed
/// closure, so the menu model stays comparable, printable and testable, and
/// the application keeps every side effect in one dispatcher.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MenuAction {
    /// The mixer along the bottom.
    Mixer,
    // File - the set is the file: a folder of scores.
    NewSet,
    NewSession,
    OpenSet,
    OpenRecent,
    RenameSet,
    /// Copy the samples this set's scores name into the set's own folder,
    /// so the folder plays on its own wherever it goes.
    ConsolidateSamples,
    ShowSet,
    Quit,
    // Scene
    NewScene,
    DuplicateScene,
    RenameScene,
    CloseScene,
    DeleteScene,
    PreviousScene,
    NextScene,
    LearnPad,
    ForgetPad,
    // Edit
    Undo,
    Redo,
    Cut,
    Copy,
    Paste,
    SelectAll,
    ToggleComment,
    FirstError,
    /// The caret's number, turned into a fader.
    SmartAction,
    // Transport
    Update,
    /// Update, with the score started from its own cycle zero, whether
    /// or not the scene is flagged for it.
    RewindUpdate,
    /// Flip whether this scene rewinds when it is played.
    SceneRewind,
    Stop,
    Record,
    /// Record a sample from the audio input, or finish the one recording.
    RecordSample,
    Export,
    ShowLastFile,
    MasterUp,
    MasterDown,
    /// Give this set a limiter on its desk, or take away the one it has.
    /// The slot; what it holds is the mixer strip's business.
    SetLimiter,
    // View
    Reference,
    Docs,
    PianoMode,
    SetPanel,
    VizPanel,
    VizPanelTwo,
    Split,
    HopPane,
    SwitchPanel,
    Log,
    Jobs,
    /// The memory breakdown, docked along the bottom or the top.
    Memory,
    Zen,
    LineNumbers,
    Wrap,
    // Options
    Settings,
    Devices,
    RemoteControl,
    ThemePicker,
    ShowMenu,
    ShowHeader,
    ShowFooter,
    // Help
    KeyboardReference,
    About,
}

/// What File > Show set says on this desk: the file manager by the name
/// the platform gives it.
pub fn show_set_label() -> &'static str {
    if cfg!(target_os = "macos") {
        "Show set in Finder"
    } else if cfg!(windows) {
        "Show set in Explorer"
    } else {
        "Show set in file manager"
    }
}

/// The shape of a row, and the live state it draws from.
///
/// `Toggle`, `Radio`, `Stepper` and a previewing `Submenu` are unused by the
/// menus slice 1 builds, and are here because the widget's contract has to be
/// settled before the surfaces that need them are folded in: a settings row
/// that steps a value and a theme list that previews on highlight cannot be
/// bolted onto an activate-only menu afterwards without rewriting it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MenuItemKind {
    Action(MenuAction),
    Toggle {
        id: MenuAction,
        on: bool,
    },
    Radio {
        id: MenuAction,
        selected: bool,
    },
    Stepper {
        id: MenuAction,
        value: String,
    },
    Submenu {
        id: MenuAction,
        items: Vec<MenuItem>,
        /// Whether merely highlighting a child changes the studio - the
        /// theme list, whose live preview is its best feature. Drives
        /// [`MenuEvent::PreviewBegin`] and [`MenuEvent::PreviewEnd`].
        previews: bool,
    },
    Separator,
}

/// One row of a dropdown.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MenuItem {
    pub kind: MenuItemKind,
    pub label: String,
    /// Byte offset into `label` of the mnemonic letter: what an unmodified
    /// press chooses, and the character the dropdown underlines.
    ///
    /// An offset rather than a `char` because the letter is not always the
    /// first one - Edit alone holds Cut, Copy and Comment, and Scene holds
    /// New beside Next - so the mnemonic has to be able to name a letter in
    /// the middle of a word, and the renderer has to know which one to mark.
    pub mnemonic_at: Option<usize>,
    /// The chord, already spelled by `KeyboardCapabilities`. Never a ⌘ form.
    pub accel: String,
    pub enabled: bool,
}

impl MenuItem {
    /// A plain command. `mnemonic` is matched case-insensitively against
    /// `label`; a letter that does not occur in the label is dropped rather
    /// than silently mismatching what the renderer underlines.
    pub fn action(id: MenuAction, label: &str, mnemonic: char, accel: impl Into<String>) -> Self {
        Self::new(MenuItemKind::Action(id), label, mnemonic, accel)
    }

    pub fn new(kind: MenuItemKind, label: &str, mnemonic: char, accel: impl Into<String>) -> Self {
        Self {
            kind,
            mnemonic_at: mnemonic_offset(label, mnemonic),
            label: label.to_owned(),
            accel: accel.into(),
            enabled: true,
        }
    }

    pub fn separator() -> Self {
        Self {
            kind: MenuItemKind::Separator,
            label: String::new(),
            mnemonic_at: None,
            accel: String::new(),
            enabled: false,
        }
    }

    /// Grey the row out. A disabled row still draws - a menu that hides what
    /// it cannot do right now teaches nothing about what the studio can do.
    #[must_use]
    pub fn enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }

    pub fn id(&self) -> Option<MenuAction> {
        match &self.kind {
            MenuItemKind::Action(id)
            | MenuItemKind::Toggle { id, .. }
            | MenuItemKind::Radio { id, .. }
            | MenuItemKind::Stepper { id, .. }
            | MenuItemKind::Submenu { id, .. } => Some(*id),
            MenuItemKind::Separator => None,
        }
    }

    /// Whether the highlight may rest here. A separator never takes it; a
    /// disabled row does not either, so arrowing past a run of unavailable
    /// commands does not stall.
    pub fn selectable(&self) -> bool {
        !matches!(self.kind, MenuItemKind::Separator) && self.enabled
    }

    /// The mnemonic letter, folded to lower case: `Shift+S` and `s` are the
    /// same choice, and the label keeps whatever capital it was written with.
    pub fn mnemonic(&self) -> Option<char> {
        let at = self.mnemonic_at?;
        Some(self.label[at..].chars().next()?.to_ascii_lowercase())
    }

    fn submenu(&self) -> Option<(&[MenuItem], bool)> {
        match &self.kind {
            MenuItemKind::Submenu {
                items, previews, ..
            } => Some((items, *previews)),
            _ => None,
        }
    }

    /// What is drawn to the right of the label: the accelerator, or a
    /// stepper's value, or the arrow that says a submenu opens here.
    fn trailer(&self) -> &str {
        match &self.kind {
            MenuItemKind::Stepper { value, .. } => value,
            MenuItemKind::Submenu { .. } => "\u{25b8}",
            _ => &self.accel,
        }
    }

    /// The mark in the left margin: a tick for a switch that is on, a dot for
    /// the live one of a set.
    fn mark(&self) -> &'static str {
        match &self.kind {
            MenuItemKind::Toggle { on: true, .. } => "\u{2713}",
            MenuItemKind::Radio { selected: true, .. } => "\u{25cf}",
            _ => " ",
        }
    }
}

/// One menu, and its title on the bar.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Menu {
    pub title: String,
    pub mnemonic_at: Option<usize>,
    pub items: Vec<MenuItem>,
}

impl Menu {
    pub fn new(title: &str, mnemonic: char, items: Vec<MenuItem>) -> Self {
        Self {
            mnemonic_at: mnemonic_offset(title, mnemonic),
            title: title.to_owned(),
            items,
        }
    }

    /// Folded to lower case, like [`MenuItem::mnemonic`].
    pub fn mnemonic(&self) -> Option<char> {
        let at = self.mnemonic_at?;
        Some(self.title[at..].chars().next()?.to_ascii_lowercase())
    }
}

/// The byte offset of `mnemonic` in `label`, matched without case.
fn mnemonic_offset(label: &str, mnemonic: char) -> Option<usize> {
    let wanted = mnemonic.to_ascii_lowercase();
    label
        .char_indices()
        .find(|(_, letter)| letter.to_ascii_lowercase() == wanted)
        .map(|(at, _)| at)
}

/// What the application must do about a keypress or click. Fired in order.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MenuEvent {
    /// The highlight moved. `None` says nothing is highlighted any more,
    /// which is the state a live preview has to be told about - arrowing off
    /// a theme and onto a separator must put the old theme back, and an
    /// event that can only name a new item cannot say that.
    Highlight(Option<MenuAction>),
    /// The highlight entered a previewing submenu: capture what to restore,
    /// once, now. Emitted on entry only, never on movement inside, so the
    /// captured original is what the musician had before they went looking.
    PreviewBegin(MenuAction),
    /// The highlight left it. `restore` distinguishes backing out (put it
    /// back) from choosing (keep it).
    PreviewEnd {
        restore: bool,
    },
    Activate(MenuAction),
    /// Left or Right on a stepper or a radio row.
    Adjust(MenuAction, i8),
    Close(CloseReason),
}

/// Why a menu closed. A bare `Close` cannot serve: Esc has to restore a
/// previewed theme, Enter has to keep it and write the preference to disk,
/// and a click on another title is not a close at all but must still undo the
/// preview before the next menu opens.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CloseReason {
    /// Esc, F1 again, or a click away: undo anything a preview did.
    Cancel,
    /// A row was chosen. Keep what the preview did.
    Committed,
    /// The bar stays focused, another title is opening.
    Switched,
}

/// Whether the menu wanted the key at all.
///
/// Three states, not two. `Option<MenuEvent>` cannot separate "I swallowed
/// that and there is nothing to do" from "that was not mine" - and with a
/// dropdown down every unmatched letter must be swallowed, or it is typed
/// into the score underneath. The rest of the studio already draws this
/// distinction (`SettingsAction::Ignored`, `dispatch_panel_key -> bool`).
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MenuKey {
    /// Not the menu's. The rest of the dispatch should see it.
    Ignored,
    /// Swallowed; nothing for the application to do.
    Consumed,
    /// Swallowed; act on these, in order.
    Events(Vec<MenuEvent>),
}

impl MenuKey {
    fn of(events: Vec<MenuEvent>) -> Self {
        if events.is_empty() {
            Self::Consumed
        } else {
            Self::Events(events)
        }
    }
}

/// The highlight at one level of nesting, and how far that level is scrolled.
/// Scroll is per level because a submenu of thirty themes scrolls while the
/// menu that owns it does not.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct Level {
    index: usize,
    scroll: usize,
}

/// Which menu is down, and where the highlight sits inside it.
///
/// Deliberately its own `Option<MenuState>` field on the application rather
/// than a `Focus`/`PanelKind` variant: `settle_focus` forces focus back to
/// the theme editor whenever that sheet is up, and would take the keyboard
/// away from the menu on the very next key. `help` is held the same way, for
/// the same reason.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MenuState {
    /// The selected title on the bar.
    open: usize,
    /// The highlight, one entry per open level. Empty means the bar has the
    /// focus but nothing is dropped down yet - what F1 alone leaves, so that
    /// F1 twice is cheap and closes again.
    path: Vec<Level>,
}

impl MenuState {
    /// F1: the bar takes the keyboard, with nothing dropped.
    pub fn opened() -> Self {
        Self {
            open: 0,
            path: Vec::new(),
        }
    }

    /// A click straight onto a title: that menu, dropped on its first row
    /// that can actually be chosen. Row zero is a greyed Undo the moment
    /// there is nothing to undo.
    pub fn dropped_on(menus: &[Menu], title: usize) -> Self {
        let index = menus
            .get(title)
            .map_or(0, |menu| first_selectable(&menu.items));
        Self {
            open: title,
            path: vec![Level { index, scroll: 0 }],
        }
    }

    pub fn open_title(&self) -> usize {
        self.open
    }

    pub fn is_dropped(&self) -> bool {
        !self.path.is_empty()
    }

    /// The highlight's position, level by level, for the renderer.
    pub fn highlight_path(&self) -> Vec<usize> {
        self.path.iter().map(|level| level.index).collect()
    }

    pub fn scroll_at(&self, depth: usize) -> usize {
        self.path.get(depth).map_or(0, |level| level.scroll)
    }

    /// The item list at `depth`, walking the open submenus.
    fn items_at<'a>(&self, menus: &'a [Menu], depth: usize) -> Option<&'a [MenuItem]> {
        let mut items = menus.get(self.open)?.items.as_slice();
        for level in self.path.iter().take(depth) {
            let (nested, _) = items.get(level.index)?.submenu()?;
            items = nested;
        }
        Some(items)
    }

    /// The deepest open list, and the highlighted item in it.
    fn current<'a>(&self, menus: &'a [Menu]) -> Option<(&'a [MenuItem], usize)> {
        let depth = self.path.len().checked_sub(1)?;
        let items = self.items_at(menus, depth)?;
        Some((items, self.path[depth].index))
    }

    fn highlighted<'a>(&self, menus: &'a [Menu]) -> Option<&'a MenuItem> {
        let (items, index) = self.current(menus)?;
        items.get(index)
    }

    fn highlighted_action(&self, menus: &[Menu]) -> Option<MenuAction> {
        self.highlighted(menus).and_then(MenuItem::id)
    }

    /// Whether the level at `depth` sits inside a submenu that previews.
    fn previewing_scope(&self, menus: &[Menu]) -> Option<MenuAction> {
        let depth = self.path.len().checked_sub(1)?;
        if depth == 0 {
            return None;
        }
        let parent = self
            .items_at(menus, depth - 1)?
            .get(self.path[depth - 1].index)?;
        parent
            .submenu()
            .and_then(|(_, previews)| previews.then(|| parent.id()).flatten())
    }
}

/// The next selectable row in `delta`'s direction, wrapping. Separators and
/// greyed rows are stepped over rather than rested on.
fn step(items: &[MenuItem], from: usize, delta: isize) -> usize {
    if items.is_empty() {
        return 0;
    }
    let count = items.len();
    let mut at = from;
    for _ in 0..count {
        at = ((at as isize + delta).rem_euclid(count as isize)) as usize;
        if items[at].selectable() {
            return at;
        }
    }
    from
}

fn first_selectable(items: &[MenuItem]) -> usize {
    items.iter().position(MenuItem::selectable).unwrap_or(0)
}

fn last_selectable(items: &[MenuItem]) -> usize {
    items.iter().rposition(MenuItem::selectable).unwrap_or(0)
}

impl MenuState {
    /// Where the highlight is, for diffing a move: the previewing scope it
    /// sits in, and its position.
    ///
    /// Position, not the action it names. Two rows may carry the same action -
    /// a list of thirty themes does by construction - and diffing by action
    /// would report no movement between them, silently losing exactly the
    /// live preview the events exist to drive.
    fn mark_of(&self, menus: &[Menu]) -> (Option<MenuAction>, usize, Vec<usize>) {
        (
            self.previewing_scope(menus),
            self.open,
            self.highlight_path(),
        )
    }

    /// Turn a before/after pair into the events the move implies.
    fn moved(
        &self,
        menus: &[Menu],
        before: (Option<MenuAction>, usize, Vec<usize>),
        events: &mut Vec<MenuEvent>,
    ) {
        let after = self.mark_of(menus);
        if before.0 != after.0 {
            if before.0.is_some() {
                events.push(MenuEvent::PreviewEnd { restore: true });
            }
            if let Some(scope) = after.0 {
                events.push(MenuEvent::PreviewBegin(scope));
            }
        }
        if (before.1, &before.2) != (after.1, &after.2) {
            events.push(MenuEvent::Highlight(self.highlighted_action(menus)));
        }
    }

    /// Close, undoing a preview if one is live.
    fn close(&self, menus: &[Menu], reason: CloseReason, events: &mut Vec<MenuEvent>) {
        if self.previewing_scope(menus).is_some() {
            events.push(MenuEvent::PreviewEnd {
                restore: reason != CloseReason::Committed,
            });
        }
        events.push(MenuEvent::Close(reason));
    }

    /// Choose the highlighted row: descend into a submenu, or fire it.
    fn choose(&mut self, menus: &[Menu], events: &mut Vec<MenuEvent>) {
        let before = self.mark_of(menus);
        let Some(item) = self.highlighted(menus) else {
            return;
        };
        if let Some((nested, _)) = item.submenu() {
            let index = first_selectable(nested);
            self.path.push(Level { index, scroll: 0 });
            self.moved(menus, before, events);
            return;
        }
        let Some(id) = item.id() else { return };
        match item.kind {
            MenuItemKind::Toggle { .. } => {
                events.push(MenuEvent::Activate(id));
                self.close(menus, CloseReason::Committed, events);
            }
            MenuItemKind::Stepper { .. } => events.push(MenuEvent::Adjust(id, 1)),
            _ => {
                events.push(MenuEvent::Activate(id));
                self.close(menus, CloseReason::Committed, events);
            }
        }
    }

    /// Page through the open dropdown using the rows it actually shows.
    pub fn key_with_area(
        &mut self,
        key: &KeyEvent,
        menus: &[Menu],
        bar: Rect,
        screen: Rect,
    ) -> MenuKey {
        if !matches!(key.code, KeyCode::PageUp | KeyCode::PageDown)
            || key
                .modifiers
                .intersects(KeyModifiers::CONTROL | KeyModifiers::SUPER | KeyModifiers::ALT)
            || menus.is_empty()
        {
            return self.key(key, menus);
        }
        let forwards = key.code == KeyCode::PageDown;
        if self.path.is_empty() {
            // A page key on the bar opens the chosen list, as an arrow does.
            let key = KeyEvent {
                code: if forwards { KeyCode::Down } else { KeyCode::Up },
                ..*key
            };
            return self.key(&key, menus);
        }
        let shown = dropdown_rects(menus, self, bar, screen)
            .last()
            .copied()
            .map_or(1, visible_rows)
            .max(1);
        let before = self.mark_of(menus);
        let mut events = Vec::new();
        if let Some((items, index)) = self.current(menus) {
            let target = if forwards {
                index
                    .saturating_add(shown)
                    .min(items.len().saturating_sub(1))
            } else {
                index.saturating_sub(shown)
            };
            // Count screen rows, then step past headings and unavailable
            // commands in the same direction. Stop at either end; no wrap.
            let selectable = |row: &usize| items.get(*row).is_some_and(MenuItem::selectable);
            let next = if forwards {
                (target..items.len())
                    .find(selectable)
                    .or_else(|| (0..target).rev().find(selectable))
            } else {
                (0..=target)
                    .rev()
                    .find(selectable)
                    .or_else(|| (target.saturating_add(1)..items.len()).find(selectable))
            };
            if let Some(next) = next {
                let depth = self.path.len() - 1;
                self.path[depth].index = next;
                self.moved(menus, before, &mut events);
            }
        }
        MenuKey::of(events)
    }

    /// A keypress while the bar has the keyboard.
    ///
    /// Takes the whole event, not a bare `KeyCode`: the mnemonics are
    /// unmodified letters and the chords the items mirror are the same
    /// letters with Control held, so `^N` must fall through to the chord
    /// table rather than firing the New mnemonic.
    pub fn key(&mut self, key: &KeyEvent, menus: &[Menu]) -> MenuKey {
        if menus.is_empty() {
            return MenuKey::Ignored;
        }
        // A modified key is never the menu's. Shift is not a modifier here -
        // it is how an uppercase mnemonic arrives.
        if key
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::SUPER | KeyModifiers::ALT)
        {
            return MenuKey::Ignored;
        }
        let before = self.mark_of(menus);
        let mut events = Vec::new();
        match key.code {
            // Opening and toggling use the live keybinding table in App.
            // The widget must not reserve F1 after that key is rebound.
            KeyCode::F(1) => return MenuKey::Ignored,
            KeyCode::Esc => {
                self.close(menus, CloseReason::Cancel, &mut events);
            }
            KeyCode::Down => {
                if self.path.is_empty() {
                    let items = &menus[self.open].items;
                    self.path.push(Level {
                        index: first_selectable(items),
                        scroll: 0,
                    });
                } else if let Some((items, index)) = self.current(menus) {
                    let next = step(items, index, 1);
                    let depth = self.path.len() - 1;
                    self.path[depth].index = next;
                }
                self.moved(menus, before, &mut events);
            }
            KeyCode::Up => {
                if self.path.is_empty() {
                    let items = &menus[self.open].items;
                    self.path.push(Level {
                        index: last_selectable(items),
                        scroll: 0,
                    });
                } else if let Some((items, index)) = self.current(menus) {
                    let next = step(items, index, -1);
                    let depth = self.path.len() - 1;
                    self.path[depth].index = next;
                }
                self.moved(menus, before, &mut events);
            }
            KeyCode::Home | KeyCode::End if !self.path.is_empty() => {
                if let Some((items, _)) = self.current(menus) {
                    let index = if key.code == KeyCode::Home {
                        first_selectable(items)
                    } else {
                        last_selectable(items)
                    };
                    let depth = self.path.len() - 1;
                    self.path[depth].index = index;
                }
                self.moved(menus, before, &mut events);
            }
            KeyCode::Left | KeyCode::Right => {
                let delta = if key.code == KeyCode::Left { -1 } else { 1 };
                let adjustable = self.highlighted(menus).is_some_and(|item| {
                    matches!(
                        item.kind,
                        MenuItemKind::Stepper { .. } | MenuItemKind::Radio { .. }
                    )
                });
                if adjustable {
                    // On a value, the arrows are the value's, not the bar's.
                    if let Some(id) = self.highlighted_action(menus) {
                        events.push(MenuEvent::Adjust(id, delta as i8));
                    }
                } else if delta < 0 && self.path.len() > 1 {
                    self.path.pop();
                    self.moved(menus, before, &mut events);
                } else if delta > 0
                    && self
                        .highlighted(menus)
                        .and_then(MenuItem::submenu)
                        .is_some()
                {
                    self.choose(menus, &mut events);
                } else {
                    // Along the bar. A dropped menu stays dropped, which is
                    // what makes browsing the menus one keypress each.
                    let dropped = self.is_dropped();
                    let count = menus.len() as isize;
                    self.open = ((self.open as isize + delta).rem_euclid(count)) as usize;
                    self.path.clear();
                    if dropped {
                        let items = &menus[self.open].items;
                        self.path.push(Level {
                            index: first_selectable(items),
                            scroll: 0,
                        });
                    }
                    self.moved(menus, before, &mut events);
                }
            }
            KeyCode::Enter => {
                if self.path.is_empty() {
                    let items = &menus[self.open].items;
                    self.path.push(Level {
                        index: first_selectable(items),
                        scroll: 0,
                    });
                    self.moved(menus, before, &mut events);
                } else {
                    self.choose(menus, &mut events);
                }
            }
            KeyCode::Char(letter) => {
                let wanted = letter.to_ascii_lowercase();
                if self.path.is_empty() {
                    // On the bar: a title mnemonic opens that menu, dropped.
                    if let Some(index) = menus
                        .iter()
                        .position(|menu| menu.mnemonic() == Some(wanted))
                    {
                        self.open = index;
                        let items = &menus[index].items;
                        self.path.push(Level {
                            index: first_selectable(items),
                            scroll: 0,
                        });
                        self.moved(menus, before, &mut events);
                    }
                } else if let Some((items, _)) = self.current(menus)
                    && let Some(index) = items
                        .iter()
                        .position(|item| item.selectable() && item.mnemonic() == Some(wanted))
                {
                    let depth = self.path.len() - 1;
                    self.path[depth].index = index;
                    self.moved(menus, before, &mut events);
                    self.choose(menus, &mut events);
                }
            }
            _ => {}
        }
        // Anything else is swallowed. With a dropdown down, a letter that is
        // not a mnemonic must not reach the score underneath it.
        MenuKey::of(events)
    }
}

/// Where each title sits on the bar. One function, used by the renderer and
/// by hit-testing, so a click can never land on a title the bar did not draw.
pub fn title_rects(menus: &[Menu], bar: Rect) -> Vec<Rect> {
    let mut rects = Vec::with_capacity(menus.len());
    let mut x = bar.x.saturating_add(1);
    for menu in menus {
        let width = UnicodeWidthStr::width(menu.title.as_str()) as u16 + TITLE_PAD;
        if x >= bar.right() {
            rects.push(Rect::new(bar.right(), bar.y, 0, 0));
            continue;
        }
        rects.push(Rect::new(x, bar.y, width.min(bar.right() - x), 1));
        x = x.saturating_add(width);
    }
    rects
}

/// The widest row a list needs, label plus whatever trails it.
fn list_width(items: &[MenuItem]) -> u16 {
    items
        .iter()
        .map(|item| {
            let label = UnicodeWidthStr::width(item.label.as_str()) as u16;
            let trailer = UnicodeWidthStr::width(item.trailer()) as u16;
            // highlight arrow, mark, label, space, gap, trailer
            3 + label + if trailer > 0 { ACCEL_GAP + trailer } else { 0 }
        })
        .max()
        .unwrap_or(0)
}

/// The panel for each open level, outermost first. Each nested list hangs off
/// its parent's highlighted row, and every one is clamped into `screen`.
pub fn dropdown_rects(menus: &[Menu], state: &MenuState, bar: Rect, screen: Rect) -> Vec<Rect> {
    let mut rects = Vec::new();
    if !state.is_dropped() || screen.is_empty() {
        return rects;
    }
    let titles = title_rects(menus, bar);
    let mut anchor_x = titles.get(state.open).map_or(bar.x, |rect| rect.x);
    let mut anchor_y = bar.y.saturating_add(1);
    for depth in 0..state.path.len() {
        let Some(items) = state.items_at(menus, depth) else {
            break;
        };
        if items.is_empty() {
            break;
        }
        let width = (list_width(items) + 2).min(screen.width);
        let room = screen.bottom().saturating_sub(anchor_y);
        let height = (items.len() as u16 + 2).min(room.max(3));
        let x = anchor_x.min(screen.right().saturating_sub(width));
        let y = anchor_y.min(screen.bottom().saturating_sub(height));
        let rect = Rect::new(x, y, width, height);
        rects.push(rect);
        // The next level opens beside the row that owns it.
        let index = state.path[depth].index;
        let scroll = state.path[depth].scroll;
        anchor_x = rect.right().saturating_sub(1);
        anchor_y = rect.y + 1 + (index.saturating_sub(scroll)) as u16;
    }
    rects
}

/// The rows a list actually shows, given its panel and scroll.
pub fn visible_rows(rect: Rect) -> usize {
    usize::from(rect.height.saturating_sub(2))
}

impl MenuState {
    /// Keep every level's highlight on screen. The application calls this
    /// after a key, a click or a hover, once it knows the frame, because the
    /// panel's height is what decides how far a thirty-theme list has to
    /// scroll.
    ///
    /// After a key (`from_keys`) the list keeps a margin of rows around the
    /// highlight, so the next rows are in sight before they are reached; see
    /// [`super::scroll`]. The pointer keeps none: a row hovered near the
    /// edge must not scroll the list under a pointer that has not moved.
    pub fn follow_scroll(&mut self, menus: &[Menu], bar: Rect, screen: Rect, from_keys: bool) {
        let rects = dropdown_rects(menus, self, bar, screen);
        // Only the level the keys walk keeps a margin. The levels it hangs
        // from stay put, or their rows would move under a pointer that
        // opened the submenu, and the next move of it would close it.
        let walked = self.path.len().saturating_sub(1);
        for (depth, rect) in rects.iter().enumerate() {
            let shown = visible_rows(*rect);
            if shown == 0 {
                continue;
            }
            let items = self.items_at(menus, depth).unwrap_or(&[]);
            let Some(level) = self.path.get_mut(depth) else {
                continue;
            };
            let margin = if from_keys && depth == walked {
                super::scroll::margin(shown)
            } else {
                0
            };
            // Counted in rows the highlight can rest on: a separator, or a
            // greyed command, does not use the margin up.
            level.scroll = super::scroll::follow_choices(
                level.scroll,
                level.index,
                shown,
                items.len(),
                margin,
                |row| items.get(row).is_some_and(MenuItem::selectable),
            );
        }
    }

    /// The level and row under a point, if the pointer is over a dropdown.
    pub fn item_at(
        &self,
        menus: &[Menu],
        bar: Rect,
        screen: Rect,
        x: u16,
        y: u16,
    ) -> Option<(usize, usize)> {
        let rects = dropdown_rects(menus, self, bar, screen);
        // Innermost first: a nested panel overlaps the one that opened it.
        for (depth, rect) in rects.iter().enumerate().rev() {
            if x < rect.x || x >= rect.right() || y < rect.y + 1 || y + 1 >= rect.bottom() {
                continue;
            }
            let row = usize::from(y - rect.y - 1) + self.scroll_at(depth);
            let items = self.items_at(menus, depth)?;
            if row < items.len() && items[row].selectable() {
                return Some((depth, row));
            }
            // Over the panel but not on a usable row: still the menu's.
            return Some((depth, usize::MAX));
        }
        None
    }

    /// A left press anywhere on the screen while the bar has the keyboard,
    /// or on the bar itself when it does not.
    ///
    /// A press that lands outside dismisses AND is consumed - the Turbo
    /// Vision rule. Letting it through would also drop the pane selection
    /// and steal focus from whatever it happened to land on.
    pub fn click(&mut self, menus: &[Menu], bar: Rect, screen: Rect, x: u16, y: u16) -> MenuKey {
        if menus.is_empty() {
            return MenuKey::Ignored;
        }
        let before = self.mark_of(menus);
        let mut events = Vec::new();
        if let Some(title) = title_rects(menus, bar)
            .iter()
            .position(|rect| !rect.is_empty() && rect.contains((x, y).into()))
        {
            if title == self.open && self.is_dropped() {
                self.close(menus, CloseReason::Cancel, &mut events);
                return MenuKey::of(events);
            }
            if title != self.open && self.previewing_scope(menus).is_some() {
                events.push(MenuEvent::PreviewEnd { restore: true });
                events.push(MenuEvent::Close(CloseReason::Switched));
            }
            self.open = title;
            self.path.clear();
            let items = &menus[title].items;
            self.path.push(Level {
                index: first_selectable(items),
                scroll: 0,
            });
            self.moved(menus, before, &mut events);
            return MenuKey::of(events);
        }
        match self.item_at(menus, bar, screen, x, y) {
            Some((_, usize::MAX)) => MenuKey::Consumed,
            Some((depth, index)) => {
                self.path.truncate(depth + 1);
                self.path[depth].index = index;
                self.moved(menus, before, &mut events);
                self.choose(menus, &mut events);
                MenuKey::of(events)
            }
            None => {
                self.close(menus, CloseReason::Cancel, &mut events);
                MenuKey::of(events)
            }
        }
    }

    /// The pointer moved over an open dropdown: highlight follows it, which
    /// is what makes a theme preview under the mouse as well as the arrows.
    pub fn hover(&mut self, menus: &[Menu], bar: Rect, screen: Rect, x: u16, y: u16) -> MenuKey {
        if !self.is_dropped() {
            return MenuKey::Ignored;
        }
        let Some((depth, index)) = self.item_at(menus, bar, screen, x, y) else {
            return MenuKey::Ignored;
        };
        if index == usize::MAX || (depth + 1 == self.path.len() && self.path[depth].index == index)
        {
            return MenuKey::Consumed;
        }
        let before = self.mark_of(menus);
        let mut events = Vec::new();
        self.path.truncate(depth + 1);
        self.path[depth].index = index;
        self.moved(menus, before, &mut events);
        MenuKey::of(events)
    }
}

/// Draw the bar itself.
pub fn render_bar(
    buffer: &mut Buffer,
    area: Rect,
    menus: &[Menu],
    state: Option<&MenuState>,
    theme: &Theme,
) {
    if area.is_empty() {
        return;
    }
    super::view::clear_overlay(
        buffer,
        area,
        Style::default().bg(theme.surface).fg(theme.foreground),
    );
    for (index, rect) in title_rects(menus, area).iter().enumerate() {
        if rect.is_empty() {
            continue;
        }
        let selected = state.is_some_and(|state| state.open == index);
        let style = if selected {
            Style::default()
                .bg(theme.accent)
                .fg(theme.background)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(theme.foreground)
        };
        buffer.set_style(*rect, style);
        let title = &menus[index].title;
        buffer.set_stringn(
            rect.x + 1,
            rect.y,
            title,
            usize::from(rect.width.saturating_sub(1)),
            style,
        );
        // The mnemonic is marked, not merely documented: it is the whole
        // navigation model once the bar has the keyboard.
        if let Some(at) = menus[index].mnemonic_at {
            let before = UnicodeWidthStr::width(&title[..at]) as u16;
            if let Some(letter) = title[at..].chars().next() {
                buffer.set_stringn(
                    rect.x + 1 + before,
                    rect.y,
                    letter.to_string(),
                    1,
                    style.add_modifier(Modifier::UNDERLINED),
                );
            }
        }
    }
}

/// Draw every open dropdown, outermost first.
pub fn render_dropdowns(
    buffer: &mut Buffer,
    screen: Rect,
    bar: Rect,
    menus: &[Menu],
    state: &MenuState,
    theme: &Theme,
) {
    let rects = dropdown_rects(menus, state, bar, screen);
    for (depth, rect) in rects.iter().enumerate() {
        let Some(items) = state.items_at(menus, depth) else {
            continue;
        };
        super::view::clear_overlay(
            buffer,
            *rect,
            Style::default().bg(theme.surface).fg(theme.foreground),
        );
        draw_border(buffer, *rect, theme);
        let shown = visible_rows(*rect);
        let scroll = state.scroll_at(depth);
        let inner = rect.width.saturating_sub(2);
        let deepest = depth + 1 == state.path.len();
        for row in 0..shown {
            let Some(item) = items.get(scroll + row) else {
                break;
            };
            let y = rect.y + 1 + row as u16;
            if matches!(item.kind, MenuItemKind::Separator) {
                let rule = format!("\u{251c}{}\u{2524}", "\u{2500}".repeat(usize::from(inner)));
                buffer.set_stringn(
                    rect.x,
                    y,
                    rule,
                    usize::from(rect.width),
                    Style::default().fg(theme.muted).bg(theme.surface),
                );
                continue;
            }
            let highlighted = deepest && scroll + row == state.path[depth].index;
            let style = if highlighted {
                Style::default().bg(theme.accent).fg(theme.background)
            } else if item.enabled {
                Style::default().fg(theme.foreground)
            } else {
                Style::default().fg(theme.muted)
            };
            let row_rect = Rect::new(rect.x + 1, y, inner, 1);
            buffer.set_style(row_rect, style);
            // Keep the current row legible in a plain terminal capture and
            // for players who cannot distinguish the highlight colours.
            let cursor = if highlighted { "\u{25b8}" } else { " " };
            buffer.set_stringn(rect.x + 1, y, cursor, 1, style);
            buffer.set_stringn(rect.x + 2, y, item.mark(), 1, style);
            buffer.set_stringn(
                rect.x + 3,
                y,
                &item.label,
                usize::from(inner.saturating_sub(2)),
                style,
            );
            if let Some(at) = item.mnemonic_at
                && item.enabled
                && let Some(letter) = item.label[at..].chars().next()
            {
                let before = UnicodeWidthStr::width(&item.label[..at]) as u16;
                buffer.set_stringn(
                    rect.x + 3 + before,
                    y,
                    letter.to_string(),
                    1,
                    style.add_modifier(Modifier::UNDERLINED),
                );
            }
            let trailer = item.trailer();
            if !trailer.is_empty() {
                let width = UnicodeWidthStr::width(trailer) as u16;
                let x = rect.right().saturating_sub(width + 1);
                buffer.set_stringn(
                    x,
                    y,
                    trailer,
                    usize::from(width),
                    if highlighted {
                        style
                    } else {
                        style.fg(theme.muted)
                    },
                );
            }
        }
        // A list longer than its panel says so, rather than pretending the
        // rows below the fold are not there.
        if items.len() > shown {
            let more = format!("{}/{}", state.path[depth].index + 1, items.len());
            let width = UnicodeWidthStr::width(more.as_str()) as u16;
            buffer.set_stringn(
                rect.right().saturating_sub(width + 1),
                rect.bottom().saturating_sub(1),
                more,
                usize::from(width),
                Style::default().fg(theme.muted),
            );
        }
    }
}

fn draw_border(buffer: &mut Buffer, rect: Rect, theme: &Theme) {
    if rect.width < 2 || rect.height < 2 {
        return;
    }
    let style = Style::default().fg(theme.muted).bg(theme.surface);
    let inner = usize::from(rect.width.saturating_sub(2));
    let top = format!("\u{256d}{}\u{256e}", "\u{2500}".repeat(inner));
    let bottom = format!("\u{2570}{}\u{256f}", "\u{2500}".repeat(inner));
    buffer.set_stringn(rect.x, rect.y, top, usize::from(rect.width), style);
    buffer.set_stringn(
        rect.x,
        rect.bottom() - 1,
        bottom,
        usize::from(rect.width),
        style,
    );
    for y in rect.y + 1..rect.bottom() - 1 {
        buffer.set_stringn(rect.x, y, "\u{2502}", 1, style);
        buffer.set_stringn(rect.right() - 1, y, "\u{2502}", 1, style);
    }
}

/// What the menus need to know about the studio to draw themselves honestly.
///
/// Read fresh every frame. A menu built from a snapshot goes stale the moment
/// anything else changes the setting it shows, and a tick that lies is worse
/// than no tick.
#[derive(Clone, Copy, Debug)]
pub struct MenuContext<'a> {
    pub capabilities: super::editor::KeyboardCapabilities,
    /// The keybinding table, read fresh like everything else here. The
    /// menus spell a chord learnt in Settings from then on. An action the
    /// player unbound shows no accelerator, because no key fires it.
    pub keybinds: &'a super::keybinds::Keybinds,
    pub can_undo: bool,
    pub can_redo: bool,
    pub has_selection: bool,
    /// The caret is somewhere the smart action has an answer for.
    pub can_smart_action: bool,
    /// The scene on screen is played from its own cycle zero.
    pub scene_rewinds: bool,
    pub can_add_scene: bool,
    pub on_prebake: bool,
    pub can_delete_scene: bool,
    pub can_step_scene: bool,
    pub can_learn_pad: bool,
    pub has_pad: bool,
    pub has_last_file: bool,
    /// A take is being recorded: the Transport row reads "Finish".
    pub recording: bool,
    /// A sample is being recorded from the input: its row reads "Finish".
    pub recording_sample: bool,
    /// A tape is being written into the set's folder, which a rename
    /// would move from under it.
    pub recording_tape: bool,
    /// Recording was enabled for this run (including before its first save).
    pub can_record_session: bool,
    /// The set panel is open, docked or as a sheet.
    pub set_panel: bool,
    /// Which visuals docks are open.
    pub viz_panels: [bool; 2],
    pub split: bool,
    pub zen: bool,
    /// The mixer panel is open.
    pub mixer: bool,
    /// The log sheet is open. A toggle like the mixer's, because the row
    /// closes it as readily as it opens it and the menu should say which
    /// of the two the next press will do.
    pub log: bool,
    /// The background jobs sheet is open.
    pub jobs: bool,
    /// The memory breakdown is docked, whether or not the terminal has
    /// room to draw it just now.
    pub memory: bool,
    /// The numbers down the left edge of the score.
    pub line_numbers: bool,
    /// Long lines continue on the next row.
    pub wrap: bool,
    /// The File / Edit menu bar along the top.
    pub show_menu: bool,
    /// The rustel PLAYING tempo line.
    pub show_header: bool,
    /// The footer's meter, orbits and device chips.
    pub show_footer: bool,
    pub at_max_gain: bool,
    pub at_min_gain: bool,
    /// This set has a limiter on its desk, so the row offers to remove it
    /// rather than to add a second one.
    pub set_limiter: bool,
    pub help_fits: bool,
    pub settings_fits: bool,
    /// Remote control is compiled in and its sheet fits the frame.
    pub remote_control_available: bool,
}

impl MenuContext<'_> {
    /// Menu accelerators use the same effective binding as dispatch and settings.
    pub fn chord_hint(&self, action: super::keybinds::BindAction) -> String {
        self.keybinds.hint(action)
    }

    fn alias_hint(&self, action: super::keybinds::BindAction) -> String {
        self.keybinds
            .advertised_alias(action)
            .map(|alias| alias.hint())
            .unwrap_or_default()
    }
}

/// Brackets hop focus in a split; their shifted spelling cycles scenes.
/// An adapted or explicit binding always keeps the action assigned by the table.
pub(super) fn scene_shortcut_hint(
    keybinds: &super::keybinds::Keybinds,
    capabilities: super::editor::KeyboardCapabilities,
    action: super::keybinds::BindAction,
    split: bool,
) -> String {
    let Some(binding) = keybinds.binding(action) else {
        return String::new();
    };
    if keybinds.overridden(action) || binding != action.default_binding() {
        return binding.hint();
    }
    if capabilities.enhanced {
        if !split {
            return binding.hint();
        }
        let shifted = super::keybinds::KeyCombo {
            shift: true,
            ..binding
        };
        let event = KeyEvent::new(shifted.code, KeyModifiers::CONTROL | KeyModifiers::SHIFT);
        if keybinds.accepts_legacy_alias(action, &event) {
            return shifted.hint();
        }
    }
    keybinds
        .effective_alias(action)
        .map(|alias| alias.hint())
        .unwrap_or_default()
}

/// The seven menus. File acts on the set, which is a folder of scores; Scene
/// acts on the scenes of that set.
pub fn menus(cx: &MenuContext) -> Vec<Menu> {
    use super::keybinds::BindAction;
    let hint = |action: BindAction| cx.chord_hint(action);
    let with_alias = |action: BindAction| {
        [hint(action), cx.alias_hint(action)]
            .into_iter()
            .filter(|hint| !hint.is_empty())
            .collect::<Vec<_>>()
            .join(" / ")
    };
    let previous = scene_shortcut_hint(
        cx.keybinds,
        cx.capabilities,
        BindAction::PreviousScene,
        cx.split,
    );
    let next = scene_shortcut_hint(
        cx.keybinds,
        cx.capabilities,
        BindAction::NextScene,
        cx.split,
    );
    // Mnemonics are unique within a menu, which is why several are not the
    // first letter: Edit alone has Cut, Copy and Comment, and Scene has New
    // beside Next.
    // The set is the file: a folder of scores, made, opened and renamed
    // here. Scores have a menu of their own.
    let file = Menu::new(
        "File",
        'F',
        vec![
            MenuItem::action(MenuAction::NewSet, "New set", 'n', ""),
            MenuItem::action(MenuAction::NewSession, "New session", 'e', "n in set panel")
                .enabled(cx.can_record_session),
            MenuItem::action(
                MenuAction::OpenSet,
                "Open set\u{2026}",
                'o',
                hint(BindAction::OpenSet),
            ),
            MenuItem::action(MenuAction::OpenRecent, "Open recent\u{2026}", 'r', ""),
            MenuItem::action(MenuAction::RenameSet, "Rename set\u{2026}", 'a', "")
                .enabled(!cx.recording),
            MenuItem::separator(),
            // A set that carries its own samples is a set you can hand
            // over: nothing of yours travels until you ask for this.
            MenuItem::action(
                MenuAction::ConsolidateSamples,
                "Copy samples into the set",
                'y',
                "",
            ),
            MenuItem::separator(),
            // The set is a folder, and the desktop can show it.
            MenuItem::action(MenuAction::ShowSet, show_set_label(), 's', ""),
            MenuItem::separator(),
            MenuItem::action(MenuAction::Quit, "Quit", 'q', hint(BindAction::Quit)),
        ],
    );
    let scene = Menu::new(
        "Scene",
        'S',
        vec![
            MenuItem::action(
                MenuAction::NewScene,
                "New scene",
                'n',
                hint(BindAction::NewScene),
            )
            .enabled(cx.can_add_scene),
            MenuItem::action(
                MenuAction::DuplicateScene,
                "Duplicate scene",
                'd',
                with_alias(BindAction::DuplicateScene),
            )
            .enabled(cx.can_add_scene && !cx.on_prebake),
            MenuItem::action(
                MenuAction::RenameScene,
                "Rename scene",
                'r',
                hint(BindAction::RenameScene),
            )
            .enabled(!cx.on_prebake),
            MenuItem::action(
                MenuAction::CloseScene,
                "Close scene",
                'c',
                hint(BindAction::CloseScene),
            ),
            // Closing keeps the file; this is the one that does not. It
            // asks twice, the way Quit does.
            MenuItem::action(MenuAction::DeleteScene, "Delete scene", 't', "")
                .enabled(cx.can_delete_scene && !cx.on_prebake),
            MenuItem::separator(),
            MenuItem::action(MenuAction::PreviousScene, "Previous scene", 'p', previous)
                .enabled(cx.can_step_scene),
            MenuItem::action(MenuAction::NextScene, "Next scene", 'x', next)
                .enabled(cx.can_step_scene),
            MenuItem::separator(),
            MenuItem::action(
                MenuAction::LearnPad,
                "Learn MIDI pad",
                'l',
                hint(BindAction::LearnPad),
            )
            .enabled(cx.can_learn_pad && !cx.on_prebake),
            MenuItem::action(
                MenuAction::ForgetPad,
                "Forget pad",
                'f',
                hint(BindAction::ForgetPad),
            )
            .enabled(cx.has_pad && !cx.on_prebake),
            MenuItem::separator(),
            // Whether this scene is played from its own beginning. It
            // belongs to the scene rather than to the key that plays it,
            // so whatever fires it - this row, ^S, a pad, a click - does
            // the same thing, and the chip wears ⟲ to say so.
            MenuItem::new(
                MenuItemKind::Toggle {
                    id: MenuAction::SceneRewind,
                    on: cx.scene_rewinds,
                },
                "Rewind on play",
                'e',
                hint(BindAction::SceneRewind),
            )
            .enabled(!cx.on_prebake),
        ],
    );
    let edit = Menu::new(
        "Edit",
        'E',
        vec![
            MenuItem::action(MenuAction::Undo, "Undo", 'u', hint(BindAction::Undo))
                .enabled(cx.can_undo),
            MenuItem::action(MenuAction::Redo, "Redo", 'r', hint(BindAction::Redo))
                .enabled(cx.can_redo),
            MenuItem::separator(),
            MenuItem::action(MenuAction::Cut, "Cut", 'c', hint(BindAction::Cut))
                .enabled(cx.has_selection),
            MenuItem::action(MenuAction::Copy, "Copy", 'o', hint(BindAction::Copy))
                .enabled(cx.has_selection),
            // Never greyed: the only honest emptiness test round-trips the
            // desktop pasteboard, which is far too costly to run while
            // painting a menu, and an empty clipboard already says so in the
            // footer.
            MenuItem::action(MenuAction::Paste, "Paste", 'p', hint(BindAction::Paste)),
            MenuItem::separator(),
            MenuItem::action(
                MenuAction::SelectAll,
                "Select all",
                's',
                hint(BindAction::SelectAll),
            ),
            MenuItem::action(
                MenuAction::ToggleComment,
                "Comment lines",
                'm',
                hint(BindAction::ToggleComment),
            ),
            MenuItem::separator(),
            // The one row that turns a number into a fader, or pastes
            // the latest recorded sample - greyed where there is
            // nothing to offer, rather than opening and saying so in
            // the footer.
            MenuItem::action(
                MenuAction::SmartAction,
                "Smart action\u{2026}",
                'a',
                hint(BindAction::SmartAction),
            )
            .enabled(cx.can_smart_action),
            MenuItem::action(
                MenuAction::FirstError,
                "First error / locate cursor",
                'f',
                hint(BindAction::FirstError),
            ),
        ],
    );
    let transport = Menu::new(
        "Transport",
        'T',
        vec![
            MenuItem::action(
                MenuAction::Update,
                "Update",
                'u',
                with_alias(BindAction::Evaluate),
            ),
            // Under Update, because it is Update with one thing changed:
            // the score is heard from its beginning instead of joining the
            // cycle already running. The scene's own ⟲ does this every
            // time; this is the once-off, for a scene that does not wear
            // one.
            MenuItem::action(
                MenuAction::RewindUpdate,
                "Rewind update",
                'w',
                hint(BindAction::RewindEvaluate),
            ),
            MenuItem::action(MenuAction::Stop, "Stop", 's', with_alias(BindAction::Stop)),
            MenuItem::separator(),
            MenuItem::action(
                MenuAction::Record,
                if cx.recording {
                    "Finish the take"
                } else {
                    "Record a take"
                },
                'r',
                hint(BindAction::RecordTake),
            ),
            MenuItem::action(
                MenuAction::RecordSample,
                if cx.recording_sample {
                    "Finish the sample"
                } else {
                    "Record a sample"
                },
                'a',
                hint(BindAction::RecordSample),
            ),
            MenuItem::action(
                MenuAction::Export,
                "Export\u{2026}",
                'e',
                hint(BindAction::Export),
            )
            .enabled(!cx.on_prebake),
            // Greyed until there is one: an export or a take, shown in
            // the file manager.
            MenuItem::action(
                MenuAction::ShowLastFile,
                "Reveal last export or take",
                'v',
                "",
            )
            .enabled(cx.has_last_file),
            MenuItem::separator(),
            MenuItem::action(
                MenuAction::MasterUp,
                "Master louder",
                'l',
                hint(BindAction::MasterUp),
            )
            .enabled(!cx.at_max_gain),
            MenuItem::action(
                MenuAction::MasterDown,
                "Master quieter",
                'q',
                hint(BindAction::MasterDown),
            )
            .enabled(!cx.at_min_gain),
            // One row that flips, because there is only ever one answer
            // to give: the set has a limiter or it does not. `m` is the
            // mnemonic because it is in both labels.
            MenuItem::action(
                MenuAction::SetLimiter,
                if cx.set_limiter {
                    "Remove limiter from set"
                } else {
                    "Add limiter to set"
                },
                'm',
                "",
            ),
        ],
    );
    // The View menu is wired to the panels that already exist. Its switches,
    // the theme list and the device list become live rows in their own right
    // when the settings sheet and the two pickers are folded in.
    let view = Menu::new(
        "View",
        'V',
        vec![
            // ^F first: macOS eats Ctrl+Space at the input-source layer and
            // the default Mac F-row sends brightness, so F2 does not arrive
            // either. ^F is a plain 0x06 that almost every terminal delivers
            // and no platform claims. ^Space is named after it only where the
            // terminal profile says it arrives.
            MenuItem::action(
                MenuAction::Reference,
                "Argument values / reference",
                'r',
                with_alias(BindAction::Reference),
            ),
            MenuItem::action(
                MenuAction::Docs,
                "Docs for the function",
                'd',
                hint(BindAction::Docs),
            ),
            MenuItem::action(
                MenuAction::PianoMode,
                "Piano mode",
                'a',
                hint(BindAction::PianoMode),
            ),
            MenuItem::separator(),
            MenuItem::new(
                MenuItemKind::Toggle {
                    id: MenuAction::SetPanel,
                    on: cx.set_panel,
                },
                "Set panel",
                's',
                hint(BindAction::SetPanel),
            ),
            MenuItem::new(
                MenuItemKind::Toggle {
                    id: MenuAction::VizPanel,
                    on: cx.viz_panels[0],
                },
                "Visuals 1",
                'u',
                hint(BindAction::VisualsOne),
            ),
            MenuItem::new(
                MenuItemKind::Toggle {
                    id: MenuAction::VizPanelTwo,
                    on: cx.viz_panels[1],
                },
                "Visuals 2",
                'v',
                hint(BindAction::VisualsTwo),
            ),
            MenuItem::action(
                MenuAction::Split,
                if cx.split {
                    "Close the split"
                } else {
                    "Split the editor"
                },
                'p',
                hint(BindAction::Split),
            ),
            MenuItem::action(
                MenuAction::HopPane,
                "Switch pane",
                'i',
                hint(BindAction::HopPane),
            )
            .enabled(cx.split),
            MenuItem::action(
                MenuAction::SwitchPanel,
                "Switch panel",
                'c',
                hint(BindAction::FocusPanels),
            ),
            MenuItem::new(
                MenuItemKind::Toggle {
                    id: MenuAction::LineNumbers,
                    on: cx.line_numbers,
                },
                "Line numbers",
                'n',
                "",
            ),
            MenuItem::new(
                MenuItemKind::Toggle {
                    id: MenuAction::Wrap,
                    on: cx.wrap,
                },
                "Word wrap",
                'w',
                hint(BindAction::Wrap),
            ),
            MenuItem::new(
                MenuItemKind::Toggle {
                    id: MenuAction::Log,
                    on: cx.log,
                },
                "Log",
                'l',
                with_alias(BindAction::Log),
            ),
            MenuItem::new(
                MenuItemKind::Toggle {
                    id: MenuAction::Jobs,
                    on: cx.jobs,
                },
                "Background jobs",
                'b',
                hint(BindAction::Jobs),
            ),
            MenuItem::new(
                MenuItemKind::Toggle {
                    id: MenuAction::Mixer,
                    on: cx.mixer,
                },
                "Mixer",
                'm',
                with_alias(BindAction::Mixer),
            ),
            MenuItem::new(
                MenuItemKind::Toggle {
                    id: MenuAction::Zen,
                    on: cx.zen,
                },
                "Zen mode",
                'z',
                hint(BindAction::Zen),
            ),
        ],
    );
    // What the studio is set to: the chrome it draws, the settings sheet,
    // the devices, the theme - the rows a Preferences or Options menu holds
    // elsewhere.
    let mut options = Menu::new(
        "Options",
        'O',
        vec![
            MenuItem::new(
                MenuItemKind::Toggle {
                    id: MenuAction::ShowMenu,
                    on: cx.show_menu,
                },
                "Menu bar",
                'm',
                "",
            ),
            MenuItem::new(
                MenuItemKind::Toggle {
                    id: MenuAction::ShowHeader,
                    on: cx.show_header,
                },
                "Header",
                'h',
                "",
            ),
            MenuItem::new(
                MenuItemKind::Toggle {
                    id: MenuAction::ShowFooter,
                    on: cx.show_footer,
                },
                "Footer",
                'f',
                "",
            ),
            MenuItem::separator(),
            MenuItem::action(
                MenuAction::Settings,
                "Settings\u{2026}",
                's',
                hint(BindAction::Settings),
            )
            .enabled(cx.settings_fits),
            MenuItem::action(
                MenuAction::Devices,
                "Devices\u{2026}",
                'd',
                hint(BindAction::Devices),
            ),
            MenuItem::action(
                MenuAction::ThemePicker,
                "Theme\u{2026}",
                't',
                hint(BindAction::ThemePicker),
            ),
        ],
    );
    if cx.remote_control_available {
        options.items.extend([
            MenuItem::separator(),
            MenuItem::action(MenuAction::RemoteControl, "Remote control\u{2026}", 'r', ""),
        ]);
    }
    let help = Menu::new(
        "Help",
        'H',
        vec![
            // No accelerator: F1 opens the reference only where there is no
            // bar. Where this menu is drawn, F1 is the bar's own key, so an
            // F1 printed here would not open the reference.
            MenuItem::action(MenuAction::KeyboardReference, "Keyboard reference", 'k', "")
                .enabled(cx.help_fits),
            MenuItem::action(MenuAction::About, "About rustel", 'a', "").enabled(cx.settings_fits),
        ],
    );
    vec![file, edit, scene, transport, view, options, help]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::editor::KeyboardCapabilities;

    fn press(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn chord(code: KeyCode, modifiers: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, modifiers)
    }

    fn context() -> MenuContext<'static> {
        static DEFAULTS: std::sync::OnceLock<crate::keybinds::Keybinds> =
            std::sync::OnceLock::new();
        let keybinds = DEFAULTS.get_or_init(crate::keybinds::Keybinds::default);
        MenuContext {
            capabilities: KeyboardCapabilities::legacy(),
            keybinds,
            can_undo: true,
            can_redo: true,
            has_selection: true,
            can_smart_action: true,
            scene_rewinds: false,
            can_add_scene: true,
            on_prebake: false,
            can_delete_scene: true,
            can_step_scene: true,
            can_learn_pad: true,
            has_pad: true,
            has_last_file: true,
            recording: false,
            recording_sample: false,
            recording_tape: false,
            can_record_session: true,
            set_panel: false,
            viz_panels: [false, false],
            split: false,
            zen: false,
            mixer: false,
            log: false,
            jobs: false,
            memory: false,
            line_numbers: true,
            wrap: false,
            show_menu: true,
            show_header: true,
            show_footer: true,
            at_max_gain: false,
            at_min_gain: false,
            set_limiter: false,
            help_fits: true,
            settings_fits: true,
            remote_control_available: true,
        }
    }

    #[test]
    fn remote_control_is_the_last_options_group_when_its_sheet_is_available() {
        let options = |available| {
            menus(&MenuContext {
                remote_control_available: available,
                ..context()
            })
            .into_iter()
            .find(|menu| menu.title == "Options")
            .expect("Options menu")
            .items
        };
        let unavailable = options(false);
        assert_eq!(
            unavailable.last().unwrap().id(),
            Some(MenuAction::ThemePicker)
        );
        assert!(
            unavailable
                .iter()
                .all(|item| item.id() != Some(MenuAction::RemoteControl))
        );

        let available = options(true);
        assert_eq!(&available[..unavailable.len()], unavailable.as_slice());
        assert_eq!(available.len(), unavailable.len() + 2);
        assert_eq!(available[unavailable.len()].kind, MenuItemKind::Separator);
        let remote = available.last().unwrap();
        assert_eq!(remote.id(), Some(MenuAction::RemoteControl));
        assert_eq!(remote.label, "Remote control\u{2026}");
        assert_eq!(remote.mnemonic(), Some('r'));
        assert!(remote.enabled);
    }

    /// Turbo Vision navigation is unmodified letters, so two rows of one menu
    /// sharing a mnemonic makes the second unreachable. The real command list
    /// collides on the obvious choice three ways in Edit (Cut, Copy, Comment)
    /// and twice in Scene (New, Next), which is why several mnemonics are
    /// deliberately not the first letter.
    #[test]
    fn every_mnemonic_is_unique_within_its_menu() {
        let menus = menus(&context());
        let mut titles = Vec::new();
        for menu in &menus {
            let letter = menu.mnemonic().unwrap_or_else(|| {
                panic!("menu {:?} has no mnemonic in its own title", menu.title)
            });
            assert!(
                !titles.contains(&letter),
                "two menus answer to {letter}: {:?}",
                menu.title
            );
            titles.push(letter);

            let mut seen = Vec::new();
            for item in &menu.items {
                if matches!(item.kind, MenuItemKind::Separator) {
                    continue;
                }
                let letter = item.mnemonic().unwrap_or_else(|| {
                    panic!("{:?} in {:?} has no mnemonic", item.label, menu.title)
                });
                assert!(
                    !seen.contains(&letter),
                    "{:?} and an earlier row of {:?} both answer to {letter}",
                    item.label,
                    menu.title
                );
                seen.push(letter);
            }
        }
    }

    /// Menu accelerators must not advertise Command under any keyboard capability.
    #[test]
    fn no_menu_accelerator_is_spelled_with_command() {
        for capabilities in [
            KeyboardCapabilities::legacy(),
            KeyboardCapabilities::enhanced(),
            KeyboardCapabilities {
                enhanced: true,
                space_seen: false,
                super_seen: true,
            },
        ] {
            let menus = menus(&MenuContext {
                capabilities,
                ..context()
            });
            for menu in &menus {
                for item in &menu.items {
                    assert!(
                        !item.accel.contains('\u{2318}'),
                        "{:?} advertises {} with Command",
                        item.label,
                        item.accel
                    );
                }
            }
        }
    }

    /// Help ▸ Keyboard reference advertises no key: F1 reaches the reference
    /// only where there is no bar, and where this row is drawn F1 is the
    /// bar's own. An accelerator printed here would be dead everywhere it
    /// is read.
    #[test]
    fn the_keyboard_reference_row_advertises_no_dead_accelerator() {
        let menus = menus(&context());
        let item = menus
            .iter()
            .flat_map(|menu| menu.items.iter())
            .find(|item| item.label == "Keyboard reference")
            .expect("the help menu lists the keyboard reference");
        assert!(
            item.accel.is_empty(),
            "the row advertises {:?}, a key that cannot open it from a bar",
            item.accel
        );
    }

    /// The menus read the keybinding table: a chord learnt in Settings is
    /// what the row spells from then on, and an unbound action advertises
    /// nothing rather than a key that no longer fires. Untouched actions
    /// keep the rows' own spellings, aliases and all.
    #[test]
    fn the_menus_spell_the_learnt_chord_and_stay_silent_when_unbound() {
        use crate::keybinds::{BindAction, Keybinds, Reach};
        let untouched = menus(&context());
        let item = |rows: &[Menu], label: &str| -> String {
            rows.iter()
                .flat_map(|menu| menu.items.iter())
                .find(|item| item.label == label)
                .unwrap_or_else(|| panic!("{label} is on a menu"))
                .accel
                .clone()
        };
        assert_eq!(item(&untouched, "Undo"), "^Z", "untouched rows stand");
        assert_eq!(item(&untouched, "Update"), "^Enter / F5", "aliases stay");

        // Undo learnt onto F2: the row says so, and nothing else moved.
        let mut binds = Keybinds::default();
        binds.learn(
            BindAction::Undo,
            Some(crate::keybinds::KeyCombo::parse("f2").unwrap()),
        );
        let learnt = menus(&MenuContext {
            keybinds: &binds,
            ..context()
        });
        assert_eq!(item(&learnt, "Undo"), "F2");
        assert_eq!(item(&learnt, "Redo"), "^\u{21e7}Z", "neighbours stand");

        // Word wrap unbound: the row shows no accelerator.
        let mut binds = Keybinds::default();
        binds.unbind(BindAction::Wrap);
        let quiet = menus(&MenuContext {
            keybinds: &binds,
            ..context()
        });
        assert_eq!(
            item(&quiet, "Word wrap"),
            "",
            "an unbound action advertises nothing"
        );
        // And through the file, because a restart must say the same.
        let mut restored = Keybinds::default();
        restored.restore(&binds.prefs());
        let quiet = menus(&MenuContext {
            keybinds: &restored,
            ..context()
        });
        assert_eq!(item(&quiet, "Word wrap"), "");

        // The table also chooses terminal fallbacks for untouched actions.
        // Conhost cannot distinguish Ctrl+Shift+letter from Ctrl+letter, so
        // these are the actual keys it receives and the menu must say so.
        let mut binds = Keybinds::default();
        binds.set_reach(Reach {
            enhanced: false,
            terminal: "conhost".to_owned(),
        });
        let legacy = menus(&MenuContext {
            keybinds: &binds,
            ..context()
        });
        assert_eq!(item(&legacy, "Rewind update"), "⇧F5");
        assert_eq!(item(&legacy, "Redo"), "^Y");
        assert_eq!(item(&legacy, "Open set…"), "⇧F6");
        assert_eq!(item(&legacy, "Forget pad"), "⇧F8");
        assert_eq!(item(&legacy, "Rewind on play"), "⇧F7");
        assert_eq!(item(&legacy, "Export…"), "⇧F12");

        // Taking an action's secondary key removes that stale promise from
        // its old row too.
        let mut binds = Keybinds::default();
        binds.learn(
            BindAction::Stop,
            Some(crate::keybinds::KeyCombo::parse("f5").unwrap()),
        );
        binds.learn(
            BindAction::Reference,
            Some(crate::keybinds::KeyCombo::parse("f2").unwrap()),
        );
        let moved_aliases = menus(&MenuContext {
            keybinds: &binds,
            ..context()
        });
        assert_eq!(item(&moved_aliases, "Update"), "^Enter");
        assert_eq!(item(&moved_aliases, "Stop"), "F5");
        // The docs row follows its own action, so it keeps its chord.
        assert_eq!(item(&moved_aliases, "Argument values / reference"), "F2");
        assert_eq!(item(&moved_aliases, "Docs for the function"), "^D");
    }

    /// View names the two chords apart: the values row carries Ctrl+F and
    /// Ctrl+Space, the docs row carries Ctrl+D, and each follows its action.
    #[test]
    fn the_view_menu_names_values_and_docs_apart() {
        use crate::keybinds::{BindAction, KeyCombo, Keybinds};
        let row = |rows: &[Menu], action: MenuAction| -> (String, String) {
            let item = rows
                .iter()
                .flat_map(|menu| &menu.items)
                .find(|item| item.id() == Some(action))
                .unwrap_or_else(|| panic!("{action:?} has a menu row"));
            (item.label.to_string(), item.accel.clone())
        };
        let untouched = menus(&context());
        assert_eq!(
            row(&untouched, MenuAction::Reference),
            (
                "Argument values / reference".to_owned(),
                "^F / ^Space".to_owned()
            )
        );
        assert_eq!(
            row(&untouched, MenuAction::Docs),
            ("Docs for the function".to_owned(), "^D".to_owned())
        );

        let mut binds = Keybinds::default();
        binds.learn(BindAction::Reference, KeyCombo::parse("f2"));
        let moved = menus(&MenuContext {
            keybinds: &binds,
            ..context()
        });
        assert_eq!(row(&moved, MenuAction::Reference).1, "F2");
        assert_eq!(row(&moved, MenuAction::Docs).1, "^D");
        binds.learn(BindAction::Docs, KeyCombo::parse("f3"));
        let moved = menus(&MenuContext {
            keybinds: &binds,
            ..context()
        });
        assert_eq!(row(&moved, MenuAction::Docs).1, "F3");
    }

    #[test]
    fn every_bindable_menu_row_reads_the_keybinding_table() {
        use crate::keybinds::{BindAction, KeyCombo, Keybinds};

        let cases = [
            (BindAction::OpenSet, MenuAction::OpenSet),
            (BindAction::RecordTake, MenuAction::Record),
            (BindAction::RecordSample, MenuAction::RecordSample),
            (BindAction::DuplicateScene, MenuAction::DuplicateScene),
            (BindAction::PreviousScene, MenuAction::PreviousScene),
            (BindAction::NextScene, MenuAction::NextScene),
            (BindAction::Jobs, MenuAction::Jobs),
            (BindAction::MasterUp, MenuAction::MasterUp),
            (BindAction::MasterDown, MenuAction::MasterDown),
            (BindAction::Reference, MenuAction::Reference),
            (BindAction::Docs, MenuAction::Docs),
            (BindAction::PianoMode, MenuAction::PianoMode),
            (BindAction::SetPanel, MenuAction::SetPanel),
            (BindAction::HopPane, MenuAction::HopPane),
            (BindAction::FocusPanels, MenuAction::SwitchPanel),
            (BindAction::Log, MenuAction::Log),
            (BindAction::Mixer, MenuAction::Mixer),
        ];
        for (action, menu_action) in cases {
            let mut binds = Keybinds::default();
            binds.learn(action, Some(KeyCombo::parse("f2").expect("F2 parses")));
            let rows = menus(&MenuContext {
                keybinds: &binds,
                split: true,
                ..context()
            });
            let row = rows
                .iter()
                .flat_map(|menu| &menu.items)
                .find(|item| item.id() == Some(menu_action))
                .unwrap_or_else(|| panic!("{menu_action:?} has a menu row"));
            assert_eq!(row.accel, "F2", "{action:?} bypassed the binding table");
        }
    }

    /// The chords the items mirror are the same letters with Control held. A
    /// widget that saw only the `KeyCode` would fire the New mnemonic on ^N
    /// and swallow every chord the studio has.
    #[test]
    fn a_modified_key_is_never_a_mnemonic() {
        let menus = menus(&context());
        for modifier in [
            KeyModifiers::CONTROL,
            KeyModifiers::SUPER,
            KeyModifiers::ALT,
        ] {
            let mut state = MenuState::opened();
            let outcome = state.key(&chord(KeyCode::Char('s'), modifier), &menus);
            assert_eq!(
                outcome,
                MenuKey::Ignored,
                "{modifier:?}+s was taken for the Scene mnemonic"
            );
            assert!(!state.is_dropped(), "{modifier:?}+s opened a menu");
        }
        // Shift is not a modifier here - it is how an uppercase mnemonic
        // arrives, and `S` must still open Scene.
        let mut state = MenuState::opened();
        let outcome = state.key(&chord(KeyCode::Char('S'), KeyModifiers::SHIFT), &menus);
        assert_ne!(outcome, MenuKey::Ignored);
        assert!(state.is_dropped());
    }

    /// With a dropdown down, a letter that is not a mnemonic must be eaten.
    /// `Option<MenuEvent>` could not say this, and the letter was typed into
    /// the score underneath the open menu.
    #[test]
    fn an_unmatched_letter_is_swallowed_rather_than_passed_on() {
        let menus = menus(&context());
        let mut state = MenuState::dropped_on(&menus, 0);
        assert_eq!(
            state.key(&press(KeyCode::Char('j')), &menus),
            MenuKey::Consumed,
            "an unmatched letter fell through to the score"
        );
        assert!(state.is_dropped(), "it also closed the menu");
    }

    #[test]
    fn arrowing_reports_every_highlight_change_and_steps_over_separators() {
        let menus = menus(&context());
        let mut state = MenuState::opened();
        // Right twice along the bar, to Scene.
        state.key(&press(KeyCode::Right), &menus);
        state.key(&press(KeyCode::Right), &menus);
        // Down drops the menu onto its first usable row.
        let MenuKey::Events(events) = state.key(&press(KeyCode::Down), &menus) else {
            panic!("Down did not drop the menu");
        };
        assert_eq!(
            events,
            vec![MenuEvent::Highlight(Some(MenuAction::NewScene))]
        );
        // Five rows down from New scene is a separator; the highlight must
        // land on Previous scene, never on the rule.
        let mut seen = Vec::new();
        for _ in 0..5 {
            if let MenuKey::Events(events) = state.key(&press(KeyCode::Down), &menus) {
                for event in events {
                    if let MenuEvent::Highlight(Some(action)) = event {
                        seen.push(action);
                    }
                }
            }
        }
        assert_eq!(
            seen,
            vec![
                MenuAction::DuplicateScene,
                MenuAction::RenameScene,
                MenuAction::CloseScene,
                MenuAction::DeleteScene,
                MenuAction::PreviousScene,
            ]
        );
    }

    #[test]
    fn a_greyed_row_never_takes_the_highlight() {
        let menus = menus(&MenuContext {
            can_undo: false,
            can_redo: false,
            ..context()
        });
        let mut state = MenuState::dropped_on(&menus, 1);
        // Edit opens on Cut, because Undo and Redo are both unavailable.
        assert_eq!(state.highlighted_action(&menus), Some(MenuAction::Cut));
        // And arrowing never lands back on them.
        for _ in 0..menus[1].items.len() * 2 {
            state.key(&press(KeyCode::Down), &menus);
            let action = state.highlighted_action(&menus);
            assert!(
                action != Some(MenuAction::Undo) && action != Some(MenuAction::Redo),
                "the highlight rested on a greyed row"
            );
        }
    }

    #[test]
    fn choosing_a_row_activates_it_and_says_why_it_closed() {
        let menus = menus(&context());
        let mut state = MenuState::dropped_on(&menus, 2);
        let MenuKey::Events(events) = state.key(&press(KeyCode::Enter), &menus) else {
            panic!("Enter did nothing");
        };
        assert_eq!(
            events,
            vec![
                MenuEvent::Activate(MenuAction::NewScene),
                MenuEvent::Close(CloseReason::Committed),
            ]
        );
    }

    #[test]
    fn view_menu_exposes_the_panel_switch_chord() {
        let menus = menus(&context());
        let view = menus
            .iter()
            .find(|menu| menu.title == "View")
            .expect("View menu");
        let item = view
            .items
            .iter()
            .find(|item| item.id() == Some(MenuAction::SwitchPanel))
            .expect("Switch panel row");
        assert_eq!(item.label, "Switch panel");
        assert_eq!(item.accel, "⇧F10");
    }

    #[test]
    fn esc_cancels_and_a_switch_is_not_a_cancel() {
        let menus = menus(&context());
        let mut state = MenuState::dropped_on(&menus, 0);
        let MenuKey::Events(events) = state.key(&press(KeyCode::Esc), &menus) else {
            panic!("Esc did nothing");
        };
        assert_eq!(events, vec![MenuEvent::Close(CloseReason::Cancel)]);

        // Along the bar, the menu stays down and nothing closed.
        let mut state = MenuState::dropped_on(&menus, 0);
        let MenuKey::Events(events) = state.key(&press(KeyCode::Right), &menus) else {
            panic!("Right did nothing");
        };
        assert!(
            !events
                .iter()
                .any(|event| matches!(event, MenuEvent::Close(_))),
            "stepping along the bar reported a close: {events:?}"
        );
        assert_eq!(state.open_title(), 1);
        assert!(state.is_dropped(), "the menu should stay down");
    }

    /// A toggle keeps the menu down, so the next switch is one key away.
    #[test]
    fn a_toggle_closes_the_menu() {
        let menus = menus(&context());
        let mut state = MenuState::dropped_on(&menus, 4);
        let MenuKey::Events(events) = state.key(&press(KeyCode::Char('z')), &menus) else {
            panic!("z did not reach Zen mode");
        };
        // A mnemonic moves the highlight and then chooses, so both are
        // reported - a preview driven off Highlight must see the row that
        // was landed on even when the same key activates it.
        assert_eq!(
            events,
            vec![
                MenuEvent::Highlight(Some(MenuAction::Zen)),
                MenuEvent::Activate(MenuAction::Zen),
                MenuEvent::Close(CloseReason::Committed),
            ]
        );
    }

    /// The contract slice 3 needs: entering a previewing list captures once,
    /// leaving it says whether to put the original back.
    #[test]
    fn a_previewing_submenu_brackets_its_children_with_begin_and_end() {
        let leaves = vec![
            MenuItem::action(MenuAction::Log, "one", 'o', ""),
            MenuItem::action(MenuAction::Docs, "two", 't', ""),
        ];
        let menus = vec![Menu::new(
            "View",
            'V',
            vec![
                MenuItem::new(
                    MenuItemKind::Submenu {
                        id: MenuAction::ThemePicker,
                        items: leaves,
                        previews: true,
                    },
                    "Theme",
                    't',
                    "",
                ),
                MenuItem::action(MenuAction::Log, "Log", 'l', ""),
            ],
        )];
        let mut state = MenuState::dropped_on(&menus, 0);
        // Entering: capture, then show the first child.
        let MenuKey::Events(events) = state.key(&press(KeyCode::Right), &menus) else {
            panic!("Right did not enter the submenu");
        };
        assert_eq!(
            events,
            vec![
                MenuEvent::PreviewBegin(MenuAction::ThemePicker),
                MenuEvent::Highlight(Some(MenuAction::Log)),
            ]
        );
        // Moving inside must NOT re-capture, or Esc restores a preview
        // rather than what the musician started with.
        let MenuKey::Events(events) = state.key(&press(KeyCode::Down), &menus) else {
            panic!("Down did nothing");
        };
        assert_eq!(events, vec![MenuEvent::Highlight(Some(MenuAction::Docs))]);
        // Backing out restores.
        let MenuKey::Events(events) = state.key(&press(KeyCode::Left), &menus) else {
            panic!("Left did not leave the submenu");
        };
        assert_eq!(events[0], MenuEvent::PreviewEnd { restore: true });
        // Esc from inside restores too; Enter keeps.
        let mut state = MenuState::dropped_on(&menus, 0);
        state.key(&press(KeyCode::Right), &menus);
        let MenuKey::Events(events) = state.key(&press(KeyCode::Esc), &menus) else {
            panic!("Esc did nothing");
        };
        assert_eq!(
            events,
            vec![
                MenuEvent::PreviewEnd { restore: true },
                MenuEvent::Close(CloseReason::Cancel),
            ]
        );
        let mut state = MenuState::dropped_on(&menus, 0);
        state.key(&press(KeyCode::Right), &menus);
        let MenuKey::Events(events) = state.key(&press(KeyCode::Enter), &menus) else {
            panic!("Enter did nothing");
        };
        assert_eq!(
            events,
            vec![
                MenuEvent::Activate(MenuAction::Log),
                MenuEvent::PreviewEnd { restore: false },
                MenuEvent::Close(CloseReason::Committed),
            ]
        );
    }

    /// Thirty themes will not fit a panel, so the list has to scroll and the
    /// highlight has to stay on screen while it does.
    #[test]
    fn a_long_list_scrolls_to_keep_the_highlight_visible() {
        let items = (0..30)
            .map(|index| MenuItem::action(MenuAction::Docs, &format!("theme {index}"), 't', ""))
            .collect();
        let menus = vec![Menu::new("View", 'V', items)];
        let bar = Rect::new(0, 0, 80, 1);
        let screen = Rect::new(0, 0, 80, 14);
        let mut state = MenuState::dropped_on(&menus, 0);
        for _ in 0..25 {
            state.key(&press(KeyCode::Down), &menus);
            state.follow_scroll(&menus, bar, screen, true);
            let rect = dropdown_rects(&menus, &state, bar, screen)[0];
            let shown = visible_rows(rect);
            let index = state.highlight_path()[0];
            let scroll = state.scroll_at(0);
            assert!(
                index >= scroll && index < scroll + shown,
                "row {index} is outside the {shown} rows shown from {scroll}"
            );
            // The keys keep rows in sight below the highlight until the
            // list has nothing more below to show.
            let margin = crate::scroll::margin(shown);
            assert!(
                scroll + shown >= (index + margin + 1).min(30),
                "row {index} keeps {margin} rows below it in the {shown} shown from {scroll}"
            );
            assert!(rect.bottom() <= screen.bottom(), "the panel ran off screen");
        }
        // From the top again: a hover onto the bottom row shown does not
        // scroll the list under the pointer, and the next key does.
        let mut state = MenuState::dropped_on(&menus, 0);
        state.follow_scroll(&menus, bar, screen, true);
        let rect = dropdown_rects(&menus, &state, bar, screen)[0];
        let shown = visible_rows(rect);
        let bottom_row = rect.y + 1 + shown as u16 - 1;
        state.hover(&menus, bar, screen, rect.x + 2, bottom_row);
        state.follow_scroll(&menus, bar, screen, false);
        assert_eq!(
            state.highlight_path()[0],
            shown - 1,
            "the hover highlights it"
        );
        assert_eq!(state.scroll_at(0), 0, "and leaves the list where it is");
        state.key(&press(KeyCode::Down), &menus);
        state.follow_scroll(&menus, bar, screen, true);
        assert_eq!(
            state.scroll_at(0),
            crate::scroll::margin(shown) + 1,
            "the key scrolls to keep its margin below the new highlight"
        );
    }

    #[test]
    fn page_keys_follow_dropdown_height_and_stop_at_ends() {
        let menus = vec![Menu::new(
            "View",
            'V',
            (0..40)
                .map(|index| MenuItem::action(MenuAction::Docs, &format!("entry {index}"), 'e', ""))
                .collect(),
        )];
        let bar = Rect::new(0, 0, 80, 1);
        for screen in [Rect::new(0, 0, 80, 14), Rect::new(0, 0, 80, 24)] {
            let mut state = MenuState::dropped_on(&menus, 0);
            let shown = visible_rows(dropdown_rects(&menus, &state, bar, screen)[0]);
            state.key_with_area(&press(KeyCode::PageDown), &menus, bar, screen);
            assert_eq!(state.highlight_path(), vec![shown]);
            state.follow_scroll(&menus, bar, screen, true);
            assert!(state.scroll_at(0) <= shown);
            assert!(state.scroll_at(0) + shown > shown);
            state.key_with_area(&press(KeyCode::PageUp), &menus, bar, screen);
            assert_eq!(state.highlight_path(), vec![0]);
            state.key_with_area(&press(KeyCode::PageUp), &menus, bar, screen);
            assert_eq!(state.highlight_path(), vec![0]);
            for _ in 0..10 {
                state.key_with_area(&press(KeyCode::PageDown), &menus, bar, screen);
            }
            assert_eq!(state.highlight_path(), vec![39]);
            assert!(state.is_dropped());
        }
    }

    #[test]
    fn submenu_pages_skip_unselectable_rows_and_preserve_parent_and_modified_keys() {
        let mut leaves: Vec<_> = (0..40)
            .map(|index| MenuItem::action(MenuAction::Docs, &format!("entry {index}"), 'e', ""))
            .collect();
        // The nested menu has ten visible rows in this frame.
        leaves[10] = MenuItem::separator();
        leaves[11].enabled = false;
        leaves[39].enabled = false;
        let menus = vec![Menu::new(
            "View",
            'V',
            vec![MenuItem::new(
                MenuItemKind::Submenu {
                    id: MenuAction::ThemePicker,
                    items: leaves,
                    previews: false,
                },
                "Themes",
                't',
                "",
            )],
        )];
        let bar = Rect::new(0, 0, 80, 1);
        let screen = Rect::new(0, 0, 80, 14);
        let mut state = MenuState::dropped_on(&menus, 0);
        state.key(&press(KeyCode::Right), &menus);
        assert_eq!(
            visible_rows(*dropdown_rects(&menus, &state, bar, screen).last().unwrap()),
            10
        );
        state.key_with_area(&press(KeyCode::PageDown), &menus, bar, screen);
        assert_eq!(state.highlight_path(), vec![0, 12]);
        state.follow_scroll(&menus, bar, screen, true);
        assert_eq!(state.scroll_at(0), 0);
        let modified = KeyEvent::new(KeyCode::PageDown, KeyModifiers::CONTROL);
        assert!(matches!(
            state.key_with_area(&modified, &menus, bar, screen),
            MenuKey::Ignored
        ));
        assert_eq!(state.highlight_path(), vec![0, 12]);
        for _ in 0..10 {
            state.key_with_area(&press(KeyCode::PageDown), &menus, bar, screen);
            state.follow_scroll(&menus, bar, screen, true);
        }
        assert_eq!(state.highlight_path(), vec![0, 38]);
    }

    #[test]
    fn the_bar_and_a_dropdown_draw_where_they_are_hit_tested() {
        let theme = Theme::default();
        let menus = menus(&context());
        let bar = Rect::new(0, 0, 80, 1);
        // Tall enough for the Scene menu whole, Quit included.
        let screen = Rect::new(0, 0, 80, 30);
        let mut state = MenuState::dropped_on(&menus, 2);
        state.follow_scroll(&menus, bar, screen, true);
        let mut buffer = Buffer::empty(screen);
        render_bar(&mut buffer, bar, &menus, Some(&state), &theme);
        render_dropdowns(&mut buffer, screen, bar, &menus, &state, &theme);

        let row = |y: u16| -> String {
            (0..screen.width)
                .map(|x| buffer.cell((x, y)).unwrap().symbol().to_owned())
                .collect()
        };
        let titles = row(0);
        for menu in &menus {
            assert!(
                titles.contains(menu.title.as_str()),
                "{:?} is missing from the bar: {titles:?}",
                menu.title
            );
        }
        // The dropdown drew, with its accelerator, under the Scene title.
        let body: String = (1..screen.height).map(row).collect();
        assert!(body.contains("New scene"), "{body:?}");
        assert!(body.contains("^N"), "the accelerator is missing: {body:?}");
        assert!(body.contains("Delete scene"), "{body:?}");

        // Every drawn title is clickable at the x it was drawn at.
        for (index, rect) in title_rects(&menus, bar).iter().enumerate() {
            let mut probe = MenuState::opened();
            probe.click(&menus, bar, screen, rect.x, rect.y);
            assert_eq!(probe.open_title(), index);
        }
    }

    #[test]
    fn the_selected_menu_row_has_a_text_marker_without_hiding_its_toggle_tick() {
        let menus = vec![Menu::new(
            "View",
            'V',
            vec![
                MenuItem::new(
                    MenuItemKind::Toggle {
                        id: MenuAction::Wrap,
                        on: true,
                    },
                    "Wrap",
                    'w',
                    "",
                ),
                MenuItem::action(MenuAction::Docs, "Docs", 'd', ""),
            ],
        )];
        let screen = Rect::new(0, 0, 40, 10);
        let bar = Rect::new(0, 0, 40, 1);
        let mut state = MenuState::dropped_on(&menus, 0);
        let rect = dropdown_rects(&menus, &state, bar, screen)[0];
        let theme = Theme::default();
        let draw = |state: &MenuState| {
            let mut buffer = Buffer::empty(screen);
            render_dropdowns(&mut buffer, screen, bar, &menus, state, &theme);
            buffer
        };

        let buffer = draw(&state);
        assert_eq!(buffer.cell((rect.x + 1, rect.y + 1)).unwrap().symbol(), "▸");
        assert_eq!(buffer.cell((rect.x + 2, rect.y + 1)).unwrap().symbol(), "✓");
        assert_eq!(buffer.cell((rect.x + 3, rect.y + 1)).unwrap().symbol(), "W");
        assert_eq!(buffer.cell((rect.x + 1, rect.y + 2)).unwrap().symbol(), " ");

        state.key(&press(KeyCode::Down), &menus);
        let buffer = draw(&state);
        assert_eq!(buffer.cell((rect.x + 1, rect.y + 1)).unwrap().symbol(), " ");
        assert_eq!(buffer.cell((rect.x + 2, rect.y + 1)).unwrap().symbol(), "✓");
        assert_eq!(buffer.cell((rect.x + 1, rect.y + 2)).unwrap().symbol(), "▸");
    }

    /// One row for the limiter. Its label names what a press does: add
    /// the limiter, or remove it.
    #[test]
    fn the_transport_row_for_the_limiter_says_which_way_it_goes() {
        let row = |set_limiter| {
            let menus = menus(&MenuContext {
                set_limiter,
                ..context()
            });
            let transport = menus
                .iter()
                .find(|menu| menu.title == "Transport")
                .expect("a Transport menu");
            transport
                .items
                .iter()
                .find(|item| item.id() == Some(MenuAction::SetLimiter))
                .expect("a limiter row")
                .label
                .clone()
        };
        assert_eq!(row(false), "Add limiter to set");
        assert_eq!(row(true), "Remove limiter from set");
    }

    #[test]
    fn clicking_a_title_opens_that_menu_and_the_rects_match_what_is_drawn() {
        let menus = menus(&context());
        let bar = Rect::new(0, 0, 80, 1);
        let screen = Rect::new(0, 0, 80, 24);
        let rects = title_rects(&menus, bar);
        assert_eq!(rects.len(), menus.len());
        for (index, rect) in rects.iter().enumerate() {
            let mut state = MenuState::opened();
            state.click(&menus, bar, screen, rect.x, rect.y);
            assert_eq!(
                state.open_title(),
                index,
                "clicking {:?} opened the wrong menu",
                menus[index].title
            );
            assert!(state.is_dropped());
        }
    }

    #[test]
    fn menu_aliases_follow_terminal_reach_and_scene_rebindings() {
        use crate::keybinds::{BindAction, KeyCombo, Keybinds, Reach};
        let mut binds = Keybinds::default();
        binds.set_reach(Reach {
            enhanced: true,
            terminal: "Windows Terminal".to_owned(),
        });
        let accel = |binds: &Keybinds, action: MenuAction| {
            menus(&MenuContext {
                keybinds: binds,
                split: true,
                ..context()
            })
            .into_iter()
            .flat_map(|menu| menu.items)
            .find(|item| item.id() == Some(action))
            .unwrap()
            .accel
        };
        assert_eq!(accel(&binds, MenuAction::Log), "F9");
        assert_eq!(accel(&binds, MenuAction::Mixer), "F4");
        assert_eq!(accel(&binds, MenuAction::Zen), "^K");
        binds.set_reach(Reach {
            enhanced: false,
            terminal: "conhost".to_owned(),
        });
        assert_eq!(accel(&binds, MenuAction::Record), "⇧F11");
        assert_eq!(accel(&binds, MenuAction::PreviousScene), "F6");
        assert_eq!(accel(&binds, MenuAction::NextScene), "F7");
        assert_eq!(accel(&binds, MenuAction::HopPane), "F10");
        binds.learn(
            BindAction::DuplicateScene,
            Some(KeyCombo::parse("f2").unwrap()),
        );
        assert_eq!(accel(&binds, MenuAction::DuplicateScene), "F2");
        binds.unbind(BindAction::RecordTake);
        assert_eq!(accel(&binds, MenuAction::Record), "");
    }

    #[test]
    fn f1_belongs_to_the_live_binding_table_not_the_menu_widget() {
        let menus = menus(&context());
        for modifiers in [
            KeyModifiers::NONE,
            KeyModifiers::SHIFT,
            KeyModifiers::CONTROL,
        ] {
            let mut state = MenuState::dropped_on(&menus, 0);
            assert_eq!(
                state.key(&chord(KeyCode::F(1), modifiers), &menus),
                MenuKey::Ignored
            );
            assert!(state.is_dropped());
        }
    }

    #[test]
    fn scene_hints_follow_split_context_and_shortcut_ownership() {
        use crate::keybinds::{BindAction, KeyCombo, Keybinds, Reach};
        let mut binds = Keybinds::default();
        let enhanced = KeyboardCapabilities::enhanced();
        let previous = |binds: &Keybinds, split| {
            scene_shortcut_hint(binds, enhanced, BindAction::PreviousScene, split)
        };
        assert_eq!(previous(&binds, false), "^[");
        assert_eq!(previous(&binds, true), "^⇧[");
        binds.learn(
            BindAction::Stop,
            Some(KeyCombo::parse("ctrl+shift+[").unwrap()),
        );
        assert_eq!(previous(&binds, true), "F6");
        binds.learn(BindAction::Evaluate, Some(KeyCombo::parse("f6").unwrap()));
        assert_eq!(previous(&binds, true), "");
        binds.learn(
            BindAction::PreviousScene,
            Some(KeyCombo::parse("f2").unwrap()),
        );
        assert_eq!(previous(&binds, false), "F2");
        assert_eq!(previous(&binds, true), "F2");
        binds.unbind(BindAction::PreviousScene);
        assert_eq!(previous(&binds, true), "");
        let mut legacy = Keybinds::default();
        legacy.set_reach(Reach {
            enhanced: false,
            terminal: "conhost".into(),
        });
        assert_eq!(previous(&legacy, true), "F6");
    }
}
