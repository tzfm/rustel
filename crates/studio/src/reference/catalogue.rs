//! Documentation ingestion, canonical names, and ranked search.

use super::*;

impl From<&rustel_core::reference::ReferenceEntry> for Entry {
    fn from(entry: &rustel_core::reference::ReferenceEntry) -> Self {
        Self {
            name: entry.name.to_owned(),
            synonyms: entry
                .synonyms
                .iter()
                .map(|name| (*name).to_owned())
                .collect(),
            summary: entry.summary.to_owned(),
            description: entry.description.to_owned(),
            params: entry
                .params
                .iter()
                .map(|param| Param {
                    name: param.name.to_owned(),
                    r#type: param.r#type.to_owned(),
                    description: param.description.to_owned(),
                    choices: rustel_core::reference::reference_choices(entry.name, param.name)
                        .map(|set| {
                            set.choices
                                .iter()
                                .map(|choice| Choice {
                                    value: choice.value.to_owned(),
                                    description: choice.description.to_owned(),
                                })
                                .collect()
                        })
                        .unwrap_or_default(),
                })
                .collect(),
            examples: entry
                .examples
                .iter()
                .map(|example| (*example).to_owned())
                .collect(),
            tags: entry.tags.iter().map(|tag| (*tag).to_owned()).collect(),
            no_autocomplete: entry.no_autocomplete,
            deprecated: entry.deprecated,
            source: String::new(),
            origin: entry.origin.to_owned(),
            snippet: None,
            keywords: Vec::new(),
        }
    }
}

impl Entry {
    /// Whether insertion should add call parentheses. Non-callable globals,
    /// sound names and snippets do not receive them.
    pub fn inserts_as_call(&self) -> bool {
        self.snippet.is_none()
            && !self.tags.iter().any(|tag| tag == "sound")
            && !rustel_runtime::lint::global_values().contains(self.name.as_str())
    }

