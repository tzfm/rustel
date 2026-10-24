//! Feedback for explicit evaluations, tied to their submitting pane and result.

use super::super::settings::{EvalAnimation, EvaluationFlashMode};
use super::super::worker::EngineFailure;
use super::*;

/// How long a success stays lit.
const SUCCESS_DURATION: Duration = Duration::from_millis(200);
/// One lit or dark beat of a failure's two pulses.
const FAILURE_BEAT: Duration = Duration::from_millis(70);

/// One animated run of the flash: how long it takes to arrive, how long it
/// holds, and how long it takes to leave.
struct Run {
    fade_in: Duration,
    hold: Duration,
    fade_out: Duration,
}

impl Run {
    const fn instant(lit: Duration) -> Self {
        Self {
            fade_in: Duration::ZERO,
            hold: lit,
            fade_out: Duration::ZERO,
        }
    }

    fn total(&self) -> Duration {
        self.fade_in + self.hold + self.fade_out
    }
}

impl EvalAnimation {
    /// The run one success takes.
    fn success_run(self) -> Run {
        match self {
            Self::Normal => Run::instant(SUCCESS_DURATION),
            Self::FadeFast => Run {
                fade_in: Duration::from_millis(40),
                hold: Duration::from_millis(170),
                fade_out: Duration::from_millis(130),
            },
            Self::FadeMedium => Run {
                fade_in: Duration::from_millis(80),
                hold: Duration::from_millis(210),
                fade_out: Duration::from_millis(350),
            },
            Self::FadeSlow => Run {
                fade_in: Duration::from_millis(120),
                hold: Duration::from_millis(250),
                fade_out: Duration::from_millis(670),
            },
            Self::Zap => Run {
                fade_in: Duration::ZERO,
                hold: Duration::from_millis(60),
                fade_out: Duration::from_millis(120),
            },
            Self::Steps => Run {
                fade_in: Duration::ZERO,
                hold: Duration::from_millis(90),
                fade_out: Duration::from_millis(270),
            },
            Self::Swipe => Run {
                fade_in: Duration::from_millis(30),
                hold: Duration::from_millis(100),
                fade_out: Duration::from_millis(500),
            },
            Self::Glitch => Run {
                fade_in: Duration::ZERO,
                hold: Duration::from_millis(40),
                fade_out: Duration::from_millis(220),
            },
            Self::Wave => Run {
                fade_in: Duration::from_millis(30),
                hold: Duration::from_millis(70),
                fade_out: Duration::from_millis(280),
            },
        }
    }

    /// The run one beat of a failure's double pulse takes: shorter in every
    /// part, the way a warning reads faster than an acknowledgement.
    fn failure_run(self) -> Run {
        let run = self.success_run();
        Run {
            fade_in: run.fade_in / 3,
            hold: FAILURE_BEAT.max(run.hold / 3),
            fade_out: run.fade_out / 3,
        }
    }

    /// How bright the run is `elapsed` in: rising over the fade-in, whole
    /// across the hold, then leaving on a curve the mode owns.
    fn intensity(self, run: &Run, elapsed: Duration) -> f32 {
        if elapsed < run.fade_in {
            let rise = elapsed.as_secs_f32() / run.fade_in.as_secs_f32();
            return rise * rise * (3.0 - 2.0 * rise);
        }
        if elapsed < run.fade_in + run.hold {
            return 1.0;
        }
        if run.fade_out.is_zero() {
            return 0.0;
        }
        let left = 1.0
            - (elapsed - run.fade_in - run.hold).as_secs_f32() / run.fade_out.as_secs_f32();
        let left = left.clamp(0.0, 1.0);
        match self {
            Self::Normal => 0.0,
            Self::FadeFast | Self::FadeMedium | Self::FadeSlow | Self::Swipe => left * left,
            Self::Zap => left * left * left,
            Self::Steps | Self::Glitch | Self::Wave => left,
        }
    }
}

