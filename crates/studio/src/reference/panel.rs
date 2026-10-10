//! Panel construction, query editing, and navigation actions.

use super::*;

/// Headings by use across the 186 songs in
/// `crates/runtime/tests/e2e/scores/corpus/songs`, most used first. An empty
/// search lists the headings in this order. Headings not named here follow.
const HEADINGS_BY_USE: &[&str] = &[
    "temporal",
    "tonal",
    "audio",
    "amplitude",
    "orbit",
    "filter",
    "combiners",
    "math",
    "pitch",
    "samples",
    "visualization",
    "functional",
    "fm",
    "distortion",
    "generators",
    "envelope",
    "other",
    "rustel",
];

impl ReferencePanel {
    /// The list, already searched for `query` - the word under the caret,
    /// so a misspelt name opens on its nearest real ones.
    pub fn browse_for(reference: &Reference, query: &str) -> Self {
        let mut panel = Self::browse(reference);
        panel.query = query.to_owned();
        panel.quick = true;
        // A new query starts at its best answer, the way typing one does.
        // `browse` has already left the cursor on the first NAME of the
        // whole reference, which is not row zero once there are headings.
        panel.selected = 0;
        panel.refresh(reference);
        panel
    }

    /// A word list instead of the reference: the names valid inside the
    /// string the caret stands in, searched for what is already typed.
    /// Enter puts the chosen name in the word's place.
    pub fn vocabulary_for(
        reference: &Reference,
        subject: &'static str,
        names: Vec<String>,
        query: &str,
    ) -> Self {
        Self::vocabulary_with(reference, subject, names, query, false)
    }

    /// Start with banks that support the score's sounds. Backspace from an
    /// empty search opens the remaining banks for an intentional change.
    pub fn bank_vocabulary_for(
        reference: &Reference,
        names: Vec<String>,
        sounds: Vec<String>,
        compatible_count: usize,
        query: &str,
    ) -> Self {
        let mut panel = Self::vocabulary_for(reference, "sample banks", names, query);
        panel.tab = Tab::Samples;
        panel
            .vocabulary
            .as_mut()
            .expect("just installed")
            .bank_compatibility = Some(BankCompatibility {
            only_compatible: !sounds.is_empty(),
            sounds,
            count: compatible_count,
        });
        panel.refresh(reference);
        panel
    }

    /// A finite parameter's values, using the same explanations rendered in
    /// that function's reference entry.
    pub fn choice_vocabulary_for(
        reference: &Reference,
        set: &rustel_core::reference::ReferenceChoiceSet,
        query: &str,
    ) -> Self {
        let names = set
            .choices
            .iter()
            .map(|choice| choice.value.to_owned())
            .collect();
        let details = set
            .choices
            .iter()
            .map(|choice| choice.description.to_owned())
            .collect();
        let mut panel = Self::vocabulary_with(reference, set.entry, names, query, false);
        panel.vocabulary.as_mut().expect("just installed").details = details;
        panel.refresh(reference);
        panel
    }

    /// Clear the search on a finite vocabulary and rest the cursor on its
    /// current value. Contextual reference is for comparing the documented
    /// choices; using the value as a search would hide every alternative
    /// until the reader erased it. A bank list keeps its compatibility
    /// filter, so a current bank that cannot play the receiver's sounds is
    /// not listed, not selected, and Enter replaces it.
    pub fn show_all_vocabulary_at(&mut self, reference: &Reference, current: &str) -> bool {
        let Some(vocabulary) = &self.vocabulary else {
            return false;
        };
        let name = vocabulary
            .names
            .iter()
            .position(|name| name.eq_ignore_ascii_case(current));
        self.query.clear();
        self.selected = 0;
        self.refresh(reference);
        let Some(name) = name else {
            return false;
        };
        let Some(result) = self.results.iter().position(|index| *index == name) else {
            return false;
        };
        self.selected = self.row_of_result(result);
        true
    }

    /// A word list whose names are colours: each row is drawn in the
    /// colour it names, so the list is a picker.
    pub fn color_vocabulary_for(reference: &Reference, query: &str) -> Self {
        Self::vocabulary_with(reference, "colours", color_vocabulary(), query, true)
    }

    fn vocabulary_with(
        reference: &Reference,
        subject: &'static str,
        names: Vec<String>,
        query: &str,
        swatches: bool,
    ) -> Self {
        let mut panel = Self::browse(reference);
        panel.quick = true;
        panel.query = query.to_owned();
        panel.vocabulary = Some(Vocabulary {
            subject,
            details: vec![String::new(); names.len()],
            names,
            swatches,
            bank_compatibility: None,
            #[cfg(feature = "vst")]
            plugin: None,
        });
        // As in `browse_for`: the word asked for is the one to land on.
        panel.selected = 0;
        panel.refresh(reference);
        panel
    }

    pub fn browse(reference: &Reference) -> Self {
        let mut panel = Self {
            selection: None,
            tab: Tab::Reference,
            mode: ReferenceMode::Browse,
            query: String::new(),
            results: Vec::new(),
            snippets_last: false,
            selected: 0,
            rows: Vec::new(),
            sounds: Vec::new(),
            sound_query: String::new(),
            sound_prefix: Vec::new(),
            sound_results: Vec::new(),
            sound_selected: 0,
            expanded: None,
            open_family: None,
            vocabulary: None,
            argument_reference: None,
            // Start at the first section, with its shelves folded.
            #[cfg(feature = "hydra")]
            section_open: std::iter::once(0).collect(),
            #[cfg(feature = "hydra")]
            snippet_open: std::collections::BTreeSet::new(),
            #[cfg(feature = "hydra")]
            snippet_selected: 0,
            #[cfg(feature = "hydra")]
            generator: super::super::ideas::Generator::default(),
            #[cfg(feature = "hydra")]
            snippet_code_scroll: std::cell::Cell::new(0),
            quick: false,
            intent: PanelIntent::Copy,
            imports: Vec::new(),
            open_categories: std::collections::BTreeSet::new(),
            chord_query: String::new(),
            chord_selected: 0,
            open_quality: None,
            scale_query: String::new(),
            scale_selected: 0,
            open_scale: None,
            #[cfg(feature = "vst")]
            vst: VstTab::default(),
            scroll: std::cell::Cell::new(0),
            sound_scroll: std::cell::Cell::new(0),
            chord_scroll: std::cell::Cell::new(0),
            scale_scroll: std::cell::Cell::new(0),
            #[cfg(feature = "hydra")]
            snippet_scroll: std::cell::Cell::new(0),
            hold_scroll: false,
            confirm_trim: None,
            confirm_delete: None,
        };
        // One writer for the results and the rows over them, so the two can
        // never be built by different rules.
        panel.refresh(reference);
        panel
    }

