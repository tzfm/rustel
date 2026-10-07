//! The Settings Sources page and the sample library behind it: rows for shipped
//! packs and user imports, handing imported sources to the library, and
//! refreshing or downloading packs to disk (one pack, the sounds a score names,
//! or the whole library). Also covers measuring and clearing the sample cache,
//! the status and log lines for sources that finished loading or whose banks
//! another import hides, and the list of running background jobs that these
//! downloads feed.

use super::*;

/// How often the cache-size row remeasures while downloads are writing
/// files - a full walk is costly, but a static MiB while packing grows is
/// worse.
const CACHE_MEASURE_WHILE_DOWNLOADING: Duration = Duration::from_secs(2);

/// What to say when imports finish: one line for the footer, and one line
/// per source for the log.
///
/// They are not the same sentence. The footer is short, shortens paths to
/// the setting, and can afford to point at the log. The log is where the
/// pointing ends: it gets the whole path and the actual reason, because a
/// log line that says "see Log" is a line that says nothing to the only
/// person reading it. A failure's line carries its source's alert key.
pub(super) struct SettledSources {
    pub(super) status: Option<String>,
    pub(super) log: Vec<(LogLevel, String, Option<String>)>,
}

/// One pack the studio ships with, as its row on Sources reads.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct ShippedSource {
    pub(super) source: rustel_runtime::samples::DefaultSource,
    /// Sounds the library knows under it; zero until its list is in.
    pub(super) sounds: usize,
    /// Distinct files those sounds name.
    pub(super) files: usize,
    /// Files of those already on disk in the host cache.
    pub(super) cached_files: usize,
    /// Bytes those cached files take on disk.
    pub(super) cache_bytes: u64,
    /// Every file url this pack has named across refreshes - so a row can
    /// show more on disk than the current manifest lists.
    pub(super) seen_files: HashSet<Arc<str>>,
    /// Its files on their way onto disk, once asked for. Cleared when
    /// done, so the row reads its cached count instead.
    pub(super) cache: Option<super::super::settings::SourceCacheProgress>,
}

impl ShippedSource {
    /// The state column at rest: what the pack holds, or that its list is
    /// not in yet.
    fn state(&self, library_loading: bool) -> String {
        if self.sounds == 0 && self.files == 0 && self.cached_files == 0 {
            if library_loading {
                "fetching its list…".to_owned()
            } else {
                "-".to_owned()
            }
        } else {
            super::super::settings::source_cache_state(
                self.sounds,
                self.cached_files,
                self.files,
                self.cache_bytes,
            )
        }
    }

    fn row(&self, library_loading: bool) -> super::super::settings::SourceRow {
        super::super::settings::SourceRow {
            spec: self.source.url.clone(),
            label: self.source.name.clone(),
            state: self.state(library_loading),
            dimmed: self.sounds == 0,
            shipped: true,
            local: false,
            cache_fill: (self.files > 0 || self.cached_files > 0)
                .then_some((self.cached_files, self.files)),
            cache: self.cache.clone(),
        }
    }
}

/// A user import as the Sources page follows it: local folders skip the
/// cache counts; remote packs show the same fraction a shipped pack does.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct UserSource {
    pub(super) spec: String,
    pub(super) local: bool,
    pub(super) sounds: usize,
    files: usize,
    cached_files: usize,
    cache_bytes: u64,
    seen_files: HashSet<Arc<str>>,
    pub(super) cache: Option<super::super::settings::SourceCacheProgress>,
}

/// What to say when imports that were still being read have landed.
///
/// `None` while nothing settled this poll. One source names itself; more
/// than one is counted, because a studio starting with a shelf of them
/// should not print a paragraph.
pub(super) fn settled_sources(
    before: &[rustel_runtime::samples::GlobalSourceReport],
    now: &[rustel_runtime::samples::GlobalSourceReport],
    full_paths: bool,
    quiet: Option<&str>,
) -> Option<SettledSources> {
    use rustel_runtime::samples::GlobalSourceState;
    let was_loading = |spec: &str| {
        before
            .iter()
            .any(|report| report.spec == spec && report.state == GlobalSourceState::Loading)
    };
    let settled: Vec<&rustel_runtime::samples::GlobalSourceReport> = now
        .iter()
        .filter(|report| report.state != GlobalSourceState::Loading && was_loading(&report.spec))
        .collect();
    let (mut sounds, mut failed) = (0usize, 0usize);
    let count = |settled: &[&rustel_runtime::samples::GlobalSourceReport],
                 sounds: &mut usize,
                 failed: &mut usize| {
        for report in settled {
            match &report.state {
                GlobalSourceState::Ready { banks } => *sounds += banks,
                GlobalSourceState::Missing(_) | GlobalSourceState::Failed(_) => *failed += 1,
                GlobalSourceState::Loading | GlobalSourceState::Off => {}
            }
        }
    };
    // Every source that settled says what happened to it, by its whole
    // name, in the log. A batch of five that half worked is five lines
    // here and one line in the footer.
    let log: Vec<(LogLevel, String, Option<String>)> = settled
        .iter()
        .filter_map(|report| match &report.state {
            GlobalSourceState::Ready { banks } => Some((
                LogLevel::Info,
                format!("{} - {banks} sound(s) imported", report.spec),
                None,
            )),
            GlobalSourceState::Missing(_) => {
                Some((LogLevel::Info, missing_source(&report.spec), None))
            }
            GlobalSourceState::Failed(why) => Some((
                LogLevel::Warn,
                format!("{} could not be imported: {why}", report.spec),
                Some(super::super::engine::import_alert(&report.spec)),
            )),
            GlobalSourceState::Loading | GlobalSourceState::Off => None,
        })
        .collect();
    // The log above covers every source that settled. The status line is
    // the one a take may have already claimed, so a quiet source is left
    // out of the sentence - and a batch of nothing else has none to say.
    let speaking: Vec<&rustel_runtime::samples::GlobalSourceReport> = settled
        .iter()
        .copied()
        .filter(|report| quiet != Some(report.spec.as_str()))
        .collect();
    count(&speaking, &mut sounds, &mut failed);
    let status = match speaking.as_slice() {
        [] => {
            return (!log.is_empty()).then_some(SettledSources { status: None, log });
        }
        [only] => match &only.state {
            GlobalSourceState::Ready { banks } => format!(
                "{} - {banks} sound(s) imported",
                status_sample_source(&only.spec, full_paths)
            ),
            GlobalSourceState::Missing(why) | GlobalSourceState::Failed(why) => {
                status_sample_source_error(&only.spec, why, full_paths)
            }
            GlobalSourceState::Loading | GlobalSourceState::Off => return None,
        },
        many if failed == 0 => format!("{} source(s) imported - {sounds} sound(s)", many.len()),
        many => format!(
            "{} of {} source(s) imported - {sounds} sound(s); see Log",
            many.len() - failed,
            many.len()
        ),
    };
    Some(SettledSources {
        status: Some(status),
        log,
    })
}