/// An explicit evaluation waiting for its outcome: the pane that submitted
/// it and the request ids it covers, a whole setup chain included.
pub(super) struct EvaluationFeedback {
    scene: SceneId,
    pane: usize,
    first_request: u64,
    pub(super) last_request: u64,
    flashed_on_submit: bool,
}

/// A flash on its pane: one pulse for a success, two for a failure.
pub(super) struct EvaluationFlash {
    pub(super) scene: SceneId,
    pub(super) pane: usize,
    pub(super) success: bool,
    pub(super) started: Instant,
    /// How the flash moves, read when it began so a change mid-flight does
    /// not retime a run already under way.
    animation: EvalAnimation,
    /// The phase last advanced to; `None` before the first advance.
    phase: Option<u8>,
}

/// An engine answer as feedback: `Some(success)`, or `None` for a cancel.
pub(super) fn outcome_success<T>(result: &Result<T, EngineFailure>) -> Option<bool> {
    match result {
        Ok(_) => Some(true),
        Err(failure) if failure.kind == "cancelled" => None,
        Err(_) => Some(false),
    }
}

impl EvaluationFlash {
    fn run(&self) -> Run {
        if self.success {
            self.animation.success_run()
        } else {
            self.animation.failure_run()
        }
    }

    fn phase_at(&self, now: Instant) -> u8 {
        let elapsed = now.saturating_duration_since(self.started);
        let slot = self.run().total();
        if slot.is_zero() {
            return 3;
        }
        if self.success {
            u8::from(elapsed >= slot)
        } else {
            (elapsed.as_millis() / slot.as_millis()).min(3) as u8
        }
    }

    /// Whether the flash draws at `now`.
    pub(super) fn is_lit(&self, now: Instant) -> bool {
        let phase = self.phase_at(now);
        phase == 0 || (!self.success && phase == 2)
    }

    /// When the flash next changes after the phase last advanced to.
    pub(super) fn next_transition(&self) -> Instant {
        self.started
            + if self.success {
                self.run().total()
            } else {
                self.run().total() * u32::from(self.phase.unwrap_or(0) + 1)
            }
    }

    /// The flash as the pane should draw it at `now`: the animation in
    /// force, how far through its run, and how bright - or `None` while
    /// dark, which is most of a failure's pattern and all of never.
    pub(super) fn drawing_at(&self, now: Instant) -> Option<(EvalAnimation, f32, f32)> {
        if !self.is_lit(now) {
            return None;
        }
        let run = self.run();
        let slot = run.total();
        let mut elapsed = now.saturating_duration_since(self.started);
        if !self.success {
            let slot_millis = slot.as_millis().max(1);
            elapsed -= slot * u32::try_from(elapsed.as_millis() / slot_millis).unwrap_or(0);
        }
        let progress = (elapsed.as_secs_f32() / slot.as_secs_f32()).clamp(0.0, 1.0);
        Some((self.animation, progress, self.animation.intensity(&run, elapsed)))
    }

    fn allowed(&self, mode: EvaluationFlashMode) -> bool {
        match mode {
            EvaluationFlashMode::Full => true,
            EvaluationFlashMode::OnSuccess => self.success,
            EvaluationFlashMode::Off => false,
        }
    }
}

impl App {
    /// Starts feedback for an evaluation from the focused pane; full mode
    /// flashes it at once.
    pub(super) fn begin_evaluation_feedback(&mut self) {
        self.evaluation_flash = None;
        let scene = self.scenes.current().id;
        let pane = self.focused;
        let flashed_on_submit = self.ui_settings.evaluation_flash == EvaluationFlashMode::Full;
        self.evaluation_feedback = Some(EvaluationFeedback {
            scene,
            pane,
            first_request: self.next_request_id,
            last_request: self.next_request_id,
            flashed_on_submit,
        });
        if flashed_on_submit {
            self.show_evaluation_flash(scene, pane, true);
        }
        self.dirty_frame = true;
    }