    pub fn open(reference: &Reference, index: usize) -> Self {
        // The list behind the entry is seeded with the entry's own name, so
        // ← has somewhere sensible to step out to. `quick` stays false and
        // `from_browse` stays false: Enter inserts, Esc still closes in one
        // press.
        let mut panel = Self::browse(reference);
        if let Some(entry) = reference.entry(index) {
            panel.query = entry.name.clone();
            panel.selected = 0;
            panel.refresh(reference);
            panel.selected = panel
                .results
                .iter()
                .position(|&found| found == index)
                .map_or(0, |position| panel.row_of_result(position));
        }
        panel.mode = ReferenceMode::Entry {
            index,
            scroll: 0,
            from_browse: false,
        };
        panel
    }

    /// The samples tab, straight away, to put a sound in the score.
    pub fn samples(reference: &Reference, sounds: Vec<SoundEntry>) -> Self {
        let mut panel = Self::browse(reference);
        panel.tab = Tab::Samples;
        panel.intent = PanelIntent::Insert;
        panel.set_sounds(sounds);
        panel
    }

    /// The name the reference tab is answering about: the open entry's, or
    /// the search box's word. `None` when it is answering about nothing in
    /// particular, or is not on the reference tab at all.
    pub fn showing(&self, reference: &Reference) -> Option<String> {
        if self.tab != Tab::Reference {
            return None;
        }
        match self.mode {
            ReferenceMode::Entry { index, .. } => {
                reference.entry(index).map(|entry| entry.name.clone())
            }
            ReferenceMode::Browse => {
                let query = self.query.trim();
                (!query.is_empty()).then(|| query.to_owned())
            }
        }
    }

    /// The snippet shelf, straight away.
    #[cfg(feature = "hydra")]
    pub fn snippets(reference: &Reference) -> Self {
        let mut panel = Self::browse(reference);
        panel.tab = Tab::Examples;
        panel
    }

    /// Shift+Tab: the tab before this one, wrapping, as in the devices
    /// panel and the settings sheet.
    pub fn previous_tab(&mut self) {
        // Round the other way: seven tabs at most, and no list to reverse.
        for _ in 0..tabs().len().saturating_sub(1) {
            self.toggle_tab();
        }
    }

    pub fn select_tab(&mut self, tab: Tab) {
        #[cfg(feature = "hydra")]
        if (self.tab == Tab::Generator) != (tab == Tab::Generator) {
            std::mem::swap(&mut self.snippet_selected, &mut self.generator.saved_row);
            self.selection = None;
        }
        #[cfg(feature = "hydra")]
        if self.tab != tab {
            self.snippet_code_scroll.set(0);
        }
        self.tab = tab;
    }

    pub fn toggle_tab(&mut self) {
        let all = tabs();
        let at = all
            .iter()
            .position(|(tab, _)| *tab == self.tab)
            .unwrap_or(0);
        self.select_tab(all[(at + 1) % all.len()].0);
    }

    pub(crate) fn refresh(&mut self, reference: &Reference) {
        if let Some(vocabulary) = &self.vocabulary {
            let count = vocabulary
                .bank_compatibility
                .as_ref()
                .filter(|bank| bank.only_compatible)
                .map_or(vocabulary.names.len(), |bank| bank.count);
            self.results = vocabulary.rank(self.query.trim(), count);
        } else {
            self.results = reference.search_with(&self.query, self.snippets_last);
        }
        self.rows = self.grouped_rows(reference);
        self.selected = self.selected.min(self.rows.len().saturating_sub(1));
        // A heading is a label, not a choice. Typing puts the cursor on row
        // zero, which in a grouped list is the first heading; the row under
        // it is always a name, because a heading with nothing under it is
        // never emitted.
        if matches!(self.rows.get(self.selected), Some(BrowseRow::Tag(_))) {
            self.selected += 1;
        }
    }

    /// The rows the browse list draws: the results, gathered under the tag
    /// each entry is filed by. An empty search orders the headings by
    /// [`HEADINGS_BY_USE`].
    ///
    /// A search orders the headings by their best member instead. What was
    /// typed has to stay on the first row the cursor lands on, or the search
    /// stops answering. Ungrouped this is the results, one row each.
    fn grouped_rows(&self, reference: &Reference) -> Vec<BrowseRow> {
        let flat = || (0..self.results.len()).map(BrowseRow::Entry).collect();
        // A word list - scales, chords, colours - has no tags to group by.
        // Nor does a `tag:` query: the filter has already said what the
        // whole list is, the count line says it again, and one heading over
        // everything says it a third time. Worse, an entry filed under its
        // first tag can sit under a heading that is not the word searched
        // for, which reads as the list ignoring what was typed.
        if self.vocabulary.is_some() || TagFilter::parse(&self.query).name.is_some() {
            return flat();
        }
        let mut runs: Vec<(&str, Vec<usize>)> = Vec::new();
        for (position, &index) in self.results.iter().enumerate() {
            let Some(entry) = reference.entry(index) else {
                continue;
            };
            match runs.iter_mut().find(|(tag, _)| *tag == entry.heading()) {
                Some((_, members)) => members.push(position),
                None => runs.push((entry.heading(), vec![position])),
            }
        }
        // One kind of thing is no grouping.
        if runs.len() < 2 {
            return flat();
        }
        if self.query.trim().is_empty() {
            runs.sort_by_key(|(tag, _)| {
                HEADINGS_BY_USE
                    .iter()
                    .position(|heading| heading == tag)
                    .unwrap_or(HEADINGS_BY_USE.len())
            });
        }
        let mut rows = Vec::with_capacity(self.results.len() + runs.len());
        for (_, members) in &runs {
            rows.push(BrowseRow::Tag(members[0]));
            rows.extend(members.iter().copied().map(BrowseRow::Entry));
        }
        rows
    }

