//! Confirming deletion of a single host-owned sample from the browser.

use super::*;

impl App {
    /// Alt+D in the Samples browser: arm deletion of the selected local
    /// file, or say why it cannot be deleted.
    pub(super) fn request_selected_sample_delete(&mut self) {
        let Some(panel) = self.reference_panel.as_ref() else {
            return;
        };
        let Some((bank, variant)) = panel.selected_sample_file() else {
            self.status = if panel.selected_bank_holds_local_files() {
                "select a sample with → and ↓ before deleting".into()
            } else {
                "delete only touches your own local samples".into()
            };
            self.dirty_frame = true;
            return;
        };
        let Some(library) = self.worker.library() else {
            self.status = "the sample library is not up yet".into();
            self.dirty_frame = true;
            return;
        };
        match library.local_sample_deletion_path(&bank, variant) {
            Ok(path) if self.sample_file_busy(&path) => {
                self.status = "finish recording or trimming before deleting this sample".into();
            }
            Ok(path) => {
                let name = path.file_name().unwrap_or_default().to_string_lossy();
                self.status = format!("delete {name} permanently? Enter deletes · Esc cancels");
                if let Some(panel) = self.reference_panel.as_mut() {
                    panel.confirm_delete = Some((bank, variant, path));
                }
            }
            Err(error) => self.status = error,
        }
        self.dirty_frame = true;
    }

    /// Enter on an armed deletion: delete the file while the selection and
    /// the file are still the ones that were armed.
    pub(super) fn confirm_sample_delete(&mut self, pending: (String, usize, PathBuf)) {
        let (bank, variant, expected) = pending;
        let selected = self
            .reference_panel
            .as_ref()
            .and_then(ReferencePanel::selected_sample_file);
        if selected
            .as_ref()
            .is_none_or(|(name, index)| name != &bank || *index != variant)
        {
            self.status = format!(
                "sample selection changed - select the file and press {} again",
                self.keybinds.hint(BindAction::DeleteSample)
            );
        } else if self.sample_file_busy(&expected) {
            self.status = "finish recording or trimming before deleting this sample".into();
        } else if let Some(library) = self.worker.library() {
            match library.delete_local_sample(&bank, variant, &expected) {
                Ok(path) => {
                    // A browser preview may be sounding the deleted file.
                    self.stop_preview();
                    self.refresh_catalogue();
                    self.lint_pending = true;
                    let name = path.file_name().unwrap_or_default().to_string_lossy();
                    self.status = format!("deleted {name}");
                }
                Err(error) => self.status = error,
            }
        } else {
            self.status = "the sample library is not up yet".into();
        }
        self.dirty_frame = true;
    }

    /// Commands handled before the reference panel must also disarm deletion.
    pub(super) fn cancel_sample_delete_for_event(&mut self, event: &Event) {
        let cancel = match event {
            Event::Key(key) if key.kind == KeyEventKind::Release => false,
            Event::Key(key) => {
                !(key.kind == KeyEventKind::Press
                    && key.modifiers.is_empty()
                    && matches!(key.code, KeyCode::Enter | KeyCode::Esc)
                    && self.focus == Focus::Panel(PanelKind::Reference))
            }
            Event::Mouse(mouse) => mouse.kind != MouseEventKind::Moved,
            Event::Paste(_) | Event::FocusLost => true,
            _ => false,
        };
        if cancel
            && let Some(panel) = self.reference_panel.as_mut()
            && panel.confirm_delete.take().is_some()
        {
            self.status = "sample deletion cancelled".into();
            self.dirty_frame = true;
        }
    }
}

#[cfg(test)]
mod tests {
    mod sample_delete {
        //! Deleting a sample through the browser's keyboard confirmation.

        use crate::app::tests::support::{app_over, precision_key, select_bank_row};
        use crate::app::*;
        use rustel_runtime::samples::SoundOrigin;

        /// The browser open on a search for `name`, its cursor on that bank.
        fn open_bank(app: &mut App, name: &str) {
            let sounds = app.worker.library().expect("library").catalogue();
            let mut panel = ReferencePanel::samples(&app.reference, sounds);
            panel.paste_query(&app.reference, name);
            select_bank_row(&mut panel, name);
            app.reference_panel = Some(panel);
            app.focus = Focus::Panel(PanelKind::Reference);
        }

