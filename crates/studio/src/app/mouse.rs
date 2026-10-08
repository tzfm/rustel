//! Mouse routing for the studio. `handle_mouse` takes every press, drag,
//! release and wheel event and passes it to whatever is under the pointer: menu
//! bar, pickers, sheets, reference, mixer, log, footer chips, panes or the
//! score; `Pointer` keeps what a press began on, so the drag and release that
//! follow go there too (`drag_pointer`, `release_pointer`). A left press and
//! the wheel are offered to each surface from the top-most down (`press_left`,
//! `press_score`, `scroll_wheel`), and what a surface does with them lives in
//! that surface's own file. This file also holds the footer and header hit
//! tests (footer chips, the go chip, the error line, the clickable last-file
//! status), the conversion of pixel mouse reports to cells, the rule that only
//! one surface (score, log, theme code, reference or mixer) holds a text
//! selection at a time, and the widening of a selection to the word or line a
//! repeated click chose.

use super::*;

/// A surface that can hold a text selection.
///
/// There are five, and until now each kept its own without knowing about
/// the others, so a band could be left lying in the log, another in the
/// reference and a third in the score, all drawn at once.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum TextSurface {
    Score,
    Log,
    ThemeCode,
    Reference,
    Mixer,
}

/// What a pointer press is currently driving. A drag belongs to whatever it
/// began on, so sliding off a fader onto the source does not start selecting
/// text half way through a fade.
#[derive(Clone, Debug, PartialEq)]
pub(super) enum Pointer {
    Editor,
    Volume,
    /// Dragging a fader on the mixer panel's desk: the strip chosen at
    /// the press keeps the pointer, so a drag that wanders keeps driving
    /// it, the way the master's does.
    MixerPanelFader,
    /// Dragging the samples tab's preview-volume row.
    PreviewVolume,
    #[cfg(feature = "hydra")]
    GeneratorControl {
        index: usize,
        rail: Rect,
    },
    #[cfg(feature = "hydra")]
    SnippetCodeScroll {
        row: u16,
        scroll: usize,
    },
    /// Dragging a selection through the theme editor's code tab.
    ThemeCode,
    /// Dragging a selection through the log.
    Log,
    /// Dragging the minimap; the offset keeps the grabbed row under the
    /// pointer, like a scrollbar thumb.
    Minimap {
        grab_rows: usize,
    },
    /// Dragging the scrollbar that stands in for the minimap.
    Scrollbar,
    /// Dragging the unwrapped editor's scrollbar along its bottom row.
    HorizontalScrollbar {
        pane: usize,
        /// Hold the grabbed cell and exact initial scroll offset.
        grab: HorizontalScrollbarGrab,
    },
    /// A press a panel took. Held so the drag and release that follow stay
    /// with the panel instead of leaking into a text selection underneath.
    Panel,
    /// Dragging a selection through a prompt's text field.
    PromptField,
    /// Dragging a character selection through the reference column's
    /// read-only text. Which block is recorded on the panel's selection.
    ReferenceText {
        granularity: super::super::textblock::Granularity,
    },
    /// Dragging a character selection through the mixer's devices block:
    /// the MIDI log and what the pads are doing. The rows it was made on
    /// are recorded on `mixer_selection`.
    MixerText {
        granularity: super::super::textblock::Granularity,
    },
    /// Dragging a score fader on the mixer's desk. Named the same way an
    /// inline slider is, and driven down the same write path, so the two
    /// pictures of one control never disagree.
    MixerFader {
        scene: SceneId,
        call_from: usize,
        rail: SliderTrack,
    },
    /// Dragging an inline slider: the knob follows the pointer along the
    /// pill, one to one. The control is named by its scene and where its
    /// call begins - the one coordinate a drag's own edits never move.
    Slider {
        scene: SceneId,
        call_from: usize,
        /// Keep the painted track through edits, which invalidate screen
        /// maps before the rest of a batch of mouse reports is handled.
        track: SliderTrack,
    },
    /// Dragging a replay timeline's scrollbar along its captured rail.
    TimelineBar {
        scene: SceneId,
        strip: Rect,
        /// Cells from the thumb's start to where it was grabbed.
        grab: u16,
    },
    /// Dragging where a block of a replay's timeline starts: the block
    /// before it gets longer or shorter, a second per cell.
    BlockStart {
        scene: SceneId,
        block: usize,
        origin_x: u16,
        origin_length: f64,
    },
}