    /// The rows of the browse list, headings and all.
    pub fn browse_rows(&self) -> &[BrowseRow] {
        &self.rows
    }

    /// What the cursor is on: an entry in the reference, or a word in the
    /// list of them - `None` on a heading, which is a label with nothing
    /// behind it.
    pub fn selected_result(&self) -> Option<usize> {
        match self.rows.get(self.selected)? {
            BrowseRow::Tag(_) => None,
            BrowseRow::Entry(position) => self.results.get(*position).copied(),
        }
    }

    /// The row a result sits on. Ungrouped, the position itself.
    pub fn row_of_result(&self, position: usize) -> usize {
        self.rows
            .iter()
            .position(|row| *row == BrowseRow::Entry(position))
            .unwrap_or(position)
    }

    /// Where the terminal's own cursor sits while a search box here has the
    /// keyboard: just past the query, on the search line.
    pub fn search_cursor(&self, inner: Rect) -> Option<(u16, u16)> {
        if !self.wants_text() || inner.is_empty() {
            return None;
        }
        let query = if self.bank_list() {
            &self.query
        } else {
            match self.tab {
                Tab::Samples => &self.sound_query,
                #[cfg(feature = "vst")]
                Tab::Vst => self.vst.query(),
                _ => &self.query,
            }
        };
        let column = "search: ".len() + query.chars().count();
        Some((
            inner.x + (column as u16).min(inner.width.saturating_sub(1)),
            inner.y + 1,
        ))
    }

    /// Whether this view has a search box, and so a use for letters.
    pub fn wants_text(&self) -> bool {
        match self.tab {
            Tab::Samples | Tab::Chords | Tab::Scales => true,
            #[cfg(feature = "vst")]
            Tab::Vst => true,
            Tab::Reference => matches!(self.mode, ReferenceMode::Browse),
            // The shelf has no search box; its letters are shortcuts.
            #[cfg(feature = "hydra")]
            Tab::Examples | Tab::Generator => false,
        }
    }

    /// Whether a letter this view had no use for belongs to the score.
    ///
    /// Only one view: the reference open on an entry's page, which has no
    /// search box because there is nothing to search. Reading an entry and
    /// typing what it teaches is why the docs are beside the code.
    /// Not the same question as `!wants_text()`, which the examples shelf
    /// also answers no to: the shelf has no search box either, and its
    /// unclaimed letters are its own business, not the music's.
    pub fn types_through(&self) -> bool {
        match self.tab {
            Tab::Reference => !matches!(self.mode, ReferenceMode::Browse),
            Tab::Samples | Tab::Chords | Tab::Scales => false,
            #[cfg(feature = "vst")]
            Tab::Vst => false,
            #[cfg(feature = "hydra")]
            Tab::Examples | Tab::Generator => false,
        }
    }

    /// A whole paste into the search box as one edit.
    ///
    /// Character by character this re-ranked the catalogue once per
    /// character; a long paste over a few thousand imported banks was
    /// seconds of a studio that answered nothing. One edit, one rank.
    pub fn paste_query(&mut self, reference: &Reference, text: &str) -> bool {
        if !self.wants_text() {
            return false;
        }
        let room = MAX_QUERY_CHARS.saturating_sub(self.query_len());
        let addition: String = text.chars().take(room).collect();
        if addition.is_empty() {
            return true;
        }
        if self.bank_list() {
            self.query.push_str(&addition);
            self.selected = 0;
            self.refresh(reference);
            return true;
        }
        match self.tab {
            Tab::Chords => {
                self.chord_query.push_str(&addition);
                self.chord_selected = 0;
            }
            Tab::Scales => {
                self.scale_query.push_str(&addition);
                self.scale_selected = 0;
            }
            Tab::Samples => {
                self.sound_query.push_str(&addition);
                self.sound_selected = 0;
                self.refresh_sounds();
            }
            #[cfg(feature = "vst")]
            Tab::Vst => self.vst.edit_query(|query| query.push_str(&addition)),
            Tab::Reference => {
                self.query.push_str(&addition);
                self.selected = 0;
                self.refresh(reference);
            }
            #[cfg(feature = "hydra")]
            Tab::Examples | Tab::Generator => unreachable!("wants_text said no"),
        }
        true
    }

    /// Empty the search box in one gesture.
    ///
    /// Backspace is a key per character, and every one of them re-ranks:
    /// a box that somehow filled up with a long string was, in practice,
    /// only emptied by quitting the studio.
    pub fn clear_query(&mut self, reference: &Reference) -> bool {
        if !self.wants_text() {
            return false;
        }
        if self.query_len() == 0 {
            return self.bank_list() && self.show_all_banks(reference);
        }
        if self.bank_list() {
            self.query.clear();
            self.selected = 0;
            self.refresh(reference);
            return true;
        }
        match self.tab {
            Tab::Chords => {
                self.chord_query.clear();
                self.chord_selected = 0;
            }
            Tab::Scales => {
                self.scale_query.clear();
                self.scale_selected = 0;
            }
            Tab::Samples => {
                self.sound_query.clear();
                self.sound_selected = 0;
                self.refresh_sounds();
            }
            #[cfg(feature = "vst")]
            Tab::Vst => self.vst.edit_query(String::clear),
            Tab::Reference => {
                self.query.clear();
                self.selected = 0;
                self.refresh(reference);
            }
            #[cfg(feature = "hydra")]
            Tab::Examples | Tab::Generator => unreachable!("wants_text said no"),
        }
        true
    }

    /// How much is in the focused tab's search box.
    fn query_len(&self) -> usize {
        let query = if self.bank_list() {
            &self.query
        } else {
            match self.tab {
                Tab::Chords => &self.chord_query,
                Tab::Scales => &self.scale_query,
                Tab::Samples => &self.sound_query,
                #[cfg(feature = "vst")]
                Tab::Vst => self.vst.query(),
                Tab::Reference => &self.query,
                #[cfg(feature = "hydra")]
                Tab::Examples | Tab::Generator => return 0,
            }
        };
        query.chars().count()
    }

    pub(crate) fn empty_search(&self) -> bool {
        self.wants_text() && self.query_len() == 0
    }

