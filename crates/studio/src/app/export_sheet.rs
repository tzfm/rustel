//! The export sheet (Ctrl+Shift+E): opening, focusing and closing it, the keys
//! and clicks it takes, starting a render of the focused scene to a file, and
//! polling the running export until it lands on the status line and in the
//! log. It also remembers the last export folder so the next export starts
//! there.

use super::*;

impl App {
    /// ⌘⇧E / Ctrl+Shift+E: the export sheet for the focused scene - one
    /// score, as written - or close it.
    pub(super) fn toggle_export_sheet(&mut self) {
        if self.scenes.current().is_replay() && self.export_sheet.is_none() {
            self.status =
                "a replay is not a score to bounce - rustel replay --export does that".into();
            self.dirty_frame = true;
            return;
        }
        // Export bounces a score. Setup makes no sound of its own.
        if let Some(scope) = self.current_prebake()
            && self.export_sheet.is_none()
        {
            self.status = format!(
                "{} is setup, not a score - select a scene to export",
                scope.tab_name()
            );
            self.dirty_frame = true;
            return;
        }
        if self.export_sheet.is_some() && self.focus != Focus::Panel(PanelKind::Export) {
            self.focus_panel(PanelKind::Export);
            return;
        }
        if self.export_sheet.take().is_some() {
            self.status = "export closed".into();
            self.settle_focus();
            self.dirty_frame = true;
            return;
        }
        if let Some(job) = self.export_job.as_mut() {
            // The tail is long enough: fade over a tenth of a second and
            // keep everything rendered so far.
            if !job.finished_early() {
                job.finish();
                self.status = format!("ending the export of {} - fading out", job.scene_name);
            }
            self.dirty_frame = true;
            return;
        }
        self.dismiss_dialogs(Some(PanelKind::Export));
        let scene = self.scenes.current();
        // Where the last export went, while that folder is still there.
        let directory = self
            .prefs
            .export_directory
            .as_deref()
            .map(PathBuf::from)
            .filter(|remembered| remembered.is_absolute() && remembered.is_dir())
            .unwrap_or_else(|| self.scenes.directory().join(EXPORT_DIRECTORY_NAME));
        let seconds = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|since| since.as_secs() as i64)
            .unwrap_or(0);
        self.export_sheet = Some(ExportSheet::open(
            scene.name(),
            ExportSettings {
                limiter_enabled: self.live_master_limiter().is_some(),
                limiter: rustel_audio::RenderLimiter {
                    settings: self.desk_limiter(),
                    makeup: self.ui_settings.master_limiter_makeup,
                },
                ..self.export_settings
            },
            &directory,
            seconds,
        ));
        self.status = format!("export - {} as written; Enter renders", scene.name());
        self.focus_panel(PanelKind::Export);
        self.dirty_frame = true;
    }

    /// On the export sheet a press lands on the control under it; the
    /// border and the hint row still claim the sheet, so the press never
    /// falls through to the score. Returns true when the sheet took it.
    pub(super) fn click_export_sheet(&mut self, x: u16, y: u16) -> bool {
        if self.export_sheet.is_some()
            && super::super::export::ExportSheetView::geometry(self.frame)
                .is_some_and(|sheet| within(sheet, x, y))
        {
            self.focus_panel(PanelKind::Export);
            self.pointer = Some(Pointer::Panel);
            let hit = self.export_sheet.as_ref().and_then(|sheet| {
                super::super::export::ExportSheetView::hit(sheet, self.frame, x, y)
            });
            if let (Some(hit), Some(sheet)) = (hit, self.export_sheet.as_mut()) {
                sheet.click(hit);
            }
            self.dirty_frame = true;
            return true;
        }
        false
    }

    /// Returns true when the sheet consumed the key.
    pub(super) fn handle_export_key(&mut self, code: KeyCode, primary: bool, shift: bool) -> bool {
        let Some(sheet) = self.export_sheet.as_mut() else {
            return false;
        };
        if primary && shift && matches!(code, KeyCode::Char('x' | 'X')) {
            self.export_sheet = None;
            self.settle_focus();
            self.dirty_frame = true;
            return true;
        }
        if primary {
            // Transport chords still belong to the transport.
            return false;
        }
        match sheet.key(code, shift) {
            SheetAction::Nothing => {}
            SheetAction::Close => {
                self.export_sheet = None;
                self.status = "export closed".into();
                self.settle_focus();
            }
            SheetAction::Render => self.start_export(),
        }
        self.dirty_frame = true;
        true
    }

    fn start_export(&mut self) {
        let Some(mut sheet) = self.export_sheet.take() else {
            return;
        };
        let settings = sheet.settings();
        self.export_settings = settings;
        self.remember_export_directory(&sheet.target.clone());
        let scene = self.scenes.current();
        let source = scene.editor.source().to_string();
        match ExportJob::start(
            sheet.scene_name.clone(),
            source,
            self.options.mini,
            settings,
            super::super::export::session_config(
                &self.options.session,
                self.ui_settings.max_polyphony,
                self.worker.master().max_polyphony_override(),
            ),
            sheet.target.clone(),
        ) {
            Ok(job) => {
                self.status = format!(
                    "exporting {} ({}) → {} - {} ends it early and keeps the file",
                    job.scene_name,
                    settings.describe_length(),
                    status_file_path(&job.target, self.ui_settings.show_full_paths),
                    self.keybinds.hint(BindAction::Export)
                );
                self.log.push(
                    LogLevel::Info,
                    "export",
                    format!("{} → {}", job.scene_name, job.target.display()),
                );
                self.export_job = Some(job);
            }
            Err(error) => {
                self.status = format!("cannot export: {error}");
                self.log.push(LogLevel::Error, "export", error.to_string());
            }
        }
        self.dirty_frame = true;
    }

    /// The folder an export goes to is where the next one starts.
    pub(super) fn remember_export_directory(&mut self, target: &std::path::Path) {
        let Some(parent) = target.parent().filter(|parent| parent.is_absolute()) else {
            return;
        };
        let remembered = Some(parent.display().to_string());
        if self.prefs.export_directory != remembered {
            self.prefs.export_directory = remembered;
            self.save_prefs_soon();
        }
    }

    /// A finished render lands on the status line and in the log; a failed
    /// one quietly, the way a take's does.
    pub(super) fn poll_export(&mut self) {
        let Some(job) = self.export_job.as_ref() else {
            return;
        };
        let Some(outcome) = job.poll() else {
            return;
        };
        let job = self.export_job.take().expect("checked");
        match outcome {
            Ok(seconds) => {
                self.status = format!(
                    "exported {} - {} ({}{}, {:.1}s to render)",
                    job.scene_name,
                    status_file_path(&job.target, self.ui_settings.show_full_paths),
                    format_take_length(seconds),
                    if job.finished_early() {
                        ", ended early"
                    } else {
                        ""
                    },
                    job.started.elapsed().as_secs_f64()
                );
                self.log.push(
                    LogLevel::Info,
                    "export",
                    format!(
                        "done {} ({})",
                        job.target.display(),
                        format_take_length(seconds)
                    ),
                );
                self.status.push_str(" - click the path to show it");
                self.remember_status_file(job.target.clone());
            }
            Err(error) => {
                self.status = format!("export of {} failed: {error}", job.scene_name);
                self.log.push(
                    LogLevel::Error,
                    "export",
                    format!("{} failed: {error}", job.scene_name),
                );
            }
        }
        self.dirty_frame = true;
    }
}
