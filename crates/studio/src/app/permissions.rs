//! The Permissions page: the origins a score imports scripts from, what
//! its samples, images and videos read, and the native orchestra switch.
//!
//! The engine refuses `await import('https://...')` from an origin that is
//! not trusted, a sample or media origin outside its policy, and an
//! orchestra while native execution is not permitted. The page reads as a
//! switch: Enter permits a refused row, and Space or either sideways arrow
//! flips a row where it stands, so a permission can be taken back without
//! the row leaving the page. A refused or turned-off origin stays listed
//! until the studio closes. Origin trusts, the CORS switch and the local
//! folder are kept in the settings, while the orchestra switch lasts one
//! run.

use super::super::settings::PermissionRow;
use super::super::worker::EngineFailure;
use super::App;
use super::transport::ErrorOwner;

/// The most refused origins the page keeps of each kind. A score names
/// one or two.
const MAX_BLOCKED_IMPORTS: usize = 16;

/// Put `origin` last on a list of refused ones, once, and keep the list
/// short.
fn note_blocked(blocked: &mut Vec<String>, origin: &str) {
    blocked.retain(|kept| kept != origin);
    if blocked.len() >= MAX_BLOCKED_IMPORTS {
        blocked.remove(0);
    }
    blocked.push(origin.to_owned());
}

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
            note_blocked(&mut self.blocked_imports, &origin);
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

    /// Put the origins the engine refused a score's samples or media from
    /// on the page. The status line names the newest.
    pub(super) fn note_refused_samples(&mut self) {
        let refused = self
            .options
            .session
            .score_sample_access
            .take_refused_origins();
        if let Some(origin) = refused
            .into_iter()
            .filter(|origin| self.note_refused_sample(origin))
            .last()
        {
            self.status =
                format!("{origin} is not trusted for samples - Settings → Permissions trusts it");
        }
    }

    /// Put one refused sample origin on the page. A refusal for an origin
    /// on the saved list is stale: the engine did not have the list yet.
    fn note_refused_sample(&mut self, origin: &str) -> bool {
        if self.prefs.sample_origins.iter().any(|kept| kept == origin) {
            return false;
        }
        note_blocked(&mut self.blocked_samples, origin);
        true
    }

    /// The rows of the permissions page: the trusted script origins, then
    /// the refused, the same for samples and media, the CORS switch, the
    /// local folder, and the orchestra switch once the studio has a reason
    /// to show it.
    pub(super) fn permission_rows(&self) -> Vec<PermissionRow> {
        let origins =
            |trusted: &[String], refused: &[String], row: fn(String, bool) -> PermissionRow| {
                let trusted = trusted.iter().map(move |origin| row(origin.clone(), true));
                let refused = refused.iter().map(move |origin| row(origin.clone(), false));
                trusted.chain(refused).collect::<Vec<_>>()
            };
        let mut rows = origins(
            &self.prefs.import_origins,
            &self.blocked_imports,
            |origin, allowed| PermissionRow::Origin { origin, allowed },
        );
        rows.extend(origins(
            &self.prefs.sample_origins,
            &self.blocked_samples,
            |origin, allowed| PermissionRow::SampleOrigin { origin, allowed },
        ));
        let host = &self.options.session.score_sample_access;
        rows.push(PermissionRow::PublicCors {
            allowed: self.public_cors(),
            fixed: !host.public_cors_origins(),
        });
        rows.push(PermissionRow::LocalFolder {
            folder: self
                .prefs
                .local_sample_root
                .as_deref()
                .filter(|_| !host.has_local_root())
                .map(|folder| super::sets::shorten_home(std::path::Path::new(folder))),
            fixed: host.has_local_root(),
        });
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
            + self.prefs.sample_origins.len()
            + self.blocked_samples.len()
            // The CORS switch and the local folder are always there.
            + 2
            + usize::from(self.csound_allowed || self.csound_refused)
    }

    /// Whether the public CORS default is on: the command line permits it
    /// and the page did not take it back.
    fn public_cors(&self) -> bool {
        self.options
            .session
            .score_sample_access
            .public_cors_origins()
            && !self.prefs.strict_sample_origins
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
                self.status = if self.rerun_refused(|message| message.contains(&origin)) {
                    format!("{origin} trusted · updating the score")
                } else {
                    format!("{origin} trusted")
                };
            }
            Some(PermissionRow::SampleOrigin {
                origin,
                allowed: true,
            }) => {
                self.status = format!("{origin} is trusted for samples · Space turns it off");
            }
            Some(PermissionRow::SampleOrigin {
                origin,
                allowed: false,
            }) => {
                self.blocked_samples.retain(|blocked| *blocked != origin);
                if !self.prefs.sample_origins.contains(&origin) {
                    self.prefs.sample_origins.push(origin.clone());
                    self.sample_grants_changed();
                }
                self.status = if self.rerun_refused(|message| {
                    message.contains(&origin) || message.contains("permitted sample origins")
                }) {
                    format!("{origin} trusted for samples · updating the score")
                } else {
                    format!("{origin} trusted for samples · the next update reads it")
                };
            }
            Some(PermissionRow::PublicCors { fixed: true, .. }) => {
                self.status =
                    "--strict-sample-origins holds public servers off for this run".into();
            }
            Some(PermissionRow::PublicCors { allowed: true, .. }) => {
                self.status = "public servers with CORS are on · Space turns them off".into();
            }
            Some(PermissionRow::PublicCors { allowed: false, .. }) => {
                self.prefs.strict_sample_origins = false;
                self.sample_grants_changed();
                self.status =
                    if self.rerun_refused(|message| message.contains("permitted sample origins")) {
                        "public servers with CORS on · updating the score".into()
                    } else {
                        "public servers with CORS on".into()
                    };
            }
            Some(PermissionRow::LocalFolder { fixed: true, .. }) => {
                self.status = "--allow-local-samples names the folder for this run".into();
            }
            // The sheet opens the folder prompt for this row.
            Some(PermissionRow::LocalFolder { .. }) => {}
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
                note_blocked(&mut self.blocked_imports, &origin);
                self.status = format!("{origin} off · its scripts no longer load");
            }
            Some(PermissionRow::SampleOrigin { allowed: false, .. }) => self.permit_row(at),
            Some(PermissionRow::SampleOrigin {
                origin,
                allowed: true,
            }) => {
                self.prefs.sample_origins.retain(|kept| *kept != origin);
                self.sample_grants_changed();
                note_blocked(&mut self.blocked_samples, &origin);
                self.status = format!("{origin} off · new samples no longer load from it");
            }
            Some(PermissionRow::PublicCors {
                allowed: true,
                fixed: false,
            }) => {
                self.prefs.strict_sample_origins = true;
                self.sample_grants_changed();
                self.status =
                    "public servers with CORS off · only trusted origins load new samples".into();
            }
            Some(PermissionRow::PublicCors { .. } | PermissionRow::LocalFolder { .. }) => {
                self.permit_row(at);
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
            Some(PermissionRow::SampleOrigin {
                origin,
                allowed: true,
            }) => {
                self.prefs.sample_origins.retain(|kept| *kept != origin);
                self.sample_grants_changed();
                self.status = format!("{origin} removed · new samples no longer load from it");
            }
            Some(PermissionRow::SampleOrigin {
                origin,
                allowed: false,
            }) => {
                self.blocked_samples.retain(|blocked| *blocked != origin);
                self.status = format!("{origin} dismissed");
            }
            // The switch is a default, not something a score asked for, so
            // it stays on the page.
            Some(PermissionRow::PublicCors { .. }) => {
                self.status = "the CORS switch stays · Space turns it on or off".into();
            }
            Some(PermissionRow::LocalFolder { fixed: true, .. }) => self.permit_row(at),
            Some(PermissionRow::LocalFolder { .. }) => {
                if self.prefs.local_sample_root.take().is_some() {
                    self.sample_grants_changed();
                }
                self.status = "local folder forgotten · samples('local:') reads nothing".into();
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
            unblocked = self.rerun_refused(|message| message.contains("Csound is not permitted"));
        }
        self.csound_owed = true;
        self.send_csound();
        unblocked
    }

    /// A permission was given: the refusal on the footer that `names` it
    /// is gone, and the update it refused runs again by itself once the
    /// engine holds the permission. The answer says whether one was owed.
    fn rerun_refused(&mut self, names: impl Fn(&str) -> bool) -> bool {
        let mut unblocked = false;
        for owner in [ErrorOwner::Evaluation, ErrorOwner::Setup] {
            if self.errors.get(owner).is_some_and(&names) {
                self.clear_error(owner);
                unblocked = true;
            }
        }
        if unblocked {
            self.refused_update_owed = true;
        }
        unblocked
    }

    /// Choose the folder `samples('local:')` and a Hydra file outside the
    /// set read. The picker stays open on a folder that does not open.
    pub(super) fn choose_local_sample_folder(&mut self, path: &std::path::Path) {
        if let Err(why) = rustel_runtime::ScoreSampleAccess::denied().permit_local_root(path) {
            if let Some((_, picker)) = self.set_prompt.as_mut() {
                picker.error = Some(format!("cannot use {}: {why}", path.display()));
            }
            return;
        }
        self.prefs.local_sample_root = Some(path.to_string_lossy().into_owned());
        self.sample_grants_changed();
        self.close_set_prompt();
        let folder = super::status_file_path(path, self.ui_settings.show_full_paths);
        self.status = if self.rerun_refused(|message| {
            message.contains("local sample root") || message.contains("is a local file")
        }) {
            format!("samples('local:') reads {folder} · updating the score")
        } else {
            format!("samples('local:') reads {folder}")
        };
        self.dirty_frame = true;
    }

    fn sample_grants_changed(&mut self) {
        self.save_prefs_soon();
        self.sample_grants_owed = true;
        self.send_sample_grants();
    }

    /// Give the engine the page's sample grants, when it does not have
    /// them yet.
    pub(super) fn send_sample_grants(&mut self) {
        if self.sample_grants_owed
            && self
                .worker
                .try_set_sample_grants(self.prefs.sample_grants())
        {
            self.sample_grants_owed = false;
        }
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

        use crate::app::App;
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

        /// The orchestra switch, which is the last row while it is shown.
        fn csound(app: &App) -> Option<PermissionRow> {
            app.permission_rows()
                .pop()
                .filter(|row| matches!(row, PermissionRow::Csound { .. }))
        }

        /// Where the orchestra switch stands on a page with no origins.
        fn csound_row(app: &App) -> usize {
            app.permission_row_count() - 1
        }

        /// The switch starts on, as the engine's does: an installed library
        /// is the studio's opt-in. Space takes it back for the run, a
        /// refusal keeps the row for Enter, and nothing reaches the
        /// preferences.
        #[test]
        fn the_orchestra_switch_starts_on_and_space_takes_it_back_for_a_run() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = app_over(directory.path());
            let at = csound_row(&app);
            assert!(app.csound_allowed);
            assert_eq!(csound(&app), Some(PermissionRow::Csound { allowed: true }));

            // Space takes the permission back; the row stays for a second
            // Space, and the refusal names the page.
            app.toggle_row(at);
            assert_eq!(csound(&app), Some(PermissionRow::Csound { allowed: false }));
            let footer = app.note_csound_refusal(&refusal()).expect("a footer");
            assert_eq!(
                footer,
                "Csound is not permitted - Settings → Permissions lets native orchestras run"
            );

            app.permit_row(at);
            assert_eq!(csound(&app), Some(PermissionRow::Csound { allowed: true }));
            assert!(app.csound_allowed);
            // The row never reaches the preferences.
            assert!(app.prefs.import_origins.is_empty());

            // `d` takes the row off the page, off either way, and a fresh
            // refusal brings it back.
            app.forget_row(at);
            assert_eq!(csound(&app), None);
            assert!(!app.csound_allowed);
            assert!(app.note_csound_refusal(&refusal()).is_some());
            assert_eq!(csound(&app), Some(PermissionRow::Csound { allowed: false }));
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
            app.toggle_row(csound_row(&app));
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

            app.permit_row(csound_row(&app));
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
            app.toggle_row(csound_row(&app));
            let scene = app.scenes.current().id;
            let revision = app.editor().revision();
            app.set_error(
                crate::app::transport::ErrorOwner::Evaluation,
                "Csound is not permitted - Settings → Permissions lets native orchestras run"
                    .into(),
            );
            app.update_failure = Some((scene, revision));

            // The player edits past the refusal before permitting.
            app.permit_row(csound_row(&app));
            type_text(&mut app, "$: s(\"bd, hh\")\n");
            app.retry_refused_update();
            assert!(
                app.pending_evaluation.is_none(),
                "an edited score waits for its own gesture"
            );
        }

        /// The row a sample origin stands on, on or off.
        fn sample_origin(app: &App, origin: &str) -> Option<(usize, bool)> {
            app.permission_rows()
                .into_iter()
                .enumerate()
                .find_map(|(at, row)| match row {
                    PermissionRow::SampleOrigin {
                        origin: named,
                        allowed,
                    } if named == origin => Some((at, allowed)),
                    _ => None,
                })
        }

        /// A refused sample origin lands on the page. Enter trusts it and
        /// keeps the trust in the settings, Space takes it back where it
        /// stands, and `d` dismisses it. The count the sheet reads agrees
        /// with the rows at every step.
        #[test]
        fn a_refused_sample_origin_is_trusted_kept_and_taken_back() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = app_over(directory.path());
            let origin = "https://kit.invalid";
            let count = |app: &App| {
                assert_eq!(app.permission_row_count(), app.permission_rows().len());
            };
            count(&app);
            assert!(app.note_refused_sample(origin));
            assert!(app.note_refused_sample(origin), "noted again, kept once");
            assert_eq!(app.blocked_samples, [origin]);
            let (at, allowed) = sample_origin(&app, origin).expect("the refused row");
            assert!(!allowed);
            count(&app);

            app.permit_row(at);
            assert_eq!(app.prefs.sample_origins, [origin]);
            assert!(app.blocked_samples.is_empty());
            assert_eq!(sample_origin(&app, origin), Some((at, true)));
            assert!(!app.sample_grants_owed, "the engine took the grant");
            assert_eq!(app.prefs.sample_grants().origins, [origin]);
            // A refusal from before the engine held the trust is stale.
            assert!(!app.note_refused_sample(origin));
            count(&app);

            app.toggle_row(at);
            assert!(app.prefs.sample_origins.is_empty());
            assert_eq!(sample_origin(&app, origin), Some((at, false)));
            app.forget_row(at);
            assert_eq!(sample_origin(&app, origin), None);
            count(&app);
        }

        /// Space flips the public CORS default the host permits, and the
        /// page keeps it off. A host that refused the default holds the
        /// switch off: the page cannot open what the command line closed.
        #[test]
        fn the_cors_switch_closes_the_default_and_never_opens_it_over_the_host() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = app_over(directory.path());
            let cors = |app: &App| {
                app.permission_rows()
                    .into_iter()
                    .enumerate()
                    .find(|(_, row)| matches!(row, PermissionRow::PublicCors { .. }))
                    .expect("the switch is always on the page")
            };
            // The test session permits nothing, as `--strict-sample-origins`
            // does.
            let (at, row) = cors(&app);
            assert_eq!(
                row,
                PermissionRow::PublicCors {
                    allowed: false,
                    fixed: true
                }
            );
            app.toggle_row(at);
            assert!(!app.prefs.strict_sample_origins);
            assert!(
                app.status.contains("--strict-sample-origins"),
                "{}",
                app.status
            );

            app.options
                .session
                .score_sample_access
                .permit_public_cors_origins();
            assert_eq!(
                cors(&app).1,
                PermissionRow::PublicCors {
                    allowed: true,
                    fixed: false
                }
            );
            app.toggle_row(at);
            assert!(app.prefs.strict_sample_origins);
            assert!(app.prefs.sample_grants().strict);
            assert!(!cors(&app).1.allowed());
            app.permit_row(at);
            assert!(!app.prefs.strict_sample_origins);
            assert!(cors(&app).1.allowed());
        }

        /// A chosen folder is kept and named on its row. One that does not
        /// open is refused, and `d` forgets the kept one.
        #[test]
        fn the_local_folder_is_chosen_kept_and_forgotten() {
            let directory = tempfile::tempdir().expect("temp dir");
            let samples = tempfile::tempdir().expect("samples dir");
            let mut app = app_over(directory.path());
            let folder = |app: &App| {
                app.permission_rows()
                    .into_iter()
                    .enumerate()
                    .find(|(_, row)| matches!(row, PermissionRow::LocalFolder { .. }))
                    .expect("the folder row is always on the page")
            };
            assert!(!folder(&app).1.allowed());

            app.choose_local_sample_folder(&samples.path().join("missing"));
            assert_eq!(app.prefs.local_sample_root, None);

            app.choose_local_sample_folder(samples.path());
            assert_eq!(
                app.prefs.sample_grants().local_root.as_deref(),
                Some(samples.path())
            );
            let (at, row) = folder(&app);
            assert!(row.allowed());
            assert!(!app.sample_grants_owed, "the engine took the folder");

            app.forget_row(at);
            assert_eq!(app.prefs.local_sample_root, None);
            assert!(!folder(&app).1.allowed());
        }
    }
}