    /// Ends the pending feedback with its outcome. A failure flashes; a
    /// success flashes unless its submission already did.
    pub(super) fn finish_evaluation_feedback(&mut self, success: bool) {
        let Some(feedback) = self.evaluation_feedback.take() else {
            return;
        };
        if !success || !feedback.flashed_on_submit {
            self.show_evaluation_flash(feedback.scene, feedback.pane, success);
        }
    }

    fn show_evaluation_flash(&mut self, scene: SceneId, pane: usize, success: bool) {
        let now = Instant::now();
        let flash = EvaluationFlash {
            scene,
            pane,
            success,
            started: now,
            animation: self.ui_settings.evaluation_animation,
            phase: None,
        };
        if flash.allowed(self.ui_settings.evaluation_flash) {
            self.evaluation_flash = Some(flash);
            self.next_frame = now;
            self.dirty_frame = true;
        }
    }

    /// Settles the pending feedback with the outcome of one of its requests:
    /// a failure ends it at once, a success only at its last request, and a
    /// cancel (`None`) retires it without a flash.
    pub(super) fn finish_evaluation_request(&mut self, request_id: u64, success: Option<bool>) {
        let Some(feedback) = &self.evaluation_feedback else {
            return;
        };
        if !(feedback.first_request..=feedback.last_request).contains(&request_id) {
            return;
        }
        match success {
            Some(false) => self.finish_evaluation_feedback(false),
            Some(true) if request_id == feedback.last_request => {
                self.finish_evaluation_feedback(true);
            }
            None => self.evaluation_feedback = None,
            Some(true) => {}
        }
    }

    /// Advances the flash to `now`, asking for a repaint at each pulse
    /// transition and at expiry whatever the frame rate. Returns whether a
    /// flash is still active: while one is, the loop keeps painting at its
    /// configured frame rate, which is what moves an animation along.
    pub(super) fn advance_evaluation_flash(&mut self, now: Instant) -> bool {
        let Some(flash) = &mut self.evaluation_flash else {
            return false;
        };
        let phase = flash.phase_at(now);
        let expired = phase >= if flash.success { 1 } else { 3 }
            || !flash.allowed(self.ui_settings.evaluation_flash);
        if expired || flash.phase != Some(phase) {
            self.dirty_frame = true;
            self.next_frame = self.next_frame.min(now);
            flash.phase = Some(phase);
        }
        if expired {
            self.evaluation_flash = None;
        }
        !expired
    }
}

#[cfg(test)]
mod tests {
    mod evaluation_flash {
        //! Evaluation gestures flash their own editor according to their outcome.

        use crate::app::tests::support::{app_over, sounding};
        use crate::app::*;
        use crate::settings::{EvalAnimation, EvaluationFlashMode};

        const BROKEN: &str = "$: s(\"bd\").orbit(3) (((";
        const SCORE: &str = "$: note(60).s(\"sine\")";

        fn evaluate(app: &mut App, source: &str) -> u64 {
            app.scenes.current_mut().editor = Editor::new(source).unwrap();
            let request_id = app.next_request_id;
            app.handle_editor_effect(EditorEffect::Evaluate {
                revision: app.editor().revision(),
                source: ropey::Rope::from_str(source),
            })
            .unwrap();
            request_id
        }

        fn answer(app: &mut App, request_id: u64, success: Option<bool>) {
            let result = match success {
                Some(true) => Ok(crate::StudioInstall {
                    generation: 1,
                    source_revision: source_revision(SCORE),
                    pending_cutover: false,
                    answered_repeat: false,
                    held: false,
                }),
                success => Err(crate::EngineFailure {
                    kind: if success.is_none() {
                        "cancelled"
                    } else {
                        "evaluation"
                    }
                    .into(),
                    message: "the evaluation did not install".into(),
                    recoverable: true,
                    playback_stopped: false,
                }),
            };
            app.handle_evaluation(EvaluationOutcome {
                request_id,
                editor_revision: app.editor().revision().0,
                result,
            });
        }

        fn flash_started(app: &App) -> Option<Instant> {
            app.evaluation_flash.as_ref().map(|flash| flash.started)
        }

