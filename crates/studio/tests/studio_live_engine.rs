//! The studio engine on the headless `silent` output: the real producer,
//! the real device ring, wall-clock paced. The audio callback tripwire
//! allocator must be the process allocator, which only a binary can
//! install - hence an integration test rather than a unit test.

use std::sync::mpsc::TryRecvError;
use std::time::{Duration, Instant};

use rustel_runtime::samples::SoundReadiness;
use rustel_studio::engine::{
    Launch, StudioConfig, StudioDiagnosticLevel, StudioEngine, StudioSnapshot, StudioTick,
    StudioUpdate, StudioUpdateSendResult,
};
use rustel_studio::{RecordingReply, StudioControlEvent, StudioWorker};

#[global_allocator]
static AUDIO_CALLBACK_ALLOCATOR: rustel_audio::tripwire::TripwireAlloc =
    rustel_audio::tripwire::TripwireAlloc;

fn accepted_update(update: StudioUpdate) -> StudioUpdateSendResult {
    let _ = update;
    Ok(())
}

/// The tripwire is armed per process, so one engine at a time: each test
/// holds this for its whole run.
static ONE_ENGINE: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn one_engine() -> std::sync::MutexGuard<'static, ()> {
    ONE_ENGINE
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
}

fn reply(worker: &StudioWorker, request_id: u64, stopped: &mut bool) -> RecordingReply {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match worker.try_recv_control() {
            Ok(StudioControlEvent::Recording(outcome)) => {
                assert_eq!(outcome.request_id, Some(request_id));
                return outcome.result.expect("recording command succeeded");
            }
            Ok(StudioControlEvent::Stopped(stop)) => {
                assert!(stop.acknowledged);
                *stopped = true;
            }
            Ok(StudioControlEvent::EngineFailure(error)) => panic!("{error:?}"),
            Err(TryRecvError::Disconnected) => panic!("worker disconnected"),
            _ => {}
        }
        assert!(Instant::now() < deadline, "recording reply timed out");
        std::thread::sleep(Duration::from_millis(2));
    }
}

#[test]
fn recording_commands_return_the_joined_take_on_the_silent_output() {
    let _engine = one_engine();
    let directory = tempfile::tempdir_in(env!("CARGO_MANIFEST_DIR")).unwrap();
    let path = directory.path().join("take.wav");
    assert!(!path.exists());
    let mut worker = StudioWorker::spawn(
        StudioConfig {
            output: Some("silent".into()),
            poll_interval: Duration::from_millis(2),
            ..Default::default()
        },
        #[cfg(feature = "hydra")]
        rustel_runtime::hydra::HydraBridge::new(),
    )
    .expect("worker");
    let mut stopped = false;
    let capture_id = worker.try_record(Some(path.clone())).expect("start queued");
    assert!(
        matches!(reply(&worker, capture_id, &mut stopped), RecordingReply::Started {
        capture_id: received,
    } if received == capture_id)
    );

    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match worker.try_recv_control() {
            Ok(StudioControlEvent::Snapshot(snapshot)) => {
                if let Some(recording) = snapshot.recording {
                    assert_eq!(recording.path, path);
                    assert_eq!(recording.error, None);
                    if recording.seconds > 0.0 {
                        break;
                    }
                }
            }
            Ok(StudioControlEvent::Recording(outcome)) => panic!("duplicate reply: {outcome:?}"),
            Ok(StudioControlEvent::EngineFailure(error)) => panic!("{error:?}"),
            Err(TryRecvError::Disconnected) => panic!("worker disconnected"),
            _ => {}
        }
        assert!(Instant::now() < deadline, "recording made no progress");
        std::thread::sleep(Duration::from_millis(2));
    }
    let stop_id = worker.try_record(None).expect("stop queued");
    assert!(stop_id > capture_id);
    // File closure and audio Stop have independent acknowledgements. This
    // public startup/restart test uses normal file I/O, not a held writer.
    worker.request_stop();
    let RecordingReply::Finished {
        capture_id: received,
        status,
    } = reply(&worker, stop_id, &mut stopped)
    else {
        panic!("explicit stop must return the joined take");
    };
    assert_eq!(received, capture_id);
    assert_eq!(status.path, path);
    assert_eq!(status.error, None);
    assert!(status.frames > 0);
    let signal = status
        .final_signal
        .expect("writer joined with its exact summary");
    assert_eq!(signal.sample_count, status.frames * 2);
    assert_eq!(signal.nonfinite_count, 0);
    assert_eq!(signal.finite_peak, 0.0, "the empty score remains silent");
    let bytes = std::fs::read(&path).unwrap();
    assert_eq!(bytes.len() as u64, status.bytes);
    assert_eq!(status.bytes, 44 + status.frames * 6);
    assert_eq!(&bytes[..4], b"RIFF");
    assert_eq!(
        u32::from_le_bytes(bytes[40..44].try_into().unwrap()) as u64,
        status.frames * 6
    );
    let deadline = Instant::now() + Duration::from_secs(5);
    while !stopped {
        match worker.try_recv_control() {
            Ok(StudioControlEvent::Stopped(stop)) => {
                assert!(stop.acknowledged);
                stopped = true;
            }
            Ok(StudioControlEvent::Recording(outcome)) => panic!("duplicate reply: {outcome:?}"),
            Ok(StudioControlEvent::EngineFailure(error)) => panic!("{error:?}"),
            Err(TryRecvError::Disconnected) => panic!("worker disconnected"),
            _ => {}
        }
        assert!(Instant::now() < deadline, "audio Stop was not acknowledged");
        std::thread::sleep(Duration::from_millis(2));
    }
    let repeated_stop = worker.try_record(None).expect("second stop queued");
    assert!(repeated_stop > stop_id);
    assert!(matches!(
        reply(&worker, repeated_stop, &mut stopped),
        RecordingReply::NoActiveTake
    ));
    let next_path = directory.path().join("restarted.wav");
    let next_capture = worker
        .try_record(Some(next_path.clone()))
        .expect("restart queued");
    assert!(next_capture > repeated_stop);
    assert!(matches!(reply(&worker, next_capture, &mut stopped),
        RecordingReply::Started { capture_id } if capture_id == next_capture));
    let next_stop = worker.try_record(None).expect("second take close queued");
    assert!(matches!(reply(&worker, next_stop, &mut stopped),
        RecordingReply::Finished { capture_id, status }
        if capture_id == next_capture && status.path == next_path
            && status.error.is_none() && status.final_signal.is_some()));
    worker.shutdown();
}

/// Native audition PCM reaches the joined take between silent margins. This
/// covers ordinary recording, not exact UI-interval alignment or loss placement.
#[test]
fn recording_a_native_audition_preserves_nonzero_interior_pcm() {
    let _engine = one_engine();
    // On assertion failure the worker drops before the directory, stopping
    // audio and joining the writer before temporary-file cleanup. Disk cleanup
    // still has the existing blocking contract and needs the outer test timeout.
    let directory = tempfile::tempdir_in(env!("CARGO_MANIFEST_DIR")).unwrap();
    let path = directory.path().join("audition.wav");
    assert!(!path.exists());
    let mut worker = StudioWorker::spawn(
        StudioConfig {
            output: Some("silent".into()),
            poll_interval: Duration::from_millis(2),
            ..Default::default()
        },
        #[cfg(feature = "hydra")]
        rustel_runtime::hydra::HydraBridge::new(),
    )
    .expect("worker");
    let mut stopped = false;
    let capture_id = worker.try_record(Some(path.clone())).expect("start queued");
    assert!(matches!(
        reply(&worker, capture_id, &mut stopped),
        RecordingReply::Started { capture_id: received } if received == capture_id
    ));
    assert!(!stopped);
    // Startup has already run its intentional allocator canary. Global
    // counters are never reset; ONE_ENGINE excludes other live test owners.
    let callback_before = rustel_audio::tripwire::Violations::capture();

    let wait_progress = |minimum_seconds: f64, minimum_device_time: f64, require_signal: bool| {
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut heard = false;
        loop {
            let peak = worker.master().take_levels().peak;
            assert!(peak.is_finite());
            heard |= peak > 0.01;
            match worker.try_recv_control() {
                Ok(StudioControlEvent::Snapshot(snapshot)) => {
                    assert!(snapshot.playing && !snapshot.stopping);
                    assert!(snapshot.device_time.is_finite());
                    let recording = snapshot.recording.expect("the same take is still open");
                    assert_eq!(recording.path, path);
                    assert_eq!(recording.error, None);
                    assert_eq!(recording.dropped_seconds, 0.0);
                    assert!(recording.seconds.is_finite());
                    if recording.seconds >= minimum_seconds
                        && snapshot.device_time >= minimum_device_time
                        && (!require_signal || heard)
                    {
                        return (recording.seconds, snapshot.device_time);
                    }
                }
                Ok(StudioControlEvent::Recording(outcome)) => {
                    panic!("unexpected recording reply: {outcome:?}");
                }
                Ok(StudioControlEvent::Stopped(_)) => panic!("audio stopped during the take"),
                Ok(StudioControlEvent::EngineFailure(error)) => panic!("{error:?}"),
                Err(TryRecvError::Disconnected) => panic!("worker disconnected"),
                _ => {}
            }
            assert!(
                Instant::now() < deadline,
                "recording phase made no progress"
            );
            std::thread::sleep(Duration::from_millis(2));
        }
    };
    let leading = wait_progress(0.1, 0.1, false);
    assert_eq!(worker.master().take_levels().peak, 0.0);
    assert!(worker.try_audition("sine", 0.25), "audition queued");
    // The meter latch prevents a queued-but-unheard audition from advancing
    // this phase. The final decoded samples, not these clocks, prove the PCM.
    let sounding = wait_progress(leading.0 + 0.3, leading.1 + 0.3, true);
    assert!(worker.try_stop_audition(), "audition stop queued");
    // StopAudition schedules the existing choke at the device's lead time;
    // leave room for it and the fade while recording continues on silence.
    let _trailing = wait_progress(sounding.0 + 0.5, sounding.1 + 0.5, false);

    let stop_id = worker.try_record(None).expect("take close queued");
    assert!(stop_id > capture_id);
    worker.request_stop();
    let RecordingReply::Finished {
        capture_id: received,
        status,
    } = reply(&worker, stop_id, &mut stopped)
    else {
        panic!("explicit close must return the joined take");
    };
    assert_eq!(received, capture_id);
    assert_eq!(status.path, path);
    assert_eq!(status.error, None);
    let deadline = Instant::now() + Duration::from_secs(5);
    while !stopped {
        match worker.try_recv_control() {
            Ok(StudioControlEvent::Stopped(stop)) => {
                assert!(stop.acknowledged);
                stopped = true;
            }
            Ok(StudioControlEvent::Recording(outcome)) => panic!("duplicate reply: {outcome:?}"),
            Ok(StudioControlEvent::EngineFailure(error)) => panic!("{error:?}"),
            Err(TryRecvError::Disconnected) => panic!("worker disconnected"),
            _ => {}
        }
        assert!(Instant::now() < deadline, "audio Stop was not acknowledged");
        std::thread::sleep(Duration::from_millis(2));
    }
    worker.shutdown();
    let callback_delta = rustel_audio::tripwire::Violations::capture().since(callback_before);
    assert!(
        callback_delta.clean(),
        "callback allocation/free: {callback_delta:?}"
    );

    let signal = status
        .final_signal
        .expect("writer joined with its raw summary");
    assert_eq!(signal.sample_count, status.frames.checked_mul(2).unwrap());
    assert_eq!(signal.nonfinite_count, 0);
    assert!(signal.finite_peak.is_finite() && signal.finite_peak > 0.01);
    let bytes = std::fs::read(&path).unwrap();
    assert_eq!(bytes.len() as u64, status.bytes);
    assert_eq!(status.bytes, 44 + status.frames.checked_mul(6).unwrap());
    assert!(bytes.len() >= 44);
    assert_eq!(&bytes[..4], b"RIFF");
    assert_eq!(&bytes[8..12], b"WAVE");
    assert_eq!(&bytes[36..40], b"data");
    assert_eq!(
        u32::from_le_bytes(bytes[4..8].try_into().unwrap()) as u64 + 8,
        status.bytes
    );
    assert_eq!(
        u32::from_le_bytes(bytes[40..44].try_into().unwrap()) as u64,
        status.frames * 6
    );
    assert_eq!(u16::from_le_bytes(bytes[20..22].try_into().unwrap()), 1);
    assert_eq!(u16::from_le_bytes(bytes[34..36].try_into().unwrap()), 24);
    let decoded = rustel_audio::decode_wav(&bytes).expect("joined PCM24 WAV");
    assert_eq!(decoded.sample_rate(), status.sample_rate);
    assert_eq!(decoded.channels(), 2);
    assert_eq!(decoded.frames() as u64, status.frames);
    const MARGIN: usize = 1024;
    assert!(decoded.frames() >= MARGIN * 3);
    let mut audible_frames = [0usize; 2];
    let mut decoded_peak = 0.0f32;
    for frame in 0..decoded.frames() {
        let (left, right) = decoded.stereo_at(frame as f64).expect("complete frame");
        for (channel, sample) in [left, right].into_iter().enumerate() {
            assert!(sample.is_finite());
            decoded_peak = decoded_peak.max(sample.abs());
            if frame < MARGIN || frame >= decoded.frames() - MARGIN {
                assert_eq!(sample, 0.0, "non-silent margin at frame {frame}");
            } else if sample.abs() > 1e-4 {
                audible_frames[channel] += 1;
            }
        }
    }
    assert!(audible_frames.into_iter().all(|count| count >= MARGIN));
    assert!(decoded_peak > 0.01);
    // The raw summary precedes PCM24 quantization; its peak is not bit-exact
    // with the decoded peak. Neither summary nor snapshots certify no loss.
}