/// The log's line for a local source whose folder is not there. Not a
/// warning: the folder may be on a drive that is unplugged, and the row
/// stays on Sources until it is removed there.
pub(super) fn missing_source(spec: &str) -> String {
    format!("{spec} is missing - Settings → Samples to remove it")
}

/// Imports that brought banks the catalogue is filing under some other
/// import: the spec, how many it lost, and how many it claimed.
///
/// Banks are keyed by name across every import and the last row in wins,
/// so a folder and one of its own subfolders - the obvious second thing to
/// try when the first drop did not show what you expected - leave one of
/// the two with a heading, a count it earned, and no rows under it.
pub(super) fn shadowed_banks(
    reports: &[rustel_runtime::samples::GlobalSourceReport],
    catalogue: &[rustel_runtime::samples::SoundEntry],
) -> Vec<(String, usize, usize)> {
    let mut showing: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
    for entry in catalogue {
        if entry.origin != rustel_runtime::samples::SoundOrigin::Global {
            continue;
        }
        if let Some(import) = entry.import.as_deref() {
            *showing.entry(import).or_default() += 1;
        }
    }
    reports
        .iter()
        .filter_map(|report| {
            let rustel_runtime::samples::GlobalSourceState::Ready { banks } = report.state else {
                return None;
            };
            let listed = showing.get(report.spec.as_str()).copied().unwrap_or(0);
            let lost = banks.checked_sub(listed).filter(|lost| *lost > 0)?;
            Some((report.spec.clone(), lost, banks))
        })
        .collect()
}

/// The folder a local source names: a `local:` spec, or an absolute path.
pub(super) fn sample_source_folder(spec: &str) -> Option<&std::path::Path> {
    let spec = spec.trim();
    if let Some(folder) = spec.strip_prefix("local:") {
        Some(std::path::Path::new(folder.trim()))
    } else {
        let path = std::path::Path::new(spec);
        path.is_absolute().then_some(path)
    }
}

/// Remote source names remain useful addresses; local folders follow the
/// same privacy setting as saved files, without changing the stored source.
pub(super) fn status_sample_source(spec: &str, full: bool) -> String {
    if !full && let Some(folder) = sample_source_folder(spec) {
        status_file_path(folder, false)
    } else {
        spec.trim().to_owned()
    }
}

pub(super) fn status_sample_source_error(spec: &str, reason: &str, full: bool) -> String {
    let label = status_sample_source(spec, full);
    // Filesystem diagnostics can contain canonical paths or nested names
    // that differ from the source spelling. Their full detail stays in Log.
    if !full && sample_source_folder(spec).is_some() {
        format!("{label} - could not import; see Log")
    } else {
        format!("{label}: {reason}")
    }
}

impl App {
    /// Show the report already observed for this import. A local scan can
    /// finish before adoption returns, so its receipt must not wait for a
    /// later poll to observe a state change that has already happened.
    pub(super) fn show_sample_source_status(&mut self, spec: &str, pending: &str) {
        use rustel_runtime::samples::GlobalSourceState;
        let Some(state) = self
            .source_reports
            .iter()
            .find(|report| report.spec == spec.trim())
            .map(|report| &report.state)
        else {
            return;
        };
        let label = status_sample_source(spec, self.ui_settings.show_full_paths);
        self.status = match state {
            GlobalSourceState::Ready { banks } => {
                format!("{label} - {banks} sound(s) imported")
            }
            GlobalSourceState::Loading => format!("{label} - {pending}"),
            GlobalSourceState::Missing(why) | GlobalSourceState::Failed(why) => {
                status_sample_source_error(spec, why, self.ui_settings.show_full_paths)
            }
            GlobalSourceState::Off => return,
        };
    }

