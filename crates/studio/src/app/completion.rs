//! Caret-context completion: works out what the caret is inside (the
//! enclosing call, the string argument and what it stands for, or the head of a
//! chain link), then opens the reference column as a list anchored to that
//! spot. Ctrl+Space and call/string completion start here, along with the
//! anchor and landing rules that say where a chosen name goes and how it is
//! written (bare, quoted, or as a call).
//!
//! String roles live in `string_roles.rs`; bank inference lives in
//! `bank_context.rs`.

use super::string_roles::StringRole;
use super::*;
use crate::reference::{ReferenceMode, ReferenceScope};
use rustel_runtime::lint::scan;

/// The word the reference column was opened on.
#[derive(Clone, Debug)]
pub(super) struct ReferenceAnchor {
    pub(super) scene: SceneId,
    revision: Revision,
    pub(super) word: String,
    range: Range<ByteOffset>,
    /// How the chosen name is written where it lands.
    pub(super) landing: Landing,
}

/// How a name chosen in the reference column is written into the score.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Landing {
    /// The name itself: a sound inside a string, a misspelt function put
    /// right.
    Bare,
    /// A node property, without call brackets but joined into its chain.
    Member,
    /// In quotes: the caret was in a call that takes a string but no
    /// quotes had been typed yet, so `chord(` becomes `chord("C^7")`
    /// rather than `chord(C^7)`.
    Quoted,
    /// As a call - `glide()` - with the caret between its brackets, and
    /// the `.` the link it was put in front of now needs. A function
    /// taking its place in a chain is a call, not a word, and what you do
    /// next is give it its argument.
    Call,
}

impl App {
    /// The word under the caret and where it sits, when there is one.
    pub(super) fn word_range_at_caret(&self) -> Option<(String, Range<ByteOffset>)> {
        let editor = self.editor();
        let (word, range) = word_before(editor.document(), editor.primary_selection().head)?;
        let source = editor.source();
        let qualified = reference_name_range(&source, range.start.0..range.end.0);
        if qualified.start == range.start.0 {
            Some((word, range))
        } else {
            Some((
                source[qualified.clone()].to_owned(),
                ByteOffset(qualified.start)..ByteOffset(qualified.end),
            ))
        }
    }

    /// A callee uses the namespace before its name. Its arguments can
    /// switch namespace themselves, as `K` and `S` do.
    pub(super) fn reference_scope_at(&self, at: ByteOffset) -> ReferenceScope {
        reference_scope(&self.editor().source(), at.0)
    }

    pub(super) fn reference_scope_at_caret(&self) -> ReferenceScope {
        self.reference_scope_at(self.editor().primary_selection().head)
    }

    /// A node member shares graph documentation with its global form, but
    /// only members belong in the list behind that documentation.
    pub(super) fn reference_name_scope_at(&self, at: ByteOffset) -> ReferenceScope {
        let scope = self.reference_scope_at(at);
        if scope.is_kabelsalat() && self.editor().source()[..at.0].trim_end().ends_with('.') {
            ReferenceScope::KabelsalatMember
        } else {
            scope
        }
    }

    fn chain_reference_scope(&self, range: &Range<ByteOffset>) -> ReferenceScope {
        let scope = self.reference_name_scope_at(range.start);
        if scope.is_kabelsalat() && range.is_empty() {
            ReferenceScope::KabelsalatMember
        } else {
            scope
        }
    }

    fn completion_scope_at_caret(&self) -> ReferenceScope {
        if let Some(range) = self.chain_position_at_caret() {
            return self.chain_reference_scope(&range);
        }
        let caret = self.editor().primary_selection().head;
        let scope = self.reference_scope_at(caret);
        let name = self
            .word_range_at_caret()
            .filter(|(_, range)| range.start < caret && caret <= range.end)
            .or_else(|| self.enclosing_call_at_caret());
        name.filter(|(_, range)| self.reference_scope_at(range.start) == scope)
            .map_or(scope, |(_, range)| {
                self.reference_name_scope_at(range.start)
            })
    }

    /// The name and range of the innermost call whose `(` is open at the
    /// caret, as in `setcpm(|`. Brackets in strings, comments and regexes
    /// are ignored. Parentheses without a preceding name only group, and
    /// the walk continues outward.
    pub(super) fn enclosing_call_at_caret(&self) -> Option<(String, Range<ByteOffset>)> {
        self.enclosing_call_argument_at_caret()
            .map(|(callee, range, _)| (callee, range))
    }

    /// The enclosing call and its zero-based argument at the caret. Only
    /// commas at the call's own depth advance the argument; a comma in a
    /// string, comment, regex, nested call, array or object does not.
    pub(super) fn enclosing_call_argument_at_caret(
        &self,
    ) -> Option<(String, Range<ByteOffset>, usize)> {
        let editor = self.editor();
        let caret = editor.primary_selection().head;
        let source = editor.source();
        let (name, argument) = enclosing_call_argument(&source, caret.0)?;
        let range = ByteOffset(name.start)..ByteOffset(name.end);
        Some((source[name].to_owned(), range, argument))
    }

    /// The object key whose value is the string under the caret. This keeps
    /// a finite choice belonging to `lfo({ shape: "…" })` from appearing for
    /// unrelated strings such as `lfo({ control: "gain" })`.
    pub(super) fn object_key_before_string_at_caret(&self) -> Option<String> {
        let editor = self.editor();
        let caret = editor.primary_selection().head;
        let document = editor.document();
        let line = document.line_of(caret).ok()?;
        let start = document.line_start(line);
        let text = document.slice(start..caret).ok()?;
        let (open, _) = open_quote(text.as_bytes())?;
        let source = editor.source();
        let before = source[..start.0 + open].trim_end();
        let key_end = before.strip_suffix(':')?.trim_end();
        if let Some(quote) = key_end
            .as_bytes()
            .last()
            .copied()
            .filter(|byte| matches!(byte, b'"' | b'\''))
        {
            let quoted = &key_end[..key_end.len() - 1];
            let from = quoted.rfind(char::from(quote))? + 1;
            let key = &quoted[from..];
            return (!key.is_empty()).then(|| key.to_owned());
        }
        // The key starts after the separator's complete UTF-8 character.
        let from = key_end
            .char_indices()
            .rev()
            .find(|(_, character)| !(character.is_ascii_alphanumeric() || *character == '_'))
            .map_or(0, |(index, character)| index + character.len_utf8());
        let key = &key_end[from..];
        (!key.is_empty()).then(|| key.to_owned())
    }

    /// The double-quoted string the caret stands inside and the call it
    /// belongs to, for completion: `("s", "Akai", range-of-Akai)` with the
    /// caret after `s("Akai`. The names valid in there are sounds, scales
    /// or chords - never functions.
    pub(super) fn string_completion_at_caret(&self) -> Option<(String, String, Range<ByteOffset>)> {
        let editor = self.editor();
        let caret = editor.primary_selection().head;
        let document = editor.document();
        let line = document.line_of(caret).ok()?;
        let start = document.line_start(line);
        let text = document.slice(start..caret).ok()?;
        let bytes = text.as_bytes();
        let (open, quote) = open_quote(bytes)?;
        let source = editor.source();
        let object_callee = self
            .object_key_before_string_at_caret()
            .and_then(|_| self.enclosing_call_at_caret().map(|(callee, _)| callee));
        let callee = object_callee
            .or_else(|| {
                rustel_runtime::lint::callee_before(&source, start.0 + open).map(str::to_owned)
            })
            .or_else(|| self.enclosing_call_at_caret().map(|(callee, _)| callee))?;
        // The token under the caret, which is what a chosen name replaces.
        // A mini-notation string holds several - `"bd:1 sd:3 hh:4"` - and
        // the one being asked about is the one the caret stands in, colon
        // and all: standing anywhere in `sd:3`, choosing `piano:1` must
        // leave `bd:1 piano:1 hh:4`. So a colon between two word
        // characters belongs to the token, in both directions; a colon at
        // either end does not.
        let content = &bytes[open + 1..];
        // A chord writes its quality in marks no other name uses: `G^9`,
        // `A-7`, `C+`, `F#7b9`. They are part of the chord, so the token
        // has to hold them - or the chosen chord lands inside the old one
        // and `G^9` becomes `G^Db-7`.
        let role = self.string_role(&callee);
        let chord = role == Some(StringRole::Chord);
        // A documented choice can be hyphenated - `large-male` - and the
        // hyphen is part of it, or the chosen name lands inside the old one
        // and `large-male` becomes `large-the-fly`.
        let hyphenated = self
            .string_choice_set(&callee)
            .is_some_and(|set| set.choices.iter().any(|choice| choice.value.contains('-')));
        let word_byte = move |byte: u8| {
            // An apostrophe belongs to a name - `s("bell'")` - but never
            // when it is the string's own delimiter, or the scan walks
            // straight through the closing quote and the replacement eats
            // it. `midin`/`midikeys` need single quotes, so this is every
            // completion they offer.
            byte != quote
                && (byte.is_ascii_alphanumeric()
                    || matches!(byte, b'_' | b'#' | b'\'')
                    || (chord && matches!(byte, b'^' | b'-' | b'+'))
                    || (hyphenated && byte == b'-'))
        };
        // A sound and a scale are each written as one word with a colon
        // in it - `bd:3`, `C:major` - and the whole of it is replaced. A
        // chord's colon is not its own (`G:7b9` voices nothing), so that
        // token stops where the word does.
        let sound = matches!(role, Some(StringRole::Sound | StringRole::Scale));
        let joined = |left: Option<u8>, right: Option<u8>| {
            sound && left.is_some_and(word_byte) && right.is_some_and(word_byte)
        };
        let mut from = content.len();
        loop {
            while from > 0 && word_byte(content[from - 1]) {
                from -= 1;
            }
            // Step over a colon that has a word on both sides.
            if from > 1
                && content[from - 1] == b':'
                && joined(
                    content.get(from - 2).copied(),
                    content.get(from).copied().or(Some(b'x')),
                )
            {
                from -= 1;
                continue;
            }
            break;
        }
        // What is typed so far, for the search box. A sound searches by
        // its bank, so the `:3` half of the token is not part of the
        // question.
        let typed = String::from_utf8_lossy(&content[from..]).into_owned();
        let prefix = match role {
            // A sound searches by its bank: the `:3` is not the question.
            Some(StringRole::Sound) => typed
                .split_once(':')
                .map(|(bank, _)| bank.to_owned())
                .unwrap_or(typed),
            _ => typed,
        };
        // The token continues past the caret: what is chosen replaces the
        // WHOLE of it, so `c:mi|nor` becomes `c:major`, not `c:majornor`,
        // and `spa|ce:11` becomes `piano`, not `piano:11`.
        let rest = source.as_bytes();
        let mut to = caret.0;
        loop {
            while to < rest.len() && word_byte(rest[to]) {
                to += 1;
            }
            if rest.get(to) == Some(&b':')
                && joined(
                    to.checked_sub(1).and_then(|at| rest.get(at)).copied(),
                    rest.get(to + 1).copied(),
                )
            {
                to += 1;
                continue;
            }
            break;
        }
        // A MIDI port is one name, and real ones have spaces in them -
        // "IAC Driver Bus 1", "Bass Station II". The token scan stops at
        // the space, and a choice that replaced one token would leave the
        // other words of the old name: 'MY MIDI DEVICE' would become
        // 'MY DEVICE2 DEVICE'. For these two roles the whole of the string
        // is what a choice replaces. A plugin name and a preset name have
        // spaces too, and take the same rule. Every other vocabulary here is
        // deliberately space-free and keeps its token, because a pattern
        // string holds several names and only the one under the caret is
        // the question.
        if role.is_some_and(StringRole::whole_string) {
            let inside = start.0 + open + 1;
            let mut close = inside;
            let bytes = source.as_bytes();
            while close < bytes.len() && bytes[close] != b'\n' {
                if bytes[close] == quote && bytes[close - 1] != b'\\' {
                    break;
                }
                close += 1;
            }
            // And the query is the whole name too, not the word the caret
            // happens to be in: searching a port list for `MIDI` because
            // that was the middle word of the old name is not the question
            // anyone was asking.
            let typed = source[inside..close].trim().to_owned();
            return Some((callee, typed, ByteOffset(inside)..ByteOffset(close)));
        }
        let range = ByteOffset(start.0 + open + 1 + from)..ByteOffset(to);
        Some((callee, prefix, range))
    }

    /// The list a call's argument is drawn from, when the caret is inside
    /// the call but no string has been opened yet: `chord(|)` asks for a
    /// chord as surely as `chord("|")` does. Only calls whose argument is
    /// one of our lists - a sound, a scale, a chord, a colour - answer
    /// here; anything else falls through to the call's own reference
    /// entry, since a number or a mini-notation pattern is not a list.
    fn call_completion_panel(&mut self) -> Option<ReferencePanel> {
        if self.string_completion_at_caret().is_some() {
            return None;
        }
        let (callee, _) = self.enclosing_call_at_caret()?;
        // Only where an argument would start and nothing has been typed
        // there: right after the bracket or a comma, spaces aside.
        // `chord(C` is a word being typed, and the word path answers it.
        let editor = self.editor();
        let caret = editor.primary_selection().head;
        let document = editor.document();
        let line = document.line_of(caret).ok()?;
        let start = document.line_start(line);
        let text = document.slice(start..caret).ok()?;
        let before = text.trim_end_matches(' ');
        if !before.ends_with('(') && !before.ends_with(',') {
            return None;
        }
        let panel = self.list_panel_for(&callee, "")?;
        self.reference_anchor = None;
        self.anchor_reference_as(&callee, caret..caret, Landing::Quoted);
        Some(panel)
    }

