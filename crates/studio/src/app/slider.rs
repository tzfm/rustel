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

/// Preserve the actual number: the text and the engine must receive the
/// same value, including fractional steps above one and very fine steps.
pub(super) fn format_value(value: f64) -> String {
    if value == 0.0 {
        "0".to_owned()
    } else {
        value.to_string()
    }
}

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
        let selection = self.editor().primary_selection();
        if !selection.is_empty() {
            return Ok(false);
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
            _ => return Ok(false),
        };
        let Some(chip) = target else {
            return Ok(false);
        };
        self.dispatch_editor(Command::ReplaceRange {
            from: ByteOffset(chip.call.start),
            to: ByteOffset(chip.call.end),
            text: String::new(),
        })?;
        self.status = "slider removed - undo brings it back whole".to_owned();
        Ok(true)
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
        self.set_slider_with_smoothing(
            scene,
            &chip,
            chip.travel.at(ratio, chip.span.min, chip.span.max),
            self.ui_settings.slider_smoothing,
        );
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
    /// One wheel notch or coarse keyboard nudge. The declared step for a coarse
    /// slider, a hundredth of the range for a fine one, so a `0.001` step
    /// does not take a thousand notches to cross.
    pub(super) fn notch(&self) -> f64 {
        let range = self.max - self.min;
        if range / self.step <= 200.0 {
            self.step
        } else {
            (range / 100.0 / self.step).round().max(1.0) * self.step
        }
    }

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
    if chip.travel == Travel::Logarithmic {
        let ratio = chip.travel.ratio(span.value, span.min, span.max);
        let target = chip.travel.at(ratio + notches / 100.0, span.min, span.max);
        // Even a coarse declared step must advance at the low end.
        if notches > 0.0 {
            target.max(span.value + span.step)
        } else {
            target.min(span.value - span.step)
        }
    } else {
        span.value + notches * span.notch()
    }
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
