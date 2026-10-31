//! The Permissions page: the sources a score's samples, images and videos
//! load from, the local folder, the origins it imports scripts from, and
//! the native orchestra switch.
//!
//! The engine refuses a sample or media address that no source allows,
//! `await import('https://...')` from an origin that is not trusted, and an
//! orchestra while native execution is not permitted. The page reads as a
//! switch: Enter allows a refused row, and Space or either sideways arrow
//! flips a row where it stands, so a permission can be taken back without
//! the row leaving the page. A refused or turned-off row stays listed
//! until the studio closes. The add rows ask for a source or an origin, and
//! the try row loads one address once to show which source allows it.
//! Sources, origins and the local folder are kept in the settings, while
//! the orchestra switch lasts one run.

use super::super::settings::{PermissionCounts, PermissionRow, TriedSource, TryVerdict};
use super::super::worker::EngineFailure;
use super::App;
use super::transport::ErrorOwner;
use rustel_runtime::{BUILTIN_SOURCES, SourceRule};

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

fn is_builtin(rule: &str) -> bool {
    BUILTIN_SOURCES.contains(&rule)
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

    /// Put the sources the engine refused a score's samples or media from
    /// on the page. The status line names the newest.
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
            self.status =
                format!("{rule} is not allowed for samples - Settings → Permissions allows it");
        }
    }

    /// Put one refused source on the page. A refusal for a source on the
    /// saved list is stale: the engine did not have the list yet. A
    /// built-in source has its row already.
    fn note_refused_sample(&mut self, rule: &str) -> bool {
        if self.prefs.allowed_sources.iter().any(|kept| kept == rule) {
            return false;
        }
        if !is_builtin(rule) {
            note_blocked(&mut self.blocked_samples, rule);
        }
        true
    }

    /// The source rows: the built-in domains, the player's sources, the
    /// ones the command line named, then the refused.
    fn sample_source_rows(&self) -> Vec<PermissionRow> {
        let host = self.options.session.score_sample_access.sources();
        let in_host = |rule: &str| SourceRule::parse(rule).is_ok_and(|rule| host.contains(&rule));
        let builtins = BUILTIN_SOURCES.iter().map(|rule| PermissionRow::Source {
            rule: (*rule).to_owned(),
            allowed: in_host(rule) && !self.prefs.builtin_sources_off.iter().any(|off| off == rule),
            builtin: true,
            fixed: !in_host(rule),
        });
        let added = self
            .prefs
            .allowed_sources
            .iter()
            .map(|rule| PermissionRow::Source {
                rule: rule.clone(),
                allowed: true,
                builtin: false,
                fixed: false,
            });
        let command_line = host
            .iter()
            .map(ToString::to_string)
            .filter(|rule| !is_builtin(rule) && !self.prefs.allowed_sources.contains(rule))
            .map(|rule| PermissionRow::Source {
                rule,
                allowed: true,
                builtin: false,
                fixed: true,
            });
        let refused = self
            .blocked_samples
            .iter()
            .map(|rule| PermissionRow::Source {
                rule: rule.clone(),
                allowed: false,
                builtin: false,
                fixed: false,
            });
        builtins
            .chain(added)
            .chain(command_line)
            .chain(refused)
            .collect()
    }

    /// The rows of the permissions page, section by section: the sources
    /// with the add and try rows, the local folder, the trusted then the
    /// refused script origins with their add row, and the orchestra switch
    /// once the studio has a reason to show it.
    pub(super) fn permission_rows(&self) -> Vec<PermissionRow> {
        let mut rows = self.sample_source_rows();
        rows.push(PermissionRow::AddSource);
        rows.push(PermissionRow::TrySource(self.tried_source.clone()));
        let host = &self.options.session.score_sample_access;
        rows.push(PermissionRow::LocalFolder {
            folder: self
                .prefs
                .local_sample_root
                .as_deref()
                .filter(|_| !host.has_local_root())
                .map(|folder| super::sets::shorten_home(std::path::Path::new(folder))),
            fixed: host.has_local_root(),
        });
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
        rows.push(PermissionRow::AddOrigin);
        if self.csound_allowed || self.csound_refused {
            rows.push(PermissionRow::Csound {
                allowed: self.csound_allowed,
            });
        }
        rows
    }

    /// How many rows each section of the permissions page shows. The sheet
    /// counts its keys by this, and the pointer counts its hits.
    pub(super) fn permission_counts(&self) -> PermissionCounts {
        PermissionCounts {
            sources: self.sample_source_rows().len(),
            scripts: self.prefs.import_origins.len() + self.blocked_imports.len(),
            csound: self.csound_allowed || self.csound_refused,
        }
    }

    /// Enter on a row of the permissions page. The sheet opens the prompt
    /// of a row that asks for something.
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
                self.status = format!("{rule} is allowed · Space turns it off");
            }
            Some(PermissionRow::Source { rule, .. }) => self.allow_source(&rule),
            Some(
                PermissionRow::AddSource
                | PermissionRow::TrySource(_)
                | PermissionRow::LocalFolder { .. }
                | PermissionRow::AddOrigin,
            ) => {}
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
                builtin: true,
                fixed: false,
            }) => {
                self.prefs.builtin_sources_off.push(rule.clone());
                self.sample_grants_changed();
                self.status = format!("{rule} off · new samples no longer load from it");
            }
            Some(PermissionRow::Source {
                rule,
                allowed: true,
                fixed: false,
                ..
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
            // A built-in source is part of the page: it goes off, and stays
            // listed so it can come back.
            Some(PermissionRow::Source {
                rule,
                allowed,
                builtin: true,
                ..
            }) => {
                if allowed {
                    self.toggle_row(at);
                }
                self.status = format!("{rule} is built in · it stays off · Space turns it on");
            }
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
            Some(PermissionRow::TrySource(Some(_))) => {
                self.tried_source = None;
                self.try_answer = None;
                self.status = "try cleared".into();
            }
            Some(
                PermissionRow::AddSource
                | PermissionRow::TrySource(None)
                | PermissionRow::AddOrigin,
            ) => {}
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

    /// Allow one source, as written on the page: a refused one, a built-in
    /// one that is off, or one the player typed.
    fn allow_source(&mut self, rule: &str) {
        self.blocked_samples.retain(|blocked| blocked != rule);
        if is_builtin(rule) {
            let held = SourceRule::parse(rule).is_ok_and(|rule| {
                !self
                    .options
                    .session
                    .score_sample_access
                    .sources()
                    .contains(&rule)
            });
            if held {
                self.status = format!("--strict-sample-origins holds {rule} off for this run");
                return;
            }
            self.prefs.builtin_sources_off.retain(|off| off != rule);
        } else if !self.prefs.allowed_sources.iter().any(|kept| kept == rule) {
            self.prefs.allowed_sources.push(rule.to_owned());
        }
        self.sample_grants_changed();
        self.status = if self.rerun_refused(names_a_refused_source) {
            format!("{rule} allowed · updating the score")
        } else {
            format!("{rule} allowed · the next update reads it")
        };
    }

    /// The source the add prompt read. A text that is no source keeps the
    /// prompt open and says how to write one.
    pub(super) fn allow_typed_source(&mut self, text: &str) {
        let rule = match SourceRule::parse(text) {
            Ok(rule) => rule.to_string(),
            Err(why) => {
                if let Some((_, picker)) = self.set_prompt.as_mut() {
                    picker.error = Some(why);
                }
                return;
            }
        };
        self.close_set_prompt();
        self.allow_source(&rule);
        self.select_permission(
            |row| matches!(row, PermissionRow::Source { rule: named, .. } if *named == rule),
        );
        self.dirty_frame = true;
    }

    /// Trust one script origin: a refused one, or one the player typed.
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

    /// The origin the add prompt of the scripts read.
    pub(super) fn trust_typed_origin(&mut self, text: &str) {
        let origin = match rustel_runtime::import_origin(text.trim()) {
            Ok(origin) => origin,
            Err(why) => {
                if let Some((_, picker)) = self.set_prompt.as_mut() {
                    picker.error = Some(why);
                }
                return;
            }
        };
        self.close_set_prompt();
        self.trust_origin(&origin);
        self.select_permission(
            |row| matches!(row, PermissionRow::Origin { origin: named, .. } if *named == origin),
        );
        self.dirty_frame = true;
    }

    /// The address the try prompt read: an http or https address, or the
    /// prompt stays open and says so.
    pub(super) fn try_typed_address(&mut self, text: &str) {
        let address = text.trim().to_owned();
        let form = url::Url::parse(&address)
            .ok()
            .filter(|url| matches!(url.scheme(), "http" | "https") && url.host_str().is_some());
        if form.is_none() {
            if let Some((_, picker)) = self.set_prompt.as_mut() {
                picker.error = Some(format!("{address} is not an http or https address"));
            }
            return;
        }
        self.close_set_prompt();
        self.try_address(address);
        self.dirty_frame = true;
    }

    /// Say which source allows `address`, and load it once when one does.
    /// The load runs off the studio's thread; [`Self::poll_tried_source`]
    /// takes its answer.
    fn try_address(&mut self, address: String) {
        let access =
            crate::engine::sample_access(&self.options.session, &self.prefs.sample_grants());
        let verdict = match access.source_for(&address) {
            Ok(Some(rule)) => {
                let by = rule.to_string();
                let (send, answer) = std::sync::mpsc::channel();
                let tried = address.clone();
                let started =
                    std::thread::Builder::new()
                        .name("try-source".into())
                        .spawn(move || {
                            let loaded = access.try_address(&tried);
                            let _ = send.send((tried, loaded));
                        });
                match started {
                    Ok(_) => {
                        self.try_answer = Some(answer);
                        TryVerdict::Loading { by }
                    }
                    Err(why) => TryVerdict::Failed {
                        by,
                        why: why.to_string(),
                    },
                }
            }
            Ok(None) => {
                let url = url::Url::parse(&address).ok();
                let domain = url.as_ref().and_then(SourceRule::for_url);
                let folder = url
                    .as_ref()
                    .and_then(SourceRule::folder_of)
                    .filter(|folder| Some(folder) != domain.as_ref());
                TryVerdict::Refused {
                    domain: domain.map(|rule| rule.to_string()),
                    folder: folder.map(|rule| rule.to_string()),
                }
            }
            Err(why) => TryVerdict::Failed {
                by: String::new(),
                why,
            },
        };
        self.status = match &verdict {
            TryVerdict::Refused { .. } => format!("{address} is not allowed"),
            TryVerdict::Loading { by } => format!("{address} is allowed by {by} · loading"),
            TryVerdict::Loaded { .. } | TryVerdict::Failed { .. } => format!("{address} tried"),
        };
        self.tried_source = Some(TriedSource { address, verdict });
    }

    /// Take the answer of the load the try row started, when it is in.
    pub(super) fn poll_tried_source(&mut self) {
        let Some(answer) = &self.try_answer else {
            return;
        };
        let (address, loaded) = match answer.try_recv() {
            Ok(answered) => answered,
            Err(std::sync::mpsc::TryRecvError::Empty) => return,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                self.try_answer = None;
                return;
            }
        };
        self.try_answer = None;
        if let Some(tried) = self.tried_source.as_mut()
            && tried.address == address
            && let TryVerdict::Loading { by } = &tried.verdict
        {
            let by = by.clone();
            tried.verdict = match loaded {
                Ok(what) => TryVerdict::Loaded { by, what },
                // The address is on the row already.
                Err(why) => TryVerdict::Failed {
                    by,
                    why: why
                        .strip_prefix("GET ")
                        .and_then(|rest| rest.split_once(": "))
                        .map_or(why.clone(), |(_, rest)| rest.to_owned()),
                },
            };
            self.dirty_frame = true;
        }
    }

    /// `a` or `u` on the try row: allow the server, or only the folder, of
    /// the address it refused, and try it again.
    pub(super) fn allow_tried(&mut self, folder: bool) {
        let Some(TriedSource {
            address,
            verdict:
                TryVerdict::Refused {
                    domain,
                    folder: narrow,
                },
        }) = self.tried_source.clone()
        else {
            self.status = "nothing refused to allow · Enter tries an address".into();
            return;
        };
        let Some(rule) = (if folder { narrow } else { domain }) else {
            self.status = "this address has no narrower folder · a allows it".into();
            return;
        };
        self.allow_source(&rule);
        let allowed = std::mem::take(&mut self.status);
        self.try_address(address);
        self.status = allowed;
    }

    /// Put the sheet's cursor on the first row that `pick` takes.
    fn select_permission(&mut self, pick: impl Fn(&PermissionRow) -> bool) {
        let at = self.permission_rows().iter().position(pick);
        let counts = self.permission_counts();
        if let Some(sheet) = self.settings_sheet.as_mut() {
            sheet.permission_counts = counts;
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
            app.permission_counts().total() - 1
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

        /// The count the sheet reads agrees with the rows.
        fn counts_agree(app: &App) {
            let counts = app.permission_counts();
            let rows = app.permission_rows();
            assert_eq!(counts.total(), rows.len());
            assert!(matches!(
                rows[counts.try_row()],
                PermissionRow::TrySource(_)
            ));
        }

        /// A refused source lands on the page. Enter allows it and keeps it
        /// in the settings, Space takes it back where it stands, and `d`
        /// dismisses it. The count the sheet reads agrees with the rows at
        /// every step.
        #[test]
        fn a_refused_source_is_allowed_kept_and_taken_back() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = app_over(directory.path());
            let rule = "https://kit.invalid";
            counts_agree(&app);
            assert!(app.note_refused_sample(rule));
            assert!(app.note_refused_sample(rule), "noted again, kept once");
            assert_eq!(app.blocked_samples, [rule]);
            let (at, allowed) = source_row(&app, rule).expect("the refused row");
            assert!(!allowed);
            counts_agree(&app);

            app.permit_row(at);
            assert_eq!(app.prefs.allowed_sources, [rule]);
            assert!(app.blocked_samples.is_empty());
            assert_eq!(source_row(&app, rule), Some((at, true)));
            assert!(!app.sample_grants_owed, "the engine took the grant");
            assert_eq!(app.prefs.sample_grants().sources, [rule]);
            // A refusal from before the engine held the source is stale.
            assert!(!app.note_refused_sample(rule));
            counts_agree(&app);

            app.toggle_row(at);
            assert!(app.prefs.allowed_sources.is_empty());
            assert_eq!(source_row(&app, rule), Some((at, false)));
            app.forget_row(at);
            assert_eq!(source_row(&app, rule), None);
            counts_agree(&app);
        }

        /// The built-in sources are always on the page. The command line
        /// can hold them off, and the page cannot open them over it. Where
        /// the host allows them, Space turns one off and on, and `d` turns
        /// it off but leaves it listed.
        #[test]
        fn a_builtin_source_turns_off_and_on_and_stays_listed() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = app_over(directory.path());
            let github = "raw.githubusercontent.com";
            let builtin = |app: &App| {
                app.permission_rows()
                    .into_iter()
                    .enumerate()
                    .find(|(_, row)| {
                        matches!(row, PermissionRow::Source { rule, .. } if rule == github)
                    })
                    .expect("a built-in source is always on the page")
            };
            // The test session allows nothing, as `--strict-sample-origins`
            // does.
            let (at, row) = builtin(&app);
            assert_eq!(
                row,
                PermissionRow::Source {
                    rule: github.to_owned(),
                    allowed: false,
                    builtin: true,
                    fixed: true,
                }
            );
            app.toggle_row(at);
            assert!(app.prefs.builtin_sources_off.is_empty());
            assert!(app.status.contains("command line"), "{}", app.status);

            app.options
                .session
                .score_sample_access
                .permit_builtin_sources();
            assert!(builtin(&app).1.allowed());
            app.toggle_row(at);
            assert_eq!(app.prefs.builtin_sources_off, [github]);
            assert_eq!(app.prefs.sample_grants().builtins_off, [github]);
            assert!(!builtin(&app).1.allowed());
            app.permit_row(at);
            assert!(app.prefs.builtin_sources_off.is_empty());
            assert!(builtin(&app).1.allowed());

            app.forget_row(at);
            assert_eq!(builtin(&app).0, at, "it stays where it was");
            assert!(!builtin(&app).1.allowed());
            // A refusal of a built-in has its row already.
            assert!(app.note_refused_sample(github));
            assert!(app.blocked_samples.is_empty());
            counts_agree(&app);
        }

        /// A typed source or origin that is not one keeps its prompt open
        /// and says how to write it. One that is lands on the page, in the
        /// settings, under the cursor.
        #[test]
        fn a_typed_source_or_origin_is_checked_kept_and_selected() {
            use crate::app::sets::SetPrompt;
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = app_over(directory.path());
            app.settings_sheet = Some(crate::settings::SettingsSheet::default());

            app.open_set_prompt(SetPrompt::AllowSource);
            app.allow_typed_source("example.com/kits");
            let error = app
                .set_prompt
                .as_ref()
                .and_then(|(_, picker)| picker.error.clone())
                .expect("the prompt says what is wrong");
            assert!(error.contains("https://example.com/kits/"), "{error}");
            assert!(app.prefs.allowed_sources.is_empty());

            app.allow_typed_source("Example.com");
            assert!(app.set_prompt.is_none(), "the prompt closed");
            assert_eq!(app.prefs.allowed_sources, ["example.com"]);
            let (at, _) = source_row(&app, "example.com").expect("the row");
            assert_eq!(app.settings_sheet.expect("the sheet").selected, at);

            app.open_set_prompt(SetPrompt::TrustScriptOrigin);
            app.trust_typed_origin("http://scripts.example");
            assert!(app.set_prompt.is_some(), "plain http is refused");
            app.trust_typed_origin("https://scripts.example/");
            assert_eq!(app.prefs.import_origins, ["https://scripts.example"]);
            counts_agree(&app);
        }

        /// The try row says no source allows an address and offers its
        /// server and its folder. `a` allows the server and tries again,
        /// and the row then says what loaded.
        #[test]
        fn the_try_row_offers_a_source_and_shows_what_loads() {
            use crate::settings::{TriedSource, TryVerdict};
            use std::io::{Read, Write};

            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = app_over(directory.path());
            let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("listener");
            let origin = format!("http://{}", listener.local_addr().unwrap());
            let server = std::thread::spawn(move || {
                let (mut stream, _) = listener.accept().expect("one request");
                let mut head = [0; 1024];
                let _ = stream.read(&mut head);
                let body = r#"{"bd": ["bd.wav"]}"#;
                let _ = write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
            });
            let address = format!("{origin}/kits/kit.json");

            app.try_typed_address("kit.json");
            assert_eq!(app.tried_source, None, "not an address");
            app.try_typed_address(&address);
            assert_eq!(
                app.tried_source,
                Some(TriedSource {
                    address: address.clone(),
                    verdict: TryVerdict::Refused {
                        domain: Some(origin.clone()),
                        folder: Some(format!("{origin}/kits/")),
                    },
                })
            );
            // The refusal asked nothing of the engine's record.
            assert!(app.blocked_samples.is_empty());

            app.allow_tried(false);
            assert_eq!(app.prefs.allowed_sources, std::slice::from_ref(&origin));
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
            loop {
                app.poll_tried_source();
                let verdict = &app.tried_source.as_ref().expect("the try").verdict;
                if !matches!(verdict, TryVerdict::Loading { .. }) {
                    assert_eq!(
                        *verdict,
                        TryVerdict::Loaded {
                            by: origin.clone(),
                            what: "sample map, 1 sound".to_owned(),
                        }
                    );
                    break;
                }
                assert!(
                    std::time::Instant::now() < deadline,
                    "the load never answered"
                );
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            server.join().expect("server thread");
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
