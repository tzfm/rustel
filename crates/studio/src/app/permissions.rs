//! The Permissions page: the sources a score's samples, images and videos
//! load from, the local folder, the origins it imports scripts from, and
//! the native orchestra switch.
//!
//! The engine refuses a sample or media file that no source allows,
//! `await import('https://...')` from an origin that is not trusted, and an
//! orchestra while native execution is not permitted. A refusal puts its
//! row on the page: the folder of a refused file, the origin of a refused
//! script. The page reads as a switch: Enter allows a refused row, and
//! Space or either sideways arrow flips a row where it stands, so a
//! permission can be taken back without the row leaving the page. `e`
//! opens a source in a prompt, to cut it down to its domain. A refused or
//! turned-off row stays listed until the studio closes. The built-in
//! sources are always on and have no row. Sources, origins and the local
//! folder are kept in the settings, while the orchestra switch lasts one
//! run.

use super::super::settings::PermissionRow;
use super::super::worker::EngineFailure;
use super::App;
use super::transport::ErrorOwner;
use rustel_runtime::{BUILTIN_SOURCES, LOCAL_FILES, SourceRule};

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

/// Whether a refusal on the footer is one that a new source can lift.
fn names_a_refused_source(message: &str) -> bool {
    message.contains("is not an allowed sample source")
        || message.contains("is not an allowed source for a")
}

