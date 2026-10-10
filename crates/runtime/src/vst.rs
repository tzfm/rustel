//! The plugin host of the process, and the numbers for a `.vst()` request.
//!
//! A plugin library loads one time in a process, so the process has one
//! host. The host starts when a score names a plugin, when the user opens
//! the plugin list, or with [`scan_in_background`]. Before this, the
//! process loads no plugin code.

use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

use rustel_audio::{InsertControls, InsertKey, InsertParam, MAX_INSERT_PARAMS};
pub use rustel_vst3::{Host, ParamInfo, PluginInfo, Resolved, Status, canonical, serve};

/// Folders with plugins, in place of the standard VST3 folders of the
/// system. The form is the form of `PATH`.
pub const FOLDERS_ENV: &str = "RUSTEL_VST3_PATH";
/// The folder in the data folder with one folder of presets for each plugin.
pub const PRESETS_DIRECTORY_NAME: &str = "vst";

static HOST: OnceLock<Host> = OnceLock::new();
static USER_FOLDERS: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());
static PINNED_FOLDERS: Mutex<Option<Vec<PathBuf>>> = Mutex::new(None);
static WORKER: Mutex<Option<rustel_vst3::WorkerProgram>> = Mutex::new(None);

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
    HOST.get_or_init(|| {
        let host = Host::new();
        if let Some(presets) = presets_folder() {
            host.set_preset_folder(presets);
        }
        host.set_worker(WORKER.lock().expect("worker program").clone());
        host.scan(&folders());
        host
    })
}

/// Sets the program that runs each plugin bundle in a process of its own:
/// the host runs `program`, `args`, then the bundle path, and the program
/// calls [`serve`] with the path. A plugin with a fault, at its load or in
/// the middle of a set, then ends that process and not this one. With no
/// such program, a bundle loads in this process.
pub fn set_worker(program: PathBuf, args: Vec<std::ffi::OsString>) {
    let worker = rustel_vst3::WorkerProgram { program, args };
    *WORKER.lock().expect("worker program") = Some(worker.clone());
    if let Some(host) = started() {
        host.set_worker(Some(worker));
    }
}

/// The host, if a score or the user asked for a plugin before.
pub fn started() -> Option<&'static Host> {
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

/// Unloads each plugin with no name in `keep` and no running copy: see
/// [`Host::unload_unused`]. Does nothing before the host starts, and waits
/// for nothing.
pub fn unload_unused(keep: &[String]) {
    if let Some(host) = started() {
        host.unload_unused(keep);
    }
}

/// How long the end of the process waits for the plugin thread.
const EXIT_LIMIT: std::time::Duration = std::time::Duration::from_secs(10);

/// Unloads each plugin and waits for the plugin thread, [`EXIT_LIMIT`] at
/// most. A program calls this as its last step, after its outputs ended. A
/// plugin library that ends with the process while the plugin thread still
/// works in the library stops the process with a fault.
pub fn shutdown() {
    if let Some(host) = started() {
        host.shutdown(EXIT_LIMIT);
    }
}

/// True while the host reads the plugin folders. The plugin list is not
/// complete before the end of the read.
pub fn scanning() -> bool {
    started().is_some_and(Host::scanning)
}

/// The numbers the audio engine carries for a `.vst()` request.
///
/// The host loads the plugin on first use and does not wait: `Ok(None)`
/// means the plugin loads now.
pub(crate) fn controls(
    request: &rustel_voice::PluginRequest<'_>,
) -> Result<Option<InsertControls>, String> {
    let (plugin, loaded) = match host().resolve(request.name, false) {
        Resolved::Ready(plugin, loaded) => (plugin, loaded),
        Resolved::Pending => return Ok(None),
        Resolved::Missing => {
            return Err(format!("no plugin has the name '{}'", request.name));
        }
        Resolved::Failed(reason) => {
            return Err(format!("{} did not load: {reason}", request.name));
        }
    };
    let name = loaded.name();
    match (request.instrument, loaded.is_instrument()) {
        (true, false) => return Err(format!("{name} is an effect: use .vst()")),
        (false, true) => return Err(format!("{name} is an instrument: use .vsti()")),
        _ => {}
    }
    let preset = match request.preset {
        None => 0,
        Some(preset) => match host().preset(plugin, preset) {
            rustel_vst3::FoundPreset::Number(number) => number,
            // The host reads the preset folder now.
            rustel_vst3::FoundPreset::Pending => return Ok(None),
            rustel_vst3::FoundPreset::Missing => {
                return Err(format!("{name} has no preset with the name '{preset}'"));
            }
        },
    };
    let mut controls = InsertControls::new(InsertKey { plugin, preset });
    for (param, value) in request.params {
        let id = loaded
            .param(param)
            .ok_or_else(|| format!("{name} has no parameter with the name '{param}'"))?;
        let value = *value as f32;
        if !controls.push(InsertParam { id, value }) {
            return Err(format!(
                "a note carries {MAX_INSERT_PARAMS} values for a plugin at most"
            ));
        }
    }
    Ok(Some(controls))
}

