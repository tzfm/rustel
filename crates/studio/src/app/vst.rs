//! VST3 plugins in the Studio: the plugin folders of the Settings sheet, the
//! plugin list of the reference column, the word lists a completion opens
//! in a plugin call, and the plugin rows of the memory breakdown.
//!
//! The process has one plugin host. The program that runs the Studio starts
//! the host and its plugin scan in the background. With no such start, the
//! host starts when one of the two vst tabs shows, when a completion opens
//! in a plugin call, or when a score names a plugin, and before this the
//! Studio reads no plugin folder. The memory breakdown reads a started host
//! and starts none.

use rustel_runtime::lint::scan;
use rustel_runtime::vst::{self, PluginInfo, Status};

use super::sets::shorten_home;
use super::*;
use crate::memory::PluginMemory;
use crate::reference::{PluginRow, PluginWords, VstTab, key_words};

/// How often the reference column reads the plugin list while its vst tab
/// shows.
const PLUGIN_REFRESH: Duration = Duration::from_millis(1_000);

/// A plugin loads on the plugin thread, and the list tells the column the
/// end of the load. While a plugin loads, the column reads the list 10 times
/// a second.
const PLUGIN_LOADING_REFRESH: Duration = Duration::from_millis(100);

/// The plugin rows of the memory breakdown: each plugin of the host in its
/// load, loaded, or failed. `processes` is [`vst::process_memory`]: the
/// memory of each plugin process, by its process number. One process runs
/// all plugins of a bundle, so the first plugin of a bundle has the figure.
fn plugin_memory(plugins: &[PluginInfo], processes: &[(u32, u64)]) -> Vec<PluginMemory> {
    let mut counted: Vec<&std::path::Path> = Vec::new();
    plugins
        .iter()
        .filter(|plugin| plugin.status != Status::Found)
        .map(|plugin| {
            let name = &plugin.name;
            let label = match &plugin.status {
                Status::Ready => format!(
                    "{name} · {} · {} running · {} ms",
                    if plugin.instrument {
                        "instrument"
                    } else {
                        "effect"
                    },
                    plugin.running,
                    plugin.load_time.as_millis()
                ),
                Status::Failed(reason) => format!("{name} · failed: {reason}"),
                Status::Found | Status::Loading => format!("{name} · loading"),
            };
            let first = !counted.contains(&plugin.bundle.as_path());
            counted.push(&plugin.bundle);
            let measured = |id: u32| processes.iter().find(|(process, _)| *process == id);
            PluginMemory {
                label,
                process: plugin
                    .process
                    .and_then(measured)
                    .map(|(_, bytes)| if first { *bytes } else { 0 }),
                loaded: plugin.status == Status::Ready,
            }
        })
        .collect()
}

/// A plugin name or a preset name as a word list writes the name into a
/// string of the score. A quote or a backslash would end or change the
/// string. The host finds a name with no punctuation too.
fn in_a_string(name: String) -> String {
    name.replace(['"', '\'', '`', '\\'], "")
}

/// The words of a word list of a plugin call, and the text beside each
/// word, as the host has them now. The host starts on first use. A key
/// list and a preset list ask for the load of their plugin, and have the
/// words of the plugin after the load. No call waits.
fn plugin_words(words: &PluginWords) -> (Vec<String>, Vec<String>) {
    let host = vst::host();
    let loaded = |name: &str| match host.resolve(name, false) {
        vst::Resolved::Ready(_, plugin) => Some(plugin),
        _ => None,
    };
    match words {
        // A load or a test tells an instrument from an effect, so a bundle
        // with none of the two yet is in the 2 lists.
        PluginWords::Names { instrument } => {
            let listed = host
                .plugins()
                .into_iter()
                .filter(|plugin| plugin.categories.is_empty() || plugin.instrument == *instrument);
            (
                listed.map(|plugin| in_a_string(plugin.name)).collect(),
                Vec::new(),
            )
        }
        PluginWords::Keys(name) => {
            key_words(loaded(name).as_ref().map_or(&[], |plugin| plugin.params()))
        }
        // The preset folder has the name of the plugin, not the name the
        // score wrote.
        PluginWords::Presets(name) => {
            let presets = loaded(name).and_then(|plugin| host.presets(plugin.name()));
            let presets = presets.unwrap_or_default().into_iter().map(in_a_string);
            (presets.collect(), Vec::new())
        }
    }
}

