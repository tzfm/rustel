//! The reference column beside the score. This file covers opening it on the
//! word under the caret (Ctrl+D), handling its keys, clicks, wheel scrolling
//! and drag text selection, dragging the generator tab's control rails, and
//! writing a chosen name or snippet into the score. It also keeps the samples
//! tab's catalogue up to date: it refreshes and installs the catalogue, reads
//! the score's samples() imports, adds input channel variants and reveals the
//! file behind a sound. Bank machine ranking lives in `bank_context.rs`.

use super::*;

/// How often the samples tab re-reads what the library knows while open.
pub(super) const CATALOGUE_REFRESH: Duration = Duration::from_millis(1_000);

/// Cached manifests normally arrive in a few milliseconds. While they are
/// arriving, follow them on the next interactive frame instead of leaving the
/// browser on its loading row for a full second.
pub(super) const CATALOGUE_LOADING_REFRESH: Duration = Duration::from_millis(50);

/// The browser's `in` row grows one variant a channel once an input is
/// open, so `s("in:1")` is a row to pick, not a number to know.
pub(super) fn with_input_channels(
    mut sounds: Vec<rustel_runtime::samples::SoundEntry>,
    input_channels: usize,
) -> Vec<rustel_runtime::samples::SoundEntry> {
    if let Some(input) = sounds
        .iter_mut()
        .find(|sound| sound.origin == rustel_runtime::samples::SoundOrigin::Input)
    {
        input.variants = input_channels.max(1);
    }
    sounds
}

/// The `samples(…)` imports a score names and where each stands, for the
/// samples browser to list them - one the library has not been asked for
/// yet counts as on its way: the checker asks when it next runs.
pub(super) fn browser_imports(
    source: &str,
    catalogue: &super::super::catalogue::Snapshot,
) -> Vec<(String, rustel_runtime::samples::SourceState)> {
    let mut imports: Vec<(String, rustel_runtime::samples::SourceState)> = Vec::new();
    for spec in rustel_runtime::sounds::samples_specs(source)
        .into_iter()
        .flatten()
    {
        if imports.iter().any(|(known, _)| *known == spec) {
            continue;
        }
        let state = catalogue.source_state(&spec);
        imports.push((spec, state));
    }
    imports
}

/// What a key on the reference column comes to.
enum ReferenceKey {
    /// Taken, with nothing left to do.
    Handled,
    /// Not the column's key: it falls through.
    NotHandled,
    /// Taken, with this action for the app to carry out.
    Apply(PanelAction),
}

impl App {
    #[cfg(feature = "hydra")]
    pub(super) fn drag_generator_control(&mut self, index: usize, rail: Rect, x: u16) {
        if rail.width < 3 {
            return;
        }
        let value = (f64::from(x.saturating_sub(rail.x).min(rail.width - 1))
            / f64::from(rail.width - 1)
            * 100.0)
            .round() as u8;
        if let Some(panel) = self
            .reference_panel
            .as_mut()
            .filter(|p| p.tab == Tab::Generator)
        {
            panel.selection = None;
            if panel.generator.set(index, value) {
                self.queue_generator_preview();
            }
        }
    }

    /// A drag on the snippet code's scrollbar moves the code as far as the
    /// pointer has moved from `row`, the row the press took the thumb at,
    /// with the code at `scroll`.
    #[cfg(feature = "hydra")]
    pub(super) fn drag_snippet_code_scroll(&mut self, row: u16, scroll: usize, y: u16) {
        let inner = inner_area(self.regions.reference);
        if let Some(panel) = self.reference_panel.as_mut()
            && let Some((rail, _, length, last)) = panel.snippet_scrollbar(inner)
        {
            let travel = (rail.height - length).max(1);
            let delta = (f64::from(y) - f64::from(row)) * last as f64 / f64::from(travel);
            panel
                .snippet_code_scroll
                .set((scroll as f64 + delta).round().clamp(0.0, last as f64) as usize);
            panel.selection = None;
        }
        self.dirty_frame = true;
    }

    /// A snippet from the reference lands the way a paste does: whole, at
    /// the caret, on lines of its own - never wrapped in the quotes or the
    /// call a name would be, and never replacing the word the panel was
    /// opened on. The panel closes and the score has the keyboard.
    fn paste_snippet(&mut self, code: String) -> Result<(), RuntimeError> {
        self.stop_preview();
        self.set_reference_panel(None);
        let anchor = self.reference_anchor.take();
        self.invalidate_maps();
        self.focus = Focus::Editor;
        self.armed_slider = None;
        self.drop_pane_selection();
        // The word the panel was opened on was reaching for this: it goes,
        // the way a name replaces it, as long as it is still there.
        let replace = anchor
            .filter(|anchor| anchor.scene == self.scenes.current().id)
            .and_then(|anchor| {
                let range = self.anchor_range_now(&anchor)?;
                let still = self
                    .editor()
                    .document()
                    .slice(ByteOffset(range.start)..ByteOffset(range.end))
                    .ok()?;
                (!anchor.word.is_empty() && still == anchor.word).then_some(range)
            });
        // On a line of its own: a snippet pasted mid-line would splice
        // itself into whatever the caret was in.
        let at = replace
            .as_ref()
            .map_or(self.editor().primary_selection().head.0, |range| {
                range.start
            });
        let source = self.editor().source();
        let line_start = source[..at].rfind('\n').map_or(0, |at| at + 1);
        let mid_line = !source[line_start..at].trim().is_empty();
        let text = if mid_line { format!("\n{code}") } else { code };
        match replace {
            Some(range) => self.dispatch_editor(Command::ReplaceRange {
                from: ByteOffset(range.start),
                to: ByteOffset(range.end),
                text,
            }),
            None => self.dispatch_editor(Command::PasteText(text)),
        }
    }

    /// Confirm a reference choice, then leave function documentation visible
    /// while the editor receives the argument. Value pickers still finish their
    /// insertion normally; a sound named after a function is not a function call.
    pub(super) fn confirm_reference_name(&mut self, name: String) -> Result<(), RuntimeError> {
        let entry = self.reference_panel.as_ref().and_then(|panel| {
            (panel.tab == Tab::Reference && panel.vocabulary.is_none())
                .then(|| self.reference.lookup(&name))
                .flatten()
        });
        let call = entry
            .and_then(|index| self.reference.entry(index))
            .is_some_and(|entry| entry.inserts_as_call());
        if call {
            if let Some(anchor) = self.reference_anchor.as_mut() {
                anchor.landing = Landing::Call;
            } else {
                let selection = self.editor().primary_selection();
                let range = if selection.is_empty() {
                    self.chain_position_at_caret()
                        .or_else(|| {
                            self.word_range_at_caret()
                                .map(|(_, range)| range)
                                .filter(|range| {
                                    range.start < selection.head && selection.head <= range.end
                                })
                        })
                        .unwrap_or_else(|| selection.ordered())
                } else {
                    selection.ordered()
                };
                let word = self
                    .editor()
                    .document()
                    .slice(range.clone())
                    .unwrap_or_default();
                self.anchor_reference_as(&word, range, Landing::Call);
            }
        } else if entry.is_some()
            && let Some(anchor) = self.reference_anchor.as_mut()
        {
            anchor.landing = Landing::Bare;
        }
        self.stop_preview();
        self.set_reference_panel(None);
        self.invalidate_maps();
        self.focus = Focus::Editor;
        self.armed_slider = None;
        self.insert_reference_name(name)?;
        if call && let Some(index) = entry {
            self.set_reference_panel(Some(ReferencePanel::open(&self.reference, index)));
            self.install_catalogue();
        }
        self.dirty_frame = true;
        Ok(())
    }

