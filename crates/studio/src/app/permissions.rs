//! The Permissions page: the origins a score imports scripts from, and
//! the native orchestra switch.
//!
//! The engine refuses `await import('https://...')` from an origin that is
//! not trusted, and an orchestra while native execution is not permitted.
//! The page reads as a switch: Enter permits a refused row, and Space flips
//! a row where it stands, so a permission can be taken back without the row
//! leaving the page. A refused or turned-off origin stays listed until the
//! studio closes; an origin trust is kept in the settings, while the
//! orchestra switch lasts one run.

use super::super::settings::PermissionRow;
use super::super::worker::EngineFailure;
use super::App;
use super::transport::ErrorOwner;

/// The most refused origins the page keeps. A score names one or two.
const MAX_BLOCKED_IMPORTS: usize = 16;

impl App {
    /// What a refused import says on the footer. The origin goes on the
    /// permissions page. `None` for every other failure.
    pub(super) fn note_untrusted_import(&mut self, failure: &EngineFailure) -> Option<String> {
        if failure.kind != "untrusted-import" {
            return None;
        }
        // The engine's message opens with the origin.
        let origin = failure.message.split_whitespace().next()?;
        let origin = rustel_runtime::import_origin(origin).ok()?;
        // A refusal for an origin on the saved list is stale.
        if !self.prefs.import_origins.contains(&origin) {
            self.blocked_imports.retain(|blocked| *blocked != origin);
            if self.blocked_imports.len() >= MAX_BLOCKED_IMPORTS {
                self.blocked_imports.remove(0);
            }
            self.blocked_imports.push(origin.clone());
        }
        Some(format!(
            "{origin} is not trusted - Settings → Permissions lets its scripts run"
        ))
    }

    /// What a refused orchestra says on the footer. The switch goes on the
    /// permissions page. `None` for every other failure.
    pub(super) fn note_csound_refusal(&mut self, failure: &EngineFailure) -> Option<String> {
        if failure.kind != "csound-permission" {
            return None;
        }
        self.csound_refused = true;
        Some(
            "Csound is not permitted - Settings → Permissions lets native orchestras run"
                .to_owned(),
        )
    }

    /// The rows of the permissions page: the trusted origins, then the
    /// refused, then the orchestra switch once the studio has a reason to
    /// show it.
    pub(super) fn permission_rows(&self) -> Vec<PermissionRow> {
        let mut rows: Vec<PermissionRow> = self
            .prefs
            .import_origins
            .iter()
            .map(|origin| PermissionRow::Origin {
                origin: origin.clone(),
                allowed: true,
            })
            .chain(
                self.blocked_imports
                    .iter()
                    .map(|origin| PermissionRow::Origin {
                        origin: origin.clone(),
                        allowed: false,
                    }),
            )
            .collect();
        if self.csound_allowed || self.csound_refused {
            rows.push(PermissionRow::Csound {
                allowed: self.csound_allowed,
            });
        }
        rows
    }

    /// How many rows the permissions page is showing. The sheet counts its
    /// keys by this, and the pointer counts its hits.
    pub(super) fn permission_row_count(&self) -> usize {
        self.prefs.import_origins.len()
            + self.blocked_imports.len()
            + usize::from(self.csound_allowed || self.csound_refused)
    }

    /// Enter on a row of the permissions page.
    pub(super) fn permit_row(&mut self, at: usize) {
        match self.permission_rows().into_iter().nth(at) {
            Some(PermissionRow::Origin {
                origin,
                allowed: true,
            }) => {
                self.status = format!("{origin} is trusted · Space turns it off");
            }
            Some(PermissionRow::Origin {
                origin,
                allowed: false,
            }) => {
                self.blocked_imports.retain(|blocked| *blocked != origin);
                if !self.prefs.import_origins.contains(&origin) {
                    self.prefs.import_origins.push(origin.clone());
                    self.import_origins_changed();
                }
                // The refusal on the footer names an origin that is trusted
                // now, and the update it refused runs again by itself.
                let mut unblocked = false;
                for owner in [ErrorOwner::Evaluation, ErrorOwner::Setup] {
                    if self
                        .errors
                        .get(owner)
                        .is_some_and(|message| message.contains(&origin))
                    {
                        self.clear_error(owner);
                        unblocked = true;
                    }
                }
                if unblocked {
                    self.refused_update_owed = true;
                    self.status = format!("{origin} trusted · updating the score");
                } else {
                    self.status = format!("{origin} trusted");
                }
            }
            Some(PermissionRow::Csound { allowed: true }) => {
                self.status = "Csound is on · Space turns it off".into();
            }
            Some(PermissionRow::Csound { allowed: false }) => {
                if self.set_csound(true) {
                    self.status = "Csound on · updating the score".into();
                } else {
                    self.status = "Csound on".into();
                }
            }
            None => {}
        }
    }