        fn import_bank(app: &mut App, root: &std::path::Path, files: &[&str]) -> Vec<PathBuf> {
            let bank = root.join("deletable");
            std::fs::create_dir_all(&bank).expect("bank directory");
            let paths = files
                .iter()
                .map(|name| {
                    let path = bank.join(name);
                    std::fs::write(&path, b"local sample fixture").expect("sample");
                    path
                })
                .collect();
            app.add_sample_sources(&[root.display().to_string()]);
            app.settle_sample_sources();
            open_bank(app, "deletable");
            paths
        }

        fn arm(app: &mut App) {
            precision_key(app, KeyCode::Char('d'), KeyModifiers::ALT);
            assert!(
                app.reference_panel
                    .as_ref()
                    .unwrap()
                    .confirm_delete
                    .is_some(),
                "deletion did not arm: {}",
                app.status
            );
        }

        #[test]
        fn alt_d_waits_for_enter_then_removes_the_local_file_and_bank() {
            let directory = tempfile::tempdir().unwrap();
            let imported = tempfile::tempdir().unwrap();
            let mut app = app_over(directory.path());
            let paths = import_bank(&mut app, imported.path(), &["solo.wav"]);
            let source = app.editor().source();

            arm(&mut app);
            assert!(paths[0].exists(), "arming must not delete the file");
            assert!(app.status.contains("Enter"), "{}", app.status);
            assert!(app.status.contains("Esc"), "{}", app.status);
            precision_key(&mut app, KeyCode::Enter, KeyModifiers::NONE);

            assert!(!paths[0].exists());
            assert!(!app.worker.library().unwrap().knows("deletable"));
            let panel = app.reference_panel.as_ref().expect("browser stays open");
            assert_eq!(panel.confirm_delete, None);
            assert_eq!(
                app.editor().source(),
                source,
                "confirm did not insert a sound"
            );
            assert_eq!(app.prefs.sample_sources.len(), 1, "the import is retained");
        }

        #[test]
        fn deleting_a_folders_last_sample_keeps_the_receipt_and_raises_no_alert() {
            let directory = tempfile::tempdir().unwrap();
            let imported = tempfile::tempdir().unwrap();
            let mut app = app_over(directory.path());
            let paths = import_bank(&mut app, imported.path(), &["solo.wav"]);
            let warnings = app.log.at_least(LogLevel::Warn);

            arm(&mut app);
            precision_key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
            app.settle_sample_sources();

            assert!(!paths[0].exists());
            assert_eq!(app.status, "deleted solo.wav");
            assert_eq!(app.log.at_least(LogLevel::Warn), warnings);
        }

        #[test]
        fn escape_keeps_the_sample_and_plain_d_searches() {
            let directory = tempfile::tempdir().unwrap();
            let imported = tempfile::tempdir().unwrap();
            let mut app = app_over(directory.path());
            let paths = import_bank(&mut app, imported.path(), &["solo.wav"]);

            arm(&mut app);
            precision_key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
            let panel = app
                .reference_panel
                .as_ref()
                .expect("Esc keeps the browser open");
            assert_eq!(panel.confirm_delete, None);
            assert!(paths[0].exists());
            let query = panel.sound_query.clone();

            precision_key(&mut app, KeyCode::Char('d'), KeyModifiers::NONE);
            let panel = app.reference_panel.as_ref().unwrap();
            assert_eq!(panel.sound_query, query + "d");
            assert_eq!(panel.confirm_delete, None);
            assert!(paths[0].exists());
        }

        /// In Vim the Esc that leaves Insert calls an armed deletion off, as
        /// Esc in Command does. The next Enter deletes nothing.
        #[test]
        fn the_esc_that_leaves_vim_insert_cancels_deletion() {
            let directory = tempfile::tempdir().unwrap();
            let imported = tempfile::tempdir().unwrap();
            let mut app = app_over(directory.path());
            app.ui_settings.editor_keybindings = crate::editor::EditorKeybindings::Vim;
            app.sync_editor_keybindings();
            let paths = import_bank(&mut app, imported.path(), &["solo.wav"]);

            precision_key(&mut app, KeyCode::Char('i'), KeyModifiers::NONE);
            assert!(app.reference_panel.as_ref().unwrap().vim_insert());
            arm(&mut app);
            precision_key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
            let panel = app.reference_panel.as_ref().expect("the browser stays");
            assert!(!panel.vim_insert(), "Esc left Insert");
            assert_eq!(panel.confirm_delete, None, "Esc cancelled the deletion");
            precision_key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
            assert!(paths[0].exists(), "Enter after the Esc deleted nothing");
        }

