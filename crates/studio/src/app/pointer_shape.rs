//! The mouse cursor shape: which OS pointer (arrow, hand, I-beam, closed
//! hand, resize arrow) the studio asks the terminal for at each screen
//! position, including while a drag is held or the pointer is over the
//! reference column. It sends the shape with OSC 22, maps names to iTerm2's X11
//! vocabulary, and re-sends the shape after Shift or focus changes make the
//! terminal reset it.

use super::*;

/// The OS pointer shapes the studio asks for, by their CSS names: the
/// arrow, the hand over anything a press acts on, the I-beam over text
/// that can be edited or selected, the closed hand while something is
/// being dragged, and the double arrow over an edge that a drag moves.
pub(super) const SHAPE_DEFAULT: &str = "default";
pub(super) const SHAPE_POINTER: &str = "pointer";
pub(super) const SHAPE_TEXT: &str = "text";
pub(super) const SHAPE_GRABBING: &str = "grabbing";
const SHAPE_EW_RESIZE: &str = "ew-resize";

/// The name a terminal wants for a pointer shape. The protocol is kitty's
/// (OSC 22) and its names are CSS's, which Ghostty shares; iTerm2
/// implements the same sequence with the X11 cursor names instead
/// (`hand2`, `xterm`, `arrow`, `fleur`, `sb_h_double_arrow`), and treats
/// any other name as "reset to the arrow" - so the CSS names would have
/// quietly done nothing there.
pub(super) fn pointer_shape_name(terminal: &str, shape: &'static str) -> &'static str {
    if terminal.starts_with("iTerm") {
        match shape {
            SHAPE_POINTER => "hand2",
            SHAPE_TEXT => "xterm",
            SHAPE_GRABBING => "fleur",
            SHAPE_EW_RESIZE => "sb_h_double_arrow",
            _ => "arrow",
        }
    } else {
        shape
    }
}

impl App {
    /// The shape a press held down keeps, wherever the pointer goes: a
    /// drag belongs to what it began on, and the shape says so rather than
    /// flickering over whatever the pointer crosses.
    pub(super) fn held_pointer_shape(&self) -> Option<&'static str> {
        match self.pointer.as_ref()? {
            #[cfg(feature = "hydra")]
            Pointer::GeneratorControl { .. } | Pointer::SnippetCodeScroll { .. } => {
                Some(SHAPE_GRABBING)
            }
            Pointer::Volume
            | Pointer::PreviewVolume
            | Pointer::Minimap { .. }
            | Pointer::Scrollbar
            | Pointer::HorizontalScrollbar { .. }
            | Pointer::Slider { .. }
            | Pointer::MixerFader { .. }
            | Pointer::MixerPanelFader
            | Pointer::Knobs { .. }
            | Pointer::TimelineBar { .. } => Some(SHAPE_GRABBING),
            Pointer::BlockStart { .. } => Some(SHAPE_EW_RESIZE),
            Pointer::Editor
            | Pointer::ThemeCode
            | Pointer::Log
            | Pointer::ReferenceText { .. }
            | Pointer::MixerText { .. }
            | Pointer::PromptField => Some(SHAPE_TEXT),
            Pointer::Panel => None,
        }
    }

    /// The pointer's shape over the reference column, when it is there.
    pub(super) fn reference_pointer_shape(&self, x: u16, y: u16) -> Option<&'static str> {
        const POINTER: &str = SHAPE_POINTER;
        const TEXT: &str = SHAPE_TEXT;
        const DEFAULT: &str = SHAPE_DEFAULT;
        let panel = self.reference_panel.as_ref()?;
        if !within(self.regions.reference, x, y) {
            return None;
        }
        use super::super::reference::{ReferenceMode, Tab};
        let inner = super::super::reference::inner_area(self.regions.reference);
        if super::super::reference::tab_at(inner, panel.tab, x, y).is_some() {
            return Some(POINTER);
        }
        if panel.tab == Tab::Samples {
            let (_, meter_y) = super::super::reference::samples_pulse_rows(inner);
            if y == meter_y {
                return Some(POINTER);
            }
        }
        if panel.text_block_at(&self.reference, inner, x, y).is_some() {
            return Some(TEXT);
        }
        let geometry = panel.geometry(inner);
        let rows = match panel.tab {
            Tab::Reference => match panel.mode {
                ReferenceMode::Browse => panel.browse_rows().len(),
                ReferenceMode::Entry { .. } => 0,
            },
            Tab::Samples => panel.sound_rows().len(),
            Tab::Chords => panel.chord_rows().len(),
            Tab::Scales => panel.scale_rows().len(),
            #[cfg(feature = "vst")]
            Tab::Vst => panel.vst.rows().len(),
            #[cfg(feature = "hydra")]
            Tab::Examples | Tab::Generator => panel.snippet_lines().len(),
        };
        if within(geometry.list, x, y)
            && geometry.first_row + usize::from(y - geometry.list.y) < rows
        {
            return Some(POINTER);
        }
        Some(DEFAULT)
    }

    /// The shape for a position, walking the screen in paint order - what
    /// is on top answers first - and agreeing with `handle_mouse` about
    /// what a press there would do: the hand where a press acts, the
    /// I-beam where it edits or selects text, the double arrow where a
    /// drag moves an edge, the arrow everywhere else.
    pub(super) fn pointer_shape_at(&self, x: u16, y: u16) -> &'static str {
        const POINTER: &str = SHAPE_POINTER;
        const TEXT: &str = SHAPE_TEXT;
        const DEFAULT: &str = SHAPE_DEFAULT;
        #[cfg(feature = "remote-control")]
        if let Some(shape) = self.remote_panel_pointer_shape(x, y) {
            return shape;
        }
        if self.piano_zen_row_at(x, y) {
            return DEFAULT;
        }
        // The keyboard help covers everything and takes no press.
        if self.help.is_some() {
            return DEFAULT;
        }
        // The bar and its dropdown are above the device picker, so they
        // answer for the pointer first.
        if within(self.menu_row(), x, y) {
            return POINTER;
        }
        if within(self.go_chip(), x, y) {
            return POINTER;
        }
        if let Some(state) = self.menu.as_ref()
            && state.is_dropped()
        {
            let menus = self.menus();
            match state.item_at(&menus, self.menu_row(), self.frame, x, y) {
                Some((_, usize::MAX)) => return DEFAULT,
                Some(_) => return POINTER,
                None => {}
            }
        }
        if self.menu.is_none() && super::super::memory::memory_chip_at(x, y) {
            return POINTER;
        }
        if self.replay_edit.is_some() {
            return DEFAULT;
        }
        if let Some(shape) = self.precision_slider_shape(x, y) {
            return shape;
        }
        // The device picker is a popover over everything.
        if let Some(panel) = self.panel {
            let entries = self.panel_row_count(panel.kind);
            if let Some(geometry) = DevicePanelView::geometry(self.frame, panel, entries.max(1)) {
                if geometry.tab_at(x, y).is_some()
                    || geometry.entry_at(x, y).is_some_and(|index| index < entries)
                {
                    return POINTER;
                }
                if within(geometry.area, x, y) {
                    return DEFAULT;
                }
            }
        }
        if self.theme_editor_sheet_visible()
            && let Some(editor) = self.theme_editor.as_ref()
            && let Some(sheet) = super::super::theme_editor::ThemeEditorView::geometry(self.frame)
            && within(sheet, x, y)
        {
            use super::super::theme_editor::{EditorTab, ThemeEditorView};
            let settled = editor.saving.is_none() && editor.picker.is_none();
            if editor.picker.is_some()
                && super::super::theme_editor::picker_cell_at(sheet, x, y).is_some()
            {
                return POINTER;
            }
            if settled && ThemeEditorView::tab_at(self.frame, x, y).is_some() {
                return POINTER;
            }
            if settled
                && editor.tab == EditorTab::Form
                && ThemeEditorView::form_row_at(editor, self.frame, x, y).is_some()
            {
                return POINTER;
            }
            if settled
                && editor.tab == EditorTab::Code
                && ThemeEditorView::code_area(self.frame).is_some_and(|body| within(body, x, y))
            {
                return TEXT;
            }
            return DEFAULT;
        }
        if let Some((_, picker)) = self.set_prompt.as_ref() {
            if picker.row_at(self.frame, x, y).is_some() {
                return POINTER;
            }
            if picker.contains(self.frame, x, y) {
                return DEFAULT;
            }
        }
        if let Some(picker) = self
            .viz_docks
            .iter()
            .flatten()
            .find_map(|panel| panel.prompt.as_ref())
            && picker.contains(self.frame, x, y)
        {
            return DEFAULT;
        }
        if let Some(panel) = self
            .viz_docks
            .iter()
            .flatten()
            .find(|panel| panel.adding.is_some())
        {
            if panel.add_row_at(self.frame, x, y).is_some() {
                return POINTER;
            }
            if panel.add_sheet_contains(self.frame, x, y) {
                return DEFAULT;
            }
        }
        // The reference over the sheets when it was raised after them.
        if self.reference_on_top()
            && let Some(shape) = self.reference_pointer_shape(x, y)
        {
            return shape;
        }
        if let Some(picker) = self.theme_picker.as_ref() {
            if picker.row_at(self.frame, x, y).is_some() {
                return POINTER;
            }
            if picker
                .geometry(self.frame)
                .is_some_and(|(sheet, _)| within(sheet, x, y))
            {
                return DEFAULT;
            }
        }
        if let Some(sheet) = self.settings_sheet {
            let settings_frame = self.settings_sheet_frame();
            // The counts the sources page reckons its rows by, filled here
            // as the key and click paths fill them: a pointer hovering a
            // page nobody has typed on yet still has rows under it.
            let mut sheet = sheet;
            sheet.source_count = self.prefs.sample_sources.len();
            sheet.default_count = self.shipped_sources.len();
            sheet.permission_counts = self.permission_counts();
            if SettingsSheet::tab_at(settings_frame, x, y).is_some()
                || sheet.shows_settings() && sheet.row_at_for(settings_frame, x, y).is_some()
                || sheet.source_row_at(settings_frame, x, y).is_some()
                || sheet.permission_row_at(settings_frame, x, y).is_some()
            {
                return POINTER;
            }
            if super::super::settings::SettingsSheetView::geometry(settings_frame)
                .is_some_and(|(area, _)| within(area, x, y))
            {
                return DEFAULT;
            }
        }
        // The log is an editor of its own: its text selects and copies.
        if self.log_panel.is_some()
            && let Some((sheet, _)) =
                LogPanelView::geometry(self.log_sheet_frame(), self.log_extent())
            && within(sheet, x, y)
        {
            if LogPanelView::list_area(self.log_sheet_frame(), self.log_extent())
                .is_some_and(|list| within(list, x, y))
            {
                return TEXT;
            }
            return DEFAULT;
        }
        if let Some(sheet) = self.export_sheet.as_ref()
            && super::super::export::ExportSheetView::geometry(self.frame)
                .is_some_and(|area| within(area, x, y))
        {
            if super::super::export::ExportSheetView::hit(sheet, self.frame, x, y).is_some() {
                return POINTER;
            }
            return DEFAULT;
        }
        // The reference stays over the docked panels even when a sheet
        // has been raised above it.
        if let Some(shape) = self.reference_pointer_shape(x, y) {
            return shape;
        }
        // Below the reference and every sheet: the docked panels.
        if let Some(index) = self.viz_dock_at(x, y) {
            if self.viz_widget_at(index, x, y).is_some() {
                return POINTER;
            }
            return DEFAULT;
        }
        if let Some(panel) = self.set_panel.as_ref()
            && !self.regions.sidebar_hidden
        {
            if panel
                .row_at(self.regions.sidebar, self.frame, x, y)
                .is_some()
            {
                return POINTER;
            }
            if panel.contains(self.regions.sidebar, self.frame, x, y) {
                return DEFAULT;
            }
        }
        // The mixer's devices block is text to drag out - the MIDI log and
        // the pads - like the reference column's entries.
        if self.mixer_panel.is_some()
            && within(self.regions.mixer, x, y)
            && self
                .mixer_devices_block()
                .is_some_and(|(area, _)| within(area, x, y))
        {
            return TEXT;
        }
        if let Some(shape) = self.mixer_pointer_shape(x, y) {
            return shape;
        }
        if let Some(shape) = self.memory_dock_shape(x, y) {
            return shape;
        }
        if self.scene_chip_at(x, y).is_some() || self.orbit_chip_at(x, y).is_some() {
            return POINTER;
        }
        // The footer: the chips and the fader act on a press; the status
        // line does when it names the last file made; and the error line,
        // like the header's badge, opens the log.
        let hits = self.footer_hits();
        if within(hits.device_chip, x, y)
            || within(hits.midi_chip, x, y)
            || within(hits.meter, x, y)
            || self.status_names_last_file() && within(hits.status, x, y)
            || self.error_line_at(x, y)
            || view::warning_badge_at(x, y)
        {
            return POINTER;
        }
        if self.slider_at(x, y).is_some() {
            return POINTER;
        }
        if self.horizontal_scrollbar_at(x, y).is_some() {
            return POINTER;
        }
        // A tape's strip: a block's start edge drags, the rest presses.
        if self.timeline_block_start_at(x, y).is_some() {
            return SHAPE_EW_RESIZE;
        }
        if self.timeline_bar_at(x, y).is_some() || self.timeline_block_at(x, y).is_some() {
            return POINTER;
        }
        if let Some(index) = self.pane_at(x, y) {
            let pane = &self.regions.panes[index];
            if within(pane.minimap, x, y)
                || self.vertical_scrollbar_at(index, x, y)
                || within(pane.title, x, y)
            {
                return POINTER;
            }
            if within(pane.timeline, x, y) {
                return DEFAULT;
            }
            // The text: the I-beam. Not the numbers down the left, and
            // not a visualizer's rows, where a press places no caret.
            if within(pane.editor, x, y) {
                let scene = self.panes[index].scene;
                let numbered = self.ui_settings.line_numbers;
                let gutter = self
                    .scenes
                    .get(scene)
                    .map_or(0, |scene| view::gutter_width(&scene.editor, numbered));
                if x < pane.editor.x.saturating_add(gutter) {
                    return DEFAULT;
                }
                if let Some(map) = self.panes[index].last_map.as_ref()
                    && let Some(super::super::editor::Hit::Virtual { .. }) =
                        map.hit_test(super::super::editor::CellPoint::new(x, y))
                {
                    return DEFAULT;
                }
            }
            return TEXT;
        }
        DEFAULT
    }

    /// Ask the terminal for the pointer shape the position deserves (OSC
    /// 22: `ESC ] 22 ; <name> ST`). kitty 0.31+ and Ghostty 1.0+ take the
    /// CSS names; iTerm2 takes X11 names only and resets to the arrow on
    /// any it does not know, so it gets its own vocabulary; the rest ignore
    /// the sequence harmlessly. Sent on change or a terminal-owned reset.
    pub(super) fn follow_pointer_shape(&mut self, x: u16, y: u16) {
        let out = rustel_runtime::stdio::out();
        let _ = self.write_pointer_shape(x, y, &mut out.lock());
    }

    pub(super) fn note_pointer_shape_event(&mut self, event: &Event) {
        if !matches!(event, Event::FocusGained | Event::FocusLost)
            && !matches!(event, Event::Key(key) if matches!(key.code, KeyCode::Modifier(_)))
        {
            return;
        }
        if !super::super::terminal::profiles::pointer_modifier_reset(&self.features.name) {
            return;
        }
        if matches!(event, Event::FocusLost) {
            self.pointer_shift_held = [false; 2];
            self.pointer_shape_reset = false;
            return;
        }
        if let Event::Key(key) = event {
            use crossterm::event::ModifierKeyCode;
            let shift = match key.code {
                KeyCode::Modifier(ModifierKeyCode::LeftShift) => Some(0),
                KeyCode::Modifier(ModifierKeyCode::RightShift) => Some(1),
                _ => None,
            };
            if let Some(index) = shift {
                self.pointer_shift_held[index] = key.kind != KeyEventKind::Release;
            }
            // Crossterm includes the released modifier itself in its mask, so
            // a Shift release must use physical-key state instead of that bit.
            if self.pointer_shift_held.contains(&true)
                || (shift.is_none() && key.modifiers.contains(KeyModifiers::SHIFT))
            {
                self.pointer_shape_reset = false;
                return;
            }
            if key.kind != KeyEventKind::Release {
                return;
            }
        }
        // Restore the application's shape even under a still mouse.
        self.pointer_shape_reset = true;
        self.dirty_frame |= self.pointer_at.is_some();
    }

    pub(super) fn write_pointer_shape(
        &mut self,
        x: u16,
        y: u16,
        out: &mut impl std::io::Write,
    ) -> std::io::Result<()> {
        self.pointer_at = Some((x, y));
        let shape = self
            .held_pointer_shape()
            .unwrap_or_else(|| self.pointer_shape_at(x, y));
        if self.pointer_shape_reset || self.pointer_shape_sent != Some(shape) {
            if self.pointer_shape_reset {
                // Affected Ghostty versions ignore an OSC 22 shape equal to
                // their cached application shape, even after a modifier changed
                // the visible cursor. A distinct shape makes the restore land.
                let reset = if shape == SHAPE_DEFAULT {
                    SHAPE_TEXT
                } else {
                    SHAPE_DEFAULT
                };
                let name = pointer_shape_name(&self.features.name, reset);
                write!(out, "\x1b]22;{name}\x1b\\")?;
            }
            let name = pointer_shape_name(&self.features.name, shape);
            write!(out, "\x1b]22;{name}\x1b\\")?;
            out.flush()?;
            self.pointer_shape_sent = Some(shape);
            self.pointer_shape_reset = false;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    mod pointer_shape {
        //! Tests for `app/pointer_shape.rs`.

        use crate::app::pointer_shape::*;
        use crate::app::tests::support::*;
        use crossterm::event::ModifierKeyCode;

        // Affected Ghostty releases keep the OSC 22 request separately from the
        // visible modifier override. Identical requests are ignored by setMouseShape.
        struct GhosttyPointer {
            requested: String,
            visible: String,
        }

        impl GhosttyPointer {
            fn text() -> Self {
                Self {
                    requested: SHAPE_TEXT.into(),
                    visible: SHAPE_TEXT.into(),
                }
            }

            fn receive(&mut self, bytes: &[u8]) {
                let output = std::str::from_utf8(bytes).unwrap();
                for command in output.split("\x1b\\").filter(|command| !command.is_empty()) {
                    let shape = command.strip_prefix("\x1b]22;").expect("OSC 22");
                    if self.requested != shape {
                        self.requested = shape.into();
                        self.visible = shape.into();
                    }
                }
            }
        }

        fn app_at(directory: &std::path::Path, terminal: &str) -> (App, (u16, u16)) {
            let mut app = app_over(directory);
            app.features.name = terminal.into();
            app.scenes.current_mut().editor = Editor::new("s(\"sine\")").unwrap();
            app.frame = Rect::new(0, 0, 100, 30);
            app.regions = view::regions(app.frame, false, 1, false, false, None, None);
            let editor = app.regions.panes[0].editor;
            let gutter = view::gutter_width(app.editor(), true);
            let point = (editor.x + gutter + 2, editor.y);
            let mut output = Vec::new();
            app.write_pointer_shape(point.0, point.1, &mut output)
                .unwrap();
            let expected = format!("\x1b]22;{}\x1b\\", pointer_shape_name(terminal, SHAPE_TEXT));
            assert_eq!(output, expected.as_bytes());
            app.dirty_frame = false;
            (app, point)
        }

        fn modifier(app: &mut App, code: ModifierKeyCode, kind: KeyEventKind) {
            let modifiers = match code {
                ModifierKeyCode::LeftShift | ModifierKeyCode::RightShift => KeyModifiers::SHIFT,
                ModifierKeyCode::LeftControl | ModifierKeyCode::RightControl => {
                    KeyModifiers::CONTROL
                }
                ModifierKeyCode::LeftAlt | ModifierKeyCode::RightAlt => KeyModifiers::ALT,
                ModifierKeyCode::LeftSuper | ModifierKeyCode::RightSuper => KeyModifiers::SUPER,
                _ => KeyModifiers::NONE,
            };
            app.handle_terminal_event(Event::Key(KeyEvent::new_with_kind(
                KeyCode::Modifier(code),
                modifiers,
                kind,
            )))
            .unwrap();
        }

        #[test]
        fn ghostty_shift_release_restores_the_ibeam_without_mouse_motion() {
            let directory = tempfile::tempdir().unwrap();
            let (mut app, point) = app_at(directory.path(), "Ghostty 1.2.3");
            let before = app.editor().primary_selection();
            let mut terminal = GhosttyPointer::text();
            for _ in 0..3 {
                modifier(&mut app, ModifierKeyCode::LeftShift, KeyEventKind::Press);
                assert!(
                    !app.pointer_shape_reset,
                    "retain native selection while Shift is held"
                );
                terminal.visible = SHAPE_DEFAULT.into(); // Ghostty's release override.
                terminal.receive(b"\x1b]22;text\x1b\\");
                assert_eq!(
                    terminal.visible, SHAPE_DEFAULT,
                    "same-shape requests cannot repair it"
                );
                modifier(&mut app, ModifierKeyCode::LeftShift, KeyEventKind::Release);
                assert!(
                    app.dirty_frame,
                    "a still pointer needs a frame after modifier release"
                );
                let mut output = Vec::new();
                app.write_pointer_shape(point.0, point.1, &mut output)
                    .unwrap();
                assert_eq!(output, b"\x1b]22;default\x1b\\\x1b]22;text\x1b\\");
                terminal.receive(&output);
                assert_eq!(terminal.visible, SHAPE_TEXT);
                assert_eq!(app.editor().primary_selection(), before);
                assert_eq!(app.editor().source(), "s(\"sine\")");
                assert_eq!(app.pointer_at, Some(point));
                assert!(!app.pointer_shape_reset);
                output.clear();
                app.write_pointer_shape(point.0, point.1, &mut output)
                    .unwrap();
                assert!(
                    output.is_empty(),
                    "ordinary frames still deduplicate OSC output"
                );
                app.dirty_frame = false;
            }
        }

        #[test]
        fn ghostty_pointer_restore_survives_modifier_release_batched_with_typing() {
            let directory = tempfile::tempdir().unwrap();
            let (mut app, point) = app_at(directory.path(), "Ghostty");
            assert!(app.accepts_raw_paste());
            let release = Event::Key(KeyEvent::new_with_kind(
                KeyCode::Modifier(ModifierKeyCode::LeftShift),
                KeyModifiers::SHIFT,
                KeyEventKind::Release,
            ));
            let queued = std::cell::RefCell::new(std::collections::VecDeque::from([
                Event::Key(KeyEvent::new_with_kind(
                    KeyCode::Char('A'),
                    KeyModifiers::NONE,
                    KeyEventKind::Release,
                )),
                release.clone(),
            ]));
            let events = read_raw_paste_batch(
                Event::Key(KeyEvent::new(KeyCode::Char('A'), KeyModifiers::NONE)),
                |_| Ok(!queued.borrow().is_empty()),
                || Ok(queued.borrow_mut().pop_front().unwrap()),
            )
            .unwrap();
            assert_eq!(events.len(), 2, "ordinary key-up stays inert");
            assert_eq!(events.last(), Some(&release));
            for event in events {
                app.handle_terminal_event(event).unwrap();
            }
            assert!(app.editor().source().contains('A'));
            assert!(app.pointer_shape_reset);
            let mut output = Vec::new();
            app.write_pointer_shape(point.0, point.1, &mut output)
                .unwrap();
            let mut terminal = GhosttyPointer::text();
            terminal.visible = SHAPE_DEFAULT.into();
            terminal.receive(&output);
            assert_eq!(terminal.visible, SHAPE_TEXT);
        }

        #[test]
        fn ghostty_modifier_release_preserves_captured_drag_shapes() {
            let directory = tempfile::tempdir().unwrap();
            let (mut app, point) = app_at(directory.path(), "tmux via Ghostty 1.2.3");
            app.pointer = Some(Pointer::Volume);
            for code in [
                ModifierKeyCode::RightShift,
                ModifierKeyCode::LeftControl,
                ModifierKeyCode::RightAlt,
                ModifierKeyCode::LeftSuper,
            ] {
                modifier(&mut app, code, KeyEventKind::Release);
                let mut output = Vec::new();
                app.write_pointer_shape(point.0, point.1, &mut output)
                    .unwrap();
                assert_eq!(output, b"\x1b]22;default\x1b\\\x1b]22;grabbing\x1b\\");
                assert!(matches!(app.pointer, Some(Pointer::Volume)));
            }
        }

        #[test]
        fn ghostty_pointer_waits_for_native_shift_selection_to_end() {
            let directory = tempfile::tempdir().unwrap();
            let (mut app, point) = app_at(directory.path(), "Ghostty");
            app.pointer = Some(Pointer::Volume);
            app.write_pointer_shape(point.0, point.1, &mut Vec::new())
                .unwrap();
            modifier(&mut app, ModifierKeyCode::LeftShift, KeyEventKind::Press);
            modifier(&mut app, ModifierKeyCode::RightShift, KeyEventKind::Press);
            modifier(&mut app, ModifierKeyCode::LeftAlt, KeyEventKind::Release);
            modifier(&mut app, ModifierKeyCode::LeftShift, KeyEventKind::Release);
            let mut output = Vec::new();
            app.write_pointer_shape(point.0, point.1, &mut output)
                .unwrap();
            assert!(output.is_empty(), "Right Shift still owns selection");
            modifier(&mut app, ModifierKeyCode::RightShift, KeyEventKind::Release);
            assert!(app.pointer_shape_reset);
            // A new press can arrive in the same input batch before the next frame.
            modifier(&mut app, ModifierKeyCode::LeftShift, KeyEventKind::Press);
            app.write_pointer_shape(point.0, point.1, &mut output)
                .unwrap();
            assert!(output.is_empty(), "new Shift press defers queued recovery");
            modifier(&mut app, ModifierKeyCode::LeftShift, KeyEventKind::Release);
            app.write_pointer_shape(point.0, point.1, &mut output)
                .unwrap();
            assert_eq!(output, b"\x1b]22;default\x1b\\\x1b]22;grabbing\x1b\\");
            // Alt-tab can lose a Shift release; focus return must not leave it held.
            modifier(&mut app, ModifierKeyCode::LeftShift, KeyEventKind::Press);
            app.handle_terminal_event(Event::FocusLost).unwrap();
            app.handle_terminal_event(Event::FocusGained).unwrap();
            output.clear();
            app.write_pointer_shape(point.0, point.1, &mut output)
                .unwrap();
            assert!(!output.is_empty());
        }

        #[test]
        fn ghostty_focus_return_repairs_default_and_text_shapes() {
            let directory = tempfile::tempdir().unwrap();
            let (mut app, text) = app_at(directory.path(), "Ghostty");
            let gutter = (app.regions.panes[0].editor.x, app.regions.panes[0].editor.y);
            for (point, expected) in [
                (gutter, "\x1b]22;text\x1b\\\x1b]22;default\x1b\\"),
                (text, "\x1b]22;default\x1b\\\x1b]22;text\x1b\\"),
            ] {
                app.handle_terminal_event(Event::FocusGained).unwrap();
                let mut output = Vec::new();
                app.write_pointer_shape(point.0, point.1, &mut output)
                    .unwrap();
                assert_eq!(output, expected.as_bytes());
            }
        }

        #[test]
        fn pointer_refresh_is_specific_to_detected_ghostty_and_modifier_releases() {
            let directory = tempfile::tempdir().unwrap();
            for terminal in ["kitty", "iTerm2", "Windows Terminal", "unknown", "Ghostty"] {
                let (mut app, point) = app_at(directory.path(), terminal);
                modifier(&mut app, ModifierKeyCode::LeftShift, KeyEventKind::Press);
                modifier(&mut app, ModifierKeyCode::LeftShift, KeyEventKind::Repeat);
                let mut output = Vec::new();
                app.write_pointer_shape(point.0, point.1, &mut output)
                    .unwrap();
                assert!(
                    output.is_empty(),
                    "{terminal}: no reset while a modifier is held"
                );
                modifier(&mut app, ModifierKeyCode::LeftShift, KeyEventKind::Release);
                app.write_pointer_shape(point.0, point.1, &mut output)
                    .unwrap();
                assert_eq!(!output.is_empty(), terminal == "Ghostty", "{terminal}");
            }
        }

        #[test]
        fn ghostty_refresh_waits_for_a_known_pointer_and_survives_failed_writes() {
            let directory = tempfile::tempdir().unwrap();
            let (mut app, point) = app_at(directory.path(), "Ghostty");
            app.pointer_at = None;
            app.dirty_frame = false;
            modifier(&mut app, ModifierKeyCode::LeftShift, KeyEventKind::Release);
            assert!(app.pointer_shape_reset);
            assert!(!app.dirty_frame, "do not invent a mouse position");

            struct FailedOutput;
            impl std::io::Write for FailedOutput {
                fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
                    Err(std::io::Error::other("test output failed"))
                }
                fn flush(&mut self) -> std::io::Result<()> {
                    Ok(())
                }
            }
            assert!(
                app.write_pointer_shape(point.0, point.1, &mut FailedOutput)
                    .is_err()
            );
            assert!(
                app.pointer_shape_reset,
                "failed output still needs a restore"
            );
            let mut output = Vec::new();
            app.write_pointer_shape(point.0, point.1, &mut output)
                .unwrap();
            assert_eq!(output, b"\x1b]22;default\x1b\\\x1b]22;text\x1b\\");
            assert!(!app.pointer_shape_reset);
        }

        /// The mixer's score fader uses the same pointer shapes as the inline
        /// pill: a hand over the rail, the closed hand for as long as a drag lasts.
        #[test]
        fn mixer_score_fader_pointer_matches_the_inline_slider() {
            use crate::mixer_panel::{FaderDesk, MixerEdge, fader_desk, parts};

            let directory = tempfile::tempdir().expect("temp dir");
            let (mut app, scene, call) = slider_app(directory.path());
            app.audible_scene = Some(scene);
            let (pill_x, _, pill_y) = lay_out(&mut app, scene, call);
            assert_eq!(
                app.pointer_shape_at(pill_x, pill_y),
                SHAPE_POINTER,
                "the inline pill is a hand"
            );
            let editor = app.regions.panes[0].editor;
            let gutter = view::gutter_width(&app.scenes.current().editor, true);
            assert_eq!(
                app.pointer_shape_at(editor.x + gutter + 2, pill_y),
                SHAPE_TEXT,
                "the score around the pill keeps the I-beam"
            );
            mouse(
                &mut app,
                MouseEventKind::Down(MouseButton::Left),
                pill_x,
                pill_y,
            );
            assert!(
                matches!(app.pointer, Some(Pointer::Slider { .. })),
                "the press keeps the pill: {:?}",
                app.pointer
            );
            assert_eq!(app.held_pointer_shape(), Some(SHAPE_GRABBING));
            mouse(
                &mut app,
                MouseEventKind::Up(MouseButton::Left),
                pill_x,
                pill_y,
            );
            assert!(app.pointer.is_none(), "the release lets the pill go");

            precision_key(&mut app, KeyCode::F(4), KeyModifiers::NONE);
            app.regions.mixer = Rect::new(0, 18, 120, 12);
            let desk = parts(app.regions.mixer, MixerEdge::Bottom)
                .expect("a band that tall is a desk")
                .desk;
            let widths = app.mixer_strip_widths();
            let layout = fader_desk(desk, &widths, 1).expect("room for the score's one fader");
            let cell = layout.cell(0).expect("the fader is drawn");
            let rail = FaderDesk::rail(cell).expect("and so is its rail");
            assert_eq!(
                app.pointer_shape_at(rail.x, rail.y),
                SHAPE_POINTER,
                "the mixer's copy of the same slider is a hand too"
            );
            mouse(
                &mut app,
                MouseEventKind::Down(MouseButton::Left),
                rail.x,
                rail.y,
            );
            assert!(
                matches!(app.pointer, Some(Pointer::MixerFader { .. })),
                "the press keeps the mixer's fader: {:?}",
                app.pointer
            );
            assert_eq!(
                app.held_pointer_shape(),
                Some(SHAPE_GRABBING),
                "and the drag keeps the closed hand, as the pill does"
            );
            assert_eq!(
                app.pointer_shape_at(editor.x + gutter + 2, pill_y),
                SHAPE_TEXT,
                "opening the mixer does not take the I-beam off the score"
            );
        }

        /// iTerm2 speaks OSC 22 in X11 names and resets on anything else; the
        /// CSS names kitty and Ghostty take would have done nothing there.
        #[test]
        fn iterm_gets_the_x11_pointer_names() {
            assert_eq!(pointer_shape_name("iTerm.app 3.5.11", "pointer"), "hand2");
            assert_eq!(pointer_shape_name("iTerm.app 3.5.11", "text"), "xterm");
            assert_eq!(pointer_shape_name("iTerm.app 3.5.11", "default"), "arrow");
            assert_eq!(pointer_shape_name("iTerm.app 3.5.11", "grabbing"), "fleur");
            assert_eq!(
                pointer_shape_name("iTerm.app 3.5.11", "ew-resize"),
                "sb_h_double_arrow"
            );
            assert_eq!(pointer_shape_name("kitty", "grabbing"), "grabbing");
            assert_eq!(pointer_shape_name("kitty", "pointer"), "pointer");
            assert_eq!(pointer_shape_name("ghostty 1.1.0", "text"), "text");
            assert_eq!(pointer_shape_name("Windows Terminal", "default"), "default");
        }

        /// The shape follows what is on top and what a press there would do:
        /// the I-beam over the score but the arrow over its line numbers, the
        /// arrow over the keyboard help and a sheet's frame, the hand over a
        /// sheet's rows and the footer's error line, the closed hand for as
        /// long as a drag lasts, and a fresh answer when the screen changes
        /// under a still pointer.
        #[test]
        fn the_pointer_shape_says_what_a_press_would_do() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = app_over(directory.path());
            let source = (0..80)
                .map(|n| format!("line {n}"))
                .collect::<Vec<_>>()
                .join("\n");
            app.scenes.current_mut().editor = Editor::new(&source).expect("editor");
            app.frame = Rect::new(0, 0, 100, 30);
            // No minimap: the pane keeps a scrollbar instead.
            app.regions = view::regions(app.frame, false, 1, false, false, None, None);
            let editor = app.regions.panes[0].editor;
            let gutter = view::gutter_width(&app.scenes.current().editor, true);
            assert!(gutter > 1, "numbered, the gutter is more than the margin");
            assert_eq!(
                app.pointer_shape_at(editor.x, editor.y),
                SHAPE_DEFAULT,
                "the line numbers take no caret"
            );
            let text = (editor.x + gutter + 2, editor.y);
            assert_eq!(app.pointer_shape_at(text.0, text.1), SHAPE_TEXT);
            assert_eq!(
                app.pointer_shape_at(app.regions.panes[0].scrollbar.x, editor.y + 2),
                SHAPE_POINTER
            );

            // The footer's error line opens the log, so it is a hand.
            app.set_error(ErrorOwner::Interface, "a test error".into());
            let footer = app.regions.footer;
            assert_eq!(app.pointer_shape_at(footer.x + 2, footer.y), SHAPE_POINTER);
            app.clear_error(ErrorOwner::Interface);
            assert_eq!(app.pointer_shape_at(footer.x + 2, footer.y), SHAPE_DEFAULT);

            // A sheet opened from the keyboard changes the answer under a
            // still pointer: its rows are hands, its frame the arrow.
            press(&mut app, KeyCode::Char('o'), KeyModifiers::CONTROL);
            let (sheet, rows) = crate::app::settings::SettingsSheetView::geometry(app.frame)
                .expect("the sheet fits");
            let control_y = (rows.y..rows.bottom())
                .find(|y| {
                    app.settings_sheet
                        .unwrap()
                        .row_at_for(app.frame, rows.x + 1, *y)
                        .is_some()
                })
                .expect("visible control");
            assert_eq!(app.pointer_shape_at(rows.x + 1, control_y), SHAPE_POINTER);
            assert_eq!(app.pointer_shape_at(sheet.x, sheet.y), SHAPE_DEFAULT);
            press(&mut app, KeyCode::Esc, KeyModifiers::NONE);
            // Over the score again - not the sheet's own row, which the strip's
            // margin now reaches.
            assert_eq!(app.pointer_shape_at(text.0, text.1), SHAPE_TEXT);

            // The keyboard help covers everything.
            press(&mut app, KeyCode::F(1), KeyModifiers::NONE);
            press(&mut app, KeyCode::Char('h'), KeyModifiers::NONE);
            press(&mut app, KeyCode::Char('k'), KeyModifiers::NONE);
            assert!(app.help.is_some(), "Help > Keyboard reference");
            assert_eq!(app.pointer_shape_at(text.0, text.1), SHAPE_DEFAULT);
            press(&mut app, KeyCode::Esc, KeyModifiers::NONE);

            // A drag keeps the closed hand wherever the pointer goes.
            let bar = app.regions.panes[0].scrollbar;
            app.handle_mouse(MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: bar.x,
                row: bar.y + 1,
                modifiers: KeyModifiers::NONE,
            })
            .expect("press");
            assert_eq!(app.pointer, Some(Pointer::Scrollbar));
            app.follow_pointer_shape(text.0, text.1);
            assert_eq!(app.pointer_shape_sent, Some(SHAPE_GRABBING));
            app.handle_mouse(MouseEvent {
                kind: MouseEventKind::Up(MouseButton::Left),
                column: text.0,
                row: text.1,
                modifiers: KeyModifiers::NONE,
            })
            .expect("release");
            assert_eq!(
                app.pointer_shape_sent,
                Some(SHAPE_TEXT),
                "let go over the text"
            );
        }

        /// The OS pointer says what a click would do: a hand over a settings
        /// row, an I-beam over the score, the arrow over plain chrome.
        #[test]
        fn the_pointer_shape_follows_what_is_under_it() {
            let directory = tempfile::tempdir().expect("tempdir");
            let mut app = app_over(directory.path());
            app.frame = Rect::new(0, 0, 120, 40);
            app.regions = view::regions(app.frame, false, 1, false, true, None, None);
            let editor = app.regions.panes[0].editor;
            let gutter = view::gutter_width(&app.scenes.current().editor, true);
            assert_eq!(
                app.pointer_shape_at(editor.x + gutter + 2, editor.y + 1),
                "text"
            );
            let minimap = app.regions.panes[0].minimap;
            assert_eq!(app.pointer_shape_at(minimap.x, minimap.y + 1), "pointer");
            let header = app.regions.header;
            assert_eq!(
                app.pointer_shape_at(header.x + header.width / 2, header.y),
                "default"
            );
            app.settings_sheet = Some(SettingsSheet::default());
            let settings_frame = app.settings_sheet_frame();
            let (_area, rows) =
                crate::settings::SettingsSheetView::geometry(settings_frame).expect("room");
            let control_y = (rows.y..rows.bottom())
                .find(|y| {
                    app.settings_sheet
                        .unwrap()
                        .row_at_for(settings_frame, rows.x + 3, *y)
                        .is_some()
                })
                .expect("visible control");
            assert_eq!(app.pointer_shape_at(rows.x + 3, control_y), "pointer");
            let blank_y = (rows.y..rows.bottom())
                .find(|y| {
                    app.settings_sheet
                        .unwrap()
                        .row_at_for(settings_frame, rows.x + 3, *y)
                        .is_none()
                })
                .expect("a blank sheet row");
            assert_eq!(
                app.pointer_shape_at(rows.x + 3, blank_y),
                "default",
                "the sheet's blank rows are not buttons"
            );
        }
    }
}