    /// The imported sources as the Sources page reads them: what each one
    /// is, and what it brought or why it did not.
    pub(super) fn source_rows(&self) -> Vec<super::super::settings::SourceRow> {
        use rustel_runtime::samples::GlobalSourceState;
        let mut rows: Vec<super::super::settings::SourceRow> = self
            .prefs
            .sample_sources
            .iter()
            .map(|source| {
                let spec = source.spec.trim();
                let tracked = self.user_sources.iter().find(|user| user.spec == spec);
                let local = tracked.is_some_and(|user| user.local)
                    || rustel_runtime::samples::source_is_local(spec);
                let state = self
                    .source_reports
                    .iter()
                    .find(|report| report.spec == spec)
                    .map(|report| report.state.clone());
                let alias_detail = self.worker.library().and_then(|library| {
                    let aliases = library.automatic_aliases_for_import(spec);
                    (!aliases.is_empty()).then(|| {
                        aliases
                            .into_iter()
                            .map(|(from, to)| format!("{from}→{to}"))
                            .collect::<Vec<_>>()
                            .join(", ")
                    })
                });
                let (state, dimmed, cache_fill) = match state {
                    Some(GlobalSourceState::Ready { banks }) => {
                        let decorate = |state: String| {
                            alias_detail
                                .as_ref()
                                .map_or(state.clone(), |aliases| format!("{state} · {aliases}"))
                        };
                        if local {
                            (decorate(format!("{banks} sound(s)")), false, None)
                        } else if let Some(user) = tracked.filter(|user| user.files > 0) {
                            (
                                decorate(super::super::settings::source_cache_state(
                                    user.sounds.max(banks),
                                    user.cached_files,
                                    user.files,
                                    user.cache_bytes,
                                )),
                                false,
                                Some((user.cached_files, user.files)),
                            )
                        } else {
                            (decorate(format!("{banks} sound(s)")), false, None)
                        }
                    }
                    Some(GlobalSourceState::Loading) => ("fetching…".to_owned(), false, None),
                    Some(GlobalSourceState::Missing(why)) => {
                        (format!("missing - {why}"), true, None)
                    }
                    Some(GlobalSourceState::Failed(why)) => (format!("failed - {why}"), true, None),
                    Some(GlobalSourceState::Off) | None if !source.enabled => {
                        ("off".to_owned(), true, None)
                    }
                    _ => ("-".to_owned(), true, None),
                };
                super::super::settings::SourceRow {
                    spec: source.spec.clone(),
                    label: String::new(),
                    state,
                    dimmed,
                    shipped: false,
                    local,
                    cache_fill,
                    cache: tracked.and_then(|user| user.cache.clone()),
                }
            })
            .collect();
        // Named as a set: what makes one folder's name enough is the other
        // folders it is listed with.
        let labels = super::super::reference::source_labels(
            &rows.iter().map(|row| row.spec.clone()).collect::<Vec<_>>(),
        );
        for (row, label) in rows.iter_mut().zip(labels) {
            row.label = label;
        }
        // Then the packs the studio ships with, after the player's own:
        // they are the library's, listed so their download can be asked
        // for and followed pack by pack, and drawn apart from the imports.
        rows.extend(
            self.shipped_sources
                .iter()
                .map(|shipped| shipped.row(self.library_loading)),
        );
        rows
    }

    /// What each shipped pack holds, read off the library: a walk over
    /// every default bank, so on the poll and when the lists land - never
    /// on a frame.
    fn refresh_shipped_sources(&mut self) {
        let Some(library) = self.worker.library() else {
            return;
        };
        let mut changed = false;
        for shipped in &mut self.shipped_sources {
            let (sounds, _files) = library.default_source_holds(&shipped.source);
            let (_, file_urls) = library.default_source_files(&shipped.source);
            shipped.seen_files.extend(file_urls.iter().cloned());
            let (cached_files, cache_bytes) = library.count_cached_urls(shipped.seen_files.iter());
            let files = file_urls.len();
            let counts_changed = (sounds, files, cached_files, cache_bytes)
                != (
                    shipped.sounds,
                    shipped.files,
                    shipped.cached_files,
                    shipped.cache_bytes,
                );
            if counts_changed {
                shipped.sounds = sounds;
                shipped.files = files;
                shipped.cached_files = cached_files;
                shipped.cache_bytes = cache_bytes;
                changed = true;
            }
        }
        self.dirty_frame |= changed;
    }

    /// What each user import holds, read off the library: local folders
    /// skip the cache walk; remote packs count files the way shipped ones
    /// do. Kept in prefs order, and a pack already caching keeps its bar.
    fn refresh_user_sources(&mut self) {
        let Some(library) = self.worker.library() else {
            return;
        };
        let in_flight: std::collections::HashMap<
            String,
            super::super::settings::SourceCacheProgress,
        > = self
            .user_sources
            .iter()
            .filter_map(|user| {
                user.cache
                    .as_ref()
                    .filter(|progress| !progress.done())
                    .cloned()
                    .map(|progress| (user.spec.clone(), progress))
            })
            .collect();
        let next: Vec<UserSource> = self
            .prefs
            .sample_sources
            .iter()
            .map(|source| {
                let spec = source.spec.trim().to_owned();
                let local = rustel_runtime::samples::source_is_local(&spec);
                let prior_seen = self
                    .user_sources
                    .iter()
                    .find(|user| user.spec == spec)
                    .map(|user| user.seen_files.clone())
                    .unwrap_or_default();
                let (sounds, files, cached_files, cache_bytes, seen_files) = if local {
                    let (sounds, files) = library.import_source_holds(&spec);
                    (sounds, files, files, 0, HashSet::new())
                } else {
                    let (sounds, _) = library.import_source_holds(&spec);
                    let (_, file_urls) = library.import_source_files(&spec);
                    let mut seen = prior_seen;
                    seen.extend(file_urls.iter().cloned());
                    let (cached_files, cache_bytes) = library.count_cached_urls(seen.iter());
                    (sounds, file_urls.len(), cached_files, cache_bytes, seen)
                };
                UserSource {
                    spec: spec.clone(),
                    local,
                    sounds,
                    files,
                    cached_files,
                    cache_bytes,
                    seen_files,
                    cache: in_flight.get(&spec).cloned(),
                }
            })
            .collect();
        if next != self.user_sources {
            self.user_sources = next;
            self.dirty_frame = true;
        }
    }

