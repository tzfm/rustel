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