    /// Space on a row of the permissions page: permit a refused row, or
    /// take the permission back where the row stands. The row stays on the
    /// page, so the permission can come back with one more Space.
    pub(super) fn toggle_row(&mut self, at: usize) {
        match self.permission_rows().into_iter().nth(at) {
            Some(PermissionRow::Origin { allowed: false, .. }) => self.permit_row(at),
            Some(PermissionRow::Origin {
                origin,
                allowed: true,
            }) => {
                self.prefs.import_origins.retain(|kept| *kept != origin);
                self.import_origins_changed();
                // The row stays: it moves to the refused ones, which the
                // studio keeps until it closes, so a permission taken back
                // can be given again.
                self.blocked_imports.retain(|blocked| *blocked != origin);
                if self.blocked_imports.len() >= MAX_BLOCKED_IMPORTS {
                    self.blocked_imports.remove(0);
                }
                self.blocked_imports.push(origin.clone());
                self.status = format!("{origin} off · its scripts no longer load");
            }
            Some(PermissionRow::Csound { allowed }) => {
                let unblocked = self.set_csound(!allowed);
                if allowed {
                    // The switch went off; the row stays so a second
                    // Space brings it back, as an origin's does.
                    self.csound_refused = true;
                }
                self.status = if allowed {
                    "Csound off · new orchestra use is blocked".into()
                } else if unblocked {
                    "Csound on · updating the score".into()
                } else {
                    "Csound on".into()
                };
            }
            None => {}
        }
    }

    /// Delete on a row of the permissions page.
    pub(super) fn forget_row(&mut self, at: usize) {
        match self.permission_rows().into_iter().nth(at) {
            Some(PermissionRow::Origin {
                origin,
                allowed: true,
            }) => {
                self.prefs.import_origins.retain(|kept| *kept != origin);
                self.import_origins_changed();
                self.status = format!("{origin} removed · its scripts no longer load");
            }
            Some(PermissionRow::Origin {
                origin,
                allowed: false,
            }) => {
                self.blocked_imports.retain(|blocked| *blocked != origin);
                self.status = format!("{origin} dismissed");
            }
            Some(PermissionRow::Csound { .. }) => {
                // The switch leaves the page with its refusal, off either
                // way: a score brings it back by asking again.
                self.set_csound(false);
                self.csound_refused = false;
                self.status = "Csound off".into();
            }
            None => {}
        }
    }

    /// Flip the orchestra switch and tell the engine. The refusal on the
    /// footer names a permission the score has now, so the update it
    /// refused runs again: the answer says whether one was unblocked.
    fn set_csound(&mut self, allowed: bool) -> bool {
        self.csound_allowed = allowed;
        let mut unblocked = false;
        if allowed {
            self.csound_refused = false;
            for owner in [ErrorOwner::Evaluation, ErrorOwner::Setup] {
                if self
                    .errors
                    .get(owner)
                    .is_some_and(|message| message.contains("Csound is not permitted"))
                {
                    self.clear_error(owner);
                    unblocked = true;
                }
            }
            if unblocked {
                self.refused_update_owed = true;
            }
        }
        self.csound_owed = true;
        self.send_csound();
        unblocked
    }

    /// Give the engine the orchestra switch, when it does not have it yet.
    pub(super) fn send_csound(&mut self) {
        if self.csound_owed && self.worker.try_set_csound(self.csound_allowed) {
            self.csound_owed = false;
        }
    }

    fn import_origins_changed(&mut self) {
        self.save_prefs_soon();
        self.import_origins_owed = true;
        self.send_import_origins();
    }

    /// Give the engine the trusted origins, when it does not have them yet.
    pub(super) fn send_import_origins(&mut self) {
        if self.import_origins_owed
            && self
                .worker
                .try_set_import_origins(self.prefs.import_origins.clone())
        {
            self.import_origins_owed = false;
        }
    }
}

#[cfg(test)]
mod tests {
    mod permissions {
        //! Tests for `app/permissions.rs`.