    /// Fetch one shipped pack onto disk, from `c` on its row: the files
    /// it names that are not there already, behind whatever a score is
    /// about to play. Its row counts them down.
    ///
    /// Queuing thousands of files is a directory walk and must not run on
    /// the UI thread, or repeated `c` presses freeze the sheet. A pack that
    /// is already caching ignores further presses until it finishes.
    pub(super) fn cache_default_source(&mut self, at: usize) {
        let Some(library) = self.worker.library() else {
            self.status = "the sample library is not up yet".into();
            self.dirty_frame = true;
            return;
        };
        let Some(shipped) = self.shipped_sources.get(at) else {
            return;
        };
        let name = shipped.source.name.clone();
        if shipped.sounds == 0 {
            self.status = format!("{name} - its list of sounds is not in yet");
            self.dirty_frame = true;
            return;
        }
        if shipped
            .cache
            .as_ref()
            .is_some_and(|progress| !progress.done())
        {
            self.status = format!("{name} - still caching; wait for the row to finish");
            self.dirty_frame = true;
            return;
        }
        let source = shipped.source.clone();
        let files = shipped.files.max(1);
        let left = files.saturating_sub(shipped.cached_files).max(1);
        if let Some(shipped) = self.shipped_sources.get_mut(at) {
            // Claim the row before the walk starts so a held `c` cannot
            // stack another walk on the UI thread.
            shipped.cache = Some(super::super::settings::SourceCacheProgress {
                total: files,
                left,
                loading: None,
            });
        }
        let library = std::sync::Arc::clone(&library);
        std::thread::Builder::new()
            .name("cache-pack".into())
            .spawn(move || {
                let _ = library.cache_default_source(&source);
            })
            .ok();
        self.status = format!("{name} - caching {left} left");
        self.log.push(
            LogLevel::Info,
            "samples",
            format!("caching {name}: {files} file(s), in the background"),
        );
        self.dirty_frame = true;
    }

    /// Fetch one remote user pack onto disk, from `c` on its row. A local
    /// folder is already here - `c` is taken so it cannot fall through to
    /// the score, and nothing is queued.
    pub(super) fn cache_user_source(&mut self, at: usize) {
        let Some(library) = self.worker.library() else {
            self.status = "the sample library is not up yet".into();
            self.dirty_frame = true;
            return;
        };
        let Some(source) = self.prefs.sample_sources.get(at) else {
            return;
        };
        let spec = source.spec.trim().to_owned();
        let label = status_sample_source(&spec, self.ui_settings.show_full_paths);
        if rustel_runtime::samples::source_is_local(&spec) {
            self.status = format!("{label} is already on this machine");
            self.dirty_frame = true;
            return;
        }
        self.refresh_user_sources();
        let Some(user) = self.user_sources.iter().find(|user| user.spec == spec) else {
            return;
        };
        if user.sounds == 0 && user.files == 0 {
            self.status = format!("{label} - its list of sounds is not in yet");
            self.dirty_frame = true;
            return;
        }
        if user.cache.as_ref().is_some_and(|progress| !progress.done()) {
            self.status = format!("{label} - still caching; wait for the row to finish");
            self.dirty_frame = true;
            return;
        }
        let files = user.files.max(1);
        let left = files.saturating_sub(user.cached_files).max(1);
        if let Some(user) = self.user_sources.iter_mut().find(|user| user.spec == spec) {
            user.cache = Some(super::super::settings::SourceCacheProgress {
                total: files,
                left,
                loading: None,
            });
        }
        let library = std::sync::Arc::clone(&library);
        let cache_spec = spec.clone();
        std::thread::Builder::new()
            .name("cache-pack".into())
            .spawn(move || {
                let _ = library.cache_import_source(&cache_spec);
            })
            .ok();
        self.status = format!("{label} - caching {left} left");
        self.log.push(
            LogLevel::Info,
            "samples",
            format!("caching {label}: {files} file(s), in the background"),
        );
        self.dirty_frame = true;
    }

    /// Every shipped pack's row, brought up to date with the loader: files
    /// of its still in the line, and the one in hand. Only for packs whose
    /// download was asked for - the others' rows say what they hold.
    fn follow_shipped_caches(&mut self) {
        let Some(library) = self.worker.library() else {
            return;
        };
        let mut changed = false;
        let mut finished_a_pack = false;
        for shipped in &mut self.shipped_sources {
            let Some(progress) = shipped.cache.as_mut() else {
                continue;
            };
            if progress.done() {
                let (cached_files, cached_total, cache_bytes) =
                    library.default_source_cached(&shipped.source);
                shipped.cached_files = cached_files;
                shipped.files = cached_total.max(shipped.files);
                shipped.cache_bytes = cache_bytes;
                shipped.cache = None;
                changed = true;
                finished_a_pack = true;
                continue;
            }
            // Down only, like the library's own bar: a score's loads share
            // the line, and a count that climbed would read as a download
            // starting over.
            let left = library
                .pending_cache_under(&shipped.source.base)
                .min(progress.left);
            let loading = library.loading_under(&shipped.source.base);
            if left != progress.left || loading != progress.loading {
                progress.left = left;
                progress.loading = loading;
                changed = true;
            }
        }
        if finished_a_pack {
            // Pack finished writing files: refresh the clear-cache size.
            self.cache_measure_due = Some(Instant::now());
        }
        self.dirty_frame |= changed;
    }

    /// User remote packs' rows, brought up to date with the loader the
    /// same way the shipped ones are.
    fn follow_user_caches(&mut self) {
        let Some(library) = self.worker.library() else {
            return;
        };
        let mut changed = false;
        let mut finished_a_pack = false;
        for user in &mut self.user_sources {
            if user.local {
                continue;
            }
            let Some(progress) = user.cache.as_mut() else {
                continue;
            };
            if progress.done() {
                let (cached_files, cached_total, cache_bytes) =
                    library.import_source_cached(&user.spec);
                user.cached_files = cached_files;
                user.files = cached_total.max(user.files);
                user.cache_bytes = cache_bytes;
                user.cache = None;
                changed = true;
                finished_a_pack = true;
                continue;
            }
            let left = library
                .pending_cache_for_import(&user.spec)
                .min(progress.left);
            let loading = library.loading_for_import(&user.spec);
            if left != progress.left || loading != progress.loading {
                progress.left = left;
                progress.loading = loading;
                changed = true;
            }
        }
        if finished_a_pack {
            self.cache_measure_due = Some(Instant::now());
        }
        self.dirty_frame |= changed;
    }

