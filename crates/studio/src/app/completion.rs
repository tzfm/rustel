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
        word_before(editor.document(), editor.primary_selection().head)
    }

    /// The name and range of the innermost call whose `(` is open at the
    /// caret, as in `setcpm(|`. Brackets in strings, comments and regexes
    /// are ignored. The name must touch its `(`; any other `(` groups, and
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
            if self.reference.lookup(whole).is_some() || self.reference.lookup(tail).is_none() {
                return None;
            }
        }
        Some(ByteOffset(start.0 + from)..caret)
    }

    /// The completion list for a new link in a chain, anchored to the
    /// half-typed name so the chosen function takes its place - and goes
    /// in as the call it is.
    pub(super) fn chain_completion_panel(&mut self) -> Option<ReferencePanel> {
        let range = self.chain_position_at_caret()?;
        let typed = self.editor().document().slice(range.clone()).ok()?;
        let panel = ReferencePanel::browse_for(&self.reference, &typed);
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
        let asked = self.word_range_at_caret().map(|(word, _)| word);
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
        let retarget =
            self.reference_panel.is_some() && ((asked.is_some() && asked != showing) || new_link);
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
                    let anchor = self.word_range_at_caret();
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
                    let entry = if typing {
                        None
                    } else {
                        self.enclosing_call_at_caret()
                            .and_then(|(word, _)| self.reference.lookup(&word))
                    };
                    let panel = match (entry, &anchor) {
                        (Some(index), _) => ReferencePanel::open(&self.reference, index),
                        (None, Some((word, _))) => {
                            ReferencePanel::browse_for(&self.reference, word)
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

fn enclosing_call_argument(source: &str, caret: usize) -> Option<(Range<usize>, usize)> {
    let code = rustel_runtime::lint::code_only(source.get(..caret)?);
    scan::open_parens(&code, code.len()).find_map(|open| {
        let name = scan::name_ending_at(&code, open);
        if name.is_empty() {
            return None;
        }
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
        Some((name, argument))
    })
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
    use super::enclosing_call_argument;

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
        ] {
            let (name, actual) = enclosing_call_argument(source, source.len())
                .unwrap_or_else(|| panic!("missing call: {source}"));
            assert_eq!(&source[name], callee, "{source}");
            assert_eq!(actual, argument, "{source}");
        }
        assert!(enclosing_call_argument("s(\"bd\")", 7).is_none());
    }
}