/// A playing score, then an unknown sound forced past the check, on the
/// headless output. The set keeps sounding: the last audible score comes
/// back, and the producer never latches into a terminal error.
#[test]
fn a_forced_unknown_sound_never_stops_the_set() {
    let _engine = one_engine();
    let config = StudioConfig {
        output: Some("silent".into()),
        poll_interval: Duration::from_millis(2),
        ..StudioConfig::default()
    };
    let mut engine = StudioEngine::new(config).expect("engine");
    engine
        .evaluate_and_start("setCps(2); note('c4').fast(8)", false)
        .expect("a playing score");
    let mut accepted_before = 0usize;
    let started = Instant::now();
    while started.elapsed() < Duration::from_millis(700) {
        if let Ok(StudioTick::Running { accepted_audio, .. }) = engine.tick(accepted_update) {
            accepted_before += accepted_audio;
        }
        std::thread::sleep(Duration::from_millis(3));
    }
    assert!(accepted_before > 0, "the first score never sounded");

    // The check is the studio's; the engine sees the score the reader
    // forced through. Its JavaScript is fine; every sound is unknown.
    engine
        .evaluate("setCps(2); s(\"sdb\").seg(8)", false)
        .expect("an unknown sound is not a JavaScript error");
    let mut accepted_after = 0usize;
    let mut errors = Vec::new();
    let started = Instant::now();
    while started.elapsed() < Duration::from_millis(2_500) {
        match engine.tick(|update| {
            if let StudioUpdate::Diagnostic(diagnostic) = &update
                && diagnostic.level == StudioDiagnosticLevel::Error
            {
                errors.push(diagnostic.message.clone());
            }
            Ok(())
        }) {
            Ok(StudioTick::Running { accepted_audio, .. }) => accepted_after += accepted_audio,
            Ok(other) => panic!("the set stopped: {other:?}"),
            Err(error) => panic!("the tick failed: {error}"),
        }
        std::thread::sleep(Duration::from_millis(3));
    }
    assert!(
        !errors
            .iter()
            .any(|message| message.contains("remains unpublished")),
        "the producer latched: {errors:?}"
    );
    assert!(
        accepted_after > 0,
        "the set went silent after the refused score: {errors:?}"
    );
    assert!(engine.is_playing(), "still playing");
    let _ = engine.stop(Duration::from_millis(200));
}

/// The reserved audition slot carries the preview alone. With a score
/// playing and nothing previewed, the master analysis has signal, and the
/// audition slot is armed and silent.
#[test]
fn the_audition_slot_carries_none_of_the_score() {
    let _engine = one_engine();
    use rustel_studio::engine::AUDITION_UI_VISUAL_SLOT;
    let config = StudioConfig {
        output: Some("silent".into()),
        poll_interval: Duration::from_millis(2),
        ..StudioConfig::default()
    };
    let mut engine = StudioEngine::new(config).expect("engine");
    engine
        .evaluate_and_start("setCps(2); note('c4').fast(8).gain(1)", false)
        .expect("a playing score");
    let mut master_peak = 0.0f32;
    let mut audition_peak = 0.0f32;
    let mut audition_frames = 0usize;
    let started = Instant::now();
    while started.elapsed() < Duration::from_millis(1_500) {
        engine
            .tick(|update| {
                if let StudioUpdate::Audio { analysis, .. } = &update {
                    master_peak = analysis
                        .master
                        .scope
                        .iter()
                        .fold(master_peak, |peak, sample| peak.max(sample.abs()));
                    for (slot, frame) in &analysis.visuals {
                        if *slot == AUDITION_UI_VISUAL_SLOT {
                            audition_frames += 1;
                            audition_peak = frame
                                .scope
                                .iter()
                                .fold(audition_peak, |peak, sample| peak.max(sample.abs()));
                        }
                    }
                }
                Ok(())
            })
            .expect("tick");
        std::thread::sleep(Duration::from_millis(3));
    }
    assert!(
        master_peak > 0.01,
        "the score is audible on the master: {master_peak}"
    );
    assert!(
        audition_frames > 0,
        "the audition slot is armed and reported"
    );
    assert_eq!(
        audition_peak, 0.0,
        "nothing of the score reaches the audition slot"
    );

    // Stopping a preview that is not sounding is harmless, and the set
    // plays on.
    engine.stop_audition();
    let mut accepted = 0usize;
    let started = Instant::now();
    while started.elapsed() < Duration::from_millis(400) {
        if let Ok(StudioTick::Running { accepted_audio, .. }) = engine.tick(accepted_update) {
            accepted += accepted_audio;
        }
        std::thread::sleep(Duration::from_millis(3));
    }
    assert!(accepted > 0, "still playing after a stop-preview");
    let _ = engine.stop(Duration::from_millis(200));
}

/// Beyond the audio's half-second horizon, the engine reads the score ahead
/// every interval. It ships the onsets to the painters as a preview batch,
/// flagged and with its own onset id space, under the sounding generation,
/// so the roll's future half is not blank.
#[test]
fn the_painters_are_told_what_is_coming_beyond_the_audio_horizon() {
    let _engine = one_engine();
    use rustel_runtime::PREVIEW_ONSET_ID_FLAG;
    let config = StudioConfig {
        output: Some("silent".into()),
        poll_interval: Duration::from_millis(2),
        ..StudioConfig::default()
    };
    let mut engine = StudioEngine::new(config).expect("engine");
    engine
        .evaluate_and_start("$: note(\"c4*8\")._pianoroll()", false)
        .expect("a playing score");
    // (preview_from_cycle, device_time, generation, traces)
    let mut previews = Vec::new();
    let mut audible = None;
    let started = Instant::now();
    while started.elapsed() < Duration::from_millis(1_500) {
        match engine.tick(|update| {
            if let StudioUpdate::Traces(request) = &update
                && let Some(from) = request.preview_from_cycle
            {
                previews.push((
                    from,
                    request.device_time,
                    request.generation,
                    request.traces.clone(),
                ));
            }
            Ok(())
        }) {
            Ok(StudioTick::Running {
                audible_generation, ..
            }) => audible = Some(audible_generation),
            Ok(other) => panic!("the set stopped: {other:?}"),
            Err(error) => panic!("the tick failed: {error}"),
        }
        std::thread::sleep(Duration::from_millis(3));
    }
    let audible = audible.expect("the set ran");
    assert!(!previews.is_empty(), "no preview batch in 1.5 s");
    let mut lead = 0.0f64;
    let mut traced = 0usize;
    for (from, device_time, generation, traces) in &previews {
        assert_eq!(
            *generation, audible,
            "a preview is under the sounding generation"
        );
        for trace in traces {
            traced += 1;
            assert!(
                trace.whole_begin.to_f64() >= *from,
                "an onset at {} sits before the audio's end {from}",
                trace.whole_begin.to_f64()
            );
            assert_ne!(
                trace.onset_id & PREVIEW_ONSET_ID_FLAG,
                0,
                "preview ids are flagged"
            );
            assert_eq!(trace.generation, audible);
            lead = lead.max(trace.target_time - device_time);
        }
    }
    assert!(traced > 0, "the previews carried no onsets");
    eprintln!(
        "preview lead {lead:.2} s over {traced} onsets in {} batches",
        previews.len()
    );
    assert!(
        lead > 1.5,
        "the furthest preview leads the clock by {lead:.2} s, not past the 0.5 s horizon"
    );

    // A new score: the next previews are its, never the old one's.
    let old = audible;
    engine
        .evaluate("$: note(\"e4*8\")._pianoroll()", false)
        .expect("the new score");
    let mut later = Vec::new();
    let mut audible = old;
    let started = Instant::now();
    while started.elapsed() < Duration::from_millis(1_200) {
        match engine.tick(|update| {
            if let StudioUpdate::Traces(request) = &update
                && request.preview_from_cycle.is_some()
            {
                later.push(request.generation);
            }
            Ok(())
        }) {
            Ok(StudioTick::Running {
                audible_generation, ..
            }) => audible = audible_generation,
            Ok(other) => panic!("the set stopped: {other:?}"),
            Err(error) => panic!("the tick failed: {error}"),
        }
        std::thread::sleep(Duration::from_millis(3));
    }
    assert!(audible > old, "the new score became audible");
    assert_eq!(
        later.last(),
        Some(&audible),
        "the last preview is the new score's"
    );
    let first_new = later.iter().position(|generation| *generation == audible);
    assert!(
        first_new.is_some_and(|at| later[at..].iter().all(|generation| *generation == audible)),
        "once the new score previews, the old one never does again: {later:?}"
    );
    let _ = engine.stop(Duration::from_millis(200));
}

#[test]
fn a_burst_of_slider_moves_makes_a_couple_of_flips_not_thirty() {
    let _engine = one_engine();
    let config = StudioConfig {
        output: Some("silent".into()),
        poll_interval: Duration::from_millis(2),
        ..StudioConfig::default()
    };
    let mut engine = StudioEngine::new(config).expect("engine");
    engine
        .evaluate_and_start(
            "setCps(2); note(\"c4*8\").gain(slider(0.5, 0, 1, 0.01))",
            false,
        )
        .expect("a playing score");
    let mut slider_id = None;
    let started = Instant::now();
    while started.elapsed() < Duration::from_millis(1_000) && slider_id.is_none() {
        engine
            .tick(|update| {
                if let StudioUpdate::Layout(envelope) = &update
                    && let Some(slider) = envelope.ui_layout.sliders.first()
                {
                    slider_id = Some(slider.id.clone());
                }
                Ok(())
            })
            .expect("tick");
        std::thread::sleep(Duration::from_millis(3));
    }
    let id = slider_id.expect("the layout names its slider");
    let generation_before = engine.generation();
    // Thirty moves in well under the interval, as a key repeat delivers.
    // Coalescing is time-based, so back-to-back calls are the strict form
    // of the burst: pacing them with a sleep would let a loaded machine
    // cross the requery interval and fail without any code change.
    for step in 0..30 {
        engine
            .set_slider(&id, 0.5 + f64::from(step) * 0.01)
            .expect("slider");
    }
    let during = engine.generation() - generation_before;
    assert!(during <= 2, "flips during the burst: {during}");
    // The interval passes: the last value gets its flip, and no more.
    let started = Instant::now();
    while started.elapsed() < Duration::from_millis(400) {
        engine.tick(accepted_update).expect("tick");
        std::thread::sleep(Duration::from_millis(3));
    }
    let total = engine.generation() - generation_before;
    assert!((1..=3).contains(&total), "flips in all: {total}");
    let _ = engine.stop(Duration::from_millis(200));
}

#[test]
fn a_direct_slider_after_pitch_transforms_updates_sustained_audio_before_the_next_onset() {
    let _engine = one_engine();
    let score =
        "note('c3').s('sine').gain(slider(0, 0, 1, .01)).scale('C:major').transpose(12).slow(16)";
    let mut inspection = rustel_runtime::Session::new().expect("inspection session");
    inspection.evaluate(score).expect("inspect score");
    let events = inspection
        .schedule_audio_through(0.0, 0.1, 48_000)
        .expect("inspect audio events");
    assert!(!events.is_empty());
    assert_ne!(
        events[0].controls.live_controls[0], 0,
        "runtime voice conversion must retain the gain binding"
    );
    let mut engine = StudioEngine::new(StudioConfig {
        output: Some("silent".into()),
        poll_interval: Duration::from_millis(2),
        ..StudioConfig::default()
    })
    .expect("engine");
    engine
        .evaluate_and_start(score, false)
        .expect("sustained score");
    let master = engine.master_bus();
    let mut slider_id = None;
    let started = Instant::now();
    while started.elapsed() < Duration::from_secs(1) && slider_id.is_none() {
        engine
            .tick(|update| {
                if let StudioUpdate::Layout(layout) = update {
                    slider_id = layout
                        .ui_layout
                        .sliders
                        .first()
                        .map(|slider| slider.id.clone());
                }
                Ok(())
            })
            .expect("initial tick");
        std::thread::sleep(Duration::from_millis(3));
    }
    let id = slider_id.expect("slider id");
    // Studio arms a future launch boundary. Wait until the silent note is
    // actually active before asking a sustained-voice update to affect it.
    let started = Instant::now();
    while engine
        .snapshot()
        .pressure
        .as_ref()
        .is_none_or(|pressure| pressure.device.realtime_pressure.active_voices == 0)
    {
        assert!(
            started.elapsed() < Duration::from_secs(3),
            "the initial note did not launch"
        );
        engine.tick(accepted_update).expect("launch tick");
        std::thread::sleep(Duration::from_millis(3));
    }
    let observe = |engine: &mut StudioEngine, duration| {
        let mut peak = 0.0_f32;
        let started = Instant::now();
        while started.elapsed() < duration {
            engine.tick(accepted_update).expect("tick");
            peak = peak.max(master.take_levels().peak);
            std::thread::sleep(Duration::from_millis(3));
        }
        peak
    };
    assert_eq!(observe(&mut engine, Duration::from_millis(150)), 0.0);
    engine
        .set_slider_smoothed(&id, 0.75)
        .expect("smoothed move");
    let audible_peak = observe(&mut engine, Duration::from_millis(250));
    assert!(
        audible_peak > 0.05,
        "the already-sounding note must become audible: peak {audible_peak}"
    );
    let pressure = engine.snapshot().pressure.expect("device pressure");
    assert_eq!(
        pressure.device.realtime_pressure.peak_active_voices, 1,
        "moving gain must not retrigger the long note"
    );
    engine.set_slider(&id, 0.0).expect("exact move");
    observe(&mut engine, Duration::from_millis(100));
    assert!(observe(&mut engine, Duration::from_millis(80)) < 0.000001);
    engine.stop(Duration::from_millis(200)).expect("stop");
}