    /// The Sources page's rows, brought up to date on the readiness poll:
    /// the pre-cache's, the shipped packs', the user imports', and the
    /// cache size while downloads write files. `loading` is whether the
    /// library still had manifests on the way when the poll sampled it.
    pub(super) fn poll_source_rows(&mut self, loading: bool) {
        self.follow_precache();
        // The shipped packs' rows: what each holds, while their lists are
        // still landing, and how far each download is while one runs.
        if loading || self.shipped_sources.iter().any(|s| s.sounds == 0) {
            self.refresh_shipped_sources();
        }
        self.follow_shipped_caches();
        let sources_open = self
            .settings_sheet
            .is_some_and(|sheet| sheet.shows_sources());
        if sources_open
            || loading
            || self.user_sources.len() != self.prefs.sample_sources.len()
            || self
                .user_sources
                .iter()
                .any(|user| !user.local && user.sounds == 0)
        {
            self.refresh_user_sources();
        }
        self.follow_user_caches();
        // Remeasure on-disk size while downloads write files, so the clear-
        // cache row's MiB keeps up without walking the folder every frame.
        if self.downloads_active()
            && self.cache_measure.is_none()
            && self
                .cache_measure_due
                .is_none_or(|due| Instant::now() >= due)
        {
            self.measure_sample_cache();
            self.cache_measure_due = Some(Instant::now() + CACHE_MEASURE_WHILE_DOWNLOADING);
        }
    }

    /// Forget the sources left inside sets that are gone from the sets
    /// folder, saying so in the log.
    pub(super) fn forget_sources_in_gone_sets(&mut self) {
        let gone = self.prefs.forget_sources_in_gone_sets();
        for spec in &gone {
            self.log.push(
                LogLevel::Info,
                "samples",
                format!("{spec} is no longer a source - the set it was in is gone"),
            );
        }
        if !gone.is_empty() {
            self.save_prefs_soon();
        }
    }

    /// Hand the imported sources to the library, replacing what it had.
    ///
    /// Every set sees these, so this runs once at startup and again
    /// whenever the list changes - not when a set opens, which is what
    /// `adopt_set_samples` is for.
    pub(super) fn adopt_global_sources(&mut self) {
        if self.prefs.deduplicate_sample_sources() {
            self.save_prefs_soon();
        }
        let Some(library) = self.worker.library() else {
            return;
        };
        // The renames are an overlay on what a source brings, so they have
        // to be in place before it brings anything.
        library.set_bank_renames(self.prefs.bank_renames());
        library.set_source_bank_renames(self.prefs.source_bank_renames());
        let before =
            self.replace_source_reports(library.adopt_global_sources(&self.prefs.global_sources()));
        let assigned: Vec<(String, String, String)> = self
            .prefs
            .sample_sources
            .iter()
            .flat_map(|source| {
                library
                    .automatic_aliases_for_import(&source.spec)
                    .into_iter()
                    .map(move |(from, to)| (source.spec.clone(), from, to))
            })
            .collect();
        let mut kept_assignment = false;
        for (source, from, to) in assigned {
            let aliases = self.prefs.sample_source_renames.entry(source).or_default();
            if let std::collections::btree_map::Entry::Vacant(entry) = aliases.entry(from) {
                entry.insert(to);
                kept_assignment = true;
            }
        }
        if kept_assignment {
            library.set_source_bank_renames(self.prefs.source_bank_renames());
            self.save_prefs_soon();
        }
        // A pack's list lands on the loader's own schedule; warming it now
        // would find nothing to warm. The poll pays the pre-cache once every
        // manifest is in.
        self.precache_owed = self.ui_settings.precache_sources;
        self.refresh_user_sources();
        // Every source, every time, with the whole path - the log is the
        // one place a spec is written out in full, since the browser has
        // to shorten it to fit a column. A source that came back Ready and
        // shows nothing in the tree is a difference you can only see if
        // both numbers are written down somewhere.
        for report in &self.source_reports {
            use rustel_runtime::samples::GlobalSourceState;
            match &report.state {
                // Said once while it stays missing, not at every adoption.
                GlobalSourceState::Missing(_) => {
                    let known = before.iter().any(|earlier| {
                        earlier.spec == report.spec
                            && matches!(earlier.state, GlobalSourceState::Missing(_))
                    });
                    self.log.push(
                        if known {
                            LogLevel::Debug
                        } else {
                            LogLevel::Info
                        },
                        "samples",
                        missing_source(&report.spec),
                    )
                }
                GlobalSourceState::Failed(why) => self.log.push_alert(
                    LogLevel::Warn,
                    "samples",
                    format!("{} was not imported: {why}", report.spec),
                    super::super::engine::import_alert(&report.spec),
                ),
                GlobalSourceState::Ready { banks } => self.log.push(
                    LogLevel::Debug,
                    "samples",
                    format!("{} - {banks} bank(s)", report.spec),
                ),
                GlobalSourceState::Loading => self.log.push(
                    LogLevel::Debug,
                    "samples",
                    format!("{} - fetching its list", report.spec),
                ),
                GlobalSourceState::Off => {
                    self.log
                        .push(LogLevel::Debug, "samples", format!("{} - off", report.spec))
                }
            }
        }
        self.report_shadowed_banks(&library);
        self.refresh_catalogue();
        self.dirty_frame = true;
    }

    /// Make `reports` the ones the Sources page reads, and return the ones
    /// they replace. A source that reads Ready has nothing left to warn
    /// about: its earlier import failure, if it had one, is resolved.
    fn replace_source_reports(
        &mut self,
        reports: Vec<rustel_runtime::samples::GlobalSourceReport>,
    ) -> Vec<rustel_runtime::samples::GlobalSourceReport> {
        for report in &reports {
            if matches!(
                report.state,
                rustel_runtime::samples::GlobalSourceState::Ready { .. }
            ) {
                self.log
                    .resolve_alert(&super::super::engine::import_alert(&report.spec));
            }
        }
        std::mem::replace(&mut self.source_reports, reports)
    }

