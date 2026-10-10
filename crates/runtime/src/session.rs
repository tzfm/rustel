//! Headless Session: evaluate → query → schedule generations.
//!
//! Score evaluation and replacement live in `session/evaluation.rs`; finite
//! playback and offline rendering live in `session/playback.rs`. This module
//! owns the shared state and live scheduling.

mod evaluation;
mod playback;
mod recovery;

pub use recovery::{catch_score_panic, panic_is_contained};

#[cfg(any(test, feature = "test-support"))]
pub use recovery::SessionPanicPoint;

#[cfg(feature = "device-audio")]
pub(crate) mod confirmation;
#[cfg(feature = "device-audio")]
mod handover;

#[cfg(all(test, feature = "device-audio"))]
mod voice_refusal_tests {
    use super::*;

    use crate::LiveFileProducer;
    use rustel_audio::device::ManualLiveOutput;
    use rustel_core::{Value, pure, stack};

    const RATE: u32 = 48_000;
    const SOURCE_A: &str =
        include_str!("../tests/e2e/scores/corpus/regressions/begingate-pulse3.strudel");

    fn confirmed_score() -> (Session, LiveFileProducer, ManualLiveOutput) {
        assert_eq!(
            crate::ui_events::source_revision(SOURCE_A),
            "87e752905cf56ee74faa82520fc8ec32de83053ad10d417c325224f2a4f669d6"
        );
        let mut session = Session::with_config(SessionConfig {
            cps: 1.0,
            horizon: 0.5,
            sample_rate: RATE,
            ..SessionConfig::default()
        })
        .expect("session");
        session.set_direct_diagnostic_logging(false);
        session.set_schedule_lead(0.0);
        session.set_continuity_margin(0.0);
        session
            .evaluate(SOURCE_A)
            .expect("unchanged compatibility fixture A");
        let mut output = ManualLiveOutput::new(RATE, session.generation()).expect("manual output");
        session
            .bind_audio_confirmations(output.device().confirmations())
            .expect("bind receipts");
        let mut producer = LiveFileProducer::unwatched(Duration::from_millis(2)).expect("producer");
        let mut pcm = [0.0_f32; 256];
        let mut sounded = false;
        for _ in 0..256 {
            let device = output.device();
            producer
                .step_unwatched_with_clock_and_cutover(
                    &mut session,
                    || device.clock_seconds(),
                    RATE,
                    |generation, takeover, cut| device.set_generation(generation, takeover, cut),
                    |event| device.push(event),
                )
                .expect("A continuation");
            output.render(&mut pcm);
            assert!(pcm.iter().all(|value| value.is_finite()));
            sounded |= pcm.iter().any(|value| *value != 0.0);
            assert!(session.consume_audio_confirmations());
            if session.audible_source.is_some() {
                break;
            }
        }
        let audible = session
            .audible_source
            .as_ref()
            .expect("real copied A window");
        assert_eq!(audible.source.as_ref(), SOURCE_A);
        assert_eq!(audible.generation, session.generation());
        assert!(sounded);
        (session, producer, output)
    }

    fn voice(sound: &str, unison: f64) -> Pattern {
        sound_controls(sound, vec![("unison".into(), Value::F64(unison))])
    }

    fn sound_controls(sound: &str, extra: Vec<(String, Value)>) -> Pattern {
        let mut controls = vec![
            ("s".into(), Value::Str(sound.into())),
            ("note".into(), Value::F64(48.0)),
            ("gain".into(), Value::F64(0.05)),
        ];
        controls.extend(extra);
        pure(Value::object(controls)).fast(8.into())
    }

    fn ordered_pair(first: Pattern, second: Pattern, reverse: bool) -> Pattern {
        let mut lanes = vec![first, second];
        if reverse {
            lanes.reverse();
        }
        stack(lanes)
    }

    fn install_candidate(
        session: &mut Session,
        producer: &mut LiveFileProducer,
        output: &ManualLiveOutput,
        candidate: Pattern,
    ) -> u64 {
        let before = session.generation();
        // A native graph is not labelled as replayable source text. Anchor its
        // first query at the actual render frontier without resetting the device.
        session
            .set_pattern_at(candidate, output.device().clock_seconds(), false)
            .expect("native candidate construction succeeds");
        let generation = session.generation();
        producer.arm_replacement(before, generation);
        generation
    }

    fn assert_refused_candidate(
        session: &mut Session,
        producer: &mut LiveFileProducer,
        output: &ManualLiveOutput,
        candidate: Pattern,
    ) {
        install_candidate(session, producer, output, candidate);
        assert_refused_replacement(session, producer, output);
    }

    fn assert_refused_replacement(
        session: &mut Session,
        producer: &mut LiveFileProducer,
        output: &ManualLiveOutput,
    ) {
        let generation_a = session.audible_source.as_ref().unwrap().generation;
        let device = output.device();
        let mut published = Vec::new();
        let mut pushed = Vec::new();
        let result = producer.step_unwatched_with_clock_and_cutover(
            session,
            || device.clock_seconds(),
            RATE,
            |generation, takeover, _cut| {
                published.push((generation, takeover));
                device.set_generation(generation, takeover, TakeoverCut::None);
            },
            |event| {
                pushed.push(event);
                device.push(event)
            },
        );
        assert!(
            published.is_empty(),
            "refused candidate was published: {published:?}"
        );
        assert!(
            pushed.is_empty(),
            "a valid sibling escaped the refused candidate"
        );
        let error = result.expect_err("one hard voice refusal rejects the entire replacement");
        assert!(
            error.to_string().contains("scalar audio refused")
                && error.to_string().contains("the replacement's first window")
                && error
                    .to_string()
                    .contains("the last audible score keeps playing"),
            "{error}"
        );
        assert_eq!(device.generation(), generation_a);
        assert_eq!(session.active_source(), Some(SOURCE_A));
        assert_eq!(
            session.audible_source.as_ref().unwrap().generation,
            generation_a
        );
        #[cfg(feature = "midi")]
        assert!(session.take_pending_midi().is_empty());
        #[cfg(feature = "osc")]
        assert!(session.take_pending_osc().is_empty());
        #[cfg(feature = "serial")]
        assert!(session.take_pending_serial().is_empty());
    }

    /// A channel past every input the engine can read is a score error and
    /// refuses the replacement, as the lint says it will; a channel this input
    /// happens not to have plays silence with a warning, and the siblings
    /// play. Either way a corrected score follows.
    #[test]
    fn out_of_range_channels_refuse_and_missing_ones_play_silence_then_a_correction_follows() {
        for reverse in [false, true] {
            let (mut session, mut producer, output) = confirmed_score();
            session.set_audio_input_channels(Some(2));
            assert_refused_candidate(
                &mut session,
                &mut producer,
                &output,
                ordered_pair(
                    voice("sine", 1.0),
                    sound_controls("in", vec![("n".into(), Value::F64(2323.0))]),
                    reverse,
                ),
            );
            let diagnostics = session.take_diagnostics();
            assert!(diagnostics.iter().any(|diagnostic| {
                diagnostic.kind == "voice-refused" && diagnostic.message.contains("in:2323")
            }));

            let events = publish_candidate(
                &mut session,
                &mut producer,
                &output,
                ordered_pair(
                    voice("sine", 1.0),
                    sound_controls("in", vec![("n".into(), Value::F64(2.0))]),
                    reverse,
                ),
            );
            assert!(
                events.iter().any(|event| matches!(
                    event.synth,
                    Some(rustel_audio::SynthSource::Input { channel: 2 })
                )),
                "the missing channel's voice is published, silent"
            );
            let diagnostics = session.take_diagnostics();
            assert!(diagnostics.iter().any(|diagnostic| {
                diagnostic.kind == "audio-input"
                    && diagnostic.message.contains("in:2 is unavailable")
                    && diagnostic.message.contains("plays silence")
            }));
            assert!(
                diagnostics
                    .iter()
                    .all(|diagnostic| diagnostic.kind != "voice-refused"),
                "not a refusal"
            );

            // Input conversion keeps its existing fractional truncation:
            // channel 1.75 is the second channel of this stereo input.
            let events = publish_candidate(
                &mut session,
                &mut producer,
                &output,
                ordered_pair(
                    voice("sine", 1.0),
                    sound_controls("in", vec![("n".into(), Value::F64(1.75))]),
                    reverse,
                ),
            );
            assert!(events.iter().any(|event| matches!(
                event.synth,
                Some(rustel_audio::SynthSource::Input { channel: 1 })
            )));
        }
    }

    /// A channel the open input does not have is silence with a warning, not
    /// a refusal: the replacement publishes, its siblings play, and the
    /// producer never latches over a microphone that turned out to be mono.
    #[test]
    fn a_missing_input_channel_plays_silence_and_warns_instead_of_refusing() {
        for reverse in [false, true] {
            let (mut session, mut producer, output) = confirmed_score();
            session.set_audio_input_channels(Some(2));
            let input = "s('in').n(sine.range(2, 3))";
            let source = if reverse {
                format!("stack({input}, s('sine')).fast(16)")
            } else {
                format!("stack(s('sine'), {input}).fast(16)")
            };
            let previous = session.generation();
            let candidate = session
                .reload_at(&source, false, output.device().clock_seconds())
                .expect("valid JavaScript with a computed input channel");
            producer.arm_replacement(previous, candidate);
            let device = output.device();
            let mut published = Vec::new();
            let mut pushed = Vec::new();
            producer
                .step_unwatched_with_clock_and_cutover(
                    &mut session,
                    || device.clock_seconds(),
                    RATE,
                    |generation, takeover, _cut| {
                        published.push(generation);
                        device.set_generation(generation, takeover, TakeoverCut::None);
                    },
                    |event| {
                        pushed.push(event);
                        device.push(event)
                    },
                )
                .expect("the replacement is published");
            assert_eq!(published, [candidate], "published, not refused");
            assert!(
                pushed.iter().any(|event| matches!(
                    event.synth,
                    Some(rustel_audio::SynthSource::Input { channel: 2 })
                )),
                "the input voice is there - the ring reads it as silence"
            );
            assert!(
                pushed.iter().any(|event| event.synth.is_none()
                    || !matches!(event.synth, Some(rustel_audio::SynthSource::Input { .. }))),
                "and its sibling plays"
            );
            assert!(session.take_diagnostics().iter().any(|diagnostic| {
                diagnostic.kind == "audio-input"
                    && diagnostic
                        .message
                        .contains("open input provides channels 0..1 - it plays silence")
            }));
        }
    }

    #[test]
    fn missing_or_expanded_input_facts_do_not_keep_an_obsolete_channel_limit() {
        for channels in [None, Some(0), Some(4)] {
            let (mut session, mut producer, output) = confirmed_score();
            session.set_audio_input_channels(Some(2));
            session.set_audio_input_channels(channels);
            let events = publish_candidate(
                &mut session,
                &mut producer,
                &output,
                sound_controls("in", vec![("n".into(), Value::F64(3.0))]),
            );
            assert!(!events.is_empty());
            assert!(events.iter().all(|event| matches!(
                event.synth,
                Some(rustel_audio::SynthSource::Input { channel: 3 })
            )));
            assert!(
                session
                    .take_diagnostics()
                    .iter()
                    .all(|diagnostic| diagnostic.kind != "voice-refused")
            );
        }
    }

    /// In every mode a channel the input lacks is a silent voice with a
    /// warning, never a refusal that could latch the producer.
    #[test]
    fn unavailable_input_channels_play_silence_in_every_mode() {
        for mode in [
            LiveQueryBudgetMode::InitialPrefill,
            LiveQueryBudgetMode::Steady,
            LiveQueryBudgetMode::ReplacementPrefill,
        ] {
            let mut session = Session::with_config(SessionConfig {
                cps: 1.0,
                horizon: 0.5,
                sample_rate: RATE,
                ..SessionConfig::default()
            })
            .unwrap();
            session.set_direct_diagnostic_logging(false);
            session.set_audio_input_channels(Some(2));
            session
                .set_pattern(ordered_pair(
                    voice("sine", 1.0),
                    sound_controls("in", vec![("n".into(), Value::F64(3.0))]),
                    false,
                ))
                .unwrap();
            let batch = session
                .schedule_audio_live_at(0.0, RATE, Duration::ZERO, mode)
                .unwrap_or_else(|_| panic!("a missing channel refuses nothing"));
            assert!(!batch.events.is_empty());
            assert_eq!(batch.dispositions.refused, 0, "{mode:?}");
            assert!(
                batch.events.iter().any(|event| matches!(
                    event.synth,
                    Some(rustel_audio::SynthSource::Input { channel: 3 })
                )),
                "{mode:?}: the voice is there, reading silence"
            );
            assert!(session.take_diagnostics().iter().any(|diagnostic| {
                diagnostic.kind == "audio-input" && diagnostic.message.contains("plays silence")
            }));
        }
    }

    #[test]
    fn explicit_audio_scheduling_warns_about_a_missing_input_channel_the_same_way() {
        for channels in [None, Some(2), Some(4)] {
            let mut session = Session::new().unwrap();
            session.set_direct_diagnostic_logging(false);
            session.set_audio_input_channels(channels);
            session
                .set_pattern(sound_controls("in", vec![("n".into(), Value::F64(3.0))]))
                .unwrap();
            let events = session.schedule_audio_at(0.0, RATE).unwrap();
            assert!(
                !events.is_empty(),
                "{channels:?}: the voice plays, silent or not"
            );
            let diagnostics = session.take_diagnostics();
            assert!(
                diagnostics
                    .iter()
                    .all(|diagnostic| diagnostic.kind != "voice-refused")
            );
            assert_eq!(
                diagnostics
                    .iter()
                    .any(|diagnostic| diagnostic.kind == "audio-input"),
                channels == Some(2),
                "{channels:?}"
            );
        }
    }

    fn publish_candidate(
        session: &mut Session,
        producer: &mut LiveFileProducer,
        output: &ManualLiveOutput,
        candidate: Pattern,
    ) -> Vec<rustel_audio::QueuedAudioEvent> {
        let generation = install_candidate(session, producer, output, candidate);
        let device = output.device();
        let mut publications = Vec::new();
        let mut events = Vec::new();
        producer
            .step_unwatched_with_clock_and_cutover(
                session,
                || device.clock_seconds(),
                RATE,
                |selected, takeover, _cut| {
                    publications.push(selected);
                    device.set_generation(selected, takeover, TakeoverCut::None);
                },
                |event| {
                    assert!(device.push(event));
                    events.push(event);
                    true
                },
            )
            .expect("permitted candidate prefill");
        assert_eq!(publications, [generation]);
        assert_eq!(device.generation(), generation);
        assert_eq!(
            session.audible_source.as_ref().unwrap().source.as_ref(),
            SOURCE_A,
            "publication alone must not replace the copied source"
        );
        events
    }

    #[test]
    fn mixed_hard_voice_refusal_keeps_the_consumer_confirmed_score() {
        for reverse in [false, true] {
            let (mut session, mut producer, mut output) = confirmed_score();
            assert_refused_candidate(
                &mut session,
                &mut producer,
                &output,
                ordered_pair(voice("sine", 1.0), voice("supersaw", 100.0), reverse),
            );

            // The exact ceiling remains playable, and a corrected edit is not
            // poisoned by the refused candidate or its re-armed rollback.
            let events = publish_candidate(
                &mut session,
                &mut producer,
                &output,
                voice("supersaw", 32.0),
            );
            assert!(!events.is_empty());
            assert!(events.iter().all(|event| matches!(
                event.synth,
                Some(rustel_audio::SynthSource::Supersaw { voices, .. }) if voices == 32.0
            )));
            let mut pcm = [0.0_f32; 256];
            let mut sounded = false;
            for _ in 0..256 {
                output.render(&mut pcm);
                assert!(pcm.iter().all(|value| value.is_finite()));
                sounded |= pcm.iter().any(|value| *value != 0.0);
                assert!(session.consume_audio_confirmations());
                if session.audible_source.is_none() {
                    break;
                }
            }
            assert!(sounded);
            assert!(
                session.audible_source.is_none(),
                "copied native graph has no replay text"
            );
            let report = output.device().report();
            assert_eq!(report.callback_errors, 0);
            assert_eq!(report.callback_allocations, 0);
            assert_eq!(report.callback_frees, 0);
            assert_eq!(report.callback_scope_misses, 0);
        }
    }

    #[test]
    fn replacement_preserves_loading_failed_and_unknown_sample_skips() {
        for kind in ["loading", "failed", "unknown"] {
            for reverse in [false, true] {
                let (mut session, mut producer, output) = confirmed_score();
                let library = crate::samples::SampleLibrary::with_loading_sample_for_test("held");
                if kind == "failed" {
                    library.fail_loading_sample_for_test();
                }
                session.samples = Some(Arc::new(library));
                let name = if kind == "unknown" {
                    "unknown-test-sample"
                } else {
                    "held"
                };
                let events = publish_candidate(
                    &mut session,
                    &mut producer,
                    &output,
                    ordered_pair(voice("sine", 1.0), voice(name, 1.0), reverse),
                );
                assert!(!events.is_empty(), "{kind}");
                assert!(events.iter().all(|event| event.sample.is_none()), "{kind}");
                // A sound still on its way is waiting, not refused, and says so
                // under its own kind; a failed or unknown one is the score's
                // problem and keeps the refusal kind.
                let expected = if kind == "loading" {
                    crate::SAMPLE_LOADING_DIAGNOSTIC
                } else {
                    "voice-refused"
                };
                assert!(
                    session
                        .take_diagnostics()
                        .iter()
                        .any(|diagnostic| diagnostic.kind == expected),
                    "the skipped {kind} sample remains visible as {expected}"
                );
            }
        }
    }

    /// A replacement whose first window has only missing sounds, but which
    /// plays a few cycles later, is published. The missing onsets are skipped
    /// and reported.
    #[test]
    fn a_first_window_of_missing_sounds_is_published_when_the_score_plays_ahead() {
        let (mut session, mut producer, output) = confirmed_score();
        // `set_pattern` re-anchors `now` at the scheduler's query cursor, so
        // the lane that will be current after install is `scheduled_to_cycle`,
        // not the audible cycle under the old score.
        let cycle = session.scheduled_to_cycle().floor().max(0.0) as usize;
        // The playable lane is the LAST one the look-ahead covers, so the
        // test pins the horizon itself, whatever it is tuned to.
        let horizon = REPLACEMENT_LOOK_AHEAD_CYCLES as usize;
        let lanes = (0..horizon)
            .map(|lane| {
                if lane == (cycle + horizon - 1) % horizon {
                    voice("sine", 1.0)
                } else {
                    voice("unknown-test-sample", 1.0)
                }
            })
            .collect();
        let events = publish_candidate(
            &mut session,
            &mut producer,
            &output,
            rustel_core::slowcat(lanes),
        );
        assert!(
            events.is_empty(),
            "the first window is the missing stretch: {events:?}"
        );
        assert!(
            session.take_diagnostics().iter().any(|diagnostic| {
                diagnostic.kind == "voice-refused"
                    && diagnostic.message.contains("unknown-test-sample")
            }),
            "the missing sound is still reported"
        );
    }

    /// A look-ahead that finds only a sample still loading does not publish.
    /// The replacement is held as retryable: the last audible score keeps
    /// playing and the candidate stays eligible.
    #[test]
    fn a_first_window_of_missing_sounds_with_only_a_loading_sample_ahead_waits() {
        let (mut session, mut producer, output) = confirmed_score();
        session.samples = Some(Arc::new(
            crate::samples::SampleLibrary::with_loading_sample_for_test("held"),
        ));
        let cycle = session.scheduled_to_cycle().floor().max(0.0) as usize;
        let horizon = REPLACEMENT_LOOK_AHEAD_CYCLES as usize;
        let lanes = (0..horizon)
            .map(|lane| {
                if lane == (cycle + 1) % horizon {
                    voice("held", 1.0)
                } else {
                    voice("unknown-test-sample", 1.0)
                }
            })
            .collect();
        install_candidate(
            &mut session,
            &mut producer,
            &output,
            rustel_core::slowcat(lanes),
        );
        let generation_a = session.audible_source.as_ref().unwrap().generation;
        let device = output.device();
        let mut published = Vec::new();
        let result = producer.step_unwatched_with_clock_and_cutover(
            &mut session,
            || device.clock_seconds(),
            RATE,
            |generation, takeover, cut| {
                published.push((generation, takeover));
                device.set_generation(generation, takeover, cut);
            },
            |event| device.push(event),
        );
        assert!(
            published.is_empty(),
            "a loading promise published a score that renders nothing: {published:?}"
        );
        // The refusal is a quiet hold, the way a first window of loading
        // refusals is: Ok with nothing scheduled, so the engine does not read
        // the wait as a failed launch, and the next turn retries.
        let step = result.expect("a loading promise holds quietly");
        assert_eq!(
            (step.scheduled, step.pushed),
            (0, 0),
            "nothing of the held candidate reaches the device"
        );
        assert_eq!(device.generation(), generation_a);
        // No rollback either: the native candidate stays installed and
        // retry-eligible, while the old generation remains the audible one.
        assert_eq!(
            session.audible_source.as_ref().unwrap().generation,
            generation_a
        );
    }

    /// An onset that has already passed in the current cycle is not a reason
    /// to publish: look-ahead starts at `now`, so a score whose only playable
    /// hit is behind the cursor, with nothing in the look-ahead stretch, is
    /// still refused.
    #[test]
    fn a_playable_onset_already_past_does_not_save_a_silent_future() {
        let (mut session, mut producer, output) = confirmed_score();
        let cycle = session.scheduled_to_cycle();
        assert!(
            cycle > cycle.floor(),
            "this cycle's onset must already be behind `now` so look-ahead is asked, got {cycle}"
        );
        let current = cycle.floor().max(0.0) as usize;
        // Twice the look-ahead horizon, so the only playable lane, the one
        // `now` stands in, does not come round again inside it. `pure` gives
        // one onset per lane, at the cycle boundary behind `now`.
        let span = 2 * REPLACEMENT_LOOK_AHEAD_CYCLES as usize;
        let lanes = (0..span)
            .map(|lane| {
                let sound = if lane == current % span {
                    "sine"
                } else {
                    "unknown-test-sample"
                };
                pure(Value::object(vec![
                    ("s".into(), Value::Str(sound.into())),
                    ("note".into(), Value::F64(48.0)),
                    ("gain".into(), Value::F64(0.05)),
                ]))
            })
            .collect();
        assert_refused_candidate(
            &mut session,
            &mut producer,
            &output,
            rustel_core::slowcat(lanes),
        );
    }

    /// An invalid control beyond the first window refuses the whole
    /// replacement, as one in the first window does, even beside a sibling
    /// that renders ahead.
    #[test]
    fn an_invalid_control_ahead_refuses_the_replacement() {
        let (mut session, mut producer, output) = confirmed_score();
        let cycle = session.scheduled_to_cycle().floor().max(0.0) as usize;
        let horizon = REPLACEMENT_LOOK_AHEAD_CYCLES as usize;
        let lanes = (0..horizon)
            .map(|lane| {
                if lane == (cycle + horizon - 1) % horizon {
                    voice("sine", 1.0)
                } else if lane == (cycle + horizon - 2) % horizon {
                    voice("supersaw", 100.0)
                } else {
                    voice("unknown-test-sample", 1.0)
                }
            })
            .collect();
        install_candidate(
            &mut session,
            &mut producer,
            &output,
            rustel_core::slowcat(lanes),
        );
        assert_refused_replacement(&mut session, &mut producer, &output);
    }

    /// The protection that rule exists for stays: a replacement with nothing
    /// to play in its first window or the cycles after it is refused, and the
    /// last audible score keeps playing.
    #[test]
    fn a_replacement_that_plays_nothing_ahead_is_still_refused() {
        let (mut session, mut producer, output) = confirmed_score();
        assert_refused_candidate(
            &mut session,
            &mut producer,
            &output,
            voice("unknown-test-sample", 1.0),
        );
    }

    #[test]
    fn loading_does_not_mask_a_hard_refusal_in_either_lane_order() {
        for reverse in [false, true] {
            let (mut session, mut producer, output) = confirmed_score();
            session.samples = Some(Arc::new(
                crate::samples::SampleLibrary::with_loading_sample_for_test("held"),
            ));
            assert_refused_candidate(
                &mut session,
                &mut producer,
                &output,
                ordered_pair(voice("supersaw", 100.0), voice("held", 1.0), reverse),
            );
        }
    }

    #[test]
    fn true_silence_remains_a_valid_replacement_with_samples_loading() {
        let (mut session, mut producer, mut output) = confirmed_score();
        session.samples = Some(Arc::new(
            crate::samples::SampleLibrary::with_loading_sample_for_test("held"),
        ));
        assert!(
            publish_candidate(&mut session, &mut producer, &output, rustel_core::silence())
                .is_empty()
        );
        let mut pcm = [0.0_f32; 256];
        output.render(&mut pcm);
        assert!(pcm.iter().all(|value| value.is_finite()));
        assert!(session.consume_audio_confirmations());
        assert!(
            session.audible_source.is_none(),
            "the selected zero-onset window reached a real host copy"
        );
    }

    /// One mistake repeated through a window is one refusal record on the
    /// command line's direct log, not one per onset. (The diagnostics queue
    /// collapses a run by itself, so only the direct log can show this.)
    #[test]
    fn a_repeated_refusal_is_one_direct_record_a_window() {
        let mut session = Session::with_config(SessionConfig {
            cps: 1.0,
            horizon: 0.5,
            sample_rate: RATE,
            ..SessionConfig::default()
        })
        .unwrap();
        session.set_direct_diagnostic_logging(false);
        session
            .set_pattern(ordered_pair(
                voice("sine", 1.0),
                sound_controls("nosuchsound", Vec::new()),
                false,
            ))
            .unwrap();
        session.set_direct_diagnostic_logging(true);
        let _ = direct_diagnostics_for_test::take();
        let batch = session
            .schedule_audio_live_at(0.0, RATE, Duration::ZERO, LiveQueryBudgetMode::Steady)
            .unwrap_or_else(|_| panic!("the sine plays, the unknown sound is skipped"));
        assert!(batch.dispositions.refused >= 4, "{:?}", batch.dispositions);
        let refusals = direct_diagnostics_for_test::take()
            .into_iter()
            .filter(|kind| kind == "voice-refused")
            .count();
        assert_eq!(refusals, 1, "one record for one repeated refusal");
    }

    #[test]
    fn initial_and_steady_windows_still_skip_individual_invalid_voices() {
        for mode in [
            LiveQueryBudgetMode::InitialPrefill,
            LiveQueryBudgetMode::Steady,
        ] {
            for reverse in [false, true] {
                let mut session = Session::with_config(SessionConfig {
                    cps: 1.0,
                    horizon: 0.5,
                    sample_rate: RATE,
                    ..SessionConfig::default()
                })
                .unwrap();
                session.set_direct_diagnostic_logging(false);
                session
                    .set_pattern(ordered_pair(
                        voice("sine", 1.0),
                        voice("supersaw", 100.0),
                        reverse,
                    ))
                    .unwrap();
                let batch = session
                    .schedule_audio_live_at(0.0, RATE, Duration::ZERO, mode)
                    .unwrap_or_else(|_| panic!("ordinary per-onset skip policy"));
                assert!(!batch.events.is_empty());
                assert!(batch.dispositions.refused > 0);
                assert!(batch.dispositions.converted > 0);
            }
        }
    }

    #[cfg(feature = "midi")]
    #[test]
    fn external_routing_is_valid_but_cannot_hide_a_hard_native_sibling() {
        for reverse in [false, true] {
            let external = || {
                sound_controls(
                    "external-only-test-sample",
                    vec![("midiport".into(), Value::Str("test-port".into()))],
                )
            };
            let (mut session, mut producer, output) = confirmed_score();
            assert_refused_candidate(
                &mut session,
                &mut producer,
                &output,
                ordered_pair(external(), voice("supersaw", 100.0), reverse),
            );

            // A valid route on the SAME onset preserves the existing external
            // policy, even when that onset cannot become native scalar audio.
            let routed_hard_voice = sound_controls(
                "supersaw",
                vec![
                    ("unison".into(), Value::F64(100.0)),
                    ("midiport".into(), Value::Str("test-port".into())),
                ],
            );
            for routed in [external(), routed_hard_voice] {
                let (mut session, mut producer, output) = confirmed_score();
                assert!(
                    publish_candidate(
                        &mut session,
                        &mut producer,
                        &output,
                        ordered_pair(routed, rustel_core::silence(), reverse)
                    )
                    .is_empty()
                );
                let generation = session.generation();
                let intents = session.take_pending_midi();
                assert!(!intents.is_empty());
                assert!(intents.iter().all(|intent| intent.generation == generation));
            }
        }
    }

    /// The warning is news once for a score and an input: a second window
    /// says nothing, another input or another score says it again.
    #[test]
    fn a_missing_input_channel_is_warned_about_once() {
        let mut session = Session::with_config(SessionConfig {
            cps: 1.0,
            horizon: 0.5,
            sample_rate: RATE,
            ..SessionConfig::default()
        })
        .unwrap();
        session.set_direct_diagnostic_logging(false);
        session.set_audio_input_channels(Some(2));
        session
            .set_pattern(sound_controls("in", vec![("n".into(), Value::F64(3.0))]))
            .unwrap();
        let warnings = |session: &mut Session| {
            session
                .take_diagnostics()
                .into_iter()
                .filter(|diagnostic| diagnostic.kind == "audio-input")
                .count()
        };
        session
            .schedule_audio_live_at(0.0, RATE, Duration::ZERO, LiveQueryBudgetMode::Steady)
            .unwrap_or_else(|_| panic!("scheduled"));
        assert_eq!(warnings(&mut session), 1);
        session
            .schedule_audio_live_at(0.5, RATE, Duration::ZERO, LiveQueryBudgetMode::Steady)
            .unwrap_or_else(|_| panic!("scheduled"));
        assert_eq!(
            warnings(&mut session),
            0,
            "the next window says nothing new"
        );
        session.set_audio_input_channels(Some(1));
        session
            .schedule_audio_live_at(1.0, RATE, Duration::ZERO, LiveQueryBudgetMode::Steady)
            .unwrap_or_else(|_| panic!("scheduled"));
        assert_eq!(warnings(&mut session), 1, "another input: news again");
        session
            .set_pattern(sound_controls("in", vec![("n".into(), Value::F64(3.0))]))
            .unwrap();
        session
            .schedule_audio_live_at(1.5, RATE, Duration::ZERO, LiveQueryBudgetMode::Steady)
            .unwrap_or_else(|_| panic!("scheduled"));
        assert_eq!(warnings(&mut session), 1, "another score: news again");
    }
}

/// The kinds of diagnostic this thread's sessions wrote straight to stderr,
/// for tests of the direct log (the queue has its own run dedupe, so
/// `take_diagnostics` cannot see a repeated record there).
#[cfg(test)]
mod direct_diagnostics_for_test {
    use std::cell::RefCell;

    thread_local! {
        static KINDS: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
    }

    pub(super) fn record(kind: String) {
        KINDS.with(|kinds| kinds.borrow_mut().push(kind));
    }

    #[cfg_attr(not(feature = "device-audio"), allow(dead_code))]
    pub(super) fn take() -> Vec<String> {
        KINDS.with(|kinds| std::mem::take(&mut *kinds.borrow_mut()))
    }
}

#[cfg(all(test, feature = "device-audio"))]
mod callback_function_error_tests {
    //! A callback that throws inside a score does not stop playback, and its
    //! warning is reported once. `.log()` prints on the live audio path.
    //!
    //! These tests use a real `Session`, scheduler and QuickJS host, and the
    //! live audio scheduling path. They mock no part of the query or schedule
    //! pipeline.

    use super::test_support::{quiet_session, warnings};
    use super::*;
    use std::time::Duration;

    const RATE: u32 = 48_000;

    fn log_lines(session: &mut Session) -> Vec<String> {
        session
            .take_diagnostics()
            .into_iter()
            .filter(|diagnostic| diagnostic.kind == "log")
            .map(|diagnostic| diagnostic.message)
            .collect()
    }

    #[test]
    fn a_throwing_filter_callback_keeps_the_notes_playing() {
        // `log` is not a function, so the predicate throws for every hap.
        // The throw must not empty the query arc or repeat the warning.
        let mut session = quiet_session(r#"$: s("bd*4").filterValues(x => log(x))"#);
        let seconds = 2.0 / session.cps();
        let (mut onsets, mut at) = (0usize, 0.0f64);
        while at < seconds {
            let through = (at + 0.25).min(seconds);
            onsets += session
                .schedule_through(at, through)
                .expect("schedule")
                .len();
            at = through;
        }
        // Two cycles of `bd*4` are eight onsets; the horizon fill that reaches
        // the loop's last window may drain one more from the following cycle.
        assert!(
            onsets >= 8,
            "the throwing filter silenced the score: {onsets} onsets"
        );
        let warnings = warnings(&mut session);
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(warnings[0].contains("log is not defined"), "{warnings:?}");
    }

    #[test]
    fn a_changed_callback_failure_is_reported_again() {
        let mut session = quiet_session(r#"$: s("bd").filterValues(x => log(x))"#);
        session.schedule_through(0.0, 0.5).expect("schedule");
        assert_eq!(warnings(&mut session).len(), 1);
        // The same broken callback, tick after tick, is not news again.
        session.schedule_through(0.5, 1.0).expect("schedule");
        assert!(
            warnings(&mut session).is_empty(),
            "an unchanged failure repeated as a warning"
        );
        // A different breakage is news again, exactly once.
        session
            .reload_at(r#"$: s("bd").filterValues(x => boom(x))"#, false, 1.0)
            .expect("edit");
        session.schedule_through(1.0, 1.5).expect("schedule");
        session.schedule_through(1.5, 2.0).expect("schedule");
        let warnings = warnings(&mut session);
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(warnings[0].contains("boom"), "{warnings:?}");
    }

    #[test]
    fn a_fatal_throw_does_not_also_report_the_contained_filter() {
        // The filter fails open. `all` then applies `.add("x")` outside the
        // implicit lane stack, so the numeric error aborts the query. The
        // window is silent and reports only that error.
        let mut session = quiet_session(
            r#"$: "bd".filterValues(x => log(x))
all(pattern => pattern.add("x"))"#,
        );
        let onsets = session.schedule_through(0.0, 0.5).expect("schedule");
        assert!(
            onsets.is_empty(),
            "parseNumeral must still silence the window: {onsets:?}"
        );
        let warnings = warnings(&mut session);
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(
            warnings[0].contains("cannot parse as numeral"),
            "{warnings:?}"
        );
        assert!(
            !warnings[0].contains("log is not defined"),
            "the contained filter leaked through a silent throw: {warnings:?}"
        );
    }

    #[test]
    fn log_lines_print_on_the_live_audio_path() {
        // The studio schedules audio through `schedule_audio_live_at`, which
        // does its own conversion. `.log()` must print on that path too.
        let mut session = quiet_session(r#"$: s("bd*2").log()"#);
        session.transport().start();
        let batch = session
            .schedule_audio_live_at(
                0.0,
                RATE,
                Duration::from_millis(2),
                LiveQueryBudgetMode::Steady,
            )
            .unwrap_or_else(|error| {
                let message = match error {
                    LiveAudioScheduleError::Retryable(error)
                    | LiveAudioScheduleError::AwaitingSamples(error)
                    | LiveAudioScheduleError::CandidateCommitted(error)
                    | LiveAudioScheduleError::CandidateRefusedScore(error) => error.to_string(),
                };
                panic!("live schedule: {message}")
            });
        assert!(
            !batch.events.is_empty(),
            "the bundled bd must schedule on the live path"
        );
        let logs = log_lines(&mut session);
        assert!(!logs.is_empty(), "the live path printed no .log() lines");
        assert!(
            logs.iter().all(|line| line.starts_with("[hap] ")),
            "{logs:?}"
        );
        // One line per onset, no more and no fewer.
        assert_eq!(logs.len(), batch.events.len(), "{logs:?}");
    }

    #[cfg(feature = "midi")]
    #[test]
    fn a_midi_press_plays_through_a_throwing_filter() {
        // The filter callback throws on every press from the studio's keyboard
        // input. The press must still sound.
        let mut session = quiet_session(
            r#"const kb = await midikeys('keyboard')
$: kb(0.5).filterValues(x => log(x)).s("hh:4")
"#,
        );
        let port = session
            .midi_input_bus()
            .find("keyboard")
            .expect("the score's keyboard");
        port.observe_note_on(rustel_core::midi_in::now_nanos(), 1, 60, 100);
        let onsets = session.schedule_through(0.0, 0.5).expect("schedule");
        assert!(!onsets.is_empty(), "the press produced no onsets");
        assert!(
            onsets.iter().any(|onset| onset.value_show.contains("s:hh")),
            "the press was not heard through the filter: {:?}",
            onsets
                .iter()
                .map(|onset| &onset.value_show)
                .collect::<Vec<_>>()
        );
        let warnings = warnings(&mut session);
        assert_eq!(warnings.len(), 1, "{warnings:?}");
    }

    #[cfg(feature = "midi")]
    #[test]
    fn a_midi_press_log_line_reaches_the_live_audio_path() {
        // `.log()` on the keyboard pattern must print.
        let mut session = quiet_session(
            r#"const kb = await midikeys('keyboard')
$: kb(0.5).log()
"#,
        );
        session.transport().start();
        let port = session
            .midi_input_bus()
            .find("keyboard")
            .expect("the score's keyboard");
        port.observe_note_on(rustel_core::midi_in::now_nanos(), 1, 60, 100);
        session
            .schedule_audio_live_at(
                0.0,
                RATE,
                Duration::from_millis(2),
                LiveQueryBudgetMode::Steady,
            )
            .unwrap_or_else(|error| {
                let message = match error {
                    LiveAudioScheduleError::Retryable(error)
                    | LiveAudioScheduleError::AwaitingSamples(error)
                    | LiveAudioScheduleError::CandidateCommitted(error)
                    | LiveAudioScheduleError::CandidateRefusedScore(error) => error.to_string(),
                };
                panic!("live schedule: {message}")
            });
        let logs = log_lines(&mut session);
        assert_eq!(logs.len(), 1, "{logs:?}");
        assert!(
            logs[0].contains("note:60"),
            "the pressed note did not print: {logs:?}"
        );
        assert!(logs[0].starts_with("[hap] "), "{logs:?}");
    }
}

#[cfg(test)]
mod offline_window_tests {
    //! The offline window every bounce door uses (`play_window`): what it
    //! reports about a pattern that threw, and which score owns the instant two
    //! of a session bounce's windows share.

    use super::test_support::{quiet_session, warnings};
    use super::*;

    /// A throw raised only by the window's LAST tick has no next iteration to
    /// take it, so only the take after the loop reports it. Without that take
    /// the bounce is silent where the score threw and exits zero.
    #[test]
    fn a_throw_raised_only_by_the_last_offline_tick_is_still_reported() {
        const WINDOW_SECS: f64 = 1.0;
        // The furthest this window's ticks query: only the last query reaches it.
        let mut probe = quiet_session(
            "globalThis.__furthest = 0;
         new Pattern(state => {
           globalThis.__furthest = Math.max(globalThis.__furthest, Number(state.span.end));
           return [];
         })",
        );
        let report = probe.play(WINDOW_SECS).expect("probe window");
        assert_eq!(report.query_threw, None);
        let furthest = probe.js.get_number("__furthest").expect("probe ran");
        assert!(furthest > 0.0, "the probe was never queried");

        let mut session = quiet_session(&format!(
            "new Pattern(state => {{
           if (Number(state.span.end) >= {furthest}) throw new Error('last tick');
           return [];
         }})"
        ));
        let report = session.play(WINDOW_SECS).expect("window");
        assert!(
            report
                .query_threw
                .as_deref()
                .is_some_and(|message| message.contains("last tick")),
            "the last tick's throw was lost: {:?}",
            report.query_threw
        );
        let warnings = warnings(&mut session);
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(warnings[0].contains("last tick"), "{warnings:?}");
    }

    /// Bounces `saves` to `until` seconds at cps 0.5 anchored at zero, where
    /// every score below holds one hap per cycle: on 0 s, 2 s and 4 s.
    fn bounce(saves: &[(f64, &str)], until: f64) -> RenderReport {
        let directory = tempfile::tempdir_in(env!("CARGO_MANIFEST_DIR")).expect("render directory");
        let mut session = Session::new().expect("session");
        session.set_direct_diagnostic_logging(false);
        let saves: Vec<(f64, String)> = saves
            .iter()
            .map(|(at, source)| (*at, (*source).to_owned()))
            .collect();
        let report = session
            .render_session(
                &saves,
                0.0,
                Some(until),
                &directory.path().join("bounce.wav"),
                false,
            )
            .expect("session bounce");
        assert!(report.query_threw.is_none(), "{report:?}");
        report
    }

    const LOW: &str = r#"$: note("c3").s("sawtooth").gain(0.25)"#;
    const HIGH: &str = r#"$: note("c5").s("sawtooth").gain(0.25)"#;
    const BROKEN: &str = "note(";

    /// An onset on the instant of a save that installs bounces once, whether the
    /// save keeps or changes the score, and whichever order a failed save at the
    /// same instant comes in.
    #[test]
    fn an_onset_on_an_installing_saves_instant_bounces_once() {
        for (label, saves) in [
            ("unchanged", vec![(0.0, LOW), (2.0, LOW)]),
            ("changed", vec![(0.0, LOW), (2.0, HIGH)]),
            (
                "failed then installed",
                vec![(0.0, LOW), (2.0, BROKEN), (2.0, HIGH)],
            ),
            (
                "installed then failed",
                vec![(0.0, LOW), (2.0, HIGH), (2.0, BROKEN)],
            ),
        ] {
            let report = bounce(&saves, 4.0);
            assert_eq!(report.onset_count, 3, "{label}: {report:?}");
        }
    }

    /// A save that fails to evaluate leaves the outgoing score's onset on its
    /// instant in the bounce.
    #[test]
    fn a_failed_save_keeps_the_outgoing_onset_on_its_instant() {
        let report = bounce(&[(0.0, LOW), (2.0, BROKEN)], 4.0);
        assert!(report.failed_save.is_some(), "{report:?}");
        assert_eq!(report.onset_count, 3, "{report:?}");
    }

    /// A save on or past the set's end leaves the onset on the closing instant in
    /// the bounce.
    #[test]
    fn a_save_on_or_past_the_end_keeps_the_closing_onset() {
        for at in [4.0, 5.0] {
            let report = bounce(&[(0.0, LOW), (at, HIGH)], 4.0);
            assert_eq!(report.onset_count, 3, "save at {at}: {report:?}");
        }
    }

    /// A filter predicate that throws fails open: the hap it tested keeps
    /// playing, so the bounce has every hit. The failure is one diagnostic and
    /// does not set `query_threw`, which fails the exit status of a bounce
    /// that is silent where the score is not.
    #[test]
    fn an_offline_bounce_through_a_throwing_filter_is_complete_not_threw() {
        let mut session = quiet_session(r#"$: s("bd*4").filterValues(x => log(x))"#);
        let seconds = 2.0 / session.cps();
        let report = session
            .play_window(0.0, seconds, QUERY_JS_CPU_BUDGET, false)
            .expect("offline window");
        assert_eq!(
            report.query_threw, None,
            "a filter that failed open was reported as a silent bounce"
        );
        assert!(
            report.onsets.len() >= 8,
            "the throwing filter silenced the bounce: {} onsets",
            report.onsets.len()
        );
        let warnings = warnings(&mut session);
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(warnings[0].contains("log is not defined"), "{warnings:?}");
    }
}

#[cfg(test)]
mod sample_queue_tests {
    //! A score's `samples()` and warm-ups behind a busy manifest loader.

    use super::test_support::session_with_sample_origin;
    use super::*;

    /// Updates made while the loader is busy install at once and say nothing
    /// about the wait. Their `samples()` registrations, a warm-up and a blocking
    /// registrant's map wait in line and land once the loader is free.
    #[test]
    fn samples_behind_a_busy_loader_register_without_a_word() {
        let mut session = session_with_sample_origin("http://127.0.0.1:9");
        session.set_direct_diagnostic_logging(false);
        let library = Arc::new(crate::samples::SampleLibrary::empty());
        session.samples = Some(Arc::clone(&library));
        let hold = library.hold_manifest_worker_for_test();
        let blocking = std::thread::spawn({
            let library = Arc::clone(&library);
            move || {
                library.register_trusted_custom(
                    r#"{"blocked":"http://127.0.0.1:9/blocked.wav"}"#,
                    None,
                )
            }
        });

        let started = Instant::now();
        let names = ["one", "two", "three"];
        for name in names {
            session
                .evaluate(&format!(
                    "samples({{ {name}: ['http://127.0.0.1:9/{name}.wav'] }}); note('c4')"
                ))
                .expect("the update installs");
        }
        let (_, warmed) = session.warm_live_samples_checked(
            &["three".to_owned()],
            0.0,
            std::time::Duration::from_millis(50),
        );
        let elapsed = started.elapsed();
        drop(hold);

        warmed.expect("a warm-up waits in line too");
        assert!(
            elapsed < std::time::Duration::from_secs(1),
            "an update waited on the loader: {elapsed:?}"
        );
        let diagnostics = session.take_diagnostics();
        assert!(
            diagnostics
                .iter()
                .all(|diagnostic| diagnostic.kind != "samples-failed"),
            "{diagnostics:?}"
        );
        blocking
            .join()
            .expect("the blocking registrant")
            .expect("its map lands");
        library.wait_until_idle(std::time::Duration::from_secs(15));
        for name in names.into_iter().chain(["blocked"]) {
            assert!(library.knows(name), "{name}");
        }
    }
}

#[cfg(test)]
mod stack_lane_error_tests {
    //! A query error in one `$:` lane. Every `$:` score is a stack, even with
    //! one lane, and a stack contains a child's throw as strudel.cc does: the
    //! lane goes silent and the other lanes play. The offline report still says
    //! the score threw. The replacement probe is covered in
    //! `lane_throw_probe_tests`.

    use super::test_support::{quiet_session, warnings};
    use super::*;

    const FAILING_LANE: &str = r#"$: note("0 4".add("x")).s("sine")"#;

    fn threw_parse_numeral(query_threw: Option<&str>) -> bool {
        query_threw.is_some_and(|message| message.contains("cannot parse as numeral"))
    }

    #[test]
    fn a_score_whose_only_lane_throws_plays_silence_and_reports_query_threw() {
        let mut session = quiet_session(FAILING_LANE);
        let report = session.play(2.0).expect("play");
        assert!(report.onsets.is_empty(), "{:?}", report.onsets);
        assert!(
            threw_parse_numeral(report.query_threw.as_deref()),
            "a silent bounce must report its throw: {:?}",
            report.query_threw
        );
        let warnings = warnings(&mut session);
        assert_eq!(warnings.len(), 1, "{warnings:?}");
    }

    #[test]
    fn a_throwing_lane_beside_a_healthy_one_plays_it_and_reports_query_threw() {
        let mut session = quiet_session(&format!("$: s(\"bd*4\")\n{FAILING_LANE}"));
        let report = session.play(2.0).expect("play");
        assert!(!report.onsets.is_empty(), "the failing lane silenced bd");
        assert!(
            report.onsets.iter().all(|onset| onset.value_show == "s:bd"),
            "{:?}",
            report.onsets
        );
        assert!(
            threw_parse_numeral(report.query_threw.as_deref()),
            "{:?}",
            report.query_threw
        );
        let warnings = warnings(&mut session);
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(
            warnings[0].contains("cannot parse as numeral"),
            "{warnings:?}"
        );
    }

    #[test]
    fn a_bounce_whose_only_lane_throws_fails_its_report() {
        let directory = tempfile::tempdir_in(env!("CARGO_MANIFEST_DIR")).expect("render directory");
        let mut session = quiet_session(FAILING_LANE);
        let report = session
            .render(
                1.0,
                &directory.path().join("onsets.json"),
                RenderFormat::OnsetJson,
            )
            .expect("render");
        assert_eq!(report.onset_count, 0);
        assert!(
            report
                .failure()
                .is_some_and(|failure| failure.contains("cannot parse as numeral")),
            "the exit status must not call a silent bounce a success: {report:?}"
        );
    }

    #[test]
    fn a_fail_open_filter_in_a_lane_is_not_a_throw() {
        let mut session = quiet_session(r#"$: s("bd*4").filterValues(x => log(x))"#);
        let report = session.play(2.0).expect("play");
        assert!(!report.onsets.is_empty());
        assert_eq!(report.query_threw, None);
    }

    #[test]
    fn a_query_report_keeps_the_healthy_lane_and_reports_query_threw() {
        let session = quiet_session(&format!("$: s(\"bd*4\")\n{FAILING_LANE}"));
        let report = session
            .query_report(Fraction::ZERO, Fraction::ONE)
            .expect("query");
        assert_eq!(report.haps.len(), 4, "{:?}", report.haps);
        assert!(
            threw_parse_numeral(report.query_threw.as_deref()),
            "{:?}",
            report.query_threw
        );
    }
}

#[cfg(test)]
mod lane_throw_probe_tests {
    //! The replacement probe and a `$:` lane that throws. Playback contains a
    //! lane's throw to that lane, but the probe still refuses a candidate whose
    //! probed cycle throws in any lane: the last-good score keeps playing. A lane
    //! that first throws after the probed cycle installs and only it goes silent.

    use super::test_support::{quiet_session, warnings};
    use super::*;

    const PLAYING: &str = r#"$: s("bd*4")"#;
    const BROKEN_LANE: &str = r#"$: note("0 4".add("x")).s("sine")"#;
    /// Adds 0 on cycle 0, the probed cycle, and throws on cycle 1.
    const LATE_BROKEN_LANE: &str = r#"$: note("0 4".add("<0 x>")).s("sine")"#;
    const REFUSED: &str = "replacement query failed; last-good score kept";

    fn two_lanes(second: &str) -> String {
        format!("$: s(\"bd*2 sd\")\n{second}")
    }

    /// Onsets of `sound` that start in `cycle`, read from their `n/d` labels.
    fn onsets_in(report: &PlayReport, cycle: i64, sound: &str) -> usize {
        let starts_in = |onset: &&OnsetEventJson| {
            let (n, d) = onset.whole_begin.split_once('/').expect("n/d label");
            let n: i64 = n.parse().expect("numerator");
            n.div_euclid(d.parse().expect("denominator")) == cycle
        };
        let sound = format!("s:{sound}");
        report
            .onsets
            .iter()
            .filter(starts_in)
            .filter(|onset| onset.value_show.contains(&sound))
            .count()
    }

    fn assert_refused_and_old_score_plays(
        session: &mut Session,
        error: &RuntimeError,
        generation: u64,
    ) {
        assert_eq!(error.kind(), "evaluation");
        let message = error.to_string();
        assert!(message.contains(REFUSED), "{message}");
        assert!(message.contains("cannot parse as numeral"), "{message}");
        assert_eq!(session.generation(), generation);
        assert_eq!(session.active_source(), Some(PLAYING));
        let report = session.play(2.0).expect("play");
        assert_eq!(onsets_in(&report, 0, "bd"), 4, "{:?}", report.onsets);
        assert!(
            report.onsets.iter().all(|onset| onset.value_show == "s:bd"),
            "the refused candidate was published: {:?}",
            report.onsets
        );
        assert_eq!(report.query_threw, None);
        // The refusal is the whole report: no warning and no console line.
        let diagnostics = session.take_diagnostics();
        assert!(
            diagnostics
                .iter()
                .all(|diagnostic| !matches!(diagnostic.kind.as_str(), "query-threw" | "log")),
            "{diagnostics:?}"
        );
    }

    #[test]
    fn an_edit_whose_second_lane_throws_is_refused_and_the_old_score_plays() {
        let mut session = quiet_session(PLAYING);
        let generation = session.generation();
        let error = session
            .reload_at(&two_lanes(BROKEN_LANE), false, 0.25)
            .expect_err("a lane that throws on the probed cycle refuses the edit");
        assert_refused_and_old_score_plays(&mut session, &error, generation);
    }

    #[test]
    fn an_edit_whose_only_lane_throws_is_refused_with_the_uncontained_error() {
        let mut session = quiet_session(PLAYING);
        let generation = session.generation();
        let error = session
            .reload_at(BROKEN_LANE, false, 0.25)
            .expect_err("a lane that throws on the probed cycle refuses the edit");
        assert_refused_and_old_score_plays(&mut session, &error, generation);

        // Without `$:` no stack contains the throw; the report is the same.
        let mut uncontained = quiet_session(PLAYING);
        let expected = uncontained
            .reload_at(BROKEN_LANE.trim_start_matches("$: "), false, 0.25)
            .expect_err("an uncontained throw refuses the edit");
        assert_eq!(error.to_string(), expected.to_string());
    }

    #[test]
    fn a_callback_bearing_edit_whose_lane_throws_is_refused_the_same_way() {
        let mut session = quiet_session(PLAYING);
        let generation = session.generation();
        let error = session
            .reload_at(
                &format!("$: s(\"bd*2 sd\").fmap(x => x)\n{BROKEN_LANE}"),
                false,
                0.25,
            )
            .expect_err("a lane that throws on the probed cycle refuses the edit");
        assert_refused_and_old_score_plays(&mut session, &error, generation);
    }

    #[test]
    fn a_lane_that_first_throws_after_the_probed_cycle_installs_and_only_it_goes_silent() {
        let mut session = quiet_session(PLAYING);
        let generation = session.generation();
        let source = two_lanes(LATE_BROKEN_LANE);
        session
            .reload_at(&source, false, 0.25)
            .expect("the probed cycle does not throw");
        assert_ne!(session.generation(), generation);
        assert_eq!(session.active_source(), Some(source.as_str()));

        let report = session.play(4.0).expect("play two cycles");
        let lanes_in = |cycle| ["bd", "sd", "sine"].map(|sound| onsets_in(&report, cycle, sound));
        assert_eq!(lanes_in(0), [2, 1, 2], "{:?}", report.onsets);
        assert_eq!(lanes_in(1), [2, 1, 0], "{:?}", report.onsets);
        let warnings = warnings(&mut session);
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        assert!(
            warnings[0].contains("cannot parse as numeral"),
            "{warnings:?}"
        );
    }

    #[test]
    fn a_valid_edit_installs_as_before() {
        let mut session = quiet_session(PLAYING);
        let generation = session.generation();
        let source = two_lanes(r#"$: note("0 4").s("sine")"#);
        session.reload_at(&source, false, 0.25).expect("valid edit");
        assert_ne!(session.generation(), generation);
        assert_eq!(session.active_source(), Some(source.as_str()));
        let report = session.play(2.0).expect("play");
        let lanes = ["bd", "sd", "sine"].map(|sound| onsets_in(&report, 0, sound));
        assert_eq!(lanes, [2, 1, 2], "{:?}", report.onsets);
        assert_eq!(report.query_threw, None);
        assert!(warnings(&mut session).is_empty());
    }

    #[test]
    fn a_first_score_with_a_throwing_lane_installs_when_its_first_query_is_not_probed() {
        let mut session = Session::new().expect("session");
        session.set_direct_diagnostic_logging(false);
        session
            .reload_at(&two_lanes(BROKEN_LANE), false, 0.0)
            .expect("a first score is not probed under the default hap budget");
        assert_eq!(session.generation(), 1);
        let report = session.play(2.0).expect("play");
        let lanes = ["bd", "sd", "sine"].map(|sound| onsets_in(&report, 0, sound));
        assert_eq!(lanes, [2, 1, 0], "{:?}", report.onsets);
        assert!(
            report
                .query_threw
                .as_deref()
                .is_some_and(|message| message.contains("cannot parse as numeral")),
            "{:?}",
            report.query_threw
        );
    }

    #[test]
    fn a_first_score_whose_pure_query_is_probed_is_refused_when_a_lane_throws() {
        let mut session = Session::new().expect("session");
        session.set_query_hap_budget(1_000).expect("hap budget");
        let error = session
            .reload_at(&two_lanes(BROKEN_LANE), false, 0.0)
            .expect_err("a probed first score refuses a throwing lane");
        let message = error.to_string();
        assert!(
            message.contains("score query failed; score was not installed"),
            "{message}"
        );
        assert!(message.contains("cannot parse as numeral"), "{message}");
        assert_eq!(session.generation(), 0);
    }
}

#[cfg(test)]
mod local_sample_check_tests {
    use super::*;
    use crate::samples::SampleLibrary;
    use crate::score_check::check_score;
    use std::sync::atomic::Ordering;

    fn local_session(granted: bool) -> (tempfile::TempDir, Session, Arc<SampleLibrary>) {
        let folder = tempfile::tempdir().expect("sample folder");
        // Checking the bank names only enumerates files; it does not decode audio.
        std::fs::write(folder.path().join("kick.wav"), b"").expect("sample name");
        let mut access = ScoreSampleAccess::denied();
        if granted {
            access
                .permit_local_root(folder.path())
                .expect("local grant");
        }
        let mut session =
            Session::with_config(SessionConfig::default().with_score_sample_access(access))
                .expect("session");
        let library = Arc::new(SampleLibrary::empty());
        session.samples = Some(Arc::clone(&library));
        (folder, session, library)
    }

    #[test]
    fn check_reports_a_misspelled_sound_after_the_granted_local_import_loads() {
        let (_folder, mut session, library) = local_session(true);
        let check = check_score(
            "await samples('local:'); s(\"kik\")",
            Some(&library),
            &mut session,
            &AtomicBool::new(false),
        );
        assert!(library.knows_samples_source("local:"));
        assert!(library.knows_sound("kick"));
        assert!(check.eval_error.is_none(), "{check:?}");
        assert!(
            check
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.message == "unknown sound \"kik\""),
            "{check:?}"
        );
        assert!(!check.is_ok());
    }

    #[test]
    fn check_accepts_a_sound_in_the_granted_local_folder() {
        let (_folder, mut session, library) = local_session(true);
        let check = check_score(
            "await samples('local:'); s(\"kick\")",
            Some(&library),
            &mut session,
            &AtomicBool::new(false),
        );
        assert!(library.knows_samples_source("local:"));
        assert!(check.is_ok(), "{check:?}");
    }

    #[test]
    fn check_does_not_register_local_banks_without_a_host_grant() {
        let (_folder, mut session, library) = local_session(false);
        check_score(
            "await samples('local:'); s(\"kick\")",
            Some(&library),
            &mut session,
            &AtomicBool::new(false),
        );
        assert!(!library.knows_samples_source("local:"));
        assert!(!library.knows_sound("kick"));
        assert!(
            session
                .take_diagnostics()
                .iter()
                .any(|diagnostic| diagnostic.kind == "samples-failed")
        );
    }

    #[test]
    fn check_without_a_lint_library_leaves_sound_names_unchecked() {
        let (_folder, mut session, _library) = local_session(true);
        let check = check_score(
            "await samples('local:'); s(\"kik\")",
            None,
            &mut session,
            &AtomicBool::new(false),
        );
        assert!(check.is_ok(), "{check:?}");
    }

    #[test]
    fn check_keeps_sound_names_unjudged_while_imports_are_pending() {
        let (_folder, mut session, library) = local_session(true);
        let _hold = library.hold_manifest_worker_for_test();
        let check = check_score(
            "await samples('local:'); samples('https://example.invalid/banks.json'); s(\"kik\")",
            Some(&library),
            &mut session,
            &AtomicBool::new(false),
        );
        assert!(library.manifests_pending() > 0);
        assert!(check.is_ok(), "{check:?}");
    }

    #[test]
    fn check_preserves_an_evaluation_failure_in_a_score_with_a_local_import() {
        let (_folder, mut session, library) = local_session(true);
        let check = check_score(
            "await samples('local:'); throw new Error('score failed');",
            Some(&library),
            &mut session,
            &AtomicBool::new(false),
        );
        assert!(
            check
                .eval_error
                .as_ref()
                .is_some_and(|error| error.message.contains("score failed")),
            "{check:?}"
        );
        assert!(!check.is_ok());
    }

    #[test]
    fn a_cancelled_check_keeps_its_evaluation_error() {
        let (_folder, mut session, library) = local_session(true);
        let check = check_score(
            "await samples('local:'); s(\"kick\")",
            Some(&library),
            &mut session,
            &AtomicBool::new(true),
        );
        assert!(check.eval_error.is_some(), "{check:?}");
        assert!(!check.is_ok());
    }

    #[test]
    fn cancellation_during_the_local_import_wait_stops_the_check() {
        let (_folder, mut session, library) = local_session(true);
        let _hold = library.hold_manifest_worker_for_test();
        let held_jobs = library.manifests_pending();
        let cancellation = AtomicBool::new(false);
        let wait_entered = crate::samples::wait_tests::watch_next_wait();
        let (check, cancelled_at) = std::thread::scope(|scope| {
            let request = scope.spawn({
                let cancellation = &cancellation;
                move || {
                    wait_entered
                        .recv_timeout(Duration::from_secs(5))
                        .expect("the evaluated score entered its sample wait");
                    let cancelled_at = Instant::now();
                    cancellation.store(true, Ordering::Relaxed);
                    cancelled_at
                }
            });
            let check = check_score(
                "await samples('local:'); s(\"kick\")",
                Some(&library),
                &mut session,
                &cancellation,
            );
            (check, request.join().expect("cancellation request"))
        });
        assert!(
            cancelled_at.elapsed() < Duration::from_secs(2),
            "cancellation waited for the five-second sample deadline"
        );
        assert!(library.manifests_pending() > held_jobs);
        assert_eq!(
            check
                .eval_error
                .as_ref()
                .map(|error| error.message.as_str()),
            Some("cancelled"),
            "{check:?}"
        );
        assert!(!check.is_ok());
    }
}

#[cfg(all(test, feature = "device-audio"))]
mod retime_tests {
    //! Following an outside clock ([`Session::retime`]).

    use super::*;

    /// A steer moves the conversion tempo with the scheduler: events scheduled
    /// after it last and are spaced one step at the steered rate, and a steer the
    /// scheduler refuses leaves both rates alone.
    #[test]
    fn a_clock_steer_retimes_the_conversion_tempo_alongside_the_scheduler() {
        let mut session = Session::new().expect("session");
        session.evaluate(r#"s("bd*16")"#).expect("score");
        assert_eq!(session.config.cps, DEFAULT_CPS);

        // An outside clock at twice the score's tempo takes over.
        session.retime(9.5, 1.0, session.cycle_at_time(9.5));
        assert_eq!(session.scheduler.cps(), 1.0);
        assert_eq!(
            session.config.cps, 1.0,
            "the conversion tempo stayed at the pre-steer one"
        );

        session.requery_active_at(9.5).expect("control requery");
        let events = session
            .schedule_through(9.5, 10.0)
            .expect("retimed schedule");
        assert!(!events.is_empty(), "the steered score still has music");
        // One sixteenth lasts 1/16 s at 1 cps; at the score's 0.5 cps it would
        // last twice that.
        for event in &events {
            assert!(
                (event.duration_secs - 1.0 / 16.0).abs() < 1e-9,
                "a gate length used the pre-steer tempo: {}",
                event.duration_secs
            );
        }
        for pair in events.windows(2) {
            assert!(
                (pair[1].target_time - pair[0].target_time - 1.0 / 16.0).abs() < 1e-9,
                "onset spacing used the pre-steer tempo: {} -> {}",
                pair[0].target_time,
                pair[1].target_time
            );
        }

        session.retime(10.0, f64::NAN, 0.0);
        assert_eq!(session.scheduler.cps(), 1.0);
        assert_eq!(session.config.cps, 1.0);
    }
}

#[cfg(test)]
mod test_support {
    //! Fixtures shared by the session's test modules.

    use super::*;

    /// A session evaluating `source`, with its diagnostics kept for the test
    /// rather than printed.
    pub(super) fn quiet_session(source: &str) -> Session {
        let mut session = Session::new().expect("session");
        session.set_direct_diagnostic_logging(false);
        session.evaluate(source).expect("evaluate");
        session
    }

    /// The "query-threw" diagnostics reported since the last take.
    pub(super) fn warnings(session: &mut Session) -> Vec<String> {
        session
            .take_diagnostics()
            .into_iter()
            .filter(|diagnostic| diagnostic.kind == "query-threw")
            .map(|diagnostic| diagnostic.message)
            .collect()
    }

    /// A session whose scores may fetch samples from `origin`.
    pub(super) fn session_with_sample_origin(origin: &str) -> Session {
        let mut config = SessionConfig::default();
        config
            .score_sample_access
            .permit_origin(origin)
            .expect("test sample origin");
        Session::with_config(config).expect("session")
    }
}

use std::collections::VecDeque;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::Duration;
use std::time::Instant;

use rustel_audio::TakeoverCut;
use rustel_core::host_value::Pointer;
use rustel_core::settings::RuntimeSettings;
use rustel_core::{Hap, Pattern, QueryArcOutcome, State, TimeSpan};
use rustel_fraction::Fraction;
use rustel_jsruntime::{JsRuntime, ScoreEffects, Slot};
use rustel_scheduler::{Clock, Scheduler, Transport, VirtualClock};
use rustel_transpiler::TranspileOptions;

use crate::hap_json::{
    BenchMetrics, OnsetEventJson, PlayReport, QueryReport, RenderReport, ValueJson, fraction_label,
    haps_to_json,
};
#[cfg(feature = "osc")]
use crate::osc_bridge::ScoreOscAccess;
#[cfg(any(feature = "device-audio", test))]
use crate::producer::{ProducerPhase, ProducerTurnRecord};
use crate::render::{RenderFormat, write_onset_dump, write_silent_wav};
use crate::samples::ScoreSampleAccess;

const DEFAULT_CPS: f64 = 0.5;
const DEFAULT_HORIZON: f64 = 0.5;
pub(crate) const DEFAULT_SAMPLE_RATE: u32 = 48_000;
const DEFAULT_CHANNELS: u16 = 2;
const SCORE_CPU_BUDGET: Duration = Duration::from_secs(2);
const PREBAKE_CPU_BUDGET: Duration = Duration::from_secs(2);
/// Maximum elapsed time before QuickJS is interrupted in one top-level query
/// or scheduler tick. This is deliberately separate from score construction:
/// query-time callbacks run later, while the producer fills its scheduling
/// horizon. It is not process CPU-time accounting or a total-query bound.
const QUERY_JS_CPU_BUDGET: Duration = Duration::from_secs(2);

/// How many cycles a replacement whose first window refused every onset is
/// looked through for anything it plays before it is refused: enough for a
/// bank or sound that alternates by cycle, as `<a b c d>` does, to come
/// round to one that is loaded.
#[cfg(feature = "device-audio")]
const REPLACEMENT_LOOK_AHEAD_CYCLES: i128 = 4;

/// What a replacement's look-ahead found in the cycles ahead of its refused
/// first window. When several apply the priority is: an invalid control
/// refuses the whole replacement even beside a rendered sibling, exactly as
/// one in the first window does; then a rendered onset publishes; then a
/// sample still on its way waits.
#[cfg(feature = "device-audio")]
enum ReplacementLookAhead {
    /// An onset rendered: the replacement can play and is published.
    Renders,
    /// Nothing rendered yet, but a sample that is still loading would. The
    /// replacement is refused as retryable and the last audible score
    /// keeps playing. Publishing now would latch silence if the fetch
    /// never completes.
    Loading(String),
    /// An invalid control waits ahead, which refuses the whole
    /// replacement as the same voice in the first window would.
    InvalidControl(String),
    /// Nothing renders and nothing is coming.
    Nothing,
}

/// Keep live callback-bearing queries small enough to complete atomically
/// inside the replacement-prefill deadline. Further horizon cover is filled
/// by subsequent producer turns.
const MAX_LIVE_IMPURE_QUERY_SPAN_CYCLES: f64 = 1.0;
/// Extra future music retained for callback-bearing scores whose ordinary
/// horizon already queries whole cycles. This absorbs one cycle-cost outlier
/// without changing the query windows seen by patterns.
#[cfg(feature = "device-audio")]
const LIVE_IMPURE_BURST_RESERVE_CYCLES: f64 = 1.0;
/// Bound the reserve in wall time so a low-tempo score or large device margin
/// cannot turn producer resilience into seconds of retained event traffic.
#[cfg(feature = "device-audio")]
const LIVE_IMPURE_BURST_RESERVE_MAX: Duration = Duration::from_millis(500);
/// Leave part of each cycle for event conversion, ring transfer and ordinary
/// producer scheduling instead of letting pattern work consume all of it.
const LIVE_REPLACEMENT_QUERY_COMPUTE_SHARE: f64 = 0.75;
/// Immediate replacements keep the old generation audible for at least this
/// long while the accepted candidate fills its first scheduling runway. A
/// measured-heavy candidate grows from this floor with its bounded one-cycle
/// query budget; see [`Session::live_replacement_takeover_headroom`].
#[cfg(any(feature = "device-audio", test))]
const LIVE_REPLACEMENT_MIN_HEADROOM: Duration = Duration::from_millis(80);
/// Smallest elapsed-time slice offered to a live impure scheduler query.
/// Smaller tokens are dominated by ordinary scheduling jitter, so a steady
/// producer that cannot afford this much fails closed before entering JS.
pub(crate) const MIN_LIVE_QUERY_JS_BUDGET: Duration = Duration::from_millis(25);

/// Set in the onset id of every preview trace - a hap read ahead for the
/// painters, never queued to the audio - so it can never collide with the
/// scheduler's dense counter, which starts at zero.
pub const PREVIEW_ONSET_ID_FLAG: u64 = 1 << 63;

/// The JavaScript a preview query may run. It runs on the engine thread
/// between producer steps, so a callback-bearing score gets the live
/// floor, never the inspection ceiling; over budget, the preview is
/// simply skipped this time.
const PREVIEW_QUERY_JS_BUDGET: Duration = MIN_LIVE_QUERY_JS_BUDGET;
/// Query budget when the live scheduling horizon has run out.
/// With no queued horizon left to protect, allow more than the steady-state
/// floor so a dense score can finish a window and resume playback.
pub(crate) const LIVE_QUERY_RECOVERY_BUDGET: Duration = Duration::from_millis(250);
#[cfg(feature = "device-audio")]
const LIVE_RELOAD_SHIELD_MAX_EXTRA: Duration = Duration::from_millis(2250);
static NEVER_CANCELLED: AtomicBool = AtomicBool::new(false);
/// Do not execute a stable watched setup under a token deadline so small that
/// ordinary scheduling jitter turns valid code into a committed rejection.
/// Below this floor the exact file identity remains Pending and retryable.
const MIN_LIVE_PREBAKE_CPU_BUDGET: Duration = Duration::from_millis(25);
const MAX_PENDING_SESSION_DIAGNOSTICS: usize = 64;
#[cfg(feature = "midi")]
const MAX_MIDI_INTENTS_PER_PASS: usize = 256;
#[cfg(feature = "midi")]
const MAX_MIDI_MESSAGES_PER_PASS: usize = 4_096;
#[cfg(feature = "midi")]
const MAX_MIDI_RETAINED_BYTES_PER_PASS: usize = 256 * 1_024;
#[cfg(feature = "midi")]
const MAX_MIDI_ROUTE_ATTEMPTS_PER_PASS: usize = 1_024;
#[cfg(feature = "osc")]
const MAX_OSC_INTENTS_PER_PASS: usize = 256;
#[cfg(feature = "osc")]
const MAX_OSC_BYTES_PER_PASS: usize = 256 * 1024;
#[cfg(feature = "osc")]
const MAX_OSC_ROUTE_ATTEMPTS_PER_PASS: usize = 1_024;
#[cfg(feature = "serial")]
const MAX_SERIAL_INTENTS_PER_PASS: usize = 256;
#[cfg(feature = "serial")]
const MAX_SERIAL_BYTES_PER_PASS: usize = 256 * 1024;
#[cfg(feature = "serial")]
const MAX_SERIAL_ROUTE_ATTEMPTS_PER_PASS: usize = 1_024;

#[cfg(feature = "osc")]
#[derive(Clone, Debug, Eq, PartialEq)]
enum ReportedOscRefusal {
    Destination(std::net::SocketAddr),
    InvalidDestination { host: String, port: u16 },
    OversizedHost,
}

#[cfg(feature = "osc")]
fn osc_refusal_key(host: &str, port: u16) -> ReportedOscRefusal {
    if host.len() > rustel_osc::MAX_OSC_HOST_BYTES {
        ReportedOscRefusal::OversizedHost
    } else if let Ok(destination) = rustel_osc::parse_osc_destination(host, port) {
        let ip = match destination.ip() {
            std::net::IpAddr::V6(ip) => ip
                .to_ipv4_mapped()
                .map(std::net::IpAddr::V4)
                .unwrap_or(std::net::IpAddr::V6(ip)),
            std::net::IpAddr::V4(ip) => std::net::IpAddr::V4(ip),
        };
        ReportedOscRefusal::Destination(std::net::SocketAddr::new(ip, destination.port()))
    } else {
        ReportedOscRefusal::InvalidDestination {
            host: host.to_owned(),
            port,
        }
    }
}

/// Presentation-neutral notice produced while a Session keeps running.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SessionDiagnostic {
    pub kind: String,
    pub message: String,
    pub recoverable: bool,
}

/// The kind of a refusal when the library has not finished loading the
/// sound.
///
/// Distinct from `voice-refused`, where the score asks for something this
/// engine cannot play. A surface matches on this kind, not on the message
/// text, so it can show a decode in progress as waiting and not as an error.
pub const SAMPLE_LOADING_DIAGNOSTIC: &str = "sample-loading";
/// The kind of a voice resolver notice: part of a voice was skipped or
/// substituted and the rest still plays. The message is one sentence for
/// people; direct logging prints the notice's JSON record instead.
pub const VOICE_NOTICE_DIAGNOSTIC: &str = "voice-notice";
/// The kind a still-loading refusal carries when its notes wait rather than
/// being skipped: a rewind's first window held whole until its samples are
/// in, or a sample just ahead that an unpublished update waits for.
pub const SAMPLE_AWAITED_DIAGNOSTIC: &str = "sample-awaited";
/// What a watched score evaluation gets when the horizon is already spent.
///
/// The floor below protects a horizon that still exists; once it is gone,
/// refusing an evaluation only guarantees the next one is refused too, and the
/// artist is left typing into a set that has stopped listening.
#[cfg(any(feature = "device-audio", test))]
const LIVE_SCORE_RECOVERY_BUDGET: Duration = Duration::from_millis(400);
/// The same floor applies to watched score JavaScript: below it, retaining the
/// stable file identity for a later attempt is safer than running ordinary
/// construction code under a deadline dominated by scheduling jitter.
#[cfg(any(feature = "device-audio", test))]
const MIN_LIVE_SCORE_CPU_BUDGET: Duration = Duration::from_millis(25);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum LivePrebakeAttempt {
    Applied,
    Deferred,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg(any(feature = "device-audio", test))]
pub(crate) enum LiveScoreAttempt {
    Applied(u64),
    Deferred,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg(feature = "device-audio")]
pub(crate) enum RollbackAttempt {
    Applied,
    Deferred,
    Unavailable,
}

#[cfg(feature = "device-audio")]
pub(crate) struct LiveAudioBatch {
    /// In onset order. The target frames can be out of order: a voice can
    /// be aimed before its onset, as a stretched one is.
    pub(crate) events: Vec<rustel_audio::AudioEvent>,
    /// Immutable decoded payload selected for each converted prefill onset.
    /// Empty for steady turns, which do not create confirmation windows.
    pub(crate) sample_identities: Vec<Option<u64>>,
    /// Disjoint dispositions of this finite scheduling pass. These describe
    /// conversion/accepted external intent, not callback or external delivery.
    pub(crate) dispositions: confirmation::WindowDispositions,
    /// End of the actually queried/drained time span, not a requested horizon
    /// that an atomic query may only have filled in part.
    pub(crate) queried_through_frame: u64,
    /// A caught query exception is not evidence of intentional silence.
    pub(crate) query_threw: bool,
    /// A fresh query, converted audio, or accepted external output can finish
    /// prefill. An empty transfer from an unchanged full horizon cannot.
    pub(crate) prefill_progress: bool,
    /// Starts immediately after Scheduler accepted the tick, before draining,
    /// JSON/audio conversion, generation handoff, or ring transfer.
    pub(crate) tail_started: Instant,
}

/// One onset's sound in a queried window: see [`Session::window_sounds`].
#[derive(Clone, Debug, PartialEq)]
pub struct WindowSound {
    /// The sound as written.
    pub s: String,
    /// `{bank}_{s}` when a bank is set: the spelling the voice looks up.
    pub banked: Option<String>,
    /// The variant the onset's `n` picks.
    pub n: f64,
    /// The note, as a MIDI number: 36 when the onset has none.
    pub midi: f64,
}

#[cfg(feature = "device-audio")]
pub(crate) enum LiveAudioScheduleError {
    /// The candidate did not commit scheduler state and may be retried.
    Retryable(RuntimeError),
    /// As [`Self::Retryable`], when the sample library is still fetching
    /// what the score asks for: the same identity stays eligible and
    /// nothing is latched. A separate variant lets the engine's health
    /// telemetry tell a library that is still decoding from a machine
    /// that cannot keep up. Only the second is pressure to report, so
    /// previews played one after another do not read as an overload.
    AwaitingSamples(RuntimeError),
    /// Scheduler state may already have advanced or drained by this point.
    /// A pending device-generation cutover must not publish an unchanged retry
    /// as successful silence after this failure.
    CandidateCommitted(RuntimeError),
    /// As [`Self::CandidateCommitted`], when the score is the reason: no
    /// compiled output can render what it asks for. Examples are an orbit
    /// past the last bus, a unison past the supersaw's ceiling, and a sound
    /// with no bank behind it.
    ///
    /// The candidate is committed, the last audible score is put back, and
    /// the player is told. A separate variant lets the engine's health
    /// telemetry tell a mistake in the score from a machine that cannot
    /// keep up, without a match on the message text.
    CandidateRefusedScore(RuntimeError),
}

#[cfg(feature = "device-audio")]
impl LiveAudioScheduleError {
    /// Whether the score's own content is what turned the candidate away,
    /// rather than a limit, a full ring or a host that fell behind.
    pub(crate) fn is_refused_score(&self) -> bool {
        matches!(self, Self::CandidateRefusedScore(_))
    }

    /// Whether this refusal is the library still fetching the score's
    /// samples: waiting, not falling behind.
    pub(crate) fn is_awaiting_samples(&self) -> bool {
        matches!(self, Self::AwaitingSamples(_))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg(any(feature = "device-audio", test))]
pub(crate) enum LiveQueryBudgetMode {
    Steady,
    InitialPrefill,
    ReplacementPrefill,
}

enum EvaluatedScore {
    JavaScript {
        pattern: Pattern,
        effects: ScoreEffects,
        settings: RuntimeSettings,
    },
    MiniFallback(Pattern),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum NoPatternPolicy {
    Reject,
    InstallSilence,
}

enum ScheduleAttemptError {
    Atomic(RuntimeError),
    Partial(RuntimeError),
}

struct ScheduledWindow {
    events: Vec<OnsetEventJson>,
    tail_started: Instant,
    #[cfg_attr(not(feature = "device-audio"), allow(dead_code))]
    status: rustel_scheduler::TickStatus,
    #[cfg_attr(not(feature = "device-audio"), allow(dead_code))]
    query_threw: bool,
}

impl ScheduleAttemptError {
    fn into_runtime_error(self) -> RuntimeError {
        match self {
            Self::Atomic(error) | Self::Partial(error) => error,
        }
    }
}

/// How source was resolved into an active pattern.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EvaluateSource {
    /// Transpiled user JS via QuickJS + native pattern host bindings.
    JavaScript,
    /// Direct `rustel_mini` compile (Rust API fallback).
    MiniRust,
    /// Explicit Pattern installed without JS.
    Pattern,
}

/// Captured source and runtime settings retained for watchdog rollback.
/// Capturing this value does not establish consumer playback.
#[cfg(feature = "device-audio")]
#[derive(Clone)]
struct AudibleSource {
    generation: u64,
    source: Arc<str>,
    mini: bool,
    settings: RuntimeSettings,
    cps: f64,
    cycle_zero_time: f64,
    #[cfg(feature = "vst")]
    insert_orbits: [u8; rustel_audio::MAX_ORBITS],
}

/// Settings a Session is built with; [`Session::with_config`] validates them.
///
/// Start from [`SessionConfig::default`] and change fields directly or with
/// the `with_` methods. The struct is non-exhaustive, so a new setting does
/// not break a host's construction.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct SessionConfig {
    pub cps: f64,
    /// Default voice budget, unless an accepted score calls setMaxPolyphony.
    pub max_polyphony: usize,
    pub horizon: f64,
    pub sample_rate: u32,
    pub channels: u16,
    /// Prepared engine-owned DSP kernels for offline and finite device rendering.
    /// RustFFT planning and compiler-generated SIMD remain independent.
    pub dsp_dispatch: rustel_audio::DspDispatch,
    /// Filesystem and network resources that evaluated score text may select
    /// through `samples(...)`. Empty by default.
    pub score_sample_access: ScoreSampleAccess,
    /// Destinations score-chosen `oschost` values may reach. Loopback only
    /// until the host grants additional IPs.
    #[cfg(feature = "osc")]
    pub score_osc_access: ScoreOscAccess,
    /// The host's pointer, which the score's `mousex` and `mousey` read.
    /// Without one they read 0.
    pub pointer: Option<Pointer>,
}

impl Default for SessionConfig {
    fn default() -> Self {
        Self {
            cps: DEFAULT_CPS,
            max_polyphony: rustel_audio::MAX_POLYPHONY,
            horizon: DEFAULT_HORIZON,
            sample_rate: DEFAULT_SAMPLE_RATE,
            channels: DEFAULT_CHANNELS,
            dsp_dispatch: rustel_audio::DspDispatch::automatic(),
            score_sample_access: ScoreSampleAccess::denied(),
            #[cfg(feature = "osc")]
            score_osc_access: ScoreOscAccess::loopback_only(),
            pointer: None,
        }
    }
}

impl SessionConfig {
    /// Set the tempo in cycles per second.
    pub fn with_cps(mut self, cps: f64) -> Self {
        self.cps = cps;
        self
    }

    /// Set the default voice budget.
    pub fn with_max_polyphony(mut self, voices: usize) -> Self {
        self.max_polyphony = voices;
        self
    }

    /// Set the scheduling horizon in seconds.
    pub fn with_horizon(mut self, seconds: f64) -> Self {
        self.horizon = seconds;
        self
    }

    /// Set the rendering sample rate in Hz.
    pub fn with_sample_rate(mut self, hz: u32) -> Self {
        self.sample_rate = hz;
        self
    }

    /// Set the output channel count.
    pub fn with_channels(mut self, channels: u16) -> Self {
        self.channels = channels;
        self
    }

    /// Select the engine-owned DSP kernels.
    pub fn with_dsp_dispatch(mut self, dispatch: rustel_audio::DspDispatch) -> Self {
        self.dsp_dispatch = dispatch;
        self
    }

    /// Grant score text access to sample sources.
    pub fn with_score_sample_access(mut self, access: ScoreSampleAccess) -> Self {
        self.score_sample_access = access;
        self
    }

    /// Grant score-chosen OSC destinations.
    #[cfg(feature = "osc")]
    pub fn with_score_osc_access(mut self, access: ScoreOscAccess) -> Self {
        self.score_osc_access = access;
        self
    }

    /// Give the score a pointer the host writes.
    pub fn with_pointer(mut self, pointer: Pointer) -> Self {
        self.pointer = Some(pointer);
        self
    }
}

/// A Session failure; [`RuntimeError::kind`] names its category.
#[derive(Debug)]
#[non_exhaustive]
pub enum RuntimeError {
    Js(String),
    Mini(String),
    Io(std::io::Error),
    NoPattern,
    /// A signal asked us to stop before the work began.
    Interrupted(i32),
    /// The work was stopped part way through, at the caller's request.
    Cancelled,
    /// Score work panicked. The Session was rebuilt, or its recovery failed
    /// and it refuses further score work.
    Panic(String),
    /// A limit refused the work. The source is not at fault.
    ///
    /// A separate variant, not a `Message` recognised by its text. The CLI
    /// maps it to a distinct exit code, so a caller can retry with a shorter
    /// window instead of reporting a broken pattern.
    ResourceLimit(String),
    /// The native audio host or device rejected playback.
    Audio(String),
    /// This build omits the feature the work needs, such as `mp3-export`.
    Unsupported(String),
    Message(String),
}

impl From<rustel_core::QueryLimit> for RuntimeError {
    fn from(limit: rustel_core::QueryLimit) -> Self {
        match limit {
            // A cancelled query is not a resource refusal and must not exit 3:
            // explicit cancellation has the same outcome as a signalled stop
            // mid-flight.
            rustel_core::QueryLimit::Cancelled => Self::Cancelled,
            other => Self::ResourceLimit(other.to_string()),
        }
    }
}

/// The host's failures are TYPED, so a refusal keeps its kind and exit code
/// rather than being classified by matching message text.
impl From<rustel_jsruntime::QueryError> for RuntimeError {
    fn from(error: rustel_jsruntime::QueryError) -> Self {
        match error {
            rustel_jsruntime::QueryError::Limit(limit) => limit.into(),
            rustel_jsruntime::QueryError::Message(message)
            | rustel_jsruntime::QueryError::Policy(message) => Self::Js(message),
        }
    }
}

impl std::fmt::Display for RuntimeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Js(e) => write!(f, "javascript: {e}"),
            Self::Mini(e) => write!(f, "mini: {e}"),
            Self::Io(e) => write!(f, "io: {e}"),
            Self::NoPattern => write!(f, "no active pattern; evaluate or set_pattern first"),
            Self::ResourceLimit(e) => write!(f, "{e}"),
            Self::Audio(e) => write!(f, "audio: {e}"),
            Self::Unsupported(e) => write!(f, "{e}"),
            Self::Interrupted(signal) => {
                write!(f, "interrupted by signal {signal} before the work started")
            }
            Self::Cancelled => write!(f, "cancelled"),
            Self::Panic(message) => write!(f, "{message}"),
            Self::Message(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for RuntimeError {}

impl RuntimeError {
    /// A stable machine-readable kind.
    ///
    /// The CLI prints this alongside the message so a caller can branch on the
    /// FAILURE rather than on prose. Every one of these strings is part of the
    /// contract: change one and a consumer's error handling silently stops
    /// matching. The CLI suite in `crates/cli/tests/cli/main.rs` asserts the
    /// kind in the JSON error that the binary prints.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::ResourceLimit(_) => "resource-limit",
            Self::Audio(_) => "audio",
            Self::Unsupported(_) => "unsupported",
            Self::Interrupted(_) => "interrupted",
            Self::Cancelled => "cancelled",
            Self::Panic(_) => "panic",
            Self::Js(_) => "evaluation",
            Self::Mini(_) => "mini",
            Self::Io(_) => "io",
            Self::NoPattern => "no-pattern",
            Self::Message(_) => "invalid-argument",
        }
    }
}

impl From<std::io::Error> for RuntimeError {
    fn from(value: std::io::Error) -> Self {
        Self::Io(value)
    }
}

/// Preserve `play`'s partial-success Stop contract across both scheduler Stop
/// shapes. A signal already visible when an impure tick enters the bounded JS
/// preflight is returned as `RuntimeError::Cancelled`; one observed inside the
/// core query is `TickStatus::Refused(Cancelled)`. Both mean "stop after the
/// onsets already collected", not an evaluation failure.
fn play_tick_or_stopped(
    result: Result<rustel_scheduler::TickStatus, RuntimeError>,
) -> Result<Option<rustel_scheduler::TickStatus>, RuntimeError> {
    match result {
        Ok(rustel_scheduler::TickStatus::Stopped) | Err(RuntimeError::Cancelled) => Ok(None),
        Ok(status) => Ok(Some(status)),
        Err(error) => Err(error),
    }
}

/// The longest window `play`/`render` will schedule.
///
/// The library enforces this bound, not only the CLI argument parser: `play`
/// is public, and an embedder or a test that calls it directly does not pass
/// through the binary's validation.
const MAX_DURATION_SECS: f64 = 24.0 * 60.0 * 60.0;

/// The shortest horizon `SessionConfig` accepts: the scheduler's smallest
/// refill floor.
const MIN_HORIZON_SECS: f64 = rustel_scheduler::MIN_REFILL_FLOOR_SECS;

/// The most onsets a single `play`/`render` will accumulate.
///
/// One million is far past any musically meaningful window: a dense pattern
/// of 32 events per cycle reaches it after about 17 hours at the default
/// 0.5 cycles per second. The cap keeps the in-memory timeline and its JSON
/// bounded.
const MAX_ONSETS: usize = 1_000_000;

/// Whether a query failure is news: `true` the first time this
/// `(generation, message)` is seen, and again when either part changes.
///
/// `report_diagnostic`'s back-of-queue check suppresses only consecutive
/// repeats, and a live set interleaves other diagnostics between ticks. A
/// fatal throw and a contained callback failure (a throwing `filterValues`
/// predicate, which keeps the music playing) each keep their own record of
/// the last report.
fn query_failure_is_news(
    generation: u64,
    message: &str,
    reported: &mut Option<(u64, String)>,
) -> bool {
    if reported.as_ref() == Some(&(generation, message.to_string())) {
        return false;
    }
    *reported = Some((generation, message.to_string()));
    true
}

/// One-process headless runtime: one QuickJS heap, one scheduler transport.
pub struct Session {
    js: JsRuntime,
    panic_guard_active: bool,
    panic_poisoned: bool,
    panic_is_query: std::cell::Cell<bool>,
    recovery_epoch: u64,
    recovery_generation: u64,
    recovered_source: Option<Arc<str>>,
    recovery_prebakes: Arc<Vec<recovery::RecoveryPrebake>>,
    #[cfg(any(test, feature = "test-support"))]
    injected_panic: std::cell::Cell<Option<SessionPanicPoint>>,
    /// Serial intents from the last scheduling pass, awaiting the live loop.
    #[cfg(feature = "serial")]
    pending_serial: Vec<(f64, crate::serial_bridge::SerialOnset)>,
    /// MIDI intents collected during the last audio scheduling pass, waiting
    /// for the live loop to convert them to wall-clock sends.
    ///
    /// Collected here rather than re-queried because `schedule_through`
    /// ADVANCES the scheduler: asking a second time for the same window would
    /// not return the same onsets, it would return the next ones.
    #[cfg(feature = "midi")]
    pending_midi: Vec<crate::midi_bridge::MidiOnset>,
    /// The visuals recording of the score that just evaluated, before anything
    /// has committed it. A candidate the replacement probe rejects is dropped
    /// here rather than reaching a window.
    #[cfg(feature = "hydra")]
    staged_hydra: Option<Box<rustel_jsruntime::HydraCandidate>>,
    /// The visuals recording of the score that is now installed, waiting for
    /// the surface that owns the window to drain it.
    #[cfg(feature = "hydra")]
    pending_hydra: Option<Box<rustel_jsruntime::HydraCandidate>>,
    /// Per-score diagnostic gates for bounded MIDI collection. They prevent a
    /// dense pattern from turning one refused route into producer-thread log
    /// spam while still reporting the first actionable failure.
    #[cfg(feature = "midi")]
    midi_oversized_route_reported: bool,
    #[cfg(feature = "midi")]
    midi_output_limit_reported: bool,
    /// OSC intents collected during the last audio scheduling pass, waiting
    /// for the live loop to send them.
    ///
    /// Collected there rather than re-queried because `schedule_through`
    /// ADVANCES the scheduler: asking a second time for the same window would
    /// return the next onsets, not these.
    #[cfg(feature = "osc")]
    pending_osc: Vec<(f64, crate::osc_bridge::OscOnset)>,
    /// Refused OSC destinations already reported for the active score. A
    /// dense pattern must not allocate and print the same policy failure once
    /// per onset on the live producer thread.
    #[cfg(feature = "osc")]
    reported_osc_refusals: Vec<ReportedOscRefusal>,
    #[cfg(feature = "osc")]
    osc_output_limit_reported: bool,
    transport: Arc<Transport>,
    scheduler: Scheduler,
    config: SessionConfig,
    last_source: Option<Arc<str>>,
    #[cfg(feature = "vst")]
    insert_orbits: [u8; rustel_audio::MAX_ORBITS],
    /// Files the most recent `preload(...)` asked for, until a caller waits.
    preload_requested: usize,
    /// Whether a setup this session ran picks sample variants: it writes an
    /// `n` or maps values through code. Its helpers can set `n` for any
    /// later score, so a score's text does not show every `n` it plays. See
    /// [`Self::prebake_selects_variants`].
    prebake_selects_variants: bool,
    /// When set, a bounce ends where the music does rather than where the
    /// requested duration does.
    stop_when_silent: Option<rustel_audio::SilenceStop>,
    /// Optional master limiter for WAV/MP3 file exports.
    export_limiter: Option<rustel_audio::RenderLimiter>,
    /// The input-channel warnings already said for this score and this
    /// input: a silent voice on `in:1` is news once, not once a window.
    input_warnings_reported: std::collections::BTreeSet<String>,
    /// The "sample X is still loading" voice refusals already said for this
    /// score, by kind: the loading episodes that re-query a held replacement
    /// re-convert the same onsets every window, and without this the log
    /// fills with the same warning per retry. Every install clears it - a
    /// new score's loading refusal is news again. Awaited and skipped are
    /// kept apart, so notes skipped after a wait are still said.
    loading_refusals_reported: std::collections::BTreeSet<(&'static str, String)>,
    /// Seconds rendered past the scheduled length, for the tail: the
    /// scheduler stops at the length, the reverbs and delays ring out into
    /// the tail, and a silence stop ends it when they have.
    render_tail_secs: f64,
    /// Rate limit for the "fell behind, resumed at the present" report: a
    /// machine that is persistently too slow would otherwise log every tick.
    /// Only the live producer can fall behind, so only that build reads it.
    #[cfg_attr(not(feature = "device-audio"), allow(dead_code))]
    last_gap_log: Option<Instant>,
    /// Last source selected by an exact consumer completion, not publication.
    #[cfg(feature = "device-audio")]
    audible_source: Option<AudibleSource>,
    #[cfg(feature = "device-audio")]
    audio_confirmations: Option<confirmation::RollbackConfirmations>,
    /// The latency-compensated audio events a live device can hold, so a
    /// takeover does not sound one of their onsets twice.
    #[cfg(feature = "device-audio")]
    compensated_onsets: handover::CompensatedOnsets,
    /// How far a live host has handed the OSC bundles out, so a takeover
    /// does not send an onset that is out again.
    #[cfg(all(feature = "device-audio", feature = "osc"))]
    osc_handed_out: handover::HandedOut,
    /// The same for the serial writes.
    #[cfg(all(feature = "device-audio", feature = "serial"))]
    serial_handed_out: handover::HandedOut,
    /// The key presses that the live windows have placed, so the OSC and
    /// serial messages of a new press go out before the frontier too.
    #[cfg(all(feature = "device-audio", any(feature = "osc", feature = "serial")))]
    placed_presses: handover::PlacedPresses,
    /// Positive channel count from the host's currently open audio input.
    /// Unknown or disconnected inputs retain their documented silent behavior.
    audio_input_channels: Option<usize>,
    /// Device schedule lead for continuous live reloads (0 until a host
    /// reports its device's playback latency).
    schedule_lead: f64,
    /// Live-reload re-query margin (the device's consumption frontier).
    /// Falls back to `schedule_lead` when the host never measured one.
    continuity_margin: Option<f64>,
    /// The sample rate of the last live schedule, in Hz. The producer
    /// converts a takeover time to the takeover frame at this rate, and
    /// [`Self::takeover_frame_edge`] uses the same rate. `None` until a
    /// live producer has scheduled: no device applies a takeover frame.
    live_sample_rate: Option<u32>,
    /// The time (seconds, session clock) where the newest live reload's
    /// re-query cursor started; the producer converts it to the device
    /// takeover frame when it publishes the replacement generation.
    requery_takeover_time: Option<f64>,
    /// Whether the newest live reload asked the consumer to silence what
    /// sounds under it at its takeover frame. A from-zero reload (a rewind)
    /// does: the restarted loop must be heard alone, like a retriggered
    /// sample, rather than over the tail of what it replaced. An ordinary
    /// reload never does: its voices ring out by contract, and cutting them
    /// would click on every edit.
    requery_takeover_cut: TakeoverCut,
    /// Where the current rewind's cycle zero sits, on the session clock:
    /// `Some` only while a replacement whose takeover carries a cut is
    /// unpublished. The first window was queried from here, so a window
    /// held for a loading sample reopens from here, and the producer slides
    /// it (with the takeover and the cut) once the clock has passed it.
    ///
    /// It equals the takeover time for both cut shapes: the edit instant
    /// for an immediate rewind, the line for a quantised one. A slide moves
    /// both together. It is a separate field because the producer consumes
    /// the (takeover, cut) pair in the middle of a turn, before it knows
    /// whether the window holds, and the anchor must outlive that.
    requery_anchor_time: Option<f64>,
    /// The next live reload takes over at this time rather than one
    /// continuity margin ahead: a launch quantised to a cycle line.
    takeover_override: Option<f64>,
    /// The next live reload plays its score from the score's own cycle
    /// zero, instead of joining the cycle already running.
    next_from_zero: bool,
    last_path: EvaluateSource,
    /// The default sample banks. `None` until enabled;
    /// resolution then falls back to the bundled `bd` alone.
    samples: Option<std::sync::Arc<crate::samples::SampleLibrary>>,
    /// Ordinary CLI surfaces report asynchronous sample failures as JSON on
    /// stderr. Full-screen clients disable that direct writer and drain the
    /// same failures into their own diagnostic channel instead, otherwise a
    /// loader thread can scribble over the alternate screen.
    direct_diagnostic_logging: bool,
    /// Full-screen owners drain this instead of allowing writes behind their
    /// alternate screen. It is bounded independently of score complexity.
    pending_diagnostics: VecDeque<SessionDiagnostic>,
    bindings_ready: bool,
    voicings_ready: bool,
    /// Whether this Session has claimed its native module settings instead of
    /// continuing to inherit the ambient Rust compatibility state.
    core_settings_owned: bool,
    /// Whether an unclaimed Session already has settings paired with its
    /// active native graph. Claiming the JavaScript realm freezes this
    /// snapshot instead of inheriting unrelated ambient changes again.
    core_settings_initialized: bool,
    /// Scheduling iterations the last `play` performed.
    ///
    /// Private telemetry, not part of any report. Counting iterations
    /// distinguishes a stopped loop from one that continues spinning too
    /// cheaply for reliable timing assertions.
    last_play_iterations: u64,
    /// The last query throw already reported, as `(generation, message)`.
    ///
    /// A broken callback used to produce one warning per tick, forever; now a
    /// message is news once per generation, and again when it changes.
    reported_query_throw: Option<(u64, String)>,
    /// The last contained callback failure already reported, as
    /// `(generation, message)`. A throwing `filterValues` predicate keeps the
    /// music playing (fail open); this is what keeps the warning from
    /// repeating per press.
    reported_callback_failure: Option<(u64, String)>,
    /// Phase observations waiting for the live producer to publish one
    /// complete bounded turn. Evaluation may precede the scheduling call, so
    /// the Session retains only this single scalar record between boundaries.
    #[cfg(any(feature = "device-audio", test))]
    pending_producer_turn: ProducerTurnRecord,
}

/// Classify what an offline bounce returned.
///
/// A stopped transport is the artist pressing Ctrl-C, not a failure: it has to
/// arrive as `Cancelled` so the CLI exits quietly with the partial file rather
/// than printing an I/O error for something that worked as asked.
fn render_error(error: std::io::Error) -> RuntimeError {
    if error.to_string().contains(rustel_audio::RENDER_CANCELLED) {
        return RuntimeError::Cancelled;
    }
    match error.kind() {
        std::io::ErrorKind::InvalidInput => RuntimeError::Message(format!("scalar audio: {error}")),
        _ => RuntimeError::Io(error),
    }
}

/// Name a top-level audio control whose numeric expression produced NaN or
/// infinity before JSON's required null conversion can erase the distinction.
fn non_finite_audio_control(value: &rustel_core::Value) -> Option<String> {
    let materialized;
    let value = if matches!(value, rustel_core::Value::JsValue(_)) {
        materialized = rustel_core::materialize_js_value(value);
        &materialized
    } else {
        value
    };
    value.as_object()?.iter().find_map(|(name, value)| {
        // Mini-notation rests carry `n: NaN` as an internal no-note
        // sentinel. It is not a malformed user control and the scheduler
        // deliberately drops that voice.
        if name == "n" {
            return None;
        }
        value
            .as_f64()
            .filter(|number| !number.is_finite())
            .map(|_| format!("{name} must be a finite number"))
    })
}

impl Session {
    pub fn new() -> Result<Self, RuntimeError> {
        Self::with_config(SessionConfig::default())
    }

    /// # Validation
    ///
    /// Every numeric field is checked here, because each one feeds a loop
    /// bound, a divisor or an allocation. `play`'s loop runs until
    /// `start_secs + duration_secs + horizon`, so an infinite horizon would
    /// keep an ordinary two-second render from terminating. The bound is a
    /// sum, so validating `duration` alone is not enough.
    pub fn with_config(config: SessionConfig) -> Result<Self, RuntimeError> {
        // The first `gamepad()` any score calls starts the pad poller; a
        // session is where the runtime and the score meet, so this is
        // where the runtime says how.
        #[cfg(feature = "gamepad")]
        rustel_core::gamepad::set_starter(crate::gamepad::ensure_polling);
        let finite = |what: &str, value: f64| -> Result<f64, RuntimeError> {
            if !value.is_finite() || value <= 0.0 {
                return Err(RuntimeError::Message(format!(
                    "{what} must be a finite number greater than zero, got {value}"
                )));
            }
            Ok(value)
        };
        if !(1..=rustel_audio::MAX_CONFIGURABLE_POLYPHONY).contains(&config.max_polyphony) {
            return Err(RuntimeError::Message(format!(
                "max_polyphony must be from 1 to {}, got {}",
                rustel_audio::MAX_CONFIGURABLE_POLYPHONY,
                config.max_polyphony
            )));
        }
        finite("cps", config.cps)?;
        let horizon = finite("horizon", config.horizon)?;
        if horizon < MIN_HORIZON_SECS {
            return Err(RuntimeError::Message(format!(
                "horizon must be at least {MIN_HORIZON_SECS} seconds, got {horizon}"
            )));
        }
        if horizon > MAX_DURATION_SECS {
            return Err(RuntimeError::ResourceLimit(format!(
                "horizon must be at most {MAX_DURATION_SECS} seconds, got {horizon}"
            )));
        }
        if config.sample_rate == 0 {
            return Err(RuntimeError::Message(
                "sample_rate must be greater than zero".into(),
            ));
        }
        if config.channels == 0 {
            return Err(RuntimeError::Message(
                "channels must be greater than zero".into(),
            ));
        }
        let js = config
            .pointer
            .clone()
            .map_or_else(JsRuntime::new, JsRuntime::with_pointer)
            .map_err(|e| RuntimeError::Js(e.to_string()))?;
        let transport = Arc::new(Transport::default());
        let scheduler = Scheduler::new(transport.clone(), config.cps, config.horizon);
        Ok(Self {
            js,
            panic_guard_active: false,
            panic_poisoned: false,
            panic_is_query: std::cell::Cell::new(false),
            recovery_epoch: 0,
            recovery_generation: 0,
            recovered_source: None,
            recovery_prebakes: Arc::default(),
            #[cfg(any(test, feature = "test-support"))]
            injected_panic: std::cell::Cell::new(None),
            #[cfg(feature = "serial")]
            pending_serial: Vec::new(),
            #[cfg(feature = "midi")]
            pending_midi: Vec::new(),
            #[cfg(feature = "hydra")]
            staged_hydra: None,
            #[cfg(feature = "hydra")]
            pending_hydra: None,
            #[cfg(feature = "midi")]
            midi_oversized_route_reported: false,
            #[cfg(feature = "midi")]
            midi_output_limit_reported: false,
            #[cfg(feature = "osc")]
            pending_osc: Vec::new(),
            #[cfg(feature = "osc")]
            reported_osc_refusals: Vec::new(),
            #[cfg(feature = "osc")]
            osc_output_limit_reported: false,
            transport,
            scheduler,
            config,
            last_source: None,
            #[cfg(feature = "vst")]
            insert_orbits: crate::vst::INSERT_ORBITS,
            preload_requested: 0,
            prebake_selects_variants: false,
            stop_when_silent: None,
            export_limiter: None,
            input_warnings_reported: std::collections::BTreeSet::new(),
            loading_refusals_reported: std::collections::BTreeSet::new(),
            render_tail_secs: 0.0,
            last_gap_log: None,
            #[cfg(feature = "device-audio")]
            audible_source: None,
            #[cfg(feature = "device-audio")]
            audio_confirmations: None,
            #[cfg(feature = "device-audio")]
            compensated_onsets: handover::CompensatedOnsets::default(),
            #[cfg(all(feature = "device-audio", feature = "osc"))]
            osc_handed_out: handover::HandedOut::default(),
            #[cfg(all(feature = "device-audio", feature = "serial"))]
            serial_handed_out: handover::HandedOut::default(),
            #[cfg(all(feature = "device-audio", any(feature = "osc", feature = "serial")))]
            placed_presses: handover::PlacedPresses::default(),
            audio_input_channels: None,
            schedule_lead: 0.0,
            continuity_margin: None,
            live_sample_rate: None,
            requery_takeover_time: None,
            requery_takeover_cut: TakeoverCut::None,
            requery_anchor_time: None,
            takeover_override: None,
            next_from_zero: false,
            last_path: EvaluateSource::Pattern,
            last_play_iterations: 0,
            samples: None,
            direct_diagnostic_logging: rustel_voice::default_direct_diagnostic_logging(),
            pending_diagnostics: VecDeque::new(),
            bindings_ready: false,
            voicings_ready: false,
            core_settings_owned: false,
            core_settings_initialized: false,
            reported_query_throw: None,
            reported_callback_failure: None,
            #[cfg(any(feature = "device-audio", test))]
            pending_producer_turn: ProducerTurnRecord::default(),
        })
    }

    #[cfg(any(feature = "device-audio", test))]
    pub(crate) fn take_pending_producer_turn(&mut self) -> ProducerTurnRecord {
        self.pending_producer_turn.scheduler_queue_depth =
            u32::try_from(self.scheduler.queued()).unwrap_or(u32::MAX);
        self.pending_producer_turn.scheduler_queue_capacity =
            u32::try_from(rustel_scheduler::MAX_QUEUED_EVENTS).unwrap_or(u32::MAX);
        self.pending_producer_turn.scheduler_trace_depth =
            u32::try_from(self.scheduler.trace_queued()).unwrap_or(u32::MAX);
        self.pending_producer_turn.scheduler_trace_capacity =
            u32::try_from(rustel_scheduler::MAX_QUEUED_TRACE_EVENTS).unwrap_or(u32::MAX);
        self.pending_producer_turn.scheduler_trace_drops =
            self.scheduler.trace_events_dropped_total();
        std::mem::take(&mut self.pending_producer_turn)
    }

    #[cfg(any(feature = "device-audio", test))]
    pub(crate) fn record_producer_phase(&mut self, phase: ProducerPhase, elapsed: Duration) {
        self.pending_producer_turn.add_phase(phase, elapsed);
    }

    /// Add producer-side asset installation performed by a live host before
    /// its next scheduling step.
    #[cfg(feature = "device-audio")]
    #[doc(hidden)]
    pub fn record_live_asset_preparation(&mut self, elapsed: Duration) {
        self.record_producer_phase(ProducerPhase::AssetPreparation, elapsed);
    }

    #[cfg(any(feature = "device-audio", test))]
    pub(crate) fn record_producer_ring_push(
        &mut self,
        elapsed: Duration,
        pushed: usize,
        saturated: bool,
    ) {
        self.record_producer_phase(ProducerPhase::RingPush, elapsed);
        self.pending_producer_turn.ring_pushes = self
            .pending_producer_turn
            .ring_pushes
            .saturating_add(u64::try_from(pushed).unwrap_or(u64::MAX));
        self.pending_producer_turn.queue_saturated |= saturated;
    }

    #[cfg(any(feature = "device-audio", test))]
    pub(crate) fn record_producer_cover_age(&mut self, elapsed: Duration) {
        self.pending_producer_turn.cover_age_nanos = self
            .pending_producer_turn
            .cover_age_nanos
            .saturating_add(crate::producer::duration_nanos(elapsed));
    }

    #[cfg(any(feature = "device-audio", test))]
    pub(crate) fn record_atomic_producer_refusal(&mut self) {
        self.pending_producer_turn.atomic_refusal_count = self
            .pending_producer_turn
            .atomic_refusal_count
            .saturating_add(1);
    }

    #[cfg(any(feature = "device-audio", test))]
    fn merge_producer_turn(&mut self, record: ProducerTurnRecord) {
        for phase in ProducerPhase::ALL {
            let index = phase as usize;
            self.pending_producer_turn.phases_nanos[index] =
                self.pending_producer_turn.phases_nanos[index]
                    .saturating_add(record.phases_nanos[index]);
        }
        self.pending_producer_turn.outcome = record.outcome;
        self.pending_producer_turn.cover_start_nanos = record.cover_start_nanos;
        self.pending_producer_turn.cover_end_nanos = record.cover_end_nanos;
        self.pending_producer_turn.cover_gained_nanos = record.cover_gained_nanos;
        self.pending_producer_turn.minimum_grid_nanos = record.minimum_grid_nanos;
        self.pending_producer_turn.continuation_reserve_nanos = record.continuation_reserve_nanos;
        self.pending_producer_turn.budget_granted_nanos = record.budget_granted_nanos;
        self.pending_producer_turn.query_span_millicycles = record.query_span_millicycles;
        self.pending_producer_turn.query_span_nanos = record.query_span_nanos;
        self.pending_producer_turn.haps = record.haps;
        self.pending_producer_turn.scheduler_events = record.scheduler_events;
        self.pending_producer_turn.converted_audio_events = record.converted_audio_events;
        self.pending_producer_turn.js_callback_calls = record.js_callback_calls;
        if record.js_callback_kind_sample.is_some() {
            self.pending_producer_turn.js_callback_kind_sample = record.js_callback_kind_sample;
        }
        self.pending_producer_turn.refused_voices = record.refused_voices;
        self.pending_producer_turn.scheduler_queue_depth = record.scheduler_queue_depth;
        self.pending_producer_turn.scheduler_queue_capacity = record.scheduler_queue_capacity;
        self.pending_producer_turn.scheduler_trace_depth = record.scheduler_trace_depth;
        self.pending_producer_turn.scheduler_trace_capacity = record.scheduler_trace_capacity;
        self.pending_producer_turn.scheduler_trace_drops = record.scheduler_trace_drops;
        self.pending_producer_turn.queue_saturated |= record.queue_saturated;
        self.pending_producer_turn.rollback_count = self
            .pending_producer_turn
            .rollback_count
            .saturating_add(record.rollback_count);
        self.pending_producer_turn.gap_resync_count = self
            .pending_producer_turn
            .gap_resync_count
            .saturating_add(record.gap_resync_count);
        self.pending_producer_turn.cover_age_nanos = self
            .pending_producer_turn
            .cover_age_nanos
            .saturating_add(record.cover_age_nanos);
        self.pending_producer_turn.atomic_refusal_count = self
            .pending_producer_turn
            .atomic_refusal_count
            .saturating_add(record.atomic_refusal_count);
        self.pending_producer_turn.committed_refusal_count = self
            .pending_producer_turn
            .committed_refusal_count
            .saturating_add(record.committed_refusal_count);
        // The reasons those refusals were not pressure ride with them.
        // Dropping them here read every score-caused and library-waiting
        // refusal back as the engine falling behind, and the header went
        // red against numbers that were all healthy.
        self.pending_producer_turn.rejected_score_count = self
            .pending_producer_turn
            .rejected_score_count
            .saturating_add(record.rejected_score_count);
        self.pending_producer_turn.loading_refusal_count = self
            .pending_producer_turn
            .loading_refusal_count
            .saturating_add(record.loading_refusal_count);
    }

    /// What the score that just evaluated asked a visuals window to draw,
    /// waiting for something to commit. Cleared into `pending_hydra` there.
    #[cfg(feature = "hydra")]
    pub(crate) fn promote_staged_hydra(&mut self) {
        if let Some(candidate) = self.staged_hydra.take() {
            self.pending_hydra = Some(candidate);
        }
    }

    /// The visuals program a committed score installed, if there is a new one.
    ///
    /// Drained by whichever product surface owns the window, beside
    /// [`Session::take_pending_midi`] and [`Session::take_pending_osc`]. An
    /// update whose program is empty is the instruction to close.
    #[cfg(feature = "hydra")]
    pub fn take_pending_hydra(&mut self) -> Option<Box<rustel_jsruntime::HydraCandidate>> {
        self.pending_hydra.take()
    }

    pub fn config(&self) -> &SessionConfig {
        &self.config
    }

    /// Effective voice budget. Like other module settings, an explicit score
    /// value persists until another accepted score changes it.
    pub fn max_polyphony(&self) -> usize {
        self.max_polyphony_override()
            .unwrap_or(self.config.max_polyphony)
    }

    /// Explicit accepted score setting, independent of the host default.
    pub fn max_polyphony_override(&self) -> Option<usize> {
        self.js
            .with_runtime_settings(rustel_core::settings::max_polyphony)
    }

    /// Change the host default without overwriting an explicit score override.
    pub fn set_default_max_polyphony(&mut self, voices: usize) {
        self.config.max_polyphony = voices.clamp(1, rustel_audio::MAX_CONFIGURABLE_POLYPHONY);
    }

    pub fn last_evaluate_source(&self) -> EvaluateSource {
        self.last_path.clone()
    }

    /// Exact source text that produced the active generation, when it came
    /// from a score evaluation rather than a directly installed Rust pattern.
    pub fn active_source(&self) -> Option<&str> {
        self.last_source.as_deref()
    }

    /// Plugin requirements of a candidate source, on the physical insert buses
    /// the source uses after its accept. The routes of the playing score stay.
    #[cfg(feature = "vst")]
    pub fn plugin_calls_for_source(&self, source: &str) -> Vec<crate::lint::NamedPlugin> {
        let orbits = crate::vst::plan_orbits(self.active_source(), source, self.insert_orbits);
        let mut plugins = crate::lint::live_plugins(source);
        for plugin in &mut plugins {
            plugin.orbit = plugin
                .orbit
                .and_then(|orbit| orbits.get(orbit))
                .map(|orbit| *orbit as usize);
        }
        plugins
    }

    pub fn generation(&self) -> u64 {
        self.transport.generation()
    }

    /// Update one slider registered by the active score. Callers are
    /// responsible for revision/id/range policy; this boundary only performs
    /// the finite host-cell mutation and reports an unknown id as `false`.
    pub fn set_slider_value(&self, id: &str, value: f64) -> Result<bool, RuntimeError> {
        self.js
            .set_slider_value(id, value)
            .map_err(RuntimeError::Js)
    }

    /// Current value of one active query-time slider cell.
    pub fn slider_value(&self, id: &str) -> Result<Option<f64>, RuntimeError> {
        self.js.slider_value(id).map_err(RuntimeError::Js)
    }

    /// Opaque direct-control token for the currently committed slider cell.
    pub fn slider_binding(&self, id: &str) -> Option<u64> {
        self.js.slider_binding(id)
    }

    /// Invalidate the safely replaceable future after a query-time control
    /// mutation, preserving the active graph and musical phase.
    ///
    /// The returned generations must be handed to the live producer before
    /// its next step. That producer prefills the new generation and publishes
    /// it to the device together with [`Self::take_requery_takeover_time`], so
    /// already-audible old events survive while stale prefetched events do not.
    pub fn requery_active_at(&mut self, now: f64) -> Result<Option<(u64, u64)>, RuntimeError> {
        if !now.is_finite() || now < 0.0 {
            return Err(RuntimeError::Message(format!(
                "control requery time must be finite and non-negative, got {now}"
            )));
        }
        if self.transport.is_stopped() {
            return Ok(None);
        }
        let margin = self.continuity_margin.unwrap_or(self.schedule_lead);
        let takeover_time = now + margin.max(0.0);
        let generation_before = self.generation();
        // A rewind that is installed but not yet published owns the
        // takeover: its line (or its edit instant) and its cut. A slider,
        // key or clock-steer requery in that window re-queries the same
        // graph, which the from-zero install already clamps to cycle zero,
        // so it publishes at the rewind's takeover. With only the time
        // replaced, the `AtTakeover` cut would apply at `now + margin`,
        // before the rewind's line.
        let pending_rewind = (self.requery_takeover_cut != TakeoverCut::None)
            .then_some(self.requery_takeover_time)
            .flatten();
        let takeover_time = pending_rewind.unwrap_or(takeover_time);
        // A live device drops the outgoing onsets from the takeover frame,
        // so the cursor starts on that frame's edge. A pending rewind keeps
        // its own takeover: its cut ends the outgoing score, and its first
        // cycle begins the new one.
        let takeover_edge = match pending_rewind {
            None => self.takeover_frame_edge(takeover_time),
            Some(_) => None,
        };
        let requeried = match takeover_edge {
            Some(edge) => self.scheduler.requery_active_from(now, edge),
            None => self.scheduler.requery_active(now, takeover_time),
        };
        let Some(generation_after) = requeried else {
            return Ok(None);
        };
        // The device swap retires a key press that the outgoing generation
        // pinned at or after the takeover frame, so that pin names a schedule
        // that never plays. Forget it so the re-query's own pass re-claims
        // the press at the takeover frame, the earliest moment the device
        // can sound it. Otherwise the player waits for a frontier that a
        // heavy sibling pattern can push out of reach. A pin before the
        // takeover stays: those frames are already in the device's lookahead
        // and keep sounding across the takeover, so re-claiming one would
        // sound the press twice.
        self.js.midi_input_bus().forget_key_placements_from(
            self.scheduler
                .cycle_at_time(takeover_edge.unwrap_or(takeover_time)),
        );
        // A slider/control re-query keeps the same graph and therefore the
        // same MIDI-input handles, but the listener's audible/session union
        // follows scheduler generations. Republish the retained input set at
        // the exact generation bump so it cannot be retired as stale.
        self.js
            .midi_input_bus()
            .republish_current_generation(generation_after);
        self.requery_takeover_time = Some(takeover_time);
        if pending_rewind.is_none() {
            // An ordinary control requery rings out: no rewind shape may
            // ride along with its time.
            self.requery_takeover_cut = TakeoverCut::None;
            self.requery_anchor_time = None;
        }
        Ok(Some((generation_before, generation_after)))
    }

    /// Refill a live device after its output stream discarded the complete
    /// event ring. Unlike an ordinary control re-query, no old audio horizon
    /// remains to cover a continuity takeover: resume at the replacement
    /// stream's safe scheduling lead and publish that exact cursor with the
    /// recovery generation.
    #[cfg(feature = "device-audio")]
    pub fn requery_after_output_recycle_at(
        &mut self,
        now: f64,
    ) -> Result<Option<(u64, u64)>, RuntimeError> {
        if !now.is_finite() || now < 0.0 {
            return Err(RuntimeError::Message(format!(
                "output recycle requery time must be finite and non-negative, got {now}"
            )));
        }
        if self.transport.is_stopped() {
            return Ok(None);
        }
        let cursor_time = now + self.schedule_lead.max(0.0);
        let generation_before = self.generation();
        let Some(generation_after) = self.scheduler.requery_active(now, cursor_time) else {
            return Ok(None);
        };
        // Same contract as the control re-query: a pin at or after the cursor
        // names audio the discarded ring never carried, so the replacement
        // pass must re-claim the press at the cursor rather than leave it
        // parked ahead.
        self.js
            .midi_input_bus()
            .forget_key_placements_from(self.scheduler.cycle_at_time(cursor_time));
        self.js
            .midi_input_bus()
            .republish_current_generation(generation_after);
        self.requery_takeover_time = Some(cursor_time);
        // The recycled stream starts empty: there is no outgoing rendition
        // to cut, and a rewind shape left standing would be published on
        // this recovery generation's cursor instead of its own line.
        self.requery_takeover_cut = TakeoverCut::None;
        self.requery_anchor_time = None;
        // Nor does it hold a latency-compensated event from before the
        // cursor: the recovery generation sounds every onset it queries.
        self.compensated_onsets.clear();
        // The OSC and serial messages that are out were timed against the
        // stream that failed. The recovery generation sends its own, in
        // time with the audio it plays.
        self.forget_handed_out();
        // These intents came from the discarded audio horizon. Letting them
        // cross after the same-graph generation bump would produce external
        // notes or packets with no corresponding recovered audio onset.
        #[cfg(feature = "midi")]
        self.pending_midi.clear();
        #[cfg(feature = "osc")]
        self.pending_osc.clear();
        #[cfg(feature = "serial")]
        self.pending_serial.clear();
        Ok(Some((generation_before, generation_after)))
    }

    fn ensure_bindings(&mut self) -> Result<(), RuntimeError> {
        if self.bindings_ready {
            return Ok(());
        }
        self.js
            .install_semantic_bindings()
            .map_err(RuntimeError::Js)?;
        // `s` is not an alias of `m`. The mini pattern is the argument to the
        // `s` control, so `s("bd sd")` produces `{s: "bd"}` control objects,
        // not bare `"bd"` values.
        self.bindings_ready = true;
        Ok(())
    }

    /// Install an explicit Rust `Pattern` as the active graph (no JS).
    pub fn set_pattern(&mut self, pattern: Pattern) -> Result<(), RuntimeError> {
        self.with_panic_recovery(0.0, |session| session.set_pattern_at(pattern, 0.0, true))
    }

    /// Select the random-number behavior for this Session only.
    pub fn set_rng_mode(&mut self, mode: rustel_core::rng::RngMode) {
        self.with_runtime_settings(|| rustel_core::rng::use_rng(mode));
    }

    /// Select the default join alignment for graph construction and deferred
    /// composers in this Session.
    pub fn set_default_join(&mut self, alignment: rustel_core::compose::Alignment) {
        self.with_runtime_settings(|| rustel_core::compose::set_default_alignment(alignment));
    }

    /// Select or reset the default voicing dictionary for this Session.
    pub fn set_default_voicings(&mut self, name: Option<&str>) {
        self.with_runtime_settings(|| match name {
            Some(name) => rustel_core::voicings::set_default_voicings(name),
            None => rustel_core::voicings::reset_default_voicings(),
        });
    }

    /// Run `f` with this Session's module settings selected.
    ///
    /// Calls to the native RNG, join, and voicing setters affect this Session
    /// and no other. A returned graph acquires Session ownership when it is
    /// installed with [`Session::set_pattern`].
    pub fn with_runtime_settings<R>(&mut self, f: impl FnOnce() -> R) -> R {
        self.claim_core_settings();
        self.js.with_runtime_settings(f)
    }

    fn claim_core_settings(&mut self) {
        if !self.core_settings_owned {
            if !self.core_settings_initialized {
                self.js.inherit_current_runtime_settings();
                self.core_settings_initialized = true;
            }
            self.core_settings_owned = true;
        }
    }

    fn set_pattern_at(
        &mut self,
        pattern: Pattern,
        now: f64,
        restart_transport: bool,
    ) -> Result<(), RuntimeError> {
        let settings =
            (!self.core_settings_owned).then(|| self.js.snapshot_ambient_runtime_settings());
        // A direct Pattern names no takeover: the device refuses the
        // outgoing generation's unplayed events from its flip, which the
        // edit instant stands for.
        let key_takeover_cycle = self.scheduler.cycle_at_time(now);
        let result = self.install_pattern_at(
            pattern,
            now,
            restart_transport,
            None,
            None,
            key_takeover_cycle,
        );
        if result.is_ok()
            && let Some(settings) = settings
        {
            self.js.adopt_runtime_settings(&settings);
            self.core_settings_initialized = true;
        }
        result
    }

    fn install_pattern_at(
        &mut self,
        pattern: Pattern,
        now: f64,
        restart_transport: bool,
        overlap_cycle: Option<(f64, f64)>,
        takeover_edge: Option<f64>,
        key_takeover_cycle: f64,
    ) -> Result<(), RuntimeError> {
        // A new score: its missing channels are its own news.
        self.input_warnings_reported.clear();
        self.loading_refusals_reported.clear();
        self.ensure_bindings()?;
        let builder = self.js.builder();
        self.js
            .set_active(&builder, pattern.clone())
            .map_err(RuntimeError::Js)?;
        let expected_generation = self.generation().checked_add(1).ok_or_else(|| {
            RuntimeError::ResourceLimit("scheduler generation counter exhausted".into())
        })?;
        // Native/mini graphs cannot name `midin()` ports. Publish their empty
        // set only after the fallible JS wrapper installation succeeds and
        // immediately before the infallible scheduler generation commit.
        self.js
            .commit_empty_midi_input_generation(expected_generation)
            .map_err(RuntimeError::Js)?;
        let generation = match overlap_cycle {
            Some(overlap) => {
                self.replace_pattern_continued(pattern, now, None, overlap, takeover_edge)
            }
            None => self.scheduler.set_pattern(pattern, now),
        };
        debug_assert_eq!(generation, expected_generation);
        if restart_transport {
            self.js.midi_input_bus().clear_keys();
            self.transport.start();
        } else if !self.transport.is_stopped() {
            // Same boundary as the JavaScript path: a pattern installed over
            // a running transport must not inherit the outgoing graph's
            // pinned key backlog, and must still hear the presses it never
            // placed or whose pins the takeover drops. See the note in
            // `commit_evaluated_score`.
            self.js
                .midi_input_bus()
                .retire_key_backlog(key_takeover_cycle);
        }
        // A direct Rust Pattern has no textual score the live watchdog can
        // reconstruct. Keep the previous audible snapshot until this
        // candidate reaches the device, but do not let a later successful
        // cutover mislabel the Pattern as the last JavaScript/mini source.
        self.last_source = None;
        #[cfg(feature = "vst")]
        {
            self.insert_orbits = crate::vst::INSERT_ORBITS;
        }
        self.last_path = EvaluateSource::Pattern;
        self.js.snapshot_active_as_last_good();
        Ok(())
    }

    /// Replace the pattern as a continued replacement over `overlap`: the
    /// cycle at the edit and the seconds from it to the takeover (see
    /// `commit_evaluated_score`).
    ///
    /// A live device drops the outgoing copies from a takeover frame. With
    /// a `takeover_edge` ([`Self::takeover_frame_edge`]) the pre-marked
    /// window ends on that frame's edge. Without one no device applies a
    /// takeover frame, and the window ends at the takeover time.
    fn replace_pattern_continued(
        &mut self,
        pattern: Pattern,
        now: f64,
        cps: Option<f64>,
        (cycle_now, takeover): (f64, f64),
        takeover_edge: Option<f64>,
    ) -> u64 {
        // The window ends where the outgoing rate reaches the takeover: the
        // device holds the outgoing copies on the times that rate gave
        // them. A window on the new rate sounds onsets twice for a slower
        // score and skips onsets for a faster score.
        //
        // Exception: a score more than twice as slow. With the window on
        // the outgoing rate, its replacement has nothing to play until
        // `margin * old / new` seconds after the edit: 2.5 s for a 0.25 s
        // margin at a tenth of the rate. So the window ends where the new
        // rate is one margin past the takeover. The replacement resumes
        // there and plays the later outgoing copies again.
        let outgoing = self.scheduler.cps();
        let rate = outgoing.min(2.0 * cps.unwrap_or(outgoing));
        let takeover_cycle = cycle_now + takeover * rate;
        match takeover_edge {
            Some(edge) => self.scheduler.replace_pattern_continued_until(
                pattern,
                now,
                cps,
                cycle_now,
                edge,
                // Only the exception ends the window before the edge.
                (rate < outgoing).then_some(takeover_cycle),
            ),
            None => self.scheduler.replace_pattern_continued(
                pattern,
                now,
                cps,
                cycle_now,
                takeover_cycle,
            ),
        }
    }

    /// The first instant whose onset lands on the takeover frame `F` of a
    /// takeover at `takeover_time`. An onset takes the first frame at or
    /// after its time, so the edge is just after frame `F - 1`:
    ///
    /// ```text
    /// frame        F - 1                F
    /// time     ------|------------------|------>
    ///                 ^ edge            ^ takeover_time, rounded
    /// outgoing   kept | dropped
    /// ```
    ///
    /// The device drops the outgoing onsets from the edge, so the incoming
    /// generation must play them. A split at the takeover time gives the
    /// onsets between the two marks to neither generation. So the window of
    /// a continued replacement, the cursor of a requery and the key pins
    /// all split the two generations on this edge.
    ///
    /// `None` until a live producer has scheduled, and for a takeover time
    /// that is not finite.
    fn takeover_frame_edge(&self, takeover_time: f64) -> Option<f64> {
        let sample_rate = self.live_sample_rate?;
        takeover_time.is_finite().then(|| {
            crate::render::onset_frame_edge(
                crate::render::takeover_frame_at(takeover_time, sample_rate),
                sample_rate,
            )
        })
    }

    /// Start the manifest-verified default sample library.
    ///
    /// Construction does no network I/O. Pinned manifests are queued on the
    /// library's bounded loader and failures surface through the normal
    /// loud-once sample failure channel; this only returns an error when the
    /// compiled manifest metadata or loader construction itself is invalid.
    pub fn enable_default_samples(&mut self) -> Result<(), RuntimeError> {
        if self.samples.is_none() {
            let library = crate::samples::SampleLibrary::load_default_async_for_session()
                .map_err(RuntimeError::Message)?;
            library.set_direct_diagnostic_logging(self.direct_diagnostic_logging);
            library.set_render_rate(self.config.sample_rate);
            self.samples = Some(std::sync::Arc::new(library));
        }
        Ok(())
    }

    pub fn sample_library(&self) -> Option<&std::sync::Arc<crate::samples::SampleLibrary>> {
        self.samples.as_ref()
    }

    /// The sample library's settled epoch ([`crate::samples::SampleLibrary::settled_epoch`]):
    /// unchanged since a window was refused as still loading means that
    /// window would be refused again. Zero without a library, where nothing
    /// loads.
    #[cfg(feature = "device-audio")]
    pub(crate) fn sample_settled_epoch(&self) -> u64 {
        self.samples
            .as_ref()
            .map_or(0, |library| library.settled_epoch())
    }

    /// Tell live conversion which channels an open input can provide. Without
    /// an open input, `in` remains a silent source; the native channel ceiling
    /// is still enforced by voice conversion regardless of this observation.
    pub fn set_audio_input_channels(&mut self, channels: Option<usize>) {
        let channels = channels
            .filter(|channels| *channels > 0)
            .map(|channels| channels.min(rustel_audio::input::MAX_INPUT_CHANNELS));
        if channels != self.audio_input_channels {
            // Another input: what it lacks is news again.
            self.input_warnings_reported.clear();
        }
        self.audio_input_channels = channels;
    }

    /// A voice on an input channel the open input does not have plays
    /// silence, because the ring reads nothing past its channels, as
    /// `s("in")` does with no input open. The warning is reported once. It
    /// is not a refusal: a refusal would reject the whole replacement, and
    /// the rollback would fail too when the last audible score reads the
    /// same channel, for example after a microphone is swapped for a mono
    /// one under a running score.
    fn input_channel_warning(&self, event: &rustel_audio::AudioEvent) -> Option<String> {
        let channels = self.audio_input_channels?;
        let Some(rustel_audio::SynthSource::Input { channel }) = event.synth else {
            return None;
        };
        (usize::from(channel) >= channels).then(|| {
            format!(
                "in:{channel} is unavailable: the open input provides channels 0..{} - it plays silence",
                channels - 1
            )
        })
    }

    #[cfg(all(any(test, feature = "test-support"), feature = "device-audio"))]
    #[doc(hidden)]
    pub fn set_sample_library_for_test(&mut self, library: Arc<crate::samples::SampleLibrary>) {
        self.samples = Some(library);
    }

    /// Fetch the map of a `samples("…")` a score names, ahead of the score
    /// being evaluated, under the same grant an evaluation would use. See
    /// [`crate::samples::SampleLibrary::look_up_samples_source`].
    pub fn look_up_samples_source(&mut self, spec: &str) -> Result<(), RuntimeError> {
        if self.samples.is_none() {
            self.enable_default_samples()?;
        }
        let Some(library) = self.samples.clone() else {
            return Ok(());
        };
        library
            .look_up_samples_source(spec, &self.config.score_sample_access)
            .map_err(RuntimeError::Message)
    }

    /// Select whether recoverable live diagnostics are written to stderr.
    ///
    /// A new Session takes this from
    /// [`rustel_voice::default_direct_diagnostic_logging`], which is off
    /// unless the host turned it on, as the `rustel` command line does. With
    /// it off, diagnostics are collected for [`Self::take_diagnostics`],
    /// which a host drains from its presentation loop; they include `.log()`
    /// lines and the voice resolver's notices ([`VOICE_NOTICE_DIAGNOSTIC`]).
    /// It also decides whether [`Self::render`] and [`Self::render_session`]
    /// print progress.
    pub fn set_direct_diagnostic_logging(&mut self, enabled: bool) {
        self.direct_diagnostic_logging = enabled;
        if let Some(library) = &self.samples {
            library.set_direct_diagnostic_logging(enabled);
        }
    }

    /// Drain presentation-neutral notices accumulated with direct logging
    /// disabled.
    pub fn take_diagnostics(&mut self) -> Vec<SessionDiagnostic> {
        self.pending_diagnostics.drain(..).collect()
    }

    /// Drain new asynchronous sample-loader failures without printing them.
    pub fn take_sample_failures(&self) -> Vec<crate::samples::SampleFailure> {
        self.samples
            .as_ref()
            .map_or_else(Vec::new, |library| library.take_failures_with_maps())
    }

    /// Start loading every sound that a saved score's text names, without
    /// waiting.
    ///
    /// A watch-save is on disk one debounce before it installs. Until then
    /// the pattern in the Session is the outgoing one, so a query would warm
    /// the score being replaced. Reading the saved text starts the decode
    /// early. Otherwise a sound that the save introduces (`hh:0`) begins
    /// decoding at the first onset that asks for it, and that onset is
    /// skipped as "still loading".
    ///
    /// The call is asynchronous and runs no query, so it does not spend the
    /// producer's turn. A miss is not an error: the first hits are skipped
    /// and the sound enters later.
    pub fn warm_sounds_in_source(&mut self, source: &str) -> usize {
        let names = crate::sounds::to_warm(source, self.prebake_selects_variants);
        if names.is_empty() {
            return 0;
        }
        // Whatever the library already is: enabling one here would put a
        // default manifest fetch on the producer's thread, and a session
        // with no library has nothing to warm anyway.
        let Some(library) = self.samples.clone() else {
            return 0;
        };
        let files = match library.warm_score_sounds_async(&names, &self.config.score_sample_access)
        {
            Ok(crate::samples::PrefetchStatus::Requested(files)) => files,
            Ok(crate::samples::PrefetchStatus::Deferred)
            | Ok(crate::samples::PrefetchStatus::Unknown) => 0,
            // A save that names something unfetchable is not worth a word:
            // the install behind it reports whatever is actually wrong, and
            // this ran before anyone asked for a sound.
            Err(_) => 0,
        };
        self.preload_requested = self.preload_requested.saturating_add(files);
        files
    }

    /// Start loading these sound names, whatever the score is doing with them.
    ///
    /// Returns how many files could be requested immediately. If a pending
    /// manifest may redefine a name, its request is queued behind that
    /// manifest and is not included in the immediate count.
    pub fn prefetch_sounds(&mut self, names: &[String]) -> usize {
        match self.prefetch_sounds_checked(names) {
            Ok(files) => files,
            Err(error) => {
                self.report_diagnostic(
                    "samples-failed",
                    error.to_string(),
                    serde_json::json!({ "samples_failed": { "message": error.to_string() } }),
                );
                0
            }
        }
    }

    /// Checked form of [`Self::prefetch_sounds`] for clients that own their
    /// diagnostic surface and must not write behind a full-screen UI.
    pub fn prefetch_sounds_checked(&mut self, names: &[String]) -> Result<usize, RuntimeError> {
        if names.is_empty() {
            return Ok(0);
        }
        if self.samples.is_none() {
            self.enable_default_samples()?;
        }
        let Some(library) = self.samples.clone() else {
            return Ok(0);
        };
        let files = match library.register_score_batch_async_for_session(
            Vec::new(),
            names,
            &self.config.score_sample_access,
        ) {
            Ok(crate::samples::PrefetchStatus::Requested(files)) => files,
            Ok(crate::samples::PrefetchStatus::Deferred)
            | Ok(crate::samples::PrefetchStatus::Unknown) => 0,
            Err(error) => return Err(RuntimeError::Message(error)),
        };
        self.preload_requested = self.preload_requested.saturating_add(files);
        Ok(files)
    }

    /// Prepare a live score without downloading unused GM variants.
    /// Actual queried events go first; source-only guesses stay behind them
    /// and behind any manifest that may redefine their sound names.
    ///
    /// Answers the sounds the queried window resolved, as
    /// [`Self::kick_sample_loads_within`] does, and the warm's refusal, if
    /// any, beside them.
    pub fn warm_live_samples_checked(
        &mut self,
        names: &[String],
        from_cycle: f64,
        query_budget: Duration,
    ) -> (
        Vec<(String, crate::sounds::Variants)>,
        Result<(), RuntimeError>,
    ) {
        if let Err(error) = self.enable_default_samples() {
            return (Vec::new(), Err(error));
        }
        let window = self.kick_sample_loads_within(from_cycle, 4.0, query_budget);
        let library = self.samples.as_ref().expect("enabled sample library");
        let files = match library.warm_score_sounds_async(names, &self.config.score_sample_access) {
            Ok(crate::samples::PrefetchStatus::Requested(files)) => files,
            Ok(crate::samples::PrefetchStatus::Deferred)
            | Ok(crate::samples::PrefetchStatus::Unknown) => 0,
            Err(error) => return (window, Err(RuntimeError::Message(error))),
        };
        self.preload_requested = self.preload_requested.saturating_add(files);
        (window, Ok(()))
    }

    /// Hold until the sounds this set is about to play have arrived.
    ///
    /// Startup only, and not conditional on `preload`: a score written for
    /// strudel.cc has no preload line and must play correctly without one.
    /// The caller warms what the opening cycles ask for, an explicit
    /// `preload` adds what the opening does not reveal, and this waits for
    /// both.
    ///
    /// During a live set the producer never blocks on a network. The
    /// studio's load mode decides what waits instead: in wait, a start holds
    /// its downbeat and an edit keeps the last score playing until their
    /// sounds have loaded; in async, both land at once and a late note is
    /// skipped.
    ///
    /// Returns (files an explicit `preload` asked for, whether the deadline
    /// passed). A deadline that passes is not an error: the set starts and
    /// the late hits play when their samples arrive.
    pub fn wait_for_sample_loads(&mut self, deadline: Duration) -> (usize, bool) {
        let requested = std::mem::take(&mut self.preload_requested);
        let Some(library) = self.samples.clone() else {
            return (requested, false);
        };
        let started = Instant::now();
        library.wait_until_idle(deadline);
        (requested, started.elapsed() >= deadline)
    }

    /// Apply one accepted evaluation's `samples(...)` registrations and
    /// `preload(...)` requests as one ordered loader transaction.
    ///
    /// Registration failures are loud per-call logs, never evaluation
    /// errors - strudel.cc logs and plays on. A score that defines sounds
    /// creates the library even when the host had not enabled defaults yet.
    fn apply_sample_effects(
        &mut self,
        mut effects: rustel_jsruntime::SamplesEffects,
        names: Vec<String>,
    ) {
        if effects.is_empty() && names.is_empty() {
            return;
        }
        if !effects.is_empty() && self.config.score_sample_access.is_denied() {
            let message =
                "setup/score samples() cannot access files or the network without a host grant";
            self.report_diagnostic(
                "samples-failed",
                message,
                serde_json::json!({
                    "samples_failed": {
                        "message": message
                    }
                }),
            );
            effects.clear();
            if names.is_empty() {
                return;
            }
        }
        if self.samples.is_none()
            && let Err(error) = self.enable_default_samples()
        {
            self.report_diagnostic(
                "samples-failed",
                error.to_string(),
                serde_json::json!({ "samples_failed": { "message": error.to_string() } }),
            );
            return;
        }
        let Some(library) = self.samples.clone() else {
            return;
        };
        let requested = match library.register_score_batch_async_for_session(
            effects,
            &names,
            &self.config.score_sample_access,
        ) {
            Ok(crate::samples::PrefetchStatus::Requested(files)) => files,
            Ok(crate::samples::PrefetchStatus::Deferred)
            | Ok(crate::samples::PrefetchStatus::Unknown) => 0,
            Err(error) => {
                self.report_diagnostic(
                    "samples-failed",
                    error.clone(),
                    serde_json::json!({ "samples_failed": { "message": &error } }),
                );
                0
            }
        };
        self.preload_requested = self.preload_requested.saturating_add(requested);
    }

    fn ensure_voicings(&mut self) -> Result<(), RuntimeError> {
        if !self.voicings_ready {
            self.js
                .install_voicings_prebake()
                .map_err(RuntimeError::Js)?;
            self.voicings_ready = true;
        }
        Ok(())
    }

    // Only the device-audio play path consults this today; keep it compiling
    // (not warning) in feature-less builds.
    #[cfg_attr(not(feature = "device-audio"), allow(dead_code))]
    fn sample_lookup(&self) -> &dyn rustel_voice::SampleLookup {
        match &self.samples {
            Some(library) => library.as_ref(),
            None => &rustel_voice::BundledOnly,
        }
    }

    /// Surface loader failures once each (never-die: a missing sample is a
    /// skipped onset plus one loud line, not an error). With direct logging
    /// off they stay for the host's [`Self::take_sample_failures`], maps
    /// and all.
    fn log_sample_failures(&mut self) {
        if !self.direct_diagnostic_logging {
            return;
        }
        for failure in self.take_sample_failures() {
            self.report_diagnostic(
                "sample-failed",
                failure.message.clone(),
                serde_json::json!({ "sample_failed": { "message": failure.message } }),
            );
        }
    }

    /// Queue the notices one [`rustel_voice::with_diagnostic_policy`] scope
    /// collected, as [`VOICE_NOTICE_DIAGNOSTIC`]s. A notice already pending
    /// is not queued again, so one raised every window holds one place.
    fn report_voice_notices(&mut self, notices: Vec<rustel_voice::VoiceNotice>) {
        let notices = notices
            .into_iter()
            .map(|notice| (notice.message, notice.record));
        // A plugin that did not start on the plugin thread has no note to
        // report through, so its reason joins the notices here.
        #[cfg(feature = "vst")]
        let notices = notices.chain(crate::vst::take_errors().into_iter().map(|message| {
            let record = serde_json::json!({ "vst_skipped": { "message": &message } });
            (message, record)
        }));
        for (message, record) in notices {
            let pending = self.pending_diagnostics.iter().any(|diagnostic| {
                diagnostic.kind == VOICE_NOTICE_DIAGNOSTIC && diagnostic.message == message
            });
            if !pending {
                self.report_diagnostic(VOICE_NOTICE_DIAGNOSTIC, message, record);
            }
        }
    }

    pub(crate) fn report_diagnostic(
        &mut self,
        kind: impl Into<String>,
        message: impl Into<String>,
        direct_record: serde_json::Value,
    ) {
        if self.direct_diagnostic_logging {
            #[cfg(test)]
            direct_diagnostics_for_test::record(kind.into());
            eprintln!("{direct_record}");
            return;
        }
        let diagnostic = SessionDiagnostic {
            kind: kind.into(),
            message: message.into(),
            recoverable: true,
        };
        if self.pending_diagnostics.back() == Some(&diagnostic) {
            return;
        }
        if self.pending_diagnostics.len() == MAX_PENDING_SESSION_DIAGNOSTICS {
            self.pending_diagnostics.pop_front();
        }
        self.pending_diagnostics.push_back(diagnostic);
    }

    /// Kick loads for every sample the score references in its first
    /// `cycles`, then wait (bounded) for the library to settle. Called once
    /// before the live transport starts so the first bar is not missing its
    /// drums; NEVER called during a running set (loads stay async there).
    pub fn prefetch_samples(&mut self, cycles: f64, deadline: std::time::Duration) {
        let started = Instant::now();
        self.kick_sample_loads(cycles);
        if let Some(library) = self.samples.clone() {
            library.wait_until_idle(deadline);
            // The first query may have happened while the default/custom
            // manifest was still pending. Once the map exists, resolve the
            // opening window again so its PCM enters the ordinary loader,
            // without granting either phase a fresh deadline.
            if let Some(remaining) = deadline.checked_sub(started.elapsed()) {
                self.kick_sample_loads(cycles);
                library.wait_until_idle(remaining);
            }
        }
        self.log_sample_failures();
    }

    /// Start fetches for the current graph's samples without waiting. A
    /// watch-save that introduces `hh:4` must not wait until the first
    /// converted onset to begin decoding - that onset is skipped as
    /// "still loading". The live loop still delivers via `take_ready`.
    pub fn kick_sample_loads(&mut self, cycles: f64) {
        self.kick_sample_loads_from(0.0, cycles);
    }

    /// Warm the sounds a span of the score will ask for, starting at
    /// `from_cycle`.
    ///
    /// Pass the cycle where the music is, and enough cycles to reach every
    /// name in an alternation such as `<clap:1 clap:6>`, and the next section.
    /// A span fixed at cycle zero misses those sounds, and the artist hears
    /// the later download as a dropped beat.
    ///
    /// Bounded by a wall clock because this runs on the producer thread: a
    /// partial warm-up is fine (the next install warms the rest), a stalled
    /// producer is not.
    pub fn kick_sample_loads_from(&mut self, from_cycle: f64, cycles: f64) {
        self.kick_sample_loads_within(from_cycle, cycles, Duration::from_millis(15));
    }

    /// [`Self::kick_sample_loads_from`] with an explicit query ceiling.
    ///
    /// The 15 ms default exists because a mid-set warm-up runs on the producer
    /// thread, where a long query is a dropout. At STARTUP there is no music to
    /// protect and the opposite is true: a ceiling that refuses the query
    /// resolves no names at all, so nothing is warmed and the set opens on the
    /// downloads it was supposed to avoid. A dense score can then spend about
    /// 15 ms in the query while warming nothing.
    ///
    /// Answers each sound the window resolved, banked spelling included,
    /// with the variants its onsets pick there: what the score is about to
    /// play, names it builds in JavaScript among them. Empty when the query
    /// did not finish.
    pub fn kick_sample_loads_within(
        &mut self,
        from_cycle: f64,
        cycles: f64,
        ceiling: Duration,
    ) -> Vec<(String, crate::sounds::Variants)> {
        let Some(library) = self.samples.clone() else {
            return Vec::new();
        };
        let mut resolved: std::collections::BTreeMap<String, std::collections::BTreeSet<i64>> =
            std::collections::BTreeMap::new();
        let haps = self.window_haps(from_cycle, cycles, ceiling);
        // A plugin loads as a sample does: before the first note asks.
        #[cfg(feature = "vst")]
        crate::vst::prepare(&haps, library.render_rate(), &self.insert_orbits);
        for sound in Self::sounds_in(&haps) {
            // `.bank("tr909")` makes the sound `tr909_bd`. Warming only `bd`
            // fetches a different bank's files, and the lane's first hit is
            // refused. Warm the banked spelling too, as the voice looks it up.
            let variant = crate::sounds::Variants::index(sound.n);
            for name in sound
                .banked
                .as_deref()
                .into_iter()
                .chain([sound.s.as_str()])
            {
                let _ = rustel_voice::SampleLookup::resolve(
                    library.as_ref(),
                    name,
                    sound.n,
                    sound.midi,
                );
                resolved.entry(name.to_owned()).or_default().insert(variant);
            }
        }
        resolved
            .into_iter()
            .map(|(name, variants)| (name, crate::sounds::Variants::Only(variants)))
            .collect()
    }

    /// The sound of every onset the active score has from `from_cycle` for
    /// `cycles`, with the `n` and note it plays; empty when the query does
    /// not finish within `ceiling`. Nothing is loaded.
    pub fn window_sounds(
        &self,
        from_cycle: f64,
        cycles: f64,
        ceiling: Duration,
    ) -> Vec<WindowSound> {
        Self::sounds_in(&self.window_haps(from_cycle, cycles, ceiling))
    }

    /// The plugins the score asks for from `from_cycle` for `cycles` that are
    /// not ready for their notes, at the rate of the output. `holds` says if
    /// the output has a plugin on a slot: see
    /// `LiveScalarDevice::holds_insert`. A start that waits for its sounds
    /// waits while the count is above 0. The call starts the load of each
    /// plugin it counts, and waits for nothing.
    #[cfg(feature = "vst")]
    pub fn plugins_pending(
        &self,
        from_cycle: f64,
        cycles: f64,
        ceiling: Duration,
        sample_rate: u32,
        holds: impl Fn(usize, rustel_audio::InsertKey) -> bool,
    ) -> usize {
        let haps = self.window_haps(from_cycle, cycles, ceiling);
        crate::vst::pending(&haps, sample_rate, &self.insert_orbits, holds)
    }

    /// The haps the active score has from `from_cycle` for `cycles`. Empty
    /// when the query does not finish within `ceiling`.
    fn window_haps(&self, from_cycle: f64, cycles: f64, ceiling: Duration) -> Vec<crate::HapJson> {
        let begin = Fraction::from_f64(from_cycle.max(0.0)).unwrap_or(Fraction::ZERO);
        let span = Fraction::from_f64(cycles.max(1.0)).unwrap_or(Fraction::int(2));
        let ceiling = Instant::now() + ceiling;
        rustel_core::with_query_deadline(ceiling, || self.query_report(begin, begin.add(span)))
            .map(|report| report.haps)
            .unwrap_or_default()
    }

    fn sounds_in(haps: &[crate::HapJson]) -> Vec<WindowSound> {
        let mut sounds = Vec::new();
        for hap in haps {
            let crate::ValueJson::Raw(serde_json::Value::Object(object)) = &hap.value else {
                continue;
            };
            let Some(s) = object.get("s").and_then(|s| s.as_str()) else {
                continue;
            };
            let n = object.get("n").and_then(|n| n.as_f64()).unwrap_or(0.0);
            let midi = match object.get("note") {
                Some(serde_json::Value::String(note)) => {
                    rustel_core::util::note_to_midi(note, 3).unwrap_or(36.0)
                }
                Some(serde_json::Value::Number(note)) => note.as_f64().unwrap_or(36.0),
                _ => 36.0,
            };
            sounds.push(WindowSound {
                s: s.to_owned(),
                banked: rustel_voice::bank_prefixed(object, s).ok().flatten(),
                n,
                midi,
            });
        }
        sounds
    }

    /// What the active score does in the next
    /// [`REPLACEMENT_LOOK_AHEAD_CYCLES`] cycles from `now`. Asked only of a
    /// replacement whose first window refused every onset, to tell a score
    /// nothing can play from one whose first window fell where its sounds
    /// are missing.
    ///
    /// The span starts at `now`, not at the beginning of that cycle: an
    /// onset that has already passed is not evidence the replacement will
    /// play. The rest of the current cycle is still included, so a bank
    /// that alternates by half-cycle is still found.
    ///
    /// It runs on the producer thread, so query and conversion share the
    /// same 15 ms ceiling as [`Self::kick_sample_loads_from`]. A look-ahead
    /// whose query fails answers [`ReplacementLookAhead::Nothing`] and the
    /// replacement is refused as it was before this was asked; one that
    /// runs out of time answers the best evidence it has seen by then,
    /// rather than refusing a score already shown to play. Nothing is
    /// scheduled and nothing is reported: the onsets are converted and
    /// thrown away.
    #[cfg(feature = "device-audio")]
    fn replacement_renders_ahead(
        &self,
        now: f64,
        sample_rate: u32,
        lookup: &dyn rustel_voice::SampleLookup,
    ) -> ReplacementLookAhead {
        const CEILING: Duration = Duration::from_millis(15);
        let cycle = self.cycle_at_time(now);
        if !cycle.is_finite() {
            return ReplacementLookAhead::Nothing;
        }
        let Some(begin) = Fraction::from_f64(cycle.max(0.0)) else {
            return ReplacementLookAhead::Nothing;
        };
        let end = begin.add(Fraction::int(REPLACEMENT_LOOK_AHEAD_CYCLES));
        let deadline = Instant::now() + CEILING;
        let queried = rustel_core::with_query_deadline(deadline, || {
            self.query_with_js_budget(begin, end, CEILING)
        });
        // Not a scheduled window: drop any console the look-ahead produced
        // so those lines cannot surface as the next tick's logs.
        let _ = self.js.take_logs();
        let Ok(haps) = queried else {
            return ReplacementLookAhead::Nothing;
        };
        let cps = self.config.cps;
        let generation = self.generation();
        let (look_ahead, _notices) = rustel_voice::with_diagnostic_policy(false, || {
            // Every onset in the stretch is converted, not only up to the
            // first that renders: an invalid control anywhere ahead
            // refuses the replacement the way the same voice in the first
            // window would.
            let mut rendered = false;
            let mut loading = None;
            let mut invalid_control = None;
            for hap in haps.iter().filter(|hap| hap.has_onset()) {
                if Instant::now() >= deadline {
                    break;
                }
                let duration = hap.duration().to_f64();
                let duration_secs = if cps > 0.0 && duration.is_finite() && duration > 0.0 {
                    duration / cps
                } else {
                    0.25
                };
                let whole_begin = hap.whole_or_part().begin;
                let onset = OnsetEventJson {
                    live_controls: hap.live_controls,
                    onset_id: 0,
                    generation,
                    whole_begin: whole_begin.show(),
                    duration_secs,
                    target_time: self.time_at_cycle(whole_begin),
                    value: ValueJson::from_value(&hap.value),
                    value_show: String::new(),
                    ui_visuals: hap.ui_visuals_context(),
                    log_line: None,
                };
                match crate::render::live_audio_event(&onset, sample_rate, cps, lookup) {
                    Ok(_) => rendered = true,
                    Err(error @ rustel_voice::VoiceError::SampleLoading(_)) => {
                        loading = Some(error.to_string());
                    }
                    Err(error @ rustel_voice::VoiceError::InvalidControl(_)) => {
                        invalid_control.get_or_insert_with(|| error.to_string());
                    }
                    Err(_) => {}
                }
            }
            if let Some(message) = invalid_control {
                ReplacementLookAhead::InvalidControl(message)
            } else if rendered {
                ReplacementLookAhead::Renders
            } else if let Some(message) = loading {
                ReplacementLookAhead::Loading(message)
            } else {
                ReplacementLookAhead::Nothing
            }
        });
        look_ahead
    }

    /// Follow an outside clock: from `now` the scheduler runs at `cps` with
    /// `cycle` at `now`, and event durations and synced effects convert at
    /// the same rate. A steer the scheduler refuses changes neither rate.
    /// Re-query afterwards ([`Self::requery_active_at`]) so the device hears it.
    pub fn retime(&mut self, now: f64, cps: f64, cycle: f64) {
        self.scheduler.retime(now, cps, cycle);
        self.config.cps = self.scheduler.cps();
    }

    /// Make the next live reload take over at `time` (device seconds):
    /// the old generation keeps sounding until then, the new one is queried
    /// from there. Consumed by that reload.
    /// Drop a takeover time the reload it was set for never consumed.
    pub fn clear_next_takeover_time(&mut self) {
        self.takeover_override = None;
    }

    pub fn set_next_takeover_time(&mut self, time: f64) {
        self.takeover_override = Some(time);
    }

    /// Play the next live reload's score from its own beginning.
    ///
    /// A reload normally joins the cycle already running, which keeps a
    /// live edit in time. A different score, such as another scene or
    /// another take, often needs the opposite: to be heard from its first
    /// event, as after a stop and a start.
    ///
    /// The transport does not change. Only the installed score is shifted,
    /// so its cycle zero falls on the moment it takes over. Everything else
    /// that reads the clock continues unchanged: the tail of the replaced
    /// score, the outgoing MIDI clock, the visuals. Consumed by that reload.
    pub fn start_next_from_zero(&mut self) {
        self.next_from_zero = true;
    }

    /// Drop a from-zero request the reload it was set for never consumed.
    pub fn clear_next_from_zero(&mut self) {
        self.next_from_zero = false;
    }

    /// The live-reload re-query cursor margin: how far past `now` the audio
    /// consumer has already drained events. Old-generation events beyond it
    /// are dropped at the swap and re-scheduled by the new generation, so a
    /// margin at the true consumption frontier closes the reload gap.
    pub fn set_continuity_margin(&mut self, margin: f64) {
        self.continuity_margin = margin.is_finite().then(|| margin.max(0.0));
        self.sync_live_schedule_horizon();
    }

    /// How far ahead of `now` a live tick must query so a reload's takeover
    /// (the consumption frontier) still sits inside already-scheduled cover.
    ///
    /// Default horizon is 0.5 s. WSLg playback latency can grow past that;
    /// takeover then lands after the last queued onset and the tee records a
    /// silent second on the save.
    fn live_schedule_cover(&self) -> f64 {
        let margin = self.continuity_margin.unwrap_or(self.schedule_lead);
        let margin = if margin.is_finite() {
            margin.max(0.0)
        } else {
            0.0
        };
        self.config.horizon + margin
    }

    /// Seconds ahead of `now` a live producer keeps scheduled: the horizon
    /// plus the continuity margin, and one more cycle (capped) for a
    /// callback-bearing score whose cover already reaches the one-cycle query
    /// cap. The extra cycle adds retained batches, not query span.
    #[cfg(feature = "device-audio")]
    pub fn live_producer_schedule_cover(&self) -> f64 {
        let base = self.live_schedule_cover();
        if !self.active_needs_host() {
            return base;
        }
        let cps = self.scheduler.cps();
        if !cps.is_finite() || cps <= 0.0 || base * cps < MAX_LIVE_IMPURE_QUERY_SPAN_CYCLES {
            return base;
        }
        base + (LIVE_IMPURE_BURST_RESERVE_CYCLES / cps)
            .min(LIVE_IMPURE_BURST_RESERVE_MAX.as_secs_f64())
    }

    fn sync_live_schedule_horizon(&mut self) {
        self.scheduler.set_horizon(self.live_schedule_cover());
    }

    /// The re-query cursor time of the newest continuous reload, consumed by
    /// the live producer to publish the device takeover frame alongside the
    /// generation flip. `None` for initial installs and transport restarts
    /// (nothing earlier survives those).
    pub fn take_requery_takeover_time(&mut self) -> Option<f64> {
        self.requery_takeover_time.take()
    }

    /// Consume the newest reload's takeover **and** what the consumer
    /// should do to what sounds under it ([`TakeoverCut`]) - an edit lets
    /// it ring out, an immediate rewind cuts at the flip, a quantised
    /// rewind cuts at the takeover (its countdown plays). Reading these
    /// together keeps the pair atomic from the producer's view: a cut
    /// whose takeover was consumed separately would either fire twice or
    /// never.
    pub fn take_requery_takeover(&mut self) -> Option<(f64, TakeoverCut)> {
        let cut = std::mem::take(&mut self.requery_takeover_cut);
        self.take_requery_takeover_time().map(|time| (time, cut))
    }

    /// Put a consumed `(takeover, cut)` pair back, unchanged.
    ///
    /// The producer's rewind hold uses this: a from-zero first window whose
    /// samples are still loading defers its publication, but the takeover
    /// and its cut must still be consumed by THAT replacement when the
    /// window finally publishes. Reading and re-arming keeps the pair
    /// atomic - the alternative (peeking the cut, consuming the time)
    /// could separate them.
    pub fn rearm_requery_takeover(&mut self, takeover: (f64, TakeoverCut)) {
        self.requery_takeover_time = Some(takeover.0);
        self.requery_takeover_cut = takeover.1;
    }

    /// Slide an unpublished rewind's cycle zero to `now` once its anchor is
    /// behind the clock.
    ///
    /// A rewind's cycle zero is anchored at install: the line for a
    /// quantised launch, the edit instant for an immediate one. The producer
    /// can reach the anchor late: an immediate rewind always does, by the
    /// install-to-publication time; a quantised one does when its evaluation
    /// outlasts its line; any rewind does when it is held for a loading
    /// sample. The first window's onsets are then in the rendered past. A
    /// voice admitted that late starts partway into its sample: the downbeat
    /// loses its attack, and a one-shot shorter than the delay never sounds.
    /// Without the slide, a hold longer than the schedule cover also lets
    /// the gap skip move the cursor past cycle zero, and a retry that
    /// reaches cycle zero queries every missed onset as one burst.
    ///
    /// A restart is the loop from its top, so the mapping re-anchors cycle
    /// zero at `now`, the takeover moves with it, and the cut becomes the
    /// flip's (whatever line it was waiting for has passed; the consumer
    /// silences the old score where the new one begins). The window is
    /// reopened first so nothing an earlier unpublished turn queried stays
    /// marked emitted.
    ///
    /// Returns whether it slid. A takeover still ahead (a quantised launch
    /// mid-countdown) is left on its line.
    #[cfg(feature = "device-audio")]
    pub(crate) fn slide_pending_rewind_anchor(&mut self, now: f64) -> bool {
        if self.requery_takeover_cut == TakeoverCut::None || !now.is_finite() {
            return false;
        }
        let Some(anchor) = self.requery_anchor_time else {
            return false;
        };
        if now <= anchor {
            return false;
        }
        self.scheduler.reopen_window_at(now, anchor);
        self.scheduler.rebase_start_anchor(now);
        self.requery_anchor_time = Some(now);
        if self.requery_takeover_time.is_some() {
            self.requery_takeover_time = Some(now);
        }
        self.requery_takeover_cut = TakeoverCut::AtFlip;
        true
    }

    /// Forget the current replacement's requery anchor: consumed with its
    /// window (published or rolled back), or superseded by a newer install.
    #[cfg(feature = "device-audio")]
    pub(crate) fn clear_requery_anchor(&mut self) {
        self.requery_anchor_time = None;
    }

    /// The live producer published `generation` to its device, to take over
    /// on `takeover_frame` with `cut`. The ledger of latency-compensated
    /// events follows the flip: it keeps what the device keeps of the
    /// generations going out.
    #[cfg(feature = "device-audio")]
    pub(crate) fn audio_takeover_published(
        &mut self,
        generation: u64,
        takeover_frame: u64,
        cut: TakeoverCut,
    ) {
        self.compensated_onsets
            .published(generation, takeover_frame, cut);
        // A cut begins a new timeline. Its first window went out whole, and
        // no later window of it meets a message of the score it cut.
        if cut != TakeoverCut::None {
            self.forget_handed_out();
        }
    }

    /// Forget how far a host handed the OSC and serial messages out. A new
    /// timeline begins: a transport start, a takeover with a cut, or the
    /// refill of an output that discarded its ring.
    #[cfg(feature = "device-audio")]
    fn forget_handed_out(&mut self) {
        #[cfg(feature = "osc")]
        self.osc_handed_out.clear();
        #[cfg(feature = "serial")]
        self.serial_handed_out.clear();
    }

    /// Tell the session that the host handed an `.osc()` bundle to the
    /// network: a bundle of `generation` for the onset at `target_time`, in
    /// seconds on the session clock.
    ///
    /// A bundle that is out cannot be recalled. It stands for the bundle a
    /// later generation stages for the same onset, and the live schedule
    /// leaves that one out. So an edit reaches OSC from the latest onset
    /// handed out, up to one schedule cover after it reaches the audio, and
    /// an onset the edit adds before that is not sent. A takeover with a cut
    /// and a transport start send all their bundles.
    ///
    /// A host that does not call this sends twice each onset that the
    /// outgoing generation handed out past a takeover.
    #[cfg(all(feature = "device-audio", feature = "osc"))]
    pub fn note_osc_handed_out(&mut self, generation: u64, target_time: f64) {
        self.osc_handed_out.note(generation, target_time);
    }

    /// Tell the session that the host handed a `.serial()` write to its
    /// sender: a write of `generation` for the onset at `target_time`, in
    /// seconds on the session clock. A write in the sender cannot be
    /// recalled, and it stands for a later generation's write of the same
    /// onset as an OSC bundle does ([`Self::note_osc_handed_out`]).
    #[cfg(all(feature = "device-audio", feature = "serial"))]
    pub fn note_serial_handed_out(&mut self, generation: u64, target_time: f64) {
        self.serial_handed_out.note(generation, target_time);
    }

    /// Leave out the staged OSC and serial intents of each onset that a
    /// message of an earlier generation is already out for. `onsets` are
    /// the onsets of the window that staged them.
    ///
    /// A takeover with a cut begins a new timeline: until it is published,
    /// the intents of its first window all stay.
    ///
    /// The intents of a key press that this window is the first to place
    /// stay too: no message is out for that press.
    #[cfg(all(feature = "device-audio", any(feature = "osc", feature = "serial")))]
    fn leave_out_handed_out_intents(&mut self, sample_rate: u32, onsets: &[OnsetEventJson]) {
        let mut staged = false;
        #[cfg(feature = "osc")]
        {
            staged |= !self.pending_osc.is_empty();
        }
        #[cfg(feature = "serial")]
        {
            staged |= !self.pending_serial.is_empty();
        }
        // A window that staged no intent hands no message out.
        if !staged {
            return;
        }
        // Before the cut is read: a window whose intents all stay counts
        // its presses as placed too.
        let placed = self.js.midi_input_bus().placed_keys();
        let new_presses = self.placed_presses.claim_new(&placed, onsets);
        if self.requery_takeover_cut != TakeoverCut::None {
            return;
        }
        #[cfg(feature = "osc")]
        {
            let handed_out = &self.osc_handed_out;
            self.pending_osc.retain(|(_, intent)| {
                new_presses.contains(&intent.onset_id)
                    || !handed_out.stands_for(intent.generation, intent.target_time, sample_rate)
            });
        }
        #[cfg(feature = "serial")]
        {
            let handed_out = &self.serial_handed_out;
            self.pending_serial.retain(|(_, intent)| {
                new_presses.contains(&intent.onset_id)
                    || !handed_out.stands_for(intent.generation, intent.target_time, sample_rate)
            });
        }
    }

    /// Re-open a consumed prefill window whose onsets never all converted.
    ///
    /// A rewind's first window that holds for a loading sample has already
    /// consumed the scheduler's cursor: its onsets, refused and ready alike,
    /// are marked emitted. A plain retry would resume past cycle zero and
    /// skip the restart's first beat. This moves the cursor back to the
    /// rewind's anchor ([`Self::requery_anchor_time`], where cycle zero
    /// sits) and clears this generation's emission marks from there, so the
    /// same span re-queries in full once the samples are ready. Nothing of
    /// the held window reached the device ring, so nothing can double.
    /// An ordinary loading skip has no anchor and reopens at its takeover.
    ///
    /// On success the takeover pair that the producer consumed on the held
    /// turn is re-armed, so the same replacement consumes it when the
    /// reopened window publishes. Returns `false`, with the pair not
    /// re-armed, when there is nothing past the anchor to reopen; the
    /// caller then puts the pair back itself.
    #[cfg(feature = "device-audio")]
    pub fn reopen_loading_window(&mut self, now: f64, takeover: (f64, TakeoverCut)) -> bool {
        // Only a cut takeover has a distinct, earlier anchor. An ordinary
        // loading skip keeps its far-cursor contract: its takeover IS its
        // window's open.
        let anchor = if takeover.1 == TakeoverCut::None {
            takeover.0
        } else {
            self.requery_anchor_time.unwrap_or(takeover.0)
        };
        let opened = self.scheduler.reopen_window_at(now, anchor);
        if opened {
            self.rearm_requery_takeover(takeover);
        }
        opened
    }

    /// Device schedule lead used by continuous live reloads (see
    /// `install_evaluated_score`); hosts set it once the device reports its
    /// playback latency.
    pub fn set_schedule_lead(&mut self, lead: f64) {
        self.schedule_lead = if lead.is_finite() { lead.max(0.0) } else { 0.0 };
        self.sync_live_schedule_horizon();
    }

    /// Limit haps produced by one query in the install probe, scheduler, and
    /// direct/preview query paths. Sessions retain the core default until set.
    pub fn set_query_hap_budget(&mut self, budget: u64) -> Result<(), RuntimeError> {
        if !(1..=rustel_core::DEFAULT_HAP_BUDGET).contains(&budget) {
            return Err(RuntimeError::Message(format!(
                "query hap budget must be between 1 and {}",
                rustel_core::DEFAULT_HAP_BUDGET
            )));
        }
        self.scheduler.set_query_hap_budget(budget);
        Ok(())
    }

    /// Opt in to the scheduler's bounded editor/visualizer observation stream.
    ///
    /// Disabled sessions pay only the scheduler's false branch. The trace is
    /// observational: a slow or absent UI cannot apply back-pressure to the
    /// audio schedule.
    pub fn set_schedule_trace_enabled(&mut self, enabled: bool) {
        self.scheduler.set_trace_enabled(enabled);
    }

    /// Drain accepted onsets waiting for an editor/visualizer client.
    pub fn take_schedule_trace_events(&mut self) -> Vec<rustel_scheduler::ScheduleTraceEvent> {
        self.scheduler.take_trace_events()
    }

    /// Drain trace events into a reusable host buffer.
    pub fn drain_schedule_trace_events_into(
        &mut self,
        events: &mut Vec<rustel_scheduler::ScheduleTraceEvent>,
    ) {
        self.scheduler.drain_trace_events_into(events);
    }

    /// Drain and reset trace records discarded at the scheduler's fixed cap.
    pub fn take_schedule_trace_events_dropped(&mut self) -> u64 {
        self.scheduler.take_trace_events_dropped()
    }

    /// Current cycles-per-second (read-only; changes ride evaluated source).
    pub fn cps(&self) -> f64 {
        self.scheduler.cps()
    }

    /// Wall-clock time of `cycle` under the current transport anchor.
    pub fn time_at_cycle(&self, cycle: Fraction) -> f64 {
        self.scheduler.time_at_cycle(cycle)
    }

    /// Cycle position at wall-clock `time` under the current anchor.
    pub fn cycle_at_time(&self, time: f64) -> f64 {
        self.scheduler.cycle_at_time(time)
    }

    /// The transport, so a caller can STOP a run in progress.
    ///
    /// `play` borrows the session mutably for its whole window, so cancellation
    /// cannot come through `&mut self` - it has to be a handle taken before the
    /// run starts. Stop is immediate by contract: the scheduler discards queued
    /// events rather than delivering them late, so a cancelled run does not
    /// finish playing what it had already scheduled.
    pub fn transport(&self) -> Arc<Transport> {
        self.transport.clone()
    }

    /// Lower the QuickJS heap ceiling for this session.
    ///
    /// Exposed so callers can exercise the session-level heap bound at a small
    /// size. Monotonic and refused below live usage, as at the host layer.
    pub fn set_js_memory_limit(&self, bytes: usize) -> Result<(), RuntimeError> {
        self.js
            .set_memory_limit(bytes)
            .map_err(RuntimeError::Message)
    }

    /// Bytes currently live on the QuickJS heap.
    ///
    /// A loaded session already holds tens of megabytes for the control
    /// surface, and that figure moves as patterns are evaluated - so a test
    /// wanting a ceiling "just above current usage" has to ask rather than
    /// guess.
    pub fn js_heap_live(&self) -> usize {
        self.js.heap_live()
    }

    /// Run QuickJS garbage collection.
    ///
    /// Two passes, matching the rest of this crate's tests: the first
    /// collects, the second reclaims cycles the first uncovered. A host
    /// uses this to tell live (rooted) heap from garbage a collection
    /// would free. The studio does not call it; QuickJS collects on its
    /// own schedule.
    pub fn run_js_gc(&self) {
        self.js.run_gc();
        self.js.run_gc();
    }

    /// Whether the active graph needs the QuickJS callback host to be queried.
    ///
    /// The scheduler queries on its own clock, so `play`/`render` have to know
    /// whether to install the host scope. Exposed because it is also the
    /// executable form of the purity claim: a source that reaches no JavaScript
    /// must schedule with nothing installed.
    pub fn active_needs_host(&self) -> bool {
        self.js.active_needs_host()
    }

    /// Query the active graph over `[begin, end)` in cycle time.
    pub fn query(&self, begin: Fraction, end: Fraction) -> Result<Vec<Hap>, RuntimeError> {
        self.query_with_js_budget(begin, end, QUERY_JS_CPU_BUDGET)
    }

    /// How far the scheduler has queried the active graph, in cycles.
    pub fn scheduled_to_cycle(&self) -> f64 {
        self.scheduler.scheduled_to_cycle()
    }

    /// What is coming: the onsets of the active graph in `[from_cycle,
    /// to_cycle)`, as traces a painter can place ahead of the audio. Read
    /// with the non-advancing query and without `_cps`, so nothing the
    /// scheduler owns moves and no MIDI key is placed. Sorted by onset, at
    /// most one UI batch of them; their onset ids carry
    /// [`PREVIEW_ONSET_ID_FLAG`]. Empty when nothing is playing.
    pub fn preview_traces(
        &self,
        from_cycle: f64,
        to_cycle: f64,
        generation: u64,
    ) -> Result<Vec<rustel_scheduler::ScheduleTraceEvent>, RuntimeError> {
        if !self.js.has_active_pattern()
            || !from_cycle.is_finite()
            || !to_cycle.is_finite()
            || to_cycle <= from_cycle
        {
            return Ok(Vec::new());
        }
        let (Some(begin), Some(end)) =
            (Fraction::from_f64(from_cycle), Fraction::from_f64(to_cycle))
        else {
            return Ok(Vec::new());
        };
        let haps = self.query_with_js_budget(begin, end, PREVIEW_QUERY_JS_BUDGET)?;
        let mut onsets = haps
            .into_iter()
            .filter(|hap| hap.has_onset())
            .filter_map(|hap| {
                let whole = hap.whole?;
                (whole.begin >= begin).then(|| (whole.begin, hap.value.show(), hap))
            })
            .collect::<Vec<_>>();
        onsets.sort_by(|left, right| left.0.cmp(&right.0).then_with(|| left.1.cmp(&right.1)));
        onsets.truncate(crate::ui_events::MAX_UI_EVENTS_PER_BATCH);
        Ok(onsets
            .into_iter()
            .enumerate()
            .filter_map(|(index, (begin, _, hap))| {
                rustel_scheduler::ScheduleTraceEvent::from_hap(
                    &hap,
                    generation,
                    PREVIEW_ONSET_ID_FLAG | index as u64,
                    self.time_at_cycle(begin),
                )
            })
            .collect())
    }

    /// Query with an explicit synchronous-JavaScript budget.
    ///
    /// Kept private because the product contract is the fixed ceiling above;
    /// focused tests inject a tiny duration so the real interrupt path is
    /// mutation-sensitive without making the suite wait two seconds.
    fn query_with_js_budget(
        &self,
        begin: Fraction,
        end: Fraction,
        budget: Duration,
    ) -> Result<Vec<Hap>, RuntimeError> {
        if self.panic_poisoned {
            return Err(RuntimeError::Panic(
                "native score work panicked; Session recovery is unavailable".into(),
            ));
        }
        self.panic_is_query.set(true);
        // Cancellable: a dense query is long enough for a user to change their
        // mind about, and a JavaScript callback can otherwise prevent the
        // recursive Rust checks from ever regaining control. One caller-owned
        // flag therefore drives both core cancellation and QuickJS's interrupt
        // handler under the same outer query boundary.
        let cancellation = self.transport.stopped_flag();
        let query = || {
            #[cfg(any(test, feature = "test-support"))]
            self.panic_if_injected(SessionPanicPoint::Query);
            let result = rustel_core::with_cancellation(cancellation, || {
                self.js
                    .query_cancellable_with_hap_budget(
                        Slot::Active,
                        0,
                        begin,
                        end,
                        budget,
                        self.scheduler.query_hap_budget(),
                        cancellation,
                    )
                    .map_err(RuntimeError::from)
            });
            self.js.raise_caught_native_panic();
            result
        };
        // Inside a recovery boundary a panic rebuilds the Session; a query
        // made outside one, such as `rustel query`, reports it as an error.
        let result = if recovery::panic_is_contained() {
            query()
        } else {
            recovery::catch_score_panic(query).unwrap_or_else(|message| {
                Err(RuntimeError::Panic(format!(
                    "native score work panicked ({message})"
                )))
            })
        };
        self.panic_is_query.set(false);
        result
    }

    pub fn query_report(
        &self,
        begin: Fraction,
        end: Fraction,
    ) -> Result<QueryReport, RuntimeError> {
        let haps = self.query(begin, end)?;
        // A stack contains a lane's throw and keeps the other lanes' haps.
        // The report still says that the score threw.
        let contained = rustel_core::take_query_contained_throw();
        Ok(QueryReport {
            begin: fraction_label(begin),
            end: fraction_label(end),
            source: self.last_source.as_deref().unwrap_or_default().to_owned(),
            haps: haps_to_json(&haps),
            // Taken after the query so the report owns the message; a
            // following query starts clean.
            query_threw: self.js.take_query_throw().or(contained),
        })
    }

    /// Fill and drain one live scheduler instant without resetting its clock,
    /// active graph, generation, or transport.
    ///
    /// This is the reusable boundary for file-watch playback and a future
    /// sample-clock producer. It may enter JavaScript on this caller thread;
    /// the returned values are fully owned plain data suitable for conversion
    /// into the audio ring's POD events off the real-time callback.
    pub fn schedule_at(&mut self, now: f64) -> Result<Vec<OnsetEventJson>, RuntimeError> {
        self.schedule_through(now, now)
    }

    /// Fill at `now` and transfer events through `through` for a downstream
    /// lookahead buffer. The deadline cannot exceed the live schedule cover
    /// (configured horizon plus any continuity margin).
    pub fn schedule_through(
        &mut self,
        now: f64,
        through: f64,
    ) -> Result<Vec<OnsetEventJson>, RuntimeError> {
        self.schedule_through_with_js_budget(now, through, QUERY_JS_CPU_BUDGET)
    }

    /// One scheduling fill with an explicit query-time JavaScript budget.
    ///
    /// Public/offline production routes supply [`QUERY_JS_CPU_BUDGET`], while
    /// the continuous-device route supplies its cover-derived live budget.
    /// The parameter also keeps refusal/state atomicity cheap to exercise in
    /// focused tests.
    fn schedule_through_with_js_budget(
        &mut self,
        now: f64,
        through: f64,
        budget: Duration,
    ) -> Result<Vec<OnsetEventJson>, RuntimeError> {
        self.schedule_through_with_js_budget_and_tail(now, through, budget)
            .map(|(events, _)| events)
            .map_err(ScheduleAttemptError::into_runtime_error)
    }

    fn schedule_through_with_js_budget_and_tail(
        &mut self,
        now: f64,
        through: f64,
        budget: Duration,
    ) -> Result<(Vec<OnsetEventJson>, Instant), ScheduleAttemptError> {
        self.schedule_through_with_js_budget_and_tail_with_cover(
            now,
            through,
            budget,
            self.live_schedule_cover(),
            None,
        )
        .map(|window| (window.events, window.tail_started))
    }

    fn schedule_through_with_js_budget_and_tail_with_cover(
        &mut self,
        now: f64,
        through: f64,
        budget: Duration,
        max_cover: f64,
        profile: Option<&mut rustel_scheduler::SchedulerTickProfile>,
    ) -> Result<ScheduledWindow, ScheduleAttemptError> {
        self.with_panic_recovery(now, |session| {
            Ok(
                session.schedule_through_with_js_budget_and_tail_with_cover_inner(
                    now, through, budget, max_cover, profile,
                ),
            )
        })
        .map_err(ScheduleAttemptError::Atomic)?
    }

    fn schedule_through_with_js_budget_and_tail_with_cover_inner(
        &mut self,
        now: f64,
        through: f64,
        budget: Duration,
        max_cover: f64,
        mut profile: Option<&mut rustel_scheduler::SchedulerTickProfile>,
    ) -> Result<ScheduledWindow, ScheduleAttemptError> {
        if !now.is_finite() || now < 0.0 {
            return Err(ScheduleAttemptError::Atomic(RuntimeError::Message(
                format!("scheduler clock must be a finite non-negative number, got {now}"),
            )));
        }
        if !max_cover.is_finite()
            || max_cover < 0.0
            || !through.is_finite()
            || through < now
            || through > now + max_cover
        {
            return Err(ScheduleAttemptError::Atomic(RuntimeError::Message(
                format!(
                    "scheduler transfer deadline must be within [{now}, {}], got {through}",
                    now + max_cover
                ),
            )));
        }
        if !self.js.has_active_pattern() {
            return Err(ScheduleAttemptError::Atomic(RuntimeError::NoPattern));
        }
        self.panic_is_query.set(true);
        #[cfg(any(test, feature = "test-support"))]
        self.panic_if_injected(SessionPanicPoint::Query);
        let clock = VirtualClock::new(now);
        let cancellation = self.transport.stopped_flag();
        let js = &self.js;
        let scheduler = &mut self.scheduler;
        let status = js
            .with_runtime_settings(|| {
                rustel_core::with_cancellation(cancellation, || {
                    if js.active_needs_host() {
                        js.with_active_scope_cancellable(budget, cancellation, || {
                            match profile.as_deref_mut() {
                                Some(profile) => scheduler.tick_with_max_span_cycles_profiled(
                                    &clock,
                                    MAX_LIVE_IMPURE_QUERY_SPAN_CYCLES,
                                    profile,
                                ),
                                None => scheduler.tick_with_max_span_cycles(
                                    &clock,
                                    MAX_LIVE_IMPURE_QUERY_SPAN_CYCLES,
                                ),
                            }
                        })
                        .map_err(RuntimeError::from)
                    } else {
                        // Pure graphs need no JavaScript host, but long native
                        // queries still inherit the cancellation scope.
                        Ok(match profile {
                            Some(profile) => scheduler.tick_profiled(&clock, profile),
                            None => scheduler.tick(&clock),
                        })
                    }
                })
            })
            .map_err(ScheduleAttemptError::Atomic)?;
        match status {
            rustel_scheduler::TickStatus::Stopped => {
                return Err(ScheduleAttemptError::Atomic(RuntimeError::Cancelled));
            }
            rustel_scheduler::TickStatus::Refused => {
                let refusal = self.scheduler.refusal().cloned().ok_or_else(|| {
                    ScheduleAttemptError::Atomic(RuntimeError::Message(
                        "scheduler refused without a typed reason".into(),
                    ))
                })?;
                return Err(ScheduleAttemptError::Atomic(refusal.into()));
            }
            rustel_scheduler::TickStatus::QueueFull => {
                return Err(ScheduleAttemptError::Partial(RuntimeError::ResourceLimit(
                    format!(
                        "live scheduling exceeded the queue cap {}",
                        rustel_scheduler::MAX_QUEUED_EVENTS
                    ),
                )));
            }
            rustel_scheduler::TickStatus::Filled | rustel_scheduler::TickStatus::HorizonFull => {}
        }

        // A query that throws yields no haps and is reported, as on
        // strudel.cc. Without the report, a score whose lanes all throw
        // renders nothing and reports success.
        let thrown = self.scheduler.take_thrown();
        let query_threw = thrown.is_some();
        let generation = self.generation();
        if let Some(message) = thrown
            && query_failure_is_news(generation, &message, &mut self.reported_query_throw)
        {
            self.report_diagnostic(
                "query-threw",
                message.clone(),
                serde_json::json!({ "query_threw": { "message": message } }),
            );
        }
        if let Some(message) = self.scheduler.take_callback_failure()
            && query_failure_is_news(generation, &message, &mut self.reported_callback_failure)
        {
            self.report_diagnostic(
                "query-threw",
                message.clone(),
                serde_json::json!({ "query_threw": { "message": message } }),
            );
        }
        // Live playback warns of a stack-contained throw through the
        // callback-failure channel; taken here so no later offline window
        // reports it.
        let _ = self.scheduler.take_contained_throw();

        let tail_started = Instant::now();
        let cps = self.config.cps;
        let drained = self.scheduler.drain_through(&clock, through);
        let events = drained
            .into_iter()
            .map(|event| OnsetEventJson {
                onset_id: event.onset_id,
                generation: event.generation,
                whole_begin: event.whole_begin.show(),
                duration_secs: event.duration.to_f64() / cps,
                target_time: event.target_time,
                value: ValueJson::from_value(&event.value),
                value_show: event.value.show(),
                live_controls: event.live_controls,
                ui_visuals: event.ui_visuals,
                log_line: event.log_line.as_deref().map(str::to_owned),
            })
            .collect();
        Ok(ScheduledWindow {
            events,
            tail_started,
            status,
            query_threw,
        })
    }

    /// Produce fixed-layout audio-ring events off the real-time thread.
    ///
    /// This works without `device-audio`: a host can send the returned events
    /// through [`rustel_audio::Ring`] to its own audio callback. `now` is in
    /// seconds on the session clock; `sample_rate` must match the host's DSP
    /// rate so each event's absolute `target_frame` uses the same clock.
    pub fn schedule_audio_at(
        &mut self,
        now: f64,
        sample_rate: u32,
    ) -> Result<Vec<rustel_audio::AudioEvent>, RuntimeError> {
        self.schedule_audio_through(now, now + self.config.horizon, sample_rate)
    }

    /// Produce fixed-layout audio-ring events through `through`, entirely
    /// off the real-time thread, without requiring an audio device backend.
    ///
    /// `now` and `through` are seconds on the session clock. The deadline is
    /// limited to the configured lookahead horizon, as in
    /// [`Self::schedule_through`]. Scheduling advances the shared cursor, so
    /// callers should use one scheduling method per window.
    pub fn schedule_audio_through(
        &mut self,
        now: f64,
        through: f64,
        sample_rate: u32,
    ) -> Result<Vec<rustel_audio::AudioEvent>, RuntimeError> {
        self.with_panic_recovery(now, |session| {
            session.schedule_audio_through_guarded(now, through, sample_rate)
        })
    }

    fn schedule_audio_through_guarded(
        &mut self,
        now: f64,
        through: f64,
        sample_rate: u32,
    ) -> Result<Vec<rustel_audio::AudioEvent>, RuntimeError> {
        // Per-voice refusals (unknown sound, unsupported control) skip THAT
        // voice, exactly as a dropped event leaves the rest playing. One bad
        // onset must never silence the whole batch. Each distinct refusal is
        // logged once.
        self.log_sample_failures();
        let library = self.samples.clone();
        let bundled = rustel_voice::BundledOnly;
        let lookup: &dyn rustel_voice::SampleLookup = match &library {
            Some(library) => library.as_ref(),
            None => &bundled,
        };
        let mut refusals = Vec::new();
        let cps = self.config.cps;
        let onsets = self.schedule_through(now, through)?;
        // Emit `.log()` text for scheduled onsets. Formatting happens during
        // the pattern query, but emission waits until an onset is accepted
        // so re-querying a span cannot print the same onset twice.
        //
        // `logger(...)` and `console.log(...)` are drained in the same pass.
        // Those are written while a score EVALUATES rather than when a note
        // sounds, so they come out at the first opportunity after.
        let mut logged: Vec<String> = self.js.take_logs();
        logged.extend(onsets.iter().filter_map(|onset| onset.log_line.clone()));
        for line in logged {
            self.report_diagnostic(
                "log",
                line.clone(),
                serde_json::json!({ "log": { "message": line } }),
            );
        }

        #[cfg(feature = "serial")]
        {
            self.collect_pending_serial(&onsets, now);
        }
        // Collected in the SAME pass: `schedule_through` advances the
        // scheduler, so asking again for this window would return the next
        // onsets, not these.
        #[cfg(feature = "midi")]
        {
            self.collect_pending_midi(&onsets);
        }
        #[cfg(feature = "osc")]
        self.collect_pending_osc(&onsets, now, cps);

        let mut input_warnings: Vec<String> = Vec::new();
        let (events, notices) =
            rustel_voice::with_diagnostic_policy(self.direct_diagnostic_logging, || {
                onsets
                    .iter()
                    .filter_map(|onset| {
                        match crate::render::live_audio_event(onset, sample_rate, cps, lookup) {
                            Ok(event) => {
                                #[cfg(feature = "vst")]
                                let event = {
                                    let mut event = event;
                                    event.controls.insert_orbit = Some(
                                        self.insert_orbits[(event.controls.orbit as usize)
                                            .min(rustel_audio::MAX_ORBITS - 1)],
                                    );
                                    event
                                };
                                if let Some(warning) = self.input_channel_warning(&event)
                                    && !input_warnings.contains(&warning)
                                {
                                    input_warnings.push(warning);
                                }
                                Some(event)
                            }
                            Err(error) => {
                                let message = error.to_string();
                                if refusals.last() != Some(&message) {
                                    refusals.push(message);
                                }
                                None
                            }
                        }
                    })
                    .collect()
            });
        self.report_voice_notices(notices);
        for message in refusals {
            self.report_diagnostic(
                "voice-refused",
                message.clone(),
                serde_json::json!({ "voice_refused": { "message": message } }),
            );
        }
        for message in input_warnings {
            if self.input_warnings_reported.insert(message.clone()) {
                self.report_diagnostic(
                    "audio-input",
                    message.clone(),
                    serde_json::json!({ "audio_input": { "message": message } }),
                );
            }
        }
        Ok(events)
    }

    /// Take the serial intents from the last scheduling pass. A live host
    /// tells the session of each write its sender takes, with
    /// `note_serial_handed_out`.
    #[cfg(feature = "serial")]
    pub fn take_pending_serial(&mut self) -> Vec<(f64, crate::serial_bridge::SerialOnset)> {
        std::mem::take(&mut self.pending_serial)
    }

    /// Convert one scheduler batch into bounded, owned serial intents.
    ///
    /// Score-controlled port names and message bytes are measured before they
    /// are retained, matching the MIDI/OSC per-pass ceilings so a dense
    /// `.serial()` pattern cannot grow the producer queue without bound.
    #[cfg(feature = "serial")]
    fn collect_pending_serial(&mut self, onsets: &[OnsetEventJson], now: f64) {
        let mut pending = Vec::new();
        let mut routed = 0usize;
        let mut retained_bytes = 0usize;
        let mut output_limited = false;

        for onset in onsets {
            if crate::serial_bridge::serial_route(onset).is_none() {
                continue;
            }
            routed = routed.saturating_add(1);
            if routed > MAX_SERIAL_ROUTE_ATTEMPTS_PER_PASS {
                output_limited = true;
                break;
            }
            let Some(intent) = crate::serial_bridge::serial_onset(onset) else {
                continue;
            };
            let owned_bytes = intent.port.len().saturating_add(intent.bytes.len());
            let next_bytes = match retained_bytes.checked_add(owned_bytes) {
                Some(next) => next,
                None => {
                    output_limited = true;
                    break;
                }
            };
            if pending.len() >= MAX_SERIAL_INTENTS_PER_PASS
                || next_bytes > MAX_SERIAL_BYTES_PER_PASS
            {
                output_limited = true;
                break;
            }
            retained_bytes = next_bytes;
            pending.push((intent.target_time - now, intent));
        }
        self.pending_serial = pending;

        if output_limited {
            let message = format!(
                "serial output exceeded the per-pass limit of {MAX_SERIAL_ROUTE_ATTEMPTS_PER_PASS} routed onsets, {MAX_SERIAL_INTENTS_PER_PASS} intents, or {MAX_SERIAL_BYTES_PER_PASS} retained bytes"
            );
            self.report_diagnostic(
                "serial-refused",
                message.clone(),
                serde_json::json!({ "serial_refused": { "message": message } }),
            );
        }
    }

    /// The MIDI-input tables this Session's scores have named with `midin()`.
    ///
    /// The live loop attaches platform listeners to these; `render` and `query`
    /// never do, so an offline bounce stays reproducible and never grabs a
    /// musician's controller out from under a live set.
    ///
    /// Not feature-gated: the tables are dependency-free atomics, and a build
    /// without the `midi` feature simply leaves them at zero.
    pub fn midi_input_bus(&self) -> std::sync::Arc<rustel_core::midi_in::InputBus> {
        self.js.midi_input_bus()
    }

    /// Take the MIDI intents from the last scheduling pass.
    ///
    /// Each carries its `target_time` on the session clock, which the live loop
    /// maps to a wall-clock instant through one stable anchor
    /// (`midi_bridge::MidiClock`) rather than per pass.
    #[cfg(feature = "midi")]
    pub fn take_pending_midi(&mut self) -> Vec<crate::midi_bridge::MidiOnset> {
        std::mem::take(&mut self.pending_midi)
    }

    /// Convert one scheduler batch into bounded, owned MIDI intents.
    ///
    /// Route discovery deliberately happens before cloning score-controlled
    /// strings. A score can produce arbitrarily large values, while this queue
    /// lives outside the JavaScript watchdog on the live producer thread.
    #[cfg(feature = "midi")]
    fn collect_pending_midi(&mut self, onsets: &[OnsetEventJson]) {
        let mut pending = Vec::new();
        let mut routed = 0usize;
        let mut messages = 0usize;
        let mut retained_bytes = 0usize;
        let mut oversized_route = false;
        let mut output_limited = false;

        // `midi_onset_with_port` resolves midimap registrations, which belong
        // to this Session's detached runtime settings rather than process
        // globals. Keep the entire conversion pass inside that binding.
        self.js.with_runtime_settings(|| {
            for onset in onsets {
                let Some(port) = crate::midi_bridge::midi_route(onset) else {
                    continue;
                };
                routed = routed.saturating_add(1);
                if routed > MAX_MIDI_ROUTE_ATTEMPTS_PER_PASS {
                    output_limited = true;
                    break;
                }
                if port.len() > crate::midi_bridge::MAX_MIDI_PORT_NAME_BYTES {
                    oversized_route = true;
                    continue;
                }

                let Some(intent) =
                    crate::midi_bridge::midi_onset_with_port(onset, port.into_owned())
                else {
                    continue;
                };
                let planned = rustel_midi::plan(&intent.controls, intent.duration_secs);
                if planned.is_empty() {
                    continue;
                }

                let next_messages = match messages.checked_add(planned.len()) {
                    Some(next) => next,
                    None => {
                        output_limited = true;
                        break;
                    }
                };
                // The only score-sized allocation retained by an intent is
                // its route. Known MIDI commands are copied from fixed tiny
                // literals and registered maps are independently bounded.
                let owned_bytes = intent.port.len();
                let next_bytes = match retained_bytes.checked_add(owned_bytes) {
                    Some(next) => next,
                    None => {
                        output_limited = true;
                        break;
                    }
                };
                if pending.len() >= MAX_MIDI_INTENTS_PER_PASS
                    || next_messages > MAX_MIDI_MESSAGES_PER_PASS
                    || next_bytes > MAX_MIDI_RETAINED_BYTES_PER_PASS
                {
                    output_limited = true;
                    break;
                }
                messages = next_messages;
                retained_bytes = next_bytes;
                pending.push(intent);
            }
        });
        self.pending_midi = pending;

        if oversized_route && !self.midi_oversized_route_reported {
            self.midi_oversized_route_reported = true;
            let message = format!(
                "MIDI output port names must be at most {} bytes",
                crate::midi_bridge::MAX_MIDI_PORT_NAME_BYTES
            );
            self.report_diagnostic(
                "midi-refused",
                message.clone(),
                serde_json::json!({ "midi_refused": { "message": message } }),
            );
        }
        if output_limited && !self.midi_output_limit_reported {
            self.midi_output_limit_reported = true;
            let message = format!(
                "MIDI output exceeded the per-pass limit of {MAX_MIDI_ROUTE_ATTEMPTS_PER_PASS} routed onsets, {MAX_MIDI_INTENTS_PER_PASS} intents, {MAX_MIDI_MESSAGES_PER_PASS} wire messages, or {MAX_MIDI_RETAINED_BYTES_PER_PASS} retained bytes"
            );
            self.report_diagnostic(
                "midi-refused",
                message.clone(),
                serde_json::json!({ "midi_refused": { "message": message } }),
            );
        }
    }

    /// Take the OSC intents from the last scheduling pass, each paired with
    /// its lead in seconds ahead of the scheduled `now`. A live host tells
    /// the session of each bundle it sends, with `note_osc_handed_out`.
    #[cfg(feature = "osc")]
    pub fn take_pending_osc(&mut self) -> Vec<(f64, crate::osc_bridge::OscOnset)> {
        std::mem::take(&mut self.pending_osc)
    }

    #[cfg(feature = "osc")]
    fn collect_pending_osc(&mut self, onsets: &[OnsetEventJson], now: f64, cps: f64) {
        self.pending_osc.clear();
        let mut refused = Vec::new();
        let mut encoded_bytes = 0usize;
        let mut route_attempts = 0usize;
        let mut output_limited = false;
        for onset in onsets {
            let Some((host, port)) = crate::osc_bridge::osc_route(onset) else {
                continue;
            };
            route_attempts += 1;
            if route_attempts > MAX_OSC_ROUTE_ATTEMPTS_PER_PASS {
                output_limited = true;
                break;
            }
            let refusal_key = osc_refusal_key(host, port);
            if self.reported_osc_refusals.contains(&refusal_key) {
                continue;
            }
            match self.config.score_osc_access.approve(host, port) {
                Ok(destination) => {
                    // Only an approved route may cause score-controlled values
                    // to be cloned or JSON-serialized.
                    let Some(mut intent) = crate::osc_bridge::osc_onset(onset, cps) else {
                        // One malformed or individually oversized onset is
                        // silent, just like an unsupported OSC control. It
                        // must not suppress independent valid onsets later in
                        // the same scheduler batch or masquerade as aggregate
                        // producer pressure.
                        continue;
                    };
                    if self.pending_osc.len() == MAX_OSC_INTENTS_PER_PASS {
                        output_limited = true;
                        break;
                    }
                    let Some(next_bytes) = encoded_bytes.checked_add(intent.encoded_bytes) else {
                        output_limited = true;
                        break;
                    };
                    if next_bytes > MAX_OSC_BYTES_PER_PASS {
                        output_limited = true;
                        break;
                    }
                    encoded_bytes = next_bytes;
                    intent.destination = Some(destination);
                    self.pending_osc.push((intent.target_time - now, intent));
                }
                Err(message) => {
                    if self.reported_osc_refusals.len() < MAX_PENDING_SESSION_DIAGNOSTICS {
                        self.reported_osc_refusals.push(refusal_key);
                        refused.push(message);
                    }
                }
            }
        }
        if output_limited && !self.osc_output_limit_reported {
            self.osc_output_limit_reported = true;
            refused.push(format!(
                "OSC output exceeded the per-pass limit of {MAX_OSC_ROUTE_ATTEMPTS_PER_PASS} routed onsets, {MAX_OSC_INTENTS_PER_PASS} packets, or {MAX_OSC_BYTES_PER_PASS} encoded bytes"
            ));
        }
        for message in refused {
            self.report_diagnostic(
                "osc-refused",
                message.clone(),
                serde_json::json!({ "osc_refused": { "message": message } }),
            );
        }
    }

    #[cfg(feature = "device-audio")]
    fn has_pending_external_output(&self) -> bool {
        #[cfg(feature = "serial")]
        if !self.pending_serial.is_empty() {
            return true;
        }
        #[cfg(feature = "midi")]
        if self
            .pending_midi
            .iter()
            .any(|intent| rustel_midi::has_output(&intent.controls))
        {
            return true;
        }
        #[cfg(feature = "osc")]
        if !self.pending_osc.is_empty() {
            return true;
        }
        false
    }

    /// Accepted intents from this pass, not route names or stale queue totals.
    /// The underlying MIDI/OSC/serial collectors already bound these vectors.
    /// Sorting once keeps prefill accounting linearithmic even for dense mixed
    /// audio/external windows; steady conversion does not build this index.
    #[cfg(feature = "device-audio")]
    fn pending_external_onset_keys(&self) -> Vec<(u64, u64)> {
        #[allow(unused_mut)]
        let mut keys = Vec::new();
        #[cfg(feature = "midi")]
        keys.extend(
            self.pending_midi
                .iter()
                .filter(|intent| rustel_midi::has_output(&intent.controls))
                .map(|intent| (intent.generation, intent.onset_id)),
        );
        #[cfg(feature = "osc")]
        keys.extend(
            self.pending_osc
                .iter()
                .map(|(_, intent)| (intent.generation, intent.onset_id)),
        );
        #[cfg(feature = "serial")]
        keys.extend(
            self.pending_serial
                .iter()
                .map(|(_, intent)| (intent.generation, intent.onset_id)),
        );
        keys.sort_unstable();
        keys.dedup();
        keys
    }

    /// Live-device-only scheduler query boundary.
    ///
    /// Steady impure generations derive their QuickJS elapsed-time slice from
    /// the horizon actually left at `now`. The first fill cannot use that
    /// calculation because a newly anchored generation has zero queried
    /// horizon; initial startup retains the fixed ceiling, while a replacement
    /// receives a capped share of the wall time represented by one query.
    #[cfg(feature = "device-audio")]
    pub(crate) fn schedule_audio_live_at(
        &mut self,
        now: f64,
        sample_rate: u32,
        continuation_reserve: Duration,
        mode: LiveQueryBudgetMode,
    ) -> Result<LiveAudioBatch, LiveAudioScheduleError> {
        self.schedule_audio_live_with_cover_at(
            now,
            sample_rate,
            continuation_reserve,
            mode,
            self.live_producer_schedule_cover(),
        )
    }

    #[cfg(feature = "device-audio")]
    pub(crate) fn schedule_audio_reload_shield_at(
        &mut self,
        now: f64,
        sample_rate: u32,
        continuation_reserve: Duration,
    ) -> Result<LiveAudioBatch, LiveAudioScheduleError> {
        let cps = self.scheduler.cps();
        let extra = if cps.is_finite() && cps > 0.0 {
            (MAX_LIVE_IMPURE_QUERY_SPAN_CYCLES / cps)
                .min(LIVE_RELOAD_SHIELD_MAX_EXTRA.as_secs_f64())
        } else {
            0.0
        };
        self.schedule_audio_live_with_cover_at(
            now,
            sample_rate,
            continuation_reserve,
            LiveQueryBudgetMode::Steady,
            self.live_schedule_cover() + extra,
        )
    }

    #[cfg(feature = "device-audio")]
    fn schedule_audio_live_with_cover_at(
        &mut self,
        now: f64,
        sample_rate: u32,
        continuation_reserve: Duration,
        mode: LiveQueryBudgetMode,
        cover: f64,
    ) -> Result<LiveAudioBatch, LiveAudioScheduleError> {
        // The producer publishes a takeover at this same rate.
        self.live_sample_rate = (sample_rate > 0).then_some(sample_rate);
        self.with_panic_recovery(now, |session| {
            Ok(session.schedule_audio_live_with_cover_at_guarded(
                now,
                sample_rate,
                continuation_reserve,
                mode,
                cover,
            ))
        })
        .map_err(LiveAudioScheduleError::Retryable)?
    }

    #[cfg(feature = "device-audio")]
    fn schedule_audio_live_with_cover_at_guarded(
        &mut self,
        now: f64,
        sample_rate: u32,
        continuation_reserve: Duration,
        mode: LiveQueryBudgetMode,
        cover: f64,
    ) -> Result<LiveAudioBatch, LiveAudioScheduleError> {
        let mut record = ProducerTurnRecord {
            cover_start_nanos: crate::producer::seconds_nanos(
                self.scheduler.horizon_remaining(now),
            ),
            minimum_grid_nanos: crate::producer::seconds_nanos(
                self.scheduler.refill_floor_seconds(),
            ),
            continuation_reserve_nanos: crate::producer::duration_nanos(continuation_reserve),
            ..ProducerTurnRecord::default()
        };
        let mut profile = rustel_scheduler::SchedulerTickProfile::default();
        let result = self.schedule_audio_live_with_cover_at_inner(
            now,
            sample_rate,
            continuation_reserve,
            mode,
            cover,
            &mut record,
            &mut profile,
        );

        record.set_phase_nanos(ProducerPhase::Query, profile.query_nanos);
        record.set_phase_nanos(ProducerPhase::JsCallbacks, profile.callback_nanos);
        record.set_phase_nanos(ProducerPhase::Scheduler, profile.acceptance_nanos);
        record.query_span_millicycles = profile.query_span_millicycles;
        let cps = self.scheduler.cps();
        if cps.is_finite() && cps > 0.0 {
            record.query_span_nanos = crate::producer::seconds_nanos(
                profile.query_span_millicycles as f64 / 1000.0 / cps,
            );
        }
        record.haps = profile.queried_haps;
        record.scheduler_events = profile.accepted_events;
        record.js_callback_calls = profile.callback_calls;
        record.js_callback_kind_sample = profile.callback_kind_sample;
        record.queue_saturated |= profile.queue_full;
        let cover_after = self.scheduler.horizon_remaining(now);
        record.cover_end_nanos = crate::producer::seconds_nanos(cover_after);
        record.cover_gained_nanos = record
            .cover_end_nanos
            .saturating_sub(record.cover_start_nanos);
        record.outcome = match &result {
            Ok(_) if profile.horizon_full || record.cover_gained_nanos == 0 => {
                crate::ProducerTurnOutcome::Idle
            }
            Ok(_) => crate::ProducerTurnOutcome::Progress,
            Err(LiveAudioScheduleError::Retryable(RuntimeError::Cancelled)) => {
                crate::ProducerTurnOutcome::Cancelled
            }
            Err(LiveAudioScheduleError::Retryable(_))
            | Err(LiveAudioScheduleError::AwaitingSamples(_)) => {
                crate::ProducerTurnOutcome::AtomicRefusal
            }
            Err(
                LiveAudioScheduleError::CandidateCommitted(_)
                | LiveAudioScheduleError::CandidateRefusedScore(_),
            ) => crate::ProducerTurnOutcome::CommittedRefusal,
        };
        match record.outcome {
            crate::ProducerTurnOutcome::AtomicRefusal => record.atomic_refusal_count = 1,
            crate::ProducerTurnOutcome::CommittedRefusal => record.committed_refusal_count = 1,
            _ => {}
        }
        // A committed refusal that the score caused still latches, rolls
        // back and is reported. It is not the engine falling behind, so the
        // health readings count it separately.
        if result
            .as_ref()
            .err()
            .is_some_and(LiveAudioScheduleError::is_refused_score)
        {
            record.rejected_score_count = 1;
        }
        // Nor is a refusal that is the library still fetching the score's
        // samples: the engine turned nothing away, and the preview waiting
        // on it is not an overload.
        if result
            .as_ref()
            .err()
            .is_some_and(LiveAudioScheduleError::is_awaiting_samples)
        {
            record.loading_refusal_count = 1;
        }
        self.merge_producer_turn(record);
        result
    }

    #[cfg(feature = "device-audio")]
    #[allow(clippy::too_many_arguments)]
    fn schedule_audio_live_with_cover_at_inner(
        &mut self,
        now: f64,
        sample_rate: u32,
        continuation_reserve: Duration,
        mode: LiveQueryBudgetMode,
        cover: f64,
        record: &mut ProducerTurnRecord,
        profile: &mut rustel_scheduler::SchedulerTickProfile,
    ) -> Result<LiveAudioBatch, LiveAudioScheduleError> {
        if self.transport.is_stopped() {
            return Err(LiveAudioScheduleError::Retryable(RuntimeError::Cancelled));
        }

        let active_needs_host = self.active_needs_host();
        let base_cover = self.live_schedule_cover();
        if !cover.is_finite() || cover < base_cover {
            return Err(LiveAudioScheduleError::Retryable(RuntimeError::Message(
                format!(
                    "live schedule cover must be finite and at least {base_cover}, got {cover}"
                ),
            )));
        }
        // Everything still un-queried from before `now` is unplayable, and
        // querying it is what makes the next tick even more expensive. Drop it
        // and resume at the clock.
        //
        // Not while a rewind is unpublished: its cursor stands at cycle zero
        // on purpose, and skipping it would begin the restart bars in, or
        // publish an empty window while its samples load. The producer
        // slides such a cycle zero to its clock before it queries
        // (`slide_pending_rewind_anchor`), so this never has a gap to skip
        // there anyway; the guard keeps it that way for any caller that
        // does not.
        if self.requery_takeover_cut == TakeoverCut::None
            && let Some(dropped) = self.scheduler.skip_past_gap(now, base_cover)
        {
            record.gap_resync_count = record.gap_resync_count.saturating_add(1);
            let report = self
                .last_gap_log
                .is_none_or(|at| at.elapsed() >= Duration::from_secs(2));
            if report {
                self.last_gap_log = Some(Instant::now());
                let message = "the producer fell behind the audio clock; resumed at the present";
                self.report_diagnostic(
                    "live-recovered",
                    format!("{message} ({:.3}s skipped)", dropped),
                    serde_json::json!({
                        "live_recovered": {
                            "message": message,
                            "dropped_seconds": (dropped * 1000.0).round() / 1000.0,
                        }
                    }),
                );
            }
        }
        let impure_fill_needed = active_needs_host && self.scheduler.horizon_remaining(now) < cover;
        let budget = if impure_fill_needed {
            match self
                .live_query_budget_at(now, continuation_reserve, mode)
                .map_err(LiveAudioScheduleError::Retryable)?
            {
                Some(budget) => budget,
                None if self.transport.is_stopped() => {
                    return Err(LiveAudioScheduleError::Retryable(RuntimeError::Cancelled));
                }
                // DEADLOCK, not a safeguard: this reserve protects the buffered
                // horizon, and by the time it cannot be covered that horizon is
                // already gone. Refusing saves nothing and only a query can
                // refill it, so the refusal becomes self-perpetuating after a
                // long CPU stall. Run a recovery slice instead.
                None => LIVE_QUERY_RECOVERY_BUDGET,
            }
        } else if active_needs_host {
            // The shared helper still opens the bounded host scope before
            // Scheduler reports HorizonFull, so give that non-querying scope
            // a valid token. No callback/getter/materialisation route runs and
            // the unaffordable steady-state calculation is intentionally not
            // consulted.
            MIN_LIVE_QUERY_JS_BUDGET
        } else {
            // Pure graphs remain host-free and therefore need no QuickJS
            // token. Core cancellation still applies in the shared helper.
            Duration::ZERO
        };
        record.budget_granted_nanos = crate::producer::duration_nanos(budget);

        // QuickJS's interrupt handler bounds only a query that enters
        // JavaScript. With no other bound, the pure native route
        // (`active_needs_host == false`) runs `Scheduler::tick` with no
        // ceiling: `chop(32).slow(0.001)` can block the producer thread until
        // the ring drains. A wall-clock deadline over the whole tick turns a
        // too-heavy query into a refusal: the cursor holds, the watchdog
        // rolls back, the music continues. A nested deadline can only
        // tighten, so the JS route keeps its own tighter budget. The deadline
        // is the slice the query itself gets: one that expires before the
        // budget would refuse the recovery it bounds.
        let wall_ceiling = Instant::now()
            + self
                .live_query_budget_at(now, continuation_reserve, mode)
                .ok()
                .flatten()
                .unwrap_or(LIVE_QUERY_RECOVERY_BUDGET)
                .max(MIN_LIVE_QUERY_JS_BUDGET);
        let through = now + cover;
        self.scheduler.set_horizon(cover);
        let query = rustel_core::with_query_deadline(wall_ceiling, || {
            self.schedule_through_with_js_budget_and_tail_with_cover(
                now,
                through,
                budget,
                cover,
                Some(profile),
            )
        });
        self.scheduler.set_horizon(base_cover);
        let ScheduledWindow {
            events: onsets,
            tail_started,
            status,
            query_threw,
        } = query.map_err(|error| match error {
            ScheduleAttemptError::Atomic(error) => LiveAudioScheduleError::Retryable(error),
            ScheduleAttemptError::Partial(error) => {
                LiveAudioScheduleError::CandidateCommitted(error)
            }
        })?;
        // Emit `.log()` text for scheduled onsets. Formatting happens during
        // the pattern query, but emission waits until an onset is accepted
        // so re-querying a span cannot print the same onset twice.
        //
        // Live conversion does not call `schedule_audio_through`, so this
        // path must also drain the onset logs.
        // `logger(...)` and `console.log(...)` drain in the same pass.
        let mut logged: Vec<String> = self.js.take_logs();
        logged.extend(onsets.iter().filter_map(|onset| onset.log_line.clone()));
        for line in logged {
            self.report_diagnostic(
                "log",
                line.clone(),
                serde_json::json!({ "log": { "message": line } }),
            );
        }
        // Same skip-and-log contract as `schedule_audio_through`: the reference
        // drops an unrenderable voice and plays the rest.
        let asset_started = Instant::now();
        self.log_sample_failures();
        let library = self.samples.clone();
        let bundled = rustel_voice::BundledOnly;
        let lookup: &dyn rustel_voice::SampleLookup = match &library {
            Some(library) => library.as_ref(),
            None => &bundled,
        };
        record.add_phase(ProducerPhase::AssetPreparation, asset_started.elapsed());
        let mut refusals = Vec::new();

        #[cfg(feature = "serial")]
        {
            self.collect_pending_serial(&onsets, now);
        }
        // The LIVE path, which is the one a set actually runs: it does its own
        // conversion and never reaches `schedule_audio_through`, so collecting
        // only there meant a live set sent no MIDI at all.
        #[cfg(feature = "midi")]
        {
            self.collect_pending_midi(&onsets);
        }
        let cps = self.config.cps;

        // The LIVE path, which is the one a set actually runs: it does its own
        // conversion and never reaches `schedule_audio_through`, so collecting
        // only there meant a live set sent no OSC at all.
        #[cfg(feature = "osc")]
        self.collect_pending_osc(&onsets, now, cps);

        let external_keys = if matches!(mode, LiveQueryBudgetMode::Steady) {
            Vec::new()
        } else {
            self.pending_external_onset_keys()
        };
        let mut dispositions = confirmation::WindowDispositions {
            intended: u32::try_from(onsets.len()).map_err(|_| {
                LiveAudioScheduleError::CandidateCommitted(RuntimeError::ResourceLimit(
                    "audio confirmation onset count exceeded its bound".into(),
                ))
            })?,
            ..Default::default()
        };
        let mut sample_identities = Vec::new();
        let mut invalid_control = None;
        let mut last_refusal_loading = false;
        let mut input_warnings: Vec<String> = Vec::new();
        // The takeover frame for this window's latency-compensated events
        // (see `handover`): the device keeps the outgoing events aimed
        // before it. A generation not yet published uses its pending
        // takeover. A cut, or no takeover time, keeps nothing.
        let generation = self.generation();
        // A device that plays its first generation begins a timeline: no
        // earlier generation handed a message out on it.
        if matches!(mode, LiveQueryBudgetMode::InitialPrefill) {
            self.forget_handed_out();
        }
        let pending_takeover = match (self.requery_takeover_time, self.requery_takeover_cut) {
            (Some(time), TakeoverCut::None) => {
                Some(crate::render::takeover_frame_at(time, sample_rate))
            }
            _ => None,
        };
        let takeover_frame = self.compensated_onsets.begin_window(
            generation,
            mode,
            (now * f64::from(sample_rate)).floor().max(0.0) as u64,
            sample_rate,
            pending_takeover,
        );
        let mut compensated: Vec<handover::ConvertedOnset<'_>> = Vec::new();
        // The plain events, for a copy whose effect a save removed. Only a
        // window with a takeover frame can meet such a copy.
        let mut plain: Vec<handover::ConvertedOnset<'_>> = Vec::new();
        let conversion_started = Instant::now();
        let (mut events, notices) =
            rustel_voice::with_diagnostic_policy(self.direct_diagnostic_logging, || {
                onsets
                    .iter()
                    .filter_map(|onset| {
                        match crate::render::live_audio_event(onset, sample_rate, cps, lookup) {
                            Ok(event) => {
                                #[cfg(feature = "vst")]
                                let event = {
                                    let mut event = event;
                                    event.controls.insert_orbit = Some(
                                        self.insert_orbits[(event.controls.orbit as usize)
                                            .min(rustel_audio::MAX_ORBITS - 1)],
                                    );
                                    event
                                };
                                if let Some(warning) = self.input_channel_warning(&event)
                                    && !input_warnings.contains(&warning)
                                {
                                    input_warnings.push(warning);
                                }
                                // A latency compensation aimed the voice
                                // before the frame of its onset. A time no
                                // frame holds reads `u64::MAX`.
                                let onset_frame =
                                    crate::render::onset_frame_at(onset.target_time, sample_rate);
                                let converted = handover::ConvertedOnset {
                                    event: dispositions.converted as usize,
                                    whole_begin: &onset.whole_begin,
                                    value: &onset.value_show,
                                    controls: &onset.value,
                                    target_frame: event.target_frame,
                                    onset_frame,
                                };
                                if event.target_frame < onset_frame && onset_frame != u64::MAX {
                                    compensated.push(converted);
                                } else if event.target_frame == onset_frame
                                    && takeover_frame.is_some()
                                {
                                    plain.push(converted);
                                }
                                dispositions.converted += 1;
                                if !matches!(mode, LiveQueryBudgetMode::Steady) {
                                    // Match the scalar source-selection order;
                                    // wavetable bodies also come from SampleBank.
                                    let sample_id = if event.synth.is_some() {
                                        None
                                    } else {
                                        event
                                            .wavetable
                                            .map(|table| table.table)
                                            .or_else(|| event.sample.map(|sample| sample.sample))
                                    };
                                    sample_identities.push(sample_id.and_then(|id| {
                                        library
                                            .as_ref()
                                            .and_then(|library| library.decoded_identity(id))
                                            .or_else(|| {
                                                (id == rustel_audio::BUNDLED_BD_SAMPLE_ID)
                                                    .then_some(
                                                        rustel_audio::BUNDLED_BD_SAMPLE_IDENTITY,
                                                    )
                                            })
                                    }));
                                }
                                Some(event)
                            }
                            Err(error) => {
                                let message = error.to_string();
                                last_refusal_loading =
                                    matches!(error, rustel_voice::VoiceError::SampleLoading(_));
                                if external_keys
                                    .binary_search(&(onset.generation, onset.onset_id))
                                    .is_ok()
                                {
                                    dispositions.external += 1;
                                } else if last_refusal_loading {
                                    dispositions.skipped_loading += 1;
                                } else {
                                    dispositions.refused += 1;
                                    if invalid_control.is_none()
                                        && matches!(mode, LiveQueryBudgetMode::ReplacementPrefill)
                                        && matches!(
                                            error,
                                            rustel_voice::VoiceError::InvalidControl(_)
                                        )
                                    {
                                        invalid_control = Some(message.clone());
                                    }
                                }
                                // A run of the same refusal is one report:
                                // `s("nosuchsound*16")` is one mistake, not
                                // sixteen records a window on the command
                                // line's direct log. What decides a hold
                                // downstream is `last_refusal_loading` and
                                // the dispositions, never this list.
                                //
                                // Each refusal carries whether it was the
                                // library still loading, so the report below
                                // can tell waiting from wrong without reading
                                // the prose back.
                                if refusals.last().map(|(_, seen)| seen) != Some(&message) {
                                    refusals.push((last_refusal_loading, message));
                                }
                                None
                            }
                        }
                    })
                    .collect::<Vec<_>>()
            });
        // Leave out each compensated event whose onset has an outgoing copy
        // on the device: both copies would play, and the outgoing copy can
        // already sound. A left-out event leaves the converted and the
        // intended counts: like an onset before the takeover, it is not this
        // window's to play. Its MIDI intent stays, because MIDI hands over
        // on the frame of the onset. Its OSC and serial intents follow their
        // own frontier (`leave_out_handed_out_intents`).
        let left_out = match takeover_frame {
            Some(takeover_frame) if !compensated.is_empty() => self
                .compensated_onsets
                .claim_outgoing_copies(generation, takeover_frame, &compensated),
            _ => Vec::new(),
        };
        let mut left_out_events = Vec::new();
        if left_out.contains(&true) {
            let mut left_out = left_out.into_iter();
            compensated.retain(|converted| {
                let stays = !left_out.next().unwrap_or(false);
                if !stays {
                    left_out_events.push(converted.event);
                }
                stays
            });
        }
        // A copy that remains can stand for a plain event: a save removed
        // the effect from its onset. That event is left out the same way.
        if let Some(takeover_frame) = takeover_frame
            && !plain.is_empty()
        {
            let left_out = self.compensated_onsets.claim_outgoing_copies_for_plain(
                generation,
                takeover_frame,
                &plain,
            );
            let plain_events = plain.iter().zip(left_out).filter(|(_, left_out)| *left_out);
            left_out_events.extend(plain_events.map(|(converted, _)| converted.event));
            left_out_events.sort_unstable();
        }
        if !left_out_events.is_empty() {
            handover::remove_ascending(&mut events, &left_out_events);
            if !sample_identities.is_empty() {
                handover::remove_ascending(&mut sample_identities, &left_out_events);
            }
            let count = u32::try_from(left_out_events.len()).unwrap_or(u32::MAX);
            dispositions.converted = dispositions.converted.saturating_sub(count);
            dispositions.intended = dispositions.intended.saturating_sub(count);
        }
        record.add_phase(ProducerPhase::Conversion, conversion_started.elapsed());
        self.report_voice_notices(notices);
        record.converted_audio_events = u64::try_from(events.len()).unwrap_or(u64::MAX);
        record.refused_voices = u64::try_from(onsets.len() - left_out_events.len())
            .unwrap_or(u64::MAX)
            .saturating_sub(record.converted_audio_events);
        let refused = refusals.last().map(|(_, message)| message.clone());
        // A replacement still waiting to publish re-converts the same first
        // window every retry (a rewind's held window is reopened and asked
        // again), so its refusals are said ONCE per install - loading or
        // not - or a held rewind logs the same refusal many times a second.
        let held_replacement = matches!(mode, LiveQueryBudgetMode::ReplacementPrefill);
        // A rewind whose whole first window waits for its samples is
        // reopened and heard whole once they are in, so its loading
        // refusals are notes awaited, not skipped.
        let window_awaited = held_replacement
            && self.requery_takeover_cut != TakeoverCut::None
            && !onsets.is_empty()
            && events.is_empty()
            && !self.has_pending_external_output()
            && last_refusal_loading;
        for (loading, message) in refusals {
            let kind = match (loading, window_awaited) {
                (true, true) => SAMPLE_AWAITED_DIAGNOSTIC,
                (true, false) => SAMPLE_LOADING_DIAGNOSTIC,
                (false, _) => "voice-refused",
            };
            // Once playing, loading refusals are still said once, which is
            // how the studio gathers the sounds a load skipped into one
            // line after it and the command line says it is waiting; any
            // other refusal reports every window: a repeated genuine
            // refusal of a sounding score IS news.
            if !(held_replacement || loading)
                || self
                    .loading_refusals_reported
                    .insert((kind, message.clone()))
            {
                if loading {
                    self.report_diagnostic(
                        kind,
                        message.clone(),
                        serde_json::json!({ "sample_loading": { "message": message } }),
                    );
                } else {
                    self.report_diagnostic(
                        "voice-refused",
                        message.clone(),
                        serde_json::json!({ "voice_refused": { "message": message } }),
                    );
                }
            }
        }
        for message in input_warnings {
            if self.input_warnings_reported.insert(message.clone()) {
                self.report_diagnostic(
                    "audio-input",
                    message.clone(),
                    serde_json::json!({ "audio_input": { "message": message } }),
                );
            }
        }
        // Invalid controls reject the whole replacement before any sibling
        // reaches the output. Asset skips and external-only onsets keep their
        // separate policies; their diagnostic text is not a failure class.
        if matches!(mode, LiveQueryBudgetMode::ReplacementPrefill)
            && let Some(message) = invalid_control
        {
            let scope = if events.is_empty() && !self.has_pending_external_output() {
                "every onset of"
            } else {
                "a voice in"
            };
            // These intents were collected from this same rejected window.
            // They must not survive for an external host to drain after the
            // audio producer returns its refusal.
            #[cfg(feature = "midi")]
            self.pending_midi.clear();
            #[cfg(feature = "osc")]
            self.pending_osc.clear();
            #[cfg(feature = "serial")]
            self.pending_serial.clear();
            return Err(LiveAudioScheduleError::CandidateRefusedScore(
                RuntimeError::Message(format!(
                    "scalar audio refused {scope} the replacement's first window ({message})"
                )),
            ));
        }
        // In steady state an unrenderable voice is skipped and logged, and
        // the rest plays. A replacement whose first window refuses every
        // onset is different: publishing it would swap a sounding score for
        // one that no compiled output can render. A MIDI, OSC or serial
        // intent is a valid external-only replacement even when native scalar
        // conversion refuses it. The scheduler has already consumed the
        // window, so an unroutable window is a committed candidate: the
        // producer latches it until a new score identity supersedes it.
        //
        // A sample that is still loading does not latch, because the same
        // identity can continue from later scheduling. A first window that
        // falls where the score's sounds are missing does not latch either:
        // `s("ht").bank("BossDR110 AkaiXR10")` has toms only in its second
        // half. The refusal therefore looks a few cycles ahead of `now`.
        //
        //   every onset refused, no external output pending
        //     last refusal is "still loading" -> AwaitingSamples (retry)
        //     not a replacement prefill       -> Ok: skip and log
        //     replacement prefill, look ahead:
        //       Renders                       -> Ok: publish, skip and log
        //       Loading                       -> AwaitingSamples (retry)
        //       InvalidControl or Nothing     -> CandidateRefusedScore
        if !onsets.is_empty()
            && events.is_empty()
            && !self.has_pending_external_output()
            && let Some(message) = &refused
        {
            if last_refusal_loading {
                return Err(LiveAudioScheduleError::AwaitingSamples(
                    RuntimeError::Message(format!(
                        "scalar audio still loading samples ({message})"
                    )),
                ));
            }
            if matches!(mode, LiveQueryBudgetMode::ReplacementPrefill) {
                match self.replacement_renders_ahead(now, sample_rate, lookup) {
                    ReplacementLookAhead::Renders => {}
                    ReplacementLookAhead::Loading(loading) => {
                        // The window itself refused nothing as loading, so
                        // nothing above said it: say the sample ahead once,
                        // as awaited, since the update waits for it rather
                        // than skipping its notes.
                        if self
                            .loading_refusals_reported
                            .insert((SAMPLE_AWAITED_DIAGNOSTIC, loading.clone()))
                        {
                            self.report_diagnostic(
                                SAMPLE_AWAITED_DIAGNOSTIC,
                                loading.clone(),
                                serde_json::json!({ "sample_loading": { "message": loading } }),
                            );
                        }
                        return Err(LiveAudioScheduleError::AwaitingSamples(
                            RuntimeError::Message(format!(
                                "scalar audio still loading samples ({loading})"
                            )),
                        ));
                    }
                    ReplacementLookAhead::InvalidControl(message) => {
                        return Err(LiveAudioScheduleError::CandidateRefusedScore(
                            RuntimeError::Message(format!(
                                "scalar audio refused a voice ahead of the \
                                 replacement's first window ({message})"
                            )),
                        ));
                    }
                    ReplacementLookAhead::Nothing => {
                        return Err(LiveAudioScheduleError::CandidateRefusedScore(
                            RuntimeError::Message(format!(
                                "scalar audio refused every onset of the \
                                 replacement's first window ({message})"
                            )),
                        ));
                    }
                }
            }
        }
        // The window goes to the producer: its compensated events are the
        // device's to hold once it publishes them.
        self.compensated_onsets
            .record(generation, mode, &compensated);
        let batch = LiveAudioBatch {
            prefill_progress: status == rustel_scheduler::TickStatus::Filled
                || !events.is_empty()
                || self.has_pending_external_output(),
            events,
            dispositions,
            sample_identities,
            query_threw,
            queried_through_frame: ((now + self.scheduler.horizon_remaining(now)).min(through)
                * f64::from(sample_rate))
            .ceil()
            .max(0.0) as u64,
            tail_started,
        };
        // Last, so the window is judged with every intent it staged: an
        // onset whose message is already out is still an external onset.
        #[cfg(any(feature = "osc", feature = "serial"))]
        self.leave_out_handed_out_intents(sample_rate, &onsets);
        Ok(batch)
    }

    /// Microbench query throughput for the active pattern.
    pub fn bench(
        &self,
        begin: Fraction,
        end: Fraction,
        iterations: u64,
    ) -> Result<BenchMetrics, RuntimeError> {
        if iterations == 0 {
            return Err(RuntimeError::Message(
                "bench iterations must be greater than 0".into(),
            ));
        }
        // Warm once so first-eval noise is outside the timed loop.
        let _ = self.query(begin, end)?;
        let start = Instant::now();
        let mut total_haps = 0u64;
        for _ in 0..iterations {
            // Checked between iterations as well as inside each query: a bench
            // of a cheap pattern is a tight loop where no single query is long
            // enough to notice a stop.
            if self.transport.is_stopped() {
                return Err(RuntimeError::Cancelled);
            }
            total_haps += self.query(begin, end)?.len() as u64;
        }
        let elapsed = start.elapsed().as_secs_f64().max(f64::EPSILON);
        Ok(BenchMetrics {
            source: self.last_source.as_deref().unwrap_or_default().to_owned(),
            begin: fraction_label(begin),
            end: fraction_label(end),
            iterations,
            total_haps,
            haps_per_iteration: total_haps as f64 / iterations as f64,
            elapsed_secs: elapsed,
            queries_per_sec: iterations as f64 / elapsed,
            haps_per_sec: total_haps as f64 / elapsed,
        })
    }
}

/// Pull a mini string out of common CLI forms when JS evaluation fails.
fn extract_mini_fallback(source: &str) -> Option<String> {
    let trimmed = source.trim().trim_end_matches(';').trim();
    // s("bd sd") / s('bd sd') / mini("...")
    for name in ["s", "mini", "m"] {
        let prefix = format!("{name}(");
        if let Some(rest) = trimmed.strip_prefix(&prefix)
            && let Some(inner) = strip_quoted_arg(rest)
        {
            return Some(inner);
        }
    }
    // Bare quoted mini: "bd sd"
    if (trimmed.starts_with('"') && trimmed.ends_with('"') && trimmed.len() >= 2)
        || (trimmed.starts_with('\'') && trimmed.ends_with('\'') && trimmed.len() >= 2)
    {
        return Some(trimmed[1..trimmed.len() - 1].to_string());
    }
    // Bare unquoted token sequence: `bd sd` - never JavaScript.
    if !trimmed.contains('(') && !trimmed.contains('{') && !trimmed.contains('=') {
        return Some(trimmed.to_string());
    }
    None
}

fn strip_quoted_arg(rest: &str) -> Option<String> {
    let rest = rest.trim();
    let quote = rest.chars().next()?;
    if quote != '"' && quote != '\'' {
        return None;
    }
    let body = rest[1..].strip_suffix(')')?.trim_end();
    let body = body.strip_suffix(quote)?;
    Some(body.to_string())
}

#[cfg(test)]
mod playback_reanchor_tests {
    use super::*;
    use rustel_core::{Value, fastcat, pure};

    fn native_session() -> Session {
        let mut session = Session::with_config(SessionConfig {
            cps: 1.0,
            horizon: 0.25,
            sample_rate: 48_000,
            channels: 2,
            ..Default::default()
        })
        .expect("session");
        let notes = [60.0, 72.0].into_iter().map(|note| {
            pure(Value::object(vec![
                ("s".into(), Value::Str("sine".into())),
                ("note".into(), Value::F64(note)),
                ("gain".into(), Value::F64(0.25)),
            ]))
        });
        session
            .set_pattern(fastcat(notes.collect()))
            .expect("native pattern");
        assert!(!session.active_needs_host());
        session
    }

    #[test]
    fn repeated_play_restarts_at_zero_without_reusing_onset_ids() {
        let mut session = native_session();
        let generation = session.generation();
        let fresh = native_session().play(0.625).expect("fresh play");
        let first = session.play(0.625).expect("first play");
        assert!(
            session.scheduler.scheduled_to_cycle().fract() > 0.0,
            "the first pass must leave a fractional query cursor"
        );
        let repeated = session.play(0.625).expect("repeated play");
        let semantics = |report: &PlayReport| {
            report
                .onsets
                .iter()
                .map(|onset| {
                    (
                        onset.whole_begin.clone(),
                        onset.target_time,
                        onset.duration_secs,
                        onset.value.clone(),
                        onset.ui_visuals,
                    )
                })
                .collect::<Vec<_>>()
        };
        assert_eq!(fresh.onsets.len(), 2);
        assert_eq!(fresh.onsets[0].target_time, 0.0);
        assert_eq!(fresh.onsets[1].target_time, 0.5);
        assert_eq!(semantics(&first), semantics(&fresh));
        assert_eq!(semantics(&repeated), semantics(&fresh));
        assert_eq!(first.generation, generation + 1);
        assert_eq!(repeated.generation, first.generation + 1);
        assert!(repeated.onsets[0].onset_id > first.onsets.last().unwrap().onset_id);
        assert!(
            repeated
                .onsets
                .iter()
                .all(|onset| onset.generation == repeated.generation)
        );
        assert_eq!(session.time_at_cycle(Fraction::ZERO), 0.0);
    }

    #[test]
    fn repeated_pcm_render_restarts_after_an_intervening_play() {
        let expected = native_session().render_pcm(0.125).expect("fresh PCM");
        assert_eq!(expected.len(), 12_000);
        assert!(expected.iter().all(|sample| sample.is_finite()));
        assert!(expected.iter().any(|sample| sample.abs() > 1e-6));
        let mut session = native_session();
        let first = session.render_pcm(0.125).expect("first PCM");
        session.play(0.625).expect("intervening play");
        let repeated = session.render_pcm(0.125).expect("repeated PCM");
        for actual in [&first, &repeated] {
            assert_eq!(actual.len(), expected.len());
            for (index, (actual, expected)) in actual.iter().zip(&expected).enumerate() {
                assert!(actual.is_finite());
                assert_eq!(actual.to_bits(), expected.to_bits(), "sample {index}");
            }
        }
    }

    #[test]
    fn non_reanchored_play_window_preserves_the_installed_timeline() {
        let mut session = native_session();
        session.scheduler.rebase_anchor(10.0, 0.25);
        let generation = session.generation();
        let report = session
            .play_window(10.0, 0.3, QUERY_JS_CPU_BUDGET, false)
            .expect("continuous window");
        assert_eq!(report.generation, generation);
        assert_eq!(session.generation(), generation);
        assert_eq!(session.time_at_cycle(Fraction::ZERO), 9.75);
        assert_eq!(report.onsets.len(), 1);
        let onset = &report.onsets[0];
        assert_eq!(onset.whole_begin, Fraction::new(1, 2).show());
        assert_eq!(onset.target_time, 10.25);
        assert_eq!(onset.duration_secs, 0.5);
        assert_eq!(
            onset.value,
            ValueJson::Raw(serde_json::json!({
                "s": "sine", "note": 72, "gain": 0.25,
            }))
        );
    }
}

#[cfg(test)]
mod tests {
    /// `setGainCurve` shapes every gain, the default 0.8 included, as
    /// superdough does.
    #[test]
    fn the_gain_curve_shapes_what_plays() {
        let peak_of = |score: &str| {
            let mut session = Session::with_config(SessionConfig::default()).expect("session");
            session.evaluate(score).expect("evaluate");
            let pcm = session.render_pcm(1.0).expect("render");
            pcm.iter().fold(0f32, |peak, sample| peak.max(sample.abs()))
        };
        let straight = peak_of("$: s(\"sine\").gain(.5)");
        let squared = peak_of("setGainCurve(x => x * x)\n$: s(\"sine\").gain(.5)");
        assert!(
            (squared / straight - 0.5).abs() < 0.05,
            "gain .5 through x² is half as loud: {straight} -> {squared}"
        );
        let default_straight = peak_of("$: s(\"sine\")");
        let default_squared = peak_of("setGainCurve(x => x * x)\n$: s(\"sine\")");
        assert!(
            (default_squared / default_straight - 0.8).abs() < 0.05,
            "the default 0.8 goes through the curve too: {default_straight} -> {default_squared}"
        );
        let identity = peak_of("setGainCurve(x => x)\n$: s(\"sine\").gain(.5)");
        assert!(
            (identity - straight).abs() < 1e-3,
            "an identity curve changes nothing"
        );
    }

    /// The `$:` form registers into a stack rather than returning a pattern,
    /// so commenting the only `$:` line out is a different path from
    /// commenting out a bare expression.
    #[test]
    fn commenting_out_the_only_dollar_line_goes_silent() {
        let mut session = Session::with_config(SessionConfig::default()).expect("session");
        session.evaluate("$: s('bd*4')").expect("initial evaluate");
        let playing = session
            .query(Fraction::new(0, 1), Fraction::new(1, 1))
            .expect("query");
        assert!(!playing.is_empty(), "the set never started");

        session
            .evaluate("// $: s('bd*4')")
            .expect("a commented-out score is accepted");
        let muted = session
            .query(Fraction::new(0, 1), Fraction::new(1, 1))
            .expect("query");
        assert!(
            muted.is_empty(),
            "commenting the only $: line left {} events playing",
            muted.len()
        );
    }

    /// An evaluate over a running transport retires the key backlog that the
    /// outgoing graph placed. Without the flush, the incoming generation
    /// re-renders up to two seconds of pinned note-ons.
    #[test]
    fn an_evaluate_over_a_running_transport_flushes_the_pinned_key_backlog() {
        let mut session = Session::new().expect("session");
        session
            .evaluate(r#"const kb = await midikeys('keyboard'); kb(0.5).s('piano')"#)
            .expect("first score");
        session.transport().start();

        // Spam the pads: a burst of note-ons the first score's queries pin
        // at future cycle positions.
        let port = session
            .midi_input_bus()
            .find("keyboard")
            .expect("keyboard port");
        for _ in 0..32 {
            port.observe_note_on(rustel_core::midi_in::now_nanos(), 1, 60, 100);
        }
        let mut placed = Vec::new();
        port.keys.select(
            0.0,
            1.0,
            rustel_core::midi_in::now_nanos(),
            Some((0, 1)),
            &mut placed,
        );
        assert_eq!(
            placed.len(),
            32,
            "the burst must be placeable before the evaluate"
        );

        // A live editor save: a different score over the running transport,
        // no restart - the path the studio actually sends.
        let stopped = std::sync::atomic::AtomicBool::new(false);
        session
            .reload_at_cancellable(
                r#"const kb = await midikeys('keyboard'); kb(0.5).s('sd:4')"#,
                false,
                0.5,
                &stopped,
            )
            .expect("live replacement");

        // The presses belonged to the score that was sounding when they
        // landed. The new generation starts with an empty key memory: what
        // the old graph already scheduled keeps sounding, but the unrendered
        // backlog must not re-render through the new voices.
        let port = session
            .midi_input_bus()
            .find("keyboard")
            .expect("keyboard port survives the reload");
        let mut after = Vec::new();
        port.keys.select(
            0.0,
            1.0,
            rustel_core::midi_in::now_nanos(),
            Some((0, 1)),
            &mut after,
        );
        assert!(
            after.is_empty(),
            "the evaluate flushed the key memory, not carried the backlog across: {} hits",
            after.len()
        );
    }

    /// A press that lands after the outgoing graph's last query, while an
    /// evaluate is in flight, is not placed. It belongs to the incoming
    /// score and survives the backlog flush.
    #[test]
    fn an_evaluate_over_a_running_transport_keeps_the_press_nobody_placed() {
        let mut session = Session::new().expect("session");
        session
            .evaluate(r#"const kb = await midikeys('keyboard'); kb(0.5).s('piano')"#)
            .expect("first score");
        session.transport().start();
        let port = session
            .midi_input_bus()
            .find("keyboard")
            .expect("keyboard port");
        for _ in 0..32 {
            port.observe_note_on(rustel_core::midi_in::now_nanos(), 1, 60, 100);
        }
        let mut placed = Vec::new();
        port.keys.select(
            0.0,
            1.0,
            rustel_core::midi_in::now_nanos(),
            Some((0, 1)),
            &mut placed,
        );
        assert_eq!(placed.len(), 32, "the outgoing score placed its backlog");
        // Played during the evaluate: no query has seen it.
        port.observe_note_on(rustel_core::midi_in::now_nanos(), 1, 67, 90);

        let stopped = std::sync::atomic::AtomicBool::new(false);
        session
            .reload_at_cancellable(
                r#"const kb = await midikeys('keyboard'); kb(0.5).s('sd:4')"#,
                false,
                0.5,
                &stopped,
            )
            .expect("live replacement");

        let port = session
            .midi_input_bus()
            .find("keyboard")
            .expect("keyboard port survives the reload");
        let mut after = Vec::new();
        port.keys.select(
            0.0,
            1.0,
            rustel_core::midi_in::now_nanos(),
            Some((1, 2)),
            &mut after,
        );
        assert_eq!(
            after
                .iter()
                .map(|hit| (hit.note, hit.velocity))
                .collect::<Vec<_>>(),
            [(67, 90)],
            "exactly the unplaced press is heard by the new score"
        );
    }

    /// The mini install door (`install_pattern_at`, which a mini reload and
    /// `set_pattern` share) keeps the same boundary as the JavaScript one:
    /// the pinned backlog is retired and the press nobody placed survives
    /// for the score taking over.
    #[test]
    fn a_mini_reload_over_a_running_transport_keeps_the_press_nobody_placed() {
        let mut session = Session::new().expect("session");
        session
            .evaluate(r#"const kb = await midikeys('keyboard'); kb(0.5).s('piano')"#)
            .expect("first score");
        session.transport().start();
        // The MIDI listener reports the sounding generation, as it does
        // while a device plays: a mini score names no ports, and only the
        // audible generation keeps the keyboard's port on the bus through
        // the install.
        let generation = session.generation();
        session
            .midi_input_bus()
            .snapshot_for(generation, generation);
        let port = session
            .midi_input_bus()
            .find("keyboard")
            .expect("keyboard port");
        for _ in 0..32 {
            port.observe_note_on(rustel_core::midi_in::now_nanos(), 1, 60, 100);
        }
        let mut placed = Vec::new();
        port.keys.select(
            0.0,
            1.0,
            rustel_core::midi_in::now_nanos(),
            Some((0, 1)),
            &mut placed,
        );
        assert_eq!(placed.len(), 32, "the outgoing score placed its backlog");
        // Played during the evaluate: no query has seen it.
        port.observe_note_on(rustel_core::midi_in::now_nanos(), 1, 67, 90);

        let stopped = std::sync::atomic::AtomicBool::new(false);
        session
            .reload_at_cancellable("sd:4", true, 0.5, &stopped)
            .expect("live mini replacement");
        assert_eq!(
            session.generation(),
            generation + 1,
            "the mini score took over"
        );

        let mut after = Vec::new();
        port.keys.select(
            0.0,
            1.0,
            rustel_core::midi_in::now_nanos(),
            Some((1, 2)),
            &mut after,
        );
        assert_eq!(
            after
                .iter()
                .map(|hit| (hit.note, hit.velocity))
                .collect::<Vec<_>>(),
            [(67, 90)],
            "exactly the unplaced press survives the mini install"
        );
    }

    /// The incoming score claims again a press pinned at or past a reload's
    /// takeover, where the device swap drops the outgoing audio. A pin before
    /// the takeover, even after the edit instant, is not claimed twice.
    #[test]
    fn a_press_pinned_past_a_reloads_takeover_is_heard_by_the_new_score_once() {
        let mut session = Session::with_config(SessionConfig {
            cps: 1.0,
            horizon: 1.0,
            ..Default::default()
        })
        .expect("session");
        session.set_schedule_lead(0.0);
        session.set_continuity_margin(0.2);
        session
            .evaluate(r#"const kb = await midikeys('keyboard'); kb(0.05).s('piano')"#)
            .expect("first score");
        session.transport().start();
        let port = session
            .midi_input_bus()
            .find("keyboard")
            .expect("keyboard port");

        // Three presses before the save. The first window pins one at its
        // begin, cycle 0. The requery that the first press arms pins one at
        // that requery's takeover, cycle 1/5: after the save's edit instant
        // and before the save's takeover. The refill pins one at its begin,
        // cycle 6/5: the frontier, which is the horizon and the continuity
        // margin ahead.
        port.observe_note_on(rustel_core::midi_in::now_nanos(), 1, 60, 100);
        session.schedule_at(0.0).expect("first window");
        port.observe_note_on(rustel_core::midi_in::now_nanos(), 1, 64, 80);
        session
            .requery_active_at(0.0)
            .expect("the press requery")
            .expect("a running transport requeries");
        session
            .take_requery_takeover_time()
            .expect("the requery names its takeover");
        session.schedule_at(0.0).expect("the requery window");
        port.observe_note_on(rustel_core::midi_in::now_nanos(), 1, 67, 90);
        session.schedule_at(0.1).expect("the refill");
        let mut pinned = Vec::new();
        port.keys.select(
            0.0,
            2.0,
            rustel_core::midi_in::now_nanos(),
            None,
            &mut pinned,
        );
        assert_eq!(
            pinned
                .iter()
                .map(|hit| (hit.note, hit.num, hit.den))
                .collect::<Vec<_>>(),
            [(60, 0, 1), (64, 1, 5), (67, 6, 5)],
            "the outgoing score pinned all three presses"
        );

        let stopped = std::sync::atomic::AtomicBool::new(false);
        let replacement = session
            .reload_at_cancellable(
                r#"const kb = await midikeys('keyboard'); kb(0.05).s('sd:4')"#,
                false,
                0.1,
                &stopped,
            )
            .expect("live replacement");
        let takeover = session
            .take_requery_takeover_time()
            .expect("a continuous replacement names its takeover");
        assert!(
            0.2 < takeover && takeover < 1.2,
            "the requery's pin lies between the edit instant and the \
             takeover, the refill's past it, with the takeover at {takeover}"
        );

        let mut heard = Vec::new();
        for turn in 0..=10 {
            heard.extend(
                session
                    .schedule_at(0.1 + f64::from(turn) * 0.1)
                    .expect("replacement window")
                    .into_iter()
                    .filter(|event| event.generation == replacement),
            );
        }
        let played = |note: &str| {
            heard
                .iter()
                .filter(|event| event.value_show.starts_with(note))
                .count()
        };
        assert_eq!(
            played("note:67 "),
            1,
            "the press whose pin the takeover drops is heard once: {:?}",
            heard
                .iter()
                .map(|event| (&event.value_show, event.target_time))
                .collect::<Vec<_>>()
        );
        let claimed = heard
            .iter()
            .find(|event| event.value_show.starts_with("note:67 "))
            .expect("the claimed press");
        assert!(
            claimed.target_time >= 0.1,
            "the claimed press lands no earlier than the edit instant, where \
             the incoming score's first span begins: {}",
            claimed.target_time
        );
        assert_eq!(
            played("note:60 "),
            0,
            "the press that sounded before the takeover is not played again"
        );
        // Split at the edit instant instead, this pin would be forgotten and
        // claimed again while the outgoing score still plays it before the
        // takeover: the same press twice.
        assert_eq!(
            played("note:64 "),
            0,
            "the press pinned between the edit instant and the takeover sounds \
             under the outgoing score only"
        );
    }

    /// The device drops the outgoing audio from the takeover frame. A key pinned
    /// in the last frame before it has its onset there, so the new score claims
    /// the press again. A pin one whole frame before stays with the outgoing score.
    #[cfg(feature = "device-audio")]
    #[test]
    fn a_press_pinned_in_the_last_frame_before_a_takeover_is_claimed_again() {
        const RATE: i64 = 48_000;
        // A requery or a reload at 0.5 s takes over 0.25 s later, on this
        // frame. At one cycle a second a frame is 1/48000 cycle.
        const TAKEOVER_FRAME: i64 = 36_000;
        // 0.3 frame before the takeover frame, and one whole frame before.
        let on_the_takeover_frame = (TAKEOVER_FRAME * 10 - 3, RATE * 10);
        let before_the_takeover_frame = (TAKEOVER_FRAME - 1, RATE);
        let pinned_session = || {
            let mut session = Session::with_config(SessionConfig {
                cps: 1.0,
                ..Default::default()
            })
            .expect("session");
            session.set_schedule_lead(0.0);
            session.set_continuity_margin(0.25);
            session
                .evaluate(r#"const kb = await midikeys('keyboard'); kb(0.05).s('tri')"#)
                .expect("score");
            session.restart_transport_at(0.0);
            // A live producer has scheduled at this rate.
            assert!(
                session
                    .schedule_audio_live_at(
                        0.0,
                        RATE as u32,
                        Duration::ZERO,
                        LiveQueryBudgetMode::InitialPrefill,
                    )
                    .is_ok(),
                "the first live window"
            );
            let port = session
                .midi_input_bus()
                .find("keyboard")
                .expect("keyboard port");
            for (note, place) in [(60, on_the_takeover_frame), (64, before_the_takeover_frame)] {
                port.observe_note_on(rustel_core::midi_in::now_nanos(), 1, note, 100);
                let mut placed = Vec::new();
                port.keys.select(
                    0.0,
                    2.0,
                    rustel_core::midi_in::now_nanos(),
                    Some(place),
                    &mut placed,
                );
            }
            (session, port)
        };
        // What a query from cycle 4/5 finds: each press with its position.
        let claimed = |port: &rustel_core::midi_in::InputPort| {
            let mut hits = Vec::new();
            port.keys.select(
                0.0,
                2.0,
                rustel_core::midi_in::now_nanos(),
                Some((4, 5)),
                &mut hits,
            );
            hits.iter()
                .map(|hit| (hit.note, hit.num, hit.den))
                .collect::<Vec<_>>()
        };

        let (mut session, port) = pinned_session();
        session
            .requery_active_at(0.5)
            .expect("requery")
            .expect("a running transport requeries");
        assert_eq!(session.take_requery_takeover_time(), Some(0.75));
        assert_eq!(
            claimed(&port),
            [
                (64, before_the_takeover_frame.0, before_the_takeover_frame.1),
                (60, 4, 5)
            ],
            "the requery keeps the pin before the takeover frame, and claims \
             the press on it again"
        );

        let (mut session, port) = pinned_session();
        session
            .reload_at(
                r#"const kb = await midikeys('keyboard'); kb(0.05).s('sine')"#,
                false,
                0.5,
            )
            .expect("live replacement");
        assert_eq!(session.take_requery_takeover_time(), Some(0.75));
        assert_eq!(
            claimed(&port),
            [(60, 4, 5)],
            "the score taking over claims the press on the takeover frame, \
             and the pin before it stays with the outgoing score"
        );
    }

    #[test]
    fn a_definition_only_register_update_goes_silent_and_publishes_the_method() {
        let mut session = Session::with_config(SessionConfig::default()).expect("session");
        let stopped = std::sync::atomic::AtomicBool::new(false);
        session
            .reload_at_cancellable("$: s('bd')", false, 0.0, &stopped)
            .expect("initial live score");

        session
            .reload_at_cancellable(
                "register('double', (amount, pat) => pat.add(amount))",
                false,
                0.1,
                &stopped,
            )
            .expect("definition-only register update");
        assert!(
            session
                .query(Fraction::ZERO, Fraction::ONE)
                .expect("silent definition query")
                .is_empty(),
            "a definition-only update should install silence"
        );

        session
            .reload_at_cancellable("$: pure(1).double(2)", false, 0.2, &stopped)
            .expect("registered method in a later update");
        let haps = session
            .query(Fraction::ZERO, Fraction::ONE)
            .expect("registered method query");
        assert_eq!(haps.len(), 1, "{haps:?}");
        assert_eq!(haps[0].value, rustel_core::Value::F64(3.0));

        let function_error = "Error converting from js 'function' into type 'NativePatternWrapper'";
        assert!(
            Session::score_holds_no_pattern_message("() => 42", function_error).is_none(),
            "an unrelated function-valued score was mistaken for register setup"
        );
    }

    #[test]
    fn a_trailing_register_keeps_collected_lanes_playing() {
        for prefix in ["", "await Promise.resolve()\n"] {
            let mut session = Session::with_config(SessionConfig::default()).expect("session");
            session
                    .evaluate(&format!(
                        "{prefix}$: pure('first')\n$: pure('second')\nregister('identityTail', (pat) => pat)"
                    ))
                    .expect("score with trailing register");
            let values = session
                .query(Fraction::ZERO, Fraction::ONE)
                .expect("lane query")
                .into_iter()
                .map(|hap| hap.value)
                .collect::<Vec<_>>();
            assert_eq!(
                values,
                [
                    rustel_core::Value::Str("first".into()),
                    rustel_core::Value::Str("second".into()),
                ],
                "prefix {prefix:?}"
            );
        }
    }

    #[test]
    #[cfg(feature = "device-audio")]
    fn direct_live_reload_anchors_at_the_post_probe_device_clock() {
        let mut session = Session::new().expect("session");
        session
            .evaluate("setcps(1); note('c4')")
            .expect("initial score");
        let stopped = AtomicBool::new(false);
        let mut clocks = [0.1, 0.9].into_iter();

        session
            .reload_with_clock_cancellable("setcps(2); note('d4')", false, &stopped, || {
                clocks.next().expect("two reload clock samples")
            })
            .expect("live replacement");

        assert!(clocks.next().is_none(), "reload sampled an extra clock");
        assert!(
            (session.scheduler.cycle_at_time(0.9) - 0.9).abs() < 1e-9,
            "replacement pivoted at the pre-evaluation clock"
        );
        assert_eq!(session.take_requery_takeover_time(), Some(0.98));
    }

    /// A score installed from zero is heard from its own beginning: its
    /// takeover becomes cycle zero and it is queried from its first cycle.
    /// An ordinary reload joins the cycle already running.
    #[test]
    #[cfg(feature = "device-audio")]
    fn a_score_installed_from_zero_starts_at_its_own_beginning() {
        let landed = |from_zero: bool| {
            let mut session = Session::new().expect("session");
            session
                .evaluate("setcps(1); $: pure('old')")
                .expect("a score to replace");
            let stopped = AtomicBool::new(false);
            let mut clocks = [5.3, 5.3].into_iter();
            if from_zero {
                session.start_next_from_zero();
            }
            session
                .reload_with_clock_cancellable("setcps(1); $: pure('new')", false, &stopped, || {
                    clocks.next().expect("two reload clock samples")
                })
                .expect("live replacement");
            let takeover = session
                .take_requery_takeover_time()
                .expect("a continuous replacement names its takeover");
            (
                session.scheduler.cycle_at_time(takeover),
                session.scheduled_to_cycle(),
            )
        };

        // Joining: the takeover lands where the count already stood, a
        // little past cycle 5. The re-query cursor sits at the edit instant:
        // the replacement queries the overlap up to the takeover, with the
        // outgoing generation's onsets pre-marked. The device flips at the
        // takeover cycle.
        let (joined, joined_cursor) = landed(false);
        assert!((joined - 5.38).abs() < 1e-9, "{joined}");
        assert!((joined_cursor - 5.3).abs() < 1e-9, "{joined_cursor}");

        // From zero: the same instant IS cycle zero, and the score is
        // queried from its first cycle rather than from cycle five.
        let (zeroed, zeroed_cursor) = landed(true);
        assert!(zeroed.abs() < 1e-9, "the takeover is cycle zero: {zeroed}");
        assert!(
            zeroed_cursor.abs() < 1e-9,
            "and the query starts there, not in the past: {zeroed_cursor}"
        );
    }

    /// A from-zero reload's takeover carries a cut, so the restarted loop is
    /// heard alone. An ordinary reload's takeover carries none: its voices
    /// ring out.
    #[test]
    #[cfg(feature = "device-audio")]
    fn a_from_zero_reload_names_a_takeover_cut_an_ordinary_one_does_not() {
        let takeover = |from_zero: bool| {
            let mut session = Session::new().expect("session");
            session
                .evaluate("setcps(1); $: pure('old')")
                .expect("a score to replace");
            let stopped = AtomicBool::new(false);
            if from_zero {
                session.start_next_from_zero();
            }
            session
                .reload_with_clock_cancellable("setcps(1); $: pure('new')", false, &stopped, || 5.3)
                .expect("live replacement");
            session
                .take_requery_takeover()
                .expect("a continuous replacement names its takeover")
        };

        let (_, ordinary_cut) = takeover(false);
        assert_eq!(
            ordinary_cut,
            TakeoverCut::None,
            "an edit lets its voices ring out"
        );

        let (rewind_takeover, rewind_cut) = takeover(true);
        assert_eq!(
            rewind_cut,
            TakeoverCut::AtFlip,
            "an immediate rewind silences what sounds under it at the flip"
        );
        assert!(
            (rewind_takeover - 5.3).abs() < 1e-9,
            "the takeover is the edit instant itself: cycle zero lands at the cut, not a margin later (the hole a margin left was the restart's lag): {rewind_takeover}"
        );
    }

    /// An immediate rewind whose evaluation fails installs nothing and
    /// clears its from-zero mark, so the next ordinary save is not a rewind.
    #[test]
    #[cfg(feature = "device-audio")]
    fn a_failed_rewind_does_not_turn_the_next_save_into_a_restart() {
        let mut session = Session::new().expect("session");
        session
            .evaluate("setcps(1); $: pure('old')")
            .expect("a score to replace");
        let stopped = AtomicBool::new(false);
        session.start_next_from_zero();
        session
            .reload_with_clock_cancellable("setcps(1); $: pure('new'", false, &stopped, || 2.0)
            .expect_err("a broken score is refused");
        session
            .reload_with_clock_cancellable("setcps(1); $: pure('edit')", false, &stopped, || 2.5)
            .expect("an ordinary save installs");
        let (_, cut) = session
            .take_requery_takeover()
            .expect("the edit names its takeover");
        assert_eq!(
            cut,
            TakeoverCut::None,
            "the failed rewind's from-zero mark must not leak into the edit"
        );
    }

    /// A quantised scene launch that does NOT rewind rings out like an edit,
    /// so its takeover keeps the margin floor: an evaluation that ran past
    /// the line publishes a little late instead of into rendered frames,
    /// where the producer refuses it and rolls the launch back.
    #[test]
    #[cfg(feature = "device-audio")]
    fn a_plain_quantised_launch_keeps_the_margin_floor() {
        for install_clock in [3.05, 2.99] {
            let mut session = Session::new().expect("session");
            session
                .evaluate("setcps(1); $: pure('old')")
                .expect("a score to replace");
            session.set_continuity_margin(0.02);
            let stopped = AtomicBool::new(false);
            session.set_next_takeover_time(3.0);
            session
                .reload_with_clock_cancellable("setcps(1); $: pure('new')", false, &stopped, || {
                    install_clock
                })
                .expect("launched replacement");
            let (takeover, cut) = session
                .take_requery_takeover()
                .expect("the launch names its takeover");
            assert_eq!(cut, TakeoverCut::None, "a plain launch rings out");
            assert!(
                takeover >= install_clock + 0.02 - 1e-9,
                "installed at {install_clock}, the takeover keeps the floor: {takeover}"
            );
        }
    }

    /// A slider, key or clock-steer requery that lands between a quantised
    /// rewind's install and its publication re-queries the same graph. It
    /// must publish at the rewind's line with the rewind's cut: taking only
    /// the requery's own `now + margin` time kept the AtTakeover cut on it,
    /// and the countdown was choked a head-room before its line.
    #[test]
    #[cfg(feature = "device-audio")]
    fn a_control_requery_keeps_a_pending_rewinds_line() {
        let mut session = Session::new().expect("session");
        session
            .evaluate("setcps(1); $: pure('old')")
            .expect("a score to replace");
        let stopped = AtomicBool::new(false);
        session.set_next_takeover_time(3.0);
        session.start_next_from_zero();
        session
            .reload_with_clock_cancellable("setcps(1); $: pure('new')", false, &stopped, || 2.9)
            .expect("launched replacement");
        assert!(
            session
                .requery_active_at(2.92)
                .expect("a knob moves during the countdown")
                .is_some(),
            "the requery bumps the generation"
        );
        let (takeover, cut) = session
            .take_requery_takeover()
            .expect("the rewind's takeover survives the requery");
        assert_eq!(cut, TakeoverCut::AtTakeover, "still the rewind's cut");
        assert!(
            (takeover - 3.0).abs() < 1e-9,
            "published at the line, not at the requery's own time: {takeover}"
        );

        // Once the rewind has published, a requery is an ordinary one: its
        // own time and no cut at all.
        session.requery_active_at(4.0).expect("a later knob move");
        let (_, cut) = session
            .take_requery_takeover()
            .expect("the requery names its takeover");
        assert_eq!(cut, TakeoverCut::None, "an ordinary requery rings out");
    }

    /// A rewind whose commit fails after its cut would have been decided
    /// installs nothing and must leave no cut behind: latched before the
    /// fallible install, a stale AtTakeover rode the next slider requery and
    /// choked every sounding voice on a knob move.
    #[test]
    #[cfg(feature = "device-audio")]
    fn a_rewind_whose_commit_fails_leaves_no_cut_for_the_next_requery() {
        let mut session = Session::new().expect("session");
        session
            .evaluate("setcps(1); $: pure('old')")
            .expect("a score to replace");
        session.set_next_takeover_time(3.0);
        session.start_next_from_zero();
        // A missing callback cell makes wrapper installation fail regardless
        // of the engine's allocator or garbage-collection behavior. Enter the
        // commit directly so the failure occurs after the cut is decided.
        let pattern = rustel_core::pure(rustel_core::Value::F64(1.0)).fmap_js(usize::MAX);
        let failed = session.commit_evaluated_score(
            EvaluatedScore::MiniFallback(pattern),
            "",
            2.9,
            false,
            None,
            Duration::ZERO,
        );
        let error = failed.expect_err("the install is refused");
        assert!(
            error.to_string().contains("no cell was provided"),
            "{error}"
        );
        session
            .requery_active_at(3.5)
            .expect("a knob moves")
            .expect("the old score remains available to requery");
        let (_, cut) = session
            .take_requery_takeover()
            .expect("the requery names its takeover");
        assert_eq!(
            cut,
            TakeoverCut::None,
            "a failed rewind's cut must not ride the next requery"
        );
    }

    /// A quantised rewinding launch, one that waits for a cycle line, cuts at
    /// the line and not at the flip: the countdown plays until the boundary.
    /// The named takeover publishes the `AtTakeover` shape.
    #[test]
    #[cfg(feature = "device-audio")]
    fn a_quantised_launch_names_a_cut_at_the_line() {
        let mut session = Session::new().expect("session");
        session
            .evaluate("setcps(1); $: pure('old')")
            .expect("a score to replace");
        let stopped = AtomicBool::new(false);
        session.set_next_takeover_time(3.0);
        session.start_next_from_zero();
        session
            .reload_with_clock_cancellable("setcps(1); $: pure('new')", false, &stopped, || 2.9)
            .expect("launched replacement");
        let (takeover, cut) = session
            .take_requery_takeover()
            .expect("the launch names its takeover");
        assert_eq!(
            cut,
            TakeoverCut::AtTakeover,
            "the countdown plays to the line"
        );
        assert!(
            (takeover - 3.0).abs() < 1e-9,
            "the line is the takeover: {takeover}"
        );
    }

    /// The cut is consumed with the takeover it belongs to.
    ///
    /// A second, ordinary reload must not inherit the previous rewind's
    /// cut: the flag is taken once, alongside its own takeover, and a
    /// reload with no takeover behind it (a stopped one) clears both.
    #[test]
    #[cfg(feature = "device-audio")]
    fn a_takeover_cut_is_consumed_alongside_its_own_reload() {
        let mut session = Session::new().expect("session");
        session
            .evaluate("setcps(1); $: pure('a')")
            .expect("first score");
        let stopped = AtomicBool::new(false);
        session.start_next_from_zero();
        session
            .reload_with_clock_cancellable("setcps(1); $: pure('b')", false, &stopped, || 1.0)
            .expect("rewinding replacement");
        let (first_takeover, first_cut) = session
            .take_requery_takeover()
            .expect("the rewind names its takeover");
        assert_eq!(first_cut, TakeoverCut::AtFlip, "the rewind's flip cuts");
        assert!(
            (first_takeover - 1.0).abs() < 1e-9,
            "the takeover is the edit instant, cycle zero with it: {first_takeover}"
        );

        // The next reload, an ordinary one, must publish NO cut.
        session
            .reload_with_clock_cancellable("setcps(1); $: pure('c')", false, &stopped, || 2.0)
            .expect("ordinary replacement");
        let (_, second_cut) = session
            .take_requery_takeover()
            .expect("the ordinary replacement names its takeover");
        assert_eq!(
            second_cut,
            TakeoverCut::None,
            "the rewind's cut did not leak to the next reload"
        );

        // And with nothing pending, taking again is clean: no stale cut.
        assert_eq!(session.take_requery_takeover(), None);
    }

    /// A typo is not the producer turning work away. The studio header
    /// paints DSP/sched red for a capacity refusal, and that flash is
    /// wrong after a score the engine will not even parse.
    #[test]
    #[cfg(feature = "device-audio")]
    fn a_syntax_error_on_live_reload_is_not_a_producer_capacity_refusal() {
        let mut session = Session::new().expect("session");
        session.evaluate("note('c4')").expect("audible score");
        let generation = session.generation();
        let stopped = AtomicBool::new(false);
        let error = session
            .reload_with_clock_cancellable("note(", false, &stopped, || 0.0)
            .expect_err("invalid syntax");
        assert_eq!(error.kind(), "evaluation");
        assert_eq!(session.generation(), generation);
        let turn = session.take_pending_producer_turn();
        assert_eq!(turn.atomic_refusal_count, 0);
        assert_eq!(turn.committed_refusal_count, 0);
    }

    /// The live reload path does not allow the mini fallback, so it reaches a
    /// different arm than `evaluate` does. Commenting the only line out was
    /// refused there and the previous part kept playing, which is what a live
    /// coder actually hits when they mute by commenting.
    #[test]
    fn a_live_reload_that_names_no_pattern_goes_silent_rather_than_being_refused() {
        let mut session = Session::with_config(SessionConfig::default()).expect("session");
        let stopped = std::sync::atomic::AtomicBool::new(false);
        session
            .reload_at_cancellable("$: s('bd*4')", false, 0.0, &stopped)
            .expect("initial reload");
        assert!(
            !session
                .query(Fraction::new(0, 1), Fraction::new(1, 1))
                .expect("query")
                .is_empty(),
            "the set never started"
        );

        session
            .reload_at_cancellable("// $: s('bd*4')", false, 0.0, &stopped)
            .expect("a commented-out live reload is accepted, not refused");
        assert!(
            session
                .query(Fraction::new(0, 1), Fraction::new(1, 1))
                .expect("query")
                .is_empty(),
            "the muted reload kept playing"
        );

        session
            .reload_at_cancellable("$: s('bd*4')", false, 0.0, &stopped)
            .expect("uncommenting resumes");
        assert!(
            !session
                .query(Fraction::new(0, 1), Fraction::new(1, 1))
                .expect("query")
                .is_empty(),
            "uncommenting did not resume"
        );
    }

    /// Commenting a part out is how a live coder mutes it. Upstream goes quiet
    /// and resumes when the comment comes off; refusing the save instead kept
    /// the old part playing, so nothing could be muted that way.
    #[test]
    fn a_score_that_names_no_pattern_installs_silence_instead_of_being_refused() {
        let mut session = Session::with_config(SessionConfig::default()).expect("session");
        session.evaluate("s('bd*16')").expect("initial evaluate");

        // Everything commented out: evaluates cleanly, names no pattern.
        session
            .evaluate("setcps(0.5)\n// $: s('bd*16')")
            .expect("a commented-out score is accepted");
        assert!(
            session
                .query(Fraction::new(0, 1), Fraction::new(1, 1))
                .expect("query")
                .is_empty(),
            "a muted score still produced events"
        );

        // And it comes back.
        session
            .evaluate("s('bd*16')")
            .expect("uncommenting resumes");
        assert!(
            !session
                .query(Fraction::new(0, 1), Fraction::new(1, 1))
                .expect("query")
                .is_empty(),
            "uncommenting did not resume"
        );
    }

    #[cfg(feature = "device-audio")]
    #[test]
    fn negative_cycle_timelines_remain_valid_without_an_explicit_start_floor() {
        for previously_started in [false, true] {
            let mut session = Session::new().expect("session");
            session.evaluate(r#"s("bd*16")"#).expect("score");
            session.set_continuity_margin(0.02);
            if previously_started {
                session.restart_transport_at(10.0);
                // An external clock takes ownership of the cycle domain.
                session.retime(9.5, 0.5, -0.25);
            } else {
                // Generic scheduling did not promise a cycle-zero start.
                session.scheduler.rebase_anchor(9.5, -0.25);
            }
            session.requery_active_at(9.5).expect("control requery");
            let events = session
                .schedule_through(9.5, 10.0)
                .expect("negative cycle schedule");
            let first = events.first().expect("negative cycle has music");
            assert_eq!(first.whole_begin, "-3/16");
            assert_eq!(first.target_time, 9.625);

            // Inspecting negative pattern arcs is always legitimate, even
            // when an explicitly restarted playback starts from cycle zero.
            session.restart_transport_at(10.0);
            assert_eq!(
                session
                    .query(Fraction::int(-1), Fraction::ZERO)
                    .expect("negative query")
                    .len(),
                16,
            );
            session
                .requery_active_at(9.7)
                .expect("restarted control requery");
            let restarted = session
                .schedule_through(9.7, 10.2)
                .expect("restarted schedule");
            let first = restarted.first().expect("restarted downbeat");
            assert_eq!(first.whole_begin, "0/1");
            assert_eq!(first.target_time, 10.0);
        }
    }

    #[cfg(feature = "device-audio")]
    #[test]
    fn preroll_requeries_never_schedule_the_previous_cycle() {
        const RATE: u32 = 48_000;
        const START: f64 = 10.0;
        for operation in ["control", "recycle", "reload", "invalid_retime"] {
            for prefilled in [false, true] {
                let mut session = Session::with_config(SessionConfig {
                    horizon: 1.0,
                    sample_rate: RATE,
                    ..SessionConfig::default()
                })
                .expect("session");
                session.evaluate(r#"s("bd*16")"#).expect("score");
                session.restart_transport_at(START);
                session.set_schedule_lead(0.35);
                session.set_continuity_margin(0.02);
                if prefilled {
                    let Ok(batch) = session.schedule_audio_live_at(
                        9.5,
                        RATE,
                        Duration::ZERO,
                        LiveQueryBudgetMode::InitialPrefill,
                    ) else {
                        panic!("initial prefill failed");
                    };
                    assert!(!batch.events.is_empty(), "initial downbeat was queued");
                    assert!(
                        batch
                            .events
                            .iter()
                            .all(|event| event.target_frame >= 480_000)
                    );
                }
                let now = if operation == "recycle" { 9.5 } else { 9.7 };
                match operation {
                    "control" => {
                        session.requery_active_at(now).expect("control requery");
                    }
                    "recycle" => {
                        session
                            .requery_after_output_recycle_at(now)
                            .expect("recycle requery");
                    }
                    "reload" => {
                        session
                            .reload_at(r#"s("bd*16").gain(0.5)"#, false, now)
                            .expect("reload");
                    }
                    "invalid_retime" => {
                        // A rejected external-clock update must not release
                        // the current transport's promised beginning.
                        session.retime(f64::NAN, 0.5, -0.25);
                        session.requery_active_at(now).expect("control requery");
                    }
                    _ => unreachable!(),
                }
                let Ok(batch) = session.schedule_audio_live_at(
                    now,
                    RATE,
                    Duration::ZERO,
                    LiveQueryBudgetMode::ReplacementPrefill,
                ) else {
                    panic!("replacement prefill failed");
                };
                let targets: Vec<_> = batch
                    .events
                    .iter()
                    .map(|event| event.target_frame)
                    .collect();
                assert_eq!(
                    targets.first().copied(),
                    Some(480_000),
                    "{operation}, prefilled={prefilled}: previous-cycle events precede cycle zero: {targets:?}"
                );
                assert!(targets.windows(2).all(|pair| pair[1] - pair[0] == 6_000));
            }
        }
    }

    /// The audio thread drops the queued events of the old generation from the
    /// takeover frame. A live reload re-queries from the continuity margin (the
    /// device's consumption frontier), not from the start lead.
    #[cfg(feature = "device-audio")]
    #[test]
    fn live_reload_requeries_from_the_continuity_margin_not_the_start_lead() {
        let mut session = Session::with_config(SessionConfig::default()).expect("session");
        session.evaluate(r#"s("bd*16")"#).expect("initial evaluate");
        session.restart_transport_at(10.0);
        session.set_schedule_lead(0.35);
        session.set_continuity_margin(0.02);
        // Prefill the horizon, as the live producer would before the save.
        let Ok(_) = session.schedule_audio_live_at(
            10.0,
            44_100,
            std::time::Duration::ZERO,
            LiveQueryBudgetMode::InitialPrefill,
        ) else {
            panic!("prefill schedule failed");
        };
        // The save lands at 11.3. Every queued event is about to be dropped
        // by the consumer, so the new generation must resume at the margin.
        session
            .reload_at(r#"s("bd*16")"#, false, 11.3)
            .expect("reload");
        let Ok(batch) = session.schedule_audio_live_at(
            11.3,
            44_100,
            std::time::Duration::ZERO,
            LiveQueryBudgetMode::ReplacementPrefill,
        ) else {
            panic!("requery schedule failed");
        };
        let first_seconds = batch
            .events
            .iter()
            .map(|event| event.target_frame as f64 / 44_100.0)
            .fold(f64::INFINITY, f64::min);
        // s("bd*16") at the default tempo onsets every 1/(16·cps) seconds;
        // the first re-scheduled onset must sit inside margin + one step of
        // the pattern, and decisively before the floored start lead.
        let step = 1.0 / (16.0 * session.cps());
        assert!(
            first_seconds <= 11.3 + 0.02 + step + 1e-9,
            "first re-scheduled onset at {first_seconds:.3}s leaves a reload hole"
        );
        assert!(
            first_seconds < 11.3 + 0.35,
            "reload cursor still sits at the floored start lead"
        );
    }

    /// Dropping from a very fast tempo to a normal one used to resume the new
    /// generation hundreds of old-tempo cycles ahead: the re-query cursor was
    /// snapshotted as a cycle under the old mapping and then interpreted
    /// under the new one. Only a second evaluate could unstick that silence.
    #[cfg(feature = "device-audio")]
    #[test]
    fn a_tempo_drop_resumes_at_the_continuity_margin_not_old_cycles_later() {
        let mut session = Session::with_config(SessionConfig::default()).expect("session");
        session
            .evaluate("setcps(400)\n$: s(\"bd*4\")")
            .expect("initial evaluate");
        session.restart_transport_at(10.0);
        session.set_schedule_lead(0.35);
        session.set_continuity_margin(0.05);
        assert!((session.cps() - 400.0).abs() < 1e-9);

        session
            .reload_at("setcps(0.6)\n$: s(\"bd*4\")", false, 11.0)
            .expect("reload");
        assert!((session.cps() - 0.6).abs() < 1e-9);
        let Ok(batch) = session.schedule_audio_live_at(
            11.0,
            44_100,
            std::time::Duration::ZERO,
            LiveQueryBudgetMode::ReplacementPrefill,
        ) else {
            panic!("requery schedule failed");
        };
        let first_seconds = batch
            .events
            .iter()
            .map(|event| event.target_frame as f64 / 44_100.0)
            .fold(f64::INFINITY, f64::min);
        let step = 1.0 / (4.0 * session.cps());
        assert!(
            first_seconds <= 11.0 + 0.05 + step + 1e-6,
            "first onset after the tempo drop lands at {first_seconds:.3}s"
        );
        assert_eq!(session.take_requery_takeover_time(), Some(11.05));
    }

    /// A host whose playback latency exceeds the default 0.5 s query horizon
    /// (WSLg RDP sinks have reported >1 s) must still schedule through the
    /// continuity margin. Otherwise takeover sits in unscheduled time and the
    /// live tee records a silent second on the save.
    #[cfg(feature = "device-audio")]
    #[test]
    fn live_schedule_covers_a_continuity_margin_past_the_default_horizon() {
        let mut session = Session::with_config(SessionConfig::default()).expect("session");
        session.evaluate(r#"s("bd*16")"#).expect("initial evaluate");
        session.restart_transport_at(10.0);
        session.set_schedule_lead(0.35);
        session.set_continuity_margin(1.0);
        let batch = match session.schedule_audio_live_at(
            10.0,
            44_100,
            std::time::Duration::ZERO,
            LiveQueryBudgetMode::InitialPrefill,
        ) {
            Ok(batch) => batch,
            Err(super::LiveAudioScheduleError::Retryable(error))
            | Err(super::LiveAudioScheduleError::AwaitingSamples(error))
            | Err(
                super::LiveAudioScheduleError::CandidateCommitted(error)
                | super::LiveAudioScheduleError::CandidateRefusedScore(error),
            ) => {
                panic!("prefill schedule failed: {error}");
            }
        };
        let last_seconds = batch
            .events
            .iter()
            .map(|event| event.target_frame as f64 / 44_100.0)
            .fold(0.0_f64, f64::max);
        assert!(
            last_seconds >= 10.0 + 1.0 - 1e-6,
            "prefill only reached {last_seconds:.3}s; takeover at 11.0s would be a hole"
        );
        session
            .reload_at(r#"s("bd*16")"#, false, 10.0)
            .expect("reload");
        let Ok(batch) = session.schedule_audio_live_at(
            10.0,
            44_100,
            std::time::Duration::from_millis(25),
            LiveQueryBudgetMode::ReplacementPrefill,
        ) else {
            panic!("requery schedule failed");
        };
        let first_seconds = batch
            .events
            .iter()
            .map(|event| event.target_frame as f64 / 44_100.0)
            .fold(f64::INFINITY, f64::min);
        let step = 1.0 / (16.0 * session.cps());
        assert!(
            first_seconds <= 10.0 + 1.0 + step + 1e-9,
            "first replacement onset at {first_seconds:.3}s leaves a reload hole"
        );
    }

    /// A live reload never moves the
    /// cycle↔time mapping (you time your ctrl-enter and the change lands on
    /// beat), while transport start rebases cycle zero. The plain
    /// `replace_pattern` re-anchor jumps the phase by the un-played horizon;
    /// `install_evaluated_score` restores continuity around it.
    #[test]
    fn live_reload_keeps_the_timeline_and_restart_rebases_to_zero() {
        let mut session = Session::with_config(SessionConfig::default()).expect("session");
        session.evaluate("note('c4')").expect("initial evaluate");
        session.restart_transport_at(10.0);
        let origin_before = session.time_at_cycle(rustel_fraction::Fraction::ZERO);
        assert!((origin_before - 10.0).abs() < 1e-12);

        // Live reload mid-flight: mapping must not move at all.
        session.set_schedule_lead(0.25);
        session
            .reload_at("note('c5')", false, 11.3)
            .expect("live reload");
        let origin_after = session.time_at_cycle(rustel_fraction::Fraction::ZERO);
        assert!(
            (origin_after - origin_before).abs() < 1e-9,
            "live reload moved the timeline origin: {origin_before} -> {origin_after}"
        );
        // The re-query cursor starts at the edit instant. The replacement
        // queries the overlap `(edit, takeover)`, and the old generation's
        // emitted onsets are pre-marked so the device's copies do not double.
        let cursor_time = session.time_at_cycle(
            rustel_fraction::Fraction::from_f64(session.cycle_at_time(11.3)).expect("cursor cycle"),
        );
        assert!(
            (cursor_time - 11.3).abs() < 1e-9,
            "cursor did not land on the edit instant: {cursor_time}"
        );
        // The takeover time itself is unchanged: the device still flips
        // generations one schedule lead after the edit.
        assert_eq!(
            session.take_requery_takeover_time(),
            Some(11.3 + 0.25),
            "the edit-instant overlap must not move the takeover"
        );

        // A tempo change pivots at `now`: the cycle position there is
        // preserved while the origin moves with the new slope.
        let cycle_at_pivot = session.cycle_at_time(12.0);
        session
            .reload_at("setcps(1); note('c4')", false, 12.0)
            .expect("tempo reload");
        assert!(
            (session.cycle_at_time(12.0) - cycle_at_pivot).abs() < 1e-9,
            "tempo change moved the phase at the pivot"
        );
        assert!((session.cps() - 1.0).abs() < 1e-12);

        // Restart semantics: play begins at cycle zero again.
        session.restart_transport_at(20.0);
        assert!((session.time_at_cycle(rustel_fraction::Fraction::ZERO) - 20.0).abs() < 1e-12);
    }

    #[test]
    fn live_mini_reload_keeps_the_timeline() {
        let mut session = Session::with_config(SessionConfig::default()).expect("session");
        session.evaluate_mini("bd sd").expect("initial mini");
        session.restart_transport_at(10.0);
        let origin_before = session.time_at_cycle(rustel_fraction::Fraction::ZERO);
        session.set_schedule_lead(0.25);
        session
            .reload_at("bd hh", true, 11.3)
            .expect("mini live reload");
        let origin_after = session.time_at_cycle(rustel_fraction::Fraction::ZERO);
        assert!(
            (origin_after - origin_before).abs() < 1e-9,
            "mini live reload moved the timeline origin: {origin_before} -> {origin_after}"
        );
    }

    /// An edit lands 0.1 s before a cycle boundary, so the hap on that
    /// boundary sits inside the 0.25 s continuity margin. The replacement is
    /// queried from the edit instant and emits that hap.
    #[test]
    fn a_live_reload_keeps_a_first_hap_inside_the_continuity_margin() {
        let mut session = Session::with_config(SessionConfig::default()).expect("session");
        // cps 1: one cycle per second, boundaries every second. The muted
        // first beat is the "muted synth lane" of the hand-timed save.
        // Restarting at 0.0 puts cycle zero at time zero, so cycle N is time N.
        // Double quotes: a single-quoted string is plain text, one hap to the
        // cycle, and the edit would change the hap on the boundary rather
        // than un-mute it.
        session
            .evaluate(r#"setcps(1); n("~ 1 2 3").s("piano")"#)
            .expect("initial score");
        session.restart_transport_at(0.0);
        session.set_schedule_lead(0.25);
        session.set_continuity_margin(0.25);

        // The producer at clock 11.9 has drained its cover, past the boundary
        // at 12.0, with `schedule_through(now, now + horizon + margin)`. One
        // tick covers at most MAX_TICK_SPAN_CYCLES, so advance the cursor in
        // steps first.
        let cover = session.config.horizon + 0.25;
        for _ in 0..10 {
            if session.scheduled_to_cycle() > 12.25 {
                break;
            }
            session
                .schedule_through(11.9, 11.9 + cover)
                .expect("producer prefetch past the boundary");
        }
        assert!(
            session.scheduled_to_cycle() > 12.25,
            "test premise: prefetched past the boundary, got {}",
            session.scheduled_to_cycle()
        );

        // The edit lands at 11.9 - 0.1 s before the boundary at 12.0 - and
        // un-mutes the first beat. The takeover is one margin later, at 12.15:
        // the hap at 12.0 lands inside the takeover window.
        session
            .reload_at(r#"setcps(1); n("1 2 3 4").s("piano")"#, false, 11.9)
            .expect("the un-muting edit");
        assert_eq!(
            session.take_requery_takeover_time(),
            Some(12.15),
            "the takeover stays one margin after the edit"
        );

        // The replacement's first window covers from the edit instant: the
        // 12.0 hap is emitted (late), not silently skipped.
        let events = session
            .schedule_through(11.9, 11.9 + cover)
            .expect("replacement first window");
        let parse_label = |label: &str| -> f64 {
            let mut parts = label.splitn(2, '/');
            let n: f64 = parts.next().expect("numerator").parse().expect("numerator");
            let d: f64 = parts
                .next()
                .map_or(1.0, |d| d.parse().expect("denominator"));
            n / d
        };
        let overlap: Vec<f64> = events
            .iter()
            .map(|event| parse_label(&event.whole_begin))
            .filter(|begin| (11.9..12.15).contains(begin))
            .collect();
        assert_eq!(
            overlap,
            vec![12.0],
            "the un-muted hap ON the boundary must be emitted from the edit \
             instant: {overlap:?}"
        );
    }

    /// An edit changes the hap on the boundary. The device still plays the
    /// outgoing copy, so the replacement does not emit a second copy. The
    /// change is heard from the first onset past the takeover.
    #[test]
    fn a_live_reload_leaves_a_changed_hap_inside_the_continuity_margin_to_the_device() {
        let mut session = Session::with_config(SessionConfig::default()).expect("session");
        session
            .evaluate(r#"setcps(1); n("1 2 3 4").s("piano")"#)
            .expect("initial score");
        session.restart_transport_at(0.0);
        session.set_schedule_lead(0.25);
        session.set_continuity_margin(0.25);

        // As above: the producer at clock 11.9 has drained the horizon past
        // the boundary at 12.0 into the device ring.
        let cover = session.config.horizon + 0.25;
        for _ in 0..10 {
            if session.scheduled_to_cycle() > 12.25 {
                break;
            }
            session
                .schedule_through(11.9, 11.9 + cover)
                .expect("producer prefetch past the boundary");
        }
        assert!(
            session.scheduled_to_cycle() > 12.25,
            "test premise: prefetched past the boundary, got {}",
            session.scheduled_to_cycle()
        );

        // Every hap gains a control. The hap at 12.0 lands inside the
        // takeover window (11.9, 12.15), where the device has its old copy.
        session
            .reload_at(r#"setcps(1); n("1 2 3 4").s("piano").gain(1)"#, false, 11.9)
            .expect("the edit that changes every hap");
        let events = session
            .schedule_through(11.9, 11.9 + cover)
            .expect("replacement first window");
        let begin = |label: &str| -> f64 {
            let mut parts = label.splitn(2, '/');
            let n: f64 = parts.next().expect("numerator").parse().expect("numerator");
            let d: f64 = parts
                .next()
                .map_or(1.0, |d| d.parse().expect("denominator"));
            n / d
        };
        let begins: Vec<f64> = events
            .iter()
            .map(|event| begin(&event.whole_begin))
            .collect();
        assert_eq!(
            begins.first(),
            Some(&12.25),
            "the changed hap ON the boundary is the device's own copy to \
             play; the replacement starts past the takeover: {begins:?}"
        );
    }

    /// What sounds across a save at clock 11.9 with a 0.25 s continuity
    /// margin, as `(time, begin)`: the outgoing copies the device keeps
    /// (aimed before the takeover at 12.15) and all that the replacement
    /// emits. The outgoing score runs at one cycle a second, so cycle N is
    /// time N. With a `live_rate`, the save reads its takeover on a frame
    /// at that sample rate, as a live device does.
    fn sounding_across_a_save(old: &str, new: &str, live_rate: Option<u32>) -> Vec<(f64, f64)> {
        let begin_of = |label: &str| -> f64 {
            let mut parts = label.splitn(2, '/');
            let n: f64 = parts.next().expect("numerator").parse().expect("numerator");
            let d: f64 = parts
                .next()
                .map_or(1.0, |d| d.parse().expect("denominator"));
            n / d
        };
        let mut session = Session::with_config(SessionConfig::default()).expect("session");
        session.evaluate(old).expect("initial score");
        session.restart_transport_at(0.0);
        session.set_schedule_lead(0.25);
        session.set_continuity_margin(0.25);
        session.live_sample_rate = live_rate;
        let cover = session.config.horizon + 0.25;
        let mut sounding = Vec::new();
        for _ in 0..10 {
            if session.scheduled_to_cycle() > 12.6 {
                break;
            }
            for event in session
                .schedule_through(11.9, 11.9 + cover)
                .expect("producer prefetch past the takeover")
            {
                if (11.9..12.15).contains(&event.target_time) {
                    sounding.push((event.target_time, begin_of(&event.whole_begin)));
                }
            }
        }
        assert!(
            session.scheduled_to_cycle() > 12.6,
            "test premise: prefetched past the takeover, got {}",
            session.scheduled_to_cycle()
        );
        session.reload_at(new, false, 11.9).expect("the save");
        for step in 0..5 {
            let now = 11.9 + 0.1 * f64::from(step);
            for event in session
                .schedule_through(now, now + cover)
                .expect("replacement window")
            {
                sounding.push((event.target_time, begin_of(&event.whole_begin)));
            }
        }
        sounding.sort_by(|a, b| a.partial_cmp(b).expect("finite times"));
        sounding
    }

    /// A save that changes the tempo sounds every onset once. The outgoing
    /// rate decides which onsets the replacement leaves to the device, for
    /// a slower score and for a faster score.
    #[test]
    fn a_live_reload_at_a_new_tempo_sounds_every_onset_once() {
        for (rate, live_rate) in [
            ("0.8", None),
            ("1.2", None),
            ("0.8", Some(48_000)),
            ("1.2", Some(48_000)),
        ] {
            let sounding = sounding_across_a_save(
                r#"setcps(1); s("bd*32")"#,
                &format!(r#"setcps({rate}); s("bd*32")"#),
                live_rate,
            );
            // Every onset from the save to cycle 12.4, on the 1/32 grid.
            for step in 381..397 {
                let begin = f64::from(step) / 32.0;
                let times: Vec<f64> = sounding
                    .iter()
                    .filter(|(_, sounded)| *sounded == begin)
                    .map(|(time, _)| *time)
                    .collect();
                assert_eq!(
                    times.len(),
                    1,
                    "at {rate} cycles a second the onset at {begin} sounded at {times:?} \
                     (live rate {live_rate:?})"
                );
            }
        }
    }

    /// A save to a tenth of the tempo resumes about one margin after the
    /// takeover. Without the exception in `replace_pattern_continued`, the
    /// replacement has nothing to play until 2.5 s after the edit.
    #[test]
    fn a_live_reload_after_a_tempo_drop_resumes_within_a_margin_of_the_takeover() {
        for live_rate in [None, Some(48_000)] {
            let sounding = sounding_across_a_save(
                r#"setcps(1); s("bd*32")"#,
                r#"setcps(0.1); s("bd*32")"#,
                live_rate,
            );
            // One step of the pattern at the new tempo.
            let step = 1.0 / (32.0 * 0.1);
            // The outgoing copies all sound before the takeover at 12.15.
            let resumed = sounding
                .iter()
                .map(|(time, _)| *time)
                .find(|time| *time >= 12.15);
            assert!(
                resumed.is_some_and(|time| time <= 12.15 + 0.25 + step),
                "the replacement resumed at {resumed:?} (live rate {live_rate:?})"
            );
        }
    }

    /// A save that moves every hap 0.02 s later or earlier leaves the haps
    /// inside the continuity margin to the device. Each has an outgoing
    /// copy 0.02 s away, so only the outgoing copies sound there.
    #[test]
    fn a_live_reload_leaves_a_moved_hap_inside_the_continuity_margin_to_the_device() {
        for (moved, live_rate) in [
            ("late", None),
            ("early", None),
            ("late", Some(48_000)),
            ("early", Some(48_000)),
        ] {
            let old = r#"setcps(1); s("bd*8")"#;
            let sounding = sounding_across_a_save(old, &format!("{old}.{moved}(0.02)"), live_rate);
            let before_takeover: Vec<f64> = sounding
                .iter()
                .map(|(time, _)| *time)
                .filter(|time| *time < 12.15)
                .collect();
            assert_eq!(
                before_takeover,
                vec![12.0, 12.125],
                "{moved}: only the outgoing copies sound before the takeover \
                 (live rate {live_rate:?})"
            );
        }
    }

    /// A replacement that evaluates but throws at query time keeps the
    /// last-good graph and generation. `"a b".add("x")` throws when it is
    /// queried, not when it is built.
    #[test]
    fn live_reload_query_failure_restores_the_exact_last_good_callback_wrapper() {
        let mut session = Session::new().expect("session");
        session
                .evaluate(
                    "setDefaultJoin('out'); (() => { let calls = 0; return pure('last-good').fmap(value => `${value}:${++calls}`); })()",
                )
                .expect("callback-bearing initial score");
        let generation = session.generation();
        let before = shown_note(&session);
        assert_eq!(before, "last-good:1");

        let error = session
            .reload_at(
                r#"setDefaultJoin("mix"); pure("foo").add("bar")"#,
                false,
                0.25,
            )
            .expect_err("query-throwing replacement must not install");
        assert_eq!(error.kind(), "evaluation");
        assert!(
            error.to_string().contains("last-good score kept"),
            "rejection must name the never-die contract: {error}"
        );
        assert_eq!(session.generation(), generation);
        assert_eq!(
            session
                .js
                .with_runtime_settings(rustel_core::compose::default_alignment),
            rustel_core::compose::Alignment::Out,
            "a rejected query probe published its candidate module settings"
        );
        session.js.run_gc();
        session.js.run_gc();
        assert_eq!(shown_note(&session), "last-good:2");
        let after = session
            .schedule_at(0.25)
            .expect("last-good still schedules");
        assert!(
            after
                .iter()
                .any(|event| event.value_show.contains("last-good:3")),
            "query-throwing reload silenced the sounding score: {after:?}"
        );
    }

    #[test]
    fn accepted_live_reload_publishes_candidate_settings_after_its_probe() {
        let mut session = Session::new().expect("session");
        session
            .evaluate("setDefaultJoin('out'); pure('last-good')")
            .expect("initial score");
        assert_eq!(
            session
                .js
                .with_runtime_settings(rustel_core::compose::default_alignment),
            rustel_core::compose::Alignment::Out
        );

        session
            .reload_at("setDefaultJoin('mix'); pure('candidate')", false, 0.25)
            .expect("accepted candidate");
        assert_eq!(
            session
                .js
                .with_runtime_settings(rustel_core::compose::default_alignment),
            rustel_core::compose::Alignment::Mix,
            "the accepted query probe did not publish its candidate module settings"
        );
        assert_eq!(shown_note(&session), "candidate");
    }

    fn queried_parts(session: &Session) -> Vec<(Fraction, Fraction)> {
        session
            .query(Fraction::ZERO, Fraction::ONE)
            .expect("query score")
            .into_iter()
            .map(|hap| (hap.part.begin, hap.part.end))
            .collect()
    }

    #[test]
    fn rejected_default_join_does_not_reconfigure_the_last_good_callback() {
        let mut session = Session::new().expect("session");
        session
                .evaluate(
                    "setDefaultJoin('restart'); pure(10).fmap(x => fastcat(x,x).add(fastcat(1,2,3))).innerJoin()",
                )
                .expect("callback-bearing last-good score");
        let thirds = vec![
            (Fraction::ZERO, Fraction::new(1, 3)),
            (Fraction::new(1, 3), Fraction::new(2, 3)),
            (Fraction::new(2, 3), Fraction::ONE),
        ];
        assert_eq!(queried_parts(&session), thirds);

        session
            .reload_at(
                "setDefaultJoin('mix'); throw new Error('reject candidate')",
                false,
                0.25,
            )
            .expect_err("candidate must reject");
        assert_eq!(
            queried_parts(&session),
            thirds,
            "rejected JavaScript join state changed the last-good callback"
        );
    }

    #[test]
    fn rejected_default_voicings_do_not_reconfigure_the_last_good_callback() {
        let mut session = Session::new().expect("session");
        session
                .evaluate(
                    "setDefaultVoicings('guidetones'); pure('C7').fmap(x => rustelScope.voicing(pure(x))).innerJoin()",
                )
                .expect("callback-bearing last-good score");
        assert_eq!(
            session.query(Fraction::ZERO, Fraction::ONE).unwrap().len(),
            2
        );

        session
            .reload_at(
                "setDefaultVoicings('lefthand'); throw new Error('reject candidate')",
                false,
                0.25,
            )
            .expect_err("candidate must reject");
        assert_eq!(
            session.query(Fraction::ZERO, Fraction::ONE).unwrap().len(),
            2,
            "rejected JavaScript voicing state changed the last-good callback"
        );
    }

    #[test]
    fn rejected_voicing_candidate_preserves_scope_and_captured_public_routes() {
        let mut session = Session::new().expect("session");
        session
                .evaluate(
                    "setDefaultVoicings('guidetones'); (() => { const acceptedVoicing = voicing; return stack(pure('C7').fmap(x => rustelScope.voicing(pure(x))).innerJoin(), pure('C7').fmap(x => acceptedVoicing(pure(x))).innerJoin()); })()",
                )
                .expect("last-good score through both voicing routes");
        assert_eq!(
            session.query(Fraction::ZERO, Fraction::ONE).unwrap().len(),
            4
        );

        session
            .reload_at(
                "setDefaultVoicings('lefthand'); throw new Error('reject candidate')",
                false,
                0.25,
            )
            .expect_err("candidate must reject");
        assert_eq!(
            session.query(Fraction::ZERO, Fraction::ONE).unwrap().len(),
            4,
            "a rejected default changed a scoped or captured voicing route"
        );
    }

    #[test]
    fn session_default_join_setter_controls_an_existing_deferred_composer() {
        let mut session = Session::new().expect("session");
        session
                .evaluate(
                    "setDefaultJoin('restart'); pure(10).fmap(x => fastcat(x,x).add(fastcat(1,2,3))).innerJoin()",
                )
                .expect("deferred composer score");
        assert_eq!(queried_parts(&session).len(), 3);

        session.set_default_join(rustel_core::compose::Alignment::Mix);
        assert_eq!(
            queried_parts(&session).len(),
            4,
            "the deferred JavaScript composer ignored Session's native setting"
        );
    }

    /// Never-die-live: a JS typo that happens to look like mini (`bd sd`)
    /// must reject on reload, not replace the playing JS song with drums.
    #[test]
    fn live_reload_does_not_mini_fallback_a_js_typo() {
        let mut session = Session::new().expect("session");
        session
            .evaluate(r#"s("bd sd").fast(2)"#)
            .expect("initial js score");
        let generation = session.generation();

        let error = session
            .reload_at("hh sd cp", false, 0.25)
            .expect_err("mini-looking leftover must not replace a JS song");
        assert_eq!(error.kind(), "evaluation");
        assert_eq!(session.generation(), generation);
        let haps = session
            .query(Fraction::ZERO, Fraction::ONE)
            .expect("last-good query");
        assert!(
            haps.len() >= 2,
            "mini fallback installed over the JS song: {haps:?}"
        );
    }

    fn shown_note(session: &Session) -> String {
        let haps = session
            .query(Fraction::ZERO, Fraction::ONE)
            .expect("query last-good");
        haps.iter()
            .map(|hap| hap.value.show())
            .collect::<Vec<_>>()
            .join(" ")
    }

    use super::test_support::session_with_sample_origin;
    use super::*;
    use rustel_core::Value;

    mod hap_budget {
        use super::*;

        const EVENT_EXPLOSION: &str =
            r#"setcpm(138/4); $: s("hh").iter(64).stut(64, 1, 1).echo(64, .001, 1)"#;

        #[test]
        fn initial_live_reload_rejects_a_pure_event_explosion_before_install() {
            let mut session = Session::new().expect("session");
            session.set_query_hap_budget(8).expect("small hap budget");

            let error = session
                .reload_at(EVENT_EXPLOSION, false, 0.0)
                .expect_err("the event-heavy score exceeds the configured hap budget");
            assert!(matches!(&error, RuntimeError::ResourceLimit(_)), "{error}");
            assert_eq!(session.generation(), 0, "the score was not installed");

            session
                .reload_at(r#"s("hh*4")"#, false, 0.0)
                .expect("an ordinary score still installs after the refusal");
            assert_eq!(
                session.query(Fraction::ZERO, Fraction::ONE).unwrap().len(),
                4
            );
        }

        #[test]
        fn live_reload_hap_budget_refusal_keeps_the_last_good_score() {
            let mut session = Session::new().expect("session");
            session.set_query_hap_budget(8).expect("small hap budget");
            let original = r#"s("bd*4")"#;
            session
                .reload_at(original, false, 0.0)
                .expect("initial score");
            let generation = session.generation();
            let before = shown_note(&session);

            let error = session
                .reload_at(EVENT_EXPLOSION, false, 0.25)
                .expect_err("event-heavy replacement must not install");
            assert!(matches!(&error, RuntimeError::ResourceLimit(_)), "{error}");
            assert_eq!(session.generation(), generation);
            assert_eq!(session.active_source(), Some(original));
            assert_eq!(shown_note(&session), before, "the old graph still queries");
        }

        #[test]
        fn initial_live_reload_applies_the_hap_cap_before_install() {
            let mut session = Session::new().expect("session");
            session.set_query_hap_budget(8).expect("small hap budget");

            let error = session
                .reload_at(r#"s("hh*16")"#, false, 0.0)
                .expect_err("the ninth hap must refuse the candidate");
            assert!(matches!(&error, RuntimeError::ResourceLimit(_)), "{error}");
            assert!(error.to_string().contains("8 haps"), "{error}");
            assert_eq!(session.generation(), 0);
        }
    }
    mod pointer {
        use super::*;

        /// The `n` of every onset a two-second play schedules.
        fn played_ns(session: &mut Session) -> Vec<serde_json::Value> {
            let report = session.play(2.0).expect("play");
            assert!(!report.onsets.is_empty(), "nothing played");
            report
                .onsets
                .iter()
                .map(|onset| match &onset.value {
                    ValueJson::Raw(value) => value["n"].clone(),
                    other => panic!("an onset without controls: {other:?}"),
                })
                .collect()
        }

        /// A Session built with a pointer plays the position the host set, read
        /// when the score is queried rather than when it is evaluated.
        #[test]
        fn a_session_with_a_pointer_plays_the_position_set() {
            let pointer = Pointer::default();
            let mut session =
                Session::with_config(SessionConfig::default().with_pointer(pointer.clone()))
                    .expect("session");
            session
                .evaluate("n(mousex.segment(2)).s('sine')")
                .expect("evaluate");
            for position in [0.25, 0.75] {
                pointer.x.set(position);
                let ns = played_ns(&mut session);
                assert!(ns.iter().all(|n| *n == position), "{position}: {ns:?}");
            }
        }
    }

    fn active_values(session: &Session) -> Vec<String> {
        session
            .js
            .query(Slot::Active, 0, Fraction::ZERO, Fraction::ONE)
            .expect("active graph remains queryable")
            .into_iter()
            .map(|hap| hap.value.show())
            .collect()
    }

    /// Long enough for Windows to finish refusing a closed loopback port.
    /// `wait_until_idle` returns immediately when work settles, so this is a
    /// deadline rather than an unconditional delay.
    const LOOPBACK_REFUSAL_DEADLINE: Duration = Duration::from_secs(15);

    const QUERY_SPIN_SOURCE: &str = r#"
      globalThis.__querySpin = true;
      new Pattern(state => {
        globalThis.__queryCalls = (globalThis.__queryCalls || 0) + 1;
        if (globalThis.__querySpin) {
          while (true) {}
        }
        return pure('recovered').query(state);
      })
    "#;

    fn set_query_spin(session: &Session, spin: bool) {
        session
            .js
            .eval(if spin {
                "globalThis.__querySpin = true"
            } else {
                "globalThis.__querySpin = false"
            })
            .expect("set query spin flag");
    }

    #[test]
    fn score_tempo_is_one_session_scheduler_and_report_commit() {
        let mut session = Session::new().expect("session");

        let before = session.generation();
        session
            .evaluate(
                "setcps(2); new Pattern(state => \
                 pure(state.controls._cps).query(state))",
            )
            .expect("score with in-file tempo and scheduler-control probe");
        assert_eq!(
            session.generation(),
            before + 1,
            "tempo and pattern committed as separate generations"
        );
        assert_eq!(session.config().cps, 2.0);
        assert_eq!(session.scheduler.cps(), 2.0);

        let control_probe = session.schedule_at(0.0).expect("schedule cps probe");
        assert_eq!(control_probe.len(), 1);
        assert_eq!(
            control_probe[0].value_show, "2",
            "scheduler query did not receive the committed `_cps`"
        );
        assert_eq!(control_probe[0].duration_secs, 0.5);

        session
            .evaluate("kick: s('bd*4')\ntempochanges: cps(1).gain(0)")
            .expect("a labeled cps lane");
        assert_eq!(session.scheduler.cps(), 1.0);
        assert_eq!(session.config().cps, 1.0);

        let before = session.generation();
        session
            .evaluate(r#"setcps(1); setcpm(120); note("c4 e4")"#)
            .expect("last tempo directive wins");
        assert_eq!(session.generation(), before + 1);
        assert_eq!(session.config().cps, 2.0, "last tempo call did not win");
        assert_eq!(session.scheduler.cps(), 2.0);

        let before = session.generation();
        session
            .evaluate(r#"note("d4 f4")"#)
            .expect("later score without a tempo directive");
        assert_eq!(session.generation(), before + 1);
        assert_eq!(
            session.config().cps,
            2.0,
            "a score without a tempo directive reset Session cps"
        );
        assert_eq!(session.scheduler.cps(), 2.0);

        let play = session.play(0.3).expect("play at committed score tempo");
        assert_eq!(play.cps, 2.0);
        assert!(
            play.onsets.len() >= 2,
            "two-step score produced too few onsets"
        );
        assert!((play.onsets[0].target_time - 0.0).abs() < 1e-9);
        assert!((play.onsets[1].target_time - 0.25).abs() < 1e-9);
        assert!((play.onsets[0].duration_secs - 0.25).abs() < 1e-9);
        assert!((play.onsets[1].duration_secs - 0.25).abs() < 1e-9);

        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock")
            .as_nanos();
        let output = std::env::temp_dir().join(format!(
            "rustel-in-file-tempo-{}-{unique}.json",
            std::process::id()
        ));
        let render = session
            .render(0.3, &output, RenderFormat::OnsetJson)
            .expect("render at committed score tempo");
        assert_eq!(render.cps, 2.0);
        assert!(render.onset_count >= 2);
        std::fs::remove_file(output).expect("remove owned tempo onset dump");
    }

    #[test]
    fn pattern_cpm_matches_the_same_session_cpm_at_a_non_one_default() {
        let mut local = Session::new().expect("local-tempo session");
        local
            .evaluate(r#"note("c4*4").cpm(124.5/4)"#)
            .expect("pattern-local cpm");

        let mut global = Session::new().expect("global-tempo session");
        global
            .evaluate(r#"setcpm(124.5/4); note("c4*4")"#)
            .expect("session cpm");

        let local = local.play(2.0).expect("play local tempo");
        let global = global.play(2.0).expect("play global tempo");
        let local_times = local
            .onsets
            .iter()
            .map(|onset| onset.target_time)
            .collect::<Vec<_>>();
        let global_times = global
            .onsets
            .iter()
            .map(|onset| onset.target_time)
            .collect::<Vec<_>>();
        assert_eq!(local_times.len(), global_times.len());
        for (local, global) in local_times.iter().zip(global_times) {
            assert!((local - global).abs() < 1e-9, "{local} != {global}");
        }
    }

    #[test]
    fn invalid_audio_controls_never_replace_the_last_good_graph() {
        let replace = |session: &mut Session, source: &str| {
            session.evaluate_at_cancellable(
                source,
                0.0,
                false,
                SCORE_CPU_BUDGET,
                &NEVER_CANCELLED,
                NoPatternPolicy::InstallSilence,
                true,
            )
        };
        let mut session = Session::new().expect("session");
        session
            .evaluate(r#"note("c3").s("supersaw").lpenv(1)"#)
            .expect("valid first graph");
        session
            .evaluate(
                r#"s("supersaw").seg(8).note("c4")
                    .sometimesBy(1, x => x.transpose(rand.range(14,24)))"#,
            )
            .expect("a fractional transpose follows the synth's falsy-note fallback");
        let generation = session.generation();
        let before = active_values(&session);

        let error = replace(
            &mut session,
            r#"note("c3").s("supersaw").lpf(1000).lpenv("...")"#,
        )
        .expect_err("invalid lpenv must be rejected before commit");
        assert!(error.to_string().contains("lpenv must be a finite number"));
        assert_eq!(session.generation(), generation);
        assert_eq!(active_values(&session), before);

        let error = replace(
            &mut session,
            r#"s("sd:3")
                    .seg(8)
                    .struct("~ x ~ x")
                    .sometimesBy(1, x => x.plyWith(4, (i, n) =>
                        x.velocity(Math.pow(0.4, i))
                    ))"#,
        )
        .expect_err("a NaN velocity must be reported before JSON turns it into null");
        assert!(
            error
                .to_string()
                .contains("velocity must be a finite number"),
            "the refusal names the control that produced NaN: {error}"
        );
        assert_eq!(session.generation(), generation);
        assert_eq!(active_values(&session), before);
    }

    #[test]
    fn rejected_tempo_effects_are_atomic_and_never_laundered_through_mini() {
        let mut session = Session::new().expect("session");
        session
            .evaluate("setcps(1); note('c4')")
            .expect("initial score tempo");
        let generation = session.generation();
        let source = session.last_source.clone();
        let values = active_values(&session);

        let thrown = session
            .evaluate("setcps(3); throw new Error('score failed')")
            .expect_err("a later throw must reject its staged tempo");
        assert_eq!(thrown.kind(), "evaluation");
        assert_eq!(session.generation(), generation);
        assert_eq!(session.config().cps, 1.0);
        assert_eq!(session.scheduler.cps(), 1.0);
        assert_eq!(session.last_source, source);
        assert_eq!(active_values(&session), values);

        session
            .evaluate_prebake(
                "globalThis.__tempoOriginalS = s; \
                 globalThis.s = (...args) => { \
                   setcps(0); \
                   return __tempoOriginalS(...args); \
                 };",
            )
            .expect("install invalid-tempo score helper");
        let invalid = session
            .evaluate("s('sd')")
            .expect_err("invalid score tempo must not become mini fallback");
        assert_eq!(invalid.kind(), "evaluation");
        assert_eq!(session.generation(), generation);
        assert_eq!(session.config().cps, 1.0);
        assert_eq!(session.scheduler.cps(), 1.0);
        assert_eq!(active_values(&session), values);

        session
            .evaluate_prebake(
                "globalThis.s = (...args) => { \
                   setcps(4); \
                   throw new Error('ordinary score failure'); \
                 };",
            )
            .expect("install ordinary-failure score helper");
        session
            .evaluate("s('sd')")
            .expect("ordinary JavaScript failure may use the mini fallback");
        assert_eq!(session.last_evaluate_source(), EvaluateSource::MiniRust);
        assert_eq!(session.generation(), generation + 1);
        assert_eq!(
            session.config().cps,
            1.0,
            "a failed JavaScript candidate leaked tempo into mini fallback"
        );
        assert_eq!(session.scheduler.cps(), 1.0);
        assert_eq!(active_values(&session), ["sd"]);
    }

    #[test]
    fn accepted_host_effects_apply_once_after_graph_and_tempo_commit() {
        let mut session = session_with_sample_origin("http://127.0.0.1:9");
        session.samples = Some(Arc::new(crate::samples::SampleLibrary::empty()));

        session
            .evaluate(
                "samples({ accepted: ['http://127.0.0.1:9/accepted.wav'] }); \
                 preload('accepted'); setCps(1.25); note('c4')",
            )
            .expect("score with host effects");

        assert_eq!(session.scheduler.cps(), 1.25);
        assert_eq!(session.config.cps, 1.25);
        assert_eq!(active_values(&session), ["note:c4"]);

        let library = session
            .samples
            .as_ref()
            .expect("test sample library")
            .clone();
        library.wait_until_idle(LOOPBACK_REFUSAL_DEADLINE);
        assert!(
            library.knows("accepted"),
            "sample registration did not follow graph publication"
        );
        assert_eq!(
            library
                .take_failures()
                .iter()
                .filter(|failure| failure.contains("accepted.wav"))
                .count(),
            1,
            "the preload did not use its same-transaction sample registration exactly once"
        );

        session
            .evaluate("note('d4')")
            .expect("later effect-free score");
        library.wait_until_idle(Duration::from_millis(100));
        assert!(library.take_failures().is_empty());
    }

    #[test]
    fn accepted_setup_effects_apply_once_without_changing_the_score() {
        let mut session = session_with_sample_origin("http://127.0.0.1:9");
        session.samples = Some(Arc::new(crate::samples::SampleLibrary::empty()));
        session
            .evaluate("setCps(1.25); note('c4')")
            .expect("initial score");
        let generation = session.generation();
        let cps = session.scheduler.cps();
        let active = active_values(&session);

        session
            .evaluate_prebake(
                "samples({ setupAccepted: ['http://127.0.0.1:9/setup.wav'] }); \
                 preload('setupAccepted'); globalThis.setupEffectsApplied = 1;",
            )
            .expect("setup with host effects");

        let library = session
            .samples
            .as_ref()
            .expect("test sample library")
            .clone();
        library.wait_until_idle(LOOPBACK_REFUSAL_DEADLINE);
        assert!(library.knows("setupAccepted"));
        assert_eq!(
            library
                .take_failures()
                .iter()
                .filter(|failure| failure.contains("setup.wav"))
                .count(),
            1
        );
        assert_eq!(session.generation(), generation);
        assert_eq!(session.scheduler.cps(), cps);
        assert_eq!(active_values(&session), active);
        assert_eq!(session.js.get_number("setupEffectsApplied"), Some(1.0));

        session
            .evaluate_prebake("globalThis.effectFreeSetup = 1;")
            .expect("later effect-free setup");
        library.wait_until_idle(Duration::from_millis(100));
        assert!(library.take_failures().is_empty());
        assert_eq!(session.generation(), generation);
        assert_eq!(session.scheduler.cps(), cps);
        assert_eq!(active_values(&session), active);
    }

    /// A pattern the setup builds carries the setup's byte offsets. Those
    /// offsets must not reach the score's document, which is a different
    /// text.
    #[test]
    fn prebake_patterns_do_not_impersonate_score_source_locations() {
        let mut session = Session::new().expect("session");
        session
            .evaluate_prebake(
                r#"
                    globalThis.fromSetup = () => stack(
                        note("c4 e4"),
                        note("g4 a4"),
                        pure('setup'),
                    );
                "#,
            )
            .expect("setup helper");
        session.evaluate("$: fromSetup()").expect("score");

        let haps = session
            .query(Fraction::ZERO, Fraction::ONE)
            .expect("query setup-created pattern");
        assert!(!haps.is_empty());
        assert!(
            haps.iter().all(|hap| hap.context.is_empty()),
            "prebake byte offsets leaked into the score document: {haps:?}"
        );
    }

    #[test]
    fn cancellation_after_setup_staging_discards_the_whole_transaction() {
        use std::sync::atomic::Ordering;

        let mut session = Session::new().expect("session");
        session.samples = Some(Arc::new(crate::samples::SampleLibrary::empty()));
        session.evaluate("note('c4')").expect("initial score");
        let generation = session.generation();
        let active = active_values(&session);
        let cancellation = Arc::new(AtomicBool::new(false));
        let request = {
            let cancellation = cancellation.clone();
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(40));
                cancellation.store(true, Ordering::Relaxed);
            })
        };

        let error = session
            .evaluate_prebake_cancellable(
                "samples({ cancelledSetup: ['http://127.0.0.1:9/cancelled.wav'] }); \
                 preload('cancelledSetup'); globalThis.cancelledSetupPrefix = 1; \
                 while (true) {}",
                cancellation.as_ref(),
            )
            .expect_err("caller cancellation must stop setup after staging");
        request.join().expect("cancellation requester");
        assert!(matches!(error, RuntimeError::Cancelled));
        assert_eq!(session.js.get_number("cancelledSetupPrefix"), Some(1.0));
        assert_eq!(session.generation(), generation);
        assert_eq!(active_values(&session), active);
        let library = session.samples.as_ref().expect("test sample library");
        assert!(!library.knows("cancelledSetup"));
        assert_eq!(session.preload_requested, 0);

        session
            .evaluate_prebake("globalThis.afterCancelledSetup = 1;")
            .expect("setup recovery after cancellation");
        let library = session.samples.as_ref().expect("test sample library");
        assert!(!library.knows("cancelledSetup"));
        assert_eq!(session.preload_requested, 0);
    }

    #[test]
    fn rejected_effects_do_not_leak_into_a_later_score_or_mini_fallback() {
        let mut session = Session::new().expect("session");
        session.samples = Some(Arc::new(crate::samples::SampleLibrary::empty()));
        session.evaluate("note('c4')").expect("last-good score");
        session
            .evaluate_prebake(
                "globalThis.effectSamples = globalThis.samples; \
                 globalThis.effectPreload = globalThis.preload; \
                 globalThis.s = (...args) => { \
                   effectSamples({ fallbackLeak: ['http://127.0.0.1:9/fallback.wav'] }); \
                   effectPreload('fallbackLeak'); \
                   throw new Error('ordinary score failure'); \
                 };",
            )
            .expect("install failing compatibility helper");

        session
            .evaluate("s('sd')")
            .expect("ordinary JavaScript failure may use Mini fallback");
        assert_eq!(session.last_evaluate_source(), EvaluateSource::MiniRust);
        assert_eq!(active_values(&session), ["sd"]);
        let library = session.samples.as_ref().expect("test sample library");
        assert!(!library.knows("fallbackLeak"));
        assert_eq!(session.preload_requested, 0);

        let rejected = session
            .evaluate(
                "effectSamples({ rejectedLeak: ['http://127.0.0.1:9/rejected.wav'] }); \
                 effectPreload('rejectedLeak'); throw new Error('reject effects')",
            )
            .expect_err("failed score with effects");
        assert!(rejected.to_string().contains("reject effects"));
        session
            .evaluate("note('e4')")
            .expect("effect-free recovery score");
        let library = session.samples.as_ref().expect("test sample library");
        assert!(!library.knows("rejectedLeak"));
        assert_eq!(session.preload_requested, 0);
    }

    #[test]
    fn live_probe_rejects_query_time_effects_and_restores_last_good() {
        let mut session = Session::new().expect("session");
        session.samples = Some(Arc::new(crate::samples::SampleLibrary::empty()));
        session.evaluate("note('c4')").expect("last-good score");
        let generation = session.generation();
        session
            .evaluate_prebake(
                "globalThis.effectSamples = globalThis.samples; \
                 globalThis.effectPreload = globalThis.preload;",
            )
            .expect("save effect bindings");

        let error = session
                .reload_at(
                    "pure('candidate').fmap(value => { \
                   try { effectSamples({ probeLeak: ['http://127.0.0.1:9/probe.wav'] }); } catch (_) {} \
                   try { effectPreload('probeLeak'); } catch (_) {} \
                   return value; \
                 })",
                    false,
                    0.25,
                )
                .expect_err("query-time effect must refuse replacement");
        assert_eq!(error.kind(), "evaluation");
        assert!(error.to_string().contains("host effect"));
        assert_eq!(session.generation(), generation);
        assert_eq!(active_values(&session), ["note:c4"]);
        let library = session.samples.as_ref().expect("test sample library");
        assert!(!library.knows("probeLeak"));
        assert_eq!(session.preload_requested, 0);
    }

    #[test]
    fn live_probe_rejects_policy_plus_pending_jobs_and_discards_effects() {
        let mut session = session_with_sample_origin("http://127.0.0.1:9");
        session.samples = Some(Arc::new(crate::samples::SampleLibrary::empty()));
        session
            .evaluate(
                "(() => { \
                   let calls = 0; \
                   return pure('last-good').fmap(value => `${value}:${++calls}`); \
                 })()",
            )
            .expect("callback-bearing last-good score");
        assert_eq!(active_values(&session), ["last-good:1"]);
        let generation = session.generation();
        session
            .evaluate_prebake(
                "globalThis.probeEffectSamples = samples; \
                 globalThis.probeEffectPreload = preload;",
            )
            .expect("retain effect bindings");

        let error = session
            .reload_at(
                "probeEffectSamples({ stagedLeak: ['http://127.0.0.1:9/staged.wav'] }); \
                 probeEffectPreload('stagedLeak'); \
                 pure('candidate').fmap(value => { \
                   try { \
                     probeEffectSamples({ queryLeak: ['http://127.0.0.1:9/query.wav'] }); \
                   } catch (_) {} \
                   Promise.resolve().then(() => { globalThis.probeJobRan = 1; }); \
                   return value; \
                 })",
                false,
                0.25,
            )
            .expect_err("pending query work must not hide a host-effect refusal");
        assert_eq!(error.kind(), "resource-limit");
        assert!(error.to_string().contains("runnable JavaScript jobs"));
        assert_eq!(session.generation(), generation);
        assert_eq!(active_values(&session), ["last-good:2"]);
        let library = session.samples.as_ref().expect("test sample library");
        assert!(!library.knows("stagedLeak"));
        assert!(!library.knows("queryLeak"));
        assert_eq!(session.preload_requested, 0);
        assert_eq!(session.js.get_number("probeJobRan"), None);
        assert!(!session.js.jobs_pending(), "rejected probe kept its job");

        session
            .reload_at("note('e4')", false, 0.5)
            .expect("recovery after pending-job probe refusal");
        assert_eq!(active_values(&session), ["note:e4"]);
    }

    #[test]
    fn live_probe_rejects_policy_even_when_callback_hits_the_js_deadline() {
        let mut session = session_with_sample_origin("http://127.0.0.1:9");
        session.samples = Some(Arc::new(crate::samples::SampleLibrary::empty()));
        session
            .evaluate(
                "(() => { \
                   let calls = 0; \
                   return pure('last-good').fmap(value => `${value}:${++calls}`); \
                 })()",
            )
            .expect("callback-bearing last-good score");
        assert_eq!(active_values(&session), ["last-good:1"]);
        let generation = session.generation();
        session
            .evaluate_prebake("globalThis.deadlineEffectSamples = samples;")
            .expect("retain sample effect binding");

        let error = session
            .reload_at(
                "pure('candidate').fmap(value => { \
                   try { \
                     deadlineEffectSamples({ deadlineLeak: ['http://127.0.0.1:9/deadline.wav'] }); \
                   } catch (_) {} \
                   while (true) {} \
                   return value; \
                 })",
                false,
                0.25,
            )
            .expect_err("a JavaScript deadline must not hide a host-effect refusal");
        assert_eq!(error.kind(), "evaluation");
        assert!(error.to_string().contains("host effect"));
        assert_eq!(session.generation(), generation);
        assert_eq!(active_values(&session), ["last-good:2"]);
        let library = session.samples.as_ref().expect("test sample library");
        assert!(!library.knows("deadlineLeak"));
        assert!(!session.js.jobs_pending());

        session
            .reload_at("note('f4')", false, 0.5)
            .expect("recovery after combined policy/deadline refusal");
        assert_eq!(active_values(&session), ["note:f4"]);
    }

    #[test]
    fn direct_query_deadline_and_stop_are_bounded_and_same_session_recovers() {
        assert_eq!(
            QUERY_JS_CPU_BUDGET,
            Duration::from_secs(2),
            "the Session query-time JavaScript ceiling changed"
        );
        let mut session = Session::new().expect("session");
        session
            .evaluate(QUERY_SPIN_SOURCE)
            .expect("evaluate query-time runaway");
        assert!(
            session.active_needs_host(),
            "a user-authored Pattern query must take the impure host route"
        );

        let tiny_budget = Duration::from_millis(40);
        let started = Instant::now();
        let deadline = session
            .query_with_js_budget(Fraction::ZERO, Fraction::ONE, tiny_budget)
            .expect_err("runaway query callback must hit its QuickJS deadline");
        assert!(
            matches!(deadline, RuntimeError::ResourceLimit(_)),
            "query deadline lost the resource-limit channel: {deadline:?}"
        );
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "40ms query deadline returned too late: {:?}",
            started.elapsed()
        );
        assert_eq!(session.js.query_depth(), 0, "deadline leaked query stack");
        assert_eq!(
            rustel_jsruntime::bridge_frame_depth(),
            0,
            "deadline leaked bridge frame"
        );

        set_query_spin(&session, false);
        let recovered = session
            .query_with_js_budget(Fraction::ZERO, Fraction::ONE, tiny_budget)
            .expect("same-session query after deadline");
        assert_eq!(recovered.len(), 1);
        assert_eq!(recovered[0].value.show(), "recovered");

        set_query_spin(&session, true);
        let transport = session.transport();
        let stopper = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(25));
            transport.stop();
        });
        let started = Instant::now();
        let cancelled = session
            .query_with_js_budget(Fraction::ZERO, Fraction::ONE, Duration::from_secs(2))
            .expect_err("Stop must interrupt a running JavaScript query");
        stopper.join().expect("stopper thread");
        assert!(
            matches!(cancelled, RuntimeError::Cancelled),
            "query Stop lost cancellation identity: {cancelled:?}"
        );
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "query Stop waited for the two-second deadline: {:?}",
            started.elapsed()
        );

        session.transport.start();
        set_query_spin(&session, false);
        let recovered_again = session
            .query_with_js_budget(Fraction::ZERO, Fraction::ONE, tiny_budget)
            .expect("same-session query after cancellation");
        assert_eq!(recovered_again.len(), 1);
        assert_eq!(recovered_again[0].value.show(), "recovered");
    }

    #[test]
    fn play_uses_the_same_query_time_js_boundary_and_recovers() {
        let mut session = Session::new().expect("session");
        session
            .evaluate(QUERY_SPIN_SOURCE)
            .expect("evaluate query-time runaway");
        let tiny_budget = Duration::from_millis(40);
        let refused = session
            .play_with_js_budget(1.0, tiny_budget)
            .expect_err("play must bound its impure scheduler tick");
        assert!(
            matches!(refused, RuntimeError::ResourceLimit(_)),
            "play query deadline lost the resource-limit channel: {refused:?}"
        );
        assert_eq!(session.js.query_depth(), 0, "play leaked query stack");
        assert_eq!(
            rustel_jsruntime::bridge_frame_depth(),
            0,
            "play leaked bridge frame"
        );

        set_query_spin(&session, false);
        let report = session
            .play_with_js_budget(1.0, tiny_budget)
            .expect("same-session play after query refusal");
        assert!(
            report
                .onsets
                .iter()
                .any(|onset| onset.value_show == "recovered"),
            "recovered play scheduled no onset: {:?}",
            report.onsets
        );
    }

    #[test]
    fn stop_inside_a_javascript_play_tick_keeps_partial_success_semantics() {
        let mut session = Session::new().expect("session");
        session
            .evaluate(QUERY_SPIN_SOURCE)
            .expect("evaluate query-time runaway");
        let transport = session.transport();
        let stopper = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(30));
            transport.stop();
        });
        let started = Instant::now();
        let report = session
            .play_with_js_budget(1.0, Duration::from_secs(2))
            .expect("Stop during play keeps the already-produced partial report");
        stopper.join().expect("stopper thread");
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "play waited for the query deadline instead of Stop: {:?}",
            started.elapsed()
        );
        assert!(
            report.onsets.is_empty(),
            "the interrupted first tick published partial events: {:?}",
            report.onsets
        );
        assert_eq!(session.js.query_depth(), 0, "Stop leaked query stack");
    }

    #[test]
    fn stop_visible_at_an_impure_tick_preflight_keeps_partial_success_semantics() {
        let outcome = play_tick_or_stopped(Err(RuntimeError::Cancelled))
            .expect("a preflight Stop is not an evaluation failure");
        assert!(
            outcome.is_none(),
            "a preflight Stop must end play without another scheduler tick"
        );

        let resource = play_tick_or_stopped(Err(RuntimeError::ResourceLimit("deadline".into())))
            .expect_err("a real query refusal must not be converted to Stop");
        assert!(matches!(resource, RuntimeError::ResourceLimit(_)));
    }

    #[test]
    fn query_deadline_is_not_an_ordinary_throw_and_pure_ticks_open_no_js_budget() {
        let tiny_budget = Duration::from_millis(40);
        let mut throwing = Session::new().expect("throwing session");
        throwing
            .evaluate("new Pattern(() => { throw new Error('ordinary query throw'); })")
            .expect("evaluate throwing query callback");
        let haps = throwing
            .query_with_js_budget(Fraction::ZERO, Fraction::ONE, tiny_budget)
            .expect("an ordinary strudel.cc query throw becomes silence");
        assert!(haps.is_empty(), "ordinary query throw produced haps");

        let mut pure = Session::new().expect("pure session");
        pure.evaluate("pure('native')")
            .expect("evaluate pure graph");
        assert!(
            !pure.active_needs_host(),
            "pure negative control unexpectedly needs QuickJS"
        );
        let events = pure
            .schedule_through_with_js_budget(0.0, 0.0, Duration::ZERO)
            .expect("pure scheduler tick must not enter the JS deadline boundary");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].value_show, "native");
        assert_eq!(pure.js.query_depth(), 0, "pure tick opened query stack");
        assert_eq!(
            rustel_jsruntime::bridge_frame_depth(),
            0,
            "pure tick opened bridge frame"
        );
    }

    #[test]
    fn scheduler_query_refusal_is_atomic_and_same_now_retry_fills_the_gap() {
        let mut session = Session::new().expect("session");
        session
            .evaluate(
                r#"
                  globalThis.__schedulerQuerySpin = false;
                  pure('tick').fast(8).fmap(value => {
                    if (globalThis.__schedulerQuerySpin) {
                      while (true) {}
                    }
                    return value;
                  })
                "#,
            )
            .expect("evaluate scheduler value callback");
        assert!(session.active_needs_host(), "fixture must enter QuickJS");

        let tiny_budget = Duration::from_millis(40);
        let initial = session
            .schedule_through_with_js_budget(0.0, 0.0, tiny_budget)
            .expect("initial horizon fill");
        assert_eq!(initial.len(), 1, "fixture should drain only the zero onset");
        assert_eq!(initial[0].target_time, 0.0);
        let queued_before = session.scheduler.queued();
        assert!(
            queued_before > 0,
            "fixture left no queued future onset to protect"
        );

        let retry_now = 0.3;
        let horizon_before = session.scheduler.horizon_remaining(retry_now);
        assert!(
            horizon_before > 0.0 && horizon_before < session.config.horizon,
            "fixture did not leave a partially drained horizon: {horizon_before}"
        );
        session
            .js
            .eval("globalThis.__schedulerQuerySpin = true")
            .expect("arm scheduler runaway");
        let refused = session
            .schedule_through_with_js_budget(retry_now, retry_now, tiny_budget)
            .expect_err("runaway scheduler callback must refuse its tick");
        assert!(
            matches!(refused, RuntimeError::ResourceLimit(_)),
            "scheduler deadline lost the resource-limit channel: {refused:?}"
        );
        assert!(
            session.scheduler.refusal().is_some(),
            "scheduler did not retain the typed refusal"
        );
        assert_eq!(
            session.scheduler.queued(),
            queued_before,
            "refused tick changed the existing event queue"
        );
        assert!(
            (session.scheduler.horizon_remaining(retry_now) - horizon_before).abs() < 1e-12,
            "refused tick advanced the query cursor"
        );

        session
            .js
            .eval("globalThis.__schedulerQuerySpin = false")
            .expect("disarm scheduler runaway");
        let through = retry_now + session.config.horizon;
        let recovered = session
            .schedule_through_with_js_budget(retry_now, through, tiny_budget)
            .expect("same-now scheduler retry");
        assert!(
            recovered
                .iter()
                .any(|event| (event.target_time - 0.5).abs() < 1e-9),
            "same-now retry did not fill the onset skipped by the refusal: {recovered:?}"
        );
        let expected_first_id = initial[0].onset_id + 1;
        assert_eq!(
            recovered.first().map(|event| event.onset_id),
            Some(expected_first_id),
            "refused tick consumed an onset id"
        );
        assert!(
            recovered
                .windows(2)
                .all(|pair| pair[1].onset_id == pair[0].onset_id + 1),
            "onset ids changed across the refused tick: {recovered:?}"
        );
    }

    #[test]
    fn scheduler_query_jobs_are_refused_before_cursor_or_queue_commit() {
        let mut session = Session::new().expect("session");
        session
            .evaluate(
                r#"
                  globalThis.__schedulerQueryQueueJob = true;
                  new Pattern(state => {
                    if (globalThis.__schedulerQueryQueueJob) {
                      queueMicrotask(() => {
                        globalThis.__schedulerQueryJobRan = 1;
                      });
                    }
                    return pure('job').fast(8).query(state);
                  })
                "#,
            )
            .expect("evaluate scheduler pending-job fixture");
        assert!(session.active_needs_host(), "fixture must enter QuickJS");

        let now = 0.0;
        let queued_before = session.scheduler.queued();
        let horizon_before = session.scheduler.horizon_remaining(now);
        let error = session
            .schedule_through_with_js_budget(now, now, Duration::from_millis(100))
            .expect_err("a scheduler callback may not leave a runnable job");
        assert!(
            matches!(error, RuntimeError::ResourceLimit(_)),
            "pending query job lost the resource-limit channel: {error:?}"
        );
        assert!(
            error.to_string().contains("runnable JavaScript jobs"),
            "pending query job lost its typed reason: {error}"
        );
        assert_eq!(
            session.scheduler.queued(),
            queued_before,
            "pending-job refusal changed the scheduler queue"
        );
        assert_eq!(
            session.scheduler.horizon_remaining(now),
            horizon_before,
            "pending-job refusal advanced the scheduler cursor"
        );
        assert_eq!(
            session.js.get_number("__schedulerQueryJobRan"),
            None,
            "refused query job was executed"
        );
        assert!(
            !session.js.jobs_pending(),
            "refused query job remained runnable"
        );

        session
            .js
            .eval("globalThis.__schedulerQueryQueueJob = false")
            .expect("disable pending-job fixture");
        let recovered = session
            .schedule_through_with_js_budget(now, now, Duration::from_millis(100))
            .expect("same-now retry after pending-job refusal");
        assert_eq!(recovered.len(), 1, "retry did not emit the refused onset");
        assert_eq!(recovered[0].onset_id, 0, "refusal consumed an onset id");
        assert_eq!(recovered[0].target_time, 0.0);
        assert_eq!(recovered[0].value_show, "job");
        assert_eq!(session.js.get_number("__schedulerQueryJobRan"), None);
    }

    #[test]
    fn live_query_budget_uses_tempo_aware_prefill_remaining_horizon_floor_and_cap() {
        let reserve = Duration::from_millis(10);
        let mut initial = Session::new().expect("initial session");
        initial.evaluate("note('c4')").expect("initial score");

        assert_eq!(
            initial
                .live_query_budget_at(0.0, reserve, LiveQueryBudgetMode::InitialPrefill)
                .expect("valid initial prefill clock"),
            Some(QUERY_JS_CPU_BUDGET),
            "cold startup lost its separate fixed ceiling"
        );
        assert_eq!(
            initial
                .live_query_budget_at(0.0, reserve, LiveQueryBudgetMode::ReplacementPrefill)
                .expect("valid replacement prefill clock"),
            Some(LIVE_QUERY_RECOVERY_BUDGET),
            "slow-tempo replacement prefill bypassed its bounded recovery cap"
        );
        assert_eq!(
            initial
                .live_query_budget_at(
                    0.0,
                    Duration::from_secs(3),
                    LiveQueryBudgetMode::ReplacementPrefill,
                )
                .expect("large replacement reserve"),
            Some(QUERY_JS_CPU_BUDGET),
            "replacement prefill bypassed the two-second maximum"
        );

        let mut moderate = Session::new().expect("moderate-tempo session");
        moderate
            .evaluate("setCpm(900/4)\nnote('c4')")
            .expect("moderate-tempo score");
        assert!((moderate.cps() - 3.75).abs() < 1e-9);
        assert_eq!(
            moderate
                .live_query_budget_at(0.0, reserve, LiveQueryBudgetMode::ReplacementPrefill)
                .expect("moderate replacement prefill clock"),
            Some(Duration::from_millis(200)),
            "replacement budget did not retain one-quarter-cycle producer headroom"
        );

        let mut extreme = Session::new().expect("extreme-tempo session");
        extreme
            .evaluate("setcps(500)\nnote('c4')")
            .expect("extreme-tempo score");
        assert_eq!(
            extreme
                .live_query_budget_at(0.0, reserve, LiveQueryBudgetMode::ReplacementPrefill)
                .expect("extreme replacement prefill clock"),
            Some(MIN_LIVE_QUERY_JS_BUDGET),
            "an impossible high-CPS replacement escaped the minimum bounded slice"
        );

        let mut session = Session::new().expect("steady session");
        session.evaluate("note('c4')").expect("steady score");
        session
            .schedule_through_with_js_budget(0.0, 0.0, Duration::from_millis(100))
            .expect("fill the default horizon");
        let remaining = session
            .live_query_budget_at(0.2, reserve, LiveQueryBudgetMode::Steady)
            .expect("valid steady clock")
            .expect("partly drained horizon should afford a steady query");
        assert!(
            remaining.abs_diff(Duration::from_millis(270)) <= Duration::from_micros(1),
            "steady budget used nominal horizon instead of the 300ms remaining: {remaining:?}"
        );
        assert!(
            session
                .live_query_budget_at(
                    0.4719,
                    Duration::from_millis(1),
                    LiveQueryBudgetMode::Steady,
                )
                .expect("valid just-above-floor clock")
                .is_some(),
            "a steady query above the 25ms viable slice was refused"
        );
        assert_eq!(
            session
                .live_query_budget_at(
                    0.4721,
                    Duration::from_millis(1),
                    LiveQueryBudgetMode::Steady,
                )
                .expect("valid just-below-floor clock"),
            None,
            "a sub-25ms steady query slice was offered to QuickJS"
        );

        let mut large = Session::with_config(SessionConfig {
            horizon: 10.0,
            ..SessionConfig::default()
        })
        .expect("large-horizon session");
        large.evaluate("note('c4')").expect("large score");
        large
            .schedule_through_with_js_budget(0.0, 0.0, Duration::from_millis(100))
            .expect("fill large horizon");
        assert_eq!(
            large
                .live_query_budget_at(0.0, reserve, LiveQueryBudgetMode::Steady)
                .expect("large valid clock"),
            Some(QUERY_JS_CPU_BUDGET),
            "a large remaining horizon bypassed the live query maximum"
        );

        for invalid in [f64::NAN, f64::INFINITY, -0.001] {
            assert!(
                session
                    .live_query_budget_at(invalid, reserve, LiveQueryBudgetMode::Steady)
                    .is_err(),
                "invalid live query clock {invalid} was accepted"
            );
        }
        assert!(
            session
                .live_query_budget_at(0.0, Duration::ZERO, LiveQueryBudgetMode::Steady)
                .is_err(),
            "zero continuation reserve was accepted"
        );
    }

    #[test]
    #[cfg(feature = "device-audio")]
    fn impure_whole_cycle_live_queries_retain_one_bounded_burst_cycle() {
        let mut high = Session::new().expect("high-CPS session");
        high.evaluate("setCpm(900/4); new Pattern(state => pure('high').query(state))")
            .expect("high-CPS impure score");
        assert!(high.active_needs_host(), "fixture must enter JavaScript");
        let base = high.live_schedule_cover();
        let expected = base + 1.0 / 3.75;
        assert!(
            (high.live_producer_schedule_cover() - expected).abs() < 1e-9,
            "whole-cycle callback score did not receive one burst cycle"
        );

        let mut low = Session::new().expect("low-CPS session");
        low.evaluate("setcps(0.5); new Pattern(state => pure('low').query(state))")
            .expect("low-CPS impure score");
        assert!(low.active_needs_host(), "fixture must enter JavaScript");
        assert_eq!(
            low.live_producer_schedule_cover(),
            low.live_schedule_cover(),
            "sub-cycle queries must keep their exact established windows"
        );

        let mut pure = Session::new().expect("pure session");
        pure.evaluate("setCpm(900/4); pure('native')")
            .expect("high-CPS pure score");
        assert!(!pure.active_needs_host(), "fixture unexpectedly needs JS");
        assert_eq!(
            pure.live_producer_schedule_cover(),
            pure.live_schedule_cover(),
            "native graphs must not pay for a callback burst reserve"
        );
    }

    #[test]
    #[cfg(feature = "device-audio")]
    fn high_cps_impure_live_prefill_is_split_into_deadline_sized_queries() {
        let mut session = Session::new().expect("session");
        session
            .evaluate(
                r#"
                  setCpm(8200/4)
                  register('inspire', (_scale, density, octaves, seed, bars, x) =>
                    x.n(rand.range(0, pure(12).mul(octaves)))
                      .scale(_scale)
                      .sometimesBy(pure(1).sub(density), x => x.mask(rand.round()))
                      .early(rand2.range(-0.001, 0.001))
                      .rib(seed, bars))
                  $: s("sine").seg(8).inspire("<ab:major>", 0.4, 2, 10, 4)
                  $: s("sine").seg(8).inspire("<ab:major>", 0.5, 1, 14, 4)
                  $: s("sine").seg(8).inspire("<ab:major>", 0.9, 1, 14, 2)
                  $: s("sine").seg(8).inspire("<ab:major>", 1, 1/12, 15, 2)
                  $: s("sine").seg(8).inspire("<ab:major>", 1, 1, 70, 4)
                  $: s("sine").seg(16).inspire("<ab:major>", 0.2, 1, 60, 4)
                  $: s("sine").seg(4).inspire("<ab:major>", 1, 2, 42, 2)
                "#,
            )
            .expect("high-CPS score");
        assert!(session.active_needs_host(), "fixture must enter JavaScript");
        // This test pins cycle-sized chunking, not host speed under the Rust
        // test runner's parallel CPU contention. Deadline arithmetic has its
        // own deterministic tests below.
        let query_budget = Duration::from_millis(100);

        let first = session.schedule_audio_live_at(
            0.0,
            48_000,
            query_budget,
            LiveQueryBudgetMode::ReplacementPrefill,
        );
        if let Err(LiveAudioScheduleError::Retryable(error))
        | Err(
            LiveAudioScheduleError::CandidateCommitted(error)
            | LiveAudioScheduleError::CandidateRefusedScore(error),
        ) = first
        {
            panic!("one-cycle replacement prefill was refused: {error}");
        }

        let one_cycle_seconds = 1.0 / session.cps();
        let first_cover = session.scheduler.horizon_remaining(0.0);
        assert!(
            first_cover <= one_cycle_seconds + 1e-6,
            "replacement prefill queried {first_cover:.6}s, more than one cycle ({one_cycle_seconds:.6}s) under one deadline"
        );

        let target_cover = session.live_schedule_cover() - 0.05;
        for _ in 0..32 {
            if session.scheduler.horizon_remaining(0.0) >= target_cover {
                break;
            }
            if let Err(LiveAudioScheduleError::Retryable(error))
            | Err(
                LiveAudioScheduleError::CandidateCommitted(error)
                | LiveAudioScheduleError::CandidateRefusedScore(error),
            ) = session.schedule_audio_live_at(
                0.0,
                48_000,
                query_budget,
                LiveQueryBudgetMode::Steady,
            ) {
                panic!("subsequent high-CPS prefill chunk was refused: {error}");
            }
        }
        assert!(
            session.scheduler.horizon_remaining(0.0) >= target_cover,
            "bounded chunks did not fill the live horizon"
        );
    }

    #[test]
    #[cfg(feature = "device-audio")]
    fn replacement_prefill_can_spend_more_than_25ms_at_a_sustainable_tempo() {
        let mut session = Session::new().expect("session");
        session
            .evaluate(
                r#"
                  setCpm(900/4)
                  note('c4').fmap(value => {
                    const until = Date.now() + 50;
                    while (Date.now() < until) {}
                    return value;
                  })
                "#,
            )
            .expect("bounded replacement fixture");
        assert!((session.cps() - 3.75).abs() < 1e-9);
        assert!(session.active_needs_host(), "fixture must enter JavaScript");
        assert_eq!(
            session
                .live_query_budget_at(
                    0.0,
                    MIN_LIVE_QUERY_JS_BUDGET,
                    LiveQueryBudgetMode::ReplacementPrefill,
                )
                .expect("replacement budget"),
            Some(Duration::from_millis(200))
        );

        let batch = match session.schedule_audio_live_at(
            0.0,
            48_000,
            MIN_LIVE_QUERY_JS_BUDGET,
            LiveQueryBudgetMode::ReplacementPrefill,
        ) {
            Ok(batch) => batch,
            Err(LiveAudioScheduleError::Retryable(error))
            | Err(LiveAudioScheduleError::AwaitingSamples(error))
            | Err(
                LiveAudioScheduleError::CandidateCommitted(error)
                | LiveAudioScheduleError::CandidateRefusedScore(error),
            ) => {
                panic!("sustainable replacement was refused: {error}")
            }
        };
        assert!(
            !batch.events.is_empty(),
            "sustainable replacement scheduled no audio"
        );
        assert!(
            session.scheduler.horizon_remaining(0.0) > 0.0,
            "sustainable replacement did not advance its query cursor"
        );
    }

    #[test]
    #[cfg(feature = "device-audio")]
    fn full_horizon_impure_live_tick_does_not_enter_javascript() {
        let mut session = Session::new().expect("session");
        session
            .evaluate(
                r#"
                  globalThis.__fullHorizonCalls = 0;
                  note('c4').fast(8).fmap(value => {
                    globalThis.__fullHorizonCalls++;
                    return value;
                  })
                "#,
            )
            .expect("impure score");
        session
            .schedule_through_with_js_budget(0.0, 0.0, Duration::from_millis(100))
            .expect("fill horizon once");
        let calls_before = session
            .js
            .get_number("__fullHorizonCalls")
            .expect("callback count after initial fill");

        let result = session.schedule_audio_live_at(
            0.0,
            48_000,
            Duration::from_secs(1),
            LiveQueryBudgetMode::Steady,
        );
        match result {
            Ok(_) => {}
            Err(LiveAudioScheduleError::Retryable(error))
            | Err(LiveAudioScheduleError::AwaitingSamples(error)) => {
                panic!("full horizon was rejected without a query: {error:?}")
            }
            Err(
                LiveAudioScheduleError::CandidateCommitted(error)
                | LiveAudioScheduleError::CandidateRefusedScore(error),
            ) => {
                panic!("full horizon committed a failing candidate: {error:?}")
            }
        }
        assert_eq!(
            session.js.get_number("__fullHorizonCalls"),
            Some(calls_before),
            "HorizonFull entered the impure callback despite needing no fill"
        );
        assert_eq!(session.js.query_depth(), 0, "full-horizon scope leaked");
    }

    #[test]
    #[cfg(feature = "device-audio")]
    fn pure_live_tick_ignores_unaffordable_js_cover_and_remains_host_free() {
        let mut session = Session::new().expect("session");
        session.evaluate("note('c4')").expect("pure score");
        assert!(!session.active_needs_host(), "fixture is not pure");

        let batch = match session.schedule_audio_live_at(
            0.0,
            48_000,
            Duration::from_secs(1),
            LiveQueryBudgetMode::Steady,
        ) {
            Ok(batch) => batch,
            Err(LiveAudioScheduleError::Retryable(error))
            | Err(LiveAudioScheduleError::AwaitingSamples(error)) => {
                panic!("pure live tick required an affordable QuickJS slice: {error:?}")
            }
            Err(
                LiveAudioScheduleError::CandidateCommitted(error)
                | LiveAudioScheduleError::CandidateRefusedScore(error),
            ) => {
                panic!("pure live tick failed after scheduler commit: {error:?}")
            }
        };
        assert_eq!(batch.events.len(), 1);
        assert_eq!(
            session.js.query_depth(),
            0,
            "pure tick opened JS query scope"
        );
        assert_eq!(
            rustel_jsruntime::bridge_frame_depth(),
            0,
            "pure tick opened a bridge frame"
        );
    }

    #[test]
    #[cfg(feature = "device-audio")]
    fn live_ui_trace_keeps_transpiler_byte_ranges_on_audio_onset_ids() {
        let source = r#"$: note("c4 e4")"#;
        let mut session = Session::with_config(SessionConfig {
            cps: 2.0,
            ..SessionConfig::default()
        })
        .expect("session");
        session.evaluate(source).expect("score");
        session.set_schedule_trace_enabled(true);

        let batch = match session.schedule_audio_live_at(
            0.0,
            48_000,
            Duration::from_secs(1),
            LiveQueryBudgetMode::Steady,
        ) {
            Ok(batch) => batch,
            Err(LiveAudioScheduleError::Retryable(error))
            | Err(LiveAudioScheduleError::AwaitingSamples(error))
            | Err(
                LiveAudioScheduleError::CandidateCommitted(error)
                | LiveAudioScheduleError::CandidateRefusedScore(error),
            ) => {
                panic!("live audio schedule failed: {error}")
            }
        };
        let traces = session.take_schedule_trace_events();

        assert_eq!(batch.events.len(), 2);
        assert_eq!(traces.len(), 2);
        assert_eq!(traces[0].context, vec![(9, 11)]);
        assert_eq!(&source.as_bytes()[9..11], b"c4");
        assert_eq!(traces[1].context, vec![(12, 14)]);
        assert_eq!(&source.as_bytes()[12..14], b"e4");
        assert!(batch.events.iter().zip(&traces).all(|(audio, trace)| {
            audio.generation == trace.generation && audio.onset_id == trace.onset_id
        }));
    }

    #[test]
    #[cfg(all(feature = "device-audio", feature = "osc"))]
    fn external_only_replacement_is_renderable_without_a_scalar_voice() {
        let mut session = Session::new().expect("session");
        session
            .evaluate(r#"s("not-a-native-sample").osc(57120).fast(8)"#)
            .expect("external-only score");

        let batch = match session.schedule_audio_live_at(
            0.0,
            48_000,
            Duration::from_secs(1),
            LiveQueryBudgetMode::ReplacementPrefill,
        ) {
            Ok(batch) => batch,
            Err(LiveAudioScheduleError::Retryable(error))
            | Err(LiveAudioScheduleError::AwaitingSamples(error))
            | Err(
                LiveAudioScheduleError::CandidateCommitted(error)
                | LiveAudioScheduleError::CandidateRefusedScore(error),
            ) => {
                panic!("external-only replacement was rejected: {error}")
            }
        };

        assert!(
            batch.events.is_empty(),
            "fixture unexpectedly rendered locally"
        );
        let pending = session.take_pending_osc();
        assert!(!pending.is_empty(), "OSC intent was not retained");
        assert!(pending.iter().all(|(_, intent)| {
            intent.port == 57120
                && intent
                    .destination
                    .is_some_and(|destination| destination.ip().is_loopback())
        }));
    }

    #[test]
    #[cfg(all(feature = "device-audio", feature = "osc"))]
    fn score_chosen_non_loopback_osc_is_dropped_and_reported_once_per_score() {
        let mut session = Session::new().expect("session");
        session.set_direct_diagnostic_logging(false);
        session
            .evaluate(
                r#"stack(
                    s("not-a-native-sample").osc(57120).oschost("10.0.0.5"),
                    s("not-a-native-sample").osc(57120).oschost("10.0.0.6")
                ).fast(8)"#,
            )
            .expect("score");

        let _ = session.schedule_audio_live_at(
            0.0,
            48_000,
            Duration::from_secs(1),
            LiveQueryBudgetMode::ReplacementPrefill,
        );
        let pending = session.take_pending_osc();
        assert!(
            pending.is_empty(),
            "an ungranted LAN destination was queued ({} intents)",
            pending.len()
        );
        let diagnostics = session.take_diagnostics();
        assert_eq!(
            diagnostics
                .iter()
                .filter(|diagnostic| diagnostic.kind == "osc-refused")
                .count(),
            2,
            "one notice per refused destination: {diagnostics:?}"
        );

        // The same score can schedule many dense windows without turning its
        // refused OSC destinations into producer-thread log spam.
        let _ = session.schedule_audio_live_at(
            0.25,
            48_000,
            Duration::from_secs(1),
            LiveQueryBudgetMode::Steady,
        );
        assert!(session.take_pending_osc().is_empty());
        assert!(
            session
                .take_diagnostics()
                .iter()
                .all(|diagnostic| diagnostic.kind != "osc-refused")
        );
    }

    #[test]
    #[cfg(feature = "osc")]
    fn osc_refusal_cache_never_retains_score_sized_host_text() {
        assert_eq!(
            osc_refusal_key(" 10.0.0.5 ", 57120),
            osc_refusal_key("::ffff:10.0.0.5", 57120),
            "equivalent spellings must deduplicate as one destination"
        );
        let mut session = Session::new().expect("session");
        session.set_direct_diagnostic_logging(false);
        let huge_host = "x".repeat(rustel_osc::MAX_OSC_HOST_BYTES + 1024 * 1024);
        let onset = OnsetEventJson {
            onset_id: 1,
            generation: 1,
            whole_begin: "0".into(),
            duration_secs: 0.25,
            target_time: 0.0,
            live_controls: [0; 2],
            ui_visuals: 0,
            value: ValueJson::Raw(serde_json::json!({
                "oscport": 57120,
                "oschost": huge_host,
                "s": "bd"
            })),
            value_show: String::new(),
            log_line: None,
        };

        session.collect_pending_osc(&[onset.clone(), onset], 0.0, 0.5);

        assert_eq!(
            session.reported_osc_refusals,
            vec![ReportedOscRefusal::OversizedHost]
        );
        assert_eq!(
            session
                .take_diagnostics()
                .iter()
                .filter(|diagnostic| diagnostic.kind == "osc-refused")
                .count(),
            1
        );
    }

    #[test]
    #[cfg(feature = "osc")]
    fn one_oversized_osc_onset_does_not_drop_a_later_valid_onset() {
        let onset = |onset_id, value| OnsetEventJson {
            onset_id,
            generation: 1,
            whole_begin: "0".into(),
            duration_secs: 0.25,
            target_time: 0.0,
            live_controls: [0; 2],
            ui_visuals: 0,
            value: ValueJson::Raw(value),
            value_show: String::new(),
            log_line: None,
        };
        let oversized = onset(
            1,
            serde_json::json!({
                "oscport": 57120,
                "s": "x".repeat(rustel_osc::MAX_DATAGRAM_BYTES * 2)
            }),
        );
        let valid = onset(2, serde_json::json!({ "oscport": 57120, "s": "bd" }));
        let mut session = Session::new().expect("session");
        session.set_direct_diagnostic_logging(false);

        session.collect_pending_osc(&[oversized, valid], 0.0, 0.5);

        assert_eq!(session.pending_osc.len(), 1);
        assert_eq!(session.pending_osc[0].1.onset_id, 2);
        assert!(
            session
                .take_diagnostics()
                .iter()
                .all(|diagnostic| !diagnostic.message.contains("per-pass limit"))
        );
    }

    #[test]
    #[cfg(feature = "osc")]
    fn cached_osc_refusals_are_still_bounded_by_the_route_attempt_cap() {
        let onset = OnsetEventJson {
            onset_id: 1,
            generation: 1,
            whole_begin: "0".into(),
            duration_secs: 0.25,
            target_time: 0.0,
            live_controls: [0; 2],
            ui_visuals: 0,
            value: ValueJson::Raw(serde_json::json!({
                "oscport": 57120,
                "oschost": "10.0.0.5",
                "s": "bd"
            })),
            value_show: String::new(),
            log_line: None,
        };
        let onsets = vec![onset; MAX_OSC_ROUTE_ATTEMPTS_PER_PASS + 1];
        let mut session = Session::new().expect("session");
        session.set_direct_diagnostic_logging(false);

        session.collect_pending_osc(&onsets, 0.0, 0.5);

        let diagnostics = session.take_diagnostics();
        assert_eq!(session.reported_osc_refusals.len(), 1);
        assert!(
            diagnostics
                .iter()
                .any(|diagnostic| diagnostic.message.contains("routed onsets"))
        );
    }

    #[test]
    #[cfg(feature = "osc")]
    fn osc_packet_cap_counts_only_valid_osc_intents() {
        let onset = |onset_id| OnsetEventJson {
            onset_id,
            generation: 1,
            whole_begin: "0".into(),
            duration_secs: 0.25,
            target_time: 0.0,
            live_controls: [0; 2],
            ui_visuals: 0,
            value: ValueJson::Raw(serde_json::json!({ "oscport": 57120, "s": "bd" })),
            value_show: String::new(),
            log_line: None,
        };
        let mut onsets = (0..=MAX_OSC_INTENTS_PER_PASS as u64)
            .map(onset)
            .collect::<Vec<_>>();
        // A non-OSC tail after an exactly full batch does not itself claim
        // another OSC packet or trigger a false cap report.
        let mut exactly_full = onsets[..MAX_OSC_INTENTS_PER_PASS].to_vec();
        exactly_full.push(OnsetEventJson {
            onset_id: u64::MAX,
            generation: 1,
            whole_begin: "0".into(),
            duration_secs: 0.25,
            target_time: 0.0,
            live_controls: [0; 2],
            ui_visuals: 0,
            value: ValueJson::Raw(serde_json::json!({ "s": "bd" })),
            value_show: String::new(),
            log_line: None,
        });
        let mut session = Session::new().expect("session");
        session.set_direct_diagnostic_logging(false);
        session.collect_pending_osc(&exactly_full, 0.0, 0.5);
        assert_eq!(session.pending_osc.len(), MAX_OSC_INTENTS_PER_PASS);
        assert!(session.take_diagnostics().is_empty());

        session.collect_pending_osc(&onsets, 0.0, 0.5);
        assert_eq!(session.pending_osc.len(), MAX_OSC_INTENTS_PER_PASS);
        assert!(
            session
                .take_diagnostics()
                .iter()
                .any(|diagnostic| diagnostic.message.contains("per-pass limit"))
        );
        // Avoid retaining the deliberately large fixture beyond this test's
        // assertions on constrained CI runners.
        onsets.clear();
    }

    #[test]
    #[cfg(feature = "osc")]
    fn osc_aggregate_byte_cap_truncates_before_retaining_an_unbounded_batch() {
        let payload = "x".repeat(7_000);
        let onsets = (0..64u64)
            .map(|onset_id| OnsetEventJson {
                onset_id,
                generation: 1,
                whole_begin: "0".into(),
                duration_secs: 0.25,
                target_time: 0.0,
                live_controls: [0; 2],
                ui_visuals: 0,
                value: ValueJson::Raw(serde_json::json!({
                    "oscport": 57120,
                    "s": payload.clone()
                })),
                value_show: String::new(),
                log_line: None,
            })
            .collect::<Vec<_>>();
        let mut session = Session::new().expect("session");
        session.set_direct_diagnostic_logging(false);

        session.collect_pending_osc(&onsets, 0.0, 0.5);

        assert!(session.pending_osc.len() < onsets.len());
        assert!(
            session
                .pending_osc
                .iter()
                .map(|(_, intent)| intent.encoded_bytes)
                .sum::<usize>()
                <= MAX_OSC_BYTES_PER_PASS
        );
        assert!(
            session
                .take_diagnostics()
                .iter()
                .any(|diagnostic| diagnostic.message.contains("per-pass limit"))
        );
    }

    #[test]
    #[cfg(all(feature = "device-audio", feature = "osc"))]
    fn a_granted_osc_host_is_queued_with_a_resolved_address() {
        let mut config = SessionConfig::default();
        config
            .score_osc_access
            .permit_host("10.0.0.5")
            .expect("grant");
        let mut session = Session::with_config(config).expect("session");
        session
            .evaluate(r#"s("not-a-native-sample").osc(57120).oschost("10.0.0.5").fast(8)"#)
            .expect("score");

        let _ = session.schedule_audio_live_at(
            0.0,
            48_000,
            Duration::from_secs(1),
            LiveQueryBudgetMode::ReplacementPrefill,
        );
        let pending = session.take_pending_osc();
        assert!(!pending.is_empty(), "granted OSC intent was dropped");
        assert!(pending.iter().all(|(_, intent)| {
            intent.destination.is_some_and(|destination| {
                destination.ip() == std::net::IpAddr::from([10, 0, 0, 5])
                    && destination.port() == 57120
            })
        }));
    }

    #[cfg(feature = "midi")]
    fn midi_collection_onset(onset_id: u64, value: serde_json::Value) -> OnsetEventJson {
        OnsetEventJson {
            onset_id,
            generation: 1,
            whole_begin: "0".into(),
            duration_secs: 0.25,
            target_time: 0.0,
            live_controls: [0; 2],
            ui_visuals: 0,
            value: ValueJson::Raw(value),
            value_show: String::new(),
            log_line: None,
        }
    }

    #[test]
    #[cfg(feature = "midi")]
    fn oversized_and_unplannable_midi_onsets_do_not_hide_a_later_valid_one() {
        let oversized = "x".repeat(crate::midi_bridge::MAX_MIDI_PORT_NAME_BYTES + 1);
        let unknown_command = "x".repeat(1024 * 1024);
        let onsets = [
            midi_collection_onset(1, serde_json::json!({ "midiport": oversized, "note": 60 })),
            midi_collection_onset(
                2,
                serde_json::json!({ "midiport": "silent", "midicmd": unknown_command }),
            ),
            midi_collection_onset(3, serde_json::json!({ "midiport": "valid", "note": 64 })),
        ];
        let mut session = Session::new().expect("session");
        session.set_direct_diagnostic_logging(false);

        session.collect_pending_midi(&onsets);

        assert_eq!(session.pending_midi.len(), 1);
        assert_eq!(session.pending_midi[0].onset_id, 3);
        assert_eq!(
            session
                .take_diagnostics()
                .iter()
                .filter(|diagnostic| diagnostic.kind == "midi-refused")
                .count(),
            1
        );
    }

    #[test]
    #[cfg(feature = "midi")]
    fn midi_route_attempt_cap_bounds_even_silent_routed_haps() {
        let onset =
            midi_collection_onset(1, serde_json::json!({ "midiport": "silent", "s": "bd" }));
        let onsets = vec![onset; MAX_MIDI_ROUTE_ATTEMPTS_PER_PASS + 1];
        let mut session = Session::new().expect("session");
        session.set_direct_diagnostic_logging(false);

        session.collect_pending_midi(&onsets);
        assert!(session.pending_midi.is_empty());
        assert!(
            session
                .take_diagnostics()
                .iter()
                .any(|diagnostic| diagnostic.message.contains("routed onsets"))
        );

        session.collect_pending_midi(&onsets);
        assert!(
            session.take_diagnostics().is_empty(),
            "limit report repeated"
        );
    }

    #[test]
    #[cfg(feature = "midi")]
    fn midi_intent_cap_counts_only_plannable_intents() {
        let silent =
            midi_collection_onset(1, serde_json::json!({ "midiport": "silent", "s": "bd" }));
        let mut onsets = vec![silent; 600];
        onsets.extend((0..MAX_MIDI_INTENTS_PER_PASS).map(|index| {
            midi_collection_onset(
                index as u64 + 2,
                serde_json::json!({ "midiport": "valid", "note": 60 }),
            )
        }));
        // A non-routed tail after an exactly full batch must not manufacture a
        // false MIDI-cap report.
        onsets.push(midi_collection_onset(
            u64::MAX,
            serde_json::json!({ "s": "bd" }),
        ));
        let mut session = Session::new().expect("session");
        session.set_direct_diagnostic_logging(false);
        session.collect_pending_midi(&onsets);
        assert_eq!(session.pending_midi.len(), MAX_MIDI_INTENTS_PER_PASS);
        assert!(session.take_diagnostics().is_empty());

        onsets.push(midi_collection_onset(
            u64::MAX - 1,
            serde_json::json!({ "midiport": "valid", "note": 60 }),
        ));
        session.collect_pending_midi(&onsets);
        assert_eq!(session.pending_midi.len(), MAX_MIDI_INTENTS_PER_PASS);
        assert!(
            session
                .take_diagnostics()
                .iter()
                .any(|diagnostic| diagnostic.message.contains("per-pass limit"))
        );
    }

    #[test]
    #[cfg(feature = "midi")]
    fn midi_retained_byte_cap_accepts_its_exact_boundary() {
        let route = "x".repeat(crate::midi_bridge::MAX_MIDI_PORT_NAME_BYTES);
        let mut onsets = (0..MAX_MIDI_INTENTS_PER_PASS)
            .map(|index| {
                midi_collection_onset(
                    index as u64,
                    serde_json::json!({ "midiport": route.clone(), "note": 60 }),
                )
            })
            .collect::<Vec<_>>();
        let mut session = Session::new().expect("session");
        session.set_direct_diagnostic_logging(false);
        session.collect_pending_midi(&onsets);
        assert_eq!(session.pending_midi.len(), MAX_MIDI_INTENTS_PER_PASS);
        assert!(session.take_diagnostics().is_empty());

        onsets.push(midi_collection_onset(
            u64::MAX,
            serde_json::json!({ "midiport": route, "note": 60 }),
        ));
        session.collect_pending_midi(&onsets);
        assert_eq!(session.pending_midi.len(), MAX_MIDI_INTENTS_PER_PASS);
        assert!(
            session
                .take_diagnostics()
                .iter()
                .any(|diagnostic| diagnostic.message.contains("retained bytes"))
        );
    }

    #[test]
    #[cfg(feature = "midi")]
    fn midi_message_cap_bounds_mapped_controls_before_retention() {
        let mut canonical = std::collections::BTreeSet::new();
        for name in rustel_core::controls::default_control_registry().names() {
            let name = rustel_core::controls::default_control_registry()
                .canonical_name(name)
                .unwrap_or(name);
            if !matches!(
                name,
                "note"
                    | "midiport"
                    | "midimap"
                    | "midichan"
                    | "ccn"
                    | "ccv"
                    | "ctlNum"
                    | "progNum"
                    | "midibend"
                    | "miditouch"
                    | "midicmd"
            ) {
                canonical.insert(name.to_string());
            }
            if canonical.len() == 32 {
                break;
            }
        }
        assert_eq!(canonical.len(), 32, "control registry fixture is too small");

        let mapping = canonical
            .iter()
            .enumerate()
            .map(|(index, name)| (name.clone(), serde_json::json!(index % 128)))
            .collect::<serde_json::Map<_, _>>();
        let mut value = canonical
            .iter()
            .map(|name| (name.clone(), serde_json::json!(0.5)))
            .collect::<serde_json::Map<_, _>>();
        value.insert("midiport".into(), serde_json::json!("mapped"));
        value.insert("note".into(), serde_json::json!(60));

        let mut session = Session::new().expect("session");
        session.set_direct_diagnostic_logging(false);
        let map_json = serde_json::Value::Object(mapping).to_string();
        session.js.with_runtime_settings(|| {
            rustel_core::midimap::register_midi_map_json("default", &map_json).expect("bounded map")
        });
        let template = midi_collection_onset(1, serde_json::Value::Object(value));
        let messages_per_intent = session.js.with_runtime_settings(|| {
            let intent = crate::midi_bridge::midi_onset(&template).expect("MIDI intent");
            rustel_midi::plan(&intent.controls, intent.duration_secs).len()
        });
        assert!(messages_per_intent > 16, "fixture is not message-dense");
        let admitted = MAX_MIDI_MESSAGES_PER_PASS / messages_per_intent;
        let onsets = (0..=admitted)
            .map(|index| {
                let mut onset = template.clone();
                onset.onset_id = index as u64;
                onset
            })
            .collect::<Vec<_>>();

        session.collect_pending_midi(&onsets);

        assert_eq!(session.pending_midi.len(), admitted);
        assert!(
            session
                .pending_midi
                .iter()
                .map(|intent| rustel_midi::plan(&intent.controls, intent.duration_secs).len())
                .sum::<usize>()
                <= MAX_MIDI_MESSAGES_PER_PASS
        );
        assert!(
            session
                .take_diagnostics()
                .iter()
                .any(|diagnostic| diagnostic.message.contains("wire messages"))
        );
    }

    #[test]
    #[cfg(all(feature = "device-audio", feature = "midi"))]
    fn midiport_without_a_plannable_message_does_not_make_a_replacement_renderable() {
        for source in [
            r#"s("not-a-native-sample").midi('missing-port').fast(8)"#,
            r#"s("not-a-native-sample").note(999).midi('missing-port').fast(8)"#,
        ] {
            let mut session = Session::new().expect("session");
            session.evaluate(source).expect("MIDI-only score");

            match session.schedule_audio_live_at(
                0.0,
                48_000,
                Duration::from_secs(1),
                LiveQueryBudgetMode::ReplacementPrefill,
            ) {
                Err(
                    LiveAudioScheduleError::CandidateCommitted(_)
                    | LiveAudioScheduleError::CandidateRefusedScore(_),
                ) => {}
                Err(LiveAudioScheduleError::Retryable(error))
                | Err(LiveAudioScheduleError::AwaitingSamples(error)) => {
                    panic!("unplannable MIDI intent was retryable: {error}")
                }
                Ok(batch) => panic!(
                    "unplannable MIDI intent made the replacement renderable: {:?}",
                    batch.events
                ),
            }

            let pending = session.take_pending_midi();
            assert!(
                pending.is_empty(),
                "unplannable MIDI intents must not consume the bounded live queue"
            );
        }
    }

    #[test]
    #[cfg(all(feature = "device-audio", feature = "midi"))]
    fn note_producing_midi_replacement_is_renderable_without_a_scalar_voice() {
        let mut session = Session::new().expect("session");
        session
            .evaluate(r#"s("not-a-native-sample").note(60).midi('missing-port').fast(8)"#)
            .expect("MIDI note score");

        let batch = match session.schedule_audio_live_at(
            0.0,
            48_000,
            Duration::from_secs(1),
            LiveQueryBudgetMode::ReplacementPrefill,
        ) {
            Ok(batch) => batch,
            Err(LiveAudioScheduleError::Retryable(error))
            | Err(LiveAudioScheduleError::AwaitingSamples(error))
            | Err(
                LiveAudioScheduleError::CandidateCommitted(error)
                | LiveAudioScheduleError::CandidateRefusedScore(error),
            ) => {
                panic!("note-producing MIDI replacement was rejected: {error}")
            }
        };

        assert!(
            batch.events.is_empty(),
            "fixture unexpectedly rendered scalar audio"
        );
        let pending = session.take_pending_midi();
        assert!(!pending.is_empty(), "MIDI note intent was not retained");
        assert!(
            pending
                .iter()
                .all(|intent| rustel_midi::has_output(&intent.controls)),
            "note-producing MIDI intent was not recognized"
        );
    }

    #[test]
    #[cfg(feature = "device-audio")]
    fn live_query_preflight_and_deadline_are_atomic_then_same_now_retry_recovers() {
        let mut session = Session::new().expect("session");
        session
            .evaluate(
                r#"
                  globalThis.__liveBudgetSpin = false;
                  globalThis.__liveBudgetCalls = 0;
                  note('c4').fast(8).fmap(value => {
                    globalThis.__liveBudgetCalls++;
                    if (globalThis.__liveBudgetSpin) while (true) {}
                    return value;
                  })
                "#,
            )
            .expect("live budget fixture");
        let initial = session
            .schedule_through_with_js_budget(0.0, 0.0, Duration::from_millis(100))
            .expect("initial horizon fill");
        let now = 0.39;
        let queued_before = session.scheduler.queued();
        let horizon_before = session.scheduler.horizon_remaining(now);

        session
            .js
            .eval("globalThis.__liveBudgetSpin = true")
            .expect("arm live runaway");
        let started = Instant::now();
        let deadline = session.schedule_audio_live_at(
            now,
            48_000,
            Duration::from_millis(25),
            LiveQueryBudgetMode::Steady,
        );
        let deadline_message = match deadline {
            Err(LiveAudioScheduleError::Retryable(RuntimeError::ResourceLimit(message))) => message,
            _ => panic!("live runaway did not retain its typed atomic refusal"),
        };
        assert!(
            deadline_message.contains("CPU deadline"),
            "{deadline_message}"
        );
        assert!(
            started.elapsed() < Duration::from_millis(500),
            "remaining-horizon query deadline returned too late: {:?}",
            started.elapsed()
        );
        assert_eq!(session.scheduler.queued(), queued_before);
        assert!(
            (session.scheduler.horizon_remaining(now) - horizon_before).abs() < 1e-12,
            "deadline refusal advanced the scheduler cursor"
        );

        session
            .js
            .eval("globalThis.__liveBudgetSpin = false")
            .expect("disarm live runaway");
        let recovered = match session.schedule_audio_live_at(
            now,
            48_000,
            Duration::from_millis(25),
            LiveQueryBudgetMode::Steady,
        ) {
            Ok(batch) => batch,
            Err(LiveAudioScheduleError::Retryable(error))
            | Err(LiveAudioScheduleError::AwaitingSamples(error)) => {
                panic!("same-now live retry did not recover: {error:?}")
            }
            Err(
                LiveAudioScheduleError::CandidateCommitted(error)
                | LiveAudioScheduleError::CandidateRefusedScore(error),
            ) => {
                panic!("same-now live retry failed after commit: {error:?}")
            }
        };
        assert!(!recovered.events.is_empty(), "retry scheduled no events");
        assert_eq!(
            recovered.events.first().map(|event| event.onset_id),
            Some(initial[0].onset_id + 1),
            "atomic refusals consumed an onset id"
        );
    }

    /// A spent horizon must still evaluate a save, not defer it forever.
    ///
    /// Deferring is right while cover remains: the identity stays retryable and
    /// the horizon refills in milliseconds. Once the horizon is GONE the same
    /// refusal repeats on every attempt, and the set freezes on its last good
    /// score while every edit is rejected.
    #[test]
    #[cfg(feature = "device-audio")]
    fn a_spent_horizon_still_evaluates_a_save() {
        let reserve = Duration::from_millis(10);
        let mut session = Session::new().expect("session");
        session.evaluate("note('c4')").expect("score");
        session.schedule_at(0.0).expect("fill the horizon");

        // Cover left: defer, cheaply, and try again in a moment.
        assert_eq!(
            session
                .live_score_budget_at(0.4999, reserve)
                .expect("valid clock"),
            None,
            "a tight but SOUNDING horizon must still defer"
        );

        // Horizon spent: there is nothing left to protect, and no later
        // attempt that could go better.
        let recovered = session
            .live_score_budget_at(0.75, reserve)
            .expect("valid clock")
            .expect("a spent horizon refused the save that could restore it");
        assert!(
            recovered > MIN_LIVE_SCORE_CPU_BUDGET,
            "recovery slice {recovered:?} is no better than the floor that failed"
        );
    }

    /// A drained horizon gets a recovery slice. The 25 ms floor protects a
    /// horizon that still exists. Once it is spent, a dense score needs more
    /// than 25 ms for one window, so the floor alone refuses every attempt.
    #[test]
    #[cfg(feature = "device-audio")]
    fn a_drained_horizon_gets_a_recovery_slice_not_the_steady_state_floor() {
        let mut session = Session::new().expect("session");
        // Each hap costs real milliseconds inside QuickJS, so one window needs
        // far more than the steady-state floor and completes inside a recovery
        // slice - the shape of a dense live score, in miniature.
        session
            .evaluate(
                r#"
                  globalThis.__calls = 0;
                  note('c4').fast(24).fmap(value => {
                    globalThis.__calls++;
                    let sink = 0;
                    for (let i = 0; i < 40000; i++) sink += i;
                    return value;
                  })
                "#,
            )
            .expect("dense fixture");
        session
            .schedule_through_with_js_budget(0.0, 0.0, Duration::from_millis(500))
            .expect("initial horizon fill");

        // Far enough in that the reserve cannot be covered: the horizon this
        // budget protects is gone.
        let now = 0.39;
        assert!(
            session
                .live_query_budget_at(now, Duration::from_millis(29), LiveQueryBudgetMode::Steady)
                .expect("valid clock")
                .is_none(),
            "fixture no longer reaches the unaffordable branch"
        );

        let batch = match session.schedule_audio_live_at(
            now,
            48_000,
            Duration::from_millis(29),
            LiveQueryBudgetMode::Steady,
        ) {
            Ok(batch) => batch,
            Err(LiveAudioScheduleError::Retryable(error))
            | Err(LiveAudioScheduleError::AwaitingSamples(error))
            | Err(
                LiveAudioScheduleError::CandidateCommitted(error)
                | LiveAudioScheduleError::CandidateRefusedScore(error),
            ) => {
                panic!("a dense score could not recover a drained horizon: {error}")
            }
        };
        assert!(
            !batch.events.is_empty(),
            "recovery scheduled nothing: the slice was too small to finish a window"
        );
        assert!(
            LIVE_QUERY_RECOVERY_BUDGET > MIN_LIVE_QUERY_JS_BUDGET,
            "the recovery slice must exceed the steady-state floor"
        );
    }

    /// Falling behind the audio clock must cost the SAME as steady state.
    ///
    /// `Scheduler::tick` queries forward from its cursor, so a producer that
    /// loses time renders music whose moment has passed, and the further
    /// behind it is, the more each tick costs. Once a tick exceeds the query
    /// budget it is refused, the cursor holds, and the gap only grows.
    #[test]
    #[cfg(feature = "device-audio")]
    fn a_producer_far_behind_the_clock_resumes_at_the_present() {
        let mut session = Session::new().expect("session");
        session
            .evaluate(r#"stack(s("bd*4"), note("c3 e3").s("sawtooth").seg(8))"#)
            .expect("score");
        session
            .schedule_through_with_js_budget(0.0, 0.0, Duration::from_millis(100))
            .expect("initial horizon fill");

        // Ten minutes of clock with no scheduling: a stalled producer, or a
        // machine that lost the thread to something else entirely.
        let now = 600.0;
        let cover = session.live_schedule_cover();
        let phase_before = session.scheduler.cycle_at_time(now);

        let started = Instant::now();
        let batch = match session.schedule_audio_live_at(
            now,
            48_000,
            Duration::from_millis(25),
            LiveQueryBudgetMode::Steady,
        ) {
            Ok(batch) => batch,
            Err(LiveAudioScheduleError::Retryable(error))
            | Err(LiveAudioScheduleError::AwaitingSamples(error))
            | Err(
                LiveAudioScheduleError::CandidateCommitted(error)
                | LiveAudioScheduleError::CandidateRefusedScore(error),
            ) => {
                panic!("a producer behind the clock could not schedule at all: {error}")
            }
        };
        let took = started.elapsed();

        assert!(!batch.events.is_empty(), "recovery scheduled no events");
        assert!(
            took < Duration::from_millis(50),
            "catching up on ten minutes of past music took {took:?}; the cost              must not scale with the gap"
        );
        assert!(
            session.scheduler.horizon_remaining(now) >= cover * 0.9,
            "the horizon is still {} of {cover} after recovery: the tick spent              its budget in the past and never reached the present",
            session.scheduler.horizon_remaining(now)
        );
        // Only the CURSOR moves. Restarting the bar would be audible as a
        // dropped beat on every recovery.
        assert!(
            (session.scheduler.cycle_at_time(now) - phase_before).abs() < 1e-9,
            "recovery moved the cycle/time mapping: {} -> {}",
            phase_before,
            session.scheduler.cycle_at_time(now)
        );
    }

    /// An unaffordable continuation reserve does not refuse the query. The
    /// horizon that the reserve protects is already spent, and only a query
    /// refills it.
    #[test]
    #[cfg(feature = "device-audio")]
    fn an_unaffordable_reserve_fills_with_the_minimum_slice_instead_of_deadlocking() {
        let mut session = Session::new().expect("session");
        session
                .evaluate("globalThis.__calls = 0; note('c4').fast(8).fmap(v => { globalThis.__calls++; return v; })")
                .expect("fixture");
        session
            .schedule_through_with_js_budget(0.0, 0.0, Duration::from_millis(100))
            .expect("initial horizon fill");

        // Far enough into the horizon that the reserve cannot be covered.
        let now = 0.39;
        let calls_before = session.js.get_number("__calls").expect("callback count");
        let horizon_before = session.scheduler.horizon_remaining(now);

        let batch = match session.schedule_audio_live_at(
            now,
            48_000,
            Duration::from_millis(29),
            LiveQueryBudgetMode::Steady,
        ) {
            Ok(batch) => batch,
            Err(LiveAudioScheduleError::Retryable(error))
            | Err(LiveAudioScheduleError::AwaitingSamples(error))
            | Err(
                LiveAudioScheduleError::CandidateCommitted(error)
                | LiveAudioScheduleError::CandidateRefusedScore(error),
            ) => panic!(
                "unaffordable reserve refused the only query that can refill the horizon: {error}"
            ),
        };
        assert!(
            session.js.get_number("__calls").unwrap_or(calls_before) > calls_before,
            "minimum-slice fill never entered JavaScript"
        );
        assert!(
            session.scheduler.horizon_remaining(now) > horizon_before,
            "minimum-slice fill did not extend the horizon: {} -> {}",
            horizon_before,
            session.scheduler.horizon_remaining(now)
        );
        assert!(
            !batch.events.is_empty(),
            "minimum-slice fill scheduled nothing"
        );

        // And again once the clock has advanced past the refill: a set that
        // survives one stall must survive the next one too.
        let later = now + 0.4;
        if let Err(LiveAudioScheduleError::Retryable(error))
        | Err(
            LiveAudioScheduleError::CandidateCommitted(error)
            | LiveAudioScheduleError::CandidateRefusedScore(error),
        ) = session.schedule_audio_live_at(
            later,
            48_000,
            Duration::from_millis(29),
            LiveQueryBudgetMode::Steady,
        ) {
            panic!("second unaffordable reserve wedged the producer: {error}");
        }
    }

    #[test]
    fn live_replacement_probe_budget_tracks_the_candidate_tempo() {
        assert_eq!(
            Session::live_replacement_probe_budget(Some(3.75)),
            Duration::from_millis(200),
            "900/4 must receive its sustainable one-cycle compute share"
        );
        assert_eq!(
            Session::live_replacement_probe_budget(Some(50.0)),
            Duration::from_millis(15),
            "very high CPS must not retain the fixed probe ceiling"
        );
        assert_eq!(
            Session::live_replacement_probe_budget(Some(0.5)),
            LIVE_QUERY_RECOVERY_BUDGET,
            "low-tempo candidates remain capped by the bounded recovery slice"
        );
        assert_eq!(
            Session::live_replacement_probe_budget(None),
            Session::PROBE_DEFAULT_CEILING
        );
        assert_eq!(
            Session::live_replacement_probe_budget(Some(f64::NAN)),
            Session::PROBE_DEFAULT_CEILING
        );

        assert_eq!(
            Session::live_replacement_takeover_headroom(Some(3.75), Duration::from_millis(60)),
            LIVE_REPLACEMENT_MIN_HEADROOM,
            "a cheap 900-BPM candidate must retain immediate edit latency"
        );
        let at_900_bpm =
            Session::live_replacement_takeover_headroom(Some(3.75), Duration::from_millis(61));
        assert!(
            (at_900_bpm.as_secs_f64() - 1.0 / 3.75).abs() < 1e-9,
            "a sustainable 900-BPM first query must finish before takeover: {at_900_bpm:?}"
        );
        assert_eq!(
            Session::live_replacement_takeover_headroom(Some(50.0), Duration::from_millis(61)),
            LIVE_REPLACEMENT_MIN_HEADROOM,
            "very short cycles retain the ordinary immediate-edit floor"
        );
        let capped_low_tempo =
            Session::live_replacement_takeover_headroom(Some(0.5), Duration::from_millis(61));
        assert!(
            (capped_low_tempo.as_secs_f64() - 1.0 / 3.0).abs() < 1e-9,
            "the bounded 250 ms probe must not create a multi-second takeover: {capped_low_tempo:?}"
        );
    }

    #[test]
    fn live_score_budget_uses_remaining_horizon_floor_and_ceiling() {
        let reserve = Duration::from_millis(10);
        let mut session = Session::new().expect("session");
        session.evaluate("note('c4')").expect("score");
        session.schedule_at(0.0).expect("fill default horizon");

        let partly_drained = session
            .live_score_budget_at(0.2, reserve)
            .expect("valid clock")
            .expect("partly drained horizon still affords score JavaScript");
        assert!(
            partly_drained.abs_diff(Duration::from_millis(270)) <= Duration::from_micros(1),
            "budget came from the nominal 500ms horizon instead of the 300ms remaining: {partly_drained:?}"
        );
        assert!(
            session
                .live_score_budget_at(0.4719, Duration::from_millis(1))
                .expect("valid just-above-floor clock")
                .is_some(),
            "a score budget above the minimum viable slice was deferred"
        );
        assert_eq!(
            session
                .live_score_budget_at(0.4721, Duration::from_millis(1))
                .expect("valid just-below-floor clock"),
            None,
            "sub-minimum score JavaScript ran instead of retaining its identity"
        );
        assert!(
            session.live_score_budget_at(0.0, Duration::ZERO).is_err(),
            "zero continuation reserve made the deadline equal the whole horizon"
        );

        let mut large = Session::with_config(SessionConfig {
            horizon: 10.0,
            ..SessionConfig::default()
        })
        .expect("large-horizon session");
        large.evaluate("note('c4')").expect("large score");
        large.schedule_at(0.0).expect("fill large horizon");
        assert_eq!(
            large
                .live_score_budget_at(0.0, reserve)
                .expect("large valid clock"),
            Some(SCORE_CPU_BUDGET),
            "a large configured horizon bypassed the score CPU ceiling"
        );
        assert_eq!(SCORE_CPU_BUDGET, Duration::from_secs(2));
    }

    #[test]
    fn live_score_defers_without_running_then_deadlines_transactionally_and_recovers() {
        let mut session = Session::new().expect("session");
        session
            .evaluate(
                "stack( \
                   s('bd').polyBind(value => pure(value).fast(2)), \
                   pure(['old-owned-value']).fast(2) \
                 )",
            )
            .expect("initial callback/JS-owned score");
        session.schedule_at(0.0).expect("fill horizon");
        let generation = session.generation();
        let last_source = session.last_source.clone();
        let last_path = session.last_evaluate_source();
        let before = active_values(&session);
        let cancellation = AtomicBool::new(false);

        let mut deferred_clock_calls = 0;
        let deferred = session
            .evaluate_live_score_cancellable(
                "globalThis.deferredScoreRan = 1; note('d4')",
                false,
                Duration::from_millis(1),
                &cancellation,
                || {
                    deferred_clock_calls += 1;
                    0.49
                },
            )
            .expect("a tiny affordable slice is deferral, not rejection");
        assert_eq!(deferred, LiveScoreAttempt::Deferred);
        assert_eq!(deferred_clock_calls, 1, "deferral sampled an install clock");
        assert_eq!(session.js.get_number("deferredScoreRan"), None);
        assert_eq!(session.config().cps, DEFAULT_CPS);
        assert_eq!(session.scheduler.cps(), DEFAULT_CPS);

        // Make the syntactically recognisable `s('sd')` compatibility form
        // run away in JavaScript. If a typed deadline is mistakenly fed into
        // the mini fallback, this call succeeds as the mini pattern `sd` and
        // the test loses its expected resource-limit failure.
        session
            .evaluate_prebake(
                "globalThis.originalScoreS = s; \
                 globalThis.blockScoreS = true; \
                 globalThis.s = (...args) => { \
                   if (blockScoreS) { \
                     setcps(4); \
                     globalThis.sessionScorePrefix = 17; \
                     while (true) {} \
                   } \
                   return originalScoreS(...args); \
                 };",
            )
            .expect("install hostile score helper");
        let mut deadline_clock_calls = 0;
        let started = Instant::now();
        let error = session
            .evaluate_live_score_cancellable(
                "setcps(2); s('sd')",
                false,
                Duration::from_millis(10),
                &cancellation,
                || {
                    deadline_clock_calls += 1;
                    0.4
                },
            )
            .expect_err("runaway watched score must hit its remaining-horizon deadline");
        assert_eq!(
            error.kind(),
            "resource-limit",
            "typed deadline was lost: {error}"
        );
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "watched score used the offline two-second ceiling instead of its remaining horizon: {:?}",
            started.elapsed()
        );
        assert_eq!(
            deadline_clock_calls, 1,
            "a failed score sampled an install clock despite making no commit"
        );
        assert_eq!(session.js.get_number("sessionScorePrefix"), Some(17.0));
        assert_eq!(session.generation(), generation);
        assert_eq!(
            session.config().cps,
            DEFAULT_CPS,
            "a timed-out watched score committed its staged tempo"
        );
        assert_eq!(session.scheduler.cps(), DEFAULT_CPS);
        assert_eq!(session.last_source, last_source);
        assert_eq!(session.last_evaluate_source(), last_path);
        session.js.run_gc();
        session.js.run_gc();
        assert_eq!(active_values(&session), before);
        assert_eq!(session.js.query_depth(), 0, "deadline leaked query scope");

        session
            .evaluate_prebake("globalThis.blockScoreS = false;")
            .expect("enable corrected score");
        let mut clocks = [0.4, 0.44].into_iter();
        let applied = session
            .evaluate_live_score_cancellable(
                "setcps(2); s('sd')",
                false,
                Duration::from_millis(10),
                &cancellation,
                || clocks.next().expect("exactly two live score clock samples"),
            )
            .expect("same-heap correction after deadline");
        assert_eq!(applied, LiveScoreAttempt::Applied(generation + 1));
        assert!(
            clocks.next().is_none(),
            "score sampled more than two clocks"
        );
        assert_eq!(session.last_evaluate_source(), EvaluateSource::JavaScript);
        assert_eq!(active_values(&session), ["s:sd"]);
        assert_eq!(session.config().cps, 2.0);
        assert_eq!(session.scheduler.cps(), 2.0);
        // The re-query cursor sits at the edit instant (the overlap up to the
        // takeover is the replacement's own to query, old onsets pre-marked),
        // so the schedule cover reaches exactly to the install clock 0.44.
        // Anchoring at the pre-evaluation clock 0.4 would leave the cover at
        // 0.4 and report zero remaining.
        assert!(
            (session.scheduler.horizon_remaining(0.4) - 0.04).abs() < 1e-9,
            "new generation was anchored at the pre-evaluation clock rather than the fresh install clock"
        );

        // A broken clock provider after successful JS cannot be reported as a
        // score failure: the active graph is already the fully-owned candidate,
        // and rebuilding the old graph from a bare Pattern would discard its
        // wrapper Sidecar roots. Commit at the validated pre-eval sample.
        let corrected = session
            .schedule_through(0.44, 0.94)
            .expect("refill corrected score");
        assert_eq!(corrected.len(), 1);
        // A live reload keeps the cycle↔time mapping fixed, and a tempo change
        // pivots at the install clock. Old mapping (anchor (0,0), cps 0.5) puts the pivot
        // 0.44 s at cycle 0.22; the next sd onset is cycle 1, landing at
        // 0.44 + (1 − 0.22)/2 = 0.83 s.
        assert!((corrected[0].target_time - 0.83).abs() < 1e-9);
        assert!((corrected[0].duration_secs - 0.5).abs() < 1e-9);
        let mut faulty_clocks = [0.44, f64::NAN].into_iter();
        let applied = session
            .evaluate_live_score_cancellable(
                "note('e4')",
                false,
                Duration::from_millis(10),
                &cancellation,
                || faulty_clocks.next().expect("two faulty-clock samples"),
            )
            .expect("successful score must commit despite a faulty post clock");
        assert_eq!(applied, LiveScoreAttempt::Applied(generation + 2));
        assert_eq!(active_values(&session), ["note:e4"]);
        assert_eq!(session.config().cps, 2.0);
        assert_eq!(session.scheduler.cps(), 2.0);
        // The re-query cursor sits at the edit instant, so the cover reaches
        // exactly to the install clock 0.44.
        assert!(session.scheduler.horizon_remaining(0.44).abs() < 1e-9);
    }

    #[test]
    fn score_cancellation_stays_typed_keeps_the_old_graph_and_recovers() {
        use std::sync::atomic::Ordering;

        let mut session = Session::new().expect("session");
        session.samples = Some(Arc::new(crate::samples::SampleLibrary::empty()));
        session.evaluate("note('c4')").expect("initial score");
        let generation = session.generation();
        let before = active_values(&session);
        session
            .evaluate_prebake(
                "globalThis.originalCancelledS = s; \
                 globalThis.effectSamples = samples; \
                 globalThis.effectPreload = preload; \
                 globalThis.blockCancelledS = true; \
                 globalThis.s = (...args) => { \
                   if (blockCancelledS) { \
                     effectSamples({ cancelledLeak: ['http://127.0.0.1:9/cancelled.wav'] }); \
                     effectPreload('cancelledLeak'); \
                     setcps(4); \
                     globalThis.sessionCancelPrefix = 23; \
                     while (true) {} \
                   } \
                   return originalCancelledS(...args); \
                 };",
            )
            .expect("install cancellable hostile helper");

        let cancellation = Arc::new(AtomicBool::new(false));
        let request = {
            let cancellation = cancellation.clone();
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(40));
                cancellation.store(true, Ordering::Relaxed);
            })
        };
        let started = Instant::now();
        let error = session
            .evaluate_cancellable("s('sd')", cancellation.as_ref())
            .expect_err("caller cancellation must interrupt score construction");
        request.join().expect("cancellation requester");
        assert!(matches!(error, RuntimeError::Cancelled));
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "cancellation waited for the two-second deadline: {:?}",
            started.elapsed()
        );
        assert_eq!(session.js.get_number("sessionCancelPrefix"), Some(23.0));
        assert_eq!(session.generation(), generation);
        assert_eq!(
            session.config().cps,
            DEFAULT_CPS,
            "a cancelled score committed its staged tempo"
        );
        assert_eq!(session.scheduler.cps(), DEFAULT_CPS);
        assert_eq!(active_values(&session), before);
        let library = session.samples.as_ref().expect("test sample library");
        assert!(!library.knows("cancelledLeak"));
        assert_eq!(session.preload_requested, 0);
        assert_eq!(
            session.js.query_depth(),
            0,
            "cancellation leaked query scope"
        );

        cancellation.store(false, Ordering::Relaxed);
        session
            .evaluate_prebake("globalThis.blockCancelledS = false;")
            .expect("enable correction");
        session
            .evaluate_cancellable("s('hh')", cancellation.as_ref())
            .expect("same-heap recovery after cancellation");
        assert_eq!(active_values(&session), ["s:hh"]);
        let library = session.samples.as_ref().expect("test sample library");
        assert!(!library.knows("cancelledLeak"));
        assert_eq!(session.preload_requested, 0);
    }

    #[test]
    fn pending_score_job_policy_cannot_be_laundered_through_mini_fallback() {
        let mut session = Session::new().expect("session");
        session.evaluate("note('c4')").expect("initial score");
        let generation = session.generation();
        let before = active_values(&session);
        let last_source = session.last_source.clone();
        session
            .js
            .eval(
                "Promise.resolve().then(() => { \
                   globalThis.staleScoreJobRan = 1; \
                 });",
            )
            .expect("seed a preexisting runnable job");

        // This shape is deliberately eligible for the compatibility Mini
        // fallback. A string-typed host-policy refusal would therefore install
        // `sd` and hide the fact that this score turn never ran.
        let error = session
            .evaluate("s('sd')")
            .expect_err("a stale score job must be a typed refusal");
        assert_eq!(error.kind(), "resource-limit");
        assert!(error.to_string().contains("runnable JavaScript jobs"));
        assert_eq!(session.generation(), generation);
        assert_eq!(session.last_source, last_source);
        assert_eq!(active_values(&session), before);
        assert_eq!(session.js.get_number("staleScoreJobRan"), None);
        assert!(!session.js.jobs_pending(), "stale job survived refusal");

        session
            .evaluate("s('hh')")
            .expect("same-heap recovery after stale-job refusal");
        assert_eq!(active_values(&session), ["s:hh"]);
    }

    /// A cancelled JavaScript evaluation is not retried through the mini
    /// fallback. `s("bd")` carries an extractable mini pattern, so the
    /// fallback could otherwise return success after a stop.
    #[test]
    fn a_cancelled_evaluation_is_not_laundered_through_the_mini_fallback() {
        let mut session = Session::new().expect("session");
        let cancellation = AtomicBool::new(true);
        let error = session
            .evaluate_cancellable("s(\"bd\")", &cancellation)
            .expect_err("a cancelled evaluation must not succeed via the mini fallback");
        assert!(
            matches!(error, RuntimeError::Cancelled),
            "expected Cancelled, got {error}"
        );
    }

    #[test]
    fn live_mini_is_outside_the_js_budget_but_uses_a_fresh_install_clock() {
        let mut session = Session::new().expect("session");
        session
            .evaluate("setcps(1); note('c4')")
            .expect("initial score");
        let generation = session.generation();
        let cancellation = AtomicBool::new(false);
        let mut clock_calls = 0;
        let applied = session
            .evaluate_live_score_cancellable(
                "e4",
                true,
                Duration::from_millis(1),
                &cancellation,
                || {
                    clock_calls += 1;
                    0.37
                },
            )
            .expect("live mini score");
        assert_eq!(applied, LiveScoreAttempt::Applied(generation + 1));
        assert_eq!(
            clock_calls, 1,
            "mini sampled a JavaScript budget clock instead of only its post-parse install clock"
        );
        assert_eq!(session.last_evaluate_source(), EvaluateSource::MiniRust);
        assert_eq!(
            session.config().cps,
            1.0,
            "an explicit live Mini replacement reset the active tempo"
        );
        assert_eq!(session.scheduler.cps(), 1.0);
        assert_eq!(active_values(&session), ["e4"]);
    }

    #[test]
    #[cfg(feature = "device-audio")]
    fn live_prebake_budget_uses_remaining_horizon_and_keeps_the_two_second_ceiling() {
        let reserve = std::time::Duration::from_millis(10);
        let mut session = Session::new().expect("session");
        session.evaluate("note('c4')").expect("score");
        session.schedule_at(0.0).expect("fill default horizon");

        let partly_drained = session
            .live_prebake_budget_at(0.2, reserve)
            .expect("valid clock")
            .expect("partly drained horizon still affords setup");
        assert!(
            partly_drained.abs_diff(std::time::Duration::from_millis(270))
                <= std::time::Duration::from_micros(1),
            "budget came from the nominal 500ms horizon instead of the 300ms remaining: {partly_drained:?}"
        );
        assert_eq!(
            session
                .live_prebake_budget_at(0.471, reserve)
                .expect("valid exhausted clock"),
            None,
            "evaluation was offered after the continuation reserve consumed the remaining horizon"
        );
        assert!(
            session
                .live_prebake_budget_at(0.4719, std::time::Duration::from_millis(1))
                .expect("valid just-above-floor clock")
                .is_some(),
            "a budget above the minimum viable setup slice was deferred"
        );
        assert_eq!(
            session
                .live_prebake_budget_at(0.4721, std::time::Duration::from_millis(1))
                .expect("valid just-below-floor clock"),
            None,
            "a sub-minimum budget ran setup instead of retaining the identity"
        );

        let mut large = Session::with_config(SessionConfig {
            horizon: 10.0,
            ..SessionConfig::default()
        })
        .expect("large-horizon session");
        large.evaluate("note('c4')").expect("large score");
        large.schedule_at(0.0).expect("fill large horizon");
        assert_eq!(
            large
                .live_prebake_budget_at(0.0, reserve)
                .expect("large valid clock"),
            Some(PREBAKE_CPU_BUDGET),
            "a large configured horizon bypassed the setup CPU ceiling"
        );
        assert!(
            session
                .live_prebake_budget_at(0.0, std::time::Duration::ZERO)
                .is_err(),
            "zero continuation reserve made the deadline equal the whole horizon"
        );
    }

    #[test]
    #[cfg(feature = "device-audio")]
    fn live_prebake_interrupt_uses_the_derived_budget_not_the_offline_constant() {
        let mut session = Session::new().expect("session");
        session.evaluate("note('c4')").expect("score");
        session.schedule_at(0.0).expect("fill horizon");
        let reserve = std::time::Duration::from_millis(1);
        let transport = session.transport();
        let deferred = session
            .evaluate_live_prebake_cancellable(
                "globalThis.mustNotRunUnderTinyBudget = 1;",
                0.49,
                reserve,
                transport.stopped_flag(),
            )
            .expect("a tiny affordable slice is deferral, not rejection");
        assert_eq!(deferred, LivePrebakeAttempt::Deferred);
        assert_eq!(session.js.get_number("mustNotRunUnderTinyBudget"), None);

        let now = 0.47;
        let derived = session
            .live_prebake_budget_at(now, reserve)
            .expect("valid clock")
            .expect("small positive budget");
        let generation = session.generation();
        let error = session
            .evaluate_live_prebake_cancellable(
                "globalThis.liveDeadlinePrefix = 1; while (true) {}",
                now,
                reserve,
                transport.stopped_flag(),
            )
            .expect_err("hostile watched setup must hit its affordable deadline");
        assert!(
            error
                .to_string()
                .contains(&format!("{} ms CPU deadline", derived.as_millis())),
            "watched setup used a fixed/offline deadline instead of {derived:?}: {error}"
        );
        assert_eq!(session.generation(), generation);
        assert_eq!(session.js.get_number("liveDeadlinePrefix"), Some(1.0));
    }

    #[test]
    fn prebake_keeps_the_active_graph_generation_transport_and_score_identity() {
        let mut session = Session::new().expect("session");
        session.evaluate_mini("c4").expect("initial mini score");
        let generation = session.generation();
        let last_source = session.last_source.clone();
        let last_path = session.last_evaluate_source();
        let before = session
            .query(Fraction::ZERO, Fraction::ONE)
            .expect("initial active query")
            .into_iter()
            .map(|hap| hap.show())
            .collect::<Vec<_>>();
        session.transport().stop();

        session
            .evaluate_prebake(
                "globalThis.loadCount = (globalThis.loadCount || 0) + 1; \
                 globalThis.fromPrebake = () => note(loadCount === 1 ? 'e4' : 'g4'); \
                 note('g4');",
            )
            .expect("prebake");
        assert_eq!(
            session.generation(),
            generation,
            "prebake replaced the active graph"
        );
        assert_eq!(session.last_source, last_source, "prebake became the score");
        assert_eq!(session.last_evaluate_source(), last_path);
        let after = session
            .js
            .query(Slot::Active, 0, Fraction::ZERO, Fraction::ONE)
            .expect("active query immediately after successful prebake")
            .into_iter()
            .map(|hap| hap.show())
            .collect::<Vec<_>>();
        assert_eq!(
            after, before,
            "successful prebake replaced the active graph"
        );
        assert!(
            session.transport().is_stopped(),
            "prebake restarted a stopped transport"
        );

        session.transport().start();
        session
            .reload_at("fromPrebake()", false, 1.0)
            .expect("score reload using setup helper");
        session
            .reload_at("fromPrebake().fast(2)", false, 2.0)
            .expect("second score reload");
        assert_eq!(
            session.js.get_number("loadCount"),
            Some(1.0),
            "score reload reran the prebake"
        );
        let haps = session
            .query(Fraction::int(2), Fraction::int(3))
            .expect("query reloaded score");
        assert_eq!(haps.len(), 2);
        assert!(
            haps.iter().all(|hap| hap.value.show().contains("e4")),
            "score reload lost the same-heap helper: {:?}",
            haps.iter().map(|hap| hap.value.show()).collect::<Vec<_>>()
        );
    }

    #[test]
    fn configured_parser_is_consumed_once_before_query_play_and_render() {
        let mut session = Session::new().expect("session");
        session
            .evaluate_prebake(
                "globalThis.productParserCalls = 0; \
                 setStringParser(value => { productParserCalls++; return mini(value); });",
            )
            .expect("install product string parser");
        session
            .evaluate("(() => stack(['c4', 'e4'].join(' ')).note())()")
            .expect("score through configured parser");
        assert_eq!(session.js.get_number("productParserCalls"), Some(1.0));
        let queried: Vec<(String, Fraction, Fraction)> = session
            .query(Fraction::ZERO, Fraction::ONE)
            .expect("query configured-parser score")
            .into_iter()
            .map(|hap| (hap.value.show(), hap.part.begin, hap.part.end))
            .collect();
        assert_eq!(
            queried,
            [
                ("note:c4".to_owned(), Fraction::ZERO, Fraction::new(1, 2),),
                ("note:e4".to_owned(), Fraction::new(1, 2), Fraction::ONE,),
            ],
            "the product query counted a parser call but ignored its two-step Pattern"
        );
        assert!(
            !session
                .play(1.0)
                .expect("play configured-parser score")
                .onsets
                .is_empty()
        );

        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock")
            .as_nanos();
        let output = std::env::temp_dir().join(format!(
            "rustel-parser-render-{}-{unique}.json",
            std::process::id()
        ));
        let report = session
            .render(1.0, &output, RenderFormat::OnsetJson)
            .expect("render configured-parser score");
        assert!(report.onset_count > 0, "render produced no onsets");
        std::fs::remove_file(&output).expect("remove owned onset dump");
        assert_eq!(
            session.js.get_number("productParserCalls"),
            Some(1.0),
            "query/play/render reparsed a construction-time string"
        );
    }

    #[test]
    fn stepwise_patterns_keep_exact_query_windows_steps_and_scheduler_onsets() {
        let mut session = Session::new().expect("session");
        session
            .evaluate("sequence(0, 1).replicate(slowcat(1, 2))")
            .expect("evaluate patterned replicate");
        assert!(
            !session.active_needs_host(),
            "a native patterned factor was pushed onto the JavaScript scheduler path"
        );

        fn shows(session: &Session, begin: Fraction, end: Fraction) -> Vec<String> {
            session
                .query(begin, end)
                .expect("query patterned replicate")
                .into_iter()
                .map(|hap| hap.show())
                .collect::<Vec<_>>()
        }
        assert_eq!(
            shows(&session, Fraction::ZERO, Fraction::int(2)),
            [
                "[ 0/1 → 1/2 | 0 ]",
                "[ 1/2 → 1/1 | 1 ]",
                "[ 1/1 → 3/2 | 0 ]",
                "[ 3/2 → 2/1 | 1 ]",
            ],
            "a joined query must select the StepJoin carrier from its own begin"
        );
        assert_eq!(
            shows(&session, Fraction::ONE, Fraction::int(2)),
            [
                "[ 1/1 → 5/4 | 0 ]",
                "[ 5/4 → 3/2 | 1 ]",
                "[ 3/2 → 7/4 | 0 ]",
                "[ 7/4 → 2/1 | 1 ]",
            ],
            "a separately queried second cycle must select factor two"
        );

        // The scheduler queries incrementally, rather than joining the full
        // render span into one carrier selection. At 0.5 CPS, cycle zero's
        // factor-one halves last one second and cycle one's factor-two quarters
        // last half a second. The final event is the documented inclusive
        // drain at the four-second boundary.
        let play = session.play(4.0).expect("schedule patterned replicate");
        let onsets = play
            .onsets
            .iter()
            .map(|onset| {
                (
                    onset.whole_begin.as_str(),
                    onset.value_show.as_str(),
                    onset.target_time,
                    onset.duration_secs,
                )
            })
            .collect::<Vec<_>>();
        assert_eq!(
            onsets,
            [
                ("0/1", "0", 0.0, 1.0),
                ("1/2", "1", 1.0, 1.0),
                ("1/1", "0", 2.0, 0.5),
                ("5/4", "1", 2.5, 0.5),
                ("3/2", "0", 3.0, 0.5),
                ("7/4", "1", 3.5, 0.5),
                ("2/1", "0", 4.0, 0.5),
            ],
            "scheduler windowing changed patterned replicate onsets"
        );

        session
            .evaluate("stepcat(sequence(0, 1, 2, 3).contract(2), 9)")
            .expect("evaluate contract step consumer");
        assert_eq!(
            session.js.active_pattern().expect("active contract").steps,
            Some(Fraction::int(3)),
            "contract metadata did not reach a real step consumer"
        );
        assert_eq!(
            shows(&session, Fraction::ZERO, Fraction::ONE),
            [
                "[ 0/1 → 1/6 | 0 ]",
                "[ 1/6 → 1/3 | 1 ]",
                "[ 1/3 → 1/2 | 2 ]",
                "[ 1/2 → 2/3 | 3 ]",
                "[ 2/3 → 1/1 | 9 ]",
            ],
            "Session lost contract's divided step metadata"
        );
    }

    #[test]
    fn take_drop_keep_exact_step_join_windows_and_scheduler_onsets() {
        fn shows(session: &Session, begin: Fraction, end: Fraction) -> Vec<String> {
            session
                .query(begin, end)
                .expect("query take/drop pattern")
                .into_iter()
                .map(|hap| hap.show())
                .collect()
        }

        let mut session = Session::new().expect("session");
        session
            .evaluate("new Pattern(state => pure('x').query(state)).take(1)")
            .expect("evaluate no-step take");
        assert!(
            !session.active_needs_host(),
            "take did not prune an unreachable no-step receiver callback"
        );
        assert!(
            shows(&session, Fraction::ZERO, Fraction::ONE).is_empty(),
            "a no-step receiver must become nothing"
        );

        session
            .evaluate("sequence(0, 1, 2).take(slowcat(1, 2))")
            .expect("evaluate patterned take");
        assert!(!session.active_needs_host());
        assert_eq!(
            session.js.active_pattern().expect("active take").steps,
            Some(Fraction::ONE),
            "take metadata must come from StepJoin's cycle-zero slice"
        );
        assert_eq!(
            shows(&session, Fraction::ZERO, Fraction::int(2)),
            ["[ 0/1 → 1/1 | 0 ]", "[ 1/1 → 2/1 | 0 ]"],
            "joined take query stopped selecting from its begin cycle"
        );
        assert_eq!(
            shows(&session, Fraction::ONE, Fraction::int(2)),
            ["[ 1/1 → 3/2 | 0 ]", "[ 3/2 → 2/1 | 1 ]"],
            "separate take query lost cycle one's amount"
        );
        let take_play = session.play(4.0).expect("schedule patterned take");
        let take_onsets = take_play
            .onsets
            .iter()
            .map(|onset| {
                (
                    onset.whole_begin.as_str(),
                    onset.value_show.as_str(),
                    onset.target_time,
                    onset.duration_secs,
                )
            })
            .collect::<Vec<_>>();
        assert_eq!(
            take_onsets,
            [
                ("0/1", "0", 0.0, 2.0),
                ("1/1", "0", 2.0, 1.0),
                ("3/2", "1", 3.0, 1.0),
                // The inclusive boundary onset was materialised by the
                // preceding lookahead window, whose StepJoin carrier is 2.
                ("2/1", "0", 4.0, 1.0),
            ],
            "scheduler stopped preserving patterned take's query windows"
        );

        // Use a fresh transport anchor for the second scheduler assertion;
        // live replacement after a completed play intentionally keeps the
        // existing clock rather than rewinding to cycle zero.
        let mut session = Session::new().expect("drop session");
        session
            .evaluate("sequence(0, 1, 2).drop(slowcat(1, 2))")
            .expect("evaluate patterned drop");
        assert!(!session.active_needs_host());
        assert_eq!(
            session.js.active_pattern().expect("active drop").steps,
            Some(Fraction::int(2)),
            "drop metadata must come from StepJoin's cycle-zero slice"
        );
        assert_eq!(
            shows(&session, Fraction::ZERO, Fraction::int(2)),
            [
                "[ 0/1 → 1/2 | 1 ]",
                "[ 1/2 → 1/1 | 2 ]",
                "[ 1/1 → 3/2 | 1 ]",
                "[ 3/2 → 2/1 | 2 ]",
            ],
            "joined drop query stopped selecting from its begin cycle"
        );
        assert_eq!(
            shows(&session, Fraction::ONE, Fraction::int(2)),
            ["[ 1/1 → 2/1 | 2 ]"],
            "separate drop query lost cycle one's amount"
        );
        let drop_play = session.play(4.0).expect("schedule patterned drop");
        let drop_onsets = drop_play
            .onsets
            .iter()
            .map(|onset| {
                (
                    onset.whole_begin.as_str(),
                    onset.value_show.as_str(),
                    onset.target_time,
                    onset.duration_secs,
                )
            })
            .collect::<Vec<_>>();
        assert_eq!(
            drop_onsets,
            [
                ("0/1", "1", 0.0, 1.0),
                ("1/2", "2", 1.0, 1.0),
                ("1/1", "2", 2.0, 2.0),
                // As above, the preceding factor-2 lookahead owns the exact
                // boundary onset.
                ("2/1", "2", 4.0, 2.0),
            ],
            "scheduler stopped preserving patterned drop's query windows"
        );
    }

    #[test]
    fn raw_take_drop_reach_session_query_play_without_stepwise_expansion_charge() {
        const TAKE_HAPS: &[&str] = &["[ 0/1 → 1/2 | d ]", "[ 1/2 → 1/1 | e ]"];
        const DROP_HAPS: &[&str] = &[
            "[ 0/1 → 1/3 | c ]",
            "[ 1/3 → 2/3 | d ]",
            "[ 2/3 → 1/1 | e ]",
        ];

        for (source, expected_haps, expected_steps, expected_onsets) in [
            (
                r#"(() => {
                  if (Object.hasOwn(globalThis, '_take')
                      || Object.hasOwn(rustelScope, '_take')
                      || typeof globalThis._take !== 'undefined'
                      || typeof rustelScope._take !== 'undefined') {
                    throw new Error('raw take leaked outside Pattern.prototype');
                  }
                  return sequence('a','b','c','d','e')._take(-2);
                })()"#,
                TAKE_HAPS,
                2,
                vec![("0/1", "d"), ("1/2", "e"), ("1/1", "d")],
            ),
            (
                r#"(() => {
                  if (Object.hasOwn(globalThis, '_drop')
                      || Object.hasOwn(rustelScope, '_drop')
                      || typeof globalThis._drop !== 'undefined'
                      || typeof rustelScope._drop !== 'undefined') {
                    throw new Error('raw drop leaked outside Pattern.prototype');
                  }
                  return sequence('a','b','c','d','e')._drop(2);
                })()"#,
                DROP_HAPS,
                3,
                vec![("0/1", "c"), ("1/3", "d"), ("2/3", "e"), ("1/1", "c")],
            ),
        ] {
            let mut session = Session::new().expect("raw take/drop session");
            session
                .evaluate(source)
                .unwrap_or_else(|error| panic!("evaluate raw take/drop {source}: {error}"));
            assert!(
                !session.active_needs_host(),
                "{source}: scalar raw take/drop retained the JS host"
            );
            assert_eq!(
                session
                    .js
                    .active_pattern()
                    .expect("active raw take/drop")
                    .steps,
                Some(Fraction::int(expected_steps))
            );
            assert_eq!(
                session
                    .query(Fraction::ZERO, Fraction::ONE)
                    .expect("query raw take/drop")
                    .into_iter()
                    .map(|hap| hap.show())
                    .collect::<Vec<_>>(),
                expected_haps,
                "{source}: raw take/drop query timing changed"
            );
            assert_eq!(
                session
                    .play(2.0)
                    .expect("schedule raw take/drop")
                    .onsets
                    .iter()
                    .map(|onset| (onset.whole_begin.as_str(), onset.value_show.as_str()))
                    .collect::<Vec<_>>(),
                expected_onsets,
                "{source}: raw take/drop scheduler onsets changed"
            );
        }

        // Raw take/drop only build zoom/take graphs; unlike shrink/grow they
        // do not materialise one entry per declared step.  Crossing the shared
        // expansion threshold is therefore a successful O(1) query, not a
        // `StepwiseExpansion` refusal or a new resource operation.
        let over = rustel_core::MAX_STEPWISE_ENTRIES + 1;
        for (name, expected_steps) in [("_take", 1), ("_drop", over - 1)] {
            let mut session = Session::new().expect("raw take/drop resource session");
            session
                .evaluate(&format!("pure('x').setSteps({over}).{name}(1)"))
                .unwrap_or_else(|error| panic!("construct pool-neutral {name}: {error}"));
            assert!(!session.active_needs_host());
            assert_eq!(
                session
                    .js
                    .active_pattern()
                    .expect("active pool-neutral raw graph")
                    .steps,
                Some(Fraction::int(i128::from(expected_steps)))
            );
            assert_eq!(
                session
                    .query(Fraction::ZERO, Fraction::ONE)
                    .unwrap_or_else(|error| panic!("query pool-neutral {name}: {error}"))
                    .len(),
                1
            );
            assert!(
                session.scheduler.refusal().is_none(),
                "{name}: raw zoom/take graph invented a stepwise refusal"
            );
        }
    }

    #[test]
    fn raw_extend_replicate_reach_session_query_play_without_stepwise_expansion_charge() {
        const EXTEND_HAPS: &[&str] = &[
            "[ 0/1 → 1/4 | a ]",
            "[ 1/4 → 1/2 | b ]",
            "[ 1/2 → 3/4 | c ]",
            "[ 3/4 → 1/1 | d ]",
        ];
        const REPLICATE_HAPS: &[&str] = &[
            "[ 0/1 → 1/4 | a ]",
            "[ 1/4 → 1/2 | b ]",
            "[ 1/2 → 3/4 | a ]",
            "[ 3/4 → 1/1 | b ]",
        ];

        for (name, expected_haps, expected_onsets) in [
            (
                "_extend",
                EXTEND_HAPS,
                vec![
                    ("0/1", "a"),
                    ("1/4", "b"),
                    ("1/2", "c"),
                    ("3/4", "d"),
                    ("1/1", "a"),
                ],
            ),
            (
                "_replicate",
                REPLICATE_HAPS,
                vec![
                    ("0/1", "a"),
                    ("1/4", "b"),
                    ("1/2", "a"),
                    ("3/4", "b"),
                    ("1/1", "c"),
                ],
            ),
        ] {
            let source = format!(
                r#"(() => {{
                  if (Object.hasOwn(globalThis, '{name}')
                      || Object.hasOwn(rustelScope, '{name}')
                      || typeof globalThis.{name} !== 'undefined'
                      || typeof rustelScope.{name} !== 'undefined') {{
                    throw new Error('raw stepwise chain leaked outside Pattern.prototype');
                  }}
                  return slowcat(sequence('a','b'),sequence('c','d')).{name}(2);
                }})()"#
            );
            let mut session = Session::new().expect("raw extend/replicate session");
            session
                .evaluate(&source)
                .unwrap_or_else(|error| panic!("evaluate {name}: {error}"));
            assert!(
                !session.active_needs_host(),
                "{name}: stable scalar chain retained the JS host"
            );
            assert_eq!(
                session
                    .js
                    .active_pattern()
                    .expect("active raw extend/replicate")
                    .steps,
                Some(Fraction::int(4)),
                "{name}: final expand metadata changed"
            );
            assert_eq!(
                session
                    .query(Fraction::ZERO, Fraction::ONE)
                    .unwrap_or_else(|error| panic!("query {name}: {error}"))
                    .into_iter()
                    .map(|hap| hap.show())
                    .collect::<Vec<_>>(),
                expected_haps,
                "{name}: cycle-varying query semantics changed"
            );
            assert_eq!(
                session
                    .play(2.0)
                    .unwrap_or_else(|error| panic!("schedule {name}: {error}"))
                    .onsets
                    .iter()
                    .map(|onset| (onset.whole_begin.as_str(), onset.value_show.as_str()))
                    .collect::<Vec<_>>(),
                expected_onsets,
                "{name}: scheduler onsets changed"
            );
        }

        // These bodies compose O(1) graph transforms. Large declared metadata
        // must not borrow a materialising stepwise operation or invent a new
        // QueryLimit variant merely because the raw slot is stepwise-adjacent.
        let over = rustel_core::MAX_STEPWISE_ENTRIES + 1;
        for name in ["_extend", "_replicate"] {
            let mut session = Session::new().expect("raw chain resource session");
            session
                .evaluate(&format!("pure('x').setSteps({over}).{name}(1)"))
                .unwrap_or_else(|error| panic!("construct pool-neutral {name}: {error}"));
            assert!(!session.active_needs_host());
            assert_eq!(
                session
                    .js
                    .active_pattern()
                    .expect("active pool-neutral raw chain")
                    .steps,
                Some(Fraction::int(i128::from(over)))
            );
            assert_eq!(
                session
                    .query(Fraction::ZERO, Fraction::ONE)
                    .unwrap_or_else(|error| panic!("query pool-neutral {name}: {error}"))
                    .len(),
                1
            );
            assert!(
                session.scheduler.refusal().is_none(),
                "{name}: raw chain invented a stepwise refusal"
            );
        }
    }

    #[test]
    fn raw_expand_contract_and_with_steps_reach_session_query_play_without_charge() {
        const EXPANDED_HAPS: &[&str] = &[
            "[ 0/1 → 2/5 | a ]",
            "[ 2/5 → 4/5 | b ]",
            "[ 4/5 → 1/1 | z ]",
        ];
        const CONTRACTED_HAPS: &[&str] = &[
            "[ 0/1 → 1/4 | a ]",
            "[ 1/4 → 1/2 | b ]",
            "[ 1/2 → 1/1 | z ]",
        ];

        for (name, transform, expected_steps, expected_haps, expected_onsets) in [
            (
                "withSteps",
                "sequence('a','b').withSteps(steps => steps.mul(2))",
                Fraction::int(5),
                EXPANDED_HAPS,
                vec![("0/1", "a"), ("2/5", "b"), ("4/5", "z"), ("1/1", "a")],
            ),
            (
                "_expand",
                "sequence('a','b')._expand(2)",
                Fraction::int(5),
                EXPANDED_HAPS,
                vec![("0/1", "a"), ("2/5", "b"), ("4/5", "z"), ("1/1", "a")],
            ),
            (
                "_contract",
                "sequence('a','b')._contract(2)",
                Fraction::int(2),
                CONTRACTED_HAPS,
                vec![("0/1", "a"), ("1/4", "b"), ("1/2", "z"), ("1/1", "a")],
            ),
        ] {
            let source = format!(
                r#"(() => {{
                  if (Object.hasOwn(globalThis, '{name}')
                      || Object.hasOwn(rustelScope, '{name}')
                      || typeof globalThis.{name} !== 'undefined'
                      || typeof rustelScope.{name} !== 'undefined') {{
                    throw new Error('prototype-only metadata helper leaked');
                  }}
                  return stepcat({transform}, 'z');
                }})()"#
            );
            let mut session = Session::new().expect("raw metadata session");
            session
                .evaluate(&source)
                .unwrap_or_else(|error| panic!("evaluate {name}: {error}"));
            assert!(
                !session.active_needs_host(),
                "{name}: eager metadata transform retained the JS host"
            );
            assert_eq!(
                session
                    .js
                    .active_pattern()
                    .expect("active raw metadata stepcat")
                    .steps,
                Some(expected_steps),
                "{name}: metadata did not reach stepcat"
            );
            assert_eq!(
                session
                    .query(Fraction::ZERO, Fraction::ONE)
                    .unwrap_or_else(|error| panic!("query {name}: {error}"))
                    .into_iter()
                    .map(|hap| hap.show())
                    .collect::<Vec<_>>(),
                expected_haps,
                "{name}: Session query timing changed"
            );
            assert_eq!(
                session
                    .play(2.0)
                    .unwrap_or_else(|error| panic!("schedule {name}: {error}"))
                    .onsets
                    .iter()
                    .map(|onset| (onset.whole_begin.as_str(), onset.value_show.as_str()))
                    .collect::<Vec<_>>(),
                expected_onsets,
                "{name}: Session scheduler timing changed"
            );
        }

        // These operations only replace declared metadata and do not
        // materialise one entry per step. Crossing the shared expansion limit
        // must therefore stay a successful one-hap query with no refusal.
        let over = rustel_core::MAX_STEPWISE_ENTRIES + 1;
        for (name, transform, expected_steps) in [
            (
                "withSteps",
                format!("pure('x').setSteps({over}).withSteps(s => s.mul(2))"),
                Fraction::int(i128::from(over) * 2),
            ),
            (
                "_expand",
                format!("pure('x').setSteps({over})._expand(2)"),
                Fraction::int(i128::from(over) * 2),
            ),
            (
                "_contract",
                format!("pure('x').setSteps({over})._contract(2)"),
                Fraction::new(i128::from(over), 2),
            ),
        ] {
            let mut session = Session::new().expect("pool-neutral metadata session");
            session
                .evaluate(&transform)
                .unwrap_or_else(|error| panic!("construct pool-neutral {name}: {error}"));
            assert!(!session.active_needs_host());
            assert_eq!(
                session
                    .js
                    .active_pattern()
                    .expect("active pool-neutral metadata graph")
                    .steps,
                Some(expected_steps)
            );
            assert_eq!(
                session
                    .query(Fraction::ZERO, Fraction::ONE)
                    .unwrap_or_else(|error| panic!("query pool-neutral {name}: {error}"))
                    .len(),
                1
            );
            assert!(
                session.scheduler.refusal().is_none(),
                "{name}: metadata-only transform invented a resource refusal"
            );
        }
    }

    #[test]
    fn raw_range_pair_reaches_session_query_play_without_resource_charge() {
        const HAPS: &[&str] = &[
            "[ 0/1 → 1/4 | 10 ]",
            "[ 1/4 → 1/2 | 15 ]",
            "[ 1/2 → 3/4 | 20 ]",
            "[ 3/4 → 1/1 | z ]",
        ];
        const ONSETS: &[(&str, &str)] = &[
            ("0/1", "10"),
            ("1/4", "15"),
            ("1/2", "20"),
            ("3/4", "z"),
            ("1/1", "10"),
        ];

        for (name, transform) in [
            ("_range", "sequence(0,.5,1)._range(10,20)"),
            ("_range2", "sequence(-1,0,1)._range2(10,20)"),
        ] {
            let source = format!(
                r#"(() => {{
                  if (Object.hasOwn(globalThis, '{name}')
                      || Object.hasOwn(rustelScope, '{name}')
                      || typeof globalThis.{name} !== 'undefined'
                      || typeof rustelScope.{name} !== 'undefined'
                      || Object.hasOwn(Pattern.prototype, '_rangex')
                      || typeof Pattern.prototype._rangex !== 'undefined') {{
                    throw new Error('bounded raw range surface leaked');
                  }}
                  return stepcat({transform}, 'z');
                }})()"#
            );
            let mut session = Session::new().expect("raw range session");
            session
                .evaluate(&source)
                .unwrap_or_else(|error| panic!("evaluate {name}: {error}"));
            assert!(
                !session.active_needs_host(),
                "{name}: stable scalar range retained the JS host"
            );
            assert_eq!(
                session
                    .js
                    .active_pattern()
                    .expect("active raw range stepcat")
                    .steps,
                Some(Fraction::int(4)),
                "{name}: source steps did not reach stepcat"
            );
            assert_eq!(
                session
                    .query(Fraction::ZERO, Fraction::ONE)
                    .unwrap_or_else(|error| panic!("query {name}: {error}"))
                    .into_iter()
                    .map(|hap| hap.show())
                    .collect::<Vec<_>>(),
                HAPS,
                "{name}: Session query values or timing changed"
            );
            assert_eq!(
                session
                    .play(2.0)
                    .unwrap_or_else(|error| panic!("schedule {name}: {error}"))
                    .onsets
                    .iter()
                    .map(|onset| (onset.whole_begin.as_str(), onset.value_show.as_str()))
                    .collect::<Vec<_>>(),
                ONSETS,
                "{name}: Session scheduler onsets changed"
            );
        }

        // Range is an O(1) composer chain. Large declared metadata must stay a
        // successful one-hap graph and must not borrow a stepwise operation or
        // introduce a new QueryLimit/refusal name.
        let over = rustel_core::MAX_STEPWISE_ENTRIES + 1;
        for (name, source) in [
            ("_range", format!("pure(.5).setSteps({over})._range(10,20)")),
            (
                "_range2",
                format!("pure(0).setSteps({over})._range2(10,20)"),
            ),
        ] {
            let mut session = Session::new().expect("pool-neutral raw range session");
            session
                .evaluate(&source)
                .unwrap_or_else(|error| panic!("construct pool-neutral {name}: {error}"));
            assert!(!session.active_needs_host());
            assert_eq!(
                session
                    .js
                    .active_pattern()
                    .expect("active pool-neutral raw range")
                    .steps,
                Some(Fraction::int(i128::from(over)))
            );
            assert_eq!(
                session
                    .query(Fraction::ZERO, Fraction::ONE)
                    .unwrap_or_else(|error| panic!("query pool-neutral {name}: {error}"))
                    .len(),
                1
            );
            assert!(
                session.scheduler.refusal().is_none(),
                "{name}: range invented a resource refusal"
            );
        }
    }

    #[test]
    fn raw_apply_reaches_session_query_play_and_preserves_nested_resource_limits() {
        const HAPS: &[&str] = &[
            "[ 0/1 → 1/3 | a ]",
            "[ 1/3 → 2/3 | b ]",
            "[ 2/3 → 1/1 | z ]",
        ];
        const ONSETS: &[(&str, &str)] = &[("0/1", "a"), ("1/3", "b"), ("2/3", "z"), ("1/1", "a")];
        let source = r#"(() => {
          if (Object.hasOwn(globalThis, '_apply')
              || Object.hasOwn(rustelScope, '_apply')
              || typeof globalThis._apply !== 'undefined'
              || typeof rustelScope._apply !== 'undefined'
              || Object.hasOwn(Pattern.prototype, '_swingBy')
              || typeof Pattern.prototype._swingBy !== 'undefined') {
            throw new Error('bounded raw apply surface leaked');
          }
          return stepcat(
            pure('ignored')._apply(value => value, sequence('a', 'b')),
            'z'
          );
        })()"#;
        let mut session = Session::new().expect("raw apply session");
        session
            .evaluate(source)
            .expect("evaluate raw apply product score");
        assert!(
            !session.active_needs_host(),
            "eager raw apply callback leaked into its native terminal"
        );
        assert_eq!(
            session
                .js
                .active_pattern()
                .expect("active raw apply stepcat")
                .steps,
            Some(Fraction::int(3))
        );
        assert_eq!(
            session
                .query(Fraction::ZERO, Fraction::ONE)
                .expect("query raw apply")
                .into_iter()
                .map(|hap| hap.show())
                .collect::<Vec<_>>(),
            HAPS,
            "Session raw apply query changed"
        );
        assert_eq!(
            session
                .play(2.0)
                .expect("schedule raw apply")
                .onsets
                .iter()
                .map(|onset| (onset.whole_begin.as_str(), onset.value_show.as_str()))
                .collect::<Vec<_>>(),
            ONSETS,
            "Session raw apply scheduler onsets changed"
        );

        // `_apply` itself only calls and returns: large metadata on the exact
        // selected terminal must not create a new resource operation.
        let over = rustel_core::MAX_STEPWISE_ENTRIES + 1;
        let mut neutral = Session::new().expect("pool-neutral raw apply session");
        neutral
            .evaluate(&format!(
                "pure('x').setSteps({over})._apply(value => value)"
            ))
            .expect("construct pool-neutral raw apply");
        assert!(!neutral.active_needs_host());
        assert_eq!(
            neutral
                .js
                .active_pattern()
                .expect("active pool-neutral raw apply")
                .steps,
            Some(Fraction::int(i128::from(over)))
        );
        assert_eq!(
            neutral
                .query(Fraction::ZERO, Fraction::ONE)
                .expect("query pool-neutral raw apply")
                .len(),
            1
        );
        assert!(
            neutral.scheduler.refusal().is_none(),
            "raw apply invented a resource refusal"
        );

        // Work selected by the eager callback keeps the selected operation's
        // existing bound and attribution; `_apply` is not a laundering layer.
        let mut nested = Session::new().expect("nested raw apply refusal session");
        nested
            .evaluate(&format!("gap({over})._apply(value => value.shrink(0))"))
            .expect("construct callback-selected shrink refusal");
        assert!(!nested.active_needs_host());
        let error = nested
            .schedule_at(0.0)
            .expect_err("callback-selected MAX+1 shrink must refuse");
        let RuntimeError::ResourceLimit(message) = error else {
            panic!("callback-selected shrink lost its resource type: {error:?}");
        };
        assert!(
            message.contains("shrink/grow")
                && message.contains(&over.to_string())
                && message.contains(&rustel_core::MAX_STEPWISE_ENTRIES.to_string()),
            "wrong callback-selected shrink refusal: {message}"
        );
        assert!(
            matches!(
                nested.scheduler.refusal(),
                Some(rustel_core::QueryLimit::StepwiseExpansion {
                    operation,
                    minimum_entries,
                    limit,
                }) if *operation == "shrink/grow"
                    && *minimum_entries == over
                    && *limit == rustel_core::MAX_STEPWISE_ENTRIES
            ),
            "raw apply changed nested refusal attribution: {:?}",
            nested.scheduler.refusal()
        );
    }

    #[test]
    fn raw_when_reaches_session_branches_and_preserves_nested_resource_limits() {
        const HAPS: &[&str] = &[
            "[ 0/1 → 1/3 | a ]",
            "[ 1/3 → 2/3 | b ]",
            "[ 2/3 → 1/1 | z ]",
        ];
        const ONSETS: &[(&str, &str)] = &[("0/1", "a"), ("1/3", "b"), ("2/3", "z"), ("1/1", "a")];
        let source = r#"(() => {
          if (Object.hasOwn(globalThis, '_when')
              || Object.hasOwn(rustelScope, '_when')
              || typeof globalThis._when !== 'undefined'
              || typeof rustelScope._when !== 'undefined'
              || typeof Pattern.prototype._when !== 'function'
              || Object.hasOwn(Pattern.prototype, '_swingBy')
              || typeof Pattern.prototype._swingBy !== 'undefined') {
            throw new Error('bounded raw when surface leaked');
          }
          globalThis.rawWhenSessionFalseHits = 0;
          globalThis.rawWhenSessionTrueHits = 0;
          const falseSelected = pure('outer')._when(
            0n,
            () => {
              rawWhenSessionFalseHits++;
              throw new Error('false callback ran');
            },
            sequence('a', 'b')
          );
          const guardedTruthy = new Proxy({}, {
            get() { throw new Error('ToBoolean read its object'); }
          });
          const trueSelected = pure('outer')._when(
            guardedTruthy,
            value => {
              rawWhenSessionTrueHits++;
              return value;
            },
            pure('z')
          );
          if (rawWhenSessionFalseHits !== 0 || rawWhenSessionTrueHits !== 1) {
            throw new Error('raw when callback phase changed');
          }
          return stepcat(falseSelected, trueSelected);
        })()"#;
        let mut session = Session::new().expect("raw when session");
        session
            .evaluate(source)
            .expect("evaluate raw when product score");
        assert_eq!(session.js.get_number("rawWhenSessionFalseHits"), Some(0.0));
        assert_eq!(session.js.get_number("rawWhenSessionTrueHits"), Some(1.0));
        assert!(
            !session.active_needs_host(),
            "eager raw when branches leaked into the selected native terminals"
        );
        assert_eq!(
            session
                .js
                .active_pattern()
                .expect("active raw when stepcat")
                .steps,
            Some(Fraction::int(3))
        );
        assert_eq!(
            session
                .query(Fraction::ZERO, Fraction::ONE)
                .expect("query raw when")
                .into_iter()
                .map(|hap| hap.show())
                .collect::<Vec<_>>(),
            HAPS,
            "Session raw when query changed"
        );
        assert_eq!(
            session
                .play(2.0)
                .expect("schedule raw when")
                .onsets
                .iter()
                .map(|onset| (onset.whole_begin.as_str(), onset.value_show.as_str()))
                .collect::<Vec<_>>(),
            ONSETS,
            "Session raw when scheduler onsets changed"
        );

        // The false branch suppresses the callback body, and `_when` itself
        // adds no resource operation. Large metadata on the selected terminal
        // therefore remains a successful one-hap graph.
        let over = rustel_core::MAX_STEPWISE_ENTRIES + 1;
        let mut neutral = Session::new().expect("pool-neutral raw when session");
        neutral
            .evaluate(&format!(
                r#"(() => {{
                  globalThis.rawWhenSuppressedHits = 0;
                  const result = gap({over})._when(
                    false,
                    value => {{
                      rawWhenSuppressedHits++;
                      return value.shrink(0);
                    }},
                    pure('safe').setSteps({over})
                  );
                  if (rawWhenSuppressedHits !== 0) {{
                    throw new Error('false branch executed nested work');
                  }}
                  return result;
                }})()"#
            ))
            .expect("construct pool-neutral false raw when");
        assert_eq!(neutral.js.get_number("rawWhenSuppressedHits"), Some(0.0));
        assert!(!neutral.active_needs_host());
        assert_eq!(
            neutral
                .js
                .active_pattern()
                .expect("active pool-neutral raw when")
                .steps,
            Some(Fraction::int(i128::from(over)))
        );
        assert_eq!(
            neutral
                .query(Fraction::ZERO, Fraction::ONE)
                .expect("query pool-neutral raw when")
                .len(),
            1
        );
        assert!(
            neutral.scheduler.refusal().is_none(),
            "raw when invented a resource refusal"
        );

        // Work actually selected by the true branch keeps the chosen
        // operation's existing type, limit, and attribution. This does not
        // make arbitrary callbacks resource-free or generally bounded.
        let mut nested = Session::new().expect("nested raw when refusal session");
        nested
            .evaluate(&format!(
                "gap({over})._when(true, value => value.shrink(0))"
            ))
            .expect("construct true-branch shrink refusal");
        assert!(!nested.active_needs_host());
        let error = nested
            .schedule_at(0.0)
            .expect_err("true-branch MAX+1 shrink must refuse");
        let RuntimeError::ResourceLimit(message) = error else {
            panic!("true-branch shrink lost its resource type: {error:?}");
        };
        assert!(
            message.contains("shrink/grow")
                && message.contains(&over.to_string())
                && message.contains(&rustel_core::MAX_STEPWISE_ENTRIES.to_string()),
            "wrong true-branch shrink refusal: {message}"
        );
        assert!(
            matches!(
                nested.scheduler.refusal(),
                Some(rustel_core::QueryLimit::StepwiseExpansion {
                    operation,
                    minimum_entries,
                    limit,
                }) if *operation == "shrink/grow"
                    && *minimum_entries == over
                    && *limit == rustel_core::MAX_STEPWISE_ENTRIES
            ),
            "raw when changed nested refusal attribution: {:?}",
            nested.scheduler.refusal()
        );
    }

    #[test]
    fn raw_never_always_reach_session_and_preserve_selected_resource_limits() {
        const HAPS: &[&str] = &[
            "[ 0/1 → 1/3 | a ]",
            "[ 1/3 → 2/3 | b ]",
            "[ 2/3 → 1/1 | z ]",
        ];
        const ONSETS: &[(&str, &str)] = &[("0/1", "a"), ("1/3", "b"), ("2/3", "z"), ("1/1", "a")];
        let source = r#"(() => {
          const names = ['never', '_never', 'always', '_always'];
          const order = Object.getOwnPropertyNames(Pattern.prototype)
            .filter(name => names.includes(name));
          if (order.join(',') !== names.join(',')
              || Object.hasOwn(globalThis, '_never')
              || Object.hasOwn(globalThis, '_always')
              || Object.hasOwn(rustelScope, '_never')
              || Object.hasOwn(rustelScope, '_always')
              || typeof globalThis._never !== 'undefined'
              || typeof globalThis._always !== 'undefined'
              || typeof rustelScope._never !== 'undefined'
              || typeof rustelScope._always !== 'undefined') {
            throw new Error('bounded raw never/always surface changed');
          }
          let ignoredTouches = 0;
          const ignored = new Proxy(function () {}, {
            apply() { ignoredTouches++; throw new Error('never callback ran'); },
            get() { ignoredTouches++; throw new Error('never callback read'); },
          });
          let alwaysHits = 0;
          const neverSelected = pure('outer')._never(
            ignored, sequence('a', 'b'), { marker: 'ignored extra' }
          );
          const alwaysSelected = pure('outer')._always(
            value => { alwaysHits++; return value; },
            pure('z'),
            { marker: 'ignored extra' }
          );
          if (ignoredTouches !== 0 || alwaysHits !== 1) {
            throw new Error('raw never/always callback phase changed');
          }
          return stepcat(neverSelected, alwaysSelected);
        })()"#;
        let mut session = Session::new().expect("raw never/always session");
        session
            .evaluate(source)
            .expect("evaluate raw never/always product score");
        assert!(
            !session.active_needs_host(),
            "raw never/always construction values leaked into native terminals"
        );
        assert_eq!(
            session
                .js
                .active_pattern()
                .expect("active raw never/always stepcat")
                .steps,
            Some(Fraction::int(3))
        );
        assert_eq!(
            session
                .query(Fraction::ZERO, Fraction::ONE)
                .expect("query raw never/always")
                .into_iter()
                .map(|hap| hap.show())
                .collect::<Vec<_>>(),
            HAPS,
            "Session raw never/always query changed"
        );
        assert_eq!(
            session
                .play(2.0)
                .expect("schedule raw never/always")
                .onsets
                .iter()
                .map(|onset| (onset.whole_begin.as_str(), onset.value_show.as_str()))
                .collect::<Vec<_>>(),
            ONSETS,
            "Session raw never/always scheduler onsets changed"
        );

        // The pair adds no resource operation: `_never` merely selects an
        // already-built target and `_always` merely calls and returns. Large
        // metadata on an otherwise bounded selected terminal stays valid.
        let over = rustel_core::MAX_STEPWISE_ENTRIES + 1;
        for (name, source) in [
            (
                "_never",
                format!("pure('outer')._never(null, pure('safe').setSteps({over}))"),
            ),
            (
                "_always",
                format!("pure('safe').setSteps({over})._always(value => value)"),
            ),
        ] {
            let mut neutral = Session::new().expect("pool-neutral raw never/always session");
            neutral
                .evaluate(&source)
                .unwrap_or_else(|error| panic!("construct pool-neutral {name}: {error}"));
            assert!(
                !neutral.active_needs_host(),
                "{name}: selected graph needs host"
            );
            assert_eq!(
                neutral
                    .js
                    .active_pattern()
                    .expect("active pool-neutral raw never/always")
                    .steps,
                Some(Fraction::int(i128::from(over)))
            );
            assert_eq!(
                neutral
                    .query(Fraction::ZERO, Fraction::ONE)
                    .unwrap_or_else(|error| panic!("query pool-neutral {name}: {error}"))
                    .len(),
                1
            );
            assert!(
                neutral.scheduler.refusal().is_none(),
                "{name} invented a resource refusal"
            );
        }

        // Caller-evaluated work selected by `_never`, and callback-selected
        // work returned by `_always`, retain the chosen operation's existing
        // type, limit, and attribution. This is not an arbitrary-callback
        // resource-free or general-bound claim.
        for (name, source) in [
            (
                "_never",
                format!("pure('outer')._never(null, gap({over}).shrink(0))"),
            ),
            (
                "_always",
                format!("gap({over})._always(value => value.shrink(0))"),
            ),
        ] {
            let mut nested = Session::new().expect("nested raw never/always refusal session");
            nested
                .evaluate(&source)
                .unwrap_or_else(|error| panic!("construct nested {name}: {error}"));
            assert!(
                !nested.active_needs_host(),
                "{name}: selected graph needs host"
            );
            let error = nested
                .schedule_at(0.0)
                .expect_err("selected MAX+1 shrink must refuse");
            let RuntimeError::ResourceLimit(message) = error else {
                panic!("{name}: selected shrink lost its resource type: {error:?}");
            };
            assert!(
                message.contains("shrink/grow")
                    && message.contains(&over.to_string())
                    && message.contains(&rustel_core::MAX_STEPWISE_ENTRIES.to_string()),
                "{name}: wrong selected shrink refusal: {message}"
            );
            assert!(
                matches!(
                    nested.scheduler.refusal(),
                    Some(rustel_core::QueryLimit::StepwiseExpansion {
                        operation,
                        minimum_entries,
                        limit,
                    }) if *operation == "shrink/grow"
                        && *minimum_entries == over
                        && *limit == rustel_core::MAX_STEPWISE_ENTRIES
                ),
                "{name}: changed selected refusal attribution: {:?}",
                nested.scheduler.refusal()
            );
        }
    }

    #[test]
    fn raw_swing_reaches_session_with_scalar_steps_and_existing_resource_attribution() {
        const HAPS: &[&str] = &[
            "[ (0/1 → 1/8) ⇝ 1/4 | a ]",
            "[ 1/24 ⇜ (1/8 → 1/4) ⇝ 7/24 | a ]",
            "[ (1/4 → 3/8) ⇝ 1/2 | b ]",
            "[ 7/24 ⇜ (3/8 → 1/2) ⇝ 13/24 | b ]",
            "[ (1/2 → 5/8) ⇝ 3/4 | c ]",
            "[ 13/24 ⇜ (5/8 → 3/4) ⇝ 19/24 | c ]",
            "[ (3/4 → 7/8) ⇝ 1/1 | d ]",
            "[ 19/24 ⇜ (7/8 → 1/1) ⇝ 25/24 | d ]",
        ];
        const ONSETS: &[(&str, &str)] = &[
            ("0/1", "a"),
            ("1/4", "b"),
            ("1/2", "c"),
            ("3/4", "d"),
            ("1/1", "a"),
        ];
        let source = r#"(() => {
          const names = ['swing', '_swing'];
          const order = Object.getOwnPropertyNames(Pattern.prototype)
            .filter(name => names.includes(name));
          if (order.join(',') !== names.join(',')
              || Object.hasOwn(globalThis, '_swing')
              || Object.hasOwn(rustelScope, '_swing')
              || typeof globalThis._swing !== 'undefined'
              || typeof rustelScope._swing !== 'undefined'
              || Object.hasOwn(Pattern.prototype, '_swingBy')
              || typeof Pattern.prototype._swingBy !== 'undefined') {
            throw new Error('bounded raw swing surface changed');
          }
          return sequence('a','b','c','d').setSteps(7)._swing(4);
        })()"#;
        let mut session = Session::new().expect("raw swing session");
        session
            .evaluate(source)
            .expect("evaluate raw swing product score");
        assert!(
            !session.active_needs_host(),
            "scalar raw swing unexpectedly retained the JS host"
        );
        assert_eq!(
            session.js.active_pattern().expect("active raw swing").steps,
            Some(Fraction::int(7)),
            "raw swing lost source steps"
        );
        assert_eq!(
            session
                .query(Fraction::ZERO, Fraction::ONE)
                .expect("query raw swing")
                .into_iter()
                .map(|hap| hap.show())
                .collect::<Vec<_>>(),
            HAPS,
            "Session raw swing query changed"
        );
        assert_eq!(
            session
                .play(2.0)
                .expect("schedule raw swing")
                .onsets
                .iter()
                .map(|onset| (onset.whole_begin.as_str(), onset.value_show.as_str()))
                .collect::<Vec<_>>(),
            ONSETS,
            "Session raw swing scheduler onsets changed"
        );

        let mut zero = Session::new().expect("zero raw swing session");
        zero.evaluate("sequence('a','b').setSteps(7)._swing(0)")
            .expect("evaluate zero raw swing");
        assert!(!zero.active_needs_host());
        assert_eq!(
            zero.js
                .active_pattern()
                .expect("active zero raw swing")
                .steps,
            Some(Fraction::ONE),
            "zero raw swing replaced constructed silence metadata"
        );
        assert!(
            zero.query(Fraction::ZERO, Fraction::ONE)
                .expect("query zero raw swing")
                .is_empty(),
            "zero raw swing produced haps"
        );

        // `_swing` adds only one dynamic handoff to the existing public
        // swing graph. Oversized declared metadata alone must not invent a
        // raw-specific resource operation or refusal.
        let over = rustel_core::MAX_STEPWISE_ENTRIES + 1;
        let mut neutral = Session::new().expect("pool-neutral raw swing session");
        neutral
            .evaluate(&format!("pure('safe').setSteps({over})._swing(4)"))
            .expect("construct pool-neutral raw swing");
        assert!(!neutral.active_needs_host());
        assert_eq!(
            neutral
                .js
                .active_pattern()
                .expect("active pool-neutral raw swing")
                .steps,
            Some(Fraction::int(i128::from(over)))
        );
        assert_eq!(
            neutral
                .query(Fraction::ZERO, Fraction::ONE)
                .expect("query pool-neutral raw swing")
                .len(),
            8,
            "raw swing's fixed scalar graph changed"
        );
        assert!(
            neutral.scheduler.refusal().is_none(),
            "raw swing invented a resource refusal"
        );

        // Existing nested work keeps its own operation name/type/limit after
        // the swing graph wraps it. This is not a general density bound.
        let mut nested = Session::new().expect("nested raw swing refusal session");
        nested
            .evaluate(&format!("gap({over}).shrink(0)._swing(4)"))
            .expect("construct nested raw swing refusal");
        assert!(!nested.active_needs_host());
        let error = nested
            .schedule_at(0.0)
            .expect_err("nested MAX+1 shrink must refuse through raw swing");
        let RuntimeError::ResourceLimit(message) = error else {
            panic!("nested raw swing lost resource type: {error:?}");
        };
        assert!(
            message.contains("shrink/grow")
                && message.contains(&over.to_string())
                && message.contains(&rustel_core::MAX_STEPWISE_ENTRIES.to_string()),
            "wrong nested raw swing refusal: {message}"
        );
        assert!(
            matches!(
                nested.scheduler.refusal(),
                Some(rustel_core::QueryLimit::StepwiseExpansion {
                    operation,
                    minimum_entries,
                    limit,
                }) if *operation == "shrink/grow"
                    && *minimum_entries == over
                    && *limit == rustel_core::MAX_STEPWISE_ENTRIES
            ),
            "raw swing changed nested refusal attribution: {:?}",
            nested.scheduler.refusal()
        );
    }

    #[test]
    fn raw_signal_quartet_reaches_session_query_play_and_preserves_nested_attribution() {
        const SURFACE: &str = r#"
          const projected = [
            'often', '_often', 'rarely', '_rarely',
            'almostNever', '_almostNever',
            'almostAlways', '_almostAlways'
          ];
          const order = Object.getOwnPropertyNames(Pattern.prototype)
            .filter(name => projected.includes(name));
          if (order.join(',') !== projected.join(',')) {
            throw new Error('raw signal quartet order changed');
          }
          for (const raw of projected.filter(name => name.startsWith('_'))) {
            if (Object.hasOwn(globalThis, raw)
                || Object.hasOwn(rustelScope, raw)
                || typeof globalThis[raw] !== 'undefined'
                || typeof rustelScope[raw] !== 'undefined') {
              throw new Error(`raw signal destination leaked: ${raw}`);
            }
          }
        "#;
        for (raw_name, query_values, play_values) in [
            (
                "_often",
                ["_often!", "_often!"],
                ["_often!", "_often!", "_often"],
            ),
            (
                "_rarely",
                ["_rarely!", "_rarely"],
                ["_rarely!", "_rarely", "_rarely"],
            ),
            (
                "_almostNever",
                ["_almostNever!", "_almostNever"],
                ["_almostNever!", "_almostNever", "_almostNever"],
            ),
            (
                "_almostAlways",
                ["_almostAlways!", "_almostAlways!"],
                ["_almostAlways!", "_almostAlways!", "_almostAlways"],
            ),
        ] {
            let source = format!(
                r#"(() => {{
                     {SURFACE}
                     globalThis.rawSignalSessionCalls = 0;
                     return pure('{raw_name}').setSteps(7).{raw_name}(
                       function (pat) {{
                         'use strict';
                         rawSignalSessionCalls++;
                         return pat.fmap(value => `${{value}}!`);
                       }}
                     );
                   }})()"#,
            );
            let mut session = Session::new().expect("raw signal query session");
            session
                .evaluate(&source)
                .unwrap_or_else(|error| panic!("evaluate Session {raw_name}: {error}"));
            assert_eq!(session.js.get_number("rawSignalSessionCalls"), Some(0.0));
            assert!(
                session.active_needs_host(),
                "{raw_name}: dynamic scalar join was granted host-free purity"
            );
            assert_eq!(
                session
                    .js
                    .active_pattern()
                    .expect("active raw signal")
                    .steps,
                None,
                "{raw_name}: outer result retained source steps"
            );
            assert_eq!(
                session
                    .query(Fraction::ZERO, Fraction::int(2))
                    .unwrap_or_else(|error| panic!("query Session {raw_name}: {error}"))
                    .into_iter()
                    .map(|hap| hap.value.show())
                    .collect::<Vec<_>>(),
                query_values,
                "{raw_name}: Session scalar RNG values changed"
            );
            assert_eq!(
                session.js.get_number("rawSignalSessionCalls"),
                Some(2.0),
                "{raw_name}: callback was not lazy once per carrier cycle"
            );

            let mut play = Session::new().expect("raw signal play session");
            play.evaluate(&source)
                .unwrap_or_else(|error| panic!("evaluate play {raw_name}: {error}"));
            assert_eq!(
                play.play(4.0)
                    .unwrap_or_else(|error| panic!("play Session {raw_name}: {error}"))
                    .onsets
                    .iter()
                    .map(|onset| (onset.whole_begin.as_str(), onset.value_show.as_str(),))
                    .collect::<Vec<_>>(),
                [
                    ("0/1", play_values[0]),
                    ("1/1", play_values[1]),
                    ("2/1", play_values[2]),
                ],
                "{raw_name}: Session scheduler RNG onsets changed"
            );
        }

        let over = rustel_core::MAX_STEPWISE_ENTRIES + 1;
        for raw_name in ["_often", "_rarely", "_almostNever", "_almostAlways"] {
            // The shorthand adds no raw-specific shared-pool operation.  The
            // Canonical tagged `rev` takes the pure native dispatch, so this
            // is also the host-free proof. Oversized source metadata alone
            // remains successful and pool-neutral.
            let mut neutral = Session::new().expect("pool-neutral raw signal session");
            neutral
                .evaluate(&format!("pure('safe').setSteps({over}).{raw_name}(rev)"))
                .unwrap_or_else(|error| panic!("construct neutral {raw_name}: {error}"));
            assert!(
                !neutral.active_needs_host(),
                "{raw_name}: tagged native route unexpectedly needs QuickJS"
            );
            assert_eq!(
                neutral
                    .query(Fraction::ZERO, Fraction::ONE)
                    .unwrap_or_else(|error| panic!("query neutral {raw_name}: {error}"))
                    .len(),
                1,
                "{raw_name}: neutral shorthand changed density"
            );
            assert!(
                neutral.scheduler.refusal().is_none(),
                "{raw_name}: invented a raw-specific resource refusal"
            );

            // The callback runs for every queried carrier before random
            // selection. Existing nested work therefore keeps its original
            // operation/type/limit attribution for every threshold.
            let mut nested = Session::new().expect("nested raw signal refusal session");
            nested
                .evaluate(&format!("gap({over}).{raw_name}(value => value.shrink(0))"))
                .unwrap_or_else(|error| panic!("construct nested {raw_name}: {error}"));
            let error = nested
                .schedule_at(0.0)
                .expect_err("nested MAX+1 shrink must refuse");
            let RuntimeError::ResourceLimit(message) = error else {
                panic!("{raw_name}: nested shrink lost resource type: {error:?}");
            };
            assert!(
                message.contains("shrink/grow")
                    && message.contains(&over.to_string())
                    && message.contains(&rustel_core::MAX_STEPWISE_ENTRIES.to_string()),
                "{raw_name}: wrong nested refusal: {message}"
            );
            assert!(
                matches!(
                    nested.scheduler.refusal(),
                    Some(rustel_core::QueryLimit::StepwiseExpansion {
                        operation,
                        minimum_entries,
                        limit,
                    }) if *operation == "shrink/grow"
                        && *minimum_entries == over
                        && *limit == rustel_core::MAX_STEPWISE_ENTRIES
                ),
                "{raw_name}: changed nested attribution: {:?}",
                nested.scheduler.refusal()
            );
        }
    }

    #[test]
    fn raw_set_reaches_session_query_play_and_preserves_source_resource_attribution() {
        const SOURCE: &str = r#"
          (() => {
            const order = Object.getOwnPropertyNames(Pattern.prototype)
              .filter(name => name === '_set' || name === 'set');
            const keys = Object.keys(Pattern.prototype)
              .filter(name => name === '_set' || name === 'set');
            const raw = Object.getOwnPropertyDescriptor(Pattern.prototype, '_set');
            const publicSet = Object.getOwnPropertyDescriptor(Pattern.prototype, 'set');
            if (order.join(',') !== '_set,set'
                || keys.join(',') !== '_set'
                || raw.writable !== true
                || raw.enumerable !== true
                || raw.configurable !== true
                || raw.value.name !== ''
                || raw.value.length !== 1
                || typeof publicSet.get !== 'function'
                || publicSet.enumerable !== false
                || publicSet.configurable !== true
                || Object.hasOwn(globalThis, '_set')
                || Object.hasOwn(rustelScope, '_set')
                || typeof globalThis._set !== 'undefined'
                || typeof rustelScope._set !== 'undefined') {
              throw new Error('raw set Session surface changed');
            }
            return pure(1).setSteps(7)._set(9);
          })()
        "#;
        let mut session = Session::new().expect("raw set query session");
        session.evaluate(SOURCE).expect("evaluate Session raw set");
        assert!(
            session.active_needs_host(),
            "raw set's lexical fmap callback was granted host-free purity"
        );
        assert_eq!(
            session.js.active_pattern().expect("active raw set").steps,
            Some(Fraction::int(7)),
            "raw set lost source steps"
        );
        assert_eq!(
            session
                .query(Fraction::ZERO, Fraction::int(2))
                .expect("query Session raw set")
                .into_iter()
                .map(|hap| hap.show())
                .collect::<Vec<_>>(),
            ["[ 0/1 → 1/1 | 9 ]", "[ 1/1 → 2/1 | 9 ]"],
            "Session raw set query changed"
        );

        let mut play = Session::new().expect("raw set play session");
        play.evaluate(SOURCE).expect("evaluate play raw set");
        assert_eq!(
            play.play(4.0)
                .expect("play Session raw set")
                .onsets
                .iter()
                .map(|onset| (onset.whole_begin.as_str(), onset.value_show.as_str()))
                .collect::<Vec<_>>(),
            [("0/1", "9"), ("1/1", "9"), ("2/1", "9")],
            "Session raw set scheduler onsets changed"
        );

        let over = rustel_core::MAX_STEPWISE_ENTRIES + 1;
        // `_set` adds no shared-pool operation or charge. Oversized declared
        // metadata alone stays successful, but the JS mapper remains host
        // backed; this is not an arbitrary-custom-fmap resource claim.
        let mut neutral = Session::new().expect("pool-neutral raw set session");
        neutral
            .evaluate(&format!("pure('safe').setSteps({over})._set('changed')"))
            .expect("construct pool-neutral raw set");
        assert!(neutral.active_needs_host());
        assert_eq!(
            neutral
                .query(Fraction::ZERO, Fraction::ONE)
                .expect("query pool-neutral raw set")
                .len(),
            1,
            "raw set changed canonical fmap density"
        );
        assert!(
            neutral.scheduler.refusal().is_none(),
            "raw set invented a raw-specific resource refusal"
        );

        // Existing work in the source graph is evaluated before the map and
        // retains its original typed operation/limit attribution.
        let mut nested = Session::new().expect("nested raw set refusal session");
        nested
            .evaluate(&format!("gap({over}).shrink(0)._set('changed')"))
            .expect("construct nested raw set refusal");
        let error = nested
            .schedule_at(0.0)
            .expect_err("nested MAX+1 shrink must refuse through raw set");
        let RuntimeError::ResourceLimit(message) = error else {
            panic!("raw set lost nested resource type: {error:?}");
        };
        assert!(
            message.contains("shrink/grow")
                && message.contains(&over.to_string())
                && message.contains(&rustel_core::MAX_STEPWISE_ENTRIES.to_string()),
            "wrong nested raw set refusal: {message}"
        );
        assert!(
            matches!(
                nested.scheduler.refusal(),
                Some(rustel_core::QueryLimit::StepwiseExpansion {
                    operation,
                    minimum_entries,
                    limit,
                }) if *operation == "shrink/grow"
                    && *minimum_entries == over
                    && *limit == rustel_core::MAX_STEPWISE_ENTRIES
            ),
            "raw set changed nested refusal attribution: {:?}",
            nested.scheduler.refusal()
        );
    }

    #[test]
    fn raw_keep_reaches_session_query_play_without_executing_ignored_graphs() {
        const SOURCE: &str = r#"
          (() => {
            const names = ['_set', 'set', '_keep', 'keep'];
            const order = Object.getOwnPropertyNames(Pattern.prototype)
              .filter(name => names.includes(name));
            const keys = Object.keys(Pattern.prototype)
              .filter(name => names.includes(name));
            const raw = Object.getOwnPropertyDescriptor(Pattern.prototype, '_keep');
            const publicKeep = Object.getOwnPropertyDescriptor(Pattern.prototype, 'keep');
            if (order.join(',') !== '_set,set,_keep,keep'
                || keys.join(',') !== '_set,_keep'
                || raw.writable !== true
                || raw.enumerable !== true
                || raw.configurable !== true
                || raw.value.name !== ''
                || raw.value.length !== 1
                || typeof publicKeep.get !== 'function'
                || publicKeep.enumerable !== false
                || publicKeep.configurable !== true
                || Object.hasOwn(globalThis, '_keep')
                || Object.hasOwn(rustelScope, '_keep')
                || typeof globalThis._keep !== 'undefined'
                || typeof rustelScope._keep !== 'undefined') {
              throw new Error('raw keep Session surface changed');
            }
            globalThis.rawKeepIgnoredQueries = 0;
            const ignored = new Pattern(() => {
              rawKeepIgnoredQueries++;
              throw new Error('raw keep queried its ignored graph');
            });
            return pure(1).setSteps(7)._keep(ignored);
          })()
        "#;
        let mut session = Session::new().expect("raw keep query session");
        session.evaluate(SOURCE).expect("evaluate Session raw keep");
        assert!(
            session.active_needs_host(),
            "raw keep's lexical fmap callback was granted host-free purity"
        );
        assert_eq!(
            session.js.active_pattern().expect("active raw keep").steps,
            Some(Fraction::int(7)),
            "raw keep lost source steps"
        );
        assert_eq!(
            session
                .query(Fraction::ZERO, Fraction::int(2))
                .expect("query Session raw keep")
                .into_iter()
                .map(|hap| hap.show())
                .collect::<Vec<_>>(),
            ["[ 0/1 → 1/1 | 1 ]", "[ 1/1 → 2/1 | 1 ]"],
            "Session raw keep query changed"
        );
        assert_eq!(
            session.js.get_number("rawKeepIgnoredQueries"),
            Some(0.0),
            "Session raw keep executed the captured ignored graph"
        );

        let mut play = Session::new().expect("raw keep play session");
        play.evaluate(SOURCE).expect("evaluate play raw keep");
        assert_eq!(
            play.play(4.0)
                .expect("play Session raw keep")
                .onsets
                .iter()
                .map(|onset| (onset.whole_begin.as_str(), onset.value_show.as_str()))
                .collect::<Vec<_>>(),
            [("0/1", "1"), ("1/1", "1"), ("2/1", "1")],
            "Session raw keep scheduler onsets changed"
        );
        assert_eq!(
            play.js.get_number("rawKeepIgnoredQueries"),
            Some(0.0),
            "play raw keep executed the captured ignored graph"
        );

        let over = rustel_core::MAX_STEPWISE_ENTRIES + 1;
        // `_keep` adds no shared-pool operation or charge. The ignored first
        // argument remains captured by the mapper, but its graph is not
        // queried and arbitrary custom-fmap/resource behavior is not claimed.
        let mut neutral = Session::new().expect("pool-neutral raw keep session");
        neutral
                .evaluate(&format!(
                    "(() => {{ globalThis.rawKeepNeutralIgnored = 0; const ignored = new Pattern(() => {{ rawKeepNeutralIgnored++; throw new Error('ignored'); }}); return pure('safe').setSteps({over})._keep(ignored); }})()"
                ))
                .expect("construct pool-neutral raw keep");
        assert!(neutral.active_needs_host());
        assert_eq!(
            neutral
                .query(Fraction::ZERO, Fraction::ONE)
                .expect("query pool-neutral raw keep")
                .into_iter()
                .map(|hap| hap.value.show())
                .collect::<Vec<_>>(),
            ["safe"],
            "raw keep changed canonical identity-map density/value"
        );
        assert_eq!(
            neutral.js.get_number("rawKeepNeutralIgnored"),
            Some(0.0),
            "pool-neutral raw keep queried its ignored graph"
        );
        assert!(
            neutral.scheduler.refusal().is_none(),
            "raw keep invented a raw-specific resource refusal"
        );

        // Existing work in the source graph is evaluated before the identity
        // map and retains its original typed operation/limit attribution.
        let mut nested = Session::new().expect("nested raw keep refusal session");
        nested
            .evaluate(&format!("gap({over}).shrink(0)._keep('ignored')"))
            .expect("construct nested raw keep refusal");
        let error = nested
            .schedule_at(0.0)
            .expect_err("nested MAX+1 shrink must refuse through raw keep");
        let RuntimeError::ResourceLimit(message) = error else {
            panic!("raw keep lost nested resource type: {error:?}");
        };
        assert!(
            message.contains("shrink/grow")
                && message.contains(&over.to_string())
                && message.contains(&rustel_core::MAX_STEPWISE_ENTRIES.to_string()),
            "wrong nested raw keep refusal: {message}"
        );
        assert!(
            matches!(
                nested.scheduler.refusal(),
                Some(rustel_core::QueryLimit::StepwiseExpansion {
                    operation,
                    minimum_entries,
                    limit,
                }) if *operation == "shrink/grow"
                    && *minimum_entries == over
                    && *limit == rustel_core::MAX_STEPWISE_ENTRIES
            ),
            "raw keep changed nested refusal attribution: {:?}",
            nested.scheduler.refusal()
        );
    }

    #[test]
    fn raw_keepif_reaches_session_query_play_and_preserves_source_attribution() {
        const FALSE_SOURCE: &str = r#"
          (() => {
            const names = ['_set', 'set', '_keep', 'keep', '_keepif', 'keepif'];
            const order = Object.getOwnPropertyNames(Pattern.prototype)
              .filter(name => names.includes(name));
            const keys = Object.keys(Pattern.prototype)
              .filter(name => names.includes(name));
            const raw = Object.getOwnPropertyDescriptor(Pattern.prototype, '_keepif');
            const publicKeepif = Object.getOwnPropertyDescriptor(
              Pattern.prototype, 'keepif'
            );
            if (order.join(',') !== '_set,set,_keep,keep,_keepif,keepif'
                || keys.join(',') !== '_set,_keep,_keepif'
                || raw.writable !== true
                || raw.enumerable !== true
                || raw.configurable !== true
                || raw.value.name !== ''
                || raw.value.length !== 1
                || typeof publicKeepif.get !== 'function'
                || publicKeepif.enumerable !== false
                || publicKeepif.configurable !== true
                || Object.hasOwn(globalThis, '_keepif')
                || Object.hasOwn(rustelScope, '_keepif')
                || typeof globalThis._keepif !== 'undefined'
                || typeof rustelScope._keepif !== 'undefined') {
              throw new Error('raw keepif Session surface changed');
            }
            return sequence(1, 2).setSteps(7)._keepif(false);
          })()
        "#;
        let mut session = Session::new().expect("raw keepif query session");
        session
            .evaluate(FALSE_SOURCE)
            .expect("evaluate Session raw keepif");
        assert!(
            session.active_needs_host(),
            "raw keepif's lexical fmap callback was granted host-free purity"
        );
        assert_eq!(
            session
                .js
                .active_pattern()
                .expect("active raw keepif")
                .steps,
            Some(Fraction::int(7)),
            "raw keepif lost source steps"
        );
        assert_eq!(
            session
                .query(Fraction::ZERO, Fraction::int(2))
                .expect("query Session raw keepif")
                .into_iter()
                .map(|hap| hap.show())
                .collect::<Vec<_>>(),
            [
                "[ 0/1 → 1/2 | undefined ]",
                "[ 1/2 → 1/1 | undefined ]",
                "[ 1/1 → 3/2 | undefined ]",
                "[ 3/2 → 2/1 | undefined ]",
            ],
            "Session raw keepif false route removed or retimed haps"
        );

        let mut play = Session::new().expect("raw keepif play session");
        play.evaluate(FALSE_SOURCE)
            .expect("evaluate play raw keepif");
        assert_eq!(
            play.play(4.0)
                .expect("play Session raw keepif")
                .onsets
                .iter()
                .map(|onset| (onset.whole_begin.as_str(), onset.value_show.as_str()))
                .collect::<Vec<_>>(),
            [
                ("0/1", "undefined"),
                ("1/2", "undefined"),
                ("1/1", "undefined"),
                ("3/2", "undefined"),
                ("2/1", "undefined"),
            ],
            "Session raw keepif scheduler false onsets changed"
        );

        // A Pattern-valued condition is truthy but never queried. This is
        // deliberately distinct from the open Pattern-valued source route.
        let mut control = Session::new().expect("raw keepif Pattern-control session");
        control
            .evaluate(
                r#"(() => {
                     globalThis.rawKeepifControlQueries = 0;
                     const condition = new Pattern(() => {
                       rawKeepifControlQueries++;
                       throw new Error('raw keepif queried its condition');
                     });
                     return sequence(1, 2)._keepif(condition);
                   })()"#,
            )
            .expect("construct Pattern-control raw keepif");
        assert_eq!(
            control
                .query(Fraction::ZERO, Fraction::ONE)
                .expect("query Pattern-control raw keepif")
                .into_iter()
                .map(|hap| hap.value.show())
                .collect::<Vec<_>>(),
            ["1", "2"],
            "truthy Pattern control changed raw keepif source values"
        );
        assert_eq!(
            control.js.get_number("rawKeepifControlQueries"),
            Some(0.0),
            "Session raw keepif queried its Pattern control"
        );

        let over = rustel_core::MAX_STEPWISE_ENTRIES + 1;
        // `_keepif` adds no shared-pool operation or charge. Oversized source
        // metadata alone stays successful, while the generated mapper remains
        // host-backed; this is not an arbitrary custom-fmap resource claim.
        let mut neutral = Session::new().expect("pool-neutral raw keepif session");
        neutral
            .evaluate(&format!("pure('safe').setSteps({over})._keepif(false)"))
            .expect("construct pool-neutral raw keepif");
        assert!(neutral.active_needs_host());
        assert_eq!(
            neutral
                .query(Fraction::ZERO, Fraction::ONE)
                .expect("query pool-neutral raw keepif")
                .into_iter()
                .map(|hap| hap.value.show())
                .collect::<Vec<_>>(),
            ["undefined"],
            "raw keepif false route changed canonical density"
        );
        assert!(
            neutral.scheduler.refusal().is_none(),
            "raw keepif invented a raw-specific resource refusal"
        );

        // Existing source work is evaluated before the mapper and retains its
        // typed operation/limit attribution even on the truthy route.
        let mut nested = Session::new().expect("nested raw keepif refusal session");
        nested
            .evaluate(&format!("gap({over}).shrink(0)._keepif(true)"))
            .expect("construct nested raw keepif refusal");
        let error = nested
            .schedule_at(0.0)
            .expect_err("nested MAX+1 shrink must refuse through raw keepif");
        let RuntimeError::ResourceLimit(message) = error else {
            panic!("raw keepif lost nested resource type: {error:?}");
        };
        assert!(
            message.contains("shrink/grow")
                && message.contains(&over.to_string())
                && message.contains(&rustel_core::MAX_STEPWISE_ENTRIES.to_string()),
            "wrong nested raw keepif refusal: {message}"
        );
        assert!(
            matches!(
                nested.scheduler.refusal(),
                Some(rustel_core::QueryLimit::StepwiseExpansion {
                    operation,
                    minimum_entries,
                    limit,
                }) if *operation == "shrink/grow"
                    && *minimum_entries == over
                    && *limit == rustel_core::MAX_STEPWISE_ENTRIES
            ),
            "raw keepif changed nested refusal attribution: {:?}",
            nested.scheduler.refusal()
        );
    }

    #[test]
    fn raw_eqt_reaches_session_query_play_and_preserves_source_attribution() {
        const SOURCE: &str = r#"
          (() => {
            const names = ['eq', '_eqt', 'eqt', 'ne', 'net'];
            const order = Object.getOwnPropertyNames(Pattern.prototype)
              .filter(name => names.includes(name));
            const keys = Object.keys(Pattern.prototype)
              .filter(name => names.includes(name));
            const raw = Object.getOwnPropertyDescriptor(Pattern.prototype, '_eqt');
            const publicEqt = Object.getOwnPropertyDescriptor(Pattern.prototype, 'eqt');
            if (order.join(',') !== 'eq,_eqt,eqt,ne,net'
                || keys.join(',') !== '_eqt'
                || raw.writable !== true
                || raw.enumerable !== true
                || raw.configurable !== true
                || raw.value.name !== ''
                || raw.value.length !== 1
                || typeof publicEqt.get !== 'function'
                || publicEqt.enumerable !== false
                || publicEqt.configurable !== true
                || Object.hasOwn(globalThis, '_eqt')
                || Object.hasOwn(rustelScope, '_eqt')
                || typeof globalThis._eqt !== 'undefined'
                || typeof rustelScope._eqt !== 'undefined') {
              throw new Error('raw eqt Session surface changed');
            }
            return sequence(1, 2).setSteps(7)._eqt(1);
          })()
        "#;
        let mut session = Session::new().expect("raw eqt query session");
        session.evaluate(SOURCE).expect("evaluate Session raw eqt");
        assert!(
            session.active_needs_host(),
            "raw eqt's lexical fmap callback was granted host-free purity"
        );
        assert_eq!(
            session.js.active_pattern().expect("active raw eqt").steps,
            Some(Fraction::int(7)),
            "raw eqt lost source steps"
        );
        assert_eq!(
            session
                .query(Fraction::ZERO, Fraction::int(2))
                .expect("query Session raw eqt")
                .into_iter()
                .map(|hap| hap.show())
                .collect::<Vec<_>>(),
            [
                "[ 0/1 → 1/2 | true ]",
                "[ 1/2 → 1/1 | false ]",
                "[ 1/1 → 3/2 | true ]",
                "[ 3/2 → 2/1 | false ]",
            ],
            "Session raw eqt boolean rhythm changed"
        );

        let mut play = Session::new().expect("raw eqt play session");
        play.evaluate(SOURCE).expect("evaluate play raw eqt");
        assert_eq!(
            play.play(4.0)
                .expect("play Session raw eqt")
                .onsets
                .iter()
                .map(|onset| (onset.whole_begin.as_str(), onset.value_show.as_str()))
                .collect::<Vec<_>>(),
            [
                ("0/1", "true"),
                ("1/2", "false"),
                ("1/1", "true"),
                ("3/2", "false"),
                ("2/1", "true"),
            ],
            "Session raw eqt scheduler onsets changed"
        );

        let over = rustel_core::MAX_STEPWISE_ENTRIES + 1;
        // `_eqt` adds no shared-pool operation or charge. Oversized declared
        // metadata alone remains successful, while the generated mapper is
        // still host-backed.
        let mut neutral = Session::new().expect("pool-neutral raw eqt session");
        neutral
            .evaluate(&format!("sequence(1, 2).setSteps({over})._eqt(1)"))
            .expect("construct pool-neutral raw eqt");
        assert!(neutral.active_needs_host());
        assert_eq!(
            neutral
                .query(Fraction::ZERO, Fraction::ONE)
                .expect("query pool-neutral raw eqt")
                .into_iter()
                .map(|hap| hap.value.show())
                .collect::<Vec<_>>(),
            ["true", "false"],
            "raw eqt changed canonical fmap density/value"
        );
        assert!(
            neutral.scheduler.refusal().is_none(),
            "raw eqt invented a raw-specific resource refusal"
        );

        // Existing bounded work in the source graph is evaluated before the
        // equality mapper and retains its original typed operation and limit.
        let mut nested = Session::new().expect("nested raw eqt refusal session");
        nested
            .evaluate(&format!("gap({over}).shrink(0)._eqt(0)"))
            .expect("construct nested raw eqt refusal");
        let error = nested
            .schedule_at(0.0)
            .expect_err("nested MAX+1 shrink must refuse through raw eqt");
        let RuntimeError::ResourceLimit(message) = error else {
            panic!("raw eqt lost nested resource type: {error:?}");
        };
        assert!(
            message.contains("shrink/grow")
                && message.contains(&over.to_string())
                && message.contains(&rustel_core::MAX_STEPWISE_ENTRIES.to_string()),
            "wrong nested raw eqt refusal: {message}"
        );
        assert!(
            matches!(
                nested.scheduler.refusal(),
                Some(rustel_core::QueryLimit::StepwiseExpansion {
                    operation,
                    minimum_entries,
                    limit,
                }) if *operation == "shrink/grow"
                    && *minimum_entries == over
                    && *limit == rustel_core::MAX_STEPWISE_ENTRIES
            ),
            "raw eqt changed nested refusal attribution: {:?}",
            nested.scheduler.refusal()
        );
    }

    #[test]
    fn raw_net_reaches_session_query_play_and_preserves_source_attribution() {
        const SOURCE: &str = r#"
          (() => {
            const names = ['ne', '_net', 'net', 'and'];
            const order = Object.getOwnPropertyNames(Pattern.prototype)
              .filter(name => names.includes(name));
            const keys = Object.keys(Pattern.prototype)
              .filter(name => names.includes(name));
            const raw = Object.getOwnPropertyDescriptor(Pattern.prototype, '_net');
            const publicNet = Object.getOwnPropertyDescriptor(Pattern.prototype, 'net');
            if (order.join(',') !== 'ne,_net,net,and'
                || keys.join(',') !== '_net'
                || raw.writable !== true
                || raw.enumerable !== true
                || raw.configurable !== true
                || raw.value.name !== ''
                || raw.value.length !== 1
                || typeof publicNet.get !== 'function'
                || publicNet.enumerable !== false
                || publicNet.configurable !== true
                || Object.hasOwn(globalThis, '_net')
                || Object.hasOwn(rustelScope, '_net')
                || Object.hasOwn(globalThis, 'net')
                || Object.hasOwn(rustelScope, 'net')
                || Object.hasOwn(Pattern.prototype, '_eq')
                || Object.hasOwn(Pattern.prototype, '_ne')) {
              throw new Error('raw net Session surface changed');
            }
            return sequence(1, 2).setSteps(7)._net(1);
          })()
        "#;
        let mut session = Session::new().expect("raw net query session");
        session.evaluate(SOURCE).expect("evaluate Session raw net");
        assert!(
            session.active_needs_host(),
            "raw net's lexical fmap callback was granted host-free purity"
        );
        assert_eq!(
            session.js.active_pattern().expect("active raw net").steps,
            Some(Fraction::int(7)),
            "raw net lost source steps"
        );
        assert_eq!(
            session
                .query(Fraction::ZERO, Fraction::int(2))
                .expect("query Session raw net")
                .into_iter()
                .map(|hap| hap.show())
                .collect::<Vec<_>>(),
            [
                "[ 0/1 → 1/2 | false ]",
                "[ 1/2 → 1/1 | true ]",
                "[ 1/1 → 3/2 | false ]",
                "[ 3/2 → 2/1 | true ]",
            ],
            "Session raw net boolean rhythm changed"
        );

        let mut play = Session::new().expect("raw net play session");
        play.evaluate(SOURCE).expect("evaluate play raw net");
        assert_eq!(
            play.play(4.0)
                .expect("play Session raw net")
                .onsets
                .iter()
                .map(|onset| (onset.whole_begin.as_str(), onset.value_show.as_str()))
                .collect::<Vec<_>>(),
            [
                ("0/1", "false"),
                ("1/2", "true"),
                ("1/1", "false"),
                ("3/2", "true"),
                ("2/1", "false"),
            ],
            "Session raw net scheduler onsets changed"
        );

        let over = rustel_core::MAX_STEPWISE_ENTRIES + 1;
        let mut neutral = Session::new().expect("pool-neutral raw net session");
        neutral
            .evaluate(&format!("sequence(1, 2).setSteps({over})._net(1)"))
            .expect("construct pool-neutral raw net");
        assert!(neutral.active_needs_host());
        assert_eq!(
            neutral
                .query(Fraction::ZERO, Fraction::ONE)
                .expect("query pool-neutral raw net")
                .into_iter()
                .map(|hap| hap.value.show())
                .collect::<Vec<_>>(),
            ["false", "true"],
            "raw net changed canonical fmap density/value"
        );
        assert!(
            neutral.scheduler.refusal().is_none(),
            "raw net invented a raw-specific resource refusal"
        );

        let mut nested = Session::new().expect("nested raw net refusal session");
        nested
            .evaluate(&format!("gap({over}).shrink(0)._net(0)"))
            .expect("construct nested raw net refusal");
        let error = nested
            .schedule_at(0.0)
            .expect_err("nested MAX+1 shrink must refuse through raw net");
        let RuntimeError::ResourceLimit(message) = error else {
            panic!("raw net lost nested resource type: {error:?}");
        };
        assert!(
            message.contains("shrink/grow")
                && message.contains(&over.to_string())
                && message.contains(&rustel_core::MAX_STEPWISE_ENTRIES.to_string()),
            "wrong nested raw net refusal: {message}"
        );
        assert!(
            matches!(
                nested.scheduler.refusal(),
                Some(rustel_core::QueryLimit::StepwiseExpansion {
                    operation,
                    minimum_entries,
                    limit,
                }) if *operation == "shrink/grow"
                    && *minimum_entries == over
                    && *limit == rustel_core::MAX_STEPWISE_ENTRIES
            ),
            "raw net changed nested refusal attribution: {:?}",
            nested.scheduler.refusal()
        );

        let mut mapper_throw = Session::new().expect("raw net mapper-throw session");
        mapper_throw
            .evaluate(
                r#"(() => {
                     globalThis.rawNetSessionMapperCalls = 0;
                     const originalFmap = Pattern.prototype.fmap;
                     Pattern.prototype.fmap = function (mapper) {
                       return Reflect.apply(originalFmap, this, [new Proxy(mapper, {
                         apply() {
                           rawNetSessionMapperCalls++;
                           throw new Error('raw net Session mapper throw');
                         },
                       })]);
                     };
                     const result = fastcat(pure(1), pure(1))._net(1);
                     Pattern.prototype.fmap = originalFmap;
                     return result;
                   })()"#,
            )
            .expect("construct Session mapper-throw residual");
        assert!(
            mapper_throw
                .query(Fraction::ZERO, Fraction::ONE)
                .expect("query Session mapper-throw residual")
                .is_empty(),
            "Session outer query retained undefined mapper-throw haps"
        );
        assert_eq!(
            mapper_throw.js.get_number("rawNetSessionMapperCalls"),
            Some(2.0),
            "Session outer query did not complete both native mapper calls"
        );
    }

    #[test]
    fn raw_and_reaches_session_query_play_and_preserves_source_attribution() {
        const SOURCE: &str = r#"
          (() => {
            const names = ['net', '_and', 'and', '_or', 'or'];
            const order = Object.getOwnPropertyNames(Pattern.prototype)
              .filter(name => names.includes(name));
            const keys = Object.keys(Pattern.prototype)
              .filter(name => names.includes(name));
            const raw = Object.getOwnPropertyDescriptor(Pattern.prototype, '_and');
            const publicAnd = Object.getOwnPropertyDescriptor(Pattern.prototype, 'and');
            if (order.join(',') !== 'net,_and,and,_or,or'
                || keys.join(',') !== '_and,_or'
                || raw.writable !== true
                || raw.enumerable !== true
                || raw.configurable !== true
                || raw.value.name !== ''
                || raw.value.length !== 1
                || typeof publicAnd.get !== 'function'
                || publicAnd.enumerable !== false
                || publicAnd.configurable !== true
                || Object.hasOwn(globalThis, '_and')
                || Object.hasOwn(rustelScope, '_and')
                || Object.hasOwn(globalThis, 'and')
                || Object.hasOwn(rustelScope, 'and')
                || !Object.hasOwn(Pattern.prototype, '_or')) {
              throw new Error('raw and Session surface changed');
            }
            return sequence(0, 1).setSteps(7)._and('right');
          })()
        "#;
        let mut session = Session::new().expect("raw and query session");
        session.evaluate(SOURCE).expect("evaluate Session raw and");
        assert!(
            session.active_needs_host(),
            "raw and's lexical fmap callback was granted host-free purity"
        );
        assert_eq!(
            session.js.active_pattern().expect("active raw and").steps,
            Some(Fraction::int(7)),
            "raw and lost source steps"
        );
        assert_eq!(
            session
                .query(Fraction::ZERO, Fraction::int(2))
                .expect("query Session raw and")
                .into_iter()
                .map(|hap| hap.show())
                .collect::<Vec<_>>(),
            [
                "[ 0/1 → 1/2 | 0 ]",
                "[ 1/2 → 1/1 | right ]",
                "[ 1/1 → 3/2 | 0 ]",
                "[ 3/2 → 2/1 | right ]",
            ],
            "Session raw and operand-selection rhythm changed"
        );

        let mut play = Session::new().expect("raw and play session");
        play.evaluate(SOURCE).expect("evaluate play raw and");
        assert_eq!(
            play.play(4.0)
                .expect("play Session raw and")
                .onsets
                .iter()
                .map(|onset| (onset.whole_begin.as_str(), onset.value_show.as_str()))
                .collect::<Vec<_>>(),
            [
                ("0/1", "0"),
                ("1/2", "right"),
                ("1/1", "0"),
                ("3/2", "right"),
                ("2/1", "0"),
            ],
            "Session raw and scheduler onsets changed"
        );

        let over = rustel_core::MAX_STEPWISE_ENTRIES + 1;
        let mut neutral = Session::new().expect("pool-neutral raw and session");
        neutral
            .evaluate(&format!("sequence(0, 1).setSteps({over})._and('right')"))
            .expect("construct pool-neutral raw and");
        assert!(neutral.active_needs_host());
        assert_eq!(
            neutral
                .query(Fraction::ZERO, Fraction::ONE)
                .expect("query pool-neutral raw and")
                .into_iter()
                .map(|hap| hap.value.show())
                .collect::<Vec<_>>(),
            ["0", "right"],
            "raw and changed canonical fmap density/operand value"
        );
        assert!(
            neutral.scheduler.refusal().is_none(),
            "raw and invented a raw-specific resource refusal"
        );

        let mut nested = Session::new().expect("nested raw and refusal session");
        nested
            .evaluate(&format!("gap({over}).shrink(0)._and('right')"))
            .expect("construct nested raw and refusal");
        let error = nested
            .schedule_at(0.0)
            .expect_err("nested MAX+1 shrink must refuse through raw and");
        let RuntimeError::ResourceLimit(message) = error else {
            panic!("raw and lost nested resource type: {error:?}");
        };
        assert!(
            message.contains("shrink/grow")
                && message.contains(&over.to_string())
                && message.contains(&rustel_core::MAX_STEPWISE_ENTRIES.to_string()),
            "wrong nested raw and refusal: {message}"
        );
        assert!(
            matches!(
                nested.scheduler.refusal(),
                Some(rustel_core::QueryLimit::StepwiseExpansion {
                    operation,
                    minimum_entries,
                    limit,
                }) if *operation == "shrink/grow"
                    && *minimum_entries == over
                    && *limit == rustel_core::MAX_STEPWISE_ENTRIES
            ),
            "raw and changed nested refusal attribution: {:?}",
            nested.scheduler.refusal()
        );

        let mut mapper_throw = Session::new().expect("raw and mapper-throw session");
        mapper_throw
            .evaluate(
                r#"(() => {
                     globalThis.rawAndSessionMapperCalls = 0;
                     const originalFmap = Pattern.prototype.fmap;
                     Pattern.prototype.fmap = function (mapper) {
                       return Reflect.apply(originalFmap, this, [new Proxy(mapper, {
                         apply() {
                           rawAndSessionMapperCalls++;
                           throw new Error('raw and Session mapper throw');
                         },
                       })]);
                     };
                     const result = fastcat(pure(1), pure(1))._and('right');
                     Pattern.prototype.fmap = originalFmap;
                     return result;
                   })()"#,
            )
            .expect("construct Session mapper-throw residual");
        assert!(
            mapper_throw
                .query(Fraction::ZERO, Fraction::ONE)
                .expect("query Session mapper-throw residual")
                .is_empty(),
            "Session outer query retained undefined mapper-throw haps"
        );
        assert_eq!(
            mapper_throw.js.get_number("rawAndSessionMapperCalls"),
            Some(2.0),
            "Session outer query did not complete both native mapper calls"
        );
    }

    #[test]
    fn raw_or_reaches_session_query_play_and_preserves_source_attribution() {
        const SOURCE: &str = r#"
          (() => {
            const names = ['and', '_or', 'or', '_func', 'func'];
            const order = Object.getOwnPropertyNames(Pattern.prototype)
              .filter(name => names.includes(name));
            const keys = Object.keys(Pattern.prototype)
              .filter(name => names.includes(name));
            const raw = Object.getOwnPropertyDescriptor(Pattern.prototype, '_or');
            const publicOr = Object.getOwnPropertyDescriptor(Pattern.prototype, 'or');
            if (order.join(',') !== 'and,_or,or'
                || keys.join(',') !== '_or'
                || raw.writable !== true
                || raw.enumerable !== true
                || raw.configurable !== true
                || raw.value.name !== ''
                || raw.value.length !== 1
                || typeof publicOr.get !== 'function'
                || publicOr.enumerable !== false
                || publicOr.configurable !== true
                || Object.hasOwn(globalThis, '_or')
                || Object.hasOwn(rustelScope, '_or')
                || Object.hasOwn(globalThis, 'or')
                || Object.hasOwn(rustelScope, 'or')
                || Object.hasOwn(Pattern.prototype, '_func')
                || Object.hasOwn(Pattern.prototype, 'func')) {
              throw new Error('raw or Session surface changed');
            }
            return sequence(0, 1).setSteps(7)._or('right');
          })()
        "#;
        let mut session = Session::new().expect("raw or query session");
        session.evaluate(SOURCE).expect("evaluate Session raw or");
        assert!(
            session.active_needs_host(),
            "raw or's lexical fmap callback was granted host-free purity"
        );
        assert_eq!(
            session.js.active_pattern().expect("active raw or").steps,
            Some(Fraction::int(7)),
            "raw or lost source steps"
        );
        assert_eq!(
            session
                .query(Fraction::ZERO, Fraction::int(2))
                .expect("query Session raw or")
                .into_iter()
                .map(|hap| hap.show())
                .collect::<Vec<_>>(),
            [
                "[ 0/1 → 1/2 | right ]",
                "[ 1/2 → 1/1 | 1 ]",
                "[ 1/1 → 3/2 | right ]",
                "[ 3/2 → 2/1 | 1 ]",
            ],
            "Session raw or operand-selection rhythm changed"
        );

        let mut play = Session::new().expect("raw or play session");
        play.evaluate(SOURCE).expect("evaluate play raw or");
        assert_eq!(
            play.play(4.0)
                .expect("play Session raw or")
                .onsets
                .iter()
                .map(|onset| (onset.whole_begin.as_str(), onset.value_show.as_str()))
                .collect::<Vec<_>>(),
            [
                ("0/1", "right"),
                ("1/2", "1"),
                ("1/1", "right"),
                ("3/2", "1"),
                ("2/1", "right"),
            ],
            "Session raw or scheduler onsets changed"
        );

        let over = rustel_core::MAX_STEPWISE_ENTRIES + 1;
        let mut neutral = Session::new().expect("pool-neutral raw or session");
        neutral
            .evaluate(&format!("sequence(0, 1).setSteps({over})._or('right')"))
            .expect("construct pool-neutral raw or");
        assert!(neutral.active_needs_host());
        assert_eq!(
            neutral
                .query(Fraction::ZERO, Fraction::ONE)
                .expect("query pool-neutral raw or")
                .into_iter()
                .map(|hap| hap.value.show())
                .collect::<Vec<_>>(),
            ["right", "1"],
            "raw or changed canonical fmap density/operand value"
        );
        assert!(
            neutral.scheduler.refusal().is_none(),
            "raw or invented a raw-specific resource refusal"
        );

        let mut nested = Session::new().expect("nested raw or refusal session");
        nested
            .evaluate(&format!("gap({over}).shrink(0)._or('right')"))
            .expect("construct nested raw or refusal");
        let error = nested
            .schedule_at(0.0)
            .expect_err("nested MAX+1 shrink must refuse through raw or");
        let RuntimeError::ResourceLimit(message) = error else {
            panic!("raw or lost nested resource type: {error:?}");
        };
        assert!(
            message.contains("shrink/grow")
                && message.contains(&over.to_string())
                && message.contains(&rustel_core::MAX_STEPWISE_ENTRIES.to_string()),
            "wrong nested raw or refusal: {message}"
        );
        assert!(
            matches!(
                nested.scheduler.refusal(),
                Some(rustel_core::QueryLimit::StepwiseExpansion {
                    operation,
                    minimum_entries,
                    limit,
                }) if *operation == "shrink/grow"
                    && *minimum_entries == over
                    && *limit == rustel_core::MAX_STEPWISE_ENTRIES
            ),
            "raw or changed nested refusal attribution: {:?}",
            nested.scheduler.refusal()
        );

        let mut mapper_throw = Session::new().expect("raw or mapper-throw session");
        mapper_throw
            .evaluate(
                r#"(() => {
                     globalThis.rawOrSessionMapperCalls = 0;
                     const originalFmap = Pattern.prototype.fmap;
                     Pattern.prototype.fmap = function (mapper) {
                       return Reflect.apply(originalFmap, this, [new Proxy(mapper, {
                         apply() {
                           rawOrSessionMapperCalls++;
                           throw new Error('raw or Session mapper throw');
                         },
                       })]);
                     };
                     const result = fastcat(pure(0), pure(0))._or('right');
                     Pattern.prototype.fmap = originalFmap;
                     return result;
                   })()"#,
            )
            .expect("construct Session mapper-throw residual");
        assert!(
            mapper_throw
                .query(Fraction::ZERO, Fraction::ONE)
                .expect("query Session mapper-throw residual")
                .is_empty(),
            "Session outer query retained undefined mapper-throw haps"
        );
        assert_eq!(
            mapper_throw.js.get_number("rawOrSessionMapperCalls"),
            Some(2.0),
            "Session outer query did not complete both native mapper calls"
        );
    }

    #[test]
    fn shrink_grow_keep_exact_step_join_windows_and_scheduler_onsets() {
        const SHRINK_JOINED: &[&str] = &[
            "[ 0/1 → 1/10 | 0 ]",
            "[ 1/10 → 1/5 | 1 ]",
            "[ 1/5 → 3/10 | 2 ]",
            "[ 3/10 → 2/5 | 3 ]",
            "[ 2/5 → 1/2 | 1 ]",
            "[ 1/2 → 3/5 | 2 ]",
            "[ 3/5 → 7/10 | 3 ]",
            "[ 7/10 → 4/5 | 2 ]",
            "[ 4/5 → 9/10 | 3 ]",
            "[ 9/10 → 1/1 | 3 ]",
            "[ 1/1 → 11/10 | 0 ]",
            "[ 11/10 → 6/5 | 1 ]",
            "[ 6/5 → 13/10 | 2 ]",
            "[ 13/10 → 7/5 | 3 ]",
            "[ 7/5 → 3/2 | 1 ]",
            "[ 3/2 → 8/5 | 2 ]",
            "[ 8/5 → 17/10 | 3 ]",
            "[ 17/10 → 9/5 | 2 ]",
            "[ 9/5 → 19/10 | 3 ]",
            "[ 19/10 → 2/1 | 3 ]",
        ];
        const SHRINK_SECOND: &[&str] = &[
            "[ 1/1 → 7/6 | 0 ]",
            "[ 7/6 → 4/3 | 1 ]",
            "[ 4/3 → 3/2 | 2 ]",
            "[ 3/2 → 5/3 | 3 ]",
            "[ 5/3 → 11/6 | 2 ]",
            "[ 11/6 → 2/1 | 3 ]",
        ];
        const GROW_JOINED: &[&str] = &[
            "[ 0/1 → 1/10 | 0 ]",
            "[ 1/10 → 1/5 | 0 ]",
            "[ 1/5 → 3/10 | 1 ]",
            "[ 3/10 → 2/5 | 0 ]",
            "[ 2/5 → 1/2 | 1 ]",
            "[ 1/2 → 3/5 | 2 ]",
            "[ 3/5 → 7/10 | 0 ]",
            "[ 7/10 → 4/5 | 1 ]",
            "[ 4/5 → 9/10 | 2 ]",
            "[ 9/10 → 1/1 | 3 ]",
            "[ 1/1 → 11/10 | 0 ]",
            "[ 11/10 → 6/5 | 0 ]",
            "[ 6/5 → 13/10 | 1 ]",
            "[ 13/10 → 7/5 | 0 ]",
            "[ 7/5 → 3/2 | 1 ]",
            "[ 3/2 → 8/5 | 2 ]",
            "[ 8/5 → 17/10 | 0 ]",
            "[ 17/10 → 9/5 | 1 ]",
            "[ 9/5 → 19/10 | 2 ]",
            "[ 19/10 → 2/1 | 3 ]",
        ];
        const GROW_SECOND: &[&str] = &[
            "[ 1/1 → 7/6 | 0 ]",
            "[ 7/6 → 4/3 | 1 ]",
            "[ 4/3 → 3/2 | 0 ]",
            "[ 3/2 → 5/3 | 1 ]",
            "[ 5/3 → 11/6 | 2 ]",
            "[ 11/6 → 2/1 | 3 ]",
        ];
        const SHRINK_ONSETS: &[(&str, &str)] = &[
            ("0/1", "0"),
            ("1/10", "1"),
            ("1/5", "2"),
            ("3/10", "3"),
            ("2/5", "1"),
            ("1/2", "2"),
            ("3/5", "3"),
            ("7/10", "2"),
            ("4/5", "3"),
            ("9/10", "3"),
            ("1/1", "0"),
            ("7/6", "1"),
            ("4/3", "2"),
            ("3/2", "3"),
            ("5/3", "2"),
            ("11/6", "3"),
            ("2/1", "0"),
        ];
        const GROW_ONSETS: &[(&str, &str)] = &[
            ("0/1", "0"),
            ("1/10", "0"),
            ("1/5", "1"),
            ("3/10", "0"),
            ("2/5", "1"),
            ("1/2", "2"),
            ("3/5", "0"),
            ("7/10", "1"),
            ("4/5", "2"),
            ("9/10", "3"),
            ("1/1", "0"),
            ("7/6", "1"),
            ("4/3", "0"),
            ("3/2", "1"),
            ("5/3", "2"),
            ("11/6", "3"),
            ("2/1", "0"),
        ];

        fn exercise(
            source: &str,
            joined: &[&str],
            second: &[&str],
            expected_onsets: &[(&str, &str)],
        ) {
            let mut session = Session::new().expect("shrink/grow session");
            session.evaluate(source).expect("evaluate shrink/grow");
            assert!(
                session.active_needs_host(),
                "patterned amount lost mutable query-time shrinklist dispatch"
            );
            assert_eq!(
                session
                    .js
                    .active_pattern()
                    .expect("active shrink/grow")
                    .steps,
                Some(Fraction::int(10)),
                "StepJoin metadata must come from cycle zero"
            );
            let shows = |begin, end| {
                session
                    .query(begin, end)
                    .expect("query shrink/grow")
                    .into_iter()
                    .map(|hap| hap.show())
                    .collect::<Vec<_>>()
            };
            assert_eq!(shows(Fraction::ZERO, Fraction::int(2)), joined);
            assert_eq!(shows(Fraction::ONE, Fraction::int(2)), second);
            assert_eq!(
                shows(Fraction::ONE, Fraction::int(2)),
                second,
                "repeated separate-window query changed"
            );

            let play = session.play(4.0).expect("schedule shrink/grow");
            assert_eq!(play.onsets.len(), expected_onsets.len());
            for (index, onset) in play.onsets.iter().enumerate() {
                let expected_target = if index < 10 {
                    index as f64 / 5.0
                } else {
                    (index - 4) as f64 / 3.0
                };
                let expected_duration = if index < 10 { 1.0 / 5.0 } else { 1.0 / 3.0 };
                assert_eq!(
                    onset.target_time, expected_target,
                    "scheduler changed shrink/grow onset {index}'s target time"
                );
                assert_eq!(
                    onset.duration_secs, expected_duration,
                    "scheduler changed shrink/grow onset {index}'s duration"
                );
            }
            let onsets = play
                .onsets
                .iter()
                .map(|onset| (onset.whole_begin.as_str(), onset.value_show.as_str()))
                .collect::<Vec<_>>();
            assert_eq!(
                onsets, expected_onsets,
                "scheduler changed per-window shrink/grow factors"
            );
            assert_eq!(session.js.query_depth(), 0, "query roots leaked");
        }

        let mut no_steps = Session::new().expect("no-step shrink session");
        no_steps
            .evaluate("new Pattern(state => pure('x').query(state)).shrink(1)")
            .expect("evaluate no-step shrink");
        assert!(!no_steps.active_needs_host());
        assert!(
            no_steps
                .query(Fraction::ZERO, Fraction::ONE)
                .expect("query no-step shrink")
                .is_empty()
        );

        exercise(
            "sequence(0, 1, 2, 3).shrink(slowcat(1, 2))",
            SHRINK_JOINED,
            SHRINK_SECOND,
            SHRINK_ONSETS,
        );
        exercise(
            "sequence(0, 1, 2, 3).grow(slowcat(1, 2))",
            GROW_JOINED,
            GROW_SECOND,
            GROW_ONSETS,
        );
    }

    #[test]
    fn canonical_pair_forms_reach_session_query_and_play_as_host_free_graphs() {
        const SHRINK_HAPS: &[&str] = &[
            "[ 0/1 → 1/7 | a ]",
            "[ 1/7 → 2/7 | b ]",
            "[ 2/7 → 3/7 | c ]",
            "[ 3/7 → 4/7 | d ]",
            "[ 4/7 → 5/7 | b ]",
            "[ 5/7 → 6/7 | c ]",
            "[ 6/7 → 1/1 | d ]",
        ];
        const GROW_HAPS: &[&str] = &[
            "[ 0/1 → 1/13 | a ]",
            "[ 1/13 → 2/13 | b ]",
            "[ (2/13 → 5/26) ⇝ 3/13 | c ]",
            "[ 5/26 → 7/26 | a ]",
            "[ 7/26 → 9/26 | b ]",
            "[ 9/26 → 11/26 | c ]",
            "[ 11/26 → 1/2 | a ]",
            "[ 1/2 → 15/26 | b ]",
            "[ 15/26 → 17/26 | c ]",
            "[ (17/26 → 9/13) ⇝ 19/26 | d ]",
            "[ 9/13 → 10/13 | a ]",
            "[ 10/13 → 11/13 | b ]",
            "[ 11/13 → 12/13 | c ]",
            "[ 12/13 → 1/1 | d ]",
        ];
        const SHRINK_ONSETS: &[(&str, &str)] = &[
            ("0/1", "a"),
            ("1/7", "b"),
            ("2/7", "c"),
            ("3/7", "d"),
            ("4/7", "b"),
            ("5/7", "c"),
            ("6/7", "d"),
            ("1/1", "a"),
        ];
        const GROW_ONSETS: &[(&str, &str)] = &[
            ("0/1", "a"),
            ("1/13", "b"),
            ("2/13", "c"),
            ("5/26", "a"),
            ("7/26", "b"),
            ("9/26", "c"),
            ("11/26", "a"),
            ("1/2", "b"),
            ("15/26", "c"),
            ("17/26", "d"),
            ("9/13", "a"),
            ("10/13", "b"),
            ("11/13", "c"),
            ("12/13", "d"),
            ("1/1", "a"),
        ];

        fn exercise(
            source: &str,
            expected: &[&str],
            steps: i128,
            expected_onsets: &[(&str, &str)],
        ) {
            let mut session = Session::new().expect("canonical pair session");
            session
                .evaluate(source)
                .unwrap_or_else(|error| panic!("evaluate {source}: {error}"));
            assert!(
                !session.active_needs_host(),
                "{source}: eager pair result unexpectedly retained the JS host"
            );
            assert_eq!(
                session
                    .js
                    .active_pattern()
                    .expect("active canonical pair")
                    .steps,
                Some(Fraction::int(steps)),
                "{source}: wrong pair step metadata"
            );
            assert_eq!(
                session
                    .query(Fraction::ZERO, Fraction::ONE)
                    .expect("query canonical pair")
                    .into_iter()
                    .map(|hap| hap.show())
                    .collect::<Vec<_>>(),
                expected,
                "{source}: wrong pair query timing"
            );

            let onsets = session
                .play(2.0)
                .expect("schedule canonical pair")
                .onsets
                .into_iter()
                .map(|onset| (onset.whole_begin, onset.value_show))
                .collect::<Vec<_>>();
            assert_eq!(
                onsets
                    .iter()
                    .map(|(whole, value)| (whole.as_str(), value.as_str()))
                    .collect::<Vec<_>>(),
                expected_onsets,
                "{source}: scheduler changed pair onsets"
            );
        }

        for source in [
            "sequence('a','b','c','d').shrink([1,2])",
            "shrink([1,2],sequence('a','b','c','d'))",
            "shrink([1,2])(sequence('a','b','c','d'))",
            "sequence('a','b','c','d').s_taper([1,2])",
            "s_taper([1,2],sequence('a','b','c','d'))",
            "s_taper([1,2])(sequence('a','b','c','d'))",
        ] {
            exercise(source, SHRINK_HAPS, 7, SHRINK_ONSETS);
        }
        for source in [
            "sequence('a','b','c','d').grow([1,2])",
            "grow([1,2],sequence('a','b','c','d'))",
            "grow([1,2])(sequence('a','b','c','d'))",
        ] {
            exercise(source, GROW_HAPS, 13, GROW_ONSETS);
        }
    }

    #[test]
    fn raw_shrink_grow_reach_session_query_play_and_atomic_resource_refusal() {
        const SHRINK_HAPS: &[&str] = &[
            "[ 0/1 → 1/7 | a ]",
            "[ 1/7 → 2/7 | b ]",
            "[ 2/7 → 3/7 | c ]",
            "[ 3/7 → 4/7 | d ]",
            "[ 4/7 → 5/7 | b ]",
            "[ 5/7 → 6/7 | c ]",
            "[ 6/7 → 1/1 | d ]",
        ];
        const GROW_HAPS: &[&str] = &[
            "[ 0/1 → 1/13 | a ]",
            "[ 1/13 → 2/13 | b ]",
            "[ (2/13 → 5/26) ⇝ 3/13 | c ]",
            "[ 5/26 → 7/26 | a ]",
            "[ 7/26 → 9/26 | b ]",
            "[ 9/26 → 11/26 | c ]",
            "[ 11/26 → 1/2 | a ]",
            "[ 1/2 → 15/26 | b ]",
            "[ 15/26 → 17/26 | c ]",
            "[ (17/26 → 9/13) ⇝ 19/26 | d ]",
            "[ 9/13 → 10/13 | a ]",
            "[ 10/13 → 11/13 | b ]",
            "[ 11/13 → 12/13 | c ]",
            "[ 12/13 → 1/1 | d ]",
        ];

        for (source, expected, steps, expected_onsets) in [
            (
                "sequence('a','b','c','d')._shrink([1,2])",
                SHRINK_HAPS,
                7,
                vec![
                    ("0/1", "a"),
                    ("1/7", "b"),
                    ("2/7", "c"),
                    ("3/7", "d"),
                    ("4/7", "b"),
                    ("5/7", "c"),
                    ("6/7", "d"),
                    ("1/1", "a"),
                ],
            ),
            (
                "sequence('a','b','c','d')._grow([1,2])",
                GROW_HAPS,
                13,
                vec![
                    ("0/1", "a"),
                    ("1/13", "b"),
                    ("2/13", "c"),
                    ("5/26", "a"),
                    ("7/26", "b"),
                    ("9/26", "c"),
                    ("11/26", "a"),
                    ("1/2", "b"),
                    ("15/26", "c"),
                    ("17/26", "d"),
                    ("9/13", "a"),
                    ("10/13", "b"),
                    ("11/13", "c"),
                    ("12/13", "d"),
                    ("1/1", "a"),
                ],
            ),
        ] {
            let mut session = Session::new().expect("raw pair session");
            session
                .evaluate(source)
                .unwrap_or_else(|error| panic!("evaluate {source}: {error}"));
            assert!(
                !session.active_needs_host(),
                "{source}: eager raw pair retained the JS host"
            );
            assert_eq!(
                session.js.active_pattern().expect("active raw pair").steps,
                Some(Fraction::int(steps))
            );
            assert_eq!(
                session
                    .query(Fraction::ZERO, Fraction::ONE)
                    .expect("query raw pair")
                    .into_iter()
                    .map(|hap| hap.show())
                    .collect::<Vec<_>>(),
                expected,
                "{source}: raw pair timing changed"
            );
            assert_eq!(
                session
                    .play(2.0)
                    .expect("schedule raw pair")
                    .onsets
                    .iter()
                    .map(|onset| (onset.whole_begin.as_str(), onset.value_show.as_str()))
                    .collect::<Vec<_>>(),
                expected_onsets,
                "{source}: raw scheduler onsets changed"
            );
        }

        let limit = rustel_core::MAX_STEPWISE_ENTRIES;
        let over = limit + 1;
        for name in ["_shrink", "_grow"] {
            let mut session = Session::new().expect("raw resource session");
            session
                .evaluate(&format!("gap({over}).{name}(0)"))
                .unwrap_or_else(|error| panic!("construct refused {name}: {error}"));
            assert!(
                !session.active_needs_host(),
                "{name}: resource graph retained the JS host"
            );
            let generation = session.generation();
            let queued = session.scheduler.queued();
            let horizon = session.scheduler.horizon_remaining(0.0);
            let error = session
                .schedule_at(0.0)
                .expect_err("raw MAX+1 expansion must refuse scheduling");
            let RuntimeError::ResourceLimit(message) = error else {
                panic!("{name}: raw refusal lost its type: {error:?}");
            };
            assert!(
                message.contains("shrink/grow")
                    && message.contains(&over.to_string())
                    && message.contains(&limit.to_string()),
                "{name}: wrong raw refusal: {message}"
            );
            assert!(
                matches!(
                    session.scheduler.refusal(),
                    Some(rustel_core::QueryLimit::StepwiseExpansion {
                        operation,
                        minimum_entries,
                        limit: refusal_limit,
                    }) if *operation == "shrink/grow"
                        && *minimum_entries == over
                        && *refusal_limit == limit
                ),
                "{name}: scheduler lost the structured raw refusal"
            );
            assert_eq!(session.scheduler.queued(), queued);
            assert_eq!(session.scheduler.horizon_remaining(0.0), horizon);
            assert_eq!(session.generation(), generation);

            session
                .evaluate("pure('raw-recovered')")
                .expect("install raw recovery score");
            let recovered = session
                .schedule_at(0.0)
                .expect("same-now recovery after raw refusal");
            assert_eq!(recovered.len(), 1);
            assert_eq!(recovered[0].onset_id, 0, "raw refusal consumed an onset id");
            assert_eq!(recovered[0].whole_begin, "0/1");
            assert_eq!(recovered[0].value_show, "raw-recovered");
        }
    }

    #[test]
    fn patterned_canonicals_keep_mutable_shrinklist_dispatch_in_session() {
        for name in ["shrink", "grow", "s_taper"] {
            let mut session = Session::new().expect("mutable canonical session");
            session
                .evaluate(&format!(
                    r#"
                      (() => {{
                        globalThis.sessionCanonicalFirstCalls = 0;
                        globalThis.sessionCanonicalSecondCalls = 0;
                        globalThis.sessionCanonicalReceiver =
                          sequence('a', 'b', 'c', 'd');
                        sessionCanonicalReceiver.shrinklist = function () {{
                          sessionCanonicalFirstCalls++;
                          return [this];
                        }};
                        globalThis.sessionCanonicalPattern =
                          sessionCanonicalReceiver.{name}(sequence(1, 2));
                        return sessionCanonicalPattern;
                      }})()
                    "#
                ))
                .unwrap_or_else(|error| panic!("construct mutable {name}: {error}"));
            assert!(
                session.active_needs_host(),
                "patterned {name} lost its mutable host dependency"
            );
            assert_eq!(
                session.js.get_number("sessionCanonicalFirstCalls"),
                Some(2.0),
                "patterned {name} changed its eager dispatch count"
            );
            assert_eq!(
                session
                    .js
                    .active_pattern()
                    .expect("active patterned canonical")
                    .steps,
                Some(Fraction::int(8)),
                "patterned {name} changed its eager cycle-zero metadata"
            );

            session
                .evaluate_prebake(
                    r#"
                      sessionCanonicalReceiver.shrinklist = function () {
                        sessionCanonicalSecondCalls++;
                        return [this];
                      };
                    "#,
                )
                .expect("replace canonical shrinklist after construction");
            session.js.run_gc();
            session.js.run_gc();
            let values = session
                .query(Fraction::ZERO, Fraction::ONE)
                .unwrap_or_else(|error| panic!("query mutable {name}: {error}"))
                .into_iter()
                .map(|hap| hap.value.show())
                .collect::<Vec<_>>();
            assert_eq!(values, ["a", "b", "c", "d", "a", "b", "c", "d"]);
            assert_eq!(
                session.js.get_number("sessionCanonicalSecondCalls"),
                Some(2.0),
                "patterned {name} did not dispatch once per factor hap"
            );
            assert_eq!(
                session
                    .js
                    .active_pattern()
                    .expect("active patterned canonical after mutation")
                    .steps,
                Some(Fraction::int(8)),
                "patterned {name} refreshed stale eager metadata"
            );

            session
                .evaluate_prebake("sessionCanonicalSecondCalls = 0;")
                .expect("reset mutable dispatch counter");
            let play = session
                .play(2.0)
                .unwrap_or_else(|error| panic!("schedule mutable {name}: {error}"));
            assert_eq!(
                play.onsets
                    .iter()
                    .map(|onset| onset.value_show.as_str())
                    .collect::<Vec<_>>(),
                ["a", "b", "c", "d", "a", "b", "c", "d", "a"],
                "Session play stopped using replacement shrinklist for {name}"
            );
            assert!(
                session
                    .js
                    .get_number("sessionCanonicalSecondCalls")
                    .is_some_and(|calls| calls > 0.0),
                "Session play did not enter replacement shrinklist for {name}"
            );
        }
    }

    #[test]
    fn shrink_grow_expansion_refusal_keeps_the_scheduler_atomic() {
        let limit = rustel_core::MAX_STEPWISE_SEGMENTS;
        let over = limit + 1;

        for operation in ["shrink", "grow"] {
            let mut session = Session::new().expect("stepwise expansion session");
            session
                .evaluate(&format!("gap({over}).{operation}(0)"))
                .unwrap_or_else(|error| panic!("construct refused {operation} graph: {error}"));
            assert!(
                !session.active_needs_host(),
                "resource-limit graph unexpectedly retained the JavaScript host"
            );

            let generation = session.generation();
            let queued = session.scheduler.queued();
            let horizon = session.scheduler.horizon_remaining(0.0);
            let error = session
                .schedule_at(0.0)
                .expect_err("MAX+1 expansion must refuse the scheduler tick");
            let RuntimeError::ResourceLimit(message) = error else {
                panic!("{operation} MAX+1 lost its typed refusal: {error:?}");
            };
            assert!(
                message.contains("shrink/grow")
                    && message.contains(&over.to_string())
                    && message.contains(&limit.to_string()),
                "wrong {operation} expansion refusal: {message}"
            );
            assert!(
                matches!(
                    session.scheduler.refusal(),
                    Some(rustel_core::QueryLimit::StepwiseExpansion {
                        operation: refusal_operation,
                        minimum_entries,
                        limit: refusal_limit,
                    }) if *refusal_operation == "shrink/grow"
                        && *minimum_entries == over
                        && *refusal_limit == limit
                ),
                "scheduler did not retain the structured {operation} refusal: {:?}",
                session.scheduler.refusal()
            );
            assert_eq!(
                session.scheduler.queued(),
                queued,
                "refused {operation} tick changed the event queue"
            );
            assert_eq!(
                session.scheduler.horizon_remaining(0.0),
                horizon,
                "refused {operation} tick advanced the query cursor"
            );
            assert_eq!(
                session.generation(),
                generation,
                "refused {operation} tick changed the score generation"
            );

            session
                .evaluate("pure('recovered')")
                .expect("install recovery score");
            let recovered = session
                .schedule_at(0.0)
                .expect("same-now recovery after expansion refusal");
            assert_eq!(recovered.len(), 1);
            assert_eq!(recovered[0].onset_id, 0, "refusal consumed an onset id");
            assert_eq!(recovered[0].whole_begin, "0/1");
            assert_eq!(recovered[0].value_show, "recovered");
        }
    }

    #[test]
    fn canonical_pair_and_override_expansion_boundaries_are_typed_and_atomic() {
        let limit = rustel_core::MAX_STEPWISE_SEGMENTS;
        assert_eq!(limit, rustel_core::MAX_STEPWISE_ENTRIES);
        assert_eq!(limit, 16_384);
        let over = limit + 1;

        fn assert_atomic_refusal(
            source: &str,
            operation: &str,
            limit: u64,
            over: u64,
            inspect_custom_phases: bool,
        ) {
            let mut session = Session::new().expect("canonical boundary session");
            session
                .evaluate(source)
                .unwrap_or_else(|error| panic!("construct refused {operation}: {error}"));
            assert!(
                !session.active_needs_host(),
                "{operation}: refusal graph retained the JavaScript host"
            );
            if inspect_custom_phases {
                assert_eq!(
                    session.js.get_number("canonicalBoundaryReverseCalls"),
                    Some(0.0)
                );
                assert_eq!(
                    session.js.get_number("canonicalBoundaryIteratorCalls"),
                    Some(0.0)
                );
                assert_eq!(
                    session.js.get_number("canonicalBoundaryReduceCalls"),
                    Some(0.0)
                );
            }

            let generation = session.generation();
            let queued = session.scheduler.queued();
            let horizon = session.scheduler.horizon_remaining(0.0);
            let error = session
                .schedule_at(0.0)
                .expect_err("MAX+1 canonical expansion must refuse scheduling");
            let RuntimeError::ResourceLimit(message) = error else {
                panic!("{operation}: canonical refusal lost its type: {error:?}");
            };
            assert!(
                message.contains("shrink/grow")
                    && message.contains(&over.to_string())
                    && message.contains(&limit.to_string()),
                "{operation}: wrong canonical refusal: {message}"
            );
            assert!(
                matches!(
                    session.scheduler.refusal(),
                    Some(rustel_core::QueryLimit::StepwiseExpansion {
                        operation: refusal_operation,
                        minimum_entries,
                        limit: refusal_limit,
                    }) if *refusal_operation == "shrink/grow"
                        && *minimum_entries == over
                        && *refusal_limit == limit
                ),
                "{operation}: scheduler lost structured canonical refusal: {:?}",
                session.scheduler.refusal()
            );
            assert_eq!(session.scheduler.queued(), queued);
            assert_eq!(session.scheduler.horizon_remaining(0.0), horizon);
            assert_eq!(session.generation(), generation);
            if inspect_custom_phases {
                assert_eq!(
                    session.js.get_number("canonicalBoundaryReverseCalls"),
                    Some(0.0)
                );
                assert_eq!(
                    session.js.get_number("canonicalBoundaryIteratorCalls"),
                    Some(0.0)
                );
                assert_eq!(
                    session.js.get_number("canonicalBoundaryReduceCalls"),
                    Some(0.0)
                );
            }

            session
                .evaluate("pure('canonical-recovered')")
                .expect("install canonical boundary recovery score");
            let recovered = session
                .schedule_at(0.0)
                .expect("same-now recovery after canonical refusal");
            assert_eq!(recovered.len(), 1);
            assert_eq!(recovered[0].onset_id, 0, "refusal consumed an onset id");
            assert_eq!(recovered[0].whole_begin, "0/1");
            assert_eq!(recovered[0].value_show, "canonical-recovered");
        }

        for operation in ["shrink", "grow"] {
            // A modest accepted control reaches the default helper handoff in
            // the Session product without repeating the exact-limit boundary
            // case under this route's fixed construction deadline.
            let mut accepted = Session::new().expect("accepted canonical pair session");
            accepted
                .evaluate(&format!("gap(128).{operation}([0, 128])"))
                .unwrap_or_else(|error| panic!("construct accepted pair {operation}: {error}"));
            assert!(!accepted.active_needs_host());
            assert!(
                accepted
                    .query(Fraction::ZERO, Fraction::ONE)
                    .unwrap_or_else(|error| panic!("query accepted pair {operation}: {error}"))
                    .is_empty(),
                "accepted pair {operation} emitted from a silent receiver"
            );

            let default_over = format!("gap({over}).{operation}([0, {over}])");
            assert_atomic_refusal(&default_over, operation, limit, over, false);

            // A matching accepted override proves the canonical body charges
            // an ordinary returned Array itself; MAX+1 below pins the product
            // boundary and atomic recovery.
            let mut custom_accepted = Session::new().expect("accepted override session");
            custom_accepted
                .evaluate(&format!(
                    r#"
                      (() => {{
                        globalThis.canonicalAcceptedReverseCalls = 0;
                        const receiver = gap(1);
                        receiver.shrinklist = function () {{
                          const list = Array(128).fill(this);
                          list.reverse = function () {{
                            canonicalAcceptedReverseCalls++;
                            return Array.prototype.reverse.call(this);
                          }};
                          return list;
                        }};
                        return receiver.{operation}(1);
                      }})()
                    "#
                ))
                .unwrap_or_else(|error| panic!("construct accepted override {operation}: {error}"));
            assert!(!custom_accepted.active_needs_host());
            assert_eq!(
                custom_accepted
                    .js
                    .get_number("canonicalAcceptedReverseCalls"),
                Some(if operation == "grow" { 1.0 } else { 0.0 }),
                "{operation}: accepted Array reverse phase changed"
            );
            assert!(
                custom_accepted
                    .query(Fraction::ZERO, Fraction::ONE)
                    .unwrap_or_else(|error| {
                        panic!("query accepted override {operation}: {error}")
                    })
                    .is_empty()
            );

            // Native safety intentionally refuses an oversized ordinary Array
            // before grow's reverse and before either canonical spreads or
            // reduces/reifies it. Proxy/accessor observation order is a
            // separate residual and is deliberately not exercised here.
            let custom_over = format!(
                r#"
                  (() => {{
                    globalThis.canonicalBoundaryReverseCalls = 0;
                    globalThis.canonicalBoundaryIteratorCalls = 0;
                    globalThis.canonicalBoundaryReduceCalls = 0;
                    const receiver = gap(1);
                    receiver.shrinklist = function () {{
                      const list = Array({over}).fill(this);
                      list.reverse = function () {{
                        canonicalBoundaryReverseCalls++;
                        return Array.prototype.reverse.call(this);
                      }};
                      list[Symbol.iterator] = function () {{
                        canonicalBoundaryIteratorCalls++;
                        return Array.prototype[Symbol.iterator].call(this);
                      }};
                      list.reduce = function (...args) {{
                        canonicalBoundaryReduceCalls++;
                        return Array.prototype.reduce.apply(this, args);
                      }};
                      return list;
                    }};
                    return receiver.{operation}(1);
                  }})()
                "#
            );
            assert_atomic_refusal(&custom_over, operation, limit, over, true);
        }
    }

    #[test]
    fn tour_keeps_exact_order_steps_and_scheduler_onsets() {
        let mut session = Session::new().expect("tour session");
        session
            .evaluate("pure('x').tour(pure('a'), pure('b'))")
            .expect("evaluate tour");
        assert!(
            !session.active_needs_host(),
            "a native tour graph was pushed onto the JavaScript scheduler path"
        );
        assert_eq!(
            session.js.active_pattern().expect("active tour").steps,
            Some(Fraction::int(9))
        );
        assert_eq!(
            session
                .query(Fraction::ZERO, Fraction::ONE)
                .expect("query tour")
                .into_iter()
                .map(|hap| hap.show())
                .collect::<Vec<_>>(),
            [
                "[ 0/1 → 1/9 | a ]",
                "[ 1/9 → 2/9 | b ]",
                "[ 2/9 → 1/3 | x ]",
                "[ 1/3 → 4/9 | a ]",
                "[ 4/9 → 5/9 | x ]",
                "[ 5/9 → 2/3 | b ]",
                "[ 2/3 → 7/9 | x ]",
                "[ 7/9 → 8/9 | a ]",
                "[ 8/9 → 1/1 | b ]",
            ]
        );

        let onsets = session
            .play(2.0)
            .expect("schedule tour")
            .onsets
            .into_iter()
            .map(|onset| (onset.whole_begin, onset.value_show))
            .collect::<Vec<_>>();
        assert_eq!(
            onsets,
            [
                ("0/1".to_owned(), "a".to_owned()),
                ("1/9".to_owned(), "b".to_owned()),
                ("2/9".to_owned(), "x".to_owned()),
                ("1/3".to_owned(), "a".to_owned()),
                ("4/9".to_owned(), "x".to_owned()),
                ("5/9".to_owned(), "b".to_owned()),
                ("2/3".to_owned(), "x".to_owned()),
                ("7/9".to_owned(), "a".to_owned()),
                ("8/9".to_owned(), "b".to_owned()),
                // The scheduler intentionally drains an onset exactly on the
                // requested duration boundary.
                ("1/1".to_owned(), "a".to_owned()),
            ]
        );
    }

    #[test]
    fn stepalt_and_s_alt_keep_lcm_order_steps_and_scheduler_onsets() {
        const CANONICAL: &str = "stepalt(\
          [pure('a'), pure('b')], [pure('c'), pure('d'), pure('e')])";
        const ALIAS: &str = "s_alt(\
          [pure('a'), pure('b')], [pure('c'), pure('d'), pure('e')])";
        const HAPS: &[&str] = &[
            "[ 0/1 → 1/12 | a ]",
            "[ 1/12 → 1/6 | c ]",
            "[ 1/6 → 1/4 | b ]",
            "[ 1/4 → 1/3 | d ]",
            "[ 1/3 → 5/12 | a ]",
            "[ 5/12 → 1/2 | e ]",
            "[ 1/2 → 7/12 | b ]",
            "[ 7/12 → 2/3 | c ]",
            "[ 2/3 → 3/4 | a ]",
            "[ 3/4 → 5/6 | d ]",
            "[ 5/6 → 11/12 | b ]",
            "[ 11/12 → 1/1 | e ]",
        ];

        let mut session = Session::new().expect("stepalt session");
        session
            .evaluate(CANONICAL)
            .expect("evaluate canonical stepalt");
        assert!(
            !session.active_needs_host(),
            "a pure stepalt graph was pushed onto the JavaScript scheduler path"
        );
        assert_eq!(
            session.js.active_pattern().expect("active stepalt").steps,
            Some(Fraction::int(12))
        );
        assert_eq!(
            session
                .query(Fraction::ZERO, Fraction::ONE)
                .expect("query canonical stepalt")
                .into_iter()
                .map(|hap| hap.show())
                .collect::<Vec<_>>(),
            HAPS
        );

        let onsets = session
            .play(2.0)
            .expect("schedule canonical stepalt")
            .onsets
            .into_iter()
            .map(|onset| (onset.whole_begin, onset.value_show))
            .collect::<Vec<_>>();
        assert_eq!(
            onsets,
            [
                ("0/1".to_owned(), "a".to_owned()),
                ("1/12".to_owned(), "c".to_owned()),
                ("1/6".to_owned(), "b".to_owned()),
                ("1/4".to_owned(), "d".to_owned()),
                ("1/3".to_owned(), "a".to_owned()),
                ("5/12".to_owned(), "e".to_owned()),
                ("1/2".to_owned(), "b".to_owned()),
                ("7/12".to_owned(), "c".to_owned()),
                ("2/3".to_owned(), "a".to_owned()),
                ("3/4".to_owned(), "d".to_owned()),
                ("5/6".to_owned(), "b".to_owned()),
                ("11/12".to_owned(), "e".to_owned()),
                // The scheduler drains an onset exactly on the duration edge.
                ("1/1".to_owned(), "a".to_owned()),
            ]
        );

        session.evaluate(ALIAS).expect("evaluate copied s_alt");
        assert_eq!(
            session
                .query(Fraction::ZERO, Fraction::ONE)
                .expect("query copied s_alt")
                .into_iter()
                .map(|hap| hap.show())
                .collect::<Vec<_>>(),
            HAPS,
            "s_alt changed canonical stepalt product timing"
        );
    }

    #[test]
    fn polymeter_aliases_keep_modern_legacy_steps_and_scheduler_onsets() {
        const MODERN: &str = "polymeter(sequence('a', 'b'), sequence('x', 'y', 'z'))";
        const MODERN_HAPS: &[&str] = &[
            "[ 0/1 → 1/6 | a ]",
            "[ 0/1 → 1/6 | x ]",
            "[ 1/6 → 1/3 | b ]",
            "[ 1/6 → 1/3 | y ]",
            "[ 1/3 → 1/2 | a ]",
            "[ 1/3 → 1/2 | z ]",
            "[ 1/2 → 2/3 | b ]",
            "[ 1/2 → 2/3 | x ]",
            "[ 2/3 → 5/6 | a ]",
            "[ 2/3 → 5/6 | y ]",
            "[ 5/6 → 1/1 | b ]",
            "[ 5/6 → 1/1 | z ]",
        ];

        let mut session = Session::new().expect("polymeter session");
        session
            .evaluate(MODERN)
            .expect("evaluate canonical modern polymeter");
        assert!(
            !session.active_needs_host(),
            "a pure polymeter graph entered the JavaScript scheduler path"
        );
        assert_eq!(
            session
                .js
                .active_pattern()
                .expect("active modern polymeter")
                .steps,
            Some(Fraction::int(6))
        );
        assert_eq!(
            session
                .query(Fraction::ZERO, Fraction::ONE)
                .expect("query canonical modern polymeter")
                .into_iter()
                .map(|hap| hap.show())
                .collect::<Vec<_>>(),
            MODERN_HAPS
        );
        assert_eq!(
            session
                .play(2.0)
                .expect("schedule canonical modern polymeter")
                .onsets
                .into_iter()
                .map(|onset| (onset.whole_begin, onset.value_show))
                .collect::<Vec<_>>(),
            [
                ("0/1".to_owned(), "a".to_owned()),
                ("0/1".to_owned(), "x".to_owned()),
                ("1/6".to_owned(), "b".to_owned()),
                ("1/6".to_owned(), "y".to_owned()),
                ("1/3".to_owned(), "a".to_owned()),
                ("1/3".to_owned(), "z".to_owned()),
                ("1/2".to_owned(), "b".to_owned()),
                ("1/2".to_owned(), "x".to_owned()),
                ("2/3".to_owned(), "a".to_owned()),
                ("2/3".to_owned(), "y".to_owned()),
                ("5/6".to_owned(), "b".to_owned()),
                ("5/6".to_owned(), "z".to_owned()),
                ("1/1".to_owned(), "a".to_owned()),
                ("1/1".to_owned(), "x".to_owned()),
            ]
        );

        session
            .evaluate("pm(sequence('a', 'b'), sequence('x', 'y', 'z'))")
            .expect("evaluate pm modern alias");
        assert_eq!(
            session
                .query(Fraction::ZERO, Fraction::ONE)
                .expect("query pm modern alias")
                .into_iter()
                .map(|hap| hap.show())
                .collect::<Vec<_>>(),
            MODERN_HAPS,
            "pm changed modern polymeter timing"
        );

        session
            .evaluate("s_polymeter(['a', 'b'], ['x', 'y', 'z'])")
            .expect("evaluate legacy polymeter alias");
        assert_eq!(
            session
                .js
                .active_pattern()
                .expect("active legacy polymeter")
                .steps,
            Some(Fraction::int(6)),
            "legacy metadata must remain the source-lane LCM"
        );
        assert_eq!(
            session
                .query(Fraction::ZERO, Fraction::ONE)
                .expect("query legacy polymeter alias")
                .into_iter()
                .map(|hap| hap.show())
                .collect::<Vec<_>>(),
            [
                "[ 0/1 → 1/2 | a ]",
                "[ 0/1 → 1/2 | x ]",
                "[ 1/2 → 1/1 | b ]",
                "[ 1/2 → 1/1 | y ]",
            ],
            "the legacy branch used modern LCM pacing"
        );
        assert_eq!(session.js.query_depth(), 0, "polymeter query roots leaked");
    }

    #[test]
    fn stepalt_saved_alias_survives_gc_and_destination_poisoning() {
        let mut session = Session::new().expect("stepalt snapshot session");
        session.js.install_gc_binding().expect("gc binding");
        session
            .evaluate_prebake(
                r#"
                globalThis.savedStepalt = Object.freeze({
                  canonical: rustelScope.stepalt,
                  alias: rustelScope.s_alt
                });
                savedStepalt.canonical.__sharedStepaltMarker = 61;
                if (savedStepalt.canonical !== savedStepalt.alias
                    || savedStepalt.alias.__sharedStepaltMarker !== 61) {
                  throw new Error('initial stepalt alias identity');
                }
                globalThis.stepaltCanonicalGlobalDecoy =
                  function stepaltCanonicalGlobalDecoy() {};
                globalThis.stepaltCanonicalScopeDecoy =
                  function stepaltCanonicalScopeDecoy() {};
                globalThis.stepaltAliasGlobalDecoy =
                  function stepaltAliasGlobalDecoy() {};
                globalThis.stepaltAliasScopeDecoy =
                  function stepaltAliasScopeDecoy() {};
                globalThis.stepaltMethodDecoy = function stepaltMethodDecoy() {};
                globalThis.stepalt = stepaltCanonicalGlobalDecoy;
                rustelScope.stepalt = stepaltCanonicalScopeDecoy;
                globalThis.s_alt = stepaltAliasGlobalDecoy;
                rustelScope.s_alt = stepaltAliasScopeDecoy;
                Pattern.prototype.stepalt = stepaltMethodDecoy;
                Pattern.prototype.s_alt = stepaltMethodDecoy;
                globalThis.assertStepaltPoison = () => {
                  if (globalThis.stepalt !== stepaltCanonicalGlobalDecoy
                      || rustelScope.stepalt !== stepaltCanonicalScopeDecoy
                      || globalThis.s_alt !== stepaltAliasGlobalDecoy
                      || rustelScope.s_alt !== stepaltAliasScopeDecoy
                      || Pattern.prototype.stepalt !== stepaltMethodDecoy
                      || Pattern.prototype.s_alt !== stepaltMethodDecoy) {
                    throw new Error('stepalt destinations were reinjected');
                  }
                };
                "#,
            )
            .expect("save and poison stepalt destinations");
        session.js.run_gc();
        session.js.run_gc();
        session
            .evaluate("assertStepaltPoison(); pure('successful-turn')")
            .expect("successful turn after stepalt poisoning");
        let failure = session
            .evaluate(
                "assertStepaltPoison(); globalThis.stepaltFailedTurn = 1; \
                 setcps(0); pure('failed-turn')",
            )
            .expect_err("invalid tempo must fail after the poison assertion");
        assert_eq!(failure.kind(), "evaluation");
        assert_eq!(session.js.get_number("stepaltFailedTurn"), Some(1.0));

        session
            .evaluate(
                r#"
                (() => {
                  assertStepaltPoison();
                  return savedStepalt.alias(
                    [pure('a'), pure('b')],
                    [pure('c'), pure('d'), pure('e')]
                  );
                })()
                "#,
            )
            .expect("call saved s_alt after GC and later turns");
        assert_eq!(
            session
                .query(Fraction::ZERO, Fraction::ONE)
                .expect("query saved s_alt")
                .into_iter()
                .map(|hap| hap.value.show())
                .collect::<Vec<_>>(),
            ["a", "c", "b", "d", "a", "e", "b", "c", "a", "d", "b", "e"]
        );
        assert_eq!(session.js.query_depth(), 0, "stepalt product roots leaked");
    }

    #[test]
    fn zip_and_s_zip_keep_lcm_order_steps_and_scheduler_onsets() {
        const CANONICAL: &str = "zip(sequence('a0', 'a1'), sequence('b0', 'b1', 'b2'))";
        const ALIAS: &str = "s_zip(sequence('a0', 'a1'), sequence('b0', 'b1', 'b2'))";
        const HAPS: &[&str] = &[
            "[ 0/1 → 1/6 | a0 ]",
            "[ 1/6 → 1/3 | b0 ]",
            "[ 1/3 → 1/2 | a1 ]",
            "[ 1/2 → 2/3 | b1 ]",
            "[ 2/3 → 5/6 | a0 ]",
            "[ 5/6 → 1/1 | b2 ]",
        ];

        let mut session = Session::new().expect("zip session");
        session.evaluate(CANONICAL).expect("evaluate canonical zip");
        assert!(
            !session.active_needs_host(),
            "a native zip graph was pushed onto the JavaScript scheduler path"
        );
        assert_eq!(
            session.js.active_pattern().expect("active zip").steps,
            Some(Fraction::int(6))
        );
        assert_eq!(
            session
                .query(Fraction::ZERO, Fraction::ONE)
                .expect("query canonical zip")
                .into_iter()
                .map(|hap| hap.show())
                .collect::<Vec<_>>(),
            HAPS
        );

        let onsets = session
            .play(2.0)
            .expect("schedule canonical zip")
            .onsets
            .into_iter()
            .map(|onset| (onset.whole_begin, onset.value_show))
            .collect::<Vec<_>>();
        assert_eq!(
            onsets,
            [
                ("0/1".to_owned(), "a0".to_owned()),
                ("1/6".to_owned(), "b0".to_owned()),
                ("1/3".to_owned(), "a1".to_owned()),
                ("1/2".to_owned(), "b1".to_owned()),
                ("2/3".to_owned(), "a0".to_owned()),
                ("5/6".to_owned(), "b2".to_owned()),
                // The scheduler intentionally drains an onset exactly on the
                // requested duration boundary.
                ("1/1".to_owned(), "a1".to_owned()),
            ]
        );

        session.evaluate(ALIAS).expect("evaluate copied s_zip");
        assert!(
            !session.active_needs_host(),
            "the copied free zip alias changed graph purity"
        );
        assert_eq!(
            session
                .query(Fraction::ZERO, Fraction::ONE)
                .expect("query copied s_zip")
                .into_iter()
                .map(|hap| hap.show())
                .collect::<Vec<_>>(),
            HAPS,
            "s_zip changed the canonical free function's product timing"
        );
    }

    #[test]
    fn shrinklist_growlist_and_s_taperlist_reach_session_query_play_after_gc() {
        const SHRINK_HAPS: &[&str] = &[
            "[ 0/1 → 1/9 | a ]",
            "[ 1/9 → 2/9 | b ]",
            "[ 2/9 → 1/3 | c ]",
            "[ 1/3 → 4/9 | d ]",
            "[ 4/9 → 5/9 | b ]",
            "[ 5/9 → 2/3 | c ]",
            "[ 2/3 → 7/9 | d ]",
            "[ 7/9 → 8/9 | c ]",
            "[ 8/9 → 1/1 | d ]",
        ];
        const GROW_VALUES: &[&str] = &["c", "d", "b", "c", "d", "a", "b", "c", "d"];

        let mut session = Session::new().expect("list-helper session");
        session.js.install_gc_binding().expect("gc binding");
        session
            .evaluate_prebake(
                r#"
                if (s_taperlist !== shrinklist
                    || Pattern.prototype.s_taperlist
                        !== Pattern.prototype.shrinklist) {
                  throw new Error('s_taperlist identity');
                }
                const savedShrinklist = shrinklist;
                const savedGrowlist = growlist;
                const savedTaperlist = s_taperlist;
                const dispatch = [];
                const receiver = {
                  shrinklist(value) { dispatch.push(`shrink:${value}`); return ['S']; },
                  growlist(value) { dispatch.push(`grow:${value}`); return ['G']; }
                };
                if (savedShrinklist(2, receiver)[0] !== 'S'
                    || savedTaperlist(3, receiver)[0] !== 'S'
                    || savedGrowlist(4, receiver)[0] !== 'G') {
                  throw new Error('free helper dispatch');
                }
                const reversed = ['left', 'right'];
                let reverseReceiver;
                reversed.reverse = function () {
                  reverseReceiver = this;
                  return Array.prototype.reverse.call(this);
                };
                const protoGrow = Pattern.prototype.growlist.call({
                  shrinklist(value) {
                    dispatch.push(`proto:${value}`);
                    return reversed;
                  }
                }, 5);
                if (dispatch.join(',') !== 'shrink:2,shrink:3,grow:4,proto:5'
                    || protoGrow !== reversed || reverseReceiver !== reversed
                    || protoGrow.join(',') !== 'right,left') {
                  throw new Error('dynamic helper dispatch');
                }

                globalThis.productListHelpers = Object.freeze({
                  shrink: savedShrinklist,
                  grow: savedGrowlist,
                  alias: savedTaperlist,
                  aliasMethod: Pattern.prototype.s_taperlist
                });
                globalThis.productListBase = sequence('a', 'b', 'c', 'd');
                globalThis.productListGlobalDecoy = function poisonedListGlobal() {};
                globalThis.productListScopeDecoy = function poisonedListScope() {};
                for (const name of ['shrinklist', 'growlist', 's_taperlist']) {
                  globalThis[name] = productListGlobalDecoy;
                  rustelScope[name] = productListScopeDecoy;
                }
                globalThis.assertProductListPoison = () => {
                  for (const name of ['shrinklist', 'growlist', 's_taperlist']) {
                    if (globalThis[name] !== productListGlobalDecoy
                        || rustelScope[name] !== productListScopeDecoy) {
                      throw new Error(`list helper destination reinjected: ${name}`);
                    }
                  }
                };
                "#,
            )
            .expect("retain and poison list helpers");
        session.js.run_gc();
        session.js.run_gc();

        let query_values = |session: &Session| {
            session
                .query(Fraction::ZERO, Fraction::ONE)
                .expect("query retained list helper")
                .into_iter()
                .map(|hap| hap.value.show())
                .collect::<Vec<_>>()
        };
        for source in [
            "stepcat(...productListHelpers.shrink([1, 3], productListBase))",
            "stepcat(...productListHelpers.alias([1, 3], productListBase))",
            "stepcat(...productListHelpers.aliasMethod.call(productListBase, [1, 3]))",
        ] {
            session
                .evaluate(&format!("assertProductListPoison(); {source}"))
                .unwrap_or_else(|error| panic!("evaluate {source}: {error}"));
            assert!(
                !session.active_needs_host(),
                "{source}: list graph became impure"
            );
            assert_eq!(
                session
                    .js
                    .active_pattern()
                    .expect("active list graph")
                    .steps,
                Some(Fraction::int(9)),
                "{source}: stepcat lost list step metadata"
            );
            assert_eq!(
                session
                    .query(Fraction::ZERO, Fraction::ONE)
                    .expect("query retained shrink helper")
                    .into_iter()
                    .map(|hap| hap.show())
                    .collect::<Vec<_>>(),
                SHRINK_HAPS,
                "{source}: shrinklist timing changed"
            );
        }

        let onsets = session
            .play(2.0)
            .expect("schedule retained shrinklist")
            .onsets
            .into_iter()
            .map(|onset| (onset.whole_begin, onset.value_show))
            .collect::<Vec<_>>();
        assert_eq!(
            onsets,
            [
                ("0/1".to_owned(), "a".to_owned()),
                ("1/9".to_owned(), "b".to_owned()),
                ("2/9".to_owned(), "c".to_owned()),
                ("1/3".to_owned(), "d".to_owned()),
                ("4/9".to_owned(), "b".to_owned()),
                ("5/9".to_owned(), "c".to_owned()),
                ("2/3".to_owned(), "d".to_owned()),
                ("7/9".to_owned(), "c".to_owned()),
                ("8/9".to_owned(), "d".to_owned()),
                ("1/1".to_owned(), "a".to_owned()),
            ],
            "Session scheduler changed shrinklist inclusive-boundary onsets"
        );

        session
            .evaluate(
                "assertProductListPoison(); \
                 stepcat(...productListHelpers.grow([1, 3], productListBase))",
            )
            .expect("evaluate retained growlist");
        assert_eq!(
            query_values(&session),
            GROW_VALUES,
            "growlist did not reverse the same planned list"
        );
        assert_eq!(
            session.js.query_depth(),
            0,
            "list-helper query roots leaked"
        );
    }

    #[test]
    fn shrinklist_helper_refusal_is_typed_atomic_and_same_now_recoverable() {
        let limit = rustel_core::MAX_STEPWISE_ENTRIES;
        assert_eq!(limit, 16_384);
        let over = limit + 1;

        for helper in ["s_taperlist", "growlist"] {
            let mut session = Session::new().expect("list-helper refusal session");
            session
                .evaluate(&format!("stepcat(...{helper}([0, {over}], gap({over})))"))
                .unwrap_or_else(|error| panic!("construct refused {helper} graph: {error}"));
            assert!(
                !session.active_needs_host(),
                "{helper}: resource refusal retained the JavaScript host"
            );

            let generation = session.generation();
            let queued = session.scheduler.queued();
            let horizon = session.scheduler.horizon_remaining(0.0);
            let error = session
                .schedule_at(0.0)
                .expect_err("MAX+1 list must refuse scheduling");
            let RuntimeError::ResourceLimit(message) = error else {
                panic!("{helper}: list refusal lost its runtime type: {error:?}");
            };
            assert!(
                message.contains("shrinklist")
                    && message.contains(&over.to_string())
                    && message.contains(&limit.to_string()),
                "{helper}: wrong list expansion refusal: {message}"
            );
            assert!(
                matches!(
                    session.scheduler.refusal(),
                    Some(rustel_core::QueryLimit::StepwiseExpansion {
                        operation,
                        minimum_entries,
                        limit: refusal_limit,
                    }) if *operation == "shrinklist"
                        && *minimum_entries == over
                        && *refusal_limit == limit
                ),
                "{helper}: scheduler lost structured shrinklist refusal: {:?}",
                session.scheduler.refusal()
            );
            assert_eq!(session.scheduler.queued(), queued);
            assert_eq!(session.scheduler.horizon_remaining(0.0), horizon);
            assert_eq!(session.generation(), generation);

            session
                .evaluate("pure('list-recovered')")
                .expect("install list-helper recovery score");
            let recovered = session
                .schedule_at(0.0)
                .expect("same-now recovery after list-helper refusal");
            assert_eq!(recovered.len(), 1);
            assert_eq!(recovered[0].onset_id, 0, "refusal consumed an onset id");
            assert_eq!(recovered[0].whole_begin, "0/1");
            assert_eq!(recovered[0].value_show, "list-recovered");
        }
    }

    #[test]
    fn tour_expansion_refusal_keeps_the_scheduler_atomic_and_recovers() {
        let limit = rustel_core::MAX_STEPWISE_ENTRIES;
        assert_eq!(limit, 16_384);

        let mut exact = Session::new().expect("exact tour session");
        exact
            .evaluate("silence.tour(...Array.from({ length: 127 }, () => silence))")
            .expect("construct exact-bound tour");
        assert_eq!(
            exact.js.active_pattern().expect("exact-bound tour").steps,
            Some(Fraction::int(16_384))
        );
        assert!(
            exact
                .schedule_at(0.0)
                .expect("schedule exact-bound tour")
                .is_empty(),
            "the all-silent exact-bound control emitted an onset"
        );

        let mut session = Session::new().expect("refused tour session");
        session
            .evaluate("silence.tour(...Array.from({ length: 128 }, () => silence))")
            .expect("construct refused tour graph");
        assert!(!session.active_needs_host());
        let generation = session.generation();
        let queued = session.scheduler.queued();
        let horizon = session.scheduler.horizon_remaining(0.0);
        let error = session
            .schedule_at(0.0)
            .expect_err("16641-entry tour must refuse the scheduler tick");
        let RuntimeError::ResourceLimit(message) = error else {
            panic!("tour expansion lost its typed refusal: {error:?}");
        };
        assert!(
            message.contains("tour")
                && message.contains("16641")
                && message.contains(&limit.to_string()),
            "wrong tour expansion refusal: {message}"
        );
        assert!(
            matches!(
                session.scheduler.refusal(),
                Some(rustel_core::QueryLimit::StepwiseExpansion {
                    operation,
                    minimum_entries,
                    limit: refusal_limit,
                }) if *operation == "tour"
                    && *minimum_entries == 16_641
                    && *refusal_limit == limit
            ),
            "scheduler did not retain the structured tour refusal: {:?}",
            session.scheduler.refusal()
        );
        assert_eq!(session.scheduler.queued(), queued);
        assert_eq!(session.scheduler.horizon_remaining(0.0), horizon);
        assert_eq!(session.generation(), generation);

        session
            .evaluate("pure('tour-recovered')")
            .expect("install recovery score");
        let recovered = session
            .schedule_at(0.0)
            .expect("same-now recovery after tour refusal");
        assert_eq!(recovered.len(), 1);
        assert_eq!(recovered[0].onset_id, 0, "refusal consumed an onset id");
        assert_eq!(recovered[0].whole_begin, "0/1");
        assert_eq!(recovered[0].value_show, "tour-recovered");
    }

    #[test]
    fn stepalt_source_and_lcm_boundaries_are_typed_atomic_and_recoverable() {
        let limit = rustel_core::MAX_STEPWISE_ENTRIES;
        assert_eq!(limit, 16_384);

        // LCM 8192 across two groups creates exactly 16,384 cycle-major
        // candidate references. All are zero-step and filtered afterwards,
        // so scheduling proves the boundary without producing 16k haps.
        let mut exact = Session::new().expect("exact stepalt session");
        exact
            .evaluate("stepalt(Array(8192).fill(nothing), [nothing])")
            .expect("construct exact-bound stepalt");
        assert_eq!(
            exact
                .js
                .active_pattern()
                .expect("exact-bound stepalt")
                .steps,
            Some(Fraction::ZERO)
        );
        assert!(
            exact
                .schedule_at(0.0)
                .expect("schedule exact-bound stepalt")
                .is_empty(),
            "the all-filtered exact-bound control emitted an onset"
        );

        let mut session = Session::new().expect("refused stepalt session");
        session
            .evaluate("stepalt(Array(8193).fill(nothing), [nothing])")
            .expect("construct refused LCM-amplified stepalt graph");
        assert!(!session.active_needs_host());
        let generation = session.generation();
        let queued = session.scheduler.queued();
        let horizon = session.scheduler.horizon_remaining(0.0);
        let error = session
            .schedule_at(0.0)
            .expect_err("16386-entry stepalt must refuse the scheduler tick");
        let RuntimeError::ResourceLimit(message) = error else {
            panic!("stepalt expansion lost its typed refusal: {error:?}");
        };
        assert!(
            message.contains("stepalt")
                && message.contains("16386")
                && message.contains(&limit.to_string()),
            "wrong stepalt expansion refusal: {message}"
        );
        assert!(
            matches!(
                session.scheduler.refusal(),
                Some(rustel_core::QueryLimit::StepwiseExpansion {
                    operation,
                    minimum_entries,
                    limit: refusal_limit,
                }) if *operation == "stepalt"
                    && *minimum_entries == 16_386
                    && *refusal_limit == limit
            ),
            "scheduler did not retain the structured stepalt refusal: {:?}",
            session.scheduler.refusal()
        );
        assert_eq!(session.scheduler.queued(), queued);
        assert_eq!(session.scheduler.horizon_remaining(0.0), horizon);
        assert_eq!(session.generation(), generation);

        session
            .evaluate("pure('stepalt-recovered')")
            .expect("install recovery score");
        let recovered = session
            .schedule_at(0.0)
            .expect("same-now recovery after stepalt refusal");
        assert_eq!(recovered.len(), 1);
        assert_eq!(recovered[0].onset_id, 0, "refusal consumed an onset id");
        assert_eq!(recovered[0].whole_begin, "0/1");
        assert_eq!(recovered[0].value_show, "stepalt-recovered");

        // An empty group makes cyclic expansion zero, but strudel.cc first maps
        // every other source group. The native cap also bounds that source
        // stage and deliberately refuses before invoking the mutable parser.
        let mut source = Session::new().expect("stepalt source-bound session");
        source
            .evaluate_prebake(
                "globalThis.stepaltRefusedParserCalls = 0; \
                 setStringParser(value => { \
                   stepaltRefusedParserCalls++; return pure(value); \
                 });",
            )
            .expect("install stepalt parser counter");
        source
            .evaluate("stepalt([], Array(16385).fill('x'))")
            .expect("construct source-bound stepalt refusal");
        assert_eq!(
            source.js.get_number("stepaltRefusedParserCalls"),
            Some(0.0),
            "oversized stepalt reified input before its resource preflight"
        );
        let error = source
            .schedule_at(0.0)
            .expect_err("oversized zero-expansion source must still refuse");
        let RuntimeError::ResourceLimit(message) = error else {
            panic!("stepalt source bound lost its typed refusal: {error:?}");
        };
        assert!(
            message.contains("stepalt")
                && message.contains("16385")
                && message.contains(&limit.to_string()),
            "wrong stepalt source-stage refusal: {message}"
        );
    }

    #[test]
    fn polymeter_boundaries_are_typed_atomic_and_recoverable() {
        let limit = rustel_core::MAX_STEPWISE_ENTRIES;
        assert_eq!(limit, 16_384);

        for (source, label) in [
            ("polymeter(gap(1), gap(16383))", "modern"),
            ("polymeter(Array(8192).fill(silence), [silence])", "legacy"),
        ] {
            let mut exact = Session::new().expect("exact polymeter session");
            exact
                .evaluate(source)
                .unwrap_or_else(|error| panic!("construct exact {label} polymeter: {error}"));
            assert!(
                exact
                    .schedule_at(0.0)
                    .unwrap_or_else(|error| panic!("schedule exact {label} polymeter: {error}"))
                    .is_empty(),
                "all-silent exact {label} polymeter emitted an onset"
            );
        }

        for (source, minimum_entries, label) in [
            ("polymeter(gap(1), gap(16384))", 16_385, "modern"),
            (
                "polymeter(Array(8193).fill(silence), [silence])",
                16_386,
                "legacy",
            ),
        ] {
            let mut session = Session::new().expect("refused polymeter session");
            session
                .evaluate(source)
                .unwrap_or_else(|error| panic!("construct refused {label} polymeter: {error}"));
            assert!(
                !session.active_needs_host(),
                "refused {label} polymeter unexpectedly retained the host"
            );
            let generation = session.generation();
            let queued = session.scheduler.queued();
            let horizon = session.scheduler.horizon_remaining(0.0);
            let error = match session.schedule_at(0.0) {
                Ok(events) => {
                    panic!("{label} polymeter did not refuse the scheduler tick: {events:?}")
                }
                Err(error) => error,
            };
            let RuntimeError::ResourceLimit(message) = error else {
                panic!("{label} polymeter lost its typed refusal: {error:?}");
            };
            assert!(
                message.contains("polymeter")
                    && message.contains(&minimum_entries.to_string())
                    && message.contains(&limit.to_string()),
                "wrong {label} polymeter refusal: {message}"
            );
            assert!(
                matches!(
                    session.scheduler.refusal(),
                    Some(rustel_core::QueryLimit::StepwiseExpansion {
                        operation,
                        minimum_entries: observed,
                        limit: refusal_limit,
                    }) if *operation == "polymeter"
                        && *observed == minimum_entries
                        && *refusal_limit == limit
                ),
                "scheduler lost the structured {label} polymeter refusal: {:?}",
                session.scheduler.refusal()
            );
            assert_eq!(session.scheduler.queued(), queued);
            assert_eq!(session.scheduler.horizon_remaining(0.0), horizon);
            assert_eq!(session.generation(), generation);

            session
                .evaluate(&format!("pure('{label}-polymeter-recovered')"))
                .unwrap_or_else(|error| panic!("install {label} recovery score: {error}"));
            let recovered = session
                .schedule_at(0.0)
                .unwrap_or_else(|error| panic!("same-now {label} recovery: {error}"));
            assert_eq!(recovered.len(), 1);
            assert_eq!(recovered[0].onset_id, 0, "refusal consumed an onset id");
            assert_eq!(recovered[0].whole_begin, "0/1");
            assert_eq!(
                recovered[0].value_show,
                format!("{label}-polymeter-recovered")
            );
        }
    }

    #[test]
    fn indexed_echo_owns_callback_arguments_and_results_across_setup_gc() {
        let mut session = Session::new().expect("session");
        session.js.install_gc_binding().expect("gc binding");
        session
            .evaluate(
                r#"
                (() => {
                  globalThis.__echoIndexedCalls = [];
                  globalThis.__echoRetainedArgs = [];
                  const source = pure('x').fmap(value => value);
                  const result = source.echoWith(3, 1/4, function (pattern, index) {
                    __echoIndexedCalls.push(
                      `${arguments.length}:${typeof index}:${index}`
                    );
                    __echoRetainedArgs.push(pattern);
                    return pattern.fmap(value => `${value}:${index}`);
                  });
                  globalThis.__echoIndexedShape = JSON.stringify(__echoIndexedCalls);
                  return result;
                })()
                "#,
            )
            .expect("install indexed echo score");
        assert_eq!(
            session.js.get_string("__echoIndexedShape").as_deref(),
            Some(r#"["2:number:0","2:number:1","2:number:2"]"#),
            "scalar echoWith did not invoke its callback eagerly, once per copy, with the exact index argument"
        );
        assert!(
            session.active_needs_host(),
            "returned fmap callbacks were incorrectly classified as host-free"
        );
        let active = session.js.active_pattern().expect("active indexed graph");
        assert_eq!(
            active.reachable_callbacks().len(),
            4,
            "the final graph must own one source callback and three returned callbacks, but not the construction-only indexed callback"
        );

        let snapshot = |session: &Session| {
            let mut haps = session
                .query(Fraction::ZERO, Fraction::ONE)
                .expect("query indexed echo graph")
                .into_iter()
                .map(|hap| {
                    let whole = hap.whole.expect("echo haps are discrete");
                    (
                        hap.value.show(),
                        whole.begin,
                        whole.end,
                        hap.part.begin,
                        hap.part.end,
                    )
                })
                .collect::<Vec<_>>();
            haps.sort();
            haps
        };
        let expected = vec![
            (
                "x:0".to_owned(),
                Fraction::ZERO,
                Fraction::ONE,
                Fraction::ZERO,
                Fraction::ONE,
            ),
            (
                "x:1".to_owned(),
                Fraction::new(-3, 4),
                Fraction::new(1, 4),
                Fraction::ZERO,
                Fraction::new(1, 4),
            ),
            (
                "x:1".to_owned(),
                Fraction::new(1, 4),
                Fraction::new(5, 4),
                Fraction::new(1, 4),
                Fraction::ONE,
            ),
            (
                "x:2".to_owned(),
                Fraction::new(-1, 2),
                Fraction::new(1, 2),
                Fraction::ZERO,
                Fraction::new(1, 2),
            ),
            (
                "x:2".to_owned(),
                Fraction::new(1, 2),
                Fraction::new(3, 2),
                Fraction::new(1, 2),
                Fraction::ONE,
            ),
        ];
        assert_eq!(snapshot(&session), expected, "wrong indexed echo timing");

        session.js.run_gc();
        session.js.run_gc();
        session
            .evaluate_prebake(
                r#"
                (() => {
                  const state = {
                    span: { begin: Fraction(0), end: Fraction(1) },
                    controls: {}
                  };
                  globalThis.__echoRetainedProof = JSON.stringify(
                    __echoRetainedArgs.map(pattern => {
                      const haps = pattern.query(state);
                      return haps.length > 0
                        && haps.every(hap => hap.value === 'x');
                    })
                  );
                })()
                "#,
            )
            .expect("query retained callback arguments after GC");
        assert_eq!(
            session.js.get_string("__echoRetainedProof").as_deref(),
            Some("[true,true,true]"),
            "callback argument wrappers lost their source sidecars after GC"
        );
        assert_eq!(
            snapshot(&session),
            expected,
            "returned patterns lost callback ownership after GC and another setup turn"
        );
        assert_eq!(session.js.query_depth(), 0, "indexed query scope leaked");
        assert_eq!(
            rustel_jsruntime::bridge_frame_depth(),
            0,
            "indexed query leaked a bridge frame"
        );
        assert_eq!(
            rustel_jsruntime::bridge_scratch_len(),
            0,
            "indexed query leaked harvested callback roots"
        );
    }

    #[test]
    fn projected_list_surface_survives_setup_turn_gc_and_global_decoys() {
        let mut session = Session::new().expect("session");
        session.js.install_gc_binding().expect("gc binding");
        session
            .evaluate_prebake(
                r#"
                const expected = [
                  'pace', 'take', 'drop', 'extend', 'replicate', 'expand',
                  'contract', 'shrink', 'grow',
                  'Fraction', 'Pattern', 'cat', 'fastcat', 'gap', 'growlist', 'noteToMidi',
                  'nothing', 'pm', 'polymeter', 'polyrhythm', 'pr', 'pure',
                  'register', 'reify',
                  's_add', 's_alt', 's_cat', 's_contract', 's_expand', 's_extend',
                  's_polymeter', 's_sub', 's_taper', 's_taperlist', 's_tour', 's_zip',
                  'seq', 'sequence',
                  'setStringParser', 'shrinklist', 'silence',
                  'slowcat', 'stack', 'stepalt', 'stepcat', 'steps', 'rustelScope',
                  'timeCat', 'timecat', 'tour', 'zip',
                  'setCps', 'setcps', 'setCpm', 'setcpm', 'cps',
                  'midimaps', 'defaultmidimap',
                  'voicings', 'rootNotes', 'voicing',
                  'setDefaultVoicings', 'resetVoicings'
                ];
                const keys = Object.keys(rustelScope);
                if (JSON.stringify(keys) !== JSON.stringify(expected)) {
                  throw new Error(`list scope order: ${keys.join(',')}`);
                }
                for (const name of expected) {
                  if (rustelScope[name] !== globalThis[name]) {
                    throw new Error(`list scope identity: ${name}`);
                  }
                }
                if (rustelScope.voicing !== globalThis.voicing) {
                  throw new Error('native voicing projection lost');
                }
                if (rustelScope.polyrhythm !== rustelScope.pr
                    || rustelScope.pr !== rustelScope.stack
                    || rustelScope.timecat !== rustelScope.stepcat) {
                  throw new Error('list alias identity');
                }
                if (rustelScope.cat === rustelScope.slowcat
                    || rustelScope.fastcat === rustelScope.seq
                    || rustelScope.fastcat === rustelScope.sequence
                    || rustelScope.seq === rustelScope.sequence) {
                  throw new Error('distinct list declarations collapsed');
                }
                const singletonNames = ['cat', 'fastcat', 'seq', 'sequence', 'slowcat'];
                for (const name of [...singletonNames, 'stack']) {
                  const descriptor = Object.getOwnPropertyDescriptor(Pattern.prototype, name);
                  if (!descriptor || descriptor.value.name !== name
                      || descriptor.value.length !== 0 || !descriptor.writable
                      || descriptor.enumerable || !descriptor.configurable) {
                    throw new Error(`prototype list descriptor: ${name}`);
                  }
                }
                const owned = suffix => `owned:${suffix}`;
                const source = reify(owned);
                for (const name of singletonNames) {
                  if (rustelScope[name](source) !== source
                      || rustelScope[name]([[[source]]]) !== source
                      || source[name]() !== source) {
                    throw new Error(`singleton wrapper identity: ${name}`);
                  }
                }
                if (rustelScope.stack(source) === source || source.stack() === source) {
                  throw new Error('stack incorrectly reused a singleton wrapper');
                }
                globalThis.productListSingleton = rustelScope.seq([[[source]]]);
                for (const name of [
                  'cat', 'fastcat', 'polyrhythm', 'pr', 'seq', 'sequence',
                  'slowcat', 'stack', 'stepcat', 'timecat'
                ]) {
                  globalThis[name] = function poisonedListGlobal() {
                    throw new Error(`mutable global consulted: ${name}`);
                  };
                }
                globalThis.productWeighted = rustelScope.timecat(
                  [1, productListSingleton], [3, pure('tail')]
                );
                globalThis.productFast = pure('left').fastcat(pure('right'));
                globalThis.productStack = productListSingleton.stack();
                "#,
            )
            .expect("install projected list surface product state");
        session
            .evaluate_prebake("__gc(); __gc();")
            .expect("collect between setup and score turns");
        session
            .evaluate(
                r#"
                rustelScope.stack(
                  productWeighted.fmap(value =>
                    typeof value === 'function' ? value('weighted') : value
                  ),
                  productFast,
                  productStack.fmap(value => value('stack'))
                )
                "#,
            )
            .expect("score through retained list surface");

        let mut got = session
            .query(Fraction::ZERO, Fraction::ONE)
            .expect("query retained list score")
            .into_iter()
            .map(|hap| (hap.value.show(), hap.part.begin, hap.part.end))
            .collect::<Vec<_>>();
        got.sort_by(|left, right| left.0.cmp(&right.0));
        assert_eq!(
            got,
            [
                ("left".to_owned(), Fraction::ZERO, Fraction::new(1, 2),),
                ("owned:stack".to_owned(), Fraction::ZERO, Fraction::ONE,),
                (
                    "owned:weighted".to_owned(),
                    Fraction::ZERO,
                    Fraction::new(1, 4),
                ),
                ("right".to_owned(), Fraction::new(1, 2), Fraction::ONE,),
                ("tail".to_owned(), Fraction::new(1, 4), Fraction::ONE,),
            ],
            "projected list route lost identity, receiver order, or timing"
        );
        assert_eq!(
            session.js.query_depth(),
            0,
            "list product leaked query roots"
        );
    }

    #[test]
    fn projected_stepwise_aliases_survive_gc_and_independent_destination_poisoning() {
        let mut session = Session::new().expect("session");
        session.js.install_gc_binding().expect("gc binding");
        session
            .evaluate_prebake(
                r#"
                const methodAliases = [
                  ['s_taper', 'shrink'],
                  ['s_taperlist', 'shrinklist'],
                  ['s_add', 'take'],
                  ['s_sub', 'drop'],
                  ['s_expand', 'expand'],
                  ['s_extend', 'extend'],
                  ['s_contract', 'contract'],
                  ['steps', 'pace']
                ];
                const freeAliases = [
                  ...methodAliases,
                  ['s_cat', 'stepcat'],
                  ['timeCat', 'stepcat'],
                  ['timecat', 'stepcat']
                ];
                const assertOrdinaryDataProperty = (object, name, value, label) => {
                  const descriptor = Object.getOwnPropertyDescriptor(object, name);
                  if (!descriptor || !Object.hasOwn(descriptor, 'value')
                      || descriptor.value !== value || !descriptor.writable
                      || !descriptor.enumerable || !descriptor.configurable) {
                    throw new Error(`${label} descriptor: ${name}`);
                  }
                };

                for (const [alias, canonical] of freeAliases) {
                  if (globalThis[alias] !== globalThis[canonical]
                      || rustelScope[alias] !== globalThis[canonical]) {
                    throw new Error(`free alias identity: ${alias}`);
                  }
                  assertOrdinaryDataProperty(
                    globalThis, alias, globalThis[canonical], 'global alias'
                  );
                  assertOrdinaryDataProperty(
                    rustelScope, alias, globalThis[canonical], 'scope alias'
                  );
                }
                for (const [alias, canonical] of methodAliases) {
                  if (Pattern.prototype[alias] !== Pattern.prototype[canonical]) {
                    throw new Error(`method alias identity: ${alias}`);
                  }
                  assertOrdinaryDataProperty(
                    Pattern.prototype,
                    alias,
                    Pattern.prototype[canonical],
                    'prototype alias'
                  );
                }
                for (const name of ['timecat', 'timeCat', 's_cat']) {
                  if (Object.hasOwn(Pattern.prototype, name)) {
                    throw new Error(`non-method alias installed on Pattern: ${name}`);
                  }
                }
                const forbiddenRawAliases = [
                  '_s_add', '_s_sub', '_s_taper', '_s_taperlist', '_s_expand', '_s_extend',
                  '_s_contract', '_s_cat', '_timeCat', '_timecat'
                ];
                for (const [label, object] of [
                  ['global', globalThis],
                  ['scope', rustelScope],
                  ['prototype', Pattern.prototype]
                ]) {
                  for (const name of forbiddenRawAliases) {
                    if (Object.hasOwn(object, name)) {
                      throw new Error(`${label} raw alias: ${name}`);
                    }
                  }
                }

                // Each saved function comes from a different public slot. The
                // later score invokes all of them only after two forced GCs.
                globalThis.productStepwiseAliases = Object.freeze({
                  taper: rustelScope.s_taper,
                  taperMethod: Pattern.prototype.s_taper,
                  take: globalThis.s_add,
                  drop: rustelScope.s_sub,
                  expandMethod: Pattern.prototype.s_expand,
                  paceMethod: Pattern.prototype.steps,
                  weighted: rustelScope.timeCat
                });

                globalThis.productAliasGlobalDecoy =
                  function poisonedAliasGlobal() {
                    throw new Error('mutable alias global consulted');
                  };
                globalThis.productAliasScopeDecoy =
                  function poisonedAliasScope() {
                    throw new Error('mutable alias scope consulted');
                  };
                globalThis.productAliasPrototypeDecoy =
                  function poisonedAliasPrototype() {
                    throw new Error('mutable alias prototype consulted');
                  };
                globalThis.productTaperCanonicalDecoys = Object.freeze({
                  global: function poisonedTaperGlobal() {
                    throw new Error('mutable shrink global consulted');
                  },
                  scope: function poisonedTaperScope() {
                    throw new Error('mutable shrink scope consulted');
                  },
                  method: function poisonedTaperMethod() {
                    throw new Error('mutable shrink prototype consulted');
                  }
                });
                globalThis.s_add = productAliasGlobalDecoy;
                rustelScope.s_sub = productAliasScopeDecoy;
                Pattern.prototype.s_expand = productAliasPrototypeDecoy;
                globalThis.s_taper = productAliasGlobalDecoy;
                rustelScope.s_taper = productAliasScopeDecoy;
                Pattern.prototype.s_taper = productAliasPrototypeDecoy;
                globalThis.shrink = productTaperCanonicalDecoys.global;
                rustelScope.shrink = productTaperCanonicalDecoys.scope;
                Pattern.prototype.shrink = productTaperCanonicalDecoys.method;
                delete globalThis.steps;
                delete rustelScope.timeCat;
                delete Pattern.prototype.s_contract;

                globalThis.assertStepwiseAliasPoison = () => {
                  if (globalThis.s_add !== productAliasGlobalDecoy
                      || rustelScope.s_sub !== productAliasScopeDecoy
                      || Pattern.prototype.s_expand
                          !== productAliasPrototypeDecoy
                      || globalThis.s_taper !== productAliasGlobalDecoy
                      || rustelScope.s_taper !== productAliasScopeDecoy
                      || Pattern.prototype.s_taper
                          !== productAliasPrototypeDecoy
                      || globalThis.shrink
                          !== productTaperCanonicalDecoys.global
                      || rustelScope.shrink
                          !== productTaperCanonicalDecoys.scope
                      || Pattern.prototype.shrink
                          !== productTaperCanonicalDecoys.method
                      || Object.hasOwn(globalThis, 'steps')
                      || Object.hasOwn(rustelScope, 'timeCat')
                      || Object.hasOwn(Pattern.prototype, 's_contract')) {
                    throw new Error('static stepwise aliases were reinjected');
                  }
                };
                "#,
            )
            .expect("install and poison projected stepwise aliases");

        session
            .evaluate(
                r#"
                (() => {
                  assertStepwiseAliasPoison();
                  return productStepwiseAliases.weighted(
                    [1, pure(70)], [1, pure(71)]
                  );
                })()
                "#,
            )
            .expect("successful score through a saved alias");
        session.js.run_gc();
        session.js.run_gc();

        let failure = session
            .evaluate(
                r#"
                (() => {
                  assertStepwiseAliasPoison();
                  globalThis.productAliasFailedTurn = 1;
                  setcps(0);
                  return pure(0);
                })()
                "#,
            )
            .expect_err("invalid-tempo score must fail after checking alias poison");
        assert_eq!(failure.kind(), "evaluation");
        assert_eq!(
            session.js.get_number("productAliasFailedTurn"),
            Some(1.0),
            "the failed score never reached its reinjection assertion"
        );

        session
            .evaluate(
                r#"
                (() => {
                  assertStepwiseAliasPoison();
                  const taperBase = () => sequence(60, 61, 62, 63);
                  const tapered = productStepwiseAliases.taper(
                    1, taperBase()
                  );
                  const taperedCurried = productStepwiseAliases.taper(1)(
                    taperBase()
                  );
                  const taperedMethod = productStepwiseAliases.taperMethod.call(
                    taperBase(), 1
                  );
                  const taperState = {
                    span: { begin: 0, end: 1 }, controls: {}
                  };
                  const taperView = pattern => `${pattern._steps.show()}|`
                    + pattern.query(taperState).map(hap =>
                      `${hap.value}:${hap.part.begin.show()}`
                        + `>${hap.part.end.show()}`
                    ).join(',');
                  const expectedTaper = '10/1|60:0/1>1/10,61:1/10>1/5,'
                    + '62:1/5>3/10,63:3/10>2/5,61:2/5>1/2,'
                    + '62:1/2>3/5,63:3/5>7/10,62:7/10>4/5,'
                    + '63:4/5>9/10,63:9/10>1/1';
                  if ([tapered, taperedCurried, taperedMethod].some(
                    pattern => taperView(pattern) !== expectedTaper
                  )) {
                    throw new Error('saved s_taper semantics mismatch');
                  }
                  globalThis.productTaperProof = 1;
                  const taken = productStepwiseAliases.take(
                    2, sequence(10, 11, 12)
                  );
                  const dropped = productStepwiseAliases.drop(
                    1, sequence(20, 21, 22)
                  );
                  const expanded = productStepwiseAliases.expandMethod.call(
                    sequence(40, 41), 2
                  );
                  const paced = productStepwiseAliases.paceMethod.call(
                    sequence(50, 51), 4
                  );
                  const weighted = productStepwiseAliases.weighted(
                    [1, pure(30)], [1, pure(31)]
                  );
                  return stack(
                    taken,
                    dropped,
                    weighted,
                    stepcat(expanded, 99),
                    paced
                  );
                })()
                "#,
            )
            .expect("score through saved aliases after GC and failed evaluation");
        assert_eq!(
            session.js.get_number("productTaperProof"),
            Some(1.0),
            "saved s_taper was not exercised after GC and destination poisoning"
        );

        let got = session
            .query(Fraction::ZERO, Fraction::ONE)
            .expect("query retained stepwise aliases")
            .into_iter()
            .map(|hap| hap.show())
            .collect::<Vec<_>>();
        assert_eq!(
            got,
            [
                "[ 0/1 → 1/4 | 50 ]",
                "[ 0/1 → 2/5 | 40 ]",
                "[ 0/1 → 1/2 | 10 ]",
                "[ 0/1 → 1/2 | 21 ]",
                "[ 0/1 → 1/2 | 30 ]",
                "[ 1/4 → 1/2 | 51 ]",
                "[ 2/5 → 4/5 | 41 ]",
                "[ 1/2 → 3/4 | 50 ]",
                "[ 1/2 → 1/1 | 11 ]",
                "[ 1/2 → 1/1 | 22 ]",
                "[ 1/2 → 1/1 | 31 ]",
                "[ 3/4 → 1/1 | 51 ]",
                "[ 4/5 → 1/1 | 99 ]",
            ],
            "saved stepwise aliases lost callability or representative semantics"
        );
        assert_eq!(
            session.js.query_depth(),
            0,
            "alias product leaked query roots"
        );
    }

    #[test]
    fn projected_elementals_survive_setup_gc_and_mutable_global_decoys() {
        let mut session = Session::new().expect("session");
        session.js.install_gc_binding().expect("gc binding");
        session
            .evaluate_prebake(
                r#"
                const expected = [
                  'pace', 'take', 'drop', 'extend', 'replicate', 'expand',
                  'contract', 'shrink', 'grow',
                  'Fraction', 'Pattern', 'cat', 'fastcat', 'gap', 'growlist', 'noteToMidi',
                  'nothing', 'pm', 'polymeter', 'polyrhythm', 'pr', 'pure',
                  'register', 'reify',
                  's_add', 's_alt', 's_cat', 's_contract', 's_expand', 's_extend',
                  's_polymeter', 's_sub', 's_taper', 's_taperlist', 's_tour', 's_zip',
                  'seq', 'sequence',
                  'setStringParser', 'shrinklist', 'silence',
                  'slowcat', 'stack', 'stepalt', 'stepcat', 'steps', 'rustelScope',
                  'timeCat', 'timecat', 'tour', 'zip',
                  'setCps', 'setcps', 'setCpm', 'setcpm', 'cps',
                  'midimaps', 'defaultmidimap',
                  'voicings', 'rootNotes', 'voicing',
                  'setDefaultVoicings', 'resetVoicings'
                ];
                const keys = Object.keys(rustelScope);
                if (JSON.stringify(keys) !== JSON.stringify(expected)) {
                  throw new Error(`elemental scope order: ${keys.join(',')}`);
                }
                for (const name of expected) {
                  const descriptor = Object.getOwnPropertyDescriptor(
                    rustelScope, name
                  );
                  const globalDescriptor = Object.getOwnPropertyDescriptor(
                    globalThis, name
                  );
                  if (rustelScope[name] !== globalThis[name]
                      || !descriptor?.writable || !descriptor.enumerable
                      || !descriptor.configurable || !globalDescriptor?.writable
                      || !globalDescriptor.enumerable
                      || !globalDescriptor.configurable) {
                    throw new Error(`elemental scope identity: ${name}`);
                  }
                }
                if (rustelScope.voicing !== globalThis.voicing) {
                  throw new Error('native voicing projection lost');
                }
                if (Object.getPrototypeOf(rustelScope) !== Object.prototype
                    || rustelScope.rustelScope !== rustelScope) {
                  throw new Error('elemental scope shape');
                }

                const constructible = value => {
                  try { Reflect.construct(value, []); return true; }
                  catch (_) { return false; }
                };
                if (Fraction.name !== 'fraction' || Fraction.length !== 1
                    || Object.hasOwn(Fraction, 'prototype')
                    || constructible(Fraction)
                    || JSON.stringify(Object.keys(Fraction)) !== '["_original"]'
                    || Fraction._original.name !== 'Fraction') {
                  throw new Error('Fraction reflection');
                }
                const originalDescriptor = Object.getOwnPropertyDescriptor(
                  Fraction, '_original'
                );
                if (!originalDescriptor.writable || !originalDescriptor.enumerable
                    || !originalDescriptor.configurable) {
                  throw new Error('Fraction._original descriptor');
                }
                if (gap.name !== 'gap' || gap.length !== 1
                    || Object.hasOwn(gap, 'prototype') || constructible(gap)) {
                  throw new Error('gap reflection');
                }
                if (pure.name !== 'pure' || pure.length !== 1
                    || !Object.hasOwn(pure, 'prototype') || !constructible(pure)) {
                  throw new Error('pure reflection');
                }

                const normalized = Fraction('2/6');
                if (!(normalized instanceof Fraction._original)
                    || Object.getPrototypeOf(normalized) !== Fraction._original.prototype
                    || normalized.show() !== '1/3' || Number(normalized) !== 1 / 3
                    || Object.getPrototypeOf(gap('2/6')._steps)
                        !== Fraction._original.prototype) {
                  throw new Error('Fraction normalization');
                }
                if (silence === nothing || silence._steps.show() !== '1/1'
                    || nothing._steps.show() !== '0/1'
                    || gap(1) === silence || gap(0) === nothing) {
                  throw new Error('canonical empty singleton shape');
                }
                if (Object.hasOwn(silence, '__pure')
                    || Object.hasOwn(nothing, '__pure')) {
                  throw new Error('empty singletons are not pure values');
                }

                globalThis.elementalCanonicals = {
                  Fraction: rustelScope.Fraction,
                  gap: rustelScope.gap,
                  nothing: rustelScope.nothing,
                  pure: rustelScope.pure,
                  silence: rustelScope.silence,
                  stack: rustelScope.stack,
                  stepcat: rustelScope.stepcat
                };
                globalThis.elementalSilence = elementalCanonicals.silence;
                globalThis.elementalNothing = elementalCanonicals.nothing;
                globalThis.elementalObject = { marker: 7 };
                globalThis.elementalFunction = suffix => `owned:${suffix}`;
                globalThis.elementalPureObject = elementalCanonicals.pure(elementalObject);
                globalThis.elementalPureFunction = elementalCanonicals.pure(elementalFunction);
                if (elementalPureObject.__pure !== elementalObject
                    || elementalPureFunction.__pure !== elementalFunction
                    || elementalCanonicals.pure(elementalObject) === elementalPureObject
                    || new (elementalCanonicals.pure)(elementalObject).__pure
                        !== elementalObject) {
                  throw new Error('pure construction identity');
                }

                // Both evalScope destinations are ordinary mutable snapshots
                // on strudel.cc. Save the canonical objects outside those slots,
                // then make assignment and deletion independently observable.
                globalThis.elementalGlobalDecoys = {};
                globalThis.elementalScopeDecoys = {};
                for (const name of ['Fraction', 'gap', 'pure']) {
                  const globalDecoy = function poisonedElementalGlobal() {
                    throw new Error(`mutable global consulted: ${name}`);
                  };
                  const scopeDecoy = function poisonedElementalScope() {
                    throw new Error(`mutable scope consulted: ${name}`);
                  };
                  elementalGlobalDecoys[name] = globalDecoy;
                  elementalScopeDecoys[name] = scopeDecoy;
                  globalThis[name] = globalDecoy;
                  rustelScope[name] = scopeDecoy;
                }
                for (const name of ['nothing', 'silence']) {
                  delete globalThis[name];
                  delete rustelScope[name];
                }
                for (const name of ['Pattern', 'Hap']) {
                  const decoy = function poisonedElementalDependency() {
                    throw new Error(`mutable dependency consulted: ${name}`);
                  };
                  elementalGlobalDecoys[name] = decoy;
                  globalThis[name] = decoy;
                }
                globalThis.elementalGap = elementalCanonicals.gap(
                  elementalCanonicals.Fraction('2/6')
                );
                if (elementalGap._steps.show() !== '1/3') {
                  throw new Error('gap Fraction coercion');
                }
                "#,
            )
            .expect("install projected elemental product state");
        session
            .evaluate_prebake("__gc(); __gc();")
            .expect("collect between elemental setup and score turns");
        session
            .evaluate(
                r#"
                (() => {
                  const state = {
                    span: {
                      begin: elementalCanonicals.Fraction(0),
                      end: elementalCanonicals.Fraction(1)
                    },
                    controls: {}
                  };
                  const objectHaps = elementalPureObject.query(state);
                  const functionHaps = elementalPureFunction.query(state);
                  if (objectHaps.length !== 1
                      || objectHaps[0].value !== elementalObject
                      || functionHaps.length !== 1
                      || functionHaps[0].value !== elementalFunction) {
                    throw new Error('pure JS identity lost across setup GC');
                  }
                  if (elementalCanonicals.silence !== elementalSilence
                      || elementalCanonicals.nothing !== elementalNothing
                      || elementalCanonicals.silence === elementalCanonicals.nothing) {
                    throw new Error('empty singleton identity lost across turns');
                  }
                  for (const name of ['Fraction', 'gap', 'pure']) {
                    const globalDecoy = elementalGlobalDecoys[name];
                    const scopeDecoy = elementalScopeDecoys[name];
                    if (globalThis[name] !== globalDecoy
                        || globalDecoy.name !== 'poisonedElementalGlobal'
                        || rustelScope[name] !== scopeDecoy
                        || scopeDecoy.name !== 'poisonedElementalScope') {
                      throw new Error(`static destination was reinjected: ${name}`);
                    }
                  }
                  for (const name of ['nothing', 'silence']) {
                    if (Object.hasOwn(globalThis, name)
                        || Object.hasOwn(rustelScope, name)) {
                      throw new Error(`deleted static was reinjected: ${name}`);
                    }
                  }
                  for (const name of ['Pattern', 'Hap']) {
                    const decoy = elementalGlobalDecoys[name];
                    if (globalThis[name] !== decoy
                        || decoy.name !== 'poisonedElementalDependency') {
                      throw new Error(`lexical dependency was reinjected: ${name}`);
                    }
                  }
                  return elementalCanonicals.stack(
                    elementalCanonicals.stepcat(
                      elementalGap, elementalCanonicals.pure('afterGap')
                    ),
                    elementalCanonicals.stepcat(
                      elementalNothing, elementalCanonicals.pure('fromNothing')
                    ),
                    elementalCanonicals.stepcat(
                      elementalSilence, elementalCanonicals.pure('fromSilence')
                    ),
                    elementalPureObject.fmap(value => `object:${value.marker}`),
                    elementalPureFunction.fmap(value => value('pure'))
                  );
                })()
                "#,
            )
            .expect("score through retained elemental surface");

        let mut got = session
            .query(Fraction::ZERO, Fraction::ONE)
            .expect("query retained elemental score")
            .into_iter()
            .map(|hap| (hap.value.show(), hap.part.begin, hap.part.end))
            .collect::<Vec<_>>();
        got.sort_by(|left, right| left.0.cmp(&right.0));
        assert_eq!(
            got,
            [
                ("afterGap".to_owned(), Fraction::new(1, 4), Fraction::ONE,),
                ("fromNothing".to_owned(), Fraction::ZERO, Fraction::ONE,),
                ("fromSilence".to_owned(), Fraction::new(1, 2), Fraction::ONE,),
                ("object:7".to_owned(), Fraction::ZERO, Fraction::ONE,),
                ("owned:pure".to_owned(), Fraction::ZERO, Fraction::ONE,),
            ],
            "elemental route lost Fraction/gap timing, singleton steps, or JS identity"
        );
        assert_eq!(
            session.js.query_depth(),
            0,
            "elemental product leaked query roots"
        );
    }

    #[test]
    fn failed_prebake_keeps_partial_js_mutations_but_not_scheduler_state() {
        let mut session = Session::new().expect("session");
        session
                .evaluate(
                    "(() => { let calls = 0; return pure('last-good').fmap(value => `${value}:${++calls}`); })()",
                )
                .expect("callback-bearing initial score");
        let generation = session.generation();
        assert_eq!(active_values(&session), ["last-good:1"]);
        let error = session
            .evaluate_prebake(
                "globalThis.__rustel_active = { query: null }; \
                 globalThis.__rustel_active.query = () => []; \
                 globalThis.partialSetup = 17; \
                 Pattern.prototype.afterFailure = function () { return this.fast(2); }; \
                 throw new Error('setup stopped here');",
            )
            .expect_err("throwing prebake must fail");
        assert_eq!(error.kind(), "evaluation");
        assert_eq!(session.js.get_number("partialSetup"), Some(17.0));
        assert_eq!(session.generation(), generation);
        session.js.run_gc();
        session.js.run_gc();
        assert_eq!(
            active_values(&session),
            ["last-good:2"],
            "failed setup replaced the active wrapper or its callback"
        );
        session
            .reload_at("note('e4').afterFailure()", false, 1.0)
            .expect("partial prototype side effect");
    }

    // -- scheduler callback lifetime ---------------------------------------
    //
    // These live inside the module rather than in `tests/` because they need
    // `self.js`, which is private, to install the `__gc()` binding a callback
    // uses to collect from inside JavaScript.

    /// A bind whose callback builds a nested callback graph on every
    /// invocation, and collects while doing so. Each nested `polyBind` adds a
    /// cell to the live `BridgeFrame`, a GC root, so the frame's lifetime
    /// sets how long those cells survive.
    const NESTED_CALLBACK_SOURCE: &str = r#"pure('bd').polyBind(x => {
        __gc();
        globalThis.peakCells = Math.max(globalThis.peakCells || 0, __cellsLive());
        return pure(x).polyBind(y => pure(y).fast(2));
    })"#;

    fn nested_callback_session() -> Session {
        let mut session = Session::new().expect("session");
        session.ensure_bindings().expect("bindings");
        session.js.install_gc_binding().expect("gc binding");
        session
            .evaluate(NESTED_CALLBACK_SOURCE)
            .expect("evaluate nested callback source");
        assert!(
            session.active_needs_host(),
            "the source must be impure, or this proves nothing about the host \
             scope"
        );
        session
    }

    /// The PEAK live-cell count observed from inside the callback while
    /// playback was running, plus how many cells the run created and how many
    /// onsets it scheduled.
    ///
    /// The peak is sampled by the callback while playback is active. After
    /// `play` returns the frame has already been dropped, hiding any growth
    /// caused by an overly broad frame lifetime.
    fn peak_cells_during_play(duration_secs: f64) -> (u64, u64, usize) {
        let mut session = nested_callback_session();
        let created_before = rustel_jsruntime::cells_created();
        let report = session.play(duration_secs).expect("play");
        let peak = session
            .js
            .get_number("peakCells")
            .expect("the callback must have run and recorded a peak") as u64;
        let created = rustel_jsruntime::cells_created() - created_before;
        (created, peak, report.onsets.len())
    }

    #[test]
    fn scheduler_callback_cells_do_not_grow_with_playback_length() {
        // A `BridgeFrame` lives for one `Scheduler::tick` query, not for the
        // whole play loop, so live cells do not grow with playback length.
        // Compare growth: process-wide counters may include unrelated work.
        let (short_created, short_live, short_onsets) = peak_cells_during_play(1.0);
        let (long_created, long_live, long_onsets) = peak_cells_during_play(12.0);

        assert!(
            long_created > short_created * 4,
            "the longer run must actually invoke the callback many more times, \
             or the plateau below is vacuous: {short_created} vs {long_created}"
        );
        assert!(
            long_live <= short_live + 8,
            "callback cells accumulate with playback length: peak {short_live} \
             during 1s, {long_live} during 12s ({short_created} vs \
             {long_created} created). The bridge frame is outliving the tick \
             that created its cells."
        );
        // ...and the output is still right, so the teardown did not simply
        // break the callbacks.
        assert!(short_onsets > 0, "short run scheduled nothing");
        assert!(
            long_onsets > short_onsets,
            "the longer run scheduled no extra onsets: {short_onsets} vs \
             {long_onsets}"
        );
    }

    #[test]
    fn scheduled_callback_output_survives_per_tick_teardown() {
        // Tearing the frame down per tick must not cost correctness: the
        // callback graph is rebuilt on each tick's query and has to keep
        // producing the same events.
        let first = nested_callback_session().play(2.0).expect("play");
        let second = nested_callback_session().play(2.0).expect("play again");
        let show = |r: &PlayReport| {
            r.onsets
                .iter()
                .map(|o| format!("{}@{}", o.value_show, o.whole_begin))
                .collect::<Vec<_>>()
        };
        assert!(!first.onsets.is_empty(), "scheduled nothing");
        assert_eq!(
            show(&first),
            show(&second),
            "replaying the same graph produced different onsets"
        );
    }

    #[test]
    fn every_scheduling_scope_unwinds() {
        // Depth counters back to zero after a successful run, after a run whose
        // callback THROWS, and for a pure graph that opens no scope at all.
        for source in [
            NESTED_CALLBACK_SOURCE,
            r#"pure('bd').polyBind(x => { throw new Error('boom'); })"#,
            r#"s("bd sd")"#,
        ] {
            let mut session = Session::new().expect("session");
            session.ensure_bindings().expect("bindings");
            session.js.install_gc_binding().expect("gc binding");
            session.evaluate(source).expect("evaluate");
            let _ = session.play(2.0).expect("play");
            assert_eq!(session.js.query_depth(), 0, "{source}: query stack");
            assert_eq!(
                rustel_jsruntime::bridge_frame_depth(),
                0,
                "{source}: bridge frame still open"
            );
            assert_eq!(
                rustel_jsruntime::bridge_scratch_len(),
                0,
                "{source}: scratch not drained"
            );
        }
    }

    #[test]
    fn play_restarts_the_transport_so_a_prior_stop_is_not_cancellation() {
        // `play` calls `transport.start()` before scheduling, so a Stop
        // issued beforehand is cleared. A run is cancelled only from the
        // handle, concurrently, while it is in flight.
        let mut session = Session::new().expect("session");
        session.evaluate(r#"s("bd sd")"#).expect("evaluate");
        session.transport().stop();
        assert!(
            !session.play(2.0).expect("play").onsets.is_empty(),
            "`play` no longer restarts the transport; if that is deliberate, \
             stop-before-play is now cancellation and this test should say so"
        );
    }

    #[test]
    fn play_carries_the_legato_alias_as_effective_hap_duration() {
        let mut session = Session::new().expect("session");
        session
            .evaluate(r#"note("c4").legato(0.25)"#)
            .expect("evaluate the public alias");
        let report = session.play(0.1).expect("play");
        let onset = report.onsets.first().expect("one scheduled onset");
        let crate::ValueJson::Raw(serde_json::Value::Object(value)) = &onset.value else {
            panic!("control pattern did not produce an object value");
        };
        assert_eq!(value.get("clip"), Some(&serde_json::json!(0.25)));
        assert!(
            (onset.duration_secs - 0.5).abs() < f64::EPSILON,
            "legato writes clip=0.25, so one cycle at 0.5 cps must gate for 0.5s; got {}",
            onset.duration_secs
        );
    }

    #[test]
    fn stopping_the_transport_mid_run_cancels_it() {
        // A `play` that ignores Stop also returns, so termination proves
        // nothing. Stop discards queued events, so a cancelled run yields
        // fewer onsets. The full run takes seconds; the stop lands at 100 ms.
        let mut session = Session::new().expect("session");
        session.evaluate(r#"s("bd*8")"#).expect("evaluate");
        let full = session.play(86_400.0).expect("play").onsets.len();
        assert!(
            full > 1000,
            "the uncancelled run scheduled only {full} onsets"
        );

        let mut session = Session::new().expect("session");
        session.evaluate(r#"s("bd*8")"#).expect("evaluate");
        let handle = session.transport();
        let stopper = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(100));
            handle.stop();
        });
        let started = std::time::Instant::now();
        let cancelled = session.play(86_400.0).expect("play").onsets.len();
        let elapsed = started.elapsed();
        stopper.join().expect("stopper thread");

        assert!(
            cancelled < full,
            "a mid-run Stop changed nothing: {cancelled} onsets vs {full} \
             uncancelled. Stop must discard queued events, not drain them."
        );
        assert!(
            elapsed < std::time::Duration::from_secs(60),
            "the cancelled run took {elapsed:?}; Stop must end it promptly"
        );
    }

    #[test]
    fn stop_ends_the_computation_not_just_the_output() {
        // Stop must end the work, not only the output. The scheduler
        // suppresses events after Stop either way, so the test compares the
        // loop's iteration count and elapsed time, not only the onset count.
        let mut session = Session::new().expect("session");
        session.evaluate(r#"s("bd*8")"#).expect("evaluate");
        let started = std::time::Instant::now();
        let full = session.play(86_400.0).expect("play").onsets.len();
        let full_elapsed = started.elapsed();
        let full_iterations = session.last_play_iterations;
        assert!(
            full > 1000,
            "the uncancelled run scheduled only {full} onsets"
        );
        assert!(
            full_iterations > 1_000_000,
            "the uncancelled run only iterated {full_iterations} times, so the \
             comparison below has no headroom"
        );

        let mut session = Session::new().expect("session");
        session.evaluate(r#"s("bd*8")"#).expect("evaluate");
        session.transport().stop();
        // `play` restarts the transport, so stop from a handle DURING the run.
        let handle = session.transport();
        let stopper = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(50));
            handle.stop();
        });
        let started = std::time::Instant::now();
        let cancelled = session.play(86_400.0).expect("play").onsets.len();
        let cancelled_elapsed = started.elapsed();
        stopper.join().expect("stopper");

        assert!(
            cancelled < full,
            "Stop delivered as many onsets as an uncancelled run: {cancelled} \
             vs {full}"
        );
        // A loose bound: a run that keeps querying after Stop takes seconds
        // and fails it.
        assert!(
            cancelled_elapsed * 10 < full_elapsed,
            "a stopped run took {cancelled_elapsed:?} against {full_elapsed:?} \
             uncancelled - Stop is not ending the work"
        );
        // The loop must stop iterating, not just stop producing. Timing alone
        // is too sensitive to machine load, so the loop counts itself.
        let cancelled_iterations = session.last_play_iterations;
        assert!(
            cancelled_iterations < full_iterations / 10,
            "a stopped run performed {cancelled_iterations} of the \
             {full_iterations} iterations an uncancelled run does: `play` is not \
             breaking on `TickStatus::Stopped`, it is spinning through the rest \
             of the window doing nothing"
        );
    }

    #[test]
    fn the_public_play_api_validates_its_own_duration() {
        // `play` is public, so an embedder or a test can call it without the
        // CLI's validation. `Session::play(f64::INFINITY)` must be refused.
        let mut session = Session::new().expect("session");
        session.evaluate(r#"s("bd")"#).expect("evaluate");
        for bad in [f64::INFINITY, f64::NAN, -1.0, 1e300, 1e12] {
            let result = session.play(bad);
            assert!(
                result.is_err(),
                "Session::play({bad}) was accepted; the bound must live at the \
                 library boundary, not only in the argument parser"
            );
        }
        // ...and the ordinary case still works.
        assert!(!session.play(2.0).expect("play").onsets.is_empty());
    }

    #[test]
    fn every_session_config_field_is_validated() {
        // `play`'s loop bound is a sum that includes `horizon`, so an
        // infinite horizon would keep a two-second render from terminating.
        let bad = [
            (
                "horizon",
                SessionConfig {
                    horizon: f64::INFINITY,
                    ..Default::default()
                },
            ),
            (
                "horizon",
                SessionConfig {
                    horizon: f64::NAN,
                    ..Default::default()
                },
            ),
            (
                "horizon",
                SessionConfig {
                    horizon: 0.0,
                    ..Default::default()
                },
            ),
            (
                "horizon",
                SessionConfig {
                    horizon: -1.0,
                    ..Default::default()
                },
            ),
            // Shorter than the scheduler's smallest refill floor.
            (
                "horizon",
                SessionConfig {
                    horizon: 0.0005,
                    ..Default::default()
                },
            ),
            (
                "horizon",
                SessionConfig {
                    horizon: 1e12,
                    ..Default::default()
                },
            ),
            (
                "cps",
                SessionConfig {
                    cps: 0.0,
                    ..Default::default()
                },
            ),
            (
                "cps",
                SessionConfig {
                    cps: f64::INFINITY,
                    ..Default::default()
                },
            ),
            (
                "cps",
                SessionConfig {
                    cps: f64::NAN,
                    ..Default::default()
                },
            ),
            (
                "cps",
                SessionConfig {
                    cps: -1.0,
                    ..Default::default()
                },
            ),
            (
                "sample_rate",
                SessionConfig {
                    sample_rate: 0,
                    ..Default::default()
                },
            ),
            (
                "channels",
                SessionConfig {
                    channels: 0,
                    ..Default::default()
                },
            ),
        ];
        for (field, config) in bad {
            assert!(
                Session::with_config(config).is_err(),
                "an invalid {field} was accepted; every config field feeds a \
                 loop bound, a divisor or an allocation"
            );
        }
        // ...and the default is of course still valid.
        assert!(Session::with_config(SessionConfig::default()).is_ok());
    }

    #[test]
    fn score_sample_effects_have_no_io_capability_by_default() {
        let mut session = Session::new().expect("session");
        session
            .evaluate(r#"samples('http://127.0.0.1:9/strudel.json'); s('bd')"#)
            .expect("the pattern remains usable when registration is denied");
        assert!(
            session.sample_library().is_none(),
            "a denied score-level samples() call initialized an I/O subsystem"
        );
    }

    #[test]
    fn a_dense_tick_cannot_allocate_past_the_scheduler_queue_bound() {
        // `tick` materialises every onset in the horizon span in ONE pass, so a
        // caller-side cap on the finished timeline is checked too late by
        // construction. This asserts the refusal happens and is reported,
        // rather than the process growing into it.
        let mut session = Session::new().expect("session");
        session.evaluate(r#"s("bd*16 sd*16")"#).expect("evaluate");
        let error = session
            .play(86_400.0)
            .expect_err("a dense maximal window must be refused");
        let text = error.to_string();
        assert!(
            text.contains("allocation bound"),
            "the refusal did not name the allocation bound: {text}"
        );
    }

    #[cfg(feature = "device-audio")]
    fn loading_prefill_fixture() -> (
        Session,
        crate::LiveFileProducer,
        Arc<crate::samples::SampleLibrary>,
    ) {
        let library = Arc::new(crate::samples::SampleLibrary::with_loading_sample_for_test(
            "heldsample",
        ));
        let mut session = Session::with_config(SessionConfig {
            cps: 1.0,
            horizon: 0.5,
            ..Default::default()
        })
        .expect("session");
        session.samples = Some(Arc::clone(&library));
        session.set_direct_diagnostic_logging(false);
        session.set_schedule_lead(0.0);
        session.set_continuity_margin(0.0);
        session.evaluate_mini("~").expect("initial silent source");
        session.restart_transport_at(0.0);
        let mut producer =
            crate::LiveFileProducer::unwatched(Duration::from_millis(2)).expect("producer");
        let initial = producer
            .step_unwatched_with_clock_and_cutover(
                &mut session,
                || 0.0,
                48_000,
                |_, _, _| panic!("initial prefill must not publish a replacement"),
                |_| panic!("initial rest emitted audio"),
            )
            .expect("initial silent prefill");
        assert_eq!(
            (initial.scheduled, initial.pushed, initial.pending),
            (0, 0, 0)
        );
        assert!(
            session.audible_source.is_none(),
            "no output receipt is bound"
        );
        let _ = producer.producer_load_snapshot();
        (session, producer, library)
    }

    /// "Sample X is still loading" is reported once per score, not once per
    /// retry window, and again for the next score, including one that a
    /// JavaScript evaluate installs.
    #[cfg(feature = "device-audio")]
    #[test]
    fn a_still_loading_sample_is_news_once_per_evaluated_score() {
        let (mut session, mut producer, _library) = loading_prefill_fixture();
        let _ = session.take_diagnostics();
        let still_loading = |session: &mut Session| {
            session
                .take_diagnostics()
                .iter()
                .filter(|diagnostic| diagnostic.kind == SAMPLE_LOADING_DIAGNOSTIC)
                .count()
        };
        let step = |session: &mut Session, producer: &mut crate::LiveFileProducer, clock| {
            producer
                .step_unwatched_with_clock_and_cutover(
                    session,
                    || clock,
                    48_000,
                    |_, _, _| panic!("a loading score published"),
                    |_| panic!("a loading onset emitted audio"),
                )
                .expect("a loading window is a held turn")
        };

        for score in ["first", "second"] {
            let before = session.generation();
            session
                .evaluate(r#"s("heldsample*4")"#)
                .expect("the JavaScript door");
            assert_eq!(session.last_path, EvaluateSource::JavaScript);
            session.restart_transport_at(0.0);
            producer.arm_replacement(before, session.generation());
            step(&mut session, &mut producer, 0.0);
            assert_eq!(still_loading(&mut session), 1, "{score} score: said once");
            step(&mut session, &mut producer, 0.5);
            assert_eq!(
                still_loading(&mut session),
                0,
                "{score} score: the next window says nothing new"
            );
        }
    }

    #[cfg(feature = "device-audio")]
    #[test]
    fn loading_prefill_does_not_publish_an_empty_retry() {
        use crate::samples::SoundReadiness;

        // An unrelated pending sample must not prevent a genuine silent save.
        for source in ["~", "heldsample*4"] {
            let (mut session, mut producer, library) = loading_prefill_fixture();

            let before = session.generation();
            let values = rustel_mini::mini(source).expect("native mini values");
            let pattern = rustel_core::controls::ControlSpec::new(["s"]).pattern(&values);
            session
                .set_pattern(pattern)
                .expect("native sound replacement");
            session.restart_transport_at(0.0);
            let after = session.generation();
            producer.arm_replacement(before, after);
            assert_eq!(after, before + 1);
            assert!(!session.active_needs_host(), "fixture must remain native");
            assert_eq!(
                library.readiness("heldsample", 0.0),
                SoundReadiness::Loading
            );
            assert!(library.take_ready().is_empty());
            assert_eq!(session.scheduler.queued(), 0);
            let cursor_before = session.scheduled_to_cycle();
            let mut published = Vec::new();
            let first = producer.step_unwatched_with_clock_and_cutover(
                &mut session,
                || 0.0,
                48_000,
                |generation, frame, _cut| published.push((generation, frame)),
                |_| panic!("rest or still-loading sample emitted audio"),
            );
            // A loading hold is a quiet turn, not a failed one: the held
            // replacement reports Ok with nothing scheduled and nothing
            // published, the same shape as the silent replacement's turn.
            let first = first.expect("held or silent replacement must not fail");
            assert_eq!((first.scheduled, first.pushed, first.pending), (0, 0, 0));
            let first_record = producer.producer_load_snapshot();
            assert!(first_record.query_span_millicycles > 0);
            assert_eq!(first_record.js_callback_calls, 0);
            assert_eq!(first_record.converted_audio_events, 0);
            assert_eq!(first_record.ring_pushes, 0);
            assert_eq!(producer.pending(), 0);
            assert_eq!(session.scheduler.queued(), 0);
            assert!(session.scheduled_to_cycle() > cursor_before);
            assert!(
                session.scheduler.horizon_remaining(0.0)
                    > session.config.horizon - session.scheduler.refill_floor_seconds()
            );

            if source == "~" {
                assert_eq!(first_record.scheduler_events, 0);
                assert_eq!(first_record.refused_voices, 0);
                assert_eq!(published, vec![(after, 0)]);
                continue;
            }

            assert_eq!(first_record.scheduler_events, 2);
            assert_eq!(first_record.refused_voices, 2);
            assert!(
                published.is_empty(),
                "failed conversion published a generation"
            );
            assert_eq!(session.generation(), after, "loading must not roll back");
            assert!(session.audible_source.is_none());

            // No clock advance, loader publication, requery or new score occurs.
            // The scheduler's full horizon must not turn lost onsets into silence.
            let retry = producer.step_unwatched_with_clock_and_cutover(
                &mut session,
                || 0.0,
                48_000,
                |generation, frame, _cut| published.push((generation, frame)),
                |_| panic!("the sample is still loading on retry"),
            );
            assert_eq!(
                library.readiness("heldsample", 0.0),
                SoundReadiness::Loading
            );
            assert!(library.take_ready().is_empty());
            assert_eq!(session.generation(), after);
            let retry = retry.expect("a loading retry is a held turn, not an error");
            assert_eq!((retry.scheduled, retry.pushed, retry.pending), (0, 0, 0));
            assert!(
                published.is_empty(),
                "still-loading full-horizon retry published {published:?}"
            );
        }
    }

    #[cfg(feature = "device-audio")]
    #[test]
    fn loading_prefill_accepts_only_later_query_progress() {
        use crate::samples::SoundReadiness;

        for (source, initial) in [
            ("heldsample*4", false),
            ("heldsample ~", false),
            ("heldsample sine", false),
            ("heldsample ~", true),
        ] {
            let (mut session, mut producer, library) = loading_prefill_fixture();
            let before = session.generation();
            let values = rustel_mini::mini(source).expect("native mini values");
            let pattern = rustel_core::controls::ControlSpec::new(["s"]).pattern(&values);
            session
                .set_pattern(pattern)
                .expect("native sound replacement");
            session.restart_transport_at(0.0);
            let after = session.generation();
            let expected_publications = if initial {
                producer = crate::LiveFileProducer::unwatched(Duration::from_millis(2))
                    .expect("fresh producer");
                Vec::new()
            } else {
                producer.arm_replacement(before, after);
                vec![(after, 0)]
            };
            let mut published = Vec::new();
            let first = producer.step_unwatched_with_clock_and_cutover(
                &mut session,
                || 0.0,
                48_000,
                |generation, frame, _cut| published.push((generation, frame)),
                |_| panic!("loading onset emitted audio"),
            );
            let first = first.expect("loading first window holds quietly");
            assert_eq!((first.scheduled, first.pushed, first.pending), (0, 0, 0));
            assert!(published.is_empty());
            assert!(
                !producer.cut_takeover_deferred_for_loading(),
                "an edit's loading hold has no line cut to withdraw"
            );
            let waiting = producer
                .step_unwatched_with_clock_and_cutover(
                    &mut session,
                    || 0.0,
                    48_000,
                    |generation, frame, _cut| published.push((generation, frame)),
                    |_| panic!("unchanged loading horizon emitted audio"),
                )
                .expect("empty retry waits");
            assert_eq!(
                (waiting.scheduled, waiting.pushed, waiting.pending),
                (0, 0, 0)
            );
            assert!(published.is_empty());
            assert!(!producer.cut_takeover_deferred_for_loading());
            assert!(session.audible_source.is_none());
            let _ = producer.producer_load_snapshot();

            let mut audio = Vec::new();
            let next = producer.step_unwatched_with_clock_and_cutover(
                &mut session,
                || 0.5,
                48_000,
                |generation, frame, _cut| published.push((generation, frame)),
                |event| {
                    audio.push(event);
                    true
                },
            );
            let record = producer.producer_load_snapshot();
            assert_eq!(record.query_span_millicycles, 500);
            assert_eq!(record.js_callback_calls, 0);
            if source != "heldsample*4" {
                let step = next.expect("fresh rest or oscillator window");
                let expected = usize::from(source == "heldsample sine");
                assert_eq!(
                    (step.scheduled, step.pushed, step.pending),
                    (expected, expected, 0)
                );
                assert_eq!(audio.len(), expected);
                assert!(audio.iter().all(|event| event.target_frame == 24_000));
                assert_eq!(published, expected_publications);
                assert!(session.audible_source.is_none());
                assert_eq!(
                    library.readiness("heldsample", 0.0),
                    SoundReadiness::Loading
                );
                continue;
            }

            let held = next.expect("held sample keeps waiting quietly");
            assert_eq!((held.scheduled, held.pushed, held.pending), (0, 0, 0));
            assert!(!producer.cut_takeover_deferred_for_loading());
            assert_eq!((record.scheduler_events, record.refused_voices), (2, 2));
            assert!(audio.is_empty());
            assert!(published.is_empty());
            let sample = library.finish_loading_sample_for_test();
            assert_eq!(library.readiness("heldsample", 0.0), SoundReadiness::Ready);
            assert_eq!(library.take_ready().len(), 1);
            let waiting = producer
                .step_unwatched_with_clock_and_cutover(
                    &mut session,
                    || 0.5,
                    48_000,
                    |generation, frame, _cut| published.push((generation, frame)),
                    |_| panic!("Ready must not replay the drained onsets"),
                )
                .expect("unchanged full horizon waits");
            assert_eq!(
                (waiting.scheduled, waiting.pushed, waiting.pending),
                (0, 0, 0)
            );
            assert!(published.is_empty());
            let _ = producer.producer_load_snapshot();
            let ready = producer
                .step_unwatched_with_clock_and_cutover(
                    &mut session,
                    || 1.0,
                    48_000,
                    |generation, frame, _cut| published.push((generation, frame)),
                    |event| {
                        audio.push(event);
                        true
                    },
                )
                .expect("new sample onsets after readiness");
            assert_eq!((ready.scheduled, ready.pushed, ready.pending), (2, 2, 0));
            assert_eq!(
                audio
                    .iter()
                    .map(|event| event.target_frame)
                    .collect::<Vec<_>>(),
                [48_000, 60_000]
            );
            assert!(
                audio
                    .iter()
                    .all(|event| event.sample.unwrap().sample == sample)
            );
            assert_eq!(published, expected_publications);
            assert!(session.audible_source.is_none());
        }
    }

    /// A REWIND whose first window names a still-loading sample holds the
    /// publication instead of certifying a partial first window.
    ///
    /// A restart is transport-start shaped: every track the score names
    /// should land on its first beat. A window published with
    /// skipped-loading onsets drops them for good (the scheduler cursor has
    /// moved past them), and their tracks enter late, alone. So the cut
    /// takeover's first prefill waits for load progress the way an empty
    /// loading window does; once the sample decodes, the whole window is
    /// scheduled, published and pushed in order. An ordinary edit keeps the
    /// skip-and-log contract and is not affected.
    #[test]
    #[cfg(feature = "device-audio")]
    fn a_rewind_first_window_holds_publication_while_a_sample_is_loading() {
        use crate::samples::SoundReadiness;

        let (mut session, mut producer, library) = loading_prefill_fixture();
        // The audible score the rewind replaces: silent (renderable), so
        // the contract under test is the rewind's, not the old voice's.
        let before = session.generation();
        let old = rustel_core::controls::ControlSpec::new(["s"])
            .pattern(&rustel_mini::mini("~").expect("silent old score"));
        session.set_pattern(old).expect("old silent score");
        session.restart_transport_at(0.0);
        let mid = session.generation();
        producer.arm_replacement(before, mid);
        let mut published: Vec<(u64, u64, TakeoverCut)> = Vec::new();
        producer
            .step_unwatched_with_clock_and_cutover(
                &mut session,
                || 0.0,
                48_000,
                |generation, frame, cut| published.push((generation, frame, cut)),
                |_| panic!("silence emitted audio"),
            )
            .expect("silent old score publishes");
        assert_eq!(published, vec![(mid, 0, TakeoverCut::None)]);
        // Inject the replay-policy target the ordinary rollback path needs
        // if the held window ever refused for real: this test exercises the
        // hold-and-ready route, not the rollback.
        session
            .mark_audible_generation(mid)
            .expect("injected replay-policy target");

        // Install the loading replacement as the TUI path does: set the
        // takeover override and the from-zero flag, then call `reload_at`,
        // which arms the (takeover, cut) pair. A raw `set_pattern` skips that.
        let rewinding = session.generation();
        session.set_next_takeover_time(0.25);
        session.start_next_from_zero();
        session
            .reload_at("s(\"heldsample\")", false, 0.0)
            .expect("native sound replacement");
        let after = session.generation();
        assert_eq!(after, rewinding + 1);
        producer.arm_replacement(rewinding, after);

        // First window: the sample is still loading, but the window is not
        // empty. The query ran and some onsets were refused as loading.
        assert_eq!(
            library.readiness("heldsample", 0.0),
            SoundReadiness::Loading
        );
        let first = producer
            .step_unwatched_with_clock_and_cutover(
                &mut session,
                || 0.0,
                48_000,
                |generation, frame, cut| published.push((generation, frame, cut)),
                |_| panic!("loading onset emitted audio"),
            )
            .expect("a loading first window is a held turn, not a failure");
        assert_eq!((first.scheduled, first.pushed), (0, 0));
        assert!(
            !published
                .iter()
                .any(|(generation, _, _)| *generation == after),
            "the partial loading window must not publish the rewind: {published:?}"
        );
        assert!(
            producer.cut_takeover_deferred_for_loading(),
            "the engine is told to withdraw the line cut while the flip waits"
        );

        // The sample decodes; the whole first window lands at once.
        let sample = library.finish_loading_sample_for_test();
        assert_eq!(library.readiness("heldsample", 0.0), SoundReadiness::Ready);
        let mut audio = Vec::new();
        let ready = producer.step_unwatched_with_clock_and_cutover(
            &mut session,
            || 0.0,
            48_000,
            |generation, frame, cut| published.push((generation, frame, cut)),
            |event| {
                audio.push(event);
                true
            },
        );
        let step = ready.expect("the whole rewind window publishes once ready");
        assert_eq!(
            published
                .iter()
                .filter(|(generation, _, _)| *generation == after)
                .collect::<Vec<_>>(),
            [&(after, 12_000, TakeoverCut::AtTakeover)],
            "exactly one publication, on the line, with the line's cut: {published:?}"
        );
        assert!(step.scheduled > 0 && step.pushed == step.scheduled);
        assert_eq!(
            audio.iter().map(|event| event.target_frame).min(),
            Some(12_000),
            "cycle zero's onset comes back, on the line"
        );
        assert!(
            audio.iter().all(|event| event
                .sample
                .is_some_and(|controls| controls.sample == sample)),
            "every first-beat onset carries the sample"
        );
        assert!(
            !producer.cut_takeover_deferred_for_loading(),
            "publication lowers the deferral: the next launch's line cut must stand"
        );
        assert_eq!(
            session.take_requery_takeover(),
            None,
            "the publication consumed the pair; nothing is left for the next reload"
        );
    }

    /// A rewind's held window that has nothing to reopen keeps its takeover
    /// and cut. Here the scheduler has not queried up to cycle zero, so the
    /// cursor stands short of the anchor.
    #[test]
    #[cfg(feature = "device-audio")]
    fn a_held_rewind_that_cannot_reopen_keeps_its_takeover_and_cut() {
        let (mut session, mut producer, _library) = rewind_hold_fixture();
        let rewinding = session.generation();
        session.set_next_takeover_time(0.25);
        session.start_next_from_zero();
        session
            .reload_at("s(\"heldsample\")", false, 0.0)
            .expect("native sound replacement");
        let after = session.generation();
        producer.arm_replacement(rewinding, after);
        // Queue the cycle-zero onset ahead of the producer, then name an
        // anchor past what that fill queried.
        session.scheduler.set_horizon(1.0);
        assert_eq!(
            session.scheduler.tick(&VirtualClock::new(0.0)),
            rustel_scheduler::TickStatus::Filled
        );
        session.scheduler.set_horizon(0.5);
        assert_eq!(session.scheduler.queued(), 1);
        session.requery_anchor_time = Some(5.0);

        let held = producer
            .step_unwatched_with_clock_and_cutover(
                &mut session,
                || 0.0,
                48_000,
                |_, _, _| panic!("a held rewind published"),
                |_| panic!("a loading onset emitted audio"),
            )
            .expect("the loading window holds quietly");
        assert_eq!((held.scheduled, held.pushed), (0, 0));
        assert!(producer.cut_takeover_deferred_for_loading());
        assert_eq!(
            session.take_requery_takeover(),
            Some((0.25, TakeoverCut::AtTakeover)),
            "the pair is back for this rewind's publication"
        );
    }

    /// The gap skip must not move an unpublished rewind's cursor off cycle
    /// zero. The producer slides a stale cycle zero to its clock before it
    /// queries, so it never presents such a gap; a direct schedule of the
    /// same state, long after the line, must still query the restart from
    /// its first beat rather than resume "at the present" bars in.
    #[test]
    #[cfg(feature = "device-audio")]
    fn the_gap_skip_leaves_an_unpublished_rewind_on_cycle_zero() {
        let (mut session, _producer, _library) = rewind_hold_fixture();
        session.set_next_takeover_time(0.25);
        session.start_next_from_zero();
        session
            .reload_at("s(\"sine\")", false, 0.0)
            .expect("native synth replacement");
        let batch = match session.schedule_audio_live_at(
            2.0,
            48_000,
            Duration::from_millis(2),
            LiveQueryBudgetMode::ReplacementPrefill,
        ) {
            Ok(batch) => batch,
            Err(_) => panic!("the synth window schedules"),
        };
        assert_eq!(
            batch.events.first().map(|event| event.target_frame),
            Some(12_000),
            "the restart is queried from its first beat"
        );
        assert_eq!(session.take_pending_producer_turn().gap_resync_count, 0);
        assert!(
            session
                .take_diagnostics()
                .iter()
                .all(|diagnostic| diagnostic.kind != "live-recovered"),
            "an unpublished restart is not a producer that fell behind"
        );
    }

    /// The loading fixture with a silent audible score published, ready for
    /// a rewind to replace it: `(session, producer, library)`.
    #[cfg(feature = "device-audio")]
    fn rewind_hold_fixture() -> (
        Session,
        crate::LiveFileProducer,
        Arc<crate::samples::SampleLibrary>,
    ) {
        let (mut session, mut producer, library) = loading_prefill_fixture();
        let before = session.generation();
        let old = rustel_core::controls::ControlSpec::new(["s"])
            .pattern(&rustel_mini::mini("~").expect("silent old score"));
        session.set_pattern(old).expect("old silent score");
        session.restart_transport_at(0.0);
        let mid = session.generation();
        producer.arm_replacement(before, mid);
        producer
            .step_unwatched_with_clock_and_cutover(
                &mut session,
                || 0.0,
                48_000,
                |_, _, _| {},
                |_| panic!("silence emitted audio"),
            )
            .expect("silent old score publishes");
        session
            .mark_audible_generation(mid)
            .expect("injected replay-policy target");
        (session, producer, library)
    }

    /// A rewind's first window where one track's sample is still loading and
    /// another's sound is ready lands whole once the sample decodes. The hold
    /// reopens the window, so the ready onset is not lost.
    #[test]
    #[cfg(feature = "device-audio")]
    fn a_partially_loading_rewind_window_lands_whole() {
        let (mut session, mut producer, library) = rewind_hold_fixture();
        let rewinding = session.generation();
        session.set_next_takeover_time(0.25);
        session.start_next_from_zero();
        session
            .reload_at("s(\"[heldsample, sine]\")", false, 0.0)
            .expect("native sound replacement");
        let after = session.generation();
        producer.arm_replacement(rewinding, after);

        let mut published: Vec<(u64, u64, TakeoverCut)> = Vec::new();
        for turn in 0..3 {
            let held = producer
                .step_unwatched_with_clock_and_cutover(
                    &mut session,
                    || 0.0,
                    48_000,
                    |generation, frame, cut| published.push((generation, frame, cut)),
                    |_| panic!("a held rewind window emitted audio"),
                )
                .expect("the partial window holds quietly");
            assert_eq!((held.scheduled, held.pushed), (0, 0), "turn {turn}");
            assert!(
                published
                    .iter()
                    .all(|(generation, _, _)| *generation != after),
                "turn {turn}: the rewind must not publish while a track loads: {published:?}"
            );
            assert!(producer.cut_takeover_deferred_for_loading());
        }

        library.finish_loading_sample_for_test();
        let mut audio = Vec::new();
        producer
            .step_unwatched_with_clock_and_cutover(
                &mut session,
                || 0.0,
                48_000,
                |generation, frame, cut| published.push((generation, frame, cut)),
                |event| {
                    audio.push(event);
                    true
                },
            )
            .expect("the whole first window publishes once ready");
        let rewind: Vec<_> = published
            .iter()
            .filter(|(generation, _, _)| *generation == after)
            .collect();
        assert_eq!(
            rewind,
            [&(after, 12_000, TakeoverCut::AtTakeover)],
            "one publication, on the line, with the line's cut"
        );
        let downbeat: Vec<bool> = audio
            .iter()
            .filter(|event| event.target_frame == 12_000)
            .map(|event| event.sample.is_some())
            .collect();
        assert!(
            downbeat.contains(&true) && downbeat.contains(&false),
            "both the sample track and the ready synth sound on cycle zero: {downbeat:?}"
        );
    }

    /// A rewind held for loading past its own takeover and past the schedule
    /// cover restarts from cycle zero when it can sound.
    #[test]
    #[cfg(feature = "device-audio")]
    fn a_rewind_held_past_its_takeover_restarts_where_it_can_sound() {
        let (mut session, mut producer, library) = rewind_hold_fixture();
        let rewinding = session.generation();
        session.set_next_takeover_time(0.25);
        session.start_next_from_zero();
        session
            .reload_at("s(\"heldsample\")", false, 0.0)
            .expect("native sound replacement");
        let after = session.generation();
        producer.arm_replacement(rewinding, after);
        let _ = producer.producer_load_snapshot();

        let mut published: Vec<(u64, u64, TakeoverCut)> = Vec::new();
        // Mid-countdown, then past the line, then past line + cover.
        for clock in [0.0, 0.8, 1.5] {
            let held = producer
                .step_unwatched_with_clock_and_cutover(
                    &mut session,
                    || clock,
                    48_000,
                    |generation, frame, cut| published.push((generation, frame, cut)),
                    |_| panic!("a held rewind emitted audio"),
                )
                .expect("the rewind holds quietly");
            assert_eq!((held.scheduled, held.pushed), (0, 0), "at {clock}");
            assert!(
                published
                    .iter()
                    .all(|(generation, _, _)| *generation != after),
                "at {clock}: no publication while the sample loads: {published:?}"
            );
        }
        assert_eq!(
            producer.producer_load_snapshot().gap_resync_count,
            0,
            "a held rewind is not a producer that fell behind"
        );

        library.finish_loading_sample_for_test();
        let mut audio = Vec::new();
        producer
            .step_unwatched_with_clock_and_cutover(
                &mut session,
                || 1.6,
                48_000,
                |generation, frame, cut| published.push((generation, frame, cut)),
                |event| {
                    audio.push(event);
                    true
                },
            )
            .expect("the rewind publishes once ready");
        let rewind: Vec<_> = published
            .iter()
            .filter(|(generation, _, _)| *generation == after)
            .collect();
        assert_eq!(
            rewind,
            [&(after, 76_800, TakeoverCut::AtFlip)],
            "cycle zero lands where the rewind can sound, and the old score is cut there"
        );
        let first = audio.iter().map(|event| event.target_frame).min();
        assert_eq!(
            first,
            Some(76_800),
            "the first onset is cycle zero, not a catch-up"
        );
        assert!(
            audio.iter().all(|event| event.target_frame >= 76_800),
            "no past-due burst of the onsets the decode missed"
        );
    }

    /// A rewind held for a loading sample parks instead of re-querying its
    /// first window on every 2 ms engine turn. Each retry reopened and
    /// re-converted the whole window, for as long as the decode took: a core
    /// busy re-refusing the same onsets beside the decoder it waited on.
    /// Parked, it retries once per safety interval of the producer's clock,
    /// and at once when the library settles the sample.
    #[test]
    #[cfg(feature = "device-audio")]
    fn a_rewind_held_for_loading_waits_for_the_decode_instead_of_requerying() {
        let (mut session, mut producer, library) = rewind_hold_fixture();
        let rewinding = session.generation();
        session.start_next_from_zero();
        session
            .reload_at("s(\"heldsample*4\")", false, 0.0)
            .expect("native sound replacement");
        let after = session.generation();
        producer.arm_replacement(rewinding, after);
        let refusals_before = producer.producer_load_snapshot().atomic_refusals;

        let mut published: Vec<(u64, u64, TakeoverCut)> = Vec::new();
        let mut clock = 0.0;
        for turn in 0..100 {
            clock = f64::from(turn) * 0.002;
            let held = producer
                .step_unwatched_with_clock_and_cutover(
                    &mut session,
                    || clock,
                    48_000,
                    |generation, frame, cut| published.push((generation, frame, cut)),
                    |_| panic!("a held rewind emitted audio"),
                )
                .expect("the rewind holds quietly");
            assert_eq!((held.scheduled, held.pushed), (0, 0), "turn {turn}");
            assert!(producer.cut_takeover_deferred_for_loading(), "turn {turn}");
        }
        assert!(published.is_empty(), "{published:?}");
        // One query at the start, then one per 50 ms of producer clock over
        // the ~200 ms the hundred turns span.
        let queries = producer.producer_load_snapshot().atomic_refusals - refusals_before;
        assert!(
            (2..=5).contains(&queries),
            "a held rewind queried {queries} times in 100 turns"
        );

        // The decode lands on the very next turn, well inside the interval:
        // the settled epoch wakes the hold.
        library.finish_loading_sample_for_test();
        let mut audio = Vec::new();
        producer
            .step_unwatched_with_clock_and_cutover(
                &mut session,
                || clock,
                48_000,
                |generation, frame, cut| published.push((generation, frame, cut)),
                |event| {
                    audio.push(event);
                    true
                },
            )
            .expect("the rewind publishes once its sample is ready");
        let frame = (clock * 48_000.0).round() as u64;
        assert_eq!(published, [(after, frame, TakeoverCut::AtFlip)]);
        assert_eq!(
            audio.iter().map(|event| event.target_frame).min(),
            Some(frame),
            "the restart begins on its first beat"
        );
    }

    /// A parked rewind held on a loading sample, ready for a test to take
    /// turns against: the replacement is armed and its first turn has
    /// queried, refused for loading, and parked.
    #[cfg(feature = "device-audio")]
    fn parked_rewind_fixture() -> (
        Session,
        crate::LiveFileProducer,
        Arc<crate::samples::SampleLibrary>,
    ) {
        let (mut session, mut producer, library) = rewind_hold_fixture();
        let rewinding = session.generation();
        session.start_next_from_zero();
        session
            .reload_at("s(\"heldsample*4\")", false, 0.0)
            .expect("native sound replacement");
        let after = session.generation();
        producer.arm_replacement(rewinding, after);
        let refusals_before = producer.producer_load_snapshot().atomic_refusals;
        producer
            .step_unwatched_with_clock_and_cutover(
                &mut session,
                || 0.0,
                48_000,
                |_, _, _| panic!("a held rewind published"),
                |_| panic!("a held rewind emitted audio"),
            )
            .expect("the rewind holds quietly");
        assert!(producer.cut_takeover_deferred_for_loading());
        assert_eq!(
            producer.producer_load_snapshot().atomic_refusals - refusals_before,
            1,
            "the first turn queried and held"
        );
        (session, producer, library)
    }

    /// The park's safety retry must not wait on the producer's clock alone.
    /// In the studio that clock is the device's count of completed frames,
    /// and a stalled, paused or recycling device stops it: a rewind parked
    /// there never retried at all, and only a settle the epoch could see
    /// would wake it. Turns count too.
    #[test]
    #[cfg(feature = "device-audio")]
    fn a_parked_rewind_retries_while_the_device_clock_stands_still() {
        let (mut session, mut producer, _library) = parked_rewind_fixture();
        let refusals_before = producer.producer_load_snapshot().atomic_refusals;
        for turn in 0..100 {
            let held = producer
                .step_unwatched_with_clock_and_cutover(
                    &mut session,
                    || 0.0,
                    48_000,
                    |_, _, _| panic!("a held rewind published"),
                    |_| panic!("a held rewind emitted audio"),
                )
                .expect("the rewind holds quietly");
            assert_eq!((held.scheduled, held.pushed), (0, 0), "turn {turn}");
        }
        let queries = producer.producer_load_snapshot().atomic_refusals - refusals_before;
        assert!(
            (3..=5).contains(&queries),
            "a hundred turns on a clock that never moves queried {queries} times"
        );
    }

    /// Every sample library's settled epoch starts at zero, so a library
    /// swapped in under a parked rewind can carry the very epoch the park
    /// waits on. The park belongs to the library it read, and a new one
    /// wakes it on the next turn.
    #[test]
    #[cfg(feature = "device-audio")]
    fn a_parked_rewind_wakes_when_the_sample_library_changes() {
        let (mut session, mut producer, library) = parked_rewind_fixture();
        let replacement = Arc::new(crate::samples::SampleLibrary::with_loading_sample_for_test(
            "heldsample",
        ));
        assert_eq!(
            replacement.settled_epoch(),
            library.settled_epoch(),
            "the new library carries the parked epoch"
        );
        session.set_sample_library_for_test(Arc::clone(&replacement));
        let refusals_before = producer.producer_load_snapshot().atomic_refusals;
        producer
            .step_unwatched_with_clock_and_cutover(
                &mut session,
                || 0.002,
                48_000,
                |_, _, _| panic!("the new library's sample is loading too"),
                |_| panic!("a held rewind emitted audio"),
            )
            .expect("the rewind holds quietly");
        assert_eq!(
            producer.producer_load_snapshot().atomic_refusals - refusals_before,
            1,
            "the turn after the swap asks the new library"
        );
    }

    /// A stop that lands during a parked turn reaches the producer on that
    /// turn, as it does on the query path, rather than waiting out the park.
    #[test]
    #[cfg(feature = "device-audio")]
    fn a_parked_rewind_lets_a_stop_through() {
        let (mut session, mut producer, _library) = parked_rewind_fixture();
        let transport = session.transport();
        let result = producer.step_unwatched_with_clock_and_cutover(
            &mut session,
            || {
                // The stop lands after the turn's own check at its start,
                // while it reads the clock.
                transport.stop();
                0.002
            },
            48_000,
            |_, _, _| panic!("a stopped rewind published"),
            |_| panic!("a stopped rewind emitted audio"),
        );
        assert!(
            matches!(result, Err(RuntimeError::Cancelled)),
            "the stop is answered on this turn: {:?}",
            result.map(|step| step.watch)
        );
    }

    /// An immediate rewind's cycle zero is anchored at the install clock,
    /// and the producer always publishes it later. Left there, the first
    /// window was aimed into rendered frames (attack lost). The producer
    /// slides cycle zero to the instant it publishes.
    #[test]
    #[cfg(feature = "device-audio")]
    fn an_immediate_rewind_restarts_where_the_producer_publishes_it() {
        let (mut session, mut producer, _library) = rewind_hold_fixture();
        let rewinding = session.generation();
        session.start_next_from_zero();
        session
            .reload_at("s(\"sine\")", false, 0.0)
            .expect("native sound replacement");
        let after = session.generation();
        producer.arm_replacement(rewinding, after);

        let mut published: Vec<(u64, u64, TakeoverCut)> = Vec::new();
        let mut audio = Vec::new();
        producer
            .step_unwatched_with_clock_and_cutover(
                &mut session,
                || 0.02,
                48_000,
                |generation, frame, cut| published.push((generation, frame, cut)),
                |event| {
                    audio.push(event);
                    true
                },
            )
            .expect("the rewind publishes");
        assert_eq!(published, [(after, 960, TakeoverCut::AtFlip)]);
        assert_eq!(
            audio.iter().map(|event| event.target_frame).min(),
            Some(960),
            "cycle zero is where the producer published, not 20 ms behind it"
        );
    }

    /// A rewind whose first window refuses an unknown sound while the look-
    /// ahead finds a sample still loading is held, reopened and asked again
    /// on every retry. Its refusals are said once per install, and the
    /// sample it waits for is said once, as awaited rather than skipped:
    /// before, the unknown sound was logged on every retry and nothing named
    /// the load.
    #[test]
    #[cfg(feature = "device-audio")]
    fn a_held_rewind_says_its_refusals_and_its_wait_once() {
        let (mut session, mut producer, _library) = rewind_hold_fixture();
        let rewinding = session.generation();
        session.set_next_takeover_time(0.25);
        session.start_next_from_zero();
        session
            .reload_at("s(\"<unknown-test-sample heldsample>\")", false, 0.0)
            .expect("native sound replacement");
        let after = session.generation();
        producer.arm_replacement(rewinding, after);
        let _ = session.take_diagnostics();

        let mut said = Vec::new();
        // Each retry is past the parked hold's interval, and all are before
        // the line, so every turn re-asks the reopened first window.
        for clock in [0.0, 0.06, 0.12, 0.18] {
            let held = producer
                .step_unwatched_with_clock_and_cutover(
                    &mut session,
                    || clock,
                    48_000,
                    |_, _, _| panic!("a held rewind published"),
                    |_| panic!("a held rewind emitted audio"),
                )
                .expect("the rewind holds quietly");
            assert_eq!((held.scheduled, held.pushed), (0, 0), "at {clock}");
            assert!(
                producer.cut_takeover_deferred_for_loading(),
                "at {clock}: the rewind is held for the sample ahead"
            );
            said.extend(
                session
                    .take_diagnostics()
                    .into_iter()
                    .filter(|diagnostic| {
                        diagnostic.kind == "voice-refused"
                            || diagnostic.kind == SAMPLE_LOADING_DIAGNOSTIC
                            || diagnostic.kind == SAMPLE_AWAITED_DIAGNOSTIC
                    })
                    .map(|diagnostic| (diagnostic.kind, diagnostic.message)),
            );
        }
        // Told apart by kind, not by reading the sentence back: a sound this
        // engine cannot play and a sound that has not arrived yet are
        // different news, and only one of them is the score's fault.
        let unknown = said
            .iter()
            .filter(|(kind, message)| {
                kind == "voice-refused" && message.contains("unknown-test-sample")
            })
            .count();
        let loading = said
            .iter()
            .filter(|(kind, _)| kind == SAMPLE_AWAITED_DIAGNOSTIC)
            .count();
        assert_eq!(
            unknown, 1,
            "the unknown sound is said once, not per retry: {said:?}"
        );
        assert_eq!(loading, 1, "the sample it waits for is said once: {said:?}");
        assert!(
            !said
                .iter()
                .any(|(kind, _)| kind == SAMPLE_LOADING_DIAGNOSTIC),
            "nothing is said skipped: {said:?}"
        );
    }

    #[cfg(feature = "device-audio")]
    fn check_loading_prefill_queued_progress(external: bool) {
        let (mut session, mut producer, library) = loading_prefill_fixture();
        let before = session.generation();
        let pattern = rustel_core::fastcat(
            (0..4)
                .map(|index| {
                    let mut controls = vec![("s".into(), Value::Str("heldsample".into()))];
                    if external && index == 3 {
                        controls.push(("note".into(), Value::F64(60.0)));
                        controls.push(("midiport".into(), Value::Str("test-port".into())));
                    }
                    rustel_core::pure(Value::object(controls))
                })
                .collect(),
        );
        session.set_pattern(pattern).expect("native queued pattern");
        session.restart_transport_at(0.0);
        let after = session.generation();
        producer.arm_replacement(before, after);
        session.scheduler.set_horizon(1.0);
        assert_eq!(
            session.scheduler.tick(&VirtualClock::new(0.0)),
            rustel_scheduler::TickStatus::Filled
        );
        session.scheduler.set_horizon(0.5);
        assert_eq!(session.scheduler.queued(), 4);
        let first = producer.step_unwatched_with_clock_and_cutover(
            &mut session,
            || 0.0,
            48_000,
            |_, _, _| panic!("loading first transfer must not publish"),
            |_| panic!("loading first transfer emitted audio"),
        );
        let first = first.expect("loading hold is a quiet turn");
        assert_eq!((first.scheduled, first.pushed, first.pending), (0, 0, 0));
        assert!(
            !producer.cut_takeover_deferred_for_loading(),
            "an edit's loading hold has no line cut to withdraw"
        );
        assert_eq!(session.scheduler.queued(), 1);
        assert!(!session.has_pending_external_output());
        let first_record = producer.producer_load_snapshot();
        assert_eq!(first_record.query_span_millicycles, 0);
        assert_eq!(first_record.scheduler_events, 0);
        assert_eq!(first_record.refused_voices, 3);
        if !external {
            library.finish_loading_sample_for_test();
            assert_eq!(library.take_ready().len(), 1);
        }
        let mut published = Vec::new();
        let mut audio = Vec::new();
        let step = producer
            .step_unwatched_with_clock_and_cutover(
                &mut session,
                || 0.3,
                48_000,
                |generation, frame, _cut| published.push((generation, frame)),
                |event| {
                    audio.push(event);
                    true
                },
            )
            .expect("queued output completes prefill without another query");
        assert!(!producer.cut_takeover_deferred_for_loading());
        let record = producer.producer_load_snapshot();
        assert_eq!(record.query_span_millicycles, 0);
        assert_eq!(record.scheduler_events, 0);
        let expected = usize::from(!external);
        assert_eq!(record.converted_audio_events, expected as u64);
        assert_eq!(record.refused_voices, u64::from(external));
        assert_eq!(
            (step.scheduled, step.pushed, step.pending),
            (expected, expected, 0)
        );
        assert_eq!(audio.len(), expected);
        assert!(audio.iter().all(|event| event.target_frame == 36_000));
        assert_eq!(published.len(), 1);
        assert_eq!(published[0].0, after);
        assert_eq!(session.scheduler.queued(), 0);
        #[cfg(feature = "midi")]
        if external {
            let intents = session.take_pending_midi();
            assert_eq!(intents.len(), 1);
            assert_eq!(intents[0].target_time, 0.75);
            assert_eq!(intents[0].generation, after);
            assert!(!rustel_midi::plan(&intents[0].controls, intents[0].duration_secs).is_empty());
        }
    }

    #[cfg(feature = "device-audio")]
    #[test]
    fn loading_prefill_preserves_queued_audio_without_a_query() {
        check_loading_prefill_queued_progress(false);
    }

    #[cfg(all(feature = "device-audio", feature = "midi"))]
    #[test]
    fn loading_prefill_preserves_queued_midi_without_a_query() {
        check_loading_prefill_queued_progress(true);
    }

    #[cfg(feature = "device-audio")]
    #[test]
    fn rollback_reports_when_no_target_exists() {
        let mut session = Session::new().expect("session");
        assert_eq!(
            session
                .rollback_to_previous_source(0.0)
                .expect("absence is not an evaluation error"),
            RollbackAttempt::Unavailable
        );
    }

    #[cfg(feature = "device-audio")]
    #[test]
    fn captured_rollback_source_survives_later_sources_and_settings() {
        use rustel_core::compose::Alignment;
        use rustel_core::rng::RngMode;

        let mut session = Session::new().expect("session");
        session.set_default_voicings(Some("lefthand"));
        session.evaluate_mini("bd").expect("source A");
        // Inject replay policy, not consumer-copy evidence.
        session
            .mark_audible_generation(session.generation())
            .expect("injected replay-policy target");
        let generation_a = session.generation();
        session.set_rng_mode(RngMode::Precise);
        session.set_default_join(Alignment::Out);
        session.set_default_voicings(Some("guidetones"));
        let generation_b = session.reload_at("sd bd", true, 0.1).expect("source B");
        let captured = session
            .capture_rollback_source(generation_b)
            .expect("capture B");
        assert!(captured.is_some());
        assert_eq!(
            session_core_settings(&session),
            (RngMode::Precise, Alignment::Out, 2)
        );
        assert_eq!(
            session.audible_source.as_ref().unwrap().generation,
            generation_a
        );

        session.set_rng_mode(RngMode::Legacy);
        session.set_default_join(Alignment::Mix);
        session.set_default_voicings(Some("lefthand"));
        session.reload_at("hh", true, 0.2).expect("source C");
        let generation_d = session.reload_at("cp", true, 0.3).expect("source D");
        assert_eq!(generation_d, generation_b + 2);
        session.install_rollback_source(captured);

        let retained = session.audible_source.as_ref().expect("retained B");
        assert_eq!(retained.generation, generation_b);
        assert_eq!(retained.source.as_ref(), "sd bd");
        assert!(retained.mini);
        assert_eq!(session.generation(), generation_d);
        assert_eq!(session.last_source.as_deref(), Some("cp"));
        assert_eq!(
            session_core_settings(&session),
            (RngMode::Legacy, Alignment::Mix, 4)
        );
        assert_eq!(
            session.rollback_to_previous_source(0.4).expect("replay B"),
            RollbackAttempt::Applied
        );
        assert_eq!(active_values(&session), ["sd", "bd"]);
        assert_eq!(
            session_core_settings(&session),
            (RngMode::Precise, Alignment::Out, 2)
        );
    }

    #[cfg(feature = "device-audio")]
    fn check_unconfirmed_prefill_rollback(
        source_a: &str,
        source_b: &str,
        mini: bool,
        delayed_b: bool,
    ) {
        use rustel_audio::device::ManualLiveOutput;

        let mut session = Session::with_config(SessionConfig {
            cps: 1.0,
            horizon: 0.5,
            sample_rate: 48_000,
            ..SessionConfig::default()
        })
        .expect("session");
        session.set_direct_diagnostic_logging(false);
        session.set_schedule_lead(0.0);
        session.set_continuity_margin(0.0);
        if mini {
            session.evaluate_mini(source_a).expect("source A");
        } else {
            session.evaluate(source_a).expect("source A");
        }
        let mut output = ManualLiveOutput::new(48_000, session.generation()).expect("output");
        session
            .bind_audio_confirmations(output.device().confirmations())
            .expect("bind output confirmations");
        let mut producer =
            crate::LiveFileProducer::unwatched(Duration::from_millis(2)).expect("producer");
        let pump = |session: &mut Session,
                    producer: &mut crate::LiveFileProducer,
                    output: &ManualLiveOutput| {
            let device = output.device();
            producer.step_unwatched_with_clock_and_cutover(
                session,
                || device.clock_seconds(),
                48_000,
                |generation, takeover, cut| device.set_generation(generation, takeover, cut),
                |event| device.push(event),
            )
        };

        // A reaches the same DSP and host-copy body used by live devices.
        // No marker or manually advanced frame counter stands in for it.
        let mut pcm = [0.0_f32; 256];
        let mut nonzero = false;
        for _ in 0..512 {
            pump(&mut session, &mut producer, &output).expect("A continuation");
            output.render(&mut pcm);
            assert!(pcm.iter().all(|sample| sample.is_finite()));
            nonzero |= pcm.iter().any(|sample| *sample != 0.0);
        }
        let copied_a = output.device().report();
        assert!(copied_a.submitted_frames > 0 && copied_a.callbacks > 0);
        assert_eq!(nonzero, !mini, "audible and valid-silence fixture controls");
        assert_eq!(copied_a.accepted_events > 0, !mini);

        let transport = session.transport();
        let now = output.device().clock_seconds();
        producer
            .shield_reload_with_clock(
                &mut session,
                || now,
                48_000,
                |event| output.device().push(event),
            )
            .expect("A shield");
        let generation_a = session.generation();
        let generation_b = session
            .reload_with_clock_cancellable(source_b, mini, transport.stopped_flag(), || now)
            .expect("B evaluates");
        producer.arm_replacement(generation_a, generation_b);
        let prefill = pump(&mut session, &mut producer, &output).expect("B prefill");
        assert_eq!(output.device().generation(), generation_b);
        assert_eq!(prefill.pushed > 0, !mini);
        assert_eq!(
            output.device().report().submitted_frames,
            copied_a.submitted_frames
        );

        // The host is deliberately not invoked for B. C is valid native mini
        // syntax, but its bare string haps cannot become scalar voice objects.
        let generation_c = session
            .reload_with_clock_cancellable("c4*64", true, transport.stopped_flag(), || now)
            .expect("C installs before conversion");
        assert!(generation_c > generation_b);
        producer.arm_replacement(generation_b, generation_c);
        let output = std::cell::RefCell::new(output);
        let mut copied_b = false;
        let refusal = producer
            .step_unwatched_with_clock_and_cutover(
                &mut session,
                || {
                    // This clock is sampled AFTER turn-entry receipt draining,
                    // with Session already at C. B completes while that failing
                    // turn is in progress; rollback itself must drain the result.
                    if delayed_b && !copied_b {
                        for _ in 0..512 {
                            output.borrow_mut().render(&mut pcm);
                            assert!(pcm.iter().all(|sample| sample.is_finite()));
                        }
                        copied_b = true;
                    }
                    output.borrow().device().clock_seconds()
                },
                48_000,
                |generation, takeover, cut| {
                    output
                        .borrow()
                        .device()
                        .set_generation(generation, takeover, cut)
                },
                |event| output.borrow().device().push(event),
            )
            .expect_err("C refuses its nonempty first conversion window")
            .to_string();
        assert!(
            refusal.contains("scalar audio refused every onset"),
            "{refusal}"
        );
        assert!(
            refusal.contains("expected hap.value to be an object"),
            "{refusal}"
        );
        assert_eq!(
            output.borrow().device().report().submitted_frames > copied_a.submitted_frames,
            delayed_b
        );
        assert_eq!(
            session.active_source(),
            Some(if delayed_b { source_b } else { source_a }),
            "only a copied window may select its retained rollback source"
        );
    }

    #[cfg(feature = "device-audio")]
    #[test]
    fn unconfirmed_audible_prefill_does_not_replace_the_rollback_source() {
        // Distinct compatibility fixtures exercise retained score ownership
        // through the same evaluator used for live source replacements.
        let source_a =
            include_str!("../tests/e2e/scores/corpus/regressions/begingate-pulse3.strudel");
        let source_b =
            include_str!("../tests/e2e/scores/corpus/regressions/begingate-pulse4.strudel");
        assert_eq!(
            crate::ui_events::source_revision(source_a),
            "87e752905cf56ee74faa82520fc8ec32de83053ad10d417c325224f2a4f669d6"
        );
        assert_eq!(
            crate::ui_events::source_revision(source_b),
            "db6313ce130abc31c00b3a8b93244a7bddf7a9bbee09a255a392feb5b7d50080"
        );
        check_unconfirmed_prefill_rollback(source_a, source_b, false, false);
    }

    #[cfg(feature = "device-audio")]
    #[test]
    fn unconfirmed_silent_prefill_does_not_replace_the_rollback_source() {
        check_unconfirmed_prefill_rollback("~", "~ ~", true, false);
    }

    #[cfg(feature = "device-audio")]
    #[test]
    fn delayed_audible_confirmation_selects_its_own_rollback_source() {
        check_unconfirmed_prefill_rollback(
            include_str!("../tests/e2e/scores/corpus/regressions/begingate-pulse3.strudel"),
            include_str!("../tests/e2e/scores/corpus/regressions/begingate-pulse4.strudel"),
            false,
            true,
        );
    }

    #[cfg(feature = "device-audio")]
    #[test]
    fn delayed_silent_confirmation_selects_its_own_rollback_source() {
        check_unconfirmed_prefill_rollback("~", "~ ~", true, true);
    }

    #[cfg(feature = "device-audio")]
    #[test]
    fn captured_direct_pattern_clears_a_later_textual_rollback_target() {
        let mut session = Session::new().expect("session");
        #[cfg(not(feature = "vst"))]
        session.evaluate_mini("bd").expect("source A");
        #[cfg(feature = "vst")]
        {
            let source = r#"s("sine").vst("Delay").orbit(1)"#;
            session.evaluate(source).expect("source A");
            session
                .reload_at(&source.replace("orbit(1)", "orbit(2)"), false, 0.1)
                .expect("move plugin");
            assert_eq!(session.insert_orbits[2], 1);
        }
        // Inject replay policy, not consumer-copy evidence.
        session
            .mark_audible_generation(session.generation())
            .expect("injected replay-policy target");
        let generation_a = session.generation();
        session
            .set_pattern(rustel_core::pure(Value::Str("native".into())))
            .expect("direct Pattern B");
        #[cfg(feature = "vst")]
        assert_eq!(session.insert_orbits, crate::vst::INSERT_ORBITS);
        let captured = session
            .capture_rollback_source(session.generation())
            .expect("capture direct Pattern");
        assert!(captured.is_none());
        assert_eq!(
            session.audible_source.as_ref().unwrap().generation,
            generation_a
        );

        let generation_c = session.reload_at("sd", true, 0.2).expect("source C");
        session
            .mark_audible_generation(generation_c)
            .expect("retain C");
        assert_eq!(
            session.audible_source.as_ref().unwrap().generation,
            generation_c
        );
        session.install_rollback_source(captured);
        assert!(session.audible_source.is_none());
        assert_eq!(session.generation(), generation_c);
        assert_eq!(session.last_source.as_deref(), Some("sd"));
        assert_eq!(
            session.rollback_to_previous_source(0.3).expect("no source"),
            RollbackAttempt::Unavailable
        );
    }

    #[cfg(feature = "device-audio")]
    #[test]
    fn captured_identical_sources_keep_distinct_generations() {
        let mut session = Session::new().expect("session");
        session.evaluate_mini("bd").expect("first source");
        let first_generation = session.generation();
        let first = session
            .capture_rollback_source(first_generation)
            .expect("capture first")
            .expect("textual source");
        let second_generation = session.reload_at("bd", true, 0.1).expect("same source");
        let second = session
            .capture_rollback_source(second_generation)
            .expect("capture second")
            .expect("textual source");
        assert_eq!(second_generation, first_generation + 1);
        assert_eq!(first.source, second.source);
        assert_eq!(first.generation, first_generation);
        assert_eq!(second.generation, second_generation);

        session.install_rollback_source(Some(first));
        assert_eq!(
            session.audible_source.as_ref().unwrap().generation,
            first_generation
        );
        session.install_rollback_source(Some(second));
        assert_eq!(
            session.audible_source.as_ref().unwrap().generation,
            second_generation
        );
        assert_eq!(session.generation(), second_generation);
    }

    #[cfg(feature = "device-audio")]
    #[test]
    fn capturing_a_stale_generation_preserves_the_rollback_target() {
        let mut session = Session::new().expect("session");
        session.evaluate_mini("bd").expect("source A");
        // Inject replay policy, not consumer-copy evidence.
        session
            .mark_audible_generation(session.generation())
            .expect("injected replay-policy target");
        let generation_a = session.generation();
        let generation_b = session.reload_at("sd", true, 0.1).expect("source B");
        assert!(matches!(
            session.capture_rollback_source(generation_a),
            Err(RuntimeError::Message(_))
        ));
        assert!(session.mark_audible_generation(generation_a).is_err());
        let retained = session.audible_source.as_ref().expect("retained A");
        assert_eq!(retained.generation, generation_a);
        assert_eq!(retained.source.as_ref(), "bd");
        assert_eq!(session.generation(), generation_b);
    }

    #[cfg(feature = "device-audio")]
    #[test]
    fn rapid_unpublished_supersession_rolls_back_to_the_last_audible_score() {
        let mut session = Session::new().expect("session");
        session.evaluate("s('bd')").expect("audible A");
        // Inject replay policy, not consumer-copy evidence.
        session
            .mark_audible_generation(session.generation())
            .expect("injected replay-policy target");
        let audible_generation = session.generation();
        session
            .reload_at("s('sd')", false, 0.1)
            .expect("unpublished B");
        session
            .reload_at("s('hh')", false, 0.2)
            .expect("unpublished C");

        let audible = session.audible_source.as_ref().expect("audible snapshot");
        assert_eq!(audible.generation, audible_generation);
        assert_eq!(audible.source.as_ref(), "s('bd')");
        assert_eq!(
            session
                .rollback_to_previous_source(0.3)
                .expect("rollback A"),
            RollbackAttempt::Applied
        );
        assert_eq!(
            session.query(Fraction::ZERO, Fraction::ONE).unwrap()[0]
                .value
                .show(),
            "s:bd"
        );

        #[cfg(feature = "vst")]
        {
            let mut session = Session::new().expect("session");
            let source = r#"s("sine").vst("Delay").orbit(1)"#;
            session.evaluate(source).expect("audible plugin source");
            session
                .mark_audible_generation(session.generation())
                .expect("retain physical bus one");
            let moved = source.replace("orbit(1)", "orbit(2)");
            assert_eq!(session.plugin_calls_for_source(&moved)[0].orbit, Some(1));
            assert_eq!(session.insert_orbits, crate::vst::INSERT_ORBITS);
            session.reload_at(&moved, false, 0.1).expect("move to two");
            let accepted = session.insert_orbits;
            assert_eq!(accepted[2], 1);
            let moved = source.replace("orbit(1)", "orbit(3)");
            assert!(matches!(
                session.evaluate_cancellable(&moved, &AtomicBool::new(true)),
                Err(RuntimeError::Cancelled)
            ));
            assert_eq!(session.insert_orbits, accepted);
            session
                .reload_at(&moved, false, 0.2)
                .expect("move to three");
            assert_eq!(session.insert_orbits[3], 1);
            assert_eq!(
                session
                    .rollback_to_previous_source(0.3)
                    .expect("rollback orbit"),
                RollbackAttempt::Applied
            );
            assert_eq!(session.active_source(), Some(source));
            assert_eq!(session.insert_orbits, crate::vst::INSERT_ORBITS);
        }
    }

    #[cfg(feature = "device-audio")]
    #[test]
    fn an_injected_intermediate_target_becomes_the_next_replay_source() {
        let mut session = Session::new().expect("session");
        session.evaluate("s('bd')").expect("audible A");
        let generation_b = session
            .reload_at("s('sd')", false, 0.1)
            .expect("candidate B");
        // Inject replay policy, not consumer-copy evidence.
        session
            .mark_audible_generation(generation_b)
            .expect("injected replay-policy target");
        session
            .reload_at("s('hh')", false, 0.2)
            .expect("candidate C");

        assert_eq!(
            session
                .rollback_to_previous_source(0.3)
                .expect("rollback B"),
            RollbackAttempt::Applied
        );
        assert_eq!(
            session.query(Fraction::ZERO, Fraction::ONE).unwrap()[0]
                .value
                .show(),
            "s:sd"
        );
    }

    #[cfg(feature = "device-audio")]
    #[test]
    fn arbitrarily_many_unpublished_candidates_keep_one_bounded_audible_snapshot() {
        let mut session = Session::new().expect("session");
        session.evaluate_mini("bd").expect("audible score");
        // Inject replay policy, not consumer-copy evidence.
        session
            .mark_audible_generation(session.generation())
            .expect("injected replay-policy target");
        let audible_generation = session.generation();
        for index in 0..1_000 {
            let source = if index % 2 == 0 { "sd" } else { "hh" };
            session
                .reload_at(source, true, 0.001 * f64::from(index))
                .expect("superseding mini candidate");
        }
        let audible = session.audible_source.as_ref().expect("audible snapshot");
        assert_eq!(audible.generation, audible_generation);
        assert_eq!(audible.source.as_ref(), "bd");
    }

    #[cfg(feature = "device-audio")]
    #[test]
    fn an_audible_direct_pattern_clears_textual_rollback_and_remains_last_good() {
        let mut session = Session::new().expect("session");
        session.evaluate("pure('old-js')").expect("old source");
        // Inject replay policy, not consumer-copy evidence.
        session
            .mark_audible_generation(session.generation())
            .expect("injected replay-policy target");
        assert_eq!(
            session.audible_source.as_ref().unwrap().generation,
            session.generation()
        );
        session
            .set_pattern(rustel_core::pure(Value::Str("native".into())))
            .expect("direct pattern");
        let generation = session.generation();
        session
            .mark_audible_generation(generation)
            .expect("native cutover");
        assert!(
            session.audible_source.is_none(),
            "a direct Pattern retained an unrelated textual rollback target"
        );

        let error = session
            .reload_at(
                "pure('candidate').fmap(() => { throw new Error('boom'); })",
                false,
                0.25,
            )
            .expect_err("the query-throwing candidate must be rejected");
        assert!(error.to_string().contains("last-good score kept"));
        assert_eq!(session.generation(), generation);
        let haps = session
            .query(Fraction::ZERO, Fraction::ONE)
            .expect("native query");
        assert_eq!(haps.len(), 1);
        assert_eq!(haps[0].value, Value::Str("native".into()));
    }

    #[cfg(feature = "device-audio")]
    #[test]
    fn takeover_freshness_rejects_converted_native_events_before_publication() {
        for expired in [false, true] {
            let mut session = Session::new().expect("session");
            session.set_continuity_margin(0.125);
            session.evaluate_mini("~").expect("previous source");
            // Inject replay policy; the live producer test separately proves
            // retention of an actually consumer-confirmed silent score.
            let original = session.generation();
            session
                .mark_audible_generation(original)
                .expect("replay target");
            let voice = rustel_core::pure(Value::object(vec![
                ("s".into(), Value::Str("sine".into())),
                ("note".into(), Value::F64(48.0)),
                ("gain".into(), Value::F64(0.05)),
            ]))
            .fast(8.into());
            session
                .set_pattern_at(voice, 0.0, false)
                .expect("native voice");
            session.requery_active_at(0.0).expect("explicit takeover");
            let candidate = session.generation();
            let mut producer =
                crate::LiveFileProducer::unwatched(Duration::from_millis(2)).expect("producer");
            producer.arm_replacement(original, candidate);
            let mut clocks = [0.0, if expired { 0.25 } else { 0.0 }, 0.5].into_iter();
            let mut published = Vec::new();
            let mut events = Vec::new();
            let result = producer.step_unwatched_with_clock_and_cutover(
                &mut session,
                || clocks.next().unwrap_or(0.5),
                48_000,
                |generation, frame, _cut| published.push((generation, frame)),
                |event| {
                    events.push(event);
                    true
                },
            );
            if expired {
                let error = result.expect_err("expired converted window");
                assert!(error.to_string().contains("takeover"), "{error}");
                assert!(published.is_empty());
                assert!(
                    events.is_empty(),
                    "converted events escaped the refused window"
                );
                assert_eq!(session.active_source(), Some("~"));
            } else {
                result.expect("fresh converted window");
                assert_eq!(published, [(candidate, 6_000)]);
                assert!(!events.is_empty(), "control must convert real voices");
                assert!(
                    events
                        .iter()
                        .all(|event| event.gain > 0.0 && event.freq_hz > 0.0)
                );
            }
        }
    }

    #[cfg(feature = "device-audio")]
    #[test]
    fn takeover_freshness_preserves_a_loading_identity_until_progress() {
        let mut session = Session::new().expect("session");
        session.set_continuity_margin(0.125);
        let library = Arc::new(crate::samples::SampleLibrary::with_loading_sample_for_test(
            "held",
        ));
        session.samples = Some(Arc::clone(&library));
        session
            .set_pattern(
                rustel_core::pure(Value::object(vec![("s".into(), Value::Str("held".into()))]))
                    .fast(8.into()),
            )
            .expect("native sample graph");
        let before = session.generation();
        session.requery_active_at(0.0).expect("explicit takeover");
        let candidate = session.generation();
        let mut producer =
            crate::LiveFileProducer::unwatched(Duration::from_millis(2)).expect("producer");
        producer.arm_replacement(before, candidate);
        let held = producer
            .step_unwatched_with_clock_and_cutover(
                &mut session,
                || 0.0,
                48_000,
                |_, _, _| panic!("Loading must not publish"),
                |_| true,
            )
            .expect("held sample waits as a quiet turn");
        assert_eq!((held.scheduled, held.pushed, held.pending), (0, 0, 0));
        assert_eq!(session.generation(), candidate);
        library.finish_loading_sample_for_test();
        let mut published = Vec::new();
        let mut events = Vec::new();
        producer
            .step_unwatched_with_clock_and_cutover(
                &mut session,
                || 1.0,
                48_000,
                |generation, frame, _cut| published.push((generation, frame)),
                |event| {
                    events.push(event);
                    true
                },
            )
            .expect("Loading keeps its existing later-progress policy");
        assert_eq!(published, [(candidate, 6_000)]);
        assert!(!events.is_empty());
    }

    /// Rollback preserves the language route used by the previous score.
    #[cfg(feature = "device-audio")]
    #[test]
    fn a_rollback_reinstalls_a_mini_score_through_the_mini_door() {
        let mut session = Session::new().expect("session");
        session
            .evaluate_mini("bd sd")
            .expect("install the mini score");
        // Inject replay policy, not consumer-copy evidence.
        session
            .mark_audible_generation(session.generation())
            .expect("injected replay-policy target");
        // The score the watchdog would be rolling back FROM.
        session
            .evaluate(r#"s("hh*4")"#)
            .expect("install a js score");

        assert_eq!(
            session
                .rollback_to_previous_source(0.0)
                .expect("the rollback must not error"),
            RollbackAttempt::Applied
        );

        let haps = session.query(Fraction::ZERO, Fraction::ONE).expect("query");
        let values: Vec<String> = haps.iter().map(|hap| hap.value.show()).collect();
        assert_eq!(
            values,
            vec!["bd".to_string(), "sd".to_string()],
            "the rollback did not restore the mini score"
        );
    }

    #[cfg(feature = "device-audio")]
    #[test]
    fn javascript_rollback_restores_the_settings_inherited_by_the_old_score() {
        let mut session = Session::new().expect("session");
        session.set_default_voicings(Some("guidetones"));
        let inherited = "pure('C7').fmap(x => rustelScope.voicing(pure(x))).innerJoin()";
        session
            .evaluate(inherited)
            .expect("install inherited score");
        // Inject replay policy, not consumer-copy evidence.
        session
            .mark_audible_generation(session.generation())
            .expect("injected replay-policy target");
        assert_eq!(
            session
                .query(Fraction::ZERO, Fraction::ONE)
                .expect("query inherited score")
                .len(),
            2
        );
        session
            .evaluate(&format!("setDefaultVoicings('lefthand'); {inherited}"))
            .expect("install replacement");
        assert_eq!(
            session
                .query(Fraction::ZERO, Fraction::ONE)
                .expect("query replacement")
                .len(),
            4
        );

        assert_eq!(
            session
                .rollback_to_previous_source(0.0)
                .expect("rollback old JavaScript score"),
            RollbackAttempt::Applied
        );
        assert_eq!(
            session
                .query(Fraction::ZERO, Fraction::ONE)
                .expect("query rolled-back score")
                .len(),
            2,
            "rollback evaluated the old score under the replacement settings"
        );
    }

    #[cfg(feature = "device-audio")]
    #[test]
    fn mini_rollback_republishes_the_settings_inherited_by_the_old_score() {
        let mut session = Session::new().expect("session");
        session.set_default_voicings(Some("guidetones"));
        session.evaluate_mini("bd sd").expect("install mini score");
        // Inject replay policy, not consumer-copy evidence.
        session
            .mark_audible_generation(session.generation())
            .expect("injected replay-policy target");
        session
            .evaluate("setDefaultVoicings('lefthand'); s('hh*4')")
            .expect("install replacement");
        assert_eq!(session_core_settings(&session).2, 4);

        assert_eq!(
            session
                .rollback_to_previous_source(0.0)
                .expect("rollback old mini score"),
            RollbackAttempt::Applied
        );
        assert_eq!(
            session_core_settings(&session).2,
            2,
            "mini rollback left the replacement settings published"
        );
        let values: Vec<String> = session
            .query(Fraction::ZERO, Fraction::ONE)
            .expect("query rolled-back mini score")
            .into_iter()
            .map(|hap| hap.value.show())
            .collect();
        assert_eq!(values, ["bd", "sd"]);
    }

    /// A deferred rollback retains its target for a later recovery attempt.
    #[cfg(feature = "device-audio")]
    #[test]
    fn a_deferred_rollback_keeps_its_target_for_the_next_attempt() {
        let mut session = Session::new().expect("session");
        // Both scores go through the JavaScript door. An explicit mini file
        // never reaches the budget gate: it does not enter QuickJS, so it has
        // no CPU-bound claim to defer on.
        session
            .evaluate(r#"s("bd sd").gain(0.9)"#)
            .expect("install a js score");
        // Inject replay policy, not consumer-copy evidence.
        session
            .mark_audible_generation(session.generation())
            .expect("injected replay-policy target");
        session
            .evaluate(r#"s("hh*4").gain(0.9)"#)
            .expect("install a second js score");
        assert_eq!(
            session.last_path,
            EvaluateSource::JavaScript,
            "the fixture scores did not go through the JavaScript door"
        );
        session.play(2.0).expect("fill the horizon");

        // Just inside the end of the filled horizon: there is cover left, so
        // the gate defers rather than spending a recovery slice, but far too
        // little of it to afford the evaluation floor.
        let scheduled_to = session.scheduler.horizon_remaining(0.0);
        assert!(scheduled_to > 0.1, "the horizon never filled");
        let starved_now = scheduled_to - 0.005;
        assert_eq!(
            session
                .rollback_to_previous_source(starved_now)
                .expect("a deferred rollback is not an error"),
            RollbackAttempt::Deferred
        );

        // The target survived, so the retry that follows can still rescue the
        // set. Past the horizon there is no cover left to protect and the gate
        // spends a recovery slice instead of deferring.
        assert_eq!(
            session
                .rollback_to_previous_source(scheduled_to + 1.0)
                .expect("the retry must not error"),
            RollbackAttempt::Applied
        );
        let haps = session.query(Fraction::ZERO, Fraction::ONE).expect("query");
        let values: Vec<String> = haps.iter().map(|hap| hap.value.show()).collect();
        assert_eq!(
            values.len(),
            2,
            "the retry did not restore the two-onset score: {values:?}"
        );
    }

    #[test]
    fn a_stopped_session_recovers_after_start() {
        // Stop must not be terminal: the runtime has to schedule again
        // afterwards, or "cancellation" is indistinguishable from breakage.
        let mut session = Session::new().expect("session");
        session.evaluate(r#"s("bd sd")"#).expect("evaluate");
        let handle = session.transport();
        handle.stop();
        handle.start();
        assert!(
            !session.play(2.0).expect("play").onsets.is_empty(),
            "the session did not recover after a stop/start cycle"
        );
    }

    #[test]
    fn pure_scheduling_installs_no_host() {
        // The complement: a graph reaching no JavaScript must schedule with
        // nothing installed, exercising the purity invariant on
        // this path.
        let mut session = Session::new().expect("session");
        session.evaluate(r#"s("bd sd").fast(2)"#).expect("evaluate");
        assert!(!session.active_needs_host());
        let report = session.play(2.0).expect("play");
        assert!(!report.onsets.is_empty());
    }

    #[test]
    fn max_polyphony_is_transactional_and_isolated_per_session() {
        assert_eq!(
            rustel_core::settings::MAX_CONFIGURABLE_POLYPHONY,
            rustel_audio::MAX_CONFIGURABLE_POLYPHONY
        );
        let mut first = Session::with_config(SessionConfig {
            max_polyphony: 192,
            ..Default::default()
        })
        .expect("session");
        let second = Session::new().expect("independent session");
        assert_eq!(first.max_polyphony(), 192);
        assert_eq!(first.max_polyphony_override(), None);
        assert_eq!(second.max_polyphony(), 128);
        first
            .evaluate("setMaxPolyphony(256); s('sine')")
            .expect("valid override");
        assert_eq!(first.max_polyphony(), 256);
        first.set_default_max_polyphony(64);
        assert_eq!(
            first.max_polyphony(),
            256,
            "host defaults do not replace score settings"
        );
        first.evaluate("s('sine')").expect("next score");
        assert_eq!(
            first.max_polyphony(),
            256,
            "omitting a module setter preserves it"
        );
        assert!(
            first
                .evaluate("setMaxPolyphony(4); throw new Error('reject')")
                .is_err()
        );
        assert_eq!(
            first.max_polyphony(),
            256,
            "failed evaluation is transactional"
        );
        assert_eq!(second.max_polyphony(), 128);
        first
            .evaluate("setMaxPolyphony(4); s('sine')")
            .expect("accepted replacement");
        assert_eq!(first.max_polyphony(), 4);
        assert_eq!(first.max_polyphony_override(), Some(4));
    }

    #[test]
    fn max_polyphony_rejects_invalid_score_values_without_changing_last_good() {
        let mut session = Session::new().expect("session");
        session
            .evaluate("setMaxPolyphony(192); s('sine')")
            .expect("score");
        let generation = session.generation();
        for bad in [
            "", "0", "-1", "1.5", "257", "512", "NaN", "Infinity", "'64'", "null", "true",
        ] {
            let source = format!("setMaxPolyphony({bad}); s('sine')");
            let error = session.evaluate(&source).expect_err(&source);
            assert!(error.to_string().contains("setMaxPolyphony"), "{error}");
            assert_eq!(session.max_polyphony(), 192, "{source}");
            assert_eq!(session.generation(), generation, "{source}");
        }
        for voices in [0, 257, usize::MAX] {
            assert!(
                Session::with_config(SessionConfig {
                    max_polyphony: voices,
                    ..Default::default()
                })
                .is_err()
            );
        }
    }

    #[test]
    fn max_polyphony_changes_the_voices_in_offline_pcm() {
        fn render(voices: usize, script_override: bool) -> Vec<f32> {
            let mut session = Session::with_config(SessionConfig {
                max_polyphony: if script_override { 128 } else { voices },
                ..Default::default()
            })
            .expect("session");
            let setter = if script_override {
                format!("setMaxPolyphony({voices});")
            } else {
                String::new()
            };
            session.evaluate(&format!("{setter} stack(note('c3'), note('e3'), note('g3'), note('c4')).s('sine').gain(0.05).attack(0).decay(0).sustain(1).release(0.1)")).expect("score");
            session.render_pcm(0.05).expect("PCM")
        }
        let limited = render(1, false);
        let all = render(4, false);
        assert_eq!(
            all,
            render(4, true),
            "score and host budgets reach the same backend"
        );
        assert!(
            limited
                .iter()
                .zip(&all)
                .any(|(a, b)| (a - b).abs() > 0.0001),
            "limiting polyphony must change actual audio"
        );
    }

    #[test]
    fn pure_scheduling_uses_the_owning_sessions_module_settings() {
        let mut first = Session::new().expect("first session");
        let mut second = Session::new().expect("second session");
        first
            .evaluate("setDefaultVoicings('guidetones'); chord('C7').voicing()")
            .expect("first score");
        second
            .evaluate("setDefaultVoicings('lefthand'); chord('C7').voicing()")
            .expect("second score");

        assert!(!first.active_needs_host());
        assert!(!second.active_needs_host());
        assert_eq!(
            first
                .play(0.25)
                .expect("schedule first session")
                .onsets
                .len(),
            2
        );
        assert_eq!(
            second
                .play(0.25)
                .expect("schedule second session")
                .onsets
                .len(),
            4
        );
    }

    fn default_voicing_value() -> Value {
        let mut controls = rustel_core::OrderedMap::new();
        controls.insert("chord".into(), Value::Str("C7".into()));
        Value::List(
            rustel_core::voicings::render_voicing(&controls).expect("configured C7 voicing"),
        )
    }

    fn default_voicing_size_pattern() -> Pattern {
        rustel_core::state_signal(|_| default_voicing_value())
    }

    fn session_core_settings(
        session: &Session,
    ) -> (
        rustel_core::rng::RngMode,
        rustel_core::compose::Alignment,
        usize,
    ) {
        session.js.with_runtime_settings(|| {
            let mut controls = rustel_core::OrderedMap::new();
            controls.insert("chord".into(), Value::Str("C7".into()));
            (
                rustel_core::rng::rng_mode(),
                rustel_core::compose::default_alignment(),
                rustel_core::voicings::render_voicing(&controls)
                    .expect("configured C7 voicing")
                    .len(),
            )
        })
    }

    #[test]
    fn rust_patterns_snapshot_ambient_settings_when_a_session_adopts_them() {
        let ambient = rustel_core::settings::RuntimeSettings::default();
        ambient.with(|| {
            rustel_core::voicings::set_default_voicings("guidetones");
            let mut session = Session::new().expect("session");
            session
                .set_pattern(default_voicing_size_pattern())
                .expect("install Rust pattern");

            rustel_core::voicings::set_default_voicings("lefthand");
            let haps = session
                .query(Fraction::ZERO, Fraction::ONE)
                .expect("query adopted pattern");
            assert!(matches!(&haps[0].value, Value::List(notes) if notes.len() == 2));
        });
    }

    #[test]
    fn adopted_rust_patterns_export_metadata_with_the_session_settings() {
        let ambient = rustel_core::settings::RuntimeSettings::default();
        ambient.with(|| {
            rustel_core::voicings::set_default_voicings("guidetones");
            let function = rustel_core::value::FunctionRef::native(
                Some("defaultVoicing"),
                Arc::new(|pattern| pattern.fmap(|_| default_voicing_value())),
                Arc::new(|pattern| pattern.fmap(|_| default_voicing_value())),
            );
            let carrier = rustel_core::pure(Value::Function(function));
            let mut session = Session::new().expect("session");
            session.set_pattern(carrier).expect("install Rust pattern");

            rustel_core::voicings::set_default_voicings("lefthand");
            let haps = session
                .query(Fraction::ZERO, Fraction::ONE)
                .expect("query adopted carrier");
            let Value::Function(function) = &haps[0].value else {
                panic!("carrier did not emit its Function");
            };
            let output = function.apply(rustel_core::pure(Value::Null));
            let output_haps = output.query_arc(Fraction::ZERO, Fraction::ONE);
            assert!(
                matches!(&output_haps[0].value, Value::List(notes) if notes.len() == 2),
                "executable metadata escaped into the ambient runtime"
            );
        });
    }

    #[test]
    fn rust_hosts_can_configure_each_session_explicitly() {
        let mut first = Session::new().expect("first session");
        let mut second = Session::new().expect("second session");
        first.set_default_voicings(Some("guidetones"));
        second.set_default_voicings(Some("lefthand"));
        first
            .set_pattern(default_voicing_size_pattern())
            .expect("first Rust pattern");
        second
            .set_pattern(default_voicing_size_pattern())
            .expect("second Rust pattern");

        let first_haps = first
            .query(Fraction::ZERO, Fraction::ONE)
            .expect("query first Rust pattern");
        let second_haps = second
            .query(Fraction::ZERO, Fraction::ONE)
            .expect("query second Rust pattern");
        assert!(matches!(&first_haps[0].value, Value::List(notes) if notes.len() == 2));
        assert!(matches!(&second_haps[0].value, Value::List(notes) if notes.len() == 4));
    }

    #[test]
    fn rejected_javascript_transition_keeps_the_native_graphs_settings() {
        let ambient = rustel_core::settings::RuntimeSettings::default();
        ambient.with(|| {
            rustel_core::rng::use_rng(rustel_core::rng::RngMode::Precise);
            rustel_core::compose::set_default_alignment(rustel_core::compose::Alignment::Out);
            rustel_core::voicings::set_default_voicings("guidetones");
            let mut session = Session::new().expect("session");
            session
                .set_pattern(default_voicing_size_pattern())
                .expect("install native graph");

            rustel_core::rng::use_rng(rustel_core::rng::RngMode::Legacy);
            rustel_core::compose::set_default_alignment(rustel_core::compose::Alignment::Mix);
            rustel_core::voicings::set_default_voicings("lefthand");
            session
                .evaluate("setDefaultJoin('mix'); throw new Error('reject transition')")
                .expect_err("reject score");

            assert_eq!(
                session_core_settings(&session),
                (
                    rustel_core::rng::RngMode::Precise,
                    rustel_core::compose::Alignment::Out,
                    2,
                )
            );
        });
    }

    #[test]
    fn each_explicit_setter_preserves_the_other_adopted_fields() {
        let ambient = rustel_core::settings::RuntimeSettings::default();
        ambient.with(|| {
            rustel_core::rng::use_rng(rustel_core::rng::RngMode::Precise);
            rustel_core::compose::set_default_alignment(rustel_core::compose::Alignment::Out);
            rustel_core::voicings::set_default_voicings("guidetones");
            let mut rng_session = Session::new().expect("rng session");
            let mut join_session = Session::new().expect("join session");
            let mut voicing_session = Session::new().expect("voicing session");
            for session in [&mut rng_session, &mut join_session, &mut voicing_session] {
                session
                    .set_pattern(default_voicing_size_pattern())
                    .expect("adopt native settings");
            }

            rustel_core::rng::use_rng(rustel_core::rng::RngMode::Legacy);
            rustel_core::compose::set_default_alignment(rustel_core::compose::Alignment::Mix);
            rustel_core::voicings::set_default_voicings("lefthand");

            rng_session.set_rng_mode(rustel_core::rng::RngMode::Legacy);
            join_session.set_default_join(rustel_core::compose::Alignment::Restart);
            voicing_session.set_default_voicings(Some("lefthand"));

            assert_eq!(
                session_core_settings(&rng_session),
                (
                    rustel_core::rng::RngMode::Legacy,
                    rustel_core::compose::Alignment::Out,
                    2,
                )
            );
            assert_eq!(
                session_core_settings(&join_session),
                (
                    rustel_core::rng::RngMode::Precise,
                    rustel_core::compose::Alignment::Restart,
                    2,
                )
            );
            assert_eq!(
                session_core_settings(&voicing_session),
                (
                    rustel_core::rng::RngMode::Precise,
                    rustel_core::compose::Alignment::Out,
                    4,
                )
            );
        });
    }

    #[test]
    fn mini_adoption_is_transactional_across_parse_and_live_probe() {
        let ambient = rustel_core::settings::RuntimeSettings::default();
        ambient.with(|| {
            rustel_core::rng::use_rng(rustel_core::rng::RngMode::Precise);
            rustel_core::compose::set_default_alignment(rustel_core::compose::Alignment::Out);
            rustel_core::voicings::set_default_voicings("guidetones");
            let mut session = Session::new().expect("session");
            session.evaluate_mini("bd").expect("initial mini graph");

            rustel_core::rng::use_rng(rustel_core::rng::RngMode::Legacy);
            rustel_core::compose::set_default_alignment(rustel_core::compose::Alignment::Mix);
            rustel_core::voicings::set_default_voicings("lefthand");
            session
                .evaluate_live_score_cancellable(
                    "[",
                    true,
                    Duration::ZERO,
                    &NEVER_CANCELLED,
                    || 0.0,
                )
                .expect_err("reject malformed mini graph");
            assert_eq!(
                session_core_settings(&session),
                (
                    rustel_core::rng::RngMode::Precise,
                    rustel_core::compose::Alignment::Out,
                    2,
                )
            );

            session
                .evaluate_live_score_cancellable(
                    "sd",
                    true,
                    Duration::ZERO,
                    &NEVER_CANCELLED,
                    || 0.0,
                )
                .expect("probe and install live mini graph");
            assert_eq!(
                session_core_settings(&session),
                (
                    rustel_core::rng::RngMode::Legacy,
                    rustel_core::compose::Alignment::Mix,
                    4,
                )
            );
        });
    }

    #[test]
    fn mini_fallback_constructs_with_the_adopted_session_settings() {
        let ambient = rustel_core::settings::RuntimeSettings::default();
        ambient.with(|| {
            rustel_core::rng::use_rng(rustel_core::rng::RngMode::Precise);
            rustel_core::compose::set_default_alignment(rustel_core::compose::Alignment::In);
            rustel_core::voicings::set_default_voicings("guidetones");
            let mut session = Session::new().expect("session");
            session
                .set_pattern(default_voicing_size_pattern())
                .expect("install native graph");

            rustel_core::rng::use_rng(rustel_core::rng::RngMode::Legacy);
            rustel_core::compose::set_default_alignment(rustel_core::compose::Alignment::Mix);
            rustel_core::voicings::set_default_voicings("lefthand");
            session
                .evaluate("bd sd")
                .expect("mini compatibility fallback");

            assert_eq!(session.last_evaluate_source(), EvaluateSource::MiniRust);
            assert_eq!(
                session_core_settings(&session),
                (
                    rustel_core::rng::RngMode::Precise,
                    rustel_core::compose::Alignment::In,
                    2,
                )
            );
        });
    }

    #[test]
    fn editor_visual_calls_keep_the_native_audio_graph_portable() {
        let mut plain = Session::new().expect("plain session");
        plain.evaluate(r#"s("bd sd")"#).expect("plain score");
        let expected = plain
            .query(Fraction::ZERO, Fraction::ONE)
            .expect("plain query")
            .into_iter()
            .map(|hap| hap.show())
            .collect::<Vec<_>>();

        for method in [
            "_scope",
            "_tscope",
            "_pianoroll",
            "_punchcard",
            "_spiral",
            "_pitchwheel",
            "_spectrum",
            "scope",
            "tscope",
            "pianoroll",
            "punchcard",
            "spiral",
            "pitchwheel",
            "spectrum",
            "markcss",
        ] {
            let argument = if method == "markcss" {
                "'color: cyan; text-decoration: underline'"
            } else {
                "{}"
            };
            let mut visual = Session::new().expect("visual session");
            visual
                .evaluate(&format!(r#"s("bd sd").{method}({argument})"#))
                .unwrap_or_else(|error| panic!("{method} did not evaluate: {error}"));
            assert_eq!(
                visual
                    .query(Fraction::ZERO, Fraction::ONE)
                    .unwrap_or_else(|error| panic!("{method} did not query: {error}"))
                    .into_iter()
                    .map(|hap| hap.show())
                    .collect::<Vec<_>>(),
                expected,
                "{method} changed the sounding graph"
            );
        }

        let mut all = Session::new().expect("all visual session");
        all.evaluate(r#"all(pianoroll); s("bd sd")"#)
            .expect("all(pianoroll) score");
        assert_eq!(
            all.query(Fraction::ZERO, Fraction::ONE)
                .expect("all(pianoroll) query")
                .into_iter()
                .map(|hap| hap.show())
                .collect::<Vec<_>>(),
            expected
        );
    }

    #[test]
    fn removed_wavetable_visual_is_not_installed() {
        let mut session = Session::new().expect("session");
        assert!(
            session.evaluate(r#"s("bd")._wavetable()"#).is_err(),
            "the removed visual must not remain as a silent compatibility call"
        );
    }

    #[test]
    fn extract_s_call() {
        assert_eq!(
            extract_mini_fallback(r#"s("bd sd")"#).as_deref(),
            Some("bd sd")
        );
    }

    #[test]
    fn rust_mini_query_bd_sd() {
        let mut session = Session::new().unwrap();
        session.evaluate_mini("bd sd").unwrap();
        let haps = session.query(Fraction::ZERO, Fraction::ONE).unwrap();
        assert_eq!(haps.len(), 2);
        assert_eq!(haps[0].value.show(), "bd");
        assert_eq!(haps[1].value.show(), "sd");
    }

    #[test]
    fn fullscreen_owner_can_drain_diagnostics_without_direct_logging() {
        let mut session = Session::new().unwrap();
        session.set_direct_diagnostic_logging(false);
        session.report_diagnostic(
            "voice-refused",
            "unknown voice",
            serde_json::json!({ "voice_refused": { "message": "unknown voice" } }),
        );
        session.report_diagnostic(
            "voice-refused",
            "unknown voice",
            serde_json::json!({ "voice_refused": { "message": "unknown voice" } }),
        );

        assert_eq!(
            session.take_diagnostics(),
            vec![SessionDiagnostic {
                kind: "voice-refused".into(),
                message: "unknown voice".into(),
                recoverable: true,
            }]
        );
        assert!(session.take_diagnostics().is_empty());
    }

    #[test]
    fn unknown_sound_name_renders_silence_and_reports_voice_refused() {
        let mut session = Session::new().expect("session");
        session.set_direct_diagnostic_logging(false);
        session
            .evaluate(r#"note("c3 e3 g3").s("gm_acoustic_grand_piano")"#)
            .expect("unknown-sound score");
        let pcm = session.render_pcm(2.0).expect("silent render");
        assert!(
            pcm.iter().all(|sample| *sample == 0.0),
            "an unknown sound name must render silence, got {} non-zero samples",
            pcm.iter().filter(|sample| **sample != 0.0).count()
        );
        let diagnostics = session.take_diagnostics();
        assert!(
            diagnostics
                .iter()
                .any(|diagnostic| diagnostic.kind == "voice-refused"),
            "expected a voice-refused diagnostic: {diagnostics:?}"
        );
    }

    #[test]
    fn known_sound_name_renders_audio_without_voice_refused() {
        let mut session = Session::new().expect("session");
        session.set_direct_diagnostic_logging(false);
        // The default sample library is how a musician's session learns the
        // `gm_*` soundfonts; without it every name is refused as unknown.
        session
            .enable_default_samples()
            .expect("default sample library");
        session
            .evaluate(r#"note("c3 e3 g3").s("gm_piano")"#)
            .expect("known-sound score");
        let pcm = session.render_pcm(2.0).expect("render");
        let diagnostics = session.take_diagnostics();
        assert!(
            !diagnostics
                .iter()
                .any(|diagnostic| diagnostic.kind == "voice-refused"),
            "a known sound name must not be refused: {diagnostics:?}"
        );
        assert!(
            pcm.iter().any(|sample| sample.abs() > 1e-6),
            "a known sound name must be audible: {diagnostics:?}"
        );
    }
}