    /// A name chosen in the reference column lands in the text: over the
    /// word the column was opened on if that word is still there untouched,
    /// otherwise at the caret.
    pub(super) fn insert_reference_name(&mut self, name: String) -> Result<(), RuntimeError> {
        let anchor = self.reference_anchor.take();
        let landing = anchor
            .as_ref()
            .map_or(Landing::Bare, |anchor| anchor.landing);
        // The word is replaced if it is still there: its range is followed
        // through whatever edited the text since - a visual growing its
        // rows counts as an edit too - and must still read the same.
        let replace = anchor
            .filter(|anchor| anchor.scene == self.scenes.current().id)
            .and_then(|anchor| {
                let range = self.anchor_range_now(&anchor)?;
                let still = self
                    .editor()
                    .document()
                    .slice(ByteOffset(range.start)..ByteOffset(range.end))
                    .ok()?;
                (still == anchor.word).then_some((anchor.word, range))
            });
        // Where the name goes, so a call can be written around it and the
        // caret put back between its brackets.
        let at = replace
            .as_ref()
            .map(|(_, range)| range.clone())
            .unwrap_or_else(|| {
                let caret = self.editor().primary_selection().head.0;
                caret..caret
            });
        let (written, inside) = match landing {
            Landing::Bare => (name, None),
            Landing::Quoted => (format!("\"{name}\""), None),
            Landing::Call => {
                let source = self.editor().source();
                let next = source.as_bytes().get(at.end).copied();
                let following = &source[at.end..];
                let trimmed = following.trim_start_matches([' ', '\t']);
                if trimmed.starts_with('(') {
                    // The call already has its brackets: its name is being
                    // put right, not a new link going in.
                    let inside = name.len() + following.len() - trimmed.len() + 1;
                    (name, Some(inside))
                } else {
                    // The `.` that opens the link, where the caret came to
                    // rest at the END of the one before it rather than
                    // after its dot.
                    let lead = match at
                        .start
                        .checked_sub(1)
                        .and_then(|before| source.as_bytes().get(before))
                    {
                        Some(b')' | b']') => ".",
                        _ => "",
                    };
                    // And the `.` the link this one goes in front of needs,
                    // which used to be the caret's own:
                    // `.voicing().|s("z_tan")` takes `glide().`, never
                    // `glide()s(…)`.
                    let joined = next.is_some_and(|byte| {
                        byte.is_ascii_alphabetic() || matches!(byte, b'_' | b'$')
                    });
                    let tail = if joined { "." } else { "" };
                    // The caret belongs between the brackets: the argument
                    // is what the list was opened to write.
                    (
                        format!("{lead}{name}(){tail}"),
                        Some(lead.len() + name.len() + 1),
                    )
                }
            }
        };
        match replace {
            // The word already reads as the chosen name: the score says
            // what it was asked to say, and inserting would say it twice.
            Some((word, _)) if word == written => {
                self.status = format!("{written} already");
            }
            Some((word, range)) => {
                self.dispatch_editor(Command::ReplaceRange {
                    from: ByteOffset(range.start),
                    to: ByteOffset(range.end),
                    text: written.clone(),
                })?;
                self.status = if word.is_empty() {
                    format!("inserted {written}")
                } else {
                    format!("{word} → {written}")
                };
            }
            None => {
                self.dispatch_editor(Command::InsertText(written.clone()))?;
                self.status = format!("inserted {written}");
            }
        }
        if let Some(inside) = inside {
            let _ = self.scenes.current_mut().editor.set_selection(
                super::super::editor::Selection::caret(ByteOffset(at.start + inside)),
            );
        }
        Ok(())
    }

    /// Ctrl+D: help for the function or argument at the caret, or close it.
    pub(super) fn toggle_reference_at_caret(&mut self) {
        // Open but not in front of you, the chord re-targets: it looks the
        // caret's word up again and takes the keyboard back. Only from
        // inside does it close.
        if self.reference_panel.is_some() && self.focus == Focus::Panel(PanelKind::Reference) {
            self.stop_preview();
            self.set_reference_panel(None);
            self.status = "reference closed".into();
            self.settle_focus();
            self.invalidate_maps();
            self.dirty_frame = true;
            return;
        }
        // Quotes ask for values of the active argument. Everywhere else
        // starts with the function's docs so its arguments are visible.
        if self.string_completion_at_caret().is_some() {
            let argument_reference = self
                .enclosing_call_at_caret()
                .and_then(|(callee, _)| self.reference.resolve(&callee));
            if self.open_string_completion() {
                if let Some(panel) = self.reference_panel.as_mut() {
                    panel.argument_reference = argument_reference;
                }
                return;
            }
            if let Some(index) = argument_reference {
                self.open_argument_reference(index);
                return;
            }
            self.status = self
                .string_completion_refusal()
                .unwrap_or_else(|| "nothing to look up inside this string".to_owned());
            self.dirty_frame = true;
            return;
        }
        // The enclosing call owns argument help until a new chain link starts.
        let enclosing = self.enclosing_call_at_caret();
        let source = self.editor().source();
        // A name still being written after a dot asks which function to
        // use, even when it is also an alias (`lp` names `lpf`). Once the
        // call has its opening bracket, its documentation is the answer.
        let unfinished_link = self.chain_position_at_caret().is_some_and(|range| {
            (enclosing.is_none() || source[..range.start.0].ends_with('.'))
                && !source[range.end.0..].trim_start().starts_with('(')
        });
        if unfinished_link && let Some(panel) = self.chain_completion_panel() {
            self.set_reference_panel(Some(panel));
            self.status = "completion - choose a function; Enter adds its call".into();
            self.install_catalogue();
            self.focus_panel(PanelKind::Reference);
            self.invalidate_maps();
            self.dirty_frame = true;
            return;
        }
        // A callee under the caret takes precedence over the enclosing call.
        // Argument values and object keys must not select a function.
        let caret = self.editor().primary_selection().head;
        let word = self.word_range_at_caret();
        let code = rustel_runtime::lint::code_only(&source);
        let callee = word.as_ref().filter(|(name, range)| {
            range.start < caret
                && caret <= range.end
                && code.get(range.start.0..range.end.0) == Some(name.as_str())
                && code[range.end.0..].trim_start().starts_with('(')
        });
        let Some((word, range)) = callee.cloned().or(enclosing).or(word) else {
            let mut panel = ReferencePanel::browse_for(&self.reference, "");
            panel.snippets_last = !self.caret_on_blank_line();
            panel.refresh(&self.reference);
            self.set_reference_panel(Some(panel));
            self.reference_anchor = None;
            self.status = "reference - type to search".into();
            self.install_catalogue();
            self.focus_panel(PanelKind::Reference);
            self.invalidate_maps();
            self.dirty_frame = true;
            return;
        };
        match self.reference.lookup(&word) {
            Some(index) => {
                self.set_reference_panel(Some(ReferencePanel::open(&self.reference, index)));
                self.reference_anchor = None;
                self.status = format!("reference - {word}");
            }
            None => {
                let mut panel = ReferencePanel::browse_for(&self.reference, &word);
                panel.snippets_last = !self.caret_on_blank_line();
                panel.refresh(&self.reference);
                self.set_reference_panel(Some(panel));
                self.anchor_reference(&word, range);
                self.status = format!("no entry for {word:?} - closest names; Enter replaces it");
            }
        }
        self.install_catalogue();
        self.focus_panel(PanelKind::Reference);
        self.invalidate_maps();
        self.dirty_frame = true;
    }