    /// The sample sources' part of the readiness poll, in three steps: fold
    /// in the library's source reports when they have changed, pay a
    /// pre-cache asked for before the packs' lists were in once nothing is
    /// still on its way, and, when the reports changed, keep in the
    /// preferences the bank names the library gave an import on its own.
    pub(super) fn poll_source_reports_precache_and_aliases(&mut self) {
        let mut sources_changed = false;
        if let Some(library) = self.worker.library() {
            sources_changed = self.fold_in_source_reports(&library);
            self.pay_owed_precache(&library);
        }
        if sources_changed {
            self.persist_automatic_bank_aliases();
        }
    }

    /// Take the library's source reports when they differ from the ones
    /// the Sources page has, so it follows the packs as they land: the
    /// catalogue is refreshed, and an import that settled is logged and
    /// said on the status line. Returns whether the reports changed.
    fn fold_in_source_reports(&mut self, library: &rustel_runtime::samples::SampleLibrary) -> bool {
        let reports = library.global_source_reports();
        if reports != self.source_reports {
            // A folder is walked off this thread, so the receipt for
            // an import arrives here rather than at the keypress that
            // asked for it. Without this the status said "fetching its
            // list of sounds" and never said anything again.
            let settled = settled_sources(
                &self.source_reports,
                &reports,
                self.ui_settings.show_full_paths,
                self.quiet_settled_source.as_deref(),
            );
            self.replace_source_reports(reports);
            self.refresh_catalogue();
            if let Some(settled) = settled {
                for (level, line, alert) in settled.log {
                    self.log.push_with(level, "samples", line, alert);
                }
                if let Some(status) = settled.status {
                    self.status = status;
                }
                // Whatever the take asked to stay quiet about has now
                // settled and been logged; the next import speaks for
                // itself again.
                self.quiet_settled_source = None;
            }
            self.dirty_frame = true;
            return true;
        }
        false
    }

    /// Pay the pre-cache asked for before the packs' lists were in, once
    /// the library has no manifest left on its way.
    fn pay_owed_precache(&mut self, library: &rustel_runtime::samples::SampleLibrary) {
        if self.precache_owed && library.manifests_pending() == 0 {
            self.precache_owed = false;
            self.precache_sources();
        }
    }

    /// Keep in the preferences the bank names the library gave an import
    /// on its own, and hand the library the renames when one was new.
    fn persist_automatic_bank_aliases(&mut self) {
        let assigned: Vec<(String, String, String)> = {
            let Some(library) = self.worker.library() else {
                return;
            };
            self.prefs
                .sample_sources
                .iter()
                .flat_map(|source| {
                    library
                        .automatic_aliases_for_import(&source.spec)
                        .into_iter()
                        .map(move |(from, to)| (source.spec.clone(), from, to))
                })
                .collect()
        };
        let mut changed = false;
        for (source, from, to) in assigned {
            let aliases = self.prefs.sample_source_renames.entry(source).or_default();
            if let std::collections::btree_map::Entry::Vacant(entry) = aliases.entry(from) {
                entry.insert(to);
                changed = true;
            }
        }
        if changed {
            if let Some(library) = self.worker.library() {
                library.set_source_bank_renames(self.prefs.source_bank_renames());
            }
            self.save_prefs_soon();
        }
    }

    /// Say when an import that succeeded is nevertheless showing nothing.
    ///
    /// Banks are keyed by name across every import, and the last row wins.
    /// Drop a pack and then one of its own subfolders - which is the
    /// obvious thing to try when the first drop did not show what you
    /// expected - and every bank in the second is a bank the first already
    /// had, so one of the two ends up with a heading, a count it earned,
    /// and no rows underneath. Nothing failed, so the import looked broken.
    /// The warning names the import and counts its banks that are listed
    /// under another one.
    fn report_shadowed_banks(&mut self, library: &rustel_runtime::samples::SampleLibrary) {
        for (spec, lost, banks) in shadowed_banks(&self.source_reports, &library.catalogue()) {
            self.log.push(
                LogLevel::Warn,
                "samples",
                format!(
                    "{spec}: {lost} of {banks} bank(s) share a name with another import and are listed under it"
                ),
            );
        }
    }

    /// Refetch every remote pack's list - shipped defaults and user URL
    /// imports - so new files show up in the fractions before cache all.
    pub(super) fn refresh_all_sample_packs(&mut self) {
        let Some(library) = self.worker.library() else {
            self.status = "the sample library is not up yet".into();
            self.dirty_frame = true;
            return;
        };
        for source in &self.prefs.sample_sources {
            if !source.enabled {
                continue;
            }
            let spec = source.spec.trim();
            if rustel_runtime::samples::source_is_local(spec) {
                continue;
            }
            library.forget_manifest(spec);
        }
        if let Err(why) = library.refresh_default_sources() {
            self.status = format!("could not refresh the default packs - {why}");
            self.dirty_frame = true;
            return;
        }
        self.adopt_global_sources();
        self.status = "refreshing every remote pack's list…".into();
        self.dirty_frame = true;
    }

