//! The vst tab: the plugins the host found, and under the open plugin two
//! rows, its presets and its parameters. → on one of the two rows opens
//! that one half of the plugin as the view of the tab, with its own search;
//! ← and Esc go back to the plugin list. The word lists of a plugin call
//! are here too: the plugin names, the parameter keys and the preset names
//! a completion offers.
//!
//! The tab and the word lists hold a copy of what the host knows and never
//! call the host. The App reads the host on the turn loop and asks for the
//! load.

use std::collections::BTreeSet;

use rustel_runtime::vst::{ParamInfo, PluginInfo, Status, canonical};

use super::{Reference, ReferencePanel, score_key};
use crate::number_form::short_number;

/// One row of the vst tab: a plugin, or a row under the open plugin.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PluginRow {
    /// A plugin, as an index into [`VstTab::plugins`].
    Plugin(usize),
    /// One of the two rows under the open plugin, naming the half of it
    /// the row opens.
    Section(VstSection),
    /// A parameter of the open plugin, as an index into its parameters.
    Param(usize),
    /// A group of parameters of the open plugin, as an index into
    /// [`VstTab::groups`].
    Group(usize),
    /// A preset of the open plugin, as an index into [`VstTab::presets`].
    Preset(usize),
}

/// One half of an open plugin: its parameters, or its presets.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum VstSection {
    #[default]
    Params,
    Presets,
}

impl VstSection {
    /// The word the tab names the half by, in a row and in the breadcrumb.
    pub fn said(self) -> &'static str {
        match self {
            VstSection::Params => "parameters",
            VstSection::Presets => "presets",
        }
    }
}

/// The state of the vst tab: the plugin list, what is typed, where the
/// cursor is, and what is open.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct VstTab {
    plugins: Vec<PluginInfo>,
    query: String,
    /// The search of the half of the plugin the tab shows. It starts empty
    /// with the half, so a filter of the plugin list does not follow the
    /// reader into it.
    inside_query: String,
    pub selected: usize,
    /// The plugin whose presets and parameters are listed, by name.
    open: Option<String>,
    /// The half of the open plugin the tab shows, with the plugin list
    /// left for its own view. `None` while the tab lists plugins.
    inside: Option<VstSection>,
    /// The parameter groups of the open plugin whose rows are listed, by
    /// name.
    open_groups: BTreeSet<String>,
    /// The preset names of the open plugin.
    presets: Vec<String>,
    /// The App owes the tab the presets of the open plugin: the plugin
    /// opened, or the plugin list changed.
    presets_due: bool,
    /// The parameter groups of the open plugin, in the order the plugin
    /// gives them: the name and the number of parameters.
    groups: Vec<(String, usize)>,
    /// Where the read of the plugin folders is, while it runs: the bundles
    /// read, of all of them. The count line says it, as the samples tab
    /// says its imports.
    scan: Option<(usize, usize)>,
    /// The host reads the preset folders, on its own thread: the count
    /// line says it while it runs.
    presets_reading: bool,
    /// The list as drawn. Kept, not derived: a plugin has up to some
    /// thousand parameters, and each frame reads the rows more than one time.
    rows: Vec<PluginRow>,
    pub(super) scroll: std::cell::Cell<usize>,
}

/// What a word list of a plugin call holds: the list a completion opens
/// inside `.vst()` or `.vsti()`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PluginWords {
    /// Plugin names, for the first argument: instruments for `.vsti()`,
    /// effects for `.vst()`.
    Names { instrument: bool },
    /// The parameter keys of the plugin with this name, and `preset`, for
    /// the object of the call.
    Keys(String),
    /// The preset names of the plugin with this name, for the string of
    /// `preset`.
    Presets(String),
}

/// The words for the object of a plugin call, and the text beside each
/// word: `preset`, then each parameter key in the form a score writes, with
/// its title and its default.
pub fn key_words(params: &[ParamInfo]) -> (Vec<String>, Vec<String>) {
    let preset = (
        "preset".to_owned(),
        "a preset file of the plugin".to_owned(),
    );
    std::iter::once(preset)
        .chain(params.iter().map(|param| {
            let detail = format!("{} · {}", param.name, param.default_shown());
            (score_key(&param.key), detail)
        }))
        .unzip()
}

/// Ranks plugin names as the host finds a plugin: upper case, spaces and
/// punctuation do not count, a part of a name is enough, and the shortest
/// name with the part leads. The names the list search finds follow, so a
/// name with a typing error still has an answer.
pub fn rank_names(query: &str, names: &[String]) -> Vec<usize> {
    let wanted = canonical(query);
    let mut ranked: Vec<usize> = Vec::new();
    if !wanted.is_empty() {
        ranked.extend((0..names.len()).filter(|index| canonical(&names[*index]).contains(&wanted)));
        ranked.sort_by_key(|index| canonical(&names[*index]).len());
    }
    let searched = super::super::fuzzy::rank(query, names.iter().map(String::as_str));
    for index in searched {
        if !ranked.contains(&index) {
            ranked.push(index);
        }
    }
    ranked
}