pub(crate) const INSERT_ORBITS: [u8; rustel_audio::MAX_ORBITS] =
    [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15];

/// Keep a unique chain on its physical bus when only its literal orbit changes.
/// Nested calls, dynamic routes and shared slots do not prove sole ownership.
pub(crate) fn plan_orbits(
    previous: Option<&str>,
    source: &str,
    mut orbits: [u8; rustel_audio::MAX_ORBITS],
) -> [u8; rustel_audio::MAX_ORBITS] {
    let Some(previous) = previous.filter(|previous| *previous != source) else {
        return orbits;
    };
    let Some((old_text, old_routes)) = orbit_source(previous) else {
        return orbits;
    };
    let Some((new_text, new_routes)) = orbit_source(source) else {
        return orbits;
    };
    if old_text != new_text || old_routes.len() != new_routes.len() {
        return orbits;
    }
    let mut changed = old_routes
        .into_iter()
        .zip(new_routes)
        .filter(|(old, new)| old != new);
    let Some((from, to)) = changed.next() else {
        return orbits;
    };
    if changed.next().is_some() {
        return orbits;
    }
    let old = crate::lint::live_plugins(previous);
    let new = crate::lint::live_plugins(source);
    let chain = |plugins: &[crate::lint::NamedPlugin], orbit| {
        let mut chain: Vec<_> = plugins
            .iter()
            .filter(|plugin| plugin.orbit == Some(orbit))
            .map(|plugin| {
                (
                    plugin.instrument,
                    plugin.stage,
                    plugin.name.clone(),
                    plugin.preset.clone(),
                )
            })
            .collect();
        chain.sort();
        chain
    };
    let moving = chain(&old, from);
    if moving.is_empty()
        || moving != chain(&new, to)
        || !chain(&old, to).is_empty()
        || !chain(&new, from).is_empty()
        || (0..rustel_audio::MAX_ORBITS).any(|orbit| orbit != from && chain(&old, orbit) == moving)
    {
        return orbits;
    }
    orbits.swap(from, to);
    orbits
}

fn orbit_source(source: &str) -> Option<(String, Vec<usize>)> {
    let code = crate::lint::code_only(source);
    let bytes = code.as_bytes();
    // Helpers, bindings and indirect reads sometimes hide another owner's chain.
    if bytes.contains(&b'=') || source.contains('`') || !code.is_ascii() {
        return None;
    }
    let mut at = 0;
    while at < bytes.len() {
        if let Some(name) = crate::lint::scan::identifier_at(&code, at) {
            let word = &code[name.clone()];
            let after = crate::lint::scan::space_after(&code, name.end);
            let key = bytes.get(after) == Some(&b':');
            if !key
                && (matches!(word, "eval" | "Function" | "globalThis" | "constructor")
                    || (!crate::lint::known_names().contains(word)
                        && !matches!(word, "true" | "false" | "null")))
            {
                return None;
            }
            at = name.end;
        } else if bytes[at].is_ascii_digit() {
            while bytes
                .get(at)
                .is_some_and(|byte| byte.is_ascii_alphanumeric() || *byte == b'.')
            {
                at += 1;
            }
        } else {
            if bytes[at] == b'[' {
                let before = crate::lint::scan::space_before(&code, at);
                if before > 0
                    && (crate::lint::scan::is_name_byte(bytes[before - 1])
                        || matches!(bytes[before - 1], b')' | b']'))
                {
                    return None;
                }
            }
            at += 1;
        }
    }
    let plugins = crate::lint::live_plugins(source);
    let mut slots = std::collections::BTreeSet::new();
    for plugin in &plugins {
        let orbit = plugin
            .orbit
            .filter(|orbit| *orbit < rustel_audio::MAX_ORBITS)?;
        if (!plugin.instrument && plugin.stage >= rustel_audio::EFFECT_CHAIN)
            || !slots.insert((orbit, plugin.instrument, plugin.stage))
        {
            return None;
        }
    }
    let mut text = String::new();
    let mut routes = Vec::new();
    let (mut depth, mut calls, mut copied, mut at) = (0usize, 0usize, 0usize, 0usize);
    while at < bytes.len() {
        if bytes[at..].starts_with(b".vst(") || bytes[at..].starts_with(b".vsti(") {
            if depth != 0 {
                return None;
            }
            calls += 1;
        }
        if bytes[at..].starts_with(b".orbit") {
            if !bytes[at..].starts_with(b".orbit(") {
                return None;
            }
            if depth != 0 {
                return None;
            }
            let mut start = at + ".orbit(".len();
            while bytes.get(start).is_some_and(u8::is_ascii_whitespace) {
                start += 1;
            }
            let mut end = start;
            while bytes.get(end).is_some_and(u8::is_ascii_digit) {
                end += 1;
            }
            let orbit = source[start..end].parse::<usize>().ok()?;
            if orbit >= rustel_audio::MAX_ORBITS || !code[end..].trim_start().starts_with(')') {
                return None;
            }
            text.push_str(&source[copied..start]);
            text.push('#');
            copied = end;
            routes.push(orbit);
        }
        match bytes[at] {
            b'(' | b'[' | b'{' => depth += 1,
            b')' | b']' | b'}' => depth = depth.checked_sub(1)?,
            _ => {}
        }
        at += 1;
    }
    if depth != 0 || calls != plugins.len() {
        return None;
    }
    text.push_str(&source[copied..]);
    Some((text, routes))
}