    fn open_argument_reference(&mut self, index: usize) {
        self.stop_preview();
        self.set_reference_panel(Some(ReferencePanel::open(&self.reference, index)));
        self.reference_anchor = None;
        if let Some(entry) = self.reference.entry(index) {
            self.status = format!("reference - {}", entry.name);
        }
        self.install_catalogue();
        self.focus_panel(PanelKind::Reference);
        self.invalidate_maps();
        self.dirty_frame = true;
    }

    /// Retain generated ideas across every way of closing/replacing the panel.
    pub(super) fn set_reference_panel(&mut self, next: Option<ReferencePanel>) {
        #[cfg(feature = "hydra")]
        let next = {
            let mut next = next;
            if let Some(panel) = self.reference_panel.as_ref() {
                self.generator_session = panel.generator.clone();
                if panel.tab == Tab::Generator {
                    self.generator_session.saved_row = panel.snippet_selected;
                }
            }
            if let Some(panel) = next.as_mut() {
                panel.generator = self.generator_session.clone();
                if panel.tab == Tab::Generator {
                    panel.snippet_selected = panel.generator.saved_row;
                }
            }
            next
        };
        self.reference_panel = next;
    }

    pub(super) fn handle_reference_key_with_modifiers(
        &mut self,
        code: KeyCode,
        primary: bool,
        shift: bool,
        alt: bool,
    ) -> Result<bool, RuntimeError> {
        if let Some(pending) = self
            .reference_panel
            .as_mut()
            .and_then(|panel| panel.confirm_delete.take())
        {
            self.dirty_frame = true;
            if !primary && !shift && !alt {
                match code {
                    KeyCode::Enter => {
                        self.confirm_sample_delete(pending);
                        return Ok(true);
                    }
                    KeyCode::Esc => {
                        self.status = "sample deletion cancelled".into();
                        return Ok(true);
                    }
                    _ => {}
                }
            }
            self.status = "sample deletion cancelled".into();
        }
        let before = self
            .reference_panel
            .as_ref()
            .map(super::super::reference::ReferencePanel::scroll_signature);
        let handled = self.handle_reference_key_inner(code, primary, shift, alt);
        // A key that moved the selection, changed the tab, or opened or
        // folded a row walks the list the keyboard's way, margin and all;
        // one that only played or copied the row leaves a clicked row where
        // the pointer left it.
        if let Some(panel) = self.reference_panel.as_mut()
            && before.is_some_and(|before| before != panel.scroll_signature())
        {
            panel.hold_scroll = false;
        }
        handled
    }

    /// A key for the reference column, tried in order: a copy of the text
    /// dragged across it; `s` on the examples or generator tab, which
    /// switches the snippet's music between one `$:` a voice and one
    /// `$: stack(...)`; the generator tab's own keys, which deal ideas and
    /// turn its controls; then the panel's key map, whose action is
    /// carried out last. Whether the key was taken.
    fn handle_reference_key_inner(
        &mut self,
        code: KeyCode,
        primary: bool,
        _shift: bool,
        alt: bool,
    ) -> Result<bool, RuntimeError> {
        // A dragged selection is what a copy key takes, ahead of every
        // other meaning - Ctrl+C included, which otherwise falls through to
        // the editor and copies the score while the eyes are on the column.
        // With nothing dragged, every key keeps its ordinary meaning.
        if matches!(code, KeyCode::Char('c' | 'C' | 'y' | 'Y'))
            && !alt
            && self.copy_reference_selection()
        {
            return Ok(true);
        }
        // `s` on the examples tab says how a track is handed over: one `$:` a
        // voice, the way a set is played, or the same voices folded into a
        // single `$: stack(...)` for somewhere that wants one pattern. It
        // is a reading of what is already composed, so the snippet on
        // screen changes with it.
        #[cfg(feature = "hydra")]
        if matches!(code, KeyCode::Char('s' | 'S'))
            && !primary
            && !alt
            && self
                .reference_panel
                .as_ref()
                .is_some_and(|panel| panel.tab.is_snippets())
        {
            self.toggle_examples_stack();
            return Ok(true);
        }
        #[cfg(feature = "hydra")]
        if self.generator_key(code, primary, _shift, alt) {
            return Ok(true);
        }
        let action = match self.reference_command_key(code, primary, alt) {
            ReferenceKey::Apply(action) => action,
            ReferenceKey::Handled => return Ok(true),
            ReferenceKey::NotHandled => return Ok(false),
        };
        self.apply_panel_action(action)?;
        Ok(true)
    }

    /// Copy the text dragged across the column. Whether there was any.
    fn copy_reference_selection(&mut self) -> bool {
        let inner = super::super::reference::inner_area(self.regions.reference);
        let text = self
            .reference_panel
            .as_ref()
            .and_then(|panel| panel.live_selection_text(&self.reference, inner));
        if let Some(text) = text {
            let lines = text.lines().count();
            match self.clipboard.set_text(text) {
                Ok(()) => {
                    // The selection stays painted and the panel stays
                    // open: a fragment copy is not the end of reading.
                    if lines > 1 {
                        self.toast(format!("copied {lines} lines"));
                    } else {
                        self.toast("copied");
                    }
                }
                Err(error) => self.status = format!("cannot copy: {error}"),
            }
            self.dirty_frame = true;
            return true;
        }
        false
    }

    /// Switch how the examples hand a track over, one `$:` a voice or one
    /// `$: stack(...)`, and keep the choice for the next session.
    #[cfg(feature = "hydra")]
    fn toggle_examples_stack(&mut self) {
        self.ui_settings.examples_stack = !self.ui_settings.examples_stack;
        // The block is re-laid-out under any selection dragged through
        // it, so what was covered is no longer what would be copied:
        // the drag is let go rather than left pointing at other text.
        if let Some(panel) = self.reference_panel.as_mut() {
            panel.selection = None;
        }
        self.apply_ui_settings();
        self.prefs.examples_stack = Some(self.ui_settings.examples_stack);
        self.save_prefs_soon();
        self.toast(if self.ui_settings.examples_stack {
            "taken as one $: stack(...)"
        } else {
            "taken as one $: a voice"
        });
        self.dirty_frame = true;
    }