    /// The signature line: `lpf(frequency)` plus its other names.
    pub fn signature(&self) -> String {
        // A snippet is lines, not a call: its name is its heading.
        if self.snippet.is_some() {
            return self.name.clone();
        }
        let params = self
            .params
            .iter()
            .map(|param| param.name.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        format!("{}({params})", self.name)
    }

    /// The one tag a list with headings files this entry under.
    ///
    /// An entry carries one to four tags - a sound is `audio` and
    /// `samples` both, a wavetable envelope is three deep - so a grouped
    /// list has to pick one, the way a bank belongs to the kind of its
    /// first bank rather than to every kind it could be read as. The first
    /// tag is the word whoever documented the name reached for first, and
    /// it is the one that stays put: a rule that counted tags would move an
    /// entry's heading the day an unrelated entry was added somewhere else.
    pub fn heading(&self) -> &str {
        self.tags.first().map(String::as_str).unwrap_or("other")
    }
}

/// Present a multiword origin in title case.
///
/// The stored form stays lower case because it is the grouping key: the
/// reference lists extensions under the name of whoever wrote them, and two
/// spellings of one origin would split the group in two.
pub(super) fn display_origin(origin: &str) -> String {
    // Preserve intentionally stylized handles containing underscores.
    if origin.contains('_') {
        return origin.to_owned();
    }
    origin
        .split(' ')
        .map(|word| {
            let mut chars = word.chars();
            match chars.next() {
                Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Map lowercase spellings to the engine's camelCase names: `setcpm` →
/// `setCpm`. The list displays the canonical spelling and keeps the
/// lowercase spelling as an alias.
pub(super) fn canonical_spellings() -> &'static HashMap<String, String> {
    use std::sync::OnceLock;
    static SPELLINGS: OnceLock<HashMap<String, String>> = OnceLock::new();
    SPELLINGS.get_or_init(|| {
        let mut camel: Vec<&String> = rustel_runtime::lint::known_names()
            .iter()
            .filter(|name| {
                name.starts_with(|c: char| c.is_ascii_lowercase())
                    && name.chars().any(|c| c.is_uppercase())
            })
            .collect();
        camel.sort();
        let mut spellings = HashMap::new();
        for name in camel {
            spellings
                .entry(name.to_lowercase())
                .or_insert_with(|| name.clone());
        }
        spellings
    })
}

/// Rename an entry documented under a lowercase alias to its camelCase
/// engine spelling, demoting the documented name to a synonym so it still
/// resolves and still shows on the entry page.
fn promote_spelling(entry: &mut Entry) {
    let Some(camel) = canonical_spellings().get(&entry.name) else {
        return;
    };
    if *camel == entry.name {
        return;
    }
    let lowercase = std::mem::replace(&mut entry.name, camel.clone());
    entry.synonyms.retain(|synonym| *synonym != entry.name);
    if !entry.synonyms.contains(&lowercase) {
        entry.synonyms.insert(0, lowercase);
    }
}

impl Reference {
    /// Assemble the reference from the code that installs each name,
    /// keeping the entries `known` accepts under any of their names. The
    /// revision records the upstream pin the ported prose came from.
    pub fn load(known: impl Fn(&str) -> bool) -> Self {
        let mut reference = Self {
            revision: UPSTREAM_DOC_REVISION.to_owned(),
            ..Self::default()
        };
        reference.ingest_all_natives(&known);
        reference.ingest_snippets();
        reference
    }

    /// The lines worth pasting rather than remembering, in the same list
    /// as the functions: found by the same search, labelled as what they
    /// are, and pasted rather than inserted. They name no installed
    /// function, so the `known` filter has nothing to say about them.
    fn ingest_snippets(&mut self) {
        for snippet in super::super::snippets::SNIPPETS {
            let entry = Entry {
                name: snippet.name.to_owned(),
                synonyms: Vec::new(),
                summary: snippet.summary.to_owned(),
                description: snippet.description.to_owned(),
                params: Vec::new(),
                examples: vec![snippet.code.to_owned()],
                tags: std::iter::once("snippet")
                    .chain(snippet.tags.iter().copied())
                    .map(str::to_owned)
                    .collect(),
                no_autocomplete: false,
                deprecated: false,
                source: String::new(),
                origin: String::new(),
                snippet: Some(snippet.code.to_owned()),
                keywords: snippet
                    .keywords
                    .iter()
                    .map(|word| (*word).to_owned())
                    .collect(),
            };
            self.ingest_entry(entry, &|_: &str| true, false);
        }
    }

    /// Every entry, in installation order.
    ///
    /// Shared spellings resolve to the first entry ingested: host functions,
    /// extensions, controls, then combinators. In particular, `density` and
    /// `ds` resolve to controls even though combinators also use those names.
    fn ingest_all_natives(&mut self, known: &impl Fn(&str) -> bool) {
        #[cfg(feature = "extensions")]
        let extension_entries = rustel_ext::reference_entries();
        #[cfg(not(feature = "extensions"))]
        let extension_entries =
            std::iter::empty::<&'static rustel_core::reference::ReferenceEntry>();
        let control_entries = rustel_core::controls_generated::visible_reference_entries();
        for documented in rustel_jsruntime::reference_entries() {
            self.ingest_native(Entry::from(documented), known, true);
        }
        // Keep extension credits for grouping; other tables' origins may
        // identify documentation authors rather than extension authors.
        for documented in extension_entries {
            self.ingest_extension(Entry::from(documented), known);
        }
        for documented in control_entries {
            self.ingest_native(Entry::from(documented), known, true);
        }
        let combinator_registry = rustel_core::register::default_registry();
        for documented in combinator_registry.visible_reference_entries() {
            self.ingest_native(Entry::from(documented), known, true);
        }
        // The painters' entries live beside the renderer that owns them.
        for documented in super::super::visuals::REFERENCE_ENTRIES {
            let entry = Entry::from(documented);
            self.ingest_native(entry.clone(), known, true);
            self.ingest_inline_painter(entry, known);
        }
        // The native synth sounds' entries live where the sounds resolve.
        for documented in rustel_voice::SOUND_REFERENCE {
            self.ingest_native(Entry::from(documented), known, false);
        }
        // The chord and scale catalogues, documented where the dictionaries
        // live.
        for documented in rustel_core::voicings::CHORD_REFERENCE {
            self.ingest_native(Entry::from(documented), known, false);
        }
        for documented in rustel_core::tonaljs_scales::SCALE_REFERENCE {
            self.ingest_native(Entry::from(documented), known, false);
        }
    }

    /// Ingest a non-extension entry accepted by `known`, keeping the first
    /// entry for each name and clearing its extension-group credit.
    ///
    /// `promote` respells a lowercase entry name to the engine's camelCase
    /// canonical. The chord/scale/sound catalogues use exact symbols, so they
    /// pass `false` to preserve the catalogue's spelling.
    fn ingest_native(&mut self, mut entry: Entry, known: &impl Fn(&str) -> bool, promote: bool) {
        entry.origin.clear();
        self.ingest_entry(entry, known, promote);
    }

    /// Inline painters share their renderer's documentation, but are separate
    /// calls: completing `_piano` must retain the underscore and its placement.
    /// Only derive spellings the transpiler actually recognizes as widgets.
    fn ingest_inline_painter(&mut self, mut entry: Entry, known: &impl Fn(&str) -> bool) {
        let plain = entry.name.clone();
        let inline = format!("_{plain}");
        if !rustel_transpiler::VISUAL_WIDGET_METHODS.contains(&inline.as_str()) {
            return;
        }
        entry.name = inline;
        let spellings =
            std::iter::once(plain.as_str()).chain(entry.synonyms.iter().map(String::as_str));
        for spelling in spellings {
            for example in &mut entry.examples {
                *example = example.replace(&format!(".{spelling}("), &format!("._{spelling}("));
            }
        }
        entry.synonyms = entry
            .synonyms
            .iter()
            .map(|name| format!("_{name}"))
            .filter(|name| rustel_transpiler::VISUAL_WIDGET_METHODS.contains(&name.as_str()))
            .collect();
        entry.summary = format!("Inline {plain} visualization below the call.");
        entry.description = format!(
            "Displays `{plain}` inline below this call. It uses the same options as `{plain}`.\n\n{}",
            entry.description
        );
        self.ingest_native(entry, known, true);
    }

    /// One entry from an extension: the same ingest, keeping the credit.
    fn ingest_extension(&mut self, entry: Entry, known: &impl Fn(&str) -> bool) {
        self.ingest_entry(entry, known, true);
    }

    fn ingest_entry(&mut self, mut entry: Entry, known: &impl Fn(&str) -> bool, promote: bool) {
        let usable = known(&entry.name) || entry.synonyms.iter().any(|name| known(name));
        if !usable {
            self.omitted += 1;
            return;
        }
        if promote {
            promote_spelling(&mut entry);
        }
        if let Some(&shadowing) = self.by_name.get(&entry.name) {
            for name in &entry.synonyms {
                self.by_name.entry(name.clone()).or_insert(shadowing);
            }
            return;
        }
        let index = self.entries.len();
        for name in std::iter::once(&entry.name).chain(&entry.synonyms) {
            self.by_name.entry(name.clone()).or_insert(index);
        }
        self.entries.push(entry);
    }

    /// The reference with `entry` added, for tests.
    #[cfg(test)]
    pub(crate) fn with_entry(mut self, entry: Entry) -> Self {
        self.ingest_entry(entry, &|_: &str| true, false);
        self
    }

    /// Everything documented, whether or not this engine plays it.
    pub fn load_all() -> Self {
        Self::load(|_| true)
    }

    /// The origins present, as a reader sees them, in stable order. The
    /// reference groups extensions by who wrote them; there will be more than
    /// one.
    pub fn origins(&self) -> Vec<String> {
        let mut origins: Vec<String> = self
            .entries
            .iter()
            .filter(|entry| !entry.origin.is_empty())
            .map(|entry| display_origin(&entry.origin))
            .collect();
        origins.sort();
        origins.dedup();
        origins
    }

    /// Every documented name from one origin, in reference order.
    pub fn from_origin(&self, origin: &str) -> Vec<&str> {
        // An empty origin marks non-extension entries, not an extension group.
        if origin.is_empty() {
            return Vec::new();
        }
        self.entries
            .iter()
            .filter(|entry| display_origin(&entry.origin) == origin)
            .map(|entry| entry.name.as_str())
            .collect()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Hide these categories from the browse list and the suggestions.
    /// Returns true when the hidden set changed.
    pub fn set_hidden(&mut self, hidden: impl IntoIterator<Item = Category>) -> bool {
        let hidden: Vec<Category> = hidden.into_iter().collect();
        if self.hidden == hidden {
            return false;
        }
        self.hidden = hidden;
        true
    }

    /// How many entries the settings hide from an unfiltered list.
    pub fn hidden_len(&self) -> usize {
        self.entries
            .iter()
            .filter(|entry| !self.offers(entry, &TagFilter::default()))
            .count()
    }

    /// Whether a search lists `entry`. The entry is in no hidden category,
    /// or the query's `tag:` names the hidden category.
    fn offers(&self, entry: &Entry, filter: &TagFilter) -> bool {
        self.hidden.iter().all(|category| {
            !category.files(entry)
                || filter.name.is_some_and(|name| {
                    category
                        .tag()
                        .starts_with(name.to_ascii_lowercase().as_str())
                })
        })
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn omitted(&self) -> usize {
        self.omitted
    }

    pub fn revision(&self) -> &str {
        &self.revision
    }

    pub fn entry(&self, index: usize) -> Option<&Entry> {
        self.entries.get(index)
    }

    /// The entry a word names, by name or synonym. Exact first, then
    /// case-insensitive, so `LPF` still finds `lpf`. Inline painters keep
    /// their underscore, which determines where the visualization appears.
    pub fn lookup(&self, word: &str) -> Option<usize> {
        let word = word.trim_matches(|c: char| !c.is_alphanumeric() && c != '_' && c != '$');
        if word.is_empty() {
            return None;
        }
        self.lookup_spelling(word)
    }

    fn lookup_spelling(&self, word: &str) -> Option<usize> {
        self.by_name.get(word).copied().or_else(|| {
            let lowered = word.to_lowercase();
            self.by_name
                .iter()
                .find(|(name, _)| name.to_lowercase() == lowered)
                .map(|(_, index)| *index)
        })
    }

    /// Every tag the reference actually uses, most-used first.
    ///
    /// Read off the entries rather than kept as a list, so a tag that
    /// arrives with a native addition needs nothing declared here.
    pub fn tags(&self) -> Vec<(&str, usize)> {
        let mut counts: HashMap<&str, usize> = HashMap::new();
        for tag in self.entries.iter().flat_map(|entry| entry.tags.iter()) {
            *counts.entry(tag.as_str()).or_default() += 1;
        }
        let mut tags = counts.into_iter().collect::<Vec<_>>();
        tags.sort_by(|left, right| right.1.cmp(&left.1).then_with(|| left.0.cmp(right.0)));
        tags
    }

    /// Whether an entry is documented under exactly this name or synonym.
    ///
    /// [`Reference::lookup`] trims spellings down to their letters, which is
    /// right for search and for forgiving a reader's typo, but it destroys
    /// the names that ARE the spelling - a chord symbol like `+`, `-` or
    /// `^7` - so the catalogues check against the exact word.
    pub fn documents(&self, name: &str) -> bool {
        self.by_name.contains_key(name)
    }

    /// The entry a name resolves to, exact spelling first.
    ///
    /// [`Reference::lookup`] trims a word down to its letters, which is right
    /// for forgiving a typo but destroys the names that ARE the spelling - a
    /// chord symbol like `+`, `-` or `^7` - so an exact hit in the name table
    /// wins before the trimmed lookup is tried.
    pub fn resolve(&self, name: &str) -> Option<usize> {
        self.by_name
            .get(name)
            .copied()
            .or_else(|| self.lookup(name))
    }

    /// Entries matching a query, best first: the exact name, then names
    /// that start with it, then names and synonyms that contain it, then
    /// summaries that mention it. An empty query lists everything,
    /// hidden-from-completion entries last. A [`Category`] the settings
    /// hide stays out unless a `tag:` word names the category.
    ///
    /// A `tag:` word narrows the field before any of that ranking happens -
    /// see [`TagFilter`]. Ranking a whole vocabulary cannot answer "show me
    /// every visualizer", because the answer is a *set*, not an order: the
    /// twelve wanted entries would sit wherever their names happened to
    /// score, among four hundred that were never asked for.
    pub fn search(&self, query: &str) -> Vec<usize> {
        self.search_with(query, false)
    }

    /// The same search, told whether a snippet could be used where the
    /// caret is standing.
    ///
    /// A snippet is a whole statement - `$: s("bd*4")` - so it only lands
    /// somewhere useful on a line of its own. Ranked by name alone they
    /// crowd the top of every search that shares a word with one, which is
    /// most of them: typing `midike` to reach `midikeys` offered three
    /// snippets first.
    pub fn search_with(&self, query: &str, snippets_last: bool) -> Vec<usize> {
        let filter = TagFilter::parse(query);
        let query = filter.rest;
        let mut ranked = self
            .entries
            .iter()
            .enumerate()
            .filter(|(_, entry)| filter.admits(entry) && self.offers(entry, &filter))
            // An underscore asks for an inline/underscored callable, not a
            // one-character typo for `H`, `i`, `n`, etc. Keep fuzzy matching
            // within that family so partial names and typos still work.
            .filter(|(_, entry)| {
                !query.starts_with('_')
                    || std::iter::once(&entry.name)
                        .chain(&entry.synonyms)
                        .any(|name| name.starts_with('_'))
            })
            .filter_map(|(index, entry)| {
                // A keyword ranks like another name - it is what a reader
                // calls the thing - without being shown as one.
                let aliases: Vec<String> = if entry.keywords.is_empty() {
                    entry.synonyms.clone()
                } else {
                    entry
                        .synonyms
                        .iter()
                        .chain(&entry.keywords)
                        .cloned()
                        .collect()
                };
                let score =
                    super::super::fuzzy::score(query, &entry.name, &aliases, &entry.summary)?;
                // Deprioritize these entries when browsing without a query.
                let score = if query.is_empty() && entry.no_autocomplete {
                    score - 30
                } else {
                    score
                };
                // Past every real match, not merely below the good ones:
                // a snippet you cannot paste where you are standing is
                // never the answer, however well its name matched.
                let score = if snippets_last && entry.tags.iter().any(|tag| tag == "snippet") {
                    score - SNIPPET_OFF_LINE_PENALTY
                } else {
                    score
                };
                Some((score, index))
            })
            .collect::<Vec<_>>();
        ranked.sort_by(|left, right| {
            right.0.cmp(&left.0).then_with(|| {
                // Break case-insensitive score ties with an exact name match:
                // `s` selects the sound control; `S` selects the unsupported call.
                let exact = |index: usize| self.entries[index].name == query;
                exact(right.1).cmp(&exact(left.1)).then_with(|| {
                    self.entries[left.1]
                        .name
                        .to_lowercase()
                        .cmp(&self.entries[right.1].name.to_lowercase())
                })
            })
        });
        ranked.into_iter().map(|(_, index)| index).collect()
    }
}

impl<'a> TagFilter<'a> {
    /// Use the first `tag:` word as a filter. A leading tag keeps the suffix
    /// as search text; a later tag keeps only the prefix.
    pub fn parse(query: &'a str) -> Self {
        let query = query.trim();
        let Some(word) = query.split_whitespace().find(|word| {
            word.get(.."tag:".len())
                .is_some_and(|prefix| prefix.eq_ignore_ascii_case("tag:"))
        }) else {
            return Self {
                name: None,
                rest: query,
            };
        };
        // Later tag words remain ordinary search text only when the first
        // tag leads the query.
        let at = word.as_ptr() as usize - query.as_ptr() as usize;
        let rest = if at == 0 {
            query[word.len()..].trim_start()
        } else {
            // Keep the prefix so `rest` can borrow one contiguous slice.
            // Text after a mid-query tag is omitted.
            query[..at].trim_end()
        };
        Self {
            name: Some(&word["tag:".len()..]),
            rest,
        }
    }

    /// Whether this entry survives the filter.
    pub fn admits(&self, entry: &Entry) -> bool {
        let Some(name) = self.name else {
            return true;
        };
        let name = name.to_ascii_lowercase();
        entry
            .tags
            .iter()
            .any(|tag| tag.to_ascii_lowercase().starts_with(&name))
    }

    /// How the count line names the filter, if there is one.
    pub fn label(&self) -> Option<String> {
        self.name.map(|name| format!("tag:{name}"))
    }
}
