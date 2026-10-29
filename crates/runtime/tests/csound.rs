//! Native orchestra loading, replacement, and export through the score host.
#![allow(
    clippy::print_stderr,
    reason = "the skip notice names the missing library"
)]

use rustel_runtime::{QUERY_WORKER_STACK_BYTES, ScoreImportAccess, Session, SessionConfig};
use std::sync::atomic::AtomicBool;

fn on_score_thread(test: impl FnOnce() + Send + 'static) {
    std::thread::Builder::new()
        .stack_size(QUERY_WORKER_STACK_BYTES)
        .spawn(test)
        .expect("score thread")
        .join()
        .expect("score test");
}

fn native_available() -> bool {
    match rustel_csound::Program::compile(&[]) {
        Ok(_) => true,
        Err(error)
            if std::env::var_os("RUSTEL_CSOUND_LIBRARY").is_none()
                && error.contains("Csound 6 is not available") =>
        {
            eprintln!("native Csound unavailable: {error}");
            false
        }
        Err(error) => panic!("configured native Csound failed: {error}"),
    }
}

fn session() -> Session {
    let mut config = SessionConfig::default()
        .with_sample_rate(8_000)
        .with_cps(1.0);
    config.score_import_access.permit_csound();
    let mut session = Session::with_config(config).expect("session");
    session.set_direct_diagnostic_logging(false);
    session
}

fn audible(session: &mut Session, seconds: f64) -> Vec<f32> {
    let pcm = session.render_pcm(seconds).expect("render");
    assert!(pcm.iter().all(|sample| sample.is_finite()));
    assert!(pcm.iter().any(|sample| sample.abs() > 0.001));
    assert!(session.take_diagnostics().is_empty());
    pcm
}

#[test]
fn native_orchestras_require_an_explicit_grant() {
    on_score_thread(|| {
        let mut session = Session::new().expect("session");
        let error = session
            .evaluate("loadCSound`instr denied\nendin`")
            .expect_err("native compilation needs permission");
        assert!(error.to_string().contains("--allow-csound"));
        session.evaluate("note('c4').csound()").expect("pattern");
        let error = session
            .render_pcm(0.01)
            .expect_err("native rendering needs permission");
        assert!(error.to_string().contains("--allow-csound"));
    });
}

#[test]
fn revoking_native_permission_keeps_builtin_audio_rendering() {
    if !native_available() {
        return;
    }
    on_score_thread(|| {
        let mut session = session();
        session
            .evaluate("loadCSound(); note('c4').s('sine')")
            .expect("initialized orchestra and built-in score");
        let expected = audible(&mut session, 0.1);
        session.set_score_import_access(ScoreImportAccess::denied());
        session
            .evaluate("note('c4').s('sine')")
            .expect("built-in score after permission revoked");
        assert_eq!(audible(&mut session, 0.1), expected);
        let error = session
            .with_panic_recovery::<()>(0.0, |_| panic!("recovery probe"))
            .expect_err("contained panic");
        assert!(error.to_string().contains("last-good source restored"));
        assert_eq!(audible(&mut session, 0.1), expected);
        session.evaluate("note('c4').csound()").expect("pattern");
        let error = session
            .render_pcm(0.1)
            .expect_err("native rendering still needs permission");
        assert!(error.to_string().contains("--allow-csound"));
    });
}

#[test]
fn documented_scores_render_with_the_native_library() {
    if !native_available() {
        return;
    }
    on_score_thread(|| {
        let examples: Vec<_> = include_str!("../../../docs/csound.md")
            .split("```js\n")
            .skip(1)
            .map(|block| block.split("```").next().expect("code block"))
            .collect();
        assert_eq!(examples.len(), 3);
        for source in examples {
            let mut session = session();
            session.evaluate(source).expect("documented score");
            audible(&mut session, 0.15);
        }
    });
}