    /// The list a call takes, filtered to what is typed so far. `None`
    /// for a call whose argument is not a list we keep - a number, a
    /// mini-notation pattern - which is what sends the chord back to the
    /// call's own reference entry.
    fn list_panel_for(&self, callee: &str, prefix: &str) -> Option<ReferencePanel> {
        if let Some(set) = self.string_choice_set(callee) {
            return Some(ReferencePanel::choice_vocabulary_for(
                &self.reference,
                set,
                prefix,
            ));
        }
        let panel = match self.string_role(callee)? {
            StringRole::Bank => {
                // Start with banks that support the sound pattern. The
                // full list remains available in explicit browse mode by
                // clearing the search and pressing Backspace once more.
                let sounds = self.bank_receiver_sounds_at_caret();
                let snapshot = self.worker.catalogue();
                let (machines, compatible_count) = bank_machines_ranked(&snapshot.sounds, &sounds);
                ReferencePanel::bank_vocabulary_for(
                    &self.reference,
                    machines,
                    sounds,
                    compatible_count,
                    prefix,
                )
            }
            // Sounds get the samples browser itself: variants, previews
            // and the library's families, already filtered to the word.
            StringRole::Sound => {
                let mut panel = ReferencePanel::browse(&self.reference);
                panel.tab = super::super::reference::Tab::Samples;
                // A completion inserts the chosen sound at the anchor.
                panel.intent = super::super::reference::PanelIntent::Insert;
                panel.sound_query = prefix.to_owned();
                panel
            }
            StringRole::Scale => {
                let mut panel = ReferencePanel::browse(&self.reference);
                panel.tab = super::super::reference::Tab::Scales;
                panel.intent = super::super::reference::PanelIntent::Insert;
                self.open_scale_in(panel, prefix)
            }
            // A chord asks for the chord browser: the qualities music
            // uses most, each opening onto its twelve roots, and every one
            // of them playable.
            StringRole::Chord => {
                let mut panel = ReferencePanel::browse(&self.reference);
                panel.tab = super::super::reference::Tab::Chords;
                panel.intent = super::super::reference::PanelIntent::Insert;
                panel.chord_query = prefix.to_owned();
                panel
            }
            // A colour name is not a function: `.color("cy` asks for cyan,
            // and the list shows each name in the colour it names.
            StringRole::Colour => ReferencePanel::color_vocabulary_for(&self.reference, prefix),
            // The divisions worth meeting by name. `edo` will take any of
            // them up to 65,536, so this list offers rather than restricts.
            StringRole::Edo => ReferencePanel::vocabulary_for(
                &self.reference,
                "equal divisions",
                super::super::reference::edo_vocabulary(),
                prefix,
            ),
            // The named tunings anyone comes looking for. The dictionary
            // holds three thousand more, and they are typed in full.
            StringRole::Tuning => ReferencePanel::vocabulary_for(
                &self.reference,
                "tunings",
                super::super::reference::tuning_vocabulary(),
                prefix,
            ),
            // The ports the dock is already listing. A port name has to be
            // typed exactly and cannot be guessed, and getting it wrong
            // fails silently, so this is the list that saves the most.
            StringRole::MidiOut => ReferencePanel::vocabulary_for(
                &self.reference,
                "MIDI outputs",
                self.midi_port_names(MidiDirection::Out),
                prefix,
            ),
            StringRole::MidiIn => ReferencePanel::vocabulary_for(
                &self.reference,
                "MIDI inputs",
                self.midi_port_names(MidiDirection::In),
                prefix,
            ),
            // The plugin host has these 2 lists. The host starts here when
            // this is its first use: the caret is in a plugin call.
            #[cfg(feature = "vst")]
            StringRole::Plugin => {
                let words = super::super::reference::PluginWords::Names {
                    instrument: callee == "vsti",
                };
                self.plugin_words_panel(words, prefix)
            }
            #[cfg(feature = "vst")]
            StringRole::Preset => {
                let (plugin, _) = self.plugin_call_at_caret()?;
                let words = super::super::reference::PluginWords::Presets(plugin);
                self.plugin_words_panel(words, prefix)
            }
        };
        Some(panel)
    }

    /// Open the scales list already standing on the scale the string names:
    /// `scale("Gb1:minor")` opens with every scale still listed, the
    /// minor group expanded, and `Gb:minor` the row under the cursor -
    /// the arrows reach a different scale, Enter writes the chosen one in
    /// place of the old name. The search stays empty so all alternatives
    /// remain visible; a word that is not a whole scale - `Gb1:min` -
    /// keeps the plain filter on the whole word.
    fn open_scale_in(&self, mut panel: ReferencePanel, prefix: &str) -> ReferencePanel {
        let Some((tonic, mode)) = prefix.rsplit_once(':') else {
            panel.scale_query = prefix.to_owned();
            return panel;
        };
        let Some(root) = super::super::reference::tonic_root(tonic) else {
            panel.scale_query = prefix.to_owned();
            return panel;
        };
        let Some((scale_index, name)) = panel
            .scale_names()
            .into_iter()
            .enumerate()
            .find(|(_, name)| name.eq_ignore_ascii_case(mode))
        else {
            panel.scale_query = prefix.to_owned();
            return panel;
        };
        panel.scale_query.clear();
        panel.open_scale = Some(name);
        if let Some(row) = panel.scale_rows().iter().position(|row| {
            matches!(
                row,
                super::super::reference::ScaleRow::Tonic(scale, tonic)
                    if *scale == scale_index && *tonic == root
            )
        }) {
            panel.scale_selected = row;
        }
        panel
    }

    /// The panel Ctrl+Space opens with the caret inside a string, when the
    /// call around the string names things a list can offer.
    fn string_completion_panel(&mut self) -> Option<ReferencePanel> {
        let (callee, prefix, range) = self.string_completion_at_caret()?;
        // The anchor covers the WHOLE token even though an unfinished
        // completion searches with only what is typed before the caret:
        // `testk|x` must find `testkick`, then replace `testkx` whole.
        // A complete scale is different: its tonic and mode select the
        // current row while leaving every scale visible, including when
        // the caret stands in the middle of the name.
        let word = self
            .editor()
            .document()
            .slice(range.clone())
            .unwrap_or_else(|_| prefix.clone());
        let query = if self.string_role(&callee) == Some(StringRole::Scale)
            || (prefix.is_empty() && !word.is_empty())
        {
            // A caret at the first character has no left prefix to search.
            // The token already in the score is still the question: using
            // it avoids opening a grouped sound catalogue on a heading.
            &word
        } else {
            &prefix
        };
        let panel = self.list_panel_for(&callee, query)?;
        self.reference_anchor = None;
        self.anchor_reference(&word, range);
        Some(panel)
    }

    /// Open the list behind the string the caret is in, on the word it is
    /// in, ranked nearest first. `false` when this string has no list.
    ///
    /// Shared by every key that means "what can go here?" - Ctrl+D, Ctrl+F
    /// and Tab all arrive at the same panel, because inside `s("…")` they
    /// are all asking the same question.
    pub(super) fn open_string_completion(&mut self) -> bool {
        let Some(mut panel) = self.string_completion_panel() else {
            return false;
        };
        let current_choice = panel
            .vocabulary
            .as_ref()
            .and(self.reference_anchor.as_ref())
            .map(|anchor| anchor.word.clone());
        {
            let catalogue = self.worker.catalogue();
            // Rank the applied bank's sounds first and insert them without
            // its prefix.
            let bank = self.banks_around_caret();
            panel.set_sounds_prefixed(
                with_input_channels(catalogue.sounds.clone(), self.input_channels()),
                bank,
            );
            panel.set_imports(browser_imports(
                &self.scenes.current().editor.source(),
                &catalogue,
            ));
        }
        self.catalogue_refreshed_at = Instant::now();
        panel.snippets_last = !self.caret_on_blank_line();
        panel.refresh(&self.reference);
        let current_choice_selected = current_choice
            .as_deref()
            .is_some_and(|current| panel.show_all_vocabulary_at(&self.reference, current));
        self.set_reference_panel(Some(panel));
        // A MIDI port is one name with spaces in it, so a choice replaces
        // the whole string rather than a word of it. Saying "the word"
        // there would be a promise about the edit that is not kept.
        let whole = self
            .string_completion_at_caret()
            .and_then(|(callee, _, _)| self.string_role(&callee))
            .is_some_and(StringRole::whole_string);
        let takes = if whole { "the name" } else { "the word" };
        self.status = match self.string_completion_panel_kind() {
            Some("the sample banks") => format!("the sample banks - Enter replaces {takes}"),
            Some("the documented choices") if current_choice_selected => {
                format!("the documented choices - current selected; Enter replaces {takes}")
            }
            Some("the documented choices") => {
                format!("the documented choices - Enter replaces {takes}")
            }
            Some(kind) => format!("{kind} - closest first; Enter replaces {takes}"),
            None => format!("closest first; Enter replaces {takes}"),
        };
        self.focus_panel(PanelKind::Reference);
        self.invalidate_maps();
        self.dirty_frame = true;
        true
    }

    /// Where an anchor sits now, after whatever has been edited since.
    ///
    /// An anchor can be empty: `bank("|` with nothing typed yet is a place,
    /// not a word. [`Editor::map_range_since`] answers `None` for an empty
    /// range, because its other caller maps highlights and an empty
    /// highlight is nothing to draw. An empty anchor is therefore followed
    /// as an offset, so the chosen name lands at the anchor even after the
    /// caret has moved.
    pub(super) fn anchor_range_now(&self, anchor: &ReferenceAnchor) -> Option<Range<usize>> {
        let editor = self.editor();
        if anchor.range.start >= anchor.range.end {
            let at = editor.map_offset_since(anchor.revision, anchor.range.start.0)?;
            return Some(at..at);
        }
        editor.map_range_since(anchor.revision, anchor.range.start.0..anchor.range.end.0)
    }

    /// Remember the word the column opened on, so a name chosen in it
    /// replaces that word.
    pub(super) fn anchor_reference(&mut self, word: &str, range: Range<ByteOffset>) {
        self.anchor_reference_as(word, range, Landing::Bare);
    }

    pub(super) fn anchor_reference_as(
        &mut self,
        word: &str,
        range: Range<ByteOffset>,
        landing: Landing,
    ) {
        self.reference_anchor = Some(ReferenceAnchor {
            scene: self.scenes.current().id,
            revision: self.editor().revision(),
            word: word.to_owned(),
            range,
            landing,
        });
    }

    /// Where a new call would go: the caret stands at the head of a link
    /// in a chain - after the `.` that opens it, with at most a half-typed
    /// name between - so what is chosen there is a function taking its
    /// place in the chain. Answers the range of the name typed so far,
    /// which is what the chosen one replaces.
    pub(super) fn chain_position_at_caret(&self) -> Option<Range<ByteOffset>> {
        // Inside a string the names on offer are sounds and chords, not
        // functions, and that list answers for itself.
        if self.string_completion_at_caret().is_some() {
            return None;
        }
        let editor = self.editor();
        let caret = editor.primary_selection().head;
        let document = editor.document();
        let line = document.line_of(caret).ok()?;
        let start = document.line_start(line);
        let text = document.slice(start..caret).ok()?;
        let bytes = text.as_bytes();
        // What has been typed of the name so far.
        let from = scan::name_ending_at(&text, text.len()).start;
        // A link opens with the `.` that introduces it.
        if from == 0 || bytes[from - 1] != b'.' {
            // The end of one link is the head of the next: a chain resting
            // at `)` takes another call after it, and that call brings its
            // own `.` with it.
            return matches!(bytes.last().copied(), Some(b')') | Some(b']')).then(|| caret..caret);
        }
        // `.5` is a number, not a link with a name half typed.
        let typed = &bytes[from..];
        if !typed.is_empty() && typed.iter().all(u8::is_ascii_digit) {
            return None;
        }
        // And that `.` hangs off something a method can be called on: a
        // call, an index, a name, a mini-notation string - across the line
        // breaks a chain is usually written over. A digit before it makes
        // it a decimal point instead.
        let source = editor.source();
        let before = scan::space_before(&source, start.0 + from - 1);
        let hangs_off = source.as_bytes().get(before.checked_sub(1)?).copied()?;
        if !(hangs_off.is_ascii_alphabetic()
            || matches!(hangs_off, b')' | b']' | b'_' | b'$' | b'"' | b'\''))
        {
            return None;
        }
        // A name that goes on past the caret means the caret is parked
        // inside one rather than at the end of one being written.
        // `.lp|env(4)` asks about `lpenv`. Only a name this engine does not
        // know, standing in front of one it does, reads as a new link
        // being written ahead of an old one: `.voicing().gli|s("z_tan")` is
        // `glide` going in before `s`. With nothing typed yet the caret
        // stands at the head of the next name, and a link goes in front of
        // it.
        let to = scan::name_starting_at(&source, caret.0).end;
        if !typed.is_empty() && to > caret.0 {
            let whole = source.get(start.0 + from..to)?;
            let tail = source.get(caret.0..to)?;
            let scope = self.reference_scope_at(caret);
            if self.reference.lookup_in(whole, scope).is_some()
                || self.reference.lookup_in(tail, scope).is_none()
            {
                return None;
            }
        }
        let range = reference_name_range(&source, start.0 + from..caret.0);
        Some(ByteOffset(range.start)..caret)
    }

    /// The completion list for a new link in a chain, anchored to the
    /// half-typed name so the chosen function takes its place - and goes
    /// in as the call it is.
    pub(super) fn chain_completion_panel(&mut self) -> Option<ReferencePanel> {
        let range = self.chain_position_at_caret()?;
        let typed = self.editor().document().slice(range.clone()).ok()?;
        let panel =
            ReferencePanel::browse_in(&self.reference, &typed, self.chain_reference_scope(&range));
        self.reference_anchor = None;
        self.anchor_reference_as(&typed, range, Landing::Call);
        Some(panel)
    }

    /// Argument choices for an explicit browser shortcut at an empty slot.
    pub(super) fn open_call_completion(&mut self) -> bool {
        let Some(mut panel) = self.call_completion_panel() else {
            return false;
        };
        {
            let catalogue = self.worker.catalogue();
            panel.set_sounds_prefixed(
                with_input_channels(catalogue.sounds.clone(), self.input_channels()),
                self.banks_around_caret(),
            );
            panel.set_imports(browser_imports(
                &self.scenes.current().editor.source(),
                &catalogue,
            ));
        }
        self.catalogue_refreshed_at = Instant::now();
        self.set_reference_panel(Some(panel));
        self.status = "completion - Enter puts the chosen name in place".into();
        self.focus_panel(PanelKind::Reference);
        #[cfg(feature = "hydra")]
        self.sync_settings_webcam_preview();
        self.invalidate_maps();
        self.dirty_frame = true;
        true
    }

