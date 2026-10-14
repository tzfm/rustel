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
        let word_byte = move |byte: u8| {
            // An apostrophe belongs to a name - `s("bell'")` - but never
            // when it is the string's own delimiter, or the scan walks
            // straight through the closing quote and the replacement eats
            // it. `midin`/`midikeys` need single quotes, so this is every
            // completion they offer.
            byte != quote
                && (byte.is_ascii_alphanumeric()
                    || matches!(byte, b'_' | b'#' | b'\'')
                    || (chord && matches!(byte, b'^' | b'-' | b'+')))
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
        // is what a choice replaces. Every other vocabulary here is
        // deliberately space-free and keeps its token, because a pattern
        // string holds several names and only the one under the caret is
        // the question.
        if matches!(role, Some(StringRole::MidiIn | StringRole::MidiOut)) {
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
        let whole = matches!(
            self.string_completion_at_caret()
                .and_then(|(callee, _, _)| self.string_role(&callee)),
            Some(StringRole::MidiIn | StringRole::MidiOut)
        );
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