    /// Put a letter in the search box, or say there is no box to put it in -
    /// in which case the letter is the score's, not this panel's.
    pub fn type_char(&mut self, reference: &Reference, character: char) -> bool {
        if !self.wants_text() {
            return false;
        }
        if self.query_len() >= MAX_QUERY_CHARS {
            // The box is full. Nothing in the studio is named this long,
            // so the keystroke has nothing left to narrow - and it is
            // still the panel's, not the score's.
            return true;
        }
        if self.bank_list() {
            self.query.push(character);
            self.selected = 0;
            self.refresh(reference);
            return true;
        }
        match self.tab {
            Tab::Chords => {
                self.chord_query.push(character);
                self.chord_selected = 0;
            }
            Tab::Scales => {
                self.scale_query.push(character);
                self.scale_selected = 0;
            }
            Tab::Samples => {
                self.sound_query.push(character);
                self.sound_selected = 0;
                self.refresh_sounds();
            }
            #[cfg(feature = "vst")]
            Tab::Vst => self.vst.edit_query(|query| query.push(character)),
            Tab::Reference => {
                self.query.push(character);
                self.selected = 0;
                self.refresh(reference);
            }
            #[cfg(feature = "hydra")]
            Tab::Examples | Tab::Generator => unreachable!("wants_text said no"),
        }
        true
    }

    pub fn backspace(&mut self, reference: &Reference) -> bool {
        if !self.wants_text() {
            return false;
        }
        if self.bank_list() {
            if self.query.pop().is_none() {
                self.show_all_banks(reference);
            }
            self.selected = 0;
            self.refresh(reference);
            return true;
        }
        match self.tab {
            Tab::Chords => {
                self.chord_query.pop();
                self.chord_selected = 0;
            }
            Tab::Scales => {
                self.scale_query.pop();
                self.scale_selected = 0;
            }
            Tab::Samples => {
                self.sound_query.pop();
                self.sound_selected = 0;
                self.refresh_sounds();
            }
            #[cfg(feature = "vst")]
            Tab::Vst => self.vst.edit_query(|query| {
                query.pop();
            }),
            Tab::Reference => {
                self.query.pop();
                self.selected = 0;
                self.refresh(reference);
            }
            #[cfg(feature = "hydra")]
            Tab::Examples | Tab::Generator => unreachable!("wants_text said no"),
        }
        true
    }

    pub fn move_by(&mut self, delta: isize) {
        // The keys and the wheel both walk through here: the margin is theirs.
        self.hold_scroll = false;
        #[cfg(feature = "hydra")]
        if self.tab.is_snippets() {
            let rows = self.snippet_lines();
            let before = self.snippet_selected;
            let mut at = (before as isize + delta).clamp(0, rows.len() as isize - 1) as usize;
            let selectable =
                |at: usize| !matches!(rows[at], SnippetLine::Generator(row) if !row.selectable());
            if !selectable(at) {
                at = if delta < 0 {
                    (0..at).rev().find(|&i| selectable(i)).unwrap_or(before)
                } else {
                    (at + 1..rows.len())
                        .find(|&i| selectable(i))
                        .unwrap_or(before)
                };
            }
            self.snippet_selected = at;
            if self.tab == Tab::Examples && before != at {
                self.snippet_code_scroll.set(0);
            }
            return;
        }
        #[cfg(feature = "vst")]
        if self.tab == Tab::Vst {
            self.vst.move_by(delta);
            return;
        }
        if self.tab == Tab::Scales {
            let rows = self.scale_rows().len();
            if rows == 0 {
                return;
            }
            self.scale_selected =
                (self.scale_selected as isize + delta).clamp(0, rows as isize - 1) as usize;
            return;
        }
        if self.tab == Tab::Chords {
            let rows = self.chord_rows().len();
            if rows == 0 {
                return;
            }
            self.chord_selected =
                (self.chord_selected as isize + delta).clamp(0, rows as isize - 1) as usize;
            return;
        }
        if self.tab == Tab::Samples && !self.bank_list() {
            let rows = self.sound_rows().len();
            if rows == 0 {
                return;
            }
            self.sound_selected =
                (self.sound_selected as isize + delta).clamp(0, rows as isize - 1) as usize;
            return;
        }
        match &mut self.mode {
            ReferenceMode::Browse => {
                if self.rows.is_empty() {
                    return;
                }
                let last = self.rows.len() as isize - 1;
                let mut at = (self.selected as isize + delta).clamp(0, last);
                // Skip headings, reversing direction at either end of the
                // list. Every heading has an entry beneath it, so this
                // reaches an entry within two steps.
                let mut step = if delta < 0 { -1 } else { 1 };
                while matches!(self.rows.get(at as usize), Some(BrowseRow::Tag(_))) {
                    if at + step < 0 || at + step > last {
                        step = -step;
                    }
                    at += step;
                }
                self.selected = at as usize;
            }
            ReferenceMode::Entry { scroll, .. } => {
                *scroll = (i32::from(*scroll) + delta as i32).max(0) as u16;
            }
        }
    }