    /// The generator tab's own keys, ahead of the column's: `g` and `v` deal
    /// a fresh or a similar idea, the arrows step through ideas, turn a
    /// control or pick a direction, Home and End take a control to its
    /// ends, and the page keys scroll the code. Whether the key was one of
    /// them.
    #[cfg(feature = "hydra")]
    fn generator_key(&mut self, code: KeyCode, primary: bool, shift: bool, alt: bool) -> bool {
        if !primary
            && !alt
            && matches!(code, KeyCode::Char('g' | 'G' | 'v' | 'V'))
            && let Some(panel) = self
                .reference_panel
                .as_mut()
                .filter(|p| p.tab == Tab::Generator)
        {
            let selected = panel.generator_row();
            let similar = matches!(code, KeyCode::Char('v' | 'V'));
            if similar {
                panel.generator.similar();
            } else {
                panel.generator.fresh();
            }
            panel.generator.open = true;
            panel.snippet_selected = panel
                .generator
                .rows()
                .iter()
                .position(|row| Some(*row) == selected)
                .unwrap_or(0);
            panel.snippet_code_scroll.set(0);
            panel.selection = None;
            self.queue_generator_preview();
            return true;
        }
        if !primary
            && !alt
            && matches!(
                code,
                KeyCode::Left | KeyCode::Right | KeyCode::Home | KeyCode::End
            )
            && let Some(panel) = self.reference_panel.as_mut()
        {
            if matches!(code, KeyCode::Left | KeyCode::Right)
                && let Some(action) = panel
                    .generator_row()
                    .and_then(super::super::ideas::Row::action)
            {
                if panel
                    .generator
                    .step_or_generate(action, code == KeyCode::Right)
                {
                    panel.selection = None;
                    panel.snippet_code_scroll.set(0);
                    self.queue_generator_preview();
                }
                return true;
            }
            if let Some(super::super::ideas::Row::Control(index)) = panel.generator_row() {
                let changed = match code {
                    KeyCode::Home => panel.generator.set(index, 0),
                    KeyCode::End => panel.generator.set(index, 100),
                    _ => panel.generator.adjust(
                        index,
                        if code == KeyCode::Left { -1 } else { 1 } * if shift { 5 } else { 1 },
                    ),
                };
                panel.selection = None;
                if changed {
                    self.queue_generator_preview();
                }
                return true;
            }
            if code == KeyCode::Right
                && matches!(panel.generator_row(), Some(super::super::ideas::Row::Direction(d)) if d != panel.generator.direction())
            {
                panel.generator_activate();
                self.queue_generator_preview();
                return true;
            }
        }
        if !primary
            && !alt
            && code == KeyCode::Right
            && let Some(panel) = self
                .reference_panel
                .as_mut()
                .filter(|panel| panel.tab == Tab::Generator)
        {
            match panel.generator_row() {
                Some(super::super::ideas::Row::Direction(_)) => {
                    panel.generator.open = true;
                }
                _ => {
                    if panel.generator_activate() == PanelAction::GeneratorChanged {
                        self.queue_generator_preview();
                    }
                }
            }
            return true;
        }
        if !primary
            && !alt
            && matches!(code, KeyCode::PageUp | KeyCode::PageDown)
            && let Some(panel) = self
                .reference_panel
                .as_mut()
                .filter(|panel| panel.tab == Tab::Generator)
        {
            let inner = inner_area(self.regions.reference);
            let page = panel.snippet_layout(inner).code_rows().height.max(1) as isize;
            panel.scroll_snippet_code(inner, if code == KeyCode::PageUp { -page } else { page });
            self.dirty_frame = true;
            return true;
        }
        false
    }

    /// What a key asks of the open panel, by the panel's key map. This half
    /// holds the column's commands: Esc, switching tabs, and the keys a tab
    /// has of its own - copying a snippet, the preview's volume, the
    /// samples tab's Alt chords. Every other key is the list's and goes on
    /// to `reference_list_key`. With no panel open no key is taken.
    fn reference_command_key(&mut self, code: KeyCode, primary: bool, alt: bool) -> ReferenceKey {
        // Read before the panel is borrowed: the list's arrows need to know
        // whether this is a COMPLETION, which is anchored to a place in the
        // score, or the reference being read beside one, which is not.
        let anchored = self.reference_anchor.is_some();
        let Some(panel) = self.reference_panel.as_mut() else {
            return ReferenceKey::NotHandled;
        };
        // Alt+T arms a trim on the row under the cursor and every other key
        // disarms it - moving, searching, Esc, a tab switch - so a stray
        // second press somewhere else can never cut a file nobody meant to
        // touch. The arm itself is handled below, where it can also read
        // the sample library; this only clears what every other key owes it.
        let is_alt_trim = matches!(code, KeyCode::Char('t' | 'T')) && alt && !primary;
        if panel.tab == Tab::Samples && !is_alt_trim {
            panel.confirm_trim = None;
        }
        let action = match code {
            // Esc first lets go of a dragged selection; the next press does
            // whatever it always did.
            KeyCode::Esc if panel.selection.is_some() => {
                panel.selection = None;
                PanelAction::Nothing
            }
            // Esc on the samples tab closes the browser, and closing silences
            // whatever it was previewing: one press, not one to stop the sound
            // and another to leave.
            KeyCode::Esc => panel.escape(),
            KeyCode::Tab => {
                panel.toggle_tab();
                PanelAction::Nothing
            }
            KeyCode::BackTab => {
                panel.previous_tab();
                PanelAction::Nothing
            }
            #[cfg(feature = "hydra")]
            KeyCode::Char('c' | 'C') if !primary && !alt && panel.tab.is_snippets() => panel.copy(),
            // Walking the list can audition as it goes. Alt-modified,
            // because plain letters belong to the search box on this tab -
            // typing "cla" must not toggle auto-play mid-word.
            KeyCode::Char('a' | 'A') if alt && !primary && panel.tab == Tab::Samples => {
                self.preview_autoplay = !self.preview_autoplay;
                self.status = if self.preview_autoplay {
                    "samples - auto-play on: moving previews".to_owned()
                } else {
                    "samples - auto-play off".to_owned()
                };
                PanelAction::Nothing
            }
            // The preview's own volume, past unity on purpose: a browser
            // next to a playing set needs headroom to be heard through it.
            // On every tab that auditions, not only the samples one - a
            // chord and a scale are played to be heard as much as a sample
            // is, and they draw the same fader.
            // ⌥↑/⌥↓ beside them, because `+` is a shifted key on most
            // layouts outside the US - ⌥+ is really ⌥⇧= there, which is a
            // chord nobody guesses and several terminals never deliver.
            // The arrows arrive everywhere.
            KeyCode::Up | KeyCode::Char('+' | '=')
                if alt && !primary && super::super::reference::tab_previews(panel.tab) =>
            {
                self.preview_gain = nudge_preview_gain(self.preview_gain, 3.0);
                self.status = format!(
                    "preview {}",
                    super::super::reference::format_preview_gain(self.preview_gain)
                );
                PanelAction::Nothing
            }
            KeyCode::Down | KeyCode::Char('-')
                if alt && !primary && super::super::reference::tab_previews(panel.tab) =>
            {
                self.preview_gain = nudge_preview_gain(self.preview_gain, -3.0);
                self.status = format!(
                    "preview {}",
                    super::super::reference::format_preview_gain(self.preview_gain)
                );
                PanelAction::Nothing
            }
            // Alt, like the browser's other commands: plain letters belong to
            // the search box, and ^O is the settings sheet here as everywhere.
            KeyCode::Char('o' | 'O') if alt && !primary && panel.tab == Tab::Samples => {
                panel.location()
            }
            // Renaming an imported bank happens on Settings ▸ Samples (`r` on
            // a user import) or here on the samples tab (Alt+R), never on any
            // other tab: a chord or a scale has no bank to alias.
            //
            // Alias the bank under the cursor without leaving the browser
            // to find it again on Settings ▸ Samples.
            KeyCode::Char('r' | 'R') if alt && !primary && panel.tab == Tab::Samples => {
                panel.rename()
            }
            KeyCode::Char('d' | 'D') if alt && !primary && panel.tab == Tab::Samples => {
                self.request_selected_sample_delete();
                PanelAction::Nothing
            }
            // Cut a sample's silence, armed by a first press and done by a
            // second - the sample-cache row's own confirm, on a chord
            // instead of Enter because a plain `t` here is a search
            // letter.
            KeyCode::Char('t' | 'T') if is_alt_trim && panel.tab == Tab::Samples => {
                self.trim_selected_sample();
                PanelAction::Nothing
            }
            // What is left is the list's: choosing, opening, playing and
            // walking its rows, and editing its search box.
            _ => return self.reference_list_key(code, primary, alt, anchored),
        };
        ReferenceKey::Apply(action)
    }

