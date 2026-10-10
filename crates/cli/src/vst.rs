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
    use rustel_runtime::vst::{self, Resolved, Status};

    /// A plugin with more parameters than this shows its groups by name.
    const MANY_PARAMS: usize = 40;

    if let Some(bundle) = worker {
        // This process is the worker of one bundle: it serves its host
        // until the host says to end.
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
    let mut told = false;
    while let Some((_, bundles)) = host.scan_progress() {
        // Ctrl-C ends the wait: the end of the command ends the scan.
        if let Some(signal) = crate::interrupted_by() {
            return Err(RuntimeError::Interrupted(signal));
        }
        if !told && !json && !crate::quiet_asked() {
            eprintln!("scanning {bundles} plugin bundles");
        }
        told = true;
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    let on = style::stdout_on();
    let Some(name) = name else {
        let plugins = host.plugins();
        if json {
            let rows: Vec<_> = plugins
                .iter()
                .map(|plugin| serde_json::json!({ "name": plugin.name, "bundle": plugin.bundle }))
                .collect();
            println!(
                "{}",
                serde_json::json!({ "plugins": rows, "folders": vst::folders() })
            );
            return Ok(());
        }
        println!("{}", style::bold(on, "VST3 plugins"));
        for plugin in &plugins {
            let bundle = plugin.bundle.display().to_string();
            println!(
                "  {}  {}",
                style::cyan(on, &plugin.name),
                style::dim(on, &bundle)
            );
        }
        if plugins.is_empty() {
            println!("  {}", style::dim(on, "no plugin in these folders:"));
            for folder in vst::folders() {
                println!("    {}", style::dim(on, &folder.display().to_string()));
            }
        } else {
            let hint = "the parameters of one plugin: rustel vst <name>";
            println!("{}", style::dim(on, hint));
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
        println!(
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
    println!(
        "{}  {}",
        style::bold(on, plugin.name()),
        style::dim(on, &format!("{vendor}  {categories}"))
    );
    // A plugin with many parameters shows its top level and the names of
    // its groups. A filter shows the parameters with the word in their
    // group, name or key.
    let wanted = filter.map(vst::canonical);
    let shown: Vec<&vst::ParamInfo> = plugin
        .params()
        .iter()
        .filter(|param| match &wanted {
            Some(word) => [&param.group, &param.name, &param.key]
                .iter()
                .any(|text| vst::canonical(text).contains(word.as_str())),
            None => plugin.params().len() <= MANY_PARAMS || param.group.is_empty(),
        })
        .collect();
    let width = shown
        .iter()
        .map(|param| param.key.chars().count())
        .max()
        .unwrap_or(0);
    for param in &shown {
        let value = param.default_shown();
        println!(
            "  {:width$}  {}  {}",
            style::cyan(on, &param.key),
            param.name,
            style::dim(on, format!("{value}  {}", param.group).trim()),
            width = width + style::cyan(on, "").len(),
        );
    }
    if shown.len() < plugin.params().len() && wanted.is_none() {
        let mut groups: Vec<(&str, usize)> = Vec::new();
        for param in plugin
            .params()
            .iter()
            .filter(|param| !param.group.is_empty())
        {
            match groups.iter_mut().find(|(name, _)| *name == param.group) {
                Some((_, count)) => *count += 1,
                None => groups.push((&param.group, 1)),
            }
        }
        println!("{}", style::bold(on, "groups:"));
        for (name, count) in groups {
            println!("  {name}  {}", style::dim(on, &count.to_string()));
        }
        let hint = format!("one group: rustel vst \"{}\" <group>", plugin.name());
        println!("{}", style::dim(on, &hint));
    }
    if !presets.is_empty() {
        println!("{} {}", style::bold(on, "presets:"), presets.join(", "));
    }
    let first = plugin.params().first().map(|param| param.key.as_str());
    let example = match first {
        Some(key) => format!(".{call}(\"{}\", {{ {key}: 0.5 }})", plugin.name()),
        None => format!(".{call}(\"{}\")", plugin.name()),
    };
    println!("{}", style::dim(on, &format!("in a score: {example}")));
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
