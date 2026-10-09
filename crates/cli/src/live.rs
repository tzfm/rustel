use super::*;

#[cfg(feature = "device-audio")]
use std::time::Instant;

pub(super) fn run_musician(
    args: MusicianArgs,
    verbosity: u8,
    dispatch: rustel_audio::DspDispatch,
) -> Result<(), RuntimeError> {
    let MusicianArgs {
        file,
        prebake,
        watch,
        export,
        cycles,
        duration,
        until_silence,
        silence_floor,
        silence_hold,
        cps,
        sample_rate,
        sample_access,
        save_session,
        no_save_session,
        session_file,
        follow,
        ui_events,
        announce_score,
        preload_sounds,
        first_install_from_zero,
        anchored,
        midi_virtual,
        midi_clock_out,
        midi_clock_in,
        audio_input,
        buffer_frames,
        // Read in `run`, which stores the one global switch `json_asked()`
        // reports; the field itself has no second reader here.
        json: _,
    } = args;
    // Reject unsupported virtual ports before loading the score or writing a file.
    if !midi_virtual.is_empty() {
        if cfg!(target_os = "windows") {
            return Err(RuntimeError::Message(
                "--midi-virtual is unavailable on Windows; install a loopback driver and use its port name in .midi()".into(),
            ));
        } else if !cfg!(feature = "midi") {
            return Err(RuntimeError::Message(
                "--midi-virtual requires a build with the midi feature".into(),
            ));
        } else if export.is_some() {
            return Err(RuntimeError::Message(
                "--midi-virtual requires live playback and cannot be used with --export".into(),
            ));
        }
    }
    // `--export` writes WAV only: a path with any other extension is refused
    // before any work, and a path with none is written as WAV. The export
    // subcommand writes the other containers.
    if let Some(export) = &export
        && export
            .extension()
            .is_some_and(|extension| !extension.eq_ignore_ascii_case("wav"))
    {
        return Err(RuntimeError::Message(format!(
            "--export writes WAV; `{}` is not a .wav path. Use a .wav path, or \
             `rustel export -o` for mp3 or onset JSON.",
            export.display()
        )));
    }
    // `--json` is the documented way to ask for machine output, and this is
    // the path that plays a score: watching one with it printed "Started
    // foo.strudel" and human lines thereafter. The form was wired to the
    // hidden `--ui-events` alone, so the flag every script reaches for
    // reached everything EXCEPT the live stream it was pointed at.
    let output = LiveOutput::new(verbosity, ui_events, json_asked());
    let input = SourceInput {
        file,
        eval: None,
        sample_access,
    };
    let cps = parse_cps(cps)?;
    if watch {
        // Fail before reading: a watched path must be a rereadable file.
        let _ = watch_file(&input)?;
    }
    let sample_rate = sample_rate.unwrap_or(48_000);
    let mut session = Session::with_config(with_sample_access(
        session_config(dispatch)
            .with_cps(cps)
            .with_sample_rate(sample_rate),
        &input.sample_access,
    )?)?;
    if export.is_none() {
        session.set_direct_diagnostic_logging(output.structured());
    }
    let _watcher = watch_for_interrupt(session.transport());
    let (loaded, initial_error) = if watch {
        load_watch_musician_sources(
            &mut session,
            &input,
            prebake.as_deref(),
            &EVALUATION_CANCELLED,
        )?
    } else {
        (
            load_musician_sources(
                &mut session,
                &input,
                prebake.as_deref(),
                &EVALUATION_CANCELLED,
            )?,
            None,
        )
    };

    if let Some(output) = export {
        // Deterministic scalar export renders through the same sample
        // library as everything else (A/B lanes depend on it).
        if let Err(error) = session.enable_default_samples() {
            notice(
                serde_json::json!({
                    "sample_library": { "status": "unavailable", "message": error.to_string() }
                }),
                || format!("warning: the sample library is unavailable - {error}"),
            );
        }
        let duration = match cycles {
            Some(cycles) => {
                let cycles = parse_positive_seconds("cycles", cycles)?;
                // A successful score-local setcps/setcpm overrides the CLI
                // baseline. `--cycles` describes musical cycles, so derive
                // its wall-clock bounce from the Session's committed tempo,
                // not the value parsed before score evaluation.
                parse_positive_seconds("duration", cycles / session.config().cps)?
            }
            // With --until-silence the duration is only a ceiling, so the
            // two-second default would end every bounce before it began.
            None if until_silence => parse_positive_seconds("duration", duration.unwrap_or(600.0))?,
            None => parse_positive_seconds("duration", duration.unwrap_or(2.0))?,
        };
        if until_silence {
            let floor_db = silence_floor.unwrap_or(-60.0);
            if !floor_db.is_finite() || floor_db > 0.0 {
                return Err(RuntimeError::Message(format!(
                    "--silence-floor is dBFS and must be at or below 0, got {floor_db}"
                )));
            }
            let hold = parse_positive_seconds("silence-hold", silence_hold.unwrap_or(2.0))?;
            let floor = 10f64.powf(floor_db / 20.0) as f32;
            session.stop_export_when_silent(floor, std::time::Duration::from_secs_f64(hold));
            notice(
                serde_json::json!({
                    "export_until_silence": {
                        "floor_dbfs": floor_db,
                        "hold_secs": hold,
                        "ceiling_secs": duration,
                        "message": "stops when the music does; --duration is only the ceiling",
                    }
                }),
                || {
                    format!(
                        "bouncing until the music stays under {floor_db} dBFS for {hold} s; {duration} s is only the ceiling"
                    )
                },
            );
        }
        let report = session.render(duration, &output, RenderFormat::ScalarWav)?;
        if json_asked() || LiveOutput::new(verbosity, ui_events, json_asked()).structured() {
            println!(
                "{}",
                serde_json::to_string_pretty(&report).map_err(json_err)?
            );
        } else {
            let on = style::stdout_on();
            println!(
                "{} {} - {:.1} s, 16-bit stereo WAV at {} Hz, {} onset{}",
                style::green(on, "wrote"),
                style::bold(on, &report.path),
                report.duration_secs,
                report.sample_rate,
                report.onset_count,
                if report.onset_count == 1 { "" } else { "s" },
            );
        }
        // Written, but not the score: see `RenderReport::failure`.
        if let Some(message) = report.failure() {
            return Err(RuntimeError::Message(message));
        }
        return Ok(());
    }

    // Watched sets record themselves. An unwatched one plays a file that never
    // changes, so its "tape" would be the file itself.
    let recording =
        (!no_save_session && watch).then(|| save_session.unwrap_or(SessionModeArg::Normal));
    let recorder = match recording {
        Some(mode) => {
            let path = session_file.unwrap_or_else(|| {
                let seconds = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|since| since.as_secs() as i64)
                    .unwrap_or(0);
                let name = rustel_runtime::session_log::default_session_filename(
                    seconds,
                    input.file.as_deref(),
                );
                session_dir().join(name)
            });
            // Only worth recording when it is not the value every engine
            // starts from; a score that calls `setCpm` sets its own anyway.
            let baseline_cps = (cps != SessionConfig::default().cps).then_some(cps);
            if let Some(parent) = path
                .parent()
                .filter(|parent| !parent.as_os_str().is_empty())
            {
                let _ = std::fs::create_dir_all(parent);
            }
            // A recording that cannot be written must never stop the set. The
            // tape is insurance; the music is the job.
            match rustel_runtime::session_log::SessionRecorder::create(
                path,
                mode.into(),
                baseline_cps,
            ) {
                Ok(recorder) => {
                    output.event(
                        LiveDetail::Verbose,
                        serde_json::json!({
                            "session_recording": {
                                "path": recorder.path().display().to_string(),
                                "keeps": recorder.mode().keeps(),
                                "message": format!(
                                    "replay with: {} replay <file>",
                                    product::COMMAND_NAME
                                ),
                            }
                        }),
                    );
                    Some(recorder)
                }
                Err(error) => {
                    output.event(
                        LiveDetail::Essential,
                        serde_json::json!({
                            "session_recording": {
                                "status": "unavailable",
                                "message": error,
                                "hint": format!(
                                    "set {}, or pass --no-save-session",
                                    product::SESSION_DIRECTORY_ENV
                                ),
                            }
                        }),
                    );
                    None
                }
            }
        }
        None => None,
    };

    let live_duration = duration
        .map(|seconds| parse_positive_seconds("duration", seconds))
        .transpose()?;
    play_live(
        &mut session,
        &input,
        watch,
        live_duration,
        &loaded,
        initial_error,
        recorder,
        preload_sounds,
        follow,
        announce_score || ui_events,
        midi_virtual,
        midi_clock_out,
        midi_clock_in,
        audio_input,
        buffer_frames,
        ui_events,
        first_install_from_zero,
        anchored,
        output,
    )
}

pub(super) fn watch_file(
    input: &SourceInput,
) -> Result<(&std::path::Path, rustel_runtime::WatchLanguage), RuntimeError> {
    let path = input
        .file
        .as_deref()
        .ok_or_else(|| RuntimeError::Message("live playback requires a source file path".into()))?;
    if path.as_os_str() == "-" {
        return Err(RuntimeError::Message(
            "live playback cannot use stdin; provide a source file path".into(),
        ));
    }
    // Starting a set on a file that does not exist yet is the normal way to
    // begin: open the editor on an empty buffer and type. Refusing to launch
    // until the artist has created the file first is friction with no payoff,
    // so create it (and its parent) and watch it from empty.
    if !path.exists() {
        if let Some(parent) = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            std::fs::create_dir_all(parent).map_err(|error| {
                RuntimeError::Message(format!("cannot create {}: {error}", parent.display()))
            })?;
        }
        std::fs::write(path, "").map_err(|error| {
            RuntimeError::Message(format!("cannot create {}: {error}", path.display()))
        })?;
    }
    // A watched score is always a full score: JavaScript, with
    // mini-notation inside quoted strings.
    Ok((path, rustel_runtime::WatchLanguage::JavaScript))
}

#[cfg(feature = "device-audio")]
pub(super) fn build_live_producer(
    path: &std::path::Path,
    language: rustel_runtime::WatchLanguage,
    loaded: &LoadedLiveSources,
    debounce: std::time::Duration,
    watch_poll: std::time::Duration,
    continuation_floor: std::time::Duration,
) -> Result<rustel_runtime::LiveFileProducer, RuntimeError> {
    rustel_runtime::LiveFileProducer::from_loaded_sources_with_prebake_floor(
        path,
        language,
        loaded.score.as_str(),
        loaded.prebake.clone(),
        debounce,
        watch_poll,
        continuation_floor,
    )
}

#[cfg(feature = "device-audio")]
pub(super) fn observe_ui_layout_if_audible(
    delivery: &mut UiLayoutDelivery,
    source: &str,
    session_generation: u64,
    audible_generation: u64,
) -> Result<bool, rustel_runtime::ui_events::UiLayoutValidationError> {
    if session_generation != audible_generation {
        return Ok(false);
    }
    delivery.observe(source, session_generation)
}

#[cfg(feature = "device-audio")]
pub(super) fn read_ui_controls(
    mut reader: impl std::io::Read,
    pending: &std::sync::Mutex<std::collections::BTreeMap<String, UiSliderControl>>,
) {
    let mut chunk = [0u8; 4096];
    let mut line = Vec::with_capacity(1024);
    let mut discarding = false;
    loop {
        let read = loop {
            match reader.read(&mut chunk) {
                Ok(read) => break read,
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) => {
                    // A dead control transport degrades to a set without UI
                    // controls, but never silently: report and stop.
                    eprintln!(
                        "{}",
                        serde_json::json!({
                            "live_error": {
                                "kind": "io",
                                "message": format!(
                                    "UI control input failed; controls are disabled: {error}"
                                ),
                                "recoverable": true,
                            }
                        })
                    );
                    return;
                }
            }
        };
        if read == 0 {
            return;
        }
        for &byte in &chunk[..read] {
            if byte == b'\n' {
                if !discarding {
                    if line.last() == Some(&b'\r') {
                        line.pop();
                    }
                    if let Ok(envelope) = serde_json::from_slice::<UiSliderControlEnvelope>(&line)
                        && envelope.ui_control.wire_valid()
                    {
                        let control = envelope.ui_control;
                        let mut pending_guard = match pending.lock() {
                            Ok(guard) => guard,
                            Err(poisoned) => poisoned.into_inner(),
                        };
                        if pending_guard.contains_key(&control.id)
                            || pending_guard.len() < MAX_PENDING_UI_SLIDERS
                        {
                            pending_guard.insert(control.id.clone(), control);
                        }
                    }
                }
                line.clear();
                discarding = false;
            } else if !discarding {
                if line.len() < MAX_UI_CONTROL_LINE_BYTES {
                    line.push(byte);
                } else {
                    line.clear();
                    discarding = true;
                }
            }
        }
    }
}

#[cfg(feature = "device-audio")]
pub(super) fn apply_ui_slider_control(
    session: &Session,
    layout: &mut UiLayoutDelivery,
    control: &UiSliderControl,
    corrective_generation: Option<u64>,
) -> UiSliderApplyStatus {
    // A control is only legitimate for a layout the client actually received
    //. The corrective exception covers the still-audible
    // layout after a failed control generation: that layout was delivered and
    // remains the authority even while a replacement is being rebuilt.
    let generation_recognized = control.generation == session.generation()
        || corrective_generation == Some(control.generation);
    let layout_delivered = layout.delivered_generation == Some(control.generation)
        || corrective_generation == Some(control.generation);
    if !generation_recognized
        || !layout_delivered
        || layout.source_revision.as_deref() != Some(control.source_revision.as_str())
    {
        return UiSliderApplyStatus::Stale;
    }
    let Some(slider) = layout.sliders.get(&control.id) else {
        return UiSliderApplyStatus::Unknown;
    };
    if control.value < slider.min || control.value > slider.max {
        return UiSliderApplyStatus::OutOfRange;
    }
    match session.set_slider_value(&control.id, control.value) {
        Ok(true) => {
            layout.set_slider_value(&control.id, control.value);
            UiSliderApplyStatus::Applied
        }
        Ok(false) => UiSliderApplyStatus::RuntimeRejected,
        Err(error) => UiSliderApplyStatus::RuntimeFailed(error.to_string()),
    }
}

