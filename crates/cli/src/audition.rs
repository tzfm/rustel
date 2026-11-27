//! Immediate sample and synth audition for CLI one-shot, persistent stdio,
//! and live `ui_control` sessions.
//!
//! All audio voice resolution, choke-group isolation, and non-blocking load
//! retries live in [`rustel_runtime::audition`]; this module validates CLI and
//! wire inputs and drives the producer loop.

use super::*;

#[cfg(feature = "device-audio")]
use rustel_runtime::audition::{
    AUDITION_LOAD_TIMEOUT, AuditionPoll, AuditionRequest, AuditionStart, AuditionTracker,
    MAX_AUDITION_DURATION_SECS, MAX_AUDITION_GAIN, MAX_AUDITION_NOTES, MIN_AUDITION_DURATION_SECS,
    parse_audition_note,
};

#[cfg(feature = "device-audio")]
pub(super) fn build_audition_request(
    sound: &str,
    variant: Option<f64>,
    note: Option<&UiAuditionNote>,
    notes: Option<&[UiAuditionNote]>,
    gain: Option<f32>,
    duration: Option<f64>,
) -> Result<AuditionRequest, String> {
    let sound = sound.trim();
    if sound.is_empty() {
        return Err("sound name cannot be empty".to_owned());
    }
    if sound.len() > rustel_runtime::ui_events::MAX_UI_LAYOUT_ID_BYTES {
        return Err("sound name is too long".to_owned());
    }
    if let Some(index) = variant
        && !index.is_finite()
    {
        return Err("variant index n must be finite".to_owned());
    }
    if note.is_some() && notes.is_some() {
        return Err("provide either note or notes, not both".to_owned());
    }
    let parsed_notes = if let Some(single) = note {
        vec![single.parse_midi()?]
    } else if let Some(list) = notes {
        if list.len() > MAX_AUDITION_NOTES {
            return Err(format!(
                "too many audition notes ({}); maximum is {MAX_AUDITION_NOTES}",
                list.len()
            ));
        }
        let mut parsed = Vec::with_capacity(list.len());
        for item in list {
            parsed.push(item.parse_midi()?);
        }
        parsed
    } else {
        Vec::new()
    };
    let gain = gain.unwrap_or(1.0);
    if !gain.is_finite() || !(0.0..=MAX_AUDITION_GAIN).contains(&gain) {
        return Err(format!(
            "gain must be finite within 0..={MAX_AUDITION_GAIN}"
        ));
    }
    if let Some(secs) = duration
        && (!secs.is_finite()
            || !(MIN_AUDITION_DURATION_SECS..=MAX_AUDITION_DURATION_SECS).contains(&secs))
    {
        return Err(format!(
            "duration must be finite within {MIN_AUDITION_DURATION_SECS}..={MAX_AUDITION_DURATION_SECS} seconds"
        ));
    }
    Ok(AuditionRequest {
        sound: sound.to_owned(),
        variant,
        notes: parsed_notes,
        gain,
        duration_secs: duration,
        step_secs: 0.0,
        slot: 0,
        at: None,
        ui_visuals: 0,
    })
}

/// Apply one `audition` or `audition_stop` command from `ui_control` stdin and
/// return its optional `ui_control_ack` envelope.
#[cfg(feature = "device-audio")]
#[allow(clippy::too_many_arguments)]
pub(super) fn apply_ui_audition_command(
    kind: &UiDiscreteControlKind,
    request: Option<String>,
    library: Option<&rustel_runtime::samples::SampleLibrary>,
    access: &ScoreSampleAccess,
    device: &rustel_audio::LiveScalarDevice,
    retained_samples: &std::collections::HashMap<
        rustel_audio::SampleId,
        rustel_audio::DecodedSample,
    >,
    cps: f64,
    generation: u64,
    tracker: &mut AuditionTracker,
) -> Option<rustel_runtime::ui_events::UiControlAckEnvelope> {
    let make_ack = |accepted: bool, message: Option<String>| {
        request.map(|id| {
            rustel_runtime::ui_events::UiControlAckEnvelope::new(id, accepted, generation, message)
        })
    };

    match kind {
        UiDiscreteControlKind::Audition(cmd) => {
            let Some(library) = library else {
                return make_ack(false, Some("sample library is unavailable".into()));
            };
            if let Some(source) = cmd.source.as_deref()
                && source.len() > rustel_runtime::ui_events::MAX_UI_LAYOUT_OPTIONS_BYTES
            {
                return make_ack(false, Some("source is too long".into()));
            }
            let built = match build_audition_request(
                &cmd.sound,
                cmd.n,
                cmd.note.as_ref(),
                cmd.notes.as_deref(),
                cmd.gain,
                cmd.duration,
            ) {
                Ok(built) => built,
                Err(message) => return make_ack(false, Some(message)),
            };
            match tracker.request(
                built,
                cmd.source.as_deref(),
                library,
                access,
                device,
                retained_samples,
                cps,
            ) {
                Ok(AuditionStart::Played) => make_ack(true, None),
                Ok(AuditionStart::Loading) => make_ack(true, Some("loading".into())),
                Err(error) => make_ack(false, Some(error.to_string())),
            }
        }
        UiDiscreteControlKind::AuditionStop => {
            tracker.stop(device, 0);
            make_ack(true, None)
        }
        _ => make_ack(false, Some("unsupported audition command".into())),
    }
}

