//! The plugin host of the process, and the numbers for a `.vst()` request.
//!
//! A plugin library loads one time in a process, so the process has one
//! host. The host starts when a score names a plugin, when the user opens
//! the plugin list, or with [`scan_in_background`]. Before this, the
//! process loads no plugin code.

mod chains;
mod lanes;

pub(crate) use chains::{
    Chains, NoteChain, Want, assign, ensure, forget_lanes_away, link_sliders, load, plugin_names,
    render_chains, score_changed, slider_links, slider_moved, take_output_lines, take_problems,
    wants,
};
#[cfg(feature = "device-audio")]
pub(crate) use chains::{forget_plan, named_chains, plan, take_slider_values, tick};
pub(crate) use lanes::LaneTable;

use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use rustel_audio::InsertControls;
pub use rustel_vst3::{
    Host, ParamInfo, ParamRole, PluginInfo, Resolved, ScanProgress, Status, canonical, serve,
};

/// Folders with plugins, in place of the standard VST3 folders of the
/// system. The form is the form of `PATH`.
pub const FOLDERS_ENV: &str = "RUSTEL_VST3_PATH";
/// Plugins whose copies run on the audio thread only, by name, split at
/// `;`. The variable takes the place of the switches of
/// [`toggle_audio_thread_only`].
pub const AUDIO_THREAD_ENV: &str = "RUSTEL_PLUGIN_AUDIO_THREAD";
/// The folder in the data folder with one folder of presets for each plugin.
pub const PRESETS_DIRECTORY_NAME: &str = "vst";

static HOST: OnceLock<Host> = OnceLock::new();
static USER_FOLDERS: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());
static PINNED_FOLDERS: Mutex<Option<Vec<PathBuf>>> = Mutex::new(None);
static WORKER: Mutex<Option<rustel_vst3::WorkerProgram>> = Mutex::new(None);
/// The names of [`set_audio_thread_plugins`]. `None` before the first read
/// of the switches: see [`audio_thread_plugins`].
static AUDIO_THREAD: Mutex<Option<Vec<String>>> = Mutex::new(None);
/// The fault of the first read of the switches, until a plugin line says
/// it: see [`take_errors`].
static AUDIO_THREAD_FAULT: Mutex<Option<String>> = Mutex::new(None);

/// The folders the host reads with no setting from the user: the folders of
/// [`FOLDERS_ENV`] when the variable is set, and the standard VST3 folders
/// of the system when not.
pub fn standard_folders() -> Vec<PathBuf> {
    if let Some(pinned) = PINNED_FOLDERS.lock().expect("plugin folders").clone() {
        return pinned;
    }
    match std::env::var_os(FOLDERS_ENV) {
        Some(listed) => std::env::split_paths(&listed)
            .filter(|path| !path.as_os_str().is_empty())
            .collect(),
        None => rustel_vst3::default_folders(),
    }
}

/// The folders the user added.
pub fn user_folders() -> Vec<PathBuf> {
    USER_FOLDERS.lock().expect("plugin folders").clone()
}

/// Every folder the host reads: the folders the user added, then
/// [`standard_folders`].
pub fn folders() -> Vec<PathBuf> {
    let mut folders = user_folders();
    folders.extend(standard_folders());
    folders
}

/// Test support: these folders take the place of [`standard_folders`], so a
/// test never reads the plugins of the machine.
#[cfg(any(test, feature = "test-support"))]
pub fn pin_standard_folders(folders: Vec<PathBuf>) {
    *PINNED_FOLDERS.lock().expect("plugin folders") = Some(folders);
    if let Some(host) = started() {
        host.scan(&self::folders());
        host.wait_idle();
    }
}

/// The folder with the preset folders: `~/.rustel/vst`.
pub fn presets_folder() -> Option<PathBuf> {
    crate::config_dir::canonical().map(|data| data.join(PRESETS_DIRECTORY_NAME))
}