    /// The list's keys, the ones `reference_command_key` leaves: Enter
    /// chooses the row under the cursor, → opens it (or plays it, with
    /// nothing to open) and ← folds it, Space plays it, ↑ and ↓ walk the
    /// list, the page keys page it (or scroll the text of an entry open on
    /// the Reference tab), and Backspace and letters edit its search box.
    /// `anchored` is whether the panel is a completion tied to a place in
    /// the score.
    fn reference_list_key(
        &mut self,
        code: KeyCode,
        primary: bool,
        alt: bool,
        anchored: bool,
    ) -> ReferenceKey {
        if code == KeyCode::Backspace
            && !primary
            && !alt
            && let Some(index) = self
                .reference_panel
                .as_ref()
                .filter(|panel| panel.empty_search())
                .and_then(|panel| panel.argument_reference)
        {
            self.open_argument_reference(index);
            return ReferenceKey::Handled;
        }
        let Some(panel) = self.reference_panel.as_mut() else {
            return ReferenceKey::NotHandled;
        };
        let action = match code {
            KeyCode::Enter if !primary && !alt => panel.confirm(&self.reference),
            // An arrow with nothing to do is not claimed: it falls through
            // and moves the caret, instead of being swallowed as a no-op.
            // That is the reference being read beside a score, where the
            // arrows still belong to the editor.
            //
            // A completion is different. It is anchored to a place in the
            // text - the word it was opened on, or the empty spot after
            // `bank("` - and the name chosen goes there. Letting the arrows
            // through walks the caret away from that spot while the list is
            // open, so the choice appears to land somewhere nobody pointed
            // at. While one is anchored the arrows stay in the list, and the
            // caret stays where the list was opened on.
            KeyCode::Right => {
                if panel.expand() {
                    PanelAction::Nothing
                } else {
                    // Nothing to open: → plays what is under the cursor -
                    // a sample, a chord, a scale - the way Space does.
                    match panel.preview() {
                        PanelAction::Nothing if !anchored => return ReferenceKey::NotHandled,
                        PanelAction::Nothing => PanelAction::Nothing,
                        action => action,
                    }
                }
            }
            KeyCode::Left => {
                if !panel.collapse() && !anchored {
                    return ReferenceKey::NotHandled;
                }
                PanelAction::Nothing
            }
            // Space plays the selected sound - and on the row that is
            // already sounding it stops it: one key, play and stop, the way
            // every sample browser's does.
            KeyCode::Char(' ') if !primary && !alt && !panel.wants_text() => {
                match (panel.preview(), &self.preview_armed) {
                    (PanelAction::Preview(sound), Some((sounding, started)))
                        if *sounding == sound && started.elapsed() <= PREVIEW_STOP_WINDOW =>
                    {
                        self.stop_preview();
                        return ReferenceKey::Handled;
                    }
                    (action, _) => action,
                }
            }
            // A search box takes its letters, but a space in a name is
            // rare and hearing the row is what a browser is for: Space
            // plays what is under the cursor, and stops it if it is the
            // one sounding.
            KeyCode::Char(' ') if !primary && !alt => {
                match (panel.preview(), &self.preview_armed) {
                    (PanelAction::Nothing, _) => {
                        let _ = panel.type_char(&self.reference, ' ');
                        PanelAction::Nothing
                    }
                    (
                        PanelAction::Preview(name)
                        | PanelAction::PreviewChord(name)
                        | PanelAction::PreviewScale(name)
                        | PanelAction::PreviewTuning(name),
                        Some((sounding, started)),
                    ) if *sounding == name && started.elapsed() <= PREVIEW_STOP_WINDOW => {
                        self.stop_preview();
                        return ReferenceKey::Handled;
                    }
                    (action, _) => action,
                }
            }
            KeyCode::Up => {
                panel.move_by(-1);
                if self.preview_autoplay && panel.tab == Tab::Samples {
                    panel.preview()
                } else {
                    PanelAction::Nothing
                }
            }
            KeyCode::Down => {
                panel.move_by(1);
                if self.preview_autoplay && panel.tab == Tab::Samples {
                    panel.preview()
                } else {
                    PanelAction::Nothing
                }
            }
            KeyCode::PageUp | KeyCode::PageDown => {
                let inner = super::super::reference::inner_area(self.regions.reference);
                let down = code == KeyCode::PageDown;
                if panel.tab == Tab::Reference
                    && let super::super::reference::ReferenceMode::Entry { index, scroll, .. } =
                        &mut panel.mode
                {
                    let body = super::super::reference::entry_body_area(inner);
                    let count = self.reference.entry(*index).map_or(0, |entry| {
                        super::super::reference::entry_body(entry, usize::from(inner.width)).len()
                    });
                    let last = count
                        .saturating_sub(usize::from(body.height))
                        .min(usize::from(u16::MAX));
                    *scroll = page_selection(
                        usize::from(*scroll),
                        last + 1,
                        usize::from(body.height),
                        down,
                    ) as u16;
                } else {
                    let page =
                        isize::try_from(panel.geometry(inner).list.height.max(1)).unwrap_or(1);
                    panel.move_by(if down { page } else { -page });
                }
                PanelAction::Nothing
            }
            // Plain Backspace removes one character, and each one re-ranks
            // the catalogue, which is slow for a long search string. Either
            // editing chord - whichever this terminal delivers - empties
            // the box in one press. With nothing to empty, the chord acts
            // like plain Backspace.
            KeyCode::Backspace if primary || alt => {
                // A tab with a search box keeps the key whether or not
                // there is anything in it, as plain Backspace does: a chord
                // that fell through to the score from over an empty box
                // would delete a word of the music.
                if !panel.clear_query(&self.reference) && !panel.wants_text() && !panel.collapse() {
                    return ReferenceKey::NotHandled;
                }
                PanelAction::Nothing
            }
            KeyCode::Backspace => {
                if !panel.backspace(&self.reference) {
                    // With no search box eating it, Backspace reads as
                    // "back": it folds what → opened, like ←.
                    if !panel.collapse() {
                        return ReferenceKey::NotHandled;
                    }
                }
                PanelAction::Nothing
            }
            // A letter with no search box to land in belongs to the score:
            // reading an entry and typing what it teaches is the point.
            KeyCode::Char(character) if !primary && !alt => {
                if !panel.type_char(&self.reference, character) {
                    return ReferenceKey::NotHandled;
                }
                PanelAction::Nothing
            }
            // Chords, arrows with modifiers and the transport fall through.
            _ => return ReferenceKey::NotHandled,
        };
        ReferenceKey::Apply(action)
    }

