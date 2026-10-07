use super::*;

/// The packs `samples cache` works on, resolved from the names a user gave.
///
/// Matching is case-insensitive on any substring, the way `midi-list` ports
/// and device names match: the pin carries CDN directory names a person
/// should not have to type in full. An unknown name says so and lists what
/// there was to pick from rather than quietly caching nothing.
fn resolve_packs(
    requested: &[String],
) -> Result<Vec<rustel_runtime::samples::DefaultSource>, String> {
    let all = rustel_runtime::samples::SampleLibrary::default_sources();
    if requested.is_empty() {
        return Ok(all.to_vec());
    }
    let mut found = Vec::new();
    for request in requested {
        let needle = request.to_lowercase();
        let matches: Vec<&rustel_runtime::samples::DefaultSource> = all
            .iter()
            .filter(|pack| pack.name.to_lowercase().contains(&needle))
            .collect();
        match matches.len() {
            0 => {
                let known = all
                    .iter()
                    .map(|pack| pack.name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ");
                return Err(format!(
                    "no pack matches {request:?}: the shipped packs are {known}"
                ));
            }
            1 => found.push(matches[0].clone()),
            _ => {
                // The first match wins, as a substring search on a shorter
                // name does elsewhere; say the choice rather than leave it
                // to be discovered from the output.
                let names = matches
                    .iter()
                    .map(|pack| pack.name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ");
                let selected = &matches[0].name;
                notice(
                    serde_json::json!({ "samples_cache_selection": {
                        "request": request, "matches": names, "selected": selected,
                    } }),
                    || format!("{request:?} matches several packs ({names}); caching {selected}"),
                );
                found.push(matches[0].clone());
            }
        }
    }
    Ok(found)
}

/// `rustel samples cache`: fetch the shipped sample packs onto disk.
///
/// One pass of the studio's own pack caching, walked from a terminal: the
/// library queues every file the pack names that is not on disk already,
/// behind whatever a playing score asks for, and this command draws one
/// progress bar for all the packs while the loaders work. Ctrl-C stops
/// the waiting; the files that landed stay, and the next run fetches only
/// what is missing.
pub(super) fn run_samples_cache(
    requested: &[String],
    list: bool,
    json: bool,
) -> Result<(), RuntimeError> {
    let packs = resolve_packs(requested).map_err(RuntimeError::Message)?;
    // A session brings the default library up; this borrows the same
    // constructors so the cache it fills is the cache a session reads.
    let mut session = Session::new().map_err(|error| {
        RuntimeError::Message(format!("the sample library could not start: {error}"))
    })?;
    session.set_direct_diagnostic_logging(false);
    session.enable_default_samples().map_err(|error| {
        RuntimeError::Message(format!("the sample library could not start: {error}"))
    })?;
    let library = session
        .sample_library()
        .cloned()
        .expect("enable_default_samples put a library in place");

    // The pack lists are pinned manifests that may still be on their way;
    // the file counts they bring are the ones the bars are drawn against.
    library.wait_until_idle(std::time::Duration::from_secs(30));

    let on = style::stdout_on();
    let rows: Vec<PackRow> = packs
        .iter()
        .map(|pack| {
            let (sounds, files) = library.default_source_holds(pack);
            let (cached, total, bytes) = library.default_source_cached(pack);
            PackRow {
                name: pack.name.clone(),
                sounds,
                files: files.max(total),
                cached,
                bytes,
            }
        })
        .collect();
    if list {
        if json {
            println!(
                "{}",
                serde_json::json!({
                    "sample_cache": sample_cache_dir().display().to_string(),
                    "packs": rows.iter().map(PackRow::json).collect::<Vec<_>>(),
                })
            );
        } else {
            println!("{}", style::bold(on, "Shipped sample packs"));
            println!(
                "{}",
                style::dim(on, &format!("cache: {}", sample_cache_dir().display()))
            );
            for row in &rows {
                println!(
                    "  {:<26} {:>4} sounds {:>5}/{:<5} files {:>8}",
                    row.name,
                    row.sounds,
                    row.cached,
                    row.files,
                    style::cyan(on, &bytes_label(row.bytes)),
                );
            }
            println!(
                "{}",
                style::dim(on, "`rustel samples cache [PACK…]` fetches what is missing")
            );
        }
        return Ok(());
    }

    // Ask each pack for what it is missing before drawing anything: the
    // walk of a pack's files is a directory-sized queue and belongs on the
    // loader, exactly as the studio's own `c` key does it.
    let mut queued_total = 0usize;
    let mut missing_total = 0usize;
    for pack in &packs {
        let request = library.cache_default_source(pack);
        missing_total += request.files;
        queued_total += request.queued;
    }
    let bases: Vec<String> = packs.iter().map(|pack| pack.base.clone()).collect();
    let pack_names: Vec<String> = packs.iter().map(|pack| pack.name.clone()).collect();
    let caches_pending = |library: &rustel_runtime::samples::SampleLibrary| -> usize {
        bases
            .iter()
            .map(|base| library.pending_cache_under(base))
            .sum::<usize>()
            .min(missing_total)
    };
    let file_in_hand = |library: &rustel_runtime::samples::SampleLibrary| -> Option<String> {
        bases.iter().find_map(|base| library.loading_under(base))
    };

    if json {
        // One line per change, on stderr, then the summary on stdout: the
        // same split as the live event stream, so a script reading stdout
        // gets the report and nothing else.
        let started = std::time::Instant::now();
        let mut last = 0usize;
        let mut failures: Vec<String> = Vec::new();
        while wait_for_cache_to_settle(&library, &bases, started) {
            let left = caches_pending(&library);
            let loading = file_in_hand(&library);
            let done = missing_total.saturating_sub(left);
            if done != last {
                let mut event = serde_json::json!({
                    "samples_cache": {
                        "done": done,
                        "total": missing_total,
                        "left": left,
                    }
                });
                if let Some(file) = loading {
                    event["samples_cache"]["file"] = serde_json::json!(file);
                }
                if !quiet_asked() {
                    eprintln!("{event}");
                }
                last = done;
            }
            failures.extend(library.take_failures());
        }
        failures.extend(library.take_failures());
        let summary = serde_json::json!({
            "samples_cache": {
                "status": "done",
                "packs": pack_names,
                "fetched": queued_total,
                "already_cached": missing_total.saturating_sub(queued_total),
                "failures": failures,
                "cache": sample_cache_dir().display().to_string(),
            }
        });
        println!("{summary}");
        if !failures.is_empty() {
            return Err(RuntimeError::Message(format!(
                "{} file(s) failed to fetch; see the failures in the report",
                failures.len()
            )));
        }
        return Ok(());
    }

    if queued_total == 0 {
        println!(
            "{} {}",
            style::green(on, "✓"),
            style::bold(
                on,
                &format!(
                    "all {missing_total} file(s) of {} pack(s) are on disk",
                    pack_names.len()
                )
            )
        );
        return Ok(());
    }

    // The bar. One line, redrawn in place: a pack's left count climbs down
    // while the loaders fetch, the file in a loader's hand is named, and the
    // line is left on "done" rather than wiped, because a terminal scrollback
    // that ends on the outcome is the one a person reads.
    let progress = style::stderr_on() && !quiet_asked();
    let draw = |done: usize, file: Option<&str>| {
        if !progress {
            return;
        }
        let on = style::stderr_on();
        let width = 28;
        let fraction = if missing_total == 0 {
            1.0
        } else {
            (done as f64 / missing_total as f64).clamp(0.0, 1.0)
        };
        let filled = (fraction * width as f64).round() as usize;
        let bar = format!("{}{}", "━".repeat(filled), "─".repeat(width - filled));
        let file = file.unwrap_or("");
        // Only a capable stderr terminal receives redraw controls.
        let stderr = std::io::stderr();
        let mut handle = stderr.lock();
        let _ = handle.write_all(
            format!(
                "\r\x1b[K{} {} {:>4}/{:<4} {file}",
                style::cyan(on, &bar),
                style::bold(on, &format!("{:>3}%", (fraction * 100.0) as usize)),
                done,
                missing_total,
            )
            .as_bytes(),
        );
        let _ = handle.flush();
    };

    let started = std::time::Instant::now();
    let mut last_done = 0usize;
    let mut last_file = String::new();
    let mut failures: Vec<String> = Vec::new();
    draw(0, None);
    while wait_for_cache_to_settle(&library, &bases, started) {
        let left = caches_pending(&library);
        let done = missing_total.saturating_sub(left);
        let file = file_in_hand(&library);
        if done != last_done || file.as_deref() != Some(last_file.as_str()) {
            draw(done, file.as_deref());
            last_done = done;
            last_file = file.unwrap_or_default();
        }
        failures.extend(library.take_failures());
    }
    failures.extend(library.take_failures());
    let seconds = started.elapsed().as_secs();
    if progress {
        eprintln!();
    }
    println!(
        "{} {} in {seconds}s - {} pack(s), {missing_total} file(s) total",
        style::green(on, "✓"),
        style::bold(on, &format!("cached {queued_total} file(s)")),
        pack_names.len(),
    );
    if !failures.is_empty() {
        for failure in &failures {
            notice(
                serde_json::json!({ "samples_cache_failure": { "message": failure } }),
                || format!("sample fetch failed: {failure}"),
            );
        }
        return Err(RuntimeError::Message(format!(
            "{} of {} file(s) failed to fetch; run `rustel samples cache` again to retry",
            failures.len(),
            missing_total
        )));
    }
    Ok(())
}

/// Wait for one poll interval while any of the asked-for packs still has
/// work in the line. `false` when nothing is left, the deadline has run,
/// or a signal arrived - the loop above then reports. The deadline bounds a
/// fetch whose connection hangs despite the loader's own budget, so a wait
/// can always end.
fn wait_for_cache_to_settle(
    library: &rustel_runtime::samples::SampleLibrary,
    bases: &[String],
    started: std::time::Instant,
) -> bool {
    const POLL: std::time::Duration = std::time::Duration::from_millis(100);
    const DEADLINE: std::time::Duration = std::time::Duration::from_secs(60 * 60);
    if started.elapsed() >= DEADLINE {
        return false;
    }
    // The library's own idle test answers for the whole queue, including
    // font files; the pending counts are what draws the bar.
    library.wait_until_idle(POLL);
    !interrupted_by().is_some()
        && bases
            .iter()
            .map(|base| library.pending_cache_under(base))
            .sum::<usize>()
            > 0
}

/// A byte count as a person reads it, for the cache rows.
fn bytes_label(bytes: u64) -> String {
    const KIB: f64 = 1024.0;
    const MIB: f64 = KIB * 1024.0;
    const GIB: f64 = MIB * 1024.0;
    let mib = bytes as f64 / MIB;
    if bytes as f64 >= GIB {
        format!("{:.1} GiB", bytes as f64 / GIB)
    } else if mib >= 1.0 {
        if mib >= 10.0 {
            format!("{mib:.0} MiB")
        } else {
            format!("{mib:.1} MiB")
        }
    } else if bytes as f64 >= KIB {
        format!("{:.0} KiB", bytes as f64 / KIB)
    } else {
        format!("{bytes} B")
    }
}

/// `rustel samples clear`: empty every downloaded sample file and say how
/// much that was.
pub(super) fn run_samples_clear(json: bool, options: &ClearOptions) -> Result<(), RuntimeError> {
    let dir = sample_cache_dir();
    if options.dry_run {
        let scope =
            "digest-named downloaded files and the score-selected namespace; other files are kept";
        if json {
            println!(
                "{}",
                serde_json::json!({ "sample_cache": {
                "status": "dry_run", "path": dir.display().to_string(), "scope": scope,
            } })
            );
        } else {
            println!("Would clear {}: {scope}", dir.display());
        }
        return Ok(());
    }
    confirm_cache_clear(&dir, options)?;
    // Measure the removed bytes from the files that existed before the clear
    // and are gone after it. A difference of totals would be wrong: the clear
    // writes the score namespace's no-legacy marker, and it keeps files that
    // are not cache entries.
    let before = directory_sizes(&dir);
    rustel_runtime::samples::clear_sample_cache().map_err(RuntimeError::Message)?;
    let after = directory_sizes(&dir);
    let removed = before
        .iter()
        .filter(|(path, _)| !after.contains_key(*path))
        .map(|(_, size)| size)
        .sum();
    if json {
        println!(
            "{}",
            serde_json::json!({
                "sample_cache": {
                    "status": "cleared",
                    "path": dir.display().to_string(),
                    "bytes": removed,
                }
            })
        );
    } else {
        let on = style::stdout_on();
        println!(
            "{} {} ({})",
            style::green(on, "✓"),
            style::bold(on, "sample cache cleared"),
            bytes_label(removed),
        );
        println!(
            "{}",
            style::dim(
                on,
                &format!(
                    "  {} - fetch it back with `rustel samples cache`",
                    dir.display()
                )
            )
        );
    }
    Ok(())
}

/// A destructive operation needs explicit consent even when stdin is piped.
pub(super) fn confirm_cache_clear(
    path: &std::path::Path,
    options: &ClearOptions,
) -> Result<(), RuntimeError> {
    if options.force {
        return Ok(());
    }
    if NO_INPUT_MODE.load(std::sync::atomic::Ordering::Relaxed)
        || !std::io::stdin().is_terminal()
        || !std::io::stderr().is_terminal()
    {
        return Err(RuntimeError::Message(
            "cache deletion requires --force without an interactive terminal; use --dry-run to preview".into()
        ));
    }
    let prompt = format!(
        "Delete downloaded cache data at {}? Type yes to confirm:",
        path.display()
    );
    if json_asked() {
        eprintln!(
            "{}",
            serde_json::json!({ "confirmation": { "message": prompt } })
        );
    } else {
        eprintln!("{prompt}");
    }
    std::io::stderr()
        .flush()
        .map_err(|error| RuntimeError::Message(error.to_string()))?;
    // The CLI installs signal handlers, so a blocking stdin read on this
    // thread would swallow Ctrl-C until the user pressed Enter as well.
    let (send, receive) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut answer = String::new();
        let result = std::io::stdin().read_line(&mut answer).map(|_| answer);
        let _ = send.send(result);
    });
    let answer = loop {
        if interrupted_by().is_some() {
            return Err(RuntimeError::Message(
                "cache deletion interrupted; no files changed".into(),
            ));
        }
        match receive.recv_timeout(std::time::Duration::from_millis(50)) {
            Ok(result) => {
                break result.map_err(|error| RuntimeError::Message(error.to_string()))?;
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => continue,
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                return Err(RuntimeError::Message(
                    "cache confirmation input closed".into(),
                ));
            }
        }
    };
    if interrupted_by().is_some() {
        return Err(RuntimeError::Message(
            "cache deletion interrupted; no files changed".into(),
        ));
    }
    if answer.trim() == "yes" {
        Ok(())
    } else {
        Err(RuntimeError::Message(
            "cache deletion cancelled; no files changed".into(),
        ))
    }
}

/// Every regular file under `dir` by path with its size - the snapshot the
/// removed-bytes measurement diffs. Same shape as the runtime's own usage
/// walk, keyed per file so what vanishes can be named.
pub(super) fn directory_sizes(dir: &std::path::Path) -> std::collections::HashMap<String, u64> {
    const MAX_ENTRIES: usize = 200_000;
    let mut map = std::collections::HashMap::new();
    let mut folders = vec![dir.to_path_buf()];
    let mut seen = 0usize;
    while let Some(folder) = folders.pop() {
        let Ok(entries) = std::fs::read_dir(&folder) else {
            continue;
        };
        for entry in entries.flatten() {
            seen += 1;
            if seen > MAX_ENTRIES {
                return map;
            }
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            if kind.is_dir() {
                folders.push(entry.path());
            } else if kind.is_file()
                && let Ok(metadata) = entry.metadata()
                && let Some(path) = entry.path().to_str()
            {
                map.insert(path.to_owned(), metadata.len());
            }
            // A symlink is neither followed nor counted, as the cache's
            // own usage walk refuses to.
        }
    }
    map
}
