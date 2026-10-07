use std::sync::Arc;

use rustel_engine::{
    audio::{ScalarBackend, render_pcm},
    core::controls::ControlSpec,
    fraction::Fraction,
    mini,
    scheduler::{Clock, Scheduler, TickStatus, Transport, VirtualClock},
    voice,
};

#[test]
fn native_pattern_schedules_and_renders_without_a_device() {
    let sample_rate = 8_000;
    let cps = 1.0;
    let transport = Arc::new(Transport::default());
    let mut scheduler = Scheduler::new(Arc::clone(&transport), cps, 1.0);
    let clock = VirtualClock::new(0.0);
    let pattern = mini::mini("c4 e4 g4 c5").unwrap();
    let pattern = ControlSpec::new(["note"]).pattern(&pattern);
    let generation = scheduler.set_pattern(pattern, clock.now());
    assert_eq!(scheduler.tick(&clock), TickStatus::Filled);
    let events = scheduler.drain_through(&clock, 1.0);
    assert_eq!(events.len(), 4);

    let mut onsets = Vec::new();
    for (index, event) in events.iter().enumerate() {
        assert_eq!(event.generation, generation);
        assert_eq!(event.whole_begin, Fraction::new(index as i128, 4));
        assert_eq!(event.target_time, index as f64 / 4.0);
        onsets.push(
            voice::resolve_hap_value(
                &event.value,
                event.onset_id,
                event.duration.to_f64() / cps,
                event.target_time,
                sample_rate,
                cps,
                &voice::BundledOnly,
            )
            .unwrap()
            .with_generation(event.generation),
        );
    }

    let pcm = render_pcm(
        &mut ScalarBackend::new(),
        sample_rate,
        sample_rate as usize,
        &onsets,
    )
    .unwrap();
    assert_eq!(pcm.len(), sample_rate as usize * 2);
    assert!(pcm.iter().all(|sample| sample.is_finite()));
    // Every quarter-cycle note reaches the audio output, including the last.
    for quarter in pcm.chunks_exact(sample_rate as usize / 2) {
        assert!(quarter.iter().any(|sample| sample.abs() > 0.001));
    }

    transport.stop();
    clock.advance(1.0);
    assert_eq!(scheduler.tick(&clock), TickStatus::Stopped);
    assert!(scheduler.drain_due(&clock).is_empty());
}

/// The base profile assigns modulator slots in score declaration order.
/// Dependency feature unification must not change that order.
#[test]
fn modulator_slots_follow_the_score_map_order() {
    // Five LFOs for four slots, declared against alphabetical order.
    let value = serde_json::from_str(
        r#"{"s": "sine", "note": 60, "cutoff": 800, "lfo": {
            "e": {"control": "cutoff", "rate": 5},
            "d": {"control": "cutoff", "rate": 4},
            "c": {"control": "cutoff", "rate": 3},
            "b": {"control": "cutoff", "rate": 2},
            "a": {"control": "cutoff", "rate": 1}
        }}"#,
    )
    .unwrap();
    let onset = voice::resolve_voice(&value, 0, 0.5, 0.0, 8_000, 1.0).unwrap();
    let rates: Vec<f32> = onset
        .controls
        .lfos
        .iter()
        .map(|lfo| lfo.expect("every slot is filled").frequency_hz)
        .collect();
    assert_eq!(rates, [5.0, 4.0, 3.0, 2.0]);
}

