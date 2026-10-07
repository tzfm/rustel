//! Sample sources, bank grouping, and sound selection.

use super::*;

/// What a family is remembered by: its section and its shared name.
///
/// The discriminant keeps an import whose spec reads like a category name
/// from sharing a key with that category.
fn family_key(entry: &SoundEntry, label: &str) -> String {
    match &entry.import {
        Some(import) => format!("i{import}\u{1}{label}"),
        None => format!("k{}\u{1}{label}", entry.category.label()),
    }
}

/// Whether one variant of an entry is a file in a local layer the player
/// owns: what Alt+R renames and Alt+D deletes.
fn owns_file(entry: &SoundEntry, variant: usize) -> bool {
    matches!(entry.origin, SoundOrigin::Global | SoundOrigin::Set)
        && entry
            .variant_names
            .get(variant)
            .is_some_and(|name| !name.is_empty())
}

impl SoundSection {
    pub(super) fn of(entry: &SoundEntry) -> Self {
        match &entry.import {
            Some(import) => Self::Import(import.clone()),
            None => Self::Kind(entry.category),
        }
    }

    pub fn label(&self) -> &str {
        match self {
            Self::Kind(category) => category.label(),
            Self::Import(import) => import,
        }
    }

    /// The spec as a header reads it, with no neighbours to be told apart
    /// from: a local folder by its own name, everything else as written.
    ///
    /// A `samples("github:user/repo")` import is already short and is what
    /// the score says, so it stays. A folder someone dropped on the window
    /// arrives as an absolute path, and a header cannot hold one: the column
    /// cuts it short. Three imports from one parent then all read the prefix
    /// they share, such as `/Users/name/Samples`. That is not a name.
    fn plain_label(&self) -> &str {
        match self {
            Self::Kind(category) => category.label(),
            Self::Import(import) => import_folder(import)
                .and_then(|folder| folder.file_name())
                .and_then(|name| name.to_str())
                .unwrap_or(import),
        }
    }
}

/// The folder an imported spec names, if it names one at all.
///
/// The same two spellings the sources page accepts: a bare absolute path,
/// and the `local:` prefix a score writes.
fn import_folder(spec: &str) -> Option<&std::path::Path> {
    let spec = spec.trim();
    let path = match spec.strip_prefix("local:") {
        Some(folder) => std::path::Path::new(folder.trim()),
        None => std::path::Path::new(spec),
    };
    path.is_absolute().then_some(path)
}

/// Short, distinct names for a list of imported source specs: the same
/// reading the browser gives its headings, for the page where the sources
/// are managed. A row there is a name to pick from, not an address.
pub fn source_labels(specs: &[String]) -> Vec<String> {
    let sections: Vec<SoundSection> = specs.iter().cloned().map(SoundSection::Import).collect();
    section_labels(&sections)
}