    /// Ctrl+Space: the searchable list, or close it.
    pub(super) fn toggle_reference_browse(&mut self) {
        // The first frame of the Samples tab must describe the library that
        // is still arriving, rather than briefly claiming there are no
        // sounds before the half-second readiness poll catches up.
        self.library_loading = self
            .worker
            .library()
            .is_none_or(|library| library.manifests_pending() > 0);
        // In the object of a plugin call the question is a parameter key.
        #[cfg(feature = "vst")]
        if self.open_plugin_key_completion() {
            return;
        }
        // Inside a call that takes one of our lists but with no string
        // opened yet - `chord(|)` - the question is the same one: the
        // list, and the name goes in quoted.
        if self.open_call_completion() {
            return;
        }
        // Inside a string the chord always means "complete this" - with the
        // panel open on something else, it re-targets rather than closing.
        if self.string_completion_at_caret().is_some()
            && let Some(mut panel) = self.string_completion_panel()
        {
            {
                let catalogue = self.worker.catalogue();
                panel.set_sounds_prefixed(
                    with_input_channels(catalogue.sounds.clone(), self.input_channels()),
                    self.banks_around_caret(),
                );
                panel.set_imports(browser_imports(
                    &self.scenes.current().editor.source(),
                    &catalogue,
                ));
            }
            self.catalogue_refreshed_at = Instant::now();
            self.set_reference_panel(Some(panel));
            self.status = "completion - Enter puts the chosen name in place".into();
            self.focus_panel(PanelKind::Reference);
            #[cfg(feature = "hydra")]
            self.sync_settings_webcam_preview();
            self.invalidate_maps();
            self.dirty_frame = true;
            return;
        }
        #[cfg(feature = "vst")]
        if self.open_plugin_parameters() {
            return;
        }
        // A string nothing can complete - a name the score invents - is
        // not a function to look up either: searching 466 names for
        // `myfunction` answers a question nobody asked.
        if self.reference_panel.is_none()
            && let Some(refusal) = self.string_completion_refusal()
        {
            self.status = refusal;
            self.dirty_frame = true;
            return;
        }
        // Open on one word and the caret on another: the chord re-targets,
        // the way it does inside a string. Only the word it is already
        // showing closes it - a second press on the same name means "put
        // it away", a first press on a new one means "this one now". This
        // is decided before the focus rule below: a panel left open behind
        // the editor must answer about the new word on the first press,
        // not spend one taking the keyboard back.
        let asked_word = self.word_range_at_caret();
        let asked = asked_word.as_ref().map(|(word, _)| word.clone());
        let showing = self
            .reference_panel
            .as_ref()
            .and_then(|panel| panel.showing(&self.reference));
        // After inserting a call its documentation stays beside the editor.
        // Moving past `)` or `.` asks for the next link, even with no name
        // typed yet; do not focus the previous call's documentation again.
        let new_link = self.focus == Focus::Editor
            && asked.is_none()
            && self.chain_position_at_caret().is_some();
        // Equal names can describe different graph and pattern entries.
        let other_scope = self
            .reference_panel
            .as_ref()
            .is_some_and(|panel| match panel.mode {
                ReferenceMode::Entry { index, .. } => asked_word
                    .as_ref()
                    .and_then(|(word, range)| {
                        self.reference
                            .lookup_in(word, self.reference_scope_at(range.start))
                    })
                    .is_some_and(|asked| asked != index),
                ReferenceMode::Browse => {
                    panel.scope != ReferenceScope::All
                        && panel.scope != self.completion_scope_at_caret()
                }
            });
        let retarget = self.reference_panel.is_some()
            && ((asked.is_some() && asked != showing) || new_link || other_scope);
        // Open but not in front of you, and nothing new to ask: the chord
        // brings you back to it. Pressing it again from there puts it away.
        if self.reference_panel.is_some()
            && !retarget
            && self.focus != Focus::Panel(PanelKind::Reference)
        {
            self.focus_panel(PanelKind::Reference);
            return;
        }
        if retarget {
            self.set_reference_panel(None);
        }
        let panel = match self.reference_panel {
            Some(_) => {
                self.stop_preview();
                None
            }
            None => {
                // Inside a string, the question is never a function: offer
                // the names valid there - sounds, scales, chords - filtered
                // to what is already typed. Otherwise, opened on a word the
                // list starts searched for it - a misspelt name shows its
                // nearest real ones, and Enter puts the chosen one in its
                // place.
                let mut panel = if let Some(panel) = self.string_completion_panel() {
                    panel
                } else if let Some(panel) = self.chain_completion_panel() {
                    // At the head of a link - `.voicing().|s(…)` - the
                    // question is which function goes there, and it goes in
                    // as a call.
                    panel
                } else {
                    let scope = self.completion_scope_at_caret();
                    let namespace = self.reference_scope_at_caret();
                    let anchor = self
                        .word_range_at_caret()
                        .filter(|(_, range)| self.reference_scope_at(range.start) == namespace);
                    // With the caret inside a call the reference knows, the
                    // chord opens the entry for that call. A word under the
                    // caret asks for a completion instead: after `x=>x.pl`
                    // the request is for `pl...`, not for the call around
                    // it. Only a caret with no word under it asks about the
                    // call.
                    let caret = self.editor().primary_selection().head;
                    let typing = anchor
                        .as_ref()
                        .is_some_and(|(_, range)| range.end >= caret && range.start < caret);
                    let enclosing = self.enclosing_call_at_caret();
                    let entry = if typing {
                        None
                    } else {
                        enclosing.as_ref().and_then(|(word, range)| {
                            let callee_scope = self.reference_name_scope_at(range.start);
                            (self.reference_scope_at(range.start) == namespace)
                                .then(|| self.reference.lookup_in(word, callee_scope))
                                .flatten()
                        })
                    };
                    let panel = match (entry, &anchor) {
                        (Some(index), _) => self.reference_entry_panel(index, scope, None),
                        (None, Some((word, _))) => {
                            ReferencePanel::browse_in(&self.reference, word, scope)
                        }
                        (None, None) if enclosing.is_some() => {
                            ReferencePanel::browse_in(&self.reference, "", scope)
                        }
                        (None, None) => ReferencePanel::browse(&self.reference),
                    };
                    self.reference_anchor = None;
                    if let Some((word, range)) = anchor {
                        self.anchor_reference(&word, range);
                    }
                    panel
                };
                // Tab inside the column reaches the samples browser, which
                // reads the library the moment the column opens.
                {
                    let catalogue = self.worker.catalogue();
                    panel.set_sounds_prefixed(
                        with_input_channels(catalogue.sounds.clone(), self.input_channels()),
                        self.banks_around_caret(),
                    );
                    panel.set_imports(browser_imports(
                        &self.scenes.current().editor.source(),
                        &catalogue,
                    ));
                }
                self.catalogue_refreshed_at = Instant::now();
                Some(panel)
            }
        };
        self.set_reference_panel(panel);
        self.status = match (&self.reference_panel, &self.reference_anchor) {
            (Some(_), Some(anchor)) if anchor.landing == Landing::Call => {
                "completion - Enter adds the call to the chain".into()
            }
            (Some(_), Some(anchor)) if anchor.word.is_empty() => {
                "completion - Enter puts the chosen name at the caret".into()
            }
            (Some(_), Some(anchor)) => {
                format!(
                    "reference - closest to {:?}; Enter replaces it",
                    anchor.word
                )
            }
            (Some(_), None) => format!(
                "reference - {} entries; type to search",
                self.reference.len() - self.reference.hidden_len()
            ),
            (None, _) => "reference closed".into(),
        };
        if self.reference_panel.is_some() {
            self.focus_panel(PanelKind::Reference);
        } else {
            self.focus = Focus::Editor;
        }
        #[cfg(feature = "hydra")]
        self.sync_settings_webcam_preview();
        self.invalidate_maps();
        self.dirty_frame = true;
    }

    /// Whether the caret stands on a line with nothing on it.
    ///
    /// A snippet is a whole statement, so this is what decides whether one
    /// could be pasted where you are: on a blank line a snippet is exactly
    /// what you want, and anywhere else it is the one thing that cannot go
    /// there.
    pub(super) fn caret_on_blank_line(&self) -> bool {
        let editor = self.editor();
        let source = editor.source();
        let caret = editor.primary_selection().head.0.min(source.len());
        let start = source[..caret].rfind('\n').map_or(0, |at| at + 1);
        let end = source[caret..]
            .find('\n')
            .map_or(source.len(), |at| caret + at);
        source[start..end].trim().is_empty()
    }
}

/// `S` inside a graph changes back to pattern expressions.
/// The nearest graph boundary wins, including a new `K` nested inside `S`.
fn reference_scope(source: &str, caret: usize) -> ReferenceScope {
    let Some(prefix) = source.get(..caret) else {
        return ReferenceScope::Pattern;
    };
    let code = rustel_runtime::lint::code_only(prefix);
    let mut pattern_input = false;
    for open in scan::open_parens(&code, code.len()) {
        let Some(name) = scan::name_before(&code, open) else {
            continue;
        };
        match &code[name] {
            "K" if call_argument_index(&code, open) == 0 => {
                return if pattern_input {
                    ReferenceScope::Pattern
                } else {
                    ReferenceScope::Kabelsalat
                };
            }
            "S" => pattern_input = true,
            _ => {}
        }
    }
    ReferenceScope::Pattern
}

/// Static graph helpers keep their receiver when searched or replaced.
/// Ordinary node methods remain unqualified names such as `sine`.
fn reference_name_range(source: &str, name: Range<usize>) -> Range<usize> {
    if name.start > 0 && source.as_bytes()[name.start - 1] == b'.' {
        let receiver = scan::name_ending_at(source, name.start - 1);
        if matches!(&source[receiver.clone()], "Node" | "nodeRegistry") {
            return receiver.start..name.end;
        }
    }
    name
}

fn enclosing_call_argument(source: &str, caret: usize) -> Option<(Range<usize>, usize)> {
    let code = rustel_runtime::lint::code_only(source.get(..caret)?);
    scan::open_parens(&code, code.len()).find_map(|open| {
        let name = reference_name_range(&code, scan::name_before(&code, open)?);
        Some((name, call_argument_index(&code, open)))
    })
}

fn call_argument_index(code: &str, open: usize) -> usize {
    let mut depth = 0usize;
    let mut argument = 0;
    for byte in code.as_bytes()[open + 1..].iter().copied() {
        match byte {
            b'(' | b'[' | b'{' => depth += 1,
            b')' | b']' | b'}' => depth = depth.saturating_sub(1),
            b',' if depth == 0 => argument += 1,
            _ => {}
        }
    }
    argument
}

/// The offset and byte of the `"` or `'` that `before_caret` leaves open.
/// The other quote inside a string and backslash-escaped quotes do not
/// count.
fn open_quote(before_caret: &[u8]) -> Option<(usize, u8)> {
    let mut open = None;
    for (index, byte) in before_caret.iter().enumerate() {
        if !matches!(*byte, b'"' | b'\'') || (index > 0 && before_caret[index - 1] == b'\\') {
            continue;
        }
        open = match open {
            Some((_, quote)) if quote == *byte => None,
            Some(open) => Some(open),
            None => Some((index, *byte)),
        };
    }
    open
}

/// The name the caret is in, or the one it is writing.
///
/// The caret is rarely inside the name you want documented: with
/// `.luma(0.4|)` it sits on a bracket, and probing only where it stands
/// answers nothing. So it walks left - but only within the call it is in.
///
/// A closing bracket ends the search. Past the `)` of `.luma()` you are no
/// longer in `luma`; you are between calls, about to write the next one, and
/// answering `luma` there is answering about something you have finished with.
///
/// Numbers are skipped: `.luma(0.4|)` is asking about `luma`, not about `4`.
/// And the caret must be inside a name or just past it - never at its start,
/// where the name is what you are about to write rather than what you are on.
pub(super) fn word_before(
    document: &super::super::editor::Document,
    head: ByteOffset,
) -> Option<(String, Range<ByteOffset>)> {
    let floor = document
        .line_of(head)
        .map_or(ByteOffset(0), |line| document.line_start(line));
    let mut offset = head;
    loop {
        if let Ok(range) = document.word_range(offset)
            && range.start < range.end
            && range.end > floor
            // The caret must be INSIDE the name or just past it, never at its
            // start: sitting before `orbit` you are about to write something
            // else, and answering about `orbit` answers about what comes next
            // rather than what you are on.
            && range.start < head
            && let Ok(word) = document.slice(range.clone())
        {
            let word = word.trim();
            if !word.is_empty()
                && word
                    .chars()
                    .all(|character| character.is_alphanumeric() || character == '_')
                // A number is an argument, not a name to look up.
                && !word.chars().all(|character| character.is_ascii_digit())
            {
                return Some((word.to_owned(), range));
            }
        }
        if offset <= floor {
            return None;
        }
        let previous = match document.previous_grapheme_boundary(offset) {
            Ok(previous) if previous < offset => previous,
            _ => return None,
        };
        // Stepping over a closing bracket leaves the call behind.
        if document
            .slice(previous..offset)
            .is_ok_and(|text| matches!(text.trim(), ")" | "]" | "}"))
        {
            return None;
        }
        offset = previous;
    }
}

#[cfg(test)]
mod argument_context_tests {
    use super::{ReferenceScope, enclosing_call_argument, reference_scope};

    #[test]
    fn graph_and_pattern_boundaries_follow_the_caret_context() {
        for (source, scope) in [
            ("sine(", ReferenceScope::Pattern),
            ("note('c3').K(() => sine(", ReferenceScope::Kabelsalat),
            (
                "note('c3').worklet (() => {\n sine(",
                ReferenceScope::Pattern,
            ),
            ("K(() => S(note('c3').adsr(", ReferenceScope::Pattern),
            ("K(() => m(note('c3').adsr(", ReferenceScope::Kabelsalat),
            ("K(() => node.m(sine(", ReferenceScope::Kabelsalat),
            ("K(() => m('unfinished", ReferenceScope::Kabelsalat),
            ("K(() => sine(220).out(), sine(", ReferenceScope::Pattern),
            (
                "worklet('sine(pat[0]).out()', sine(",
                ReferenceScope::Pattern,
            ),
            (
                "K(() => { let a = 2, b = 3; sine(",
                ReferenceScope::Kabelsalat,
            ),
            ("K(() => sine(S(note('c3')), ", ReferenceScope::Kabelsalat),
            (
                "K(() => S(note('c3').K(() => sine(",
                ReferenceScope::Kabelsalat,
            ),
            ("S(note('c3').K(() => sine(", ReferenceScope::Kabelsalat),
            ("K(() => sine(220)).adsr(", ReferenceScope::Pattern),
            ("// K(() =>\nsine(", ReferenceScope::Pattern),
            ("'K(() => S('; sine(", ReferenceScope::Pattern),
            ("const pattern = /K\\(/; sine(", ReferenceScope::Pattern),
            ("K(() => { /* S( */ sine(", ReferenceScope::Kabelsalat),
            ("K(() => { // S(\n sine(", ReferenceScope::Kabelsalat),
            (
                "K(() => poly({name: 'S(', voice: sine(",
                ReferenceScope::Kabelsalat,
            ),
            ("K(() => { /* S(", ReferenceScope::Kabelsalat),
            ("K(() => S('unfinished K(", ReferenceScope::Pattern),
        ] {
            assert_eq!(reference_scope(source, source.len()), scope, "{source}");
        }
        // The opening parenthesis switches the arguments, never the
        // callee whose name appears before it.
        let source = "note('c3').K(() => S(note('c4')))";
        assert_eq!(
            reference_scope(source, source.find("K(").unwrap()),
            ReferenceScope::Pattern
        );
        assert_eq!(
            reference_scope(source, source.find("S(").unwrap()),
            ReferenceScope::Kabelsalat
        );
        assert_eq!(
            reference_scope(source, source.find("S(").unwrap() + 2),
            ReferenceScope::Pattern
        );
    }