/// One plugin request of a note: the kind, the slot, the plugin name and
/// the preset name.
struct Request<'a> {
    instrument: bool,
    slot: usize,
    name: &'a str,
    preset: Option<&'a str>,
}

/// The plugin requests of the haps, each one time.
fn requests<'a>(
    haps: &'a [crate::HapJson],
    orbits: &[u8; rustel_audio::MAX_ORBITS],
) -> Vec<Request<'a>> {
    let mut asked: Vec<Request<'_>> = Vec::new();
    for hap in haps {
        let crate::ValueJson::Raw(serde_json::Value::Object(object)) = &hap.value else {
            continue;
        };
        let orbit = object
            .get("orbit")
            .and_then(serde_json::Value::as_f64)
            .unwrap_or(1.0);
        if !(0.0..rustel_audio::MAX_ORBITS as f64).contains(&orbit) {
            continue;
        }
        let orbit = orbits[orbit as usize] as usize;
        let instrument = object.get("vsti").map(|plugin| {
            let slot = rustel_audio::instrument_slot(orbit);
            (true, slot, plugin)
        });
        // The effects are a list, in chain order.
        let effects = object.get("vst").and_then(serde_json::Value::as_array);
        let effects = effects
            .into_iter()
            .flatten()
            .enumerate()
            .map(|(stage, plugin)| {
                let slot = rustel_audio::effect_slot(orbit, stage);
                (false, slot, plugin)
            });
        for (instrument, slot, plugin) in effects.take(rustel_audio::EFFECT_CHAIN).chain(instrument)
        {
            let Some(name) = plugin.get("name").and_then(serde_json::Value::as_str) else {
                continue;
            };
            let preset = plugin.get("preset").and_then(serde_json::Value::as_str);
            let known = asked.iter().any(|request| {
                (request.slot, request.name, request.preset) == (slot, name, preset)
            });
            if !known {
                asked.push(Request {
                    instrument,
                    slot,
                    name,
                    preset,
                });
            }
        }
    }
    asked
}

/// Loads the plugins the haps name and prepares each one for its orbit,
/// with no wait, so a plugin is ready before its first note plays.
pub(crate) fn prepare(
    haps: &[crate::HapJson],
    sample_rate: u32,
    orbits: &[u8; rustel_audio::MAX_ORBITS],
) {
    for request in requests(haps, orbits) {
        host().prepare(
            request.name,
            request.preset,
            request.instrument,
            sample_rate,
            request.slot,
        );
    }
}

/// The plugin requests of the haps that are not ready for their notes: a
/// plugin that loads, or a copy for an orbit that is not built. `holds`
/// says if the output has the plugin on the slot already. A request the
/// host cannot serve does not count: the note reports the reason. The call
/// starts the work for each request it counts, and waits for nothing.
pub(crate) fn pending(
    haps: &[crate::HapJson],
    sample_rate: u32,
    orbits: &[u8; rustel_audio::MAX_ORBITS],
    holds: impl Fn(usize, InsertKey) -> bool,
) -> usize {
    let host = host();
    let mut pending = 0;
    for request in requests(haps, orbits) {
        let Request {
            instrument,
            slot,
            name,
            preset,
        } = request;
        match host.prepared(name, preset, instrument, sample_rate, slot) {
            rustel_vst3::Prepared::Ready(_) | rustel_vst3::Prepared::Unavailable => {}
            rustel_vst3::Prepared::Unbuilt(key) if holds(slot, key) => {}
            rustel_vst3::Prepared::Unbuilt(_) => {
                host.prepare(name, preset, instrument, sample_rate, slot);
                pending += 1;
            }
            rustel_vst3::Prepared::Pending => pending += 1,
        }
    }
    pending
}

