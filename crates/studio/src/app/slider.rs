//! Inline sliders in the score: the pill drawn over each `slider(` call, and
//! the drags, nudges, arrow keys and deletes that move or remove it. A move
//! rewrites the value literal and hands the value to the running score
//! without re-evaluating; the rail's travel and step snapping are here too.

use super::*;

/// Travel changes only the relation between the rail and its numeric value.
/// Source literals and declared steps always stay in the original units.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Travel {
    Linear,
    Logarithmic,
}

impl Travel {
    pub(super) fn frequency(enabled: bool, min: f64, max: f64) -> Self {
        if enabled && min > 0.0 && max > min && min.is_finite() && max.is_finite() {
            Self::Logarithmic
        } else {
            Self::Linear
        }
    }

    fn log_range(min: f64, max: f64) -> f64 {
        let relative = (max - min) / min;
        if relative.is_finite() {
            relative.ln_1p()
        } else {
            max.ln() - min.ln()
        }
    }

    pub(super) fn at(self, ratio: f64, min: f64, max: f64) -> f64 {
        let ratio = ratio.clamp(0.0, 1.0);
        if ratio == 0.0 {
            return min;
        }
        if ratio == 1.0 {
            return max;
        }
        match self {
            Self::Linear => min + ratio * (max - min),
            Self::Logarithmic => {
                let exponent = ratio * Self::log_range(min, max);
                let value = if ((max - min) / min).is_finite() {
                    min + min * exponent.exp_m1()
                } else {
                    (min.ln() + exponent).exp()
                };
                value.clamp(min, max)
            }
        }
    }

    pub(super) fn ratio(self, value: f64, min: f64, max: f64) -> f64 {
        if max <= min {
            return 0.0;
        }
        let value = value.clamp(min, max);
        match self {
            Self::Linear => (value - min) / (max - min),
            Self::Logarithmic => Self::log_range(min, value) / Self::log_range(min, max),
        }
        .clamp(0.0, 1.0)
    }
}

/// Match an HTML range input: steps start at `min`, midpoint ties go up,
/// and the last selectable value is a step that does not exceed `max`.
pub(super) fn snap_value(value: f64, min: f64, max: f64, step: f64) -> f64 {
    let value = if value.is_nan() {
        min
    } else {
        value.clamp(min, max)
    };
    let position = (value - min) / step;
    // An exceptionally small step can overflow the step count. Such a
    // step is below the precision of this range's floating-point values.
    if !position.is_finite() {
        return value;
    }
    let lower = position.floor();
    let fraction = position - lower;
    // Tolerate the few ulps lost in division at decimal midpoints without
    // ever treating an exact integer step as a midpoint for a fine range.
    let roundoff = (f64::EPSILON * position.abs().max(1.0) * 4.0).min(0.125);
    let mut steps = if fraction >= 0.5 || (fraction - 0.5).abs() <= roundoff {
        lower + 1.0
    } else {
        lower
    };
    let decimals = decimal_places(min).max(decimal_places(step));
    let mut snapped = decimal_value(min + steps * step, decimals);
    if snapped > max {
        steps = (steps - 1.0).max(0.0);
        snapped = decimal_value(min + steps * step, decimals);
    }
    snapped.clamp(min, max)
}

/// The number form's writer, kept under the path the app has always
/// called it by.
pub(super) use crate::number_form::format_value;

/// Count decimal places in the shortest representation, including its
/// exponent. A step of .25 needs two places, regardless of its magnitude;
/// the minimum can require more places than the step, as in .005 + n*.1.
fn decimal_places(value: f64) -> usize {
    let scientific = format!("{value:e}");
    let (mantissa, exponent) = scientific.split_once('e').expect("scientific notation");
    let fraction = mantissa
        .split_once('.')
        .map_or(0, |(_, digits)| digits.len());
    let exponent: i32 = exponent.parse().expect("a decimal exponent");
    (fraction as i32 - exponent).max(0) as usize
}

/// Remove arithmetic residue at the declared decimal precision before
/// handing the value to either the editor or the engine. Formatting only
/// the source would make a later evaluation change the sounding value.
fn decimal_value(value: f64, decimals: usize) -> f64 {
    format!("{value:.decimals$}")
        .parse()
        .expect("a formatted floating-point number")
}

impl App {
    /// Place the audible generation's sliders in its own text.
    pub(super) fn rebuild_slider_spans(&mut self) {
        let previous = std::mem::take(&mut self.sliders);
        self.sliders_revision = None;
        let Some(revision) = self.visual_revision else {
            return;
        };
        let Some(layout) = self.visual.layout() else {
            return;
        };
        let Some(editor) = self
            .audible_scene
            .and_then(|id| self.scenes.get(id))
            .map(|scene| &scene.editor)
        else {
            return;
        };
        let mut spans = Vec::with_capacity(layout.sliders.len());
        for slider in &layout.sliders {
            let Some(range) = slider_span_range(editor, revision, slider, &previous) else {
                continue;
            };
            spans.push(SliderSpan {
                id: slider.id.clone(),
                from: range.start,
                to: range.end,
                min: slider.min,
                max: slider.max,
                step: slider.step,
                value: slider.value,
            });
        }
        self.sliders = spans;
        self.sliders_revision = Some(editor.revision());
    }

    /// Carry the slider spans through whatever was edited since they were
    /// last placed. A literal whose text was deleted stops being a control.
    pub(super) fn follow_slider_spans(&mut self) {
        let Some(revision) = self.sliders_revision else {
            return;
        };
        let Some(editor) = self
            .audible_scene
            .and_then(|id| self.scenes.get(id))
            .map(|scene| &scene.editor)
        else {
            self.sliders.clear();
            self.sliders_revision = None;
            return;
        };
        let current = editor.revision();
        if revision == current {
            return;
        }
        let mut kept = Vec::with_capacity(self.sliders.len());
        for span in self.sliders.drain(..) {
            if let Some(range) = editor.map_range_since(revision, span.from..span.to) {
                kept.push(SliderSpan {
                    from: range.start,
                    to: range.end,
                    ..span
                });
            }
        }
        self.sliders = kept;
        self.sliders_revision = Some(current);
    }

    /// Whether the editor's caret sits on a live slider's call, where
    /// Alt+Enter opens the precision control.
    pub(super) fn caret_on_live_slider(&self) -> bool {
        if self.focus != Focus::Editor {
            return false;
        }
        let scene = self.scenes.current().id;
        let caret = self.editor().primary_selection().head.0;
        self.live_chips(scene)
            .into_iter()
            .any(|chip| chip.call.contains(&caret))
    }

    pub(super) fn slider_hint(&self, value: f64, step: f64, travel: Travel) -> String {
        let mouse = if self.features.pixel_mouse {
            "pixels"
        } else {
            "cells"
        };
        let scale = if travel == Travel::Logarithmic {
            " · Log"
        } else {
            ""
        };
        format!(
            "{} · mouse: {mouse}{scale} · {}←/→ {} · Enter expands · Esc lets go",
            format_value(value),
            crate::terminal::symbol("⇧"),
            format_value(step)
        )
    }

    /// Backspace just after a control, or Delete just before it, removes the
    /// whole call. A control is one thing to the hand - one drag, one row of
    /// glyphs - so it is one thing to delete, rather than fifteen characters
    /// to chew through from the end. One undo brings it back whole. With the
    /// caret inside the call, deleting stays character-wise: mid-edit the
    /// numbers are the interface.
    pub(super) fn delete_slider_call(&mut self, command: &Command) -> Result<bool, RuntimeError> {
        let Some(range) = self.slider_delete_range(command) else {
            return Ok(false);
        };
        self.dispatch_editor(Command::ReplaceRange {
            from: range.start,
            to: range.end,
            text: String::new(),
        })?;
        self.status = "slider removed - undo brings it back whole".to_owned();
        Ok(true)
    }

    pub(super) fn slider_delete_range(&self, command: &Command) -> Option<Range<ByteOffset>> {
        let selection = self.editor().primary_selection();
        if !selection.is_empty() {
            return None;
        }
        let caret = selection.head.0;
        let scene = self.scenes.current().id;
        let target = match command {
            // Backspace just after the pill: the widget is what is being
            // deleted, and it goes whole - call, numbers and all; undo
            // brings it back whole. The numbers behind it edit like text.
            Command::DeleteBackward => self
                .live_chips(scene)
                .into_iter()
                .find(|chip| chip.span.from == caret),
            Command::DeleteForward => self
                .live_chips(scene)
                .into_iter()
                .find(|chip| chip.call.start == caret),
            _ => return None,
        };
        target.map(|chip| ByteOffset(chip.call.start)..ByteOffset(chip.call.end))
    }

    /// A scene's live controls, resolved into its current text.
    ///
    /// A range an edit has eaten stops resolving and its control is simply
    /// not in the list - which is the backspace behaviour: the widget is
    /// gone the same frame, not when the next lint pass notices.
    pub(super) fn live_chips(&self, scene_id: SceneId) -> Vec<LiveChip> {
        let Some(set) = self.live_sliders.get(&scene_id) else {
            return Vec::new();
        };
        let Some(scene) = self.scenes.get(scene_id) else {
            return Vec::new();
        };
        let editor = &scene.editor;
        set.sliders
            .iter()
            .filter_map(|live| {
                let call = editor.map_range_since(set.revision, live.call.0..live.call.1)?;
                let value =
                    editor.map_range_since(set.revision, live.slider.from..live.slider.to)?;
                // A drag needs the call to still be one call: the value must
                // not have escaped it.
                (call.start < value.start && value.end <= call.end).then_some(LiveChip {
                    travel: Travel::frequency(
                        live.frequency && self.ui_settings.frequency_slider_log,
                        live.slider.min,
                        live.slider.max,
                    ),
                    frequency: live.frequency,
                    call,
                    span: SliderSpan {
                        id: live.slider.id.clone(),
                        from: value.start,
                        to: value.end,
                        min: live.slider.min,
                        max: live.slider.max,
                        step: live.slider.step,
                        value: live.slider.value,
                    },
                    label: live.label.clone(),
                })
            })
            .collect()
    }

    /// The controls a pane should draw, in its scene's current coordinates.
    ///
    /// Every pane draws its scene's controls, updated or not - a control
    /// exists the moment its call is well formed, exactly as the web
    /// editor's widgets appear as soon as the code does.
    pub(super) fn slider_chip_views(
        &self,
        scene_id: SceneId,
    ) -> Vec<super::super::view::SliderChip> {
        self.live_chips(scene_id)
            .into_iter()
            .map(|chip| {
                let span = &chip.span;
                super::super::view::SliderChip {
                    // The pill covers `slider(` and the room after it, as
                    // the web editor's widget sits before the numbers: the
                    // numbers stay text, read and edited in place, and the
                    // value is written into them as the knob is dragged.
                    from: chip.call.start,
                    to: chip.span.from,
                    notch: chip.travel.ratio(span.value, span.min, span.max),
                    armed: self.armed_slider == Some((scene_id, chip.call.start)),
                }
            })
            .collect()
    }

    /// The room every live control's pill takes on its row, at the current
    /// text's offsets: the cell before the value, widened.
    pub(super) fn slider_pill_widths(
        &self,
        scene_id: SceneId,
    ) -> Vec<super::super::editor::InlineWidth> {
        self.live_chips(scene_id)
            .into_iter()
            .map(|chip| super::super::editor::InlineWidth {
                at: ByteOffset(chip.span.from.saturating_sub(1)),
                extra: SLIDER_PILL_EXTRA,
            })
            .collect()
    }

    /// The live control whose call covers `offset` in a scene's text.
    pub(super) fn live_chip_at(&self, scene_id: SceneId, offset: usize) -> Option<LiveChip> {
        self.live_chips(scene_id)
            .into_iter()
            .find(|chip| chip.call.contains(&offset))
    }

    /// The live control whose drawn cover holds `offset` - the pill over
    /// `slider(`. The value text after it is ordinary text and stays the
    /// editor's.
    fn slider_cover_at(&self, scene_id: SceneId, offset: usize) -> Option<LiveChip> {
        self.live_chips(scene_id)
            .into_iter()
            .find(|chip| offset >= chip.call.start && offset < chip.span.from)
    }

    /// The control under a pointer position, in whichever pane it is over.
    pub(super) fn slider_at(&self, x: u16, y: u16) -> Option<(SceneId, LiveChip)> {
        for pane in &self.panes {
            let Some(map) = pane.last_map.as_ref() else {
                continue;
            };
            let Some(hit) = map.hit_test(super::super::editor::CellPoint::new(x, y)) else {
                continue;
            };
            let super::super::editor::Hit::Text { offset, .. } = hit else {
                continue;
            };
            if let Some(chip) = self.slider_cover_at(pane.scene, offset.0)
                && self.slider_pill_track(pane.scene, &chip).is_some()
            {
                return Some((pane.scene, chip));
            }
        }
        None
    }