#[cfg(all(
    feature = "device-audio",
    any(feature = "midi", feature = "osc", feature = "serial")
))]
// Read by the live player's OSC and serial deadlines, and by its test in any
// build that has an external output at all.
#[cfg(any(
    all(feature = "device-audio", any(feature = "osc", feature = "serial")),
    all(test, any(feature = "midi", feature = "osc", feature = "serial"))
))]
pub(super) fn remaining_output_delay(target_time: f64, device_time: f64) -> std::time::Duration {
    let remaining = target_time - device_time;
    std::time::Duration::from_secs_f64(if remaining.is_finite() {
        remaining.clamp(0.0, 3600.0)
    } else {
        0.0
    })
}

/// Submit complete UI records without ever waiting behind the editor.
///
/// Events are grouped by generation so the envelope's generation is an exact
/// statement about every event it contains. `dropped` is carried forward
/// until one record reaches the bounded writer queue.
#[cfg(feature = "device-audio")]
fn emit_ui_trace_batches(
    sink: &rustel_runtime::ui_events::UiEventSink,
    session: &Session,
    device: &rustel_audio::LiveScalarDevice,
    traces: Vec<rustel_scheduler::ScheduleTraceEvent>,
    generation_sources: &std::collections::BTreeMap<u64, (String, f64)>,
    dropped: &mut u64,
) {
    let device_time = device.clock_seconds();
    let cycle = session.cycle_at_time(device_time);
    rustel_runtime::ui_events::dispatch_trace_batches(
        device_time,
        cycle,
        device.generation(),
        traces,
        generation_sources,
        dropped,
        |batch| {
            sink.try_send_traces(
                batch.device_time,
                batch.cycle,
                batch.cps,
                batch.generation,
                batch.source_revision,
                batch.traces,
                batch.dropped,
            )
        },
    );
}

#[cfg(feature = "device-audio")]
#[derive(Debug, Default)]
pub(super) struct UiVisualAudioCapture {
    mask: u64,
    source_revision: Option<String>,
}

#[cfg(feature = "device-audio")]
#[derive(Debug, PartialEq)]
pub(super) enum UiVisualAudioUpdate {
    Set(u64),
    Reset(u64),
}

#[cfg(feature = "device-audio")]
impl UiVisualAudioCapture {
    pub(super) fn update(
        &mut self,
        layout: &UiLayoutDelivery,
        audible_generation: u64,
    ) -> Option<UiVisualAudioUpdate> {
        let ready = layout.ready_for(audible_generation);
        let mask = if ready { layout.visual_audio_mask } else { 0 };
        // Slot numbers belong to a source revision. Reset only after its
        // cutover, so tails from the old score cannot enter reassigned slots.
        if ready && layout.source_revision != self.source_revision {
            self.source_revision.clone_from(&layout.source_revision);
            self.mask = mask;
            Some(UiVisualAudioUpdate::Reset(mask))
        } else if mask != self.mask {
            self.mask = mask;
            Some(UiVisualAudioUpdate::Set(mask))
        } else {
            None
        }
    }

    fn sync(&mut self, layout: &UiLayoutDelivery, device: &rustel_audio::LiveScalarDevice) {
        match self.update(layout, device.generation()) {
            Some(UiVisualAudioUpdate::Set(mask)) => device.set_visual_analysis_mask(mask),
            Some(UiVisualAudioUpdate::Reset(mask)) => device.reset_visual_analysis_mask(mask),
            None => {}
        }
    }
}

#[cfg(feature = "device-audio")]
pub(super) fn emit_ui_audio_frame(
    sink: &rustel_runtime::ui_events::UiEventSink,
    device: &rustel_audio::LiveScalarDevice,
    audible_generation: u64,
    visual_mask: u64,
    analyzer: &mut rustel_runtime::ui_analysis::UiAudioAnalyzer,
    samples: &mut [f32; rustel_audio::LIVE_ANALYSIS_WINDOW_SAMPLES],
    sequence: &mut u64,
) {
    let Some(snapshot) = device.copy_analysis_window(samples) else {
        // A callback may be publishing at the same instant. Skipping is the
        // intended bounded behavior; the next 30 Hz frame tries again.
        return;
    };
    *sequence = sequence.saturating_add(1);
    let master = analyzer.analyze(samples);
    let mut visuals = Vec::with_capacity(visual_mask.count_ones() as usize);
    let mut slots = visual_mask;
    while slots != 0 {
        let slot = slots.trailing_zeros() as u8;
        slots &= slots - 1;
        let Some(visual_snapshot) = device.copy_visual_analysis_window(slot, samples) else {
            continue;
        };
        // Each view uses its newest complete window; the callback can advance
        // while earlier views are being analyzed.
        if visual_snapshot.stream_id != snapshot.stream_id
            || visual_snapshot.sample_rate != snapshot.sample_rate
        {
            continue;
        }
        visuals.push((slot, analyzer.analyze(samples)));
    }
    let device_time = snapshot.end_frame as f64 / f64::from(snapshot.sample_rate.max(1));
    let _ = sink.try_send_audio_analysis(
        rustel_runtime::ui_events::UiAudioMetadata {
            sequence: *sequence,
            generation: audible_generation,
            device_time,
            stream_id: snapshot.stream_id,
            epoch: snapshot.epoch,
            end_frame: snapshot.end_frame,
            sample_rate: snapshot.sample_rate,
        },
        rustel_runtime::ui_analysis::UiAudioAnalysisSet {
            master,
            visuals,
            sides: Vec::new(),
        },
    );
}

/// Copy the newest `window` frames to their absolute positions in `buffer`.
///
/// `window` ends at absolute frame `end`. Until `end` reaches the window
/// length, the front of the window is startup padding and is not copied.
/// Frames at or past `limit` are dropped. A gap between calls stays zero.
///
/// ```text
/// window:                [ padding | available ]
/// buffer: [ 0 .. start ) [ start ........ end )    end is clipped to limit
/// ```
#[cfg(feature = "device-audio")]
fn capture_analysis_window(buffer: &mut Vec<f32>, end: usize, window: &[f32], limit: usize) {
    let available = end.min(window.len());
    let start = end - available;
    let end = end.min(limit);
    if buffer.len() < end {
        buffer.resize(end, 0.0);
    }
    if start < end {
        let offset = window.len() - available;
        buffer[start..end].copy_from_slice(&window[offset..offset + end - start]);
    }
}

#[cfg(all(test, feature = "device-audio"))]
mod live_capture_tests {
    use super::capture_analysis_window;

    #[test]
    fn debug_capture_uses_available_suffix_before_first_full_window() {
        const WINDOW: usize = rustel_audio::LIVE_ANALYSIS_WINDOW_SAMPLES;
        for end in [0, 1, 128, 1536, WINDOW - 1, WINDOW] {
            let samples: Vec<_> = (1..=end).map(|frame| frame as f32).collect();
            let mut window = [0.0; WINDOW];
            window[WINDOW - end..].copy_from_slice(&samples);
            let mut buffer = Vec::new();

            capture_analysis_window(&mut buffer, end, &window, WINDOW);

            assert_eq!(buffer, samples, "startup frame {end}");
        }
    }

    #[test]
    fn debug_capture_keeps_absolute_frames_across_overlaps_and_gaps() {
        let mut buffer = Vec::new();
        capture_analysis_window(&mut buffer, 2, &[0.0, 0.0, 1.0, 2.0], 12);
        capture_analysis_window(&mut buffer, 5, &[2.0, 3.0, 4.0, 5.0], 12);
        capture_analysis_window(&mut buffer, 11, &[8.0, 9.0, 10.0, 11.0], 12);

        assert_eq!(
            buffer,
            [1.0, 2.0, 3.0, 4.0, 5.0, 0.0, 0.0, 8.0, 9.0, 10.0, 11.0]
        );
    }

    #[test]
    fn debug_capture_clips_the_last_window_at_its_sample_limit() {
        let mut buffer = Vec::new();
        capture_analysis_window(&mut buffer, 6, &[3.0, 4.0, 5.0, 6.0], 5);
        assert_eq!(buffer, [0.0, 0.0, 3.0, 4.0, 5.0]);

        capture_analysis_window(&mut buffer, 11, &[8.0, 9.0, 10.0, 11.0], 5);
        assert_eq!(buffer, [0.0, 0.0, 3.0, 4.0, 5.0]);
    }

    #[test]
    fn debug_capture_limits_a_gap_that_reaches_past_the_recording() {
        let mut buffer = Vec::new();
        capture_analysis_window(&mut buffer, 9, &[6.0, 7.0, 8.0, 9.0], 4);
        assert_eq!(buffer, [0.0; 4]);
    }
}

#[cfg(all(test, feature = "device-audio"))]
mod live_panic_tests {
    use super::*;
    use rustel_audio::device::ManualLiveOutput;
    use rustel_runtime::{ReloadStatus, WatchLanguage, WatchPoll};
    use std::sync::Arc;
    use std::time::Duration;

    #[test]
    fn live_score_panic_reports_the_error_and_restores_audible_output() {
        let home = tempfile::tempdir().expect("temporary set");
        let path = home.path().join("live.strudel");
        let source = "note('c3').s('sine').fast(8).gain(0.1)";
        std::fs::write(&path, source).expect("score file");
        let loaded = LoadedLiveSources {
            score: source.into(),
            prebake: None,
        };
        let mut session = Session::new().expect("session");
        session.set_schedule_lead(0.0);
        session.set_continuity_margin(0.125);
        session.evaluate(&loaded.score).expect("initial score");
        session.restart_transport_at(0.0);
        let original = session.generation();
        let mut output = ManualLiveOutput::new(48_000, original).expect("manual output");
        session
            .bind_audio_confirmations(output.device().confirmations())
            .expect("bind output");
        let mut producer = build_live_producer(
            &path,
            WatchLanguage::JavaScript,
            &loaded,
            Duration::from_millis(120),
            Duration::from_millis(250),
            Duration::from_millis(2),
        )
        .expect("CLI live producer");
        let initial = producer
            .step_with_clock(
                &mut session,
                Duration::ZERO,
                || 0.0,
                48_000,
                |_, _, _| panic!("initial prefill replaced the generation"),
                |event| output.device().push(event),
            )
            .expect("initial prefill");
        assert!(initial.pushed > 0);
        let mut block = [0.0; 256];
        let mut sounded = false;
        for _ in 0..512 {
            output.render(&mut block);
            sounded |= block.iter().any(|sample| sample.abs() > 0.0001);
            session.consume_audio_confirmations();
            if session.confirmed_audio_generation() == Some(original) {
                break;
            }
        }
        assert!(sounded, "initial score was not audible");
        assert_eq!(session.confirmed_audio_generation(), Some(original));
        let transport = session.transport();
        session
            .set_pattern(rustel_core::signal(|_, _| {
                panic!("injected CLI query panic")
            }))
            .expect("candidate graph");
        let candidate = session.generation();
        producer.arm_replacement(original, candidate);
        let error = producer
            .step_with_clock(
                &mut session,
                Duration::from_millis(2),
                || output.device().clock_seconds(),
                48_000,
                |_, _, _| panic!("panicked candidate reached the output"),
                |_| panic!("panicked candidate queued audio"),
            )
            .expect_err("candidate query panics");
        let event = live_step_error_event(&error, output.device().stream_id());
        assert_eq!(event["live_error"]["kind"], "panic");
        assert_eq!(event["live_error"]["recoverable"], true);
        assert_eq!(
            event["live_error"]["stream_id"],
            output.device().stream_id()
        );
        assert!(
            event["live_error"]["message"]
                .as_str()
                .unwrap()
                .contains("injected CLI query panic")
        );
        assert_eq!(session.active_source(), Some(source));
        assert!(Arc::ptr_eq(&transport, &session.transport()));
        assert_eq!(output.device().generation(), original);
        assert_eq!(producer.take_abandoned_generation(), Some(candidate));
        let restored = session.generation();
        assert!(restored > candidate);

        let mut published = Vec::new();
        let mut takeover = 0;
        let recovered = producer
            .step_with_clock(
                &mut session,
                Duration::from_millis(4),
                || output.device().clock_seconds(),
                48_000,
                |generation, frame, cut| {
                    published.push(generation);
                    takeover = frame;
                    output.device().set_generation(generation, frame, cut);
                },
                |event| output.device().push(event),
            )
            .expect("restored producer");
        assert!(published.is_empty());
        let WatchPoll::Event(rejected) = recovered.watch else {
            panic!("candidate rejection report");
        };
        assert_eq!(rejected.status, ReloadStatus::Rejected);
        assert_eq!(rejected.error_kind.as_deref(), Some("panic"));
        let resumed = producer
            .step_with_clock(
                &mut session,
                Duration::from_millis(6),
                || output.device().clock_seconds(),
                48_000,
                |generation, frame, cut| {
                    published.push(generation);
                    takeover = frame;
                    output.device().set_generation(generation, frame, cut);
                },
                |event| output.device().push(event),
            )
            .expect("resume after the rejection report");
        assert_eq!(published, [restored]);
        // Recovery includes the time spent unwinding and rebuilding the realm.
        // Advance the device to its handoff before checking the restored audio.
        let resume_frame = takeover.checked_add(1).expect("takeover frame");
        let frames = resume_frame.saturating_sub(output.device().clock_frames());
        let block_frames = block.len() / 2;
        for offset in (0..frames).step_by(block_frames) {
            let count = (frames - offset).min(block_frames as u64) as usize;
            output.render(&mut block[..count * 2]);
        }
        assert!(
            output.device().clock_frames() >= resume_frame,
            "device did not reach recovery: takeover={takeover}, clock={}",
            output.device().clock_frames()
        );
        let mut pushed = recovered.pushed + resumed.pushed;
        let mut restored_sound = false;
        for turn in 0..512 {
            pushed += producer
                .step_with_clock(
                    &mut session,
                    Duration::from_millis(8 + turn * 2),
                    || output.device().clock_seconds(),
                    48_000,
                    |generation, frame, cut| output.device().set_generation(generation, frame, cut),
                    |event| output.device().push(event),
                )
                .expect("next live turn")
                .pushed;
            output.render(&mut block);
            restored_sound |= block.iter().any(|sample| sample.abs() > 0.0001);
            session.consume_audio_confirmations();
            if restored_sound && session.confirmed_audio_generation() == Some(restored) {
                break;
            }
        }
        assert!(pushed > 0, "restored score queued no audio");
        assert!(restored_sound, "restored score was not audible");
        assert_eq!(session.confirmed_audio_generation(), Some(restored));
        assert!(!session.transport().is_stopped());
    }
}

