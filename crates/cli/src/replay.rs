use super::*;

/// The `replay_save` event live replay emits for a save the tape marks
/// rejected.
fn replay_save_event(save: &rustel_runtime::session_log::SessionSave) -> serde_json::Value {
    serde_json::json!({
        "replay_save": {
            "t": (save.at * 1000.0).round() / 1000.0,
            "recorded_status": save.status,
        }
    })
}

/// Replay a recorded set from `from`. Live replay writes the plan's opening
/// and then its later saves into a watched file on the tape's clock, and plays
/// them through the `--watch` path. `--export` renders the same plan offline
/// with [`Session::render_session`].
pub(super) fn run_replay(
    session_path: &std::path::Path,
    run: ReplayRun,
    verbosity: u8,
    dispatch: rustel_audio::DspDispatch,
) -> Result<(), RuntimeError> {
    use rustel_runtime::session_log::SessionScript;

    let ReplayRun {
        out,
        speed,
        from,
        export,
        export_format,
        duration,
        follow,
        score_events,
        sample_access,
    } = run;
    let output = LiveOutput::new(verbosity, false, json_asked());

    if !(speed.is_finite() && speed > 0.0) {
        return Err(RuntimeError::Message(format!(
            "replay speed must be a positive number, got {speed}"
        )));
    }
    let from = if from.is_finite() && from > 0.0 {
        from
    } else {
        0.0
    };
    // Validate the export path before any work or output. The extension
    // selects the container unless `--format` names one; a mismatch is
    // refused.
    let mut export_mp3 = false;
    if let Some(export) = &export {
        let extension = export
            .extension()
            .map(|ext| ext.to_string_lossy().to_ascii_lowercase())
            .unwrap_or_default();
        export_mp3 = match (export_format, extension.as_str()) {
            (Some(ReplayExportFormat::Mp3), "mp3") | (None, "mp3") => true,
            (Some(ReplayExportFormat::Wav), "wav") | (None, "wav") => false,
            (Some(chosen), _) => {
                return Err(RuntimeError::Message(format!(
                    "replay --export --format {} does not match `{}`; use a .{} path",
                    if chosen == ReplayExportFormat::Mp3 {
                        "mp3"
                    } else {
                        "wav"
                    },
                    export.display(),
                    if chosen == ReplayExportFormat::Mp3 {
                        "mp3"
                    } else {
                        "wav"
                    },
                )));
            }
            (None, _) => {
                return Err(RuntimeError::Message(format!(
                    "replay --export writes WAV or mp3; `{}` is neither. Use a .wav or .mp3 path, \
                     or pass --format.",
                    export.display()
                )));
            }
        };
    }
    let script = SessionScript::load(session_path).map_err(RuntimeError::Message)?;
    let plan = ReplayPlan::new(&script, from);
    // An export needs something that installed. Live replay still plays a
    // tape of rejected saves.
    if export.is_some() && !plan.any_installed() {
        return Err(RuntimeError::Message(format!(
            "{} holds no installed saves: nothing on it ever sounded, so there is nothing to bounce",
            session_path.display()
        )));
    }
    // Live replay waits `offset / speed` before each later save, so the longest
    // wait goes through the bound every other window does, before a device or
    // scratch file is touched. `--export` ignores `--speed` and is not bounded
    // here.
    if export.is_none() {
        let span = plan.later.iter().map(|cue| cue.offset).fold(0.0, f64::max);
        parse_positive_seconds(
            "replay length (last save after --from / --speed)",
            span / speed,
        )?;
    }
    // Replay drives the ordinary watch path, so it needs a file to write the
    // score into - but that is an implementation detail and does not belong in
    // the artist's directory next to the tape. `--out` is how you ask for it
    // somewhere you can watch.
    let (scratch, target) = match &out {
        Some(path) => (None, path.clone()),
        None => {
            let scratch = private_scratch_dir().map_err(|error| {
                RuntimeError::Message(format!("cannot create a replay folder: {error}"))
            })?;
            let target = scratch.join("replay.strudel");
            (Some(scratch), target)
        }
    };

    std::fs::write(&target, plan.watched_opening()).map_err(|error| {
        RuntimeError::Message(format!("cannot write {}: {error}", target.display()))
    })?;
    let total_saves = script.saves.len();
    // Only a sounding opening is announced.
    if export.is_none()
        && (score_events || follow)
        && let ReplayOpening::Sounding { index, save } = &plan.opening
    {
        announce_active_score(
            index + 1,
            Some(total_saves),
            save.at,
            &save.source,
            score_events,
            follow,
        );
    }

    // An export is a file, not a performance to watch: the score path, the
    // speed and the invitation to open an editor are all noise in front of it.
    if export.is_none() {
        output.event(
            LiveDetail::Essential,
            serde_json::json!({
                "replay": {
                    "session": session_path.display().to_string(),
                    "score": target.display().to_string(),
                    "saves": script.saves.len(),
                    "duration_secs": (script.duration() * 1000.0).round() / 1000.0,
                    "speed": speed,
                    "from_secs": from,
                    "keeps": script.keeps,
                    "message": "open the score file in an editor to watch the set being typed",
                }
            }),
        );
    }

    // A replay knows the whole set in advance, so it can warm every sound
    // before the set starts. Every save on the tape contributes its names,
    // including saves that did not parse.
    let mut sounds: Vec<String> = Vec::new();
    for save in &script.saves {
        for name in rustel_runtime::sounds::in_score(&save.source) {
            if !sounds.contains(&name) {
                sounds.push(name);
            }
        }
    }

    // An export renders the plan's timeline offline in one pass, with no
    // audio device; see [`ReplayPlan::export_timeline`].
    if let Some(export) = &export {
        let mut session = Session::with_config(with_sample_access(
            session_config(dispatch)
                .with_cps(script.baseline_cps.unwrap_or(SessionConfig::default().cps)),
            &sample_access,
        )?)?;
        // Ctrl-C has to reach a render that runs for minutes. The watcher keeps
        // re-asserting the stop, which is what makes it stick across windows
        // that each start the transport again.
        let _watcher = watch_for_interrupt(session.transport());
        if let Err(error) = session.enable_default_samples() {
            notice(
                serde_json::json!({
                    "sample_library": { "status": "unavailable", "message": error.to_string() }
                }),
                || format!("warning: the sample library is unavailable - {error}"),
            );
        }
        session.prefetch_sounds(&sounds);
        let (_, timed_out) = session.wait_for_sample_loads(std::time::Duration::from_secs(60));
        let until = match duration {
            Some(secs) if secs > 0.0 && secs.is_finite() => Some(secs),
            Some(secs) => {
                return Err(RuntimeError::Message(format!(
                    "--duration must be a positive number of seconds, got {secs}"
                )));
            }
            None => None,
        };
        let timeline = plan.export_timeline(until);
        // A long bounce is otherwise a minute of silence with no sign it is
        // working. Say what is about to happen before it happens.
        let set_secs = Session::render_session_secs(&timeline, REPLAY_EXPORT_TAIL_SECS, until);
        let set_secs = (set_secs * 10.0).round() / 10.0;
        notice(
            serde_json::json!({
                "replay_export": {
                    "path": export.display().to_string(),
                    "saves": timeline.len(),
                    "set_secs": set_secs,
                    "message": "rendering offline; no device, faster than real time",
                }
            }),
            || {
                format!(
                    "replaying {} {}, {set_secs} s of set, into {} - offline, faster than real time",
                    timeline.len(),
                    if timeline.len() == 1 { "save" } else { "saves" },
                    export.display()
                )
            },
        );
        let started = std::time::Instant::now();
        let report = session.render_session(
            &timeline,
            REPLAY_EXPORT_TAIL_SECS,
            until,
            export,
            export_mp3,
        )?;
        let elapsed = started.elapsed();
        let seconds_rendered = (report.duration_secs * 10.0).round() / 10.0;
        let took_secs = (elapsed.as_secs_f64() * 10.0).round() / 10.0;
        let faster_than_realtime =
            ((report.duration_secs / elapsed.as_secs_f64().max(1e-9)) * 10.0).round() / 10.0;
        notice(
            serde_json::json!({
                "replay_export": {
                    "path": export.display().to_string(),
                    "seconds_rendered": seconds_rendered,
                    "took_secs": took_secs,
                    "faster_than_realtime": faster_than_realtime,
                    "onsets": report.onset_count,
                    "samples_incomplete": timed_out,
                    "message": "offline bounce",
                }
            }),
            || {
                format!(
                    "wrote {} - {seconds_rendered} s in {took_secs} s ({faster_than_realtime}× real time), {} onsets{}",
                    export.display(),
                    report.onset_count,
                    if timed_out {
                        "; some samples never arrived"
                    } else {
                        ""
                    }
                )
            },
        );
        remove_scratch(scratch.as_deref());
        // Written, but not the score: see `RenderReport::failure`.
        if let Some(message) = report.failure() {
            return Err(RuntimeError::Message(message));
        }
        return Ok(());
    }

    // A rejected save held at a silent opening is reported, not written.
    if let ReplayOpening::Silent { held: Some(save) } = &plan.opening {
        output.event(LiveDetail::Essential, replay_save_event(save));
    }
    // The plan's later saves are delivered by a side thread while the
    // ordinary watch loop owns the audio device on the main thread. Its clock
    // starts once the watch loop anchors cycle zero.
    let first_install_from_zero = plan.first_install_from_zero();
    let (anchored, anchor_signal) = std::sync::mpsc::channel();
    let later = plan.later;
    if !later.is_empty() {
        let target = target.clone();
        std::thread::spawn(move || {
            if anchor_signal.recv().is_err() {
                return;
            }
            let started = std::time::Instant::now();
            for cue in later {
                // The length check above refuses a longer wait; the clamp keeps
                // `from_secs_f64` from panicking on its own.
                let due = std::time::Duration::from_secs_f64(
                    (cue.offset / speed).clamp(0.0, MAX_DURATION_SECS),
                );
                if let Some(wait) = due.checked_sub(started.elapsed()) {
                    std::thread::sleep(wait);
                }
                // Same atomic swap a text editor performs, so the watcher sees
                // whole files rather than half-written ones.
                let tmp = target.with_extension("rustel.replay-tmp");
                if std::fs::write(&tmp, cue.save.source.as_bytes()).is_ok() {
                    let _ = std::fs::rename(&tmp, &target);
                }
                // A save the tape marks installed is announced; one it marks
                // rejected is reported.
                if cue.save.installed() {
                    if score_events || follow {
                        announce_active_score(
                            cue.index + 1,
                            Some(total_saves),
                            cue.save.at,
                            &cue.save.source,
                            score_events,
                            follow,
                        );
                    }
                } else {
                    output.event(LiveDetail::Essential, replay_save_event(&cue.save));
                }
            }
        });
    }

    let outcome = run_musician(
        MusicianArgs {
            file: Some(target),
            prebake: None,
            preload_sounds: sounds,
            first_install_from_zero,
            anchored: Some(anchored),
            // A replay reproduces a recorded set; it does not publish ports,
            // and its clock is the tape's rather than a hardware one.
            midi_virtual: Vec::new(),
            midi_clock_out: None,
            midi_clock_in: None,
            // A replay plays a tape back; it does not listen to anything.
            audio_input: None,
            // A replay carries no device override; the tape sounds the same
            // at any buffer size.
            buffer_frames: None,
            watch: true,
            export: None,
            cycles: None,
            duration: None,
            until_silence: false,
            silence_floor: None,
            silence_hold: None,
            cps: script.baseline_cps.unwrap_or(0.5),
            sample_rate: None,
            sample_access,
            // A replay is a playback of a tape, not a new performance: recording
            // it would fill the folder with copies of what is already there.
            save_session: None,
            no_save_session: true,
            session_file: None,
            announce_score: false,
            // Replay announces its own states on the tape's clock, before the
            // engine installs them; a second announcement per install would draw
            // every state twice.
            follow: false,
            ui_events: false,
            // The JSON switch is one global, decided in `run` from the `replay`
            // command's own flag; this synthesized line is never re-read.
            json: false,
        },
        verbosity,
        dispatch,
    );
    // The tape is the artifact; everything replay needed to run is not.
    remove_scratch(scratch.as_deref());
    outcome
}

