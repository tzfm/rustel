use super::*;

impl Session {
    /// Select whether recoverable live diagnostics are written to stderr.
    ///
    /// A new Session takes this from
    /// [`rustel_voice::default_direct_diagnostic_logging`], which is off
    /// unless the host turned it on, as the `rustel` command line does. With
    /// it off, diagnostics are collected for [`Self::take_diagnostics`],
    /// which a host drains from its presentation loop; they include `.log()`
    /// lines, the voice resolver's notices ([`VOICE_NOTICE_DIAGNOSTIC`]) and
    /// the plugin lines ([`PLUGIN_DIAGNOSTIC`]).
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

    /// Surface loader failures once each (never-die: a missing sample is a
    /// skipped onset plus one loud line, not an error). With direct logging
    /// off they stay for the host's [`Self::take_sample_failures`], maps
    /// and all.
    pub(super) fn log_sample_failures(&mut self) {
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
    /// is not queued again, so one raised every window holds one place. A
    /// plugin notice is a plugin line: see [`Self::report_plugin_line`].
    pub(super) fn report_voice_notices(&mut self, notices: Vec<rustel_voice::VoiceNotice>) {
        for notice in notices {
            if notice.record.get("vst_skipped").is_some() {
                self.report_plugin_line(notice.message, notice.record);
                continue;
            }
            let pending = self.pending_diagnostics.iter().any(|diagnostic| {
                diagnostic.kind == VOICE_NOTICE_DIAGNOSTIC && diagnostic.message == notice.message
            });
            if !pending {
                self.report_diagnostic(VOICE_NOTICE_DIAGNOSTIC, notice.message, notice.record);
            }
        }
        // A problem of the plugin thread has no note to report through, so
        // its line joins here. Only the session of the live output takes the
        // lines of the host and of the output. A session with no live output
        // leaves them. The chains of this session give the lanes with no
        // room for a chain.
        #[cfg(feature = "vst")]
        {
            if self.plugins_live() {
                self.report_plugin_lines(crate::vst::take_errors());
                self.report_plugin_lines(crate::vst::take_output_lines(&self.chains));
            }
            self.report_plugin_lines(crate::vst::take_problems(&self.chains));
        }
        for message in self.csound.take_errors() {
            let record = serde_json::json!({ "csound_failed": { "message": &message } });
            self.report_plugin_line(message, record);
        }
    }

    /// Queues each line as a plugin line: see [`Self::report_plugin_line`].
    #[cfg(feature = "vst")]
    pub(super) fn report_plugin_lines(&mut self, lines: Vec<String>) {
        for message in lines {
            let record = serde_json::json!({ "vst_skipped": { "message": &message } });
            self.report_plugin_line(message, record);
        }
    }

    /// Queue one plugin line as a [`PLUGIN_DIAGNOSTIC`]. A note gives its
    /// line again in each window, so a text goes out one time for each
    /// evaluation. A score with a new text for each note fills the set, and
    /// its later texts do not go out.
    pub(super) fn report_plugin_line(&mut self, message: String, record: serde_json::Value) {
        if self.plugin_lines_reported.len() < MAX_PENDING_SESSION_DIAGNOSTICS
            && self.plugin_lines_reported.insert(message.clone())
        {
            self.report_diagnostic(PLUGIN_DIAGNOSTIC, message, record);
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
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(all(test, feature = "device-audio"))]
    mod callback_function_error_tests {
        //! A callback that throws inside a score does not stop playback, and its
        //! warning is reported once. `.log()` prints on the live audio path.
        //!
        //! These tests use a real `Session`, scheduler and QuickJS host, and the
        //! live audio scheduling path. They mock no part of the query or schedule
        //! pipeline.

        use super::*;
        use crate::session::test_support::{quiet_session, warnings};
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