    /// Alt+T on the samples tab: arm a trim of the sample under the cursor,
    /// or start it on the row a first press armed. Refusals are checked,
    /// and said, before anything is armed: a row Alt+T cannot touch never
    /// gets a "press again" that would lie about what the next press does.
    fn trim_selected_sample(&mut self) {
        let Some(panel) = self.reference_panel.as_mut() else {
            return;
        };
        let row = panel.sound_selected;
        let armed_here = panel.confirm_trim == Some(row);
        panel.confirm_trim = None;
        match panel.selected_bank() {
            None => self.status = "nothing here to trim".into(),
            Some((_name, origin))
                if !matches!(
                    origin,
                    rustel_runtime::samples::SoundOrigin::Global
                        | rustel_runtime::samples::SoundOrigin::Set
                ) => {}
            Some((name, _origin)) => {
                let variant = match panel.sound_rows().get(row) {
                    Some(SoundRow::Variant(_, variant)) => Some(*variant),
                    _ => None,
                };
                match self.worker.library() {
                    None => self.status = "the sample library is not up yet".into(),
                    Some(library) => match trim_candidate(&library, &name, variant) {
                        Err(why) => self.status = why,
                        Ok(path) if armed_here => {
                            let decoded = library.peek_ready_ids(&name).into_iter().collect();
                            self.start_trim(TrimTarget::Sample {
                                name: name.clone(),
                                path,
                                decoded,
                            });
                            self.status = format!("trimming {name}…");
                        }
                        Ok(_) => {
                            panel.confirm_trim = Some(row);
                            self.status = format!(
                                "{} {name} · Esc cancels",
                                super::super::keybinds::shortcut_label("Alt+T again trims")
                            );
                        }
                    },
                }
            }
        }
    }

    /// Carry out what a key on the column asked for, and repaint.
    fn apply_panel_action(&mut self, action: PanelAction) -> Result<(), RuntimeError> {
        // A Hydra snippet off the shelf can be a theme's sketch; a score
        // line can only be copied.
        #[cfg(feature = "hydra")]
        let from_hydra_shelf = self.reference_panel.as_ref().is_some_and(|panel| {
            panel.tab.is_snippets()
                && panel.selected_snippet_kind() == Some(super::super::examples::Kind::Hydra)
        });
        match action {
            PanelAction::Nothing => {}
            PanelAction::Close => {
                self.stop_preview();
                self.set_reference_panel(None);
                self.status = "reference closed".into();
                // The column was holding the keyboard; without this the
                // focus stays claimed by a panel that is no longer open
                // until some other path settles it.
                self.settle_focus();
                self.invalidate_maps();
            }
            PanelAction::Insert(name) => {
                self.confirm_reference_name(name)?;
            }
            PanelAction::Paste(code) => self.paste_snippet(code)?,
            PanelAction::Preview(sound) => self.preview_sound(&sound),
            #[cfg(feature = "hydra")]
            PanelAction::PreviewScore(code) => self.preview_snippet_score(&code),
            #[cfg(feature = "hydra")]
            PanelAction::GeneratorChanged => self.queue_generator_preview(),
            PanelAction::PreviewChord(chord) => self.preview_chord(&chord),
            PanelAction::PreviewScale(scale) => self.preview_scale(&scale),
            PanelAction::PreviewTuning(tuning) => self.preview_tuning(&tuning),
            PanelAction::Reveal { name, variant, url } => {
                self.reveal_sound(&name, variant, &url);
            }
            PanelAction::Copy(text) => {
                // With the theme editor open, taking a snippet off the shelf
                // means "this is the theme's sketch" - that is what the shelf
                // was opened for, and it needs no initHydra dressing.
                #[cfg(feature = "hydra")]
                if from_hydra_shelf && self.theme_editor.is_some() {
                    if let Some(editor) = self.theme_editor.as_mut() {
                        // A dirty code tab parses first so its other edits
                        // survive the take; a half-typed entry is let go.
                        editor.entry = None;
                        if editor.code_dirty_at.take().is_some() {
                            editor.parse_code();
                        }
                        editor.draft.set_hydra_code(text);
                        editor.rebuild_code();
                        let draft = editor.draft.clone();
                        self.apply_theme_draft(&draft);
                    }
                    self.set_reference_panel(None);
                    self.reference_anchor = None;
                    self.invalidate_maps();
                    self.focus_panel(PanelKind::ThemeEditor);
                    self.toast("the snippet is the theme's sketch now");
                    self.dirty_frame = true;
                    return Ok(());
                }
                // A snippet is a block of code, so say "copied" rather than
                // quoting five lines of it back. The shelf stays open: one
                // take is rarely the end of it - the next press regenerates,
                // or takes the drums to go with the bassline.
                let block = text.contains('\n');
                match self.clipboard.set_text(text.clone()) {
                    Ok(()) => {
                        // A sound's name is short enough to say back.
                        if block {
                            self.toast("copied");
                        } else {
                            self.toast(format!("copied {text}"));
                        }
                    }
                    Err(error) => self.status = format!("cannot copy: {error}"),
                }
            }
            PanelAction::RenameSample { name, variant } => {
                if let Some(library) = self.worker.library() {
                    match library.local_sample_path(&name, variant) {
                        Ok(path) => {
                            let stem = path
                                .file_stem()
                                .unwrap_or_default()
                                .to_string_lossy()
                                .into_owned();
                            self.renaming_sample = Some((name, variant, path));
                            self.open_set_prompt(SetPrompt::RenameSample);
                            if let Some((_, picker)) = self.set_prompt.as_mut() {
                                picker.offer(&stem);
                            }
                        }
                        Err(error) => self.status = error,
                    }
                }
            }
            PanelAction::RenameBank { name, origin } => {
                // Only the imported layers answer to a rename - the set's
                // own folder and a Settings source - the same rule
                // `rename_bank` itself was written under. A pinned bank is
                // shared vocabulary the studio ships with; aliasing it here
                // would set an overlay `rebuild_global` never reads for a
                // name it does not carry, which is a silent no-op dressed
                // up as success. Say so instead of doing nothing quietly.
                use rustel_runtime::samples::SoundOrigin;
                match origin {
                    SoundOrigin::Global | SoundOrigin::Set => {
                        let scoped = (origin == SoundOrigin::Global)
                            .then(|| {
                                let library = self.worker.library()?;
                                let source = library
                                    .catalogue()
                                    .into_iter()
                                    .find(|entry| entry.name == name)?
                                    .import?;
                                let original = library.original_for_import_alias(&source, &name)?;
                                Some((source, original))
                            })
                            .flatten();
                        if let Some((source, original)) = scoped {
                            self.renaming_bank_source = Some(source);
                            self.renaming_bank = Some(original);
                        } else {
                            self.renaming_bank_source = None;
                            self.renaming_bank = Some(name.clone());
                        }
                        self.open_set_prompt(SetPrompt::RenameBank);
                        if let Some((_, picker)) = self.set_prompt.as_mut() {
                            picker.offer(&name);
                        }
                    }
                    _ => {
                        self.status = format!(
                            "{name} ships with the studio - only an imported bank can be aliased"
                        );
                    }
                }
            }
        }
        self.dirty_frame = true;
        Ok(())
    }

    /// Show the file behind a sound the browser has under its cursor.
    fn reveal_sound(&mut self, name: &str, variant: Option<usize>, url: &str) {
        match self.sound_reveal_target(name, variant, url) {
            Ok(target) => self.reveal_target(target),
            Err(status) => {
                self.status = status;
                self.dirty_frame = true;
            }
        }
    }

    /// Where showing a browser sound goes: the library's own address for
    /// that sample when it has one - a numbered sample's own file - else the
    /// address its bank was listed with.
    pub(super) fn sound_reveal_target(
        &self,
        name: &str,
        variant: Option<usize>,
        url: &str,
    ) -> Result<RevealTarget, String> {
        let located = self
            .worker
            .library()
            .and_then(|library| library.file_location(name, variant));
        let url = located.as_deref().unwrap_or(url);
        bank_location(url).ok_or_else(|| format!("nowhere to open for {url}"))
    }

    /// Re-read what the library can play into the open samples tab.
    pub(super) fn refresh_catalogue(&mut self) {
        self.catalogue_refreshed_at = Instant::now();
        self.worker.invalidate_catalogue();
        self.refresh_catalogue_metadata();
    }