#[cfg(feature = "device-audio")]
fn audition_output_options(
    dispatch: rustel_audio::DspDispatch,
    buffer_frames: Option<u32>,
) -> rustel_audio::LiveOutputOptions {
    let options = rustel_audio::LiveOutputOptions::default().with_dispatch(dispatch);
    match buffer_frames {
        Some(frames) => {
            options.with_buffer_preference(rustel_audio::AudioBufferPreference::Frames(frames))
        }
        None => options,
    }
}

#[cfg(feature = "device-audio")]
fn open_audition_device(
    output: Option<&str>,
    dispatch: rustel_audio::DspDispatch,
    buffer_frames: Option<u32>,
) -> Result<rustel_audio::LiveScalarDevice, RuntimeError> {
    let mut device = rustel_audio::LiveScalarDevice::start_output_with_options(
        output,
        1,
        audition_output_options(dispatch, buffer_frames),
    )
    .map_err(runtime_device_error)?;
    if !device.arm_callback_tripwire() {
        device.stop();
        return Err(RuntimeError::Audio(
            "audio callback allocator/reporting tripwire is not armed".into(),
        ));
    }
    Ok(device)
}

#[cfg(feature = "device-audio")]
#[allow(clippy::too_many_arguments)]
pub(super) fn run_samples_audition(
    sound: Option<String>,
    stdio: bool,
    source: Option<String>,
    notes: Vec<String>,
    gain: f32,
    duration: Option<f64>,
    output: Option<String>,
    buffer_frames: Option<u32>,
    sample_access: SampleAccessArgs,
    json: bool,
    dispatch: rustel_audio::DspDispatch,
) -> Result<(), RuntimeError> {
    let access = with_sample_access(session_config(dispatch), &sample_access)?.score_sample_access;
    if stdio {
        return run_samples_audition_stdio(source, output, buffer_frames, access, dispatch);
    }
    let sound = sound
        .ok_or_else(|| RuntimeError::Message("provide a sound name or pass --stdio".into()))?;
    run_samples_audition_once(
        &sound,
        source.as_deref(),
        &notes,
        gain,
        duration,
        output.as_deref(),
        buffer_frames,
        &access,
        json,
        dispatch,
    )
}