    #[test]
    fn counts_only_the_enclosing_calls_argument_separators() {
        for (source, callee, argument) in [
            (r#"s("bd").inspire("C:major:pentatonic", "#, "inspire", 1),
            (r#"inspire("C:major", "density,"#, "inspire", 1),
            (r#"inspire(["C:major", "D:minor"], "#, "inspire", 1),
            (r#"inspire({ a: 1, b: [2, 3] }, "#, "inspire", 1),
            (r#"inspire(choose("a,b", "c"), "#, "inspire", 1),
            (r#"inspire(("a", "b"), "#, "inspire", 1),
            (r#"inspire(/[,)]/, /* , ) */ "#, "inspire", 1),
            ("inspire(`a,b`, // , )\n", "inspire", 1),
            ("inspire(\"C:major\", (", "inspire", 1),
            ("outer(1, inner([2, 3], ", "inner", 1),
            ("outer(1, inner(2, 3), ", "outer", 2),
            ("lfo({ control: 'gain', shape: '", "lfo", 0),
            ("lfo({ shape: 'sine' }, '", "lfo", 1),
            ("K(() => Node.parseInput (", "Node.parseInput", 0),
            ("K(() => nodeRegistry.get(", "nodeRegistry.get", 0),
            ("K(() => input.sine(", "sine", 0),
        ] {
            let (name, actual) = enclosing_call_argument(source, source.len())
                .unwrap_or_else(|| panic!("missing call: {source}"));
            assert_eq!(&source[name], callee, "{source}");
            assert_eq!(actual, argument, "{source}");
        }
        assert!(enclosing_call_argument("s(\"bd\")", 7).is_none());
    }
}

#[cfg(test)]
mod scoped_completion_tests {
    use super::*;
    use crate::editor::{Editor, Selection};

    fn seed(app: &mut App, marked: &str) {
        let caret = marked.find('|').expect("caret marker");
        app.scenes.current_mut().editor = Editor::new(&marked.replace('|', "")).expect("editor");
        app.editor_mut()
            .set_selection(Selection::caret(ByteOffset(caret)))
            .expect("caret");
        app.set_reference_panel(None);
        app.reference_anchor = None;
        app.focus = Focus::Editor;
    }

    #[test]
    fn completion_switches_namespace_at_graph_and_pattern_inputs() {
        let directory = tempfile::tempdir().expect("temp dir");
        let mut app = crate::app::tests::support::app_over(directory.path());
        for (marked, scope, query) in [
            ("K(|", ReferenceScope::Kabelsalat, ""),
            ("K(() => S(|", ReferenceScope::Pattern, ""),
            ("K(() => si|)", ReferenceScope::Kabelsalat, "si"),
            ("K(() => sFreq.si|)", ReferenceScope::KabelsalatMember, "si"),
            ("K(() => sine(220)|)", ReferenceScope::KabelsalatMember, ""),
            ("K(() => Node.|)", ReferenceScope::Kabelsalat, "Node."),
            (
                "K(() => Node.parseI|)",
                ReferenceScope::Kabelsalat,
                "Node.parseI",
            ),
            ("K(() => S(note('c3').ad|))", ReferenceScope::Pattern, "ad"),
        ] {
            seed(&mut app, marked);
            app.toggle_reference_browse();
            let panel = app.reference_panel.as_ref().expect("completion");
            assert_eq!(panel.scope, scope, "{marked}");
            assert_eq!(panel.query, query, "{marked}");
            assert_eq!(
                app.reference_anchor
                    .as_ref()
                    .map(|anchor| anchor.word.as_str()),
                (!query.is_empty() || scope == ReferenceScope::KabelsalatMember).then_some(query),
                "{marked}"
            );
        }
    }

    #[test]
    fn graph_completion_separates_global_inputs_and_node_members() {
        let directory = tempfile::tempdir().expect("temp dir");
        let mut app = crate::app::tests::support::app_over(directory.path());
        for (marked, allowed, excluded) in [
            ("K(() => sFre|)", "sFreq", "apply2"),
            ("K(() => sFreq.|)", "apply2", "sFreq"),
            ("K(() => app|)", "sFreq", "apply2"),
            ("K(() => sFreq.sFre|)", "apply2", "sFreq"),
            ("K(() => Node.parseI|)", "Node.parseInput", "apply2"),
        ] {
            seed(&mut app, marked);
            app.toggle_reference_browse();
            let panel = app.reference_panel.as_ref().expect("completion");
            let excluded = app.reference.lookup_in(excluded, panel.scope).unwrap();
            assert!(!panel.results.contains(&excluded), "{marked}");
            // Emptying the query keeps its namespace, including after a
            // partially typed global name on a node.
            let mut panel = panel.clone();
            panel.query.clear();
            panel.refresh(&app.reference);
            let allowed = app.reference.lookup_in(allowed, panel.scope).unwrap();
            assert!(panel.results.contains(&allowed), "{marked}");
            assert!(!panel.results.contains(&excluded), "{marked}");
        }
    }

    #[test]
    fn help_and_inserted_calls_keep_their_namespace_when_returning_to_the_list() {
        let directory = tempfile::tempdir().expect("temp dir");
        let mut app = crate::app::tests::support::app_over(directory.path());
        for (marked, scope) in [
            ("sine|.range(0, 1)", ReferenceScope::Pattern),
            ("K(() => sine|(220))", ReferenceScope::Kabelsalat),
            (
                "K(() => sFreq.sine|(220))",
                ReferenceScope::KabelsalatMember,
            ),
        ] {
            seed(&mut app, marked);
            app.toggle_reference_at_caret();
            let panel = app.reference_panel.as_ref().expect("documentation");
            assert_eq!(panel.scope, scope, "{marked}");
            let mut panel = panel.clone();
            assert!(panel.collapse(), "{marked}");
            assert_eq!(
                panel.selected_result(),
                app.reference.lookup_in("sine", scope),
                "{marked}"
            );
        }

        for (marked, scope, query) in [
            ("K(() => sin|)", ReferenceScope::Kabelsalat, "sin"),
            (
                "K(() => sFreq.sin|)",
                ReferenceScope::KabelsalatMember,
                "sin",
            ),
            (
                "K(() => sin|)",
                ReferenceScope::Kabelsalat,
                "tag:kabelsalat sine",
            ),
        ] {
            seed(&mut app, marked);
            app.toggle_reference_browse();
            let mut panel = app.reference_panel.take().unwrap();
            panel.query = query.to_owned();
            panel.refresh(&app.reference);
            let graph_sine = app.reference.lookup_in("sine", scope).unwrap();
            panel.selected = panel.row_of_result(
                panel
                    .results
                    .iter()
                    .position(|&index| index == graph_sine)
                    .unwrap(),
            );
            app.reference_panel = Some(panel);
            app.confirm_reference_name("sine".to_owned()).unwrap();
            let panel = app.reference_panel.as_ref().expect("inserted call help");
            assert_eq!(panel.scope, scope, "{marked}");
            assert_eq!(
                panel.query,
                if query.starts_with("tag:") {
                    query
                } else {
                    "sine"
                }
            );
            assert_eq!(panel.selected_result(), Some(graph_sine));
        }
    }

    #[test]
    fn same_spelling_retargets_completion_between_graph_and_pattern() {
        let directory = tempfile::tempdir().expect("temp dir");
        let mut app = crate::app::tests::support::app_over(directory.path());
        seed(
            &mut app,
            "sine|.range(0, 1)\nnote('c3').K(() => sine(220).out())",
        );
        let source = app.editor().source();
        let pattern = source.find("sine").unwrap() + 4;
        let graph = source.rfind("sine").unwrap() + 4;
        for (before, after, scope) in [
            (pattern, graph, ReferenceScope::Kabelsalat),
            (graph, pattern, ReferenceScope::Pattern),
        ] {
            app.focus = Focus::Editor;
            app.editor_mut()
                .set_selection(Selection::caret(ByteOffset(before)))
                .unwrap();
            app.toggle_reference_at_caret();
            app.focus = Focus::Editor;
            app.editor_mut()
                .set_selection(Selection::caret(ByteOffset(after)))
                .unwrap();
            app.toggle_reference_browse();
            let panel = app.reference_panel.as_ref().expect("completion");
            assert_eq!(panel.scope, scope);
            assert_eq!(panel.query, "sine");
            assert_eq!(
                panel.selected_result(),
                app.reference.lookup_in("sine", scope)
            );
        }
    }
}

#[cfg(test)]
mod tests {
    mod completion {
        //! Tests for `app/completion.rs`.

        use crate::app::string_roles::StringRole;
        use crate::app::tests::support::*;
        use crate::app::*;
        use crate::editor::{MemoryClipboard, Selection};

        /// A snippet is a whole statement. Mid-expression it ranks below a
        /// function whose name matches; on a blank line it ranks above.
        #[test]
        fn snippets_sink_when_the_caret_is_not_on_a_blank_line() {
            let directory = tempfile::tempdir().expect("temp dir");
            let (mut app, _scene) = plain_app(directory.path(), "$: s(\"bd\").lpf(800)\n\n");
            let reference = &app.reference;
            let snippet_at = |results: &[usize]| {
                results.iter().position(|index| {
                    reference
                        .entry(*index)
                        .is_some_and(|entry| entry.tags.iter().any(|tag| tag == "snippet"))
                })
            };
            let named_at = |results: &[usize], name: &str| {
                results.iter().position(|index| {
                    reference
                        .entry(*index)
                        .is_some_and(|entry| entry.name == name)
                })
            };

            let on_line = reference.search_with("midike", true);
            let on_blank = reference.search_with("midike", false);
            assert!(
                !on_line.is_empty() && !on_blank.is_empty(),
                "the query finds things"
            );

            let keys_on_line =
                named_at(&on_line, "midikeys").expect("midikeys is found either way");
            let snippet_on_line =
                snippet_at(&on_line).expect("snippets are still offered, not hidden");
            assert!(
                keys_on_line < snippet_on_line,
                "mid-expression, the function a name matches comes before a snippet \
         that cannot go there: {keys_on_line} vs {snippet_on_line}"
            );

            let snippet_on_blank = snippet_at(&on_blank).expect("a snippet");
            let keys_on_blank = named_at(&on_blank, "midikeys").expect("midikeys");
            assert!(
                snippet_on_blank < keys_on_blank,
                "on a blank line a snippet is what was wanted: {snippet_on_blank} vs {keys_on_blank}"
            );
            // Nothing is dropped either way - the order changed, not the answer.
            assert_eq!(on_line.len(), on_blank.len());

            // And the app reads the caret's own line to decide which it is.
            let source = app.editor().source();
            let mid = source.find("800").expect("a number mid-line");
            app.editor_mut()
                .set_selection(Selection::caret(ByteOffset(mid)))
                .expect("caret mid-line");
            assert!(!app.caret_on_blank_line());
            let blank = source.rfind("\n\n").expect("the blank line") + 1;
            app.editor_mut()
                .set_selection(Selection::caret(ByteOffset(blank)))
                .expect("caret on the blank line");
            assert!(app.caret_on_blank_line());
        }

        /// Ctrl+D inside a sound, chord or scale string opens that call's own
        /// list. It does not refuse.
        #[test]
        fn ctrl_d_inside_a_list_string_opens_that_list_on_the_word() {
            let directory = tempfile::tempdir().expect("temp dir");
            for (score, needle, what) in [
                ("$: s(\"siren\")\n", "siren", "sounds"),
                ("$: n(\"0\").chord(\"Cmaj7\")\n", "Cmaj7", "chords"),
                ("$: n(\"0\").scale(\"C:minor\")\n", "C:minor", "scales"),
            ] {
                let mut app = app_over(directory.path());
                // `app_over` writes its own scene; this is the one under test.
                app.scenes.current_mut().editor =
                    crate::editor::Editor::new(score).expect("editor");
                // Put the caret in the middle of the quoted name.
                let column = score.find(needle).expect("needle") + needle.len() / 2;
                for _ in 0..column {
                    press(&mut app, KeyCode::Right, KeyModifiers::NONE);
                }
                press(&mut app, KeyCode::Char('d'), KeyModifiers::CONTROL);
                let panel = app
                    .reference_panel
                    .as_ref()
                    .unwrap_or_else(|| panic!("{what}: Ctrl+D refused instead of answering"));
                assert!(
                    !panel.results.is_empty(),
                    "{what}: opened on nothing - {}",
                    app.status
                );
                assert!(
                    !app.status.contains("no reference"),
                    "{what}: still refusing - {}",
                    app.status
                );
            }
        }

        #[test]
        fn ctrl_d_shows_function_arguments_until_a_string_is_opened() {
            use crate::reference::{ReferenceMode, Tab};

            let directory = tempfile::tempdir().expect("temp dir");
            for (marked, name) in [
                ("$: s(|)", "s"),
                ("$: chord(|)", "chord"),
                ("$: n(0).scale(|)", "scale"),
                #[cfg(feature = "extensions")]
                ("$: n(0).inspire(|)", "inspire"),
                ("$: s(\"bd\").warpmode(|)", "warpmode"),
                #[cfg(feature = "extensions")]
                ("$: n(0).inspire(\"C:major:pentatonic\", |)", "inspire"),
                #[cfg(feature = "extensions")]
                ("$: n(0).inspire(\"C:major:pentatonic\", 4, |)", "inspire"),
            ] {
                let caret = marked.find('|').expect("caret marker");
                let source = marked.replace('|', "");
                let mut app = app_over(directory.path());
                app.scenes.current_mut().editor = Editor::new(&source).expect("editor");
                app.editor_mut()
                    .set_selection(Selection::caret(ByteOffset(caret)))
                    .expect("caret");

                press(&mut app, KeyCode::Char('d'), KeyModifiers::CONTROL);

                let panel = app.reference_panel.as_ref().expect("reference opened");
                assert_eq!(panel.tab, Tab::Reference, "{marked}");
                let ReferenceMode::Entry { index, .. } = panel.mode else {
                    panic!("{marked}: expected the function's argument documentation")
                };
                assert_eq!(app.reference.entry(index).unwrap().name, name, "{marked}");
                assert!(panel.vocabulary.is_none(), "{marked}");
                assert!(
                    app.reference_anchor.is_none(),
                    "{marked}: docs must not insert"
                );
                assert_eq!(app.editor().source(), source, "{marked}");
                assert_eq!(app.editor().primary_selection().head, ByteOffset(caret));
            }
        }

        #[test]
        fn ctrl_d_after_an_opening_quote_offers_the_first_arguments_values() {
            use crate::reference::Tab;

            let directory = tempfile::tempdir().expect("temp dir");
            for (marked, tab) in [
                ("$: s(\"|\")", Tab::Samples),
                ("$: chord(\"|\")", Tab::Chords),
                ("$: n(0).scale(\"|\")", Tab::Scales),
                #[cfg(feature = "extensions")]
                ("$: n(0).inspire(\"|\")", Tab::Scales),
                #[cfg(feature = "extensions")]
                ("$: n(0).inspire('|')", Tab::Scales),
                #[cfg(feature = "extensions")]
                ("$: n(0).inspire(\"|", Tab::Scales),
            ] {
                let caret = marked.find('|').expect("caret marker");
                let source = marked.replace('|', "");
                let mut app = app_over(directory.path());
                app.scenes.current_mut().editor = Editor::new(&source).expect("editor");
                app.editor_mut()
                    .set_selection(Selection::caret(ByteOffset(caret)))
                    .expect("caret");

                press(&mut app, KeyCode::Char('d'), KeyModifiers::CONTROL);

                let panel = app.reference_panel.as_ref().expect("value picker opened");
                assert_eq!(panel.tab, tab, "{marked}");
                assert!(
                    app.reference_anchor.is_some(),
                    "{marked}: values can be inserted"
                );
                assert_eq!(app.editor().source(), source, "{marked}");
            }
        }

        #[test]
        fn ctrl_d_in_later_string_arguments_does_not_reuse_the_first_arguments_values() {
            use crate::reference::{ReferenceMode, Tab};

            let directory = tempfile::tempdir().expect("temp dir");
            for (marked, name) in [
                #[cfg(feature = "extensions")]
                (r#"$: n(0).inspire("C:major:pentatonic", "|")"#, "inspire"),
                #[cfg(feature = "extensions")]
                (r#"$: n(0).inspire("C:major, D:minor", "|")"#, "inspire"),
                #[cfg(feature = "extensions")]
                (
                    r#"$: n(0).inspire(pick(["C:major", "D:minor"]), "|")"#,
                    "inspire",
                ),
                #[cfg(feature = "extensions")]
                (
                    r#"$: n(0).inspire({ scales: ["C:major", "D:minor"] }, "|")"#,
                    "inspire",
                ),
                #[cfg(feature = "extensions")]
                (r#"$: n(0).inspire(/[,)]/.source, "|")"#, "inspire"),
                #[cfg(feature = "extensions")]
                ("$: n(0).inspire(\"C:major\", /* , ( */\n \"|\")", "inspire"),
                (r#"$: s("bd").warpmode("binary", "|")"#, "warpmode"),
                (r#"register("my|function", value => value)"#, "register"),
            ] {
                let caret = marked.find('|').expect("caret marker");
                let source = marked.replace('|', "");
                let mut app = app_over(directory.path());
                app.scenes.current_mut().editor = Editor::new(&source).expect("editor");
                app.editor_mut()
                    .set_selection(Selection::caret(ByteOffset(caret)))
                    .expect("caret");

                press(&mut app, KeyCode::Char('d'), KeyModifiers::CONTROL);

                let panel = app.reference_panel.as_ref().expect("reference opened");
                assert_eq!(panel.tab, Tab::Reference, "{marked}");
                let ReferenceMode::Entry { index, .. } = panel.mode else {
                    panic!("{marked}: expected the function's argument documentation")
                };
                assert_eq!(app.reference.entry(index).unwrap().name, name, "{marked}");
                assert!(panel.vocabulary.is_none(), "{marked}");
                assert!(app.reference_anchor.is_none(), "{marked}");
                assert_eq!(app.editor().source(), source, "{marked}");
            }
        }

        #[test]
        fn ctrl_d_inside_a_finite_choice_lists_explains_and_replaces_it() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = app_over(directory.path());
            let score = "$: s(\"basique\").warpmode(\"binary\")\n";
            app.scenes.current_mut().editor = crate::editor::Editor::new(score).expect("editor");
            let at = score.find("binary").expect("choice") + "bin".len();
            app.editor_mut()
                .set_selection(Selection::caret(ByteOffset(at)))
                .expect("caret");

            press(&mut app, KeyCode::Char('d'), KeyModifiers::CONTROL);
            let panel = app.reference_panel.as_ref().expect("Ctrl+D answered");
            let vocabulary = panel.vocabulary.as_ref().expect("a choice list");
            assert_eq!(vocabulary.subject, "warpmode");
            assert_eq!(panel.query, "", "Ctrl+D shows every documented choice");
            assert_eq!(panel.results.len(), vocabulary.names.len());
            let spin = vocabulary
                .names
                .iter()
                .position(|name| name == "spin")
                .expect("spin is offered");
            assert!(
                vocabulary.details[spin].contains("Ripples phase"),
                "the picker explains the choice: {:?}",
                vocabulary.details
            );
            assert_eq!(
                app.status,
                "the documented choices - current selected; Enter replaces the word"
            );
            assert_eq!(
                panel
                    .selected_result()
                    .and_then(|index| vocabulary.names.get(index))
                    .map(String::as_str),
                Some("binary"),
                "the value already in the score is selected"
            );

            // Move to the next documented choice and prove Enter replaces only the
            // choice under the caret, through the ordinary anchored edit path.
            press(&mut app, KeyCode::Down, KeyModifiers::NONE);
            let selected = app
                .reference_panel
                .as_ref()
                .and_then(|panel| panel.selected_result())
                .and_then(|index| {
                    app.reference_panel
                        .as_ref()?
                        .vocabulary
                        .as_ref()?
                        .names
                        .get(index)
                })
                .cloned()
                .expect("selected choice");
            assert_ne!(selected, "s", "Down must select a replacement");
            press(&mut app, KeyCode::Enter, KeyModifiers::NONE);
            assert_ne!(app.editor().source(), score, "Enter must edit the score");
            assert!(
                app.editor()
                    .source()
                    .contains(&format!("warpmode(\"{selected}\")")),
                "Enter replaced the choice: {}",
                app.editor().source()
            );
        }

        #[test]
        fn ctrl_d_exposes_every_direct_finite_choice_with_the_current_value_selected() {
            let directory = tempfile::tempdir().expect("temp dir");
            for set in rustel_core::reference::REFERENCE_CHOICE_SETS
                .iter()
                .filter(|set| set.completes_string)
            {
                // A hyphenated name, where the list has one, is the case a
                // word scan most easily cuts short.
                let current = set
                    .choices
                    .iter()
                    .find(|choice| choice.value.contains('-'))
                    .unwrap_or(&set.choices[set.choices.len() / 2])
                    .value;
                let score = match set.entry {
                    "setDefaultJoin" | "useRNG" => {
                        format!(r#"{}("{current}")\n"#, set.entry)
                    }
                    "lfo" => format!(r#"$: s("sine").lfo({{ shape: "{current}" }})\n"#),
                    "talkit" => format!(
                        r#"$: talkit("hello", {{ {}: "{current}" }})\n"#,
                        set.parameter.trim_start_matches("config.")
                    ),
                    _ => format!(r#"$: s("sine").{}("{current}")\n"#, set.entry),
                };
                let mut app = app_over(directory.path());
                app.scenes.current_mut().editor =
                    crate::editor::Editor::new(&score).expect("editor");
                let at = score.find(current).expect("current choice") + current.len() / 2;
                app.editor_mut()
                    .set_selection(Selection::caret(ByteOffset(at)))
                    .expect("caret");

                press(&mut app, KeyCode::Char('d'), KeyModifiers::CONTROL);
                let panel = app.reference_panel.as_ref().unwrap_or_else(|| {
                    panic!("{}.{}: Ctrl+D did not open", set.entry, set.parameter)
                });
                let vocabulary = panel.vocabulary.as_ref().unwrap_or_else(|| {
                    panic!(
                        "{}.{}: Ctrl+D did not open choices",
                        set.entry, set.parameter
                    )
                });
                let expected = set
                    .choices
                    .iter()
                    .map(|choice| choice.value.to_owned())
                    .collect::<Vec<_>>();
                assert_eq!(
                    vocabulary.names, expected,
                    "{}.{}: incomplete or reordered choices",
                    set.entry, set.parameter
                );
                assert_eq!(
                    panel.query, "",
                    "{}.{}: the current choice became a search filter",
                    set.entry, set.parameter
                );
                assert_eq!(
                    panel
                        .selected_result()
                        .and_then(|index| vocabulary.names.get(index))
                        .map(String::as_str),
                    Some(current),
                    "{}.{}: the current choice was not selected",
                    set.entry,
                    set.parameter
                );
                assert!(
                    vocabulary
                        .details
                        .iter()
                        .all(|detail| !detail.trim().is_empty()),
                    "{}.{}: a choice has no explanation",
                    set.entry,
                    set.parameter
                );
            }
        }

        /// Ctrl+D inside a scale the score names opens the scales list on that
        /// scale: every scale stays listed, its group is open, and Enter writes
        /// the selected scale in place of the old name.
        #[test]
        fn ctrl_d_inside_a_written_scale_lands_on_that_scale() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = app_over(directory.path());
            let score = "$: n(\"0\").scale(\"Gb1:minor\")\n";
            app.scenes.current_mut().editor = crate::editor::Editor::new(score).expect("editor");
            // The caret stands on the `i` in `minor`, mid-word.
            let at = score.find("Gb1:minor").expect("the scale") + "Gb1:mi".len();
            app.editor_mut()
                .set_selection(Selection::caret(ByteOffset(at)))
                .expect("caret");
            press(&mut app, KeyCode::Char('d'), KeyModifiers::CONTROL);
            let panel = app.reference_panel.as_ref().expect("Ctrl+D answered");
            assert_eq!(panel.tab, crate::reference::Tab::Scales);
            assert_eq!(
                panel.scale_query, "",
                "a known scale leaves the full catalogue visible"
            );
            assert_eq!(
                panel.open_scale.as_deref(),
                Some("minor"),
                "the named scale's group is open"
            );
            assert_eq!(
                panel.selected_scale().as_deref(),
                Some("Gb:minor"),
                "the scale the score names is the row under the cursor"
            );
            assert_eq!(
                panel.scale_names().len(),
                crate::reference::scale_vocabulary().len(),
                "every scale stays listed around the open one"
            );

            // Enter on the selected scale writes it in place of the old name.
            press(&mut app, KeyCode::Enter, KeyModifiers::NONE);
            assert!(
                app.editor().source().contains("scale(\"Gb:minor\")"),
                "Enter replaced the old scale: {}",
                app.editor().source()
            );
        }

        #[test]
        fn no_call_is_found_in_empty_text_or_a_lone_bracket() {
            use crate::app::{ByteOffset, completion::word_before};
            for (source, offset) in [("", 0), ("osc()\n\n", 6), ("(", 1)] {
                let document = crate::editor::Document::new(source, 1 << 16).expect("document");
                assert_eq!(
                    word_before(&document, ByteOffset(offset)),
                    None,
                    "{source:?}"
                );
            }
        }

        /// Ctrl+D asks about the call the caret is in, and no other.
        #[test]
        fn the_caret_finds_the_name_of_the_call_it_is_inside() {
            use crate::app::{ByteOffset, completion::word_before};
            let source = "await initHydra()\nosc(10, 0.1).luma(0.4).repeatX(4)\n";
            let document = crate::editor::Document::new(source, 1 << 16).expect("document");
            let after =
                |needle: &str| ByteOffset(source.find(needle).expect(needle) + needle.len());
            let name = |at| word_before(&document, at).map(|(word, _)| word);

            // Inside the name, and just after it.
            assert_eq!(name(after("lum")).as_deref(), Some("luma"));
            assert_eq!(name(after(".luma")).as_deref(), Some("luma"));
            // But not at its start: sitting before `luma` you are about to write
            // something, and the name to your right is not what you are on.
            assert_eq!(name(after("0.1).")), None);
            // Inside its brackets, over an argument: still the call, not the
            // number, because `4` is not something to look up.
            assert_eq!(name(after(".luma(")).as_deref(), Some("luma"));
            assert_eq!(name(after(".luma(0.4")).as_deref(), Some("luma"));

            // Past the closing bracket the call is finished, and answering about
            // `luma` there answers about something already left behind.
            assert_eq!(name(after(".luma(0.4)")), None);
            assert_eq!(name(after("osc(10, 0.1)")), None);
            assert_eq!(name(after(".repeatX(4)")), None);
        }

        /// Ctrl+Space with the caret inside `setcpm(` documents setcpm - the
        /// argument being typed is the question being asked.
        #[test]
        fn the_reference_opens_on_the_call_the_caret_stands_inside() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = app_over(directory.path());
            app.editor_mut()
                .dispatch(
                    Command::PasteText("setcpm(".into()),
                    HistoryMoment(0),
                    &mut MemoryClipboard::default(),
                )
                .expect("seed");
            // Caret sits right after the bracket: no word under it, but the
            // enclosing call is setcpm.
            let (word, _) = app.enclosing_call_at_caret().expect("an enclosing call");
            assert_eq!(word, "setcpm");
            assert!(
                app.reference.lookup("setcpm").is_some(),
                "the reference knows setcpm"
            );
            app.toggle_reference_browse();
            let panel = app.reference_panel.as_ref().expect("reference open");
            assert!(
                matches!(panel.mode, crate::reference::ReferenceMode::Entry { .. }),
                "the entry page opens directly, parameters and all"
            );

            // Deeper in, mid-argument on a number, the call still answers.
            app.editor_mut()
                .dispatch(
                    Command::PasteText("140/".into()),
                    HistoryMoment(1),
                    &mut MemoryClipboard::default(),
                )
                .expect("type an argument");
            let (word, _) = app.enclosing_call_at_caret().expect("still inside");
            assert_eq!(word, "setcpm");
        }

        /// `enclosing_call_at_caret` ignores brackets in strings, comments and
        /// regexes, and permits whitespace between the name and its `(`.
        #[test]
        fn the_enclosing_call_is_read_past_strings_and_comments() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = app_over(directory.path());
            for (source, expected) in [
                ("$: stack(\")\", ", Some("stack")),
                ("$: f(x /* ) */, ", Some("f")),
                ("$: s(\"bd(3,8", Some("s")),
                ("$: note(\"c\").when(/(/, x => x.fast(", Some("fast")),
                ("$: note(\"c\").when(/(/, ", Some("when")),
                ("// setcpm(", None),
                ("$: setcpm (", Some("setcpm")),
            ] {
                app.scenes.current_mut().editor =
                    crate::editor::Editor::new(source).expect("editor");
                app.editor_mut()
                    .set_selection(Selection::caret(ByteOffset(source.len())))
                    .expect("caret");
                let call = app.enclosing_call_at_caret().map(|(word, range)| {
                    assert_eq!(source[range.start.0..range.end.0], word, "{source}");
                    word
                });
                assert_eq!(call.as_deref(), expected, "{source}");
            }
        }

        /// Ctrl+Space with a word under the fingers completes THAT word, even
        /// inside a call: after `sometimesBy(0.6, x=>x.pl` the question is
        /// `pl…`, not sometimesBy's own page.
        #[test]
        fn a_word_being_typed_beats_the_enclosing_call() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = app_over(directory.path());
            app.editor_mut()
                .dispatch(
                    Command::PasteText("sometimesBy(0.6, x=>x.pl".into()),
                    HistoryMoment(0),
                    &mut MemoryClipboard::default(),
                )
                .expect("seed");
            app.toggle_reference_browse();
            let panel = app.reference_panel.as_ref().expect("reference open");
            assert!(
                matches!(panel.mode, crate::reference::ReferenceMode::Browse),
                "a partial word opens the searched list, not the call's page"
            );
            assert_eq!(panel.query, "pl");
        }

        /// Ctrl+Space inside `s("…")` opens the samples browser on the word;
        /// inside `scale("…")` it offers scale names. Functions are never the
        /// answer inside a string.
        /// Open on one name, caret on another: the chord asks about the new
        /// one instead of closing. Pressed again on the same name, it closes.
        #[test]
        fn the_chord_retargets_to_the_word_under_the_caret() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = app_over(directory.path());
            app.editor_mut()
                .dispatch(
                    Command::PasteText("$: note(\"c4\").pianoroll".into()),
                    HistoryMoment(0),
                    &mut MemoryClipboard::default(),
                )
                .expect("seed");
            app.toggle_reference_browse();
            let showing = |app: &App| {
                app.reference_panel
                    .as_ref()
                    .and_then(|panel| panel.showing(&app.reference))
            };
            assert_eq!(showing(&app).as_deref(), Some("pianoroll"));

            // The caret moves onto another name: one press asks about it.
            app.editor_mut()
                .dispatch(
                    Command::PasteText("\n$: note(\"c4\").lpenv(4)".into()),
                    HistoryMoment(1),
                    &mut MemoryClipboard::default(),
                )
                .expect("more");
            let source = app.editor().source();
            let at = source.rfind("lpenv").expect("lpenv");
            app.editor_mut()
                .set_selection(Selection::caret(ByteOffset(at + 2)))
                .expect("caret in sine");
            app.focus = Focus::Editor;
            app.toggle_reference_browse();
            assert!(app.reference_panel.is_some(), "the panel stays open");
            assert_eq!(
                showing(&app).as_deref(),
                Some("lpenv"),
                "the chord asks about the word under the caret"
            );

            // Pressed again on the same word, it closes.
            app.toggle_reference_browse();
            assert!(app.reference_panel.is_none(), "the same word puts it away");
        }

        /// While an anchored completion list is open, the Left and Right arrows
        /// do not move the caret. The chosen name lands inside the quotes where
        /// the list opened.
        #[test]
        fn the_arrows_do_not_walk_the_caret_out_of_an_open_completion() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = app_over(directory.path());
            app.editor_mut()
                .dispatch(
                    Command::PasteText("$: s(\"oh\").dec(0.4).bank(\"".into()),
                    HistoryMoment(0),
                    &mut MemoryClipboard::default(),
                )
                .expect("seed");
            let opened_at =
                app.editor().source().find("bank(\"").expect("the call") + "bank(\"".len();
            app.editor_mut()
                .set_selection(Selection::caret(ByteOffset(opened_at)))
                .expect("caret");

            app.toggle_reference_browse();
            assert!(app.reference_panel.is_some(), "the list is up");
            assert!(
                app.reference_anchor.is_some(),
                "and it is anchored: this is a completion, not the reference being read"
            );

            for code in [KeyCode::Left, KeyCode::Left, KeyCode::Right] {
                press(&mut app, code, KeyModifiers::NONE);
                assert_eq!(
                    app.editor().primary_selection().head.0,
                    opened_at,
                    "{code:?} moved the caret out of the string being completed"
                );
                assert!(app.reference_panel.is_some(), "{code:?} closed the list");
            }

            // And the name lands where the list was opened: inside the quotes.
            app.insert_reference_name("tr909".into()).expect("it lands");
            assert!(
                app.editor().source().contains("bank(\"tr909"),
                "{}",
                app.editor().source()
            );
        }

        /// The name lands where the list opened, not at the caret, even when
        /// the anchor is an empty string such as `bank("|`.
        #[test]
        fn a_completion_lands_where_it_was_opened_even_if_the_caret_moved() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = app_over(directory.path());
            app.editor_mut()
                .dispatch(
                    Command::PasteText("$: s(\"oh\").dec(0.4).bank(\"".into()),
                    HistoryMoment(0),
                    &mut MemoryClipboard::default(),
                )
                .expect("seed");
            let opened_at =
                app.editor().source().find("bank(\"").expect("the call") + "bank(\"".len();
            app.editor_mut()
                .set_selection(Selection::caret(ByteOffset(opened_at)))
                .expect("caret");
            app.toggle_reference_browse();
            assert!(app.reference_anchor.is_some(), "anchored on the empty spot");

            // The caret goes somewhere else entirely - a click in the score
            // while the list is still up.
            app.editor_mut()
                .set_selection(Selection::caret(ByteOffset(2)))
                .expect("caret elsewhere");

            app.insert_reference_name("tr909".into()).expect("it lands");
            assert!(
                app.editor().source().contains("bank(\"tr909"),
                "the name belongs where the list was opened, not at the caret: {}",
                app.editor().source()
            );
        }

        /// The reference being READ beside a score is not anchored, and there
        /// the arrows still belong to the editor: a musician reads a page with
        /// one hand and moves the caret with the other. Only a completion, which
        /// has a place in the text to protect, keeps them.
        #[test]
        fn an_unanchored_reference_still_lets_the_arrows_move_the_caret() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = app_over(directory.path());
            app.editor_mut()
                .dispatch(
                    Command::PasteText("$: s(\"oh\").dec(0.4).bank(\"".into()),
                    HistoryMoment(0),
                    &mut MemoryClipboard::default(),
                )
                .expect("seed");
            let opened_at =
                app.editor().source().find("bank(\"").expect("the call") + "bank(\"".len();
            app.editor_mut()
                .set_selection(Selection::caret(ByteOffset(opened_at)))
                .expect("caret");
            app.toggle_reference_browse();
            // Reading, not completing: nothing in the score is waiting for a name.
            app.reference_anchor = None;

            press(&mut app, KeyCode::Left, KeyModifiers::NONE);
            assert_eq!(
                app.editor().primary_selection().head.0,
                opened_at - 1,
                "an unanchored column lets the arrow through to the caret"
            );
        }

        /// The token a chosen name replaces is the one the caret stands in,
        /// colon and all - one sound out of a mini-notation string of them.
        #[test]
        fn the_completion_replaces_the_whole_sound_under_the_caret() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = app_over(directory.path());
            app.editor_mut()
                .dispatch(
                    Command::PasteText("$: s(\"bd:1 sd:3 hh:4\")".into()),
                    HistoryMoment(0),
                    &mut MemoryClipboard::default(),
                )
                .expect("seed");
            let source = app.editor().source();
            let token = |caret: usize, app: &mut App| {
                app.editor_mut()
                    .set_selection(Selection::caret(ByteOffset(caret)))
                    .expect("caret");
                let (callee, prefix, range) = app
                    .string_completion_at_caret()
                    .expect("the caret stands in a string");
                (
                    callee,
                    prefix,
                    app.editor().source()[range.start.0..range.end.0].to_owned(),
                )
            };
            // Anywhere inside `sd:3` - before the colon, on it, after it -
            // asks about that sound and replaces the whole of it.
            let at = source.find("sd:3").expect("sd");
            for caret in [at, at + 1, at + 2, at + 3, at + 4] {
                let (callee, prefix, covered) = token(caret, &mut app);
                assert_eq!(callee, "s");
                assert_eq!(covered, "sd:3", "caret at {caret}");
                assert!(
                    "sd".starts_with(prefix.as_str()),
                    "the search is the bank, not the index: {prefix:?}"
                );
            }
            // And the replacement lands: `bd:1 piano:1 hh:4`.
            app.editor_mut()
                .set_selection(Selection::caret(ByteOffset(at + 3)))
                .expect("caret");
            app.toggle_reference_browse();
            app.insert_reference_name("piano:1".into())
                .expect("the name lands");
            assert!(
                app.editor().source().contains("s(\"bd:1 piano:1 hh:4\")"),
                "{}",
                app.editor().source()
            );
        }

        /// A chord writes its quality in marks a word does not use - `G^9`,
        /// `A-7`, `C+` - and the whole of it is the token a chosen chord
        /// replaces: standing anywhere in `G^9`, taking `Db-7` must leave
        /// `Db-7`, not `G^Db-7`.
        #[test]
        fn the_completion_replaces_the_whole_chord_under_the_caret() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = app_over(directory.path());
            app.editor_mut()
                .dispatch(
                    Command::PasteText("$: chord(\"G^9 Bb^9\").anchor(\"c4\").voicing()".into()),
                    HistoryMoment(0),
                    &mut MemoryClipboard::default(),
                )
                .expect("seed");
            let source = app.editor().source();
            let at = source.find("G^9").expect("the chord");
            // Anywhere in it - before the `G`, on the `^`, after the `9` - the
            // question is that chord, and the whole of it is what answers.
            for caret in at..=at + "G^9".len() {
                app.editor_mut()
                    .set_selection(Selection::caret(ByteOffset(caret)))
                    .expect("caret");
                let (callee, _, range) = app
                    .string_completion_at_caret()
                    .expect("the caret stands in a string");
                assert_eq!(callee, "chord");
                assert_eq!(
                    &app.editor().source()[range.start.0..range.end.0],
                    "G^9",
                    "caret at {caret}"
                );
            }
            // And the replacement lands whole: `Db-7 Bb^9`, never `G^Db-7`.
            app.editor_mut()
                .set_selection(Selection::caret(ByteOffset(at + 3)))
                .expect("caret");
            app.toggle_reference_browse();
            app.insert_reference_name("Db-7".into())
                .expect("the name lands");
            assert!(
                app.editor().source().contains("chord(\"Db-7 Bb^9\")"),
                "{}",
                app.editor().source()
            );

            // The chord already under the caret is not inserted twice: taking
            // the name that is already there leaves the score as it reads.
            let at = app.editor().source().find("Db-7").expect("the chord");
            app.editor_mut()
                .set_selection(Selection::caret(ByteOffset(at + 4)))
                .expect("caret");
            app.focus = Focus::Editor;
            app.toggle_reference_browse();
            app.insert_reference_name("Db-7".into())
                .expect("the name lands");
            assert!(
                app.editor().source().contains("chord(\"Db-7 Bb^9\")"),
                "{}",
                app.editor().source()
            );
        }

        /// A decimal point is not a link, and a name the caret is parked
        /// inside is a name to ask about rather than a place to write one.
        #[test]
        fn only_the_head_of_a_link_takes_a_call() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = app_over(directory.path());
            let mut moment = 0u64;
            let mut chain_at = |app: &mut App, source: &str, find: &str| {
                app.editor_mut()
                    .dispatch(
                        Command::SelectAll,
                        HistoryMoment(moment),
                        &mut MemoryClipboard::default(),
                    )
                    .expect("all");
                moment += 1;
                app.editor_mut()
                    .dispatch(
                        Command::PasteText(source.into()),
                        HistoryMoment(moment),
                        &mut MemoryClipboard::default(),
                    )
                    .expect("seed");
                moment += 1;
                let at = app.editor().source().find(find).expect("target");
                app.editor_mut()
                    .set_selection(Selection::caret(ByteOffset(at)))
                    .expect("caret");
                app.chain_position_at_caret()
            };
            // The head of a link, however the chain is laid out - and the end
            // of one link is the head of the next.
            assert!(chain_at(&mut app, "$: s(\"bd\").lpf(400)", "lpf(400)").is_some());
            let end = ByteOffset(app.editor().source().trim_end().len());
            app.editor_mut()
                .set_selection(Selection::caret(end))
                .expect("caret");
            assert!(
                app.chain_position_at_caret().is_some(),
                "a chain resting at `)` takes the call after it"
            );
            assert!(chain_at(&mut app, "$: note(\"c a f\")\n  .s(\"x\")", "s(\"x\")").is_some());
            assert!(chain_at(&mut app, "$: \"0 1\".n().s(\"x\")", "n()").is_some());
            // A decimal point is not a link.
            assert!(chain_at(&mut app, "$: s(\"bd\").gain(0.5)", "5)").is_none());
            // A name the caret stands inside is the question, not a place to
            // write a new one.
            assert!(chain_at(&mut app, "$: note(\"c4\").lpenv(4)", "env(4)").is_none());
            // And inside a string the names on offer are not functions.
            assert!(chain_at(&mut app, "$: s(\"bd.x\")", "x\")").is_none());
        }

        /// `chord(` with nothing typed asks for a chord as surely as
        /// `chord("` does, and the name goes in quoted. A call whose argument
        /// is not a list we keep falls through to its own reference entry.
        #[test]
        fn a_call_that_takes_a_list_offers_it_before_a_quote_is_typed() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = app_over(directory.path());
            app.editor_mut()
                .dispatch(
                    Command::PasteText("$: note(\"c a f e\").chord()".into()),
                    HistoryMoment(0),
                    &mut MemoryClipboard::default(),
                )
                .expect("seed");
            // The caret between the chord call's brackets, where an argument
            // would go.
            let open = app.editor().source().find(".chord(").expect("the call") + ".chord(".len();
            app.editor_mut()
                .set_selection(Selection::caret(ByteOffset(open)))
                .expect("caret");
            app.toggle_reference_browse();
            {
                let panel = app.reference_panel.as_ref().expect("panel open");
                assert_eq!(panel.tab, crate::reference::Tab::Chords);
            }
            // Taking one writes the string, quotes and all.
            app.insert_reference_name("C^7".into()).expect("insert");
            assert!(
                app.editor().source().contains(".chord(\"C^7\")"),
                "{}",
                app.editor().source()
            );

            // A call the studio keeps no list for still opens its own entry.
            app.reference_panel = None;
            app.focus = Focus::Editor;
            let end = ByteOffset(app.editor().source().len());
            app.editor_mut()
                .set_selection(Selection::caret(end))
                .expect("caret");
            app.editor_mut()
                .dispatch(
                    Command::PasteText("\n$: s(\"bd\").lpf()".into()),
                    HistoryMoment(1),
                    &mut MemoryClipboard::default(),
                )
                .expect("a numeric call");
            let open = app.editor().source().find(".lpf(").expect("the call") + ".lpf(".len();
            app.editor_mut()
                .set_selection(Selection::caret(ByteOffset(open)))
                .expect("caret");
            app.toggle_reference_browse();
            let panel = app.reference_panel.as_ref().expect("panel open");
            assert_eq!(panel.tab, crate::reference::Tab::Reference);
            assert!(
                matches!(panel.mode, crate::reference::ReferenceMode::Entry { .. }),
                "a number has no list, so the call's own entry answers"
            );
        }

        /// A MIDI port name has to be typed exactly, cannot be guessed, and
        /// fails silently when it is wrong. It is also the one list nothing
        /// compiled in can know, so it is read off the dock's own probe.
        #[test]
        fn the_midi_ports_complete_from_what_is_plugged_in() {
            use crate::devices::{DeviceEntry, DeviceInventory};

            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = app_over(directory.path());
            let port = |name: &str| DeviceEntry {
                name: name.to_owned(),
                id: String::new(),
                detail: String::new(),
                is_default: false,
            };
            app.devices.install_for_tests(DeviceInventory {
                midi_outputs: vec![port("IAC Driver Bus 1"), port("Scarlett 2i2")],
                midi_inputs: vec![port("LPK25")],
                ..DeviceInventory::default()
            });

            assert_eq!(app.string_role("midi"), Some(StringRole::MidiOut));
            assert_eq!(app.string_role("midin"), Some(StringRole::MidiIn));
            assert_eq!(app.string_role("midikeys"), Some(StringRole::MidiIn));

            // The outputs and the inputs are different lists, and neither is
            // a guess: they are what the probe last saw.
            assert_eq!(
                app.midi_port_names(MidiDirection::Out),
                vec!["IAC Driver Bus 1".to_owned(), "Scarlett 2i2".to_owned()]
            );
            assert_eq!(
                app.midi_port_names(MidiDirection::In),
                vec!["LPK25".to_owned()]
            );

            // And unplugging is reflected, because the list is read at the
            // moment the completion opens rather than captured at launch.
            app.devices.install_for_tests(DeviceInventory::default());
            assert!(app.midi_port_names(MidiDirection::Out).is_empty());
        }

        /// A MIDI port name can contain spaces. A chosen port replaces the
        /// whole quoted name from any caret position, and the closing single
        /// quote stays outside the replaced range.
        #[test]
        fn a_chosen_midi_port_replaces_the_whole_name_and_keeps_its_quotes() {
            use crate::devices::{DeviceEntry, DeviceInventory};

            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = app_over(directory.path());
            app.devices.install_for_tests(DeviceInventory {
                midi_inputs: vec![DeviceEntry {
                    name: "Bass Station II".to_owned(),
                    id: String::new(),
                    detail: String::new(),
                    is_default: false,
                }],
                ..DeviceInventory::default()
            });
            let source = "$: midikeys('MY MIDI DEVICE')
";
            for caret in [13, 16, 21, 26, 27] {
                app.editor_mut()
                    .dispatch(
                        Command::SelectAll,
                        HistoryMoment(0),
                        &mut MemoryClipboard::default(),
                    )
                    .expect("select");
                app.editor_mut()
                    .dispatch(
                        Command::PasteText(source.into()),
                        HistoryMoment(0),
                        &mut MemoryClipboard::default(),
                    )
                    .expect("seed");
                app.editor_mut()
                    .set_selection(Selection::caret(ByteOffset(caret)))
                    .expect("caret in the name");
                let (callee, query, range) = app
                    .string_completion_at_caret()
                    .unwrap_or_else(|| panic!("a completion at {caret}"));
                assert_eq!(callee, "midikeys");
                assert_eq!(
                    query, "MY MIDI DEVICE",
                    "the whole name is the question, not the word at {caret}"
                );
                assert_eq!(
                    &source[range.start.0..range.end.0],
                    "MY MIDI DEVICE",
                    "the whole name is what a choice replaces, from caret {caret}"
                );
                assert_eq!(
                    source.as_bytes()[range.end.0],
                    b'\'',
                    "and the closing quote is outside it"
                );
            }
        }

        /// A `.bank(...)` string names the machine, not a sound:
        /// `s("cymbal").bank("Metal")` plays `Metal_cymbal`. The list offers
        /// the library's machines, filtered by the typed text and widened by
        /// Backspace, and Enter writes the chosen one.
        #[test]
        fn a_bank_string_completes_the_machines_not_the_sounds() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = app_over(directory.path());
            // Drain the worker's background default-library load first, so the
            // library under test is the last word on the machines offered.
            settle(&mut app);
            let library = std::sync::Arc::new(rustel_runtime::samples::SampleLibrary::empty());
            library
                .register_trusted_custom(
                    r#"{"Metal_cymbal":["http://127.0.0.1:9/a.wav"],"RolandTR909_bd":["http://127.0.0.1:9/b.wav"],"RolandTR909_sd":["http://127.0.0.1:9/c.wav"],"RolandTR909_cymbal":["http://127.0.0.1:9/d.wav"],"AkaiXR10_ht":["http://127.0.0.1:9/e.wav"]}"#,
                    None,
                )
                .expect("banks");
            app.worker.set_library(std::sync::Arc::clone(&library));
            app.editor_mut()
                .dispatch(
                    Command::PasteText("$: s(\"cymbal\").bank(\"Met".into()),
                    HistoryMoment(0),
                    &mut MemoryClipboard::default(),
                )
                .expect("seed");
            let (callee, prefix, _) = app
                .string_completion_at_caret()
                .expect("the caret stands in a string");
            assert_eq!((callee.as_str(), prefix.as_str()), ("bank", "Met"));
            assert_eq!(app.string_role(&callee), Some(StringRole::Bank));
            app.toggle_reference_browse();
            {
                let panel = app.reference_panel.as_ref().expect("panel open");
                assert!(panel.bank_vocabulary(), "a machine list, not the sounds");
                let names = panel.vocabulary.as_ref().expect("vocabulary").names.clone();
                assert_eq!(
                    names,
                    vec![
                        "Metal".to_owned(),
                        "RolandTR909".to_owned(),
                        "AkaiXR10".to_owned()
                    ],
                    "every machine, the two that can play `cymbal` leading"
                );
                // Typed narrow: `Met` answers Metal and says so.
                let results = panel.results.clone();
                assert_eq!(results, vec![0], "only Metal survives the filter");
                assert_eq!(panel.query, "Met");
            }
            let action = app
                .reference_panel
                .as_mut()
                .expect("panel open")
                .confirm(&app.reference);
            assert_eq!(
                action,
                crate::reference::PanelAction::Insert("Metal".to_owned()),
                "Enter puts the machine in place"
            );

            // An empty bank string initially offers only compatible machines.
            app.reference_panel = None;
            app.focus = Focus::Editor;
            app.editor_mut()
                .dispatch(
                    Command::PasteText("\n$: s(\"cymbal\").bank(\"".into()),
                    HistoryMoment(1),
                    &mut MemoryClipboard::default(),
                )
                .expect("an empty quote");
            app.toggle_reference_browse();
            {
                let panel = app.reference_panel.as_ref().expect("panel open");
                assert!(panel.bank_vocabulary());
                assert_eq!(panel.query, "");
                assert_eq!(panel.results.len(), 2, "only machines with cymbal");
            }

            // The plain `s("…")` is untouched: the sounds, searched by the
            // word, however a bank line sits beside it.
            app.reference_panel = None;
            app.focus = Focus::Editor;
            app.editor_mut()
                .dispatch(
                    Command::PasteText("\n$: s(\"cym".into()),
                    HistoryMoment(2),
                    &mut MemoryClipboard::default(),
                )
                .expect("a sound string");
            app.toggle_reference_browse();
            {
                let panel = app.reference_panel.as_ref().expect("panel open");
                assert!(!panel.bank_vocabulary(), "still the samples browser");
                assert_eq!(panel.tab, crate::reference::Tab::Samples);
                assert_eq!(panel.sound_query, "cym");
            }

            // A machine search that matches nothing says so in the words of
            // the question: the typed text was a sound's name, and no machine
            // holds one.
            app.reference_panel = None;
            app.focus = Focus::Editor;
            app.editor_mut()
                .dispatch(
                    Command::PasteText("\n$: s(\"cymbal\").bank(\"kjkjkjsdf".into()),
                    HistoryMoment(3),
                    &mut MemoryClipboard::default(),
                )
                .expect("a bank miss");
            app.toggle_reference_browse();
            {
                let panel = app.reference_panel.as_ref().expect("panel open");
                assert!(panel.bank_vocabulary());
                assert!(panel.results.is_empty(), "nothing matches the gibberish");
                let missed = panel.banks_miss_line().expect("the miss is said");
                assert_eq!(
                    missed, "no compatible banks named \"kjkjkjsdf\"",
                    "said in the words of the question, not counted"
                );
            }

            // Machines that can play the line's sound pattern lead the list.
            // BossDR110 has no `ht`, so it does not lead for a line that asks
            // `ht`. The other machines stay available through Backspace on the
            // empty search.
            app.reference_panel = None;
            app.focus = Focus::Editor;
            app.editor_mut()
                .dispatch(
                    Command::PasteText("\n$: s(\"ht\").bank(\"".into()),
                    HistoryMoment(4),
                    &mut MemoryClipboard::default(),
                )
                .expect("a sound-shaped question");
            app.toggle_reference_browse();
            {
                let panel = app.reference_panel.as_ref().expect("panel open");
                assert!(panel.bank_vocabulary());
                let names = panel.vocabulary.as_ref().expect("vocabulary").names.clone();
                assert_eq!(
                    names,
                    vec![
                        "AkaiXR10".to_owned(),
                        "Metal".to_owned(),
                        "RolandTR909".to_owned()
                    ],
                    "the machine that can play `ht` leads, the rest follow: {names:?}"
                );
                assert_eq!(
                    panel.results.first().copied(),
                    Some(0),
                    "and the empty query lands on the compatible machine"
                );
                assert_eq!(panel.results.len(), 1);
            }
        }

        /// Ctrl+D inside a bank with underscores opens the bank list under
        /// Samples and keeps the whole machine name.  The active sound gives
        /// the boundary; splitting on the first underscore would offer `wt`.
        #[test]
        fn ctrl_d_opens_the_full_underscore_bank_in_samples() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = app_over(directory.path());
            settle(&mut app);
            let library = std::sync::Arc::new(rustel_runtime::samples::SampleLibrary::empty());
            library
                .register_trusted_custom(
                    r#"{"wt_digital_basique":["http://127.0.0.1:9/a.wav"]}"#,
                    None,
                )
                .expect("bank");
            app.worker.set_library(library);
            let score = "$: s(\"basique\").bank(\"wt_digital\")\n";
            app.scenes.current_mut().editor = crate::editor::Editor::new(score).expect("editor");
            let at = score.find("wt_digital").expect("bank name") + "wt_di".len();
            app.editor_mut()
                .set_selection(Selection::caret(ByteOffset(at)))
                .expect("caret");

            press(&mut app, KeyCode::Char('d'), KeyModifiers::CONTROL);

            let panel = app.reference_panel.as_ref().expect("bank list");
            assert_eq!(panel.tab, crate::reference::Tab::Samples);
            assert!(panel.bank_vocabulary());
            assert_eq!(
                panel
                    .vocabulary
                    .as_ref()
                    .map(|vocabulary| &vocabulary.names),
                Some(&vec!["wt_digital".to_owned()])
            );
            assert_eq!(panel.results, vec![0]);
            assert_eq!(app.status, "the sample banks - Enter replaces the word");

            // Explicit browsing still broadens an initially empty compatibility
            // list without shortening the bank's name. Ctrl+D's empty-search
            // Backspace instead returns to the function's arguments.
            press(&mut app, KeyCode::Esc, KeyModifiers::NONE);
            let score = score.replace("basique", "basique:3");
            app.scenes.current_mut().editor = crate::editor::Editor::new(&score).expect("editor");
            let at = score.find("wt_digital").expect("bank name") + 3;
            app.editor_mut()
                .set_selection(Selection::caret(ByteOffset(at)))
                .expect("caret");
            app.toggle_reference_browse();
            press(&mut app, KeyCode::Backspace, KeyModifiers::CONTROL);
            let panel = app.reference_panel.as_ref().expect("bank list");
            assert!(panel.results.is_empty());
            assert_eq!(
                panel.banks_miss_line().as_deref(),
                Some("no compatible banks")
            );
            press(&mut app, KeyCode::Backspace, KeyModifiers::NONE);
            let panel = app.reference_panel.as_ref().expect("bank list");
            assert_eq!(
                panel.vocabulary.as_ref().expect("vocabulary").names,
                ["wt_digital"]
            );
            assert_eq!(panel.results, [0]);
        }

        /// A mini-notation sound still filters the bank list: `hh:3*16` picks
        /// a variant and repeats it, and the list shows the machines that
        /// hold `hh`. The `:3` and the `*16` are structure, not sounds named
        /// `3` and `16`.
        ///
        /// Backspace edits the search first; another Backspace from the empty
        /// search explicitly includes incompatible machines.
        #[test]
        fn a_mini_notated_sound_filters_its_banks_and_backspace_reaches_the_rest() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = app_over(directory.path());
            // Drain the worker's background default-library load first, so the
            // library under test is the last word on the machines offered.
            settle(&mut app);
            let library = std::sync::Arc::new(rustel_runtime::samples::SampleLibrary::empty());
            library
                .register_trusted_custom(
                    r#"{"RolandTR909_hh":["http://127.0.0.1:9/a.wav","http://127.0.0.1:9/b.wav","http://127.0.0.1:9/c.wav","http://127.0.0.1:9/d.wav"],"AkaiLinn_hh":["http://127.0.0.1:9/c.wav"],"BossDR110_bd":["http://127.0.0.1:9/d.wav"]}"#,
                    None,
                )
                .expect("banks");
            app.worker.set_library(std::sync::Arc::clone(&library));
            app.editor_mut()
                .dispatch(
                    Command::PasteText("$: s(\"hh:3*16\").velocity(tri.fast(4)).bank(\"".into()),
                    HistoryMoment(0),
                    &mut MemoryClipboard::default(),
                )
                .expect("seed");
            // Explicit browsing keeps Backspace available to widen bank compatibility.
            app.toggle_reference_browse();
            {
                let panel = app.reference_panel.as_ref().expect("panel open");
                assert!(panel.bank_vocabulary(), "a machine list, not the sounds");
                let names = panel.vocabulary.as_ref().expect("vocabulary").names.clone();
                assert_eq!(
                    names,
                    vec![
                        "RolandTR909".to_owned(),
                        "AkaiLinn".to_owned(),
                        "BossDR110".to_owned()
                    ],
                    "only RolandTR909 has hh:3; the others remain available on request"
                );
                assert_eq!(panel.results.len(), 1, "only the bank with hh:3 is shown");
            }
            // Typing narrows to the matching machine…
            let panel = app.reference_panel.as_mut().expect("panel open");
            assert!(panel.type_char(&app.reference, 'R'));
            assert_eq!(panel.results.len(), 1, "only RolandTR909 answers `R`");
            // Removing the search preserves the compatibility filter.
            assert!(panel.backspace(&app.reference));
            assert_eq!(panel.query, "");
            assert_eq!(panel.results.len(), 1);
            // A second Backspace asks for every bank, including missing variants.
            press(&mut app, KeyCode::Backspace, KeyModifiers::NONE);
            let panel = app.reference_panel.as_ref().expect("panel open");
            assert_eq!(
                panel.results.len(),
                3,
                "backspacing goes back to all of them, BossDR110 among them"
            );
        }