/// Header names for the sections on screen together, kept distinct.
///
/// A folder is its own name where that is enough. Where two imports end in
/// the same word - `Vol.1/kicks` and `Vol.2/kicks`, which is how sample
/// packs are actually laid out - the ones that clash take a parent each
/// until they differ, and no more than that. Growing only the clashing
/// rows keeps the common case one word.
pub fn section_labels(sections: &[SoundSection]) -> Vec<String> {
    // How many trailing components each label is showing.
    let mut depth = vec![1usize; sections.len()];
    let parts = |section: &SoundSection| -> Option<Vec<String>> {
        let SoundSection::Import(import) = section else {
            return None;
        };
        let folder = import_folder(import)?;
        let parts: Vec<String> = folder
            .components()
            .filter_map(|part| match part {
                std::path::Component::Normal(name) => Some(name.to_string_lossy().into_owned()),
                _ => None,
            })
            .collect();
        (!parts.is_empty()).then_some(parts)
    };
    let all: Vec<Option<Vec<String>>> = sections.iter().map(parts).collect();
    let render = |at: usize, depth: usize| -> String {
        match &all[at] {
            Some(parts) => {
                let from = parts.len().saturating_sub(depth);
                parts[from..].join("/")
            }
            None => sections[at].plain_label().to_owned(),
        }
    };
    // Grow the clashing rows a component at a time. Bounded by the longest
    // path, and every round either separates a pair or stops growing it.
    let longest = all
        .iter()
        .filter_map(|parts| parts.as_ref().map(Vec::len))
        .max()
        .unwrap_or(1);
    for _ in 1..longest.max(1) {
        let drawn: Vec<String> = (0..sections.len())
            .map(|at| render(at, depth[at]))
            .collect();
        let mut grew = false;
        for at in 0..sections.len() {
            let Some(parts) = &all[at] else { continue };
            if depth[at] >= parts.len() {
                continue;
            }
            if drawn.iter().enumerate().any(|(other, label)| {
                other != at && *label == drawn[at] && sections[other] != sections[at]
            }) {
                depth[at] += 1;
                grew = true;
            }
        }
        if !grew {
            break;
        }
    }
    let mut labels: Vec<String> = (0..sections.len())
        .map(|at| render(at, depth[at]))
        .collect();
    // Two specs can name one folder - `/packs/kicks` imported here and
    // `samples('local:/packs/kicks')` written in a score - and no amount
    // of parent will ever tell those apart. They are still two rows, so
    // they say the thing that does differ: what was written.
    let clashing: Vec<usize> = (0..labels.len())
        .filter(|at| {
            labels
                .iter()
                .enumerate()
                .any(|(other, label)| other != *at && *label == labels[*at])
        })
        .collect();
    for at in clashing {
        labels[at] = sections[at].label().to_owned();
    }
    labels
}

impl ReferencePanel {
    /// Whether this panel is a word list for a `.bank(…)` argument: its
    /// names are machines, refilled from the library when it changes.
    pub fn bank_vocabulary(&self) -> bool {
        self.vocabulary
            .as_ref()
            .is_some_and(|vocabulary| vocabulary.subject == "sample banks")
    }

    /// The sounds a bank list was opened for - what the bank call's
    /// receiver named then - kept so a library refresh ranks against the
    /// same question even after the caret moves. `None` for any other list.
    pub fn bank_sounds(&self) -> Option<&[String]> {
        self.vocabulary
            .as_ref()?
            .bank_compatibility
            .as_ref()
            .map(|bank| bank.sounds.as_slice())
    }

    /// Whether a bank list is showing only the machines that can play its
    /// sounds, before Backspace on an empty search opened the rest.
    pub fn compatible_banks_only(&self) -> bool {
        self.vocabulary
            .as_ref()
            .and_then(|vocabulary| vocabulary.bank_compatibility.as_ref())
            .is_some_and(|bank| bank.only_compatible)
    }

    pub(super) fn show_all_banks(&mut self, reference: &Reference) -> bool {
        let Some(bank) = self
            .vocabulary
            .as_mut()
            .and_then(|vocabulary| vocabulary.bank_compatibility.as_mut())
        else {
            return false;
        };
        if !bank.only_compatible {
            return false;
        }
        bank.only_compatible = false;
        self.selected = 0;
        self.refresh(reference);
        true
    }

    /// A `.bank(…)` completion belongs to Samples, but uses the compact
    /// vocabulary rows rather than the full sound tree.
    pub(super) fn bank_list(&self) -> bool {
        self.tab == Tab::Samples && self.bank_vocabulary()
    }

    /// Explain an empty bank list, including when no bank supports the
    /// connected sound pattern before the reader types a search.
    pub fn banks_miss_line(&self) -> Option<String> {
        if !self.results.is_empty() || !self.bank_vocabulary() {
            return None;
        }
        let query = self.query.trim();
        let subject = if self.compatible_banks_only() {
            "compatible banks"
        } else {
            "sample banks"
        };
        if query.is_empty() {
            self.compatible_banks_only()
                .then(|| format!("no {subject}"))
        } else {
            Some(format!("no {subject} named {query:?}"))
        }
    }