impl ReferencePanel {
    /// A word list for a plugin call, searched for what is typed. `details`
    /// has the text beside each name, or nothing.
    pub fn plugin_words_for(
        reference: &Reference,
        words: PluginWords,
        names: Vec<String>,
        details: Vec<String>,
        query: &str,
    ) -> Self {
        let subject = match words {
            PluginWords::Names { .. } => "plugins",
            PluginWords::Keys(_) => "parameter",
            PluginWords::Presets(_) => "presets",
        };
        let mut panel = Self::vocabulary_for(reference, subject, names, query);
        let vocabulary = panel.vocabulary.as_mut().expect("a word list");
        vocabulary.plugin = Some(words);
        if !details.is_empty() {
            vocabulary.details = details;
        }
        panel.refresh(reference);
        panel
    }

    /// What the word list holds, when the list is of a plugin call.
    pub fn plugin_words(&self) -> Option<&PluginWords> {
        self.vocabulary.as_ref()?.plugin.as_ref()
    }

    /// New words from the plugin host for the word list of a plugin call:
    /// a plugin ended its load, or the host read a folder. The cursor stays
    /// on its word. Returns false with no change.
    pub fn set_plugin_words(
        &mut self,
        reference: &Reference,
        names: Vec<String>,
        details: Vec<String>,
    ) -> bool {
        let selected = self.selected_result();
        let Some(vocabulary) = self
            .vocabulary
            .as_mut()
            .filter(|list| list.plugin.is_some())
        else {
            return false;
        };
        let details = if details.is_empty() {
            vec![String::new(); names.len()]
        } else {
            details
        };
        if vocabulary.names == names && vocabulary.details == details {
            return false;
        }
        let word = selected.and_then(|index| vocabulary.names.get(index).cloned());
        vocabulary.names = names;
        vocabulary.details = details;
        self.refresh(reference);
        let names = &self.vocabulary.as_ref().expect("checked").names;
        let kept = word
            .and_then(|word| names.iter().position(|name| *name == word))
            .and_then(|name| self.results.iter().position(|index| *index == name));
        if let Some(result) = kept {
            self.selected = self.row_of_result(result);
        }
        true
    }
}

fn parameter_text(param: &ParamInfo) -> String {
    format!("{}: {}", score_key(&param.key), short_number(param.default))
}

/// The rows of the parameters of one group, in the order of the plugin. The
/// group with no name is the top level.
fn rows_of_group<'a>(
    params: &'a [ParamInfo],
    group: &'a str,
) -> impl Iterator<Item = PluginRow> + 'a {
    params
        .iter()
        .enumerate()
        .filter(move |(_, param)| param.group == group)
        .map(|(index, _)| PluginRow::Param(index))
}

impl VstTab {
    /// A key completion writes a usable option, with the same default as
    /// a parameter chosen from the plugin's expanded row.
    pub fn key_text(params: &[ParamInfo], key: &str) -> Option<String> {
        if key == "preset" {
            return Some("preset: \"\"".into());
        }
        params
            .iter()
            .find(|param| score_key(&param.key) == key)
            .map(parameter_text)
    }

    /// Open the plugin named by the call in the editor. Keep the name while
    /// the folder scan or plugin load is still filling in the list.
    pub fn open_named(&mut self, name: String) {
        self.set_open(Some(name));
        self.refresh();
        self.select_open_plugin();
    }

    fn select_open_plugin(&mut self) {
        if let Some(index) = self
            .plugins
            .iter()
            .position(|plugin| Some(&plugin.name) == self.open.as_ref())
        {
            self.select(PluginRow::Plugin(index));
        }
    }

    pub fn plugins(&self) -> &[PluginInfo] {
        &self.plugins
    }

    /// The text of the search box: of the plugin list, or of the half of
    /// the plugin the tab shows.
    pub fn query(&self) -> &str {
        if self.inside.is_some() {
            &self.inside_query
        } else {
            &self.query
        }
    }

    /// The half of the open plugin the tab shows, when the plugin list is
    /// left for it.
    pub fn inside(&self) -> Option<VstSection> {
        self.inside
    }

    /// Where the tab stands, for the line under its tabs: `Serum >
    /// presets`. `None` while the tab lists plugins.
    pub fn breadcrumb(&self) -> Option<String> {
        Some(format!(
            "{} > {}",
            self.open.as_deref()?,
            self.inside?.said()
        ))
    }

    pub fn presets(&self) -> &[String] {
        &self.presets
    }

