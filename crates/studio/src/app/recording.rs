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

#[cfg(test)]
mod tests {
    mod recording {
        //! Tests for `app/recording.rs`.

        use crate::app::recording::*;
        use crate::app::tests::support::*;
        use crate::engine::{RecordingInfo, StudioStop};
        use crate::wav::{RawSignalSummary, TakeStatus};

        /// A take and a sample are one recorder to the player: pressing one
        /// while the other runs says, in the footer, which to finish first -
        /// and starts nothing.
        #[test]
        fn a_take_and_a_sample_refuse_each_other_in_the_footer() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = app_over(directory.path());

            app.active_recording_id = Some(7);
            app.toggle_sample();
            assert!(app.sample_take.is_none(), "no sample started under a take");
            assert!(app.pending_sample.is_none());
            let shown = app.errors.visible().expect("a footer error").to_owned();
            assert!(
                shown.starts_with("a take is recording - finish it (^\u{21e7}R)"),
                "{shown}"
            );

            app.active_recording_id = None;
            app.sample_take = Some(SampleTake {
                started: Instant::now(),
                path: directory.path().join("sample.wav"),
                shown_second: 0,
            });
            app.toggle_take();
            assert!(
                app.pending_recording.is_none(),
                "no take started under a sample"
            );
            let shown = app.errors.visible().expect("a footer error").to_owned();
            assert!(
                shown.starts_with("a sample is recording - finish it (^H)"),
                "{shown}"
            );
        }

        /// The chord the table shows for recording a sample is the chord that
        /// records one. It had no handler of its own, so the key the menu
        /// promised did nothing in any terminal.
        #[test]
        fn the_record_sample_chord_starts_a_sample() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = app_over(directory.path());
            let chord = app
                .keybinds
                .binding(BindAction::RecordSample)
                .expect("record a sample has a chord");
            let mut modifiers = KeyModifiers::NONE;
            if chord.control {
                modifiers |= KeyModifiers::CONTROL;
            }
            if chord.shift {
                modifiers |= KeyModifiers::SHIFT;
            }
            press(&mut app, chord.code, modifiers);
            assert!(
                app.pending_sample.is_some(),
                "the press reached the recorder: {}",
                app.status
            );
            assert!(
                app.sample_take.is_some(),
                "a sample is on its way: {}",
                app.status
            );
            assert_eq!(app.status, "● recording a sample…");

