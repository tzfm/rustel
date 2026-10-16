//! The reference column beside the score. This file covers opening it on the
//! word under the caret (Ctrl+D), handling its keys, clicks, wheel scrolling
//! and drag text selection, dragging the generator tab's control rails, and
//! writing a chosen name or snippet into the score. It also keeps the samples
//! tab's catalogue up to date: it refreshes and installs the catalogue, reads
//! the score's samples() imports, adds input channel variants and reveals the
//! file behind a sound. Bank machine ranking lives in `bank_context.rs`.

use super::*;
use crate::reference::{ReferenceMode, ReferenceScope, TagFilter};

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
        #[cfg(feature = "vst")]
        let name = if self.reference_panel.as_ref().is_some_and(|panel| {
            panel.tab == Tab::Vst
                || (panel.tab == Tab::Reference
                    && matches!(
                        panel.plugin_words(),
                        Some(crate::reference::PluginWords::Keys(_))
                    ))
        }) {
            let Some(name) = self.plugin_text_at_caret(name) else {
                return Ok(());
            };
            name
        } else {
            name
        };
        let entry = self.reference_panel.as_ref().and_then(|panel| {
            (panel.tab == Tab::Reference && panel.vocabulary.is_none())
                .then(|| match panel.mode {
                    ReferenceMode::Browse => panel.selected_result(),
                    ReferenceMode::Entry { index, .. } => Some(index),
                })
                .flatten()
        });
        let call = entry
            .and_then(|index| self.reference.entry(index))
            .is_some_and(|entry| entry.inserts_as_call());
        let documentation = entry.filter(|_| call).map(|index| {
            let panel = self.reference_panel.as_ref().expect("selected reference");
            self.reference_entry_panel(index, panel.scope, Some(&panel.query))
        });
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
        } else if let Some(entry) = entry.and_then(|index| self.reference.entry(index))
            && let Some(anchor) = self.reference_anchor.as_mut()
        {
            anchor.landing = if entry.tags.iter().any(|tag| tag == "method") {
                Landing::Member
            } else {
                Landing::Bare
            };
        }
        self.stop_preview();
        self.set_reference_panel(None);
        self.invalidate_maps();
        self.focus = Focus::Editor;
        self.armed_slider = None;
        self.insert_reference_name(name)?;
        if let Some(panel) = documentation {
            self.set_reference_panel(Some(panel));
            self.install_catalogue();
        }
        self.dirty_frame = true;
        Ok(())
    }

    /// Direct help and inserted calls retain the namespace of their list.
    /// An explicit tag search remains the reader's chosen browse filter.
    pub(super) fn reference_entry_panel(
        &self,
        index: usize,
        scope: ReferenceScope,
        query: Option<&str>,
    ) -> ReferencePanel {
        let mut panel = ReferencePanel::open(&self.reference, index);
        panel.scope = scope;
        if let Some(query) = query.filter(|query| TagFilter::parse(query).name.is_some()) {
            panel.query = query.to_owned();
        }
        panel.refresh(&self.reference);
        panel.selected = panel
            .results
            .iter()
            .position(|&found| found == index)
            .map_or(0, |position| panel.row_of_result(position));
        panel
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
            Landing::Member => {
                let source = self.editor().source();
                let lead = match at
                    .start
                    .checked_sub(1)
                    .and_then(|before| source.as_bytes().get(before))
                {
                    Some(b')' | b']') => ".",
                    _ => "",
                };
                let joined = source
                    .as_bytes()
                    .get(at.end)
                    .is_some_and(|byte| byte.is_ascii_alphabetic() || matches!(byte, b'_' | b'$'));
                let tail = if joined { "." } else { "" };
                (format!("{lead}{name}{tail}"), None)
            }
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
            let argument_reference = self.enclosing_call_at_caret().and_then(|(callee, range)| {
                self.reference
                    .lookup_in(&callee, self.reference_name_scope_at(range.start))
            });
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
            (enclosing.is_none()
                || source[..range.start.0].ends_with('.')
                || source[range.start.0..range.end.0].contains('.'))
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
        // A callee or graph input under the caret takes precedence over
        // the enclosing call. Other argument values keep the call's help.
        let caret = self.editor().primary_selection().head;
        let word = self.word_range_at_caret();
        let code = rustel_runtime::lint::code_only(&source);
        let named_entry = word.as_ref().filter(|(name, range)| {
            range.start < caret
                && caret <= range.end
                && code.get(range.start.0..range.end.0) == Some(name.as_str())
                && (code[range.end.0..].trim_start().starts_with('(')
                    || (self.reference_scope_at(range.start).is_kabelsalat()
                        && self
                            .reference
                            .lookup_in(name, ReferenceScope::Kabelsalat)
                            .and_then(|index| self.reference.entry(index))
                            .is_some_and(|entry| !entry.inserts_as_call())))
        });
        let Some((word, range)) = named_entry.cloned().or(enclosing).or(word) else {
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
        let scope = self.reference_name_scope_at(range.start);
        match self.reference.lookup_in(&word, scope) {
            Some(index) => {
                self.set_reference_panel(Some(self.reference_entry_panel(index, scope, None)));
                self.reference_anchor = None;
                self.status = format!("reference - {word}");
            }
            None => {
                let mut panel = ReferencePanel::browse_in(&self.reference, &word, scope);
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
        let scope = self.enclosing_call_at_caret().map_or_else(
            || self.reference_scope_at_caret(),
            |(_, range)| self.reference_name_scope_at(range.start),
        );
        self.set_reference_panel(Some(self.reference_entry_panel(index, scope, None)));
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
        #[cfg(feature = "vst")]
        let next = {
            let mut next = next;
            if let Some(panel) = next.as_mut()
                && let Some((plugin, _)) = self.plugin_call_at_caret()
            {
                panel.vst.open_named(plugin);
            }
            next
        };
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
        // The next frame has the plugin list for a key onto the vst tab,
        // and the load for a key onto a plugin.
        #[cfg(feature = "vst")]
        self.sync_plugins();
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
        // The vst tab writes at the caret, so its arrows stay in the list
        // too.
        #[cfg(feature = "vst")]
        let anchored = anchored || panel.tab == Tab::Vst;
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
                                "{} again trims {name} · Esc cancels",
                                self.keybinds.hint(BindAction::TrimSample)
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
            #[cfg(feature = "vst")]
            self.sync_plugins();
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
        #[cfg(feature = "vst")]
        self.sync_plugins();
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
        let selection = match (
            &panel.selection,
            super::super::editor::click_extends(modifiers),
        ) {
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

#[cfg(test)]
mod tests {
    mod catalogue {
        //! Tests for catalogue.

        use crate::app::*;
        use crate::editor::MemoryClipboard;
        use rustel_runtime::samples::SampleLibrary;
        use std::sync::{Arc, mpsc};

        fn library() -> Arc<SampleLibrary> {
            let library = Arc::new(SampleLibrary::empty());
            library
                .register_trusted_custom(r#"{"cached_bd":["http://127.0.0.1:9/bd.wav"]}"#, None)
                .unwrap();
            library
        }

        fn contains(app: &App, name: &str) -> bool {
            app.reference_panel
                .as_ref()
                .unwrap()
                .sounds
                .iter()
                .any(|sound| sound.name == name)
        }

        #[test]
        fn catalogue_is_ready_on_every_open_even_while_library_is_locked() {
            let directory = tempfile::tempdir().unwrap();
            let mut app = crate::app::tests::support::app_over(directory.path());
            let library = library();
            app.worker.set_library(Arc::clone(&library));
            let (locked, entered) = mpsc::sync_channel(1);
            let (release, gate) = mpsc::sync_channel(1);
            let holder = std::thread::spawn(move || {
                library.with_catalogue_lock_for_tests(|| {
                    locked.send(()).unwrap();
                    gate.recv_timeout(Duration::from_secs(5)).is_ok()
                })
            });
            entered.recv_timeout(Duration::from_secs(2)).unwrap();
            app.refresh_catalogue();
            let start = Instant::now();
            for _ in 0..10 {
                app.reference_panel = None;
                app.focus = Focus::Editor;
                app.toggle_reference_browse();
                assert!(contains(&app, "cached_bd"), "Ctrl+Space first frame");
                app.reference_panel = None;
                app.focus = Focus::Editor;
                app.toggle_reference_at_caret();
                assert!(contains(&app, "cached_bd"), "Ctrl+D first frame");
                app.reference_panel.as_mut().unwrap().tab = Tab::Reference;
                app.handle_reference_key(KeyCode::Tab, false, false)
                    .unwrap();
                assert_eq!(app.reference_panel.as_ref().unwrap().tab, Tab::Samples);
                assert!(
                    contains(&app, "cached_bd"),
                    "tab switching needs no refresh"
                );
            }
            eprintln!(
                "20 catalogue panel opens while library is locked: {:?}",
                start.elapsed()
            );
            let _ = release.send(());
            assert!(
                holder.join().unwrap(),
                "opening the panel waited for the library lock to time out"
            );
        }

        #[test]
        fn catalogue_arrivals_are_installed_before_switching_to_samples() {
            let directory = tempfile::tempdir().unwrap();
            let mut app = crate::app::tests::support::app_over(directory.path());
            let library = library();
            app.worker.set_library(Arc::clone(&library));
            app.toggle_reference_browse();
            app.reference_panel.as_mut().unwrap().tab = Tab::Reference;
            library
                .register_trusted_custom(r#"{"arrived_sn":["http://127.0.0.1:9/sn.wav"]}"#, None)
                .unwrap();
            app.refresh_catalogue();
            let deadline = Instant::now() + Duration::from_secs(5);
            while !contains(&app, "arrived_sn") && Instant::now() < deadline {
                app.pump_turn();
                std::thread::sleep(Duration::from_millis(1));
            }
            assert!(
                contains(&app, "arrived_sn"),
                "hidden Samples list was not kept ready"
            );
            assert_eq!(app.reference_panel.as_ref().unwrap().tab, Tab::Reference);
            app.handle_reference_key(KeyCode::Tab, false, false)
                .unwrap();
            assert_eq!(app.reference_panel.as_ref().unwrap().tab, Tab::Samples);
            assert!(contains(&app, "arrived_sn"));
        }

        #[test]
        fn requesting_a_catalogue_refresh_does_not_undo_a_visible_rename() {
            let directory = tempfile::tempdir().unwrap();
            let mut app = crate::app::tests::support::app_over(directory.path());
            app.worker.set_library(library());
            app.toggle_reference_browse();
            app.reference_panel
                .as_mut()
                .unwrap()
                .prepare_sound_rename("cached_bd", "vox1");
            app.refresh_catalogue();
            assert!(contains(&app, "vox1"));
            assert!(!contains(&app, "cached_bd"));
        }

        #[test]
        fn large_cached_catalogue_renders_its_first_frame_with_complete_grouping() {
            use crate::reference::{ReferenceView, SoundRow};
            use ratatui::{buffer::Buffer, widgets::Widget};
            use rustel_runtime::samples::{SoundCategory, SoundEntry, SoundOrigin};
            let reference = Reference::load_all();
            let sounds = (0..10_000)
                .map(|i| SoundEntry {
                    name: format!("machine{:04}_sound{:02}", i / 10, i % 10),
                    variants: 16,
                    variant_names: Vec::new(),
                    origin: SoundOrigin::Default,
                    category: SoundCategory::Drums,
                    location: Some(format!("https://example.invalid/samples/{i}.wav")),
                    import: None,
                })
                .collect::<Vec<_>>();
            let theme = Theme::built_in_default();
            let area = ratatui::layout::Rect::new(0, 0, 90, 32);
            let mut buffer = Buffer::empty(area);
            let start = Instant::now();
            let mut panel = ReferencePanel::samples(&reference, sounds);
            panel.open_categories.extend(panel.section_categories());
            let opened = start.elapsed();
            ReferenceView {
                keybinds: &crate::keybinds::Keybinds::default(),
                focused: true,
                pulse: None,
                sounding_note: None,
                loading: None,
                caching: 0,
                importing: 0,
                library_loading: false,
                #[cfg(feature = "hydra")]
                playing: None,
                #[cfg(feature = "hydra")]
                preview_note: None,
                #[cfg(feature = "hydra")]
                preview_progress: None,
                #[cfg(feature = "hydra")]
                preview_row: None,
                picture: false,
                refused: None,
                reference: &reference,
                panel: &panel,
                theme: &theme,
            }
            .render(area, &mut buffer);
            eprintln!(
                "10,000-bank cached catalogue: open {:?}, first render {:?}, total {:?}",
                opened,
                start.elapsed() - opened,
                start.elapsed()
            );
            assert_eq!(panel.sound_rows().len(), 1_000);
            assert!(
                panel
                    .sound_rows()
                    .iter()
                    .all(|row| matches!(row, SoundRow::Family(_)))
            );
            let drawn = buffer
                .content
                .iter()
                .map(|cell| cell.symbol())
                .collect::<String>();
            assert!(
                drawn.contains("machine0000"),
                "first frame contains the catalogue"
            );
            assert!(
                drawn.contains("10000 of 10000 sounds"),
                "all banks are present"
            );
        }

        /// A `.bank(…)` completion whose library arrives under the open panel
        /// refills its machines - the list never stays short of what would
        /// play, and what the reader has typed keeps filtering.
        #[test]
        fn a_bank_list_refills_when_the_library_arrives_late() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = crate::app::tests::support::app_over(directory.path());
            // The library the panel opens under has not met a bank yet.
            app.worker.set_library(std::sync::Arc::new(
                rustel_runtime::samples::SampleLibrary::empty(),
            ));
            app.editor_mut()
                .dispatch(
                    Command::PasteText("$: s(\"cymbal\").bank(\"".into()),
                    HistoryMoment(0),
                    &mut MemoryClipboard::default(),
                )
                .expect("seed");
            app.toggle_reference_browse();
            {
                let panel = app.reference_panel.as_ref().expect("panel open");
                assert!(panel.bank_vocabulary());
                assert!(panel.results.is_empty(), "no library yet, no machines");
            }
            let library = std::sync::Arc::new(rustel_runtime::samples::SampleLibrary::empty());
            library
                .register_trusted_custom(r#"{"Metal_cymbal":["http://127.0.0.1:9/a.wav"]}"#, None)
                .expect("bank");
            app.worker.set_library(std::sync::Arc::clone(&library));
            // set_library publishes the test snapshot synchronously.
            app.install_catalogue();
            let panel = app.reference_panel.as_ref().expect("panel open");
            assert_eq!(
                panel.vocabulary.as_ref().expect("vocabulary").names,
                vec!["Metal".to_owned()],
                "the machine the library brought"
            );
            assert_eq!(panel.results.len(), 1);
        }
    }

    mod reference_insertion {
        //! Tests for reference insertion.

        use crate::app::*;
        use crate::editor::MemoryClipboard;
        use crate::editor::Selection;
        use crate::reference::{PanelIntent, ReferenceMode};

        fn press(app: &mut App, code: KeyCode, modifiers: KeyModifiers) {
            app.handle_terminal_event(Event::Key(KeyEvent::new(code, modifiers)))
                .unwrap();
        }

        fn seed(app: &mut App, source: &str) {
            let caret = source.find('|').unwrap();
            let source = source.replace('|', "");
            app.editor_mut()
                .dispatch(
                    Command::SelectAll,
                    HistoryMoment(0),
                    &mut MemoryClipboard::default(),
                )
                .unwrap();
            app.editor_mut()
                .dispatch(
                    Command::PasteText(source),
                    HistoryMoment(1),
                    &mut MemoryClipboard::default(),
                )
                .unwrap();
            app.editor_mut()
                .set_selection(Selection::caret(ByteOffset(caret)))
                .unwrap();
            app.focus = Focus::Editor;
        }

        fn marked(app: &App) -> String {
            let text = app.editor().source();
            let caret = app.editor().primary_selection().head.0;
            format!("{}|{}", &text[..caret], &text[caret..])
        }

        fn choose(app: &mut App, name: &str) {
            press(app, KeyCode::Char('d'), KeyModifiers::CONTROL);
            app.set_reference_panel(Some(ReferencePanel::browse_for(&app.reference, name)));
            press(app, KeyCode::Enter, KeyModifiers::NONE);
        }

        fn assert_parameters(app: &App, name: &str) {
            let panel = app
                .reference_panel
                .as_ref()
                .expect("parameters stay visible");
            assert_eq!(panel.tab, Tab::Reference);
            let ReferenceMode::Entry { index, .. } = panel.mode else {
                panic!("not the parameter page")
            };
            assert_eq!(app.reference.entry(index).unwrap().name, name);
            assert_eq!(
                app.focus,
                Focus::Editor,
                "arguments go straight into the score"
            );
            assert!(app.reference_anchor.is_none());
        }

        #[test]
        fn ctrl_d_inserts_a_closed_call_and_keeps_its_parameters_while_typing() {
            let directory = tempfile::tempdir().unwrap();
            let mut app = crate::app::tests::support::app_over(directory.path());
            seed(&mut app, "$: |");
            press(&mut app, KeyCode::Char('d'), KeyModifiers::CONTROL);
            for letter in "lpf".chars() {
                press(&mut app, KeyCode::Char(letter), KeyModifiers::NONE);
            }
            press(&mut app, KeyCode::Enter, KeyModifiers::NONE);
            assert_eq!(marked(&app), "$: lpf(|)");
            assert_parameters(&app, "lpf");
            for letter in "800".chars() {
                press(&mut app, KeyCode::Char(letter), KeyModifiers::NONE);
            }
            assert_eq!(marked(&app), "$: lpf(800|)");
            assert_parameters(&app, "lpf");
        }

        #[test]
        fn chained_calls_existing_arguments_and_zero_argument_functions_keep_their_shape() {
            for (before, name, after) in [
                ("$: s(\"bd\").|", "gain", "$: s(\"bd\").gain(|)"),
                ("$: s(\"bd\")|", "gain", "$: s(\"bd\").gain(|)"),
                (
                    "$: s(\"bd\").gaiin|(0.5)",
                    "gain",
                    "$: s(\"bd\").gain(|0.5)",
                ),
                ("$: s(\"bd\").gain|(0.5)", "gain", "$: s(\"bd\").gain(|0.5)"),
                (
                    "$: s(\"bd\").gaiin| (0.5)",
                    "gain",
                    "$: s(\"bd\").gain (|0.5)",
                ),
                ("$: s(\"bd\").|", "rev", "$: s(\"bd\").rev(|)"),
            ] {
                let directory = tempfile::tempdir().unwrap();
                let mut app = crate::app::tests::support::app_over(directory.path());
                seed(&mut app, before);
                choose(&mut app, name);
                assert_eq!(marked(&app), after);
                assert_parameters(&app, name);
            }
        }

        #[test]
        fn a_quote_then_ctrl_d_offers_samples_and_inserts_the_choice_as_a_value() {
            let directory = tempfile::tempdir().unwrap();
            let mut app = crate::app::tests::support::app_over(directory.path());
            seed(&mut app, "$: |");
            choose(&mut app, "s");
            assert_eq!(marked(&app), "$: s(|)");
            assert_parameters(&app, "s");
            press(&mut app, KeyCode::Char('"'), KeyModifiers::NONE);
            press(&mut app, KeyCode::Char('d'), KeyModifiers::CONTROL);
            let panel = app.reference_panel.as_ref().unwrap();
            assert_eq!(panel.tab, Tab::Samples);
            assert_eq!(panel.intent, PanelIntent::Insert);
            // A synth whose name also has a reference page must remain a quoted value.
            app.confirm_reference_name("sine".into()).unwrap();
            assert_eq!(marked(&app), "$: s(\"sine|)");
            press(&mut app, KeyCode::Char('"'), KeyModifiers::NONE);
            assert_eq!(marked(&app), "$: s(\"sine\"|)");
            assert!(app.reference_panel.is_none());
            assert_eq!(app.focus, Focus::Editor);
            rustel_runtime::Session::new()
                .unwrap()
                .evaluate(&app.editor().source())
                .unwrap();
        }

        #[test]
        fn a_quote_then_ctrl_d_offers_documented_parameter_choices() {
            let directory = tempfile::tempdir().unwrap();
            let mut app = crate::app::tests::support::app_over(directory.path());
            seed(&mut app, "$: s(\"bd\").|");
            choose(&mut app, "warpmode");
            assert_parameters(&app, "warpmode");
            press(&mut app, KeyCode::Char('"'), KeyModifiers::NONE);
            press(&mut app, KeyCode::Char('d'), KeyModifiers::CONTROL);
            assert!(app.reference_panel.as_ref().unwrap().vocabulary.is_some());
            app.confirm_reference_name("asym".into()).unwrap();
            assert_eq!(marked(&app), "$: s(\"bd\").warpmode(\"asym|)");
            press(&mut app, KeyCode::Char('"'), KeyModifiers::NONE);
            assert_eq!(marked(&app), "$: s(\"bd\").warpmode(\"asym\"|)");
            assert!(app.reference_panel.is_none());
        }

        #[test]
        fn signals_stay_values_and_insertion_returns_to_editor_with_the_set_panel_open() {
            let directory = tempfile::tempdir().unwrap();
            let mut app = crate::app::tests::support::app_over(directory.path());
            seed(&mut app, "$: |");
            choose(&mut app, "sine");
            assert_eq!(marked(&app), "$: sine|");
            app.toggle_set_panel();
            seed(&mut app, "$: s(\"bd\").|");
            choose(&mut app, "gain");
            assert_parameters(&app, "gain");
            press(&mut app, KeyCode::Esc, KeyModifiers::NONE);
            assert!(app.reference_panel.is_none());
            assert_eq!(marked(&app), "$: s(\"bd\").gain(|)");
        }

        /// A global that is a value, as a fresh runtime reports it, inserts bare:
        /// `nothing()` and `sliderValues()` would throw `not a function`.
        #[test]
        fn values_the_runtime_reports_insert_bare() {
            let directory = tempfile::tempdir().unwrap();
            let mut app = crate::app::tests::support::app_over(directory.path());
            seed(&mut app, "$: |");
            choose(&mut app, "nothing");
            assert_eq!(marked(&app), "$: nothing|");
            seed(&mut app, "$: |");
            choose(&mut app, "sliderValues");
            assert_eq!(marked(&app), "$: sliderValues|");
        }

        /// ^F on a half-typed word, then a snippet: the word was reaching for
        /// it, so it goes and the snippet stands where it stood.
        #[test]
        fn a_snippet_replaces_the_word_the_reference_was_opened_on() {
            use crate::reference::Tab;
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = crate::app::tests::support::app_over(directory.path());
            app.scenes.current_mut().editor = Editor::new("$: s(\"bd\")\ngame").expect("editor");
            press(&mut app, KeyCode::End, KeyModifiers::CONTROL);
            press(&mut app, KeyCode::End, KeyModifiers::NONE);
            app.toggle_reference_browse();
            assert_eq!(
                app.reference_panel.as_ref().map(|panel| panel.tab),
                Some(Tab::Reference)
            );
            assert!(
                app.reference_anchor.is_some(),
                "opened on the word, the panel remembers it"
            );
            {
                let panel = app.reference_panel.as_mut().expect("panel");
                panel.query.clear();
                for character in "gamepad setup".chars() {
                    panel.type_char(&app.reference, character);
                }
            }
            press(&mut app, KeyCode::Enter, KeyModifiers::NONE);
            let source = app.scenes.current().editor.source();
            assert!(
                source.starts_with("$: s(\"bd\")\nconst gp = gamepad(0)"),
                "the word went and the snippet took its line: {source}"
            );
            assert!(!source.contains("game\n"), "{source}");
        }

        /// Enter on a snippet row pastes its lines into the score, whole and
        /// on lines of their own, and hands the keyboard back to the score.
        #[test]
        fn enter_on_a_snippet_pastes_it_into_the_score() {
            use crate::reference::Tab;
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = crate::app::tests::support::app_over(directory.path());
            app.scenes.current_mut().editor = Editor::new("$: s(\"bd\")").expect("editor");
            // Caret at the end of the only line: the snippet goes under it.
            press(&mut app, KeyCode::End, KeyModifiers::NONE);
            app.toggle_reference_browse();
            assert_eq!(
                app.reference_panel.as_ref().map(|panel| panel.tab),
                Some(Tab::Reference)
            );
            for character in "set tempo".chars() {
                press(&mut app, KeyCode::Char(character), KeyModifiers::NONE);
            }
            press(&mut app, KeyCode::Enter, KeyModifiers::NONE);
            assert!(app.reference_panel.is_none(), "the panel closed");
            assert_eq!(app.focus, Focus::Editor, "and the score has the keyboard");
            let source = app.scenes.current().editor.source();
            assert_eq!(
                source, "$: s(\"bd\")\nsetcpm(120/4)",
                "pasted whole, on a line of its own"
            );
        }

        /// A function chosen at the head of a chain link goes in as a call with
        /// the `.` the next link needs: at `.voicing().|s("z_tan")`, `glide`
        /// leaves `.glide(|).s("z_tan")`.
        #[test]
        #[cfg(feature = "extensions")]
        fn a_function_taken_for_a_chain_goes_in_as_a_call() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = crate::app::tests::support::app_over(directory.path());
            let seed = "$: chord(\"G^9 Bb^9\").anchor(\"c4\").voicing().s(\"z_tan\")";
            let head = |app: &App| {
                let source = app.editor().source();
                let head = app.editor().primary_selection().head.0;
                format!("{}|{}", &source[..head], &source[head..])
            };
            app.editor_mut()
                .dispatch(
                    Command::SelectAll,
                    HistoryMoment(0),
                    &mut MemoryClipboard::default(),
                )
                .expect("all");
            app.editor_mut()
                .dispatch(
                    Command::PasteText(seed.into()),
                    HistoryMoment(1),
                    &mut MemoryClipboard::default(),
                )
                .expect("seed");
            // The caret at the head of the last link, where a new one goes.
            let at = app
                .editor()
                .source()
                .find("s(\"z_tan\")")
                .expect("the link");
            app.editor_mut()
                .set_selection(Selection::caret(ByteOffset(at)))
                .expect("caret");
            app.focus = Focus::Editor;
            // Ctrl+Space, then the name typed into the panel's search box, then
            // Enter: the whole gesture, as it is actually made.
            press(&mut app, KeyCode::Char(' '), KeyModifiers::CONTROL);
            assert_eq!(app.focus, Focus::Panel(PanelKind::Reference));
            for letter in "glide".chars() {
                press(&mut app, KeyCode::Char(letter), KeyModifiers::NONE);
            }
            press(&mut app, KeyCode::Enter, KeyModifiers::NONE);
            assert_eq!(
                head(&app),
                "$: chord(\"G^9 Bb^9\").anchor(\"c4\").voicing().glide(|).s(\"z_tan\")"
            );
            assert_eq!(
                app.reference_panel
                    .as_ref()
                    .and_then(|panel| panel.showing(&app.reference))
                    .as_deref(),
                Some("glide"),
                "the inserted call's parameters remain visible"
            );
            assert_eq!(app.focus, Focus::Editor);

            // The end of one link is the head of the next: a chain resting at
            // `)` takes the call after it, `.` and all.
            let end = ByteOffset(app.editor().source().trim_end().len());
            app.editor_mut()
                .set_selection(Selection::caret(end))
                .expect("caret");
            app.focus = Focus::Editor;
            app.toggle_reference_browse();
            app.insert_reference_name("room".into()).expect("insert");
            assert_eq!(
                head(&app),
                "$: chord(\"G^9 Bb^9\").anchor(\"c4\").voicing().glide().s(\"z_tan\").room(|)"
            );

            // With the name half typed in front of an existing link, what is
            // taken replaces only what was typed: `glide` goes in ahead of `s`,
            // it does not eat it.
            app.editor_mut()
                .dispatch(
                    Command::SelectAll,
                    HistoryMoment(2),
                    &mut MemoryClipboard::default(),
                )
                .expect("all");
            app.editor_mut()
                .dispatch(
                    Command::PasteText("$: note(\"c4\").voicing().glis(\"z_tan\")".into()),
                    HistoryMoment(3),
                    &mut MemoryClipboard::default(),
                )
                .expect("seed");
            let at = app
                .editor()
                .source()
                .find("s(\"z_tan\")")
                .expect("the link");
            app.editor_mut()
                .set_selection(Selection::caret(ByteOffset(at)))
                .expect("caret");
            app.focus = Focus::Editor;
            app.toggle_reference_browse();
            app.insert_reference_name("glide".into()).expect("insert");
            assert_eq!(
                head(&app),
                "$: note(\"c4\").voicing().glide(|).s(\"z_tan\")"
            );

            // A call that already has its brackets keeps them: its name is
            // being put right, not a link going in.
            app.editor_mut()
                .dispatch(
                    Command::SelectAll,
                    HistoryMoment(4),
                    &mut MemoryClipboard::default(),
                )
                .expect("all");
            app.editor_mut()
                .dispatch(
                    Command::PasteText("$: note(\"c4\").gaiin(0.5)".into()),
                    HistoryMoment(5),
                    &mut MemoryClipboard::default(),
                )
                .expect("seed");
            let at = app.editor().source().find("(0.5)").expect("the brackets");
            app.editor_mut()
                .set_selection(Selection::caret(ByteOffset(at)))
                .expect("caret");
            app.focus = Focus::Editor;
            app.toggle_reference_browse();
            app.insert_reference_name("gain".into()).expect("insert");
            assert_eq!(head(&app), "$: note(\"c4\").gain(|0.5)");
        }
    }

    mod reference_panel {
        //! Tests for `app/reference_panel.rs`.

        use crate::app::tests::support::*;
        use crate::app::*;
        use crate::editor::{MemoryClipboard, Selection};

        #[test]
        fn ctrl_d_explains_the_function_while_editing_its_arguments() {
            let directory = tempfile::tempdir().unwrap();
            for (marked, expected) in [
                ("$: s(\"bd\").distort(\"0.1:0.3:|", "distort"),
                ("$: distort(\"0.1:|0.3:fold\")", "distort"),
                ("$: distort(\"gain|\")", "distort"),
                ("$: distort(amount|)", "distort"),
                ("$: distort(gain|)", "distort"),
                ("$: distort(0.1, |)", "distort"),
                ("$: distort(\n  amount + |\n)", "distort"),
                ("$: distort([0.1, 0.3]|)", "distort"),
                ("$: distort(sine(1)|)", "distort"),
                ("$: distort(sine(freq|))", "sine"),
                ("$: distort(si|ne(1))", "sine"),
                ("$: distort(|sine(1))", "distort"),
                ("$: distort(sine|(1))", "sine"),
                ("$: lfo({ min: 0.1, max: amount| })", "lfo"),
                ("$: lfo({ control: \"gain|\" })", "lfo"),
                ("$: distort(amount /* gain|( */)", "distort"),
                ("$: distort(/gain|(/)", "distort"),
                ("$: distort(`0.1:\n0.3:|`)", "distort"),
            ] {
                let mut app = app_over(directory.path());
                let caret = ByteOffset(marked.find('|').unwrap());
                let source = marked.replace('|', "");
                app.scenes.current_mut().editor = Editor::new(&source).unwrap();
                app.editor_mut()
                    .set_selection(Selection::caret(caret))
                    .unwrap();

                press(&mut app, KeyCode::Char('d'), KeyModifiers::CONTROL);

                let panel = app.reference_panel.as_ref().expect(marked);
                let crate::reference::ReferenceMode::Entry { index, .. } = panel.mode else {
                    panic!("expected parameter help for {marked}");
                };
                assert_eq!(
                    app.reference.entry(index).unwrap().name,
                    expected,
                    "{marked}"
                );
                assert_eq!(app.focus, Focus::Panel(PanelKind::Reference), "{marked}");
                assert!(app.reference_anchor.is_none(), "{marked}");
                assert_eq!(app.editor().source(), source, "{marked}");
                assert_eq!(app.editor().primary_selection(), Selection::caret(caret));

                press(&mut app, KeyCode::Char('d'), KeyModifiers::CONTROL);
                assert!(app.reference_panel.is_none(), "{marked}");
                assert_eq!(app.focus, Focus::Editor, "{marked}");
                assert_eq!(app.editor().source(), source, "{marked}");
                assert_eq!(app.editor().primary_selection(), Selection::caret(caret));
            }
        }

        #[test]
        fn ctrl_d_opens_a_visible_reference_overlay_in_zen() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = crate::app::tests::support::app_over(directory.path());
            app.frame = Rect::new(0, 0, 120, 40);
            press(&mut app, KeyCode::F(11), KeyModifiers::NONE);

            press(&mut app, KeyCode::Char('d'), KeyModifiers::CONTROL);

            assert!(
                app.reference_panel.is_some(),
                "Ctrl+D did not open reference"
            );
            assert_eq!(app.focus, Focus::Panel(PanelKind::Reference));
            let layout = view::regions(
                app.frame,
                app.ui_settings.zen,
                app.panes.len(),
                app.reference_panel.is_some(),
                false,
                None,
                None,
            );
            assert!(
                !layout.reference.is_empty(),
                "reference is invisible in zen"
            );
            assert_eq!(layout.panes[0].editor, app.frame);
            assert!(layout.reference_overlays);
        }

        #[test]
        fn backspace_from_an_empty_contextual_picker_returns_to_function_arguments() {
            use crate::reference::{ReferenceMode, Tab};

            let directory = tempfile::tempdir().expect("temp dir");
            for (marked, name) in [
                (r#"$: s("|")"#, "s"),
                (r#"$: chord("|")"#, "chord"),
                (r#"$: n(0).scale("|")"#, "scale"),
                #[cfg(feature = "extensions")]
                (r#"$: n(0).inspire("|")"#, "inspire"),
                (r#"$: s("bd").warpmode("|")"#, "warpmode"),
                (r#"$: s("bd").bank("|")"#, "bank"),
            ] {
                let caret = marked.find('|').expect("caret marker");
                let source = marked.replace('|', "");
                let mut app = app_over(directory.path());
                app.scenes.current_mut().editor = Editor::new(&source).expect("editor");
                app.editor_mut()
                    .set_selection(Selection::caret(ByteOffset(caret)))
                    .expect("caret");
                press(&mut app, KeyCode::Char('d'), KeyModifiers::CONTROL);
                assert!(
                    app.reference_anchor.is_some(),
                    "{marked}: a contextual picker"
                );

                press(&mut app, KeyCode::Backspace, KeyModifiers::NONE);

                let panel = app
                    .reference_panel
                    .as_ref()
                    .expect("arguments stay visible");
                assert_eq!(panel.tab, Tab::Reference, "{marked}");
                let ReferenceMode::Entry { index, .. } = panel.mode else {
                    panic!("{marked}: expected the enclosing function's arguments")
                };
                assert_eq!(app.reference.entry(index).unwrap().name, name, "{marked}");
                assert!(panel.vocabulary.is_none(), "{marked}");
                assert!(
                    app.reference_anchor.is_none(),
                    "{marked}: discard the value anchor"
                );
                assert_eq!(
                    app.editor().source(),
                    source,
                    "{marked}: Backspace must not edit"
                );
                assert_eq!(app.editor().primary_selection().head, ByteOffset(caret));
            }
        }

        #[test]
        fn backspace_edits_a_contextual_search_before_returning_to_function_arguments() {
            use crate::reference::{ReferenceMode, Tab};

            let directory = tempfile::tempdir().expect("temp dir");
            let source = "$: s(\"su\")";
            let caret = source.find("su").expect("sound") + 2;
            let mut app = app_over(directory.path());
            app.scenes.current_mut().editor = Editor::new(source).expect("editor");
            app.editor_mut()
                .set_selection(Selection::caret(ByteOffset(caret)))
                .expect("caret");
            press(&mut app, KeyCode::Char('d'), KeyModifiers::CONTROL);

            for query in ["s", ""] {
                press(&mut app, KeyCode::Backspace, KeyModifiers::NONE);
                let panel = app.reference_panel.as_ref().expect("picker stays open");
                assert_eq!(panel.tab, Tab::Samples);
                assert_eq!(panel.sound_query, query);
                assert!(app.reference_anchor.is_some());
                assert_eq!(app.editor().source(), source);
            }

            press(&mut app, KeyCode::Backspace, KeyModifiers::NONE);
            let panel = app.reference_panel.as_ref().expect("function arguments");
            assert_eq!(panel.tab, Tab::Reference);
            let ReferenceMode::Entry { index, .. } = panel.mode else {
                panic!("expected sound function arguments")
            };
            assert_eq!(app.reference.entry(index).unwrap().name, "s");
            assert!(app.reference_anchor.is_none());
            assert_eq!(app.editor().source(), source);
            assert_eq!(app.editor().primary_selection().head, ByteOffset(caret));
        }

        /// The chord walks its three states: open+focus, refocus, close.
        #[test]
        fn the_chord_comes_back_before_it_closes() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = crate::app::tests::support::app_over(directory.path());
            app.toggle_reference_browse();
            assert!(app.reference_panel.is_some());
            app.focus = Focus::Editor;
            app.toggle_reference_browse();
            assert!(
                app.reference_panel.is_some(),
                "first press comes back to it"
            );
            assert_eq!(app.focus, Focus::Panel(PanelKind::Reference));
            app.toggle_reference_browse();
            assert!(app.reference_panel.is_none(), "second press puts it away");
            assert_eq!(app.focus, Focus::Editor);
        }

        /// Every key for the reference opens and closes the panel. macOS takes
        /// Ctrl+Space by default, and F2 arrives only on a Mac that sends real
        /// function keys, so Ctrl+F is the key that always arrives there.
        #[test]
        fn every_spelling_reaches_the_reference_browser_when_ctrl_space_never_arrives() {
            let directory = tempfile::tempdir().expect("temp dir");
            for (code, modifiers) in [
                // The one with nothing in its way.
                (KeyCode::Char('f'), KeyModifiers::CONTROL),
                (KeyCode::F(2), KeyModifiers::NONE),
                (KeyCode::Char(' '), KeyModifiers::CONTROL),
                // What a legacy terminal sends for Ctrl+Space, where it arrives.
                (KeyCode::Null, KeyModifiers::NONE),
            ] {
                let mut app = crate::app::tests::support::app_over(directory.path());
                assert!(app.reference_panel.is_none(), "{code:?} starts closed");
                press(&mut app, code, modifiers);
                assert!(
                    app.reference_panel.is_some(),
                    "{code:?} with {modifiers:?} did not open the reference"
                );
                press(&mut app, code, modifiers);
                assert!(
                    app.reference_panel.is_none(),
                    "{code:?} with {modifiers:?} did not close it again"
                );
            }
        }

        /// `c` copies the dragged fragment and leaves the panel standing; with
        /// nothing dragged it keeps its old meaning, whole-chain and closing.
        #[cfg(feature = "hydra")]
        #[test]
        fn c_copies_the_dragged_selection_and_leaves_the_panel_open() {
            use crate::reference::{PaneSelection, SelectionTarget, Tab};
            use crate::textblock::{TextPoint, TextSelection};
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = crate::app::tests::support::app_over(directory.path());
            app.toggle_reference_browse();
            app.regions = view::regions(Rect::new(0, 0, 120, 40), false, 1, true, true, None, None);
            {
                let panel = app.reference_panel.as_mut().expect("panel");
                panel.tab = Tab::Examples;
                panel.snippet_open.insert((0, 0));
                panel.snippet_selected = 2;
                let inner = crate::reference::inner_area(app.regions.reference);
                let rows = panel.snippet_layout(inner).code_rows();
                let lines =
                    panel.snippet_code_lines(usize::from(rows.height), usize::from(rows.width));
                assert!(!lines.is_empty());
                panel.selection = Some(PaneSelection {
                    target: SelectionTarget::Snippet {
                        row: panel.snippet_selected,

                        width: rows.width,
                    },
                    selection: TextSelection {
                        anchor: TextPoint { line: 0, column: 0 },
                        head: TextPoint {
                            line: 0,
                            column: lines[0].chars().count(),
                        },
                    },
                });
            }
            press(&mut app, KeyCode::Char('c'), KeyModifiers::NONE);
            assert!(
                app.reference_panel.is_some(),
                "a fragment copy is not the end of reading"
            );

            // Released, `c` means the whole snippet again - and that leaves the
            // shelf open too: one take is rarely the end of it.
            app.reference_panel.as_mut().expect("panel").selection = None;
            press(&mut app, KeyCode::Char('c'), KeyModifiers::NONE);
            assert!(
                app.reference_panel.is_some(),
                "taking a whole snippet keeps the shelf open"
            );
        }

        /// A drag begun on the hint row below the entry's text anchors at the
        /// right end of the last line of text, as a drag from a pane's blank
        /// half does in an editor.
        #[test]
        fn a_drag_begun_below_the_entry_selects_from_its_text_edge() {
            use crate::reference::{ReferenceMode, Tab};
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = crate::app::tests::support::app_over(directory.path());
            app.editor_mut()
                .dispatch(
                    Command::PasteText("s(".into()),
                    HistoryMoment(0),
                    &mut MemoryClipboard::default(),
                )
                .expect("seed");
            app.toggle_reference_browse();
            let index = app.reference.lookup("s").expect("the s entry");
            {
                let panel = app.reference_panel.as_mut().expect("panel");
                panel.tab = Tab::Reference;
                panel.mode = ReferenceMode::Entry {
                    index,
                    scroll: 0,
                    from_browse: false,
                };
            }
            app.focus_panel(PanelKind::Reference);
            app.frame = Rect::new(0, 0, 120, 40);
            app.regions = view::regions(app.frame, false, 1, true, true, None, None);

            let inner = crate::reference::inner_area(app.regions.reference);
            let body = crate::reference::entry_body_area(inner);
            let hint_row = body.bottom();
            let entry = app.reference.entry(index).expect("entry");
            let lines: Vec<String> = crate::reference::entry_body(entry, usize::from(inner.width))
                .into_iter()
                .map(|line| line.text)
                .collect();
            let first_line = 0;
            let block = crate::textblock::TextBlock {
                lines: &lines,
                area: body,
                first_line,
            };
            // What a press IN the text at the hint row would have read, before
            // the block claimed the row beside it.
            let expected = block
                .point_at(inner.x + 2, body.y + 1, crate::textblock::Clamp::Inside)
                .expect("text exists");
            // A press on the hint row below the body starts a selection. Its
            // anchor is the end of the last visible line.
            app.handle_mouse(MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: inner.x + 2,
                row: hint_row,
                modifiers: KeyModifiers::NONE,
            })
            .expect("press below the body");
            let panel = app.reference_panel.as_ref().expect("panel");
            let held = panel.selection.as_ref().expect("the press began a drag");
            let last_index = lines
                .iter()
                .rposition(|line| !line.is_empty())
                .expect("the body has text");
            let to = held.selection.ordered().1;
            assert_eq!(
                to,
                crate::textblock::TextPoint {
                    line: last_index,
                    column: lines[last_index].chars().count(),
                },
                "the anchor is the text's right end: the last visible line set in ink"
            );
            // The head follows the drag into the text: the selection then reads
            // as if it had started at the text's edge under the pointer.
            app.handle_mouse(MouseEvent {
                kind: MouseEventKind::Drag(MouseButton::Left),
                column: inner.x + 2,
                row: body.y + 1,
                modifiers: KeyModifiers::NONE,
            })
            .expect("drag into the text");
            let panel = app.reference_panel.as_ref().expect("panel");
            let held = panel.selection.as_ref().expect("drag held");
            let (from, _) = held.selection.ordered();
            assert_eq!(from, expected, "the selection reads from the text");
            assert!(
                !held.selection.is_empty(),
                "dragging up from the hint row takes text with it"
            );
        }

        #[test]
        fn reference_on_an_unfinished_alias_offers_functions_before_arguments() {
            use crate::reference::ReferenceMode;

            let directory = tempfile::tempdir().unwrap();
            let mut app = app_over(directory.path());
            let source = "$: s(\"bd\").lp";
            app.scenes.current_mut().editor = Editor::new(source).unwrap();
            app.editor_mut()
                .set_selection(Selection::caret(ByteOffset(source.len())))
                .unwrap();
            app.focus = Focus::Editor;
            press(&mut app, KeyCode::Char('d'), KeyModifiers::CONTROL);
            assert_eq!(app.focus, Focus::Panel(PanelKind::Reference));
            let panel = app.reference_panel.as_mut().unwrap();
            assert_eq!(panel.mode, ReferenceMode::Browse);
            let names: Vec<_> = panel
                .results
                .iter()
                .map(|&index| app.reference.entry(index).unwrap().name.as_str())
                .collect();
            assert!(names.contains(&"lpf"), "{names:?}");
            panel.selected =
                panel.row_of_result(names.iter().position(|&name| name == "lpenv").unwrap());
            press(&mut app, KeyCode::Enter, KeyModifiers::NONE);
            assert_eq!(app.editor().source(), "$: s(\"bd\").lpenv()");
            assert_eq!(
                app.editor().primary_selection().head.0,
                app.editor().source().len() - 1
            );
            assert_eq!(
                app.reference_panel
                    .as_ref()
                    .unwrap()
                    .showing(&app.reference)
                    .as_deref(),
                Some("lpenv")
            );
            assert_eq!(app.focus, Focus::Editor);

            let source = "$: s(\"bd\").lp(300)";
            app.set_reference_panel(None);
            app.scenes.current_mut().editor = Editor::new(source).unwrap();
            app.editor_mut()
                .set_selection(Selection::caret(ByteOffset(source.find("lp").unwrap() + 2)))
                .unwrap();
            press(&mut app, KeyCode::Char('d'), KeyModifiers::CONTROL);
            assert_eq!(
                app.reference_panel
                    .as_ref()
                    .unwrap()
                    .showing(&app.reference)
                    .as_deref(),
                Some("lpf")
            );
            assert_eq!(app.editor().source(), source);
        }

        #[cfg(feature = "hydra")]
        #[test]
        fn s_toggles_the_example_code_between_stack_and_voices() {
            use crate::reference::{ReferencePanel, SnippetLine};
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = crate::app::tests::support::app_over(directory.path());
            // The Tracks section's first shelf opens onto a genre kit, which is
            // stored in the JSON catalogue.
            let mut panel = ReferencePanel::snippets(&app.reference);
            panel.section_open.clear();
            panel.section_open.insert(1);
            panel.snippet_open.clear();
            panel.snippet_open.insert((1, 0));
            let lines = panel.snippet_lines();
            let row = lines
                .iter()
                .position(|line| matches!(line, SnippetLine::Snippet(1, 0, 0)))
                .expect("the first track example");
            panel.snippet_selected = row;
            app.reference_panel = Some(panel);
            app.focus_panel(PanelKind::Reference);

            let shown = |app: &App| {
                app.reference_panel
                    .as_ref()
                    .expect("panel")
                    .selected_snippet_code()
                    .expect("a kit")
                    .into_owned()
            };
            // The default reading is one `$:` a voice…
            let lanes = shown(&app);
            assert!(!lanes.contains("stack("), "split into voices:\n{lanes}");
            assert!(lanes.lines().count() >= 2, "{lanes}");
            // …s folds to the stack…
            app.handle_reference_key(KeyCode::Char('s'), false, false)
                .expect("s");
            let stack = shown(&app);
            assert!(stack.contains("$: stack("), "folded:\n{stack}");
            // …s again unfolds - the toggle is live, not one-way…
            app.handle_reference_key(KeyCode::Char('s'), false, false)
                .expect("s");
            assert_eq!(shown(&app), lanes, "back to the voices");
            // …and Enter copies whichever reading is on screen.
            app.handle_reference_key(KeyCode::Enter, false, false)
                .expect("Enter");
            assert_eq!(
                app.clipboard.get_text().ok().as_deref(),
                Some(lanes.as_str())
            );
        }

        /// The browser lists every import the score names, each with where it
        /// stands; one nobody has asked the library for yet is on its way.
        #[test]
        fn the_browser_lists_the_scores_imports_as_they_stand() {
            let library = rustel_runtime::samples::SampleLibrary::empty();
            library.note_samples_source_state_for_tests(
                "github:me/gone",
                rustel_runtime::samples::SourceState::Failed("refused".into()),
            );
            let imports = browser_imports(
                "samples('github:me/gone')\nsamples('github:me/new')\nsamples('github:me/new')",
                &crate::catalogue::Snapshot {
                    sounds: Vec::new(),
                    sources: library.browser_source_states(),
                },
            );
            assert_eq!(
                imports,
                [
                    (
                        "github:me/gone".to_owned(),
                        rustel_runtime::samples::SourceState::Failed("refused".into())
                    ),
                    (
                        "github:me/new".to_owned(),
                        rustel_runtime::samples::SourceState::Loading
                    ),
                ]
            );
        }

        /// Esc on the samples tab closes the browser outright.
        #[test]
        fn esc_closes_the_samples_browser() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = crate::app::tests::support::app_over(directory.path());
            let sounds = vec![rustel_runtime::samples::SoundEntry {
                name: "bd".into(),
                variants: 3,
                variant_names: Vec::new(),
                origin: rustel_runtime::samples::SoundOrigin::Default,
                category: rustel_runtime::samples::SoundCategory::Drums,
                location: None,
                import: None,
            }];
            app.reference_panel = Some(ReferencePanel::samples(&app.reference, sounds));
            app.focus = Focus::Panel(PanelKind::Reference);
            press(&mut app, KeyCode::Right, KeyModifiers::NONE);
            // A preview still sounding does not cost a press of its own: Esc
            // closes the browser and silences it in one go.
            app.preview_armed = Some(("bd:0".into(), Instant::now()));
            press(&mut app, KeyCode::Esc, KeyModifiers::NONE);
            assert!(app.reference_panel.is_none(), "the browser is gone");
            assert_eq!(app.preview_armed, None, "and the preview with it");
        }
    }
}