    pub fn groups(&self) -> &[(String, usize)] {
        &self.groups
    }

    pub fn rows(&self) -> &[PluginRow] {
        &self.rows
    }

    /// The open plugin, when the list has the plugin.
    pub fn open_plugin(&self) -> Option<&PluginInfo> {
        let open = self.open.as_deref()?;
        self.plugins.iter().find(|plugin| plugin.name == open)
    }

    /// The name of the open plugin as the tab holds it, also while the
    /// host loads the plugin and the list has it not yet.
    pub fn open_name(&self) -> Option<&str> {
        self.open.as_deref()
    }

    pub fn group_is_open(&self, group: &str) -> bool {
        self.open_groups.contains(group)
    }

    /// True while a search lists the rows of the view the cursor is in:
    /// the plugins, or the half of the open plugin.
    pub fn searching(&self) -> bool {
        !self.query().trim().is_empty()
    }

    /// The open plugin, when the host did not load the plugin yet. The App
    /// asks the host for the load.
    pub fn plugin_to_load(&self) -> Option<&str> {
        self.open.as_deref().filter(|_| {
            self.open_plugin()
                .is_none_or(|plugin| plugin.status == Status::Found)
        })
    }

    /// True while the host loads a plugin of the list.
    pub fn loading(&self) -> bool {
        self.plugins
            .iter()
            .any(|plugin| plugin.status == Status::Loading)
    }

    /// The plugin whose preset folder the App has to read now.
    pub fn presets_due(&self) -> Option<&str> {
        self.open.as_deref().filter(|_| self.presets_due)
    }

    pub fn set_presets(&mut self, presets: Vec<String>) {
        self.presets_due = false;
        self.keeping_place(|tab| tab.presets = presets);
    }

    /// The presets came from a read that is past: the next sync asks the
    /// host again. The answer says the tab owes the ask.
    pub fn presets_stale(&mut self) -> bool {
        if self.open.is_none() || self.presets_due {
            return false;
        }
        self.presets_due = true;
        true
    }

    /// Where the read of the plugin folders is, while it runs: the bundles
    /// read, of all of them.
    pub fn scan(&self) -> Option<(usize, usize)> {
        self.scan
    }

    /// Takes where the read of the plugin folders is, `None` when no read
    /// runs. The answer says the count line changed.
    pub fn set_scan(&mut self, scan: Option<(usize, usize)>) -> bool {
        let changed = self.scan != scan;
        self.scan = scan;
        changed
    }

    /// Takes whether the host reads the preset folders now. The answer
    /// says the count line changed.
    pub fn set_presets_reading(&mut self, reading: bool) -> bool {
        let changed = self.presets_reading != reading;
        self.presets_reading = reading;
        changed
    }

    /// True while the host reads the preset folders on its own thread.
    pub fn presets_reading(&self) -> bool {
        self.presets_reading
    }

    /// Takes a new plugin list from the host. Returns false when the list
    /// did not change. The open plugin stays open and the cursor stays on
    /// its row.
    pub fn set_plugins(&mut self, plugins: Vec<PluginInfo>) -> bool {
        if plugins == self.plugins {
            return false;
        }
        let bundle = self.open_plugin().map(|plugin| plugin.bundle.clone());
        let pending = self.open.is_some() && bundle.is_none();
        self.keeping_place(|tab| {
            tab.plugins = plugins;
            // A plugin has the name of its bundle before the load, and its
            // own name after. The open row follows the bundle then.
            if tab.open_plugin().is_none() {
                let renamed = tab
                    .plugins
                    .iter()
                    .find(|plugin| Some(&plugin.bundle) == bundle.as_ref())
                    .map(|plugin| plugin.name.clone())
                    .or_else(|| {
                        let wanted = canonical(tab.open.as_deref()?);
                        tab.plugins
                            .iter()
                            .filter(|plugin| canonical(&plugin.name).contains(&wanted))
                            .min_by_key(|plugin| canonical(&plugin.name).len())
                            .map(|plugin| plugin.name.clone())
                    })
                    .or_else(|| pending.then(|| tab.open.clone()).flatten());
                tab.set_open(renamed);
            }
            tab.presets_due = true;
        });
        if pending {
            self.select_open_plugin();
        }
        true
    }

    /// Changes what is typed in the search box of the view that shows: the
    /// plugin list, or the half of the plugin the tab is in. The cursor
    /// goes to the first row.
    pub fn edit_query(&mut self, edit: impl FnOnce(&mut String)) {
        let query = if self.inside.is_some() {
            &mut self.inside_query
        } else {
            &mut self.query
        };
        edit(query);
        self.selected = 0;
        self.refresh();
    }