        #[test]
        fn bank_compatibility_survives_refresh_and_reopens_after_browsing_all() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = app_over(directory.path());
            settle(&mut app);
            let library = std::sync::Arc::new(rustel_runtime::samples::SampleLibrary::empty());
            library.register_trusted_custom(
                r#"{"Ready_bd":["http://127.0.0.1:9/0.wav","http://127.0.0.1:9/1.wav","http://127.0.0.1:9/2.wav","http://127.0.0.1:9/3.wav"],"Ready_sd":["http://127.0.0.1:9/0.wav"],"Short_bd":["http://127.0.0.1:9/0.wav"],"Short_sd":["http://127.0.0.1:9/0.wav"],"Hat_hh":["http://127.0.0.1:9/0.wav"]}"#,
                None,
            ).expect("banks");
            app.worker.set_library(library);
            let score = "$: sound('BD:3 sd').dec(.1).bank(\"Short\").fast(2)\n$: s(\"hh\")\n";
            app.scenes.current_mut().editor = crate::editor::Editor::new(score).expect("editor");
            let at = score.find("Short").expect("bank") + 2;
            app.editor_mut()
                .set_selection(Selection::caret(ByteOffset(at)))
                .expect("caret");
            app.toggle_reference_browse();
            press(&mut app, KeyCode::Backspace, KeyModifiers::CONTROL);
            let visible = |app: &App| {
                let panel = app.reference_panel.as_ref().expect("bank list");
                let names = &panel.vocabulary.as_ref().expect("vocabulary").names;
                panel
                    .results
                    .iter()
                    .map(|index| names[*index].clone())
                    .collect::<Vec<_>>()
            };
            assert_eq!(
                visible(&app),
                ["Ready"],
                "the explicit variant and every sound must exist"
            );