    /// The keys an armed slider takes, whatever else is open: ←/→ step it -
    /// a notch, or with ⇧ the control's own step - held keys included,
    /// and Esc lets it go. Anything else is not its business.
    pub(super) fn armed_slider_key(&mut self, key: &crossterm::event::KeyEvent) -> bool {
        let Some((scene, call_from)) = self.armed_slider else {
            return false;
        };
        let primary = key
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::SUPER);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);
        match key.code {
            KeyCode::Enter if !primary && !alt && key.kind == KeyEventKind::Press => {
                self.open_precision_slider(scene, call_from);
                true
            }
            KeyCode::Left | KeyCode::Right if !primary && !alt => {
                let direction = if key.code == KeyCode::Right {
                    1.0
                } else {
                    -1.0
                };
                match self.live_chip_at(scene, call_from) {
                    Some(chip) => self.nudge_live_slider(scene, &chip, direction, shift),
                    None => self.armed_slider = None,
                }
                true
            }
            KeyCode::Esc if key.kind == KeyEventKind::Press => {
                self.armed_slider = None;
                self.status = "slider let go".to_owned();
                self.dirty_frame = true;
                true
            }
            _ => false,
        }
    }

    pub(super) fn slider_at_caret(&self) -> Option<LiveChip> {
        let scene = self.scenes.current().id;
        let caret = self.editor().primary_selection().head.0;
        // The caret rests at the block's edges, so a control it touches -
        // either edge included - is the one it means.
        self.live_chips(scene)
            .into_iter()
            .find(|chip| caret >= chip.call.start && caret <= chip.call.end)
    }

    pub(super) fn nudge_slider_key(&mut self, key: &KeyEvent) -> bool {
        if key.modifiers.contains(KeyModifiers::ALT)
            && !key
                .modifiers
                .intersects(KeyModifiers::CONTROL | KeyModifiers::SUPER)
            && matches!(key.code, KeyCode::Up | KeyCode::Down)
        {
            let direction = if key.code == KeyCode::Up { 1.0 } else { -1.0 };
            return self.nudge_slider_at_caret(direction);
        }
        false
    }

    pub(super) fn nudge_slider_at_caret(&mut self, direction: f64) -> bool {
        let scene = self.scenes.current().id;
        let Some(chip) = self.slider_at_caret() else {
            return false;
        };
        // The caret shortcut follows the declared numeric increment,
        // regardless of the rail's resolution or frequency travel.
        self.nudge_live_slider(scene, &chip, direction, true);
        true
    }

    /// One nudge follows the rail's scale; logarithmic controls move a
    /// hundredth of the travel, with at least one declared step. Fine
    /// nudges always use exactly the declared numeric step.
    pub(super) fn nudge_live_slider(
        &mut self,
        scene: SceneId,
        chip: &LiveChip,
        direction: f64,
        fine: bool,
    ) {
        let value = if fine {
            chip.span.value + direction * chip.span.step
        } else {
            slider_stepped(chip, direction)
        };
        self.set_slider(scene, chip, value);
    }

    /// A press on an inline slider's pill: the knob goes under the pointer,
    /// the slider is armed for the arrows, and the drag that follows keeps
    /// it. Returns true when it took the press.
    pub(super) fn press_slider(&mut self, x: u16, y: u16) -> bool {
        if let Some((scene, chip)) = self.slider_at(x, y)
            && let Some((track_x, width)) = self.slider_pill_track(scene, &chip)
        {
            let track = SliderTrack { x: track_x, width };
            // Capture before rewriting the literal: the edit
            // invalidates the map, including during this batch.
            self.pointer = Some(Pointer::Slider {
                scene,
                call_from: chip.call.start,
                track,
            });
            self.drag_slider(scene, chip.call.start, track, x);
            self.armed_slider = Some((scene, chip.call.start));
            // The pill is the score's: a click on it takes the
            // keyboard back from whatever panel had it, or the
            // panel keeps the arrows and the control never steps.
            self.focus = Focus::Editor;
            #[cfg(feature = "hydra")]
            self.sync_settings_webcam_preview();
            let value = self
                .live_chip_at(scene, chip.call.start)
                .map_or(chip.span.value, |live| live.span.value);
            self.status = self.slider_hint(value, chip.span.step, chip.travel);
            self.dirty_frame = true;
            return true;
        }
        false
    }

    /// One report of a drag: the knob goes under the pointer, and past
    /// either end of the pill the value holds at that end - a drag across
    /// the pill is the whole range, as it is on a fader. Where the terminal
    /// reports the pointer in pixels the knob has every pixel of the pill;
    /// where it reports cells, one place per cell. The rail captured at the
    /// press survives value edits and batches received between frames.
    pub(super) fn drag_slider(
        &mut self,
        scene: SceneId,
        call_from: usize,
        track: SliderTrack,
        x: u16,
    ) {
        let Some(chip) = self.live_chip_at(scene, call_from) else {
            return;
        };
        let cell_width = self
            .features
            .pixel_mouse
            .then_some(self.features.cell_pixels)
            .flatten()
            .map(|(width, _)| width);
        let ratio = track.ratio(x, self.pointer_subcell, cell_width);
        let span = &chip.span;
        let resolution = track.resolution(cell_width);
        let value = pointer_value(
            chip.travel,
            ratio,
            resolution,
            span.min,
            span.max,
            span.step,
        );
        self.set_slider_with_smoothing(scene, &chip, value, self.ui_settings.slider_smoothing);
    }

    /// The pill's track on screen: its first column and its width in
    /// cells, two at least - `None` when the pill is not drawn as one row.
    pub(super) fn slider_pill_track(&self, scene: SceneId, chip: &LiveChip) -> Option<(u16, u16)> {
        let pane = self.panes.iter().find(|pane| pane.scene == scene)?;
        let map = pane.last_map.as_ref()?;
        for row in map.rows() {
            let super::super::editor::ScreenRow::Text(row) = row else {
                continue;
            };
            let Some((start, end)) =
                view::slider_cover_bounds(row, chip.call.start, chip.span.from)
            else {
                continue;
            };
            let span = view::slider_track_span(usize::from(end.saturating_sub(start)))?;
            return Some((start + span.start as u16, span.len() as u16));
        }
        None
    }

    /// Move a slider: rewrite its literal in the score, exactly as the web
    /// editor's inline widget does, and - when the running score has this
    /// control - hand the value to it without re-evaluating anything. A
    /// control the engine has never seen edits the text alone, and starts
    /// sounding on the next update like any other edit.
    pub(super) fn set_slider(&mut self, scene_id: SceneId, chip: &LiveChip, value: f64) {
        self.set_slider_with_smoothing(scene_id, chip, value, false);
    }

    pub(super) fn set_slider_with_smoothing(
        &mut self,
        scene_id: SceneId,
        chip: &LiveChip,
        value: f64,
        smooth: bool,
    ) {
        let value = chip.span.clamp(value);
        if chip.span.value == value {
            // An exact entry at the current target also finishes an audio
            // ramp that has not reached it yet, without another score edit.
            if !smooth && Some(scene_id) == self.audible_scene {
                self.follow_slider_spans();
                if let Some(span) = self
                    .sliders
                    .iter()
                    .find(|span| span.from < chip.span.to && chip.span.from < span.to)
                {
                    self.pending_slider = Some((span.id.clone(), span.clamp(value), false));
                    self.flush_pending_slider();
                }
            }
            return;
        }
        // The audible spans must be expressed at the revision this edit is
        // about to apply on, or the overlap test below compares ranges from
        // two different texts. Following BEFORE the edit matters: afterwards
        // the mapping would collapse the dragged span across its own
        // replacement.
        self.follow_slider_spans();
        let (from, to) = (chip.span.from, chip.span.to);
        let text = format_value(value);
        let moment = self.moment();
        let Some(scene) = self.scenes.get_mut(scene_id) else {
            return;
        };
        let viewport = scene.editor.viewport();
        let before = scene.editor.revision();
        let transaction = Transaction::new(EditOrigin::Control)
            .replace(Edit::new(ByteOffset(from)..ByteOffset(to), text.clone()));
        match scene.editor.apply_transaction(transaction, moment) {
            Ok(()) => {
                // A control edit belongs to the visible rail. Revealing an
                // unrelated text caret would scroll the rail away from the
                // pointer while the gesture still holds its old geometry.
                scene.editor.set_viewport(viewport);
                self.clear_error(ErrorOwner::Editor);
            }
            Err(error) => {
                self.set_error(ErrorOwner::Editor, error.to_string());
                return;
            }
        }
        self.after_edit(scene_id, before, false);
        let Some(editor) = self.scenes.get(scene_id).map(|scene| &scene.editor) else {
            return;
        };
        let after = editor.revision();

        // The replacement collapses both ends of the old value range, so the
        // edited control is placed by hand everywhere it is tracked; its
        // neighbours follow the edit as usual.
        if let Some(set) = self.live_sliders.get_mut(&scene_id) {
            let mut kept = Vec::with_capacity(set.sliders.len());
            for live in set.sliders.drain(..) {
                let Some(call) = editor.map_range_since(set.revision, live.call.0..live.call.1)
                else {
                    continue;
                };
                let edited = call.start == chip.call.start;
                let Some(range) = (if edited {
                    Some(from..from + text.len())
                } else {
                    editor.map_range_since(set.revision, live.slider.from..live.slider.to)
                }) else {
                    continue;
                };
                kept.push(rustel_runtime::ui_events::LiveSlider {
                    frequency: live.frequency,
                    label: live.label.clone(),
                    call: (call.start, call.end),
                    slider: rustel_runtime::ui_events::UiSlider {
                        from: range.start,
                        to: range.end,
                        value: if edited { value } else { live.slider.value },
                        ..live.slider
                    },
                });
            }
            set.sliders = kept;
            set.revision = after;
        }

        // The engine's own copy, when the running score has this control:
        // the audible spans are kept in step and the new value is handed to
        // the pattern.
        let audible_id = (Some(scene_id) == self.audible_scene)
            .then(|| {
                self.sliders
                    .iter()
                    .position(|span| span.from < to && from < span.to)
            })
            .flatten();
        if let Some(index) = audible_id {
            // The id and the evaluated bounds, taken before the rebuild can
            // shift indices by dropping unmappable neighbours.
            let id = self.sliders[index].id.clone();
            // The engine's control keeps its EVALUATED bounds until the next
            // update; a text whose bounds were edited since can name values
            // the engine would refuse, once per mouse-move. Clamp what is
            // sent; the text keeps what was written.
            let engine_value = self.sliders[index].clamp(value);
            let mut kept = Vec::with_capacity(self.sliders.len());
            for (position, span) in self.sliders.drain(..).enumerate() {
                if position == index {
                    kept.push(SliderSpan {
                        from,
                        to: from + text.len(),
                        value,
                        ..span
                    });
                } else if let Some(range) = editor.map_range_since(before, span.from..span.to) {
                    kept.push(SliderSpan {
                        from: range.start,
                        to: range.end,
                        ..span
                    });
                }
            }
            self.sliders = kept;
            self.sliders_revision = Some(after);
            self.pending_slider = Some((id, engine_value, smooth));
            self.slider_tape_due = Some(Instant::now() + TAPE_GESTURE_SETTLE);
            self.flush_pending_slider();
        }
        self.status = if self.armed_slider == Some((scene_id, chip.call.start)) {
            self.slider_hint(value, chip.span.step, chip.travel)
        } else {
            // Named by the call it sits in, where it has one: a score with
            // four faders in it says which one just moved.
            match chip.label.as_deref() {
                Some(label) => format!("{label} = {text}"),
                None => format!("slider = {text}"),
            }
        };
        self.dirty_frame = true;
    }

    pub(super) fn flush_pending_slider(&mut self) {
        if !self.engine_connected || !self.is_playing() {
            self.pending_slider = None;
            return;
        }
        let Some((id, value, smooth)) = self.pending_slider.take() else {
            return;
        };
        let sent = if smooth {
            self.worker.try_set_slider_smoothed(&id, value)
        } else {
            self.worker.try_set_slider(&id, value)
        };
        if !sent {
            self.pending_slider = Some((id, value, smooth));
        }
    }
}

/// A normal `slider(` becomes a twenty-cell track. The numeric arguments
/// remain editable text after it and never change the track's length.
const SLIDER_PILL_EXTRA: u16 = 13;

/// A scene's live controls, pinned to the text revision they were read from.
///
/// This is what the last lint pass saw, which is why a control is on screen
/// the moment its call is well formed and gone the moment a backspace breaks
/// it - no update in between. Ranges are mapped forward from `revision` at
/// the point of use, and a range an edit has eaten simply stops resolving.
pub(super) struct LiveSliderSet {
    pub(super) revision: Revision,
    pub(super) sliders: Vec<rustel_runtime::ui_events::LiveSlider>,
}

/// One live control resolved into a scene's current text: the whole call it
/// is drawn over, and the value literal a gesture edits.
#[derive(Clone, Debug)]
pub(super) struct LiveChip {
    pub(super) travel: Travel,
    pub(super) frequency: bool,
    pub(super) call: std::ops::Range<usize>,
    pub(super) span: SliderSpan,
    /// The call this slider is the first argument of - `lpf` for
    /// `.lpf(slider(800, 100, 4000))`. What the mixer's fader is called.
    pub(super) label: Option<String>,
}

/// A slider literal of the audible score, followed into the coordinates of
/// the text on screen.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct SliderSpan {
    pub(super) id: String,
    pub(super) from: usize,
    pub(super) to: usize,
    pub(super) min: f64,
    pub(super) max: f64,
    pub(super) step: f64,
    /// The value the score's cell currently holds, as far as this side knows.
    pub(super) value: f64,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct SliderTrack {
    pub(super) x: u16,
    pub(super) width: u16,
}

impl SliderTrack {
    /// The distance, as a ratio of the track, between two places the
    /// terminal reports: a cell apart, or a pixel apart where it reports
    /// pixels.
    pub(super) fn resolution(self, cell_width: Option<u16>) -> f64 {
        let cells = f64::from(self.width.saturating_sub(1).max(1));
        let pixels = f64::from(cell_width.filter(|width| *width > 0).unwrap_or(1));
        1.0 / (cells * pixels)
    }

    pub(super) fn ratio(self, x: u16, subcell: f64, cell_width: Option<u16>) -> f64 {
        // The handle fills a character cell. Its centre travels between
        // the centres of the first and last cells, just like a web thumb.
        // Cell-only reports already refer to these discrete positions.
        let centre = cell_width
            .filter(|width| *width > 0)
            .map_or(0.0, |width| f64::from(width - 1) / (2.0 * f64::from(width)));
        (f64::from(x) - f64::from(self.x) + subcell - centre) / f64::from(self.width - 1)
    }
}

impl SliderSpan {
    pub(super) fn clamp(&self, value: f64) -> f64 {
        snap_value(value, self.min, self.max, self.step)
    }
}

/// A slider moved by `notches` along its rail - a notch a hundredth of
/// the travel on a logarithmic control, with at least one declared step,
/// the declared notch on a linear one - as the keys, the wheel and an
/// endless encoder all move it.
pub(super) fn slider_stepped(chip: &LiveChip, notches: f64) -> f64 {
    let span = &chip.span;
    rail_stepped(
        chip.travel,
        span.value,
        span.min,
        span.max,
        span.step,
        notches,
    )
}

/// One wheel notch or coarse keyboard nudge. The declared step for a coarse
/// slider, about a hundredth of the range for a fine one, so a `0.001` step
/// does not take a thousand notches to cross. The notch is a round number
/// of steps - 1, 2 or 5 times a power of ten - so its stops are round
/// numbers too.
pub(super) fn rail_notch(min: f64, max: f64, step: f64) -> f64 {
    let range = max - min;
    if range / step <= 200.0 {
        return step;
    }
    let steps = (range / 100.0 / step).max(1.0);
    let decade = 10f64.powf(steps.log10().floor());
    let distance = |multiple: f64| (multiple / steps).ln().abs();
    let round = [1.0, 2.0, 5.0, 10.0]
        .map(|multiple| multiple * decade)
        .into_iter()
        .min_by(|a, b| distance(*a).total_cmp(&distance(*b)))
        .unwrap_or(1.0);
    round * step
}

