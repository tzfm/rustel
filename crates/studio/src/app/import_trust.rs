//! The Trust page: the origins a score imports scripts from.
//!
//! The engine refuses `await import('https://...')` from an origin that is
//! not trusted. The studio keeps each refused origin until it closes, and
//! the page offers it, so the player trusts an origin with one key.

use super::super::settings::TrustRow;
use super::super::worker::EngineFailure;
use super::App;
use super::transport::ErrorOwner;

/// The most refused origins the page keeps. A score names one or two.
const MAX_BLOCKED_IMPORTS: usize = 16;

impl App {
    /// What a refused import says on the footer. The origin goes on the
    /// Trust page. `None` for every other failure.
    pub(super) fn note_untrusted_import(&mut self, failure: &EngineFailure) -> Option<String> {
        if failure.kind != "untrusted-import" {
            return None;
        }
        // The engine's message opens with the origin.
        let origin = failure.message.split_whitespace().next()?;
        let origin = rustel_runtime::import_origin(origin).ok()?;
        self.blocked_imports.retain(|blocked| *blocked != origin);
        if self.blocked_imports.len() >= MAX_BLOCKED_IMPORTS {
            self.blocked_imports.remove(0);
        }
        self.blocked_imports.push(origin.clone());
        Some(format!(
            "{origin} is not trusted - Settings → Trust lets its scripts run"
        ))
    }

    /// The rows of the Trust page: the trusted origins, then the refused.
    pub(super) fn trust_rows(&self) -> Vec<TrustRow> {
        let row = |allowed| {
            move |origin: &String| TrustRow {
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

    /// Enter on a row of the Trust page.
    pub(super) fn trust_import(&mut self, at: usize) {
        let Some(row) = self.trust_rows().into_iter().nth(at) else {
            return;
        };
        if row.allowed {
            self.status = format!("{} is trusted · d removes it", row.origin);
            return;
        }
        self.blocked_imports
            .retain(|blocked| *blocked != row.origin);
        self.prefs.import_origins.push(row.origin.clone());
        self.import_origins_changed();
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

    /// Delete on a row of the Trust page.
    pub(super) fn forget_import(&mut self, at: usize) {
        let Some(row) = self.trust_rows().into_iter().nth(at) else {
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