    /// The same honesty on the sounds search: `no sample banks named
    /// "kjkjkjsdf"` is a readable miss where `0 of 683 sounds` was a
    /// number to decode.
    pub fn sounds_miss_line(&self) -> Option<String> {
        let searching = !self.sound_query.trim().is_empty();
        (!self.sounds.is_empty() && searching && self.sound_results.is_empty())
            .then(|| format!("no sample banks named {:?}", self.sound_query.trim()))
    }

    /// Replace a bank list's names in place, compatible machines first -
    /// a `.bank(…)` completion whose library arrived or changed under the
    /// open panel. What the reader has typed keeps filtering, whether the
    /// rest were opened is kept, and the ranking is redone so a name that
    /// just appeared can surface. Returns whether anything changed.
    pub fn set_bank_names(
        &mut self,
        reference: &Reference,
        names: Vec<String>,
        compatible_count: usize,
    ) -> bool {
        let Some(vocabulary) = self.vocabulary.as_mut() else {
            return false;
        };
        let Some(bank) = vocabulary.bank_compatibility.as_mut() else {
            return false;
        };
        if vocabulary.names == names && bank.count == compatible_count {
            return false;
        }
        bank.count = compatible_count;
        vocabulary.names = names;
        vocabulary.details = vec![String::new(); vocabulary.names.len()];
        self.refresh(reference);
        true
    }

    /// The catalogue with no bank connected: no machine's sounds lead a
    /// search, and a chosen sound goes in under its full name.
    pub fn set_sounds(&mut self, sounds: Vec<SoundEntry>) -> bool {
        self.set_sounds_prefixed(sounds, Vec::new())
    }

    /// Remap the identities held by an upcoming catalogue refresh after a
    /// bank alias changes. The refresh then restores the same selected row,
    /// expanded bank, and open family under its new spelling.
    pub fn prepare_sound_rename(&mut self, from: &str, to: &str) {
        let Some(entry) = self.sounds.iter_mut().find(|entry| entry.name == from) else {
            return;
        };
        if let Some(open) = self.open_family.as_mut() {
            let old_label = entry.name.split('_').next().unwrap_or(&entry.name);
            if *open == family_key(entry, old_label) {
                let new_label = to.split('_').next().unwrap_or(to);
                *open = family_key(entry, new_label);
            }
        }
        entry.name = to.to_owned();
    }

    /// The catalogue, with the machines of the bank that applies where the
    /// panel was opened: every machine the bank names, when it names
    /// several.
    pub fn set_sounds_prefixed(
        &mut self,
        sounds: Vec<SoundEntry>,
        prefixed_by: Vec<String>,
    ) -> bool {
        if sounds == self.sounds && prefixed_by == self.sound_prefix {
            return false;
        }
        let selected = self.selected_sound();
        let expanded_name = self
            .expanded
            .and_then(|index| self.sounds.get(index))
            .map(|entry| entry.name.clone());
        self.sounds = sounds;
        self.sound_prefix = prefixed_by;
        self.expanded =
            expanded_name.and_then(|name| self.sounds.iter().position(|entry| entry.name == name));
        self.refresh_sounds();
        // A reload keeps the reader where they were, whatever kind of row
        // that is.
        if let Some(row) = selected.and_then(|selected| self.row_of(&selected)) {
            self.sound_selected = row;
        }
        true
    }

    /// Input channel changes don't require rebuilding the sample catalogue.
    pub fn set_input_channels(&mut self, channels: usize) -> bool {
        let Some(input) = self
            .sounds
            .iter_mut()
            .find(|entry| entry.origin == SoundOrigin::Input)
        else {
            return false;
        };
        let channels = channels.max(1);
        if input.variants == channels {
            return false;
        }
        input.variants = channels;
        self.refresh_sounds();
        true
    }

