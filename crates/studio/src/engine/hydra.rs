use super::*;

/// The log alert under which a refused camera is raised and later resolved.
#[cfg(feature = "hydra")]
const CAMERA_ALERT: &str = "hydra:camera";

/// A Hydra failure as the log says it. A refused camera is a condition, not
/// an event: it stands until nothing asks for the camera, so it is raised
/// under a key that [`settle_camera_alert`] resolves.
#[cfg(feature = "hydra")]
fn hydra_diagnostic(message: String) -> StudioDiagnostic {
    let blocked = message.ends_with(rustel_runtime::hydra::WEBCAM_BLOCKED_HINT);
    let diagnostic = StudioDiagnostic::message("hydra", message);
    if blocked {
        diagnostic.raising(CAMERA_ALERT)
    } else {
        diagnostic
    }
}

/// Resolve the camera alert once a theme or score stops asking for the
/// camera, or Settings allows it. The log keeps the line as history.
#[cfg(feature = "hydra")]
fn settle_camera_alert(
    pending: &mut PendingDiagnostics,
    policy: &rustel_runtime::hydra::HydraInputPolicy,
) {
    // Only while the alert stands: nothing to read on an ordinary turn.
    if !pending.alert_keys.contains(CAMERA_ALERT) {
        return;
    }
    let status = policy.webcam_status();
    let refused =
        status.requested && status.state == rustel_runtime::hydra::HydraWebcamState::Blocked;
    if !refused {
        queue_diagnostic(pending, StudioDiagnostic::resolve(CAMERA_ALERT));
    }
}

impl StudioEngine {
    /// What the snippet shelf wants shown, if anything has changed.
    ///
    /// `Some(None)` means "give the screen back to the score"; `None` means
    /// nothing has been asked for since the last turn.
    #[cfg(feature = "hydra")]
    pub fn set_hydra_preview(&mut self, code: Option<String>) {
        self.hydra_preview = Some(code);
    }

    #[cfg(feature = "hydra")]
    pub fn set_hydra_theme(&mut self, code: Option<String>, webcam: bool) {
        self.hydra_theme = Some((code, webcam));
    }

    #[cfg(feature = "hydra")]
    pub fn set_hydra_frame_size(&mut self, wanted: rustel_runtime::hydra::HydraFrameRequest) {
        self.hydra_frame_size = Some(wanted);
    }

    /// Hand this turn's visuals to the window, and the window this turn's
    /// signals and sound.
    ///
    /// Runs on the engine worker, where the Session lives, so an `H(...)`
    /// pattern is sampled with exactly the settings and at exactly the cycle
    /// the audio is using. Nothing here blocks: the window is on its own
    /// thread behind a channel that drops rather than waits.
    ///
    /// Each distinct failure of the turn is queued with the engine's own
    /// diagnostics, which wait for room in the channel. The returned notice
    /// is informational, and a full channel may skip it.
    #[cfg(feature = "hydra")]
    pub fn drive_hydra(
        &mut self,
        hydra: &mut rustel_runtime::hydra::HydraBridge,
        audio: Option<&rustel_runtime::ui_analysis::UiAudioAnalysisFrame>,
    ) -> Option<StudioDiagnostic> {
        let mut note = None;
        let mut trouble = Vec::new();
        let mut failure = |message: String| {
            if !trouble.contains(&message) {
                trouble.push(message);
            }
        };
        if let Some(wanted) = self.hydra_frame_size.take() {
            hydra.set_frames_wanted(wanted);
        }
        if let Some(preview) = self.hydra_preview.take()
            && let Err(message) = hydra.preview(preview.as_deref())
        {
            failure(message);
        }
        if let Some((theme, webcam)) = self.hydra_theme.take()
            && let Err(message) = hydra.theme_with_camera(theme.as_deref(), webcam)
        {
            failure(message);
        }
        if let Some(candidate) = self.session.take_pending_hydra() {
            match rustel_runtime::hydra::HydraUpdate::from_candidate(&candidate) {
                Ok(update) => {
                    let folder = self
                        .session
                        .sample_library()
                        .and_then(|library| library.set_folder());
                    hydra.set_media_folder(folder.as_deref());
                    if let Err(message) = hydra.apply_with_sample_access(
                        update,
                        &self.session.config().score_sample_access,
                    ) {
                        failure(message);
                    }
                }
                Err(message) => failure(message),
            }
        }
        for event in hydra.take_events() {
            match event {
                rustel_hydra::HydraEvent::Failed { message, .. } => failure(message),
                // Say once, in the log, which surface the visuals landed on.
                // Without it there is no way to tell a sketch drawing behind
                // the code from one drawing in a window somewhere else.
                rustel_hydra::HydraEvent::Ready { renderer } => {
                    note.get_or_insert_with(|| format!("visuals are drawing here ({renderer})"));
                }
                rustel_hydra::HydraEvent::Input {
                    slot,
                    message,
                    failed,
                } => {
                    let message = format!("Hydra s{slot}: {message}");
                    if failed {
                        failure(message);
                    } else {
                        note.get_or_insert(message);
                    }
                }
                _ => {}
            }
        }
        // A stopped set wipes the picture, the way the strudel.cc editor does.
        // The shelf's own picture is a second renderer with its own liveness,
        // so browsing does not keep the score's visuals alive - and inserting
        // a snippet leaves the screen still until you press play.
        hydra.set_drawing(self.is_playing());
        let now = Instant::now();
        if let Err(message) = hydra.tick(
            now,
            self.session.cycle_at_time(self.device_time()),
            self.session.cps(),
        ) {
            queue_diagnostic(
                &mut self.pending_diagnostics,
                StudioDiagnostic::message("hydra", message),
            );
        }
        if let Some(frame) = audio.filter(|_| hydra.wants_audio()) {
            let rms = (frame.scope.iter().map(|s| s * s).sum::<f32>()
                / frame.scope.len().max(1) as f32)
                .sqrt();
            hydra.audio(&rustel_runtime::hydra::audio_frame(rms, &frame.spectrum));
        }
        for message in trouble {
            queue_diagnostic(&mut self.pending_diagnostics, hydra_diagnostic(message));
        }
        settle_camera_alert(&mut self.pending_diagnostics, &hydra.input_policy());
        note.map(|message| StudioDiagnostic::info("hydra", message))
    }

