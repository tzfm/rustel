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
            if !job.is_audio() {
                self.status = format!(
                    "exporting {} - a MIDI file finishes on its own",
                    job.scene_name
                );
            } else if !job.finished_early() {
                // The tail is long enough: fade over a tenth of a second and
                // keep everything rendered so far.
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
        // What play runs, the bounce runs: both setups, global first, as
        // stored - an unsaved edit in a setup tab is the engine's business,
        // not the bounce's.
        let score = ExportScore {
            source,
            mini: self.options.mini,
            prebakes: super::super::prebake::PrebakeScope::ALL
                .map(|scope| self.stored_prebake(scope).to_owned()),
            csound: self
                .snapshot
                .as_ref()
                .map(|snapshot| snapshot.csound.clone())
                .unwrap_or_default(),
            samples: ExportSamples {
                bank_renames: self.prefs.bank_renames(),
                source_bank_renames: self.prefs.source_bank_renames(),
                global_sources: self.prefs.global_sources(),
                set_folder: Some(self.scenes.directory().to_path_buf()),
            },
        };
        match ExportJob::start(
            sheet.scene_name.clone(),
            score,
            settings,
            super::super::export::session_config(
                &self.options.session,
                self.ui_settings.max_polyphony,
                self.worker.master().max_polyphony_override(),
                &self.prefs.import_origins,
                self.csound_allowed,
            ),
            sheet.target.clone(),
        ) {
            Ok(job) => {
                let early = if settings.format.is_audio() {
                    format!(
                        " - {} ends it early and keeps the file",
                        self.keybinds.hint(BindAction::Export)
                    )
                } else {
                    String::new()
                };
                self.status = format!(
                    "exporting {} ({}) → {}{early}",
                    job.scene_name,
                    settings.describe_length(),
                    status_file_path(&job.target, self.ui_settings.show_full_paths),
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

#[cfg(test)]
mod tests {
    mod export_sheet {
        //! Tests for `app/export_sheet.rs`.

        use crate::app::tests::support::*;
        use crate::app::*;
        use crate::export::ExportFormat;

        #[test]
        fn a_click_on_the_export_sheet_chooses_what_it_lands_on() {
            use crate::export::{ExportSheetView, Field, Length};
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = crate::app::tests::support::app_over(directory.path());
            app.frame = Rect::new(0, 0, 120, 40);
            app.toggle_export_sheet();
            let sheet = app.export_sheet.as_ref().expect("the sheet opens");
            let layout = ExportSheetView::layout(sheet, app.frame).expect("room");
            let click = |app: &mut App, column: u16, row: u16| {
                for kind in [
                    MouseEventKind::Down(MouseButton::Left),
                    MouseEventKind::Up(MouseButton::Left),
                ] {
                    app.handle_mouse(MouseEvent {
                        kind,
                        column,
                        row,
                        modifiers: KeyModifiers::NONE,
                    })
                    .expect("mouse");
                }
            };
            click(&mut app, layout.until_silence.x, layout.until_silence.y);
            let sheet = app.export_sheet.as_ref().expect("still open");
            assert!(matches!(sheet.settings.length, Length::UntilSilence { .. }));
            assert_eq!(sheet.field, Field::Length);
            assert_eq!(app.focus, Focus::Panel(PanelKind::Export));

            let layout = ExportSheetView::layout(sheet, app.frame).expect("room");
            // A build without the encoder draws no mp3 radio to click.
            if rustel_runtime::MP3_EXPORT {
                click(&mut app, layout.mp3.x, layout.mp3.y);
                let sheet = app.export_sheet.as_ref().expect("still open");
                assert_eq!(sheet.settings.format, ExportFormat::Mp3);
            }
            click(&mut app, layout.limiter_on.x, layout.limiter_on.y);
            let sheet = app.export_sheet.as_ref().expect("still open");
            assert!(sheet.settings.limiter_enabled);
            assert_eq!(sheet.field, Field::Limiter);
            click(&mut app, layout.limiter_off.x, layout.limiter_off.y);
            assert!(!app.export_sheet.as_ref().unwrap().settings.limiter_enabled);
            click(&mut app, layout.target.x + 2, layout.target.y);
            assert_eq!(
                app.export_sheet.as_ref().expect("open").field,
                Field::Target
            );
            // The border claims the sheet without changing anything.
            click(&mut app, layout.panel.x, layout.panel.y);
            assert_eq!(
                app.export_sheet.as_ref().expect("open").field,
                Field::Target
            );
        }

        #[test]
        fn each_export_opens_with_the_current_mixer_limiter() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = app_over(directory.path());
            let settings = rustel_audio::LimiterSettings {
                threshold_db: -9.0,
                character: rustel_audio::LimiterCharacter::Punchy,
            };
            app.ui_settings.master_limiter_on = true;
            app.ui_settings.master_limiter_makeup = true;
            for state in [
                SetLimiter::Defer,
                SetLimiter::Says {
                    bypassed: false,
                    settings,
                },
                SetLimiter::Says {
                    bypassed: true,
                    settings,
                },
                SetLimiter::None,
            ] {
                app.scenes.set_limiter(state.clone());
                let expected = rustel_audio::RenderLimiter {
                    settings: app.desk_limiter(),
                    makeup: true,
                };
                let enabled = app.live_master_limiter().is_some();
                app.toggle_export_sheet();
                let sheet = app.export_sheet.as_mut().expect("open");
                assert_eq!(sheet.settings.limiter_enabled, enabled, "{state:?}");
                assert_eq!(sheet.settings.limiter, expected);
                sheet.settings.limiter_enabled = !enabled;
                app.export_settings = sheet.settings;
                app.toggle_export_sheet();
                assert_eq!(
                    app.scenes.limiter(),
                    &state,
                    "export does not edit the mixer"
                );
                app.toggle_export_sheet();
                assert_eq!(
                    app.export_sheet.as_ref().unwrap().settings.limiter_enabled,
                    enabled,
                    "reopening follows the mixer, not the last bounce"
                );
                app.toggle_export_sheet();
            }
        }

        #[test]
        fn the_export_folder_is_remembered_for_the_next_sheet() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = crate::app::tests::support::app_over(directory.path());
            app.frame = Rect::new(0, 0, 120, 40);
            let bounces = directory.path().join("bounces");
            std::fs::create_dir_all(&bounces).expect("folder");
            app.remember_export_directory(&bounces.join("take.wav"));
            assert_eq!(
                app.prefs.export_directory.as_deref(),
                Some(bounces.to_string_lossy().as_ref())
            );
            app.toggle_export_sheet();
            let target = app.export_sheet.as_ref().expect("open").target.clone();
            assert!(target.starts_with(&bounces), "{}", target.display());
            // A relative target teaches nothing.
            app.remember_export_directory(std::path::Path::new("exports/take.wav"));
            assert_eq!(
                app.prefs.export_directory.as_deref(),
                Some(bounces.to_string_lossy().as_ref())
            );
        }
    }
}