    /// The score's imports and where they stand, for the samples tab to
    /// list the ones the library has nothing of yet. Returns whether
    /// anything changed.
    pub fn set_imports(&mut self, imports: Vec<(String, SourceState)>) -> bool {
        if imports == self.imports {
            return false;
        }
        let selected = self.selected_sound();
        self.imports = imports;
        self.refresh_sounds();
        if let Some(row) = selected.and_then(|selected| self.row_of(&selected)) {
            self.sound_selected = row;
        }
        true
    }

    /// Where an import the score names stands, for its heading.
    pub fn import_state(&self, spec: &str) -> Option<&SourceState> {
        self.imports
            .iter()
            .find(|(import, _)| import == spec)
            .map(|(_, state)| state)
    }

    /// The bank that stands in for its whole source: an import holding one
    /// bank, named as the source is - the recordings folder and its
    /// `recordings` bank. A header opening onto a single row of the same
    /// name says it twice and costs a keypress, so the bank takes the
    /// header's place.
    fn lone_bank(&self, section: &SoundSection, count: usize) -> Option<usize> {
        if count != 1 || !matches!(section, SoundSection::Import(_)) {
            return None;
        }
        let index = self
            .sound_results
            .iter()
            .copied()
            .find(|&index| SoundSection::of(&self.sounds[index]) == *section)?;
        // A folder by its own name; a `github:user/repo` import by its repo.
        let label = section.plain_label();
        let source = label.rsplit('/').next().unwrap_or(label);
        self.sounds[index]
            .name
            .eq_ignore_ascii_case(source)
            .then_some(index)
    }

    /// Every bank drawn in its source's place - see [`Self::lone_bank`].
    pub(crate) fn lone_banks(&self) -> Vec<usize> {
        self.visible_categories()
            .iter()
            .filter_map(|(section, count)| self.lone_bank(section, *count))
            .collect()
    }

    /// The kinds of sound present, in the order they are listed, each
    /// with how many sounds it holds. Empty while a search is on: a search
    /// answers with the sounds themselves.
    pub fn visible_categories(&self) -> Vec<(SoundSection, usize)> {
        if !self.sound_query.trim().is_empty() {
            return Vec::new();
        }
        let mut counts: HashMap<SoundSection, usize> = HashMap::new();
        for &index in &self.sound_results {
            *counts
                .entry(SoundSection::of(&self.sounds[index]))
                .or_default() += 1;
        }
        // The kinds in the chooser's order; the score's imports, in the
        // order the library lists them, where the score's own kind sits.
        let mut imports = counts
            .keys()
            .filter(|section| matches!(section, SoundSection::Import(_)))
            .cloned()
            .collect::<Vec<_>>();
        // An import the score names whose banks are not in yet - still on
        // its way, or refused - is listed all the same, so the reader sees
        // it coming where it will land.
        for (spec, _) in &self.imports {
            let section = SoundSection::Import(spec.clone());
            if !counts.contains_key(&section) {
                counts.insert(section.clone(), 0);
                imports.push(section);
            }
        }
        imports.sort();
        let mut present = Vec::new();
        for category in SoundCategory::ALL {
            let section = SoundSection::Kind(category);
            if let Some(&count) = counts.get(&section) {
                present.push((section, count));
            }
            if category == SoundCategory::Score {
                for import in imports.drain(..) {
                    let count = counts[&import];
                    present.push((import, count));
                }
            }
        }
        // One kind of sound is no grouping: a library of drums alone lists
        // its drums.
        if present.len() > 1 {
            present
        } else {
            Vec::new()
        }
    }

    /// Open a kind of sound, or fold it away again.
    pub(super) fn toggle_category(&mut self, position: usize) -> bool {
        let Some((category, _)) = self.visible_categories().get(position).cloned() else {
            return false;
        };
        if !self.open_categories.remove(&category) {
            self.open_categories.insert(category);
        }
        true
    }