    /// Fetch what a source holds into the cache now, from Enter on its row.
    pub(super) fn refresh_sample_source(&mut self, at: usize) {
        let Some(source) = self.prefs.sample_sources.get(at).cloned() else {
            return;
        };
        let label = status_sample_source(&source.spec, self.ui_settings.show_full_paths);
        if !source.enabled {
            self.status = format!("{label} is off - Space turns it on");
            return;
        }
        // Re-adopting walks a folder again, so files added since are picked
        // up. A pack answers out of the cache unless the cache is told to
        // forget it, so that comes first: Enter means "go and get it", and
        // for a `shabda:` source it means a fresh draw.
        if let Some(library) = self.worker.library() {
            library.forget_manifest(&source.spec);
        }
        self.adopt_global_sources();
        let missing = self
            .source_reports
            .iter()
            .find_map(|report| match &report.state {
                rustel_runtime::samples::GlobalSourceState::Missing(why)
                    if report.spec == source.spec.trim() =>
                {
                    Some(why.clone())
                }
                _ => None,
            });
        if let Some(why) = missing {
            self.status = format!("{label} is missing - {why}");
            return;
        }
        // Enter on a row means its files too, whether or not background
        // pre-caching is on: that switch is about every source all the
        // time, this is about this one now. Paid when the list is in.
        self.precache_owed = true;
        self.status = format!("{label} - fetching again");
        self.show_sample_source_status(&source.spec, "fetching again");
    }

    /// Cache the imported sounds the current score actually names: the
    /// same as `c` on a pack, but only the files those names reach. Fetch
    /// imports is this, automatic. Progress lives on the pack rows, not
    /// on the switch.
    pub(super) fn precache_sources(&mut self) {
        let Some(library) = self.worker.library() else {
            return;
        };
        let source = self.scenes.current().editor.source();
        self.cache_score_imports(&source, &library);
    }

    /// Put each imported sound the score names onto disk. Bank-prefixed
    /// and bare names are both tried, matching how a note looks them up.
    /// Defaults and fonts are left alone: those are `c` / cache all.
    pub(super) fn cache_score_imports(
        &self,
        source: &str,
        library: &rustel_runtime::samples::SampleLibrary,
    ) {
        let banks = rustel_runtime::sounds::banks_in_score(source);
        for sound in rustel_runtime::sounds::in_score(source) {
            let (name, index) = match sound.split_once(':') {
                Some((name, index)) => (name, index.parse::<f64>().ok()),
                None => (sound.as_str(), None),
            };
            let mut candidates = banks
                .iter()
                .map(|bank| format!("{bank}_{name}"))
                .collect::<Vec<_>>();
            candidates.push(name.to_owned());
            for candidate in candidates {
                if !library.is_imported_sound(&candidate) {
                    continue;
                }
                let spec = match index {
                    Some(n) => format!("{candidate}:{n}"),
                    None => candidate,
                };
                let _ = library.cache_to_disk(&spec);
                break;
            }
        }
    }

    /// Every remote pack - shipped defaults and user URL imports - fetched
    /// once onto disk. Local folders are already here. Pack by pack, so
    /// each row on Sources follows its own files.
    pub(super) fn cache_whole_library(&mut self) {
        let Some(library) = self.worker.library() else {
            self.status = "the sample library is not up yet".into();
            self.dirty_frame = true;
            return;
        };
        self.refresh_user_sources();
        if library.manifests_pending() > 0 && self.shipped_sources.iter().all(|s| s.sounds == 0) {
            self.status = "the packs' lists are not in yet - try again in a moment".into();
            self.dirty_frame = true;
            return;
        }
        if self.shipped_sources.iter().any(|shipped| {
            shipped
                .cache
                .as_ref()
                .is_some_and(|progress| !progress.done())
        }) || self
            .user_sources
            .iter()
            .any(|user| user.cache.as_ref().is_some_and(|progress| !progress.done()))
        {
            self.status = "still caching packs - wait for the rows to finish".into();
            self.dirty_frame = true;
            return;
        }
        let mut total_files = 0usize;
        let mut sources = Vec::with_capacity(self.shipped_sources.len());
        for shipped in &mut self.shipped_sources {
            if shipped.files == 0 || shipped.cached_files >= shipped.files {
                shipped.cache = None;
                continue;
            }
            let files = shipped.files;
            let left = files.saturating_sub(shipped.cached_files).max(1);
            total_files = total_files.saturating_add(files);
            shipped.cache = Some(super::super::settings::SourceCacheProgress {
                total: files,
                left,
                loading: None,
            });
            sources.push(shipped.source.clone());
        }
        let mut import_specs = Vec::new();
        for user in &mut self.user_sources {
            if user.local || user.files == 0 || user.cached_files >= user.files {
                user.cache = None;
                continue;
            }
            let files = user.files;
            let left = files.saturating_sub(user.cached_files).max(1);
            total_files = total_files.saturating_add(files);
            user.cache = Some(super::super::settings::SourceCacheProgress {
                total: files,
                left,
                loading: None,
            });
            import_specs.push(user.spec.clone());
        }
        if total_files == 0 {
            let any_remote = self.shipped_sources.iter().any(|shipped| shipped.files > 0)
                || self
                    .user_sources
                    .iter()
                    .any(|user| !user.local && user.files > 0);
            self.status = if any_remote {
                "every remote pack is already on disk".into()
            } else {
                "the remote packs have nothing to cache yet".into()
            };
            self.dirty_frame = true;
            return;
        }
        self.precache = Some(super::super::settings::PrecacheProgress {
            kind: super::super::settings::PrecacheKind::Library,
            total: total_files.max(1),
            left: total_files.max(1),
            loading: None,
        });
        let library = std::sync::Arc::clone(&library);
        std::thread::Builder::new()
            .name("cache-library".into())
            .spawn(move || {
                for source in &sources {
                    let _ = library.cache_default_source(source);
                }
                for spec in &import_specs {
                    let _ = library.cache_import_source(spec);
                }
                // Strays: default banks under no pack's base.
                let claimed: std::collections::HashSet<String> =
                    sources.iter().map(|source| source.base.clone()).collect();
                let under_a_pack = |location: &str| {
                    claimed
                        .iter()
                        .any(|base| location.starts_with(base.trim_end_matches('/')))
                };
                for entry in library.catalogue() {
                    if entry.origin == rustel_runtime::samples::SoundOrigin::Default
                        && !entry.location.as_deref().is_some_and(&under_a_pack)
                    {
                        let _ = library.cache_to_disk(&entry.name);
                    }
                }
            })
            .ok();
        self.status =
            "caching every remote pack onto disk - each row counts its own files down".into();
        self.log.push(
            LogLevel::Info,
            "samples",
            format!("caching all remote packs: {total_files} file(s), in the background"),
        );
        self.dirty_frame = true;
    }