/// [`slider_stepped`] for any rail: the knobs column moves its rails the
/// way the score's own sliders move.
pub(super) fn rail_stepped(
    travel: Travel,
    value: f64,
    min: f64,
    max: f64,
    step: f64,
    notches: f64,
) -> f64 {
    if travel == Travel::Logarithmic {
        let ratio = travel.ratio(value, min, max);
        let target = travel.at(ratio + notches / 100.0, min, max);
        // Even a coarse declared step must advance at the low end.
        if notches > 0.0 {
            target.max(value + step)
        } else {
            target.min(value - step)
        }
    } else {
        // A notch moves to the next stop on the notch's own grid, so a
        // value off the grid, such as 16.3, comes back to 16.5 and 17
        // rather than keeping its fraction for ever. The grid counts from
        // 0, so whole numbers are stops, when the rail's own steps pass
        // through 0. Otherwise it counts from the rail's minimum, where
        // the steps start: a stop between two steps would snap back to
        // where the move began.
        let notch = rail_notch(min, max, step);
        let steps_from_zero = min / step;
        let anchor = if (steps_from_zero - steps_from_zero.round()).abs() < 1e-9 {
            0.0
        } else {
            min
        };
        let place = (value - anchor) / notch;
        let stop = place.round();
        let from = if (place - stop).abs() < 1e-6 {
            stop
        } else if notches > 0.0 {
            place.floor()
        } else {
            place.ceil()
        };
        anchor + (from + notches) * notch
    }
}

/// The value a pointer at `ratio` along a rail sets: the rail's value
/// there, on its own steps. `resolution` is the distance, as a ratio,
/// between two places the terminal reports - a cell, or a pixel. A whole
/// number inside the place under the pointer wins when the rail's steps
/// reach it, so a drag can always land back on 16 and never settles on
/// 15.99 beside it. A place finer than one whole number keeps the rail's
/// fractions.
pub(super) fn pointer_value(
    travel: Travel,
    ratio: f64,
    resolution: f64,
    min: f64,
    max: f64,
    step: f64,
) -> f64 {
    let ratio = ratio.clamp(0.0, 1.0);
    let at = travel.at(ratio, min, max);
    let half = resolution / 2.0;
    let low = travel.at((ratio - half).max(0.0), min, max);
    let high = travel.at((ratio + half).min(1.0), min, max);
    let whole = at.round();
    let inside = whole == at || (low < whole && whole < high);
    let target = if inside && snap_value(whole, min, max, step) == whole {
        whole
    } else {
        at
    };
    snap_value(target, min, max, step)
}

/// Where one of the layout's sliders sits in the editor's current text.
///
/// The layout's range is mapped forward from the revision it was made
/// against. Moving a slider replaces its literal outright, and a range whose
/// every byte was replaced maps forward to nothing - so a slider the studio
/// has already placed keeps the span `set_slider` maintains by hand, rather
/// than dying on the very move that proved it works. Nothing to map and
/// nothing remembered is a slider the score no longer has, and stays a drop.
pub(super) fn slider_span_range(
    editor: &Editor,
    revision: Revision,
    slider: &rustel_runtime::ui_events::UiSlider,
    previous: &[SliderSpan],
) -> Option<std::ops::Range<usize>> {
    if let Some(range) = editor.map_range_since(revision, slider.from..slider.to) {
        return Some(range);
    }
    previous
        .iter()
        .find(|span| span.id == slider.id)
        .map(|span| span.from..span.to)
}

#[cfg(test)]
mod tests {
    mod slider {
        //! Tests for `app/slider.rs`.

        use crate::app::slider::*;
        use crate::app::tests::support::*;
        use crate::editor::{MemoryClipboard, Selection};

        /// A slider keeps its place through the one edit that moves it.
        ///
        /// Dragging a chip replaces the value literal, and the engine then echoes
        /// a layout for the same source revision. Mapping the layout's range
        /// through that replacement collapses both of its ends, and the control
        /// must stay live through the gesture.
        #[test]
        fn a_moved_slider_survives_the_layouts_echo() {
            let source = "$: s(\"bd\").gain(slider(0.5, 0, 1, 0.05))\n";
            let mut editor = Editor::new(source).unwrap();
            let revision = editor.revision();
            let layout = rustel_runtime::ui_events::visual_layout(source, 1)
                .unwrap()
                .ui_layout;
            let slider = &layout.sliders[0];
            assert_eq!(&source[slider.from..slider.to], "0.5");

            // Untouched, the layout's own range maps through and no memory is
            // consulted.
            assert_eq!(
                slider_span_range(&editor, revision, slider, &[]),
                Some(slider.from..slider.to)
            );

            // The drag: the literal is replaced outright, exactly as `set_slider`
            // writes it.
            editor
                .dispatch(
                    Command::ReplaceRange {
                        from: ByteOffset(slider.from),
                        to: ByteOffset(slider.to),
                        text: "0.75".into(),
                    },
                    HistoryMoment(0),
                    &mut MemoryClipboard::default(),
                )
                .unwrap();

            // Both ends collapse, so the mapping alone returns nothing.
            assert_eq!(
                editor.map_range_since(revision, slider.from..slider.to),
                None
            );

            // The span the studio maintained by hand carries it across.
            let hand_placed = SliderSpan {
                id: slider.id.clone(),
                from: slider.from,
                to: slider.from + "0.75".len(),
                min: slider.min,
                max: slider.max,
                step: slider.step,
                value: 0.75,
            };
            let survived = slider_span_range(
                &editor,
                revision,
                slider,
                std::slice::from_ref(&hand_placed),
            )
            .expect("the slider is not dropped");
            assert_eq!(survived, hand_placed.from..hand_placed.to);
            assert_eq!(&editor.source()[survived], "0.75");

            // A slider nobody has placed stays a drop: an unknown id is not
            // invented a position.
            assert_eq!(slider_span_range(&editor, revision, slider, &[]), None);
        }

        #[test]
        fn updating_identical_replaced_playing_source_rebinds_live_sliders() {
            let directory = tempfile::tempdir().expect("temp dir");
            let source = "setcpm(90)\n$: s(\"sine\").lpf(slider(2900, 0, 5000, 50)).gain(.06)\n";
            let (mut app, scene, call) = slider_app_with_source(directory.path(), source);
            let original_revision = app.editor().revision();
            let hash = source_revision(source);
            app.evaluation_revisions.remember(
                hash.clone(),
                original_revision,
                source.into(),
                scene,
                None,
            );
            let layout = rustel_runtime::ui_events::visual_layout(source, 1).expect("layout");
            app.handle_control(StudioControlEvent::Layout(layout.clone()));
            app.snapshot = Some(sounding(true, false));
            assert!(app.live_chip_at(scene, call).is_some());
            assert_eq!(app.sliders.len(), 1);

            press(&mut app, KeyCode::Char('a'), KeyModifiers::CONTROL);
            app.handle_terminal_event(Event::Paste(source.into()))
                .expect("replace the score with identical text");
            assert_eq!(app.editor().source(), source);
            let replaced_revision = app.editor().revision();
            assert_ne!(replaced_revision, original_revision);
            // A frame between paste and update retires the old engine-bound span.
            app.follow_slider_spans();
            assert!(app.live_chip_at(scene, call).is_none());
            assert!(app.sliders.is_empty());

            let request = app.next_request_id;
            press(&mut app, KeyCode::Char('s'), KeyModifiers::CONTROL);
            assert!(app.status.starts_with("unchanged"), "{}", app.status);
            assert_eq!(app.next_request_id, request, "playback is not reevaluated");
            assert!(app.pending_evaluation.is_none() && app.inflight.is_empty());
            assert!(app.is_playing());
            assert!(app.live_chip_at(scene, call).is_some());
            assert_eq!(app.sliders.len(), 1, "the audible control is rebound too");
            assert_eq!(app.visual_revision, Some(replaced_revision));
            assert_eq!(
                app.evaluation_revisions.get(&hash).unwrap().revision,
                Some(replaced_revision)
            );

            // A later layout echo must not restore the now-unmappable revision.
            app.handle_control(StudioControlEvent::Layout(layout));
            assert_eq!(app.sliders.len(), 1);
            app.editor_mut()
                .set_selection(Selection::caret(ByteOffset(call)))
                .unwrap();
            press(&mut app, KeyCode::Up, KeyModifiers::ALT);
            assert!(app.editor().source().contains("slider(2950, 0, 5000, 50)"));
            assert_eq!(app.editor().primary_selection().head.0, call);
            assert_eq!(
                app.sliders[0].value, 2950.0,
                "the engine-bound slider moved"
            );
            assert!(
                app.slider_tape_due.is_some(),
                "the audible gesture was sent"
            );
            assert_eq!(app.next_request_id, request);
        }

        #[test]
        fn updating_unchanged_source_preserves_an_identical_queued_scenes_layout_owner() {
            for replay in [false, true] {
                let directory = tempfile::tempdir().expect("temp dir");
                let source = "$: s(\"sine\").gain(slider(0.5, 0, 1, 0.05))\n";
                let mut app = app_over(directory.path());
                press(&mut app, KeyCode::Char('a'), KeyModifiers::CONTROL);
                app.handle_terminal_event(Event::Paste(source.into()))
                    .expect("replace the startup score");
                assert_eq!(app.editor().source(), source);
                let scene = app.scenes.current().id;
                let call = source.find("slider(").expect("slider call");
                let hash = source_revision(source);
                press(&mut app, KeyCode::Char('s'), KeyModifiers::CONTROL);
                assert_eq!(
                    app.evaluation_revisions
                        .get(&hash)
                        .expect("playing source")
                        .scene,
                    scene
                );
                let layout = rustel_runtime::ui_events::visual_layout(source, 1).expect("layout");
                app.handle_control(StudioControlEvent::Layout(layout.clone()));
                app.snapshot = Some(sounding(true, false));
                assert!(app.live_chip_at(scene, call).is_some());

                if replay {
                    let path = write_tape(directory.path(), "same-source", &[(0.0, source)]);
                    app.open_replay_tab(&path);
                } else {
                    app.new_scene(true);
                    app.next_launch = Launch::Quantised { unit_cycles: 4.0 };
                }
                let queued_scene = app.scenes.current().id;
                assert_ne!(queued_scene, scene);
                assert_eq!(
                    app.editor().source(),
                    source,
                    "the queued text is identical"
                );
                let queued_request = app.next_request_id;
                press(&mut app, KeyCode::Char('s'), KeyModifiers::CONTROL);
                assert_eq!(
                    app.next_request_id,
                    queued_request + 1,
                    "a new launch is queued"
                );
                assert!(app.pending_evaluation.is_some() || !app.inflight.is_empty());
                let queued = app.evaluation_revisions.get(&hash).expect("queued source");
                assert_eq!(queued.scene, queued_scene);
                assert_eq!(queued.replay_block.is_some(), replay);
                let armed = (app.armed_scene, app.armed_request);
                if !replay {
                    assert_eq!(armed.0, Some(queued_scene));
                }

                // Return before the queued layout arrives, then replace and update
                // the still-sounding scene. Its controls need fresh coordinates,
                // while the queued launch must remain the owner of the next layout.
                app.select_scene(app.scenes.index_of(scene).expect("playing scene"));
                assert_eq!(app.scenes.current().id, scene);
                press(&mut app, KeyCode::Char('a'), KeyModifiers::CONTROL);
                app.handle_terminal_event(Event::Paste(source.into()))
                    .expect("replace the playing score");
                app.follow_slider_spans();
                assert!(app.live_chip_at(scene, call).is_none());
                let request = app.next_request_id;
                press(&mut app, KeyCode::Char('s'), KeyModifiers::CONTROL);
                assert!(app.status.starts_with("unchanged"), "{}", app.status);
                assert_eq!(app.next_request_id, request);
                assert_eq!((app.armed_scene, app.armed_request), armed);
                assert!(app.live_chip_at(scene, call).is_some());
                assert_eq!(app.visual_revision, Some(app.editor().revision()));

                let retained = app.evaluation_revisions.get(&hash).expect("queued source");
                assert_eq!(retained.scene, queued.scene);
                assert_eq!(retained.revision, queued.revision);
                assert_eq!(retained.replay_block, queued.replay_block);
                app.handle_control(StudioControlEvent::Layout(layout));
                assert_eq!(app.audible_scene, Some(queued_scene));
                assert_eq!(app.visual_revision, queued.revision);
                assert_eq!(app.visual_replay_block, queued.replay_block);
            }
        }

        #[test]
        fn updating_identical_replaced_armed_source_rebinds_sliders_without_rearming() {
            let directory = tempfile::tempdir().expect("temp dir");
            let source = "$: s(\"sine\").gain(slider(0.5, 0, 1, 0.05))\n";
            let (mut app, scene, call) = slider_app_with_source(directory.path(), source);
            let audible_revision = app.visual_revision;
            let hash = source_revision(source);
            app.evaluation_revisions.remember(
                hash.clone(),
                app.editor().revision(),
                source.into(),
                scene,
                None,
            );
            app.snapshot = Some(sounding(true, false));
            app.armed_scene = Some(scene);
            app.armed_request = Some(3);
            app.armed_revision = Some(hash.clone());

            press(&mut app, KeyCode::Char('a'), KeyModifiers::CONTROL);
            app.handle_terminal_event(Event::Paste(source.into()))
                .expect("replace the armed score with identical text");
            let request = app.next_request_id;
            press(&mut app, KeyCode::Char('s'), KeyModifiers::CONTROL);
            assert!(app.status.starts_with("already armed"), "{}", app.status);
            assert_eq!(app.next_request_id, request);
            assert_eq!(app.armed_request, Some(3));
            assert!(app.pending_evaluation.is_none() && app.inflight.is_empty());
            assert!(app.live_chip_at(scene, call).is_some());
            assert_eq!(
                app.visual_revision, audible_revision,
                "the score is still waiting"
            );

            // When its line arrives, the layout uses the replacement's coordinates.
            let layout = rustel_runtime::ui_events::visual_layout(source, 1).expect("layout");
            app.handle_control(StudioControlEvent::Layout(layout));
            assert_eq!(app.visual_revision, Some(app.editor().revision()));
            assert_eq!(app.sliders.len(), 1);
            app.editor_mut()
                .set_selection(Selection::caret(ByteOffset(call)))
                .unwrap();
            press(&mut app, KeyCode::Up, KeyModifiers::ALT);
            assert!(app.editor().source().contains("slider(0.55, 0, 1, 0.05)"));
            assert_eq!(app.editor().primary_selection().head.0, call);
            assert_eq!(app.sliders[0].value, 0.55);
        }