    pub(super) fn selected_sound(&self) -> Option<SelectedSound> {
        match self.sound_rows().get(self.sound_selected)? {
            SoundRow::Category(position) => self
                .visible_categories()
                .get(*position)
                .map(|(category, _)| SelectedSound::Category(category.clone())),
            SoundRow::Family(family) => self
                .sound_families()
                .get(*family)
                .map(|family| SelectedSound::Family(family.key.clone())),
            SoundRow::Bank(index) => self
                .sounds
                .get(*index)
                .map(|entry| SelectedSound::Sound(entry.name.clone())),
            SoundRow::Variant(index, variant) => self
                .sounds
                .get(*index)
                .map(|entry| SelectedSound::Variant(entry.name.clone(), *variant)),
        }
    }

    /// The bank under the cursor, and where it came from - for a host that
    /// wants to act on the sound rather than play it.
    pub fn selected_bank(&self) -> Option<(String, rustel_runtime::samples::SoundOrigin)> {
        let index = match self.sound_rows().get(self.sound_selected)? {
            SoundRow::Bank(index) | SoundRow::Variant(index, _) => *index,
            SoundRow::Category(_) | SoundRow::Family(_) => return None,
        };
        let entry = self.sounds.get(index)?;
        Some((entry.name.clone(), entry.origin))
    }

    /// The collection the selected sample came from, in the spelling that
    /// helps a player recognise it: a score import keeps `github:…`, a local
    /// folder keeps its last component, and shipped banks name their pack.
    pub fn selected_sample_source(&self) -> Option<String> {
        let index = match self.sound_rows().get(self.sound_selected)? {
            SoundRow::Bank(index) | SoundRow::Variant(index, _) => *index,
            SoundRow::Category(_) | SoundRow::Family(_) => return None,
        };
        let entry = self.sounds.get(index)?;
        let compact_source = |source: &str| {
            if source.contains(':') && !std::path::Path::new(source).is_absolute() {
                return source.to_owned();
            }
            source
                .trim_end_matches(['/', '\\'])
                .rsplit(['/', '\\'])
                .find(|name| !name.is_empty())
                .unwrap_or(source)
                .to_owned()
        };
        Some(match entry.origin {
            SoundOrigin::Score => entry
                .import
                .as_deref()
                .map(compact_source)
                .unwrap_or_else(|| "this score".to_owned()),
            SoundOrigin::Set => "this set".to_owned(),
            SoundOrigin::Global => entry
                .import
                .as_deref()
                .map(compact_source)
                .unwrap_or_else(|| "my samples".to_owned()),
            SoundOrigin::Default => entry
                .location
                .as_deref()
                .and_then(|location| {
                    rustel_runtime::samples::SampleLibrary::default_sources()
                        .iter()
                        .find(|source| location.starts_with(source.base.trim_end_matches('/')))
                        .map(|source| source.name.clone())
                })
                .unwrap_or_else(|| "built-in samples".to_owned()),
            SoundOrigin::Font => "gm soundfonts".to_owned(),
            SoundOrigin::Synth => "built-in synth".to_owned(),
            SoundOrigin::Input => "audio input".to_owned(),
        })
    }

    /// The row a remembered selection names, in the list as it is now.
    pub(super) fn row_of(&self, selected: &SelectedSound) -> Option<usize> {
        let categories = self.visible_categories();
        let families = self.sound_families();
        self.sound_rows()
            .iter()
            .position(|row| match (row, selected) {
                (SoundRow::Category(position), SelectedSound::Category(category)) => categories
                    .get(*position)
                    .is_some_and(|(at, _)| at == category),
                (SoundRow::Family(family), SelectedSound::Family(key)) => families
                    .get(*family)
                    .is_some_and(|family| &family.key == key),
                (SoundRow::Bank(index), SelectedSound::Sound(name)) => self
                    .sounds
                    .get(*index)
                    .is_some_and(|entry| &entry.name == name),
                (SoundRow::Variant(index, variant), SelectedSound::Variant(name, wanted)) => {
                    variant == wanted
                        && self
                            .sounds
                            .get(*index)
                            .is_some_and(|entry| &entry.name == name)
                }
                _ => false,
            })
    }