#[cfg(feature = "device-audio")]
#[allow(clippy::too_many_arguments)]
fn run_samples_audition_once(
    sound: &str,
    source: Option<&str>,
    notes: &[String],
    gain: f32,
    duration: Option<f64>,
    output: Option<&str>,
    buffer_frames: Option<u32>,
    access: &ScoreSampleAccess,
    json: bool,
    dispatch: rustel_audio::DspDispatch,
) -> Result<(), RuntimeError> {
    use std::time::{Duration, Instant};

    const POLL: Duration = Duration::from_millis(2);
    const DEFAULT_CPS: f64 = 0.5;

    let parsed_notes = if notes.is_empty() {
        None
    } else {
        if notes.len() > MAX_AUDITION_NOTES {
            return Err(RuntimeError::Message(format!(
                "too many audition notes ({}); maximum is {MAX_AUDITION_NOTES}",
                notes.len()
            )));
        }
        let mut parsed = Vec::with_capacity(notes.len());
        for note in notes {
            let midi = parse_audition_note(note).map_err(RuntimeError::Message)?;
            parsed.push(UiAuditionNote::Number(midi));
        }
        Some(parsed)
    };
    let request = build_audition_request(
        sound,
        None,
        None,
        parsed_notes.as_deref(),
        Some(gain),
        duration,
    )
    .map_err(RuntimeError::Message)?;

    let library = if let Some(spec) = source {
        rustel_runtime::samples::SampleLibrary::load_for_sources(
            &[spec.to_owned()],
            access,
            AUDITION_LOAD_TIMEOUT,
            || interrupted_by().is_some(),
        )
        .map_err(RuntimeError::Message)?
    } else {
        let library = rustel_runtime::samples::SampleLibrary::load_default_sources_async(None)
            .map_err(|error| {
                RuntimeError::Message(format!("the sample library could not start: {error}"))
            })?;
        library.set_direct_diagnostic_logging(false);
        library
    };

    let device = open_audition_device(output, dispatch, buffer_frames)?;
    library.set_render_rate(device.sample_rate());
    let mut retained_samples = std::collections::HashMap::new();
    let mut tracker = AuditionTracker::new();

    let start = tracker.request(
        request,
        source,
        &library,
        access,
        &device,
        &retained_samples,
        DEFAULT_CPS,
    );
    let mut played = match start {
        Ok(AuditionStart::Played) => true,
        Ok(AuditionStart::Loading) => false,
        Err(error) => {
            device.stop_and_wait(Duration::from_millis(250));
            return Err(error);
        }
    };

    while !played {
        if interrupted_by().is_some() {
            device.stop_and_wait(Duration::from_millis(250));
            return Err(RuntimeError::Cancelled);
        }
        if let Err(error) = device.check_health() {
            device.stop();
            return Err(runtime_device_error(error));
        }
        install_ready_samples(&library, &mut retained_samples, |id, decoded| {
            device.install_sample(id, decoded)
        });
        device.reclaim_samples();
        match tracker.poll(&library, &device, &retained_samples, DEFAULT_CPS) {
            AuditionPoll::Played => played = true,
            AuditionPoll::Failed { message, .. } => {
                device.stop_and_wait(Duration::from_millis(250));
                return Err(RuntimeError::Message(message));
            }
            AuditionPoll::Idle | AuditionPoll::Pending => std::thread::sleep(POLL),
        }
    }

    // Let the auditioned voice finish before closing the audio stream.
    let horizon = tracker.horizon_frame();
    let cap_secs = duration.unwrap_or(MAX_AUDITION_DURATION_SECS) + 1.0;
    let deadline = Instant::now() + Duration::from_secs_f64(cap_secs);
    while device.clock_frames() < horizon && Instant::now() < deadline {
        if interrupted_by().is_some() {
            device.stop_and_wait(Duration::from_millis(250));
            return Err(RuntimeError::Cancelled);
        }
        if let Err(error) = device.check_health() {
            device.stop();
            return Err(runtime_device_error(error));
        }
        std::thread::sleep(POLL);
    }

    // Allow queued audio latency to flush to speakers before closing.
    let latency_nanos = device.report().playback_latency_nanos;
    if latency_nanos > 0 {
        let flush = Duration::from_nanos(latency_nanos).min(Duration::from_millis(250));
        let flush_deadline = Instant::now() + flush;
        while Instant::now() < flush_deadline && interrupted_by().is_none() {
            std::thread::sleep(POLL);
        }
    }
    let played_secs = tracker.last_duration_secs().min(cap_secs);
    device.stop_and_wait(Duration::from_millis(250));

    if json {
        outln!(
            "{}",
            serde_json::json!({
                "audition": {
                    "sound": sound,
                    "source": source,
                    "played": true,
                    "duration_secs": (played_secs * 1000.0).round() / 1000.0,
                }
            })
        );
    } else if !quiet_asked() {
        let on = style::stdout_on();
        outln!(
            "{} {}",
            style::green(on, "played"),
            style::safe_source(sound),
        );
    }
    Ok(())
}