    pub(super) fn refresh_catalogue_metadata(&mut self) {
        if self.reference_panel.is_none() {
            return;
        }
        let imports = browser_imports(
            &self.scenes.current().editor.source(),
            &self.worker.catalogue(),
        );
        let input_channels = self.input_channels();
        let panel = self.reference_panel.as_mut().expect("open reference");
        self.dirty_frame |= panel.set_imports(imports);
        self.dirty_frame |= panel.set_input_channels(input_channels);
    }

    pub(super) fn install_catalogue(&mut self) {
        let catalogue = self.worker.catalogue();
        let imports = browser_imports(&self.scenes.current().editor.source(), &catalogue);
        let input_channels = self.input_channels();
        let Some(panel) = self.reference_panel.as_mut() else {
            return;
        };
        // A `.bank(…)` completion lists machines, not sounds: a library
        // that arrives (or changes) under the open panel changes the
        // machines on offer. Keep the insertion anchor's sound context and
        // the reader's choice to show compatible banks or all banks.
        if let Some(bank_sounds) = panel.bank_sounds() {
            let (machines, compatible_count) = bank_machines_ranked(&catalogue.sounds, bank_sounds);
            self.dirty_frame |= panel.set_bank_names(&self.reference, machines, compatible_count);
        }
        // Keep Samples ready even while Reference, Chords, or Scales is visible.
        let sounds = with_input_channels(catalogue.sounds.clone(), input_channels);
        // Keep the bank connection across a refresh; only the sounds change.
        let connected = panel.sound_prefix.clone();
        let changed = panel.set_sounds_prefixed(sounds, connected);
        let changed = panel.set_imports(imports) || changed;
        self.dirty_frame |= changed;
    }

    /// A press on the reference column: a tab, the preview fader, a text
    /// selection, or a row.
    pub(super) fn click_reference(
        &mut self,
        mouse: MouseEvent,
        x: u16,
        y: u16,
    ) -> Result<(), RuntimeError> {
        self.focus_panel(PanelKind::Reference);
        // The header row's tabs are buttons.
        let inner = super::super::reference::inner_area(self.regions.reference);
        let showing = self.reference_panel.as_ref().map(|panel| panel.tab);
        if let Some(tab) =
            showing.and_then(|showing| super::super::reference::tab_at(inner, showing, x, y))
        {
            if let Some(panel) = self.reference_panel.as_mut() {
                panel.select_tab(tab);
            }
            self.pointer = Some(Pointer::Panel);
            self.dirty_frame = true;
            return Ok(());
        }
        #[cfg(feature = "hydra")]
        if let Some(panel) = self.reference_panel.as_mut() {
            if let Some((rail, top, length, last)) = panel.snippet_scrollbar(inner)
                && within(rail, x, y)
            {
                let mut scroll = panel.snippet_code_scroll.get().min(last);
                if y < rail.y + top || y >= rail.y + top + length {
                    let travel = usize::from(rail.height - length);
                    let target = usize::from(y.saturating_sub(rail.y).saturating_sub(length / 2))
                        .min(travel);
                    scroll = (target * last + travel / 2) / travel.max(1);
                    panel.snippet_code_scroll.set(scroll);
                }
                panel.selection = None;
                self.pointer = Some(Pointer::SnippetCodeScroll { row: y, scroll });
                self.dirty_frame = true;
                return Ok(());
            }
            if let Some((action, forward, selected)) = panel.generator_history_at(inner, x, y) {
                panel.snippet_selected = selected;
                if panel.generator.step_or_generate(action, forward) {
                    panel.selection = None;
                    panel.snippet_code_scroll.set(0);
                    self.queue_generator_preview();
                }
                self.pointer = Some(Pointer::Panel);
                self.dirty_frame = true;
                return Ok(());
            }
        }
        #[cfg(feature = "hydra")]
        if let Some((index, rail)) = self
            .reference_panel
            .as_ref()
            .and_then(|panel| panel.generator_control_at(inner, x, y))
        {
            if let Some(panel) = self.reference_panel.as_mut() {
                panel.snippet_selected = panel
                    .generator
                    .rows()
                    .iter()
                    .position(|row| *row == super::super::ideas::Row::Control(index))
                    .unwrap_or(0);
            }
            self.pointer = Some(Pointer::GeneratorControl { index, rail });
            self.drag_generator_control(index, rail, x);
            return Ok(());
        }
        // On a tab that auditions, the meter row is the preview fader:
        // press or drag sets the volume, like the master. Every tab that
        // draws the fader takes the press, the chords and scales tabs
        // included.
        if self
            .reference_panel
            .as_ref()
            .is_some_and(|panel| super::super::reference::tab_previews(panel.tab))
        {
            let inner = super::super::reference::inner_area(self.regions.reference);
            let (_, meter_y) = if showing == Some(Tab::Samples) {
                super::super::reference::sample_pulse_rows(inner)
            } else {
                super::super::reference::samples_pulse_rows(inner)
            };
            if y == meter_y {
                self.preview_gain = super::super::reference::preview_gain_at(inner, x);
                self.status = format!(
                    "preview {}",
                    super::super::reference::format_preview_gain(self.preview_gain)
                );
                self.pointer = Some(Pointer::PreviewVolume);
                self.dirty_frame = true;
                return Ok(());
            }
        }
        // A press in the entry body or the snippet code starts a
        // character selection; everything else in the column
        // keeps its row action.
        if self.begin_reference_selection(x, y, mouse.modifiers) {
            return Ok(());
        }
        self.pointer = Some(Pointer::Panel);
        let geometry = self
            .reference_panel
            .as_ref()
            .map(|panel| panel.geometry(inner_area(self.regions.reference)));
        let action = match (self.reference_panel.as_mut(), geometry) {
            (Some(panel), Some(geometry)) => panel.click(&self.reference, geometry, y),
            _ => PanelAction::Nothing,
        };
        match action {
            PanelAction::Preview(sound) => self.preview_sound(&sound),
            #[cfg(feature = "hydra")]
            PanelAction::PreviewScore(code) => self.preview_snippet_score(&code),
            #[cfg(feature = "hydra")]
            PanelAction::GeneratorChanged => self.queue_generator_preview(),
            PanelAction::PreviewChord(chord) => self.preview_chord(&chord),
            PanelAction::PreviewScale(scale) => self.preview_scale(&scale),
            PanelAction::PreviewTuning(tuning) => self.preview_tuning(&tuning),
            PanelAction::Insert(name) => {
                self.confirm_reference_name(name)?;
            }
            PanelAction::Paste(code) => self.paste_snippet(code)?,
            PanelAction::Close => {
                self.stop_preview();
                self.set_reference_panel(None);
            }
            PanelAction::Reveal { name, variant, url } => {
                self.reveal_sound(&name, variant, &url);
            }
            PanelAction::Copy(sound) => {
                // Same gesture as Enter on the shelf: with the
                // theme editor open, a taken snippet is the
                // theme's sketch, not a clipboard entry.
                #[cfg(feature = "hydra")]
                if sound.contains('\n')
                    && self.theme_editor.is_some()
                    && self.reference_panel.as_ref().is_some_and(|panel| {
                        panel.selected_snippet_kind() == Some(super::super::examples::Kind::Hydra)
                    })
                {
                    if let Some(editor) = self.theme_editor.as_mut() {
                        editor.entry = None;
                        if editor.code_dirty_at.take().is_some() {
                            editor.parse_code();
                        }
                        editor.draft.set_hydra_code(sound);
                        editor.rebuild_code();
                        let draft = editor.draft.clone();
                        self.apply_theme_draft(&draft);
                    }
                    self.set_reference_panel(None);
                    self.reference_anchor = None;
                    self.invalidate_maps();
                    self.focus_panel(PanelKind::ThemeEditor);
                    self.toast("the snippet is the theme's sketch now");
                    self.dirty_frame = true;
                    return Ok(());
                }
                let _ = self.clipboard.set_text(sound.clone());
                self.status = format!("copied {sound}");
            }
            // A click never asks to rename a bank - Alt+R is a keyboard
            // chord with nothing for the pointer to land on - but the
            // match still has to answer for every variant.
            PanelAction::RenameBank { .. } | PanelAction::RenameSample { .. } => {}
            PanelAction::Nothing => {}
        }
        self.dirty_frame = true;
        Ok(())
    }

