use std::sync::Arc;

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

use super::super::{Command, HistoryMoment, KeyboardCapabilities, key_to_command};
use super::{Editor, EditorKeybindings, Mode};
use crate::vim_config::{VimMappedKey, VimMappingMode, VimMappings, canonical_key};

const SEQUENCE_TIMEOUT_MS: u64 = 1_000;

#[derive(Clone, Debug, Default)]
pub(in crate::editor) struct RemapState {
    mappings: Arc<VimMappings>,
    pending: Vec<KeyEvent>,
    last_key: HistoryMoment,
}

impl RemapState {
    pub(in crate::editor) fn clear_pending(&mut self) {
        self.pending.clear();
    }
}

impl Editor {
    pub(crate) fn set_vim_mappings(&mut self, mappings: Arc<VimMappings>) {
        if !Arc::ptr_eq(&self.vim_remap.mappings, &mappings)
            && self.vim_remap.pending.is_empty()
            && !self.vim_has_pending_command()
        {
            self.vim_remap.mappings = mappings;
        }
    }

    pub(crate) fn vim_has_pending_command(&self) -> bool {
        self.find_open()
            || self.vim.count != 0
            || self.vim.operator.is_some()
            || self.vim.replace.is_some()
            || self.vim.g
            || self.vim.command.is_some()
            || self.vim.text_object.is_some()
    }

    fn vim_mapping_mode(&self) -> Option<VimMappingMode> {
        // The key after r, g, or a text object's i or a is read as itself,
        // as Vim reads it. gg and r{char} never wait for a mapping.
        if self.vim.keybindings != EditorKeybindings::Vim
            || self.vim.command.is_some()
            || self.vim.replace.is_some()
            || self.vim.g
            || self.vim.text_object.is_some()
            || self.find_open()
        {
            return None;
        }
        Some(if self.vim.operator.is_some() {
            VimMappingMode::OperatorPending
        } else {
            match self.vim.mode {
                Mode::Normal => VimMappingMode::Normal,
                Mode::Insert => VimMappingMode::Insert,
                Mode::Visual { .. } => VimMappingMode::Visual,
            }
        })
    }

    pub(crate) fn vim_queue_mapped_key(&mut self, key: KeyEvent, moment: HistoryMoment) -> bool {
        // A terminal that reports every key sends Shift and Ctrl presses of
        // their own. They are not part of a mapped sequence.
        if key.kind == KeyEventKind::Release || matches!(key.code, KeyCode::Modifier(_)) {
            return false;
        }
        let canonical = canonical_key(key);
        let starts_mapping = self.vim_mapping_mode().is_some_and(|mode| {
            self.vim_remap
                .mappings
                .for_mode(mode)
                .iter()
                .any(|mapping| mapping.keys.first() == Some(&canonical))
        });
        if self.vim_remap.pending.is_empty() && !starts_mapping {
            return false;
        }
        self.vim_remap.pending.push(key);
        self.vim_remap.last_key = moment;
        true
    }

    /// Replacement keys bypass mappings, so self-maps and cycles cannot recurse.
    pub(crate) fn vim_take_mapped_keys(
        &mut self,
        force: bool,
    ) -> Option<(Vec<VimMappedKey>, Vec<KeyEvent>)> {
        if self.vim_remap.pending.is_empty() {
            return None;
        }
        let mappings = self
            .vim_mapping_mode()
            .map_or(&[][..], |mode| self.vim_remap.mappings.for_mode(mode));
        let pending: Vec<_> = self
            .vim_remap
            .pending
            .iter()
            .copied()
            .map(canonical_key)
            .collect();
        if !force
            && mappings.iter().any(|mapping| {
                mapping.keys.len() > pending.len() && mapping.keys.starts_with(&pending)
            })
        {
            return None;
        }
        if let Some(mapping) = mappings
            .iter()
            .filter(|mapping| pending.starts_with(&mapping.keys))
            .max_by_key(|mapping| mapping.keys.len())
        {
            let keys = mapping.replacement.clone();
            self.vim_remap.pending.drain(..mapping.keys.len());
            Some((keys, std::mem::take(&mut self.vim_remap.pending)))
        } else {
            // No mapping can match this sequence. Its first key goes out
            // as a built-in key, and the rest go back through the
            // mappings, where a mapping that follows still runs.
            let key = self.vim_remap.pending.remove(0);
            Some((
                vec![VimMappedKey::Key(key)],
                std::mem::take(&mut self.vim_remap.pending),
            ))
        }
    }

    pub(crate) fn vim_mapping_timed_out(&self, moment: HistoryMoment) -> bool {
        !self.vim_remap.pending.is_empty()
            && moment.0.saturating_sub(self.vim_remap.last_key.0) >= SEQUENCE_TIMEOUT_MS
    }

    pub(crate) fn vim_insert_command(&self, mut key: KeyEvent) -> Option<Command> {
        if self.vim.keybindings != EditorKeybindings::Vim
            || self.vim.mode != Mode::Insert
            || key.modifiers.intersects(
                KeyModifiers::CONTROL
                    | KeyModifiers::ALT
                    | KeyModifiers::SUPER
                    | KeyModifiers::HYPER
                    | KeyModifiers::META,
            )
        {
            return None;
        }
        if key.code == KeyCode::Tab && key.modifiers.contains(KeyModifiers::SHIFT) {
            key.code = KeyCode::BackTab;
        }
        key_to_command(key, KeyboardCapabilities::enhanced())
    }