#[cfg(feature = "device-audio")]
fn run_samples_audition_stdio(
    initial_source: Option<String>,
    output: Option<String>,
    buffer_frames: Option<u32>,
    access: ScoreSampleAccess,
    dispatch: rustel_audio::DspDispatch,
) -> Result<(), RuntimeError> {
    use std::time::Duration;

    const POLL: Duration = Duration::from_millis(2);
    const DEFAULT_CPS: f64 = 0.5;

    let library = rustel_runtime::samples::SampleLibrary::load_default_sources_async(None)
        .map_err(|error| {
            RuntimeError::Message(format!("the sample library could not start: {error}"))
        })?;
    library.set_direct_diagnostic_logging(false);
    if let Some(spec) = initial_source.as_deref() {
        library
            .look_up_samples_source(spec, &access)
            .map_err(RuntimeError::Message)?;
    }

    let ui_sink = rustel_runtime::ui_events::UiEventSink::stdout().map_err(|error| {
        RuntimeError::Message(format!(
            "could not start the audition output writer: {error}"
        ))
    })?;
    let ui_controls = UiControlInbox::stdin().map_err(|error| {
        RuntimeError::Message(format!(
            "could not start the audition stdin reader: {error}"
        ))
    })?;

    let device = open_audition_device(output.as_deref(), dispatch, buffer_frames)?;
    library.set_render_rate(device.sample_rate());
    let mut retained_samples = std::collections::HashMap::new();
    let mut tracker = AuditionTracker::new();

    if !quiet_asked() {
        eprintln!(
            "{}",
            serde_json::json!({
                "audition_stdio": {
                    "status": "ready",
                    "stream_id": device.stream_id(),
                    "device": device.name(),
                    "sample_rate": device.sample_rate(),
                }
            })
        );
    }

    let result = loop {
        if interrupted_by().is_some() {
            break Err(RuntimeError::Cancelled);
        }
        if let Err(error) = device.check_health() {
            break Err(runtime_device_error(error));
        }

        install_ready_samples(&library, &mut retained_samples, |id, decoded| {
            device.install_sample(id, decoded)
        });
        device.reclaim_samples();

        let (commands, stdin_eof) = ui_controls.drain_commands_with_eof();
        let mut stop_requested = false;
        for command in commands {
            let generation = device.generation();
            let ack = match &command.kind {
                UiDiscreteControlKind::Audition(_) | UiDiscreteControlKind::AuditionStop => {
                    apply_ui_audition_command(
                        &command.kind,
                        command.request,
                        Some(&library),
                        &access,
                        &device,
                        &retained_samples,
                        DEFAULT_CPS,
                        generation,
                        &mut tracker,
                    )
                }
                UiDiscreteControlKind::Transport {
                    action: UiTransportAction::Stop,
                } => {
                    tracker.stop(&device, 0);
                    stop_requested = true;
                    command.request.map(|request| {
                        rustel_runtime::ui_events::UiControlAckEnvelope::new(
                            request, true, generation, None,
                        )
                    })
                }
                _ => command.request.map(|request| {
                    rustel_runtime::ui_events::UiControlAckEnvelope::new(
                        request,
                        false,
                        generation,
                        Some("unsupported command in standalone audition mode".into()),
                    )
                }),
            };
            if let Some(ack) = ack {
                let _ = ui_sink.try_send_control_ack(ack);
            }
            if stop_requested {
                break;
            }
        }
        if stop_requested || stdin_eof {
            break Ok(());
        }

        if let AuditionPoll::Failed { sound, message } =
            tracker.poll(&library, &device, &retained_samples, DEFAULT_CPS)
            && !quiet_asked()
        {
            eprintln!(
                "{}",
                serde_json::json!({
                    "live_error": {
                        "kind": "audition",
                        "sound": sound,
                        "message": message,
                        "recoverable": true,
                        "stream_id": device.stream_id(),
                    }
                })
            );
        }

        std::thread::sleep(POLL);
    };

    device.stop_and_wait(Duration::from_millis(250));
    ui_sink.finish_timeout(Duration::from_millis(250));
    result
}

#[cfg(not(feature = "device-audio"))]
#[allow(clippy::too_many_arguments)]
pub(super) fn run_samples_audition(
    _sound: Option<String>,
    _stdio: bool,
    _source: Option<String>,
    _notes: Vec<String>,
    _gain: f32,
    _duration: Option<f64>,
    _output: Option<String>,
    _buffer_frames: Option<u32>,
    _sample_access: SampleAccessArgs,
    _json: bool,
    _dispatch: rustel_audio::DspDispatch,
) -> Result<(), RuntimeError> {
    Err(RuntimeError::Audio(
        "sample audition is not compiled; rebuild rustel with --features device-audio".into(),
    ))
}
