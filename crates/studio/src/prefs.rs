//! What the studio remembers between sessions: `studio.json` in Rustel's
//! config folder, beside the user themes. One small file, read
//! once at start and written when a choice is made, so a set opens the way
//! it was left.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use super::bounded_file::{self, MAX_DOCUMENT_BYTES};

pub const PREFS_FILE_NAME: &str = "studio.json";
/// The folder new sets are made in, beside the preferences.
pub const SETS_DIRECTORY_NAME: &str = "sets";

/// The preferences file this thread reads and writes.
///
/// Production uses one name. The test build uses one name per thread: the
/// suite runs many `App`s at once, and `flush_prefs` saves whenever a panel
/// moves or a widget is added. With one shared file, a test can read the
/// widgets that another test wrote.
///
/// An `App` touches preferences only from the thread that owns it, so one
/// name per thread keeps each test's file private without a lock.
#[cfg(not(test))]
fn prefs_file_name() -> std::borrow::Cow<'static, str> {
    std::borrow::Cow::Borrowed(PREFS_FILE_NAME)
}

#[cfg(test)]
fn prefs_file_name() -> std::borrow::Cow<'static, str> {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    thread_local! {
        static NAME: String = format!(
            "studio-test-{}.json",
            NEXT.fetch_add(1, Ordering::Relaxed)
        );
    }
    std::borrow::Cow::Owned(NAME.with(String::clone))
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct StudioPrefs {
    /// Advanced rendering override; absent means Automatic. The obsolete
    /// `graphics` key stays ignored so stale pixel settings do not reappear.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rendering: Option<String>,
    /// Shortcut conflict profile override; absent means automatic detection.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terminal_profile: Option<String>,
    /// How often the screen repaints while a set plays: `60`, `30`, `15`
    /// or `8`. Absent is sixty, which is what a machine with cores to
    /// spare should have.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frame_rate: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub show_full_paths: Option<bool>,
    /// The theme chosen in the picker.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub theme: Option<String>,
    /// `off`, `beat`, `cycle`, or a number of cycles.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quantise: Option<String>,
    /// `wait` or `async`. `strudel-like` still reads as async.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub load_mode: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub clock_out: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub clock_in: Option<String>,
    /// The MIDI destinations this machine is allowed to open, by port name.
    /// A port not named here is never opened, and a score that asks for it
    /// is told so rather than playing into nothing.
    ///
    /// ABSENT and present-but-empty mean different things, which is why this
    /// is an `Option` around a list that is usually shorter than the list of
    /// ports. Absent is a studio that predates the switch: every port on the
    /// machine at that first launch is taken as enabled, because a set that
    /// was playing last week must not open silent this week. Empty is
    /// somebody having turned everything off, and is honoured.
    ///
    /// A port that appears LATER - something just plugged in - is off until
    /// it is ticked. That is the half of "off by default" worth having: a
    /// device you have only now connected does not start receiving a set
    /// already in progress.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub midi_out_enabled: Option<Vec<String>>,
    /// The MIDI sources this machine is allowed to open, on the same terms
    /// as [`Self::midi_out_enabled`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub midi_in_enabled: Option<Vec<String>>,
    /// The audio input `s("in")` plays.
    ///
    /// Remembered where the audio OUTPUT deliberately is not: an output falls
    /// back to the host default and still makes sound, while a missing input
    /// leaves a score that listens silently silent, with nothing on screen to
    /// say why.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audio_input: Option<String>,
    /// The mixer's input fader, in tenths of a decibel (the prefs compare
    /// as equal, and a float does not): a microphone that needs a boost
    /// needs it every time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_gain_tenths_db: Option<i32>,
    /// The output buffer size: `auto`, or a size in frames. Whoever tuned
    /// the output latency once wants it every launch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_latency: Option<String>,
    /// Default voice limit; absent retains the existing 128-voice policy.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_polyphony: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub piano_sound: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub piano_volume: Option<u16>,
    /// The master limiter: `off`, or a ceiling and a character as
    /// `-1.0:transparent`. Absent is off, which is the default - it is
    /// latency on everything that plays, so it is opted into rather than
    /// out of, and whoever opted in wants it every launch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub master_limiter: Option<String>,
    /// Where that limiter holds, in tenths of a dB. Its own line because
    /// it outlives being switched off: a player who set -6 and then turned
    /// the limiter off to hear something wants -6 back at the next launch,
    /// not the default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub master_limiter_ceiling_tenths_db: Option<i16>,
    /// How that limiter works, by its key. Its own line for the same
    /// reason as the ceiling: the line above says `off` while the limiter
    /// is off, and a character has to survive that to come back.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub master_limiter_mode: Option<String>,
    /// Whether that ceiling is brought back up to full scale.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub master_limiter_makeup: Option<bool>,
    /// The mixer panel (F4) is open, along the top or the bottom, and how
    /// many rows tall it was made.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mixer_panel: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mixer_top: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mixer_rows: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub animation: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub highlights: Option<bool>,
    /// Which evaluation outcomes flash: full, on-success, or off.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evaluation_flash: Option<String>,
    /// A finished recording has its leading and trailing silence cut before
    /// it is saved. On by default: the silence around a take is the count-in
    /// and the wait for the next one, not part of the performance.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trim_recordings: Option<bool>,
    /// How long a sounding mark lingers after its event: theme, off,
    /// 150ms, 300ms, 600ms or 1s.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub highlight_fade: Option<String>,
    /// How much process and machine detail the header draws: none,
    /// basic, advanced or full. Absent is basic.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metric_detail: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub minimap: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub show_scrollbars: Option<bool>,
    /// The set panel docks at the right rather than the left.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub set_panel_right: Option<bool>,
    /// The set browser's preferred width, in terminal columns.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub set_panel_width: Option<u16>,
    /// The visuals docks: each its edge, whether it was open, and its
    /// widgets - the studio's own, the same in every set and every launch.
    #[serde(default = "super::viz_panel::DockPrefs::defaults")]
    pub visuals: [super::viz_panel::DockPrefs; super::viz_panel::DOCKS],
    /// The first dock's keeping before the docks had a record each,
    /// read once to carry it over and never written again.
    #[serde(default, skip_serializing)]
    pub viz_panel_right: Option<bool>,
    #[serde(default, skip_serializing)]
    pub viz_panel: Option<bool>,
    #[serde(default, skip_serializing)]
    pub widgets: Vec<super::viz_panel::WidgetSpec>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line_numbers: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wrap: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub master_scope: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub brackets: Option<bool>,
    /// DECSCUSR caret shape. Absent keeps the historical steady bar.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub caret_shape: Option<String>,
    /// When the editor marks the syntax checker's findings: `full`,
    /// `on-update` or `off`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub syntax_check_mode: Option<String>,
    /// The log panel opens showing the running commentary too.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub log_verbose: Option<bool>,
    /// ⇧F9: the log is docked furniture - like the mixer panel - rather
    /// than a sheet, and comes back that way at the next launch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub log_sticky: Option<bool>,
    /// Which edge the sticky log docks at: `top` or `bottom`. A file from
    /// when the log could be a column may say `left` or `right`; it still
    /// reads, and the log comes back along the bottom.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub log_edge: Option<super::viz_panel::Edge>,
    /// The rows the sticky log's band was made with `-` and `+`. Absent,
    /// it takes a third of the terminal.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub log_height: Option<u16>,
    /// The memory breakdown is docked, and comes back docked at the next
    /// launch. `memory_edge` is `top` or `bottom`; absent is the bottom.
    /// `memory_height` is the height in rows once `-` or `+` set it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory_docked: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory_edge: Option<super::viz_panel::Edge>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory_height: Option<u16>,
    /// The examples tab hands over one `$: stack(...)` rather than a voice
    /// a line.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[serde(alias = "generator_stack")]
    pub examples_stack: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backdrop_smoothing: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub slider_smoothing: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frequency_slider_log: Option<bool>,
    /// The twelve mapping slots, in order: the MIDI knob (`cc74/ch2`) or
    /// the gamepad axis (`x1`) that drives each one, `off` for a slot
    /// nobody has bound. Slot `n` drives the `n`th slider of whatever
    /// score is playing, so the list is global and outlives the set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mappings: Option<Vec<String>>,
    /// What each slot does with its controller's position: `scaled`,
    /// `jump`, `relative`. Same order and length as `mappings`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub takeover: Option<Vec<String>>,
    /// Explicit consent for score-requested Hydra webcam capture. Kept even
    /// by builds without Hydra so temporarily using a lean binary does not
    /// erase the operator's choice.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hydra_webcam: Option<bool>,
    /// The folder the last export was written to: where the next one
    /// starts. Absent until an export has happened.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub export_directory: Option<String>,
    /// Where new sets are made: absent, `sets/` beside this file.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sets_directory: Option<String>,
    /// Where finished audio takes are written: absent, `recordings/` beside
    /// this file.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recordings_directory: Option<String>,
    /// The set open when the studio last wrote its preferences: what a
    /// bare `rustel studio` opens.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_set: Option<String>,
    /// The sets opened lately, newest first, for File > Open recent.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub recent_sets: Vec<String>,
    /// Whether the set panel was open, so it comes back open.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub set_panel: Option<bool>,
    /// The three opacities, and the one place their `Option` carries meaning.
    ///
    /// **Present means the reader chose this**, so it outlives a restart and
    /// no theme may overwrite it again. **Absent means follow the theme**, so
    /// the next theme applied gets to suggest. Applying a theme clears them;
    /// touching the setting writes them. Without that distinction the file
    /// cannot tell "85 because this theme asked" from "85 because the reader
    /// chose it", and a theme would either overwrite the reader's tuning on
    /// every launch or be frozen out by their first nudge.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backdrop_opacity: Option<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub interface_opacity: Option<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub editor_opacity: Option<u8>,
    /// The File / Edit menu bar along the top. Absent is shown.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub show_menu: Option<bool>,
    /// The rustel PLAYING tempo line. Absent is shown.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub show_header: Option<bool>,
    /// The footer's meter, orbits and device chips. Absent is shown. Off
    /// still keeps the notices and the status line.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub show_footer: Option<bool>,
    /// Folders and packs the player imported, which every set sees.
    /// Order is precedence within the layer: a later row wins a name.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sample_sources: Vec<SampleSourcePref>,
    /// Whether an imported source is fetched into the cache in the
    /// background rather than on the first note that needs it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub precache_sources: Option<bool>,
    /// What an imported bank was renamed to, by the name it arrived under.
    /// An overlay: the files are untouched, and removing the entry brings
    /// the old name back.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub sample_renames: std::collections::BTreeMap<String, String>,
    /// Aliases keyed first by source, then by the bank's original name.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub sample_source_renames:
        std::collections::BTreeMap<String, std::collections::BTreeMap<String, String>>,
    /// Ceiling on decoded PCM the sounding score does not name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preview_budget: Option<String>,
    /// The most one sound may hold, by rung key.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sample_ceiling: Option<String>,
    /// After this long without a sample preview, unused decoded PCM is dropped.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unused_sample_idle: Option<String>,
    /// The learnt keyboard shortcuts, as Settings ▸ Keybinds keeps them.
    /// Only OVERRIDES are here; an action absent from the list keeps the
    /// studio's own chord, so a new default ships to everyone who has not
    /// re-bound it. An untouched table writes nothing at all, the way an
    /// unset option is absent rather than empty.
    #[serde(
        default,
        skip_serializing_if = "super::keybinds::KeybindPrefs::is_empty"
    )]
    pub keybinds: super::keybinds::KeybindPrefs,
}

