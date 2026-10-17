//! The Permissions page: the origins a score imports scripts from, and
//! the native orchestra switch.
//!
//! The engine refuses `await import('https://...')` from an origin that is
//! not trusted. The page reads as a switch: Enter permits a refused origin,
//! and Space flips a row where it stands, so a permission can be taken back
//! without the origin leaving the page. A refused or turned-off origin
//! stays listed until the studio closes; an origin trust is kept in the
//! settings.

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

    /// The rows of the permissions page: the trusted origins, then the
    /// refused.
    pub(super) fn permission_rows(&self) -> Vec<PermissionRow> {
        let row = |allowed| {
            move |origin: &String| PermissionRow {
                origin: origin.clone(),
                allowed,
            }
        };
        self.prefs
            .import_origins
            .iter()
            .map(row(true))
            .chain(self.blocked_imports.iter().map(row(false)))
            .collect()
    }

    /// Enter on a row of the permissions page.
    pub(super) fn permit_row(&mut self, at: usize) {
        let Some(row) = self.permission_rows().into_iter().nth(at) else {
            return;
        };
        if row.allowed {
            self.status = format!("{} is trusted · Space turns it off", row.origin);
            return;
        }
        self.blocked_imports
            .retain(|blocked| *blocked != row.origin);
        if !self.prefs.import_origins.contains(&row.origin) {
            self.prefs.import_origins.push(row.origin.clone());
            self.import_origins_changed();
        }
        // The refusal on the footer names an origin that is trusted now.
        for owner in [ErrorOwner::Evaluation, ErrorOwner::Setup] {
            if self
                .errors
                .get(owner)
                .is_some_and(|message| message.contains(&row.origin))
            {
                self.clear_error(owner);
            }
        }
        self.status = format!("{} trusted · update the score to load it", row.origin);
    }

    /// Space on a row of the permissions page: permit a refused origin, or
    /// take the permission back where the row stands. The origin stays on
    /// the page, so the permission can come back with one more Space.
    pub(super) fn toggle_row(&mut self, at: usize) {
        let Some(row) = self.permission_rows().into_iter().nth(at) else {
            return;
        };
        if !row.allowed {
            self.permit_row(at);
            return;
        }
        self.prefs
            .import_origins
            .retain(|origin| *origin != row.origin);
        self.import_origins_changed();
        // The row stays: it moves to the refused ones, which the studio
        // keeps until it closes, so a trust taken back can be given again.
        self.blocked_imports
            .retain(|blocked| *blocked != row.origin);
        if self.blocked_imports.len() >= MAX_BLOCKED_IMPORTS {
            self.blocked_imports.remove(0);
        }
        self.blocked_imports.push(row.origin.clone());
        self.status = format!("{} off · its scripts no longer load", row.origin);
    }

    /// Delete on a row of the permissions page.
    pub(super) fn forget_row(&mut self, at: usize) {
        let Some(row) = self.permission_rows().into_iter().nth(at) else {
            return;
        };
        if row.allowed {
            self.prefs
                .import_origins
                .retain(|origin| *origin != row.origin);
            self.import_origins_changed();
            self.status = format!("{} removed · its scripts no longer load", row.origin);
        } else {
            self.blocked_imports
                .retain(|blocked| *blocked != row.origin);
            self.status = format!("{} dismissed", row.origin);
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