/// Not a test: the entry of a plugin worker. The plugin tests give this
/// binary to the host as its worker program, so each bundle runs in a
/// process of its own, as in the product. The host adds the bundle path as
/// the last argument. The argument `slow` makes a load of 600 ms. The
/// argument `fault=` and a file path gives the effect a fault in its first
/// audio block, one time: the fault makes the file.
#[cfg(feature = "vst")]
#[test]
fn plugin_worker() {
    let args: Vec<String> = std::env::args().collect();
    // An ordinary run of the tests has no bundle path.
    let Some(bundle) = args.last().filter(|arg| arg.ends_with(".vst3")) else {
        return;
    };
    if args.iter().any(|arg| arg == "slow") {
        std::thread::sleep(Duration::from_millis(600));
    }
    if let Some(file) = args.iter().find_map(|arg| arg.strip_prefix("fault=")) {
        let fault = format!("{}:{file}", rustel_vst3_fixture::ABORT_IN_AUDIO);
        // SAFETY: this process is the worker, and no other thread of it
        // reads a variable now.
        unsafe { std::env::set_var(rustel_vst3_fixture::ABORT_ENV, fault) };
    }
    std::process::exit(rustel_runtime::vst::serve(std::path::Path::new(bundle)));
}

/// Gives the host this binary as its worker program, with one more
/// argument or none: see [`plugin_worker`].
#[cfg(feature = "vst")]
fn use_plugin_workers(argument: Option<String>) {
    let program = std::env::current_exe().expect("test program path");
    let mut args = vec!["plugin_worker".into(), "--exact".into()];
    args.extend(argument.map(std::ffi::OsString::from));
    rustel_runtime::vst::set_worker(program, args);
}

/// The fixture plugin in a new folder, as the only plugin folder of the
/// process: no test here reads the plugins of the machine. A new folder is
/// a new bundle for the host, so each call gives a plugin not yet loaded.
#[cfg(feature = "vst")]
fn fixture_plugins() -> tempfile::TempDir {
    let folder = tempfile::tempdir().unwrap();
    rustel_vst3_fixture::install(folder.path());
    use_plugin_workers(None);
    rustel_runtime::vst::pin_standard_folders(vec![folder.path().to_path_buf()]);
    folder
}

/// A live note goes through its plugin. The plugin is the fixture gain, and
/// the notes never stop: a silent set at gain 0 is the plugin at work. The
/// first notes play dry while the plugin loads on its own thread.
///
/// The plugin runs in a worker process, and the first worker has a fault in
/// its first audio block. The set plays on with dry notes, and the host
/// starts a new worker: the silent set at gain 0 is the plugin in that
/// worker.
#[cfg(feature = "vst")]
#[test]
fn a_live_note_plays_through_its_plugin() {
    let _engine = one_engine();
    let plugins = fixture_plugins();
    let fault = plugins.path().join("fault");
    use_plugin_workers(Some(format!("fault={}", fault.display())));
    let mut engine = silent_engine();
    let score = |gain: u8| {
        format!("note('c3').s('sine').vst('rustel fixture', {{ gain: {gain} }}).fast(8)")
    };
    engine
        .evaluate_and_start(&score(1), false)
        .expect("score with a plugin");
    let master = engine.master_bus();
    let until = |engine: &mut StudioEngine, what: &str, reached: fn(f32) -> bool| {
        let started = Instant::now();
        loop {
            let mut peak = 0.0_f32;
            tick_for(engine, Duration::from_millis(300), &mut || {
                peak = peak.max(master.take_levels().peak);
            });
            if reached(peak) {
                break;
            }
            assert!(started.elapsed() < Duration::from_secs(20), "{what}");
        }
    };
    until(&mut engine, "the set did not sound", |peak| peak > 0.05);
    engine.evaluate(&score(0), false).expect("gain 0");
    until(&mut engine, "the notes did not reach the plugin", |peak| {
        peak == 0.0
    });
    engine.evaluate(&score(1), false).expect("gain 1");
    until(
        &mut engine,
        "the plugin did not take the new gain",
        |peak| peak > 0.05,
    );
    // The plugin is loaded, so the check of a score names a key that is no
    // parameter of the plugin.
    let wrong = "note('c3').vst('rustel fixture', { gian: 1, gain: 1 })";
    let marks = rustel_runtime::lint::lint(wrong, false, None);
    let messages: Vec<&str> = marks.iter().map(|mark| mark.message.as_str()).collect();
    assert_eq!(
        messages,
        ["Rustel Fixture has no parameter \"gian\" - did you mean \"gain\"?"]
    );
    engine.stop(Duration::from_millis(200)).expect("stop");
    assert!(fault.exists(), "the first worker had no fault");
    use_plugin_workers(None);
}

/// In the wait mode a start holds its downbeat, and an edit keeps the last
/// score, until the plugin of the score is ready. The plugin is the fixture
/// at gain 0 and the last score is silent, so each note with no plugin is
/// heard. The load test of the bundle takes 600 ms, as the load of a bridged
/// plugin does. In the async mode the first notes play dry.
#[cfg(feature = "vst")]
#[test]
fn a_wait_start_and_a_wait_edit_play_no_note_before_their_plugin() {
    use rustel_studio::settings::LoadMode;
    let _engine = one_engine();
    let score = "note('c3').s('sine').vst('rustel fixture', { gain: 0 }).fast(8)";
    // The peak of the first 1.6 seconds, the cycle of the first onset, and
    // if the engine held the start or the edit.
    let heard = |mode: LoadMode, edit: bool| {
        let _plugins = fixture_plugins();
        // The fixture loads in a few ms. A slow load shows the 2 modes.
        use_plugin_workers(Some("slow".into()));
        let mut engine = silent_engine();
        let master = engine.master_bus();
        master.set_load_mode(mode);
        let held = if edit {
            engine
                .evaluate_and_start("silence", false)
                .expect("a start");
            tick_for(&mut engine, Duration::from_millis(300), &mut || {});
            let held = engine.hold_update(score, false, false);
            if !held {
                engine.evaluate(score, false).expect("an edit");
            }
            held
        } else {
            engine.evaluate_and_start(score, false).expect("a start");
            let cue = engine.snapshot().loading;
            cue.is_some_and(|cue| cue.waiting && cue.plugins == 1)
        };
        let (mut peak, mut first_onset) = (0.0_f32, None);
        tick_with(
            &mut engine,
            Duration::from_millis(1_600),
            &mut |update| note_first_onset(&mut first_onset, update),
            &mut |_| {
                peak = peak.max(master.take_levels().peak);
                false
            },
        );
        let landed = !held || !edit || matches!(engine.take_launch_outcome(), Some(Ok(_)));
        engine.stop(Duration::from_millis(200)).expect("stop");
        (peak, first_onset, held && landed)
    };
    let (peak, first_onset, held) = heard(LoadMode::Wait, false);
    assert!(held, "the start waits, and the header counts 1 plugin");
    assert_eq!(peak, 0.0, "a note of the start played with no plugin");
    assert_eq!(first_onset, Some(0.0), "the start began on cycle 0");
    let (peak, _, held) = heard(LoadMode::Wait, true);
    assert!(held, "the edit waits for the load, then lands");
    assert_eq!(peak, 0.0, "a note of the edit played with no plugin");
    for edit in [false, true] {
        let (peak, _, held) = heard(LoadMode::Async, edit);
        assert!(!held && peak > 0.05, "async, edit {edit}: peak {peak}");
    }
    use_plugin_workers(None);
}

/// The idle sweep unloads a plugin no tab names, after the output of its
/// score closed. A tab with the plugin name keeps the plugin loaded across
/// the sweep.
#[cfg(feature = "vst")]
#[test]
fn the_idle_sweep_unloads_a_plugin_no_tab_names() {
    use rustel_runtime::vst;
    use rustel_studio::engine::LiveMaterial;
    let _engine = one_engine();
    let _plugins = fixture_plugins();
    let mut engine = silent_engine_with(StudioConfig {
        unused_sample_idle: Duration::from_millis(1),
        ..StudioConfig::default()
    });
    let loaded = || {
        let plugins = vst::host().plugins();
        plugins
            .iter()
            .any(|plugin| plugin.status == vst::Status::Ready)
    };
    // Plays `score` from its tab, stops, and runs the idle sweep of the
    // stopped studio. True when the sweep left the fixture loaded.
    let mut play_and_sweep = |score: &str| {
        engine.set_live_material(&LiveMaterial {
            pinned: vec![score.into()],
            ..LiveMaterial::default()
        });
        engine.evaluate_and_start(score, false).expect("a start");
        tick_for(&mut engine, Duration::from_millis(200), &mut || {});
        let _ = engine.stop(Duration::from_millis(200));
        std::thread::sleep(Duration::from_millis(5));
        engine.idle_turn(accepted_update);
        // The plugin thread ends its load and the unload of the sweep.
        vst::host().wait_idle();
        loaded()
    };
    let with_plugin = "note('c3').s('sine').vst('rustel fixture')";
    assert!(play_and_sweep(with_plugin), "a tab names the plugin");
    assert!(!play_and_sweep("note('c3').s('sine')"), "no tab does");
}

/// What the audition slot heard over a window, and the most voices the
/// device held at once while it played: the two things the headless
/// harness can see of a preview.
fn observe_preview(engine: &mut StudioEngine, window: Duration) -> (f32, u64, u64) {
    use rustel_studio::engine::AUDITION_UI_VISUAL_SLOT;
    let mut peak = 0.0f32;
    let mut peak_voices = 0u64;
    let mut voices_now = 0u64;
    let started = Instant::now();
    while started.elapsed() < window {
        engine
            .tick(|update| {
                if let StudioUpdate::Audio { analysis, .. } = &update {
                    for (slot, frame) in &analysis.visuals {
                        if *slot == AUDITION_UI_VISUAL_SLOT {
                            peak = frame
                                .scope
                                .iter()
                                .fold(peak, |peak, sample| peak.max(sample.abs()));
                        }
                    }
                }
                Ok(())
            })
            .expect("tick");
        if let Some(pressure) = engine.snapshot().pressure {
            peak_voices = peak_voices.max(pressure.device.realtime_pressure.peak_active_voices);
            voices_now = voices_now.max(pressure.device.realtime_pressure.active_voices);
        }
        std::thread::sleep(Duration::from_millis(3));
    }
    (peak, peak_voices, voices_now)
}