    /// → on the samples tab: list the selected bank's variants.
    pub fn expand(&mut self) -> bool {
        #[cfg(feature = "hydra")]
        if self.tab == Tab::Generator {
            if let Some(super::super::ideas::Row::Direction(direction)) = self.generator_row()
                && direction == self.generator.direction()
            {
                let changed = !self.generator.open;
                self.generator.open = true;
                return changed;
            }
            return false;
        }
        if self.bank_list() {
            return false;
        }
        #[cfg(feature = "vst")]
        if self.tab == Tab::Vst {
            return self.vst.expand();
        }
        #[cfg(feature = "hydra")]
        if self.tab.is_snippets() {
            return match self.snippet_lines().get(self.snippet_selected).copied() {
                Some(SnippetLine::Section(section)) => self.section_open.insert(section),
                Some(SnippetLine::Shelf(section, shelf)) => {
                    self.snippet_open.insert((section, shelf))
                }
                _ => false,
            };
        }
        // → walks into an entry the way it walks into a bank - and without
        // asking for the second press Enter needs. It reads in `quick` mode
        // too, where Enter replaces the caret's word: that is the point, the
        // entry can be read before the word is replaced.
        if self.tab == Tab::Reference {
            // A vocabulary row is a word, not an entry: there is no page
            // behind it to walk into.
            if self.vocabulary.is_some() {
                return false;
            }
            let ReferenceMode::Browse = self.mode else {
                return false;
            };
            let Some(index) = self.selected_result() else {
                return false;
            };
            self.mode = ReferenceMode::Entry {
                index,
                scroll: 0,
                from_browse: true,
            };
            return true;
        }
        if self.tab == Tab::Scales {
            return match self.scale_rows().get(self.scale_selected).copied() {
                Some(ScaleRow::Scale(index)) => {
                    let open = self
                        .scale_names()
                        .get(index)
                        .is_some_and(|name| self.open_scale.as_deref() == Some(name.as_str()));
                    if open {
                        false
                    } else {
                        self.toggle_scale(index)
                    }
                }
                // A tonic is a scale to hear, not a row to open.
                Some(ScaleRow::Tonic(..)) | None => false,
            };
        }
        if self.tab == Tab::Chords {
            return match self.chord_rows().get(self.chord_selected).copied() {
                Some(ChordRow::Quality(index)) => {
                    if self.open_quality.as_deref()
                        == self.chord_qualities().get(index).map(|q| q.symbol)
                    {
                        false
                    } else {
                        self.toggle_quality(index)
                    }
                }
                // On a chord there is nothing to open: → plays it, the way
                // it auditions a sample.
                Some(ChordRow::Chord(..)) | None => false,
            };
        }
        if self.tab != Tab::Samples {
            return false;
        }
        let row = self.sound_rows().get(self.sound_selected).copied();
        self.keeping_place(|panel| match row {
            // A kind of sound opens onto its banks.
            Some(SoundRow::Category(position)) => {
                let Some((category, _)) = panel.visible_categories().get(position).cloned() else {
                    return false;
                };
                if panel.open_categories.contains(&category) {
                    return false;
                }
                panel.toggle_category(position)
            }
            Some(SoundRow::Family(family)) => {
                let Some(family) = panel.sound_families().get(family).cloned() else {
                    return false;
                };
                panel.open_family = Some(family.key.clone());
                true
            }
            Some(SoundRow::Bank(index)) if panel.bank_opens(index) => {
                panel.expanded = Some(index);
                true
            }
            _ => false,
        })
    }

    /// ← folds what → opened: the variants of a bank, or an open entry,
    /// staying where the reader was.
    pub fn collapse(&mut self) -> bool {
        #[cfg(feature = "hydra")]
        if self.tab == Tab::Generator {
            self.generator.open = false;
            self.snippet_selected = super::super::ideas::Direction::ALL
                .iter()
                .position(|d| *d == self.generator.direction())
                .unwrap_or(0);
            return true;
        }
        if self.bank_list() {
            return false;
        }
        #[cfg(feature = "vst")]
        if self.tab == Tab::Vst {
            return self.vst.collapse();
        }
        #[cfg(feature = "hydra")]
        if self.tab.is_snippets() {
            // Fold the nearest open shelf or section and select its
            // heading, so another left arrow can fold the parent.
            let lines = self.snippet_lines();
            let Some(&line) = lines.get(self.snippet_selected) else {
                return false;
            };
            let heading = match line {
                SnippetLine::Snippet(section, shelf, _) => Some(SnippetLine::Shelf(section, shelf)),
                SnippetLine::Shelf(section, shelf)
                    if self.snippet_open.contains(&(section, shelf)) =>
                {
                    Some(line)
                }
                SnippetLine::Shelf(section, _) => Some(SnippetLine::Section(section)),
                SnippetLine::Section(_) => Some(line),
                SnippetLine::Generator(_) => None,
            };
            let folded = match heading {
                Some(SnippetLine::Shelf(section, shelf)) => {
                    self.snippet_open.remove(&(section, shelf))
                }
                Some(SnippetLine::Section(section)) => self.section_open.remove(&section),
                _ => false,
            };
            if let Some(heading) = heading
                && let Some(row) = self
                    .snippet_lines()
                    .iter()
                    .position(|line| *line == heading)
            {
                self.snippet_selected = row;
                return true;
            }
            return folded;
        }
        // ← steps out of an entry to the list, whichever way the entry was
        // opened; unlike Esc it never closes, so it can be leaned on.
        if self.tab == Tab::Reference {
            let ReferenceMode::Entry { .. } = self.mode else {
                return false;
            };
            self.mode = ReferenceMode::Browse;
            return true;
        }
        if self.tab == Tab::Scales {
            return match self.scale_rows().get(self.scale_selected).copied() {
                Some(ScaleRow::Scale(index)) => {
                    let open = self
                        .scale_names()
                        .get(index)
                        .is_some_and(|name| self.open_scale.as_deref() == Some(name.as_str()));
                    open && self.toggle_scale(index)
                }
                Some(ScaleRow::Tonic(index, _)) => self.toggle_scale(index),
                None => false,
            };
        }
        if self.tab == Tab::Chords {
            return match self.chord_rows().get(self.chord_selected).copied() {
                Some(ChordRow::Quality(index)) => {
                    let open = self.chord_qualities().get(index).is_some_and(|quality| {
                        self.open_quality.as_deref() == Some(quality.symbol)
                    });
                    open && self.toggle_quality(index)
                }
                // From inside a quality, ← folds it and leaves the reader
                // on its row.
                Some(ChordRow::Chord(index, _)) => self.toggle_quality(index),
                None => false,
            };
        }
        if self.tab != Tab::Samples {
            return false;
        }
        // ← folds what the reader stands in, innermost first: the
        // samples of a bank, then the machine, then the kind - each time
        // leaving them on the row that now holds what they folded.
        let row = self.sound_rows().get(self.sound_selected).copied();
        match row {
            Some(SoundRow::Category(position)) => {
                let Some((category, _)) = self.visible_categories().get(position).cloned() else {
                    return false;
                };
                self.keeping_place(|panel| panel.open_categories.remove(&category))
            }
            Some(SoundRow::Family(family)) => {
                let Some(family) = self.sound_families().get(family).cloned() else {
                    return false;
                };
                if self.open_family.as_deref() != Some(family.key.as_str()) {
                    return false;
                }
                self.expanded = None;
                self.open_family = None;
                self.reselect_family(&family.key);
                true
            }
            Some(SoundRow::Bank(index) | SoundRow::Variant(index, _)) => {
                if self.expanded.is_some() {
                    self.expanded = None;
                    if let Some(row) = self
                        .sound_rows()
                        .iter()
                        .position(|row| *row == SoundRow::Bank(index))
                    {
                        self.sound_selected = row;
                    }
                    return true;
                }
                // Nothing expanded: ← from inside a machine folds the
                // machine, and from a loose bank it folds the kind.
                let families = self.sound_families();
                if let Some(family) = Self::family_of(&families, index) {
                    let key = families[family].key.clone();
                    if self.open_family.as_deref() == Some(key.as_str()) {
                        self.open_family = None;
                        self.reselect_family(&key);
                        return true;
                    }
                }
                let Some(category) = self.sounds.get(index).map(SoundSection::of) else {
                    return false;
                };
                if !self.open_categories.remove(&category) {
                    return false;
                }
                if let Some(row) = self.row_of(&SelectedSound::Category(category)) {
                    self.sound_selected = row;
                } else {
                    self.sound_selected = self
                        .sound_selected
                        .min(self.sound_rows().len().saturating_sub(1));
                }
                true
            }
            None => false,
        }
    }