    /// Open or fold something, and stay on the row you were on. Opening a
    /// bank folds whatever else was open, and rows above the cursor
    /// disappearing is how a reader ends up somewhere they never went.
    pub(super) fn keeping_place<T>(&mut self, change: impl FnOnce(&mut Self) -> T) -> T {
        let standing_on = self.selected_sound();
        let outcome = change(self);
        let rows = self.sound_rows().len();
        self.sound_selected = standing_on
            .and_then(|selected| self.row_of(&selected))
            .unwrap_or(self.sound_selected)
            .min(rows.saturating_sub(1));
        outcome
    }

    pub(super) fn refresh_sounds(&mut self) {
        let query = self.sound_query.trim();
        self.sound_results = if query.is_empty() {
            // Unasked, the catalogue keeps its own order: the tree groups
            // it, and a ranking would scatter the groups.
            (0..self.sounds.len()).collect()
        } else {
            let mut scored: Vec<_> = self
                .sounds
                .iter()
                .enumerate()
                .filter_map(|(index, entry)| {
                    super::super::fuzzy::score(query, &entry.name, &entry.variant_names, "")
                        .map(|score| (index, score))
                })
                .collect();
            scored.sort_by_key(|(_, score)| std::cmp::Reverse(*score));
            let mut ranked: Vec<_> = scored.into_iter().map(|(index, _)| index).collect();
            // The connected machine comes first: searching `cym` under
            // `.bank("Metal")` asks about Metal's cymbals, not every
            // cymbal in the library. The bank's matches lead, and the
            // rest follow in their ranked order - nothing is hidden, the
            // question is just answered with the likely machine first.
            if !self.sound_prefix.is_empty() {
                let prefixes = self.sound_prefix.clone();
                let (mut ours, rest): (Vec<_>, Vec<_>) = ranked.into_iter().partition(|&index| {
                    prefixes
                        .iter()
                        .any(|prefix| self.sounds[index].name.starts_with(&format!("{prefix}_")))
                });
                ours.extend(rest);
                ranked = ours;
            }
            ranked
        };
        if let Some(expanded) = self.expanded
            && !self.sound_results.contains(&expanded)
        {
            self.expanded = None;
        }
        let rows = self.sound_rows().len();
        self.sound_selected = self.sound_selected.min(rows.saturating_sub(1));
    }

    /// Banks grouped by the name before their first `_`, when more than one
    /// shares it: 683 drum-machine banks read as 89 machines. Only the
    /// unfiltered list groups - a search shows exactly the banks it matched.
    ///
    /// Grouped within a section, never across. A family across sections
    /// would be drawn under the section of its first member, and the other
    /// sections would expand to nothing while their headers still count
    /// those banks. Two imported folders of the same pack share a prefix
    /// such as `KSHMR_`, so this case is common.
    pub fn sound_families(&self) -> Vec<SoundFamily> {
        if !self.sound_query.trim().is_empty() {
            return Vec::new();
        }
        let mut families: Vec<SoundFamily> = Vec::new();
        let mut by_key: HashMap<String, usize> = HashMap::new();
        for &index in &self.sound_results {
            let entry = &self.sounds[index];
            let name = entry.name.as_str();
            let label = name.split('_').next().unwrap_or(name);
            let key = family_key(entry, label);
            match by_key.get(&key) {
                Some(&at) => families[at].members.push(index),
                None => {
                    by_key.insert(key.clone(), families.len());
                    families.push(SoundFamily {
                        label: label.to_owned(),
                        key,
                        members: vec![index],
                    });
                }
            }
        }
        // A family of one is just a bank; only real groups earn a header.
        families.retain(|family| family.members.len() > 1);
        families
    }

    /// The categories the list is grouped under, in the chooser's order.
    /// Empty while a search or a single category is doing the narrowing:
    /// one heading over the whole list says nothing.
    pub fn section_categories(&self) -> Vec<SoundSection> {
        self.visible_categories()
            .into_iter()
            .map(|(section, _)| section)
            .collect()
    }

    /// The family a bank belongs to, as an index into [`Self::sound_families`].
    pub(super) fn family_of(families: &[SoundFamily], index: usize) -> Option<usize> {
        families
            .iter()
            .position(|family| family.members.contains(&index))
    }

