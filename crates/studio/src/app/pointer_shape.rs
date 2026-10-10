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
            if SettingsSheet::tab_at(settings_frame, x, y).is_some()
                || sheet.shows_settings() && sheet.row_at_for(settings_frame, x, y).is_some()
                || sheet.source_row_at(settings_frame, x, y).is_some()
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
        let _ = self.write_pointer_shape(x, y, &mut std::io::stdout().lock());
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