    pub fn move_by(&mut self, delta: isize) {
        let last = self.rows.len().saturating_sub(1) as isize;
        self.selected = (self.selected as isize + delta).clamp(0, last) as usize;
    }

    /// → opens the plugin or the group under the cursor, and the half of
    /// the open plugin a row under it names. Returns false with nothing to
    /// open.
    pub fn expand(&mut self) -> bool {
        match self.rows.get(self.selected).copied() {
            Some(PluginRow::Plugin(index)) => {
                let name = self.plugins[index].name.clone();
                if self.open.as_deref() == Some(name.as_str()) {
                    return false;
                }
                self.keeping_place(|tab| tab.set_open(Some(name)));
                true
            }
            Some(PluginRow::Section(section)) => self.show_section(section),
            Some(PluginRow::Group(index)) => {
                let name = self.groups[index].0.clone();
                let opened = self.open_groups.insert(name);
                self.refresh();
                opened
            }
            _ => false,
        }
    }

    /// ← closes what the cursor is in, the innermost first: the group, then
    /// the half of the open plugin, then the plugin. The cursor goes to the
    /// row of what closed. An empty half has no row under the cursor: ←
    /// leaves it the way Esc does.
    pub fn collapse(&mut self) -> bool {
        let Some(row) = self.rows.get(self.selected).copied() else {
            return self.leave();
        };
        let group = match row {
            PluginRow::Group(index) => Some(index),
            PluginRow::Param(index) if !self.searching() => self
                .open_plugin()
                .and_then(|plugin| plugin.params.get(index))
                .and_then(|param| {
                    self.groups
                        .iter()
                        .position(|(name, _)| *name == param.group)
                }),
            _ => None,
        };
        if let Some(index) = group
            && self.open_groups.remove(&self.groups[index].0)
        {
            self.refresh();
            self.select(PluginRow::Group(index));
            return true;
        }
        if self.leave() {
            return true;
        }
        let plugin = match row {
            PluginRow::Plugin(index) => Some(index),
            _ => None,
        };
        let open = self
            .plugins
            .iter()
            .position(|plugin| Some(&plugin.name) == self.open.as_ref());
        if open.is_none() || plugin.is_some_and(|index| Some(index) != open) {
            return false;
        }
        self.set_open(None);
        self.refresh();
        if let Some(index) = open {
            self.select(PluginRow::Plugin(index));
        }
        true
    }

    /// Enter on a group: open the group, or close the group.
    pub fn toggle_group(&mut self, index: usize) {
        let name = &self.groups[index].0;
        if !self.open_groups.remove(name) {
            self.open_groups.insert(name.clone());
        }
        self.refresh();
    }

    /// Opens one half of the open plugin as the view of the tab - the
    /// plugin list left for it - with its search empty and the cursor on
    /// its first row. The half fills in with the load of the plugin.
    /// Returns false with no plugin open.
    pub fn show_section(&mut self, section: VstSection) -> bool {
        if self.open.is_none() {
            return false;
        }
        self.inside = Some(section);
        self.inside_query.clear();
        self.refresh();
        self.selected = 0;
        self.scroll.set(0);
        true
    }

    /// Tab in the half of an open plugin: the other half of the plugin
    /// shows, its search stays, so a search holds in both halves. Returns
    /// false while the tab lists plugins, where Tab goes to the next tab
    /// of the panel.
    pub fn toggle_section(&mut self) -> bool {
        let Some(section) = self.inside else {
            return false;
        };
        let other = match section {
            VstSection::Params => VstSection::Presets,
            VstSection::Presets => VstSection::Params,
        };
        self.inside = Some(other);
        self.refresh();
        self.selected = 0;
        true
    }

    /// ← and Esc in the half of an open plugin: back to the plugin list,
    /// the cursor on the row of the half. Returns false while the tab
    /// lists plugins.
    pub fn leave(&mut self) -> bool {
        let Some(section) = self.inside else {
            return false;
        };
        self.inside = None;
        self.refresh();
        self.select(PluginRow::Section(section));
        true
    }

    /// The cursor goes to the first row under the open plugin, and the
    /// search of the plugin list goes with it: the rows under the plugin
    /// are whole again. Returns false when the open plugin lists no row
    /// under it.
    pub fn select_under(&mut self) -> bool {
        if !self.query.is_empty() {
            self.query.clear();
            self.refresh();
        }
        let Some(at) = self
            .rows
            .iter()
            .position(|row| matches!(row, PluginRow::Section(_)))
        else {
            return false;
        };
        self.selected = at;
        true
    }

    /// The row a list position has, for a click.
    pub fn row(&self, at: usize) -> Option<PluginRow> {
        self.rows.get(at).copied()
    }