            // A refresh must use the opening context, even if the editor caret moves.
            app.editor_mut()
                .set_selection(Selection::caret(ByteOffset(score.len())))
                .expect("caret");
            app.install_catalogue();
            assert_eq!(visible(&app), ["Ready"]);
            press(&mut app, KeyCode::Backspace, KeyModifiers::NONE);
            assert_eq!(visible(&app), ["Ready", "Hat", "Short"]);
            app.install_catalogue();
            assert_eq!(
                visible(&app),
                ["Ready", "Hat", "Short"],
                "refresh preserves the opt-out"
            );

            press(&mut app, KeyCode::Esc, KeyModifiers::NONE);
            app.editor_mut()
                .set_selection(Selection::caret(ByteOffset(at)))
                .expect("caret");
            press(&mut app, KeyCode::Char('d'), KeyModifiers::CONTROL);
            assert_eq!(
                visible(&app),
                ["Ready"],
                "reopening restores the useful default"
            );
            press(&mut app, KeyCode::Enter, KeyModifiers::NONE);
            assert_eq!(app.editor().source(), score.replace("Short", "Ready"));
        }

        /// A native synth plays by its own name whatever the bank, and the lint
        /// lets it pass under any, so `sbd` beside `hh` asks the bank for `hh`
        /// alone. Counted as a sound no bank holds, it emptied the list.
        #[test]
        fn native_synths_beside_samples_leave_the_banks_that_hold_the_samples() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = app_over(directory.path());
            settle(&mut app);
            let library = std::sync::Arc::new(rustel_runtime::samples::SampleLibrary::empty());
            library
                .register_trusted_custom(
                    r#"{"RolandTR909_hh":["http://127.0.0.1:9/a.wav"],"AkaiLinn_hh":["http://127.0.0.1:9/b.wav"],"BossDR110_bd":["http://127.0.0.1:9/c.wav"]}"#,
                    None,
                )
                .expect("banks");
            app.worker.set_library(library);
            for (score, expected) in [
                (
                    "$: s(\"sbd hh*2\").bank(\"",
                    &["AkaiLinn", "RolandTR909"][..],
                ),
                // Nothing but synths asks nothing of the bank: every one is offered.
                (
                    "$: s(\"sine white\").bank(\"",
                    &["AkaiLinn", "BossDR110", "RolandTR909"][..],
                ),
            ] {
                app.reference_panel = None;
                app.focus = Focus::Editor;
                app.scenes.current_mut().editor =
                    crate::editor::Editor::new(score).expect("editor");
                app.editor_mut()
                    .set_selection(Selection::caret(ByteOffset(score.len())))
                    .expect("caret");
                app.toggle_reference_at_caret();
                let panel = app.reference_panel.as_ref().expect("bank list");
                let names = &panel.vocabulary.as_ref().expect("vocabulary").names;
                let mut visible = panel
                    .results
                    .iter()
                    .map(|index| names[*index].as_str())
                    .collect::<Vec<_>>();
                visible.sort_unstable();
                assert_eq!(visible, expected, "{score}");
            }
        }

        /// On a banked chain, the bank's sounds rank first and are inserted without
        /// its prefix. An unknown bank connects nothing, and a patterned bank
        /// connects each machine it names.
        #[test]
        fn a_sound_string_in_a_banked_chain_ranks_the_bank_and_inserts_bare() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = app_over(directory.path());
            // The worker loads its default library in the background; drain
            // that arrival first, so the library under test is the last word.
            settle(&mut app);
            let library = std::sync::Arc::new(rustel_runtime::samples::SampleLibrary::empty());
            library
                .register_trusted_custom(
                    r#"{"Metal_cymbal":["http://127.0.0.1:9/a.wav"],"Tin_cymbal":["http://127.0.0.1:9/b.wav"],"Metal_bd":["http://127.0.0.1:9/c.wav"]}"#,
                    None,
                )
                .expect("banks");
            app.worker.set_library(std::sync::Arc::clone(&library));
            app.editor_mut()
                .dispatch(
                    Command::PasteText("$: s(\"cym\").bank(\"Metal\")".into()),
                    HistoryMoment(0),
                    &mut MemoryClipboard::default(),
                )
                .expect("seed");
            // Walk the caret back into the sound string - the question is the
            // sound, with the bank call beside it.
            for _ in 0..16 {
                app.editor_mut()
                    .dispatch(
                        Command::Move {
                            motion: crate::editor::Motion::Left,
                            extend: false,
                        },
                        HistoryMoment(0),
                        &mut MemoryClipboard::default(),
                    )
                    .expect("a move");
            }
            let (callee, prefix, _) = app
                .string_completion_at_caret()
                .expect("the caret stands in the sound string");
            assert_eq!((callee.as_str(), prefix.as_str()), ("s", "cym"));
            app.toggle_reference_browse();
            {
                let panel = app.reference_panel.as_ref().expect("panel open");
                assert_eq!(panel.tab, crate::reference::Tab::Samples);
                assert_eq!(
                    panel.sound_prefix,
                    vec!["Metal".to_owned()],
                    "the chain's bank is read off the text"
                );
                // `cym` matches Metal's and Tin's cymbals; the connected
                // machine's own leads.
                let first = panel
                    .sound_results
                    .first()
                    .and_then(|&index| panel.sounds.get(index))
                    .map(|entry| entry.name.clone())
                    .expect("a match");
                assert_eq!(first, "Metal_cymbal", "the bank's own sound leads");
                assert_eq!(panel.sound_results.len(), 2, "nothing else is hidden");
                // Enter on the leading row offers the bare spelling.
                let row = panel.sound_selected;
                let offered = panel
                    .sound_of(panel.sound_rows()[row], true)
                    .expect("the row names a sound");
                assert_eq!(offered, "cymbal:0", "bare - the bank supplies the prefix");
            }
            // Taking it: the score reads `$: s("cymbal:0").bank("Metal")` -
            // a pattern that keeps playing under another machine by editing
            // the one call.
            let chosen = match app
                .reference_panel
                .as_mut()
                .expect("panel open")
                .confirm(&app.reference)
            {
                crate::reference::PanelAction::Insert(chosen) => chosen,
                other => panic!("Enter offers the sound, not {other:?}"),
            };
            assert_eq!(chosen, "cymbal", "bare, patternizable under the bank");

            // An unknown bank connects nothing: no priority, no stripping -
            // the full name is the only spelling that would play.
            app.reference_panel = None;
            app.focus = Focus::Editor;
            app.editor_mut()
                .dispatch(
                    Command::PasteText("\n$: s(\"cym\").bank(\"nope\"".into()),
                    HistoryMoment(1),
                    &mut MemoryClipboard::default(),
                )
                .expect("an unknown bank");
            app.toggle_reference_browse();
            {
                let panel = app.reference_panel.as_ref().expect("panel open");
                assert!(panel.sound_prefix.is_empty(), "nope connects nothing");
                let first = panel
                    .sound_results
                    .first()
                    .and_then(|&index| panel.sounds.get(index))
                    .map(|entry| entry.name.clone())
                    .expect("a match");
                assert!(!first.starts_with("Metal_"), "no machine is privileged");
            }

            // A patterned bank connects each machine it names.
            app.reference_panel = None;
            app.focus = Focus::Editor;
            app.editor_mut()
                .dispatch(
                    Command::PasteText("\n$: s(\"cym\").bank(\"Metal Tin\")".into()),
                    HistoryMoment(2),
                    &mut MemoryClipboard::default(),
                )
                .expect("a two-machine bank");
            for _ in 0..23 {
                app.editor_mut()
                    .dispatch(
                        Command::Move {
                            motion: crate::editor::Motion::Left,
                            extend: false,
                        },
                        HistoryMoment(0),
                        &mut MemoryClipboard::default(),
                    )
                    .expect("a move");
            }
            app.toggle_reference_browse();
            {
                let panel = app.reference_panel.as_mut().expect("panel open");
                assert_eq!(
                    panel.sound_prefix,
                    vec!["Metal".to_owned(), "Tin".to_owned()],
                    "the whole pattern is the connection"
                );
                let chosen = match panel.confirm(&app.reference) {
                    crate::reference::PanelAction::Insert(chosen) => chosen,
                    other => panic!("Enter offers the sound, not {other:?}"),
                };
                assert_eq!(
                    chosen, "cymbal",
                    "bare under either machine - the bank call prefixes"
                );
            }
        }

        /// A score writes `s('bd')` as readily as `s("bd")`, and the chord
        /// answers the same question in both - the synths included.
        #[test]
        fn a_single_quoted_sound_is_completed_like_a_double_quoted_one() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = app_over(directory.path());
            app.editor_mut()
                .dispatch(
                    Command::PasteText("$: note(\"c4\").s('supe".into()),
                    HistoryMoment(0),
                    &mut MemoryClipboard::default(),
                )
                .expect("seed");
            let (callee, prefix, _) = app
                .string_completion_at_caret()
                .expect("the caret stands in a string");
            assert_eq!((callee.as_str(), prefix.as_str()), ("s", "supe"));
            app.toggle_reference_browse();
            {
                let panel = app.reference_panel.as_ref().expect("panel open");
                assert_eq!(panel.tab, crate::reference::Tab::Samples);
                assert_eq!(panel.sound_query, "supe");
                assert!(
                    panel
                        .sounds
                        .iter()
                        .any(|entry| entry.name == "supersaw" || entry.name == "sawtooth"),
                    "the synths are in the list the chord opens"
                );
            }

            // The quote alone, nothing typed yet: still the sounds, not the
            // entry for `s` - the question is which sound, and the answer is
            // the whole browser.
            app.reference_panel = None;
            app.focus = Focus::Editor;
            app.editor_mut()
                .dispatch(
                    Command::PasteText("rsaw')\n$: s('".into()),
                    HistoryMoment(1),
                    &mut MemoryClipboard::default(),
                )
                .expect("an empty quote");
            let (callee, prefix, _) = app
                .string_completion_at_caret()
                .expect("the caret stands in an empty string");
            assert_eq!((callee.as_str(), prefix.as_str()), ("s", ""));
            app.toggle_reference_browse();
            let panel = app.reference_panel.as_ref().expect("panel open");
            assert_eq!(panel.tab, crate::reference::Tab::Samples);
            assert!(panel.sound_query.is_empty());
        }

        /// Inside `.color("…")` the question is a colour, not a function: the
        /// chord offers the names a score can use, and Enter replaces the word.
        #[test]
        fn inside_a_colour_the_chord_offers_colours() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = app_over(directory.path());
            app.editor_mut()
                .dispatch(
                    Command::PasteText("$: note(\"c4\").color(\"cy".into()),
                    HistoryMoment(0),
                    &mut MemoryClipboard::default(),
                )
                .expect("seed");
            app.toggle_reference_browse();
            let panel = app.reference_panel.as_ref().expect("panel open");
            let vocabulary = panel.vocabulary.as_ref().expect("a word list");
            assert_eq!(vocabulary.subject, "colours");
            assert!(vocabulary.swatches, "a colour list is drawn in its colours");
            let offered: Vec<&str> = panel
                .results
                .iter()
                .filter_map(|&index| vocabulary.names.get(index))
                .map(String::as_str)
                .collect();
            assert_eq!(offered.first().copied(), Some("cyan"), "{offered:?}");
        }

        #[test]
        fn inside_a_string_the_chord_offers_the_names_valid_there() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = app_over(directory.path());
            app.editor_mut()
                .dispatch(
                    Command::PasteText("s(\"Aka".into()),
                    HistoryMoment(0),
                    &mut MemoryClipboard::default(),
                )
                .expect("seed");
            app.toggle_reference_browse();
            {
                let panel = app.reference_panel.as_ref().expect("panel open");
                assert_eq!(panel.tab, crate::reference::Tab::Samples);
                assert_eq!(panel.sound_query, "Aka");
            }
            // Inside a string the chord always re-targets rather than closing;
            // Esc is the way out.
            app.reference_panel = None;
            app.focus = Focus::Editor;

            app.editor_mut()
                .dispatch(
                    Command::PasteText("i\").scale(\"C:maj".into()),
                    HistoryMoment(1),
                    &mut MemoryClipboard::default(),
                )
                .expect("more");
            app.toggle_reference_browse();
            let panel = app.reference_panel.as_ref().expect("panel open");
            assert_eq!(panel.tab, crate::reference::Tab::Scales);
            // `C:maj` is one token: the tab searches it whole and offers the
            // majors, and taking one replaces the tonic with it.
            let names: Vec<String> = panel.scale_names();
            assert!(
                names.first().is_some_and(|name| name.starts_with("major")),
                "{names:?}"
            );
            let anchor = app.reference_anchor.as_ref().expect("anchored");
            assert_eq!(
                anchor.word, "C:maj",
                "a scale is one word, tonic and all, and the whole of it is replaced"
            );
        }
    }

    mod completion_unicode {
        //! Completion keeps character boundaries in object keys.

        use crate::app::tests::support::*;
        use crate::app::*;
        use crate::editor::Selection;

        #[test]
        fn a_non_ascii_object_key_before_a_string_is_not_a_panic() {
            let directory = tempfile::tempdir().expect("temp dir");
            for key in ["pré", "🎵"] {
                let (mut app, _scene) =
                    plain_app(directory.path(), &format!("$: lfo({{ {key}: \"si"));
                // Inside the open quote, after the `si` - not `source().len()`,
                // which the trailing newline puts on the next line.
                let caret = app.editor().source().find("si").expect("the string") + "si".len();
                app.editor_mut()
                    .set_selection(Selection::caret(ByteOffset(caret)))
                    .expect("caret");
                assert_eq!(
                    app.object_key_before_string_at_caret(),
                    None,
                    "`{key}` ends in a non-word char, so no key follows it"
                );
                let (callee, prefix, _) = app
                    .string_completion_at_caret()
                    .expect("the caret stands in a string");
                assert_eq!(callee, "lfo", "the completion falls back to the callee");
                assert_eq!(prefix, "si", "the token under the caret survives");
            }
        }
    }
}
