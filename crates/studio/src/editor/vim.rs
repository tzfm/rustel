mod motion;
pub(super) mod remap;
mod text_object;

use motion::motion_for;
use text_object::TextObjectKind;

use std::ops::Range;

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use unicode_segmentation::UnicodeSegmentation;

use super::{
    ByteOffset, Edit, EditOrigin, Editor, EditorError, FindDirection, HistoryMoment, Selection,
    SelectionSet, Transaction, normalize_paste_line_endings,
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
    insert_moment: HistoryMoment,
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
            )
        {
            // Insert mode keeps one undo step across pauses and line breaks.
            transaction.origin = EditOrigin::Control;
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
            self.vim_replace(count, key, moment)?;
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
            KeyCode::Left | KeyCode::Backspace => 'h',
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
                self.vim_apply_operator(operator, range, linewise, moment)?;
            } else if let Some((pending, count)) = self.vim.operator.take() {
                if pending == operator {
                    let count = count.saturating_mul(self.vim_take_count());
                    let line = self.document.line_of(self.vim_head())?;
                    let range = self.vim_line_range(line, line.saturating_add(count - 1));
                    self.vim_apply_operator(operator, range, true, moment)?;
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
                    'I' => self.document.first_non_whitespace(line),
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
                let (range, linewise) = if let Mode::Visual { linewise, .. } = self.vim.mode {
                    (self.primary_selection().ordered(), linewise)
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
                self.vim_apply_operator(operator, range, linewise, moment)?;
            }
            'p' | 'P' => self.vim_put(character == 'P', count, moment)?,
            'r' if self.vim.mode == Mode::Normal => {
                self.vim.replace = Some(count);
            }
            'u' => {
                self.vim_escape()?;
                for _ in 0..count.min(self.undo_depth()) {
                    self.undo()?;
                }
                self.vim_normalize_caret()?;
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
                    "u" | "undo" => {
                        self.undo()?;
                        self.vim_normalize_caret()?;
                    }
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
            KeyCode::Char(character) => {
                if let Some(command) = self.vim.command.as_mut()
                    && command.len() < 64
                    && !character.is_control()
                {
                    command.push(character);
                }
            }
            _ => {}
        }
        Ok(())
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
        let head = self.vim_head();
        let target = if self.vim.mode == Mode::Insert {
            let start = self.document.line_start(self.document.line_of(head)?);
            self.document.previous_grapheme_boundary(head)?.max(start)
        } else {
            head
        };
        self.vim.mode = Mode::Normal;
        self.vim_cancel_pending();
        self.vim_place_caret(target)?;
        self.vim_normalize_caret()
    }

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
        self.ensure_primary_visible();
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

    fn vim_apply_operator(
        &mut self,
        operator: Operator,
        mut range: Range<ByteOffset>,
        linewise: bool,
        moment: HistoryMoment,
    ) -> Result<(), EditorError> {
        self.vim_cancel_pending();
        let mut register = self.document.slice(range.clone())?;
        let ending = self.preferred_line_ending();
        let has_line_ending = range.start < range.end
            && range.end == self.document.line_start(self.document.line_of(range.end)?);
        if linewise && !has_line_ending {
            register.push_str(&ending);
        }
        let original_head = if matches!(self.vim.mode, Mode::Visual { .. }) {
            range.start
        } else {
            self.vim_head()
        };
        self.vim.mode = Mode::Normal;
        if (range.is_empty() && !linewise || self.document.len_bytes() == 0)
            && operator != Operator::Change
        {
            self.vim_place_caret(original_head)?;
            return self.vim_normalize_caret();
        }
        if operator == Operator::Yank {
            self.vim.register = register;
            self.vim.linewise_register = linewise;
            self.vim_place_caret(original_head)?;
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
        self.vim.register = register;
        self.vim.linewise_register = linewise;
        if operator != Operator::Change {
            self.vim_normalize_caret()?;
            if linewise {
                let line = self.document.line_of(self.vim_head())?;
                self.vim_place_caret(self.document.first_non_whitespace(line))?;
            }
        }
        Ok(())
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
        )
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
                    text.truncate(text.len().saturating_sub(ending.len()));
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
            let (at, prefix) = if self.document.len_bytes() == 0 {
                text.truncate(text.len().saturating_sub(ending.len()));
                (ByteOffset::ZERO, 0)
            } else if before {
                (self.document.line_start(line), 0)
            } else if line + 1 < self.document.line_count() {
                (self.document.line_start(line + 1), 0)
            } else {
                text.truncate(text.len().saturating_sub(ending.len()));
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
            self.vim_place_caret(self.document.first_non_whitespace(line))?;
        } else if target > ByteOffset::ZERO {
            self.vim_place_caret(self.document.previous_grapheme_boundary(target)?)?;
        }
        if let Some((register, linewise)) = replaced_register {
            self.vim.register = register;
            self.vim.linewise_register = linewise;
        }
        self.vim_normalize_caret()
    }
}