#[cfg(all(test, feature = "device-audio"))]
mod live_input_tests {
    use super::*;
    use std::time::Duration;

    #[derive(Default)]
    struct InputDevice {
        named: bool,
        failed: bool,
        channels: usize,
        opens: usize,
        closes: usize,
        open_error: Option<&'static str>,
    }

    impl InputDevice {
        fn update(
            &mut self,
            recovery: &mut LiveInputRecovery,
            now: Instant,
        ) -> Option<serde_json::Value> {
            recovery.update_with(
                LiveInputObservation {
                    now,
                    named: self.named,
                    failed: self.failed,
                    stream_id: 17,
                },
                "selected input",
                self,
                |device| {
                    device.closes += 1;
                    device.named = false;
                    device.failed = false;
                    device.channels = 0;
                },
                |device, wanted| {
                    assert_eq!(wanted, "selected input");
                    device.opens += 1;
                    if let Some(message) = device.open_error {
                        return Err(rustel_audio::DevicePlaybackError::Unavailable(
                            message.into(),
                        ));
                    }
                    device.named = true;
                    device.failed = false;
                    device.channels = 2;
                    Ok((wanted.into(), device.channels))
                },
            )
        }
    }

    #[test]
    fn selected_input_opens_on_the_first_turn() {
        let now = Instant::now();
        let mut recovery = LiveInputRecovery::new(now);
        let mut device = InputDevice::default();
        assert_eq!(
            device.update(&mut recovery, now),
            Some(serde_json::json!({
                "audio_input": {
                    "status": "open",
                    "device": "selected input",
                    "channels": 2,
                }
            }))
        );
        assert_eq!(device.opens, 1);
        assert_eq!(device.closes, 0);
    }

    #[test]
    fn failed_named_input_closes_once_and_waits_before_reopening() {
        let now = Instant::now();
        let mut recovery = LiveInputRecovery::new(now);
        let mut device = InputDevice::default();
        device.update(&mut recovery, now).expect("first open");
        device.failed = true;
        let failed_at = now + Duration::from_millis(1);
        assert_eq!(
            device.update(&mut recovery, failed_at),
            Some(serde_json::json!({
                "live_error": {
                    "kind": "audio",
                    "message": "the audio input failed; retrying",
                    "recoverable": true,
                    "stream_id": 17,
                }
            }))
        );
        assert!(!device.named);
        assert_eq!(device.channels, 0);
        assert_eq!((device.opens, device.closes), (1, 1));
        for elapsed in [0, 2, 250, 499] {
            assert_eq!(
                device.update(&mut recovery, failed_at + Duration::from_millis(elapsed)),
                None
            );
            assert_eq!((device.opens, device.closes), (1, 1));
        }
        let event = device
            .update(&mut recovery, failed_at + Duration::from_millis(500))
            .expect("retry at the deadline");
        assert_eq!(event["audio_input"]["status"], "open");
        assert_eq!((device.opens, device.closes), (2, 1));
        assert_eq!(recovery.attempts, 1);
    }

    #[test]
    fn consecutive_open_then_fail_attempts_increase_the_delay_without_error_spam() {
        let mut now = Instant::now();
        let mut recovery = LiveInputRecovery::new(now);
        let mut device = InputDevice::default();
        device.update(&mut recovery, now).expect("first open");
        for (index, delay_ms) in [500, 1_000, 2_000, 4_000, 8_000, 16_000, 16_000]
            .into_iter()
            .enumerate()
        {
            now += Duration::from_millis(1);
            device.failed = true;
            let event = device.update(&mut recovery, now);
            assert_eq!(event.is_some(), index == 0);
            assert_eq!(recovery.attempts, index as u64 + 1);
            assert_eq!((device.opens, device.closes), (index + 1, index + 1));
            let deadline = now + Duration::from_millis(delay_ms);
            assert_eq!(recovery.retry_at, deadline);
            assert_eq!(
                device.update(&mut recovery, deadline - Duration::from_nanos(1)),
                None
            );
            assert_eq!(device.opens, index + 1);
            assert!(device.update(&mut recovery, deadline).is_some());
            assert_eq!(device.opens, index + 2);
            assert_eq!(recovery.attempts, index as u64 + 1);
            now = deadline;
        }
    }

    #[test]
    fn synchronous_open_errors_keep_the_delay_and_report_once() {
        let now = Instant::now();
        let mut recovery = LiveInputRecovery::new(now);
        let mut device = InputDevice {
            open_error: Some("input is busy"),
            ..InputDevice::default()
        };
        assert_eq!(
            device.update(&mut recovery, now),
            Some(serde_json::json!({
                "live_error": {
                    "kind": "audio",
                    "message": "could not open the audio input: input is busy",
                    "recoverable": true,
                    "stream_id": 17,
                }
            }))
        );
        assert_eq!(recovery.retry_at, now + Duration::from_millis(500));
        assert_eq!(
            device.update(&mut recovery, now + Duration::from_millis(499)),
            None
        );
        assert_eq!(device.opens, 1);
        assert_eq!(
            device.update(&mut recovery, now + Duration::from_millis(500)),
            None
        );
        assert_eq!(device.opens, 2);
        assert_eq!(device.closes, 0);
        assert_eq!(recovery.attempts, 2);
        assert_eq!(recovery.retry_at, now + Duration::from_millis(1_500));
        device.open_error = None;
        assert!(
            device
                .update(&mut recovery, now + Duration::from_millis(1_500))
                .is_some()
        );
        assert_eq!(recovery.attempts, 2);
        assert!(recovery.refused);
    }

    #[test]
    fn a_healthy_observation_resets_the_failure_episode() {
        let now = Instant::now();
        let mut recovery = LiveInputRecovery::new(now);
        let mut device = InputDevice {
            open_error: Some("input is busy"),
            ..InputDevice::default()
        };
        device.update(&mut recovery, now).expect("open error");
        device.open_error = None;
        let opened_at = now + Duration::from_millis(500);
        device
            .update(&mut recovery, opened_at)
            .expect("retry opens");
        assert_eq!(recovery.attempts, 1);
        assert!(recovery.refused);
        let healthy_at = opened_at + Duration::from_millis(1);
        assert_eq!(device.update(&mut recovery, healthy_at), None);
        assert_eq!(recovery.attempts, 0);
        assert!(!recovery.refused);
        assert_eq!(device.opens, 2);
        device.failed = true;
        let failed_at = healthy_at + Duration::from_millis(1);
        let event = device
            .update(&mut recovery, failed_at)
            .expect("new failure is reported");
        assert_eq!(event["live_error"]["kind"], "audio");
        assert_eq!(recovery.attempts, 1);
        assert_eq!(recovery.retry_at, failed_at + Duration::from_millis(500));
        assert_eq!(device.opens, 2);
    }
}

#[cfg(feature = "device-audio")]
struct LiveInputObservation {
    now: Instant,
    named: bool,
    failed: bool,
    stream_id: u64,
}

#[cfg(feature = "device-audio")]
struct LiveInputRecovery {
    attempts: u64,
    retry_at: Instant,
    refused: bool,
}

#[cfg(feature = "device-audio")]
impl LiveInputRecovery {
    fn new(now: Instant) -> Self {
        Self {
            attempts: 0,
            retry_at: now,
            refused: false,
        }
    }

    fn update(
        &mut self,
        wanted: &str,
        device: &mut rustel_audio::LiveScalarDevice,
    ) -> Option<serde_json::Value> {
        let observed = LiveInputObservation {
            now: Instant::now(),
            named: device.input_name().is_some(),
            failed: device.input_failed(),
            stream_id: device.stream_id(),
        };
        self.update_with(
            observed,
            wanted,
            device,
            rustel_audio::LiveScalarDevice::close_input,
            |device, wanted| {
                device
                    .open_input(Some(wanted))
                    .map(|name| (name, device.input_channels()))
            },
        )
    }

    fn update_with<D>(
        &mut self,
        observed: LiveInputObservation,
        wanted: &str,
        device: &mut D,
        close: impl FnOnce(&mut D),
        open: impl FnOnce(&mut D, &str) -> Result<(String, usize), rustel_audio::DevicePlaybackError>,
    ) -> Option<serde_json::Value> {
        if observed.failed {
            close(device);
            return self.refuse(observed, "the audio input failed; retrying".into());
        }
        if observed.named {
            self.attempts = 0;
            self.refused = false;
            return None;
        }
        if observed.now < self.retry_at {
            return None;
        }
        // Opening can succeed before the callback fails. Reset the delay only
        // after a later turn observes an open input without a failure.
        match open(device, wanted) {
            Ok((name, channels)) => Some(serde_json::json!({
                "audio_input": {
                    "status": "open",
                    "device": name,
                    "channels": channels,
                }
            })),
            Err(error) => self.refuse(observed, format!("could not open the audio input: {error}")),
        }
    }

    fn refuse(
        &mut self,
        observed: LiveInputObservation,
        message: String,
    ) -> Option<serde_json::Value> {
        self.attempts = self.attempts.saturating_add(1);
        self.retry_at = observed
            .now
            .checked_add(rustel_audio::input_retry_delay(self.attempts))
            .unwrap_or(observed.now);
        let report = !self.refused;
        self.refused = true;
        report.then(|| {
            serde_json::json!({
                "live_error": {
                    "kind": "audio",
                    "message": message,
                    "recoverable": true,
                    "stream_id": observed.stream_id,
                }
            })
        })
    }
}

#[cfg(feature = "device-audio")]
fn live_step_error_event(error: &RuntimeError, stream_id: u64) -> serde_json::Value {
    serde_json::json!({
        "live_error": {
            "kind": error.kind(),
            "message": error.to_string(),
            "recoverable": true,
            "stream_id": stream_id,
        }
    })
}

/// Move every newly decoded sample into the live device without losing the
/// unvisited tail when its bounded install ring is temporarily full. Accepted
/// PCM is retained on the producer thread so a recycled device can be seeded
/// without downloading or decoding it again.
#[cfg(feature = "device-audio")]
fn install_ready_samples(
    library: &rustel_runtime::samples::SampleLibrary,
    retained: &mut std::collections::HashMap<rustel_audio::SampleId, rustel_audio::DecodedSample>,
    install: impl FnMut(
        rustel_audio::SampleId,
        rustel_audio::DecodedSample,
    ) -> Result<(), rustel_audio::DecodedSample>,
) {
    library.requeue_ready_batch_before_newer(install_sample_batch(
        library.take_ready(),
        retained,
        install,
    ));
}

#[cfg(feature = "device-audio")]
pub(super) fn install_sample_batch(
    ready: Vec<(rustel_audio::SampleId, rustel_audio::DecodedSample)>,
    retained: &mut std::collections::HashMap<rustel_audio::SampleId, rustel_audio::DecodedSample>,
    mut install: impl FnMut(
        rustel_audio::SampleId,
        rustel_audio::DecodedSample,
    ) -> Result<(), rustel_audio::DecodedSample>,
) -> Vec<(rustel_audio::SampleId, rustel_audio::DecodedSample)> {
    let mut ready = ready.into_iter();
    while let Some((id, decoded)) = ready.next() {
        let retained_sample = decoded.clone();
        if let Err(decoded) = install(id, decoded) {
            let mut retry = Vec::with_capacity(ready.len().saturating_add(1));
            retry.push((id, decoded));
            retry.extend(ready);
            return retry;
        }
        if (id.0 as usize) < rustel_audio::SAMPLE_BANK_CAPACITY {
            retained.insert(id, retained_sample);
        }
    }
    Vec::new()
}

/// Restore retained PCM after a device reopen unless a newer same-ID
/// publication is already queued; the library performs that decision while
/// holding the publication mutex.
#[cfg(feature = "device-audio")]
fn requeue_retained_samples(
    library: &rustel_runtime::samples::SampleLibrary,
    retained: &std::collections::HashMap<rustel_audio::SampleId, rustel_audio::DecodedSample>,
) {
    let mut ready = Vec::with_capacity(retained.len());
    for (id, decoded) in retained {
        ready.push((*id, decoded.clone()));
    }
    library.requeue_ready_batch_before_newer(ready);
}

/// Reconcile host listeners with both generations even when the producer step
/// cannot be scheduled. Besides opening/closing devices, `sync` acknowledges
/// the live host's generation union so rejected candidates cannot accumulate
/// forever in the input bus.
#[cfg(all(feature = "device-audio", feature = "midi"))]
pub(super) fn sync_live_midi_inputs(
    inputs: &mut rustel_runtime::midi_input::MidiInputs,
    bus: &rustel_core::midi_in::InputBus,
    audible_generation: u64,
    session_generation: u64,
) -> Vec<String> {
    inputs.sync(bus, audible_generation, session_generation)
}

#[cfg(feature = "device-audio")]
fn report_session_diagnostics(
    session: &mut Session,
    output: LiveOutput,
    stream_id: u64,
    reported: &mut std::collections::VecDeque<(String, String)>,
) {
    if output.structured() {
        return;
    }
    // The human live log does not list the voice resolver's notices; the
    // structured log prints them directly.
    let mut diagnostics: Vec<_> = session
        .take_diagnostics()
        .into_iter()
        .filter(|diagnostic| diagnostic.kind != rustel_runtime::VOICE_NOTICE_DIAGNOSTIC)
        .collect();
    diagnostics.extend(session.take_sample_failures().into_iter().map(|failure| {
        rustel_runtime::SessionDiagnostic {
            kind: "sample-failed".into(),
            message: failure.message,
            recoverable: true,
        }
    }));
    for diagnostic in diagnostics {
        let key = (diagnostic.kind.clone(), diagnostic.message.clone());
        if reported.contains(&key) {
            continue;
        }
        const MAX_REPORTED_SESSION_DIAGNOSTICS: usize = 64;
        if reported.len() == MAX_REPORTED_SESSION_DIAGNOSTICS {
            reported.pop_front();
        }
        reported.push_back(key);
        let event = serde_json::json!({
            "live_error": {
                "kind": diagnostic.kind,
                "message": diagnostic.message,
                "recoverable": diagnostic.recoverable,
                "stream_id": stream_id,
            }
        });
        output.event(LiveDetail::Essential, event);
    }
}

