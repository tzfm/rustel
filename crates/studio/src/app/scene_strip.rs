//! The scene strip: the row of scene chips above the score. This file covers
//! choosing a scene for the focused pane, stepping to the previous or next
//! scene, launching a scene into the playing pane, creating, duplicating,
//! renaming and closing scenes, the per-scene rewind-on-play flag, the keys the
//! strip takes while a rename is being typed or a pad is being learned,
//! working out which chip is under the mouse, and what every path onto a
//! scene ends with.

use super::*;

impl App {
    /// Keys the scene strip consumes while it is renaming or learning.
    pub(super) fn handle_strip_key(&mut self, code: KeyCode, primary: bool) -> bool {
        match &mut self.strip_mode {
            SceneStripMode::Idle => false,
            SceneStripMode::Learning => {
                if code == KeyCode::Esc {
                    self.strip_mode = SceneStripMode::Idle;
                    self.status = "learn cancelled".into();
                    self.dirty_frame = true;
                    true
                } else {
                    false
                }
            }
            SceneStripMode::Renaming(draft) => {
                match code {
                    KeyCode::Esc => {
                        self.strip_mode = SceneStripMode::Idle;
                        self.status = "rename cancelled".into();
                    }
                    KeyCode::Enter => {
                        let name = draft.clone();
                        self.finish_rename(&name);
                    }
                    KeyCode::Backspace => {
                        self.rename_untouched = false;
                        draft.pop();
                    }
                    KeyCode::Char(character) if !primary && draft.chars().count() < 40 => {
                        // The old name is shown as a starting point; typing
                        // replaces it, as a selected name would be.
                        if self.rename_untouched {
                            draft.clear();
                            self.rename_untouched = false;
                        }
                        draft.push(character);
                    }
                    _ => {}
                }
                self.dirty_frame = true;
                true
            }
        }
    }

    /// Put another scene in the focused pane. Nothing about playback
    /// changes: the engine keeps sounding whatever it was given. A scene
    /// already showing in the other pane is not opened twice; the caret
    /// goes there instead.
    pub(super) fn select_scene(&mut self, index: usize) -> bool {
        // A slider armed in one scene must not catch arrows meant for
        // another.
        self.armed_slider = None;
        let Some(scene) = self.scenes.scenes().get(index) else {
            return false;
        };
        let id = scene.id;
        // A tape has the studio to itself: choosing a score leaves the
        // replay view first, and finds the score again once the tape is
        // gone from the set.
        if self.replay_view.is_some() && !self.scenes.get(id).is_some_and(|scene| scene.is_replay())
        {
            self.close_replay();
            let Some(index) = self.scenes.index_of(id) else {
                return false;
            };
            return self.select_scene(index);
        }
        if let Some(other) = self.panes.iter().position(|pane| pane.scene == id)
            && other != self.focused
        {
            self.focus_pane(other);
            return true;
        }
        if !self.scenes.select(index) {
            return false;
        }
        self.close_smart_action_on_scene_change();
        self.note_scene_visit(id);
        self.panes[self.focused].scene = id;
        self.strip_mode = SceneStripMode::Idle;
        self.invalidate_maps();
        self.pointer = None;
        self.rebuild_slider_spans();
        self.lint_pending = true;
        // Which tab has the caret is part of how the set was left, so the
        // set file takes it now rather than at some tidy moment that a
        // crash or a kill would never reach.
        self.persist_manifest();
        let scene = self.scenes.current();
        self.status = format!("scene {} - {}", index + 1, scene.name());
        self.dirty_frame = true;
        true
    }

    /// A click on a scene chip puts the scene in the focused pane. A scene
    /// the other pane shows stays there, and the caret stays here: a scene
    /// has one caret, so two panes do not show the same scene.
    pub(super) fn click_scene_chip(&mut self, index: usize) {
        let elsewhere = self.scenes.scenes().get(index).and_then(|scene| {
            let pane = self.panes.iter().position(|pane| pane.scene == scene.id)?;
            (pane != self.focused).then(|| (pane, scene.name()))
        });
        let Some((pane, name)) = elsewhere else {
            self.select_scene(index);
            return;
        };
        self.status = format!(
            "{name:?} is open in the {} pane - {} goes there",
            if pane == 0 { "left" } else { "right" },
            self.shortcut_or_menu(BindAction::HopPane, "View > Switch pane")
        );
        self.dirty_frame = true;
    }