    /// Where the audible clock is now; zero when nothing is open, which is
    /// the same answer the status bar gives, and the same one the snapshot
    /// carries.
    pub(super) fn device_time(&self) -> f64 {
        self.live
            .as_ref()
            .map_or(0.0, |live| live.device.clock_seconds())
    }
}

#[cfg(all(test, feature = "hydra"))]
mod tests {
    use super::*;
    use crate::engine::tests::*;

    /// A refused camera warns under a key. The warning resolves when nothing
    /// asks for the camera any more, or when Settings allows it, and the
    /// other Hydra failures carry no key.
    #[test]
    fn a_refused_camera_warning_resolves_when_the_request_ends() {
        let blocked = hydra_diagnostic(format!(
            "Hydra s0: webcam theme blocked; {}",
            rustel_runtime::hydra::WEBCAM_BLOCKED_HINT
        ));
        assert_eq!(
            blocked.alert,
            Some(DiagnosticAlert::Raise(CAMERA_ALERT.into()))
        );
        assert_eq!(
            hydra_diagnostic("Hydra s0: no such image".into()).alert,
            None
        );

        let resolves = |pending: &PendingDiagnostics| {
            pending.iter().any(|diagnostic| {
                diagnostic.alert == Some(DiagnosticAlert::Resolve(CAMERA_ALERT.into()))
            })
        };
        let bridge = rustel_runtime::hydra::HydraBridge::new();
        let policy = bridge.input_policy();

        // Asked for and refused: the warning stands.
        policy.set_settings_preview_requested(true);
        let mut pending = PendingDiagnostics::default();
        queue_diagnostic(&mut pending, blocked.clone());
        settle_camera_alert(&mut pending, &policy);
        assert!(!resolves(&pending));

        // Nothing asks for it any more.
        policy.set_settings_preview_requested(false);
        settle_camera_alert(&mut pending, &policy);
        assert!(resolves(&pending));

        // Still asked for, but Settings now allows it.
        let mut pending = PendingDiagnostics::default();
        queue_diagnostic(&mut pending, blocked);
        policy.set_settings_preview_requested(true);
        policy.set_webcam_allowed(true);
        settle_camera_alert(&mut pending, &policy);
        assert!(resolves(&pending));
    }

    /// Two chains that fail differently in one frame each reach the
    /// diagnostics once, through the queue a full channel holds.
    #[cfg(feature = "hydra")]
    #[test]
    fn chains_that_fail_in_one_frame_each_reach_the_diagnostics() {
        if rustel_hydra::native::NativeRenderer::new(8, 8).is_err() {
            eprintln!("skipped: no GPU and no software rasteriser");
            return;
        }
        let mut engine = silent_engine_with_pattern(rustel_core::silence());
        engine
            .session
            .evaluate("await initHydra()\nosc().foo().out(o0)\nosc().bar().out(o1)\n")
            .expect("the score records both chains");
        let mut hydra = rustel_runtime::hydra::HydraBridge::new();
        let frames = hydra.frames();
        let mut notices: Vec<StudioDiagnostic> =
            engine.drive_hydra(&mut hydra, None).into_iter().collect();
        // Both chains fail before their frame is read back, so the next turn
        // drains both failures at once.
        let until = Instant::now() + Duration::from_secs(20);
        while frames.take().is_none() {
            assert!(Instant::now() < until, "the sketch never drew a frame");
            std::thread::sleep(Duration::from_millis(10));
        }
        notices.extend(engine.drive_hydra(&mut hydra, None));

        emit_pending_diagnostics(&mut engine.pending_diagnostics, &mut |update| {
            Err((UiEventSendStatus::DroppedFull, update))
        });
        let mut said = notices.clone();
        emit_pending_diagnostics(&mut engine.pending_diagnostics, &mut |update| {
            if let StudioUpdate::Diagnostic(diagnostic) = update {
                said.push(diagnostic);
            }
            Ok(())
        });
        let failures: Vec<&str> = said
            .iter()
            .filter(|diagnostic| {
                diagnostic.kind == "hydra" && diagnostic.level == StudioDiagnosticLevel::Warning
            })
            .map(|diagnostic| diagnostic.message.as_str())
            .collect();
        assert!(
            failures.len() == 2
                && failures.iter().any(|message| message.contains("foo"))
                && failures.iter().any(|message| message.contains("bar")),
            "each chain's failure must be said once: {said:?}"
        );
        assert!(
            notices
                .iter()
                .all(|notice| notice.level != StudioDiagnosticLevel::Warning),
            "a failure must not ride the notice a full channel skips: {notices:?}"
        );
    }
}
