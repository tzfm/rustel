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
        if let Some(job) = self.export_job.as_mut() {
            // The tail is long enough: fade over a tenth of a second and
            // keep everything rendered so far. Over the open sheet this is
            // the render window's own abort.
            if !job.finished_early() {
                job.finish();
                self.status = format!("ending the export of {} - fading out", job.scene_name);
            }
            self.dirty_frame = true;
            return;
        }
        if self.export_sheet.is_some() {
            self.close_export_sheet();
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
            // Over a running bounce the settings face is not drawn, so its
            // controls claim no clicks.
            if self.export_job.is_none()
                && let (Some(hit), Some(sheet)) = (hit, self.export_sheet.as_mut())
            {
                sheet.click(hit);
            }
            self.dirty_frame = true;
            return true;
        }
        false
    }

    /// Returns true when the sheet consumed the key.
    pub(super) fn handle_export_key(&mut self, code: KeyCode, primary: bool, shift: bool) -> bool {
        if self.export_sheet.is_none() {
            return false;
        }
        // Over a running bounce the export chord is the render window's
        // abort, so it falls through to the action, which ends the bounce
        // early and keeps the file.
        if primary && shift && matches!(code, KeyCode::Char('x' | 'X')) && self.export_job.is_none()
        {
            self.close_export_sheet();
            return true;
        }
        if primary {
            // Transport chords still belong to the transport.
            return false;
        }
        if self.export_job.is_some() && !matches!(code, KeyCode::Esc) {
            // No settings stand over a running bounce: the render face's
            // hint says Esc runs on in the header, and the export chord
            // ends it early and keeps the file.
            self.dirty_frame = true;
            return true;
        }
        let Some(sheet) = self.export_sheet.as_mut() else {
            return false;
        };
        match sheet.key(code, shift) {
            SheetAction::Nothing => {}
            SheetAction::Close => self.close_export_sheet(),
            SheetAction::Render => self.start_export(),
        }
        self.dirty_frame = true;
        true
    }

    /// Close the export sheet. A bounce under it is not stopped: it runs
    /// on, and the header carries its progress.
    pub(super) fn close_export_sheet(&mut self) {
        let running = self.export_job.is_some();
        self.export_sheet = None;
        self.status = if running {
            "export closed - the bounce runs on, the header shows it".into()
        } else {
            "export closed".into()
        };
        self.settle_focus();
        self.dirty_frame = true;
    }

    fn start_export(&mut self) {
        let (settings, scene_name, target) = match self.export_sheet.as_mut() {
            Some(sheet) => {
                let settings = sheet.settings();
                (settings, sheet.scene_name.clone(), sheet.target.clone())
            }
            None => return,
        };
        self.export_settings = settings;
        self.remember_export_directory(&target);
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
            scene_name,
            score,
            settings,
            super::super::export::session_config(
                &self.options.session,
                self.ui_settings.max_polyphony,
                self.worker.master().max_polyphony_override(),
                &self.prefs.import_origins,
                &self.prefs.sample_grants(),
                self.csound_allowed,
            ),
            target,
        ) {
            Ok(job) => {
                self.status = format!(
                    "exporting {} ({}) → {} - {} ends it early and keeps the file",
                    job.scene_name,
                    settings.describe_length(),
                    status_file_path(&job.output(), self.ui_settings.show_full_paths),
                    self.keybinds.hint(BindAction::Export)
                );
                self.log.push(
                    LogLevel::Info,
                    "export",
                    format!("{} → {}", job.scene_name, job.output().display()),
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
            // The bounce runs: its progress moves, in the sheet's render
            // face and in the header chip, so every poll draws again.
            self.dirty_frame = true;
            return;
        };
        let job = self.export_job.take().expect("checked");
        // The render window closes on its outcome, as a DAW's does, and
        // the result lands on the status line.
        if self.export_sheet.take().is_some() {
            self.settle_focus();
        }
        match outcome {
            Ok(seconds) => {
                self.status = format!(
                    "exported {} - {} ({}{}, {:.1}s to render)",
                    job.scene_name,
                    status_file_path(&job.output(), self.ui_settings.show_full_paths),
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
                        job.output().display(),
                        format_take_length(seconds)
                    ),
                );
                self.status.push_str(" - click the path to show it");
                self.remember_status_file(job.output());
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
                assert!(sheet.settings.mp3);
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

        /// The sheet stays open over the bounce it starts, as a DAW's render
        /// window does, and the keys change nothing while it runs.
        #[test]
        fn the_sheet_stays_open_over_its_bounce_and_esc_leaves_it_running() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = app_over(directory.path());
            app.frame = Rect::new(0, 0, 120, 40);
            app.toggle_export_sheet();
            assert!(app.handle_export_key(crossterm::event::KeyCode::Enter, false, false));
            assert!(app.export_job.is_some(), "the bounce started");
            assert!(app.export_sheet.is_some(), "the sheet stays open over it");
            assert_eq!(app.focus, Focus::Panel(PanelKind::Export));
            // No setting stands while it runs: the key is taken and changes
            // nothing, so a second Enter cannot start a second bounce.
            assert!(app.handle_export_key(crossterm::event::KeyCode::Tab, false, false));
            assert!(app.handle_export_key(crossterm::event::KeyCode::Enter, false, false));
            let job = app.export_job.as_ref().expect("the one bounce");
            assert!(!job.finished_early(), "Enter over the run changed nothing");
            // Esc closes the sheet; the bounce runs on in the header.
            assert!(app.handle_export_key(crossterm::event::KeyCode::Esc, false, false));
            assert!(app.export_sheet.is_none());
            assert!(app.export_job.is_some(), "the bounce runs on");
            assert!(app.status.contains("runs on"), "{}", app.status);
            let landed = std::time::Instant::now() + std::time::Duration::from_secs(60);
            while app.export_job.is_some() && std::time::Instant::now() < landed {
                app.poll_export();
                if app.export_job.is_some() {
                    std::thread::sleep(std::time::Duration::from_millis(50));
                }
            }
            assert!(app.export_job.is_none(), "the bounce never landed");
            assert!(app.status.starts_with("exported"), "{}", app.status);
        }

        /// Over a running bounce the sheet's Vim text modes do not apply:
        /// `i` enters no Insert, a letter edits no hidden field, and the
        /// first Esc closes the sheet as the render face says.
        #[test]
        fn vim_text_modes_stand_down_over_a_running_bounce() {
            use crate::export::Field;
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = app_over(directory.path());
            app.frame = Rect::new(0, 0, 120, 40);
            app.ui_settings.editor_keybindings = crate::editor::EditorKeybindings::Vim;
            app.sync_editor_keybindings();
            app.toggle_export_sheet();
            app.export_sheet.as_mut().expect("open").field = Field::Target;
            let target = app.export_sheet.as_ref().expect("open").target.clone();
            press(&mut app, KeyCode::Enter, KeyModifiers::NONE);
            assert!(app.export_job.is_some(), "the bounce started");

            press(&mut app, KeyCode::Char('i'), KeyModifiers::NONE);
            assert!(
                !app.export_sheet.as_ref().expect("open").vim_insert(),
                "i entered no Insert"
            );
            press(&mut app, KeyCode::Char('x'), KeyModifiers::NONE);
            assert_eq!(
                app.export_sheet.as_ref().expect("open").target,
                target,
                "the hidden target is untouched"
            );
            press(&mut app, KeyCode::Esc, KeyModifiers::NONE);
            assert!(app.export_sheet.is_none(), "the first Esc closed the sheet");
            assert!(app.export_job.is_some(), "the bounce runs on");
            let landed = std::time::Instant::now() + std::time::Duration::from_secs(60);
            while app.export_job.is_some() && std::time::Instant::now() < landed {
                app.poll_export();
                if app.export_job.is_some() {
                    std::thread::sleep(std::time::Duration::from_millis(50));
                }
            }
            assert!(app.export_job.is_none(), "the bounce never landed");
        }

        /// The render window closes on its outcome, and the export chord
        /// over it ends the bounce early, as the status says.
        #[test]
        fn the_render_window_closes_on_its_outcome_and_the_chord_ends_it_early() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = app_over(directory.path());
            app.frame = Rect::new(0, 0, 120, 40);
            app.toggle_export_sheet();
            assert!(app.handle_export_key(crossterm::event::KeyCode::Enter, false, false));
            // The chord over the open render window ends the bounce, and
            // leaves the window open to watch the fade land.
            app.toggle_export_sheet();
            assert!(app.export_sheet.is_some(), "the window stays for the fade");
            assert!(
                app.export_job
                    .as_ref()
                    .expect("still running")
                    .finished_early()
            );
            assert!(app.status.contains("ending the export"), "{}", app.status);
            let landed = std::time::Instant::now() + std::time::Duration::from_secs(60);
            while app.export_job.is_some() && std::time::Instant::now() < landed {
                app.poll_export();
                if app.export_job.is_some() {
                    std::thread::sleep(std::time::Duration::from_millis(50));
                }
            }
            assert!(app.export_job.is_none(), "the bounce never landed");
            assert!(
                app.export_sheet.is_none(),
                "the window closed with the outcome"
            );
            assert!(app.status.starts_with("exported"), "{}", app.status);
            assert!(app.status.contains("ended early"), "{}", app.status);
        }
    }
}