#[cfg(feature = "session")]
#[test]
fn session_prepares_owned_events_for_a_native_callback() {
    std::thread::Builder::new()
        .stack_size(rustel_engine::QUERY_WORKER_STACK_BYTES)
        .spawn(|| {
            let mut session = rustel_engine::Session::with_config(
                rustel_engine::SessionConfig::default()
                    .with_cps(1.0)
                    .with_horizon(1.0)
                    .with_sample_rate(8_000),
            )
            .unwrap();
            session
                .evaluate(r#"note("c4 e4 g4 c5").s("sine")"#)
                .unwrap();
            let events = session.schedule_audio_through(0.0, 1.0, 8_000).unwrap();
            assert_eq!(events.len(), 4, "{:?}", session.take_diagnostics());
            assert!(
                events
                    .iter()
                    .all(|event| event.generation == session.generation())
            );
            assert_eq!(
                events
                    .iter()
                    .map(|event| event.target_frame)
                    .collect::<Vec<_>>(),
                vec![0, 2_000, 4_000, 6_000]
            );
            assert!(
                session
                    .schedule_audio_through(0.0, 1.0, 8_000)
                    .unwrap()
                    .is_empty()
            );
        })
        .unwrap()
        .join()
        .unwrap();
}

/// A host that never opts into direct logging gets each refused voice back
/// from `take_diagnostics`, whether it schedules, renders PCM or writes a file.
#[cfg(feature = "session")]
#[test]
fn a_new_session_collects_refused_voices_instead_of_printing_them() {
    use rustel_engine::{RenderFormat, Session, SessionConfig};

    fn refusing_session() -> Session {
        let config = SessionConfig::default()
            .with_cps(1.0)
            .with_sample_rate(8_000);
        let mut session = Session::with_config(config).unwrap();
        session.evaluate(r#"s("supersaw").unison(100)"#).unwrap();
        session
    }
    fn refusals(session: &mut Session) -> usize {
        session
            .take_diagnostics()
            .iter()
            .filter(|diagnostic| diagnostic.kind == "voice-refused")
            .count()
    }

    std::thread::Builder::new()
        .stack_size(rustel_engine::QUERY_WORKER_STACK_BYTES)
        .spawn(|| {
            let mut session = refusing_session();
            session.schedule_audio_at(0.0, 8_000).unwrap();
            assert_eq!(refusals(&mut session), 1, "scheduling");

            let mut session = refusing_session();
            session.render_pcm(1.0).unwrap();
            assert_eq!(refusals(&mut session), 1, "PCM render");

            let path = std::env::temp_dir().join(format!(
                "rustel-embedding-refusal-{}.wav",
                std::process::id()
            ));
            let mut session = refusing_session();
            let rendered = session.render(1.0, &path, RenderFormat::ScalarWav);
            let _ = std::fs::remove_file(&path);
            rendered.unwrap();
            assert_eq!(refusals(&mut session), 1, "file render");
        })
        .unwrap()
        .join()
        .unwrap();
}

/// A host that never opts into direct logging gets the voice resolver's
/// notices and the score's `.log()` lines from `take_diagnostics`, whether it
/// schedules or renders PCM.
#[cfg(feature = "session")]
#[test]
fn a_new_session_collects_voice_notices_and_log_lines() {
    use rustel_engine::{Session, SessionConfig, SessionDiagnostic, VOICE_NOTICE_DIAGNOSTIC};

    fn noticing_session() -> Session {
        let config = SessionConfig::default()
            .with_cps(1.0)
            .with_horizon(1.0)
            .with_sample_rate(8_000);
        let mut session = Session::with_config(config).unwrap();
        session
            .evaluate(r#"note("c4 e4").s("sine").duckorbit(99).log()"#)
            .unwrap();
        session
    }
    fn messages(diagnostics: &[SessionDiagnostic], kind: &str) -> Vec<String> {
        diagnostics
            .iter()
            .filter(|diagnostic| diagnostic.kind == kind)
            .map(|diagnostic| diagnostic.message.clone())
            .collect()
    }
    fn assert_collected(diagnostics: &[SessionDiagnostic], what: &str) {
        assert_eq!(
            messages(diagnostics, VOICE_NOTICE_DIAGNOSTIC),
            ["duck target orbit 99 does not exist"],
            "{what}: {diagnostics:?}"
        );
        let logs = messages(diagnostics, "log");
        assert!(logs.len() >= 2, "{what}: {diagnostics:?}");
    }

    std::thread::Builder::new()
        .stack_size(rustel_engine::QUERY_WORKER_STACK_BYTES)
        .spawn(|| {
            let mut session = noticing_session();
            session.schedule_audio_through(0.0, 1.0, 8_000).unwrap();
            assert_collected(&session.take_diagnostics(), "scheduling");

            let mut session = noticing_session();
            session.render_pcm(1.0).unwrap();
            assert_collected(&session.take_diagnostics(), "PCM render");
        })
        .unwrap()
        .join()
        .unwrap();
}