/// The host, started on first use.
pub fn host() -> &'static Host {
    #[cfg(test)]
    tests::HOST_CALLS.set(tests::HOST_CALLS.get() + 1);
    HOST.get_or_init(|| {
        let host = Host::new();
        if let Some(presets) = presets_folder() {
            host.set_preset_folder(presets);
        }
        host.set_worker(WORKER.lock().expect("worker program").clone());
        host.set_audio_thread_plugins(&audio_thread_plugins(std::env::var_os(AUDIO_THREAD_ENV)));
        host.scan(&folders());
        host
    })
}

/// Sets the program that loads a plugin bundle in a process of its own for
/// the scan: the host runs `program`, `args`, then the bundle path, and the
/// program calls [`serve`] with the path. A bundle with a fault at its load
/// then ends that process and not this one. With no such program, the host
/// runs no scan.
pub fn set_worker(program: PathBuf, args: Vec<std::ffi::OsString>) {
    let worker = rustel_vst3::WorkerProgram { program, args };
    *WORKER.lock().expect("worker program") = Some(worker.clone());
    if let Some(host) = started() {
        host.set_worker(Some(worker));
    }
}

/// Keeps the copies of the plugins in `names` on the audio thread, for a
/// plugin that breaks on another thread. A name is the whole name of a
/// plugin. A copy an output takes after the call follows the list.
/// [`AUDIO_THREAD_ENV`] takes the place of this list.
fn set_audio_thread_plugins(names: Vec<String>) {
    *AUDIO_THREAD.lock().expect("audio thread plugins") = Some(names);
    if let Some(host) = started() {
        host.set_audio_thread_plugins(&audio_thread_plugins(std::env::var_os(AUDIO_THREAD_ENV)));
    }
}

/// Turns the switch `audio thread only` of the plugin `name` on when the
/// user settings have it off, and off when they have it on, as Space on the
/// vst page of Studio does: see [`Host::set_audio_thread_plugins`]. The user
/// settings keep the switches, and the host reads them at its start, in
/// Studio and in `rustel play`. Gives the new state of the switch, or `None`
/// when [`AUDIO_THREAD_ENV`] takes the place of the switches.
pub fn toggle_audio_thread_only(name: &str) -> std::io::Result<Option<bool>> {
    let mut names = crate::settings::plugin_audio_thread()?;
    let key = canonical(name);
    let on = !names.iter().any(|listed| canonical(listed) == key);
    names.retain(|listed| canonical(listed) != key);
    if on {
        names.push(name.to_owned());
    }
    crate::settings::set_plugin_audio_thread(&names)?;
    set_audio_thread_plugins(names);
    Ok(std::env::var_os(AUDIO_THREAD_ENV).is_none().then_some(on))
}

/// The plugins whose copies run on the audio thread only: the names in
/// `listed`, the value of [`AUDIO_THREAD_ENV`], split at `;`. With no value,
/// the names of [`set_audio_thread_plugins`]. Before its first call, the
/// switches in the user settings: see [`toggle_audio_thread_only`].
fn audio_thread_plugins(listed: Option<std::ffi::OsString>) -> Vec<String> {
    match listed {
        Some(listed) => listed
            .to_string_lossy()
            .split(';')
            .map(str::to_owned)
            .collect(),
        None => {
            // Settings the host cannot read keep no plugin on the audio
            // thread. A plugin line says why: see `take_errors`. With no
            // config folder there is no switch, as with no file.
            let stored = || {
                crate::settings::plugin_audio_thread().unwrap_or_else(|error| {
                    if error.kind() != std::io::ErrorKind::NotFound {
                        let fault = format!("audio thread only off for each plugin: {error}");
                        *AUDIO_THREAD_FAULT.lock().expect("audio thread fault") = Some(fault);
                    }
                    Vec::new()
                })
            };
            let mut names = AUDIO_THREAD.lock().expect("audio thread plugins");
            names.get_or_insert_with(stored).clone()
        }
    }
}

/// The host, if a score or the user asked for a plugin before.
pub fn started() -> Option<&'static Host> {
    #[cfg(test)]
    tests::HOST_CALLS.set(tests::HOST_CALLS.get() + 1);
    HOST.get()
}