    /// The text Enter writes into the score for the row under the cursor:
    /// the plugin call, one parameter with its value at the start, or one
    /// preset. A group and a row naming a half of the plugin have no text;
    /// Enter opens them. A plugin has no text before its load or its test:
    /// these tell an instrument from an effect, and a score writes
    /// `.vsti()` for the first and `.vst()` for the second.
    pub fn score_text(&self) -> Option<String> {
        match self.rows.get(self.selected).copied()? {
            PluginRow::Plugin(index) => {
                let plugin = &self.plugins[index];
                let call = if plugin.instrument { "vsti" } else { "vst" };
                let known = plugin.status == Status::Ready || !plugin.categories.is_empty();
                known.then(|| format!(".{call}({:?})", plugin.name))
            }
            PluginRow::Param(index) => {
                let param = &self.open_plugin()?.params[index];
                Some(parameter_text(param))
            }
            PluginRow::Group(_) | PluginRow::Section(_) => None,
            PluginRow::Preset(index) => Some(format!("preset: {:?}", self.presets[index])),
        }
    }

    /// Opens one plugin, or none. The open groups and the presets belong to
    /// the plugin open before, so they go, and so does the half of it the
    /// tab shows when the plugin itself closes.
    fn set_open(&mut self, name: Option<String>) {
        if self.open != name {
            self.open = name;
            self.open_groups.clear();
            self.presets.clear();
            self.presets_due = true;
            if self.open.is_none() {
                self.inside = None;
            }
        }
    }

    fn select(&mut self, row: PluginRow) {
        if let Some(at) = self.rows.iter().position(|shown| *shown == row) {
            self.selected = at;
        }
    }

    /// Changes the list and puts the cursor back on its row: the same
    /// plugin by name, or by bundle after the load gave the plugin its own
    /// name, and the same row under the open plugin.
    fn keeping_place(&mut self, change: impl FnOnce(&mut Self)) {
        let row = self.rows.get(self.selected).copied();
        let plugin = match row {
            Some(PluginRow::Plugin(index)) => self.plugins.get(index),
            Some(_) => self.open_plugin(),
            None => None,
        }
        .map(|plugin| (plugin.name.clone(), plugin.bundle.clone()));
        change(self);
        self.refresh();
        let plugin = plugin.and_then(|(name, bundle)| {
            let named = self.plugins.iter().position(|plugin| plugin.name == name);
            named.or_else(|| {
                self.plugins
                    .iter()
                    .position(|plugin| plugin.bundle == bundle)
            })
        });
        let wanted = [
            row.filter(|row| !matches!(row, PluginRow::Plugin(_))),
            plugin.map(PluginRow::Plugin),
        ];
        if let Some(at) = wanted
            .into_iter()
            .flatten()
            .find_map(|row| self.rows.iter().position(|shown| *shown == row))
        {
            self.selected = at;
        }
        self.selected = self.selected.min(self.rows.len().saturating_sub(1));
    }

    /// Builds the groups and the rows again after a change.
    fn refresh(&mut self) {
        let open = self.open_index();
        let params: &[ParamInfo] = open.map_or(&[], |index| &self.plugins[index].params);

        self.groups.clear();
        // The parameters of one group are together in most plugins, so the
        // group of the parameter before is the first guess.
        let mut last = usize::MAX;
        for param in params.iter().filter(|param| !param.group.is_empty()) {
            if self
                .groups
                .get(last)
                .is_none_or(|(name, _)| *name != param.group)
            {
                last = match self
                    .groups
                    .iter()
                    .position(|(name, _)| *name == param.group)
                {
                    Some(known) => known,
                    None => {
                        self.groups.push((param.group.clone(), 0));
                        self.groups.len() - 1
                    }
                };
            }
            self.groups[last].1 += 1;
        }

        // A field read, not `query()`: the borrow of the query ends after
        // the match, and the rows are built on the borrowed text.
        let query = match self.inside {
            Some(_) => self.inside_query.trim(),
            None => self.query.trim(),
        };
        self.rows.clear();
        if let Some(section) = self.inside {
            // The half of the open plugin is the view of the tab: the
            // search of it lists the half, and nothing else.
            match section {
                VstSection::Params => {
                    if query.is_empty() {
                        self.rows.extend(rows_of_group(params, ""));
                        for (index, (name, _)) in self.groups.iter().enumerate() {
                            self.rows.push(PluginRow::Group(index));
                            if self.open_groups.contains(name) {
                                self.rows.extend(rows_of_group(params, name));
                            }
                        }
                    } else {
                        // A search reads each parameter by its key, its
                        // title and its group, and lists the matches flat,
                        // the nearest first.
                        let score = |text: &String| super::super::fuzzy::name_score(query, text);
                        let mut matches: Vec<_> = params
                            .iter()
                            .enumerate()
                            .filter_map(|(index, param)| {
                                let best = [&param.key, &param.name, &param.group]
                                    .into_iter()
                                    .filter_map(score)
                                    .max()?;
                                Some((best, index))
                            })
                            .collect();
                        matches.sort_by_key(|(best, _)| std::cmp::Reverse(*best));
                        self.rows
                            .extend(matches.iter().map(|(_, index)| PluginRow::Param(*index)));
                    }
                }
                // The presets of the other half: all of them, or the ones
                // the search finds.
                VstSection::Presets => {
                    self.rows.extend(
                        self.presets
                            .iter()
                            .enumerate()
                            .filter(|(_, preset)| {
                                query.is_empty()
                                    || super::super::fuzzy::name_score(query, preset).is_some()
                            })
                            .map(|(index, _)| PluginRow::Preset(index)),
                    );
                }
            }
        } else {
            // A search of the plugin list keeps the plugins whose names it
            // finds, the nearest first, and nothing else: the halves of a
            // plugin are rows of their own view.
            let shown = super::super::fuzzy::rank(
                query,
                self.plugins.iter().map(|plugin| plugin.name.as_str()),
            );
            for index in shown {
                self.rows.push(PluginRow::Plugin(index));
                if Some(index) == open {
                    self.rows.push(PluginRow::Section(VstSection::Presets));
                    self.rows.push(PluginRow::Section(VstSection::Params));
                }
            }
        }
        self.selected = self.selected.min(self.rows.len().saturating_sub(1));
    }