    pub(super) fn step_scene(&mut self, delta: isize) {
        if self.replay_view.is_some() {
            self.status = format!(
                "the tape has the studio to itself - {} goes back to the set",
                self.shortcut_or_menu(BindAction::CloseScene, "Scene > Close scene")
            );
            self.dirty_frame = true;
            return;
        }
        let len = self.scenes.len();
        if len < 2 {
            self.status = format!(
                "this set has one scene - {} adds another",
                self.shortcut_or_menu(BindAction::NewScene, "Scene > New scene")
            );
            self.dirty_frame = true;
            return;
        }
        // Cycling replaces only the focused pane. It skips the scene in the
        // other pane, so that pane and the musician's caret stay where they
        // were.
        let current = self.scenes.current_index() as isize;
        let next = (1..len).find_map(|offset| {
            let index =
                (current + delta.signum() * offset as isize).rem_euclid(len as isize) as usize;
            let id = self.scenes.scenes()[index].id;
            (!self.panes.iter().any(|pane| pane.scene == id)).then_some(index)
        });
        if let Some(next) = next {
            self.select_scene(next);
        } else {
            self.status = format!(
                "both scenes are already open - {} adds another",
                self.shortcut_or_menu(BindAction::NewScene, "Scene > New scene")
            );
            self.dirty_frame = true;
        }
    }

    /// Play a scene: what a pad or a Shift+click does. It lands in the pane
    /// the last update came from, so the pane you are writing in is left
    /// alone - unless nothing has played yet, in which case it is this one.
    /// It launches the way the settings say: on the next cycle line, or now
    /// when `launch on` is off.
    pub(super) fn launch_scene(&mut self, index: usize) -> Result<(), RuntimeError> {
        // A prebake is setup, not music: there is nothing here to launch.
        if let Some(scope) = self
            .scenes
            .scenes()
            .get(index)
            .and_then(super::super::scenes::Scene::prebake)
        {
            self.select_scene(index);
            self.status = format!(
                "{} is setup - {} applies it",
                scope.tab_name(),
                self.keybinds.hint(BindAction::Evaluate)
            );
            self.dirty_frame = true;
            return Ok(());
        }
        self.next_launch = match self.ui_settings.quantise.unit_cycles() {
            Some(unit_cycles) => Launch::Quantised { unit_cycles },
            None => Launch::Now,
        };
        let target = self
            .playing_pane
            .filter(|pane| *pane < self.panes.len())
            .unwrap_or(self.focused);
        if target != self.focused {
            self.focus_pane(target);
        }
        self.select_scene(index);
        self.dispatch_editor(Command::Evaluate)
    }

    pub(super) fn new_scene(&mut self, duplicate: bool) {
        // A new scene is the set's; a tape gives the studio back first.
        if self.replay_view.is_some() && !duplicate {
            self.close_replay();
        }
        if duplicate && self.scenes.current().is_replay() {
            self.status = format!(
                "a replay is a tape, not a score - {} makes an empty scene",
                self.shortcut_or_menu(BindAction::NewScene, "Scene > New scene")
            );
            self.dirty_frame = true;
            return;
        }
        // Duplicating setup into a score would make a score of something
        // that plays nothing.
        if duplicate && let Some(scope) = self.current_prebake() {
            self.status = format!(
                "{} is setup, not a score - {} makes an empty scene",
                scope.tab_name(),
                self.shortcut_or_menu(BindAction::NewScene, "Scene > New scene")
            );
            self.dirty_frame = true;
            return;
        }
        let source = if duplicate {
            self.editor().source()
        } else {
            String::new()
        };
        let created = if duplicate {
            self.scenes.duplicate_current(&source)
        } else {
            self.scenes.create(&source)
        };
        match created {
            Ok(id) => {
                let index = self.scenes.current_index();
                self.panes[self.focused].scene = id;
                self.reconcile_panes();
                self.strip_mode = SceneStripMode::Idle;
                self.rebuild_slider_spans();
                self.persist_manifest();
                self.refresh_set_panel(None);
                self.status = format!(
                    "scene {} - {} ({}, written beside the set); {} names it",
                    index + 1,
                    self.scenes.current().name(),
                    if duplicate { "a copy" } else { "empty" },
                    self.shortcut_or_menu(BindAction::RenameScene, "Scene > Rename scene")
                );
            }
            // A full set is not a fault: it is a thing to do something
            // about, like every other refusal the status line carries. It
            // used to take the red ribbon, which is sticky, and so stayed
            // up long after scenes had been closed and the set had room
            // again. A real failure to write one still gets the ribbon.
            Err(SceneError::Full) => {
                self.status = format!("{} - close one first", SceneError::Full);
            }
            Err(error) => self.set_error(ErrorOwner::Interface, error.to_string()),
        }
        self.dirty_frame = true;
    }