/// Sets the folders the user added, and reads all folders again. The read
/// runs on the plugin thread: see [`scanning`].
pub fn set_user_folders(folders: Vec<PathBuf>) {
    *USER_FOLDERS.lock().expect("plugin folders") = folders;
    if let Some(host) = started() {
        host.scan(&self::folders());
    }
}

/// Reads all folders again: the scan reads each new bundle, and each
/// bundle with changed files. The read runs on the plugin thread: see
/// [`scanning`].
pub fn scan() {
    host().scan(&folders());
}

/// Reads all folders again, with an empty scan cache: the scan reads each
/// bundle that is not loaded again, a bundle that failed too. The read
/// runs on the plugin thread: see [`scanning`].
pub fn rescan() {
    host().rescan(&folders());
}

/// The plugin names a score text asks for with `.vst("name")` or
/// `.vsti("name")`, as the text writes them. A name in a comment counts
/// too: the list says which plugins to keep loaded, and one more does no
/// harm.
pub fn names_in_source(source: &str) -> Vec<String> {
    let mut names = Vec::new();
    for call in ["vst(", "vsti("] {
        let mut rest = source;
        while let Some(at) = rest.find(call) {
            let before = rest[..at].chars().next_back();
            rest = &rest[at + call.len()..];
            // `myvst(` is a different call.
            if before.is_some_and(|char| char.is_alphanumeric() || char == '_') {
                continue;
            }
            let argument = rest.trim_start();
            let Some(quote) = argument
                .chars()
                .next()
                .filter(|char| "\"'`".contains(*char))
            else {
                continue;
            };
            if let Some(end) = argument[1..].find(quote) {
                let name = &argument[1..1 + end];
                if !name.is_empty() && !names.iter().any(|known| known == name) {
                    names.push(name.to_owned());
                }
            }
        }
    }
    names
}

/// The quoted strings of a score text outside the comments. A call that
/// reads its name from a variable, as `.vst(fx)` after `const fx = 'ott'`,
/// has the name in a string: see [`whole_names`].
pub fn strings_in_source(source: &str) -> Vec<String> {
    let strings = crate::lint::string_literals(source).into_iter();
    let mut strings: Vec<String> = strings
        .map(|literal| source[literal.content].to_owned())
        .collect();
    strings.sort_unstable();
    strings.dedup();
    strings
}

/// The strings that are the whole name or the bundle name of a plugin:
/// see [`Host::whole_names`]. Such a string keeps its plugin loaded. A
/// part of a name keeps nothing, so `s('piano')` keeps no `Pianoteq 8`.
/// Empty before the host starts.
pub fn whole_names(strings: &[String]) -> Vec<String> {
    started().map_or_else(Vec::new, |host| host.whole_names(strings))
}

/// Unloads each plugin with no name in `keep` and no running copy: see
/// [`Host::unload_unused`]. Does nothing before the host starts, and waits
/// for nothing.
pub fn unload_unused(keep: &[String]) {
    if let Some(host) = started() {
        host.unload_unused(keep);
    }
}

/// True when an engine holds a copy of a plugin with no name in `keep`: see
/// [`Host::engine_holds_unkept`]. False before the host starts.
pub fn engine_holds_unkept(keep: &[String]) -> bool {
    started().is_some_and(|host| host.engine_holds_unkept(keep))
}

/// How long the end of the process waits for the plugin thread.
const EXIT_LIMIT: Duration = Duration::from_secs(10);

/// Unloads each plugin and waits for the plugin thread, [`EXIT_LIMIT`] at
/// most. A program calls this as its last step, after its outputs ended. A
/// plugin library that ends with the process while the plugin thread still
/// works in the library stops the process with a fault.
pub fn shutdown() {
    if let Some(host) = started() {
        host.shutdown(EXIT_LIMIT);
    }
}

/// Whether the plugins need the process's main thread: on macOS, yes.
/// See [`install_main_service`] for the contract.
pub fn needs_the_main_thread() -> bool {
    rustel_vst3::needs_the_main_thread()
}

/// Registers the process's main thread as the server of the plugin
/// creation and release steps, on macOS. No-op when the service is
/// already installed or the caller is not the process's main thread.
pub fn install_main_service() {
    rustel_vst3::install_main_service();
}