        #[test]
        fn syntax_refusals_flash_only_in_full_mode() {
            for mode in [
                EvaluationFlashMode::Full,
                EvaluationFlashMode::OnSuccess,
                EvaluationFlashMode::Off,
            ] {
                let directory = tempfile::tempdir().unwrap();
                let mut app = app_over(directory.path());
                app.ui_settings.evaluation_flash = mode;

                let request_id = evaluate(&mut app, BROKEN);

                assert_eq!(app.next_request_id, request_id, "the text was refused");
                assert!(app.status.starts_with("refused - "), "{}", app.status);
                assert_eq!(
                    app.evaluation_flash.as_ref().map(|flash| flash.success),
                    (mode == EvaluationFlashMode::Full).then_some(false),
                    "{mode:?}"
                );
            }
        }

        #[test]
        fn engine_outcomes_obey_the_flash_mode() {
            for mode in [
                EvaluationFlashMode::Full,
                EvaluationFlashMode::OnSuccess,
                EvaluationFlashMode::Off,
            ] {
                for success in [true, false] {
                    let directory = tempfile::tempdir().unwrap();
                    let mut app = app_over(directory.path());
                    app.ui_settings.evaluation_flash = mode;
                    let request_id = evaluate(&mut app, SCORE);
                    assert_eq!(app.next_request_id, request_id + 1);
                    let submitted = flash_started(&app);
                    assert_eq!(submitted.is_some(), mode == EvaluationFlashMode::Full);

                    answer(&mut app, request_id, Some(success));

                    let visible = mode == EvaluationFlashMode::Full
                        || (mode == EvaluationFlashMode::OnSuccess && success);
                    assert_eq!(
                        app.evaluation_flash.as_ref().map(|flash| flash.success),
                        visible.then_some(success),
                        "{mode:?}, success={success}"
                    );
                    if mode == EvaluationFlashMode::Full && success {
                        assert_eq!(
                            flash_started(&app),
                            submitted,
                            "success does not flash twice"
                        );
                    }
                }
            }
        }

        #[test]
        fn unchanged_and_already_armed_evaluations_still_flash_as_successes() {
            for armed in [false, true] {
                let directory = tempfile::tempdir().unwrap();
                let mut app = app_over(directory.path());
                app.ui_settings.evaluation_flash = EvaluationFlashMode::OnSuccess;
                if armed {
                    app.armed_revision = Some(source_revision(SCORE));
                } else {
                    app.snapshot = Some(sounding(true, false));
                    app.audible_scene = Some(app.scenes.current().id);
                    app.installed_revision = Some(source_revision(SCORE));
                }

                let request_id = evaluate(&mut app, SCORE);

                assert_eq!(app.next_request_id, request_id, "no new install is needed");
                assert!(app.evaluation_flash.as_ref().unwrap().success);
                assert!(
                    app.status
                        .starts_with(if armed { "already armed" } else { "unchanged" }),
                    "{}",
                    app.status
                );
            }
        }

        #[test]
        fn cancelled_and_unknown_answers_do_not_restart_or_steal_a_pending_gesture() {
            let directory = tempfile::tempdir().unwrap();
            let mut app = app_over(directory.path());
            let first = evaluate(&mut app, SCORE);
            let first_started = flash_started(&app);

            answer(&mut app, first + 10, Some(false));
            assert_eq!(flash_started(&app), first_started);
            answer(&mut app, first, None);
            assert_eq!(flash_started(&app), first_started);

            let second = evaluate(&mut app, SCORE);
            let second_started = flash_started(&app);
            answer(&mut app, first, Some(false));
            assert_eq!(flash_started(&app), second_started);
            answer(&mut app, second, Some(true));
            assert!(app.evaluation_flash.as_ref().unwrap().success);
            assert_eq!(flash_started(&app), second_started);
        }

        #[test]
        fn background_score_installations_do_not_flash() {
            for success in [true, false] {
                let directory = tempfile::tempdir().unwrap();
                let mut app = app_over(directory.path());
                let request_id = app.next_request_id;
                app.queue_evaluation_for(
                    app.scenes.current().id,
                    app.editor().revision(),
                    Arc::from(SCORE),
                    false,
                );

                answer(&mut app, request_id, Some(success));

                assert!(app.evaluation_flash.is_none());
            }
        }