    /// The samples list as drawn: a kind of sound, opening onto the banks
    /// of that kind - grouped into machines where a family of banks shares
    /// a name - opening onto the numbered samples. A search skips the
    /// groups and answers with the sounds themselves.
    pub fn sound_rows(&self) -> Vec<SoundRow> {
        let families = self.sound_families();
        let mut family_of = vec![None; self.sounds.len()];
        for (family, group) in families.iter().enumerate() {
            for &member in &group.members {
                family_of[member] = Some(family);
            }
        }
        let categories = self.visible_categories();
        let mut rows = Vec::with_capacity(self.sound_results.len() + MAX_VARIANT_ROWS);
        let push_bank = |rows: &mut Vec<SoundRow>, index: usize| {
            rows.push(SoundRow::Bank(index));
            if self.expanded == Some(index) && self.bank_opens(index) {
                let variants = self.sounds[index].variants.clamp(1, MAX_VARIANT_ROWS);
                for variant in 0..variants {
                    rows.push(SoundRow::Variant(index, variant));
                }
            }
        };
        // A family belongs to the kind of its first bank: the banks of one
        // collection are one kind of sound.
        let push_banks = |rows: &mut Vec<SoundRow>, keep: Option<&SoundSection>| {
            for &index in &self.sound_results {
                match family_of[index] {
                    Some(family) => {
                        let Some(&first) = families[family].members.first() else {
                            continue;
                        };
                        if first != index {
                            continue;
                        }
                        if keep.is_some_and(|kind| SoundSection::of(&self.sounds[first]) != *kind) {
                            continue;
                        }
                        rows.push(SoundRow::Family(family));
                        if self.open_family.as_deref() == Some(families[family].key.as_str()) {
                            for &member in &families[family].members {
                                push_bank(rows, member);
                            }
                        }
                    }
                    None => {
                        if keep.is_some_and(|kind| SoundSection::of(&self.sounds[index]) != *kind) {
                            continue;
                        }
                        push_bank(rows, index);
                    }
                }
            }
        };
        if categories.is_empty() {
            push_banks(&mut rows, None);
            return rows;
        }
        for (position, (category, count)) in categories.iter().enumerate() {
            if let Some(index) = self.lone_bank(category, *count) {
                push_bank(&mut rows, index);
                continue;
            }
            rows.push(SoundRow::Category(position));
            if self.open_categories.contains(category) {
                push_banks(&mut rows, Some(category));
            }
        }
        rows
    }

    /// Whether a bank has anything to open: numbered variants. A synth is
    /// one sound under one name, and a bank of one sample is the sample.
    pub(super) fn bank_opens(&self, index: usize) -> bool {
        self.sounds.get(index).is_some_and(|entry| {
            entry.origin != SoundOrigin::Synth
                && (entry.variants > 1
                    || (matches!(entry.origin, SoundOrigin::Global | SoundOrigin::Set)
                        && !entry.variant_names.is_empty()))
        })
    }

    /// The name a chosen sound goes into the score as. Connected to a
    /// bank, it goes in bare - `cymbal:0`, not `Metal_cymbal:0` - because
    /// the bank call prefixes it at play time, and a pattern keeps working
    /// under another machine by editing that one call. A sound the bank
    /// does not hold keeps its full name: that is the only spelling that
    /// would play.
    fn bare_sound(&self, name: String) -> String {
        if self.sound_prefix.is_empty() {
            return name;
        }
        let (key, variant) = match name.split_once(':') {
            Some((key, variant)) => (key, Some(variant)),
            None => (name.as_str(), None),
        };
        // A sound of one of the connected machines goes in bare - the bank
        // call prefixes it at play time. `Metal_cymbal` under
        // `.bank("Metal Tin")` is `cymbal`, whichever machine takes the hap.
        let belongs = self
            .sound_prefix
            .iter()
            .any(|prefix| key.starts_with(&format!("{prefix}_")));
        if !belongs {
            return name;
        }
        let bare = &key[key.find('_').expect("a prefix underscore") + 1..];
        match variant {
            Some(variant) => format!("{bare}:{variant}"),
            None => bare.to_owned(),
        }
    }