    pub(crate) fn claims_key(&self, key: &KeyEvent) -> bool {
        if self.vim.keybindings != EditorKeybindings::Vim || key.kind == KeyEventKind::Release {
            return false;
        }
        let key = canonical_key(*key);
        if !self.vim_remap.pending.is_empty() || self.vim_has_pending_command() {
            return true;
        }
        if self.vim_mapping_mode().is_some_and(|mode| {
            self.vim_remap
                .mappings
                .for_mode(mode)
                .iter()
                .any(|mapping| mapping.keys.first() == Some(&key))
        }) {
            return true;
        }
        if key.modifiers == KeyModifiers::CONTROL
            && (matches!(key.code, KeyCode::Char('r' | '['))
                || (matches!(key.code, KeyCode::Char('c' | 'C')) && self.vim.mode == Mode::Insert))
        {
            return true;
        }
        if key.modifiers == KeyModifiers::CONTROL
            && matches!(key.code, KeyCode::Char('d' | 'u'))
            && self.vim.mode != Mode::Insert
        {
            return true;
        }
        if key.modifiers.intersects(
            KeyModifiers::CONTROL
                | KeyModifiers::ALT
                | KeyModifiers::SUPER
                | KeyModifiers::HYPER
                | KeyModifiers::META,
        ) {
            return false;
        }
        key.code == KeyCode::Esc
            || if self.vim.mode == Mode::Insert {
                self.vim_insert_command(key).is_some()
            } else {
                matches!(
                    key.code,
                    KeyCode::Char(_)
                        | KeyCode::Left
                        | KeyCode::Right
                        | KeyCode::Up
                        | KeyCode::Down
                        | KeyCode::Home
                        | KeyCode::End
                        | KeyCode::Backspace
                        | KeyCode::Delete
                        | KeyCode::Enter
                        | KeyCode::Tab
                        | KeyCode::BackTab
                )
            }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::editor::{ByteOffset, Editor, HistoryMoment};
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use std::sync::Arc;

    fn key(character: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(character), KeyModifiers::NONE)
    }

    /// A prefix that is no mapping sends its first key alone and reads the
    /// rest again, so `jjk` types one j and then leaves Insert.
    #[test]
    fn an_unmatched_prefix_sends_one_key_and_reads_the_rest_again() {
        let mut editor = Editor::new("one\n").expect("editor");
        editor.set_keybindings(EditorKeybindings::Vim);
        editor.set_vim_mappings(Arc::new(
            VimMappings::parse("inoremap jk <Esc>\n").expect("mapping"),
        ));
        editor
            .vim_key(key('i'), HistoryMoment(0), None)
            .expect("Insert");
        assert!(editor.vim_queue_mapped_key(key('j'), HistoryMoment(1)));
        assert!(editor.vim_queue_mapped_key(key('j'), HistoryMoment(2)));
        let (mapped, remaining) = editor
            .vim_take_mapped_keys(false)
            .expect("jj is not a mapping");
        assert_eq!(mapped, vec![VimMappedKey::Key(key('j'))], "one j types");
        assert_eq!(remaining, vec![key('j')], "the other j is read again");
        assert!(editor.vim_queue_mapped_key(key('j'), HistoryMoment(2)));
        assert!(editor.vim_queue_mapped_key(key('k'), HistoryMoment(3)));
        let (mapped, _) = editor.vim_take_mapped_keys(false).expect("jk is a mapping");
        assert_eq!(
            mapped,
            vec![VimMappedKey::Key(KeyEvent::new(
                KeyCode::Esc,
                KeyModifiers::NONE
            ))],
            "jk leaves Insert"
        );
    }

    /// The key after g or r is read as itself, as Vim reads it: gg does
    /// not wait beside a `gs` mapping, and r replaces with a mapped key.
    #[test]
    fn the_key_after_g_or_r_is_not_mapped() {
        let mut editor = Editor::new("one\ntwo\n").expect("editor");
        editor.set_keybindings(EditorKeybindings::Vim);
        editor.set_vim_mappings(Arc::new(
            VimMappings::parse("nnoremap gs <Action:stop>\nnnoremap Q dd\nvnoremap Q dd\n")
                .expect("mappings"),
        ));
        editor.vim_key(key('j'), HistoryMoment(0), None).expect("j");
        editor.vim_key(key('g'), HistoryMoment(1), None).expect("g");
        assert!(
            !editor.vim_queue_mapped_key(key('g'), HistoryMoment(2)),
            "the second g does not wait for gs"
        );
        editor
            .vim_key(key('g'), HistoryMoment(2), None)
            .expect("gg");
        assert_eq!(editor.primary_selection().head, ByteOffset(0), "gg");

        editor.vim_key(key('r'), HistoryMoment(3), None).expect("r");
        assert!(
            !editor.vim_queue_mapped_key(key('Q'), HistoryMoment(4)),
            "the Q after r is not the Q mapping"
        );
        editor
            .vim_key(key('Q'), HistoryMoment(4), None)
            .expect("rQ");
        assert_eq!(editor.source(), "Qne\ntwo\n");

        for character in ['v', 'e', 'r'] {
            editor
                .vim_key(key(character), HistoryMoment(5), None)
                .expect("key");
        }
        assert!(
            !editor.vim_queue_mapped_key(key('Q'), HistoryMoment(6)),
            "the Q after Visual r is not the Q mapping"
        );
        editor
            .vim_key(key('Q'), HistoryMoment(6), None)
            .expect("Visual rQ");
        assert_eq!(editor.source(), "QQQ\ntwo\n");
    }
}