        #[test]
        fn background_outcomes_cannot_complete_a_manual_evaluation() {
            for (mode, success) in [
                (EvaluationFlashMode::Full, false),
                (EvaluationFlashMode::OnSuccess, true),
            ] {
                let directory = tempfile::tempdir().unwrap();
                let mut app = app_over(directory.path());
                app.ui_settings.evaluation_flash = mode;
                let manual = evaluate(&mut app, SCORE);
                let submitted = flash_started(&app);
                let background = app.next_request_id;
                app.queue_evaluation_for(
                    app.scenes.current().id,
                    app.editor().revision(),
                    Arc::from(SCORE),
                    false,
                );

                answer(&mut app, background, Some(false));
                assert_eq!(flash_started(&app), submitted);
                answer(&mut app, manual, Some(success));
                assert_eq!(app.evaluation_flash.as_ref().unwrap().success, success);
            }
        }

        #[test]
        fn a_manual_prebake_chain_updates_its_flash_only_for_the_final_outcome() {
            for (mode, fail_at) in [EvaluationFlashMode::Full, EvaluationFlashMode::OnSuccess]
                .into_iter()
                .flat_map(|mode| {
                    [None, Some(PrebakeScope::Global), Some(PrebakeScope::Local)]
                        .map(|fail_at| (mode, fail_at))
                })
            {
                let directory = tempfile::tempdir().unwrap();
                let mut app = app_over(directory.path());
                app.ui_settings.evaluation_flash = mode;
                app.scenes.set_local_prebake("globalThis.localHelper = 2");
                app.open_prebake(PrebakeScope::Global);
                let first = evaluate(&mut app, "globalThis.globalHelper = 1");
                assert_eq!(app.next_request_id, first + 2);
                let submitted = flash_started(&app);
                assert_eq!(submitted.is_some(), mode == EvaluationFlashMode::Full);

                for (offset, scope) in PrebakeScope::ALL.into_iter().enumerate() {
                    let failed = fail_at == Some(scope);
                    app.handle_prebake(PrebakeOutcome {
                        request_id: first + offset as u64,
                        scope,
                        result: if failed {
                            Err(crate::EngineFailure {
                                kind: "evaluation".into(),
                                message: "setup did not apply".into(),
                                recoverable: true,
                                playback_stopped: false,
                            })
                        } else {
                            Ok(())
                        },
                    });
                    if failed {
                        assert_eq!(
                            app.evaluation_flash.as_ref().map(|flash| flash.success),
                            (mode == EvaluationFlashMode::Full).then_some(false)
                        );
                        break;
                    }
                    assert_eq!(
                        app.evaluation_flash.as_ref().map(|flash| flash.success),
                        (mode == EvaluationFlashMode::Full || scope == PrebakeScope::Local)
                            .then_some(true)
                    );
                    if mode == EvaluationFlashMode::Full {
                        assert_eq!(flash_started(&app), submitted);
                    }
                }
            }
        }

        #[test]
        fn an_empty_prebake_evaluation_flashes_as_a_success() {
            let directory = tempfile::tempdir().unwrap();
            let mut app = app_over(directory.path());
            app.open_prebake(PrebakeScope::Local);

            let request_id = evaluate(&mut app, "");

            assert_eq!(app.next_request_id, request_id);
            assert!(app.evaluation_flash.as_ref().unwrap().success);
        }