    /// The `bd:3` a row stands for; a bank row is its first variant. A
    /// family names no sound. Connected to a bank, the name goes in bare
    /// (see [`Self::bare_sound`]).
    pub(crate) fn sound_of(&self, row: SoundRow, first_variant_for_bank: bool) -> Option<String> {
        let name = match row {
            SoundRow::Category(_) | SoundRow::Family(_) => return None,
            SoundRow::Bank(index) => self.sounds.get(index).map(|entry| {
                if first_variant_for_bank && entry.origin != SoundOrigin::Synth {
                    format!("{}:0", entry.name)
                } else {
                    entry.name.clone()
                }
            }),
            SoundRow::Variant(index, variant) => self
                .sounds
                .get(index)
                .map(|entry| format!("{}:{variant}", entry.name)),
        }?;
        Some(self.bare_sound(name))
    }

    /// Alt+R aliases a bank row and renames an individual local file.
    pub fn rename(&self) -> PanelAction {
        if self.tab != Tab::Samples {
            return PanelAction::Nothing;
        }
        let Some(row) = self.sound_rows().get(self.sound_selected).cloned() else {
            return PanelAction::Nothing;
        };
        let (index, variant) = match row {
            SoundRow::Bank(index) => (index, None),
            SoundRow::Variant(index, variant) => (index, Some(variant)),
            _ => return PanelAction::Nothing,
        };
        let entry = &self.sounds[index];
        if !matches!(entry.origin, SoundOrigin::Global | SoundOrigin::Set) {
            return PanelAction::Nothing;
        }
        match variant {
            Some(variant) if owns_file(entry, variant) => PanelAction::RenameSample {
                name: entry.name.clone(),
                variant,
            },
            Some(_) => PanelAction::Nothing,
            None => PanelAction::RenameBank {
                name: entry.name.clone(),
                origin: entry.origin,
            },
        }
    }

    pub fn prepare_sample_rename(
        &mut self,
        bank: &str,
        variant: usize,
        name: &str,
        location: &str,
    ) {
        if let Some(entry) = self.sounds.iter_mut().find(|entry| entry.name == bank) {
            if let Some(stem) = entry.variant_names.get_mut(variant) {
                if self.sound_query.trim().eq_ignore_ascii_case(stem) {
                    self.sound_query = name.to_owned();
                }
                *stem = name.to_owned();
            }
            if variant == 0 {
                entry.location = Some(location.to_owned());
            }
        }
    }

    /// A single local file, including a bank row when it contains only one.
    pub fn selected_sample_file(&self) -> Option<(String, usize)> {
        if self.tab != Tab::Samples {
            return None;
        }
        let (index, variant) = match self.sound_rows().get(self.sound_selected)? {
            SoundRow::Bank(index) if self.sounds.get(*index)?.variants == 1 => (*index, 0),
            SoundRow::Variant(index, variant) => (*index, *variant),
            _ => return None,
        };
        let entry = self.sounds.get(index)?;
        owns_file(entry, variant).then(|| (entry.name.clone(), variant))
    }

    /// Whether the selected row is a bank row of local files, whose files
    /// are expanded and picked one at a time.
    pub fn selected_bank_holds_local_files(&self) -> bool {
        self.tab == Tab::Samples
            && matches!(
                self.sound_rows().get(self.sound_selected),
                Some(SoundRow::Bank(index))
                    if self.sounds.get(*index).is_some_and(|entry| owns_file(entry, 0))
            )
    }

    /// Whether the selected row belongs to a local layer the player owns.
    pub fn selected_bank_is_editable(&self) -> bool {
        self.selected_bank()
            .is_some_and(|(_, origin)| matches!(origin, SoundOrigin::Global | SoundOrigin::Set))
    }
}
