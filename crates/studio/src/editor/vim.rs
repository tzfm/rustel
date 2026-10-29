mod motion;
pub(super) mod remap;
mod text_object;

use motion::motion_for;
use text_object::TextObjectKind;

use std::ops::Range;

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use unicode_segmentation::UnicodeSegmentation;

use super::{
    ByteOffset, Edit, EditOrigin, Editor, EditorError, FindDirection, HistoryMoment, Revision,
    Selection, SelectionSet, Transaction, document::line_ending_len, normalize_paste_line_endings,
};

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum EditorKeybindings {
    #[default]
    Default,
    Vim,
}

impl EditorKeybindings {
    pub fn key(self) -> &'static str {
        match self {
            Self::Default => "default",
            Self::Vim => "vim",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Default => "Default",
            Self::Vim => "Vim",
        }
    }

    pub fn from_key(key: &str) -> Option<Self> {
        match key {
            "default" => Some(Self::Default),
            "vim" => Some(Self::Vim),
            _ => None,
        }
    }

    pub fn cycle(&mut self) {
        *self = match self {
            Self::Default => Self::Vim,
            Self::Vim => Self::Default,
        };
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum Mode {
    #[default]
    Normal,
    Insert,
    Visual {
        anchor: ByteOffset,
        head: ByteOffset,
        linewise: bool,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Operator {
    Delete,
    Change,
    Yank,
}

#[derive(Clone, Copy)]
enum Motion {
    Left,
    Right,
    Up,
    Down,
    LineStart,
    FirstText,
    LineEnd,
    WordNext,
    WordPrevious,
    WordEnd,
    WordChange,
    FirstLine,
    LastLine,
}

#[derive(Clone, Debug, Default)]
pub(super) struct VimState {
    keybindings: EditorKeybindings,
    mode: Mode,
    count: usize,
    operator: Option<(Operator, usize)>,
    /// `r` waits for its replacement character, and how many to replace.
    replace: Option<usize>,
    text_object: Option<TextObjectKind>,
    g: bool,
    command: Option<String>,
    message: Option<&'static str>,
    motion_blocked: bool,
    register: String,
    linewise_register: bool,
    /// How many times a yank, delete or put has filled the register, so
    /// the same text yanked again still counts as the newest copy.
    register_writes: u64,
    insert_moment: HistoryMoment,
    /// The indent `o`, `O` or `cc` copied, and the revision that holds it.
    autoindent: Option<(Revision, Range<ByteOffset>)>,
}

impl Editor {
    pub(crate) fn set_keybindings(&mut self, keybindings: EditorKeybindings) {
        if self.vim.keybindings == keybindings {
            return;
        }
        self.vim_remap.clear_pending();
        self.vim = VimState {
            keybindings,
            register: std::mem::take(&mut self.vim.register),
            linewise_register: self.vim.linewise_register,
            register_writes: self.vim.register_writes,
            ..VimState::default()
        };
        self.close_history_group();
        if keybindings == EditorKeybindings::Vim {
            let _ = self.vim_normalize_caret();
        }
    }

    pub(crate) fn vim_is_insert(&self) -> bool {
        self.vim.keybindings == EditorKeybindings::Default || self.vim.mode == Mode::Insert
    }

    /// Whether typed text lands at the caret now. Normal editing always
    /// takes it; a modal profile takes it only in its text mode.
    pub(crate) fn accepts_typed_text(&self) -> bool {
        self.vim_is_insert()
    }

    /// Vim is on and rests in Normal, with no motion or command waiting.
    pub(crate) fn vim_is_normal(&self) -> bool {
        self.vim.keybindings == EditorKeybindings::Vim && self.vim.mode == Mode::Normal
    }

    pub(crate) fn uses_modal_keys(&self) -> bool {
        self.vim.keybindings == EditorKeybindings::Vim
    }

    /// The find bar outside a modal profile stays open after Enter.
    pub(crate) fn find_bar_keeps_open(&self) -> bool {
        !self.uses_modal_keys()
    }

    /// A mode where `p` puts text: Normal or Visual.
    pub(crate) fn vim_accepts_put(&self) -> bool {
        self.vim.keybindings == EditorKeybindings::Vim
            && matches!(self.vim.mode, Mode::Normal | Mode::Visual { .. })
    }

    /// The unnamed register, when it holds text.
    pub(crate) fn vim_register(&self) -> Option<&str> {
        (!self.vim.register.is_empty()).then_some(self.vim.register.as_str())
    }

    /// Fill the unnamed register with text copied elsewhere. Text that
    /// ends in a line break is linewise, as Vim reads a clipboard.
    pub(crate) fn set_vim_register(&mut self, text: String) {
        self.vim.linewise_register = text.ends_with(['\n', '\r']);
        self.vim.register = text;
    }

    /// Visual's two ends after `edits`, mapped the way the selection maps
    /// its own.
    pub(super) fn vim_visual_through(&self, edits: &[Edit]) -> Option<(ByteOffset, ByteOffset)> {
        let Mode::Visual { anchor, head, .. } = self.vim.mode else {
            return None;
        };
        let forward = anchor <= head;
        Some((
            super::map_offset(anchor, edits, !forward),
            super::map_offset(head, edits, forward),
        ))
    }

    pub(super) fn vim_set_visual_ends(&mut self, ends: Option<(ByteOffset, ByteOffset)>) {
        if let (Some(ends), Mode::Visual { anchor, head, .. }) = (ends, &mut self.vim.mode) {
            (*anchor, *head) = ends;
        }
    }

    /// How many times Vim itself has filled the register. Text taken in
    /// from the clipboard does not count.
    pub(crate) fn vim_register_writes(&self) -> u64 {
        self.vim.register_writes
    }

    /// A yank, delete or put fills the register.
    fn vim_fill_register(&mut self, text: String, linewise: bool) {
        self.vim.register = text;
        self.vim.linewise_register = linewise;
        self.vim.register_writes = self.vim.register_writes.wrapping_add(1);
    }

    /// Show a short Vim message in the footer's editor label.
    pub(crate) fn vim_say(&mut self, message: &'static str) {
        if self.vim.keybindings == EditorKeybindings::Vim {
            self.vim.message = Some(message);
        }
    }

    pub(crate) fn vim_motion_blocked(&self) -> bool {
        self.vim.motion_blocked
    }

    pub(crate) fn vim_needs_escape(&self) -> bool {
        self.vim.keybindings == EditorKeybindings::Vim
            && (self.vim.mode != Mode::Normal
                || self.vim.count != 0
                || self.vim.operator.is_some()
                || self.vim.text_object.is_some()
                || self.vim.g
                || self.vim.command.is_some()
                || self.find_open())
    }

    pub(crate) fn vim_label(&self) -> Option<String> {
        if self.vim.keybindings != EditorKeybindings::Vim {
            return None;
        }
        if self.find_open() {
            return self.find_label();
        }
        if let Some(command) = &self.vim.command {
            return Some(format!(":{command}"));
        }
        if let Some(message) = self.vim.message {
            return Some(message.to_owned());
        }
        let mut label = match self.vim.mode {
            Mode::Normal => "NORMAL",
            Mode::Insert => "INSERT",
            Mode::Visual {
                linewise: false, ..
            } => "VISUAL",
            Mode::Visual { linewise: true, .. } => "V-LINE",
        }
        .to_owned();
        if let Some((operator, count)) = self.vim.operator {
            label.push(' ');
            if count > 1 {
                label.push_str(&count.to_string());
            }
            label.push(match operator {
                Operator::Delete => 'd',
                Operator::Change => 'c',
                Operator::Yank => 'y',
            });
        }
        if self.vim.count > 0 {
            label.push(' ');
            label.push_str(&self.vim.count.to_string());
        }
        if self.vim.g {
            label.push_str(" g");
        }
        if let Some(kind) = self.vim.text_object {
            label.push_str(match kind {
                TextObjectKind::Inner => " i",
                TextObjectKind::Around => " a",
            });
        }
        Some(label)
    }

    pub(crate) fn vim_cancel_pending(&mut self) {
        if self.vim.keybindings != EditorKeybindings::Vim {
            return;
        }
        self.vim.count = 0;
        self.vim.operator = None;
        self.vim.replace = None;
        self.vim.text_object = None;
        self.vim.g = false;
        self.vim.command = None;
        self.vim.message = None;
        // The indent `o` copied is taken back only within its own undo
        // step. Once that step is closed it stays, so one undo still takes
        // the whole line.
        self.vim.autoindent = None;
        self.close_history_group();
    }

    pub(crate) fn interrupt_key_mode(&mut self) {
        self.find_cancel();
        self.vim_remap.clear_pending();
        self.vim_cancel_pending();
        if matches!(self.vim.mode, Mode::Visual { .. }) {
            self.vim.mode = Mode::Normal;
        }
    }

    /// Leave every key mode for a prompt that searches the score. Vim
    /// searches from Normal, as its `/` does, so Insert ends the way Esc
    /// ends it, and `n` walks the matches after Enter.
    pub(crate) fn leave_key_mode_for_find(&mut self) -> Result<(), EditorError> {
        self.interrupt_key_mode();
        if self.vim.keybindings == EditorKeybindings::Vim && self.vim.mode == Mode::Insert {
            self.vim_escape()?;
        }
        Ok(())
    }

    /// A paste while `:` waits for its command types into the command
    /// line, as its keys do. `false` when no command line is open.
    pub(crate) fn vim_command_paste(&mut self, text: &str) -> bool {
        if self.vim.command.is_none() {
            return false;
        }
        for character in text.chars() {
            self.vim_command_type(character);
        }
        true
    }

    /// After any input in Normal mode, the caret rests on a character, not
    /// past the end of its line. A caret that already rests on one is left
    /// alone, and so is a view the wheel scrolled away from it.
    pub(crate) fn normalize_after_input(&mut self) -> Result<(), EditorError> {
        if self.vim.keybindings == EditorKeybindings::Vim
            && self.vim.mode == Mode::Normal
            && self.selections.len() == 1
            && self.primary_selection().is_empty()
        {
            self.vim_normalize_caret()?;
        }
        Ok(())
    }

    pub(super) fn mode_history_transaction(
        &self,
        transaction: &mut Transaction,
        moment: HistoryMoment,
    ) -> HistoryMoment {
        if self.vim.keybindings == EditorKeybindings::Vim
            && self.vim.mode == Mode::Insert
            && matches!(
                transaction.origin,
                EditOrigin::Typing
                    | EditOrigin::Backspace
                    | EditOrigin::Delete
                    | EditOrigin::Newline
                    | EditOrigin::Indent
                    | EditOrigin::Paste
            )
        {
            // Insert mode keeps one undo step across pauses, line breaks
            // and pastes. A slider or knob edit between keeps its own.
            transaction.origin = EditOrigin::Insert;
            self.vim.insert_moment
        } else {
            moment
        }
    }

    pub(crate) fn vim_key(
        &mut self,
        key: KeyEvent,
        moment: HistoryMoment,
        atomic_delete_range: Option<Range<ByteOffset>>,
    ) -> Result<bool, EditorError> {
        self.vim.motion_blocked = false;
        if self.vim.keybindings != EditorKeybindings::Vim {
            return Ok(false);
        }
        if key.kind == KeyEventKind::Release {
            return Ok(true);
        }
        // Shift, Ctrl and the other modifier keys arrive as keys of their
        // own when a terminal reports every key. They change no Vim state.
        if matches!(key.code, KeyCode::Modifier(_)) {
            return Ok(true);
        }
        if self.find_open() {
            self.find_key(key);
            return Ok(true);
        }
        if key.modifiers.intersects(
            KeyModifiers::ALT | KeyModifiers::SUPER | KeyModifiers::HYPER | KeyModifiers::META,
        ) {
            return Ok(false);
        }
        if key.code == KeyCode::Esc
            || (key.code == KeyCode::Char('[') && key.modifiers == KeyModifiers::CONTROL)
        {
            self.vim_escape()?;
            return Ok(true);
        }
        // r waits for its replacement character. Esc above cancels it.
        if let Some(count) = self.vim.replace.take() {
            if matches!(self.vim.mode, Mode::Visual { .. }) {
                self.vim_replace_selection(key, moment)?;
            } else {
                self.vim_replace(count, key, moment)?;
            }
            return Ok(true);
        }
        // Ctrl+C leaves Insert mode, as it does in Vim. Elsewhere it keeps
        // its studio meaning: copy. Enhanced terminals report Shift too.
        if matches!(key.code, KeyCode::Char('c' | 'C'))
            && key.modifiers.contains(KeyModifiers::CONTROL)
            && self.vim.mode == Mode::Insert
        {
            self.vim_escape()?;
            return Ok(true);
        }
        if key.code == KeyCode::Char('r') && key.modifiers == KeyModifiers::CONTROL {
            if self.vim.mode == Mode::Insert || self.vim.command.is_some() {
                self.vim.message = Some("Register insertion is not supported");
            } else {
                let count = self.vim_take_count();
                self.vim_escape()?;
                for _ in 0..count.min(self.redo_depth()) {
                    self.redo()?;
                }
                self.vim_normalize_caret()?;
            }
            return Ok(true);
        }
        if self.vim.mode == Mode::Insert {
            self.vim.message = None;
            return Ok(false);
        }
        // A mouse drag or a Shift+arrow selection is still a selection.
        // Adopt it as Visual for the commands that act on one, so d, c, y,
        // x and p use it. Other keys keep their Normal meaning: i enters
        // Insert and a motion moves instead of extending.
        let pending = self.vim_has_pending_command();
        let acts_on_selection = key.modifiers.is_empty()
            && (matches!(key.code, KeyCode::Backspace | KeyCode::Delete)
                || matches!(
                    key.code,
                    KeyCode::Char(
                        'd' | 'c' | 'y' | 'x' | 'X' | 'D' | 'C' | 'Y' | 'p' | 'P' | 'v' | 'V'
                    )
                ));
        let entered_visual = !pending && acts_on_selection && self.vim_selection_to_visual()?;
        if entered_visual && matches!(key.code, KeyCode::Char('v' | 'V')) {
            return Ok(true);
        }
        if !entered_visual
            && !pending
            && self.vim.mode == Mode::Normal
            && key.modifiers == KeyModifiers::SHIFT
            && let Some(motion) = selection_motion(key.code)
        {
            // The ordinary editor extends the selection exactly as it does
            // outside Vim. Visual takes it over at once.
            self.move_selection(motion, true)?;
            self.vim_selection_to_visual()?;
            return Ok(true);
        }
        if key.modifiers == KeyModifiers::CONTROL && matches!(key.code, KeyCode::Char('d' | 'u')) {
            let counted = self.vim.count != 0;
            let count = self.vim_take_count();
            if self.vim.operator.take().is_some() {
                self.vim_cancel_pending();
                return Ok(true);
            }
            self.vim.message = None;
            self.vim_half_page(key.code == KeyCode::Char('d'), counted.then_some(count))?;
            return Ok(true);
        }
        if key.modifiers.contains(KeyModifiers::CONTROL) {
            return Ok(false);
        }
        self.vim.message = None;
        if self.vim.command.is_some() {
            self.vim_command_key(key)?;
            return Ok(true);
        }
        if let Some(kind) = self.vim.text_object.take() {
            if let KeyCode::Char(character) = key.code {
                self.vim_finish_text_object(kind, character, moment)?;
            } else {
                self.vim_cancel_pending();
            }
            return Ok(true);
        }
        self.vim_normalize_caret()?;
        let character = match key.code {
            KeyCode::Char(character) => character,
            KeyCode::Left => 'h',
            // Vim's Visual backspace deletes the highlighted text, while
            // Normal backspace is the left motion.
            KeyCode::Backspace => {
                if matches!(self.vim.mode, Mode::Visual { .. }) {
                    'x'
                } else {
                    'h'
                }
            }
            KeyCode::Right => 'l',
            KeyCode::Up => 'k',
            KeyCode::Down => 'j',
            KeyCode::Home => '0',
            KeyCode::End => '$',
            KeyCode::Delete => 'x',
            KeyCode::Enter => {
                if self.vim.operator.is_some() {
                    self.vim_finish_motion(Motion::Down, moment)?;
                    return Ok(true);
                }
                let count = self.vim_take_count();
                self.vim_move(Motion::Down, count)?;
                self.vim_move(Motion::FirstText, 1)?;
                return Ok(true);
            }
            KeyCode::Tab | KeyCode::BackTab => return Ok(true),
            _ => {
                self.interrupt_key_mode();
                return Ok(false);
            }
        };
        if character.is_ascii_digit() && (character != '0' || self.vim.count != 0) {
            self.vim.count = self
                .vim
                .count
                .saturating_mul(10)
                .saturating_add((character as u8 - b'0') as usize)
                .min(self.document.maximum_bytes());
            return Ok(true);
        }
        if self.vim.g {
            self.vim.g = false;
            if character == 'g' {
                return self
                    .vim_finish_motion(Motion::FirstLine, moment)
                    .map(|()| true);
            }
            self.vim_cancel_pending();
            return Ok(true);
        }
        if character == 'g' {
            self.vim.g = true;
            return Ok(true);
        }
        if matches!(character, 'i' | 'a')
            && (self.vim.operator.is_some() || matches!(self.vim.mode, Mode::Visual { .. }))
        {
            self.vim.text_object = Some(if character == 'i' {
                TextObjectKind::Inner
            } else {
                TextObjectKind::Around
            });
            return Ok(true);
        }
        if let Some(motion) = motion_for(character) {
            self.vim_finish_motion(motion, moment)?;
            return Ok(true);
        }
        let operator = match character {
            'd' => Some(Operator::Delete),
            'c' => Some(Operator::Change),
            'y' => Some(Operator::Yank),
            _ => None,
        };
        if let Some(operator) = operator {
            if let Mode::Visual { linewise, .. } = self.vim.mode {
                let range = self.primary_selection().ordered();
                let start = range.start;
                self.vim_apply_operator(operator, range, linewise, start, moment)?;
            } else if let Some((pending, count)) = self.vim.operator.take() {
                if pending == operator {
                    let count = count.saturating_mul(self.vim_take_count());
                    let head = self.vim_head();
                    let line = self.document.line_of(head)?;
                    let range = self.vim_line_range(line, line.saturating_add(count - 1));
                    self.vim_apply_operator(operator, range, true, head, moment)?;
                } else {
                    self.vim_cancel_pending();
                }
            } else {
                let count = self.vim_take_count();
                self.vim.operator = Some((operator, count));
            }
            return Ok(true);
        }
        if self.vim.operator.take().is_some() {
            self.vim_cancel_pending();
            return Ok(true);
        }
        let count = self.vim_take_count();
        match character {
            'i' | 'a' | 'I' | 'A' if self.vim.mode == Mode::Normal => {
                let head = self.vim_head();
                let line = self.document.line_of(head)?;
                let range = self.document.line_content_range(line);
                let target = match character {
                    'a' => self.document.next_grapheme_boundary(head)?.min(range.end),
                    'I' => self.document.indent_end(line),
                    'A' => range.end,
                    _ => head,
                };
                self.vim_place_caret(target)?;
                self.vim_begin_insert(moment);
            }
            'o' | 'O' if self.vim.mode == Mode::Normal => {
                self.vim_open_line(character == 'O', moment)?;
            }
            'v' | 'V' => self.vim_toggle_visual(character == 'V')?,
            'x' | 'X' | 'D' | 'C' | 'Y' => {
                let operator = match character {
                    'C' => Operator::Change,
                    'Y' => Operator::Yank,
                    _ => Operator::Delete,
                };
                let head = self.vim_head();
                let line = self.document.line_of(head)?;
                let (range, linewise) = if let Mode::Visual {
                    anchor, linewise, ..
                } = self.vim.mode
                {
                    if character == 'x' {
                        (self.primary_selection().ordered(), linewise)
                    } else {
                        // Visual D, X, Y and C take the highlighted lines
                        // whole, as in Vim.
                        let first = self.document.line_of(anchor.min(head))?;
                        let last = self.document.line_of(anchor.max(head))?;
                        (self.vim_line_range(first, last), true)
                    }
                } else if character == 'Y' {
                    (
                        self.vim_line_range(line, line.saturating_add(count - 1)),
                        true,
                    )
                } else if character == 'x'
                    && let Some(range) = atomic_delete_range.filter(|range| range.start == head)
                {
                    let line_end = self
                        .document
                        .line_content_range(self.document.line_of(range.end)?)
                        .end;
                    let tail = self.document.slice(range.end..line_end)?;
                    let extra = tail
                        .grapheme_indices(true)
                        .nth(count - 1)
                        .map_or(tail.len(), |(offset, _)| offset);
                    (range.start..ByteOffset(range.end.0 + extra), false)
                } else {
                    let motion = match character {
                        'X' => Motion::Left,
                        'x' => Motion::Right,
                        _ => Motion::LineEnd,
                    };
                    let target = self.vim_motion_target(motion, count, true)?.0;
                    (head.min(target)..head.max(target), false)
                };
                // Y keeps the caret where it is, as yy does.
                let start = if linewise && self.vim.mode == Mode::Normal {
                    head
                } else {
                    range.start
                };
                self.vim_apply_operator(operator, range, linewise, start, moment)?;
            }
            'p' | 'P' => self.vim_put(character == 'P', count, moment)?,
            'r' => {
                self.vim.replace = Some(count);
            }
            'u' => {
                self.vim_escape()?;
                self.vim_undo(count)?;
            }
            '/' | '?' if self.vim.mode == Mode::Normal => {
                self.vim_escape()?;
                let direction = if character == '/' {
                    FindDirection::Forward
                } else {
                    FindDirection::Backward
                };
                self.begin_find(direction, false, count);
            }
            'n' | 'N' if self.vim.mode == Mode::Normal => {
                self.repeat_find(character == 'N', count);
            }
            ':' => {
                self.vim_escape()?;
                self.vim.command = Some(String::new());
            }
            _ => {}
        }
        Ok(true)
    }

    fn vim_command_key(&mut self, key: KeyEvent) -> Result<(), EditorError> {
        match key.code {
            KeyCode::Enter => {
                let command = self.vim.command.take().unwrap_or_default();
                match command.trim() {
                    "u" | "undo" => self.vim_undo(1)?,
                    "red" | "redo" => {
                        self.redo()?;
                        self.vim_normalize_caret()?;
                    }
                    "$" => self.vim_move(Motion::LastLine, 0)?,
                    "" => {}
                    number => match number.parse::<usize>() {
                        Ok(number) => self.vim_move(Motion::FirstLine, number.max(1))?,
                        Err(_) => self.vim.message = Some("Use :<line>, :undo or :redo"),
                    },
                }
            }
            KeyCode::Backspace => {
                if let Some(command) = self.vim.command.as_mut()
                    && command.pop().is_none()
                {
                    self.vim.command = None;
                }
            }
            KeyCode::Char(character) => self.vim_command_type(character),
            _ => {}
        }
        Ok(())
    }

    fn vim_command_type(&mut self, character: char) {
        if let Some(command) = self.vim.command.as_mut()
            && command.len() < 64
            && !character.is_control()
        {
            command.push(character);
        }
    }

    /// Undo `count` steps. An undo brings back the selection its change
    /// was made from, and Normal keeps a caret at its start, where Vim
    /// puts the cursor.
    fn vim_undo(&mut self, count: usize) -> Result<(), EditorError> {
        let steps = count.min(self.undo_depth());
        for _ in 0..steps {
            self.undo()?;
        }
        if steps > 0 {
            self.vim_place_caret(self.primary_selection().ordered().start)?;
        }
        self.vim_normalize_caret()
    }

    fn vim_head(&self) -> ByteOffset {
        match self.vim.mode {
            Mode::Visual { head, .. } => head,
            _ => self.primary_selection().head,
        }
    }

    fn vim_take_count(&mut self) -> usize {
        std::mem::take(&mut self.vim.count).max(1)
    }

    fn vim_begin_insert(&mut self, moment: HistoryMoment) {
        self.vim_cancel_pending();
        self.vim.mode = Mode::Insert;
        self.vim.insert_moment = moment;
    }

    fn vim_escape(&mut self) -> Result<(), EditorError> {
        self.vim_drop_unused_indent()?;
        let head = self.vim_head();
        let target = if self.vim.mode == Mode::Insert {
            let start = self.document.line_start(self.document.line_of(head)?);
            self.document.previous_grapheme_boundary(head)?.max(start)
        } else {
            head
        };
        self.vim.mode = Mode::Normal;
        self.vim_cancel_pending();
        // Esc moves the caret out of Insert or Visual. A caret already in
        // place stays, and so does a view the wheel scrolled away from it.
        let selection = self.primary_selection();
        if self.selections.len() > 1 || selection.anchor != target || selection.head != target {
            self.vim_place_caret(target)?;
        } else {
            // The column j and k aim for is where the caret rests now, as
            // after a move.
            self.selections.set_single(Selection::caret(target));
        }
        self.vim_normalize_caret()
    }

    /// Rest the caret on a character, and bring it into view when that
    /// moved it. A caret that did not move leaves the view where it is.
    fn vim_normalize_caret(&mut self) -> Result<(), EditorError> {
        if matches!(self.vim.mode, Mode::Visual { .. }) {
            return Ok(());
        }
        let head = self.primary_selection().head;
        let range = self
            .document
            .line_content_range(self.document.line_of(head)?);
        let target = if head >= range.end && range.end > range.start {
            self.document.previous_grapheme_boundary(range.end)?
        } else {
            head
        };
        let goal = self.primary_selection().goal_column;
        self.selections.set_single(Selection {
            anchor: target,
            head: target,
            goal_column: goal,
        });
        if target != head {
            self.ensure_primary_visible();
        }
        Ok(())
    }

    fn vim_place_caret(&mut self, head: ByteOffset) -> Result<(), EditorError> {
        self.set_selection(Selection::caret(head))?;
        self.ensure_primary_visible();
        Ok(())
    }

    fn vim_toggle_visual(&mut self, linewise: bool) -> Result<(), EditorError> {
        match self.vim.mode {
            Mode::Visual {
                linewise: current, ..
            } if current == linewise => self.vim_escape(),
            Mode::Visual { anchor, head, .. } => self.vim_select_visual(anchor, head, linewise),
            _ => {
                let head = self.vim_head();
                self.vim_select_visual(head, head, linewise)
            }
        }
    }

    /// A selection made with the mouse or with Shift+arrows is still a
    /// selection. Adopt it as Visual before the next command, so d, c, y,
    /// x and p act on it instead of collapsing it. `true` when a Normal
    /// mode selection became Visual.
    fn vim_selection_to_visual(&mut self) -> Result<bool, EditorError> {
        if !matches!(self.vim.mode, Mode::Normal | Mode::Visual { .. })
            || matches!(self.vim.mode, Mode::Visual { linewise: true, .. })
        {
            return Ok(false);
        }
        let selection = self.primary_selection();
        if selection.is_empty() {
            return Ok(false);
        }
        let was_normal = self.vim.mode == Mode::Normal;
        let (anchor, head) = if selection.anchor <= selection.head {
            (
                selection.anchor,
                self.document
                    .previous_grapheme_boundary(selection.head)?
                    .max(selection.anchor),
            )
        } else {
            (
                self.document.previous_grapheme_boundary(selection.anchor)?,
                selection.head,
            )
        };
        self.vim_select_visual(anchor, head, false)?;
        Ok(was_normal)
    }

    /// Take a released pointer selection into Visual, and a plain click
    /// back to Normal. Drag updates stay untouched until the release.
    pub(super) fn sync_selection_mode(&mut self) -> Result<(), EditorError> {
        if self.vim.keybindings != EditorKeybindings::Vim {
            return Ok(());
        }
        if self.primary_selection().is_empty() {
            if matches!(self.vim.mode, Mode::Visual { .. }) {
                self.vim.mode = Mode::Normal;
            }
            return Ok(());
        }
        // The pointer picked characters, not lines. V-LINE gives way, or
        // its whole lines would stand for a range it never selected.
        if matches!(self.vim.mode, Mode::Visual { linewise: true, .. }) {
            self.vim.mode = Mode::Normal;
        }
        self.vim_selection_to_visual()?;
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn vim_is_visual(&self) -> bool {
        matches!(self.vim.mode, Mode::Visual { .. })
    }

    fn vim_select_visual(
        &mut self,
        anchor: ByteOffset,
        head: ByteOffset,
        linewise: bool,
    ) -> Result<(), EditorError> {
        let selection = if linewise {
            let first = self.document.line_of(anchor.min(head))?;
            let last = self.document.line_of(anchor.max(head))?;
            let range = self.vim_line_range(first, last);
            if head >= anchor {
                Selection::range(range.start, range.end)
            } else {
                Selection::range(range.end, range.start)
            }
        } else if head >= anchor {
            Selection::range(anchor, self.document.next_grapheme_boundary(head)?)
        } else {
            Selection::range(self.document.next_grapheme_boundary(anchor)?, head)
        };
        self.set_selection(selection)?;
        self.vim.mode = Mode::Visual {
            anchor,
            head,
            linewise,
        };
        self.ensure_primary_visible();
        Ok(())
    }

    /// Run `operator` over `range`. A yank leaves the caret at `start`, the
    /// start of the text as Vim takes it: where a motion back ended, or the
    /// caret itself on the first line of a linewise command.
    fn vim_apply_operator(
        &mut self,
        operator: Operator,
        range: Range<ByteOffset>,
        linewise: bool,
        start: ByteOffset,
        moment: HistoryMoment,
    ) -> Result<(), EditorError> {
        self.vim_cancel_pending();
        let (mut range, linewise) = if linewise || matches!(self.vim.mode, Mode::Visual { .. }) {
            (range, linewise)
        } else {
            self.vim_exclusive_range(range)?
        };
        let mut register = self.document.slice(range.clone())?;
        let ending = self.preferred_line_ending();
        let has_line_ending = range.start < range.end
            && range.end == self.document.line_start(self.document.line_of(range.end)?);
        if linewise && !has_line_ending {
            register.push_str(&ending);
        }
        self.vim.mode = Mode::Normal;
        if (range.is_empty() && !linewise || self.document.len_bytes() == 0)
            && operator != Operator::Change
        {
            self.vim_place_caret(start)?;
            return self.vim_normalize_caret();
        }
        if operator == Operator::Yank {
            self.vim_fill_register(register, linewise);
            self.vim_place_caret(start)?;
            self.vim_normalize_caret()?;
            return Ok(());
        }
        let mut replacement = String::new();
        let mut indent_bytes = 0;
        if linewise && operator == Operator::Change {
            replacement = self
                .document
                .line_content(self.document.line_of(range.start)?)
                .chars()
                .take_while(|character| matches!(character, ' ' | '\t'))
                .collect();
            indent_bytes = replacement.len();
            if range.end.0 < self.document.len_bytes() || has_line_ending {
                replacement.push_str(&ending);
            }
        } else if linewise
            && range.end.0 == self.document.len_bytes()
            && range.start > ByteOffset::ZERO
            && !has_line_ending
        {
            let first = self.document.line_of(range.start)?;
            range.start = self.document.line_content_range(first - 1).end;
        }
        let target = ByteOffset(range.start.0 + indent_bytes);
        if operator == Operator::Change {
            self.vim_begin_insert(moment);
        }
        self.apply_transaction(
            Transaction::new(EditOrigin::Typing)
                .replace(Edit::new(range, replacement))
                .with_selection(SelectionSet::single(target)),
            moment,
        )?;
        self.vim_fill_register(register, linewise);
        if operator == Operator::Change {
            self.vim_note_autoindent(indent_bytes);
        } else {
            if linewise {
                let line = self
                    .document
                    .line_of(self.vim_head())?
                    .min(self.vim_last_line());
                self.vim_place_caret(self.document.indent_end(line))?;
            }
            self.vim_normalize_caret()?;
        }
        Ok(())
    }

    /// An exclusive range that ends where a later line starts stops at the
    /// end of the line before. One that begins at or before the text of its
    /// first line takes those lines whole instead, as Vim's `:h exclusive`
    /// says. So `dw` on an empty line deletes the line.
    fn vim_exclusive_range(
        &self,
        range: Range<ByteOffset>,
    ) -> Result<(Range<ByteOffset>, bool), EditorError> {
        let first = self.document.line_of(range.start)?;
        let last = self.document.line_of(range.end)?;
        if last == first || range.end != self.document.line_start(last) {
            Ok((range, false))
        } else if range.start <= self.document.indent_end(first) {
            Ok((self.vim_line_range(first, last - 1), true))
        } else {
            Ok((
                range.start..self.document.line_content_range(last - 1).end,
                false,
            ))
        }
    }

    /// Remember the indent of `indent` bytes just copied before the caret.
    fn vim_note_autoindent(&mut self, indent: usize) {
        let head = self.primary_selection().head;
        self.vim.autoindent =
            (indent > 0).then(|| (self.document.revision(), ByteOffset(head.0 - indent)..head));
    }

    /// Take back the indent `o`, `O` or `cc` copied when nothing was typed
    /// after it, as Vim drops an unused autoindent. It is part of the
    /// insert, so one undo takes the line with it.
    fn vim_drop_unused_indent(&mut self) -> Result<(), EditorError> {
        match self.vim.autoindent.take() {
            Some((revision, indent)) if revision == self.document.revision() => self
                .apply_transaction(
                    Transaction::new(EditOrigin::Insert).replace(Edit::new(indent, String::new())),
                    self.vim.insert_moment,
                ),
            _ => Ok(()),
        }
    }

    fn vim_open_line(&mut self, above: bool, moment: HistoryMoment) -> Result<(), EditorError> {
        let line = self.document.line_of(self.vim_head())?;
        let content = self.document.line_content(line);
        let indent = content
            .chars()
            .take_while(|character| matches!(character, ' ' | '\t'))
            .collect::<String>();
        let ending = self.preferred_line_ending();
        let (at, text, target) = if above {
            let at = self.document.line_start(line);
            (
                at,
                format!("{indent}{ending}"),
                ByteOffset(at.0 + indent.len()),
            )
        } else {
            let at = self.document.line_content_range(line).end;
            (
                at,
                format!("{ending}{indent}"),
                ByteOffset(at.0 + ending.len() + indent.len()),
            )
        };
        self.vim_begin_insert(moment);
        self.apply_transaction(
            Transaction::new(EditOrigin::Typing)
                .replace(Edit::new(at..at, text))
                .with_selection(SelectionSet::single(target)),
            moment,
        )?;
        self.vim_note_autoindent(indent.len());
        Ok(())
    }

    /// `r{char}`: replace one character, or a count of them, without
    /// entering Insert. A count that runs past the line changes nothing,
    /// as Vim does.
    fn vim_replace(
        &mut self,
        count: usize,
        key: KeyEvent,
        moment: HistoryMoment,
    ) -> Result<(), EditorError> {
        let character = match key.code {
            KeyCode::Char(character)
                if !character.is_control()
                    && !key
                        .modifiers
                        .intersects(KeyModifiers::CONTROL | KeyModifiers::SUPER) =>
            {
                character
            }
            _ => {
                self.vim_cancel_pending();
                return Ok(());
            }
        };
        self.vim_cancel_pending();
        let head = self.vim_head();
        let line_end = self
            .document
            .line_content_range(self.document.line_of(head)?)
            .end;
        let mut end = head;
        for _ in 0..count {
            if end >= line_end {
                return Ok(());
            }
            end = self.document.next_grapheme_boundary(end)?;
        }
        let text = character.to_string().repeat(count);
        let target = ByteOffset(head.0 + (count - 1) * character.len_utf8());
        self.apply_transaction(
            Transaction::new(EditOrigin::Typing)
                .replace(Edit::new(head..end, text))
                .with_selection(SelectionSet::single(target)),
            moment,
        )?;
        self.vim_normalize_caret()?;
        Ok(())
    }

    /// Visual `r`: every selected character becomes the typed character.
    /// Line breaks stay, so a linewise selection keeps its lines.
    fn vim_replace_selection(
        &mut self,
        key: KeyEvent,
        moment: HistoryMoment,
    ) -> Result<(), EditorError> {
        let character = match key.code {
            KeyCode::Char(character)
                if !character.is_control()
                    && !key.modifiers.intersects(
                        KeyModifiers::CONTROL | KeyModifiers::SUPER | KeyModifiers::ALT,
                    ) =>
            {
                character
            }
            _ => {
                self.vim_cancel_pending();
                return Ok(());
            }
        };
        let range = self.primary_selection().ordered();
        if range.is_empty() {
            self.vim_cancel_pending();
            return Ok(());
        }
        let selected = self.document.slice(range.clone())?;
        let replacement = character.to_string();
        let text: String = selected
            .graphemes(true)
            .map(|cluster| match cluster {
                "\n" | "\r\n" | "\r" => cluster.to_owned(),
                _ => replacement.clone(),
            })
            .collect();
        self.vim.mode = Mode::Normal;
        self.vim_cancel_pending();
        self.apply_transaction(
            Transaction::new(EditOrigin::Typing)
                .replace(Edit::new(range.clone(), text))
                .with_selection(SelectionSet::single(range.start)),
            moment,
        )?;
        self.vim_normalize_caret()?;
        Ok(())
    }

    fn vim_put(
        &mut self,
        before: bool,
        count: usize,
        moment: HistoryMoment,
    ) -> Result<(), EditorError> {
        if self.vim.register.is_empty() {
            return Ok(());
        }
        let ending = self.preferred_line_ending();
        let register = normalize_paste_line_endings(&self.vim.register, &ending);
        let bytes = register.len().saturating_mul(count);
        let visual = match self.vim.mode {
            Mode::Visual { linewise, .. } => Some((self.primary_selection().ordered(), linewise)),
            _ => None,
        };
        let removed_bytes = visual
            .as_ref()
            .map_or(0, |(range, _)| range.end.0 - range.start.0);
        if bytes
            > self
                .document
                .maximum_bytes()
                .saturating_sub(self.document.len_bytes() - removed_bytes)
        {
            return Err(super::DocumentError::DocumentTooLarge {
                attempted: (self.document.len_bytes() - removed_bytes).saturating_add(bytes),
                maximum: self.document.maximum_bytes(),
            }
            .into());
        }
        let mut text = register.repeat(count);
        let head = self.vim_head();
        let line = self.document.line_of(head)?;
        let linewise_put =
            self.vim.linewise_register || visual.as_ref().is_some_and(|(_, linewise)| *linewise);
        let mut replaced_register = None;
        let (range, target) = if let Some((range, linewise)) = visual {
            let mut removed = self.document.slice(range.clone())?;
            let has_line_ending = range.start < range.end
                && range.end == self.document.line_start(self.document.line_of(range.end)?);
            if linewise && !has_line_ending {
                removed.push_str(&ending);
            }
            if !before {
                replaced_register = Some((removed, linewise));
            }
            let target = if linewise {
                if !self.vim.linewise_register && has_line_ending {
                    text.push_str(&ending);
                } else if self.vim.linewise_register && !has_line_ending {
                    text.truncate(text.len() - line_ending_len(&text));
                }
                range.start
            } else if self.vim.linewise_register {
                text.insert_str(0, &ending);
                ByteOffset(range.start.0 + ending.len())
            } else {
                ByteOffset(range.start.0 + text.len())
            };
            (range, target)
        } else if self.vim.linewise_register {
            let line = line.min(self.vim_last_line());
            let (at, prefix) = if self.document.len_bytes() == 0 {
                text.truncate(text.len() - line_ending_len(&text));
                (ByteOffset::ZERO, 0)
            } else if before {
                (self.document.line_start(line), 0)
            } else if line + 1 < self.document.line_count() {
                (self.document.line_start(line + 1), 0)
            } else {
                text.truncate(text.len() - line_ending_len(&text));
                text.insert_str(0, &ending);
                (ByteOffset(self.document.len_bytes()), ending.len())
            };
            (at..at, ByteOffset(at.0 + prefix))
        } else {
            let at = if before {
                head
            } else {
                self.document
                    .next_grapheme_boundary(head)?
                    .min(self.document.line_content_range(line).end)
            };
            (at..at, ByteOffset(at.0 + text.len()))
        };
        self.vim.mode = Mode::Normal;
        self.close_history_group();
        self.apply_transaction(
            Transaction::new(EditOrigin::Paste)
                .replace(Edit::new(range, text))
                .with_selection(SelectionSet::single(target)),
            moment,
        )?;
        if linewise_put {
            let line = self.document.line_of(target)?;
            self.vim_place_caret(self.document.indent_end(line))?;
        } else if target > ByteOffset::ZERO {
            self.vim_place_caret(self.document.previous_grapheme_boundary(target)?)?;
        }
        if let Some((register, linewise)) = replaced_register {
            self.vim_fill_register(register, linewise);
        }
        self.vim_normalize_caret()
    }
}

/// The ordinary editor motion a Shift+arrow asks for.
fn selection_motion(code: KeyCode) -> Option<super::Motion> {
    Some(match code {
        KeyCode::Left => super::Motion::Left,
        KeyCode::Right => super::Motion::Right,
        KeyCode::Up => super::Motion::Up,
        KeyCode::Down => super::Motion::Down,
        KeyCode::Home => super::Motion::LineStart,
        KeyCode::End => super::Motion::LineEnd,
        KeyCode::PageUp => super::Motion::PageUp,
        KeyCode::PageDown => super::Motion::PageDown,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    fn editor() -> Editor {
        let source = "a\n".repeat(12);
        let mut editor = Editor::new(&source).expect("editor");
        editor.set_keybindings(EditorKeybindings::Vim);
        editor.set_page_rows(6);
        editor
    }

    fn press(editor: &mut Editor, code: KeyCode, modifiers: KeyModifiers) {
        editor
            .vim_key(KeyEvent::new(code, modifiers), HistoryMoment(0), None)
            .expect("key");
    }

    fn line(editor: &Editor) -> usize {
        editor
            .document
            .line_of(editor.primary_selection().head)
            .expect("line")
    }

    /// A Vim editor on `source` with the caret at `caret`, after `keys`.
    fn typed(source: &str, caret: usize, keys: &str) -> Editor {
        let mut editor = Editor::new(source).expect("editor");
        editor.set_keybindings(EditorKeybindings::Vim);
        editor
            .set_selection(Selection::caret(ByteOffset(caret)))
            .expect("caret");
        type_keys(&mut editor, keys);
        editor
    }

    fn type_keys(editor: &mut Editor, keys: &str) {
        for character in keys.chars() {
            press(editor, KeyCode::Char(character), KeyModifiers::NONE);
        }
    }

    fn head(editor: &Editor) -> usize {
        editor.primary_selection().head.0
    }

    /// `p` puts a linewise register that ends in a Unicode line break. The
    /// break was cut by its byte count and split inside the character.
    #[test]
    fn p_puts_lines_that_end_in_a_unicode_line_break() {
        for separator in ['\u{2028}', '\u{0085}', '\u{2029}'] {
            let source = format!("a{separator}b\nc");
            let put = typed(&source, 0, "yyGp");
            assert_eq!(put.source(), format!("{source}\na"), "{separator:?}");
            let replaced = typed(&source, 0, "yyGVp");
            assert_eq!(
                replaced.source(),
                format!("a{separator}b\na"),
                "{separator:?}"
            );
            let alone = typed(&format!("a{separator}"), 0, "ddp");
            assert_eq!(alone.source(), "a", "{separator:?}");
        }
    }

    /// `dw` and `cw` stop at the end of the line where their last word ends,
    /// as in Vim. They once took the line break and the next line's start.
    #[test]
    fn dw_and_cw_stop_at_the_end_of_the_line() {
        assert_eq!(typed("foo   \nbar", 3, "dw").source(), "foo\nbar");
        assert_eq!(typed("\n\n\nbar", 0, "dw").source(), "\n\nbar");
        assert_eq!(typed("foo bar\nbaz", 0, "2dw").source(), "\nbaz");
        let mut editor = typed("\nbar", 0, "cw");
        assert!(editor.vim_is_insert());
        editor
            .dispatch(
                super::super::Command::InsertText("x".into()),
                HistoryMoment(0),
                &mut super::super::MemoryClipboard::default(),
            )
            .expect("type");
        assert_eq!(editor.source(), "x\nbar");
    }

    /// `di(` and `ci(` on a block of whole lines take the lines inside it.
    /// The line break after the opening bracket and the indent before the
    /// closing one stay, so `ci(` leaves a line to type on.
    #[test]
    fn inner_blocks_over_whole_lines_take_the_lines_inside() {
        let source = "stack(\n  s(\"bd\"),\n  s(\"hh\")\n)\n";
        assert_eq!(typed(source, 7, "di(").source(), "stack(\n)\n");
        let changed = typed(source, 7, "ci(");
        assert_eq!(changed.source(), "stack(\n  \n)\n");
        assert!(changed.vim_is_insert());
        assert_eq!(head(&changed), 9, "the caret waits after the indent");
        let yanked = typed("{\n  a\n}", 2, "yi{");
        assert_eq!(yanked.vim_register(), Some("  a\n"));
        assert!(yanked.vim.linewise_register);
    }

    /// A counted `cw` from the last character of a word changes through the
    /// words after it. Only the first word stops short, as in Vim.
    #[test]
    fn a_counted_cw_from_a_word_end_changes_every_word() {
        assert_eq!(typed("a b c", 0, "2cw").source(), " c");
        assert_eq!(typed("a b c", 0, "c2w").source(), " c");
        assert_eq!(typed("ab cd ef", 1, "2cw").source(), "a ef");
    }

    /// Character Visual `D`, `X`, `Y` and `C` take the highlighted lines
    /// whole, as Vim's do.
    #[test]
    fn visual_capital_commands_take_whole_lines() {
        for keys in ["vlD", "vlX"] {
            assert_eq!(typed("abc\ndef\n", 0, keys).source(), "def\n", "{keys}");
        }
        let yanked = typed("abc\ndef\n", 0, "vlY");
        assert_eq!(yanked.vim_register(), Some("abc\n"));
        assert!(yanked.vim.linewise_register);
        let changed = typed("abc\ndef\n", 0, "vlC");
        assert_eq!(changed.source(), "\ndef\n");
        assert!(changed.vim_is_insert());
    }

    /// `k` on the first line and `j` on the last fail, and the operator
    /// waiting for them does nothing. It once took the caret's own line.
    #[test]
    fn an_operator_with_a_blocked_j_or_k_does_nothing() {
        for (caret, keys) in [(0, "dk"), (0, "ck"), (2, "dj"), (2, "cj")] {
            let editor = typed("a\nb", caret, keys);
            assert_eq!(editor.source(), "a\nb", "{keys}");
            assert!(editor.vim_is_normal(), "{keys}");
        }
        for (caret, keys) in [(0, "yk"), (2, "yj")] {
            assert_eq!(typed("a\nb", caret, keys).vim_register(), None, "{keys}");
        }
    }

    /// The empty line after a final line break is drawn, but Vim's motions
    /// and operators end on the line before it, as Vim has no such line.
    #[test]
    fn the_line_after_a_final_line_break_is_not_a_vim_line() {
        assert_eq!(head(&typed("a\nb\n", 0, "G")), 2);
        assert_eq!(head(&typed("a\nb\n", 0, "j")), 2);
        assert_eq!(head(&typed("a\nb\n", 2, "j")), 2);
        assert_eq!(head(&typed("a\nbc\n", 2, "w")), 3);
        assert_eq!(typed("a\nb\n", 0, "Gdd").source(), "a\n");
        assert_eq!(typed("a\nb\n", 2, "dj").source(), "a\nb\n");
        assert_eq!(typed("a\nb\n", 2, "ddp").source(), "a\nb\n");
    }

    /// `$` keeps `j` and `k` at the end of each line, as Vim's goal column
    /// past the line end does.
    #[test]
    fn dollar_keeps_j_and_k_at_the_line_end() {
        let mut editor = typed("ab\nabcdef\nabc\n", 0, "$j");
        assert_eq!(head(&editor), 8);
        type_keys(&mut editor, "j");
        assert_eq!(head(&editor), 12);
        type_keys(&mut editor, "kk");
        assert_eq!(head(&editor), 1);
    }

    /// `w` and `b` stop on an empty line, as Vim counts it as a word.
    #[test]
    fn w_and_b_stop_on_an_empty_line() {
        assert_eq!(head(&typed("foo\n\nbar", 0, "w")), 4);
        assert_eq!(head(&typed("foo\n\nbar", 5, "b")), 4);
        assert_eq!(head(&typed("foo\n\n\nbar", 0, "2w")), 5);
    }

    /// A yank leaves the caret where the yanked text starts, so a yank
    /// backward moves it there, as Vim's does.
    #[test]
    fn a_yank_leaves_the_caret_at_the_start_of_the_text() {
        for (source, caret, keys, start) in [
            ("foo bar", 4, "yb", 0),
            ("foo bar", 4, "yh", 3),
            ("foo bar", 4, "y0", 0),
            ("f(ab)", 3, "yi(", 2),
            ("x \"ab\"", 4, "ya\"", 1),
            ("ab\ncd", 4, "yk", 1),
            ("ab\ncd", 4, "ygg", 0),
            ("ab\ncd", 1, "yj", 1),
            ("ab\ncd", 1, "yy", 1),
        ] {
            assert_eq!(
                head(&typed(source, caret, keys)),
                start,
                "{keys} in {source:?}"
            );
        }
    }

    /// On a line of only spaces, `^` rests on the last space and `I`
    /// inserts after them all, as Vim's do.
    #[test]
    fn caret_and_capital_i_on_a_line_of_only_spaces() {
        assert_eq!(head(&typed("    ", 0, "^")), 3);
        let editor = typed("    ", 0, "I");
        assert!(editor.vim_is_insert());
        assert_eq!(head(&editor), 4);
    }

    /// `o` copies the line's indent. Leaving Insert with nothing typed takes
    /// it away again, as Vim drops an unused autoindent.
    #[test]
    fn escape_after_o_drops_the_unused_indent() {
        let mut editor = typed("  foo\n", 0, "o");
        assert_eq!(editor.source(), "  foo\n  \n");
        press(&mut editor, KeyCode::Esc, KeyModifiers::NONE);
        assert_eq!(editor.source(), "  foo\n\n");
        assert_eq!(head(&editor), 6);
        type_keys(&mut editor, "u");
        assert_eq!(editor.source(), "  foo\n", "one undo takes the whole line");

        let mut typed_on = typed("  foo\n", 0, "o");
        typed_on
            .dispatch(
                super::super::Command::InsertText("x".into()),
                HistoryMoment(0),
                &mut super::super::MemoryClipboard::default(),
            )
            .expect("type");
        press(&mut typed_on, KeyCode::Esc, KeyModifiers::NONE);
        assert_eq!(typed_on.source(), "  foo\n  x\n", "a used indent stays");

        // A chord or a click between closes the undo step. The indent then
        // stays, so one undo still takes the whole line.
        let mut interrupted = typed("  foo\n", 0, "o");
        interrupted.interrupt_key_mode();
        press(&mut interrupted, KeyCode::Esc, KeyModifiers::NONE);
        type_keys(&mut interrupted, "u");
        assert_eq!(
            interrupted.source(),
            "  foo\n",
            "one undo after an interrupt"
        );
    }

    /// Esc resets the column j and k aim for even when the caret does not
    /// move, so j from an empty line lands at the start of the next one.
    #[test]
    fn escape_resets_the_column_j_aims_for() {
        let mut editor = typed("abcdef\n\nabcdefgh\n", 0, "A");
        // Insert hands ↓ to the editor's own command, as the studio does.
        let down = editor
            .vim_insert_command(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE))
            .expect("↓ in Insert");
        editor
            .dispatch(
                down,
                HistoryMoment(0),
                &mut super::super::MemoryClipboard::default(),
            )
            .expect("down");
        assert_eq!(head(&editor), 7, "↓ reached the empty line");
        press(&mut editor, KeyCode::Esc, KeyModifiers::NONE);
        type_keys(&mut editor, "j");
        assert_eq!(head(&editor), 8, "j kept the column from before Esc");
    }

    /// The wheel scrolls a Vim score as it scrolls any other. The input hook
    /// after it once brought the caret back into view, so every step of a
    /// trackpad swipe was undone before the next one.
    #[test]
    fn the_wheel_scrolls_away_from_the_caret_in_normal_mode() {
        use crossterm::event::{MouseEvent, MouseEventKind};
        let mut editor = editor();
        let area = crate::editor::GridRect::new(0, 0, 20, 6);
        for _ in 0..2 {
            let map = editor.screen_map(area).expect("map");
            editor
                .mouse_event(
                    MouseEvent {
                        kind: MouseEventKind::ScrollDown,
                        column: 0,
                        row: 0,
                        modifiers: KeyModifiers::NONE,
                    },
                    &map,
                    HistoryMoment(0),
                )
                .expect("wheel");
            editor.normalize_after_input().expect("normalize");
        }
        assert_eq!(line(&editor), 0, "the caret stays where it was");
        assert!(
            editor.viewport().top_row > 0,
            "the view stays where the wheel put it"
        );
    }

    #[test]
    fn a_selection_outside_visual_is_adopted() {
        let mut editor = Editor::new("hello world\n").expect("editor");
        editor.set_keybindings(EditorKeybindings::Vim);
        editor
            .set_selection(Selection::range(ByteOffset(0), ByteOffset(5)))
            .expect("selection");
        press(&mut editor, KeyCode::Char('d'), KeyModifiers::NONE);
        assert_eq!(editor.source(), " world\n");
        assert_eq!(editor.vim.mode, Mode::Normal);
    }

    #[test]
    fn v_keeps_a_selection_made_outside_visual() {
        let mut editor = Editor::new("hello world\n").expect("editor");
        editor.set_keybindings(EditorKeybindings::Vim);
        editor
            .set_selection(Selection::range(ByteOffset(0), ByteOffset(5)))
            .expect("selection");
        press(&mut editor, KeyCode::Char('v'), KeyModifiers::NONE);
        assert!(matches!(editor.vim.mode, Mode::Visual { .. }));
        assert_eq!(
            editor.primary_selection().ordered(),
            ByteOffset(0)..ByteOffset(5)
        );
    }

    #[test]
    fn i_with_a_selection_still_enters_insert() {
        let mut editor = Editor::new("hello world\n").expect("editor");
        editor.set_keybindings(EditorKeybindings::Vim);
        editor
            .set_selection(Selection::range(ByteOffset(0), ByteOffset(5)))
            .expect("selection");
        press(&mut editor, KeyCode::Char('i'), KeyModifiers::NONE);
        assert!(editor.vim_is_insert());
    }

    #[test]
    fn a_control_chord_does_not_adopt_a_selection() {
        let mut editor = Editor::new("hello world\n").expect("editor");
        editor.set_keybindings(EditorKeybindings::Vim);
        editor
            .set_selection(Selection::range(ByteOffset(0), ByteOffset(5)))
            .expect("selection");
        press(&mut editor, KeyCode::Char('c'), KeyModifiers::CONTROL);
        assert!(editor.vim_is_normal(), "Ctrl+C keeps the Normal mode");
        assert_eq!(
            editor.primary_selection().ordered(),
            ByteOffset(0)..ByteOffset(5),
            "the copy chord leaves the band for the editor to copy"
        );
    }

    #[test]
    fn visual_r_replaces_every_selected_character_and_keeps_line_breaks() {
        let mut editor = Editor::new("ab\ncd\n").expect("editor");
        editor.set_keybindings(EditorKeybindings::Vim);
        editor
            .set_selection(Selection::range(ByteOffset(0), ByteOffset(5)))
            .expect("selection");
        press(&mut editor, KeyCode::Char('v'), KeyModifiers::NONE);
        press(&mut editor, KeyCode::Char('r'), KeyModifiers::NONE);
        press(&mut editor, KeyCode::Char('x'), KeyModifiers::NONE);
        assert_eq!(editor.source(), "xx\nxx\n");
        assert_eq!(editor.vim.mode, Mode::Normal);
    }

    #[test]
    fn backspace_deletes_a_selection() {
        let mut editor = Editor::new("hello world\n").expect("editor");
        editor.set_keybindings(EditorKeybindings::Vim);
        editor
            .set_selection(Selection::range(ByteOffset(0), ByteOffset(5)))
            .expect("selection");
        press(&mut editor, KeyCode::Backspace, KeyModifiers::NONE);
        assert_eq!(editor.source(), " world\n");
        assert_eq!(editor.vim.mode, Mode::Normal);
    }

    #[test]
    fn backspace_without_a_selection_still_moves_left() {
        let mut editor = Editor::new("hello world\n").expect("editor");
        editor.set_keybindings(EditorKeybindings::Vim);
        editor
            .set_selection(Selection::caret(ByteOffset(5)))
            .expect("caret");
        press(&mut editor, KeyCode::Backspace, KeyModifiers::NONE);
        assert_eq!(editor.primary_selection().head, ByteOffset(4));
    }

    #[test]
    fn shift_right_enters_visual() {
        let mut editor = Editor::new("hello world\n").expect("editor");
        editor.set_keybindings(EditorKeybindings::Vim);
        press(&mut editor, KeyCode::Right, KeyModifiers::SHIFT);
        assert!(editor.vim_is_visual());
        assert!(!editor.primary_selection().is_empty());
        press(&mut editor, KeyCode::Char('d'), KeyModifiers::NONE);
        assert_eq!(editor.source(), "ello world\n");
    }

    #[test]
    fn ctrl_d_moves_half_a_page_and_ctrl_u_returns() {
        let mut editor = editor();
        assert_eq!(editor.viewport().page_rows, 6);
        press(&mut editor, KeyCode::Char('d'), KeyModifiers::CONTROL);
        assert_eq!(line(&editor), 3, "half of six rows");
        assert_eq!(editor.viewport().top_row, 3, "the view scrolls too");
        press(&mut editor, KeyCode::Char('u'), KeyModifiers::CONTROL);
        assert_eq!(line(&editor), 0);
        assert_eq!(editor.viewport().top_row, 0);
    }

    #[test]
    fn a_count_moves_that_many_lines() {
        let mut editor = editor();
        press(&mut editor, KeyCode::Char('2'), KeyModifiers::NONE);
        press(&mut editor, KeyCode::Char('d'), KeyModifiers::CONTROL);
        assert_eq!(line(&editor), 2);
        assert_eq!(editor.viewport().top_row, 2);
    }

    #[test]
    fn ctrl_d_extends_a_visual_selection() {
        let mut editor = editor();
        press(&mut editor, KeyCode::Char('v'), KeyModifiers::NONE);
        press(&mut editor, KeyCode::Char('d'), KeyModifiers::CONTROL);
        let selection = editor.primary_selection();
        assert_eq!(selection.anchor, ByteOffset(0));
        assert_eq!(line(&editor), 3, "the selection reaches the third line");
        assert!(selection.head.0 >= 6);
    }

    #[test]
    fn j_and_d_continue_from_where_ctrl_d_took_the_visual_head() {
        let mut editor = editor();
        press(&mut editor, KeyCode::Char('V'), KeyModifiers::NONE);
        press(&mut editor, KeyCode::Char('d'), KeyModifiers::CONTROL);
        press(&mut editor, KeyCode::Char('j'), KeyModifiers::NONE);
        press(&mut editor, KeyCode::Char('d'), KeyModifiers::NONE);
        assert_eq!(
            editor.source(),
            "a\n".repeat(7),
            "V, Ctrl+D and j cover five lines"
        );
    }

    #[test]
    fn ctrl_shift_arrows_are_left_to_the_studio() {
        let mut editor = editor();
        for code in [
            KeyCode::Up,
            KeyCode::Down,
            KeyCode::Left,
            KeyCode::Right,
            KeyCode::Home,
            KeyCode::End,
        ] {
            let handled = editor
                .vim_key(
                    KeyEvent::new(code, KeyModifiers::CONTROL | KeyModifiers::SHIFT),
                    HistoryMoment(0),
                    None,
                )
                .expect("key");
            assert!(!handled, "Ctrl+Shift+{code:?} reaches the studio's chords");
            assert!(editor.vim_is_normal(), "Ctrl+Shift+{code:?} left Normal");
            assert!(
                editor.primary_selection().is_empty(),
                "Ctrl+Shift+{code:?} made a selection"
            );
        }
    }

    /// Esc and the function keys move no caret, so a view the wheel
    /// scrolled stays where it is. Both once pulled it back to the caret.
    /// A motion still brings the caret into view.
    #[test]
    fn keys_that_move_no_caret_keep_a_wheel_scrolled_view() {
        use crossterm::event::{MouseEvent, MouseEventKind};
        let mut editor = editor();
        let area = crate::editor::GridRect::new(0, 0, 20, 6);
        for _ in 0..2 {
            let map = editor.screen_map(area).expect("map");
            editor
                .mouse_event(
                    MouseEvent {
                        kind: MouseEventKind::ScrollDown,
                        column: 0,
                        row: 0,
                        modifiers: KeyModifiers::NONE,
                    },
                    &map,
                    HistoryMoment(0),
                )
                .expect("wheel");
            editor.normalize_after_input().expect("normalize");
        }
        let top = editor.viewport().top_row;
        assert!(top > 0, "the wheel scrolled");
        for code in [KeyCode::F(5), KeyCode::F(8), KeyCode::Esc] {
            press(&mut editor, code, KeyModifiers::NONE);
            editor.normalize_after_input().expect("normalize");
            assert_eq!(editor.viewport().top_row, top, "{code:?} moved the view");
        }
        press(&mut editor, KeyCode::Char('j'), KeyModifiers::NONE);
        let view = editor.viewport();
        assert_eq!(line(&editor), 1);
        assert!(
            view.top_row <= 1 && 1 < view.top_row + view.page_rows,
            "a motion brings the caret into view"
        );
    }

    /// A mouse selection made in V-LINE covers the characters it covers,
    /// and Visual becomes character-wise to match. V-LINE once stayed on,
    /// so `d` mixed the mouse's range with whole lines: it deleted the
    /// wrong text, or overflowed on a last line with no line break.
    #[test]
    fn a_mouse_selection_in_visual_line_becomes_character_wise() {
        use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
        for (source, from, to, left, register) in [
            ("abc\ndef", 1, 7, "a", "bc\ndef"),
            ("abc\ndef\n", 5, 6, "abc\ndf\n", "e"),
        ] {
            let mut editor = Editor::new(source).expect("editor");
            editor.set_keybindings(EditorKeybindings::Vim);
            editor.set_page_rows(6);
            press(&mut editor, KeyCode::Char('V'), KeyModifiers::NONE);
            let area = crate::editor::GridRect::new(0, 0, 20, 6);
            for (kind, offset) in [
                (MouseEventKind::Down(MouseButton::Left), from),
                (MouseEventKind::Drag(MouseButton::Left), to),
                (MouseEventKind::Up(MouseButton::Left), to),
            ] {
                let map = editor.screen_map(area).expect("map");
                let cell = map.cell_for_offset(ByteOffset(offset)).expect("cell");
                editor
                    .mouse_event(
                        MouseEvent {
                            kind,
                            column: cell.x,
                            row: cell.y,
                            modifiers: KeyModifiers::NONE,
                        },
                        &map,
                        HistoryMoment(0),
                    )
                    .expect("mouse");
            }
            assert_eq!(
                editor.vim_label().as_deref(),
                Some("VISUAL"),
                "{source:?}: the mode matches the selection"
            );
            press(&mut editor, KeyCode::Char('d'), KeyModifiers::NONE);
            assert_eq!(editor.source(), left, "{source:?}");
            assert_eq!(editor.vim.register, register, "{source:?}");
            assert!(!editor.vim.linewise_register, "{source:?}");
        }
    }

    /// `u` after a Visual change puts the caret where the change began,
    /// as Vim does, and so does `:undo`. Both once left it at the end of
    /// the old selection.
    #[test]
    fn undo_after_a_visual_change_puts_the_caret_at_its_start() {
        let mut editor = Editor::new("abc def").expect("editor");
        editor.set_keybindings(EditorKeybindings::Vim);
        for character in ['v', 'e', 'd'] {
            press(&mut editor, KeyCode::Char(character), KeyModifiers::NONE);
        }
        assert_eq!(editor.source(), " def");
        press(&mut editor, KeyCode::Char('u'), KeyModifiers::NONE);
        assert_eq!(editor.source(), "abc def");
        assert_eq!(editor.primary_selection(), Selection::caret(ByteOffset(0)));
        press(&mut editor, KeyCode::Char('r'), KeyModifiers::CONTROL);
        assert_eq!(editor.source(), " def");
        for character in [':', 'u'] {
            press(&mut editor, KeyCode::Char(character), KeyModifiers::NONE);
        }
        press(&mut editor, KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(editor.source(), "abc def");
        assert_eq!(editor.primary_selection(), Selection::caret(ByteOffset(0)));
    }
}