/// Whether the source written `wide` allows what `narrow` names.
fn covers(wide: &str, narrow: &str) -> bool {
    SourceRule::parse(wide)
        .is_ok_and(|wide| SourceRule::parse(narrow).is_ok_and(|narrow| wide.covers(&narrow)))
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

    /// Put what the engine refused a score's samples or media on the page:
    /// the folder of each refused file, and the local folder row when a
    /// score asked for `local:` files. The status line names the newest.
    pub(super) fn note_refused_samples(&mut self) {
        let refused = self
            .options
            .session
            .score_sample_access
            .take_refused_sources();
        if let Some(rule) = refused
            .into_iter()
            .filter(|rule| self.note_refused_sample(rule))
            .last()
        {
            self.status = if rule == LOCAL_FILES {
                "samples('local:') has no folder - Settings → Permissions chooses one".into()
            } else {
                format!("{rule} is not allowed for samples - Settings → Permissions allows it")
            };
        }
    }

    /// Put one refused source on the page. A refusal that a saved source
    /// covers is stale: the engine did not have the list yet.
    fn note_refused_sample(&mut self, rule: &str) -> bool {
        if rule == LOCAL_FILES {
            self.local_files_asked = true;
            return true;
        }
        if self
            .prefs
            .allowed_sources
            .iter()
            .any(|kept| covers(kept, rule))
        {
            return false;
        }
        note_blocked(&mut self.blocked_samples, rule);
        true
    }

    /// The source rows: the player's sources, the ones the command line
    /// named, then the refused. The built-in sources have no row.
    fn sample_source_rows(&self) -> impl Iterator<Item = PermissionRow> + '_ {
        let host = self.options.session.score_sample_access.sources();
        let added = self
            .prefs
            .allowed_sources
            .iter()
            .map(|rule| PermissionRow::Source {
                rule: rule.clone(),
                allowed: true,
                fixed: false,
            });
        let command_line = host
            .iter()
            .map(ToString::to_string)
            .filter(|rule| {
                !BUILTIN_SOURCES.contains(&rule.as_str())
                    && !self.prefs.allowed_sources.contains(rule)
            })
            .map(|rule| PermissionRow::Source {
                rule,
                allowed: true,
                fixed: true,
            });
        let refused = self
            .blocked_samples
            .iter()
            .map(|rule| PermissionRow::Source {
                rule: rule.clone(),
                allowed: false,
                fixed: false,
            });
        added.chain(command_line).chain(refused)
    }

    /// The local folder row, once there is a folder or a score asked for
    /// `local:` files.
    fn local_folder_row(&self) -> Option<PermissionRow> {
        let fixed = self.options.session.score_sample_access.has_local_root();
        let folder = self
            .prefs
            .local_sample_root
            .as_deref()
            .filter(|_| !fixed)
            .map(|folder| super::sets::shorten_home(std::path::Path::new(folder)));
        (fixed || folder.is_some() || self.local_files_asked)
            .then_some(PermissionRow::LocalFolder { folder, fixed })
    }

    /// The rows of the permissions page: the sources, the local folder,
    /// the trusted then the refused script origins, and the orchestra
    /// switch once the studio has a reason to show it.
    pub(super) fn permission_rows(&self) -> Vec<PermissionRow> {
        let mut rows: Vec<_> = self.sample_source_rows().collect();
        rows.extend(self.local_folder_row());
        let trusted = self
            .prefs
            .import_origins
            .iter()
            .map(|origin| (origin, true));
        let refused = self.blocked_imports.iter().map(|origin| (origin, false));
        rows.extend(
            trusted
                .chain(refused)
                .map(|(origin, allowed)| PermissionRow::Origin {
                    origin: origin.clone(),
                    allowed,
                }),
        );
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
        self.permission_rows().len()
    }

    /// Enter on a row of the permissions page. The sheet opens the folder
    /// prompt of the local folder row.
    pub(super) fn permit_row(&mut self, at: usize) {
        match self.permission_rows().into_iter().nth(at) {
            Some(
                PermissionRow::Source { fixed: true, .. }
                | PermissionRow::LocalFolder { fixed: true, .. },
            ) => {
                self.status = "the command line set this for the run".into();
            }
            Some(PermissionRow::Source {
                rule,
                allowed: true,
                ..
            }) => {
                self.status = format!("{rule} is allowed · e edits it · Space turns it off");
            }
            Some(PermissionRow::Source { rule, .. }) => self.allow_source(&rule),
            Some(PermissionRow::LocalFolder { .. }) => {}
            Some(PermissionRow::Origin {
                origin,
                allowed: true,
            }) => {
                self.status = format!("{origin} is trusted · Space turns it off");
            }
            Some(PermissionRow::Origin {
                origin,
                allowed: false,
            }) => self.trust_origin(&origin),
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

    /// Space on a row of the permissions page: allow a refused row, or
    /// take the permission back where the row stands. The row stays on the
    /// page, so the permission can come back with one more Space.
    pub(super) fn toggle_row(&mut self, at: usize) {
        match self.permission_rows().into_iter().nth(at) {
            Some(PermissionRow::Source {
                rule,
                allowed: true,
                fixed: false,
            }) => {
                self.prefs.allowed_sources.retain(|kept| *kept != rule);
                self.sample_grants_changed();
                // The row stays: it moves to the refused ones, which the
                // studio keeps until it closes, so a permission taken back
                // can be given again.
                note_blocked(&mut self.blocked_samples, &rule);
                self.status = format!("{rule} off · new samples no longer load from it");
            }
            Some(PermissionRow::Origin {
                origin,
                allowed: true,
            }) => {
                self.prefs.import_origins.retain(|kept| *kept != origin);
                self.import_origins_changed();
                note_blocked(&mut self.blocked_imports, &origin);
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
            Some(_) => self.permit_row(at),
            None => {}
        }
    }

    /// Delete on a row of the permissions page.
    pub(super) fn forget_row(&mut self, at: usize) {
        match self.permission_rows().into_iter().nth(at) {
            Some(
                PermissionRow::Source { fixed: true, .. }
                | PermissionRow::LocalFolder { fixed: true, .. },
            ) => self.permit_row(at),
            Some(PermissionRow::Source {
                rule,
                allowed: true,
                ..
            }) => {
                self.prefs.allowed_sources.retain(|kept| *kept != rule);
                self.sample_grants_changed();
                self.status = format!("{rule} removed · new samples no longer load from it");
            }
            Some(PermissionRow::Source { rule, .. }) => {
                self.blocked_samples.retain(|blocked| *blocked != rule);
                self.status = format!("{rule} dismissed");
            }
            Some(PermissionRow::LocalFolder { .. }) => {
                if self.prefs.local_sample_root.take().is_some() {
                    self.sample_grants_changed();
                }
                self.status = "local folder forgotten · samples('local:') reads nothing".into();
            }
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

    /// `e` on a row of the permissions page: the source it names, for the
    /// edit prompt. Only a source of the page's own edits; a script origin
    /// stays exact, since a script runs code.
    pub(super) fn edit_row(&mut self, at: usize) -> Option<String> {
        match self.permission_rows().into_iter().nth(at) {
            Some(PermissionRow::Source {
                rule, fixed: false, ..
            }) => {
                self.editing_source = Some(rule.clone());
                Some(rule)
            }
            Some(PermissionRow::Source { fixed: true, .. }) => {
                self.status = "the command line set this for the run".into();
                None
            }
            Some(PermissionRow::Origin { .. }) => {
                self.status = "a script origin stays exact · e edits a sample source".into();
                None
            }
            _ => None,
        }
    }

    /// The source the edit prompt read. It must still allow what the row
    /// named, so a typo cannot open another site: an edit cuts a source
    /// down toward its domain. A text that does not keeps the prompt open
    /// and says why. The edited source takes the row's place, on.
    pub(super) fn edit_typed_source(&mut self, text: &str) {
        let Some(was) = self.editing_source.clone() else {
            self.close_set_prompt();
            return;
        };
        let checked = SourceRule::parse(text).and_then(|rule| {
            if covers(&rule.to_string(), &was) {
                Ok(rule.to_string())
            } else {
                Err(format!(
                    "{rule} does not allow {was} · cut the address down, such as to its domain"
                ))
            }
        });
        let rule = match checked {
            Ok(rule) => rule,
            Err(why) => {
                if let Some((_, picker)) = self.set_prompt.as_mut() {
                    picker.error = Some(why);
                }
                return;
            }
        };
        self.editing_source = None;
        self.close_set_prompt();
        let kept = &mut self.prefs.allowed_sources;
        if let Some(at) = kept.iter().position(|source| *source == was) {
            if kept.contains(&rule) {
                kept.remove(at);
            } else {
                kept[at] = rule.clone();
            }
        }
        self.blocked_samples.retain(|blocked| *blocked != was);
        self.allow_source(&rule);
        self.select_permission(
            |row| matches!(row, PermissionRow::Source { rule: named, .. } if *named == rule),
        );
        self.dirty_frame = true;
    }

    /// Allow one source: a refused one, or one the edit prompt read. The
    /// refused rows it covers leave the page with it.
    fn allow_source(&mut self, rule: &str) {
        self.blocked_samples
            .retain(|blocked| blocked != rule && !covers(rule, blocked));
        if !self.prefs.allowed_sources.iter().any(|kept| kept == rule) {
            self.prefs.allowed_sources.push(rule.to_owned());
        }
        self.sample_grants_changed();
        self.status = if self.rerun_refused(names_a_refused_source) {
            format!("{rule} allowed · updating the score")
        } else {
            format!("{rule} allowed · the next update reads it")
        };
    }

    /// Trust one refused script origin.
    fn trust_origin(&mut self, origin: &str) {
        self.blocked_imports.retain(|blocked| blocked != origin);
        if !self.prefs.import_origins.iter().any(|kept| kept == origin) {
            self.prefs.import_origins.push(origin.to_owned());
            self.import_origins_changed();
        }
        self.status = if self.rerun_refused(|message| message.contains(origin)) {
            format!("{origin} trusted · updating the score")
        } else {
            format!("{origin} trusted")
        };
    }

    /// Put the sheet's cursor on the first row that `pick` takes.
    fn select_permission(&mut self, pick: impl Fn(&PermissionRow) -> bool) {
        let rows = self.permission_rows();
        let at = rows.iter().position(pick);
        if let Some(sheet) = self.settings_sheet.as_mut() {
            sheet.permissions_count = rows.len();
            if let Some(at) = at {
                sheet.selected = at;
            }
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

        /// The row a source stands on, on or off.
        fn source_row(app: &App, rule: &str) -> Option<(usize, bool)> {
            app.permission_rows()
                .into_iter()
                .enumerate()
                .find_map(|(at, row)| match row {
                    PermissionRow::Source {
                        rule: named,
                        allowed,
                        ..
                    } if named == rule => Some((at, allowed)),
                    _ => None,
                })
        }

        /// The page starts empty: the built-in sources are always on and
        /// have no row, and there is no folder yet.
        #[test]
        fn the_page_starts_with_no_source_rows() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = app_over(directory.path());
            app.options
                .session
                .score_sample_access
                .permit_builtin_sources();
            assert!(
                app.permission_rows()
                    .iter()
                    .all(|row| matches!(row, PermissionRow::Csound { .. })),
                "{:?}",
                app.permission_rows()
            );
        }

        /// The folder of a refused file lands on the page. Enter allows it
        /// and keeps it in the settings, Space takes it back where it
        /// stands, and `d` dismisses it.
        #[test]
        fn a_refused_folder_is_allowed_kept_and_taken_back() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = app_over(directory.path());
            let rule = "kit.invalid/kits/";
            assert!(app.note_refused_sample(rule));
            assert!(app.note_refused_sample(rule), "noted again, kept once");
            assert_eq!(app.blocked_samples, [rule]);
            let (at, allowed) = source_row(&app, rule).expect("the refused row");
            assert!(!allowed);

            app.permit_row(at);
            assert_eq!(app.prefs.allowed_sources, [rule]);
            assert!(app.blocked_samples.is_empty());
            assert_eq!(source_row(&app, rule), Some((at, true)));
            assert!(!app.sample_grants_owed, "the engine took the grant");
            assert_eq!(app.prefs.sample_grants().sources, [rule]);
            // A refusal that a saved source covers is stale.
            assert!(!app.note_refused_sample("kit.invalid/kits/808/"));

            app.toggle_row(at);
            assert!(app.prefs.allowed_sources.is_empty());
            assert_eq!(source_row(&app, rule), Some((at, false)));
            app.forget_row(at);
            assert_eq!(source_row(&app, rule), None);
            assert_eq!(app.permission_row_count(), app.permission_rows().len());
        }

        /// `e` opens a refused folder in a prompt. An edit that does not
        /// allow the folder keeps the prompt open and says why. Cut down to
        /// the domain, the row allows all of it, takes the refused rows it
        /// covers along, and keeps the cursor.
        #[test]
        fn an_edit_cuts_a_folder_down_to_its_domain() {
            use crate::app::sets::SetPrompt;
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = app_over(directory.path());
            app.settings_sheet = Some(crate::settings::SettingsSheet::default());
            let folder = "github.com/me/kit/raw/main/";
            app.note_refused_sample(folder);
            app.note_refused_sample("github.com/me/other/");
            let (at, _) = source_row(&app, folder).expect("the refused row");

            assert_eq!(app.edit_row(at).as_deref(), Some(folder));
            app.open_set_prompt(SetPrompt::EditSource);
            for wrong in [
                "gihtub.com",
                "http://github.com/me/",
                "github.com/me/kit/raw/main/drums/",
            ] {
                app.edit_typed_source(wrong);
                let error = app
                    .set_prompt
                    .as_ref()
                    .and_then(|(_, picker)| picker.error.clone())
                    .expect("the prompt says what is wrong");
                assert!(error.contains("does not allow"), "{wrong}: {error}");
            }
            assert!(app.prefs.allowed_sources.is_empty());

            app.edit_typed_source("github.com");
            assert!(app.set_prompt.is_none(), "the prompt closed");
            assert_eq!(app.prefs.allowed_sources, ["github.com"]);
            assert!(app.blocked_samples.is_empty(), "the domain covers both");
            let (at, allowed) = source_row(&app, "github.com").expect("the row");
            assert!(allowed);
            assert_eq!(app.settings_sheet.expect("the sheet").selected, at);
            assert!(app.status.contains("github.com allowed"), "{}", app.status);
        }

        /// An edit of a saved source keeps its place on the list. A script
        /// origin and a command line source are not edited.
        #[test]
        fn an_edit_keeps_its_place_and_scripts_stay_exact() {
            use crate::app::sets::SetPrompt;
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = app_over(directory.path());
            app.prefs.allowed_sources =
                vec!["a.example/kits/".to_owned(), "b.example/kits/".to_owned()];
            let (at, _) = source_row(&app, "a.example/kits/").expect("the row");
            app.edit_row(at);
            app.open_set_prompt(SetPrompt::EditSource);
            app.edit_typed_source("a.example");
            assert_eq!(app.prefs.allowed_sources, ["a.example", "b.example/kits/"]);

            app.prefs
                .import_origins
                .push("https://scripts.example".to_owned());
            let origin = app
                .permission_rows()
                .iter()
                .position(|row| matches!(row, PermissionRow::Origin { .. }))
                .expect("the origin row");
            assert_eq!(app.edit_row(origin), None);
            assert!(app.status.contains("stays exact"), "{}", app.status);

            app.options
                .session
                .score_sample_access
                .permit_source("http://localhost:8000")
                .unwrap();
            let (fixed, _) = source_row(&app, "http://localhost:8000").expect("the row");
            assert_eq!(app.edit_row(fixed), None);
            assert!(app.status.contains("command line"), "{}", app.status);
        }

        /// The folder row comes onto the page when a score asks for
        /// `local:` files. A chosen folder is kept and named on its row,
        /// one that does not open is refused, and `d` forgets the kept one.
        #[test]
        fn the_local_folder_is_asked_for_chosen_kept_and_forgotten() {
            let directory = tempfile::tempdir().expect("temp dir");
            let samples = tempfile::tempdir().expect("samples dir");
            let mut app = app_over(directory.path());
            let folder = |app: &App| {
                app.permission_rows()
                    .into_iter()
                    .enumerate()
                    .find(|(_, row)| matches!(row, PermissionRow::LocalFolder { .. }))
            };
            assert_eq!(folder(&app), None, "no folder and nobody asked");
            assert!(app.note_refused_sample(rustel_runtime::LOCAL_FILES));
            let (_, asked) = folder(&app).expect("a score asked for local files");
            assert!(!asked.allowed());
            assert!(app.blocked_samples.is_empty(), "it is not a source");

            app.choose_local_sample_folder(&samples.path().join("missing"));
            assert_eq!(app.prefs.local_sample_root, None);

            app.choose_local_sample_folder(samples.path());
            assert_eq!(
                app.prefs.sample_grants().local_root.as_deref(),
                Some(samples.path())
            );
            let (at, row) = folder(&app).expect("the folder row");
            assert!(row.allowed());
            assert!(!app.sample_grants_owed, "the engine took the folder");

            app.forget_row(at);
            assert_eq!(app.prefs.local_sample_root, None);
            assert!(!folder(&app).expect("still asked for").1.allowed());
        }
    }
}