/// Starts the host and the read of each plugin bundle with no plugin names
/// in the scan cache, as a DAW scans its plugins. The work runs on threads
/// of the host and in processes of their own, so the caller goes on at
/// once. The file `scan.json` in [`presets_folder`] keeps the result: a
/// bundle with the same files is not read again.
pub fn scan_in_background() {
    host().scan_plugins();
}

/// The bundles the scan read and the bundles of the scan in all, while
/// the scan runs: see [`scan_in_background`].
pub fn scan_progress() -> Option<(usize, usize)> {
    started().and_then(Host::scan_progress)
}

/// The plugins in their load now, one name for each bundle. Empty before
/// the host starts.
pub fn loads() -> Vec<String> {
    started().map(Host::loads).unwrap_or_default()
}

/// True while a plugin call of a score text is not ready for its notes: the
/// plugin loads or its copy for the orbit is not built. The call starts
/// the work and waits for nothing. With no orbit in the text, the load
/// alone counts. A call the host has no plugin for reads false: the note
/// reports the reason.
///
/// This is for a score not yet in play, such as an edit a host holds until
/// its plugins are ready. Prepared copies stay in the host until the
/// score's notes need them: a readiness check must not replace the
/// plugin used by the score still playing. [`crate::Session::plugins_pending`]
/// reads the score in play.
pub fn loading(plugin: &crate::lint::NamedPlugin, sample_rate: u32) -> bool {
    let host = host();
    let (name, preset) = (plugin.name.as_str(), plugin.preset.as_deref());
    let Some(orbit) = plugin
        .orbit
        .filter(|orbit| *orbit < rustel_audio::MAX_ORBITS)
    else {
        return matches!(host.resolve(name, false), Resolved::Pending);
    };
    let slot = if plugin.instrument {
        rustel_audio::instrument_slot(orbit)
    } else if plugin.stage < rustel_audio::EFFECT_CHAIN {
        rustel_audio::effect_slot(orbit, plugin.stage)
    } else {
        // The note does not go through this effect.
        return false;
    };
    match host.prepared(name, preset, plugin.instrument, sample_rate, slot) {
        rustel_vst3::Prepared::Ready(_) | rustel_vst3::Prepared::Unavailable => false,
        rustel_vst3::Prepared::Unbuilt(_) => {
            host.prepare(name, preset, plugin.instrument, sample_rate, slot);
            true
        }
        rustel_vst3::Prepared::Pending => true,
    }
}

/// The memory of the plugin processes of the host, in bytes, by the process
/// number of each worker: see [`PluginInfo::process`]. The figure of a
/// worker has each process of its process group, so a process the plugin
/// started counts too, such as the bridge of a Windows plugin on Linux.
/// The read is a read of `/proc`, and gives no entry on a different system.
pub fn process_memory() -> Vec<(u32, u64)> {
    let mut workers: Vec<(u32, u64)> = started()
        .map(Host::plugins)
        .unwrap_or_default()
        .iter()
        .filter_map(|plugin| Some((plugin.process?, 0)))
        .collect();
    workers.sort_unstable();
    workers.dedup();
    if !cfg!(target_os = "linux") || workers.is_empty() {
        return Vec::new();
    }
    for entry in std::fs::read_dir("/proc").into_iter().flatten().flatten() {
        let folder = entry.path();
        // The process group is the third number after the name, and the
        // name is in brackets with any text in it.
        let group = std::fs::read_to_string(folder.join("stat"))
            .ok()
            .and_then(|stat| {
                let after = stat.rsplit_once(')')?.1;
                after.split_whitespace().nth(2)?.parse::<u32>().ok()
            });
        let Some(worker) = workers.iter_mut().find(|(id, _)| Some(*id) == group) else {
            continue;
        };
        let resident = std::fs::read_to_string(folder.join("status"))
            .ok()
            .and_then(|status| {
                let line = status.lines().find(|line| line.starts_with("VmRSS:"))?;
                line.split_whitespace().nth(1)?.parse::<u64>().ok()
            });
        worker.1 += resident.unwrap_or(0) * 1024;
    }
    workers
}