            // The fixture opens no default input, so the engine's answer is the
            // refusal a machine with no input gives - and it leaves nothing open.
            let deadline = Instant::now() + Duration::from_secs(5);
            while app.pending_sample.is_some() && Instant::now() < deadline {
                while let Ok(event) = app.worker.try_recv_control() {
                    app.handle_control(event);
                }
                std::thread::sleep(Duration::from_millis(5));
            }
            assert!(app.pending_sample.is_none(), "the engine answered");
            assert!(app.sample_take.is_none(), "a refused start leaves no take");
            assert_eq!(app.status, "no sample is recording");
            assert_eq!(
                app.errors.visible(),
                Some(
                    "could not record a sample: no audio input to record from: \
             this studio opens no default input"
                )
            );
        }

        /// A sample whose writer failed on the way out says so twice over: the
        /// error line has the reason, and the status stops saying "finishing the
        /// sample…", which beside that error would read as a finish still under
        /// way.
        #[test]
        fn a_sample_that_fails_to_save_stops_saying_it_is_finishing() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = app_over(directory.path());
            let path = directory.path().join("sample.wav");
            app.sample_take = Some(SampleTake {
                started: Instant::now(),
                path: path.clone(),
                shown_second: 0,
            });
            app.toggle_sample();
            let request_id = app.pending_sample.expect("the finish went to the engine");
            assert_eq!(app.status, "finishing the sample…");

            let mut failed = status(&path);
            failed.error = Some("No space left on device (os error 28)".into());
            app.handle_control(StudioControlEvent::SampleRecording(
                SampleRecordingOutcome {
                    request_id,
                    result: Ok(SampleReply::Finished(failed)),
                },
            ));
            assert_eq!(app.pending_sample, None);
            assert!(app.sample_take.is_none());
            assert_eq!(app.status, "sample not saved");
            assert_eq!(
                app.errors.visible(),
                Some("sample not saved: No space left on device (os error 28)")
            );
        }

        /// A finished sample names the bank the player types - `recordings:2` -
        /// first in the footer, before the long filename that would otherwise
        /// push it off a narrow row. That name is what ^J pastes.
        #[test]
        fn a_saved_sample_names_its_bank_first_in_the_footer() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = app_over(directory.path());
            app.ui_settings.trim_recordings = false;
            let recordings = directory.path().join("recordings");
            std::fs::create_dir_all(&recordings).expect("recordings folder");
            app.prefs.recordings_directory = Some(recordings.display().to_string());
            for name in ["sample-a.wav", "sample-b.wav"] {
                std::fs::write(recordings.join(name), b"").expect("existing take");
            }
            let path = recordings.join("sample-c.wav");
            std::fs::write(&path, b"").expect("the new sample");
            app.sample_take = Some(SampleTake {
                started: Instant::now(),
                path: path.clone(),
                shown_second: 0,
            });
            app.toggle_sample();
            let request_id = app.pending_sample.expect("the finish went to the engine");

            app.handle_control(StudioControlEvent::SampleRecording(
                SampleRecordingOutcome {
                    request_id,
                    result: Ok(SampleReply::Finished(status(&path))),
                },
            ));

            assert_eq!(
                app.last_recorded_sample.as_deref(),
                Some("recordings:2"),
                "the latest sample is the one ^J pastes"
            );
            assert!(
                app.status.starts_with("sample saved - recordings:2"),
                "the footer leads with the name to type: {}",
                app.status
            );
            assert!(
                app.status.contains("sample-c.wav"),
                "the file is still named, after the bank: {}",
                app.status
            );
            assert_eq!(
                app.last_file_status.as_ref(),
                Some(&app.status),
                "the status line's path is still a link to the file"
            );
        }

        /// The sample saved names the variant that plays it: a folder bank
        /// lists its audio files in name order, and nothing else counts.
        #[test]
        fn a_saved_sample_is_named_by_the_variant_that_plays_it() {
            let directory = tempfile::tempdir().expect("temp dir");
            for name in [
                "session-2026-09-22T01-00-00.wav",
                "sample-2026-09-22T02-00-00.wav",
                "notes.txt",
                "sample-2026-09-22T01-30-00.wav",
            ] {
                std::fs::write(directory.path().join(name), b"").expect("file");
            }
            std::fs::create_dir(directory.path().join("sample-a-folder")).expect("folder");
            assert_eq!(
                variant_index(
                    directory.path(),
                    &directory.path().join("sample-2026-09-22T02-00-00.wav")
                ),
                Some(1)
            );
            assert_eq!(
                variant_index(
                    directory.path(),
                    &directory.path().join("sample-2026-09-22T03-00-00.wav")
                ),
                Some(2),
                "a recording not written yet already has its place"
            );
            assert_eq!(
                recording_sound(&directory.path().join("sample-2026-09-22T01-30-00.wav")),
                format!(
                    "{}:0",
                    directory.path().file_name().unwrap().to_string_lossy()
                )
            );
        }

        #[test]
        fn a_take_that_ends_on_its_own_is_said_in_two_words() {
            assert_eq!(
                short_disk_reason("No space left on device (os error 28)"),
                "disk full"
            );
            assert_eq!(
                short_disk_reason("Permission denied (os error 13)"),
                "no permission"
            );
            assert_eq!(
                short_disk_reason("Read-only file system (os error 30)"),
                "read-only disk"
            );
            assert_eq!(
                short_disk_reason("Input/output error (os error 5)"),
                "disk error"
            );
            assert_eq!(format_take_length(754.0), "12:34");
        }

        fn status(path: &std::path::Path) -> TakeStatus {
            TakeStatus {
                path: path.to_path_buf(),
                sample_rate: 48_000,
                frames: 48_000,
                bytes: 288_044,
                error: None,
                final_signal: Some(RawSignalSummary {
                    sample_count: 96_000,
                    nonfinite_count: 0,
                    finite_peak: 0.5,
                }),
            }
        }

        fn finished(request_id: u64, capture_id: u64, status: TakeStatus) -> StudioControlEvent {
            StudioControlEvent::Recording(RecordingOutcome {
                request_id: Some(request_id),
                result: Ok(RecordingReply::Finished { capture_id, status }),
            })
        }

        fn recording(path: &std::path::Path, error: Option<String>) -> StudioSnapshot {
            StudioSnapshot {
                recording: Some(RecordingInfo {
                    path: path.to_path_buf(),
                    seconds: 1.0,
                    bytes: 288_044,
                    dropped_seconds: 0.0,
                    error,
                }),
                ..Default::default()
            }
        }

        #[test]
        fn joined_take_is_retained_after_stop_and_only_then_reported_saved() {
            let directory = tempfile::tempdir().unwrap();
            let mut app = app_over(directory.path());
            // Trimming has its own asynchronous test below; this test isolates
            // the recorder's join and final-reply ordering.
            app.ui_settings.trim_recordings = false;
            let status = status(&directory.path().join("take.wav"));
            app.active_recording_id = Some(1);
            app.toggle_take();
            let request_id = app.pending_recording.expect("stop enqueued");
            assert_eq!(app.status, "closing take…");
            assert_eq!(app.last_file, None);
            assert!(app.last_finished_take.is_none());
            app.handle_control(StudioControlEvent::Stopped(Box::new(StudioStop::idle())));
            let outcome = finished(request_id, 1, status.clone());
            app.handle_control(outcome.clone());
            assert_eq!(app.pending_recording, None);
            assert_eq!(app.last_file.as_ref(), Some(&status.path));
            // The status names the bank the player must type, not the file:
            // that is the whole point of registering a take immediately.
            assert!(
                app.status.starts_with("take saved - take.wav · s(\""),
                "{}",
                app.status
            );
            // Registering the take must not land on Settings ▸ Samples the way
            // a dropped folder does: a sheet over the score is the last thing
            // anyone wants the moment they stop recording.
            assert!(
                app.settings_sheet.is_none(),
                "stopping a take opened the settings sheet"
            );
            app.handle_control(StudioControlEvent::Snapshot(Box::default()));
            app.handle_control(outcome);
            assert!(matches!(&app.last_finished_take.as_ref().unwrap().result,
                Ok(RecordingReply::Finished { capture_id: 1, status: received }) if received == &status));
            assert_eq!(
                app.log
                    .lines()
                    .iter()
                    .filter(|line| line.kind == "take" && line.text.starts_with("saved "))
                    .count(),
                1
            );
        }

        #[test]
        fn a_silent_take_is_removed_and_not_registered() {
            let directory = tempfile::tempdir().unwrap();
            let mut app = app_over(directory.path());
            let path = directory.path().join("silent.wav");
            std::fs::write(&path, b"placeholder").unwrap();
            let mut silent = status(&path);
            silent.final_signal.as_mut().unwrap().finite_peak = 0.0;
            app.active_recording_id = Some(1);
            app.pending_recording = Some(2);

            app.handle_control(finished(2, 1, silent));

            assert!(!path.exists(), "silent capture was left on disk");
            assert_eq!(
                app.last_file, None,
                "silent capture was advertised as saved"
            );
            assert_eq!(app.status, "take not saved - silence");
            assert!(
                app.log
                    .lines()
                    .iter()
                    .any(|line| line.kind == "take" && line.text.starts_with("discarded silence"))
            );
        }

        #[test]
        fn older_final_reply_cannot_close_a_newer_take_even_at_the_same_path() {
            let directory = tempfile::tempdir().unwrap();
            let mut app = app_over(directory.path());
            let status = status(&directory.path().join("take.wav"));
            let snapshot = recording(&status.path, None);
            app.snapshot = Some(snapshot.clone());
            app.active_recording_id = Some(3);
            app.pending_recording = Some(4);
            let older = finished(2, 1, status.clone());
            app.handle_control(older.clone());
            assert_eq!(app.snapshot, Some(snapshot));
            assert_eq!(app.active_recording_id, Some(3));
            assert_eq!(app.pending_recording, Some(4));
            assert_eq!(app.last_file, None);
            assert_eq!(app.last_finished_take.as_ref().unwrap().request_id, Some(2));
            app.handle_control(finished(4, 3, status));
            app.handle_control(older);
            assert_eq!(app.last_finished_take.as_ref().unwrap().request_id, Some(4));
            assert_eq!(app.active_recording_id, None);
        }

        #[test]
        fn final_error_details_are_preserved_without_a_saved_notice() {
            let directory = tempfile::tempdir().unwrap();
            let mut app = app_over(directory.path());
            let mut status = status(&directory.path().join("take.wav"));
            app.active_recording_id = Some(1);
            app.pending_recording = Some(2);
            app.reported_take_error = Some("write failed".into());
            status.error = Some("write failed; writer thread panicked".into());
            status.final_signal = None;
            app.handle_control(finished(2, 1, status.clone()));
            assert_eq!(app.last_file, None);
            assert_eq!(
                app.status,
                "take ended: write failed; writer thread panicked"
            );
            assert!(matches!(&app.last_finished_take.as_ref().unwrap().result,
                Ok(RecordingReply::Finished { status: received, .. }) if received == &status));
            // An I/O error can also accompany a complete received-input summary.
            status.final_signal = Some(RawSignalSummary {
                sample_count: 2,
                nonfinite_count: 1,
                finite_peak: 0.5,
            });
            app.reported_take_error = None;
            app.active_recording_id = Some(3);
            app.pending_recording = Some(4);
            app.handle_control(finished(4, 3, status.clone()));
            assert!(matches!(&app.last_finished_take.as_ref().unwrap().result,
                Ok(RecordingReply::Finished { status: received, .. }) if received == &status));
            assert_eq!(
                app.log
                    .lines()
                    .iter()
                    .filter(|line| line.kind == "take")
                    .map(|line| line.repeats)
                    .sum::<u32>(),
                2,
                "both takes' errors are reported, the second as a repeat of the first"
            );
        }

        #[test]
        fn automatic_final_is_not_an_answer_to_an_absent_or_pending_request() {
            let directory = tempfile::tempdir().unwrap();
            let mut app = app_over(directory.path());
            let status = status(&directory.path().join("take.wav"));
            let previous_status = app.status.clone();
            app.handle_control(StudioControlEvent::Recording(RecordingOutcome {
                request_id: None,
                result: Ok(RecordingReply::Finished {
                    capture_id: 1,
                    status: status.clone(),
                }),
            }));
            assert_eq!(app.last_file, None);
            assert_eq!(app.status, previous_status);
            assert!(app.last_finished_take.is_some());

            app.active_recording_id = Some(3);
            app.pending_recording = Some(4);
            app.handle_control(StudioControlEvent::Snapshot(Box::default()));
            assert_eq!(app.active_recording_id, Some(3));
            let final_reply = RecordingOutcome {
                request_id: None,
                result: Ok(RecordingReply::Finished {
                    capture_id: 3,
                    status: status.clone(),
                }),
            };
            app.handle_recording(final_reply.clone());
            app.handle_recording(final_reply);
            assert_eq!(app.pending_recording, Some(4));
            assert_eq!(app.active_recording_id, None);
            assert_eq!(app.last_file, Some(status.path));
            assert_eq!(
                app.log
                    .lines()
                    .iter()
                    .filter(|line| line.kind == "take" && line.text.starts_with("saved "))
                    .count(),
                1
            );
        }

        #[test]
        fn start_reply_discards_the_previous_snapshot_before_watching_errors() {
            let directory = tempfile::tempdir().unwrap();
            let mut app = app_over(directory.path());
            app.snapshot = Some(recording(
                &directory.path().join("take.wav"),
                Some("old error".into()),
            ));
            app.pending_recording = Some(3);
            app.watch_take();
            assert_eq!(app.reported_take_error, None);
            assert_eq!(app.pending_recording, Some(3));
            app.handle_control(StudioControlEvent::Recording(RecordingOutcome {
                request_id: Some(3),
                result: Ok(RecordingReply::Started { capture_id: 3 }),
            }));
            app.watch_take();
            assert_eq!(app.active_recording_id, Some(3));
            assert_eq!(app.pending_recording, None);
            assert_eq!(app.reported_take_error, None);
            assert!(app.snapshot.as_ref().unwrap().recording.is_none());
            // A snapshot can arrive while the closed writer is still finishing.
            app.handle_control(StudioControlEvent::Snapshot(Box::default()));
            assert_eq!(app.active_recording_id, Some(3));
            assert!(app.last_finished_take.is_none());
            app.handle_control(StudioControlEvent::Recording(RecordingOutcome {
                request_id: None,
                result: Ok(RecordingReply::Finished {
                    capture_id: 3,
                    status: status(&directory.path().join("take.wav")),
                }),
            }));
            assert_eq!(app.active_recording_id, None);
            app.pending_recording = Some(4);
            app.handle_control(StudioControlEvent::Recording(RecordingOutcome {
                request_id: Some(4),
                result: Ok(RecordingReply::Started { capture_id: 4 }),
            }));
            assert_eq!(app.active_recording_id, Some(4));
            assert_eq!(app.pending_recording, None);
        }

        /// A finished take registers as a source at once, and the status names
        /// the bank that plays it with `s("<stem>")`. `trim_recordings` (on by
        /// default) trims the file first; off leaves the file as recorded.
        #[test]
        fn a_finished_take_registers_and_the_status_names_its_bank() {
            let directory = tempfile::tempdir().unwrap();
            let mut app = app_over(directory.path());

            let takes = directory.path().join("takes");
            std::fs::create_dir_all(&takes).unwrap();
            app.prefs.recordings_directory = Some(takes.display().to_string());
            let make_take = |name: &str| {
                let path = takes.join(name);
                let mut writer =
                    crate::wav::TakeWriter::start(path.clone(), 48_000).expect("writer");
                assert_eq!(writer.push_padded(Vec::new(), 2_000), Ok(true));
                let loud: Vec<f32> = std::iter::repeat_n([0.6_f32, -0.6_f32], 1_000)
                    .flatten()
                    .collect();
                assert!(writer.push(loud));
                assert_eq!(writer.push_padded(Vec::new(), 2_000), Ok(true));
                let take_status = writer.finish();
                assert_eq!(take_status.error, None);
                path
            };

            // Trimmed by default.
            let path = make_take("one.wav");
            let before = std::fs::read(&path).unwrap();
            app.active_recording_id = Some(1);
            app.pending_recording = Some(2);
            app.handle_control(finished(2, 1, status(&path)));
            let deadline = Instant::now() + Duration::from_secs(5);
            while !app.trim_jobs.is_empty() && Instant::now() < deadline {
                std::thread::yield_now();
                app.poll_trim_jobs();
            }
            assert!(app.trim_jobs.is_empty(), "trim worker did not answer");
            assert!(app.status.contains("s(\"takes:0\")"), "{}", app.status);
            assert!(app.status.contains("trimmed"), "{}", app.status);
            let after = std::fs::read(&path).unwrap();
            assert!(
                after.len() < before.len(),
                "the padded silence either side was cut"
            );
            app.settle_sample_sources();
            assert!(
                app.worker
                    .library()
                    .is_some_and(|library| library.knows("one")),
                "the take is playable at once, not on the next periodic scan"
            );

            // Off: the file the recorder wrote is left exactly as it was.
            app.ui_settings.trim_recordings = false;
            let path2 = make_take("two.wav");
            let before2 = std::fs::read(&path2).unwrap();
            app.active_recording_id = Some(3);
            app.pending_recording = Some(4);
            app.handle_control(finished(4, 3, status(&path2)));
            assert!(app.status.contains("s(\"takes:1\")"), "{}", app.status);
            assert!(!app.status.contains("trimmed"), "{}", app.status);
            let after2 = std::fs::read(&path2).unwrap();
            assert_eq!(
                before2, after2,
                "trim_recordings off leaves the file untouched"
            );
        }

        /// A take that lands in a set's own `sessions/` folder is kept but never
        /// registered as a sample source, and the footer offers no `s("…")` that
        /// would not play it.
        #[test]
        fn a_take_saved_in_a_sets_sessions_folder_is_not_a_sample_source() {
            let directory = tempfile::tempdir().unwrap();
            let mut app = app_over(directory.path());
            app.ui_settings.trim_recordings = false;
            let sessions = app.scenes.sessions_directory();
            std::fs::create_dir_all(&sessions).unwrap();
            app.prefs.recordings_directory = Some(sessions.display().to_string());
            let path = sessions.join("take.wav");
            std::fs::write(&path, b"placeholder").unwrap();
            app.active_recording_id = Some(1);
            app.pending_recording = Some(2);

            app.handle_control(finished(2, 1, status(&path)));

            assert!(path.exists(), "the take is kept");
            assert_eq!(app.last_file.as_ref(), Some(&path));
            assert_eq!(app.status, "take saved - take.wav");
            assert!(
                app.prefs.sample_sources.is_empty(),
                "{:?}",
                app.prefs.sample_sources
            );
            assert!(
                app.log.lines().iter().any(
                    |line| line.kind == "take" && line.text.contains("not imported as samples")
                ),
                "the log says why the take is not a sound"
            );
        }

        #[test]
        fn takes_default_to_recordings_while_session_tapes_stay_with_the_set() {
            let directory = tempfile::tempdir().unwrap();
            let app = app_over(directory.path());

            assert_eq!(
                app.recordings_directory()
                    .file_name()
                    .and_then(|name| name.to_str()),
                Some("recordings")
            );
            assert_ne!(app.recordings_directory(), app.sessions_directory());
            assert_eq!(app.sessions_directory(), app.scenes.sessions_directory());
        }
    }
}