#[cfg(feature = "device-audio")]
pub(super) fn live_engine_pressure_event(
    pressure: rustel_runtime::EnginePressureSnapshot,
    context: rustel_runtime::EnginePressureReportContext,
) -> serde_json::Value {
    let device = pressure.device;
    let producer = pressure.producer;
    let verdict = match pressure.cause {
        rustel_runtime::EnginePressureCause::Healthy => "healthy",
        rustel_runtime::EnginePressureCause::DspOverload => {
            "dsp over budget: one callback's render outran its own buffer"
        }
        rustel_runtime::EnginePressureCause::ProducerOverload => {
            "cpu-bound: the scheduler cannot prepare this set in real time"
        }
        rustel_runtime::EnginePressureCause::HostStarvation => {
            "audio thread starved: something else on this machine is preempting it"
        }
        rustel_runtime::EnginePressureCause::VoicePressure => {
            "voice capacity pressure: voices, pools, or pending events saturated"
        }
        rustel_runtime::EnginePressureCause::Refusal => {
            "engine refused work before it could be scheduled"
        }
    };
    let report = pressure.report_v1(context);
    serde_json::json!({
        "live_load": {
            "producer_busy": f64::from(producer.slow_load_basis_points) / 10_000.0,
            "steps": producer.window_turns,
            "max_callback_gap_ms": device.max_callback_gap_nanos as f64 / 1e6,
            "late_events": device.late_events,
            "ring_peak_depth": device.ring_peak_depth,
            "callback_allocations": device.callback_allocations,
            "callback_busy_ms": device.max_callback_busy_nanos as f64 / 1e6,
            "verdict": verdict,
        },
        "engine_pressure": report,
    })
}

/// A clock port that will not open is a recoverable problem: the set keeps
/// playing, and the reason is said once rather than every pass.
#[cfg(all(feature = "device-audio", feature = "midi"))]
fn report_midi_clock_failure(
    output: &LiveOutput,
    stream_id: u64,
    direction: &str,
    message: String,
) {
    output.event(
        LiveDetail::Essential,
        serde_json::json!({
            "live_error": {
                "kind": "midi",
                "message": format!("could not open the MIDI clock {direction}: {message}"),
                "recoverable": true,
                "stream_id": stream_id,
            }
        }),
    );
}

/// Drain every external intent family once per live tick.
///
/// A failed step may have staged a partial batch. Discard all families together
/// so none of that work can escape on a later successful tick.
#[cfg(all(
    feature = "device-audio",
    any(feature = "midi", feature = "osc", feature = "serial")
))]
pub(super) fn take_tick_intents(session: &mut Session, step_failed: bool) -> TickIntents {
    let mut intents = TickIntents {
        #[cfg(feature = "midi")]
        midi: session.take_pending_midi(),
        #[cfg(feature = "osc")]
        osc: session.take_pending_osc(),
        #[cfg(feature = "serial")]
        serial: session.take_pending_serial(),
    };
    if step_failed {
        #[cfg(feature = "midi")]
        intents.midi.clear();
        #[cfg(feature = "osc")]
        intents.osc.clear();
        #[cfg(feature = "serial")]
        intents.serial.clear();
    }
    intents
}