        use crate::app::tests::support::*;
        use crate::settings::PermissionRow;

        fn refusal() -> crate::worker::EngineFailure {
            crate::worker::EngineFailure {
                kind: "csound-permission".into(),
                message: "Csound is not permitted. Permit native orchestras with --allow-csound, only for trusted scores".into(),
                recoverable: true,
                playback_stopped: false,
            }
        }

        /// The switch starts on, as the engine's does: an installed library
        /// is the studio's opt-in. Space takes it back for the run, a
        /// refusal keeps the row for Enter, and nothing reaches the
        /// preferences.
        #[test]
        fn the_orchestra_switch_starts_on_and_space_takes_it_back_for_a_run() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = app_over(directory.path());
            assert!(app.csound_allowed);
            assert_eq!(
                app.permission_rows(),
                vec![PermissionRow::Csound { allowed: true }]
            );

            // Space takes the permission back; the row stays for a second
            // Space, and the refusal names the page.
            app.toggle_row(0);
            assert_eq!(
                app.permission_rows(),
                vec![PermissionRow::Csound { allowed: false }]
            );
            let footer = app.note_csound_refusal(&refusal()).expect("a footer");
            assert_eq!(
                footer,
                "Csound is not permitted - Settings → Permissions lets native orchestras run"
            );

            app.permit_row(0);
            assert_eq!(
                app.permission_rows(),
                vec![PermissionRow::Csound { allowed: true }]
            );
            assert!(app.csound_allowed);
            // The row never reaches the preferences.
            assert!(app.prefs.import_origins.is_empty());

            // `d` takes the row off the page, off either way, and a fresh
            // refusal brings it back.
            app.forget_row(0);
            assert_eq!(app.permission_rows(), Vec::new());
            assert!(!app.csound_allowed);
            assert!(app.note_csound_refusal(&refusal()).is_some());
            assert_eq!(
                app.permission_rows(),
                vec![PermissionRow::Csound { allowed: false }]
            );
        }

        /// Enter on the switch after a refusal re-runs the refused update by
        /// itself, so permitting is the whole walk. The re-run is owed, not
        /// fired: the engine must hold the permission first, or the re-run
        /// would be refused by the state it is running to use.
        #[test]
        fn a_permit_that_unblocked_a_refused_update_runs_it_again() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = app_over(directory.path());
            // Space takes the switch back, as a player does who wants no
            // native code this run.
            app.toggle_row(0);
            assert!(!app.csound_allowed);
            // The way a refused orchestra leaves the studio: the footer
            // names the page, and the failed update is remembered against
            // the text still in the buffer.
            let scene = app.scenes.current().id;
            let revision = app.editor().revision();
            app.set_error(
                crate::app::transport::ErrorOwner::Evaluation,
                "Csound is not permitted - Settings → Permissions lets native orchestras run"
                    .into(),
            );
            app.update_failure = Some((scene, revision));

            app.permit_row(0);
            assert!(app.csound_allowed);
            assert!(app.refused_update_owed, "the re-run is owed");
            assert!(app.status.contains("updating the score"), "{}", app.status);
            assert!(
                app.pending_evaluation.is_none(),
                "nothing ran before the engine holds the permission"
            );

            // The event loop's half: the switch reaches the engine, then the
            // refused update runs again from its buffer.
            app.send_csound();
            assert!(!app.csound_owed);
            app.retry_refused_update();
            let rerun = app
                .inflight
                .values()
                .find(|pending| pending.scene == scene)
                .expect("the refused update re-ran");
            assert_eq!(rerun.editor_revision, revision);
        }

        /// An edit after the refusal owns the next run: the re-run touches
        /// only the text that was refused, still in the buffer.
        #[test]
        fn a_permit_does_not_re_run_a_score_edited_since_the_refusal() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = app_over(directory.path());
            app.toggle_row(0);
            let scene = app.scenes.current().id;
            let revision = app.editor().revision();
            app.set_error(
                crate::app::transport::ErrorOwner::Evaluation,
                "Csound is not permitted - Settings → Permissions lets native orchestras run"
                    .into(),
            );
            app.update_failure = Some((scene, revision));

            // The player edits past the refusal before permitting.
            app.permit_row(0);
            type_text(&mut app, "$: s(\"bd, hh\")\n");
            app.retry_refused_update();
            assert!(
                app.pending_evaluation.is_none(),
                "an edited score waits for its own gesture"
            );
        }
    }
}