/// A chord preview: the notes sound together, on the preview's own slot,
/// and one stop silences all of them. The harness cannot see three
/// separate voices in a summed scope, so the count the device reports is
/// what says "three at once".
#[test]
fn a_chord_preview_sounds_its_notes_together_and_stops_together() {
    let _engine = one_engine();
    let config = StudioConfig {
        output: Some("silent".into()),
        poll_interval: Duration::from_millis(2),
        ..StudioConfig::default()
    };
    let mut engine = StudioEngine::new(config).expect("engine");
    engine
        .evaluate_and_start("silence", false)
        .expect("something to preview through");

    // Nothing previewed: the slot is silent and no voice is held.
    let (quiet, _, resting) = observe_preview(&mut engine, Duration::from_millis(400));
    assert_eq!(quiet, 0.0, "the slot carries only the preview");
    assert_eq!(resting, 0, "a silent score holds no voices");

    // A triad on a synth, which needs no download.
    engine
        .audition_notes(&[60.0, 64.0, 67.0], "triangle", 1.0)
        .expect("the chord previews");
    // Past the choke window: a voice that was cut is gone by now, so what
    // is still held is what is actually sounding. If a chord's notes share
    // a choke group, they cut each other and one note remains.
    let _onset = observe_preview(&mut engine, Duration::from_millis(200));
    let (chord_peak, chord_voices, still_held) =
        observe_preview(&mut engine, Duration::from_millis(500));
    assert!(chord_peak > 0.01, "the chord was heard: {chord_peak}");
    assert!(
        chord_voices >= 3,
        "three notes sounded at once, not one after another: {chord_voices}"
    );
    assert!(
        still_held >= 3,
        "and they are all still ringing rather than cutting each other: {still_held}"
    );

    // A second chord cuts the first rather than piling on it: once the
    // choke has passed, only the new chord's notes are held.
    engine
        .audition_notes(&[62.0, 65.0, 69.0], "triangle", 1.0)
        .expect("the next chord previews");
    let _settle = observe_preview(&mut engine, Duration::from_millis(250));
    let (_, _, held) = observe_preview(&mut engine, Duration::from_millis(300));
    assert!(
        (3..=4).contains(&held),
        "the new chord replaced the old rather than adding to it: {held}"
    );

    // One stop silences the whole chord.
    engine.stop_audition();
    let _fade = observe_preview(&mut engine, Duration::from_millis(300));
    let (after, _, left) = observe_preview(&mut engine, Duration::from_millis(400));
    assert!(after < 0.01, "the chord was silenced: {after}");
    assert_eq!(left, 0, "no voice of it is still held");

    // A run is the same notes one after another, each ringing over the
    // one before it: a scale on a piano is a hand rolling through it, so
    // several voices are held at once even though the notes started apart.
    engine
        .audition_run(&[60.0, 62.0, 64.0, 65.0, 67.0], "triangle", 1.0, 0.15)
        .expect("the run previews");
    let (run_peak, run_voices, _) = observe_preview(&mut engine, Duration::from_millis(900));
    assert!(run_peak > 0.01, "the run was heard: {run_peak}");
    assert!(
        run_voices >= 3,
        "its notes ring over each other rather than cutting each other: {run_voices}"
    );
    // Stopping it stops the notes that have not sounded yet, which is why
    // the engine hands them over one at a time.
    engine
        .audition_run(
            &[72.0, 74.0, 76.0, 77.0, 79.0, 81.0, 83.0, 84.0],
            "triangle",
            1.0,
            0.15,
        )
        .expect("a longer run");
    let _started = observe_preview(&mut engine, Duration::from_millis(200));
    engine.stop_audition();
    let _fade = observe_preview(&mut engine, Duration::from_millis(500));
    let (after_stop, _, left) = observe_preview(&mut engine, Duration::from_millis(600));
    assert!(
        after_stop < 0.01,
        "the rest of the run never sounded: {after_stop}"
    );
    assert_eq!(left, 0, "and nothing is left holding");

    // A sample preview still works exactly as it did.
    engine
        .audition("triangle", 1.0)
        .expect("a one-sound preview");
    let (one_peak, _, _) = observe_preview(&mut engine, Duration::from_millis(500));
    assert!(one_peak > 0.01, "the single sound was heard: {one_peak}");
    let _ = engine.stop(Duration::from_millis(200));
}

/// A studio on the headless output, as the external-output tests play it.
fn silent_engine() -> StudioEngine {
    silent_engine_with(StudioConfig::default())
}

/// [`silent_engine`], its other settings taken from `config`.
fn silent_engine_with(config: StudioConfig) -> StudioEngine {
    StudioEngine::new(StudioConfig {
        output: Some("silent".into()),
        poll_interval: Duration::from_millis(2),
        ..config
    })
    .expect("engine")
}

/// Tick a playing engine for `how_long`, or until `done` says so after a
/// tick, handing each update to `emit`.
fn tick_with(
    engine: &mut StudioEngine,
    how_long: Duration,
    emit: &mut dyn FnMut(&StudioUpdate),
    done: &mut dyn FnMut(&StudioEngine) -> bool,
) {
    let started = Instant::now();
    while started.elapsed() < how_long {
        match engine.tick(|update| {
            emit(&update);
            Ok(())
        }) {
            Ok(StudioTick::Running { .. }) => {}
            Ok(other) => panic!("the set stopped: {other:?}"),
            Err(error) => panic!("the tick failed: {error}"),
        }
        if done(engine) {
            break;
        }
        std::thread::sleep(Duration::from_millis(3));
    }
}

/// Tick a playing engine for `how_long`, calling `each` after every tick.
fn tick_for(engine: &mut StudioEngine, how_long: Duration, each: &mut dyn FnMut()) {
    tick_with(engine, how_long, &mut |_| {}, &mut |_| {
        each();
        false
    });
}

/// A set folder holding, for each `(bank, takes)` of `banks`, a bank of
/// that many short silent takes.
fn set_of_banks(banks: &[(&str, usize)]) -> tempfile::TempDir {
    let set = tempfile::tempdir_in(env!("CARGO_MANIFEST_DIR")).unwrap();
    for &(bank, takes) in banks {
        let folder = set.path().join(bank);
        std::fs::create_dir(&folder).unwrap();
        for take in 0..takes {
            rustel_runtime::write_silent_wav(folder.join(format!("{take}.wav")), 48_000, 1, 0.01)
                .expect("a take on disk");
        }
    }
    set
}

/// A stopped studio over the set folder `set` with each `(name, n)` of
/// `sounds` decoded and waiting to be installed, and the unused-sample
/// idle, a millisecond, past: the next sweep drops what nothing keeps.
fn studio_over_decoded(set: &std::path::Path, sounds: &[(&str, usize)]) -> StudioEngine {
    let engine = silent_engine_with(StudioConfig {
        unused_sample_idle: Duration::from_millis(1),
        ..StudioConfig::default()
    });
    let library = engine.sample_library().expect("a sample library");
    library.adopt_set_folder(set).expect("the set's banks");
    let deadline = Instant::now() + Duration::from_secs(5);
    for &(name, n) in sounds {
        library.prefetch(&format!("{name}:{n}"));
        while library.readiness(name, n as f64) != SoundReadiness::Ready {
            assert!(Instant::now() < deadline, "{name}:{n} never decoded");
            std::thread::sleep(Duration::from_millis(2));
        }
    }
    std::thread::sleep(Duration::from_millis(5));
    engine
}

/// Lower `first` to the cycle the earliest score onset in `update` starts
/// on.
fn note_first_onset(first: &mut Option<f64>, update: &StudioUpdate) {
    if let StudioUpdate::Traces(request) = update
        && request.preview_from_cycle.is_none()
    {
        for trace in &request.traces {
            let begin = trace.whole_begin.to_f64();
            *first = Some(first.map_or(begin, |seen| seen.min(begin)));
        }
    }
}

/// How a start from stopped went.
struct FromStopped {
    /// The cycle the first onset heard starts on.
    first_onset: Option<f64>,
    /// What was said to be still loading.
    still_loading: Vec<String>,
    /// The decoded sound the score keeps after its first turn, in bytes.
    kept_after_first_turn: usize,
}

impl FromStopped {
    /// Assert the first onset sounded on cycle 0 with nothing still
    /// loading; `when` names the start.
    fn assert_on_time(&self, when: &str) {
        assert_eq!(self.first_onset, Some(0.0), "{when}: the first onset");
        assert!(
            self.still_loading.is_empty(),
            "{when}: {:?}",
            self.still_loading
        );
    }
}

/// Start `score` on a stopped studio, play a little over a cycle and stop.
fn play_a_cycle_from_stopped(engine: &mut StudioEngine, score: &str) -> FromStopped {
    engine.evaluate_and_start(score, false).expect("a start");
    let mut first_onset = None;
    let mut still_loading = Vec::new();
    let mut note = |update: &StudioUpdate| {
        note_first_onset(&mut first_onset, update);
        if let StudioUpdate::Diagnostic(diagnostic) = update
            && diagnostic.kind == rustel_runtime::SAMPLE_LOADING_DIAGNOSTIC
        {
            still_loading.push(diagnostic.message.clone());
        }
    };
    engine
        .tick(|update| {
            note(&update);
            Ok(())
        })
        .expect("the first turn");
    let kept_after_first_turn = engine.snapshot().sample_memory.live_bytes;
    tick_with(engine, Duration::from_millis(1_200), &mut note, &mut |_| {
        false
    });
    let _ = engine.stop(Duration::from_millis(200));
    FromStopped {
        first_onset,
        still_loading,
        kept_after_first_turn,
    }
}

/// A start from stopped keeps the score's decoded drum through the sweep
/// its first turn runs, whether the text names it or the score builds its
/// name in JavaScript, so the drum sounds on cycle 0 rather than a cycle
/// late.
#[test]
fn a_decoded_drum_survives_the_first_turns_sweep_and_sounds_on_cycle_zero() {
    let _engine = one_engine();
    for score in [
        "setcps(1); s(\"mlkr-grsl:3\")",
        "setcps(1); const kit = ['mlkr', 'grsl'].join('-'); s(kit).n(3)",
    ] {
        let set = set_of_banks(&[("mlkr-grsl", 4)]);
        let mut engine = studio_over_decoded(set.path(), &[("mlkr-grsl", 3)]);
        let played = play_a_cycle_from_stopped(&mut engine, score);
        assert!(
            played.kept_after_first_turn > 0,
            "{score}: the first turn swept the drum"
        );
        played.assert_on_time(score);
    }
}