    pub(super) fn begin_rename(&mut self) {
        if self.scenes.current().is_replay() {
            self.status = "a replay is named by its tape".into();
            self.dirty_frame = true;
            return;
        }
        // A prebake's name says which of the two it is. There is no third.
        if let Some(scope) = self.current_prebake() {
            self.status = format!("{} keeps its name", scope.tab_name());
            self.dirty_frame = true;
            return;
        }
        let name = self.scenes.current().name();
        self.strip_mode = SceneStripMode::Renaming(name);
        self.rename_untouched = true;
        self.status = "renaming - type the new name, Enter keeps it, Esc cancels".into();
        self.dirty_frame = true;
    }

    pub(super) fn finish_rename(&mut self, name: &str) {
        let before = self.scenes.current().name();
        let scene_id = self.scenes.current().id;
        let dirty = self.scenes.current().dirty;
        let revision = self.scenes.current().editor.revision();
        let source = Arc::<str>::from(self.scenes.current().editor.source());
        match self.scenes.rename_current(name) {
            Ok(()) => {
                self.refresh_set_panel(None);
                let after = self.scenes.current().name();
                self.strip_mode = SceneStripMode::Idle;
                // An in-flight save still targets the old path. Queue a write
                // to the renamed file so the scene the strip shows is the one
                // that receives the bytes; handle_save_result ignores a
                // completion whose path no longer matches.
                if dirty || before != after {
                    self.write_scene(scene_id, revision, source);
                }
                self.persist_manifest();
                self.status = if before == after {
                    format!("scene stays {after:?}")
                } else {
                    format!("renamed {before:?} to {after:?}")
                };
            }
            Err(error) => {
                // Restore the actual name. Keep rename mode active so the
                // next character replaces the name and leaves the score intact.
                self.strip_mode = SceneStripMode::Renaming(before.clone());
                self.rename_untouched = true;
                self.set_error(
                    ErrorOwner::Interface,
                    format!("could not rename {before:?}: {error}"),
                );
            }
        }
        self.dirty_frame = true;
    }

    pub(super) fn close_scene(&mut self) {
        if let Some(scope) = self.current_prebake() {
            return self.close_prebake(scope);
        }
        if self.scenes.current().is_replay() {
            return self.close_replay();
        }
        // A dirty scene is written before it closes, and the file stays in
        // the folder. A scene that cannot be written stays open: its text is
        // nowhere else, and the save error says why.
        let scene = self.scenes.current();
        if scene.dirty {
            let (id, revision, source) = (
                scene.id,
                scene.editor.revision(),
                Arc::<str>::from(scene.editor.source()),
            );
            self.write_scene(id, revision, source);
            self.wait_for_saves();
            if let Some(scene) = self.scenes.get(id).filter(|scene| scene.unsaved()) {
                self.status = format!(
                    "{:?} is not closed - its text could not be saved",
                    scene.name()
                );
                self.dirty_frame = true;
                return;
            }
        }
        if self.scenes.score_count() == 1 {
            // Keep a score available while closing the old one normally.
            let current = self.scenes.current_index();
            if let Err(error) = self.scenes.create("") {
                self.set_error(ErrorOwner::Interface, error.to_string());
                self.dirty_frame = true;
                return;
            }
            self.scenes.select(current);
        }
        match self.scenes.close_current() {
            Ok(closed) => {
                self.forget_scene(closed.id);
                self.reconcile_panes();
                self.strip_mode = SceneStripMode::Idle;
                self.rebuild_slider_spans();
                self.persist_manifest();
                // Reopening the file will not restore a pad attached to this tab.
                let pad = if closed.pad.is_some() {
                    " · its pad is forgotten"
                } else {
                    ""
                };
                self.status = format!(
                    "closed {:?} - its file stays in the set; {} lists it{pad}",
                    closed.name(),
                    self.keybinds.hint(BindAction::SetPanel)
                );
                self.refresh_set_panel(None);
            }
            Err(error) => self.set_error(ErrorOwner::Interface, error.to_string()),
        }
        self.dirty_frame = true;
    }

