//! External Vim mapping preferences and reload notices.

use super::*;

impl App {
    pub(super) fn refresh_vim_config(&mut self) {
        let changed = self.vim_config.configure(
            self.ui_settings.editor_keybindings == crate::editor::EditorKeybindings::Vim,
            self.ui_settings.vim_mappings_path.as_deref(),
            Instant::now(),
        );
        if changed {
            self.sync_editor_keybindings();
        }
        self.show_vim_config_notice();
    }

    pub(super) fn poll_vim_config(&mut self, now: Instant) {
        if self.vim_config.poll(now) {
            self.sync_editor_keybindings();
        }
        self.show_vim_config_notice();
    }

    fn show_vim_config_notice(&mut self) {
        if let Some(notice) = self.vim_config.take_notice() {
            if self.vim_config.error().is_some() {
                self.log.push(LogLevel::Warn, "keybinds", notice.clone());
            }
            self.status = notice;
            self.dirty_frame = true;
        }
    }

    pub(super) fn vim_mappings_path_label(&self) -> String {
        crate::vim_config::path(self.ui_settings.vim_mappings_path.as_deref())
            .map(|path| path.to_string_lossy().into_owned())
            .unwrap_or_else(|| "default path unavailable".into())
    }

    pub(super) fn set_vim_mappings_path(&mut self, path: Option<String>) {
        self.ui_settings.vim_mappings_path = path;
        self.prefs.set_ui_settings(&self.ui_settings);
        self.save_prefs_soon();
        self.refresh_vim_config();
        self.status = self
            .vim_config
            .error()
            .map(str::to_owned)
            .unwrap_or_else(|| {
                "Vim mappings file kept · external changes reload automatically".into()
            });
        self.dirty_frame = true;
    }
}