// Every argument is a distinct decision the command line already made;
// bundling them into a struct would move the same list one file away.
#[cfg(feature = "device-audio")]
#[allow(clippy::too_many_arguments)]
pub(super) fn play_live(
    session: &mut Session,
    input: &SourceInput,
    reload: bool,
    duration_secs: Option<f64>,
    loaded: &LoadedLiveSources,
    initial_error: Option<RuntimeError>,
    mut recorder: Option<rustel_runtime::session_log::SessionRecorder>,
    extra_sounds: Vec<String>,
    follow: bool,
    announce_score: bool,
    #[cfg_attr(not(feature = "midi"), allow(unused_variables))] midi_virtual: Vec<String>,
    #[cfg_attr(not(feature = "midi"), allow(unused_variables))] midi_clock_out: Option<String>,
    #[cfg_attr(not(feature = "midi"), allow(unused_variables))] midi_clock_in: Option<String>,
    audio_input: Option<String>,
    buffer_frames: Option<u32>,
    ui_events: bool,
    first_install_from_zero: bool,
    anchored: Option<std::sync::mpsc::Sender<()>>,
    output: LiveOutput,
) -> Result<(), RuntimeError> {
    use std::time::{Duration, Instant};

    use rustel_runtime::{
        EnginePressureMonitor, EnginePressureReportContext, ProcessMonitor, ReloadStatus,
        WatchPoll, WatchTarget,
    };

    const DEBOUNCE: Duration = Duration::from_millis(100);
    const POLL: Duration = Duration::from_millis(2);
    const WATCH_POLL: Duration = Duration::from_millis(20);
    const DEVICE_PROGRESS_DEADLINE: Duration = Duration::from_secs(3);
    const MAX_PENDING_UI_TRACES: usize = 8_192;
    const STALE_UI_TRACE_GRACE_SECS: f64 = 1.0;
    const UI_TRACE_PRUNE_INTERVAL: Duration = Duration::from_millis(250);
    // 33_334 us is just slower than 30 Hz; timer jitter can reduce this rate
    // but can never make analysis work exceed it.
    const UI_AUDIO_INTERVAL: Duration = Duration::from_micros(33_334);

    let (path, language) = watch_file(input)?;
    session.set_direct_diagnostic_logging(output.structured());
    let ui_sink = if ui_events {
        match rustel_runtime::ui_events::UiEventSink::stdout() {
            Ok(sink) => Some(sink),
            Err(error) => {
                output.event(
                    LiveDetail::Essential,
                    serde_json::json!({
                        "live_error": {
                            "kind": "io",
                            "message": format!("could not start the UI event writer: {error}"),
                            "recoverable": true,
                        }
                    }),
                );
                None
            }
        }
    } else {
        None
    };
    let ui_events_enabled = ui_sink.is_some();
    let ui_controls = if ui_events_enabled {
        match UiControlInbox::stdin() {
            Ok(inbox) => Some(inbox),
            Err(error) => {
                output.event(
                    LiveDetail::Essential,
                    serde_json::json!({
                        "live_error": {
                            "kind": "io",
                            "message": format!("could not start the UI control reader: {error}"),
                            "recoverable": true,
                        }
                    }),
                );
                None
            }
        }
    } else {
        None
    };
    session.set_schedule_trace_enabled(ui_events_enabled);
    let mut ui_layout_delivery = UiLayoutDelivery::default();
    if ui_events_enabled
        && let Some(source) = session.active_source()
        && let Err(error) = ui_layout_delivery.observe(source, session.generation())
    {
        output.event(
            LiveDetail::Essential,
            serde_json::json!({
                "live_error": {
                    "kind": "ui-layout",
                    "message": error.to_string(),
                    "recoverable": true,
                    "generation": session.generation(),
                }
            }),
        );
    }
    if let Some(sink) = ui_sink.as_ref() {
        ui_layout_delivery.try_deliver(|layout| {
            sink.try_send_layout_recover(layout)
                .map_err(|(_, layout)| layout)
        });
    }
    let mut ui_generation_sources = std::collections::BTreeMap::new();
    if ui_events_enabled && let Some(source) = session.active_source() {
        ui_generation_sources.insert(
            session.generation(),
            (
                rustel_runtime::ui_events::source_revision(source),
                session.cps(),
            ),
        );
    }
    // Default sample banks are manifest-verified and fetched on demand by
    // the library's loader thread. Without a network, the set starts with
    // the bundled bd and reports the failure once. It does not stop.
    if let Err(error) = session.enable_default_samples() {
        output.event(
            LiveDetail::Essential,
            serde_json::json!({
                "sample_library": { "status": "unavailable", "message": error.to_string() }
            }),
        );
    }
    let mut device = rustel_audio::LiveScalarDevice::start_output_with_options(
        None,
        session.generation(),
        live_output_options(session, buffer_frames),
    )
    .map_err(runtime_device_error)?;
    sync_live_polyphony(session, &device);
    // What opened, once: the host, the device, the buffer's real cost.
    output.event(
        LiveDetail::Essential,
        serde_json::json!({ "audio": device.audio_facts().output().describe_one_line() }),
    );
    if let Err(error) = session.bind_audio_confirmations(device.confirmations()) {
        session.transport().stop();
        device.stop();
        return Err(error);
    }
    let mut ui_audio_enabled = ui_events_enabled
        && ui_layout_delivery.ready_for(session.generation())
        && ui_layout_delivery.audio_requested();
    // The tap has more than one reader: editor scopes and the debug
    // capture. One place decides, once per pass, whether the tap is on.
    let mut analysis_enabled = ui_audio_enabled;
    device.set_analysis_enabled(analysis_enabled);
    let mut ui_visual_audio = UiVisualAudioCapture::default();
    ui_visual_audio.sync(&ui_layout_delivery, &device);
    let mut ui_audio_analyzer = None;
    let mut ui_audio_samples = [0.0f32; rustel_audio::LIVE_ANALYSIS_WINDOW_SAMPLES];
    let mut ui_audio_sequence = 0u64;
    let allocator_tripwire_armed = device.arm_callback_tripwire();
    if !allocator_tripwire_armed {
        return Err(RuntimeError::Audio(
            "audio callback allocator/reporting tripwire is not armed".into(),
        ));
    }
    let sample_rate = device.sample_rate();
    if let Some(library) = session.sample_library() {
        // Decode to the rate this device actually opened at, the way an
        // AudioContext decodes to its own.
        library.set_render_rate(sample_rate);
    }
    // Transport-start semantics: the score was evaluated BEFORE the audio
    // clock existed, so the anchor pointed somewhere arbitrary in that clock.
    // Cycle ZERO
    // begins a short pre-roll AFTER the clock reading: anchoring at "now"
    // put the first onsets in the past before the producer's first push
    // (telemetry: late_events=2 on every start).
    // Wait briefly for the first callback so the lead includes current
    // playback latency rather than only a constant pre-roll.
    let lead_deadline = Instant::now() + Duration::from_millis(250);
    while device.report().playback_latency_nanos == 0 && Instant::now() < lead_deadline {
        std::thread::sleep(Duration::from_millis(5));
    }
    // First-start prefetch: kick every sample the first cycles reference
    // and give the library a bounded window to settle (disk-cache hits are
    // milliseconds; cold fetches get a few seconds), then deliver, so the
    // FIRST bar has its drums. Mid-set loads stay fully async.
    let mut warmup_error = session
        .with_panic_recovery(device.clock_seconds(), |session| {
            session.prefetch_samples(4.0, Duration::from_secs(8));
            Ok(())
        })
        .err();
    let mut retained_samples = std::collections::HashMap::new();
    if let Some(library) = session.sample_library() {
        install_ready_samples(library, &mut retained_samples, |id, decoded| {
            device.install_sample(id, decoded)
        });
    }
    // Watched reloads keep the timeline fixed and re-query one device lead
    // ahead.
    session.set_schedule_lead(device.schedule_lead_seconds());
    session.set_continuity_margin(device.continuity_margin_seconds());
    // The explicit floor is the live loop's poll quantum. The
    // producer adds the observed high-water of its actual continuation; this
    // is local adaptive protection, not a cross-platform timing guarantee.
    // The set does not open on a silent bar while its first sounds download.
    // This holds for ANY score, not only one that calls `preload`: a score
    // written for strudel.cc has no preload line and must not need one, so the
    // engine warms whatever the opening cycles ask for by itself.
    //
    // Startup only. A mid-set save never blocks, because holding the producer
    // for a network is how music stops.
    const PRELOAD_CEILING: Duration = Duration::from_secs(60);
    const WARM_QUERY_CEILING: Duration = Duration::from_secs(2);
    let preload_started = Instant::now();
    // Several rounds, because resolution is a chain: a name resolves to a
    // bank, a bank to a URL, and a soundfont lists its files only after the
    // first is decoded. One pass warms only the first link.
    // Start from the score text. It names every lane in the file, including
    // lanes that are commented out or muted. A query cannot see those lanes,
    // and their sounds would otherwise download when switched on mid-set.
    let mut named: Vec<String> = rustel_runtime::sounds::in_score(&loaded.score);
    for name in extra_sounds {
        if !named.contains(&name) {
            named.push(name);
        }
    }
    let named_files = session.prefetch_sounds(&named);
    let mut asked = 0usize;
    let mut timed_out = false;
    for _ in 0..3 {
        if warmup_error.is_some() {
            break;
        }
        let now = device.clock_seconds();
        if let Err(error) = session.with_panic_recovery(now, |session| {
            session.kick_sample_loads_within(session.cycle_at_time(now), 16.0, WARM_QUERY_CEILING);
            Ok(())
        }) {
            warmup_error = Some(error);
            break;
        }
        let remaining = PRELOAD_CEILING.saturating_sub(preload_started.elapsed());
        if remaining.is_zero() {
            timed_out = true;
            break;
        }
        let (round_asked, round_timed_out) = session.wait_for_sample_loads(remaining);
        asked += round_asked;
        timed_out |= round_timed_out;
    }
    let waited = preload_started.elapsed();
    if waited >= Duration::from_millis(50) || named_files + asked > 0 {
        output.event(
            LiveDetail::Verbose,
            serde_json::json!({
                "sample_warmup": {
                    "sounds_named": named.len(),
                    "files": named_files + asked,
                    "waited_ms": waited.as_millis(),
                    "timed_out": timed_out,
                    "message": "held the downbeat until the opening sounds were ready",
                }
            }),
        );
    }
    let mut producer = build_live_producer(path, language, loaded, DEBOUNCE, WATCH_POLL, POLL)?;
    if let Some(error) = warmup_error {
        producer.recover_after_panic(session, device.generation(), &error.to_string());
        output.event(
            LiveDetail::Essential,
            serde_json::json!({
                "live_error": {
                    "kind": error.kind(),
                    "message": error.to_string(),
                    "recoverable": true,
                    "stream_id": device.stream_id(),
                }
            }),
        );
    }
    // Anchor cycle ZERO only now, after every start-up cost that could eat
    // into the schedule lead (producer construction above; the sample
    // prefetch earlier), plus a fixed pre-roll for the first prefill query
    // itself. Anchoring before those costs put the first onsets in the past
    // by whatever they consumed, truncating the first beat; telemetry then
    // reports late events on every start.
    const START_PREROLL_SECS: f64 = 0.15;
    // ... and only once the device is actually running. A stream is not ready
    // the moment it opens: WSLg, and any remote or network sink, keeps
    // deepening its buffer for the first seconds, so `schedule_lead_seconds`
    // read here is still growing. Anchoring cycle zero against a lead that
    // then grows underneath it places the opening onsets at an offset that is
    // wrong by however much it grew, which took the attack off the first beat
    // -- and because the settling is not deterministic, by a different amount
    // on every start.
    //
    // A browser never shows this: its AudioContext has been running since the
    // page loaded, so the sink is warm long before a pattern plays. Wait for
    // that same condition rather than guessing a constant at it: callbacks
    // actually delivered, and a playback latency that has stopped moving.
    // Sampled per CALLBACK, never per poll: the latency reading only moves
    // when the device delivers, so two equal reads 10 ms apart prove nothing
    // and would call it settled immediately. Measured on WSLg the reading runs
    // 41.7 ms at the second callback, 74.4 at the third, then holds near 81 --
    // it DOUBLES. Anchoring on the first reading therefore placed the opening
    // onsets about 40 ms out, which is more than the 20 ms attack of a kick.
    let settle_deadline = Instant::now() + Duration::from_millis(800);
    let settle_started = Instant::now();
    let mut recent: Vec<u64> = Vec::new();
    let mut last_callbacks = 0u64;
    let settled = loop {
        let report = device.report();
        if report.callbacks != last_callbacks {
            last_callbacks = report.callbacks;
            recent.push(report.playback_latency_nanos);
            if recent.len() > 3 {
                recent.remove(0);
            }
            // Three consecutive callbacks within a millisecond of each other,
            // and enough of them to be past the ramp above.
            if report.callbacks >= 4 && recent.len() == 3 {
                let low = recent.iter().copied().min().unwrap_or(0);
                let high = recent.iter().copied().max().unwrap_or(0);
                if high.saturating_sub(low) < 1_000_000 {
                    break true;
                }
            }
        }
        if Instant::now() >= settle_deadline {
            break false;
        }
        std::thread::sleep(Duration::from_millis(2));
    };
    let settle_report = device.report();
    output.event(
        LiveDetail::Verbose,
        serde_json::json!({
            "device_settle": {
                "waited_ms": settle_started.elapsed().as_millis(),
                "callbacks": settle_report.callbacks,
                "playback_latency_ms":
                    (settle_report.playback_latency_nanos as f64 / 1e6 * 100.0).round() / 100.0,
                "settled": settled,
                "message": "anchored the downbeat only once the device stopped moving",
            }
        }),
    );
    let transport_anchor =
        device.clock_seconds() + device.schedule_lead_seconds() + START_PREROLL_SECS;
    #[cfg(feature = "midi")]
    session.midi_input_bus().clear_keys();
    session.restart_transport_at(transport_anchor);
    if let Some(anchored) = anchored {
        let _ = anchored.send(());
    }
    let wall_started = Instant::now();
    let live_deadline = duration_secs
        .map(Duration::from_secs_f64)
        .and_then(|duration| wall_started.checked_add(duration));
    let mut progress_at = wall_started;
    let mut progress_clock = device.clock_nanos();
    let mut last_step_error: Option<String> = None;
    let mut reported_session_diagnostics = std::collections::VecDeque::new();
    // Opened lazily on the first onset that asks, so a score without
    // `.serial()` never touches the platform serial stack.
    #[cfg(feature = "serial")]
    let mut serial_outputs = rustel_runtime::serial_bridge::SerialOutputs::new();
    // Ports open lazily, on the first onset that names one, so a score with no
    // `.midi()` never touches the platform MIDI stack at all.
    #[cfg(feature = "midi")]
    let mut midi_outputs = rustel_runtime::midi_bridge::MidiOutputs::new();
    // One mapping from the device clock to the wall clock for the whole set,
    // so messages a millisecond apart stay a millisecond apart and in order.
    // Rebasing per send instead pins each message to the jitter of the moment
    // it was handed over, and the note-off margin is only a millisecond.
    #[cfg(feature = "midi")]
    let mut midi_clock = rustel_runtime::midi_bridge::MidiClock::new();
    // Preserve note-off/on order against drift accumulated over a whole gate.
    #[cfg(feature = "midi")]
    let mut note_order = rustel_runtime::midi_bridge::NoteOrdering::new();
    // Inputs a score named with `midin()`/`midikeys()`. Attached after each
    // pass, never during evaluation: enumerating a platform MIDI stack can
    // block, and the score deadline cannot interrupt a blocked device open.
    #[cfg(feature = "midi")]
    let mut midi_inputs = rustel_runtime::midi_input::MidiInputs::new();
    // Published before the first onset so the port is already there when a DAW
    // goes looking, rather than appearing once the music starts.
    #[cfg(feature = "midi")]
    for name in &midi_virtual {
        // Virtual publication is the one deliberately synchronous MIDI
        // operation. It happens before the producer/device loop starts;
        // score-selected hardware outputs use the async opener below.
        if let Err(message) = midi_outputs.publish_at_startup(name) {
            output.event(
                LiveDetail::Essential,
                serde_json::json!({
                    "live_error": {
                        "kind": "midi",
                        "message": message,
                        "recoverable": true,
                        "stream_id": device.stream_id(),
                    }
                }),
            );
        } else {
            output.event(
                LiveDetail::Verbose,
                serde_json::json!({ "midi_published": { "port": name } }),
            );
        }
    }
    // Engine-driven MIDI clock. The score cannot ask for this: 24 pulses a
    // beat out to the hardware, or an outside clock the scheduler follows.
    // Opened here rather than lazily, because a clock has to be running
    // before the first onset rather than after it.
    #[cfg(feature = "midi")]
    let mut clock_out = match midi_clock_out.as_deref() {
        Some(port) => match rustel_runtime::midi_clock::ClockOut::open(port) {
            Ok(opened) => {
                output.event(
                    LiveDetail::Essential,
                    serde_json::json!({
                        "midi_clock": { "status": "out", "port": opened.port().to_owned() }
                    }),
                );
                Some(opened)
            }
            Err(message) => {
                report_midi_clock_failure(&output, device.stream_id(), "out", message);
                None
            }
        },
        None => None,
    };
    #[cfg(feature = "midi")]
    let clock_in = match midi_clock_in.as_deref() {
        Some(port) => match rustel_runtime::midi_clock::ClockIn::open(port) {
            Ok(opened) => {
                output.event(
                    LiveDetail::Essential,
                    serde_json::json!({
                        "midi_clock": { "status": "in", "port": opened.port().to_owned() }
                    }),
                );
                Some(opened)
            }
            Err(message) => {
                report_midi_clock_failure(&output, device.stream_id(), "in", message);
                None
            }
        },
        None => None,
    };
    #[cfg(feature = "midi")]
    let mut clock_followed_at = Instant::now();
    #[cfg(feature = "midi")]
    let mut clock_locked = false;
    // Open the selected input on the first pass. Delay further attempts after
    // an open or stream failure. With no selection, `s("in")` stays silent.
    let mut input_recovery = LiveInputRecovery::new(Instant::now());
    #[cfg(feature = "midi")]
    let mut last_midi_report = rustel_midi::MidiReport::default();
    #[cfg(feature = "midi")]
    let mut midi_queue_refusal_reported = false;
    #[cfg(feature = "midi")]
    let mut midi_late_drop_reported = false;
    #[cfg(feature = "midi")]
    let mut midi_send_error_reported = false;
    #[cfg(feature = "midi")]
    let mut midi_error_drop_reported = false;
    // One socket for the whole set, opened lazily on the first onset that asks
    // for OSC, so a score without `.osc()` never binds anything.
    #[cfg(feature = "osc")]
    let mut osc_sender: Option<rustel_osc::OscSender> = None;
    #[cfg(feature = "osc")]
    let mut osc_open_failed = false;
    let mut last_recycle = Instant::now() - Duration::from_secs(30);
    const RECYCLE_COOLDOWN: Duration = Duration::from_secs(8);
    let live_source_revision = ui_generation_sources
        .get(&session.generation())
        .map(|(revision, _)| revision.clone());
    output.event(
        LiveDetail::Essential,
        serde_json::json!({
            "live": {
                "status": "started",
                "watch": reload,
                "stream_id": device.stream_id(),
                "generation": session.generation(),
                "source_revision": live_source_revision,
                "path": path.display().to_string(),
                "device": device.name(),
                "sample_rate": sample_rate,
                "channels": device.channels(),
                "requested_buffer_frames": device.requested_buffer_frames(),
                "reported_buffer_frames": device.reported_buffer_frames(),
                "allocator_tripwire_armed": allocator_tripwire_armed,
                "audio_backend": "scalar-rust/cpal",
                "device_audio": "played"
            }
        }),
    );
    let mut pressure_monitor = EnginePressureMonitor::default();
    let mut pressure_since = Instant::now();
    let mut process_monitor = output.enabled(LiveDetail::Debug).then(|| {
        let now = Instant::now();
        let mut monitor = ProcessMonitor::new(now);
        monitor.sample(now);
        monitor
    });
    let mut installs = 0usize;
    // The state the set opened on. Without it a replay would start from the
    // first edit, so everything before that edit - usually the whole opening -
    // would be missing from the tape.
    if (announce_score || follow)
        && let Ok(source) = std::fs::read_to_string(path)
    {
        installs += 1;
        announce_active_score(installs, None, 0.0, &source, announce_score, follow);
    }
    if let Some(recorder) = recorder.as_mut()
        && let Ok(source) = std::fs::read_to_string(path)
    {
        recorder.record_save(
            0.0,
            if initial_error.is_some() {
                rustel_runtime::session_log::SaveStatus::Rejected
            } else {
                rustel_runtime::session_log::SaveStatus::Installed
            },
            &source,
            initial_error
                .as_ref()
                .map(|error| error.to_string())
                .as_deref(),
        );
    }
    if let Some(error) = initial_error {
        output.event(
            LiveDetail::Essential,
            serde_json::json!({
                "live_error": {
                    "kind": error.kind(),
                    "message": error.to_string(),
                    "recoverable": true,
                    "stream_id": device.stream_id(),
                }
            }),
        );
    }

    let mut reported_refused_voices = 0u64;
    // Grows with the pending count. Reserving the cap would hold 6 MB.
    let mut pending_ui_traces =
        std::collections::HashMap::<u64, rustel_scheduler::ScheduleTraceEvent>::new();
    let mut withheld_ui_traces = Vec::<rustel_scheduler::ScheduleTraceEvent>::new();
    let mut fresh_ui_traces = Vec::new();
    let mut ui_events_dropped = 0u64;
    let mut next_ui_trace_prune = Instant::now();
    let mut next_ui_audio = Instant::now();
    let mut pending_ui_control_cutover: Option<PendingUiControlCutover> = None;
    let mut last_slider_runtime_error: Option<String> = None;
    // Ten minutes at 48 kHz, after which the capture simply stops growing.
    const DEBUG_CAPTURE_MAX_SAMPLES: usize = 48_000 * 600;
    let mut debug_capture: Option<(Vec<f32>, usize)> =
        std::env::var_os("RUSTEL_DEBUG_CAPTURE").map(|_| (Vec::new(), 0usize));
    #[cfg(feature = "hydra")]
    let mut hydra_reported = false;
    if debug_capture.is_some() {
        device.set_analysis_enabled(true);
    }
    let mut first_install = replay_plan::FirstInstallFromZero::new(first_install_from_zero);
    let result = loop {
        #[cfg(feature = "gamepad")]
        rustel_runtime::gamepad::pump();
        session.consume_audio_confirmations();
        if live_deadline.is_some_and(|deadline| Instant::now() >= deadline) {
            session.transport().stop();
            break Ok(());
        }
        if session.transport().is_stopped() {
            // Only a signal earns the tail. A stop from anywhere else -- a
            // finished duration, a host calling stop -- keeps its contract of
            // ending immediately.
            if interrupted_by().is_some() {
                drain_tail_after_stop(&device, output);
            }
            break Err(RuntimeError::Cancelled);
        }
        let corrective_generation = pending_ui_control_cutover.and_then(|pending| {
            pending.corrective_generation(session.generation(), device.generation())
        });
        if pending_ui_control_cutover.is_some() && corrective_generation.is_none() {
            pending_ui_control_cutover = None;
        }
        // stdin is parsed and coalesced on a separate bounded reader, but the
        // QuickJS cell mutation happens here on the sole Session/producer
        // thread. No control path can run on or wait for the audio callback.
        let mut slider_changed = false;
        let mut slider_runtime_failure: Option<String> = None;
        if let Some(controls) = ui_controls.as_ref() {
            for control in controls.drain() {
                let applied = session.with_panic_recovery(device.clock_seconds(), |session| {
                    Ok(apply_ui_slider_control(
                        session,
                        &mut ui_layout_delivery,
                        &control,
                        corrective_generation,
                    ))
                });
                match applied {
                    Ok(UiSliderApplyStatus::Applied) => slider_changed = true,
                    Ok(UiSliderApplyStatus::RuntimeFailed(message)) => {
                        // The client stays silent per protocol v1, but the
                        // operator must learn that a drag did not take.
                        slider_runtime_failure.get_or_insert(message);
                    }
                    Err(error) => {
                        slider_changed = false;
                        slider_runtime_failure.get_or_insert(error.to_string());
                        break;
                    }
                    _ => {}
                }
            }
        }
        if let Some(message) = slider_runtime_failure.as_ref()
            && last_slider_runtime_error.as_ref() != Some(message)
        {
            last_slider_runtime_error = Some(message.clone());
            output.event(
                LiveDetail::Essential,
                serde_json::json!({
                    "live_error": {
                        "kind": "ui-control",
                        "message": format!("a slider change was refused by the running score: {message}"),
                        "recoverable": true,
                        "stream_id": device.stream_id(),
                    }
                }),
            );
        } else if slider_runtime_failure.is_none() {
            last_slider_runtime_error = None;
        }
        if slider_changed {
            match session.requery_active_at(device.clock_seconds()) {
                Ok(Some((generation_before, generation_after))) => {
                    producer.arm_control_requery(generation_before, generation_after);
                    pending_ui_control_cutover = Some(PendingUiControlCutover {
                        audible_generation: device.generation(),
                        session_generation: generation_after,
                    });
                }
                Ok(None) => {}
                Err(error) => {
                    output.event(
                        LiveDetail::Essential,
                        serde_json::json!({
                            "live_error": {
                                "kind": error.kind(),
                                "message": error.to_string(),
                                "recoverable": true,
                                "stream_id": device.stream_id(),
                            }
                        }),
                    );
                }
            }
        }
        if let Err(error) = device.check_health() {
            break Err(runtime_device_error(error));
        }
        // Voice refusals are reported, never fatal: the set plays on with a
        // dropped note rather than stopping. Reported on CHANGE so a busy
        // passage does not spam the stream once per iteration.
        let refused = device.refused_voices();
        if refused > reported_refused_voices {
            reported_refused_voices = refused;
            output.event(
                LiveDetail::Essential,
                serde_json::json!({
                    "live_error": {
                        "kind": "resource-limit",
                        "message": format!(
                            "live scalar voice capacity was exceeded {refused} time(s); \
                             those notes did not sound and the set continues"
                        ),
                        "recoverable": true,
                        "stream_id": device.stream_id(),
                    }
                }),
            );
        }

        let clock = device.clock_nanos();
        if clock != progress_clock {
            progress_clock = clock;
            progress_at = Instant::now();
        } else if progress_at.elapsed() >= DEVICE_PROGRESS_DEADLINE
            && last_recycle.elapsed() >= RECYCLE_COOLDOWN
        {
            let name = device.name().to_string();
            let old_sample_rate = device.sample_rate();
            session.consume_audio_confirmations();
            let recycled = device.recycle_output();
            session.consume_audio_confirmations();
            match recycled {
                Ok(()) => {
                    sync_live_polyphony(session, &device);
                    last_recycle = Instant::now();
                    progress_clock = device.clock_nanos();
                    progress_at = Instant::now();
                    if device.sample_rate() != old_sample_rate
                        && let Some(library) = session.sample_library()
                    {
                        // A reopened device can land on a different rate;
                        // later decodes follow it.
                        library.set_render_rate(device.sample_rate());
                    }
                    session.set_schedule_lead(device.schedule_lead_seconds());
                    session.set_continuity_margin(device.continuity_margin_seconds());
                    // The stream ring and its absolute frame domain vanished.
                    // Hard-silence every score MIDI timeline before advancing
                    // the Session generation; published virtual ports remain
                    // open but have no stale score membership.
                    #[cfg(feature = "midi")]
                    midi_outputs.reset_after_audio_recycle();
                    session.consume_audio_confirmations();
                    let requery = session.requery_after_output_recycle_at(device.clock_seconds());
                    session.consume_audio_confirmations();
                    match requery {
                        Ok(Some((generation_before, generation_after))) => producer
                            .arm_output_recovery_requery(generation_before, generation_after),
                        Ok(None) => {}
                        Err(error) => {
                            output.event(
                                LiveDetail::Essential,
                                serde_json::json!({
                                    "live_error": {
                                        "kind": error.kind(),
                                        "message": error.to_string(),
                                        "recoverable": true,
                                        "stream_id": device.stream_id(),
                                    }
                                }),
                            );
                        }
                    }
                    if let Some(library) = session.sample_library() {
                        requeue_retained_samples(library, &retained_samples);
                    }
                    output.event(
                        LiveDetail::Verbose,
                        serde_json::json!({
                            "live": {
                                "status": "recycled",
                                "reason": "clock-stall",
                                "device": name,
                                "stream_id": device.stream_id(),
                                "old_sample_rate": old_sample_rate,
                                "sample_rate": device.sample_rate(),
                                "requested_buffer_frames": device.requested_buffer_frames(),
                            }
                        }),
                    );
                    continue;
                }
                // Opening a fresh stream failed: the device is genuinely
                // gone, not paused. This is the one stall outcome that
                // ends the set.
                Err(error) => break Err(runtime_device_error(error)),
            }
        }
        // The stall outlasted one recycle (a Windows lock holds the RDP
        // path for as long as the operator is away). Hold the set alive:
        // ring back-pressure bounds the producer, and the next cooldown
        // window retries the recycle. Breaking here ended the set on any
        // lock longer than ~10 seconds.

        // Recycling may select a replacement output with a different native
        // sample rate. Take the rate from the current stream every pass so
        // scheduling, cutover, and MIDI retirement all share its frame unit.
        let sample_rate = device.sample_rate();

        // Deliver freshly decoded samples to the callback's bank; returned
        // (displaced) boxes are freed here, never on the audio thread.
        let asset_install_started = Instant::now();
        if let Some(library) = session.sample_library() {
            install_ready_samples(library, &mut retained_samples, |id, decoded| {
                device.install_sample(id, decoded)
            });
        }
        session.record_live_asset_preparation(asset_install_started.elapsed());

        // The playback latency (and with it the consumption frontier) settles
        // only after the first callbacks - WSLg keeps deepening its sink for
        // seconds - so the reload cursor margin is refreshed every tick.
        session.set_continuity_margin(device.continuity_margin_seconds());

        // `RUSTEL_DEBUG_CAPTURE=out.wav` reconstructs the real post-mix output
        // from the analysis tap, each window placed at its absolute frame so a
        // gap in the mix shows up as a gap in the file. This is how a glitch
        // gets attributed: if the capture is clean, the engine handed the
        // device correct audio and the artifact is downstream of us.
        //
        // Bounded, because an unbounded buffer in the live loop is exactly the
        // kind of slow leak that ends a set.
        if let Some((buffer, seen)) = debug_capture.as_mut()
            && buffer.len() < DEBUG_CAPTURE_MAX_SAMPLES
        {
            let mut window = [0.0f32; rustel_audio::LIVE_ANALYSIS_WINDOW_SAMPLES];
            if let Some(snapshot) = device.copy_analysis_window(&mut window) {
                let end = snapshot.end_frame as usize;
                if end > *seen {
                    capture_analysis_window(buffer, end, &window, DEBUG_CAPTURE_MAX_SAMPLES);
                    *seen = end;
                }
            }
        }
        let elapsed = wall_started.elapsed();
        let mut pushed_ui_ids = Vec::new();
        let mut reverbs = rustel_audio::LiveReverbBatch::default();
        let mut push_audio = |event: rustel_audio::QueuedAudioEvent| {
            let pushed = device.push(event);
            if pushed {
                reverbs.observe(&device, &event);
                if ui_events_enabled {
                    pushed_ui_ids.push(rustel_runtime::ui_events::UiAcceptedOnset {
                        generation: event.generation,
                        onset_id: event.onset_id,
                        frequency_hz: Some(event.freq_hz),
                        gain: Some(event.gain),
                    });
                }
            }
            pushed
        };
        // Slider controls arm a Session re-query that must publish to the
        // device the same way a watched reload does. The plain unwatched path
        // intentionally no-ops `set_generation`; with `--ui-events` that would
        // leave audio on the old generation after every accepted control while
        // Session had already moved on.
        let mut set_generation = |generation, takeover_frame, cut| {
            // The cut goes too: without one, an outgoing note that sounds
            // from the takeover frame on stands in for its incoming copy.
            #[cfg(feature = "midi")]
            midi_outputs.take_over_generation_from(
                device.generation(),
                generation,
                takeover_frame,
                takeover_frame,
                cut,
            );
            device.set_generation(generation, takeover_frame, cut);
        };
        let step_result = first_install.step(session, |session| {
            if reload {
                producer.step_with_clock(
                    session,
                    elapsed,
                    || device.render_frontier_seconds(),
                    sample_rate,
                    &mut set_generation,
                    &mut push_audio,
                )
            } else if ui_events_enabled || device.generation() != session.generation() {
                producer.step_unwatched_with_clock_and_cutover(
                    session,
                    || device.render_frontier_seconds(),
                    sample_rate,
                    &mut set_generation,
                    &mut push_audio,
                )
            } else {
                producer.step_unwatched_with_clock(
                    session,
                    || device.render_frontier_seconds(),
                    sample_rate,
                    &mut push_audio,
                )
            }
        });

        // Watch and replay can accept a score inside the producer step.
        // Even if its first scheduling window fails, its accepted module
        // settings are now the Session's; rejected evaluations keep the old one.
        sync_live_polyphony(session, &device);

        let reverb_preparation_started = Instant::now();
        reverbs.flush(&device);
        producer.record_asset_preparation(reverb_preparation_started.elapsed());

        report_session_diagnostics(
            session,
            output,
            device.stream_id(),
            &mut reported_session_diagnostics,
        );

        let audible_generation = device.generation();
        // Maintenance must happen before interpreting `step_result`: an
        // accepted candidate can fail its first scheduling window, but its
        // input generation still has to replace older failed candidates.
        #[cfg(feature = "midi")]
        for message in sync_live_midi_inputs(
            &mut midi_inputs,
            &session.midi_input_bus(),
            audible_generation,
            session.generation(),
        ) {
            output.event(
                LiveDetail::Essential,
                serde_json::json!({
                    "live_error": {
                        "kind": "midi",
                        "message": message,
                        "recoverable": true,
                        "stream_id": device.stream_id(),
                    }
                }),
            );
        }
        if let Some(wanted) = audio_input.as_deref()
            && let Some(event) = input_recovery.update(wanted, &mut device)
        {
            output.event(LiveDetail::Essential, event);
        }
        // Follow an outside clock, then put the engine's own ticks out. Both
        // happen once a pass: the follower rate-limits itself, and the sender
        // schedules to a horizon a little ahead of now.
        #[cfg(feature = "midi")]
        if let Some(follower) = clock_in.as_ref() {
            let now_instant = Instant::now();
            let estimate = follower.estimate(now_instant);
            let running = estimate.is_some_and(|heard| heard.running);
            if clock_locked && !running {
                clock_locked = false;
                output.event(
                    LiveDetail::Essential,
                    serde_json::json!({
                        "midi_clock": { "status": "lost", "port": follower.port() }
                    }),
                );
            }
            if let Some(estimate) = estimate
                && running
                && !session.transport().is_stopped()
                && now_instant.duration_since(clock_followed_at)
                    >= rustel_runtime::midi_clock::FOLLOW_INTERVAL
            {
                clock_followed_at = now_instant;
                let now = device.clock_seconds();
                let (steer, locked) = rustel_runtime::midi_clock::steer(
                    estimate,
                    session.cycle_at_time(now),
                    session.cps(),
                );
                if locked != clock_locked {
                    clock_locked = locked;
                    output.event(
                        LiveDetail::Essential,
                        serde_json::json!({
                            "midi_clock": {
                                "status": if locked { "locked" } else { "lost" },
                                "port": follower.port(),
                            }
                        }),
                    );
                }
                if let rustel_runtime::midi_clock::Steer::To { cps, cycle } = steer {
                    session.retime(now, cps, cycle);
                    if let Ok(Some((before, after))) = session.requery_active_at(now) {
                        producer.arm_control_requery(before, after);
                    }
                }
            }
        }
        #[cfg(feature = "midi")]
        if let Some(sender) = clock_out.as_mut() {
            let now = device.clock_seconds();
            sender.advance(
                !session.transport().is_stopped(),
                session.cycle_at_time(now),
                session.cps(),
                Instant::now(),
            );
        }
        #[cfg(any(feature = "midi", feature = "osc", feature = "serial"))]
        let intents = take_tick_intents(session, step_result.is_err());
        #[cfg(feature = "midi")]
        let pending_midi = intents.midi;
        #[cfg(feature = "osc")]
        let pending_osc = intents.osc;
        #[cfg(feature = "serial")]
        let pending_serial = intents.serial;
        // Reserve every route the successful step can actually emit before
        // exact-frame retirement. Otherwise a same-name old sender can be
        // retired at the boundary just before this pass submits the new
        // generation, and its detached emergency silence can cut new notes.
        #[cfg(feature = "midi")]
        for intent in &pending_midi {
            if (intent.generation == audible_generation
                || intent.generation == session.generation())
                && rustel_midi::has_output(&intent.controls)
                && let Err(message) =
                    midi_outputs.reserve_generation_port(intent.generation, &intent.port)
            {
                output.event(
                    LiveDetail::Essential,
                    serde_json::json!({
                        "live_error": {
                            "kind": "midi",
                            "message": message,
                            "recoverable": true,
                            "stream_id": device.stream_id(),
                        }
                    }),
                );
            }
        }
        // Output expiry/retirement is driven by the audio frontier, not by a
        // successful query pass. A recoverable score error below must not skip
        // the exact takeover boundary while the device clock keeps advancing.
        #[cfg(feature = "midi")]
        for message in midi_outputs.poll_at(
            audible_generation,
            session.generation(),
            device.clock_frames(),
        ) {
            output.event(
                LiveDetail::Essential,
                serde_json::json!({
                    "live_error": {
                        "kind": "midi",
                        "message": message,
                        "recoverable": true,
                        "stream_id": device.stream_id(),
                    }
                }),
            );
        }
        // The bridge opens serial ports on its own threads, so a blocked
        // Bluetooth or USB-CDC open cannot stall this tick. A failed open is
        // reported here one tick later, once per port. A port that is still
        // opening after a couple of seconds and each serial delivery fault
        // are also reported once, with kinds of their own: a slow port is
        // waiting and a dropped write is a warning. Neither stops the set.
        #[cfg(feature = "serial")]
        for news in serial_outputs.poll() {
            output.event(
                LiveDetail::Essential,
                serde_json::json!({
                    "live_error": {
                        "kind": news.kind.live_kind(),
                        "message": news.message,
                        "recoverable": true,
                        "stream_id": device.stream_id(),
                    }
                }),
            );
        }
        let trace_started = Instant::now();
        fresh_ui_traces.clear();
        let mut ui_layout_ready = false;
        if ui_events_enabled {
            ui_events_dropped =
                ui_events_dropped.saturating_add(session.take_schedule_trace_events_dropped());
            session.drain_schedule_trace_events_into(&mut fresh_ui_traces);
            let session_generation = session.generation();
            // Session installs a watched candidate before its first window is
            // proven schedulable. The audio consumer deliberately remains on
            // the old generation until that prefill succeeds, so no client may
            // advertise the candidate early.
            if session_generation == audible_generation {
                let mut observed_new_layout = false;
                if let std::collections::btree_map::Entry::Vacant(entry) =
                    ui_generation_sources.entry(session_generation)
                    && let Some(source) = session.active_source()
                {
                    entry.insert((
                        rustel_runtime::ui_events::source_revision(source),
                        session.cps(),
                    ));
                }
                if let Some(source) = session.active_source() {
                    match observe_ui_layout_if_audible(
                        &mut ui_layout_delivery,
                        source,
                        session_generation,
                        audible_generation,
                    ) {
                        Ok(observed) => observed_new_layout = observed,
                        Err(error) => {
                            output.event(
                                LiveDetail::Essential,
                                serde_json::json!({
                                    "live_error": {
                                        "kind": "ui-layout",
                                        "message": error.to_string(),
                                        "recoverable": true,
                                        "generation": session_generation,
                                        "stream_id": device.stream_id(),
                                    }
                                }),
                            );
                        }
                    }
                }
                // Reading a query-time slider crosses into QuickJS. Do it once
                // when a newly audible generation rebuilds its layout, not on
                // every ~2 ms producer tick. Applied controls update the owned
                // layout value directly between generations.
                if observed_new_layout
                    && let Err(error) =
                        session.with_panic_recovery(device.clock_seconds(), |session| {
                            ui_layout_delivery.sync_slider_values(session);
                            Ok(())
                        })
                {
                    output.event(
                        LiveDetail::Essential,
                        serde_json::json!({
                            "live_error": {
                                "kind": error.kind(),
                                "message": error.to_string(),
                                "recoverable": true,
                                "stream_id": device.stream_id(),
                            }
                        }),
                    );
                }
            }
            // An older layout may already be waiting behind a full writer.
            // Keep retrying it while an unpublished candidate is prefetched.
            if let Some(sink) = ui_sink.as_ref() {
                ui_layout_delivery.try_deliver(|layout| {
                    sink.try_send_layout_recover(layout)
                        .map_err(|(_, layout)| layout)
                });
            }
            ui_layout_ready = ui_layout_delivery.ready_for(audible_generation);
            let should_analyze_audio = ui_layout_ready && ui_layout_delivery.audio_requested();
            ui_audio_enabled = should_analyze_audio;
            ui_visual_audio.sync(&ui_layout_delivery, &device);
            let oldest_revision = audible_generation.saturating_sub(1);
            ui_generation_sources.retain(|known, _| *known >= oldest_revision);
        }
        rustel_runtime::ui_events::ingest_pending_traces(
            &mut pending_ui_traces,
            fresh_ui_traces.drain(..),
            step_result.is_ok(),
            audible_generation,
            MAX_PENDING_UI_TRACES,
            &mut ui_events_dropped,
        );

        // Correlate local audio now; external-only routes are added after
        // their own dispatch boundaries below. Unknown/duplicate IDs are
        // harmless, so a hap routed to several outputs still emits once.
        let correlation = rustel_runtime::ui_events::correlate_submitted_traces(
            &mut pending_ui_traces,
            pushed_ui_ids,
        );
        ui_events_dropped = ui_events_dropped.saturating_add(correlation.dropped);
        let mut ready_ui_traces = rustel_runtime::ui_events::release_traces_when_layout_ready(
            &mut withheld_ui_traces,
            correlation.ready,
            ui_layout_ready,
            audible_generation,
            MAX_PENDING_UI_TRACES,
            &mut ui_events_dropped,
        );
        let ui_audio_now = Instant::now();
        if ui_audio_enabled && ui_audio_now >= next_ui_audio {
            next_ui_audio = ui_audio_now + UI_AUDIO_INTERVAL;
            if let Some(sink) = ui_sink.as_ref() {
                let analyzer = ui_audio_analyzer
                    .get_or_insert_with(rustel_runtime::ui_analysis::UiAudioAnalyzer::new);
                emit_ui_audio_frame(
                    sink,
                    &device,
                    audible_generation,
                    ui_visual_audio.mask,
                    analyzer,
                    &mut ui_audio_samples,
                    &mut ui_audio_sequence,
                );
            }
        }
        if ui_events_enabled && Instant::now() >= next_ui_trace_prune {
            next_ui_trace_prune = Instant::now() + UI_TRACE_PRUNE_INTERVAL;
            let device_time = device.clock_seconds();
            rustel_runtime::ui_events::prune_pending_traces(
                &mut pending_ui_traces,
                audible_generation,
                device_time,
                STALE_UI_TRACE_GRACE_SECS,
                &mut ui_events_dropped,
            );
        }

        producer.complete_turn(trace_started.elapsed());
        if pressure_since.elapsed() >= Duration::from_secs(1) {
            if output.enabled(LiveDetail::Debug) {
                let device_report = device.report();
                let producer_report = producer.producer_load_snapshot();
                let buffer_frames = device
                    .reported_buffer_frames()
                    .unwrap_or_else(|| device.requested_buffer_frames());
                let pressure =
                    pressure_monitor.sample(device_report, producer_report, buffer_frames);
                let process = process_monitor
                    .as_mut()
                    .map(|monitor| monitor.sample(Instant::now()))
                    .unwrap_or_default();
                output.event(
                    LiveDetail::Debug,
                    live_engine_pressure_event(
                        pressure,
                        EnginePressureReportContext {
                            device_name: Some(device.name().to_owned()),
                            requested_buffer_frames: Some(device.requested_buffer_frames()),
                            reported_buffer_frames: device.reported_buffer_frames(),
                            process_cpu_percent: process.cpu_percent.map(f64::from),
                            process_resident_bytes: process.resident_bytes,
                        },
                    ),
                );
            }
            pressure_since = Instant::now();
        }
        let step = match step_result {
            Ok(step) => step,
            Err(RuntimeError::Cancelled) => break Err(RuntimeError::Cancelled),
            Err(error) => {
                if let Some(sink) = ui_sink.as_ref() {
                    emit_ui_trace_batches(
                        sink,
                        session,
                        &device,
                        ready_ui_traces,
                        &ui_generation_sources,
                        &mut ui_events_dropped,
                    );
                }
                // Keep the transport running through score/query errors so the
                // next save can recover. Report only when the message changes.
                let message = error.to_string();
                if last_step_error.as_deref() != Some(message.as_str()) {
                    let report = live_step_error_event(&error, device.stream_id());
                    if let Some(recorder) = recorder.as_mut() {
                        recorder.record_log(device.clock_seconds(), &report.to_string());
                    }
                    output.event(LiveDetail::Essential, report);
                    last_step_error = Some(message);
                }
                std::thread::sleep(POLL);
                continue;
            }
        };
        #[cfg(any(feature = "midi", feature = "osc", feature = "serial"))]
        let mut accepted_external_ui_ids = Vec::new();
        #[cfg(not(any(feature = "midi", feature = "osc", feature = "serial")))]
        let accepted_external_ui_ids = Vec::<rustel_runtime::ui_events::UiAcceptedOnset>::new();
        // Hand this pass's MIDI to its port. Everything here is best-effort:
        // a device that is not plugged in, or that went away mid-set, is
        // reported once and then ignored. MIDI silence is a bad night; a
        // stopped process is a ruined one.
        #[cfg(feature = "midi")]
        {
            let clock_now = device.clock_seconds();
            let session_generation = session.generation();
            for intent in pending_midi {
                // A failed/replaced scheduling pass may leave owned intents
                // behind. Only the consumer-visible generation union may
                // reach a device; everything else is stale score state.
                if intent.generation != audible_generation
                    && intent.generation != session_generation
                {
                    continue;
                }

                // Plan the complete onset before opening or borrowing its
                // port. The sender admits this vector atomically, so a full
                // queue can never accept a note-on while refusing its paired
                // note-off.
                let planned = rustel_midi::plan(&intent.controls, intent.duration_secs);
                if planned.is_empty() {
                    continue;
                }
                // One anchor for the whole set, not one per send. Taking
                // `Instant::now()` here pins every message to the jitter of the
                // moment it happened to be handed over, and consecutive haps
                // are handed over at different moments - so a note-off could
                // land after the next note-on and the synth cut the note it had
                // just started. `MidiClock` keeps the mapping monotone and
                // still tracks the device clock, by slewing rather than
                // stepping.
                let base = midi_clock.instant_for(clock_now, intent.target_time);
                let batch =
                    note_order.stamp_batch(&intent.port, intent.target_time, base, &planned);
                let sent = match midi_outputs.submit_batch_for_generation_at(
                    intent.generation,
                    &intent.port,
                    rustel_runtime::midi_bridge::onset_frame_at(intent.target_time, sample_rate),
                    batch,
                ) {
                    Ok(sent) => sent,
                    Err(message) => {
                        output.event(
                            LiveDetail::Essential,
                            serde_json::json!({
                                "live_error": {
                                    "kind": "midi",
                                    "message": message,
                                    "recoverable": true,
                                    "stream_id": device.stream_id(),
                                }
                            }),
                        );
                        continue;
                    }
                };
                if sent && ui_events_enabled {
                    accepted_external_ui_ids.push(rustel_runtime::ui_events::UiAcceptedOnset {
                        generation: intent.generation,
                        onset_id: intent.onset_id,
                        frequency_hz: None,
                        gain: None,
                    });
                }
            }
            let midi_report = midi_outputs.report();
            let mut problems = Vec::new();
            if midi_report.refused_full > last_midi_report.refused_full
                && !midi_queue_refusal_reported
            {
                midi_queue_refusal_reported = true;
                problems.push("MIDI output queue filled; a complete onset was refused");
            }
            if midi_report.dropped_late > last_midi_report.dropped_late && !midi_late_drop_reported
            {
                midi_late_drop_reported = true;
                problems.push("MIDI messages arrived too late and were dropped");
            }
            if midi_report.send_errors > last_midi_report.send_errors && !midi_send_error_reported {
                midi_send_error_reported = true;
                problems.push("a MIDI output device rejected messages or disconnected");
            }
            if midi_report.errors_dropped > last_midi_report.errors_dropped
                && !midi_error_drop_reported
            {
                midi_error_drop_reported = true;
                problems.push("bounded MIDI lifecycle diagnostics were truncated or dropped");
            }
            if !problems.is_empty() {
                let message = problems.join("; ");
                let diagnostic = serde_json::json!({
                    "live_error": {
                        "kind": "midi",
                        "message": message,
                        "recoverable": true,
                        "stream_id": device.stream_id(),
                        "midi": {
                            "sent": midi_report.sent,
                            "dropped_late": midi_report.dropped_late,
                            "refused_full": midi_report.refused_full,
                            "send_errors": midi_report.send_errors,
                            "errors_dropped": midi_report.errors_dropped,
                        }
                    }
                });
                if let Some(recorder) = recorder.as_mut() {
                    recorder.record_log(device.clock_seconds(), &diagnostic.to_string());
                }
                output.event(LiveDetail::Essential, diagnostic);
            }
            last_midi_report = midi_report;
        }

        // Hand this pass's OSC to the network. Unlike MIDI there is no send
        // thread: the bundle carries an NTP timetag and the receiver schedules
        // on it, which is how Tidal drives SuperDirt and keeps our jitter out
        // of the sound.
        #[cfg(feature = "osc")]
        {
            // Drained at the top of the tick, beside the MIDI take: a failed
            // step must never leave its staged intents for a later tick.
            let pending = pending_osc;
            if !pending.is_empty() && osc_sender.is_none() && !osc_open_failed {
                match rustel_osc::OscSender::new() {
                    Ok(sender) => osc_sender = Some(sender),
                    Err(message) => {
                        osc_open_failed = true;
                        output.event(
                            LiveDetail::Essential,
                            serde_json::json!({
                                "live_error": {
                                    "kind": "osc",
                                    "message": message,
                                    "recoverable": true,
                                    "stream_id": device.stream_id(),
                                }
                            }),
                        );
                    }
                }
            }
            if let Some(sender) = osc_sender.as_ref() {
                for (_lead_secs, intent) in pending {
                    let Some(destination) = intent.destination else {
                        continue;
                    };
                    let when = std::time::SystemTime::now()
                        + remaining_output_delay(intent.target_time, device.clock_seconds());
                    let sent = sender.send_dirt(destination, when, &intent.args);
                    if sent {
                        // The bundle is out: it stands for the copy that a
                        // takeover stages of the same onset.
                        session.note_osc_handed_out(intent.generation, intent.target_time);
                    }
                    if sent && ui_events_enabled {
                        accepted_external_ui_ids.push(rustel_runtime::ui_events::UiAcceptedOnset {
                            generation: intent.generation,
                            onset_id: intent.onset_id,
                            frequency_hz: None,
                            gain: None,
                        });
                    }
                }
            }
        }

        // Visuals are drawn behind the code, and this command has no code on
        // screen to draw behind. Say so once and carry on playing; `rustel
        // studio` is where a sketch is seen.
        #[cfg(feature = "hydra")]
        if let Some(candidate) = session.take_pending_hydra()
            && !candidate.is_empty()
            && !hydra_reported
        {
            hydra_reported = true;
            output.event(
                LiveDetail::Essential,
                serde_json::json!({
                    "hydra": { "status": "unavailable", "message": "this score draws visuals; run it in `rustel studio` to see them" }
                }),
            );
        }

        // One owner for the analysis tap, so the editor's scopes and the debug
        // capture cannot switch it off under each other.
        {
            let wanted = ui_audio_enabled || debug_capture.is_some();
            if wanted != analysis_enabled {
                device.set_analysis_enabled(wanted);
                analysis_enabled = wanted;
            }
        }

        // Hand this pass's serial writes to their port. Like MIDI the wire
        // has no notion of "later", so the sender schedules them; unlike MIDI
        // strudel.cc adds a fixed 100ms first, which is kept so a sketch tuned
        // against the browser does not receive everything early. The platform
        // open runs on a thread of the bridge's own, so a first onset for a
        // port crosses into its pre-open queue instead of waiting on a driver.
        #[cfg(feature = "serial")]
        {
            // Drained at the top of the tick, beside the MIDI take: a failed
            // step must never leave its staged intents for a later tick.
            for (_lead_secs, intent) in pending_serial {
                let due = Instant::now()
                    + remaining_output_delay(intent.target_time, device.clock_seconds())
                    + rustel_serial::SERIAL_LATENCY;
                let submitted =
                    match serial_outputs.submit(&intent.port, intent.baud, due, intent.bytes) {
                        Ok(submitted) => submitted,
                        Err(message) => {
                            output.event(
                                LiveDetail::Essential,
                                serde_json::json!({
                                    "live_error": {
                                        "kind": "serial",
                                        "message": message,
                                        "recoverable": true,
                                        "stream_id": device.stream_id(),
                                    }
                                }),
                            );
                            continue;
                        }
                    };
                // A baud note is news, not a refusal: the bridge already
                // dedupes it, and the write still goes out, at the baud the
                // port has for this set.
                if let Some(note) = submitted.note {
                    output.event(
                        LiveDetail::Essential,
                        serde_json::json!({
                            "live_error": {
                                "kind": "serial-baud",
                                "message": note,
                                "recoverable": true,
                                "stream_id": device.stream_id(),
                            }
                        }),
                    );
                }
                if submitted.accepted {
                    // The sender has the write: it stands for the copy
                    // that a takeover stages of the same onset.
                    session.note_serial_handed_out(intent.generation, intent.target_time);
                }
                if submitted.accepted && ui_events_enabled {
                    accepted_external_ui_ids.push(rustel_runtime::ui_events::UiAcceptedOnset {
                        generation: intent.generation,
                        onset_id: intent.onset_id,
                        frequency_hz: None,
                        gain: None,
                    });
                }
            }
        }

        let external_correlation = rustel_runtime::ui_events::correlate_submitted_traces(
            &mut pending_ui_traces,
            accepted_external_ui_ids,
        );
        ui_events_dropped = ui_events_dropped.saturating_add(external_correlation.dropped);
        ready_ui_traces.extend(rustel_runtime::ui_events::release_traces_when_layout_ready(
            &mut withheld_ui_traces,
            external_correlation.ready,
            ui_layout_ready,
            audible_generation,
            MAX_PENDING_UI_TRACES,
            &mut ui_events_dropped,
        ));

        last_step_error = None;
        let mut stopped = false;
        for poll in [step.prebake_watch, step.watch] {
            match poll {
                WatchPoll::Unchanged | WatchPoll::Pending => {}
                WatchPoll::Stopped => stopped = true,
                WatchPoll::Event(mut event) => {
                    if event.status == ReloadStatus::Installed {
                        // Warm from the current cycle, across enough cycles
                        // for alternations (`<clap:1 clap:6>`) and section
                        // changes to occur. A name that is not warm downloads
                        // when it first plays, and the artist hears a dropped
                        // beat.
                        let now = device.clock_seconds();
                        let generation = session.generation();
                        if let Err(error) = session.with_panic_recovery(now, |session| {
                            let at = session.cycle_at_time(now);
                            session.kick_sample_loads_from(at, 16.0);
                            Ok(())
                        }) {
                            producer.recover_after_panic(session, generation, &error.to_string());
                            output.event(
                                LiveDetail::Essential,
                                serde_json::json!({
                                    "live_error": {
                                        "kind": error.kind(),
                                        "message": error.to_string(),
                                        "recoverable": true,
                                        "stream_id": device.stream_id(),
                                    }
                                }),
                            );
                            // Recovery put the previous score back, so the
                            // save is announced and taped as refused.
                            if matches!(event.target, WatchTarget::Score(_)) {
                                event.status = ReloadStatus::Rejected;
                                event.error_kind = Some(error.kind().to_owned());
                                event.message = Some(error.to_string());
                            }
                        }
                    }
                    if (announce_score || follow)
                        && matches!(event.target, WatchTarget::Score(_))
                        && event.status == ReloadStatus::Installed
                        && let Ok(source) = std::fs::read_to_string(path)
                    {
                        installs += 1;
                        announce_active_score(
                            installs,
                            None,
                            device.clock_seconds(),
                            &source,
                            announce_score,
                            follow,
                        );
                    }
                    if let Some(recorder) = recorder.as_mut()
                        && matches!(event.target, WatchTarget::Score(_))
                    {
                        // Read the file rather than trusting a cached copy:
                        // the tape has to hold what the artist actually saved,
                        // including the save that failed to install.
                        if let Ok(source) = std::fs::read_to_string(path) {
                            let status = if event.status == ReloadStatus::Installed {
                                rustel_runtime::session_log::SaveStatus::Installed
                            } else {
                                rustel_runtime::session_log::SaveStatus::Rejected
                            };
                            recorder.record_save(
                                device.clock_seconds(),
                                status,
                                &source,
                                event.message.as_deref(),
                            );
                        }
                    }
                    if event.status == ReloadStatus::Installed {
                        reported_session_diagnostics.clear();
                    }
                    let reload_source_revision = if ui_events_enabled
                        && event.status == ReloadStatus::Installed
                        && matches!(event.target, WatchTarget::Score(_))
                    {
                        ui_generation_sources
                            .get(&event.generation_after)
                            .map(|(revision, _)| revision.clone())
                    } else {
                        None
                    };
                    let device_report = live_device_report_value(device.report());
                    output.event(
                        LiveDetail::Essential,
                        serde_json::json!({
                            "reload": {
                                "target": match event.target {
                                    WatchTarget::Score(_) => "score",
                                    WatchTarget::Prebake => "prebake",
                                },
                                "stream_id": device.stream_id(),
                                "path": event.path.display().to_string(),
                                "status": match event.status {
                                    ReloadStatus::Installed => "installed",
                                    ReloadStatus::Rejected => "rejected",
                                },
                                "generation_before": event.generation_before,
                                "generation_after": event.generation_after,
                                "source_revision": reload_source_revision,
                                "error_kind": event.error_kind,
                                "message": event.message,
                                "device": device_report,
                            }
                        }),
                    );
                }
            }
        }
        if let Some(sink) = ui_sink.as_ref() {
            // Diagnostics and UI events use separate streams so a blocked UI
            // writer cannot hold stderr's process-global lock in front of the
            // producer. Clients must use generation and revision gates rather
            // than assume cross-stream delivery order.
            emit_ui_trace_batches(
                sink,
                session,
                &device,
                ready_ui_traces,
                &ui_generation_sources,
                &mut ui_events_dropped,
            );
        }
        if stopped {
            break Err(RuntimeError::Cancelled);
        }
        std::thread::sleep(POLL);
    };
    let stop_acknowledged = device.stop_and_wait(Duration::from_secs(1));
    #[cfg(feature = "midi")]
    {
        // Close callbacks before advancing the key epoch; otherwise a driver
        // callback racing shutdown could repopulate the just-cleared ring.
        drop(midi_inputs);
        session.midi_input_bus().clear_keys();
    }
    #[cfg(feature = "midi")]
    let midi_shutdown_complete = midi_outputs.shutdown(Duration::from_millis(500));
    #[cfg(feature = "midi")]
    if !midi_shutdown_complete {
        output.event(
            LiveDetail::Essential,
            serde_json::json!({
                "live_error": {
                    "kind": "midi",
                    "message": "MIDI emergency silence exceeded the 500ms shutdown deadline; a wedged driver is retiring in the background",
                    "recoverable": true,
                    "stream_id": device.stream_id(),
                }
            }),
        );
    }
    // Every live-loop exit finalizes the capture without changing its outcome.
    if let (Some(path), Some((buffer, _))) = (
        std::env::var_os("RUSTEL_DEBUG_CAPTURE"),
        debug_capture.as_ref(),
    ) {
        let sr = device.sample_rate();
        let n = buffer.len();
        let mut bytes = Vec::with_capacity(44 + n * 2);
        bytes.extend_from_slice(b"RIFF");
        bytes.extend_from_slice(&((36 + n * 2) as u32).to_le_bytes());
        bytes.extend_from_slice(b"WAVEfmt ");
        bytes.extend_from_slice(&16u32.to_le_bytes());
        bytes.extend_from_slice(&1u16.to_le_bytes());
        bytes.extend_from_slice(&1u16.to_le_bytes());
        bytes.extend_from_slice(&sr.to_le_bytes());
        bytes.extend_from_slice(&(sr * 2).to_le_bytes());
        bytes.extend_from_slice(&2u16.to_le_bytes());
        bytes.extend_from_slice(&16u16.to_le_bytes());
        bytes.extend_from_slice(b"data");
        bytes.extend_from_slice(&((n * 2) as u32).to_le_bytes());
        for sample in buffer {
            let clamped = (sample.clamp(-1.0, 1.0) * 32767.0) as i16;
            bytes.extend_from_slice(&clamped.to_le_bytes());
        }
        if let Err(error) = std::fs::write(&path, bytes) {
            output.event(
                LiveDetail::Essential,
                serde_json::json!({
                    "debug_capture": {
                        "path": path.to_string_lossy(),
                        "message": format!("could not write the capture: {error}"),
                    }
                }),
            );
        }
    }
    let report = device.report();
    drop(device);
    session.consume_audio_confirmations();
    let producer_report = producer.producer_load_snapshot();
    let outcome_kind = result
        .as_ref()
        .err()
        .map(RuntimeError::kind)
        .unwrap_or("success");
    #[cfg(feature = "midi")]
    let midi_report = {
        let report = midi_outputs.report();
        Some(serde_json::json!({
            "sent": report.sent,
            "dropped_late": report.dropped_late,
            "refused_full": report.refused_full,
            "send_errors": report.send_errors,
            "errors_dropped": report.errors_dropped,
            "shutdown_complete": midi_shutdown_complete,
        }))
    };
    #[cfg(not(feature = "midi"))]
    let midi_report: Option<serde_json::Value> = None;
    output.event(
        LiveDetail::Essential,
        serde_json::json!({
            "live": {
                "status": "stopped",
                // Whether a signal ended this, so a reader knows if the stop
                // has already spoken for itself.
                "interrupted": interrupted_by().is_some(),
                "outcome_kind": outcome_kind,
                "stream_id": report.stream_id,
                "wall_elapsed_nanos": u64::try_from(wall_started.elapsed().as_nanos()).unwrap_or(u64::MAX),
                "submitted_frames": report.submitted_frames,
                "playhead_nanos": report.playhead_nanos,
                "buffer_playback_nanos": report.buffer_playback_nanos,
                "playback_latency_nanos": report.playback_latency_nanos,
                "max_playback_latency_nanos": report.max_playback_latency_nanos,
                "generation": report.generation,
                "callbacks": report.callbacks,
                "callback_deadline_misses": report.realtime_load.callbacks_over_100_percent,
                "accepted_events": report.accepted_events,
                "stale_events_filtered": report.stale_events_filtered,
        "late_events": report.late_events,
        "max_callback_gap_ms": report.max_callback_gap_nanos as f64 / 1e6,
                "refused_voices": report.refused_voices,
                "callback_errors": report.callback_errors,
                "callback_scope_misses": report.callback_scope_misses,
                "callback_allocations": report.callback_allocations,
                "callback_frees": report.callback_frees,
                "allocator_tripwire_armed": allocator_tripwire_armed,
                "stop_acknowledged": stop_acknowledged && report.stop_acknowledged,
                "ring_refusals": report.ring_refusals,
                "ring_peak_depth": report.ring_peak_depth,
                "producer_atomic_refusals": producer_report.atomic_refusals,
                "producer_committed_refusals": producer_report.committed_refusals,
                "cutover_race_blocks": report.cutover_race_blocks,
                "midi": midi_report,
                "realtime_metrics": "available"
            }
        }),
    );
    // Give the UI writer a bounded grace period to flush its final records.
    // A client that stopped reading cannot block shutdown: a still-busy
    // writer detaches and the process exits without it.
    if let Some(sink) = ui_sink {
        sink.finish_timeout(Duration::from_millis(250));
    }
    result
}

