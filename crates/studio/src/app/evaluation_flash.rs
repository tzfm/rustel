//! Feedback for explicit evaluations, tied to their submitting pane and result.

use super::super::settings::EvaluationFlashMode;
use super::super::worker::EngineFailure;
use super::*;

/// How long a success stays lit.
const SUCCESS_DURATION: Duration = Duration::from_millis(200);
/// One lit or dark beat of a failure's two pulses.
const FAILURE_BEAT: Duration = Duration::from_millis(70);

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
    fn phase_at(&self, now: Instant) -> u8 {
        let elapsed = now.saturating_duration_since(self.started);
        if self.success {
            u8::from(elapsed >= SUCCESS_DURATION)
        } else {
            (elapsed.as_millis() / FAILURE_BEAT.as_millis()).min(3) as u8
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
                SUCCESS_DURATION
            } else {
                FAILURE_BEAT * u32::from(self.phase.unwrap_or(0) + 1)
            }
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
    /// flash is still active.
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