    /// Flip whether this scene is played from its own cycle zero.
    ///
    /// `Scene ▸ Rewind on play`, ^⇧U, and the ⟲ the chip wears when it is
    /// on. The flag belongs to the scene and is written into the set
    /// file, so one pad mapped to one scene plays it the way that scene
    /// wants to be played - there is no second pad for the rewinding
    /// kind, and no setting that quietly changes what every key does.
    pub(super) fn toggle_scene_rewind(&mut self) {
        if self.scenes.current().is_replay() {
            self.status = "a replay is on its own clock - there is nothing to rewind".into();
            self.dirty_frame = true;
            return;
        }
        let Some(rewinds) = self.scenes.toggle_rewind() else {
            if let Some(scope) = self.current_prebake() {
                self.status = format!("{} is setup, not a scene that plays", scope.tab_name());
                self.dirty_frame = true;
            }
            return;
        };
        self.persist_manifest();
        let name = self.scenes.current().name();
        self.status = if rewinds {
            format!("{name:?} plays from its own cycle 0 \u{21ba}")
        } else {
            format!("{name:?} joins the cycle already running")
        };
        self.dirty_frame = true;
    }

    pub(super) fn scene_chip_at(&self, x: u16, y: u16) -> Option<usize> {
        let hit = view::scene_strip_hits(self.regions.scenes, &self.scene_chips())
            .iter()
            .position(|hit| within(*hit, x, y))?;
        // Under a tape the strip holds the tape's chip alone; its scene
        // is wherever the set keeps it.
        if self.replay_view.is_some() {
            return self
                .scenes
                .scenes()
                .iter()
                .position(|scene| scene.is_replay());
        }
        Some(hit)
    }

    /// What every path onto a scene ends with: the focused pane shows the
    /// set's current scene, and everything laid out for the old one is
    /// built again for this one.
    pub(super) fn land_on_current_scene(&mut self) {
        self.close_smart_action_on_scene_change();
        let id = self.scenes.current().id;
        self.panes[self.focused].scene = id;
        // Reopening a tab from the set panel gives it a new id; count that
        // visit when ordering recently used tabs.
        self.note_scene_visit(id);
        self.strip_mode = SceneStripMode::Idle;
        self.armed_slider = None;
        self.invalidate_maps();
        self.pointer = None;
        self.rebuild_slider_spans();
        self.lint_pending = true;
        self.dirty_frame = true;
    }