#[cfg(feature = "device-audio")]
fn live_device_report_value(report: rustel_audio::LiveDeviceReport) -> serde_json::Value {
    serde_json::json!({
        "stream_id": report.stream_id,
        "submitted_frames": report.submitted_frames,
        "playhead_nanos": report.playhead_nanos,
        "buffer_playback_nanos": report.buffer_playback_nanos,
        "playback_latency_nanos": report.playback_latency_nanos,
        "max_playback_latency_nanos": report.max_playback_latency_nanos,
        "generation": report.generation,
        "callbacks": report.callbacks,
        "accepted_events": report.accepted_events,
        "stale_events_filtered": report.stale_events_filtered,
        "late_events": report.late_events,
        "max_callback_gap_ms": report.max_callback_gap_nanos as f64 / 1e6,
        "refused_voices": report.refused_voices,
        "callback_errors": report.callback_errors,
        "callback_scope_misses": report.callback_scope_misses,
        "callback_allocations": report.callback_allocations,
        "callback_frees": report.callback_frees,
        "stop_acknowledged": report.stop_acknowledged,
        "ring_refusals": report.ring_refusals,
        "ring_peak_depth": report.ring_peak_depth,
        "cutover_race_blocks": report.cutover_race_blocks,
    })
}

#[cfg(feature = "device-audio")]
pub(super) fn runtime_device_error(error: rustel_audio::DevicePlaybackError) -> RuntimeError {
    match error {
        rustel_audio::DevicePlaybackError::Unavailable(message) => RuntimeError::Audio(message),
        rustel_audio::DevicePlaybackError::ResourceLimit(message) => {
            RuntimeError::ResourceLimit(message)
        }
        rustel_audio::DevicePlaybackError::Cancelled => RuntimeError::Cancelled,
    }
}