    /// Open a closed family or fold an open one, keeping the selection on
    /// its header.
    fn toggle_family(&mut self, family: usize) {
        let Some(family) = self.sound_families().get(family).cloned() else {
            return;
        };
        if self.open_family.as_deref() == Some(family.key.as_str()) {
            self.expanded = None;
            self.open_family = None;
        } else {
            self.open_family = Some(family.key.clone());
        }
        self.reselect_family(&family.key);
    }

    /// Put the selection back on a family's header row, or clamp it if the
    /// family is gone.
    fn reselect_family(&mut self, key: &str) {
        let families = self.sound_families();
        let rows = self.sound_rows();
        // By key rather than by label: two imports can each hold a family
        // called `KSHMR`, and folding one must not land the cursor on the
        // other's header.
        if let Some(row) = rows
            .iter()
            .position(|row| matches!(row, SoundRow::Family(family) if families[*family].key == key))
        {
            self.sound_selected = row;
        } else {
            self.sound_selected = self.sound_selected.min(rows.len().saturating_sub(1));
        }
    }

    /// The selected sound's name, for the clipboard - or, on the shelf, the
    /// whole snippet, which is the thing anyone browsing it actually wants.
    pub fn copy(&self) -> PanelAction {
        #[cfg(feature = "hydra")]
        if self.tab.is_snippets() {
            return self
                .selected_snippet_code()
                .map(|code| PanelAction::Copy(code.into_owned()))
                .unwrap_or(PanelAction::Nothing);
        }
        if self.bank_list() {
            return self
                .vocabulary
                .as_ref()
                .and_then(|vocabulary| {
                    self.selected_result()
                        .and_then(|index| vocabulary.names.get(index))
                })
                .map(|name| PanelAction::Copy(name.clone()))
                .unwrap_or(PanelAction::Nothing);
        }
        if self.tab != Tab::Samples {
            return PanelAction::Nothing;
        }
        self.sound_rows()
            .get(self.sound_selected)
            .and_then(|row| self.sound_of(*row, false))
            .map(PanelAction::Copy)
            .unwrap_or(PanelAction::Nothing)
    }

    /// Alt+O on the samples tab: the file behind the row under the cursor -
    /// a numbered sample's own, a bank's first. A kind or a family of sounds
    /// is not a file, and neither is a font or a synth.
    pub fn location(&self) -> PanelAction {
        if self.tab != Tab::Samples {
            return PanelAction::Nothing;
        }
        let (bank, variant) = match self.sound_rows().get(self.sound_selected) {
            Some(SoundRow::Bank(index)) => (*index, None),
            Some(SoundRow::Variant(index, variant)) => (*index, Some(*variant)),
            Some(SoundRow::Category(_) | SoundRow::Family(_)) | None => {
                return PanelAction::Nothing;
            }
        };
        let Some(entry) = self.sounds.get(bank) else {
            return PanelAction::Nothing;
        };
        entry
            .location
            .clone()
            .map(|url| PanelAction::Reveal {
                name: entry.name.clone(),
                variant,
                url,
            })
            .unwrap_or(PanelAction::Nothing)
    }

    pub fn preview(&self) -> PanelAction {
        if self.bank_list() {
            return PanelAction::Nothing;
        }
        // A snippet is a score, so hearing it is playing it: → and Space
        // put it under the set at the set's own tempo.
        #[cfg(feature = "hydra")]
        if self.tab.is_snippets() {
            return match (
                self.selected_snippet_kind(),
                self.selected_snippet_code().as_deref(),
            ) {
                (Some(super::super::examples::Kind::Music), Some(code)) => {
                    PanelAction::PreviewScore(code.to_owned())
                }
                _ => PanelAction::Nothing,
            };
        }
        // A word list of scales is a list of things to hear, not just to
        // read: → and Space play the one under the cursor.
        if let Some(vocabulary) = &self.vocabulary
            && vocabulary.subject == "scales"
        {
            return self
                .selected_result()
                .and_then(|index| vocabulary.names.get(index))
                .map(|name| PanelAction::PreviewScale(name.clone()))
                .unwrap_or(PanelAction::Nothing);
        }
        // And a tuning even more so: its name says nothing at all, so
        // hearing it is the only way to choose one.
        if let Some(vocabulary) = &self.vocabulary
            && vocabulary.subject == "tunings"
        {
            return self
                .selected_result()
                .and_then(|index| vocabulary.names.get(index))
                .map(|name| PanelAction::PreviewTuning(name.clone()))
                .unwrap_or(PanelAction::Nothing);
        }
        if self.tab == Tab::Scales {
            return match self.selected_scale() {
                Some(scale) => PanelAction::PreviewScale(scale),
                None => PanelAction::Nothing,
            };
        }
        if self.tab == Tab::Chords {
            return match self.selected_chord() {
                Some(chord) => PanelAction::PreviewChord(chord),
                None => PanelAction::Nothing,
            };
        }
        if self.tab != Tab::Samples {
            return PanelAction::Nothing;
        }
        self.sound_rows()
            .get(self.sound_selected)
            .and_then(|row| self.sound_of(*row, true))
            .map(PanelAction::Preview)
            .unwrap_or(PanelAction::Nothing)
    }