impl App {
    /// Whether the status line is showing the last file's path, which a
    /// press on it opens.
    pub(super) fn status_names_last_file(&self) -> bool {
        !self.piano.open
            && self.last_file.is_some()
            && self.last_file_status.as_ref() == Some(&self.status)
    }

    /// The footer's error line, which a press opens the log from.
    pub(super) fn error_line_at(&self, x: u16, y: u16) -> bool {
        self.errors.visible().is_some()
            && !self.regions.footer.is_empty()
            && y == self.regions.footer.y
            && x < self.footer_hits().status.right()
    }

    pub(super) fn handle_mouse(&mut self, mouse: MouseEvent) -> Result<(), RuntimeError> {
        let (x, y) = (mouse.column, mouse.row);
        // A press begins a new gesture wherever it lands. Whatever the last
        // one held is over, even when its release never came (let go
        // outside the window, or taken by a sheet or menu on its way): its
        // pointer is dropped, and so is any drag an editor still keeps (a
        // score's, the log's, the theme code's), so nothing this gesture
        // does can extend it. The wheel extends a live drag on purpose.
        if matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left)) {
            self.pointer = None;
            for scene in self.scenes.scenes_mut() {
                scene.editor.cancel_drag();
            }
            if let Some(log) = self.log_panel.as_mut() {
                log.editor.cancel_drag();
            }
            if let Some(theme) = self.theme_editor.as_mut() {
                theme.code.cancel_drag();
            }
        }
        if self.handle_horizontal_scrollbar_pointer(mouse, x, y) {
            return Ok(());
        }
        if self.handle_replay_pointer(mouse) {
            return Ok(());
        }
        // The OS pointer follows what is under it: an I-beam over editable
        // text, the arrow elsewhere - where the terminal honours the
        // pointer-shape escape (kitty, wezterm, foot do; Windows Terminal
        // does not yet, and ignores it harmlessly).
        // Followed on every event, not only a move: a press turns the hand
        // into the closed hand of a drag, and the release turns it back.
        if !matches!(mouse.kind, MouseEventKind::Down(_) | MouseEventKind::Up(_)) {
            self.follow_pointer_shape(x, y);
        }
        // The bar and its dropdown paint above every panel, so they claim
        // clicks above every panel too - ahead of the device picker below.
        //
        // A press that lands outside an open menu dismisses it and is
        // consumed. Letting it fall through would also run the "every target
        // from here down belongs to the score" branch, which steals focus
        // and drops a live pane selection on the way past.
        self.settle_menu();
        // The readiness chip counts what the log has to say about the
        // score, so pressing it opens the log at what it is counting.
        if matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left))
            && self.menu.is_none()
            && within(self.go_chip(), x, y)
        {
            if self.log_panel.is_none() {
                self.toggle_log_panel();
            } else {
                self.focus_panel(PanelKind::Log);
            }
            return Ok(());
        }
        if matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left))
            && self.menu.is_none()
            && super::super::memory::memory_chip_at(x, y)
        {
            self.open_log_from_memory();
            return Ok(());
        }
        if self.menu_mouse(mouse, x, y)? {
            return Ok(());
        }
        if self.precision_slider_mouse(mouse) {
            return Ok(());
        }
        // An open picker owns the pointer: a press acts on it or dismisses
        // it, and the drag that follows never leaks into a text selection.
        if self.panel.is_some() {
            if let MouseEventKind::Down(MouseButton::Left) = mouse.kind {
                self.click_panel(x, y);
                self.settle_focus();
            }
            return Ok(());
        }
        match mouse.kind {
            MouseEventKind::Down(MouseButton::Right)
                if self.reference_panel.is_some() && within(self.regions.reference, x, y) =>
            {
                self.right_click_reference(y);
            }
            MouseEventKind::Down(MouseButton::Left) => {
                if self.press_left(mouse, x, y)? {
                    return Ok(());
                }
            }
            MouseEventKind::Drag(MouseButton::Left) => {
                if self.drag_pointer(mouse, x, y) {
                    return Ok(());
                }
            }
            MouseEventKind::Up(MouseButton::Left) => {
                if self.release_pointer(mouse, x, y) {
                    return Ok(());
                }
            }
            MouseEventKind::ScrollUp
            | MouseEventKind::ScrollDown
            | MouseEventKind::ScrollLeft
            | MouseEventKind::ScrollRight => {
                // A two-finger swipe drives a slider or the fader: up/right
                // raises it, down/left lowers it - the horizontal swipe
                // moving the horizontal control the way the hand expects.
                let direction: f32 = match mouse.kind {
                    MouseEventKind::ScrollUp | MouseEventKind::ScrollRight => 1.0,
                    _ => -1.0,
                };
                // Which way the fingers went, kept apart from what the
                // swipe means to a control. A vertical list and a strip
                // laid out sideways do not agree about `direction`: up is
                // earlier and right is LATER, and folding both into one
                // sign sent a right-to-left swipe backwards along the tape.
                let sideways = matches!(
                    mouse.kind,
                    MouseEventKind::ScrollLeft | MouseEventKind::ScrollRight
                );
                if self.scroll_wheel(mouse, x, y, direction, sideways) {
                    return Ok(());
                }
            }
            _ => {}
        }
        self.forward_mouse_to_editor(mouse)
    }

    /// A left press, offered to what is under it from the top-most surface
    /// down. Returns true when nothing more is owed the press and false
    /// when it is left to the score's editor, as `press_score` says.
    fn press_left(&mut self, mouse: MouseEvent, x: u16, y: u16) -> Result<bool, RuntimeError> {
        // What paints on top takes the press first: the picker and
        // the settings rows outrank the reference column and the
        // blanket sheet claims, or their clicks are eaten by
        // whatever they happen to overlap.
        let extend = super::super::editor::click_extends(mouse.modifiers);
        // Nothing the pointer does plays a scene.
        //
        // A modified click cannot be relied on for it: macOS turns
        // Ctrl+click into a right click before any terminal sees it,
        // iTerm2 keeps that right click for its own menu, and Windows
        // Terminal keeps Shift for selection. Such a gesture would
        // work on some machines, do nothing on others, and look like
        // the click that merely chooses a scene. A pad plays a scene,
        // and so does an update; a click chooses, and only chooses.
        if self.click_set_prompt(x, y, extend)
            || self.click_viz_add_sheet(x, y)
            || self.click_viz_prompt(x, y, extend)
        {
            return Ok(true);
        }
        // Reference can overlap Settings and the other sheets. If
        // it was raised later, it owns every shared cell before an
        // underlying row gets a chance to react or take focus.
        if self.reference_panel.is_some()
            && within(self.regions.reference, x, y)
            && self.reference_on_top()
        {
            self.click_reference(mouse, x, y)?;
            return Ok(true);
        }
        // Then the sheets, each ahead of the ones it paints over. Settings
        // is taken in two places because its tabs and rows used to come
        // after the blanket sheet claims, its own included, which ate their
        // clicks: they were moved up, and its blanket claim was left where
        // it was. The only claim between the two is the theme editor's,
        // and that sheet is hidden while Settings is open.
        if self.click_jobs_panel(x, y)
            || self.click_theme_picker(x, y)
            || self.click_settings_sheet(x, y)
            || self.click_theme_editor(mouse, x, y)
            || self.claim_settings_sheet(x, y)
            || self.click_log_panel(mouse, x, y)
            || self.click_export_sheet(x, y)
        {
            return Ok(true);
        }
        // The reference under the sheets, over the docks.
        if self.reference_panel.is_some() && within(self.regions.reference, x, y) {
            self.click_reference(mouse, x, y)?;
            return Ok(true);
        }
        // Below every sheet: the set panel, docked or as a sheet,
        // the mixer's desk and the visuals panel.
        if self.click_set_panel(x, y)
            || self.click_mixer_panel(x, y)
            || self.click_memory_dock(x, y)
            || self.click_viz_panel(x, y)
        {
            return Ok(true);
        }
        self.press_score(mouse, x, y)
    }

    /// A press below every sheet and panel, on the score's side of the
    /// screen: a pane's title, scrollbars and minimap, the scene strip, the
    /// footer and header chips, a slider's pill and a replay's timeline.
    ///
    /// Returns true when nothing more is owed the press: one of those took
    /// it, or it landed on a pane without the caret and
    /// `click_unfocused_pane` has already passed it to that pane's editor.
    /// Returns false when none of them took it: the score's editor now
    /// holds the selection and the pointer, and the press is left for the
    /// caller to pass on to it.
    fn press_score(&mut self, mouse: MouseEvent, x: u16, y: u16) -> Result<bool, RuntimeError> {
        // From here down every target belongs to the score's side
        // of the screen: the keyboard follows the click, and a pane
        // selection that stayed painted would lie about what Ctrl+C
        // copies.
        self.focus = Focus::Editor;
        #[cfg(feature = "hydra")]
        self.sync_settings_webcam_preview();
        self.drop_pane_selection();
        if self.click_pane_title(x, y) || self.press_horizontal_scrollbar(x, y) {
            return Ok(true);
        }
        if self.click_unfocused_pane(mouse, x, y)? {
            return Ok(true);
        }
        if within(self.regions.scenes, x, y) {
            if let Some(index) = self.scene_chip_at(x, y) {
                self.select_scene(index);
            }
            return Ok(true);
        }
        if let Some((orbit, pair)) = self.orbit_chip_at(x, y) {
            self.route_orbit_next(orbit, pair);
            return Ok(true);
        }
        if self.status_names_last_file() && within(self.footer_hits().status, x, y) {
            self.reveal_last_file();
            return Ok(true);
        }
        if let Some((chip, anchor)) = self.footer_chip_at(x, y) {
            self.open_device_panel_on(chip, Some(anchor));
            return Ok(true);
        }
        if self.error_line_at(x, y) {
            if self.log_panel.is_none() {
                self.toggle_log_panel();
            }
            self.focus_panel(PanelKind::Log);
            self.pointer = Some(Pointer::Panel);
            return Ok(true);
        }
        // The header's warning badge - the icon and count, nothing
        // around them - opens the log, which marks it seen and puts
        // the badge away.
        if view::warning_badge_at(x, y) {
            if self.log_panel.is_none() {
                self.toggle_log_panel();
            }
            return Ok(true);
        }
        if super::super::jobs::jobs_chip_at(x, y) {
            self.toggle_jobs_panel();
            return Ok(true);
        }
        if self.volume_at(x, y).is_some() {
            self.pointer = Some(Pointer::Volume);
            self.drag_volume(x, y);
            return Ok(true);
        }
        if self.press_vertical_scrollbar(x, y) {
            return Ok(true);
        }
        if within(self.regions.panes[self.focused].minimap, x, y) {
            self.press_minimap(y);
            return Ok(true);
        }
        if self.press_slider(x, y) {
            return Ok(true);
        }
        // The scrollbar of a replay's timeline scrolls it; a block
        // of the timeline puts its code up.
        if self.press_timeline_bar(x, y)
            || self.press_block_start(x, y)
            || self.click_timeline_block(x, y)
        {
            return Ok(true);
        }
        if self
            .pane_at(x, y)
            .is_some_and(|pane| within(self.regions.panes[pane].timeline, x, y))
        {
            self.pointer = Some(Pointer::Panel);
            return Ok(true);
        }
        // Any other press lets go of an armed slider.
        self.armed_slider = None;
        self.own_text_selection(TextSurface::Score);
        self.pointer = Some(Pointer::Editor);
        Ok(false)
    }

    /// A left drag, taken by whatever the press began on: a fader, a
    /// slider, the minimap or the scrollbar follows the pointer, and a
    /// selection in a prompt, the theme code, the log, the reference or
    /// the mixer is pulled through its text. Returns true when that
    /// surface took the drag; false passes it on to the score's editor.
    fn drag_pointer(&mut self, mouse: MouseEvent, x: u16, y: u16) -> bool {
        match self.pointer.clone() {
            Some(Pointer::Volume) => {
                self.drag_volume(x, y);
                true
            }
            Some(Pointer::MixerPanelFader) => {
                self.drag_mixer_panel_fader(y);
                true
            }
            Some(Pointer::MixerFader {
                scene,
                call_from,
                rail,
            }) => {
                self.drag_mixer_fader(scene, call_from, rail, x);
                true
            }
            Some(Pointer::PromptField) => {
                self.drag_prompt_field(x, y);
                true
            }
            Some(Pointer::ThemeCode) => {
                self.forward_mouse_to_theme_code(mouse);
                true
            }
            Some(Pointer::Log) => {
                self.forward_mouse_to_log(mouse);
                true
            }
            #[cfg(feature = "hydra")]
            Some(Pointer::SnippetCodeScroll { row, scroll }) => {
                self.drag_snippet_code_scroll(row, scroll, y);
                true
            }
            #[cfg(feature = "hydra")]
            Some(Pointer::GeneratorControl { index, rail }) => {
                self.drag_generator_control(index, rail, x);
                true
            }
            Some(Pointer::PreviewVolume) => {
                self.drag_preview_volume(x);
                true
            }
            Some(Pointer::Minimap { grab_rows }) => {
                self.drag_minimap(y, grab_rows);
                true
            }
            Some(Pointer::Scrollbar) => {
                self.press_scrollbar(y);
                true
            }
            Some(Pointer::Slider {
                scene,
                call_from,
                track,
            }) => {
                self.drag_slider(scene, call_from, track, x);
                true
            }
            Some(Pointer::Panel) => true,
            Some(Pointer::ReferenceText { granularity }) => {
                self.drag_reference_selection(x, y, granularity);
                true
            }
            Some(Pointer::MixerText { granularity }) => {
                self.drag_mixer_selection(x, y, granularity);
                true
            }
            _ => false,
        }
    }

    /// A left release lets go of whatever the press began on: a fader or
    /// slider takes the last motion the release carries, a generator
    /// control sends its last preview, and the theme code and the log
    /// close the drag they keep of their own. Returns true when the
    /// release was dealt with here; false passes it on to the score's
    /// editor.
    fn release_pointer(&mut self, mouse: MouseEvent, x: u16, y: u16) -> bool {
        let released = self.pointer.take();
        if let Some(Pointer::MixerFader {
            scene,
            call_from,
            rail,
        }) = released
        {
            // The release can carry the last motion, as the pill's does.
            self.drag_mixer_fader(scene, call_from, rail, x);
            self.follow_pointer_shape(x, y);
            return true;
        }
        if let Some(Pointer::Slider {
            scene,
            call_from,
            track,
        }) = released
        {
            // The release can contain the last motion (or be the
            // only report after a quick click-and-drag).
            self.drag_slider(scene, call_from, track, x);
            self.follow_pointer_shape(x, y);
            return true;
        }
        self.follow_pointer_shape(x, y);
        #[cfg(feature = "hydra")]
        if matches!(released, Some(Pointer::GeneratorControl { .. })) {
            self.generator_last_preview = None;
            self.flush_generator_preview(Instant::now());
            return true;
        }
        #[cfg(feature = "hydra")]
        if matches!(released, Some(Pointer::SnippetCodeScroll { .. })) {
            return true;
        }
        // The theme editor's code buffer keeps a drag of its own, and
        // only the release closes it. If the release were swallowed here,
        // that drag would stay live, and the wheel extends a live drag on
        // purpose: every later scroll would re-anchor the selection to the
        // text under the stationary pointer.
        if released == Some(Pointer::ThemeCode) {
            self.forward_mouse_to_theme_code(mouse);
            return true;
        }
        if released == Some(Pointer::Log) {
            self.forward_mouse_to_log(mouse);
            return true;
        }
        // Letting go of the master volume, the minimap, the pane's
        // scrollbar, a panel, the preview volume, a desk fader, a prompt's
        // field or a reference or desk selection needs nothing more. Only
        // the score's own gesture goes on to the score's editor: it never
        // saw these presses, so their release is not its to finish.
        matches!(
            released,
            Some(
                Pointer::Volume
                    | Pointer::Minimap { .. }
                    | Pointer::Scrollbar
                    | Pointer::Panel
                    | Pointer::PreviewVolume
                    | Pointer::MixerPanelFader
                    | Pointer::PromptField
                    | Pointer::ReferenceText { .. }
                    | Pointer::MixerText { .. }
            )
        )
    }

    /// The wheel, offered to what is under the pointer from the top-most
    /// surface down, as a press is: `direction` is what the swipe means to
    /// a control, `sideways` which way the fingers went. Returns true when
    /// a surface took it; false leaves it to the score's editor.
    fn scroll_wheel(
        &mut self,
        mouse: MouseEvent,
        x: u16,
        y: u16,
        direction: f32,
        sideways: bool,
    ) -> bool {
        if self.scroll_log_panel(mouse, x, y) || self.scroll_theme_picker(direction) {
            return true;
        }
        // Over the pulse block the wheel is the preview volume's - before
        // the reference's own scrolling claims the column, exactly as the
        // press does.
        if self.scroll_preview_volume(x, y, direction) {
            return true;
        }
        // The reference over the sheets when it was raised after them.
        if self.reference_on_top() && self.scroll_reference(x, y, direction) {
            return true;
        }
        if self.scroll_settings_sheet(x, y, direction)
            || self.scroll_theme_editor(mouse, x, y, direction)
        {
            return true;
        }
        // The reference under the sheets, over the docks.
        if self.scroll_reference(x, y, direction) {
            return true;
        }
        // Below the sheets: the panels, then the stage.
        if self.scroll_mixer_panel(x, y, direction)
            || self.scroll_mixer_fader(x, y, direction)
            || self.scroll_set_panel(x, y, direction)
            || self.memory_dock_at(x, y)
            || self.scroll_viz_panel(x, y, direction)
        {
            return true;
        }
        if self.scroll_timeline(x, y, direction, sideways) {
            return true;
        }
        if self.volume_at(x, y).is_some() {
            self.nudge_master_gain_db(direction * VOLUME_SCROLL_STEP_DB);
            return true;
        }
        if let Some((scene, chip)) = self.slider_at(x, y) {
            self.nudge_live_slider(scene, &chip, f64::from(direction), false);
            return true;
        }
        // The wheel scrolls the pane under the pointer, focused or
        // not: in a split, the other scene rolls without the caret
        // leaving its own.
        if let Some(index) = self.pane_at(x, y)
            && index != self.focused
            && within(self.regions.panes[index].editor, x, y)
        {
            self.scroll_pane(index, f64::from(direction));
            return true;
        }
        false
    }

    pub(super) fn footer_hits(&self) -> view::FooterHits {
        let content = view::footer_content_area(
            self.regions.footer,
            self.footer_notice_rows(),
            self.has_piano_notes(),
            self.ui_settings.show_footer,
        );
        if !self.ui_settings.show_footer {
            return view::status_footer_hits(content);
        }
        let device = self
            .snapshot
            .as_ref()
            .and_then(|snapshot| snapshot.device.as_ref())
            .map(StudioDeviceInfo::name);
        let input = self
            .snapshot
            .as_ref()
            .and_then(|snapshot| snapshot.input_device.as_deref());
        view::footer_hits(
            content,
            device,
            input,
            self.devices.inventory().midi_port_counts(),
            rustel_core::gamepad::connected_pads().len(),
            self.snapshot
                .as_ref()
                .map(|snapshot| snapshot.orbits.as_slice())
                .unwrap_or(&[]),
        )
    }

    /// Where the header's readiness chip is - `✓ ready`, `✗ 5 problems`.
    ///
    /// The chip is the one place on screen that counts what the log has to
    /// say about the score, so pressing it opens the log.
    pub(super) fn go_chip(&self) -> Rect {
        view::header_go_chip(
            self.regions.header,
            view::TransportWord {
                evaluating: self.is_evaluating(),
                stopping: self.is_stopping(),
                playing: self.is_playing(),
            },
            self.go_state(),
        )
    }

    /// Give the selection to one surface and take it from every other.
    ///
    /// A screen showing two selections is showing two answers to "what
    /// does ^C take", and only one of them is true - whichever the copy
    /// path happens to ask for first. Worse, the stale one is the one the
    /// eye has stopped watching: a band left in the log while a name is
    /// picked out in the score is a line of log output about to be pasted
    /// into the music.
    ///
    /// Every terminal and every desktop behaves this way, which is why
    /// nobody thinks about it until a studio does not.
    pub(super) fn own_text_selection(&mut self, owner: TextSurface) {
        fn collapse(editor: &mut Editor) -> bool {
            let selection = editor.primary_selection();
            if selection.is_empty() {
                return false;
            }
            // To a caret at the head: the insertion point stays where the
            // hand last left it, so a surface that gets the keyboard back
            // does not also jump.
            let _ = editor.set_selection(super::super::editor::Selection::caret(selection.head));
            true
        }

        let mut cleared = false;
        if owner != TextSurface::Score {
            let scenes: Vec<SceneId> = self.panes.iter().map(|pane| pane.scene).collect();
            for scene in scenes {
                if let Some(scene) = self.scenes.get_mut(scene) {
                    cleared |= collapse(&mut scene.editor);
                }
            }
        }
        if owner != TextSurface::Log
            && let Some(panel) = self.log_panel.as_mut()
        {
            cleared |= collapse(&mut panel.editor);
        }
        if owner != TextSurface::ThemeCode
            && let Some(editor) = self.theme_editor.as_mut()
        {
            cleared |= collapse(&mut editor.code);
        }
        if owner != TextSurface::Reference {
            cleared |= self
                .reference_panel
                .as_mut()
                .is_some_and(|panel| panel.selection.take().is_some());
        }
        if owner != TextSurface::Mixer {
            cleared |= self.mixer_selection.take().is_some();
        }
        if cleared {
            self.dirty_frame = true;
        }
    }

    /// A pointer the terminal reports in pixels, brought back to the cell
    /// grid everything is laid out on - keeping the fraction of the cell it
    /// was in for the controls that can use it. A terminal reporting cells
    /// leaves both fractions at nought, and every position means what it
    /// did.
    pub(super) fn pointer_to_cells(&mut self, event: &mut Event) {
        let Event::Mouse(mouse) = event else {
            return;
        };
        let cell = self
            .features
            .pixel_mouse
            .then_some(self.features.cell_pixels)
            .flatten()
            .filter(|(width, height)| *width > 0 && *height > 0);
        let Some((width, height)) = cell else {
            self.pointer_subcell = 0.0;
            self.pointer_subrow = 0.0;
            return;
        };
        let (x, y) = (mouse.column, mouse.row);
        mouse.column = x / width;
        mouse.row = y / height;
        self.pointer_subcell = f64::from(x % width) / f64::from(width);
        self.pointer_subrow = f64::from(y % height) / f64::from(height);
    }

    pub(super) fn refresh_cell_pixels(&mut self, current: Option<(u16, u16)>) {
        if let Some(size) = current.filter(|(width, height)| *width > 0 && *height > 0)
            && Some(size) != self.last_pty_cell_pixels
        {
            self.features.cell_pixels = Some(size);
            self.last_pty_cell_pixels = Some(size);
            super::super::graphics::set_tier(self.ui_settings.rendering.resolve(&self.features));
            self.dirty_frame = true;
        }
        super::super::graphics::set_cell_pixels(self.features.cell_pixels);
    }

    /// A drag through a prompt's field extends its selection.
    fn drag_prompt_field(&mut self, x: u16, y: u16) {
        let frame = self.frame;
        let picker = self
            .set_prompt
            .as_mut()
            .map(|(_, picker)| picker)
            .or_else(|| {
                self.viz_docks
                    .iter_mut()
                    .flatten()
                    .find_map(|panel| panel.prompt.as_mut())
            });
        if let Some(picker) = picker
            && let Some(at) = picker.field_at(frame, x, y)
        {
            picker.caret_to(at, true);
            self.dirty_frame = true;
        }
    }
}