// Same signature as the device-audio build on purpose; see that one.
#[cfg(not(feature = "device-audio"))]
#[allow(clippy::too_many_arguments)]
pub(super) fn play_live(
    _session: &mut Session,
    input: &SourceInput,
    _reload: bool,
    _duration_secs: Option<f64>,
    loaded: &LoadedLiveSources,
    _initial_error: Option<RuntimeError>,
    _recorder: Option<rustel_runtime::session_log::SessionRecorder>,
    _extra_sounds: Vec<String>,
    _follow: bool,
    _announce_score: bool,
    _midi_virtual: Vec<String>,
    _midi_clock_out: Option<String>,
    _midi_clock_in: Option<String>,
    _audio_input: Option<String>,
    _buffer_frames: Option<u32>,
    _ui_events: bool,
    _first_install_from_zero: bool,
    // Dropped unsent, so a replay's delivery thread exits.
    _anchored: Option<std::sync::mpsc::Sender<()>>,
    _output: LiveOutput,
) -> Result<(), RuntimeError> {
    // Keep the default-build product boundary structurally coupled to the same
    // loaded setup/score bundle even though it cannot construct the CPAL
    // producer. Feature-gated tests exercise the actual handoff.
    let _loaded_identity = (&loaded.score, &loaded.prebake);
    let _ = watch_file(input)?;
    Err(RuntimeError::Audio(
        "live watch output is not compiled; rebuild rustel with --features device-audio".into(),
    ))
}

#[cfg(feature = "device-audio")]
pub(super) fn play(
    session: &mut Session,
    duration: f64,
    device_audio: bool,
) -> Result<rustel_runtime::PlayReport, RuntimeError> {
    if device_audio {
        session.play_on_device(duration)
    } else {
        session.play(duration)
    }
}

#[cfg(not(feature = "device-audio"))]
pub(super) fn play(
    session: &mut Session,
    duration: f64,
    device_audio: bool,
) -> Result<rustel_runtime::PlayReport, RuntimeError> {
    if device_audio {
        Err(RuntimeError::Audio(
            "device output is not compiled; rebuild rustel with --features device-audio".into(),
        ))
    } else {
        session.play(duration)
    }
}