/// The nearest plugin call, including a caret on its name or inside a
/// nested parameter value. A string or a comment opens no call.
fn plugin_call(source: &str, caret: usize) -> Option<(String, usize)> {
    let code = rustel_runtime::lint::code_only(source);
    let typed = scan::name_ending_at(&code, caret);
    let name = scan::name_starting_at(&code, typed.start);
    let on_call = matches!(code.get(name.clone()), Some("vst" | "vsti"))
        .then(|| name.end + code[name.end..].len() - code[name.end..].trim_start().len())
        .filter(|open| code.as_bytes().get(*open) == Some(&b'('));
    on_call
        .into_iter()
        .chain(scan::open_parens(&code, caret))
        .find_map(|open| {
            let name = scan::name_before(&code, open)?;
            if !matches!(&code[name], "vst" | "vsti") {
                return None;
            }
            let name = rustel_runtime::sounds::string_argument(&source[open + 1..])?;
            (!name.trim().is_empty()).then(|| (name.into_owned(), open))
        })
}

/// The separators of one bracketed argument/object list, ending with its
/// closing bracket. `code_only` keeps punctuation in strings and comments
/// from being mistaken for a separator. A typed key sometimes ends at EOF,
/// before the player closes the list.
fn members(code: &str, open: usize, unclosed: bool) -> Option<Vec<Range<usize>>> {
    let mut brackets = vec![*code.as_bytes().get(open)?];
    let mut start = open + 1;
    let mut ranges = Vec::new();
    for (at, byte) in code.bytes().enumerate().skip(start) {
        match byte {
            b'(' | b'[' | b'{' => brackets.push(byte),
            b')' | b']' | b'}' => {
                if !matches!(
                    (brackets.pop()?, byte),
                    (b'(', b')') | (b'[', b']') | (b'{', b'}')
                ) {
                    return None;
                }
                if brackets.is_empty() {
                    ranges.push(start..at);
                    return Some(ranges);
                }
            }
            b',' if brackets.len() == 1 => {
                ranges.push(start..at);
                start = at + 1;
            }
            _ => {}
        }
    }
    unclosed.then(|| {
        ranges.push(start..code.len());
        ranges
    })
}