/// A kit reached through a helper: no text names a drum, so only what
/// the score has played can keep them.
const KIT_THROUGH_A_HELPER: &str = "setcps(1)
const d = _s => s(_s).bank(\"RolandTR909\")
$: d(\"bd*4\")
$: d(\"[- sd]*2\")
$: d(\"[- <hh hh hh <hh hh hh oh>>]*4\")";

/// The banks [`KIT_THROUGH_A_HELPER`] strikes; all but the last sound in
/// its first cycle.
const KIT: [&str; 4] = [
    "RolandTR909_bd",
    "RolandTR909_sd",
    "RolandTR909_hh",
    "RolandTR909_oh",
];

/// The decoded sound the studio holds, in bytes.
fn decoded_bytes(engine: &mut StudioEngine) -> usize {
    let memory = engine.snapshot().sample_memory;
    memory.live_bytes + memory.preview_bytes
}

/// Run the stopped studio's idle sweep, the idle elapsed. Answers the
/// decoded sound kept before it, and all it leaves.
fn sweep_while_stopped(engine: &mut StudioEngine) -> (usize, usize) {
    let kept = engine.snapshot().sample_memory.live_bytes;
    std::thread::sleep(Duration::from_millis(5));
    engine.idle_turn(accepted_update);
    (kept, decoded_bytes(engine))
}

/// Stop, then play the same score again: what it played stays through the
/// stopped studio's idle sweep, a preview heard in between included, so
/// its first kick lands on cycle 0 with nothing still loading, though no
/// text names a drum. A different score starting lets the kit go.
#[test]
fn a_score_played_again_after_stop_keeps_what_it_played() {
    let _engine = one_engine();
    let set = set_of_banks(&KIT.map(|bank| (bank, 1)));
    let mut engine = studio_over_decoded(set.path(), &KIT.map(|bank| (bank, 0))[..3]);
    play_a_cycle_from_stopped(&mut engine, KIT_THROUGH_A_HELPER).assert_on_time("the first play");
    // Off the disk, the kit sounds only from what the studio kept.
    for bank in KIT {
        std::fs::remove_file(set.path().join(bank).join("0.wav")).unwrap();
    }

    let (kept, swept) = sweep_while_stopped(&mut engine);
    play_a_cycle_from_stopped(&mut engine, KIT_THROUGH_A_HELPER).assert_on_time("after Stop");
    assert!(kept > 0, "the kit played is kept after Stop");
    assert_eq!(swept, kept, "the sweep after Stop");

    // A preview heard while stopped plays over silence, which is no score.
    engine.audition("RolandTR909_hh", 0.5).expect("a preview");
    let deadline = Instant::now() + Duration::from_secs(5);
    while engine.is_playing() {
        assert!(
            Instant::now() < deadline,
            "the preview's output never closed"
        );
        let _ = engine.tick(accepted_update);
        std::thread::sleep(Duration::from_millis(3));
    }
    let (after_preview, swept) = sweep_while_stopped(&mut engine);
    play_a_cycle_from_stopped(&mut engine, KIT_THROUGH_A_HELPER).assert_on_time("after a preview");
    assert_eq!(
        (after_preview, swept),
        (kept, kept),
        "the sweep after a preview"
    );

    engine
        .evaluate_and_start("setcps(1); s(\"sine\")", false)
        .expect("a different score");
    engine.tick(accepted_update).expect("its first turn");
    assert_eq!(decoded_bytes(&mut engine), 0, "a different score playing");
    let _ = engine.stop(Duration::from_millis(200));
}

/// A cps-1 score striking `notes`, eight a cycle, through `output`.
fn eight_a_cycle(notes: &str, output: &str) -> String {
    format!("setCps(1); note(\"{notes}\").s(\"sawtooth\").{output}")
}

/// Play `score` for 2.5 s, save a score that cannot parse, then play on for
/// 2.6 s and stop, ticking through `tick`. Returns when the save was refused.
fn refuse_a_save_mid_set(
    engine: &mut StudioEngine,
    score: &str,
    tick: &mut dyn FnMut(&mut StudioEngine, Duration),
) -> Instant {
    engine
        .evaluate_and_start(score, false)
        .expect("a playing score");
    tick(engine, Duration::from_millis(2_500));
    let refused_at = Instant::now();
    let error = engine
        .evaluate("note(", false)
        .expect_err("the broken score is refused");
    assert_eq!(error.kind(), "evaluation");
    tick(engine, Duration::from_millis(2_600));
    let timeout = engine.stop_timeout();
    engine.stop(timeout);
    refused_at
}

/// Onsets a port received, in seconds past a refused save, cover the
/// shield's window whole: at cps 1 and eight notes a cycle, at least 9 of
/// the 13 due between 0.3 s and 1.9 s arrive, no two 450 ms apart.
fn assert_the_shielded_window_sounds(what: &str, past_refusal: &[f64]) {
    let mut window: Vec<f64> = past_refusal
        .iter()
        .copied()
        .filter(|at| (0.3..=1.9).contains(at))
        .collect();
    window.sort_by(f64::total_cmp);
    assert!(
        window.len() >= 9,
        "the audible generation's {what} went missing: {} in the window, at {window:.3?}",
        window.len()
    );
    for pair in window.windows(2) {
        let gap = pair[1] - pair[0];
        assert!(
            gap < 0.45,
            "the {what} went silent for {gap:.3}s while the audio played on: {window:.3?}"
        );
    }
}

/// Play `score` for 2 s, arm a rewind of `launched` on the next cycle line,
/// then play through the line for 4 s and stop, ticking through `tick`.
/// Returns when the line was due, in Unix seconds.
fn launch_a_rewind_mid_set(
    engine: &mut StudioEngine,
    score: &str,
    launched: &str,
    tick: &mut dyn FnMut(&mut StudioEngine, Duration),
) -> f64 {
    engine
        .evaluate_and_start(score, false)
        .expect("a playing score");
    tick(engine, Duration::from_millis(2_000));
    let armed_at = unix_now();
    let info = engine
        .arm_launch(launched, false, 1.0, true)
        .expect("the launch arms")
        .expect("a countdown");
    assert!(
        info.seconds_left > 0.0 && info.seconds_left < 1.5,
        "the line falls within a cycle: {info:?}"
    );
    tick(engine, Duration::from_millis(4_000));
    let timeout = engine.stop_timeout();
    engine.stop(timeout);
    armed_at + info.seconds_left
}

/// The outgoing score's notes, on every port.
const LOW: &str = "48 50 52 53 55 57 59 60";
/// The replacing score's notes, on every port.
const HIGH: &str = "72 74 76 77 79 81 83 84";

/// Half the step between two onsets of a score striking eight a cycle at
/// cps 1, in seconds.
const HALF_STEP: f64 = 0.0625;

/// How far past a launch's line, in seconds, the steady drains alone reach.
#[cfg(feature = "serial")]
const PAST_THE_LINE: f64 = 0.45;

/// Whether `note` is one of `notes`, a score's note numbers.
fn one_of(notes: &str, note: f64) -> bool {
    notes
        .split_whitespace()
        .any(|number| number.parse::<f64>() == Ok(note))
}

/// Whether `note` is one of [`LOW`]'s.
fn outgoing(note: f64) -> bool {
    one_of(LOW, note)
}

/// Whether `note` is one of [`HIGH`]'s.
fn replacing(note: f64) -> bool {
    one_of(HIGH, note)
}

/// When each onset whose note `keep` accepts came, of (when, note) pairs.
fn onsets_where(captured: &[(f64, f64)], keep: fn(f64) -> bool) -> Vec<f64> {
    captured
        .iter()
        .filter(|(_, note)| keep(*note))
        .map(|(at, _)| *at)
        .collect()
}

/// Both scores reached the port, and no outgoing onset came more than
/// `slack` seconds after the replacing score's first.
fn assert_nothing_outgoing_past(what: &str, outgoing: &[f64], replacing: &[f64], slack: f64) {
    assert!(
        !outgoing.is_empty(),
        "the outgoing score's {what} reached the port"
    );
    let first = replacing
        .iter()
        .copied()
        .reduce(f64::min)
        .unwrap_or_else(|| panic!("the replacing score's {what} reached the port"));
    let late: Vec<f64> = outgoing
        .iter()
        .map(|at| at - first)
        .filter(|past| *past > slack)
        .collect();
    assert!(
        late.is_empty(),
        "{} of the outgoing score's {what} came after the replacing score's first, by \
         {late:.3?}s",
        late.len()
    );
}

/// No two onsets, in seconds, are closer than [`HALF_STEP`]:
/// none went out late beside others that fell due with it, and none twice.
#[cfg(all(feature = "osc", feature = "serial"))]
fn assert_on_the_grid(what: &str, onsets: &[f64]) {
    let mut sorted = onsets.to_vec();
    sorted.sort_by(f64::total_cmp);
    let crowded: Vec<f64> = sorted
        .windows(2)
        .filter(|pair| pair[1] - pair[0] < HALF_STEP)
        .map(|pair| pair[1] - sorted[0])
        .collect();
    assert!(
        crowded.is_empty(),
        "{} {what} fell off the score's grid, at seconds past the first: {crowded:.3?}",
        crowded.len()
    );
}

/// A save that takes a second to evaluate, ahead of the score it installs.
#[cfg(all(feature = "osc", feature = "serial"))]
const A_SLOW_SECOND: &str = "const until = Date.now() + 1000; while (Date.now() < until) {}\n";

/// Seconds since the Unix epoch: the clock OSC timetags are written in, and
/// the one every pin reads its ports on.
fn unix_now() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("after the epoch")
        .as_secs_f64()
}

/// `at` in Unix seconds.
fn unix_at(at: Instant) -> f64 {
    let now = Instant::now();
    if at <= now {
        unix_now() - (now - at).as_secs_f64()
    } else {
        unix_now() + (at - now).as_secs_f64()
    }
}

/// Every bundle waiting on the loopback socket, with when it was read in
/// Unix seconds.
#[cfg(feature = "osc")]
fn receive_osc(listener: &std::net::UdpSocket) -> Vec<(f64, rustel_osc::DecodedBundle)> {
    let mut buffer = [0u8; 4096];
    let mut received = Vec::new();
    while let Ok((len, _)) = listener.recv_from(&mut buffer) {
        let bundle = rustel_osc::decode_bundle(&buffer[..len])
            .expect("what the studio sends reads back as a bundle");
        received.push((unix_now(), bundle));
    }
    received
}

/// A nonblocking loopback socket standing in for SuperDirt, and its port.
#[cfg(feature = "osc")]
fn loopback_osc() -> (std::net::UdpSocket, u16) {
    let listener = std::net::UdpSocket::bind("127.0.0.1:0").expect("a loopback port");
    listener.set_nonblocking(true).expect("nonblocking");
    let port = listener.local_addr().expect("the bound address").port();
    (listener, port)
}

/// A `.osc()` score on the headless output drives SuperDirt over the wire.
/// The listener stands in for SuperDirt: each bundle must reach the port
/// before its timetag is due, carry the dirt fields, and land half a cycle
/// after the one before. What SuperDirt makes of them is checked by ear
/// against a running one, not here.
#[cfg(feature = "osc")]
#[test]
fn a_dirt_score_reaches_the_port_ahead_of_its_timetags() {
    use rustel_osc::OscValue;

    let _engine = one_engine();
    let (listener, port) = loopback_osc();
    let mut engine = silent_engine();
    engine
        .evaluate_and_start(
            &format!("note(\"c3 e3\").s(\"sawtooth\").osc({port})"),
            false,
        )
        .expect("a playing score");

    let mut bundles = Vec::new();
    let started = Instant::now();
    while bundles.len() < 4 && started.elapsed() < Duration::from_secs(8) {
        engine.tick(accepted_update).expect("the tick");
        bundles.extend(receive_osc(&listener));
        std::thread::sleep(Duration::from_millis(3));
    }
    let timeout = engine.stop_timeout();
    engine.stop(timeout);
    assert!(bundles.len() >= 4, "only {} bundles arrived", bundles.len());

    for (received_at, bundle) in &bundles {
        let pairs = bundle.dirt_pairs();
        let field = |key: &str| pairs.iter().find(|(k, _)| k == key).map(|(_, v)| v.clone());
        assert_eq!(bundle.messages.len(), 1, "{pairs:?}");
        assert_eq!(bundle.messages[0].address, "/dirt/play");
        assert_eq!(
            field("s"),
            Some(OscValue::Str("sawtooth".into())),
            "{pairs:?}"
        );
        assert!(
            matches!(field("midinote"), Some(OscValue::Float(note)) if note == 48.0 || note == 52.0),
            "{pairs:?}"
        );
        assert_eq!(field("cps"), Some(OscValue::Float(0.5)), "{pairs:?}");
        assert!(
            matches!(field("delta"), Some(OscValue::Float(delta)) if (delta - 1.0).abs() < 1e-3),
            "{pairs:?}"
        );
        assert!(field("cycle").is_some(), "{pairs:?}");
        assert!(
            bundle.unix_seconds() + 0.05 >= *received_at,
            "a bundle arrived after it was due: due {} received {received_at}",
            bundle.unix_seconds()
        );
    }
    // Two notes a cycle at 0.5 cps: a second from one onset to the next.
    let due: Vec<f64> = bundles
        .iter()
        .map(|(_, bundle)| bundle.unix_seconds())
        .collect();
    for pair in due.windows(2) {
        let gap = pair[1] - pair[0];
        assert!((gap - 1.0).abs() < 0.05, "onsets {gap:.3}s apart: {due:?}");
    }
}

/// Route `.midi()` to a capture port, and return it.
fn capture_midi(engine: &mut StudioEngine) -> rustel_midi::CapturePort {
    let capture = rustel_midi::CapturePort::new();
    let port = capture.clone();
    engine.set_midi_port_opener(std::sync::Arc::new(move |name: &str| {
        Ok(rustel_midi::MidiSender::with_port(
            Box::new(port.clone()),
            name.to_string(),
        ))
    }));
    capture
}

/// Every note-on a capture port received: when, in Unix seconds, and its
/// pitch.
fn note_ons(capture: &rustel_midi::CapturePort) -> Vec<(f64, f64)> {
    note_ons_in(capture.messages())
}

fn note_ons_in(messages: Vec<(Instant, Vec<u8>)>) -> Vec<(f64, f64)> {
    messages
        .into_iter()
        .filter(|(_, bytes)| bytes.len() == 3 && bytes[0] & 0xf0 == 0x90 && bytes[2] > 0)
        .map(|(at, bytes)| (unix_at(at), f64::from(bytes[1])))
        .collect()
}

/// A `.midi()` score on the headless output reaches its port: the notes
/// go on and off in order, with the pitches the score wrote, through the
/// same planner and sender the CLI's live loop uses. The port is a
/// capture standing in for hardware, so the test proves the route, not
/// a device.
#[test]
fn a_midi_score_reaches_its_port_with_notes_on_and_off() {
    let _engine = one_engine();
    let mut engine = silent_engine();
    let capture = capture_midi(&mut engine);
    engine
        .evaluate_and_start(
            "setCps(1); note(\"c3 e3\").s(\"sawtooth\").midi('capture')",
            false,
        )
        .expect("a playing score");
    tick_for(&mut engine, Duration::from_millis(2_500), &mut || {});
    let timeout = engine.stop_timeout();
    engine.stop(timeout);

    let messages = capture.messages();
    let ons: Vec<f64> = note_ons(&capture)
        .into_iter()
        .map(|(_, pitch)| pitch)
        .collect();
    assert!(ons.len() >= 3, "notes on: {ons:?}");
    assert!(
        ons.iter().all(|note| *note == 48.0 || *note == 52.0),
        "the score's pitches: {ons:?}"
    );
    let offs = messages
        .iter()
        .filter(|(_, bytes)| bytes.len() == 3 && bytes[0] & 0xf0 == 0x80)
        .count();
    assert!(
        offs >= 3,
        "every note goes off: {offs} offs for {} ons",
        ons.len()
    );
    let first_on = messages
        .iter()
        .position(|(_, bytes)| bytes[0] & 0xf0 == 0x90)
        .expect("an on");
    let first_off = messages
        .iter()
        .position(|(_, bytes)| bytes[0] & 0xf0 == 0x80)
        .expect("an off");
    assert!(first_on < first_off, "on before off");
}

/// A refused save leaves the old score sounding through the reload shield's
/// window, and its MIDI keeps reaching the port there.
#[test]
fn a_failed_reload_shield_still_sends_the_audible_generations_midi() {
    let _engine = one_engine();
    let mut engine = silent_engine();
    let capture = capture_midi(&mut engine);
    let refused = unix_at(refuse_a_save_mid_set(
        &mut engine,
        &eight_a_cycle(LOW, "midi('capture')"),
        &mut |engine, how_long| tick_for(engine, how_long, &mut || {}),
    ));
    let past: Vec<f64> = note_ons(&capture)
        .iter()
        .map(|(at, _)| at - refused)
        .collect();
    assert_the_shielded_window_sounds("note-ons", &past);
}

/// A fired rewind cuts the outgoing score at its line, and the MIDI its
/// shield submitted past the line goes with it.
#[test]
fn a_launch_shield_never_sends_midi_past_its_takeover_line() {
    let _engine = one_engine();
    let mut engine = silent_engine();
    let capture = capture_midi(&mut engine);
    launch_a_rewind_mid_set(
        &mut engine,
        &eight_a_cycle(LOW, "midi('capture')"),
        &eight_a_cycle(HIGH, "midi('capture')"),
        &mut |engine, how_long| tick_for(engine, how_long, &mut || {}),
    );
    // The launched score takes over on the line, a grid step after the
    // outgoing score's last note.
    let ons = note_ons(&capture);
    assert_nothing_outgoing_past(
        "note-ons",
        &onsets_where(&ons, outgoing),
        &onsets_where(&ons, replacing),
        HALF_STEP,
    );
}

/// Drain the loopback socket, recording each bundle as (received, due,
/// midinote) in Unix seconds.
#[cfg(feature = "osc")]
fn drain_osc(listener: &std::net::UdpSocket, out: &mut Vec<(f64, f64, f64)>) {
    use rustel_osc::OscValue;
    out.extend(receive_osc(listener).into_iter().map(|(received, bundle)| {
        let note = bundle
            .dirt_pairs()
            .iter()
            .find(|(key, _)| key == "midinote")
            .map(|(_, value)| match value {
                OscValue::Float(note) => f64::from(*note),
                OscValue::Int(note) => f64::from(*note),
                OscValue::Str(text) => panic!("a numeric midinote, got {text:?}"),
            })
            .expect("a note bundle");
        (received, bundle.unix_seconds(), note)
    }));
}

/// A successful edit's takeover drops the outgoing score's audio, and no
/// bundle of the outgoing score goes out after the edit: the steady drains
/// had sent what fell within their cover, and the takeover cut the rest.
#[cfg(feature = "osc")]
#[test]
fn a_successful_edit_puts_no_old_generation_osc_past_its_takeover() {
    let _engine = one_engine();
    let (listener, port) = loopback_osc();
    let mut engine = silent_engine();
    let mut bundles: Vec<(f64, f64, f64)> = Vec::new();
    engine
        .evaluate_and_start(&eight_a_cycle(LOW, &format!("osc({port})")), false)
        .expect("a playing score");
    tick_for(&mut engine, Duration::from_millis(2_500), &mut || {
        drain_osc(&listener, &mut bundles)
    });
    drain_osc(&listener, &mut bundles);
    let edited_at = unix_now();
    engine
        .evaluate(&eight_a_cycle(HIGH, &format!("osc({port})")), false)
        .expect("the edit installs");
    tick_for(&mut engine, Duration::from_millis(2_000), &mut || {
        drain_osc(&listener, &mut bundles)
    });
    let timeout = engine.stop_timeout();
    engine.stop(timeout);
    drain_osc(&listener, &mut bundles);

    assert!(
        bundles.iter().any(|(_, _, note)| replacing(*note)),
        "the replacement reached the wire"
    );
    let ghosts: Vec<(f64, f64)> = bundles
        .iter()
        .filter(|(received, _, note)| outgoing(*note) && *received > edited_at)
        .map(|(received, due, _)| (received - edited_at, due - edited_at))
        .collect();
    assert!(
        ghosts.is_empty(),
        "{} outgoing bundles went out after the edit, (received, due) in seconds past \
         it: {ghosts:.3?}",
        ghosts.len()
    );
}

/// The OSC side of a refused save: the old score's bundles keep their
/// timetags through the reload shield's window.
#[cfg(feature = "osc")]
#[test]
fn a_failed_reload_shield_still_sends_the_audible_generations_osc() {
    let _engine = one_engine();
    let (listener, port) = loopback_osc();
    let mut engine = silent_engine();
    let mut bundles: Vec<(f64, f64, f64)> = Vec::new();
    let refused = unix_at(refuse_a_save_mid_set(
        &mut engine,
        &eight_a_cycle(LOW, &format!("osc({port})")),
        &mut |engine, how_long| {
            tick_for(engine, how_long, &mut || drain_osc(&listener, &mut bundles));
        },
    ));
    drain_osc(&listener, &mut bundles);
    let past: Vec<f64> = bundles.iter().map(|(_, due, _)| due - refused).collect();
    assert_the_shielded_window_sounds("bundles", &past);
}

/// A `.serial()` score naming a port that is not there is reported once
/// and the set plays on; the route reaches the sender, which is the part
/// that can be proved without an adapter.
#[cfg(feature = "serial")]
#[test]
fn a_serial_port_that_is_not_there_is_reported_and_the_set_plays_on() {
    let _engine = one_engine();
    let mut engine = silent_engine();
    engine
        .evaluate_and_start(
            "setCps(1); note(\"60 64\").s(\"sawtooth\").serial(115200, true, false, '/dev/rustel-no-such-port')",
            false,
        )
        .expect("a playing score");
    let mut reports = Vec::new();
    // The audio itself, not the accepted onsets: a serial onset held while
    // its port opens counts as accepted too, and would pass for sound.
    let mut peak = 0.0f32;
    tick_serial_for(
        &mut engine,
        Duration::from_millis(1_500),
        &mut |engine| {
            peak = peak.max(engine.master_bus().take_levels().peak);
            false
        },
        &mut reports,
    );
    let timeout = engine.stop_timeout();
    engine.stop(timeout);
    assert_eq!(reports.len(), 1, "reported once: {reports:?}");
    assert!(
        reports[0].contains("rustel-no-such-port"),
        "names the port: {}",
        reports[0]
    );
    assert!(peak > 0.01, "the audio kept playing: peak {peak}");
}

/// Tick a playing engine for `how_long`, or until `done` says so after a
/// tick, keeping its serial diagnostics.
#[cfg(feature = "serial")]
fn tick_serial_for(
    engine: &mut StudioEngine,
    how_long: Duration,
    done: &mut dyn FnMut(&StudioEngine) -> bool,
    reports: &mut Vec<String>,
) {
    tick_with(
        engine,
        how_long,
        &mut |update| {
            if let StudioUpdate::Diagnostic(diagnostic) = update
                && diagnostic.kind == "serial"
            {
                reports.push(diagnostic.message.clone());
            }
        },
        done,
    );
}

/// An opener standing in for a slow driver: it counts its calls, and the
/// first one holds until the test releases it with its outcome. Later
/// calls open the capture at once.
#[cfg(feature = "serial")]
fn slow_first_opener(
    capture: &rustel_serial::CapturePort,
) -> (
    std::sync::Arc<rustel_runtime::serial_bridge::SerialOpenFn>,
    std::sync::Arc<std::sync::atomic::AtomicUsize>,
    std::sync::mpsc::Sender<Result<(), String>>,
) {
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    let opened = capture.clone();
    let calls = Arc::new(AtomicUsize::new(0));
    let counted = Arc::clone(&calls);
    let (release_tx, release_rx) = std::sync::mpsc::channel::<Result<(), String>>();
    let release_rx = std::sync::Mutex::new(release_rx);
    let opener: Arc<rustel_runtime::serial_bridge::SerialOpenFn> =
        Arc::new(move |port: &str, _baud| {
            if counted.fetch_add(1, Ordering::SeqCst) == 0 {
                release_rx
                    .lock()
                    .unwrap_or_else(|poison| poison.into_inner())
                    .recv_timeout(Duration::from_secs(10))
                    .map_err(|_| "the test never released the open".to_string())??;
            }
            Ok(rustel_serial::SerialSender::with_port(
                Box::new(opened.clone()),
                port.to_string(),
            ))
        });
    (opener, calls, release_tx)
}

/// Stop and play again while a port is still opening, as a musician does
/// when a Bluetooth adapter takes its seconds: the new set adopts the open
/// already in the driver's hands, and only the new set's writes go through
/// it. Asking for the port a second time fails on an exclusive COM port
/// while the first open is pending, and strands one more thread per
/// restart on a wedged adapter.
#[cfg(feature = "serial")]
#[test]
fn a_restart_during_a_slow_serial_open_adopts_it_rather_than_opening_again() {
    use std::sync::atomic::Ordering;

    let _engine = one_engine();
    let capture = rustel_serial::CapturePort::new();
    let (opener, calls, release) = slow_first_opener(&capture);
    let mut engine = silent_engine();
    engine.set_serial_port_opener(opener);
    let mut reports = Vec::new();
    // The two sets write different notes, so the port shows whose writes
    // reached it.
    let first = "setCps(2); note(\"60 64\").s(\"sawtooth\").serial(115200, false, false, 'rustel-slow-port')";
    let second = "setCps(2); note(\"72 76\").s(\"sawtooth\").serial(115200, false, false, 'rustel-slow-port')";

    engine
        .evaluate_and_start(first, false)
        .expect("a playing score");
    tick_serial_for(
        &mut engine,
        Duration::from_secs(5),
        &mut |_| calls.load(Ordering::SeqCst) > 0,
        &mut reports,
    );
    // Long enough for the first set to hold writes of its own.
    tick_serial_for(
        &mut engine,
        Duration::from_millis(300),
        &mut |_| false,
        &mut reports,
    );
    let timeout = engine.stop_timeout();
    engine.stop(timeout);

    engine
        .evaluate_and_start(second, false)
        .expect("the restart");
    // Long enough for the restarted set to strike the port several times.
    tick_serial_for(
        &mut engine,
        Duration::from_millis(600),
        &mut |_| false,
        &mut reports,
    );
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "the restart asked the driver for the port a second time"
    );

    release.send(Ok(())).expect("the open gave up waiting");
    tick_serial_for(
        &mut engine,
        Duration::from_secs(5),
        &mut |_| !capture.writes().is_empty(),
        &mut reports,
    );
    let timeout = engine.stop_timeout();
    engine.stop(timeout);
    let writes = capture.writes();
    assert!(
        !writes.is_empty(),
        "the restarted set never wrote through the adopted open: {reports:?}"
    );
    let has = |bytes: &[u8], needle: &[u8]| bytes.windows(needle.len()).any(|at| at == needle);
    assert!(
        writes.iter().all(|(_, bytes)| has(bytes, b"note:7")),
        "a write held by the stopped set reached the port: {writes:?}"
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

/// A stop and a restart while a port is opening, and the open then
/// fails. The failure belongs to the stopped set. The new set opens the
/// port once more and plays through it.
#[cfg(feature = "serial")]
#[test]
fn a_restart_during_a_failing_serial_open_tries_the_port_again() {
    use std::sync::atomic::Ordering;

    let _engine = one_engine();
    let capture = rustel_serial::CapturePort::new();
    let (opener, calls, release) = slow_first_opener(&capture);
    let mut engine = silent_engine();
    engine.set_serial_port_opener(opener);
    let mut reports = Vec::new();
    let score =
        "setCps(2); note(\"60 64\").s(\"sawtooth\").serial(115200, false, false, 'rustel-bt-port')";

    engine
        .evaluate_and_start(score, false)
        .expect("a playing score");
    tick_serial_for(
        &mut engine,
        Duration::from_secs(5),
        &mut |_| calls.load(Ordering::SeqCst) > 0,
        &mut reports,
    );
    let timeout = engine.stop_timeout();
    engine.stop(timeout);

    engine
        .evaluate_and_start(score, false)
        .expect("the restart");
    tick_serial_for(
        &mut engine,
        Duration::from_millis(300),
        &mut |_| false,
        &mut reports,
    );
    release
        .send(Err(
            "could not open serial port \"rustel-bt-port\": semaphore timeout".into(),
        ))
        .expect("the open gave up waiting");
    tick_serial_for(
        &mut engine,
        Duration::from_secs(5),
        &mut |_| !capture.writes().is_empty(),
        &mut reports,
    );
    let timeout = engine.stop_timeout();
    engine.stop(timeout);
    assert!(
        !capture.writes().is_empty(),
        "the restarted set kept the stopped set's failure: {reports:?}"
    );
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    assert!(
        reports
            .iter()
            .all(|report| !report.contains("semaphore timeout")),
        "the stopped set's failure was told to the new one: {reports:?}"
    );
}

/// Where the serial pins' scores write: a port the capture opener serves.
#[cfg(feature = "serial")]
const SERIAL_CAPTURE: &str = "serial(115200, true, false, 'capture')";

/// Route `.serial()` to a capture port, and return it.
#[cfg(feature = "serial")]
fn capture_serial(engine: &mut StudioEngine) -> rustel_serial::CapturePort {
    let capture = rustel_serial::CapturePort::new();
    let port = capture.clone();
    engine.set_serial_port_opener(std::sync::Arc::new(move |name: &str, _baud: u32| {
        Ok(rustel_serial::SerialSender::with_port(
            Box::new(port.clone()),
            name.to_string(),
        ))
    }));
    capture
}

/// Every write a capture port received: when, in Unix seconds, and the note
/// it carries. Without an `action` a write is strudel.cc's framing, the
/// `key:value` fields with no separator, so `note("60")` reads
/// `note:60s:sawtooth`.
#[cfg(feature = "serial")]
fn serial_notes(capture: &rustel_serial::CapturePort) -> Vec<(f64, f64)> {
    serial_notes_in(capture.writes())
}

#[cfg(feature = "serial")]
fn serial_notes_in(writes: Vec<(Instant, Vec<u8>)>) -> Vec<(f64, f64)> {
    writes
        .into_iter()
        .map(|(at, bytes)| {
            let text = String::from_utf8_lossy(&bytes);
            let digits: String = text
                .split_once("note:")
                .map(|(_, value)| value)
                .unwrap_or_default()
                .chars()
                .take_while(|character| character.is_ascii_digit() || *character == '.')
                .collect();
            let note = digits
                .parse()
                .unwrap_or_else(|_| panic!("a note in the write: {text:?}"));
            (unix_at(at), note)
        })
        .collect()
}

/// The serial side of a refused save: the old score's writes keep reaching
/// the port through the reload shield's window.
#[cfg(feature = "serial")]
#[test]
fn a_failed_reload_shield_still_writes_the_audible_generations_serial() {
    let _engine = one_engine();
    let mut engine = silent_engine();
    let capture = capture_serial(&mut engine);
    let mut reports = Vec::new();
    let refused = unix_at(refuse_a_save_mid_set(
        &mut engine,
        &eight_a_cycle(LOW, SERIAL_CAPTURE),
        &mut |engine, how_long| tick_serial_for(engine, how_long, &mut |_| false, &mut reports),
    ));
    let past: Vec<f64> = serial_notes(&capture)
        .iter()
        .map(|(at, _)| at - refused)
        .collect();
    assert_the_shielded_window_sounds(&format!("serial writes (reports: {reports:?})"), &past);
}

/// A fired rewind cuts the outgoing score at its line, and the serial
/// writes its shield held past the line go with it.
#[cfg(feature = "serial")]
#[test]
fn a_launch_shield_never_writes_serial_past_its_takeover_line() {
    let _engine = one_engine();
    let mut engine = silent_engine();
    let schedule = CapturedSchedule::default();
    let port = ScheduledCapture {
        capture: rustel_serial::CapturePort::new(),
        schedule: schedule.clone(),
    };
    engine.set_serial_port_opener(std::sync::Arc::new(move |name: &str, _baud: u32| {
        Ok(rustel_serial::SerialSender::with_port(
            Box::new(port.clone()),
            name.to_string(),
        ))
    }));
    let mut reports = Vec::new();
    launch_a_rewind_mid_set(
        &mut engine,
        &eight_a_cycle(LOW, SERIAL_CAPTURE),
        &eight_a_cycle(HIGH, SERIAL_CAPTURE),
        &mut |engine, how_long| tick_serial_for(engine, how_long, &mut |_| false, &mut reports),
    );
    // Queue deadlines keep the takeover separate from thread scheduling.
    // Both scores have the same serial latency, which cancels in this check.
    // A takeover cannot recall writes that steady drains already queued.
    let writes = serial_notes_in(schedule.messages());
    assert_nothing_outgoing_past(
        &format!(
            "serial writes due (reports: {reports:?}, maximum receipt delay: {:?})",
            schedule.max_receipt_delay()
        ),
        &onsets_where(&writes, outgoing),
        &onsets_where(&writes, replacing),
        PAST_THE_LINE,
    );
}

/// A score striking `notes` on MIDI, OSC and serial, eight a cycle, after
/// `prelude`.
#[cfg(all(feature = "osc", feature = "serial"))]
fn on_every_output(prelude: &str, notes: &str, osc_port: u16) -> String {
    format!(
        "{prelude}setCps(1); stack(note(\"{notes}\").s(\"sawtooth\").midi('capture'), \
         note(\"{notes}\").s(\"sawtooth\").osc({osc_port}), \
         note(\"{notes}\").s(\"sawtooth\").{SERIAL_CAPTURE})"
    )
}

#[cfg(feature = "serial")]
struct CapturedDeadline {
    due: Instant,
    received: Instant,
    bytes: Vec<u8>,
}

#[cfg(feature = "serial")]
#[derive(Clone, Default)]
struct CapturedSchedule(std::sync::Arc<std::sync::Mutex<Vec<CapturedDeadline>>>);

#[cfg(feature = "serial")]
impl CapturedSchedule {
    fn record(&self, due: Instant, bytes: &[u8]) {
        let received = Instant::now();
        self.0
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .push(CapturedDeadline {
                due,
                received,
                bytes: bytes.to_vec(),
            });
    }

    fn messages(&self) -> Vec<(Instant, Vec<u8>)> {
        self.0
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .iter()
            .map(|message| (message.due, message.bytes.clone()))
            .collect()
    }

    fn max_receipt_delay(&self) -> Duration {
        self.0
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .iter()
            .map(|message| message.received.saturating_duration_since(message.due))
            .max()
            .unwrap_or_default()
    }
}

#[cfg(feature = "serial")]
#[derive(Clone)]
struct ScheduledCapture<C> {
    capture: C,
    schedule: CapturedSchedule,
}

#[cfg(all(feature = "osc", feature = "serial"))]
impl rustel_midi::MidiPort for ScheduledCapture<rustel_midi::CapturePort> {
    fn send(&mut self, bytes: &[u8]) -> Result<(), String> {
        rustel_midi::MidiPort::send(&mut self.capture, bytes)
    }

    fn send_scheduled(&mut self, due: Instant, bytes: &[u8]) -> Result<(), String> {
        self.send(bytes)?;
        self.schedule.record(due, bytes);
        Ok(())
    }
}

#[cfg(feature = "serial")]
impl rustel_serial::SerialPort for ScheduledCapture<rustel_serial::CapturePort> {
    fn write(&mut self, bytes: &[u8]) -> Result<(), String> {
        rustel_serial::SerialPort::write(&mut self.capture, bytes)
    }

    fn write_scheduled(&mut self, due: Instant, bytes: &[u8]) -> Result<(), String> {
        self.write(bytes)?;
        self.schedule.record(due, bytes);
        Ok(())
    }
}

/// Every external port a pin reads, and what reached each.
#[cfg(all(feature = "osc", feature = "serial"))]
struct Ports {
    osc: std::net::UdpSocket,
    osc_port: u16,
    bundles: Vec<(f64, f64, f64)>,
    midi: rustel_midi::CapturePort,
    midi_schedule: CapturedSchedule,
    serial_schedule: CapturedSchedule,
    /// When each tick handed control back.
    ticks: Vec<Instant>,
}

#[cfg(all(feature = "osc", feature = "serial"))]
impl Ports {
    fn open(engine: &mut StudioEngine) -> Self {
        let (osc, osc_port) = loopback_osc();
        let midi = rustel_midi::CapturePort::new();
        let serial = rustel_serial::CapturePort::new();
        let midi_schedule = CapturedSchedule::default();
        let serial_schedule = CapturedSchedule::default();
        let midi_port = ScheduledCapture {
            capture: midi.clone(),
            schedule: midi_schedule.clone(),
        };
        engine.set_midi_port_opener(std::sync::Arc::new(move |name: &str| {
            Ok(rustel_midi::MidiSender::with_port(
                Box::new(midi_port.clone()),
                name.to_string(),
            ))
        }));
        let serial_port = ScheduledCapture {
            capture: serial.clone(),
            schedule: serial_schedule.clone(),
        };
        engine.set_serial_port_opener(std::sync::Arc::new(move |name: &str, _baud: u32| {
            Ok(rustel_serial::SerialSender::with_port(
                Box::new(serial_port.clone()),
                name.to_string(),
            ))
        }));
        Self {
            osc,
            osc_port,
            bundles: Vec::new(),
            midi,
            midi_schedule,
            serial_schedule,
            ticks: Vec::new(),
        }
    }

    fn score(&self, prelude: &str, notes: &str) -> String {
        on_every_output(prelude, notes, self.osc_port)
    }

    /// Tick for `how_long`, reading the OSC port after every tick.
    fn tick(&mut self, engine: &mut StudioEngine, how_long: Duration) {
        tick_for(engine, how_long, &mut || {
            self.ticks.push(Instant::now());
            drain_osc(&self.osc, &mut self.bundles);
        });
    }

    /// Read the OSC port once the set has stopped, after the serial writes
    /// still queued have had their latency.
    fn settle(&mut self) {
        std::thread::sleep(rustel_serial::SERIAL_LATENCY * 2);
        drain_osc(&self.osc, &mut self.bundles);
    }

    /// When the engine got control back from its longest tick, in Unix
    /// seconds.
    fn resumed_after_the_longest_tick(&self) -> f64 {
        self.ticks
            .windows(2)
            .max_by_key(|pair| pair[1] - pair[0])
            .map(|pair| unix_at(pair[1]))
            .expect("the engine ticked")
    }

    /// Queue deadlines and OSC timetags describe the score's schedule.
    /// Port receipt times also include delay from the thread scheduler.
    fn onsets(&self) -> [(&'static str, Vec<(f64, f64)>); 3] {
        let latency = rustel_serial::SERIAL_LATENCY.as_secs_f64();
        let midi = note_ons_in(self.midi_schedule.messages());
        let serial = serial_notes_in(self.serial_schedule.messages());
        [
            ("note-ons", midi),
            (
                "bundles",
                self.bundles
                    .iter()
                    .map(|(_, due, note)| (*due, *note))
                    .collect(),
            ),
            (
                "serial writes",
                serial
                    .into_iter()
                    .map(|(at, note)| (at - latency, note))
                    .collect(),
            ),
        ]
    }

    fn receipt_delays(&self) -> String {
        format!(
            "maximum receipt delay: MIDI {:?}, serial {:?}",
            self.midi_schedule.max_receipt_delay(),
            self.serial_schedule.max_receipt_delay()
        )
    }

    /// On every port, both scores keep the grid.
    fn assert_every_score_keeps_the_grid(&self) {
        for (what, onsets) in self.onsets() {
            for (score, keep) in [
                ("outgoing", outgoing as fn(f64) -> bool),
                ("replacing", replacing),
            ] {
                assert_on_the_grid(
                    &format!("{score} {what} due ({})", self.receipt_delays()),
                    &onsets_where(&onsets, keep),
                );
            }
        }
    }

    /// On every port, no onset of the outgoing score comes after the
    /// replacing score's first by more than the grid's half step.
    fn assert_nothing_outgoing_after_the_replacement(&self) {
        for (what, onsets) in self.onsets() {
            assert_nothing_outgoing_past(
                &format!("{what} due ({})", self.receipt_delays()),
                &onsets_where(&onsets, outgoing),
                &onsets_where(&onsets, replacing),
                HALF_STEP,
            );
        }
    }
}

/// A save that takes a second installs a replacement: the old score's MIDI
/// keeps time through the evaluation up to the takeover, its held OSC and
/// serial go out on time or not at all, and no outgoing onset comes after
/// the replacement's first.
#[cfg(all(feature = "osc", feature = "serial"))]
#[test]
fn a_slow_successful_edit_hands_over_every_output_on_time() {
    let _engine = one_engine();
    let mut engine = silent_engine();
    let mut ports = Ports::open(&mut engine);
    engine
        .evaluate_and_start(&ports.score("", LOW), false)
        .expect("a playing score");
    ports.tick(&mut engine, Duration::from_millis(2_500));
    let edited = unix_now();
    engine
        .evaluate(&ports.score(A_SLOW_SECOND, HIGH), false)
        .expect("the slow edit installs");
    ports.tick(&mut engine, Duration::from_millis(2_500));
    let timeout = engine.stop_timeout();
    engine.stop(timeout);
    ports.settle();

    // Eight notes a cycle: one onset every 125 ms, from the outgoing score
    // until the takeover and from the replacement after it.
    let ons = note_ons(&ports.midi);
    let first_new = onsets_where(&ons, replacing)
        .into_iter()
        .reduce(f64::min)
        .expect("the replacement reached the MIDI port");
    let mut heard: Vec<f64> = ons
        .iter()
        .map(|(at, _)| at - edited)
        .filter(|past| (0.3..=first_new - edited).contains(past))
        .collect();
    heard.sort_by(f64::total_cmp);
    for pair in heard.windows(2) {
        assert!(
            pair[1] - pair[0] < 0.3,
            "the MIDI port went silent for {:.3}s while the evaluation ran: {heard:.3?}",
            pair[1] - pair[0]
        );
    }
    ports.assert_every_score_keeps_the_grid();
    ports.assert_nothing_outgoing_after_the_replacement();
}

/// A save that installs at once takes over while the outgoing score has OSC
/// bundles and serial writes out that nothing can recall: on every port each
/// onset goes out once, and none is missing where the scores change.
#[cfg(all(feature = "osc", feature = "serial"))]
#[test]
fn a_fast_edit_sends_each_onset_once_on_every_output() {
    let _engine = one_engine();
    let mut engine = silent_engine();
    let mut ports = Ports::open(&mut engine);
    engine
        .evaluate_and_start(&ports.score("", LOW), false)
        .expect("a playing score");
    ports.tick(&mut engine, Duration::from_millis(2_500));
    let edited = unix_now();
    engine
        .evaluate(&ports.score("", HIGH), false)
        .expect("the edit installs");
    ports.tick(&mut engine, Duration::from_millis(2_500));
    let timeout = engine.stop_timeout();
    engine.stop(timeout);
    ports.settle();

    ports.assert_every_score_keeps_the_grid();
    ports.assert_nothing_outgoing_after_the_replacement();
    for (what, onsets) in ports.onsets() {
        // Both scores strike the same grid: an onset sent by both is two
        // onsets on one step.
        let mut due: Vec<f64> = onsets.iter().map(|(at, _)| *at).collect();
        assert_on_the_grid(&format!("{what} of the two scores due"), &due);
        // One onset every 125 ms through the second after the edit.
        due.retain(|at| (0.0..=1.0).contains(&(at - edited)));
        due.sort_by(f64::total_cmp);
        for pair in due.windows(2) {
            assert!(
                pair[1] - pair[0] < 3.0 * HALF_STEP,
                "{what} missed a step {:.3}s past the edit ({})",
                pair[0] - edited,
                ports.receipt_delays()
            );
        }
        assert!(
            due.len() >= 7,
            "{what} due in the second past the edit: {due:.3?}"
        );
    }
}

/// A rewind whose save outlasts its line: the pre-armed cut retires the
/// outgoing score there. No port receives an onset due between the line's
/// steady bleed and the engine's return from the evaluation.
#[cfg(all(feature = "osc", feature = "serial"))]
fn assert_a_slow_rewind_leaves_its_line_silent(refused: bool) {
    let _engine = one_engine();
    let mut engine = silent_engine();
    let mut ports = Ports::open(&mut engine);
    let launched = if refused {
        format!("{A_SLOW_SECOND}throw new Error('refused')")
    } else {
        ports.score(A_SLOW_SECOND, HIGH)
    };
    let score = ports.score("", LOW);
    let line = launch_a_rewind_mid_set(&mut engine, &score, &launched, &mut |engine, how_long| {
        ports.tick(engine, how_long)
    });
    ports.settle();
    assert_eq!(
        engine
            .take_launch_outcome()
            .expect("the rewind fired")
            .is_err(),
        refused,
        "the rewind's save was refused: {refused}"
    );

    let resumed = ports.resumed_after_the_longest_tick() - line;
    for (what, onsets) in ports.onsets() {
        let heard: Vec<f64> = onsets_where(&onsets, outgoing)
            .into_iter()
            .map(|at| at - line)
            .filter(|past| *past > PAST_THE_LINE && *past < resumed)
            .collect();
        assert!(
            heard.is_empty(),
            "the outgoing score's {what} were due {heard:.3?}s past the line it was cut at, \
             before the engine returned {resumed:.3}s past it ({})",
            ports.receipt_delays()
        );
    }
    ports.assert_every_score_keeps_the_grid();
}

#[cfg(all(feature = "osc", feature = "serial"))]
#[test]
fn a_slow_failed_rewind_past_its_line_sends_nothing_the_shield_held() {
    assert_a_slow_rewind_leaves_its_line_silent(true);
}

#[cfg(all(feature = "osc", feature = "serial"))]
#[test]
fn a_slow_successful_rewind_past_its_line_sends_nothing_the_shield_held() {
    assert_a_slow_rewind_leaves_its_line_silent(false);
}

/// A Stop while a save is still evaluating hushes the set: no MIDI of the
/// outgoing score starts after the hush, and nothing the reload shield held
/// reaches a port in the next set.
#[cfg(all(feature = "osc", feature = "serial"))]
#[test]
fn a_stop_during_a_slow_evaluation_sends_nothing_the_shield_held() {
    let _engine = one_engine();
    let mut engine = silent_engine();
    let mut ports = Ports::open(&mut engine);
    engine
        .evaluate_and_start(&ports.score("", LOW), false)
        .expect("a playing score");
    ports.tick(&mut engine, Duration::from_millis(2_500));

    // A save that runs until the performer stops it, a second in: by then
    // the shield holds onsets both before and after the stop.
    let stop = engine.stop_handle();
    let stopper = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_secs(1));
        stop.request_stop();
    });
    let error = engine
        .evaluate("while (true) {}", false)
        .expect_err("the stop cancels the save");
    let stopped = unix_now();
    stopper.join().expect("the stop was asked for");
    assert_eq!(error.kind(), "cancelled");
    let settling = Instant::now();
    while settling.elapsed() < Duration::from_millis(1_500) {
        let tick = engine.tick(accepted_update).expect("a stopping tick");
        if !matches!(tick, StudioTick::Stopping) {
            break;
        }
        std::thread::sleep(Duration::from_millis(3));
    }
    let timeout = engine.stop_timeout();
    engine.stop(timeout);
    ports.settle();

    // The next set, long enough for its clock to pass every onset the last
    // one held.
    let restarted = unix_now();
    engine
        .evaluate_and_start(&ports.score("", HIGH), false)
        .expect("the next set plays");
    ports.tick(&mut engine, Duration::from_millis(4_500));
    let timeout = engine.stop_timeout();
    engine.stop(timeout);
    ports.settle();

    let late_ons: Vec<f64> = onsets_where(&note_ons(&ports.midi), outgoing)
        .into_iter()
        .map(|at| at - stopped)
        .filter(|past| *past > HALF_STEP)
        .collect();
    assert!(
        late_ons.is_empty(),
        "outgoing note-ons started {late_ons:.3?}s after the stop"
    );
    for (what, onsets) in ports.onsets() {
        let carried: Vec<f64> = onsets_where(&onsets, outgoing)
            .into_iter()
            .map(|at| at - restarted)
            .filter(|past| *past >= 0.0)
            .collect();
        assert!(
            carried.is_empty(),
            "the next set carried the stopped one's {what}, due {carried:.3?}s after it began"
        );
    }
}

/// A refused score still consumes its rewind. A refused score installs
/// nothing, so a flag left standing would rewind the next save and play
/// it from its beginning.
#[test]
fn a_refused_rewind_does_not_rewind_the_next_save() {
    let _engine = one_engine();
    let mut engine = StudioEngine::new(StudioConfig {
        output: Some("silent".into()),
        ..StudioConfig::default()
    })
    .expect("engine");
    engine
        .evaluate_guarded("setcps(1); $: pure('a')", false, || false)
        .expect("a score to replace");

    // The worker plants the flag just before calling in, exactly as a
    // rewinding evaluate asks.
    engine.plant_from_zero_for_test();
    let error = engine
        .evaluate_guarded("note(", false, || false)
        .expect_err("the refused score");
    assert_eq!(error.kind(), "evaluation");

    // The next save joins the cycle that the count is in. Cycle zero would
    // mean that an unrelated typo rewound it.
    engine
        .evaluate_guarded("setcps(1); $: pure('b')", false, || false)
        .expect("the next save");
    let joined = engine
        .requery_takeover_cycle_for_test()
        .expect("a continuous replacement names its takeover");
    assert!(
        joined.abs() > 1e-9,
        "the next save joined the cycle at {joined}, not its beginning"
    );
}

#[test]
fn rejected_piano_tripwire_output_is_retired_before_the_next_launch() {
    let _engine = one_engine();
    let mut engine = StudioEngine::new(StudioConfig {
        output: Some("silent".into()),
        poll_interval: Duration::from_millis(2),
        ..Default::default()
    })
    .expect("studio");
    engine.prepare_piano().expect("piano output");
    let rejected_stream = engine.device_report().unwrap().stream_id;

    rustel_audio::tripwire::audio_scope(|| {
        let allocation = vec![1_u8; 8];
        std::hint::black_box(allocation);
    });
    let error = engine
        .evaluate_and_start("silence", false)
        .expect_err("the contaminated output must be refused");
    assert!(format!("{error:?}").contains("prior violation"));
    assert!(
        engine.device_report().is_none(),
        "a rejected piano stream must not be reused"
    );
    assert!(!engine.is_playing());

    engine
        .evaluate_and_start("silence", false)
        .expect("a fresh output can launch the next score");
    let fresh = engine.device_report().expect("fresh output");
    assert_ne!(fresh.stream_id, rejected_stream);
    assert_eq!(fresh.callback_allocations, 0);
    assert_eq!(fresh.callback_frees, 0);
    engine.stop(Duration::from_millis(500));
}

#[test]
fn piano_direct_output_survives_invalid_score_and_is_adopted_by_valid_score() {
    let _engine = one_engine();
    let mut engine = StudioEngine::new(StudioConfig {
        output: Some("silent".into()),
        poll_interval: Duration::from_millis(2),
        ..Default::default()
    })
    .unwrap();
    engine.set_piano_settings(rustel_studio::PianoSound::Triangle, 130);
    let before = engine.snapshot();
    engine.piano_note_on(0, 60, 100).unwrap();
    engine.piano_note_on(4, 64, 100).unwrap();
    let stream = engine.device_report().unwrap().stream_id;
    let deadline = Instant::now() + Duration::from_secs(2);
    while engine
        .device_report()
        .unwrap()
        .realtime_pressure
        .active_voices
        < 2
    {
        assert!(Instant::now() < deadline, "direct piano did not sound");
        engine.idle_turn(accepted_update);
        std::thread::sleep(Duration::from_millis(2));
    }
    assert!(!engine.is_playing());
    assert_eq!(engine.snapshot().cycle, before.cycle);
    assert_eq!(engine.generation(), before.session_generation);
    assert!(engine.evaluate_and_start("note(", false).is_err());
    assert_eq!(engine.device_report().unwrap().stream_id, stream);
    assert!(!engine.is_playing());
    assert_eq!(engine.snapshot().cycle, before.cycle);
    assert!(
        engine
            .device_report()
            .unwrap()
            .realtime_pressure
            .active_voices
            >= 2
    );
    engine
        .evaluate_and_start("silence", false)
        .expect("valid score starts");
    assert!(engine.is_playing());
    assert_eq!(
        engine.device_report().unwrap().stream_id,
        stream,
        "score adopts the same callback instead of opening another output"
    );
    engine.tick(accepted_update).unwrap();
    assert!(
        engine
            .device_report()
            .unwrap()
            .realtime_pressure
            .active_voices
            >= 2,
        "starting a score must preserve independently held keys"
    );
    engine.stop_piano();
    assert!(engine.is_playing(), "helper exit does not stop the score");
    engine.stop(Duration::from_millis(500));
}

fn piano_worker_snapshot(
    worker: &StudioWorker,
    ready: impl Fn(&StudioSnapshot) -> bool,
) -> Box<StudioSnapshot> {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match worker.try_recv_control() {
            Ok(StudioControlEvent::Snapshot(snapshot)) if ready(&snapshot) => return snapshot,
            Ok(StudioControlEvent::Evaluation(outcome)) => {
                outcome.result.expect("score starts");
            }
            Ok(StudioControlEvent::Stopped(_)) => panic!("output closed before the final stop"),
            Ok(StudioControlEvent::EngineFailure(error)) => panic!("{error:?}"),
            Err(TryRecvError::Disconnected) => panic!("worker disconnected"),
            _ => {}
        }
        assert!(
            Instant::now() < deadline,
            "piano playback snapshot timed out"
        );
        std::thread::sleep(Duration::from_millis(2));
    }
}

fn piano_over_playback_tail_stops(immediate: bool) {
    let _engine = one_engine();
    let mut worker = StudioWorker::spawn(
        StudioConfig {
            output: Some("silent".into()),
            poll_interval: Duration::from_millis(2),
            ..Default::default()
        },
        #[cfg(feature = "hydra")]
        rustel_runtime::hydra::HydraBridge::new(),
    )
    .expect("worker");
    assert!(worker.try_configure_piano(rustel_studio::PianoSound::Triangle, 130, false));
    worker
        .try_evaluate(
            1,
            1,
            "note(36).s('sine').slow(1)".into(),
            false,
            Launch::Now,
            false,
        )
        .expect("score queued");
    let sounding = piano_worker_snapshot(&worker, |snapshot| {
        snapshot
            .pressure
            .as_ref()
            .is_some_and(|pressure| pressure.device.realtime_pressure.active_voices == 1)
    });
    let stream = sounding.pressure.as_ref().unwrap().device.stream_id;

    // The real producer has already scheduled a long score. Piano must sound
    // on its callback without waiting for that score's next onset.
    assert!(worker.try_piano_note_on(0, 60, 100));
    let chord = piano_worker_snapshot(&worker, |snapshot| {
        snapshot
            .pressure
            .as_ref()
            .is_some_and(|pressure| pressure.device.realtime_pressure.active_voices == 2)
    });
    assert_eq!(chord.pressure.as_ref().unwrap().device.stream_id, stream);
    assert!(chord.playing);

    worker.request_stop();
    let draining = piano_worker_snapshot(&worker, |snapshot| snapshot.stopping);
    assert!(worker.try_piano_note_on(4, 64, 100));
    let detached = piano_worker_snapshot(&worker, |snapshot| !snapshot.playing);
    assert_eq!(detached.cycle, draining.cycle);
    let still_detached = piano_worker_snapshot(&worker, |snapshot| !snapshot.playing);
    assert_eq!(still_detached.cycle, draining.cycle);

    // Both a fresh Ctrl-. and a second, immediate Ctrl-. must acknowledge
    // the actual device, even though the score no longer owns that device.
    if immediate {
        worker.force_stop();
    } else {
        worker.request_stop();
    }
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match worker.try_recv_control() {
            Ok(StudioControlEvent::Stopped(stop)) => {
                assert!(stop.acknowledged);
                assert!(
                    stop.report.stop_acknowledged,
                    "must stop the actual callback"
                );
                assert_eq!(stop.report.stream_id, stream);
                break;
            }
            Ok(StudioControlEvent::EngineFailure(error)) => panic!("{error:?}"),
            Err(TryRecvError::Disconnected) => panic!("worker disconnected"),
            _ => {}
        }
        assert!(Instant::now() < deadline, "piano Stop was not acknowledged");
        std::thread::sleep(Duration::from_millis(2));
    }
    let stopped = piano_worker_snapshot(&worker, |snapshot| !snapshot.playing);
    assert!(!stopped.stopping);
    assert_eq!(stopped.cycle, draining.cycle);
    worker
        .try_evaluate(
            2,
            2,
            "note(36).s('sine').slow(64)".into(),
            false,
            Launch::Now,
            false,
        )
        .expect("restart queued");
    let restarted = piano_worker_snapshot(&worker, |snapshot| {
        snapshot
            .pressure
            .as_ref()
            .is_some_and(|pressure| pressure.device.realtime_pressure.active_voices == 1)
    });
    assert!(restarted.playing);
    assert_ne!(restarted.pressure.unwrap().device.stream_id, stream);
    worker.shutdown();
}

#[test]
fn piano_over_playback_tail_acknowledges_stop() {
    piano_over_playback_tail_stops(false);
}

#[test]
fn piano_over_playback_tail_acknowledges_immediate_stop() {
    piano_over_playback_tail_stops(true);
}