        /// A refusal flashes, on screen, the pane that submitted it rather than the
        /// pane that is playing.
        #[test]
        fn a_refusal_flashes_the_submitting_pane_while_another_pane_is_playing() {
            let directory = tempfile::tempdir().unwrap();
            let mut app = app_over(directory.path());
            app.scenes.current_mut().editor = Editor::new(SCORE).unwrap();
            app.toggle_split();
            app.playing_pane = Some(0);
            let scene = app.scenes.current().id;

            evaluate(&mut app, BROKEN);

            let flash = app.evaluation_flash.as_mut().unwrap();
            assert_eq!(flash.scene, scene);
            assert_eq!(flash.pane, 1);
            assert_eq!(app.playing_pane, Some(0));

            // Held in its first pulse for as long as the paint takes.
            flash.started = Instant::now() + Duration::from_secs(3600);
            let mut terminal =
                ratatui::Terminal::new(ratatui::backend::TestBackend::new(120, 30)).unwrap();
            let mut reversed_per_pane = |app: &mut App| {
                terminal
                    .draw(|frame| app.paint_into(frame, Instant::now()).unwrap())
                    .unwrap();
                let buffer = terminal.backend().buffer();
                app.regions
                    .panes
                    .iter()
                    .map(|pane| {
                        let area = pane.editor;
                        (area.top()..area.bottom())
                            .flat_map(|y| (area.left()..area.right()).map(move |x| (x, y)))
                            .filter(|&(x, y)| {
                                buffer[(x, y)]
                                    .modifier
                                    .contains(ratatui::style::Modifier::REVERSED)
                            })
                            .count()
                    })
                    .collect::<Vec<_>>()
            };
            let lit = reversed_per_pane(&mut app);
            app.evaluation_flash = None;
            let unlit = reversed_per_pane(&mut app);
            assert_eq!(lit.len(), 2);
            assert_eq!(lit[0], unlit[0], "the playing pane does not flash");
            assert!(lit[1] > unlit[1], "the submitting pane flashes");
        }

        #[test]
        fn a_delayed_failure_keeps_its_submitting_scene_and_pane_after_focus_moves() {
            let directory = tempfile::tempdir().unwrap();
            let mut app = app_over(directory.path());
            app.toggle_split();
            let scene = app.scenes.current().id;
            let request_id = evaluate(&mut app, SCORE);
            app.focus_pane(0);

            answer(&mut app, request_id, Some(false));

            let flash = app.evaluation_flash.as_ref().unwrap();
            assert_eq!(flash.scene, scene);
            assert_eq!(flash.pane, 1);
            assert_eq!(app.focused, 0);
        }

        #[test]
        fn a_failure_has_two_quick_pulses_and_each_transition_requests_a_frame() {
            let directory = tempfile::tempdir().unwrap();
            let mut app = app_over(directory.path());
            evaluate(&mut app, BROKEN);
            let started = app.evaluation_flash.as_ref().unwrap().started;

            for (elapsed_ms, lit, next_ms) in [(0, true, 70), (70, false, 140), (140, true, 210)] {
                let now = started + Duration::from_millis(elapsed_ms);
                app.dirty_frame = false;
                app.next_frame = now + Duration::from_millis(125);

                assert!(app.advance_evaluation_flash(now));

                let flash = app.evaluation_flash.as_ref().unwrap();
                assert_eq!(flash.is_lit(now), lit);
                assert_eq!(
                    flash.next_transition(),
                    started + Duration::from_millis(next_ms)
                );
                assert!(
                    app.dirty_frame,
                    "pulse transitions repaint at low frame rates"
                );
                assert_eq!(app.next_frame, now);

                app.dirty_frame = false;
                assert!(app.advance_evaluation_flash(now + Duration::from_millis(69)));
                assert!(
                    !app.dirty_frame,
                    "a phase does not continuously request frames"
                );
            }

            let expired = started + Duration::from_millis(210);
            app.next_frame = expired + Duration::from_millis(125);
            assert!(!app.advance_evaluation_flash(expired));
            assert!(app.evaluation_flash.is_none());
            assert!(app.dirty_frame, "expiry removes the final visible pulse");
            assert_eq!(app.next_frame, expired);
        }

