//! Recording audio from the studio: starting and stopping mix takes
//! (Ctrl+Shift+R) and input samples, handling the engine's replies, trimming
//! silence and saving each finished take into the recordings bank as a playable
//! sound such as `recordings:4`. The same silence trim runs on a local sample
//! when Alt+T asks for it in the samples tab. It also covers the recording chip
//! and take-ended notice shown in the header, and writing settled slider and
//! fader moves onto the session tape.

use super::*;

/// How long a take that ended on its own stays said in the header.
pub(super) const TAKE_NOTICE_DURATION: Duration = Duration::from_secs(8);

/// Where Alt+T would cut, and whether it is even allowed to: the same
/// address Alt+O reveals resolves through [`bank_location`] to a real
/// file, checked against the two things trim refuses regardless of what
/// is on it - a downloaded pack sample, shared and re-fetchable, and
/// anything that is not the recorder's own `.wav`.
pub(super) fn trim_candidate(
    library: &rustel_runtime::samples::SampleLibrary,
    name: &str,
    variant: Option<usize>,
) -> Result<PathBuf, String> {
    let located = library
        .file_location(name, variant)
        .ok_or_else(|| format!("{name} has no file of its own - nothing to trim"))?;
    let path = match bank_location(&located) {
        Some(RevealTarget::File(path)) => path,
        _ => return Err(format!("{name} is not a local file - nothing to trim")),
    };
    if library.is_in_download_cache(&path) {
        return Err(format!(
            "{name} is a downloaded pack sample - trim only touches your own local files"
        ));
    }
    let is_wav = path
        .extension()
        .and_then(|ext| ext.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("wav"));
    if !is_wav {
        return Err("trim only works on .wav files".to_owned());
    }
    Ok(path)
}

/// A sample recording from the input, as the app keeps it.
pub(super) struct SampleTake {
    pub(super) started: Instant,
    pub(super) path: PathBuf,
    /// The whole second the header last drew, so its clock moves while the
    /// engine is idle and nothing else asks for a frame.
    pub(super) shown_second: u64,
}

/// Where `file` sits, or will sit once written, among the audio files
/// directly in `directory`, in the name order a folder bank lists them in:
/// the `n` that plays it. Known before the file exists, so a recording can
/// say what it will be called while it is still being made.
pub(super) fn variant_index(directory: &std::path::Path, file: &std::path::Path) -> Option<usize> {
    let name = file.file_name()?;
    let before = std::fs::read_dir(directory)
        .ok()?
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_file()))
        .map(|entry| entry.path())
        .filter(|path| rustel_runtime::samples::is_sample_audio(path))
        .filter_map(|path| path.file_name().map(std::ffi::OsStr::to_os_string))
        .filter(|candidate| candidate.as_os_str() < name)
        .count();
    Some(before)
}

/// The sound a recording in the recordings folder plays as: its folder's
/// bank and its place in it, `recordings:4`.
pub(super) fn recording_sound(path: &std::path::Path) -> String {
    let directory = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty());
    let bank = directory
        .and_then(std::path::Path::file_name)
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "recordings".to_owned());
    match directory.and_then(|directory| variant_index(directory, path)) {
        Some(index) => format!("{bank}:{index}"),
        None => bank,
    }
}

pub(super) enum TrimTarget {
    Take {
        path: PathBuf,
        /// A sample recorded from the input rather than a take of the mix:
        /// the same bank, told as a sample.
        sample: bool,
    },
    Sample {
        name: String,
        path: PathBuf,
        decoded: HashSet<SampleId>,
    },
}

pub(super) struct TrimJob {
    target: TrimTarget,
    result: std::sync::mpsc::Receiver<Result<Option<super::super::wav::Trimmed>, String>>,
}