/// Add one parameter to the call's options, or select its existing key for
/// an unchanged replacement. Existing values, nested expressions, comments
/// and trailing commas stay intact.
fn parameter_edit(
    source: &str,
    open: usize,
    text: &str,
    typed: Option<Range<usize>>,
) -> Option<(Range<usize>, String)> {
    let code = rustel_runtime::lint::code_only(source);
    let args = members(&code, open, typed.is_some())?;
    let name = args.first()?;
    let key = &text[..rustel_runtime::lint::code_only(text).find(':')?];
    let plain_key = |mut key: &str| {
        // A leading comment sits before either an identifier or a quoted
        // property. Trailing comments are blank in the identifier path.
        loop {
            key = key.trim_start();
            if let Some(comment) = key.strip_prefix("/*") {
                key = comment.get(comment.find("*/")? + 2..)?;
            } else if let Some(comment) = key.strip_prefix("//") {
                key = comment.get(comment.find('\n')?..)?;
            } else {
                break;
            }
        }
        Some(rustel_runtime::sounds::string_argument(key).map_or_else(
            || rustel_runtime::lint::code_only(key).trim().to_owned(),
            |key| key.into_owned(),
        ))
    };
    let Some(options) = args.get(1) else {
        // The name is a literal. Its end precedes any trailing comment.
        let literal = source[name.clone()].trim_start();
        let quote = *literal.as_bytes().first()?;
        let mut escaped = false;
        let end = literal.bytes().enumerate().skip(1).find_map(|(at, byte)| {
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == quote {
                return Some(at + 1);
            }
            None
        })?;
        let at = name.end - literal.len() + end;
        if !code[at..name.end].trim().is_empty() {
            return None;
        }
        return Some((at..at, format!(", {{ {text} }}")));
    };
    let object = code[options.clone()]
        .bytes()
        .position(|byte| !byte.is_ascii_whitespace());
    let Some(object) = object.map(|at| options.start + at) else {
        return Some((options.start..options.start, format!(" {{ {text} }}")));
    };
    if code.as_bytes()[object] != b'{' {
        return None;
    }
    let entries = members(&code, object, typed.is_some())?;
    let last = entries.last()?;
    if last.end < options.end && !code[last.end + 1..options.end].trim().is_empty() {
        return None;
    }
    let typed_entry = typed.as_ref().and_then(|typed| {
        entries
            .iter()
            .position(|entry| entry.start <= typed.start && typed.end <= entry.end)
    });
    let existing = entries
        .iter()
        .enumerate()
        .filter(|(_, entry)| {
            let raw = source[(*entry).clone()].trim();
            let colon = code[(*entry).clone()].find(':');
            let existing = colon.map_or(raw, |at| source[entry.start..entry.start + at].trim());
            plain_key(existing).is_some_and(|existing| Some(existing) == plain_key(key))
        })
        // Prefer a complete member elsewhere over the key being typed.
        .min_by_key(|(index, _)| Some(*index) == typed_entry);
    if let Some((index, entry)) = existing {
        if let Some(typed) = &typed
            && Some(index) == typed_entry
            && !code[entry.clone()].contains(':')
        {
            return Some((typed.clone(), text.to_owned()));
        }
        if typed.as_ref().is_some_and(|typed| !typed.is_empty())
            && let Some(partial) = typed_entry.filter(|partial| *partial != index)
        {
            // The chosen key already has a value elsewhere. Remove
            // the unfinished member and one adjacent comma.
            let range = if partial > 0 {
                entries[partial].start - 1..entries[partial].end
            } else {
                entries[partial].start..entries[partial].end + 1
            };
            return Some((range, String::new()));
        }
        return Some((entry.clone(), source[entry.clone()].to_owned()));
    }
    if let Some(typed) = typed {
        let entry = entries
            .iter()
            .find(|entry| entry.start <= typed.start && typed.end <= entry.end)?;
        let value = code[typed.end..entry.end].trim_start().starts_with(':');
        return Some((typed, if value { key } else { text }.to_owned()));
    }
    // Inserting before an existing member avoids its trailing line comment
    // swallowing the new value and preserves the object's indentation.
    let empty = entries.len() == 1
        && code[last.clone()].trim().is_empty()
        && source[last.clone()].trim().is_empty();
    let rest = &source[object + 1..];
    let leading = &rest[..rest.len() - rest.trim_start().len()];
    let at = object + 1 + leading.len();
    let prefix = if leading.is_empty() { " " } else { "" };
    let suffix = if empty {
        " ".to_owned()
    } else if leading.is_empty() {
        ", ".to_owned()
    } else {
        format!(",{leading}")
    };
    Some((at..at, format!("{prefix}{text}{suffix}")))
}

impl App {
    /// The word list of a plugin call, searched for `query`.
    pub(super) fn plugin_words_panel(&self, words: PluginWords, query: &str) -> ReferencePanel {
        let (names, details) = plugin_words(&words);
        ReferencePanel::plugin_words_for(&self.reference, words, names, details, query)
    }

    /// The plugin of the `.vst()` or `.vsti()` call the caret is in, and the
    /// offset of the bracket of the call. The plugin is the first argument
    /// of the call, when the argument is a quoted string.
    pub(super) fn plugin_call_at_caret(&self) -> Option<(String, usize)> {
        plugin_call(
            &self.editor().source(),
            self.editor().primary_selection().head.0,
        )
    }