        #[test]
        fn moving_pasting_clicking_and_modified_enter_cancel_deletion() {
            let directory = tempfile::tempdir().unwrap();
            let imported = tempfile::tempdir().unwrap();
            let mut app = app_over(directory.path());
            let paths = import_bank(&mut app, imported.path(), &["solo.wav"]);
            let mouse = |kind| {
                Event::Mouse(MouseEvent {
                    kind,
                    column: 0,
                    row: 0,
                    modifiers: KeyModifiers::NONE,
                })
            };
            let events = [
                Event::Key(crossterm::event::KeyEvent::new(
                    KeyCode::Down,
                    KeyModifiers::NONE,
                )),
                Event::Key(crossterm::event::KeyEvent::new(
                    KeyCode::Enter,
                    KeyModifiers::SHIFT,
                )),
                Event::Key(crossterm::event::KeyEvent::new(
                    KeyCode::Char('d'),
                    KeyModifiers::CONTROL,
                )),
                Event::Paste("x".into()),
                mouse(MouseEventKind::Down(MouseButton::Left)),
                mouse(MouseEventKind::ScrollDown),
            ];
            for event in events {
                open_bank(&mut app, "deletable");
                arm(&mut app);
                let description = format!("{event:?}");
                app.handle_terminal_event(event).expect("cancelling event");
                assert!(
                    app.reference_panel
                        .as_ref()
                        .is_none_or(|panel| panel.confirm_delete.is_none()),
                    "{description} left the deletion armed"
                );
                precision_key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
                assert!(paths[0].exists(), "{description} confirmed deletion");
            }
        }

        #[test]
        fn a_multi_sample_bank_requires_a_variant_and_only_deletes_that_file() {
            let directory = tempfile::tempdir().unwrap();
            let imported = tempfile::tempdir().unwrap();
            let mut app = app_over(directory.path());
            let paths = import_bank(&mut app, imported.path(), &["a.wav", "b.wav"]);

            precision_key(&mut app, KeyCode::Char('d'), KeyModifiers::ALT);
            assert_eq!(app.reference_panel.as_ref().unwrap().confirm_delete, None);
            assert!(app.status.contains("select a sample"), "{}", app.status);
            assert!(paths.iter().all(|path| path.exists()));
            precision_key(&mut app, KeyCode::Right, KeyModifiers::NONE);
            precision_key(&mut app, KeyCode::Down, KeyModifiers::NONE);
            assert!(matches!(
                app.reference_panel
                    .as_ref()
                    .unwrap()
                    .sound_rows()
                    .get(app.reference_panel.as_ref().unwrap().sound_selected),
                Some(SoundRow::Variant(_, 0))
            ));

            arm(&mut app);
            precision_key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
            assert!(!paths[0].exists());
            assert!(paths[1].exists());
            let library = app.worker.library().unwrap();
            let bank = library
                .catalogue()
                .into_iter()
                .find(|entry| entry.name == "deletable")
                .unwrap();
            assert_eq!(bank.variants, 1);
            assert_eq!(bank.variant_names, vec!["b"]);
        }

        #[test]
        fn a_bank_of_remote_files_is_not_offered_for_deletion() {
            let directory = tempfile::tempdir().unwrap();
            let imported = tempfile::tempdir().unwrap();
            let mut app = app_over(directory.path());
            let paths = import_bank(&mut app, imported.path(), &["a.wav", "b.wav"]);
            // A pack's variants are URLs, which name no local file.
            let panel = app.reference_panel.as_mut().unwrap();
            panel
                .sounds
                .iter_mut()
                .find(|entry| entry.name == "deletable")
                .unwrap()
                .variant_names
                .iter_mut()
                .for_each(String::clear);

            precision_key(&mut app, KeyCode::Char('d'), KeyModifiers::ALT);
            assert_eq!(app.reference_panel.as_ref().unwrap().confirm_delete, None);
            assert!(app.status.contains("own local samples"), "{}", app.status);
            assert!(paths.iter().all(|path| path.exists()));
        }