    /// The pre-cache's row, brought up to date: files left in the line
    /// and the one in hand. The count only ever goes down - a score's own
    /// loads join the same line, and a bar that climbs back up reads as a
    /// download starting over. When the last file is in, the log says so
    /// and the cache row is measured again.
    fn follow_precache(&mut self) {
        let Some(library) = self.worker.library() else {
            return;
        };
        let Some(progress) = self.precache.as_mut() else {
            return;
        };
        if progress.done() {
            return;
        }
        let left = library.pending_loads().min(progress.left);
        let loading = library.loading_now();
        if left == progress.left && loading == progress.loading {
            return;
        }
        progress.left = left;
        progress.loading = loading;
        self.dirty_frame = true;
        if progress.done() {
            let total = progress.total;
            let message = match progress.kind {
                super::super::settings::PrecacheKind::Imports => {
                    format!("fetch imports done - {total} file(s)")
                }
                super::super::settings::PrecacheKind::Library => {
                    format!("remote packs cached - {total} file(s)")
                }
            };
            self.log.push(LogLevel::Info, "samples", message);
            self.measure_sample_cache();
        }
    }

    /// Walk the sample cache and remember what it holds.
    ///
    /// Once when the sheet opens, while downloads run, and after it is
    /// emptied - never on a frame: it is a directory walk, and the number
    /// it produces changes about as often as you download a sample bank.
    pub(super) fn measure_sample_cache(&mut self) {
        // One walk at a time: stacking receivers would drop earlier results.
        if self.cache_measure.is_some() {
            return;
        }
        // Off the UI thread: a large cache is a directory walk of tens of
        // thousands of entries, and the sheet should not open a frame late
        // for a number that says "-" perfectly well until it arrives.
        let (tx, rx) = std::sync::mpsc::channel();
        self.cache_measure = Some(rx);
        std::thread::Builder::new()
            .name("sample-cache-measure".into())
            .spawn(move || {
                let _ = tx.send(rustel_runtime::samples::sample_cache_usage());
            })
            .ok();
    }

    /// Files are still landing in the sample cache - pack `c`/`C`, fetch
    /// imports, or the loader's ordinary line.
    pub(super) fn downloads_active(&self) -> bool {
        self.caching_samples > 0
            || self
                .precache
                .as_ref()
                .is_some_and(|progress| !progress.done())
            || self.shipped_sources.iter().any(|shipped| {
                shipped
                    .cache
                    .as_ref()
                    .is_some_and(|progress| !progress.done())
            })
            || self
                .user_sources
                .iter()
                .any(|user| user.cache.as_ref().is_some_and(|progress| !progress.done()))
    }

    /// What the cache row reads.
    pub(super) fn sample_cache_label(&self) -> String {
        let Some(bytes) = self.sample_cache_bytes else {
            return "-".to_owned();
        };
        super::super::settings::format_cache_bytes(bytes)
    }

    /// Empty the sample cache, after a confirmed Enter on its row.
    ///
    /// Off the UI thread: emptying can be a walk of thousands of files and
    /// must not freeze the sheet. Pack rows are zeroed at once so they do
    /// not keep reading as fully cached while the folder is still being
    /// cleared.
    pub(super) fn clear_sample_cache(&mut self) {
        if self.cache_clear.is_some() {
            self.status = "still clearing the cache…".into();
            self.dirty_frame = true;
            return;
        }
        for shipped in &mut self.shipped_sources {
            shipped.cached_files = 0;
            shipped.cache_bytes = 0;
            shipped.seen_files.clear();
            shipped.cache = None;
        }
        for user in &mut self.user_sources {
            user.cached_files = 0;
            user.cache_bytes = 0;
            user.seen_files.clear();
            user.cache = None;
        }
        self.sample_cache_bytes = None;
        self.precache = None;
        let (tx, rx) = std::sync::mpsc::channel();
        self.cache_clear = Some(rx);
        std::thread::Builder::new()
            .name("sample-cache-clear".into())
            .spawn(move || {
                let _ = tx.send(rustel_runtime::samples::clear_sample_cache());
            })
            .ok();
        self.status = "clearing the cache…".into();
        self.dirty_frame = true;
    }

    /// The cache walk and the cache clear run on their own threads, and
    /// what they come back with is picked up on the readiness poll: the
    /// size for the cache row, and a clear's outcome for the status line.
    pub(super) fn poll_cache_results(&mut self) {
        if let Some(measured) = self
            .cache_measure
            .as_ref()
            .and_then(|rx| rx.try_recv().ok())
        {
            self.sample_cache_bytes = Some(measured);
            self.cache_measure = None;
            self.dirty_frame = true;
        }
        if let Some(outcome) = self.cache_clear.as_ref().and_then(|rx| rx.try_recv().ok()) {
            self.cache_clear = None;
            match outcome {
                Ok(()) => {
                    // Counts were zeroed when the clear started; measure
                    // again so the size row says empty, and walk the packs
                    // once more in case anything survived.
                    self.measure_sample_cache();
                    self.refresh_shipped_sources();
                    self.status = "cache cleared - files download again when needed".into();
                }
                Err(error) => {
                    self.refresh_shipped_sources();
                    self.measure_sample_cache();
                    self.status = format!("cache not cleared: {error}");
                    self.log.push(LogLevel::Warn, "samples", error);
                }
            }
            self.dirty_frame = true;
        }
    }
}