/// Make a new private folder for the watched score in the temporary
/// directory. Creation fails on a path that exists, so another user cannot
/// own the folder or put a link in it first.
fn private_scratch_dir() -> std::io::Result<PathBuf> {
    let base = std::env::temp_dir();
    let mut taken = None;
    for attempt in 0..16u32 {
        let name = match attempt {
            0 => format!("rustel-replay-{}", std::process::id()),
            _ => format!("rustel-replay-{}-{attempt}", std::process::id()),
        };
        let path = base.join(name);
        #[cfg(unix)]
        let builder = {
            let mut builder = std::fs::DirBuilder::new();
            std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
            builder
        };
        #[cfg(not(unix))]
        let builder = std::fs::DirBuilder::new();
        match builder.create(&path) {
            Ok(()) => return Ok(path),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => taken = Some(error),
            Err(error) => return Err(error),
        }
    }
    Err(taken.unwrap_or_else(|| std::io::Error::other("no free folder name")))
}

fn remove_scratch(scratch: Option<&std::path::Path>) {
    if let Some(scratch) = scratch {
        let _ = std::fs::remove_dir_all(scratch);
    }
}

/// Announce the code that is sounding right now.
///
/// The terminal draws the event under `--follow`. A desktop or web editor
/// pane reads the same `score_active` line over its own transport. This is
/// the only protocol for following a set live, and there is no file to poll.
pub(super) fn announce_active_score(
    index: usize,
    total: Option<usize>,
    at: f64,
    source: &str,
    emit_event: bool,
    follow: bool,
) {
    if emit_event {
        let mut event = serde_json::json!({
            "t": (at * 1000.0).round() / 1000.0,
            "index": index,
            "lines": source.lines().filter(|line| !line.trim().is_empty()).count(),
            "source": rustel_runtime::session_log::encode_base64(source.as_bytes()),
        });
        // A live set has no last save yet; only a tape knows how many there are.
        if let Some(total) = total {
            event["of"] = serde_json::json!(total);
        }
        eprintln!("{}", serde_json::json!({ "score_active": event }));
    }
    if follow {
        render_active_score(index, total, at, source);
    }
}

/// Draw the code that is sounding, in place.
///
/// The one renderer: `--follow` calls it directly and `watch-code` calls it
/// from the other end of a pipe, so the two ergonomics cannot drift apart.
pub(super) fn render_active_score(index: usize, total: Option<usize>, at: f64, source: &str) {
    use std::io::Write as _;

    let counter = match total {
        Some(total) => format!("save {index}/{total}"),
        None => format!("save {index}"),
    };
    // Home the cursor and clear, so the set redraws in place instead of
    // scrolling: the point is to sit back and watch one screen.
    if style::stdout_on() {
        print!("\x1b[H\x1b[2J");
    }
    println!("── {at:>8.2}s   {counter} ──\n");
    println!("{}", style::safe_source(source.trim_end()));
    let _ = std::io::stdout().flush();
}
