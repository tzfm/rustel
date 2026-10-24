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

use super::args::parameter_edit;
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
/// load, loaded, or failed.
fn plugin_memory(plugins: &[PluginInfo]) -> Vec<PluginMemory> {
    plugins
        .iter()
        .filter(|plugin| plugin.status != Status::Found)
        .map(|plugin| {
            let name = &plugin.name;
            let label = match &plugin.status {
                Status::Ready => {
                    // A plugin with no latency shows none.
                    let latency = plugin
                        .latency
                        .filter(|frames| *frames > 0)
                        .map(|frames| format!(" · latency {frames} samples"))
                        .unwrap_or_default();
                    format!(
                        "{name} · {} · {} running · {} ms{latency}",
                        if plugin.instrument {
                            "instrument"
                        } else {
                            "effect"
                        },
                        plugin.running,
                        plugin.load_time.as_millis()
                    )
                }
                Status::Failed(reason) => format!("{name} · failed: {reason}"),
                Status::Found | Status::Loading => format!("{name} · loading"),
            };
            PluginMemory {
                label,
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

/// The row a letter jumps to on the vst page: the next plugin that begins
/// with it, from `at` and around the list. The row `at` itself counts as
/// visited, so the same letter again goes to the next of its plugins.
/// `None` when no plugin begins with the letter.
fn jump_row(names: &[&str], at: usize, letter: char) -> Option<usize> {
    (at + 1..at + 1 + names.len())
        .find(|row| {
            names[row % names.len()]
                .chars()
                .next()
                .is_some_and(|first| first.eq_ignore_ascii_case(&letter))
        })
        .map(|row| row % names.len())
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
    /// no host. This runs when the breakdown opens and 2 times a second
    /// while the breakdown shows, and not for each frame.
    pub(super) fn measure_plugins(&mut self) {
        let plugins = vst::started().map(vst::Host::plugins).unwrap_or_default();
        let measured = plugin_memory(&plugins);
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

    /// Gives the host the preset folders the player added, and the host
    /// reads the preset folders again.
    pub(super) fn adopt_preset_folders(&self) {
        vst::set_preset_folders(
            self.prefs
                .preset_folders
                .iter()
                .map(PathBuf::from)
                .collect(),
        );
    }

    /// Tells the sheet how far the cursor of the vst page goes.
    pub(super) fn count_vst_folders(&self, sheet: &mut SettingsSheet) {
        sheet.vst_folder_count = self.prefs.vst_folders.len();
        sheet.vst_standard_count = vst::standard_folders().len();
        sheet.vst_preset_folder_count = self.prefs.preset_folders.len();
        sheet.vst_scanned_count = vst::host().plugins().len();
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
        let plugins = vst::host().plugins();
        settings::VstPage {
            folders: added
                .map(|path| folder(path, false))
                .chain(
                    vst::standard_folders()
                        .into_iter()
                        .map(|path| folder(path, true)),
                )
                .collect(),
            preset_folders: self
                .prefs
                .preset_folders
                .iter()
                .map(|path| folder(PathBuf::from(path), false))
                .collect(),
            scanned: plugins
                .iter()
                .map(|plugin| settings::VstScanned {
                    name: plugin.name.clone(),
                    state: match &plugin.status {
                        Status::Ready => "ready".to_owned(),
                        Status::Loading => "loading".to_owned(),
                        Status::Found => "found".to_owned(),
                        Status::Failed(reason) => {
                            let reason = reason.lines().next().unwrap_or_default();
                            format!("failed · {reason}")
                        }
                    },
                    audio_thread: plugin.audio_thread,
                    presets: plugin.presets,
                })
                .collect(),
            plugins: plugins.len(),
            scan: self.plugin_scan.clone(),
            open_presets: self.settings_vst_open_presets.clone(),
        }
    }

    /// A preset file dropped on the window: the host copies it into the
    /// preset folder of its plugin, and the status line says where.
    pub(super) fn add_dropped_preset(&mut self, path: &std::path::Path) {
        let file = path.file_name().unwrap_or_default().to_string_lossy();
        let (level, said) = match vst::host().add_preset(path) {
            Ok((plugin, preset)) => (
                LogLevel::Info,
                format!("preset '{preset}' added to {plugin}"),
            ),
            Err(reason) => (LogLevel::Warn, format!("{file}: {reason}")),
        };
        self.log.push(level, "drop", said.clone());
        self.status = said;
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

    /// The folder the picker gave goes on the list, and the host reads the
    /// preset folders again. The cursor goes to the new row.
    pub(super) fn add_preset_folder(&mut self, path: &std::path::Path) {
        if !path.is_dir() {
            if let Some((_, picker)) = self.set_prompt.as_mut() {
                picker.error = Some(format!("cannot use {}: not a folder", path.display()));
            }
            return;
        }
        self.close_set_prompt();
        let path = &std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
        let label = status_file_path(path, self.ui_settings.show_full_paths);
        let folder = path.to_string_lossy().into_owned();
        let known = self
            .prefs
            .preset_folders
            .iter()
            .position(|at| *at == folder);
        let row = known.unwrap_or(self.prefs.preset_folders.len());
        if known.is_none() {
            self.prefs.preset_folders.push(folder);
            self.save_prefs_soon();
            self.adopt_preset_folders();
        }
        if let Some(sheet) = self.settings_sheet.as_mut() {
            // The page counts one row more, and the cursor lands on it:
            // the keys of the page reach the new row.
            sheet.vst_preset_folder_count = self.prefs.preset_folders.len();
            sheet.select(
                settings::VST_CONTROL_COUNT
                    + self.prefs.vst_folders.len()
                    + sheet.vst_standard_count
                    + row,
            );
        }
        self.status = if known.is_some() {
            format!("{label} is on the list")
        } else {
            format!("{label} added")
        };
        self.dirty_frame = true;
    }

    /// `d` on a preset folder the player added.
    pub(super) fn remove_preset_folder(&mut self, at: usize) {
        if at >= self.prefs.preset_folders.len() {
            return;
        }
        let gone = PathBuf::from(self.prefs.preset_folders.remove(at));
        self.save_prefs_soon();
        self.adopt_preset_folders();
        self.status = format!(
            "{} removed",
            status_file_path(&gone, self.ui_settings.show_full_paths)
        );
    }

    /// Enter on a plugin row: the row opens its presets, the names of the
    /// `.vstpreset` files the folders hold for the plugin. The names come
    /// with the read of the folders: see [`Self::poll_plugins`].
    pub(super) fn toggle_vst_presets(&mut self, sheet: &mut SettingsSheet, at: usize) {
        if sheet.vst_open == Some(at) {
            sheet.vst_open = None;
            sheet.vst_presets_due = false;
        } else {
            sheet.vst_open = Some(at);
            sheet.vst_presets_due = true;
        }
        sheet.vst_open_count = 0;
        self.settings_vst_open_presets.clear();
        self.dirty_frame = true;
    }

    /// A letter on the vst page: the cursor goes to the next plugin whose
    /// name begins with it, from the row the cursor holds and around the
    /// list, so `s` reaches Serum and then the rest of the s's. The names
    /// come from the host, which the page lists in their order.
    pub(super) fn jump_vst_plugin(&mut self, sheet: &mut SettingsSheet, letter: char) {
        let plugins = vst::host().plugins();
        let names: Vec<&str> = plugins.iter().map(|plugin| plugin.name.as_str()).collect();
        let Some(last) = names.len().checked_sub(1) else {
            return;
        };
        let base = settings::VST_CONTROL_COUNT
            + sheet.vst_folder_count
            + sheet.vst_standard_count
            + sheet.vst_preset_folder_count;
        let at = sheet.selected.saturating_sub(base).min(last);
        match jump_row(&names, at, letter) {
            Some(row) => sheet.select(base + row),
            None => {
                self.status = format!("no plugin begins with '{}'", letter.to_ascii_lowercase())
            }
        }
        self.dirty_frame = true;
    }

    /// What the status line says while the host reads the plugin folders.
    /// The end of the read retires it: see [`Self::poll_plugins`].
    const READING_THE_FOLDERS: &str = "reading the plugin folders";

    /// Enter on the scan row. A plugin with an answer in the scan cache
    /// for the files it has now gets no new test, a failed plugin too.
    pub(super) fn scan_vst(&mut self) {
        vst::scan();
        self.status = Self::READING_THE_FOLDERS.to_owned();
    }

    /// Enter on the rescan row. The scan cache is empty after, so each
    /// plugin gets a new test, a failed plugin too. The host reads the
    /// folders on its own thread, and the page shows the count at the end
    /// of the read.
    pub(super) fn rescan_vst(&mut self) {
        vst::rescan();
        self.status = Self::READING_THE_FOLDERS.to_owned();
    }

    /// Space on a plugin row: the switch `audio thread only` of the plugin.
    /// The runtime keeps the switch in the user settings, so `rustel play`
    /// follows it too. A copy that loads after the change follows it. A row
    /// before the load or the test of its plugin takes no switch.
    pub(super) fn toggle_audio_thread(&mut self, at: usize) {
        let plugins = vst::host().plugins();
        let Some(plugin) = plugins.get(at) else {
            return;
        };
        // Before its load or its test, a row has the name of its bundle.
        // The test gives the row the name of the plugin, and a switch saved
        // under the bundle name then names no plugin.
        if plugin.status != Status::Ready && plugin.categories.is_empty() {
            self.status = format!(
                "{} · no switch before the load or the test of the plugin",
                plugin.name
            );
            return;
        }
        self.status = match vst::toggle_audio_thread_only(&plugin.name) {
            Ok(Some(on)) => {
                let state = if on { "on" } else { "off" };
                let name = &plugin.name;
                format!("{name} · audio thread only {state} · for the copies that load from now")
            }
            Ok(None) => format!(
                "{} saved · {} decides while it is set",
                plugin.name,
                vst::AUDIO_THREAD_ENV
            ),
            Err(error) => {
                let said = format!("{}: {error}", plugin.name);
                self.log.push(LogLevel::Warn, "vst", said.clone());
                said
            }
        };
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
            // The read of the folders ended: the line that said it runs
            // says the count now, when no other word came in between.
            if self.plugins_scanning && self.status == Self::READING_THE_FOLDERS {
                self.status = settings::plugins_found(vst::host().plugins().len());
            }
            self.plugins_scanning = scanning;
            self.dirty_frame = true;
        }
        // The vst page of the Settings sheet ends in a row for every
        // plugin the scan found, so its cursor needs the new count as the
        // list grows. The page redraws from the host either way.
        if self
            .settings_sheet
            .as_ref()
            .is_some_and(|sheet| sheet.shows_vst())
        {
            let count = vst::host().plugins().len();
            if let Some(sheet) = self.settings_sheet.as_mut() {
                self.dirty_frame |= sheet.vst_scanned_count != count;
                sheet.vst_scanned_count = count;
                // The host reads the preset folders on its own thread. An
                // open plugin row asks again on each poll until the names
                // of its presets are here, and asks once more when a read
                // changes the count the row holds.
                if let Some(open) = sheet.vst_open
                    && let Some(plugin) = vst::host().plugins().get(open)
                {
                    sheet.vst_presets_due |= plugin.presets != sheet.vst_open_count;
                    if sheet.vst_presets_due
                        && let Some(presets) = vst::host().presets(&plugin.name)
                    {
                        self.settings_vst_open_presets = presets;
                        sheet.vst_open_count = self.settings_vst_open_presets.len();
                        sheet.vst_presets_due = false;
                        self.dirty_frame = true;
                    }
                }
            }
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
    fn a_letter_reaches_the_next_of_its_plugins() {
        let names = ["Serum", "Surge", "Vital"];
        // The row the cursor holds counts as visited: the same letter
        // again goes on, and around the end of the list.
        assert_eq!(jump_row(&names, 0, 's'), Some(1));
        assert_eq!(jump_row(&names, 1, 's'), Some(0));
        assert_eq!(jump_row(&names, 2, 's'), Some(0));
        assert_eq!(jump_row(&names, 1, 'V'), Some(2));
        // A letter no plugin begins with reaches none.
        assert_eq!(jump_row(&names, 1, 'z'), None);
    }

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

    /// The memory breakdown has one row for each plugin the host loaded, and
    /// no row for a plugin the host only found. A plugin is in the Studio
    /// process and has no figure of its own. A row shows the latency of the
    /// copies of its plugin when the latency is above 0. An unload takes the
    /// rows away.
    #[test]
    fn a_loaded_plugin_is_a_row_of_the_memory_breakdown() {
        let folder = tempfile::tempdir().unwrap();
        rustel_vst3_fixture::install(folder.path());
        // A host of its own reads this folder only: no plugin of the
        // machine, and no other test changes its list.
        let host = vst::Host::new();
        // The default preset of the effect delays its output by 1024 frames:
        // 0.25 of 4096.
        let presets = tempfile::tempdir().unwrap();
        let effect_presets = presets.path().join(rustel_vst3_fixture::NAME);
        std::fs::create_dir_all(&effect_presets).unwrap();
        let delay = rustel_vst3_fixture::preset_with_delay(1.0, 0.0, 0.25);
        std::fs::write(effect_presets.join("default.vstpreset"), delay).unwrap();
        host.set_preset_folder(presets.path().to_path_buf());
        host.scan(&[folder.path().to_path_buf()]);
        host.wait_idle();
        assert!(plugin_memory(&host.plugins()).is_empty());

        host.resolve(rustel_vst3_fixture::NAME, true);
        let figures = MemoryFigures {
            plugins: plugin_memory(&host.plugins()),
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
        assert_eq!(effect.value, "in process");
        let facts = "Rustel Fixture Tone · instrument · 0 running · ";
        assert!(tone.label.starts_with(facts), "{}", tone.label);
        assert_eq!(tone.value, "in process");
        // No copy ran, so the row has no latency.
        assert!(effect.label.ends_with(" ms"), "{}", effect.label);

        // The copy of the effect plays the default preset, and its row shows
        // the delay as the latency. The copy of the instrument has a latency
        // of 0, and its row shows none.
        let rate = 48_000;
        let slot = rustel_audio::InsertSlot::effect(1, 0);
        host.prepare(rustel_vst3_fixture::NAME, None, false, rate, slot);
        let slot = rustel_audio::InsertSlot::instrument(1);
        host.prepare(rustel_vst3_fixture::TONE_NAME, None, true, rate, slot);
        host.wait_idle();
        let [effect, tone] = &plugin_memory(&host.plugins())[..] else {
            panic!("2 plugin rows");
        };
        let facts = "Rustel Fixture · effect · 1 running · ";
        assert!(effect.label.starts_with(facts), "{}", effect.label);
        let latency = " ms · latency 1024 samples";
        assert!(effect.label.ends_with(latency), "{}", effect.label);
        let facts = "Rustel Fixture Tone · instrument · 1 running · ";
        assert!(tone.label.starts_with(facts), "{}", tone.label);
        assert!(tone.label.ends_with(" ms"), "{}", tone.label);

        host.unload_unused(&[]);
        host.wait_idle();
        assert!(plugin_memory(&host.plugins()).is_empty());
    }
}