/// Serves the plugin steps that arrived, on the calling thread: the main
/// thread of a program with the service installed calls this every few
/// milliseconds, also while it shuts down.
pub fn pump_main() {
    rustel_vst3::pump_main();
}

/// True while the host reads the plugin folders. The plugin list is not
/// complete before the end of the read.
pub fn scanning() -> bool {
    started().is_some_and(Host::scanning)
}

/// The numbers the audio engine carries for a `.vst()` request.
///
/// The host loads the plugin on first use and does not wait: `Ok(None)`
/// means the plugin loads now. A note drops a value the plugin does not
/// take and keeps its plugin: see [`rustel_vst3::Plugin::controls`]. The
/// reason is a notice of the conversion, so a conversion that drops its
/// notices says nothing.
pub(crate) fn controls(
    request: &rustel_voice::PluginRequest<'_>,
) -> Result<Option<InsertControls>, String> {
    let Some((key, loaded)) = host().key(request.name, request.preset, request.instrument)? else {
        return Ok(None);
    };
    let (controls, dropped) = loaded.controls(key, request.params);
    for reason in dropped {
        rustel_voice::plugin_value_skipped(request, &reason);
    }
    Ok(Some(controls))
}

/// Starts the host and the read of each plugin bundle with no plugin names
/// in the scan cache, as a DAW scans its plugins. The work runs on threads
/// of the host and in processes of their own, so the caller goes on at
/// once. The file `scan.json` in [`presets_folder`] keeps the result: a
/// bundle with the same files is not read again.
pub fn scan_in_background() {
    host().scan_plugins();
}

/// Where the scan is, while it runs: see [`scan_in_background`].
pub fn scan_progress() -> Option<ScanProgress> {
    started().and_then(Host::scan_progress)
}

/// The plugins in their load now, one name for each bundle. Empty before
/// the host starts.
pub fn loads() -> Vec<String> {
    started().map(Host::loads).unwrap_or_default()
}

/// The problems of the plugin thread since the last call, after the fault
/// of the first read of the switches: see [`audio_thread_plugins`].
pub(crate) fn take_errors() -> Vec<String> {
    let fault = AUDIO_THREAD_FAULT
        .lock()
        .expect("audio thread fault")
        .take();
    let errors = started().map(Host::take_errors).unwrap_or_default();
    fault.into_iter().chain(errors).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    // The calls of this thread to `host()` and `started()`. The host serves
    // the whole process, so a test reads the calls of its own thread.
    std::thread_local! {
        pub(super) static HOST_CALLS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    }

    #[test]
    fn a_score_text_gives_the_plugin_names_of_its_calls_and_its_strings() {
        let source = r#"
            $: note("c2").vsti("Serum 2", { macro1: 0.5 }).vst( 'ott' )
            $: s("bd").vst(`valhalla supermassive`).myvst("no").vst(name)
            // .vst("ott") again, and .vsti("") with no name
        "#;
        assert_eq!(
            names_in_source(source),
            ["ott", "valhalla supermassive", "Serum 2"]
        );
        // Each string outside the comments counts, as a name in a variable. A
        // string in a comment does not.
        let variable = "const fx = 'Pro-Q 3' // 'Vital'\n$: s('bd').vst(fx) // .vst('OTT')";
        assert_eq!(names_in_source(variable), ["OTT"]);
        assert_eq!(strings_in_source(variable), ["Pro-Q 3", "bd"]);
    }

    /// The variable of the plugins on the audio thread splits its names at
    /// `;`, and takes the place of the switches.
    #[test]
    fn the_audio_thread_variable_takes_the_place_of_the_switches() {
        set_audio_thread_plugins(vec!["Vital".into()]);
        assert_eq!(audio_thread_plugins(None), ["Vital"]);
        let listed = audio_thread_plugins(Some("Serum 2;OTT".into()));
        assert_eq!(listed, ["Serum 2", "OTT"]);
        set_audio_thread_plugins(Vec::new());
        assert!(audio_thread_plugins(None).is_empty());
    }
}