        #[test]
        fn a_success_is_one_two_hundred_millisecond_pulse() {
            let directory = tempfile::tempdir().unwrap();
            let mut app = app_over(directory.path());
            let request_id = evaluate(&mut app, SCORE);
            answer(&mut app, request_id, Some(true));
            let started = app.evaluation_flash.as_ref().unwrap().started;

            assert!(app.advance_evaluation_flash(started));
            let flash = app.evaluation_flash.as_ref().unwrap();
            assert!(flash.is_lit(started + Duration::from_millis(199)));
            assert!(!flash.is_lit(started + Duration::from_millis(200)));
            assert_eq!(
                flash.next_transition(),
                started + Duration::from_millis(200)
            );

            assert!(!app.advance_evaluation_flash(started + Duration::from_millis(200)));
            assert!(app.evaluation_flash.is_none());
        }

        /// An animation stretches the window the flash is lit over and
        /// gives the pane a brightness that rises and falls inside it,
        /// while the plain look stays exactly what it was.
        #[test]
        fn an_animated_flash_runs_longer_and_fades_rather_than_cutting() {
            // The look, how long it stays lit, and the middle of its hold.
            for (animation, lit_ms, hold_mid_ms) in [
                (EvalAnimation::Normal, 200, 100),
                (EvalAnimation::FadeFast, 340, 125),
                (EvalAnimation::FadeMedium, 640, 185),
                (EvalAnimation::FadeSlow, 1040, 245),
                (EvalAnimation::Swipe, 630, 80),
                (EvalAnimation::Zap, 180, 30),
                (EvalAnimation::Steps, 360, 45),
                (EvalAnimation::Glitch, 260, 20),
                (EvalAnimation::Wave, 380, 65),
            ] {
                let directory = tempfile::tempdir().unwrap();
                let mut app = app_over(directory.path());
                app.ui_settings.evaluation_animation = animation;
                let request_id = evaluate(&mut app, SCORE);
                answer(&mut app, request_id, Some(true));
                let started = app.evaluation_flash.as_ref().unwrap().started;
                let flash = app.evaluation_flash.as_ref().unwrap();

                assert!(
                    flash.is_lit(started + Duration::from_millis(lit_ms - 1)),
                    "{animation:?} stays lit to its end"
                );
                assert!(
                    !flash.is_lit(started + Duration::from_millis(lit_ms)),
                    "{animation:?} ends at {lit_ms}ms"
                );
                assert_eq!(
                    flash.next_transition(),
                    started + Duration::from_millis(lit_ms),
                    "{animation:?} hands the loop its end"
                );

                let (_, _, whole) = flash
                    .drawing_at(started + Duration::from_millis(hold_mid_ms))
                    .unwrap_or_else(|| panic!("{animation:?} draws mid-hold"));
                assert_eq!(whole, 1.0, "{animation:?} is whole across its hold");
                let (_, _, late) = flash
                    .drawing_at(started + Duration::from_millis(lit_ms - 20))
                    .unwrap_or_else(|| panic!("{animation:?} draws near its end"));
                if animation != EvalAnimation::Normal {
                    assert!(
                        late < 0.5,
                        "{animation:?}: {late} near the end, down from whole"
                    );
                } else {
                    assert_eq!(late, 1.0, "the plain look holds whole to the cut");
                }
                assert!(
                    flash.drawing_at(started + Duration::from_millis(lit_ms + 5)).is_none(),
                    "{animation:?} draws nothing once dark"
                );
            }

            // Normal arrives whole, the moment it starts: no run-in, no
            // ramp, the look the reverse modifier always made.
            let directory = tempfile::tempdir().unwrap();
            let mut app = app_over(directory.path());
            let request_id = evaluate(&mut app, SCORE);
            answer(&mut app, request_id, Some(true));
            let started = app.evaluation_flash.as_ref().unwrap().started;
            let flash = app.evaluation_flash.as_ref().unwrap();
            let (animation, _, intensity) = flash.drawing_at(started).unwrap();
            assert_eq!(animation, EvalAnimation::Normal);
            assert_eq!(intensity, 1.0);
        }