    /// Anywhere else in a named plugin call, open its parameters directly.
    /// Name and preset strings keep their more specific completion lists.
    pub(super) fn open_plugin_parameters(&mut self) -> bool {
        if self.plugin_call_at_caret().is_none() {
            return false;
        }
        let mut panel = ReferencePanel::browse(&self.reference);
        panel.select_tab(Tab::Vst);
        self.reference_anchor = None;
        self.set_reference_panel(Some(panel));
        self.sync_plugins();
        self.install_catalogue();
        self.status = "plugin parameters - Enter adds the choice to this call".into();
        self.focus_panel(PanelKind::Reference);
        self.invalidate_maps();
        self.dirty_frame = true;
        true
    }

    /// The plugin and the range of the key at the caret, when the caret is
    /// where a key goes in the object of a plugin call: after `{` or `,`,
    /// with the start of a key or nothing typed.
    fn plugin_key_at_caret(&self) -> Option<(String, Range<ByteOffset>)> {
        let (plugin, open) = self.plugin_call_at_caret()?;
        let caret = self.editor().primary_selection().head.0;
        let code = rustel_runtime::lint::code_only(self.editor().source().get(..caret)?);
        // The object is the one bracket open between the call and the
        // caret. A string or a comment at the caret is blank here, and
        // takes no key.
        let mut brackets = Vec::new();
        for byte in code.bytes().skip(open + 1) {
            match byte {
                b'(' | b'[' | b'{' => brackets.push(byte),
                b')' | b']' | b'}' => {
                    brackets.pop();
                }
                _ => {}
            }
        }
        let typed = scan::name_ending_at(&code, caret);
        let before = code[..typed.start].trim_end();
        let key_place = before.ends_with('{') || before.ends_with(',');
        // The key runs to its end after the caret: a choice replaces all
        // of the key.
        let rest = &self.editor().source()[caret..];
        let named = |char: char| char.is_alphanumeric() || char == '_';
        let end = caret + rest.find(|char| !named(char)).unwrap_or(rest.len());
        (brackets == *b"{" && key_place && self.string_completion_at_caret().is_none())
            .then_some((plugin, ByteOffset(typed.start)..ByteOffset(end)))
    }

    /// Ctrl+Space in the object of a plugin call, where a key goes: the
    /// list of the parameter keys of the plugin, and `preset`. Enter fills
    /// the key and its default value. False in each other place.
    pub(super) fn open_plugin_key_completion(&mut self) -> bool {
        let Some((plugin, typed)) = self.plugin_key_at_caret() else {
            return false;
        };
        let word = self
            .editor()
            .document()
            .slice(typed.clone())
            .unwrap_or_default();
        let panel = self.plugin_words_panel(PluginWords::Keys(plugin), &word);
        self.reference_anchor = None;
        self.anchor_reference(&word, typed);
        self.set_reference_panel(Some(panel));
        self.install_catalogue();
        self.status = "completion - Enter puts the chosen name in place".into();
        self.focus_panel(PanelKind::Reference);
        self.invalidate_maps();
        self.dirty_frame = true;
        true
    }

    /// Reads the plugin rows of the memory breakdown. The breakdown starts
    /// no host. The memory of the plugin processes is a read of `/proc` on
    /// Linux, so this runs when the breakdown opens and 2 times a second
    /// while the breakdown shows, and not for each frame.
    pub(super) fn measure_plugins(&mut self) {
        let plugins = vst::started().map(vst::Host::plugins).unwrap_or_default();
        let measured = if plugins.iter().all(|plugin| plugin.status == Status::Found) {
            Vec::new()
        } else {
            plugin_memory(&plugins, &vst::process_memory())
        };
        if measured != self.plugin_memory {
            self.plugin_memory = measured;
            self.dirty_frame = true;
        }
    }

    /// Gives the host the folders the player added. Before the host starts,
    /// this stores the folders and reads no folder.
    pub(super) fn adopt_vst_folders(&self) {
        vst::set_user_folders(self.prefs.vst_folders.iter().map(PathBuf::from).collect());
    }