    /// The strip's chips, as drawn and as hit-tested.
    pub(super) fn scene_chips(&self) -> Vec<SceneChip> {
        let playing_scene = self.is_playing().then_some(self.audible_scene).flatten();
        // Under a tape the strip is the tape's alone.
        self.scenes
            .scenes()
            .iter()
            .enumerate()
            .filter(|(_, scene)| self.replay_view.is_none() || scene.is_replay())
            .map(|(index, scene)| SceneChip {
                prebake: scene.prebake(),
                replay: scene.is_replay(),
                name: scene.name(),
                current: index == self.scenes.current_index(),
                // A click on this chip does not choose the scene; it says
                // which pane holds it. The chip says that first, in colour.
                other_pane: self
                    .panes
                    .iter()
                    .enumerate()
                    .any(|(pane, shown)| pane != self.focused && shown.scene == scene.id),
                playing: playing_scene == Some(scene.id),
                dirty: scene.dirty,
                rewind: scene.rewind,
                pad: scene.pad.map(Pad::label),
                errors: self.lint.get(&scene.id).is_some_and(|result| {
                    result.input_channels == self.lint_input_channels() && lint_has_problems(result)
                }),
                armed: (self.armed_scene == Some(scene.id))
                    .then(|| {
                        self.snapshot
                            .as_ref()
                            .and_then(|snapshot| snapshot.launch)
                            .map(|launch| launch.cycles_left)
                    })
                    .flatten(),
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    mod scene_strip {
        //! Tests for `app/scene_strip.rs`.

        use crate::app::*;

        /// A rename moves the scene file and saves its new path in the manifest.
        #[test]
        fn renaming_a_new_scene_keeps_its_file_and_manifest_in_step() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = crate::app::tests::support::app_over(directory.path());
            app.new_scene(false);
            let old_path = directory.path().join("scene 1.strudel");
            let new_path = directory.path().join("buffy-tmp.strudel");
            assert!(old_path.exists(), "new scenes are written at once");

            app.begin_rename();
            crate::app::tests::support::type_text(&mut app, "buffy-tmp");
            crate::app::tests::support::press(&mut app, KeyCode::Enter, KeyModifiers::NONE);
            assert!(matches!(app.strip_mode, SceneStripMode::Idle));
            assert_eq!(app.scenes.current().path, new_path);
            assert!(new_path.exists(), "rename moved the empty file");
            assert!(!old_path.exists(), "the old path was left behind");

            let manifest = crate::scenes::SetManifest::load(directory.path()).expect("set file");
            assert_eq!(manifest.current.as_deref(), Some("buffy-tmp.strudel"));
            assert_eq!(manifest.scenes[1].file, "buffy-tmp.strudel");

            app.dispatch_editor(Command::InsertText("// saved after rename\n".into()))
                .expect("edit renamed scene");
            app.dispatch_editor(Command::Evaluate)
                .expect("update renamed scene");
            app.wait_for_saves();
            assert_eq!(
                std::fs::read_to_string(&new_path).unwrap(),
                "// saved after rename\n"
            );
            assert!(
                !old_path.exists(),
                "an update did not recreate the old file"
            );

            assert!(app.select_scene(0));
            assert!(app.select_scene(1));
            assert_eq!(app.scenes.current().name(), "buffy-tmp");
            let reopened =
                crate::scenes::SceneSet::open(directory.path(), "starter").expect("reopen");
            assert_eq!(reopened.current().path, new_path);
            assert_eq!(
                reopened.current().editor.source(),
                "// saved after rename\n"
            );
        }

        /// A failed rename restores the actual name and keeps edits in rename mode.
        #[test]
        fn a_failed_rename_shows_the_name_the_scene_still_has() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = crate::app::tests::support::app_over(directory.path());
            app.new_scene(false);
            let old_path = app.scenes.current().path.clone();
            std::fs::write(directory.path().join("taken.strudel"), "// another file")
                .expect("occupied target");

            app.begin_rename();
            crate::app::tests::support::type_text(&mut app, "taken");
            crate::app::tests::support::press(&mut app, KeyCode::Enter, KeyModifiers::NONE);

            let error = app.errors.visible().expect("the refusal is reported");
            assert!(error.contains(r#"could not rename "scene 1""#), "{error}");
            assert!(
                matches!(&app.strip_mode, SceneStripMode::Renaming(draft) if draft == "scene 1")
            );
            assert_eq!(app.scenes.current().path, old_path);
            assert_eq!(app.scenes.current().name(), "scene 1");
            let manifest = crate::scenes::SetManifest::load(directory.path()).expect("set file");
            assert_eq!(manifest.current.as_deref(), Some("scene 1.strudel"));

            let score = app.editor().source();
            crate::app::tests::support::type_text(&mut app, "other");
            assert_eq!(app.editor().source(), score, "the score was edited");
            assert!(matches!(&app.strip_mode, SceneStripMode::Renaming(draft) if draft == "other"));
            crate::app::tests::support::press(&mut app, KeyCode::Enter, KeyModifiers::NONE);
            app.wait_for_saves();
            assert!(matches!(app.strip_mode, SceneStripMode::Idle));
            assert_eq!(app.scenes.current().name(), "other");
            assert_eq!(app.editor().source(), score);
            assert!(directory.path().join("other.strudel").exists());
            assert!(!old_path.exists());
        }

        /// A copy of a scene numbered at the top of the range takes the last number
        /// instead of overflowing past it.
        #[test]
        fn duplicating_the_highest_numbered_scene_does_not_overflow() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = crate::app::tests::support::app_over(directory.path());
            let name = format!("x {}", usize::MAX - 1);
            app.finish_rename(&name);
            app.new_scene(true);
            assert_eq!(app.scenes.current().name(), format!("x {}", usize::MAX));
        }

        /// An unrelated orphan must not make the first copy generic, and a
        /// same-name orphan must be kept when later copies continue the sequence.
        #[test]
        fn duplicate_uses_the_source_name_without_replacing_an_orphan() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = crate::app::tests::support::app_over(directory.path());
            app.finish_rename("encore");
            let source = app.editor().source();
            let unrelated_orphan = directory.path().join("scene 2.strudel");
            std::fs::write(&unrelated_orphan, "// unrelated orphan\n").expect("orphan file");

            app.new_scene(true);
            assert_eq!(app.scenes.current().name(), "encore 2");
            assert_eq!(app.editor().source(), source);
            assert_eq!(
                std::fs::read_to_string(directory.path().join("encore 2.strudel")).unwrap(),
                source
            );
            assert_eq!(
                std::fs::read_to_string(&unrelated_orphan).unwrap(),
                "// unrelated orphan\n"
            );

            app.new_scene(true);
            assert_eq!(app.scenes.current().name(), "encore 3");
            assert_eq!(app.editor().source(), source);
            let orphan = directory.path().join("encore 4.strudel");
            std::fs::write(&orphan, "// same-name orphan\n").expect("orphan file");
            app.new_scene(true);
            assert_eq!(app.scenes.current().name(), "encore 5");
            assert_eq!(
                std::fs::read_to_string(&orphan).unwrap(),
                "// same-name orphan\n"
            );
            let manifest = crate::scenes::SetManifest::load(directory.path()).expect("set file");
            assert_eq!(manifest.current.as_deref(), Some("encore 5.strudel"));
            let reopened =
                crate::scenes::SceneSet::open(directory.path(), "encore").expect("reopen");
            assert_eq!(reopened.current().name(), "encore 5");

            app.new_scene(false);
            assert_eq!(app.scenes.current().name(), "scene 1");
            assert!(app.editor().source().is_empty());
        }

        /// A full set is a thing to do something about, not a fault. It took
        /// the red ribbon, which is sticky, so it stayed on screen long after
        /// scenes had been closed and the set had room again.
        #[test]
        fn a_full_set_says_so_without_leaving_a_mark() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = crate::app::tests::support::app_over(directory.path());
            while app.scenes.len() < crate::scenes::MAX_SCENES {
                app.new_scene(false);
                assert!(app.errors.visible().is_none(), "{:?}", app.errors.visible());
            }

            app.new_scene(false);
            assert_eq!(app.scenes.len(), crate::scenes::MAX_SCENES);
            assert!(
                app.status.contains("close one first"),
                "the status says what to do: {}",
                app.status
            );
            assert!(
                app.errors.visible().is_none(),
                "and nothing sticks: {:?}",
                app.errors.visible()
            );
        }

        /// No click plays a scene: no modifier, no second click, no right
        /// button. The OS or the terminal can claim Ctrl+click, the right click
        /// and Shift+click before the studio sees them. A pad, the Scene menu or
        /// an update plays a scene.
        #[test]
        fn no_click_on_a_scene_chip_plays_it() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = crate::app::tests::support::app_over(directory.path());
            app.frame = Rect::new(0, 0, 120, 40);
            app.regions.scenes = Rect::new(0, 1, 120, 1);
            app.new_scene(false);
            let second = app.scenes.current().id;
            app.scenes.select(0);
            app.panes[0].scene = app.scenes.current().id;

            let chips = app.scene_chips();
            let hits = view::scene_strip_hits(app.regions.scenes, &chips);
            let target = hits.get(1).copied().expect("a chip for the second scene");
            let press = |kind, modifiers| MouseEvent {
                kind,
                column: target.x,
                row: target.y,
                modifiers,
            };

            for (kind, modifiers) in [
                (MouseEventKind::Down(MouseButton::Left), KeyModifiers::NONE),
                (
                    MouseEventKind::Down(MouseButton::Left),
                    KeyModifiers::CONTROL,
                ),
                (MouseEventKind::Down(MouseButton::Left), KeyModifiers::SHIFT),
                // Twice over, which is a double click at this distance and
                // inside the window a double click is counted in.
                (MouseEventKind::Down(MouseButton::Left), KeyModifiers::NONE),
                (MouseEventKind::Down(MouseButton::Right), KeyModifiers::NONE),
            ] {
                app.handle_mouse(press(kind, modifiers)).expect("click");
                assert!(
                    app.pending_evaluation.is_none() && app.inflight.is_empty(),
                    "{kind:?} with {modifiers:?} played a scene"
                );
            }

            // What a click does do: it chooses the scene it landed on.
            assert_eq!(app.scenes.current().id, second);
        }

        /// Only the chip of the scene the other pane holds is marked, so
        /// the strip can say which clicks only report where a scene is.
        #[test]
        fn chips_mark_which_scene_the_other_pane_holds() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = crate::app::tests::support::app_over(directory.path());
            app.new_scene(false);
            app.toggle_split();
            app.focus_pane(0);
            let other = app.panes[1].scene;
            let held = app
                .scenes
                .get(other)
                .expect("the other pane's scene")
                .name();

            let chips = app.scene_chips();
            assert_eq!(
                chips
                    .iter()
                    .filter(|chip| chip.other_pane)
                    .map(|chip| chip.name.as_str())
                    .collect::<Vec<_>>(),
                vec![held.as_str()],
                "the other pane's scene alone is marked"
            );
            assert!(
                !chips
                    .iter()
                    .find(|chip| chip.current)
                    .expect("a current chip")
                    .other_pane
            );
        }
    }
}