        /// A fade rises from nothing: the code is read at the start, then
        /// the light comes up over it.
        #[test]
        fn a_fade_begins_dark_and_climbs() {
            let directory = tempfile::tempdir().unwrap();
            let mut app = app_over(directory.path());
            app.ui_settings.evaluation_animation = EvalAnimation::FadeMedium;
            let request_id = evaluate(&mut app, SCORE);
            answer(&mut app, request_id, Some(true));
            let started = app.evaluation_flash.as_ref().unwrap().started;
            let flash = app.evaluation_flash.as_ref().unwrap();

            let (_, _, start) = flash.drawing_at(started).unwrap();
            let (_, _, quarter) = flash.drawing_at(started + Duration::from_millis(20)).unwrap();
            let (_, _, half) = flash.drawing_at(started + Duration::from_millis(40)).unwrap();
            let (_, _, whole) = flash.drawing_at(started + Duration::from_millis(100)).unwrap();
            assert_eq!(start, 0.0, "the fade begins dark");
            assert!(quarter < half && half < whole, "{quarter} {half} {whole}");
            assert_eq!(whole, 1.0, "the hold is whole");
        }

        /// A failure keeps its double pulse under every animation, quicker
        /// in every part: the warning still reads faster than the nod.
        #[test]
        fn every_animation_keeps_the_failure_double_pulse() {
            for animation in [
                EvalAnimation::Normal,
                EvalAnimation::FadeFast,
                EvalAnimation::FadeMedium,
                EvalAnimation::FadeSlow,
                EvalAnimation::Zap,
                EvalAnimation::Steps,
                EvalAnimation::Glitch,
                EvalAnimation::Wave,
            ] {
                let directory = tempfile::tempdir().unwrap();
                let mut app = app_over(directory.path());
                app.ui_settings.evaluation_animation = animation;
                evaluate(&mut app, BROKEN);
                let started = app.evaluation_flash.as_ref().unwrap().started;
                let flash = app.evaluation_flash.as_ref().unwrap();

                assert!(
                    flash.drawing_at(started).is_some(),
                    "{animation:?} flashes at once"
                );
                // The dark gap between the two pulses is real, and the
                // second pulse relights after it.
                let gap_start = dark_again(&app, started);
                let relit = lit_again(&app, gap_start);
                assert!(gap_start > started, "{animation:?} has a dark gap");
                assert!(relit > gap_start, "{animation:?} pulses twice");
                assert!(
                    flash.is_lit(relit) && !flash.is_lit(gap_start),
                    "{animation:?}: dark at the gap, lit on the second pulse"
                );
            }
        }

        /// The first instant the flash goes dark after `from`, stepped
        /// coarsely: the pulses are tens of milliseconds, the step can be
        /// one.
        fn dark_again(app: &App, from: Instant) -> Instant {
            let flash = app.evaluation_flash.as_ref().unwrap();
            (1..4000)
                .map(|ms| from + Duration::from_millis(ms))
                .find(|now| !flash.is_lit(*now))
                .unwrap_or_else(|| panic!("never dark after {from:?}"))
        }

        fn lit_again(app: &App, from: Instant) -> Instant {
            let flash = app.evaluation_flash.as_ref().unwrap();
            (1..4000)
                .map(|ms| from + Duration::from_millis(ms))
                .find(|now| flash.is_lit(*now))
                .expect("the second pulse comes")
        }

        #[test]
        fn a_quantized_evaluation_flashes_immediately_in_full_mode_without_repeating_on_success() {
            let directory = tempfile::tempdir().unwrap();
            let mut app = app_over(directory.path());
            app.snapshot = Some(sounding(true, false));
            app.next_launch = Launch::Quantised { unit_cycles: 4.0 };

            let request_id = evaluate(&mut app, SCORE);

            assert_eq!(app.armed_request, Some(request_id));
            let started =
                flash_started(&app).expect("the evaluation gesture is acknowledged immediately");
            assert!(!app.advance_evaluation_flash(started + Duration::from_millis(200)));

            answer(&mut app, request_id, Some(true));

            assert!(
                app.evaluation_flash.is_none(),
                "the delayed success does not flash again"
            );
        }
    }
}