    /// Enter: open the selected entry from the list, or insert the open
    /// entry's name; on the samples tab, insert the selected sound.
    pub fn confirm(&mut self, reference: &Reference) -> PanelAction {
        #[cfg(feature = "hydra")]
        if self.tab == Tab::Generator {
            return self.generator_activate();
        }
        #[cfg(feature = "hydra")]
        if self.tab.is_snippets() {
            // Enter toggles a heading or copies a snippet for the reader
            // to paste where needed.
            return match self.snippet_lines().get(self.snippet_selected).copied() {
                Some(SnippetLine::Section(section)) => {
                    self.toggle_section(section);
                    PanelAction::Nothing
                }
                Some(SnippetLine::Shelf(section, shelf)) => {
                    self.toggle_shelf(section, shelf);
                    PanelAction::Nothing
                }
                Some(SnippetLine::Snippet(..)) => self
                    .selected_snippet_code()
                    .map(|code| PanelAction::Copy(code.into_owned()))
                    .unwrap_or(PanelAction::Nothing),
                Some(SnippetLine::Generator(_)) | None => PanelAction::Nothing,
            };
        }
        #[cfg(feature = "vst")]
        if self.tab == Tab::Vst {
            // Enter writes the row into the score: the plugin call, a
            // parameter, or a preset. A row with no text opens: a group, or
            // a plugin before its load, which has no kind yet.
            let text = self.vst.score_text();
            match self.vst.row(self.vst.selected) {
                Some(PluginRow::Group(index)) => self.vst.toggle_group(index),
                Some(PluginRow::Plugin(_)) if text.is_none() => {
                    self.vst.expand();
                }
                _ => {}
            }
            return text.map_or(PanelAction::Nothing, PanelAction::Insert);
        }
        if self.tab == Tab::Scales {
            return match self.scale_rows().get(self.scale_selected).copied() {
                Some(ScaleRow::Scale(index)) => {
                    self.toggle_scale(index);
                    PanelAction::Nothing
                }
                Some(ScaleRow::Tonic(..)) => match (self.selected_scale(), self.intent) {
                    (Some(scale), PanelIntent::Insert) => PanelAction::Insert(scale),
                    (Some(scale), PanelIntent::Copy) => PanelAction::Copy(scale),
                    (None, _) => PanelAction::Nothing,
                },
                None => PanelAction::Nothing,
            };
        }
        if self.tab == Tab::Chords {
            return match self.chord_rows().get(self.chord_selected).copied() {
                Some(ChordRow::Quality(index)) => {
                    self.toggle_quality(index);
                    PanelAction::Nothing
                }
                Some(ChordRow::Chord(..)) => match (self.selected_chord(), self.intent) {
                    (Some(chord), PanelIntent::Insert) => PanelAction::Insert(chord),
                    (Some(chord), PanelIntent::Copy) => PanelAction::Copy(chord),
                    (None, _) => PanelAction::Nothing,
                },
                None => PanelAction::Nothing,
            };
        }
        if let Some(vocabulary) = &self.vocabulary {
            return self
                .selected_result()
                .and_then(|index| vocabulary.names.get(index))
                .map(|name| PanelAction::Insert(name.clone()))
                .unwrap_or(PanelAction::Nothing);
        }
        if self.tab == Tab::Samples {
            // Enter on a group opens and closes it, like a shelf; on a
            // sound it does what the panel was opened for: puts the name
            // in the score, or on the clipboard.
            match self.sound_rows().get(self.sound_selected).copied() {
                Some(SoundRow::Category(position)) => {
                    self.keeping_place(|panel| panel.toggle_category(position));
                    return PanelAction::Nothing;
                }
                Some(SoundRow::Family(family)) => {
                    self.keeping_place(|panel| panel.toggle_family(family));
                    return PanelAction::Nothing;
                }
                _ => {}
            }
            return self
                .sound_rows()
                .get(self.sound_selected)
                .and_then(|row| self.sound_of(*row, false))
                .map(|name| match self.intent {
                    PanelIntent::Insert => PanelAction::Insert(name),
                    PanelIntent::Copy => PanelAction::Copy(name),
                })
                .unwrap_or(PanelAction::Nothing);
        }
        // A snippet is for pasting: Enter on its row puts its lines in the
        // score at once, from the list as from its page. Reading it first
        // is what → is for.
        let snippet_under_cursor = match self.mode {
            ReferenceMode::Browse => self
                .selected_result()
                .and_then(|index| reference.entry(index)),
            ReferenceMode::Entry { index, .. } => reference.entry(index),
        }
        .and_then(|entry| entry.snippet.clone());
        if let Some(code) = snippet_under_cursor {
            return PanelAction::Paste(code);
        }
        match self.mode.clone() {
            ReferenceMode::Browse if self.quick => self
                .selected_result()
                .and_then(|index| reference.entry(index))
                .map(|entry| PanelAction::Insert(entry.name.clone()))
                .unwrap_or(PanelAction::Nothing),
            ReferenceMode::Browse => match self.selected_result() {
                Some(index) => {
                    self.mode = ReferenceMode::Entry {
                        index,
                        scroll: 0,
                        from_browse: true,
                    };
                    PanelAction::Nothing
                }
                None => PanelAction::Nothing,
            },
            ReferenceMode::Entry { index, .. } => reference
                .entry(index)
                .map(|entry| PanelAction::Insert(entry.name.clone()))
                .unwrap_or(PanelAction::Nothing),
        }
    }

    /// Esc: back to the list an entry was opened from, or close.
    pub fn escape(&mut self) -> PanelAction {
        if self.tab == Tab::Chords || self.tab == Tab::Scales {
            if self.collapse() {
                return PanelAction::Nothing;
            }
            return PanelAction::Close;
        }
        // On the samples tab Esc leaves the browser altogether, however
        // deep in the tree the reader is: folding is what ← is for, and a
        // reader pressing Esc wants their score back.
        if self.tab == Tab::Samples && !self.bank_list() {
            return PanelAction::Close;
        }
        // The same on the vst tab: ← folds a plugin, Esc leaves.
        #[cfg(feature = "vst")]
        if self.tab == Tab::Vst {
            return PanelAction::Close;
        }
        match self.mode {
            ReferenceMode::Entry {
                from_browse: true, ..
            } => {
                self.mode = ReferenceMode::Browse;
                PanelAction::Nothing
            }
            _ => PanelAction::Close,
        }
    }