/// One imported sample source, as `studio.json` keeps it.
#[derive(Clone, Debug, Default, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct SampleSourcePref {
    /// A folder path, a URL, or any spelling a score could write -
    /// `github:user/repo`.
    pub spec: String,
    /// Off keeps the row and drops its sounds, so a pack you want back
    /// next week does not have to be found again.
    #[serde(default = "yes")]
    pub enabled: bool,
}

fn yes() -> bool {
    true
}

/// `path` made absolute, with `.` and `..` read off by name and without
/// the verbatim `\\?\` prefix `canonicalize` gives on Windows.
fn comparable_path(path: &std::path::Path) -> PathBuf {
    let absolute = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
    let text = absolute.to_string_lossy();
    let mut resolved = PathBuf::new();
    for part in
        std::path::Path::new(rustel_runtime::samples::without_verbatim_prefix(&text).as_ref())
            .components()
    {
        match part {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                resolved.pop();
            }
            part => resolved.push(part),
        }
    }
    resolved
}

/// Whether two file names name one entry: on Windows without case, as its
/// file system compares them.
pub(super) fn same_file_name(a: &str, b: &str) -> bool {
    if cfg!(windows) {
        a.to_lowercase() == b.to_lowercase()
    } else {
        a == b
    }
}

/// The names of `path` below `folder`, when `path` is `folder` or lies
/// inside it. Both are compared as [`comparable_path`] spells them, name
/// by name as [`same_file_name`] compares them.
pub(super) fn names_below(folder: &std::path::Path, path: &std::path::Path) -> Option<Vec<String>> {
    let names = |path: &std::path::Path| -> Vec<String> {
        comparable_path(path)
            .to_string_lossy()
            .split(std::path::is_separator)
            .filter(|name| !name.is_empty())
            .map(str::to_owned)
            .collect()
    };
    let (folder, path) = (names(folder), names(path));
    (!folder.is_empty()
        && path.len() >= folder.len()
        && folder.iter().zip(&path).all(|(a, b)| same_file_name(a, b)))
    .then(|| path[folder.len()..].to_vec())
}

/// The most sets Open recent offers.
pub const RECENT_SETS: usize = 8;

impl Default for StudioPrefs {
    fn default() -> Self {
        // What an empty file reads as: every key unset, and the docks in
        // their starting shape - so a missing file and an empty one agree.
        serde_json::from_str("{}").expect("an empty preferences file reads")
    }
}

impl StudioPrefs {
    /// Remember `directory` as the set open now and the newest of the
    /// recent ones; a set already on the list moves to the front.
    pub fn remember_set(&mut self, directory: &std::path::Path) {
        let entry = directory.to_string_lossy().into_owned();
        self.recent_sets.retain(|recent| *recent != entry);
        self.recent_sets.insert(0, entry.clone());
        self.recent_sets.truncate(RECENT_SETS);
        self.last_set = Some(entry);
    }

    /// Take a set off the recent list: it is gone from the disk.
    pub fn forget_set(&mut self, directory: &std::path::Path) {
        let entry = directory.to_string_lossy().into_owned();
        self.recent_sets.retain(|recent| *recent != entry);
        if self.last_set.as_deref() == Some(entry.as_str()) {
            self.last_set = None;
        }
    }

    /// Where new sets are made: the folder chosen in the settings, or
    /// `sets/` in the config folder.
    pub fn sets_directory(&self) -> Option<PathBuf> {
        match self.sets_directory.as_deref().map(str::trim) {
            Some(chosen) if !chosen.is_empty() => Some(PathBuf::from(chosen)),
            _ => super::config::directory().map(|directory| directory.join(SETS_DIRECTORY_NAME)),
        }
    }

    /// Where audio takes are written: the folder chosen in Settings, or
    /// `recordings/` in the config folder.
    pub fn recordings_directory(&self) -> Option<PathBuf> {
        match self.recordings_directory.as_deref().map(str::trim) {
            Some(chosen) if !chosen.is_empty() => Some(PathBuf::from(chosen)),
            _ => super::config::directory().map(|directory| {
                directory.join(rustel_runtime::product::RECORDINGS_DIRECTORY_NAME)
            }),
        }
    }

    /// The settings as saved, with the theme answering for anything the
    /// reader has not chosen.
    pub fn ui_settings_for(&self, theme: &super::theme::Theme) -> super::settings::UiSettings {
        let suggested = theme.opacities();
        let mut settings = self.ui_settings();
        settings.backdrop_opacity = self
            .backdrop_opacity
            .map_or(suggested.backdrop, |o| o.min(100));
        settings.interface_opacity = self
            .interface_opacity
            .map_or(suggested.interface, |o| o.min(100));
        settings.editor_opacity = self.editor_opacity.map_or(suggested.editor, |o| o.min(100));
        settings
    }

    /// Forget the reader's opacity choices, so the theme's suggestion is what
    /// answers next. Called when a theme is applied.
    pub fn follow_theme_opacity(&mut self) {
        self.backdrop_opacity = None;
        self.interface_opacity = None;
        self.editor_opacity = None;
    }

    /// Replace the imported sources, keeping their order.
    pub fn set_sample_sources(&mut self, sources: Vec<SampleSourcePref>) {
        self.sample_sources = sources;
    }

    /// Remove enabled local children already owned by a broader enabled
    /// source. Returns whether the saved list changed.
    pub fn deduplicate_sample_sources(&mut self) -> bool {
        let before = self.sample_sources.clone();
        let (sources, ownership) = rustel_runtime::samples::deduplicated_global_sources_with_owners(
            &self.global_sources(),
        );
        // Later source rows used to win name collisions. Migrate them first;
        // an alias already set directly on the surviving parent wins all.
        for (child, parent) in ownership.into_iter().rev() {
            let Some(child_aliases) = self.sample_source_renames.remove(&child) else {
                continue;
            };
            let parent_aliases = self.sample_source_renames.entry(parent).or_default();
            for (bank, alias) in child_aliases {
                parent_aliases.entry(bank).or_insert(alias);
            }
        }
        self.sample_sources = sources
            .into_iter()
            .map(|source| SampleSourcePref {
                spec: source.spec,
                enabled: source.enabled,
            })
            .collect();
        self.sample_sources != before
    }

    /// Point every local source at or inside `from` at the same place
    /// inside `to`, bank aliases and all, once that folder has moved.
    /// Returns whether a source changed.
    pub fn follow_moved_folder(&mut self, from: &std::path::Path, to: &std::path::Path) -> bool {
        let mut changed = false;
        for source in &mut self.sample_sources {
            let spec = source.spec.trim();
            let Some(folder) = rustel_runtime::samples::folder_of_spec(spec) else {
                continue;
            };
            let Some(below) = names_below(from, std::path::Path::new(&folder)) else {
                continue;
            };
            let mut moved = comparable_path(to);
            moved.extend(below);
            // Spelled the way it was: a `local:` source stays one.
            let spelling = spec.strip_suffix(folder.as_str()).unwrap_or_default();
            let moved = format!("{spelling}{}", moved.display());
            if let Some(aliases) = self.sample_source_renames.remove(&source.spec) {
                let kept = self.sample_source_renames.entry(moved.clone()).or_default();
                for (bank, alias) in aliases {
                    kept.entry(bank).or_insert(alias);
                }
            }
            source.spec = moved;
            changed = true;
        }
        changed
    }

    /// Drop every local source inside a set folder under the sets folder
    /// when that set folder is gone, bank aliases and all, and return
    /// their specs. Nothing goes while the sets folder itself is not
    /// there, as on a drive that is unplugged.
    pub fn forget_sources_in_gone_sets(&mut self) -> Vec<String> {
        let Some(sets) = self.sets_directory().filter(|sets| sets.is_dir()) else {
            return Vec::new();
        };
        let gone: Vec<String> = self
            .sample_sources
            .iter()
            .filter(|source| {
                rustel_runtime::samples::folder_of_spec(&source.spec)
                    .and_then(|folder| names_below(&sets, std::path::Path::new(&folder)))
                    .and_then(|below| below.into_iter().next())
                    .is_some_and(|set| matches!(sets.join(set).try_exists(), Ok(false)))
            })
            .map(|source| source.spec.clone())
            .collect();
        self.sample_sources
            .retain(|source| !gone.contains(&source.spec));
        for spec in &gone {
            self.sample_source_renames.remove(spec);
        }
        gone
    }

    /// The renames as the library takes them.
    pub fn bank_renames(&self) -> std::collections::HashMap<String, String> {
        self.sample_renames
            .iter()
            .map(|(from, to)| (from.clone(), to.clone()))
            .collect()
    }

    pub fn source_bank_renames(&self) -> std::collections::HashMap<(String, String), String> {
        self.sample_source_renames
            .iter()
            .flat_map(|(source, aliases)| {
                aliases
                    .iter()
                    .map(|(from, to)| ((source.clone(), from.clone()), to.clone()))
            })
            .collect()
    }

    /// The imported sources as the library takes them.
    pub fn global_sources(&self) -> Vec<rustel_runtime::samples::GlobalSource> {
        self.sample_sources
            .iter()
            .map(|source| rustel_runtime::samples::GlobalSource {
                spec: source.spec.clone(),
                enabled: source.enabled,
            })
            .collect()
    }

