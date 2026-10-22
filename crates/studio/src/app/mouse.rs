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
        // A prompt is a popover: it owns the pointer until it is gone. A
        // press outside it is the press that puts it away, and the wheel
        // stays with it. Letting either reach the page behind moved the
        // focus there while the prompt stayed up, and Esc then closed the
        // page instead of the prompt. A drag in the prompt's field still
        // needs the routing below.
        if self.menu.is_none() && self.set_prompt.is_some() {
            match mouse.kind {
                MouseEventKind::Down(MouseButton::Left) => {
                    let extend = super::super::editor::click_extends(mouse.modifiers);
                    if !self.click_set_prompt(x, y, extend) {
                        self.close_set_prompt();
                        self.status = "set prompt closed".into();
                        self.dirty_frame = true;
                    }
                    return Ok(());
                }
                MouseEventKind::Down(_)
                | MouseEventKind::ScrollUp
                | MouseEventKind::ScrollDown
                | MouseEventKind::ScrollLeft
                | MouseEventKind::ScrollRight => return Ok(()),
                _ => {}
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
        if let Some(picker) = picker {
            picker.cancel_click();
            if let Some(at) = picker.field_at(frame, x, y) {
                picker.caret_to(at, true);
                self.dirty_frame = true;
            }
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

#[cfg(test)]
mod tests {
    mod keybind_mouse {
        //! The Keybinds page works from the mouse as well as the keyboard: a row
        //! is a button - the click arms the learn on it, a second click calls it
        //! off - and the page's own geometry answers where the rows live, so a
        //! test clicks the way a hand would.

        use crate::app::tests::support::app_over;
        use crate::app::*;
        use crate::keybinds::KeybindLearn;
        use crate::settings::{
            KEYBIND_ACTION_START, SettingsPage, SettingsSheet, SettingsSheetView,
            TERMINAL_PROFILE_ROW,
        };

        /// The first of the five panel rows, which close the table.
        const SHOW_FILE_ROW: usize = crate::settings::RESET_KEYBINDS_ROW - 5;

        /// A studio with the settings sheet open on the Keybinds page, focused,
        /// and the frame sized the way a painted one would be - the sheet's
        /// hit-tests read `self.frame`, so that is all they need.
        fn keybinds_page_app(directory: &std::path::Path) -> App {
            let mut app = app_over(directory);
            app.frame = Rect::new(0, 0, 140, 40);
            app.capabilities = KeyboardCapabilities::enhanced();
            let mut sheet = SettingsSheet::default();
            sheet.show_page(SettingsPage::Keybinds);
            // The row count the app refreshes on every key and click arrival: the
            // learnable rows plus the fixed ones past them.
            sheet.keybind_count = crate::settings::RESET_KEYBINDS_ROW + 1;
            app.settings_sheet = Some(sheet);
            app.focus = Focus::Panel(PanelKind::Settings);
            app
        }

        /// A click, Down then Up, through the whole routing as the terminal
        /// sends it. The app acts on the Down.
        fn click(app: &mut App, x: u16, y: u16) {
            for kind in [
                MouseEventKind::Down(MouseButton::Left),
                MouseEventKind::Up(MouseButton::Left),
            ] {
                app.handle_terminal_event(Event::Mouse(MouseEvent {
                    kind,
                    column: x,
                    row: y,
                    modifiers: KeyModifiers::NONE,
                }))
                .expect("the click is handled");
            }
        }

        /// Find the row's label in the painted sheet before clicking it.
        fn row_point(app: &mut App, index: usize) -> (u16, u16) {
            let label = app.keybind_rows()[index].action;
            let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(
                app.frame.width,
                app.frame.height,
            ))
            .expect("test terminal");
            let painted = terminal
                .draw(|frame| app.paint_into(frame, Instant::now()).expect("frame paints"))
                .expect("draw");
            let (_, rows) =
                SettingsSheetView::geometry(app.settings_sheet_frame()).expect("the sheet fits");
            let y = (rows.y..rows.bottom())
                .find(|&y| {
                    (rows.x..rows.right())
                        .map(|x| painted.buffer[(x, y)].symbol())
                        .collect::<String>()
                        .contains(label)
                })
                .expect("the row label is visible");
            (rows.x + 2, y)
        }

        /// A click on a row arms the learn on its action, exactly as Enter does.
        #[test]
        fn clicking_a_keybind_row_arms_the_learn() {
            let directory = tempfile::tempdir().unwrap();
            let mut app = keybinds_page_app(directory.path());
            let (x, y) = row_point(&mut app, KEYBIND_ACTION_START); // the first binding is update / play

            click(&mut app, x, y);

            assert!(app.keybind_learn.is_some(), "the learn armed");
            assert_eq!(
                app.keybind_learn.unwrap().action(),
                BindAction::Evaluate,
                "the row clicked is the row armed"
            );
            assert!(
                app.keybind_rows()[KEYBIND_ACTION_START].learning,
                "the row says it is waiting for a chord"
            );
        }

        #[test]
        fn clicking_the_terminal_row_cycles_the_profile_without_learning_a_binding() {
            let directory = tempfile::tempdir().unwrap();
            let mut app = keybinds_page_app(directory.path());
            app.ui_settings.terminal_profile = None;
            app.keybind_learn = Some(KeybindLearn::arm(BindAction::Evaluate));
            let custom = app.keybinds.prefs();
            let (x, y) = row_point(&mut app, TERMINAL_PROFILE_ROW);

            click(&mut app, x, y);

            assert_eq!(app.settings_sheet.unwrap().selected, TERMINAL_PROFILE_ROW);
            assert_eq!(
                app.ui_settings.terminal_profile.as_deref(),
                Some("Windows Terminal")
            );
            assert!(
                app.keybind_learn.is_none(),
                "the profile is not a learnable binding"
            );
            assert_eq!(
                app.keybinds.prefs(),
                custom,
                "a profile click learns nothing"
            );
            assert_eq!(app.keybind_rows()[TERMINAL_PROFILE_ROW].action, "Terminal");
            assert_eq!(
                app.keybind_rows()[KEYBIND_ACTION_START].action,
                BindAction::Evaluate.label()
            );
            assert_eq!(app.focus, Focus::Panel(PanelKind::Settings));
        }

        /// A second click on the armed row calls the learn off - the mouse's
        /// answer to Esc - and learns nothing in the meantime.
        #[test]
        fn a_second_click_on_the_armed_row_calls_the_learn_off() {
            let directory = tempfile::tempdir().unwrap();
            let mut app = keybinds_page_app(directory.path());
            let (x, y) = row_point(&mut app, KEYBIND_ACTION_START + 1); // stop all sound

            app.keybind_learn = Some(KeybindLearn::arm(BindAction::Stop));
            click(&mut app, x, y);

            assert!(
                app.keybind_learn.is_none(),
                "the learn was called off, not re-armed"
            );
            assert_eq!(
                app.keybinds.binding(BindAction::Stop).unwrap().key(),
                "ctrl+.",
                "calling a learn off learns nothing"
            );
        }

        /// While a take is being asked - the chord in hand is another action's -
        /// a click cannot take it: choosing to take is Enter's work alone.
        #[test]
        fn a_click_cannot_confirm_a_take() {
            let directory = tempfile::tempdir().unwrap();
            let mut app = keybinds_page_app(directory.path());
            app.keybinds.learn(
                BindAction::RenameScene,
                crate::keybinds::KeyCombo::parse("ctrl+r"),
            );
            // The ask is up for undo onto ^R, rename-scene's chord.
            app.keybind_learn = Some(KeybindLearn::Confirm {
                action: BindAction::Undo,
                chord: app.keybinds.binding(BindAction::RenameScene).unwrap(),
                held_by: BindAction::RenameScene,
            });
            let (x, y) = row_point(&mut app, KEYBIND_ACTION_START);

            click(&mut app, x, y);

            assert_eq!(
                app.keybinds.binding(BindAction::Undo).unwrap().key(),
                "ctrl+z",
                "the take did not happen by click"
            );
            assert!(
                app.keybind_learn.is_some(),
                "the ask was not silently swallowed either"
            );
        }

        /// A click below the rows claims the sheet - the editor underneath must
        /// not take the press - without arming anything.
        #[test]
        fn a_click_below_the_rows_claims_the_sheet_without_arming() {
            let directory = tempfile::tempdir().unwrap();
            let mut app = keybinds_page_app(directory.path());
            let (sheet, rows) = SettingsSheetView::geometry(app.frame).expect("the sheet fits");
            let x = rows.x + 2;
            let y = sheet.bottom().saturating_sub(1); // the hint row, not a chord row

            click(&mut app, x, y);

            assert!(app.keybind_learn.is_none(), "no row, no learn");
            assert_eq!(
                app.focus,
                Focus::Panel(PanelKind::Settings),
                "the press still belongs to the sheet"
            );
        }

        /// The panel actions close the table. Each row shows the Alt chord
        /// the panel answers to until a chord is learnt.
        #[test]
        fn the_panel_rows_close_the_table_on_their_alt_chords() {
            let directory = tempfile::tempdir().unwrap();
            let app = keybinds_page_app(directory.path());

            let rows = app.keybind_rows();
            assert_eq!(rows.len(), crate::settings::RESET_KEYBINDS_ROW + 1);
            let panel = &rows[SHOW_FILE_ROW..crate::settings::RESET_KEYBINDS_ROW];
            let chords: Vec<&str> = panel.iter().map(|row| row.chord.as_str()).collect();
            assert_eq!(
                chords,
                ["Alt+O", "Alt+R", "Alt+D", "Alt+T", "Alt+T"].map(crate::keybinds::shortcut_label),
                "the row names the chord"
            );
            assert!(
                panel[0].action.contains("file"),
                "the row says what the chord does: {}",
                panel[0].action
            );
            assert!(panel.iter().all(|row| !row.learnt && !row.learning));
        }

        /// Enter or a click on a panel row arms a learn, as on every other row.
        #[test]
        fn enter_or_a_click_on_a_panel_row_arms_a_learn() {
            let directory = tempfile::tempdir().unwrap();
            let mut app = keybinds_page_app(directory.path());
            // Selecting the row first scrolls the page, since at 140x40 the
            // row sits past the last visible one.
            if let Some(sheet) = app.settings_sheet.as_mut() {
                sheet.selected = SHOW_FILE_ROW;
            }
            let armed = Some(KeybindLearn::arm(BindAction::ShowFile));

            app.handle_terminal_event(Event::Key(KeyEvent::new(
                KeyCode::Enter,
                KeyModifiers::NONE,
            )))
            .expect("Enter is handled");
            assert_eq!(app.keybind_learn, armed);
            app.keybind_learn = None;

            let point = row_point(&mut app, SHOW_FILE_ROW);
            click(&mut app, point.0, point.1);
            assert_eq!(app.keybind_learn, armed);
        }

        /// The Sources page too: a click on a row selects it, exactly as the
        /// arrows do - an import or a shipped pack - and selects only. Acting on
        /// it stays the keys' work, so a click cannot start a download or drop an
        /// import; and the rule between the two lists is not a row.
        #[test]
        fn clicking_a_source_row_selects_it() {
            let directory = tempfile::tempdir().unwrap();
            let mut app = app_over(directory.path());
            app.frame = Rect::new(0, 0, 140, 40);
            app.prefs.sample_sources = ["/tmp/first", "/tmp/second"]
                .into_iter()
                .map(|spec| crate::prefs::SampleSourcePref {
                    spec: spec.to_owned(),
                    enabled: true,
                })
                .collect();
            let mut sheet = SettingsSheet::default();
            sheet.show_page(SettingsPage::Sources);
            // The app syncs the page's row counts as it draws and answers; a
            // test building the sheet by hand owes the same two numbers.
            sheet.source_count = app.prefs.sample_sources.len();
            sheet.default_count = app.shipped_sources.len();
            app.settings_sheet = Some(sheet);
            app.focus = Focus::Panel(PanelKind::Settings);
            let packs = app.shipped_sources.len();
            assert!(packs > 0, "the studio ships with packs");
            let (_, rows) = SettingsSheetView::geometry(app.frame).expect("the sheet fits");
            let x = rows.x + 3;

            // Each click lands where the page says the row is drawn, the same
            // mapping the handler uses: a click on a drawn row selects it.
            let drawn_at = |app: &App, row: usize| {
                (0..app.frame.height)
                    .map(|dy| rows.y + dy)
                    .find(|&y| {
                        app.settings_sheet
                            .as_ref()
                            .expect("the sheet is open")
                            .source_row_at(app.frame, x, y)
                            == Some(row)
                    })
                    .unwrap_or_else(|| panic!("row {row} is drawn on the page"))
            };
            // Row map with the four controls: imports at 4 and 5, packs from 6.
            let second_import = drawn_at(&app, 5);
            click(&mut app, x, second_import);
            assert_eq!(app.settings_sheet.unwrap().selected, 5, "the second import");
            let first_pack = drawn_at(&app, 6);
            click(&mut app, x, first_pack);
            assert_eq!(app.settings_sheet.unwrap().selected, 6, "the first pack");
            // The defaults header above the packs is a label, not a row.
            let header_above = first_pack - 1;
            click(&mut app, x, header_above);
            assert_eq!(
                app.settings_sheet.unwrap().selected,
                6,
                "the default-samples rule is not a row: the selection stays"
            );
            assert_eq!(app.prefs.sample_sources.len(), 2, "a click removes nothing");
            assert!(
                app.shipped_sources.iter().all(|pack| pack.cache.is_none()),
                "a click starts no download"
            );
            assert_eq!(app.focus, Focus::Panel(PanelKind::Settings));
        }

        fn press(app: &mut App, code: KeyCode) {
            app.handle_terminal_event(Event::Key(KeyEvent::new(code, KeyModifiers::NONE)))
                .expect("the key is handled");
        }

        /// `a` on the Sources page asks for the source over the page, which
        /// stays: Esc comes back to it rather than to a closed sheet, and an
        /// import lands on the page as a row, under the cursor, without the
        /// browser opening over it.
        #[test]
        fn adding_a_source_asks_over_the_sources_page() {
            let directory = tempfile::tempdir().unwrap();
            let kit = directory.path().join("kit");
            std::fs::create_dir_all(&kit).unwrap();
            let mut app = app_over(directory.path());
            app.frame = Rect::new(0, 0, 140, 40);
            let mut sheet = SettingsSheet::default();
            sheet.show_page(SettingsPage::Sources);
            app.settings_sheet = Some(sheet);
            app.focus = Focus::Panel(PanelKind::Settings);

            let on_sources = |app: &App| {
                app.settings_sheet
                    .is_some_and(|sheet| sheet.shows_sources())
            };

            press(&mut app, KeyCode::Char('a'));
            assert_eq!(
                app.set_prompt.as_ref().map(|(prompt, _)| *prompt),
                Some(SetPrompt::AddSampleSource),
                "a asks for the source"
            );
            assert!(on_sources(&app), "the page stays under the prompt");

            press(&mut app, KeyCode::Esc);
            assert!(app.set_prompt.is_none(), "Esc closes the prompt");
            assert!(on_sources(&app), "and comes back to the page");
            assert_eq!(app.focus, Focus::Panel(PanelKind::Settings));

            press(&mut app, KeyCode::Char('a'));
            app.set_prompt
                .as_mut()
                .unwrap()
                .1
                .offer(&kit.to_string_lossy());
            press(&mut app, KeyCode::Enter);
            assert!(
                app.set_prompt.is_none(),
                "Enter imports and closes the prompt"
            );
            assert_eq!(
                app.prefs
                    .sample_sources
                    .iter()
                    .map(|source| source.spec.as_str())
                    .collect::<Vec<_>>(),
                vec![kit.to_string_lossy().as_ref()],
                "the folder is imported"
            );
            assert!(on_sources(&app), "the page is still up");
            assert_eq!(
                app.settings_sheet.unwrap().selected,
                crate::settings::SOURCE_CONTROL_COUNT,
                "the cursor is on the new row, past the cache controls"
            );
            assert_eq!(app.focus, Focus::Panel(PanelKind::Settings));
            assert!(
                app.reference_panel.is_none(),
                "the browser does not open over the page"
            );
        }

        /// A folder dropped while the Sources page is open imports onto that
        /// page: the sheet stays, the browser does not open over it, and the
        /// cursor lands on the new row.
        #[test]
        fn dropping_on_the_sources_page_imports_there() {
            let directory = tempfile::tempdir().unwrap();
            let kit = directory.path().join("kit");
            std::fs::create_dir_all(&kit).unwrap();
            std::fs::write(kit.join("kick.wav"), b"RIFF....WAVEfmt ").unwrap();
            let mut app = app_over(directory.path());
            app.frame = Rect::new(0, 0, 140, 40);
            let mut sheet = SettingsSheet::default();
            sheet.show_page(SettingsPage::Sources);
            app.settings_sheet = Some(sheet);
            app.focus = Focus::Panel(PanelKind::Settings);

            app.handle_terminal_event(Event::Paste(kit.to_string_lossy().into_owned()))
                .expect("the drop is handled");

            assert!(
                app.settings_sheet
                    .is_some_and(|sheet| sheet.shows_sources()),
                "the Sources page stays"
            );
            assert_eq!(
                app.prefs
                    .sample_sources
                    .iter()
                    .map(|source| source.spec.as_str())
                    .collect::<Vec<_>>(),
                vec![kit.to_string_lossy().as_ref()]
            );
            assert_eq!(
                app.settings_sheet.unwrap().selected,
                crate::settings::SOURCE_CONTROL_COUNT,
                "the cursor is on the new import"
            );
            assert!(
                app.reference_panel.is_none(),
                "the samples browser does not open over the page"
            );
            assert_eq!(app.focus, Focus::Panel(PanelKind::Settings));
        }

        /// Emptying the sample cache asks twice, clears pack counts at once, and
        /// does the folder walk off the UI thread so Enter cannot freeze the sheet.
        #[test]
        fn emptying_the_sample_cache_asks_twice_and_refreshes_the_packs() {
            let directory = tempfile::tempdir().unwrap();
            let mut app = app_over(directory.path());
            app.frame = Rect::new(0, 0, 140, 40);
            let mut sheet = SettingsSheet::default();
            sheet.show_page(SettingsPage::Sources);
            // The clear-cache control is the fourth row: the pre-cache switch,
            // the defaults ask, the refresh, then the cache empty.
            sheet.selected = 3;
            app.settings_sheet = Some(sheet);
            app.focus = Focus::Panel(PanelKind::Settings);
            if let Some(pack) = app.shipped_sources.first_mut() {
                pack.files = 10;
                pack.cached_files = 10;
                pack.cache_bytes = 1_000_000;
            }
            app.sample_cache_bytes = Some(1_000_000);

            press(&mut app, KeyCode::Enter);
            assert!(
                app.settings_sheet
                    .is_some_and(|sheet| sheet.confirm_clear_cache),
                "the first Enter arms the empty"
            );
            assert!(
                app.status.contains("Enter again"),
                "the status asks for a second Enter: {}",
                app.status
            );
            assert_eq!(
                app.shipped_sources[0].cached_files, 10,
                "nothing is cleared until confirmed"
            );

            press(&mut app, KeyCode::Esc);
            assert!(
                app.settings_sheet
                    .is_some_and(|sheet| !sheet.confirm_clear_cache),
                "Esc cancels the ask"
            );
            assert!(
                app.settings_sheet
                    .is_some_and(|sheet| sheet.shows_sources()),
                "Esc keeps the sheet"
            );

            press(&mut app, KeyCode::Enter);
            press(&mut app, KeyCode::Enter);
            assert!(
                app.cache_clear.is_some() || app.sample_cache_bytes.is_none(),
                "the clear is under way or already reflected"
            );
            assert_eq!(
                app.shipped_sources[0].cached_files, 0,
                "pack rows stop claiming the cache at once"
            );
            assert_eq!(app.shipped_sources[0].cache_bytes, 0);
            // Drain the background clear so the test process does not race drop.
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
            while app.cache_clear.is_some() && std::time::Instant::now() < deadline {
                std::thread::sleep(std::time::Duration::from_millis(10));
                app.poll_readiness();
            }
            assert!(app.cache_clear.is_none(), "the clear finishes");
        }

        #[test]
        fn reset_all_click_requests_but_never_confirms_the_reset() {
            let directory = tempfile::tempdir().unwrap();
            let mut app = keybinds_page_app(directory.path());
            app.keybinds.learn(
                BindAction::Undo,
                crate::keybinds::KeyCombo::parse("ctrl+shift+q"),
            );
            let custom = app.keybinds.prefs();
            let at = crate::settings::RESET_KEYBINDS_ROW;
            app.settings_sheet.as_mut().unwrap().select(at);
            let (x, y) = row_point(&mut app, at);
            click(&mut app, x, y);
            assert!(app.settings_sheet.unwrap().confirm_reset_keybinds);
            assert!(app.keybind_learn.is_none());
            click(&mut app, x, y);
            assert_eq!(
                app.keybinds.prefs(),
                custom,
                "a double click is not confirmation"
            );
            app.handle_terminal_event(Event::Key(KeyEvent::new(
                KeyCode::Enter,
                KeyModifiers::NONE,
            )))
            .unwrap();
            assert!(app.keybinds.prefs().is_empty());
        }
    }

    mod mouse {
        //! Tests for `app/mouse.rs`.

        use crate::app::tests::support::*;
        use crate::app::*;
        use crate::editor::MemoryClipboard;

        #[test]
        fn resizing_cancels_the_old_track_and_refreshed_metrics_convert_new_pixels() {
            let directory = tempfile::tempdir().expect("temp dir");
            let (mut app, scene, call) = slider_app(directory.path());
            let (x, _, row) = lay_out(&mut app, scene, call);
            mouse(&mut app, MouseEventKind::Down(MouseButton::Left), x, row);
            app.handle_terminal_event(Event::Resize(100, 30))
                .expect("resize handled");
            assert!(!matches!(app.pointer, Some(Pointer::Slider { .. })));
            mouse(&mut app, MouseEventKind::Up(MouseButton::Left), 99, row);
            assert_eq!(
                value(&app, scene, call),
                0.0,
                "old coordinates cannot move a resized rail"
            );

            app.features.pixel_mouse = true;
            app.features.cell_pixels = Some((9, 18));
            app.last_pty_cell_pixels = Some((10, 20));
            app.refresh_cell_pixels(Some((10, 20)));
            assert_eq!(
                app.features.cell_pixels,
                Some((9, 18)),
                "an unchanged PTY estimate preserves the terminal's direct report"
            );
            app.refresh_cell_pixels(Some((11, 22)));
            assert_eq!(app.features.cell_pixels, Some((11, 22)));
            let mut event = Event::Mouse(MouseEvent {
                kind: MouseEventKind::Moved,
                column: 38,
                row: 55,
                modifiers: KeyModifiers::NONE,
            });
            app.pointer_to_cells(&mut event);
            let Event::Mouse(converted) = event else {
                panic!("mouse event")
            };
            assert_eq!((converted.column, converted.row), (3, 2));
            assert!((app.pointer_subcell - 5.0 / 11.0).abs() < 1e-9);
            app.refresh_cell_pixels(None);
            app.refresh_cell_pixels(Some((0, 0)));
            assert_eq!(
                app.features.cell_pixels,
                Some((11, 22)),
                "failed metric reads retain known dimensions"
            );
        }

        #[test]
        fn late_cell_dimensions_activate_the_selected_pixel_renderer() {
            use crate::graphics::{self, RenderingMode, Tier};
            let directory = tempfile::tempdir().unwrap();
            let (mut app, _, _) = slider_app(directory.path());
            let before = graphics::tier();
            app.features.kitty_graphics = true;
            app.features.fine_glyphs = true;
            app.features.cell_pixels = None;
            app.last_pty_cell_pixels = None;
            app.ui_settings.rendering = RenderingMode::Kitty;
            graphics::set_tier(app.ui_settings.rendering.resolve(&app.features));
            assert_eq!(
                graphics::tier(),
                Tier::Fine,
                "safe fallback while dimensions are unavailable"
            );
            app.refresh_cell_pixels(Some((10, 20)));
            assert_eq!(graphics::tier(), Tier::Pixels);
            graphics::set_tier(before);
        }

        /// A terminal reporting the pointer in pixels (DECSET 1016) is brought
        /// back to the cell grid, with the fraction of the cell kept for the
        /// slider; one reporting cells is left exactly as it was.
        #[test]
        fn a_pixel_pointer_is_brought_back_to_cells() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = app_over(directory.path());
            let report = |column: u16, row: u16| {
                Event::Mouse(MouseEvent {
                    kind: MouseEventKind::Moved,
                    column,
                    row,
                    modifiers: KeyModifiers::NONE,
                })
            };
            app.features.pixel_mouse = true;
            app.features.cell_pixels = Some((9, 18));
            let mut event = report(31, 40);
            app.pointer_to_cells(&mut event);
            let Event::Mouse(mouse) = event else {
                panic!("still a mouse event");
            };
            assert_eq!((mouse.column, mouse.row), (3, 2));
            assert!(
                (app.pointer_subcell - 4.0 / 9.0).abs() < 1e-9,
                "{}",
                app.pointer_subcell
            );
            // Down the cell as well as along it: the desk's faders are
            // columns, so the row's fraction is the one they need.
            assert!(
                (app.pointer_subrow - 4.0 / 18.0).abs() < 1e-9,
                "{}",
                app.pointer_subrow
            );

            // Cells are cells.
            app.features.pixel_mouse = false;
            let mut event = report(31, 40);
            app.pointer_to_cells(&mut event);
            let Event::Mouse(mouse) = event else {
                panic!("still a mouse event");
            };
            assert_eq!((mouse.column, mouse.row), (31, 40));
            assert_eq!(app.pointer_subcell, 0.0);
            assert_eq!(app.pointer_subrow, 0.0);

            // And pixels without a known cell size were never asked for; if
            // they arrive anyway they are left alone rather than divided by
            // nothing.
            app.features.pixel_mouse = true;
            app.features.cell_pixels = None;
            let mut event = report(31, 40);
            app.pointer_to_cells(&mut event);
            let Event::Mouse(mouse) = event else {
                panic!("still a mouse event");
            };
            assert_eq!((mouse.column, mouse.row), (31, 40));
        }

        /// One selection on the screen at a time. A band left lying in the
        /// log while a name is picked out in the score is a line of log
        /// output about to be pasted into the music - and two selections
        /// drawn at once are two answers to "what does ^C take".
        #[test]
        fn a_selection_in_one_surface_takes_it_from_the_others() {
            use crate::editor::Selection;
            use crate::textblock::{TextPoint, TextSelection};
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = app_over(directory.path());
            app.editor_mut()
                .dispatch(
                    Command::PasteText("$: s(\"bd\")".into()),
                    HistoryMoment(0),
                    &mut MemoryClipboard::default(),
                )
                .expect("seed");

            // A band in the score, and another in the log behind it.
            app.editor_mut()
                .set_selection(Selection::range(ByteOffset(0), ByteOffset(5)))
                .expect("score selection");
            app.toggle_log_panel();
            // The panel's editor holds the log's text, so there has to be
            // some before there is anything to select.
            app.log
                .push(LogLevel::Info, "test", "a line worth selecting");
            let list = Rect::new(0, 0, 80, 10);
            if let Some(panel) = app.log_panel.as_mut() {
                panel.prepare(&app.log, list);
                panel
                    .editor
                    .set_selection(Selection::range(ByteOffset(0), ByteOffset(3)))
                    .expect("log selection");
            }
            assert!(!app.editor().primary_selection().is_empty());

            // The log takes it: the score's band goes, and the caret stays
            // where the hand left it rather than jumping to the top.
            app.own_text_selection(TextSurface::Log);
            assert!(
                app.editor().primary_selection().is_empty(),
                "the score let go"
            );
            assert_eq!(
                app.editor().primary_selection().head,
                ByteOffset(5),
                "and kept its caret where the selection ended"
            );
            assert!(
                !app.log_panel
                    .as_ref()
                    .expect("the log")
                    .editor
                    .primary_selection()
                    .is_empty(),
                "the surface that claimed it keeps it"
            );

            // And back the other way, with the reference column in play too.
            app.toggle_reference_browse();
            if let Some(panel) = app.reference_panel.as_mut() {
                panel.selection = Some(crate::reference::PaneSelection {
                    target: crate::reference::SelectionTarget::Entry {
                        index: 0,
                        width: 40,
                    },
                    selection: TextSelection {
                        anchor: TextPoint { line: 0, column: 0 },
                        head: TextPoint { line: 0, column: 3 },
                    },
                });
            }
            app.own_text_selection(TextSurface::Score);
            assert!(
                app.log_panel
                    .as_ref()
                    .expect("the log")
                    .editor
                    .primary_selection()
                    .is_empty(),
                "the log let go"
            );
            assert!(
                app.reference_panel
                    .as_ref()
                    .is_none_or(|panel| panel.selection.is_none()),
                "and so did the column"
            );
        }

        #[test]
        fn a_reference_click_wins_over_the_settings_row_beneath_it() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = app_over(directory.path());
            app.frame = Rect::new(0, 0, 140, 40);
            press(&mut app, KeyCode::Char('o'), KeyModifiers::CONTROL);
            app.reference_panel = Some(ReferencePanel::samples(&app.reference, Vec::new()));
            app.focus_panel(PanelKind::Reference);
            app.regions = view::regions(app.frame, false, 1, true, true, None, None);

            let inner = crate::reference::inner_area(app.regions.reference);
            let (_, meter_y) = crate::reference::sample_pulse_rows(inner);
            let (sheet, _) =
                crate::settings::SettingsSheetView::geometry(app.frame).expect("settings sheet");
            let x = inner.x.max(sheet.x) + 1;
            assert!(within(sheet, x, meter_y) && within(app.regions.reference, x, meter_y));

            app.handle_mouse(MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: x,
                row: meter_y,
                modifiers: KeyModifiers::NONE,
            })
            .expect("click");

            assert_eq!(app.focus, Focus::Panel(PanelKind::Reference));
            assert!(app.reference_on_top());
            assert_eq!(app.pointer, Some(Pointer::PreviewVolume));
        }

        /// The readiness chip counts what the log has to say about the score,
        /// so pressing it is how you go and read it - and the pointer says so
        /// before you press.
        #[test]
        fn the_readiness_chip_opens_the_log() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = app_over(directory.path());
            app.frame = Rect::new(0, 0, 120, 40);
            app.regions = view::regions(app.frame, false, 1, false, true, None, None);
            // A score the linter refused, which is the state the chip counts.
            let scene = app.scenes.current().id;
            app.absorb_lint(crate::lint::LintResult {
                scene,
                revision: app.scenes.current().editor.revision(),
                input_channels: app.lint_input_channels(),
                diagnostics: vec![rustel_runtime::lint::Diagnostic {
                    level: LintLevel::Value,
                    message: "unknown sound \"top\"".into(),
                    from: 0,
                    to: 1,
                }],
            });
            assert_eq!(app.go_state(), view::GoState::Problems { count: 1 });
            let chip = app.go_chip();
            assert!(chip.width > 0, "there is a chip to press");
            assert_eq!(app.pointer_shape_at(chip.x, chip.y), "pointer");
            // Its neighbours are not it: the transport word to the left keeps
            // its own meaning, and so does the empty header to the right.
            assert_eq!(app.pointer_shape_at(chip.x - 2, chip.y), "default");
            assert_eq!(app.pointer_shape_at(chip.right() + 4, chip.y), "default");

            let press = |app: &mut App, x: u16, y: u16| {
                app.handle_terminal_event(Event::Mouse(MouseEvent {
                    kind: MouseEventKind::Down(MouseButton::Left),
                    column: x,
                    row: y,
                    modifiers: KeyModifiers::NONE,
                }))
                .expect("mouse handled");
            };

            assert!(app.log_panel.is_none());
            press(&mut app, chip.x + 1, chip.y);
            assert!(app.log_panel.is_some(), "the log opens where it was asked");
            assert_eq!(app.focus, Focus::Panel(PanelKind::Log));

            // Pressing it again with the log already open brings it forward
            // rather than shutting the thing you just asked to read.
            app.focus = Focus::Editor;
            press(&mut app, chip.x + 1, chip.y);
            assert!(app.log_panel.is_some());
            assert_eq!(app.focus, Focus::Panel(PanelKind::Log));
        }

        /// A drag in a prompt's field is the field's alone, down to the release: a
        /// score drag whose release never arrived stays where it was.
        #[test]
        fn a_prompt_field_drag_never_reaches_the_score() {
            let directory = tempfile::tempdir().unwrap();
            let mut app = app_over(directory.path());
            type_text(
                &mut app,
                "$: s(\"bd hh sd hh bd hh sd hh bd hh sd hh bd hh\")",
            );
            app.editor_mut().set_wrap(false);
            let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(120, 40))
                .expect("test terminal");
            let mut draw = |app: &mut App| {
                terminal
                    .draw(|frame| app.paint_into(frame, Instant::now()).expect("frame paints"))
                    .expect("draw");
            };
            draw(&mut app);

            // A drag in the score whose release never arrives.
            let score = app.focused_map().expect("the score is drawn").area();
            mouse(
                &mut app,
                MouseEventKind::Down(MouseButton::Left),
                score.x + 2,
                score.y,
            );
            mouse(
                &mut app,
                MouseEventKind::Drag(MouseButton::Left),
                score.x + 6,
                score.y,
            );
            draw(&mut app);
            let selection = app.editor().primary_selection();
            let viewport = app.editor().viewport();
            assert!(!selection.is_empty(), "a score drag is live");

            app.set_prompt = Some((SetPrompt::OpenSet, FilePicker::naming("new set", "create")));
            let prompt_text = "a longer set name 🎛️ café";
            app.set_prompt.as_mut().unwrap().1.offer(prompt_text);
            draw(&mut app);
            let picker = &app.set_prompt.as_ref().unwrap().1;
            let (area, list) = picker.geometry(app.frame).expect("the prompt fits");
            let row = area.y + 1;
            let start = list.x + picker.label.len() as u16 + 2;
            assert_eq!(picker.field_at(app.frame, start, row), Some(0));

            mouse(
                &mut app,
                MouseEventKind::Down(MouseButton::Left),
                start,
                row,
            );
            assert_eq!(app.pointer, Some(Pointer::PromptField));
            mouse(
                &mut app,
                MouseEventKind::Drag(MouseButton::Left),
                start + 4,
                row,
            );
            mouse(
                &mut app,
                MouseEventKind::Up(MouseButton::Left),
                start + 4,
                row,
            );

            assert_eq!(
                app.set_prompt.as_ref().unwrap().1.selection(),
                Some(0..4),
                "the drag selected in the prompt's field"
            );
            let copy = |app: &mut App| {
                app.handle_terminal_event(Event::Key(KeyEvent::new(
                    KeyCode::Char('c'),
                    KeyModifiers::CONTROL,
                )))
                .expect("copy handled");
            };
            copy(&mut app);
            assert_eq!(app.clipboard.get_text().unwrap(), "a lo");

            for kind in [
                MouseEventKind::Down(MouseButton::Left),
                MouseEventKind::Up(MouseButton::Left),
                MouseEventKind::Down(MouseButton::Left),
                MouseEventKind::Drag(MouseButton::Left),
                MouseEventKind::Up(MouseButton::Left),
            ] {
                let jitter = u16::from(matches!(kind, MouseEventKind::Drag(_)));
                mouse(&mut app, kind, start + 4 + jitter, row);
            }
            assert_eq!(
                app.set_prompt.as_ref().unwrap().1.selection(),
                Some(0..prompt_text.chars().count()),
                "a double click selects the whole field through pointer jitter"
            );
            copy(&mut app);
            assert_eq!(app.clipboard.get_text().unwrap(), prompt_text);

            for kind in [
                MouseEventKind::Down(MouseButton::Left),
                MouseEventKind::Up(MouseButton::Left),
            ] {
                mouse(&mut app, kind, start + 4, row);
            }
            assert!(app.set_prompt.as_ref().unwrap().1.selection().is_none());
            copy(&mut app);
            assert_eq!(app.clipboard.get_text().unwrap(), prompt_text);
            assert_eq!(
                app.editor().primary_selection(),
                selection,
                "the score's selection did not follow the prompt's release"
            );
            assert_eq!(
                app.editor().viewport(),
                viewport,
                "the score did not scroll"
            );
        }

        /// A press starts a new gesture wherever it lands. The go chip takes its
        /// press without holding the pointer, so the drag and release after it
        /// went on to the score's editor, which still held a drag whose release
        /// had never arrived.
        #[test]
        fn a_press_that_holds_no_pointer_still_ends_the_last_drag() {
            let directory = tempfile::tempdir().unwrap();
            let mut app = app_over(directory.path());
            type_text(
                &mut app,
                "$: s(\"bd hh sd hh bd hh sd hh bd hh sd hh bd hh\")",
            );
            app.editor_mut().set_wrap(false);
            let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(120, 40))
                .expect("test terminal");
            let mut draw = |app: &mut App| {
                terminal
                    .draw(|frame| app.paint_into(frame, Instant::now()).expect("frame paints"))
                    .expect("draw");
            };
            draw(&mut app);

            // A drag in the score whose release never arrives.
            let score = app.focused_map().expect("the score is drawn").area();
            mouse(
                &mut app,
                MouseEventKind::Down(MouseButton::Left),
                score.x + 2,
                score.y,
            );
            mouse(
                &mut app,
                MouseEventKind::Drag(MouseButton::Left),
                score.x + 6,
                score.y,
            );
            draw(&mut app);
            let selection = app.editor().primary_selection();
            let viewport = app.editor().viewport();
            assert!(!selection.is_empty(), "a score drag is live");

            // A score the linter refused, so the header shows the go chip.
            let scene = app.scenes.current().id;
            app.absorb_lint(crate::lint::LintResult {
                scene,
                revision: app.scenes.current().editor.revision(),
                input_channels: app.lint_input_channels(),
                diagnostics: vec![rustel_runtime::lint::Diagnostic {
                    level: LintLevel::Value,
                    message: "unknown sound \"top\"".into(),
                    from: 0,
                    to: 1,
                }],
            });
            draw(&mut app);
            let chip = app.go_chip();
            assert!(!chip.is_empty(), "the header shows the go chip");
            assert!(app.log_panel.is_none());
            mouse(
                &mut app,
                MouseEventKind::Down(MouseButton::Left),
                chip.x,
                chip.y,
            );
            assert!(app.log_panel.is_some(), "the chip opened the log");
            draw(&mut app);
            for column in [chip.x + 1, chip.x + 2, score.x + score.width - 1] {
                mouse(
                    &mut app,
                    MouseEventKind::Drag(MouseButton::Left),
                    column,
                    chip.y,
                );
            }
            mouse(
                &mut app,
                MouseEventKind::Up(MouseButton::Left),
                score.x + score.width - 1,
                chip.y,
            );

            assert_eq!(
                app.editor().primary_selection(),
                selection,
                "the score's selection did not follow a gesture that began on the chip"
            );
            assert_eq!(
                app.editor().viewport(),
                viewport,
                "the score did not scroll"
            );
        }
    }
}