#[test]
fn orchestra_replacements_are_transactional_and_survive_export_snapshots() {
    if !native_available() {
        return;
    }
    on_score_thread(|| {
        let mut session = session();
        let source = |amplitude| {
            format!(
                "loadCSound`instr probe\naOut init {amplitude}\nouts aOut,aOut\nendin`\nnote('c4').csound('probe')"
            )
        };
        session
            .evaluate_prebake(&source(0.025))
            .expect("setup definition");
        let stopped = AtomicBool::new(false);
        session
            .reload_at_cancellable(&source(0.05), false, 0.0, &stopped)
            .expect("first score");
        let accepted = session.csound_snapshot();
        let first = audible(&mut session, 0.1);
        for rejected in [
            "loadCSound`instr probe\nthis_is_not_an_opcode\nendin`\nnote('c4').csound('probe')",
            "loadCSound`instr probe\naOut init .9\nouts aOut,aOut\nendin`\nmissingFunction()",
        ] {
            assert!(
                session
                    .reload_at_cancellable(rejected, false, 0.0, &stopped)
                    .is_err()
            );
            assert_eq!(session.csound_snapshot(), accepted);
            assert_eq!(audible(&mut session, 0.1), first);
        }
        session.evaluate(&source(0.1)).expect("second definition");
        assert_ne!(audible(&mut session, 0.1), first);
        session
            .evaluate(&source(0.05))
            .expect("restore earlier definition");
        assert_eq!(audible(&mut session, 0.1), first);
        session
            .evaluate("note('c4').csound('probe')")
            .expect("reference retained definition");
        let snapshot = session.csound_snapshot();
        #[cfg(all(feature = "device-audio", feature = "test-support"))]
        {
            session
                .mark_audible_generation(session.generation())
                .expect("retain audible definition");
            for control in [".gain('invalid')", ".csoundm(0)"] {
                let rejected = format!("{}{control}", source(0.1));
                let error = session
                    .reload_at_cancellable(&rejected, false, 0.0, &stopped)
                    .expect_err("invalid Csound control");
                assert!(error.to_string().contains("invalid audio control"));
                assert_eq!(session.csound_snapshot(), snapshot);
                assert_eq!(audible(&mut session, 0.1), first);
            }
            session
                .reload_at_cancellable(&source(0.1), false, 0.0, &stopped)
                .expect("unconfirmed definition");
            assert_ne!(session.csound_snapshot(), snapshot);
        }
        let error = session
            .with_panic_recovery(0.0, |_| -> Result<(), rustel_runtime::RuntimeError> {
                panic!("recovery probe");
            })
            .expect_err("contained panic");
        assert!(error.to_string().contains("last-good source restored"));
        assert_eq!(session.csound_snapshot(), snapshot);
        assert_eq!(audible(&mut session, 0.1), first);
        let mut exported = self::session();
        exported.restore_csound_snapshot(&snapshot);
        exported
            .evaluate("note('c4').csound('probe')")
            .expect("retained instrument");
        assert_eq!(audible(&mut exported, 0.1), first);
    });
}

#[test]
fn recorded_sets_keep_the_orchestra_of_each_accepted_save() {
    if !native_available() {
        return;
    }
    on_score_thread(|| {
        let mut session = session();
        let source = |amplitude| {
            format!(
                "loadCSound`instr probe\naOut init {amplitude}\nouts aOut,aOut\nendin`\nnote('c4').csound('probe').fast(4)"
            )
        };
        let saves = [
            (0.0, source(0.025)),
            (0.0, source(0.05)),
            (0.25, "note('c4').csound('probe').fast(4).clip(2)".into()),
            (
                0.5,
                "loadCSound`instr probe\nthis_is_not_an_opcode\nendin`\nnote('c4').csound('probe')"
                    .into(),
            ),
            (0.5, source(0.1)),
            (0.508, format!("{}.late(0.008)", source(0.075))),
            (1.0, source(0.2)),
        ];
        let directory = tempfile::tempdir().expect("render directory");
        let path = directory.path().join("set.wav");
        let report = session
            .render_session(&saves, 0.0, Some(1.0), &path, false)
            .expect("recorded set");
        assert!(report.failed_save.is_some());
        assert!(report.query_threw.is_none());
        let decoded = rustel_audio::decode_wav(&std::fs::read(path).expect("WAV bytes"))
            .expect("decoded set");
        let mean = |start: usize| {
            decoded.pcm()[start * 2..(start + 400) * 2]
                .iter()
                .map(|sample| sample.abs())
                .sum::<f32>()
                / 800.0
        };
        let first = mean(800);
        let unchanged = mean(2_800);
        let third = mean(4_800);
        assert!(first > 0.001);
        assert!((unchanged / first - 1.0).abs() < 0.02);
        assert!(
            (third / first - 1.5).abs() < 0.02,
            "levels: {first}, {unchanged}, {third}"
        );
        // Both revisions land inside the same 128-frame render block, each on
        // its own frame. From that frame the new orchestra plays at its full
        // level. The orchestra before sounds through each swap and fades out
        // in 5 ms (40 frames at 8 kHz). In the first 4 frames of the fade it
        // plays at 0.9 of its level or more. Then the new one plays alone.
        let near = |frame: usize, level: f32| {
            for channel in 0..2 {
                let actual = decoded.pcm()[frame * 2 + channel];
                assert!(
                    (actual - level).abs() < 3.0 / 32_768.0,
                    "frame {frame}, channel {channel}: {actual} != {level}"
                );
            }
        };
        let landed = |frame: usize, new: f32, before: f32| {
            for channel in 0..2 {
                let actual = decoded.pcm()[frame * 2 + channel];
                assert!(
                    actual > new + before / 2.0 && actual < new + before + 3.0 / 32_768.0,
                    "frame {frame}, channel {channel}: {actual} is not {new} with a fade of {before}"
                );
            }
        };
        for frame in 3_996..4_000 {
            near(frame, first);
        }
        for frame in 4_000..4_004 {
            landed(frame, first * 2.0, first);
        }
        for frame in 4_040..4_064 {
            near(frame, first * 2.0);
        }
        for frame in 4_064..4_068 {
            landed(frame, first * 1.5, first * 2.0);
            let actual = decoded.pcm()[frame * 2];
            assert!(
                (actual - first * 2.0).abs() > 3.0 / 32_768.0,
                "frame {frame}: the second revision did not land"
            );
        }
        for frame in 4_104..4_108 {
            near(frame, first * 1.5);
        }
    });
}