        #[test]
        fn a_sets_own_sample_can_be_deleted() {
            let directory = tempfile::tempdir().unwrap();
            let bank = directory.path().join("own");
            std::fs::create_dir(&bank).unwrap();
            let path = bank.join("solo.wav");
            std::fs::write(&path, b"local sample fixture").unwrap();
            let mut app = app_over(directory.path());
            app.adopt_set_samples();
            open_bank(&mut app, "own");
            assert_eq!(
                app.reference_panel.as_ref().unwrap().selected_bank(),
                Some(("own".into(), SoundOrigin::Set))
            );

            arm(&mut app);
            precision_key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
            assert!(!path.exists());
            assert!(!app.worker.library().unwrap().knows("own"));
        }

        #[test]
        fn score_and_builtin_rows_never_arm_deletion() {
            let directory = tempfile::tempdir().unwrap();
            let imported = tempfile::tempdir().unwrap();
            let mut app = app_over(directory.path());
            let paths = import_bank(&mut app, imported.path(), &["solo.wav"]);

            for origin in [
                SoundOrigin::Score,
                SoundOrigin::Default,
                SoundOrigin::Font,
                SoundOrigin::Synth,
                SoundOrigin::Input,
            ] {
                open_bank(&mut app, "deletable");
                let panel = app.reference_panel.as_mut().unwrap();
                panel
                    .sounds
                    .iter_mut()
                    .find(|entry| entry.name == "deletable")
                    .unwrap()
                    .origin = origin;
                precision_key(&mut app, KeyCode::Char('d'), KeyModifiers::ALT);
                assert_eq!(app.reference_panel.as_ref().unwrap().confirm_delete, None);
                assert!(paths[0].exists(), "{origin:?} deleted a file");
            }
        }

        #[test]
        fn importing_the_download_cache_does_not_make_its_files_deletable() {
            let directory = tempfile::tempdir().unwrap();
            let mut app = app_over(directory.path());
            let cache = rustel_runtime::samples::sample_cache_dir();
            std::fs::create_dir_all(&cache).unwrap();
            let imported = tempfile::tempdir_in(cache).unwrap();
            let paths = import_bank(&mut app, imported.path(), &["cached.wav"]);

            precision_key(&mut app, KeyCode::Char('d'), KeyModifiers::ALT);
            assert_eq!(app.reference_panel.as_ref().unwrap().confirm_delete, None);
            assert!(app.status.contains("downloaded"), "{}", app.status);
            precision_key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
            assert!(paths[0].exists());
        }

        #[test]
        fn a_file_removed_after_confirmation_was_requested_reports_failure() {
            let directory = tempfile::tempdir().unwrap();
            let imported = tempfile::tempdir().unwrap();
            let mut app = app_over(directory.path());
            let paths = import_bank(&mut app, imported.path(), &["solo.wav"]);
            arm(&mut app);
            std::fs::remove_file(&paths[0]).unwrap();

            precision_key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
            assert_eq!(app.reference_panel.as_ref().unwrap().confirm_delete, None);
            assert!(app.status.contains("cannot read sample"), "{}", app.status);
        }

        #[test]
        fn recording_blocks_both_requesting_and_confirming_deletion() {
            let directory = tempfile::tempdir().unwrap();
            let imported = tempfile::tempdir().unwrap();
            let mut app = app_over(directory.path());
            let paths = import_bank(&mut app, imported.path(), &["solo.wav"]);

            app.pending_recording = Some(1);
            precision_key(&mut app, KeyCode::Char('d'), KeyModifiers::ALT);
            assert_eq!(app.reference_panel.as_ref().unwrap().confirm_delete, None);
            assert!(app.status.contains("finish recording"), "{}", app.status);
            assert!(paths[0].exists());

            app.pending_recording = None;
            arm(&mut app);
            app.pending_sample = Some(2);
            precision_key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
            assert_eq!(app.reference_panel.as_ref().unwrap().confirm_delete, None);
            assert!(app.status.contains("finish recording"), "{}", app.status);
            assert!(paths[0].exists());
        }