    /// A click on the list: open the reference entry, or on the samples
    /// tab select the row, expanding a bank or previewing a variant.
    pub fn click(&mut self, reference: &Reference, geometry: PanelGeometry, y: u16) -> PanelAction {
        self.hold_scroll = true;
        let before = self.scroll_signature();
        let action = self.click_row(reference, geometry, y);
        // Opening one tree folds another: a scale, a chord quality, a bank or
        // a family open above the one clicked shuts, and every row above the
        // click moves up, the selection with them. The window moves as well,
        // so the row clicked is still on the screen row it was clicked on. A
        // click that changed nothing - a heading, the blank under a short
        // list - leaves the window alone.
        if let Some(row) = y
            .checked_sub(geometry.list.y)
            .filter(|row| *row < geometry.list.height)
            && self.scroll_signature() != before
        {
            let (selected, scroll) = self.selection_and_scroll();
            scroll.set(selected.saturating_sub(usize::from(row)));
        }
        action
    }

    fn click_row(&mut self, reference: &Reference, geometry: PanelGeometry, y: u16) -> PanelAction {
        #[cfg(feature = "hydra")]
        if self.tab.is_snippets() && y >= geometry.list.bottom() {
            return self.copy();
        }
        let Some(row) = y.checked_sub(geometry.list.y) else {
            return PanelAction::Nothing;
        };
        if row >= geometry.list.height {
            return PanelAction::Nothing;
        }
        let index = geometry.first_row + usize::from(row);
        #[cfg(feature = "hydra")]
        if self.tab.is_snippets() {
            // A click selects and, on a snippet, plays it - the way a
            // click plays a sound, a chord or a scale. Inserting a whole
            // chain into the score is still Enter's job and wants to be
            // deliberate. On a heading the click is the fold: open and
            // close by mouse, the way the sample banks already do.
            let lines = self.snippet_lines();
            if let Some(line) = lines.get(index).copied() {
                if matches!(line, SnippetLine::Generator(row) if !row.selectable()) {
                    return PanelAction::Nothing;
                }
                if self.tab == Tab::Examples && self.snippet_selected != index {
                    self.snippet_code_scroll.set(0);
                }
                self.snippet_selected = index;
                match line {
                    SnippetLine::Section(section) => self.toggle_section(section),
                    SnippetLine::Shelf(section, shelf) => self.toggle_shelf(section, shelf),
                    SnippetLine::Snippet(..) => return self.preview(),
                    SnippetLine::Generator(super::super::ideas::Row::Control(_)) => {
                        return PanelAction::Nothing;
                    }
                    SnippetLine::Generator(_) => return self.generator_activate(),
                }
            }
            return PanelAction::Nothing;
        }
        #[cfg(feature = "vst")]
        if self.tab == Tab::Vst {
            // A click selects the row. On a plugin or a group the click is
            // the fold also, as on the sample banks.
            let Some(clicked) = self.vst.row(index) else {
                return PanelAction::Nothing;
            };
            self.vst.selected = index;
            if matches!(clicked, PluginRow::Plugin(_) | PluginRow::Group(_)) && !self.vst.expand() {
                self.vst.collapse();
            }
            return PanelAction::Nothing;
        }
        if self.tab == Tab::Scales {
            let rows = self.scale_rows();
            let Some(clicked) = rows.get(index).copied() else {
                return PanelAction::Nothing;
            };
            self.scale_selected = index;
            return match clicked {
                ScaleRow::Scale(scale) => {
                    self.toggle_scale(scale);
                    PanelAction::Nothing
                }
                ScaleRow::Tonic(..) => self.preview(),
            };
        }
        if self.tab == Tab::Chords {
            let rows = self.chord_rows();
            let Some(clicked) = rows.get(index).copied() else {
                return PanelAction::Nothing;
            };
            self.chord_selected = index;
            return match clicked {
                ChordRow::Quality(quality) => {
                    self.toggle_quality(quality);
                    PanelAction::Nothing
                }
                ChordRow::Chord(..) => self.preview(),
            };
        }
        if self.tab == Tab::Samples {
            let rows = self.sound_rows();
            let Some(clicked) = rows.get(index).copied() else {
                return PanelAction::Nothing;
            };
            self.sound_selected = index;
            return match clicked {
                SoundRow::Category(position) => {
                    self.keeping_place(|panel| panel.toggle_category(position));
                    PanelAction::Nothing
                }
                SoundRow::Family(family) => {
                    self.toggle_family(family);
                    PanelAction::Nothing
                }
                SoundRow::Bank(bank) if self.expanded == Some(bank) => {
                    self.keeping_place(|panel| panel.expanded = None);
                    PanelAction::Nothing
                }
                SoundRow::Bank(bank) => {
                    self.keeping_place(|panel| panel.expanded = Some(bank));
                    self.preview()
                }
                SoundRow::Variant(..) => self.preview(),
            };
        }
        if !matches!(self.mode, ReferenceMode::Browse) {
            return PanelAction::Nothing;
        }
        match self.rows.get(index) {
            // A heading is a label: there is nothing behind it to open.
            Some(BrowseRow::Tag(_)) | None => PanelAction::Nothing,
            Some(BrowseRow::Entry(_)) => {
                self.selected = index;
                self.confirm(reference)
            }
        }
    }

    /// A right click on the samples tab: select the row and put the sound
    /// on the clipboard, without previewing or opening it.
    pub fn right_click(&mut self, geometry: PanelGeometry, y: u16) -> PanelAction {
        if self.tab != Tab::Samples || self.bank_list() {
            return PanelAction::Nothing;
        }
        self.hold_scroll = true;
        let Some(row) = y.checked_sub(geometry.list.y) else {
            return PanelAction::Nothing;
        };
        let index = geometry.first_row + usize::from(row);
        if row >= geometry.list.height || index >= self.sound_rows().len() {
            return PanelAction::Nothing;
        }
        self.sound_selected = index;
        self.copy()
    }
}
