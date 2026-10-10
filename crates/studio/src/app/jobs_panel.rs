//! The jobs sheet: the list of background jobs (sample downloads, caches,
//! exports), its keys, and the click anywhere off the sheet that closes it.

use super::*;

impl App {
    /// View ▸ Background jobs: a read-only list of running background work.
    /// The score keeps the keyboard; Esc or a click elsewhere closes it.
    pub(super) fn toggle_jobs_panel(&mut self) {
        if self.jobs_panel.is_none() {
            self.dismiss_dialogs(Some(PanelKind::Jobs));
        }
        self.jobs_panel = match &self.jobs_panel {
            Some(_) => None,
            None => {
                self.raise(PanelKind::Jobs);
                self.status = "background jobs - Esc closes".into();
                Some(JobsPanel::opened())
            }
        };
        if self.jobs_panel.is_none() && self.focus == Focus::Panel(PanelKind::Jobs) {
            self.focus = Focus::Editor;
        }
        self.dirty_frame = true;
    }

    /// A press on the jobs sheet is the sheet's; one anywhere else closes
    /// the sheet and goes on to whatever is under it. Returns true when the
    /// sheet took the press.
    pub(super) fn click_jobs_panel(&mut self, x: u16, y: u16) -> bool {
        if self.jobs_panel.is_some() {
            let count = self.background_jobs().len();
            let on_sheet = JobsPanelView::geometry(self.frame, count)
                .map(|(sheet, _)| within(sheet, x, y))
                .unwrap_or(false);
            if on_sheet {
                return true;
            }
            self.close_panel(PanelKind::Jobs);
            self.settle_focus();
            self.dirty_frame = true;
        }
        false
    }

    /// Returns true when the jobs sheet consumed the key.
    pub(super) fn handle_jobs_key(&mut self, code: KeyCode, primary: bool, shift: bool) -> bool {
        let _ = (primary, shift);
        let jobs = self.background_jobs();
        let Some(panel) = self.jobs_panel.as_mut() else {
            return false;
        };
        panel.clamp(jobs.len());
        match code {
            KeyCode::Esc => {
                self.jobs_panel = None;
                self.settle_focus();
            }
            KeyCode::Up => panel.step(-1, jobs.len()),
            KeyCode::Down => panel.step(1, jobs.len()),
            KeyCode::PageUp | KeyCode::PageDown => {
                let rows = JobsPanelView::geometry(self.frame, jobs.len())
                    .map_or(1, |(_, list)| list.height.max(1)) as usize;
                panel.page(code == KeyCode::PageDown, rows, jobs.len());
            }
            _ => return false,
        }
        self.dirty_frame = true;
        true
    }

    /// Every background task currently running - downloads, exports, clears -
    /// for the header chip and the jobs sheet.
    pub(super) fn background_jobs(&self) -> Vec<BackgroundJob> {
        let mut jobs = Vec::new();
        for shipped in &self.shipped_sources {
            if let Some(progress) = &shipped.cache
                && !progress.done()
            {
                let done = progress.total.saturating_sub(progress.left);
                jobs.push(BackgroundJob {
                    name: format!("cache {}", shipped.source.name),
                    percent: Some(super::super::jobs::percent_of(done, progress.total)),
                });
            }
        }
        for user in &self.user_sources {
            if let Some(progress) = &user.cache
                && !progress.done()
            {
                let done = progress.total.saturating_sub(progress.left);
                let name = user
                    .spec
                    .rsplit(['/', '\\', ':'])
                    .find(|part| !part.is_empty())
                    .unwrap_or(user.spec.as_str());
                jobs.push(BackgroundJob {
                    name: format!("cache {name}"),
                    percent: Some(super::super::jobs::percent_of(done, progress.total)),
                });
            }
        }
        if let Some(progress) = &self.precache
            && !progress.done()
            && progress.kind == super::super::settings::PrecacheKind::Library
        {
            let done = progress.total.saturating_sub(progress.left);
            jobs.push(BackgroundJob {
                name: "cache all".to_owned(),
                percent: Some(super::super::jobs::percent_of(done, progress.total)),
            });
        }
        if self.cache_clear.is_some() {
            jobs.push(BackgroundJob {
                name: "clear sample cache".into(),
                percent: None,
            });
        }
        if let Some(job) = self.export_job.as_ref() {
            let glance = job.glance();
            jobs.push(BackgroundJob {
                name: format!("export {}", glance.scene_name),
                percent: glance.percent,
            });
        }
        // The plugin host works on threads of its own: the scan of the
        // plugins with no test yet, and each plugin in its load.
        #[cfg(feature = "vst")]
        {
            if let Some((done, total)) = self.plugin_scan {
                jobs.push(BackgroundJob {
                    name: "scan plugins".into(),
                    percent: Some(super::super::jobs::percent_of(done, total)),
                });
            }
            let loads = self.plugin_loads.iter();
            jobs.extend(loads.map(|name| BackgroundJob {
                name: format!("load {name}"),
                percent: None,
            }));
        }
        // Loader line with no named pack/import progress yet.
        if jobs.is_empty() && self.caching_samples > 0 {
            jobs.push(BackgroundJob {
                name: format!("cache {}", self.caching_samples),
                percent: None,
            });
        }
        jobs
    }
}