    /// Tells the sheet how far the cursor of the vst page goes.
    pub(super) fn count_vst_folders(&self, sheet: &mut SettingsSheet) {
        sheet.vst_folder_count = self.prefs.vst_folders.len();
        sheet.vst_standard_count = vst::standard_folders().len();
    }

    /// The vst page as the Settings sheet reads the page. Empty while a
    /// different page shows, so the host starts with the page.
    pub(super) fn vst_page(&self) -> settings::VstPage {
        if !self.settings_sheet.is_some_and(|sheet| sheet.shows_vst()) {
            return settings::VstPage::default();
        }
        let folder = |path: PathBuf, standard| settings::VstFolder {
            path: shorten_home(&path),
            standard,
        };
        let added = self.prefs.vst_folders.iter().map(PathBuf::from);
        settings::VstPage {
            folders: added
                .map(|path| folder(path, false))
                .chain(
                    vst::standard_folders()
                        .into_iter()
                        .map(|path| folder(path, true)),
                )
                .collect(),
            plugins: vst::host().plugins().len(),
            presets: vst::presets_folder()
                .map(|presets| shorten_home(&presets))
                .unwrap_or_default(),
        }
    }

    /// The folder the picker gave goes on the list, and the host reads all
    /// folders again. The cursor goes to the new row.
    pub(super) fn add_vst_folder(&mut self, path: &std::path::Path) {
        if !path.is_dir() {
            if let Some((_, picker)) = self.set_prompt.as_mut() {
                picker.error = Some(format!("cannot use {}: not a folder", path.display()));
            }
            return;
        }
        self.close_set_prompt();
        // The folder is kept as a full path: a later start in a different
        // folder reads the same place.
        let path = &std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
        let label = status_file_path(path, self.ui_settings.show_full_paths);
        let folder = path.to_string_lossy().into_owned();
        let known = self.prefs.vst_folders.iter().position(|at| *at == folder);
        let row = known.unwrap_or(self.prefs.vst_folders.len());
        if known.is_none() {
            self.prefs.vst_folders.push(folder);
            self.save_prefs_soon();
            self.adopt_vst_folders();
        }
        if let Some(sheet) = self.settings_sheet.as_mut() {
            sheet.select(settings::VST_CONTROL_COUNT + row);
        }
        self.status = if known.is_some() {
            format!("{label} is on the list")
        } else {
            format!("{label} added")
        };
        self.dirty_frame = true;
    }

    /// `d` on a folder the player added.
    pub(super) fn remove_vst_folder(&mut self, at: usize) {
        if at >= self.prefs.vst_folders.len() {
            return;
        }
        let gone = PathBuf::from(self.prefs.vst_folders.remove(at));
        self.save_prefs_soon();
        self.adopt_vst_folders();
        self.status = format!(
            "{} removed",
            status_file_path(&gone, self.ui_settings.show_full_paths)
        );
    }

    /// Enter on the rescan row. The scan cache is empty after, so each
    /// plugin gets a new test, a failed plugin too. The host reads the
    /// folders on its own thread, and the page shows the count at the end
    /// of the read.
    pub(super) fn rescan_vst(&mut self) {
        vst::rescan();
        self.status = "reading the plugin folders".to_owned();
    }

    /// The plugins the reference column shows now: the open plugin of the
    /// vst tab, and the plugin of a key list or a preset list. The engine
    /// keeps them loaded: the column asks the host for the load of a shown
    /// plugin, so an unload starts the load again.
    pub(super) fn plugins_shown(&self) -> Vec<String> {
        let Some(panel) = &self.reference_panel else {
            return Vec::new();
        };
        let listed = match panel.plugin_words() {
            Some(PluginWords::Keys(name) | PluginWords::Presets(name)) => Some(name.clone()),
            _ => None,
        };
        let open = (panel.tab == Tab::Vst)
            .then(|| panel.vst.open_plugin())
            .flatten()
            .map(|plugin| plugin.name.clone());
        listed.into_iter().chain(open).collect()
    }