        #[test]
        fn held_option_arrows_keep_nudging_the_slider_without_moving_the_caret() {
            let directory = tempfile::tempdir().unwrap();
            let mut app = app_over(directory.path());
            let source = "// above\n$: s(\"bd\").gain(slider(0.5, 0, 1, 0.05))\n// below\n";
            app.scenes.current_mut().editor = crate::editor::Editor::new(source).unwrap();
            let scene = app.scenes.current().id;
            let call = source.find("slider(").unwrap();
            app.live_sliders.insert(
                scene,
                LiveSliderSet {
                    revision: app.editor().revision(),
                    sliders: rustel_runtime::ui_events::literal_sliders(source),
                },
            );
            app.editor_mut()
                .set_selection(crate::editor::Selection::caret(ByteOffset(call)))
                .unwrap();
            for (code, limit) in [(KeyCode::Up, 1.0), (KeyCode::Down, 0.0)] {
                for step in 0..30 {
                    let kind = if step == 0 {
                        KeyEventKind::Press
                    } else {
                        KeyEventKind::Repeat
                    };
                    app.handle_terminal_event(Event::Key(KeyEvent::new_with_kind(
                        code,
                        KeyModifiers::ALT,
                        kind,
                    )))
                    .unwrap();
                    assert_eq!(app.editor().primary_selection().head.0, call);
                    assert_eq!(app.focus, Focus::Editor);
                }
                assert_eq!(app.slider_at_caret().unwrap().span.value, limit);
                app.handle_terminal_event(Event::Key(KeyEvent::new_with_kind(
                    code,
                    KeyModifiers::ALT,
                    KeyEventKind::Release,
                )))
                .unwrap();
                assert_eq!(app.slider_at_caret().unwrap().span.value, limit);
                assert_eq!(app.editor().primary_selection().head.0, call);
            }
        }

        /// A slider is text until the score is evaluated. A chip drawn over a
        /// call the engine has never seen would look live and move nothing:
        /// the number under the pointer would change and the sound would not.
        #[test]
        fn a_slider_becomes_a_control_only_once_the_score_is_evaluated() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = app_over(directory.path());
            let scene = app.scenes.current().id;
            let source = "$: s(\"bd\").gain(slider(0.5, 0, 1, 0.05))\n";
            app.editor_mut()
                .dispatch(
                    Command::PasteText(source.into()),
                    HistoryMoment(0),
                    &mut MemoryClipboard::default(),
                )
                .expect("seed the score");
            let call = app
                .scenes
                .current()
                .editor
                .source()
                .find("slider(")
                .expect("the call is in the text");

            // The linter has read it and found nothing wrong - and that is
            // all the linter does.
            app.absorb_lint(crate::lint::LintResult {
                scene,
                revision: app.scenes.current().editor.revision(),
                input_channels: None,
                diagnostics: Vec::new(),
            });
            assert!(
                app.live_chip_at(scene, call).is_none(),
                "an unevaluated slider is ordinary text"
            );

            // Evaluating hands the engine the score: now the control is real.
            let revision = app.scenes.current().editor.revision();
            let evaluated: std::sync::Arc<str> = app.scenes.current().editor.source().into();
            app.queue_evaluation(revision, evaluated);
            let chip = app
                .live_chip_at(scene, call)
                .expect("the evaluated slider is a control");
            assert!((chip.span.value - 0.5).abs() < 1e-9);

            // A second slider typed afterwards waits its turn the same way.
            app.editor_mut()
                .dispatch(
                    Command::PasteText("$: s(\"hh\").gain(slider(0.2, 0, 1))\n".into()),
                    HistoryMoment(1),
                    &mut MemoryClipboard::default(),
                )
                .expect("a second control");
            let second = app
                .scenes
                .current()
                .editor
                .source()
                .rfind("slider(")
                .expect("the second call");
            assert!(
                app.live_chip_at(scene, second).is_none(),
                "the new one is text until the next update"
            );
            assert!(
                app.live_chip_at(scene, call).is_some(),
                "the evaluated one keeps its control while the text around it grows"
            );
        }

        /// The lint checks a slider's numbers and creates no controls. A lint
        /// result for an older revision cannot replace the live set that a drag
        /// keeps up to date.
        #[test]
        fn a_stale_lint_result_cannot_flicker_a_dragged_control_away() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = app_over(directory.path());
            let scene = app.scenes.current().id;
            let source = "$: s(\"bd\").gain(slider(0.5, 0, 1, 0.05))\n";
            app.editor_mut()
                .dispatch(
                    Command::PasteText(source.into()),
                    HistoryMoment(0),
                    &mut MemoryClipboard::default(),
                )
                .expect("seed the score");
            let stale_revision = app.scenes.current().editor.revision();
            app.live_sliders.insert(
                scene,
                LiveSliderSet {
                    revision: stale_revision,
                    sliders: rustel_runtime::ui_events::literal_sliders(
                        &app.scenes.current().editor.source(),
                    ),
                },
            );

            // The drag rewrites the literal; the set is hand-updated to the
            // NEW revision by set_slider.
            let call = app
                .scenes
                .current()
                .editor
                .source()
                .find("slider(")
                .unwrap();
            let chip = app.live_chip_at(scene, call).expect("a control");
            app.set_slider(scene, &chip, 0.8);
            let dragged_revision = app.live_sliders[&scene].revision;
            assert!(dragged_revision > stale_revision, "the edit moved on");