/// The problems of the plugin thread since the last call.
pub(crate) fn take_errors() -> Vec<String> {
    started().map(Host::take_errors).unwrap_or_default()
}

/// The problems of the plugin host at the end of a render. The plugin
/// thread learns of the end of a plugin process a moment after the audio,
/// so this call looks at each plugin process and waits for the plugin
/// thread, 2 seconds at most.
pub(crate) fn settle() -> Vec<String> {
    let Some(host) = started() else {
        return Vec::new();
    };
    host.check_workers();
    host.wait_idle_within(std::time::Duration::from_secs(2), || false);
    host.take_errors()
}

/// Gives a live output the plugins of the process. A note plays with no
/// effect until its plugin is ready.
#[cfg(feature = "device-audio")]
pub fn attach(device: &rustel_audio::LiveScalarDevice) {
    device.set_insert_provider(std::sync::Arc::new(|key, sample_rate, orbit| {
        started()?.insert(key, sample_rate, orbit, false)
    }));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_orbit_edit_keeps_a_unique_chain_on_its_bus() {
        let source = r#"setcpm(90/4)
$: note("c3").vsti("Synth").vst("Delay")
    .vst("Reverb", {mix: 0.3}).orbit(1)
$: s("sine").gain(0.1)"#;
        let mut previous = source.to_owned();
        let mut orbits = INSERT_ORBITS;
        for output in [2, 7, 1, 2, 1] {
            let incoming = source.replace(".orbit(1)", &format!(".orbit({output})"));
            orbits = plan_orbits(Some(&previous), &incoming, orbits);
            assert_eq!(orbits[output], 1);
            let mut sorted = orbits;
            sorted.sort();
            assert_eq!(sorted, INSERT_ORBITS);
            previous = incoming;
        }
        assert_eq!(orbits[1], 1);
        let incoming = source.replace(".orbit(1)", ".orbit(2)");
        let moved = plan_orbits(Some(source), &incoming, orbits);
        assert_eq!(moved[2], 1);
        assert_eq!(
            plan_orbits(Some(&incoming), &incoming.replace("0.3", "0.4"), moved),
            moved
        );
    }

    #[test]
    fn ambiguous_or_changed_chains_keep_their_existing_buses() {
        for source in [
            r#"$: s("sine").vst("Delay").orbit(1)
$: s("sawtooth").vst("Other")"#,
            r#"$: s("sine").vst("Delay").orbit(1)
$: s("sawtooth").vst("Delay").orbit(3)"#,
            r#"$: s("sine").vst("Delay").orbit(1)
$: s("sawtooth").vst("Other").orbit(2)"#,
            r#"stack(s("sine").vst("Delay").orbit(1))"#,
            r#"let line = s("sine").vst("Delay").orbit(1); line"#,
            r#"s("sine").vst("Delay").orbit(1).orbit("<1 2>")"#,
            r#"s("sine").vst("Delay").orbit(1).orbit (3)"#,
            r#"s("sine").vst(name).orbit(1)"#,
            r#"$: s("sine").vst("Delay").orbit(1)
$: helper()"#,
            r#"$: s("sine").vst("Delay").orbit(1)
$: savedPattern"#,
            r#"s("sine").vst("Delay").orbit(1); eval("helper()")"#,
            r#"s("sine").vst("Delay").orbit(1); s["helper"]()"#,
            r#"s(`${helper()}`).vst("Delay").orbit(1)"#,
        ] {
            assert_eq!(
                plan_orbits(
                    Some(source),
                    &source.replace(".orbit(1)", ".orbit(2)"),
                    INSERT_ORBITS
                ),
                INSERT_ORBITS,
                "{source}"
            );
        }
        let source = r#"s("sine").vst("Delay").orbit(1)"#;
        for incoming in [
            r#"s("sine").vst("Other").orbit(2)"#,
            r#"s("sine").vst("Delay").orbit("<1 2>")"#,
        ] {
            assert_eq!(
                plan_orbits(Some(source), incoming, INSERT_ORBITS),
                INSERT_ORBITS
            );
        }
    }

    #[test]
    fn a_score_text_gives_the_plugin_names_of_its_calls() {
        let source = r#"
            $: note("c2").vsti("Serum 2", { macro1: 0.5 }).vst( 'ott' )
            $: s("bd").vst(`valhalla supermassive`).myvst("no").vst(name)
            // .vst("ott") again, and .vsti("") with no name
        "#;
        assert_eq!(
            names_in_source(source),
            ["ott", "valhalla supermassive", "Serum 2"]
        );
    }
}