    pub fn ui_settings(&self) -> super::settings::UiSettings {
        let defaults = super::settings::UiSettings::default();
        super::settings::UiSettings {
            terminal_profile: self
                .terminal_profile
                .as_deref()
                .and_then(super::terminal::conflicts::canonical_profile)
                .map(str::to_owned),
            show_full_paths: self.show_full_paths.unwrap_or(defaults.show_full_paths),
            rendering: self
                .rendering
                .as_deref()
                .map(super::graphics::RenderingMode::parse)
                .unwrap_or_default(),
            frame_rate: self
                .frame_rate
                .as_deref()
                .map(super::settings::FrameRate::parse)
                .unwrap_or(defaults.frame_rate),
            quantise: self
                .quantise
                .as_deref()
                .map(super::settings::Quantise::parse)
                .unwrap_or(defaults.quantise),
            load_mode: self
                .load_mode
                .as_deref()
                .map(super::settings::LoadMode::parse)
                .unwrap_or(defaults.load_mode),
            clock_out: self.clock_out.clone(),
            clock_in: self.clock_in.clone(),
            animation: self.animation.unwrap_or(defaults.animation),
            highlights: self.highlights.unwrap_or(defaults.highlights),
            evaluation_flash: self
                .evaluation_flash
                .as_deref()
                .map(super::settings::EvaluationFlashMode::parse)
                .unwrap_or(defaults.evaluation_flash),
            trim_recordings: self.trim_recordings.unwrap_or(defaults.trim_recordings),
            output_latency: self
                .output_latency
                .as_deref()
                .map(super::settings::OutputLatency::parse)
                .unwrap_or(defaults.output_latency),
            piano_sound: self
                .piano_sound
                .as_deref()
                .map(super::PianoSound::parse)
                .unwrap_or(defaults.piano_sound),
            piano_volume: self
                .piano_volume
                .unwrap_or(defaults.piano_volume)
                .min(super::piano::MAX_PIANO_VOLUME),
            max_polyphony: self
                .max_polyphony
                .map(super::settings::normalize_max_polyphony)
                .unwrap_or(defaults.max_polyphony),
            master_limiter_on: self
                .master_limiter
                .as_deref()
                .map_or(defaults.master_limiter_on, |text| {
                    super::settings::parse_master_limiter(text).is_some()
                }),
            // The character its own line, then what the older single line
            // held: a preferences file written before the mode moved to
            // Advanced still says `-1.0:warm`, and that player's warm is
            // not to be traded for the default on the way past.
            master_limiter_character: self
                .master_limiter_mode
                .as_deref()
                .and_then(|text| rustel_audio::LimiterCharacter::parse(text.trim()))
                .or_else(|| {
                    self.master_limiter
                        .as_deref()
                        .and_then(super::settings::parse_master_limiter)
                        .map(|held| held.character)
                })
                .unwrap_or(defaults.master_limiter_character),
            // The ceiling is its own line because it outlives being
            // switched off: a player who set -6 and then turned the
            // limiter off to hear something wants -6 back at the next
            // launch, not the default.
            master_limiter_makeup: self
                .master_limiter_makeup
                .unwrap_or(defaults.master_limiter_makeup),
            master_limiter_ceiling_db: self
                .master_limiter_ceiling_tenths_db
                .map_or(defaults.master_limiter_ceiling_db, |tenths| {
                    super::settings::clamp_limiter_ceiling(f32::from(tenths) / 10.0)
                }),
            highlight_fade: self
                .highlight_fade
                .as_deref()
                .map(super::settings::HighlightFade::parse)
                .unwrap_or(defaults.highlight_fade),
            metric_detail: self
                .metric_detail
                .as_deref()
                .map(super::settings::MetricDetail::parse)
                .unwrap_or(defaults.metric_detail),
            minimap: self.minimap.unwrap_or(defaults.minimap),
            show_scrollbars: self.show_scrollbars.unwrap_or(defaults.show_scrollbars),
            set_panel_right: self.set_panel_right.unwrap_or(defaults.set_panel_right),
            mixer_top: self.mixer_top.unwrap_or(defaults.mixer_top),
            viz_edges: [self.visuals[0].edge, self.visuals[1].edge],
            line_numbers: self.line_numbers.unwrap_or(defaults.line_numbers),
            wrap: self.wrap.unwrap_or(defaults.wrap),
            master_scope: self.master_scope.unwrap_or(defaults.master_scope),
            brackets: self.brackets.unwrap_or(defaults.brackets),
            caret_shape: self
                .caret_shape
                .as_deref()
                .and_then(super::terminal::CaretShape::parse)
                .unwrap_or(defaults.caret_shape),
            syntax_check: self
                .syntax_check_mode
                .as_deref()
                .map(super::settings::SyntaxCheck::parse)
                .unwrap_or(defaults.syntax_check),
            log_verbose: self.log_verbose.unwrap_or(defaults.log_verbose),
            examples_stack: self.examples_stack.unwrap_or(defaults.examples_stack),
            slider_smoothing: self.slider_smoothing.unwrap_or(defaults.slider_smoothing),
            frequency_slider_log: self
                .frequency_slider_log
                .unwrap_or(defaults.frequency_slider_log),
            mappings: {
                let saved = self.mappings.as_deref().unwrap_or_default();
                std::array::from_fn(|slot| {
                    saved
                        .get(slot)
                        .and_then(|text| super::settings::MappingSource::parse(text))
                })
            },
            takeover: {
                let saved = self.takeover.as_deref().unwrap_or_default();
                std::array::from_fn(|slot| {
                    saved
                        .get(slot)
                        .map_or_else(super::settings::Takeover::default, |text| {
                            super::settings::Takeover::parse(text)
                        })
                })
            },
            backdrop_smoothing: self
                .backdrop_smoothing
                .unwrap_or(defaults.backdrop_smoothing),
            #[cfg(feature = "hydra")]
            hydra_webcam: self.hydra_webcam.unwrap_or(defaults.hydra_webcam),
            // Capped on the way in: the file is editable by hand, and a
            // value past 100 would make the sheet's arrows useless until it
            // was stepped back into range.
            backdrop_opacity: self
                .backdrop_opacity
                .map_or(defaults.backdrop_opacity, |opacity| opacity.min(100)),
            interface_opacity: self
                .interface_opacity
                .map_or(defaults.interface_opacity, |opacity| opacity.min(100)),
            editor_opacity: self
                .editor_opacity
                .map_or(defaults.editor_opacity, |opacity| opacity.min(100)),
            // Deliberately not remembered: the studio always opens with its
            // stage up.
            zen: defaults.zen,
            show_menu: self.show_menu.unwrap_or(defaults.show_menu),
            show_header: self.show_header.unwrap_or(defaults.show_header),
            show_footer: self.show_footer.unwrap_or(defaults.show_footer),
            precache_sources: self.precache_sources.unwrap_or(defaults.precache_sources),
            sample_ceiling: self
                .sample_ceiling
                .as_deref()
                .map(super::settings::SampleCeiling::parse)
                .unwrap_or(defaults.sample_ceiling),
            preview_budget: self
                .preview_budget
                .as_deref()
                .map(super::settings::PreviewBudget::parse)
                .unwrap_or(defaults.preview_budget),
            unused_sample_idle: self
                .unused_sample_idle
                .as_deref()
                .map(super::settings::UnusedSampleIdle::parse)
                .unwrap_or(defaults.unused_sample_idle),
        }
    }

    pub fn set_ui_settings(&mut self, settings: &super::settings::UiSettings) {
        self.show_full_paths = Some(settings.show_full_paths);
        self.rendering = Some(settings.rendering.key().to_owned());
        self.terminal_profile = settings
            .terminal_profile
            .as_deref()
            .and_then(super::terminal::conflicts::canonical_profile)
            .map(str::to_owned);
        self.frame_rate = Some(settings.frame_rate.key().to_owned());
        self.clock_out = settings.clock_out.clone();
        self.clock_in = settings.clock_in.clone();
        self.quantise = Some(settings.quantise.key());
        self.load_mode = Some(settings.load_mode.key().to_owned());
        self.animation = Some(settings.animation);
        self.highlights = Some(settings.highlights);
        self.evaluation_flash = Some(settings.evaluation_flash.key().to_owned());
        self.trim_recordings = Some(settings.trim_recordings);
        self.highlight_fade = Some(settings.highlight_fade.key().to_owned());
        self.metric_detail = Some(settings.metric_detail.key().to_owned());
        self.max_polyphony = Some(super::settings::normalize_max_polyphony(
            settings.max_polyphony,
        ));
        // A size this build refuses (outside the engine's bounds, or not a
        // number) reads as automatic, but is kept word for word while the
        // setting still says automatic, as an unreadable limiter is below:
        // writing "auto" over a hand-edited 20000 on the next save would
        // lose the only copy of what somebody typed.
        let unreadable = self.output_latency.as_deref().filter(|text| {
            let text = text.trim();
            text != "auto"
                && text != "0"
                && super::settings::OutputLatency::parse(text)
                    == super::settings::OutputLatency::Automatic
        });
        self.piano_sound = Some(settings.piano_sound.key().to_owned());
        self.piano_volume = Some(settings.piano_volume.min(super::piano::MAX_PIANO_VOLUME));
        self.output_latency = match (unreadable, settings.output_latency) {
            (Some(text), super::settings::OutputLatency::Automatic) => Some(text.to_owned()),
            (_, chosen) => Some(chosen.key()),
        };
        // A limiter this build cannot read is kept word for word, the way a
        // set file keeps it (`SetLimiter::Unreadable`): the preferences file
        // is the only copy, and `ui_settings` resolves unreadable to off, so
        // writing back what the settings now say would replace a newer
        // rustel's `-1.0:gluey` - or a line a sync client cut in half - with
        // `off` on any path that persists, and persisting is not rare. Only a
        // real choice overwrites it.
        let unreadable = self.master_limiter.as_deref().filter(|text| {
            text.trim() != super::scenes::OFF_LIMITER
                && super::settings::parse_master_limiter(text).is_none()
        });
        self.master_limiter = match (unreadable, settings.master_limiter()) {
            (Some(text), None) => Some(text.to_owned()),
            (_, chosen) => Some(super::settings::write_master_limiter(chosen)),
        };
        self.master_limiter_ceiling_tenths_db =
            Some((settings.master_limiter_ceiling_db * 10.0).round() as i16);
        self.master_limiter_mode = Some(settings.master_limiter_character.key().to_owned());
        self.master_limiter_makeup = Some(settings.master_limiter_makeup);
        self.minimap = Some(settings.minimap);
        self.show_scrollbars = Some(settings.show_scrollbars);
        self.set_panel_right = Some(settings.set_panel_right);
        self.mixer_top = Some(settings.mixer_top);
        for (dock, edge) in self.visuals.iter_mut().zip(settings.viz_edges) {
            dock.edge = edge;
        }
        self.line_numbers = Some(settings.line_numbers);
        self.wrap = Some(settings.wrap);
        self.master_scope = Some(settings.master_scope);
        self.brackets = Some(settings.brackets);
        self.caret_shape = Some(settings.caret_shape.key().to_owned());
        self.syntax_check_mode = Some(settings.syntax_check.key().to_owned());
        self.log_verbose = Some(settings.log_verbose);
        self.examples_stack = Some(settings.examples_stack);
        self.backdrop_smoothing = Some(settings.backdrop_smoothing);
        self.slider_smoothing = Some(settings.slider_smoothing);
        self.frequency_slider_log = Some(settings.frequency_slider_log);
        // Always the full twelve, in order: a shorter list would make
        // the slot a mapping sits in depend on how many came before it.
        self.mappings = Some(
            settings
                .mappings
                .iter()
                .map(|source| {
                    source.map_or_else(|| "off".to_owned(), super::settings::MappingSource::key)
                })
                .collect(),
        );
        self.takeover = Some(
            settings
                .takeover
                .iter()
                .map(|mode| mode.key().to_owned())
                .collect(),
        );
        #[cfg(feature = "hydra")]
        {
            self.hydra_webcam = Some(settings.hydra_webcam);
        }
        self.preview_budget = Some(settings.preview_budget.key().to_owned());
        self.sample_ceiling = Some(settings.sample_ceiling.key().to_owned());
        self.unused_sample_idle = Some(settings.unused_sample_idle.key().to_owned());
        self.precache_sources = Some(settings.precache_sources);
        self.show_menu = Some(settings.show_menu);
        self.show_header = Some(settings.show_header);
        self.show_footer = Some(settings.show_footer);
        // `sample_sources` is NOT written here. The sheet edits the rows in
        // place, through `set_sample_sources`, because a source is a thing
        // you added rather than a switch you flipped: rebuilding the list
        // out of a settings snapshot would lose the order the rows are in,
        // which is their precedence.
        // The three opacities are deliberately NOT written here. Their
        // presence means "the reader chose this", and saving any other
        // setting is not the reader choosing an opacity - it would freeze
        // every theme's suggestion the first time someone toggled the
        // minimap. `claim_opacities` is how they are claimed.
    }