    /// The place of the open plugin in the list, once the host loaded it.
    fn open_index(&self) -> Option<usize> {
        self.plugins
            .iter()
            .position(|plugin| Some(&plugin.name) == self.open.as_ref())
            .filter(|index| self.plugins[*index].status == Status::Ready)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn param(key: &str, name: &str, group: &str) -> ParamInfo {
        ParamInfo {
            id: 0,
            name: name.into(),
            key: key.into(),
            group: group.into(),
            units: String::new(),
            default: 0.5,
            steps: 0,
            default_text: "0.5".into(),
            role: Default::default(),
        }
    }

    fn plugin(name: &str, status: Status, params: Vec<ParamInfo>) -> PluginInfo {
        PluginInfo {
            name: name.into(),
            bundle: format!("/plugins/{name}.vst3").into(),
            vendor: String::new(),
            categories: String::new(),
            status,
            params: params.into(),
            instrument: false,
            running: 0,
            load_time: std::time::Duration::ZERO,
            audio_thread: false,
            latency: None,
            presets: 0,
        }
    }

    /// An instrument with 1 parameter at the top level and 2 groups.
    fn synth() -> PluginInfo {
        let params = vec![
            param("volume", "Volume", ""),
            param("oscalevel", "Level", "OSC A"),
            param("oscapan", "Pan", "OSC A"),
            param("filter1cutoff", "Cutoff", "Filter 1"),
        ];
        PluginInfo {
            instrument: true,
            ..plugin("Synth", Status::Ready, params)
        }
    }

    #[test]
    fn a_call_can_open_its_plugin_before_the_folder_scan_finishes() {
        let mut tab = VstTab::default();
        tab.open_named("synth".into());
        assert_eq!(tab.plugin_to_load(), Some("synth"));
        tab.set_plugins(vec![plugin("Echo", Status::Found, Vec::new()), synth()]);
        assert_eq!(tab.open_plugin().unwrap().name, "Synth");
        assert_eq!(tab.row(tab.selected), Some(PluginRow::Plugin(1)));
        assert!(
            tab.rows()
                .contains(&PluginRow::Section(VstSection::Presets))
        );
        assert_eq!(tab.plugin_to_load(), None);
    }

    /// → opens a plugin onto the two rows of its halves, and → on one of
    /// them makes that half the view of the tab. Tab flips the halves, ←
    /// and Esc go back to the row of the half, and another ← closes the
    /// plugin.
    #[test]
    fn a_plugin_opens_onto_two_rows_and_each_row_opens_its_half() {
        use PluginRow::{Group, Param, Plugin, Preset, Section};
        let mut tab = VstTab::default();
        tab.set_plugins(vec![plugin("Echo", Status::Found, Vec::new()), synth()]);
        assert_eq!(tab.rows(), [Plugin(0), Plugin(1)]);

        tab.move_by(1);
        assert!(tab.expand());
        assert_eq!(tab.presets_due(), Some("Synth"));
        tab.set_presets(vec!["Lead".into()]);
        assert_eq!(
            tab.groups(),
            [("OSC A".to_owned(), 2), ("Filter 1".to_owned(), 1)]
        );
        let closed = [
            Plugin(0),
            Plugin(1),
            Section(VstSection::Presets),
            Section(VstSection::Params),
        ];
        assert_eq!(tab.rows(), closed);

        // → on the row of the parameters opens them as the view of the tab,
        // with the search empty and the cursor on the first row.
        tab.selected = 3;
        assert!(tab.expand());
        assert_eq!(tab.breadcrumb().as_deref(), Some("Synth > parameters"));
        assert_eq!(tab.rows(), [Param(0), Group(0), Group(1)]);
        assert_eq!(tab.selected, 0);
        assert_eq!(tab.query(), "");

        // → on a group lists its parameters. ← from a parameter closes the
        // group and goes to its row.
        tab.selected = 1;
        assert!(tab.expand());
        assert_eq!(
            tab.rows(),
            [Param(0), Group(0), Param(1), Param(2), Group(1)]
        );
        tab.selected = 3;
        assert_eq!(tab.score_text().as_deref(), Some("oscapan: 0.5"));
        assert_eq!(score_key("516000001"), "516000001");
        assert_eq!(score_key("2ndosc"), "'2ndosc'");
        assert_eq!(score_key("gain²"), "'gain²'");
        assert!(tab.collapse());
        assert_eq!(
            (tab.rows(), tab.selected),
            (&[Param(0), Group(0), Group(1)][..], 1)
        );

        // Tab flips to the other half, and the cursor goes to its first
        // row. ← and Esc go back to the plugin list, the cursor on the row
        // of the half, and another ← closes the plugin.
        assert!(tab.toggle_section());
        assert_eq!(tab.breadcrumb().as_deref(), Some("Synth > presets"));
        assert_eq!(tab.rows(), [Preset(0)]);
        assert_eq!(tab.score_text().as_deref(), Some("preset: \"Lead\""));
        assert!(tab.collapse());
        assert_eq!((tab.rows(), tab.selected), (&closed[..], 2));
        assert!(tab.show_section(VstSection::Presets));
        assert!(tab.leave());
        assert_eq!((tab.rows(), tab.selected), (&closed[..], 2));
        tab.selected = 2;
        assert!(tab.collapse());
        assert_eq!(tab.rows(), [Plugin(0), Plugin(1)]);
        assert_eq!(tab.selected, 1);
        assert_eq!(tab.score_text().as_deref(), Some(".vsti(\"Synth\")"));
        tab.selected = 0;
        assert_eq!(tab.score_text(), None, "no kind before the load");
    }

    /// A search of the plugin list keeps the plugins its text finds and
    /// nothing else. A search of a half of an open plugin lists the rows
    /// of that half, and goes with Tab to the other half.
    /// An empty half of the open plugin has no row under the cursor, and
    /// ← still leaves it, the way Esc does.
    #[test]
    fn left_leaves_an_empty_half_of_the_open_plugin() {
        use PluginRow::Section;
        let mut tab = VstTab::default();
        tab.set_plugins(vec![synth()]);
        tab.expand();
        // The plugin holds no preset file: the half of it is empty.
        assert!(tab.show_section(VstSection::Presets));
        assert!(tab.rows().is_empty(), "the half holds no preset");
        assert!(tab.collapse(), "left leaves the empty half");
        assert_eq!(tab.inside(), None);
        assert_eq!(
            tab.row(tab.selected),
            Some(Section(VstSection::Presets)),
            "the cursor stands on the row of the half"
        );
        assert_eq!(tab.rows().len(), 3, "the plugin with its two rows");
    }

    #[test]
    fn a_search_of_the_list_reads_names_and_a_search_of_the_half_reads_the_half() {
        use PluginRow::{Param, Plugin, Preset};
        let mut tab = VstTab::default();
        tab.set_plugins(vec![plugin("Echo", Status::Ready, Vec::new()), synth()]);
        tab.move_by(1);
        tab.expand();
        tab.set_presets(vec!["Lead".into()]);

        // The search of the list reads plugin names only: the open plugin
        // with no matching name goes with its two rows.
        tab.edit_query(|query| query.push_str("cutoff"));
        assert_eq!(tab.rows(), [], "no plugin name holds the letters");
        tab.edit_query(|query| *query = "echo".into());
        assert_eq!(tab.rows(), [Plugin(0)]);
        assert_eq!(tab.score_text().as_deref(), Some(".vst(\"Echo\")"));

        // A search of the parameters reads the key, the title and the group
        // of each one, flat.
        tab.edit_query(String::clear);
        tab.show_section(VstSection::Params);
        tab.edit_query(|query| query.push_str("cutoff"));
        assert_eq!(tab.rows(), [Param(3)]);
        tab.edit_query(|query| *query = "osc".into());
        assert_eq!(tab.rows(), [Param(1), Param(2)]);

        // The search holds in the other half: Tab with it lists the
        // presets it finds, and the cursor goes to them.
        tab.edit_query(|query| *query = "lead".into());
        assert!(tab.toggle_section());
        assert_eq!(tab.row(tab.selected), Some(Preset(0)));
        // The search gone, the half lists all of it.
        tab.edit_query(String::clear);
        assert_eq!(tab.rows(), [Preset(0)]);
    }

    #[test]
    fn a_new_plugin_list_keeps_the_open_plugin_the_open_group_and_the_cursor() {
        let mut tab = VstTab::default();
        let found = plugin("synth-bundle", Status::Found, Vec::new());
        tab.set_plugins(vec![found.clone()]);
        assert!(tab.expand());
        assert_eq!(tab.plugin_to_load(), Some("synth-bundle"));

        // The load gives the plugin its own name, and the half of it that
        // shows fills in.
        let ready = PluginInfo {
            bundle: found.bundle,
            ..synth()
        };
        assert!(tab.set_plugins(vec![ready.clone()]));
        assert_eq!(
            tab.open_plugin().map(|plugin| plugin.name.as_str()),
            Some("Synth")
        );
        assert_eq!(tab.plugin_to_load(), None);
        assert!(tab.show_section(VstSection::Params));

        // A second plugin goes before the first in the list.
        tab.selected = 1;
        tab.expand();
        tab.move_by(1);
        let row = tab.rows()[tab.selected];
        assert_eq!(row, PluginRow::Param(1));
        assert!(tab.set_plugins(vec![plugin("Echo", Status::Found, Vec::new()), ready]));
        assert!(tab.group_is_open("OSC A"));
        assert_eq!(tab.rows()[tab.selected], row);
        assert!(!tab.set_plugins(tab.plugins().to_vec()));

        // The host unloaded the open plugin. The half stays open with no
        // row in it, and the tab asks for the load again.
        let mut unloaded = tab.plugins().to_vec();
        unloaded[1].status = Status::Found;
        unloaded[1].params = Vec::new().into();
        assert!(tab.set_plugins(unloaded));
        assert_eq!(tab.rows(), []);
        assert_eq!(tab.plugin_to_load(), Some("Synth"));
        // The plugin that comes back fills the half in again.
        let mut reloaded = tab.plugins().to_vec();
        reloaded[1].status = Status::Ready;
        reloaded[1].params = synth().params;
        assert!(tab.set_plugins(reloaded));
        assert_eq!(tab.row(0), Some(PluginRow::Param(0)));
    }

    /// The watch of the preset folders read them again: the tab owes the
    /// open plugin its presets, and the presets that come keep the place
    /// of the cursor. With no plugin open, no ask is owed.
    #[test]
    fn a_read_past_the_one_of_the_tab_asks_for_its_presets_again() {
        use PluginRow::{Plugin, Preset};
        let mut tab = VstTab::default();
        tab.set_plugins(vec![synth()]);
        assert!(!tab.presets_stale());
        assert!(tab.expand());
        tab.set_presets(vec!["Lead".into()]);
        assert_eq!(tab.presets_due(), None);
        assert!(tab.show_section(VstSection::Presets));
        assert!(tab.presets_stale());
        assert_eq!(tab.presets_due(), Some("Synth"));
        // A read past the one the tab holds again owes nothing more: the
        // ask of the first is still owed.
        assert!(!tab.presets_stale());
        tab.set_presets(vec!["Lead".into(), "Dark".into()]);
        assert_eq!(tab.rows(), [Preset(0), Preset(1)]);
        assert_eq!(
            tab.row(tab.selected),
            Some(Preset(0)),
            "the cursor keeps its preset"
        );
        // The half left, the plugin row and its two rows are the list
        // again.
        assert!(tab.leave());
        assert_eq!(
            tab.rows(),
            [
                Plugin(0),
                PluginRow::Section(VstSection::Presets),
                PluginRow::Section(VstSection::Params)
            ]
        );
    }

    /// The count line of the tab says where the read of the plugin folders
    /// is, while it runs, and says nothing when no read runs. The read of
    /// the preset folders says itself the same way.
    #[test]
    fn the_read_of_the_plugin_folders_says_where_it_is() {
        let mut tab = VstTab::default();
        assert_eq!(tab.scan(), None);
        assert!(tab.set_scan(Some((1, 3))));
        assert!(!tab.set_scan(Some((1, 3))));
        assert_eq!(tab.scan(), Some((1, 3)));
        assert!(tab.set_scan(Some((2, 3))));
        assert!(tab.set_scan(None));
        assert_eq!(tab.scan(), None);
        assert!(!tab.set_presets_reading(false));
        assert!(tab.set_presets_reading(true));
        assert!(tab.presets_reading());
        assert!(!tab.set_presets_reading(true));
        assert!(tab.set_presets_reading(false));
        assert!(!tab.presets_reading());
    }
}