/// A selection grown to the granularity a repeated click chose: the word
/// under each end, or each end's whole line.
pub(super) fn widen_selection(
    selection: super::super::textblock::TextSelection,
    granularity: super::super::textblock::Granularity,
    lines: &[String],
) -> super::super::textblock::TextSelection {
    use super::super::textblock::{Granularity, TextPoint, TextSelection, word_range};
    let (from, to) = selection.ordered();
    let line_chars = |line: usize| lines.get(line).map_or(0, |text| text.chars().count());
    let (from, to) = match granularity {
        Granularity::Character => return selection,
        Granularity::Word => {
            let start = lines
                .get(from.line)
                .map_or(from.column..from.column, |text| {
                    word_range(text, from.column)
                });
            let end = lines.get(to.line).map_or(to.column..to.column, |text| {
                word_range(
                    text,
                    to.column
                        .saturating_sub(usize::from(!selection.is_empty() && to.column > 0)),
                )
            });
            (
                TextPoint {
                    line: from.line,
                    column: start.start,
                },
                TextPoint {
                    line: to.line,
                    column: end.end,
                },
            )
        }
        Granularity::Line => (
            TextPoint {
                line: from.line,
                column: 0,
            },
            TextPoint {
                line: to.line,
                column: line_chars(to.line),
            },
        ),
    };
    // The anchor keeps its side, so a widened drag still extends the way it
    // was being dragged.
    if selection.anchor <= selection.head {
        TextSelection {
            anchor: from,
            head: to,
        }
    } else {
        TextSelection {
            anchor: to,
            head: from,
        }
    }
}