impl App {
    pub(super) fn sample_file_busy(&self, path: &std::path::Path) -> bool {
        self.sample_take.is_some()
            || self.pending_sample.is_some()
            || self.active_recording_id.is_some()
            || self.pending_recording.is_some()
            || self
                .snapshot
                .as_ref()
                .is_some_and(|s| s.recording.is_some())
            || self.trim_jobs.iter().any(|job| match &job.target {
                TrimTarget::Take { path: target, .. } | TrimTarget::Sample { path: target, .. } => {
                    target == path
                        || match (target.canonicalize(), path.canonicalize()) {
                            (Ok(target), Ok(path)) => target == path,
                            _ => false,
                        }
                }
            })
    }

    /// ⌘⇧R / Ctrl+Shift+R: start a take, or close the one in progress.
    /// Every start is a new file - take one, take two - in the configured
    /// recordings folder.
    pub(super) fn toggle_take(&mut self) {
        if self.sample_take.is_some() || self.pending_sample.is_some() {
            let finish = self
                .keybinds
                .hint(super::super::keybinds::BindAction::RecordSample);
            self.set_error(
                ErrorOwner::Interface,
                if finish.is_empty() {
                    "a sample is recording - finish it before recording a take".to_owned()
                } else {
                    format!("a sample is recording - finish it ({finish}) before recording a take")
                },
            );
            return;
        }
        if self.pending_recording.is_some() {
            self.status = "engine busy - press again".into();
            self.dirty_frame = true;
            return;
        }
        if self.active_recording_id.is_some()
            || self
                .snapshot
                .as_ref()
                .is_some_and(|s| s.recording.is_some())
        {
            if let Some(request_id) = self.worker.try_record(None) {
                self.pending_recording = Some(request_id);
                self.status = "closing take…".into();
            } else {
                self.status = "engine busy - press again".into();
            }
            self.dirty_frame = true;
            return;
        }
        let directory = self.recordings_directory();
        let seconds = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|since| since.as_secs() as i64)
            .unwrap_or(0);
        let path = directory.join(take_filename(seconds, None));
        if let Err(error) = std::fs::create_dir_all(&directory) {
            self.notice_take_ended(&error.to_string());
            self.status = format!("cannot record into {}: {error}", directory.display());
            self.dirty_frame = true;
            return;
        }
        if let Some(request_id) = self.worker.try_record(Some(path.clone())) {
            self.pending_recording = Some(request_id);
            self.reported_take_error = None;
            self.status = format!(
                "● recording - {} · s(\"{}\") once finished",
                status_file_path(&path, self.ui_settings.show_full_paths),
                recording_sound(&path)
            );
            self.log.push(
                LogLevel::Info,
                "take",
                format!("started {}", path.display()),
            );
            self.record_control(serde_json::json!({
                "take": { "started": path.file_name().map(|n| n.to_string_lossy().into_owned()) }
            }));
        } else {
            self.status = "engine busy - press again".into();
        }
        self.dirty_frame = true;
    }

    /// ^H: start recording a sample from the audio input, or finish the one
    /// recording. It lands in the recordings folder like a take, and is in
    /// the recordings bank to play the moment it is saved - sing a line,
    /// press again, `s("recordings:3")` it.
    ///
    /// A take and a sample are one recorder to the player: while one runs
    /// the other is refused, and the footer says which to finish.
    pub(super) fn toggle_sample(&mut self) {
        if self.pending_sample.is_some() {
            self.status = "engine busy - press again".into();
            self.dirty_frame = true;
            return;
        }
        let take_running = self.pending_recording.is_some()
            || self.active_recording_id.is_some()
            || self
                .snapshot
                .as_ref()
                .is_some_and(|snapshot| snapshot.recording.is_some());
        if take_running {
            let finish = self
                .keybinds
                .hint(super::super::keybinds::BindAction::RecordTake);
            self.set_error(
                ErrorOwner::Interface,
                if finish.is_empty() {
                    "a take is recording - finish it before recording a sample".to_owned()
                } else {
                    format!("a take is recording - finish it ({finish}) before recording a sample")
                },
            );
            return;
        }
        if self.sample_take.is_some() {
            match self.worker.try_record_sample(None) {
                Some(request_id) => {
                    self.pending_sample = Some(request_id);
                    self.status = "finishing the sample…".into();
                }
                None => self.status = "engine busy - press again".into(),
            }
            self.dirty_frame = true;
            return;
        }
        let directory = self.recordings_directory();
        if let Err(error) = std::fs::create_dir_all(&directory) {
            self.set_error(
                ErrorOwner::Interface,
                format!("cannot record into {}: {error}", directory.display()),
            );
            return;
        }
        let seconds = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|since| since.as_secs() as i64)
            .unwrap_or(0);
        let path = directory.join(take_filename(seconds, Some(std::path::Path::new("sample"))));
        match self.worker.try_record_sample(Some(path.clone())) {
            Some(request_id) => {
                self.pending_sample = Some(request_id);
                self.sample_take = Some(SampleTake {
                    started: Instant::now(),
                    path,
                    shown_second: 0,
                });
                self.status = "● recording a sample…".into();
            }
            None => self.status = "engine busy - press again".into(),
        }
        self.dirty_frame = true;
    }

    pub(super) fn handle_sample_recording(&mut self, outcome: SampleRecordingOutcome) {
        if self.pending_sample == Some(outcome.request_id) {
            self.pending_sample = None;
        }
        match outcome.result {
            Ok(SampleReply::Started) => {
                if let Some(take) = self.sample_take.as_mut() {
                    // The clock counts from when the engine took it.
                    take.started = Instant::now();
                    self.log.push(
                        LogLevel::Info,
                        "take",
                        format!("sample started {}", take.path.display()),
                    );
                    self.status = format!(
                        "● recording a sample - {} · {} once finished",
                        status_file_path(&take.path, self.ui_settings.show_full_paths),
                        recording_sound(&take.path)
                    );
                }
            }
            Ok(SampleReply::Finished(status)) => {
                self.sample_take = None;
                if let Some(error) = &status.error {
                    // The finishing press left "finishing the sample…" on
                    // the status line; beside this error it would read as
                    // a finish still under way.
                    self.status = "sample not saved".into();
                    self.set_error(ErrorOwner::Interface, format!("sample not saved: {error}"));
                } else if status.is_silent() {
                    let _ = std::fs::remove_file(&status.path);
                    self.log.push(
                        LogLevel::Info,
                        "take",
                        format!(
                            "sample discarded - silence ({})",
                            format_take_length(status.seconds())
                        ),
                    );
                    self.status = "sample not saved - the input was silent".into();
                } else {
                    self.log.push(
                        LogLevel::Info,
                        "take",
                        format!(
                            "sample saved {} ({})",
                            status.path.display(),
                            format_take_length(status.seconds())
                        ),
                    );
                    self.remember_status_file(status.path.clone());
                    if self.ui_settings.trim_recordings {
                        self.start_trim(TrimTarget::Take {
                            path: status.path.clone(),
                            sample: true,
                        });
                        self.status = "sample saved - trimming silence…".into();
                    } else {
                        self.finish_saved_take(&status.path, "", true);
                    }
                }
            }
            Ok(SampleReply::NotRecording) => {
                self.sample_take = None;
                self.status = "no sample is recording".into();
            }
            Err(error) => {
                // Refused at the start: nothing is recording, and the
                // status must stop saying so. The press put "● recording a
                // sample…" there before the engine answered, and that line
                // beside the refusal reads as a sample still running.
                self.sample_take = None;
                self.status = "no sample is recording".into();
                self.set_error(
                    ErrorOwner::Interface,
                    format!("could not record a sample: {}", error.message),
                );
            }
        }
        self.dirty_frame = true;
    }

    /// A take whose disk said no is closed and reported, once.
    pub(super) fn watch_take(&mut self) {
        // Wait for the start reply before interpreting a recording snapshot.
        // The held snapshot may still describe the preceding take.
        if self.active_recording_id.is_none() {
            return;
        }
        let Some(error) = self
            .snapshot
            .as_ref()
            .and_then(|s| s.recording.as_ref())
            .and_then(|recording| recording.error.clone())
        else {
            return;
        };
        if self.reported_take_error.as_ref() != Some(&error) {
            self.reported_take_error = Some(error.clone());
            self.log
                .push(LogLevel::Error, "take", format!("ended: {error}"));
            // Quietly: this screen may be facing an audience.
            self.notice_take_ended(&error);
            self.status = format!("take ended: {error}");
            self.dirty_frame = true;
        }
        if self.pending_recording.is_none() {
            self.pending_recording = self.worker.try_record(None);
        }
    }

    pub(super) fn handle_recording(&mut self, outcome: RecordingOutcome) {
        let requested = outcome
            .request_id
            .is_some_and(|id| self.pending_recording == Some(id));
        if requested {
            self.pending_recording = None;
        }
        match &outcome.result {
            Ok(RecordingReply::Started { capture_id }) if requested => {
                self.active_recording_id = Some(*capture_id);
                // A queued snapshot may still describe the previous take.
                if let Some(snapshot) = self.snapshot.as_mut() {
                    snapshot.recording = None;
                }
            }
            Ok(RecordingReply::Finished { capture_id, status }) => {
                if self.last_finished_take.as_ref().is_some_and(|previous| {
                    matches!(&previous.result,
                        Ok(RecordingReply::Finished { capture_id: previous, .. })
                        if previous >= capture_id)
                }) {
                    return;
                }
                if self.active_recording_id == Some(*capture_id)
                    || requested && self.active_recording_id.is_none()
                {
                    self.active_recording_id = None;
                    if let Some(snapshot) = self.snapshot.as_mut() {
                        snapshot.recording = None;
                    }
                    if let Some(error) = &status.error {
                        if self.reported_take_error.as_ref() != Some(error) {
                            self.reported_take_error = Some(error.clone());
                            self.log
                                .push(LogLevel::Error, "take", format!("ended: {error}"));
                            self.notice_take_ended(error);
                            self.status = format!("take ended: {error}");
                        }
                    } else if status.is_silent() {
                        match std::fs::remove_file(&status.path) {
                            Ok(()) => {}
                            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                            Err(error) => self.log.push(
                                LogLevel::Warn,
                                "take",
                                format!(
                                    "silent take could not be removed {}: {error}",
                                    status.path.display()
                                ),
                            ),
                        }
                        self.log.push(
                            LogLevel::Info,
                            "take",
                            format!(
                                "discarded silence ({})",
                                format_take_length(status.seconds())
                            ),
                        );
                        self.status = "take not saved - silence".into();
                    } else {
                        self.log.push(
                            LogLevel::Info,
                            "take",
                            format!(
                                "saved {} ({})",
                                status.path.display(),
                                format_take_length(status.seconds())
                            ),
                        );
                        self.remember_status_file(status.path.clone());
                        self.record_control(serde_json::json!({
                            "take": { "ended": status.path.file_name().map(|n| n.to_string_lossy().into_owned()) }
                        }));
                        if self.ui_settings.trim_recordings {
                            self.start_trim(TrimTarget::Take {
                                sample: false,
                                path: status.path.clone(),
                            });
                            self.status = format!(
                                "take saved - {} · trimming silence…",
                                status
                                    .path
                                    .file_name()
                                    .map(|name| name.to_string_lossy().into_owned())
                                    .unwrap_or_else(|| "take".to_owned())
                            );
                        } else {
                            self.finish_saved_take(&status.path, "", false);
                        }
                    }
                }
                self.last_finished_take = Some(outcome);
            }
            Ok(RecordingReply::NoActiveTake) if requested => {
                self.active_recording_id = None;
                if let Some(snapshot) = self.snapshot.as_mut() {
                    snapshot.recording = None;
                }
                if self.reported_take_error.is_none() {
                    self.status = "no take is recording".into();
                }
            }
            Err(error) if requested => self.handle_control(StudioControlEvent::Diagnostic(
                StudioDiagnostic::message("record", format!("could not record: {}", error.message)),
            )),
            _ => {}
        }
        self.dirty_frame = true;
    }

    pub(super) fn start_trim(&mut self, target: TrimTarget) {
        let path = match &target {
            TrimTarget::Take { path, .. } | TrimTarget::Sample { path, .. } => path.clone(),
        };
        let (tx, rx) = std::sync::mpsc::channel();
        if std::thread::Builder::new()
            .name("wav-silence-trim".into())
            .spawn(move || {
                let _ = tx.send(super::super::wav::trim_silence(&path));
            })
            .is_ok()
        {
            self.trim_jobs.push(TrimJob { target, result: rx });
        } else {
            self.finish_trim(target, Err("could not start the trim worker".into()));
        }
    }

    pub(super) fn poll_trim_jobs(&mut self) {
        let mut finished = Vec::new();
        let mut at = 0;
        while at < self.trim_jobs.len() {
            let result = match self.trim_jobs[at].result.try_recv() {
                Ok(result) => Some(result),
                Err(TryRecvError::Disconnected) => {
                    Some(Err("the trim worker ended without a result".into()))
                }
                Err(TryRecvError::Empty) => None,
            };
            if let Some(result) = result {
                let job = self.trim_jobs.swap_remove(at);
                finished.push((job.target, result));
            } else {
                at += 1;
            }
        }
        for (target, result) in finished {
            self.finish_trim(target, result);
        }
    }

    fn finish_trim(
        &mut self,
        target: TrimTarget,
        result: Result<Option<super::super::wav::Trimmed>, String>,
    ) {
        match target {
            TrimTarget::Take { path, sample } => {
                let note = match result {
                    Ok(Some(trimmed)) => format!(
                        " · trimmed {} frame(s)",
                        trimmed.frames_before + trimmed.frames_after
                    ),
                    Ok(None) => String::new(),
                    Err(error) => {
                        // The untrimmed take is still useful and must remain
                        // registered even if the optional rewrite failed.
                        self.log
                            .push(LogLevel::Warn, "take", format!("not trimmed: {error}"));
                        String::new()
                    }
                };
                self.finish_saved_take(&path, &note, sample);
            }
            TrimTarget::Sample {
                name,
                path,
                decoded,
            } => match result {
                Ok(Some(trimmed)) => {
                    if let Some(library) = self.worker.library() {
                        library.forget_decoded(&decoded);
                    }
                    self.status = format!(
                        "{name} trimmed - {} frame(s) of silence removed",
                        trimmed.frames_before + trimmed.frames_after
                    );
                    self.log.push(
                        LogLevel::Info,
                        "samples",
                        format!(
                            "trimmed {} - {} leading, {} trailing frame(s) cut",
                            path.display(),
                            trimmed.frames_before,
                            trimmed.frames_after
                        ),
                    );
                }
                Ok(None) => self.status = format!("{name} - nothing to trim"),
                Err(error) => self.status = format!("{name}: {error}"),
            },
        }
        self.dirty_frame = true;
    }

    fn finish_saved_take(&mut self, path: &std::path::Path, trim_note: &str, sample: bool) {
        // A take belongs to the recordings bank. Register the directory so
        // subsequent takes become variants of that one bank rather than a
        // permanent source row and bank for every recording.
        // Use the path that actually received this take: the preference can
        // change while a recording is still in progress.
        let recordings_dir = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .map(std::path::Path::to_path_buf)
            .unwrap_or_else(|| self.recordings_directory());
        // A set's own output folder is never a source: the take is kept
        // there, and nothing offers a name that would not play it.
        let set_output = self.set_output_folder(&recordings_dir);
        if let Some(folder) = &set_output {
            self.log.push(
                LogLevel::Info,
                "take",
                format!(
                    "{} kept, not imported as samples: {}",
                    path.display(),
                    super::sets::set_output_reason(folder)
                ),
            );
        } else if self.take_directory_covered(&recordings_dir) {
            self.adopt_global_sources();
        } else {
            let spec = recordings_dir.display().to_string();
            self.add_sample_sources(std::slice::from_ref(&spec));
            self.quiet_settled_source = Some(spec);
        }
        self.refresh_catalogue();
        let file = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| "take".to_owned());
        // The spelling that plays this very file, to type straight into the
        // score: a folder bank lists its files in name order. A sample
        // leads with that name - `recordings:2` - so a long take filename
        // cannot push it off a narrow footer.
        let name = match set_output {
            Some(_) => None,
            None => Some(recording_sound(path)),
        };
        if sample {
            self.last_recorded_sample = name.clone();
            self.status = match &name {
                Some(name) => format!("sample saved - {name} · {file}{trim_note}"),
                None => format!("sample saved - {file}{trim_note}"),
            };
        } else {
            let sound = name
                .as_ref()
                .map(|name| format!(" · s(\"{name}\")"))
                .unwrap_or_default();
            self.status = format!("take saved - {file}{sound}{trim_note}");
        }
        // The status line links to the file with this, its final line,
        // while no newer file has taken the link.
        if self.last_file.as_deref() == Some(path) {
            self.remember_status_file(path.to_path_buf());
        }
    }

    fn notice_take_ended(&mut self, error: &str) {
        self.take_notice = Some((
            format!("take ended · {}", short_disk_reason(error)),
            Instant::now(),
        ));
    }

    pub(super) fn recording_chip(&self) -> Option<RecordingChip> {
        if let Some(take) = self.sample_take.as_ref() {
            return Some(RecordingChip {
                seconds: take.started.elapsed().as_secs_f64(),
                bytes: 0,
                dropped_seconds: 0.0,
                sample: true,
            });
        }
        self.take_chip()
    }

    /// The take of the mix in progress, as the engine reports it.
    pub(super) fn take_chip(&self) -> Option<RecordingChip> {
        let recording = self.snapshot.as_ref()?.recording.as_ref()?;
        Some(RecordingChip {
            seconds: recording.seconds,
            bytes: recording.bytes,
            dropped_seconds: recording.dropped_seconds,
            sample: false,
        })
    }

    /// Where finished audio takes go. Session tapes deliberately keep their
    /// existing per-set `sessions` directory.
    pub(super) fn recordings_directory(&self) -> PathBuf {
        self.prefs.recordings_directory().unwrap_or_else(|| {
            self.scenes
                .directory()
                .parent()
                .map(|parent| parent.join(rustel_runtime::product::RECORDINGS_DIRECTORY_NAME))
                .unwrap_or_else(|| {
                    PathBuf::from(rustel_runtime::product::RECORDINGS_DIRECTORY_NAME)
                })
        })
    }

    /// Whether an enabled sample source already reaches `directory` - the
    /// directory itself, or some ancestor of it - so a fresh file landing
    /// there is already covered and does not need a second source pointing
    /// at the same files. Only a source that is a local folder can answer
    /// yes; a URL or a `github:` pack never covers anything on this disk.
    fn take_directory_covered(&self, directory: &std::path::Path) -> bool {
        let Ok(directory) = directory.canonicalize() else {
            return false;
        };
        self.prefs.sample_sources.iter().any(|source| {
            source.enabled
                && std::path::Path::new(source.spec.trim())
                    .canonicalize()
                    .is_ok_and(|root| directory.starts_with(&root))
        })
    }
}

/// Two words for why the disk said no, for a header an audience can see.
pub(super) fn short_disk_reason(error: &str) -> &'static str {
    let lower = error.to_lowercase();
    if lower.contains("no space") || lower.contains("disk full") || lower.contains("quota") {
        "disk full"
    } else if lower.contains("permission") || lower.contains("access is denied") {
        "no permission"
    } else if lower.contains("read-only") {
        "read-only disk"
    } else {
        "disk error"
    }
}

/// `12:34` for a take's length.
pub(super) fn format_take_length(seconds: f64) -> String {
    let total = seconds.max(0.0) as u64;
    if total >= 3600 {
        format!(
            "{}:{:02}:{:02}",
            total / 3600,
            (total / 60) % 60,
            total % 60
        )
    } else {
        format!("{}:{:02}", total / 60, total % 60)
    }
}