            // Now the linter answers for the OLD text. It must be refused.
            app.absorb_lint(crate::lint::LintResult {
                scene,
                revision: stale_revision,
                input_channels: None,
                diagnostics: Vec::new(),
            });
            assert_eq!(
                app.live_sliders[&scene].revision, dragged_revision,
                "the stale result did not land"
            );
            assert!(
                app.live_chip_at(scene, call).is_some(),
                "the control never flickers out"
            );
        }

        /// A control works before any update. A drag rewrites the literal even
        /// when the engine has never evaluated the score, and a second drag
        /// finds the control that the first one moved.
        #[test]
        fn a_live_slider_drags_before_the_score_is_ever_evaluated() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = app_over(directory.path());
            let scene = app.scenes.current().id;
            let source = "$: s(\"bd\").gain(slider(0.5, 0, 1, 0.05))\n";
            app.editor_mut()
                .dispatch(
                    Command::PasteText(source.into()),
                    HistoryMoment(0),
                    &mut MemoryClipboard::default(),
                )
                .expect("seed the score");

            // The lint pass has not run in this test; hand its result over the
            // same way drain_lint would.
            let revision = app.scenes.current().editor.revision();
            app.live_sliders.insert(
                scene,
                LiveSliderSet {
                    revision,
                    sliders: rustel_runtime::ui_events::literal_sliders(
                        &app.scenes.current().editor.source(),
                    ),
                },
            );

            let call = app
                .scenes
                .current()
                .editor
                .source()
                .find("slider(")
                .unwrap();
            let chip = app.live_chip_at(scene, call).expect("a control to drag");
            assert_eq!(chip.span.value, 0.5);

            app.set_slider(scene, &chip, 0.8);
            assert!(
                app.scenes
                    .current()
                    .editor
                    .source()
                    .contains("slider(0.8, 0, 1, 0.05)"),
                "the literal was rewritten: {}",
                app.scenes.current().editor.source()
            );

            // The control survived its own move - the hand-refresh - and a
            // second gesture finds it where it is now.
            let chip = app
                .live_chip_at(scene, call)
                .expect("the control survived its own move");
            assert_eq!(chip.span.value, 0.8);
            app.set_slider(scene, &chip, 0.25);
            assert!(
                app.scenes
                    .current()
                    .editor
                    .source()
                    .contains("slider(0.25, 0, 1, 0.05)"),
                "{}",
                app.scenes.current().editor.source()
            );

            // Nothing was ever handed to an engine that has not got it.
            assert!(app.pending_slider.is_none());

            // And an edit that eats the call takes the control with it, before
            // any lint pass has a chance to say so.
            let source = app.scenes.current().editor.source();
            let from = source.find("slider(").unwrap();
            let to = source.find("))").unwrap() + 2;
            app.editor_mut()
                .dispatch(
                    Command::ReplaceRange {
                        from: ByteOffset(from),
                        to: ByteOffset(to),
                        text: "0.5".into(),
                    },
                    HistoryMoment(1),
                    &mut MemoryClipboard::default(),
                )
                .expect("delete the call");
            assert!(
                app.live_chip_at(scene, from).is_none(),
                "a control whose call is gone is gone"
            );
        }

        /// One Backspace at the pill's right edge takes the whole control;
        /// one undo brings it back. After the numbers, Backspace is just text
        /// editing - the numbers are the interface too.
        #[test]
        fn backspace_after_a_control_removes_the_whole_call() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = app_over(directory.path());
            let scene = app.scenes.current().id;
            app.editor_mut()
                .dispatch(
                    Command::PasteText("$: s(\"bd\").gain(slider(0.5, 0, 1, 0.05))\n".into()),
                    HistoryMoment(0),
                    &mut MemoryClipboard::default(),
                )
                .expect("seed");
            let revision = app.scenes.current().editor.revision();
            app.live_sliders.insert(
                scene,
                LiveSliderSet {
                    revision,
                    sliders: rustel_runtime::ui_events::literal_sliders(
                        &app.scenes.current().editor.source(),
                    ),
                },
            );
            let cover = app
                .scenes
                .current()
                .editor
                .source()
                .find("slider(")
                .unwrap()
                + "slider(".len();
            app.editor_mut()
                .set_selection(Selection::caret(ByteOffset(cover)))
                .expect("caret at the pill's right edge");
            press(&mut app, KeyCode::Backspace, KeyModifiers::NONE);
            let source = app.scenes.current().editor.source();
            assert!(
                !source.contains("slider"),
                "the whole call went in one press: {source}"
            );
            assert!(source.contains(".gain()"), "{source}");
            // And one undo restores it whole.
            press(&mut app, KeyCode::Char('z'), KeyModifiers::CONTROL);
            assert!(
                app.scenes
                    .current()
                    .editor
                    .source()
                    .contains("slider(0.5, 0, 1, 0.05)"),
                "undo brings it back whole"
            );
        }

        /// A notch is a round number of steps, and it moves to the next
        /// stop on its own grid: a value with a fraction comes back to a
        /// whole number, and a whole number comes back to itself.
        #[test]
        fn a_notch_is_round_and_lands_back_on_whole_numbers() {
            assert_eq!(slider::rail_notch(0.0, 250.0, 1.0), 2.0);
            assert_eq!(slider::rail_notch(0.0, 50.0, 0.1), 0.5);
            let step = |value: f64, notches: f64| {
                slider::rail_stepped(Travel::Linear, value, 0.0, 50.0, 0.1, notches)
            };
            assert_eq!(step(16.3, 1.0), 16.5);
            assert_eq!(step(16.5, 1.0), 17.0);
            assert_eq!(step(17.0, -1.0), 16.5);
            assert_eq!(step(16.3, -1.0), 16.0);
            let coarse = |value: f64, notches: f64| {
                slider::rail_stepped(Travel::Linear, value, 0.0, 250.0, 1.0, notches)
            };
            assert_eq!(coarse(coarse(16.0, 1.0), -1.0), 16.0);
            // Steps that start at 0.1 never pass through 0: the notch counts
            // from 0.1 too, so a move left is a move, not a snap back.
            let odd = |value: f64, notches: f64| {
                let moved = slider::rail_stepped(Travel::Linear, value, 0.1, 0.9, 0.2, notches);
                slider::snap_value(moved, 0.1, 0.9, 0.2)
            };
            assert_eq!(odd(0.5, -1.0), 0.3);
            assert_eq!(odd(0.5, 1.0), 0.7);
        }

        /// A pointer lands on a whole number whenever the cell under it
        /// holds one, so a rail of 50 over 23 cells writes no 15.85. A
        /// cell finer than one whole number keeps the rail's fractions.
        #[test]
        fn a_pointer_lands_on_a_whole_number_its_cell_holds() {
            let cell = 1.0 / 22.0;
            let value = |ratio: f64, min: f64, max: f64, step: f64| {
                slider::pointer_value(Travel::Linear, ratio, cell, min, max, step)
            };
            assert_eq!(value(0.317, 0.0, 50.0, 0.1), 16.0);
            for cell in 0..23 {
                let landed = value(f64::from(cell) / 22.0, 0.0, 50.0, 0.1);
                assert_eq!(landed.fract(), 0.0, "cell {cell} landed on {landed}");
            }
            assert_eq!(value(0.5, 0.0, 1.0, 0.01), 0.5);
            // A whole number the slider's steps miss is no stop: the pointer
            // takes the step nearest to it instead.
            assert_eq!(value(0.485, 0.0, 10.0, 0.3), 4.8);
            assert_eq!(value(0.6, 0.0, 10.0, 0.3), 6.0);
            let hertz = slider::pointer_value(Travel::Logarithmic, 0.41, cell, 20.0, 20000.0, 0.01);
            assert_eq!(hertz.fract(), 0.0);
        }

        #[test]
        fn slider_values_are_written_back_with_the_precision_of_their_step() {
            assert_eq!(slider::format_value(0.25), "0.25");
            assert_eq!(slider::format_value(0.5), "0.5");
            assert_eq!(slider::format_value(3.0), "3");
            assert_eq!(slider::format_value(-2.125), "-2.125");
            let span = SliderSpan {
                id: "s".into(),
                from: 0,
                to: 3,
                min: 0.0,
                max: 1.0,
                step: 0.001,
                value: 0.5,
            };
            assert!((slider::rail_notch(span.min, span.max, span.step) - 0.01).abs() < 1e-9);
            assert_eq!(span.clamp(1.7), 1.0);
            assert!((span.clamp(0.2504) - 0.25).abs() < 1e-9);
            let coarse = SliderSpan { step: 0.05, ..span };
            assert!((slider::rail_notch(coarse.min, coarse.max, coarse.step) - 0.05).abs() < 1e-9);
        }

        #[test]
        fn logarithmic_travel_has_equal_ratios_and_exact_endpoints() {
            let log = Travel::frequency(true, 100.0, 10000.0);
            assert_eq!(log, Travel::Logarithmic);
            for (ratio, expected) in [
                (0.0, 100.0),
                (0.25, 316.227766016838),
                (0.5, 1000.0),
                (0.75, 3162.27766016838),
                (1.0, 10000.0),
            ] {
                let value = log.at(ratio, 100.0, 10000.0);
                assert!((value - expected).abs() < 1e-9);
                assert!((log.ratio(value, 100.0, 10000.0) - ratio).abs() < 1e-12);
            }
            assert_eq!(log.at(-0.1, 100.0, 10000.0), 100.0);
            assert_eq!(log.at(1.1, 100.0, 10000.0), 10000.0);
            assert_eq!(Travel::frequency(false, 100.0, 10000.0), Travel::Linear);
            for min in [-100.0, 0.0] {
                assert_eq!(Travel::frequency(true, min, 10000.0), Travel::Linear);
            }
        }

        #[test]
        fn logarithmic_travel_is_finite_monotone_and_invertible_across_ranges() {
            for (min, max) in [
                (20.0, 20000.0),
                (0.001, 1.0),
                (1e-200, 1e200),
                (100.0, 100.0001),
            ] {
                let travel = Travel::frequency(true, min, max);
                let mut previous = min;
                for pixel in 0..=1000 {
                    let ratio = f64::from(pixel) / 1000.0;
                    let value = travel.at(ratio, min, max);
                    assert!(value.is_finite() && value >= previous && value <= max);
                    assert!(
                        (travel.ratio(value, min, max) - ratio).abs() < 1e-8,
                        "{min}..{max} at {ratio}: {value}"
                    );
                    previous = value;
                }
                assert_eq!(travel.at(0.0, min, max), min);
                assert_eq!(travel.at(1.0, min, max), max);
            }
            // Even ranges a handful of representable floats wide stay bounded.
            let min = 100.0;
            let max = 100.00000000000003;
            assert!((min..=max).contains(&Travel::Logarithmic.at(0.5, min, max)));
        }

        #[test]
        fn fractional_steps_and_offsets_keep_all_their_digits() {
            for (input, min, max, step, expected) in [
                (0.23, 0.0, 1.0, 0.25, "0.25"),
                (1.1, 0.0, 5.0, 1.25, "1.25"),
                (0.104, 0.005, 1.005, 0.1, "0.105"),
                (-0.149, -0.255, 0.745, 0.1, "-0.155"),
                (0.300_000_04, 0.0, 1.0, 0.000_000_1, "0.3"),
                (0.000_000_25, 0.0, 1.0, 0.000_000_01, "0.00000025"),
            ] {
                let value = snap_value(input, min, max, step);
                assert_eq!(format_value(value), expected);
                assert_eq!(expected.parse::<f64>().unwrap(), value);
            }
        }

        #[test]
        fn arithmetic_residue_never_changes_the_source_or_engine_value() {
            for (min, max, step) in [
                (0.0, 1.0, 0.1),
                (0.005, 1.005, 0.1),
                (-1.0, 1.0, 0.025),
                (0.0, 1.0, 0.000_000_01),
                (0.0, 1.0, 1e-16),
            ] {
                for pixel in 0..=320 {
                    let input = min + f64::from(pixel) / 320.0 * (max - min);
                    let engine_value = snap_value(input, min, max, step);
                    let source_value: f64 = format_value(engine_value).parse().unwrap();
                    assert_eq!(source_value, engine_value);
                    assert_eq!(snap_value(source_value, min, max, step), engine_value);
                    assert!((min..=max).contains(&engine_value));
                }
            }
            assert_eq!(snap_value(0.7, 0.0, 1.0, 0.1), 0.7);
            assert_eq!(format_value(-0.0), "0");
        }

        #[test]
        fn midpoint_ties_choose_the_higher_valid_step() {
            assert_eq!(snap_value(0.15, 0.0, 1.0, 0.1), 0.2);
            assert_eq!(snap_value(-0.15, -0.3, 0.3, 0.1), -0.1);
            assert_eq!(snap_value(0.124, 0.0, 1.0, 0.25), 0.0);
            assert_eq!(snap_value(0.125, 0.0, 1.0, 0.25), 0.25);
        }

        #[test]
        fn endpoints_respect_steps_even_when_the_range_is_not_divisible() {
            assert_eq!(snap_value(-1.0, 0.0, 1.0, 0.1), 0.0);
            assert_eq!(snap_value(2.0, 0.0, 1.0, 0.1), 1.0);
            assert_eq!(snap_value(1.0, 0.0, 1.0, 0.3), 0.9);
            assert_eq!(snap_value(0.26, 0.0, 0.26, 0.1), 0.2);
            assert_eq!(snap_value(0.3, 0.0, 0.3, 0.1), 0.3);
            assert_eq!(snap_value(1.0, 0.0, 1.0, 2.0), 0.0);
        }

        #[test]
        fn formatting_preserves_values_finer_than_a_typical_slider_step() {
            for value in [0.25, 1.25, 0.105, 1e-12, 3e-100, 1.234_567_890_123_456] {
                assert_eq!(format_value(value).parse::<f64>().unwrap(), value);
            }
        }
    }

    mod slider_interaction {
        //! Tests for slider interaction.

        use crate::app::tests::support::*;
        use crate::app::*;
        use crate::editor::{GridRect, MemoryClipboard, Selection};

        #[test]
        fn a_live_slider_under_the_caret_advertises_its_keyboard_nudge() {
            let directory = tempfile::tempdir().expect("temp dir");
            let (mut app, _, call) = slider_app(directory.path());

            app.editor_mut()
                .set_selection(Selection::caret(ByteOffset(call)))
                .expect("caret on slider");
            assert!(
                app.footer_status().starts_with(
                    crate::keybinds::shortcut_label("Alt+Up/Down changes slider").as_ref()
                ),
                "the contextual key is the first thing in the footer: {}",
                app.footer_status()
            );
            let before = app.editor().source();
            precision_key(&mut app, KeyCode::Up, KeyModifiers::ALT);
            assert_ne!(app.editor().source(), before, "Alt+Up moves the slider");
            assert!(
                app.footer_status().starts_with(
                    crate::keybinds::shortcut_label("Alt+Up/Down changes slider").as_ref()
                ),
                "the hint remains while the caret still touches the slider"
            );

            app.editor_mut()
                .set_selection(Selection::caret(ByteOffset(0)))
                .expect("caret off slider");
            assert!(
                !app.footer_status()
                    .contains(crate::keybinds::shortcut_label("Alt+Up/Down").as_ref()),
                "the hint leaves with the slider"
            );
        }

        #[test]
        fn option_arrows_use_one_declared_step_for_presses_and_repeats() {
            for (source, initial, first, second) in [
                (SOURCE, 0.5, 0.5001, 0.5002),
                (
                    "$: s(\"bd\").lpf(slider(1000, 100, 10000, .25))\n",
                    1000.0,
                    1000.25,
                    1000.5,
                ),
                (
                    "$: s(\"bd\").gain(slider(.405, .005, 1.005, .1))\n",
                    0.405,
                    0.505,
                    0.605,
                ),
            ] {
                let directory = tempfile::tempdir().unwrap();
                let (mut app, scene, call) = slider_app_with_source(directory.path(), source);
                app.editor_mut()
                    .set_selection(Selection::caret(ByteOffset(call)))
                    .unwrap();
                for (code, kind, expected) in [
                    (KeyCode::Up, KeyEventKind::Press, first),
                    (KeyCode::Up, KeyEventKind::Repeat, second),
                    (KeyCode::Up, KeyEventKind::Release, second),
                    (KeyCode::Down, KeyEventKind::Press, first),
                    (KeyCode::Down, KeyEventKind::Repeat, initial),
                    (KeyCode::Down, KeyEventKind::Release, initial),
                ] {
                    app.handle_terminal_event(Event::Key(KeyEvent::new_with_kind(
                        code,
                        KeyModifiers::ALT,
                        kind,
                    )))
                    .unwrap();
                    assert_eq!(
                        value(&app, scene, call),
                        expected,
                        "{source}: {code:?} {kind:?}"
                    );
                    assert_eq!(app.editor().primary_selection().head.0, call);
                    assert_eq!(app.focus, Focus::Editor);
                }
            }
        }

        #[test]
        fn a_batched_gesture_tracks_absolute_cells_and_uses_the_release_position() {
            let directory = tempfile::tempdir().expect("temp dir");
            let (mut app, scene, call) = slider_app(directory.path());
            let (x, width, row) = lay_out(&mut app, scene, call);
            assert!(
                width >= 20,
                "the rail provides more cell positions: {width}"
            );

            mouse(&mut app, MouseEventKind::Down(MouseButton::Left), x, row);
            assert_eq!(value(&app, scene, call), 0.0);
            assert!(
                app.panes[0].last_map.is_none(),
                "the first edit invalidates layout"
            );

            // No redraw between reports: every position still updates, even when
            // replacing 0 with a longer decimal changes source offsets.
            let mut previous = 0.0;
            for offset in 1..width - 1 {
                mouse(
                    &mut app,
                    MouseEventKind::Drag(MouseButton::Left),
                    x + offset,
                    row,
                );
                let current = value(&app, scene, call);
                assert!(current > previous, "each cell advances the value");
                assert_value(&app, scene, call, f64::from(offset) / f64::from(width - 1));
                previous = current;
            }

            // Leaving the rail clamps immediately; returning to an earlier cell
            // means the same value, with no accumulated drag distance to unwind.
            mouse(&mut app, MouseEventKind::Drag(MouseButton::Left), 119, row);
            assert_eq!(value(&app, scene, call), 1.0);
            mouse(
                &mut app,
                MouseEventKind::Drag(MouseButton::Left),
                x + 3,
                row,
            );
            assert_value(&app, scene, call, 3.0 / f64::from(width - 1));
            mouse(&mut app, MouseEventKind::Drag(MouseButton::Left), 0, row);
            assert_eq!(value(&app, scene, call), 0.0);
            mouse(
                &mut app,
                MouseEventKind::Drag(MouseButton::Left),
                x + 3,
                row,
            );
            assert_value(&app, scene, call, 3.0 / f64::from(width - 1));

            mouse(
                &mut app,
                MouseEventKind::Up(MouseButton::Left),
                x + width - 1,
                row,
            );
            assert_eq!(
                value(&app, scene, call),
                1.0,
                "release applies its own position"
            );
            assert!(app.pointer.is_none(), "release ends the capture");
            mouse(&mut app, MouseEventKind::Moved, x, row);
            assert_eq!(
                value(&app, scene, call),
                1.0,
                "hover after release cannot drag"
            );
        }

        #[test]
        fn a_slider_drag_keeps_the_visible_track_when_the_text_caret_is_offscreen() {
            let directory = tempfile::tempdir().expect("temp dir");
            let (mut app, scene, call) = slider_app(directory.path());
            let area = app.regions.panes[0].editor;
            let grid = GridRect::new(area.x + 4, area.y, 50, 4);
            map_on(&mut app, scene, grid, false, 0);
            let chip = app.live_chip_at(scene, call).expect("live slider");
            let initial_track = app.slider_pill_track(scene, &chip).expect("visible rail");
            let row = app.panes[0]
                .last_map
                .as_ref()
                .unwrap()
                .cell_for_offset(ByteOffset(call))
                .expect("visible call")
                .y;

            // Scrolling can leave the text caret beyond the visible columns.
            // A slider press does not move that caret into its own numeric literal.
            let caret = app.editor().source().find('\n').expect("first line");
            app.editor_mut()
                .set_selection(Selection::caret(ByteOffset(caret)))
                .expect("place the text caret beyond the visible rail");
            let viewport_before = app.editor().viewport();
            mouse(
                &mut app,
                MouseEventKind::Down(MouseButton::Left),
                initial_track.0 + 3,
                row,
            );
            assert!(app.panes[0].last_map.is_none());

            // Rebuild exactly the editor map draw() uses. Unlike map_on(), a
            // redraw must retain the real selection and the current viewport.
            let widths = app.slider_pill_widths(scene);
            let editor = app.editor_mut();
            editor.set_inline_widths(widths);
            editor.set_wrap(false);
            editor.set_view_size(usize::from(grid.width), usize::from(grid.height));
            let map = editor.screen_map(grid).expect("redrawn map");
            app.panes[0].last_map = Some(map);
            let chip = app
                .live_chip_at(scene, call)
                .expect("live slider after edit");
            assert_eq!(
                app.slider_pill_track(scene, &chip),
                Some(initial_track),
                "redraw must keep the rail under its captured pointer; viewport {:?} became {:?}",
                viewport_before,
                app.editor().viewport()
            );
            assert_eq!(
                app.editor().primary_selection().head.0,
                app.editor().source().find('\n').unwrap(),
                "the text caret still follows its original source position"
            );
            mouse(
                &mut app,
                MouseEventKind::Up(MouseButton::Left),
                initial_track.0 + initial_track.1 - 1,
                row,
            );
            assert_eq!(value(&app, scene, call), 1.0);
        }

        #[test]
        fn a_slider_drag_keeps_its_row_when_the_text_caret_is_above_the_viewport() {
            let directory = tempfile::tempdir().expect("temp dir");
            let (mut app, scene, _) = slider_app(directory.path());
            let source = format!(
                "{}{SOURCE}{}",
                "// spacer\n".repeat(50),
                "// tail\n".repeat(30)
            );
            let call = source.find("slider(").expect("slider on line fifty");
            for command in [Command::SelectAll, Command::PasteText(source)] {
                app.editor_mut()
                    .dispatch(command, HistoryMoment(1), &mut MemoryClipboard::default())
                    .expect("seed the long score");
            }
            app.live_sliders.insert(
                scene,
                LiveSliderSet {
                    revision: app.editor().revision(),
                    sliders: rustel_runtime::ui_events::literal_sliders(&app.editor().source()),
                },
            );
            let area = app.regions.panes[0].editor;
            let grid = GridRect::new(area.x + 4, area.y, 80, 15);
            map_on(&mut app, scene, grid, false, 0);
            let mut viewport = app.editor().viewport();
            viewport.top_row = 40;
            app.editor_mut().set_viewport(viewport);

            // Drawing keeps the real selection and scroll position. The caret
            // remains on the first line while the pointer works on line fifty.
            let redraw = |app: &mut App| {
                let widths = app.slider_pill_widths(scene);
                let editor = app.editor_mut();
                editor.set_inline_widths(widths);
                editor.set_wrap(false);
                editor.set_view_size(usize::from(grid.width), usize::from(grid.height));
                let map = editor.screen_map(grid).expect("redrawn map");
                app.panes[0].last_map = Some(map);
            };
            redraw(&mut app);
            let viewport_before = app.editor().viewport();
            assert_eq!(viewport_before.top_row, 40);
            let chip = app.live_chip_at(scene, call).expect("live slider");
            let initial_track = app.slider_pill_track(scene, &chip).expect("visible rail");
            let row = app.panes[0]
                .last_map
                .as_ref()
                .unwrap()
                .cell_for_offset(ByteOffset(call))
                .expect("visible slider on line fifty")
                .y;
            assert_eq!(row, grid.y + 10);

            for (kind, offset) in [
                (MouseEventKind::Down(MouseButton::Left), 0),
                (MouseEventKind::Drag(MouseButton::Left), 3),
                (MouseEventKind::Up(MouseButton::Left), initial_track.1 - 1),
            ] {
                mouse(&mut app, kind, initial_track.0 + offset, row);
                assert_value(
                    &app,
                    scene,
                    call,
                    f64::from(offset) / f64::from(initial_track.1 - 1),
                );
                redraw(&mut app);
                assert_eq!(
                    app.editor().viewport(),
                    viewport_before,
                    "slider edits must not reveal the first-line caret"
                );
                assert_eq!(app.editor().primary_selection().head, ByteOffset(0));
                let chip = app
                    .live_chip_at(scene, call)
                    .expect("live slider after edit");
                assert_eq!(app.slider_pill_track(scene, &chip), Some(initial_track));
                assert_eq!(
                    app.panes[0]
                        .last_map
                        .as_ref()
                        .unwrap()
                        .cell_for_offset(ByteOffset(call))
                        .expect("slider remains visible")
                        .y,
                    row,
                    "the rail stays under the pointer after every redraw"
                );
            }
            assert_eq!(value(&app, scene, call), 1.0);
            assert!(
                app.pointer.is_none(),
                "release ends the drag at its endpoint"
            );
        }

        #[test]
        fn a_slider_drag_keeps_the_visible_track_when_its_value_changes_wrapping() {
            let directory = tempfile::tempdir().expect("temp dir");
            let (mut app, scene, call) = slider_app(directory.path());
            let area = app.regions.panes[0].editor;
            // The initial value and its comma fit at the right edge. A longer
            // numeric value overflows before that comma supplies a wrap point.
            // Wrapping reserves one column for the caret, so 41 gives 40 text columns.
            let grid = GridRect::new(area.x + 4, area.y, 41, 4);
            map_on(&mut app, scene, grid, true, 0);
            let chip = app.live_chip_at(scene, call).expect("live slider");
            let initial_track = app.slider_pill_track(scene, &chip).expect("visible rail");
            let initial_row = app.panes[0]
                .last_map
                .as_ref()
                .unwrap()
                .cell_for_offset(ByteOffset(call))
                .expect("visible call")
                .y;
            let viewport_before = app.editor().viewport();
            mouse(
                &mut app,
                MouseEventKind::Down(MouseButton::Left),
                initial_track.0 + 3,
                initial_row,
            );
            assert_value(&app, scene, call, 3.0 / f64::from(initial_track.1 - 1));

            let widths = app.slider_pill_widths(scene);
            let editor = app.editor_mut();
            editor.set_inline_widths(widths);
            editor.set_wrap(true);
            editor.set_view_size(usize::from(grid.width), usize::from(grid.height));
            let map = editor.screen_map(grid).expect("redrawn map");
            app.panes[0].last_map = Some(map);
            let chip = app
                .live_chip_at(scene, call)
                .expect("live slider after edit");
            let redrawn_track = app.slider_pill_track(scene, &chip).expect("redrawn rail");
            let redrawn_row = app.panes[0]
                .last_map
                .as_ref()
                .unwrap()
                .cell_for_offset(ByteOffset(call))
                .expect("redrawn call")
                .y;
            assert_eq!(app.editor().viewport(), viewport_before);
            assert_eq!(app.editor().primary_selection().head, ByteOffset(0));
            assert_eq!(
                (redrawn_track, redrawn_row),
                (initial_track, initial_row),
                "changing only the slider value must not wrap its rail away from the pointer"
            );
            mouse(
                &mut app,
                MouseEventKind::Up(MouseButton::Left),
                initial_track.0 + initial_track.1 - 1,
                initial_row,
            );
            assert_eq!(value(&app, scene, call), 1.0);
        }

        #[test]
        fn pixel_reports_keep_subcell_precision_and_align_with_the_drawn_rail() {
            let directory = tempfile::tempdir().expect("temp dir");
            let (mut app, scene, call) = slider_app(directory.path());
            let (x, width, row) = lay_out(&mut app, scene, call);
            app.features.pixel_mouse = true;
            app.features.cell_pixels = Some((9, 18));
            // Crossterm supplies zero-based pixel indices: pixel 4 is the center
            // of a nine-pixel cell. The first and last handle centers are bounds.
            let first = x * 9 + 4;
            let last = (x + width - 1) * 9 + 4;
            let pixel_row = row * 18 + 8;
            mouse(
                &mut app,
                MouseEventKind::Down(MouseButton::Left),
                first,
                pixel_row,
            );
            assert_eq!(value(&app, scene, call), 0.0);

            let mut previous = 0.0;
            let mut near_middle = Vec::new();
            for pixel in first + 1..last {
                mouse(
                    &mut app,
                    MouseEventKind::Drag(MouseButton::Left),
                    pixel,
                    pixel_row,
                );
                let current = value(&app, scene, call);
                assert!(
                    current > previous,
                    "one pixel changes the value, even within a cell"
                );
                assert_value(
                    &app,
                    scene,
                    call,
                    f64::from(pixel - first) / f64::from(last - first),
                );
                if pixel == (first + last) / 2 || pixel == (first + last).div_ceil(2) {
                    near_middle.push(current);
                }
                previous = current;
            }
            assert_eq!(
                near_middle.len(),
                2,
                "an even-width rail straddles its center"
            );
            assert!(near_middle[0] < 0.5 && near_middle[1] > 0.5);
            assert!((near_middle[0] + near_middle[1] - 1.0).abs() < 0.000_1);
            mouse(
                &mut app,
                MouseEventKind::Up(MouseButton::Left),
                last,
                pixel_row,
            );
            assert_eq!(value(&app, scene, call), 1.0);
        }

        #[test]
        fn cell_and_pixel_drags_snap_to_declared_steps_from_the_minimum() {
            let steps = [
                0.005, 0.105, 0.205, 0.305, 0.405, 0.505, 0.605, 0.705, 0.805, 0.905,
            ];
            for expanded in [false, true] {
                for pixels in [false, true] {
                    let directory = tempfile::tempdir().unwrap();
                    let (mut app, scene, call) = slider_app_with_source(
                        directory.path(),
                        "$: s(\"bd\").gain(slider(.505, .005, .955, .1))\n",
                    );
                    let (x, width, row) = if expanded {
                        app.open_precision_slider(scene, call);
                        let area = slider_precision::geometry(app.frame);
                        (area.x + 2, area.width - 4, area.y + 3)
                    } else {
                        lay_out(&mut app, scene, call)
                    };
                    app.features.pixel_mouse = pixels;
                    app.features.cell_pixels = Some((9, 18));
                    let (first, last, row) = if pixels {
                        (x * 9 + 4, (x + width - 1) * 9 + 4, row * 18 + 8)
                    } else {
                        (x, x + width - 1, row)
                    };
                    let mut visited = Vec::new();
                    for column in first..=last {
                        let kind = if column == first {
                            MouseEventKind::Down(MouseButton::Left)
                        } else if column == last {
                            MouseEventKind::Up(MouseButton::Left)
                        } else {
                            MouseEventKind::Drag(MouseButton::Left)
                        };
                        mouse(&mut app, kind, column, row);
                        let current = value(&app, scene, call);
                        assert!(steps.contains(&current), "off-step mouse value: {current}");
                        if visited.last() != Some(&current) {
                            visited.push(current);
                        }
                    }
                    assert_eq!(visited, steps, "expanded={expanded}, pixels={pixels}");
                    assert_eq!(value(&app, scene, call), 0.905, "max lies between steps");
                }
            }
        }

        #[test]
        fn the_reported_large_range_uses_cell_or_pixel_positions_and_exact_shift_steps() {
            let directory = tempfile::tempdir().expect("temp dir");
            let (mut app, scene, call) = slider_app_with_source(
                directory.path(),
                "$: s(\"bd\").lpf(slider(100, 100, 10000, .01))\n",
            );
            app.ui_settings.frequency_slider_log = false;
            let (x, width, row) = lay_out(&mut app, scene, call);
            assert_eq!(width, 20);
            app.features.cell_pixels = Some((10, 20));

            // The reported Windows Terminal session supplies cells despite its
            // known cell dimensions. Nineteen intervals span the entire range.
            mouse(
                &mut app,
                MouseEventKind::Down(MouseButton::Left),
                x + 2,
                row,
            );
            // A cell holds hundreds of hertz here, so it lands on a whole one.
            assert_eq!(value(&app, scene, call), 1142.0);
            assert!(app.status.starts_with("1142 · mouse: cells"));
            mouse(
                &mut app,
                MouseEventKind::Drag(MouseButton::Left),
                x + 3,
                row,
            );
            assert_eq!(value(&app, scene, call), 1663.0);
            assert!(app.status.starts_with("1663 · mouse: cells"));
            mouse(&mut app, MouseEventKind::Up(MouseButton::Left), x + 3, row);

            // A terminal supplying pixels provides distinct positions within
            // that same character. A ten-pixel cell has its center at index 4.5.
            // A pixel still spans about 52 Hz, so each lands on a whole one.
            let (x, _, row) = lay_out(&mut app, scene, call);
            app.features.pixel_mouse = true;
            let pixel_cell_start = (x + 3) * 10;
            let pixel_row = row * 20 + 9;
            let mut previous = None;
            for (offset, expected) in [
                (2, 1533.0),
                (3, 1585.0),
                (4, 1637.0),
                (5, 1689.0),
                (6, 1741.0),
            ] {
                let kind = if previous.is_none() {
                    MouseEventKind::Down(MouseButton::Left)
                } else {
                    MouseEventKind::Drag(MouseButton::Left)
                };
                let pixel = pixel_cell_start + offset;
                assert_eq!(pixel / 10, x + 3, "every report is in the same cell");
                mouse(&mut app, kind, pixel, pixel_row);
                let current = value(&app, scene, call);
                assert_eq!(current, expected);
                assert!(app.status.starts_with(&format!(
                    "{} · mouse: pixels",
                    slider::format_value(expected)
                )));
                if let Some(previous) = previous {
                    assert!(
                        current > previous,
                        "each additional pixel advances the value"
                    );
                }
                previous = Some(current);
            }
            mouse(
                &mut app,
                MouseEventKind::Up(MouseButton::Left),
                pixel_cell_start + 6,
                pixel_row,
            );

            assert_eq!(app.armed_slider, Some((scene, call)));
            for (key, expected) in [(KeyCode::Right, 1741.01), (KeyCode::Left, 1741.0)] {
                app.handle_terminal_event(Event::Key(KeyEvent::new(key, KeyModifiers::SHIFT)))
                    .expect("fine adjustment handled");
                assert_eq!(
                    value(&app, scene, call),
                    expected,
                    "Shift+arrow moves exactly one .01 step"
                );
                assert!(app.status.starts_with(&slider::format_value(expected)));
                assert!(app.status.contains("⇧←/→ 0.01"));
            }
        }

        #[test]
        fn a_full_rail_at_the_right_edge_does_not_need_the_value_on_screen() {
            let directory = tempfile::tempdir().expect("temp dir");
            let (mut app, scene, call) = slider_app(directory.path());
            let (_, width, _) = lay_out(&mut app, scene, call);
            let area = app.regions.panes[0].editor;
            let grid = GridRect::new(area.x + 4, area.y, call as u16 + width, 4);
            map_on(&mut app, scene, grid, false, 0);
            let chip = app.live_chip_at(scene, call).expect("live slider");
            let map = app.panes[0].last_map.as_ref().unwrap();
            assert!(map.cell_for_offset(ByteOffset(chip.span.from)).is_none());
            assert_eq!(
                app.slider_pill_track(scene, &chip),
                Some((grid.x + call as u16, width))
            );

            mouse(
                &mut app,
                MouseEventKind::Down(MouseButton::Left),
                grid.x + grid.width - 1,
                grid.y,
            );
            assert_eq!(
                value(&app, scene, call),
                1.0,
                "the expanded '(' supplies the full rail"
            );
            assert!(
                app.status.starts_with("1 · mouse: cells"),
                "the footer must show the value when its source literal is offscreen"
            );
            mouse(
                &mut app,
                MouseEventKind::Up(MouseButton::Left),
                grid.x + call as u16,
                grid.y,
            );
            assert_eq!(value(&app, scene, call), 0.0);
        }

        #[test]
        fn wrapped_or_clipped_covers_do_not_capture_a_slider_drag() {
            let directory = tempfile::tempdir().expect("temp dir");
            let (mut app, scene, call) = slider_app(directory.path());
            let (_, width, _) = lay_out(&mut app, scene, call);
            let area = app.regions.panes[0].editor;
            let grid = GridRect::new(area.x + 4, area.y, call as u16 + width - 1, 4);
            for (wrap, left) in [(false, 0), (true, 0), (false, call + 1)] {
                // A row wide enough for the whole rail can wrap it intact. To
                // exercise a split cover, the wrapped row must be narrower.
                let grid = if wrap {
                    GridRect {
                        width: width - 1,
                        ..grid
                    }
                } else {
                    grid
                };
                map_on(&mut app, scene, grid, wrap, left);
                let chip = app.live_chip_at(scene, call).expect("live slider");
                assert_eq!(
                    app.slider_pill_track(scene, &chip),
                    None,
                    "wrap={wrap}, left={left}"
                );
                let (x, row) = app.panes[0]
                    .last_map
                    .as_ref()
                    .unwrap()
                    .rows()
                    .iter()
                    .find_map(|row| {
                        let crate::editor::ScreenRow::Text(row) = row else {
                            return None;
                        };
                        row.cells
                            .iter()
                            .find(|cell| {
                                cell.bytes.start.0 >= chip.call.start
                                    && cell.bytes.end.0 <= chip.span.from
                            })
                            .map(|cell| (cell.screen_x.start, row.screen_y))
                    })
                    .expect("some of the cover remains visible");
                mouse(&mut app, MouseEventKind::Down(MouseButton::Left), x, row);
                assert!(!matches!(app.pointer, Some(Pointer::Slider { .. })));
                assert_eq!(value(&app, scene, call), 0.5);
                mouse(&mut app, MouseEventKind::Up(MouseButton::Left), x, row);
            }
        }

        #[test]
        fn a_cover_wrapped_intact_onto_the_next_row_keeps_its_full_track() {
            let directory = tempfile::tempdir().expect("temp dir");
            let (mut app, scene, call) = slider_app(directory.path());
            let (_, width, _) = lay_out(&mut app, scene, call);
            let area = app.regions.panes[0].editor;
            let grid = GridRect::new(area.x + 4, area.y, call as u16 + width - 1, 4);
            map_on(&mut app, scene, grid, true, 0);
            let chip = app.live_chip_at(scene, call).expect("live slider");
            let (x, wrapped_width) = app
                .slider_pill_track(scene, &chip)
                .expect("intact wrapped rail");
            assert_eq!(wrapped_width, width);
            let row = app.panes[0]
                .last_map
                .as_ref()
                .unwrap()
                .cell_for_offset(ByteOffset(call))
                .expect("wrapped call")
                .y;
            assert!(row > grid.y, "the rail moved onto the next row");
            mouse(&mut app, MouseEventKind::Down(MouseButton::Left), x, row);
            assert_eq!(value(&app, scene, call), 0.0);
            mouse(
                &mut app,
                MouseEventKind::Up(MouseButton::Left),
                x + width - 1,
                row,
            );
            assert_eq!(value(&app, scene, call), 1.0);
        }

        #[test]
        fn the_first_press_in_another_pane_already_drags_its_slider() {
            let directory = tempfile::tempdir().expect("temp dir");
            let (mut app, scene, call) = slider_app(directory.path());
            app.toggle_split();
            app.panes.swap(0, 1);
            app.focus_pane(0);
            app.regions = view::regions(app.frame, false, 2, false, false, None, None);
            assert_ne!(app.scenes.current().id, scene);
            let (x, width, row) = lay_out(&mut app, scene, call);

            mouse(&mut app, MouseEventKind::Down(MouseButton::Left), x, row);
            assert_eq!(app.focused, 1);
            assert_eq!(app.scenes.current().id, scene);
            assert!(matches!(app.pointer, Some(Pointer::Slider { .. })));
            assert_eq!(value(&app, scene, call), 0.0);
            mouse(
                &mut app,
                MouseEventKind::Up(MouseButton::Left),
                x + width - 1,
                row,
            );
            assert_eq!(value(&app, scene, call), 1.0);
        }

        #[test]
        fn expanded_slider_has_more_cell_positions_and_keeps_captured_rail() {
            let directory = tempfile::tempdir().unwrap();
            let (mut app, scene, call) = slider_app(directory.path());
            app.armed_slider = Some((scene, call));
            precision_key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
            assert!(app.slider_precision.is_some());
            let area = slider_precision::geometry(app.frame);
            let left = area.x + 2;
            let right = area.right() - 3;
            let row = area.y + 3;
            assert!(right - left > 20);
            mouse(&mut app, MouseEventKind::Down(MouseButton::Left), left, row);
            assert_value(&app, scene, call, 0.0);
            mouse(
                &mut app,
                MouseEventKind::Drag(MouseButton::Left),
                left + 1,
                row,
            );
            let first = value(&app, scene, call);
            assert!(first > 0.0 && first < 0.02);
            mouse(&mut app, MouseEventKind::Up(MouseButton::Left), right, row);
            assert_value(&app, scene, call, 1.0);
        }

        #[test]
        fn expanded_slider_exact_entry_snaps_and_cancel_does_not_edit_source() {
            let directory = tempfile::tempdir().unwrap();
            let (mut app, scene, call) = slider_app(directory.path());
            app.open_precision_slider(scene, call);
            app.handle_terminal_event(Event::Paste("0.123456".into()))
                .unwrap();
            assert_value(&app, scene, call, 0.5);
            assert!(!app.editor_accepts_raw_paste());
            precision_key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
            assert_value(&app, scene, call, 0.1235);
            let committed = app.editor().source();
            app.handle_terminal_event(Event::Paste("1e999".into()))
                .unwrap();
            precision_key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
            assert_eq!(app.editor().source(), committed);
            precision_key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
            assert!(app.slider_precision.is_some(), "first Esc cancels entry");
            precision_key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
            assert!(app.slider_precision.is_none());
            assert_eq!(app.editor().source(), committed);
        }

        #[test]
        fn expanded_slider_shift_keys_and_paste_stay_out_of_the_score() {
            let directory = tempfile::tempdir().unwrap();
            let (mut app, scene, call) = slider_app(directory.path());
            app.open_precision_slider(scene, call);
            precision_key(&mut app, KeyCode::Right, KeyModifiers::SHIFT);
            assert_value(&app, scene, call, 0.5001);
            let before = app.editor().source();
            app.handle_terminal_event(Event::Paste("s(\"do not paste into the score\")".into()))
                .unwrap();
            assert_eq!(app.editor().source(), before);
            precision_key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
            precision_key(&mut app, KeyCode::Char('s'), KeyModifiers::NONE);
            assert!(app.ui_settings.slider_smoothing);
            assert_eq!(app.editor().source(), before);
        }

        #[test]
        fn resizing_expanded_slider_cancels_the_old_drag_capture() {
            let directory = tempfile::tempdir().unwrap();
            let (mut app, scene, call) = slider_app(directory.path());
            app.open_precision_slider(scene, call);
            let area = slider_precision::geometry(app.frame);
            mouse(
                &mut app,
                MouseEventKind::Down(MouseButton::Left),
                area.x + 2,
                area.y + 3,
            );
            assert_value(&app, scene, call, 0.0);
            app.handle_terminal_event(Event::Resize(80, 24)).unwrap();
            mouse(
                &mut app,
                MouseEventKind::Drag(MouseButton::Left),
                area.right(),
                area.y + 3,
            );
            assert_value(&app, scene, call, 0.0);
        }

        #[test]
        fn expanded_slider_renders_cleanly_on_small_and_regular_terminals() {
            use ratatui::{Terminal, backend::TestBackend};
            let directory = tempfile::tempdir().unwrap();
            let (mut app, scene, call) = slider_app(directory.path());
            app.open_precision_slider(scene, call);
            let view = app.precision_slider_view().unwrap();
            for (width, height) in [(1, 1), (10, 5), (80, 24), (140, 40)] {
                let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
                terminal
                    .draw(|frame| view.render(frame, &app.theme, true))
                    .unwrap();
                if width >= 80 {
                    let buffer = terminal.backend().buffer();
                    let text: String = buffer.content.iter().map(|cell| cell.symbol()).collect();
                    assert!(text.contains("value: 0.5"));
                    assert!(text.contains("mouse: cells"));
                    assert!(text.contains("audio smoothing off"));
                }
            }
        }

        #[test]
        fn expanded_slider_covers_pixel_visuals_without_erasing_the_surrounding_scene() {
            use crate::graphics::{self, PixelImage};
            use ratatui::{Terminal, backend::TestBackend};
            let directory = tempfile::tempdir().unwrap();
            let (mut app, scene, call) = slider_app(directory.path());
            app.open_precision_slider(scene, call);
            let view = app.precision_slider_view().unwrap();
            let frame = Rect::new(0, 0, 80, 24);
            let popup = slider_precision::geometry(frame);
            let (_, images) = graphics::capture_images(|| {
                graphics::push_image(PixelImage::inline(frame, 80, 24, vec![255; 80 * 24 * 4]));
                let mut terminal =
                    Terminal::new(TestBackend::new(frame.width, frame.height)).unwrap();
                terminal
                    .draw(|frame| view.render(frame, &app.theme, true))
                    .unwrap();
            });
            assert_eq!(images.len(), 1);
            for y in frame.y..frame.bottom() {
                for x in frame.x..frame.right() {
                    let alpha = images[0].rgba[(usize::from(y) * 80 + usize::from(x)) * 4 + 3];
                    assert_eq!(alpha, if within(popup, x, y) { 0 } else { 255 });
                }
            }
        }

        #[test]
        fn expanded_slider_opens_from_caret_without_a_mouse_click() {
            let directory = tempfile::tempdir().unwrap();
            let (mut app, scene, call) = slider_app(directory.path());
            app.editor_mut()
                .set_selection(Selection::caret(ByteOffset(call + 8)))
                .unwrap();
            let before = app.editor().source();
            precision_key(&mut app, KeyCode::Enter, KeyModifiers::ALT);
            assert!(app.slider_precision.is_some());
            assert_eq!(app.armed_slider, Some((scene, call)));
            assert_eq!(app.editor().source(), before);
        }

        #[test]
        fn expanded_slider_pixels_change_value_inside_one_handle_cell() {
            let directory = tempfile::tempdir().unwrap();
            let (mut app, scene, call) = slider_app(directory.path());
            app.features.pixel_mouse = true;
            app.features.cell_pixels = Some((10, 20));
            app.open_precision_slider(scene, call);
            let area = slider_precision::geometry(app.frame);
            let column = area.x + 12;
            let row = (area.y + 3) * 20;
            mouse(
                &mut app,
                MouseEventKind::Down(MouseButton::Left),
                column * 10 + 4,
                row,
            );
            let first = value(&app, scene, call);
            mouse(
                &mut app,
                MouseEventKind::Drag(MouseButton::Left),
                column * 10 + 5,
                row,
            );
            let second = value(&app, scene, call);
            assert!(second > first && second - first < 0.002);
        }

        #[test]
        fn logarithmic_frequency_rails_track_pixels_and_captured_endpoints() {
            for expanded in [false, true] {
                let directory = tempfile::tempdir().unwrap();
                let (mut app, scene, call) = slider_app_with_source(
                    directory.path(),
                    "$: s(\"saw\").lpf(slider(1000, 100, 10000, .01))\n",
                );
                assert!(app.ui_settings.frequency_slider_log);
                assert_eq!(
                    app.live_chip_at(scene, call).unwrap().travel,
                    slider::Travel::Logarithmic,
                );
                // This frame gives the expanded rail an odd number of cells, so
                // its center is an exact pixel as well as an exact character.
                app.frame = Rect::new(0, 0, 103, 30);
                app.regions = view::regions(app.frame, false, 1, false, false, None, None);
                let (x, width, row) = if expanded {
                    app.open_precision_slider(scene, call);
                    let area = slider_precision::geometry(app.frame);
                    (area.x + 2, area.width - 4, area.y + 3)
                } else {
                    lay_out(&mut app, scene, call)
                };
                // Cell-reporting terminals use the same logarithmic scale, with
                // fewer available positions along the rail.
                mouse(&mut app, MouseEventKind::Down(MouseButton::Left), x, row);
                assert_eq!(value(&app, scene, call), 100.0);
                let middle_cell = width / 2;
                mouse(
                    &mut app,
                    MouseEventKind::Drag(MouseButton::Left),
                    x + middle_cell,
                    row,
                );
                // A place on this rail spans more than one hertz, so the
                // pointer lands on the whole hertz inside it.
                let ratio = f64::from(middle_cell) / f64::from(width - 1);
                let expected = (100.0 * 100_f64.powf(ratio)).round();
                assert_value(&app, scene, call, expected);
                mouse(
                    &mut app,
                    MouseEventKind::Up(MouseButton::Left),
                    x + width - 1,
                    row,
                );
                assert_eq!(value(&app, scene, call), 10000.0);
                if !expanded {
                    assert_eq!(lay_out(&mut app, scene, call), (x, width, row));
                }
                app.features.pixel_mouse = true;
                app.features.cell_pixels = Some((9, 18));
                let first = x * 9 + 4;
                let last = (x + width - 1) * 9 + 4;
                let y = row * 18 + 8;
                mouse(&mut app, MouseEventKind::Down(MouseButton::Left), first, y);
                assert_eq!(value(&app, scene, call), 100.0);

                // Multiple reports within one character must still change Hz.
                // Compute the expected exponential independently of Travel.
                let column = x + width / 2;
                let mut previous = 100.0;
                for offset in 1..8 {
                    let pixel = column * 9 + offset;
                    assert_eq!(pixel / 9, column);
                    mouse(&mut app, MouseEventKind::Drag(MouseButton::Left), pixel, y);
                    let ratio = f64::from(pixel - first) / f64::from(last - first);
                    let expected = (100.0 * 100_f64.powf(ratio)).round();
                    assert_value(&app, scene, call, expected);
                    let current = value(&app, scene, call);
                    assert!(current > previous, "every pixel changes frequency");
                    previous = current;
                }
                let middle = (first + last) / 2;
                mouse(&mut app, MouseEventKind::Drag(MouseButton::Left), middle, y);
                let midpoint = value(&app, scene, call);
                if expanded {
                    assert_eq!(midpoint, 1000.0, "the center is the geometric mean");
                } else {
                    assert!(midpoint < 1000.0 && midpoint > 980.0);
                    mouse(
                        &mut app,
                        MouseEventKind::Drag(MouseButton::Left),
                        middle + 1,
                        y,
                    );
                    assert!(value(&app, scene, call) > 1000.0);
                }
                // No redraw between reports. Leaving the rail clamps; returning
                // immediately recovers its absolute value without unwinding motion.
                mouse(
                    &mut app,
                    MouseEventKind::Drag(MouseButton::Left),
                    last + 100,
                    0,
                );
                assert_eq!(value(&app, scene, call), 10000.0);
                mouse(&mut app, MouseEventKind::Drag(MouseButton::Left), middle, y);
                assert_eq!(value(&app, scene, call), midpoint);
                mouse(&mut app, MouseEventKind::Drag(MouseButton::Left), 0, 0);
                assert_eq!(value(&app, scene, call), 100.0);
                mouse(&mut app, MouseEventKind::Up(MouseButton::Left), last, 0);
                assert_eq!(value(&app, scene, call), 10000.0);
                mouse(&mut app, MouseEventKind::Moved, middle, y);
                assert_eq!(value(&app, scene, call), 10000.0);
            }
        }

        #[test]
        fn frequency_travel_toggle_moves_both_drawn_knobs_without_editing_hz() {
            use ratatui::{Terminal, backend::TestBackend};
            let directory = tempfile::tempdir().unwrap();
            let (mut app, scene, call) = slider_app_with_source(
                directory.path(),
                "$: s(\"saw\").lpf(slider(1000, 100, 10000, .01))\n",
            );
            app.frame = Rect::new(0, 0, 103, 30);
            app.armed_slider = Some((scene, call));
            app.open_precision_slider(scene, call);
            let area = slider_precision::geometry(app.frame);
            let left = area.x + 2;
            let last = area.right() - 3;
            let render = |app: &App| {
                let mut terminal =
                    Terminal::new(TestBackend::new(app.frame.width, app.frame.height)).unwrap();
                let view = app.precision_slider_view().unwrap();
                terminal
                    .draw(|frame| view.render(frame, &app.theme, true))
                    .unwrap();
                let buffer = terminal.backend().buffer();
                let knob = (left..=last)
                    .find(|&x| buffer[(x, area.y + 3)].symbol() == "█")
                    .expect("the expanded rail has a visible handle");
                let text: String = buffer.content.iter().map(|cell| cell.symbol()).collect();
                (knob, text)
            };
            assert!((app.slider_chip_views(scene)[0].notch - 0.5).abs() < 1e-12);
            let (knob, text) = render(&app);
            assert_eq!(knob, (left + last) / 2);
            assert!(text.contains("frequency travel Log"));
            let before = app.editor().source();
            let revision = app.editor().revision();
            precision_key(&mut app, KeyCode::Char('l'), KeyModifiers::NONE);
            assert!(!app.ui_settings.frequency_slider_log);
            assert_eq!(app.prefs.frequency_slider_log, Some(false));
            assert!(!app.status.contains(" · Log"));
            assert!(
                app.prefs_pending,
                "the global preference is scheduled for persistence"
            );
            assert_eq!(app.editor().source(), before);
            assert_eq!(app.editor().revision(), revision);
            assert_eq!(value(&app, scene, call), 1000.0);
            assert!((app.slider_chip_views(scene)[0].notch - 1.0 / 11.0).abs() < 1e-12);
            let (knob, text) = render(&app);
            assert_eq!(knob, left + 9);
            assert!(text.contains("frequency travel Linear"));
            precision_key(&mut app, KeyCode::Char('l'), KeyModifiers::NONE);
            assert_eq!(app.prefs.frequency_slider_log, Some(true));
            assert!(app.status.contains(" · Log"));
            assert_eq!(app.editor().revision(), revision);
            assert_eq!(render(&app).0, (left + last) / 2);

            precision_key(&mut app, KeyCode::Right, KeyModifiers::NONE);
            assert_eq!(value(&app, scene, call), 1047.13);
            precision_key(&mut app, KeyCode::Left, KeyModifiers::NONE);
            assert_eq!(value(&app, scene, call), 1000.0);
            mouse(&mut app, MouseEventKind::ScrollUp, left, area.y + 3);
            assert_eq!(value(&app, scene, call), 1047.13);
            precision_key(&mut app, KeyCode::Right, KeyModifiers::SHIFT);
            assert_eq!(value(&app, scene, call), 1047.14);
            precision_key(&mut app, KeyCode::Left, KeyModifiers::SHIFT);
            assert_eq!(value(&app, scene, call), 1047.13);
            precision_key(&mut app, KeyCode::Home, KeyModifiers::NONE);
            assert_eq!(value(&app, scene, call), 100.0);
            precision_key(&mut app, KeyCode::End, KeyModifiers::NONE);
            assert_eq!(value(&app, scene, call), 10000.0);
            app.handle_terminal_event(Event::Paste("1234.567".into()))
                .unwrap();
            assert_eq!(value(&app, scene, call), 10000.0, "entry waits for Enter");
            precision_key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
            assert_eq!(value(&app, scene, call), 1234.57, "exact entry remains Hz");
        }

        #[test]
        fn an_unrelated_recovered_syntax_error_keeps_the_frequency_knob_and_travel() {
            let directory = tempfile::tempdir().unwrap();
            let (mut app, scene, call) = slider_app_with_source(
                directory.path(),
                "$: s(\"saw\").lpf(slider(1000, 100, 10000, .01))\n",
            );
            let before = app.slider_chip_views(scene)[0].notch;
            let end = app.editor().source().len();
            app.dispatch_editor(Command::ReplaceRange {
                from: ByteOffset(end),
                to: ByteOffset(end),
                text: "const x;".into(),
            })
            .unwrap();
            // This incomplete declaration produces a real recoverable diagnostic.
            // Refresh the text controls just as a subsequent lint pass does.
            let source = app.editor().source();
            let parsed = rustel_transpiler::transpile(&source, &Default::default());
            assert!(!parsed.diagnostics.is_empty());
            app.live_sliders.insert(
                scene,
                LiveSliderSet {
                    revision: app.editor().revision(),
                    sliders: rustel_runtime::ui_events::literal_sliders(&source),
                },
            );
            assert_eq!(app.slider_chip_views(scene)[0].notch, before);
            assert!((before - 0.5).abs() < 1e-12);
            app.drag_slider(scene, call, SliderTrack { x: 10, width: 3 }, 11);
            assert_eq!(value(&app, scene, call), 1000.0);
            assert_eq!(app.editor().source(), source);
        }

        #[test]
        fn coarse_frequency_steps_advance_and_exact_entry_snaps_from_the_declared_minimum() {
            let directory = tempfile::tempdir().unwrap();
            let (mut app, scene, call) = slider_app_with_source(
                directory.path(),
                "$: s(\"saw\").lpf(slider(100.005, 100.005, 10000.005, 100))\n",
            );
            app.armed_slider = Some((scene, call));
            precision_key(&mut app, KeyCode::Right, KeyModifiers::NONE);
            assert_eq!(
                value(&app, scene, call),
                200.005,
                "a log notch advances at least one declared step"
            );
            precision_key(&mut app, KeyCode::Left, KeyModifiers::NONE);
            assert_eq!(value(&app, scene, call), 100.005);
            app.open_precision_slider(scene, call);
            app.handle_terminal_event(Event::Paste("249.999".into()))
                .unwrap();
            precision_key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
            assert_eq!(
                value(&app, scene, call),
                200.005,
                "steps retain the minimum's fractional offset"
            );
            precision_key(&mut app, KeyCode::Right, KeyModifiers::SHIFT);
            assert_eq!(value(&app, scene, call), 300.005);
        }

        #[test]
        fn nonpositive_frequency_bounds_and_ordinary_positive_gain_stay_linear() {
            for (source, midpoint, maximum, frequency) in [
                (
                    "$: s(\"saw\").lpf(slider(1000, 0, 10000, .01))\n",
                    5000.0,
                    10000.0,
                    true,
                ),
                (
                    "$: s(\"saw\").lpf(slider(0, -100, 100, .01))\n",
                    0.0,
                    100.0,
                    true,
                ),
                (
                    "$: s(\"saw\").gain(slider(1000, 100, 10000, .01))\n",
                    5050.0,
                    10000.0,
                    false,
                ),
            ] {
                let directory = tempfile::tempdir().unwrap();
                let (mut app, scene, call) = slider_app_with_source(directory.path(), source);
                let chip = app.live_chip_at(scene, call).unwrap();
                assert_eq!(chip.frequency, frequency);
                assert_eq!(chip.travel, slider::Travel::Linear);
                app.frame = Rect::new(0, 0, 103, 30);
                app.open_precision_slider(scene, call);
                let before = app.editor().source();
                let preference = app.prefs.frequency_slider_log;
                precision_key(&mut app, KeyCode::Char('l'), KeyModifiers::NONE);
                assert!(app.ui_settings.frequency_slider_log);
                assert_eq!(app.prefs.frequency_slider_log, preference);
                assert_eq!(app.editor().source(), before);
                let area = slider_precision::geometry(app.frame);
                let left = area.x + 2;
                let right = area.right() - 3;
                mouse(
                    &mut app,
                    MouseEventKind::Down(MouseButton::Left),
                    (left + right) / 2,
                    area.y + 3,
                );
                assert_eq!(value(&app, scene, call), midpoint);
                mouse(
                    &mut app,
                    MouseEventKind::Up(MouseButton::Left),
                    right,
                    area.y + 3,
                );
                assert_eq!(value(&app, scene, call), maximum);
            }
        }

        /// A click on the pill arms the control: ←/→ then step it, Esc lets go.
        #[test]
        fn a_click_on_the_pill_puts_the_slider_on_the_arrows() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = app_over(directory.path());
            let scene = app.scenes.current().id;
            let source = "$: s(\"bd\").gain(slider(0.5, 0, 1, 0.01))\n";
            app.editor_mut()
                .dispatch(
                    Command::PasteText(source.into()),
                    HistoryMoment(0),
                    &mut MemoryClipboard::default(),
                )
                .expect("seed the score");
            let revision = app.scenes.current().editor.revision();
            app.live_sliders.insert(
                scene,
                LiveSliderSet {
                    revision,
                    sliders: rustel_runtime::ui_events::literal_sliders(
                        &app.scenes.current().editor.source(),
                    ),
                },
            );
            // The reference panel has the keyboard, so the arrows walk its
            // list. A click on a pill must still step it.
            app.toggle_reference_browse();
            assert_eq!(app.focus, Focus::Panel(PanelKind::Reference));
            // A frame's worth of layout, as `draw` lays it out - before every
            // click, since an edit invalidates the map until the next frame.
            app.frame = Rect::new(0, 0, 120, 30);
            app.regions = view::regions(app.frame, false, 1, true, true, None, None);
            let call = app
                .scenes
                .current()
                .editor
                .source()
                .find("slider(")
                .unwrap();
            fn lay_out(app: &mut App, scene: SceneId, call: usize) -> (u16, u16) {
                let widths = app.slider_pill_widths(scene);
                let region = app.regions.panes[0];
                let editor = &mut app.scenes.current_mut().editor;
                editor.set_inline_widths(widths);
                let grid = view::source_grid(editor, region.editor, true);
                editor.set_view_size(usize::from(grid.width), usize::from(grid.height));
                let map = editor.screen_map(grid).expect("map");
                let row = map
                    .cell_for_offset(ByteOffset(call))
                    .expect("the call is on screen")
                    .y;
                app.panes[0].last_map = Some(map);
                let chip = app.live_chip_at(scene, call).expect("a control");
                let (track_x, width) = app
                    .slider_pill_track(scene, &chip)
                    .expect("the pill is on screen");
                assert!(width >= 10, "a pill twice the keyword: {width}");
                (row, track_x)
            }
            // Through the whole routing, as the terminal sends it - a motion
            // report first, as a terminal reporting all motion would.
            fn mouse(app: &mut App, kind: MouseEventKind, x: u16, row: u16) {
                app.handle_terminal_event(Event::Mouse(MouseEvent {
                    kind,
                    column: x,
                    row,
                    modifiers: KeyModifiers::NONE,
                }))
                .expect("mouse handled");
            }
            let value_of = |app: &App| {
                app.live_chip_at(scene, call)
                    .expect("still a control")
                    .span
                    .value
            };

            let (row, track_x) = lay_out(&mut app, scene, call);
            mouse(&mut app, MouseEventKind::Moved, track_x + 1, row);
            mouse(
                &mut app,
                MouseEventKind::Down(MouseButton::Left),
                track_x + 1,
                row,
            );
            mouse(
                &mut app,
                MouseEventKind::Up(MouseButton::Left),
                track_x + 1,
                row,
            );
            mouse(&mut app, MouseEventKind::Moved, track_x + 3, row);
            assert_eq!(
                app.armed_slider,
                Some((scene, call)),
                "the click armed the control"
            );
            assert_eq!(
                app.focus,
                Focus::Editor,
                "the click took the keyboard for the score"
            );
            let after_click = value_of(&app);

            press(&mut app, KeyCode::Right, KeyModifiers::NONE);
            let stepped = value_of(&app);
            assert!(
                stepped > after_click,
                "→ stepped it up: {after_click} → {stepped}"
            );
            press(&mut app, KeyCode::Left, KeyModifiers::NONE);
            press(&mut app, KeyCode::Left, KeyModifiers::NONE);
            let back = value_of(&app);
            assert!(
                back < after_click,
                "← stepped it down: {after_click} → {back}"
            );
            assert_eq!(
                app.armed_slider,
                Some((scene, call)),
                "stepping keeps it armed"
            );

            // Esc lets go - of the control, not of the panel beside it.
            press(&mut app, KeyCode::Esc, KeyModifiers::NONE);
            assert_eq!(app.armed_slider, None, "Esc lets go");
            assert!(
                app.reference_panel.is_some(),
                "Esc on an armed slider closed the panel"
            );

            // And a panel taking the keyboard back lets go too, so the arrows
            // never step a control while the status says they walk a list.
            let (row, track_x) = lay_out(&mut app, scene, call);
            mouse(
                &mut app,
                MouseEventKind::Down(MouseButton::Left),
                track_x + 1,
                row,
            );
            mouse(
                &mut app,
                MouseEventKind::Up(MouseButton::Left),
                track_x + 1,
                row,
            );
            assert_eq!(app.armed_slider, Some((scene, call)));
            app.focus_panel(PanelKind::Reference);
            assert_eq!(
                app.armed_slider, None,
                "a panel with the keyboard lets the slider go"
            );
        }

        /// ⇧←/→ step by the control's own step where the notch is coarser.
        #[test]
        fn shift_steps_an_armed_slider_by_its_own_step() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = app_over(directory.path());
            let scene = app.scenes.current().id;
            let source = "$: s(\"bd\").gain(slider(0.5, 0, 1, 0.001))\n";
            app.editor_mut()
                .dispatch(
                    Command::PasteText(source.into()),
                    HistoryMoment(0),
                    &mut MemoryClipboard::default(),
                )
                .expect("seed the score");
            let revision = app.scenes.current().editor.revision();
            app.live_sliders.insert(
                scene,
                LiveSliderSet {
                    revision,
                    sliders: rustel_runtime::ui_events::literal_sliders(
                        &app.scenes.current().editor.source(),
                    ),
                },
            );
            let call = app
                .scenes
                .current()
                .editor
                .source()
                .find("slider(")
                .unwrap();
            app.armed_slider = Some((scene, call));
            let value_of = |app: &App| app.live_chip_at(scene, call).expect("a control").span.value;
            press(&mut app, KeyCode::Right, KeyModifiers::NONE);
            assert!(
                (value_of(&app) - 0.51).abs() < 1e-9,
                "a notch: {}",
                value_of(&app)
            );
            press(&mut app, KeyCode::Right, KeyModifiers::SHIFT);
            assert!(
                (value_of(&app) - 0.511).abs() < 1e-9,
                "a step: {}",
                value_of(&app)
            );
            press(&mut app, KeyCode::Left, KeyModifiers::SHIFT);
            press(&mut app, KeyCode::Left, KeyModifiers::NONE);
            assert!(
                (value_of(&app) - 0.5).abs() < 1e-9,
                "and back: {}",
                value_of(&app)
            );
        }

        /// Under the enhanced keyboard protocol a held arrow is Press, Repeat…,
        /// Release. Every press and repeat steps an armed slider; sent to the
        /// score instead, the repeats walked the caret away from the control.
        #[test]
        fn a_held_arrow_keeps_stepping_an_armed_slider() {
            let directory = tempfile::tempdir().expect("temp dir");
            let mut app = app_over(directory.path());
            let scene = app.scenes.current().id;
            let source = "$: s(\"bd\").gain(slider(0.5, 0, 1, 0.01))\n";
            app.editor_mut()
                .dispatch(
                    Command::PasteText(source.into()),
                    HistoryMoment(0),
                    &mut MemoryClipboard::default(),
                )
                .expect("seed the score");
            let revision = app.scenes.current().editor.revision();
            app.live_sliders.insert(
                scene,
                LiveSliderSet {
                    revision,
                    sliders: rustel_runtime::ui_events::literal_sliders(
                        &app.scenes.current().editor.source(),
                    ),
                },
            );
            let call = app
                .scenes
                .current()
                .editor
                .source()
                .find("slider(")
                .unwrap();
            app.armed_slider = Some((scene, call));
            // The caret ahead of the control, where the control's own edits
            // cannot move it: only a key sent to the score would.
            let _ = app
                .editor_mut()
                .set_selection(crate::editor::Selection::caret(ByteOffset(0)));
            let caret = app.editor().primary_selection().head.0;
            let value_of = |app: &App| app.live_chip_at(scene, call).expect("a control").span.value;
            let key = |app: &mut App, kind: KeyEventKind| {
                app.handle_terminal_event(Event::Key(crossterm::event::KeyEvent::new_with_kind(
                    KeyCode::Right,
                    KeyModifiers::NONE,
                    kind,
                )))
                .expect("key handled");
            };
            key(&mut app, KeyEventKind::Press);
            key(&mut app, KeyEventKind::Repeat);
            key(&mut app, KeyEventKind::Repeat);
            key(&mut app, KeyEventKind::Release);
            assert!(
                (value_of(&app) - 0.53).abs() < 1e-9,
                "three steps: {}",
                value_of(&app)
            );
            assert_eq!(app.armed_slider, Some((scene, call)), "still armed");
            assert_eq!(
                app.editor().primary_selection().head.0,
                caret,
                "the caret stayed put"
            );
        }
    }
}
