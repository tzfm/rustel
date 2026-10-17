//! `rustel vst`: the plugins `.vst()` finds, and the parameters of one.

use rustel_runtime::RuntimeError;

use crate::style;

#[cfg(feature = "vst")]
pub(super) fn run_vst(
    name: Option<&str>,
    filter: Option<&str>,
    json: bool,
    rescan: bool,
    worker: Option<std::path::PathBuf>,
) -> Result<(), RuntimeError> {
    use rustel_runtime::outln;
    use rustel_runtime::vst::{self, Resolved, Status};

    /// A plugin with more parameters than this shows its groups by name.
    const MANY_PARAMS: usize = 40;

    if let Some(bundle) = worker {
        // This process is the scan process of one bundle: it writes the
        // plugins of the bundle for its host, and ends.
        std::process::exit(vst::serve(&bundle));
    }
    let host = vst::host();
    if rescan {
        vst::rescan();
    }
    // The list has each plugin by name after the scan read its bundle. A
    // bundle in the scan cache needs no new read.
    host.scan_plugins();
    // The first read of the plugin folders runs on the plugin thread.
    host.wait_idle();
    // A terminal gets one line, drawn again for each plugin, so a plugin
    // that stops the scan has its name on the screen. Other output gets
    // the count only.
    let tell = !json && !crate::quiet_asked();
    let redraw = tell && style::stderr_on();
    let mut told = None;
    while let Some(progress) = host.scan_progress() {
        // Ctrl-C ends the wait: the end of the command ends the scan.
        if let Some(signal) = crate::interrupted_by() {
            if told.is_some() && redraw {
                eprintln!();
            }
            return Err(RuntimeError::Interrupted(signal));
        }
        if redraw && told.as_ref() != Some(&progress) {
            // A short line does not wrap, so the next draw covers it.
            let plugin: String = progress.plugin.chars().take(48).collect();
            eprint!(
                "\r\x1b[Kscanning {} of {} bundles · {plugin}",
                progress.read, progress.bundles
            );
        } else if tell && told.is_none() {
            eprintln!("scanning {} plugin bundles", progress.bundles);
        }
        told = Some(progress);
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    if told.is_some() && redraw {
        eprint!("\r\x1b[K");
    }
    let on = style::stdout_on();
    let Some(name) = name else {
        let plugins = host.plugins();
        if json {
            // A path is not always UTF-8, and JSON has no other text.
            let text = |path: &std::path::Path| path.to_string_lossy().into_owned();
            let rows: Vec<_> = plugins
                .iter()
                .map(|plugin| {
                    serde_json::json!({ "name": plugin.name, "bundle": text(&plugin.bundle) })
                })
                .collect();
            let folders: Vec<_> = vst::folders().iter().map(|path| text(path)).collect();
            outln!(
                "{}",
                serde_json::json!({ "plugins": rows, "folders": folders })
            );
            return Ok(());
        }
        outln!("{}", style::bold(on, "VST3 plugins"));
        for plugin in &plugins {
            let bundle = plugin.bundle.display().to_string();
            match &plugin.status {
                // A bundle with a fault at its scan stays off. The reason
                // tells why the row has no parameters, cut to one line:
                // the full text is the error of `rustel vst <name>`.
                Status::Failed(reason) => {
                    let mut short: String = reason.chars().take(120).collect();
                    if short.len() < reason.len() {
                        short.push('…');
                    }
                    outln!(
                        "  {}  {}",
                        style::dim(on, &plugin.name),
                        style::dim(on, &format!("{bundle}  off: {short}"))
                    );
                }
                _ => outln!(
                    "  {}  {}",
                    style::cyan(on, &plugin.name),
                    style::dim(on, &bundle)
                ),
            }
        }
        if plugins.is_empty() {
            outln!("  {}", style::dim(on, "no plugin in these folders:"));
            for folder in vst::folders() {
                outln!("    {}", style::dim(on, &folder.display().to_string()));
            }
        } else {
            let hint = "the parameters of one plugin: rustel vst <name>";
            outln!("{}", style::dim(on, hint));
        }
        return Ok(());
    };

    let plugin = match host.resolve(name, true) {
        Resolved::Ready(_, plugin) => plugin,
        Resolved::Failed(reason) => {
            return Err(RuntimeError::Message(format!(
                "{name} did not load: {reason}"
            )));
        }
        Resolved::Missing | Resolved::Pending => {
            return Err(RuntimeError::Message(format!(
                "no plugin has the name '{name}'. `rustel vst` lists the plugins"
            )));
        }
    };
    let row = host
        .plugins()
        .into_iter()
        .find(|row| row.status == Status::Ready && row.name == plugin.name());
    let (vendor, categories) = row
        .map(|row| (row.vendor, row.categories))
        .unwrap_or_default();
    // The load read the preset folder. `None` is a read that runs now.
    let presets = host.presets(plugin.name()).unwrap_or_else(|| {
        host.wait_idle();
        host.presets(plugin.name()).unwrap_or_default()
    });
    if json {
        let params: Vec<_> = plugin
            .params()
            .iter()
            .map(|param| {
                serde_json::json!({
                    "key": param.key,
                    "name": param.name,
                    "group": param.group,
                    "id": param.id,
                    "default": param.default,
                    "default_text": param.default_text,
                    "units": param.units,
                    "steps": param.steps,
                })
            })
            .collect();
        outln!(
            "{}",
            serde_json::json!({
                "name": plugin.name(),
                "instrument": plugin.is_instrument(),
                "vendor": vendor,
                "categories": categories,
                "params": params,
                "presets": presets,
            })
        );
        return Ok(());
    }
    let call = if plugin.is_instrument() {
        "vsti"
    } else {
        "vst"
    };
    outln!(
        "{}  {}",
        style::bold(on, plugin.name()),
        style::dim(on, &format!("{vendor}  {categories}"))
    );
    // A plugin with many parameters shows its top level and the names of
    // its groups. A top level of more parameters than the count is cut at
    // the count, with a line that tells where the rest is. A filter shows
    // the parameters with the word in their group, name or key.
    let wanted = filter.map(vst::canonical);
    let params = plugin.params();
    let shown: Vec<&vst::ParamInfo> = match &wanted {
        Some(word) => params
            .iter()
            .filter(|param| {
                [&param.group, &param.name, &param.key]
                    .iter()
                    .any(|text| vst::canonical(text).contains(word.as_str()))
            })
            .collect(),
        None => params
            .iter()
            .filter(|param| params.len() <= MANY_PARAMS || param.group.is_empty())
            .take(MANY_PARAMS)
            .collect(),
    };
    let width = shown
        .iter()
        .map(|param| param.key.chars().count())
        .max()
        .unwrap_or(0);
    for param in &shown {
        let value = param.default_shown();
        outln!(
            "  {:width$}  {}  {}",
            style::cyan(on, &param.key),
            param.name,
            style::dim(on, format!("{value}  {}", param.group).trim()),
            width = width + style::cyan(on, "").len(),
        );
    }
    if shown.len() < params.len() && wanted.is_none() {
        let top = params.iter().filter(|param| param.group.is_empty()).count();
        if top > shown.len() {
            let more = top - shown.len();
            let said = format!("{more} more parameters with no group");
            outln!("  {}", style::dim(on, &said));
        }
        let mut groups: Vec<(&str, usize)> = Vec::new();
        for param in params.iter().filter(|param| !param.group.is_empty()) {
            match groups.iter_mut().find(|(name, _)| *name == param.group) {
                Some((_, count)) => *count += 1,
                None => groups.push((&param.group, 1)),
            }
        }
        if groups.is_empty() {
            let hint = format!(
                "a word in a name shows them: rustel vst \"{}\" <word>",
                plugin.name()
            );
            outln!("{}", style::dim(on, &hint));
        } else {
            outln!("{}", style::bold(on, "groups:"));
            for (name, count) in groups {
                outln!("  {name}  {}", style::dim(on, &count.to_string()));
            }
            let hint = format!("one group: rustel vst \"{}\" <group>", plugin.name());
            outln!("{}", style::dim(on, &hint));
        }
    }
    if !presets.is_empty() {
        outln!("{} {}", style::bold(on, "presets:"), presets.join(", "));
    }
    let first = plugin.params().first().map(|param| param.key.as_str());
    let example = match first {
        Some(key) => format!(".{call}(\"{}\", {{ {key}: 0.5 }})", plugin.name()),
        None => format!(".{call}(\"{}\")", plugin.name()),
    };
    outln!("{}", style::dim(on, &format!("in a score: {example}")));
    Ok(())
}

#[cfg(not(feature = "vst"))]
pub(super) fn run_vst(
    _name: Option<&str>,
    _filter: Option<&str>,
    _json: bool,
    _rescan: bool,
    _worker: Option<std::path::PathBuf>,
) -> Result<(), RuntimeError> {
    let _ = style::stdout_on;
    Err(RuntimeError::Message(
        "This command requires the 'vst' feature to be enabled at compile time.".into(),
    ))
}