    /// The text of a row of the vst tab, as the text goes in at the caret.
    /// The text is no spelling of the word the column opened on, so the
    /// anchor goes. After a dot the plugin call has its dot.
    pub(super) fn plugin_text_at_caret(&mut self, text: String) -> Option<String> {
        self.reference_anchor = None;
        let keys = self.reference_panel.as_ref().is_some_and(|panel| {
            panel.tab == Tab::Reference
                && matches!(panel.plugin_words(), Some(PluginWords::Keys(_)))
        });
        if keys && let Some((plugin, typed)) = self.plugin_key_at_caret() {
            let params = match vst::host().resolve(&plugin, false) {
                vst::Resolved::Ready(_, plugin) => plugin.params().to_vec(),
                _ => Vec::new(),
            };
            let text = VstTab::key_text(&params, &text)?;
            let (_, open) = self.plugin_call_at_caret()?;
            let source = self.editor().source();
            let typed = typed.start.0..typed.end.0;
            let (range, replacement) = parameter_edit(&source, open, &text, Some(typed))?;
            let word = source[range.clone()].to_owned();
            self.anchor_reference(&word, ByteOffset(range.start)..ByteOffset(range.end));
            return Some(replacement);
        }
        let parameter = self.reference_panel.as_ref().is_some_and(|panel| {
            matches!(
                panel.vst.row(panel.vst.selected),
                Some(PluginRow::Param(_) | PluginRow::Preset(_))
            )
        });
        if parameter && let Some((_, open)) = self.plugin_call_at_caret() {
            let Some((range, replacement)) =
                parameter_edit(&self.editor().source(), open, &text, None)
            else {
                self.status =
                    "plugin parameters need a closed call with a literal options object".into();
                self.dirty_frame = true;
                return None;
            };
            let word = self.editor().source()[range.clone()].to_owned();
            self.anchor_reference(&word, ByteOffset(range.start)..ByteOffset(range.end));
            return Some(replacement);
        }
        let caret = self.editor().primary_selection().head.0;
        Some(match text.strip_prefix('.') {
            Some(call) if self.editor().source()[..caret].ends_with('.') => call.to_owned(),
            _ => text,
        })
    }

    /// The turn loop reads the plugin list for the vst tab at
    /// [`PLUGIN_REFRESH`], or at [`PLUGIN_LOADING_REFRESH`] while the list
    /// has a plugin in its load. A word list of a plugin call reads the
    /// host at the fast rate: its words come with the end of a load.
    pub(super) fn poll_plugins(&mut self, now: Instant) {
        // The host reads the plugin folders on its own thread. The end of
        // a read changes the plugin count of the Settings page.
        let scanning = vst::scanning();
        if scanning != self.plugins_scanning {
            self.plugins_scanning = scanning;
            self.dirty_frame = true;
        }
        // The list of background jobs has a row for the scan and a row
        // for each load. A test of the scan gives the list new names.
        let (loads, scan) = (vst::loads(), vst::scan_progress());
        if loads != self.plugin_loads || scan != self.plugin_scan {
            self.plugin_loads = loads;
            self.plugin_scan = scan;
            self.dirty_frame = true;
        }
        let loading = scanning
            || self
                .reference_panel
                .as_ref()
                .is_some_and(|panel| panel.vst.loading() || panel.plugin_words().is_some());
        let refresh = if loading {
            PLUGIN_LOADING_REFRESH
        } else {
            PLUGIN_REFRESH
        };
        if now.duration_since(self.plugins_polled_at) >= refresh {
            self.sync_plugins();
        }
    }