#[test]
fn native_score_errors_fail_the_export() {
    if !native_available() {
        return;
    }
    on_score_thread(|| {
        let mut session = session();
        session
            .evaluate("note('c4').csound('missing_instrument')")
            .expect("score");
        let error = session
            .render_pcm(0.1)
            .expect_err("undefined instrument must fail");
        assert!(error.to_string().contains("Csound"));
    });
}

#[test]
fn long_exports_prepare_score_events_as_the_audio_advances() {
    if !native_available() {
        return;
    }
    on_score_thread(|| {
        let mut session = session();
        session.evaluate(
            "loadCSound`instr short\naOut init .01\nouts aOut,aOut\nendin`\nnote('c4').csound('short').fast(8193)"
        ).expect("dense score");
        let pcm = audible(&mut session, 8.01);
        assert!(
            pcm[pcm.len() - 100..]
                .iter()
                .any(|sample| sample.abs() > 0.001)
        );
    });
}

/// A Csound note on a plugin effect with latency converts again, 480 frames
/// early, so the effect plays in time. The note keeps the score event of
/// its first conversion: the second conversion prepares none, so the score
/// events of the notes after it follow on.
#[cfg(all(feature = "vst", feature = "device-audio", feature = "test-support"))]
#[test]
fn a_note_on_an_effect_with_latency_keeps_its_score_event() {
    if !native_available() {
        return;
    }
    on_score_thread(|| {
        // The fixture plugin as the only plugin of the process, with a preset
        // that delays its output by 480 frames and gives that delay as its
        // latency.
        let folder = tempfile::tempdir().expect("plugin folder");
        rustel_vst3_fixture::install(folder.path());
        rustel_runtime::vst::pin_standard_folders(vec![folder.path().to_path_buf()]);
        let presets = folder.path().join("presets");
        rustel_runtime::vst::host().set_preset_folder(presets.clone());
        let presets = presets.join(rustel_vst3_fixture::NAME);
        std::fs::create_dir_all(&presets).expect("preset folder");
        let delay = rustel_vst3_fixture::preset_with_delay(1.0, 0.0, 0.1171875);
        std::fs::write(presets.join("Delay.vstpreset"), delay).expect("preset file");
        let mut session = session();
        let library = rustel_runtime::samples::SampleLibrary::empty();
        session.set_sample_library_for_test(std::sync::Arc::new(library));
        let latent = "note('c4').csound('probe').vst('rustel fixture', { preset: 'delay' })";
        let dry = "note('e4').csound('probe').late(0.25)";
        session
            .evaluate(&format!(
                "loadCSound`instr probe\naOut init .05\nouts aOut,aOut\nendin`\nstack({latent}, {dry})"
            ))
            .expect("score");
        let output = rustel_audio::LiveScalarDevice::start_silent(8_000, session.generation())
            .expect("silent output");
        session.attach_plugins(&output);
        let started = std::time::Instant::now();
        let ceiling = std::time::Duration::from_secs(1);
        while session.plugins_pending(0.0, 1.0, ceiling) > 0 {
            assert!(started.elapsed().as_secs() < 20, "no copy");
            std::thread::sleep(std::time::Duration::from_millis(25));
        }
        // A window from `now`: the score event, the start frame and the lead
        // of the note on the effect, and the score event of the note after
        // it. The last look raised the lead of the chains to 480 frames, and
        // a start moves cycle zero by that lead. So cycle zero is at 0.1 s,
        // frame 800.
        session.restart_transport_at(0.1 - session.plugin_lead());
        let window = |session: &mut Session, now: f64| {
            let events = session.schedule_audio_at(now, 8_000).expect("events");
            let score = |event: &rustel_audio::AudioEvent| {
                let instrument = event.controls.instrument.expect("the score event");
                instrument.score().expect("a score event")
            };
            let [latent, dry] = &events[..] else {
                panic!("{} notes", events.len());
            };
            assert!(latent.controls.effects[0].is_some());
            let start = (latent.target_frame, latent.controls.lead);
            (score(latent), start, score(dry))
        };
        let (first, start, after) = window(&mut session, 0.0);
        assert_eq!(start, (320, 480));
        assert_eq!(after, first + 1);
        // The next cycle prepares one score event for each note again.
        let (second, start, next) = window(&mut session, 1.0);
        assert_eq!(start, (8_320, 480));
        assert_eq!((second, next), (after + 1, after + 2));
    });
}