    /// Record the three opacities as the reader's own, so no theme overwrites
    /// them again and they survive a restart.
    pub fn claim_opacities(&mut self, settings: &super::settings::UiSettings) {
        self.backdrop_opacity = Some(settings.backdrop_opacity);
        self.interface_opacity = Some(settings.interface_opacity);
        self.editor_opacity = Some(settings.editor_opacity);
    }

    /// The learnt shortcuts, restored from the file. The overrides live
    /// beside the other preferences rather than inside `UiSettings`:
    /// every surface reads the table through the app, and the sheet edits
    /// it in place, like the imported sources it sits beside.
    pub fn keybinds(&self) -> super::keybinds::Keybinds {
        let mut binds = super::keybinds::Keybinds::default();
        binds.restore(&self.keybinds);
        binds
    }

    /// Remember the learnt shortcuts as the table now holds them.
    pub fn set_keybinds(&mut self, binds: &super::keybinds::Keybinds) {
        self.keybinds = binds.prefs();
    }
}

/// Where [`StudioPrefs::load`] moved a file that did not read, until the
/// studio reports it.
static KEPT_ASIDE: std::sync::Mutex<Option<PathBuf>> = std::sync::Mutex::new(None);

/// Take the new path of a preferences file that did not read.
pub fn take_kept_aside() -> Option<PathBuf> {
    KEPT_ASIDE
        .lock()
        .unwrap_or_else(|slot| slot.into_inner())
        .take()
}

/// Move a preferences file that exists and does not read to
/// `studio.json.bad` (then `.bad 2`, ...). It is the only copy of the
/// player's settings. Returns the new path when the file was moved.
fn keep_unreadable_aside(path: &std::path::Path) -> Option<PathBuf> {
    match bounded_file::read_to_string(path, MAX_DOCUMENT_BYTES) {
        Ok(text) if serde_json::from_str::<StudioPrefs>(&text).is_ok() => return None,
        Err(bounded_file::ReadError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
            return None;
        }
        _ => {}
    }
    let mut candidate = path.with_extension("json.bad");
    let mut suffix = 2u32;
    while candidate.exists() {
        if suffix > 100 {
            return None;
        }
        candidate = path.with_extension(format!("json.bad {suffix}"));
        suffix += 1;
    }
    std::fs::rename(path, &candidate).ok()?;
    Some(candidate)
}

impl StudioPrefs {
    /// Where the file lives: Rustel's config directory plus `studio.json`.
    pub fn path() -> Option<PathBuf> {
        super::config::directory().map(|directory| directory.join(prefs_file_name().as_ref()))
    }

    /// Read the file; a missing or unreadable one is the defaults. An
    /// unreadable file is first moved aside, because the next save replaces
    /// it. See [`take_kept_aside`].
    pub fn load() -> Self {
        let path = super::config::read_path(prefs_file_name().as_ref());
        if let Some(kept) = path.as_deref().and_then(keep_unreadable_aside) {
            *KEPT_ASIDE.lock().unwrap_or_else(|slot| slot.into_inner()) = Some(kept);
        }
        Self::load_from(path.as_deref())
    }

    pub fn load_from(path: Option<&std::path::Path>) -> Self {
        let mut prefs: Self = path
            .and_then(|path| bounded_file::read_to_string(path, MAX_DOCUMENT_BYTES).ok())
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default();
        prefs.carry_over_first_dock();
        prefs
    }

    /// A file from before the docks had a record each kept the first
    /// dock's edge, state and widgets under keys of their own; they go
    /// into the first dock's record and are not written back.
    fn carry_over_first_dock(&mut self) {
        use super::viz_panel::Edge;
        let first = &mut self.visuals[0];
        if let Some(open) = self.viz_panel.take() {
            first.open = open;
        }
        if let Some(right) = self.viz_panel_right.take() {
            first.edge = if right { Edge::Right } else { Edge::Left };
        }
        if !self.widgets.is_empty() {
            first.widgets = std::mem::take(&mut self.widgets);
        }
    }

    /// Write the file, creating its folder. Says why when it cannot.
    pub fn save(&self) -> Result<PathBuf, String> {
        let path = Self::path().ok_or_else(|| "no config folder on this system".to_owned())?;
        self.save_to(&path)?;
        Ok(path)
    }

    pub fn save_to(&self, path: &std::path::Path) -> Result<(), String> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|error| format!("cannot create {}: {error}", parent.display()))?;
        }
        let text = serde_json::to_string_pretty(self).map_err(|error| error.to_string())?;
        // Written through a temporary and renamed, never truncated in place.
        // `fs::write` opens with truncate, so a kill or a power loss between
        // that and the last byte left a short file - and a short file does not
        // parse, and an unparsed file is silently the defaults. Every
        // preference would go, including the webcam consent this file exists
        // to remember.
        super::save::atomic_write(path, &(text + "\n"))
            .map_err(|error| format!("cannot write {}: {error}", path.display()))
    }
}

#[cfg(test)]
mod tests {
    mod migration {
        //! Preferences files written by older builds.

        use super::super::{PREFS_FILE_NAME, StudioPrefs};
        use crate::settings::{MAPPING_SLOTS, UiSettings};
        use crate::terminal::CaretShape;

        /// A preferences file written by a build that had the old slider
        /// stick/knob settings still loads. Removing a field must not make a
        /// player's whole file unreadable - that would take their theme, their
        /// sets folder and their sample sources with it.
        #[test]
        fn a_file_from_before_the_mapping_slots_still_loads() {
            let directory = tempfile::tempdir().expect("temp dir");
            let path = directory.path().join(PREFS_FILE_NAME);
            std::fs::write(
                &path,
                r#"{
          "slider_stick": "y2",
          "slider_cc": "cc74/ch2",
          "slider_cc_push": true,
          "theme": "mono",
          "wrap": true
        }"#,
            )
            .expect("write");
            let prefs = StudioPrefs::load_from(Some(&path));
            assert_eq!(prefs.theme.as_deref(), Some("mono"), "the rest survived");
            assert_eq!(prefs.wrap, Some(true));
            assert_eq!(prefs.mappings, None, "and nothing was invented for it");
            assert_eq!(
                prefs.ui_settings().mappings,
                [None; MAPPING_SLOTS],
                "an old knob does not become a slot: it named the focused \
         slider, which no longer exists"
            );
        }