    /// A right click on the reference column gives it the keyboard, and on
    /// a sound copies the sound's name.
    pub(super) fn right_click_reference(&mut self, y: u16) {
        self.focus_panel(PanelKind::Reference);
        // A right click on a sound copies its name; the left click
        // stays the preview.
        let geometry = self
            .reference_panel
            .as_ref()
            .map(|panel| panel.geometry(inner_area(self.regions.reference)));
        if let (Some(panel), Some(geometry)) = (self.reference_panel.as_mut(), geometry)
            && let PanelAction::Copy(sound) = panel.right_click(geometry, y)
        {
            self.status = match self.clipboard.set_text(sound.clone()) {
                Ok(()) => format!("copied {sound} - paste it into the score"),
                Err(error) => format!("cannot copy: {error}"),
            };
            self.dirty_frame = true;
        }
    }

    /// The wheel over the pulse block nudges the preview volume: the docs
    /// hold that the meter row is the preview fader, wheeled like the
    /// master's. Every tab that draws that fader answers it - the same
    /// predicate the press and alt+/- paths use, because "draws one" and
    /// "can be moved" have to be the same list. Whether it took the wheel.
    pub(super) fn scroll_preview_volume(&mut self, x: u16, y: u16, direction: f32) -> bool {
        if self
            .reference_panel
            .as_ref()
            .is_some_and(|panel| super::super::reference::tab_previews(panel.tab))
            && within(self.regions.reference, x, y)
        {
            let inner = super::super::reference::inner_area(self.regions.reference);
            let (scope_y, meter_y) = if self
                .reference_panel
                .as_ref()
                .is_some_and(|panel| panel.tab == Tab::Samples)
            {
                super::super::reference::sample_pulse_rows(inner)
            } else {
                super::super::reference::samples_pulse_rows(inner)
            };
            if y == meter_y || y == scope_y {
                self.preview_gain = nudge_preview_gain(self.preview_gain, direction * 1.5);
                self.status = format!(
                    "preview {}",
                    super::super::reference::format_preview_gain(self.preview_gain)
                );
                self.dirty_frame = true;
                return true;
            }
        }
        false
    }

    /// A drag along the preview fader sets the volume under the pointer,
    /// as the press on it does.
    pub(super) fn drag_preview_volume(&mut self, x: u16) {
        let inner = super::super::reference::inner_area(self.regions.reference);
        self.preview_gain = super::super::reference::preview_gain_at(inner, x);
        self.status = format!(
            "preview {}",
            super::super::reference::format_preview_gain(self.preview_gain)
        );
        self.dirty_frame = true;
    }

    /// The wheel over the reference walks the reference, not what is
    /// under it. Whether it took the wheel.
    pub(super) fn scroll_reference(&mut self, x: u16, y: u16, direction: f32) -> bool {
        if self.reference_panel.is_none() || !within(self.regions.reference, x, y) {
            return false;
        }
        if let Some(panel) = self.reference_panel.as_mut() {
            #[cfg(feature = "hydra")]
            {
                let inner = inner_area(self.regions.reference);
                if panel.tab.is_snippets() && within(panel.snippet_layout(inner).code, x, y) {
                    panel.scroll_snippet_code(inner, if direction > 0.0 { -3 } else { 3 });
                    self.dirty_frame = true;
                    return true;
                }
                if let Some((index, _)) = panel.generator_control_at(inner, x, y) {
                    panel.selection = None;
                    if panel.generator.adjust(index, direction as i16) {
                        self.queue_generator_preview();
                    }
                    self.dirty_frame = true;
                    return true;
                }
            }
            panel.move_by(if direction > 0.0 { -3 } else { 3 });
            self.dirty_frame = true;
        }
        true
    }

    /// Let go of a dragged pane selection, if one is held.
    pub(super) fn drop_pane_selection(&mut self) {
        if let Some(panel) = self.reference_panel.as_mut()
            && panel.selection.take().is_some()
        {
            self.dirty_frame = true;
        }
    }

    /// Begin a drag selection where the press landed, if it landed in one of
    /// the reference column's selectable text blocks. Double picks the word,
    /// triple the line; Shift extends what is already held.
    fn begin_reference_selection(&mut self, x: u16, y: u16, modifiers: KeyModifiers) -> bool {
        use super::super::textblock::{Clamp, TextBlock, TextSelection};
        let inner = super::super::reference::inner_area(self.regions.reference);
        let Some(panel) = self.reference_panel.as_ref() else {
            return false;
        };
        let Some((target, area, lines, first_line)) =
            panel.text_block_at(&self.reference, inner, x, y)
        else {
            return false;
        };
        let block = TextBlock {
            lines: &lines,
            area,
            first_line,
        };
        let Some(point) = block.point_at(x, y, Clamp::Inside) else {
            return false;
        };
        let granularity = self.clicks.press(x, y, self.moment());
        let Some(panel) = self.reference_panel.as_mut() else {
            return false;
        };
        let selection = match (&panel.selection, modifiers.contains(KeyModifiers::SHIFT)) {
            // Shift extends the selection already held in this block.
            (Some(held), true) if held.target == target => TextSelection {
                anchor: held.selection.anchor,
                head: point,
            },
            _ => TextSelection::caret(point),
        };
        panel.selection = Some(super::super::reference::PaneSelection {
            target,
            selection: widen_selection(selection, granularity, &lines),
        });
        self.own_text_selection(TextSurface::Reference);
        self.pointer = Some(Pointer::ReferenceText { granularity });
        self.dirty_frame = true;
        true
    }

    /// Extend the drag to wherever the pointer is now, at the granularity
    /// the press chose. A tag that stopped matching drops both selection and
    /// drag rather than banding text that is no longer there.
    pub(super) fn drag_reference_selection(
        &mut self,
        x: u16,
        y: u16,
        granularity: super::super::textblock::Granularity,
    ) {
        use super::super::textblock::{Clamp, TextBlock};
        let inner = super::super::reference::inner_area(self.regions.reference);
        let Some(panel) = self.reference_panel.as_ref() else {
            return;
        };
        let Some((area, lines, first_line)) = panel.selection_block(&self.reference, inner) else {
            if let Some(panel) = self.reference_panel.as_mut() {
                panel.selection = None;
            }
            self.pointer = None;
            return;
        };
        let block = TextBlock {
            lines: &lines,
            area,
            first_line,
        };
        let Some(point) = block.point_at(x, y, Clamp::Extend) else {
            return;
        };
        if let Some(panel) = self.reference_panel.as_mut()
            && let Some(held) = panel.selection.as_mut()
        {
            held.selection.head = point;
            held.selection = widen_selection(held.selection, granularity, &lines);
            self.dirty_frame = true;
        }
    }
}