    /// Reads the plugin list into the reference column while its vst tab
    /// shows, and asks the host to load the open plugin. A word list of a
    /// plugin call gets its words again. No call waits for a load. The
    /// frame is stale only after a change.
    pub(super) fn sync_plugins(&mut self) {
        let Some(panel) = self.reference_panel.as_mut() else {
            return;
        };
        if panel.tab == Tab::Reference
            && let Some(words) = panel.plugin_words()
        {
            self.plugins_polled_at = Instant::now();
            let (names, details) = plugin_words(words);
            self.dirty_frame |= panel.set_plugin_words(&self.reference, names, details);
            return;
        }
        if panel.tab != Tab::Vst {
            return;
        }
        self.plugins_polled_at = Instant::now();
        let host = vst::host();
        if let Some(name) = panel.vst.plugin_to_load() {
            host.resolve(name, false);
        }
        let mut changed = panel.vst.set_plugins(host.plugins());
        // The host reads the preset folder on its own thread. The tab asks
        // again on the next read of the list until the names are here.
        if let Some(presets) = panel.vst.presets_due().and_then(|name| host.presets(name)) {
            panel.vst.set_presets(presets);
            changed = true;
        }
        self.dirty_frame |= changed;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::{self, MemoryFigures};

    #[test]
    fn a_plugin_call_contains_its_name_and_nested_values() {
        let source = "s('bd').vst ('Kickstart 2').vsti(\"Synth\", {cutoff: saw.range(.1, .5)})";
        for (needle, expected) in [
            ("vs", "Kickstart 2"),
            ("Kick", "Kickstart 2"),
            ("vsti", "Synth"),
            ("Synth", "Synth"),
            ("cutoff", "Synth"),
            (".1", "Synth"),
            (".5)", "Synth"),
        ] {
            let at = source.find(needle).expect(needle);
            let (name, _) = plugin_call(source, at + needle.len()).expect(needle);
            assert_eq!(name, expected, "{needle}");
        }
        assert!(
            plugin_call(source, source.len()).is_none(),
            "after the call is a new link"
        );
        assert!(plugin_call("s('vst(\"fake\")')", 10).is_none());
        assert!(plugin_call("// vst('fake')", 9).is_none());
    }

    #[test]
    fn a_parameter_creates_or_merges_options_without_replacing_values() {
        for (source, expected) in [
            ("vst('FX')", "vst('FX', { gain: 0.5 })"),
            (
                "vsti('FX' /* a name */)",
                "vsti('FX', { gain: 0.5 } /* a name */)",
            ),
            ("vst('FX',)", "vst('FX', { gain: 0.5 })"),
            ("vst('FX', {})", "vst('FX', { gain: 0.5 })"),
            ("vst('FX', { })", "vst('FX', { gain: 0.5 })"),
            ("vst('FX', {wet: .6})", "vst('FX', { gain: 0.5, wet: .6})"),
            (
                "vst('FX', { wet: .6, })",
                "vst('FX', { gain: 0.5, wet: .6, })",
            ),
            (
                "vst('FX', {\n  wet: saw.range(0, 1) // keep\n})",
                "vst('FX', {\n  gain: 0.5,\n  wet: saw.range(0, 1) // keep\n})",
            ),
            (
                "vst('FX', { gain: saw.range(.1, .9) })",
                "vst('FX', { gain: saw.range(.1, .9) })",
            ),
            ("vst('FX', { 'gain': .7 })", "vst('FX', { 'gain': .7 })"),
            ("vst('FX', { \"gain\": .7 })", "vst('FX', { \"gain\": .7 })"),
            ("vst('FX', { gain })", "vst('FX', { gain })"),
            (
                "vst('FX', { /* level */ gain /* value */: .7 })",
                "vst('FX', { /* level */ gain /* value */: .7 })",
            ),
            (
                "vst('FX', { // level\n 'gain': .7 })",
                "vst('FX', { // level\n 'gain': .7 })",
            ),
        ] {
            let open = source.find('(').unwrap();
            let (range, text) = parameter_edit(source, open, "gain: 0.5", None).unwrap();
            let mut result = source.to_owned();
            result.replace_range(range, &text);
            assert_eq!(result, expected, "{source}");
        }
        let source = "vst('FX', { 'gain:left': .7 })";
        let (range, text) = parameter_edit(source, 3, "'gain:left': .5", None).unwrap();
        assert_eq!(&source[range], text);
        for source in [
            "vst('FX', options)",
            "vst('FX', {",
            "vst('FX'",
            "vst('FX', { gain: .7)",
            "vst('FX' + suffix)",
            "vst('FX', {} || other)",
        ] {
            assert!(
                parameter_edit(source, source.find('(').unwrap(), "gain: 0.5", None).is_none(),
                "{source}"
            );
        }
    }

    #[test]
    fn a_parameter_key_completion_keeps_values_and_removes_duplicate_partial_keys() {
        for (marked, expected) in [
            ("vst('FX', { |ga| })", "vst('FX', { gain: 0.5 })"),
            ("vst('FX', { |ga|", "vst('FX', { gain: 0.5"),
            ("vst('FX', { |gain| })", "vst('FX', { gain: 0.5 })"),
            ("vst('FX', { |ga|: .2 })", "vst('FX', { gain: .2 })"),
            ("vst('FX', { |ga|: .2", "vst('FX', { gain: .2"),
            ("vst('FX', { |ga|: .2 }", "vst('FX', { gain: .2 }"),
            ("vst('FX', { |gain|: .2 })", "vst('FX', { gain: .2 })"),
            ("vst('FX', { |gain|: .2", "vst('FX', { gain: .2"),
            (
                "vst('FX', { |ga|, 'gain': saw.range(0, 1) })",
                "vst('FX', { 'gain': saw.range(0, 1) })",
            ),
            (
                "vst('FX', { |ga|, 'gain': saw.range(0, 1)",
                "vst('FX', { 'gain': saw.range(0, 1)",
            ),
            ("vst('FX', { |gain|, gain: .2 })", "vst('FX', { gain: .2 })"),
            ("vst('FX', { gain: .2, |ga| })", "vst('FX', { gain: .2})"),
            ("vst('FX', { gain: .2, |ga|", "vst('FX', { gain: .2"),
        ] {
            let typed = marked.find('|').unwrap()..marked.rfind('|').unwrap() - 1;
            let source = marked.replace('|', "");
            let (range, text) = parameter_edit(&source, 3, "gain: 0.5", Some(typed)).unwrap();
            let mut result = source.clone();
            result.replace_range(range, &text);
            assert_eq!(result, expected, "{source}");
        }
    }

    /// The memory breakdown has one row for each plugin the host loaded, and
    /// no row for a plugin the host only found. A plugin process has a
    /// figure, on the first plugin of its bundle. A plugin in the Studio
    /// process has no figure of its own. An unload takes the rows away.
    #[test]
    fn a_loaded_plugin_is_a_row_of_the_memory_breakdown() {
        let folder = tempfile::tempdir().unwrap();
        rustel_vst3_fixture::install(folder.path());
        // A host of its own reads this folder only: no plugin of the
        // machine, and no other test changes its list.
        let host = vst::Host::new();
        host.scan(&[folder.path().to_path_buf()]);
        host.wait_idle();
        assert!(plugin_memory(&host.plugins(), &[]).is_empty());

        host.resolve(rustel_vst3_fixture::NAME, true);
        // As with a worker: the 2 plugins of the bundle run in process 7.
        let mut plugins = host.plugins();
        for plugin in &mut plugins {
            plugin.process = Some(7);
        }
        let figures = MemoryFigures {
            plugins: plugin_memory(&plugins, &[(7, 3 << 20)]),
            ..MemoryFigures::default()
        };
        let rows = memory::rows(&figures);
        let section = rows.iter().position(|row| row.label == "plugins");
        let [heading, effect, tone, ..] = &rows[section.expect("a plugin section")..] else {
            panic!("2 plugin rows under the heading: {rows:#?}");
        };
        assert_eq!(heading.value, "2 loaded");
        let facts = "Rustel Fixture · effect · 0 running · ";
        assert!(effect.label.starts_with(facts), "{}", effect.label);
        assert_eq!(effect.value, "3.00MB");
        let facts = "Rustel Fixture Tone · instrument · 0 running · ";
        assert!(tone.label.starts_with(facts), "{}", tone.label);
        assert_eq!(tone.value, "same process");

        host.unload_unused(&[]);
        host.wait_idle();
        assert!(plugin_memory(&host.plugins(), &[]).is_empty());
    }
}