        #[test]
        fn caret_shape_round_trips_and_old_preferences_keep_the_steady_bar() {
            assert_eq!(
                StudioPrefs::default().ui_settings().caret_shape,
                CaretShape::SteadyBar
            );
            let settings = UiSettings {
                caret_shape: CaretShape::BlinkingUnderline,
                ..Default::default()
            };
            let mut prefs = StudioPrefs::default();
            prefs.set_ui_settings(&settings);
            let encoded = serde_json::to_string(&prefs).unwrap();
            let restored: StudioPrefs = serde_json::from_str(&encoded).unwrap();
            assert_eq!(restored.caret_shape.as_deref(), Some("blinking-underline"));
            assert_eq!(
                restored.ui_settings().caret_shape,
                CaretShape::BlinkingUnderline
            );
        }
    }
    mod source_folders {
        //! Sample sources that name folders the studio itself moves.

        use std::path::Path;

        use super::super::{SampleSourcePref, StudioPrefs};

        fn with_sources(specs: &[String]) -> StudioPrefs {
            StudioPrefs {
                sample_sources: specs
                    .iter()
                    .map(|spec| SampleSourcePref {
                        spec: spec.clone(),
                        enabled: true,
                    })
                    .collect(),
                ..StudioPrefs::default()
            }
        }

        fn specs(prefs: &StudioPrefs) -> Vec<&str> {
            prefs
                .sample_sources
                .iter()
                .map(|source| source.spec.as_str())
                .collect()
        }

        /// A folder moved from `monday` to `tuesday` takes the sources inside it
        /// along, bank aliases and `local:` spelling included, and leaves alone a
        /// sibling whose name merely starts the same way.
        #[test]
        fn sources_inside_a_moved_folder_follow_it() {
            let root = std::env::temp_dir().join("rustel-moved-folder");
            let monday = root.join("monday");
            let tuesday = root.join("tuesday");
            let takes = monday.join("takes").display().to_string();
            let local = format!("local:{}", monday.join("kit").display());
            let sibling = root.join("monday two").display().to_string();
            let trailing = format!("{}{}", monday.display(), std::path::MAIN_SEPARATOR);
            let mut prefs = with_sources(&[
                takes.clone(),
                local,
                sibling.clone(),
                trailing,
                "github:tidalcycles/dirt-samples".to_owned(),
            ]);
            prefs
                .sample_source_renames
                .entry(takes)
                .or_default()
                .insert("takes".into(), "mine".into());

            assert!(prefs.follow_moved_folder(&monday, &tuesday));

            let moved_takes = tuesday.join("takes").display().to_string();
            let moved_kit = format!("local:{}", tuesday.join("kit").display());
            let moved_whole = tuesday.display().to_string();
            assert_eq!(
                specs(&prefs),
                [
                    moved_takes.as_str(),
                    moved_kit.as_str(),
                    sibling.as_str(),
                    moved_whole.as_str(),
                    "github:tidalcycles/dirt-samples",
                ]
            );
            assert_eq!(
                prefs.sample_source_renames[&moved_takes]["takes"], "mine",
                "the alias moved with its source"
            );
            assert_eq!(prefs.sample_source_renames.len(), 1);
            assert!(
                !prefs.follow_moved_folder(&monday, &tuesday),
                "nothing is left to move"
            );
        }

        /// A set opened by a relative path (`rustel studio monday/`) is renamed by
        /// that path, while its sources are absolute: the two still meet.
        #[test]
        fn a_folder_moved_by_a_relative_path_carries_absolute_sources() {
            let here = std::env::current_dir().expect("working directory");
            let source = here.join("monday").join("takes").display().to_string();
            let mut prefs = with_sources(std::slice::from_ref(&source));

            assert!(prefs.follow_moved_folder(Path::new("monday/"), Path::new("./tuesday")));

            assert_eq!(
                specs(&prefs),
                [here.join("tuesday").join("takes").display().to_string()]
            );
        }

        /// Windows spells one folder several ways: any case, and the verbatim
        /// `\\?\` form `canonicalize` gives. Each is the same folder.
        #[cfg(windows)]
        #[test]
        fn windows_spellings_of_a_moved_folder_are_one_folder() {
            let mut prefs = with_sources(&[
                r"C:\Users\Me\.rustel\sets\2026-09-11 b\takes".to_owned(),
                r"c:\users\me\.RUSTEL\sets\2026-09-11 B\kit\".to_owned(),
            ]);

            assert!(prefs.follow_moved_folder(
                Path::new(r"\\?\C:\Users\me\.rustel\sets\2026-09-11 b"),
                Path::new(r"C:\Users\me\.rustel\sets\live"),
            ));

            let live = std::path::PathBuf::from(r"C:\Users\me\.rustel\sets\live");
            assert_eq!(
                specs(&prefs),
                [
                    live.join("takes").display().to_string(),
                    live.join("kit").display().to_string(),
                ]
            );
        }

        /// Sources inside a set folder that is gone from the sets folder are
        /// dropped with their aliases; the rest stay. While the sets folder itself
        /// is not there, as on an unplugged drive, nothing is dropped.
        #[test]
        fn sources_inside_a_gone_set_are_forgotten_unless_the_sets_folder_is_gone() {
            let home = tempfile::tempdir().expect("temp dir");
            let sets = home.path().join("sets");
            let here = sets.join("tuesday");
            std::fs::create_dir_all(&here).expect("a set that is there");
            let gone = sets.join("monday").join("sessions").display().to_string();
            let whole = sets.join("sunday").display().to_string();
            let kept = here.join("kit").display().to_string();
            let outside = home.path().join("elsewhere").display().to_string();
            let mut prefs =
                with_sources(&[gone.clone(), kept.clone(), whole.clone(), outside.clone()]);
            prefs.sets_directory = Some(sets.display().to_string());
            prefs
                .sample_source_renames
                .entry(gone.clone())
                .or_default()
                .insert("sessions".into(), "old".into());

            assert_eq!(prefs.forget_sources_in_gone_sets(), [gone, whole]);
            assert_eq!(specs(&prefs), [kept.as_str(), outside.as_str()]);
            assert!(prefs.sample_source_renames.is_empty());

            let mut unplugged = with_sources(std::slice::from_ref(&kept));
            unplugged.sets_directory = Some(home.path().join("unplugged").display().to_string());
            assert!(unplugged.forget_sources_in_gone_sets().is_empty());
            assert_eq!(specs(&unplugged), [kept.as_str()]);
        }
    }

    #[test]
    fn piano_preferences_default_bound_and_round_trip() {
        use crate::PianoSound;
        for (json, sound, volume) in [
            (r#"{}"#, PianoSound::Triangle, 130),
            (
                r#"{"piano_sound":"piano","piano_volume":120}"#,
                PianoSound::Triangle,
                120,
            ),
            (
                r#"{"piano_sound":"sine","piano_volume":70}"#,
                PianoSound::Sine,
                70,
            ),
            (
                r#"{"piano_sound":"triangle","piano_volume":90}"#,
                PianoSound::Triangle,
                90,
            ),
            (
                r#"{"piano_sound":"square","piano_volume":0}"#,
                PianoSound::Square,
                0,
            ),
            (
                r#"{"piano_sound":"unknown","piano_volume":999}"#,
                PianoSound::Triangle,
                200,
            ),
        ] {
            let mut prefs: StudioPrefs = serde_json::from_str(json).unwrap();
            let settings = prefs.ui_settings();
            assert_eq!(
                (settings.piano_sound, settings.piano_volume),
                (sound, volume)
            );
            prefs.set_ui_settings(&settings);
            let saved = serde_json::to_string(&prefs).unwrap();
            let loaded: StudioPrefs = serde_json::from_str(&saved).unwrap();
            assert_eq!(
                (
                    loaded.ui_settings().piano_sound,
                    loaded.ui_settings().piano_volume
                ),
                (sound, volume)
            );
        }
    }

    #[test]
    fn polyphony_preferences_default_bound_and_round_trip() {
        for (json, expected) in [
            (r#"{}"#, 128),
            (r#"{"max_polyphony":0}"#, 128),
            (r#"{"max_polyphony":96}"#, 96),
            (r#"{"max_polyphony":256}"#, 256),
            (r#"{"max_polyphony":999999}"#, 256),
        ] {
            let mut prefs: StudioPrefs = serde_json::from_str(json).unwrap();
            let settings = prefs.ui_settings();
            assert_eq!(settings.max_polyphony, expected, "{json}");
            prefs.set_ui_settings(&settings);
            let saved = serde_json::to_string(&prefs).unwrap();
            let loaded: StudioPrefs = serde_json::from_str(&saved).unwrap();
            assert_eq!(loaded.ui_settings().max_polyphony, expected, "{saved}");
        }
    }

    use super::*;

    #[test]
    fn an_oversized_preferences_file_uses_defaults() {
        let directory = tempfile::tempdir().expect("temp dir");
        let path = directory.path().join(PREFS_FILE_NAME);
        // Valid JSON, refused for its length alone.
        let mut text = String::from(r#"{"theme":"night"}"#);
        text.push_str(&" ".repeat(MAX_DOCUMENT_BYTES as usize));
        std::fs::write(&path, text).expect("prefs file");

        assert_eq!(
            StudioPrefs::load_from(Some(&path)),
            StudioPrefs::load_from(None)
        );
    }

    #[test]
    fn a_file_that_does_not_parse_is_kept_aside_and_a_good_one_is_left() {
        let directory = tempfile::tempdir().expect("temp dir");
        let path = directory.path().join(PREFS_FILE_NAME);
        assert_eq!(super::keep_unreadable_aside(&path), None, "no file");

        std::fs::write(&path, r#"{"theme":"night"}"#).expect("prefs file");
        assert_eq!(super::keep_unreadable_aside(&path), None);
        assert!(path.exists());

        for kept in ["studio.json.bad", "studio.json.bad 2"] {
            std::fs::write(&path, r#"{"theme":"night",}"#).expect("broken prefs file");
            let kept = directory.path().join(kept);
            assert_eq!(super::keep_unreadable_aside(&path), Some(kept.clone()));
            assert!(!path.exists() && kept.exists());
        }
    }

    #[test]
    fn parent_source_inherits_aliases_when_child_row_is_collapsed() {
        let directory = tempfile::tempdir().expect("sample root");
        let parent = directory.path().join("samples");
        let child = parent.join("drums");
        let later_child = parent.join("percussion");
        std::fs::create_dir_all(&child).expect("nested sample folders");
        std::fs::create_dir_all(&later_child).expect("second nested sample folder");
        let parent = parent.to_string_lossy().into_owned();
        let child = child.to_string_lossy().into_owned();
        let later_child = later_child.to_string_lossy().into_owned();
        let mut prefs = StudioPrefs {
            sample_sources: vec![
                SampleSourcePref {
                    spec: child.clone(),
                    enabled: true,
                },
                SampleSourcePref {
                    spec: parent.clone(),
                    enabled: true,
                },
                SampleSourcePref {
                    spec: later_child.clone(),
                    enabled: true,
                },
            ],
            ..StudioPrefs::default()
        };
        prefs
            .sample_source_renames
            .entry(child.clone())
            .or_default()
            .extend([
                ("kick".into(), "deep-kick".into()),
                ("clash".into(), "earlier-child".into()),
                ("shared".into(), "child-name".into()),
            ]);
        prefs
            .sample_source_renames
            .entry(later_child.clone())
            .or_default()
            .insert("clash".into(), "later-child".into());
        prefs
            .sample_source_renames
            .entry(parent.clone())
            .or_default()
            .insert("shared".into(), "parent-name".into());

        assert!(prefs.deduplicate_sample_sources());
        assert_eq!(
            prefs.sample_sources,
            vec![SampleSourcePref {
                spec: parent.clone(),
                enabled: true,
            }]
        );
        assert!(!prefs.sample_source_renames.contains_key(&child));
        assert!(!prefs.sample_source_renames.contains_key(&later_child));
        let aliases = &prefs.sample_source_renames[&parent];
        assert_eq!(aliases["kick"], "deep-kick");
        assert_eq!(aliases["clash"], "later-child");
        assert_eq!(aliases["shared"], "parent-name");
    }

    /// The mixer panel comes back the way it was left: open or not, along
    /// which edge, and how tall.
    #[test]
    fn the_mixer_panel_is_remembered() {
        let directory = tempfile::tempdir().expect("temp dir");
        let path = directory.path().join(PREFS_FILE_NAME);
        let mut prefs = StudioPrefs::default();
        assert_eq!(prefs.mixer_panel, None);
        assert!(
            !prefs.ui_settings().mixer_top,
            "along the bottom by default"
        );
        prefs.mixer_panel = Some(true);
        prefs.mixer_rows = Some(20);
        let mut settings = prefs.ui_settings();
        settings.mixer_top = true;
        prefs.set_ui_settings(&settings);
        prefs.save_to(&path).expect("saved");
        let restored = StudioPrefs::load_from(Some(&path));
        assert_eq!(restored.mixer_panel, Some(true));
        assert_eq!(restored.mixer_top, Some(true));
        assert_eq!(restored.mixer_rows, Some(20));
        assert!(restored.ui_settings().mixer_top);
    }

    /// The sticky log keeps its edge across a restart: docked or a sheet,
    /// and along which edge if it is docked - the same two facts a visuals
    /// dock's `open` and `edge` are kept by.
    #[test]
    fn the_sticky_log_keeps_its_edge_across_a_restart() {
        use super::super::viz_panel::Edge;
        let directory = tempfile::tempdir().expect("temp dir");
        let path = directory.path().join(PREFS_FILE_NAME);
        let mut prefs = StudioPrefs::default();
        assert_eq!(prefs.log_sticky, None, "not sticky until asked");
        assert_eq!(prefs.log_edge, None);
        prefs.log_sticky = Some(true);
        prefs.log_edge = Some(Edge::Top);
        prefs.save_to(&path).expect("saved");
        let restored = StudioPrefs::load_from(Some(&path));
        assert_eq!(restored.log_sticky, Some(true));
        assert_eq!(restored.log_edge, Some(Edge::Top));
    }

    /// The docked log keeps its height once `-` or `+` set it. A file with no
    /// height and a side edge still reads: it keeps the side, and the app
    /// brings the log back along the bottom.
    #[test]
    fn the_docked_logs_height_is_kept_and_an_older_file_still_reads() {
        use super::super::viz_panel::Edge;
        let directory = tempfile::tempdir().expect("temp dir");
        let path = directory.path().join(PREFS_FILE_NAME);
        let mut prefs = StudioPrefs::default();
        assert_eq!(prefs.log_height, None, "a third of the terminal until set");
        prefs.log_height = Some(17);
        prefs.save_to(&path).expect("saved");
        assert_eq!(StudioPrefs::load_from(Some(&path)).log_height, Some(17));

        std::fs::write(&path, r#"{"log_sticky": true, "log_edge": "left"}"#).expect("written");
        let older = StudioPrefs::load_from(Some(&path));
        assert_eq!(older.log_sticky, Some(true), "the file was read, not reset");
        assert_eq!(older.log_edge, Some(Edge::Left));
        assert_eq!(older.log_height, None);
        assert_eq!(older.log_edge.map(Edge::band), Some(Edge::Bottom));
    }

    /// The memory breakdown keeps whether it was docked, its edge and its
    /// height across a restart; a file from before it was docked reads as
    /// closed.
    #[test]
    fn the_memory_breakdown_keeps_its_dock_across_a_restart() {
        use super::super::viz_panel::Edge;
        let directory = tempfile::tempdir().expect("temp dir");
        let path = directory.path().join(PREFS_FILE_NAME);
        std::fs::write(&path, r#"{"log_sticky": true}"#).expect("written");
        let older = StudioPrefs::load_from(Some(&path));
        assert_eq!(older.log_sticky, Some(true), "the file was read");
        assert_eq!(
            (older.memory_docked, older.memory_edge, older.memory_height),
            (None, None, None)
        );

        let mut prefs = older;
        prefs.memory_docked = Some(true);
        prefs.memory_edge = Some(Edge::Top);
        prefs.memory_height = Some(8);
        prefs.save_to(&path).expect("saved");
        let restored = StudioPrefs::load_from(Some(&path));
        assert_eq!(restored.memory_docked, Some(true));
        assert_eq!(restored.memory_edge, Some(Edge::Top));
        assert_eq!(restored.memory_height, Some(8));
    }

    /// The twelve mapping slots survive a restart, in order and with
    /// their channels: an unbound slot is written too, or the slot a
    /// mapping sits in would depend on how many came before it.
    #[test]
    fn the_mapping_slots_are_remembered() {
        use super::super::settings::{MAPPING_SLOTS, MappingSource, SliderCc, StickAxis};
        let directory = tempfile::tempdir().expect("temp dir");
        let path = directory.path().join(PREFS_FILE_NAME);
        let mut prefs = StudioPrefs::default();
        let defaults = prefs.ui_settings();
        assert_eq!(
            defaults.mappings, [None; MAPPING_SLOTS],
            "nothing is bound until a player learns it: there is no CC map \
         every controller agrees on"
        );
        let mut settings = defaults.clone();
        settings.mappings[0] = Some(MappingSource::Knob(SliderCc {
            controller: 74,
            channel: Some(2),
        }));
        settings.mappings[2] = Some(MappingSource::Knob(SliderCc {
            controller: 90,
            channel: None,
        }));
        settings.mappings[11] = Some(MappingSource::Stick(StickAxis::Y2));
        prefs.set_ui_settings(&settings);
        prefs.save_to(&path).expect("saved");
        let restored = StudioPrefs::load_from(Some(&path));
        let saved = restored.mappings.clone().expect("written");
        assert_eq!(saved.len(), MAPPING_SLOTS);
        assert_eq!(saved[0], "cc74/ch2");
        assert_eq!(saved[1], "off");
        assert_eq!(saved[2], "cc90");
        assert_eq!(saved[11], "y2");
        assert_eq!(restored.ui_settings().mappings, settings.mappings);
        // A short or missing list is simply unbound slots, not a panic.
        let stubby = StudioPrefs {
            mappings: Some(vec!["cc7".to_owned()]),
            ..StudioPrefs::default()
        };
        let settings = stubby.ui_settings();
        assert_eq!(
            settings.mappings[0],
            Some(MappingSource::Knob(SliderCc {
                controller: 7,
                channel: None
            }))
        );
        assert_eq!(settings.mappings[1], None);
    }

    #[test]
    fn prefs_round_trip_and_missing_files_are_defaults() {
        let dir = std::env::temp_dir().join(format!("rustel-prefs-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("nested").join(PREFS_FILE_NAME);
        assert_eq!(StudioPrefs::load_from(Some(&path)), StudioPrefs::default());
        let prefs = StudioPrefs {
            theme: Some("solarized".into()),
            ..StudioPrefs::default()
        };
        prefs.save_to(&path).unwrap();
        assert_eq!(StudioPrefs::load_from(Some(&path)), prefs);
        assert!(
            std::fs::read_to_string(&path)
                .unwrap()
                .contains("\"theme\": \"solarized\"")
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A terminal profile round-trips by name, and an unset one writes no
    /// key. An unknown name reads as no profile.
    #[test]
    fn terminal_profile_defaults_round_trip_and_recover_from_unknown_names() {
        let mut prefs = StudioPrefs::default();
        assert!(prefs.ui_settings().terminal_profile.is_none());
        assert!(
            serde_json::to_value(&prefs)
                .unwrap()
                .get("terminal_profile")
                .is_none()
        );
        for name in super::super::terminal::conflicts::known() {
            let mut settings = prefs.ui_settings();
            settings.terminal_profile = Some(name.to_ascii_lowercase());
            prefs.set_ui_settings(&settings);
            let json = serde_json::to_string(&prefs).unwrap();
            let restored: StudioPrefs = serde_json::from_str(&json).unwrap();
            assert_eq!(
                restored.ui_settings().terminal_profile.as_deref(),
                Some(name)
            );
        }
        let unknown: StudioPrefs =
            serde_json::from_str(r#"{"terminal_profile":"removed-terminal"}"#).unwrap();
        assert!(unknown.ui_settings().terminal_profile.is_none());
        let mut settings = prefs.ui_settings();
        settings.terminal_profile = None;
        prefs.set_ui_settings(&settings);
        assert!(
            serde_json::to_value(&prefs)
                .unwrap()
                .get("terminal_profile")
                .is_none()
        );
    }

    #[test]
    fn keybinds_round_trip_and_an_untouched_table_writes_nothing() {
        let mut prefs = StudioPrefs::default();
        let json = serde_json::to_value(&prefs).expect("serialize");
        assert!(
            json.get("keybinds").is_none(),
            "an untouched table is absent, not empty: {json}"
        );

        let mut binds = prefs.keybinds();
        binds.learn(
            super::super::keybinds::BindAction::Undo,
            super::super::keybinds::KeyCombo::parse("ctrl+shift+q"),
        );
        prefs.set_keybinds(&binds);
        let restored: StudioPrefs =
            serde_json::from_str(&serde_json::to_string(&prefs).unwrap()).unwrap();
        assert_eq!(
            restored
                .keybinds()
                .binding(super::super::keybinds::BindAction::Undo)
                .expect("the learnt chord reads back")
                .key(),
            "ctrl+shift+q"
        );

        binds.learn(super::super::keybinds::BindAction::Undo, None);
        prefs.set_keybinds(&binds);
        assert!(prefs.keybinds.is_empty());
        let json = serde_json::to_value(&prefs).expect("serialize");
        assert!(json.get("keybinds").is_none(), "{json}");
    }

    #[test]
    fn legacy_graphics_override_is_ignored_and_not_written_back() {
        let mut prefs: StudioPrefs = serde_json::from_str(
            r#"{
            "graphics": "pixels",
            "animation": false,
            "highlights": true,
            "interface_opacity": 42
        }"#,
        )
        .expect("legacy preferences still load");

        let settings = prefs.ui_settings();
        assert_eq!(
            settings.rendering,
            super::super::graphics::RenderingMode::Automatic
        );
        assert!(!settings.animation);
        assert!(settings.highlights);
        assert_eq!(settings.interface_opacity, 42);

        prefs.set_ui_settings(&settings);
        let encoded = serde_json::to_value(&prefs).expect("preferences serialize");
        assert!(encoded.get("graphics").is_none());
        assert_eq!(
            encoded.get("animation"),
            Some(&serde_json::Value::Bool(false))
        );
        assert_eq!(
            encoded.get("interface_opacity"),
            Some(&serde_json::json!(42))
        );
    }

    /// Trimming a recording is on for a file that never mentioned it, and a
    /// player's choice to turn it off is the one found at the next launch.
    #[test]
    fn trim_recordings_defaults_on_and_remembers_off() {
        let legacy: StudioPrefs = serde_json::from_str("{}").unwrap();
        assert!(
            legacy.ui_settings().trim_recordings,
            "on unless told otherwise"
        );
        for enabled in [false, true] {
            let settings = super::super::settings::UiSettings {
                trim_recordings: enabled,
                ..legacy.ui_settings()
            };
            let mut prefs = StudioPrefs::default();
            prefs.set_ui_settings(&settings);
            let restored: StudioPrefs =
                serde_json::from_str(&serde_json::to_string(&prefs).unwrap()).unwrap();
            assert_eq!(restored.ui_settings().trim_recordings, enabled);
        }
    }

    #[test]
    fn scrollbars_default_on_and_remember_off() {
        let legacy: StudioPrefs = serde_json::from_str("{}").unwrap();
        assert!(legacy.ui_settings().show_scrollbars);
        for enabled in [false, true] {
            let settings = super::super::settings::UiSettings {
                show_scrollbars: enabled,
                ..legacy.ui_settings()
            };
            let mut prefs = StudioPrefs::default();
            prefs.set_ui_settings(&settings);
            let restored: StudioPrefs =
                serde_json::from_str(&serde_json::to_string(&prefs).unwrap()).unwrap();
            assert_eq!(restored.ui_settings().show_scrollbars, enabled);
        }
    }

    #[test]
    fn frequency_slider_travel_defaults_to_log_and_remembers_linear() {
        let legacy: StudioPrefs = serde_json::from_str("{}").unwrap();
        assert!(legacy.ui_settings().frequency_slider_log);
        for enabled in [false, true] {
            let settings = super::super::settings::UiSettings {
                frequency_slider_log: enabled,
                ..legacy.ui_settings()
            };
            let mut prefs = StudioPrefs::default();
            prefs.set_ui_settings(&settings);
            let restored: StudioPrefs =
                serde_json::from_str(&serde_json::to_string(&prefs).unwrap()).unwrap();
            assert_eq!(restored.ui_settings().frequency_slider_log, enabled);
        }
    }

    /// The syntax check is full for a file that never mentioned it, and a
    /// player's chosen mode is the one found at the next launch.
    #[test]
    fn syntax_check_defaults_full_and_remembers_each_mode() {
        use super::super::settings::SyntaxCheck;
        let legacy: StudioPrefs = serde_json::from_str("{}").unwrap();
        assert_eq!(legacy.ui_settings().syntax_check, SyntaxCheck::Full);
        for mode in [SyntaxCheck::Off, SyntaxCheck::OnUpdate, SyntaxCheck::Full] {
            let settings = super::super::settings::UiSettings {
                syntax_check: mode,
                ..legacy.ui_settings()
            };
            let mut prefs = StudioPrefs::default();
            prefs.set_ui_settings(&settings);
            assert_eq!(prefs.syntax_check_mode.as_deref(), Some(mode.key()));
            let restored: StudioPrefs =
                serde_json::from_str(&serde_json::to_string(&prefs).unwrap()).unwrap();
            assert_eq!(restored.ui_settings().syntax_check, mode);
        }
        // A file written by the two-way switch this replaced still loads.
        let switch: StudioPrefs = serde_json::from_str(r#"{"syntax_check": false}"#).unwrap();
        assert_eq!(switch.ui_settings().syntax_check, SyntaxCheck::Full);
    }

    #[test]
    fn evaluation_flash_defaults_full_and_remembers_each_mode() {
        use super::super::settings::EvaluationFlashMode;

        for json in ["{}", r#"{"evaluation_flash":"unknown"}"#] {
            let prefs: StudioPrefs = serde_json::from_str(json).unwrap();
            assert_eq!(
                prefs.ui_settings().evaluation_flash,
                EvaluationFlashMode::Full
            );
        }
        for (mode, key) in [
            (EvaluationFlashMode::Full, "full"),
            (EvaluationFlashMode::OnSuccess, "on-success"),
            (EvaluationFlashMode::Off, "off"),
        ] {
            let settings = super::super::settings::UiSettings {
                evaluation_flash: mode,
                ..Default::default()
            };
            let mut prefs = StudioPrefs::default();
            prefs.set_ui_settings(&settings);
            let encoded = serde_json::to_value(&prefs).unwrap();
            assert_eq!(encoded["evaluation_flash"], key);
            let restored: StudioPrefs = serde_json::from_value(encoded).unwrap();
            assert_eq!(restored.ui_settings().evaluation_flash, mode);
        }
    }

    /// The master limiter survives the file. An explicit `off` is written
    /// and kept, so it stays off if the default changes.
    #[test]
    fn the_master_limiter_survives_the_file_including_off() {
        use super::super::settings::UiSettings;

        assert_eq!(
            StudioPrefs::default().ui_settings().master_limiter(),
            None,
            "a file that has never seen a studio opens undelayed"
        );

        let round = |limiter: Option<rustel_audio::LimiterSettings>| {
            let settings = UiSettings {
                master_limiter_on: limiter.is_some(),
                master_limiter_character: limiter
                    .map_or(UiSettings::default().master_limiter_character, |held| {
                        held.character
                    }),
                master_limiter_ceiling_db: limiter
                    .map_or(UiSettings::default().master_limiter_ceiling_db, |held| {
                        held.threshold_db
                    }),
                ..UiSettings::default()
            };
            let mut prefs = StudioPrefs::default();
            prefs.set_ui_settings(&settings);
            let json = serde_json::to_string(&prefs).expect("write");
            let restored: StudioPrefs = serde_json::from_str(&json).expect("read");
            (restored.ui_settings().master_limiter(), json)
        };

        let (back, json) = round(None);
        assert_eq!(back, None);
        assert!(json.contains("\"master_limiter\":\"off\""), "{json}");

        for character in rustel_audio::LimiterCharacter::ALL {
            for threshold_db in [-0.5f32, -1.0, -6.0, -24.0] {
                let limiter = Some(rustel_audio::LimiterSettings {
                    threshold_db,
                    character,
                });
                let (back, _) = round(limiter);
                assert_eq!(back, limiter, "{} at {threshold_db}", character.key());
            }
        }

        // The ceiling survives a restart with the limiter OFF. It is the
        // whole reason it is a line of its own: a player who set -6 and
        // then switched off to hear something wants -6 back, not -1.
        let settings = UiSettings {
            master_limiter_on: false,
            master_limiter_character: rustel_audio::LimiterCharacter::Warm,
            master_limiter_ceiling_db: -6.0,
            ..UiSettings::default()
        };
        let mut prefs = StudioPrefs::default();
        prefs.set_ui_settings(&settings);
        let restored: StudioPrefs =
            serde_json::from_str(&serde_json::to_string(&prefs).expect("write")).expect("read");
        assert_eq!(restored.ui_settings().master_limiter(), None, "still off");
        assert!(
            (restored.ui_settings().master_limiter_ceiling_db + 6.0).abs() < 1e-6,
            "and still -6: {}",
            restored.ui_settings().master_limiter_ceiling_db
        );
        // And so does the character, for the same reason: switching off to
        // hear something and on again gives back the limiter that was
        // there, not the default one.
        assert_eq!(
            restored.ui_settings().master_limiter_character,
            rustel_audio::LimiterCharacter::Warm,
            "the mode outlives the switch"
        );

        // A ceiling from outside the range the sheet and the desk offer is
        // brought into it rather than believed.
        for (tenths, expected) in [(-400i16, -24.0f32), (60, 0.0), (-65, -6.5)] {
            let prefs = StudioPrefs {
                master_limiter_ceiling_tenths_db: Some(tenths),
                ..StudioPrefs::default()
            };
            assert!(
                (prefs.ui_settings().master_limiter_ceiling_db - expected).abs() < 1e-6,
                "{tenths} tenths became {}",
                prefs.ui_settings().master_limiter_ceiling_db
            );
        }

        // A line nobody can read is off, not on: the limiter is opted into,
        // so a corrupt file does not quietly start delaying the output.
        for nonsense in [
            "",
            "on",
            "yes:transparent",
            "-3.0:brickwall",
            "-3.0",
            "3.0:warm",
        ] {
            let prefs = StudioPrefs {
                master_limiter: Some(nonsense.to_owned()),
                ..StudioPrefs::default()
            };
            assert_eq!(
                prefs.ui_settings().master_limiter(),
                None,
                "{nonsense:?} is not an invitation to turn it on"
            );
        }
    }

    /// A kept off-ladder output size reads back as itself and is written
    /// back as itself; a size this build refuses plays as automatic but is
    /// not overwritten with "auto" until somebody chooses a size.
    #[test]
    fn an_output_latency_this_build_refuses_survives_a_persist() {
        let mut prefs: StudioPrefs =
            serde_json::from_str(r#"{"output_latency":"96"}"#).expect("prefs");
        let settings = prefs.ui_settings();
        assert_eq!(
            settings.output_latency,
            super::super::settings::OutputLatency::Frames(96)
        );
        prefs.set_ui_settings(&settings);
        assert_eq!(prefs.output_latency.as_deref(), Some("96"));

        let mut prefs: StudioPrefs =
            serde_json::from_str(r#"{"output_latency":"20000"}"#).expect("prefs");
        let mut settings = prefs.ui_settings();
        assert_eq!(
            settings.output_latency,
            super::super::settings::OutputLatency::Automatic
        );
        prefs.set_ui_settings(&settings);
        let written = serde_json::to_string(&prefs).expect("serialise");
        assert!(written.contains("20000"), "{written}");

        settings.output_latency = super::super::settings::OutputLatency::Frames256;
        prefs.set_ui_settings(&settings);
        assert_eq!(
            prefs.output_latency.as_deref(),
            Some("256"),
            "a choice wins"
        );
    }

    #[test]
    fn a_limiter_this_build_cannot_read_survives_a_persist() {
        // `set_ui_settings` must keep an unreadable limiter line unchanged. A
        // write-back from the settings would replace it with `off`.
        for unreadable in ["-1.0:gluey", "-3.0:brickwall", "-3.0", ""] {
            let mut prefs = StudioPrefs {
                master_limiter: Some(unreadable.to_owned()),
                ..StudioPrefs::default()
            };
            let settings = prefs.ui_settings();
            assert_eq!(
                settings.master_limiter(),
                None,
                "{unreadable:?} plays as off"
            );
            prefs.set_ui_settings(&settings);
            assert_eq!(
                prefs.master_limiter.as_deref(),
                Some(unreadable),
                "{unreadable:?} comes back word for word"
            );
            assert_eq!(
                prefs.ui_settings().master_limiter(),
                None,
                "{unreadable:?} still reads as off, not as a guess"
            );
        }

        // `off` is not unreadable - it is the one word this build writes
        // itself - so it is not mistaken for a line worth preserving.
        let mut prefs = StudioPrefs {
            master_limiter: Some(super::super::scenes::OFF_LIMITER.to_owned()),
            ..StudioPrefs::default()
        };
        let settings = prefs.ui_settings();
        prefs.set_ui_settings(&settings);
        assert_eq!(prefs.master_limiter.as_deref(), Some("off"));

        // A real choice does overwrite it: the player picked a character, so
        // the unreadable line has been replaced by an opinion rather than
        // quietly kept behind one.
        let mut prefs = StudioPrefs {
            master_limiter: Some("-1.0:gluey".to_owned()),
            ..StudioPrefs::default()
        };
        let mut settings = prefs.ui_settings();
        settings.master_limiter_on = true;
        settings.master_limiter_character = rustel_audio::LimiterCharacter::Warm;
        prefs.set_ui_settings(&settings);
        let written = prefs.master_limiter.clone().expect("written");
        assert!(
            written.ends_with(":warm"),
            "the chosen character replaces the unreadable line: {written:?}"
        );
        assert_eq!(
            super::super::settings::parse_master_limiter(&written).map(|held| held.character.key()),
            Some("warm"),
            "and what replaced it reads back"
        );
    }

    #[test]
    fn rendering_and_path_privacy_roundtrip_without_reviving_legacy_graphics() {
        use super::super::graphics::RenderingMode;
        assert_eq!(
            StudioPrefs::default().ui_settings().rendering,
            RenderingMode::Automatic
        );
        assert!(!StudioPrefs::default().ui_settings().show_full_paths);
        for rendering in [
            RenderingMode::Automatic,
            RenderingMode::Cells,
            RenderingMode::Fine,
            RenderingMode::Kitty,
        ] {
            let settings = super::super::settings::UiSettings {
                rendering,
                show_full_paths: true,
                slider_smoothing: true,
                ..super::super::settings::UiSettings::default()
            };
            let mut prefs = StudioPrefs::default();
            prefs.set_ui_settings(&settings);
            let json = serde_json::to_string(&prefs).unwrap();
            let restored: StudioPrefs = serde_json::from_str(&json).unwrap();
            assert_eq!(restored.ui_settings().rendering, rendering);
            assert!(restored.ui_settings().show_full_paths);
            assert!(restored.ui_settings().slider_smoothing);
        }
        // A file written before a row existed opens on its default rather
        // than on nothing.
        let older: StudioPrefs = serde_json::from_str(r#"{"animation":true}"#).unwrap();
        assert_eq!(
            older.ui_settings().output_latency,
            super::super::settings::OutputLatency::Automatic
        );

        let unknown: StudioPrefs =
            serde_json::from_str(r#"{"rendering":"future-backend"}"#).unwrap();
        assert_eq!(unknown.ui_settings().rendering, RenderingMode::Automatic);
    }

    /// A studio nobody has opened the settings sheet in keeps a
    /// `studio.json` with no `metric_detail` line at all, and reads back
    /// on the same header it always drew; a player who did choose a level
    /// gets that level back, whichever one it was.
    #[test]
    fn metric_detail_round_trips_and_an_untouched_setting_writes_nothing() {
        use super::super::settings::MetricDetail;

        let prefs = StudioPrefs::default();
        let json = serde_json::to_value(&prefs).expect("serialize");
        assert!(
            json.get("metric_detail").is_none(),
            "an untouched setting is absent, not the default spelled out: {json}"
        );
        assert_eq!(prefs.ui_settings().metric_detail, MetricDetail::Basic);

        for level in [
            MetricDetail::None,
            MetricDetail::Basic,
            MetricDetail::Advanced,
            MetricDetail::Full,
        ] {
            let settings = super::super::settings::UiSettings {
                metric_detail: level,
                ..super::super::settings::UiSettings::default()
            };
            let mut prefs = StudioPrefs::default();
            prefs.set_ui_settings(&settings);
            let restored: StudioPrefs =
                serde_json::from_str(&serde_json::to_string(&prefs).unwrap()).unwrap();
            assert_eq!(restored.ui_settings().metric_detail, level);
        }

        let unknown: StudioPrefs =
            serde_json::from_str(r#"{"metric_detail":"unheard-of"}"#).unwrap();
        assert_eq!(
            unknown.ui_settings().metric_detail,
            MetricDetail::default(),
            "a name nobody wrote is the default, whatever the default is"
        );
    }

    #[test]
    fn chrome_visibility_round_trips_and_an_untouched_setting_writes_nothing() {
        let prefs = StudioPrefs::default();
        let json = serde_json::to_value(&prefs).expect("serialize");
        for key in ["show_menu", "show_header", "show_footer"] {
            assert!(
                json.get(key).is_none(),
                "an untouched setting is absent, not the default spelled out: {json}"
            );
        }
        let settings = prefs.ui_settings();
        assert!(settings.show_menu && settings.show_header && settings.show_footer);

        for (menu, header, footer) in [
            (false, true, true),
            (true, false, true),
            (true, true, false),
            (false, false, false),
        ] {
            let settings = super::super::settings::UiSettings {
                show_menu: menu,
                show_header: header,
                show_footer: footer,
                ..super::super::settings::UiSettings::default()
            };
            let mut prefs = StudioPrefs::default();
            prefs.set_ui_settings(&settings);
            let restored: StudioPrefs =
                serde_json::from_str(&serde_json::to_string(&prefs).unwrap()).unwrap();
            let loaded = restored.ui_settings();
            assert_eq!(loaded.show_menu, menu);
            assert_eq!(loaded.show_header, header);
            assert_eq!(loaded.show_footer, footer);
            assert!(!loaded.zen, "zen is still not remembered");
        }

        let absent: StudioPrefs = serde_json::from_str(r#"{}"#).unwrap();
        let defaults = absent.ui_settings();
        assert!(defaults.show_menu && defaults.show_header && defaults.show_footer);
    }

    #[cfg(feature = "hydra")]
    #[test]
    fn hydra_webcam_consent_is_explicit_and_persisted() {
        let mut settings = super::super::settings::UiSettings::default();
        assert!(!settings.hydra_webcam);
        settings.hydra_webcam = true;
        let mut prefs = StudioPrefs::default();
        prefs.set_ui_settings(&settings);
        assert_eq!(prefs.hydra_webcam, Some(true));
        assert!(prefs.ui_settings().hydra_webcam);

        let encoded = serde_json::to_string(&prefs).unwrap();
        let decoded: StudioPrefs = serde_json::from_str(&encoded).unwrap();
        assert!(decoded.ui_settings().hydra_webcam);
        assert!(!StudioPrefs::default().ui_settings().hydra_webcam);
    }

    /// A preferences file from a build that knew keys this one does not -
    /// the retired webcam thumbnail switch among them - still loads, and
    /// the retired key simply leaves the file the next time it is written.
    #[test]
    fn a_retired_preference_key_is_ignored_rather_than_refused() {
        let mut prefs: StudioPrefs =
            serde_json::from_str(r#"{"webcam_preview_braille":true,"hydra_webcam":false}"#)
                .expect("an unknown key is not a parse error");
        let settings = prefs.ui_settings();
        #[cfg(feature = "hydra")]
        assert!(!settings.hydra_webcam);
        prefs.set_ui_settings(&settings);
        let written = serde_json::to_string(&prefs).unwrap();
        assert!(
            !written.contains("webcam_preview_braille"),
            "the retired key is dropped, not carried: {written}"
        );
        let decoded: StudioPrefs = serde_json::from_str(&written).unwrap();
        assert_eq!(decoded.hydra_webcam, Some(false));
    }

    /// The set open now is the last set and the newest of the recent
    /// ones, a set opened again moves to the front, the list stays short,
    /// and a set deleted from the disk is forgotten.
    #[test]
    fn recent_sets_are_a_short_list_newest_first() {
        let mut prefs = StudioPrefs::default();
        for name in ["a", "b", "c"] {
            prefs.remember_set(std::path::Path::new(name));
        }
        assert_eq!(prefs.last_set.as_deref(), Some("c"));
        assert_eq!(prefs.recent_sets, ["c", "b", "a"]);
        prefs.remember_set(std::path::Path::new("a"));
        assert_eq!(prefs.recent_sets, ["a", "c", "b"]);
        for number in 0..RECENT_SETS {
            prefs.remember_set(std::path::Path::new(&format!("set {number}")));
        }
        assert_eq!(prefs.recent_sets.len(), RECENT_SETS);
        assert!(!prefs.recent_sets.iter().any(|recent| recent == "b"));
        let newest = prefs.last_set.clone().unwrap();
        prefs.forget_set(std::path::Path::new(&newest));
        assert!(prefs.last_set.is_none());
        assert!(!prefs.recent_sets.contains(&newest));
        assert_eq!(
            prefs.sets_directory(),
            super::super::config::directory().map(|directory| directory.join(SETS_DIRECTORY_NAME))
        );
        prefs.sets_directory = Some("/tmp/my sets".into());
        assert_eq!(prefs.sets_directory(), Some(PathBuf::from("/tmp/my sets")));
        assert_eq!(
            prefs.recordings_directory(),
            super::super::config::directory()
                .map(|directory| directory.join(rustel_runtime::product::RECORDINGS_DIRECTORY_NAME))
        );
        prefs.recordings_directory = Some("/tmp/my recordings".into());
        assert_eq!(
            prefs.recordings_directory(),
            Some(PathBuf::from("/tmp/my recordings"))
        );
    }

    /// The load mode is kept: wait unless async was chosen, and an
    /// unreadable value reads as wait. `strudel-like` also reads as async.
    #[test]
    fn the_load_mode_is_kept_and_waits_by_default() {
        use crate::settings::LoadMode;
        let mut prefs = StudioPrefs::default();
        assert_eq!(prefs.ui_settings().load_mode, LoadMode::Wait);
        let mut settings = prefs.ui_settings();
        settings.load_mode = LoadMode::Async;
        prefs.set_ui_settings(&settings);
        let text = serde_json::to_string(&prefs).expect("prefs write");
        assert!(text.contains(r#""load_mode":"async""#), "{text}");
        let reread: StudioPrefs = serde_json::from_str(&text).expect("prefs read");
        assert_eq!(reread.ui_settings().load_mode, LoadMode::Async);
        let old_name: StudioPrefs =
            serde_json::from_str(r#"{"load_mode":"strudel-like"}"#).expect("old key");
        assert_eq!(old_name.ui_settings().load_mode, LoadMode::Async);
        let unreadable: StudioPrefs =
            serde_json::from_str(r#"{"load_mode":"sometimes"}"#).expect("prefs read");
        assert_eq!(unreadable.ui_settings().load_mode, LoadMode::Wait);
    }
}