        #[test]
        fn trimming_the_file_blocks_both_requesting_and_confirming_deletion() {
            let directory = tempfile::tempdir().unwrap();
            let imported = tempfile::tempdir().unwrap();
            let mut app = app_over(directory.path());
            let paths = import_bank(&mut app, imported.path(), &["solo.wav"]);
            let trim_target = || TrimTarget::Sample {
                name: "deletable".into(),
                path: paths[0].clone(),
                decoded: HashSet::new(),
            };

            app.start_trim(trim_target());
            precision_key(&mut app, KeyCode::Char('d'), KeyModifiers::ALT);
            assert_eq!(app.reference_panel.as_ref().unwrap().confirm_delete, None);
            assert!(app.status.contains("trimming"), "{}", app.status);
            assert!(paths[0].exists());

            app.trim_jobs.clear();
            arm(&mut app);
            app.start_trim(trim_target());
            precision_key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
            assert_eq!(app.reference_panel.as_ref().unwrap().confirm_delete, None);
            assert!(app.status.contains("trimming"), "{}", app.status);
            assert!(paths[0].exists());
        }

        #[cfg(unix)]
        #[test]
        fn trimming_through_a_symlinked_recording_folder_blocks_deletion() {
            let directory = tempfile::tempdir().unwrap();
            let imported = tempfile::tempdir().unwrap();
            let mut app = app_over(directory.path());
            let paths = import_bank(&mut app, imported.path(), &["solo.wav"]);
            let alias = directory.path().join("recording-folder");
            std::os::unix::fs::symlink(imported.path(), &alias).unwrap();

            arm(&mut app);
            app.start_trim(TrimTarget::Take {
                path: alias.join("deletable/solo.wav"),
                sample: false,
            });
            precision_key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
            assert_eq!(app.reference_panel.as_ref().unwrap().confirm_delete, None);
            assert!(app.status.contains("trimming"), "{}", app.status);
            assert!(paths[0].exists());

            precision_key(&mut app, KeyCode::Char('d'), KeyModifiers::ALT);
            assert_eq!(app.reference_panel.as_ref().unwrap().confirm_delete, None);
            assert!(app.status.contains("trimming"), "{}", app.status);
            assert!(paths[0].exists());
        }

        #[test]
        fn a_selection_changed_without_a_key_cannot_confirm_the_old_sample() {
            let directory = tempfile::tempdir().unwrap();
            let imported = tempfile::tempdir().unwrap();
            let mut app = app_over(directory.path());
            let paths = import_bank(&mut app, imported.path(), &["a.wav", "b.wav"]);
            precision_key(&mut app, KeyCode::Right, KeyModifiers::NONE);
            precision_key(&mut app, KeyCode::Down, KeyModifiers::NONE);
            arm(&mut app);

            // A catalogue refresh can change the selected row without a key event
            // passing through the ordinary cancellation route.
            let panel = app.reference_panel.as_mut().unwrap();
            panel.sound_selected = panel
                .sound_rows()
                .iter()
                .position(|row| matches!(row, SoundRow::Variant(_, 1)))
                .unwrap();
            precision_key(&mut app, KeyCode::Enter, KeyModifiers::NONE);

            assert_eq!(app.reference_panel.as_ref().unwrap().confirm_delete, None);
            assert!(app.status.contains("selection changed"), "{}", app.status);
            assert!(paths.iter().all(|path| path.exists()));
        }

        #[test]
        fn a_reindexed_variant_does_not_delete_the_file_that_replaced_it() {
            let directory = tempfile::tempdir().unwrap();
            let imported = tempfile::tempdir().unwrap();
            let mut app = app_over(directory.path());
            let paths = import_bank(&mut app, imported.path(), &["a.wav", "b.wav"]);
            precision_key(&mut app, KeyCode::Right, KeyModifiers::NONE);
            precision_key(&mut app, KeyCode::Down, KeyModifiers::NONE);
            arm(&mut app);

            // Another owner removes the armed variant while the prompt is visible.
            // The row still says variant zero, but that address now resolves to b.
            let library = app.worker.library().unwrap();
            let expected = library.local_sample_path("deletable", 0).unwrap();
            library
                .delete_local_sample("deletable", 0, &expected)
                .unwrap();
            precision_key(&mut app, KeyCode::Enter, KeyModifiers::NONE);

            assert_eq!(app.reference_panel.as_ref().unwrap().confirm_delete, None);
            assert!(app.status.contains("sample changed"), "{}", app.status);
            assert!(!paths[0].exists());
            assert!(paths[1].exists(), "the replacement variant is retained");
        }
    }
}
