//! Retire host state that belonged to a discarded score Session.

use super::*;

impl StudioEngine {
    #[cfg(test)]
    pub(crate) fn inject_panic_for_test(&self, point: rustel_runtime::SessionPanicPoint) {
        self.session.inject_panic_for_test(point);
    }

    pub(super) fn sync_recovered_session(&mut self) {
        let epoch = self.session.recovery_epoch();
        if self.recovery_epoch == epoch {
            return;
        }
        self.recovery_epoch = epoch;
        crate::crash::forget_recovered_panic();
        if let Some(live) = &mut self.live {
            live.producer.recover_after_panic(
                &self.session,
                live.device.generation(),
                "native score panic discarded the pending score",
            );
        }
        self.session
            .midi_input_bus()
            .set_launch_pads(&self.launch_pads);
        self.pending_live_controls.clear();
        self.slider_requery_pending = false;
        self.pending_accepted_audio.clear();
        self.shield_hold.clear();
        self.cancel_pending_launch();
        self.abandon_landing(false, true);
        self.recent_rewind = None;
        self.ui.reset();
        let source = self.session.active_source().unwrap_or_default().to_owned();
        if self.current_score != source
            && self.played_reset_pending
            && self.played_before_preview.is_none()
        {
            self.restore_played_score(&source);
        }
        self.current_score = source;
        self.current_score_names = protected_names(&self.current_score);
        self.score_window_names.clear();
        self.protection_generation = self.protection_generation.wrapping_add(1);
        self.idle_swept = false;
    }
}
